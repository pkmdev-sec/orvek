#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use orvek_harness::{
    Digest,
    capabilities::{ToolContext, ToolError, WorkspaceTools},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Fixture {
    directory: TempDir,
    context: ToolContext,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        Self {
            directory,
            context: ToolContext {
                workspace,
                task_id: Uuid::new_v4(),
                generation: 7,
                job_id: Uuid::new_v4(),
                readonly: false,
                can_write: true,
                max_output_bytes: 64 * 1024,
                timeout_ms: 5000,
            },
        }
    }
    fn path(&self, path: &str) -> PathBuf {
        self.context.workspace.join(path)
    }
    fn call(&self, name: &str, args: Value) -> Result<Value, ToolError> {
        WorkspaceTools::execute_file_tool(name, args, &self.context, &CancellationToken::new())
    }
    fn write(&self, name: &str, content: &[u8]) {
        fs::write(self.path(name), content).unwrap();
    }
}
fn expected(bytes: &[u8]) -> Value {
    json!({"kind":"digest","digest":Digest::of(bytes)})
}
fn replace(path: &str, expected: Value, content: &str) -> Value {
    json!({"operation":"replace","path":path,"expected":expected,"content":content})
}
fn read_bytes(output: &Value) -> Vec<u8> {
    let content = &output["result"]["content"];
    let data = content["data"].as_str().unwrap();
    if content["encoding"] == "utf8" {
        data.as_bytes().to_vec()
    } else {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap()
    }
}

#[test]
fn definitions_are_stable_closed_function_tools_without_model_authority_fields() {
    let definitions = WorkspaceTools::definitions();
    assert_eq!(
        definitions
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["read_file", "search", "write_file", "exec_command"]
    );
    for definition in &definitions {
        assert_eq!(definition["type"], "function");
        assert_eq!(definition["parameters"]["type"], "object");
        let encoded = serde_json::to_string(definition).unwrap();
        assert!(!encoded.contains("task_id"));
        assert!(!encoded.contains("can_write"));
        assert!(!encoded.contains("job_id"));
    }
}

#[test]
fn reads_exact_bytes_with_whole_file_identity_and_truncation() {
    let fixture = Fixture::new();
    fixture.write("code.rs", b"abcdefghijk");
    let output = fixture
        .call(
            "read_file",
            json!({"path":"code.rs","offset":2,"max_bytes":4}),
        )
        .unwrap();
    assert_eq!(read_bytes(&output), b"cdef");
    assert_eq!(
        output["result"]["identity"]["digest"],
        Digest::of(b"abcdefghijk").to_string()
    );
    assert_eq!(output["result"]["identity"]["bytes"], 11);
    assert_eq!(output["result"]["truncated"], true);
    assert_eq!(output["result"]["eof"], false);
    assert_eq!(output["task_id"], fixture.context.task_id.to_string());
    assert_eq!(output["generation"], 7);
    assert_eq!(output["job_id"], fixture.context.job_id.to_string());
    let eof = fixture
        .call("read_file", json!({"path":"code.rs","offset":11}))
        .unwrap();
    assert!(read_bytes(&eof).is_empty());
    assert_eq!(eof["result"]["eof"], true);
    assert!(matches!(
        fixture.call("read_file", json!({"path":"code.rs","offset":12})),
        Err(ToolError::InvalidArguments)
    ));
}

#[test]
fn binary_and_partial_unicode_reads_use_exact_base64_without_lossy_decoding() {
    let fixture = Fixture::new();
    fixture.write("binary", &[0xff, 0, 0x80]);
    let output = fixture.call("read_file", json!({"path":"binary"})).unwrap();
    assert_eq!(output["result"]["content"]["encoding"], "base64");
    assert_eq!(read_bytes(&output), [0xff, 0, 0x80]);
    fixture.write("unicode", "éclair".as_bytes());
    let partial = fixture
        .call("read_file", json!({"path":"unicode","max_bytes":1}))
        .unwrap();
    assert_eq!(read_bytes(&partial), vec![0xc3]);
    assert_eq!(partial["result"]["truncated"], true);
}

#[test]
fn atomic_create_replace_and_delete_return_exact_before_after_identities_and_modes() {
    let fixture = Fixture::new();
    let created = fixture
        .call(
            "write_file",
            replace("file", json!({"kind":"absent"}), "initial"),
        )
        .unwrap();
    assert_eq!(fs::read(fixture.path("file")).unwrap(), b"initial");
    assert!(created["result"]["before"].is_null());
    assert_eq!(
        created["result"]["after"]["digest"],
        Digest::of(b"initial").to_string()
    );
    fs::set_permissions(fixture.path("file"), fs::Permissions::from_mode(0o751)).unwrap();
    let output = fixture
        .call(
            "write_file",
            replace("file", expected(b"initial"), "replacement"),
        )
        .unwrap();
    assert_eq!(
        output["result"]["before"]["digest"],
        Digest::of(b"initial").to_string()
    );
    assert_eq!(
        output["result"]["after"]["digest"],
        Digest::of(b"replacement").to_string()
    );
    assert_eq!(output["result"]["after"]["mode"], 0o751);
    assert_eq!(
        fs::metadata(fixture.path("file"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
    assert_eq!(fs::read(fixture.path("file")).unwrap(), b"replacement");
    let deleted = fixture
        .call(
            "write_file",
            json!({"operation":"delete","path":"file","expected":expected(b"replacement")}),
        )
        .unwrap();
    assert_eq!(
        deleted["result"]["before"]["digest"],
        Digest::of(b"replacement").to_string()
    );
    assert!(deleted["result"]["after"].is_null());
    assert!(!fixture.path("file").exists());
    let absent = fixture
        .call(
            "write_file",
            json!({"operation":"delete","path":"file","expected":{"kind":"absent"}}),
        )
        .unwrap();
    assert_eq!(absent["result"]["changed"], false);
    assert!(fs::read_dir(fixture.directory.path()).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".orvek-write-")
    }));
}

#[test]
fn stale_digests_and_false_absence_preserve_existing_bytes() {
    let fixture = Fixture::new();
    fixture.write("file", b"user edit");
    for args in [
        replace("file", expected(b"old version"), "overwrite"),
        replace("file", json!({"kind":"absent"}), "overwrite"),
        json!({"operation":"delete","path":"file","expected":expected(b"old version")}),
    ] {
        assert!(matches!(
            fixture.call("write_file", args),
            Err(ToolError::Conflict)
        ));
        assert_eq!(fs::read(fixture.path("file")).unwrap(), b"user edit");
    }
}

#[test]
fn readonly_and_missing_write_grants_cannot_be_overridden_by_json() {
    for readonly in [false, true] {
        let mut fixture = Fixture::new();
        fixture.context.readonly = readonly;
        fixture.context.can_write = readonly;
        let mut args = replace("file", json!({"kind":"absent"}), "forged");
        args["can_write"] = true.into();
        assert!(matches!(
            fixture.call("write_file", args),
            Err(ToolError::Readonly)
        ));
        assert!(!fixture.path("file").exists());
    }
    let fixture = Fixture::new();
    fixture.write("file", b"safe");
    for field in ["task_id", "job_id", "generation", "readonly", "workspace"] {
        let mut args = json!({"path":"file"});
        args[field] = json!("forged");
        assert!(matches!(
            fixture.call("read_file", args),
            Err(ToolError::InvalidArguments)
        ));
    }
    assert!(matches!(
        fixture.call("shell", json!({})),
        Err(ToolError::UnknownTool)
    ));
    assert!(matches!(
        fixture.call("exec_command", json!({"command":"echo bypass"})),
        Err(ToolError::UnknownTool)
    ));
}

#[test]
fn traversal_absolute_paths_and_symlinks_never_reach_outside_files() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("secret");
    fs::write(&outside, b"host sentinel").unwrap();
    fs::create_dir(fixture.path("nested")).unwrap();
    symlink(&outside, fixture.path("leaf-link")).unwrap();
    symlink(fixture.directory.path(), fixture.path("directory-link")).unwrap();
    for path in [
        "../secret",
        "nested/../../secret",
        "/etc/passwd",
        "./leaf-link",
        "nested//file",
        "leaf-link",
        "directory-link/secret",
        "file/..",
        "bad\0path",
    ] {
        assert!(fixture.call("read_file", json!({"path":path})).is_err());
        assert!(
            fixture
                .call(
                    "write_file",
                    replace(path, expected(b"host sentinel"), "malicious")
                )
                .is_err()
        );
        assert_eq!(fs::read(&outside).unwrap(), b"host sentinel");
    }
    let workspace_link = fixture.directory.path().join("workspace-link");
    symlink(&fixture.context.workspace, &workspace_link).unwrap();
    let mut context = fixture.context.clone();
    context.workspace = workspace_link;
    assert!(
        WorkspaceTools::execute_file_tool(
            "read_file",
            json!({"path":"leaf-link"}),
            &context,
            &CancellationToken::new()
        )
        .is_err()
    );
}

#[test]
fn hardlinks_and_nonregular_files_are_rejected_without_reading_their_contents() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("host");
    fs::write(&outside, b"private host bytes").unwrap();
    fs::hard_link(&outside, fixture.path("hardlink")).unwrap();
    for name in ["read_file", "write_file"] {
        let args = if name == "read_file" {
            json!({"path":"hardlink"})
        } else {
            replace("hardlink", expected(b"private host bytes"), "changed")
        };
        assert!(matches!(
            fixture.call(name, args),
            Err(ToolError::UnsupportedFile)
        ));
    }
    fs::create_dir(fixture.path("directory")).unwrap();
    assert!(matches!(
        fixture.call("read_file", json!({"path":"directory"})),
        Err(ToolError::UnsupportedFile)
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"private host bytes");
}

#[test]
fn serialized_output_limits_are_enforced_and_read_truncation_is_explicit() {
    let mut fixture = Fixture::new();
    fixture.context.max_output_bytes = 2048;
    fixture.write("control", &vec![1u8; 10000]);
    let output = fixture
        .call("read_file", json!({"path":"control"}))
        .unwrap();
    assert!(serde_json::to_vec(&output).unwrap().len() <= 2048);
    assert_eq!(output["result"]["truncated"], true);
    assert!(read_bytes(&output).len() < 10000);
    fixture.write("too-large", &vec![b'a'; 1024 * 1024 + 1]);
    assert!(matches!(
        fixture.call("read_file", json!({"path":"too-large"})),
        Err(ToolError::FileTooLarge)
    ));
    assert!(matches!(
        fixture.call(
            "write_file",
            replace(
                "new",
                json!({"kind":"absent"}),
                &"x".repeat(1024 * 1024 + 1)
            )
        ),
        Err(ToolError::FileTooLarge)
    ));
    assert!(!fixture.path("new").exists());
}

#[test]
fn search_is_literal_bounded_and_reports_unsearched_entries() {
    let fixture = Fixture::new();
    fixture.write("source", b"literal a.*b\nnot aXXb\na.*b twice a.*b\n");
    fs::create_dir(fixture.path("nested")).unwrap();
    fixture.write("nested/other", b"literal a.*b\n");
    let full = fixture.call("search", json!({"query":"a.*b"})).unwrap();
    assert_eq!(full["result"]["matches"].as_array().unwrap().len(), 3);
    assert_eq!(full["result"]["complete"], true);
    assert_eq!(full["result"]["files_searched"], 2);
    let limited = fixture
        .call("search", json!({"query":"a.*b","max_results":1}))
        .unwrap();
    assert_eq!(limited["result"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(limited["result"]["complete"], false);
    assert!(
        limited["result"]["limits"]
            .as_array()
            .unwrap()
            .contains(&json!("results"))
    );
    let byte_limited = fixture
        .call("search", json!({"query":"a.*b","max_bytes":1}))
        .unwrap();
    assert_eq!(byte_limited["result"]["complete"], false);
    assert!(byte_limited["result"]["bytes_searched"].as_u64().unwrap() <= 1);
    symlink(
        fixture.directory.path().join("missing"),
        fixture.path("link"),
    )
    .unwrap();
    let skipped = fixture.call("search", json!({"query":"a.*b"})).unwrap();
    assert_eq!(skipped["result"]["complete"], false);
    assert_eq!(skipped["result"]["skipped_entries"], 1);
    assert!(matches!(
        fixture.call("search", json!({"query":"line\nbreak"})),
        Err(ToolError::InvalidArguments)
    ));
}

#[test]
fn search_output_bound_keeps_valid_json_and_match_preview_limits() {
    let mut fixture = Fixture::new();
    fixture.context.max_output_bytes = 2048;
    fixture.write(
        "source",
        format!("{}needle{}\n", "x".repeat(5000), "y".repeat(5000))
            .repeat(20)
            .as_bytes(),
    );
    let output = fixture.call("search", json!({"query":"needle"})).unwrap();
    assert!(serde_json::to_vec(&output).unwrap().len() <= 2048);
    assert_eq!(output["result"]["truncated"], true);
    assert!(!output["result"]["matches"].as_array().unwrap().is_empty());
    assert_eq!(output["result"]["matches"][0]["preview_truncated"], true);
    assert_eq!(output["result"]["matches"][0]["byte_column"], 5000);
}

#[test]
fn cancellation_before_mutation_leaves_no_file_or_staging_changes() {
    let fixture = Fixture::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        WorkspaceTools::execute_file_tool(
            "write_file",
            replace("new", json!({"kind":"absent"}), "no write"),
            &fixture.context,
            &cancel
        ),
        Err(ToolError::Cancelled)
    ));
    assert!(!fixture.path("new").exists());
    assert_eq!(fs::read_dir(fixture.directory.path()).unwrap().count(), 1);
}

#[test]
fn concurrent_expected_absence_has_exactly_one_winner() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|i| {
            let context = fixture.context.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                WorkspaceTools::execute_file_tool(
                    "write_file",
                    replace("new", json!({"kind":"absent"}), &format!("writer-{i}")),
                    &context,
                    &CancellationToken::new(),
                )
            })
        })
        .collect();
    let results: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| matches!(error, ToolError::Conflict))
    );
    let winner = results.into_iter().find_map(Result::ok).unwrap();
    assert_eq!(
        winner["result"]["after"]["digest"],
        Digest::of(&fs::read(fixture.path("new")).unwrap()).to_string()
    );
}

fn create_regular_if_absent(path: &Path, content: &[u8]) {
    if let Ok(mut file) = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
    {
        file.write_all(content).unwrap();
    }
}

#[test]
fn repeated_leaf_symlink_swaps_cannot_read_or_overwrite_the_outside_target() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("outside");
    fs::write(&outside, b"outside sentinel").unwrap();
    fixture.write("leaf", b"inside");
    let running = Arc::new(AtomicBool::new(true));
    let race = {
        let running = running.clone();
        let leaf = fixture.path("leaf");
        let outside = outside.clone();
        thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                let _ = fs::remove_file(&leaf);
                let _ = symlink(&outside, &leaf);
                thread::yield_now();
                let _ = fs::remove_file(&leaf);
                create_regular_if_absent(&leaf, b"inside");
            }
        })
    };
    for _ in 0..300 {
        if let Ok(output) = fixture.call("read_file", json!({"path":"leaf"})) {
            assert_ne!(read_bytes(&output), b"outside sentinel");
        }
        let _ = fixture.call(
            "write_file",
            replace("leaf", expected(b"inside"), "replacement"),
        );
    }
    running.store(false, Ordering::Release);
    race.join().unwrap();
    assert_eq!(fs::read(outside).unwrap(), b"outside sentinel");
}

#[test]
fn repeated_parent_symlink_swaps_keep_descriptor_operations_inside_the_workspace() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file"), b"outside sentinel").unwrap();
    fs::create_dir(fixture.path("flip")).unwrap();
    fixture.write("flip/file", b"inside");
    let running = Arc::new(AtomicBool::new(true));
    let race = {
        let running = running.clone();
        let flip = fixture.path("flip");
        let parked = fixture.path("parked");
        let outside = outside.clone();
        thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                if fs::rename(&flip, &parked).is_ok() {
                    let _ = symlink(&outside, &flip);
                    thread::yield_now();
                    let _ = fs::remove_file(&flip);
                    let _ = fs::rename(&parked, &flip);
                }
            }
        })
    };
    for _ in 0..300 {
        if let Ok(output) = fixture.call("read_file", json!({"path":"flip/file"})) {
            assert_ne!(read_bytes(&output), b"outside sentinel");
        }
        let _ = fixture.call(
            "write_file",
            replace("flip/file", expected(b"inside"), "replacement"),
        );
    }
    running.store(false, Ordering::Release);
    race.join().unwrap();
    assert_eq!(fs::read(outside.join("file")).unwrap(), b"outside sentinel");
}

#[tokio::test]
#[ignore = "requires an explicitly provided locally installed ORVEK_WORKSPACE_TEST_IMAGE and Docker"]
async fn exec_command_runs_only_through_the_isolated_docker_executor() {
    let mut fixture = Fixture::new();
    let shared = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orvek/workspace-tool-tests");
    fs::create_dir_all(&shared).unwrap();
    let directory = tempfile::tempdir_in(shared).unwrap();
    fixture.context.workspace = directory.path().join("workspace");
    fs::create_dir(&fixture.context.workspace).unwrap();
    fixture.directory = directory;
    let image = std::env::var("ORVEK_WORKSPACE_TEST_IMAGE").expect("explicit local test image");
    let executor = orvek_harness::runtime::DockerExecutor::connect(&image)
        .await
        .unwrap();
    let tools = WorkspaceTools::new(Arc::new(executor));
    let output = tools
        .execute(
            "exec_command",
            json!({"command":"printf docker-only > from-command; printf output"}),
            fixture.context.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(output["result"]["status"]["kind"], "exited");
    assert_eq!(output["result"]["status"]["code"], 0);
    assert_eq!(
        fs::read(fixture.path("from-command")).unwrap(),
        b"docker-only"
    );
    assert_eq!(output["job_id"], fixture.context.job_id.to_string());
}
