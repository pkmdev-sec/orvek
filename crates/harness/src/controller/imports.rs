use super::{Host, HostError};
use crate::{
    Digest,
    import::{ImportLimits, LegacyArchive, PublicationLimits, prepare_import},
    session::{SessionConfig, SessionState},
};
use std::path::PathBuf;
use uuid::Uuid;

impl Host {
    /// The operation binds its first committed source selection. A different
    /// operation inspects the current source, then reuses content-identical progress.
    pub async fn import_legacy_request(
        &self,
        operation: Uuid,
        database: PathBuf,
        source_session: String,
        mut config: SessionConfig,
    ) -> Result<SessionState, HostError> {
        let fingerprint = Digest::of_value(&(
            "tact.import.request.v1",
            &database,
            &source_session,
            &config,
        ))?;
        if let Some(existing) = self
            .store
            .lock()
            .await
            .lookup_legacy_import(operation, fingerprint)?
        {
            return Ok(existing);
        }
        let database = database.canonicalize()?;
        config.workspace = config.workspace.canonicalize()?;
        if self.root.starts_with(&config.workspace) || config.workspace.starts_with(&self.root) {
            return Err(HostError::Invalid(
                "protected state and source workspace must not overlap",
            ));
        }
        if source_session.is_empty()
            || source_session.len() > 256
            || config.instructions.len() > 256 * 1024
        {
            return Err(HostError::Invalid("legacy import input exceeds its bounds"));
        }
        let artifacts = self.store.lock().await.artifacts().clone();
        let archive = LegacyArchive::open(&database, ImportLimits::default())?;
        let prepared = prepare_import(
            &archive,
            &artifacts,
            &source_session,
            PublicationLimits::default(),
        )?;
        self.store
            .lock()
            .await
            .commit_legacy_import(operation, fingerprint, config, prepared)
            .map_err(HostError::from)
    }
}
