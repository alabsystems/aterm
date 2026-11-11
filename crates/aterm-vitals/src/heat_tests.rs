// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The heat chain's laws (A4 §15) on the readings the recordings hold: each with the control that
//! breaks it.

use super::*;
use crate::model::Throttle;

/// Apple Silicon's Mach timebase (24 MHz).
const TB: (u32, u32) = (125, 3);

/// This M5's pmgr tables: Super 1308–4464 MHz, Efficiency 972–3048 MHz.
const M5: ClockRange = ClockRange {
    p_mhz: (1308.0, 4464.0),
    e_mhz: (972.0, 3048.0),
};

const SECOND: u64 = 1_000_000_000;

/// A gate whose every verdict passed: what a chip would carry once M-PCLK and M-ECLK pass there.
const PASSED: ClockGate = ClockGate {
    p: [Verdict::Pass; DECILES],
    e: Verdict::Pass,
};

fn ticks(ns: u64) -> u64 {
    ns * 3 / 125
}

/// One second's change: `p_cores` Super cores at `p_mhz`, `e_cores` Efficiency cores at
/// `e_mhz`, every time account the busy time, 2 instructions a cycle, 1 W a busy core.
fn second(p_cores: f64, p_mhz: f64, e_cores: f64, e_mhz: f64) -> Counters {
    let p_ns = (p_cores * 1e9) as u64;
    let e_ns = (e_cores * 1e9) as u64;
    let p_cycles = (p_mhz * 1e6 * p_cores) as u64;
    let e_cycles = (e_mhz * 1e6 * e_cores) as u64;
    Counters {
        cpu_ticks: ticks(p_ns + e_ns),
        qos_ticks: ticks(p_ns + e_ns),
        p_ticks: ticks(p_ns),
        energy_nj: p_ns + e_ns,
        instructions: 2 * (p_cycles + e_cycles),
        cycles: p_cycles + e_cycles,
        p_instructions: 2 * p_cycles,
        p_cycles,
        ..Counters::default()
    }
}

fn moved(c: Counters) -> HeatMove {
    HeatMove {
        counters: c,
        window_ns: SECOND,
        timebase: TB,
    }
}

/// Four Super cores at `p_busy` each and six Efficiency cores at `e_busy`.
fn fracs(p_busy: f64, e_busy: f64) -> Vec<f64> {
    let mut f = vec![p_busy; 4];
    f.extend([e_busy; 6]);
    f
}

const P_CORES: [usize; 4] = [0, 1, 2, 3];

fn input<'a>(
    pressure: ThermalPressure,
    m: HeatMove,
    gate: ClockGate,
    cores: &'a [f64],
) -> Input<'a> {
    Input {
        lane: CoalLane::On,
        pressure: Some(Ok(pressure)),
        moved: Some(Ok(m)),
        range: Some(Ok(M5)),
        gate,
        cores: Some((cores, SECOND)),
        p_cores: Some(&P_CORES),
        host: HostWatts::default(),
    }
}

/// An engine that learned `mhz` in the full Super decile (9) over a minute at Nominal.
fn learned(mhz: f64, gate: ClockGate) -> Engine {
    let mut e = Engine::default();
    let cores = fracs(1.0, 0.0);
    for _ in 0..60 {
        let m = moved(second(4.0, mhz, 0.0, 0.0));
        let v = e.absorb(&input(ThermalPressure::Nominal, m, gate, &cores));
        assert!(v.chain.is_ok(), "{v:?}");
    }
    e
}

/// `the_heat_ladder_follows_pressure` (A4 §15): pressure Heavy with thermalState Fair lights two
/// steps and the well, the 10:42 recording. CONTROL: A2's thermalState-only rule draws nothing.
#[test]
fn the_heat_ladder_follows_pressure() {
    let fair = Thermal {
        level: Throttle::Light,
        raw: 1,
        low_power: Some(false),
        exact: true,
    };
    let heavy = HeatView {
        pressure: Ok(ThermalPressure::Heavy),
        ..HeatView::default()
    };
    assert_eq!(heavy.steps(), 2);
    assert_eq!(heavy.well(Some(fair)), Some(State::Warn));
    assert_eq!(
        fair.state(),
        None,
        "control: thermalState alone draws nothing"
    );
    let moderate = HeatView {
        pressure: Ok(ThermalPressure::Moderate),
        ..HeatView::default()
    };
    assert_eq!((moderate.steps(), moderate.well(Some(fair))), (1, None));
    let serious = Thermal {
        level: Throttle::Heavy,
        raw: 2,
        ..fair
    };
    assert_eq!(moderate.well(Some(serious)), Some(State::Warn));
    let trapping = HeatView {
        pressure: Ok(ThermalPressure::Trapping),
        ..HeatView::default()
    };
    assert_eq!(trapping.well(Some(serious)), Some(State::Critical));
    assert_eq!(HeatView::default().steps(), 0, "unread lights nothing");
}

/// A posted state outside `0..=4` is no level, never a guessed one.
#[test]
fn a_pressure_state_past_sleeping_is_no_level() {
    let levels: Vec<_> = (0..=5).map(ThermalPressure::of_state).collect();
    assert_eq!(
        levels,
        [
            Some(ThermalPressure::Nominal),
            Some(ThermalPressure::Moderate),
            Some(ThermalPressure::Heavy),
            Some(ThermalPressure::Trapping),
            Some(ThermalPressure::Sleeping),
            None,
        ]
    );
}

/// `heat_scores_nothing_without_a_cut` (A4 §15): Moderate and Heavy with the Super clock at its
/// usual score 0 and light the ladder (and at Heavy the well) alone: 10:10:09, a 10-thread spin at
/// Moderate with the Super clock at 4114 against its usual 4113. So does a clock under its usual by
/// less than [`CUT_SHARE`] (4 % here): the cut is more than 5 % under, never any dip. CONTROL: the
/// first draft's 0.8 for Moderate scores there.
#[test]
fn heat_scores_nothing_without_a_cut() {
    let first_draft = |p: ThermalPressure| if p.judges_cut() { 0.8 } else { 0.0 };
    for level in [ThermalPressure::Moderate, ThermalPressure::Heavy] {
        for clock in [4114.0, 4113.0 * 0.96] {
            let mut e = learned(4113.0, PASSED);
            let cores = fracs(1.0, 1.0);
            let v = e.absorb(&input(
                level,
                moved(second(4.0, clock, 6.0, 2900.0)),
                PASSED,
                &cores,
            ));
            let chain = v.chain.expect("judged");
            assert!(
                matches!(chain.cut, Cut::Within(_)),
                "{level:?} at {clock}: {chain:?}"
            );
            assert_eq!(v.score(), 0.0, "{level:?} at {clock}");
            assert_eq!(v.steps(), level.level());
            assert_ne!(first_draft(level), v.score(), "control: {level:?}");
        }
    }
}

/// The 10:42:59 reading: the Super clock at 2770 against its usual 4113 at Heavy scores
/// 1 + log2(4113 ÷ 2770) ÷ 3 = 1.190 where the decile's verdict passed. On the M5 as N1 measured
/// it (decile 9 passed at Heavy only), the same reading is gated and scores nothing.
#[test]
fn the_heavy_cut_scores_its_capacity_where_its_decile_passed() {
    let cores = fracs(1.0, 1.0);
    let reading = moved(second(4.0, 2770.0, 6.0, 3048.0));
    let mut passed = learned(4113.0, PASSED);
    let v = passed.absorb(&input(ThermalPressure::Heavy, reading, PASSED, &cores));
    let chain = v.chain.expect("judged");
    assert_eq!(chain.decile, Some(9));
    assert!(matches!(chain.cut, Cut::Cut(_)), "{chain:?}");
    assert!((v.score() - 1.190).abs() < 0.001, "{}", v.score());
    let mut m5 = learned(4113.0, ClockGate::APPLE_M5);
    let v = m5.absorb(&input(
        ThermalPressure::Heavy,
        reading,
        ClockGate::APPLE_M5,
        &cores,
    ));
    let chain = v.chain.expect("judged");
    assert_eq!(chain.cut, Cut::Gated(Verdict::HeavyOnly));
    assert_eq!(v.score(), 0.0);
    assert_eq!(
        chain.usual_mhz,
        Some(4113.0),
        "the usual is learned all the same"
    );
}

/// `a_cut_is_judged_against_its_own_decile` (A4 §15): at light Super load the derived clock reads
/// low (1.8–5.6 % under CPU Stats, `rec/heat-clocks.jsonl` i=20–29); learned the same way per
/// decile, a clock 5.6 % under CPU Stats claims no cut. CONTROL: judged against the CPU Stats
/// clock (4000 MHz), the same reading claims one.
#[test]
fn a_cut_is_judged_against_its_own_decile() {
    let biased = 4000.0 * (1.0 - 0.056);
    let cores = fracs(0.15, 0.2);
    let mut e = Engine::default();
    for _ in 0..40 {
        let m = moved(second(0.6, biased, 1.2, 2000.0));
        e.absorb(&input(ThermalPressure::Nominal, m, PASSED, &cores));
    }
    let v = e.absorb(&input(
        ThermalPressure::Moderate,
        moved(second(0.6, biased, 1.2, 2000.0)),
        PASSED,
        &cores,
    ));
    let chain = v.chain.expect("judged");
    assert_eq!(chain.decile, Some(1));
    assert!(matches!(chain.cut, Cut::Within(_)), "{chain:?}");
    let control = Cut::judge(
        Some(ThermalPressure::Moderate),
        chain.judged.p_mhz,
        chain.decile,
        Verdict::Pass,
        Some(4000.0),
    );
    assert!(matches!(control, Cut::Cut(_)), "control: {control:?}");
}

/// The usual clock learns only at Nominal or Moderate, and claims nothing before
/// [`USUAL_LEARN_S`] of readings. CONTROL: the same readings at Heavy teach nothing.
#[test]
fn the_usual_clock_learns_only_at_nominal_or_moderate() {
    let cores = fracs(1.0, 0.0);
    let reading = moved(second(4.0, 4100.0, 0.0, 0.0));
    let mut e = Engine::default();
    for _ in 0..60 {
        e.absorb(&input(ThermalPressure::Heavy, reading, PASSED, &cores));
    }
    assert_eq!(e.usual.usual(9), None, "Heavy teaches nothing");
    for _ in 0..30 {
        let v = e.absorb(&input(ThermalPressure::Moderate, reading, PASSED, &cores));
        assert_eq!(v.chain.expect("judged").cut, Cut::Unlearned);
    }
    let usual = e.usual.usual(9).expect("learned after 30 s");
    assert!((usual - 4100.0).abs() < 1e-6, "{usual}");
    let v = e.absorb(&input(ThermalPressure::Moderate, reading, PASSED, &cores));
    assert!(matches!(v.chain.expect("judged").cut, Cut::Within(_)));
    assert_eq!(e.usual.usual(8), None, "another decile learns its own");
}

/// After [`USUAL_SPAN_S`] of readings a decile's usual moves as an exponential mean, so it follows
/// a lasting change instead of holding the first hour forever. A reading whose decile is past 9, or
/// whose clock or window is not a positive number, teaches nothing: a zero window on a decile that
/// has learned nothing would make its usual 0/0, a NaN no later reading undoes.
#[test]
fn a_learned_usual_follows_a_lasting_change() {
    let mut u = UsualClocks::default();
    for _ in 0..(USUAL_SPAN_S as usize) {
        u.note(9, 4100.0, 1.0);
    }
    for _ in 0..(USUAL_SPAN_S as usize) {
        u.note(9, 3900.0, 1.0);
    }
    let usual = u.usual(9).expect("learned");
    assert!(usual < 3990.0, "{usual}");
    u.note(10, 1.0, 1.0);
    for (mhz, window_s) in [
        (f64::NAN, 1.0),
        (0.0, 1.0),
        (-3900.0, 1.0),
        (3900.0, -1.0),
        (3900.0, f64::INFINITY),
    ] {
        u.note(9, mhz, window_s);
        assert_eq!(
            u.usual(9),
            Some(usual),
            "nothing taught by {mhz} MHz over {window_s} s"
        );
    }
    u.note(3, 3900.0, 0.0);
    for _ in 0..(USUAL_LEARN_S as usize) {
        u.note(3, 4000.0, 1.0);
    }
    assert_eq!(
        u.usual(3),
        Some(4000.0),
        "a zero window taught an unlearned decile"
    );
}

/// `the_e_clock_is_hollow_until_its_bound_holds` (A4 §15): with M-ECLK failed (the M5 as N1
/// measured it) the Efficiency clock is read and drawn hollow, and so it is under every verdict
/// short of a pass (unmeasured, as on every other chip; marginal; Heavy only). CONTROL: a chip
/// whose M-ECLK passed draws it filled.
#[test]
fn the_e_clock_is_hollow_until_its_bound_holds() {
    let cores = fracs(0.5, 0.5);
    let reading = moved(second(2.0, 4000.0, 3.0, 2500.0));
    let v = Engine::default().absorb(&input(
        ThermalPressure::Nominal,
        reading,
        ClockGate::APPLE_M5,
        &cores,
    ));
    let chain = v.chain.expect("judged");
    assert!(
        chain.judged.e_mhz.is_some_and(|e| (e - 2500.0).abs() < 1.0),
        "{chain:?}"
    );
    assert_eq!(chain.e_drawn(), None);
    for e in [Verdict::Unmeasured, Verdict::Marginal, Verdict::HeavyOnly] {
        let gate = ClockGate { e, ..PASSED };
        let v = Engine::default().absorb(&input(ThermalPressure::Nominal, reading, gate, &cores));
        assert_eq!(v.chain.expect("judged").e_drawn(), None, "{e:?}");
    }
    let v = Engine::default().absorb(&input(ThermalPressure::Nominal, reading, PASSED, &cores));
    assert!(v.chain.expect("judged").e_drawn().is_some(), "control");
}

/// `a_clock_above_the_tables_top_is_refused` (A4 §15): the recorded 6.03 GHz Efficiency clock
/// (`rec/heat-clocks.jsonl` i=6) is refused alone, past the table's top: the Super clock and the
/// reading stand. Each kind is held to its own table: an Efficiency clock past the Efficiency top is
/// refused though the Super table reaches it (3.5 GHz), and a Super clock past the Super top
/// (4.7 GHz), or under its own bottom where the Efficiency table's would hold it (600 MHz), is no
/// clock, so the reading is refused. CONTROL: within the table it is given.
#[test]
fn a_clock_above_the_tables_top_is_refused() {
    let m = Counters {
        cpu_ticks: ticks(4_610_000_000),
        qos_ticks: ticks(4_610_000_000),
        p_ticks: ticks(3_660_000_000),
        energy_nj: 12_770_000_000,
        instructions: 40_000_000_000,
        cycles: 13_436_000_000 + 5_728_500_000,
        p_instructions: 30_000_000_000,
        p_cycles: 13_436_000_000,
        ..Counters::default()
    };
    let busy = Busy {
        all_ns: 4_610_000_000,
        p_ns: Some(3_660_000_000),
    };
    let j = judge(&m, SECOND, TB, Some(&M5), Some(busy)).expect("the reading stands");
    assert!(j.p_mhz.is_some_and(|p| (p - 3671.04).abs() < 0.05), "{j:?}");
    assert!(j.e_refused && j.e_mhz.is_none(), "{j:?}");
    let within = Counters {
        cycles: 13_436_000_000 + 2_500_000_000,
        ..m
    };
    let j = judge(&within, SECOND, TB, Some(&M5), Some(busy)).expect("stands");
    assert!(!j.e_refused && j.e_mhz.is_some(), "control: {j:?}");
    // 3 325 M cycles over the 0.95 s of Efficiency time: 3500 MHz, under the Super top's 4553.
    let past_e_top = Counters {
        cycles: 13_436_000_000 + 3_325_000_000,
        ..m
    };
    let j = judge(&past_e_top, SECOND, TB, Some(&M5), Some(busy)).expect("the reading stands");
    assert!(j.e_refused && j.e_mhz.is_none(), "{j:?}");
    // 17 202 M Super cycles over 3.66 s: 4700 MHz; the whole clock (4274 MHz) stays in range.
    let past_p_top = Counters {
        p_cycles: 17_202_000_000,
        cycles: 17_202_000_000 + 2_500_000_000,
        ..m
    };
    assert_eq!(
        judge(&past_p_top, SECOND, TB, Some(&M5), Some(busy)),
        Err("p_clock_range")
    );
    // 2196 M Super cycles over 3.66 s: 600 MHz, under the Super bottom's 654 though over the
    // whole range's 486; the whole clock (1019 MHz) stays in range.
    let under_p_bottom = Counters {
        p_cycles: 2_196_000_000,
        cycles: 2_196_000_000 + 2_500_000_000,
        ..m
    };
    assert_eq!(
        judge(&under_p_bottom, SECOND, TB, Some(&M5), Some(busy)),
        Err("p_clock_range")
    );
}

/// The whole clock reads the QoS time: a cool machine's Super cores near their top, with
/// `cpu_time` at its p5 share of the busy cores (0.882, N1), judges. CONTROL: over `cpu_time` the
/// same reading's whole clock passes the table's top and would be refused.
#[test]
fn the_whole_clock_reads_the_qos_time() {
    let m = Counters {
        cpu_ticks: ticks(3_528_000_000),
        ..second(4.0, 4400.0, 0.0, 0.0)
    };
    let busy = Busy {
        all_ns: 4 * SECOND,
        p_ns: Some(4 * SECOND),
    };
    let j = judge(&m, SECOND, TB, Some(&M5), Some(busy)).expect("judges");
    assert!(j.mhz.is_some_and(|v| (v - 4400.0).abs() < 1.0), "{j:?}");
    let over_cpu_time = m.cycles as f64 * 1e3 / 3_528_000_000.0;
    assert!(
        !ClockRange::holds(over_cpu_time, M5.whole()),
        "control: {over_cpu_time}"
    );
}

/// `a_coalition_layout_that_fails_its_self_check_reads_unread` (A4 §15): a read whose records did
/// not decode, or a change the self-check refused, reads unread with its reason: no watts, no
/// clock, never a zero. CONTROL: the same reading judged gives every value.
#[test]
fn a_coalition_layout_that_fails_its_self_check_reads_unread() {
    let cores = fracs(1.0, 0.5);
    let good = moved(second(4.0, 4000.0, 3.0, 2500.0));
    let mut i = input(ThermalPressure::Nominal, good, PASSED, &cores);
    let v = Engine::default().absorb(&i);
    assert!(
        v.chain.is_ok() && v.watts.compute().is_ok(),
        "control: {v:?}"
    );
    i.moved = Some(Err("layout"));
    let v = Engine::default().absorb(&i);
    assert_eq!(v.chain, Err(NoHeat::Unread("layout")));
    assert_eq!(v.watts.compute(), Err(NoHeat::Unread("layout")));
    let shifted = Counters {
        cpu_ticks: 7,
        ..good.counters
    };
    i.moved = Some(Ok(moved(shifted)));
    let v = Engine::default().absorb(&i);
    assert_eq!(v.chain, Err(NoHeat::Unread("busy_time")));
    assert!(v.watts.cpu.is_err() && v.watts.gpu.is_err() && v.watts.ane.is_err());
}

/// The chain's absences are explicit: a system with no coalitions has none (absent), and its
/// watts are its own readers' (here none); a lane off reads unread; a system with no documented
/// pressure level has none.
#[test]
fn every_absence_names_its_reason() {
    // Two Super and three Efficiency cores busy over the second, as the ticks say.
    let cores = fracs(0.5, 0.5);
    let mut i = input(
        ThermalPressure::Nominal,
        moved(second(2.0, 4000.0, 3.0, 2500.0)),
        PASSED,
        &cores,
    );
    i.lane = CoalLane::Unsupported;
    i.pressure = None;
    i.moved = None;
    let v = Engine::default().absorb(&i);
    assert_eq!(v.chain, Err(NoHeat::NotSupported("no_coalitions")));
    assert_eq!(v.pressure, Err(NoHeat::NotSupported("no_pressure_level")));
    assert_eq!(
        v.watts.compute(),
        Err(NoHeat::NotSupported("no_power_reader"))
    );
    i.lane = CoalLane::Off;
    i.pressure = Some(Err("state"));
    assert_eq!(
        Engine::default().absorb(&i).chain,
        Err(NoHeat::Unread("lane_off"))
    );
    assert_eq!(
        Engine::default().absorb(&i).pressure,
        Err(NoHeat::Unread("state"))
    );
    i.lane = CoalLane::On;
    assert_eq!(
        Engine::default().absorb(&i).chain,
        Err(NoHeat::Unread("no_heat_read"))
    );
    // With no clock table the chain stands, its clocks absent and saying why; its watts stand.
    i.moved = Some(Ok(moved(second(2.0, 4000.0, 3.0, 2500.0))));
    for (range, why) in [
        (None, NoHeat::NotSupported("no_clock_table")),
        (Some(Err("pmgr")), NoHeat::Unread("pmgr")),
    ] {
        i.range = range;
        let v = Engine::default().absorb(&i);
        let chain = v.chain.expect("judged");
        assert_eq!(chain.range, Err(why));
        assert_eq!((chain.judged.p_mhz, chain.judged.e_mhz), (None, None));
        assert!(v.watts.compute().is_ok());
    }
}

/// With one kind of core (or cores read over another window) the reading has no decile, claims no
/// cut and teaches no usual; its watts and clocks stand.
#[test]
fn no_decile_without_the_super_cores_over_the_window() {
    let cores = fracs(1.0, 1.0);
    let reading = moved(second(4.0, 4000.0, 6.0, 2900.0));
    let mut one_kind = input(ThermalPressure::Heavy, reading, PASSED, &cores);
    one_kind.p_cores = None;
    let mut e = Engine::default();
    let chain = e.absorb(&one_kind).chain.expect("judged");
    assert_eq!((chain.decile, chain.cut), (None, Cut::Unread));
    let mut stale = input(ThermalPressure::Nominal, reading, PASSED, &cores);
    stale.cores = Some((&cores, 2 * SECOND));
    let chain = e
        .absorb(&stale)
        .chain
        .expect("judged without the busy time");
    assert_eq!(chain.decile, None);
    assert_eq!(e.usual, UsualClocks::default(), "nothing taught");
    assert!(busy_of(&cores, Some(&P_CORES), SECOND + SECOND / 5, SECOND).is_some());
    assert!(busy_of(&cores, Some(&P_CORES), SECOND + SECOND / 3, SECOND).is_none());
    assert_eq!(
        busy_of(&cores, None, SECOND, SECOND).map(|b| b.p_ns),
        Some(None),
        "one kind of core checks no Super time"
    );
}

/// Work per joule and the Super shares come from the same change: the time share over the QoS
/// time, never `cpu_time` (here at 0.926 of it, N1's p50), and the instruction share over the
/// instructions, never the cycles (here the Super cores retire 3 a cycle, the Efficiency cores 2).
#[test]
fn work_per_joule_and_the_super_shares() {
    let even = second(2.0, 4000.0, 2.0, 2000.0);
    let c = Counters {
        cpu_ticks: ticks(3_704_000_000),
        instructions: 3 * even.p_cycles + 2 * (even.cycles - even.p_cycles),
        p_instructions: 3 * even.p_cycles,
        ..even
    };
    let j = judge(&c, SECOND, TB, Some(&M5), None).expect("judges");
    let want_ipj = c.instructions as f64 / (c.energy_nj as f64 / 1e9);
    assert!(j.ipj.is_some_and(|v| (v - want_ipj).abs() < 1.0), "{j:?}");
    assert!(
        j.p_time_share.is_some_and(|v| (v - 0.5).abs() < 1e-9),
        "{j:?}"
    );
    let p_share = j.p_instruction_share.expect("timed");
    assert!((p_share - 24.0 / 32.0).abs() < 1e-9, "{p_share}");
    assert!((j.cpu_w - 4.0).abs() < 1e-9);
    let idle = Counters::default();
    let j = judge(&idle, SECOND, TB, Some(&M5), None).expect("an idle machine judges");
    assert_eq!((j.ipj, j.p_mhz, j.ipc), (None, None, None));
    assert_eq!(j.cpu_w, 0.0);
}

fn book(at_ns: u64, rows: &[(u64, Counters)]) -> HeatBook {
    let by_id: Vec<(u64, Words)> = rows.iter().map(|(id, c)| (*id, c.words())).collect();
    let mut total = [0u64; COUNTERS];
    for (_, w) in &by_id {
        for (t, v) in total.iter_mut().zip(w) {
            *t = t.wrapping_add(*v);
        }
    }
    HeatBook {
        at_ns,
        timebase: TB,
        by_id,
        total: Ok(total),
    }
}

fn plus(a: Counters, b: Counters) -> Counters {
    let mut w = a.words();
    for (x, y) in w.iter_mut().zip(b.words()) {
        *x += y;
    }
    Counters::from_words(w)
}

/// The change is the later total less what the earlier kept: each matched coalition's change plus
/// each born one whole, a gone one's last counters taken off the earlier total. CONTROL: with the
/// gone coalition left on it, the counters run backwards.
#[test]
fn the_change_takes_the_gone_off_the_earlier_total() {
    let a = second(1.0, 3000.0, 1.0, 2000.0);
    let b = second(2.0, 4000.0, 0.0, 0.0);
    let born = second(0.0, 0.0, 1.0, 1000.0);
    let then = book(SECOND, &[(30, a), (10, b), (20, a)]);
    let now = book(
        2 * SECOND,
        &[(30, plus(a, a)), (20, plus(a, born)), (40, born)],
    );
    let m = change(&then, &now, &[10]).expect("moves");
    assert_eq!(m.window_ns, SECOND);
    assert_eq!(m.counters, plus(plus(a, born), born));
    assert_eq!(change(&then, &now, &[]), Err("backwards"), "control");
    assert_eq!(change(&then, &now, &[11]), Err("gone_unread"));
}

/// Either read's refusal, another timebase or a window that does not run forward (backwards, or no
/// time at all: two reads at one instant) refuses the change.
#[test]
fn a_refused_read_refuses_the_change() {
    let a = second(1.0, 3000.0, 1.0, 2000.0);
    let then = book(SECOND, &[(10, a)]);
    let now = book(2 * SECOND, &[(10, plus(a, a))]);
    assert!(change(&then, &now, &[]).is_ok(), "control");
    let refused = HeatBook {
        total: Err("unwritten"),
        ..now.clone()
    };
    assert_eq!(change(&then, &refused, &[]), Err("unwritten"));
    assert_eq!(change(&refused, &now, &[]), Err("unwritten"));
    let other = HeatBook {
        timebase: (1, 1),
        ..now.clone()
    };
    assert_eq!(change(&then, &other, &[]), Err("timebase"));
    assert_eq!(change(&now, &then, &[]), Err("window"));
    let same_instant = HeatBook {
        at_ns: then.at_ns,
        ..now.clone()
    };
    assert_eq!(change(&then, &same_instant, &[]), Err("window"));
}

/// Compute watts is the CPU's, the GPU's and the Neural Engine's; a part this system has no reader
/// of adds nothing, and an unread one leaves the figure unread.
#[test]
fn compute_watts_sum_what_was_read() {
    let w = Watts {
        cpu: Ok(4.0),
        gpu: Ok(1.5),
        ane: Ok(0.5),
    };
    assert_eq!(w.compute(), Ok(6.0));
    let gpu_absent = Watts {
        gpu: Err(NoHeat::NotSupported("no_gpu_power")),
        ane: Err(NoHeat::NotSupported("no_neural_engine")),
        ..w
    };
    assert_eq!(gpu_absent.compute(), Ok(4.0));
    let gpu_unread = Watts {
        gpu: Err(NoHeat::Unread("nvml")),
        ..w
    };
    assert_eq!(gpu_unread.compute(), Err(NoHeat::Unread("nvml")));
    let cpu_absent = Watts {
        cpu: Err(NoHeat::NotSupported("rapl_root_only")),
        ..w
    };
    assert_eq!(
        cpu_absent.compute(),
        Err(NoHeat::NotSupported("rapl_root_only"))
    );
}

/// The gate is the chip's: N1's verdicts for an Apple M5, nothing measured for any other.
#[test]
fn the_gate_is_the_chips() {
    assert_eq!(ClockGate::of_chip(Some("Apple M5")), ClockGate::APPLE_M5);
    for other in [Some("Apple M5 Max"), Some("Apple M4"), None] {
        assert_eq!(ClockGate::of_chip(other), ClockGate::default(), "{other:?}");
    }
}

/// `base` × `k`: a coalition's lifetime counters after `k` seconds of the same work.
fn times(base: Counters, k: u64) -> Counters {
    Counters::from_words(base.words().map(|w| w * k))
}

/// The model's reading `k` (one a second): four Super cores busy and six Efficiency cores half
/// busy, one coalition doing `base` a second, at `pressure`.
fn reading(k: u64, base: Counters, pressure: ThermalPressure) -> crate::model::Sample {
    use crate::model::{ClusterSpec, Sample, Ticks};
    use crate::starts::{CoalTaskRead, CoalTasks, StartCount};
    let tasks = CoalTaskRead::of(
        Some(1),
        false,
        [CoalTasks {
            id: 1,
            started: 1,
            exited: 0,
        }],
    )
    .with_heat(book(k * SECOND, &[(1, times(base, k))]));
    Sample {
        at_ns: k * SECOND,
        at_unix_ms: 1_800_000_000_000 + i64::try_from(k * 1000).expect("small"),
        mono_ms: k * 1000,
        cadence_ms: 1000,
        cores: Some(
            (0..10)
                .map(|c| Ticks {
                    busy: k * if c < 4 { 100 } else { 50 },
                    total: k * 100,
                })
                .collect(),
        ),
        clusters: vec![
            ClusterSpec {
                name: "Super".into(),
                logical: vec![0, 1, 2, 3],
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: (4..10).collect(),
            },
        ],
        starts: Some(StartCount::Coalitions {
            read: tasks,
            own: None,
        }),
        coal_lane: CoalLane::On,
        thermal_pressure: Some(Ok(pressure)),
        clock_range: Some(Ok(M5)),
        cpu_brand: Some("Apple M5".into()),
        ..Sample::default()
    }
}

/// One engine (A4 §12.5): the heat chain enters through the model's own reading and the start
/// counter's one merge, and the Snapshot carries it: unread on the first reading, judged from the
/// second, its gates N1's for this chip.
#[test]
fn the_snapshot_carries_the_heat_chain() {
    let base = second(4.0, 4000.0, 3.0, 2500.0);
    let mut v = crate::model::Vitals::new();
    let first = v.absorb(&reading(1, base, ThermalPressure::Nominal));
    assert_eq!(first.heat.chain, Err(NoHeat::Unread("warming")));
    assert_eq!(first.heat.pressure, Ok(ThermalPressure::Nominal));
    let snap = v.absorb(&reading(2, base, ThermalPressure::Moderate));
    let chain = snap.heat.chain.expect("judged");
    assert_eq!(chain.counters, base);
    assert_eq!(chain.decile, Some(9));
    assert!(
        chain
            .judged
            .p_mhz
            .is_some_and(|p| (p - 4000.0).abs() < 0.01)
    );
    assert_eq!(chain.p_verdict, Verdict::HeavyOnly);
    assert_eq!(chain.cut, Cut::Gated(Verdict::HeavyOnly));
    assert_eq!(chain.e_drawn(), None, "M-ECLK failed on this chip");
    assert!(
        snap.heat
            .watts
            .compute()
            .is_ok_and(|w| (w - 7.0).abs() < 1e-9)
    );
    assert_eq!(snap.heat.steps(), 1);
}

/// A system with no coalitions and no documented pressure level (Linux's forks) carries the
/// heat chain as absent, never as zeros, and its own watts readers' readings: compute watts from
/// RAPL and NVML where both read, absent with RAPL root's alone (A4 §4), and none at all with
/// neither reader (Windows). CONTROL: on a system whose coalitions read, the host's readers are
/// not taken (the chain's own watts stand).
#[test]
fn a_system_without_coalitions_carries_the_chain_absent_and_its_own_watts() {
    let linux = HostWatts {
        cpu: Ok(35.0),
        gpu: Ok(31.5),
    };
    let mut v = crate::model::Vitals::new();
    for k in 1..=2u64 {
        let s = crate::model::Sample {
            starts: Some(crate::starts::StartCount::Forks(k * 10)),
            thermal_pressure: None,
            coal_lane: CoalLane::Unsupported,
            host_watts: linux,
            ..reading(k, Counters::default(), ThermalPressure::Nominal)
        };
        let snap = v.absorb(&s);
        assert_eq!(snap.heat.chain, Err(NoHeat::NotSupported("no_coalitions")));
        assert_eq!(
            snap.heat.pressure,
            Err(NoHeat::NotSupported("no_pressure_level"))
        );
        assert_eq!(snap.heat.watts.compute(), Ok(66.5));
    }
    let cores = fracs(0.5, 0.5);
    let mut i = input(
        ThermalPressure::Nominal,
        moved(second(2.0, 4000.0, 3.0, 2500.0)),
        PASSED,
        &cores,
    );
    i.lane = CoalLane::Unsupported;
    i.host = HostWatts {
        cpu: Err(NoHeat::NotSupported("rapl_needs_root")),
        ..linux
    };
    let w = Engine::default().absorb(&i).watts;
    assert_eq!(
        (w.gpu, w.compute()),
        (Ok(31.5), Err(NoHeat::NotSupported("rapl_needs_root")))
    );
    i.host = HostWatts::default();
    assert_eq!(
        Engine::default().absorb(&i).watts.compute(),
        Err(NoHeat::NotSupported("no_power_reader"))
    );
    i.lane = CoalLane::On;
    i.host = linux;
    let chain = Engine::default().absorb(&i).watts;
    assert_eq!(
        (chain.cpu, chain.compute()),
        (Ok(5.0), Ok(5.0)),
        "control: the chain's own"
    );
}
