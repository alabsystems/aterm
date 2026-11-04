// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search --workers` as processes (design §11.7): the six-line plans file on three workers,
//! honest and under each injected fault, against one local run. A lease window is the default
//! minute, except where a fault waits one out ([`expiring`]) or holds one open ([`HELD`]); a pool
//! run's hello and drain grace are past the run ([`PAST_THE_RUN`]), except the grace where a fault
//! never answers its `Cancel` ([`unanswered`]), in `foreign@0`'s one-worker run, which drains
//! nothing, and in the signal tests, so no deadline of the coordinator's ends a stall its detector
//! should judge. The two signal tests ([`sigterm_voids_and_reaps`],
//! [`sigkill_to_the_coordinator_ends_every_worker_by_the_lifeline`]) spawn [`HELD`] (no
//! `--frames`), at [`LIMITS_V1`]'s drain grace, which bounds the drain the signal starts: each
//! holds its run to the detector on one step, all three workers' pid lines (their accepted
//! hellos; a run that ends first fails at once), then signals and asserts what the signal ends
//! under a bare detector, since a drain or a lifeline is no CPU-bound work. Every other run is
//! held to the one-minute hang detector on its next new step or its end, and never retried. It
//! says its steps under `--frames` (a worker frame's step or one of the coordinator's own
//! [`PHASES`], or in a run without a pool its evaluator built and each candidate trained), so its
//! detector restarts at each and never spans its whole length. A run is CPU-bound, so one past the
//! detector whose threads waited for a core at least half as long as they were watched, and longer
//! than they ran, from 10 s into the stretch and again over its last 10 s ([`starved_through`]),
//! is COULD NOT RUN, naming both; one that waited less, or ran longer, hung, and fails however
//! loaded the host.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_aos::{ArtifactId, ArtifactKind, Digest};
use aterm_effects::aos_momentum::bundle::{SelectionPart, UncheckedBundle};
use aterm_effects::aos_momentum::lease::LIMITS_V1;
use aterm_effects::aos_momentum::m4::Compared;
use aterm_effects::aos_momentum::search::{Mode, ProposerId, SearchStats};
use aterm_spec::wait::{MUST_HAPPEN, until_within};
use aterm_uds::spawnfd::{Child, Stdio};

/// Slots 0–5: the grid winner `{SettleAtStep{0}, NextStep}`, `{Shipped, NextStep}`,
/// `{Shipped, Never}` (refused at item 0), `{Shipped, ColdOnly}` (item 1), `{Shipped, Poll{1000}}`
/// (item 0) and `{Shipped, Cadence}`: three admitted.
const SIX: &str = "\
010100000000000000000000
010000000000000000000000
010004000000000000000000
010001000000000000000000
0100020000000000e8030000
010003000000000000000000
";

/// Take one slot from the shared subprocess pool (`aterm_spec::subprocess_lane`): each run
/// spawns four judging processes, so multi-worker searches share the stage's subprocess budget.
fn slot() -> impl Drop {
    aterm_spec::subprocess_lane::acquire().unwrap_or_else(|e| {
        aterm_spec::host::could_not_run(format!("subprocess admission refused ({e})"))
    })
}

/// One finished run.
#[derive(Debug)]
struct Run {
    code: Option<i32>,
    summary: String,
    stderr: String,
    out: PathBuf,
    _dir: aterm_tempfile::TempDir,
}

impl Run {
    /// The `key=value` value on the first line starting with `prefix`.
    fn field(&self, prefix: &str, key: &str) -> Option<String> {
        let line = self.summary.lines().find(|l| l.starts_with(prefix))?;
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.strip_prefix('=').map(str::to_owned))
    }

    fn count(&self, key: &str) -> u64 {
        self.field("workers counts", key)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no count {key}:\n{}", self.summary))
    }

    fn processes(&self, key: &str) -> u64 {
        self.field("workers processes", key)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no process figure {key}:\n{}", self.summary))
    }

    fn fired(&self) -> BTreeMap<String, u64> {
        let v = self.field("workers faults", "fired").unwrap_or_default();
        if v == "none" {
            return BTreeMap::new();
        }
        v.split(',')
            .filter_map(|kv| {
                let (k, n) = kv.rsplit_once('=')?;
                Some((k.to_owned(), n.parse().ok()?))
            })
            .collect()
    }

    fn bundle(&self) -> UncheckedBundle {
        let bytes = std::fs::read(self.out.join("plans.bundle"))
            .unwrap_or_else(|e| panic!("no bundle ({e}):\n{}", self.summary));
        UncheckedBundle::decode(&bytes).expect("a bundle")
    }

    /// Every worker spawned was reaped.
    fn reaped_all(&self) {
        assert_eq!(
            self.processes("reaped"),
            self.processes("spawned"),
            "{}",
            self.summary
        );
    }
}

/// A spawned coordinator, killed and reaped (its workers then ended by the lifeline) when dropped,
/// however its test ends: one [`HELD`] open never ends by itself.
struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for Spawned {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Spawned {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

/// `aos-search --plans` with `plans` as its file, and `args`.
fn spawn(plans: &str, args: &[&str]) -> (Spawned, aterm_tempfile::TempDir, PathBuf) {
    // The coordinator and its workers exec this one file: macOS's first-exec check of a fresh
    // build is waited out here, outside the runs' hang detectors.
    aterm_spec::wait::first_exec(
        std::path::Path::new(env!("CARGO_BIN_EXE_aterm-lab")),
        &["--help"],
    );
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let plans_file = dir.path().join("plans.txt");
    std::fs::write(&plans_file, plans).expect("the plans file");
    let out = dir.path().join("out");
    let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-lab"));
    cmd.args(["aos-search", "--plans"])
        .arg(&plans_file)
        .arg("--out")
        .arg(&out)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("run aterm-lab");
    (Spawned(child), dir, out)
}

fn drain(mut r: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        s
    })
}

/// `r`'s text once it ends, and each line as it is said.
fn said(
    r: impl std::io::Read + Send + 'static,
) -> (Receiver<String>, std::thread::JoinHandle<String>) {
    let (tell, told) = std::sync::mpsc::channel();
    let text = std::thread::spawn(move || {
        let mut r = BufReader::new(r);
        let mut text = String::new();
        let mut line = Vec::new();
        while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
            let said = String::from_utf8_lossy(&line).into_owned();
            text.push_str(&said);
            line.clear();
            let _ = tell.send(said);
        }
        text
    });
    (told, text)
}

/// The stretches of the coordinator's own work at a search's end, which `--frames` says as each
/// begins (`aos-workers step=G.<phase>`): a pool run's check of its claims, then the winner's
/// held-out read and verify, which a run without a pool says too.
const PHASES: [&str; 3] = ["check", "held-out", "verify"];

/// Whether the coordinator's stderr `line` names a step of the run not taken before (`--frames`):
/// a worker's hello, item `k < items` or the end of a lease on an attempt within `max_attempts`
/// ([`LIMITS_V1`]), or one of the coordinator's [`PHASES`] of a search; in a run without a pool,
/// its evaluator built (`0.evaluator`) or a search's candidate trained (`G.trained.N`, `N` below
/// [`SIX`]'s slots). A slot has at most `max_attempts × (items + 1)` such steps, a search
/// [`PHASES`]' three and one per slot trained, and the command's plans fix its slots, so a run
/// that keeps trading frames without finishing (a lease re-run without end, a frame said again)
/// takes no new step, and the detector ends it.
fn advanced(line: &str, taken: &mut BTreeSet<String>) -> bool {
    let number = |n: &str| n.parse::<u32>().is_ok();
    if let Some(step) = line.strip_prefix("aos-workers step=") {
        let step = step.trim_end();
        let own = match step.split('.').collect::<Vec<_>>().as_slice() {
            ["0", "evaluator"] => true,
            [search, phase] => number(search) && PHASES.contains(phase),
            [search, "trained", n] => {
                number(search) && n.parse::<usize>().is_ok_and(|n| n < SIX.lines().count())
            }
            _ => false,
        };
        return own && taken.insert(step.to_owned());
    }
    let Some(step) = line
        .strip_prefix("aos-workers frame ")
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("step=")))
    else {
        return false;
    };
    let bounded = match step.split('.').collect::<Vec<_>>().as_slice() {
        ["hello", w] => w.parse::<usize>().is_ok_and(|w| w < LIMITS_V1.max_workers),
        [search, slot, attempt, k] => {
            number(search)
                && number(slot)
                && attempt
                    .parse::<u8>()
                    .is_ok_and(|a| (1..=LIMITS_V1.max_attempts).contains(&a))
                && (*k == "end" || k.parse::<u16>().is_ok_and(|k| k < LIMITS_V1.items))
        }
        _ => false,
    };
    bounded && taken.insert(step.to_owned())
}

/// What a run's processes had used when read: the coordinator's and each of its children's (its
/// workers') CPU time and runnable time (on a core or waiting for one), ns, by pid and start.
type Used = BTreeMap<(u32, u64), (u64, u64)>;

/// [`Used`] now, for the run whose coordinator is `coordinator`.
fn used(coordinator: u32) -> Used {
    aterm_sysprobe::procs::processes()
        .map(|sweep| {
            sweep
                .procs
                .into_iter()
                .filter(|p| p.pid == coordinator || p.ppid == coordinator)
                .filter_map(|p| Some(((p.pid, p.start?), (p.cpu_ns?, p.runnable_ns?))))
                .collect()
        })
        .unwrap_or_default()
}

/// What a run's processes did between two reads of them, summed over them and their threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spent {
    /// How long they waited for a core: runnable time less CPU time.
    waited: Duration,
    /// How long they ran on one: CPU time.
    ran: Duration,
}

impl Spent {
    /// Whether a run that took no step in a stretch of `span` was starved of cores, which is the
    /// machine's: it waited for one at least half as long as the stretch, and longer than it ran
    /// on one. One that waited less was blocked or stopped; one that ran longer was given cores
    /// and took no step on them (it spun), however long its threads' waits add up to: four threads
    /// spinning on a host just past its CPUs wait half the stretch between them. Either one hung.
    fn starved(self, span: Duration) -> bool {
        self.waited * 2 >= span && self.waited > self.ran
    }
}

/// [`Spent`] between two reads of a run's processes: each one's CPU time, and its runnable time
/// less its CPU time, grown since `before` (all of it for one `before` lacks, which started
/// since), summed over them.
fn spent(before: &Used, now: &Used) -> Spent {
    let (waited, ran) = now
        .iter()
        .map(|(member, &(cpu, runnable))| {
            let (cpu0, runnable0) = before.get(member).copied().unwrap_or_default();
            let ran = cpu.saturating_sub(cpu0);
            (runnable.saturating_sub(runnable0).saturating_sub(ran), ran)
        })
        .fold((0u64, 0u64), |(w, r), (dw, dr)| {
            (w.saturating_add(dw), r.saturating_add(dr))
        });
    Spent {
        waited: Duration::from_nanos(waited),
        ran: Duration::from_nanos(ran),
    }
}

/// How far into a stretch without a step the detector reads the run's processes: 10 s in, so that
/// a stretch it ends is judged on what they did in the rest of it, and 10 s before it ends, on what
/// they did at its end.
const READS: [Duration; 2] = [
    Duration::from_secs(10),
    MUST_HAPPEN.saturating_sub(Duration::from_secs(10)),
];

/// Whether a stretch past the detector was the machine's, from what the run's processes spent from
/// each of its [`READS`] to its end (`spans`): starved ([`Spent::starved`]) from the first read on,
/// and still from the last. A run whose wait was spent early and that then took no step without
/// waiting (its workers' evaluator builds starved beside a coordinator that had stopped) hung.
fn starved_through(spans: &[(Duration, Spent)]) -> bool {
    spans.len() == READS.len() && spans.iter().all(|&(span, s)| s.starved(span))
}

/// `step`'s value once it has one, polled with `child` under the detector `budget` (the one
/// minute, [`MUST_HAPPEN`], but in a test of the stretch itself), the run's processes read at each
/// of [`READS`] it reaches. Past `budget` the run is killed and reaped, then judged ([`verdict`])
/// as `what` it waited for: one cut short of both reads is never the machine's.
///
/// # Panics
/// When `step` gave no value within `budget`: COULD NOT RUN or a failure, by [`verdict`].
#[track_caller]
fn stretch<T>(
    budget: Duration,
    child: &mut Child,
    what: &str,
    mut step: impl FnMut(&mut Child) -> Option<T>,
) -> T {
    let coordinator = child.id();
    let since = Instant::now();
    let mut reads: Vec<(Instant, Used)> = Vec::new();
    let next = aterm_spec::wait::within(budget, Duration::from_millis(50), || {
        if let Some(v) = step(child) {
            return Some(v);
        }
        if READS
            .get(reads.len())
            .is_some_and(|&at| since.elapsed() >= at)
        {
            // After the sweep: a slow one's numbers are as of its end, so its span starts there.
            let read = used(coordinator);
            reads.push((Instant::now(), read));
        }
        None
    });
    if let Some(v) = next {
        return v;
    }
    let now = used(coordinator);
    let spans: Vec<(Duration, Spent)> = reads
        .iter()
        .map(|(at, before)| (at.elapsed(), spent(before, &now)))
        .collect();
    let _ = child.kill();
    let _ = child.wait();
    match verdict(what, budget, &spans) {
        Verdict::Starved(why) => aterm_spec::host::could_not_run(why),
        Verdict::Hung(why) => panic!("{why}"),
    }
}

/// What a stretch past the detector was, in the words its end says.
#[derive(Debug)]
enum Verdict {
    /// Starved through ([`starved_through`]): the machine's, a COULD NOT RUN's.
    Starved(String),
    /// Not starved: it hung, a failure's.
    Hung(String),
}

/// The verdict on a stretch past its detector `budget`, `what` it waited for, from what the run's
/// processes spent from each of [`READS`] to its end (`spans`). A run's stretches without a step
/// (the evaluator builds before the hellos or the evaluator's own step, a candidate's training in
/// a run without a pool, each of the coordinator's own phases) are CPU-bound, and a full verify's
/// load has starved one past the detector, which is the machine's ([`starved_through`]). A run
/// that was not starved hung (blocked, stopped, or spinning on cores it was given), however loaded
/// the host, and fails. Either names `what` and `spans`, and the starved one the host's load.
fn verdict(what: &str, budget: Duration, spans: &[(Duration, Spent)]) -> Verdict {
    let detail: String = spans
        .iter()
        .map(|(span, s)| {
            format!(
                "; in its last {span:.1?} its threads waited {:.1?} for a core and ran {:.1?}",
                s.waited, s.ran
            )
        })
        .collect();
    let why = format!("{what} within {budget:?}{detail}");
    if starved_through(spans) {
        let load =
            aterm_spec::host::oversubscribed().unwrap_or_else(|| "load within the CPUs".to_owned());
        return Verdict::Starved(format!("{why} ({load})"));
    }
    Verdict::Hung(why)
}

fn finish(mut child: Spawned, dir: aterm_tempfile::TempDir, out: PathBuf) -> Run {
    let stdout = drain(child.stdout.take().expect("piped"));
    let (told, stderr) = said(child.stderr.take().expect("piped"));
    let mut taken = BTreeSet::new();
    // The detector waits for the run's next new step ([`advanced`]) or its end.
    let status = loop {
        let next = stretch(
            MUST_HAPPEN,
            &mut child,
            "aos-search took no new step and did not end",
            |child| {
                if let Some(s) = child.try_wait().ok().flatten() {
                    return Some(Some(s));
                }
                told.try_iter()
                    .any(|line| advanced(&line, &mut taken))
                    .then_some(None)
            },
        );
        if let Some(s) = next {
            break s;
        }
    };
    Run {
        code: status.code(),
        summary: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
        out,
        _dir: dir,
    }
}

/// `aos-search --plans SIX` with `args`, to its end, saying its steps (`--frames`).
fn run(args: &[&str]) -> Run {
    run_on(SIX, args)
}

/// [`run`] with `plans` as the plans file.
fn run_on(plans: &str, args: &[&str]) -> Run {
    let _slot = slot();
    let (child, dir, out) = spawn(plans, &[args, &["--frames"]].concat());
    finish(child, dir, out)
}

/// The coordinator's own steps on `r`'s stderr, in the order said.
fn own_steps(r: &Run) -> Vec<&str> {
    r.stderr
        .lines()
        .filter_map(|l| l.strip_prefix("aos-workers step="))
        .collect()
}

/// A run without a pool says its steps: its evaluator built, each of [`SIX`]'s candidates trained
/// (numbered as each finishes, so in order only on one thread: `ordered`), then the winner's
/// held-out read and its verify.
fn said_local_steps(r: &Run, ordered: bool) {
    let mut own = own_steps(r);
    let phases = own.len().saturating_sub(2);
    if !ordered && let Some(trained) = own.get_mut(1..phases) {
        trained.sort_unstable();
    }
    let trained = (0..SIX.lines().count()).map(|n| format!("1.trained.{n}"));
    let want: Vec<String> = std::iter::once("0.evaluator".to_owned())
        .chain(trained)
        .chain(["1.held-out".to_owned(), "1.verify".to_owned()])
        .collect();
    assert_eq!(own, want, "{}", r.stderr);
}

/// A day, past any run's end, for a coordinator deadline on something that must happen. Such a
/// deadline starts at a spawn or a `Cancel`, not at the run's last step, and ends a stretch with a
/// loss, not a verdict, so none may end a stall before [`finish`]'s detector judges it against
/// the run's wait for a core.
const PAST_THE_RUN: &str = "86400000";

/// A pool run on three workers at the default lease window, its drain grace past the run: a
/// holder's answer to `Cancel` (its ack, or a late result) and a worker's exit once its stdin
/// closes must happen, so a loaded host cannot turn one into a `DrainTimeout` loss or a kill. A
/// passing run waits for each and no longer.
fn workers(extra: &[&str]) -> Run {
    pool(PAST_THE_RUN, extra)
}

/// A pool run on three workers with the drain grace `grace_ms`, each worker's hello past the run:
/// it says hello once its evaluator is built and its own executable digested, which a loaded host
/// stretches, and must happen, so a slow one is never a `HelloTimeout` loss and a kill.
fn pool(grace_ms: &str, extra: &[&str]) -> Run {
    let mut args = vec![
        "--workers",
        "3",
        "--hello-ms",
        PAST_THE_RUN,
        "--grace-ms",
        grace_ms,
    ];
    args.extend_from_slice(extra);
    run(&args)
}

/// `--lease-ms 10000`: a ten-second lease window, for a fault whose lease must expire; the run
/// waits the window out once.
const EXPIRING: [&str; 2] = ["--lease-ms", "10000"];

/// [`workers`] at the [`EXPIRING`] window.
fn expiring(extra: &[&str]) -> Run {
    workers(&[&EXPIRING[..], extra].concat())
}

/// [`expiring`] at `LIMITS_V1`'s drain grace, for a fault that never answers its `Cancel`: its
/// drain can end only by that grace, the product's bound on a holder that does not stop, so the
/// run waits it out once.
fn unanswered(extra: &[&str]) -> Run {
    let grace = (LIMITS_V1.grace_ns / 1_000_000).to_string();
    pool(&grace, &[&EXPIRING[..], extra].concat())
}

/// The local run every distributed one is compared with, once for this binary ([`kept`]): one that
/// ended COULD NOT RUN or failed ends every comparison with its message ([`same_as`]).
fn local() -> &'static UncheckedBundle {
    static LOCAL: OnceLock<Result<UncheckedBundle, String>> = OnceLock::new();
    kept(&LOCAL, || {
        let r = run(&[]);
        assert_eq!(r.code, Some(0), "{}\n{}", r.summary, r.stderr);
        said_local_steps(&r, true);
        r.bundle()
    })
}

/// `init`'s value in `cell`, `init` run once however it ends. `OnceLock::get_or_init` leaves the
/// cell empty when `init` panics, so its next caller would run it again: a retry. A panic is kept
/// as its message instead.
///
/// # Panics
/// When `init` panicked, in every caller, with its message first (so a COULD NOT RUN keeps its
/// prefix), then a line saying it was kept.
#[track_caller]
fn kept<T>(cell: &OnceLock<Result<T, String>>, init: impl FnOnce() -> T) -> &T {
    let kept = cell.get_or_init(|| catch_unwind(AssertUnwindSafe(init)).map_err(|p| message(&*p)));
    match kept {
        Ok(v) => v,
        Err(why) => panic!("{why}\n(the run this binary keeps ended, and is not run again)"),
    }
}

/// What a panic said: its `String` or `&str` payload.
fn message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// `b`'s parts with the timings, the toolchain line and the mode set to `l`'s, the local run's.
fn aside(mut b: UncheckedBundle, l: &UncheckedBundle) -> UncheckedBundle {
    b.timings.clone_from(&l.timings);
    b.toolchain.clone_from(&l.toolchain);
    b.selection.mode = l.selection.mode;
    b
}

/// [`same_as`] the local run.
fn same_as_local(r: &Run, mode: Mode, own: impl FnOnce(&Run)) {
    same_as(r, mode, own, local);
}

/// `r` ended 0 with every worker reaped, `own`'s checks of it held and its bundle in `mode`, then
/// that bundle is `reference`'s aside. `reference` is read after every check of `r` alone, so a
/// local run that ended ([`kept`]: COULD NOT RUN on a starved host) ends only the comparison,
/// never `r`'s exit, its reaping, `own`'s checks, its bundle's read or its mode.
fn same_as<'a>(
    r: &Run,
    mode: Mode,
    own: impl FnOnce(&Run),
    reference: impl FnOnce() -> &'a UncheckedBundle,
) {
    assert_eq!(r.code, Some(0), "{}\n{}", r.summary, r.stderr);
    r.reaped_all();
    own(r);
    let b = r.bundle();
    assert_eq!(b.selection.mode, mode, "{}", r.summary);
    let l = reference();
    assert_eq!(&aside(b, l), l, "{}", r.summary);
}

fn fired_once(r: &Run, spec: &str) {
    let fired = r.fired();
    assert_eq!(
        fired.get(spec),
        Some(&1),
        "{spec}: {fired:?}\n{}",
        r.summary
    );
    assert_eq!(fired.len(), 1, "{fired:?}");
}

/// The void cancelled live leases and each drain ended by its holder's acknowledgement: every
/// worker killed is the offender or a holder retired by its ack (a retired worker is killed,
/// design §11.5), and no drain timed out. Its callers' hello and drain grace are past the run
/// ([`workers`], [`pool`]), so a loaded host times neither out: a loss is a worker's own end.
fn drained_by_acks(r: &Run) {
    let cancelled = r.count("cancelled");
    assert!(
        cancelled >= 1,
        "the fault fired while another lease was live\n{}",
        r.summary
    );
    assert_eq!(r.processes("killed"), 1 + cancelled, "{}", r.summary);
    assert_eq!(r.count("lost"), 0, "no drain timed out\n{}", r.summary);
}

fn voided(r: &Run, refusal: &str) {
    assert_eq!(r.code, Some(1), "{}\n{}", r.summary, r.stderr);
    assert!(
        r.summary
            .contains(&format!("aos-search result=void refusal={refusal} ")),
        "{refusal}:\n{}",
        r.summary
    );
    assert!(
        !r.out.join("plans.bundle").exists(),
        "a void leaves no bundle"
    );
    r.reaped_all();
}

/// The detector's steps, a worker frame's and the coordinator's own: each counts once, and only
/// within the ledger's bounds, [`PHASES`], the evaluator built or a candidate of [`SIX`] trained;
/// no other line counts.
#[test]
fn a_step_counts_once_and_only_within_the_ledgers_bounds() {
    let mut taken = BTreeSet::new();
    let line = |step: &str| format!("aos-workers frame from=0 step={step}\n");
    for step in [
        "hello.0",
        "1.0.1.0",
        "1.0.1.16",
        "1.0.1.end",
        "1.0.3.0",
        "2.53.2.end",
    ] {
        assert!(advanced(&line(step), &mut taken), "{step}: new");
        assert!(!advanced(&line(step), &mut taken), "{step}: said again");
    }
    let own = |step: &str| format!("aos-workers step={step}\n");
    for step in ["1.check", "1.held-out", "1.verify", "2.check"] {
        assert!(advanced(&own(step), &mut taken), "{step}: new");
        assert!(!advanced(&own(step), &mut taken), "{step}: said again");
    }
    for step in ["1.write", "x.check", "1", "check", "1.check.0", "-"] {
        assert!(!advanced(&own(step), &mut taken), "{step}: no phase");
    }
    // A run without a pool: its evaluator built, then each of SIX's candidates trained.
    let trained = (0..SIX.lines().count()).map(|n| format!("1.trained.{n}"));
    for step in std::iter::once("0.evaluator".to_owned()).chain(trained) {
        assert!(advanced(&own(&step), &mut taken), "{step}: new");
        assert!(!advanced(&own(&step), &mut taken), "{step}: said again");
    }
    for step in [
        format!("1.trained.{}", SIX.lines().count()),
        "1.trained.x".to_owned(),
        "x.trained.0".to_owned(),
        "1.trained".to_owned(),
        "1.trained.0.0".to_owned(),
        "evaluator".to_owned(),
        "1.evaluator".to_owned(),
    ] {
        assert!(!advanced(&own(&step), &mut taken), "{step}: no step");
    }
    for step in [
        format!("1.0.{}.0", LIMITS_V1.max_attempts + 1),
        format!("1.0.1.{}", LIMITS_V1.items),
        format!("hello.{}", LIMITS_V1.max_workers),
        "1.0.0.0".to_owned(),
        "x.0.1.0".to_owned(),
        "1.0.1".to_owned(),
        "-".to_owned(),
    ] {
        assert!(!advanced(&line(&step), &mut taken), "{step}: out of bounds");
    }
    assert!(
        !advanced("aos-workers worker=0 pid=1\n", &mut taken),
        "a pid line"
    );
    assert!(!advanced("aos-search: any other line\n", &mut taken));
}

/// A stretch is judged on what the run's processes did in it: each one's growth in CPU time and in
/// runnable time less CPU time, all of one that started since, none of one gone. It was starved
/// only if they waited for a core half the stretch and longer than they ran, over the whole of it
/// and at its end.
#[test]
fn a_stretch_is_judged_on_the_runs_wait_for_a_core() {
    let s = |n: u64| n * 1_000_000_000;
    let secs = Duration::from_secs;
    let before: Used = [
        ((1, 7), (s(2), s(3))),
        ((2, 7), (s(1), s(1))),
        ((3, 7), (s(1), s(1))),
    ]
    .into();
    let now: Used = [
        ((1, 7), (s(5), s(46))), // starved: runnable 43 s, on a core 3 s of it
        ((2, 7), (s(1), s(1))),  // blocked: neither grew
        ((4, 7), (s(1), s(9))),  // started since: all of its 8 s wait
    ]
    .into();
    let starved = spent(&before, &now);
    let (waited, ran) = (secs(48), secs(4));
    assert_eq!(starved, Spent { waited, ran });
    assert!(starved.starved(secs(50)));
    let blocked = spent(&now, &now);
    let (waited, ran) = (Duration::ZERO, Duration::ZERO);
    assert_eq!(blocked, Spent { waited, ran });
    assert!(!blocked.starved(secs(50)), "a hung run waits for no core");
    // Four threads spinning for 50 s on a host just past its CPUs: each waits 9 s of it, 36 s
    // between them, past half the stretch, and runs 41 s.
    let spinning: Used = [((5, 7), (s(4 * 41), s(4 * 50)))].into();
    let spun = spent(&Used::new(), &spinning);
    let (waited, ran) = (secs(36), secs(164));
    assert_eq!(spun, Spent { waited, ran });
    assert!(!spun.starved(secs(50)), "a spinning run was given cores");
    // Workers whose evaluator builds starved beside a stopped coordinator and then blocked: the
    // whole stretch waited (32 s, measured at load 160), its last 10 s did not.
    let (waited, ran) = (secs(32), secs(3));
    let early = Spent { waited, ran };
    assert!(!starved_through(&[(secs(50), early), (secs(10), blocked)]));
    let (waited, ran) = (secs(9), secs(1));
    let still = Spent { waited, ran };
    assert!(starved_through(&[(secs(50), early), (secs(10), still)]));
    assert!(
        !starved_through(&[(secs(50), early)]),
        "a stretch the detector did not read at its end is not the machine's"
    );
}

/// A stretch the detector ended (a run's next step, or the signal tests' wait for every worker's
/// hello) is the machine's only when it was starved through, and otherwise hung; either names
/// what it waited for. Judged without a COULD NOT RUN: its words anywhere in a failed test's
/// output make `verify` read the failure as the machine's.
#[test]
fn a_stretch_past_the_detector_is_could_not_run_only_when_starved_through() {
    let secs = Duration::from_secs;
    let (waited, ran) = (secs(40), secs(4));
    let starved = Spent { waited, ran };
    let (waited, ran) = (secs(9), secs(1));
    let still = Spent { waited, ran };
    let blocked = Spent {
        waited: Duration::ZERO,
        ran: Duration::ZERO,
    };
    let said = "a fake stretch within 60s";
    let crowded = verdict(
        "a fake stretch",
        MUST_HAPPEN,
        &[(secs(50), starved), (secs(10), still)],
    );
    assert!(
        matches!(&crowded, Verdict::Starved(why) if why.starts_with(said)),
        "{crowded:?}"
    );
    for spans in [&[(secs(50), starved), (secs(10), blocked)][..], &[]] {
        let hung = verdict("a fake stretch", MUST_HAPPEN, spans);
        assert!(
            matches!(&hung, Verdict::Hung(why) if why.starts_with(said)),
            "{hung:?}"
        );
    }
}

/// What `f`'s panic said; `f` must panic.
fn ends(f: impl FnOnce()) -> String {
    let p = catch_unwind(AssertUnwindSafe(f)).expect_err("it ends its caller");
    message(&*p)
}

/// A kept run that ended ends its first caller and every later one with its message first (as a
/// COULD NOT RUN's sentinel leads it), and none runs it again. The init's panic is a plain one,
/// never the sentinel, which would make this test's own failure read as the machine's.
#[test]
fn a_reference_that_ended_is_never_run_again() {
    let ran = AtomicUsize::new(0);
    let cell: OnceLock<Result<(), String>> = OnceLock::new();
    let call = || {
        ends(|| {
            kept(&cell, || {
                ran.fetch_add(1, Ordering::Relaxed);
                panic!("a fake crowd")
            });
        })
    };
    for why in [call(), call()] {
        assert!(why.starts_with("a fake crowd"), "{why}");
    }
    assert_eq!(ran.load(Ordering::Relaxed), 1, "run once");
}

/// A bundle whose parts are all empty but its selection's `mode`, which is all [`same_as`] reads
/// of a run's bundle before the comparison.
fn a_bundle_in(mode: Mode) -> UncheckedBundle {
    let id = |kind| ArtifactId::new(kind, "", 0, b"");
    let none = Digest([0; 32]);
    UncheckedBundle {
        policy: id(ArtifactKind::Policy),
        base: id(ArtifactKind::Code),
        candidate: id(ArtifactKind::Code),
        plan: Vec::new(),
        proposer: ProposerId {
            name: String::new(),
            version: 0,
            config: none,
        },
        round: 0,
        index: 0,
        stranded_light: id(ArtifactKind::Contract),
        lane_fidelity: id(ArtifactKind::Contract),
        interpreter: String::new(),
        lane: String::new(),
        oracle: String::new(),
        corpus: none,
        schema: id(ArtifactKind::Schema),
        profile: id(ArtifactKind::Profile),
        build: String::new(),
        toolchain: String::new(),
        rows: Vec::new(),
        selection: SelectionPart {
            mode,
            stats: SearchStats::default(),
            held_out_looks: 0,
            train: Compared::default(),
            held_out: Compared::default(),
            s0: Compared::default(),
            useful: false,
            gain_ppm: 0,
            s0_gain_ppm: 0,
        },
        publication: None,
        timings: Vec::new(),
        grade: String::new(),
        tested_assumptions: Vec::new(),
        plan_assumptions: Vec::new(),
    }
}

/// A comparison reads the local run only once every check of the run held, so a local run that
/// ended (COULD NOT RUN on a starved host) ends only the comparison: a worker left unreaped, a
/// count its caller checks, or a bundle missing, undecodable or in another mode still fails as
/// itself.
#[test]
fn a_reference_that_ended_masks_only_the_comparison() {
    let ended = || -> &'static UncheckedBundle { panic!("a fake crowd") };
    let finished = |reaped: u64, bundle: Option<&[u8]>| {
        let dir = aterm_tempfile::tempdir().expect("a scratch dir");
        let out = dir.path().join("out");
        if let Some(bytes) = bundle {
            std::fs::create_dir_all(&out).expect("an out dir");
            std::fs::write(out.join("plans.bundle"), bytes).expect("a bundle written");
        }
        Run {
            code: Some(0),
            summary: format!("workers processes spawned=3 reaped={reaped}\n"),
            stderr: String::new(),
            out,
            _dir: dir,
        }
    };
    let unreaped = ends(|| same_as(&finished(2, None), Mode::Distributed, |_| {}, ended));
    assert!(unreaped.contains("left: 2"), "{unreaped}");
    let own = ends(|| {
        same_as(
            &finished(3, None),
            Mode::Distributed,
            |_| panic!("its own count"),
            ended,
        );
    });
    assert_eq!(own, "its own count");
    let missing = ends(|| same_as(&finished(3, None), Mode::Distributed, |_| {}, ended));
    assert!(missing.starts_with("no bundle"), "{missing}");
    let garbled = finished(3, Some(b"not a bundle"));
    let undecodable = ends(|| same_as(&garbled, Mode::Distributed, |_| {}, ended));
    assert!(undecodable.starts_with("a bundle"), "{undecodable}");
    let searched = finished(3, Some(&a_bundle_in(Mode::Search).encode()));
    let moded = ends(|| same_as(&searched, Mode::Distributed, |_| {}, ended));
    assert!(moded.contains("left: Search"), "{moded}");
    // Every check of the run held: the comparison is made, and ends with the local run's message.
    let distributed = finished(3, Some(&a_bundle_in(Mode::Distributed).encode()));
    let compared = ends(|| same_as(&distributed, Mode::Distributed, |_| {}, ended));
    assert_eq!(compared, "a fake crowd");
}

/// A kept run that returned is run once, and every caller gets the one value.
#[test]
fn a_reference_that_returned_is_run_once_and_shared() {
    let ran = AtomicUsize::new(0);
    let cell = OnceLock::new();
    let call = || {
        kept(&cell, || {
            ran.fetch_add(1, Ordering::Relaxed);
            ran.load(Ordering::Relaxed)
        })
    };
    let (first, second) = (call(), call());
    assert!(std::ptr::eq(first, second), "one value");
    assert_eq!((*first, ran.load(Ordering::Relaxed)), (1, 1));
}

#[test]
fn honest_workers_give_the_local_result() {
    let r = workers(&[]);
    same_as_local(&r, Mode::Distributed, |r| {
        for key in [
            "lost",
            "expired",
            "stale",
            "duplicate",
            "late_progress",
            "foreign",
            "retries",
        ] {
            assert_eq!(r.count(key), 0, "{key}\n{}", r.summary);
        }
        assert_eq!(r.count("joined"), 3);
        assert_eq!(r.count("leases"), 6);
        assert_eq!(r.processes("killed"), 0, "{}", r.summary);
        assert!(r.fired().is_empty());
        assert!(r.summary.contains("admitted_claimed=3"), "{}", r.summary);
        let pids = r
            .stderr
            .lines()
            .filter(|l| l.starts_with("aos-workers worker="))
            .count();
        assert_eq!(pids, 3, "{}", r.stderr);
        // The coordinator says each stretch of its own work after the last claim as a step, and
        // no evaluator or candidate step, which its workers' frames take.
        let own = own_steps(r);
        assert_eq!(own, ["1.check", "1.held-out", "1.verify"], "{}", r.stderr);
    });
}

#[test]
fn check_all_judges_every_claim_and_records_search() {
    let r = workers(&["--check", "all"]);
    same_as_local(&r, Mode::Search, |r| {
        assert_eq!(
            r.field("  claimed", "claimed_only").as_deref(),
            Some("0"),
            "{}",
            r.summary
        );
        assert!(r.summary.contains(" admitted=3 "), "{}", r.summary);
    });
}

#[test]
fn threads_give_the_local_result() {
    let r = run(&["--threads", "3"]);
    assert_eq!(r.code, Some(0), "{}\n{}", r.summary, r.stderr);
    said_local_steps(&r, false);
    let b = r.bundle();
    assert_eq!(b.selection.mode, Mode::Search);
    let l = local();
    assert_eq!(&aside(b, l), l);
}

/// A search with no winner ([`SIX`]'s slots 2–4, each refused) reads no held-out split and
/// verifies no bundle, so it says neither step: a run without a pool says its evaluator built and
/// each candidate trained, a pool run its check of the claims, and each fails with no bundle.
#[test]
fn a_search_with_no_winner_says_no_held_out_or_verify() {
    let refused: String = SIX
        .lines()
        .skip(2)
        .take(3)
        .map(|l| l.to_owned() + "\n")
        .collect();
    let serial = run_on(&refused, &[]);
    let pooled = run_on(
        &refused,
        &[
            "--workers",
            "3",
            "--hello-ms",
            PAST_THE_RUN,
            "--grace-ms",
            PAST_THE_RUN,
        ],
    );
    for (r, steps) in [
        (
            &serial,
            &["0.evaluator", "1.trained.0", "1.trained.1", "1.trained.2"][..],
        ),
        (&pooled, &["1.check"]),
    ] {
        assert_eq!(r.code, Some(1), "{}\n{}", r.summary, r.stderr);
        assert!(
            r.summary
                .contains("aos-search result=fail searches=1 winners=0\n"),
            "{}",
            r.summary
        );
        assert!(!r.out.join("plans.bundle").exists(), "{}", r.summary);
        assert_eq!(own_steps(r), steps, "{}", r.stderr);
    }
    pooled.reaped_all();
}

#[test]
fn a_killed_worker_costs_a_retry_only() {
    let r = workers(&["--fault", "kill@1.0.1:3"]);
    same_as_local(&r, Mode::Distributed, |r| {
        fired_once(r, "kill@1.0.1:3");
        assert!(r.count("lost") >= 1, "{}", r.summary);
        assert!(r.count("retries") >= 1, "{}", r.summary);
    });
}

#[test]
fn a_hung_worker_expires_and_is_retried() {
    let r = unanswered(&["--fault", "hang@1.1.1:2"]);
    same_as_local(&r, Mode::Distributed, |r| {
        fired_once(r, "hang@1.1.1:2");
        assert!(r.count("expired") >= 1, "{}", r.summary);
        assert!(r.count("retries") >= 1, "{}", r.summary);
    });
}

/// The slow worker judges its last items only after the lease it stopped on expires; the drain's
/// grace is past the run ([`PAST_THE_RUN`]), not a latency budget, so a loaded host cannot retire
/// it before its result is compared.
#[test]
fn a_slow_worker_is_stale_and_its_late_progress_dropped() {
    let r = expiring(&["--fault", "slow@1.0.1:3"]);
    same_as_local(&r, Mode::Distributed, |r| {
        fired_once(r, "slow@1.0.1:3");
        assert!(r.count("late_progress") >= 1, "{}", r.summary);
        assert!(r.count("stale") >= 1, "{}", r.summary);
    });
}

/// The late worker answers only once the lease it held is cancelled, inside the drain; the
/// drain's grace is past the run ([`PAST_THE_RUN`]), not a latency budget, so a loaded host
/// cannot lose the answer before it is compared (as it lost the late lie's: `lost=1`, the search
/// passing).
#[test]
fn a_late_equal_result_is_stale() {
    let r = expiring(&["--fault", "late@1.1.1"]);
    same_as_local(&r, Mode::Distributed, |r| {
        fired_once(r, "late@1.1.1");
        assert!(r.count("stale") >= 1, "{}", r.summary);
    });
}

#[test]
fn an_equal_resend_is_a_duplicate() {
    let r = workers(&["--fault", "dup@1.0.1"]);
    same_as_local(&r, Mode::Distributed, |r| {
        fired_once(r, "dup@1.0.1");
        assert_eq!(r.count("duplicate"), 1, "{}", r.summary);
    });
}

/// The late lie is compared only if the drain waits for it (`a_late_equal_result_is_stale`).
#[test]
fn a_late_lie_voids() {
    let r = expiring(&["--fault", "late-lie@1.1.1"]);
    voided(&r, "Differs");
    fired_once(&r, "late-lie@1.1.1");
}

#[test]
fn a_differing_resend_voids_and_drains_by_acks() {
    let r = workers(&["--fault", "dup-lie@1.0.1"]);
    voided(&r, "Differs");
    fired_once(&r, "dup-lie@1.0.1");
    drained_by_acks(&r);
}

#[test]
fn a_refusal_claimed_for_an_admitted_plan_voids() {
    let r = workers(&["--fault", "lie-refusal@1.0.1"]);
    voided(&r, "Contradicted");
    fired_once(&r, "lie-refusal@1.0.1");
}

#[test]
fn a_forged_best_voids() {
    let r = workers(&["--fault", "lie-best@1.0.1"]);
    voided(&r, "Contradicted");
    fired_once(&r, "lie-best@1.0.1");
}

#[test]
fn an_inflated_winner_is_found_by_the_dominance_check() {
    let r = workers(&["--fault", "inflate@1.0.1"]);
    voided(&r, "Contradicted");
    fired_once(&r, "inflate@1.0.1");
    // Raised past Shipped's claim, slot 0's is no longer the claimed best: Shipped is judged in
    // full and slot 0 is reached only by the dominance check, after more than the best's 17 rows.
    assert!(
        r.summary.contains("job 0's claim is contradicted"),
        "{}",
        r.summary
    );
    let judged: u64 = r
        .field("  claimed ", "local_judged")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no local_judged:\n{}", r.summary));
    assert!(judged > 17, "{}", r.summary);
}

#[test]
fn garbage_voids_and_drains_by_acks() {
    // Slot 5 is leased last, after the refusals, while slots 0 and 1 are still being judged.
    let r = workers(&["--fault", "garbage@1.5.1"]);
    voided(&r, "Frame");
    fired_once(&r, "garbage@1.5.1");
    drained_by_acks(&r);
}

#[test]
fn a_flood_voids() {
    let r = workers(&["--fault", "flood@1.0.1"]);
    voided(&r, "NoProgress");
    fired_once(&r, "flood@1.0.1");
}

#[test]
fn a_foreign_worker_is_refused_alone() {
    let r = workers(&["--fault", "foreign@0"]);
    same_as_local(&r, Mode::Distributed, |r| {
        assert_eq!(r.count("foreign"), 1, "{}", r.summary);
        assert!(
            r.summary.contains("workers foreign worker=0 field=build"),
            "{}",
            r.summary
        );
        // Its one worker's hello must come, as a pool run's: only that refusal leaves it none.
        let alone = run(&[
            "--workers",
            "1",
            "--hello-ms",
            PAST_THE_RUN,
            "--fault",
            "foreign@0",
        ]);
        assert_eq!(alone.count("foreign"), 1, "{}", alone.summary);
        voided(&alone, "NoWorkers");
    });
}

/// A void ends the command (design §11.3): a lie in the plans search, the second, removes the
/// grid's bundle, which was written first, since its admitted count rested on the same pool.
#[test]
fn a_void_in_a_later_search_removes_the_earlier_bundle() {
    let r = workers(&["--grid", "--fault", "lie-best@2.0.1"]);
    voided(&r, "Contradicted");
    fired_once(&r, "lie-best@2.0.1");
    assert!(!r.out.join("grid.bundle").exists(), "{}", r.summary);
    assert!(
        r.summary
            .lines()
            .any(|l| l.starts_with("aos-search removed ") && l.contains("grid.bundle")),
        "{}",
        r.summary
    );
    assert!(
        r.summary.contains(" searches=2 winners=0\n"),
        "{}",
        r.summary
    );
}

/// A lie in the grid search, the first, runs no plans search: no lease of the second is issued.
/// Grid slot 6 is the grid's winner, judged through every item, so its first progress frame,
/// where `garbage` fires, is sent.
#[test]
fn a_void_in_an_earlier_search_runs_no_later_one() {
    let r = workers(&["--grid", "--fault", "garbage@1.6.1"]);
    voided(&r, "Frame");
    fired_once(&r, "garbage@1.6.1");
    assert!(!r.out.join("grid.bundle").exists(), "{}", r.summary);
    assert!(
        r.summary.contains(" searches=1 winners=0\n"),
        "{}",
        r.summary
    );
}

/// A lie the shutdown drain reads voids the command too: `close-lie` holds its lie until the
/// coordinator closes the worker's stdin, which its shutdown does after the last search wrote its
/// bundle, so only the drain can read it. The bundle it wrote is removed (`voided` finds none).
#[test]
fn a_lie_the_shutdown_drain_reads_removes_the_bundle() {
    let r = workers(&["--fault", "close-lie@1.0.1"]);
    voided(&r, "Differs");
    fired_once(&r, "close-lie@1.0.1");
    assert!(
        r.summary
            .lines()
            .any(|l| l.starts_with("aos-search removed ") && l.contains("plans.bundle")),
        "the search wrote its bundle before the drain read the lie\n{}",
        r.summary
    );
}

/// Every lie a worker holds is sent at the close, in the order held: on one worker, which draws
/// every lease, `close-lie` on slots 0 and 1 voids on slot 0's, the first held, and each spec
/// fires once (a fault's line names only the spec keyed by the lease it fired on).
#[test]
fn every_held_lie_is_sent_at_the_close_in_order() {
    let r = run(&[
        "--workers",
        "1",
        "--hello-ms",
        PAST_THE_RUN,
        "--grace-ms",
        PAST_THE_RUN,
        "--fault",
        "close-lie@1.0.1",
        "--fault",
        "close-lie@1.1.1",
    ]);
    voided(&r, "Differs");
    let fired = r.fired();
    assert_eq!(fired.get("close-lie@1.0.1"), Some(&1), "{fired:?}");
    assert_eq!(fired.get("close-lie@1.1.1"), Some(&1), "{fired:?}");
    assert_eq!(fired.len(), 2, "{fired:?}");
    assert!(
        r.summary.contains("for search 1 slot 0 differs"),
        "the first lie held is the first read\n{}",
        r.summary
    );
}

/// The signal tests' run: three workers, each hello past the run, and slot 2's lease (refused at
/// item 0, so judged at once) held until its `Cancel` at a window past the run (`late@1.2.1`). A
/// search is complete once every job is claimed, whether or not every worker has said hello, so a
/// worker whose hello lagged the others' work would be accepted only in the shutdown, where a
/// signal can land after the coordinator's last look for one, or never (its lifeline ends it
/// first): held open, the pool accepts every hello, and the signal lands, while it runs. The
/// holder answers a void's `Cancel` with its result, which the drain drops, so [`LIMITS_V1`]'s
/// grace ends that drain with a kill.
const HELD: [&str; 8] = [
    "--workers",
    "3",
    "--hello-ms",
    PAST_THE_RUN,
    "--lease-ms",
    PAST_THE_RUN,
    "--fault",
    "late@1.2.1",
];

/// The pids the coordinator `child` printed, once all `n` are on its `stderr`, under the detector
/// `budget` (as [`stretch`]'s): each worker's accepted hello, which follows its own evaluator
/// build and the coordinator's, so the wait is a [`stretch`] of the run. A run [`HELD`] open
/// cannot end before its signal: one that ended first fails, whatever pid lines its stderr has
/// yet to give.
///
/// # Panics
/// When the run ended first, at once, naming its end; past `budget`, by [`stretch`].
#[track_caller]
fn pids(budget: Duration, child: &mut Child, stderr: &Mutex<String>, n: usize) -> Vec<u32> {
    let said = stretch(
        budget,
        child,
        "aos-search did not say every worker's pid line",
        |child| {
            if let Some(end) = child.try_wait().ok().flatten() {
                return Some(Err(end));
            }
            let text = stderr.lock().unwrap_or_else(|p| p.into_inner());
            let pids: Vec<u32> = text
                .lines()
                .filter_map(|l| l.strip_prefix("aos-workers worker="))
                .filter_map(|l| l.split_once(" pid=")?.1.parse().ok())
                .collect();
            (pids.len() == n).then_some(Ok(pids))
        },
    );
    match said {
        Ok(pids) => pids,
        Err(end) => {
            let text = stderr.lock().unwrap_or_else(|p| p.into_inner());
            panic!("aos-search ended ({end}) before every worker's pid line:\n{text}")
        }
    }
}

/// The signal tests' wait for every worker's pid line is a [`stretch`] of the run: past its
/// detector (here at once, before any read, so not the machine's) the run is killed and reaped
/// and the wait fails, naming it; a run that ended before every pid line fails at once, naming
/// its end.
#[test]
fn the_wait_for_every_pid_line_is_a_stretch_that_ends_with_the_run() {
    let unsaid = Mutex::new(String::new());
    let _slot = slot();
    let (mut child, _dir, _out) = spawn(SIX, &HELD);
    let why = ends(|| drop(pids(Duration::ZERO, &mut child, &unsaid, 3)));
    let wait = "aos-search did not say every worker's pid line within 0ns";
    assert!(why.starts_with(wait), "{why}");
    let end = child.try_wait().ok().flatten();
    assert_eq!(end.and_then(|s| s.signal()), Some(9), "killed: {end:?}");
    // Refused at its arguments, before any worker is spawned.
    let (mut child, _dir, _out) = spawn(SIX, &["--workers", "x"]);
    let why = ends(|| drop(pids(MUST_HAPPEN, &mut child, &unsaid, 3)));
    let first = "aos-search ended (exit status: 2) before every worker's pid line";
    assert!(why.starts_with(first), "{why}");
}

fn tee(mut r: impl std::io::Read + Send + 'static) -> std::sync::Arc<Mutex<String>> {
    let text = std::sync::Arc::new(Mutex::new(String::new()));
    let into = std::sync::Arc::clone(&text);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            into.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push_str(&String::from_utf8_lossy(buf.get(..n).unwrap_or_default()));
        }
    });
    text
}

fn signal(sig: &str, pid: u32) -> bool {
    aterm_uds::spawnfd::command("/bin/kill")
        .args([sig, &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// TERM ends the command and reaps its workers. It lands while [`HELD`]'s lease is open, whose
/// holder answers its `Cancel` with its result, not an acknowledgement, and the other workers may
/// be idle by then, so this counts no cancellation acks.
#[test]
fn sigterm_voids_and_reaps() {
    let _slot = slot();
    let (mut child, dir, out) = spawn(SIX, &HELD);
    let stdout = drain(child.stdout.take().expect("piped"));
    let stderr = tee(child.stderr.take().expect("piped"));
    let _ = pids(MUST_HAPPEN, &mut child, &stderr, 3);
    assert!(signal("-TERM", child.id()));
    let status = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(50), || {
        child.try_wait().ok().flatten()
    })
    .expect("ends after SIGTERM");
    let r = Run {
        code: status.code(),
        summary: stdout.join().unwrap_or_default(),
        stderr: stderr.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        out,
        _dir: dir,
    };
    assert_eq!(r.code, Some(143), "{}\n{}", r.summary, r.stderr);
    assert!(
        r.summary
            .contains("aos-search result=void refusal=Cancelled "),
        "{}",
        r.summary
    );
    r.reaped_all();
    assert!(!r.out.join("plans.bundle").exists());
}

#[test]
fn sigkill_to_the_coordinator_ends_every_worker_by_the_lifeline() {
    let _slot = slot();
    let (mut child, _dir, _out) = spawn(SIX, &HELD);
    let _stdout = drain(child.stdout.take().expect("piped"));
    let stderr = tee(child.stderr.take().expect("piped"));
    let pids = pids(MUST_HAPPEN, &mut child, &stderr, 3);
    assert!(signal("-KILL", child.id()));
    let _ = child.wait();
    until_within(MUST_HAPPEN, "every worker gone", || {
        pids.iter().all(|&p| !signal("-0", p)).then_some(())
    });
}
