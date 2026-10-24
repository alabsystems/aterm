// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search --workers` as processes (design §11.7): the six-line plans file on three workers at
//! a ten-second lease, honest and under each injected fault, against one local run. Each run is
//! held to the one-minute hang detector on its next new step (a worker frame's step its
//! coordinator says under `--frames`, or its end), never on its whole length, and never retried.
//! A run is CPU-bound, so one past the detector on a host whose load exceeds its CPUs is COULD NOT
//! RUN, naming the load.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use aterm_effects::aos_momentum::bundle::UncheckedBundle;
use aterm_effects::aos_momentum::lease::LIMITS_V1;
use aterm_effects::aos_momentum::search::Mode;
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

/// At most two runs at once: each is four processes judging on this host's cores.
fn slot() -> impl Drop {
    struct Slot;
    static GATE: (Mutex<usize>, Condvar) = (Mutex::new(0), Condvar::new());
    impl Drop for Slot {
        fn drop(&mut self) {
            let mut n = GATE.0.lock().unwrap_or_else(|p| p.into_inner());
            *n -= 1;
            GATE.1.notify_one();
        }
    }
    let mut n = GATE.0.lock().unwrap_or_else(|p| p.into_inner());
    while *n >= 2 {
        n = GATE.1.wait(n).unwrap_or_else(|p| p.into_inner());
    }
    *n += 1;
    Slot
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

fn spawn(args: &[&str]) -> (Child, aterm_tempfile::TempDir, PathBuf) {
    // The coordinator and its workers exec this one file: macOS's first-exec check of a fresh
    // build is waited out here, outside the runs' hang detectors.
    aterm_spec::wait::first_exec(
        std::path::Path::new(env!("CARGO_BIN_EXE_aterm-lab")),
        &["--help"],
    );
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let plans = dir.path().join("six.txt");
    std::fs::write(&plans, SIX).expect("the plans file");
    let out = dir.path().join("out");
    let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-lab"));
    cmd.args(["aos-search", "--plans"])
        .arg(&plans)
        .arg("--out")
        .arg(&out)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("run aterm-lab");
    (child, dir, out)
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

/// Whether the coordinator's stderr `line` names a step of the run not taken before (`--frames`):
/// a worker's hello, or item `k < items` or the end of a lease on an attempt within
/// `max_attempts` ([`LIMITS_V1`]). A slot has at most `max_attempts × (items + 1)` such steps and
/// the command's plans fix its slots, so a run that keeps trading frames without finishing (a
/// lease re-run without end, a frame said again) takes no new step, and the detector ends it.
fn advanced(line: &str, taken: &mut BTreeSet<String>) -> bool {
    let Some(step) = line
        .strip_prefix("aos-workers frame ")
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("step=")))
    else {
        return false;
    };
    let number = |n: &str| n.parse::<u32>().is_ok();
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

fn finish(mut child: Child, dir: aterm_tempfile::TempDir, out: PathBuf) -> Run {
    let stdout = drain(child.stdout.take().expect("piped"));
    let (told, stderr) = said(child.stderr.take().expect("piped"));
    let mut taken = BTreeSet::new();
    // The detector waits for the run's next new step ([`advanced`]) or its end.
    let status = loop {
        let next =
            aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(50), || {
                match child.try_wait().ok().flatten() {
                    Some(s) => Some(Some(s)),
                    None => told
                        .try_iter()
                        .any(|line| advanced(&line, &mut taken))
                        .then_some(None),
                }
            });
        match next {
            Some(Some(s)) => break s,
            Some(None) => {}
            None => {
                let _ = child.kill();
                let _ = child.wait();
                // A run's stretches without a frame (the workers' evaluator builds before their
                // hellos, the coordinator's own judging) are CPU-bound, and a full verify's load
                // has stretched a run past the detector; on such a host it times the load.
                if let Some(load) = aterm_spec::host::oversubscribed() {
                    aterm_spec::host::could_not_run(format!(
                        "aos-search took no new step and did not end within {MUST_HAPPEN:?} on \
                         an oversubscribed host ({load})"
                    ));
                }
                panic!("aos-search took no new step and did not end within {MUST_HAPPEN:?}");
            }
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

/// `aos-search --plans SIX` with `args`, to its end; a pool run says its frames (`--frames`).
fn run(args: &[&str]) -> Run {
    let _slot = slot();
    let mut args = args.to_vec();
    if args.contains(&"--workers") {
        args.push("--frames");
    }
    let (child, dir, out) = spawn(&args);
    finish(child, dir, out)
}

fn workers(extra: &[&str]) -> Run {
    let mut args = vec!["--workers", "3", "--lease-ms", "10000"];
    args.extend_from_slice(extra);
    run(&args)
}

/// The local run every distributed one is compared with.
fn local() -> &'static UncheckedBundle {
    static LOCAL: OnceLock<UncheckedBundle> = OnceLock::new();
    LOCAL.get_or_init(|| {
        let r = run(&[]);
        assert_eq!(r.code, Some(0), "{}\n{}", r.summary, r.stderr);
        r.bundle()
    })
}

/// `b`'s parts with the timings, the toolchain line and the mode set to the local run's.
fn aside(mut b: UncheckedBundle) -> UncheckedBundle {
    let l = local();
    b.timings.clone_from(&l.timings);
    b.toolchain.clone_from(&l.toolchain);
    b.selection.mode = l.selection.mode;
    b
}

fn same_as_local(r: &Run, mode: Mode) {
    assert_eq!(r.code, Some(0), "{}\n{}", r.summary, r.stderr);
    let b = r.bundle();
    assert_eq!(b.selection.mode, mode, "{}", r.summary);
    assert_eq!(&aside(b), local(), "{}", r.summary);
    r.reaped_all();
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
/// design §11.5), and no drain timed out.
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

/// The detector's steps: each counts once, and only within the ledger's bounds; no other line
/// counts.
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

#[test]
fn honest_workers_give_the_local_result() {
    let r = workers(&[]);
    same_as_local(&r, Mode::Distributed);
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
}

#[test]
fn check_all_judges_every_claim_and_records_search() {
    let r = workers(&["--check", "all"]);
    same_as_local(&r, Mode::Search);
    assert_eq!(
        r.field("  claimed", "claimed_only").as_deref(),
        Some("0"),
        "{}",
        r.summary
    );
    assert!(r.summary.contains(" admitted=3 "), "{}", r.summary);
}

#[test]
fn threads_give_the_local_result() {
    let r = run(&["--threads", "3"]);
    assert_eq!(r.code, Some(0), "{}", r.summary);
    let b = r.bundle();
    assert_eq!(b.selection.mode, Mode::Search);
    assert_eq!(&aside(b), local());
}

#[test]
fn a_killed_worker_costs_a_retry_only() {
    let r = workers(&["--fault", "kill@1.0.1:3"]);
    same_as_local(&r, Mode::Distributed);
    fired_once(&r, "kill@1.0.1:3");
    assert!(r.count("lost") >= 1, "{}", r.summary);
    assert!(r.count("retries") >= 1, "{}", r.summary);
}

#[test]
fn a_hung_worker_expires_and_is_retried() {
    let r = workers(&["--fault", "hang@1.1.1:2"]);
    same_as_local(&r, Mode::Distributed);
    fired_once(&r, "hang@1.1.1:2");
    assert!(r.count("expired") >= 1, "{}", r.summary);
    assert!(r.count("retries") >= 1, "{}", r.summary);
}

#[test]
fn a_slow_worker_is_stale_and_its_late_progress_dropped() {
    let r = workers(&["--fault", "slow@1.0.1:3"]);
    same_as_local(&r, Mode::Distributed);
    fired_once(&r, "slow@1.0.1:3");
    assert!(r.count("late_progress") >= 1, "{}", r.summary);
    assert!(r.count("stale") >= 1, "{}", r.summary);
}

#[test]
fn a_late_equal_result_is_stale() {
    let r = workers(&["--fault", "late@1.1.1"]);
    same_as_local(&r, Mode::Distributed);
    fired_once(&r, "late@1.1.1");
    assert!(r.count("stale") >= 1, "{}", r.summary);
}

#[test]
fn an_equal_resend_is_a_duplicate() {
    let r = workers(&["--fault", "dup@1.0.1"]);
    same_as_local(&r, Mode::Distributed);
    fired_once(&r, "dup@1.0.1");
    assert_eq!(r.count("duplicate"), 1, "{}", r.summary);
}

#[test]
fn a_late_lie_voids() {
    let r = workers(&["--fault", "late-lie@1.1.1"]);
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
    same_as_local(&r, Mode::Distributed);
    assert_eq!(r.count("foreign"), 1, "{}", r.summary);
    assert!(
        r.summary.contains("workers foreign worker=0 field=build"),
        "{}",
        r.summary
    );
    let alone = run(&["--workers", "1", "--fault", "foreign@0"]);
    voided(&alone, "NoWorkers");
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

/// The pids the coordinator printed, once all `n` are on its stderr.
fn pids(stderr: &Mutex<String>, n: usize) -> Vec<u32> {
    until_within(MUST_HAPPEN, "every worker's pid line", || {
        let text = stderr.lock().unwrap_or_else(|p| p.into_inner());
        let pids: Vec<u32> = text
            .lines()
            .filter_map(|l| l.strip_prefix("aos-workers worker="))
            .filter_map(|l| l.split_once(" pid=")?.1.parse().ok())
            .collect();
        (pids.len() == n).then_some(pids)
    })
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

/// TERM ends the command and reaps its workers. Hello lines establish worker startup, not
/// that a lease is still live when the signal arrives, so this does not count cancellation acks.
#[test]
fn sigterm_voids_and_reaps() {
    let _slot = slot();
    let (mut child, dir, out) = spawn(&["--workers", "3", "--lease-ms", "10000"]);
    let stdout = drain(child.stdout.take().expect("piped"));
    let stderr = tee(child.stderr.take().expect("piped"));
    let _ = pids(&stderr, 3);
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
    let (mut child, _dir, _out) = spawn(&["--workers", "3", "--lease-ms", "10000"]);
    let _stdout = drain(child.stdout.take().expect("piped"));
    let stderr = tee(child.stderr.take().expect("piped"));
    let pids = pids(&stderr, 3);
    assert!(signal("-KILL", child.id()));
    let _ = child.wait();
    until_within(MUST_HAPPEN, "every worker gone", || {
        pids.iter().all(|&p| !signal("-0", p)).then_some(())
    });
}
