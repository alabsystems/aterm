// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic accounts for tests of what the outlook and the glass say: a roster, an accounts
//! state and series rows built through the stores' own writers ([`Fx`]), and the glass design's
//! four cases (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1) at its now, Thu 5 Nov 2026
//! 17:00, UTC. The outlook's and the quota view's tests build on it, and so do the window's
//! (its quota row's gallery draws [`cases`]) and its private-dev `harness-layouts` scene, which a
//! debug build or one with `dev-seams` compiles; no shipped build reaches it.

use std::collections::{BTreeMap, BTreeSet};

use super::account_series::{Fig, PlanSaid, Said, SeriesRow};
use super::accounts::{self, Class, Mode, Roster, Row, SessionRow, State, TapRead, WindowRead};
use super::outlook::{Inputs, Offsets, Outlook, outlook};
use super::quota_view::QuotaView;

/// Sun 25 Oct 2026 19:30, UTC (the words below are said at offset 0).
pub const NOW: i64 = 1_792_956_600;
/// A minute, an hour and a day, in seconds.
pub const MIN: i64 = 60;
pub const H: i64 = 3600;
pub const D: i64 = 86_400;
/// The machine every synthetic reading is said on.
pub const MACHINE: &str = "0a0b";

/// The clock the words below are said on.
pub fn utc() -> Offsets {
    Offsets::fixed(0)
}

/// A synthetic roster, state and series (the quota view's tests build on it too).
pub struct Fx {
    pub roster: Roster,
    pub state: State,
    pub enrolled: BTreeSet<String>,
    pub series: BTreeMap<String, Vec<SeriesRow>>,
    pub renewals: BTreeMap<String, String>,
    pub mode: Mode,
    pub credits: bool,
    pub now: i64,
    /// The local clock's offset at each instant.
    pub offset: fn(i64) -> i64,
}

impl Fx {
    /// The first row is the default login; the others have their own login, enrolled.
    pub fn new(rows: &[(&str, Class)]) -> Fx {
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
    pub fn read(
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
    pub fn fig(&mut self, label: &str, t: i64, five: Option<(u32, i64)>, week: Option<(u32, i64)>) {
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

    pub fn plan(&mut self, label: &str, word: &str, tier: Option<&str>, t: i64) {
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

    pub fn ran(&mut self, label: &str, at: i64) {
        self.state
            .accounts
            .entry(label.to_owned())
            .or_default()
            .ran_at = Some(at);
    }

    pub fn session(&mut self, sid: &str, label: &str, at: i64) {
        self.state.sessions.insert(
            sid.into(),
            SessionRow {
                account: label.into(),
                at,
                held: None,
            },
        );
    }

    pub fn inputs(&self) -> Inputs<'_> {
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

    pub fn outlook(&self) -> Outlook {
        outlook(&self.inputs())
    }
}

/// Thu 5 Nov 2026 17:00, UTC: the glass design's now (its §1).
pub const T5: i64 = NOW + 941_400;
/// Sat 7 Nov 10:00: case (b)'s week resets.
pub const SAT7: i64 = T5 + 41 * H;
/// Sat 14 Nov 10:00: the week after.
pub const SAT14: i64 = SAT7 + 7 * D;

/// Mon 9 Nov 09:00: case (c)'s `alt1` week resets.
pub const MON9: i64 = T5 + 3 * D + 16 * H;
/// Wed 11 Nov 09:00: case (c)'s `alt2` week resets. (The design's first table said 18:00, but a
/// week that resets 6 d 1 h ahead began inside the pace's 24-hour lookback, so its 90% would
/// count as a day's rise, 100% a day; 09:00 keeps the design's 36% a day.)
pub const WED9: i64 = T5 + 5 * D + 16 * H;

/// Case (a): one Max 20x account at 40% of its week at Thu 17:00, its reset Sat 10:00, read here
/// from 30% a day before: 10% a day, about 57% by its reset.
pub fn case_a() -> Fx {
    let mut fx = Fx::new(&[("main", Class::Preferred)]);
    fx.now = T5;
    fx.plan("main", "max", Some("20x"), T5 - H);
    for (t, x10) in [(T5 - 24 * H, 300), (T5 - 12 * H, 350)] {
        fx.fig("main", t, None, Some((x10, SAT7)));
    }
    fx.read("main", None, Some((400, SAT7)), T5);
    fx.ran("main", T5);
    fx.session("s-main", "main", T5);
    fx
}

/// The glass design's case (b): one Max 20x account at 63% of its week at Thu 17:00, its reset
/// Sat 10:00, read here from 42% a day before (50% by 23:00, flat overnight): 21% a day.
pub fn case_b() -> Fx {
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

/// Case (c): `main` at 75% (resets Sat 10:00), `alt1` at 80% (Mon 09:00) and `alt2` at 90% (Wed
/// 09:00), all Max 20x and read now, at 36% a day (`main` read from 39% a day before): the work
/// moves main → alt1 → alt2 as main and alt1 reach their low mark, and once alt2 is out back to
/// main and alt1 for their last points; every account is out from Sat 05:40, when the three
/// weeks' 55 points are spent, until main's reset, then main carries, then alt1 (§1.3 (c)).
pub fn case_c() -> Fx {
    let mut fx = Fx::new(&[
        ("main", Class::Preferred),
        ("alt1", Class::Preferred),
        ("alt2", Class::Preferred),
    ]);
    fx.now = T5;
    for label in ["main", "alt1", "alt2"] {
        fx.plan(label, "max", Some("20x"), T5 - H);
    }
    for (t, x10) in [(T5 - 24 * H, 390), (T5 - 12 * H, 570)] {
        fx.fig("main", t, None, Some((x10, SAT7)));
    }
    fx.read("main", None, Some((750, SAT7)), T5);
    fx.read("alt1", None, Some((800, MON9)), T5);
    fx.read("alt2", None, Some((900, WED9)), T5);
    fx.ran("main", T5);
    fx.session("s-main", "main", T5);
    fx
}

/// Case (d): one account, never read.
pub fn case_d() -> Fx {
    let mut fx = Fx::new(&[("main", Class::Preferred)]);
    fx.now = T5;
    fx
}

/// The words the tap prints under the composer's bottom rule for the tab `view` is of
/// ([`super::tap::Reading::week_words`]), said on [`utc`] at the view's now: its account's week,
/// as the view reads it, and its reset; no figure (`week · no figure yet`) where the view has
/// none to read. A scene that lays the row for the claim to read takes these, so a case's figure
/// or the tap's grammar cannot drift from what the glass draws over them.
#[must_use]
pub fn tap_words(view: &QuotaView) -> String {
    use super::outlook::Figure;
    use super::tap::{Reading, Row, Window};
    let seven_day = view.named.iter().find(|n| n.this_tab).and_then(|tab| {
        let (Figure::Exact(x10) | Figure::AtLeast(x10)) = tab.figure else {
            return None;
        };
        let x10 = u32::from(x10);
        Some(Window {
            pct: (x10 + 5) / 10,
            resets_at: tab.reset.unwrap_or_default(),
            pct_x10: Some(x10),
        })
    });
    Reading {
        at: view.as_of,
        sid: "s-main".to_owned(),
        pid: 1,
        session: None,
        login: None,
        home: None,
        api_ms: 0,
        five_hour: None,
        seven_day,
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        ctx_left: None,
        dur_us: 0,
        row: Some(Row::Own),
    }
    .week_words(view.as_of, Some(&utc()))
}

/// The design's four cases as `main`'s tab sees them, each named by its letter: one account
/// with room (a), one that runs out before its reset (b), three whose work moves and then
/// waits (c), and one never read (d).
#[must_use]
pub fn cases() -> Vec<(&'static str, QuotaView)> {
    [
        ("a", case_a()),
        ("b", case_b()),
        ("c", case_c()),
        ("d", case_d()),
    ]
    .into_iter()
    .map(|(name, fx)| (name, fx.outlook().quota("main", None, true)))
    .collect()
}
