use std::process::Command;

fn git(arguments: &[&str]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // The Git label belongs to the executing harness, not the later exporter.
    // Source archives without Git metadata retain an explicit unknown label.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let revision = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&[
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        "src",
        "Cargo.toml",
        "build.rs",
    ])
    .map(|status| if status.is_empty() { "false" } else { "true" })
    .unwrap_or("unknown");
    println!("cargo:rustc-env=ORVEK_TRACE_SOURCE_REVISION={revision}");
    println!("cargo:rustc-env=ORVEK_TRACE_SOURCE_DIRTY={dirty}");
}
