use super::{MAX_EVENT_BYTES, Store, StoreError, aggregate_hash, now_ms};
use crate::{
    AdaptivePromotionDataset, AdaptivePromotionRef, AdaptivePromotionReservation,
    AdaptiveScoreReport, AdaptiveVerdict, AuditEpochId, AuditEpochStatus, BaselineReason,
    CampaignEvent, CampaignId, CampaignState, CandidateId, CandidateStage, CohortId, CohortLedger,
    CompositeOutcome, CompositeStage, CompositionError, CompositionFailure,
    CompositionFailureReason, CompositionInput, DecisionCoordinates, Digest, EvaluationCohortSpec,
    FinalAuditAccess, FinalAuditRef, FinalAuditReservation, FinalVerdict, HarnessBinding,
    HarnessProvenance, LedgerLimit, LedgerStatus, MiningEvidenceRef, MiningEvidenceReservation,
    PolicyIdentity, RoundId, RoundVerdict, RoundVerdictId, ScoreResultId, SelectedHarness,
    TargetProfile, TerminalState, ValidatedHarnessRevision, VerifiedComposition, apply_campaign,
    artifacts::ArtifactError,
    compose_candidate, compose_candidates,
    evolution::{
        EvidencePurpose, EvidenceReservation, LedgerDebit, SealedArtifactRef, evaluate_adaptive,
        scoring_policy_digest, selected_composition_id,
    },
    session::{SessionAdmissionProfile, SessionAdmissionRequest},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use uuid::Uuid;

const MAX_BLOCK_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_COHORT_SPEC_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCORE_REPORT_BYTES: usize = 2 * 1024 * 1024;

impl Store {
    pub(crate) fn validate_session_profile(
        &self,
        profile: &SessionAdmissionProfile,
    ) -> Result<(), StoreError> {
        profile.validate().map_err(StoreError::Invalid)?;
        let binding = profile.binding();
        let revision = match profile.provenance() {
            HarnessProvenance::Registered => {
                if !target_revision_is_registered(
                    &self.connection,
                    binding.target(),
                    binding.revision(),
                )? {
                    return Err(StoreError::Integrity(
                        "session admission revision is not registered for its target",
                    ));
                }
                load_validated_revision(&self.connection, binding)?
            }
            HarnessProvenance::CompiledBaseline { .. } => {
                let revision = ValidatedHarnessRevision::compiled_baseline()?;
                let policy =
                    PolicyIdentity::from_digest(Digest::of(revision.policy_id().as_bytes()));
                if binding.revision() != revision.digest()
                    || binding.behavior() != revision.behavior_digest()
                    || binding.envelope() != revision.envelope_digest()
                    || binding.policy() != policy
                {
                    return Err(StoreError::Integrity(
                        "compiled session admission differs from its baseline",
                    ));
                }
                revision
            }
        };
        if profile.revision_manifest() != revision.canonical_bytes() {
            return Err(StoreError::Integrity(
                "session admission manifest differs from its immutable revision",
            ));
        }
        Ok(())
    }

    pub(crate) fn bind_session_request(
        &self,
        request: SessionAdmissionRequest,
        target: TargetProfile,
        authority: Digest,
        fallback_reason: BaselineReason,
    ) -> Result<SessionAdmissionProfile, StoreError> {
        let (binding, revision, provenance) = match self.resolve_harness(target)? {
            Some(binding) => (
                binding,
                load_validated_revision(&self.connection, binding)?,
                HarnessProvenance::Registered,
            ),
            None => {
                let revision = ValidatedHarnessRevision::compiled_baseline()?;
                let policy =
                    PolicyIdentity::from_digest(Digest::of(revision.policy_id().as_bytes()));
                let binding = HarnessBinding::baseline(
                    target,
                    revision.digest(),
                    revision.behavior_digest(),
                    revision.envelope_digest(),
                    policy,
                );
                (
                    binding,
                    revision,
                    HarnessProvenance::CompiledBaseline {
                        reason: fallback_reason,
                    },
                )
            }
        };
        SessionAdmissionProfile::new(request, binding, provenance, authority, &revision)
            .map_err(StoreError::from)
    }

    pub(crate) fn bind_baseline_session_request(
        &self,
        request: SessionAdmissionRequest,
        target: TargetProfile,
        authority: Digest,
        reason: BaselineReason,
    ) -> Result<SessionAdmissionProfile, StoreError> {
        let revision = ValidatedHarnessRevision::compiled_baseline()?;
        let policy = PolicyIdentity::from_digest(Digest::of(revision.policy_id().as_bytes()));
        let binding = HarnessBinding::baseline(
            target,
            revision.digest(),
            revision.behavior_digest(),
            revision.envelope_digest(),
            policy,
        );
        SessionAdmissionProfile::new(
            request,
            binding,
            HarnessProvenance::CompiledBaseline { reason },
            authority,
            &revision,
        )
        .map_err(StoreError::from)
    }

    /// Registers the compiled behavior baseline for one exact target key.
    ///
    /// No caller-supplied revision is accepted here. Later activation remains a
    /// separate certificate-gated operation.
    pub fn register_supported_target(
        &mut self,
        target: TargetProfile,
    ) -> Result<HarnessBinding, StoreError> {
        let baseline = ValidatedHarnessRevision::compiled_baseline()?;
        let policy = PolicyIdentity::from_digest(Digest::of(baseline.policy_id().as_bytes()));
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created_at_ms = storage_integer(now_ms(), "baseline timestamp exceeds storage bounds")?;

        transaction.execute(
            "INSERT OR IGNORE INTO harness_revisions(
                digest,parent_digest,behavior_digest,envelope_digest,policy_id,policy_digest,manifest,created_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                baseline.digest().to_string(),
                baseline.parent().to_string(),
                baseline.behavior_digest().to_string(),
                baseline.envelope_digest().to_string(),
                baseline.policy_id(),
                policy.to_string(),
                baseline.canonical_bytes(),
                created_at_ms,
            ],
        )?;
        verify_stored_baseline(&transaction, &baseline, policy)?;

        transaction.execute(
            "INSERT OR IGNORE INTO harness_targets(
                model_digest,protocol_digest,environment_digest,task_profile_digest,channel,
                baseline_revision,active_revision,updated_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?6,?7)",
            params![
                target.model.to_string(),
                target.protocol.to_string(),
                target.environment.to_string(),
                target.task_profile.to_string(),
                target.channel.as_str(),
                baseline.digest().to_string(),
                created_at_ms,
            ],
        )?;
        register_target_revision(&transaction, target, baseline.digest(), created_at_ms)?;

        let (registered_baseline, binding) = load_target_binding(&transaction, target)?
            .ok_or(StoreError::Integrity("registered target is missing"))?;
        if registered_baseline != baseline.digest() {
            return Err(StoreError::Integrity(
                "target baseline differs from compiled behavior",
            ));
        }
        transaction.commit()?;
        Ok(binding)
    }

    /// Resolves one exact model/protocol/environment/task/channel key.
    pub fn resolve_harness(
        &self,
        target: TargetProfile,
    ) -> Result<Option<HarnessBinding>, StoreError> {
        load_target_binding(&self.connection, target)
            .map(|binding| binding.map(|(_, binding)| binding))
    }

    /// Loads and revalidates one immutable harness revision by content digest.
    pub fn load_harness_revision(
        &self,
        revision: Digest,
    ) -> Result<ValidatedHarnessRevision, StoreError> {
        load_immutable_harness_revision(&self.connection, revision)
    }

    /// Freezes a Store-global cohort and both non-resettable ledgers atomically.
    pub fn register_evaluation_cohort(
        &mut self,
        spec: &EvaluationCohortSpec,
    ) -> Result<(), StoreError> {
        spec.validate().map_err(StoreError::Invalid)?;
        let block_manifest = serde_json::to_vec(&spec.blocks)?;
        if block_manifest.len() > MAX_BLOCK_MANIFEST_BYTES {
            return Err(StoreError::Invalid(
                "evaluation block manifest exceeds storage limit",
            ));
        }
        let cohort_spec = serde_json::to_vec(&spec)?;
        if cohort_spec.len() > MAX_COHORT_SPEC_BYTES {
            return Err(StoreError::Invalid(
                "evaluation cohort specification exceeds storage limit",
            ));
        }
        let cohort_spec_digest = Digest::of(&cohort_spec);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if cohort_exists(&transaction, spec.id)? {
            return Err(StoreError::Invalid(
                "evaluation cohort identity cannot be reused",
            ));
        }
        let (_, binding) = load_target_binding(&transaction, spec.target)?
            .ok_or(StoreError::Invalid("evaluation target is not registered"))?;
        if binding.revision() != spec.base_revision {
            return Err(StoreError::Invalid(
                "evaluation cohort base is not the active target revision",
            ));
        }
        if binding.policy() != spec.policy {
            return Err(StoreError::Invalid(
                "evaluation cohort policy differs from the base revision",
            ));
        }

        let created_at_ms = storage_integer(now_ms(), "cohort timestamp exceeds storage bounds")?;
        transaction.execute(
            "INSERT INTO evaluation_cohorts(
                id,model_digest,protocol_digest,environment_digest,task_profile_digest,channel,
                base_revision,evaluator_digest,policy_digest,mining_commitment,adaptive_commitment,
                final_commitment,block_manifest,created_at_ms,cohort_spec,cohort_spec_digest
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                spec.id.to_string(),
                spec.target.model.to_string(),
                spec.target.protocol.to_string(),
                spec.target.environment.to_string(),
                spec.target.task_profile.to_string(),
                spec.target.channel.as_str(),
                spec.base_revision.to_string(),
                spec.evaluator.to_string(),
                spec.policy.to_string(),
                spec.partitions.mining.to_string(),
                spec.partitions.adaptive_promotion.to_string(),
                spec.partitions.final_audit.to_string(),
                block_manifest,
                created_at_ms,
                cohort_spec,
                cohort_spec_digest.to_string(),
            ],
        )?;
        insert_ledger(
            &transaction,
            spec.id,
            CohortLedger::AdaptivePromotion,
            spec.adaptive_promotion,
        )?;
        insert_ledger(
            &transaction,
            spec.id,
            CohortLedger::FinalAudit,
            spec.final_audit,
        )?;
        transaction.execute(
            "INSERT INTO audit_epochs(cohort,epoch,status,created_at_ms,retired_at_ms)
             VALUES (?1,?2,'active',?3,NULL)",
            params![
                spec.id.to_string(),
                spec.audit_epoch.to_string(),
                created_at_ms
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn cohort_ledger_status(
        &self,
        cohort: CohortId,
        ledger: CohortLedger,
    ) -> Result<LedgerStatus, StoreError> {
        load_ledger_status(&self.connection, cohort, ledger)
    }

    /// Starts a campaign against the immutable base and policy frozen by its cohort.
    pub fn create_campaign(&mut self, event: CampaignEvent) -> Result<CampaignState, StoreError> {
        let state = apply_campaign(None, &event)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_campaign_cohort(&transaction, &state)?;

        let bytes = campaign_event_bytes(&event)?;
        let head = aggregate_hash(
            "campaign",
            state.id().as_uuid(),
            state.revision(),
            None,
            &bytes,
        )?;
        transaction.execute(
            "INSERT INTO events(aggregate,kind,revision,event,hash)
             VALUES (?1,'campaign',?2,?3,?4)",
            params![
                state.id().to_string(),
                storage_integer(state.revision(), "campaign revision exceeds storage bounds")?,
                bytes,
                head.to_string(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO campaigns(id,cohort,revision,state,head) VALUES (?1,?2,?3,?4,?5)",
            params![
                state.id().to_string(),
                state.cohort().to_string(),
                storage_integer(state.revision(), "campaign revision exceeds storage bounds")?,
                serde_json::to_vec(&state)?,
                head.to_string(),
            ],
        )?;
        transaction.commit()?;
        Ok(state)
    }

    /// Replays and validates the complete campaign journal before returning its projection.
    pub fn load_campaign(&self, id: CampaignId) -> Result<CampaignState, StoreError> {
        load_campaign_state(&self.connection, id).map(|stored| stored.state)
    }

    /// Appends one legal campaign transition with optimistic revision fencing.
    /// Evaluation results require typed boundaries that atomically persist their evidence and
    /// ledger use.
    pub fn append_campaign_event(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        event: CampaignEvent,
    ) -> Result<CampaignState, StoreError> {
        match &event {
            CampaignEvent::CandidateScoreRecorded { .. } => {
                return Err(StoreError::Invalid(
                    "candidate score requires typed adaptive scoring",
                ));
            }
            CampaignEvent::RoundVerdictRecorded {
                verdict: RoundVerdict::Compose { .. } | RoundVerdict::ComposeComposite { .. },
                ..
            }
            | CampaignEvent::CompositionRecorded { .. }
            | CampaignEvent::CompositeCompositionRecorded { .. }
            | CampaignEvent::CompositeCompositionFailed { .. } => {
                return Err(StoreError::Invalid(
                    "composition requires typed verified candidate authority",
                ));
            }
            CampaignEvent::CompositeScoreRecorded { .. } => {
                return Err(StoreError::Invalid(
                    "composite score requires typed adaptive scoring",
                ));
            }
            CampaignEvent::FinalVerdictRecorded { .. } => {
                return Err(StoreError::Invalid(
                    "final verdict requires an atomic final-audit ledger debit",
                ));
            }
            _ => {}
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        let next = apply_campaign(Some(&stored.state), &event)?;
        if next.revision() == stored.state.revision() {
            return Ok(stored.state);
        }
        append_campaign_event_in_transaction(&transaction, &stored, &next, &event)?;
        transaction.commit()?;
        Ok(next)
    }

    /// Selects the only verified candidate in one round, persists its immutable revision, and
    /// records the verdict and composition lineage in one transaction.
    pub fn compose_verified_candidate(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        round: RoundId,
        input: CompositionInput<'_>,
    ) -> Result<(CampaignState, SelectedHarness), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        validate_composition_inputs_authority(&transaction, &stored.state, round, &[input])?;
        let parent = load_immutable_harness_revision(&transaction, stored.state.base_revision())?;
        let selected = compose_candidate(&parent, input)?;
        persist_harness_revision(&transaction, selected.revision())?;

        let verdict_event = CampaignEvent::RoundVerdictRecorded {
            round,
            verdict: RoundVerdict::Compose {
                candidate: selected.candidate(),
                basis: RoundVerdictId::from_digest(selected.composition().digest()),
            },
        };
        let composing = apply_campaign(Some(&stored.state), &verdict_event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &composing, &verdict_event)?;

        let stored = load_campaign_state(&transaction, id)?;
        let composition_event = CampaignEvent::CompositionRecorded {
            round,
            candidate: selected.candidate(),
            composition: selected.composition(),
            revision: selected.revision().digest(),
        };
        let auditing = apply_campaign(Some(&stored.state), &composition_event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &auditing, &composition_event)?;
        transaction.commit()?;
        Ok((auditing, selected))
    }

    /// Composes every verified candidate in one round, stores the immutable result, and moves the
    /// campaign to a fresh composite trial. Deterministic merge failures terminally keep the
    /// parent while preserving the exact verified child set in the journal.
    pub fn compose_verified_candidates(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        round: RoundId,
        inputs: &[CompositionInput<'_>],
    ) -> Result<(CampaignState, VerifiedComposition), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        validate_composition_inputs_authority(&transaction, &stored.state, round, inputs)?;
        let parent = load_immutable_harness_revision(&transaction, stored.state.base_revision())?;
        let composition = match compose_candidates(&parent, inputs.iter().copied()) {
            Ok(composed) => VerifiedComposition::Composed(Box::new(composed)),
            Err(CompositionError::FieldConflict {
                field,
                first,
                second,
            }) => VerifiedComposition::FellBack(CompositionFailure::from_inputs(
                &parent,
                inputs.iter().copied(),
                CompositionFailureReason::FieldConflict {
                    field,
                    first,
                    second,
                },
            )?),
            Err(CompositionError::Manifest(_)) => {
                VerifiedComposition::FellBack(CompositionFailure::from_inputs(
                    &parent,
                    inputs.iter().copied(),
                    CompositionFailureReason::CombinedManifestRejected,
                )?)
            }
            Err(error) => return Err(error.into()),
        };

        let (candidate, composition_id) = match &composition {
            VerifiedComposition::Composed(composed) => {
                persist_harness_revision(&transaction, composed.revision())?;
                (composed.plan().candidate(), composed.plan().id())
            }
            VerifiedComposition::FellBack(failure) => (failure.candidate(), failure.id()),
        };

        let verdict_event = CampaignEvent::RoundVerdictRecorded {
            round,
            verdict: RoundVerdict::ComposeComposite {
                candidate,
                composition: composition_id,
                basis: RoundVerdictId::from_digest(composition_id.digest()),
            },
        };
        let composing = apply_campaign(Some(&stored.state), &verdict_event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &composing, &verdict_event)?;

        let stored = load_campaign_state(&transaction, id)?;
        let composition_event = match &composition {
            VerifiedComposition::Composed(composed) => {
                CampaignEvent::CompositeCompositionRecorded {
                    round,
                    plan: composed.plan().clone(),
                }
            }
            VerifiedComposition::FellBack(failure) => CampaignEvent::CompositeCompositionFailed {
                round,
                failure: failure.clone(),
            },
        };
        let next = apply_campaign(Some(&stored.state), &composition_event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &next, &composition_event)?;
        transaction.commit()?;
        Ok((next, composition))
    }

    /// Scores one adaptive dataset, charges its frozen family allocation, stores the exact
    /// content-addressed report, and advances the candidate in one transaction.
    pub fn record_adaptive_score(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        round: RoundId,
        dataset: AdaptivePromotionDataset,
        coordinates: DecisionCoordinates,
    ) -> Result<(CampaignState, AdaptiveScoreReport), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        let cohort = load_scoring_cohort(&transaction, stored.state.cohort())?;
        let ledger = load_ledger_status(
            &transaction,
            stored.state.cohort(),
            CohortLedger::AdaptivePromotion,
        )?;
        ensure_score_coordinates_available(&transaction, cohort.id, coordinates)?;
        let report = evaluate_adaptive(&cohort, id, round, &dataset, ledger, coordinates)?;
        let result_id = report.result_id()?;
        let is_composite = stored
            .state
            .rounds()
            .iter()
            .find(|state| state.id() == round)
            .and_then(|state| state.composite())
            .is_some_and(|composite| composite.plan().candidate() == dataset.candidate());
        let event = if is_composite {
            CampaignEvent::CompositeScoreRecorded {
                round,
                candidate: dataset.candidate(),
                score: result_id,
                outcome: composite_outcome(report.verdict),
            }
        } else {
            CampaignEvent::CandidateScoreRecorded {
                round,
                candidate: dataset.candidate(),
                score: result_id,
            }
        };
        let next = apply_campaign(Some(&stored.state), &event)?;
        persist_adaptive_score_report(&transaction, result_id, &report)?;
        if report.ledger.query_debit > 0 || report.ledger.error_nanos_debit > 0 {
            let after = debit_cohort_ledger_in_transaction(
                &transaction,
                next.cohort(),
                CohortLedger::AdaptivePromotion,
                LedgerDebit {
                    campaign: id,
                    use_id: result_id.digest(),
                    queries: report.ledger.query_debit,
                    error_nanos: report.ledger.error_nanos_debit,
                },
            )?;
            if after != report.ledger.after {
                return Err(StoreError::Integrity(
                    "adaptive score report ledger transition differs from stored usage",
                ));
            }
        }
        append_campaign_event_in_transaction(&transaction, &stored, &next, &event)?;
        transaction.commit()?;
        Ok((next, report))
    }

    /// Loads and revalidates an immutable content-addressed adaptive score report.
    pub fn load_adaptive_score_report(
        &self,
        result: ScoreResultId,
    ) -> Result<AdaptiveScoreReport, StoreError> {
        load_adaptive_score_report(&self.connection, result)
    }

    pub fn reserve_mining_evidence(
        &mut self,
        cohort: CohortId,
        max_bytes: u64,
    ) -> Result<MiningEvidenceReservation, StoreError> {
        self.reserve_evidence(cohort, EvidencePurpose::Mining, max_bytes)
            .map(MiningEvidenceReservation)
    }

    pub fn stage_mining_evidence(
        &mut self,
        reservation: &MiningEvidenceReservation,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.stage_evidence(&reservation.0, bytes)
    }

    pub fn commit_mining_evidence(
        &mut self,
        reservation: MiningEvidenceReservation,
    ) -> Result<MiningEvidenceRef, StoreError> {
        self.commit_evidence(reservation.0).map(MiningEvidenceRef)
    }

    pub fn cancel_mining_evidence(
        &mut self,
        reservation: MiningEvidenceReservation,
    ) -> Result<(), StoreError> {
        self.cancel_evidence(reservation.0)
    }

    pub fn read_mining_evidence(&self, evidence: MiningEvidenceRef) -> Result<Vec<u8>, StoreError> {
        self.read_evidence(evidence.0)
    }

    pub fn reserve_adaptive_promotion_evidence(
        &mut self,
        cohort: CohortId,
        max_bytes: u64,
    ) -> Result<AdaptivePromotionReservation, StoreError> {
        self.reserve_evidence(cohort, EvidencePurpose::AdaptivePromotion, max_bytes)
            .map(AdaptivePromotionReservation)
    }

    pub fn stage_adaptive_promotion_evidence(
        &mut self,
        reservation: &AdaptivePromotionReservation,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.stage_evidence(&reservation.0, bytes)
    }

    pub fn commit_adaptive_promotion_evidence(
        &mut self,
        reservation: AdaptivePromotionReservation,
    ) -> Result<AdaptivePromotionRef, StoreError> {
        self.commit_evidence(reservation.0)
            .map(AdaptivePromotionRef)
    }

    pub fn cancel_adaptive_promotion_evidence(
        &mut self,
        reservation: AdaptivePromotionReservation,
    ) -> Result<(), StoreError> {
        self.cancel_evidence(reservation.0)
    }

    pub fn read_adaptive_promotion_evidence(
        &self,
        evidence: AdaptivePromotionRef,
    ) -> Result<Vec<u8>, StoreError> {
        self.read_evidence(evidence.0)
    }

    pub fn reserve_final_audit_evidence(
        &mut self,
        cohort: CohortId,
        max_bytes: u64,
    ) -> Result<FinalAuditReservation, StoreError> {
        self.reserve_evidence(cohort, EvidencePurpose::FinalAudit, max_bytes)
            .map(FinalAuditReservation)
    }

    pub fn stage_final_audit_evidence(
        &mut self,
        reservation: &FinalAuditReservation,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.stage_evidence(&reservation.0, bytes)
    }

    pub fn commit_final_audit_evidence(
        &mut self,
        reservation: FinalAuditReservation,
    ) -> Result<FinalAuditRef, StoreError> {
        self.commit_evidence(reservation.0).map(FinalAuditRef)
    }

    pub fn cancel_final_audit_evidence(
        &mut self,
        reservation: FinalAuditReservation,
    ) -> Result<(), StoreError> {
        self.cancel_evidence(reservation.0)
    }

    /// Retires the epoch before touching sealed bytes. A missing or corrupt
    /// artifact therefore cannot preserve a reusable final-audit partition.
    pub fn read_final_audit_evidence(
        &mut self,
        access: FinalAuditAccess,
    ) -> Result<Vec<u8>, StoreError> {
        let evidence = access.evidence.0;
        self.verify_evidence_metadata(&self.connection, evidence)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_audit_epoch(&transaction, evidence.cohort, access.epoch)?;
        transaction.commit()?;
        self.read_sealed_bytes(evidence)
    }

    pub fn audit_epoch_status(
        &self,
        cohort: CohortId,
        epoch: AuditEpochId,
    ) -> Result<AuditEpochStatus, StoreError> {
        let status: Option<String> = self
            .connection
            .query_row(
                "SELECT status FROM audit_epochs WHERE cohort=?1 AND epoch=?2",
                params![cohort.to_string(), epoch.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match status.as_deref() {
            Some("active") => Ok(AuditEpochStatus::Active),
            Some("retired") => Ok(AuditEpochStatus::Retired),
            Some(_) => Err(StoreError::Integrity("unknown audit epoch status")),
            None if cohort_exists(&self.connection, cohort)? => Err(StoreError::Invalid(
                "audit epoch identity does not match cohort",
            )),
            None => Err(StoreError::MissingCohort(cohort)),
        }
    }

    // ── Phase 13: certificates, approval, activation, rollback ────────────

    /// Records the final audit verdict with an atomic final-audit ledger
    /// debit; a verdict can never be appended through the generic path.
    pub fn record_final_verdict(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        verdict: FinalVerdict,
    ) -> Result<CampaignState, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        let debit_queries = 1u64;
        let debit_error_nanos = 1u64;
        debit_cohort_ledger_in_transaction(
            &transaction,
            stored.state.cohort(),
            CohortLedger::FinalAudit,
            LedgerDebit {
                campaign: id,
                use_id: verdict.report().digest(),
                queries: debit_queries,
                error_nanos: debit_error_nanos,
            },
        )?;
        let event = CampaignEvent::FinalVerdictRecorded { verdict };
        let next = apply_campaign(Some(&stored.state), &event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &next, &event)?;
        transaction.commit()?;
        Ok(next)
    }

    /// Persists an immutable activation certificate.
    pub fn persist_activation_certificate(
        &mut self,
        certificate: &crate::evolution::promotion::ActivationCertificate,
    ) -> Result<(), StoreError> {
        let dataset = certificate.dataset();
        let payload = serde_json::to_vec(dataset)
            .map_err(|_| StoreError::Integrity("certificate payload is not canonical"))?;
        let created_at_ms = storage_integer(now_ms(), "certificate timestamp overflows storage")?;
        self.connection.execute(
            "INSERT INTO activation_certificates(
                 digest,campaign,cohort,revision,expected_base,payload,created_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                certificate.digest().to_string(),
                dataset.campaign.to_string(),
                dataset.cohort.to_string(),
                dataset.verdict.revision().to_string(),
                dataset.expected_base.to_string(),
                payload,
                created_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Loads a certificate by digest, revalidating its payload binding.
    pub fn read_activation_certificate(
        &self,
        digest: Digest,
    ) -> Result<crate::evolution::promotion::ActivationCertificate, StoreError> {
        let (payload,): (Vec<u8>,) = self.connection.query_row(
            "SELECT payload FROM activation_certificates WHERE digest=?1",
            params![digest.to_string()],
            |row| Ok((row.get(0)?,)),
        )?;
        let dataset: crate::evolution::promotion::FinalAuditDataset =
            serde_json::from_slice(&payload)
                .map_err(|_| StoreError::Integrity("certificate payload is unreadable"))?;
        let certificate = crate::evolution::promotion::ActivationCertificate::issue(dataset)
            .map_err(|_| StoreError::Integrity("certificate digest does not bind its payload"))?;
        if certificate.digest() != digest {
            return Err(StoreError::Integrity(
                "certificate digest does not bind its payload",
            ));
        }
        Ok(certificate)
    }

    /// Records an operator approval bound to a stored certificate. The
    /// decision revision must equal both the certificate revision and the
    /// campaign's awaiting revision.
    pub fn record_campaign_approval(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        decision: crate::evolution::ApprovalDecision,
        certificate: Digest,
    ) -> Result<CampaignState, StoreError> {
        let revision = decision.revision();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        let certificate_row: (String, String) = transaction.query_row(
            "SELECT campaign,revision FROM activation_certificates WHERE digest=?1",
            params![certificate.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if certificate_row.0 != id.to_string() || certificate_row.1 != revision.to_string() {
            return Err(StoreError::Invalid(
                "approval must reference this campaign's certificate revision",
            ));
        }
        let event = crate::evolution::CampaignEvent::ApprovalRecorded { decision };
        let approved = apply_campaign(Some(&stored.state), &event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &approved, &event)?;
        transaction.commit()?;
        Ok(approved)
    }

    /// Compare-and-swaps the target's active revision and journals the
    /// activation. Only a committed CAS emits `Activated`; a lost race
    /// records `Superseded` with the surviving revision.
    pub fn activate_harness_revision(
        &mut self,
        id: CampaignId,
        expected_revision: u64,
        target: crate::evolution::TargetProfile,
        certificate: Digest,
        expected_active: Digest,
    ) -> Result<(CampaignState, crate::evolution::ActivationOutcome), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_campaign_state(&transaction, id)?;
        require_campaign_revision(&stored.state, expected_revision)?;
        let new_revision: Digest = {
            let (campaign, revision): (String, String) = transaction.query_row(
                "SELECT campaign,revision FROM activation_certificates WHERE digest=?1",
                params![certificate.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if campaign != id.to_string() {
                return Err(StoreError::Invalid(
                    "certificate belongs to a different campaign",
                ));
            }
            revision
                .parse()
                .map_err(|_| StoreError::Integrity("certificate revision is not a digest"))?
        };
        // Compare-and-swap: exactly one approved campaign can move a pointer
        // off its expected base; every loser records supersession.
        let updated = transaction.execute(
            "UPDATE harness_targets SET active_revision=?7,updated_at_ms=?8
             WHERE model_digest=?1 AND protocol_digest=?2 AND environment_digest=?3
               AND task_profile_digest=?4 AND channel=?5 AND active_revision=?6",
            params![
                target.model.to_string(),
                target.protocol.to_string(),
                target.environment.to_string(),
                target.task_profile.to_string(),
                target.channel.as_str(),
                expected_active.to_string(),
                new_revision.to_string(),
                storage_integer(now_ms(), "activation timestamp overflows storage")?,
            ],
        )?;
        let timestamp = storage_integer(now_ms(), "receipt timestamp overflows storage")?;
        let receipt = crate::evolution::ActivationReceiptId::from_digest(Digest::of(
            &serde_json::to_vec(&serde_json::json!({
                "campaign": id.to_string(),
                "certificate": certificate.to_string(),
                "revision": new_revision.to_string(),
                "expected_base": expected_active.to_string(),
            }))
            .expect("activation receipt payload serializes"),
        ));
        let outcome = if updated == 1 {
            crate::evolution::ActivationOutcome::Activated {
                revision: new_revision,
                receipt,
            }
        } else {
            let (active,): (String,) = transaction.query_row(
                "SELECT active_revision FROM harness_targets
                 WHERE model_digest=?1 AND protocol_digest=?2 AND environment_digest=?3
                   AND task_profile_digest=?4 AND channel=?5",
                params![
                    target.model.to_string(),
                    target.protocol.to_string(),
                    target.environment.to_string(),
                    target.task_profile.to_string(),
                    target.channel.as_str(),
                ],
                |row| Ok((row.get(0)?,)),
            )?;
            let active: Digest = active
                .parse()
                .map_err(|_| StoreError::Integrity("active revision is not a digest"))?;
            crate::evolution::ActivationOutcome::Superseded {
                requested_revision: new_revision,
                active_revision: active,
                receipt,
            }
        };
        let superseded = u8::from(matches!(
            outcome,
            crate::evolution::ActivationOutcome::Superseded { .. }
        ));
        transaction.execute(
            "INSERT INTO activation_receipts(
                 receipt,campaign,certificate,from_revision,to_revision,superseded,created_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                receipt.to_string(),
                id.to_string(),
                certificate.to_string(),
                expected_active.to_string(),
                new_revision.to_string(),
                superseded,
                timestamp,
            ],
        )?;
        let event = crate::evolution::CampaignEvent::ActivationRecorded { outcome };
        let activated = apply_campaign(Some(&stored.state), &event)?;
        append_campaign_event_in_transaction(&transaction, &stored, &activated, &event)?;
        transaction.commit()?;
        Ok((activated, outcome))
    }

    /// Returns the (from, to) revisions an activation receipt recorded.
    pub fn activation_receipt_revisions(
        &self,
        receipt: crate::evolution::ActivationReceiptId,
    ) -> Result<Option<(Digest, Digest)>, StoreError> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT from_revision,to_revision FROM activation_receipts WHERE receipt=?1",
                params![receipt.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(from, to)| {
            let from: Digest = from
                .parse()
                .map_err(|_| StoreError::Integrity("receipt from-revision is not a digest"))?;
            let to: Digest = to
                .parse()
                .map_err(|_| StoreError::Integrity("receipt to-revision is not a digest"))?;
            Ok((from, to))
        })
        .transpose()
    }

    /// Permanently retires an audit epoch. There is deliberately no reopen or
    /// replacement operation for an existing cohort.
    pub fn retire_audit_epoch(
        &mut self,
        cohort: CohortId,
        epoch: AuditEpochId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        retire_audit_epoch(&transaction, cohort, epoch)?;
        transaction.commit()?;
        Ok(())
    }

    fn reserve_evidence(
        &mut self,
        cohort: CohortId,
        purpose: EvidencePurpose,
        max_bytes: u64,
    ) -> Result<EvidenceReservation, StoreError> {
        if max_bytes == 0 || max_bytes > i64::MAX as u64 {
            return Err(StoreError::Invalid(
                "artifact reservation must fit storage bounds and be positive",
            ));
        }
        let quota = self.artifact_staging.quota();
        let _guard = quota.lock()?;
        quota.ensure_capacity_locked(max_bytes)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !cohort_exists(&transaction, cohort)? {
            return Err(StoreError::MissingCohort(cohort));
        }
        let reservation = EvidenceReservation {
            id: Uuid::new_v4(),
            cohort,
            max_bytes,
            purpose,
        };
        transaction.execute(
            "INSERT INTO evolution_artifact_reservations(
                id,cohort,purpose,max_bytes,staged_digest,staged_bytes,created_at_ms
             ) VALUES (?1,?2,?3,?4,NULL,NULL,?5)",
            params![
                reservation.id.to_string(),
                reservation.cohort.to_string(),
                reservation.purpose.as_str(),
                storage_integer(
                    reservation.max_bytes,
                    "artifact reservation exceeds storage bounds"
                )?,
                storage_integer(now_ms(), "artifact timestamp exceeds storage bounds")?,
            ],
        )?;
        transaction.commit()?;
        Ok(reservation)
    }

    fn stage_evidence(
        &mut self,
        reservation: &EvidenceReservation,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        if bytes.len() as u64 > reservation.max_bytes {
            return Err(StoreError::Invalid(
                "staged evidence exceeds its reservation",
            ));
        }
        let staging = self.artifact_staging.clone();
        let quota = staging.quota();
        let _guard = quota.lock()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_reservation(&transaction, reservation)?;
        if stored.staged_digest.is_some() {
            return Err(StoreError::Invalid(
                "artifact reservation is already staged",
            ));
        }
        let digest = opaque_evidence_error(staging.write_locked(reservation.id, bytes))?;
        let updated = transaction.execute(
            "UPDATE evolution_artifact_reservations
             SET staged_digest=?2,staged_bytes=?3 WHERE id=?1 AND staged_digest IS NULL",
            params![
                reservation.id.to_string(),
                digest.to_string(),
                storage_integer(bytes.len() as u64, "staged artifact exceeds storage bounds")?
            ],
        );
        match updated {
            Ok(1) => transaction.commit().map_err(StoreError::from),
            Ok(_) => {
                opaque_evidence_error(staging.remove_locked(reservation.id))?;
                Err(StoreError::Integrity(
                    "artifact reservation changed while staging",
                ))
            }
            Err(error) => {
                opaque_evidence_error(staging.remove_locked(reservation.id))?;
                Err(error.into())
            }
        }
    }

    fn commit_evidence(
        &mut self,
        reservation: EvidenceReservation,
    ) -> Result<SealedArtifactRef, StoreError> {
        let staging = self.artifact_staging.clone();
        let sealed = self.sealed_artifacts.clone();
        let quota = staging.quota();
        let _guard = quota.lock()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = load_reservation(&transaction, &reservation)?;
        let (digest, bytes) = stored
            .staged_digest
            .zip(stored.staged_bytes)
            .ok_or(StoreError::Invalid("artifact reservation is not staged"))?;
        opaque_evidence_error(staging.promote_locked(reservation.id, digest, &sealed))?;
        insert_evidence_metadata(
            &transaction,
            digest,
            reservation.cohort,
            reservation.purpose,
            bytes,
        )?;
        let removed = transaction.execute(
            "DELETE FROM evolution_artifact_reservations WHERE id=?1",
            [reservation.id.to_string()],
        )?;
        if removed != 1 {
            return Err(StoreError::Integrity(
                "artifact reservation disappeared during commit",
            ));
        }
        transaction.commit()?;
        Ok(SealedArtifactRef::registered(
            digest,
            reservation.cohort,
            reservation.purpose,
        ))
    }

    fn cancel_evidence(&mut self, reservation: EvidenceReservation) -> Result<(), StoreError> {
        let staging = self.artifact_staging.clone();
        let quota = staging.quota();
        let _guard = quota.lock()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_reservation(&transaction, &reservation)?;
        let removed = transaction.execute(
            "DELETE FROM evolution_artifact_reservations WHERE id=?1",
            [reservation.id.to_string()],
        )?;
        if removed != 1 {
            return Err(StoreError::Integrity(
                "artifact reservation disappeared during cancellation",
            ));
        }
        transaction.commit()?;
        opaque_evidence_error(staging.remove_locked(reservation.id))
    }

    fn read_evidence(&self, evidence: SealedArtifactRef) -> Result<Vec<u8>, StoreError> {
        self.verify_evidence_metadata(&self.connection, evidence)?;
        self.read_sealed_bytes(evidence)
    }

    fn verify_evidence_metadata(
        &self,
        connection: &Connection,
        evidence: SealedArtifactRef,
    ) -> Result<u64, StoreError> {
        let bytes: Option<i64> = connection
            .query_row(
                "SELECT bytes FROM evolution_evidence
                 WHERE digest=?1 AND cohort=?2 AND purpose=?3",
                params![
                    evidence.digest.to_string(),
                    evidence.cohort.to_string(),
                    evidence.purpose.as_str()
                ],
                |row| row.get(0),
            )
            .optional()?;
        bytes
            .ok_or(StoreError::Invalid(
                "sealed evidence capability is not registered",
            ))
            .and_then(|bytes| stored_u64(bytes, "invalid sealed evidence length"))
    }

    fn read_sealed_bytes(&self, evidence: SealedArtifactRef) -> Result<Vec<u8>, StoreError> {
        let expected = self.verify_evidence_metadata(&self.connection, evidence)?;
        let bytes = opaque_evidence_error(self.sealed_artifacts.read(evidence.digest))?;
        if bytes.len() as u64 != expected {
            return Err(StoreError::Integrity(
                "sealed evidence length differs from metadata",
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn recover_evolution_artifacts(&mut self) -> Result<(), StoreError> {
        let public = self.artifacts.clone();
        let staging = self.artifact_staging.clone();
        let sealed = self.sealed_artifacts.clone();
        let quota = staging.quota();
        let _guard = quota.lock()?;
        public.collect_temporary_locked()?;
        opaque_evidence_error(sealed.collect_temporary_locked())?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservations = {
            let mut statement = transaction.prepare(
                "SELECT id,cohort,purpose,max_bytes,staged_digest,staged_bytes
                 FROM evolution_artifact_reservations ORDER BY id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (id, cohort, purpose, max_bytes, staged_digest, staged_bytes) in reservations {
            let id = Uuid::parse_str(&id)
                .map_err(|_| StoreError::Integrity("invalid artifact reservation identity"))?;
            let cohort = cohort
                .parse()
                .map_err(|_| StoreError::Integrity("invalid artifact cohort identity"))?;
            let purpose = EvidencePurpose::parse(&purpose).map_err(StoreError::Integrity)?;
            let max_bytes = stored_u64(max_bytes, "invalid artifact reservation size")?;
            match (staged_digest, staged_bytes) {
                (Some(digest), Some(bytes)) => {
                    let digest = parse_digest(&digest, "invalid staged artifact identity")?;
                    let bytes = stored_u64(bytes, "invalid staged artifact size")?;
                    if bytes > max_bytes {
                        return Err(StoreError::Integrity(
                            "staged artifact exceeds its reservation",
                        ));
                    }
                    if opaque_evidence_error(staging.has_locked(id))? {
                        opaque_evidence_error(staging.promote_locked(id, digest, &sealed))?;
                    } else {
                        opaque_evidence_error(sealed.read(digest))?;
                    }
                    insert_evidence_metadata(&transaction, digest, cohort, purpose, bytes)?;
                }
                (None, None) => {}
                _ => {
                    return Err(StoreError::Integrity(
                        "artifact reservation has partial staged metadata",
                    ));
                }
            }
            transaction.execute(
                "DELETE FROM evolution_artifact_reservations WHERE id=?1",
                [id.to_string()],
            )?;
        }
        transaction.commit()?;
        opaque_evidence_error(staging.collect_all_locked())?;

        let mut statement = self
            .connection
            .prepare("SELECT DISTINCT digest FROM evolution_evidence ORDER BY digest")?;
        let referenced = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|digest| digest?.parse().map_err(|_| rusqlite::Error::InvalidQuery))
            .collect::<Result<BTreeSet<Digest>, _>>()?;
        drop(statement);
        for digest in opaque_evidence_error(sealed.digests_locked())? {
            if !referenced.contains(&digest) {
                opaque_evidence_error(sealed.remove_locked(digest))?;
            }
        }
        Ok(())
    }
}

fn validate_composition_inputs_authority(
    transaction: &Transaction<'_>,
    state: &CampaignState,
    round: RoundId,
    inputs: &[CompositionInput<'_>],
) -> Result<(), StoreError> {
    let round_state = state
        .rounds()
        .iter()
        .find(|candidate_round| candidate_round.id() == round)
        .ok_or(StoreError::Invalid(
            "composition round is not in the campaign",
        ))?;
    let mut selected = BTreeSet::new();
    for input in inputs {
        if !selected.insert(input.candidate()) {
            return Err(StoreError::Invalid(
                "composition candidate occurs more than once",
            ));
        }
    }
    let mut verified = BTreeSet::new();
    for candidate in round_state.candidates() {
        let CandidateStage::Scored {
            proposal, score, ..
        } = candidate.stage()
        else {
            return Err(StoreError::Invalid(
                "composition requires every round candidate to be scored",
            ));
        };
        let report = load_adaptive_score_report(transaction, score)?;
        if report.cohort != state.cohort()
            || report.campaign != state.id()
            || report.round != round
            || report.candidate != candidate.id()
        {
            return Err(StoreError::Integrity(
                "adaptive score report differs from its composition candidate",
            ));
        }
        if report.verdict == AdaptiveVerdict::Verified {
            verified.insert(candidate.id());
        }
        if let Some(input) = inputs
            .iter()
            .find(|input| input.candidate() == candidate.id())
            && (input.proposal().id() != proposal || input.score() != score)
        {
            return Err(StoreError::Invalid(
                "composition input differs from its scored campaign lineage",
            ));
        }
    }
    if selected != verified {
        return Err(StoreError::Invalid(
            "composition must include every and only verified candidate",
        ));
    }
    Ok(())
}

fn opaque_evidence_error<T>(result: Result<T, ArtifactError>) -> Result<T, StoreError> {
    result.map_err(|_| StoreError::EvidenceUnavailable)
}

#[derive(Clone, Copy)]
struct StoredReservation {
    staged_digest: Option<Digest>,
    staged_bytes: Option<u64>,
}

struct StoredReservationRow {
    cohort: String,
    purpose: String,
    max_bytes: i64,
    staged_digest: Option<String>,
    staged_bytes: Option<i64>,
}

fn load_reservation(
    connection: &Connection,
    reservation: &EvidenceReservation,
) -> Result<StoredReservation, StoreError> {
    let row: Option<StoredReservationRow> = connection
        .query_row(
            "SELECT cohort,purpose,max_bytes,staged_digest,staged_bytes
             FROM evolution_artifact_reservations WHERE id=?1",
            [reservation.id.to_string()],
            |row| {
                Ok(StoredReservationRow {
                    cohort: row.get(0)?,
                    purpose: row.get(1)?,
                    max_bytes: row.get(2)?,
                    staged_digest: row.get(3)?,
                    staged_bytes: row.get(4)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(StoreError::Invalid(
            "artifact reservation is unknown or already consumed",
        ));
    };
    if row.cohort != reservation.cohort.to_string()
        || row.purpose != reservation.purpose.as_str()
        || stored_u64(row.max_bytes, "invalid artifact reservation size")? != reservation.max_bytes
    {
        return Err(StoreError::Integrity(
            "artifact reservation metadata differs from its capability",
        ));
    }
    Ok(StoredReservation {
        staged_digest: row
            .staged_digest
            .map(|digest| parse_digest(&digest, "invalid staged artifact identity"))
            .transpose()?,
        staged_bytes: row
            .staged_bytes
            .map(|bytes| stored_u64(bytes, "invalid staged artifact size"))
            .transpose()?,
    })
}

fn insert_evidence_metadata(
    connection: &Connection,
    digest: Digest,
    cohort: CohortId,
    purpose: EvidencePurpose,
    bytes: u64,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT OR IGNORE INTO evolution_evidence(digest,cohort,purpose,bytes,created_at_ms)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            digest.to_string(),
            cohort.to_string(),
            purpose.as_str(),
            storage_integer(bytes, "evidence size exceeds storage bounds")?,
            storage_integer(now_ms(), "evidence timestamp exceeds storage bounds")?,
        ],
    )?;
    let stored: i64 = connection.query_row(
        "SELECT bytes FROM evolution_evidence WHERE digest=?1 AND cohort=?2 AND purpose=?3",
        params![digest.to_string(), cohort.to_string(), purpose.as_str()],
        |row| row.get(0),
    )?;
    if stored_u64(stored, "invalid sealed evidence length")? != bytes {
        return Err(StoreError::Integrity(
            "sealed evidence metadata differs from committed content",
        ));
    }
    Ok(())
}

fn retire_audit_epoch(
    connection: &Connection,
    cohort: CohortId,
    epoch: AuditEpochId,
) -> Result<(), StoreError> {
    let retired_at_ms = storage_integer(now_ms(), "audit timestamp exceeds storage bounds")?;
    let changed = connection.execute(
        "UPDATE audit_epochs SET status='retired',retired_at_ms=?3
         WHERE cohort=?1 AND epoch=?2 AND status='active'",
        params![cohort.to_string(), epoch.to_string(), retired_at_ms],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let status: Option<(String, String)> = connection
        .query_row(
            "SELECT epoch,status FROM audit_epochs WHERE cohort=?1",
            [cohort.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match status {
        None => Err(StoreError::MissingCohort(cohort)),
        Some((stored, _)) if stored != epoch.to_string() => Err(StoreError::Invalid(
            "audit epoch identity does not match cohort",
        )),
        Some((_, _)) => Err(StoreError::Invalid("audit epoch is already retired")),
    }
}

fn verify_stored_baseline(
    connection: &Connection,
    baseline: &ValidatedHarnessRevision,
    policy: PolicyIdentity,
) -> Result<(), StoreError> {
    let stored: (String, String, String, String, String, Vec<u8>) = connection.query_row(
        "SELECT parent_digest,behavior_digest,envelope_digest,policy_id,policy_digest,manifest
         FROM harness_revisions WHERE digest=?1",
        [baseline.digest().to_string()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let exact = stored.0 == baseline.parent().to_string()
        && stored.1 == baseline.behavior_digest().to_string()
        && stored.2 == baseline.envelope_digest().to_string()
        && stored.3 == baseline.policy_id()
        && stored.4 == policy.to_string()
        && stored.5 == baseline.canonical_bytes();
    if !exact {
        return Err(StoreError::Integrity(
            "stored baseline revision differs from compiled behavior",
        ));
    }
    Ok(())
}

fn persist_harness_revision(
    transaction: &Transaction<'_>,
    revision: &ValidatedHarnessRevision,
) -> Result<(), StoreError> {
    let policy = revision.policy_identity();
    transaction.execute(
        "INSERT OR IGNORE INTO harness_revisions(
            digest,parent_digest,behavior_digest,envelope_digest,policy_id,policy_digest,manifest,created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            revision.digest().to_string(),
            revision.parent().to_string(),
            revision.behavior_digest().to_string(),
            revision.envelope_digest().to_string(),
            revision.policy_id(),
            policy.to_string(),
            revision.canonical_bytes(),
            storage_integer(now_ms(), "revision timestamp exceeds storage bounds")?,
        ],
    )?;
    let stored: (String, String, String, String, String, Vec<u8>) = transaction.query_row(
        "SELECT parent_digest,behavior_digest,envelope_digest,policy_id,policy_digest,manifest
         FROM harness_revisions WHERE digest=?1",
        [revision.digest().to_string()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    if stored.0 != revision.parent().to_string()
        || stored.1 != revision.behavior_digest().to_string()
        || stored.2 != revision.envelope_digest().to_string()
        || stored.3 != revision.policy_id()
        || stored.4 != policy.to_string()
        || stored.5 != revision.canonical_bytes()
    {
        return Err(StoreError::Integrity(
            "stored revision differs from the composed harness",
        ));
    }
    Ok(())
}

const fn composite_outcome(verdict: AdaptiveVerdict) -> CompositeOutcome {
    match verdict {
        AdaptiveVerdict::Verified => CompositeOutcome::Verified,
        AdaptiveVerdict::NotVerified => CompositeOutcome::NotVerified,
        AdaptiveVerdict::Inconclusive => CompositeOutcome::Inconclusive,
    }
}

fn load_target_binding(
    connection: &Connection,
    target: TargetProfile,
) -> Result<Option<(Digest, HarnessBinding)>, StoreError> {
    let row: Option<(String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT t.baseline_revision,t.active_revision,r.behavior_digest,r.envelope_digest,
                    r.policy_id,r.policy_digest
             FROM harness_targets t
             JOIN harness_revisions r ON r.digest=t.active_revision
             WHERE t.model_digest=?1 AND t.protocol_digest=?2 AND t.environment_digest=?3
               AND t.task_profile_digest=?4 AND t.channel=?5",
            params![
                target.model.to_string(),
                target.protocol.to_string(),
                target.environment.to_string(),
                target.task_profile.to_string(),
                target.channel.as_str(),
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((baseline, revision, behavior, envelope, policy_id, policy_digest)) = row else {
        return Ok(None);
    };
    let baseline = parse_digest(&baseline, "invalid target baseline identity")?;
    let revision = parse_digest(&revision, "invalid active harness identity")?;
    let behavior = parse_digest(&behavior, "invalid behavior identity")?;
    let envelope = parse_digest(&envelope, "invalid envelope identity")?;
    let policy_digest = parse_digest(&policy_digest, "invalid policy identity")?;
    if Digest::of(policy_id.as_bytes()) != policy_digest {
        return Err(StoreError::Integrity(
            "policy identity does not match policy",
        ));
    }
    Ok(Some((
        baseline,
        HarnessBinding::registered(
            target,
            revision,
            behavior,
            envelope,
            PolicyIdentity::from_digest(policy_digest),
        ),
    )))
}

fn target_revision_is_registered(
    connection: &Connection,
    target: TargetProfile,
    revision: Digest,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT 1 FROM harness_target_revisions
             WHERE model_digest=?1 AND protocol_digest=?2 AND environment_digest=?3
               AND task_profile_digest=?4 AND channel=?5 AND revision=?6",
            params![
                target.model.to_string(),
                target.protocol.to_string(),
                target.environment.to_string(),
                target.task_profile.to_string(),
                target.channel.as_str(),
                revision.to_string(),
            ],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(StoreError::from)
}

fn register_target_revision(
    transaction: &Transaction<'_>,
    target: TargetProfile,
    revision: Digest,
    bound_at_ms: i64,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT OR IGNORE INTO harness_target_revisions(
            model_digest,protocol_digest,environment_digest,task_profile_digest,channel,
            revision,bound_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            target.model.to_string(),
            target.protocol.to_string(),
            target.environment.to_string(),
            target.task_profile.to_string(),
            target.channel.as_str(),
            revision.to_string(),
            bound_at_ms,
        ],
    )?;
    Ok(())
}

fn load_validated_revision(
    connection: &Connection,
    binding: HarnessBinding,
) -> Result<ValidatedHarnessRevision, StoreError> {
    let revision = load_immutable_harness_revision(connection, binding.revision())?;
    if revision.behavior_digest() != binding.behavior()
        || revision.envelope_digest() != binding.envelope()
        || revision.policy_identity() != binding.policy()
    {
        return Err(StoreError::Integrity(
            "active harness revision differs from its registry binding",
        ));
    }
    Ok(revision)
}

fn load_immutable_harness_revision(
    connection: &Connection,
    digest: Digest,
) -> Result<ValidatedHarnessRevision, StoreError> {
    let stored: (String, String, String, String, String, Vec<u8>) = connection
        .query_row(
            "SELECT parent_digest,behavior_digest,envelope_digest,policy_id,policy_digest,manifest
             FROM harness_revisions WHERE digest=?1",
            [digest.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::MissingHarnessRevision(digest))?;
    let revision = ValidatedHarnessRevision::from_manifest_json(&stored.5)?;
    let exact = revision.digest() == digest
        && revision.parent().to_string() == stored.0
        && revision.behavior_digest().to_string() == stored.1
        && revision.envelope_digest().to_string() == stored.2
        && revision.policy_id() == stored.3
        && revision.policy_identity().to_string() == stored.4
        && Digest::of(stored.3.as_bytes()).to_string() == stored.4;
    if !exact {
        return Err(StoreError::Integrity(
            "immutable harness revision differs from its stored identity",
        ));
    }
    Ok(revision)
}

fn insert_ledger(
    transaction: &Transaction<'_>,
    cohort: CohortId,
    role: CohortLedger,
    limit: LedgerLimit,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO cohort_ledgers(
            cohort,role,query_limit,error_limit_nanos,query_used,error_used_nanos
         ) VALUES (?1,?2,?3,?4,0,0)",
        params![
            cohort.to_string(),
            role.as_str(),
            storage_integer(limit.queries, "query limit exceeds storage bounds")?,
            storage_integer(limit.error_nanos, "error limit exceeds storage bounds")?,
        ],
    )?;
    Ok(())
}

fn load_scoring_cohort(
    connection: &Connection,
    cohort: CohortId,
) -> Result<EvaluationCohortSpec, StoreError> {
    let stored: Option<(Option<Vec<u8>>, Option<String>)> = connection
        .query_row(
            "SELECT cohort_spec,cohort_spec_digest FROM evaluation_cohorts WHERE id=?1",
            [cohort.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((Some(bytes), Some(stored_digest))) = stored else {
        return if cohort_exists(connection, cohort)? {
            Err(StoreError::Invalid(
                "legacy evaluation cohort has no frozen scoring policy",
            ))
        } else {
            Err(StoreError::MissingCohort(cohort))
        };
    };
    if bytes.len() > MAX_COHORT_SPEC_BYTES {
        return Err(StoreError::Integrity(
            "stored evaluation cohort specification exceeds its limit",
        ));
    }
    let stored_digest = parse_digest(
        &stored_digest,
        "evaluation cohort specification has an invalid digest",
    )?;
    if Digest::of(&bytes) != stored_digest {
        return Err(StoreError::Integrity(
            "evaluation cohort specification differs from its digest",
        ));
    }
    let spec: EvaluationCohortSpec = serde_json::from_slice(&bytes)?;
    if spec.id != cohort {
        return Err(StoreError::Integrity(
            "evaluation cohort specification has the wrong identity",
        ));
    }
    spec.validate()
        .map_err(|_| StoreError::Integrity("stored evaluation cohort specification is invalid"))?;
    Ok(spec)
}

fn ensure_score_coordinates_available(
    transaction: &Transaction<'_>,
    cohort: CohortId,
    coordinates: DecisionCoordinates,
) -> Result<(), StoreError> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM adaptive_score_reports
             WHERE cohort=?1
               AND coordinate_candidate=?2
               AND coordinate_round=?3
               AND coordinate_composite=?4
               AND coordinate_fallback=?5
               AND coordinate_campaign=?6
               AND coordinate_activation_attempt=?7",
            params![
                cohort.to_string(),
                storage_integer(
                    coordinates.candidate,
                    "candidate decision coordinate exceeds storage bounds"
                )?,
                storage_integer(
                    coordinates.round,
                    "round decision coordinate exceeds storage bounds"
                )?,
                storage_integer(
                    coordinates.composite,
                    "composite decision coordinate exceeds storage bounds"
                )?,
                storage_integer(
                    coordinates.fallback,
                    "fallback decision coordinate exceeds storage bounds"
                )?,
                storage_integer(
                    coordinates.campaign,
                    "campaign decision coordinate exceeds storage bounds"
                )?,
                storage_integer(
                    coordinates.activation_attempt,
                    "activation decision coordinate exceeds storage bounds"
                )?,
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        return Err(StoreError::Invalid(
            "adaptive scoring decision coordinates were already used",
        ));
    }
    Ok(())
}

fn persist_adaptive_score_report(
    transaction: &Transaction<'_>,
    result: ScoreResultId,
    report: &AdaptiveScoreReport,
) -> Result<(), StoreError> {
    if report.result_id()? != result {
        return Err(StoreError::Integrity(
            "adaptive score report identity is invalid",
        ));
    }
    let bytes = serde_json::to_vec(report)?;
    if bytes.len() > MAX_SCORE_REPORT_BYTES {
        return Err(StoreError::Invalid(
            "adaptive score report exceeds storage limit",
        ));
    }
    transaction.execute(
        "INSERT INTO adaptive_score_reports(
            result_id,cohort,campaign,round,candidate,
            coordinate_candidate,coordinate_round,coordinate_composite,coordinate_fallback,
            coordinate_campaign,coordinate_activation_attempt,policy_digest,evidence_root,
            report,created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            result.to_string(),
            report.cohort.to_string(),
            report.campaign.to_string(),
            report.round.to_string(),
            report.candidate.to_string(),
            storage_integer(
                report.coordinates.candidate,
                "candidate decision coordinate exceeds storage bounds"
            )?,
            storage_integer(
                report.coordinates.round,
                "round decision coordinate exceeds storage bounds"
            )?,
            storage_integer(
                report.coordinates.composite,
                "composite decision coordinate exceeds storage bounds"
            )?,
            storage_integer(
                report.coordinates.fallback,
                "fallback decision coordinate exceeds storage bounds"
            )?,
            storage_integer(
                report.coordinates.campaign,
                "campaign decision coordinate exceeds storage bounds"
            )?,
            storage_integer(
                report.coordinates.activation_attempt,
                "activation decision coordinate exceeds storage bounds"
            )?,
            report.policy_digest.to_string(),
            report.evidence_root.to_string(),
            bytes,
            storage_integer(now_ms(), "score report timestamp exceeds storage bounds")?,
        ],
    )?;
    Ok(())
}

fn load_adaptive_score_report(
    connection: &Connection,
    result: ScoreResultId,
) -> Result<AdaptiveScoreReport, StoreError> {
    struct StoredAdaptiveScoreReport {
        cohort: String,
        campaign: String,
        round: String,
        candidate: String,
        policy: String,
        evidence: String,
        bytes: Vec<u8>,
    }

    let stored: Option<StoredAdaptiveScoreReport> = connection
        .query_row(
            "SELECT cohort,campaign,round,candidate,policy_digest,evidence_root,report
             FROM adaptive_score_reports WHERE result_id=?1",
            [result.to_string()],
            |row| {
                Ok(StoredAdaptiveScoreReport {
                    cohort: row.get(0)?,
                    campaign: row.get(1)?,
                    round: row.get(2)?,
                    candidate: row.get(3)?,
                    policy: row.get(4)?,
                    evidence: row.get(5)?,
                    bytes: row.get(6)?,
                })
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Err(StoreError::Invalid("adaptive score report is missing"));
    };
    if stored.bytes.len() > MAX_SCORE_REPORT_BYTES {
        return Err(StoreError::Integrity(
            "stored adaptive score report exceeds its limit",
        ));
    }
    let report: AdaptiveScoreReport = serde_json::from_slice(&stored.bytes)?;
    if report.result_id()? != result
        || report.cohort.to_string() != stored.cohort
        || report.campaign.to_string() != stored.campaign
        || report.round.to_string() != stored.round
        || report.candidate.to_string() != stored.candidate
        || report.policy_digest.to_string() != stored.policy
        || report.evidence_root.to_string() != stored.evidence
    {
        return Err(StoreError::Integrity(
            "adaptive score report differs from its immutable metadata",
        ));
    }
    let cohort = load_scoring_cohort(connection, report.cohort)?;
    if scoring_policy_digest(&cohort)? != report.policy_digest {
        return Err(StoreError::Integrity(
            "adaptive score report differs from its frozen cohort policy",
        ));
    }
    Ok(report)
}

fn debit_cohort_ledger_in_transaction(
    transaction: &Transaction<'_>,
    cohort: CohortId,
    ledger: CohortLedger,
    debit: LedgerDebit,
) -> Result<LedgerStatus, StoreError> {
    debit.validate().map_err(StoreError::Invalid)?;
    let previous: Option<(String, i64, i64)> = transaction
        .query_row(
            "SELECT campaign,queries,error_nanos FROM cohort_ledger_uses
             WHERE cohort=?1 AND role=?2 AND use_id=?3",
            params![
                cohort.to_string(),
                ledger.as_str(),
                debit.use_id.to_string()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((campaign, queries, error_nanos)) = previous {
        let exact = campaign == debit.campaign.to_string()
            && stored_u64(queries, "negative ledger debit")? == debit.queries
            && stored_u64(error_nanos, "negative ledger error debit")? == debit.error_nanos;
        if !exact {
            return Err(StoreError::Invalid(
                "cohort ledger use identity cannot be rewritten",
            ));
        }
        return load_ledger_status(transaction, cohort, ledger);
    }

    let current = load_ledger_status(transaction, cohort, ledger)?;
    let query_used = current
        .query_used
        .checked_add(debit.queries)
        .filter(|used| *used <= current.query_limit)
        .ok_or(StoreError::CohortLedgerExhausted)?;
    let error_used_nanos = current
        .error_used_nanos
        .checked_add(debit.error_nanos)
        .filter(|used| *used <= current.error_limit_nanos)
        .ok_or(StoreError::CohortLedgerExhausted)?;
    transaction.execute(
        "INSERT INTO cohort_ledger_uses(
            cohort,role,use_id,campaign,queries,error_nanos,created_at_ms
         ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            cohort.to_string(),
            ledger.as_str(),
            debit.use_id.to_string(),
            debit.campaign.to_string(),
            storage_integer(debit.queries, "query debit exceeds storage bounds")?,
            storage_integer(debit.error_nanos, "error debit exceeds storage bounds")?,
            storage_integer(now_ms(), "ledger timestamp exceeds storage bounds")?,
        ],
    )?;
    transaction.execute(
        "UPDATE cohort_ledgers SET query_used=?3,error_used_nanos=?4
         WHERE cohort=?1 AND role=?2",
        params![
            cohort.to_string(),
            ledger.as_str(),
            storage_integer(query_used, "query use exceeds storage bounds")?,
            storage_integer(error_used_nanos, "error use exceeds storage bounds")?,
        ],
    )?;
    Ok(LedgerStatus {
        query_limit: current.query_limit,
        query_used,
        error_limit_nanos: current.error_limit_nanos,
        error_used_nanos,
    })
}

fn load_ledger_status(
    connection: &Connection,
    cohort: CohortId,
    ledger: CohortLedger,
) -> Result<LedgerStatus, StoreError> {
    let row: Option<(i64, i64, i64, i64)> = connection
        .query_row(
            "SELECT query_limit,query_used,error_limit_nanos,error_used_nanos
             FROM cohort_ledgers WHERE cohort=?1 AND role=?2",
            params![cohort.to_string(), ledger.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((query_limit, query_used, error_limit, error_used)) = row else {
        return if cohort_exists(connection, cohort)? {
            Err(StoreError::Integrity("cohort ledger is missing"))
        } else {
            Err(StoreError::MissingCohort(cohort))
        };
    };
    let status = LedgerStatus {
        query_limit: stored_u64(query_limit, "invalid query limit")?,
        query_used: stored_u64(query_used, "invalid query use")?,
        error_limit_nanos: stored_u64(error_limit, "invalid error limit")?,
        error_used_nanos: stored_u64(error_used, "invalid error use")?,
    };
    if status.query_used > status.query_limit || status.error_used_nanos > status.error_limit_nanos
    {
        return Err(StoreError::Integrity("cohort ledger exceeds its limit"));
    }
    Ok(status)
}

fn cohort_exists(connection: &Connection, cohort: CohortId) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT 1 FROM evaluation_cohorts WHERE id=?1",
            [cohort.to_string()],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(StoreError::from)
}

struct StoredCampaign {
    state: CampaignState,
    head: Digest,
}

fn load_campaign_state(
    connection: &Connection,
    id: CampaignId,
) -> Result<StoredCampaign, StoreError> {
    let row: Option<(String, i64, Vec<u8>, String)> = connection
        .query_row(
            "SELECT cohort,revision,state,head FROM campaigns WHERE id=?1",
            [id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((stored_cohort, stored_revision, stored_state, stored_head)) = row else {
        return Err(StoreError::MissingCampaign(id));
    };
    let stored_cohort = stored_cohort
        .parse::<CohortId>()
        .map_err(|_| StoreError::Integrity("campaign has an invalid cohort identity"))?;
    let stored_revision = stored_u64(stored_revision, "campaign has an invalid revision")?;
    let cached: CampaignState = serde_json::from_slice(&stored_state)?;
    cached
        .validate()
        .map_err(|_| StoreError::Integrity("cached campaign projection is invalid"))?;
    if cached.id() != id || cached.cohort() != stored_cohort || cached.revision() != stored_revision
    {
        return Err(StoreError::Integrity(
            "cached campaign projection differs from its index",
        ));
    }

    let mut statement = connection.prepare(
        "SELECT revision,event,hash FROM events
         WHERE aggregate=?1 AND kind='campaign' ORDER BY revision",
    )?;
    let mut rows = statement.query([id.to_string()])?;
    let mut replayed: Option<CampaignState> = None;
    let mut replay_head: Option<Digest> = None;
    let mut expected_revision = 1_u64;
    while let Some(row) = rows.next()? {
        let revision = stored_u64(row.get(0)?, "campaign event has an invalid revision")?;
        let bytes: Vec<u8> = row.get(1)?;
        let hash: String = row.get(2)?;
        if revision != expected_revision {
            return Err(StoreError::Integrity(
                "campaign journal revisions are not contiguous",
            ));
        }
        if bytes.len() > MAX_EVENT_BYTES {
            return Err(StoreError::Integrity(
                "campaign event exceeds journal limit",
            ));
        }
        let event: CampaignEvent = serde_json::from_slice(&bytes)?;
        let next = apply_campaign(replayed.as_ref(), &event)
            .map_err(|_| StoreError::Integrity("campaign journal transition is invalid"))?;
        if next.id() != id || next.revision() != revision {
            return Err(StoreError::Integrity(
                "campaign journal projection does not match its revision",
            ));
        }
        let expected_hash =
            aggregate_hash("campaign", id.as_uuid(), revision, replay_head, &bytes)?;
        if hash != expected_hash.to_string() {
            return Err(StoreError::Integrity("campaign journal hash differs"));
        }
        replayed = Some(next);
        replay_head = Some(expected_hash);
        expected_revision = expected_revision
            .checked_add(1)
            .ok_or(StoreError::Integrity("campaign revision is exhausted"))?;
    }
    let replayed = replayed.ok_or(StoreError::Integrity("campaign journal is empty"))?;
    let replay_head = replay_head.ok_or(StoreError::Integrity("campaign journal is empty"))?;
    if replayed.cohort() != stored_cohort
        || replayed.revision() != stored_revision
        || replay_head.to_string() != stored_head
        || serde_json::to_vec(&replayed)? != stored_state
    {
        return Err(StoreError::Integrity(
            "campaign projection differs from its journal",
        ));
    }
    validate_campaign_cohort(connection, &replayed)?;
    validate_campaign_dependencies(connection, &replayed)?;
    Ok(StoredCampaign {
        state: replayed,
        head: replay_head,
    })
}

fn validate_campaign_cohort(
    connection: &Connection,
    state: &CampaignState,
) -> Result<(), StoreError> {
    let identity: Option<(String, String)> = connection
        .query_row(
            "SELECT base_revision,policy_digest FROM evaluation_cohorts WHERE id=?1",
            [state.cohort().to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((base_revision, policy)) = identity else {
        return Err(StoreError::MissingCohort(state.cohort()));
    };
    let base_revision = parse_digest(&base_revision, "cohort has an invalid base revision")?;
    let policy = policy
        .parse::<PolicyIdentity>()
        .map_err(|_| StoreError::Integrity("cohort has an invalid policy identity"))?;
    if state.base_revision() != base_revision || state.policy() != policy {
        return Err(StoreError::Invalid(
            "campaign base or policy differs from its immutable cohort",
        ));
    }
    Ok(())
}

fn validate_campaign_dependencies(
    connection: &Connection,
    state: &CampaignState,
) -> Result<(), StoreError> {
    for round in state.rounds() {
        let mut verified = BTreeSet::new();
        for candidate in round.candidates() {
            let (proposal, score, selected) = match candidate.stage() {
                CandidateStage::Proposed { .. } | CandidateStage::Trialed { .. } => continue,
                CandidateStage::Scored {
                    proposal, score, ..
                } => (proposal, score, None),
                CandidateStage::Composed {
                    proposal,
                    score,
                    composition,
                    revision,
                    ..
                }
                | CandidateStage::Audited {
                    proposal,
                    score,
                    composition,
                    revision,
                    ..
                } => (proposal, score, Some((composition, revision))),
            };
            let report =
                load_campaign_score_report(connection, state, round.id(), candidate.id(), score)?;
            if report.verdict == AdaptiveVerdict::Verified {
                verified.insert(candidate.id());
            }
            if let Some((composition, revision)) = selected {
                if report.verdict != AdaptiveVerdict::Verified {
                    return Err(StoreError::Integrity(
                        "selected campaign candidate is not backed by a verified score",
                    ));
                }
                let stored_revision = load_immutable_harness_revision(connection, revision)?;
                if stored_revision.parent() != state.base_revision()
                    || selected_composition_id(
                        state.base_revision(),
                        candidate.id(),
                        proposal,
                        score,
                        revision,
                    )? != composition
                {
                    return Err(StoreError::Integrity(
                        "selected campaign revision differs from its composition lineage",
                    ));
                }
            }
        }

        if let Some(composite) = round.composite() {
            let plan = composite.plan();
            let children = plan
                .children()
                .iter()
                .map(|child| child.candidate())
                .collect::<BTreeSet<_>>();
            if children != verified {
                return Err(StoreError::Integrity(
                    "composite plan is not backed by every verified candidate",
                ));
            }
            let revision = load_immutable_harness_revision(connection, plan.revision())?;
            if revision.parent() != plan.parent() {
                return Err(StoreError::Integrity(
                    "composite harness revision differs from its plan parent",
                ));
            }
            match composite.stage() {
                CompositeStage::AwaitingTrial | CompositeStage::Trialed { .. } => {}
                CompositeStage::Scored { score, outcome, .. } => {
                    let report = load_campaign_score_report(
                        connection,
                        state,
                        round.id(),
                        plan.candidate(),
                        score,
                    )?;
                    if composite_outcome(report.verdict) != outcome {
                        return Err(StoreError::Integrity(
                            "composite score outcome differs from its report",
                        ));
                    }
                }
                CompositeStage::Audited { score, .. } => {
                    let report = load_campaign_score_report(
                        connection,
                        state,
                        round.id(),
                        plan.candidate(),
                        score,
                    )?;
                    if report.verdict != AdaptiveVerdict::Verified {
                        return Err(StoreError::Integrity(
                            "audited composite is not backed by a verified score",
                        ));
                    }
                }
            }
        }

        match round.verdict() {
            Some(RoundVerdict::Compose { candidate, .. }) => {
                if verified != BTreeSet::from([candidate]) {
                    return Err(StoreError::Integrity(
                        "selected composition is not the only verified candidate",
                    ));
                }
            }
            Some(RoundVerdict::ComposeComposite { .. }) if round.composite().is_none() => {
                if let Some(TerminalState::CompositionFallback {
                    round: failed_round,
                    failure,
                }) = state.terminal()
                    && *failed_round == round.id()
                {
                    let children = failure
                        .children()
                        .iter()
                        .map(|child| child.candidate())
                        .collect::<BTreeSet<_>>();
                    if children != verified {
                        return Err(StoreError::Integrity(
                            "composition failure is not backed by every verified candidate",
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn load_campaign_score_report(
    connection: &Connection,
    state: &CampaignState,
    round: RoundId,
    candidate: CandidateId,
    score: ScoreResultId,
) -> Result<AdaptiveScoreReport, StoreError> {
    let report = load_adaptive_score_report(connection, score)?;
    if report.cohort != state.cohort()
        || report.campaign != state.id()
        || report.round != round
        || report.candidate != candidate
    {
        return Err(StoreError::Integrity(
            "adaptive score report differs from its campaign lineage",
        ));
    }
    Ok(report)
}

fn require_campaign_revision(
    state: &CampaignState,
    expected_revision: u64,
) -> Result<(), StoreError> {
    if state.revision() != expected_revision {
        return Err(StoreError::CampaignRevision {
            expected: expected_revision,
            actual: state.revision(),
        });
    }
    Ok(())
}

fn campaign_event_bytes(event: &CampaignEvent) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_json::to_vec(event)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(StoreError::Invalid("campaign event exceeds journal limit"));
    }
    Ok(bytes)
}

fn append_campaign_event_in_transaction(
    transaction: &Transaction<'_>,
    stored: &StoredCampaign,
    next: &CampaignState,
    event: &CampaignEvent,
) -> Result<(), StoreError> {
    if next.id() != stored.state.id()
        || next.cohort() != stored.state.cohort()
        || next.base_revision() != stored.state.base_revision()
        || next.policy() != stored.state.policy()
        || next.revision() != stored.state.revision() + 1
    {
        return Err(StoreError::Integrity(
            "campaign transition changed immutable identity or skipped a revision",
        ));
    }
    let bytes = campaign_event_bytes(event)?;
    let head = aggregate_hash(
        "campaign",
        next.id().as_uuid(),
        next.revision(),
        Some(stored.head),
        &bytes,
    )?;
    transaction.execute(
        "INSERT INTO events(aggregate,kind,revision,event,hash)
         VALUES (?1,'campaign',?2,?3,?4)",
        params![
            next.id().to_string(),
            storage_integer(next.revision(), "campaign revision exceeds storage bounds")?,
            bytes,
            head.to_string(),
        ],
    )?;
    let changed = transaction.execute(
        "UPDATE campaigns SET revision=?2,state=?3,head=?4
         WHERE id=?1 AND revision=?5 AND head=?6",
        params![
            next.id().to_string(),
            storage_integer(next.revision(), "campaign revision exceeds storage bounds")?,
            serde_json::to_vec(next)?,
            head.to_string(),
            storage_integer(
                stored.state.revision(),
                "campaign revision exceeds storage bounds"
            )?,
            stored.head.to_string(),
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::Integrity(
            "campaign projection changed during an atomic append",
        ));
    }
    Ok(())
}

fn parse_digest(value: &str, error: &'static str) -> Result<Digest, StoreError> {
    value.parse().map_err(|_| StoreError::Integrity(error))
}

fn storage_integer(value: u64, error: &'static str) -> Result<i64, StoreError> {
    value.try_into().map_err(|_| StoreError::Invalid(error))
}

fn stored_u64(value: i64, error: &'static str) -> Result<u64, StoreError> {
    value.try_into().map_err(|_| StoreError::Integrity(error))
}
