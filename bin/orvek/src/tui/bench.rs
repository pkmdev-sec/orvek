//! Criterion baselines against the actual native terminal projection and renderer.
#![allow(dead_code, unused_imports)]

#[path = "../app/mod.rs"]
mod app;
#[path = "../core/mod.rs"]
mod core;
#[path = "../review/mod.rs"]
mod review;
#[path = "mod.rs"]
mod tui;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use orvek_harness::session::{SessionCursor, SessionId};
use ratatui::{Terminal, backend::TestBackend};
use std::{hint::black_box, path::Path, sync::Arc};
use tui::{
    components::{AppEvent, AppNode, RootNode},
    host_projection::ViewChange,
    pane::PaneId,
    theme::Theme,
    transcript::TranscriptRecord,
};
use uuid::Uuid;

pub(crate) fn install_tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn application() -> AppNode {
    AppNode::new(
        Theme::default(),
        "/fixture".into(),
        RootNode::new(Path::new("/fixture"), app::config::ReasoningEffort::Medium),
    )
}
fn record(index: u64, change: ViewChange) -> Arc<TranscriptRecord> {
    Arc::new(TranscriptRecord::from_host(
        index,
        index,
        SessionCursor {
            version: 1,
            session: SessionId(Uuid::nil()),
            revision: index,
        },
        change,
    ))
}
fn populated() -> AppNode {
    let mut app = application();
    for index in 1..=64 {
        app.update(AppEvent::Transcript {pane:PaneId::Main,record:record(index,ViewChange::Assistant {request:None,item:format!("message-{index}"),text:format!("Message {index}\n\n```rust\nfn example() {{ println!(\"native host\"); }}\n```"),replace:true,confirmed:true})});
    }
    app
}
fn benchmarks(c: &mut Criterion) {
    c.bench_function("native_terminal_first_frame", |b| {
        b.iter_batched(
            || {
                (
                    application(),
                    Terminal::new(TestBackend::new(120, 40)).unwrap(),
                )
            },
            |(mut app, mut terminal)| {
                terminal.draw(|frame| app.render(frame)).unwrap();
                black_box(terminal);
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("native_terminal_history_frame", |b| {
        b.iter_batched(
            || {
                (
                    populated(),
                    Terminal::new(TestBackend::new(120, 40)).unwrap(),
                )
            },
            |(mut app, mut terminal)| {
                terminal.draw(|frame| app.render(frame)).unwrap();
                black_box(terminal);
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("native_terminal_stream_delta", |b| {
        b.iter_batched(
            populated,
            |mut app| {
                app.update(AppEvent::Transcript {
                    pane: PaneId::Main,
                    record: record(
                        65,
                        ViewChange::Assistant {
                            request: None,
                            item: "stream".into(),
                            text: "incremental text".into(),
                            replace: false,
                            confirmed: false,
                        },
                    ),
                });
                black_box(app);
            },
            BatchSize::SmallInput,
        )
    });
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
