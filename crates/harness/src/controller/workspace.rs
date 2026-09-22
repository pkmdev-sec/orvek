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

#[cfg(test)]
pub(super) struct DerivationGate {
    pub(super) entered: tokio::sync::mpsc::UnboundedSender<Snapshot>,
    pub(super) release: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
}

#[cfg(test)]
impl DerivationGate {
    pub(super) async fn hold(&self, snapshot: &Snapshot) {
        self.entered.send(snapshot.clone()).unwrap();
        self.release.lock().await.recv().await.unwrap();
    }
}

pub(super) struct StagedTree {
    path: PathBuf,
}

impl StagedTree {
    pub(super) async fn materialize(
        parent: &Path,
        prefix: &str,
        snapshot: &Snapshot,
        artifacts: &crate::artifacts::ArtifactStore,
        #[cfg(test)] gate: Option<&DerivationGate>,
    ) -> Result<Self, HostError> {
        fs::create_dir_all(parent)?;
        #[cfg(test)]
        if let Some(gate) = gate {
            gate.hold(snapshot).await;
        }
        let path = parent.join(format!(".{prefix}-{}", Uuid::new_v4()));
        snapshot.materialize(&path, artifacts, false)?;
        Ok(Self { path })
    }

    pub(super) fn publish_noclobber(mut self, destination: &Path) -> Result<bool, HostError> {
        match fs::rename(&self.path, destination) {
            Ok(()) => {
                self.path.clear();
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn exchange(mut self, destination: &Path) -> Result<Self, HostError> {
        crate::runtime::exchange(destination, &self.path)?;
        Ok(Self {
            path: std::mem::take(&mut self.path),
        })
    }
}

impl Drop for StagedTree {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(super) fn predicates_match(expected: &TaskState, current: &TaskState) -> bool {
    expected.revision == current.revision
        && expected.workspace_override == current.workspace_override
        && expected.origin == current.origin
        && expected.baseline == current.baseline
        && expected.candidate == current.candidate
        && expected.jobs == current.jobs
        && expected.model_reservations == current.model_reservations
        && expected.outcome == current.outcome
}
