// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search --threads` as a process: the six-line plans file on three threads against one
//! serial run. Each run is held to the one-minute hang detector on its next new step or its end,
//! and never retried. It says its steps under `--frames` (its evaluator built, each candidate
//! trained, the winner's held-out read and its verify), so its detector restarts at each and never
//! spans its whole length. A run is CPU-bound, so one past the detector whose threads waited for a
//! core at least half as long as they were watched, and longer than they ran, from 10 s into the
//! stretch and again over its last 10 s ([`starved_through`]), is COULD NOT RUN, naming both; one
//! that waited less, or ran longer, hung, and fails however loaded the host.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use aterm_effects::aos_momentum::bundle::UncheckedBundle;
use aterm_effects::aos_momentum::search::Mode;
use aterm_spec::wait::MUST_HAPPEN;
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

/// Take one slot from the shared subprocess pool (`aterm_spec::subprocess_lane`), so a threaded
/// search shares the stage's subprocess budget.
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
    fn bundle(&self) -> UncheckedBundle {
        let bytes = std::fs::read(self.out.join("plans.bundle"))
            .unwrap_or_else(|e| panic!("no bundle ({e}):\n{}", self.summary));
        UncheckedBundle::decode(&bytes).expect("a bundle")
    }
}

/// A spawned run, killed and reaped when dropped, however its test ends.
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

/// `aos-search --plans` with [`SIX`] as its file, and `args`.
fn spawn(args: &[&str]) -> (Spawned, aterm_tempfile::TempDir, PathBuf) {
    // macOS's first-exec check of a fresh build is waited out here, outside the run's hang
    // detector.
    let lab = aterm_tempfile::bin_exe("aterm-lab");
    aterm_spec::wait::first_exec(&lab, &["--help"]);
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let plans_file = dir.path().join("plans.txt");
    std::fs::write(&plans_file, SIX).expect("the plans file");
    let out = dir.path().join("out");
    let mut cmd = aterm_uds::spawnfd::command(&lab);
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

/// The stretches of a search's own work at its end, which `--frames` says as each begins
/// (`aos-search step=G.<phase>`): the winner's held-out read and its verify.
const PHASES: [&str; 2] = ["held-out", "verify"];

/// Whether stderr's `line` names a step of the run not taken before (`--frames`): its evaluator
/// built (`0.evaluator`), one of a search's [`PHASES`], or a search's candidate trained
/// (`G.trained.N`, `N` below [`SIX`]'s slots). The command's plans fix its steps, so a run that
/// keeps saying a step without finishing takes no new one, and the detector ends it.
fn advanced(line: &str, taken: &mut BTreeSet<String>) -> bool {
    let number = |n: &str| n.parse::<u32>().is_ok();
    let Some(step) = line.strip_prefix("aos-search step=") else {
        return false;
    };
    let step = step.trim_end();
    let own = match step.split('.').collect::<Vec<_>>().as_slice() {
        ["0", "evaluator"] => true,
        [search, phase] => number(search) && PHASES.contains(phase),
        [search, "trained", n] => {
            number(search) && n.parse::<usize>().is_ok_and(|n| n < SIX.lines().count())
        }
        _ => false,
    };
    own && taken.insert(step.to_owned())
}

/// What a run's process had used when read: its CPU time and runnable time (on a core or waiting
/// for one), ns, by pid and start.
type Used = BTreeMap<(u32, u64), (u64, u64)>;

/// [`Used`] now, for the run whose process is `run`.
fn used(run: u32) -> Used {
    aterm_sysprobe::procs::processes()
        .map(|sweep| {
            sweep
                .procs
                .into_iter()
                .filter(|p| p.pid == run)
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

/// How far into a stretch without a step the detector reads the run's process: 10 s in, so that a
/// stretch it ends is judged on what it did in the rest of it, and 10 s before it ends, on what it
/// did at its end.
const READS: [Duration; 2] = [
    Duration::from_secs(10),
    MUST_HAPPEN.saturating_sub(Duration::from_secs(10)),
];

/// Whether a stretch past the detector was the machine's, from what the run's process spent from
/// each of its [`READS`] to its end (`spans`): starved ([`Spent::starved`]) from the first read on,
/// and still from the last. A run whose wait was spent early and that then took no step without
/// waiting hung.
fn starved_through(spans: &[(Duration, Spent)]) -> bool {
    spans.len() == READS.len() && spans.iter().all(|&(span, s)| s.starved(span))
}

/// `step`'s value once it has one, polled with `child` under the one-minute detector, the run's
/// process read at each of [`READS`] it reaches. Past that the run is killed and reaped, then
/// judged ([`verdict`]) as `what` it waited for: one cut short of both reads is never the machine's.
///
/// # Panics
/// When `step` gave no value within [`MUST_HAPPEN`]: COULD NOT RUN or a failure, by [`verdict`].
#[track_caller]
fn stretch<T>(child: &mut Child, what: &str, mut step: impl FnMut(&mut Child) -> Option<T>) -> T {
    let run = child.id();
    let since = Instant::now();
    let mut reads: Vec<(Instant, Used)> = Vec::new();
    let next = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(50), || {
        if let Some(v) = step(child) {
            return Some(v);
        }
        if READS
            .get(reads.len())
            .is_some_and(|&at| since.elapsed() >= at)
        {
            // After the sweep: a slow one's numbers are as of its end, so its span starts there.
            let read = used(run);
            reads.push((Instant::now(), read));
        }
        None
    });
    if let Some(v) = next {
        return v;
    }
    let now = used(run);
    let spans: Vec<(Duration, Spent)> = reads
        .iter()
        .map(|(at, before)| (at.elapsed(), spent(before, &now)))
        .collect();
    let _ = child.kill();
    let _ = child.wait();
    match verdict(what, &spans) {
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

/// The verdict on a stretch past the detector, `what` it waited for, from what the run's process
/// spent from each of [`READS`] to its end (`spans`). A run's stretches without a step (the
/// evaluator's build, a candidate's training, the held-out read, the verify) are CPU-bound, and a
/// full verify's load has starved one past the detector, which is the machine's
/// ([`starved_through`]). A run that was not starved hung (blocked, stopped, or spinning on cores
/// it was given), however loaded the host, and fails. Either names `what` and `spans`, and the
/// starved one the host's load.
fn verdict(what: &str, spans: &[(Duration, Spent)]) -> Verdict {
    let detail: String = spans
        .iter()
        .map(|(span, s)| {
            format!(
                "; in its last {span:.1?} its threads waited {:.1?} for a core and ran {:.1?}",
                s.waited, s.ran
            )
        })
        .collect();
    let why = format!("{what} within {MUST_HAPPEN:?}{detail}");
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
    let _slot = slot();
    let (child, dir, out) = spawn(&[args, &["--frames"]].concat());
    finish(child, dir, out)
}

/// A run says its steps: its evaluator built, each of [`SIX`]'s candidates trained (numbered as
/// each finishes, so in order only on one thread: `ordered`), then the winner's held-out read and
/// its verify.
fn said_steps(r: &Run, ordered: bool) {
    let mut own: Vec<&str> = r
        .stderr
        .lines()
        .filter_map(|l| l.strip_prefix("aos-search step="))
        .collect();
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

/// A step counts once, and only as one of [`PHASES`], the evaluator built or a candidate of [`SIX`]
/// trained; no other line counts.
#[test]
fn a_step_counts_once_and_only_when_the_run_names_it() {
    let mut taken = BTreeSet::new();
    let own = |step: &str| format!("aos-search step={step}\n");
    let trained = (0..SIX.lines().count()).map(|n| format!("1.trained.{n}"));
    let phases = ["1.held-out", "1.verify", "2.verify"].map(str::to_owned);
    for step in std::iter::once("0.evaluator".to_owned())
        .chain(trained)
        .chain(phases)
    {
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
        "1.write".to_owned(),
        "x.verify".to_owned(),
        "1".to_owned(),
        "verify".to_owned(),
        "1.verify.0".to_owned(),
        "-".to_owned(),
    ] {
        assert!(!advanced(&own(&step), &mut taken), "{step}: no step");
    }
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
    // A wait spent early, then a run that blocked: the whole stretch waited, its last 10 s did not.
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

/// A stretch the detector ended is the machine's only when it was starved through, and otherwise
/// hung; either names what it waited for. Judged without a COULD NOT RUN: its words anywhere in a
/// failed test's output make `verify` read the failure as the machine's.
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
    let crowded = verdict("a fake stretch", &[(secs(50), starved), (secs(10), still)]);
    assert!(
        matches!(&crowded, Verdict::Starved(why) if why.starts_with(said)),
        "{crowded:?}"
    );
    for spans in [&[(secs(50), starved), (secs(10), blocked)][..], &[]] {
        let hung = verdict("a fake stretch", spans);
        assert!(
            matches!(&hung, Verdict::Hung(why) if why.starts_with(said)),
            "{hung:?}"
        );
    }
}

/// `--threads` selects what the serial search selects and writes the same bundle, timings aside,
/// in the search's own mode, both runs saying each step. It compares the two arms, so a defect both
/// share (`spread_map` losing a result under any spread) is not seen here: the lab's
/// `aos_search::tests::a_small_plans_search_writes_a_verified_bundle_and_its_summary` pins the
/// serial run's counts, and its no-winner half that no bundle is written. No process test holds
/// that a run with no winner says no held-out or verify step.
#[test]
fn threads_give_the_local_result() {
    let threaded = run(&["--threads", "3"]);
    assert_eq!(
        threaded.code,
        Some(0),
        "{}\n{}",
        threaded.summary,
        threaded.stderr
    );
    said_steps(&threaded, false);
    let mut b = threaded.bundle();
    assert_eq!(b.selection.mode, Mode::Search);
    let serial = run(&[]);
    assert_eq!(
        serial.code,
        Some(0),
        "{}\n{}",
        serial.summary,
        serial.stderr
    );
    said_steps(&serial, true);
    let l = serial.bundle();
    b.timings.clone_from(&l.timings);
    assert_eq!(b, l);
}
