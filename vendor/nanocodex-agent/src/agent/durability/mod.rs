#[cfg(not(target_family = "wasm"))]
#[path = "native.rs"]
mod platform;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[path = "disabled.rs"]
mod platform;

pub(super) use platform::{Durability, DurabilityConfig};
