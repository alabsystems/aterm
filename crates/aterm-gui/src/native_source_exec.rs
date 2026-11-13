// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native custody for a source replacement. Preparation retains signed original
//! and target builds, a pending SDK constructor, input position and runtime DATA.
//! Possible execution is journaled before constructor entry. Full-runtime
//! discovery remains unavailable until restoration and recovery are integrated.
use super::*;
use aterm_agent::harness::codex_source_registry::SourceExecReservation;
use aterm_agent::harness::source_exec_journal::{Journal, Snapshot as StoredSnapshot};
use std::sync::{Weak, mpsc};

const REFUSED: &str = "ERR native source-exec unavailable\n";
static WORKERS: std::sync::LazyLock<Arc<crate::budget::Slots>> =
    std::sync::LazyLock::new(|| Arc::new(crate::budget::Slots::new(64)));

#[derive(Clone, Debug, PartialEq, Eq)]
struct Recipe {
    operation: String,
    build: u64,
    source_build: u64,
    primary: String,
}

#[derive(Debug)]
pub(super) struct Operation {
    recipe: Recipe,
    source: Process,
    generation: i32,
    authority: SupervisorAuthority,
    human_seq: u64,
    // The proven source owns input preservation from this revocable seal.
    // It does not grant constructor entry, Execute, or exemption from holds.
    input_sealed: std::sync::atomic::AtomicBool,
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
    requested: bool,
    possible: bool,
    granted: bool,
    snapshot: Option<StoredSnapshot>,
    successor: Option<(Process, i32)>,
}
#[derive(Debug)]
enum Phase {
    Preparing,
    Prepared(Arc<Payload>),
    Possible(Arc<Payload>),
    Activated(Activation),
    Failed,
    Closed,
}
#[derive(Debug)]
struct Activation {
    // Retain signed rollback/target images until another operation replaces
    // this receipt; activation ACK loss must not dispose original custody.
    _payload: Arc<Payload>,
    process: Process,
    generation: i32,
    authority: SupervisorAuthority,
    settled: bool,
    adopted: bool,
    disconnected: bool,
}
struct Payload {
    // Rust drops fields in declaration order: revoke and reap the constructor
    // before releasing either signed executable lease.
    constructor: ConstructorOwner,
    _target: atpkg::prerequisite::HeldTool,
    _rollback: atpkg::prerequisite::HeldTool,
    journal: Mutex<Journal>,
    command: String,
}
impl std::fmt::Debug for Payload {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("NativeSourceTarget(<private>)")
    }
}
// Own the pending constructor immediately, before any fallible encoding or IO.
// Revocation precedes reaping even when preparation never reaches Payload.
struct ConstructorOwner {
    reservation: Option<SourceExecReservation>,
    registry: Arc<aterm_agent::harness::codex_source_registry::Registry>,
}
impl Drop for ConstructorOwner {
    fn drop(&mut self) {
        drop(self.reservation.take());
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
            && (self.input_sealed.load(std::sync::atomic::Ordering::Acquire)
                || ctx.human_input.seq() == self.human_seq)
            && remaining(self.deadline).is_some()
    }
    fn seal_input(&self, ctx: &crate::SessionCtx, authority: &SupervisorAuthority) -> bool {
        // The native caller retains Meta. Check policy/process conditions
        // before acquiring operation state, preserving the native/state order.
        if !self.source_current() || !self.current(ctx, authority) {
            return false;
        }
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !matches!(state.phase, Phase::Preparing | Phase::Prepared(_))
            || state.possible
            || self.cancelled.load(std::sync::atomic::Ordering::Acquire)
        {
            return false;
        }
        self.input_sealed
            .store(true, std::sync::atomic::Ordering::Release);
        true
    }

    fn status(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let (phase, build) = match &state.phase {
            Phase::Preparing => ("preparing", self.recipe.build),
            Phase::Prepared(payload) => ("prepared", payload._target.build()),
            Phase::Possible(payload) => ("possible-execution", payload._target.build()),
            Phase::Activated(activation) => (
                if activation.settled {
                    "settled"
                } else {
                    "activated"
                },
                self.recipe.build,
            ),
            Phase::Failed => ("failed", self.recipe.build),
            Phase::Closed => ("closed", self.recipe.build),
        };
        let runtime = match &state.data {
            None => "unbound",
            Some(data) if data.retained().is_some() => "retained",
            Some(_) => "reserved",
        };
        format!(
            "OK native source-exec state={phase} operation={} build={} runtime={} execution=unavailable input={}\n",
            self.recipe.operation,
            build,
            runtime,
            if self.input_sealed.load(std::sync::atomic::Ordering::Acquire) {
                "sealed"
            } else {
                "open"
            },
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
            // A timeout, cancellation, EOF or panic cannot prove NoExec after
            // the first journal attempt. Keep the original DATA and leases.
            if state.possible {
                self.wake.notify_all();
                return;
            }
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
    if fields.len() != 4 {
        return None;
    }
    let operation = *fields.get("operation")?;
    let primary = *fields.get("primary")?;
    let build = fields.get("build")?.parse::<u64>().ok()?;
    let source_build = fields.get("source-build")?.parse::<u64>().ok()?;
    (hex(operation)
        && self::primary(primary)
        && build != 0
        && source_build != 0
        && source_build.to_string() == *fields.get("source-build")?
        && build.to_string() == *fields.get("build")?)
    .then(|| Recipe {
        operation: operation.into(),
        build,
        source_build,
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
        "info" | "prepare" | "seal-input" | "query" | "execute" | "load" | "activate"
        | "settle" | "adopt" => action,
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
            "OK native source-exec preparation={} execution=unavailable input-fence=1\n",
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
    if !matches!(
        verb,
        "seal-input" | "query" | "abort" | "execute" | "load" | "activate" | "settle" | "adopt"
    ) || fields.len() != 1
    {
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
    let Some(op) = op.filter(|op| op.recipe.operation == *operation) else {
        return REFUSED.into();
    };
    if matches!(verb, "load" | "activate" | "settle" | "adopt") {
        return successor(ctx, &op, &peer, verb);
    }
    if op.source != peer || !op.source_current() {
        return REFUSED.into();
    }
    if verb == "seal-input" {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if !op.seal_input(ctx, &meta.supervisor_authority(crate::metrics::now_us())) {
            return REFUSED.into();
        }
        return op.status();
    }
    if verb == "abort" {
        op.close();
    }
    if verb == "execute" {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if !op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us())) {
            return REFUSED.into();
        }
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if !op.input_sealed.load(std::sync::atomic::Ordering::Acquire)
            || !matches!(state.phase, Phase::Prepared(_) | Phase::Possible(_))
            || state
                .data
                .as_ref()
                .and_then(runtime_data::Pin::retained)
                .is_none()
        {
            return REFUSED.into();
        }
        state.requested = true;
        op.wake.notify_all();
    }
    if matches!(verb, "query" | "execute") {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us())) {
            let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.granted
                && let (Phase::Possible(payload), Some(snapshot)) = (&state.phase, state.snapshot)
            {
                return format!(
                    "OK native source-exec state=go operation={} build={} primary={} bytes={} digest={} command={}\n",
                    op.recipe.operation,
                    payload._target.build(),
                    op.recipe.primary,
                    snapshot.bytes,
                    aterm_codec::hex::encode_lower(&snapshot.digest),
                    aterm_codec::hex::encode_lower(payload.command.as_bytes()),
                );
            }
        }
    }
    op.status()
}

/// A successor must enter this operation's original fresh constructor. Runtime
/// bytes, a matching PID or a self-reported build cannot establish admission.
fn successor(ctx: &crate::SessionCtx, op: &Operation, peer: &Process, verb: &str) -> String {
    let Some(generation) = generation(peer.pid).filter(|generation| *generation != op.generation)
    else {
        return REFUSED.into();
    };
    if verb == "settle" {
        return settle(ctx, op, peer, generation);
    }
    if verb == "adopt" {
        return adopt(ctx, op, peer, generation);
    }
    let payload = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Phase::Activated(activation) = &state.phase {
            if !activation_current(
                ctx,
                op,
                peer,
                generation,
                &meta.supervisor_authority(crate::metrics::now_us()),
                activation,
            ) {
                return REFUSED.into();
            }
            activation._payload.clone()
        } else {
            if !op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us()))
                || !state.granted
            {
                return REFUSED.into();
            }
            let Phase::Possible(payload) = &state.phase else {
                return REFUSED.into();
            };
            payload.clone()
        }
    };
    if payload._target.validate().is_err()
        || payload
            .constructor
            .reservation
            .as_ref()
            .is_none_or(|ctor| ctor.entered(peer) != Ok(true))
    {
        return REFUSED.into();
    }
    let snapshot = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Phase::Activated(activation) = &state.phase {
            return if activation_current(
                ctx,
                op,
                peer,
                generation,
                &meta.supervisor_authority(crate::metrics::now_us()),
                activation,
            ) {
                activation_reply(op)
            } else {
                REFUSED.into()
            };
        }
        if !op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us()))
            || state
                .successor
                .as_ref()
                .is_some_and(|saved| saved != &(peer.clone(), generation))
            || Process::observe(peer.pid).as_ref() != Some(peer)
        {
            return REFUSED.into();
        }
        let Some(snapshot) = state.snapshot else {
            return REFUSED.into();
        };
        if verb == "load" {
            if state.data.as_ref().and_then(runtime_data::Pin::retained)
                != Some((
                    snapshot.bytes,
                    aterm_codec::hex::encode_lower(&snapshot.digest),
                ))
            {
                return REFUSED.into();
            }
            state.successor = Some((peer.clone(), generation));
            return format!(
                "OK native source-exec state=loaded operation={} primary={} bytes={} digest={}\n",
                op.recipe.operation,
                op.recipe.primary,
                snapshot.bytes,
                aterm_codec::hex::encode_lower(&snapshot.digest)
            );
        }
        if state.successor.as_ref() != Some(&(peer.clone(), generation)) {
            return REFUSED.into();
        }
        snapshot
    };
    let Some(connection) = crate::control::serving_connection() else {
        return REFUSED.into();
    };
    // Activation history grants no permission. A changed owner after IO refuses
    // the live transition, retaining DATA, rollback image and input custody.
    if payload
        .journal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .activated()
        .is_err()
    {
        return REFUSED.into();
    }
    let old = {
        let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if !op.current(ctx, &meta.supervisor_authority(crate::metrics::now_us()))
            || state.snapshot != Some(snapshot)
            || state.successor.as_ref() != Some(&(peer.clone(), generation))
            || Process::observe(peer.pid).as_ref() != Some(peer)
            || self::generation(peer.pid) != Some(generation)
        {
            return REFUSED.into();
        }
        let Some(authority) =
            meta.transfer_native_exec_claim(&op.authority, connection, crate::metrics::now_us())
        else {
            return REFUSED.into();
        };
        std::mem::replace(
            &mut state.phase,
            Phase::Activated(Activation {
                _payload: payload.clone(),
                process: peer.clone(),
                generation,
                authority,
                settled: false,
                adopted: false,
                disconnected: false,
            }),
        )
    };
    drop(old);
    op.wake.notify_all();
    activation_reply(op)
}
fn activation_current(
    ctx: &crate::SessionCtx,
    op: &Operation,
    peer: &Process,
    generation: i32,
    authority: &SupervisorAuthority,
    activation: &Activation,
) -> bool {
    activation.process == *peer
        && activation.generation == generation
        && activation.authority.connection() == crate::control::serving_connection()
        && *authority == activation.authority
        && ctx.fabric.hold().is_none()
        && !op.cancelled.load(std::sync::atomic::Ordering::Acquire)
        && Process::observe(peer.pid).as_ref() == Some(peer)
        && self::generation(peer.pid) == Some(generation)
}

fn adoption_current(
    ctx: &crate::SessionCtx,
    op: &Operation,
    peer: &Process,
    generation: i32,
    authority: &SupervisorAuthority,
    activation: &Activation,
) -> bool {
    activation.settled
        && activation.process == *peer
        && activation.generation == generation
        && *authority == activation.authority
        && ctx.fabric.hold().is_none()
        && !op.cancelled.load(std::sync::atomic::Ordering::Acquire)
        && Process::observe(peer.pid).as_ref() == Some(peer)
        && self::generation(peer.pid) == Some(generation)
}

/// Transfer supervision to its independent control socket only after actual
/// settlement. Later reconnection requires the native EOF witness; an operation
/// id, disk record, matching image or PID never recreates the original grant.
fn adopt(ctx: &crate::SessionCtx, op: &Operation, peer: &Process, generation: i32) -> String {
    let Some(connection) = crate::control::serving_connection() else {
        return REFUSED.into();
    };
    let payload = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !adoption_current(
            ctx,
            op,
            peer,
            generation,
            &meta.supervisor_authority(crate::metrics::now_us()),
            activation,
        ) {
            return REFUSED.into();
        }
        activation._payload.clone()
    };
    if payload._target.validate().is_err()
        || payload
            .constructor
            .reservation
            .as_ref()
            .is_none_or(|ctor| ctor.entered(peer) != Ok(true))
    {
        return REFUSED.into();
    }
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    let Phase::Activated(activation) = &mut state.phase else {
        return REFUSED.into();
    };
    if !adoption_current(
        ctx,
        op,
        peer,
        generation,
        &meta.supervisor_authority(crate::metrics::now_us()),
        activation,
    ) {
        return REFUSED.into();
    }
    if !activation.adopted && activation.authority.connection() == Some(connection) {
        return REFUSED.into();
    }
    if activation.adopted
        && !activation.disconnected
        && activation.authority.connection() != Some(connection)
    {
        // A real EOF may still be queued behind the first reconnect request.
        // Waiting retains the old live authority and grants this socket nothing.
        return format!(
            "OK native source-exec state=waiting operation={} primary={} build={}\n",
            op.recipe.operation, op.recipe.primary, op.recipe.build
        );
    }
    if !activation.adopted || activation.authority.connection() != Some(connection) {
        let Some(authority) = meta.transfer_native_exec_claim(
            &activation.authority,
            connection,
            crate::metrics::now_us(),
        ) else {
            return REFUSED.into();
        };
        activation.authority = authority;
        activation.adopted = true;
        activation.disconnected = false;
    }
    format!(
        "OK native source-exec state=adopted operation={} primary={} build={}\n",
        op.recipe.operation, op.recipe.primary, op.recipe.build
    )
}

fn settle(ctx: &crate::SessionCtx, op: &Operation, peer: &Process, generation: i32) -> String {
    let payload = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !activation_current(
            ctx,
            op,
            peer,
            generation,
            &meta.supervisor_authority(crate::metrics::now_us()),
            activation,
        ) {
            return REFUSED.into();
        }
        activation._payload.clone()
    };
    if payload._target.validate().is_err()
        || payload
            .constructor
            .reservation
            .as_ref()
            .is_none_or(|ctor| ctor.entered(peer) != Ok(true))
        || payload
            .journal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .settled()
            .is_err()
    {
        return REFUSED.into();
    }
    let old = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !activation_current(
            ctx,
            op,
            peer,
            generation,
            &meta.supervisor_authority(crate::metrics::now_us()),
            activation,
        ) {
            return REFUSED.into();
        }
        if activation.settled {
            return settlement_reply(op);
        }
        let Some(snapshot) = state.snapshot else {
            return REFUSED.into();
        };
        let Some(released) = state.data.as_ref().and_then(|pin| {
            pin.release_retained(
                snapshot.bytes,
                &aterm_codec::hex::encode_lower(&snapshot.digest),
            )
        }) else {
            return REFUSED.into();
        };
        let Phase::Activated(activation) = &mut state.phase else {
            unreachable!()
        };
        activation.settled = true;
        (released, state.custody.take(), state.data.take())
    };
    drop(old);
    op.wake.notify_all();
    settlement_reply(op)
}
fn settlement_reply(op: &Operation) -> String {
    format!(
        "OK native source-exec state=settled operation={} primary={} build={}\n",
        op.recipe.operation, op.recipe.primary, op.recipe.build
    )
}

fn activation_reply(op: &Operation) -> String {
    format!(
        "OK native source-exec state=activated operation={} primary={} build={}\n",
        op.recipe.operation, op.recipe.primary, op.recipe.build
    )
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
        input_sealed: std::sync::atomic::AtomicBool::new(false),
        deadline,
        cancelled: std::sync::atomic::AtomicBool::new(false),
        state: Mutex::new(State {
            phase: Phase::Preparing,
            custody: Some(Arc::new(custody)),
            data: Some(data),
            requested: false,
            possible: false,
            granted: false,
            snapshot: None,
            successor: None,
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Capabilities {
    protocol: u32,
    full_runtime: u32,
    runtime_readers: Vec<u32>,
    #[serde(default, rename = "extensions")]
    _extensions: BTreeMap<String, aterm_json::Value>,
    exec_custody: u32,
    exec_custody_adopted: bool,
    pid: u32,
    executable: std::path::PathBuf,
    device: u64,
    inode: u64,
    product_version: String,
}

impl Capabilities {
    fn valid(&self) -> bool {
        self.protocol == 3
            && self.full_runtime != 0
            && self.exec_custody == 2
            && !self.exec_custody_adopted
            && self.pid > 1
            && self
                .runtime_readers
                .first()
                .is_some_and(|version| *version != 0)
            && self
                .runtime_readers
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && self.runtime_readers.contains(&self.full_runtime)
    }

    fn accepts_runtime(&self, source_writer: u32) -> bool {
        self.valid() && source_writer != 0 && self.runtime_readers.contains(&source_writer)
    }
}

fn probe(target: &atpkg::prerequisite::HeldTool, image: &Image) -> Result<Capabilities, ()> {
    use std::os::unix::ffi::OsStrExt;
    let mut command = target.command().map_err(|_| ())?;
    command.args(["debug", "upgrade-capabilities"]);
    let output = aterm_uds::deadline::output_until_complete_stdout_with_limit(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(15),
        16 * 1024,
    )
    .map_err(|_| ())?;
    let response: Capabilities = aterm_json::from_slice(&output.stdout).map_err(|_| ())?;
    // Compatibility only. The signed HeldTool and the actual SDK constructor
    // independently supply image custody and execution admission; this PID
    // declaration is never used as either kind of proof.
    if !output.status.success()
        || !response.valid()
        || response.executable.as_os_str().as_bytes() != image.path
        || response.device != image.device
        || response.inode != image.inode
        || response.product_version != target.version()
    {
        return Err(());
    }
    target.validate().map_err(|_| ())?;
    Ok(response)
}

fn execute(job: &Job, ctx: &crate::SessionCtx, payload: &Arc<Payload>) {
    if !job
        .op
        .input_sealed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return;
    }
    let Some((bytes, digest)) = job
        .op
        .state
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .data
        .as_ref()
        .and_then(runtime_data::Pin::retained)
    else {
        return;
    };
    let Some(digest) = aterm_codec::hex::decode_vec(digest.as_bytes())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    else {
        return;
    };
    let snapshot = StoredSnapshot { bytes, digest };
    if payload._target.validate().is_err()
        || payload._rollback.validate().is_err()
        || !job.op.source_current()
    {
        return;
    }
    {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if !job
            .op
            .current(ctx, &meta.supervisor_authority(crate::metrics::now_us()))
        {
            return;
        }
        let mut state = job.op.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.snapshot.is_some_and(|old| old != snapshot) {
            return;
        }
        // Mark custody before IO: even panic/unknown durability is not NoExec.
        state.possible = true;
        state.snapshot = Some(snapshot);
        state.phase = Phase::Possible(payload.clone());
    }
    let mut journal = payload.journal.lock().unwrap_or_else(|p| p.into_inner());
    if journal.may_execute(snapshot).is_err() {
        return;
    }
    drop(journal);
    // Registry and package IO never run under Meta or operation state locks.
    if !job.op.source_current() {
        return;
    }
    {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        if !job
            .op
            .current(ctx, &meta.supervisor_authority(crate::metrics::now_us()))
        {
            return;
        }
    }
    if payload
        .constructor
        .reservation
        .as_ref()
        .is_none_or(|ctor| ctor.grant().is_err())
    {
        return;
    }
    let mut state = job.op.state.lock().unwrap_or_else(|p| p.into_inner());
    state.granted = true;
    job.op.wake.notify_all();
}

fn target(job: &Job, ctx: &crate::SessionCtx) -> Result<Payload, ()> {
    let rollback = atpkg::prerequisite::configured_tool(
        atpkg::prerequisite::Prerequisite::Codex,
        job.op.recipe.source_build,
        "native source replacement rollback custody",
    )
    .map_err(|_| ())?;
    if Image::at(rollback.physical_path()).as_ref() != Some(&job.op.source.image) {
        return Err(());
    }
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
    // Read the writer from the retained signed source image, rather than
    // guessing from the host's release or trusting the requested DATA bytes.
    let source = probe(&rollback, &job.op.source.image)?;
    if !probe(&target, &image)?.accepts_runtime(source.full_runtime) {
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
    let constructor = ConstructorOwner {
        reservation: Some(constructor),
        registry: job.registry.clone(),
    };
    let command = constructor
        .reservation
        .as_ref()
        .ok_or(())?
        .command()
        .encode()
        .ok_or(())?;
    let intent = aterm_json::json!({
        "version": 1, "epoch": host_epoch().ok_or(())?,
        "sid": ctx.self_id.as_str(), "operation": job.op.recipe.operation,
        "primary": job.op.recipe.primary, "generation": job.op.generation,
        "source": {"pid": job.op.source.pid, "birth": job.op.source.birth,
            "parent": job.op.source.parent, "group": job.op.source.group,
            "session": job.op.source.session, "image": job.op.source.image.path,
            "device": job.op.source.image.device, "inode": job.op.source.image.inode},
        "authority": format!("{:?}", job.op.authority),
        "human_seq": job.op.human_seq,
        "input_policy": "sealed-before-execute-v1",
        "deadline": job.op.deadline,
        "rollback_build": rollback.build(), "rollback_tree": rollback.tree_root(),
        "target_build": target.build(), "target_tree": target.tree_root(),
        "command": command,
    })
    .to_string();
    let journal = Journal::prepare(intent.as_bytes()).map_err(|_| ())?;
    let payload = Payload {
        _target: target,
        _rollback: rollback,
        constructor,
        journal: Mutex::new(journal),
        command,
    };
    payload._target.validate().map_err(|_| ())?;
    payload._rollback.validate().map_err(|_| ())?;
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
        let possible = job
            .op
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .possible;
        if !valid && !possible {
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
        // Once the original source execs, the retained operation belongs to
        // the successor front door. Image replacement is not cancellation.
        if !current
            && job
                .op
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .possible
        {
            return;
        }
        if !valid {
            job.op.close();
            return;
        }
        let mut state = job.op.state.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(state.phase, Phase::Closed) {
            return;
        }
        if let Some(result) = result.take() {
            state.phase = Phase::Prepared(Arc::new(result));
        }
        if state.requested {
            state.requested = false;
            let payload = match &state.phase {
                Phase::Prepared(payload) | Phase::Possible(payload) => Some(payload.clone()),
                _ => None,
            };
            drop(state);
            if let Some(payload) = payload {
                execute(&job, &ctx, &payload);
            }
            continue;
        }
        let _parked = job
            .op
            .wake
            .wait_timeout(state, Duration::from_millis(100))
            .unwrap_or_else(|p| p.into_inner());
    }
}

/// EOF cannot retract possible execution or return this exact claim's input
/// duty to the built-in harness while the operation retains native custody.
pub(super) fn retain_claim(ctx: &crate::SessionCtx, connection: u64) -> bool {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    let Some(op) = op else { return false };
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let current = meta.supervisor_authority(crate::metrics::now_us());
    let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    if let Phase::Activated(activation) = &mut state.phase {
        if activation.authority.connection() != Some(connection) {
            return false;
        }
        if activation.settled && activation.adopted {
            if current.connection() != Some(connection)
                || current.contract() != Some(2)
                || current.identity() != activation.authority.identity()
            {
                return false;
            }
            // Called only from the actual control-socket EOF path. Keep the
            // original live claim while its same fresh SDK actor reconnects.
            if Process::observe(activation.process.pid).as_ref() != Some(&activation.process)
                || generation(activation.process.pid) != Some(activation.generation)
                || activation
                    ._payload
                    .constructor
                    .reservation
                    .as_ref()
                    .is_none_or(|ctor| ctor.entered(&activation.process) != Ok(true))
            {
                return false;
            }
            // Ordinary acts may have renewed this same live claim's revision.
            // Capture only the native authority observed at this actual EOF.
            activation.authority = current;
            activation.disconnected = true;
            return true;
        }
    }
    let authority = match &state.phase {
        Phase::Activated(activation) => &activation.authority,
        _ => &op.authority,
    };
    authority.connection() == Some(connection) && state.possible && state.custody.is_some()
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
    if !was_pending {
        return false;
    }
    op.cancelled
        .store(true, std::sync::atomic::Ordering::Release);
    op.wake.notify_all();
    true
}

#[cfg(test)]
#[path = "native_source_exec_tests.rs"]
mod tests;
