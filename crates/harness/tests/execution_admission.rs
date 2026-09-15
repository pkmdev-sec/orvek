use orvek_harness::{
    Store, StoreError,
    admission::{RepositoryProfile, RequestPolicy},
    contract::{
        BaselinePolicy, CheckDefinition, CheckKind, Contract, ControlRequirement, DeliveryKind,
        FlakePolicy, Limits, Origin, Requirement,
    },
    inference::ModelSettings,
    input,
    session::{SessionConfig, SessionId},
    state::{JobInvocation, JobStatus, TaskState},
    submission::{Schedule, WorkIntent},
};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    directory: tempfile::TempDir,
    store: Store,
    task: TaskState,
    invocation: JobInvocation,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let session = store
            .create_session(
                SessionId::new(),
                SessionConfig {
                    workspace: directory.path().into(),
                    model: ModelSettings::default(),
                    instructions: String::new(),
                    context_window_tokens: orvek_harness::context::DEFAULT_WINDOW_TOKENS,
                },
                None,
            )
            .unwrap();
        let artifact = store
            .public_artifacts()
            .write(
                &serde_json::to_vec(&RequestPolicy {
                    version: 1,
                    delivery: DeliveryKind::Source,
                    profile: RepositoryProfile {
                        version: 1,
                        name: "fixture".into(),
                        checks: Default::default(),
                    },
                })
                .unwrap(),
            )
            .unwrap()
            .digest();
        let request = Uuid::new_v4();
        let (_, task, _) = store
            .start_request(
                session.id,
                request,
                "Update the workspace".into(),
                Limits::default(),
                artifact,
            )
            .unwrap();
        Self {
            directory,
            store,
            task,
            invocation: JobInvocation {
                session: session.id,
                request,
                call_id: None,
                capability: "workspace.write".into(),
                input: artifact,
                environment: artifact,
            },
        }
    }

    fn admit_contract(&mut self, open_questions: Vec<String>) {
        let contract = Contract {
            request: self.task.request.clone(),
            outcome: "Workspace updated".into(),
            scope: "workspace".into(),
            requirements: vec![Requirement {
                id: "update".into(),
                behavior: "The requested file is updated".into(),
                origin: Origin::User(self.task.request.clone()),
                checks: vec!["acceptance".into()],
                depends_on: vec![],
            }],
            checks: [(
                "acceptance".into(),
                CheckDefinition {
                    purpose: "check file content".into(),
                    kind: CheckKind::Static,
                    verifier: self.invocation.input,
                    command: vec!["check-file".into()],
                    timeout_ms: 1000,
                    minimum_assertions: 1,
                    control: ControlRequirement::BaselineFailure,
                    control_source: None,
                    baseline: BaselinePolicy::MustPass,
                    flake: FlakePolicy::RejectAnyFailure,
                },
            )]
            .into(),
            protected_behavior: vec![],
            assumptions: vec![],
            open_questions,
            delivery: DeliveryKind::Source,
            limits: self.task.initial_limits,
        };
        self.task = self
            .store
            .admit_contract(
                self.task.id,
                self.task.revision,
                contract,
                "user request".into(),
                self.invocation.input,
            )
            .unwrap();
    }

    fn execute_and_reload(mut self) -> Self {
        let initial_revision = self.task.revision;
        let generation = self.task.generation;
        self.task = self
            .store
            .invalidate_candidate(self.task.id, initial_revision, "workspace write".into())
            .unwrap();
        assert_eq!(self.task.generation, generation + 1);
        assert!(matches!(
            self.store.start_execution_job(
                self.task.id,
                initial_revision,
                true,
                1000,
                self.invocation.clone()
            ),
            Err(StoreError::Revision { .. })
        ));
        let (task, job) = self
            .store
            .start_execution_job(
                self.task.id,
                self.task.revision,
                true,
                1000,
                self.invocation.clone(),
            )
            .unwrap();
        assert_eq!(task.revision, initial_revision + 2);
        assert_eq!(task.jobs[&job].generation, task.generation);
        assert!(
            self.store
                .settle_job(task.id, job, JobStatus::Succeeded)
                .is_err()
        );
        self.task = self
            .store
            .settle_execution_job(task.id, job, JobStatus::Succeeded, self.invocation.input)
            .unwrap();
        assert_eq!(self.task.revision, initial_revision + 3);
        assert_eq!(
            self.task.jobs[&job].execution_receipt,
            Some(self.invocation.input)
        );
        assert!(
            self.store
                .begin_check(task.id, self.task.revision, "acceptance")
                .is_err()
        );
        assert!(self.store.complete(task.id, self.task.revision).is_err());
        drop(self.store);
        self.store = Store::open(self.directory.path()).unwrap();
        assert_eq!(self.store.load(task.id).unwrap(), self.task);
        self
    }
}

#[test]
fn workspace_execution_before_contract_admission_preserves_journal_and_receipts() {
    let mut fixture = Fixture::new();
    assert!(fixture.task.contract.is_none());
    fixture = fixture.execute_and_reload();
    assert!(fixture.task.contract.is_none());
}

#[test]
fn workspace_execution_during_pending_followup_does_not_admit_the_contract() {
    let mut fixture = Fixture::new();
    fixture.admit_contract(vec![]);
    let prepared = input::prepare(
        vec![json!({"type":"input_text","text":"Also update the documentation"})],
        fixture.store.public_artifacts(),
    )
    .unwrap();
    fixture
        .store
        .submit(
            fixture.invocation.session,
            Uuid::new_v4(),
            prepared.artifact,
            WorkIntent::Continue {
                task: fixture.task.id,
                scope_revision: fixture.task.scope_revision,
                schedule: Schedule::Steer,
            },
        )
        .unwrap();
    fixture.task = fixture.store.load(fixture.task.id).unwrap();
    assert!(fixture.task.amendment_pending);
    let scope_revision = fixture.task.scope_revision;
    fixture = fixture.execute_and_reload();
    assert!(fixture.task.amendment_pending);
    assert_eq!(fixture.task.scope_revision, scope_revision);
}

#[test]
fn early_workspace_execution_still_requires_the_active_request_owner() {
    let mut fixture = Fixture::new();
    fixture.invocation.request = Uuid::new_v4();
    assert!(matches!(
        fixture.store.start_execution_job(
            fixture.task.id,
            fixture.task.revision,
            true,
            1000,
            fixture.invocation
        ),
        Err(StoreError::Invalid(
            "execution actor does not own this active request"
        ))
    ));
    assert_eq!(fixture.store.load(fixture.task.id).unwrap(), fixture.task);
}

#[test]
fn workspace_execution_with_open_questions_does_not_resolve_them() {
    let mut fixture = Fixture::new();
    fixture.admit_contract(vec!["Which output format should be used?".into()]);
    let contract = fixture.task.contract.clone();
    fixture = fixture.execute_and_reload();
    assert_eq!(fixture.task.contract, contract);
}
