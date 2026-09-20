#![cfg(unix)]

use orvek_harness::{
    Store, StoreError,
    inference::ModelSettings,
    session::{SessionConfig, SessionId},
};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

fn assert_locked(result: Result<Store, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock)
    );
}

/// Owns exactly one harmless child, stopped after fork and before exec.
struct GatedChild {
    gate: UnixStream,
    worker: Option<JoinHandle<io::Result<ExitStatus>>>,
}

impl GatedChild {
    fn start() -> Self {
        let (gate, child_gate) = UnixStream::pair().unwrap();
        gate.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let worker = thread::spawn(move || {
            let mut command = Command::new("/usr/bin/true");
            // Only read/write syscalls run in the child. No Store or Arc is captured.
            unsafe {
                command.pre_exec(move || {
                    if rustix::io::write(&child_gate, b"R")? != 1 {
                        return Err(io::ErrorKind::WriteZero.into());
                    }
                    let mut release = [0];
                    if rustix::io::read(&child_gate, &mut release)? != 1 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    Ok(())
                });
            }
            command.status()
        });
        let mut child = Self {
            gate,
            worker: Some(worker),
        };
        let mut ready = [0];
        child.gate.read_exact(&mut ready).unwrap();
        assert_eq!(ready, *b"R");
        child
    }

    fn finish(mut self) -> ExitStatus {
        self.gate.write_all(b"G").unwrap();
        self.worker.take().unwrap().join().unwrap().unwrap()
    }
}

impl Drop for GatedChild {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.gate.write_all(b"G");
            let _ = worker.join();
        }
    }
}

#[test]
fn store_drop_releases_owner_while_unrelated_child_is_before_exec() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();
    let child = GatedChild::start();
    assert_locked(Store::open(root.path()));

    drop(store);
    let reopened = Store::open(root.path());
    if reopened.is_ok() {
        assert_locked(Store::open(root.path()));
    }
    assert!(child.finish().success());
    if reopened.is_err() {
        // Control: exec closes the child's inherited CLOEXEC descriptor.
        Store::open(root.path()).expect("reopen after the unrelated child execs");
    }
    let reopened = reopened.expect("Store::drop must release the lease before unrelated exec");
    assert_locked(Store::open(root.path()));
    drop(reopened);
    Store::open(root.path()).unwrap();
}

#[test]
fn retained_store_owner_still_excludes_another_writer() {
    let root = tempfile::tempdir().unwrap();
    let owner = Arc::new(Mutex::new(Store::open(root.path()).unwrap()));
    let retained = Arc::clone(&owner);
    drop(owner);
    assert_locked(Store::open(root.path()));
    drop(retained);
    Store::open(root.path()).unwrap();
}

#[test]
fn failed_store_initialization_releases_owner() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("v1.sqlite3");
    std::fs::create_dir(&database).unwrap();
    assert!(matches!(Store::open(root.path()), Err(StoreError::Sql(_))));
    std::fs::remove_dir(database).unwrap();
    let store = Store::open(root.path()).unwrap();
    assert_locked(Store::open(root.path()));
    drop(store);
    Store::open(root.path()).unwrap();
}

struct OwnerProcess(Child);

impl Drop for OwnerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn killed_owner_releases_lease_and_preserves_durable_state() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .create_session(
            SessionId::new(),
            SessionConfig {
                workspace: root.path().to_owned(),
                model: ModelSettings::default(),
                instructions: String::new(),
                context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
            },
            None,
        )
        .unwrap();
    drop(store);

    let mut child = OwnerProcess(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_owner",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ORVEK_STORE_OWNER_ROOT", root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "owner exited before ready"
        );
        if line.trim_end().ends_with("owner-ready") {
            break;
        }
    }
    // This open is in a different process from the live owner.
    assert_locked(Store::open(root.path()));
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());

    let mut recovered = Store::open(root.path()).unwrap();
    assert_eq!(recovered.load_session(session.id).unwrap(), session);
    assert!(recovered.recover_interrupted().unwrap().is_empty());
    assert_locked(Store::open(root.path()));
}

#[test]
#[ignore = "subprocess fixture; invoked only by killed_owner_releases_lease_and_preserves_durable_state"]
fn process_owner() {
    // Skip when invoked directly, so an --include-ignored sweep cannot fail here.
    let Some(root) = std::env::var_os("ORVEK_STORE_OWNER_ROOT") else {
        return;
    };
    let _store = Store::open(std::path::Path::new(&root)).unwrap();
    io::stdout().write_all(b"owner-ready\n").unwrap();
    io::stdout().flush().unwrap();
    let mut release = [0];
    io::stdin().read_exact(&mut release).unwrap();
}
