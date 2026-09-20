use orvek_executor::{self as wire, Complete, Entry, Hello, Outcome, Quota, Request};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Component, Path},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
const FALLBACK_UID: u32 = 65534;

pub fn run() -> io::Result<()> {
    if unsafe { libc::getpid() } != 1 || unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::other("stub must be namespace init"));
    }
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let request = wire::read_request(&mut io::stdin().lock())?;
    validate_request(&request)?;
    let started = Instant::now();
    let source_owner = fs::metadata(if request.readonly {
        "/workspace"
    } else {
        "/source"
    })?
    .uid();
    let child_uid = if source_owner == 0 {
        FALLBACK_UID
    } else {
        source_owner
    };
    let workspace_quota = if request.readonly {
        verify_readonly(Path::new("/workspace"))?;
        None
    } else {
        Some(verify_quota(
            Path::new("/workspace"),
            request.workspace_bytes,
            request.workspace_inodes,
        )?)
    };
    verify_quota(
        Path::new("/cache"),
        request.cache_bytes,
        request.cache_inodes,
    )?;
    verify_quota(
        Path::new("/tmp"),
        request.temporary_bytes,
        request.temporary_inodes,
    )?;
    verify_quota(
        Path::new("/dev/shm"),
        request.temporary_bytes,
        request.temporary_inodes,
    )?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_all(wire::MAGIC)?;
    wire::write_json(
        &mut out,
        wire::HELLO,
        &Hello {
            version: wire::VERSION,
            job_id: request.job_id.clone(),
            nonce: request.nonce.clone(),
            root_uid: 0,
            child_uid,
            readonly: request.readonly,
            quota: workspace_quota,
        },
    )?;
    if !request.readonly && copy_source(&request, started, child_uid).is_err() {
        return finish(&mut out, &request, Outcome::InvalidWorkspace, false, 0, 0);
    }
    fs::create_dir_all("/cache/home")?;
    chown(Path::new("/cache/home"), child_uid)?;
    let workspace_mode = fs::metadata("/workspace")?.mode() & 0o7777;
    let before_oom = oom_kills();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&request.command)
        .current_dir("/workspace")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/cache/home")
        .env("TMPDIR", "/tmp")
        .env("XDG_CACHE_HOME", "/cache")
        .env("CARGO_HOME", "/cache/cargo")
        .env("CARGO_TARGET_DIR", "/cache/target")
        .env("npm_config_cache", "/cache/npm")
        .env("LC_ALL", "C");
    let file_limit = request.workspace_bytes;
    // Only Linux syscalls run after fork. The child loses all privileged IDs,
    // capabilities and inherited environment before any workspace code executes.
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(child_uid) != 0
                || libc::setuid(child_uid) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let files = libc::rlimit {
                rlim_cur: 256,
                rlim_max: 256,
            };
            let core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let size = libc::rlimit {
                rlim_cur: file_limit,
                rlim_max: file_limit,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &files) != 0
                || libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
                || libc::setrlimit(libc::RLIMIT_FSIZE, &size) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return finish(&mut out, &request, Outcome::SupervisorError, false, 0, 0),
    };
    let mut child_out = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("stdout missing"))?;
    let mut child_err = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("stderr missing"))?;
    nonblocking(child_out.as_raw_fd())?;
    nonblocking(child_err.as_raw_fd())?;
    let mut remaining = request.output_bytes;
    let mut output_open = true;
    let mut error_open = true;
    let mut outcome = loop {
        let limit = drain(
            &mut child_out,
            &mut output_open,
            &mut out,
            wire::STDOUT,
            &mut remaining,
            false,
        )? | drain(
            &mut child_err,
            &mut error_open,
            &mut out,
            wire::STDERR,
            &mut remaining,
            false,
        )?;
        if STOP.load(Ordering::Relaxed) {
            break Outcome::Cancelled;
        }
        if started.elapsed() >= Duration::from_millis(request.timeout_ms) {
            break Outcome::TimedOut;
        }
        if limit {
            break Outcome::OutputLimit;
        }
        if let Some(status) = child.try_wait()? {
            break if let Some(code) = status.code() {
                Outcome::Exited { code }
            } else if status.signal() == Some(libc::SIGXFSZ) {
                Outcome::QuotaLimit
            } else {
                Outcome::SupervisorError
            };
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    if !quiesce(Duration::from_secs(2))? {
        return Err(io::Error::other("descendants remain"));
    }
    let limit = drain(
        &mut child_out,
        &mut output_open,
        &mut out,
        wire::STDOUT,
        &mut remaining,
        true,
    )? | drain(
        &mut child_err,
        &mut error_open,
        &mut out,
        wire::STDERR,
        &mut remaining,
        true,
    )?;
    if !output_open && !error_open {
    } else {
        return Err(io::Error::other("output remained open after quiescence"));
    }
    if limit {
        outcome = Outcome::OutputLimit;
    }
    if oom_kills() > before_oom {
        outcome = Outcome::MemoryLimit;
    }
    for path in ["/workspace", "/cache", "/tmp", "/dev/shm"] {
        if request.readonly && path == "/workspace" {
            continue;
        }
        let current = quota(Path::new(path))?;
        if current.free_bytes == 0 || current.free_inodes == 0 {
            outcome = Outcome::QuotaLimit;
        }
    }
    if request.readonly || !matches!(outcome, Outcome::Exited { .. }) {
        return finish(&mut out, &request, outcome, false, 0, 0);
    }
    if fs::metadata("/workspace")?.mode() & 0o7777 != workspace_mode {
        return finish(&mut out, &request, Outcome::InvalidWorkspace, false, 0, 0);
    }
    let entries = match tree(
        Path::new("/workspace"),
        request.workspace_bytes,
        request.workspace_inodes,
    ) {
        Ok(entries) => entries,
        Err(_) => return finish(&mut out, &request, Outcome::InvalidWorkspace, false, 0, 0),
    };
    let mut total = 0u64;
    for entry in &entries {
        wire::write_json(&mut out, wire::ENTRY, entry)?;
        if let Entry::File { path, bytes, .. } = entry {
            let mut file = open_regular(&Path::new("/workspace").join(path))?;
            let mut left = *bytes;
            let mut buffer = [0u8; wire::CHUNK_BYTES];
            while left > 0 {
                let count =
                    file.read(&mut buffer[..left.min(wire::CHUNK_BYTES as u64) as usize])?;
                if count == 0 {
                    return Err(io::Error::other("short workspace file"));
                }
                wire::write_frame(&mut out, wire::FILE_DATA, &buffer[..count])?;
                left -= count as u64;
            }
            if file.read(&mut buffer[..1])? != 0 {
                return Err(io::Error::other("workspace file grew"));
            }
            total = total
                .checked_add(*bytes)
                .ok_or_else(|| io::Error::other("size overflow"))?;
        }
    }
    finish(
        &mut out,
        &request,
        outcome,
        true,
        entries.len() as u64,
        total,
    )
}
fn finish(
    out: &mut impl Write,
    request: &Request,
    outcome: Outcome,
    exported: bool,
    entries: u64,
    bytes: u64,
) -> io::Result<()> {
    wire::write_json(
        out,
        wire::COMPLETE,
        &Complete {
            version: wire::VERSION,
            nonce: request.nonce.clone(),
            outcome,
            exported,
            entries,
            bytes,
            quiescent: true,
        },
    )
}
fn validate_request(r: &Request) -> io::Result<()> {
    if r.version != wire::VERSION
        || r.job_id.len() != 36
        || r.nonce.len() != 36
        || r.command.is_empty()
        || r.command.len() > wire::MAX_COMMAND_BYTES
        || r.timeout_ms == 0
        || r.timeout_ms > 3600000
        || r.output_bytes == 0
        || r.output_bytes > 16 * 1024 * 1024
        || r.workspace_bytes == 0
        || r.workspace_bytes > 1024 * 1024 * 1024
        || r.workspace_inodes < 16
        || r.workspace_inodes > 100000
    {
        return Err(io::Error::other("invalid request"));
    }
    Ok(())
}
fn nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn drain(
    reader: &mut impl Read,
    open: &mut bool,
    out: &mut impl Write,
    kind: u8,
    remaining: &mut u64,
    after_stop: bool,
) -> io::Result<bool> {
    let mut overflow = false;
    if !*open {
        return Ok(false);
    }
    let mut buffer = [0u8; wire::CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *open = false;
                return Ok(overflow);
            }
            Ok(count) => {
                let accepted = (count as u64).min(*remaining) as usize;
                if accepted > 0 {
                    wire::write_frame(out, kind, &buffer[..accepted])?;
                }
                *remaining -= accepted as u64;
                if accepted < count {
                    overflow = true;
                    if !after_stop {
                        return Ok(true);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }
}
fn quiesce(limit: Duration) -> io::Result<bool> {
    let started = Instant::now();
    loop {
        unsafe {
            libc::kill(-1, libc::SIGKILL);
        }
        let mut status = 0;
        let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ECHILD) {
                return Ok(true);
            }
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
        }
        if started.elapsed() > limit {
            return Ok(false);
        }
        if result == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
fn oom_kills() -> u64 {
    fs::read_to_string("/sys/fs/cgroup/memory.events")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("oom_kill ").and_then(|n| n.parse().ok()))
        })
        .unwrap_or(0)
}
fn quota(path: &Path) -> io::Result<Quota> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(Quota {
        total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
        free_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
        total_inodes: stat.f_files,
        free_inodes: stat.f_favail,
    })
}
// libc uses different signedness for f_type across Linux libc targets.
#[allow(clippy::unnecessary_cast)]
fn verify_quota(path: &Path, bytes: u64, inodes: u64) -> io::Result<Quota> {
    let name = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    let q = quota(path)?;
    if stat.f_type as u64 != libc::TMPFS_MAGIC as u64
        || q.total_bytes > bytes
        || q.total_bytes == 0
        || q.total_inodes > inodes
        || q.total_inodes == 0
    {
        return Err(io::Error::other("quota mount mismatch"));
    }
    Ok(q)
}
fn verify_readonly(path: &Path) -> io::Result<()> {
    let name = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stat.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
        if !mountinfo_readonly(&mountinfo, path) {
            return Err(io::Error::other("source mount is writable"));
        }
    }
    Ok(())
}

fn mountinfo_readonly(contents: &str, path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let mut found = false;
    for line in contents.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some((mount_point, options)) = fields.nth(4).zip(fields.next()) else {
            continue;
        };
        if mount_point != path {
            continue;
        }
        found = true;
        if !options.split(',').any(|option| option == "ro") {
            return false;
        }
    }
    found
}
fn safe(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.contains('\0')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
fn open_regular(path: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(io::Error::other("nonregular file"));
    }
    Ok(file)
}
fn tree(root: &Path, max_bytes: u64, max_inodes: u64) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut bytes = 0;
    walk(root, "", 0, &mut entries, &mut bytes, max_bytes, max_inodes)?;
    Ok(entries)
}
fn walk(
    root: &Path,
    prefix: &str,
    depth: usize,
    entries: &mut Vec<Entry>,
    bytes: &mut u64,
    max_bytes: u64,
    max_inodes: u64,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other("path depth"));
    }
    let mut names = fs::read_dir(root.join(prefix))?
        .map(|r| r.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    for name in names {
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("nonutf8 path"))?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if !safe(&path) || entries.len() as u64 >= max_inodes {
            return Err(io::Error::other("entry limit"));
        }
        let full = root.join(&path);
        let meta = fs::symlink_metadata(&full)?;
        let mode = meta.mode() & 0o7777;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&full)?
                .into_os_string()
                .into_string()
                .map_err(|_| io::Error::other("nonutf8 link"))?;
            if target.len() > 4096 {
                return Err(io::Error::other("link limit"));
            }
            entries.push(Entry::Symlink { path, target });
        } else if meta.is_dir() {
            if mode & 0o7000 != 0 || mode & 0o500 != 0o500 {
                return Err(io::Error::other("directory mode"));
            }
            entries.push(Entry::Directory {
                path: path.clone(),
                mode,
            });
            walk(
                root,
                &path,
                depth + 1,
                entries,
                bytes,
                max_bytes,
                max_inodes,
            )?;
        } else if meta.is_file() {
            if mode & 0o7000 != 0 || mode & 0o400 == 0 || meta.nlink() != 1 {
                return Err(io::Error::other("file mode or links"));
            }
            *bytes = bytes
                .checked_add(meta.len())
                .ok_or_else(|| io::Error::other("byte overflow"))?;
            if *bytes > max_bytes {
                return Err(io::Error::other("byte limit"));
            }
            entries.push(Entry::File {
                path,
                mode,
                bytes: meta.len(),
            });
        } else {
            return Err(io::Error::other("unsupported file type"));
        }
    }
    Ok(())
}
fn chown(path: &Path, uid: u32) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    if unsafe { libc::lchown(path.as_ptr(), uid, uid) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn copy_source(request: &Request, started: Instant, uid: u32) -> io::Result<()> {
    let entries = tree(
        Path::new("/source"),
        request.workspace_bytes,
        request.workspace_inodes,
    )?;
    for entry in &entries {
        if STOP.load(Ordering::Relaxed)
            || started.elapsed() >= Duration::from_millis(request.timeout_ms)
        {
            return Err(io::Error::other("copy stopped"));
        }
        match entry {
            Entry::Directory { path, .. } => {
                let target = Path::new("/workspace").join(path);
                fs::create_dir(&target)?;
            }
            Entry::File { path, mode, bytes } => {
                let source = Path::new("/source").join(path);
                let target = Path::new("/workspace").join(path);
                let mut source = open_regular(&source)?;
                let mut target_file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)?;
                let copied = io::copy(
                    &mut Read::by_ref(&mut source).take(bytes.saturating_add(1)),
                    &mut target_file,
                )?;
                if copied != *bytes {
                    return Err(io::Error::other("source changed"));
                }
                fs::set_permissions(&target, fs::Permissions::from_mode(*mode))?;
                chown(&target, uid)?;
            }
            Entry::Symlink { .. } => {}
        }
    }
    for entry in &entries {
        if let Entry::Symlink { path, target } = entry {
            let path = Path::new("/workspace").join(path);
            std::os::unix::fs::symlink(target, &path)?;
            chown(&path, uid)?;
        }
    }
    for entry in entries.iter().rev() {
        if let Entry::Directory { path, mode } = entry {
            fs::set_permissions(
                Path::new("/workspace").join(path),
                fs::Permissions::from_mode(*mode),
            )?;
            chown(&Path::new("/workspace").join(path), uid)?;
        }
    }
    fs::set_permissions(
        "/workspace",
        fs::Permissions::from_mode(fs::metadata("/source")?.mode() & 0o777),
    )?;
    chown(Path::new("/workspace"), uid)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::mountinfo_readonly;
    use std::path::Path;

    #[test]
    fn recognizes_readonly_bind_mount_options() {
        let readonly = "42 31 0:52 /source /workspace ro,relatime - ext4 /dev/root rw
";
        let writable = "42 31 0:52 /source /workspace rw,relatime - ext4 /dev/root rw
";

        assert!(mountinfo_readonly(readonly, Path::new("/workspace")));
        assert!(!mountinfo_readonly(writable, Path::new("/workspace")));
    }
}
