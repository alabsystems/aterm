// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `outlook-captures` ([`cmd_outlook_captures`]): the accounts outlook as the window draws it
//! (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`). One private headless
//! instance per scene, look and scale, its HOME seeded with synthetic identities only
//! (`me@example.test`, `alt1`, `alt2`, `pool1`): a Claude Code profile, the roster, the accounts
//! store and `me`'s series ([`seed`]). Its one tab runs the lab's `claude` fixture
//! ([`crate::fleet_fixture::claude`], by a link of that name), so the footer reads the profile it
//! was seeded with. Each scene's footer is shot at the three widths of §2.4, and the Account card
//! of the covered, gap, never-read, no-pace and no-figure scenes, of the glass's case (c) (J) and
//! of the two card scenes L and M at 60 and 44 cells ([`CARD_SCENES`]); beside each png the
//! card's rows (`controls fact-menu`) and a `manifest-<tag>.tsv` row. `--aterm` points the same
//! seeds at another build (main's, for the shots side by side).
//!
//! L and M are the card's own scenes, the two a Mac in a lab shows and no other scene seeds: a
//! pool file (`~/.secrets/ai-pool.toml`) that gives each account its home Mac and names this Mac
//! `m20`, and a scratch vault this lab process serves on loopback with the instance's home
//! seated in it ([`crate::setup_token_lab::seat_scratch_vault`]). L's tab runs on a login no
//! roster row names, whose home is another Mac and for which the vault holds no token; M's runs
//! on a pool row whose home is this Mac and for which the vault holds a verified lab token
//! ([`crate::stub::labelled_oauth_token`], no credential anywhere).
//!
//! Scenes I, J and K are the harness glass's cases (b), (c) and (d)
//! (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1, §6 S5, §7), under Cruise, their
//! accounts `main`, `alt1` and `alt2`: the fixture's `--settings` names the build's own usage tap
//! (`<aterm> harness tap`) as its statusLine, which the fixture runs with `main`'s seeded figures
//! as its payload's `rate_limits` ([`crate::fleet_fixture::RATE_LIMITS`]) and prints under its
//! bottom rule, so line E is the quota row the window draws over the tap's words; every glass
//! scene's reading is this Mac's, so the outlook holds it. J's card is a tab aterm can switch:
//! its `Forecast` row is the outlook's projection for the tab's account (the design's §1.4). The
//! scenes A to H run no statusLine: their line D carries the account unit as the fallback, and
//! their cards are Autopilot Off's, whose `Forecast` row says there is none and why.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts::{self, Class, TapRead, WindowFig, WindowRead};
use aterm_agent::harness::tap;
use aterm_agent::harness::{account_series, outlook};

use crate::band::Band;
use crate::instance::{
    BOOT, Instance, Launch, World, config_after, config_with, take_value, with_harness_keys,
};
use crate::{Fail, Res};

const M: i64 = 60;
const H: i64 = 3600;
const D: i64 = 86_400;

/// The scenes of §2.4: A covered, B a gap, C an alt never read, D a 5-hour stop, E no pace yet,
/// F walled, G an aged figure, H no figure; and the glass design's line E: I one account out
/// before its reset (case (b)), J three accounts whose work moves then waits (case (c)), K no
/// figure yet (case (d)). And the Account card's own: L a login outside this Mac's pool whose
/// home is another Mac, M a pool account with a vault token whose home is this Mac.
pub const SCENES: [char; 13] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M',
];

/// The scenes whose Account card is shot: covered, a gap, an alt never read (§8.2), no pace yet,
/// no figure and no history (H), the glass design's case (c) under Cruise (J), the one card here
/// whose tab aterm can switch, so the one whose `Forecast` row holds a projection (its §1.4),
/// and the card's own two (L, M).
pub const CARD_SCENES: [char; 8] = ['A', 'B', 'C', 'E', 'H', 'J', 'L', 'M'];

/// The footer's three widths of §2.4, as pane columns.
pub const WIDTHS: [(&str, u16); 3] = [("wide", 170), ("medium", 100), ("narrow", 70)];

/// The card's two widths (§8.4): 60 cells beside its drawn rows, and 44, as pane columns. The
/// narrow pane is the card's 44 cells and its border, no more: a card with a chart takes every
/// cell a pane has up to 60, so a wider pane would shoot it at more than 44.
pub const CARD_WIDTHS: [(&str, u16); 2] = [("card60", 100), ("card44", 46)];

/// The pane's rows.
const LINES: u16 = 40;

/// How long a footer may take to show the account unit: a hang detector, not a budget.
const SHOWN: Duration = Duration::from_secs(60);

/// A look: the theme its instance's `aterm.toml` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Day,
    Night,
}

impl Look {
    const BOTH: [Look; 2] = [Look::Day, Look::Night];

    const fn name(self) -> &'static str {
        match self {
            Look::Day => "day",
            Look::Night => "night",
        }
    }

    const fn theme(self) -> &'static str {
        match self {
            Look::Day => "aterm Day",
            Look::Night => "aterm Night",
        }
    }
}

/// One account's seeded reading: `(pct_x10, reset, read)` of its week and of its five hours, as
/// offsets from now.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    week: Option<(u32, i64, i64)>,
    five: Option<(u32, i64, i64)>,
}

/// What a scene seeds.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The roster's rows in order: the first this Mac's own login (`me`, or `main` in the glass
    /// scenes), then logins with a folder each.
    rows: Vec<(&'static str, Class)>,
    /// The store's readings.
    seen: Vec<(&'static str, Seen)>,
    /// The own login's series: `(offset, pct_x10)` of its week.
    series: Vec<(i64, u32)>,
    /// The profile's cached figures `(week %, five-hour %, age in seconds)`, or none read.
    profile: Option<(u32, u32, i64)>,
    /// The plan rows earlier footers' gated profile reads wrote: `(label, tier, age)`, each of a
    /// roster row (the own login's only where its profile was read).
    plans: &'static [(&'static str, &'static str, i64)],
    /// The fixture runs the build's usage tap as its statusLine, its payload's `rate_limits` the
    /// own login's seen figures: line E is the quota row.
    tapped: bool,
    /// Every login's reading in `seen` was made on this Mac, as a tap's is, so it is a row of
    /// that login's series too (aterm-agent's `quota_scenes::Fx::read`), which the card's lane
    /// draws; else only the own login's `series` is written (the scenes A to H).
    read_here: bool,
    /// The Autopilot mode the instance runs under.
    autopilot: &'static str,
    /// The tab's login is in no roster row: the profile in the default home names `me`, and
    /// the roster's first row, the default home's, names another account. The footer shows a
    /// default home's profile by its own address whatever its row expects, so the card is of
    /// an account outside this Mac's pool.
    outside: bool,
    /// The pool file: the lab name of the home Mac of the tab's account ([`THIS_MAC`] for this
    /// one), every other row's home this Mac; or no pool file, as on a Mac in no lab.
    home: Option<&'static str>,
    /// The scratch vault the instance's home is seated in, or none.
    vault: Option<Vault>,
}

/// What a scene's scratch vault holds for the tab's account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vault {
    /// No token.
    Empty,
    /// A verified lab token, made on this Mac.
    Token,
}

/// The lab name the card scenes' store and pool file give this Mac.
const THIS_MAC: &str = "m20";

impl Plan {
    /// This Mac's own login: the roster's first row, or `me` where no row names it.
    fn own(&self) -> &'static str {
        if self.outside {
            return "me";
        }
        self.default_row()
    }

    /// The row of the default home, which has no folder of its own: the roster's first.
    fn default_row(&self) -> &'static str {
        self.rows.first().map_or("me", |row| row.0)
    }

    /// The address the roster gives the row `label`: the profile's for the own login's row;
    /// in a card scene with a pool file each other row its own too (an account is named by its
    /// address, and the pool file lists an account by it), elsewhere none.
    fn email_of(&self, label: &str) -> Option<String> {
        if label == self.own() {
            Some(ME_EMAIL.into())
        } else {
            self.home.map(|_| format!("{label}@example.test"))
        }
    }

    /// The pool file's text at `now`, where the scene has one: the tab's account at its home
    /// since 40 days (set on the accounts sheet where that is another Mac, assigned where it
    /// is this one), each other row assigned to this Mac since 30.
    fn pool_file(&self, now: i64) -> Option<String> {
        let home = self.home?;
        let stamp = |ago: i64| crate::util::utc_iso(now - ago);
        let account = |email: &str, mac: &str, why: &str, since: i64| {
            let used = stamp(D);
            format!(
                "\n[[account]]\nprovider = \"claude\"\nemail = \"{email}\"\nwarm = [\"{mac}\"]\n\
                 since = [\"{}\"]\nwhy = [\"{why}\"]\nused = [\"{}\"]\n",
                stamp(since),
                &used[..10]
            )
        };
        let why = if home == THIS_MAC { "fill" } else { "pin" };
        let mut text = String::from("version = 1\n");
        text.push_str(&account(ME_EMAIL, home, why, 40 * D));
        for (label, _) in self.rows.iter().filter(|row| row.0 != self.own()) {
            let email = self.email_of(label).unwrap_or_default();
            text.push_str(&account(&email, THIS_MAC, "fill", 30 * D));
        }
        Some(text)
    }

    /// The own login's seen figures.
    fn own_seen(&self) -> Option<Seen> {
        let own = self.own();
        self.seen
            .iter()
            .find(|(label, _)| *label == own)
            .map(|(_, seen)| *seen)
    }

    /// The `rate_limits` of the fixture's statusLine payload where it runs the tap
    /// ([`crate::fleet_fixture::RATE_LIMITS`]): the own login's seen windows at `now`, as Claude
    /// Code hands them; `None` where it runs none or the own login was never read (case (d): the
    /// tap then prints `week · no figure yet`).
    fn rate_limits(&self, now: i64) -> Option<String> {
        let seen = self.own_seen().filter(|_| self.tapped)?;
        let window = |w: Option<(u32, i64, i64)>| {
            w.map(|(pct_x10, reset, _)| {
                aterm_json::json!({
                    "used_percentage": f64::from(pct_x10) / 10.0,
                    "resets_at": now + reset
                })
            })
        };
        let mut limits = aterm_json::Map::new();
        for (name, w) in [("five_hour", seen.five), ("seven_day", seen.week)] {
            if let Some(w) = window(w) {
                limits.insert(name.to_owned(), w);
            }
        }
        (!limits.is_empty())
            .then(|| aterm_json::to_string(&aterm_json::Value::Object(limits)).ok())
            .flatten()
    }
}

/// This Mac's own login's address: synthetic.
const ME_EMAIL: &str = "me@example.test";

/// `me`'s week resets Thu 09:00 when now is Sun 19:30; its five hours at 22:00.
const ME_WEEK: i64 = 3 * D + 13 * H + 30 * M;
const ME_FIVE: i64 = 2 * H + 30 * M;

/// The plan rows of the scenes A to H: `me`'s read an hour ago where its profile was, the alts'
/// on their own sessions.
const ME_PLANS: &[(&str, &str, i64)] = &[
    ("me", "20x", H),
    ("alt1", "20x", 2 * D),
    ("alt2", "5x", 6 * D),
];

/// The glass scenes' plan rows: every account Max 20x, read an hour ago (aterm-agent's
/// `harness::quota_scenes`).
const MAIN_PLANS: &[(&str, &str, i64)] =
    &[("main", "20x", H), ("alt1", "20x", H), ("alt2", "20x", H)];

/// Case (b)'s and (c)'s `main` week resets 41 hours on (Sat 10:00 when now is Thu 17:00), (c)'s
/// `alt1` 3 d 16 h on (Mon 09:00) and its `alt2` 5 d 16 h on (aterm-agent's
/// `harness::quota_scenes::WED9`, which keeps the design's 36% a day).
const MAIN_WEEK: i64 = 41 * H;
const ALT1_WEEK: i64 = 3 * D + 16 * H;
const ALT2_WEEK: i64 = 5 * D + 16 * H;

/// The glass scenes' five hours: `main`'s 20% used, resetting in 2 h 30 min (never printed).
const MAIN_FIVE: (u32, i64, i64) = (200, 2 * H + 30 * M, 0);

/// Scene `scene`'s seeds, the appendix of the design moved to now.
#[must_use]
pub fn plan(scene: char) -> Plan {
    let me = |week: u32, five: u32, read: i64| Seen {
        week: Some((week, ME_WEEK, read)),
        five: Some((five, ME_FIVE, read)),
    };
    let week = |pct: u32, reset: i64, read: i64| Seen {
        week: Some((pct, reset, read)),
        five: None,
    };
    let alt1 = |pct: u32| week(pct, 4 * D + 18 * H + 30 * M, -3 * H);
    let pace = vec![(-24 * H, 420), (-16 * H, 473), (-8 * H, 527), (0, 580)];
    let two = vec![("me", Class::Preferred), ("alt1", Class::Preferred)];
    match scene {
        'A' | 'G' => {
            let aged = if scene == 'G' { 16 * M } else { 0 };
            Plan {
                rows: vec![
                    ("me", Class::Preferred),
                    ("alt1", Class::Preferred),
                    ("alt2", Class::Preferred),
                    ("pool1", Class::Pool),
                ],
                seen: vec![
                    ("me", me(580, 380, -aged)),
                    ("alt1", alt1(310)),
                    ("alt2", week(710, -(3 * D + 8 * H + 30 * M), -6 * D)),
                    ("pool1", week(400, 5 * D + 12 * H + 30 * M, -5 * H)),
                ],
                series: pace,
                profile: Some((58, 38, aged)),
                ..untapped()
            }
        }
        'B' => Plan {
            rows: two,
            seen: vec![("me", me(580, 380, 0)), ("alt1", alt1(920))],
            series: pace,
            profile: Some((58, 38, 0)),
            ..untapped()
        },
        'C' => Plan {
            rows: vec![
                ("me", Class::Preferred),
                ("alt1", Class::Preferred),
                ("pool1", Class::Pool),
            ],
            seen: vec![("me", me(580, 380, 0)), ("alt1", alt1(920))],
            series: pace,
            profile: Some((58, 38, 0)),
            ..untapped()
        },
        'D' => Plan {
            rows: two,
            seen: vec![
                ("me", me(580, 810, 0)),
                (
                    "alt1",
                    Seen {
                        five: Some((950, H + 40 * M, -20 * M)),
                        ..alt1(310)
                    },
                ),
            ],
            series: pace,
            profile: Some((58, 81, 0)),
            ..untapped()
        },
        'E' => Plan {
            rows: two,
            seen: vec![
                (
                    "me",
                    Seen {
                        week: Some((30, 6 * D + 22 * H + 40 * M, 0)),
                        five: Some((380, ME_FIVE, 0)),
                    },
                ),
                ("alt1", alt1(310)),
            ],
            series: vec![(-40 * M, 20), (0, 30)],
            profile: Some((3, 38, 0)),
            ..untapped()
        },
        'F' => Plan {
            rows: two,
            seen: vec![("me", me(1000, 380, 0)), ("alt1", alt1(310))],
            series: pace,
            profile: Some((100, 38, 0)),
            ..untapped()
        },
        'H' => Plan {
            rows: two,
            seen: vec![("alt1", alt1(310))],
            series: Vec::new(),
            profile: None,
            ..untapped()
        },
        'I' => Plan {
            rows: vec![("main", Class::Preferred)],
            seen: vec![(
                "main",
                Seen {
                    week: Some((630, MAIN_WEEK, 0)),
                    five: Some(MAIN_FIVE),
                },
            )],
            series: vec![(-24 * H, 420), (-18 * H, 500), (-10 * H, 500), (0, 630)],
            profile: Some((63, 20, 0)),
            ..tapped()
        },
        'J' => Plan {
            rows: vec![
                ("main", Class::Preferred),
                ("alt1", Class::Preferred),
                ("alt2", Class::Preferred),
            ],
            seen: vec![
                (
                    "main",
                    Seen {
                        week: Some((750, MAIN_WEEK, 0)),
                        five: Some(MAIN_FIVE),
                    },
                ),
                ("alt1", week(800, ALT1_WEEK, 0)),
                ("alt2", week(900, ALT2_WEEK, 0)),
            ],
            series: vec![(-24 * H, 390), (-12 * H, 570), (0, 750)],
            profile: Some((75, 20, 0)),
            ..tapped()
        },
        'L' => Plan {
            rows: vec![
                ("alt1", Class::Preferred),
                ("alt2", Class::Preferred),
                ("pool1", Class::Pool),
            ],
            seen: vec![("alt1", alt1(120))],
            profile: Some((16, 3, 0)),
            outside: true,
            home: Some("m15"),
            vault: Some(Vault::Empty),
            ..untapped()
        },
        'M' => Plan {
            rows: vec![("me", Class::Preferred), ("alt1", Class::Preferred)],
            seen: vec![("me", me(390, 600, 0)), ("alt1", alt1(120))],
            series: week_of_work(),
            profile: Some((39, 60, 0)),
            home: Some(THIS_MAC),
            vault: Some(Vault::Token),
            ..untapped()
        },
        _ => Plan {
            rows: vec![("main", Class::Preferred)],
            seen: Vec::new(),
            series: Vec::new(),
            profile: None,
            ..tapped()
        },
    }
}

/// Scene M's series: `me`'s week as eight days of work read it, a reading every two hours of
/// each working day. The week before ran to 97% used, 3 d 10 h 30 min ago it began again
/// (`me`'s week resets [`ME_WEEK`] on, so it began a week before that), and this one stands at
/// 39% now.
fn week_of_work() -> Vec<(i64, u32)> {
    let began = ME_WEEK - 7 * D;
    let mut out = Vec::new();
    // The hours of the eight days back, oldest first.
    for ago in (0..=8 * 24).rev().step_by(2) {
        let at = -(ago * H);
        // Nothing is read overnight: eight of every twenty-four hours.
        if (ago / 2) % 12 >= 8 {
            continue;
        }
        let pct_x10 = if at < began {
            // The week before: from 55% eight days back to 97% at its end.
            970 - u32::try_from((began - at) * 420 / (8 * D + began)).unwrap_or(0)
        } else {
            390 - u32::try_from(-at * 390 / -began).unwrap_or(0)
        };
        out.push((at, pct_x10));
    }
    out
}

/// The rest of a plan of the scenes A to H: `me`'s plan rows, no statusLine, Autopilot off.
fn untapped() -> Plan {
    Plan {
        rows: Vec::new(),
        seen: Vec::new(),
        series: Vec::new(),
        profile: None,
        plans: ME_PLANS,
        tapped: false,
        read_here: false,
        autopilot: "off",
        outside: false,
        home: None,
        vault: None,
    }
}

/// The rest of a plan of the glass scenes I to K: `main`'s plan rows, the usage tap as the
/// fixture's statusLine, every reading this Mac's, Autopilot Cruise (the design's §1 is under
/// Cruise).
fn tapped() -> Plan {
    Plan {
        plans: MAIN_PLANS,
        tapped: true,
        read_here: true,
        autopilot: "cruise",
        ..untapped()
    }
}

/// The harness state an instance in `world` keeps its accounts store under.
fn harness_state(world: &World) -> PathBuf {
    world
        .state()
        .join("harness")
        .join(aterm_agent::harness::cli::HARNESS)
}

/// The synthetic Claude Code profile of the own login: its account, its Max 20x plan and, where
/// `plan` read them, its cached figures, resetting where the store's own readings say (`me`'s
/// [`ME_WEEK`] and [`ME_FIVE`] where it has none).
fn profile(plan: &Plan, now: i64) -> String {
    let seen = plan.own_seen().unwrap_or_default();
    let week_reset = seen.week.map_or(ME_WEEK, |(_, reset, _)| reset);
    let five_reset = seen.five.map_or(ME_FIVE, |(_, reset, _)| reset);
    let account = aterm_json::json!({
        "accountUuid": "outlook-fixture-me",
        "emailAddress": ME_EMAIL,
        "organizationType": "claude_max",
        "userRateLimitTier": "default_claude_max_20x",
        "planDisplayName": "Max 20x"
    });
    let body = match plan.profile {
        Some((week, five, age)) => aterm_json::json!({
            "oauthAccount": account,
            "cachedUsageUtilization": {
                "fetchedAtMs": (now - age) * 1000,
                "accountUuid": "outlook-fixture-me",
                "utilization": {
                    "seven_day": {
                        "utilization": week,
                        "resets_at": crate::util::utc_stamp(now + week_reset)
                    },
                    "five_hour": {
                        "utilization": five,
                        "resets_at": crate::util::utc_stamp(now + five_reset)
                    }
                }
            }
        }),
        None => aterm_json::json!({ "oauthAccount": account }),
    };
    aterm_json::to_string(&body).unwrap_or_default()
}

/// Seed `world` for `plan` at `now`: the profile in its HOME, the roster beside its
/// `aterm.toml`, a placeholder file in each other login's folder, the store's readings with the
/// default home's row active, the own login's series (and, where every
/// reading is this Mac's, each other login's week as its series row), the plan rows, where the
/// fixture runs the tap its statusLine payload's `rate_limits`, and where the scene has a pool
/// file that file and this Mac's lab name in the store. Nothing outside `world` is written.
/// # Errors
/// A file could not be written, or the store refused a reading.
pub fn seed(world: &World, plan: &Plan, now: i64) -> Result<(), String> {
    let io = |what: &str, e: std::io::Error| format!("outlook seed: {what}: {e}");
    let own = plan.own();
    crate::util::write_fresh(&world.home.join(".claude.json"), profile(plan, now))
        .map_err(|e| io("profile", e))?;
    if let Some(limits) = plan.rate_limits(now) {
        let path = world.home.join(crate::fleet_fixture::RATE_LIMITS);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io("rate limits", e))?;
        }
        crate::util::write_fresh(&path, limits).map_err(|e| io("rate limits", e))?;
    }
    let mut rows = Vec::new();
    for &(label, class) in &plan.rows {
        let dir = (label != plan.default_row()).then(|| world.home.join("logins").join(label));
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).map_err(|e| io(label, e))?;
            crate::util::write_fresh(&dir.join("a-login-file"), "{}").map_err(|e| io(label, e))?;
        }
        rows.push(accounts::Row {
            label: label.into(),
            class,
            dir: dir.map(|d| d.to_string_lossy().into_owned()),
            // The own login names the profile's address, so the footer's gate reads its profile
            // under a roster of several rows.
            email: plan.email_of(label),
            ..accounts::Row::default()
        });
    }
    let roster = accounts::append_rows("", &rows)?;
    crate::util::write_fresh(&world.cfg.join("aterm").join("accounts.toml"), roster)
        .map_err(|e| io("roster", e))?;
    let state_dir = harness_state(world);
    std::fs::create_dir_all(&state_dir).map_err(|e| io("state", e))?;
    let store = accounts::Store::at(&state_dir);
    let (mut state, _) = store.load(now)?;
    // The store believes the default home runs its row, whatever login the profile names.
    state.active = Some(plan.default_row().into());
    if let Some(text) = plan.pool_file(now) {
        // The pool file names Macs by their lab names; the store says which is this one.
        state.swarm_label = Some(THIS_MAC.into());
        let secrets = world.home.join(".secrets");
        crate::util::create_private_dir_all(&secrets).map_err(|e| io("pool file", e))?;
        crate::util::write_fresh(
            &secrets.join(aterm_agent::harness::ai_pool::AI_POOL_FILE),
            text,
        )
        .map_err(|e| io("pool file", e))?;
    }
    let window = |seen: Option<(u32, i64, i64)>| {
        seen.map(|(pct_x10, reset, _)| WindowRead {
            pct_x10,
            reset: now + reset,
            refilled: None,
        })
    };
    for (label, seen) in &plan.seen {
        let at = now + seen.week.or(seen.five).map_or(0, |(_, _, read)| read);
        let read = TapRead {
            five_hour: window(seen.five),
            seven_day: window(seen.week),
            at,
            seen_before: true,
            born: None,
            key: None,
        };
        let _ = accounts::note_reading_held(
            &mut state,
            label,
            &read,
            at,
            accounts::ReadingOwner::Unobserved,
        );
    }
    store.save(&state).map_err(|e| io("store", e))?;
    for &(offset, pct_x10) in &plan.series {
        let at = now + offset;
        let mut reset = plan
            .own_seen()
            .and_then(|s| s.week)
            .map_or(now + ME_WEEK, |(_, reset, _)| now + reset);
        // A reading from before this week began is of the week that reset then.
        while at < reset - 7 * D {
            reset -= 7 * D;
        }
        let fig = WindowFig {
            pct_x10,
            reset,
            at,
            by: state.machine.clone(),
            source: accounts::SOURCE_TAP.into(),
            epoch_at: None,
        };
        account_series::note(&state_dir, own, &state.machine, None, Some(&fig), at)
            .map_err(|e| io("series", e))?;
    }
    // Where every reading is this Mac's, each other login's week is its series row too.
    for (label, seen) in plan
        .seen
        .iter()
        .filter(|(l, _)| plan.read_here && *l != own)
    {
        if let Some((pct_x10, reset, read)) = seen.week {
            let fig = WindowFig {
                pct_x10,
                reset: now + reset,
                at: now + read,
                by: state.machine.clone(),
                source: accounts::SOURCE_TAP.into(),
                epoch_at: None,
            };
            account_series::note(
                &state_dir,
                label,
                &state.machine,
                None,
                Some(&fig),
                now + read,
            )
            .map_err(|e| io("series", e))?;
        }
    }
    // The plans, as earlier footers' gated profile reads wrote them (the own login's only where
    // its profile was read); `pool1`'s was never seen. The instance's own read of the own login
    // writes nothing new for the same tier.
    for &(label, tier, ago) in plan.plans {
        let read = label != own || plan.profile.is_some();
        if read && plan.rows.iter().any(|(l, _)| *l == label) {
            let said = account_series::PlanSaid {
                word: "max".into(),
                tier: Some(tier.into()),
                src: "profile".into(),
            };
            account_series::note_plan(&state_dir, label, &state.machine, &said, now - ago)
                .map_err(|e| io("plan", e))?;
        }
    }
    Ok(())
}

/// One shot the manifest lists.
struct Row {
    scene: char,
    what: String,
    look: Look,
    scale: u8,
    png: PathBuf,
    note: String,
}

/// What `outlook-captures` was asked for.
struct Opts {
    aterm: Option<String>,
    out: PathBuf,
    tag: String,
    scenes: Vec<char>,
    looks: Vec<Look>,
    scales: Vec<u8>,
}

const HELP: &str =
    "usage: targo --unverified lab outlook-captures --out DIR [--aterm ATERM] [--tag NAME]
       [--scenes ABCDEFGHIJKLM] [--look day|night|both] [--scale 1|2|both]

The accounts outlook on a private headless instance per scene, look and scale, seeded with
synthetic identities only: the footer at 170, 100 and 70 columns, and the A, B, C, E, H, J, L
and M scenes' Account card at 60 and 44 cells. Scenes I, J and K are the harness glass's line E
(its cases (b), (c) and (d), under Cruise): the fixture runs the build's usage tap as its
statusLine and prints its row under the bottom rule, which the glass takes over. J's card is the
one whose tab aterm can switch: its Forecast row holds the projection. L and M are the card's
own: a pool file gives each account its home Mac and a scratch vault served on loopback seats
the instance's home; L's tab runs on a login outside the roster whose home is another Mac and
which has no vault token, M's on a pool account at home here with a verified lab token. Writes
outlook-<tag>-<scene>-<width>-<look>-<scale>x.png, the card's rows beside each card png, and
manifest-<tag>.tsv.
";

fn opts(args: &[String]) -> Result<Opts, Fail> {
    let mut a = args.to_vec();
    if a.iter().any(|w| w == "-h" || w == "--help") {
        return Err(Fail::usage(HELP));
    }
    let aterm = take_value(&mut a, "--aterm")?;
    let out = take_value(&mut a, "--out")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-captures: --out DIR is required"))?;
    let tag = take_value(&mut a, "--tag")?.unwrap_or_else(|| "branch".into());
    let scenes: Vec<char> =
        take_value(&mut a, "--scenes")?.map_or_else(|| SCENES.to_vec(), |s| s.chars().collect());
    if let Some(bad) = scenes.iter().find(|c| !SCENES.contains(c)) {
        return Err(Fail::usage(format!("outlook-captures: no scene {bad}")));
    }
    let looks = match take_value(&mut a, "--look")?.as_deref() {
        None | Some("both") => Look::BOTH.to_vec(),
        Some("day") => vec![Look::Day],
        Some("night") => vec![Look::Night],
        Some(other) => return Err(Fail::usage(format!("outlook-captures: no look {other}"))),
    };
    let scales = match take_value(&mut a, "--scale")?.as_deref() {
        None | Some("both") => vec![1, 2],
        Some("1") => vec![1],
        Some("2") => vec![2],
        Some(other) => return Err(Fail::usage(format!("outlook-captures: no scale {other}"))),
    };
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "outlook-captures: unexpected '{extra}'"
        )));
    }
    Ok(Opts {
        aterm,
        out,
        tag,
        scenes,
        looks,
        scales,
    })
}

/// `outlook-captures`: see [`HELP`].
/// # Errors
/// Usage, or an instance that would not start or shoot.
pub fn cmd_outlook_captures(args: &[String]) -> Res {
    crate::stop::install();
    let o = opts(args)?;
    let aterm = crate::instance::resolve_aterm(o.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    std::fs::create_dir_all(&o.out)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", o.out.display())))?;
    let mut rows = Vec::new();
    for &scene in &o.scenes {
        for &look in &o.looks {
            for &scale in &o.scales {
                rows.extend(shoot(&o, &aterm, scene, look, scale)?);
            }
        }
    }
    let mut manifest = String::from("scene\twhat\tlook\tscale\tpng\tnote\n");
    for r in &rows {
        manifest.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            r.scene,
            r.what,
            r.look.name(),
            r.scale,
            r.png.display(),
            r.note
        ));
    }
    let path = o.out.join(format!("manifest-{}.tsv", o.tag));
    crate::util::write_fresh(&path, manifest)
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    println!("outlook-captures: {} shots, {}", rows.len(), path.display());
    Ok(0)
}

/// A seeded world and the private headless instance running in it, the instance first so it
/// stops before its world is removed.
struct Lab {
    inst: Instance,
    world: World,
    /// The fixture runs the usage tap as its statusLine ([`Plan::tapped`]).
    tapped: bool,
    /// The scratch vault's server, where the scene has a vault: it runs while the instance
    /// does, which reads the vault through it.
    _vault: Option<aterm_secrets::server::Running>,
}

/// How long a vault write may take: a hang detector, not a budget.
#[cfg(debug_assertions)]
const VAULT: Duration = Duration::from_secs(60);

/// Seat `world`'s home in a scratch vault this process serves ([`Plan::vault`]) and, where the
/// scene holds a token, write the tab's account a lab token there and verify it as the Mac
/// that made it: the account keyed as the harness keys it, the token the stub's labelled one
/// (no credential anywhere), its end a year on.
/// A debug lab image only, as `setup-token-lab` is: the scratch vault and the account's key are
/// made through seams no release build has.
/// # Errors
/// The vault could not be made or seated, the account could not be keyed, or a write was
/// refused.
#[cfg(debug_assertions)]
fn seat(world: &World, holds: Vault, now: i64) -> Result<aterm_secrets::server::Running, Fail> {
    use aterm_secrets::claude_token::{self, ExpiryFrom, Key, Meta, Verified};
    use aterm_secrets::client::{MemberWritten, OnBusy};
    let die = |what: &str, why: &dyn std::fmt::Display| {
        Fail::die(format!("outlook-captures: {what}: {why}"))
    };
    let (vault, server) =
        crate::setup_token_lab::seat_scratch_vault(&world.dir.join("vault-server"), &world.home)
            .map_err(|why| die("the scratch vault", &why))?;
    if holds == Vault::Empty {
        return Ok(server);
    }
    let key = aterm_agent::harness::journal::with_keyer(
        |realm, email| aterm_db::typed::keys::account_pk(realm, email).ok(),
        || aterm_agent::harness::journal::claude_email_key(ME_EMAIL),
    )
    .and_then(|key| Key::parse(key.as_str()))
    .ok_or_else(|| die("the token", &"the account cannot be keyed"))?;
    let at = u64::try_from(now).unwrap_or(0);
    let here = claude_token::seat_here(&vault)
        .ok()
        .flatten()
        .unwrap_or_else(|| "lab-mac".to_owned());
    let meta = Meta {
        email: ME_EMAIL.into(),
        account_id: String::new(),
        minted_at: at.saturating_sub(20 * 86_400),
        expires_at: at + 345 * 86_400,
        expiry_from: ExpiryFrom::Printed,
        minted_on: here.clone(),
        cli: "2.1.318".into(),
        verified: None,
    };
    let wrote = |written: aterm_secrets::Result<MemberWritten>, what: &str| match written {
        Ok(MemberWritten::Refused(why)) => Err(die(what, &why)),
        Ok(MemberWritten::Busy) => Err(die(what, &"the vault is busy")),
        Ok(_) => Ok(()),
        Err(why) => Err(die(what, &why)),
    };
    let busy = || OnBusy::Refuse(Instant::now() + VAULT);
    wrote(
        claude_token::write(
            &vault,
            &key,
            &aterm_secrets::SecretBytes::from(crate::stub::labelled_oauth_token("capture")),
            &meta,
            busy(),
        ),
        "the token's write",
    )?;
    wrote(
        claude_token::verify(
            &vault,
            &key,
            Verified {
                on: here,
                at: at.saturating_sub(19 * 86_400),
                by: "windows".into(),
                seven_day_resets_at: None,
                five_hour_resets_at: None,
            },
            busy(),
        ),
        "the token's verification",
    )?;
    Ok(server)
}

/// A release lab image has no vault seam, so it shoots no scene that has a vault (L, M).
/// # Errors
/// Always: the scene needs a debug lab image.
#[cfg(not(debug_assertions))]
fn seat(_: &World, _: Vault, _: i64) -> Result<aterm_secrets::server::Running, Fail> {
    Err(Fail::die(
        "outlook-captures: scenes L and M need a debug lab image for the vault seam",
    ))
}

/// The `--settings` value that names `aterm`'s usage tap as the fixture's statusLine, exactly as
/// a relaunch line aterm types names it ([`tap::settings_value`]), so the window reads the
/// fixture as tapped: `aterm` by its canonical path, or by a link in `world` where that path
/// cannot be named in the command.
fn tap_settings(aterm: &Path, world: &World) -> Result<String, Fail> {
    let exe = std::fs::canonicalize(aterm).unwrap_or_else(|_| aterm.to_path_buf());
    let exe = if tap::exe_admissible(&exe) {
        exe
    } else {
        let link = world.dir.join("aterm-tap");
        std::os::unix::fs::symlink(&exe, &link).map_err(|e| Fail::die(format!("tap link: {e}")))?;
        link
    };
    tap::settings_value(&exe).ok_or_else(|| {
        Fail::die(format!(
            "outlook-captures: {} cannot be named in a statusLine command",
            exe.display()
        ))
    })
}

/// Seed a world for `scene` in `look` and start `aterm` in it at `scale`, its one tab the lab's
/// `claude` fixture at `cols` columns, under the scene's Autopilot mode, its `--settings` naming
/// `aterm`'s usage tap where the scene runs it.
fn launch(aterm: &Path, scene: char, look: Look, scale: u8, cols: u16) -> Result<Lab, Fail> {
    let plan = plan(scene);
    let config = config_after(
        &config_with(&[format!("theme = {:?}", look.theme())]),
        true,
        &[],
    );
    let config = with_harness_keys(&config, &[format!("autopilot = {:?}", plan.autopilot)]);
    let world = World::new(&config)?;
    let now = crate::util::now_secs();
    // The vault first: it makes the home's `.secrets`, which the pool file then joins.
    let vault = plan
        .vault
        .map(|holds| seat(&world, holds, now))
        .transpose()?;
    seed(&world, &plan, now).map_err(Fail::die)?;
    let bin = world.dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| Fail::die(format!("bin: {e}")))?;
    let claude = bin.join("claude");
    std::os::unix::fs::symlink(&crate::paths::paths().exe, &claude)
        .map_err(|e| Fail::die(format!("claude link: {e}")))?;
    let mut args = vec![
        "--columns".into(),
        cols.to_string(),
        "--lines".into(),
        LINES.to_string(),
        "--scale".into(),
        scale.to_string(),
        "-e".into(),
        claude.to_string_lossy().into_owned(),
        "idle".into(),
    ];
    if plan.tapped {
        args.extend(["--settings".into(), tap_settings(aterm, &world)?]);
    }
    let mut inst = Instance::start_in(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: true,
            args,
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        Band::Agent,
    )?;
    inst.await_socket(BOOT)?;
    Ok(Lab {
        inst,
        world,
        tapped: plan.tapped,
        _vault: vault,
    })
}

/// One scene in one look and scale: its instance, its footer at each width, its card.
fn shoot(o: &Opts, aterm: &Path, scene: char, look: Look, scale: u8) -> Result<Vec<Row>, Fail> {
    let (_, first) = WIDTHS[0];
    let mut lab = launch(aterm, scene, look, scale, first)?;
    let inst = &lab.inst;
    let mut rows = Vec::new();
    let name = |what: &str| {
        o.out.join(format!(
            "outlook-{}-{scene}-{what}-{}-{scale}x.png",
            o.tag,
            look.name()
        ))
    };
    let scratch = o.out.join(format!(".outlook-{}-{scene}-frame.png", o.tag));
    let rl = lab
        .tapped
        .then(|| tap::rl_dir_in(&harness_state(&lab.world)));
    settle(inst, rl.as_deref())?;
    let mut at = first;
    for (what, cols) in WIDTHS {
        resize(inst, (at, cols), &scratch)?;
        at = cols;
        let png = name(what);
        let note = image(inst, &png)?;
        rows.push(Row {
            scene,
            what: what.into(),
            look,
            scale,
            png,
            note,
        });
    }
    if CARD_SCENES.contains(&scene) {
        for (what, cols) in CARD_WIDTHS {
            resize(inst, (at, cols), &scratch)?;
            at = cols;
            let png = name(what);
            let (card, note) = shoot_card(inst, &scratch, &png)?;
            let rows_file = png.with_extension("controls.txt");
            crate::util::write_fresh(&rows_file, &card)
                .map_err(|e| Fail::die(format!("cannot write {}: {e}", rows_file.display())))?;
            let _ = inst.ctl(&["key", "escape"]);
            rows.push(Row {
                scene,
                what: what.into(),
                look,
                scale,
                png,
                note,
            });
        }
    }
    let _ = std::fs::remove_file(&scratch);
    lab.inst.stop().map_err(Fail::die)?;
    Ok(rows)
}

/// A frame of the window: its width in pixels and its pixels.
fn frame(inst: &Instance, scratch: &Path) -> Result<(usize, Vec<u8>), Fail> {
    image(inst, scratch)?;
    let bytes = crate::util::read_regular_file(scratch)
        .map_err(|e| Fail::die(format!("cannot read {}: {e}", scratch.display())))?;
    let (rgba, w, _) = aterm_png::decode_rgba8(&bytes, 1 << 15)
        .ok_or_else(|| Fail::die(format!("{}: not a png", scratch.display())))?;
    Ok((w, rgba))
}

/// Wait until the window draws a steady frame: two frames in a row alike, 300 ms apart, the
/// first of them `fresh` (a frame drawn after the change waited on). A one-minute hang
/// detector ([`SHOWN`]), never a budget: the fixture repaints the same screen and a headless
/// window never animates, so a frame that will not hold still is a failure.
/// # Errors
/// No steady frame within [`SHOWN`], or the lab was stopped.
fn steady(
    inst: &Instance,
    scratch: &Path,
    what: &str,
    fresh: impl Fn(usize) -> bool,
) -> Result<(), Fail> {
    let end = Instant::now() + SHOWN;
    let mut last: Option<Vec<u8>> = None;
    while Instant::now() < end {
        let (w, px) = frame(inst, scratch)?;
        if !fresh(w) {
            last = None;
        } else if last.as_ref() == Some(&px) {
            return Ok(());
        } else {
            last = Some(px);
        }
        crate::stop::sleep(Duration::from_millis(300))?;
    }
    Err(Fail::die(format!(
        "outlook-captures: {what}: no steady frame within {} s",
        SHOWN.as_secs()
    )))
}

/// Resize the pane from `from` to `cols` columns: the verb must take it, and the window must
/// draw it there (a frame of another width where the width moves) and hold still.
/// # Errors
/// A refused resize, no steady frame at the new width, or the lab was stopped.
fn resize(inst: &Instance, (from, cols): (u16, u16), scratch: &Path) -> Result<(), Fail> {
    let (before, _) = frame(inst, scratch)?;
    let said = inst.ctl(&["resize", &LINES.to_string(), &cols.to_string()]);
    if said.code != 0 {
        return Err(Fail::die(format!(
            "outlook-captures: resize to {cols}: {}",
            said.first()
        )));
    }
    steady(inst, scratch, &format!("resize to {cols}"), |w| {
        from == cols || w != before
    })
}

/// A row of the card as `controls fact-menu` words it, of the kinds a capture reads.
#[derive(Debug, PartialEq, Eq)]
enum CardLine {
    /// A section's heading: its title.
    Section(String),
    /// A named fact: its name and its whole value, the rows it wraps over joined by a blank.
    Fact(String, String),
}

/// The Rust-quoted string after `key=` in `line` (`text="a \"b\""`), unquoted. `None` where
/// the line has no such field or its string does not close.
fn quoted(line: &str, key: &str) -> Option<String> {
    let from = line.find(&format!(" {key}=\""))? + key.len() + 3;
    let mut out = String::new();
    let mut chars = line[from..].chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
}

/// The sections and facts of `card`, the words of `controls fact-menu`, in the card's order. A
/// fact's value that wraps is one fact here: a `fact` row with no name directly under a `fact`
/// row continues it, and any other row (a `section` among them) ends it.
fn card_lines(card: &str) -> Vec<CardLine> {
    let mut out: Vec<CardLine> = Vec::new();
    let mut open = false;
    for line in card.lines() {
        let kind = line
            .strip_prefix("fact-menu ")
            .and_then(|rest| rest.split(' ').next());
        match kind {
            Some("section") => {
                out.extend(quoted(line, "text").map(CardLine::Section));
                open = false;
            }
            Some("fact") => {
                let (Some(label), Some(text)) = (quoted(line, "label"), quoted(line, "text"))
                else {
                    open = false;
                    continue;
                };
                match out.last_mut() {
                    Some(CardLine::Fact(_, value)) if open && label.is_empty() => {
                        value.push(' ');
                        value.push_str(&text);
                    }
                    _ => out.push(CardLine::Fact(label, text)),
                }
                open = true;
            }
            _ => open = false,
        }
    }
    out
}

/// Whether `card`, the words of `controls fact-menu`, says the Account card is open on the
/// account with its worker's answers: its `Token`, `Budget` and `Machine` facts are there, and
/// `Machine` no longer says `not read yet`, which it says until the account reader's card
/// arrives. Not while the pane's recovery observer is busy, a moment (until its next read, a
/// second on) in which the `Forecast` fact says there is none for that reason
/// ([`outlook::Refusal::ObserverBusy`]), which a capture of a scene's card is not.
fn card_shown(card: &str) -> bool {
    let head = card.lines().next().unwrap_or_default();
    let lines = card_lines(card);
    let fact = |name: &str| {
        lines.iter().find_map(|line| match line {
            CardLine::Fact(label, value) if label == name => Some(value.as_str()),
            _ => None,
        })
    };
    head.contains("open=true")
        && head.contains("fact=account")
        && fact("Token").is_some()
        && fact("Budget").is_some()
        && fact("Machine").is_some_and(|said| said != "not read yet")
        && !fact("Forecast")
            .is_some_and(|said| said.contains(&outlook::Refusal::ObserverBusy.words()))
}

/// Open the Account card and shoot it as `png` once it is drawn: `controls fact-menu` says it is
/// shown ([`card_shown`]), the frame holds still, and its rows are the same after the shot as
/// before it, so the png and the rows beside it are one card. Its controls text and the image
/// verb's verdict line. [`SHOWN`] is a hang detector.
/// # Errors
/// A refused open or shot, a card never shown and held still within [`SHOWN`], or the lab was
/// stopped.
fn shoot_card(inst: &Instance, scratch: &Path, png: &Path) -> Result<(String, String), Fail> {
    let opened = inst.ctl(&["open", "fact-menu", "account"]);
    if opened.code != 0 {
        return Err(Fail::die(format!(
            "outlook-captures: open the card: {}",
            opened.first()
        )));
    }
    let end = Instant::now() + SHOWN;
    loop {
        let card = inst.ctl(&["controls", "fact-menu"]).stdout;
        if card_shown(&card) {
            steady(inst, scratch, "the card", |_| true)?;
            let before = inst.ctl(&["controls", "fact-menu"]).stdout;
            let note = image(inst, png)?;
            if card_shown(&before) && inst.ctl(&["controls", "fact-menu"]).stdout == before {
                return Ok((before, note));
            }
        }
        if Instant::now() >= end {
            let head = card.lines().next().unwrap_or_default();
            return Err(Fail::die(format!(
                "outlook-captures: the card was not shown and held still within {} s: {head}",
                SHOWN.as_secs()
            )));
        }
        crate::stop::sleep(Duration::from_millis(300))?;
    }
}

/// Wait until the window knows the fixture as Claude Code (`status program=claude`) and, where
/// the fixture runs the usage tap, until the tap has written a reading of its own row into `rl`
/// (the glass's claim reads it); then for the footer's resolver to look: it looks every 10 s and
/// its memo replays at its first look, so the frames after 12 s hold the account unit, or line
/// E's quota row where the tap's row is claimed. [`SHOWN`] is a hang detector.
/// # Errors
/// The fixture never showed as Claude Code, or its tap wrote no reading, within [`SHOWN`], or
/// the lab was stopped.
fn settle(inst: &Instance, rl: Option<&Path>) -> Result<(), Fail> {
    let end = Instant::now() + SHOWN;
    while Instant::now() < end {
        let status = inst.ctl(&["status"]);
        if status.stdout.contains("program=claude") && rl.is_none_or(read_own_row) {
            crate::stop::sleep(Duration::from_secs(12))?;
            return Ok(());
        }
        crate::stop::sleep(Duration::from_millis(500))?;
    }
    Err(Fail::die(format!(
        "outlook-captures: the fixture never showed as Claude Code{} within {} s",
        if rl.is_some() {
            ", its usage tap reading its own row,"
        } else {
            ""
        },
        SHOWN.as_secs()
    )))
}

/// Whether a reading in the usage tap's directory `rl` says Claude Code shows the tap's own row
/// ([`tap::Reading::claimable`]).
fn read_own_row(rl: &Path) -> bool {
    std::fs::read_dir(rl).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            crate::util::read_regular_text(&entry.path())
                .ok()
                .and_then(|text| tap::Reading::of_json(&text))
                .is_some_and(|reading| reading.claimable())
        })
    })
}

/// Take the frame as `png`: the verb's verdict line.
fn image(inst: &Instance, png: &Path) -> Result<String, Fail> {
    let shot = inst.ctl(&["image", "outlook.png"]);
    let Some(path) = shot.image_path() else {
        return Err(Fail::die(format!(
            "outlook-captures: image: {}",
            shot.first()
        )));
    };
    crate::util::copy_regular_file(path, png)
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", png.display())))?;
    Ok(shot.first())
}

/// What `outlook-perf` was asked for.
struct PerfOpts {
    main: PathBuf,
    aterm: Option<String>,
    out: PathBuf,
    pairs: usize,
}

const PERF_HELP: &str =
    "usage: targo --unverified lab outlook-perf --main ATERM --out DIR [--aterm ATERM] [--pairs N]

The outlook's cost against another build on the same seeds (scene A: a 4-account roster, the
footer's unit with its bar and outlook, Aterm Day, 1x, 170x40). Each pair starts both builds'
private headless instances at once and, behind a barrier, types into both at once (200 keys, then
200 backspaces, 50 ms apart, under one `video 60 keys`), then leaves both idle 20 s. Prints and
writes perf.txt: per arm the typing take's `metrics percentiles`, the idle window's timer wakes,
renders, deadline owners and wakes by variant, the 1-minute load, and the resolver's own cost on
the seed, each account's series grown to a full one (the store and logins read, the series read
whole and by its tail, and the replay on this machine's clock), in microseconds.
";

/// The typing take's `metrics percentiles` keys.
const TYPING_KEYS: [&str; 8] = [
    "n_input",
    "input_p50_ms",
    "input_p95_ms",
    "input_p99_ms",
    "n_render",
    "render_p50_ms",
    "render_p95_ms",
    "render_p99_ms",
];

/// The idle window's `metrics` keys.
const IDLE_KEYS: [&str; 5] = [
    "timer_wakes",
    "frames",
    "deadline_arms_by_owner",
    "deadline_fires_by_owner",
    "wakes_by_variant",
];

/// The idle window.
const IDLE: Duration = Duration::from_secs(20);

fn perf_opts(args: &[String]) -> Result<PerfOpts, Fail> {
    let mut a = args.to_vec();
    if a.iter().any(|w| w == "-h" || w == "--help") {
        return Err(Fail::usage(PERF_HELP));
    }
    let main = take_value(&mut a, "--main")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-perf: --main ATERM is required"))?;
    let aterm = take_value(&mut a, "--aterm")?;
    let out = take_value(&mut a, "--out")?
        .map(|p| PathBuf::from(crate::paths::from_caller(&p)))
        .ok_or_else(|| Fail::usage("outlook-perf: --out DIR is required"))?;
    let pairs = take_value(&mut a, "--pairs")?
        .map_or(Ok(3), |n| n.parse::<usize>())
        .map_err(|e| Fail::usage(format!("outlook-perf: --pairs: {e}")))?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!("outlook-perf: unexpected '{extra}'")));
    }
    Ok(PerfOpts {
        main,
        aterm,
        out,
        pairs: pairs.max(1),
    })
}

/// `key=value` words of `text` for `keys`, in order, `-` for one it lacks.
fn picked(text: &str, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| {
            let v = crate::forever::word(text, k).unwrap_or_else(|| "-".into());
            format!("{k}={v}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The burst: 200 keys, then 200 backspaces, 50 ms apart.
fn burst(inst: &Instance) -> Result<(), Fail> {
    let keys = "the quick brown fox jumps over the lazy dog ";
    for c in keys.chars().cycle().take(200) {
        if c == ' ' {
            let _ = inst.ctl(&["key", "space"]);
        } else {
            let _ = inst.ctl(&["send", &c.to_string()]);
        }
        crate::stop::sleep(Duration::from_millis(50))?;
    }
    for _ in 0..200 {
        let _ = inst.ctl(&["key", "backspace"]);
        crate::stop::sleep(Duration::from_millis(50))?;
    }
    Ok(())
}

/// One arm of one pair: its instance on scene A, the typing take and the idle window behind
/// `gate` (both arms at once), its two lines.
fn arm(aterm: &Path, gate: &std::sync::Barrier) -> Result<(String, String), Fail> {
    let lab = launch(aterm, 'A', Look::Day, 1, WIDTHS[0].1);
    let ready = lab.as_ref().map_err(|f| f.msg.clone().unwrap_or_default());
    let shown = match &ready {
        Ok(lab) => settle(&lab.inst, None),
        Err(_) => Ok(()),
    };
    gate.wait();
    let lab = lab?;
    shown?;
    let inst = &lab.inst;
    let _ = inst.ctl(&["metrics", "reset"]);
    let video = std::thread::scope(|s| {
        let recording = s.spawn(|| inst.ctl(&["video", "60", "keys"]).first());
        crate::stop::sleep(Duration::from_millis(500))?;
        let typed = burst(inst);
        let pct = inst.ctl(&["metrics", "percentiles"]).stdout;
        let _ = inst.ctl(&["video", "stop"]);
        let said = recording
            .join()
            .unwrap_or_else(|_| "ERR the recording's thread panicked".into());
        typed.map(|()| (pct, said))
    });
    let (pct, said) = video?;
    let typing = format!(
        "typing: {} video={:?}",
        picked(&pct, &TYPING_KEYS),
        crate::util::one_line(&said, 120)
    );
    gate.wait();
    let _ = inst.ctl(&["metrics", "reset"]);
    crate::stop::sleep(IDLE)?;
    let metrics = inst.ctl(&["metrics"]).stdout;
    let pct = inst.ctl(&["metrics", "percentiles"]).stdout;
    let idle = format!(
        "idle {} s: {} {}",
        IDLE.as_secs(),
        picked(&metrics, &IDLE_KEYS),
        picked(&pct, &TYPING_KEYS[4..])
    );
    Ok((typing, idle))
}

/// Nearest-rank p50, p95, p99 and the max of `us`, in microseconds.
fn spread(us: &mut [u128]) -> String {
    us.sort_unstable();
    let at = |q: usize| {
        us[(us.len() * q)
            .div_ceil(100)
            .saturating_sub(1)
            .min(us.len() - 1)]
    };
    format!(
        "p50={} p95={} p99={} max={} n={}",
        at(50),
        at(95),
        at(99),
        us[us.len() - 1],
        us.len()
    )
}

/// Grow each roster row's series under `dir` to a full one, as a compaction leaves a busy
/// account's near [`account_series::BOUND_BYTES`], before the rows the seed wrote: a `day` row a
/// day over the daily tier ([`account_series::HOURLY_S`] to [`account_series::DAILY_S`]), one
/// `fig` row an hour over the hourly tier ([`account_series::RAW_S`] to `HOURLY_S`), and raw
/// `fig` rows over the week before the seed's last day to fill the rest, each in its own week.
fn full_series(
    dir: &Path,
    roster: &accounts::Roster,
    state: &accounts::State,
    now: i64,
) -> Result<(), Fail> {
    use account_series::{BOUND_BYTES, DAILY_S, HOURLY_S, RAW_S};
    for row in &roster.rows {
        let path = account_series::path_of(dir, &row.label, &state.machine)
            .ok_or_else(|| Fail::die(format!("{}: no series path", row.label)))?;
        let reset = state
            .accounts
            .get(&row.label)
            .and_then(|a| a.windows.get(accounts::WEEK_LIMIT))
            .map_or(now + 5 * D, |w| w.reset);
        // The week `t` falls in, and how far into it `t` is, in tenths of a percent.
        let week = |t: i64| {
            let behind = (reset - t).div_euclid(7 * D);
            let window_reset = reset - 7 * D * behind;
            let into = 7 * D - (window_reset - t);
            (
                window_reset,
                u32::try_from(into * 1000 / (7 * D)).unwrap_or(0),
            )
        };
        let print = |said: account_series::Said, t: i64| {
            account_series::SeriesRow {
                t,
                by: state.machine.clone(),
                said,
            }
            .line()
            .ok_or_else(|| Fail::die("a series row that will not print"))
        };
        let fig = |t: i64| {
            let (window_reset, pct_x10) = week(t);
            print(
                account_series::Said::Fig {
                    five_hour: None,
                    seven_day: Some(account_series::Fig {
                        pct_x10,
                        reset: window_reset,
                        at: t,
                        src: accounts::SOURCE_TAP.into(),
                        epoch: None,
                    }),
                    ac: None,
                    key: None,
                },
                t,
            )
        };
        let mut text = String::new();
        let push = |text: &mut String, line: String| {
            text.push_str(&line);
            text.push('\n');
        };
        let mut t = (now - DAILY_S).div_euclid(D) * D + D;
        while t < now - HOURLY_S {
            let (window_reset, first) = week(t);
            let (_, last) = week(t + D - H);
            push(
                &mut text,
                print(
                    account_series::Said::Day(account_series::Day {
                        span_s: D,
                        tz_s: 0,
                        five_hour: None,
                        seven_day: Some(account_series::DayWeek {
                            first,
                            last: last.max(first),
                            reset: window_reset,
                            closed: window_reset < t + D,
                        }),
                        n: 24,
                        ac: None,
                        key: None,
                    }),
                    t,
                )?,
            );
            t += D;
        }
        let mut t = now - HOURLY_S + H;
        while t < now - RAW_S {
            push(&mut text, fig(t)?);
            t += H;
        }
        let seeded = crate::util::read_regular_text(&path).unwrap_or_default();
        let (from, to) = (now - RAW_S + H, now - 25 * H);
        let one = fig(to)?.len() + 1;
        let room = usize::try_from(BOUND_BYTES)
            .unwrap_or(usize::MAX)
            .saturating_sub(text.len() + seeded.len() + 16 * 1024);
        let raw = i64::try_from(room / one).unwrap_or(0).max(1);
        for i in 0..raw {
            push(&mut text, fig(from + (to - from) * i / raw)?);
        }
        text.push_str(&seeded);
        crate::util::write_fresh(&path, text)
            .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(())
}

/// The resolver's own cost on scene A's seed (four accounts), each series grown to a full one
/// ([`full_series`]), as its memo spends it, `n` times each: the store and logins read; the
/// series read whole (what the memo did before it kept a [`account_series::Tail`]) and read as
/// the memo now reads them, a tail's refresh and the rows handed to the replay; and the replay
/// on this machine's own clock (each instant's offset,
/// [`outlook::Offsets::around`](aterm_agent::harness::outlook::Offsets::around)).
fn resolver_cost(n: usize) -> Result<String, Fail> {
    use aterm_agent::harness::outlook;
    let world = World::new(crate::instance::CONFIG_OFF)?;
    let now = crate::util::now_secs();
    seed(&world, &plan('A'), now).map_err(Fail::die)?;
    let roster = accounts::Roster::load(&world.cfg.join("aterm").join("accounts.toml"))
        .map_err(Fail::die)?;
    let dir = harness_state(&world);
    let store = accounts::Store::at(&dir);
    let (state, _) = store.load(now).map_err(Fail::die)?;
    full_series(&dir, &roster, &state, now)?;
    let mut tails: Vec<(String, account_series::Tail)> = roster
        .rows
        .iter()
        .filter_map(|r| {
            let path = account_series::path_of(&dir, &r.label, &state.machine)?;
            let mut tail = account_series::Tail::new(path);
            tail.refresh(now);
            Some((r.label.clone(), tail))
        })
        .collect();
    let rows_held: usize = tails.iter().map(|(_, t)| t.rows().len()).sum();
    let (mut read, mut whole, mut tailed, mut replay) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let (state, _) = store.load(now).map_err(Fail::die)?;
        let enrolled = accounts::enrolled(&roster);
        let renewals = store
            .load_logins(now)
            .map(|(logins, _)| outlook::renewals_of(&roster, &logins))
            .unwrap_or_default();
        read.push(t.elapsed().as_micros());
        let t = Instant::now();
        let all = outlook::series_of(&dir, &roster, &state.machine, now);
        whole.push(t.elapsed().as_micros());
        let t = Instant::now();
        let series: std::collections::BTreeMap<String, Vec<account_series::SeriesRow>> = tails
            .iter_mut()
            .filter_map(|(label, tail)| {
                tail.refresh(now);
                (!tail.rows().is_empty()).then(|| (label.clone(), tail.rows().to_vec()))
            })
            .collect();
        tailed.push(t.elapsed().as_micros());
        if series != all {
            return Err(Fail::die("the tails' rows are not the whole read's"));
        }
        let t = Instant::now();
        let o = outlook::outlook(&outlook::Inputs {
            roster: &roster,
            state: &state,
            enrolled: &enrolled,
            series: &series,
            renewals: &renewals,
            mode: accounts::Mode::Early,
            credits: false,
            now,
            offset: |t| aterm_agent::supervise::limit::local_offset_at(t).unwrap_or(0),
            signing_in: None,
        });
        replay.push(t.elapsed().as_micros());
        out = Some(o.verdict);
    }
    Ok(format!(
        "resolver cost, scene A ({} rows, {rows_held} series rows, verdict {:?}): store read us \
         {}; series whole us {}; series tail us {}; replay us {}",
        roster.rows.len(),
        out,
        spread(&mut read),
        spread(&mut whole),
        spread(&mut tailed),
        spread(&mut replay)
    ))
}

/// `outlook-perf`: see [`PERF_HELP`].
/// # Errors
/// Usage, or an instance that would not start.
pub fn cmd_outlook_perf(args: &[String]) -> Res {
    crate::stop::install();
    let o = perf_opts(args)?;
    let branch = crate::instance::resolve_aterm(o.aterm.clone()).map_err(|e| Fail::exit(3, e))?;
    std::fs::create_dir_all(&o.out)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", o.out.display())))?;
    let mut lines = vec![format!(
        "# outlook-perf: main {} vs branch {}, {} pairs; {}",
        o.main.display(),
        branch.display(),
        o.pairs,
        PERF_HELP.lines().nth(2).unwrap_or_default()
    )];
    let say = |lines: &mut Vec<String>, l: String| {
        println!("outlook-perf: {l}");
        lines.push(l);
    };
    for pair in 1..=o.pairs {
        let load = crate::bench_stats::load_average()
            .map_or_else(|e| format!("load=? ({e})"), |l| format!("load1={l:.2}"));
        let gate = std::sync::Barrier::new(2);
        let (main, mine) = std::thread::scope(|s| {
            let main = s.spawn(|| arm(&o.main, &gate));
            let mine = s.spawn(|| arm(&branch, &gate));
            (
                main.join()
                    .unwrap_or_else(|_| Err(Fail::die("the main arm panicked"))),
                mine.join()
                    .unwrap_or_else(|_| Err(Fail::die("the branch arm panicked"))),
            )
        });
        for (name, took) in [("main", main), ("branch", mine)] {
            match took {
                Ok((typing, idle)) => {
                    say(&mut lines, format!("pair {pair} {name} {load} {typing}"));
                    say(&mut lines, format!("pair {pair} {name} {load} {idle}"));
                }
                Err(f) => say(
                    &mut lines,
                    format!(
                        "pair {pair} {name} NOT TAKEN: {}",
                        f.msg.unwrap_or_default()
                    ),
                ),
            }
        }
    }
    let cost = resolver_cost(100)?;
    say(&mut lines, cost);
    let path = o.out.join("perf.txt");
    crate::util::write_fresh(&path, format!("{}\n", lines.join("\n")))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))?;
    println!("outlook-perf: {}", path.display());
    Ok(0)
}

#[cfg(test)]
#[path = "outlook_captures_tests.rs"]
mod tests;
