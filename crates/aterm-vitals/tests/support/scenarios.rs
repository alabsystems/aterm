// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The design's stuck scenarios (`docs/DESIGN-system-awareness-2026-10-16.md`
//! §18 item 8) as scripted fixtures: each row of the expectation table, in
//! each shape it names (`pty`: the person's foreground job in a tab;
//! `setsid`: an agent's detached call), with the readings gap-1 measured
//! (curl's 5 syscalls and 1 wakeup a second; the 1 ms poller's 635 a second
//! at 2.6 ms/s of CPU; the spinner's CR-only output; the clang loop's
//! 900 ms/s of child CPU; the silent writer's 84 KB/s; M7's logical-write
//! noise). Thresholds sit at minutes, so a run repeats its steady tick with
//! the clock advanced, as the design's replay does (gap-1's quiet rows are
//! exactly constant). Expected results are the table's, at the 60 s prior and
//! stretch 1 unless the row says otherwise: "the table is the conformance
//! test's expectation list: a row that changes is a design change".
//!
//! The `D2` rows are `docs/DESIGN-hung-background-call-2026-10-20.md`'s
//! no-progress word: an agent's call asleep with no CPU (I4's deadlocked
//! test, held 26 h) reads quiet and `no_progress=1` from φ 8, and the calls
//! that print, burn CPU, use a little CPU or had a reading refused, and the
//! person's own `sleep`, never do.

#![allow(dead_code, reason = "each test binary mounting this uses part of it")]

use std::net::SocketAddr;

use aterm_vitals::jobs::{
    Block, Class, Consumer, Fault, Fd, FdKind, GateWord, JobKind, JobRow, JobsView, Knote,
    KnoteFilter, NamedWait, PHI_STUCK_MILLI, ProcState, TcpState, TierB, Verdict, WaitName,
};

use super::world::{OutFile, Proc, World, hint};

/// The tick cadence while a job can accrue suspicion (§11.3).
pub const TICK_MS: u64 = 2_000;
/// The agent in front of tab 1 for every `setsid` shape.
pub const AGENT: u32 = 300;
/// The call's `zsh -c` leader.
pub const CALL: u32 = 400;
/// The call's command.
pub const CHILD: u32 = 401;
/// The `pty` shape's foreground process (and group).
pub const FG: u32 = 1000;

/// One expectation, at a time relative to the scenario's start.
#[derive(Clone, Copy, Debug)]
pub enum Expect {
    /// The watched job's verdict at this time.
    At(u64, Verdict),
    /// The watched job's class at this time.
    ClassAt(u64, Class),
    /// The watched job's shown wait at this time.
    WaitAt(u64, WaitName),
    /// φ × 1000 at this time, within 5.
    PhiAt(u64, u64),
    /// The first tick with this verdict falls in `[t, t + TICK_MS)`.
    FirstAt(Verdict, u64),
    /// Never this verdict.
    Never(Verdict),
    /// From this time on, φ × 1000 stays at most this.
    PhiAtMost(u64, u64),
    /// No row for the watched job at this time.
    Gone(u64),
    /// No job at all at this time.
    NoJobs(u64),
    /// Another job's verdict at this time.
    OtherAt(u64, &'static str, Verdict),
    /// A zombie finding of this many children of this parent at this time.
    Zombies(u64, u32, u32),
    /// Never two rows with the watched id at one tick.
    OneRow,
    /// The watched job reads `no_progress` at every tick from this time on,
    /// and at none before.
    NoProgressFrom(u64),
    /// The watched job never reads `no_progress`.
    NoProgressNever,
}

/// One scripted scenario.
pub struct Scenario {
    /// `<scenario>.<shape>`, as the design names its traces.
    pub name: &'static str,
    /// The design table's row.
    pub row: &'static str,
    /// `Contention::factor_milli`.
    pub stretch_milli: u32,
    /// Lay the processes out at the start.
    pub setup: fn(&mut World),
    /// Called after the clock advanced, before each tick's reading.
    pub each: fn(&mut World, u64),
    /// The job the expectations name.
    pub id: &'static str,
    /// How long it runs, ms.
    pub until_ms: u64,
    pub expect: &'static [Expect],
    /// Whether the StuckVerdict model covers it (the unread-input gate is
    /// its own machine, `InputUnreadGate`, so gate-decided runs are not).
    pub model: bool,
}

/// What a run saw at each tick.
pub struct Trace {
    /// `(time since start, the watched job's row, every row)`.
    pub ticks: Vec<(u64, Option<JobRow>, JobsView)>,
}

impl Trace {
    pub fn at(&self, t: u64) -> Option<&(u64, Option<JobRow>, JobsView)> {
        self.ticks.iter().find(|(rel, _, _)| *rel == t)
    }
}

/// Run `s`, with `fault` injected into the engine.
pub fn run(s: &Scenario, fault: Option<Fault>) -> Trace {
    let mut w = World::new();
    if let Some(f) = fault {
        w = w.with_fault(f);
    }
    w.machine.stretch_milli = s.stretch_milli;
    (s.setup)(&mut w);
    let mut ticks = Vec::new();
    let mut rel = 0;
    loop {
        if rel > 0 {
            w.advance(TICK_MS);
        }
        (s.each)(&mut w, rel);
        let view = w.observe();
        let row = view.row(s.id).cloned();
        ticks.push((rel, row, view));
        if rel >= s.until_ms {
            break;
        }
        rel += TICK_MS;
    }
    Trace { ticks }
}

/// Every expectation `s` lists, held to `t`; the failures, one line each.
pub fn check(s: &Scenario, t: &Trace) -> Vec<String> {
    let mut bad = Vec::new();
    let verdict_at = |rel: u64| {
        t.at(rel)
            .and_then(|(_, r, _)| r.as_ref())
            .map(|r| r.verdict)
    };
    for e in s.expect {
        let ok = match *e {
            Expect::At(rel, v) => verdict_at(rel) == Some(v),
            Expect::ClassAt(rel, c) => t
                .at(rel)
                .and_then(|(_, r, _)| r.as_ref())
                .is_some_and(|r| r.class == c),
            Expect::WaitAt(rel, n) => t
                .at(rel)
                .and_then(|(_, r, _)| r.as_ref())
                .and_then(|r| r.wait.as_ref())
                .is_some_and(|w| w.name == n),
            Expect::PhiAt(rel, phi) => t
                .at(rel)
                .and_then(|(_, r, _)| r.as_ref())
                .and_then(|r| r.phi_milli)
                .is_some_and(|p| p.abs_diff(phi) <= 5),
            Expect::FirstAt(v, at) => t
                .ticks
                .iter()
                .find(|(_, r, _)| r.as_ref().is_some_and(|r| r.verdict == v))
                .is_some_and(|(rel, _, _)| *rel >= at && *rel < at + TICK_MS),
            Expect::Never(v) => t
                .ticks
                .iter()
                .all(|(_, r, _)| r.as_ref().is_none_or(|r| r.verdict != v)),
            Expect::PhiAtMost(from, max) => {
                t.ticks
                    .iter()
                    .filter(|(rel, _, _)| *rel >= from)
                    .all(|(_, r, _)| {
                        r.as_ref()
                            .and_then(|r| r.phi_milli)
                            .is_none_or(|p| p <= max)
                    })
            }
            Expect::Gone(rel) => t.at(rel).is_some_and(|(_, r, _)| r.is_none()),
            Expect::NoJobs(rel) => t.at(rel).is_some_and(|(_, _, v)| v.rows.is_empty()),
            Expect::OtherAt(rel, id, v) => t
                .at(rel)
                .and_then(|(_, _, view)| view.row(id))
                .is_some_and(|r| r.verdict == v),
            Expect::Zombies(rel, parent, count) => t.at(rel).is_some_and(|(_, _, v)| {
                v.findings
                    .zombies
                    .iter()
                    .any(|z| z.ppid == parent && z.count == count)
            }),
            Expect::OneRow => t
                .ticks
                .iter()
                .all(|(_, _, v)| v.rows.iter().filter(|r| r.id == s.id).count() <= 1),
            Expect::NoProgressFrom(from) => t
                .ticks
                .iter()
                .all(|(rel, r, _)| r.as_ref().is_some_and(|r| r.no_progress == (*rel >= from))),
            Expect::NoProgressNever => t
                .ticks
                .iter()
                .all(|(_, r, _)| r.as_ref().is_none_or(|r| !r.no_progress)),
        };
        if !ok {
            let seen = match *e {
                Expect::FirstAt(v, _) => t
                    .ticks
                    .iter()
                    .find(|(_, r, _)| r.as_ref().is_some_and(|r| r.verdict == v))
                    .map_or_else(
                        || "never".to_owned(),
                        |(rel, _, _)| format!("first at {rel}"),
                    ),
                Expect::OneRow => t
                    .ticks
                    .iter()
                    .find(|(_, _, v)| v.rows.iter().filter(|r| r.id == s.id).count() > 1)
                    .map_or_else(String::new, |(rel, _, v)| {
                        let rows: Vec<String> = v
                            .rows
                            .iter()
                            .filter(|r| r.id == s.id)
                            .map(|r| r.wire())
                            .collect();
                        format!("at {rel}: {}", rows.join(" | "))
                    }),
                Expect::NoProgressFrom(from) => t
                    .ticks
                    .iter()
                    .find(|(rel, r, _)| r.as_ref().is_none_or(|r| r.no_progress != (*rel >= from)))
                    .map_or_else(String::new, |(rel, r, _)| {
                        format!(
                            "at {rel}: {}",
                            r.as_ref().map_or("no row".to_owned(), JobRow::wire)
                        )
                    }),
                Expect::NoProgressNever => t
                    .ticks
                    .iter()
                    .find_map(|(rel, r, _)| r.as_ref().filter(|r| r.no_progress).map(|r| (rel, r)))
                    .map_or_else(String::new, |(rel, r)| format!("at {rel}: {}", r.wire())),
                Expect::At(rel, _)
                | Expect::ClassAt(rel, _)
                | Expect::WaitAt(rel, _)
                | Expect::PhiAt(rel, _)
                | Expect::Gone(rel)
                | Expect::NoJobs(rel)
                | Expect::OtherAt(rel, _, _)
                | Expect::Zombies(rel, _, _) => t.at(rel).map_or_else(
                    || "no tick".to_owned(),
                    |(_, r, v)| match r {
                        Some(r) => r.wire(),
                        None => format!("no row ({} rows)", v.rows.len()),
                    },
                ),
                _ => String::new(),
            };
            bad.push(format!("{} ({}): {e:?} failed; saw {seen}", s.name, s.row));
        }
    }
    // Every row of every tick: `no_progress` is a word derived from a quiet
    // agent's call at φ ≥ 8, never a verdict of its own, and the row prints
    // it beside the verdict exactly when it holds.
    let derived = |r: &JobRow| {
        let wire = r.wire();
        if !r.no_progress {
            return !wire.contains(" no_progress=");
        }
        r.kind == JobKind::AgentCall
            && matches!(
                r.consumer,
                Consumer::AgentFg | Consumer::AgentBg | Consumer::Agent
            )
            && r.verdict == Verdict::Quiet
            && r.phi_milli.is_some_and(|p| p >= PHI_STUCK_MILLI)
            && wire.ends_with(" verdict=quiet no_progress=1")
    };
    if let Some((rel, r)) = t
        .ticks
        .iter()
        .find_map(|(rel, _, v)| v.rows.iter().find(|r| !derived(r)).map(|r| (rel, r)))
    {
        bad.push(format!(
            "{} ({}): no_progress is not derived at {rel}: {} (no_progress {})",
            s.name,
            s.row,
            r.wire(),
            r.no_progress
        ));
    }
    bad
}

// Builders

pub fn tcp_fd(fd: i32) -> Fd {
    Fd {
        fd,
        kind: FdKind::Tcp {
            state: TcpState::Established,
            remote: Some("192.0.2.10:8080".parse::<SocketAddr>().expect("an address")),
            local_port: 50_123,
            queued_read: Some(0),
        },
    }
}

pub fn follow_fds() -> Vec<Fd> {
    vec![
        Fd {
            fd: 3,
            kind: FdKind::File {
                regular: true,
                size: 4096,
                dev: 1,
                ino: 777,
                lf_tail: None,
            },
        },
        Fd {
            fd: 4,
            kind: FdKind::EventQueue {
                sleeping: Some(true),
                knotes: Some(vec![Knote {
                    ident: 3,
                    filter: KnoteFilter::Read,
                }]),
            },
        },
    ]
}

pub fn pipe_end(fd: i32, handle: u64, peer: u64, write: bool, status: u32, queued: u64) -> Fd {
    Fd {
        fd,
        kind: FdKind::Pipe {
            handle,
            peer,
            status: Some(status),
            queued: Some(queued),
            write_end: Some(write),
        },
    }
}

/// The `pty` shape: `p` in front of tab 1 under block 51.
fn pty(w: &mut World, p: Proc, command: &str) {
    w.run_fg(1, 51, command, p);
}

/// The `setsid` shape: `child` as an agent's call (consumer unknown: agent).
fn setsid(w: &mut World, child: Proc) {
    w.call(1, AGENT, CALL, child, None);
}

pub fn sleeper(pid: u32, name: &str) -> Proc {
    Proc::new(pid, 0, name)
}

pub fn curl(pid: u32) -> Proc {
    let mut p = Proc::new(pid, 0, "curl");
    p.syscalls_per_s = 5;
    p.wakeups_per_s = 1;
    p.fds.push(tcp_fd(5));
    p
}

pub fn spinner(pid: u32, name: &str) -> Proc {
    let mut p = Proc::new(pid, 0, name);
    p.state = ProcState::Running;
    p.cpu_per_s = 990_000_000;
    p
}

fn poller(pid: u32) -> Proc {
    let mut p = Proc::new(pid, 0, "poll1ms");
    p.cpu_per_s = 2_600_000;
    p.syscalls_per_s = 635;
    p
}

fn nothing(_: &mut World, _: u64) {}

/// Name the watched job's wait with Tier B `word` at 20 min + 2 s.
fn name_at_20m(w: &mut World, rel: u64, leader: u32, word: TierB, holder: Option<u32>) {
    if rel == 1_202_000 {
        w.name(
            1,
            NamedWait {
                leader,
                word,
                holder,
            },
        );
    }
}

/// The setsid shape's exit at once: no terminal, stdin `/dev/null`.
fn exit_at_once(w: &mut World, rel: u64) {
    if rel == TICK_MS {
        w.exit(CHILD, false);
        w.exit(CALL, false);
    }
}

// The table

const M1: u64 = 60_000;
const M20: u64 = 1_200_000;
/// 26 h: about I4's age when it was found (`etime 01-02:28:34`).
const H26: u64 = 93_600_000;
/// The first tick at φ ≥ 8 at the 60 s prior (18 min 25 s, read on the
/// 2 s grid).
const PHI8: u64 = 1_106_000;

/// Every scenario of the table, each shape a row of its own.
pub fn all() -> Vec<Scenario> {
    use Expect::{
        At, ClassAt, FirstAt, Gone, Never, NoJobs, NoProgressFrom, NoProgressNever, OneRow,
        OtherAt, PhiAt, PhiAtMost, WaitAt, Zombies,
    };
    use Verdict::{Busy, Exited, Fine, NeverReturns, Parked, Quiet, Stuck, WaitingYou};
    vec![
        Scenario {
            name: "idle.pty",
            row: "1 idle prompt",
            stretch_milli: 1000,
            setup: |_| {},
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[NoJobs(M1), NoJobs(M20)],
            model: true,
        },
        Scenario {
            name: "sleep.pty",
            row: "2 sleep 600",
            stretch_milli: 1000,
            setup: |w| pty(w, sleeper(FG, "sleep"), "sleep 600"),
            each: |w, rel| name_at_20m(w, rel, FG, TierB::Timer, None),
            id: "b51",
            until_ms: M20 + 10_000,
            expect: &[
                At(M1, Fine),
                PhiAt(M1, 434),
                WaitAt(M1, WaitName::Unnamed),
                FirstAt(Quiet, 416_000),
                At(M20, Quiet),
                Never(Stuck),
                At(M20 + 10_000, Fine),
                // Quiet at φ 8.67 with no CPU, but the person's own job: the
                // no-progress word is an agent's call's.
                NoProgressNever,
            ],
            model: true,
        },
        Scenario {
            name: "sleep.setsid",
            row: "2 sleep 600",
            stretch_milli: 1000,
            setup: |w| setsid(w, sleeper(CHILD, "sleep")),
            each: |w, rel| name_at_20m(w, rel, CALL, TierB::Timer, None),
            id: "a400",
            until_ms: M20 + 10_000,
            expect: &[
                At(M1, Fine),
                PhiAt(M1, 434),
                WaitAt(M1, WaitName::Unnamed),
                At(M20, Quiet),
                Never(Stuck),
                At(M20 + 10_000, Fine),
            ],
            model: true,
        },
        Scenario {
            name: "sleep_eperm.setsid",
            row: "2 sleep 600, with refused readings",
            stretch_milli: 1000,
            setup: |w| setsid(w, sleeper(CHILD, "sleep")),
            each: |w, rel| {
                // An EPERM reading at 3 min and at 10 min: unknown, never zero.
                let refused = rel == 180_000 || rel == 600_000;
                w.p(CHILD).refused = refused;
            },
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                FirstAt(Quiet, 416_000),
                At(M20, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "tailf.pty",
            row: "3 tail -f",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(FG, "tail");
                p.fds = follow_fds();
                pty(w, p, "tail -f build.log");
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Follow),
                At(M20, Fine),
                Never(Quiet),
                PhiAtMost(0, 0),
            ],
            model: true,
        },
        Scenario {
            name: "tailf.setsid",
            row: "3b tail -f, agent-fg then promoted",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(CHILD, "tail");
                p.fds = follow_fds();
                w.call(
                    1,
                    AGENT,
                    CALL,
                    p,
                    Some(hint(CALL, Consumer::AgentFg, Some("c81d0f2ma"), None)),
                );
            },
            each: |w, rel| {
                if rel == 120_000 {
                    w.hint(
                        1,
                        hint(CALL, Consumer::AgentBg, Some("c81d0f2ma"), Some(120_000)),
                    );
                }
            },
            id: "a400",
            until_ms: M20,
            expect: &[
                FirstAt(NeverReturns, 10_000),
                At(M1, NeverReturns),
                At(120_000, Fine),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "less.pty",
            row: "4 less",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "less"), "less README");
                w.tty_mode(1, false, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::TtyRaw),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "vi.pty",
            row: "5 vi -n",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "vi"), "vi -n notes");
                w.tty_mode(1, false, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[At(M1, Fine), WaitAt(M1, WaitName::TtyRaw), At(M20, Fine)],
            model: true,
        },
        Scenario {
            name: "read.pty",
            row: "6 read -r x",
            stretch_milli: 1000,
            setup: |w| {
                // The shell's own builtin: the foreground stays the shell.
                let at = w.at;
                w.tab(1).tab.block = Some(Block {
                    id: 51,
                    started_ms: at,
                    open: true,
                    command: "read -r x".to_owned(),
                    status: None,
                });
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::TtyBuiltinRead),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "less.setsid",
            row: "4-6 less / vi -n / read without a terminal: exit at once",
            stretch_milli: 1000,
            setup: |w| setsid(w, sleeper(CHILD, "less")),
            each: exit_at_once,
            id: "a400",
            until_ms: M20,
            expect: &[At(TICK_MS, Exited), At(M1, Exited), Gone(M20)],
            model: true,
        },
        Scenario {
            name: "sshkeygen.pty",
            row: "7 ssh-keygen -p",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "ssh-keygen"), "ssh-keygen -p");
                w.tty_mode(1, true, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, WaitingYou),
                WaitAt(M1, WaitName::TtyPassword),
                At(M20, WaitingYou),
                PhiAtMost(0, 0),
            ],
            model: true,
        },
        Scenario {
            name: "sshkeygen.setsid",
            row: "7 ssh-keygen -p without a terminal: exits",
            stretch_milli: 1000,
            setup: |w| setsid(w, sleeper(CHILD, "ssh-keygen")),
            each: exit_at_once,
            id: "a400",
            until_ms: M1,
            expect: &[At(M1, Exited)],
            model: true,
        },
        Scenario {
            name: "flock.pty",
            row: "8 flock wait",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "flock"), "flock /tmp/l sleep 1");
                // The holder sleeps outside the tree.
                let mut h = sleeper(9100, "sleep");
                h.ppid = 1;
                h.sid = 9100;
                w.add(h);
            },
            each: |w, rel| name_at_20m(w, rel, FG, TierB::Lock, Some(9100)),
            id: "b51",
            until_ms: M20 + 4_000,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Unnamed),
                At(M20, Quiet),
                At(M20 + 2_000, Stuck),
                At(M20 + 4_000, Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "flock.setsid",
            row: "8 flock wait",
            stretch_milli: 1000,
            setup: |w| {
                setsid(w, sleeper(CHILD, "flock"));
                let mut h = sleeper(9100, "sleep");
                h.ppid = 1;
                h.sid = 9100;
                w.add(h);
            },
            each: |w, rel| name_at_20m(w, rel, CALL, TierB::Lock, Some(9100)),
            id: "a400",
            until_ms: M20 + 4_000,
            expect: &[At(M1, Fine), At(M20, Quiet), At(M20 + 4_000, Stuck)],
            model: true,
        },
        Scenario {
            name: "cargolock.pty",
            row: "9 cargo build-dir lock",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "cargo"), "cargo build");
                holder_tab(w);
            },
            each: |w, rel| name_at_20m(w, rel, FG, TierB::Lock, Some(2100)),
            id: "b51",
            until_ms: M20 + 6_000,
            expect: &[
                At(M1, Fine),
                At(M20, Quiet),
                OtherAt(M20, "b61", Fine),
                At(M20 + 2_000, Quiet),
                At(M20 + 4_000, Fine),
            ],
            model: true,
        },
        Scenario {
            name: "cargolock.setsid",
            row: "9 cargo build-dir lock",
            stretch_milli: 1000,
            setup: |w| {
                setsid(w, sleeper(CHILD, "cargo"));
                holder_tab(w);
            },
            each: |w, rel| name_at_20m(w, rel, CALL, TierB::Lock, Some(2100)),
            id: "a400",
            until_ms: M20 + 6_000,
            expect: &[At(M1, Fine), At(M20, Quiet), At(M20 + 6_000, Fine)],
            model: true,
        },
        Scenario {
            name: "curl.pty",
            row: "10 curl to a silent TCP peer",
            stretch_milli: 1000,
            setup: |w| pty(w, curl(FG), "curl http://192.0.2.10:8080/"),
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                PhiAt(M1, 434),
                WaitAt(M1, WaitName::Tcp),
                FirstAt(Quiet, 416_000),
                FirstAt(Stuck, 1_106_000),
                At(M20, Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "curl.setsid",
            row: "10 curl to a silent TCP peer",
            stretch_milli: 1000,
            setup: |w| setsid(w, curl(CHILD)),
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                PhiAt(M1, 434),
                WaitAt(M1, WaitName::Tcp),
                FirstAt(Stuck, 1_106_000),
                At(M20, Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "sleepcat.pty",
            row: "11 sleep 600 | cat",
            stretch_milli: 1000,
            setup: |w| {
                let mut s = sleeper(FG, "sleep");
                s.fds.push(pipe_end(1, 0xA2, 0xA1, true, 0, 0));
                pty(w, s, "sleep 600 | cat");
                let mut c = sleeper(FG + 1, "cat");
                c.pgid = FG;
                c.fds.push(pipe_end(0, 0xA1, 0xA2, false, 0x8, 0));
                w.run_bg(1, c);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Pipe),
                At(M20, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "sleepcat.setsid",
            row: "11 sleep 600 | cat",
            stretch_milli: 1000,
            setup: |w| {
                let mut s = sleeper(CHILD, "sleep");
                s.fds.push(pipe_end(1, 0xA2, 0xA1, true, 0, 0));
                setsid(w, s);
                let mut c = sleeper(CHILD + 1, "cat");
                c.ppid = CALL;
                c.pgid = CALL;
                c.sid = CALL;
                c.fds.push(pipe_end(0, 0xA1, 0xA2, false, 0x8, 0));
                w.add(c);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Pipe),
                At(M20, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "sleepsleep.setsid",
            row: "11b sleep | sleep (no wait flag)",
            stretch_milli: 1000,
            setup: |w| {
                let mut s = sleeper(CHILD, "sleep");
                s.fds.push(pipe_end(1, 0xC2, 0xC1, true, 0, 0));
                setsid(w, s);
                let mut c = sleeper(CHILD + 1, "sleep");
                c.ppid = CALL;
                c.pgid = CALL;
                c.sid = CALL;
                c.fds.push(pipe_end(0, 0xC1, 0xC2, false, 0, 0));
                w.add(c);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[At(M1, Fine), WaitAt(M1, WaitName::Unnamed), At(M20, Quiet)],
            model: true,
        },
        Scenario {
            name: "yessleep.setsid",
            row: "11b yes | sleep (pipe-full)",
            stretch_milli: 1000,
            setup: |w| {
                let mut y = sleeper(CHILD, "yes");
                y.fds.push(pipe_end(1, 0xB2, 0xB1, true, 0, 0));
                setsid(w, y);
                let mut s = sleeper(CHILD + 1, "sleep");
                s.ppid = CALL;
                s.pgid = CALL;
                s.sid = CALL;
                s.fds.push(pipe_end(0, 0xB1, 0xB2, false, 0x10, 65_536));
                w.add(s);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::PipeFull),
                At(M20, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "ctrlz.pty",
            row: "12 Ctrl-Z on a spinner",
            stretch_milli: 1000,
            setup: |w| pty(w, spinner(FG, "spin"), "./spin"),
            each: |w, rel| {
                if rel < 10_000 {
                    w.cr(1);
                }
                if rel == 10_000 {
                    w.p(FG).state = ProcState::Stopped;
                    w.close_block(1, 148);
                }
            },
            id: "b51",
            until_ms: M20,
            expect: &[
                ClassAt(M1, Class::Stopped),
                At(M1, Parked),
                At(M20, Parked),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "sigstop.pty",
            row: "13 external SIGSTOP of the tty leader",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = spinner(FG, "spin");
                p.state = ProcState::Stopped;
                pty(w, p, "./spin");
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Stopped),
                FirstAt(Quiet, 416_000),
                At(M20, Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "sigstopcr.pty",
            row: "13 external SIGSTOP of the tty leader, then a CR",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = spinner(FG, "spin");
                p.state = ProcState::Stopped;
                pty(w, p, "./spin");
            },
            each: |w, rel| {
                if rel >= 4_000 {
                    w.gate(1, GateWord::Stopped, 1, rel - 4_000);
                }
            },
            id: "b51",
            until_ms: M20,
            expect: &[
                FirstAt(Stuck, 4_000),
                ClassAt(M1, Class::UnreadInput),
                At(M20, Stuck),
            ],
            model: false,
        },
        Scenario {
            name: "sigstopwait.setsid",
            row: "13b SIGSTOP under zsh -c … ; wait",
            stretch_milli: 1000,
            setup: |w| {
                let mut node = sleeper(CHILD, "node");
                node.state = ProcState::Stopped;
                setsid(w, node);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[At(M1, Fine), ClassAt(M1, Class::Stopped), At(M20, Stuck)],
            model: true,
        },
        Scenario {
            name: "zombie.pty",
            row: "14 an unreaped zombie",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "parent"), "./parent");
                let mut z = sleeper(FG + 1, "child");
                z.ppid = FG;
                z.pgid = FG;
                z.sid = 200;
                z.state = ProcState::Zombie;
                w.add(z);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                ClassAt(12_000, Class::Zombie),
                At(M1, Fine),
                At(M20, Stuck),
                Zombies(M20, FG, 1),
            ],
            model: true,
        },
        Scenario {
            name: "zombie.setsid",
            row: "14 an unreaped zombie",
            stretch_milli: 1000,
            setup: |w| {
                setsid(w, sleeper(CHILD, "parent"));
                let mut z = sleeper(CHILD + 1, "child");
                z.ppid = CHILD;
                z.pgid = CALL;
                z.sid = CALL;
                z.state = ProcState::Zombie;
                w.add(z);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Zombie),
                At(M20, Stuck),
                Zombies(M20, CHILD, 1),
            ],
            model: true,
        },
        Scenario {
            name: "spin.pty",
            row: "15 user-space spin",
            stretch_milli: 1000,
            setup: |w| pty(w, spinner(FG, "spin"), "./spin"),
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::BusySilent),
                At(M20, Busy),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "spin.setsid",
            row: "15 user-space spin",
            stretch_milli: 1000,
            setup: |w| setsid(w, spinner(CHILD, "spin")),
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::BusySilent),
                At(M20, Busy),
                Never(Stuck),
                NoProgressNever,
            ],
            model: true,
        },
        Scenario {
            name: "poll1ms.pty",
            row: "16 1 ms poller (its 20 min column reads at stretch 2.0, as its text says)",
            stretch_milli: 2000,
            setup: |w| pty(w, poller(FG), "./poll1ms"),
            each: nothing,
            id: "b51",
            until_ms: 2_220_000,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Polling),
                At(M20, Quiet),
                FirstAt(Stuck, 2_212_000),
            ],
            model: true,
        },
        Scenario {
            name: "poll1ms.setsid",
            row: "16 1 ms poller (stretch 2.0)",
            stretch_milli: 2000,
            setup: |w| setsid(w, poller(CHILD)),
            each: nothing,
            id: "a400",
            until_ms: 2_220_000,
            expect: &[At(M1, Fine), At(M20, Quiet), FirstAt(Stuck, 2_212_000)],
            model: true,
        },
        Scenario {
            name: "poll1ms.loadrise.setsid",
            row: "16 + the load doubles at 10 min: μ grows, quiet leaves under 2.5, and φ climbs again on the new μ",
            stretch_milli: 1000,
            setup: |w| setsid(w, poller(CHILD)),
            each: |w, rel| {
                if rel >= 602_000 {
                    w.machine.stretch_milli = 2000;
                }
            },
            id: "a400",
            until_ms: 2_220_000,
            expect: &[
                FirstAt(Quiet, 416_000),
                At(600_000, Quiet),
                At(602_000, Fine),
                At(828_000, Fine),
                At(830_000, Quiet),
                FirstAt(Stuck, 2_212_000),
            ],
            model: true,
        },
        Scenario {
            name: "poll1ms.loadrise_stuck.setsid",
            row: "16 + the load doubles at 20 min, after stuck: stuck holds while φ ≥ 3 (no flap)",
            stretch_milli: 1000,
            setup: |w| setsid(w, poller(CHILD)),
            each: |w, rel| {
                if rel >= 1_202_000 {
                    w.machine.stretch_milli = 2000;
                }
            },
            id: "a400",
            until_ms: 1_400_000,
            expect: &[
                FirstAt(Stuck, 1_106_000),
                At(1_202_000, Stuck),
                At(1_400_000, Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "clang.pty",
            row: "17 clang compile loop",
            stretch_milli: 1000,
            setup: |w| {
                let mut make = sleeper(FG, "make");
                make.child_cpu_per_s = 900_000_000;
                pty(w, make, "make");
            },
            each: |w, rel| churn(w, FG, FG, rel, "cc1"),
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Running),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "clang.setsid",
            row: "17 clang compile loop",
            stretch_milli: 1000,
            setup: |w| {
                let mut make = sleeper(CHILD, "make");
                make.child_cpu_per_s = 900_000_000;
                setsid(w, make);
            },
            each: |w, rel| churn(w, CHILD, CALL, rel, "cc1"),
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Running),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "every10.pty",
            row: "18 a line every 10 s",
            stretch_milli: 1000,
            setup: |w| pty(w, sleeper(FG, "printslow"), "./printslow"),
            each: |w, rel| {
                if rel % 10_000 == 0 && rel > 0 {
                    w.line(1);
                }
            },
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                At(M20, Fine),
                Never(Quiet),
                PhiAtMost(40_000, 390),
            ],
            model: true,
        },
        Scenario {
            name: "every10.setsid",
            row: "18 a line every 10 s",
            stretch_milli: 1000,
            setup: |w| setsid(w, sleeper(CHILD, "printslow")),
            each: |w, rel| {
                if rel % 10_000 == 0 && rel > 0 {
                    w.call_file(CALL).size += 40;
                }
            },
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                At(M20, Fine),
                Never(Quiet),
                PhiAtMost(40_000, 390),
                NoProgressNever,
            ],
            model: true,
        },
        Scenario {
            name: "silentwriter.pty",
            row: "19 silent writer",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(FG, "writer");
                p.writes_per_s = 84_000;
                p.cpu_per_s = 30_000_000;
                pty(w, p, "./writer");
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[
                ClassAt(M1, Class::Running),
                At(M1, Fine),
                At(M20, Fine),
                Never(Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "silentwriter.setsid",
            row: "19 silent writer",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(CHILD, "writer");
                p.writes_per_s = 84_000;
                setsid(w, p);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[ClassAt(M1, Class::Running), At(M20, Fine), Never(Quiet)],
            model: true,
        },
        Scenario {
            name: "compute.pty",
            row: "20 legit compute",
            stretch_milli: 1000,
            setup: |w| pty(w, spinner(FG, "compute"), "./compute"),
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[At(M1, Fine), At(M20, Busy), Never(Stuck)],
            model: true,
        },
        Scenario {
            name: "regex.setsid",
            row: "21 regex backtrack (the real hang reads busy, by design)",
            stretch_milli: 1000,
            setup: |w| setsid(w, spinner(CHILD, "python3")),
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[At(M1, Fine), At(M20, Busy), Never(Stuck)],
            model: true,
        },
        Scenario {
            name: "rawspin.pty",
            row: "22 raw-mode spin",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, spinner(FG, "dash"), "./dash");
                w.tty_mode(1, false, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[At(M1, Fine), ClassAt(M1, Class::BusySilent), At(M20, Busy)],
            model: true,
        },
        Scenario {
            name: "rawspinkey.pty",
            row: "22 raw-mode spin, a key at 8 s (the gate publishes at 20 s)",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, spinner(FG, "dash"), "./dash");
                w.tty_mode(1, false, false);
            },
            each: |w, rel| {
                if rel >= 8_000 {
                    let word = if rel >= 20_000 {
                        GateWord::Stalled
                    } else {
                        GateWord::Pending
                    };
                    w.gate(1, word, 1, rel - 8_000);
                }
            },
            id: "b51",
            until_ms: M20,
            expect: &[
                At(10_000, Fine),
                FirstAt(Stuck, 20_000),
                At(M1, Stuck),
                At(M20, Stuck),
            ],
            model: false,
        },
        Scenario {
            name: "rawsleep.pty",
            row: "23 raw-mode sleep (deadlocked TUI)",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "tui"), "./tui");
                w.tty_mode(1, false, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20,
            expect: &[At(M1, Fine), WaitAt(M1, WaitName::TtyRaw), At(M20, Fine)],
            model: true,
        },
        Scenario {
            name: "rawsleepkey.pty",
            row: "23 raw-mode sleep, a key at 30 s (published at 300 s × stretch)",
            stretch_milli: 1000,
            setup: |w| {
                pty(w, sleeper(FG, "tui"), "./tui");
                w.tty_mode(1, false, false);
            },
            each: |w, rel| {
                if rel >= 30_000 {
                    let word = if rel >= 330_000 {
                        GateWord::Stalled
                    } else {
                        GateWord::Pending
                    };
                    w.gate(1, word, 1, rel - 30_000);
                }
            },
            id: "b51",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::UnreadInput),
                FirstAt(Stuck, 330_000),
                At(M20, Stuck),
            ],
            model: false,
        },
        Scenario {
            name: "standin.setsid",
            row: "24 stand-in agent: the agent runs, its call waits on tcp",
            stretch_milli: 1000,
            setup: |w| setsid(w, curl(CHILD)),
            each: |w, _| w.line(1),
            id: "a400",
            until_ms: M20,
            expect: &[
                At(M1, Fine),
                OtherAt(M1, "b1", Fine),
                At(M20, Stuck),
                OtherAt(M20, "b1", Fine),
            ],
            model: true,
        },
        Scenario {
            name: "sleeppoll.pty",
            row: "+ sleep-poll loop (stuck at 18 min 25 s × stretch: read at stretch 2.0)",
            stretch_milli: 2000,
            setup: |w| pty(w, sleeper(FG, "sh"), "while :; do sleep 0.5; done"),
            each: |w, rel| idle_churn(w, FG, FG, rel),
            id: "b51",
            until_ms: 2_220_000,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Polling),
                At(M20, Quiet),
                FirstAt(Stuck, 2_212_000),
            ],
            model: true,
        },
        Scenario {
            name: "sleeppoll.setsid",
            row: "+ sleep-poll loop (stretch 2.0)",
            stretch_milli: 2000,
            setup: |w| setsid(w, sleeper(CHILD, "sh")),
            each: |w, rel| idle_churn(w, CHILD, CALL, rel),
            id: "a400",
            until_ms: 2_220_000,
            expect: &[At(M1, Fine), At(M20, Quiet), FirstAt(Stuck, 2_212_000)],
            model: true,
        },
        Scenario {
            name: "spinsocket.pty",
            row: "+ spinner over a silent socket (CR-only output is not progress)",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = curl(FG);
                p.name = "npm".to_owned();
                p.writes_per_s = 5_000;
                pty(w, p, "npm install");
            },
            each: |w, _| w.cr(1),
            id: "b51",
            until_ms: M20,
            expect: &[At(M1, Fine), WaitAt(M1, WaitName::Tcp), At(M20, Stuck)],
            model: true,
        },
        Scenario {
            name: "spinsocket.setsid",
            row: "+ spinner over a silent socket, M7's logical-write noise replayed",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = curl(CHILD);
                p.name = "npm".to_owned();
                p.writes_per_s = 5_000;
                setsid(w, p);
                *w.call_file(CALL) = OutFile {
                    size: 0,
                    per_s: 74,
                    lf: Some(false),
                };
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[At(M1, Fine), WaitAt(M1, WaitName::Tcp), At(M20, Stuck)],
            model: true,
        },
        Scenario {
            name: "sleeppoll.interactive",
            row: "+ sleep-poll loop typed at an interactive shell: each sleep its own group, \
                  ticks between commands (one job; stuck at 18 min 25 s × stretch 2.0)",
            stretch_milli: 2000,
            setup: |w| {
                let at = w.at;
                w.tab(1).tab.block = Some(Block {
                    id: 51,
                    started_ms: at,
                    open: true,
                    command: "while :; do sleep 0.5; done".to_owned(),
                    status: None,
                });
            },
            each: typed_sleep_loop,
            id: "b51",
            until_ms: 2_220_000,
            expect: &[
                OneRow,
                At(M1, Fine),
                ClassAt(M1, Class::Polling),
                At(M20, Quiet),
                FirstAt(Stuck, 2_212_000),
            ],
            model: true,
        },
        Scenario {
            name: "makechain.pty",
            row: "+ make && make test: two groups in front of one block, the shell between them",
            stretch_milli: 1000,
            setup: |w| {
                let mut make = sleeper(FG, "make");
                make.child_cpu_per_s = 900_000_000;
                pty(w, make, "make && make test");
            },
            each: make_chain,
            id: "b51",
            until_ms: 120_000,
            expect: &[
                OneRow,
                ClassAt(10_000, Class::Running),
                At(10_000, Fine),
                ClassAt(40_000, Class::Running),
                At(40_000, Fine),
                At(62_000, Exited),
                At(118_000, Exited),
                Gone(120_000),
            ],
            model: true,
        },
        Scenario {
            name: "devserver.pty",
            row: "+ dev server asleep on its listener, an idle keep-alive client connected \
                  (listen, never tcp)",
            stretch_milli: 1000,
            setup: |w| pty(w, dev_server(FG), "npm run dev"),
            each: nothing,
            id: "b51",
            until_ms: M20 + 600_000,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Listen),
                At(M20, Fine),
                Never(Quiet),
                Never(Stuck),
                PhiAtMost(0, 0),
            ],
            model: true,
        },
        Scenario {
            name: "devserver.setsid",
            row: "+ dev server as an agent's call, an idle keep-alive client connected",
            stretch_milli: 1000,
            setup: |w| setsid(w, dev_server(CHILD)),
            each: nothing,
            id: "a400",
            until_ms: M20 + 600_000,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Listen),
                At(M20, Fine),
                Never(Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "tuiidle.pty",
            row: "+ idle full-screen program waking 1,100 times a second, no output, no key \
                  (an idle Claude Code: tty-raw, never POLLING)",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(FG, "claude");
                p.cpu_per_s = 18_000_000;
                p.syscalls_per_s = 1_100;
                pty(w, p, "claude");
                w.tty_mode(1, false, false);
            },
            each: nothing,
            id: "b51",
            until_ms: M20 + 600_000,
            expect: &[
                At(M1, Fine),
                ClassAt(M1, Class::Waiting),
                WaitAt(M1, WaitName::TtyRaw),
                At(M20, Fine),
                Never(Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "deadlock.setsid",
            row: "D2 an agent's background call deadlocked on its own lock, asleep with no CPU, \
                  held 26 h (I4 as it ran: agent-bg, quiet, never stuck; no_progress from φ 8)",
            stretch_milli: 1000,
            setup: |w| {
                w.call(
                    1,
                    AGENT,
                    CALL,
                    sleeper(CHILD, "aterm_gui-e0964a"),
                    Some(hint(CALL, Consumer::AgentBg, Some("bplle5dxj"), None)),
                );
            },
            each: nothing,
            id: "a400",
            until_ms: H26,
            expect: &[
                At(M1, Fine),
                WaitAt(M1, WaitName::Unnamed),
                FirstAt(Quiet, 416_000),
                NoProgressFrom(PHI8),
                At(H26, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "deadlock_eperm.setsid",
            row: "D2 the deadlocked call in three suspect runs: a reading refused at 10 min, a \
                  line and a refused reading at 20 min, a line at 40 min (a run with an \
                  unknown reading, mid-run or at its first instant, never says no_progress; \
                  the clean run after it does)",
            stretch_milli: 1000,
            setup: deadlocked,
            each: |w, rel| {
                w.p(CHILD).refused = rel == 600_000 || rel == M20;
                if rel == M20 || rel == 2 * M20 {
                    w.call_file(CALL).size += 40;
                }
            },
            id: "a400",
            until_ms: 3 * M20,
            expect: &[
                FirstAt(Quiet, 416_000),
                At(M20 - TICK_MS, Quiet),
                At(M20, Fine),
                At(2 * M20 - TICK_MS, Quiet),
                NoProgressFrom(2 * M20 + PHI8),
                At(3 * M20, Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "trickle.setsid",
            row: "D2 an agent's call asleep on an unnamed wait that wakes for 1.02 ms of CPU \
                  a minute (just above the bound: no no_progress)",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(CHILD, "python3");
                // A run from 0 sums rel_s × 17,000 ns: 1.02 ms a minute at
                // every tick, so the bound's units are pinned within 2 %.
                p.cpu_per_s = 17_000;
                setsid(w, p);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                WaitAt(M1, WaitName::Unnamed),
                FirstAt(Quiet, 416_000),
                At(M20, Quiet),
                NoProgressNever,
            ],
            model: true,
        },
        Scenario {
            name: "sip.setsid",
            row: "D2 the same wait at 0.96 ms of CPU a minute (just under the bound: \
                  no_progress from φ 8)",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(CHILD, "python3");
                p.cpu_per_s = 16_000;
                setsid(w, p);
            },
            each: nothing,
            id: "a400",
            until_ms: M20,
            expect: &[
                FirstAt(Quiet, 416_000),
                NoProgressFrom(PHI8),
                At(M20, Quiet),
            ],
            model: true,
        },
        Scenario {
            name: "deadlock_promoted.setsid",
            row: "D2 the deadlocked call as an agent's foreground call, promoted to the \
                  background at 20 min, after φ 8 (both consumers read at φ ≥ 8; the \
                  clock and the CPU sum carry across the promotion)",
            stretch_milli: 1000,
            setup: |w| {
                w.call(
                    1,
                    AGENT,
                    CALL,
                    sleeper(CHILD, "aterm_gui-e0964a"),
                    Some(hint(CALL, Consumer::AgentFg, Some("bplle5dxj"), None)),
                );
            },
            each: |w, rel| {
                if rel == M20 {
                    w.hint(
                        1,
                        hint(CALL, Consumer::AgentBg, Some("bplle5dxj"), Some(M20)),
                    );
                }
            },
            id: "a400",
            until_ms: 2 * M20,
            expect: &[
                FirstAt(Quiet, 416_000),
                NoProgressFrom(PHI8),
                At(2 * M20, Quiet),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "deadlock_after_work.setsid",
            row: "D2 I4's whole shape: the call burns a core for 10 min, prints its last \
                  line, then deadlocks with no CPU (the CPU the last line's tick read is \
                  before the run)",
            stretch_milli: 1000,
            setup: |w| setsid(w, spinner(CHILD, "aterm_gui-e0964a")),
            each: |w, rel| {
                if rel == 600_000 {
                    w.call_file(CALL).size += 40;
                    let p = w.p(CHILD);
                    p.cpu_per_s = 0;
                    p.state = ProcState::Sleeping;
                }
            },
            id: "a400",
            until_ms: 2 * M20,
            expect: &[
                At(600_000, Fine),
                NoProgressFrom(600_000 + PHI8),
                Never(Stuck),
            ],
            model: true,
        },
        Scenario {
            name: "deadlock_after_follow.setsid",
            row: "D2 the call follows a file for 10 min (legitimate; a reading refused at \
                  5 min), then its wait goes unnamed: the CPU sum restarts with the clock \
                  when the legitimate class ends, so the old refusal does not hide the word",
            stretch_milli: 1000,
            setup: |w| {
                let mut p = sleeper(CHILD, "tail");
                p.fds = follow_fds();
                setsid(w, p);
            },
            each: |w, rel| {
                w.p(CHILD).refused = rel == 300_000;
                if rel == 600_000 {
                    w.p(CHILD).fds.clear();
                }
            },
            id: "a400",
            until_ms: 2 * M20,
            expect: &[
                WaitAt(M1, WaitName::Follow),
                At(600_000, Fine),
                // The shown wait turns unnamed after 2 agreeing ticks, at
                // 602 s: the suspect run (and its CPU sum) starts there.
                NoProgressFrom(602_000 + PHI8),
                Never(Stuck),
            ],
            model: true,
        },
    ]
}

/// I4's shape: an agent's call (its consumer unknown: no transcript joins
/// it) whose test binary waits for ever on a mutex its own thread holds; no
/// descriptor names the wait, and it uses no CPU.
fn deadlocked(w: &mut World) {
    setsid(w, sleeper(CHILD, "aterm_gui-e0964a"));
}

/// A dev server: a listening socket, one idle keep-alive connection from a
/// browser, asleep in its kqueue with a read entry on each.
pub fn dev_server(pid: u32) -> Proc {
    let mut p = Proc::new(pid, 0, "node");
    p.fds = vec![
        Fd {
            fd: 20,
            kind: FdKind::Tcp {
                state: TcpState::Listen,
                remote: None,
                local_port: 5173,
                queued_read: Some(0),
            },
        },
        Fd {
            fd: 21,
            kind: FdKind::Tcp {
                state: TcpState::Established,
                remote: Some("127.0.0.1:52011".parse::<SocketAddr>().expect("an address")),
                local_port: 5173,
                queued_read: Some(0),
            },
        },
        Fd {
            fd: 9,
            kind: FdKind::EventQueue {
                sleeping: Some(true),
                knotes: Some(vec![
                    Knote {
                        ident: 20,
                        filter: KnoteFilter::Read,
                    },
                    Knote {
                        ident: 21,
                        filter: KnoteFilter::Read,
                    },
                ]),
            },
        },
    ];
    p
}

/// `while :; do sleep 0.5; done` typed at an interactive shell: the loop
/// runs in the shell (pid 200), and each `sleep` is a group of its own the
/// terminal puts in front. Every fifth tick lands between two commands, the
/// shell holding the terminal.
fn typed_sleep_loop(w: &mut World, rel: u64) {
    let n = u32::try_from(rel / TICK_MS).unwrap_or(0);
    let born = 3_000 + n;
    w.exit(born - 1, false);
    if n % 5 == 4 {
        w.tab(1).tpgid = 200;
        return;
    }
    let mut s = Proc::new(born, 200, "sleep");
    s.sid = 200;
    s.tty = true;
    s.counters.cpu_ns = 1_900_000;
    w.add(s);
    w.tab(1).tpgid = born;
}

/// `make && make test`: `make` prints a line every tick and churns
/// compilers until 18 s; the shell holds the terminal at 20 s; `make test`
/// (its own group) prints until 60 s, then the block closes with status 2.
fn make_chain(w: &mut World, rel: u64) {
    match rel {
        r if r < 20_000 => w.line(1),
        20_000 => {
            w.exit(FG, false);
            w.tab(1).tpgid = 200;
        }
        r if r < 60_000 => {
            if !w.procs.contains_key(&(FG + 1)) {
                let mut t = sleeper(FG + 1, "make");
                t.ppid = 200;
                t.sid = 200;
                t.tty = true;
                t.child_cpu_per_s = 900_000_000;
                w.add(t);
                w.tab(1).tpgid = FG + 1;
            }
            w.line(1);
        }
        60_000 => {
            w.exit(FG + 1, false);
            w.close_block(1, 2);
        }
        _ => {}
    }
}

/// The scenario named `name`.
pub fn named(name: &str) -> Scenario {
    all()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no scenario {name}"))
}

/// A second tab (key 2, `tab2`, shell 210) whose `cargo build` (pid 2100,
/// block 61) holds the build-directory lock while its build script sleeps
/// (`hrtimer_nanosleep`: a timer, Linux's public wait channel).
fn holder_tab(w: &mut World) {
    w.shell_tab(2, 2, 210);
    let cargo = sleeper(2100, "cargo");
    w.run_fg(2, 61, "cargo build", cargo);
    let mut script = sleeper(2101, "build-script-build");
    script.pgid = 2100;
    script.ppid = 2100;
    script.sid = 210;
    script.tty = true;
    script.wchan = Some("hrtimer_nanosleep".to_owned());
    w.add(script);
}

/// A compile loop: each tick a new compiler child with its own CPU is born
/// under `parent` and the last one reaped.
pub fn churn(w: &mut World, parent: u32, pgid: u32, rel: u64, name: &str) {
    let born = 3_000 + u32::try_from(rel / TICK_MS).unwrap_or(0);
    w.exit(born - 1, false);
    let mut cc = Proc::new(born, parent, name);
    cc.pgid = pgid;
    cc.sid = w.procs.get(&parent).map_or(0, |p| p.sid);
    cc.tty = w.procs.get(&parent).is_some_and(|p| p.tty);
    cc.state = ProcState::Running;
    cc.cpu_per_s = 900_000_000;
    cc.counters.cpu_ns = 400_000_000;
    w.add(cc);
}

/// A shell poll loop: each tick a `sleep` child is born and the last reaped,
/// with no work (1.9 ms/s of child CPU).
fn idle_churn(w: &mut World, parent: u32, pgid: u32, rel: u64) {
    let born = 3_000 + u32::try_from(rel / TICK_MS).unwrap_or(0);
    w.exit(born - 1, false);
    let mut s = Proc::new(born, parent, "sleep");
    s.pgid = pgid;
    s.sid = w.procs.get(&parent).map_or(0, |p| p.sid);
    s.tty = w.procs.get(&parent).is_some_and(|p| p.tty);
    s.counters.cpu_ns = 1_900_000;
    w.add(s);
}
