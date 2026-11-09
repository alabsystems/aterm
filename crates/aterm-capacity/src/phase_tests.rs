// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A build's phases (CAP3): the step a held claim takes once its tree runs what its build made,
//! and the turns of the claims behind it, by the ledger's own looks ([`crate::admit::decide`]) on
//! an injected machine. No test reads the live one.

use std::path::PathBuf;

use super::{Phases, RunNeed, Step, Steps, built, phased, run_key, run_need};
use crate::admit::{Other, decide};
use crate::book::{Key, Records};
use crate::floor::GIB;
use crate::reading::TreeUse;
use crate::tree_meter::Member;
use crate::{Fixed, Ledger, Need, Polled, Reading, Record, Short, Waiter, reserved};

/// A test's stand-in for a held claim: what it was admitted with, its run phase's memory, and
/// what it claims now.
struct StandIn {
    admitted: u64,
    run: Option<u64>,
    now: u64,
}

impl StandIn {
    fn new(admitted: u64, run: u64) -> StandIn {
        StandIn {
            admitted,
            run: Some(run),
            now: admitted,
        }
    }
}

impl Steps for StandIn {
    fn admitted(&self) -> u64 {
        self.admitted
    }

    fn run_mem(&self) -> Option<u64> {
        self.run
    }

    fn step_down(&mut self) -> std::io::Result<()> {
        self.now = self
            .run
            .expect("a step down is asked only of a claim that plans one");
        Ok(())
    }

    fn step_back(&mut self) -> std::io::Result<()> {
        self.now = self.admitted;
        Ok(())
    }
}

/// What each pid of a scripted tree runs: a driver, a compiler, a build script (by Cargo's two
/// names for one), an example, and two programs the build made.
fn image(pid: u32) -> Option<PathBuf> {
    let path = match pid {
        10 => "/w/bin/targo",
        11 => "/w/toolchain/bin/trustc",
        12 => "/w/target/debug/build/big-0123456789abcdef/build-script-build",
        13 => "/w/target/debug/build/big-0123456789abcdef/build_script_build-0123456789abcdef",
        14 => "/w/target/debug/examples/demo-0123456789abcdef",
        20 => "/w/target/debug/deps/big-0123456789abcdef",
        21 => "/w/target/debug/deps/fleet-fedcba9876543210",
        _ => return None,
    };
    Some(PathBuf::from(path))
}

/// One read of a scripted tree where `pids` hold `footprint` together, all of it the last one's,
/// none of them past what it holds now.
fn tree(pids: &[u32], footprint: u64) -> Vec<Member> {
    let last = pids.len().saturating_sub(1);
    pids.iter()
        .enumerate()
        .map(|(i, pid)| {
            let held = if i == last { footprint } else { 0 };
            Member {
                pid: *pid,
                start: 1,
                footprint: held,
                peak: held,
            }
        })
        .collect()
}

/// The phase is read from what the tree runs, never from the build's words: a driver, a
/// compiler, a build script and an example are its compile; a program its build made, a test
/// binary of any target, is its run.
#[test]
fn a_tree_is_in_its_run_phase_only_while_it_runs_a_program_its_build_made() {
    for pid in [10, 11, 12, 13, 14] {
        assert!(
            !built(&image(pid).expect("scripted")),
            "pid {pid} is the compile's"
        );
    }
    for pid in [20, 21] {
        assert!(
            built(&image(pid).expect("scripted")),
            "pid {pid} is the run's"
        );
    }
    let mut claim = StandIn::new(20 * GIB, 4 * GIB);
    let mut phases = Phases::new();
    for members in [&[10, 11][..], &[10, 12, 13], &[10, 14], &[10, 99]] {
        assert_eq!(
            phases.read(&mut claim, &tree(members, GIB), &image),
            None,
            "{members:?} compiles"
        );
    }
    assert_eq!(phases.run_peak(), None, "no run began");
    assert_eq!(
        claim.now,
        20 * GIB,
        "a compile claims what it was admitted with"
    );
}

/// (c) A PHASE NEVER CLAIMS LESS THAN ITS TREE HOLDS: a run phase whose tree already holds more
/// than its kept figure at its first read does not step; one that steps steps back up to what it
/// was admitted with once its tree holds more than the step, once; and a claim that plans no step
/// keeps what it was admitted with.
#[test]
fn a_phase_never_claims_less_than_its_tree_holds() {
    let mut heavy = StandIn::new(20 * GIB, 4 * GIB);
    let mut phases = Phases::new();
    assert_eq!(
        phases.read(&mut heavy, &tree(&[10, 20], 6 * GIB), &image),
        None,
        "a run already past its step does not step"
    );
    assert_eq!(heavy.now, 20 * GIB);
    assert_eq!(
        phases.read(&mut heavy, &tree(&[10, 20], 3 * GIB), &image),
        None
    );
    assert_eq!(heavy.now, 20 * GIB, "its phase began once, unstepped");
    assert_eq!(phases.run_peak(), Some(6 * GIB));

    let mut claim = StandIn::new(20 * GIB, 4 * GIB);
    let mut phases = Phases::new();
    assert_eq!(
        phases.read(&mut claim, &tree(&[10, 11], 9 * GIB), &image),
        None,
        "compiling"
    );
    assert_eq!(
        phases.read(&mut claim, &tree(&[10, 20], 2 * GIB), &image),
        Some(Step::Down(4 * GIB))
    );
    assert_eq!(claim.now, 4 * GIB);
    assert_eq!(
        phases.read(&mut claim, &tree(&[10, 21], 4 * GIB), &image),
        None
    );
    assert_eq!(claim.now, 4 * GIB, "a tree that holds the step stays in it");
    assert_eq!(
        phases.read(&mut claim, &tree(&[10, 21], 5 * GIB), &image),
        Some(Step::Up(20 * GIB)),
        "a run that outgrows its step claims what it was admitted with again"
    );
    assert_eq!(claim.now, 20 * GIB);
    assert_eq!(
        phases.read(&mut claim, &tree(&[10, 21], 7 * GIB), &image),
        None
    );
    assert_eq!(phases.read(&mut claim, &tree(&[10, 21], GIB), &image), None);
    assert_eq!(
        claim.now,
        20 * GIB,
        "it steps back once, and never down again"
    );
    assert_eq!(
        phases.run_peak(),
        Some(7 * GIB),
        "the run's peak, not the compile's"
    );

    let mut unplanned = StandIn {
        run: None,
        ..StandIn::new(20 * GIB, 0)
    };
    let mut phases = Phases::new();
    assert_eq!(phases.read(&mut unplanned, &tree(&[20], GIB), &image), None);
    assert_eq!(unplanned.now, 20 * GIB, "no run phase kept, no step");
    assert_eq!(phases.run_peak(), Some(GIB), "its first run learns one");
}

/// A scratch ledger, removed when the test ends.
fn scratch(label: &str) -> (Ledger, aterm_tempfile::TempDir) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("cap-phase-{label}-"))
        .tempdir()
        .expect("a scratch ledger");
    (Ledger::at(dir.path().join("ledger")), dir)
}

#[track_caller]
fn admitted(w: Waiter, m: &Fixed) -> crate::Claim {
    match w.poll(m) {
        Polled::Admitted(c) => c,
        Polled::Waiting(_, short) => panic!("expected admission, waits: {short}"),
        Polled::Gone(e) => panic!("expected admission, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected admission, unqualified: {why}"),
    }
}

/// (a) A TWO-PHASE CLAIM RESERVES ITS COMPILE'S PEAK, THEN ITS RUN'S. A `targo test` admitted
/// with its whole run's 20 GiB, its run phase kept at 4 GiB, holds 20 GiB while its tree
/// compiles, so a 30 GiB claim behind it waits on 40 GiB available; once its tree runs a test
/// binary the claim steps to 4 GiB in its record, every other look reads that (the listing's
/// reservation included), and the waiter is admitted beside the run. Its run phase's peak is
/// kept apart from its compile's: the next need of its key asks that run phase's figure.
#[test]
fn a_two_phase_claim_reserves_its_compile_peak_then_its_test_peak() {
    let (ledger, _dir) = scratch("two");
    let m = Fixed::new(Reading::of_gib(64, 40, 14));
    let need = Need::new("targo test -p big").mem_gib(20).run_mem(4 * GIB);
    assert_eq!(need.run_mem, Some(4 * GIB));
    let mut build = admitted(Waiter::enter(&ledger, &m, need).expect("entered"), &m);
    let behind = Waiter::enter(&ledger, &m, Need::new("targo check").mem_gib(30)).expect("entered");
    let behind = match behind.poll(&m) {
        Polled::Waiting(w, _) => w,
        _ => panic!("a 30 GiB claim waits behind a 20 GiB compile on 40 GiB"),
    };
    let reserves = |ledger: &Ledger| {
        let listed = ledger.list().expect("listed");
        let at = listed
            .iter()
            .position(|r| r.held.is_some())
            .expect("the build's claim");
        (
            listed[at].need.mem_now(),
            reserved(&listed, None, &[])[at].mem,
        )
    };
    let mut phases = Phases::new();
    assert_eq!(
        phases.read(&mut build, &tree(&[10, 11, 13], 9 * GIB), &image),
        None
    );
    assert_eq!(
        reserves(&ledger),
        (20 * GIB, 20 * GIB),
        "the compile holds its peak"
    );
    assert_eq!(
        phases.read(&mut build, &tree(&[10, 20], GIB), &image),
        Some(Step::Down(4 * GIB))
    );
    assert_eq!(
        reserves(&ledger),
        (4 * GIB, 4 * GIB),
        "the run holds its own"
    );
    let listed = ledger.list().expect("listed");
    let stepped = listed
        .iter()
        .find(|r| r.held.is_some())
        .expect("the build's claim");
    assert_eq!(
        stepped.need.stepped,
        Some(4 * GIB),
        "as another process reads it"
    );
    assert_eq!(stepped.need.run_mem, Some(4 * GIB));
    assert_eq!(
        stepped.need.mem,
        20 * GIB,
        "admitted with its whole run's peak"
    );
    match behind.poll(&m) {
        Polled::Admitted(_) => {}
        Polled::Waiting(_, short) => panic!("the waiter is admitted beside the run: {short}"),
        Polled::Gone(e) => panic!("gone: {e}"),
        Polled::Unqualified(why) => panic!("unqualified: {why}"),
    }

    let key = Key {
        stage: "targo test".to_owned(),
        checkout: "/w".to_owned(),
        crates: Some(vec!["big".to_owned()]),
        variant: None,
    };
    let mut own = Records::default();
    own.push(key.clone(), 17 * GIB, 1);
    assert_eq!(
        run_need(&own, &key, Some(64 * GIB)),
        None,
        "no run phase kept yet"
    );
    own.push(run_key(&key), 3 * GIB, 2);
    assert_eq!(
        run_need(&own, &key, Some(64 * GIB)),
        Some(RunNeed {
            bytes: crate::peak_claim(3 * GIB),
            same_work: true,
        }),
        "its run phase's own figure"
    );
    assert_eq!(
        crate::book::need(&own, &key, None, Some(64 * GIB)).bytes,
        crate::peak_claim(17 * GIB),
        "the whole run's figure is the compile's still"
    );
}

/// One claim of a scripted schedule: when it asks (a tick), what it needs, and how long it
/// compiles and then runs, in ticks. A need that plans a step takes it as its compile ends.
struct Job {
    name: &'static str,
    asks: u64,
    need: Need,
    compiles: u64,
    runs: u64,
}

fn job(name: &'static str, asks: u64, need: Need, compiles: u64, runs: u64) -> Job {
    Job {
        name,
        asks,
        need,
        compiles,
        runs,
    }
}

/// When each of `jobs` is admitted, by the ledger's own looks on a machine with `available` GiB
/// that no claim's tree uses: every tick, the steps and ends due first, then each claim that asked
/// and waits, in its turn, looks once. `None` for one never admitted by tick 400. Every tick it
/// checks that what the held claims claim, and what the runs in their run phases take, fit the
/// machine.
fn schedule(available: u64, jobs: &[Job]) -> Vec<Option<u64>> {
    let reading = Reading::of_gib(64, available, 14);
    let mut records: Vec<Record> = jobs
        .iter()
        .enumerate()
        .map(|(i, j)| Record {
            timing: crate::TimingEvidence::default(),
            accounting: crate::Accounting::Holder,
            pid: 100 + u32::try_from(i).expect("few"),
            start_ms: j.asks * 1000 + u64::try_from(i).expect("few"),
            seq: 0,
            checkout: "/w".to_owned(),
            need: j.need.clone(),
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        })
        .collect();
    let mut admitted: Vec<Option<u64>> = vec![None; jobs.len()];
    let mut ended = vec![false; jobs.len()];
    for t in 0..=400 {
        for (i, j) in jobs.iter().enumerate() {
            let Some(at) = admitted[i] else { continue };
            if t == at + j.compiles && records[i].need.run_mem.is_some() {
                records[i].need.stepped = records[i].need.run_mem;
            }
            if t == at + j.compiles + j.runs {
                ended[i] = true;
            }
        }
        for i in 0..jobs.len() {
            if admitted[i].is_some() || jobs[i].asks > t {
                continue;
            }
            let others: Vec<Other<'_>> = records
                .iter()
                .enumerate()
                .filter(|(k, _)| {
                    *k != i && !ended[*k] && (admitted[*k].is_some() || jobs[*k].asks <= t)
                })
                .map(|(_, record)| Other {
                    record,
                    used: TreeUse::default(),
                    credited: TreeUse::default(),
                })
                .collect();
            if decide(&records[i].need, records[i].order(), &reading, &others, t).is_ok() {
                admitted[i] = Some(t);
                records[i].held = Some(crate::Held {
                    admitted_ms: t,
                    cores: 0,
                    alone: false,
                });
            }
        }
        let live = |i: usize| !ended[i] && admitted[i].is_some();
        let claimed: u64 = (0..jobs.len())
            .filter(|i| live(*i))
            .map(|i| records[i].need.mem_now())
            .sum();
        assert!(
            claimed <= available * GIB,
            "tick {t}: held claims claim {claimed} of {available} GiB"
        );
        let running: u64 = (0..jobs.len())
            .filter(|i| live(*i) && admitted[*i].is_some_and(|at| t >= at + jobs[*i].compiles))
            .map(|i| jobs[i].need.run_mem.unwrap_or(jobs[i].need.mem))
            .sum();
        assert!(
            running <= available * GIB,
            "tick {t}: run phases take {running} of {available} GiB"
        );
    }
    admitted
}

/// The schedule as `(name, tick)` pairs, to read in a failure.
fn named(jobs: &[Job], at: &[Option<u64>]) -> Vec<(&'static str, Option<u64>)> {
    jobs.iter()
        .map(|j| j.name)
        .zip(at.iter().copied())
        .collect()
}

/// (b) A CLAIM THAT FITS GOES FIRST PAST A WAITER THAT CANNOT START ONLY WITHIN WHAT THAT WAITER
/// SPARES, and the waiter's turn comes when it would have. On 40 GiB, A1 (12 GiB, ends at tick 10)
/// and A2 (18 GiB, ends at 30) are held; H asks 20 GiB and lacks 10, which A1's end is the least to
/// bring, 2 GiB over: H spares 2 GiB. Two 1 GiB claims go first past it; a third waits, since H's
/// turn at A1's end needs all that is left. H is admitted at tick 10 as under first in, first out;
/// the third once A2 ends.
///
/// Then the steps: X (20 GiB) plans a step down to 4 GiB as its compile ends at tick 10, and every
/// look reckons that step as one more end. H (18 GiB on 34) lacks 4, which the step brings 12 over:
/// a 12 GiB claim goes first, a 13 GiB one waits, and H is admitted at the step either way. And no
/// stream starves a head: a 1 GiB claim a tick behind H (20 GiB) and A (30 GiB, ends at 20) fills
/// what H spares, and H is admitted at A's end.
#[test]
fn a_claim_goes_first_past_a_waiter_only_within_what_it_spares() {
    let jobs = [
        job("A1", 0, Need::new("A1").mem_gib(12), 10, 0),
        job("A2", 0, Need::new("A2").mem_gib(18), 30, 0),
        job("H", 1, Need::new("H").mem_gib(20), 100, 0),
        job("C1", 2, Need::new("C1").mem_gib(1), 100, 0),
        job("C2", 3, Need::new("C2").mem_gib(1), 100, 0),
        job("C3", 4, Need::new("C3").mem_gib(1), 100, 0),
    ];
    let at = schedule(40, &jobs);
    assert_eq!(
        named(&jobs, &at),
        [
            ("A1", Some(0)),
            ("A2", Some(0)),
            ("H", Some(10)),
            ("C1", Some(2)),
            ("C2", Some(3)),
            ("C3", Some(30)),
        ]
    );

    let x = || Need::new("X").mem_gib(20).run_mem(4 * GIB);
    for (passer, gib, passes) in [("D", 12, true), ("C", 13, false)] {
        let jobs = [
            job("X", 0, x(), 10, 40),
            job("H", 1, Need::new("H").mem_gib(18), 100, 0),
            job(passer, 2, Need::new(passer).mem_gib(gib), 100, 0),
        ];
        let at = schedule(34, &jobs);
        assert_eq!(
            named(&jobs, &at),
            [
                ("X", Some(0)),
                ("H", Some(10)),
                (passer, Some(if passes { 2 } else { 50 })),
            ],
            "a {gib} GiB claim behind H"
        );
    }

    let mut stream = vec![
        job("A", 0, Need::new("A").mem_gib(30), 20, 0),
        job("H", 1, Need::new("H").mem_gib(20), 100, 0),
    ];
    stream.extend((2..=30).map(|t| job("C", t, Need::new("C").mem_gib(1), 1000, 0)));
    let at = schedule(40, &stream);
    assert_eq!(at[1], Some(20), "{:?}", named(&stream, &at));
    let first: Vec<Option<u64>> = at[2..].to_vec();
    assert!(
        first[..10].iter().all(|a| a.is_some_and(|t| t < 20)),
        "ten 1 GiB claims fill what H spares: {first:?}"
    );
    assert!(
        first[10..19].iter().all(|a| *a == Some(20)),
        "the rest wait for H's turn: {first:?}"
    );
}

/// (d) TWO TEST RUNS WHOSE PEAKS SUM OVER THE MACHINE STILL RUN ONE AT A TIME. A `targo test`
/// whose run phase took 25 GiB, more than its 8 GiB compile, asks 25 GiB: its whole run's peak, so
/// it plans no step. On 40 GiB a second such test waits through the first's compile and its run
/// and is admitted at its end, and a claim of the same shape behind a waiter that cannot start
/// does not go first past it, though it would fit beside the compile alone; a lighter test goes
/// first only within what that waiter spares. At no tick do the held claims, nor the runs in
/// their run phases, take more than the machine.
#[test]
fn two_test_runs_whose_peaks_sum_over_the_machine_run_one_at_a_time() {
    let test = |name: &'static str| Need::new(name).mem_gib(8).run_mem(25 * GIB);
    assert_eq!(test("X").mem, 25 * GIB);
    assert_eq!(test("X").run_mem, None);
    let jobs = [
        job("X", 0, test("X"), 10, 40),
        job("Y", 1, test("Y"), 10, 40),
    ];
    let at = schedule(40, &jobs);
    assert_eq!(named(&jobs, &at), [("X", Some(0)), ("Y", Some(50))]);

    let jobs = [
        job("X", 0, test("X"), 10, 40),
        job("H", 1, Need::new("H").mem_gib(26), 100, 0),
        job("P", 2, test("P"), 10, 40),
    ];
    let at = schedule(40, &jobs);
    assert_eq!(
        named(&jobs, &at),
        [("X", Some(0)), ("H", Some(50)), ("P", Some(150))]
    );

    // The passing rule holds a run to the machine too. H lacks 11 GiB beside X's 25, which X's
    // end brings 14 over: a test whose whole run takes 12 GiB goes first, beside X's run (37 of
    // 40), and one of 15 GiB, which fits beside X too, waits, since H's turn at X's end would then
    // come later.
    for (gib, passes) in [(12, true), (15, false)] {
        let jobs = [
            job("X", 0, test("X"), 10, 40),
            job("H", 1, Need::new("H").mem_gib(26), 100, 0),
            job("P", 2, Need::new("P").mem_gib(4).run_mem(gib * GIB), 10, 40),
        ];
        let at = schedule(40, &jobs);
        assert_eq!(
            named(&jobs, &at),
            [
                ("X", Some(0)),
                ("H", Some(50)),
                ("P", Some(if passes { 2 } else { 150 })),
            ],
            "a test of {gib} GiB behind H"
        );
    }
}

/// A RUN COUNTS WHAT ITS PROGRAMS TOOK BETWEEN TWO READS (CAP3): the kernel keeps each process's
/// lifetime peak, so a test binary that took 6 GiB between two reads that each found it holding
/// 1 GiB counts 6 GiB, in the run phase's peak its holder keeps and in the step, as the meter's
/// whole peak counts it. A claim whose run phase claims 4 GiB does not step down where its first
/// test binary already took more, and one that stepped steps back up at the first read that finds
/// a later test binary took more than the step. The driver, which a read found while the build
/// compiled, counts by what it holds now: the 9 GiB its life held was the compile's. NEGATIVE
/// CONTROLS: judged by the reads' sums alone, the first claim steps down beside a 6 GiB run and
/// keeps a 1.5 GiB run phase; judged by every member's lifetime peak, the driver's compile holds
/// the second claim at its whole peak through its run.
#[test]
fn a_run_counts_what_its_programs_took_between_two_reads() {
    let member = |pid, footprint, peak| Member {
        pid,
        start: 1,
        footprint,
        peak,
    };
    let mut claim = StandIn::new(20 * GIB, 4 * GIB);
    let mut phases = Phases::new();
    assert_eq!(
        phases.read(&mut claim, &[member(10, 9 * GIB, 9 * GIB)], &image),
        None,
        "compiling"
    );
    assert_eq!(
        phases.read(
            &mut claim,
            &[member(10, GIB / 2, 9 * GIB), member(20, GIB, 6 * GIB)],
            &image
        ),
        None,
        "its test binary took 6 GiB between the reads"
    );
    assert_eq!(claim.now, 20 * GIB, "no step beside a run past it");
    assert_eq!(phases.run_peak(), Some(6 * GIB), "the run's peak kept");

    let mut claim = StandIn::new(20 * GIB, 4 * GIB);
    let mut phases = Phases::new();
    assert_eq!(
        phases.read(&mut claim, &[member(10, 9 * GIB, 9 * GIB)], &image),
        None
    );
    assert_eq!(
        phases.read(
            &mut claim,
            &[member(10, GIB / 2, 9 * GIB), member(20, GIB, 2 * GIB)],
            &image
        ),
        Some(Step::Down(4 * GIB)),
        "the driver's compile peak is no run's"
    );
    assert_eq!(
        phases.read(
            &mut claim,
            &[member(10, GIB / 2, 9 * GIB), member(21, GIB, 5 * GIB)],
            &image
        ),
        Some(Step::Up(20 * GIB)),
        "a later test binary took 5 GiB between the reads"
    );
    assert_eq!(phases.run_peak(), Some(5 * GIB));
}

/// A WIDER SELECTION'S RUN PHASE BOUNDS THE STEP AND NEVER RAISES THE CLAIM (CAP3): a need of a
/// key whose own whole run claims 1.5 GiB keeps that, though a wider selection's run phase claims
/// 7.2 GiB: its own whole run bounds its run phase too, and a figure no lighter than it claims
/// plans no step. Lighter than it claims whole (the cold figure, or a wider whole run's), the
/// wider run phase is its step. A run phase of the same work kept heavier than its whole run is
/// its peak. And `run_need` reads the cold cap a wider selection's in another checkout takes.
/// NEGATIVE CONTROL: a wider run phase that raised the claim would claim 7.2 GiB.
#[test]
fn a_wider_selections_run_phase_bounds_the_step_and_never_raises_the_claim() {
    let wider = |bytes| {
        Some(RunNeed {
            bytes,
            same_work: false,
        })
    };
    let own = |bytes| {
        Some(RunNeed {
            bytes,
            same_work: true,
        })
    };
    let small = phased(3 * GIB / 2, wider(crate::peak_claim(6 * GIB)));
    assert_eq!(
        (small.mem, small.run_mem),
        (3 * GIB / 2, None),
        "its own whole run bounds it"
    );
    let cold = phased(16 * GIB, wider(crate::peak_claim(6 * GIB)));
    assert_eq!(
        (cold.mem, cold.run_mem),
        (16 * GIB, Some(crate::peak_claim(6 * GIB))),
        "a lighter wider run phase is its step"
    );
    let heavy = phased(3 * GIB / 2, own(3 * GIB));
    assert_eq!(
        (heavy.mem, heavy.run_mem),
        (3 * GIB, None),
        "its own run phase raises it"
    );
    assert_eq!(phased(16 * GIB, None).run_mem, None);

    let key = |checkout: &str, crates: &[&str]| Key {
        stage: "targo test".to_owned(),
        checkout: checkout.to_owned(),
        crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
        variant: None,
    };
    let mut book = Records::default();
    book.push(run_key(&key("/w", &["a", "b"])), 6 * GIB, 1);
    assert_eq!(
        run_need(&book, &key("/w", &["a"]), Some(64 * GIB)),
        wider(crate::peak_claim(6 * GIB)),
        "a wider selection in its checkout"
    );
    let mut far = Records::default();
    far.push(run_key(&key("/v", &["a", "b"])), 30 * GIB, 1);
    assert_eq!(
        run_need(&far, &key("/w", &["a"]), Some(64 * GIB)),
        wider(crate::cold_claim(Some(64 * GIB))),
        "a wider selection elsewhere, at most the cold figure"
    );
}

/// Each of `records` as a look takes it, no tree using any memory.
fn others(records: &[Record]) -> Vec<Other<'_>> {
    records
        .iter()
        .map(|record| Other {
            record,
            used: TreeUse::default(),
            credited: TreeUse::default(),
        })
        .collect()
}

/// A record of the ledger's for a look: `pid`, asked at `asks` (ms), held or waiting, needing
/// `need`.
fn claim_of(pid: u32, asks: u64, need: Need, held: bool) -> Record {
    Record {
        timing: crate::TimingEvidence::default(),
        accounting: crate::Accounting::Holder,
        pid,
        start_ms: asks,
        seq: 0,
        checkout: "/w".to_owned(),
        need,
        held: held.then_some(crate::Held {
            admitted_ms: asks,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// A WAITER WHOSE SHORTFALL NO CLAIM'S END BRINGS SPARES WHAT FIRST IN, FIRST OUT LEFT BESIDE IT
/// (CAP3): a VM's boot asks 20 GiB and 24 GiB spare beside it on 40 GiB with no claim held, so it
/// waits for memory no claim holds, and an 8 GiB claim behind it goes first beside its
/// reservation, as before the passing rule; a 21 GiB one waits, and its line says why, with no
/// figure that contradicts it. Where the claims ahead can bring the VM's room (a held 10 GiB
/// claim's end), what goes first past it is held to what it spares, 6 GiB, so its turn comes no
/// later, and a waiter that asked to run alone lets none go first. NEGATIVE CONTROL: a margin
/// waiter that spared nothing held the 8 GiB claim for good.
#[test]
fn a_waiter_no_claims_end_can_start_spares_what_first_in_first_out_left_beside_it() {
    let reading = Reading::of_gib(64, 40, 14);
    let look =
        |records: &[Record], need: Need| decide(&need, (9, 99, 0), &reading, &others(records), 10);
    let vm = claim_of(1, 1, Need::new("vm").mem_gib(20).margin(24 * GIB), false);
    let alone = [vm.clone()];
    assert!(
        look(&alone, Need::new("small").mem_gib(8)).is_ok(),
        "an 8 GiB claim goes first beside the VM's reservation"
    );
    let short = look(&alone, Need::new("big").mem_gib(21)).expect_err("21 GiB waits");
    assert_eq!(
        short.shorts,
        [Short::Memory {
            need: 21 * GIB,
            margin: 0,
            available: 40 * GIB,
            reserved: 20 * GIB,
            passes: Some(20 * GIB),
        }]
    );
    assert_eq!(
        short.shorts[0].to_string(),
        "memory short by 1 GiB: needs 21 GiB, 40 GiB available less 20 GiB reserved, and past \
         the claims ahead that cannot start yet only a claim of 20 GiB or less goes first"
    );

    let held = claim_of(2, 0, Need::new("held").mem_gib(10), true);
    let vm = claim_of(1, 1, Need::new("vm").mem_gib(20).margin(14 * GIB), false);
    let coverable = [held.clone(), vm];
    assert!(look(&coverable, Need::new("small").mem_gib(6)).is_ok());
    let short = look(&coverable, Need::new("small").mem_gib(8)).expect_err("past what it spares");
    assert_eq!(
        short.shorts,
        [Short::Memory {
            need: 8 * GIB,
            margin: 0,
            available: 40 * GIB,
            reserved: 30 * GIB,
            passes: Some(6 * GIB),
        }]
    );

    let lone = claim_of(1, 1, Need::new("lone").mem_gib(8).alone(), false);
    let short = look(&[held, lone], Need::new("small").mem_gib(1)).expect_err("none passes");
    assert!(
        short.shorts[0]
            .to_string()
            .ends_with("and no claim goes first past the claims ahead that cannot start yet"),
        "{}",
        short.shorts[0]
    );
}

/// A WAITER THAT FITS BESIDE THE RESERVATIONS AHEAD SPARES WHAT ITS FIT LEAVES (CAP3): on 40 GiB
/// with a 10 GiB claim held, a VM's boot of 20 GiB and 14 GiB beside it cannot start, and spares
/// 6 GiB (the held claim's end brings its room); an 8 GiB claim behind it fits the room but not
/// what the VM spares, so it waits for the VM alone, though the VM's 20 GiB and its own 8 leave 2
/// GiB of the room: a 2 GiB claim behind both goes first, since once the held claim ends the VM
/// and the 8 GiB claim both start beside it. NEGATIVE CONTROL: a waiter held only by another that
/// spared nothing would hold the 2 GiB claim too.
#[test]
fn a_waiter_that_fits_beside_the_reservations_ahead_spares_what_its_fit_leaves() {
    let reading = Reading::of_gib(64, 40, 14);
    let held = claim_of(2, 0, Need::new("held").mem_gib(10), true);
    let vm = claim_of(1, 1, Need::new("vm").mem_gib(20).margin(14 * GIB), false);
    let fits = claim_of(3, 2, Need::new("fits").mem_gib(8), false);
    let records = [held, vm, fits];
    let look = |need: Need| decide(&need, (9, 99, 0), &reading, &others(&records), 10);
    assert!(
        look(Need::new("two").mem_gib(2)).is_ok(),
        "2 GiB goes first past both"
    );
    let short = look(Need::new("three").mem_gib(3)).expect_err("3 GiB waits");
    assert_eq!(
        short.shorts,
        [Short::Memory {
            need: 3 * GIB,
            margin: 0,
            available: 40 * GIB,
            reserved: 38 * GIB,
            passes: Some(2 * GIB),
        }]
    );
}

/// Lead priority also orders the phased backfill calculation. Without the lead flag, the
/// older margin waiter leaves only 2 GiB past both waiters; with it, the compiler reserves
/// first and their eventual ends leave 6 GiB. The ordinary variant is the negative control.
#[test]
fn phased_backfill_orders_compiler_leads_before_older_waiters() {
    let reading = Reading::of_gib(64, 40, 14);
    let admits = |leading| {
        let held = claim_of(1, 0, Need::new("held").mem_gib(10), true);
        let older = claim_of(2, 1, Need::new("older").mem_gib(20).margin(14 * GIB), false);
        let mut compiler = Need::new("compiler").mem_gib(8);
        compiler.leads = leading;
        let lead = claim_of(3, 2, compiler, false);
        decide(
            &Need::new("small").mem_gib(5),
            (9, 99, 0),
            &reading,
            &others(&[held, older, lead]),
            10,
        )
        .is_ok()
    };
    assert!(admits(true));
    assert!(!admits(false));
}
