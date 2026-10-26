// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `ReapCredit` (`aterm_models_misc::reap_credit_model`): the real ledger
//! ([`aterm_vitals::explain::ReapLedger`]) steps through a window that dips, and each step is a
//! `Tick` of the model.
//!
//! SA §7.5 measured one 10 s window at 2.27 cores by the naive Δ(live + reaped) against 3.62
//! live-only (`f2/reconcile3.out`, since lost). This window is a synthetic one in its shape, read
//! once a second: claude (pid 300) runs at 3.62 cores throughout; it ran cargo (310) through
//! root's `sudo -u` (305, so the ledger has no row for it), and cargo came into the window with
//! 14.7 s of its own and its children's time. The sudo exits at 3.5 s (40 ms of its own, which
//! claude's reaped time takes) and leaves cargo to launchd, which reaps it at 6.5 s after the
//! 6 s reading read it as a zombie; claude's rustc (320), born at 7.2 s, is reaped by claude at
//! 9.5 s after 1.16 s.

use std::collections::BTreeMap;

use aterm_models_misc::reap_credit_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_vitals::explain::ReapLedger;
use aterm_vitals::explain::fixtures::tab;
use aterm_vitals::explain::reap::{ReapFault, StepIn};
use aterm_vitals::model::{ProcSample, ProcState, TabRoot, attribute};

type S = BTreeMap<&'static str, i64>;

const MS: u64 = 1_000_000;
const ME: u32 = 501;
const ATERM: u32 = 100;
const SHELL: u32 = 200;
const CLAUDE: u32 = 300;
const SUDO: u32 = 305;
const CARGO: u32 = 310;
const RUSTC: u32 = 320;
/// The first reading's wall clock, unix ms.
const T0_MS: i64 = 1_792_517_000_000;

/// One process at one reading, ms.
fn proc(pid: u32, ppid: u32, start: u64, birth_s: i64, cpu: u64, child: u64) -> ProcSample {
    ProcSample {
        pid,
        ppid,
        start: Some(start),
        cpu_ns: Some(cpu * MS),
        child_cpu_ns: Some(child * MS),
        uid: Some(ME),
        state: ProcState::Run,
        birth_ms: Some(T0_MS + birth_s * 1000),
        ..ProcSample::default()
    }
}

/// The sweep at second `t`.
fn sweep(t: u64) -> Vec<ProcSample> {
    let mut procs = vec![
        proc(ATERM, 1, 1_000, -3_600, 5_000, 0),
        proc(SHELL, ATERM, 2_000, -3_000, 0, 0),
        proc(
            CLAUDE,
            SHELL,
            3_000,
            -600,
            100_000 + 3_620 * t,
            u64::try_from(claude_child(t)).unwrap_or(0),
        ),
    ];
    if t <= 3 {
        procs.push(ProcSample {
            pid: SUDO,
            ppid: CLAUDE,
            uid: Some(0),
            state: ProcState::Run,
            birth_ms: Some(T0_MS - 60_000),
            ..ProcSample::default()
        });
    }
    if t <= 6 {
        let (cpu, child) = if t <= 5 {
            (10_200 + 20 * t, 4_500 + 10 * t)
        } else {
            (10_330, 4_560)
        };
        let mut cargo = proc(CARGO, if t <= 3 { SUDO } else { 1 }, 3_100, -59, cpu, child);
        if t == 6 {
            cargo.state = ProcState::Zombie;
        }
        procs.push(cargo);
    }
    if t == 8 || t == 9 {
        let birth = proc(RUSTC, CLAUDE, 3_200, 0, if t == 8 { 400 } else { 900 }, 0);
        procs.push(ProcSample {
            birth_ms: Some(T0_MS + 7_200),
            ..birth
        });
    }
    // The kernel's order: newest first.
    procs.reverse();
    procs
}

/// The model's `m` at second `t`: `(gen, ms, mo, mc, mk)`.
fn m_at(t: u64) -> (i64, i64, i64, i64, i64) {
    match t {
        0..=5 => (
            0,
            1,
            i64::from(t >= 4),
            10_200 + 20 * t as i64,
            4_500 + 10 * t as i64,
        ),
        6 => (0, 2, 1, 10_330, 4_560),
        7 => (0, 0, 1, 10_330, 4_560),
        8 => (1, 1, 0, 400, 0),
        9 => (1, 1, 0, 900, 0),
        _ => (1, 0, 0, 1_160, 0),
    }
}

/// `m`'s pid and start in generation `g`.
fn m_key(g: i64) -> (u32, u64) {
    if g == 0 {
        (CARGO, 3_100)
    } else {
        (RUSTC, 3_200)
    }
}

/// Everything the tab's members have done by second `t`, ms: claude's own and reaped time,
/// cargo's own and its children's (after launchd reaped it among it), and rustc's while alive.
fn work(t: u64) -> i64 {
    let (g, ms, _, mc, mk) = m_at(t);
    let cargo = if g == 0 { mc + mk } else { 14_890 };
    let rustc = if g == 1 && ms > 0 { mc + mk } else { 0 };
    100_000 + 3_620 * t as i64 + claude_child(t) + cargo + rustc
}

/// Claude's reaped children's time at second `t`, ms: the sudo's 40 ms, then rustc's 1.16 s.
fn claude_child(t: u64) -> i64 {
    7_000 + if t >= 4 { 40 } else { 0 } + if t >= 10 { 1_160 } else { 0 }
}

/// The model's environment at second `t`: what the sweep reads, and `w`, the tab's work since
/// the first reading ([`ticks`] puts it in the counter's frame).
fn env(t: u64) -> S {
    let (g, ms, mo, mc, mk) = m_at(t);
    S::from([
        ("pc", 100_000 + 3_620 * t as i64),
        ("pk", claude_child(t)),
        ("gen", g),
        ("ms", ms),
        ("mo", mo),
        ("mc", mc),
        ("mk", mk),
        ("w", work(t) - work(0)),
        ("n", 0),
    ])
}

/// The ledger's state after second `t`'s step, in the model's terms.
fn ledger(l: &ReapLedger, t: u64) -> S {
    let (g, ms, _, _, _) = m_at(t);
    let claude = l.row(CLAUDE).expect("claude's row");
    let (pid, start) = m_key(g);
    let m = l.row(pid).filter(|r| r.start == start && ms > 0);
    let ms_of = |ns: u64| i64::try_from(ns / MS).unwrap_or(i64::MAX);
    S::from([
        ("c", ms_of(l.totals().tab(1))),
        ("rpc", ms_of(claude.cpu_ns)),
        ("rpk", ms_of(claude.child_ns)),
        ("pend", ms_of(claude.pending_ns + claude.doubt_ns)),
        ("row", i64::from(m.is_some())),
        ("rgen", g),
        ("rmc", m.map_or(0, |r| ms_of(r.cpu_ns))),
        ("rmk", m.map_or(0, |r| ms_of(r.child_ns))),
        ("rpar", m.map_or(0, |r| i64::from(r.parent.is_none()))),
        ("zread", m.map_or(0, |r| i64::from(r.zombie))),
    ])
}

/// What second `t`'s reading read, for the next step.
fn last(t: u64) -> S {
    let e = env(t);
    S::from([
        ("lpc", e["pc"]),
        ("lmc", e["mc"]),
        ("lgen", e["gen"]),
        ("lpres", i64::from(e["ms"] > 0)),
        ("lmo", e["mo"]),
    ])
}

/// The live-only figure's growth from second `t - 1` to `t`: the own CPU of the members read
/// at both readings, ms.
fn live_step(t: u64) -> i64 {
    let (before, now) = (env(t - 1), env(t));
    let m = if before["ms"] > 0 && now["ms"] > 0 && before["gen"] == now["gen"] {
        now["mc"] - before["mc"]
    } else {
        0
    };
    now["pc"] - before["pc"] + m
}

/// The ledger stepped over the window's eleven readings, after each step.
fn run(fault: Option<ReapFault>) -> Vec<S> {
    let tabs: Vec<TabRoot> = vec![tab(1, SHELL)];
    let mut l = match fault {
        Some(f) => ReapLedger::default().with_fault(f),
        None => ReapLedger::default(),
    };
    let mut after = Vec::new();
    for t in 0..=10 {
        let procs = sweep(t);
        let members = attribute(&procs, ATERM, &tabs);
        l.step(&StepIn {
            at_ns: (t + 1) * 1_000 * MS,
            at_unix_ms: T0_MS + i64::try_from(t * 1000).unwrap_or(0),
            procs: &procs,
            members: &members,
            delegated: &vec![false; procs.len()],
            tabs: &tabs,
            self_uid: ME,
            truncated: false,
        });
        after.push(ledger(&l, t));
    }
    after
}

/// Each step from the second reading on as the model's `Tick`: its state before and after, the
/// tab's work (`w`) in the counter's frame, from what it read at the first reading (which counts
/// none: every process was there before it), so the counter meets it where nothing counts twice.
fn ticks(m: &Model, after: &[S]) -> Vec<(S, S)> {
    let mut steps = Vec::new();
    let (mut c, mut live, mut pend, mut fr) = (after[0]["c"], 0, after[0]["pend"], 0);
    let frame = |t: u64| ("w", env(t)["w"] + after[0]["c"]);
    for t in 1..=10u64 {
        let k = usize::try_from(t).unwrap_or(0);
        let (before, now) = (env(t - 1), env(t));
        let gone = i64::from(before["ms"] > 0 && (now["ms"] == 0 || now["gen"] != before["gen"]));
        let mut pre = m.init_state();
        pre.extend(now.clone());
        pre.extend(after[k - 1].clone());
        pre.extend(last(t - 1));
        pre.extend([
            frame(t),
            ("gone", gone),
            ("cp", c),
            ("live", live),
            ("livep", live),
            ("pendp", pend),
            ("fr", fr),
        ]);
        let mut post = m.init_state();
        post.extend(now);
        post.extend(after[k].clone());
        post.extend(last(t));
        fr = i64::from(gone == 1 && before["mo"] == 1);
        post.extend([
            frame(t),
            ("gone", 0),
            ("cp", after[k - 1]["c"]),
            ("live", live + live_step(t)),
            ("livep", live),
            ("pendp", after[k - 1]["pend"]),
            ("fr", fr),
        ]);
        (c, live, pend) = (post["c"], post["live"], post["pend"]);
        steps.push((pre, post));
    }
    steps
}

/// Replays `steps` on `m`: `Err` where a step is no `Tick`, else the laws the states break.
fn replay(m: &Model, steps: &[(S, S)]) -> Result<Vec<&'static str>, String> {
    let mut broken = std::collections::BTreeSet::new();
    for (t, (pre, post)) in steps.iter().enumerate() {
        if interp::admits(m, pre, post) != Some("Tick") {
            return Err(format!(
                "the step at {} s is no `Tick`: {pre:?} to {post:?}",
                t + 1
            ));
        }
        for i in &m.invariants {
            if !m.check_invariant(i.name, post) {
                broken.insert(i.name);
            }
        }
    }
    Ok(broken.into_iter().collect())
}

/// The window's figure from the ledger's counter, millicores over its 10 s.
fn window_mc(after: &[S]) -> i64 {
    (after[10]["c"] - after[0]["c"]) / 10
}

#[test]
fn the_window_that_dips_is_a_run_of_the_model() {
    let m = interp::with_buggy(&reap_credit_model(), 0);
    let after = run(None);
    assert_eq!(replay(&m, &ticks(&m, &after)), Ok(vec![]));

    // The live-only figure: the tab's members alive at both ends, by its parent chains (claude).
    let live_mc = (env(10)["pc"] - env(0)["pc"]) / 10;
    // The naive figure: the members' `cpu + child` at the end less at the start (sudo unread).
    let naive = |t: u64| {
        let procs = sweep(t);
        let members = attribute(&procs, ATERM, &[tab(1, SHELL)]);
        procs
            .iter()
            .zip(&members)
            .filter(|(_, m)| m.tab == Some(0))
            .map(|(p, _)| p.cpu_ns.unwrap_or(0) + p.child_cpu_ns.unwrap_or(0))
            .sum::<u64>()
    };
    let naive_mc = i64::try_from((naive(10) - naive(0)) / MS).unwrap_or(0) / 10;
    assert_eq!(
        (live_mc, naive_mc),
        (3_620, 2_270),
        "the measured window's shape"
    );
    // Claude's 36.2 s, the sudo's 40 ms, cargo's 190 ms since the start and rustc's 1.16 s.
    assert_eq!(window_mc(&after), 3_759);
}

fn caught(fault: ReapFault, dial: i64, laws: &[&str]) -> Vec<S> {
    let model = reap_credit_model();
    let after = run(Some(fault));
    let healthy = interp::with_buggy(&model, 0);
    assert!(
        replay(&healthy, &ticks(&healthy, &after)).is_err(),
        "{fault:?}: the healthy model admits the defect"
    );
    let mutant = interp::with_buggy(&model, dial);
    let broken = replay(&mutant, &ticks(&mutant, &after))
        .unwrap_or_else(|e| panic!("{fault:?}: Buggy {dial} does not admit the run: {e}"));
    assert_eq!(broken, laws, "{fault:?}");
    after
}

/// THE NEGATIVE CONTROL SA names: the naive Δ(live + reaped) reads the measured dip, 2.27
/// cores against 3.62 live-only, and breaks `RateNeverBelowLive` at launchd's reap of cargo.
#[test]
fn the_naive_sum_is_caught_by_rate_never_below_live() {
    let after = caught(
        ReapFault::Naive,
        1,
        &["RateNeverBelowLive", "TabCounterMonotone"],
    );
    assert_eq!(window_mc(&after), 2_270);
}

#[test]
fn a_zombie_read_forgotten_is_caught_by_zombie_read_once() {
    let after = caught(
        ReapFault::ForgetZombieRead,
        2,
        &["RateNeverBelowLive", "ZombieReadOnce"],
    );
    assert_eq!(window_mc(&after), 3_755, "cargo's last 40 ms lost");
}

#[test]
fn a_first_parent_kept_is_caught_by_pending_drops_on_foreign_reap() {
    let after = caught(
        ReapFault::KeepFirstParent,
        3,
        &["PendingDropsOnForeignReap"],
    );
    assert_eq!(
        window_mc(&after),
        3_733,
        "cargo's time pending against claude absorbs rustc's last 260 ms"
    );
}

/// Nothing a gone member left absorbed: rustc, counted from its rows, counts again in claude's
/// reaped time when claude reaps it, and the counter passes the tab's work.
#[test]
fn nothing_absorbed_is_caught_by_counted_once() {
    let after = caught(ReapFault::AbsorbNothing, 4, &["CountedOnce"]);
    assert_eq!(window_mc(&after), 3_849, "rustc's 900 ms counted twice");
}
