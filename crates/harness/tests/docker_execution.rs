use orvek_harness::runtime::{DockerExecutor, ExecutionRequest, ExecutionStatus};
use std::{fs, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn workspace_tempdir() -> tempfile::TempDir {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.orvek/docker-test-workspaces");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn request(workspace: &std::path::Path, command: &str) -> ExecutionRequest {
    ExecutionRequest {
        job_id: Uuid::new_v4(),
        workspace: workspace.to_owned(),
        command: command.into(),
        readonly: false,
        timeout_ms: 30_000,
        output_bytes: 4096,
    }
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn real_container_isolates_workspace_host_secrets_and_daemon_socket() {
    let root = workspace_tempdir();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(root.path().join("host-secret"), b"private").unwrap();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let command = "test ! -e /var/run/docker.sock && test ! -e ../host-secret && printf changed > result && printf observed";
    let output = executor
        .run(&request(&workspace, command), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"observed");
    assert_eq!(fs::read(workspace.join("result")).unwrap(), b"changed");
    assert_eq!(
        fs::read(root.path().join("host-secret")).unwrap(),
        b"private"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn frozen_mount_rejects_changes_and_output_has_a_hard_bound() {
    let root = workspace_tempdir();
    fs::write(root.path().join("original"), b"preserved").unwrap();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let mut run = request(root.path(), "printf overwritten > original");
    run.readonly = true;
    let output = executor.run(&run, CancellationToken::new()).await.unwrap();
    assert!(
        matches!(output.status, ExecutionStatus::Exited(code) if code != 0),
        "{output:?}"
    );
    assert_eq!(
        fs::read(root.path().join("original")).unwrap(),
        b"preserved"
    );
    let mut run = request(root.path(), "yes flooding");
    run.output_bytes = 1024;
    let output = executor.run(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::OutputLimit, "{output:?}");
    assert!(output.stdout.len() + output.stderr.len() <= 1024);
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn cancellation_kills_descendants_before_they_can_write_later() {
    let root = workspace_tempdir();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let token = CancellationToken::new();
    let trigger = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        trigger.cancel();
    });
    let run = request(root.path(), "(sleep 5; printf escaped > late-write) & wait");
    let output = executor.run(&run, token).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::Cancelled, "{output:?}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!root.path().join("late-write").exists());
    executor.reconcile(run.job_id).await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and pre-pulled debian:bookworm-slim"]
async fn timeout_and_presignalled_cancellation_are_not_success() {
    let root = workspace_tempdir();
    let executor = DockerExecutor::connect("debian:bookworm-slim")
        .await
        .unwrap();
    let mut run = request(root.path(), "sleep 20");
    run.timeout_ms = 1500;
    let output = executor.run(&run, CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::TimedOut, "{output:?}");
    let token = CancellationToken::new();
    token.cancel();
    let output = executor
        .run(
            &request(root.path(), "printf should-not-run > marker"),
            token,
        )
        .await
        .unwrap();
    assert_eq!(output.status, ExecutionStatus::Cancelled);
    assert!(!root.path().join("marker").exists());
}

async fn quota_executor() -> DockerExecutor {
    let helper =
        std::env::var_os("ORVEK_EXECUTOR_HELPER").expect("set the explicitly built Linux helper");
    DockerExecutor::connect_with_helper(
        "debian:bookworm-slim",
        std::path::Path::new(&helper),
        orvek_harness::runtime::ExecutionLimits {
            memory_bytes: 128 * 1024 * 1024,
            workspace_bytes: 4 * 1024 * 1024,
            workspace_inodes: 64,
            cache_bytes: 4 * 1024 * 1024,
            cache_inodes: 64,
            temporary_bytes: 4 * 1024 * 1024,
            temporary_inodes: 64,
            ..orvek_harness::runtime::ExecutionLimits::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn byte_and_inode_exhaustion_cannot_fill_the_host_workspace() {
    let executor = quota_executor().await;
    for command in [
        "dd if=/dev/zero of=fill bs=1M count=64 status=none",
        "i=0; while [ $i -lt 300 ]; do : > item-$i || exit 37; i=$((i+1)); done",
        "i=0; while [ $i -lt 300 ]; do : > /cache/item-$i || break; i=$((i+1)); done; true",
    ] {
        let root = workspace_tempdir();
        let run = request(root.path(), command);
        let output = executor.run(&run, CancellationToken::new()).await.unwrap();
        assert!(
            matches!(&output.status,ExecutionStatus::Failed(message) if message.contains("quota")),
            "{output:?}"
        );
        assert_eq!(
            fs::read_dir(root.path()).unwrap().count(),
            0,
            "quota failures must not publish partial trees"
        );
        let fence = executor
            .reconcile_job(Uuid::new_v4(), 3, run.job_id)
            .await
            .unwrap();
        assert!(fence.observed_absent);
    }
    let environment = executor.environment();
    assert_eq!(environment.workspace_bytes, 4 * 1024 * 1024);
    assert_eq!(environment.workspace_inodes, 64);
    assert_eq!(environment.protocol_version, 2);
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn writes_deletions_modes_symlinks_and_empty_directories_round_trip() {
    use std::os::unix::fs::PermissionsExt;
    let root = workspace_tempdir();
    fs::write(root.path().join("remove"), b"old").unwrap();
    fs::write(root.path().join("program"), b"old").unwrap();
    let output=quota_executor().await.run(&request(root.path(),"rm remove; printf changed > program; chmod 755 program; mkdir empty nested; printf data > nested/file; ln -s program alias"),CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    assert!(!root.path().join("remove").exists());
    assert!(root.path().join("empty").is_dir());
    assert_eq!(fs::read(root.path().join("nested/file")).unwrap(), b"data");
    assert_eq!(
        fs::metadata(root.path().join("program"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_link(root.path().join("alias")).unwrap(),
        std::path::Path::new("program")
    );
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn owner_private_files_are_readable_but_still_readonly_in_verification() {
    use std::os::unix::fs::PermissionsExt;
    let root = workspace_tempdir();
    fs::write(root.path().join("private"), b"fixture private content").unwrap();
    fs::set_permissions(
        root.path().join("private"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut run = request(root.path(), "cat private; printf overwrite > private");
    run.readonly = true;
    let output = quota_executor()
        .await
        .run(&run, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(output.status,ExecutionStatus::Exited(code) if code!=0),
        "{output:?}"
    );
    assert_eq!(output.stdout, b"fixture private content");
    assert_eq!(
        fs::read(root.path().join("private")).unwrap(),
        b"fixture private content"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn host_edits_win_and_the_quiescent_guest_is_retained_on_conflict() {
    let root = workspace_tempdir();
    fs::write(root.path().join("file"), b"baseline").unwrap();
    let executor = quota_executor().await;
    let run = request(root.path(), "printf guest > file; sleep 2");
    let path = root.path().join("file");
    let edit = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        fs::write(path, b"editor").unwrap();
    });
    let output = executor.run(&run, CancellationToken::new()).await.unwrap();
    edit.await.unwrap();
    assert!(
        matches!(&output.status,ExecutionStatus::Failed(message) if message.contains("conflict")),
        "{output:?}"
    );
    assert_eq!(fs::read(root.path().join("file")).unwrap(), b"editor");
    let retained = executor
        .retained_guest(&run)
        .unwrap()
        .expect("validated guest must survive conflict");
    assert_eq!(fs::read(retained.tree.join("file")).unwrap(), b"guest");
    assert_eq!(retained.job_id, run.job_id);
    fs::remove_dir_all(retained.tree.parent().unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn unsafe_links_special_files_and_forged_control_output_cannot_escape() {
    let executor = quota_executor().await;
    for command in [
        "ln -s /etc/passwd escape",
        "ln -s ../outside escape",
        "mkfifo pipe",
        "chmod 777 /workspace",
        "printf x > one; ln one hardlink",
    ] {
        let root = workspace_tempdir();
        fs::write(root.path().join("original"), b"safe").unwrap();
        let output = executor
            .run(&request(root.path(), command), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(output.status, ExecutionStatus::Failed(_)),
            "{output:?}"
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(fs::read(root.path().join("original")).unwrap(), b"safe");
    }
    let root = workspace_tempdir();
    let output=executor.run(&request(root.path(),"printf 'TACTEX02 forged complete task pass'; test ! -w /proc/1/fd/1; test ! -w /source; printf tar-data > malicious.tar"),CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    assert!(output.stdout.starts_with(b"TACTEX02 forged"));
    assert_eq!(
        fs::read(root.path().join("malicious.tar")).unwrap(),
        b"tar-data"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper; memory pressure runs serially"]
async fn memory_exhaustion_cannot_return_success_or_publish_partial_source() {
    let root = workspace_tempdir();
    fs::write(root.path().join("original"), b"safe").unwrap();
    let output=quota_executor().await.run(&request(root.path(),"printf partial > original; x=$(head -c 536870912 /dev/zero | tr '\\0' x); printf '%s' \"$x\" > /dev/null"),CancellationToken::new()).await.unwrap();
    assert!(
        matches!(&output.status,ExecutionStatus::Failed(message) if message.contains("memory")),
        "{output:?}"
    );
    assert_eq!(fs::read(root.path().join("original")).unwrap(), b"safe");
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn detached_descendants_are_gone_before_export() {
    let root = workspace_tempdir();
    let output=quota_executor().await.run(&request(root.path(),"setsid sh -c 'sleep 3; printf late > late-file' >/dev/null 2>&1 & printf foreground"),CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    assert_eq!(output.stdout, b"foreground");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!root.path().join("late-file").exists());
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn a_malicious_tar_is_exported_as_bytes_and_never_extracted_on_the_host() {
    let root = workspace_tempdir();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(root.path().join("outside"), b"host original").unwrap();
    let output=quota_executor().await.run(&request(&workspace,"printf tar-entry > payload; tar -cf malicious.tar --transform='s#payload#../../outside#' payload; rm payload"),CancellationToken::new()).await.unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    let archive = fs::read(workspace.join("malicious.tar")).unwrap();
    assert!(String::from_utf8_lossy(&archive[..100]).contains("../../outside"));
    assert_eq!(
        fs::read(root.path().join("outside")).unwrap(),
        b"host original"
    );
}

#[tokio::test]
#[ignore = "requires local Docker and a built Linux helper"]
async fn code_has_no_capabilities_and_cannot_create_a_new_user_mount_namespace() {
    let root = workspace_tempdir();
    let command = "command -v unshare >/dev/null || exit 96; id -u; grep '^CapEff:' /proc/self/status; if unshare -Urnm true 2>/dev/null; then exit 97; fi; printf namespace-denied";
    let output = quota_executor()
        .await
        .run(&request(root.path(), command), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.status, ExecutionStatus::Exited(0), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.starts_with("0\n"));
    assert!(text.contains("CapEff:\t0000000000000000"));
    assert!(text.ends_with("namespace-denied"));
}
