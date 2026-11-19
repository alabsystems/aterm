// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native custody for a source replacement. Preparation retains signed original
//! and target builds, a pending SDK constructor, input position and runtime DATA.
//! Possible execution is journaled before constructor entry. Full-runtime
//! discovery remains unavailable until restoration and recovery are integrated.
//! Every decision reads the session in one order ([`Seen`]): the fabric's hold,
//! then Meta, then the operation's state beneath Meta.
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
    execution: Execution,
    possible: bool,
    snapshot: Option<StoredSnapshot>,
    successor: Option<(Process, i32)>,
    /// The claim the control EOF kept for this operation ([`retain_claim`]), handed back when
    /// its custody ends ([`hold_to_end`]).
    kept_claim: Option<KeptClaim>,
}

/// The worker can take a request and retry if it fails before granting the
/// constructor. Once Go is granted, no queued or later request can redo it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Execution {
    Idle,
    Requested,
    Granted,
}

impl Execution {
    fn request(&mut self) -> bool {
        if *self != Self::Idle {
            return false;
        }
        *self = Self::Requested;
        true
    }

    fn take_request(&mut self) -> bool {
        if *self != Self::Requested {
            return false;
        }
        *self = Self::Idle;
        true
    }
}

/// A claim a control connection's EOF left with a possible execution ([`retain_claim`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeptClaim {
    /// The closed connection that holds the claim.
    connection: u64,
    /// The window's local id of the session, whose metadata wake follows the claim's release,
    /// as it follows a release at the EOF itself.
    local: u64,
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
    first_adoption: Option<PendingAdoption>,
    adopted: bool,
    disconnected: bool,
}

fn entered_activation(process: &Process, expected_generation: i32, payload: &Payload) -> bool {
    Process::observe(process.pid).as_ref() == Some(process)
        && generation(process.pid) == Some(expected_generation)
        && payload
            .constructor
            .reservation
            .as_ref()
            .is_some_and(|ctor| ctor.entered(process) == Ok(true))
}

/// A live settlement has released runtime DATA and preparation custody. Only
/// the SDK socket may still carry the claim until independent adoption.
#[derive(Debug, Default)]
struct PendingAdoption {
    closed_until: Option<u64>,
}

impl PendingAdoption {
    fn released_after_eof(&self, now: u64) -> Option<bool> {
        match self.closed_until {
            None => Some(false),
            Some(until) if now < until => Some(true),
            Some(_) => None,
        }
    }

    fn retain_at_eof(
        &mut self,
        meta: &mut crate::session_timeline::SessionMeta,
        expected: &SupervisorAuthority,
        now: u64,
        until: Option<u64>,
        constructor_current: impl FnOnce() -> bool,
    ) -> bool {
        let until = *self.closed_until.get_or_insert(until.unwrap_or(now));
        if until > now
            && meta.supervisor_authority(now) == *expected
            && constructor_current()
            && meta.retain_closed_native_exec_claim(expected, until, now)
        {
            return true;
        }
        self.closed_until = Some(now);
        false
    }
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

/// The session as one decision reads it: the fabric's hold, then Meta and the
/// supervisor authority beneath it. The hold is read from the copy each hold
/// change stores ([`crate::fabric::SessionFabric::hold`]), never through the
/// fabric's Inbox, which a task mark holds while it waits on Meta: a decision
/// that waited on that Inbox beneath Meta would deadlock against the mark, and
/// with it every later reader of this session's Meta, the window's included
/// ([`crate::fabric::lock_rank`]). A hold change takes no Meta, so the hold is
/// a snapshot however it is read.
struct Seen<'a> {
    held: bool,
    meta: crate::session_timeline::MetaGuard<'a>,
    authority: SupervisorAuthority,
}
impl<'a> Seen<'a> {
    fn of(ctx: &'a crate::SessionCtx) -> Self {
        let held = ctx.fabric.hold().is_some();
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let authority = meta.supervisor_authority(crate::metrics::now_us());
        Self {
            held,
            meta,
            authority,
        }
    }
}

impl Operation {
    fn source_current(&self) -> bool {
        Process::observe(self.source.pid).as_ref() == Some(&self.source)
            && generation(self.source.pid) == Some(self.generation)
    }
    /// The operation's close: its deadline plus the
    /// [`aterm_agent::harness::native_act::COMPLETION_US`] a native restart's close adds,
    /// since a successor enters through the same registry constructor a restart's does. A
    /// successor activated before the deadline may settle and first adopt until the close.
    /// Settlement frees the worker early ([`hold_to_end`]); a closed SDK claim awaiting
    /// its first adoption lapses at the close.
    fn close_us(&self) -> u64 {
        self.deadline
            .saturating_add(aterm_agent::harness::native_act::COMPLETION_US)
    }
    fn current(&self, ctx: &crate::SessionCtx, seen: &Seen<'_>) -> bool {
        seen.authority == self.authority
            && !self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && !seen.held
            && (self.input_sealed.load(std::sync::atomic::Ordering::Acquire)
                || ctx.human_input.seq() == self.human_seq)
            && remaining(self.deadline).is_some()
    }
    fn seal_input(&self, ctx: &crate::SessionCtx, seen: &Seen<'_>) -> bool {
        // The caller's Seen retains Meta. Check policy/process conditions
        // before acquiring operation state, preserving the Meta/state order.
        if !self.source_current() || !self.current(ctx, seen) {
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

    #[cfg(debug_assertions)]
    fn fixture_observation<'a>(
        &'a self,
        ctx: &'a crate::SessionCtx,
        peer: &Process,
        generation: i32,
    ) -> crate::control::native_host_fixture::settle_ack_fault::NativeObservation<'a> {
        crate::control::native_host_fixture::settle_ack_fault::NativeObservation {
            sid: ctx.self_id.as_str(),
            operation: &self.recipe.operation,
            primary: &self.recipe.primary,
            build: self.recipe.build,
            source_build: self.recipe.source_build,
            pid: peer.pid,
            birth: peer.birth,
            generation,
        }
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
            // the first journal attempt. Keep the original DATA and leases
            // until the worker proves the operation ended ([`hold_to_end`]).
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
    /// Close the live operation and release its input custody. Only a dead, granted source
    /// with the checkpoint recorded for Go leaves evidence until the original close.
    fn end(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        source_gone: bool,
    ) -> (Option<KeptClaim>, Option<RecoveryEvidence>) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        let kept = state.kept_claim.take();
        let phase = std::mem::replace(&mut state.phase, Phase::Closed);
        let custody = state.custody.take();
        let data = state.data.take();
        let checkpoint = if source_gone && state.execution == Execution::Granted && state.possible {
            state.snapshot
        } else {
            None
        };
        drop(state);
        drop(custody);
        self.wake.notify_all();
        let evidence = data.and_then(|pin| {
            let saved = checkpoint?;
            // Settlement releases this exact DATA. Its journaled digest alone cannot retain it.
            let (size, digest) = pin.retained()?;
            (size == saved.bytes && digest == aterm_codec::hex::encode_lower(&saved.digest))
                .then_some(RecoveryEvidence { _phase: phase, pin })
        });
        (kept, evidence)
    }
}

/// The original signed images and constructor, journal and pinned DATA, removed from the
/// operation's executable phase. This owner grants no right to read or restore a checkpoint.
struct RecoveryEvidence {
    _phase: Phase,
    pin: runtime_data::Pin,
}

impl RecoveryEvidence {
    fn hold_to_close(self, op: &Operation, session: &Weak<crate::SessionCtx>, clock: &Clock) {
        // Only the passive pin needs this DATA now. A new proven source can reserve its own
        // slot while we retain the old checkpoint and signed images to the original close.
        if let Some(ctx) = session.upgrade() {
            let old = {
                let mut table = ctx
                    .fenced_writes
                    .native
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                self.pin.detach(&mut table.runtime_data)
            };
            drop(old);
        } else {
            return;
        }
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        while session.strong_count() > 0 {
            let Some(left) = clock.remaining(op.close_us()) else {
                break;
            };
            (state, _) = op
                .wake
                .wait_timeout(state, left.min(Duration::from_secs(1)))
                .unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// What one pass of [`hold_to_end`] does with a possible execution's custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// End its live custody and hand back the claim. A granted dead source's evidence has
    /// the original close as its last owner, independent of the input and supervisor.
    End,
    /// It stops holding the app handoff and keeps its input position, DATA and kept claim.
    FreeHandoff,
    /// It keeps everything: a step of it may still come.
    Keep,
}

/// A possible execution as one pass of [`hold_to_end`] reads it.
#[derive(Clone, Copy, Debug)]
struct Standing {
    /// Its constructor was granted, so a Go may have gone out.
    granted: bool,
    /// Its source's kernel process is gone.
    source_gone: bool,
    /// It was cancelled: by an abort, its owner's stop or a panic, or by its worker once it
    /// went stale before its source exec'd.
    cancelled: bool,
    /// A successor activated it and has not settled.
    activated: bool,
    /// Its deadline has passed: no load, activation or Go is admitted.
    past_deadline: bool,
    /// Its close has passed ([`Operation::close_us`]).
    past_close: bool,
}

impl Standing {
    /// What this pass does with the custody.
    /// - A constructor never granted sent no Go, and dropping its ungranted
    ///   reservation revokes it, so no process can enter through it: NoExec,
    ///   and the operation ends.
    /// - A source whose kernel process is gone left nothing of the operation
    ///   running in the tab, so it ends.
    /// - At its close it ends, whatever is still to come: a successor that
    ///   never settles, because a hold refused its settlement or its
    ///   connection died, holds the tab's input and the app's updates no
    ///   longer.
    /// - Before the close, a granted operation that can start nothing more
    ///   (cancelled, or past its deadline before activation) stops holding the
    ///   app handoff, and keeps its input position until the close, since its
    ///   granted source may still exec or its successor still start. An
    ///   activated one may still settle until the close, so past the deadline
    ///   it keeps the handoff too.
    fn pass(self) -> Pass {
        if !self.granted || self.source_gone || self.past_close {
            Pass::End
        } else if self.cancelled || (self.past_deadline && !self.activated) {
            Pass::FreeHandoff
        } else {
            Pass::Keep
        }
    }
}

/// A possible execution keeps its custody, DATA and leases past its worker's
/// last act ([`Operation::finish`]). The worker stays with it until that
/// custody ends: a successor's settlement takes it ([`settle`]), or a pass ends
/// it ([`Standing::pass`]). Only this worker grants the constructor, and it
/// grants nothing once [`run`] has returned. An ended operation hands back the
/// claim the control EOF kept for it, and the window hears that the session's
/// metadata moved, as it does when the EOF hands a claim back itself.
fn hold_to_end(op: &Operation, session: &Weak<crate::SessionCtx>, clock: &Clock) {
    loop {
        if session.strong_count() == 0 {
            // The tab closed: its operation table drops the custody.
            return;
        }
        // The kernel's process table is read before the operation's state is taken, never
        // beneath it. A source gone after this read is seen on the next pass.
        let source_gone = runtime_data::gone(&op.source);
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(custody) = state.custody.as_ref() else {
            return;
        };
        let standing = Standing {
            granted: state.execution == Execution::Granted,
            source_gone,
            cancelled: op.cancelled.load(std::sync::atomic::Ordering::Acquire),
            activated: matches!(state.phase, Phase::Activated(_)),
            past_deadline: clock.remaining(op.deadline).is_none(),
            past_close: clock.remaining(op.close_us()).is_none(),
        };
        match standing.pass() {
            Pass::End => {
                let (kept, evidence) = op.end(state, source_gone);
                if let (Some(kept), Some(ctx)) = (kept, session.upgrade())
                    && crate::session_timeline::release_closed_connection(
                        &ctx,
                        kept.connection,
                        crate::metrics::now_us(),
                        crate::control::reexec_grace_us(),
                    )
                {
                    crate::presence::post_meta_changed(kept.local);
                }
                if let Some(evidence) = evidence {
                    evidence.hold_to_close(op, session, clock);
                }
                return;
            }
            Pass::FreeHandoff => custody.release_handoff_guard(),
            Pass::Keep => {}
        }
        let _parked = op
            .wake
            .wait_timeout(state, Duration::from_secs(1))
            .unwrap_or_else(|p| p.into_inner());
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
/// `prepare source-exec` inherits prepare's connection/EOF tracking.
/// `settled source-exec` observes the exact replacement after the SDK EOF.
pub(super) fn route(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let action = words.next()?;
    if words.next() != Some("source-exec") {
        return None;
    }
    let action = match action {
        "info" | "prepare" | "seal-input" | "query" | "execute" | "load" | "activate"
        | "settle" | "settled" | "adopt" => action,
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
        "seal-input"
            | "query"
            | "abort"
            | "execute"
            | "load"
            | "activate"
            | "settle"
            | "settled"
            | "adopt"
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
    if matches!(verb, "load" | "activate" | "settle" | "settled" | "adopt") {
        return successor(ctx, &op, &peer, verb);
    }
    if op.source != peer || !op.source_current() {
        return REFUSED.into();
    }
    if verb == "seal-input" {
        let seen = Seen::of(ctx);
        if !op.seal_input(ctx, &seen) {
            return REFUSED.into();
        }
        return op.status();
    }
    if verb == "abort" {
        op.close();
    }
    if verb == "execute" {
        let seen = Seen::of(ctx);
        if !op.current(ctx, &seen) {
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
        if state.execution.request() {
            op.wake.notify_all();
        }
    }
    if matches!(verb, "query" | "execute") {
        let seen = Seen::of(ctx);
        if op.current(ctx, &seen) {
            let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.execution == Execution::Granted
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
    if verb == "settled" {
        return observe_settlement(ctx, op, peer, generation);
    }
    if verb == "adopt" {
        return adopt(ctx, op, peer, generation);
    }
    let payload = {
        let seen = Seen::of(ctx);
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Phase::Activated(activation) = &state.phase {
            if !activation_current(op, peer, generation, &seen, activation) {
                return REFUSED.into();
            }
            activation._payload.clone()
        } else {
            if !op.current(ctx, &seen) || state.execution != Execution::Granted {
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
        let seen = Seen::of(ctx);
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Phase::Activated(activation) = &state.phase {
            return if activation_current(op, peer, generation, &seen, activation) {
                activation_reply(op)
            } else {
                REFUSED.into()
            };
        }
        if !op.current(ctx, &seen)
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
        let mut seen = Seen::of(ctx);
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if !op.current(ctx, &seen)
            || state.snapshot != Some(snapshot)
            || state.successor.as_ref() != Some(&(peer.clone(), generation))
            || Process::observe(peer.pid).as_ref() != Some(peer)
            || self::generation(peer.pid) != Some(generation)
        {
            return REFUSED.into();
        }
        let Some(authority) = crate::control::transfer_served_native_exec_claim(
            &mut seen.meta,
            &op.authority,
            connection,
            None,
            crate::metrics::now_us(),
        ) else {
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
                first_adoption: None,
                adopted: false,
                disconnected: false,
            }),
        )
    };
    drop(old);
    op.wake.notify_all();
    activation_reply(op)
}
fn activation_owned(
    op: &Operation,
    peer: &Process,
    generation: i32,
    seen: &Seen<'_>,
    activation: &Activation,
) -> bool {
    activation.process == *peer
        && activation.generation == generation
        && seen.authority == activation.authority
        && !seen.held
        && !op.cancelled.load(std::sync::atomic::Ordering::Acquire)
        && Process::observe(peer.pid).as_ref() == Some(peer)
        && self::generation(peer.pid) == Some(generation)
}

fn activation_current(
    op: &Operation,
    peer: &Process,
    generation: i32,
    seen: &Seen<'_>,
    activation: &Activation,
) -> bool {
    activation.authority.connection() == crate::control::serving_connection()
        && activation_owned(op, peer, generation, seen, activation)
}

fn adoption_current(
    op: &Operation,
    peer: &Process,
    generation: i32,
    seen: &Seen<'_>,
    activation: &Activation,
) -> bool {
    activation.settled
        && (activation.adopted || remaining(op.close_us()).is_some())
        && activation_owned(op, peer, generation, seen, activation)
}

/// Transfer supervision to its independent control socket only after actual
/// settlement. Later reconnection requires the native EOF witness; an operation
/// id, disk record, matching image or PID never recreates the original grant.
fn adopt(ctx: &crate::SessionCtx, op: &Operation, peer: &Process, generation: i32) -> String {
    let Some(connection) = crate::control::serving_connection() else {
        return REFUSED.into();
    };
    let payload = {
        let seen = Seen::of(ctx);
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !adoption_current(op, peer, generation, &seen, activation) {
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
    let mut seen = Seen::of(ctx);
    let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    let Phase::Activated(activation) = &mut state.phase else {
        return REFUSED.into();
    };
    if !adoption_current(op, peer, generation, &seen, activation) {
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
        let detached_until = activation
            .first_adoption
            .as_ref()
            .and_then(|pending| pending.closed_until);
        let Some(authority) = crate::control::transfer_served_native_exec_claim(
            &mut seen.meta,
            &activation.authority,
            connection,
            detached_until,
            crate::metrics::now_us(),
        ) else {
            return REFUSED.into();
        };
        activation.authority = authority;
        activation.first_adoption = None;
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
        let seen = Seen::of(ctx);
        let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !activation_current(op, peer, generation, &seen, activation) {
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
        let seen = Seen::of(ctx);
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        let Phase::Activated(activation) = &state.phase else {
            return REFUSED.into();
        };
        if !activation_current(op, peer, generation, &seen, activation) {
            return REFUSED.into();
        }
        if activation.settled {
            return settlement_reply(op, "settled");
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
        activation.first_adoption = Some(PendingAdoption::default());
        (released, state.custody.take(), state.data.take())
    };
    drop(old);
    op.wake.notify_all();
    #[cfg(debug_assertions)]
    if let Some(connection) = crate::control::serving_connection() {
        crate::control::native_host_fixture::settle_ack_fault::record_commit(
            ctx.self_id.as_str(),
            &op.recipe.operation,
            peer.pid,
            connection,
            generation,
        );
    }
    settlement_reply(op, "settled")
}

fn settlement_observation(
    ctx: &crate::SessionCtx,
    op: &Operation,
    peer: &Process,
    generation: i32,
) -> Option<(Arc<Payload>, bool)> {
    let connection = crate::control::serving_connection()?;
    let seen = Seen::of(ctx);
    // Preparation cannot replace the operation while this decision holds Meta.
    let current = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .as_ref()
        .is_some_and(|current| std::ptr::eq(current.as_ref(), op));
    if !current {
        return None;
    }
    let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    let Phase::Activated(activation) = &state.phase else {
        return None;
    };
    if activation.adopted
        || activation.authority.connection() == Some(connection)
        || remaining(op.close_us()).is_none()
        || !activation_owned(op, peer, generation, &seen, activation)
    {
        return None;
    }
    let released = if activation.settled {
        activation
            .first_adoption
            .as_ref()?
            .released_after_eof(crate::metrics::now_us())?
    } else {
        false
    };
    Some((activation._payload.clone(), released))
}

/// Read back settlement after a lost acknowledgement. Only the old SDK's
/// actual EOF proves its serving thread has finished releasing custody.
fn observe_settlement(
    ctx: &crate::SessionCtx,
    op: &Operation,
    peer: &Process,
    generation: i32,
) -> String {
    let Some((payload, released)) = settlement_observation(ctx, op, peer, generation) else {
        return REFUSED.into();
    };
    if released && payload._target.validate().is_err() {
        return REFUSED.into();
    }
    if !entered_activation(peer, generation, &payload) {
        return REFUSED.into();
    }
    if !settlement_observation(ctx, op, peer, generation).is_some_and(
        |(current, currently_released)| {
            (!released || currently_released) && Arc::ptr_eq(&current, &payload)
        },
    ) {
        return REFUSED.into();
    }
    #[cfg(debug_assertions)]
    if released && let Some(connection) = crate::control::serving_connection() {
        crate::control::native_host_fixture::settle_ack_fault::record_readback(
            op.fixture_observation(ctx, peer, generation),
            connection,
        );
    }
    settlement_reply(op, if released { "settled" } else { "waiting" })
}

fn settlement_reply(op: &Operation, state: &str) -> String {
    format!(
        "OK native source-exec state={state} operation={} primary={} build={}\n",
        op.recipe.operation, op.recipe.primary, op.recipe.build
    )
}

fn activation_reply(op: &Operation) -> String {
    format!(
        "OK native source-exec state=activated operation={} primary={} build={}\n",
        op.recipe.operation, op.recipe.primary, op.recipe.build
    )
}

fn source_claim_allows_prepare(
    authority: &SupervisorAuthority,
    proof: &crate::self_claim::Proven,
    connection: u64,
) -> bool {
    authority.contract() == Some(2)
        && authority.identity() == Some(proof)
        && authority.connection() == Some(connection)
        && authority.external_takes(&crate::harness_host::holder(), Scope::Upgrade)
}

struct PrepareSource<'a> {
    proof: &'a crate::self_claim::Proven,
    connection: u64,
    process: Process,
    generation: i32,
}

impl PrepareSource<'_> {
    fn authorize<'ctx>(&self, ctx: &'ctx crate::SessionCtx) -> Option<Seen<'ctx>> {
        let seen = Seen::of(ctx);
        (source_claim_allows_prepare(&seen.authority, self.proof, self.connection)
            && !seen.held
            && Process::observe(self.process.pid).as_ref() == Some(&self.process)
            && generation(self.process.pid) == Some(self.generation))
        .then_some(seen)
    }

    fn existing_reply(&self, old: Option<&Operation>, recipe: &Recipe) -> Option<String> {
        let old = old?;
        if old.recipe.operation == recipe.operation {
            return Some(
                if old.recipe == *recipe
                    && old.source == self.process
                    && old.generation == self.generation
                {
                    old.status()
                } else {
                    REFUSED.into()
                },
            );
        }
        old.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .custody
            .is_some()
            .then(|| REFUSED.into())
    }
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
    let Some(connection) = crate::control::serving_connection() else {
        return REFUSED.into();
    };
    let (Some(process), Some(generation)) = (Process::observe(pid), generation(pid)) else {
        return REFUSED.into();
    };
    prepare_source(
        ctx,
        recipe,
        PrepareSource {
            proof: &proof,
            connection,
            process,
            generation,
        },
        &WORKERS,
    )
}

fn prepare_source(
    ctx: &Arc<crate::SessionCtx>,
    recipe: Recipe,
    source: PrepareSource<'_>,
    workers: &Arc<crate::budget::Slots>,
) -> String {
    {
        let Some(_seen) = source.authorize(ctx) else {
            return REFUSED.into();
        };
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(reply) = source.existing_reply(table.source_exec.as_deref(), &recipe) {
            return reply;
        }
    }

    // Only a new preparation captures the launch inputs and reserves a worker.
    let pid = source.process.pid;
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
    let Some(registry) = crate::control::serving_source_registry() else {
        return REFUSED.into();
    };
    let Some(worker) = Worker::reserve(workers) else {
        return REFUSED.into();
    };

    // A concurrent prepare or a changed claim may win while the inputs are read.
    let Some(seen) = source.authorize(ctx) else {
        return REFUSED.into();
    };
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(reply) = source.existing_reply(table.source_exec.as_deref(), &recipe) {
        return reply;
    }
    let Some((op, previous)) = publish_preparation(
        ctx,
        &mut table,
        recipe,
        source.process,
        source.generation,
        seen.authority.clone(),
        aterm_types::update_boundary::Boundary::process(),
    ) else {
        return REFUSED.into();
    };
    drop(table);
    drop(seen);
    drop(previous);
    #[cfg(debug_assertions)]
    crate::control::native_host_fixture::settle_ack_fault::record_prepare(op.fixture_observation(
        ctx,
        &op.source,
        op.generation,
    ));
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

/// Admit the preparation while Meta and the operation table are held. Publish its data
/// slot and operation together once no admission step can refuse. Return the previous
/// operation so its resources are dropped outside both locks.
fn publish_preparation(
    ctx: &crate::SessionCtx,
    table: &mut Operations,
    recipe: Recipe,
    source: Process,
    generation: i32,
    authority: SupervisorAuthority,
    boundary: &'static aterm_types::update_boundary::Boundary,
) -> Option<(Arc<Operation>, Option<Arc<Operation>>)> {
    let mut slot = table.runtime_data.clone();
    let data = runtime_data::Slot::reserve_pinned(&mut slot, &recipe.operation, &source)?;
    let boundary =
        boundary.try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)?;
    let custody = ctx
        .fenced_writes
        .reserve_native(authority.revision(), &recipe.operation, boundary)
        .ok()?;
    let deadline = aterm_agent::harness::native_act::monotonic_us()?.checked_add(120_000_000)?;
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
            execution: Execution::Idle,
            possible: false,
            snapshot: None,
            successor: None,
            kept_claim: None,
        }),
        wake: std::sync::Condvar::new(),
    });
    table.runtime_data = slot;
    let previous = table.source_exec.replace(op.clone());
    Some((op, previous))
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
    fn reserve(workers: &Arc<crate::budget::Slots>) -> Option<Self> {
        let budget = workers.try_take()?;
        let (send, receive) = mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new()
            .name("codex-source-prepare".into())
            .spawn(move || {
                let _budget = budget;
                if let Ok(job) = receive.recv() {
                    let op = job.op.clone();
                    let session = job.ctx.clone();
                    guarded(&op, || run(job));
                    guarded(&op, || hold_to_end(&op, &session, &Clock::System));
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
        let seen = Seen::of(ctx);
        if !job.op.current(ctx, &seen) {
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
    if !job.op.current(ctx, &Seen::of(ctx)) {
        return;
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
    state.execution = Execution::Granted;
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
        let valid = current && job.op.current(&ctx, &Seen::of(&ctx));
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
        let valid = current && job.op.current(&ctx, &Seen::of(&ctx));
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
        if state.execution.take_request() {
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

/// A live renewal leaves the authority revision unchanged. A release and
/// retake can restore the same actor and connection under a new tenure.
fn record_adopted_eof(
    expected: &SupervisorAuthority,
    current: &SupervisorAuthority,
    disconnected: &mut bool,
    constructor_current: impl FnOnce() -> bool,
) -> bool {
    if current != expected || !constructor_current() {
        return false;
    }
    *disconnected = true;
    true
}

/// EOF cannot retract possible execution or return this exact claim's input
/// duty to the built-in harness while the operation retains native custody.
/// The claim kept moves to a successor's connection when one activates or
/// adopts (`transfer_native_exec_claim` closes the old one). An unsettled
/// claim goes back when custody ends ([`hold_to_end`]). A settled SDK claim
/// gains a bounded first-adoption expiry at its EOF. `local` is the window's
/// id of the session `ctx` is.
pub(super) fn retain_claim(ctx: &crate::SessionCtx, local: u64, connection: u64) -> bool {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    let Some(op) = op else { return false };
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    let now = crate::metrics::now_us();
    let current = meta.supervisor_authority(now);
    if let Phase::Activated(activation) = &mut state.phase {
        if activation.authority.connection() != Some(connection) {
            return false;
        }
        if let Some(pending) = &mut activation.first_adoption {
            // Convert the remaining cross-exec budget to the app's metadata clock.
            let until = remaining(op.close_us())
                .and_then(|left| u64::try_from(left.as_micros()).ok())
                .and_then(|left| now.checked_add(left));
            let kept = pending.retain_at_eof(&mut meta, &activation.authority, now, until, || {
                !op.cancelled.load(std::sync::atomic::Ordering::Acquire)
                    && entered_activation(
                        &activation.process,
                        activation.generation,
                        &activation._payload,
                    )
            });
            #[cfg(debug_assertions)]
            if kept && let Some(expires_us) = pending.closed_until {
                crate::control::native_host_fixture::settle_ack_fault::record_eof(
                    ctx.self_id.as_str(),
                    &op.recipe.operation,
                    activation.process.pid,
                    connection,
                    activation.authority.revision(),
                    expires_us,
                );
            }
            drop(state);
            drop(meta);
            // Retention bypasses the control lane's ordinary EOF metadata wake.
            crate::presence::post_meta_changed(local);
            return kept;
        }
        if activation.settled && activation.adopted {
            // Called only from the actual control-socket EOF path. Keep the
            // original live claim while its same fresh SDK actor reconnects.
            return record_adopted_eof(
                &activation.authority,
                &current,
                &mut activation.disconnected,
                || {
                    entered_activation(
                        &activation.process,
                        activation.generation,
                        &activation._payload,
                    )
                },
            );
        }
    }
    let authority = match &state.phase {
        Phase::Activated(activation) => &activation.authority,
        _ => &op.authority,
    };
    let kept =
        authority.connection() == Some(connection) && state.possible && state.custody.is_some();
    if kept {
        state.kept_claim = Some(KeptClaim { connection, local });
    }
    kept
}

fn first_adoption_deadline(phase: &Phase) -> Option<u64> {
    match phase {
        Phase::Activated(activation) => activation.first_adoption.as_ref()?.closed_until,
        _ => None,
    }
}

pub(super) fn adoption_deadline(ctx: &crate::SessionCtx) -> Option<u64> {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone()?;
    let state = op.state.lock().unwrap_or_else(|p| p.into_inner());
    first_adoption_deadline(&state.phase)
}

pub(super) fn expire_adoption(ctx: &crate::SessionCtx, now: u64) {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .source_exec
        .clone();
    let Some(op) = op else { return };
    let expired = {
        let mut state = op.state.lock().unwrap_or_else(|p| p.into_inner());
        if first_adoption_deadline(&state.phase).is_none_or(|at| at > now) {
            return;
        }
        std::mem::replace(&mut state.phase, Phase::Closed)
    };
    // Package and registry disposal may do I/O. A short cleanup after the
    // timer fires needs no preparation slot and holds no session locks.
    let _ = std::thread::Builder::new()
        .name("codex-source-adoption-expiry".into())
        .spawn(move || drop(expired));
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
