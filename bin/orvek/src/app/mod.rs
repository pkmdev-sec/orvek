//! Application boundaries for configuration, authentication, and command dispatch.

pub(crate) mod artifacts;
mod auth;
pub(crate) mod auxiliary;
pub(crate) mod browser;
mod cli;
pub(crate) mod config;
pub(crate) mod error;
mod event_intake;
mod headless;
pub(crate) mod herdr;
pub(crate) mod host;
pub(crate) mod installation;
mod monitor;
pub(crate) mod secret;
mod shutdown;
pub(crate) mod submission;
mod trace;
pub(crate) mod update;

pub(crate) use cli::Cli;
