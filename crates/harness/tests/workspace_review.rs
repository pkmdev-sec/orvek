use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};
use orvek_harness::{
    Digest,
    artifacts::ArtifactStore,
    review::{self, ReviewError, ReviewLimits, ReviewRange, ReviewSide},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Repo {
    temp: TempDir,
    artifacts: ArtifactStore,
}
impl Repo {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("repo")).unwrap();
        let artifacts =
            ArtifactStore::open(&temp.path().join("artifacts"), 128 * 1024 * 1024).unwrap();
        let repo = Self { temp, artifacts };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "Fixture"]);
        repo.git(&["config", "user.email", "fixture@example.invalid"]);
        repo.write("text.txt", b"base\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "initial"]);
        repo
    }
    fn path(&self) -> std::path::PathBuf {
        self.temp.path().join("repo")
    }
    fn write(&self, name: &str, data: &[u8]) {
        let path = self.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("/usr/bin/git")
            .current_dir(self.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn bytes(&self, id: Digest, side: ReviewSide, path: &str) -> Vec<u8> {
        let file = review::file(&self.artifacts, id, side, path)
            .unwrap()
            .unwrap();
        self.artifacts.read(file.content).unwrap()
    }
    async fn inspect(&self, range: ReviewRange) -> review::ReviewInspection {
        review::inspect(
            &self.path(),
            range,
            &self.artifacts,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn staged_and_committed_reads_stay_bound_to_their_range() {
    let repo = Repo::new();
    repo.write("text.txt", b"staged\n");
    repo.git(&["add", "text.txt"]);
    repo.write("text.txt", b"working\n");
    let index = fs::read(repo.path().join(".git/index")).unwrap();
    let staged = repo.inspect(ReviewRange::Staged { base: None }).await;
    assert_eq!(
        repo.bytes(staged.manifest, ReviewSide::Before, "text.txt"),
        b"base\n"
    );
    assert_eq!(
        repo.bytes(staged.manifest, ReviewSide::After, "text.txt"),
        b"staged\n"
    );
    let repeated = repo.inspect(ReviewRange::Staged { base: None }).await;
    assert_eq!(staged.source_identity, repeated.source_identity);
    assert_eq!(staged.manifest, repeated.manifest);
    assert_eq!(fs::read(repo.path().join(".git/index")).unwrap(), index);
    repo.git(&["commit", "-qm", "second"]);
    let commit = repo
        .inspect(ReviewRange::Commit {
            revision: "HEAD".into(),
        })
        .await;
    repo.write("text.txt", b"later\n");
    assert_eq!(
        repo.bytes(commit.manifest, ReviewSide::After, "text.txt"),
        b"staged\n"
    );
    assert_eq!(
        repo.bytes(staged.manifest, ReviewSide::After, "text.txt"),
        b"staged\n"
    );
    let patch = String::from_utf8(repo.artifacts.read(commit.patch).unwrap()).unwrap();
    assert!(patch.contains("+staged"));
    assert!(!patch.contains("working"));
    let catalog = review::catalog(&repo.path(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(catalog.commits.len(), 2);
    assert_eq!(catalog.commits[0].title, "second");
    assert_eq!(catalog.branches[0].name, "refs/heads/main");
}

#[tokio::test]
async fn working_binary_quoted_paths_modes_links_and_deletions_are_frozen() {
    let repo = Repo::new();
    fs::remove_file(repo.path().join("text.txt")).unwrap();
    repo.write("quoted \" café\n.txt", b"hello\n");
    repo.write("binary.dat", &[0, 255, 0, 8]);
    repo.write("run", b"#!/bin/false\n");
    fs::set_permissions(repo.path().join("run"), fs::Permissions::from_mode(0o751)).unwrap();
    let outside = repo.temp.path().join("outside");
    fs::write(&outside, b"PRIVATE OUTSIDE").unwrap();
    symlink(&outside, repo.path().join("link")).unwrap();
    let inspection = repo.inspect(ReviewRange::WorkingTree { base: None }).await;
    assert!(
        review::file(
            &repo.artifacts,
            inspection.manifest,
            ReviewSide::After,
            "text.txt"
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        repo.bytes(inspection.manifest, ReviewSide::After, "binary.dat"),
        vec![0, 255, 0, 8]
    );
    assert_eq!(
        repo.bytes(
            inspection.manifest,
            ReviewSide::After,
            "quoted \" café\n.txt"
        ),
        b"hello\n"
    );
    assert_eq!(
        repo.bytes(inspection.manifest, ReviewSide::After, "link"),
        outside.as_os_str().as_encoded_bytes()
    );
    let file = review::file(
        &repo.artifacts,
        inspection.manifest,
        ReviewSide::After,
        "run",
    )
    .unwrap()
    .unwrap();
    assert_eq!(file.mode, 0o100755);
    assert_eq!(file.permissions, Some(0o751));
    let patch = String::from_utf8(repo.artifacts.read(inspection.patch).unwrap()).unwrap();
    assert!(patch.contains("GIT binary patch"));
    assert!(patch.contains("new file mode 100755"));
    assert!(!patch.contains("PRIVATE OUTSIDE"));
    let page = review::page(
        &repo.artifacts,
        inspection.manifest,
        ReviewSide::After,
        0,
        2,
    )
    .unwrap();
    assert_eq!(page.total, 4);
    assert_eq!(page.next_offset, Some(2));
    assert!(matches!(
        review::file(
            &repo.artifacts,
            inspection.manifest,
            ReviewSide::After,
            "../outside"
        ),
        Err(ReviewError::Path)
    ));
}

#[tokio::test]
async fn poisoned_repository_configuration_cannot_execute_helpers_or_modify_index() {
    let repo = Repo::new();
    let marker = repo.temp.path().join("executed");
    let poison = repo.temp.path().join("poison");
    fs::write(
        &poison,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&poison, fs::Permissions::from_mode(0o755)).unwrap();
    for key in [
        "core.fsmonitor",
        "diff.external",
        "diff.evil.command",
        "diff.evil.textconv",
        "filter.evil.clean",
        "filter.evil.smudge",
        "credential.helper",
    ] {
        repo.git(&["config", key, poison.to_str().unwrap()]);
    }
    repo.git(&[
        "config",
        "core.hooksPath",
        repo.temp.path().to_str().unwrap(),
    ]);
    repo.write(".gitattributes", b"* diff=evil filter=evil\n");
    repo.write("text.txt", b"changed\n");
    let index = fs::read(repo.path().join(".git/index")).unwrap();
    let inspection = repo.inspect(ReviewRange::WorkingTree { base: None }).await;
    assert_eq!(
        repo.bytes(inspection.manifest, ReviewSide::After, "text.txt"),
        b"changed\n"
    );
    assert!(!marker.exists());
    assert_eq!(fs::read(repo.path().join(".git/index")).unwrap(), index);
}

#[tokio::test]
async fn limits_cancellation_and_corrupt_manifests_fail_explicitly() {
    let repo = Repo::new();
    repo.write("large", &vec![b'x'; 1024]);
    let limits = ReviewLimits {
        max_file_bytes: 32,
        ..ReviewLimits::default()
    };
    assert!(matches!(
        review::inspect_with_limits(
            &repo.path(),
            ReviewRange::WorkingTree { base: None },
            &repo.artifacts,
            &CancellationToken::new(),
            limits
        )
        .await,
        Err(ReviewError::Limit(_))
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        review::inspect(
            &repo.path(),
            ReviewRange::Staged { base: None },
            &repo.artifacts,
            &cancel
        )
        .await,
        Err(ReviewError::Cancelled)
    ));
    let inspection = repo.inspect(ReviewRange::Staged { base: None }).await;
    fs::write(repo.artifacts.path(inspection.manifest), b"{}").unwrap();
    assert!(matches!(
        review::manifest(&repo.artifacts, inspection.manifest),
        Err(ReviewError::Corrupt)
    ));
}

#[tokio::test]
async fn redirected_worktree_is_rejected() {
    let repo = Repo::new();
    let outside = repo.temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    repo.git(&["config", "core.worktree", outside.to_str().unwrap()]);
    assert!(matches!(
        review::inspect(
            &repo.path(),
            ReviewRange::WorkingTree { base: None },
            &repo.artifacts,
            &CancellationToken::new()
        )
        .await,
        Err(ReviewError::Unsupported(_))
    ));
}

#[tokio::test]
async fn concurrent_working_changes_reject_the_capture() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let repo = Repo::new();
    for n in 0..500 {
        repo.write(&format!("many/{n:04}"), b"snapshot data\n");
    }
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = stop.clone();
    let root = repo.path();
    let writer = std::thread::spawn(move || {
        let mut n = 0u64;
        while !writer_stop.load(Ordering::Relaxed) {
            n += 1;
            fs::write(root.join("changing.tmp"), format!("{n}\n")).unwrap();
            fs::rename(root.join("changing.tmp"), root.join("text.txt")).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        n
    });
    let result = review::inspect(
        &repo.path(),
        ReviewRange::WorkingTree { base: None },
        &repo.artifacts,
        &CancellationToken::new(),
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    assert!(writer.join().unwrap() > 1);
    assert!(
        matches!(result, Err(ReviewError::Changed) | Err(ReviewError::Io(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn split_index_and_revision_operators_are_supported_without_source_writes() {
    let repo = Repo::new();
    repo.write("text.txt", b"second\n");
    repo.git(&["add", "text.txt"]);
    repo.git(&["commit", "-qm", "second"]);
    repo.git(&["update-index", "--split-index"]);
    let index = fs::read(repo.path().join(".git/index")).unwrap();
    let view = repo
        .inspect(ReviewRange::Staged {
            base: Some("HEAD~1".into()),
        })
        .await;
    assert_eq!(
        repo.bytes(view.manifest, ReviewSide::Before, "text.txt"),
        b"base\n"
    );
    assert_eq!(
        repo.bytes(view.manifest, ReviewSide::After, "text.txt"),
        b"second\n"
    );
    assert_eq!(fs::read(repo.path().join(".git/index")).unwrap(), index);
    assert!(matches!(
        review::inspect(
            &repo.path(),
            ReviewRange::Commit {
                revision: "HEAD:text.txt".into()
            },
            &repo.artifacts,
            &CancellationToken::new()
        )
        .await,
        Err(ReviewError::Range)
    ));
}

#[tokio::test]
async fn inherited_git_environment_is_ignored() {
    let repo = Repo::new();
    repo.write("text.txt", b"changed\n");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "environment_fixture_child"])
        .env("ORVEK_REVIEW_TEST_REPO", repo.path())
        .env(
            "ORVEK_REVIEW_TEST_ARTIFACTS",
            repo.temp.path().join("artifacts"),
        )
        .env("GIT_DIR", "/definitely/not/a/repository")
        .env("GIT_INDEX_FILE", "/definitely/not/an/index")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", "/definitely/not/workspace")
        .env("GIT_EXTERNAL_DIFF", "/definitely/not/a/helper")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
}
#[tokio::test]
#[ignore = "subprocess fixture for inherited environment isolation"]
async fn environment_fixture_child() {
    let path = std::env::var_os("ORVEK_REVIEW_TEST_REPO").unwrap();
    let artifacts = ArtifactStore::open(
        &std::path::PathBuf::from(std::env::var_os("ORVEK_REVIEW_TEST_ARTIFACTS").unwrap()),
        128 * 1024 * 1024,
    )
    .unwrap();
    let view = review::inspect(
        std::path::Path::new(&path),
        ReviewRange::WorkingTree { base: None },
        &artifacts,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let file = review::file(&artifacts, view.manifest, ReviewSide::After, "text.txt")
        .unwrap()
        .unwrap();
    assert_eq!(artifacts.read(file.content).unwrap(), b"changed\n");
}

#[tokio::test]
async fn immutable_snapshot_review_never_reads_current_source_and_retains_metadata() {
    use orvek_harness::workspace::{Snapshot, SnapshotPolicy};
    let repo = Repo::new();
    let before =
        Snapshot::capture(&repo.path(), SnapshotPolicy::default(), &repo.artifacts).unwrap();
    let text = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
    repo.write("text.txt", text.as_bytes());
    repo.write("run", b"#!/bin/false\n");
    fs::set_permissions(repo.path().join("run"), fs::Permissions::from_mode(0o751)).unwrap();
    fs::create_dir(repo.path().join("empty")).unwrap();
    fs::set_permissions(repo.path().join("empty"), fs::Permissions::from_mode(0o700)).unwrap();
    let after =
        Snapshot::capture(&repo.path(), SnapshotPolicy::default(), &repo.artifacts).unwrap();
    fs::remove_dir_all(repo.path()).unwrap();
    let view =
        review::inspect_snapshots(&before, &after, &repo.artifacts, &CancellationToken::new())
            .await
            .unwrap();
    let repeated =
        review::inspect_snapshots(&before, &after, &repo.artifacts, &CancellationToken::new())
            .await
            .unwrap();
    assert_eq!(view.manifest, repeated.manifest);
    assert_eq!(
        repo.bytes(view.manifest, ReviewSide::Before, "text.txt"),
        b"base\n"
    );
    assert_eq!(
        repo.bytes(view.manifest, ReviewSide::After, "text.txt"),
        text.as_bytes()
    );
    let dir = review::file(&repo.artifacts, view.manifest, ReviewSide::After, "empty")
        .unwrap()
        .unwrap();
    assert_eq!(dir.kind, review::FileKind::Directory);
    assert_eq!(dir.permissions, Some(0o700));
    assert!(view.metadata_changes.contains(&"empty".into()));
    assert!(view.metadata_changes.contains(&"run".into()));
    assert_eq!(
        review::file(&repo.artifacts, view.manifest, ReviewSide::After, "run")
            .unwrap()
            .unwrap()
            .permissions,
        Some(0o751)
    );
    let manifest = review::manifest(&repo.artifacts, view.manifest).unwrap();
    assert!(
        matches!(manifest.range,ReviewRange::Snapshots {before:base,after:candidate} if base==before.publish(&repo.artifacts).unwrap()&&candidate==after.publish(&repo.artifacts).unwrap())
    );
    let patch = String::from_utf8(repo.artifacts.read(view.patch).unwrap()).unwrap();
    assert!(patch.contains("+line 39"));
    assert!(patch.contains("new file mode 100755"));
}

#[tokio::test]
async fn snapshot_review_full_context_tamper_limits_and_paths_are_checked() {
    use orvek_harness::workspace::{Entry, Snapshot, SnapshotPolicy};
    let repo = Repo::new();
    let text = (0..40).map(|n| format!("line {n}\n")).collect::<String>();
    repo.write("text.txt", text.as_bytes());
    let before =
        Snapshot::capture(&repo.path(), SnapshotPolicy::default(), &repo.artifacts).unwrap();
    repo.write(
        "text.txt",
        text.replace("line 20\n", "changed\n").as_bytes(),
    );
    let after =
        Snapshot::capture(&repo.path(), SnapshotPolicy::default(), &repo.artifacts).unwrap();
    let view =
        review::inspect_snapshots(&before, &after, &repo.artifacts, &CancellationToken::new())
            .await
            .unwrap();
    let patch = String::from_utf8(repo.artifacts.read(view.patch).unwrap()).unwrap();
    assert!(patch.contains(" line 0\n"));
    assert!(patch.contains(" line 39\n"));
    let limits = ReviewLimits {
        max_output_bytes: 64,
        ..ReviewLimits::default()
    };
    assert!(matches!(
        review::inspect_snapshots_with_limits(
            &before,
            &after,
            &repo.artifacts,
            &CancellationToken::new(),
            limits
        )
        .await,
        Err(ReviewError::Limit(_))
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        review::inspect_snapshots(&before, &after, &repo.artifacts, &cancel).await,
        Err(ReviewError::Cancelled)
    ));
    let mut invalid = after.clone();
    invalid
        .entries
        .insert("../outside".into(), Entry::Directory { mode: 0o755 });
    assert!(matches!(
        review::inspect_snapshots(
            &before,
            &invalid,
            &repo.artifacts,
            &CancellationToken::new()
        )
        .await,
        Err(ReviewError::Path)
    ));
    let Entry::File { content, .. } = after.entries["text.txt"] else {
        panic!()
    };
    fs::write(repo.artifacts.path(content), b"tampered").unwrap();
    assert!(matches!(
        review::inspect_snapshots(&before, &after, &repo.artifacts, &CancellationToken::new())
            .await,
        Err(ReviewError::Corrupt)
    ));
}

#[tokio::test]
async fn catalog_preserves_remote_default_and_different_named_upstream_precedence() {
    let repo = Repo::new();
    let base = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "-qb", "feature"]);
    repo.write("text.txt", b"feature\n");
    repo.git(&["commit", "-qam", "feature"]);
    repo.git(&["update-ref", "refs/remotes/origin/integration", &base]);
    repo.git(&[
        "config",
        "remote.origin.url",
        "https://example.invalid/no-network",
    ]);
    repo.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    repo.git(&["config", "branch.feature.remote", "origin"]);
    repo.git(&["config", "branch.feature.merge", "refs/heads/integration"]);
    let view = review::catalog(&repo.path(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(view.default_branch.name, "refs/remotes/origin/integration");
    assert_eq!(view.default_branch.merge_base, base);
    repo.git(&["update-ref", "refs/remotes/origin/release", &base]);
    repo.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/release",
    ]);
    let view = review::catalog(&repo.path(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(view.default_branch.name, "refs/remotes/origin/release");
    repo.git(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/feature", &base]);
    repo.git(&["config", "branch.feature.merge", "refs/heads/feature"]);
    let view = review::catalog(&repo.path(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(view.default_branch.name, "refs/heads/main");
}
