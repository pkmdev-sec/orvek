#![cfg(unix)]
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::Duration,
};
use orvek_harness::{
    Digest,
    artifacts::{ArtifactError, ArtifactStore},
    delivery::{DeliveryError, PatchArtifact, PatchBuilder, PatchLimits},
    workspace::{Snapshot, SnapshotPolicy},
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _directory: tempfile::TempDir,
    base: PathBuf,
    candidate: PathBuf,
    scratch: PathBuf,
    artifacts: ArtifactStore,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("source");
        let candidate = directory.path().join("candidate-source");
        let scratch = directory.path().join("scratch");
        for path in [&base, &candidate, &scratch] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let artifacts =
            ArtifactStore::open(&directory.path().join("artifacts"), 64 * 1024 * 1024).unwrap();
        Self {
            _directory: directory,
            base,
            candidate,
            scratch,
            artifacts,
        }
    }
    fn snapshots(&self) -> (Snapshot, Snapshot) {
        (
            Snapshot::capture(&self.base, SnapshotPolicy::default(), &self.artifacts).unwrap(),
            Snapshot::capture(&self.candidate, SnapshotPolicy::default(), &self.artifacts).unwrap(),
        )
    }
    async fn build(&self) -> Result<PatchArtifact, DeliveryError> {
        let (base, candidate) = self.snapshots();
        PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .build(
                &base,
                &candidate,
                &self.artifacts,
                &self.scratch,
                CancellationToken::new(),
            )
            .await
    }
}
fn file(root: &Path, path: &str, bytes: &[u8], mode: u32) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn dump_error(error: &DeliveryError, store: &ArtifactStore) {
    if let DeliveryError::GitFailed { stderr, .. } = error {
        eprintln!(
            "Git diagnostics: {}",
            String::from_utf8_lossy(&store.read(*stderr).unwrap())
        );
    }
}

#[tokio::test]
async fn text_add_delete_and_executable_changes_round_trip_without_touching_source_or_index() {
    let fixture = Fixture::new();
    file(&fixture.base, "modified.txt", b"before\nline two\n", 0o644);
    file(&fixture.base, "deleted.txt", b"remove\n", 0o644);
    file(
        &fixture.base,
        "program",
        b"#!/bin/sh\nprintf example\n",
        0o644,
    );
    file(
        &fixture.candidate,
        "modified.txt",
        b"after\nline two\n",
        0o644,
    );
    file(&fixture.candidate, "added.txt", b"added\n", 0o644);
    file(
        &fixture.candidate,
        "program",
        b"#!/bin/sh\nprintf example\n",
        0o755,
    );
    fs::create_dir(fixture.base.join(".git")).unwrap();
    fs::write(fixture.base.join(".git/index"), b"original index sentinel").unwrap();
    let (before, candidate) = fixture.snapshots();
    let built = fixture
        .build()
        .await
        .inspect_err(|error| dump_error(error, &fixture.artifacts))
        .unwrap();
    assert_eq!(built.baseline, before.publish(&fixture.artifacts).unwrap());
    assert_eq!(
        built.candidate,
        candidate.publish(&fixture.artifacts).unwrap()
    );
    assert_eq!(built.receipt.applied_snapshot, built.candidate);
    let patch = String::from_utf8(fixture.artifacts.read(built.patch).unwrap()).unwrap();
    assert!(patch.contains("-before"));
    assert!(patch.contains("+after"));
    assert!(patch.contains("new file mode 100644"));
    assert!(patch.contains("deleted file mode 100644"));
    assert!(patch.contains("new mode 100755"));
    assert!(
        built
            .receipt
            .commands
            .iter()
            .any(|c| c.step == "apply-patch")
    );
    assert!(
        built
            .receipt
            .commands
            .iter()
            .all(|c| c.exit_code == 0 && c.process_group_quiescent)
    );
    assert!(before.matches(&fixture.base).unwrap());
    assert!(candidate.matches(&fixture.candidate).unwrap());
    assert_eq!(
        fs::read(fixture.base.join(".git/index")).unwrap(),
        b"original index sentinel"
    );
    assert!(fixture.artifacts.read(built.receipt_digest).is_ok());
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 0);
}

#[tokio::test]
async fn binary_and_quoted_paths_produce_real_apply_compatible_patches() {
    let fixture = Fixture::new();
    let names = [
        "with space.txt",
        "quote\"and\\backslash.txt",
        "line\nbreak.txt",
        "café-日.txt",
        "--leading-option",
    ];
    for name in names {
        file(&fixture.base, name, b"old\n", 0o644);
        file(&fixture.candidate, name, b"new\n", 0o644);
    }
    file(&fixture.base, "binary", &[0, 255, 1, 2, 3, 4, 0, 9], 0o644);
    file(
        &fixture.candidate,
        "binary",
        &[0, 254, 7, 6, 5, 4, 0, 8],
        0o644,
    );
    let built = fixture
        .build()
        .await
        .inspect_err(|error| dump_error(error, &fixture.artifacts))
        .unwrap();
    let bytes = fixture.artifacts.read(built.patch).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("GIT binary patch"));
    assert!(String::from_utf8_lossy(&bytes).contains("\\n"));
    let (base, candidate) = fixture.snapshots();
    let verified = PatchBuilder::new(PatchLimits::default())
        .unwrap()
        .verify_patch(
            &base,
            &candidate,
            built.patch,
            &fixture.artifacts,
            &fixture.scratch,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(verified.receipt.applied_snapshot, verified.candidate);
}

#[tokio::test]
async fn symlinks_and_nested_directory_additions_round_trip() {
    let fixture = Fixture::new();
    file(&fixture.base, "referent", b"old", 0o644);
    symlink("referent", fixture.base.join("alias")).unwrap();
    file(&fixture.candidate, "referent", b"new", 0o644);
    file(&fixture.candidate, "nested/new-target", b"target", 0o644);
    symlink("nested/new-target", fixture.candidate.join("alias")).unwrap();
    let built = fixture
        .build()
        .await
        .inspect_err(|error| dump_error(error, &fixture.artifacts))
        .unwrap();
    assert_eq!(built.receipt.applied_snapshot, built.candidate);
    assert!(
        String::from_utf8_lossy(&fixture.artifacts.read(built.patch).unwrap()).contains("120000")
    );
}

#[tokio::test]
async fn patch_digest_is_reproducible_across_scratch_runs() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"before", 0o644);
    file(&fixture.candidate, "file", b"after", 0o644);
    let first = fixture.build().await.unwrap();
    let second = fixture.build().await.unwrap();
    assert_eq!(first.patch, second.patch);
    assert_eq!(first.baseline, second.baseline);
    assert_eq!(first.candidate, second.candidate);
}

#[tokio::test]
async fn unchanged_empty_directories_are_preserved_but_changed_empty_directories_are_rejected() {
    let fixture = Fixture::new();
    for root in [&fixture.base, &fixture.candidate] {
        fs::create_dir(root.join("unchanged-empty")).unwrap();
        fs::set_permissions(
            root.join("unchanged-empty"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let unchanged = fixture
        .build()
        .await
        .inspect_err(|error| dump_error(error, &fixture.artifacts))
        .unwrap();
    assert!(fixture.artifacts.read(unchanged.patch).unwrap().is_empty());
    fs::create_dir(fixture.candidate.join("new-empty")).unwrap();
    fs::set_permissions(
        fixture.candidate.join("new-empty"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(matches!(
        fixture.build().await,
        Err(DeliveryError::Unrepresentable { .. })
    ));
    fs::remove_dir(fixture.candidate.join("new-empty")).unwrap();
    fs::remove_dir(fixture.candidate.join("unchanged-empty")).unwrap();
    assert!(matches!(
        fixture.build().await,
        Err(DeliveryError::Unrepresentable { .. })
    ));
}

#[tokio::test]
async fn full_permission_changes_and_policy_changes_are_explicit_errors() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"same", 0o644);
    file(&fixture.candidate, "file", b"same", 0o600);
    assert!(matches!(
        fixture.build().await,
        Err(DeliveryError::Unrepresentable { .. })
    ));
    let (base, mut candidate) = fixture.snapshots();
    candidate.policy.max_files -= 1;
    assert!(matches!(
        PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .build(
                &base,
                &candidate,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::PolicyChanged)
    ));
}

#[tokio::test]
async fn a_different_valid_patch_cannot_receive_a_receipt_for_the_requested_candidate() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"before\n", 0o644);
    file(&fixture.candidate, "file", b"after\n", 0o644);
    let built = fixture.build().await.unwrap();
    let (base, mut wrong_candidate) = fixture.snapshots();
    let content = fixture
        .artifacts
        .put(b"not the requested output\n")
        .unwrap();
    wrong_candidate.entries.insert(
        "file".into(),
        orvek_harness::workspace::Entry::File {
            content,
            mode: 0o644,
        },
    );
    assert!(matches!(
        PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .verify_patch(
                &base,
                &wrong_candidate,
                built.patch,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::ReproductionMismatch(_))
    ));
    fs::write(fixture.artifacts.path(built.patch), b"tampered patch").unwrap();
    assert!(matches!(
        PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .verify_patch(
                &base,
                &wrong_candidate,
                built.patch,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::Artifact(ArtifactError::Integrity(_)))
    ));
}

#[tokio::test]
async fn malformed_patch_and_tampered_source_artifact_never_get_success_receipts() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"before", 0o644);
    file(&fixture.candidate, "file", b"after", 0o644);
    let (base, candidate) = fixture.snapshots();
    let builder = PatchBuilder::new(PatchLimits::default()).unwrap();
    let invalid = fixture.artifacts.put(b"not a patch\n").unwrap();
    assert!(matches!(
        builder
            .verify_patch(
                &base,
                &candidate,
                invalid,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::GitFailed { .. })
    ));
    let content = Digest::of(b"before");
    fs::write(fixture.artifacts.path(content), b"corrupt").unwrap();
    assert!(matches!(
        builder
            .build(
                &base,
                &candidate,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::Artifact(ArtifactError::Integrity(_)))
    ));
}

#[tokio::test]
async fn cancellation_deadlines_and_output_bounds_fail_without_success_receipts() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"before", 0o644);
    file(&fixture.candidate, "file", &vec![b'x'; 100000], 0o644);
    let (base, candidate) = fixture.snapshots();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .build(
                &base,
                &candidate,
                &fixture.artifacts,
                &fixture.scratch,
                cancel
            )
            .await,
        Err(DeliveryError::Cancelled)
    ));
    let limits = PatchLimits {
        timeout: Duration::from_nanos(1),
        ..PatchLimits::default()
    };
    assert!(matches!(
        PatchBuilder::new(limits)
            .unwrap()
            .build(
                &base,
                &candidate,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::TimedOut)
    ));
    let limits = PatchLimits {
        max_patch_bytes: 16,
        ..PatchLimits::default()
    };
    assert!(matches!(
        PatchBuilder::new(limits)
            .unwrap()
            .build(
                &base,
                &candidate,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new()
            )
            .await,
        Err(DeliveryError::Limit("Git output"))
    ));
    assert!(base.matches(&fixture.base).unwrap());
    assert!(candidate.matches(&fixture.candidate).unwrap());
}

#[tokio::test]
async fn inherited_git_configuration_cannot_execute_helpers_or_touch_an_external_index() {
    let poison = tempfile::tempdir().unwrap();
    let hooks = poison.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let helper = poison.path().join("helper");
    let marker = poison.path().join("executed");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nexit 91\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    for name in ["post-index-change", "post-checkout", "pre-commit"] {
        fs::copy(&helper, hooks.join(name)).unwrap();
    }
    let config = poison.path().join("global");
    fs::write(&config,format!("[core]\n hooksPath = {}\n fsmonitor = {}\n attributesFile = {}\n[filter \"evil\"]\n clean = {}\n smudge = {}\n required = true\n[diff \"evil\"]\n command = {}\n textconv = {}\n[credential]\n helper = {}\n",hooks.display(),helper.display(),poison.path().join("attributes").display(),helper.display(),helper.display(),helper.display(),helper.display(),helper.display())).unwrap();
    fs::write(
        poison.path().join("attributes"),
        "* filter=evil diff=evil text eol=crlf\n",
    )
    .unwrap();
    let index = poison.path().join("external-index");
    fs::write(&index, b"external index sentinel").unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "poisoned_configuration_child",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("ORVEK_PATCH_POISON_CHILD", poison.path())
        .env("HOME", poison.path())
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_EXTERNAL_DIFF", &helper)
        .env("GIT_EXEC_PATH", poison.path())
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", &helper)
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(poison.path().join("verified").exists());
    assert!(!marker.exists());
    assert_eq!(fs::read(index).unwrap(), b"external index sentinel");
}

#[tokio::test]
#[ignore = "invoked with a controlled environment by the parent isolation test"]
async fn poisoned_configuration_child() {
    let poison = std::env::var_os("ORVEK_PATCH_POISON_CHILD").expect("isolated parent fixture");
    let fixture = Fixture::new();
    for root in [&fixture.base, &fixture.candidate] {
        file(
            root,
            ".gitattributes",
            b"* filter=evil diff=evil text eol=crlf working-tree-encoding=UTF-16\n",
            0o644,
        );
    }
    file(&fixture.base, "nested/text", b"raw before\r\n", 0o644);
    file(&fixture.candidate, "nested/text", b"raw after\r\n", 0o644);
    let built = fixture
        .build()
        .await
        .inspect_err(|error| dump_error(error, &fixture.artifacts))
        .unwrap();
    assert_eq!(built.receipt.applied_snapshot, built.candidate);
    fs::write(
        PathBuf::from(poison).join("verified"),
        built.patch.to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn nonempty_garbage_is_not_accepted_as_an_empty_patch_on_identical_snapshots() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"same", 0o644);
    file(&fixture.candidate, "file", b"same", 0o644);
    let (base, candidate) = fixture.snapshots();
    let garbage = fixture
        .artifacts
        .put(b"arbitrary output is not a patch\n")
        .unwrap();
    let result = PatchBuilder::new(PatchLimits::default())
        .unwrap()
        .verify_patch(
            &base,
            &candidate,
            garbage,
            &fixture.artifacts,
            &fixture.scratch,
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(result, Err(DeliveryError::GitFailed { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn excluded_paths_cannot_hide_extra_patch_effects() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"same", 0o644);
    file(&fixture.candidate, "file", b"same", 0o644);
    let (base, candidate) = fixture.snapshots();
    for name in [".env", "target/extra"] {
        let patch = format!(
            "diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+hidden effect\n"
        );
        let digest = fixture.artifacts.put(patch.as_bytes()).unwrap();
        let result = PatchBuilder::new(PatchLimits::default())
            .unwrap()
            .verify_patch(
                &base,
                &candidate,
                digest,
                &fixture.artifacts,
                &fixture.scratch,
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(DeliveryError::ReproductionMismatch(_))
        ));
    }
}

#[tokio::test]
async fn escaping_patch_paths_cannot_modify_the_original_source() {
    let fixture = Fixture::new();
    file(&fixture.base, "file", b"safe\n", 0o644);
    file(&fixture.candidate, "file", b"safe\n", 0o644);
    let (base, candidate) = fixture.snapshots();
    let path = "../../../source/file";
    let patch = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-safe\n+overwritten\n"
    );
    let digest = fixture.artifacts.put(patch.as_bytes()).unwrap();
    let result = PatchBuilder::new(PatchLimits::default())
        .unwrap()
        .verify_patch(
            &base,
            &candidate,
            digest,
            &fixture.artifacts,
            &fixture.scratch,
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(result, Err(DeliveryError::GitFailed { .. })),
        "{result:?}"
    );
    assert_eq!(fs::read(fixture.base.join("file")).unwrap(), b"safe\n");
}
