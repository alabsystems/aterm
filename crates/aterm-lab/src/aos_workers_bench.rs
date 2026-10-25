// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-workers-bench`: Aos M5b's measurement of useful accepted work per elapsed time, as
//! pre-registered in design §11.8 (`docs/DESIGN-aos-hosted-execution-2026-10-21.md`). It runs six
//! arms of `aos-search --grid --plans F` in a Williams order, computes the validity gates, C0's
//! bundle parity, the criteria C1–C3 and the statistics, and writes them with every raw figure to
//! `DIR/record.txt`. Every figure is measured, never scored: the session gates no build or push.
//! The pure parts are apart from the effects (spawning the lab, `sysctl`, the files), so the tests
//! drive them on captured summaries.
//!
//! Where §11.8 is silent, the runner decides, and fails closed:
//! * Gate 0: every run's summary names one build, which names a commit and does not end `-dirty`;
//!   the lab is a debug build, known only when `--lab` is byte-identical to the runner; and the
//!   lab's digest is the same at the end as at the start. Otherwise the session is void.
//! * Only a gate-2 defect stops a session (§11.8 item 2). A void is decided at the end, so a void
//!   session still runs every round and reports every figure.
//! * C0's reference is the session's first A run that reproduces the M4 record (gate 1). Every run
//!   is compared with it, a run before it waits for it, and a run never compared voids the session.
//! * A gate-2 defect is also: an exit other than 0, a `result=` other than `pass` with 2 winners, a
//!   bundle missing or not decoding, a summary that does not parse or is not the arm it was spawned
//!   as, `reaped != spawned` and `result=void`.
//! * Gate 3 also counts `foreign`, `dropped`, `retries` and `cancelled`, and any fault armed or
//!   fired.
//! * Gate 4 fails when no round is valid. Gate 6 of a smoke run of N rounds needs N − ⌊N/6⌋ of
//!   them valid.
//! * A smoke run (`--rounds` below 12) is never the registered session, and no claim is made from
//!   it.
//! * The fault session follows a valid clean session, or, in a smoke run, one no defect stopped.
//!   Its C0 reference is its own A run, which must reproduce the M4 record or the fault session is
//!   void; it runs `min(3, N)` rounds, and a C0 mismatch, a void or `reaped != spawned` in it is a
//!   defect that stops it.
//! * A fault's added wall time and CPU are the run's less the median of the clean session's valid
//!   P8 runs (the summary lacks the faulted job's lease-to-accept time), reported with the leases
//!   beyond the jobs. Neither is the faulted attempt's wasted work, which §11.8 leaves unreported.
//! * The summary prints each run's trip percentiles, not each job's trip, so the record reports the
//!   median over the valid runs of each run's p50, p95 and p99.
//! * The summary prints neither a worker's spawn-to-Hello time nor its evaluator build, so process
//!   creation less the build is reported as not exposed.
//! * The P8A grid `verify_ms` defect flag is judged per valid round, against that round's A.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_effects::aos_momentum::bundle::UncheckedBundle;
use aterm_effects::aos_momentum::search::Mode;
use aterm_uds::spawnfd::Stdio;

use crate::aos_search::{MAX_BUNDLE_BYTES, read_bounded, word};
use crate::aos_workers::{EXECUTABLE_DOMAIN, available};
use crate::instance::{take_flag, take_value};
use crate::lab_build::check_build;
use crate::paths::{from_caller, paths};
use crate::stop::{self, Stopped};
use crate::sys::digest_file;
use crate::{Fail, Res};

/// The verb's own help (`aos-workers-bench --help`).
pub const HELP: &str = "\
usage: targo --unverified lab aos-workers-bench --plans FILE --out DIR [--lab PATH] [--faults] [--rounds N]

Design §11.8's pre-registered measurement of useful accepted work per elapsed time (MEASURED, never scored; it
gates no build or push). Six arms, each one run of `LAB aos-search --grid --plans FILE --out <fresh dir under DIR>`
plus the arm's flags: A (none), T4 and T8 (--threads 4|8), P4 and P8 (--workers 4|8) and P8A (--workers 8 --check
all). 12 rounds in a Williams order for six arms (six orders, each arm once in every position and after every other
arm once), twice over. Before each run the runner waits 20 s idle and records the 1-minute load average (sysctl -n
vm.loadavg); each run has a 180 s deadline (killed, and invalid, at it), and its wall time (spawn to exit), its
wait_with_rusage (CPU = user + system, peak RSS), its exit status and its summary are recorded. Then the validity
gates 0-6, C0 (each run's two bundles decoded and compared with A's, timings, toolchain line and mode aside, and the
mode rule), the criteria C1-C3 and the reported statistics are computed here and written with every raw figure to
DIR/record.txt, which is printed.

  --plans FILE   the workload: the M4 record's 17-line plans file (config digest e5463d8e4c08)
  --out DIR      absent or empty, OUTSIDE the repository: record.txt and runs/ (each run's out dir, stdout and
                 stderr) go here
  --lab PATH     the lab binary every run execs (default: this executable), built once beforehand; the session
                 is void unless it is byte-identical to this runner and a debug build
  --faults       after the clean session (a valid one, unless a smoke run), the fault session: one A run, then 3
                 rounds of P8 at --lease-ms 10000 with one fault per run (kill@1.6.1:3, hang@1.6.1:3,
                 slow@1.6.1:3, dup@1.6.1: the grid winner's slot 6 of search 1)
  --rounds N     1 to 12 (default 12). Any N below 12 is a SMOKE run: printed as such, never the registered
                 session, and no claim is made from it (its fault session runs min(3, N) rounds)

exit: 0 a valid session (and, with --faults, every fault shown in every run); 1 a void or stopped session, or a
fault not shown; 2 usage; 128+N interrupted by signal N (the record so far is written)
";

/// The registered session's rounds.
pub const REGISTERED_ROUNDS: usize = 12;

/// The idle wait before each run.
pub const IDLE: Duration = Duration::from_secs(20);

/// Each run's deadline (gate 5).
pub const DEADLINE: Duration = Duration::from_secs(180);

/// Gate 5's memory budget: the runner's peak RSS plus the workers' summed peaks.
pub const RSS_BUDGET: u64 = 16 << 30;

const MIB: u64 = 1 << 20;

/// Gate 4: A's wall times across the valid rounds span at most this.
pub const A_SPAN_MAX: f64 = 1.3;

/// The fault session's rounds.
pub const FAULT_ROUNDS: usize = 3;

/// The fault session's lease window, in milliseconds.
pub const FAULT_LEASE_MS: u64 = 10_000;

/// A P8A grid `verify_ms` above this multiple of the same round's A is flagged as a defect.
pub const VERIFY_RATIO_MAX: f64 = 1.5;

/// The evaluator builds of one registered clean round: 1 each for A, T4 and T8, 5 for P4, 9 each
/// for P8 and P8A.
pub const BUILDS_PER_ROUND: u64 = 26;

/// How often the runner looks for the run's exit: the wall time's resolution.
const POLL: Duration = Duration::from_millis(2);

/// The searches of a run, in order, by their bundle files' stems.
pub const SEARCHES: [&str; 2] = ["grid", "plans"];

/// The M4 record's figures (`docs/measured/aos-m4-candidates-2026-10-22.md`, "The grid search" and
/// "The model's proposals"): gate 1's constants.
pub mod m4_record {
    /// The grid's proposals.
    pub const GRID_PROPOSED: u64 = 54;
    /// The grid's admitted plans.
    pub const GRID_ADMITTED: u64 = 18;
    /// The grid's refusals by law, and nothing else refused.
    pub const GRID_REFUSED: [(&str, u64); 3] = [
        ("HotHasWake", 9),
        ("NoStepSlept", 9),
        ("WakeNoLaterThanClear", 18),
    ];
    /// The winner `{SettleAtStep{0}, NextStep}`.
    pub const WINNER_PLAN: &str = "010100000000000000000000";
    /// The grid winner's round.
    pub const GRID_ROUND: u64 = 0;
    /// The grid winner's index.
    pub const GRID_INDEX: u64 = 6;
    /// The held-out `ΣT`.
    pub const HELD_OUT_T: u64 = 5_119;
    /// The held-out `ΣZ`.
    pub const HELD_OUT_Z: u64 = 161;
    /// The gain.
    pub const GAIN_PPM: i64 = 234_255;
    /// The plans file's config digest: the workload.
    pub const PLANS_CONFIG: &str = "e5463d8e4c08";
    /// The plans file's admitted plans.
    pub const PLANS_ADMITTED: u64 = 7;
    /// The plans file winner's index.
    pub const PLANS_INDEX: u64 = 7;
    /// The grid's stepped events, compared only with A and T.
    pub const GRID_STEPPED: u64 = 592_060;
}

use m4_record as m4;

// ---------------------------------------------------------------------------------------------
// Arms and the order.

/// One arm of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arm {
    /// No flag: M4's serial path, the baseline.
    A,
    /// `--threads 4`.
    T4,
    /// `--threads 8`.
    T8,
    /// `--workers 4`.
    P4,
    /// `--workers 8`.
    P8,
    /// `--workers 8 --check all`.
    P8A,
}

impl Arm {
    /// Every arm, in the Williams square's numbering.
    pub const ALL: [Arm; 6] = [Arm::A, Arm::T4, Arm::T8, Arm::P4, Arm::P8, Arm::P8A];

    /// Its name in the record.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arm::A => "A",
            Arm::T4 => "T4",
            Arm::T8 => "T8",
            Arm::P4 => "P4",
            Arm::P8 => "P8",
            Arm::P8A => "P8A",
        }
    }

    /// The flags it adds to the workload's command.
    #[must_use]
    pub const fn flags(self) -> &'static [&'static str] {
        match self {
            Arm::A => &[],
            Arm::T4 => &["--threads", "4"],
            Arm::T8 => &["--threads", "8"],
            Arm::P4 => &["--workers", "4"],
            Arm::P8 => &["--workers", "8"],
            Arm::P8A => &["--workers", "8", "--check", "all"],
        }
    }

    /// The threads a T arm names.
    #[must_use]
    pub const fn threads(self) -> Option<u64> {
        match self {
            Arm::T4 => Some(4),
            Arm::T8 => Some(8),
            _ => None,
        }
    }

    /// The workers a P arm names.
    #[must_use]
    pub const fn workers(self) -> Option<u64> {
        match self {
            Arm::P4 => Some(4),
            Arm::P8 | Arm::P8A => Some(8),
            _ => None,
        }
    }

    /// The check level a P arm's summary names.
    #[must_use]
    pub const fn check(self) -> Option<&'static str> {
        match self {
            Arm::P4 | Arm::P8 => Some("dominance"),
            Arm::P8A => Some("all"),
            _ => None,
        }
    }

    /// C0's mode rule: `distributed` under P4 and P8, `search` under A, T4, T8 and P8A.
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Arm::P4 | Arm::P8 => Mode::Distributed,
            _ => Mode::Search,
        }
    }
}

/// A Williams square for an even `n`: `n` orders of `0..n`, each value once in every position and
/// immediately after every other value exactly once. Row 0 is 0, 1, n−1, 2, n−2, …; row r adds r
/// mod n. An odd `n` has no such square of `n` rows: `None`. Pure.
#[must_use]
pub fn williams(n: usize) -> Option<Vec<Vec<usize>>> {
    if n == 0 || !n.is_multiple_of(2) {
        return None;
    }
    let mut first = vec![0];
    let (mut lo, mut hi) = (0, n);
    while first.len() < n {
        if first.len() % 2 == 1 {
            lo += 1;
            first.push(lo);
        } else {
            hi -= 1;
            first.push(hi);
        }
    }
    Some(
        (0..n)
            .map(|r| first.iter().map(|v| (v + r) % n).collect())
            .collect(),
    )
}

/// The clean session's order: round r (from 0) runs the Williams square's order r mod 6, so 12
/// rounds are the six orders twice over. Pure.
#[must_use]
pub fn schedule(rounds: usize) -> Vec<[Arm; 6]> {
    let square = williams(Arm::ALL.len()).unwrap_or_default();
    (0..rounds)
        .map(|r| {
            let mut order = Arm::ALL;
            if let Some(row) = square.get(r % Arm::ALL.len()) {
                for (slot, i) in order.iter_mut().zip(row) {
                    if let Some(arm) = Arm::ALL.get(*i) {
                        *slot = *arm;
                    }
                }
            }
            order
        })
        .collect()
}

/// One injected fault of the fault session (design §11.7), each on the grid winner's slot 6 of
/// search 1, attempt 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultCase {
    /// `kill@1.6.1:3`: SIGKILL after item 3.
    Kill,
    /// `hang@1.6.1:3`: stop after item 3, no progress, no reply.
    Hang,
    /// `slow@1.6.1:3`: stop after item 3 until `Cancel`, then judge the rest and send the result.
    Slow,
    /// `dup@1.6.1`: the result twice.
    Dup,
}

impl FaultCase {
    /// Every fault, in the order each fault round runs them.
    pub const ALL: [FaultCase; 4] = [
        FaultCase::Kill,
        FaultCase::Hang,
        FaultCase::Slow,
        FaultCase::Dup,
    ];

    /// Its `--fault` spec, which is also its key in `workers faults fired=`.
    #[must_use]
    pub const fn spec(self) -> &'static str {
        match self {
            FaultCase::Kill => "kill@1.6.1:3",
            FaultCase::Hang => "hang@1.6.1:3",
            FaultCase::Slow => "slow@1.6.1:3",
            FaultCase::Dup => "dup@1.6.1",
        }
    }

    /// Its short name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            FaultCase::Kill => "kill",
            FaultCase::Hang => "hang",
            FaultCase::Slow => "slow",
            FaultCase::Dup => "dup",
        }
    }

    /// The counters it must move, each to at least 1.
    #[must_use]
    pub const fn moves(self) -> &'static [&'static str] {
        match self {
            FaultCase::Kill => &["lost", "retries"],
            FaultCase::Hang => &["expired"],
            FaultCase::Slow => &["late_progress", "stale"],
            FaultCase::Dup => &["duplicate"],
        }
    }
}

/// The flags a run adds after `--out`: its arm's, then a fault's.
#[must_use]
pub fn run_flags(arm: Arm, fault: Option<FaultCase>) -> Vec<String> {
    let mut flags: Vec<String> = arm.flags().iter().map(|f| (*f).to_owned()).collect();
    if let Some(f) = fault {
        flags.extend([
            "--lease-ms".to_owned(),
            FAULT_LEASE_MS.to_string(),
            "--fault".to_owned(),
            f.spec().to_owned(),
        ]);
    }
    flags
}

// ---------------------------------------------------------------------------------------------
// Arguments.

/// What the verb was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    /// The plans file, as given.
    pub plans: String,
    /// The output directory, as given.
    pub out: String,
    /// `--lab`, as given.
    pub lab: Option<String>,
    /// `--faults`.
    pub faults: bool,
    /// `--rounds`.
    pub rounds: usize,
}

/// The verb's arguments. Pure.
/// # Errors
/// A usage error: an unknown word, a flag without its value, no `--plans` or `--out`, or `--rounds`
/// outside 1 to 12.
pub fn parse_args(args: &[String]) -> Result<Args, Fail> {
    let mut a = args.to_vec();
    let faults = take_flag(&mut a, "--faults");
    let plans = take_value(&mut a, "--plans")?;
    let out = take_value(&mut a, "--out")?;
    let lab = take_value(&mut a, "--lab")?;
    let rounds = take_value(&mut a, "--rounds")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "aos-workers-bench: unexpected '{extra}' (aos-workers-bench --help)"
        )));
    }
    let plans = plans.ok_or_else(|| Fail::usage("aos-workers-bench: --plans FILE is required"))?;
    let out = out.ok_or_else(|| Fail::usage("aos-workers-bench: --out DIR is required"))?;
    let rounds = match rounds {
        None => REGISTERED_ROUNDS,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=REGISTERED_ROUNDS).contains(n))
            .ok_or_else(|| {
                Fail::usage(format!(
                    "aos-workers-bench: --rounds takes 1 to {REGISTERED_ROUNDS}, not '{v}'"
                ))
            })?,
    };
    Ok(Args {
        plans,
        out,
        lab,
        faults,
        rounds,
    })
}

// ---------------------------------------------------------------------------------------------
// The summary `aos-search` prints.

/// One class of jobs' trip percentiles in one search, as the summary prints them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trip {
    /// The jobs of the class.
    pub n: u64,
    /// Lease to accepted result, p50, p95 and p99 in ms.
    pub trip: Option<[f64; 3]>,
    /// The same less the worker's declared judging time (labelled declared).
    pub over: Option<[f64; 3]>,
}

/// A search's winner line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Winner {
    /// The plan's hex.
    pub plan: String,
    /// The plan's digest.
    pub digest: String,
    /// Its round.
    pub round: u64,
    /// Its index.
    pub index: u64,
    /// Useful under policy v1.
    pub useful: bool,
}

/// A distributed search's `claimed` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Claimed {
    /// Jobs.
    pub jobs: u64,
    /// Rows the accepted claims carry.
    pub rows: u64,
    /// Judgments made by the coordinator (local).
    pub local_judged: u64,
    /// Rows taken on a worker's word (claimed).
    pub claimed_only: u64,
    /// The check level.
    pub check: String,
    /// The barrier's threads.
    pub barrier_threads: u64,
}

/// One search's lines of a summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchSummary {
    /// `grid` or `plans`.
    pub proposer: String,
    /// The proposer's config digest.
    pub config: String,
    /// Proposed.
    pub proposed: Option<u64>,
    /// Distinct.
    pub distinct: Option<u64>,
    /// Admitted (`admitted=` or `admitted_claimed=`).
    pub admitted: Option<u64>,
    /// The admitted count rests on workers' claims (`admitted_claimed=`).
    pub on_claims: bool,
    /// Oversized.
    pub oversized: Option<u64>,
    /// Duplicates.
    pub duplicates: Option<u64>,
    /// Over budget.
    pub over_budget: Option<u64>,
    /// Rounds.
    pub rounds: Option<u64>,
    /// A `counts` line that carries no counts (void, or an outcome without them).
    pub counts_note: Option<String>,
    /// Refusals by law, in the summary's order.
    pub refused: Vec<(String, u64)>,
    /// `unreported=`.
    pub unreported: Option<u64>,
    /// The winner.
    pub winner: Option<Winner>,
    /// `selection provenance=`.
    pub provenance: Option<String>,
    /// Training `ΣT`, `ΣZ`.
    pub train: Option<(u64, u64)>,
    /// Held-out `ΣT`, `ΣZ` and the gain in ppm.
    pub held_out: Option<(u64, u64, i64)>,
    /// `S0`'s `ΣT`, `ΣZ` and gain in ppm.
    pub s0: Option<(u64, u64, i64)>,
    /// The bundle file and its digest.
    pub bundle: Option<(String, String)>,
    /// `no-outcome …`.
    pub no_outcome: Option<String>,
    /// `wall_ms=`: search start to `Admitted` (M4's scope; the record's accept_ms).
    pub wall_ms: Option<f64>,
    /// `stepped=` (A and T).
    pub stepped: Option<u64>,
    /// `stepped_here=` (P).
    pub stepped_here: Option<u64>,
    /// `stepped_claimed=` (P, declared).
    pub stepped_claimed: Option<u64>,
    /// The `claimed` line (P).
    pub claimed: Option<Claimed>,
    /// `last_claim_ms=`.
    pub last_claim_ms: Option<f64>,
    /// `check_ms=`: each barrier call's task count and wall time.
    pub check_ms: Vec<(u64, f64)>,
    /// `trip_ms class=admitted`.
    pub trip_admitted: Option<Trip>,
    /// `trip_ms class=refused`.
    pub trip_refused: Option<Trip>,
    /// `held_out_ms=`.
    pub held_out_ms: Option<f64>,
    /// `verify_ms=`.
    pub verify_ms: Option<f64>,
}

/// The pool's lines of a summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoolSummary {
    /// Workers asked.
    pub n: Option<u64>,
    /// The lease window.
    pub window_ms: Option<u64>,
    /// The faults armed.
    pub faults: Vec<String>,
    /// The ledger's counts, in the summary's order.
    pub counts: Vec<(String, u64)>,
    /// Workers spawned.
    pub spawned: Option<u64>,
    /// Workers reaped.
    pub reaped: Option<u64>,
    /// Workers killed.
    pub killed: Option<u64>,
    /// Spawn to the last Hello.
    pub pool_ms: Option<f64>,
    /// The largest worker peak RSS, MiB.
    pub peak_rss_mb_max: Option<u64>,
    /// The workers' summed peak RSS, MiB (floored).
    pub peak_rss_mb_sum: Option<u64>,
    /// The reaped workers' CPU, as the coordinator's `wait_with_rusage` saw it.
    pub cpu_ms: Option<f64>,
    /// Frames read from workers.
    pub frames_in: Option<u64>,
    /// Their bytes.
    pub bytes_in: Option<u64>,
    /// Frames written to workers.
    pub frames_out: Option<u64>,
    /// Their bytes.
    pub bytes_out: Option<u64>,
    /// The most events queued at once.
    pub queue_max: Option<u64>,
    /// Time inside the core (decode, ledger, fold), ms.
    pub core_ms: Option<f64>,
    /// Each fault that fired, and how often.
    pub fired: Vec<(String, u64)>,
    /// `workers void …`.
    pub void: Option<String>,
    /// Other `workers` lines (foreign workers, stderr tails).
    pub notes: Vec<String>,
}

impl PoolSummary {
    /// A ledger count by name.
    #[must_use]
    pub fn count(&self, key: &str) -> Option<u64> {
        self.counts.iter().find(|(k, _)| k == key).map(|(_, n)| *n)
    }
}

/// A parsed `aos-search` summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// The policy digest.
    pub policy: String,
    /// The build, as printed (spaces as `_`).
    pub build: String,
    /// The toolchain line, as printed.
    pub toolchain: String,
    /// The coordinator's evaluator build.
    pub evaluator_build_ms: Option<f64>,
    /// `threads n=`.
    pub threads: Option<u64>,
    /// The searches, in order.
    pub searches: Vec<SearchSummary>,
    /// The pool, under `--workers`.
    pub pool: Option<PoolSummary>,
    /// `pass`, `fail` or `void`.
    pub result: String,
    /// `refusal=` of a void.
    pub refusal: Option<String>,
    /// `searches=`.
    pub searches_n: Option<u64>,
    /// `winners=`.
    pub winners: Option<u64>,
    /// `aos-search removed …` lines.
    pub removed: u64,
}

impl Summary {
    /// The search of the proposer `name`.
    #[must_use]
    pub fn search(&self, name: &str) -> Option<&SearchSummary> {
        self.searches.iter().find(|s| s.proposer == name)
    }
}

fn pairs(text: &str) -> Vec<(&str, &str)> {
    text.split_whitespace()
        .filter_map(|w| w.split_once('='))
        .collect()
}

fn first<'a>(p: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    p.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn int(p: &[(&str, &str)], key: &str) -> Option<u64> {
    first(p, key)?.parse().ok()
}

fn float(p: &[(&str, &str)], key: &str) -> Option<f64> {
    first(p, key)?.parse().ok()
}

fn need<'a>(p: &[(&'a str, &'a str)], key: &str, line: &str) -> Result<&'a str, String> {
    first(p, key).ok_or_else(|| format!("no {key}= in `{line}`"))
}

fn need_int(p: &[(&str, &str)], key: &str, line: &str) -> Result<u64, String> {
    need(p, key, line)?
        .parse()
        .map_err(|_| format!("{key}= is not a count in `{line}`"))
}

fn need_i64(p: &[(&str, &str)], key: &str, line: &str) -> Result<i64, String> {
    need(p, key, line)?
        .parse()
        .map_err(|_| format!("{key}= is not an integer in `{line}`"))
}

fn sums(p: &[(&str, &str)], line: &str) -> Result<(u64, u64, i64), String> {
    Ok((
        need_int(p, "T", line)?,
        need_int(p, "Z", line)?,
        need_i64(p, "gain_ppm", line)?,
    ))
}

fn parse_trip(t: &str) -> Result<(String, Trip), String> {
    let (head, over) = t
        .split_once(" overhead_ms(declared)")
        .ok_or_else(|| format!("no overhead_ms(declared) in `{t}`"))?;
    let h = pairs(head);
    let o = pairs(over);
    let three = |q: &[(&str, &str)]| -> Option<[f64; 3]> {
        Some([float(q, "p50")?, float(q, "p95")?, float(q, "p99")?])
    };
    Ok((
        need(&h, "class", t)?.to_owned(),
        Trip {
            n: need_int(&h, "n", t)?,
            trip: three(&h),
            over: three(&o),
        },
    ))
}

fn parse_checks(v: &str) -> Result<Vec<(u64, f64)>, String> {
    if v == "none" {
        return Ok(Vec::new());
    }
    v.split(',')
        .map(|part| {
            let (n, ms) = part
                .split_once(':')
                .ok_or_else(|| format!("check_ms part `{part}` is not N:MS"))?;
            Ok((
                n.parse()
                    .map_err(|_| format!("check_ms part `{part}`: N is not a count"))?,
                ms.parse()
                    .map_err(|_| format!("check_ms part `{part}`: MS is not a time"))?,
            ))
        })
        .collect()
}

fn parse_fired(v: &str) -> Result<Vec<(String, u64)>, String> {
    if v == "none" {
        return Ok(Vec::new());
    }
    v.split(',')
        .map(|kv| {
            let (k, n) = kv
                .rsplit_once('=')
                .ok_or_else(|| format!("fired part `{kv}` is not SPEC=N"))?;
            Ok((
                k.to_owned(),
                n.parse()
                    .map_err(|_| format!("fired part `{kv}`: N is not a count"))?,
            ))
        })
        .collect()
}

fn search_line(c: &mut SearchSummary, line: &str) -> Result<(), String> {
    let p = pairs(line);
    let head = line.split_whitespace().next().unwrap_or_default();
    match head {
        "counts" if first(&p, "proposed").is_some() => {
            c.proposed = Some(need_int(&p, "proposed", line)?);
            c.distinct = Some(need_int(&p, "distinct", line)?);
            match (int(&p, "admitted"), int(&p, "admitted_claimed")) {
                (Some(n), None) => c.admitted = Some(n),
                (None, Some(n)) => {
                    c.admitted = Some(n);
                    c.on_claims = true;
                }
                _ => return Err(format!("no single admitted count in `{line}`")),
            }
            c.oversized = int(&p, "oversized");
            c.duplicates = int(&p, "duplicates");
            c.over_budget = int(&p, "over_budget");
            c.rounds = int(&p, "rounds");
        }
        "counts" => c.counts_note = Some(line.to_owned()),
        "refused" | "refused_claimed" => {
            for (k, v) in &p {
                let n = v
                    .parse::<u64>()
                    .map_err(|_| format!("{k}= is not a count in `{line}`"))?;
                if *k == "unreported" {
                    c.unreported = Some(n);
                } else {
                    c.refused.push(((*k).to_owned(), n));
                }
            }
        }
        "winner" => {
            c.winner = Some(Winner {
                plan: need(&p, "plan", line)?.to_owned(),
                digest: need(&p, "digest", line)?.to_owned(),
                round: need_int(&p, "round", line)?,
                index: need_int(&p, "index", line)?,
                useful: match need(&p, "useful", line)? {
                    "true" => true,
                    "false" => false,
                    other => return Err(format!("useful={other} in `{line}`")),
                },
            });
        }
        "selection" => c.provenance = first(&p, "provenance").map(str::to_owned),
        "train" => c.train = Some((need_int(&p, "T", line)?, need_int(&p, "Z", line)?)),
        "held_out" => c.held_out = Some(sums(&p, line)?),
        "s0" => c.s0 = Some(sums(&p, line)?),
        "bundle" => {
            c.bundle = Some((
                need(&p, "file", line)?.to_owned(),
                need(&p, "digest", line)?.to_owned(),
            ));
        }
        "no-outcome" => {
            c.no_outcome = Some(
                line.strip_prefix("no-outcome")
                    .unwrap_or(line)
                    .trim()
                    .to_owned(),
            );
        }
        "claimed" => {
            c.claimed = Some(Claimed {
                jobs: need_int(&p, "jobs", line)?,
                rows: need_int(&p, "rows", line)?,
                local_judged: need_int(&p, "local_judged", line)?,
                claimed_only: need_int(&p, "claimed_only", line)?,
                check: need(&p, "check", line)?.to_owned(),
                barrier_threads: need_int(&p, "barrier_threads", line)?,
            });
        }
        "MEASURED" => {
            let m = line
                .strip_prefix("MEASURED (never scored) ")
                .ok_or_else(|| format!("a MEASURED line without its label: `{line}`"))?;
            if let Some(t) = m.strip_prefix("trip_ms ") {
                let (class, trip) = parse_trip(t)?;
                match class.as_str() {
                    "admitted" => c.trip_admitted = Some(trip),
                    "refused" => c.trip_refused = Some(trip),
                    other => return Err(format!("trip class {other} in `{line}`")),
                }
            } else {
                for (k, v) in &p {
                    match *k {
                        "wall_ms" => c.wall_ms = v.parse().ok(),
                        "stepped" => c.stepped = v.parse().ok(),
                        "stepped_here" => c.stepped_here = v.parse().ok(),
                        "stepped_claimed" => c.stepped_claimed = v.parse().ok(),
                        "last_claim_ms" => c.last_claim_ms = v.parse().ok(),
                        "check_ms" => c.check_ms = parse_checks(v)?,
                        "held_out_ms" => c.held_out_ms = v.parse().ok(),
                        "verify_ms" => c.verify_ms = v.parse().ok(),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn pool_line(pool: &mut PoolSummary, rest: &str) -> Result<(), String> {
    let p = pairs(rest);
    if rest.starts_with("n=") {
        pool.n = int(&p, "n");
        pool.window_ms = int(&p, "window_ms");
        pool.faults = match first(&p, "faults") {
            None | Some("none") => Vec::new(),
            Some(v) => v.split(',').map(str::to_owned).collect(),
        };
    } else if rest.starts_with("counts ") {
        pool.counts = p
            .iter()
            .map(|(k, v)| {
                v.parse::<u64>()
                    .map(|n| ((*k).to_owned(), n))
                    .map_err(|_| format!("workers count {k}={v} is not a count"))
            })
            .collect::<Result<_, String>>()?;
    } else if rest.starts_with("processes ") {
        pool.spawned = int(&p, "spawned");
        pool.reaped = int(&p, "reaped");
        pool.killed = int(&p, "killed");
    } else if rest.starts_with("MEASURED (never scored) ") {
        pool.pool_ms = float(&p, "pool_ms");
        pool.peak_rss_mb_max = int(&p, "peak_rss_mb_max");
        pool.peak_rss_mb_sum = int(&p, "peak_rss_mb_sum");
        pool.cpu_ms = float(&p, "cpu_ms");
        pool.frames_in = int(&p, "frames_in");
        pool.bytes_in = int(&p, "bytes_in");
        pool.frames_out = int(&p, "frames_out");
        pool.bytes_out = int(&p, "bytes_out");
        pool.queue_max = int(&p, "queue_max");
        pool.core_ms = float(&p, "core_ms");
    } else if let Some(v) = rest.strip_prefix("faults fired=") {
        pool.fired = parse_fired(v)?;
    } else if let Some(v) = rest.strip_prefix("void ") {
        pool.void = Some(v.to_owned());
    } else {
        pool.notes.push(rest.to_owned());
    }
    Ok(())
}

/// The summary `aos-search` printed on stdout. Pure.
/// # Errors
/// A line the summary's shape does not allow, or no header or result line.
pub fn parse_summary(text: &str) -> Result<Summary, String> {
    let mut s = Summary::default();
    let mut header = false;
    let mut ended = false;
    let mut cur: Option<SearchSummary> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("  ") {
            let c = cur
                .as_mut()
                .ok_or_else(|| format!("an indented line outside a search: `{line}`"))?;
            search_line(c, rest)?;
            continue;
        }
        if let Some(c) = cur.take() {
            s.searches.push(c);
        }
        let p = pairs(line);
        if line.starts_with("aos-search policy=") {
            s.policy = need(&p, "policy", line)?.to_owned();
            s.build = need(&p, "build", line)?.to_owned();
            s.toolchain = need(&p, "toolchain", line)?.to_owned();
            header = true;
        } else if line.starts_with("MEASURED (never scored) evaluator_build_ms=") {
            s.evaluator_build_ms = float(&p, "evaluator_build_ms");
        } else if line.starts_with("threads ") {
            s.threads = Some(need_int(&p, "n", line)?);
        } else if line.starts_with("search proposer=") {
            cur = Some(SearchSummary {
                proposer: need(&p, "proposer", line)?.to_owned(),
                config: need(&p, "config", line)?.to_owned(),
                ..SearchSummary::default()
            });
        } else if let Some(rest) = line.strip_prefix("workers ") {
            pool_line(s.pool.get_or_insert_default(), rest)?;
        } else if line.starts_with("aos-search result=") {
            s.result = need(&p, "result", line)?.to_owned();
            s.refusal = first(&p, "refusal").map(str::to_owned);
            s.searches_n = int(&p, "searches");
            s.winners = int(&p, "winners");
            ended = true;
        } else if line.starts_with("aos-search removed ") {
            s.removed += 1;
        }
    }
    if let Some(c) = cur.take() {
        s.searches.push(c);
    }
    if !header {
        return Err("no `aos-search policy=` header line".to_owned());
    }
    if !ended {
        return Err("no `aos-search result=` line".to_owned());
    }
    Ok(s)
}

/// The 1-minute load average of `sysctl -n vm.loadavg`'s `{ 1m 5m 15m }`. Pure.
#[must_use]
pub fn parse_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace()
        .find(|w| *w != "{")
        .and_then(|w| w.parse().ok())
}

// ---------------------------------------------------------------------------------------------
// C0: a run's bundles against A's.

/// What C0 found for one search of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parity {
    /// Equal to A's, timings, toolchain line and mode aside, in the arm's mode.
    Equal,
    /// The bundle (or the reference's) is absent or does not decode.
    Unread(String),
    /// Not the arm's mode.
    Mode {
        /// The bundle's mode.
        found: Mode,
        /// The mode rule's.
        want: Mode,
    },
    /// These parts differ.
    Differs(Vec<&'static str>),
}

/// A mode's name in a bundle report.
#[must_use]
pub const fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Search => "search",
        Mode::OnDemand => "on-demand",
        Mode::Distributed => "distributed",
    }
}

impl Parity {
    /// Its text in the record.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Parity::Equal => "equal".to_owned(),
            Parity::Unread(why) => format!("unread({})", word(why)),
            Parity::Mode { found, want } => {
                format!("mode={} want={}", mode_name(*found), mode_name(*want))
            }
            Parity::Differs(parts) => format!("differs({})", parts.join(",")),
        }
    }
}

/// C0 for one search: `run` against the reference A's bundle, with the timings, the toolchain line
/// and the mode aside, and `run`'s mode `want`. Every part is compared and named; then the whole
/// bundle with the three asides set equal, so a part added later cannot be missed. Pure.
#[must_use]
pub fn parity(run: &UncheckedBundle, reference: &UncheckedBundle, want: Mode) -> Parity {
    if run.selection.mode != want {
        return Parity::Mode {
            found: run.selection.mode,
            want,
        };
    }
    let (s, r) = (&run.selection, &reference.selection);
    let parts = [
        ("policy", run.policy == reference.policy),
        ("base", run.base == reference.base),
        ("candidate", run.candidate == reference.candidate),
        ("plan", run.plan == reference.plan),
        ("proposer", run.proposer == reference.proposer),
        ("round", run.round == reference.round),
        ("index", run.index == reference.index),
        (
            "stranded_light",
            run.stranded_light == reference.stranded_light,
        ),
        (
            "lane_fidelity",
            run.lane_fidelity == reference.lane_fidelity,
        ),
        ("interpreter", run.interpreter == reference.interpreter),
        ("lane", run.lane == reference.lane),
        ("oracle", run.oracle == reference.oracle),
        ("corpus", run.corpus == reference.corpus),
        ("schema", run.schema == reference.schema),
        ("profile", run.profile == reference.profile),
        ("build", run.build == reference.build),
        ("rows", run.rows == reference.rows),
        ("stats", s.stats == r.stats),
        ("held_out_looks", s.held_out_looks == r.held_out_looks),
        ("train", s.train == r.train),
        ("held_out", s.held_out == r.held_out),
        ("s0", s.s0 == r.s0),
        ("useful", s.useful == r.useful),
        ("gain_ppm", s.gain_ppm == r.gain_ppm),
        ("s0_gain_ppm", s.s0_gain_ppm == r.s0_gain_ppm),
        ("publication", run.publication == reference.publication),
        ("grade", run.grade == reference.grade),
        (
            "tested_assumptions",
            run.tested_assumptions == reference.tested_assumptions,
        ),
        (
            "plan_assumptions",
            run.plan_assumptions == reference.plan_assumptions,
        ),
    ];
    let differs: Vec<&'static str> = parts
        .iter()
        .filter(|(_, same)| !same)
        .map(|(name, _)| *name)
        .collect();
    let mut whole = run.clone();
    whole.timings.clone_from(&reference.timings);
    whole.toolchain.clone_from(&reference.toolchain);
    whole.selection.mode = reference.selection.mode;
    match (differs.is_empty(), whole == *reference) {
        (true, true) => Parity::Equal,
        (true, false) => Parity::Differs(vec!["(a part not named here)"]),
        (false, _) => Parity::Differs(differs),
    }
}

/// C0 for each search of a run: its bundles against the reference's, in [`SEARCHES`] order. Pure.
#[must_use]
pub fn compare(
    run: &[Result<UncheckedBundle, String>],
    reference: &[Result<UncheckedBundle, String>],
    want: Mode,
) -> Vec<Parity> {
    SEARCHES
        .iter()
        .enumerate()
        .map(|(i, _)| match (run.get(i), reference.get(i)) {
            (Some(Ok(b)), Some(Ok(r))) => parity(b, r, want),
            (Some(Err(e)), _) => Parity::Unread(e.clone()),
            (None, _) => Parity::Unread("absent".to_owned()),
            (_, Some(Err(e))) => Parity::Unread(format!("the reference: {e}")),
            (_, None) => Parity::Unread("the reference: absent".to_owned()),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// A run, and its judgment.

/// Which session a run belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The clean session.
    Clean,
    /// The fault session.
    Fault,
}

/// What the runner measured of one run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measured {
    /// Spawn to exit, ms.
    pub wall_ms: f64,
    /// Killed at the deadline.
    pub deadline: bool,
    /// The exit code.
    pub exit: Option<i32>,
    /// The signal that ended it.
    pub signal: Option<i32>,
    /// User plus system CPU of the lab and every process it reaped, ms.
    pub cpu_ms: Option<f64>,
    /// The largest single process's peak RSS, bytes.
    pub maxrss: Option<u64>,
    /// Why the runner could not spawn or wait.
    pub error: Option<String>,
    /// The lab's stdout: its summary.
    pub stdout: String,
    /// The lab's stderr.
    pub stderr: String,
}

impl Measured {
    /// How it ended, as one word.
    #[must_use]
    pub fn status(&self) -> String {
        match (self.deadline, &self.error, self.exit, self.signal) {
            (true, _, _, _) => "killed:deadline".to_owned(),
            (false, Some(_), _, _) => "error".to_owned(),
            (false, None, Some(c), _) => format!("exit:{c}"),
            (false, None, None, Some(s)) => format!("signal:{s}"),
            (false, None, None, None) => "unknown".to_owned(),
        }
    }
}

/// A gate a run can fail without stopping the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gate {
    /// Gate 3: a clean P run counted an event [`CLEAN_COUNTERS`] names, or armed or fired a fault.
    G3,
    /// Gate 5: a budget.
    G5,
    /// The fault session: the fault did not show.
    Fault,
}

impl Gate {
    const fn name(self) -> &'static str {
        match self {
            Gate::G3 => "gate3",
            Gate::G5 => "gate5",
            Gate::Fault => "fault",
        }
    }
}

/// A run's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It counts.
    Valid,
    /// It does not count, and its round is left out.
    Invalid(Vec<(Gate, String)>),
    /// A gate-2 defect: the session stops.
    Defect(String),
}

/// A run's judgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judgment {
    /// The verdict.
    pub verdict: Verdict,
    /// An A run's gate-1 mismatches (empty: it reproduces the M4 record).
    pub gate1: Option<Vec<String>>,
    /// Useful accepted results: searches ending admitted and useful that pass C0.
    pub uar: u64,
}

/// One run of a session.
#[derive(Clone, Debug)]
pub struct Run {
    /// `cRR.P` (clean) or `fRR.P` (fault; round 0 is its A run).
    pub id: String,
    /// The session.
    pub phase: Phase,
    /// The round, from 1 (0: the fault session's A run).
    pub round: usize,
    /// The position in the round, from 1.
    pub pos: usize,
    /// The arm.
    pub arm: Arm,
    /// The fault, in the fault session.
    pub fault: Option<FaultCase>,
    /// Its out dir, relative to DIR.
    pub out: String,
    /// The 1-minute load average before it.
    pub load1: Result<f64, String>,
    /// What the runner measured.
    pub m: Measured,
    /// Its summary.
    pub summary: Result<Summary, String>,
    /// C0, once the reference exists.
    pub parity: Option<Vec<Parity>>,
    /// The judgment, once it can be made.
    pub judged: Option<Judgment>,
}

impl Run {
    fn label(&self) -> String {
        match self.fault {
            Some(f) => format!("{} {} {}", self.id, self.arm.name(), f.spec()),
            None => format!("{} {}", self.id, self.arm.name()),
        }
    }

    fn valid(&self) -> bool {
        self.judged
            .as_ref()
            .is_some_and(|j| j.verdict == Verdict::Valid)
    }

    fn summary(&self) -> Option<&Summary> {
        self.summary.as_ref().ok()
    }

    fn search(&self, name: &str) -> Option<&SearchSummary> {
        self.summary()?.search(name)
    }

    fn pool(&self) -> Option<&PoolSummary> {
        self.summary()?.pool.as_ref()
    }
}

/// A run's id: `c01.1`, `f00.1`.
#[must_use]
pub fn run_id(phase: Phase, round: usize, pos: usize) -> String {
    let p = match phase {
        Phase::Clean => 'c',
        Phase::Fault => 'f',
    };
    format!("{p}{round:02}.{pos}")
}

fn want(miss: &mut Vec<String>, ok: bool, what: &str, found: impl std::fmt::Display) {
    if !ok {
        miss.push(format!("{what} (found {found})"));
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "none".to_owned(), |v| v.to_string())
}

fn refused_text(r: &[(String, u64)]) -> String {
    if r.is_empty() {
        return "none".to_owned();
    }
    r.iter()
        .map(|(k, n)| format!("{k}:{n}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Gate 1: an A run reproduces the M4 record (§10.9). The mismatches; none when it does. Pure.
#[must_use]
pub fn gate1(s: &Summary) -> Vec<String> {
    let mut miss = Vec::new();
    match s.search("grid") {
        None => miss.push("no grid search".to_owned()),
        Some(g) => {
            want(
                &mut miss,
                g.proposed == Some(m4::GRID_PROPOSED),
                "grid proposed 54",
                opt(g.proposed),
            );
            want(
                &mut miss,
                g.admitted == Some(m4::GRID_ADMITTED) && !g.on_claims,
                "grid admitted 18, judged here",
                format!("{} on_claims={}", opt(g.admitted), g.on_claims),
            );
            let refused: Vec<(String, u64)> = m4::GRID_REFUSED
                .iter()
                .map(|(k, n)| ((*k).to_owned(), *n))
                .collect();
            let mut found = g.refused.clone();
            found.sort();
            want(
                &mut miss,
                found == refused && g.unreported == Some(0),
                "grid refused HotHasWake 9, NoStepSlept 9, WakeNoLaterThanClear 18, unreported 0",
                format!(
                    "{} unreported={}",
                    refused_text(&g.refused),
                    opt(g.unreported)
                ),
            );
            let w = g.winner.as_ref();
            want(
                &mut miss,
                w.is_some_and(|w| {
                    w.plan == m4::WINNER_PLAN
                        && w.round == m4::GRID_ROUND
                        && w.index == m4::GRID_INDEX
                }),
                "grid winner 010100000000000000000000 at round 0 index 6",
                w.map_or_else(
                    || "none".to_owned(),
                    |w| format!("{} at round {} index {}", w.plan, w.round, w.index),
                ),
            );
            want(
                &mut miss,
                g.held_out == Some((m4::HELD_OUT_T, m4::HELD_OUT_Z, m4::GAIN_PPM)),
                "grid held out ΣT 5119 ΣZ 161 gain 234255 ppm",
                g.held_out.map_or_else(
                    || "none".to_owned(),
                    |(t, z, ppm)| format!("ΣT {t} ΣZ {z} gain {ppm}"),
                ),
            );
        }
    }
    match s.search("plans") {
        None => miss.push("no plans search".to_owned()),
        Some(p) => {
            want(
                &mut miss,
                p.config == m4::PLANS_CONFIG,
                "plans config e5463d8e4c08 (the workload)",
                &p.config,
            );
            want(
                &mut miss,
                p.admitted == Some(m4::PLANS_ADMITTED) && !p.on_claims,
                "plans admitted 7, judged here",
                format!("{} on_claims={}", opt(p.admitted), p.on_claims),
            );
            want(
                &mut miss,
                p.winner
                    .as_ref()
                    .is_some_and(|w| w.index == m4::PLANS_INDEX),
                "plans winner at index 7",
                opt(p.winner.as_ref().map(|w| w.index)),
            );
        }
    }
    miss
}

/// The ledger counts a clean P run must leave at zero (gate 3: lost, expired, stale, duplicate and
/// late progress, with foreign Hellos, dropped frames, retries and cancellations; a refused frame
/// voids the run, so it is gate 2's).
pub const CLEAN_COUNTERS: [&str; 9] = [
    "lost",
    "expired",
    "stale",
    "duplicate",
    "late_progress",
    "foreign",
    "dropped",
    "retries",
    "cancelled",
];

/// Gate 3 for a clean P run: what it counted. Empty when it is clean. Pure.
#[must_use]
pub fn gate3(p: &PoolSummary) -> Vec<String> {
    let mut out = Vec::new();
    for k in CLEAN_COUNTERS {
        match p.count(k) {
            Some(0) => {}
            Some(n) => out.push(format!("{k}={n}")),
            None => out.push(format!("{k} not counted")),
        }
    }
    if !p.faults.is_empty() {
        out.push(format!("faults armed {}", p.faults.join(",")));
    }
    if !p.fired.is_empty() {
        out.push(format!("faults fired {}", refused_text(&p.fired)));
    }
    out
}

/// Gate 5's memory figure: the runner's peak RSS plus the workers' summed peaks, which the summary
/// floors to whole MiB, so a MiB less a byte is added to them. Pure.
#[must_use]
pub fn rss_bound(maxrss: u64, workers_sum_mb: Option<u64>) -> u64 {
    maxrss.saturating_add(workers_sum_mb.map_or(0, |mb| {
        mb.saturating_add(1).saturating_mul(MIB).saturating_sub(1)
    }))
}

/// Gate 5 for a run: the budgets it broke. Empty when it kept them. Pure.
#[must_use]
pub fn gate5(m: &Measured, pool: Option<&PoolSummary>) -> Vec<String> {
    let mut out = Vec::new();
    let limit_ms = DEADLINE.as_secs_f64() * 1e3;
    if m.deadline || m.wall_ms > limit_ms {
        out.push(format!(
            "wall_ms={:.1} past the {} s deadline",
            m.wall_ms,
            DEADLINE.as_secs()
        ));
    }
    let sum = pool.map(|p| p.peak_rss_mb_sum);
    match (m.maxrss, sum) {
        (None, _) => out.push("no peak RSS from wait_with_rusage".to_owned()),
        (Some(_), Some(None)) => out.push("no peak_rss_mb_sum in the summary".to_owned()),
        (Some(rss), sum) => {
            let bound = rss_bound(rss, sum.flatten());
            if bound > RSS_BUDGET {
                out.push(format!(
                    "peak RSS bound {:.1} MiB over the {} MiB budget",
                    bound as f64 / MIB as f64,
                    RSS_BUDGET / MIB
                ));
            }
        }
    }
    out
}

/// Whether a fault run's fault showed: it fired exactly once, no other fault fired, and its
/// counters moved. What did not show; empty when it did. Pure.
#[must_use]
pub fn fault_shown(case: FaultCase, p: &PoolSummary) -> Vec<String> {
    let mut out = Vec::new();
    let spec = case.spec();
    match p.fired.iter().find(|(k, _)| k == spec).map(|(_, n)| *n) {
        Some(1) => {}
        Some(n) => out.push(format!("{spec} fired {n} times")),
        None => out.push(format!("{spec} did not fire")),
    }
    let others: Vec<&str> = p
        .fired
        .iter()
        .filter(|(k, _)| k != spec)
        .map(|(k, _)| k.as_str())
        .collect();
    if !others.is_empty() {
        out.push(format!("other faults fired: {}", others.join(",")));
    }
    for c in case.moves() {
        match p.count(c) {
            Some(n) if n >= 1 => {}
            n => out.push(format!("{c}={} (it must move to at least 1)", opt(n))),
        }
    }
    out
}

/// The reason a summary is not the arm it was spawned as, if it is not. Pure.
fn arm_mismatch(arm: Arm, fault: Option<FaultCase>, s: &Summary) -> Option<String> {
    if s.threads != arm.threads() {
        return Some(format!("threads={} for arm {}", opt(s.threads), arm.name()));
    }
    match (arm.workers(), &s.pool) {
        (None, None) => None,
        (None, Some(_)) => Some(format!("a pool for arm {}", arm.name())),
        (Some(_), None) => Some(format!("no pool for arm {}", arm.name())),
        (Some(n), Some(p)) => {
            if p.n != Some(n) {
                return Some(format!("workers n={} for arm {}", opt(p.n), arm.name()));
            }
            if let Some(x) = s
                .searches
                .iter()
                .find(|x| x.claimed.as_ref().map(|c| c.check.as_str()) != arm.check())
            {
                return Some(format!(
                    "search {} check={} for arm {}",
                    x.proposer,
                    opt(x.claimed.as_ref().map(|c| c.check.clone())),
                    arm.name()
                ));
            }
            let want_faults: Vec<String> = fault.iter().map(|f| f.spec().to_owned()).collect();
            if fault.is_some() && (p.faults != want_faults || p.window_ms != Some(FAULT_LEASE_MS)) {
                return Some(format!(
                    "faults={} window_ms={} for {}",
                    p.faults.join(","),
                    opt(p.window_ms),
                    fault.map_or("", FaultCase::spec)
                ));
            }
            None
        }
    }
}

/// A run's judgment, once it can be made: `None` while C0 waits for the reference. `build` is
/// the session's build. Pure.
#[must_use]
pub fn judge(run: &Run, build: Option<&str>) -> Option<Judgment> {
    let defect = |why: String| {
        Some(Judgment {
            verdict: Verdict::Defect(why),
            gate1: None,
            uar: 0,
        })
    };
    if run.m.deadline {
        return Some(Judgment {
            verdict: Verdict::Invalid(vec![(
                Gate::G5,
                format!("killed at the {} s deadline", DEADLINE.as_secs()),
            )]),
            gate1: None,
            uar: 0,
        });
    }
    if let Some(e) = &run.m.error {
        return defect(format!("the runner could not run it: {e}"));
    }
    let s = match &run.summary {
        Ok(s) => s,
        Err(e) => {
            return defect(format!(
                "{}: its summary does not parse: {e}",
                run.m.status()
            ));
        }
    };
    if s.result == "void" {
        return defect(format!(
            "result=void refusal={}",
            s.refusal.as_deref().unwrap_or("none")
        ));
    }
    if let Some(p) = &s.pool
        && (p.reaped.is_none() || p.reaped != p.spawned)
    {
        return defect(format!(
            "reaped={} spawned={}: a worker was not reaped",
            opt(p.reaped),
            opt(p.spawned)
        ));
    }
    if run.m.exit != Some(0)
        || s.result != "pass"
        || s.winners != Some(2)
        || s.searches_n != Some(2)
    {
        return defect(format!(
            "{} result={} searches={} winners={} (an honest run passes with 2 winners)",
            run.m.status(),
            s.result,
            opt(s.searches_n),
            opt(s.winners)
        ));
    }
    if let Some(b) = build
        && s.build != b
    {
        return defect(format!("build {} is not the session's {b}", s.build));
    }
    let names: Vec<&str> = s.searches.iter().map(|x| x.proposer.as_str()).collect();
    if names != SEARCHES {
        return defect(format!("searches {} are not grid,plans", names.join(",")));
    }
    if let Some(why) = arm_mismatch(run.arm, run.fault, s) {
        return defect(format!("not the arm it was spawned as: {why}"));
    }
    let g1 = (run.arm == Arm::A).then(|| gate1(s));
    let parity = run.parity.as_ref()?;
    let bad: Vec<String> = SEARCHES
        .iter()
        .zip(parity)
        .filter(|(_, p)| **p != Parity::Equal)
        .map(|(n, p)| format!("{n} {}", p.text()))
        .collect();
    if parity.len() != SEARCHES.len() {
        return defect(format!("C0: {} searches compared, not 2", parity.len()));
    }
    if !bad.is_empty() {
        return defect(format!("C0: {}", bad.join("; ")));
    }
    let mut invalid = Vec::new();
    match (run.fault, &s.pool) {
        (Some(case), Some(p)) => {
            invalid.extend(fault_shown(case, p).into_iter().map(|w| (Gate::Fault, w)));
        }
        (None, Some(p)) => invalid.extend(gate3(p).into_iter().map(|w| (Gate::G3, w))),
        (Some(_), None) | (None, None) => {}
    }
    invalid.extend(
        gate5(&run.m, s.pool.as_ref())
            .into_iter()
            .map(|w| (Gate::G5, w)),
    );
    let uar = s
        .searches
        .iter()
        .filter(|x| x.winner.as_ref().is_some_and(|w| w.useful) && x.bundle.is_some())
        .count();
    Some(Judgment {
        verdict: if invalid.is_empty() {
            Verdict::Valid
        } else {
            Verdict::Invalid(invalid)
        },
        gate1: g1,
        uar: u64::try_from(uar).unwrap_or(u64::MAX),
    })
}

/// The session's build: the first summary's. Pure.
#[must_use]
pub fn session_build(runs: &[Run]) -> Option<String> {
    runs.iter()
        .find_map(|r| r.summary().map(|s| s.build.clone()))
}

/// C0's reference in `phase`: the first A run that ended by itself with a passing summary that
/// reproduces the M4 record, and whose two bundles decode. Pure.
#[must_use]
pub fn reference(
    runs: &[Run],
    bundles: &[Vec<Result<UncheckedBundle, String>>],
    phase: Phase,
) -> Option<usize> {
    runs.iter().zip(bundles).position(|(r, b)| {
        r.phase == phase
            && r.arm == Arm::A
            && !r.m.deadline
            && r.m.error.is_none()
            && r.m.exit == Some(0)
            && r.summary()
                .is_some_and(|s| s.result == "pass" && gate1(s).is_empty())
            && b.len() == SEARCHES.len()
            && b.iter().all(Result::is_ok)
    })
}

/// Fills C0 for every run of `phase` that waits for it (once the reference exists), then judges
/// every run not yet judged. Pure.
pub fn settle(runs: &mut [Run], bundles: &[Vec<Result<UncheckedBundle, String>>], phase: Phase) {
    if let Some(r) = reference(runs, bundles, phase)
        && let Some(reference) = bundles.get(r)
    {
        for (run, b) in runs.iter_mut().zip(bundles) {
            if run.phase == phase && run.parity.is_none() {
                run.parity = Some(compare(b, reference, run.arm.mode()));
            }
        }
    }
    let build = session_build(runs);
    for run in runs.iter_mut() {
        if run.judged.is_none() {
            run.judged = judge(run, build.as_deref());
        }
    }
}

/// The first defect among `phase`'s runs, as `id: why`. Pure.
#[must_use]
pub fn defect_in(runs: &[Run], phase: Phase) -> Option<String> {
    runs.iter()
        .filter(|r| r.phase == phase)
        .find_map(|r| match &r.judged {
            Some(Judgment {
                verdict: Verdict::Defect(why),
                ..
            }) => Some(format!("{}: {why}", r.label())),
            _ => None,
        })
}

// ---------------------------------------------------------------------------------------------
// The session.

/// What the runner knows of the lab binary (gate 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LabFacts {
    /// A debug build: known only when `--lab` is byte-identical to this runner.
    pub debug: Option<bool>,
    /// Its digest at the end equals its digest at the start; `None` until the end.
    pub unchanged: Option<bool>,
}

/// Gate 0, one clean commit and one debug lab binary: what breaks it. Pure.
#[must_use]
pub fn gate0(build: Option<&str>, lab: LabFacts, done: bool) -> Vec<String> {
    let mut out = Vec::new();
    match build {
        None => out.push("no run printed a build".to_owned()),
        Some(b) if check_build(b).is_err() => out.push(format!("the build {b} names no commit")),
        Some(b) if b.ends_with("-dirty") => {
            out.push(format!("the build {b} ends -dirty: not one clean commit"));
        }
        Some(_) => {}
    }
    match lab.debug {
        Some(true) => {}
        Some(false) => out.push("the lab is not a debug build".to_owned()),
        None => out.push(
            "the lab's build profile is unknown: --lab is not byte-identical to this runner"
                .to_owned(),
        ),
    }
    match lab.unchanged {
        Some(true) => {}
        Some(false) => out.push("the lab binary changed during the session".to_owned()),
        None if done => out.push("the lab binary was not read again at the end".to_owned()),
        None => {}
    }
    out
}

/// Gate 6's valid rounds of `rounds`: 10 of the registered 12, `N − ⌊N/6⌋` of a smoke run's N.
#[must_use]
pub const fn required_valid(rounds: usize) -> usize {
    rounds - rounds / 6
}

/// Where a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every gate held.
    Valid,
    /// These gates did not.
    Void(Vec<String>),
    /// A gate-2 defect stopped it.
    Stopped(String),
}

/// The clean session's gates.
#[derive(Clone, Debug, PartialEq)]
pub struct CleanEval {
    /// Rounds scheduled.
    pub rounds: usize,
    /// The session's build.
    pub build: Option<String>,
    /// Gate 0's failures.
    pub gate0: Vec<String>,
    /// The A runs gate 1 checked.
    pub gate1_checked: usize,
    /// Gate 1's failures, `id: mismatches`.
    pub gate1_failed: Vec<String>,
    /// The defect that stopped the session.
    pub defect: Option<String>,
    /// Runs never compared with A's.
    pub pending: Vec<String>,
    /// The runs judged.
    pub judged: usize,
    /// The P runs judged past C0.
    pub p_checked: usize,
    /// Runs left out by gate 3, `id: why`.
    pub gate3_out: Vec<String>,
    /// Runs left out by gate 5, `id: why`.
    pub gate5_out: Vec<String>,
    /// Rounds with all six runs.
    pub complete: Vec<usize>,
    /// Rounds with all six runs valid.
    pub valid_rounds: Vec<usize>,
    /// A's wall times over the valid rounds.
    pub a_walls: Vec<f64>,
    /// Gate 6's requirement.
    pub required: usize,
    /// The outcome.
    pub outcome: Outcome,
}

fn reasons(v: &[(Gate, String)], gate: Gate) -> Vec<String> {
    v.iter()
        .filter(|(g, _)| *g == gate)
        .map(|(_, w)| w.clone())
        .collect()
}

/// A's wall-time span over the valid rounds: max / min. Pure.
#[must_use]
pub fn span_ratio(walls: &[f64]) -> Option<f64> {
    let (lo, hi) = span(walls)?;
    (lo > 0.0).then(|| hi / lo)
}

/// The clean session's gates 0–6 over its runs so far. Pure.
#[must_use]
pub fn evaluate_clean(runs: &[Run], rounds: usize, lab: LabFacts, done: bool) -> CleanEval {
    let clean: Vec<&Run> = runs.iter().filter(|r| r.phase == Phase::Clean).collect();
    let build = session_build(runs);
    let gate0 = gate0(build.as_deref(), lab, done);
    let mut gate1_checked = 0;
    let mut gate1_failed = Vec::new();
    let mut gate3_out = Vec::new();
    let mut gate5_out = Vec::new();
    let mut pending = Vec::new();
    let mut p_checked = 0;
    for r in &clean {
        match &r.judged {
            None => pending.push(r.id.clone()),
            Some(j) => {
                if let Some(g1) = &j.gate1 {
                    gate1_checked += 1;
                    if !g1.is_empty() {
                        gate1_failed.push(format!("{}: {}", r.id, g1.join("; ")));
                    }
                }
                if r.arm.workers().is_some()
                    && r.parity.is_some()
                    && !matches!(j.verdict, Verdict::Defect(_))
                {
                    p_checked += 1;
                }
                if let Verdict::Invalid(v) = &j.verdict {
                    let g3 = reasons(v, Gate::G3);
                    if !g3.is_empty() {
                        gate3_out.push(format!("{}: {}", r.label(), g3.join(", ")));
                    }
                    let g5 = reasons(v, Gate::G5);
                    if !g5.is_empty() {
                        gate5_out.push(format!("{}: {}", r.label(), g5.join(", ")));
                    }
                }
            }
        }
    }
    let defect = defect_in(runs, Phase::Clean);
    let in_round =
        |n: usize| -> Vec<&Run> { clean.iter().copied().filter(|r| r.round == n).collect() };
    let complete: Vec<usize> = (1..=rounds)
        .filter(|n| in_round(*n).len() == Arm::ALL.len())
        .collect();
    let valid_rounds: Vec<usize> = complete
        .iter()
        .copied()
        .filter(|n| in_round(*n).iter().all(|r| r.valid()))
        .collect();
    let a_walls: Vec<f64> = valid_rounds
        .iter()
        .filter_map(|n| {
            in_round(*n)
                .iter()
                .find(|r| r.arm == Arm::A)
                .map(|r| r.m.wall_ms)
        })
        .collect();
    let required = required_valid(rounds);
    let mut void = Vec::new();
    for g in &gate0 {
        void.push(format!("gate 0: {g}"));
    }
    if gate1_checked == 0 {
        void.push("gate 1: no A run was checked".to_owned());
    }
    for g in &gate1_failed {
        void.push(format!("gate 1: {g}"));
    }
    if !pending.is_empty() {
        void.push(format!(
            "gate 2: {} runs were never compared with A's ({})",
            pending.len(),
            pending.join(",")
        ));
    }
    match span_ratio(&a_walls) {
        None => void.push("gate 4: no valid round".to_owned()),
        Some(x) if x > A_SPAN_MAX => void.push(format!(
            "gate 4: A's wall times span {x:.3}x, over {A_SPAN_MAX}x"
        )),
        Some(_) => {}
    }
    if valid_rounds.len() < required {
        void.push(format!(
            "gate 6: {} of {rounds} rounds valid, under {required}",
            valid_rounds.len()
        ));
    }
    let outcome = match (&defect, void.is_empty()) {
        (Some(d), _) => Outcome::Stopped(d.clone()),
        (None, true) => Outcome::Valid,
        (None, false) => Outcome::Void(void),
    };
    CleanEval {
        rounds,
        build,
        gate0,
        gate1_checked,
        gate1_failed,
        defect,
        pending,
        judged: clean.iter().filter(|r| r.judged.is_some()).count(),
        p_checked,
        gate3_out,
        gate5_out,
        complete,
        valid_rounds,
        a_walls,
        required,
        outcome,
    }
}

/// Which figure a criterion compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Figure {
    /// The runner's wall time.
    Wall,
    /// The runner's `wait_with_rusage` CPU.
    Cpu,
}

impl Figure {
    fn of(self, r: &Run) -> Option<f64> {
        match self {
            Figure::Wall => Some(r.m.wall_ms),
            Figure::Cpu => r.m.cpu_ms,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Figure::Wall => "wall_ms",
            Figure::Cpu => "cpu_ms",
        }
    }
}

/// A criterion, paired within each valid round: `lhs`'s figure below `rhs`'s.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    /// `C1`, `C2`, `C3`.
    pub key: &'static str,
    /// The claim it licenses.
    pub claim: &'static str,
    /// The arm that must be lower.
    pub lhs: Arm,
    /// The arm it is compared with.
    pub rhs: Arm,
    /// The figure.
    pub figure: Figure,
    /// Each valid round's (round, lhs, rhs).
    pub pairs: Vec<(usize, f64, f64)>,
}

impl Criterion {
    /// The valid rounds where it held.
    #[must_use]
    pub fn holds(&self) -> usize {
        self.pairs.iter().filter(|(_, l, r)| l < r).count()
    }

    /// It held in every valid round, and there was one.
    #[must_use]
    pub fn shown(&self) -> bool {
        !self.pairs.is_empty() && self.holds() == self.pairs.len()
    }
}

/// C1–C3 over the clean session's valid rounds. Pure.
#[must_use]
pub fn criteria(runs: &[Run], valid_rounds: &[usize]) -> Vec<Criterion> {
    let one = |key, claim, lhs: Arm, rhs: Arm, figure: Figure| {
        let at = |n: usize, arm: Arm| {
            runs.iter()
                .find(|r| r.phase == Phase::Clean && r.round == n && r.arm == arm)
                .and_then(|r| figure.of(r))
        };
        Criterion {
            key,
            claim,
            lhs,
            rhs,
            figure,
            pairs: valid_rounds
                .iter()
                .filter_map(|n| Some((*n, at(*n, lhs)?, at(*n, rhs)?)))
                .collect(),
        }
    };
    vec![
        one(
            "C1",
            "Process workers reach useful accepted results sooner than serial search on this Mac",
            Arm::P8,
            Arm::A,
            Figure::Wall,
        ),
        one(
            "C2",
            "Threads beat process workers on this Mac",
            Arm::T8,
            Arm::P8,
            Figure::Wall,
        ),
        one(
            "C3",
            "The dominance check saves CPU over full checking",
            Arm::P8,
            Arm::P8A,
            Figure::Cpu,
        ),
    ]
}

/// The held-out reads and evaluator builds a phase's runs disclose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Disclosed {
    /// Evaluator builds: one per coordinator, one per worker spawned.
    pub builds: u64,
    /// Runs without a summary, counted by their arm.
    pub by_arm: u64,
    /// Held-out reads through the gate (`held_out_ms` printed).
    pub gate_reads: u64,
    /// In-search verify re-reads (`verify_ms` printed).
    pub verify_reads: u64,
}

/// What `phase`'s runs disclose (§11.8 "Disclosed held-out reads"), counted from the runs made.
/// Pure.
#[must_use]
pub fn disclosed(runs: &[Run], phase: Phase) -> Disclosed {
    let mut d = Disclosed::default();
    for r in runs.iter().filter(|r| r.phase == phase) {
        match r.summary() {
            Some(s) => {
                d.builds += 1 + s.pool.as_ref().and_then(|p| p.spawned).unwrap_or(0);
                for x in &s.searches {
                    d.gate_reads += u64::from(x.held_out_ms.is_some());
                    d.verify_reads += u64::from(x.verify_ms.is_some());
                }
            }
            None => {
                d.builds += 1 + r.arm.workers().unwrap_or(0);
                d.by_arm += 1;
            }
        }
    }
    d
}

/// The fault session's rounds for a session of `rounds`.
#[must_use]
pub fn fault_rounds(rounds: usize) -> usize {
    rounds.min(FAULT_ROUNDS)
}

/// One fault's runs.
#[derive(Clone, Debug, PartialEq)]
pub struct FaultRow {
    /// The fault.
    pub case: FaultCase,
    /// Each run's id and why it is invalid (empty: valid).
    pub runs: Vec<(String, Vec<String>)>,
    /// Each judged run's wall time less the clean median P8 wall time.
    pub added_wall: Vec<f64>,
    /// Each judged run's CPU less the clean median P8 CPU.
    pub added_cpu: Vec<f64>,
    /// Each judged run's leases beyond its jobs.
    pub extra_leases: Vec<i64>,
    /// Shown: every one of its runs valid.
    pub shown: bool,
}

/// How the fault session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultOutcome {
    /// Every fault showed in every run, and every result equalled A's.
    Shown,
    /// These faults did not show in every run.
    NotShown(Vec<String>),
    /// It could not be judged.
    Void(Vec<String>),
    /// A defect stopped it.
    Stopped(String),
}

/// The fault session's evaluation.
#[derive(Clone, Debug, PartialEq)]
pub struct FaultEval {
    /// Its rounds.
    pub rounds: usize,
    /// Its A run's gate-1 result: the id, and its mismatches.
    pub a: Option<(String, Vec<String>)>,
    /// One row per fault.
    pub rows: Vec<FaultRow>,
    /// The clean session's median P8 wall time over its valid rounds.
    pub clean_p8_wall: Option<f64>,
    /// The same of CPU.
    pub clean_p8_cpu: Option<f64>,
    /// The outcome.
    pub outcome: FaultOutcome,
}

fn clean_figures(runs: &[Run], valid_rounds: &[usize], arm: Arm, f: Figure) -> Vec<f64> {
    runs.iter()
        .filter(|r| r.phase == Phase::Clean && r.arm == arm && valid_rounds.contains(&r.round))
        .filter_map(|r| f.of(r))
        .collect()
}

fn jobs_of(r: &Run) -> Option<u64> {
    r.summary()?
        .searches
        .iter()
        .map(|x| x.claimed.as_ref().map(|c| c.jobs))
        .sum()
}

/// The fault session's criteria over its runs. Pure.
#[must_use]
pub fn evaluate_faults(runs: &[Run], rounds: usize, clean: &CleanEval) -> FaultEval {
    let fault: Vec<&Run> = runs.iter().filter(|r| r.phase == Phase::Fault).collect();
    let clean_p8_wall = median(&clean_figures(
        runs,
        &clean.valid_rounds,
        Arm::P8,
        Figure::Wall,
    ));
    let clean_p8_cpu = median(&clean_figures(
        runs,
        &clean.valid_rounds,
        Arm::P8,
        Figure::Cpu,
    ));
    let a_run = fault.iter().find(|r| r.arm == Arm::A && r.fault.is_none());
    let a = a_run.and_then(|r| {
        r.judged
            .as_ref()
            .and_then(|j| j.gate1.clone())
            .map(|g| (r.id.clone(), g))
    });
    let mut void = Vec::new();
    match (a_run, &a) {
        (None, _) => void.push("the fault session has no A run".to_owned()),
        (Some(r), None) => void.push(format!(
            "its A run {} was not judged against the M4 record",
            r.id
        )),
        (Some(_), Some((id, g))) if !g.is_empty() => {
            void.push(format!(
                "its A run {id} does not reproduce the M4 record: {}",
                g.join("; ")
            ));
        }
        (Some(r), Some(_)) if !r.valid() => {
            void.push(format!("its A run {} is not valid", r.id));
        }
        _ => {}
    }
    let pending: Vec<&str> = fault
        .iter()
        .filter(|r| r.judged.is_none())
        .map(|r| r.id.as_str())
        .collect();
    if !pending.is_empty() {
        void.push(format!(
            "runs never compared with its A run: {}",
            pending.join(",")
        ));
    }
    let rows: Vec<FaultRow> = FaultCase::ALL
        .iter()
        .map(|&case| {
            let of: Vec<&&Run> = fault.iter().filter(|r| r.fault == Some(case)).collect();
            let judged: Vec<&&Run> = of.iter().copied().filter(|r| r.judged.is_some()).collect();
            let runs_of: Vec<(String, Vec<String>)> = of
                .iter()
                .map(|r| {
                    let why = match &r.judged {
                        None => vec!["not judged".to_owned()],
                        Some(j) => match &j.verdict {
                            Verdict::Valid => Vec::new(),
                            Verdict::Invalid(v) => {
                                v.iter().map(|(g, w)| format!("{} {w}", g.name())).collect()
                            }
                            Verdict::Defect(d) => vec![format!("DEFECT {d}")],
                        },
                    };
                    (r.id.clone(), why)
                })
                .collect();
            let less = |f: Figure, base: Option<f64>| -> Vec<f64> {
                judged
                    .iter()
                    .filter_map(|r| Some(f.of(r)? - base?))
                    .collect()
            };
            let extra_leases = judged
                .iter()
                .filter_map(|r| {
                    let leases = i64::try_from(r.pool()?.count("leases")?).ok()?;
                    let jobs = i64::try_from(jobs_of(r)?).ok()?;
                    Some(leases - jobs)
                })
                .collect();
            let shown = of.len() == rounds && runs_of.iter().all(|(_, why)| why.is_empty());
            FaultRow {
                case,
                added_wall: less(Figure::Wall, clean_p8_wall),
                added_cpu: less(Figure::Cpu, clean_p8_cpu),
                extra_leases,
                runs: runs_of,
                shown,
            }
        })
        .collect();
    let outcome = if let Some(d) = defect_in(runs, Phase::Fault) {
        FaultOutcome::Stopped(d)
    } else if !void.is_empty() {
        FaultOutcome::Void(void)
    } else {
        let not: Vec<String> = rows
            .iter()
            .filter(|r| !r.shown)
            .map(|r| r.case.spec().to_owned())
            .collect();
        if not.is_empty() {
            FaultOutcome::Shown
        } else {
            FaultOutcome::NotShown(not)
        }
    };
    FaultEval {
        rounds,
        a,
        rows,
        clean_p8_wall,
        clean_p8_cpu,
        outcome,
    }
}

// ---------------------------------------------------------------------------------------------
// Statistics.

/// The median: the middle value, or the mean of the two middle values. Pure.
#[must_use]
pub fn median(xs: &[f64]) -> Option<f64> {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => v.get(n / 2).copied(),
        _ => Some((v.get(n / 2 - 1)? + v.get(n / 2)?) / 2.0),
    }
}

/// The smallest and largest value. Pure.
#[must_use]
pub fn span(xs: &[f64]) -> Option<(f64, f64)> {
    let lo = xs.iter().copied().min_by(f64::total_cmp)?;
    let hi = xs.iter().copied().max_by(f64::total_cmp)?;
    Some((lo, hi))
}

/// `median=… range=lo..hi n=…` at `prec` decimals, or `none`. Pure.
#[must_use]
pub fn stat(xs: &[f64], prec: usize) -> String {
    match (median(xs), span(xs)) {
        (Some(m), Some((lo, hi))) => format!(
            "median={m:.prec$} range={lo:.prec$}..{hi:.prec$} n={}",
            xs.len()
        ),
        _ => "none".to_owned(),
    }
}

fn f1(v: Option<f64>) -> String {
    v.map_or_else(|| "none".to_owned(), |v| format!("{v:.1}"))
}

fn three(v: Option<[f64; 3]>) -> String {
    v.map_or_else(
        || "none".to_owned(),
        |[a, b, c]| format!("{a:.1}/{b:.1}/{c:.1}"),
    )
}

// ---------------------------------------------------------------------------------------------
// The record.

/// Whether and how the fault session runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultPlan {
    /// No `--faults`.
    NotAsked,
    /// Asked; the clean session is still running.
    Pending,
    /// Asked and not run, for this reason.
    Skipped(String),
    /// Asked and run, with a note.
    Ran(Option<String>),
}

/// What the record says of the session before its runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Rounds.
    pub rounds: usize,
    /// The fault session.
    pub faults: FaultPlan,
    /// The lab's path.
    pub lab: String,
    /// The lab's digest (the workers' executable identity's domain), short.
    pub lab_digest: String,
    /// `--lab` is byte-identical to this runner.
    pub runner_is_lab: Option<bool>,
    /// This runner's build profile.
    pub profile: String,
    /// Gate 0's lab facts.
    pub lab_facts: LabFacts,
    /// The plans file's path.
    pub plans: String,
    /// This host's parallelism.
    pub parallelism: usize,
}

impl Header {
    /// Not the registered session.
    #[must_use]
    pub const fn smoke(&self) -> bool {
        self.rounds != REGISTERED_ROUNDS
    }
}

/// Where the runner is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Runs remain.
    Running,
    /// Every run is made.
    Done,
    /// A signal cut the session short.
    Interrupted(i32),
}

fn yn(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn search_segment(x: &SearchSummary) -> String {
    let mut o = format!("{} config={}", x.proposer, x.config);
    let mut put = |k: &str, v: String| {
        let _ = write!(o, " {k}={v}");
    };
    if let Some(n) = x.proposed {
        put("proposed", n.to_string());
    }
    if let Some(n) = x.distinct {
        put("distinct", n.to_string());
    }
    if let Some(n) = x.admitted {
        put(
            if x.on_claims {
                "admitted_claimed"
            } else {
                "admitted"
            },
            n.to_string(),
        );
    }
    for (k, v) in [
        ("oversized", x.oversized),
        ("duplicates", x.duplicates),
        ("over_budget", x.over_budget),
        ("rounds", x.rounds),
    ] {
        if let Some(v) = v {
            put(k, v.to_string());
        }
    }
    if let Some(note) = &x.counts_note {
        put("counts", word(note));
    }
    put("refused", refused_text(&x.refused));
    put("unreported", opt(x.unreported));
    match &x.winner {
        Some(w) => put(
            "winner",
            format!(
                "{}@{}.{}/{}/useful={}",
                w.plan, w.round, w.index, w.digest, w.useful
            ),
        ),
        None => put("winner", "none".to_owned()),
    }
    if let Some(p) = &x.provenance {
        put("provenance", p.clone());
    }
    if let Some((t, z)) = x.train {
        put("train", format!("T{t}/Z{z}"));
    }
    if let Some((t, z, g)) = x.held_out {
        put("held_out", format!("T{t}/Z{z}/gain_ppm{g}"));
    }
    if let Some((t, z, g)) = x.s0 {
        put("s0", format!("T{t}/Z{z}/gain_ppm{g}"));
    }
    if let Some((f, d)) = &x.bundle {
        put("bundle", format!("{f}/{d}"));
    }
    if let Some(n) = &x.no_outcome {
        put("no_outcome", word(n));
    }
    put("accept_ms", f1(x.wall_ms));
    for (k, v) in [
        ("stepped", x.stepped),
        ("stepped_here", x.stepped_here),
        ("stepped_claimed(declared)", x.stepped_claimed),
    ] {
        if let Some(v) = v {
            put(k, v.to_string());
        }
    }
    if let Some(c) = &x.claimed {
        put("jobs", c.jobs.to_string());
        put("rows", c.rows.to_string());
        put("local_judged", c.local_judged.to_string());
        put("claimed_only", c.claimed_only.to_string());
        put("check", c.check.clone());
        put("barrier_threads", c.barrier_threads.to_string());
    }
    if x.last_claim_ms.is_some() {
        put("last_claim_ms", f1(x.last_claim_ms));
    }
    if !x.check_ms.is_empty() {
        put(
            "check_ms",
            x.check_ms
                .iter()
                .map(|(n, ms)| format!("{n}:{ms:.1}"))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    for (class, t) in [("admitted", &x.trip_admitted), ("refused", &x.trip_refused)] {
        if let Some(t) = t {
            put(
                &format!("trip_{class}"),
                format!(
                    "n{}/p50-95-99:{}/overhead(declared):{}",
                    t.n,
                    three(t.trip),
                    three(t.over)
                ),
            );
        }
    }
    put("held_out_ms", f1(x.held_out_ms));
    put("verify_ms", f1(x.verify_ms));
    o
}

fn pool_segment(p: &PoolSummary) -> String {
    let mut o = format!(
        "pool n={} window_ms={} faults={}",
        opt(p.n),
        opt(p.window_ms),
        if p.faults.is_empty() {
            "none".to_owned()
        } else {
            p.faults.join(",")
        }
    );
    for (k, n) in &p.counts {
        let _ = write!(o, " {k}={n}");
    }
    let _ = write!(
        o,
        " spawned={} reaped={} killed={} pool_ms={} peak_rss_mb_max={} peak_rss_mb_sum={} \
         cpu_ms(workers)={} frames_in={} bytes_in={} frames_out={} bytes_out={} queue_max={} \
         core_ms={} fired={}",
        opt(p.spawned),
        opt(p.reaped),
        opt(p.killed),
        f1(p.pool_ms),
        opt(p.peak_rss_mb_max),
        opt(p.peak_rss_mb_sum),
        f1(p.cpu_ms),
        opt(p.frames_in),
        opt(p.bytes_in),
        opt(p.frames_out),
        opt(p.bytes_out),
        opt(p.queue_max),
        f1(p.core_ms),
        refused_text(&p.fired)
    );
    if let Some(v) = &p.void {
        let _ = write!(o, " void={}", word(v));
    }
    for n in &p.notes {
        let _ = write!(o, " note={}", word(n));
    }
    o
}

/// A run's line: every raw figure. Pure.
#[must_use]
pub fn run_line(r: &Run) -> String {
    let mut o = format!(
        "run {} round={} pos={} arm={} fault={} out={} MEASURED (never scored) load1={} \
         wall_ms={:.1} cpu_ms={} maxrss_bytes={} status={}",
        r.id,
        r.round,
        r.pos,
        r.arm.name(),
        r.fault.map_or("none", FaultCase::spec),
        word(&r.out),
        match &r.load1 {
            Ok(l) => format!("{l:.2}"),
            Err(e) => format!("unread({})", word(e)),
        },
        r.m.wall_ms,
        f1(r.m.cpu_ms),
        opt(r.m.maxrss),
        r.m.status()
    );
    if let Some(e) = &r.m.error {
        let _ = write!(o, " error={}", word(e));
    }
    match &r.summary {
        Err(e) => {
            let _ = write!(o, " | summary unread: {}", word(e));
            if let Some(last) = r.m.stderr.lines().next_back() {
                let _ = write!(o, " | stderr_last={}", word(last));
            }
        }
        Ok(s) => {
            let _ = write!(
                o,
                " | result={} searches={} winners={} refusal={} removed={} policy={} build={} \
                 toolchain={} evaluator_build_ms={} threads={}",
                s.result,
                opt(s.searches_n),
                opt(s.winners),
                s.refusal.as_deref().unwrap_or("none"),
                s.removed,
                s.policy,
                s.build,
                s.toolchain,
                f1(s.evaluator_build_ms),
                opt(s.threads)
            );
            for x in &s.searches {
                let _ = write!(o, " | {}", search_segment(x));
            }
            if let Some(p) = &s.pool {
                let _ = write!(o, " | {}", pool_segment(p));
            }
        }
    }
    o
}

/// A run's verdict line. Pure.
#[must_use]
pub fn verdict_line(r: &Run) -> String {
    let c0 = r.parity.as_ref().map_or_else(
        || "pending".to_owned(),
        |p| {
            SEARCHES
                .iter()
                .zip(p)
                .map(|(n, p)| format!("{n}:{}", p.text()))
                .collect::<Vec<_>>()
                .join(",")
        },
    );
    match &r.judged {
        None => format!("verdict {} pending (no A reference yet) c0={c0}", r.label()),
        Some(j) => {
            let g1 = match &j.gate1 {
                None => String::new(),
                Some(g) if g.is_empty() => " gate1=pass".to_owned(),
                Some(g) => format!(" gate1=FAIL({})", word(&g.join("; "))),
            };
            match &j.verdict {
                Verdict::Valid => {
                    format!("verdict {} valid c0={c0} uar={}{g1}", r.label(), j.uar)
                }
                Verdict::Invalid(v) => format!(
                    "verdict {} invalid ({}) c0={c0} uar={}{g1}",
                    r.label(),
                    v.iter()
                        .map(|(g, w)| format!("{} {w}", g.name()))
                        .collect::<Vec<_>>()
                        .join("; "),
                    j.uar
                ),
                Verdict::Defect(d) => format!("verdict {} DEFECT gate 2: {d}", r.label()),
            }
        }
    }
}

fn pass_fail(ok: bool) -> &'static str {
    if ok { "pass" } else { "FAIL" }
}

fn gates(o: &mut String, ev: &CleanEval, runs: &[Run]) {
    let _ = writeln!(o, "gates (the clean session)");
    let _ = writeln!(
        o,
        "  gate 0 one clean commit and one debug lab binary: {}{}",
        pass_fail(ev.gate0.is_empty()),
        if ev.gate0.is_empty() {
            format!(" build={}", ev.build.as_deref().unwrap_or("none"))
        } else {
            format!(" ({})", ev.gate0.join("; "))
        }
    );
    let _ = writeln!(
        o,
        "  gate 1 A reproduces the M4 record (grid 54 proposed, 18 admitted, HotHasWake 9, \
         NoStepSlept 9, WakeNoLaterThanClear 18, winner 010100000000000000000000 at round 0 \
         index 6, held out ΣT 5119 ΣZ 161, gain 234255 ppm; plans config e5463d8e4c08, 7 \
         admitted, winner at index 7): {} ({} A runs checked{})",
        pass_fail(ev.gate1_checked > 0 && ev.gate1_failed.is_empty()),
        ev.gate1_checked,
        if ev.gate1_failed.is_empty() {
            String::new()
        } else {
            format!("; {}", ev.gate1_failed.join(" | "))
        }
    );
    let _ = writeln!(
        o,
        "  gate 2 parity (C0) and process accounting: {} ({} runs judged, {} never compared{})",
        match (&ev.defect, ev.pending.is_empty()) {
            (Some(_), _) => "DEFECT",
            (None, true) => "pass",
            (None, false) => "FAIL",
        },
        ev.judged,
        ev.pending.len(),
        ev.defect
            .as_ref()
            .map_or_else(String::new, |d| format!("; {d}; the session stopped"))
    );
    let p_runs = runs
        .iter()
        .filter(|r| r.phase == Phase::Clean && r.arm.workers().is_some())
        .count();
    let _ = writeln!(
        o,
        "  gate 3 a clean P run counts no lost, expired, stale, duplicate, late progress, \
         foreign, dropped, retry or cancel: {} ({} P runs, {} past C0, {} left out{})",
        pass_fail(ev.gate3_out.is_empty()),
        p_runs,
        ev.p_checked,
        ev.gate3_out.len(),
        if ev.gate3_out.is_empty() {
            String::new()
        } else {
            format!(": {}; their rounds are left out", ev.gate3_out.join(" | "))
        }
    );
    let _ = writeln!(
        o,
        "  gate 4 A's wall times across the valid rounds span at most {A_SPAN_MAX}x: {} ({})",
        pass_fail(span_ratio(&ev.a_walls).is_some_and(|x| x <= A_SPAN_MAX)),
        match (span_ratio(&ev.a_walls), span(&ev.a_walls)) {
            (Some(x), Some((lo, hi))) => format!(
                "span={x:.3}x min_ms={lo:.1} max_ms={hi:.1} over {} valid rounds",
                ev.a_walls.len()
            ),
            _ => "no valid round".to_owned(),
        }
    );
    let walls: Vec<f64> = runs
        .iter()
        .filter(|r| r.phase == Phase::Clean)
        .map(|r| r.m.wall_ms)
        .collect();
    let bounds: Vec<f64> = runs
        .iter()
        .filter(|r| r.phase == Phase::Clean)
        .filter_map(|r| {
            let sum = r.pool().and_then(|p| p.peak_rss_mb_sum);
            Some(rss_bound(r.m.maxrss?, sum) as f64 / MIB as f64)
        })
        .collect();
    let _ = writeln!(
        o,
        "  gate 5 budgets (a run ends within {} s; the runner's peak RSS plus the workers' \
         peak_rss_mb_sum at most {} MiB): {} (largest wall_ms={} largest bound_mib={}; {} left \
         out{})",
        DEADLINE.as_secs(),
        RSS_BUDGET / MIB,
        pass_fail(ev.gate5_out.is_empty()),
        f1(span(&walls).map(|(_, hi)| hi)),
        f1(span(&bounds).map(|(_, hi)| hi)),
        ev.gate5_out.len(),
        if ev.gate5_out.is_empty() {
            String::new()
        } else {
            format!(": {}; their rounds are left out", ev.gate5_out.join(" | "))
        }
    );
    let _ = writeln!(
        o,
        "  gate 6 at least {} of {} rounds valid: {} (valid={} [{}] complete={} of {})",
        ev.required,
        ev.rounds,
        pass_fail(ev.valid_rounds.len() >= ev.required),
        ev.valid_rounds.len(),
        ev.valid_rounds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
        ev.complete.len(),
        ev.rounds
    );
}

fn arm_runs<'a>(runs: &'a [Run], valid: &[usize], arm: Arm) -> Vec<&'a Run> {
    runs.iter()
        .filter(|r| r.phase == Phase::Clean && r.arm == arm && valid.contains(&r.round))
        .collect()
}

fn figures(rs: &[&Run], f: impl Fn(&Run) -> Option<f64>) -> Vec<f64> {
    rs.iter().filter_map(|r| f(r)).collect()
}

fn uar_of(r: &Run) -> Option<f64> {
    r.judged.as_ref().map(|j| j.uar as f64)
}

fn reported(o: &mut String, runs: &[Run], ev: &CleanEval) {
    let valid = &ev.valid_rounds;
    let _ = writeln!(
        o,
        "reported MEASURED (never scored), over the {} valid rounds; not criteria",
        valid.len()
    );
    for arm in Arm::ALL {
        let rs = arm_runs(runs, valid, arm);
        let _ = writeln!(
            o,
            "  arm={} wall_ms {} | uar_per_s {} | uar_per_cpu_s {} | cpu_ms {} | maxrss_mib {} | \
             uar {} | load1 {}",
            arm.name(),
            stat(&figures(&rs, |r| Some(r.m.wall_ms)), 1),
            stat(&figures(&rs, |r| Some(uar_of(r)? / (r.m.wall_ms / 1e3))), 4),
            stat(&figures(&rs, |r| Some(uar_of(r)? / (r.m.cpu_ms? / 1e3))), 4),
            stat(&figures(&rs, |r| r.m.cpu_ms), 1),
            stat(&figures(&rs, |r| Some(r.m.maxrss? as f64 / MIB as f64)), 1),
            stat(&figures(&rs, uar_of), 0),
            stat(&figures(&rs, |r| r.load1.as_ref().ok().copied()), 2),
        );
    }
    type SearchFigure = fn(&SearchSummary) -> Option<f64>;
    let per_search: [(&str, SearchFigure); 10] = [
        ("accept_ms", |x| x.wall_ms),
        ("last_claim_ms", |x| x.last_claim_ms),
        ("check_ms(total)", |x| {
            (!x.check_ms.is_empty()).then(|| x.check_ms.iter().map(|(_, ms)| ms).sum())
        }),
        ("held_out_ms", |x| x.held_out_ms),
        ("verify_ms", |x| x.verify_ms),
        ("stepped", |x| x.stepped.map(|n| n as f64)),
        ("stepped_here", |x| x.stepped_here.map(|n| n as f64)),
        ("stepped_claimed(declared)", |x| {
            x.stepped_claimed.map(|n| n as f64)
        }),
        ("local_judged", |x| {
            x.claimed.as_ref().map(|c| c.local_judged as f64)
        }),
        ("claimed_only", |x| {
            x.claimed.as_ref().map(|c| c.claimed_only as f64)
        }),
    ];
    for search in SEARCHES {
        for arm in Arm::ALL {
            let rs = arm_runs(runs, valid, arm);
            let mut line = format!("  search={search} arm={}", arm.name());
            if search == "grid" && arm.workers().is_some() {
                let _ = write!(
                    line,
                    " pool_ms(spawn_to_last_hello) {}",
                    stat(&figures(&rs, |r| r.pool()?.pool_ms), 1)
                );
            }
            for (name, f) in &per_search {
                let xs = figures(&rs, |r| f(r.search(search)?));
                if !xs.is_empty() {
                    let _ = write!(line, " | {name} {}", stat(&xs, 1));
                }
            }
            let _ = writeln!(o, "{line}");
        }
    }
    for search in SEARCHES {
        for arm in Arm::ALL.into_iter().filter(|a| a.workers().is_some()) {
            let rs = arm_runs(runs, valid, arm);
            for class in ["admitted", "refused"] {
                let trip = |r: &Run| -> Option<Trip> {
                    let x = r.search(search)?;
                    if class == "admitted" {
                        x.trip_admitted.clone()
                    } else {
                        x.trip_refused.clone()
                    }
                };
                let pick = |i: usize, over: bool| {
                    stat(
                        &figures(&rs, |r| {
                            let t = trip(r)?;
                            let v = if over { t.over } else { t.trip };
                            v.and_then(|v| v.get(i).copied())
                        }),
                        1,
                    )
                };
                let _ = writeln!(
                    o,
                    "  trip search={search} arm={} class={class} jobs {} | trip_ms p50 {} | p95 {} \
                     | p99 {} | overhead_ms(declared) p50 {} | p95 {} | p99 {} (each the median \
                     over runs of that run's percentile)",
                    arm.name(),
                    stat(&figures(&rs, |r| Some(trip(r)?.n as f64)), 0),
                    pick(0, false),
                    pick(1, false),
                    pick(2, false),
                    pick(0, true),
                    pick(1, true),
                    pick(2, true),
                );
            }
        }
    }
    for arm in Arm::ALL.into_iter().filter(|a| a.workers().is_some()) {
        let rs = arm_runs(runs, valid, arm);
        let counted = |f: fn(&PoolSummary) -> Option<u64>| {
            stat(&figures(&rs, |r| f(r.pool()?).map(|n| n as f64)), 0)
        };
        let retries: u64 = rs.iter().filter_map(|r| r.pool()?.count("retries")).sum();
        let _ = writeln!(
            o,
            "  wire arm={} frames_in {} | bytes_in {} | frames_out {} | bytes_out {} | queue_max \
             {} | core_ms(decode,ledger,fold) {} | leases {} | retries_total={retries} | \
             workers_cpu_ms(coordinator's rusage, not added to CPU) {} | peak_rss_mb_sum {}",
            arm.name(),
            counted(|p| p.frames_in),
            counted(|p| p.bytes_in),
            counted(|p| p.frames_out),
            counted(|p| p.bytes_out),
            counted(|p| p.queue_max),
            stat(&figures(&rs, |r| r.pool()?.core_ms), 1),
            counted(|p| p.count("leases")),
            stat(&figures(&rs, |r| r.pool()?.cpu_ms), 1),
            counted(|p| p.peak_rss_mb_sum),
        );
    }
    let at = Arm::ALL
        .into_iter()
        .filter(|a| a.workers().is_none())
        .flat_map(|a| arm_runs(runs, valid, a))
        .filter_map(|r| r.search("grid")?.stepped)
        .collect::<Vec<_>>();
    let _ = writeln!(
        o,
        "  stepped grid under A, T4 and T8: {} of {} runs equal the M4 record's {} (P runs print \
         stepped_here and stepped_claimed instead)",
        at.iter().filter(|n| **n == m4::GRID_STEPPED).count(),
        at.len(),
        m4::GRID_STEPPED
    );
    let wall_median = |a: Arm| median(&figures(&arm_runs(runs, valid, a), |r| Some(r.m.wall_ms)));
    let paired = |a: Arm, b: Arm| -> Vec<f64> {
        valid
            .iter()
            .filter_map(|n| {
                let x = arm_runs(runs, &[*n], a).first()?.m.wall_ms;
                let y = arm_runs(runs, &[*n], b).first()?.m.wall_ms;
                (y > 0.0).then(|| x / y)
            })
            .collect()
    };
    for (a, b) in [(Arm::P4, Arm::P8), (Arm::T4, Arm::T8)] {
        let _ = writeln!(
            o,
            "  ratio wall {}/{}: of medians {} | paired per round {}",
            a.name(),
            b.name(),
            match (wall_median(a), wall_median(b)) {
                (Some(x), Some(y)) if y > 0.0 => format!("{:.3}", x / y),
                _ => "none".to_owned(),
            },
            stat(&paired(a, b), 3)
        );
    }
    let verify_ratio: Vec<(usize, f64)> = valid
        .iter()
        .filter_map(|n| {
            let x = arm_runs(runs, &[*n], Arm::P8A)
                .first()?
                .search("grid")?
                .verify_ms?;
            let y = arm_runs(runs, &[*n], Arm::A)
                .first()?
                .search("grid")?
                .verify_ms?;
            (y > 0.0).then_some((*n, x / y))
        })
        .collect();
    let flagged: Vec<String> = verify_ratio
        .iter()
        .filter(|(_, x)| *x > VERIFY_RATIO_MAX)
        .map(|(n, x)| format!("round {n} {x:.3}x"))
        .collect();
    let _ = writeln!(
        o,
        "  verify grid P8A/A per valid round: {} | flag: {}",
        stat(&verify_ratio.iter().map(|(_, x)| *x).collect::<Vec<_>>(), 3),
        if flagged.is_empty() {
            format!("none above {VERIFY_RATIO_MAX}x")
        } else {
            format!(
                "DEFECT P8A's grid verify_ms above {VERIFY_RATIO_MAX}x A's in {}",
                flagged.join(", ")
            )
        }
    );
    let _ = writeln!(
        o,
        "  operations: process creation less the declared evaluator build is not exposed by the \
         summary (it prints neither a worker's spawn-to-Hello time nor its declared build); the \
         pool's spawn-to-last-Hello (pool_ms), the coordinator's evaluator_build_ms (in each run \
         line) and the per-job dispatch overhead (overhead_ms, declared) are reported as separate \
         figures"
    );
    let d = disclosed(runs, Phase::Clean);
    let made = runs.iter().filter(|r| r.phase == Phase::Clean).count() as u64;
    let _ = writeln!(
        o,
        "  disclosed held-out reads (clean session, counted from the {made} runs made): \
         gate_reads={} verify_rereads={} evaluator_builds={} (each measures the shipped baseline \
         on the held-out items; {} runs without a summary counted by their arm) | registered for \
         {} rounds: gate_reads={} verify_rereads={} evaluator_builds={}",
        d.gate_reads,
        d.verify_reads,
        d.builds,
        d.by_arm,
        ev.rounds,
        2 * 6 * ev.rounds,
        2 * 6 * ev.rounds,
        BUILDS_PER_ROUND * ev.rounds as u64
    );
}

fn fault_section(o: &mut String, h: &Header, runs: &[Run], f: &FaultEval) {
    let _ = writeln!(
        o,
        "fault session (after the clean session, not timed against it): one A run, then {} rounds \
         of P8 at --lease-ms {FAULT_LEASE_MS}, one fault per run{}",
        f.rounds,
        match &h.faults {
            FaultPlan::Ran(Some(note)) => format!(" ({note})"),
            _ => String::new(),
        }
    );
    let _ = writeln!(
        o,
        "  its A run: {}",
        match &f.a {
            None => "none judged".to_owned(),
            Some((id, g)) if g.is_empty() => format!("{id} reproduces the M4 record (gate 1)"),
            Some((id, g)) => format!("{id} does NOT reproduce the M4 record: {}", g.join("; ")),
        }
    );
    for row in &f.rows {
        let valid = row.runs.iter().filter(|(_, w)| w.is_empty()).count();
        let _ = writeln!(
            o,
            "  fault {} (fires once; moves {}): {} ({valid} of {} runs valid, {} scheduled){}",
            row.case.spec(),
            row.case
                .moves()
                .iter()
                .map(|c| format!("{c}>=1"))
                .collect::<Vec<_>>()
                .join(" and "),
            if row.shown { "shown" } else { "NOT shown" },
            row.runs.len(),
            f.rounds,
            row.runs
                .iter()
                .filter(|(_, w)| !w.is_empty())
                .map(|(id, w)| format!("; {id}: {}", w.join(", ")))
                .collect::<String>()
        );
        let _ = writeln!(
            o,
            "    MEASURED (never scored) added_wall_ms (the run's wall time less the clean \
             session's median valid P8 wall time {}; the faulted job's own lease-to-accept time is \
             not in the summary) {} | added_cpu_ms (less the clean median P8 CPU {}; a difference \
             across sessions, not the faulted attempt's wasted work) {} | leases beyond the jobs {}",
            f1(f.clean_p8_wall),
            stat(&row.added_wall, 1),
            f1(f.clean_p8_cpu),
            stat(&row.added_cpu, 1),
            stat(
                &row.extra_leases
                    .iter()
                    .map(|n| *n as f64)
                    .collect::<Vec<_>>(),
                0
            )
        );
    }
    let d = disclosed(runs, Phase::Fault);
    let made = runs.iter().filter(|r| r.phase == Phase::Fault).count();
    let reg_runs = 1 + FaultCase::ALL.len() * f.rounds;
    let _ = writeln!(
        o,
        "  disclosed (fault session, counted from the {made} runs made): this session builds {} \
         evaluators ({} runs without a summary counted by their arm), gate_reads={} \
         verify_rereads={} | scheduled for {} rounds: {} evaluators, {} gate reads, {} verify \
         re-reads (the registered fault session's 3 rounds: 109, 26, 26)",
        d.builds,
        d.by_arm,
        d.gate_reads,
        d.verify_reads,
        f.rounds,
        1 + 9 * (reg_runs as u64 - 1),
        2 * reg_runs,
        2 * reg_runs
    );
    let _ = writeln!(
        o,
        "fault session result={}",
        match &f.outcome {
            FaultOutcome::Shown => {
                "shown (every result equals its A run's; every fault fired once with its counter \
                 moved in every run)"
                    .to_owned()
            }
            FaultOutcome::NotShown(v) => format!("not-shown ({})", v.join(",")),
            FaultOutcome::Void(v) => format!("void ({})", v.join("; ")),
            FaultOutcome::Stopped(d) => format!("stopped (DEFECT gate 2: {d})"),
        }
    );
}

/// The record of `runs` and its exit status. Pure.
#[must_use]
pub fn render(h: &Header, runs: &[Run], state: State) -> (String, i32) {
    let done = state == State::Done;
    let ev = evaluate_clean(runs, h.rounds, h.lab_facts, done);
    let faults = matches!(h.faults, FaultPlan::Ran(_))
        .then(|| evaluate_faults(runs, fault_rounds(h.rounds), &ev));
    let mut o = String::new();
    let _ = writeln!(
        o,
        "aos-workers-bench record v1: design §11.8, pre-registered; every figure MEASURED (never \
         scored)"
    );
    let _ = writeln!(
        o,
        "session kind={} rounds={} registered_rounds={REGISTERED_ROUNDS} faults={}{}",
        if h.smoke() { "smoke" } else { "registered" },
        h.rounds,
        match &h.faults {
            FaultPlan::NotAsked => "no".to_owned(),
            FaultPlan::Pending => "pending".to_owned(),
            FaultPlan::Skipped(why) => format!("skipped({})", word(why)),
            FaultPlan::Ran(_) => "ran".to_owned(),
        },
        if h.smoke() {
            " (SMOKE: NOT the registered session, which runs 12 rounds; no claim is made from it)"
        } else {
            ""
        }
    );
    let _ = writeln!(
        o,
        "lab path={} digest={} runner_is_lab={} runner_profile={} debug={} unchanged={}",
        word(&h.lab),
        h.lab_digest,
        yn(h.runner_is_lab),
        word(&h.profile),
        yn(h.lab_facts.debug),
        yn(h.lab_facts.unchanged)
    );
    let _ = writeln!(o, "plans path={}", word(&h.plans));
    let _ = writeln!(o, "host available_parallelism={}", h.parallelism);
    let _ = writeln!(
        o,
        "protocol: each run is `LAB aos-search --grid --plans PLANS --out DIR/runs/<run>` plus its \
         arm's flags, after {} s idle and a `sysctl -n vm.loadavg` read; deadline {} s; budget {} \
         MiB",
        IDLE.as_secs(),
        DEADLINE.as_secs(),
        RSS_BUDGET / MIB
    );
    let _ = writeln!(
        o,
        "arms {}",
        Arm::ALL
            .iter()
            .map(|a| format!(
                "{}=[{}]",
                a.name(),
                if a.flags().is_empty() {
                    "none".to_owned()
                } else {
                    a.flags().join(" ")
                }
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    for (n, order) in schedule(h.rounds).iter().enumerate() {
        let _ = writeln!(
            o,
            "order round={} {}",
            n + 1,
            order.iter().map(|a| a.name()).collect::<Vec<_>>().join(" ")
        );
    }
    let _ = writeln!(
        o,
        "runs (one line per run with every raw figure, then its verdict)"
    );
    for r in runs {
        let _ = writeln!(o, "{}", run_line(r));
        let _ = writeln!(o, "{}", verdict_line(r));
    }
    for n in 1..=h.rounds {
        let rs: Vec<&Run> = runs
            .iter()
            .filter(|r| r.phase == Phase::Clean && r.round == n)
            .collect();
        if rs.is_empty() {
            continue;
        }
        let _ = writeln!(
            o,
            "round {n} complete={} valid={} load1(paired, not gated) {}",
            if ev.complete.contains(&n) {
                "yes"
            } else {
                "no"
            },
            if ev.valid_rounds.contains(&n) {
                "yes"
            } else {
                "no"
            },
            rs.iter()
                .map(|r| format!(
                    "{}={}",
                    r.arm.name(),
                    r.load1
                        .as_ref()
                        .map_or_else(|_| "unread".to_owned(), |l| format!("{l:.2}"))
                ))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    gates(&mut o, &ev, runs);
    let _ = writeln!(
        o,
        "session clean result={}",
        match &ev.outcome {
            Outcome::Valid => "valid".to_owned(),
            Outcome::Void(v) => format!("void ({})", v.join("; ")),
            Outcome::Stopped(d) =>
                format!("stopped (DEFECT gate 2: {d}; report and fix it before another session)"),
        }
    );
    let _ = writeln!(
        o,
        "criteria (paired within each valid round; a claim only if it holds in every valid round; \
         medians are reported, never tested)"
    );
    let compared = runs
        .iter()
        .filter(|r| r.phase == Phase::Clean && r.parity.is_some())
        .count();
    let _ = writeln!(
        o,
        "  C0 every search's winner, round, index, stats, held-out sums, gain and bundle parts \
         (timings, toolchain line and mode aside) equal A's; mode distributed under P4 and P8, \
         search under A, T4, T8 and P8A: {} ({compared} runs compared{})",
        if ev.defect.as_deref().is_some_and(|d| d.contains("C0:")) {
            "NOT held"
        } else if compared > 0 && ev.pending.is_empty() {
            "held"
        } else {
            "not shown"
        },
        ev.defect
            .as_ref()
            .map_or_else(String::new, |d| format!("; {d}"))
    );
    for c in criteria(runs, &ev.valid_rounds) {
        let l: Vec<f64> = c.pairs.iter().map(|(_, l, _)| *l).collect();
        let r: Vec<f64> = c.pairs.iter().map(|(_, _, r)| *r).collect();
        let _ = writeln!(
            o,
            "  {} {} {} < {} {}: {} (holds in {} of {} valid rounds) | {} {} | {} {}",
            c.key,
            c.lhs.name(),
            c.figure.name(),
            c.rhs.name(),
            c.figure.name(),
            if c.shown() { "shown" } else { "not shown" },
            c.holds(),
            c.pairs.len(),
            c.lhs.name(),
            stat(&l, 1),
            c.rhs.name(),
            stat(&r, 1)
        );
        let _ = writeln!(
            o,
            "    claim \"{}\": {}",
            c.claim,
            match (c.shown(), &ev.outcome, h.smoke(), done) {
                (_, _, _, false) => "not made (the session has not ended)",
                (_, Outcome::Stopped(_), _, _) => "not made (the session stopped on a defect)",
                (_, Outcome::Void(_), _, _) => "not made (the session is void)",
                (_, _, true, _) => "not made (a smoke run is not the registered session)",
                (false, _, _, _) => "not made (not shown)",
                (true, Outcome::Valid, false, true) => "made",
            }
        );
    }
    reported(&mut o, runs, &ev);
    if let Some(f) = &faults {
        fault_section(&mut o, h, runs, f);
    } else if let FaultPlan::Skipped(why) = &h.faults {
        let _ = writeln!(o, "fault session: not run ({why})");
    }
    let faults_ok = match (&h.faults, &faults) {
        (FaultPlan::NotAsked, _) => true,
        (_, Some(f)) => f.outcome == FaultOutcome::Shown,
        (_, None) => false,
    };
    let (word_, code) = match state {
        State::Running => ("in-progress", 1),
        State::Interrupted(sig) => ("interrupted", 128 + sig),
        State::Done => match (&ev.outcome, faults_ok) {
            (Outcome::Stopped(_), _) => ("stopped", 1),
            (Outcome::Void(_), _) => ("void", 1),
            (Outcome::Valid, false) => ("valid-faults-not-shown", 1),
            (Outcome::Valid, true) => ("valid", 0),
        },
    };
    let _ = writeln!(
        o,
        "aos-workers-bench result={word_} kind={} exit={code}",
        if h.smoke() { "smoke" } else { "registered" }
    );
    (o, code)
}

// ---------------------------------------------------------------------------------------------
// Effects: the lab runs, the load average, the files.

fn drain<R: std::io::Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut r) = r {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = r.read_to_end(&mut bytes);
            let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
        });
    }
    rx
}

/// One run of the lab: spawn, poll for the exit every [`POLL`], kill at the deadline.
/// # Errors
/// An interrupt arrived (the run is killed and reaped first).
fn measure(lab: &Path, plans: &Path, out: &Path, flags: &[String]) -> Result<Measured, Stopped> {
    let mut cmd = aterm_uds::spawnfd::command(lab);
    cmd.args(["aos-search", "--grid", "--plans"])
        .arg(plans)
        .arg("--out")
        .arg(out)
        .args(flags)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(Measured {
                error: Some(format!("cannot spawn {}: {e}", lab.display())),
                ..Measured::default()
            });
        }
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = started + DEADLINE;
    let mut at_deadline = false;
    let mut stopped = None;
    let mut killed: Option<Instant> = None;
    let waited = loop {
        match child.try_wait_with_rusage() {
            Ok(Some(u)) => break Ok(u),
            Ok(None) => {}
            Err(e) => break Err(format!("wait4: {e}")),
        }
        let now = Instant::now();
        match killed {
            None => {
                at_deadline = now >= deadline;
                stopped = stop::check().err();
                if at_deadline || stopped.is_some() {
                    let _ = child.kill();
                    killed = Some(now);
                }
            }
            Some(t) if now.duration_since(t) > Duration::from_secs(10) => {
                break Err("the run did not end within 10 s of SIGKILL".to_owned());
            }
            Some(_) => {}
        }
        std::thread::sleep(POLL);
    };
    let wall_ms = started.elapsed().as_secs_f64() * 1e3;
    let take =
        |rx: mpsc::Receiver<String>| rx.recv_timeout(Duration::from_secs(10)).unwrap_or_default();
    let (stdout, stderr) = (take(stdout), take(stderr));
    if let Some(s) = stopped {
        return Err(s);
    }
    let mut m = Measured {
        wall_ms,
        deadline: at_deadline,
        stdout,
        stderr,
        ..Measured::default()
    };
    match waited {
        Ok(u) => {
            use std::os::unix::process::ExitStatusExt as _;
            m.exit = u.status.code();
            m.signal = u.status.signal();
            m.cpu_ms = Some(u.cpu.as_secs_f64() * 1e3);
            m.maxrss = Some(u.maxrss);
        }
        Err(e) => m.error = Some(e),
    }
    Ok(m)
}

/// The 1-minute load average, from `sysctl -n vm.loadavg` spawned through `spawnfd`.
pub(crate) fn load_average() -> Result<f64, String> {
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/sysctl");
    cmd.args(["-n", "vm.loadavg"]);
    let out = aterm_uds::deadline::output_within(&mut cmd, Duration::from_secs(10))
        .map_err(|e| format!("sysctl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!("sysctl exited {}", out.status));
    }
    parse_loadavg(&text).ok_or_else(|| format!("sysctl printed `{}`", text.trim()))
}

fn read_bundle(path: &Path) -> Result<UncheckedBundle, String> {
    if !path.exists() {
        return Err("absent".to_owned());
    }
    let bytes = read_bounded(&path.display().to_string(), MAX_BUNDLE_BYTES)?;
    UncheckedBundle::decode(&bytes).map_err(|r| format!("does not decode: {r}"))
}

/// The regular file `given` names, made absolute and physical.
fn regular_file(given: &str, flag: &str) -> Result<PathBuf, Fail> {
    let path = std::fs::canonicalize(given)
        .map_err(|e| Fail::usage(format!("aos-workers-bench: {flag} {given}: {e}")))?;
    if !path.is_file() {
        return Err(Fail::usage(format!(
            "aos-workers-bench: {flag} {given}: not a regular file"
        )));
    }
    Ok(path)
}

struct Bench {
    lab: PathBuf,
    plans: PathBuf,
    out: PathBuf,
    header: Header,
    runs: Vec<Run>,
    bundles: Vec<Vec<Result<UncheckedBundle, String>>>,
}

enum Halt {
    Interrupted(Stopped),
    Evidence(Fail),
}

impl From<Stopped> for Halt {
    fn from(stopped: Stopped) -> Self {
        Halt::Interrupted(stopped)
    }
}

impl From<Fail> for Halt {
    fn from(failed: Fail) -> Self {
        Halt::Evidence(failed)
    }
}

fn write_evidence(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    std::fs::write(path, bytes).map_err(|e| {
        Fail::die(format!(
            "aos-workers-bench: cannot write {}: {e}",
            path.display()
        ))
    })
}

impl Bench {
    fn write(&self, state: State) -> Result<(String, i32), Fail> {
        let (text, code) = render(&self.header, &self.runs, state);
        write_evidence(&self.out.join("record.txt"), text.as_bytes())?;
        Ok((text, code))
    }

    fn one(
        &mut self,
        phase: Phase,
        round: usize,
        pos: usize,
        arm: Arm,
        fault: Option<FaultCase>,
    ) -> Result<(), Halt> {
        stop::sleep(IDLE)?;
        let load1 = load_average();
        let id = run_id(phase, round, pos);
        let name = format!("{id}-{}", fault.map_or(arm.name(), FaultCase::name));
        let runs_dir = self.out.join("runs");
        let dir = runs_dir.join(&name);
        let m = measure(&self.lab, &self.plans, &dir, &run_flags(arm, fault))?;
        write_evidence(
            &runs_dir.join(format!("{name}.stdout")),
            m.stdout.as_bytes(),
        )?;
        write_evidence(
            &runs_dir.join(format!("{name}.stderr")),
            m.stderr.as_bytes(),
        )?;
        let summary = parse_summary(&m.stdout);
        let bundles = SEARCHES
            .iter()
            .map(|s| read_bundle(&dir.join(format!("{s}.bundle"))))
            .collect();
        self.runs.push(Run {
            id,
            phase,
            round,
            pos,
            arm,
            fault,
            out: format!("runs/{name}"),
            load1,
            m,
            summary,
            parity: None,
            judged: None,
        });
        self.bundles.push(bundles);
        settle(&mut self.runs, &self.bundles, phase);
        self.write(State::Running)?;
        if let Some(r) = self.runs.last() {
            eprintln!(
                "aos-workers-bench: {} wall_ms={:.1} cpu_ms={} {}",
                r.label(),
                r.m.wall_ms,
                f1(r.m.cpu_ms),
                match r.judged.as_ref().map(|j| &j.verdict) {
                    None => "pending".to_owned(),
                    Some(Verdict::Valid) => "valid".to_owned(),
                    Some(Verdict::Invalid(_)) => "invalid".to_owned(),
                    Some(Verdict::Defect(d)) => format!("DEFECT {d}"),
                }
            );
        }
        Ok(())
    }

    fn clean_session(&mut self) -> Result<(), Halt> {
        for (n, order) in schedule(self.header.rounds).iter().enumerate() {
            for (p, arm) in order.iter().enumerate() {
                self.one(Phase::Clean, n + 1, p + 1, *arm, None)?;
                if defect_in(&self.runs, Phase::Clean).is_some() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn fault_session(&mut self) -> Result<(), Halt> {
        self.one(Phase::Fault, 0, 1, Arm::A, None)?;
        for n in 1..=fault_rounds(self.header.rounds) {
            for (p, case) in FaultCase::ALL.iter().enumerate() {
                if defect_in(&self.runs, Phase::Fault).is_some() {
                    return Ok(());
                }
                self.one(Phase::Fault, n, p + 1, Arm::P8, Some(*case))?;
            }
        }
        Ok(())
    }

    /// Whether the fault session runs after the clean one, and why not.
    fn fault_plan(&self) -> FaultPlan {
        let ev = evaluate_clean(&self.runs, self.header.rounds, self.header.lab_facts, false);
        match (&ev.outcome, self.header.smoke()) {
            (Outcome::Stopped(_), _) => {
                FaultPlan::Skipped("the clean session stopped on a defect".to_owned())
            }
            (Outcome::Void(_), false) => FaultPlan::Skipped(
                "the clean session is void, and the fault session follows a valid one".to_owned(),
            ),
            (Outcome::Void(_), true) => FaultPlan::Ran(Some(
                "smoke: run although the clean session is void".to_owned(),
            )),
            (Outcome::Valid, _) => FaultPlan::Ran(None),
        }
    }
}

/// `aos-workers-bench`: the §11.8 session into `--out`.
pub fn cmd_aos_workers_bench(args: &[String]) -> Res {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }
    let args = parse_args(args)?;
    let out = crate::trail_gallery::resolve_out(
        &from_caller(&args.out),
        &crate::trail_gallery::checkouts(&paths().repo),
    )
    .map_err(|e| Fail::usage(format!("aos-workers-bench: --out {e}")))?;
    if out.exists() {
        let empty = std::fs::read_dir(&out)
            .map_err(|e| Fail::usage(format!("aos-workers-bench: --out {}: {e}", out.display())))?
            .next()
            .is_none();
        if !empty {
            return Err(Fail::usage(format!(
                "aos-workers-bench: --out {} is not empty: a record never mixes sessions",
                out.display()
            )));
        }
    }
    let plans = regular_file(&from_caller(&args.plans), "--plans")?;
    let me = std::env::current_exe().map_err(|e| {
        Fail::die(format!(
            "aos-workers-bench: cannot name this executable: {e}"
        ))
    })?;
    let lab = match &args.lab {
        Some(p) => regular_file(&from_caller(p), "--lab")?,
        None => me.clone(),
    };
    let lab_digest = digest_file(&lab, EXECUTABLE_DOMAIN)
        .map_err(|e| Fail::usage(format!("aos-workers-bench: --lab {e}")))?;
    let runner_is_lab = digest_file(&me, EXECUTABLE_DOMAIN)
        .ok()
        .map(|d| d == lab_digest);
    std::fs::create_dir_all(out.join("runs")).map_err(|e| {
        Fail::die(format!(
            "aos-workers-bench: cannot make {}: {e}",
            out.join("runs").display()
        ))
    })?;
    stop::install();
    let header = Header {
        rounds: args.rounds,
        faults: if args.faults {
            FaultPlan::Pending
        } else {
            FaultPlan::NotAsked
        },
        lab: lab.display().to_string(),
        lab_digest: lab_digest.short(),
        runner_is_lab,
        profile: env!("ATERM_LAB_PROFILE").to_owned(),
        lab_facts: LabFacts {
            debug: runner_is_lab.and_then(|same| same.then_some(cfg!(debug_assertions))),
            unchanged: None,
        },
        plans: plans.display().to_string(),
        parallelism: available(),
    };
    if header.smoke() {
        eprintln!(
            "aos-workers-bench: SMOKE (--rounds {}): NOT the registered session ({REGISTERED_ROUNDS} \
             rounds)",
            args.rounds
        );
    }
    let mut bench = Bench {
        lab: lab.clone(),
        plans,
        out: out.clone(),
        header,
        runs: Vec::new(),
        bundles: Vec::new(),
    };
    let ran = bench.clean_session().and_then(|()| {
        if args.faults {
            bench.header.faults = bench.fault_plan();
            if matches!(bench.header.faults, FaultPlan::Ran(_)) {
                return bench.fault_session();
            }
        }
        Ok(())
    });
    bench.header.lab_facts.unchanged =
        Some(digest_file(&lab, EXECUTABLE_DOMAIN).is_ok_and(|d| d == lab_digest));
    match ran {
        Err(Halt::Interrupted(stopped)) => {
            let (text, _) = bench.write(State::Interrupted(stopped.0))?;
            print!("{text}");
            Err(Fail::from(stopped))
        }
        Err(Halt::Evidence(failed)) => Err(failed),
        Ok(()) => {
            let (text, code) = bench.write(State::Done)?;
            print!("{text}");
            println!(
                "aos-workers-bench: wrote {}",
                out.join("record.txt").display()
            );
            Ok(code)
        }
    }
}

#[cfg(test)]
#[path = "aos_workers_bench_tests.rs"]
mod tests;
