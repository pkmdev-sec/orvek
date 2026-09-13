use std::collections::BTreeMap;
use orvek_harness::{
    Digest,
    workspace::{Entry, Snapshot, SnapshotPolicy, WorkspaceError},
};
fn file(text: &str, mode: u32) -> Entry {
    Entry::File {
        content: Digest::of(text.as_bytes()),
        mode,
    }
}
fn snapshot(entries: &[(&str, Entry)]) -> Snapshot {
    Snapshot {
        version: 1,
        policy: SnapshotPolicy {
            excluded_roots: Vec::new(),
            max_files: 1000,
            max_bytes: 1024 * 1024,
        },
        entries: entries
            .iter()
            .map(|(name, entry)| (name.to_string(), entry.clone()))
            .collect(),
    }
}
#[test]
fn independent_content_permissions_additions_and_deletions_merge_without_mutating_inputs() {
    let origin = snapshot(&[
        ("edit", file("base", 0o644)),
        ("delete", file("base", 0o644)),
        ("same", file("base", 0o644)),
    ]);
    let mut private = origin.clone();
    private
        .entries
        .insert("edit".into(), file("private", 0o644));
    private.entries.remove("delete");
    private
        .entries
        .insert("private".into(), file("added", 0o600));
    let mut user = origin.clone();
    user.entries.insert("edit".into(), file("base", 0o755));
    user.entries.insert("user".into(), file("added", 0o644));
    let original_inputs = (origin.clone(), private.clone(), user.clone());
    let merged = Snapshot::reconcile(&origin, &private, &user).unwrap();
    assert_eq!(merged.entries["edit"], file("private", 0o755));
    assert!(!merged.entries.contains_key("delete"));
    assert!(merged.entries.contains_key("private"));
    assert!(merged.entries.contains_key("user"));
    assert_eq!(
        original_inputs,
        (origin.clone(), private.clone(), user.clone())
    );
    assert_eq!(
        Snapshot::reconcile(&origin, &user, &private).unwrap(),
        merged
    );
    assert_eq!(
        Snapshot::reconcile(&origin, &merged, &user).unwrap(),
        merged
    );
}
#[test]
fn identical_changes_converge_and_divergent_content_or_modes_conflict() {
    let origin = snapshot(&[("file", file("base", 0o644))]);
    let private = snapshot(&[("file", file("same", 0o755))]);
    assert_eq!(
        Snapshot::reconcile(&origin, &private, &private).unwrap(),
        private
    );
    for user in [
        snapshot(&[("file", file("different", 0o644))]),
        snapshot(&[("file", file("base", 0o700))]),
        snapshot(&[]),
    ] {
        assert!(
            matches!(Snapshot::reconcile(&origin,&private,&user),Err(WorkspaceError::ReconcileConflict{paths,truncated:false}) if paths==vec!["file"])
        );
    }
}
#[test]
fn directory_deletion_conflicts_with_independent_new_descendant() {
    let origin = snapshot(&[("dir", Entry::Directory { mode: 0o755 })]);
    let mut private = origin.clone();
    private.entries.insert("dir/new".into(), file("new", 0o644));
    let user = snapshot(&[]);
    assert!(
        matches!(Snapshot::reconcile(&origin,&private,&user),Err(WorkspaceError::ReconcileConflict{paths,..}) if paths==vec!["dir","dir/new"])
    );
}
#[test]
fn replacing_directory_with_file_cannot_merge_a_private_descendant() {
    let origin = snapshot(&[("dir", Entry::Directory { mode: 0o755 })]);
    let mut private = origin.clone();
    private.entries.insert("dir/new".into(), file("new", 0o644));
    let user = snapshot(&[("dir", file("replacement", 0o644))]);
    assert!(matches!(
        Snapshot::reconcile(&origin, &private, &user),
        Err(WorkspaceError::ReconcileConflict { .. })
    ));
}
#[test]
fn symlink_target_removed_by_other_side_is_a_conflict() {
    let origin = snapshot(&[("target", file("base", 0o644))]);
    let private = snapshot(&[]);
    let mut user = origin.clone();
    user.entries.insert(
        "link".into(),
        Entry::Symlink {
            target: "target".into(),
        },
    );
    assert!(
        matches!(Snapshot::reconcile(&origin,&private,&user),Err(WorkspaceError::ReconcileConflict{paths,..}) if paths==vec!["link","target"])
    );
}
#[test]
fn conflicts_are_bounded_and_never_partial_success() {
    let mut origin = snapshot(&[]);
    let mut private = origin.clone();
    let mut user = origin.clone();
    for n in 0..100 {
        let path = format!("file-{n:03}");
        origin.entries.insert(path.clone(), file("base", 0o644));
        private.entries.insert(path.clone(), file("left", 0o644));
        user.entries.insert(path, file("right", 0o644));
    }
    assert!(
        matches!(Snapshot::reconcile(&origin,&private,&user),Err(WorkspaceError::ReconcileConflict{paths,truncated:true}) if paths.len()==64&&paths[0]=="file-000")
    );
}
#[test]
fn policies_modes_paths_versions_and_merged_entry_counts_are_validated() {
    let origin = snapshot(&[]);
    let mut invalid = origin.clone();
    invalid.policy.max_bytes += 1;
    assert!(matches!(
        Snapshot::reconcile(&origin, &invalid, &origin),
        Err(WorkspaceError::ReconcilePolicy)
    ));
    invalid = origin.clone();
    invalid.version = 9;
    assert!(matches!(
        Snapshot::reconcile(&origin, &invalid, &origin),
        Err(WorkspaceError::Version)
    ));
    for entry in [
        ("../escape", file("data", 0o644)),
        ("bad", file("data", 0o100644)),
        ("child/file", file("data", 0o644)),
        (
            "link",
            Entry::Symlink {
                target: "/outside".into(),
            },
        ),
    ] {
        invalid = snapshot(&[entry]);
        assert!(matches!(
            Snapshot::reconcile(&origin, &invalid, &origin),
            Err(WorkspaceError::UnsafePath(_))
        ));
    }
    let mut small = origin.clone();
    small.policy.max_files = 1;
    let mut left = small.clone();
    let mut right = small.clone();
    left.entries.insert("a".into(), file("a", 0o644));
    right.entries.insert("b".into(), file("b", 0o644));
    assert!(matches!(
        Snapshot::reconcile(&small, &left, &right),
        Err(WorkspaceError::Limit)
    ));
    assert_eq!(
        Snapshot::reconcile(&small, &small, &small).unwrap().entries,
        BTreeMap::new()
    );
}
