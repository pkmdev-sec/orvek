use super::storage::StorageError;
use std::path::PathBuf;
use thiserror::Error;

/// Errors retain the transcript path because failures otherwise occur after the
/// terminal has already yielded its screen to the application.
#[derive(Debug, Error)]
pub(crate) enum TranscriptError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("failed to encode transcript record for {path}: {source}")]
    Encode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("the transcript writer for {0} stopped unexpectedly")]
    WriterStopped(PathBuf),
    #[error("the transcript writer for {path} stopped after persistence failed: {reason}")]
    WriterFailed { path: PathBuf, reason: String },
    #[error("the transcript writer task stopped unexpectedly: {0}")]
    WriterTask(#[source] tokio::task::JoinError),
}
