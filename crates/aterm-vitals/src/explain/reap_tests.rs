// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reap credit's ledger, step by step.

use super::{ReapFault, ReapLedger, Scope, StepIn};
use crate::explain::fixtures::tab;
use crate::model::{NativeCpuCut, NativeCpuWindow, ProcSample, ProcState, TabRoot, attribute};

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
    /// CPU alone is unread while its generation remains listed.
    cpu_unread: bool,
    /// Whether the host read it no birth (gone before it could).
    unborn: bool,
    /// Its session; 0 for unread.
    session: u32,
    /// Whether the kernel reaps its children for it (it ignores `SIGCHLD`), as the sweep reads
    /// it: of every process, at every reading, whatever it has reaped.
    nocld: bool,
    read_at: Option<u64>,
    other_user_readable: bool,
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
        cpu_unread: false,
        unborn: false,
        session: 0,
        nocld: false,
        read_at: None,
        other_user_readable: false,
    }
}

impl P {
    fn other_user(self) -> Self {
        Self {
            other_user_readable: true,
            ..self
        }
    }
    fn read_at(self, ms: u64) -> Self {
        Self {
            read_at: Some(ms * MS),
            ..self
        }
    }
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
    native_roots: Vec<crate::delegated::ProcessKey>,
    native_terminals: Vec<crate::model::NativeTerminal>,
    native_incomplete: bool,
    cpu_cut_boot: Option<u64>,
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
            native_roots: Vec::new(),
            native_terminals: Vec::new(),
            native_incomplete: false,
            cpu_cut_boot: None,
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
                cpu_ns: (!q.root && !q.unread && !q.cpu_unread).then_some(q.cpu * MS),
                child_cpu_ns: (!q.root && !q.unread).then_some(q.child * MS),
                uid: Some(if q.root || q.other_user_readable {
                    0
                } else {
                    ME
                }),
                sid: q.session,
                reaps_unwaited: Some(q.nocld),
                at_ns: q.read_at,
                state: if q.zombie {
                    ProcState::Zombie
                } else {
                    ProcState::Run
                },
                ..ProcSample::default()
            })
            .collect();
        let mut members = attribute(&procs, ATERM, &self.tabs);
        for (p, member) in procs.iter().zip(&mut members) {
            if self
                .native_roots
                .iter()
                .any(|key| key.pid == p.pid && Some(key.start) == p.start)
            {
                *member = crate::model::Membership {
                    aterm: true,
                    shared: false,
                    tab: Some(0),
                };
            }
        }
        for _ in 0..procs.len() {
            let mut changed = false;
            for (k, process) in procs.iter().enumerate() {
                if members[k].tab.is_none()
                    && let Some(parent) = procs.iter().position(|p| p.pid == process.ppid)
                    && members[parent].tab.is_some()
                {
                    members[k] = members[parent];
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let delegated: Vec<bool> = procs.iter().map(|q| self.jobs.contains(&q.pid)).collect();
        self.prepare_terminals(at_ns);
        self.at_ns = at_ns;
        self.ledger.step_with_terminals(
            &StepIn {
                at_ns,
                at_unix_ms: unix_ms(at_ns),
                cpu_cut: NativeCpuCut {
                    continuous_ns: Some(at_ns),
                    boot: self.cpu_cut_boot,
                },
                procs: &procs,
                members: &members,
                delegated: &delegated,
                tabs: &self.tabs,
                self_uid: ME,
                truncated,
            },
            &self.native_terminals,
            self.native_incomplete,
        );
    }

    /// Give synthetic native fixtures explicit first-delivery timing and birth evidence.
    /// Tests of old or unpositioned receipts set their timing directly instead.
    fn prepare_terminals(&mut self, at_ns: u64) {
        for record in &mut self.native_terminals {
            if record.timing != NativeCpuWindow::default() {
                continue;
            }
            record.timing = NativeCpuWindow {
                previous: NativeCpuCut {
                    continuous_ns: Some(self.at_ns),
                    boot: None,
                },
                current: NativeCpuCut {
                    continuous_ns: Some(at_ns),
                    boot: None,
                },
                exited_ns: Some(self.at_ns + (at_ns - self.at_ns) / 2),
            };
            if record.process.birth_ms.is_none() {
                record.process.birth_ms = Some(
                    if record
                        .process
                        .start
                        .zip(self.ledger.newest)
                        .is_some_and(|(start, before)| start > before)
                    {
                        unix_ms(self.at_ns) + 500
                    } else {
                        0
                    },
                );
            }
        }
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

fn terminal(
    seq: u64,
    pid: u32,
    start: u64,
    cpu_ms: u64,
    child_ms: u64,
) -> crate::model::NativeTerminal {
    crate::model::NativeTerminal {
        timing: NativeCpuWindow::default(),
        seq,
        host: crate::delegated::ProcessKey {
            pid: ATERM,
            start: u64::from(ATERM) * 10,
        },
        process: ProcSample {
            pid,
            start: Some(start),
            ppid: ATERM,
            cpu_ns: Some(cpu_ms * MS),
            child_cpu_ns: Some(child_ms * MS),
            uid: Some(ME),
            state: ProcState::Zombie,
            reaps_unwaited: Some(false),
            ..ProcSample::default()
        },
        wait_cpu_ns: Some((cpu_ms + child_ms) * MS),
        tab: Some(1),
    }
}

#[test]
fn original_wait_finals_charge_short_siblings_once_and_leave_unrelated_reaps_own() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_terminals = vec![
        terminal(1, 300, 3000, 40, 160),
        terminal(2, 310, 3100, 10, 90),
    ];
    let now = [p(ATERM, 1, 0, 300), p(SHELL, ATERM, 0, 0)];
    r.step(&now);
    assert_eq!((r.tab(), r.all()), (300, 300));
    r.step(&now);
    assert_eq!(
        (r.tab(), r.all()),
        (300, 300),
        "replaying the same terminal cursor adds nothing"
    );
    r.native_terminals.clear();
    r.step(&[p(ATERM, 1, 0, 400), p(SHELL, ATERM, 0, 0)]);
    assert_eq!(
        (r.tab(), r.all()),
        (300, 400),
        "the daemon's other child remains its own work"
    );
}

#[test]
fn terminal_transport_custody_survives_truncated_and_missing_process_sweeps() {
    for truncated in [false, true] {
        let mut r = Run::new();
        r.step(&sweep(0, &[]));
        let base = r.ledger.totals();
        r.native_terminals = vec![terminal(1, 300, 3000, 40, 60)];
        r.prepare_terminals(r.at_ns + 1000 * MS);
        let record = r.native_terminals.pop().unwrap();
        let now = [p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)];
        if truncated {
            r.native_terminals = vec![record];
            r.step_cut(&now, true);
            r.native_terminals.clear();
        } else {
            // Machine-only reads accept source records before WhyState's ordinary
            // process/identity guard returns.
            r.ledger.ingest_terminals(&[record], false);
        }
        r.step(&now);
        assert_eq!((r.tab(), r.all()), (100, 100));
        assert_eq!(
            r.ledger.totals().reported_since(&base, 1).0,
            40 * MS,
            "a deferred whole sweep uses its actual broader adoption interval"
        );
        assert!(r.ledger.pending_terminals.is_empty());
    }
}

#[test]
fn delayed_native_receipts_never_recharge_an_old_wait_or_create_new_pending_debt() {
    for parent_already_read in [false, true] {
        let mut r = Run::new();
        r.step(&sweep(0, &[]));
        r.native_incomplete = true;
        let before_receipt = if parent_already_read { 100 } else { 0 };
        r.step(&[p(ATERM, 1, 0, before_receipt), p(SHELL, ATERM, 0, 0)]);
        let base = r.ledger.totals();
        let mut final_read = terminal(1, 300, 3000, 100, 0);
        final_read.process.birth_ms = Some(unix_ms(1200 * MS));
        final_read.timing = NativeCpuWindow {
            previous: NativeCpuCut {
                continuous_ns: Some(2000 * MS),
                boot: None,
            },
            current: NativeCpuCut {
                continuous_ns: Some(3000 * MS),
                boot: None,
            },
            exited_ns: Some(1500 * MS),
        };
        r.native_terminals = vec![final_read.clone()];
        r.native_incomplete = false;
        r.step(&[p(ATERM, 1, 0, before_receipt), p(SHELL, ATERM, 0, 0)]);
        let after = r.ledger.totals();
        assert_eq!(
            after.all, base.all,
            "the receipt cannot add old raw CPU a second time"
        );
        assert_eq!(after.reported_since(&base, 1), (0, true));
        assert!(after.since(&base, 1).1);
        assert_eq!(r.ledger.row(ATERM).unwrap().pending_ns, 0);
        assert_eq!(
            r.native_terminals[0], final_read,
            "source counters and timing are retained"
        );
        r.native_terminals.clear();
        let later = if parent_already_read { 170 } else { 100 };
        r.step(&[p(ATERM, 1, 0, later), p(SHELL, ATERM, 0, 0)]);
        assert_eq!(
            r.all(),
            later,
            "ordinary parent growth remains visible once"
        );
        assert_eq!(r.tab(), 0, "late receipts do not invent current tab rates");
    }
}

#[test]
fn a_fresh_exit_does_not_place_an_unobserved_old_lifetime_in_this_window() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_incomplete = true;
    r.step(&sweep(0, &[]));
    let base = r.ledger.totals();
    let mut final_read = terminal(1, 300, 3000, 100, 0);
    final_read.process.birth_ms = Some(0);
    final_read.timing = NativeCpuWindow {
        previous: NativeCpuCut {
            continuous_ns: Some(2000 * MS),
            boot: None,
        },
        current: NativeCpuCut {
            continuous_ns: Some(3000 * MS),
            boot: None,
        },
        exited_ns: Some(2500 * MS),
    };
    r.native_terminals = vec![final_read];
    r.native_incomplete = false;
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (0, 100));
    assert_eq!(r.ledger.totals().reported_since(&base, 1), (0, true));
}

#[test]
fn an_unpositioned_native_receipt_is_consumed_as_diagnostic_evidence_only() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let mut final_read = terminal(1, 300, 3000, 100, 0);
    final_read.timing.current.continuous_ns = Some(2000 * MS);
    r.native_terminals = vec![final_read];
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (0, 100));
    assert_eq!(r.ledger.totals().reported_since(&base, 1), (0, true));
    assert!(r.ledger.pending_terminals.is_empty());
    assert_eq!(
        r.ledger.native_sequences.get(&r.native_terminals[0].host),
        Some(&1)
    );
    r.native_terminals[0].timing.exited_ns = Some(2500 * MS);
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!(
        (r.tab(), r.all()),
        (0, 100),
        "a consumed diagnostic cannot later be retimed into work"
    );
}

#[test]
fn native_birth_uses_the_kernel_clock_and_requires_a_strict_new_birth() {
    for start in [2500, 3000] {
        let mut r = Run::new();
        r.cpu_cut_boot = Some(2500);
        r.step(&sweep(0, &[]));
        let base = r.ledger.totals();
        let mut final_read = terminal(1, 300, start, 100, 0);
        // A coarse wall clock cannot override readable, incompatible boot-clock proof.
        final_read.process.birth_ms = Some(unix_ms(1500 * MS));
        final_read.timing = NativeCpuWindow {
            exited_ns: Some(1500 * MS),
            ..NativeCpuWindow::default()
        };
        r.cpu_cut_boot = Some(3500);
        r.native_terminals = vec![final_read];
        r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
        let proved = if start == 3000 { 100 } else { 0 };
        assert_eq!(r.tab(), proved);
        assert_eq!(r.all(), 100);
        assert_eq!(r.ledger.totals().reported_since(&base, 1).0, proved * MS);
    }
}

#[test]
fn an_own_cpu_counter_read_after_an_unread_step_starts_a_new_reported_baseline() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 10, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 30, 0)]));
    r.step(&sweep(
        0,
        &[P {
            cpu_unread: true,
            ..p(300, SHELL, 60, 0)
        }],
    ));
    let gap = r.ledger.totals();
    r.step(&sweep(0, &[p(300, SHELL, 100, 0)]));
    let recovered = r.ledger.totals();
    assert_eq!(
        recovered.reported_since(&gap, 1),
        (0, true),
        "the 70ms since the last readable counter spans more than this rate window"
    );
    assert!(recovered.since(&gap, 1).1);
    r.step(&sweep(0, &[p(300, SHELL, 120, 0)]));
    assert_eq!(
        r.ledger.totals().reported_since(&recovered, 1),
        (20 * MS, false)
    );
}

#[test]
fn a_decreasing_own_counter_preserves_its_maximum_until_a_new_baseline_is_read() {
    let mut r = Run::new();
    r.step(&sweep(0, &[p(300, SHELL, 0, 0)]));
    r.step(&sweep(0, &[p(300, SHELL, 100, 0)]));
    let before = r.ledger.totals();
    r.step(&sweep(0, &[p(300, SHELL, 50, 0)]));
    let rollback = r.ledger.totals();
    assert_eq!(rollback.reported_since(&before, 1), (0, true));
    assert!(rollback.since(&before, 1).1);
    r.step(&sweep(0, &[p(300, SHELL, 110, 0)]));
    let recovered = r.ledger.totals();
    assert_eq!(
        recovered.reported_since(&rollback, 1),
        (0, true),
        "110 is a new baseline, not 60ms of new work after the invalid 50ms counter"
    );
    assert!(recovered.since(&rollback, 1).1);
    r.step(&sweep(0, &[p(300, SHELL, 120, 0)]));
    let after = r.ledger.totals();
    assert_eq!(after.reported_since(&recovered, 1), (10 * MS, false));
    assert_eq!(
        after.all_reported,
        110 * MS,
        "previously proved lifetime CPU is never counted twice"
    );
}

#[test]
fn final_child_counters_absorb_descendants_already_observed_in_the_tab() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_roots.push(crate::delegated::ProcessKey {
        pid: 300,
        start: 3000,
    });
    r.step(&sweep(0, &[p(300, ATERM, 10, 0), p(310, 300, 100, 0)]));
    assert_eq!((r.tab(), r.all()), (110, 110));
    r.native_terminals = vec![terminal(1, 300, 3000, 20, 150)];
    r.step(&[p(ATERM, 1, 0, 170), p(SHELL, ATERM, 0, 0)]);
    assert_eq!(
        (r.tab(), r.all()),
        (170, 170),
        "the final 150 includes the already credited descendant's 100"
    );
}

#[test]
fn a_native_final_does_not_double_a_descendant_still_listed_from_an_earlier_read() {
    for aggregate in [false, true] {
        let mut r = Run::new();
        r.step(&sweep(0, &[]));
        let base = r.ledger.totals();
        r.native_roots.push(crate::delegated::ProcessKey {
            pid: 300,
            start: 3000,
        });
        r.step(&sweep(0, &[p(300, ATERM, 0, 0), p(310, 300, 100, 0)]));
        let mut final_read = terminal(1, 300, 3000, 0, 160);
        final_read.process.at_ns = Some(3000 * MS);
        if aggregate {
            final_read.process.cpu_ns = None;
            final_read.process.child_cpu_ns = None;
        }
        r.native_terminals = vec![final_read];
        r.step(&[
            p(ATERM, 1, 0, 160),
            p(SHELL, ATERM, 0, 0),
            p(310, 300, 100, 0).read_at(2500),
        ]);
        assert_eq!(
            (r.tab(), r.all()),
            (160, 160),
            "the final child160 may already include the listed descendant100; aggregate={aggregate}"
        );
        assert_eq!(r.ledger.totals().reported_since(&base, 1).0 / MS, 100);
        assert!(r.ledger.totals().since(&base, 1).1);
        assert!(r.ledger.totals().reported_since(&base, 1).1);
    }
}

#[test]
fn an_escaped_or_post_final_descendant_remains_independent_of_the_native_final() {
    for escaped in [false, true] {
        let mut r = Run::new();
        r.step(&sweep(0, &[]));
        r.native_roots.push(crate::delegated::ProcessKey {
            pid: 300,
            start: 3000,
        });
        r.step(&sweep(0, &[p(300, ATERM, 0, 0), p(310, 300, 100, 0)]));
        let mut final_read = terminal(1, 300, 3000, 0, 160);
        final_read.process.at_ns = Some(3000 * MS);
        r.native_terminals = vec![final_read];
        let child = if escaped {
            p(310, 1, 100, 0).read_at(2500)
        } else {
            p(310, 300, 100, 0).read_at(3500)
        };
        r.step(&[p(ATERM, 1, 0, 160), p(SHELL, ATERM, 0, 0), child]);
        assert_eq!(
            (r.tab(), r.all()),
            (260, 260),
            "a child already escaped or read after the parent's final cannot be in that parent's final waited time"
        );
    }
}

#[test]
fn an_ordinary_later_parent_read_deduplicates_the_older_child_snapshot() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.step(&sweep(0, &[p(300, SHELL, 0, 0), p(310, 300, 100, 0)]));
    r.step(&sweep(
        0,
        &[
            p(300, SHELL, 0, 160).read_at(3000),
            p(310, 300, 100, 0).read_at(2500),
        ],
    ));
    assert_eq!((r.tab(), r.all()), (160, 160));
    r.step(&sweep(0, &[p(300, SHELL, 0, 160)]));
    assert_eq!(
        (r.tab(), r.all()),
        (160, 160),
        "the later disappearance does not make the parent absorb that lifetime twice"
    );
}

#[test]
fn own_reported_keeps_panes_separate_and_retains_a_closed_panes_logical_remainder() {
    let mut ledger = ReapLedger::default();
    let mut tabs = vec![tab(1, SHELL), tab(2, 201)];
    tabs[1].tab = 1;
    let input = |cpu_a, cpu_b| {
        vec![
            ProcSample {
                pid: ATERM,
                start: Some(1000),
                ppid: 1,
                cpu_ns: Some(0),
                child_cpu_ns: Some(0),
                uid: Some(ME),
                ..ProcSample::default()
            },
            ProcSample {
                pid: SHELL,
                start: Some(2000),
                ppid: 1,
                cpu_ns: Some(cpu_a * MS),
                child_cpu_ns: Some(0),
                uid: Some(ME),
                ..ProcSample::default()
            },
            ProcSample {
                pid: 201,
                start: Some(2010),
                ppid: ATERM,
                cpu_ns: Some(cpu_b * MS),
                child_cpu_ns: Some(0),
                uid: Some(ME),
                ..ProcSample::default()
            },
        ]
    };
    let step = |ledger: &mut ReapLedger, at, procs: &[ProcSample], tabs: &[TabRoot]| {
        let members = attribute(procs, ATERM, tabs);
        ledger.step(&StepIn {
            at_ns: at * MS,
            at_unix_ms: unix_ms(at * MS),
            cpu_cut: NativeCpuCut::default(),
            procs,
            members: &members,
            delegated: &[false; 3],
            tabs,
            self_uid: ME,
            truncated: false,
        });
    };
    step(&mut ledger, 1000, &input(0, 0), &tabs);
    let base = ledger.totals();
    step(&mut ledger, 2000, &input(500, 250), &tabs);
    let now = ledger.totals();
    assert_eq!(now.pane_reported_since(&base, 1), 500 * MS);
    assert_eq!(now.pane_reported_since(&base, 2), 250 * MS);
    assert_eq!(now.reported_since(&base, 1).0, 750 * MS);
    tabs.remove(0);
    tabs[0].tab = 2;
    step(&mut ledger, 3000, &input(600, 300), &tabs);
    let later = ledger.totals();
    assert_eq!(later.pane_reported_since(&now, 2), 50 * MS);
    assert_eq!(
        later.reported_since(&now, 2).0,
        150 * MS,
        "the closed pane's orphan stays in its logical tab without becoming the surviving pane's own work"
    );
}

#[test]
fn coalition_reported_reuses_only_an_observed_unchanged_exec_version() {
    let mut ledger = ReapLedger::default();
    let tabs = [tab(1, SHELL)];
    for (at, cpu, coalition, version) in [
        (1000, 0, Some(400), 1),
        (2000, 100, Some(400), 1),
        (3000, 150, None, 1),
        (4000, 200, None, 2),
        (5000, 250, None, 2),
        (6000, 300, Some(500), 2),
    ] {
        let procs = [
            ProcSample {
                pid: ATERM,
                start: Some(1000),
                ppid: 1,
                cpu_ns: Some(0),
                child_cpu_ns: Some(0),
                uid: Some(ME),
                ..ProcSample::default()
            },
            ProcSample {
                pid: SHELL,
                start: Some(2000),
                ppid: ATERM,
                cpu_ns: Some(cpu * MS),
                child_cpu_ns: Some(0),
                uid: Some(ME),
                coalition,
                version: Some(version),
                ..ProcSample::default()
            },
        ];
        let members = attribute(&procs, ATERM, &tabs);
        ledger.step(&StepIn {
            at_ns: at * MS,
            at_unix_ms: unix_ms(at * MS),
            cpu_cut: NativeCpuCut::default(),
            procs: &procs,
            members: &members,
            delegated: &[false; 2],
            tabs: &tabs,
            self_uid: ME,
            truncated: false,
        });
    }
    let totals = ledger.totals();
    assert_eq!(totals.coalition_reported.get(&400), Some(&(150 * MS)));
    assert_eq!(totals.coalition_reported.get(&500), Some(&(50 * MS)));
    assert_eq!(totals.unmapped_reported, 100 * MS);
    assert_eq!(totals.all_reported, 300 * MS);
}

#[test]
fn own_reported_does_not_keep_a_foreign_process_after_delegated_custody_ends() {
    use crate::model::Membership;
    let mut ledger = ReapLedger::default();
    let tabs = [tab(1, SHELL)];
    for (at, owned, cpu) in [(1000, true, 0), (2000, true, 100), (3000, false, 200)] {
        let processes = [
            ProcSample {
                pid: ATERM,
                start: Some(1000),
                ppid: 1,
                uid: Some(ME),
                cpu_ns: Some(0),
                child_cpu_ns: Some(0),
                ..ProcSample::default()
            },
            ProcSample {
                pid: 400,
                start: Some(4000),
                ppid: 1,
                uid: Some(ME),
                cpu_ns: Some(cpu * MS),
                child_cpu_ns: Some(0),
                ..ProcSample::default()
            },
        ];
        let members = [
            Membership {
                aterm: true,
                ..Membership::default()
            },
            Membership {
                aterm: owned,
                tab: owned.then_some(0),
                ..Membership::default()
            },
        ];
        ledger.step(&StepIn {
            at_ns: at * MS,
            at_unix_ms: unix_ms(at * MS),
            cpu_cut: NativeCpuCut::default(),
            procs: &processes,
            members: &members,
            delegated: &[false, owned],
            tabs: &tabs,
            self_uid: ME,
            truncated: false,
        });
    }
    assert_eq!(ledger.totals().all_reported, 0);
    assert_eq!(ledger.totals().all, 0);
}

#[test]
fn readable_owned_cpu_keeps_its_real_other_uid_and_unrelated_processes_stay_out() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let base = r.ledger.totals();
    r.step(&sweep(
        0,
        &[
            p(300, SHELL, 100, 0).other_user(),
            p(400, 1, 300, 0).other_user(),
        ],
    ));
    assert_eq!((r.tab(), r.all()), (100, 100));
    assert_eq!(
        r.ledger.totals().reported_since(&base, 1),
        (100 * MS, false)
    );
    assert!(r.ledger.row(400).is_none());
}

#[test]
fn own_cpu_reported_growth_never_inherits_a_hidden_auto_reaping_child_charge() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 0), p(SHELL, ATERM, 10, 200)]);
    let total = r.ledger.totals();
    assert_eq!(total.since(&base, 1).0 / MS, 210);
    assert_eq!(total.reported_since(&base, 1), (10 * MS, true));
    assert_eq!(total.own_reported_since(&base), (0, false));
    r.tabs.clear();
    let before_close = r.ledger.totals();
    r.step(&[p(ATERM, 1, 5, 0), p(SHELL, ATERM, 20, 200)]);
    assert_eq!(
        r.ledger.totals().own_reported_since(&before_close).0 / MS,
        15,
        "folded historical tab observations does not become current own-scope work"
    );
}

#[test]
fn terminal_generations_and_a_live_reused_pid_each_keep_their_own_cpu() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_terminals = vec![terminal(1, 300, 3000, 10, 0), terminal(2, 300, 4000, 20, 0)];
    r.step(&[
        p(ATERM, 1, 0, 30),
        p(SHELL, ATERM, 0, 0),
        p(300, SHELL, 7, 0).started(5000),
        p(600, SHELL, 5, 0),
    ]);
    assert_eq!((r.tab(), r.all()), (42, 42));
    assert_eq!(r.ledger.row(300).map(|row| row.start), Some(5000));
}

#[test]
fn a_missing_final_split_stays_incomplete_and_never_becomes_invented_own_cpu() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let mut missing = terminal(1, 300, 3000, 0, 0);
    missing.process.child_cpu_ns = None;
    missing.wait_cpu_ns = None;
    r.native_terminals = vec![missing];
    let base = r.ledger.totals();
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (0, 100));
    assert!(r.ledger.totals().since(&base, 1).1);
}

#[test]
fn a_known_owning_wait_aggregate_is_credited_without_inventing_a_final_split() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let mut aggregate = terminal(1, 300, 3000, 40, 60);
    aggregate.process.cpu_ns = None;
    aggregate.process.child_cpu_ns = None;
    r.native_terminals = vec![aggregate];
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (100, 100));
    r.step(&[p(ATERM, 1, 0, 100), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (100, 100));
}

#[test]
fn an_aggregate_final_deduplicates_observed_descendants_and_the_parent_reap() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_roots.push(crate::delegated::ProcessKey {
        pid: 300,
        start: 3000,
    });
    r.step(&sweep(0, &[p(300, ATERM, 10, 0), p(310, 300, 100, 0)]));
    let mut aggregate = terminal(1, 300, 3000, 20, 150);
    aggregate.process.cpu_ns = None;
    aggregate.process.child_cpu_ns = None;
    r.native_terminals = vec![aggregate];
    r.step(&[p(ATERM, 1, 0, 170), p(SHELL, ATERM, 0, 0)]);
    assert_eq!((r.tab(), r.all()), (170, 170));
}

#[test]
fn an_auto_reaping_original_never_turns_doubled_child_time_into_exact_work() {
    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    r.native_roots.push(crate::delegated::ProcessKey {
        pid: 300,
        start: 3000,
    });
    r.step(&sweep(
        0,
        &[p(300, ATERM, 10, 0).nocld(), p(310, 300, 100, 0)],
    ));
    let base = r.ledger.totals();
    let mut final_read = terminal(1, 300, 3000, 20, 300);
    final_read.process.reaps_unwaited = Some(true);
    r.native_terminals = vec![final_read];
    r.step(&[p(ATERM, 1, 0, 320), p(SHELL, ATERM, 0, 0)]);
    assert_eq!(
        (r.tab(), r.all()),
        (120, 120),
        "known own CPU and the observed descendant count once; unread tail stays unknown"
    );
    assert!(r.ledger.totals().since(&base, 1).1);

    let mut r = Run::new();
    r.step(&sweep(0, &[]));
    let mut final_read = terminal(1, 300, 3000, 20, 300);
    final_read.process.cpu_ns = None;
    final_read.process.child_cpu_ns = None;
    final_read.process.reaps_unwaited = Some(true);
    r.native_terminals = vec![final_read];
    r.step(&[p(ATERM, 1, 0, 320), p(SHELL, ATERM, 0, 0)]);
    assert_eq!(
        (r.tab(), r.all()),
        (0, 0),
        "the known wait aggregate is kept as a charge, not invented actual CPU"
    );
    assert!(r.ledger.totals().missed > 0);
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

#[test]
fn an_own_scope_tail_marks_global_coverage_without_marking_an_unrelated_tab() {
    let run = |zombie| {
        let mut r = Run::new();
        r.step(&sweep(0, &[p(300, ATERM, 0, 0)]));
        let process = p(300, 1, 100, 0);
        r.step(&sweep(
            0,
            &[if zombie { process.zombie() } else { process }],
        ));
        let base = r.ledger.totals();
        r.step(&sweep(0, &[]));
        let now = r.ledger.totals();
        (now.missed > base.step, now.since(&base, 1).1)
    };
    assert_eq!(run(false), (true, false));
    assert_eq!(
        run(true),
        (false, false),
        "a zombie's final own CPU reading loses no tail"
    );
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

/// A kernel start newer than the listed processes is insufficient to bound an unseen lifetime.
/// A start after the actual previous source cut proves a current birth independently.
#[test]
fn a_process_with_no_birth_read_needs_a_source_cut_to_prove_current_cpu() {
    let run = |start, source_cut: bool| {
        let mut r = Run::new();
        r.cpu_cut_boot = source_cut.then_some(8_000);
        r.step(&sweep(0, &[p(300, SHELL, 0, 0).started(3_000)]));
        let base = r.ledger.totals();
        r.cpu_cut_boot = source_cut.then_some(10_000);
        let job = p(310, 300, 40, 0).started(start).unborn();
        r.step(&sweep(0, &[p(300, SHELL, 0, 0).started(3_000), job]));
        let (floor, ambiguous) = r.ledger.totals().reported_since(&base, 1);
        (since(&r, &base, 1), (floor / MS, ambiguous))
    };
    assert_eq!(
        run(9_000, false),
        ((40, true), (0, true)),
        "the reconciled counter charge stays known without a current own-counter observation"
    );
    assert_eq!(run(2_500, false), ((0, true), (0, true)));
    assert_eq!(
        run(9_000, true),
        ((40, false), (40, false)),
        "a birth after the previous source cut places the whole own counter in this window"
    );
}

/// A first counter whose birth predates the previous cut may include earlier CPU. Retain its
/// reconciled charge separately while withholding an unproved current published-growth baseline.
#[test]
fn a_process_first_seen_after_a_sweep_boundary_keeps_its_interval_uncertainty() {
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
        let (floor, ambiguous) = r.ledger.totals().reported_since(&base, 1);
        (since(&r, &base, 1), (floor / MS, ambiguous))
    };
    assert_eq!(
        run(9_000),
        ((1_039, true), (0, true)),
        "a newer listed start does not place the lifetime after the previous cut"
    );
    assert_eq!(run(2_500), ((0, true), (0, true)));
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
    assert_eq!(
        since(&r, &base, 1),
        (1_000, true),
        "the shell's charge counts once while the live sudo's own CPU stays unread"
    );
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
    assert_eq!(
        since(&r, &base, 1),
        (0, true),
        "zero charge growth does not complete the unread sudo's own-counter coverage"
    );
    // The shell's own 1,000 ms job.
    let shell = |child| [p(ATERM, 1, 0, 0), p(SHELL, ATERM, 0, child)];
    let base = r.ledger.totals();
    r.step(&[&shell(1_000)[..], &[sudo]].concat());
    assert_eq!(
        since(&r, &base, 1),
        (1_000, true),
        "the held child's charge stays counted once, independently of unread own CPU"
    );
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
