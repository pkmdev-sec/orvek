//! Bounded, searchable workspace file discovery.

use std::{collections::VecDeque, ffi::OsStr, fs, path::Path};

pub(crate) const MAX_FILE_INDEX_ENTRIES: usize = 50_000;

const SKIPPED_DIRECTORIES: [&str; 4] = [".git", ".jj", "node_modules", "target"];

#[derive(Debug)]
pub(crate) struct FileIndex {
    entries: Vec<FileIndexEntry>,
    truncated: bool,
}

impl FileIndex {
    pub(crate) fn from_paths(mut paths: Vec<String>, truncated: bool) -> Self {
        paths.sort_unstable();
        paths.dedup();
        let entries = paths
            .into_iter()
            .map(|path| {
                let search_text = path.to_ascii_lowercase();
                FileIndexEntry { path, search_text }
            })
            .collect();
        Self { entries, truncated }
    }

    pub(crate) fn entries(&self) -> &[FileIndexEntry] {
        &self.entries
    }

    pub(crate) fn is_truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug)]
pub(crate) struct FileIndexEntry {
    path: String,
    search_text: String,
}

impl FileIndexEntry {
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn search_text(&self) -> &str {
        &self.search_text
    }
}

pub(crate) fn discover_file_index(workspace: &Path) -> FileIndex {
    discover_file_index_with_limit(workspace, MAX_FILE_INDEX_ENTRIES)
}

fn discover_file_index_with_limit(workspace: &Path, limit: usize) -> FileIndex {
    let mut directories = VecDeque::from([workspace.to_path_buf()]);
    let mut paths = Vec::new();

    while let Some(directory) = directories.pop_front() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }

            let is_directory = file_type.is_dir();
            if is_directory && is_skipped_directory(&entry) {
                continue;
            }
            if !is_directory && !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            let Some(mut relative) = relative_path(workspace, &path) else {
                continue;
            };
            if is_directory {
                relative.push('/');
            }

            if paths.len() == limit {
                return FileIndex::from_paths(paths, true);
            }
            paths.push(relative);

            if is_directory {
                directories.push_back(path);
            }
        }
    }

    FileIndex::from_paths(paths, false)
}

fn relative_path(workspace: &Path, path: &Path) -> Option<String> {
    let relative = path
        .strip_prefix(workspace)
        .ok()?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    (!relative.chars().any(char::is_control)).then_some(relative)
}

fn is_skipped_directory(entry: &fs::DirEntry) -> bool {
    let file_name = entry.file_name();
    SKIPPED_DIRECTORIES
        .iter()
        .any(|name| file_name == OsStr::new(name))
}

#[cfg(test)]
mod tests {
    use super::{FileIndex, discover_file_index, discover_file_index_with_limit};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn from_paths_sorts_deduplicates_and_precomputes_search_text() {
        let index = FileIndex::from_paths(
            vec![
                "Zoo/FILE.rs".to_owned(),
                "alpha.txt".to_owned(),
                "Zoo/FILE.rs".to_owned(),
            ],
            true,
        );

        let entries = index.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path(), "Zoo/FILE.rs");
        assert_eq!(entries[0].search_text(), "zoo/file.rs");
        assert_eq!(entries[1].path(), "alpha.txt");
        assert_eq!(entries[1].search_text(), "alpha.txt");
        assert!(index.is_truncated());
    }

    #[test]
    fn discovery_returns_relative_paths_and_skips_large_generated_trees() {
        let workspace = tempdir().expect("create workspace");
        fs::write(workspace.path().join("root.txt"), "root").expect("write root file");
        fs::create_dir_all(workspace.path().join("visible/sub")).expect("create visible tree");
        fs::write(workspace.path().join("visible/sub/file.rs"), "source")
            .expect("write visible file");

        for skipped in [".git", ".jj", "node_modules", "target"] {
            let directory = workspace.path().join(skipped);
            fs::create_dir_all(&directory).expect("create skipped directory");
            fs::write(directory.join("hidden.txt"), "hidden").expect("write skipped file");
        }

        let index = discover_file_index(workspace.path());
        let paths = index
            .entries()
            .iter()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                "root.txt",
                "visible/",
                "visible/sub/",
                "visible/sub/file.rs"
            ]
        );
        assert!(!index.is_truncated());
    }

    #[test]
    fn injectable_limit_caps_entries_and_reports_only_real_truncation() {
        let workspace = tempdir().expect("create workspace");
        for name in ["one", "two", "three"] {
            fs::write(workspace.path().join(name), name).expect("write file");
        }

        let capped = discover_file_index_with_limit(workspace.path(), 2);
        assert_eq!(capped.entries().len(), 2);
        assert!(capped.is_truncated());

        let exact = discover_file_index_with_limit(workspace.path(), 3);
        assert_eq!(exact.entries().len(), 3);
        assert!(!exact.is_truncated());
    }

    #[cfg(unix)]
    #[test]
    fn discovery_rejects_control_characters_and_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempdir().expect("create workspace");
        fs::create_dir_all(workspace.path().join("bad\nname")).expect("create control directory");
        fs::write(workspace.path().join("bad\nname/hidden"), "hidden").expect("write control path");
        fs::create_dir_all(workspace.path().join("real")).expect("create real directory");
        fs::write(workspace.path().join("real/file"), "visible").expect("write real file");
        symlink(
            workspace.path().join("real"),
            workspace.path().join("linked"),
        )
        .expect("create directory symlink");

        let index = discover_file_index(workspace.path());
        let paths = index
            .entries()
            .iter()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();

        assert_eq!(paths, ["real/", "real/file"]);
    }
}
