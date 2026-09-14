use super::*;

impl Host {
    pub(super) fn prepare_workspace(
        &self,
        session: &SessionState,
        artifacts: &crate::artifacts::ArtifactStore,
    ) -> Result<(crate::Digest, Snapshot), HostError> {
        let current = Snapshot::capture(session.workspace(), SnapshotPolicy::default(), artifacts)?;
        let origin = current.publish(artifacts)?;
        let snapshot = if let Some(seed) = &session.branch.workspace {
            Snapshot::reconcile(
                &Snapshot::load(seed.origin, artifacts)?,
                &Snapshot::load(seed.source, artifacts)?,
                &current,
            )?
        } else {
            current
        };
        snapshot.verify_artifacts(artifacts)?;
        Ok((origin, snapshot))
    }
}
