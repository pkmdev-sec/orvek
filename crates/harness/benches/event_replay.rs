use criterion::{Criterion, black_box, criterion_group, criterion_main};
use orvek_harness::{
    context::DEFAULT_WINDOW_TOKENS,
    inference::ModelSettings,
    session::{SessionCommand, SessionConfig, SessionCursor, SessionId},
    store::Store,
};
use tempfile::TempDir;
use uuid::Uuid;

const EVENT_COUNT: u64 = 4_096;

struct LongSession {
    _state: TempDir,
    _workspace: TempDir,
    store: Store,
    id: SessionId,
}

fn long_session() -> LongSession {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut store = Store::open(state.path()).unwrap();
    let id = SessionId::new();
    let config = SessionConfig {
        workspace: workspace.path().to_owned(),
        model: ModelSettings::default(),
        instructions: "long event stream benchmark".into(),
        context_window_tokens: DEFAULT_WINDOW_TOKENS,
    };
    let mut session = store.create_session(id, config, None).unwrap();
    for index in 0..EVENT_COUNT {
        session = store
            .session_command(
                id,
                session.revision,
                Uuid::new_v4(),
                SessionCommand::Feedback {
                    message: format!("representative event {index}"),
                },
            )
            .unwrap();
    }
    LongSession {
        _state: state,
        _workspace: workspace,
        store,
        id,
    }
}

fn event_replay(criterion: &mut Criterion) {
    let fixture = long_session();
    criterion.bench_function("load_session_4096_events", |bencher| {
        bencher.iter(|| black_box(fixture.store.load_session(fixture.id).unwrap()));
    });
    criterion.bench_function("load_historical_session_4000_events", |bencher| {
        bencher.iter(|| {
            black_box(
                fixture
                    .store
                    .load_session_cursor(&SessionCursor {
                        version: 1,
                        session: fixture.id,
                        revision: 4_000,
                    })
                    .unwrap(),
            )
        });
    });
}

criterion_group!(benches, event_replay);
criterion_main!(benches);
