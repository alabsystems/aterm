// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the quota view ([`super`]): `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2 and
//! slice S3 of its §6, on the outlook tests' synthetic fixtures, at the design's now (Thu 5 Nov
//! 2026 17:00, UTC) or theirs.

use std::collections::BTreeSet;

use super::*;
use crate::harness::account_progress::{AccountProgress, ProgressPhase};
use crate::harness::accounts::{self, Class};
use crate::harness::footer::FooterFacts;
use crate::harness::outlook::tests::{SAT8, scenario_b, scenario_c, scenario_d, worst_case};
use crate::harness::outlook::{Refusal, round_down};
use crate::harness::quota_scenes::{
    D, Fx, H, MIN, NOW, SAT7, SAT14, T5, case_a, case_b, case_c, case_d,
};

/// Both accounts walled now at 97%: the work waits until main's reset.
fn walled() -> Fx {
    let mut fx = Fx::new(&[("main", Class::Preferred), ("alt1", Class::Preferred)]);
    fx.now = T5;
    for (label, reset) in [("main", SAT7), ("alt1", SAT7 + D)] {
        fx.read(label, None, Some((970, reset)), T5 - 10 * MIN);
        accounts::note_wall(
            &mut fx.state,
            label,
            accounts::WEEK_LIMIT,
            Some(reset),
            T5,
            T5,
        );
    }
    fx
}

/// Case (b) with usage credits on: the stop is spent on credits.
fn credits() -> Fx {
    let mut fx = case_b();
    fx.credits = true;
    fx
}

/// Scenario C with `pool1` read at 99% five hours ago: both branches of the lean stop, so the
/// stop stands whatever pool1 holds (`Verdict::Stop { pass: Week, lean: Some("pool1") }`), and
/// it lies in the lean's worst branch, not among the week pass's own stops.
fn lean_stops() -> Fx {
    let mut fx = scenario_c();
    fx.read("pool1", None, Some((990, SAT8)), NOW - 5 * H);
    fx
}

/// `main` walled now on its week, `alt1` at 30% and read now: the policy moves the work to alt1
/// at the turn's end (`Walled { to: Some("alt1") }`).
fn moves() -> Fx {
    let mut fx = Fx::new(&[("main", Class::Preferred), ("alt1", Class::Preferred)]);
    fx.now = T5;
    fx.read("main", None, Some((970, SAT7)), T5 - 10 * MIN);
    accounts::note_wall(
        &mut fx.state,
        "main",
        accounts::WEEK_LIMIT,
        Some(SAT7),
        T5,
        T5,
    );
    fx.read("alt1", None, Some((300, SAT7 + D)), T5);
    fx
}

/// Case (b) walled now on its 5-hour window, which resets at 19:00: the one account waits until
/// then, its wall standing to the reset's grace (19:02:30).
fn five_wall() -> Fx {
    let mut fx = case_b();
    accounts::note_wall(
        &mut fx.state,
        "main",
        accounts::SESSION_LIMIT,
        Some(T5 + 2 * H),
        T5,
        T5,
    );
    fx
}

/// Case (b) as [`five_wall`], its 5-hour window read too (30% two hours ago, full now), so a
/// working pass runs: it begins with the wait itself, and at 35% an hour the next window walls
/// again within hours.
fn five_wall_working() -> Fx {
    let mut fx = case_b();
    fx.fig(
        "main",
        T5 - 2 * H,
        Some((300, T5 + 2 * H)),
        Some((600, SAT7)),
    );
    fx.read("main", Some((1000, T5 + 2 * H)), Some((630, SAT7)), T5);
    accounts::note_wall(
        &mut fx.state,
        "main",
        accounts::SESSION_LIMIT,
        Some(T5 + 2 * H),
        T5,
        T5,
    );
    fx
}

/// `me` alone, its week an hour old (its pace refused, `no pace yet`), walled now on its 5-hour
/// window, which resets at 21:30: nothing is projected, and the work waits.
fn early_wall() -> Fx {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    let reset = NOW + 7 * D - H;
    fx.fig("me", NOW - 40 * MIN, None, Some((10, reset)));
    fx.read("me", None, Some((30, reset)), NOW);
    accounts::note_wall(
        &mut fx.state,
        "me",
        accounts::SESSION_LIMIT,
        Some(NOW + 2 * H),
        NOW,
        NOW,
    );
    fx
}

/// Scenario D with a pool row, `pool1`, read at 99.9% of its week five hours ago: at alt1's
/// 5-hour wall the working pass leans on pool1, and both branches stop, so the stop stands
/// whatever pool1 holds (`Verdict::Stop { pass: Working, lean: Some("pool1") }`); it lies in the
/// lean's worst branch, not among the working pass's own stops.
fn working_lean_stops() -> Fx {
    let mut fx = scenario_d();
    fx.roster.rows.push(accounts::Row {
        label: "pool1".into(),
        class: Class::Pool,
        dir: Some("/logins/pool1".into()),
        ..accounts::Row::default()
    });
    fx.enrolled.insert("pool1".into());
    fx.read("pool1", None, Some((999, SAT8)), NOW - 5 * H);
    fx
}

/// `me`'s week began an hour ago, read for 40 minutes (the outlook tests' too-early case), so
/// its pace is refused (`no pace yet`); `alt1` is read now.
fn too_early() -> Fx {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    let reset = NOW + 7 * D - H;
    fx.fig("me", NOW - 40 * MIN, None, Some((10, reset)));
    fx.read("me", None, Some((30, reset)), NOW);
    fx.read("alt1", None, Some((300, reset)), NOW);
    fx
}

/// A roster label longer than a name's twelve cells (labels are ASCII, one cell a character).
const LONG: &str = "a-long-account-label-0123456789";

/// [`LONG`]'s account at 50% and `alt1` at 30%, both read now.
fn long_label() -> Fx {
    let mut fx = Fx::new(&[(LONG, Class::Preferred), ("alt1", Class::Preferred)]);
    fx.now = T5;
    fx.read(LONG, None, Some((500, SAT7)), T5);
    fx.read("alt1", None, Some((300, SAT7 + D)), T5);
    fx
}

/// Every scenario, named, with its outlook and its tab: the design's four cases, a gap, a lean,
/// a 5-hour stop, a lean whose worst case stops, one whose both branches stop (in the week pass
/// and in the working pass), credits, a wall on either tab, a wall the work moves off, a 5-hour
/// wall (with no working pass, with one, and under a refused pace), a refused pace on either
/// tab, and a label longer than a name.
fn outlooks() -> Vec<(&'static str, Outlook, &'static str)> {
    [
        ("a", case_a(), "main"),
        ("b", case_b(), "main"),
        ("c", case_c(), "main"),
        ("d", case_d(), "main"),
        ("gap", scenario_b(), "me"),
        ("lean", scenario_c(), "me"),
        ("lean-stop", lean_stops(), "me"),
        ("working", scenario_d(), "me"),
        ("worst", worst_case(), "me"),
        ("credits", credits(), "main"),
        ("wall", walled(), "main"),
        ("wall-alt1", walled(), "alt1"),
        ("moves", moves(), "main"),
        ("five-wall", five_wall(), "main"),
        ("five-wall-working", five_wall_working(), "main"),
        ("early-wall", early_wall(), "me"),
        ("working-lean-stop", working_lean_stops(), "me"),
        ("early", too_early(), "me"),
        ("early-alt1", too_early(), "alt1"),
        ("long", long_label(), LONG),
    ]
    .into_iter()
    .map(|(name, fx, tab)| (name, fx.outlook(), tab))
    .collect()
}

/// Every scenario's view ([`outlooks`]).
fn scenarios() -> Vec<(&'static str, QuotaView)> {
    outlooks()
        .into_iter()
        .map(|(name, out, tab)| (name, out.quota(tab, None, true)))
        .collect()
}

/// The view laid at `room`, where a rung fits.
fn laid(view: &QuotaView, room: usize) -> Option<QuotaLine> {
    let mut line = QuotaLine::default();
    view.lay(room, &mut line).then_some(line)
}

/// The words the glass says of `view` now: the row at every room, the title, the spoken words and
/// the fallback unit's forms.
fn words_now(view: &QuotaView) -> Vec<String> {
    let mut out: Vec<String> = (0..=200)
        .filter_map(|room| laid(view, room))
        .map(|line| line.text())
        .collect();
    out.push(view.title.clone());
    out.push(view.spoken.clone());
    out.extend(
        view.fallback()
            .iter()
            .map(|form| form.iter().map(|p| p.text.as_str()).collect::<String>()),
    );
    out
}

/// Every word the glass says of `view`: [`words_now`], and every chart column's title at every
/// grain (on UTC), a past column's naming the reading it holds as read then.
fn every_word(view: &QuotaView) -> Vec<String> {
    let mut out = words_now(view);
    let utc = Offsets::fixed(0);
    for per_day in PER_DAY {
        let n = cells(&view.chart, per_day).len();
        out.extend((0..n).filter_map(|cell| view.column_title(per_day, cell, &utc)));
    }
    out
}

/// The chart's cells at `per_day` cells a day.
fn cells(chart: &Chart, per_day: u8) -> Vec<Cell> {
    let mut out = Vec::new();
    chart.cells(per_day, &mut out);
    out
}

/// The chart's future cells (after `▕`) at `per_day` cells a day.
fn future(chart: &Chart, per_day: u8) -> Vec<Cell> {
    cells(chart, per_day).split_off(usize::from(per_day) + 1)
}

/// A chart at the design's now with `main` carrying the whole week at `x10`.
fn flat(x10: u16) -> Chart {
    Chart {
        now: T5,
        runs: vec![Run {
            label: "main".into(),
            from: T5,
            to: T5 + 7 * D,
            knots: vec![
                Knot {
                    t: T5,
                    x10: Some(x10),
                },
                Knot {
                    t: T5 + 7 * D,
                    x10: Some(x10),
                },
            ],
        }],
        ..Chart::default()
    }
}

/// A reading of the week that resets Sat 10:00.
fn read(t: i64, x10: u16) -> Read {
    Read {
        t,
        x10,
        reset: SAT7,
        epoch: None,
    }
}

fn labels(view: &QuotaView) -> (Vec<&str>, Vec<&str>) {
    (
        view.named.iter().map(|n| n.label.as_str()).collect(),
        view.folded.iter().map(String::as_str).collect(),
    )
}

// ---------------------------------------------------------------------------------------------
// Selection (§2.5)

/// THE NAMED THREE FOLLOW ROSTER ORDER: `Class::Preferred` rows first, then file order; one to
/// three rows are all named; past three, the first three are named and the rest fold, in that
/// order; with no roster row the one login is named `this login`. The glass names accounts by
/// their roster labels, so no email reaches the row, the title, the spoken words or the
/// fallback. Negative control: ordering by file order alone names the pool row `r0` among six.
#[test]
fn the_named_three_follow_roster_order() {
    use Class::{Pool, Preferred};
    let six = [Pool, Preferred, Pool, Preferred, Preferred, Pool];
    for n in [0usize, 1, 2, 3, 6] {
        let names: Vec<String> = (0..n).map(|i| format!("r{i}")).collect();
        let rows: Vec<(&str, Class)> = names
            .iter()
            .enumerate()
            .map(|(i, l)| (l.as_str(), if n == 6 { six[i] } else { Preferred }))
            .collect();
        let mut fx = Fx::new(&rows);
        fx.now = T5;
        for (i, label) in names.iter().enumerate() {
            fx.roster.rows[i].email = Some(format!("{label}.person@example.test"));
            let x10 = u32::try_from(100 * (i + 1)).unwrap();
            fx.read(label, None, Some((x10, SAT7)), T5);
        }
        let tab = if n == 6 { "r1" } else { "r0" };
        fx.state.active = (n > 0).then(|| tab.to_owned());
        let view = fx
            .outlook()
            .quota(tab, (n == 0).then_some(Figure::Exact(500)), true);
        let want: (Vec<&str>, Vec<&str>) = match n {
            0 => (vec![THIS_LOGIN], vec![]),
            1 => (vec!["r0"], vec![]),
            2 => (vec!["r0", "r1"], vec![]),
            3 => (vec!["r0", "r1", "r2"], vec![]),
            _ => (vec!["r1", "r3", "r4"], vec!["r0", "r2", "r5"]),
        };
        assert_eq!(labels(&view), want, "{n} rows");
        for words in every_word(&view) {
            assert!(
                !words.contains('@') && !words.contains("person"),
                "no email on the glass: {words}"
            );
        }
    }
}

/// THIS TAB AND THE CARRIER ARE ALWAYS NAMED: among six preferred rows, one outside the first
/// three takes the place of the last named row that is neither, the named stay in roster order,
/// and the card's lanes are the same accounts. Negative control: naming the first three alone
/// folds the carrier ranked fifth (`r4`).
#[test]
fn this_tab_and_the_carrier_are_always_named() {
    let names = ["r0", "r1", "r2", "r3", "r4", "r5"];
    let rows: Vec<(&str, Class)> = names.iter().map(|l| (*l, Class::Preferred)).collect();
    for (tab, carrier, named, folded) in [
        ("r5", "r4", ["r0", "r4", "r5"], ["r1", "r2", "r3"]),
        ("r5", "r0", ["r0", "r1", "r5"], ["r2", "r3", "r4"]),
        ("r0", "r4", ["r0", "r1", "r4"], ["r2", "r3", "r5"]),
        ("r3", "r3", ["r0", "r1", "r3"], ["r2", "r4", "r5"]),
    ] {
        let mut fx = Fx::new(&rows);
        fx.now = T5;
        for (i, label) in names.iter().enumerate() {
            let x10 = u32::try_from(100 * (i + 1)).unwrap();
            fx.read(label, None, Some((x10, SAT7)), T5);
        }
        fx.state.active = Some(carrier.to_owned());
        let out = fx.outlook();
        assert_eq!(out.carrier.as_deref(), Some(carrier));
        let view = out.quota(tab, None, true);
        assert_eq!(
            labels(&view),
            (named.to_vec(), folded.to_vec()),
            "{tab}, {carrier}"
        );
        assert!(view.named.iter().any(|n| n.this_tab && n.label == tab));
        assert!(view.named.iter().any(|n| n.carrier && n.label == carrier));
        let lanes: Vec<&str> = view.lanes.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(lanes, named, "the card's lanes are the named three");
    }
}

/// NO ROSTER ROW IS EVER DROPPED: for rosters of none to seven rows, every tab (and a login the
/// roster cannot name) and every carrier (and none), the named and the folded are disjoint, in
/// roster order, and together the whole roster; at most three are named; and the widest lead
/// counts every folded row in its `+N`. Negative control: a pick that keeps the first three and
/// drops the rest (no `folded`) loses rows past the third.
#[test]
fn no_roster_row_is_ever_dropped() {
    for n in 0..=7usize {
        let names: Vec<String> = (0..n).map(|i| format!("r{i}")).collect();
        let rows: Vec<(&str, Class)> = names
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let class = if i % 3 == 1 {
                    Class::Pool
                } else {
                    Class::Preferred
                };
                (l.as_str(), class)
            })
            .collect();
        let mut fx = Fx::new(&rows);
        fx.now = T5;
        for label in &names {
            fx.read(label, None, Some((300, SAT7)), T5);
        }
        let base = fx.outlook();
        let tabs = names.iter().map(String::as_str).chain(["unnamed"]);
        for tab in tabs {
            for carrier in names.iter().map(Some).chain([None]) {
                let mut out = base.clone();
                out.carrier = carrier.cloned();
                let view = out.quota(tab, Some(Figure::Exact(300)), true);
                let (named, folded) = labels(&view);
                assert!(named.len() <= NAMED_MAX);
                let mut all: Vec<&str> = named.iter().chain(&folded).copied().collect();
                let roster: BTreeSet<&str> = names.iter().map(String::as_str).collect();
                let mut want: BTreeSet<&str> = roster.clone();
                if !roster.contains(tab) {
                    want.insert(THIS_LOGIN);
                }
                all.sort_unstable();
                let distinct: BTreeSet<&str> = all.iter().copied().collect();
                assert_eq!(distinct.len(), all.len(), "disjoint: {named:?} {folded:?}");
                assert_eq!(distinct, want, "{n} rows, tab {tab}, carrier {carrier:?}");
                let order: Vec<&str> = roster_order(&out.rows)
                    .into_iter()
                    .map(|i| out.rows[i].label.as_str())
                    .collect();
                let rank = |l: &str| order.iter().position(|o| *o == l);
                for list in [&named, &folded] {
                    let ranks: Vec<_> = list.iter().filter_map(|l| rank(l)).collect();
                    assert!(ranks.windows(2).all(|w| w[0] < w[1]), "{list:?}");
                }
                if let Some(line) = laid(&view, 200)
                    && !view.folded.is_empty()
                {
                    let lead = line.text_of(&line.lead);
                    assert!(
                        lead.ends_with(&format!(" +{}", view.folded.len())),
                        "{lead}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cells (§2.2)

/// A CELL IS A GAP WHEN A STOP COVERS HALF OF IT: at 12 cells a day (2 h a cell), a stop from 1 h
/// to 5 h takes cells 0 to 2 (one hour of cell 0, all of cell 1, one hour of cell 2); one from
/// 21 h and a second to 25 h covers just under half of cell 10, which keeps its level, all of 11
/// and half of 12. A credits span covers by the same rule and draws `$`. Negative control: a
/// stop that marks every cell it overlaps (`over > 0`) makes cell 10 a gap.
#[test]
fn a_cell_is_a_gap_when_a_stop_covers_half_of_it() {
    let mut chart = flat(500);
    let level = Cell::Ahead(level(500));
    chart.stops = vec![
        (T5 + H, T5 + 5 * H, false),
        (T5 + 21 * H + 1, T5 + 25 * H, false),
    ];
    let f = future(&chart, 12);
    assert_eq!(f[..4], [Cell::Gap, Cell::Gap, Cell::Gap, level]);
    assert_eq!(f[10..14], [level, Cell::Gap, Cell::Gap, level]);
    chart.stops = vec![(T5 + 21 * H + 1, T5 + 25 * H, true)];
    let f = future(&chart, 12);
    assert_eq!(f[10..14], [level, Cell::Credits, Cell::Credits, level]);
}

/// EVERY STOP TAKES A CELL: at every grain, a ten-minute stop anywhere inside a cell (or one hour
/// across a boundary, under half of either cell) covers no cell half, so it takes the cell that
/// holds its start, and that cell alone. Negative control: dropping the start-cell rule leaves
/// no `╳` for a ten-minute stop.
#[test]
fn every_stop_takes_a_cell() {
    for per_day in PER_DAY {
        let s = DAY / i64::from(per_day);
        for into in [0, 10 * MIN, s / 2 - 1, s - 10 * MIN] {
            let mut chart = flat(500);
            let from = T5 + 5 * s + into;
            chart.stops = vec![(from, from + 10 * MIN, false)];
            let gaps: Vec<usize> = future(&chart, per_day)
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == Cell::Gap)
                .map(|(j, _)| j)
                .collect();
            assert_eq!(gaps, [5], "{per_day} a day, {into} s into the cell");
        }
        let mut chart = flat(500);
        chart.stops = vec![(T5 + s - 30 * MIN, T5 + s + 30 * MIN, false)];
        let f = future(&chart, per_day);
        assert_eq!((f[0], f[1]), (Cell::Gap, Cell::Ahead(level(500))));
    }
}

/// A HAND-OFF IS ALWAYS DRAWN: at every grain, the `→` of each hand-off stands in the cell that
/// holds it (the first future cell for a carrier that is not this tab's), and one a gap
/// displaces (the work moves to alt2 inside every account's stop) moves to the first cell after
/// the gap, so a drop in level never reads as a refill; at 12 cells a day the stop takes cells
/// 15 to 19 and alt2's `→` stands in cell 20, its name after it. Negative control: a displaced
/// `→` that is dropped rather than moved leaves cell 20 a level.
#[test]
fn a_hand_off_is_always_drawn() {
    let run = |label: &str, from: i64, to: i64, a: u16, b: u16| Run {
        label: label.into(),
        from,
        to,
        knots: vec![
            Knot {
                t: from,
                x10: Some(a),
            },
            Knot {
                t: to,
                x10: Some(b),
            },
        ],
    };
    let chart = Chart {
        now: T5,
        runs: vec![
            run("alt1", T5, T5 + 30 * H, 500, 1000),
            run("alt2", T5 + 40 * H, T5 + 60 * H, 100, 400),
            run("main", T5 + 60 * H, T5 + 7 * D, 0, 600),
        ],
        stops: vec![(T5 + 30 * H, T5 + 40 * H, false)],
        marks: vec![
            (T5, Mark::Handoff("alt1".into())),
            (T5 + 31 * H, Mark::Handoff("alt2".into())),
            (T5 + 60 * H, Mark::Handoff("main".into())),
        ],
        ..Chart::default()
    };
    for per_day in PER_DAY {
        let s = DAY / i64::from(per_day);
        let f = future(&chart, per_day);
        for (t, _) in &chart.marks {
            let mut j = usize::try_from((t - T5) / s).unwrap();
            while f[j] == Cell::Gap {
                j += 1;
            }
            assert_eq!(f[j], Cell::Handoff, "{per_day} a day: the hand-off at {t}");
        }
        let arrows = f.iter().filter(|c| **c == Cell::Handoff).count();
        assert_eq!(arrows, 3, "{per_day} a day");
    }
    let f = future(&chart, 12);
    assert!(f[15..20].iter().all(|c| *c == Cell::Gap), "{f:?}");
    assert_eq!(
        f[20..25],
        [
            Cell::Handoff,
            Cell::Name('a'),
            Cell::Name('l'),
            Cell::Name('t'),
            Cell::Name('2')
        ]
    );
}

/// A NAME IS DRAWN ONLY WHERE ITS RUN HOLDS IT: at 12 cells a day, `→alt1` at now has five plain
/// cells before the next mark, its name and one more, so it reads `→alt1`; `→alt2` six hours on
/// has four, just its name, so it stands alone; `→main` ten hours on has the rest of the week.
/// A lean's `→pool1?` needs its `?` too. Negative control: a name drawn where the run holds just
/// the name (`room >= len`) draws `→alt2` flush against `→main`.
#[test]
fn a_name_is_drawn_only_where_its_run_holds_it() {
    let flat_run = |label: &str, from: i64, to: i64| Run {
        label: label.into(),
        from,
        to,
        knots: vec![
            Knot {
                t: from,
                x10: Some(300),
            },
            Knot {
                t: to,
                x10: Some(300),
            },
        ],
    };
    let mut chart = Chart {
        now: T5,
        runs: vec![
            flat_run("alt1", T5, T5 + 12 * H),
            flat_run("alt2", T5 + 12 * H, T5 + 22 * H),
            flat_run("main", T5 + 22 * H, T5 + 7 * D),
        ],
        marks: vec![
            (T5, Mark::Handoff("alt1".into())),
            (T5 + 12 * H, Mark::Handoff("alt2".into())),
            (T5 + 22 * H, Mark::Handoff("main".into())),
        ],
        ..Chart::default()
    };
    let text = |chart: &Chart| -> String { future(chart, 12).iter().map(|c| c.glyph()).collect() };
    let l = Cell::Ahead(level(300)).glyph();
    let want = format!("\u{2192}alt1{l}\u{2192}{l}{l}{l}{l}\u{2192}main{l}");
    assert!(text(&chart).starts_with(&want), "{}", text(&chart));
    chart.runs.truncate(1);
    chart.marks = vec![(T5 + 2 * H, Mark::Lean("pool1".into()))];
    chart.unknown_from = Some(T5 + 2 * H);
    let lean: String = text(&chart).chars().take(10).collect();
    assert_eq!(lean, format!("{l}\u{2192}pool1?\u{2504}\u{2504}"));
}

/// PAST CELLS HOLD THE LAST READING AND NEVER INTERPOLATE: at 12 cells a day, readings of 20% at
/// −23 h, 60% at −13 h and 65% at −1 h draw level 2 until the cell that holds −13 h ends, then
/// level 5: a step, nothing between; at 3 a day the same steps. A reset inside a cell draws `↺`
/// there, and a window that ended is blank until the new one is read. Each cell's span is the
/// one it draws. Negative control: interpolating between readings draws levels 3 and 4 between
/// them. A window that begins at a cell's end leaves that cell its reading.
#[test]
fn past_cells_hold_the_last_reading_and_never_interpolate() {
    let mut chart = flat(650);
    chart.past = vec![
        read(T5 - 23 * H, 200),
        read(T5 - 13 * H, 600),
        read(T5 - H, 650),
    ];
    let past = |chart: &Chart, per_day: u8| -> Vec<Cell> {
        let mut c = cells(chart, per_day);
        c.truncate(usize::from(per_day));
        c
    };
    let r = Cell::Read;
    assert_eq!(
        past(&chart, 12),
        [
            r(2),
            r(2),
            r(2),
            r(2),
            r(2),
            r(5),
            r(5),
            r(5),
            r(5),
            r(5),
            r(5),
            r(5)
        ]
    );
    assert_eq!(past(&chart, 3), [r(2), r(5), r(5)]);
    assert_eq!(chart.span_of(12, 0), Some((T5 - D, T5 - D + 2 * H)));
    assert_eq!(chart.span_of(12, 11), Some((T5 - 2 * H, T5)));
    assert_eq!(chart.span_of(12, 12), None, "the now cell has no time");
    assert_eq!(chart.span_of(12, 13), Some((T5, T5 + 2 * H)));
    assert_eq!(
        chart.span_of(12, 96),
        Some((T5 + 7 * D - 2 * H, T5 + 7 * D))
    );
    assert_eq!(chart.span_of(12, 97), None);

    let old = T5 - 6 * H;
    chart.past = vec![
        Read {
            reset: old,
            ..read(T5 - 20 * H, 900)
        },
        Read {
            reset: old + 7 * D,
            ..read(T5 - H, 50)
        },
    ];
    let p = past(&chart, 12);
    assert_eq!(
        p[8..],
        [r(7), Cell::Reset, Cell::Blank, r(1)],
        "the reset at −6 h, nothing known until the new week's first reading"
    );
}

/// CELLS BEFORE THE FIRST READING ARE BLANK: with no reading every past cell is blank; with the
/// first at −5 h, every cell that ends before it is; and across an epoch (a used limit reset at
/// −15 h restarted the window) the cells between it and the new window's first reading are
/// blank, the epoch's cell `↺`. Negative control: carrying a reading across the epoch (no
/// later-window check) draws 50% in the cell after it.
#[test]
fn cells_before_the_first_reading_are_blank() {
    let mut chart = flat(400);
    let past = |chart: &Chart| -> Vec<Cell> {
        let mut c = cells(chart, 12);
        c.truncate(12);
        c
    };
    assert!(past(&chart).iter().all(|c| *c == Cell::Blank));
    chart.past = vec![read(T5 - 5 * H, 400)];
    let p = past(&chart);
    assert!(p[..9].iter().all(|c| *c == Cell::Blank), "{p:?}");
    assert!(p[9..].iter().all(|c| *c == Cell::Read(3)), "{p:?}");
    chart.past = vec![
        read(T5 - 20 * H, 500),
        Read {
            epoch: Some(T5 - 15 * H),
            ..read(T5 - 10 * H, 100)
        },
    ];
    let p = past(&chart);
    assert_eq!(
        p[2..7],
        [
            Cell::Read(4),
            Cell::Read(4),
            Cell::Reset,
            Cell::Blank,
            Cell::Read(1)
        ]
    );
}

/// SIXTY-THREE AND NINETY-EIGHT ARE DIFFERENT LEVELS: in eighths 63% is level 5 and 98% level 7
/// (level 8 is kept for a full cell), each with its glyph: `▅` read, `🭸` and `🭶` projected; in
/// case (b) the last past cell is `▅` and the curve's cell before `↺` is `🭶`. Every glyph is the
/// design's code point. Negative control: quarters (`clamp(round(x·4/100), 1, 3)`) put both on
/// one level.
#[test]
fn sixty_three_and_ninety_eight_are_different_levels() {
    assert_eq!((level(630), level(980)), (5, 7));
    assert_eq!(Cell::Read(5).glyph(), '\u{2585}');
    assert_eq!(Cell::Ahead(5).glyph(), '\u{1fb78}');
    assert_eq!(Cell::Ahead(7).glyph(), '\u{1fb76}');
    let read: String = (1..=8).map(|l| Cell::Read(l).glyph()).collect();
    assert_eq!(
        read,
        "\u{2581}\u{2582}\u{2583}\u{2584}\u{2585}\u{2586}\u{2587}\u{2588}"
    );
    let ahead: String = (1..=8).map(|l| Cell::Ahead(l).glyph()).collect();
    assert_eq!(
        ahead,
        "\u{2581}\u{1fb7b}\u{1fb7a}\u{1fb79}\u{1fb78}\u{1fb77}\u{1fb76}\u{2594}"
    );
    let marks: String = [
        Cell::Now,
        Cell::Gap,
        Cell::Handoff,
        Cell::Reset,
        Cell::Credits,
        Cell::Unknown,
    ]
    .iter()
    .map(|c| c.glyph())
    .collect();
    assert_eq!(marks, "\u{2595}\u{2573}\u{2192}\u{21ba}$\u{2504}");
    let view = case_b().outlook().quota("main", None, true);
    let c = cells(&view.chart, 12);
    assert_eq!(c[11], Cell::Read(5), "63% now");
    let at_reset = c.iter().position(|c| *c == Cell::Reset).unwrap();
    assert_eq!(c[at_reset - 1], Cell::Ahead(7), "about 98% at the reset");
}

/// NO CELL IS FULL WITHOUT A READ 100 OR A PROJECTED FILL: level 8 is 1000 tenths and nothing
/// else; a lead of 99% over a replay of 40% (case (a)) shifts the first run up to its reset, yet
/// the curve stays short of a fill there (the tail says `~99%`) and no cell before it reaches
/// level 8, since a shift is clamped under a fill; a wall read at 97% draws level 7. Negative
/// control: an unclamped shift (`v + shift` capped only at 1000) fills the curve at main's
/// reset.
#[test]
fn no_cell_is_full_without_a_read_100_or_a_projected_fill() {
    for x10 in 0..=1000u16 {
        assert_eq!(level(x10) == 8, x10 == 1000, "{x10}");
    }
    let view = case_a()
        .outlook()
        .quota("main", Some(Figure::Exact(990)), true);
    assert_eq!(view.chart.runs[0].at(T5), Some(990));
    let reset = view.tab().unwrap().reset.unwrap();
    let at_reset = view.chart.runs[0].at(reset).unwrap();
    assert!(at_reset < 1000, "short of a fill at the reset: {at_reset}");
    assert_eq!(view.tail.own.as_ref().unwrap().by_reset, Some(99));
    for per_day in PER_DAY {
        let c = cells(&view.chart, per_day);
        let reset = c.iter().position(|c| *c == Cell::Reset).unwrap();
        assert!(
            c[..reset]
                .iter()
                .all(|c| !matches!(c, Cell::Read(8) | Cell::Ahead(8))),
            "{per_day}: {c:?}"
        );
    }
    let wall = walled().outlook().quota("main", None, true);
    assert!(cells(&wall.chart, 12).contains(&Cell::Read(7)));
    assert!(!cells(&wall.chart, 12).contains(&Cell::Read(8)));
}

/// A BURN-UP DRAWS READS AS AREA AND THE REPLAY AS LINE: case (b)'s lane, on the work row's
/// clock, holds main's last day as read (solid steps under the lane's ceiling, `▁…▅`), `▕` now,
/// the projected one-eighth line from the replay's fills (`▁ 🭻…🭸`), then `↺` in the cell its
/// reset falls in, its last; a walled account's lane is out ([`Cell::Out`]) to its reset. No
/// cell of either reaches the top quarter of its cell. Negative control: a lane that draws its
/// past with `Cell::Ahead` fails it; one on the chart's eighths ([`level`]) draws `🭶` before
/// main's reset.
#[test]
fn a_burn_up_draws_reads_as_area_and_the_replay_as_line() {
    let view = case_b().outlook().quota("main", None, true);
    let lane = &view.lanes[0];
    assert_eq!((lane.label.as_str(), lane.renews), ("main", SAT7));
    assert_eq!(lane.start, SAT7 - 7 * D);
    let mut c = Vec::new();
    lane.cells(6, T5, &mut c);
    let now = c.iter().position(|c| *c == Cell::Now).unwrap();
    assert_eq!(now, 6, "the work row's day before now, 4 h a cell");
    assert!(
        c[..now].iter().all(|c| matches!(c, Cell::Read(_))),
        "read from a day ago: {c:?}"
    );
    assert!(c[now - 1] == Cell::Read(lane_level(630)));
    let ahead = &c[now + 1..c.len() - 1];
    assert_eq!(
        ahead.len(),
        10,
        "41 h to the reset: ten cells of line, then the reset's"
    );
    assert!(ahead.iter().all(|c| matches!(c, Cell::Ahead(_))), "{c:?}");
    assert_eq!(c.last(), Some(&Cell::Reset));
    for cell in &c[..now] {
        let g = cell.glyph();
        assert!(g == ' ' || ('\u{2581}'..='\u{2585}').contains(&g), "{c:?}");
    }
    for cell in ahead {
        let g = cell.glyph();
        assert!(
            g == '\u{2581}' || ('\u{1fb78}'..='\u{1fb7b}').contains(&g),
            "{c:?}"
        );
    }

    let wall = walled().outlook().quota("main", None, true);
    let lane = &wall.lanes[0];
    lane.cells(6, T5, &mut c);
    let now = c.iter().position(|c| *c == Cell::Now).unwrap();
    assert!(c[now + 1..c.len() - 1].iter().all(|c| *c == Cell::Out));
    assert_eq!(Cell::Out.role(), Role::Warn);
}

/// A LANE KEEPS THE TOP QUARTER OF ITS CELL CLEAR: a lane sits right under its item's words, so
/// its levels ([`lane_level`]) top out at [`LANE_TOP`], the cell's sixth eighth: a full week read
/// is `▆` and the line `🭷` at most, which only out ([`Cell::Out`]) reaches, so out stands one
/// step above every projected level as well as in the warn ink. 63% and 98% stay apart (levels
/// 3 and 5). Negative control: the chart's eighths ([`level`]) put 98% on `🭶` and a full read on
/// `█`, both in the top quarter.
#[test]
fn a_lane_keeps_the_top_quarter_of_its_cell_clear() {
    assert_eq!((lane_level(630), lane_level(980)), (3, 5));
    assert_eq!(
        (lane_level(0), lane_level(999), lane_level(1000)),
        (1, 5, LANE_TOP)
    );
    let top = ['\u{2594}', '\u{1fb76}', '\u{2587}', '\u{2588}'];
    for x10 in 0..=1000u16 {
        let l = lane_level(x10);
        assert!((1..=LANE_TOP).contains(&l), "{x10}");
        assert_eq!(
            l == LANE_TOP,
            x10 >= 1000,
            "{x10}: the ceiling is out alone"
        );
        for cell in [Cell::Read(l), Cell::Ahead(l)] {
            assert!(!top.contains(&cell.glyph()), "{x10}: {cell:?}");
        }
    }
    assert_eq!(Cell::Out.glyph(), '\u{1fb77}');
    assert_eq!(Cell::Read(LANE_TOP).glyph(), '\u{2586}');
}

/// A RESET PAST A WEEK IS NEVER SAID OR DRAWN: a stored reading keeps the vendor's `resets_at`
/// as sent, so a week read with one 30 days ahead, or at `i64::MAX`, on a clock east of UTC, is
/// no week's reset: the tab says no `↺`, its lane draws nothing ahead, and no word adds the
/// local offset to it. A lane holds the work row's span, a day of cells before `▕` and a week
/// after, with `↺` only where its reset falls inside them, whatever it is built with. Negative
/// controls: `reset_ahead` filtering only `> as_of` says `↺` for the reset 30 days on and
/// overflows adding the offset to `i64::MAX`; `Burn::cells` drawn to its reset unbounded runs
/// past the chart's end on `renews = i64::MAX`.
#[test]
fn a_reset_past_a_week_is_never_said_or_drawn() {
    let mut c = Vec::new();
    for reset in [T5 + 30 * D, i64::MAX] {
        let mut fx = Fx::new(&[("main", Class::Preferred)]);
        fx.now = T5;
        fx.offset = |_| 9 * H;
        fx.read("main", None, Some((500, reset)), T5);
        let view = fx.outlook().quota("main", None, true);
        let tab = view.tab().unwrap();
        assert_eq!(
            (tab.figure, tab.reset),
            (Figure::Exact(500), None),
            "{reset}"
        );
        assert_eq!(view.tail.own, None, "{reset}");
        let lane = &view.lanes[0];
        assert!(lane.renews <= T5 && lane.ahead.is_empty(), "{reset}");
        lane.cells(12, T5, &mut c);
        assert_eq!(c.last(), Some(&Cell::Now), "{reset}");
        for words in every_word(&view) {
            assert!(!words.contains(RESET_MARK), "{reset}: {words}");
        }
    }
    let lane = |start: i64, renews: i64| Burn {
        label: "main".into(),
        start,
        renews,
        read: Vec::new(),
        ahead: Vec::new(),
        out_at: None,
    };
    for per_day in PER_DAY {
        let k = usize::from(per_day);
        lane(i64::MAX - 7 * D, i64::MAX).cells(per_day, T5, &mut c);
        assert_eq!(
            c.len(),
            k + 1 + 7 * k,
            "{per_day}: a day, now, then a week of line"
        );
        assert!(
            !c.contains(&Cell::Reset),
            "{per_day}: its reset is past the edge"
        );
        lane(i64::MIN, T5 + D).cells(per_day, T5, &mut c);
        assert_eq!(
            c.len(),
            k + 1 + k + 1,
            "{per_day}: the work row's day of past"
        );
        lane(T5, T5 + 7 * D - H).cells(per_day, T5, &mut c);
        assert_eq!(c.len(), k + 1 + 7 * k, "{per_day}: a week to its reset");
        assert_eq!(c.last(), Some(&Cell::Reset), "{per_day}");
    }
}

/// A clock like Santiago's in spring: −4 h until a local midnight (Sun 8 Nov here), then −3 h,
/// so that day has no 00:00.
const SANTIAGO_SPRING: i64 = (T5 + 3 * D).div_euclid(D) * D + 4 * H;

fn santiago(t: i64) -> i64 {
    if t < SANTIAGO_SPRING { -4 * H } else { -3 * H }
}

/// MIDNIGHTS BEGIN EACH LOCAL DAY: the chart's hairlines fall at each local midnight inside
/// [now − 1 d, now + 7 d), eight of them at the design's now on a fixed clock, each at a local
/// 00:00; on a clock that springs forward over midnight ([`santiago`]), the day with no 00:00 begins
/// at the change, its first instant on the new offset (local 01:00), and every other day at its
/// 00:00. Negative control: `midnights` without the skipped-midnight step puts Sunday's an hour
/// early, at 23:00 on Saturday.
#[test]
fn midnights_begin_each_local_day() {
    for offset in [0, 9 * H, -4 * H] {
        let offsets = Offsets::fixed(offset);
        let m = midnights(T5 - D, T5 + 7 * D, &offsets);
        assert_eq!(m.len(), 8, "{offset}");
        assert!(
            m.iter().all(|t| (t + offset).rem_euclid(D) == 0),
            "{offset}"
        );
    }
    let offsets = Offsets::around(T5, santiago);
    let m = midnights(T5 - D, T5 + 7 * D, &offsets);
    assert!(m.contains(&SANTIAGO_SPRING), "{m:?}");
    assert!(!m.contains(&(SANTIAGO_SPRING - H)), "{m:?}");
    assert_eq!(m.len(), 8);
    for t in m.iter().filter(|t| **t != SANTIAGO_SPRING) {
        assert_eq!((t + offsets.at(*t)).rem_euclid(D), 0, "{t}");
    }
    assert_eq!(
        (SANTIAGO_SPRING + offsets.at(SANTIAGO_SPRING)).rem_euclid(D),
        H
    );
    let mut fx = case_a();
    fx.offset = santiago;
    let view = fx.outlook().quota("main", None, true);
    assert_eq!(
        view.chart.midnights, m,
        "the view's hairlines are the outlook clock's"
    );
}

/// THE ROW FIGURE, THE CURVE AND THE TAIL AGREE: the lead's figure, the last past cell, the curve
/// at `▕` and, at this tab's reset, the curve and the tail's `~N%` tell one figure, both from the
/// series' own reading and from a newer one the footer holds (70% over the replay's 63%, or a
/// bound of 45% over case (a)'s 40%). Negative control: dropping the shift leaves the curve at
/// `▕` at the replay's 63% under a lead of 70%.
#[test]
fn the_row_figure_the_curve_and_the_tail_agree() {
    for (fx, own) in [
        (case_a(), None),
        (case_a(), Some(Figure::AtLeast(450))),
        (case_b(), None),
        (case_b(), Some(Figure::Exact(700))),
    ] {
        let view = fx.outlook().quota("main", own, true);
        let tab = view.tab().unwrap();
        let x10 = match tab.figure {
            Figure::Exact(x) | Figure::AtLeast(x) => x,
            other => panic!("{other:?}"),
        };
        let line = laid(&view, 166).unwrap();
        let lead = line.text_of(&line.lead);
        assert!(
            lead.starts_with(&format!("main {}", tab.figure.words())),
            "{lead}"
        );
        let c = cells(&view.chart, 12);
        assert_eq!(c[11], Cell::Read(level(x10)), "the last past cell");
        let first = &view.chart.runs[0];
        assert_eq!(first.at(T5), Some(x10), "the curve at now");
        let reset = tab.reset.unwrap();
        let at_reset = first.at(reset).unwrap();
        let own = view.tail.own.as_ref().unwrap();
        assert_eq!(own.by_reset, Some(((at_reset + 5) / 10).min(99)));
        let tail = line.text_of(&line.tail);
        assert!(
            tail.starts_with(&format!("~{}% by", own.by_reset.unwrap())),
            "{tail}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Words (§2.3, §2.6)

/// NO GLASS WORD NAMES 5H, 7D OR LEFT: across every scenario, the row at every room, the title,
/// the spoken words and the fallback say use (`used`, `≥`, `out`), never `% left`, and never name
/// the 5-hour or 7-day window by its short word (a 5-hour stop's title says `a 5-hour limit`).
/// Negative control: a phrase 2 in the words the outlook's verdict had before the glass (case
/// (a)'s `continuous 7d`, deleted with them in 2026-11-07's review fixes) fails it.
#[test]
fn no_glass_word_names_5h_7d_or_left() {
    for (name, view) in scenarios() {
        for words in every_word(&view) {
            let bad = words
                .split(|c: char| !c.is_alphanumeric())
                .find(|w| ["5h", "7d", "left", "weekly"].contains(w));
            assert_eq!(bad, None, "{name}: {words}");
        }
    }
}

/// NO INFERRED 100: a wall says `out` and no figure, though its reading said 97%, and the
/// footer's own figure of 97% (a reading, never `out`) does not override a wall standing now;
/// a curve that reaches 99.5% or more by the reset, short of a fill, says `~99%`; an unread
/// account the worst case assumes full is `?`, and no word of any scenario says `100`, a chart
/// column's title included. Negative controls: the tail rounded half up without the cap
/// (`(x10 + 5) / 10`) says `~100%` for case (b) under a lead of 64%; `own` taken over the row's
/// figure whatever it is says `main 97%` on a walled tab; a hand-off's title saying the fill it
/// leaves by `whole_spoken` (as S5b did) says `the work moves from alt2 (100%) to main`.
#[test]
fn no_inferred_100() {
    let wall = walled().outlook().quota("main", None, true);
    assert_eq!(wall.tab().unwrap().figure, Figure::Out);
    let line = laid(&wall, 166).unwrap();
    assert!(line.text_of(&line.lead).starts_with("main out"));
    let footer = walled()
        .outlook()
        .quota("main", Some(Figure::Exact(970)), true);
    assert_eq!(footer.tab().unwrap().figure, Figure::Out);
    assert!(footer.title.contains("out now"), "{}", footer.title);
    for words in words_now(&footer) {
        assert!(!words.contains("97"), "{words}");
    }
    let high = case_b()
        .outlook()
        .quota("main", Some(Figure::Exact(640)), true);
    let reset = high.tab().unwrap().reset.unwrap();
    let curve = high.chart.runs[0].at(reset).unwrap();
    assert!((995..1000).contains(&curve), "{curve}");
    assert_eq!(high.tail.own.as_ref().unwrap().by_reset, Some(99));
    let worst = worst_case().outlook().quota("me", None, true);
    let pool = worst.named.iter().find(|n| n.label == "pool1").unwrap();
    assert_eq!(pool.figure, Figure::Unread);
    for (name, view) in scenarios()
        .into_iter()
        .chain([("high", high), ("wall", wall)])
    {
        for words in every_word(&view) {
            assert!(
                !words.contains("100%") && !words.contains("100 percent"),
                "{name}: {words}"
            );
        }
    }
}

/// THE SPOKEN ROW HAS NO DRAWING GLYPH: across every scenario, the words a reader hears carry no
/// box drawing or block (U+2500–259F), braille (U+2800–28FF) or legacy computing (U+1FB00–1FBFF)
/// glyph, and none of the row's marks (`→ ↺ ⧗ ≥ ~ $ ?`): instants are said in words, a lower
/// bound `at least`. Negative control: speaking the tail's phrase 2 (`╳ ~Thu 12 Nov 04:00`)
/// fails it.
#[test]
fn the_spoken_row_has_no_drawing_glyph() {
    for (name, view) in scenarios() {
        assert!(view.spoken.starts_with("Accounts. "), "{name}");
        let bad = view.spoken.chars().find(|c| {
            ('\u{2500}'..='\u{259f}').contains(c)
                || ('\u{2800}'..='\u{28ff}').contains(c)
                || ('\u{1fb00}'..='\u{1fbff}').contains(c)
                || "\u{2192}\u{21ba}\u{29d7}\u{2265}~$?\u{2026}".contains(*c)
        });
        assert_eq!(bad, None, "{name}: {}", view.spoken);
    }
}

/// EVERY STOP THE CHART DRAWS IS SPOKEN (§2.6: the spoken row carries the stops the row shows):
/// across every scenario, each stop the chart draws (`╳`, or `$` under usage credits) is said by
/// its start, `from about …`, and a wall the pane waits at now, a stop from now, by its end, `Out
/// now: the work waits until …`. A lean whose both branches stop (`lean-stop`: scenario C with
/// pool1 read at 99%) is `Verdict::Stop { pass: Week, lean: Some }` with no stop of the week
/// pass's own: the chart draws the worst branch's stop, the tail says `╳`, and a reader hears
/// `…, whatever pool1 holds`. Every stop the working pass projects is said within hours,
/// whatever the verdict: `five-wall-working` waits at its wall now and walls twice more before
/// morning. Negative controls: `chain_spoken` without the lean's stop leaves `lean-stop`'s stop
/// unsaid; the working pass's stop said only where it is the verdict (the S3 rule) leaves
/// `five-wall-working`'s later 5-hour stops unsaid.
#[test]
fn every_stop_the_chart_draws_is_spoken() {
    for (name, out, tab) in outlooks() {
        let view = out.quota(tab, None, true);
        for &(from, to, _) in &view.chart.stops {
            let waits = format!(
                "Out now: the work waits until {}.",
                spoken_when(to, out.as_of, &out.offsets)
            );
            if from <= out.as_of && view.spoken.contains(&waits) {
                continue;
            }
            let at = spoken_when(
                round_down(from, out.as_of, &out.offsets),
                out.as_of,
                &out.offsets,
            );
            assert!(
                view.spoken.contains(&format!("from about {at}")),
                "{name}: the stop from {at} in {}",
                view.spoken
            );
        }
    }
    let out = lean_stops().outlook();
    let Verdict::Stop {
        pass: PassKind::Week,
        from,
        lean: Some(row),
        ..
    } = &out.verdict
    else {
        panic!("a stop both branches meet: {:?}", out.verdict);
    };
    assert_eq!(row, "pool1");
    assert!(
        out.week.as_ref().unwrap().stops.is_empty(),
        "the lean's stop"
    );
    let view = out.quota("me", None, true);
    assert!(view.chart.stops.iter().any(|(f, _, _)| f == from));
    assert!(cells(&view.chart, 12).contains(&Cell::Gap));
    assert!(
        view.tail
            .verdict
            .as_ref()
            .unwrap()
            .full
            .starts_with(GAP_MARK)
    );
    assert!(
        view.spoken.contains("whatever pool1 holds."),
        "{}",
        view.spoken
    );
}

/// EVERY STOP THE ROW SAYS IS DRAWN: across every scenario, where the tail's phrase 2 says a stop
/// (`╳`), usage credits (`$`) or a wait at a wall standing now (`⧗`), the chart draws it at every
/// grain, in a future cell its span overlaps (`$` under credits, else `╳`), and the title says it
/// by the same instant: a stop's start, a wait's end. Negative controls: `chart_of` without the
/// wall's stop fails `five-wall` and `early-wall`; drawing only the week pass's lean stop (the S3
/// rule) fails `working-lean-stop`.
#[test]
fn every_stop_the_row_says_is_drawn() {
    let mut said = BTreeSet::new();
    for (name, out, tab) in outlooks() {
        let view = out.quota(tab, None, true);
        let Some(phrase) = &view.tail.verdict else {
            continue;
        };
        let at = |t: i64| when(t, out.as_of, &out.offsets);
        let about = |t: i64| format!("~{}", at(round_down(t, out.as_of, &out.offsets)));
        let tab_row = out.rows.iter().position(|r| r.label == tab);
        let (mark, from, to, words, cell) = match out.verdict_for(tab_row) {
            Verdict::Walled {
                to: None,
                until: Some(until),
            } => {
                let end = out.wall_reset(until).unwrap_or(until);
                let words = format!("the work waits until {}", at(end));
                (WAIT_MARK, out.as_of, end, words, Cell::Gap)
            }
            Verdict::Stop { from, end, .. } => {
                let to = end.at().unwrap_or(i64::MAX);
                (GAP_MARK, from, to, about(from), Cell::Gap)
            }
            Verdict::Credits { at: from } => {
                let to = out
                    .week
                    .iter()
                    .chain(&out.working)
                    .flat_map(Outlook::projected)
                    .find(|(stop, _)| stop.from == from)
                    .and_then(|(stop, _)| stop.end.at())
                    .unwrap_or(i64::MAX);
                (CREDITS_MARK, from, to, about(from), Cell::Credits)
            }
            _ => continue,
        };
        assert!(phrase.full.starts_with(mark), "{name}: {}", phrase.full);
        assert!(
            view.title.contains(&words),
            "{name}: `{words}` in {}",
            view.title
        );
        for per_day in PER_DAY {
            let s = DAY / i64::from(per_day);
            let f = future(&view.chart, per_day);
            let drawn = f.iter().enumerate().any(|(j, c)| {
                let a = out.as_of + i64::try_from(j).unwrap() * s;
                *c == cell && a < to && from < a + s
            });
            assert!(drawn, "{name} at {per_day} a day: {f:?}");
        }
        said.insert(name);
    }
    for name in [
        "b",
        "lean-stop",
        "working",
        "credits",
        "wall",
        "five-wall",
        "five-wall-working",
        "early-wall",
        "working-lean-stop",
    ] {
        assert!(said.contains(name), "{name} says a stop: {said:?}");
    }
}

/// A WALL STANDING NOW IS DRAWN FROM NOW (§2.2, §2.3): the wall a pane waits at is a stop from now
/// until it ends, at the vendor reset its grace follows, so the chart draws `╳` from `▕` by the
/// half-cover rule, at least the first future cell, beside the tail's `⧗`, the title and the
/// spoken words. Case (b) walled on its 5-hour window, which resets at 19:00, has no working pass
/// (`five-wall`): its first future cell is `╳` at every grain and the next a level, on the laid
/// row too. Under a refused pace nothing is projected (`early-wall`), and the wait is drawn over
/// `┄`; a tab the rotation cannot move says no wait and draws none. A pass that begins the stop
/// now itself (`five-wall-working`'s working pass, `wall`'s week pass) has it drawn once, and
/// the working pass's is not said again within hours. Negative control: `chart_of` without the
/// wall's stop leaves `five-wall`'s first future cell a level and `early-wall`'s `┄`.
#[test]
fn a_wall_standing_now_is_drawn_from_now() {
    let out = five_wall().outlook();
    assert!(out.working.is_none(), "no working pass");
    let view = out.quota("main", None, true);
    assert!(
        view.chart.stops.contains(&(T5, T5 + 2 * H, false)),
        "{:?}",
        view.chart.stops
    );
    for per_day in PER_DAY {
        let f = future(&view.chart, per_day);
        assert_eq!(f[0], Cell::Gap, "{per_day} a day");
        assert!(matches!(f[1], Cell::Ahead(_)), "{per_day} a day: {f:?}");
    }
    let line = laid(&view, 166).unwrap();
    let now = line.chart.start + usize::from(line.per_day);
    assert_eq!(
        (line.cells[now].0, line.cells[now + 1].0),
        (NOW_MARK, GAP_MARK)
    );
    assert_eq!(
        view.tail.verdict.as_ref().unwrap().full,
        format!("{WAIT_MARK} until 19:00")
    );
    assert!(
        view.title.contains("the work waits until 19:00"),
        "{}",
        view.title
    );
    assert!(
        view.spoken.contains("Out now: the work waits until 19:00."),
        "{}",
        view.spoken
    );

    let out = early_wall().outlook();
    assert!(out.week.is_none(), "nothing projected: {:?}", out.verdict);
    let view = out.quota("me", None, true);
    assert_eq!(view.chart.stops, [(NOW, NOW + 2 * H, false)]);
    for per_day in PER_DAY {
        let f = future(&view.chart, per_day);
        assert_eq!(f[0], Cell::Gap, "{per_day} a day");
        assert!(
            f[1..]
                .iter()
                .all(|c| matches!(c, Cell::Unknown | Cell::Reset)),
            "{per_day} a day: {f:?}"
        );
    }
    assert_eq!(
        view.tail.verdict.as_ref().unwrap().full,
        format!("{WAIT_MARK} until 21:30")
    );
    assert!(
        view.spoken.contains("Out now: the work waits until 21:30."),
        "{}",
        view.spoken
    );
    let fixed = out.quota("me", None, false);
    assert!(fixed.chart.stops.is_empty() && fixed.tail.verdict.is_none());

    let out = five_wall_working().outlook();
    let working = out.working.as_ref().expect("a working pass");
    assert_eq!(working.stops[0].from, T5, "the pass begins the wait");
    assert!(working.stops.len() > 1, "and walls again: {working:?}");
    let view = out.quota("main", None, true);
    assert_eq!(
        view.spoken.matches("until 19:00").count(),
        1,
        "{}",
        view.spoken
    );
    for out in [out, walled().outlook()] {
        let view = out.quota("main", None, true);
        let from_now = view
            .chart
            .stops
            .iter()
            .filter(|(from, ..)| *from <= out.as_of);
        assert_eq!(from_now.count(), 1, "drawn once: {:?}", view.chart.stops);
    }
}

/// THE STOP BOTH BRANCHES OF A LEAN MEET IS DRAWN IN EITHER PASS (§2.2, §2.6): scenario D with
/// pool1 read at 99.9% of its week (`working-lean-stop`): at alt1's 5-hour wall the working pass
/// leans on pool1 and both branches stop, so the verdict is `Stop { pass: Working, lean:
/// Some("pool1") }` with no stop of the working pass's own. The chart draws the worst branch's
/// stop as `╳` at every grain, and the tail, the title and the spoken words say that stop by
/// the same start and end: `╳ ~20:10`, `… (a 5-hour limit), whatever pool1 holds`, and `Within
/// hours: …, whatever pool1 holds`. The week pass's (`lean-stop`) is drawn as before. Negative
/// control: `chart_of` drawing only the week pass's lean stop (the S3 rule) leaves
/// `working-lean-stop` with no `╳`.
#[test]
fn the_stop_both_branches_of_a_lean_meet_is_drawn_in_either_pass() {
    let out = working_lean_stops().outlook();
    let Verdict::Stop {
        pass: PassKind::Working,
        from,
        end,
        lean: Some(row),
    } = &out.verdict
    else {
        panic!("a working stop both branches meet: {:?}", out.verdict);
    };
    assert_eq!(row, "pool1");
    let working = out.working.as_ref().unwrap();
    assert!(working.stops.is_empty(), "the lean's stop: {working:?}");
    assert_eq!(
        Outlook::lean_stop(working).map(|s| (s.from, s.end)),
        Some((*from, *end))
    );
    let to = end.at().unwrap();
    let view = out.quota("me", None, true);
    assert_eq!(view.chart.stops, [(*from, to, false)]);
    for per_day in PER_DAY {
        let s = DAY / i64::from(per_day);
        let f = future(&view.chart, per_day);
        let first = usize::try_from((from - out.as_of) / s).unwrap();
        let last = usize::try_from((to - 1 - out.as_of) / s).unwrap();
        assert!(
            f[first..=last].contains(&Cell::Gap),
            "{per_day} a day: {f:?}"
        );
    }
    let start = round_down(*from, out.as_of, &out.offsets);
    let (at, end) = (
        when(start, out.as_of, &out.offsets),
        when(to, out.as_of, &out.offsets),
    );
    assert_eq!(
        view.tail.verdict.as_ref().unwrap().short,
        format!("{GAP_MARK} ~{at}")
    );
    let title = format!("~{at} until {end} (a 5-hour limit), whatever pool1 holds");
    assert!(view.title.contains(&title), "{}", view.title);
    let spoken = format!(
        "Within hours: every account out from about {} until {}, a 5-hour limit, whatever pool1 \
         holds.",
        spoken_when(start, out.as_of, &out.offsets),
        spoken_when(to, out.as_of, &out.offsets)
    );
    assert!(view.spoken.contains(&spoken), "{}", view.spoken);
    let week = lean_stops().outlook().quota("me", None, true);
    assert!(cells(&week.chart, 12).contains(&Cell::Gap));
}

/// A WAIT IS SAID AT THE VENDOR RESET IT ENDS ON:a wall stands to its reset's grace
/// (`RESET_GRACE_S` past it), and the outlook names the reset (`Walled::reset`), so the glass
/// says the reset exactly whichever window walled: case (b) walled on its 5-hour window (reset
/// 19:00) waits `⧗ until 19:00` on the row, in the title and in the spoken words, beside its
/// week's own `↺ Sat 10:00`; both accounts walled on their weeks wait `⧗ until Sat 10:00`.
/// Negative control: `resumes` matching only a week's reset (the S3 rule) says the 5-hour wait
/// `⧗ until 19:02`.
#[test]
fn a_wait_is_said_at_the_vendor_reset_it_ends_on() {
    let five = T5 + 2 * H;
    let out = five_wall().outlook();
    let walled_now = out.rows[0].walled.as_ref().expect("walled now");
    assert_eq!(
        (walled_now.until, walled_now.reset),
        (Some(five + accounts::RESET_GRACE_S), Some(five))
    );
    let view = out.quota("main", None, true);
    let wait = format!("{WAIT_MARK} until 19:00");
    assert_eq!(view.tail.verdict.as_ref().unwrap().full, wait);
    let line = laid(&view, 66).unwrap();
    assert_eq!(line.text_of(&line.tail), wait);
    let line = laid(&view, 96).unwrap();
    assert_eq!(
        line.text_of(&line.tail),
        format!("{RESET_MARK} Sat 10:00{SEP}{wait}")
    );
    assert!(
        view.title.contains("the work waits until 19:00 "),
        "{}",
        view.title
    );
    assert!(
        view.spoken.contains("the work waits until 19:00."),
        "{}",
        view.spoken
    );
    let wall = walled().outlook().quota("main", None, true);
    assert_eq!(
        wall.tail.verdict.unwrap().full,
        format!("{WAIT_MARK} until Sat 10:00")
    );
}

/// A WAIT KEEPS THIS TAB'S OWN RESET where it is another instant: with both accounts walled the
/// work waits until main's reset (Sat 10:00); alt1's tab, whose week resets Sun 10:00, keeps
/// phrase 1 (`↺ Sun 10:00 · ⧗ until Sat 10:00`, and the fallback's widest form says it), while
/// main's tab, where it would repeat phrase 2, leaves it out. Negative control: leaving phrase 1
/// out at every wait leaves alt1's `↺ Sun 10:00` unsaid.
#[test]
fn a_wait_keeps_this_tabs_own_reset() {
    let out = walled().outlook();
    let alt1 = out.quota("alt1", None, true);
    assert_eq!(
        alt1.tail.own.as_ref().map(|o| o.reset.as_str()),
        Some("Sun 10:00")
    );
    let both = format!("{RESET_MARK} Sun 10:00{SEP}{WAIT_MARK} until Sat 10:00");
    let line = laid(&alt1, 96).unwrap();
    assert_eq!(line.text_of(&line.tail), both);
    let widest: String = alt1.fallback()[0].iter().map(|p| p.text.as_str()).collect();
    assert_eq!(widest, format!("alt1 out{SEP}{both}"));
    let main = out.quota("main", None, true);
    assert_eq!(main.tail.own, None, "main's reset is the wait's");
}

/// A WALL THE WORK MOVES OFF IS TITLED: a tab walled now whose policy moves the work at the
/// turn's end (main out, alt1 at 30%) has no phrase 2, since the chart's `→alt1` draws the move;
/// its hover title says `the work moves to alt1 at the turn's end` (at the narrowest rung, with
/// no chart, nothing else does), as the spoken words do. A tab the rotation cannot move says no
/// move. Negative control: titling a verdict only where the tail has a phrase 2 leaves it out.
#[test]
fn a_wall_the_work_moves_off_is_titled() {
    let out = moves().outlook();
    let row = out.rows.iter().find(|r| r.label == "main").unwrap();
    assert_eq!(
        row.walled.as_ref().and_then(|w| w.to.as_deref()),
        Some("alt1")
    );
    let view = out.quota("main", None, true);
    assert_eq!(view.tail.verdict, None);
    let moves = "the work moves to alt1 at the turn's end";
    assert!(view.title.contains(moves), "{}", view.title);
    assert!(view.spoken.contains(moves), "{}", view.spoken);
    assert_eq!(future(&view.chart, 12)[0], Cell::Handoff);
    let fixed = out.quota("main", None, false);
    assert!(!fixed.title.contains(moves), "{}", fixed.title);
}

/// A REFUSED PACE SAYS ITS SHORT WORDS ON THE CARRIER'S TAB (§2.3's last row): `me`'s week is an
/// hour old, so its pace is refused (`TooEarly`); on `me`'s tab, the carrier's, phrase 2 is the
/// refusal's short words, `no pace yet`, not kept, so the narrowest rungs give it phrase 1's
/// place (`no pace yet` alone), and the title says the whole refusal; on `alt1`'s tab it is not
/// said. `Refusal::short` cuts only the detail. Negative controls: `Refusal::short` returning
/// `words()` says `no pace yet: 1h of readings, 2h needed` on the row; the carrier guard inverted
/// says nothing on `me`'s tab.
#[test]
fn a_refused_pace_says_its_short_words_on_the_carriers_tab() {
    let out = too_early().outlook();
    let why = Refusal::TooEarly {
        have_s: H,
        need_s: 2 * H,
    };
    assert_eq!(out.verdict, Verdict::Refused(why));
    let me = out.quota("me", None, true);
    let phrase = me.tail.verdict.as_ref().expect("phrase 2");
    assert_eq!(
        (phrase.full.as_str(), phrase.short.as_str()),
        ("no pace yet", "no pace yet")
    );
    assert!(!phrase.kept);
    let line = laid(&me, 166).unwrap();
    assert!(
        line.text_of(&line.tail).ends_with("no pace yet"),
        "{}",
        line.text()
    );
    let line = laid(&me, 30).unwrap();
    assert_eq!(line.text_of(&line.tail), "no pace yet");
    assert!(me.title.contains(&why.words()), "{}", me.title);
    let alt1 = out.quota("alt1", None, true);
    assert_eq!(alt1.tail.verdict, None);
    assert!(alt1.tail.own.is_some());
    assert_eq!(Refusal::Stale { age_s: 2 * H }.short(), "not projected");
    assert_eq!(
        Refusal::SessionPaused.short(),
        Refusal::SessionPaused.words()
    );
}

/// A READING THE REPLAY HAS NOT SEEN IS NOT REFUSED (§2.2, 2026-11-08): case (d)'s replay read
/// nothing of `main`, its carrier, so its verdict is `Refusal::NoFigure` (`no reading`). A pane
/// whose footer holds `main`'s first reading (`own`, 63%) leads with it, and nothing on its row
/// at any rung, in its title or in its spoken words says `no reading` beside it: the past holds
/// no step and the future is `┄` until a replay reads the reading, and the tail is empty. With no
/// `own` the pane is case (d), the first reading's wait. A refusal of another kind is said as
/// before (`a_refused_pace_says_its_short_words_on_the_carriers_tab`). NEGATIVE CONTROL (run
/// once, restored): `Words::outdated` always false says `no reading` after `main 63% used`, as
/// the first `lab claude --own` run's first minute showed (§7 step 4).
#[test]
fn a_reading_the_replay_has_not_seen_is_not_refused() {
    let out = case_d().outlook();
    let refused = Refusal::NoFigure;
    assert_eq!(out.verdict, Verdict::Refused(refused));
    assert!(
        out.rows.iter().any(|r| r.label == "main" && r.carrier),
        "main carries, so its tab says the refusal"
    );
    assert!(out.quota("main", None, true).waiting(), "case (d)");
    let view = out.quota("main", Some(Figure::Exact(630)), true);
    assert_eq!(view.tab().map(|t| t.figure), Some(Figure::Exact(630)));
    assert_eq!(view.tail.verdict, None, "no phrase 2");
    for room in [166, 96, 66, 24] {
        let line = laid(&view, room).unwrap_or_else(|| panic!("a rung fits {room}"));
        let text = line.text();
        assert!(text.starts_with("main 63%"), "at {room}: {text}");
        assert!(!text.contains(&refused.short()), "at {room}: {text}");
    }
    assert!(!view.title.contains(&refused.words()), "{}", view.title);
    assert!(!view.spoken.contains(&refused.spoken()), "{}", view.spoken);
    assert!(
        future(&view.chart, 12).iter().all(|c| *c == Cell::Unknown),
        "nothing projected"
    );
}

// ---------------------------------------------------------------------------------------------
// The ladder (§2.4)

/// What a laid row shows: the names in its lead, its grain, and each phrase (`own`, `verdict`)
/// with whether it is full.
type Parts = (BTreeSet<String>, u8, Vec<(&'static str, bool)>);

/// The [`Parts`] of a laid row.
fn parts(view: &QuotaView, line: &QuotaLine) -> Parts {
    let lead = line.text_of(&line.lead);
    let names = view
        .named
        .iter()
        .map(|n| cut(&n.label))
        .filter(|n| lead.split(' ').any(|w| w == n))
        .collect();
    let tail = line.text_of(&line.tail);
    let mut phrases = Vec::new();
    if let Some(own) = &view.tail.own
        && tail.contains(&format!("{RESET_MARK} {}", own.reset))
    {
        let full = own
            .by_reset
            .is_some_and(|p| tail.contains(&format!("~{p}% by")));
        phrases.push(("own", full || own.by_reset.is_none()));
    }
    if let Some(v) = &view.tail.verdict {
        if tail.contains(&v.full) && v.full != v.short {
            phrases.push(("verdict", true));
        } else if tail.contains(&v.short) {
            phrases.push(("verdict", v.full == v.short));
        }
    }
    (names, line.per_day, phrases)
}

/// THE QUOTA LADDER KEEPS ITS INVARIANTS, over every room from 0 to 200, in the design's cases
/// (a) to (d) and in a gap, a lean, a 5-hour stop, a lean whose worst case stops, credits and a
/// wall: a laid row fits its room, and so does every wider room's; (1) this tab's name and figure
/// never fold; (2) a stop, wall, lean or credits phrase stays, at least short, while any figure
/// shows; (3) no figure shows without its name; (4) each narrower room's parts are a subset of a
/// wider one's (names, phrases, a full phrase only where the wider is full) and its chart is
/// never finer; (5) no rung is finer than 12 cells a day. The design's mocks hold their rungs at
/// 166, 96 and 66 cells in each case's own test (`case_a_lays_r1_r3_r6`, `case_b_lays_r2_r5_r7`,
/// `case_c_lays_r2_r5_r7`, `case_d_lays_its_words_full_then_short`). Negative control: a T2 that
/// keeps phrase 1 over phrase 2 drops case (b)'s stop from 77 cells down (invariant 2).
#[test]
fn the_quota_ladder_keeps_its_invariants() {
    for (name, view) in scenarios() {
        let tab = view.tab().unwrap();
        let mut wider: Option<Parts> = None;
        let mut fits = false;
        for room in (0..=200).rev() {
            let Some(line) = laid(&view, room) else {
                fits = false;
                continue;
            };
            assert!(line.cells.len() <= room, "{name} at {room}");
            assert!(
                room == 200 || fits,
                "{name}: laid at {room} but not at {}",
                room + 1
            );
            fits = true;
            let lead = line.text_of(&line.lead);
            let text = line.text();
            // (1) and (3).
            if view.waiting() {
                assert_eq!(lead, cut(&tab.label), "{name} at {room}");
            } else {
                let mut body = lead.as_str();
                body = body.strip_suffix(" used").unwrap_or(body);
                if let Some((head, n)) = body.rsplit_once(" +")
                    && n.chars().all(|c| c.is_ascii_digit())
                {
                    body = head;
                }
                let shown: Vec<&str> = body.split(SEP).collect();
                for part in &shown {
                    assert!(
                        view.named
                            .iter()
                            .any(|n| *part == format!("{} {}", cut(&n.label), n.figure.words())),
                        "{name} at {room}: `{part}` in `{lead}`"
                    );
                }
                let own = format!("{} {}", cut(&tab.label), tab.figure.words());
                assert!(shown.contains(&own.as_str()), "{name} at {room}: {lead}");
                // (2).
                if let Some(v) = view.tail.verdict.as_ref().filter(|v| v.kept) {
                    assert!(
                        text.contains(&v.short) || text.contains(&v.full),
                        "{name} at {room}: {text}"
                    );
                }
            }
            // (5).
            assert!(line.per_day == 0 || PER_DAY.contains(&line.per_day));
            assert!(line.per_day <= 12);
            // (4).
            let now = parts(&view, &line);
            if let Some(w) = &wider {
                assert!(now.0.is_subset(&w.0), "{name} at {room}: {now:?} {w:?}");
                assert!(now.1 <= w.1, "{name} at {room}: finer than a wider room");
                for (phrase, full) in &now.2 {
                    let was = w.2.iter().find(|(p, _)| p == phrase);
                    assert!(
                        was.is_some_and(|(_, f)| *f || !full),
                        "{name} at {room}: {now:?} {w:?}"
                    );
                }
            }
            wider = Some(now);
        }
    }
}

/// CASE (C) LAYS R2, R5 AND R7 (§1.3 (c), §2.4) at 166, 96 and 66 cells, R2 by one cell: its 30
/// lead cells, 65 chart cells at 8 a day and 36 tail cells take 135 with the two gaps, and R1's
/// 97 chart cells take 167. R2's T0 says phrase 1 in its short words, because main is full
/// before its reset (§2.3): the rotation leaves it at its low mark ~Fri 08:00, comes back to it
/// once alt1 is low too and alt2 is out (~Sat 02:00), and it fills ~Sat 04:00, so no run of main
/// holds its reset. Every account is out once the three weeks' 25 + 20 + 10 points are
/// spent at 36% a day, 36 h 40 min from now (Sat 05:40), said `~Sat 05:00` at the hour's grain
/// of a start a day or more ahead (§3.2). Negative controls: R2 laid with T1, as R3 is, says
/// `↺ Sat 10:00 · ╳ ~Sat 05:00` at 166 cells under the same lead at 8 cells a day; phrase 1 read
/// off main's curve where the work first leaves it (97.5%) says the design's first mock's
/// `~98% by ↺ Sat 10:00`.
#[test]
fn case_c_lays_r2_r5_r7() {
    let view = case_c().outlook().quota("main", None, true);
    let stop = "\u{2573} ~Sat 05:00";
    let reset = "\u{21ba} Sat 10:00";
    let rungs = [
        (
            166,
            "main 75% \u{b7} alt1 80% \u{b7} alt2 90%",
            8,
            format!("{reset} \u{b7} {stop}\u{2013}Sat 10:00"),
        ),
        (96, "main 75% +2", 6, format!("{reset} \u{b7} {stop}")),
        (66, "main 75% +2", 4, stop.to_owned()),
    ];
    for (room, lead, per_day, tail) in rungs {
        let line = laid(&view, room).unwrap();
        assert_eq!(
            (
                line.text_of(&line.lead).as_str(),
                line.per_day,
                line.text_of(&line.tail)
            ),
            (lead, per_day, tail),
            "at {room}"
        );
    }
    assert_eq!(laid(&view, 166).unwrap().cells.len(), 135);
    assert_eq!(laid(&view, 167).unwrap().per_day, 12);
    let main: Vec<_> = view
        .chart
        .runs
        .iter()
        .filter(|r| r.label == "main" && r.from < SAT7)
        .map(|r| {
            let (first, last) = (r.knots.first().unwrap(), r.knots.last().unwrap());
            (first.t - T5, first.x10, last.t - T5, last.x10)
        })
        .collect();
    assert_eq!(
        main,
        [
            (0, Some(750), 15 * H, Some(975)),
            (33 * H + 20 * MIN, Some(975), 35 * H, Some(1000)),
        ]
    );
    let own = view.tail.own.as_ref().unwrap();
    assert_eq!((own.by_reset, own.reset.as_str()), (None, "Sat 10:00"));
    assert_eq!(view.chart.stops, [(T5 + 36 * H + 40 * MIN, SAT7, false)]);
}

/// A row the design draws (§1.3) at one of its widths: the room, the lead, the chart's cells a
/// day, the tail, and the whole row cell for cell.
type Mock = (usize, &'static str, u8, &'static str, &'static str);

/// The lead, the chart's cells a day and the tail `view` lays at `room`.
fn rung_at(view: &QuotaView, room: usize) -> (String, u8, String) {
    let line = laid(view, room).unwrap_or_else(|| panic!("nothing fits {room}"));
    (
        line.text_of(&line.lead),
        line.per_day,
        line.text_of(&line.tail),
    )
}

/// `view` lays each of `mocks` at its room as the design draws it.
fn lays_the_mocks(view: &QuotaView, mocks: &[Mock]) {
    for &(room, lead, per_day, tail, row) in mocks {
        let (l, k, t) = rung_at(view, room);
        assert_eq!(
            (l.as_str(), k, t.as_str()),
            (lead, per_day, tail),
            "at {room}"
        );
        assert_eq!(laid(view, room).unwrap().text(), row, "at {room}");
    }
}

/// CASE (A) LAYS R1, R3 AND R6 (§1.3 (a), §2.4) at 166, 96 and 66 cells, each row cell for cell
/// as the design draws it: R1, the widest rung, takes 133 cells; at 96, R3 takes 93 and R2's
/// T0 would take 101; at 66, R6 takes 56 and R5's 6 cells a day would take 72 (with no phrase 2,
/// R6's T1 and R7's T2 say the same `↺ Sat 10:00`). Read on this Mac at 30% a day ago, 35% at
/// Thu 05:00 and 40% now, main runs at 10% a day: the past is `▂` (30%) until the cell that ends
/// at Thu 05:00, then `▃` (35% and 40%). The fill is held in tenths, rounded down, so where it
/// meets a half level on a cell's end (Fri 02:00 and Sat 08:00 at 8 cells a day, each of the next
/// week's steps at 12) the cell keeps the level below. Main's week holds 40 + 41 h × 10/24 =
/// 57.08% at its reset Sat 10:00, said `~57%`; its next week, from 0 then, holds 52.9% at the
/// chart's end, and nothing stops. Negative controls, each of which the ladder's checks of lead,
/// chart grain and tail at the three widths pass: a rung that must leave a cell spare (`<` for
/// `<=` in [`QuotaView::lay`]) lays R2 at 133 cells; the eighths truncated ([`level`] without its
/// half) draw R1's past `▂▂▂▂▂▂▂▂▂▂▂▃`.
#[test]
fn case_a_lays_r1_r3_r6() {
    let view = case_a().outlook().quota("main", None, true);
    let mocks: [Mock; 3] = [
        (
            166,
            "main 40% used",
            12,
            "~57% by ↺ Sat 10:00",
            "main 40% used  ▂▂▂▂▂▃▃▃▃▃▃▃▕🭺🭺🭺🭺🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭸↺▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁\
             🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹  ~57% by ↺ Sat 10:00",
        ),
        (
            96,
            "main 40% used",
            8,
            "↺ Sat 10:00",
            "main 40% used  ▂▂▂▃▃▃▃▃▕🭺🭺🭺🭹🭹🭹🭹🭹🭹🭹🭹🭹🭹↺▁▁▁▁▁▁▁▁▁▁▁▁▁▁🭻🭻🭻🭻🭻🭻🭻🭻🭻🭻\
             🭺🭺🭺🭺🭺🭺🭺🭺🭺🭺🭹🭹🭹🭹🭹🭹🭹🭹  ↺ Sat 10:00",
        ),
        (
            66,
            "main 40%",
            4,
            "↺ Sat 10:00",
            "main 40%  ▂▃▃▃▕🭺🭹🭹🭹🭹🭹↺▁▁▁▁▁▁▁🭻🭻🭻🭻🭻🭺🭺🭺🭺🭺🭹🭹🭹🭹  ↺ Sat 10:00",
        ),
    ];
    lays_the_mocks(&view, &mocks);
    let (r1, r2, r3, r5, r6) = (
        ("main 40% used", 12, "~57% by ↺ Sat 10:00"),
        ("main 40% used", 8, "~57% by ↺ Sat 10:00"),
        ("main 40% used", 8, "↺ Sat 10:00"),
        ("main 40%", 6, "↺ Sat 10:00"),
        ("main 40%", 4, "↺ Sat 10:00"),
    );
    for (room, (lead, k, tail)) in [
        (133, r1),
        (132, r2),
        (101, r2),
        (100, r3),
        (72, r5),
        (71, r6),
    ] {
        assert_eq!(
            rung_at(&view, room),
            (lead.to_owned(), k, tail.to_owned()),
            "at {room}"
        );
    }
    let fills: Vec<_> = view
        .chart
        .runs
        .iter()
        .map(|r| {
            let knots: Vec<_> = r.knots.iter().map(|k| (k.t - T5, k.x10)).collect();
            (r.label.as_str(), knots)
        })
        .collect();
    assert_eq!(
        fills,
        [(
            "main",
            vec![
                (0, Some(400)),
                (41 * H, Some(570)),
                (41 * H, Some(0)),
                (7 * D, Some(529)),
            ]
        )]
    );
    assert_eq!(
        view.tail.own,
        Some(Own {
            by_reset: Some(57),
            reset: "Sat 10:00".into()
        })
    );
    assert!(view.chart.stops.is_empty() && view.tail.verdict.is_none());
}

/// CASE (B) LAYS R2, R5 AND R7 (§1.3 (b), §2.4) at 166, 96 and 66 cells, each row cell for cell
/// as the design draws it, with the midnights' ticks under R2's: R2 takes 140 cells and R1's 12
/// a day would take 172; R5 takes 94 and R4's full lead 99; R7 takes 64 and R6's T1 78. Read on
/// this Mac at 42% a day ago, 50% from Wed 23:00 and 63% now, main runs at 21% a day (8.75
/// tenths an hour; the replay takes each instant to the second up). Its week reaches its low
/// mark (98%, from 97.5% half up) at Sat 08:26, which moves nothing with no other account, and
/// holds 98.875% at its reset Sat 10:00, said `~99%` (whole percent half up, at most 99: §2.1,
/// §2.3). Its next week, from 0 then, reaches its low mark at Thu 12 Nov 01:26 and is out at
/// 04:17, 4 d 18 h 17 min after the reset, said `~Thu 12 Nov 04:00` at the hour's grain of a
/// start a day or more ahead (§3.2), until its reset Sat 14 Nov 10:00, past the chart's end. At 8
/// cells a day the fill lands 43 min before the end of its 3-hour cell, which the stop covers
/// less than half of, so the cell is the line at a projected fill, `▔` (§2.2), and the 4 cells
/// after it are `╳`. [`QuotaView::sample`], which other crates' tests draw, says the same words.
/// Negative controls, each of which the ladder's checks of lead, chart grain and tail at the
/// three widths pass: a rung that must leave a cell spare (`<` for `<=` in [`QuotaView::lay`])
/// lays R2 at 172 cells; the eighths truncated ([`level`] without its half) draw R2's future
/// `🭸🭸🭸🭸🭷…`; the sample's first words (`~98%`, `~Thu 12 Nov 05:00`) fail its pin.
#[test]
fn case_b_lays_r2_r5_r7() {
    let view = case_b().outlook().quota("main", None, true);
    let full = "~99% by ↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00–Sat 14 Nov 10:00";
    let short = "↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00";
    let stop = "╳ ~Thu 12 Nov 04:00";
    let mocks: [Mock; 3] = [
        (
            166,
            "main 63% used",
            8,
            full,
            "main 63% used  ▃▄▄▄▄▄▄▅▕🭸🭸🭷🭷🭷🭷🭶🭶🭶🭶🭶🭶🭶↺▁▁▁▁▁▁🭻🭻🭻🭻🭻🭺🭺🭺🭺🭺🭹🭹🭹🭹🭹🭸🭸🭸🭸\
             🭷🭷🭷🭷🭷🭶🭶🭶🭶🭶🭶🭶▔╳╳╳╳  ~99% by ↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00–Sat 14 Nov 10:00",
        ),
        (
            96,
            "main 63%",
            6,
            short,
            "main 63%  ▃▄▄▄▄▅▕🭸🭷🭷🭷🭷🭶🭶🭶🭶🭶↺▁▁▁▁🭻🭻🭻🭻🭺🭺🭺🭹🭹🭹🭹🭸🭸🭸🭷🭷🭷🭷🭶🭶🭶🭶🭶▔╳╳╳  \
             ↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00",
        ),
        (
            66,
            "main 63%",
            4,
            stop,
            "main 63%  ▄▄▄▅▕🭸🭷🭷🭶🭶🭶↺▁▁▁🭻🭻🭺🭺🭺🭹🭹🭸🭸🭷🭷🭷🭶🭶🭶▔╳╳  ╳ ~Thu 12 Nov 04:00",
        ),
    ];
    lays_the_mocks(&view, &mocks);
    // The second line of R2's mock: each midnight's tick, in the chart's cells.
    let r2 = laid(&view, 166).unwrap();
    let ticks: Vec<i64> = r2.ticks.iter().map(|p| p / CELL_PLACES).collect();
    assert_eq!(ticks, [2, 11, 19, 27, 35, 43, 51, 59]);
    // §2.6's titles of a past column and a future one, at 8 cells a day.
    let utc = Offsets::fixed(0);
    assert_eq!(
        view.column_title(8, 1, &utc).as_deref(),
        Some("Wed 4 Nov 20:00–23:00 · main 50% (read on this Mac)")
    );
    assert_eq!(
        view.column_title(8, 9 + 39, &utc).as_deref(),
        Some("~Tue 10 Nov 14:00–17:00 · main ~69% (projected)")
    );
    let s = |lead: &str, k: u8, tail: &str| (lead.to_owned(), k, tail.to_owned());
    for (room, rung) in [
        (172, s("main 63% used", 12, full)),
        (171, s("main 63% used", 8, full)),
        (99, s("main 63% used", 6, short)),
        (98, s("main 63%", 6, short)),
        (78, s("main 63%", 4, short)),
        (77, s("main 63%", 4, stop)),
    ] {
        assert_eq!(rung_at(&view, room), rung, "at {room}");
    }
    // `x10` tenths at 21% a day, to the second up.
    let at_pace = |x10: i64| (x10 * D + 209) / 210;
    let reset = SAT7 - T5;
    let fills: Vec<_> = view
        .chart
        .runs
        .iter()
        .map(|r| {
            let knots: Vec<_> = r.knots.iter().map(|k| (k.t - T5, k.x10)).collect();
            (r.label.as_str(), knots)
        })
        .collect();
    assert_eq!(
        fills,
        [(
            "main",
            vec![
                (0, Some(630)),
                (at_pace(975 - 630), Some(975)),
                (reset, Some(988)),
                (reset, Some(0)),
                (reset + at_pace(975), Some(975)),
                (reset + at_pace(1000), Some(1000)),
            ]
        )]
    );
    assert_eq!(at_pace(1000), 4 * D + 18 * H + 17 * MIN + 9);
    assert_eq!(view.chart.stops, [(SAT7 + at_pace(1000), SAT14, false)]);
    assert_eq!(
        view.tail.own,
        Some(Own {
            by_reset: Some(99),
            reset: "Sat 10:00".into()
        })
    );
    let sample = QuotaView::sample();
    assert_eq!(
        (&sample.tail, &sample.title, &sample.spoken),
        (&view.tail, &view.title, &view.spoken)
    );
    assert_eq!(sample.fallback(), view.fallback());
}

/// CASE (D) LAYS ITS WORDS FULL, THEN SHORT (§1.3 (d), §2.4): with this tab's week unread there
/// is no chart, and its full words take 64 cells, so 166, 96 and 66 lay them alike; from 63
/// cells down to 24 it says them short, and nothing fits 23. Negative control: a form that must
/// leave a cell spare (`<` for `<=` in [`QuotaView::lay`]) says the short words at 64 cells,
/// though the ladder's check at 66 passes.
#[test]
fn case_d_lays_its_words_full_then_short() {
    let view = case_d().outlook().quota("main", None, true);
    let row = format!("main  {NO_FIGURE_FULL}");
    for room in [166, 96, 66, 64] {
        let line = laid(&view, room).unwrap();
        assert_eq!(
            (line.text_of(&line.lead).as_str(), line.per_day),
            ("main", 0),
            "at {room}"
        );
        assert_eq!(line.text(), row, "at {room}");
    }
    for room in [63, 24] {
        assert_eq!(
            laid(&view, room).unwrap().text(),
            format!("main  {NO_FIGURE}"),
            "at {room}"
        );
    }
    assert!(laid(&view, 23).is_none());
}

/// A PAUSED RECOVERY SHOWS NO FORECAST (moved from the footer's outlook tests): a pane whose
/// session policy paused its recovery, or whose observer is busy, gets the pool's observations
/// alone (`Outlook::observations_only` with the footer's refusal). The view keeps every named
/// figure, this tab's past and its reset (`↺` over `┄`), and draws no run, stop, hand-off or
/// lean; no phrase 2, no `~N%`, no pace, no chain spoken. A tab the rotation cannot move is the
/// same. A recovery neither paused nor busy refuses nothing. Negative control: saying a refused
/// pace's words whatever the carrier says `session recovery paused` as phrase 2.
#[test]
fn a_paused_recovery_shows_no_forecast() {
    for (fx, tab) in [(case_c(), "main"), (scenario_b(), "me")] {
        let out = fx.outlook();
        let raw = out.quota(tab, None, true);
        assert!(!raw.chart.runs.is_empty() && raw.tail.verdict.is_some());
        let mut facts = FooterFacts::default();
        let mut views = Vec::new();
        for (phase, paused, busy) in [
            (ProgressPhase::Paused, false, false),
            (ProgressPhase::Paused, false, true),
            (ProgressPhase::Waiting, false, true),
            (ProgressPhase::Unknown, false, true),
            (
                ProgressPhase::Switching {
                    to: "alternate".into(),
                },
                true,
                false,
            ),
        ] {
            let mut progress = AccountProgress::unknown(100);
            progress.phase = phase.clone();
            progress.forecasts_paused = paused;
            facts.account_progress = Some(progress);
            facts.account_progress_busy = busy;
            let known_pause = paused || phase == ProgressPhase::Paused;
            let refusal = facts.account_forecast_refusal().expect("a refusal");
            assert_eq!(
                refusal,
                if known_pause {
                    Refusal::SessionPaused
                } else {
                    Refusal::ObserverBusy
                },
                "{phase:?}, busy={busy}"
            );
            views.push(out.observations_only(refusal).quota(tab, None, true));
        }
        views.push(out.quota(tab, None, false));
        for view in views {
            let figures = |v: &QuotaView| -> Vec<(String, Figure)> {
                v.named
                    .iter()
                    .map(|n| (n.label.clone(), n.figure))
                    .collect()
            };
            assert_eq!(figures(&view), figures(&raw));
            assert_eq!(view.chart.past, raw.chart.past);
            assert!(view.chart.runs.is_empty() && view.chart.stops.is_empty());
            assert!(view.chart.marks.iter().all(|(_, m)| *m == Mark::Reset));
            assert!(
                future(&view.chart, 12)
                    .iter()
                    .all(|c| matches!(c, Cell::Unknown | Cell::Reset))
            );
            assert!(future(&view.chart, 12).contains(&Cell::Reset));
            assert_eq!(view.tail.verdict, None);
            let own = view.tail.own.as_ref().expect("the reset, an observation");
            assert_eq!(own.by_reset, None);
            assert!(!view.title.contains("pace") && !view.title.contains(" out ~"));
            assert!(!view.spoken.contains("Next 7 days"), "{}", view.spoken);
            assert!(view.lanes.iter().all(|l| l.ahead.is_empty()));
        }
        facts.account_progress_busy = false;
        for phase in [ProgressPhase::Unknown, ProgressPhase::Disabled] {
            let mut progress = AccountProgress::unknown(101);
            progress.phase = phase;
            facts.account_progress = Some(progress);
            assert_eq!(facts.account_forecast_refusal(), None);
        }
    }
}

/// HOVERING A CHART COLUMN NAMES ITS INSTANT (§2.6): every cell of case (b)'s and case (c)'s
/// charts but the now cell has a title that names a span or an instant, at every grain; a past
/// cell the reading it holds, a level its projected fill, a `→` its hand-off with the account
/// it leaves at its fill (`out` where the projection fills it), said exactly where the work moves
/// at a vendor reset, a `╳` its stop until the reset that ends it. Negative control: making
/// [`QuotaView::column_title`] map every cell through the past arm titles case (c)'s `→` cell
/// `no reading on this Mac`, and the hand-off pin fails.
#[test]
fn a_chart_column_names_its_instant() {
    let utc = Offsets::fixed(0);
    let mut seen = Vec::new();
    for (name, fx) in [("b", case_b()), ("c", case_c())] {
        let view = fx.outlook().quota("main", None, true);
        for per_day in PER_DAY {
            let all = cells(&view.chart, per_day);
            let k = usize::from(per_day);
            assert_eq!(view.column_title(per_day, k, &utc), None, "the now cell");
            assert_eq!(
                view.column_title(per_day, all.len(), &utc),
                None,
                "past the end"
            );
            for (i, cell) in all.iter().enumerate().filter(|(i, _)| *i != k) {
                let title = view
                    .column_title(per_day, i, &utc)
                    .unwrap_or_else(|| panic!("case ({name}) cell {i} {cell:?} at {per_day}"));
                assert!(title.contains(" \u{b7} "), "{title}");
                assert!(!title.contains("ultra"), "{title}");
                seen.push((name, per_day, *cell, title));
            }
        }
    }
    let at = |name: &str, per_day: u8, cell: Cell| {
        seen.iter()
            .filter(|(n, p, c, _)| *n == name && *p == per_day && *c == cell)
            .map(|(.., t)| t.as_str())
            .collect::<Vec<_>>()
    };
    // Case (b) at 12 a day: the oldest past cell, Wed 4 Nov 17:00–19:00, holds 42%.
    let past: Vec<_> = seen
        .iter()
        .filter(|(n, p, c, _)| *n == "b" && *p == 12 && matches!(c, Cell::Read(_)))
        .map(|(.., t)| t.as_str())
        .collect();
    assert_eq!(
        past[0],
        "Wed 4 Nov 17:00\u{2013}19:00 \u{b7} main 42% (read on this Mac)"
    );
    assert_eq!(
        at("b", 12, Cell::Reset),
        ["Sat 10:00 \u{b7} main's week resets"]
    );
    let gaps = at("b", 12, Cell::Gap);
    assert!(!gaps.is_empty());
    assert!(
        gaps.iter().all(|t| t.ends_with(" \u{b7} every account out")
            && t.starts_with("from ~Thu 12 Nov ")),
        "{gaps:?}"
    );
    // Case (c): the hand-offs, each with the account the work leaves and its fill.
    let moves = at("c", 12, Cell::Handoff);
    assert!(moves.len() >= 2, "{moves:?}");
    assert!(moves[0].contains("the work moves from main (") && moves[0].ends_with(") to alt1"));
    assert!(moves[1].contains("the work moves from alt1 (") && moves[1].ends_with(") to alt2"));
    // A move off an account the projection fills says `out`, never 100% (§2.1); the move at
    // main's reset, where the wall lifts, says that reset exactly (§2.3), as the gap does.
    assert!(moves.iter().any(|t| t.contains(" (out) to ")), "{moves:?}");
    assert!(
        moves.contains(&"Sat 10:00 \u{b7} the work moves to main"),
        "{moves:?}"
    );
    let out: Vec<_> = at("c", 12, Cell::Gap);
    assert!(
        out.iter()
            .all(|t| t.ends_with("every account out until main's reset")),
        "{out:?}"
    );
    let ahead: Vec<_> = seen
        .iter()
        .filter(|(n, p, c, _)| *n == "c" && *p == 12 && matches!(c, Cell::Ahead(_)))
        .map(|(.., t)| t.as_str())
        .collect();
    assert!(
        ahead
            .iter()
            .all(|t| t.starts_with('~') && t.ends_with("(projected)")),
        "{ahead:?}"
    );
}

/// A SPAN IS SAID WITH ITS DATE ONCE: an end on the same local day keeps only its time, one on
/// another day says its day too. Negative control: saying the end with [`when`] alone drops the
/// second date.
#[test]
fn a_span_is_dated_once() {
    let utc = Offsets::fixed(0);
    assert_eq!(
        span_dated(T5 - 21 * H, T5 - 19 * H, &utc),
        "Wed 4 Nov 20:00\u{2013}22:00"
    );
    assert_eq!(
        span_dated(T5 + 7 * H, T5 + 9 * H, &utc),
        "Fri 6 Nov 00:00\u{2013}02:00"
    );
    assert_eq!(
        span_dated(T5 + 6 * H, T5 + 8 * H, &utc),
        "Thu 5 Nov 23:00\u{2013}Fri 6 Nov 01:00"
    );
}

/// The reset milestone is laid bold wherever the glass draws it (the tail's `↺`, the chart's reset
/// cell, the fallback unit's piece): its font glyph, laid regular, reads fainter at 1x than the
/// quiet `·` beside it, though §1.3 inks it `lit`. Negative control: [`heavy`] reduced to
/// `role == Role::Warn` (as S5b laid it) leaves case (b)'s tail `↺` regular, which the first
/// assertion names.
#[test]
fn the_reset_mark_is_laid_bold() {
    let mut seen = (0, 0);
    for (name, view) in scenarios() {
        for room in 0..=200 {
            let Some(line) = laid(&view, room) else {
                continue;
            };
            for (i, (c, role, bold)) in line.cells.iter().enumerate() {
                if *c == RESET_MARK {
                    seen.0 += 1;
                    assert!(bold, "{name} at {room}: cell {i} ({role:?}) is regular");
                }
            }
        }
        for form in view.fallback() {
            for piece in form.iter().filter(|p| p.text.contains(RESET_MARK)) {
                seen.1 += 1;
                assert!(piece.bold, "{name}: fallback piece {piece:?} is regular");
            }
        }
    }
    assert!(
        seen.0 > 0 && seen.1 > 0,
        "the scenarios draw a reset on the row and in the fallback: {seen:?}"
    );
    assert!(!heavy('·', Role::Quiet) && heavy('↺', Role::Lit) && heavy('╳', Role::Warn));
}

/// A SCENE'S TAP WORDS ARE THE TAP'S OWN FOR ITS CASE ([`crate::harness::quota_scenes::tap_words`]):
/// the private-dev scene lays, for the claim to read, the line the tap prints for the case's
/// reading, through [`crate::harness::tap::Reading::week_words`], never hand-typed words. Each
/// parses as the tap's grammar ([`crate::harness::tap::parse_line`]) whole, says the figure the
/// view's lead says, and case (d)'s is the tap's no-figure line. CONTROL: case (b) read at 64%
/// says 64%, so the words follow the case.
#[test]
fn a_scenes_tap_words_are_the_taps_own_for_its_case() {
    use crate::harness::quota_scenes::{cases, tap_words};
    use crate::harness::tap::{NO_WEEK, parse_line};
    let said: Vec<(&str, String)> = cases()
        .iter()
        .map(|(name, view)| (*name, tap_words(view)))
        .collect();
    assert_eq!(
        said,
        [
            ("a", "week 40% used \u{b7} resets Sat 10:00".to_owned()),
            ("b", "week 63% used \u{b7} resets Sat 10:00".to_owned()),
            ("c", "week 75% used \u{b7} resets Sat 10:00".to_owned()),
            ("d", NO_WEEK.to_owned()),
        ]
    );
    for (name, words) in &said {
        assert_eq!(
            parse_line(words),
            Some(words.chars().count()),
            "{name}: {words}"
        );
    }
    let mut moved = case_b();
    moved.read("main", None, Some((640, SAT7)), T5);
    let view = moved.outlook().quota("main", None, true);
    assert_eq!(tap_words(&view), "week 64% used \u{b7} resets Sat 10:00");
}
