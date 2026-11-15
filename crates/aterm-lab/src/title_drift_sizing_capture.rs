// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scoped sizing observations, never timing authority. Kernel lifetime peaks supplement
//! sampled current sums. Unobserved short-lived descendants remain outside the sampling
//! contract and require the preparer's explicit conservative growth allowance.

use crate::timing_comparison::{FOOTPRINT_SEMANTICS, FootprintObservation};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;

use super::super::{Build, HANG};
use aterm_json::{Value, json};
use aterm_uds::process::Identity;

pub(super) const COMPONENTS: [&str; 4] = [
    "controller",
    "app_and_keeper",
    "producers",
    "setup_cleanup_overlap",
];

pub(super) struct Report {
    pub witnesses: BTreeMap<String, Vec<Value>>,
    pub raw_path: PathBuf,
    pub raw_digest: String,
}

#[derive(Clone)]
struct Bound {
    keeper: Identity,
    app: Identity,
}

enum Request {
    Bind(Bound),
    Phase(&'static str),
    Check(mpsc::Sender<Result<(), String>>),
    Complete(mpsc::Sender<Result<Report, String>>),
    Cancel(mpsc::Sender<Result<Report, String>>),
}

pub(super) struct Capture {
    send: mpsc::Sender<Request>,
    thread: Option<JoinHandle<()>>,
    stopped: bool,
}

impl Capture {
    pub(super) fn start(
        raw_path: PathBuf,
        key: String,
        generation: String,
        build: Build,
        controller_path: PathBuf,
        controller_digest: String,
        budget: u64,
    ) -> Result<Self, String> {
        let controller = aterm_uds::process::read_identity(std::process::id() as i32)
            .ok_or("sizing controller identity unavailable")?;
        let mut probe = Probe {
            controller,
            controller_path,
            controller_digest,
            build,
            bound: None,
            known: HashSet::new(),
            originals: HashSet::new(),
            retired: HashSet::new(),
            hashes: HashMap::new(),
        };
        let first = probe.snapshot(false, false)?;
        if first.total > budget {
            return Err(format!(
                "explicit functional sizing budget {budget} is below actual controller envelope {}: {:?}",
                first.total, first.roles
            ));
        }
        let (send, recv) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("title-memory-capture".into())
            .spawn(move || {
                let opened = Writer::new(raw_path, key, generation, budget, first);
                match opened {
                    Err(why) => {
                        let _ = ready.send(Err(why));
                    }
                    Ok(mut writer) => {
                        let _ = ready.send(Ok(()));
                        let mut phase = "startup";
                        let mut state = CaptureState::default();
                        loop {
                            let request = recv.recv_timeout(aterm_capacity::tree_meter::READ_EVERY);
                            match request {
                                Ok(Request::Bind(bound)) => {
                                    probe.originals.extend([bound.app, bound.keeper]);
                                    probe.bound = Some(bound);
                                }
                                Ok(Request::Phase(next)) => phase = next,
                                Ok(Request::Check(reply)) => {
                                    let result = state.observe(
                                        &mut writer,
                                        phase,
                                        probe.snapshot(true, phase == "cleanup"),
                                    );
                                    let _ = reply.send(result);
                                }
                                Ok(Request::Complete(reply)) => {
                                    let snapshot = probe.snapshot(true, true);
                                    let result =
                                        state.complete(writer, &probe.controller.token(), snapshot);
                                    let _ = reply.send(result);
                                    break;
                                }
                                Ok(Request::Cancel(reply)) => {
                                    let result = state.cancel(writer);
                                    let _ = reply.send(result);
                                    break;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    let _ = state.observe(
                                        &mut writer,
                                        phase,
                                        probe.snapshot(false, phase == "cleanup"),
                                    );
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let mut capture = Self {
            send,
            thread: Some(thread),
            stopped: false,
        };
        match started
            .recv_timeout(HANG)
            .map_err(|_| "sizing recorder startup hang detector")?
        {
            Ok(()) => Ok(capture),
            Err(why) => {
                capture.stopped = true;
                if let Some(thread) = capture.thread.take() {
                    let _ = thread.join();
                }
                Err(why)
            }
        }
    }

    pub(super) fn bind(
        &self,
        inst: &crate::instance::Instance,
        build: &Build,
    ) -> Result<(), String> {
        let pid = inst.aterm_pid().ok_or("owned sizing app PID unavailable")?;
        let app = aterm_uds::process::read_identity(pid)
            .ok_or("owned sizing app identity unavailable")?;
        let keeper = aterm_uds::process::read_identity(inst.pid)
            .ok_or("owned sizing keeper identity unavailable")?;
        if aterm_uds::process::read_process_path(pid).as_ref() != Some(&build.path) {
            return Err("sizing app executable differs from frozen build".into());
        }
        self.send
            .send(Request::Bind(Bound { keeper, app }))
            .map_err(|_| "sizing recorder stopped".into())
    }
    pub(super) fn phase(&self, phase: &'static str) -> Result<(), String> {
        self.send
            .send(Request::Phase(phase))
            .map_err(|_| "sizing recorder stopped".into())
    }
    pub(super) fn checkpoint(&self) -> Result<(), String> {
        let (send, recv) = mpsc::channel();
        self.send
            .send(Request::Check(send))
            .map_err(|_| "sizing recorder stopped")?;
        recv.recv_timeout(HANG)
            .map_err(|_| "sizing sweep hang detector")?
    }
    pub(super) fn finish(mut self) -> Result<Report, String> {
        self.stop(true)
    }
    fn stop(&mut self, completing: bool) -> Result<Report, String> {
        if self.stopped {
            return Err("duplicate sizing recorder completion".into());
        }
        self.stopped = true;
        let (send, recv) = mpsc::channel();
        self.send
            .send(if completing {
                Request::Complete(send)
            } else {
                Request::Cancel(send)
            })
            .map_err(|_| "sizing recorder stopped")?;
        let report = recv
            .recv_timeout(HANG)
            .map_err(|_| "sizing recorder stop hang detector")?;
        self.stopped = true;
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| "sizing recorder panicked")?;
        }
        report
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.stop(false);
        }
    }
}

struct Sample {
    at: u64,
    roles: BTreeMap<String, Vec<Value>>,
    total: u64,
    retired: Vec<String>,
    originals: Vec<String>,
}

// Noncloneable final evidence, created only after the current whole sweep establishes
// positive retirement of every original, not merely the keeper/app roots.
struct Retirement;
impl Sample {
    fn retirement(&self, controller: &str) -> Result<Retirement, String> {
        let live = crate::timing_comparison::sizing_live_identities(&json!(self.roles))?;
        crate::timing_comparison::sizing_final_retirement(
            controller,
            &self.originals.iter().cloned().collect(),
            &self.retired.iter().cloned().collect(),
            &live,
        )?;
        Ok(Retirement)
    }
}
/// Both timer ticks and mandatory checkpoints use this same sticky production decision.
/// Cleanup never clears a read, budget, identity or persistence failure.
#[derive(Default)]
struct CaptureState {
    failure: Option<String>,
}
impl CaptureState {
    fn observe(
        &mut self,
        writer: &mut Writer,
        phase: &str,
        sample: Result<Sample, String>,
    ) -> Result<(), String> {
        if let Err(why) = sample.and_then(|sample| writer.observe(phase, sample)) {
            self.refuse(writer, phase, why);
        }
        self.failure.clone().map_or(Ok(()), Err)
    }
    fn refuse(&mut self, writer: &mut Writer, phase: &str, why: String) {
        self.failure.get_or_insert_with(|| why.clone());
        if let Err(persistence) =
            writer.event(json!({"event":"refused","phase":phase,"reason":why}))
        {
            self.failure.get_or_insert(persistence);
        }
    }
    fn complete(
        mut self,
        mut writer: Writer,
        controller: &str,
        sample: Result<Sample, String>,
    ) -> Result<Report, String> {
        let proof = sample
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|sample| sample.retirement(controller));
        let _ = self.observe(&mut writer, "cleanup_complete", sample);
        let proof = match proof {
            Ok(proof) => Some(proof),
            Err(why) => {
                self.refuse(&mut writer, "cleanup_complete", why);
                None
            }
        };
        writer.finish(self.failure, proof)
    }
    fn cancel(self, writer: Writer) -> Result<Report, String> {
        writer.finish(
            Some(
                self.failure
                    .unwrap_or_else(|| "sizing capture cancelled without final retirement".into()),
            ),
            None,
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Presence {
    Live,
    Retired,
}
fn presence(
    expected: Identity,
    actual: Option<Identity>,
    alive: bool,
    cleanup: bool,
) -> Result<Presence, String> {
    match actual {
        Some(actual) if actual == expected => Ok(Presence::Live),
        None if !alive && cleanup => Ok(Presence::Retired),
        _ => Err(
            "original sizing identity changed, is unreadable, or disappeared before cleanup".into(),
        ),
    }
}

struct Probe {
    controller: Identity,
    controller_path: PathBuf,
    controller_digest: String,
    build: Build,
    bound: Option<Bound>,
    known: HashSet<Identity>,
    originals: HashSet<Identity>,
    retired: HashSet<Identity>,
    hashes: HashMap<PathBuf, String>,
}

/// A known snapshot is bracketed by the exact PID/birth/exec identity. Missing or
/// unavailable kernel counters are not manufactured as zero.
fn checked_member(
    expected: Identity,
    observed: (Option<Identity>, Option<Identity>),
    path: PathBuf,
    memory: Option<(u64, u64)>,
    digest: String,
    source: Option<&str>,
    record: Option<&str>,
) -> Result<Value, String> {
    if observed != (Some(expected), Some(expected))
        || expected.pid <= 1
        || expected.birth.0 == 0
        || expected.idversion.is_none()
        || !path.is_absolute()
    {
        return Err("sizing PID/birth/exec/path changed or unavailable".into());
    }
    let (current, peak) = memory
        .filter(|(current, peak)| *current > 0 && *peak > 0)
        .ok_or_else(|| {
            let reason = match memory {
                None => "rusage_unavailable",
                Some((0, _)) => "current_footprint_zero",
                Some(_) => "lifetime_peak_zero",
            };
            format!(
                "sizing current/lifetime footprint unavailable or inconsistent: \
                 identity={} pid={} birth={:?} idversion={:?} executable={path:?} \
                 counters_current_lifetime_peak={memory:?} reason={reason}",
                expected.token(),
                expected.pid,
                expected.birth,
                expected.idversion
            )
        })?;
    let counters = FootprintObservation::new(current, peak)?;
    Ok(
        json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"identity":expected.token(),"executable":path.display().to_string(),"binary_sha256":digest,
        "source_sha":source,"build_record_sha256":record,"footprint_bytes":counters.current,
        "lifetime_peak_footprint_bytes":counters.lifetime_max,"lifetime_scope":"kernel_process_birth"}),
    )
}

/// Passive controller observation for the explicitly admitted evidence-preparation phase.
pub(super) fn controller_snapshot() -> Result<Value, String> {
    let pid = std::process::id() as i32;
    let before = aterm_uds::process::read_identity(pid);
    let identity = before.ok_or("preparation controller identity unavailable")?;
    let path = aterm_uds::process::read_process_path(pid)
        .ok_or("preparation controller path unavailable")?;
    if path != crate::paths::paths().exe {
        return Err("preparation controller executable changed".into());
    }
    let memory =
        crate::sys::rusage(pid).map(|r| (r.ri_phys_footprint, r.ri_lifetime_max_phys_footprint));
    let digest = aterm_update_core::sha256_file(&path)?;
    checked_member(
        identity,
        (before, aterm_uds::process::read_identity(pid)),
        path,
        memory,
        digest,
        None,
        None,
    )
}

impl Probe {
    fn member(&mut self, expected: Identity) -> Result<Value, String> {
        let before = aterm_uds::process::read_identity(expected.pid);
        let path = aterm_uds::process::read_process_path(expected.pid)
            .ok_or("sizing executable path unavailable")?;
        let memory = crate::sys::rusage(expected.pid)
            .map(|r| (r.ri_phys_footprint, r.ri_lifetime_max_phys_footprint));
        let after = aterm_uds::process::read_identity(expected.pid);
        let (digest, source, record) = if path == self.build.path {
            (
                self.build.digest.clone(),
                Some(self.build.source.as_str()),
                Some(self.build.record_digest.as_str()),
            )
        } else if path == self.controller_path {
            (self.controller_digest.clone(), None, None)
        } else {
            let digest = match self.hashes.get(&path) {
                Some(hash) => hash.clone(),
                None => {
                    let hash = aterm_update_core::sha256_file(&path)?;
                    self.hashes.insert(path.clone(), hash.clone());
                    hash
                }
            };
            (digest, None, None)
        };
        checked_member(
            expected,
            (before, after),
            path,
            memory,
            digest,
            source,
            record,
        )
    }
    fn snapshot(&mut self, whole: bool, cleanup: bool) -> Result<Sample, String> {
        let controller = self.member(self.controller)?;
        if controller.get("executable").and_then(Value::as_str) != self.controller_path.to_str() {
            return Err("sizing controller path changed".into());
        }
        let mut roots = vec![controller.clone()];
        let mut app_and_keeper = Vec::new();
        let mut active_roots = vec![self.controller.pid as u32];
        if let Some(bound) = self.bound.clone() {
            for (expected, path) in [
                (bound.keeper, self.controller_path.clone()),
                (bound.app, self.build.path.clone()),
            ] {
                match presence(
                    expected,
                    aterm_uds::process::read_identity(expected.pid),
                    aterm_uds::process::pid_alive(expected.pid as u32),
                    cleanup,
                )? {
                    Presence::Live => {
                        // Once a live member is observed, ANY failed memory/path/hash read
                        // is fatal, even if the process then exits during this sweep.
                        let member = self.member(expected)?;
                        if member.get("executable").and_then(Value::as_str) != path.to_str() {
                            return Err("sizing app/keeper image changed".into());
                        }
                        active_roots.push(expected.pid as u32);
                        roots.push(member.clone());
                        app_and_keeper.push(member);
                    }
                    Presence::Retired => {
                        self.retired.insert(expected);
                    }
                }
            }
        } else if whole {
            return Err("sizing whole-tree checkpoint lacks its actual app and keeper".into());
        }
        if whole {
            let sweep = aterm_sysprobe::processes().ok_or("sizing process sweep unavailable")?;
            let mut inside: HashSet<u32> = active_roots.iter().copied().collect();
            loop {
                let mut changed = false;
                for row in &sweep.procs {
                    if inside.contains(&row.ppid) {
                        changed |= inside.insert(row.pid);
                    }
                }
                if !changed {
                    break;
                }
            }
            for row in &sweep.procs {
                if inside.contains(&row.pid) && !active_roots.contains(&row.pid) {
                    let pid = i32::try_from(row.pid).map_err(|_| "invalid sizing child pid")?;
                    match aterm_uds::process::read_identity(pid) {
                        Some(identity) => {
                            self.known.insert(identity);
                            self.originals.insert(identity);
                        }
                        None if !aterm_uds::process::pid_alive(row.pid) => {}
                        None => return Err("sizing descendant identity unavailable".into()),
                    }
                }
            }
        }
        let mut producers = Vec::new();
        for expected in self.known.clone() {
            match presence(
                expected,
                aterm_uds::process::read_identity(expected.pid),
                aterm_uds::process::pid_alive(expected.pid as u32),
                cleanup,
            )? {
                Presence::Live => producers.push(self.member(expected)?),
                Presence::Retired => {
                    self.known.remove(&expected);
                    self.retired.insert(expected);
                }
            }
        }
        producers.sort_by_key(|m| {
            m.get("identity")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        });
        let mut all = roots.clone();
        all.extend(producers.clone());
        let total = peak_sum(&all)?;
        let at = crate::sys::monotonic_ns();
        if at == 0 {
            return Err("sizing monotonic clock unavailable".into());
        }
        let mut roles = BTreeMap::new();
        roles.insert("controller".into(), vec![controller]);
        if app_and_keeper.len() == 2 {
            roles.insert("app_and_keeper".into(), app_and_keeper);
        }
        if !producers.is_empty() {
            roles.insert("producers".into(), producers);
        }
        if whole
            && (!roles.contains_key("app_and_keeper") || !roles.contains_key("producers"))
            && !cleanup
        {
            return Err("complete sizing fixture membership unavailable".into());
        }
        roles.insert("setup_cleanup_overlap".into(), all);
        let mut retired: Vec<_> = self.retired.iter().map(|id| id.token()).collect();
        let mut originals: Vec<_> = self.originals.iter().map(|id| id.token()).collect();
        retired.sort();
        originals.sort();
        Ok(Sample {
            at,
            roles,
            total,
            retired,
            originals,
        })
    }
}

// The established TreeMeter envelope: the largest observed current-reading sum,
// floored by the largest kernel lifetime peak of one member. Unobserved overlap
// requires the preparer's explicit growth allowance; no unseen peak is proved.
fn peak_sum(members: &[Value]) -> Result<u64, String> {
    let mut sum = 0u64;
    let mut one = 0u64;
    for member in members {
        let counters = FootprintObservation::read(member)?;
        sum = sum
            .checked_add(counters.current)
            .ok_or("sizing footprint sum overflow")?;
        one = one.max(counters.lifetime_max);
    }
    Ok(sum.max(one))
}

struct Writer {
    out: BufWriter<std::fs::File>,
    raw_path: PathBuf,
    key: String,
    generation: String,
    budget: u64,
    last: u64,
    selected: BTreeMap<(String, String), (u64, Value)>,
}
impl Writer {
    fn new(
        raw_path: PathBuf,
        key: String,
        generation: String,
        budget: u64,
        first: Sample,
    ) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&raw_path)
            .map_err(|e| e.to_string())?;
        let mut writer = Self {
            out: BufWriter::new(file),
            raw_path,
            key,
            generation,
            budget,
            last: 0,
            selected: BTreeMap::new(),
        };
        writer.event(json!({"footprint_semantics":FOOTPRINT_SEMANTICS,"kind":"functional_memory_sizing","timing_qualified":false,"experiment_sha256":writer.key,"generation":writer.generation,"functional_budget_bytes":budget,"spare_bytes":crate::admission::FUNCTIONAL_MARGIN,"sample_interval_ms":aterm_capacity::tree_meter::READ_EVERY.as_millis()}))?;
        writer.observe("startup", first)?;
        Ok(writer)
    }
    fn event(&mut self, value: Value) -> Result<(), String> {
        writeln!(self.out, "{value}")
            .and_then(|()| self.out.flush())
            .map_err(|e| e.to_string())
    }
    fn observe(&mut self, phase: &str, sample: Sample) -> Result<(), String> {
        if sample.at <= self.last {
            return Err("stale sizing sweep".into());
        }
        self.last = sample.at;
        self.event(json!({"event":"sweep","phase":phase,"monotonic_ns":sample.at,"owned_envelope_bytes":sample.total,"components":sample.roles,"retired_original_identities":sample.retired,"original_non_controller_identities":sample.originals}))?;
        if sample.total > self.budget {
            return Err(
                "actual sizing envelope exceeded its explicitly admitted functional budget".into(),
            );
        }
        for (role, members) in sample.roles {
            let score = peak_sum(&members)?;
            let observation = json!({"monotonic_ns":sample.at,"phase":phase,"capture_generation":self.generation,"members":members});
            let entry = self
                .selected
                .entry((role, phase.to_owned()))
                .or_insert((0, observation.clone()));
            if score > entry.0 {
                *entry = (score, observation);
            }
        }
        Ok(())
    }
    fn finish(
        mut self,
        failure: Option<String>,
        proof: Option<Retirement>,
    ) -> Result<Report, String> {
        let failure = failure.or_else(|| {
            proof
                .is_none()
                .then(|| "missing positive final original retirement".into())
        });
        self.event(
            json!({"event":"recorder_stop","error":failure,"cleanup_complete":proof.is_some()}),
        )?;
        // A later comparison reuses these contents, so seal them after a durable flush.
        // A power loss may lose the directory entry; missing evidence refuses reuse.
        self.out
            .flush()
            .and_then(|()| aterm_durable::durable(self.out.get_ref()))
            .map_err(|e| e.to_string())?;
        if let Some(why) = failure {
            return Err(why);
        }
        let mut witnesses: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for ((role, _), (_, value)) in self.selected {
            witnesses.entry(role).or_default().push(value);
        }
        for values in witnesses.values_mut() {
            values.sort_by_key(|v| v.get("monotonic_ns").and_then(Value::as_u64));
        }
        let raw_digest = aterm_update_core::sha256_file(&self.raw_path)?;
        Ok(Report {
            witnesses,
            raw_path: self.raw_path,
            raw_digest,
        })
    }
}

#[cfg(test)]
#[path = "title_drift_sizing_capture_tests.rs"]
mod tests;
