use super::{
    Control, Digest, Expected, MAX_FILE_BYTES, OUTPUT_METADATA_BYTES, ReadArgs, SearchArgs,
    ToolContext, ToolError, WriteArgs, encoded,
};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{self, AtFlags, Dev, Dir, FileType, Mode, OFlags, RawMode, RenameFlags, Stat},
    io::Errno,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};
use uuid::Uuid;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const FILE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

pub(super) struct Workspace {
    root: OwnedFd,
    host_parent: OwnedFd,
    device: Dev,
}

#[derive(Clone, Copy, Serialize)]
struct FileIdentity {
    digest: Digest,
    bytes: usize,
    mode: RawMode,
}
struct OpenFile {
    file: File,
    stat: Stat,
    bytes: Vec<u8>,
    identity: FileIdentity,
}

impl Workspace {
    pub fn open(path: &Path) -> Result<Self, ToolError> {
        if path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(ToolError::PathDenied);
        }
        let parent = path.parent().ok_or(ToolError::PathDenied)?;
        let name = path.file_name().ok_or(ToolError::PathDenied)?;
        let host_parent = fs::open(parent, DIRECTORY_FLAGS, Mode::empty()).map_err(map_error)?;
        let root =
            fs::openat(&host_parent, name, DIRECTORY_FLAGS, Mode::empty()).map_err(map_error)?;
        let device = fs::fstat(&root).map_err(map_error)?.st_dev;
        Ok(Self {
            root,
            host_parent,
            device,
        })
    }

    pub fn read(
        &self,
        args: ReadArgs,
        context: &ToolContext,
        control: &Control<'_>,
    ) -> Result<Value, ToolError> {
        if args.max_bytes == 0 || args.max_bytes > MAX_FILE_BYTES {
            return Err(ToolError::InvalidArguments);
        }
        let budget = self.path_budget(&args.path, context)?;
        let (parent, name) = self.parent(&args.path)?;
        let opened = self
            .open_file(&parent, &name, control, MAX_FILE_BYTES)?
            .ok_or(ToolError::NotFound)?;
        if args.offset > opened.bytes.len() {
            return Err(ToolError::InvalidArguments);
        }
        let length = args
            .max_bytes
            .min(budget)
            .min(opened.bytes.len() - args.offset);
        let end = args.offset + length;
        Ok(
            json!({"path":args.path,"identity":opened.identity,"offset":args.offset,"content":encoded(&opened.bytes[args.offset..end]),"truncated":end < opened.bytes.len(),"eof":end == opened.bytes.len()}),
        )
    }

    pub fn write(
        &self,
        args: WriteArgs,
        context: &ToolContext,
        control: &Control<'_>,
    ) -> Result<Value, ToolError> {
        let (path, expected, content) = match args {
            WriteArgs::Replace {
                path,
                expected,
                content,
            } => (path, expected, Some(content)),
            WriteArgs::Delete { path, expected } => (path, expected, None),
        };
        self.path_budget(&path, context)?;
        if content.as_ref().is_some_and(|s| s.len() > MAX_FILE_BYTES) {
            return Err(ToolError::FileTooLarge);
        }
        let (parent, name) = self.parent(&path)?;
        let original = self.open_file(&parent, &name, control, MAX_FILE_BYTES)?;
        check_expected(expected, original.as_ref())?;
        let before = original.as_ref().map(|f| f.identity);
        if content.is_none() && original.is_none() {
            return Ok(
                json!({"path":path,"before":before,"after":Value::Null,"changed":false,"durable":true}),
            );
        }
        let mode = before.map_or(0o644, |before| before.mode);
        let after = content.as_ref().map(|content| FileIdentity {
            digest: Digest::of(content.as_bytes()),
            bytes: content.len(),
            mode,
        });
        if let Some(content) = content {
            let stage = Staging::new(&self.host_parent, self.device)?;
            let mut file = File::from(
                fs::openat(
                    &stage.directory,
                    "content",
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(map_error)?,
            );
            for chunk in content.as_bytes().chunks(64 * 1024) {
                control.check()?;
                file.write_all(chunk)?;
            }
            fs::fchmod(&file, Mode::from_raw_mode(mode)).map_err(map_error)?;
            file.sync_all()?;
            control.check()?;
            self.revalidate(&path, &parent, &name, original.as_ref(), expected, control)?;
            control.check()?;
            match expected {
                Expected::Absent => fs::renameat_with(
                    &stage.directory,
                    "content",
                    &parent,
                    &name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(map_error)?,
                Expected::Digest { .. } => {
                    fs::renameat(&stage.directory, "content", &parent, &name).map_err(map_error)?
                }
            }
        } else {
            self.revalidate(&path, &parent, &name, original.as_ref(), expected, control)?;
            control.check()?;
            fs::unlinkat(&parent, &name, AtFlags::empty()).map_err(map_error)?;
        }
        // Publication already happened. Preserve its result if durability cannot
        // be confirmed, rather than returning a retryable pre-mutation failure.
        let durable = fs::fsync(&parent).is_ok();
        Ok(json!({"path":path,"before":before,"after":after,"changed":true,"durable":durable}))
    }

    fn revalidate(
        &self,
        path: &str,
        parent: &OwnedFd,
        name: &OsStr,
        original: Option<&OpenFile>,
        expected: Expected,
        control: &Control<'_>,
    ) -> Result<(), ToolError> {
        let (current_parent, _) = self.parent(path)?;
        if !same_inode(
            &fs::fstat(parent).map_err(map_error)?,
            &fs::fstat(&current_parent).map_err(map_error)?,
        ) {
            return Err(ToolError::Conflict);
        }
        let current = self.open_file(parent, name, control, MAX_FILE_BYTES)?;
        check_expected(expected, current.as_ref())?;
        if let (Some(original), Some(current)) = (original, current.as_ref())
            && (!same_inode(&original.stat, &current.stat)
                || !same_snapshot(
                    &original.stat,
                    &fs::fstat(&original.file).map_err(map_error)?,
                ))
        {
            return Err(ToolError::Conflict);
        }
        Ok(())
    }

    fn path_budget(&self, path: &str, context: &ToolContext) -> Result<usize, ToolError> {
        let overhead = serde_json::to_vec(path)
            .map_err(|_| ToolError::InvalidArguments)?
            .len()
            .saturating_add(OUTPUT_METADATA_BYTES);
        if overhead > context.max_output_bytes {
            return Err(ToolError::OutputBudget);
        }
        Ok((context.max_output_bytes - overhead) / 6)
    }

    fn parent(&self, path: &str) -> Result<(OwnedFd, OsString), ToolError> {
        let mut components = relative(path, false)?;
        let name = components.pop().ok_or(ToolError::PathDenied)?;
        Ok((self.directory(&components)?, name))
    }

    fn directory(&self, components: &[OsString]) -> Result<OwnedFd, ToolError> {
        let mut directory =
            fs::openat(&self.root, ".", DIRECTORY_FLAGS, Mode::empty()).map_err(map_error)?;
        for component in components {
            directory = fs::openat(&directory, component, DIRECTORY_FLAGS, Mode::empty())
                .map_err(map_error)?;
            if fs::fstat(&directory).map_err(map_error)?.st_dev != self.device {
                return Err(ToolError::PathDenied);
            }
        }
        Ok(directory)
    }

    fn open_file(
        &self,
        parent: impl AsFd,
        name: &OsStr,
        control: &Control<'_>,
        max_bytes: usize,
    ) -> Result<Option<OpenFile>, ToolError> {
        control.check()?;
        let descriptor = match fs::openat(parent, name, FILE_FLAGS, Mode::empty()) {
            Ok(file) => file,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(map_error(error)),
        };
        let stat = fs::fstat(&descriptor).map_err(map_error)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_dev != self.device
        {
            return Err(ToolError::UnsupportedFile);
        }
        if stat.st_size < 0 || stat.st_size as u64 > max_bytes as u64 {
            return Err(ToolError::FileTooLarge);
        }
        let mut file = File::from(descriptor);
        let mut bytes = Vec::with_capacity(stat.st_size as usize);
        let mut buffer = [0u8; 64 * 1024];
        while bytes.len() < max_bytes {
            control.check()?;
            let remaining = (max_bytes - bytes.len()).min(buffer.len());
            let size = file.read(&mut buffer[..remaining])?;
            if size == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..size]);
        }
        if bytes.len() != stat.st_size as usize
            || !same_snapshot(&stat, &fs::fstat(&file).map_err(map_error)?)
        {
            return Err(ToolError::Conflict);
        }
        let identity = FileIdentity {
            digest: Digest::of(&bytes),
            bytes: bytes.len(),
            mode: stat.st_mode & 0o7777,
        };
        Ok(Some(OpenFile {
            file,
            stat,
            bytes,
            identity,
        }))
    }

    pub fn search(
        &self,
        args: SearchArgs,
        context: &ToolContext,
        control: &Control<'_>,
    ) -> Result<Value, ToolError> {
        if args.query.is_empty()
            || args.query.contains('\n')
            || args.query.len() > 256
            || args.max_results == 0
            || args.max_results > 200
            || args.max_files == 0
            || args.max_files > 10000
            || args.max_bytes == 0
            || args.max_bytes > 16 * 1024 * 1024
        {
            return Err(ToolError::InvalidArguments);
        }
        let components = relative(&args.path, true)?;
        let directory = self.directory(&components)?;
        let mut search = Search {
            args: &args,
            control,
            matches: Vec::new(),
            files: 0,
            bytes: 0,
            entries: 0,
            skipped: 0,
            reasons: Vec::new(),
            output_bytes: OUTPUT_METADATA_BYTES,
            output_limit: context.max_output_bytes,
        };
        self.search_directory(directory, &args.path, 0, &mut search)?;
        Ok(
            json!({"matches":search.matches,"files_searched":search.files,"bytes_searched":search.bytes,"entries_visited":search.entries,"skipped_entries":search.skipped,"complete":search.reasons.is_empty(),"truncated":!search.reasons.is_empty(),"limits":search.reasons}),
        )
    }

    fn search_directory(
        &self,
        directory: OwnedFd,
        prefix: &str,
        depth: usize,
        search: &mut Search<'_>,
    ) -> Result<(), ToolError> {
        if depth >= 32 {
            search.limit("depth");
            return Ok(());
        }
        let entries = Dir::read_from(&directory).map_err(map_error)?;
        for entry in entries {
            search.control.check()?;
            if search.exhausted() {
                break;
            }
            if search.entries >= 10000 {
                search.limit("entries");
                break;
            }
            search.entries += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    search.skip();
                    continue;
                }
            };
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            let Ok(name_text) = std::str::from_utf8(name) else {
                search.skip();
                continue;
            };
            let path = if prefix.is_empty() {
                name_text.to_owned()
            } else {
                format!("{prefix}/{name_text}")
            };
            if path.len() > 4096 {
                search.skip();
                continue;
            }
            let name = OsStr::from_bytes(name);
            let stat = match fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(_) => {
                    search.skip();
                    continue;
                }
            };
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    let opened = fs::openat(&directory, name, DIRECTORY_FLAGS, Mode::empty());
                    match opened {
                        Ok(child)
                            if fs::fstat(&child).map_err(map_error)?.st_dev == self.device =>
                        {
                            self.search_directory(child, &path, depth + 1, search)?
                        }
                        _ => search.skip(),
                    }
                }
                FileType::RegularFile => {
                    if search.files >= search.args.max_files {
                        search.limit("files");
                        break;
                    }
                    if stat.st_size < 0 || stat.st_size as u64 > MAX_FILE_BYTES as u64 {
                        search.skip();
                        continue;
                    }
                    if search.bytes.saturating_add(stat.st_size as usize) > search.args.max_bytes {
                        search.limit("bytes");
                        break;
                    }
                    let opened = match self.open_file(
                        &directory,
                        name,
                        search.control,
                        (search.args.max_bytes - search.bytes).min(MAX_FILE_BYTES),
                    ) {
                        Ok(Some(file)) => file,
                        Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
                        Err(ToolError::TimedOut) => return Err(ToolError::TimedOut),
                        _ => {
                            search.skip();
                            continue;
                        }
                    };
                    if search.bytes.saturating_add(opened.bytes.len()) > search.args.max_bytes {
                        search.limit("bytes");
                        break;
                    }
                    search.files += 1;
                    search.bytes += opened.bytes.len();
                    for (line_index, line) in opened.bytes.split(|&byte| byte == b'\n').enumerate()
                    {
                        search.control.check()?;
                        if let Some(column) =
                            memchr::memmem::find(line, search.args.query.as_bytes())
                        {
                            let start = column.saturating_sub(80);
                            let end = line.len().min(
                                column
                                    .saturating_add(search.args.query.len())
                                    .saturating_add(80),
                            );
                            let found = json!({"path":path,"line":line_index+1,"byte_column":column,"digest":opened.identity.digest,"preview":encoded(&line[start..end]),"preview_offset":start,"preview_truncated":start>0 || end<line.len()});
                            let size = serde_json::to_vec(&found)
                                .map_err(|_| ToolError::OutputBudget)?
                                .len()
                                + 1;
                            if search.output_bytes.saturating_add(size) > search.output_limit {
                                search.limit("output");
                                break;
                            }
                            search.output_bytes += size;
                            search.matches.push(found);
                            if search.matches.len() >= search.args.max_results {
                                search.limit("results");
                                break;
                            }
                        }
                    }
                }
                _ => search.skip(),
            }
        }
        Ok(())
    }
}

struct Search<'a> {
    args: &'a SearchArgs,
    control: &'a Control<'a>,
    matches: Vec<Value>,
    files: usize,
    bytes: usize,
    entries: usize,
    skipped: usize,
    reasons: Vec<&'static str>,
    output_bytes: usize,
    output_limit: usize,
}
impl Search<'_> {
    fn limit(&mut self, reason: &'static str) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }
    fn skip(&mut self) {
        self.skipped += 1;
        self.limit("unsearched_entries");
    }
    fn exhausted(&self) -> bool {
        self.reasons.iter().any(|reason| {
            matches!(
                *reason,
                "entries" | "files" | "bytes" | "results" | "output"
            )
        })
    }
}

fn relative(path: &str, allow_root: bool) -> Result<Vec<OsString>, ToolError> {
    if allow_root && path.is_empty() {
        return Ok(Vec::new());
    }
    if path.is_empty() || path.len() > 4096 || path.contains('\0') || path.split('/').count() > 64 {
        return Err(ToolError::PathDenied);
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ToolError::PathDenied);
    }
    Ok(path.split('/').map(OsString::from).collect())
}
fn check_expected(expected: Expected, file: Option<&OpenFile>) -> Result<(), ToolError> {
    match (expected, file) {
        (Expected::Absent, None) => Ok(()),
        (Expected::Digest { digest }, Some(file)) if digest == file.identity.digest => Ok(()),
        _ => Err(ToolError::Conflict),
    }
}
fn same_inode(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn same_snapshot(a: &Stat, b: &Stat) -> bool {
    same_inode(a, b)
        && a.st_size == b.st_size
        && a.st_mode == b.st_mode
        && a.st_nlink == b.st_nlink
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn map_error(error: Errno) -> ToolError {
    match error {
        Errno::NOENT => ToolError::NotFound,
        Errno::LOOP | Errno::NOTDIR | Errno::XDEV => ToolError::PathDenied,
        Errno::EXIST => ToolError::Conflict,
        _ => ToolError::Io(error.into()),
    }
}

/// Private sibling staging is outside the executor's workspace mount. The model
/// cannot replace a temporary source name with a symlink before publication.
struct Staging<'a> {
    directory: OwnedFd,
    parent: &'a OwnedFd,
    name: String,
}
impl<'a> Staging<'a> {
    fn new(parent: &'a OwnedFd, device: Dev) -> Result<Self, ToolError> {
        if fs::fstat(parent).map_err(map_error)?.st_dev != device {
            return Err(ToolError::PathDenied);
        }
        let name = format!(".tact-write-{}", Uuid::new_v4());
        fs::mkdirat(parent, &name, Mode::from_raw_mode(0o700)).map_err(map_error)?;
        let directory = match fs::openat(parent, &name, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = fs::unlinkat(parent, &name, AtFlags::REMOVEDIR);
                return Err(map_error(error));
            }
        };
        Ok(Self {
            directory,
            parent,
            name,
        })
    }
}
impl Drop for Staging<'_> {
    fn drop(&mut self) {
        let _ = fs::unlinkat(&self.directory, "content", AtFlags::empty());
        let _ = fs::unlinkat(self.parent, &self.name, AtFlags::REMOVEDIR);
    }
}
