//! Terminal presentation and authenticated commands to the durable local host.

pub(crate) mod children;
mod client;
mod clipboard;
pub(crate) mod components;
mod context;
mod editor;
mod event_loop;
mod file_index;
#[cfg(test)]
// Fixture builders exist for the test and bench targets only; compiling them
// into an ordinary build would ship unused sample data.
#[cfg(test)]
pub(crate) mod fixtures;
mod format;
mod handoff_controller;
pub(crate) mod host_projection;
pub(crate) mod pane;
mod prompt;
mod scheduler;
pub(crate) mod session;
mod spinner;
mod terminal;
pub(crate) mod theme;
pub(crate) mod transcript;

use crate::app::{
    config::Config,
    error::{Result, RuntimeError},
};
use orvek_harness::inference::Model;
use std::io::{self, IsTerminal};
use tokio_util::sync::CancellationToken;

pub(crate) enum StartupMode {
    NewSession(Model),
    ResumeSession(String),
    ResumeSelector(Model),
}
pub(crate) async fn run(
    config: Config,
    startup: StartupMode,
    shutdown: CancellationToken,
) -> Result<Option<String>> {
    ensure_interactive()?;
    event_loop::run(config, startup, shutdown).await
}
pub(crate) fn ensure_interactive() -> Result<()> {
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(RuntimeError::InteractiveTerminal.into())
    }
}
