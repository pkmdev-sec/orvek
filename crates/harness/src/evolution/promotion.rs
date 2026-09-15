//! Final-audit certificates, bounded-channel approval, and rollback naming.
//!
//! Phase-13 boundary: verification alone cannot move a pointer; approval
//! names a campaign, certificate, and channel (never a raw digest); activation
//! compares-and-swaps the expected base; rollback names a prior activation
//! receipt or the last-known-good receipt. Public structures carry identities
//! and commitments only — never raw final outcomes.

use crate::{
    Digest,
    evolution::{ApprovalDecision, CampaignId, FinalVerdict, MonitoringOutcome, RollbackReceiptId},
};
use serde::{Deserialize, Serialize};

/// Everything a one-use final audit binds into a certificate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalAuditDataset {
    pub campaign: CampaignId,
    pub cohort: crate::evolution::CohortId,
    pub epoch: crate::evolution::AuditEpochId,
    /// The verified verdict: candidate, revision, and its audit report.
    pub verdict: FinalVerdict,
    /// The adaptive score report that verified the candidate.
    pub score: crate::evolution::ScoreResultId,
    /// The composition lineage that produced the revision.
    pub composition: crate::evolution::CompositionId,
    /// Digest of the sealed final-audit evidence consumed by the audit.
    pub evidence: Digest,
    /// Exact campaign and cohort aggregate roots at audit time.
    pub campaign_root: Digest,
    pub cohort_root: Digest,
    /// Shared cohort ledger debits after the audit.
    pub ledger: crate::evolution::LedgerStatus,
    /// The active revision the activation expects to replace.
    pub expected_base: Digest,
    /// The revision a rollback restores, if a prior activation exists.
    pub rollback_target: Option<Digest>,
}

/// A content-addressed certificate over a final-audit dataset. The digest is
/// the certificate identity referenced by approvals and activation receipts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationCertificate {
    digest: Digest,
    dataset: FinalAuditDataset,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum PromotionError {
    #[error("only a verified final verdict can issue a certificate")]
    UnverifiedVerdict,
    #[error("campaign {0} is not awaiting approval")]
    NotAwaitingApproval(CampaignId),
    #[error("certificate {0} does not belong to campaign {1}")]
    CertificateCampaign(Digest, CampaignId),
    #[error("certificate {0} revises {1}, not {2}")]
    CertificateRevision(Digest, Digest, Digest),
    #[error("channel {0} is not a bounded approval channel")]
    UnboundedChannel(String),
    #[error("rollback must name a prior activation receipt or last-known-good")]
    UnnamedRollback,
    #[error("receipt {0} does not exist for campaign {1}")]
    UnknownReceipt(Digest, CampaignId),
    #[error("receipt {0} activated {1}, not {2}")]
    ReceiptRevision(Digest, Digest, Digest),
}

impl ActivationCertificate {
    /// Issues a certificate over a verified final audit. The dataset is
    /// canonicalized so the digest binds every field exactly.
    pub fn issue(dataset: FinalAuditDataset) -> Result<Self, PromotionError> {
        if !matches!(dataset.verdict, FinalVerdict::Verified { .. }) {
            return Err(PromotionError::UnverifiedVerdict);
        }
        let canonical =
            serde_json::to_vec(&dataset).map_err(|_| PromotionError::UnverifiedVerdict)?;
        let digest = Digest::of(&canonical);
        Ok(Self { digest, dataset })
    }

    pub const fn digest(&self) -> Digest {
        self.digest
    }

    pub const fn dataset(&self) -> &FinalAuditDataset {
        &self.dataset
    }

    /// The revision this certificate proposes to activate.
    pub fn revision(&self) -> Digest {
        self.dataset.verdict.revision()
    }
}

/// A bounded approval request: campaign, certificate, channel. The channel is
/// part of the request so an approval can never be replayed across channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApprovalRequest<'a> {
    pub campaign: CampaignId,
    pub certificate: &'a ActivationCertificate,
    pub channel: crate::evolution::Channel,
}

impl ApprovalRequest<'_> {
    /// Validates the request against the certificate and returns the decision
    /// an operator approval may record. Approval cannot nominate an arbitrary
    /// digest: the revision always comes from the certificate.
    pub fn validate(
        &self,
        awaiting_revision: Digest,
        approval_id: Digest,
    ) -> Result<ApprovalDecision, PromotionError> {
        let certificate = self.certificate;
        if certificate.dataset().campaign != self.campaign {
            return Err(PromotionError::CertificateCampaign(
                certificate.digest(),
                self.campaign,
            ));
        }
        if certificate.revision() != awaiting_revision {
            return Err(PromotionError::CertificateRevision(
                certificate.digest(),
                certificate.revision(),
                awaiting_revision,
            ));
        }
        if !matches!(self.channel, crate::evolution::Channel::Canary) {
            // Stable-channel rollout stays disabled until the open-rollout
            // prerequisites are committed; canary is the only bounded channel.
            return Err(PromotionError::UnboundedChannel(
                self.channel.as_str().to_owned(),
            ));
        }
        Ok(ApprovalDecision::Approved {
            id: crate::evolution::ApprovalId::from_digest(approval_id),
            revision: certificate.revision(),
        })
    }
}

/// What a rollback restores: a named prior activation receipt, or the
/// last-known-good receipt that activated the currently monitored revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackTarget {
    PriorReceipt {
        receipt: crate::evolution::ActivationReceiptId,
        restores: Digest,
    },
    LastKnownGood {
        activation: crate::evolution::ActivationReceiptId,
    },
}

/// Validates a rollback request and derives the monitoring outcome. The
/// caller supplies a receipt lookup returning `(from, to)` revisions, so
/// validation stays pure and the store stays the only receipt authority.
pub fn validate_rollback(
    campaign: CampaignId,
    from_revision: Digest,
    target: RollbackTarget,
    receipt_revisions: impl Fn(crate::evolution::ActivationReceiptId) -> Option<(Digest, Digest)>,
    report: crate::evolution::MonitoringReportId,
    reason: &str,
) -> Result<(MonitoringOutcome, RollbackReceiptId), PromotionError> {
    let restored = match target {
        RollbackTarget::PriorReceipt { receipt, restores } => {
            let Some((_, activated)) = receipt_revisions(receipt) else {
                return Err(PromotionError::UnknownReceipt(receipt.digest(), campaign));
            };
            if activated != restores {
                return Err(PromotionError::ReceiptRevision(
                    receipt.digest(),
                    activated,
                    restores,
                ));
            }
            restores
        }
        RollbackTarget::LastKnownGood { activation } => {
            // The named receipt must be the one that activated the revision
            // now being rolled back; last-known-good restores what that
            // activation replaced.
            let Some((activated_from, activated_to)) = receipt_revisions(activation) else {
                return Err(PromotionError::UnknownReceipt(
                    activation.digest(),
                    campaign,
                ));
            };
            if activated_to != from_revision {
                return Err(PromotionError::ReceiptRevision(
                    activation.digest(),
                    activated_to,
                    from_revision,
                ));
            }
            activated_from
        }
    };
    let receipt = RollbackReceiptId::from_digest(Digest::of(
        &serde_json::to_vec(&serde_json::json!({
            "campaign": campaign.to_string(),
            "from": from_revision.to_string(),
            "restored": restored.to_string(),
            "report": report.to_string(),
            "reason": reason,
        }))
        .expect("rollback receipt payload serializes"),
    ));
    Ok((
        MonitoringOutcome::RolledBack {
            from_revision,
            restored_revision: restored,
            report,
            receipt,
        },
        receipt,
    ))
}
