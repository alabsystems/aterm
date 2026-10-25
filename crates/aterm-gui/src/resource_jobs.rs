// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated delegated jobs.

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
                "OK resources version=1 available={} self_register=peer-pid-start group_signals=exec-inherit runner={} ai={} session={}\n",
                u8::from(supported),
                aterm_codec::pct::encode(runner.unwrap_or("-")),
                program(ctx),
                ctx.self_id.as_str()
            )
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
                    if expected_start.is_some() {
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
    "ERR usage: resources [capabilities|register start=<kernel-start> [ai=<word>] job=<percent-encoded-label>]\n".into()
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

/// Encodes the release fixture's live producer root for seamless handoff tests.
#[cfg(all(test, unix))]
pub(crate) fn live_fixture_carry(session: &str) -> aterm_json::Value {
    let root = key(std::process::id()).expect("fixture producer has a live kernel identity");
    encode_carry(Carry {
        boot: boot().expect("fixture producer has a boot identity"),
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
    .expect("live fixture identity fits the handoff format")
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
mod tests {
    use super::*;

    fn carried(root: ProcessKey, member: ProcessKey) -> aterm_json::Value {
        aterm_json::to_value(Carry {
            boot: [7; 16],
            jobs: vec![CarriedJob {
                pid: root.pid,
                start: root.start,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "project:compiler".into(),
                active: true,
                members: vec![(member.pid, member.start)],
            }],
        })
        .unwrap()
    }

    fn changed_carry(root: ProcessKey, change: impl FnOnce(&mut CarriedJob)) -> aterm_json::Value {
        let mut carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
        change(&mut carry.jobs[0]);
        aterm_json::to_value(carry).unwrap()
    }

    #[test]
    fn capabilities_default_to_the_current_observed_program_not_a_previous_agent() {
        let session = crate::stub_session(0);
        for observed in [Some("codex"), Some("claude"), Some("zsh"), None] {
            {
                let mut timeline = session.ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(42);
                timeline.set_program(42, observed.map(str::to_string));
            }
            let expected = observed.unwrap_or("unknown");
            assert_eq!(program(&session.ctx), expected);
            let reply = command(&session.ctx, true, "capabilities");
            assert!(
                reply
                    .split_whitespace()
                    .any(|part| part == format!("ai={expected}"))
            );
        }
    }

    fn claimed_session() -> crate::Session {
        let session = crate::stub_session(0);
        let mut meta = session.ctx.meta.lock().unwrap();
        meta.supervisor = Some(crate::session_timeline::SupervisorClaim {
            holder: "codex-alab@42".into(),
            conn: Some(1),
            expires_us: None,
            proven: Some(crate::self_claim::Proven {
                pid: 42,
                start: "fixture birth".into(),
            }),
            scoped: None,
        });
        meta.supervisor_revision = 1;
        drop(meta);
        session
    }

    #[test]
    fn a_live_proven_foreground_claim_fills_an_unresolved_resource_program() {
        let session = claimed_session();
        let ctx = &session.ctx;
        assert_eq!(
            program_by(
                ctx,
                || {
                    assert!(ctx.meta.try_lock().is_ok());
                    assert!(ctx.timeline.try_lock().is_ok());
                    42
                },
                |proof| {
                    assert_eq!(proof.pid, 42);
                    assert_eq!(proof.start, "fixture birth");
                    assert!(ctx.meta.try_lock().is_ok());
                    assert!(ctx.timeline.try_lock().is_ok());
                    true
                },
                |pid| {
                    assert_eq!(pid, 42);
                    assert!(ctx.meta.try_lock().is_ok());
                    assert!(ctx.timeline.try_lock().is_ok());
                    true
                },
            ),
            "codex"
        );
    }

    #[test]
    fn resource_program_needs_proof_and_a_current_process_image_and_foreground() {
        for failure in [
            "unproven",
            "expired",
            "dead",
            "reused",
            "image",
            "background",
            "unreadable",
        ] {
            let session = claimed_session();
            let ctx = &session.ctx;
            {
                let mut meta = ctx.meta.lock().unwrap();
                let claim = meta.supervisor.as_mut().unwrap();
                match failure {
                    "unproven" => claim.proven = None,
                    "expired" => claim.expires_us = Some(0),
                    _ => {}
                }
            }
            assert_eq!(
                program_by(
                    ctx,
                    || match failure {
                        "background" => 43,
                        "unreadable" => -1,
                        _ => 42,
                    },
                    |_| !matches!(failure, "dead" | "reused"),
                    |_| failure != "image",
                ),
                "unknown",
                "{failure}"
            );
        }
        let session = crate::stub_session(0);
        assert_eq!(
            program_by(
                &session.ctx,
                || panic!("no proof"),
                |_| panic!("no proof"),
                |_| panic!("no proof")
            ),
            "unknown"
        );
    }

    #[test]
    fn resource_program_rejects_identity_image_or_foreground_lost_during_probes() {
        for changed in ["birth", "image", "foreground"] {
            let session = claimed_session();
            let mut identities = 0;
            let mut images = 0;
            let mut foregrounds = 0;
            assert_eq!(
                program_by(
                    &session.ctx,
                    || {
                        foregrounds += 1;
                        if changed == "foreground" && foregrounds == 2 {
                            43
                        } else {
                            42
                        }
                    },
                    |_| {
                        identities += 1;
                        changed != "birth" || identities == 1
                    },
                    |_| {
                        images += 1;
                        changed != "image" || images == 1
                    },
                ),
                "unknown",
                "{changed}"
            );
        }
    }

    #[test]
    fn resource_program_rechecks_claim_revision_proof_and_expiry_after_probes() {
        for changed in ["renewed", "replaced", "released", "expired"] {
            let session = claimed_session();
            let ctx = &session.ctx;
            assert_eq!(
                program_by(
                    ctx,
                    || {
                        let mut meta = ctx.meta.lock().unwrap();
                        match changed {
                            "renewed" => meta.supervisor_revision += 1,
                            "replaced" => {
                                meta.supervisor
                                    .as_mut()
                                    .unwrap()
                                    .proven
                                    .as_mut()
                                    .unwrap()
                                    .start = "another birth".into()
                            }
                            "released" => meta.supervisor = None,
                            "expired" => meta.supervisor.as_mut().unwrap().expires_us = Some(0),
                            _ => unreachable!(),
                        }
                        42
                    },
                    |_| true,
                    |_| true,
                ),
                "unknown",
                "{changed}"
            );
        }
    }

    #[test]
    fn resource_program_preserves_observed_names_including_names_resolved_during_probes() {
        for name in ["codex", "claude", "zsh"] {
            let session = claimed_session();
            let ctx = &session.ctx;
            let observe = || {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.note_foreground_group(42);
                timeline.set_program(42, Some(name.into()));
            };
            assert_eq!(
                program_by(
                    ctx,
                    || 42,
                    |_| true,
                    |_| {
                        observe();
                        true
                    }
                ),
                name
            );
            assert_eq!(
                program_by(
                    ctx,
                    || panic!("already observed"),
                    |_| panic!("already observed"),
                    |_| panic!("already observed")
                ),
                name
            );
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                timeline.set_program(42, None);
            }
            assert_eq!(
                program_by(
                    ctx,
                    || 42,
                    |_| true,
                    |_| {
                        observe();
                        false
                    }
                ),
                name,
                "a resolved name also wins when the proof's image is gone"
            );
        }
    }

    #[test]
    fn activation_is_bounded_under_tab_churn_and_keeps_closed_tabs_live_jobs() {
        let mut state = State::default();
        for id in 0..1000 {
            activate_resource_sessions(&mut state, &[(id, format!("s-{id:x}"))]);
        }
        assert!(state.activated.is_empty());
        let root = ProcessKey { pid: 20, start: 2 };
        state
            .ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
        activate_resource_sessions(&mut state, &[(10, "s-a".into())]);
        activate_resource_sessions(&mut state, &[]);
        assert_eq!(state.activated, HashSet::from(["s-a".to_string()]));
        state.ledger.observe(&[]);
        activate_resource_sessions(&mut state, &[]);
        assert!(state.activated.is_empty());
    }

    #[test]
    fn a_carried_closed_tabs_authorized_job_stays_active_but_new_jobs_wait_for_hosting() {
        let root = ProcessKey { pid: 20, start: 2 };
        let current = |pid| (pid == root.pid).then_some(root);
        let mut state = State::default();
        let active = restore_into(
            &mut state.ledger,
            carried(root, root),
            Some([7; 16]),
            &current,
        );
        state.activated.extend(active);
        activate_resource_sessions(&mut state, &[]);
        assert!(state.activated.contains("s-a"));
        let mut fresh = State::default();
        let value = changed_carry(root, |job| job.active = false);
        let active = restore_into(&mut fresh.ledger, value, Some([7; 16]), &current);
        fresh.activated.extend(active);
        activate_resource_sessions(&mut fresh, &[]);
        assert!(fresh.activated.is_empty());
        activate_resource_sessions(&mut fresh, &[(10, "s-a".into())]);
        assert!(fresh.activated.contains("s-a"));
    }

    #[test]
    fn unrepresentable_resource_carry_is_omitted_before_terminal_serialization() {
        let root = ProcessKey {
            pid: 20,
            start: u64::MAX,
        };
        let carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
        assert!(encode_carry(carry).is_none());
        let root = ProcessKey {
            pid: 20,
            start: i64::MAX as u64,
        };
        let carry: Carry = aterm_json::from_value(carried(root, root)).unwrap();
        assert!(encode_carry(carry).is_some());
    }

    #[test]
    fn registration_requires_the_callers_expected_start_and_two_matching_kernel_reads() {
        let root = ProcessKey { pid: 20, start: 2 };
        assert!(confirmed_peer(root, Some(2), Some(root)));
        assert!(!confirmed_peer(root, None, Some(root)));
        assert!(!confirmed_peer(root, Some(1), Some(root)));
        assert!(!confirmed_peer(root, Some(2), None));
        assert!(!confirmed_peer(
            root,
            Some(2),
            Some(ProcessKey { pid: 20, start: 3 })
        ));
    }

    #[test]
    fn late_registered_root_replaces_the_sweeps_stale_pid_identity() {
        let root = ProcessKey { pid: 20, start: 2 };
        let mut ledger = Ledger::default();
        ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
        let sweep = aterm_sysprobe::Sweep {
            procs: vec![aterm_sysprobe::ProcessReading {
                pid: 20,
                start: Some(1),
                ppid: 1,
                ..Default::default()
            }],
            truncated: false,
        };
        assert!(!reconcile(&mut ledger, &sweep, &|_| Identity::Known(root)));
        assert_eq!(ledger.jobs().len(), 1);
        assert_eq!(
            ledger.bindings(),
            vec![Binding {
                process: root,
                root
            }]
        );
        reconcile(&mut ledger, &aterm_sysprobe::Sweep::default(), &|_| {
            Identity::Absent
        });
        assert!(ledger.is_empty());
    }

    #[test]
    fn truncated_sweep_retains_ownership_and_never_refreshes_old_usage_as_zero() {
        let root = ProcessKey { pid: 20, start: 2 };
        let mut state = State::default();
        state
            .ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
        state.ledger.restore(Binding {
            process: root,
            root,
        });
        let usage = vec![aterm_vitals::model::DelegatedUsage {
            root,
            cores: 1.0,
            footprint: Some(4096),
            procs: 1,
        }];
        let sampled = Instant::now();
        publish_into(&mut state, Some(usage.clone()), sampled);
        reconcile(
            &mut state.ledger,
            &aterm_sysprobe::Sweep {
                truncated: true,
                ..Default::default()
            },
            &|_| Identity::Absent,
        );
        publish_into(
            &mut state,
            None,
            sampled + std::time::Duration::from_secs(5),
        );
        assert_eq!(
            state.ledger.bindings(),
            vec![Binding {
                process: root,
                root
            }]
        );
        assert_eq!(state.usage, usage);
        assert_eq!(state.sampled, Some(sampled));
        assert!(!state.current_complete);
    }

    fn reading(key: ProcessKey, parent: u32, cpu_ns: u64) -> aterm_sysprobe::ProcessReading {
        aterm_sysprobe::ProcessReading {
            pid: key.pid,
            start: Some(key.start),
            ppid: parent,
            cpu_ns: Some(cpu_ns),
            footprint: Some(u64::from(key.pid) * 1024),
            ..Default::default()
        }
    }

    fn sampled(
        state: &mut State,
        vitals: &mut aterm_vitals::Vitals,
        sweep: &aterm_sysprobe::Sweep,
        lookup: &dyn Fn(u32) -> Identity,
        second: u64,
        epoch: Instant,
    ) -> aterm_vitals::model::Snapshot {
        let observed = sample_into(state, sweep, &[(1, "s-a".into())], lookup, epoch);
        let complete = observed.procs.is_some();
        let snapshot = vitals.absorb(&aterm_vitals::model::Sample {
            at_ns: second * 1_000_000_000,
            at_unix_ms: second as i64 * 1000,
            mono_ms: second * 1000,
            cadence_ms: 1000,
            self_pid: 100,
            tabs: vec![aterm_vitals::model::TabRoot {
                key: 1,
                tab: 1,
                shell_pid: 200,
                label: "Tab 1".into(),
                program: "codex".into(),
            }],
            procs: observed.procs,
            delegated: observed.delegated,
            ..Default::default()
        });
        publish_into(
            state,
            complete.then(|| snapshot.delegated.clone()),
            epoch + std::time::Duration::from_secs(second),
        );
        snapshot
    }

    #[test]
    fn unreadable_retained_members_keep_ownership_and_old_age_until_complete_recovery() {
        let root = ProcessKey { pid: 20, start: 2 };
        let member = ProcessKey { pid: 21, start: 3 };
        for root_live in [false, true] {
            for (failure, fresh_available) in [
                ("omitted", false),
                ("start", false),
                ("omitted", true),
                ("start", true),
                ("cpu", true),
                ("footprint", true),
            ] {
                let mut state = State::default();
                state
                    .ledger
                    .register(Job {
                        root,
                        session: "s-a".into(),
                        ai: "codex".into(),
                        label: "build".into(),
                    })
                    .unwrap();
                let mut vitals = aterm_vitals::Vitals::new();
                let epoch = Instant::now();
                let initial = aterm_sysprobe::Sweep {
                    procs: vec![reading(root, 10, 0), reading(member, root.pid, 0)],
                    truncated: false,
                };
                sampled(
                    &mut state,
                    &mut vitals,
                    &initial,
                    &|pid| {
                        if pid == root.pid {
                            Identity::Known(root)
                        } else {
                            Identity::Absent
                        }
                    },
                    1,
                    epoch,
                );
                let healthy = |second: u64| aterm_sysprobe::Sweep {
                    procs: if root_live {
                        vec![
                            reading(root, 10, second * 1_000_000_000),
                            reading(member, root.pid, second * 1_000_000_000),
                        ]
                    } else {
                        vec![reading(member, 1, second * 1_000_000_000)]
                    },
                    truncated: false,
                };
                let lookup = |pid| {
                    if pid == member.pid {
                        Identity::Known(member)
                    } else if root_live && pid == root.pid {
                        Identity::Known(root)
                    } else {
                        Identity::Absent
                    }
                };
                sampled(&mut state, &mut vitals, &healthy(2), &lookup, 2, epoch);
                assert!(state.current_complete);
                assert!(state.usage[0].cores > 0.0);
                let old_usage = state.usage.clone();
                let old_age = state.sampled;
                let old_bindings = state.ledger.bindings();
                let mut unreadable = healthy(3);
                match failure {
                    "omitted" => unreadable.procs.retain(|p| p.pid != member.pid),
                    _ => {
                        let p = unreadable
                            .procs
                            .iter_mut()
                            .find(|p| p.pid == member.pid)
                            .unwrap();
                        match failure {
                            "start" => p.start = None,
                            "cpu" => p.cpu_ns = None,
                            "footprint" => p.footprint = None,
                            _ => unreachable!(),
                        }
                    }
                }
                let partial_lookup = |pid| {
                    if !fresh_available && pid == member.pid {
                        Identity::Unavailable
                    } else {
                        lookup(pid)
                    }
                };
                let partial = sampled(
                    &mut state,
                    &mut vitals,
                    &unreadable,
                    &partial_lookup,
                    3,
                    epoch,
                );
                assert!(
                    partial.aterm.is_none(),
                    "{root_live}/{failure}/fresh={fresh_available} cannot publish reduced process totals"
                );
                assert!(partial.delegated.is_empty());
                assert_eq!(state.ledger.bindings(), old_bindings);
                assert_eq!(state.usage, old_usage);
                assert_eq!(state.sampled, old_age);
                assert!(!state.current_complete);
                let recovered = sampled(&mut state, &mut vitals, &healthy(4), &lookup, 4, epoch);
                let count = if root_live { 2 } else { 1 };
                assert!(state.current_complete);
                assert_eq!(state.ledger.bindings(), old_bindings);
                assert_eq!(state.usage.len(), 1);
                assert_eq!(state.usage[0].procs, count);
                assert_eq!(recovered.aterm.as_ref().unwrap().procs, count);
                assert_eq!(recovered.tab(1).unwrap().scope.procs, count);
                assert_eq!(
                    state.sampled,
                    Some(epoch + std::time::Duration::from_secs(4))
                );
            }
        }
    }

    #[test]
    fn fresh_reused_member_identity_never_repairs_old_detached_ownership() {
        let root = ProcessKey { pid: 20, start: 2 };
        let member = ProcessKey { pid: 21, start: 3 };
        let reused = ProcessKey {
            pid: member.pid,
            start: 9,
        };
        let mut ledger = Ledger::default();
        ledger
            .register(Job {
                root,
                session: "s-a".into(),
                ai: "codex".into(),
                label: "build".into(),
            })
            .unwrap();
        ledger.observe(&[
            Process {
                key: root,
                parent: 10,
            },
            Process {
                key: member,
                parent: root.pid,
            },
        ]);
        ledger.observe(&[Process {
            key: member,
            parent: 1,
        }]);
        assert!(reconcile(
            &mut ledger,
            &aterm_sysprobe::Sweep::default(),
            &|pid| if pid == reused.pid {
                Identity::Known(reused)
            } else {
                Identity::Absent
            }
        ));
        assert!(ledger.is_empty());
        assert!(ledger.bindings().is_empty());
        reconcile(
            &mut ledger,
            &aterm_sysprobe::Sweep {
                procs: vec![reading(reused, 1, 1)],
                truncated: false,
            },
            &|_| Identity::Known(reused),
        );
        assert!(ledger.bindings().is_empty());
    }

    #[test]
    fn confirmed_exit_or_reuse_prunes_previously_unavailable_detached_members() {
        let root = ProcessKey { pid: 20, start: 2 };
        let member = ProcessKey { pid: 21, start: 3 };
        for later in [
            Identity::Absent,
            Identity::Known(ProcessKey {
                pid: member.pid,
                start: 9,
            }),
        ] {
            let mut ledger = Ledger::default();
            ledger
                .register(Job {
                    root,
                    session: "s-a".into(),
                    ai: "codex".into(),
                    label: "build".into(),
                })
                .unwrap();
            ledger.observe(&[
                Process {
                    key: root,
                    parent: 10,
                },
                Process {
                    key: member,
                    parent: root.pid,
                },
            ]);
            ledger.observe(&[Process {
                key: member,
                parent: 1,
            }]);
            assert!(!reconcile(
                &mut ledger,
                &aterm_sysprobe::Sweep::default(),
                &|pid| if pid == member.pid {
                    Identity::Unavailable
                } else {
                    Identity::Absent
                }
            ));
            assert_eq!(
                ledger.bindings(),
                vec![Binding {
                    process: member,
                    root
                }]
            );
            reconcile(&mut ledger, &aterm_sysprobe::Sweep::default(), &|pid| {
                if pid == member.pid {
                    later
                } else {
                    Identity::Absent
                }
            });
            assert!(ledger.is_empty());
            assert!(ledger.bindings().is_empty());
        }
    }

    #[test]
    fn missing_old_and_new_roots_at_one_pid_share_one_fresh_identity_read() {
        let old = ProcessKey { pid: 20, start: 2 };
        let new = ProcessKey { pid: 20, start: 9 };
        let child = ProcessKey { pid: 21, start: 3 };
        let mut ledger = Ledger::default();
        for root in [old, new] {
            ledger
                .register(Job {
                    root,
                    session: "s-a".into(),
                    ai: "codex".into(),
                    label: "build".into(),
                })
                .unwrap();
        }
        ledger.restore(Binding {
            process: child,
            root: old,
        });
        let reads = std::cell::Cell::new(0);
        assert!(!reconcile(
            &mut ledger,
            &aterm_sysprobe::Sweep::default(),
            &|pid| {
                if pid == new.pid {
                    reads.set(reads.get() + 1);
                    Identity::Known(new)
                } else if pid == child.pid {
                    Identity::Known(child)
                } else {
                    Identity::Absent
                }
            }
        ));
        assert_eq!(reads.get(), 1);
        assert_eq!(ledger.jobs().len(), 2);
        assert_eq!(
            ledger.bindings(),
            vec![
                Binding {
                    process: new,
                    root: new
                },
                Binding {
                    process: child,
                    root: old
                }
            ]
        );
    }

    #[test]
    fn handoff_keeps_live_detached_members_and_refuses_other_boots_and_reused_pids() {
        let root = ProcessKey { pid: 20, start: 2 };
        let member = ProcessKey { pid: 21, start: 3 };
        let current = |pid| (pid == member.pid).then_some(member);
        let mut ledger = Ledger::default();
        restore_into(&mut ledger, carried(root, member), Some([7; 16]), &current);
        assert_eq!(ledger.jobs().len(), 1);
        assert_eq!(
            ledger.bindings(),
            vec![Binding {
                process: member,
                root
            }]
        );
        assert_eq!(ledger.jobs()[0].session, "s-a");
        let mut other = Ledger::default();
        restore_into(&mut other, carried(root, member), Some([8; 16]), &current);
        assert!(other.is_empty());
        restore_into(&mut other, carried(root, member), Some([7; 16]), &|pid| {
            Some(ProcessKey { pid, start: 99 })
        });
        assert!(other.is_empty());
    }

    #[test]
    fn handoff_bounds_and_invalid_sessions_never_create_job_ownership() {
        let root = ProcessKey { pid: 20, start: 2 };
        let value = changed_carry(root, |job| job.session = "s-not-a-valid-id".into());
        let mut ledger = Ledger::default();
        restore_into(&mut ledger, value, Some([7; 16]), &|_| Some(root));
        assert!(ledger.is_empty());
        let value = changed_carry(root, |job| {
            job.members = vec![(20, 2); aterm_vitals::delegated::MAX_MEMBERS + 1];
        });
        restore_into(&mut ledger, value, Some([7; 16]), &|_| Some(root));
        assert!(ledger.is_empty());
    }
}
