//! Same-user local webhook adapter. This is not an HTTP server.
use super::{
    config::Config,
    error::{Error, Result},
    host::HostClient,
};
use clap::Subcommand;
use orvek_harness::{
    event_intake::{MAX_PAYLOAD_BYTES, SourceConfig},
    ipc::Command,
};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub(crate) enum EventCommand {
    /// Register immutable source configuration bound to an existing session.
    Register { file: PathBuf },
    /// Forward a webhook payload over authenticated same-user IPC (not HTTP).
    Deliver {
        source: Uuid,
        key: String,
        #[arg(long)]
        payload: PathBuf,
    },
    /// Inspect the durable intake and normal queue receipt.
    Inspect { source: Uuid, key: String },
    /// Inspect source configuration and schedule cursor.
    Source { source: Uuid },
    /// Cancel one event through the normal host queue.
    Cancel { source: Uuid, key: String },
    /// Permanently disable a source and cancel outstanding events.
    Disable { source: Uuid },
}

fn read_bounded(path: &PathBuf, limit: usize) -> Result<String> {
    let mut reader: Box<dyn Read> = if path.as_os_str() == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).map_err(Error::Connection)?)
    };
    let mut text = String::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(Error::Connection)?;
    if text.len() > limit {
        return Err(Error::HostRequest(format!("input exceeds {limit} bytes")));
    }
    Ok(text)
}

impl EventCommand {
    pub(crate) async fn run(self, config: &Config) -> Result<()> {
        let command = match self {
            Self::Register { file } => {
                let config: SourceConfig = serde_json::from_str(&read_bounded(&file, 256 * 1024)?)
                    .map_err(|error| Error::HostRequest(error.to_string()))?;
                Command::RegisterEventSource { config }
            }
            Self::Deliver {
                source,
                key,
                payload,
            } => Command::DeliverEvent {
                source,
                key,
                payload: read_bounded(&payload, MAX_PAYLOAD_BYTES)?,
            },
            Self::Inspect { source, key } => Command::InspectEvent { source, key },
            Self::Source { source } => Command::EventSource { source },
            Self::Cancel { source, key } => Command::CancelEvent { source, key },
            Self::Disable { source } => Command::DisableEventSource { source },
        };
        let client = HostClient::connect(config).await?;
        let response = client.query(command).await?;
        println!(
            "{}",
            serde_json::to_string(&response)
                .map_err(|error| Error::HostRequest(error.to_string()))?
        );
        Ok(())
    }
}
