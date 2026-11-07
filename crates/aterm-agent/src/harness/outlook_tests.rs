// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the accounts outlook ([`super::outlook`]): the appendix scenarios of
//! `docs/DESIGN-accounts-outlook-2026-10-25.md`, all synthetic, at a fixed `now`.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::harness::account_series::{Fig, PlanSaid, Said, SeriesRow};
use crate::harness::accounts::{Class, Mode, Roster, Row, SessionRow, State, TapRead, WindowRead};

/// Sun 25 Oct 2026 19:30, UTC (the words below are said at offset 0).
pub(crate) const NOW: i64 = 1_792_956_600;
pub(crate) const MIN: i64 = 60;
pub(crate) const H: i64 = 3600;
pub(crate) const D: i64 = 86_400;
/// Thu 29 Oct 09:00: `me`'s week resets.
const THU9: i64 = NOW + 307_800;
/// Fri 30 Oct 14:00: `alt1`'s week resets.
const FRI14: i64 = NOW + 412_200;
/// Thu 22 Oct 11:00: `alt2`'s week reset, passed.
const THU22: i64 = NOW - 289_800;
/// Sat 31 Oct 08:00: `pool1`'s week resets.
pub(crate) const SAT8: i64 = NOW + 477_000;
const MACHINE: &str = "0a0b";

/// The clock the words below are said on.
pub(crate) fn utc() -> Offsets {
    Offsets::fixed(0)
}

/// A synthetic roster, state and series (the quota view's tests build on it too).
pub(crate) struct Fx {
    pub(crate) roster: Roster,
    pub(crate) state: State,
    pub(crate) enrolled: BTreeSet<String>,
    pub(crate) series: BTreeMap<String, Vec<SeriesRow>>,
    pub(crate) renewals: BTreeMap<String, String>,
    pub(crate) mode: Mode,
    pub(crate) credits: bool,
    pub(crate) now: i64,
    /// The local clock's offset at each instant.
    pub(crate) offset: fn(i64) -> i64,
}

impl Fx {
    /// The first row is the default login; the others have their own login, enrolled.
    pub(crate) fn new(rows: &[(&str, Class)]) -> Fx {
        let roster = Roster {
            rows: rows
                .iter()
                .enumerate()
                .map(|(i, (label, class))| Row {
                    label: (*label).to_owned(),
                    class: *class,
                    dir: (i > 0).then(|| format!("/logins/{label}")),
                    ..Row::default()
                })
                .collect(),
        };
        let enrolled = rows.iter().map(|(l, _)| (*l).to_owned()).collect();
        Fx {
            roster,
            state: State {
                machine: MACHINE.into(),
                active: rows.first().map(|(l, _)| (*l).to_owned()),
                proven: true,
                ..State::default()
            },
            enrolled,
            series: BTreeMap::new(),
            renewals: BTreeMap::new(),
            mode: Mode::Early,
            credits: false,
            now: NOW,
            offset: |_| 0,
        }
    }

    /// The tap's reading of `label` at `at`, through the store's own writer, and its series row.
    pub(crate) fn read(
        &mut self,
        label: &str,
        five: Option<(u32, i64)>,
        week: Option<(u32, i64)>,
        at: i64,
    ) {
        let window = |w: Option<(u32, i64)>| {
            w.map(|(pct_x10, reset)| WindowRead {
                pct_x10,
                reset,
                refilled: None,
            })
        };
        let read = TapRead {
            five_hour: window(five),
            seven_day: window(week),
            at,
            seen_before: true,
            born: None,
            key: None,
        };
        let _ = accounts::note_reading_held(&mut self.state, label, &read, at);
        self.fig(label, at, five, week);
    }

    /// A series `fig` row of `label` at `t`.
    pub(crate) fn fig(
        &mut self,
        label: &str,
        t: i64,
        five: Option<(u32, i64)>,
        week: Option<(u32, i64)>,
    ) {
        let fig = |w: Option<(u32, i64)>| {
            w.map(|(pct_x10, reset)| Fig {
                pct_x10,
                reset,
                at: t,
                src: "tap".into(),
                epoch: None,
            })
        };
        let rows = self.series.entry(label.to_owned()).or_default();
        rows.push(SeriesRow {
            t,
            by: MACHINE.into(),
            said: Said::Fig {
                five_hour: fig(five),
                seven_day: fig(week),
                ac: None,
                key: None,
            },
        });
        rows.sort_by_key(|r| r.t);
    }

    pub(crate) fn plan(&mut self, label: &str, word: &str, tier: Option<&str>, t: i64) {
        let rows = self.series.entry(label.to_owned()).or_default();
        rows.push(SeriesRow {
            t,
            by: MACHINE.into(),
            said: Said::Plan(PlanSaid {
                word: word.into(),
                tier: tier.map(str::to_owned),
                src: "profile".into(),
            }),
        });
        rows.sort_by_key(|r| r.t);
    }

    pub(crate) fn ran(&mut self, label: &str, at: i64) {
        self.state
            .accounts
            .entry(label.to_owned())
            .or_default()
            .ran_at = Some(at);
    }

    pub(crate) fn session(&mut self, sid: &str, label: &str, at: i64) {
        self.state.sessions.insert(
            sid.into(),
            SessionRow {
                account: label.into(),
                at,
                held: None,
            },
        );
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            roster: &self.roster,
            state: &self.state,
            enrolled: &self.enrolled,
            series: &self.series,
            renewals: &self.renewals,
            mode: self.mode,
            credits: self.credits,
            now: self.now,
            offset: self.offset,
        }
    }

    pub(crate) fn outlook(&self) -> Outlook {
        outlook(&self.inputs())
    }
}

/// `me` at 58% of a Max 20x week, 16 points of it a day for the last 24 h, its 5h at 38% since
/// 17:00 (15.2 points an hour).
fn me_at_today(fx: &mut Fx) {
    me_at(fx, 580);
}

/// `me` at `week` tenths of a Max 20x week now, after 16 points of it in the last 24 h.
fn me_at(fx: &mut Fx, week: u32) {
    fx.plan("me", "max", Some("20x"), NOW - H);
    for (t, rose, five) in [
        (NOW - 24 * H, 0, None),
        (NOW - 16 * H, 53, None),
        (NOW - 8 * H, 107, None),
        (NOW - H, 153, Some(228)),
        (NOW - 30 * MIN, 157, Some(304)),
    ] {
        fx.fig(
            "me",
            t,
            five.map(|f| (f, NOW + 150 * MIN)),
            Some((week - 160 + rose, THU9)),
        );
    }
    fx.read("me", Some((380, NOW + 150 * MIN)), Some((week, THU9)), NOW);
    fx.ran("me", NOW);
    fx.session("s-me", "me", NOW);
}

/// `alt1`, Max 20x, its week at `week` tenths read 19:30 (now), run here last at 15:30; its
/// 5h window, reset 18:40, unopened since.
fn alt1_read(fx: &mut Fx, week: u32) {
    alt1_read_at(fx, week, NOW);
}

/// `alt1` as [`alt1_read`], its reading taken at `at` (a 5h figure only before its reset).
fn alt1_read_at(fx: &mut Fx, week: u32, at: i64) {
    fx.plan("alt1", "max", Some("20x"), NOW - 2 * D);
    fx.ran("alt1", NOW - 4 * H);
    let five_reset = NOW - 50 * MIN;
    let five = (at < five_reset).then_some((120, five_reset));
    fx.read("alt1", five, Some((week, FRI14)), at);
}

/// Scenario A with `alt1` read 16:30 and idle here since: `idle-here`, a bound, since another
/// Mac may prefer and run it too.
fn scenario_a_idle() -> Fx {
    let mut fx = scenario_a();
    fx.state.accounts.remove("alt1");
    fx.series.remove("alt1");
    alt1_read_at(&mut fx, 310, NOW - 3 * H);
    fx
}

/// Scenario A: covered.
fn scenario_a() -> Fx {
    let mut fx = Fx::new(&[
        ("me", Class::Preferred),
        ("alt1", Class::Preferred),
        ("alt2", Class::Preferred),
        ("pool1", Class::Pool),
    ]);
    me_at_today(&mut fx);
    alt1_read(&mut fx, 310);
    fx.plan("alt2", "max", Some("5x"), NOW - 6 * D);
    fx.ran("alt2", THU22 - 3 * D - H);
    fx.read("alt2", None, Some((710, THU22)), THU22 - 3 * D);
    fx.read("pool1", None, Some((400, SAT8)), NOW - 5 * H);
    fx
}

/// Scenario B: a gap.
pub(crate) fn scenario_b() -> Fx {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    me_at_today(&mut fx);
    alt1_read(&mut fx, 920);
    fx
}

/// Scenario C: an account never read.
pub(crate) fn scenario_c() -> Fx {
    let mut fx = Fx::new(&[
        ("me", Class::Preferred),
        ("alt1", Class::Preferred),
        ("pool1", Class::Pool),
    ]);
    me_at_today(&mut fx);
    alt1_read(&mut fx, 920);
    fx
}

/// Scenario D: `me`'s 5h at 81% since 17:00 (32.4 points an hour), `alt1`'s at 95% read 19:20.
pub(crate) fn scenario_d() -> Fx {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    fx.plan("me", "max", Some("20x"), NOW - H);
    for (t, week) in [(NOW - 24 * H, 420), (NOW - 8 * H, 527)] {
        fx.fig("me", t, None, Some((week, THU9)));
    }
    fx.fig(
        "me",
        NOW - H,
        Some((486, NOW + 150 * MIN)),
        Some((573, THU9)),
    );
    fx.read("me", Some((810, NOW + 150 * MIN)), Some((580, THU9)), NOW);
    fx.ran("me", NOW);
    fx.session("s-me", "me", NOW);
    fx.plan("alt1", "max", Some("20x"), NOW - 2 * D);
    fx.ran("alt1", NOW - 30 * MIN);
    fx.read(
        "alt1",
        Some((950, NOW + 100 * MIN)),
        Some((310, FRI14)),
        NOW - 10 * MIN,
    );
    fx
}

fn labels(pass: &Pass) -> Vec<(&str, i64, i64, Ends)> {
    pass.chain
        .iter()
        .map(|l| (l.label.as_str(), l.from, l.to, l.ends))
        .collect()
}

fn stops(pass: &Pass) -> Vec<(i64, StopEnd)> {
    pass.stops.iter().map(|s| (s.from, s.end)).collect()
}

fn rise_of(fx: &Fx, w: Window, lookback: i64) -> (i64, Option<i64>, bool) {
    let multiples: BTreeMap<String, u32> = fx
        .roster
        .rows
        .iter()
        .map(|r| (r.label.clone(), 20))
        .collect();
    rise(&fx.series, &multiples, 20, w, lookback, fx.now)
}

/// T3: THE PACE IS THE WORK'S, ACROSS A ROTATION. 12 points on `me` over 18 h, then 4 on `alt1`
/// over its last 1 h 50 m (the work rotated at 17:40), give 16 points of a Max 20x week a day,
/// on the day's basis. Negative control: a carrier-only slope (`alt1`'s rows alone) refuses or
/// gives another pace.
#[test]
fn demand_survives_a_rotation() {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    fx.plan("me", "max", Some("20x"), NOW - 3 * D);
    fx.plan("alt1", "max", Some("20x"), NOW - 3 * D);
    for (t, week) in [
        (NOW - 24 * H, 460),
        (NOW - 18 * H, 500),
        (NOW - 12 * H, 540),
        (NOW - 6 * H, 580),
    ] {
        fx.fig("me", t, None, Some((week, THU9)));
    }
    fx.read("me", None, Some((580, THU9)), NOW - 6 * H);
    for (t, week) in [(NOW - 110 * MIN, 310), (NOW - 55 * MIN, 330)] {
        fx.fig("alt1", t, None, Some((week, FRI14)));
    }
    fx.read("alt1", Some((300, NOW + 3 * H)), Some((350, FRI14)), NOW);
    fx.state.active = Some("alt1".into());
    fx.session("s-1", "alt1", NOW);
    let out = fx.outlook();
    assert_eq!(out.carrier.as_deref(), Some("alt1"));
    let pace = out.demand.week.expect("the week's pace stands");
    assert_eq!(pace.basis, Basis::Day);
    assert_eq!(pace.span_s, WEEK_LOOKBACK_S);
    assert_eq!(
        pace.x10_m_per_day,
        160 * 20,
        "16 points of a 20x week a day, the rise of both rows"
    );
}

/// THE PACE READS A WEEK AND ITS LOOKBACK OF THE SERIES, as the card and the report read it
/// ([`account_series::outlook_view`]): a weekly figure seven and a half days old, then nothing
/// until a week that began 12 hours ago, is a row before the 24 h lookback, so the week's pace
/// spreads the new week's rise over the whole lookback. Negative control: a view of seven days
/// drops the old figure, and the pace divides by the new week's 12 hours (twice the pace).
#[test]
fn the_pace_reads_a_week_and_its_lookback_of_the_series() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.plan("me", "max", Some("20x"), NOW - 9 * D);
    let began = NOW - 12 * H;
    let reset = began + 7 * D;
    fx.fig("me", NOW - 7 * D - 12 * H, None, Some((900, began)));
    fx.fig("me", NOW - 11 * H, None, Some((20, reset)));
    fx.read("me", None, Some((60, reset)), NOW - H);
    let rows = fx.series.remove("me").expect("a series");
    fx.series
        .insert("me".into(), account_series::outlook_view(rows, NOW));
    assert!(
        rise_of(&fx, Window::Week, WEEK_LOOKBACK_S).2,
        "the old figure is a row before the lookback"
    );
    let pace = fx.outlook().demand.week.expect("the week's pace stands");
    assert_eq!(pace.span_s, WEEK_LOOKBACK_S);
    assert_eq!(
        pace.x10_m_per_day,
        60 * 20,
        "six points of a 20x week a day"
    );
}

/// T4: RUNS SPLIT AT A RESET, A REFILL AND AN EPOCH, so no rise spans one. Negative control:
/// merging the runs counts the reset's drop as no rise at all.
#[test]
fn runs_split_at_reset_refill_epoch() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    let old = NOW - 2 * H;
    let new = old + 7 * D;
    // A run up to its reset; the new window's run; a refill inside it; then a limit reset used.
    fx.fig("me", NOW - 4 * H, None, Some((900, old)));
    fx.fig("me", NOW - 3 * H, None, Some((950, old)));
    fx.fig("me", NOW - 90 * MIN, None, Some((20, new)));
    fx.fig("me", NOW - 80 * MIN, None, Some((60, new)));
    fx.fig("me", NOW - 70 * MIN, None, Some((15, new)));
    fx.fig("me", NOW - 60 * MIN, None, Some((35, new)));
    let epoch = NOW - 55 * MIN;
    for (t, pct) in [(NOW - 50 * MIN, 50), (NOW - 40 * MIN, 70)] {
        let rows = fx.series.get_mut("me").unwrap();
        rows.push(SeriesRow {
            t,
            by: MACHINE.into(),
            said: Said::Fig {
                five_hour: None,
                seven_day: Some(Fig {
                    pct_x10: pct,
                    reset: new,
                    at: t,
                    src: "tap".into(),
                    epoch: Some(epoch),
                }),
                ac: None,
                key: None,
            },
        });
    }
    let rows = &fx.series["me"];
    let cut = runs(rows, Window::Week, NOW);
    let shape: Vec<(Began, Vec<u32>)> = cut
        .iter()
        .map(|run| (run.began, run.figs.iter().map(|f| f.pct_x10).collect()))
        .collect();
    assert_eq!(
        shape,
        vec![
            (Began::Window, vec![900, 950]),
            (Began::Window, vec![20, 60]),
            (Began::Refill, vec![15, 35]),
            (Began::Epoch(epoch), vec![50, 70]),
        ],
        "a new reset, a drop of a point or more and a used limit reset each start a run"
    );
    // Every run's rise, none negative: 50; 60 from the new window's beginning; 20 from the
    // refill's first row; 70 from the epoch.
    let (units, _, _) = rise_of(&fx, Window::Week, 24 * H);
    assert_eq!(units, (50 + 60 + 20 + 70) * 20);
}

/// T5: A WINDOW THAT BEGAN INSIDE THE LOOKBACK BEGINS EMPTY, at `reset − W`. Negative control:
/// starting at the window's first row under-counts its rise.
#[test]
fn a_window_begins_empty() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    let reset = NOW + 5 * H - 40 * MIN;
    fx.fig("me", NOW - 30 * MIN, Some((100, reset)), None);
    fx.fig("me", NOW, Some((300, reset)), None);
    let (units, earliest, before) = rise_of(&fx, Window::Five, H);
    assert!(!before);
    assert_eq!(
        earliest,
        Some(NOW - 40 * MIN),
        "the window opened 40 minutes ago"
    );
    assert_eq!(units, 300 * 20, "from 0 at its opening");
}

/// A GAP IN THE READINGS IS SPREAD OVER IT (§15.2): a run's start is its value at `a`, read
/// linearly between the rows on either side, so 53 points read 8 hours apart are not this hour's
/// pace. Negative control: starting at the newest row at or before `a` charges the whole gap to
/// the lookback.
#[test]
fn a_gap_in_the_readings_is_spread_over_it() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.fig("me", NOW - 8 * H, None, Some((527, THU9)));
    fx.fig("me", NOW, None, Some((580, THU9)));
    let (units, earliest, before) = rise_of(&fx, Window::Week, H);
    assert!(before);
    assert_eq!(
        earliest,
        Some(NOW - H),
        "the start is read at the lookback's start"
    );
    assert_eq!(
        units,
        (580 - 573) * 20,
        "an eighth of the gap's rise, not all of it"
    );
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.fig("me", NOW - 72 * H, None, Some((400, THU9)));
    fx.fig("me", NOW, None, Some((580, THU9)));
    let (units, _, _) = rise_of(&fx, Window::Week, 24 * H);
    assert_eq!(
        units,
        60 * 20,
        "a third of three days' rise falls in the last day"
    );
    // A row exactly at the lookback's start is the start, read as it is.
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.fig("me", NOW - H, None, Some((570, THU9)));
    fx.fig("me", NOW, None, Some((580, THU9)));
    assert_eq!(rise_of(&fx, Window::Week, H).0, 10 * 20);
}

/// T6: A PACE IS REFUSED ON A STALE, SKEWED, TOO EARLY OR MISSING FIGURE, each with its word, and
/// the same inputs with the defect removed project. Negative controls: each fixed fixture.
#[test]
fn pace_refusals() {
    // Stale: the carrier's week read 3 h ago while a session runs on it.
    let mut stale = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    me_at_today(&mut stale);
    stale.state.accounts.clear();
    stale.read("me", None, Some((580, THU9)), NOW - 3 * H);
    stale.session("s-me", "me", NOW);
    let out = stale.outlook();
    assert_eq!(out.demand.week.err().map(Refusal::word), Some("stale"));
    assert!(matches!(
        out.verdict,
        Verdict::Refused(Refusal::Stale { .. })
    ));
    assert!(out.week.is_none(), "no pass runs on a refused pace");
    stale.state.sessions.clear();
    assert!(
        stale.outlook().demand.week.is_ok(),
        "not in use, it projects"
    );

    // Skew: the newest reading is stamped two minutes ahead of this clock.
    let mut skew = scenario_b();
    skew.state.accounts.remove("me");
    skew.read("me", None, Some((580, THU9)), NOW + 2 * MIN);
    assert_eq!(skew.outlook().demand.week.err(), Some(Refusal::Skew));
    skew.state.accounts.remove("me");
    skew.read("me", None, Some((580, THU9)), NOW + 30);
    assert!(
        skew.outlook().demand.week.is_ok(),
        "inside the skew it projects"
    );

    // Too early: the week began an hour ago (so it is known empty from then), read for 40
    // minutes; neither an hour of rise nor an hour-old week is enough.
    let early = |began: i64| {
        let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
        let reset = NOW + 7 * D - began;
        fx.fig("me", NOW - 40 * MIN, None, Some((10, reset)));
        fx.read("me", None, Some((30, reset)), NOW);
        fx.outlook()
    };
    let out = early(H);
    assert_eq!(
        out.demand.week,
        Err(Refusal::TooEarly {
            have_s: H,
            need_s: WEEK_LEAST_S
        })
    );
    let words = out
        .demand
        .week
        .err()
        .map(Refusal::words)
        .unwrap_or_default();
    assert!(words.starts_with("no pace yet"), "{words}");
    assert!(out.week.is_none());
    let three = early(3 * H)
        .demand
        .week
        .expect("three hours since an empty week project");
    assert_eq!((three.basis, three.span_s), (Basis::Day, 3 * H));

    // No figure: the carrier's week was never read, or was read before a reset that has passed
    // since (that figure says nothing of this week).
    let mut none = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    none.read("me", Some((100, NOW + H)), None, NOW);
    assert_eq!(none.outlook().demand.week.err(), Some(Refusal::NoFigure));
    let mut passed = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    passed.read("me", None, Some((580, NOW - H)), NOW - 2 * H);
    assert_eq!(
        passed.outlook().demand.week.err(),
        Some(Refusal::NoFigure),
        "a figure from before its reset"
    );
    none.read("me", Some((100, NOW + H)), Some((580, THU9)), NOW);
    none.fig("me", NOW - 24 * H, None, Some((420, THU9)));
    assert!(none.outlook().demand.week.is_ok());
}

fn class_of(fx: &Fx, label: &str, w: Window) -> Fresh {
    fx.outlook()
        .rows
        .iter()
        .find(|r| r.label == label)
        .and_then(|r| r.window(w))
        .map(|w| w.fresh)
        .expect("the row")
}

#[test]
fn quota_clock_admission_survives_the_codec_without_current_claims() {
    for ahead in [60, 61, 120] {
        let mut fx = Fx::new(&[("me", Class::Preferred)]);
        fx.read("me", Some((380, NOW + H)), Some((580, THU9)), NOW + ahead);
        fx.state = State::from_json(&fx.state.to_json(), NOW).unwrap();
        let before = fx.state.clone();
        assert_eq!(
            fx.state.accounts["me"].windows[accounts::WEEK_LIMIT].at,
            NOW + ahead
        );
        let out = fx.outlook();
        let week = out.rows[0].window(Window::Week).unwrap();
        assert_eq!(week.at, Some(NOW + ahead));
        assert_eq!(week.reset, Some(THU9));
        if ahead == 60 {
            assert_eq!(week.fresh, Fresh::Now);
            assert_eq!(week.used_x10, Some(580));
        } else {
            assert_eq!(week.fresh, Fresh::ClockInvalid);
            assert_eq!(week.used_x10, None);
            assert_eq!(week.projection, None);
            assert_eq!(out.demand.week, Err(Refusal::Skew));
            for supplied in [None, Some(580)] {
                let glance = out.glance("me", supplied, true).unwrap();
                assert!(glance.bar.is_none() && glance.reset.is_none() && glance.five.is_none());
            }
            for width in [44, 60] {
                let cards = out.card_rows(Some("me"), width, None);
                assert!(
                    cards
                        .iter()
                        .any(|row| matches!(row, CardRow::Account { label, .. } if label == "me"))
                );
            }
        }
        assert_eq!(fx.state, before);
    }
    let invalid = Seen {
        pct_x10: 500,
        reset: NOW - 1,
        at: NOW + 120,
        source: Source {
            kind: "tap",
            here: true,
        },
    };
    let five = win_of(Some(invalid), Fresh::ClockInvalid, 1, NOW, Window::Five);
    assert_eq!(five.known, Known::Unknown);
    assert_eq!(five.reset, None);
    assert_eq!(five.acc, 0);
}

#[test]
fn access_classes_cannot_admit_a_rejected_quota_clock() {
    for not_enrolled in [false, true] {
        let mut fx = Fx::new(&[("me", Class::Preferred)]);
        fx.read("me", Some((380, NOW - 1)), Some((580, THU9)), NOW + 120);
        if not_enrolled {
            fx.roster.rows[0].dir = Some("/logins/me".into());
            fx.enrolled.clear();
        } else {
            accounts::note_auth(&mut fx.state, "me", accounts::Auth::SignedOut, NOW);
        }
        let out = fx.outlook();
        let row = &out.rows[0];
        let week = row.window(Window::Week).unwrap();
        assert_eq!(
            week.fresh,
            if not_enrolled {
                Fresh::NotEnrolled
            } else {
                Fresh::SignedOut
            }
        );
        assert_eq!((week.at, week.reset), (Some(NOW + 120), Some(THU9)));
        assert_eq!(week.used_x10, None);
        assert!(out.reset_words(week).is_empty());
        for supplied in [None, Some(580)] {
            let glance = out.glance("me", supplied, false).unwrap();
            assert!(glance.bar.is_none() && glance.reset.is_none() && glance.five.is_none());
            assert!(glance.title.is_none() && glance.spoken.is_none());
        }
        let five = row.window(Window::Five).unwrap();
        let mut without_source = five.clone();
        without_source.at = None;
        without_source.reset = None;
        assert_eq!(
            out.window_line(row, five),
            out.window_line(row, &without_source)
        );
    }
}

#[test]
fn quota_source_fallback_obeys_clock_and_refill_before_newest() {
    for post_epoch in [false, true] {
        let mut fx = Fx::new(&[("me", Class::Preferred)]);
        let at = if post_epoch {
            NOW - 5 * MIN
        } else {
            NOW - 20 * MIN
        };
        fx.read("me", None, Some((950, THU9)), NOW + 120);
        let acct = fx.state.accounts.get_mut("me").unwrap();
        acct.windows.get_mut(accounts::WEEK_LIMIT).unwrap().epoch_at = Some(NOW - 10 * MIN);
        acct.sample = Some(accounts::Sample {
            week_pct: Some(38),
            week_reset: Some(THU9),
            week_read_at: Some(at),
            at: NOW + 120,
            ..accounts::Sample::default()
        });
        let out = fx.outlook();
        let week = out.rows[0].window(Window::Week).unwrap();
        assert_eq!(week.used_x10, post_epoch.then_some(380));
        assert_eq!(
            week.fresh,
            if post_epoch {
                Fresh::Now
            } else {
                Fresh::ClockInvalid
            }
        );
        let clone = quota_replay_state(&fx.state, NOW);
        assert_eq!(
            clone.accounts["me"].sample.as_ref().unwrap().week_pct,
            post_epoch.then_some(38)
        );
        assert_eq!(
            fx.state.accounts["me"]
                .sample
                .as_ref()
                .unwrap()
                .week_read_at,
            Some(at)
        );
    }
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.read("me", None, Some((385, THU9)), NOW - 20 * MIN);
    fx.state.accounts.get_mut("me").unwrap().sample = Some(accounts::Sample {
        week_pct: Some(95),
        week_reset: Some(THU9),
        week_read_at: Some(NOW + 120),
        at: NOW,
        ..accounts::Sample::default()
    });
    let out = fx.outlook();
    let week = out.rows[0].window(Window::Week).unwrap();
    assert_eq!(
        (week.fresh, week.at, week.used_x10),
        (Fresh::AsOf, Some(NOW - 20 * MIN), Some(385))
    );
    assert_eq!(
        outlook(&Inputs {
            now: 0,
            ..fx.inputs()
        })
        .rows[0]
            .window(Window::Week)
            .unwrap()
            .fresh,
        Fresh::ClockInvalid
    );
}

#[test]
fn a_clock_invalid_alternate_is_an_unknown_lean_not_measured_capacity() {
    let mut passes = Vec::new();
    for used in [0, 950] {
        let mut fx = scenario_b();
        fx.state.accounts.remove("alt1");
        fx.series.remove("alt1");
        fx.read(
            "alt1",
            Some((used, NOW + H)),
            Some((used, FRI14)),
            NOW + 120,
        );
        let before = fx.state.clone();
        let out = fx.outlook();
        let alt = out.rows.iter().find(|row| row.label == "alt1").unwrap();
        assert_eq!(alt.window(Window::Week).unwrap().fresh, Fresh::ClockInvalid);
        assert!(
            alt.windows
                .iter()
                .all(|window| window.used_x10.is_none() && window.projection.is_none())
        );
        let pass = out.week.as_ref().expect("valid carrier pace");
        assert_eq!(
            pass.lean.as_ref().map(|lean| lean.why),
            Some(Fresh::ClockInvalid)
        );
        passes.push(pass.clone());
        assert_eq!(fx.state, before);
    }
    assert_eq!(
        passes[0], passes[1],
        "rejected numeric values cannot change the replay"
    );
}

#[test]
fn replay_clock_sanitation_preserves_walls_access_and_refill_cuts() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.read("me", Some((380, NOW + H)), Some((580, THU9)), NOW + 120);
    accounts::note_wall(
        &mut fx.state,
        "me",
        accounts::WEEK_LIMIT,
        Some(THU9),
        NOW,
        NOW,
    );
    accounts::note_auth(&mut fx.state, "me", accounts::Auth::Ok, NOW);
    fx.state.pinned = true;
    fx.session("s-me", "me", NOW);
    fx.state
        .accounts
        .get_mut("me")
        .unwrap()
        .windows
        .get_mut(accounts::WEEK_LIMIT)
        .unwrap()
        .epoch_at = Some(NOW + 121);
    let before = fx.state.clone();
    let clone = quota_replay_state(&fx.state, NOW);
    assert_eq!(clone.active, before.active);
    assert_eq!(clone.pinned, before.pinned);
    assert_eq!(clone.proven, before.proven);
    assert_eq!(clone.sessions, before.sessions);
    assert_eq!(clone.claimed_at, before.claimed_at);
    assert_eq!(clone.accounts["me"].walls, before.accounts["me"].walls);
    assert_eq!(clone.accounts["me"].auth, accounts::Auth::Ok);
    assert_eq!(clone.accounts["me"].auth_at, before.accounts["me"].auth_at);
    assert_eq!(
        clone.accounts["me"].windows[accounts::WEEK_LIMIT],
        before.accounts["me"].windows[accounts::WEEK_LIMIT]
    );
    let out = fx.outlook();
    assert_eq!(
        out.rows[0].window(Window::Week).unwrap().fresh,
        Fresh::Walled
    );
    let glance = out.glance("me", None, true).unwrap();
    // A wall is `out`: no bar of an inferred 100%.
    assert!(glance.bar.is_none());
    assert_eq!(glance, out.glance("me", Some(0), true).unwrap());
    assert_eq!(fx.state, before);
}

#[test]
fn a_future_series_point_cannot_invent_past_demand() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.fig("me", NOW - 2 * D, None, Some((100, THU9)));
    let before = rise_of(&fx, Window::Week, D);
    fx.fig("me", NOW + 120, None, Some((95, THU9)));
    assert_eq!(rise_of(&fx, Window::Week, D), before);
}

/// T7: EACH CLASS OF §6 FROM A FIXTURE, AND ONE BIT MOVES IT. A reset that passed unread is
/// `reset-unread` whatever this Mac's order for the row, since any Mac may run an account (§0 of
/// the lab model: a preference is no claim), so no row is counted fresh since its reset.
/// Negative controls: a session after the reading turns `idle-here` into `used-since`; a
/// `FreshSinceReset` arm restored for a preferred row turns `alt2` fresh (`no row is fresh`).
#[test]
fn freshness_one_bit() {
    let a = scenario_a();
    assert_eq!(class_of(&a, "me", Window::Week), Fresh::Now);
    assert_eq!(class_of(&a, "alt1", Window::Week), Fresh::Now);
    assert_eq!(
        class_of(&scenario_a_idle(), "alt1", Window::Week),
        Fresh::IdleHere
    );
    assert_eq!(
        class_of(&a, "alt2", Window::Week),
        Fresh::ResetUnread,
        "no row is fresh since a reset nobody read"
    );
    assert_eq!(class_of(&a, "pool1", Window::Week), Fresh::Pool);
    assert_eq!(
        class_of(&scenario_c(), "pool1", Window::Week),
        Fresh::NeverRead
    );

    let mut pool = scenario_a();
    pool.roster.rows[2].class = Class::Pool;
    assert_eq!(class_of(&pool, "alt2", Window::Week), Fresh::ResetUnread);

    let mut ran = scenario_a();
    ran.ran("alt2", THU22 + H);
    assert_eq!(class_of(&ran, "alt2", Window::Week), Fresh::ResetUnread);

    let mut since = scenario_a();
    since.session("s-2", "alt2", THU22);
    assert_eq!(class_of(&since, "alt2", Window::Week), Fresh::ResetUnread);

    let mut used = scenario_a_idle();
    used.session("s-alt1", "alt1", NOW - 2 * H);
    assert_eq!(class_of(&used, "alt1", Window::Week), Fresh::UsedSince);

    // The carrier's reading 40 minutes old is `as-of`; past an hour while in use, `stale`.
    let mut aged = scenario_a();
    aged.state.accounts.remove("me");
    aged.read("me", None, Some((580, THU9)), NOW - 40 * MIN);
    assert_eq!(class_of(&aged, "me", Window::Week), Fresh::AsOf);
    aged.state.accounts.remove("me");
    aged.read("me", None, Some((580, THU9)), NOW - 90 * MIN);
    assert_eq!(class_of(&aged, "me", Window::Week), Fresh::Stale);

    // A wall, a signed-out login, a login never made here.
    let mut walled = scenario_a();
    accounts::note_wall(
        &mut walled.state,
        "alt1",
        accounts::WEEK_LIMIT,
        Some(FRI14),
        NOW - H,
        NOW - H,
    );
    assert_eq!(class_of(&walled, "alt1", Window::Week), Fresh::Walled);
    let mut out = scenario_a();
    out.enrolled.remove("pool1");
    assert_eq!(class_of(&out, "pool1", Window::Week), Fresh::NotEnrolled);
    // No class that is not read shows a number.
    for row in &scenario_c().outlook().rows {
        for w in &row.windows {
            if !w.fresh.shows_figure() {
                assert_eq!(w.used_x10, None, "{} {:?}", row.label, w.fresh);
            }
        }
    }
}

/// One random draw (xorshift), for the property test.
struct Draw(u64);

impl Draw {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn random_fixture(draw: &mut Draw) -> Fx {
    let n = 1 + draw.below(6) as usize;
    let names = ["me", "alt1", "alt2", "alt3", "pool1", "pool2"];
    let rows: Vec<(&str, Class)> = (0..n)
        .map(|i| {
            (
                names[i],
                if draw.below(3) == 0 {
                    Class::Pool
                } else {
                    Class::Preferred
                },
            )
        })
        .collect();
    let mut fx = Fx::new(&rows);
    fx.mode = if draw.below(5) == 0 {
        Mode::Wall
    } else {
        Mode::Early
    };
    let tiers = [("max", Some("20x")), ("max", Some("5x")), ("pro", None)];
    for (i, (label, _)) in rows.iter().enumerate() {
        let (word, tier) = tiers[draw.below(3) as usize];
        if draw.below(4) != 0 {
            fx.plan(label, word, tier, NOW - D);
        }
        match draw.below(5) {
            0 if i > 0 => {}
            1 => {
                let reset = NOW - (draw.below(6 * 24) as i64) * H;
                fx.read(
                    label,
                    None,
                    Some((draw.below(1001) as u32, reset)),
                    reset - D,
                );
            }
            _ => {
                let at = NOW - (draw.below(6 * 60) as i64) * MIN;
                let reset = NOW + 1 + (draw.below(7 * 24) as i64) * H;
                let five = (draw.below(2) == 0).then(|| {
                    (
                        draw.below(1001) as u32,
                        NOW + (draw.below(300) as i64) * MIN,
                    )
                });
                fx.read(label, five, Some((draw.below(1001) as u32, reset)), at);
                fx.fig(label, at - 24 * H, None, Some((0, reset)));
            }
        }
        if draw.below(4) == 0 {
            fx.ran(label, NOW - (draw.below(48) as i64) * H);
        }
    }
    if draw.below(6) == 0 {
        fx.state.pinned = true;
    }
    for _ in 0..draw.below(4) {
        fx.state.rotations.push(NOW - (draw.below(60) as i64) * MIN);
    }
    fx
}

/// T9: THE REPLAY IS THE POLICY. Over random synthetic rosters (1–6 rows, mixed class, tier,
/// mode, figures, resets, pins and recent rotations), every decision the replay took equals a
/// direct `advise_past_probe` on the same clone; at each turn's end the clone holds the
/// carrier's modeled figures, so the policy decided on the replay's own figures and not on
/// the store's; and every move it was told to make it made. Negative controls: a replay that
/// moves to the lowest other label fails it; one that never writes its figures into the clone
/// fails the turn ends.
#[test]
fn replay_is_the_policy() {
    let mut draw = Draw(0x9e37_79b9_7f4a_7c15);
    let mut decided = 0usize;
    let mut moved_on = 0usize;
    for _ in 0..300 {
        let fx = random_fixture(&mut draw);
        let (out, trace) = outlook_traced(&fx.inputs());
        for d in &trace {
            let direct = accounts::advise_past_probe(
                &fx.roster,
                &d.state,
                &fx.enrolled,
                &me(&d.carrier),
                &d.point,
                fx.mode,
                d.t,
            );
            assert_eq!(direct, d.advice, "at {} on {}", d.t, d.carrier);
            if d.point != Point::TurnEnd {
                continue;
            }
            let held = d.state.accounts.get(&d.carrier);
            for &(w, pct_x10, reset) in &d.modeled {
                let fig = held.and_then(|a| a.windows.get(w.id()));
                assert_eq!(
                    fig.map(|f| (f.pct_x10, f.reset)),
                    Some((pct_x10, reset)),
                    "{:?} of {} at {}: the clone holds the replay's figure",
                    w,
                    d.carrier,
                    d.t
                );
                let stored = fx
                    .state
                    .accounts
                    .get(&d.carrier)
                    .and_then(|a| a.windows.get(w.id()))
                    .map(|f| f.pct_x10);
                moved_on += usize::from(stored != Some(pct_x10));
            }
        }
        decided += trace.len();
        // Every move the policy answered is taken, by a link, a lean or a refused rotation.
        let mut taken: BTreeSet<(i64, String)> = BTreeSet::new();
        let mut passes: Vec<&Pass> = out.week.iter().chain(out.working.iter()).collect();
        while let Some(pass) = passes.pop() {
            for link in &pass.chain {
                taken.insert((link.from, link.label.clone()));
            }
            if let Some(lean) = &pass.lean {
                taken.insert((lean.at, lean.row.clone()));
                passes.push(&lean.worst);
                passes.push(&lean.best);
            }
        }
        for d in &trace {
            if let Advice::Move { to, rotates, .. } = &d.advice {
                let mut after = d.state.clone();
                let refused = *rotates
                    && accounts::begin_rotation(
                        &mut after,
                        &fx.roster,
                        &fx.enrolled,
                        d.state.active.as_deref(),
                        accounts::Mover {
                            sid: "outlook",
                            account: &d.carrier,
                        },
                        to,
                        d.t,
                    )
                    .is_err();
                // Past its pass's horizon a move only ends a stop: no link follows it.
                let horizon = d.t >= d.horizon;
                assert!(
                    refused || horizon || taken.contains(&(d.t, to.clone())),
                    "the move to {to} at {} was not taken",
                    d.t
                );
            }
        }
    }
    assert!(decided > 300, "the draws reached decisions: {decided}");
    assert!(
        moved_on > 100,
        "CONTROL: turn ends whose figure moved past the store's: {moved_on}"
    );
}

/// T10: THE REPLAY SEES ONLY WHAT WAS OBSERVED. In A, `alt2`'s reset passed and nothing read it
/// since, so the policy ranks it with no week reset, as the real one does, and `alt1` is picked:
/// another Mac may have run it since its reset.
/// Negative control: writing a fresh week into the clone for the unread reset picks `alt2`.
#[test]
fn replay_sees_only_what_was_observed() {
    let out = scenario_a().outlook();
    let week = out.week.as_ref().expect("the week pass");
    assert_eq!(
        week.chain
            .iter()
            .map(|l| l.label.as_str())
            .collect::<Vec<_>>(),
        ["me", "alt1"]
    );
}

/// T11: THE SCENARIOS TO THE MINUTE, from the policy's own functions (the store's half-up whole
/// percent leaves at 97.5%, so `judge/sim.py`'s 98% instants come 45 minutes later: §15 of the
/// design records it).
#[test]
fn scenarios_to_the_minute() {
    // `me` leaves at 97.5%: 39.5 points at 16 a day.
    let leaves = NOW + 213_300;
    assert_eq!(when(leaves, NOW, &utc()), "Wed 06:45");

    let a = scenario_a().outlook();
    let week = a.week.as_ref().unwrap();
    assert_eq!(
        labels(week),
        [
            ("me", NOW, leaves, Ends::Leaves),
            ("alt1", leaves, NOW + 7 * D, Ends::Horizon)
        ]
    );
    assert!(week.stops.is_empty());
    assert_eq!(a.verdict, Verdict::Continuous);
    let alt1 = a.rows.iter().find(|r| r.label == "alt1").unwrap();
    let proj = alt1
        .window(Window::Week)
        .and_then(|w| w.projection)
        .unwrap();
    assert_eq!(
        whole(proj.at_reset_x10),
        68,
        "alt1 reaches 67.8% by Fri 14:00"
    );

    // B: alt1 walls at 18:45, low `me` spends its last 2.5% to 22:30, then every account is out
    // to `me`'s reset Thu 09:00 (its grace to 09:02:30).
    let b = scenario_b().outlook();
    let week = b.week.as_ref().unwrap();
    let alt1_full = leaves + 8 * H + 15 * MIN + 3 * H + 45 * MIN;
    let me_full = alt1_full + 3 * H + 45 * MIN;
    assert_eq!(when(alt1_full, NOW, &utc()), "Wed 18:45");
    assert_eq!(
        labels(week)[..3],
        [
            ("me", NOW, leaves, Ends::Leaves),
            ("alt1", leaves, alt1_full, Ends::Walls),
            ("me", alt1_full, me_full, Ends::Walls),
        ]
    );
    assert_eq!(stops(week), [(me_full, StopEnd::Reset(THU9))]);
    assert_eq!(
        b.verdict,
        Verdict::Stop {
            pass: PassKind::Week,
            from: me_full,
            end: StopEnd::Reset(THU9),
            lean: None
        }
    );
    let words = b.verdict_words().unwrap();
    // 75 hours ahead, the start rounds down to the hour; the reset is exact.
    assert_eq!(words.full, "\u{2573} ~Wed 22:00\u{2013}Thu 09:00");
    assert!(words.warn && words.kept_narrow);

    // C: alt1 leaves early onto `pool1` (shared, never read) at Wed 15:00: the worst case is B's
    // stop, the best none.
    let c = scenario_c().outlook();
    let week = c.week.as_ref().unwrap();
    let lean = week.lean.as_ref().expect("a lean");
    assert_eq!(
        (lean.row.as_str(), lean.at),
        ("pool1", leaves + 8 * H + 15 * MIN)
    );
    assert_eq!(lean.why, Fresh::NeverRead);
    assert_eq!(stops(&lean.worst), [(me_full, StopEnd::Reset(THU9))]);
    assert!(lean.best.stops.is_empty() && lean.best.lean.is_none());
    assert_eq!(
        c.verdict,
        Verdict::Lean {
            until: lean.at,
            row: "pool1".into(),
            why: Fresh::NeverRead,
            stop: Some((me_full, StopEnd::Reset(THU9)))
        }
    );

    // D: the working pass: `me`'s 5h low at 19:45:45 with no fresh row, its wall at 20:05:12,
    // low `alt1` admitted there and walled at 20:14:28, then a stop to alt1's reset 21:10.
    let d = scenario_d().outlook();
    let working = d.working.as_ref().expect("the working pass");
    let me_wall = NOW + 35 * MIN + 12;
    let alt1_wall = me_wall + 9 * MIN + 16;
    assert_eq!(
        labels(working)[..2],
        [
            ("me", NOW, me_wall, Ends::Walls),
            ("alt1", me_wall, alt1_wall, Ends::Walls)
        ]
    );
    let alt1_reset = NOW + 100 * MIN;
    assert_eq!(stops(working)[0], (alt1_wall, StopEnd::Reset(alt1_reset)));
    assert_eq!(
        d.verdict_words().unwrap().full,
        "\u{2573} ~20:10\u{2013}21:10"
    );
}

/// A PREFERENCE IS NO CLAIM: `alt1` read three hours ago and idle here since is `idle-here`,
/// and the replay counts its reading as a bound, as it does a pool row's, since another Mac may
/// prefer and run it too (§0 of the lab model). So the idle A leans on alt1 where the same
/// figure read now is continuous. Negative control (run): counting `idle-here` as exact
/// (`Known::Exact` in `Fresh::known`) makes the idle A continuous.
#[test]
fn an_idle_preferred_row_is_a_bound() {
    let idle = scenario_a_idle().outlook();
    assert!(
        matches!(&idle.verdict, Verdict::Lean { row, why: Fresh::IdleHere, .. } if row == "alt1"),
        "the idle A leans on alt1: {:?}",
        idle.verdict
    );
    assert_eq!(scenario_a().outlook().verdict, Verdict::Continuous);
}

/// T12: A LEAN BOUNDS BOTH WAYS: an unread row gives a lean with a worst-case stop and a best
/// case. Negative control: taking the unread row as fresh shows `continuous`.
#[test]
fn a_lean_bounds_both_ways() {
    let out = scenario_c().outlook();
    let Verdict::Lean { row, stop, .. } = &out.verdict else {
        panic!("a lean: {:?}", out.verdict);
    };
    assert_eq!(row, "pool1");
    assert!(stop.is_some(), "its worst case stops");
    let words = out.verdict_words().unwrap();
    assert!(!words.warn, "a lean is not a stop");
    assert!(words.full.ends_with("then pool1 ?"), "{}", words.full);
    // Both branches stop: a stop with `?`.
    let mut both = scenario_c();
    both.read("pool1", None, Some((990, SAT8)), NOW - 5 * H);
    let out = both.outlook();
    assert!(
        matches!(&out.verdict, Verdict::Stop { lean: Some(row), .. } if row == "pool1"),
        "{:?}",
        out.verdict
    );
    assert!(out.verdict_words().unwrap().full.ends_with(" ?"));
}

#[test]
fn unavailable_carriers_never_resume_from_a_quota_reset() {
    for not_enrolled in [false, true] {
        for proven in [false, true] {
            let mut fx = Fx::new(&[("me", Class::Preferred)]);
            me_at(&mut fx, 1000);
            fx.credits = true;
            fx.state.proven = proven;
            if not_enrolled {
                fx.roster.rows[0].dir = Some("/logins/me".into());
                fx.enrolled.clear();
            } else {
                accounts::note_auth(&mut fx.state, "me", accounts::Auth::SignedOut, NOW);
            }
            let before = fx.state.clone();
            let (out, trace) = outlook_traced(&fx.inputs());
            assert_eq!(out.verdict, Verdict::Refused(Refusal::AccountUnavailable));
            assert_eq!(out.carrier, None);
            assert!(out.week.is_none() && out.working.is_none() && trace.is_empty());
            assert_eq!(out.rows.len(), 1);
            let window = out.rows[0].window(Window::Week).unwrap();
            assert_eq!((window.reset, window.at), (Some(THU9), Some(NOW)));
            assert_eq!(window.used_x10, None);
            assert_eq!(fx.state, before);
            fx.now = THU9 + 1;
            let after_reset = fx.outlook();
            assert_eq!(after_reset.verdict, out.verdict);
            assert!(after_reset.week.is_none() && after_reset.working.is_none());

            // An actual access change, unlike a quota reset, admits the same history.
            fx.enrolled.insert("me".into());
            accounts::note_auth(&mut fx.state, "me", accounts::Auth::Ok, NOW + 1);
            fx.now = NOW + 1;
            for mode in [Mode::Off, Mode::Wall, Mode::Early] {
                fx.mode = mode;
                for pinned in [false, true] {
                    fx.state.pinned = pinned;
                    let admitted = fx.outlook();
                    assert_eq!(admitted.carrier.as_deref(), Some("me"));
                    assert!(admitted.week.as_ref().unwrap().chain.iter().any(|link| {
                        link.label == "me" && link.from >= THU9 && link.to > link.from
                    }));
                }
            }
        }
    }
}

#[test]
fn available_alternatives_do_not_prove_a_signed_out_active_account() {
    for alternate_pct in [310, 1000] {
        let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
        me_at(&mut fx, 1000);
        alt1_read(&mut fx, alternate_pct);
        if alternate_pct == 1000 {
            accounts::note_wall(
                &mut fx.state,
                "alt1",
                accounts::WEEK_LIMIT,
                Some(FRI14),
                NOW,
                NOW,
            );
        }
        accounts::note_auth(&mut fx.state, "me", accounts::Auth::SignedOut, NOW);
        fx.state.proven = false;
        let before = fx.state.clone();
        let (out, trace) = outlook_traced(&fx.inputs());
        assert_eq!(out.carrier.as_deref(), Some("alt1"));
        assert!(!trace.is_empty());
        assert!(
            trace
                .iter()
                .all(|decision| { decision.state.account("me").auth == accounts::Auth::SignedOut })
        );
        let mut passes: Vec<_> = out.week.iter().chain(out.working.iter()).collect();
        while let Some(pass) = passes.pop() {
            assert!(pass.chain.iter().all(|link| link.label != "me"));
            if let Some(lean) = &pass.lean {
                passes.push(&lean.worst);
                passes.push(&lean.best);
            }
        }
        assert!(out.rows.iter().all(|row| {
            row.walled
                .as_ref()
                .is_none_or(|wall| wall.to.as_deref() != Some("me"))
        }));
        assert_eq!(fx.state, before);
    }
}

#[test]
fn retained_access_refusals_cannot_become_forecast_carriers() {
    for blocker in 0..4 {
        let mut fx = Fx::new(&[("me", Class::Preferred)]);
        me_at(&mut fx, 1000);
        match blocker {
            0 => fx.roster.rows[0].held_by = Some("another-machine".into()),
            1 => {
                fx.state.prepared_accounts.insert("me".into());
            }
            2 => fx.state.note_lease("me", "another-machine", NOW + D, NOW),
            3 => fx.state.unrecorded = true,
            _ => unreachable!(),
        }
        let out = fx.outlook();
        assert_eq!(out.verdict, Verdict::Refused(Refusal::AccountUnavailable));
        assert!(out.carrier.is_none() && out.week.is_none() && out.working.is_none());
        fx.roster.rows[0].held_by = None;
        fx.state.prepared_accounts.clear();
        fx.state.clear_lease("me");
        fx.state.unrecorded = false;
        assert!(fx.outlook().week.is_some());
    }
}

#[test]
fn enrollment_without_a_reading_remains_an_unknown_budget() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    let home = fx.outlook();
    assert_eq!(home.carrier.as_deref(), Some("me"));
    assert_eq!(home.verdict, Verdict::Refused(Refusal::NoFigure));
    let mut missing = Fx::new(&[("me", Class::Preferred)]);
    me_at_today(&mut missing);
    missing.enrolled.clear();
    let before = missing.state.clone();
    let absent = missing.outlook();
    assert_eq!(
        absent.verdict,
        Verdict::Refused(Refusal::AccountUnavailable)
    );
    assert!(absent.carrier.is_none() && absent.week.is_none() && absent.working.is_none());
    assert_eq!(absent.rows[0].condition, accounts::Condition::NotEnrolled);
    assert_eq!(missing.state, before);
    fx.roster.rows[0].dir = Some("/logins/me".into());
    let enrolled = fx.outlook();
    assert_eq!(enrolled.carrier, home.carrier);
    assert_eq!(enrolled.verdict, home.verdict);
}

#[test]
fn rotation_off_does_not_replace_an_unavailable_current_account() {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    me_at(&mut fx, 1000);
    alt1_read(&mut fx, 310);
    fx.mode = Mode::Off;
    let limited = fx.outlook();
    assert_eq!(limited.carrier.as_deref(), Some("me"));
    assert!(
        limited
            .week
            .as_ref()
            .unwrap()
            .chain
            .iter()
            .all(|link| link.label == "me")
    );
    accounts::note_auth(&mut fx.state, "me", accounts::Auth::SignedOut, NOW);
    let unavailable = fx.outlook();
    assert_eq!(unavailable.verdict, Verdict::Refused(Refusal::RotationOff));
    assert!(unavailable.carrier.is_none() && unavailable.week.is_none());
    fx.state.active = Some("removed".into());
    let removed = fx.outlook();
    assert_eq!(removed.verdict, Verdict::Refused(Refusal::RotationOff));
    assert!(removed.carrier.is_none() && removed.week.is_none());
    fx.state.active = None;
    assert_eq!(fx.outlook().carrier.as_deref(), Some("alt1"));
    fx.state.active = Some("me".into());
    fx.mode = Mode::Early;
    assert_eq!(fx.outlook().carrier.as_deref(), Some("alt1"));
}

#[test]
fn a_custody_refused_pin_holds_only_while_the_active_account_runs() {
    for pct in [580, 1000] {
        for assigned_elsewhere in [false, true] {
            let case = format!("pct={pct}, assigned_elsewhere={assigned_elsewhere}");
            let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
            me_at(&mut fx, pct);
            alt1_read(&mut fx, 310);
            if pct == 1000 {
                accounts::note_wall(
                    &mut fx.state,
                    "me",
                    accounts::WEEK_LIMIT,
                    Some(THU9),
                    NOW,
                    NOW,
                );
            }
            assert_eq!(
                matches!(
                    accounts::condition(&fx.roster.rows[0], &fx.state, true, NOW),
                    accounts::Condition::Limited { .. }
                ),
                pct == 1000,
                "{case}"
            );
            fx.state.pinned = true;
            if assigned_elsewhere {
                fx.roster.rows[0].held_by = Some("another-machine".into());
            } else {
                fx.state.note_lease("me", "another-machine", NOW + D, NOW);
            }
            let held = fx.outlook();
            if pct == 580 && !assigned_elsewhere {
                assert_eq!(
                    held.verdict,
                    Verdict::Refused(Refusal::AccountUnavailable),
                    "{case}"
                );
                assert!(held.carrier.is_none() && held.week.is_none(), "{case}");
            } else {
                assert_eq!(held.carrier.as_deref(), Some("alt1"), "{case}");
                let mut passes = vec![held.week.as_ref().unwrap()];
                let mut chain = Vec::new();
                while let Some(pass) = passes.pop() {
                    chain.extend(&pass.chain);
                    if let Some(lean) = &pass.lean {
                        passes.push(&lean.worst);
                        passes.push(&lean.best);
                    }
                }
                if assigned_elsewhere {
                    assert!(
                        chain.iter().all(|link| link.label != "me"),
                        "{case}: {chain:?}"
                    );
                } else {
                    let not_before = fx.state.leases["me"]
                        .until
                        .max(fx.state.accounts["me"].limited_until(NOW).unwrap());
                    let resumed: Vec<_> = chain.iter().filter(|link| link.label == "me").collect();
                    assert!(!resumed.is_empty(), "{case}: {chain:?}");
                    assert!(
                        resumed.iter().all(|link| link.from >= not_before),
                        "{case}: {chain:?}"
                    );
                }
            }
            fx.state.pinned = false;
            assert_eq!(
                fx.outlook().carrier.as_deref(),
                Some("alt1"),
                "{case}: unpinned"
            );
        }
    }
}

#[test]
fn unavailable_pool_cards_keep_the_reason_and_observed_rows() {
    let mut fx = scenario_b();
    for label in ["me", "alt1"] {
        accounts::note_auth(&mut fx.state, label, accounts::Auth::SignedOut, NOW);
    }
    for (mode, why) in [
        (Mode::Early, Refusal::AccountUnavailable),
        (Mode::Off, Refusal::RotationOff),
    ] {
        fx.mode = mode;
        let out = fx.outlook();
        let json = out.json();
        assert_eq!(json["verdict"]["why"].as_str(), Some(why.word()));
        assert!(json["passes"].as_array().unwrap().is_empty());
        assert!(out.strip(56, 4).is_none());
        assert!(
            out.rows
                .iter()
                .flat_map(|row| &row.windows)
                .all(|window| window.projection.is_none())
        );
        for room in [60, 44, 30] {
            let rows = out.card_rows(Some("me"), room, None);
            assert!(rows.iter().all(|row| row.width() <= room));
            assert!(matches!(rows[0], CardRow::Note { .. }));
            assert!(row_text(&rows[0]).contains(&why.words()));
            let labels: Vec<_> = rows
                .iter()
                .filter_map(|row| match row {
                    CardRow::Account { label, .. } => Some(label.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(labels, ["me", "alt1"]);
        }
    }
}

#[test]
fn a_paused_session_keeps_pool_observations_without_service_advice() {
    let raw = scenario_b().outlook();
    let before = raw.clone();
    assert!(raw.week.is_some());
    let view = raw.observations_only(Refusal::SessionPaused);
    assert_eq!(view.verdict, Verdict::Refused(Refusal::SessionPaused));
    assert!(view.carrier.is_none() && view.week.is_none() && view.working.is_none());
    assert!(view.not_needed.is_empty());
    assert_eq!(
        (view.as_of, view.mode, view.credits, view.demand),
        (raw.as_of, raw.mode, raw.credits, raw.demand)
    );
    for (shown, observed) in view.rows.iter().zip(&raw.rows) {
        assert_eq!(
            (
                &shown.label,
                shown.active,
                shown.condition,
                &shown.plan,
                shown.ran_at
            ),
            (
                &observed.label,
                observed.active,
                observed.condition,
                &observed.plan,
                observed.ran_at
            )
        );
        assert!(!shown.carrier && shown.walled.is_none());
        assert_eq!(shown.windows.len(), observed.windows.len());
        for (shown, observed) in shown.windows.iter().zip(&observed.windows) {
            assert_eq!(
                (
                    shown.window,
                    shown.used_x10,
                    shown.reset,
                    shown.at,
                    shown.fresh,
                    shown.source
                ),
                (
                    observed.window,
                    observed.used_x10,
                    observed.reset,
                    observed.at,
                    observed.fresh,
                    observed.source
                )
            );
            assert!(shown.projection.is_none());
        }
    }
    for width in [60, 44, 30] {
        let labels: Vec<_> = view
            .card_rows(Some("me"), width, None)
            .into_iter()
            .filter_map(|row| {
                if let CardRow::Account { label, .. } = row {
                    Some(label)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            labels,
            raw.rows
                .iter()
                .map(|row| row.label.clone())
                .collect::<Vec<_>>()
        );
        assert!(view.strip(width, 4).is_none());
    }
    assert_eq!(raw, before);
    let mut busy = view.clone();
    busy.verdict = Verdict::Refused(Refusal::ObserverBusy);
    assert_eq!(raw.observations_only(Refusal::ObserverBusy), busy);
}

/// T13: THE BREAKER, `Mode::Off` AND CREDITS. The breaker gives a stop to its end; `off` gives the
/// carrier alone; credits turn a stop into `→ credits`. Negative controls: each switch flipped.
#[test]
fn breaker_pin_off_credits() {
    // The breaker: five rotations in the last half hour on two rows, and `me` walls in minutes.
    let breaker = |rotations: bool| {
        let mut fx = Fx::new(&[
            ("me", Class::Preferred),
            ("alt1", Class::Preferred),
            ("alt2", Class::Preferred),
            ("pool1", Class::Pool),
        ]);
        me_at(&mut fx, 999);
        alt1_read(&mut fx, 310);
        if rotations {
            fx.state.rotations = (0..9).map(|i| NOW - (30 - i) * MIN).collect();
        }
        fx.outlook()
    };
    let held = breaker(true);
    let wall = NOW + 9 * MIN;
    let week = held.week.as_ref().unwrap();
    let first = week.stops.first().expect("a breaker stop");
    assert_eq!(first.from, wall);
    // Nine rotations from 19:00 on four rows (a bound of eight): it holds to 19:01 + 1 h.
    assert_eq!(first.end, StopEnd::About(NOW + 31 * MIN));
    assert!(
        breaker(false).week.unwrap().stops.is_empty(),
        "no breaker, no stop"
    );

    // `off`: the carrier alone walls Wed 10:30 and waits to its reset.
    let mut off = scenario_b();
    off.mode = Mode::Off;
    let out = off.outlook();
    let alone = NOW + 63 * H;
    assert_eq!(
        out.verdict,
        Verdict::Stop {
            pass: PassKind::Week,
            from: alone,
            end: StopEnd::Reset(THU9),
            lean: None
        }
    );
    assert_eq!(
        out.week.as_ref().unwrap().chain.len(),
        2,
        "me, then me again after its reset"
    );

    // Credits on: the stop is carried.
    let mut credits = scenario_b();
    credits.credits = true;
    let out = credits.outlook();
    assert!(
        matches!(out.verdict, Verdict::Credits { .. }),
        "{:?}",
        out.verdict
    );
    assert!(!out.verdict.warns());
    assert!(matches!(
        scenario_b().outlook().verdict,
        Verdict::Stop { .. }
    ));
}

/// T14: THE REPLAY WRITES NOTHING AND NEVER PROBES. Over a store on disk whose fixture is live
/// (`advise` with its probe arm returns `Probe` on it), `state.json`, the ledger and the series
/// stay byte-identical across `outlook()`, and no decision is a probe. Two guards keep the
/// probe out: the replay asks `advise_past_probe`, and it reads each modeled figure into its
/// clone before it asks, so the clone's sample is never due. Negative control: a replay that
/// asks `advise` (with the probe) over a clone without its samples takes a `Probe`; either
/// guard alone holds.
#[test]
fn replay_writes_nothing_and_never_probes() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-outlook-t14-")
        .tempdir()
        .expect("temp dir");
    let mut fx = scenario_b();
    // `me`'s sample an hour old: its probe is due.
    fx.state.accounts.remove("me");
    fx.read("me", None, Some((580, THU9)), NOW - 40 * MIN);
    let live = accounts::advise(
        &fx.roster,
        &fx.state,
        &fx.enrolled,
        &me("me"),
        &accounts::Point::TurnEnd,
        Mode::Early,
        NOW,
    );
    assert_eq!(live, Advice::Probe, "the fixture is live");
    let store = accounts::Store::at(dir.path());
    store.save(&fx.state).expect("save");
    store
        .append(&[accounts::Event::new(NOW - H, accounts::Kind::Sample, "me")])
        .expect("ledger");
    for (label, rows) in &fx.series {
        for row in rows {
            if let Said::Fig {
                five_hour,
                seven_day,
                ..
            } = &row.said
            {
                account_series::note_within(
                    dir.path(),
                    label,
                    MACHINE,
                    (five_hour.clone(), seven_day.clone()),
                    (None, None),
                    row.t,
                    std::time::Duration::from_secs(60),
                )
                .expect("series");
            }
        }
    }
    let snapshot = || {
        let mut files = BTreeMap::new();
        for entry in walk(dir.path()) {
            files.insert(entry.clone(), std::fs::read(&entry).expect("read"));
        }
        files
    };
    let before = snapshot();
    assert!(
        before.len() >= 3,
        "state, ledger and series: {:?}",
        before.keys()
    );
    let loaded = store.load(NOW).expect("load").0;
    let series: BTreeMap<String, Vec<SeriesRow>> = fx
        .roster
        .rows
        .iter()
        .map(|r| {
            (
                r.label.clone(),
                account_series::read(dir.path(), &r.label, MACHINE),
            )
        })
        .collect();
    let inputs = Inputs {
        state: &loaded,
        series: &series,
        ..fx.inputs()
    };
    let (out, trace) = outlook_traced(&inputs);
    assert!(out.week.is_some());
    assert!(!trace.is_empty());
    assert!(
        trace.iter().all(|d| d.advice != Advice::Probe),
        "the replay never probes"
    );
    assert_eq!(
        loaded,
        store.load(NOW).expect("load").0,
        "the live state is untouched"
    );
    assert_eq!(before, snapshot(), "no file changed");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// T15: THE REPLAY IS BOUNDED. 32 Pro rows whose 5-hour windows each last two minutes at this
/// hour's pace (all 32 rose 99 points in the last hour) cycle through the working pass's 12 hours:
/// it stops at 256 events with `cut_at` set, and time never steps back. Negative control: without
/// the bound the pass runs on to its horizon. The wait is a one-minute hang detector.
#[test]
fn replay_is_bounded() {
    let names: Vec<String> = (0..32).map(|i| format!("a{i:02}")).collect();
    let rows: Vec<(&str, Class)> = names
        .iter()
        .map(|n| (n.as_str(), Class::Preferred))
        .collect();
    let mut fx = Fx::new(&rows);
    let week = NOW + 6 * D;
    for (i, name) in names.iter().enumerate() {
        fx.plan(name, "pro", None, NOW - D);
        fx.fig(name, NOW - D, None, Some((0, week)));
        fx.fig(name, NOW - 59 * MIN, Some((0, NOW - MIN)), Some((0, week)));
        if i == 0 {
            fx.read(name, Some((0, NOW + 5 * H)), Some((0, week)), NOW);
        } else {
            fx.read(name, Some((990, NOW - MIN)), Some((0, week)), NOW - 2 * MIN);
        }
    }
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(fx.outlook());
    });
    let out = recv
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the replay ends");
    for pass in out.week.iter().chain(out.working.iter()) {
        assert!(pass.events <= MAX_EVENTS);
        let mut t = NOW;
        for link in &pass.chain {
            assert!(
                link.from >= t && link.to >= link.from,
                "time never steps back"
            );
            t = link.to;
        }
    }
    let working = out.working.as_ref().expect("the working pass");
    assert_eq!(working.events, MAX_EVENTS);
    assert!(
        working
            .cut_at
            .is_some_and(|at| at < NOW + WORKING_HORIZON_S)
    );
    // The fills are bounded too: at most `MAX_EVENTS` knots a row, all rows together, none past
    // the horizon.
    for pass in out.week.iter().chain(out.working.iter()) {
        let knots: usize = pass.fills.values().map(Vec::len).sum();
        assert!(knots <= MAX_EVENTS * names.len(), "{knots} knots");
        assert!(
            pass.fills
                .values()
                .flatten()
                .all(|k| k.t >= NOW && k.t <= pass.horizon)
        );
    }
}

/// T16: PLAN MULTIPLES. A Max 5x after a Max 20x spends 4× faster; a row whose plan was not seen
/// is counted as the carrier's and listed in `approx`. Negative control: one scale for every
/// plan spends the 5x as slowly as the 20x.
#[test]
fn plan_multiples() {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("five", Class::Preferred)]);
    me_at(&mut fx, 970);
    fx.plan("five", "max", Some("5x"), NOW - D);
    fx.ran("five", NOW - 2 * D);
    fx.read("five", None, Some((0, NOW + 6 * D)), NOW - 10 * MIN);
    let out = fx.outlook();
    let week = out.week.as_ref().unwrap();
    let five = week
        .chain
        .iter()
        .find(|l| l.label == "five")
        .expect("five carries");
    // Its whole week (low `me` is no fresh row to leave for) at 16 points of a 20x week a day,
    // which is 64 of a 5x's: 1000 / 640 days.
    assert_eq!(five.to - five.from, 1000 * D / 640);
    // A row with no plan seen: counted as the carrier's.
    let mut unseen = scenario_a();
    unseen.series.remove("alt1");
    let out = unseen.outlook();
    let alt1 = out.rows.iter().find(|r| r.label == "alt1").unwrap();
    assert_eq!((alt1.plan.m, alt1.plan.source), (20, PlanSource::Assumed));
    assert!(out.approx.iter().any(|a| matches!(
        a,
        Approx::PlanAssumed { label, like: Some(like), .. } if label == "alt1" && like == "Max 20x"
    )));
    assert_eq!(
        plan_of_title("Max 20x").map(|p| p.tier),
        Some(Some("20x".into()))
    );
    assert_eq!(multiple_of("pro", None), Some(1));
    // Mixed multiples are said; one multiple for every row is not.
    let scaled = |out: &Outlook| out.approx.contains(&Approx::MultiplesScaled);
    let mixed = fx.outlook();
    assert!(scaled(&mixed), "{:?}", mixed.approx);
    assert!(mixed.text().contains(&format!(
        "  \u{2248} {}\n",
        mixed.approx_words(&Approx::MultiplesScaled, false)[0]
    )));
    assert!(!scaled(&scenario_b().outlook()), "B: both rows Max 20x");
}

/// T17: THE CLI'S TEXT AND JSON ARE ONE MODEL: the same stop instants and the same percents.
/// Every stop of every pass in scenarios B and D is said in the text at its JSON instant
/// rounded down by its lead; D's 5-hour stop starts off the ten-minute grid, so a rounding
/// shows; the verdict's JSON `end` is the vendor reset the text says. Negative control: a text
/// that rounds its stop's start up says another instant for D.
#[test]
fn text_and_json_one_model() {
    let mut off_grid = false;
    for (name, fx) in [("B", scenario_b()), ("D", scenario_d())] {
        let out = fx.outlook();
        let text = out.text();
        let json = out.json();
        let mut stops = 0;
        for pass in json
            .get("passes")
            .and_then(aterm_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            for stop in pass
                .get("stops")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let from = stop
                    .get("from")
                    .and_then(aterm_json::Value::as_i64)
                    .unwrap();
                off_grid |= round_down(from, NOW, &utc()) != round_up(from, NOW, &utc());
                let said = format!(
                    "~{} \u{2192}",
                    long_when(round_down(from, NOW, &utc()), &utc())
                );
                assert!(
                    text.lines()
                        .any(|l| l.trim_start().starts_with("stop") && l.contains(&said)),
                    "{name}: a stop from {said} in\n{text}"
                );
                stops += 1;
            }
        }
        assert!(stops > 0, "{name}: a stop to say");
    }
    assert!(off_grid, "a stop off the ten-minute grid");
    let out = scenario_b().outlook();
    let text = out.text();
    let json = out.json();
    let verdict = json.get("verdict").unwrap();
    assert_eq!(
        verdict.get("kind").and_then(aterm_json::Value::as_str),
        Some("stop")
    );
    let from = verdict
        .get("from")
        .and_then(aterm_json::Value::as_i64)
        .unwrap();
    assert!(verdict.get("to").is_some_and(aterm_json::Value::is_null));
    let end = verdict.get("end").unwrap();
    assert_eq!(
        end.get("kind").and_then(aterm_json::Value::as_str),
        Some("reset")
    );
    let to = end.get("at").and_then(aterm_json::Value::as_i64).unwrap();
    assert_eq!(to, THU9);
    assert!(
        text.contains(&format!(
            "stop  ~{} \u{2192} {}",
            long_when(round_down(from, NOW, &utc()), &utc()),
            long_when(to, &utc())
        )),
        "{text}"
    );
    let passes = json
        .get("passes")
        .and_then(aterm_json::Value::as_array)
        .unwrap();
    let week = &passes[0];
    let stop = &week
        .get("stops")
        .and_then(aterm_json::Value::as_array)
        .unwrap()[0];
    assert_eq!(
        stop.get("from").and_then(aterm_json::Value::as_i64),
        Some(from)
    );
    assert_eq!(stop.get("end"), Some(end));
    // Each row's projected percent in its window line and in its JSON.
    for row in &out.rows {
        let lines = out.row_lines(&row.label).join("\n");
        if let Some((_, proj)) = out.window_json(&row.label, accounts::WEEK_LIMIT)
            && let Some(x10) = proj.get("at_reset_x10").and_then(aterm_json::Value::as_u64)
        {
            let x10 = u32::try_from(x10).unwrap();
            let fills = proj.get("fills_at").is_some_and(|v| !v.is_null());
            if !fills && x10 < 1000 {
                assert!(
                    lines.contains(&format!("~{}% by reset", whole(x10))),
                    "{lines}"
                );
            }
        }
    }
}

/// The scenarios the card and the footer are drawn for.
fn scenes() -> Vec<(&'static str, Outlook)> {
    vec![
        ("A", scenario_a().outlook()),
        ("B", scenario_b().outlook()),
        ("C", scenario_c().outlook()),
        ("D", scenario_d().outlook()),
    ]
}

fn row_text(row: &CardRow) -> String {
    match row {
        CardRow::Note { text, .. } | CardRow::Account { text, .. } => text.clone(),
        CardRow::Plot { cells, .. } => cells.iter().map(|c| c.0).collect(),
        CardRow::Rule => String::new(),
    }
}

fn is_strip(row: &CardRow) -> bool {
    matches!(row, CardRow::Plot { spoken, .. } if spoken.starts_with("Next 7 days"))
}

fn is_legend(row: &CardRow) -> bool {
    matches!(row, CardRow::Plot { spoken, .. } if spoken.starts_with("Legend"))
}

/// The bars under each item: (label, windows kept).
fn bars_of(rows: &[CardRow]) -> Vec<(String, Vec<&'static str>)> {
    let mut out: Vec<(String, Vec<&'static str>)> = Vec::new();
    for row in rows {
        match row {
            CardRow::Account { label, .. } => out.push((label.clone(), Vec::new())),
            CardRow::Plot { spoken, .. } => {
                if let Some(last) = out.last_mut() {
                    if spoken.contains(", week:") {
                        last.1.push("week");
                    } else if spoken.contains(", 5-hour:") {
                        last.1.push("5h");
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// T22 (the agent's half): ACCESSIBILITY. No spoken or title string holds a box-drawing glyph
/// (U+2500-257F); every drawn row carries a caption; the strip's caption names the chain in its
/// order. Negative controls: a strip caption that names the chain backwards fails scenario A
/// (`me` then `alt1`); a bar's glyph in its caption fails every scene.
#[test]
fn spoken_words_carry_no_glyph_and_name_the_chain() {
    let boxy = |s: &str| s.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c));
    for (name, out) in scenes() {
        for room in [60, 44, 30] {
            for row in out.card_rows(Some("me"), room, None) {
                match &row {
                    CardRow::Note {
                        spoken: Some(s), ..
                    } => assert!(!boxy(s), "{name}: {s}"),
                    CardRow::Plot { spoken, .. } => {
                        assert!(
                            !spoken.is_empty(),
                            "{name} {room}: a plot without a caption"
                        );
                        assert!(!boxy(spoken), "{name}: {spoken}");
                    }
                    CardRow::Account { spoken, .. } => assert!(!boxy(spoken), "{name}: {spoken}"),
                    _ => {}
                }
            }
        }
        for label in ["me", "alt1"] {
            let g = out.glance(label, None, true).expect("a glance");
            for s in [&g.title, &g.title_verdict, &g.spoken]
                .into_iter()
                .flatten()
            {
                assert!(!boxy(s), "{name}: {s}");
            }
        }
        let pass = out.week.as_ref().expect("the week pass");
        let mut order: Vec<&str> = Vec::new();
        for link in pass.chain.iter().filter(|l| l.to > l.from) {
            if order.last() != Some(&link.label.as_str()) {
                order.push(&link.label);
            }
        }
        let caption = out
            .card_rows(Some("me"), 60, None)
            .into_iter()
            .find(is_strip)
            .map(|row| match row {
                CardRow::Plot { spoken, .. } => spoken,
                _ => String::new(),
            })
            .expect("the strip");
        let mut at = 0;
        for label in &order {
            let found = caption[at..]
                .find(&format!("{label} until"))
                .or_else(|| caption[at..].find(&format!("then {label}")))
                .or_else(|| caption[at..].find(label));
            let found = found.unwrap_or_else(|| panic!("{name}: {label} in order in {caption}"));
            at += found + label.len();
        }
        if let Some(lean) = &pass.lean {
            assert!(
                caption[at..].contains(&lean.row),
                "{name}: the lean last: {caption}"
            );
        }
    }
}

/// T23: THE CARD FITS. At 60, 44 and 30 cells every row fits and none of the scenarios' rows is
/// cut; items keep roster order; with less height, rows give way in §8.4's order (the extra notes,
/// the strip, the legend, the other rows' bars folded into their items, the carrier's 5-hour bar,
/// its week bar) while the verdict and every item stay. Negative controls: a card that always
/// takes its widest words cuts scenario B's verdict at 44; dropping the carrier's week bar before
/// its 5-hour bar breaks the order.
#[test]
fn card_rows_fit() {
    for (name, out) in scenes() {
        let roster: Vec<String> = out.rows.iter().map(|r| r.label.clone()).collect();
        for room in [60usize, 44, 30] {
            let rows = out.card_rows(Some("me"), room, None);
            for row in &rows {
                let text = row_text(row);
                assert!(row.width() <= room, "{name} {room}: {text}");
                assert!(!text.ends_with('\u{2026}'), "{name} {room}: cut: {text}");
            }
            let items: Vec<String> = rows
                .iter()
                .filter_map(|r| match r {
                    CardRow::Account { label, .. } => Some(label.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(items, roster, "{name} {room}: roster order");
            assert!(
                matches!(rows[0], CardRow::Note { .. }),
                "{name}: the verdict leads"
            );
            if room <= 30 {
                assert!(rows.iter().all(|r| !matches!(r, CardRow::Plot { .. })));
            }
        }
        // The height ladder.
        let full = out.card_rows(Some("me"), 60, None);
        let carrier = out.carrier.clone().expect("a carrier");
        let mut last_score = 0usize;
        for h in (0..=full.len()).rev() {
            let rows = out.card_rows(Some("me"), 60, Some(h));
            assert_eq!(rows[0], full[0], "{name} h{h}: the verdict stays");
            let items = rows
                .iter()
                .filter(|r| matches!(r, CardRow::Account { .. }))
                .count();
            assert_eq!(items, roster.len(), "{name} h{h}: every item stays");
            let bars = bars_of(&rows);
            let others = bars.iter().any(|(l, b)| *l != carrier && !b.is_empty());
            let mine = bars
                .iter()
                .find(|(l, _)| *l == carrier)
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            // What is still shown, as the rung of the ladder it sits on: each step down drops
            // the next kind in order, never a later one first.
            let kinds = [
                rows.iter().any(|r| {
                    row_text(r).starts_with("   not needed")
                        || row_text(r).starts_with("   \u{2248}")
                }),
                rows.iter().any(is_strip),
                rows.iter().any(is_legend),
                others,
                mine.contains(&"5h"),
                mine.contains(&"week"),
            ];
            let present = kinds.to_vec();
            let had: Vec<bool> = {
                let k = [
                    full.iter().any(|r| {
                        row_text(r).starts_with("   not needed")
                            || row_text(r).starts_with("   \u{2248}")
                    }),
                    full.iter().any(is_strip),
                    full.iter().any(is_legend),
                    bars_of(&full)
                        .iter()
                        .any(|(l, b)| *l != carrier && !b.is_empty()),
                    bars_of(&full)
                        .iter()
                        .any(|(l, b)| *l == carrier && b.contains(&"5h")),
                    bars_of(&full)
                        .iter()
                        .any(|(l, b)| *l == carrier && b.contains(&"week")),
                ];
                k.to_vec()
            };
            // The first kind still present that the full card had: every earlier one is gone,
            // every later one the full card had is still there.
            let score = (0..6).find(|&i| had[i] && present[i]).unwrap_or(6);
            for i in 0..6 {
                if had[i] {
                    assert_eq!(
                        present[i],
                        i >= score,
                        "{name} h{h}: kind {i} ({present:?})"
                    );
                }
            }
            assert!(score >= last_score, "{name} h{h}: a later kind went first");
            last_score = score;
            if h >= full.len() {
                assert_eq!(rows, full, "{name}: room enough drops nothing");
            }
        }
    }
}

/// A 5-HOUR WALL IS NOT A SPENT WEEK: `me` hit its 5-hour limit five minutes ago (it resets in
/// 100 minutes) with 58% of its week used. The policy moves the work to `alt1` at the wall, and
/// `me`'s week stays a figure of now, so the work comes back to it and the week stops where B's
/// does with no wall: the same capacity at the same pace. Negative control: a standing 5-hour
/// wall read as walling the week spends `me`'s week at once and stops the week a day early.
#[test]
fn a_five_hour_wall_keeps_the_week() {
    let mut fx = scenario_b();
    accounts::note_wall(
        &mut fx.state,
        "me",
        accounts::SESSION_LIMIT,
        Some(NOW + 100 * MIN),
        NOW - 5 * MIN,
        NOW - 5 * MIN,
    );
    let out = fx.outlook();
    assert_eq!(class_of(&fx, "me", Window::Five), Fresh::Walled);
    assert_eq!(class_of(&fx, "me", Window::Week), Fresh::Now);
    let week = out.week.as_ref().expect("the week pass");
    assert!(
        week.chain
            .iter()
            .any(|l| l.label == "me" && l.from < THU9 && l.to - l.from > D),
        "the week comes back to me: {:?}",
        labels(week)
    );
    let unwalled = scenario_b().outlook();
    assert_eq!(
        stops(week),
        stops(unwalled.week.as_ref().expect("B's week pass")),
        "B's own stop"
    );
}

/// A ROW TAKEN SPENT WALLS AT ONCE: `me` walled by an unnamed limit (its week spent until its
/// reset, the wall standing half an hour in the store), the work moves to `alt1` and, when the
/// store's wall has lapsed, the policy may move back to `me`, whose week the replay holds
/// spent: the replay walls there at once and never runs `me` before its reset. Negative
/// control: a take that carries on a spent row runs `me` past its own wall.
#[test]
fn a_row_taken_spent_walls_at_once() {
    let mut fx = scenario_b();
    accounts::note_wall(
        &mut fx.state,
        "me",
        "limit",
        None,
        NOW - 5 * MIN,
        NOW - 5 * MIN,
    );
    assert_eq!(class_of(&fx, "me", Window::Week), Fresh::Walled);
    let out = fx.outlook();
    let week = out.week.as_ref().expect("the week pass");
    for link in &week.chain {
        assert!(
            link.label != "me" || link.to <= link.from || link.from >= THU9,
            "me runs spent: {:?}",
            labels(week)
        );
    }
    assert!(
        matches!(out.verdict, Verdict::Stop { .. }),
        "{:?}",
        out.verdict
    );
}

/// AN IDLE CARRIER KEEPS ITS FIGURE: `alt1` holds the work but has been idle here since its
/// reading two days ago (31% of its week), so the replay starts it at 31%, not at two days of
/// the work's pace on top: it reaches 97.5% at 16 points a day in 99 h 45 m. Negative control:
/// advancing every carrier from its reading's instant ends its link two days early.
#[test]
fn an_idle_carrier_keeps_its_figure() {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("alt1", Class::Preferred)]);
    me_at_today(&mut fx);
    fx.plan("alt1", "max", Some("20x"), NOW - 3 * D);
    fx.ran("alt1", NOW - 2 * D - H);
    fx.read("alt1", None, Some((310, FRI14)), NOW - 2 * D);
    fx.state.active = Some("alt1".into());
    let out = fx.outlook();
    assert_eq!(out.carrier.as_deref(), Some("alt1"));
    let first = &out.week.as_ref().expect("the week pass").chain[0];
    assert_eq!(
        (first.label.as_str(), first.to - NOW),
        ("alt1", 99 * H + 45 * MIN)
    );
}

/// A RENEWAL'S `max` NAMES NO MULTIPLE: `alt1`'s only plan word is its renewal's `max` (no tier),
/// so it is counted as the carrier's Max 20x and the assumption is said, with the word seen.
/// Negative control: `max` read as Max 5x counts alt1 at a quarter of its week.
#[test]
fn a_renewal_max_is_counted_as_the_carrier() {
    let mut fx = scenario_a();
    fx.series
        .get_mut("alt1")
        .unwrap()
        .retain(|r| !matches!(r.said, Said::Plan(_)));
    fx.renewals.insert("alt1".into(), "max".into());
    let out = fx.outlook();
    let alt1 = out.rows.iter().find(|r| r.label == "alt1").unwrap();
    assert_eq!((alt1.plan.m, alt1.plan.named), (20, None));
    assert!(out.approx.iter().any(|a| matches!(
        a,
        Approx::PlanAssumed { label, word: Some(word), .. } if label == "alt1" && word == "max"
    )));
    assert_eq!(multiple_of("max", None), None);
}

/// AN UNSEEN CARRIER TIER COUNTS EVERY ROW ALIKE: with `me`'s tier never seen, B still stops
/// (both rows are Max 20x), whether alt1's tier was seen or only its renewal's `max`, and the
/// assumption is said. Negative control: the unseen carrier counted as 1 beside a seen 20x
/// gives alt1 twenty weeks of `me`'s and no stop.
#[test]
fn an_unseen_carrier_tier_counts_every_row_alike() {
    for renewal in [false, true] {
        let mut fx = scenario_b();
        for label in ["me", "alt1"].into_iter().take(if renewal { 2 } else { 1 }) {
            fx.series
                .get_mut(label)
                .unwrap()
                .retain(|r| !matches!(r.said, Said::Plan(_)));
        }
        if renewal {
            fx.renewals.insert("alt1".into(), "max".into());
        }
        let out = fx.outlook();
        assert!(
            matches!(out.verdict, Verdict::Stop { .. }),
            "renewal {renewal}: {:?}",
            out.verdict
        );
        assert!(
            out.rows.iter().all(|r| r.plan.m == 1),
            "renewal {renewal}: every row counted alike"
        );
        assert!(
            out.approx
                .iter()
                .any(|a| matches!(a, Approx::PlansAlike { carrier } if carrier == "me"))
        );
    }
}

/// AN AGED 5-HOUR FIGURE IS NOT SAID BARE: the rule's 5-hour part carries no age, so a reading
/// forty minutes old leaves it out (the week, which says its age, stays). Negative control: a
/// figure "as of" shown as the 5-hour part says a 40-minute-old figure as now.
#[test]
fn an_aged_five_hour_figure_is_not_said_bare() {
    let mut fx = scenario_a();
    let fresh = fx.outlook().glance("me", Some(580), true).unwrap();
    assert_eq!(fresh.five.as_deref(), Some("5h 62% left"), "CONTROL");
    fx.state.accounts.remove("me");
    fx.read(
        "me",
        Some((380, NOW + 150 * MIN)),
        Some((580, THU9)),
        NOW - 40 * MIN,
    );
    let g = fx.outlook().glance("me", Some(580), true).unwrap();
    assert_eq!(g.five, None);
}

/// A LEAN'S STOP ENDS ON ITS OWN WORDS: in C the worst case stops to `me`'s reset, the vendor's
/// instant, said exactly, as a stop's end is, and the stop is a projected gap: `╳`, never the
/// wall's `⧗`. Negative controls: the end said as a forecast instant (`~`, rounded down) says
/// the reset 0 minutes early with a `~`; the note marked `⧗` (the words before the glass design)
/// fails the mark.
#[test]
fn a_leans_stop_ends_on_the_reset() {
    let out = scenario_c().outlook();
    let rows: Vec<String> = out
        .card_rows(Some("me"), 60, None)
        .iter()
        .map(row_text)
        .collect();
    let note = rows
        .iter()
        .find(|r| r.contains("has nothing"))
        .expect("the lean's worst case");
    assert!(
        note.ends_with(&format!("\u{2013} {}", out.when(THU9))),
        "{note}"
    );
    assert!(!note.contains(&format!("~{}", out.when(THU9))), "{note}");
    assert!(
        note.contains("\u{2573} ") && !note.contains('\u{29d7}'),
        "{note}"
    );
    assert!(
        out.verdict_spoken().contains(&out.spoken_when(THU9)),
        "{}",
        out.verdict_spoken()
    );
}

/// EVERY ASSUMPTION IS SAID ON THE CARD AND IN THE TEXT: an unseen plan and mixed multiples
/// each show, in the words the one renderer gives. Negative control: a card that leaves an
/// assumption kind out misses it.
#[test]
fn every_assumption_is_said_on_the_card_and_in_the_text() {
    let mut fx = scenario_a();
    fx.series.remove("alt1");
    let out = fx.outlook();
    let kinds: Vec<&str> = out
        .approx
        .iter()
        .map(|a| match a {
            Approx::PlanAssumed { .. } => "plan",
            Approx::PlansAlike { .. } => "alike",
            Approx::MultiplesScaled => "scaled",
        })
        .collect();
    for kind in ["plan", "scaled"] {
        assert!(kinds.contains(&kind), "A says {kind}: {:?}", out.approx);
    }
    let card: Vec<String> = out
        .card_rows(Some("me"), 60, None)
        .iter()
        .map(row_text)
        .collect();
    let text = out.text();
    for a in &out.approx {
        assert!(
            out.approx_words(a, true)
                .iter()
                .any(|w| card.contains(&format!("   \u{2248} {w}"))),
            "the card says {a:?}: {card:#?}"
        );
        let said = out.approx_words(a, false).into_iter().next().unwrap();
        assert!(text.contains(&format!("  \u{2248} {said}\n")), "{text}");
    }
}

/// A STOP'S WORDS LEAD WITH ITS MARK: every warn form (full, short and shortest) of every
/// projected stop in B and D (the week's and the 5-hour one alike) begins with `╳`, the gap,
/// and a wall standing now with `⧗`, its one meaning (glass design §4.3). The words never name
/// the 5-hour window, so the hover title says where D's stop comes from (`(a 5-hour limit)`, as
/// its spoken `A 5-hour stop` does) and B's says no such thing (§2.3). Negative controls: a
/// short form without the mark fails it; the projected stop marked `⧗` (the words before the
/// glass design) fails the first loop; a title made from the words alone (the slice before this
/// fix) says `stop ~20:10–21:10` for D and fails the title.
#[test]
fn a_stops_words_lead_with_its_mark() {
    let forms = |words: &VerdictWords| -> Vec<String> {
        [
            Some(&words.full),
            Some(&words.short),
            words.shortest.as_ref(),
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect()
    };
    for (fx, title) in [
        (scenario_b(), "stop ~Wed 22:00\u{2013}Thu 09:00"),
        (scenario_d(), "stop ~20:10\u{2013}21:10 (a 5-hour limit)"),
    ] {
        let out = fx.outlook();
        let words = out.verdict_words().expect("a stop");
        assert!(words.warn);
        for form in forms(&words) {
            assert!(form.starts_with("\u{2573} "), "{form}");
            assert!(!form.contains("5h") && !form.contains("stop"), "{form}");
        }
        let glance = out.glance("me", None, true).expect("me's glance");
        assert_eq!(glance.title_verdict.as_deref(), Some(title));
        assert_eq!(
            glance
                .spoken
                .as_deref()
                .is_some_and(|s| s.contains("A 5-hour stop")),
            title.contains("5-hour"),
            "{:?}",
            glance.spoken
        );
    }
    let mut walled = Fx::new(&[("me", Class::Preferred)]);
    walled.read("me", None, Some((1000, THU9)), NOW);
    accounts::note_wall(
        &mut walled.state,
        "me",
        accounts::WEEK_LIMIT,
        Some(THU9),
        NOW,
        NOW,
    );
    let out = walled.outlook();
    assert!(
        matches!(out.verdict, Verdict::Walled { to: None, .. }),
        "{:?}",
        out.verdict
    );
    let words = out.verdict_words().expect("a wall");
    assert!(words.warn);
    for form in forms(&words) {
        assert!(form.starts_with('\u{29d7}'), "{form}");
    }
}

/// THE GLANCE SAYS ITS RESET ALOUD: the spoken glance and the card's bar say the week's reset and
/// where today's pace leaves it, in one clause. Negative control: a clause without its reset
/// says only the pace.
#[test]
fn the_glance_says_its_reset_aloud() {
    let out = scenario_a().outlook();
    let g = out.glance("me", Some(580), true).unwrap();
    let spoken = g.spoken.expect("spoken");
    let reset = out.spoken_when(THU9);
    let clause = out
        .reset_pace_spoken(
            Some(THU9),
            out.rows[0].window(Window::Week).unwrap().projection,
        )
        .expect("a clause");
    assert!(clause.contains(&format!("resets {reset}")), "{clause}");
    assert!(
        spoken.to_lowercase().starts_with(&clause.to_lowercase()),
        "{spoken}"
    );
    let card_spoken: Vec<String> = out
        .card_rows(Some("me"), 60, None)
        .iter()
        .filter_map(|r| match r {
            CardRow::Plot { spoken, .. } => Some(spoken.clone()),
            _ => None,
        })
        .collect();
    assert!(
        card_spoken.iter().any(|s| s.contains(&clause)),
        "{card_spoken:#?}"
    );
}

/// A RESET PAST A DAYLIGHT CHANGE IS SAID ON ITS OWN CLOCK: on Los Angeles' clock (PDT until
/// 1 Nov 09:00 UTC, PST after), a reset at 1 Nov 18:30 UTC is 10:30 local. Negative control: one
/// offset for the whole week (now's PDT) says 11:30.
#[test]
fn a_reset_past_a_daylight_change_is_said_on_its_own_clock() {
    const PST_FROM: i64 = 1_793_523_600;
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.offset = |t| if t < PST_FROM { -7 * H } else { -8 * H };
    let reset = NOW + 7 * D - H;
    fx.plan("me", "max", Some("20x"), NOW - H);
    fx.read("me", None, Some((580, reset)), NOW);
    let out = fx.outlook();
    assert_eq!(out.offsets.at(PST_FROM - 1), -7 * H);
    assert_eq!(out.offsets.at(PST_FROM), -8 * H);
    let g = out.glance("me", Some(580), true).unwrap();
    assert_eq!(g.reset.as_deref(), Some("Sun 1 Nov 10:30"));
    assert_eq!(when(reset, NOW, &Offsets::fixed(-7 * H)), "Sun 1 Nov 11:30");
}

// ---------------------------------------------------------------------------------------------
// Projection truth (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §3, slice S1)

/// Thu 5 Nov 2026 17:00, UTC: the glass design's now (its §1).
pub(crate) const T5: i64 = NOW + 941_400;
/// Sat 7 Nov 10:00: case (b)'s week resets.
pub(crate) const SAT7: i64 = T5 + 41 * H;
/// Sat 14 Nov 10:00: the week after.
pub(crate) const SAT14: i64 = SAT7 + 7 * D;

/// The glass design's case (b): one Max 20x account at 63% of its week at Thu 17:00, its reset
/// Sat 10:00, read here from 42% a day before (50% by 23:00, flat overnight): 21% a day.
pub(crate) fn case_b() -> Fx {
    let mut fx = Fx::new(&[("main", Class::Preferred)]);
    fx.now = T5;
    fx.plan("main", "max", Some("20x"), T5 - H);
    for (t, x10) in [(T5 - 24 * H, 420), (T5 - 18 * H, 500), (T5 - 10 * H, 500)] {
        fx.fig("main", t, None, Some((x10, SAT7)));
    }
    fx.read("main", None, Some((630, SAT7)), T5);
    fx.ran("main", T5);
    fx.session("s-main", "main", T5);
    fx
}

/// Two Max 20x rows under `wall` mode at 50% of a week a day: `alt1` carries at 90% (its week
/// resets in 29 h), `main` read now at 30% (its week resets in 41 h).
fn frees_first() -> Fx {
    let mut fx = Fx::new(&[("alt1", Class::Preferred), ("main", Class::Preferred)]);
    fx.mode = Mode::Wall;
    fx.plan("alt1", "max", Some("20x"), NOW - D);
    fx.plan("main", "max", Some("20x"), NOW - D);
    fx.fig("alt1", NOW - 24 * H, None, Some((400, NOW + 29 * H)));
    fx.read("alt1", None, Some((900, NOW + 29 * H)), NOW);
    fx.ran("alt1", NOW);
    fx.session("s-alt1", "alt1", NOW);
    fx.ran("main", NOW - 4 * H);
    fx.read("main", None, Some((300, NOW + 41 * H)), NOW);
    fx
}

/// `me` carries at 50% of a Max 20x week, 30% a day, its week resetting in a day; `pool1` is a
/// shared row never read. Early mode leaves `me` at 97.5% of its next week (day 4.25) for
/// `pool1`: a lean.
pub(crate) fn worst_case() -> Fx {
    let mut fx = Fx::new(&[("me", Class::Preferred), ("pool1", Class::Pool)]);
    fx.plan("me", "max", Some("20x"), NOW - H);
    fx.fig("me", NOW - 24 * H, None, Some((200, NOW + D)));
    fx.read("me", None, Some((500, NOW + D)), NOW);
    fx.ran("me", NOW);
    fx.session("s-me", "me", NOW);
    fx
}

/// A STOP PAST THE HORIZON ENDS AT THE RESET THAT RESUMES IT: in case (b) `main` reaches about
/// 99% by Sat 10:00; its next week runs out on Thu 12 Nov, inside the 7-day horizon (Thu 12 Nov
/// 17:00), and stays out to its reset Sat 14 Nov 10:00, past it. The stop ends there, the
/// vendor's instant said exactly, never at the horizon, and no link follows it. Negative
/// control: restoring `begin_stop`'s `.min(horizon)` plans the stop to the horizon, its reset
/// lookup misses there, and it ends `About(Sat 14 Nov 10:02:30)`.
#[test]
fn a_stop_past_the_horizon_ends_at_the_reset_that_resumes_it() {
    let out = case_b().outlook();
    let week = out.week.as_ref().expect("the week pass");
    assert_eq!(week.horizon, T5 + 7 * D);
    let [stop] = week.stops.as_slice() else {
        panic!("one stop: {:?}", week.stops);
    };
    assert!(stop.from > SAT7 && stop.from < week.horizon, "{stop:?}");
    assert_eq!(stop.end, StopEnd::Reset(SAT14));
    assert_eq!(
        out.verdict,
        Verdict::Stop {
            pass: PassKind::Week,
            from: stop.from,
            end: StopEnd::Reset(SAT14),
            lean: None
        }
    );
    let words = out.verdict_words().unwrap();
    assert_eq!(
        words.full,
        "\u{2573} ~Thu 12 Nov 04:00\u{2013}Sat 14 Nov 10:00"
    );
    assert_eq!(words.short, "\u{2573} ~Thu 12 Nov 04:00");
    assert!(
        week.chain.iter().all(|l| l.to <= stop.from),
        "{:?}",
        labels(week)
    );
    assert!(
        !out.text().contains(&long_when(week.horizon, &utc())),
        "the horizon is never said:\n{}",
        out.text()
    );
}

/// A STOP ENDS AT THE FIRST ACCOUNT THAT FREES IT: in [`frees_first`] the work runs alt1, main,
/// alt1, main, each to its wall, and every account is out from main's second wall (day 5.6)
/// until alt1's week resets (day 8.2): past the horizon, and before main's own reset (day 8.7).
/// The policy resumes on alt1 there, so the stop ends `Reset(alt1's)`. Negative control: a stop
/// past the horizon planned to the carrier's next reset (the shortcut the design rejects) ends
/// `Reset(main's)`.
#[test]
fn a_stop_ends_at_the_first_account_that_frees_it() {
    let out = frees_first().outlook();
    let week = out.week.as_ref().expect("the week pass");
    let (a2, m2) = (NOW + 29 * H + 7 * D, NOW + 41 * H + 7 * D);
    assert_eq!(
        labels(week),
        [
            ("alt1", NOW, NOW + 17_280, Ends::Walls),
            ("main", NOW + 17_280, NOW + 138_240, Ends::Walls),
            ("alt1", NOW + 138_240, NOW + 311_040, Ends::Walls),
            ("main", NOW + 311_040, NOW + 483_840, Ends::Walls),
        ]
    );
    assert!(week.horizon < a2 && a2 < m2);
    assert_eq!(stops(week), [(NOW + 483_840, StopEnd::Reset(a2))]);
}

/// A WORST-CASE STOP NEVER ENDS AT THE INVENTED WALL: in [`worst_case`] `me` leaves for the
/// unread `pool1` at day 4.25; the worst case walls `pool1` at once, the work goes back to `me`,
/// which walls two hours later, and every account is out until `me`'s next reset (day 8), past
/// the horizon. `pool1`'s wall is the worst case's invention, standing to the resume bound, past
/// every instant the replay steps to, so it is never an event and the stop ends at `me`'s reset.
/// No flag is needed for that: a flag that kept the invention's instants out of the events
/// changed no outcome, since they lie at or past the bound by construction (review of S1).
/// Negative control (run): `assume_worst` noting its wall to the horizon (the replay before this
/// slice) ends the stop `Reset(horizon)`, the invented instant.
#[test]
fn a_worst_case_stop_never_ends_at_the_invented_wall() {
    let out = worst_case().outlook();
    let week = out.week.as_ref().expect("the week pass");
    let lean = week.lean.as_ref().expect("a lean");
    assert_eq!(lean.row, "pool1");
    let Verdict::Lean {
        stop: Some((from, end)),
        ..
    } = out.verdict
    else {
        panic!("a lean whose worst case stops: {:?}", out.verdict);
    };
    assert!(from > lean.at && from < week.horizon, "{from}");
    assert_eq!(end, StopEnd::Reset(NOW + 8 * D));
    assert!(lean.best.stops.is_empty(), "the best case runs on");
}

/// A STOP NOTHING ENDS IS OPEN: `main` alone, 35% of a week whose reset is 14 days and an hour
/// away (no vendor week is that long: the bound is what is under test), walls at day 6.5 at 10%
/// a day, its wall believed to that reset, and nothing frees it by the horizon plus a week: the
/// stop is `Open`, said `╳ from ~…` in every form and `with no end in reach` aloud, and its JSON
/// end is `{"kind": "open", "at": null}`. Negative control: a finish that ends a stop still
/// open at the bound `About(bound)` says `╳ ~…–~…`.
#[test]
fn a_stop_nothing_ends_is_open() {
    let mut fx = Fx::new(&[("main", Class::Preferred)]);
    let far = NOW + 14 * D + H;
    fx.plan("main", "max", Some("20x"), NOW - H);
    fx.fig("main", NOW - 24 * H, None, Some((250, far)));
    fx.read("main", None, Some((350, far)), NOW);
    fx.ran("main", NOW);
    fx.session("s-main", "main", NOW);
    let out = fx.outlook();
    let Verdict::Stop { from, end, .. } = out.verdict else {
        panic!("a stop: {:?}", out.verdict);
    };
    assert_eq!(from, NOW + 6 * D + 12 * H);
    assert_eq!(end, StopEnd::Open);
    let said = format!("\u{2573} from {}", out.about(from));
    assert_eq!(said, "\u{2573} from ~Sun 1 Nov 07:00");
    let words = out.verdict_words().unwrap();
    assert_eq!(
        (
            words.full.as_str(),
            words.short.as_str(),
            words.shortest.as_deref()
        ),
        (said.as_str(), said.as_str(), Some(said.as_str()))
    );
    assert!(
        out.verdict_spoken().contains("with no end in reach"),
        "{}",
        out.verdict_spoken()
    );
    let json = out.json();
    let end = json
        .get("verdict")
        .and_then(|v| v.get("end"))
        .expect("the verdict's end");
    assert_eq!(
        end.get("kind").and_then(aterm_json::Value::as_str),
        Some("open")
    );
    assert!(end.get("at").is_some_and(aterm_json::Value::is_null));
}

/// THE FILLS FOLLOW THE CHAIN AND DROP AT RESETS: in [`frees_first`] each link's row has a knot
/// at the link's start and its end, its fill rising between and full where it walls; each
/// carried row's reset inside the horizon is a drop, two knots at one instant (alt1 at day 1.2,
/// main at day 1.7); and in A `pool1`, read and never carried, drops to unknown at its reset.
/// In [`worst_case`] the worst branch assumes the unread `pool1` full: an assumption, never a
/// figure, so its every knot there is unknown (never an inferred 100%). Knots are in time order
/// and never past the horizon. Negative controls: an `on_resets` that pushes no knot draws alt1
/// rising straight across its reset, with no drop; `assume_worst` noting its week
/// `Known::Exact` (the slice before this fix) gives `pool1` knots of `Some(1000)`.
#[test]
fn fills_follow_the_chain_and_drop_at_resets() {
    let out = frees_first().outlook();
    let week = out.week.as_ref().expect("the week pass");
    let at = |label: &str, t: i64| -> Vec<Option<u16>> {
        week.fills[label]
            .iter()
            .filter(|k| k.t == t)
            .map(|k| k.x10)
            .collect()
    };
    for knots in week.fills.values() {
        assert!(knots.windows(2).all(|w| w[0].t <= w[1].t), "{knots:?}");
        assert!(knots.iter().all(|k| k.t >= NOW && k.t <= week.horizon));
    }
    for link in &week.chain {
        let (start, end) = (at(&link.label, link.from), at(&link.label, link.to));
        let (Some(Some(start)), Some(Some(end))) = (start.last(), end.first()) else {
            panic!("{link:?}: {:?}", week.fills[&link.label]);
        };
        assert!(start < end, "{link:?} rises");
        assert_eq!(*end, 1000, "{link:?} walls full");
    }
    assert_eq!(at("alt1", NOW + 29 * H), [Some(1000), Some(0)]);
    assert_eq!(at("main", NOW + 41 * H), [Some(1000), Some(0)]);
    let a = scenario_a().outlook();
    let pool = &a.week.as_ref().expect("A's week pass").fills["pool1"];
    let drop = [
        Knot {
            t: SAT8,
            x10: Some(400),
        },
        Knot { t: SAT8, x10: None },
    ];
    assert!(pool.windows(2).any(|w| w == drop), "{pool:?}");

    let worst = worst_case().outlook();
    let lean = worst
        .week
        .as_ref()
        .and_then(|p| p.lean.as_deref())
        .expect("a lean on pool1");
    let assumed = &lean.worst.fills["pool1"];
    assert!(
        assumed.iter().any(|k| k.t == lean.at) && assumed.iter().all(|k| k.x10.is_none()),
        "{assumed:?}"
    );
}

/// THE NEXT WEEK'S RUN-OUT HAS A CURVE: in case (b) `main`'s projection keeps its first window's
/// meaning (99% by Sat 10:00), and its fills carry the week after: the drop at Sat 10:00, then a
/// rise to 100% at the stop's start, flat (out) to the horizon. Negative control: a replay that
/// keeps only `Proj` (as before this slice) has no point past Sat 10:00.
#[test]
fn the_next_weeks_run_out_has_a_curve() {
    let out = case_b().outlook();
    let week = out.week.as_ref().expect("the week pass");
    let proj = out.rows[0]
        .window(Window::Week)
        .and_then(|w| w.projection)
        .expect("the first window's projection");
    assert_eq!(whole(proj.at_reset_x10), 99);
    assert!(proj.fills_at.is_none());
    let stop = week.stops[0].from;
    let knots = &week.fills["main"];
    assert_eq!(
        knots.first(),
        Some(&Knot {
            t: T5,
            x10: Some(630)
        })
    );
    let drop = knots
        .iter()
        .position(|k| k.t == SAT7 && k.x10 == Some(0))
        .expect("the drop at the reset");
    assert_eq!(
        knots[drop - 1],
        Knot {
            t: SAT7,
            x10: u16::try_from(proj.at_reset_x10).ok()
        }
    );
    let after = &knots[drop + 1..];
    assert!(after.windows(2).all(|w| w[0].x10 <= w[1].x10), "{after:?}");
    assert!(after.contains(&Knot {
        t: stop,
        x10: Some(1000)
    }));
    assert_eq!(
        after.last(),
        Some(&Knot {
            t: week.horizon,
            x10: Some(1000)
        })
    );
}

/// A BEAT REPEATS ONLY ITS ROW'S WEEK: a series beat says its figures equal the last `fig`
/// row's, both windows, `None` included (`account_series::note_within`). So a beat after a row
/// that read the week repeats that week at the beat's time, and a beat after a row that read
/// the 5 hours alone repeats no week: [`RowOutlook::read`] never holds a week this Mac did not
/// read then. Negative control: a `reads_of` whose `last` only a row with a week sets (the
/// slice before this fix) brings 40% back at the third row's time.
#[test]
fn a_beat_repeats_only_its_rows_week() {
    let reset = NOW + 3 * D;
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.fig("me", NOW - 5 * H, Some((200, NOW + H)), Some((400, reset)));
    fx.fig("me", NOW - 4 * H, Some((300, NOW + H)), None);
    fx.fig("me", NOW - 2 * H, Some((350, NOW + H)), Some((450, reset)));
    let rows = fx.series.get_mut("me").expect("me's series");
    for t in [NOW - 3 * H, NOW - H] {
        rows.push(SeriesRow {
            t,
            by: MACHINE.into(),
            said: Said::Beat,
        });
    }
    rows.sort_by_key(|r| r.t);
    let out = fx.outlook();
    let read = |t: i64, x10: u16| Read {
        t,
        x10,
        reset,
        epoch: None,
    };
    assert_eq!(
        out.rows[0].read,
        [
            read(NOW - 5 * H, 400),
            read(NOW - 2 * H, 450),
            read(NOW - H, 450)
        ]
    );
}

/// FORECAST INSTANTS ROUND BY THEIR LEAD: seen at 19:30, a start rounds down and an end up, to
/// 10 minutes up to six hours ahead, 30 minutes up to a day, an hour beyond, on the local
/// clock; the glass design's run-out at 05:50, 6.5 days ahead, reads `~Thu 12 Nov 05:00`; a
/// vendor reset is said exactly. Negative control: one 10-minute grain (the `ROUND_S` before
/// this slice) says `~Thu 12 Nov 05:50`.
#[test]
fn forecast_instants_round_by_their_lead() {
    let u = utc();
    let at = |h: i64, m: i64| NOW + h * H + m * MIN;
    let both = |t: i64, offsets: &Offsets| (round_down(t, NOW, offsets), round_up(t, NOW, offsets));
    assert_eq!(both(at(2, 7), &u), (at(2, 0), at(2, 10)));
    assert_eq!(both(at(5, 57), &u), (at(5, 50), at(6, 0)));
    assert_eq!(both(at(6, 7), &u), (at(6, 0), at(6, 30)));
    assert_eq!(both(at(30, 7), &u), (at(29, 30), at(30, 30)));
    assert_eq!(both(at(30, 30), &u), (at(30, 30), at(30, 30)));
    // On a half-hour zone's clock the hour is its own: 01:37 UTC is 07:07 in India.
    let india = Offsets::fixed(5 * H + 30 * MIN);
    assert_eq!(both(at(30, 7), &india), (at(30, 0), at(31, 0)));
    let runout = T5 + 6 * D + 12 * H + 50 * MIN;
    assert_eq!(when(round_down(runout, T5, &u), T5, &u), "Thu 12 Nov 05:00");
    assert_eq!(when(round_up(runout, T5, &u), T5, &u), "Thu 12 Nov 06:00");
    let b = scenario_b().outlook();
    assert!(
        b.verdict_words()
            .unwrap()
            .full
            .ends_with("\u{2013}Thu 09:00")
    );
}

/// WHEN ADDS THE DATE FROM A WEEK: `HH:MM` today, `Ddd HH:MM` one to six days on, `Ddd D Mon
/// HH:MM` from seven (a day-7 instant shares today's weekday) and for an earlier day; spoken the
/// same way. Negative controls: the day-7 form with no weekday (`1 Nov 19:30`, before this
/// slice) and the one with no date (`Sun 19:30`, which reads as today) each fail it.
#[test]
fn when_adds_the_date_from_a_week() {
    let u = utc();
    assert_eq!(when(NOW + H, NOW, &u), "20:30");
    assert_eq!(when(NOW + D, NOW, &u), "Mon 19:30");
    assert_eq!(when(NOW + 6 * D, NOW, &u), "Sat 19:30");
    assert_eq!(when(NOW + 7 * D, NOW, &u), "Sun 1 Nov 19:30");
    assert_eq!(when(NOW - D, NOW, &u), "Sat 24 Oct 19:30");
    assert_eq!(spoken_when(NOW + 3 * D, NOW, &u), "Wednesday 19:30");
    assert_eq!(spoken_when(NOW + 7 * D, NOW, &u), "Sunday 1 November 19:30");
    assert_eq!(
        spoken_when(T5 + 6 * D + 12 * H, T5, &u),
        "Thursday 12 November 05:00"
    );
}

/// A WALL SAYS OUT, NEVER 100: a standing wall's week is `Figure::Out` (`out`; `out until its
/// reset` aloud), and the glance draws no bar and gives the week no title words (the footer's
/// own `weekly out` says it, so the account hover never reads `weekly out (out)`), with no
/// percentage anywhere, though the last reading said 97%; its verdict clause says how long.
/// Negative controls: the glance's wall-as-1000 path (before this slice) draws a full bar
/// titled `100% used`; a wall titled `out` (the slice before this fix) fails the title.
#[test]
fn a_wall_says_out_never_100() {
    let mut fx = Fx::new(&[("me", Class::Preferred)]);
    fx.read("me", None, Some((970, THU9)), NOW - 10 * MIN);
    accounts::note_wall(
        &mut fx.state,
        "me",
        accounts::WEEK_LIMIT,
        Some(THU9),
        NOW,
        NOW,
    );
    let out = fx.outlook();
    let week = out.rows[0].window(Window::Week).expect("the week");
    assert_eq!(week.fresh, Fresh::Walled);
    let figure = Figure::of_week(week, out.as_of);
    assert_eq!(figure, Figure::Out);
    assert_eq!(figure.words(), "out");
    assert_eq!(figure.spoken(), "out until its reset");
    let g = out.glance("me", Some(970), true).expect("a glance");
    assert_eq!(g.bar, None);
    assert_eq!(g.title, None);
    let clause = g.title_verdict.expect("the wall's clause");
    assert!(
        clause.starts_with('\u{29d7}') && !clause.contains('%'),
        "{clause}"
    );
}

/// A LOWER BOUND SAYS AT LEAST: a reading of now is exact (`58%`); a preferred row idle here
/// since a reading three hours old, or a pool row's, is a bound (`≥31%`, `≥40%`), since another
/// Mac may have used it since; so is the carrier's own reading an hour and a half old while in
/// use (`stale`, whose pace is refused but whose figure stays true: `≥58%`, as
/// [`Figure::of_reading`] says the same reading); a reading whose reset passed is unread (`?`);
/// under 100 is never said 100; and a bound rounds down, so it stays true (62.5% is `≥62%`),
/// where an exact figure rounds half up (`63%`). Negative controls: an aged reading said exact
/// prints `31%` for alt1; `of_week` showing only the classes `shows_figure` admits (the slice
/// before this fix) says `?` for the stale carrier; a bound rounded half up says `≥63%`.
#[test]
fn a_lower_bound_says_at_least() {
    let out = scenario_a_idle().outlook();
    let fig = |label: &str| {
        let row = out.rows.iter().find(|r| r.label == label).unwrap();
        Figure::of_week(row.window(Window::Week).unwrap(), out.as_of)
    };
    assert_eq!(fig("me"), Figure::Exact(580));
    assert_eq!(fig("me").words(), "58%");
    assert_eq!(fig("alt1"), Figure::AtLeast(310));
    assert_eq!(fig("alt1").words(), "\u{2265}31%");
    assert_eq!(fig("alt1").spoken(), "at least 31 percent used");
    assert_eq!(fig("pool1").words(), "\u{2265}40%");
    assert_eq!(fig("alt2"), Figure::Unread);
    assert_eq!(fig("alt2").words(), "?");
    assert_eq!(Figure::of_reading(996, THU9, NOW, NOW).words(), "99%");
    assert_eq!(Figure::of_reading(1000, THU9, NOW, NOW).words(), "100%");
    assert_eq!(
        Figure::of_reading(580, THU9, NOW - 2 * H, NOW),
        Figure::AtLeast(580)
    );

    let mut aged = scenario_a();
    aged.state.accounts.remove("me");
    aged.read("me", None, Some((580, THU9)), NOW - 90 * MIN);
    let stale = aged.outlook();
    let me = stale.rows.iter().find(|r| r.label == "me").unwrap();
    assert_eq!(me.window(Window::Week).unwrap().fresh, Fresh::Stale);
    assert_eq!(me.figure(stale.as_of), Figure::AtLeast(580));
    assert_eq!(
        me.figure(stale.as_of),
        Figure::of_reading(580, THU9, NOW - 90 * MIN, NOW)
    );
    assert_eq!(me.figure(stale.as_of).words(), "\u{2265}58%");

    assert_eq!(Figure::AtLeast(625).words(), "\u{2265}62%");
    assert_eq!(Figure::AtLeast(625).spoken(), "at least 62 percent used");
    assert_eq!(Figure::AtLeast(999).words(), "\u{2265}99%");
    assert_eq!(Figure::AtLeast(1000).words(), "\u{2265}100%");
    assert_eq!(Figure::Exact(625).words(), "63%");
    assert_eq!(Figure::Exact(995).words(), "99%");
}
