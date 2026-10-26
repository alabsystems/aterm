// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reap credit's ledger, step by step.

use super::{ReapFault, ReapLedger, Scope, StepIn};
use crate::explain::fixtures::tab;
use crate::model::{ProcSample, ProcState, TabRoot, attribute};

const ME: u32 = 501;
const ATERM: u32 = 100;
const SHELL: u32 = 200;
const MS: u64 = 1_000_000;

/// One process as a sweep reads it: pid, parent, CPU and reaped-children CPU in ms.
#[derive(Clone, Copy)]
struct P {
    pid: u32,
    ppid: u32,
    start: u64,
    cpu: u64,
    child: u64,
    zombie: bool,
    /// Its start, unix ms; `None` for the harness's: before the first step for a process the
    /// first step reads, else just before the step that first reads it.
    birth: Option<i64>,
    /// Whether it is root's, so its usage and start go unread.
    root: bool,
    /// Whether its usage and start go unread though it is this user's (mid-exit).
    unread: bool,
    /// Whether the host read it no birth (gone before it could).
    unborn: bool,
    /// Its session; 0 for unread.
    session: u32,
    /// Whether the kernel reaps its children for it (it ignores `SIGCHLD`), as the sweep reads
    /// it: of every process, at every reading, whatever it has reaped.
    nocld: bool,
}

fn p(pid: u32, ppid: u32, cpu: u64, child: u64) -> P {
    P {
        pid,
        ppid,
        start: u64::from(pid) * 10,
        cpu,
        child,
        zombie: false,
        birth: None,
        root: false,
        unread: false,
        unborn: false,
        session: 0,
        nocld: false,
    }
}

impl P {
    fn zombie(self) -> Self {
        Self {
            zombie: true,
            ..self
        }
    }

    fn started(self, start: u64) -> Self {
        Self { start, ..self }
    }

    /// Root's: no usage, no start.
    fn root(self) -> Self {
        Self { root: true, ..self }
    }

    /// Listed, this user's, but its usage and start unread.
    fn unread(self) -> Self {
        Self {
            unread: true,
            ..self
        }
    }

    /// Its birth unread: gone before the host read it.
    fn unborn(self) -> Self {
        Self {
            unborn: true,
            ..self
        }
    }

    /// In session `session`.
    fn session(self, session: u32) -> Self {
        Self { session, ..self }
    }

    /// Ignoring `SIGCHLD`: the kernel reaps its children for it.
    fn nocld(self) -> Self {
        Self {
            nocld: true,
            ..self
        }
    }

    /// Started before the first step, though a later step first reads it: its kernel start too
    /// is older than every one the harness starts at its pid's tenfold.
    fn old(self) -> Self {
        Self {
            birth: Some(0),
            start: u64::from(self.pid),
            ..self
        }
    }
}

/// aterm, its tab's shell (tab 1) and `rest`, at aterm's and the shell's CPU `at`.
fn sweep(at: u64, rest: &[P]) -> Vec<P> {
    let mut all = vec![p(ATERM, 1, at, 0), p(SHELL, ATERM, at, 0)];
    all.extend_from_slice(rest);
    all
}

struct Run {
    ledger: ReapLedger,
    tabs: Vec<TabRoot>,
    /// The pids a delegated job holds.
    jobs: Vec<u32>,
    at_ns: u64,
    /// Each process's start, unix ms, by pid and kernel start.
    births: std::collections::HashMap<(u32, u64), i64>,
}

/// The wall clock at monotonic `at_ns`, unix ms.
fn unix_ms(at_ns: u64) -> i64 {
    1_792_000_000_000 + i64::try_from(at_ns / MS).unwrap_or(i64::MAX)
}

impl Run {
    fn new() -> Self {
        Self {
            ledger: ReapLedger::default(),
            tabs: vec![tab(1, SHELL)],
            jobs: Vec::new(),
            at_ns: 0,
            births: std::collections::HashMap::new(),
        }
    }

    fn with(fault: ReapFault) -> Self {
        let mut run = Self::new();
        run.ledger.fault = Some(fault);
        run
    }

    fn step_cut(&mut self, ps: &[P], truncated: bool) {
        let first = self.at_ns == 0;
        let at_ns = self.at_ns + 1_000 * MS;
        let procs: Vec<ProcSample> = ps
            .iter()
            .map(|q| ProcSample {
                birth_ms: (!q.unborn).then(|| {
                    q.birth.unwrap_or_else(|| {
                        *self.births.entry((q.pid, q.start)).or_insert(if first {
                            0
                        } else {
                            unix_ms(at_ns) - 500
                        })
                    })
                }),
                pid: q.pid,
                ppid: q.ppid,
                start: (!q.root && !q.unread).then_some(q.start),
                cpu_ns: (!q.root && !q.unread).then_some(q.cpu * MS),
                child_cpu_ns: (!q.root && !q.unread).then_some(q.child * MS),
                uid: Some(if q.root { 0 } else { ME }),
                sid: q.session,
                reaps_unwaited: Some(q.nocld),
                state: if q.zombie {
                    ProcState::Zombie
                } else {
                    ProcState::Run
                },
                ..ProcSample::default()
            })
            .collect();
        let members = attribute(&procs, ATERM, &self.tabs);
        let delegated: Vec<bool> = procs.iter().map(|q| self.jobs.contains(&q.pid)).collect();
        self.at_ns = at_ns;
        self.ledger.step(&StepIn {
            at_ns,
            at_unix_ms: unix_ms(at_ns),
            procs: &procs,
            members: &members,
            delegated: &delegated,
            tabs: &self.tabs,
            self_uid: ME,
            truncated,
        });
    }

    fn step(&mut self, ps: &[P]) -> u64 {
        self.step_cut(ps, false);
        self.tab()
    }

    /// Tab 1's counter, ms.
    fn tab(&self) -> u64 {
        self.ledger.totals().tab(1) / MS
    }

    /// Every counter summed, ms.
    fn all(&self) -> u64 {
        self.ledger.totals().all / MS
    }
}

/// The last step that knowingly missed some of tab 1's work; 0 for none.
fn missed(r: &Run) -> u64 {
    r.ledger
        .totals()
        .tabs
        .iter()
        .find(|t| t.key == 1)
        .map_or(0, |t| t.missed)
}

/// Tab `key`'s growth since `base`, ms, and whether a step between knowingly missed some of it.
fn since(r: &Run, base: &super::ReapTotals, key: u64) -> (u64, bool) {
    let (ns, missed) = r.ledger.totals().since(base, key);
    (ns / MS, missed)
}

/// Tab 1's certain and uncertain pending at `pid`'s row, ms.
fn owed(r: &Run, pid: u32) -> Option<(u64, u64)> {
    r.ledger
        .row(pid)
        .map(|v| (v.pending_ns / MS, v.doubt_ns / MS))
}

/// The first step counts nothing (every process was there before it); a process born under a
/// member since counts from its birth, and its reaped children with it.
#[test]
fn the_first_step_is_a_baseline_and_a_child_born_since_counts_from_birth() {
    let mut r = Run::new();
    assert_eq!(r.step(&sweep(50, &[p(300, SHELL, 9_000, 4_000)])), 0);
    assert_eq!(missed(&r), 0, "a baseline misses nothing");
    assert_eq!(
        r.step(&sweep(
            50,
            &[p(300, SHELL, 9_100, 4_000), p(310, 300, 70, 30)]
        )),
        100 + 70 + 30
    );
}

/// A process older than the last step that a step first reads (one the last sweep could not
/// read, a shell adopted from a predecessor) counts from that step: its earlier work is no
/// window's, so it is no member counted since an earlier step either.
#[test]
fn a_process_older_than_the_last_step_counts_from_its_first_reading() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    let old = |cpu| p(310, 300, cpu, 4_000).old();
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 0), old(9_000)])), 0);
    assert!(!r.ledger.counted(310, Some(310), 1));
    assert!(r.ledger.counted(310, Some(310), 2));
    assert_eq!(missed(&r), 2, "what it ran since the last step is missed");
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 0), old(9_100)])), 100);
    assert_eq!(missed(&r), 2);
}

/// A build's compilers that start and finish between two readings, reaped by a member, count in
/// the tab through its reaped-children time. NEGATIVE CONTROL: aterm's own counter does not
/// take them.
#[test]
fn work_reaped_between_readings_by_a_member_counts_in_its_tab() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 10, 2_500)])), 2_510);
    assert_eq!(r.all(), 2_510, "aterm's own took none of it");
}

/// A member read alive, then reaped by its member parent: what was counted of it is pending
/// against the parent and absorbed from its reaped-children time, so the child counts once, its
/// tail since its last reading included.
#[test]
fn a_member_reaped_by_its_parent_counts_once_with_its_tail() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 100)]));
    assert_eq!(r.tab(), 500);
    // 310 ran 200 more ms, exited and was reaped: 300's reaped time grew by its 700.
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 700)])), 700);
    assert_eq!(r.ledger.row(300).map(|v| v.pending_ns), Some(0));
}

/// A member gone while its parent has yet to show it (its reap read after the parent's row, or
/// the parent's reaped time read before): the pending waits for the growth to come.
#[test]
fn pending_waits_for_the_parents_reaped_time_to_show_it() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 0)])), 400);
    assert_eq!(r.ledger.row(300).map(|v| v.pending_ns), Some(400 * MS));
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 450)])), 450);
    assert_eq!(r.ledger.row(300).map(|v| v.pending_ns), Some(0));
}

/// A member that moved to launchd keeps its tab (sticky membership), its children too, and
/// when launchd reaps it nothing is pending: its parent's later reaped time counts in full.
/// The counter never goes back. NEGATIVE CONTROL: [`ReapFault::KeepFirstParent`] leaves its
/// work pending, so the parent's next reaped child is absorbed against it.
#[test]
fn a_member_launchd_reaps_keeps_its_tab_and_leaves_nothing_pending() {
    let run = |mut r: Run| {
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 1_000, 0)]));
        // 300's wrapper exited: 310 runs on under launchd, with a child of its own.
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 1, 1_300, 0), p(320, 310, 50, 0)],
        ));
        assert_eq!(r.ledger.row(310).map(|v| v.scope), Some(Scope::Tab(1)));
        assert_eq!(r.ledger.row(320).map(|v| v.scope), Some(Scope::Tab(1)));
        assert_eq!(r.tab(), 1_350);
        // Launchd reaped both; then 300 reaps a 200 ms child of its own.
        let gone = r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        assert_eq!(gone, 1_350, "the counter never goes back");
        r.step(&sweep(0, &[p(300, SHELL, 0, 200)]))
    };
    assert_eq!(run(Run::new()), 1_550);
    assert_eq!(
        run(Run::with(ReapFault::KeepFirstParent)),
        1_350,
        "NEGATIVE CONTROL"
    );
}

/// A member read as a zombie counts at its final figure, read once: later readings of the
/// zombie add nothing, and its reap is absorbed. NEGATIVE CONTROL:
/// [`ReapFault::ForgetZombieRead`] keeps its last live reading, so launchd's reap of it loses
/// the tail.
#[test]
fn a_zombie_counts_at_its_final_figure_once() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
    let final_ = r.step(&sweep(
        0,
        &[p(300, SHELL, 0, 0), p(310, 300, 650, 0).zombie()],
    ));
    assert_eq!(final_, 650);
    assert_eq!(
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 300, 650, 0).zombie()]
        )),
        650
    );
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 650)])), 650);

    // The same member, moved to launchd before it exited: launchd reaps it.
    let orphan = |mut r: Run| {
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 1, 650, 0).zombie()],
        ));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]))
    };
    assert_eq!(orphan(Run::new()), 650);
    assert_eq!(
        orphan(Run::with(ReapFault::ForgetZombieRead)),
        400,
        "NEGATIVE CONTROL"
    );
}

/// A grandchild and its parent both gone between two readings, the grandparent reaping the
/// parent: the grandchild's counted part passes up to the grandparent, so nothing counts twice.
#[test]
fn a_member_and_its_child_gone_together_pass_their_pending_up() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(
        0,
        &[
            p(300, SHELL, 0, 0),
            p(310, 300, 100, 0),
            p(320, 310, 300, 0),
        ],
    ));
    assert_eq!(r.tab(), 400);
    // 320 ran to 350 and 310 reaped it; 310 ran to 120 and 300 reaped it.
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 470)])), 470);
    assert_eq!(missed(&r), 0, "the reaper's reaped time held all of it");
}

/// The same, but the grandparent reaped the parent while the sweep ran, after its own row was
/// read: its reaped time did not move though a child it stayed the parent of went, which says
/// the reap came after its reading, so what the grandchild left waits for its next reading as
/// at once. That reading holds it, so it counts once, and only the first step is marked: its
/// window lacks the tails the next one reads. Had launchd reaped the grandchild, the next reading
/// falls short of it and is marked too. NEGATIVE CONTROL (by hand): without the late-reap rule
/// the healthy run's second step is marked as well.
#[test]
fn a_member_gone_with_its_parent_waits_one_reading_for_the_reap() {
    let run = |reaped: u64| {
        let mut r = Run::new();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(
            0,
            &[
                p(300, SHELL, 0, 0),
                p(310, 300, 100, 0),
                p(320, 310, 300, 0),
            ],
        ));
        assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 0)])), 400);
        assert_eq!(missed(&r), 3, "its reaped time fell short of it there");
        let tab = r.step(&sweep(0, &[p(300, SHELL, 0, reaped)]));
        (tab, owed(&r, 300), missed(&r))
    };
    assert_eq!(run(470), (470, Some((0, 0)), 3), "the next reading held it");
    assert_eq!(
        run(120),
        (400, Some((0, 280)), 4),
        "launchd reaped the grandchild: the next reading fell short of it"
    );
}

/// A build killed with its compilers: cargo and its rustcs gone between two readings, the
/// rustcs reaped by launchd (cargo died without waiting for them), so the shell's reaped time
/// shows cargo's work alone. What the rustcs left waits through the shell's next reading, which
/// it takes out of, and is dropped: the shell's later children count in full. NEGATIVE
/// CONTROL: [`ReapFault::HoldEveryPending`] keeps it waiting, so it is taken out of every later
/// child until it is spent.
#[test]
fn a_build_killed_with_its_compilers_leaves_no_debt() {
    let run = |mut r: Run| {
        r.step(&sweep(0, &[]));
        r.step(&sweep(
            0,
            &[
                p(300, SHELL, 100, 0),
                p(310, 300, 2_000, 0),
                p(320, 300, 3_000, 0),
            ],
        ));
        assert_eq!(r.tab(), 5_100);
        let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
        // Killed: cargo ran 10 ms more and the shell reaped it; launchd reaped the rustcs.
        assert_eq!(r.step(&shell(110)), 5_100);
        // The shell's next two jobs, 1,000 ms each, started and finished between readings:
        // the first is taken out against what the rustcs left, the second counts.
        r.step(&shell(1_110));
        (
            r.step(&shell(2_110)),
            r.ledger.row(SHELL).map(|v| v.pending_ns),
        )
    };
    assert_eq!(run(Run::new()), (6_100, Some(0)));
    assert_eq!(
        run(Run::with(ReapFault::HoldEveryPending)),
        (5_100, Some(2_990 * MS)),
        "NEGATIVE CONTROL"
    );
}

/// A member listed but unread for a reading (mid-exit) keeps its row: read next as a zombie it
/// counts at its final figure, and its reap leaves nothing owed. NEGATIVE CONTROL: as gone, it
/// would go pending at its last live figure, come back as a new row counted from its final
/// figure (born before the last step), and leave that first figure owed against its parent.
#[test]
fn a_member_listed_but_unread_keeps_its_row() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
    assert_eq!(
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 300, 0, 0).unread()]
        )),
        400
    );
    assert_eq!(r.ledger.row(310).map(|v| v.cpu_ns), Some(400 * MS));
    assert_eq!(
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 300, 650, 0).zombie()]
        )),
        650
    );
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 650)])), 650);
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 750)])), 750);
    assert_eq!(r.ledger.row(300).map(|v| v.pending_ns), Some(0));
}

/// A member under a process whose usage is not read (root's `sudo`) passes its pending up
/// through it: the sudo gets no row, and the shell's reaped time, once it reaps the sudo, holds
/// the member's, so its counted 400 ms are absorbed there, not counted again (855, not 1,255).
#[test]
fn a_member_under_a_process_with_no_row_passes_its_pending_through_it() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let sudo = p(400, SHELL, 5, 0).root();
    assert_eq!(r.step(&sweep(0, &[sudo, p(410, 400, 400, 0)])), 400);
    assert_eq!(r.ledger.row(400), None);
    assert_eq!(r.ledger.row(410).and_then(|v| v.parent), Some(SHELL));
    // 410 ran 50 ms more and the sudo reaped it; the shell reaped the sudo (5 ms of its own).
    let after = r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, 5 + 450)]);
    assert_eq!(after, 455);
}

/// A build killed under a wrapper: make (300) ran cargo (310), whose rustcs (320, 330) were
/// killed with it and reaped by launchd; make reaped cargo and exited before the next reading,
/// the shell reaping it with its next 1,000 ms job. What the rustcs left was uncertain at make
/// (their parent went too) and stays uncertain when make goes, so it waits through the shell's
/// next reading and is dropped: the shell's third job counts in full, nothing is owed, and each
/// step it took work out against is marked as one that missed some of the tab's.
#[test]
fn uncertain_work_stays_uncertain_when_its_holder_goes() {
    let mut r = Run::new();
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
    r.step(&shell(0));
    let build = [
        p(300, SHELL, 100, 0),
        p(310, 300, 100, 0),
        p(320, 310, 2_000, 0),
        p(330, 310, 3_000, 0),
    ];
    r.step(&[&shell(0)[..], &build].concat());
    assert_eq!(r.tab(), 5_200);
    // Killed: cargo ran 10 ms more and make reaped it; launchd reaped the rustcs.
    r.step(&[&shell(0)[..], &[p(300, SHELL, 100, 110)]].concat());
    assert_eq!((r.tab(), owed(&r, 300)), (5_200, Some((0, 4_990))));
    assert_eq!(missed(&r), 3);
    r.step(&shell(210 + 1_000));
    assert_eq!(owed(&r, SHELL), Some((0, 3_990)), "still uncertain");
    r.step(&shell(2_210));
    assert_eq!(missed(&r), 5);
    assert_eq!(r.step(&shell(3_210)), 6_200, "the third job counts");
    assert_eq!((owed(&r, SHELL), missed(&r)), (Some((0, 0)), 5));
}

/// The same build, the shell's next job big enough to cover what make held uncertain: the step
/// that takes it out of the shell's reaped time is marked as one that may have missed some of
/// the tab's work, since what it took out may be the job's.
#[test]
fn uncertain_work_passed_up_marks_the_step_that_takes_it_out() {
    let mut r = Run::new();
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
    r.step(&shell(0));
    let build = [
        p(300, SHELL, 100, 0),
        p(310, 300, 100, 0),
        p(320, 310, 2_000, 0),
        p(330, 310, 3_000, 0),
    ];
    r.step(&[&shell(0)[..], &build].concat());
    r.step(&[&shell(0)[..], &[p(300, SHELL, 100, 110)]].concat());
    assert_eq!(missed(&r), 3);
    assert_eq!(r.step(&shell(210 + 6_000)), 5_200 + 1_010);
    assert_eq!((owed(&r, SHELL), missed(&r)), (Some((0, 0)), 4));
}

/// A build killed with its compilers whose cargo (300) waits as an unreaped zombie: the rustcs,
/// launchd's to reap, are gone, and a zombie reaps nothing, so what they left is uncertain, as
/// under a parent gone too. When the shell reaps cargo with its next job, the shell's third job
/// counts in full and nothing is owed.
#[test]
fn work_left_under_a_zombie_parent_is_uncertain() {
    let mut r = Run::new();
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
    r.step(&shell(0));
    let build = [
        p(300, SHELL, 100, 0),
        p(310, 300, 2_000, 0),
        p(320, 300, 3_000, 0),
    ];
    r.step(&[&shell(0)[..], &build].concat());
    // Killed: cargo ran 10 ms more and waits unreaped; launchd reaped the rustcs.
    r.step(&[&shell(0)[..], &[p(300, SHELL, 110, 0).zombie()]].concat());
    assert_eq!((r.tab(), owed(&r, 300)), (5_110, Some((0, 5_000))));
    r.step(&shell(110 + 1_000));
    r.step(&shell(2_110));
    assert_eq!(r.step(&shell(3_110)), 6_110, "the third job counts");
    assert_eq!((owed(&r, SHELL), missed(&r)), (Some((0, 0)), 5));
}

/// A member read under launchd and reaped by launchd loses its tail since its last reading:
/// the ledger marks that step as one that missed some of its tab's work, so a window over it is
/// a lower bound. One last read as a zombie was counted at its final figure, and misses nothing.
#[test]
fn a_tail_launchd_reaped_is_marked_missed() {
    let run = |last: P| {
        let mut r = Run::new();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 100, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), last]));
        let base = r.ledger.totals();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.ledger.totals().since(&base, 1)
    };
    assert_eq!(run(p(310, 1, 150, 0)), (0, true));
    assert_eq!(run(p(310, 1, 150, 0).zombie()), (0, false));
}

/// A pid reused by another user's process, whose kernel start goes unread, is told from the
/// member it held by its birth: the member is gone, and what was counted of it is pending at
/// its parent, which reaped it (450, not 850). A setuid exec keeps its birth, and so its row.
#[test]
fn a_pid_another_user_reused_is_a_new_process() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
    // 310 ran 50 ms more and 300 reaped it; root's new process took its pid.
    let root = p(310, 1, 0, 0).started(77).root();
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 450), root])), 450);
    assert_eq!(r.ledger.row(310), None);

    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 400, 0)]));
    let setuid = p(310, 300, 0, 0).root();
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 0, 0), setuid])), 400);
    assert_eq!(r.ledger.row(310).map(|v| v.cpu_ns), Some(400 * MS));
}

/// A split tab whose least pane closes takes its next pane's key and keeps its counter: the
/// ledger's id for a tab stays while any of its panes does, so the tab's growth over a window
/// across the close is all of it, and none of it folds into aterm's own.
#[test]
fn a_split_tab_keeps_its_counter_when_its_least_pane_closes() {
    const RIGHT: u32 = 210;
    let mut r = Run::new();
    r.tabs = vec![
        tab(1, SHELL),
        TabRoot {
            tab: 1,
            tab_id: Some(1),
            ..tab(2, RIGHT)
        },
    ];
    let panes = |work| {
        [
            p(ATERM, 1, 0, 0),
            p(SHELL, ATERM, 0, 0),
            p(RIGHT, ATERM, work, 0),
        ]
    };
    r.step(&panes(0));
    let base = r.ledger.totals();
    r.step(&panes(100));
    // The left pane closes: its shell exits, aterm reaps it, and the tab takes the right's key.
    r.tabs = vec![TabRoot {
        tab_id: Some(1),
        ..tab(2, RIGHT)
    }];
    r.step(&[p(ATERM, 1, 0, 0), p(RIGHT, ATERM, 200, 0)]);
    let now = r.ledger.totals();
    assert_eq!(now.since(&base, 2), (200 * MS, false));
    assert_eq!(now.all - base.all, 200 * MS, "none in aterm's own");
}

/// A short job the sweep read but the host read no birth for (gone before it could) is dated by
/// its kernel start: one newer than every start the last sweep read was born since and counts
/// from its birth, unmarked; one older counts from its first reading, its work before it missed.
#[test]
fn a_process_with_no_birth_read_is_dated_by_its_kernel_start() {
    let run = |start| {
        let mut r = Run::new();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0).started(3_000)]));
        let job = p(310, 300, 40, 0).started(start).unborn();
        let tab = r.step(&sweep(0, &[p(300, SHELL, 0, 0).started(3_000), job]));
        (tab, missed(&r))
    };
    assert_eq!(
        run(9_000),
        (40, 0),
        "newer than every process the last sweep read"
    );
    assert_eq!(run(2_500), (0, 2), "older than one");
}

/// A process whose spawn straddled a reading (`targo` spawning `trustc`: its fork stamped 1 ms
/// before the reading's wall clock, but still being created as the sweep listed, so that sweep
/// read it no start) is first read at the next reading, older than that wall clock by its birth:
/// its kernel start, newer than every one that sweep read, says nothing counted it, so it counts
/// from its birth, unmarked; one older than a process that sweep read counts from its first
/// reading, its work before missed. Live (2026-10-26), dated by its birth alone, the first read
/// `tab_mc=≥1019 … unattributed_mc=1650` over a 591 ms window, its 1,039 ms lost. NEGATIVE
/// CONTROL (by hand): dated by its birth where the host read one, the first reads (0, true) too.
#[test]
fn a_process_spawned_as_a_sweep_listed_counts_from_its_birth() {
    let run = |start| {
        let mut r = Run::new();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        let base = r.ledger.totals();
        let spawned = P {
            birth: Some(unix_ms(2_000 * MS) - 1),
            ..p(310, 300, 1_039, 0).started(start)
        };
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), spawned]));
        (since(&r, &base, 1), missed(&r))
    };
    assert_eq!(
        run(9_000),
        ((1_039, false), 0),
        "newer than every process the last sweep read"
    );
    assert_eq!(run(2_500), ((0, true), 3), "older than one");
}

/// A pid reused by a new process is a new member, never the old one's row.
#[test]
fn a_reused_pid_is_a_new_member() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 900, 0)]));
    // 310 was reaped by launchd (it had moved there), and its pid now names a new child.
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 1, 900, 0)]));
    assert_eq!(
        r.step(&sweep(
            0,
            &[p(300, SHELL, 0, 0), p(310, 300, 5, 0).started(77)]
        )),
        905
    );
}

/// A closed tab's counter folds into aterm's own, its members' later work counts there, and
/// the sum keeps every tab's work.
#[test]
fn a_closed_tabs_work_folds_into_aterms_own() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 300, 0)]));
    assert_eq!((r.tab(), r.all()), (300, 300));
    r.tabs.clear();
    r.step(&sweep(0, &[p(300, SHELL, 500, 0)]));
    assert_eq!((r.tab(), r.all()), (0, 500));
    assert_eq!(r.ledger.row(300).map(|v| v.scope), Some(Scope::Own));
}

/// A process aterm started before its tab was read (the shell, first seen as aterm's own)
/// moves to the tab once a reading puts it there; what it ran before stays aterm's.
#[test]
fn a_tabs_shell_first_seen_as_aterms_moves_to_its_tab() {
    let mut r = Run::new();
    r.tabs.clear();
    r.step(&sweep(0, &[]));
    r.step(&sweep(40, &[]));
    assert_eq!((r.tab(), r.all()), (0, 80));
    r.tabs = vec![tab(1, SHELL)];
    r.step(&sweep(100, &[]));
    assert_eq!((r.tab(), r.all()), (60, 200));
}

/// A cut sweep is skipped: a member it missed may be alive, so nothing goes pending and the
/// next whole sweep reads on from the last.
#[test]
fn a_cut_sweep_is_skipped() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 100, 0)]));
    r.step_cut(&sweep(0, &[p(300, SHELL, 0, 0)]), true);
    assert_eq!(r.ledger.row(310).map(|v| v.cpu_ns), Some(100 * MS));
    assert_eq!(
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 160, 0)])),
        160
    );
}

/// THE NAIVE Δ(LIVE + REAPED) DIPS: each counter its members' `cpu + child` now loses a
/// member's whole work when launchd reaps it, where the ledger keeps it.
#[test]
fn the_naive_sum_dips_when_launchd_reaps_a_member() {
    let script = |mut r: Run| {
        r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 1_000, 0)]));
        r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 1, 1_000, 0)]));
        let before = r.tab();
        (before, r.step(&sweep(0, &[p(300, SHELL, 100, 0)])))
    };
    assert_eq!(script(Run::new()), (1_000, 1_100));
    let (before, after) = script(Run::with(ReapFault::Naive));
    assert!(after < before, "naive: {before} then {after}");
}

/// A parent that ignores `SIGCHLD` has the kernel reap its children for it, which adds each
/// one's time to its reaped time twice (measured on macOS 27: 400 ms for a 200 ms child). None
/// of that growth counts past what it absorbs, and each such step is marked short of the tab's
/// work: the tab reads 110 of the 360 ms its processes ran, a lower bound, never the 710 the
/// doubled time would read. A member the kernel reaped for it is absorbed once.
#[test]
fn a_parent_the_kernel_reaps_for_counts_none_of_its_doubled_reaped_time() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0).nocld()]));
    // An unseen 200 ms child, reaped for it (400 ms); 310 born and read at 100 ms.
    let tab = r.step(&sweep(
        0,
        &[p(300, SHELL, 10, 400).nocld(), p(310, 300, 100, 0)],
    ));
    assert_eq!((tab, missed(&r)), (110, 2));
    // 310 ran 50 ms more and exited, reaped for it: 300's reaped time grew by 300.
    assert_eq!(r.step(&sweep(0, &[p(300, SHELL, 10, 700).nocld()])), 110);
    assert_eq!((owed(&r, 300), missed(&r)), (Some((0, 0)), 3));
}

/// The doubled time passes up: when a parent the kernel reaped for exits, its reaper's reaped
/// time holds its tail's doubled growth too, so the growth past what is absorbed at that step,
/// and at the next (the reaper may have reaped it while the sweep ran, after its reading),
/// counts nothing and is marked. NEGATIVE CONTROL (by hand): without the pass-up, the window over
/// that step reads the 100 ms doubled tail as exact, against the 50 ms its child ran.
#[test]
fn a_parent_the_kernel_reaped_for_passes_its_doubt_to_its_reaper() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0).nocld()]));
    r.step(&sweep(0, &[p(300, SHELL, 10, 400).nocld()]));
    let base = r.ledger.totals();
    // 300 had another 50 ms child reaped for it (100 more), then exited; the shell reaped it.
    r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, 10 + 500)]);
    assert_eq!(since(&r, &base, 1), (0, true));
    assert_eq!(owed(&r, SHELL), Some((0, 0)));
    // The shell's next two jobs, 1,000 ms each: the first reading is still in doubt, the second
    // counts in full.
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, 1_510)]);
    assert_eq!(since(&r, &base, 1), (0, true));
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, 2_510)]);
    assert_eq!(since(&r, &base, 1), (1_000, false));
}

/// A short-lived parent that ignores `SIGCHLD`, read once with nothing reaped yet (the sweep reads
/// every process's flag), runs one batch of children the kernel reaps for it and exits, its
/// waiting parent reaping it between two readings: that reaper's reaped growth holds the batch
/// twice (2,010 ms for 1,010 run), so the window over the exits counts none of it past what is
/// absorbed and is marked. NEGATIVE CONTROL (by hand): with the parent read as waiting, the window
/// reads (1,800, false), the batch's 800 ms since its reading counted twice as exact.
#[test]
fn a_one_batch_parent_the_kernel_reaps_for_marks_its_reapers_window() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    let read = r.step(&sweep(
        0,
        &[
            p(300, SHELL, 0, 0),
            p(310, 300, 10, 0).nocld(),
            p(320, 310, 100, 0),
            p(330, 310, 100, 0),
        ],
    ));
    assert_eq!((read, missed(&r)), (210, 0));
    let base = r.ledger.totals();
    // 320 and 330 ran to 500 ms each, reaped for 310 (2,000 ms); 300 reaped 310.
    r.step(&sweep(0, &[p(300, SHELL, 0, 10 + 2_000)]));
    assert_eq!(since(&r, &base, 1), (0, true));
    assert_eq!(owed(&r, 300), Some((0, 0)));
}

/// A tab's process that outlived its parent (an orphan launchd adopted) is in no parent chain to
/// the tab, but it is in the session of the tab's shell, and every process in a session descends
/// from its leader: it takes that tab, so its work counts there, and launchd's reap of it, which
/// loses its tail, marks the step. NEGATIVE CONTROL: in a session aterm's own process is in too
/// it takes no tab, its work in no counter and nothing marked.
#[test]
fn an_orphan_in_a_tabs_session_keeps_the_tab() {
    let run = |helper_session: u32| {
        let mut r = Run::new();
        let shell = p(SHELL, ATERM, 0, 0).session(SHELL);
        let helper = p(330, ATERM, 0, 0).session(helper_session);
        let at = |rest: &[P]| [&[p(ATERM, 1, 0, 0), shell, helper][..], rest].concat();
        r.step(&at(&[]));
        let tab = r.step(&at(&[p(320, 1, 6, 0).session(SHELL)]));
        r.step(&at(&[]));
        (tab, missed(&r))
    };
    assert_eq!(run(330), (6, 3));
    assert_eq!(run(SHELL), (0, 0), "NEGATIVE CONTROL");
}

/// A pane shared into a second tab (Open Active Session in New Window) and listed before its
/// own tab, which the host gives each pane to, leaves each tab its id: an id follows the host's
/// identity for the tab, never a pane, so over a window across the share the split tab keeps its
/// work from before it and the new tab takes none of it, only the shared pane's work since. When
/// the split tab closes, the shared pane's processes count in the tab that still shows them. The
/// same holds where the shared pane is the split tab's least, its key then moving to its other
/// pane's (the host's roots: `vitals_host_tests.rs`). NEGATIVE CONTROL (by hand): ids by pane
/// alone give the new tab the split tab's id, and it reads the split tab's 500 ms.
#[test]
fn a_shared_pane_listed_first_takes_no_tabs_counter() {
    let root = |key, tab, shell, id| TabRoot {
        key,
        tab,
        shell_pid: shell,
        tab_id: Some(id),
        ..crate::explain::fixtures::tab(key, shell)
    };
    let panes = |left, right| {
        [
            p(ATERM, 1, 0, 0),
            p(205, ATERM, left, 0),
            p(207, ATERM, right, 0),
        ]
    };
    let mut r = Run::new();
    r.tabs = vec![root(5, 5, 205, 10), root(7, 5, 207, 10)];
    r.step(&panes(0, 0));
    let base = r.ledger.totals();
    r.step(&panes(500, 0));
    // Pane 7 is shared into tab 20, which the host lists first.
    r.tabs = vec![root(7, 7, 207, 20), root(5, 5, 205, 10)];
    r.step(&panes(2_500, 100));
    assert_eq!(since(&r, &base, 5), (2_500, false), "the split tab's");
    assert_eq!(
        since(&r, &base, 7),
        (100, false),
        "the shared pane's, where it is listed"
    );
    // The split tab closes; the twin keeps pane 7.
    r.tabs = vec![root(7, 7, 207, 20)];
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 0), p(207, ATERM, 400, 0)]);
    assert_eq!(since(&r, &base, 7), (300, false));

    // The split tab's least pane, 5, shared into the twin listed first: the host gives pane 5 to
    // the twin and the split tab its other pane's key, so the split tab keeps its counter under
    // key 7 and the twin takes none of it, only pane 5's work since.
    let mut r = Run::new();
    r.tabs = vec![root(5, 5, 205, 10), root(7, 5, 207, 10)];
    r.step(&panes(0, 0));
    let base = r.ledger.totals();
    r.step(&panes(500, 0));
    r.tabs = vec![root(5, 5, 205, 20), root(7, 7, 207, 10)];
    r.step(&panes(2_500, 100));
    assert_eq!(since(&r, &base, 7), (600, false), "the split tab's");
    assert_eq!(since(&r, &base, 5), (2_000, false), "the twin's, since");
}

/// A member reaped by a live process whose usage goes unread (root's `sudo -u me`) passes its
/// work up only when that process goes: until then it is held there, so the shell's own job
/// meanwhile counts in full (1,000, not 600), and the sudo's exit leaves only its own 5 ms and
/// the member's 50 ms tail (55, not 455). The step the member goes is marked: its tail is in no
/// figure until the sudo goes. NEGATIVE CONTROL (by hand): pending at the shell from the
/// member's exit, it is taken out of the shell's job instead.
#[test]
fn work_a_live_unread_process_reaped_waits_for_it_to_go() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let sudo = p(400, SHELL, 5, 0).root();
    assert_eq!(r.step(&sweep(0, &[sudo, p(410, 400, 400, 0)])), 400);
    // 410 ran 50 ms more and the sudo reaped it; the sudo lives on.
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child), sudo];
    assert_eq!(r.step(&shell(0)), 400);
    assert_eq!(owed(&r, SHELL), Some((0, 0)), "held at the sudo");
    assert_eq!(missed(&r), 3, "its tail is held");
    // The shell's own 1,000 ms job.
    let base = r.ledger.totals();
    r.step(&shell(1_000));
    assert_eq!(since(&r, &base, 1), (1_000, false));
    // The sudo exits; the shell reaps it (its 5 ms, and 410's 450).
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, 1_000 + 455)]);
    assert_eq!(since(&r, &base, 1), (55, false));
}

/// The same work held under a member that ran `exec sudo -u me cmd` (300: its row kept, unread
/// as root's), whose sudo's monitor (400) reaped the member: when the monitor exits while the
/// sudo lives on, the work is held again at the sudo, as it would be at a process with no row,
/// so the shell's own job meanwhile counts in full (1,000, not 600), and the sudo's exit leaves
/// only its own 5 ms and the member's 50 ms tail (55, not 455). NEGATIVE CONTROL (by hand): held
/// again only at a parent with no row, it goes uncertain to the shell instead, is taken out of
/// the shell's job and then dropped, and the member's work counts twice at the sudo's exit.
#[test]
fn work_held_under_a_member_that_ran_sudo_waits_for_the_sudo() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    let sudo = p(300, SHELL, 0, 0).root();
    let monitor = p(400, 300, 0, 0).root();
    assert_eq!(
        r.step(&sweep(0, &[sudo, monitor, p(410, 400, 400, 0)])),
        400
    );
    // 410 ran 50 ms more and the monitor reaped it; the monitor lives on.
    r.step(&sweep(0, &[sudo, monitor]));
    assert_eq!(missed(&r), 3, "its tail is held");
    // The monitor exits; the sudo reaps it and lives on.
    let base = r.ledger.totals();
    r.step(&sweep(0, &[sudo]));
    assert_eq!(since(&r, &base, 1), (0, false));
    // The shell's own 1,000 ms job.
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
    let base = r.ledger.totals();
    r.step(&[&shell(1_000)[..], &[sudo]].concat());
    assert_eq!(since(&r, &base, 1), (1_000, false));
    // The sudo exits; the shell reaps it (its 5 ms, and 410's 450).
    let base = r.ledger.totals();
    r.step(&shell(1_000 + 455));
    assert_eq!(since(&r, &base, 1), (55, false));
    assert_eq!(owed(&r, SHELL), Some((0, 0)));
}

/// A delegated job's figure reads its members' own CPU from a window's start to its end, so a
/// member it holds that a step first reads, or that is gone, or whose reaped time grew past what
/// it absorbs (children no reading counted), puts work of the tab's in no figure: each such step
/// is marked. A step in which none of that happens is not.
#[test]
fn a_jobs_member_new_gone_or_reaping_unseen_work_marks_the_step() {
    let mut r = Run::new();
    r.jobs = vec![300, 310];
    let job = |child| p(300, SHELL, 0, child);
    r.step(&sweep(0, &[job(0)]));
    r.step(&sweep(0, &[job(0)]));
    assert_eq!(missed(&r), 0);
    r.step(&sweep(0, &[job(0), p(310, 300, 40, 0)]));
    assert_eq!(missed(&r), 3, "a member first read");
    r.step(&sweep(0, &[job(0), p(310, 300, 80, 0)]));
    assert_eq!(missed(&r), 3);
    // 310 exits; 300 reaps it.
    r.step(&sweep(0, &[job(80)]));
    assert_eq!(missed(&r), 5, "a member gone");
    r.step(&sweep(0, &[job(80)]));
    assert_eq!(missed(&r), 5);
    r.step(&sweep(0, &[job(500)]));
    assert_eq!(missed(&r), 7, "unseen children reaped");
    assert_eq!(
        r.tab(),
        0,
        "the job's work counts in its figure, never here"
    );
}
