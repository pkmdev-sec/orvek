#[cfg(target_os = "linux")]
mod supervisor;
#[cfg(target_os = "linux")]
fn main() {
    if supervisor::run().is_err() {
        std::process::exit(70);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("orvek-executor is a Linux sandbox stub; install the matching Linux binary");
    std::process::exit(64);
}
