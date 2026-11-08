// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the account stats view ([`super::view`], [`super::text`], [`super::json`]).

use aterm_json::{Value, json};

use super::*;
use crate::harness::accounts::{self, CreditsUse, State, TapRead, Wall, WindowRead};
use crate::harness::spend::{Message, PRICES_TOML, PriceTable};
use crate::harness::usage::TokenUsage;

const NOW: i64 = 1_792_082_400;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const MACHINE: &str = "3f9a1b2c4d5e6f70";

const UUID: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

fn profile(usage: Option<Value>) -> Result<Profile, NoProfile> {
    profile_of(UUID, usage)
}

fn profile_of(uuid: &str, usage: Option<Value>) -> Result<Profile, NoProfile> {
    let mut doc = json!({
        "oauthAccount": {
            "accountUuid": uuid,
            "organizationType": "claude_max",
            "userRateLimitTier": "default_claude_max_5x",
            "profileFetchedAt": (NOW - DAY) * 1000
        },
        "cachedExtraUsageDisabledReason": null,
        "cachedGrowthBookFeatures": {
            "tengu_saffron_credits_only_tiers": ["enterprise"],
            "tengu_saffron_lattice": {}
        }
    });
    if let (Some(body), Some(doc)) = (usage, doc.as_object_mut()) {
        doc.insert(
            "cachedUsageUtilization".to_owned(),
            json!({
                "fetchedAtMs": (NOW - 2 * DAY) * 1000,
                "accountUuid": uuid,
                "utilization": body
            }),
        );
    }
    profile::parse(aterm_json::to_string(&doc).expect("compose").as_bytes())
}

const WEEK_END: i64 = 1_792_356_600;

fn look<'a>(
    account: Option<&'a accounts::Account>,
    profile: &'a Result<Profile, NoProfile>,
    tap: Tap,
) -> Look<'a> {
    Look {
        now: NOW,
        tz_s: 0,
        machine: Some(MACHINE),
        label: "main",
        profile,
        account,
        state_unread: false,
        several: false,
        api_key_seen: false,
        tap,
        limit_reset: true,
        spend: None,
        prices_as_of: Some("2026-10-15"),
        login: None,
        sessions: Ok(2),
    }
}

fn tapped(five: u32, week: u32, at: i64) -> State {
    let mut state = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    let read = TapRead {
        five_hour: Some(WindowRead {
            pct_x10: five,
            reset: NOW + 85 * MIN,
            refilled: None,
        }),
        seven_day: Some(WindowRead {
            pct_x10: week,
            reset: WEEK_END,
            refilled: None,
        }),
        at,
        seen_before: true,
        born: None,
        key: None,
    };
    accounts::note_reading(&mut state, "main", &read, at);
    state
}

fn win<'a>(view: &'a View, id: &str) -> &'a Win {
    view.windows
        .iter()
        .find(|w| w.id == id)
        .expect("the window")
}

fn doc(look: &Look<'_>) -> Value {
    aterm_json::from_str(&json(look, &view(look), "0.105.0")).expect("one JSON document")
}

#[test]
fn with_the_tap_off_every_empty_figure_names_its_reason() {
    let p = profile(None);
    let l = look(None, &p, Tap::Off);
    let shown = text(&l, &view(&l));
    assert!(
        shown.starts_with("accounts at Thu Oct 15 16:40 \u{b7} this machine only \u{b7} prices 2026-10-15 (list)\n\n"),
        "{shown}"
    );
    for line in [
        "main \u{b7} claude \u{b7} Max 5x \u{b7} credits on\n",
        "  5h     \u{2014}  usage_tap off ([harness] usage_tap = true turns it on)\n",
        "  week   \u{2014}  usage_tap off\n",
        "  Fable  \u{2014}  no /usage seen on this home \u{b7} plan: Fable included\n",
        "  used   here (2 sessions)\n",
    ] {
        assert!(shown.contains(line), "{line:?} in {shown}");
    }
    let d = doc(&l);
    let windows = &d["accounts"][0]["windows"];
    let reasons: Vec<&Value> = (0..3).map(|i| &windows[i]["missing"]["reason"]).collect();
    assert_eq!(
        reasons,
        [
            &Value::from("usage-tap-off"),
            &Value::from("usage-tap-off"),
            &Value::from("no-usage-seen")
        ]
    );
    let harness_off = look(None, &p, Tap::HarnessOff);
    let said = text(&harness_off, &view(&harness_off));
    assert!(
        said.contains("  5h     \u{2014}  usage_tap off (the harness is off for Claude Code)\n"),
        "the key is on: the harness is what is off, {said}"
    );
    assert!(!said.contains("usage_tap = true turns it on"), "{said}");
    assert_eq!(
        doc(&harness_off)["accounts"][0]["windows"][0]["missing"]["reason"],
        Value::from("usage-tap-off"),
        "the same closed word"
    );
    assert_eq!(
        windows[0]["used_pct"],
        Value::Null,
        "unknown is null, never 0"
    );
    assert_eq!(windows[0]["state"], Value::Null);
    for (tap, word) in [
        (Tap::On, "no-reading"),
        (Tap::OtherHome, "tap-not-this-home"),
    ] {
        let l = look(None, &p, tap);
        assert_eq!(
            doc(&l)["accounts"][0]["windows"][0]["missing"]["reason"],
            Value::from(word)
        );
    }
}

/// The store after one tap reading that carried the week alone (Claude Code 2.1.316 on a login
/// whose replies report no five-hour window), at `at`.
fn week_only(week: u32, at: i64) -> State {
    let mut state = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    let read = TapRead {
        five_hour: None,
        seven_day: Some(WindowRead {
            pct_x10: week,
            reset: WEEK_END,
            refilled: None,
        }),
        at,
        seen_before: true,
        born: None,
        key: None,
    };
    accounts::note_reading(&mut state, "main", &read, at);
    state
}

/// A WINDOW THE VENDOR DOES NOT REPORT IS SAID SO, NOT "NO READING YET": where the account's
/// newest sample carried the week and no five-hour window, the five-hour row says `not reported`
/// with the sample's age, and `--json` says `not-reported`; with no sample it stays `no reading
/// yet`, and a void five-hour figure the sample came after reads `not reported` too, where one it
/// came before still reads `window reset … no reading since`.
#[test]
fn a_window_the_vendor_does_not_report_is_said_so() {
    let state = week_only(10, NOW - 4 * MIN);
    let p = profile(None);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    assert_eq!(win(&v, SESSION_LIMIT).missing, Some(Missing::NotReported));
    assert_eq!(win(&v, SESSION_LIMIT).fig, None, "no figure is invented");
    assert_eq!(win(&v, WEEK_LIMIT).missing, None);
    let shown = text(&l, &v);
    assert!(
        shown.contains(
            "  5h     \u{2014}  not reported: Claude Code reports the week but no 5h window for \
             this login (as of 4m)\n"
        ),
        "{shown}"
    );
    assert_eq!(
        doc(&l)["accounts"][0]["windows"][0]["missing"]["reason"],
        Value::from("not-reported")
    );
    let none = look(None, &p, Tap::On);
    assert_eq!(
        win(&view(&none), SESSION_LIMIT).missing,
        Some(Missing::NoReading)
    );
    let mut panel = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    accounts::note_sample(
        &mut panel,
        "main",
        accounts::Sample {
            week_pct: Some(1),
            week_reset: Some(WEEK_END),
            at: NOW - MIN,
            ..accounts::Sample::default()
        },
        NOW,
    );
    let l = look(panel.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), SESSION_LIMIT).missing,
        Some(Missing::NoReading),
        "a /usage panel's sample is not taken for it: a narrow pane may have cut the row"
    );
    let mut five_only = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    let read = TapRead {
        five_hour: Some(WindowRead {
            pct_x10: 400,
            reset: NOW + HOUR,
            refilled: None,
        }),
        seven_day: None,
        at: NOW - MIN,
        seen_before: true,
        born: None,
        key: None,
    };
    accounts::note_reading(&mut five_only, "main", &read, NOW - MIN);
    let l = look(five_only.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), WEEK_LIMIT).missing,
        Some(Missing::NoReading),
        "the week is never said unreported"
    );

    let mut after = week_only(10, NOW - 4 * MIN);
    let old = accounts::WindowFig {
        pct_x10: 300,
        reset: NOW - HOUR,
        at: NOW - 3 * HOUR,
        by: MACHINE.to_string(),
        source: accounts::SOURCE_TAP.to_string(),
        epoch_at: None,
    };
    let held = after.accounts.get_mut("main").expect("held");
    held.windows.insert(SESSION_LIMIT.to_string(), old.clone());
    let l = look(after.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), SESSION_LIMIT).missing,
        Some(Missing::NotReported),
        "the sample came after the window reset"
    );
    let held = after.accounts.get_mut("main").expect("held");
    held.windows.insert(
        SESSION_LIMIT.to_string(),
        accounts::WindowFig {
            reset: NOW - MIN,
            ..old
        },
    );
    let l = look(after.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), SESSION_LIMIT).missing,
        Some(Missing::WindowReset),
        "the sample came before the window reset"
    );
    let held = held_windows(&after.accounts["main"], None, Some(MACHINE), Tap::On, NOW);
    assert_eq!(held.windows.len(), 2, "the two windows, no per-model week");
}

#[test]
fn a_tap_figure_is_current_to_a_tenth_and_void_after_its_reset() {
    let state = tapped(425, 710, NOW - MIN);
    let p = profile(None);
    let account = state.accounts.get("main");
    let l = look(account, &p, Tap::On);
    let d = doc(&l);
    let five = &d["accounts"][0]["windows"][0];
    assert_eq!(five["id"], Value::from("five_hour"));
    assert_eq!(five["used_pct_x10"], Value::from(425u64));
    assert_eq!(five["used_pct"], Value::from(43u64));
    assert_eq!(five["left_pct"], Value::from(57u64));
    assert_eq!(five["resets_at"], Value::from(NOW + 85 * MIN));
    assert_eq!(five["source"], Value::from("tap"));
    assert_eq!(five["by"], Value::from(MACHINE));
    assert_eq!(five["state"], Value::from("current"));
    assert_eq!(five["as_of"], Value::from(NOW - MIN));
    assert_eq!(five["age_s"], Value::from(MIN));
    assert_eq!(five["missing"], Value::Null);
    let shown = text(&l, &view(&l));
    assert!(
        shown.contains("  5h     42.5% used \u{b7} 57.5% left \u{b7} resets 18:05 (in 1h25m)   as of 1m (tap, here)\n"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "  week   71.0% used \u{b7} 29.0% left \u{b7} resets Sun Oct 18 20:50 (in 3d4h)"
        ),
        "{shown}"
    );
    let after = Look {
        now: NOW + 86 * MIN,
        ..l
    };
    let d = doc(&after);
    let five = &d["accounts"][0]["windows"][0];
    assert_eq!(five["state"], Value::from("void"));
    assert_eq!(five["used_pct"], Value::Null);
    assert_eq!(five["missing"]["reason"], Value::from("window-reset"));
    let shown = text(&after, &view(&after));
    assert!(
        shown.contains("  5h     \u{2014}  window reset 18:05, no reading since\n"),
        "{shown}"
    );
    let before = Look {
        now: NOW + 84 * MIN,
        ..l
    };
    assert_eq!(
        doc(&before)["accounts"][0]["windows"][0]["state"],
        Value::from("current")
    );

    // Across the 2026-11-01 fall-back (09:00Z, -7h PDT -> -8h PST), `text_local`
    // places a post-change reset and login renewal on PST rather than `now`'s PDT.
    let dst_now = 1_793_498_400; // 2026-10-31T19:00:00-07:00
    let dst_week_reset = 1_793_556_000; // 2026-11-01T10:00:00-08:00 (11:00 under -7h)
    let dst_renew_by = 1_793_559_600; // 2026-11-01T11:00:00-08:00 (12:00 under -7h)
    let mut dst_state = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    accounts::note_reading(
        &mut dst_state,
        "main",
        &TapRead {
            seven_day: Some(WindowRead {
                pct_x10: 500,
                reset: dst_week_reset,
                refilled: None,
            }),
            at: dst_now - MIN,
            seen_before: true,
            ..TapRead::default()
        },
        dst_now - MIN,
    );
    let dst_look = Look {
        now: dst_now,
        tz_s: -7 * HOUR,
        account: dst_state.accounts.get("main"),
        login: Some(Login {
            state: "ok",
            renew_by: Some(dst_renew_by),
        }),
        ..look(None, &p, Tap::On)
    };
    let tz_at = |t: i64| {
        if t < 1_793_523_600 {
            -7 * HOUR
        } else {
            -8 * HOUR
        }
    };
    let dst_shown = text_local(&dst_look, &view(&dst_look), &tz_at);
    assert!(
        dst_shown.starts_with("accounts at Sat Oct 31 19:00 "),
        "{dst_shown}"
    );
    assert!(dst_shown.contains("resets Sun Nov 1 10:00 "), "{dst_shown}");
    assert!(
        dst_shown.contains("renew by Sun Nov 1 11:00\n"),
        "{dst_shown}"
    );
}

/// ACCOUNT STATS JSON SAYS 100% ONLY FOR A READING OF 100, as the glass and usage HUD do:
/// the compatibility whole is 99 for 995 and 999 tenths and 100 at 1000, while the exact tenths
/// stay unchanged. `left_pct` keeps its existing complement-of-half-up semantics. NEGATIVE
/// CONTROL: publishing `(pct_x10 + 5) / 10` makes both sub-100 readings `used_pct: 100` (run
/// once, restored).
#[test]
fn account_stats_json_says_100_only_for_a_reading_of_100() {
    let window = |pct_x10| Win {
        id: SESSION_LIMIT,
        kind: SESSION_LIMIT,
        name: None,
        length_s: 5 * HOUR,
        fig: Some(accounts::WindowFig {
            pct_x10,
            reset: NOW + HOUR,
            at: NOW - MIN,
            by: MACHINE.to_owned(),
            source: accounts::SOURCE_TAP.to_owned(),
            epoch_at: None,
        }),
        void_at: None,
        missing: None,
        since: None,
    };
    for (x10, whole) in [(995, 99), (999, 99), (1000, 100)] {
        let value = window_value(&window(x10), NOW);
        assert_eq!(value["used_pct_x10"], Value::from(x10), "{x10} tenths");
        assert_eq!(value["used_pct"], Value::from(whole), "{x10} tenths");
        assert_eq!(value["left_pct"], Value::from(0), "{x10} tenths");
    }
}

#[test]
fn a_reset_used_with_no_reading_since_says_so() {
    let mut state = tapped(1000, 400, NOW - HOUR);
    let fig = state
        .accounts
        .get_mut("main")
        .and_then(|a| a.windows.get_mut(SESSION_LIMIT))
        .expect("held");
    fig.epoch_at = Some(NOW - 10 * MIN);
    let p = profile(None);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let d = doc(&l);
    let five = &d["accounts"][0]["windows"][0];
    assert_eq!(five["missing"]["reason"], Value::from("reset-used"));
    assert_eq!(five["epoch_at"], Value::from(NOW - 10 * MIN));
    assert_eq!(five["used_pct"], Value::Null);
    let shown = text(&l, &view(&l));
    assert!(
        shown.contains("  5h     \u{2014}  reset used 16:30, no reading since\n"),
        "{shown}"
    );
    let plain = tapped(1000, 400, NOW - HOUR);
    let l = look(plain.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        doc(&l)["accounts"][0]["windows"][0]["used_pct"],
        Value::from(100u64)
    );

    let cached = profile(Some(
        json!({"seven_day": {"utilization": 96.0, "resets_at": "2026-10-18T20:50:00Z"}}),
    ));
    let mut said = State {
        machine: MACHINE.to_string(),
        ..State::default()
    };
    said.accounts.insert(
        "main".to_string(),
        accounts::Account {
            warned_week: Some(accounts::WindowSaid {
                pct: None,
                reset: None,
                at: NOW - DAY,
            }),
            ..accounts::Account::default()
        },
    );
    let l = look(said.accounts.get("main"), &cached, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        week["missing"]["reason"],
        Value::from("reset-used"),
        "{week}"
    );
    said.accounts.get_mut("main").expect("held").warned_week = Some(accounts::WindowSaid {
        pct: None,
        reset: None,
        at: NOW - 3 * DAY,
    });
    let l = look(said.accounts.get("main"), &cached, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        (&week["used_pct"], &week["source"]),
        (&Value::from(96u64), &Value::from("usage-cache"))
    );
}

fn week_said(pct_x10: u32, at: i64, said: accounts::WindowSaid) -> accounts::Account {
    accounts::Account {
        windows: [(
            WEEK_LIMIT.to_owned(),
            fig(pct_x10, WEEK_END, at, MACHINE, accounts::SOURCE_TAP),
        )]
        .into_iter()
        .collect(),
        warned_week: Some(said),
        ..accounts::Account::default()
    }
}

#[test]
fn a_reset_used_cuts_each_source_before_the_merge() {
    let cached = profile(Some(
        json!({"seven_day": {"utilization": 98.0, "resets_at": "2026-10-18T20:50:00Z"}}),
    ));
    let used = accounts::WindowSaid {
        pct: None,
        reset: None,
        at: NOW - DAY,
    };
    let after = week_said(50, NOW - MIN, used.clone());
    let l = look(Some(&after), &cached, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        (&week["used_pct_x10"], &week["source"], &week["epoch_at"]),
        (
            &Value::from(50u64),
            &Value::from("tap"),
            &Value::from(NOW - DAY)
        ),
        "{week}"
    );
    let before = week_said(50, NOW - 3 * DAY, used);
    let l = look(Some(&before), &cached, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        week["missing"]["reason"],
        Value::from("reset-used"),
        "{week}"
    );
}

#[test]
fn a_limit_that_lifted_by_itself_cuts_nothing() {
    let p = profile(None);
    let lifted = accounts::WindowSaid {
        pct: None,
        reset: Some(WEEK_END - WEEK_WINDOW_S),
        at: NOW - DAY,
    };
    let account = week_said(200, NOW - 2 * DAY, lifted.clone());
    let l = look(Some(&account), &p, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        (&week["used_pct_x10"], &week["epoch_at"]),
        (&Value::from(200u64), &Value::Null),
        "{week}"
    );
    let used = week_said(
        200,
        NOW - 2 * DAY,
        accounts::WindowSaid {
            reset: None,
            ..lifted
        },
    );
    let l = look(Some(&used), &p, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        week["missing"]["reason"],
        Value::from("reset-used"),
        "{week}"
    );
}

#[test]
fn a_figure_held_only_in_the_sample_is_shown() {
    let p = profile(None);
    let sample = |session: u32, at: i64| accounts::Sample {
        session_pct: Some(session),
        session_reset: Some(NOW + 85 * MIN),
        week_pct: Some(61),
        week_reset: Some(WEEK_END),
        at,
        by: MACHINE.to_owned(),
        ..accounts::Sample::default()
    };
    let only = accounts::Account {
        sample: Some(sample(42, NOW - 5 * MIN)),
        ..accounts::Account::default()
    };
    let l = look(Some(&only), &p, Tap::On);
    let d = doc(&l);
    let windows = &d["accounts"][0]["windows"];
    for (i, x10) in [(0, 420u64), (1, 610)] {
        assert_eq!(
            (
                &windows[i]["used_pct_x10"],
                &windows[i]["source"],
                &windows[i]["as_of"]
            ),
            (
                &Value::from(x10),
                &Value::from("sample"),
                &Value::from(NOW - 5 * MIN)
            ),
            "{windows}"
        );
    }
    let mut state = tapped(425, 710, NOW - MIN);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let five = win(&view(&l), SESSION_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((five.pct_x10, five.source.as_str()), (425, SOURCE_TAP_WORD));
    accounts::note_sample(&mut state, "main", sample(45, NOW), NOW);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let five = win(&view(&l), SESSION_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((five.pct_x10, five.source.as_str()), (450, SOURCE_SAMPLE));

    let old = NOW - 3 * HOUR;
    let mut state = tapped(420, 610, old);
    let reading = TapRead {
        five_hour: Some(WindowRead {
            pct_x10: 420,
            reset: NOW + 85 * MIN,
            refilled: None,
        }),
        seven_day: Some(WindowRead {
            pct_x10: 610,
            reset: WEEK_END,
            refilled: None,
        }),
        at: old,
        seen_before: true,
        ..TapRead::default()
    };
    let reload = |state: &State, now| State::from_json(&state.to_json(), now).unwrap();
    let observation = |state: &State, now| {
        let l = Look {
            now,
            ..look(state.accounts.get("main"), &p, Tap::On)
        };
        let view = view(&l);
        let week = win(&view, WEEK_LIMIT).current(now).expect("a running week");
        (week.pct_x10, week.at, week.source.clone())
    };
    assert_eq!(
        accounts::note_reading(&mut state, "main", &reading, NOW - MIN).len(),
        1
    );
    state = reload(&state, NOW);
    assert_eq!(
        observation(&state, NOW),
        (610, old, SOURCE_TAP_WORD.into()),
        "a tap heartbeat does not make account stats' held figure newly observed"
    );
    assert_eq!(
        accounts::note_sample(&mut state, "main", sample(42, NOW), NOW).len(),
        1
    );
    state = reload(&state, NOW);
    assert_eq!(
        observation(&state, NOW),
        (610, NOW, SOURCE_SAMPLE.into()),
        "a real fresh panel with equal values reaches account stats after storage"
    );
    let beat = NOW + accounts::READING_HEARTBEAT_S;
    assert_eq!(
        accounts::note_reading(&mut state, "main", &reading, beat).len(),
        1
    );
    state = reload(&state, beat);
    assert_eq!(
        observation(&state, beat),
        (610, NOW, SOURCE_SAMPLE.into()),
        "a later heartbeat preserves the actual panel's observation time"
    );
}

#[test]
fn every_source_of_a_window_merges_by_one_rule() {
    let state = tapped(400, 300, NOW - MIN);
    let body = json!({
        "five_hour": {"utilization": 60.0, "resets_at": "2026-10-15T18:05:00Z"},
        "seven_day": {"utilization": 90.0, "resets_at": "2026-10-11T20:50:00Z"},
        "limits": [{"kind": "weekly_scoped", "percent": 40.0, "resets_at": "2026-10-18T20:50:00Z",
                    "scope": {"model": {"display_name": "Fable"}}}]
    });
    let p = profile(Some(body));
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    let five = win(&v, SESSION_LIMIT).current(NOW).expect("current");
    assert_eq!(
        (five.pct_x10, five.source.as_str()),
        (600, SOURCE_USAGE_CACHE)
    );
    let week = win(&v, WEEK_LIMIT).current(NOW).expect("current");
    assert_eq!((week.pct_x10, week.source.as_str()), (300, SOURCE_TAP_WORD));
    let fable = win(&v, Bucket::Fable.id());
    assert_eq!(fable.name.as_deref(), Some("Fable"));
    let f = fable.current(NOW).expect("as last seen");
    assert_eq!((f.pct_x10, f.at), (400, NOW - 2 * DAY));
    let shown = text(&l, &view(&l));
    assert!(
        shown.contains("  Fable  40% used \u{b7} 60% left"),
        "{shown}"
    );
    assert!(
        shown.contains("as of 2d (/usage, here) \u{b7} plan: Fable included\n"),
        "{shown}"
    );
    let mut walled = state.clone();
    walled.accounts.get_mut("main").expect("held").walls.insert(
        SESSION_LIMIT.to_string(),
        Wall {
            until: Some(NOW + 85 * MIN),
            at: NOW,
            by: MACHINE.to_string(),
            attempts: 0,
            cleared: false,
        },
    );
    let l = look(walled.accounts.get("main"), &p, Tap::On);
    let five = &doc(&l)["accounts"][0]["windows"][0];
    assert_eq!(
        (&five["used_pct"], &five["source"]),
        (&Value::from(100u64), &Value::from("wall"))
    );
}

const SOURCE_TAP_WORD: &str = accounts::SOURCE_TAP;

#[test]
fn the_dollars_come_from_the_spend_beside_the_windows_reading() {
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the table");
    let model = prices
        .models()
        .find(|m| m.starts_with("claude-opus"))
        .expect("an Opus row")
        .to_owned();
    let mut spend = spend::Spend::new(std::path::Path::new("/h"), &prices);
    let tokens = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };
    spend.add_message(
        &Message {
            at: NOW - 2 * HOUR,
            model: &model,
            tokens,
            approx: false,
        },
        &prices,
        NOW,
    );
    let usd = prices.message_usd(&model, &tokens).expect("priced");
    let state = tapped(100, 500, NOW - MIN);
    let p = profile(None);
    let l = Look {
        spend: Some(SpendLook {
            spend: &spend,
            complete: true,
        }),
        ..look(state.accounts.get("main"), &p, Tap::On)
    };
    let v = view(&l);
    let week = dollars(&l, &v, win(&v, WEEK_LIMIT)).expect("a figure");
    assert!(!week.rolling);
    let c = week.capacity.as_ref().expect("a capacity");
    assert!((c.usd - usd / 0.5).abs() < 1e-6, "{c:?}");
    let left = week.left.as_ref().expect("left");
    assert!((left.usd - usd).abs() < 1e-6, "{left:?}");
    let d = doc(&l);
    let w = &d["accounts"][0]["spend"]["windows"][1];
    assert_eq!(w["id"], Value::from("seven_day"));
    assert_eq!(w["capacity_bound"], Value::from(">="));
    assert_eq!(
        d["accounts"][0]["spend"]["table_as_of"],
        Value::from("2026-10-15")
    );
    assert_eq!(d["complete"], Value::from(true));
    let shown = text(&l, &v);
    assert!(shown.contains(" of \u{2265}"), "{shown}");
    let mut credited = state.clone();
    credited.accounts.get_mut("main").expect("held").credits = Some(Credits::InUse(CreditsUse {
        since: NOW - HOUR,
        seen: NOW - HOUR,
        until: None,
        told: true,
    }));
    let l2 = Look {
        account: credited.accounts.get("main"),
        ..l
    };
    let v2 = view(&l2);
    let week = dollars(&l2, &v2, win(&v2, WEEK_LIMIT)).expect("a figure");
    assert_eq!(week.capacity.err(), Some(spend::Why::OnCredits));
    let l3 = Look { account: None, ..l };
    let v3 = view(&l3);
    let week = dollars(&l3, &v3, win(&v3, WEEK_LIMIT)).expect("a figure");
    assert!(week.rolling);
    assert!((week.used.usd - usd).abs() < 1e-6);
    assert_eq!(week.capacity.err(), Some(spend::Why::NoReading));
    let shown = text(&l3, &v3);
    assert!(shown.contains("week so far "), "{shown}");
    assert!(shown.contains("(rolling 7d)"), "{shown}");
    assert!(shown.contains("capacity \u{2014} no reading"), "{shown}");
}

fn shown_with(
    spend: &spend::Spend,
    profile: &Result<Profile, NoProfile>,
    account: Option<&accounts::Account>,
) -> (Value, String) {
    let l = Look {
        spend: Some(SpendLook {
            spend,
            complete: true,
        }),
        ..look(account, profile, Tap::On)
    };
    (doc(&l), text(&l, &view(&l)))
}

#[test]
fn a_record_of_another_account_is_not_shown_under_the_one_signed_in() {
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the table");
    let spend = spend::Spend::new(std::path::Path::new("/h"), &prices);
    let mut state = tapped(1000, 800, NOW - MIN);
    let held = state.accounts.get_mut("main").expect("held");
    held.key = profile::AccountKey::claude(UUID);
    held.walls.insert(
        SESSION_LIMIT.to_string(),
        Wall {
            until: Some(NOW + 85 * MIN),
            at: NOW,
            by: MACHINE.to_string(),
            attempts: 0,
            cleared: false,
        },
    );
    held.warned_week = Some(accounts::WindowSaid {
        pct: Some(80),
        reset: Some(WEEK_END),
        at: NOW - MIN,
    });
    held.credits = Some(Credits::InUse(CreditsUse {
        since: NOW - HOUR,
        seen: NOW - MIN,
        until: None,
        told: true,
    }));
    let other = "7d1e4a90-5b2c-4f3e-8a6d-0c9b8e7f6a5d";
    let b = profile_of(other, None);
    let b_key = profile::AccountKey::claude(other).expect("a key");
    let on_credits = |d: &Value| {
        d["accounts"][0]["spend"]["windows"]
            .as_array()
            .expect("the spend's windows")
            .iter()
            .any(|w| w["on_credits"] == Value::from(true))
    };
    let (d, shown) = shown_with(&spend, &b, state.accounts.get("main"));
    let account = &d["accounts"][0];
    assert_eq!(account["key"], Value::from(b_key.to_string()));
    for i in 0..2 {
        let w = &account["windows"][i];
        assert_eq!(w["used_pct"], Value::Null, "{w}");
        assert_eq!(w["state"], Value::Null, "{w}");
        assert_eq!(w["missing"]["reason"], Value::from("no-reading"), "{w}");
    }
    assert!(!on_credits(&d), "{d}");
    for row in [
        "  5h     \u{2014}  no reading yet\n",
        "  week   \u{2014}  no reading yet\n",
    ] {
        assert!(shown.contains(row), "{row:?} in {shown}");
    }
    assert!(!shown.contains("100%"), "{shown}");
    let a = profile(None);
    let (d, shown) = shown_with(&spend, &a, state.accounts.get("main"));
    let five = &d["accounts"][0]["windows"][0];
    assert_eq!(
        (&five["used_pct"], &five["source"]),
        (&Value::from(100u64), &Value::from("wall"))
    );
    assert!(on_credits(&d), "{d}");
    assert!(shown.contains("  5h     100% used"), "{shown}");
    let mut unkeyed = state.clone();
    unkeyed.accounts.get_mut("main").expect("held").key = None;
    let (d, _) = shown_with(&spend, &b, unkeyed.accounts.get("main"));
    assert_eq!(
        d["accounts"][0]["windows"][0]["used_pct"],
        Value::from(100u64)
    );
    let cached_b = profile_of(
        other,
        Some(json!({
            "five_hour": {"utilization": 20.0, "resets_at": "2026-10-15T18:05:00Z"},
            "seven_day": {"utilization": 60.0, "resets_at": "2026-10-18T20:50:00Z"}
        })),
    );
    for several in [false, true] {
        let l = Look {
            several,
            spend: Some(SpendLook {
                spend: &spend,
                complete: true,
            }),
            ..look(state.accounts.get("main"), &cached_b, Tap::On)
        };
        let d = doc(&l);
        let shown = text(&l, &view(&l));
        assert_eq!(d["accounts"][0]["key"], Value::from(b_key.to_string()));
        for (i, pct) in [(0, 20u64), (1, 60)] {
            let window = &d["accounts"][0]["windows"][i];
            assert_eq!(window["used_pct"], Value::from(pct));
            assert_eq!(window["source"], Value::from("usage-cache"));
        }
        assert!(!on_credits(&d));
        assert!(shown.contains("  5h     20% used"), "{shown}");
        assert!(shown.contains("  week   60% used"), "{shown}");
        assert!(!shown.contains("100%"), "{shown}");
        for key in [Some(b_key), None] {
            let mut admitted = state.accounts["main"].clone();
            admitted.key = key;
            let same = Look {
                account: Some(&admitted),
                ..l
            };
            let d = doc(&same);
            let five = &d["accounts"][0]["windows"][0];
            assert_eq!(d["accounts"][0]["key"], Value::from(b_key.to_string()));
            assert_eq!(five["used_pct"], Value::from(100u64));
            assert_eq!(five["source"], Value::from("wall"));
            assert!(on_credits(&d));
            let shown = text(&same, &view(&same));
            assert!(shown.contains("  5h     100% used"), "{shown}");
        }
    }
}

const OTHER: &str = "7d1e4a90-5b2c-4f3e-8a6d-0c9b8e7f6a5d";

fn profile_cached_by(uuid: &str, cached_by: &str, fetched: i64) -> Result<Profile, NoProfile> {
    let doc = json!({
        "oauthAccount": {"accountUuid": uuid, "organizationType": "claude_max"},
        "cachedUsageUtilization": {
            "fetchedAtMs": fetched * 1000,
            "accountUuid": cached_by,
            "utilization": {"seven_day": {"utilization": 90.0, "resets_at": "2026-10-18T20:50:00Z"}}
        }
    });
    profile::parse(aterm_json::to_string(&doc).expect("compose").as_bytes())
}

#[test]
fn spend_since_another_account_was_seen_on_the_home_is_unattributed() {
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the table");
    let model = prices
        .models()
        .find(|m| m.starts_with("claude-opus"))
        .expect("an Opus row")
        .to_owned();
    let mut spend = spend::Spend::new(std::path::Path::new("/h"), &prices);
    spend.add_message(
        &Message {
            at: NOW - 2 * HOUR,
            model: &model,
            tokens: TokenUsage {
                input: 1_000_000,
                ..TokenUsage::default()
            },
            approx: false,
        },
        &prices,
        NOW,
    );
    let (a_key, b_key) = (
        profile::AccountKey::claude(UUID),
        profile::AccountKey::claude(OTHER),
    );
    let attribution = |d: &Value| d["accounts"][0]["spend"]["attribution"].clone();
    let b = profile_of(OTHER, None);
    let mut a_state = tapped(100, 500, NOW - MIN);
    a_state.accounts.get_mut("main").expect("held").key = a_key;
    let (d, shown) = shown_with(&spend, &b, a_state.accounts.get("main"));
    assert_eq!(attribution(&d), Value::from("unattributed"), "{d}");
    assert!(shown.contains("(rolling 7d, unattributed)"), "{shown}");
    let a = profile(None);
    let (d, shown) = shown_with(&spend, &a, a_state.accounts.get("main"));
    assert_eq!(attribution(&d), Value::from("assumed"), "{d}");
    assert!(shown.contains(" of \u{2265}"), "{shown}");

    let keyed = |keyed_at: Option<i64>| {
        let mut state = tapped(100, 100, NOW - MIN);
        let held = state.accounts.get_mut("main").expect("held");
        held.key = b_key;
        held.keyed_at = keyed_at;
        state
    };
    let week = |p: &Result<Profile, NoProfile>, state: &State| {
        let l = Look {
            spend: Some(SpendLook {
                spend: &spend,
                complete: true,
            }),
            ..look(state.accounts.get("main"), p, Tap::On)
        };
        let v = view(&l);
        let figure = dollars(&l, &v, win(&v, WEEK_LIMIT)).expect("a figure");
        (figure, doc(&l), text(&l, &v))
    };
    let changed = keyed(Some(NOW - HOUR));
    let (figure, d, shown) = week(&b, &changed);
    assert!(!figure.rolling);
    assert_eq!(figure.capacity.err(), Some(spend::Why::Unattributed));
    assert_eq!(figure.left.err(), Some(spend::Why::Unattributed));
    assert_eq!(attribution(&d), Value::from("unattributed"));
    assert_eq!(
        d["accounts"][0]["spend"]["windows"][1]["suppressed"],
        Value::from("unattributed"),
        "{d}"
    );
    assert!(
        shown.contains("(capacity \u{2014} unattributed)"),
        "{shown}"
    );
    assert!(shown.contains("left \u{2014} unattributed"), "{shown}");
    assert!(!shown.contains(" of \u{2265}"), "{shown}");
    let (figure, d, shown) = week(&b, &keyed(Some(NOW - 8 * DAY)));
    assert!(figure.capacity.is_ok(), "{figure:?}");
    assert_eq!(attribution(&d), Value::from("assumed"));
    assert!(shown.contains(" of \u{2265}"), "{shown}");

    let cached = profile_cached_by(OTHER, UUID, NOW - 2 * DAY);
    let (figure, d, _) = week(&cached, &keyed(None));
    assert_eq!(figure.capacity.err(), Some(spend::Why::Unattributed));
    assert_eq!(attribution(&d), Value::from("unattributed"));
    let before = profile_cached_by(OTHER, UUID, NOW - 8 * DAY);
    let (figure, d, _) = week(&before, &keyed(None));
    assert!(figure.capacity.is_ok(), "{figure:?}");
    assert_eq!(attribution(&d), Value::from("assumed"));
}

#[test]
fn the_accounts_report_shows_no_record_of_another_account() {
    let mut state = tapped(400, 800, NOW - MIN);
    let held = state.accounts.get_mut("main").expect("held");
    held.key = profile::AccountKey::claude(UUID);
    held.walls.insert(
        SESSION_LIMIT.to_string(),
        Wall {
            until: Some(NOW + 85 * MIN),
            at: NOW,
            by: MACHINE.to_string(),
            attempts: 0,
            cleared: false,
        },
    );
    let record = &state.accounts["main"];
    let shown = |key| {
        held_windows(record, key, Some(MACHINE), Tap::On, NOW)
            .windows
            .iter()
            .map(|w| (w.current(NOW).map(|f| f.pct_x10), w.missing))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shown(profile::AccountKey::claude(OTHER)),
        [
            (None, Some(Missing::NoReading)),
            (None, Some(Missing::NoReading))
        ]
    );
    let b = profile_of(OTHER, None);
    let l = look(Some(record), &b, Tap::On);
    assert_eq!(
        view(&l)
            .windows
            .iter()
            .take(2)
            .map(|w| (w.current(NOW).map(|f| f.pct_x10), w.missing))
            .collect::<Vec<_>>(),
        shown(profile::AccountKey::claude(OTHER))
    );
    for key in [profile::AccountKey::claude(UUID), None] {
        assert_eq!(shown(key), [(Some(1000), None), (Some(800), None)]);
    }
}

#[test]
fn the_used_row_names_the_sessions_and_the_login() {
    let p = profile(None);
    let l = Look {
        login: Some(Login {
            state: "due",
            renew_by: Some(NOW + 2 * DAY),
        }),
        sessions: Ok(3),
        ..look(None, &p, Tap::On)
    };
    let shown = text(&l, &view(&l));
    assert!(
        shown.contains(
            "  used   here (3 sessions) \u{b7} login: here due, renew by Sat Oct 17 16:40\n"
        ),
        "{shown}"
    );
    let d = doc(&l);
    assert_eq!(
        d["accounts"][0]["used_by"][0]["sessions"],
        Value::from(3u64)
    );
    let mut keys: Vec<&str> = d["accounts"][0]["used_by"][0]
        .as_object()
        .expect("a used_by entry")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["last_seen", "last_seen_rfc3339", "machine", "sessions"],
        "an entry is this machine's census: its id, its count and when it was read"
    );
    assert_eq!(d["accounts"][0]["login"][0]["state"], Value::from("due"));
    let unread = Look {
        sessions: Err("census-unread"),
        login: None,
        ..l
    };
    let shown = text(&unread, &view(&unread));
    assert!(
        shown.contains("  used   here (sessions not counted: census-unread)\n"),
        "{shown}"
    );
    assert!(!shown.contains("login:"), "{shown}");
    let d = doc(&unread);
    assert_eq!(d["accounts"][0]["used_by"], Value::Array(Vec::new()));
    let missing = d["missing"].as_array().expect("a list");
    assert!(
        missing
            .iter()
            .any(|m| m["what"] == Value::from("used_by")
                && m["reason"] == Value::from("census-unread")),
        "{missing:?}"
    );
}

#[test]
fn the_texts_spans_instants_and_dollars() {
    assert_eq!(span(40), "40s");
    assert_eq!(span(12 * MIN), "12m");
    assert_eq!(span(85 * MIN), "1h25m");
    assert_eq!(span(2 * HOUR), "2h");
    assert_eq!(span(3 * DAY + 4 * HOUR + 10), "3d4h");
    assert_eq!(clock_local(NOW + 85 * MIN, NOW, &|_| 0), "18:05");
    assert_eq!(clock_local(WEEK_END, NOW, &|_| 0), "Sun Oct 18 20:50");
    assert_eq!(
        clock_local(NOW + 85 * MIN, NOW, &|_| -7 * HOUR),
        "11:05",
        "the local clock"
    );
    let dst_seam = 1_772_964_000; // 2026-03-08T10:00:00Z (02:00 PST -> 03:00 PDT)
    assert_eq!(
        clock_local(
            dst_seam + 30 * MIN,
            dst_seam - 30 * MIN,
            &|s| if s < dst_seam { -8 * HOUR } else { -7 * HOUR },
        ),
        "03:30",
        "DST spring-forward evaluates each instant's own offset"
    );
    assert_eq!(usd(4.214), "$4.21");
    assert_eq!(usd(1890.2), "$1,890");
    assert_eq!(usd(1_234_567.0), "$1,234,567");
}

#[test]
fn a_reset_used_clears_its_walls_hundred_percent() {
    let mut state = tapped(400, 900, NOW - HOUR);
    let _ = accounts::note_wall(
        &mut state,
        "main",
        WEEK_LIMIT,
        Some(WEEK_END),
        NOW - HOUR,
        NOW - HOUR,
    );
    let p = profile(None);
    let week = |state: &State| {
        let l = look(state.accounts.get("main"), &p, Tap::On);
        doc(&l)["accounts"][0]["windows"][1].clone()
    };
    let walled = week(&state);
    assert_eq!(
        (&walled["used_pct"], &walled["source"]),
        (&Value::from(100u64), &Value::from("wall")),
        "{walled}"
    );
    assert_eq!(
        accounts::claim_reset(
            &mut state,
            "main",
            (WEEK_LIMIT, WEEK_END),
            "s-1",
            41,
            "1792078800.250000",
            NOW - 30 * MIN
        ),
        accounts::ResetClaimed::Taken
    );
    let _ = accounts::reset_outcome(
        &mut state,
        "main",
        WEEK_END,
        "s-1",
        &accounts::ResetDone::Used {
            kind: aterm_phase::ResetKind::Weekly,
            left: Some(0),
        },
        NOW - 20 * MIN,
    );
    let used = week(&state);
    assert_eq!(used["used_pct"], Value::Null, "{used}");
    assert_eq!(
        used["missing"]["reason"],
        Value::from("reset-used"),
        "{used}"
    );
    let read = TapRead {
        seven_day: Some(WindowRead {
            pct_x10: 30,
            reset: WEEK_END,
            refilled: None,
        }),
        at: NOW - MIN,
        seen_before: true,
        ..TapRead::default()
    };
    accounts::note_reading(&mut state, "main", &read, NOW - MIN);
    let after = week(&state);
    assert_eq!(
        (&after["used_pct_x10"], &after["source"]),
        (&Value::from(30u64), &Value::from("tap")),
        "{after}"
    );
}

#[test]
fn a_per_model_weeks_dollars_are_its_own_models() {
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the table");
    let opus = prices
        .models()
        .find(|m| m.starts_with("claude-opus"))
        .expect("an Opus row")
        .to_owned();
    let mut spend = spend::Spend::new(std::path::Path::new("/h"), &prices);
    let tokens = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };
    spend.add_message(
        &Message {
            at: NOW - 2 * HOUR,
            model: &opus,
            tokens,
            approx: false,
        },
        &prices,
        NOW,
    );
    let usd = prices.message_usd(&opus, &tokens).expect("priced");
    let body = json!({
        "seven_day_sonnet": {"utilization": 50.0, "resets_at": "2026-10-18T20:50:00Z"},
        "seven_day_opus": {"utilization": 50.0, "resets_at": "2026-10-18T20:50:00Z"},
        "limits": [{"kind": "weekly_scoped", "percent": 40.0, "resets_at": "2026-10-18T20:50:00Z",
                    "scope": {"model": {"display_name": "Haiku"}}}]
    });
    let p = profile(Some(body));
    let state = tapped(100, 500, NOW - MIN);
    let l = Look {
        spend: Some(SpendLook {
            spend: &spend,
            complete: true,
        }),
        ..look(state.accounts.get("main"), &p, Tap::On)
    };
    let v = view(&l);
    let spent = |id: &str| dollars(&l, &v, win(&v, id)).map(|f| f.used.usd);
    assert!(
        spent(WEEK_LIMIT).is_some_and(|d| (d - usd).abs() < 1e-6),
        "the week: every model's"
    );
    assert!(spent("seven_day_opus").is_some_and(|d| (d - usd).abs() < 1e-6));
    assert_eq!(spent("seven_day_sonnet"), Some(0.0), "no Sonnet spend");
    assert_eq!(spent("seven_day_other"), None, "no family named");
}

#[test]
fn each_unknown_model_week_is_its_own_window() {
    let row = |name: &str, pct: f64| {
        json!({"kind": "weekly_scoped", "percent": pct, "resets_at": "2026-10-18T20:50:00Z",
               "scope": {"model": {"display_name": name}}})
    };
    let p = profile(Some(
        json!({"limits": [row("Sonnet", 10.0), row("Haiku", 85.0)]}),
    ));
    let l = look(None, &p, Tap::On);
    let v = view(&l);
    let others: Vec<(Option<&str>, Option<u32>)> = v
        .windows
        .iter()
        .filter(|w| w.id == "seven_day_other")
        .map(|w| (w.name.as_deref(), w.current(NOW).map(|f| f.pct_x10)))
        .collect();
    assert_eq!(
        others,
        [(Some("Sonnet"), Some(100)), (Some("Haiku"), Some(850))]
    );
}

#[test]
fn a_refill_the_tap_saw_cuts_the_warning_and_the_wall() {
    let read = |pct_x10: u32, refilled: Option<u32>, at: i64| TapRead {
        seven_day: Some(WindowRead {
            pct_x10,
            reset: WEEK_END,
            refilled,
        }),
        at,
        seen_before: true,
        ..TapRead::default()
    };
    let walled = |refilled: Option<u32>| {
        let mut state = State {
            machine: MACHINE.to_string(),
            ..State::default()
        };
        accounts::note_reading(&mut state, "main", &read(970, None, NOW - HOUR), NOW - HOUR);
        let _ = accounts::note_wall(
            &mut state,
            "main",
            WEEK_LIMIT,
            Some(WEEK_END),
            NOW - HOUR,
            NOW - HOUR,
        );
        state.accounts.get_mut("main").expect("held").warned_week = Some(accounts::WindowSaid {
            pct: Some(97),
            reset: Some(WEEK_END),
            at: NOW - HOUR,
        });
        let pct = if refilled.is_some() { 20 } else { 990 };
        accounts::note_reading(
            &mut state,
            "main",
            &read(pct, refilled, NOW - MIN),
            NOW - MIN,
        );
        state
    };
    let p = profile(None);
    let state = walled(Some(970));
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let week = win(&view(&l), WEEK_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((week.pct_x10, week.source.as_str()), (20, SOURCE_TAP_WORD));
    let state = walled(None);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let week = win(&view(&l), WEEK_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((week.pct_x10, week.source.as_str()), (1000, SOURCE_WALL));
}

const FABLE: &str = "seven_day_overage_included";

/// `state` with the screen's word on the Fable week.
fn fable_said(mut state: State, pct: u32, reset: Option<i64>, at: i64, wall: bool) -> State {
    state
        .accounts
        .entry("main".to_owned())
        .or_default()
        .weeks_said
        .insert(
            FABLE.to_owned(),
            accounts::WeekSaid {
                pct: Some(pct),
                reset,
                at,
                by: MACHINE.to_owned(),
                wall,
                ends_by: None,
            },
        );
    state
}

fn unread() -> State {
    State {
        machine: MACHINE.to_string(),
        ..State::default()
    }
}

/// The Fable row of the `--json` document.
fn fable_doc(l: &Look<'_>) -> Value {
    let d = doc(l);
    let windows = d["accounts"][0]["windows"]
        .as_array()
        .expect("windows")
        .clone();
    windows
        .into_iter()
        .find(|w| w["id"] == Value::from(FABLE))
        .expect("the Fable window")
}

#[test]
fn a_screens_fable_week_shows_with_its_source_and_a_newer_window_wins() {
    let p = profile(None);
    let none = unread();
    let l = look(none.accounts.get("main"), &p, Tap::On);
    assert!(
        text(&l, &view(&l)).contains(
            "  Fable  \u{2014}  no /usage seen on this home \u{b7} plan: Fable included\n"
        ),
        "the control: no word of the screen's keeps the missing word"
    );
    for (pct, wall, source) in [(85, false, SOURCE_WARNING), (100, true, SOURCE_WALL)] {
        let state = fable_said(unread(), pct, Some(WEEK_END), NOW - HOUR, wall);
        let l = look(state.accounts.get("main"), &p, Tap::On);
        let v = view(&l);
        let fable = win(&v, FABLE);
        assert_eq!(
            fable.current(NOW).map(|f| (f.pct_x10, f.source.as_str())),
            Some((pct * 10, source))
        );
        assert_eq!(fable.void_at, None);
        let row = format!(
            "  Fable  {pct}% used \u{b7} {}% left \u{b7} resets Sun Oct 18 20:50 (in 3d4h)   as of 1h ({source}, here) \u{b7} plan: Fable included\n",
            100 - pct
        );
        let shown = text(&l, &v);
        assert!(shown.contains(&row), "{row:?} in {shown}");
        let j = fable_doc(&l);
        assert_eq!(
            (&j["resets_at"], &j["void_at"], &j["source"]),
            (&Value::from(WEEK_END), &Value::Null, &Value::from(source))
        );
    }
    let cached = profile(Some(json!({"limits": [{
        "kind": "weekly_scoped", "percent": 40.0, "resets_at": "2026-10-18T20:50:00Z",
        "scope": {"model": {"display_name": "Fable"}}}]})));
    let older = fable_said(unread(), 85, Some(NOW + HOUR), NOW - 3 * DAY, false);
    let l = look(older.accounts.get("main"), &cached, Tap::On);
    assert_eq!(
        win(&view(&l), FABLE)
            .current(NOW)
            .map(|f| (f.pct_x10, f.source.as_str())),
        Some((400, SOURCE_USAGE_CACHE)),
        "the cache's newer window"
    );
}

/// A cached `/usage` body read before the wall, its Fable week at 40% to [`WEEK_END`].
fn fable_cached() -> Result<Profile, NoProfile> {
    profile(Some(json!({"limits": [{
        "kind": "weekly_scoped", "percent": 40.0, "resets_at": "2026-10-18T20:50:00Z",
        "scope": {"model": {"display_name": "Fable"}}}]})))
}

#[test]
fn a_fable_wall_that_named_no_reset_is_full_with_no_reset_given() {
    let p = fable_cached();
    let walled = fable_said(tapped(100, 500, NOW - MIN), 100, None, NOW - HOUR, true);
    let l = look(walled.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    let fable = win(&v, FABLE);
    assert_eq!(
        fable.void_at,
        Some(WEEK_END),
        "the reset an older reading of the Fable week named"
    );
    assert_eq!(fable.resets_at(), None);
    let shown = text(&l, &v);
    let row =
        "  Fable  100%, no reset given \u{b7} as of 1h (wall, here) \u{b7} plan: Fable included\n";
    assert!(shown.contains(row), "{row:?} in {shown}");
    let j = fable_doc(&l);
    assert_eq!(
        (
            &j["resets_at"],
            &j["resets_at_rfc3339"],
            &j["void_at"],
            &j["void_at_rfc3339"],
            &j["state"],
            &j["used_pct"],
            &j["source"],
        ),
        (
            &Value::Null,
            &Value::Null,
            &Value::from(WEEK_END),
            &Value::from("2026-10-18T20:50:00Z"),
            &Value::from("current"),
            &Value::from(100u64),
            &Value::from("wall"),
        ),
        "{j}"
    );
    let d = doc(&l);
    let week = &d["accounts"][0]["windows"][1];
    assert_eq!(
        (&week["void_at"], &week["void_at_rfc3339"]),
        (&Value::Null, &Value::Null),
        "every window carries the key, null unless it named no reset"
    );

    let alone = fable_said(unread(), 100, None, NOW - HOUR, true);
    let none = profile(None);
    let l = look(alone.accounts.get("main"), &none, Tap::On);
    assert_eq!(
        win(&view(&l), FABLE).void_at,
        Some(NOW - HOUR + WEEK_WINDOW_S),
        "with no reset of the Fable week held, a week after the reading"
    );

    let mut tapped_wall = tapped(100, 500, NOW - MIN);
    let _ = accounts::note_wall(
        &mut tapped_wall,
        "main",
        FABLE,
        None,
        NOW - HOUR,
        NOW - HOUR,
    );
    let l = look(tapped_wall.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    assert_eq!(
        (
            win(&v, FABLE).void_at,
            win(&v, FABLE).current(NOW).map(|f| f.pct_x10)
        ),
        (Some(WEEK_END), Some(1000)),
        "a wall a 429 recorded with no reset is the same form"
    );
}

#[test]
fn a_fable_wall_that_named_no_reset_is_void_after_the_week_and_on_a_newer_reading() {
    let p = fable_cached();
    let walled = fable_said(tapped(100, 500, NOW - MIN), 100, None, NOW - HOUR, true);
    let after = Look {
        now: WEEK_END + MIN,
        ..look(walled.accounts.get("main"), &p, Tap::On)
    };
    let j = fable_doc(&after);
    assert_eq!(
        (
            &j["state"],
            &j["used_pct"],
            &j["resets_at"],
            &j["void_at"],
            &j["missing"]["reason"]
        ),
        (
            &Value::from("void"),
            &Value::Null,
            &Value::Null,
            &Value::from(WEEK_END),
            &Value::from("window-reset")
        ),
        "{j}"
    );
    let shown = text(&after, &view(&after));
    assert!(
        shown.contains("  Fable  \u{2014}  window reset by "),
        "a reset no reading named is no window's instant: {shown}"
    );
    let before = Look {
        now: WEEK_END - MIN,
        ..after
    };
    assert_eq!(
        fable_doc(&before)["state"],
        Value::from("current"),
        "the control: current up to the reset"
    );
    let older = fable_said(unread(), 100, None, NOW - 3 * DAY, true);
    let l = look(older.accounts.get("main"), &p, Tap::On);
    let fable = win(&view(&l), FABLE).clone();
    assert_eq!(
        (fable.void_at, fable.current(NOW).map(|f| f.pct_x10)),
        (None, Some(400)),
        "a newer reading of the week replaces it"
    );
    let newer = fable_said(unread(), 100, None, NOW - DAY, true);
    let l = look(newer.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), FABLE).void_at,
        Some(WEEK_END),
        "the control: an older reading does not, and its reset ends the wall's week"
    );
}

/// The week of all models is another window than a model's own: its reset, and a reading of the
/// week after it, neither end a model's wall that named no reset nor bring it back.
#[test]
fn a_fable_wall_that_named_no_reset_is_its_own_weeks_alone() {
    let p = profile(None);
    let walled = fable_said(tapped(100, 500, NOW - MIN), 100, None, NOW - HOUR, true);
    let mut rolled = walled.clone();
    let next = TapRead {
        seven_day: Some(WindowRead {
            pct_x10: 50,
            reset: WEEK_END + WEEK_WINDOW_S,
            refilled: None,
        }),
        at: WEEK_END + DAY,
        seen_before: true,
        ..TapRead::default()
    };
    accounts::note_reading(&mut rolled, "main", &next, WEEK_END + DAY);
    let fable = |state: &State, now: i64| {
        let l = Look {
            now,
            ..look(state.accounts.get("main"), &p, Tap::On)
        };
        let v = view(&l);
        let fable = win(&v, FABLE);
        (fable.void_at, fable.current(now).map(|f| f.pct_x10))
    };
    assert_eq!(
        fable(&rolled, WEEK_END + DAY),
        fable(&walled, WEEK_END + DAY),
        "a reading of the next week of all models"
    );
    assert_eq!(
        fable(&walled, WEEK_END + MIN),
        (Some(NOW - HOUR + WEEK_WINDOW_S), Some(1000)),
        "past the reset of the week of all models"
    );
}

/// The no-reset form is a wall's: a warning that named no reset is no figure, as a session or
/// weekly warning that named none is not.
#[test]
fn a_fable_warning_that_named_no_reset_is_no_figure() {
    let p = profile(None);
    let warned = fable_said(unread(), 85, None, NOW - HOUR, false);
    let l = look(warned.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    assert_eq!(
        (win(&v, FABLE).fig.as_ref(), win(&v, FABLE).void_at),
        (None, None)
    );
    assert!(
        text(&l, &v).contains("  Fable  \u{2014}  no /usage seen on this home"),
        "{}",
        text(&l, &v)
    );
}

/// A tap's 429 that named no reset and a screen's wall that named one, read in the same second:
/// the reset is known.
#[test]
fn a_reset_read_with_a_no_reset_wall_of_its_second_is_kept() {
    let p = profile(None);
    let mut state = fable_said(unread(), 100, Some(WEEK_END), NOW - HOUR, true);
    let _ = accounts::note_wall(&mut state, "main", FABLE, None, NOW - HOUR, NOW - HOUR);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    let fable = win(&v, FABLE);
    assert_eq!((fable.void_at, fable.resets_at()), (None, Some(WEEK_END)));
    let mut alone = unread();
    let _ = accounts::note_wall(&mut alone, "main", FABLE, None, NOW - HOUR, NOW - HOUR);
    let l = look(alone.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), FABLE).void_at,
        Some(NOW - HOUR + WEEK_WINDOW_S),
        "the control: the 429 alone"
    );
}

/// A limit reset used on the Fable week ([`accounts::reset_outcome`]'s word, no figure and no
/// reset) cuts an older figure of the week, as a refill the screen said cuts the session's and the
/// week's, and outranks an older 429 that named no reset.
#[test]
fn a_limit_reset_used_on_the_fable_week_cuts_its_older_figure() {
    let p = fable_cached();
    let mut state = unread();
    let _ = accounts::note_wall(
        &mut state,
        "main",
        FABLE,
        None,
        NOW - 2 * HOUR,
        NOW - 2 * HOUR,
    );
    state
        .accounts
        .entry("main".to_owned())
        .or_default()
        .weeks_said
        .insert(
            FABLE.to_owned(),
            accounts::WeekSaid {
                pct: None,
                reset: None,
                at: NOW - HOUR,
                by: MACHINE.to_owned(),
                wall: false,
                ends_by: None,
            },
        );
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let v = view(&l);
    let fable = win(&v, FABLE);
    assert_eq!(
        (fable.missing, fable.current(NOW).is_some(), fable.void_at),
        (Some(Missing::ResetUsed), false, None)
    );
    let none = unread();
    let l = look(none.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        win(&view(&l), FABLE).current(NOW).map(|f| f.pct_x10),
        Some(400),
        "the control: the cached figure with no reset since"
    );
}

#[test]
fn a_fable_wall_that_named_no_reset_has_the_rolling_weeks_dollars() {
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the table");
    let model = prices
        .models()
        .find(|m| m.starts_with("claude-fable"))
        .expect("a Fable row")
        .to_owned();
    let mut spend = spend::Spend::new(std::path::Path::new("/h"), &prices);
    let tokens = TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    };
    spend.add_message(
        &Message {
            at: NOW - 2 * HOUR,
            model: &model,
            tokens,
            approx: false,
        },
        &prices,
        NOW,
    );
    let usd = prices.message_usd(&model, &tokens).expect("priced");
    let p = profile(None);
    let figure = |reset: Option<i64>| {
        let state = fable_said(tapped(100, 500, NOW - MIN), 100, reset, NOW - HOUR, true);
        let l = Look {
            spend: Some(SpendLook {
                spend: &spend,
                complete: true,
            }),
            ..look(state.accounts.get("main"), &p, Tap::On)
        };
        let v = view(&l);
        (
            dollars(&l, &v, win(&v, FABLE)).expect("a figure"),
            text(&l, &v),
        )
    };
    let (unnamed, shown) = figure(None);
    assert!(unnamed.rolling, "{unnamed:?}");
    assert_eq!(unnamed.from, NOW - WEEK_WINDOW_S);
    assert!((unnamed.used.usd - usd).abs() < 1e-6);
    assert_eq!(unnamed.capacity.err(), Some(spend::Why::NoReading));
    assert!(shown.contains("Fable so far "), "{shown}");
    let (named, _) = figure(Some(WEEK_END));
    assert!(
        !named.rolling,
        "the control: a wall that named its reset gives the window"
    );
}

/// A series row of `main`'s readings at `at`: the five-hour window's `(pct_x10, reset)` and the
/// week's.
fn series_fig(at: i64, five: Option<(u32, i64)>, week: Option<(u32, i64)>) -> SeriesRow {
    let fig = |(pct_x10, reset): (u32, i64)| account_series::Fig {
        pct_x10,
        reset,
        at,
        src: accounts::SOURCE_TAP.to_owned(),
        epoch: None,
    };
    SeriesRow {
        t: at,
        by: MACHINE.to_owned(),
        said: account_series::Said::Fig {
            five_hour: five.map(fig),
            seven_day: week.map(fig),
            ac: None,
            key: None,
        },
    }
}

/// `main`'s view at five-hour `five` and week `week` read a minute ago, its `since` filled from
/// `rows` over `span_s`: the view, its text and its JSON.
fn since_view(five: u32, week: u32, rows: &[SeriesRow], span_s: i64) -> (View, String, Value) {
    let state = tapped(five, week, NOW - MIN);
    let p = profile(None);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let mut v = view(&l);
    with_since(&mut v, (rows, account_series::Keys::default()), span_s, NOW);
    let shown = text(&l, &v);
    let d = aterm_json::from_str(&json(&l, &v, "0.105.0")).expect("one JSON document");
    (v, shown, d)
}

/// SINCE SAYS THE RISE, A NEW WINDOW OR A RESET USED. Against a day of readings, the week rose
/// from 53% a day ago to 71% now (+18, not +31 from the older reading before it), and the
/// five-hour window a day ago is one that has since ended, so a new one began at its reset less
/// five hours; a week that refilled says the reset used and when; a series younger than the
/// span says its own age, and one whose newest reading before the span's start is older than a
/// reading heartbeat says the span it did compare (25h, 2d), never a rise over a longer gap put
/// inside 24h; a figure that fell by less than a refill rose by nothing, never a negative
/// number; and the window with no figure has no `since`. Text and JSON say the same.
/// Negative control: taking the baseline as the newest figure (`max_by_key` over every `fig`
/// in `since_of`, ignoring `now − span_s`) compares now with now, gives `+0 since 24h`, and the
/// first assertion fails.
#[test]
fn since_says_the_rise_a_new_window_or_a_reset_used() {
    let five_reset = NOW + 85 * MIN;
    let rows = [
        series_fig(
            NOW - 30 * HOUR,
            Some((200, NOW - 20 * HOUR)),
            Some((400, WEEK_END)),
        ),
        series_fig(
            NOW - 24 * HOUR,
            Some((500, NOW - 20 * HOUR)),
            Some((530, WEEK_END)),
        ),
        series_fig(
            NOW - 2 * HOUR,
            Some((300, five_reset)),
            Some((700, WEEK_END)),
        ),
        series_fig(NOW - MIN, Some((380, five_reset)), Some((710, WEEK_END))),
    ];
    let (v, shown, d) = since_view(380, 710, &rows, DAY);
    assert_eq!(
        win(&v, "seven_day").since,
        Some(Since {
            span_s: DAY,
            moved: Moved::Rose(18)
        })
    );
    assert_eq!(
        win(&v, "five_hour").since,
        Some(Since {
            span_s: DAY,
            moved: Moved::NewWindow(five_reset - 5 * HOUR)
        })
    );
    assert_eq!(win(&v, Bucket::Fable.id()).since, None);
    let row = |id: &str| {
        shown
            .lines()
            .find(|l| l.trim_start().starts_with(id))
            .unwrap_or_default()
            .to_owned()
    };
    assert!(row("week").ends_with(" \u{b7} +18 since 24h"), "{shown}");
    assert!(
        row("5h").ends_with(" \u{b7} new window since 13:05"),
        "{shown}"
    );
    let windows = &d["accounts"][0]["windows"];
    assert_eq!(
        windows[1]["since"],
        json!({"span_s": DAY, "used_pct": 18, "new_window": null, "new_window_rfc3339": null,
               "reset_used": null, "reset_used_rfc3339": null})
    );
    assert_eq!(windows[0]["since"]["used_pct"], Value::Null);
    assert_eq!(
        windows[0]["since"]["new_window"],
        Value::from(five_reset - 5 * HOUR)
    );
    assert_eq!(
        windows[0]["since"]["new_window_rfc3339"],
        Value::from("2026-10-15T13:05:00Z")
    );
    assert_eq!(windows[2]["since"], Value::Null);

    let refilled = [
        series_fig(NOW - 25 * HOUR, None, Some((900, WEEK_END))),
        series_fig(NOW - 10 * HOUR, None, Some((100, WEEK_END))),
    ];
    let (v, shown, d) = since_view(380, 150, &refilled, DAY);
    assert_eq!(
        win(&v, "seven_day").since,
        Some(Since {
            span_s: 25 * HOUR,
            moved: Moved::ResetUsed(NOW - 10 * HOUR)
        }),
        "the baseline an hour before the span's start says the span it covers"
    );
    assert!(
        shown.contains(" \u{b7} reset used since 06:40\n"),
        "{shown}"
    );
    assert_eq!(
        d["accounts"][0]["windows"][1]["since"]["reset_used"],
        Value::from(NOW - 10 * HOUR)
    );
    assert_eq!(win(&v, "five_hour").since, None, "no five-hour rows");

    let young = [series_fig(NOW - 3 * HOUR, None, Some((660, WEEK_END)))];
    let (v, shown, _) = since_view(380, 710, &young, DAY);
    assert_eq!(
        win(&v, "seven_day").since,
        Some(Since {
            span_s: 3 * HOUR,
            moved: Moved::Rose(5)
        })
    );
    assert!(shown.contains(" \u{b7} +5 since 3h\n"), "{shown}");

    let fell = [series_fig(NOW - 2 * DAY, None, Some((715, WEEK_END)))];
    let (v, shown, _) = since_view(380, 710, &fell, DAY);
    assert_eq!(
        win(&v, "seven_day").since,
        Some(Since {
            span_s: 2 * DAY,
            moved: Moved::Rose(0)
        })
    );
    assert!(shown.contains(" \u{b7} +0 since 2d\n"), "{shown}");
}

/// Thursday, November 12, 2026, at the hour and minute of [`NOW`] (UTC).
const NOV_12: i64 = NOW + 28 * DAY;

/// The record of `main` after one limit-reset offer was noted of it on machine `by`
/// ([`accounts::note_offer`], the record's one writer), seen at `at`.
fn offered(
    kind: Option<aterm_phase::ResetKind>,
    left: u32,
    use_by: Option<i64>,
    at: i64,
    by: &str,
) -> accounts::Account {
    let mut state = State {
        machine: by.to_string(),
        ..State::default()
    };
    assert!(accounts::note_offer(
        &mut state, "main", kind, left, use_by, at, NOW
    ));
    state.account("main")
}

/// The `resets` object of the `--json` document.
fn resets_doc(l: &Look<'_>) -> Value {
    doc(l)["accounts"][0]["resets"].clone()
}

/// THE `resets` OBJECT IS THE LIMIT-RESET OFFER AS LAST SEEN, NEVER AN INVENTED ONE: its count, its
/// kind where one was named (null where not), the use-by instant the confirmation drew, when and by
/// which machine it was seen, and `void` from that instant on; and with no offer held, every value
/// null (never 0) and `missing` naming why: `no-grant-read`, or `limit-reset-off` where the
/// harness's limit reset, the one program that records an offer, is off. An offer held stays as
/// last seen with the limit reset off since. It carries instants only, never a local day or a hint
/// flag: the use-by is a deadline the vendor drew to the minute. The next-available date is never
/// read, so it is null in every case.
#[test]
fn the_resets_object_is_the_offer_as_last_seen_and_never_invented() {
    use aterm_phase::ResetKind;
    let p = profile(None);
    let seen = NOW - 3 * DAY;
    let current = offered(Some(ResetKind::Weekly), 1, Some(NOV_12), seen, MACHINE);
    let r = resets_doc(&look(Some(&current), &p, Tap::On));
    assert_eq!(
        r,
        json!({
            "left": 1,
            "kind": "weekly",
            "use_by": NOV_12,
            "use_by_rfc3339": "2026-11-12T16:40:00Z",
            "next_available_at": null,
            "next_available_at_rfc3339": null,
            "as_of": seen,
            "as_of_rfc3339": "2026-10-12T16:40:00Z",
            "source": "limit-reset",
            "by": MACHINE,
            "state": "current",
            "missing": null
        })
    );
    let east = Look {
        tz_s: 9 * HOUR,
        ..look(Some(&current), &p, Tap::On)
    };
    assert_eq!(resets_doc(&east), r, "no local clock in the document");
    let passed = offered(Some(ResetKind::Weekly), 1, Some(NOW - DAY), seen, MACHINE);
    let r = resets_doc(&look(Some(&passed), &p, Tap::On));
    assert_eq!(r["state"], Value::from("void"));
    assert_eq!(r["left"], Value::from(1u32), "as last seen");
    assert_eq!(r["use_by"], Value::from(NOW - DAY));
    let at_it = Look {
        now: NOV_12,
        ..look(Some(&current), &p, Tap::On)
    };
    assert_eq!(
        resets_doc(&at_it)["state"],
        Value::from("void"),
        "at use_by"
    );
    let kindless = offered(None, 2, None, seen, MACHINE);
    let r = resets_doc(&look(Some(&kindless), &p, Tap::On));
    assert_eq!(
        (&r["left"], &r["kind"], &r["use_by"], &r["state"]),
        (
            &Value::from(2u32),
            &Value::Null,
            &Value::Null,
            &Value::from("current")
        )
    );
    let none = |why: &str| {
        json!({
            "left": null,
            "kind": null,
            "use_by": null,
            "use_by_rfc3339": null,
            "next_available_at": null,
            "next_available_at_rfc3339": null,
            "as_of": null,
            "as_of_rfc3339": null,
            "source": null,
            "by": null,
            "state": null,
            "missing": {"reason": why}
        })
    };
    let bare = tapped(425, 710, NOW - MIN);
    for (account, why) in [
        (bare.accounts.get("main"), "a record with no offer"),
        (None, "no record"),
    ] {
        assert_eq!(
            resets_doc(&look(account, &p, Tap::On)),
            none("no-grant-read"),
            "{why}"
        );
        let off = Look {
            limit_reset: false,
            ..look(account, &p, Tap::On)
        };
        assert_eq!(
            resets_doc(&off),
            none("limit-reset-off"),
            "{why}, the limit reset off"
        );
    }
    for account in [&current, &passed, &kindless] {
        let r = resets_doc(&look(Some(account), &p, Tap::On));
        assert_eq!(r["next_available_at"], Value::Null);
        assert_eq!(r["missing"], Value::Null, "the control: an offer is held");
        let off = Look {
            limit_reset: false,
            ..look(Some(account), &p, Tap::On)
        };
        assert_eq!(
            resets_doc(&off),
            r,
            "as last seen, the limit reset off since"
        );
    }
}

/// A STATE THAT COULD NOT BE READ GIVES THE `resets` FIGURE THE WORD EVERY WINDOW GETS,
/// `state-unread`, in the document and the text: never the word of a state read whole that holds
/// no offer.
#[test]
fn an_unread_state_gives_the_resets_figure_the_windows_word() {
    let p = profile(None);
    let unread = Look {
        state_unread: true,
        ..look(None, &p, Tap::On)
    };
    let d = doc(&unread);
    let five = d["accounts"][0]["windows"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|w| w["id"] == Value::from("five_hour"))
        .cloned()
        .expect("the five-hour window");
    assert_eq!(
        five["missing"],
        json!({"reason": "state-unread"}),
        "the control: a window's word"
    );
    let r = &d["accounts"][0]["resets"];
    assert_eq!(r["missing"], json!({"reason": "state-unread"}));
    assert_eq!((&r["left"], &r["state"]), (&Value::Null, &Value::Null));
    let shown = text(&unread, &view(&unread));
    assert!(
        shown.contains("  resets \u{2014}  the accounts state could not be read\n"),
        "{shown}"
    );
}

/// THE `resets` ROW SAYS THE OFFER AS LAST SEEN IN THE WINDOWS' STYLE: `1 weekly left`, or `2 left`
/// where no kind was named; its use-by instant on the local clock at that instant with the time
/// to it, or `passed` from that instant on; its age, source and machine; and with no offer, never a
/// count: `—  no offer read yet`, or, the harness's limit reset off, that only it records one. It
/// comes after the dollars and before `used`.
#[test]
fn the_resets_row_says_the_offer_as_last_seen() {
    use aterm_phase::ResetKind;
    let p = profile(None);
    let seen = NOW - 3 * DAY;
    let row = |account: Option<&accounts::Account>| -> String {
        let l = look(account, &p, Tap::On);
        let shown = text(&l, &view(&l));
        let lines: Vec<&str> = shown.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with("  resets "))
            .unwrap_or_else(|| panic!("a resets row in {shown}"));
        assert!(lines[at - 1].starts_with("  \u{2248}$ "), "{shown}");
        assert!(lines[at + 1].starts_with("  used "), "{shown}");
        lines[at].to_owned()
    };
    for (account, line) in [
        (
            offered(Some(ResetKind::Weekly), 1, Some(NOV_12), seen, MACHINE),
            "  resets 1 weekly left \u{b7} use by Thu Nov 12 16:40 (in 28d) \u{b7} as of 3d (limit-reset, here)",
        ),
        (
            offered(None, 2, None, seen, MACHINE),
            "  resets 2 left \u{b7} as of 3d (limit-reset, here)",
        ),
        (
            offered(None, 2, Some(NOW + 5 * HOUR), seen, MACHINE),
            "  resets 2 left \u{b7} use by 21:40 (in 5h) \u{b7} as of 3d (limit-reset, here)",
        ),
        (
            offered(Some(ResetKind::Weekly), 1, Some(NOW - DAY), seen, MACHINE),
            "  resets 1 weekly left \u{b7} use by Wed Oct 14 16:40, passed \u{b7} as of 3d (limit-reset, here)",
        ),
        (
            offered(
                Some(ResetKind::Session),
                0,
                None,
                NOW - 2 * HOUR,
                "a1b2c3d4e5f60718",
            ),
            "  resets 0 session left \u{b7} as of 2h (limit-reset, a1b2c3)",
        ),
    ] {
        assert_eq!(row(Some(&account)), line);
    }
    let bare = tapped(425, 710, NOW - MIN);
    for account in [bare.accounts.get("main"), None] {
        assert_eq!(
            row(account),
            "  resets \u{2014}  no offer read yet",
            "{account:?}"
        );
        let off = Look {
            limit_reset: false,
            ..look(account, &p, Tap::On)
        };
        let shown = text(&off, &view(&off));
        assert!(
            shown.contains(
                "  resets \u{2014}  not read: only the harness's limit reset ([harness] limit_reset) records an offer, and it is off\n"
            ),
            "{account:?}: {shown}"
        );
    }
}

/// THE `resets` ROW NAMES THE USE-BY DAY ON THE LOCAL CLOCK AT THAT INSTANT, as every instant in
/// the text is named: across a change of the zone's offset, the day is the one the clock shows at
/// the deadline, not the one it shows now.
#[test]
fn the_resets_rows_use_by_is_on_the_clock_at_that_instant() {
    use aterm_phase::ResetKind;
    let p = profile(None);
    let by = NOV_12 + 14 * HOUR + 50 * MIN;
    let offer = offered(Some(ResetKind::Weekly), 1, Some(by), NOW - DAY, MACHINE);
    let l = look(Some(&offer), &p, Tap::On);
    let at = |t: i64| if t >= NOV_12 { -8 * HOUR } else { -7 * HOUR };
    let shown = text_local(&l, &view(&l), &at);
    assert!(
        shown.contains("use by Thu Nov 12 23:30 (in 28d14h)"),
        "{shown}"
    );
    let now_only = |_: i64| -7 * HOUR;
    let shown = text_local(&l, &view(&l), &now_only);
    assert!(
        shown.contains("use by Fri Nov 13 00:30 (in 28d14h)"),
        "the control, the offset now: {shown}"
    );
}
