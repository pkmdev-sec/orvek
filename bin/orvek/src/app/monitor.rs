//! Same-user operator access; deliberately absent from model tools and event data.
use super::{
    config::Config,
    error::{Error, Result},
    host::HostClient,
};
use clap::Subcommand;
use orvek_harness::{Digest, ipc::Command};

#[derive(Debug, Subcommand)]
pub(crate) enum MonitorCommand {
    /// Inspect cohort counts, sampling and deterministic recovery episodes.
    Report {
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Sample one of every N eligible underfilled read pages. All counts remain recorded.
    Sampling { every: u32 },
    /// Release a native read reply envelope for NEW sessions. No policy or task-limit change.
    InstallRead {
        expected: Digest,
        bytes: u32,
        note: String,
    },
    /// Restore the previous version for NEW sessions using compare-and-swap.
    Rollback { expected: Digest },
}
impl MonitorCommand {
    pub(crate) async fn run(self, config: &Config) -> Result<()> {
        let command = match self {
            Self::Report { offset, limit } => Command::MonitorReport { offset, limit },
            Self::Sampling { every } => Command::MonitorSampling { every },
            Self::InstallRead {
                expected,
                bytes,
                note,
            } => Command::InstallReadBehavior {
                expected,
                bytes,
                note,
            },
            Self::Rollback { expected } => Command::RollbackReadBehavior { expected },
        };
        let response = HostClient::connect(config).await?.query(command).await?;
        println!(
            "{}",
            serde_json::to_string(&response).map_err(|e| Error::HostRequest(e.to_string()))?
        );
        Ok(())
    }
}
