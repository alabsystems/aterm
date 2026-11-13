// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real rejected-dispatch paths, with observations from Live and an isolated ledger.

use super::*;
use aterm_capacity::{Ledger, Machine, Need, Procs, Reading};

pub(crate) const CANDIDATES: usize = 32;
pub(crate) const WORKERS: usize = 2;
pub(crate) const PASSES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arm {
    Old,
    Probe,
}

impl Arm {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::Probe => "probe",
        }
    }
}

struct BlockedLive {
    reads: AtomicUsize,
    sweeps: AtomicUsize,
    lineages: AtomicUsize,
    waits: AtomicUsize,
    unavailable: AtomicUsize,
}

fn complete_reading(reading: &Reading) -> bool {
    reading.cores > 0
        && reading
            .available
            .zip(reading.physical)
            .is_some_and(|(available, physical)| physical > 0 && available <= physical)
        && reading
            .wired
            .zip(reading.physical)
            .is_some_and(|(wired, physical)| wired < physical)
        && [reading.load1, reading.load5]
            .into_iter()
            .all(|load| load.is_some_and(|load| load.is_finite() && load >= 0.0))
}

fn block_observed_memory(reading: &mut Reading) -> bool {
    if !complete_reading(reading) {
        return false;
    }
    reading.available = Some(0);
    true
}

struct HolderMachine(Reading);
impl Machine for HolderMachine {
    fn read(&self, _path: Option<&Path>) -> Reading {
        self.0.clone()
    }
    fn procs(&self) -> Option<Procs> {
        None
    }
}

impl BlockedLive {
    fn reset(&self) {
        self.reads.store(0, Ordering::SeqCst);
        self.sweeps.store(0, Ordering::SeqCst);
        self.lineages.store(0, Ordering::SeqCst);
        self.waits.store(0, Ordering::SeqCst);
        self.unavailable.store(0, Ordering::SeqCst);
    }
}

impl Machine for BlockedLive {
    fn release_run(&self) -> bool {
        aterm_capacity::Live.release_run()
    }
    fn read(&self, path: Option<&Path>) -> Reading {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut reading = aterm_capacity::Live.read(path);
        // An unread or inconsistent OS observation is never replaced by invented evidence.
        if !block_observed_memory(&mut reading) {
            self.unavailable.fetch_add(1, Ordering::SeqCst);
        }
        reading
    }
    fn procs(&self) -> Option<Procs> {
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        let procs = aterm_capacity::Live.procs();
        if procs.is_none() {
            self.unavailable.fetch_add(1, Ordering::SeqCst);
        }
        procs
    }
    fn lineage(&self, pid: u32) -> Option<Procs> {
        self.lineages.fetch_add(1, Ordering::SeqCst);
        let lineage = aterm_capacity::Live.lineage(pid);
        if lineage.is_none() {
            self.unavailable.fetch_add(1, Ordering::SeqCst);
        }
        lineage
    }
    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        aterm_capacity::Live.waits()
    }
    fn volume(&self, path: &Path) -> Option<aterm_capacity::Volume> {
        aterm_capacity::Live.volume(path)
    }
    fn cpu_point(&self) -> Option<aterm_capacity::CpuPoint> {
        aterm_capacity::Live.cpu_point()
    }
    fn gpu_busy(&self, stopped: &dyn Fn() -> bool) -> Option<u8> {
        aterm_capacity::Live.gpu_busy(stopped)
    }
}

#[derive(Default)]
struct GateState {
    ready: usize,
    start: Option<bool>,
    deadline: Option<Instant>,
    finished: bool,
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Gate {
    fn ready(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.ready += 1;
        self.changed.notify_all();
        while state.start.is_none() {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.start == Some(true)
    }
    fn wait_ready(&self) {
        let end = Instant::now() + Duration::from_secs(60);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.ready != WORKERS {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Returning would join the same stuck scoped worker forever. This is
                // exclusively the benchmark child; admission has not started yet.
                aterm_uds::process::kill_self_now();
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
    fn start(&self, run: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.start = Some(run);
        state.deadline = run.then(|| Instant::now() + Duration::from_secs(60));
        self.changed.notify_all();
    }
    fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.finished = true;
        state.start.get_or_insert(false);
        self.changed.notify_all();
    }
    fn watch(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.start.is_none() && !state.finished {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        while !state.finished && state.start == Some(true) {
            let left = state
                .deadline
                .expect("armed measurement deadline")
                .saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(state);
                // This is the dedicated benchmark child, never the invoking verifier.
                aterm_uds::process::kill_self_now();
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

struct FinishGate<'a>(&'a Gate);
impl Drop for FinishGate<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

fn scan(
    state: &Mutex<Dispatcher>,
    pass: &Mutex<AdmissionPass>,
    waiters: &BinaryWaiters,
    ctx: &crate::Ctx,
    ledger: &Ledger,
    machine: &BlockedLive,
    arm: Arm,
) -> Result<usize, String> {
    let mut d = state.lock().unwrap_or_else(PoisonError::into_inner);
    let generation = d.generation();
    let mut attempts = 0;
    let mut error = None;
    let started = d.next_with(0.0, &mut |b| {
        attempts += 1;
        let mut global_wait = None;
        let mut core_wait = None;
        let outcome = binary_permit_noting(
            Some(ctx),
            None,
            b,
            &mut |need, mode| {
                if mode.is_some() {
                    return Err(aterm_capacity::Error::Ledger(std::io::Error::other(
                        "unexpected overriding admission",
                    )));
                }
                let result = waiters.poll_noting(
                    b.index,
                    need,
                    machine,
                    |need| {
                        aterm_capacity::admit::Waiter::enter(ledger, machine, need)
                            .map_err(aterm_capacity::Error::Ledger)
                    },
                    &mut |need| {
                        (arm == Arm::Probe)
                            .then(|| {
                                pass.lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .rejection(generation, ledger, machine, need, &Instant::now)
                            })
                            .flatten()
                    },
                    &mut global_wait,
                );
                if matches!(&result, Ok(Some(_))) {
                    pass.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .acquired();
                }
                result
            },
            false,
            &mut core_wait,
        );
        if let Some(result) = outcome {
            error = Some(match result {
                Ok(permit) => {
                    drop(permit);
                    "blocked fixture unexpectedly acquired resources".into()
                }
                Err(why) => why.to_string(),
            });
        } else if let Some(reason) = core_wait.or(global_wait) {
            let (source, kinds) = reason.classification();
            if source != "ledger" || !kinds.contains(&"memory") {
                error = Some("blocked fixture returned a non-memory wait reason".into());
            }
            std::hint::black_box(reason);
        } else {
            error = Some("blocked fixture returned no wait reason".into());
        }
        false // A measurement never dispatches work, even if the safety oracle fails.
    });
    if started.is_some() || error.is_some() || attempts != CANDIDATES {
        return Err(error.unwrap_or_else(|| format!("rejected scan visited {attempts} candidates")));
    }
    Ok(attempts)
}

fn counters() -> Result<aterm_sysprobe::jobs::Counters, String> {
    let c = aterm_sysprobe::jobs::counters_of(std::process::id())
        .ok_or("process counters unavailable")?;
    if c.logical_writes.is_none() || c.disk_read.is_none() || c.disk_written.is_none() {
        return Err("logical/storage byte counters unavailable".into());
    }
    Ok(c)
}

pub(crate) fn measure(arm: Arm) -> Result<aterm_json::Value, String> {
    if aterm_capacity::Live.release_run() {
        return Err("admission benchmark requires ordinary, not release, scheduling".into());
    }
    let dir = aterm_tempfile::tempdir().map_err(|e| e.to_string())?;
    let ledger = Ledger::at(dir.path().join("admission"));
    let reading = aterm_capacity::Live.read(None);
    if !complete_reading(&reading) {
        return Err("essential Live machine observation unavailable".into());
    }
    let physical = reading.physical.ok_or("physical memory unavailable")?;
    if reading.reachable().is_none_or(|bytes| bytes <= 2 << 30) {
        return Err("host memory ceiling is too small for the blocked fixture".into());
    }
    let mut holder_reading = reading.clone();
    holder_reading.available = holder_reading.reachable();
    let holder = aterm_capacity::admit::try_admit_with(
        &ledger,
        &HolderMachine(holder_reading),
        Need::new("isolated benchmark holder").mem_gib(1),
    )
    .map_err(|e| e.to_string())?
    .ok_or("isolated holder refused")?;
    let holder_bytes = std::fs::read(holder.path()).map_err(|e| e.to_string())?;
    let machine = BlockedLive {
        reads: AtomicUsize::new(0),
        sweeps: AtomicUsize::new(0),
        lineages: AtomicUsize::new(0),
        waits: AtomicUsize::new(0),
        unavailable: AtomicUsize::new(0),
    };
    let mut ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(physical),
        Some(physical),
    ));
    let share = u32::try_from(WORKERS).map_err(|_| "worker count exceeds planner range")?;
    let mut plan = test_plan::plan(&vec![None; CANDIDATES], share, None, 1 << 30);
    for binary in &mut plan.order {
        binary.peak = 1 << 30;
        binary.charge = 1;
    }
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let pass = Mutex::new(AdmissionPass::default());
    let waiters = BinaryWaiters::default();
    let gate = Gate::default();
    std::thread::scope(|scope| {
        let _finish = FinishGate(&gate);
        let watchdog = std::thread::Builder::new()
            .name("admission-bench-deadline".into())
            .spawn_scoped(scope, || gate.watch())
            .map_err(|e| format!("spawn measurement watchdog: {e}"))?;
        let mut workers = Vec::new();
        for index in 0..WORKERS {
            let (state, pass, waiters, ctx, ledger, machine, gate) =
                (&state, &pass, &waiters, &ctx, &ledger, &machine, &gate);
            let spawned = std::thread::Builder::new()
                .name(format!("admission-bench-{index}"))
                .spawn_scoped(scope, move || {
                    let warm = scan(state, pass, waiters, ctx, ledger, machine, arm);
                    let clock = aterm_sysprobe::dstate::thread_ns();
                    if !gate.ready() {
                        return Err("measurement did not start".to_string());
                    }
                    warm?;
                    if clock == 0 {
                        return Err("thread CPU clock unavailable".into());
                    }
                    let before = aterm_sysprobe::dstate::thread_ns();
                    if before == 0 {
                        return Err("thread CPU clock unavailable".into());
                    }
                    let mut attempts = 0;
                    for round in 0..PASSES {
                        attempts += scan(state, pass, waiters, ctx, ledger, machine, arm)?;
                        if round + 1 < PASSES {
                            std::thread::sleep(LOOK_AGAIN);
                        }
                    }
                    let after = aterm_sysprobe::dstate::thread_ns();
                    Ok((
                        after
                            .checked_sub(before)
                            .ok_or("thread CPU clock regressed")?,
                        attempts,
                    ))
                });
            match spawned {
                Ok(worker) => workers.push(worker),
                Err(why) => {
                    // A prior worker may already be stuck in warmup. Returning would
                    // enter the scope's unbounded implicit join before a deadline is armed.
                    eprintln!("admission-wait: spawn benchmark worker: {why}");
                    aterm_uds::process::kill_self_now();
                }
            }
        }
        gate.wait_ready();
        let ready = {
            if machine.unavailable.load(Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err("Live observation unavailable during warmup".into())
            }
        };
        let population_before = aterm_sysprobe::processes().map(|sweep| sweep.procs.len());
        let entry = ready.and_then(|()| crate::perf::measurement_entry());
        if let Err(why) = entry {
            gate.start(false);
            return Err(why);
        }
        let entry = entry.expect("entry checked");
        let queued_before = ledger.list().map_err(|e| e.to_string())?;
        let retained = queued_before
            .iter()
            .find(|record| record.held.is_none())
            .ok_or("warmup retained no fair-queue waiter")?
            .order();
        *pass.lock().unwrap_or_else(PoisonError::into_inner) = AdmissionPass::default();
        machine.reset();
        let before = match counters() {
            Ok(before) => before,
            Err(why) => {
                gate.start(false);
                return Err(why);
            }
        };
        let began = Instant::now();
        gate.start(true);
        let mut results = Vec::new();
        for worker in workers {
            results.push(
                worker
                    .join()
                    .map_err(|_| "admission benchmark worker panicked".to_string())
                    .and_then(|r| r),
            );
        }
        gate.finish();
        watchdog
            .join()
            .map_err(|_| "measurement watchdog panicked")?;
        let results: Vec<_> = results.into_iter().collect::<Result<_, _>>()?;
        let wall_ns = began.elapsed().as_nanos();
        let after = counters()?;
        entry.timing_admission().map_err(|e| e.to_string())?;
        let population_after = aterm_sysprobe::processes().map(|sweep| sweep.procs.len());
        let expected = CANDIDATES * WORKERS * PASSES;
        let attempts: usize = results.iter().map(|(_, n)| n).sum();
        if machine.unavailable.load(Ordering::SeqCst) != 0 {
            return Err("Live observation unavailable during measured scans".into());
        }
        let queued = {
            let dispatcher = state.lock().unwrap_or_else(PoisonError::into_inner);
            dispatcher.held() == 0
                && (0..CANDIDATES).all(|i| dispatcher.phase(i) == Some(test_plan::Phase::Queued))
        };
        let records = ledger.list().map_err(|e| e.to_string())?;
        if attempts != expected
            || !queued
            || records.len() != 2
            || !records.iter().any(|record| record.order() == retained)
            || std::fs::read(holder.path()).map_err(|e| e.to_string())? != holder_bytes
        {
            return Err("blocked workload changed dispatcher or holder custody".into());
        }
        waiters.clear();
        if ledger.list().map_err(|e| e.to_string())?.len() != 1 {
            return Err("clearing the retained waiter changed unrelated custody".into());
        }
        let delta = |end: Option<u64>, start: Option<u64>| {
            end.zip(start)
                .and_then(|(end, start)| end.checked_sub(start))
                .ok_or("process write counter unavailable or regressed")
        };
        Ok(aterm_json::json!({
            "arm":arm.word(), "candidates":CANDIDATES, "workers":WORKERS, "passes":PASSES,
            "attempts":attempts, "admitted":0, "all_queued":true, "holder_unchanged":true,
            "fair_queue_waiter_retained":true,
            "worker_cpu_ns":results.iter().map(|(cpu,_)| *cpu).collect::<Vec<_>>(),
            "process_cpu_ns":after.cpu_ns.checked_sub(before.cpu_ns).ok_or("process CPU regressed")?,
            "wall_ns":wall_ns.to_string(), "read_calls":machine.reads.load(Ordering::SeqCst),
            "process_sweeps":machine.sweeps.load(Ordering::SeqCst),
            "lineage_reads":machine.lineages.load(Ordering::SeqCst),
            "wait_mark_reads":machine.waits.load(Ordering::SeqCst),
            "unavailable_observations":machine.unavailable.load(Ordering::SeqCst),
            "process_population_before":population_before,
            "process_population_after":population_after,
            "process_population_scope":"independent boundary sweeps outside timed counters; not cardinality of each Live observation",
            "logical_write_bytes":delta(after.logical_writes,before.logical_writes)?,
            "storage_read_bytes":delta(after.disk_read,before.disk_read)?,
            "storage_write_bytes":delta(after.disk_written,before.disk_written)?,
            "io_scope":"whole benchmark process; logical writes include descriptors; storage bytes exclude cache hits",
            "reason_scope":"production admission reason construction; excludes Trace transitions and event persistence",
            "clock":"CLOCK_THREAD_CPUTIME_ID", "available_memory_override":0,
            "physical_memory":physical, "lookup_interval_ms":LOOK_AGAIN.as_millis().to_string()
        }))
    })
}

#[cfg(test)]
#[path = "testrun_admission_bench_tests.rs"]
mod tests;
