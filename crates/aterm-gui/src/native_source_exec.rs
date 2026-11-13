// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native custody before a source commits its runtime. Preparation retains the
//! original signed build lease, pending SDK constructor, input position and data
//! reservation. It publishes no command or Go; execution remains unavailable.
use super::*;
use aterm_agent::harness::codex_source_registry::SourceExecReservation;
use std::sync::{Weak, mpsc};

const REFUSED: &str = "ERR native source-exec unavailable\n";
static WORKERS: std::sync::LazyLock<Arc<crate::budget::Slots>> =
    std::sync::LazyLock::new(|| Arc::new(crate::budget::Slots::new(64)));

#[derive(Clone, Debug, PartialEq, Eq)]
struct Recipe {
    operation: String,
    build: u64,
    primary: String,
}

#[derive(Debug)]
pub(super) struct Operation {
    recipe: Recipe,
    source: Process,
    generation: i32,
    authority: SupervisorAuthority,
    human_seq: u64,
    deadline: u64,
    cancelled: std::sync::atomic::AtomicBool,
    state: Mutex<State>,
    wake: std::sync::Condvar,
}

#[derive(Debug)]
struct State {
    phase: Phase,
    custody: Option<Arc<NativeCustody>>,
    data: Option<runtime_data::Pin>,
}
#[derive(Debug)]
enum Phase {
    Preparing,
    Prepared(Box<Payload>),
    Failed,
    Closed,
}
struct Payload {
    _target: atpkg::prerequisite::HeldTool,
    constructor: Option<SourceExecReservation>,
    registry: Arc<aterm_agent::harness::codex_source_registry::Registry>,
}
impl std::fmt::Debug for Payload {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("NativeSourceTarget(<private>)")
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        drop(self.constructor.take());
        self.registry.reap_gone();
    }
}

impl Operation {
    fn source_current(&self) -> bool {
        Process::observe(self.source.pid).as_ref() == Some(&self.source)
            && generation(self.source.pid) == Some(self.generation)
    }
    fn current(&self, ctx: &crate::SessionCtx, authority: &SupervisorAuthority) -> bool {
        *authority == self.authority
            && !self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && ctx.fabric.hold().is_none()
            && ctx.human_input.seq() == self.human_seq
            && remaining(self.deadline).is_some()
    }
    fn status(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (phase, build) = match &state.phase {
            Phase::Preparing => ("preparing", self.recipe.build),
            Phase::Prepared(payload) => ("prepared", payload._target.build()),
            Phase::Failed => ("failed", self.recipe.build),
            Phase::Closed => ("closed", self.recipe.build),
        };
        let runtime = match &state.data {
            None => "unbound",
            Some(data) if data.retained().is_some() => "retained",
            Some(_) => "reserved",
        };
        format!(
            "OK native source-exec state={phase} operation={} build={} runtime={} execution=unavailable\n",
            self.recipe.operation, build, runtime,
        )
    }
    fn close(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.finish(Phase::Closed);
    }
    fn finish(&self, phase: Phase) {
        let old = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(state.phase, Phase::Closed) {
                return;
            }
            let phase = if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                Phase::Closed
            } else {
                phase
            };
            // Every resource is disposed after the state lock, including SDK
            // constructor revocation and the original package/input leases.
            (
                std::mem::replace(&mut state.phase, phase),
                state.custody.take(),
                state.data.take(),
            )
        };
        drop(old);
        self.wake.notify_all();
    }
}

fn generation(pid: u32) -> Option<i32> {
    aterm_uds::process::read_identity(i32::try_from(pid).ok()?)?.idversion
}
fn hex(value: &str) -> bool {
    value.len() == 32 && aterm_codec::hex::is_lower(value, 32)
}
fn primary(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && value
            .bytes()
            .any(|b| matches!(b, b'1'..=b'9' | b'a'..=b'f'))
}
fn fields(line: &str) -> Option<(&str, BTreeMap<&str, &str>)> {
    let mut words = line.split_ascii_whitespace();
    let verb = words.next()?;
    let mut fields = BTreeMap::new();
    for word in words {
        let (name, value) = word.split_once('=')?;
        if value.is_empty() || fields.insert(name, value).is_some() {
            return None;
        }
    }
    Some((verb, fields))
}
fn recipe(fields: &BTreeMap<&str, &str>) -> Option<Recipe> {
    if fields.len() != 3 {
        return None;
    }
    let operation = *fields.get("operation")?;
    let primary = *fields.get("primary")?;
    let build = fields.get("build")?.parse::<u64>().ok()?;
    (hex(operation)
        && self::primary(primary)
        && build != 0
        && build.to_string() == *fields.get("build")?)
    .then(|| Recipe {
        operation: operation.into(),
        build,
        primary: primary.into(),
    })
}

/// Use the existing native operation channel and its action classification.
/// `prepare source-exec` inherits prepare's connection/EOF tracking. Only
/// `info source-exec` is read-only; query and cancellation retain native-act's
/// normal authorization, while the worker independently closes on a hold.
pub(super) fn route(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let action = words.next()?;
    if words.next() != Some("source-exec") {
        return None;
    }
    let action = match action {
        "info" | "prepare" | "query" => action,
        "cancel-prepare" => "abort",
        _ => return Some(REFUSED.into()),
    };
    let mut request = action.to_owned();
    for word in words {
        request.push(' ');
        request.push_str(word);
    }
    Some(serve(ctx, master, &request))
}

fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    if line == "info" {
        return format!(
            "OK native source-exec preparation={} execution=unavailable\n",
            u8::from(cfg!(target_os = "macos"))
        );
    }
    let Some((verb, fields)) = fields(line) else {
        return REFUSED.into();
    };
    if verb == "prepare" {
        return recipe(&fields)
            .map_or_else(|| REFUSED.into(), |recipe| prepare(ctx, master, recipe));
    }
    if !matches!(verb, "query" | "abort") || fields.len() != 1 {
        return REFUSED.into();
    }
    let Some(operation) = fields.get("operation").filter(|value| hex(value)) else {
        return REFUSED.into();
    };
    let Some(peer) = crate::control::serving_peer_pid().and_then(Process::observe) else {
        return REFUSED.into();
    };
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    let Some(op) = op
        .filter(|op| op.recipe.operation == *operation && op.source == peer && op.source_current())
    else {
        return REFUSED.into();
    };
    if verb == "abort" {
        op.close();
    }
    op.status()
}

fn prepare(ctx: &Arc<crate::SessionCtx>, master: i32, recipe: Recipe) -> String {
    if !cfg!(target_os = "macos")
        || crate::control::serving_principal() == crate::control::Principal::Harness
    {
        return REFUSED.into();
    }
    let Some(pid) = crate::control::serving_peer_pid() else {
        return REFUSED.into();
    };
    let Some(proof) =
        crate::control::prove_self_claim_for(ctx, &format!("codex-alab@{pid}"), master)
    else {
        return REFUSED.into();
    };
    let (Some(source), Some(generation)) = (Process::observe(pid), generation(pid)) else {
        return REFUSED.into();
    };
    let Some(args) = atpkg::caller_shell::process_args(pid) else {
        return REFUSED.into();
    };
    let Some(cwd) = aterm_sysprobe::process_cwd(pid).and_then(|p| p.to_str().map(str::to_owned))
    else {
        return REFUSED.into();
    };
    let Ok(env) = codex_account_env() else {
        return REFUSED.into();
    };
    let Some(registry) = crate::control::source_registry() else {
        return REFUSED.into();
    };
    let Some(worker) = Worker::reserve() else {
        return REFUSED.into();
    };
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    if authority.contract() != Some(2)
        || authority.identity() != Some(&proof)
        || authority.connection() != crate::control::serving_connection()
        || authority
            .scopes()
            .is_none_or(|scopes| !scopes.contains(Scope::Upgrade))
        || ctx.fabric.hold().is_some()
        || Process::observe(pid).as_ref() != Some(&source)
        || self::generation(pid) != Some(generation)
    {
        return REFUSED.into();
    }
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(old) = &table.source_exec {
        if old.recipe.operation == recipe.operation {
            return if old.recipe == recipe && old.source == source && old.generation == generation {
                old.status()
            } else {
                REFUSED.into()
            };
        }
        if old
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .custody
            .is_some()
        {
            return REFUSED.into();
        }
    }
    if table
        .runtime_data
        .as_ref()
        .is_none_or(|slot| slot.reusable_by(&recipe.operation, &source))
    {
        table.runtime_data = Some(runtime_data::Slot::reserved(
            recipe.operation.clone(),
            source.clone(),
        ));
    }
    let Some(data) = table
        .runtime_data
        .as_ref()
        .and_then(|slot| slot.pin(&recipe.operation, &source))
    else {
        return REFUSED.into();
    };
    let Some(boundary) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return REFUSED.into();
    };
    let Ok(custody) =
        ctx.fenced_writes
            .reserve_native(authority.revision(), &recipe.operation, boundary)
    else {
        return REFUSED.into();
    };
    let Some(deadline) = aterm_agent::harness::native_act::monotonic_us()
        .and_then(|now| now.checked_add(120_000_000))
    else {
        return REFUSED.into();
    };
    let op = Arc::new(Operation {
        recipe,
        source,
        generation,
        authority,
        human_seq: ctx.human_input.seq(),
        deadline,
        cancelled: std::sync::atomic::AtomicBool::new(false),
        state: Mutex::new(State {
            phase: Phase::Preparing,
            custody: Some(Arc::new(custody)),
            data: Some(data),
        }),
        wake: std::sync::Condvar::new(),
    });
    let previous = table.source_exec.replace(op.clone());
    drop(table);
    drop(meta);
    drop(previous);
    worker.start(Job {
        ctx: Arc::downgrade(ctx),
        op: op.clone(),
        env,
        registry,
        args,
        cwd,
    });
    op.status()
}

struct Job {
    ctx: Weak<crate::SessionCtx>,
    op: Arc<Operation>,
    env: aterm_agent::harness::cli::Env,
    registry: Arc<aterm_agent::harness::codex_source_registry::Registry>,
    args: atpkg::caller_shell::ProcArgs,
    cwd: String,
}
struct Worker(mpsc::SyncSender<Job>);
impl Worker {
    fn reserve() -> Option<Self> {
        let budget = WORKERS.try_take()?;
        let (send, receive) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("codex-source-prepare".into())
            .spawn(move || {
                let _budget = budget;
                if let Ok(job) = receive.recv() {
                    let op = job.op.clone();
                    guarded(&op, || run(job));
                }
            })
            .ok()?;
        Some(Self(send))
    }
    fn start(self, job: Job) {
        self.0
            .send(job)
            .unwrap_or_else(|_| unreachable!("reserved source worker exited"));
    }
}

fn target(job: &Job, ctx: &crate::SessionCtx) -> Result<Payload, ()> {
    let target = atpkg::prerequisite::configured_tool(
        atpkg::prerequisite::Prerequisite::Codex,
        job.op.recipe.build,
        "native source replacement preparation",
    )
    .map_err(|_| ())?;
    let image = Image::at(target.physical_path()).ok_or(())?;
    if image == job.op.source.image || !job.op.source_current() {
        return Err(());
    }
    let mut env = Vec::new();
    for name in ["HOME", "CODEX_HOME", "CODEX_AUTH_PATH", "CODEX_AUTH_SLOT"] {
        let prefix = format!("{name}=");
        let mut values = job
            .args
            .env
            .iter()
            .filter_map(|word| word.strip_prefix(&prefix));
        if let Some(value) = values.next() {
            env.push((name.into(), value.into()));
        }
        if values.next().is_some() {
            return Err(());
        }
    }
    let command = RestartCommand {
        image,
        program: target.physical_path().to_str().ok_or(())?.into(),
        argv: aterm_agent::harness::codex_alab::rewrite_argv(
            &job.args.argv,
            Some(&job.op.recipe.primary),
        )
        .map_err(|_| ())?,
        env,
        unset: None,
        cwd: Some(job.cwd.clone()),
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
        account_intent: None,
        settles: None,
    };
    let constructor = job
        .registry
        .reserve_source_exec(&job.env, ctx.self_id.as_str(), &command, &job.op.source)
        .map_err(|_| ())?;
    let payload = Payload {
        _target: target,
        constructor: Some(constructor),
        registry: job.registry.clone(),
    };
    payload._target.validate().map_err(|_| ())?;
    Ok(payload)
}

fn guarded(op: &Operation, work: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
        op.close();
    }
}

fn run(job: Job) {
    loop {
        let Some(ctx) = job.ctx.upgrade() else {
            job.op.close();
            return;
        };
        let current = job.op.source_current();
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let valid = current
            && job
                .op
                .current(&ctx, &meta.supervisor_authority(crate::metrics::now_us()));
        drop(meta);
        if !valid {
            job.op.close();
            return;
        }
        let state = job.op.state.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(state.phase, Phase::Closed) {
            return;
        }
        if state
            .custody
            .as_ref()
            .is_some_and(|custody| custody.predecessors_drained())
        {
            break;
        }
        let _parked = job
            .op
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap_or_else(|p| p.into_inner());
    }
    let Some(ctx) = job.ctx.upgrade() else {
        job.op.close();
        return;
    };
    let result = target(&job, &ctx);
    drop(ctx);
    let Ok(payload) = result else {
        job.op.finish(Phase::Failed);
        return;
    };
    let mut result = Some(payload);
    loop {
        let Some(ctx) = job.ctx.upgrade() else {
            job.op.close();
            return;
        };
        let current = job.op.source_current();
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let valid = current
            && job
                .op
                .current(&ctx, &meta.supervisor_authority(crate::metrics::now_us()));
        drop(meta);
        if !valid {
            job.op.close();
            return;
        }
        let mut state = job.op.state.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(state.phase, Phase::Closed) {
            return;
        }
        if let Some(result) = result.take() {
            state.phase = Phase::Prepared(Box::new(result));
        }
        // The worker owns closing even after EOF or an unanswered preparation.
        // Only preparation exists; it can never issue or replay execution.
        let _parked = job
            .op
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap_or_else(|p| p.into_inner());
    }
}

pub(super) fn pending(ctx: &crate::SessionCtx) -> bool {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    op.is_some_and(|op| {
        op.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .custody
            .is_some()
    })
}
pub(super) fn cancel_owner(ctx: &crate::SessionCtx) -> bool {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    let Some(op) = op else { return false };
    let was_pending = op
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .custody
        .is_some();
    op.cancelled
        .store(true, std::sync::atomic::Ordering::Release);
    op.wake.notify_all();
    was_pending
}

#[cfg(test)]
#[path = "native_source_exec_tests.rs"]
mod tests;
