//! Pure three-way merge of immutable workspace entries. No materialization occurs.
use super::{Entry, Snapshot, WorkspaceError, resolve_link, safe_relative};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const MAX_CONFLICT_PATHS: usize = 64;
impl Snapshot {
    /// Merge independent private and user changes against the original snapshot.
    ///
    /// Content digests and file permissions merge separately. The caller must
    /// validate aggregate artifact bytes before materializing the result: a
    /// Snapshot records content digests, not their byte lengths.
    pub fn reconcile(
        origin: &Self,
        private_head: &Self,
        current_user: &Self,
    ) -> Result<Self, WorkspaceError> {
        for snapshot in [origin, private_head, current_user] {
            validate(snapshot)?;
        }
        if origin.policy != private_head.policy || origin.policy != current_user.policy {
            return Err(WorkspaceError::ReconcilePolicy);
        }
        let paths = origin
            .entries
            .keys()
            .chain(private_head.entries.keys())
            .chain(current_user.entries.keys())
            .collect::<BTreeSet<_>>();
        let mut entries = BTreeMap::new();
        let mut conflicts = Conflicts::default();
        for path in paths {
            match merge_entry(
                origin.entries.get(path),
                private_head.entries.get(path),
                current_user.entries.get(path),
            ) {
                Ok(Some(entry)) => {
                    entries.insert(path.clone(), entry);
                }
                Ok(None) => {}
                Err(()) => conflicts.add(path),
            }
        }
        conflicts.finish()?;
        if entries.len() > origin.policy.max_files {
            return Err(WorkspaceError::Limit);
        }
        let mut conflicts = Conflicts::default();
        for (path, entry) in &entries {
            for parent in Path::new(path)
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                let parent = parent.to_str().expect("ancestor of UTF-8 path");
                if !matches!(entries.get(parent), Some(Entry::Directory { .. })) {
                    conflicts.add(path);
                    conflicts.add(parent);
                }
            }
            if let Entry::Symlink { target } = entry {
                let resolved = resolve_link(Path::new(path), target)?;
                if !matches!(
                    entries.get(&resolved),
                    Some(Entry::File { .. } | Entry::Directory { .. })
                ) {
                    conflicts.add(path);
                    conflicts.add(&resolved);
                }
            }
        }
        conflicts.finish()?;
        let result = Self {
            version: origin.version,
            policy: origin.policy.clone(),
            entries,
        };
        result.validate()?;
        Ok(result)
    }
}
fn merge_entry(
    origin: Option<&Entry>,
    private: Option<&Entry>,
    user: Option<&Entry>,
) -> Result<Option<Entry>, ()> {
    if private == user {
        return Ok(private.cloned());
    }
    if private == origin {
        return Ok(user.cloned());
    }
    if user == origin {
        return Ok(private.cloned());
    }
    match (origin, private, user) {
        (
            Some(Entry::File {
                content: base,
                mode: base_mode,
            }),
            Some(Entry::File {
                content: left,
                mode: left_mode,
            }),
            Some(Entry::File {
                content: right,
                mode: right_mode,
            }),
        ) => Ok(Some(Entry::File {
            content: merge(base, left, right)?,
            mode: merge(base_mode, left_mode, right_mode)?,
        })),
        _ => Err(()),
    }
}
fn merge<T: Copy + Eq>(origin: &T, private: &T, user: &T) -> Result<T, ()> {
    if private == user || user == origin {
        Ok(*private)
    } else if private == origin {
        Ok(*user)
    } else {
        Err(())
    }
}
fn validate(snapshot: &Snapshot) -> Result<(), WorkspaceError> {
    if snapshot.policy.max_files == 0 || snapshot.policy.max_bytes == 0 {
        return Err(WorkspaceError::Limit);
    }
    for excluded in &snapshot.policy.excluded_roots {
        if excluded.len() > 4096
            || Path::new(excluded).components().count() != 1
            || !safe_relative(Path::new(excluded))
        {
            return Err(WorkspaceError::UnsafePath(excluded.into()));
        }
    }
    for (path, entry) in &snapshot.entries {
        if path.len() > 4096
            || path.contains('\0')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(WorkspaceError::UnsafePath(path.into()));
        }
        match entry {
            Entry::File { mode, .. } | Entry::Directory { mode } if *mode > 0o7777 => {
                return Err(WorkspaceError::UnsafePath(path.into()));
            }
            Entry::Symlink { target }
                if target.is_empty() || target.len() > 4096 || target.contains('\0') =>
            {
                return Err(WorkspaceError::UnsafePath(path.into()));
            }
            _ => {}
        }
    }
    snapshot.validate()
}
#[derive(Default)]
struct Conflicts {
    paths: BTreeSet<String>,
    truncated: bool,
}
impl Conflicts {
    fn add(&mut self, path: &str) {
        if self.paths.len() < MAX_CONFLICT_PATHS {
            self.paths.insert(path.into());
        } else if !self.paths.contains(path) {
            self.truncated = true;
        }
    }
    fn finish(self) -> Result<(), WorkspaceError> {
        if self.paths.is_empty() {
            Ok(())
        } else {
            Err(WorkspaceError::ReconcileConflict {
                paths: self.paths.into_iter().collect(),
                truncated: self.truncated,
            })
        }
    }
}
