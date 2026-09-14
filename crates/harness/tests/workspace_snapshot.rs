use orvek_harness::{
    Digest, Store,
    workspace::{Entry, Snapshot, SnapshotPolicy, WorkspaceError},
};
use std::{collections::BTreeMap, fs};

#[test]
fn snapshot_preserves_bytes_modes_empty_directories_and_declared_exclusions() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join("empty")).unwrap();
    fs::create_dir(source.join(".git")).unwrap();
    fs::write(source.join(".git/private"), "not a source input").unwrap();
    fs::write(source.join("program"), b"#!/bin/sh\nprintf ok\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("program"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024 * 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let snapshot = Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).unwrap();
    assert!(!snapshot.entries.keys().any(|p| p.starts_with(".git")));
    assert!(snapshot.matches(&source).unwrap());
    let digest = snapshot.publish(&artifacts).unwrap();
    let loaded = Snapshot::load(digest, &artifacts).unwrap();
    let target = root.path().join("candidate");
    loaded.materialize(&target, &artifacts, false).unwrap();
    assert_eq!(
        fs::read(target.join("program")).unwrap(),
        fs::read(source.join("program")).unwrap()
    );
    assert!(target.join("empty").is_dir());
    assert!(snapshot.matches(&target).unwrap());
    fs::write(source.join("program"), b"changed by editor").unwrap();
    assert!(!snapshot.matches(&source).unwrap());
    assert_eq!(
        fs::read(target.join("program")).unwrap(),
        b"#!/bin/sh\nprintf ok\n"
    );
}

#[test]
fn snapshot_limits_and_existing_destinations_are_enforced() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data"), vec![0; 100]).unwrap();
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let policy = SnapshotPolicy {
        max_bytes: 99,
        ..SnapshotPolicy::default()
    };
    assert!(matches!(
        Snapshot::capture(&source, policy, &artifacts),
        Err(WorkspaceError::Limit)
    ));
    let snapshot = Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).unwrap();
    assert!(matches!(
        snapshot.materialize(&source, &artifacts, false),
        Err(WorkspaceError::Destination(_))
    ));
}

#[test]
fn exact_delivery_verification_detects_additions_in_normally_excluded_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("code"), b"expected").unwrap();
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let snapshot = Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).unwrap();
    assert!(snapshot.matches_exact(&source).unwrap());
    fs::write(source.join(".env"), b"unexpected runtime configuration").unwrap();
    assert!(snapshot.matches(&source).unwrap());
    assert!(!snapshot.matches_exact(&source).unwrap());
}

#[test]
fn manifest_cannot_write_outside_destination_or_through_symlink_parent() {
    let root = tempfile::tempdir().unwrap();
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let content = Digest::of(b"owned");
    for name in ["../escape", "/escape", "link/data"] {
        let snapshot = Snapshot {
            version: 1,
            policy: SnapshotPolicy::default(),
            entries: BTreeMap::from([(
                name.into(),
                Entry::File {
                    content,
                    mode: 0o644,
                },
            )]),
        };
        assert!(snapshot.publish(&artifacts).is_err());
        assert!(
            snapshot
                .materialize(&root.path().join("destination"), &artifacts, false)
                .is_err()
        );
    }
    assert!(!root.path().join("escape").exists());
}

#[cfg(unix)]
#[test]
fn relative_symlinks_are_preserved_but_escape_and_chain_are_rejected() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), "data").unwrap();
    symlink("file", source.join("alias")).unwrap();
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let snapshot = Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).unwrap();
    let target = root.path().join("candidate");
    snapshot.materialize(&target, &artifacts, false).unwrap();
    assert_eq!(
        fs::read_link(target.join("alias"))
            .unwrap()
            .to_str()
            .unwrap(),
        "file"
    );
    symlink("../host-secret", source.join("escape")).unwrap();
    assert!(Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).is_err());
    fs::remove_file(source.join("escape")).unwrap();
    symlink("alias", source.join("chain")).unwrap();
    assert!(Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts).is_err());
}

#[cfg(unix)]
#[test]
fn raced_symlink_never_copies_outside_bytes() {
    use std::{
        os::unix::fs::symlink,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let secret = b"outside-secret-never-an-input";
    fs::write(root.path().join("secret"), secret).unwrap();
    fs::write(source.join("file"), b"inside").unwrap();
    let artifacts_root = root.path().join("artifacts");
    let artifacts = Store::open_with_artifact_limit(root.path(), 1024)
        .unwrap()
        .public_artifacts()
        .clone();
    let active = Arc::new(AtomicBool::new(true));
    let runner_active = active.clone();
    let runner_source = source.clone();
    let writer = std::thread::spawn(move || {
        while runner_active.load(Ordering::Relaxed) {
            let next = runner_source.join("next");
            let _ = fs::remove_file(&next);
            let _ = symlink("../secret", &next);
            let _ = fs::rename(&next, runner_source.join("file"));
            let _ = fs::write(&next, b"inside");
            let _ = fs::rename(&next, runner_source.join("file"));
        }
    });
    for _ in 0..50 {
        let _ = Snapshot::capture(&source, SnapshotPolicy::default(), &artifacts);
    }
    active.store(false, Ordering::Relaxed);
    writer.join().unwrap();
    assert!(!artifacts_root.join(Digest::of(secret).to_string()).exists());
}
