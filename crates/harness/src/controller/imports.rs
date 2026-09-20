use super::{Host, HostError};
use crate::{
    Digest,
    import::{ImportLimits, LegacyArchive, PublicationLimits, prepare_import},
    session::{SessionAdmissionRequest, SessionState},
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
        request: SessionAdmissionRequest,
    ) -> Result<SessionState, HostError> {
        let request = self.canonicalize_admission_request(request)?;
        let fingerprint = Digest::of_value(&(
            "orvek.import.request.v2",
            &database,
            &source_session,
            &request,
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
        if source_session.is_empty() || source_session.len() > 256 {
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
        let profile = super::resolve_admission(
            self.runtime(),
            self.config_identity,
            request,
            crate::admission_profile::BaselineReason::LegacyImport,
        )?;
        self.store
            .lock()
            .await
            .commit_bound_legacy_import(operation, fingerprint, profile, prepared)
            .map_err(HostError::from)
    }
}
