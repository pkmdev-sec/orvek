use crate::{
    EvaluatorIdentity, IsolationInstanceId, PolicyIdentity, ProposalIntent, ProposalRequest,
    TargetProfile, TrialPairSpec, TrialRunAssignment, TrialTransportCapability,
};
use thiserror::Error;

/// Host-authorized proposal work for one frozen target profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProposalDispatch {
    target: TargetProfile,
    request: ProposalRequest,
    intents: Vec<ProposalIntent>,
}

impl ProposalDispatch {
    pub const fn target(&self) -> TargetProfile {
        self.target
    }

    pub const fn request(&self) -> ProposalRequest {
        self.request
    }

    pub fn intents(&self) -> &[ProposalIntent] {
        &self.intents
    }
}

/// Binds proposal generation to identities registered by the trusted Host.
pub fn prepare_proposal_dispatch(
    target: TargetProfile,
    registered_policy: PolicyIdentity,
    request: ProposalRequest,
) -> Result<ProposalDispatch, ProposalDispatchError> {
    if request.model() != target.model {
        return Err(ProposalDispatchError::ModelMismatch);
    }
    if request.protocol() != target.protocol {
        return Err(ProposalDispatchError::ProtocolMismatch);
    }
    if request.policy() != registered_policy {
        return Err(ProposalDispatchError::PolicyMismatch);
    }

    Ok(ProposalDispatch {
        target,
        request,
        intents: request.intents(),
    })
}

#[derive(Debug, Error)]
pub enum ProposalDispatchError {
    #[error("proposal model does not match the frozen target model")]
    ModelMismatch,
    #[error("proposal protocol does not match the frozen target protocol")]
    ProtocolMismatch,
    #[error("proposal policy is not registered by the Host")]
    PolicyMismatch,
}

/// Host-authorized paired trial work with a distinct isolation instance per side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialDispatch {
    target: TargetProfile,
    evaluator: EvaluatorIdentity,
    pair: TrialPairSpec,
    runs: [TrialRunAssignment; 2],
}

impl TrialDispatch {
    pub const fn target(&self) -> TargetProfile {
        self.target
    }

    pub const fn evaluator(&self) -> EvaluatorIdentity {
        self.evaluator
    }

    pub const fn pair(&self) -> TrialPairSpec {
        self.pair
    }

    pub const fn runs(&self) -> &[TrialRunAssignment; 2] {
        &self.runs
    }
}

/// The current Docker API can fence a known job after a Host restart, but it
/// cannot recover evaluator output that was completed before the restart.
pub const fn native_trial_transport_capability() -> TrialTransportCapability {
    TrialTransportCapability::FenceUnknownAfterStart
}

pub fn prepare_trial_dispatch(
    target: TargetProfile,
    registered_evaluator: EvaluatorIdentity,
    pair: TrialPairSpec,
) -> Result<TrialDispatch, TrialDispatchError> {
    pair.validate()
        .map_err(|_| TrialDispatchError::InvalidPair)?;
    let runtime = pair.context().runtime();
    if runtime.model() != target.model {
        return Err(TrialDispatchError::ModelMismatch);
    }
    if runtime.protocol() != target.protocol {
        return Err(TrialDispatchError::ProtocolMismatch);
    }
    if runtime.environment() != target.environment {
        return Err(TrialDispatchError::EnvironmentMismatch);
    }
    if runtime.evaluator() != registered_evaluator {
        return Err(TrialDispatchError::EvaluatorMismatch);
    }

    let [first, second] = pair.ordered_sides();
    let capability = native_trial_transport_capability();
    let runs = [
        TrialRunAssignment::new(
            pair.key(first)
                .map_err(|_| TrialDispatchError::InvalidPair)?,
            IsolationInstanceId::new(),
            capability,
        ),
        TrialRunAssignment::new(
            pair.key(second)
                .map_err(|_| TrialDispatchError::InvalidPair)?,
            IsolationInstanceId::new(),
            capability,
        ),
    ];

    Ok(TrialDispatch {
        target,
        evaluator: registered_evaluator,
        pair,
        runs,
    })
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TrialDispatchError {
    #[error("trial pair is invalid")]
    InvalidPair,
    #[error("trial model does not match the frozen target model")]
    ModelMismatch,
    #[error("trial protocol does not match the frozen target protocol")]
    ProtocolMismatch,
    #[error("trial environment does not match the frozen target environment")]
    EnvironmentMismatch,
    #[error("trial evaluator is not registered by the Host")]
    EvaluatorMismatch,
}
