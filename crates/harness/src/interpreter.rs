//! Session-local computation. This runtime owns no filesystem, network or tool authority.
use crate::{Digest, session::SessionId, state::TaskId};
use rquickjs::{
    Atom, Context, Ctx, Function, Object, Promise, Runtime, context::intrinsic, object::Filter,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc as async_mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const BOOTSTRAP: &str = include_str!("interpreter/bootstrap.js");
const MAX_VALUE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn source_identity() -> Digest {
    Digest::of_value(&(
        "orvek-interpreter-v1",
        "rquickjs-0.12.1",
        BOOTSTRAP,
        include_str!("interpreter.rs"),
    ))
    .expect("static identity")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub version: u32,
    pub session: SessionId,
    pub runtime: Digest,
    pub source: Digest,
    pub values: Value,
    pub digest: Digest,
}
impl Checkpoint {
    pub(crate) fn new(session: SessionId, source: Digest, values: Value) -> Self {
        let runtime = source_identity();
        let digest =
            Digest::of_value(&(1, session, runtime, source, &values)).expect("JSON checkpoint");
        Self {
            version: 1,
            session,
            runtime,
            source,
            values,
            digest,
        }
    }
    pub(crate) fn validate(&self, session: SessionId, source: Digest) -> Result<(), String> {
        let expected = Self::new(session, source, self.values.clone());
        if self != &expected {
            return Err("checkpoint version, session, runtime, source or digest mismatch".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRef {
    pub artifact: Digest,
    pub source: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InterpreterEvent {
    Started {
        cell: Uuid,
        task: TaskId,
        outer_call: String,
        source: Digest,
        runtime: Digest,
        generation: u64,
        environment: Digest,
    },
    CallStarted {
        cell: Uuid,
        ordinal: u64,
        call_id: String,
        name: String,
        arguments: Digest,
    },
    CallSettled {
        cell: Uuid,
        ordinal: u64,
        result: Digest,
    },
    Settled {
        cell: Uuid,
        result: Digest,
        checkpoint: Option<CheckpointRef>,
        state_lost: bool,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InterpreterState {
    pub checkpoint: Option<CheckpointRef>,
    pub cells: BTreeMap<Uuid, CellState>,
    pub pending_calls: BTreeMap<String, Uuid>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CellState {
    pub request: Uuid,
    pub task: TaskId,
    pub status: CellStatus,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellStatus {
    Pending,
    Settled { result: Digest },
    Interrupted,
}
impl CellStatus {
    pub(crate) fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}
impl InterpreterState {
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.checkpoint.is_none()
    }
    pub(crate) fn interrupt(&mut self, request: Uuid) {
        for cell in self
            .cells
            .values_mut()
            .filter(|cell| cell.request == request && cell.status.pending())
        {
            cell.status = CellStatus::Interrupted;
        }
    }
    pub(crate) fn admits(&self, call_id: &str, request: Uuid, task: TaskId) -> bool {
        self.pending_calls
            .get(call_id)
            .and_then(|cell| self.cells.get(cell))
            .is_some_and(|cell| {
                cell.request == request && cell.task == task && cell.status.pending()
            })
    }
    pub(crate) fn apply(&mut self, request: Uuid, event: &InterpreterEvent) {
        match event {
            InterpreterEvent::Started { cell, task, .. } => {
                self.cells.insert(
                    *cell,
                    CellState {
                        request,
                        task: *task,
                        status: CellStatus::Pending,
                    },
                );
            }
            InterpreterEvent::Settled {
                cell,
                result,
                checkpoint,
                ..
            } => {
                if let Some(state) = self.cells.get_mut(cell) {
                    state.status = CellStatus::Settled { result: *result };
                }
                self.pending_calls.retain(|_, owner| owner != cell);
                if checkpoint.is_some() {
                    self.checkpoint = checkpoint.clone();
                }
            }
            InterpreterEvent::CallStarted { cell, call_id, .. } => {
                self.pending_calls.insert(call_id.clone(), *cell);
            }
            InterpreterEvent::CallSettled { cell, ordinal, .. } => {
                self.pending_calls.remove(&format!("{cell}/{ordinal}"));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct VmLimits {
    pub memory: usize,
    pub cpu: Duration,
    pub ticks: u64,
}
impl Default for VmLimits {
    fn default() -> Self {
        Self {
            memory: 32 * 1024 * 1024,
            cpu: Duration::from_secs(1),
            ticks: 1_000_000,
        }
    }
}

pub(crate) struct HostCall {
    pub ordinal: u64,
    pub name: String,
    pub arguments: Value,
    pub reply: mpsc::SyncSender<Result<Value, String>>,
}
pub(crate) struct CellOutput {
    pub value: Value,
    pub checkpoint: Option<Value>,
}
pub(crate) struct CellRun {
    pub calls: async_mpsc::UnboundedReceiver<HostCall>,
    pub done: oneshot::Receiver<Result<CellOutput, String>>,
}
struct Cell {
    code: String,
    cancellation: CancellationToken,
    calls: async_mpsc::UnboundedSender<HostCall>,
    done: oneshot::Sender<Result<CellOutput, String>>,
}

/// The channel is all that crosses threads. QuickJS values never leave their owning actor.
pub(crate) struct Interpreter {
    sender: mpsc::SyncSender<Cell>,
}
impl Interpreter {
    pub(crate) async fn start(restore: Option<Value>, limits: VmLimits) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (ready_tx, ready) = oneshot::channel();
        std::thread::Builder::new()
            .name("orvek-interpreter".into())
            .spawn(move || {
                actor(receiver, restore, limits, ready_tx);
            })
            .map_err(|error| error.to_string())?;
        ready
            .await
            .map_err(|_| "interpreter initialization stopped".to_owned())??;
        Ok(Self { sender })
    }
    pub(crate) fn start_cell(
        &self,
        code: String,
        cancellation: CancellationToken,
    ) -> Result<CellRun, String> {
        let (calls_tx, calls) = async_mpsc::unbounded_channel();
        let (done_tx, done) = oneshot::channel();
        self.sender
            .try_send(Cell {
                code,
                cancellation,
                calls: calls_tx,
                done: done_tx,
            })
            .map_err(|_| "interpreter unavailable or already running".to_owned())?;
        Ok(CellRun { calls, done })
    }
}

struct Budget {
    cancellation: CancellationToken,
    elapsed: Duration,
    started: Instant,
    ticks: u64,
}
struct Pending<'js> {
    resolve: Function<'js>,
    reject: Function<'js>,
    reply: mpsc::Receiver<Result<Value, String>>,
}
struct Active {
    calls: async_mpsc::UnboundedSender<HostCall>,
    ordinal: u64,
    checkpoint: Option<Value>,
}

fn actor(
    receiver: mpsc::Receiver<Cell>,
    restore: Option<Value>,
    limits: VmLimits,
    ready: oneshot::Sender<Result<(), String>>,
) {
    let initialized = (|| {
        let runtime = Runtime::new().map_err(|e| e.to_string())?;
        runtime.set_memory_limit(limits.memory);
        if runtime.memory_usage().malloc_limit != limits.memory as i64 {
            return Err("QuickJS allocator does not enforce memory limit".to_owned());
        }
        runtime.set_max_stack_size(256 * 1024);
        let context = Context::builder()
            .with::<intrinsic::Eval>()
            .with::<intrinsic::Json>()
            .with::<intrinsic::Promise>()
            .with::<intrinsic::MapSet>()
            .build(&runtime)
            .map_err(|e| e.to_string())?;
        Ok((runtime, context))
    })();
    let (runtime, context) = match initialized {
        Ok(pair) => pair,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let budget: Arc<Mutex<Option<Budget>>> = Arc::new(Mutex::new(None));
    let hook = budget.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let mut guard = hook.lock().expect("VM budget");
        let Some(budget) = guard.as_mut() else {
            return false;
        };
        budget.ticks += 1;
        budget.cancellation.is_cancelled()
            || budget.ticks > limits.ticks
            || budget.elapsed + budget.started.elapsed() >= limits.cpu
    })));
    context.with(|ctx| actor_context(ctx, receiver, restore, limits, ready, budget));
}

fn actor_context<'js>(
    ctx: Ctx<'js>,
    receiver: mpsc::Receiver<Cell>,
    restore: Option<Value>,
    limits: VmLimits,
    ready: oneshot::Sender<Result<(), String>>,
    budget: Arc<Mutex<Option<Budget>>>,
) {
    let active: Rc<RefCell<Option<Active>>> = Rc::new(RefCell::new(None));
    let pending = Rc::new(RefCell::new(VecDeque::new()));
    let call_active = active.clone();
    let call_pending = pending.clone();
    let call = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, name: String, arguments: String| {
            if arguments.len() > MAX_VALUE_BYTES || call_pending.borrow().len() >= 32 {
                return Err(rquickjs::Exception::throw_message(
                    &ctx,
                    "host arguments or pending-call window exceeds limit",
                ));
            }
            let arguments: Value = serde_json::from_str(&arguments)
                .map_err(|_| rquickjs::Exception::throw_message(&ctx, "invalid JSON arguments"))?;
            let mut active = call_active.borrow_mut();
            let active = active
                .as_mut()
                .ok_or_else(|| rquickjs::Exception::throw_message(&ctx, "no active cell"))?;
            let (promise, resolve, reject) = Promise::new(&ctx)?;
            let (reply, received) = mpsc::sync_channel(1);
            active.ordinal += 1;
            active
                .calls
                .send(HostCall {
                    ordinal: active.ordinal,
                    name,
                    arguments,
                    reply,
                })
                .map_err(|_| rquickjs::Exception::throw_message(&ctx, "host call receiver lost"))?;
            call_pending.borrow_mut().push_back(Pending {
                resolve,
                reject,
                reply: received,
            });
            Ok(promise)
        },
    );
    let save_active = active.clone();
    let save = Function::new(ctx.clone(), move |ctx: Ctx<'_>, encoded: String| {
        if encoded.len() > MAX_VALUE_BYTES {
            return Err(rquickjs::Exception::throw_message(
                &ctx,
                "checkpoint exceeds limit",
            ));
        }
        let value = serde_json::from_str(&encoded)
            .map_err(|_| rquickjs::Exception::throw_message(&ctx, "invalid checkpoint JSON"))?;
        let mut active = save_active.borrow_mut();
        let active = active
            .as_mut()
            .ok_or_else(|| rquickjs::Exception::throw_message(&ctx, "no active cell"))?;
        active.checkpoint = Some(value);
        Ok(())
    });
    let setup = (|| -> rquickjs::Result<Function<'_>> {
        let boot: Function = ctx.eval(BOOTSTRAP)?;
        let object = Object::new(ctx.clone())?;
        // SAFETY: these values are live objects in this actor's locked context.
        // Compare native classes, not mutable prototypes: a Promise or generator
        // with Object.prototype is still a live object, never checkpoint data.
        let plain_class = unsafe { rquickjs::qjs::JS_GetClassID(object.as_value().as_raw()) };
        let data_object = Function::new(ctx.clone(), move |value: rquickjs::Value<'js>| {
            let Some(object) = value.as_object() else {
                return false;
            };
            object
                .own_keys::<Atom>(Filter::new().private())
                .next()
                .is_none()
                && (value.is_array()
                // SAFETY: as_object above guards the QuickJS object-only API.
                || unsafe { rquickjs::qjs::JS_GetClassID(value.as_raw()) } == plain_class)
        })?;
        boot.call((
            call?,
            save?,
            restore.unwrap_or(Value::Null).to_string(),
            data_object,
        ))
    })();
    let run = match setup {
        Ok(run) => run,
        Err(error) => {
            let _ = ready.send(Err(js_error(&ctx, error)));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    while let Ok(cell) = receiver.recv() {
        *budget.lock().expect("VM budget") = Some(Budget {
            cancellation: cell.cancellation.clone(),
            elapsed: Duration::ZERO,
            started: Instant::now(),
            ticks: 0,
        });
        *active.borrow_mut() = Some(Active {
            calls: cell.calls,
            ordinal: 0,
            checkpoint: None,
        });
        let evaluated = (|| -> Result<Value, String> {
            if cell.cancellation.is_cancelled() {
                return Err("cell cancelled".into());
            }
            let promise: Promise = run.call((cell.code,)).map_err(|e| js_error(&ctx, e))?;
            loop {
                // Drain jobs even after the main promise resolves: initiated calls cannot
                // leak into a later cell with a different request's authority.
                while ctx.execute_pending_job() {
                    if cell.cancellation.is_cancelled() {
                        return Err("cell cancelled".into());
                    }
                    let guard = budget.lock().expect("VM budget");
                    let current = guard.as_ref().expect("active budget");
                    if current.elapsed + current.started.elapsed() >= limits.cpu {
                        return Err("VM CPU budget exceeded".into());
                    }
                }
                let next = pending.borrow_mut().pop_front();
                if let Some(next) = next {
                    {
                        let mut guard = budget.lock().expect("VM budget");
                        let current = guard.as_mut().expect("active budget");
                        current.elapsed += current.started.elapsed();
                    }
                    let reply = next.reply.recv().map_err(|_| {
                        "host call interrupted; effects may be unresolved".to_owned()
                    })?;
                    budget
                        .lock()
                        .expect("VM budget")
                        .as_mut()
                        .expect("active budget")
                        .started = Instant::now();
                    if cell.cancellation.is_cancelled() {
                        return Err("cell cancelled; inspect inner receipts".into());
                    }
                    match reply {
                        Ok(value) => next.resolve.call::<_, ()>((value.to_string(),)),
                        Err(error) => next.reject.call::<_, ()>((error,)),
                    }
                    .map_err(|e| js_error(&ctx, e))?;
                    continue;
                }
                let encoded = promise
                    .result::<String>()
                    .ok_or_else(|| {
                        "cell has an unresolved promise without a host operation".to_owned()
                    })?
                    .map_err(|e| js_error(&ctx, e))?;
                if encoded.len() > MAX_VALUE_BYTES {
                    return Err("cell result exceeds limit".into());
                }
                return serde_json::from_str(&encoded).map_err(|e| e.to_string());
            }
        })();
        let checkpoint = active
            .borrow_mut()
            .take()
            .and_then(|active| active.checkpoint);
        *budget.lock().expect("VM budget") = None;
        let failed = evaluated.is_err();
        let _ = cell
            .done
            .send(evaluated.map(|value| CellOutput { value, checkpoint }));
        if failed {
            cell.cancellation.cancel();
            pending.borrow_mut().clear();
            break;
        }
    }
}

fn js_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if error.is_exception() {
        let value = ctx.catch();
        if let Some(exception) = value.as_exception() {
            return exception
                .message()
                .unwrap_or_else(|| "JavaScript exception".into());
        }
        if let Some(message) = value.as_string() {
            return message
                .to_string()
                .unwrap_or_else(|_| "JavaScript string exception".into());
        }
        return "JavaScript exception (non-Error value)".into();
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::timeout;

    async fn evaluate(actor: &Interpreter, code: &str) -> Result<CellOutput, String> {
        let mut run = actor.start_cell(code.into(), CancellationToken::new())?;
        assert!(run.calls.recv().await.is_none(), "unexpected host call");
        timeout(Duration::from_secs(3), run.done)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn interpreter_retains_values_and_isolates_sessions_without_ambient_authority() {
        let first = Interpreter::start(None, VmLimits::default()).await.unwrap();
        let second = Interpreter::start(None, VmLimits::default()).await.unwrap();
        evaluate(&first, "globalThis.rows = Array.from({length:10000}, (_,id) => ({id, text:'row-'+id})); return rows.length;").await.unwrap();
        assert_eq!(
            evaluate(
                &first,
                "return rows.filter(r => r.id % 2000 === 0).map(r => r.id);"
            )
            .await
            .unwrap()
            .value,
            json!([0, 2000, 4000, 6000, 8000])
        );
        assert_eq!(
            evaluate(&second, "return typeof rows;")
                .await
                .unwrap()
                .value,
            json!("undefined")
        );
        let kinds = evaluate(&first, "return ['fetch','require','process','std','os','Atomics','SharedArrayBuffer','Uint8Array','Worker'].map(k => typeof globalThis[k]);").await.unwrap().value;
        assert_eq!(kinds, json!(vec!["undefined"; 9]));
    }

    #[tokio::test]
    async fn interpreter_checkpoint_rejects_lossy_values_and_live_objects() {
        for value in [
            "{f:()=>1}",
            "{p:Promise.resolve(1)}",
            "{n:NaN}",
            "{u:undefined}",
            "new Map()",
            "Object.setPrototypeOf(Promise.resolve(1), null)",
            "Object.setPrototypeOf(new Map(), null)",
            "Object.setPrototypeOf((function*(){yield 1})(), null)",
            "Object.setPrototypeOf(new Error('live'), null)",
            "Object.setPrototypeOf(new (class {#value=1})(), null)",
            "[1,,3]",
            "Object.create({x:1})",
            "{get x(){return 1}}",
            "(()=>{const x={};x.x=x;return x})()",
        ] {
            let actor = Interpreter::start(None, VmLimits::default()).await.unwrap();
            let result = evaluate(&actor, &format!("host.checkpoint({value}); return true;")).await;
            assert!(result.is_err(), "accepted {value}");
        }
        let actor = Interpreter::start(None, VmLimits::default()).await.unwrap();
        let saved = evaluate(
            &actor,
            "host.checkpoint({rows:[1,2], text:'kept'}); return 3;",
        )
        .await
        .unwrap()
        .checkpoint
        .unwrap();
        let restored = Interpreter::start(Some(saved.clone()), VmLimits::default())
            .await
            .unwrap();
        assert_eq!(
            evaluate(&restored, "return restored;").await.unwrap().value,
            saved
        );
    }

    #[test]
    fn interpreter_checkpoint_detects_tampering_and_wrong_identity() {
        let session = SessionId::new();
        let source = Digest::of(b"cell source");
        let checkpoint = Checkpoint::new(session, source, json!({"value":42}));
        assert!(checkpoint.validate(session, source).is_ok());
        assert!(checkpoint.validate(SessionId::new(), source).is_err());
        assert!(
            checkpoint
                .validate(session, Digest::of(b"different source"))
                .is_err()
        );
        for field in ["version", "runtime", "digest", "values"] {
            let mut altered = checkpoint.clone();
            match field {
                "version" => altered.version += 1,
                "runtime" => altered.runtime = Digest::of(b"other"),
                "digest" => altered.digest = Digest::of(b"other"),
                _ => altered.values = json!(43),
            }
            assert!(altered.validate(session, source).is_err());
        }
    }

    #[tokio::test]
    async fn interpreter_bounds_cpu_memory_and_cross_thread_cancellation() {
        let limits = VmLimits {
            memory: 8 * 1024 * 1024,
            cpu: Duration::from_millis(30),
            ticks: 10000,
        };
        let actor = Interpreter::start(None, limits).await.unwrap();
        assert!(
            evaluate(
                &actor,
                "while(true) {try {for(let i=0;i<10000;i++) {}} catch (_) {}} "
            )
            .await
            .is_err()
        );
        let actor = Interpreter::start(None, limits).await.unwrap();
        assert!(
            evaluate(&actor, "return 'x'.repeat(32*1024*1024);")
                .await
                .is_err()
        );
        let actor = Interpreter::start(
            None,
            VmLimits {
                cpu: Duration::from_secs(30),
                ticks: u64::MAX,
                ..limits
            },
        )
        .await
        .unwrap();
        let cancellation = CancellationToken::new();
        let mut run = actor
            .start_cell(
                "host.call('read_file',{}); while(true) {}".into(),
                cancellation.clone(),
            )
            .unwrap();
        let _entered_vm = run.calls.recv().await.unwrap();
        cancellation.cancel();
        assert!(
            timeout(Duration::from_secs(2), run.done)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn interpreter_host_wait_is_pending_without_consuming_vm_budget_and_rebinds_each_cell() {
        let actor = Interpreter::start(
            None,
            VmLimits {
                cpu: Duration::from_millis(30),
                ..VmLimits::default()
            },
        )
        .await
        .unwrap();
        let mut first = actor.start_cell("globalThis.savedCall = host.call; return await savedCall('read_file',{path:'first'});".into(),CancellationToken::new()).unwrap();
        let call = first.calls.recv().await.unwrap();
        assert_eq!(call.ordinal, 1);
        assert_eq!(call.arguments, json!({"path":"first"}));
        // A pending host operation lasts ten times the VM budget. Tokio remains responsive.
        assert!(
            timeout(Duration::from_millis(300), &mut first.done)
                .await
                .is_err()
        );
        call.reply.send(Ok(json!({"request":"first"}))).unwrap();
        assert_eq!(
            first.done.await.unwrap().unwrap().value,
            json!({"request":"first"})
        );
        let mut second = actor
            .start_cell(
                "return await savedCall('read_file',{path:'second'});".into(),
                CancellationToken::new(),
            )
            .unwrap();
        let call = second.calls.recv().await.unwrap();
        assert_eq!(call.ordinal, 1);
        assert_eq!(call.arguments, json!({"path":"second"}));
        call.reply.send(Ok(json!({"request":"second"}))).unwrap();
        assert_eq!(
            second.done.await.unwrap().unwrap().value,
            json!({"request":"second"})
        );
    }
}
