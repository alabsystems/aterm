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

#[derive(Default)]
struct State {
    ledger: Ledger,
    activated: HashSet<String>,
    usage: Vec<aterm_vitals::model::DelegatedUsage>,
    sampled: Option<Instant>,
    current_complete: bool,
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
    !state()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ledger
        .is_empty()
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
}

/// Folds process identities from `sweep` into the delegated ledger.
pub(crate) fn sample(
    sweep: &aterm_sysprobe::Sweep,
    tabs: &[(u64, String)],
    epoch: std::time::Instant,
) -> Observation {
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    sample_into(&mut state, sweep, tabs, &identity, epoch)
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
        procs: complete.then(|| {
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
    }
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
    if state.ledger.is_empty() {
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
    let mut state = state().lock().unwrap_or_else(|p| p.into_inner());
    let active = restore_into(&mut state.ledger, value, boot(), &key);
    state.activated.extend(active);
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
