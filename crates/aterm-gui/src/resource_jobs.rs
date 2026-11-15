// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated delegated jobs.
//!
//! A tab's tool call can run in a shared, launchd-owned app server outside the tab's parent
//! tree: parent traversal misses that work, and charging the server would bill every tab it
//! serves to one. So a process claims itself. `resources register` binds only the caller's own
//! kernel peer PID, checked against the start record it names (`aterm resource-run` registers,
//! then execs the command in place), never an arbitrary PID, its ancestors or its siblings, and
//! the ledger credits that root's start-checked descendants to its tab once.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use aterm_vitals::delegated::{Binding, Job, Ledger, Process, ProcessKey};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
#[path = "resource_jobs_receipts.rs"]
mod receipts;

#[derive(Default)]
struct State {
    ledger: Ledger,
    origins: Vec<CarriedOrigin>,
    origins_restore_pending: bool,
    origins_incomplete: bool,
    activated: HashSet<String>,
    usage: Vec<aterm_vitals::model::DelegatedUsage>,
    sampled: Option<Instant>,
    current_complete: bool,
    #[cfg(unix)]
    native_host: Option<aterm_resource_host::resource_observation::ProcessKey>,
    #[cfg(unix)]
    native_seq: u64,
    #[cfg(unix)]
    native_lost_origins: u64,
    #[cfg(unix)]
    native_pending: std::collections::BTreeMap<(u32, u64, u64), PendingNative>,
    #[cfg(unix)]
    native_delivered: std::collections::BTreeMap<(u32, u64, u64), PendingNative>,
    #[cfg(unix)]
    native_receipts: HashMap<String, (u64, u64)>,
    #[cfg(unix)]
    native_queue_lost: u64,
    #[cfg(unix)]
    native_tracking_cut: Option<u64>,
    #[cfg(unix)]
    native_tracking: bool,
    #[cfg(unix)]
    native_observed_revision: u64,
    #[cfg(unix)]
    native_outstanding: bool,
    #[cfg(unix)]
    native_unadopted: bool,
    #[cfg(unix)]
    native_awaiting_observation: bool,
    #[cfg(unix)]
    native_source_lost: bool,
    #[cfg(unix)]
    native_cpu_cut: aterm_vitals::model::NativeCpuCut,
}

#[cfg(unix)]
static NATIVE_SUBMITTING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(unix)]
static NATIVE_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(unix)]
static NATIVE_WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

#[cfg(unix)]
pub(crate) fn set_native_wake(wake: impl Fn() + Send + Sync + 'static) {
    let _ = NATIVE_WAKE.set(Box::new(wake));
}

#[cfg(unix)]
fn wake_native_sampler() {
    if let Some(wake) = NATIVE_WAKE.get() {
        wake();
    }
}

/// Keep sampling across the submission handoff without making execution wait on telemetry.
#[cfg(unix)]
pub(crate) struct NativeSubmission;

#[cfg(unix)]
pub(crate) fn native_submission() -> NativeSubmission {
    use std::sync::atomic::Ordering;
    NATIVE_SUBMITTING.fetch_add(1, Ordering::AcqRel);
    NATIVE_REVISION.fetch_add(1, Ordering::AcqRel);
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    state.native_tracking = true;
    if state.native_tracking_cut.is_none() {
        state.native_tracking_cut = aterm_uds::process::continuous_ns();
    }
    drop(state);
    wake_native_sampler();
    NativeSubmission
}

#[cfg(unix)]
impl Drop for NativeSubmission {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        // Even a job that completed before the first sweep owes one observer drain. Publishing
        // this revision before releasing the handoff prevents a transient idle decision.
        NATIVE_REVISION.fetch_add(1, Ordering::AcqRel);
        NATIVE_SUBMITTING.fetch_sub(1, Ordering::AcqRel);
        wake_native_sampler();
    }
}

#[cfg(unix)]
fn native_demand(state: &State, submitting: usize, revision: u64) -> bool {
    submitting > 0
        || revision != state.native_observed_revision
        || state.native_outstanding
        || state.native_unadopted
        || state.native_awaiting_observation
        || !state.native_pending.is_empty()
}

/// A delivered final stays in sampling demand until the model adopted it on a whole sweep.
pub(crate) fn native_model_pending(pending: bool) {
    #[cfg(unix)]
    {
        let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
        native_model_pending_into(&mut state, pending);
        let finished = !native_demand(
            &state,
            NATIVE_SUBMITTING.load(std::sync::atomic::Ordering::Acquire),
            NATIVE_REVISION.load(std::sync::atomic::Ordering::Acquire),
        );
        if finished {
            state.native_tracking = false;
            state.native_tracking_cut = None;
            if aterm_resource_host::finish_resource_observer() {
                state.native_host = None;
                state.native_seq = 0;
                state.native_lost_origins = 0;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pending;
}

#[cfg(unix)]
fn native_model_pending_into(state: &mut State, pending: bool) {
    state.native_unadopted = pending;
    if !pending {
        state.native_delivered.clear();
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PendingNative {
    host_pid: u32,
    host_start: u64,
    seq: u64,
    session: String,
    pid: u32,
    start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coalition: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cpu_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_cpu_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait_cpu_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reaps_unwaited: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_cut_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<ReceiptWindow>,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct CarriedCpuCut {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuous_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot: Option<u64>,
}

impl From<aterm_vitals::model::NativeCpuCut> for CarriedCpuCut {
    fn from(cut: aterm_vitals::model::NativeCpuCut) -> Self {
        Self {
            continuous_ns: cut.continuous_ns,
            boot: cut.boot,
        }
    }
}

impl From<CarriedCpuCut> for aterm_vitals::model::NativeCpuCut {
    fn from(cut: CarriedCpuCut) -> Self {
        Self {
            continuous_ns: cut.continuous_ns,
            boot: cut.boot,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct ReceiptWindow {
    previous: CarriedCpuCut,
    current: CarriedCpuCut,
}

static RUNNER: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Registers this process's front door ([`crate::ledger_key::own_front_door`]) as the resource
/// runner: a client runs `<runner> resource-run …`, a front-door verb, which a window alias's
/// own path would hand to a window instead.
pub(crate) fn register_runner() {
    if let Some(path) = crate::ledger_key::own_front_door() {
        let _ = RUNNER.set(path);
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Whether delegated work needs samples even when the Deck is hidden.
pub(crate) fn active() -> bool {
    let state = state().lock().unwrap_or_else(|p| p.into_inner());
    let classic = !state.ledger.is_empty();
    #[cfg(unix)]
    {
        use std::sync::atomic::Ordering;
        classic
            || native_demand(
                &state,
                NATIVE_SUBMITTING.load(Ordering::Acquire),
                NATIVE_REVISION.load(Ordering::Acquire),
            )
    }
    #[cfg(not(unix))]
    classic
}

fn key(pid: u32) -> Option<ProcessKey> {
    let pid_signed = i32::try_from(pid).ok()?;
    aterm_uds::process::read_process_start(pid_signed).map(|start| ProcessKey {
        pid,
        start: start.boot.0,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Identity {
    Known(ProcessKey),
    Absent,
    Unavailable,
}

fn identity(pid: u32) -> Identity {
    if let Some(key) = key(pid) {
        Identity::Known(key)
    } else if aterm_uds::process::pid_alive(pid) {
        Identity::Unavailable
    } else {
        Identity::Absent
    }
}

fn word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn label(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 200 && !text.chars().any(char::is_control)
}

fn observed_program(ctx: &crate::SessionCtx) -> Option<String> {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .program
        .clone()
        .filter(|name| word(name))
}

fn program(ctx: &crate::SessionCtx) -> String {
    program_by(
        ctx,
        || crate::quit_safety::foreground_pgrp(ctx.sink.master()),
        crate::self_claim::Proven::alive,
        crate::self_claim::runs_alab_build,
    )
}

fn program_by(
    ctx: &crate::SessionCtx,
    mut foreground: impl FnMut() -> i32,
    mut alive: impl FnMut(&crate::self_claim::Proven) -> bool,
    mut image: impl FnMut(u32) -> bool,
) -> String {
    if let Some(name) = observed_program(ctx) {
        return name;
    }
    let claim = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.clone())
            .map(|proof| (proof, meta.supervisor_revision))
    };
    let Some((proof, revision)) = claim else {
        return observed_program(ctx).unwrap_or_else(|| "unknown".into());
    };
    let mut current = true;
    for _ in 0..2 {
        if !alive(&proof) || !image(proof.pid) || i64::from(foreground()) != i64::from(proof.pid) {
            current = false;
            break;
        }
    }
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let standing = meta.supervisor_revision == revision
        && meta
            .live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref())
            == Some(&proof);
    observed_program(ctx).unwrap_or_else(|| {
        if current && standing {
            "codex"
        } else {
            "unknown"
        }
        .into()
    })
}

/// Handle a session-resolved resource request after the control op gate.
pub(crate) fn command(ctx: &crate::SessionCtx, owner: bool, rest: &str) -> String {
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("capabilities") if words.next().is_none() => {
            let runner = RUNNER.get().and_then(|path| path.to_str());
            let supported =
                key(std::process::id()).is_some() && boot().is_some() && runner.is_some();
            format!(
                "OK resources version=1 available={} self_register=peer-pid-start group_signals=exec-inherit cpu=shared-kernel-leases runner={} ai={} session={}\n",
                u8::from(supported),
                aterm_codec::pct::encode(runner.unwrap_or("-")),
                program(ctx),
                ctx.self_id.as_str()
            )
        }
        Some("cpu") if words.next().is_none() => {
            let dir = aterm_capacity::ledger::user_dir();
            let cpu = aterm_capacity::CpuObservation::read(dir.as_deref());
            format!("OK 1 resources-cpu version=1\n{}\n", cpu.json())
        }
        Some("register") => {
            if !owner {
                return "ERR denied\n".into();
            }
            let Some(root) = crate::control::serving_peer_pid().and_then(key) else {
                return "ERR resources no-live-local-peer\n".into();
            };
            if boot().is_none() {
                return "ERR resources boot-unavailable\n".into();
            }
            let mut ai = None;
            let mut job = None;
            let mut expected_start = None;
            for part in words {
                if let Some(value) = part.strip_prefix("start=") {
                    if expected_start.is_some() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return usage();
                    }
                    let Ok(start) = value.parse::<u64>() else {
                        return usage();
                    };
                    expected_start = Some(start);
                } else if let Some(value) = part.strip_prefix("ai=") {
                    if ai.is_some() || !word(value) {
                        return usage();
                    }
                    ai = Some(value.to_string());
                } else if let Some(value) = part.strip_prefix("job=") {
                    let value = aterm_codec::pct::decode(value);
                    if job.is_some() || !label(&value) {
                        return usage();
                    }
                    job = Some(value);
                } else {
                    return usage();
                }
            }
            let Some(label) = job else {
                return usage();
            };
            let session = ctx.self_id.as_str().to_string();
            let job = Job {
                root,
                session: session.clone(),
                ai: ai.unwrap_or_else(|| program(ctx)),
                label,
            };
            if !confirmed_peer(root, expected_start, key(root.pid)) {
                return "ERR resources stale-peer\n".into();
            }
            let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
            if let Err(error) = state.ledger.register(job) {
                return format!("ERR resources {error:?}\n");
            }
            state.activated.insert(session.clone());
            format!(
                "OK resources version=1 root_pid={} start={} session={}\n",
                root.pid, root.start, session
            )
        }
        None => list(ctx.self_id.as_str()),
        _ => usage(),
    }
}

fn confirmed_peer(root: ProcessKey, expected: Option<u64>, now: Option<ProcessKey>) -> bool {
    expected == Some(root.start) && now == Some(root)
}

fn usage() -> String {
    "ERR usage: resources [capabilities|cpu|register start=<kernel-start> [ai=<word>] job=<percent-encoded-label>]\n".into()
}

fn list(session: &str) -> String {
    let state = state().lock().unwrap_or_else(|p| p.into_inner());
    let age = state
        .sampled
        .map(|sampled| sampled.elapsed().as_millis().to_string())
        .unwrap_or_else(|| "-".into());
    let mut lines = Vec::new();
    #[cfg(unix)]
    if let Some(&(cpu, unread)) = state.native_receipts.get(session) {
        lines.push(format!("native_receipt session={session} cpu_charge_ns={cpu} unread={unread} source_queue_lost={} source_lost={} source=original-owning-wait\n", state.native_queue_lost, state.native_source_lost));
    }
    for job in state
        .ledger
        .jobs()
        .into_iter()
        .filter(|job| job.session == session)
    {
        let usage = state.usage.iter().find(|usage| usage.root == job.root);
        let cpu = usage
            .map(|usage| format!("{:.4}", usage.cores))
            .unwrap_or_else(|| "-".into());
        let footprint = usage
            .and_then(|usage| usage.footprint)
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "-".into());
        let procs = usage
            .map(|usage| usage.procs.to_string())
            .unwrap_or_else(|| "-".into());
        let job_age = if usage.is_some() { age.as_str() } else { "-" };
        let quality = if usage.is_some() && state.current_complete {
            "complete"
        } else {
            "unavailable"
        };
        lines.push(format!("job root_pid={} start={} session={} ai={} label={} cpu_cores={} physical_bytes={} procs={} age_ms={} sample={}\n",
            job.root.pid, job.root.start, job.session, job.ai, aterm_codec::pct::encode(&job.label),
            cpu, footprint, procs, job_age, quality));
    }
    format!("OK {} resources version=1\n{}", lines.len(), lines.concat())
}

/// One process reading, with ownership and counter availability kept together.
#[derive(Default)]
pub(crate) struct Observation {
    pub procs: Option<Vec<aterm_vitals::model::ProcSample>>,
    pub delegated: Vec<aterm_vitals::model::DelegatedJob>,
    pub native_roots: Vec<aterm_vitals::model::NativeRoot>,
    pub native_terminals: Vec<aterm_vitals::model::NativeTerminal>,
    pub incomplete: bool,
    pub source_lost: bool,
    pub unproved_tabs: Vec<u64>,
}

/// Folds process identities from `sweep` into the delegated ledger.
pub(crate) fn sample(
    sweep: &aterm_sysprobe::Sweep,
    tabs: &[(u64, String)],
    epoch: std::time::Instant,
    cutoff: aterm_vitals::model::NativeCpuCut,
) -> Observation {
    #[cfg(unix)]
    let (native, native_revision) = {
        let (since, revision) = {
            let state = state().lock().unwrap_or_else(|p| p.into_inner());
            (
                state.native_host.map_or(0, |_| state.native_seq),
                NATIVE_REVISION.load(std::sync::atomic::Ordering::Acquire),
            )
        };
        // The enrolled native reader can wait for its custodian. Registration and capability
        // reads must retain access to the GUI ledger while that private request is pending.
        (aterm_resource_host::resource_observation(since), revision)
    };
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    let observation = sample_into(&mut state, sweep, tabs, &identity, epoch);
    #[cfg(unix)]
    let mut observation = observation;
    #[cfg(unix)]
    apply_native_read(&mut state, &mut observation, native, native_revision, tabs);
    #[cfg(unix)]
    deliver_native(&mut state, &mut observation, tabs, cutoff);
    #[cfg(not(unix))]
    let _ = cutoff;
    observation
}

#[cfg(unix)]
fn apply_native_read(
    state: &mut State,
    observation: &mut Observation,
    read: aterm_resource_host::ObservationRead,
    revision: u64,
    tabs: &[(u64, String)],
) {
    if read.source_ended {
        // A dead custodian cannot settle more observer debt. Historical receipts remain
        // separately owned, and a successor starts at its own cursor, never the old one.
        state.native_source_lost = true;
        state.native_host = None;
        state.native_seq = 0;
        state.native_lost_origins = 0;
        state.native_outstanding = false;
        state.native_awaiting_observation = false;
    }
    match read.result {
        Ok(native) => {
            if apply_native(state, observation, native, tabs) {
                state.native_observed_revision = revision;
            }
        }
        Err(error) => {
            let disconnected = error.kind() == std::io::ErrorKind::NotConnected;
            if disconnected && state.native_source_lost && state.native_host.is_none() {
                // No replacement is enrolled. A concurrent submit still holds its guard or
                // publishes a later revision, so acknowledging this read cannot hide it.
                state.native_observed_revision = revision;
                state.native_awaiting_observation = false;
            } else {
                observation.incomplete |=
                    state.native_host.is_some() || state.native_tracking || !disconnected;
            }
        }
    }
    observation.source_lost = state.native_source_lost;
}

fn sample_into(
    state: &mut State,
    sweep: &aterm_sysprobe::Sweep,
    tabs: &[(u64, String)],
    lookup: &dyn Fn(u32) -> Identity,
    epoch: std::time::Instant,
) -> Observation {
    let complete = reconcile(&mut state.ledger, sweep, lookup);
    let jobs = activate_resource_sessions(state, tabs);
    let bindings = state.ledger.bindings();
    Observation {
        procs: Some({
            sweep
                .procs
                .iter()
                .map(|p| crate::vitals_host::proc_sample(p, epoch))
                .collect()
        }),
        delegated: jobs
            .into_iter()
            .filter(|job| state.activated.contains(&job.session))
            .map(|job| aterm_vitals::model::DelegatedJob {
                root: job.root,
                tab: tabs
                    .iter()
                    .find(|(_, session)| *session == job.session)
                    .map(|(local, _)| *local),
                members: bindings
                    .iter()
                    .filter(|binding| binding.root == job.root)
                    .map(|binding| binding.process)
                    .collect(),
            })
            .collect(),
        incomplete: !complete,
        unproved_tabs: if complete {
            Vec::new()
        } else {
            tabs.iter()
                .filter(|(_, sid)| state.activated.contains(sid))
                .map(|(key, _)| *key)
                .collect()
        },
        ..Observation::default()
    }
}

#[cfg(unix)]
fn apply_native(
    state: &mut State,
    observation: &mut Observation,
    native: aterm_resource_host::resource_observation::Observation,
    tabs: &[(u64, String)],
) -> bool {
    use aterm_vitals::model::NativeRoot;
    let Some(receipts) = receipts::Index::new(&native.sessions) else {
        observation.incomplete = true;
        return false;
    };
    let established = state.native_host == Some(native.host);
    if established
        && (native.next_seq < state.native_seq
            || native.lost_origins < state.native_lost_origins
            || receipts.regresses(&state.native_receipts))
    {
        observation.incomplete = true;
        return false;
    }
    if !established {
        state.native_receipts.clear();
    }
    state.native_source_lost |= state.native_host.is_some() && !established
        || established && native.lost_origins > state.native_lost_origins
        || established && native.first_seq > state.native_seq.saturating_add(1)
        || !established
            && state.native_tracking
            && (native.lost_origins > 0
                || native.first_seq > 1
                || native.sessions.iter().any(|receipt| receipt.unread > 0));
    observation.incomplete |= native.unproven_active > 0;
    let tab = |session: &str| {
        tabs.iter()
            .find(|(_, sid)| sid == session)
            .map(|(key, _)| *key)
    };
    if let Some(procs) = observation.procs.as_ref() {
        for original in &native.originals {
            let key = ProcessKey {
                pid: original.process.pid,
                start: original.process.start,
            };
            if procs
                .iter()
                .any(|p| p.pid == key.pid && p.start == Some(key.start))
            {
                observation.native_roots.push(NativeRoot {
                    process: key,
                    tab: tab(&original.session),
                });
            }
        }
        let host = ProcessKey {
            pid: native.host.pid,
            start: native.host.start,
        };
        if procs
            .iter()
            .any(|process| process.pid == host.pid && process.start == Some(host.start))
        {
            observation.native_roots.push(NativeRoot {
                process: host,
                tab: None,
            });
        }
    }
    state.native_source_lost |= state.native_tracking && state.native_tracking_cut.is_none();
    if established || state.native_tracking {
        for terminal in native.terminals.iter().filter(|t| {
            if established {
                t.seq > state.native_seq
            } else {
                t.exit_cut_ns
                    .zip(state.native_tracking_cut)
                    .is_none_or(|(ended, began)| ended >= began)
            }
        }) {
            state
                .native_pending
                .entry((native.host.pid, native.host.start, terminal.seq))
                .or_insert_with(|| PendingNative {
                    host_pid: native.host.pid,
                    host_start: native.host.start,
                    seq: terminal.seq,
                    session: terminal.session.clone(),
                    pid: terminal.process.pid,
                    start: terminal.process.start,
                    uid: terminal.uid,
                    coalition: terminal.coalition,
                    version: terminal.version,
                    cpu_ns: terminal.cpu_ns,
                    child_cpu_ns: terminal.child_cpu_ns,
                    wait_cpu_ns: terminal.wait_cpu_ns,
                    reaps_unwaited: terminal.reaps_unwaited,
                    exit_cut_ns: terminal.exit_cut_ns,
                    window: None,
                });
        }
    }
    for receipt in &native.sessions {
        let counters = (receipt.reaped_cpu_ns, receipt.unread);
        if let Some(previous) = state.native_receipts.get_mut(receipt.session.as_str()) {
            state.native_source_lost |=
                established && (receipt.reaped_cpu_ns < previous.0 || receipt.unread != previous.1);
            *previous = counters;
        } else {
            state
                .native_receipts
                .insert(receipt.session.clone(), counters);
        }
    }
    state.native_host = Some(native.host);
    state.native_seq = native.next_seq;
    state.native_lost_origins = native.lost_origins;
    state.native_outstanding = !native.outstanding_sessions.is_empty();
    state.native_awaiting_observation = false;
    bound_native_queue(state, observation);
    observation.source_lost = state.native_source_lost;
    true
}

#[cfg(unix)]
fn bound_native_queue(state: &mut State, observation: &mut Observation) {
    let excess = state
        .native_pending
        .len()
        .saturating_add(state.native_delivered.len())
        .saturating_sub(65536);
    if excess == 0 {
        return;
    }
    let mut ordered: Vec<_> = state
        .native_pending
        .iter()
        .chain(&state.native_delivered)
        .map(|(key, record)| (record.exit_cut_ns.unwrap_or(0), *key))
        .collect();
    ordered.sort_unstable();
    let unproved: std::collections::BTreeSet<_> = std::mem::take(&mut observation.unproved_tabs)
        .into_iter()
        .collect();
    for (_, key) in ordered.into_iter().take(excess) {
        let _ = state
            .native_pending
            .remove(&key)
            .or_else(|| state.native_delivered.remove(&key));
        state.native_queue_lost = state.native_queue_lost.saturating_add(1);
        state.native_source_lost = true;
    }
    observation.unproved_tabs = unproved.into_iter().collect();
    observation.source_lost = state.native_source_lost;
}

#[cfg(unix)]
fn deliver_native(
    state: &mut State,
    observation: &mut Observation,
    tabs: &[(u64, String)],
    cutoff: aterm_vitals::model::NativeCpuCut,
) {
    use aterm_vitals::model::{NativeRoot, NativeTerminal, ProcSample, ProcState};
    let tab = |session: &str| {
        tabs.iter()
            .find(|(_, sid)| sid == session)
            .map(|(key, _)| *key)
    };
    let mut delivered = Vec::new();
    let previous = state.native_cpu_cut;
    for (key, record) in state
        .native_pending
        .iter_mut()
        .chain(&mut state.native_delivered)
    {
        let process = ProcessKey {
            pid: record.pid,
            start: record.start,
        };
        if observation.procs.as_ref().is_some_and(|procs| {
            procs
                .iter()
                .any(|p| p.pid == process.pid && p.start == Some(process.start))
        }) {
            observation.native_roots.push(NativeRoot {
                process,
                tab: tab(&record.session),
            });
        }
        if record
            .exit_cut_ns
            .zip(cutoff.continuous_ns)
            .is_some_and(|(ended, cut)| ended > cut)
        {
            continue;
        }
        // Unknown timing still has an original final split worth retaining diagnostically.
        // The model consumes its cursor once without assigning it to a CPU rate window.
        state.native_source_lost |= record.exit_cut_ns.is_none() || cutoff.continuous_ns.is_none();
        let window = *record.window.get_or_insert(ReceiptWindow {
            previous: previous.into(),
            current: cutoff.into(),
        });
        observation.native_terminals.push(NativeTerminal {
            host: ProcessKey {
                pid: record.host_pid,
                start: record.host_start,
            },
            seq: record.seq,
            tab: tab(&record.session),
            wait_cpu_ns: record.wait_cpu_ns,
            timing: aterm_vitals::model::NativeCpuWindow {
                previous: window.previous.into(),
                current: window.current.into(),
                exited_ns: record.exit_cut_ns,
            },
            process: ProcSample {
                pid: record.pid,
                start: Some(record.start),
                ppid: record.host_pid,
                cpu_ns: record.cpu_ns,
                child_cpu_ns: record.child_cpu_ns,
                uid: record.uid,
                coalition: record.coalition,
                version: record.version,
                state: ProcState::Zombie,
                reaps_unwaited: record.reaps_unwaited,
                name: "native-job".into(),
                ..ProcSample::default()
            },
        });
        delivered.push(*key);
    }
    for key in delivered {
        if let Some(record) = state.native_pending.remove(&key) {
            state.native_delivered.insert(key, record);
        }
    }
    state.native_unadopted |= !observation.native_terminals.is_empty();
    state.native_cpu_cut = cutoff;
    observation.source_lost = state.native_source_lost;
}

fn activate_resource_sessions(state: &mut State, tabs: &[(u64, String)]) -> Vec<Job> {
    let jobs = state.ledger.jobs();
    state
        .activated
        .retain(|session| jobs.iter().any(|job| &job.session == session));
    for job in &jobs {
        if tabs.iter().any(|(_, session)| session == &job.session) {
            state.activated.insert(job.session.clone());
        }
    }
    jobs
}

fn reconcile(
    ledger: &mut Ledger,
    sweep: &aterm_sysprobe::Sweep,
    lookup: &dyn Fn(u32) -> Identity,
) -> bool {
    if sweep.truncated {
        return false;
    }
    let mut processes: Vec<_> = sweep
        .procs
        .iter()
        .filter_map(|process| {
            Some(Process {
                key: ProcessKey {
                    pid: process.pid,
                    start: process.start?,
                },
                parent: process.ppid,
            })
        })
        .collect();
    let present: HashSet<_> = processes.iter().map(|process| process.key).collect();
    let known: HashSet<_> = ledger
        .jobs()
        .into_iter()
        .map(|job| job.root)
        .chain(ledger.bindings().into_iter().map(|binding| binding.process))
        .collect();
    let mut repairs = Vec::new();
    let mut unavailable = HashSet::new();
    let mut current = HashMap::new();
    for expected in known {
        if !present.contains(&expected) {
            match *current
                .entry(expected.pid)
                .or_insert_with(|| lookup(expected.pid))
            {
                Identity::Known(now) if now == expected => repairs.push(Process {
                    key: expected,
                    parent: 0,
                }),
                Identity::Unavailable => {
                    unavailable.insert(expected);
                }
                Identity::Known(_) | Identity::Absent => {}
            }
        }
    }
    let replaced: HashSet<_> = repairs.iter().map(|process| process.key.pid).collect();
    processes.retain(|process| !replaced.contains(&process.key.pid));
    let complete = repairs.is_empty() && unavailable.is_empty();
    processes.extend(repairs);
    let bindings = ledger.observe_with_unavailable(&processes, &unavailable);
    let readable: HashSet<_> = sweep
        .procs
        .iter()
        .filter_map(|process| {
            Some(ProcessKey {
                pid: process.pid,
                start: process
                    .start
                    .filter(|_| process.cpu_ns.is_some() && process.footprint.is_some())?,
            })
        })
        .collect();
    complete
        && bindings
            .iter()
            .all(|binding| readable.contains(&binding.process))
}

/// Publish exactly the job totals folded into the normal aterm/tab snapshot.
pub(crate) fn publish(usage: Option<Vec<aterm_vitals::model::DelegatedUsage>>) {
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    publish_into(&mut state, usage, Instant::now());
}

fn publish_into(
    state: &mut State,
    usage: Option<Vec<aterm_vitals::model::DelegatedUsage>>,
    now: Instant,
) {
    state.current_complete = usage.is_some();
    if let Some(usage) = usage {
        state.usage = usage;
        state.sampled = Some(now);
    }
}

#[derive(Serialize, Deserialize)]
struct Carry {
    boot: [u8; 16],
    jobs: Vec<CarriedJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native: Option<NativeCarry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    origins: Vec<CarriedOrigin>,
    #[serde(default)]
    origins_incomplete: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CarriedOrigin {
    pid: u32,
    start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    shared: bool,
}

/// Consume a handoff inventory once. A stable SID is the only translation between GUI-local
/// pane keys; a closed session retains app ownership without acquiring a different tab.
pub(crate) fn restore_origins(
    vitals: &mut aterm_vitals::Vitals,
    sessions: &[(u64, String)],
) -> bool {
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    restore_origins_into(&mut state, vitals, sessions)
}

fn restore_origins_into(
    state: &mut State,
    vitals: &mut aterm_vitals::Vitals,
    sessions: &[(u64, String)],
) -> bool {
    if std::mem::take(&mut state.origins_restore_pending) {
        let origins: Vec<_> = state
            .origins
            .iter()
            .map(|origin| aterm_vitals::model::ProcessOrigin {
                process: ProcessKey {
                    pid: origin.pid,
                    start: origin.start,
                },
                tab: origin.session.as_ref().and_then(|sid| {
                    sessions
                        .iter()
                        .find(|(_, session)| session == sid)
                        .map(|(key, _)| *key)
                }),
                shared: origin.shared,
            })
            .collect();
        vitals.restore_process_origins(&origins);
    }
    state.origins_incomplete
}

/// Publish the last real model's generation proofs into the existing private handoff owner.
pub(crate) fn publish_origins(
    inventory: (Vec<aterm_vitals::model::ProcessOrigin>, bool),
    sessions: &[(u64, String)],
) {
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    publish_origins_into(&mut state, inventory, sessions);
}

fn publish_origins_into(
    state: &mut State,
    (origins, truncated): (Vec<aterm_vitals::model::ProcessOrigin>, bool),
    sessions: &[(u64, String)],
) {
    state.origins_incomplete |=
        truncated || origins.len() > aterm_vitals::model::PROCESS_ORIGIN_CAP;
    // A reading can finish after handoff intake. Its old model must not replace proofs
    // that the next reading has yet to adopt.
    if state.origins_restore_pending {
        return;
    }
    state.origins = origins
        .into_iter()
        .take(aterm_vitals::model::PROCESS_ORIGIN_CAP)
        .map(|origin| CarriedOrigin {
            pid: origin.process.pid,
            start: origin.process.start,
            shared: origin.shared,
            session: origin.tab.and_then(|key| {
                sessions
                    .iter()
                    .find(|(pane, _)| *pane == key)
                    .map(|(_, sid)| sid.clone())
            }),
        })
        .collect();
}

fn restore_origin_carry(state: &mut State, carry: &Carry, current_boot: Option<[u8; 16]>) {
    if Some(carry.boot) != current_boot {
        if current_boot.is_none() {
            state.origins_incomplete |= !carry.origins.is_empty() || carry.origins_incomplete;
        }
        return;
    }
    state.origins_incomplete |=
        carry.origins_incomplete || carry.origins.len() > aterm_vitals::model::PROCESS_ORIGIN_CAP;
    let mut claims: std::collections::BTreeMap<_, CarriedOrigin> =
        std::collections::BTreeMap::new();
    let mut rejected = HashSet::new();
    for origin in carry
        .origins
        .iter()
        .take(aterm_vitals::model::PROCESS_ORIGIN_CAP)
    {
        let key = (origin.pid, origin.start);
        if rejected.contains(&key) {
            continue;
        }
        let valid = origin.pid > 1
            && origin
                .session
                .as_ref()
                .is_none_or(|sid| aterm_types::control_request::Sid::parse(sid).is_some())
            && !(origin.shared && origin.session.is_some());
        state.origins_incomplete |= !valid;
        if !valid {
            rejected.insert(key);
            claims.remove(&key);
            continue;
        }
        if claims
            .get(&key)
            .is_some_and(|previous| *previous != *origin)
        {
            claims.remove(&key);
            rejected.insert(key);
            state.origins_incomplete = true;
        } else {
            claims.insert(key, origin.clone());
        }
    }
    state.origins = claims.into_values().collect();
    state.origins_restore_pending = true;
}

#[derive(Serialize, Deserialize)]
struct NativeCarry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start: Option<u64>,
    seq: u64,
    lost_origins: u64,
    #[serde(default)]
    pending: Vec<PendingNative>,
    #[serde(default)]
    queue_lost: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tracking_cut: Option<u64>,
    #[serde(default)]
    tracking: bool,
    #[serde(default)]
    outstanding: bool,
    #[serde(default)]
    awaiting_observation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observer: Option<aterm_json::Value>,
    #[serde(default)]
    source_lost: bool,
    #[serde(default)]
    cpu_cut: CarriedCpuCut,
}

#[cfg(unix)]
fn carry_native(state: &State, submitting: usize, revision: u64) -> Option<NativeCarry> {
    let awaiting_observation = state.native_awaiting_observation
        || submitting > 0
        || revision != state.native_observed_revision;
    if state.native_host.is_none()
        && !state.native_tracking
        && state.native_pending.is_empty()
        && state.native_delivered.is_empty()
        && !awaiting_observation
        && !state.native_source_lost
    {
        return None;
    }
    Some(NativeCarry {
        pid: state.native_host.map(|host| host.pid),
        start: state.native_host.map(|host| host.start),
        seq: state.native_seq,
        lost_origins: state.native_lost_origins,
        pending: state
            .native_pending
            .values()
            .chain(state.native_delivered.values())
            .cloned()
            .collect(),
        queue_lost: state.native_queue_lost,
        tracking_cut: state.native_tracking_cut,
        tracking: state.native_tracking,
        outstanding: state.native_outstanding,
        awaiting_observation,
        observer: None,
        source_lost: state.native_source_lost,
        cpu_cut: state.native_cpu_cut.into(),
    })
}

#[derive(Serialize, Deserialize)]
struct CarriedJob {
    pid: u32,
    start: u64,
    session: String,
    ai: String,
    label: String,
    #[serde(default)]
    active: bool,
    members: Vec<(u32, u64)>,
}

fn boot() -> Option<[u8; 16]> {
    astream_sys::boot_session()
}

/// Encodes the ledger's delegated jobs, each with its activation, for seamless handoff.
#[cfg(any(unix, test))]
pub(crate) fn carry() -> Option<aterm_json::Value> {
    let state = state().lock().unwrap_or_else(|p| p.into_inner());
    #[cfg(unix)]
    let mut native = carry_native(
        &state,
        NATIVE_SUBMITTING.load(std::sync::atomic::Ordering::Acquire),
        NATIVE_REVISION.load(std::sync::atomic::Ordering::Acquire),
    );
    #[cfg(unix)]
    if let Some(native) = &mut native {
        native.observer = aterm_resource_host::carry_resource_observer();
    }
    #[cfg(not(unix))]
    let native = None;
    if state.ledger.is_empty()
        && native.is_none()
        && state.origins.is_empty()
        && !state.origins_incomplete
    {
        return None;
    }
    let bindings = state.ledger.bindings();
    let jobs = state
        .ledger
        .jobs()
        .into_iter()
        .map(|job| CarriedJob {
            pid: job.root.pid,
            start: job.root.start,
            active: state.activated.contains(&job.session),
            session: job.session,
            ai: job.ai,
            label: job.label,
            members: bindings
                .iter()
                .filter(|binding| binding.root == job.root)
                .map(|binding| (binding.process.pid, binding.process.start))
                .collect(),
        })
        .collect();
    encode_carry(Carry {
        boot: boot()?,
        jobs,
        native,
        origins: state.origins.clone(),
        origins_incomplete: state.origins_incomplete,
    })
}

#[cfg(any(unix, test))]
fn encode_carry(carry: Carry) -> Option<aterm_json::Value> {
    let value = aterm_json::to_value(carry).ok()?;
    aterm_toml::to_string(&value).ok()?;
    Some(value)
}

/// Encodes one explicit producer root for a release fixture. Product carry
/// still obtains both identities from the running kernel in [`carry`].
#[cfg(all(test, unix))]
pub(crate) fn fixture_carry(
    session: &str,
    fixture_boot: [u8; 16],
    root: ProcessKey,
) -> aterm_json::Value {
    encode_carry(Carry {
        boot: fixture_boot,
        native: None,
        origins: Vec::new(),
        origins_incomplete: false,
        jobs: vec![CarriedJob {
            pid: root.pid,
            start: root.start,
            session: session.to_string(),
            ai: "codex".into(),
            label: "handoff:fixture".into(),
            active: true,
            members: vec![(root.pid, root.start)],
        }],
    })
    .expect("fixture identity fits the handoff format")
}

/// Restores carried delegated jobs matching the current boot and live process identities.
pub(crate) fn restore(value: aterm_json::Value) {
    #[cfg(unix)]
    let native = if let Ok(carried) = aterm_json::from_value::<Carry>(value.clone())
        && Some(carried.boot) == boot()
    {
        carried.native
    } else {
        None
    };
    #[cfg(unix)]
    if let Some(observer) = native.as_ref().and_then(|native| native.observer.as_ref()) {
        let _ = aterm_resource_host::restore_resource_observer(observer);
    }
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    if let Ok(carried) = aterm_json::from_value::<Carry>(value.clone()) {
        restore_origin_carry(&mut state, &carried, boot());
    }
    #[cfg(unix)]
    if let Some(native) = native {
        restore_native(&mut state, native, &key);
    }
    let active = restore_into(&mut state.ledger, value, boot(), &key);
    state.activated.extend(active);
}

#[cfg(unix)]
fn restore_native(
    state: &mut State,
    native: NativeCarry,
    lookup: &dyn Fn(u32) -> Option<ProcessKey>,
) {
    let host = native
        .pid
        .zip(native.start)
        .map(|(pid, start)| ProcessKey { pid, start });
    if let Some(host) = host.filter(|host| host.pid > 1 && lookup(host.pid) == Some(*host)) {
        state.native_host = Some(aterm_resource_host::resource_observation::ProcessKey {
            pid: host.pid,
            start: host.start,
        });
        state.native_seq = native.seq;
        state.native_lost_origins = native.lost_origins;
        state.native_outstanding = native.outstanding;
    }
    state.native_tracking_cut = native.tracking_cut;
    state.native_tracking = native.tracking || native.tracking_cut.is_some();
    state.native_awaiting_observation |= native.awaiting_observation;
    state.native_queue_lost = native.queue_lost;
    state.native_source_lost |= native.source_lost;
    state.native_cpu_cut = native.cpu_cut.into();
    // An owning-wait receipt is historical evidence. Its custodian may have exited; this
    // does not turn it into a live PID claim or invalidate already accepted terminal custody.
    for pending in native.pending.into_iter().take(65536) {
        if aterm_types::control_request::Sid::parse(&pending.session).is_some() {
            state
                .native_pending
                .insert((pending.host_pid, pending.host_start, pending.seq), pending);
        }
    }
}

fn restore_into(
    ledger: &mut Ledger,
    value: aterm_json::Value,
    current_boot: Option<[u8; 16]>,
    lookup: &dyn Fn(u32) -> Option<ProcessKey>,
) -> Vec<String> {
    let Ok(carry) = aterm_json::from_value::<Carry>(value) else {
        return Vec::new();
    };
    if Some(carry.boot) != current_boot || carry.jobs.len() > aterm_vitals::delegated::MAX_JOBS {
        return Vec::new();
    }
    if carry
        .jobs
        .iter()
        .try_fold(0usize, |sum, job| sum.checked_add(job.members.len()))
        .is_none_or(|total| total > aterm_vitals::delegated::MAX_MEMBERS)
    {
        return Vec::new();
    }
    let mut activated = Vec::new();
    for carried in carry.jobs {
        if aterm_types::control_request::Sid::parse(&carried.session).is_none()
            || !word(&carried.ai)
            || !label(&carried.label)
        {
            continue;
        }
        let root = ProcessKey {
            pid: carried.pid,
            start: carried.start,
        };
        let members: Vec<_> = carried
            .members
            .into_iter()
            .filter_map(|(pid, start)| {
                let expected = ProcessKey { pid, start };
                (lookup(pid) == Some(expected)).then_some(expected)
            })
            .collect();
        if lookup(root.pid) != Some(root) && members.is_empty() {
            continue;
        }
        let session = carried.session.clone();
        if ledger
            .register(Job {
                root,
                session: carried.session,
                ai: carried.ai,
                label: carried.label,
            })
            .is_err()
        {
            continue;
        }
        if carried.active {
            activated.push(session);
        }
        for process in members {
            ledger.restore(Binding { process, root });
        }
    }
    activated
}

#[cfg(test)]
#[path = "resource_jobs_tests.rs"]
mod tests;
