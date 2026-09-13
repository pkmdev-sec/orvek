//! Exercise the installed command boundary without credentials or model requests.

use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orvek"));
    command.env_clear().env("HOME", home);
    command
}

fn shown_config(home: &Path, variables: &[(&str, &str)], args: &[&str]) -> toml::Value {
    let output = command(home)
        .envs(variables.iter().copied())
        .args(args)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap()
}

#[test]
fn command_identifies_as_orvek_and_exposes_context_controls() {
    let home = TempDir::new().unwrap();
    let version = command(home.path()).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(version.stdout.starts_with(b"orvek 0.1.0\n"));
    let help = command(home.path()).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Usage: orvek"));
    assert!(help.contains("ORVEK_COMPACTION"));
    assert!(!help.contains("Usage: tact"));
}

#[test]
fn cli_flags_override_new_environment_which_overrides_legacy_environment() {
    let home = TempDir::new().unwrap();
    let legacy = shown_config(home.path(), &[("TACT_THINKING", "low")], &[]);
    assert_eq!(legacy["agent"]["thinking"].as_str(), Some("low"));
    let variables = [("TACT_THINKING", "low"), ("ORVEK_THINKING", "high")];
    let current = shown_config(home.path(), &variables, &[]);
    assert_eq!(current["agent"]["thinking"].as_str(), Some("high"));
    let explicit = shown_config(home.path(), &variables, &["--thinking", "medium"]);
    assert_eq!(explicit["agent"]["thinking"].as_str(), Some("medium"));
}

#[test]
fn legacy_database_directory_is_selected_even_without_a_config_file() {
    let home = TempDir::new().unwrap();
    let legacy = home.path().join(".tact");
    fs::create_dir_all(legacy.join("sessions")).unwrap();
    let sentinel = legacy.join("sessions/untouched-fixture");
    fs::write(&sentinel, b"existing session bytes").unwrap();
    let path = command(home.path())
        .args(["config", "path"])
        .output()
        .unwrap();
    assert!(path.status.success());
    assert_eq!(
        String::from_utf8(path.stdout).unwrap().trim(),
        legacy.join("config.toml").to_str().unwrap()
    );
    let current = home.path().join(".orvek");
    fs::create_dir(&current).unwrap();
    let path = command(home.path())
        .args(["config", "path"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(path.stdout).unwrap().trim(),
        current.join("config.toml").to_str().unwrap()
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"existing session bytes");
}

#[test]
fn new_home_override_wins_without_moving_shared_codex_credentials() {
    let home = TempDir::new().unwrap();
    let legacy = home.path().join("legacy");
    let current = home.path().join("current");
    let config = shown_config(
        home.path(),
        &[
            ("TACT_HOME", legacy.to_str().unwrap()),
            ("ORVEK_HOME", current.to_str().unwrap()),
        ],
        &[],
    );
    assert_eq!(
        config["auth"]["file"].as_str(),
        home.path().join(".codex/auth.json").to_str()
    );
    let path = command(home.path())
        .env("TACT_HOME", &legacy)
        .env("ORVEK_HOME", &current)
        .args(["config", "path"])
        .output()
        .unwrap();
    assert!(path.status.success());
    assert_eq!(
        String::from_utf8(path.stdout).unwrap().trim(),
        current.join("config.toml").to_str().unwrap()
    );
}
