use super::*;

impl Host {
    pub async fn record_review(
        &self,
        session: SessionId,
        operation: Uuid,
        manifest: crate::Digest,
        source_identity: crate::Digest,
        disposition: crate::feedback::Disposition,
        body: String,
    ) -> Result<crate::Digest, HostError> {
        if body.len() > 64 * 1024 {
            return Err(HostError::Invalid("review feedback exceeds 64 KiB"));
        }
        let mut store = self.store.lock().await;
        let state = store.load_session(session)?;
        let source = crate::review::manifest(store.artifacts(), manifest)?;
        if source.source_identity != source_identity {
            return Err(HostError::Invalid("review source identity changed"));
        }
        let feedback = crate::feedback::ReviewFeedback {
            version: 1,
            session,
            manifest,
            source_identity,
            disposition,
            body,
        };
        let digest = store
            .artifacts()
            .put(&serde_json::to_vec(&feedback)?)
            .map_err(StoreError::from)?;
        store.session_command(
            session,
            state.revision,
            operation,
            SessionCommand::ReviewRecorded { feedback: digest },
        )?;
        Ok(digest)
    }

    pub(super) async fn read_review_feedback(
        &self,
        session: SessionId,
        args: Value,
    ) -> Result<Value, HostError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Query {
            digest: crate::Digest,
            #[serde(default)]
            offset: usize,
            limit: usize,
        }
        let query = match serde_json::from_value::<Query>(args) {
            Ok(query) => query,
            Err(error) => return Ok(json!({"error":error.to_string()})),
        };
        if query.limit == 0 || query.limit > 16_384 {
            return Ok(json!({"error":"feedback pages must contain 1..16384 characters"}));
        }
        let store = self.store.lock().await;
        let mut state = store.load_session(session)?;
        let mut allowed = state.feedbacks.contains(&query.digest);
        // Hidden auxiliary input is intentionally absent from ordinary history.
        if let Some(request) = state.active_request
            && let Some(submission) = state.submissions.get(&request)
        {
            let input = crate::input::load(submission.input, store.artifacts())?;
            allowed |= input.messages.iter().any(|message| {
                message["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part["type"] == "tact_review" && part["digest"] == json!(query.digest)
                    })
                })
            });
        }
        for _ in 0..128 {
            if allowed {
                break;
            }
            let Some(parent) = state.parent else {
                break;
            };
            state = store.load_session_cursor(&parent)?;
            allowed = state.feedbacks.contains(&query.digest);
        }
        if !allowed {
            return Ok(
                json!({"error":"feedback is not attached to this session or an inherited cursor"}),
            );
        }
        let feedback = crate::feedback::read(store.artifacts(), query.digest)?;
        let total = feedback.body.chars().count();
        if query.offset > total {
            return Ok(json!({"error":"feedback offset exceeds its length"}));
        }
        let text = feedback
            .body
            .chars()
            .skip(query.offset)
            .take(query.limit)
            .collect::<String>();
        let end = query.offset + text.chars().count();
        Ok(
            json!({"feedback":query.digest,"source_identity":feedback.source_identity,"manifest":feedback.manifest,"disposition":feedback.disposition,"text":text,"offset":query.offset,"next":(end<total).then_some(end),"total_characters":total,"authority":"human feedback only; not evaluator evidence"}),
        )
    }
    pub async fn inspect_session_review(
        &self,
        session: SessionId,
        cancellation: CancellationToken,
    ) -> Result<crate::review::ReviewInspection, HostError> {
        let (seed, artifacts) = {
            let store = self.store.lock().await;
            let state = store.load_session(session)?;
            (
                state.branch.workspace.ok_or(HostError::Invalid(
                    "session has no frozen private workspace",
                ))?,
                store.artifacts().clone(),
            )
        };
        let before = Snapshot::load(seed.origin, &artifacts)?;
        let after = Snapshot::load(seed.source, &artifacts)?;
        Ok(crate::review::inspect_snapshots(&before, &after, &artifacts, &cancellation).await?)
    }
    pub async fn inspect_task_review(
        &self,
        task: TaskId,
        cancellation: CancellationToken,
    ) -> Result<(ArtifactView, crate::review::ReviewInspection), HostError> {
        let view = self.inspect_artifacts(task, cancellation.clone()).await?;
        let artifacts = self.store.lock().await.artifacts().clone();
        let before = Snapshot::load(
            view.baseline
                .ok_or(HostError::Invalid("task has no frozen baseline yet"))?,
            &artifacts,
        )?;
        let after = Snapshot::load(
            view.snapshot
                .ok_or(HostError::Invalid("task has no reviewable snapshot yet"))?,
            &artifacts,
        )?;
        let review =
            crate::review::inspect_snapshots(&before, &after, &artifacts, &cancellation).await?;
        Ok((view, review))
    }
    pub async fn review_catalog(
        &self,
        workspace: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<crate::review::ReviewCatalog, HostError> {
        let workspace = self.review_workspace(workspace)?;
        Ok(crate::review::catalog(&workspace, &cancellation).await?)
    }

    pub async fn inspect_workspace(
        &self,
        workspace: PathBuf,
        range: crate::review::ReviewRange,
        cancellation: CancellationToken,
    ) -> Result<crate::review::ReviewInspection, HostError> {
        let workspace = self.review_workspace(workspace)?;
        let artifacts = self.store.lock().await.artifacts().clone();
        Ok(crate::review::inspect(&workspace, range, &artifacts, &cancellation).await?)
    }

    fn review_workspace(&self, workspace: PathBuf) -> Result<PathBuf, HostError> {
        let workspace = workspace.canonicalize()?;
        if workspace.starts_with(&self.root) || self.root.starts_with(&workspace) {
            return Err(HostError::Invalid(
                "review workspace overlaps protected host state",
            ));
        }
        Ok(workspace)
    }

    pub async fn review_file(
        &self,
        manifest: crate::Digest,
        side: crate::review::ReviewSide,
        path: String,
    ) -> Result<Option<crate::review::FrozenFile>, HostError> {
        Ok(crate::review::file(
            self.store.lock().await.artifacts(),
            manifest,
            side,
            &path,
        )?)
    }

    pub async fn review_files(
        &self,
        manifest: crate::Digest,
        side: crate::review::ReviewSide,
        offset: usize,
        limit: usize,
    ) -> Result<crate::review::ReviewFilePage, HostError> {
        Ok(crate::review::page(
            self.store.lock().await.artifacts(),
            manifest,
            side,
            offset,
            limit,
        )?)
    }
}
