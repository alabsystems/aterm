// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the account stats' view and its two renderings ([`super::view`],
//! [`super::text`], [`super::json`]). Every state, profile and spend here is
//! composed in memory, on a fixed clock: invented ids and labels, no file.

use aterm_json::{Value, json};

use super::*;
use crate::harness::accounts::{self, CreditsUse, State, TapRead, Wall, WindowRead};
use crate::harness::spend::{Message, PRICES_TOML, PriceTable};
use crate::harness::usage::TokenUsage;

/// 2026-10-15T16:40:00Z, a Thursday.
const NOW: i64 = 1_792_082_400;
const MIN: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const MACHINE: &str = "3f9a1b2c4d5e6f70";

/// The account id the composed profiles carry (invented).
const UUID: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

/// A composed profile: Max 5x, credits on, Fable included; `usage` its
/// cached `/usage` body, where one is given.
fn profile(usage: Option<Value>) -> Result<Profile, NoProfile> {
    profile_of(UUID, usage)
}

/// [`profile`], of the account whose id is `uuid`.
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

/// The week's reset the composed figures name: Sun Oct 18 20:50 UTC.
const WEEK_END: i64 = 1_792_356_600;

/// What a look at `account` says, with the tap `tap` and no spend.
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
        spend: None,
        prices_as_of: Some("2026-10-15"),
        login: None,
        sessions: Ok(2),
    }
}

/// A state with `main`'s figures as the tap recorded them: five hours at
/// `five` tenths and the week at `week`, read at `at`.
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

/// DAY ONE, THE PACKAGED CONFIG (design "What you get"): the tap off and no
/// `/usage` seen, every empty figure says why — `usage-tap-off` for the two
/// windows, `no-usage-seen` for Fable, beside the plan's own Fable answer —
/// in the text and in JSON. NEGATIVE CONTROLS: with the tap on it is
/// `no-reading`, and for a home the tap does not serve `tap-not-this-home`.
#[test]
fn day_one_every_empty_figure_names_its_reason() {
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
    assert_eq!(
        windows[0]["used_pct"],
        Value::Null,
        "unknown is null, never 0"
    );
    assert_eq!(windows[0]["state"], Value::Null);
    // NEGATIVE CONTROLS: the tap on; another home.
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

/// THE TAP'S FIGURE, TO A TENTH, WITH ITS OWN AS-OF (design §3.3, §7.2): a
/// reading of 42.5% shows `used_pct_x10: 425`, `used_pct: 43`, the reset the
/// vendor named, `source: tap`, this machine, `current`; the text reads
/// `42.5% used · 57.5% left · resets 18:05 (in 1h25m)   as of 1m (tap,
/// here)`. AFTER ITS RESET it is VOID — `window reset 18:05, no reading
/// since`, `used_pct: null`, `missing: window-reset` — never 0%. NEGATIVE
/// CONTROL: a minute before the reset it is still current.
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
    // After the reset: void.
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
    // NEGATIVE CONTROL: a minute before it, current.
    let before = Look {
        now: NOW + 84 * MIN,
        ..l
    };
    assert_eq!(
        doc(&before)["accounts"][0]["windows"][0]["state"],
        Value::from("current")
    );
}

/// A LIMIT RESET USED INSIDE THE WINDOW, NO READING SINCE (design §3.5): the
/// figure held is cut by the epoch — `reset used 16:30, no reading since`,
/// `missing: reset-used`, `epoch_at` named. S5's word that a reset refilled
/// the week cuts a figure the `/usage` cache held from before it. NEGATIVE
/// CONTROLS: with no epoch the same figure is current; S5's refill from
/// before the cache's read leaves the cached figure standing.
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
    // NEGATIVE CONTROL: no epoch, current.
    let plain = tapped(1000, 400, NOW - HOUR);
    let l = look(plain.accounts.get("main"), &p, Tap::On);
    assert_eq!(
        doc(&l)["accounts"][0]["windows"][0]["used_pct"],
        Value::from(100u64)
    );

    // S5's refill word cuts the week the cache held from before it.
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
    // NEGATIVE CONTROL: refilled before the cache was read: the 96% stands.
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

/// `main`'s week as the state holds it: the tap's figure `pct_x10` read at
/// `at` under [`WEEK_END`], and S5's word `said` on the week.
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

/// S5'S WORD THAT A LIMIT RESET WAS USED IS AN EPOCH OF EVERY SOURCE, taken
/// before the merge (design §3.5): the `/usage` cache's week of 98%, fetched
/// two days ago, the tap's 5% read a minute ago with no epoch kept beside
/// it, and a reset used a day ago — the week is the tap's 5%, read after the
/// reset, never `reset used, no reading since`. NEGATIVE CONTROL: the tap's
/// reading from before the reset too, both are cut, and the week says so.
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
    // NEGATIVE CONTROL: read before the reset, the tap's figure is cut too.
    let before = week_said(50, NOW - 3 * DAY, used);
    let l = look(Some(&before), &cached, Tap::On);
    let week = &doc(&l)["accounts"][0]["windows"][1];
    assert_eq!(
        week["missing"]["reason"],
        Value::from("reset-used"),
        "{week}"
    );
}

/// A LIMIT THAT LIFTED BY ITSELF REFILLED NOTHING INSIDE ITS WINDOW
/// ([`accounts::reset_outcome`]'s `Lifted`, design §3.5): its word names the
/// reset that came, and the next window's 20%, read before the outcome was,
/// stands. NEGATIVE CONTROL: the same word of a reset USED (no reset named)
/// cuts it — `reset used`.
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
    // NEGATIVE CONTROL: a reset used.
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

/// THE SAMPLE IS ONE MORE SOURCE ([`accounts::sample_window`], design §3.2):
/// an account that holds only the sample — the `/usage` probe's panel, or a
/// state the tap of an older build wrote — shows its figures, `source:
/// sample`, as of the sample; beside the tap's record, a sample that says
/// another figure (the probe's 45% over the tap's 42.5%) merges by the one
/// rule. NEGATIVE CONTROL: the sample the tap itself wrote, the
/// whole-percent copy of its record, adds nothing — the figure is the tap's,
/// to a tenth.
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
    // NEGATIVE CONTROL: the tap's own copy is no second figure.
    let mut state = tapped(425, 710, NOW - MIN);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let five = win(&view(&l), SESSION_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((five.pct_x10, five.source.as_str()), (425, SOURCE_TAP_WORD));
    // The probe's panel beside it: the larger, from the sample.
    accounts::note_sample(&mut state, "main", sample(45, NOW), NOW);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let five = win(&view(&l), SESSION_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((five.pct_x10, five.source.as_str()), (450, SOURCE_SAMPLE));
}

/// EVERY SOURCE OF A WINDOW, ONE RULE (design §3.3, §3.5): the tap's 40% and
/// the `/usage` cache's 60% under one reset give 60% from the cache (the
/// largest), a wall gives 100% until its reset, and the Fable week comes
/// from the cache's `limits[]` row under its own name — as last seen, two
/// days old, `/usage`. NEGATIVE CONTROL: a cached week under an OLDER reset
/// than the tap's gives way to the tap's newer window.
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
    // A wall: 100% until its reset.
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

/// The tap's source word, as the record keeps it.
const SOURCE_TAP_WORD: &str = accounts::SOURCE_TAP;

/// THE DOLLARS (design §3.6, A5's figures wired in): with a current reading
/// of the week at 50% and $20 of list-price spend before it, the week holds
/// `≥$40` and `≈$20` is left; with no reading the figure is rolling and no
/// capacity is claimed. Credits in use in the window suppress both
/// (`on-credits`). NEGATIVE CONTROL: the same spend with no reading claims
/// no capacity.
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
    // Credits in use in the window: no capacity.
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
    // NEGATIVE CONTROL: no reading, rolling, no capacity.
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

/// What a look at `account` says beside `spend`, with the tap on: the JSON
/// document and the text.
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

/// A RECORD OF ANOTHER ACCOUNT IS NOT SHOWN under the one the profile names
/// ([`Account::of_another`], the rule `note_reading` drops it by): `main`'s
/// record holds account A's figures — the five hours at a wall, the week at
/// 80% with S5's warning of it, credits in use — and the home's profile now
/// names account B, of which no reading has come. The view says B's key and
/// none of A's figures: each window is empty and says `no-reading` (the tap
/// is on, and has read nothing of B), no `100%` is printed, and no window's
/// dollars are on credits. NEGATIVE CONTROLS: the profile naming A shows
/// A's wall and its credits; an unkeyed record (one an older build wrote),
/// of no account the view can tell, is shown.
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
    // NEGATIVE CONTROL: the profile names A, whose record it is.
    let a = profile(None);
    let (d, shown) = shown_with(&spend, &a, state.accounts.get("main"));
    let five = &d["accounts"][0]["windows"][0];
    assert_eq!(
        (&five["used_pct"], &five["source"]),
        (&Value::from(100u64), &Value::from("wall"))
    );
    assert!(on_credits(&d), "{d}");
    assert!(shown.contains("  5h     100% used"), "{shown}");
    // NEGATIVE CONTROL: an unkeyed record says nothing of whose it is.
    let mut unkeyed = state.clone();
    unkeyed.accounts.get_mut("main").expect("held").key = None;
    let (d, _) = shown_with(&spend, &b, unkeyed.accounts.get("main"));
    assert_eq!(
        d["accounts"][0]["windows"][0]["used_pct"],
        Value::from(100u64)
    );
    // Several logins make home-wide spend ambiguous, not the identity of
    // the profile/cache. B's cache is still B's and cannot merge A's wall.
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
        // Positive controls: a keyed record of B, or an unkeyed legacy
        // record, is admitted beside this same profile/cache.
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

/// Account B's id, beside [`UUID`]'s A (invented).
const OTHER: &str = "7d1e4a90-5b2c-4f3e-8a6d-0c9b8e7f6a5d";

/// A composed profile of `uuid`'s account whose cached `/usage` body names
/// `cached_by`'s, fetched at `fetched` (Unix seconds).
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

/// SPEND SINCE ANOTHER ACCOUNT WAS SEEN ON THE HOME IS UNATTRIBUTED (design
/// §3.6): the fold is home-wide, so its messages since a second account was
/// seen there cannot be put on the one the profile names. Account A spent
/// in the home two hours ago; B now signs in.
/// * Before B's first reading — the record is A's (`of_another`) — the
///   rolling week reads `(rolling 7d, unattributed)`, and JSON's
///   `attribution` is `unattributed`.
/// * After it — the record is B's, its key changed an hour ago
///   (`keyed_at`), B's week at 10% — the week's capacity and `left` read
///   `— unattributed` (`suppressed`), never `≥` A's dollars over B's
///   reading. So they do where B's record never changed key but the cached
///   `/usage` body names A, fetched inside the week.
///
/// NEGATIVE CONTROLS: the key changed, or A's cache was fetched, before
/// every window began — `assumed`, and the capacity is claimed; the profile
/// naming A, whose record it is — `assumed`.
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
    // Before B's first reading: the record is A's.
    let mut a_state = tapped(100, 500, NOW - MIN);
    a_state.accounts.get_mut("main").expect("held").key = a_key;
    let (d, shown) = shown_with(&spend, &b, a_state.accounts.get("main"));
    assert_eq!(attribution(&d), Value::from("unattributed"), "{d}");
    assert!(shown.contains("(rolling 7d, unattributed)"), "{shown}");
    // NEGATIVE CONTROL: the profile names A, whose record it is.
    let a = profile(None);
    let (d, shown) = shown_with(&spend, &a, a_state.accounts.get("main"));
    assert_eq!(attribution(&d), Value::from("assumed"), "{d}");
    assert!(shown.contains(" of \u{2265}"), "{shown}");

    // After it: B's record, its key changed an hour ago.
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
    // NEGATIVE CONTROL: the key changed before every window began.
    let (figure, d, shown) = week(&b, &keyed(Some(NOW - 8 * DAY)));
    assert!(figure.capacity.is_ok(), "{figure:?}");
    assert_eq!(attribution(&d), Value::from("assumed"));
    assert!(shown.contains(" of \u{2265}"), "{shown}");

    // A cache of A's, fetched inside the week, on B's unchanged record.
    let cached = profile_cached_by(OTHER, UUID, NOW - 2 * DAY);
    let (figure, d, _) = week(&cached, &keyed(None));
    assert_eq!(figure.capacity.err(), Some(spend::Why::Unattributed));
    assert_eq!(attribution(&d), Value::from("unattributed"));
    // NEGATIVE CONTROL: fetched before every window began.
    let before = profile_cached_by(OTHER, UUID, NOW - 8 * DAY);
    let (figure, d, _) = week(&before, &keyed(None));
    assert!(figure.capacity.is_ok(), "{figure:?}");
    assert_eq!(attribution(&d), Value::from("assumed"));
}

/// `aterm harness accounts` READS THE KEY RULE TOO ([`held_windows`] with the
/// key of the account the default home's profile names): A's record — a
/// wall on the five hours, the week at 80% — under B's key shows nothing,
/// each window empty and `no-reading`, as `accounts stats` says at the same
/// moment. NEGATIVE CONTROLS: under A's key, A's wall (100%); with no key
/// read, the record is shown.
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
    // The stats view of the same moment.
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
    // NEGATIVE CONTROLS: A's own key; no key read.
    for key in [profile::AccountKey::claude(UUID), None] {
        assert_eq!(shown(key), [(Some(1000), None), (Some(800), None)]);
    }
}

/// WHO USES IT AND ITS LOGIN (design §3.3): the sessions on the home now and
/// S7's word, in the text and JSON; a census that could not be read says
/// so, never `0`. NEGATIVE CONTROL: with no renewable row no login is said.
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

/// The text's words for spans, instants and dollars.
#[test]
fn the_texts_spans_instants_and_dollars() {
    assert_eq!(span(40), "40s");
    assert_eq!(span(12 * MIN), "12m");
    assert_eq!(span(85 * MIN), "1h25m");
    assert_eq!(span(2 * HOUR), "2h");
    assert_eq!(span(3 * DAY + 4 * HOUR + 10), "3d4h");
    assert_eq!(clock(NOW + 85 * MIN, NOW, 0), "18:05");
    assert_eq!(clock(WEEK_END, NOW, 0), "Sun Oct 18 20:50");
    assert_eq!(
        clock(NOW + 85 * MIN, NOW, -7 * HOUR),
        "11:05",
        "the local clock"
    );
    assert_eq!(usd(4.214), "$4.21");
    assert_eq!(usd(1890.2), "$1,890");
    assert_eq!(usd(1_234_567.0), "$1,234,567");
}

/// A RESET USED CLEARS ITS WALL'S 100% (design §3.5;
/// [`accounts::reset_outcome`]): the week walled at 100% (the tap's 90%
/// from before it) and aterm's own limit reset claimed and used on it, the
/// wall's slot keeps a cleared record — no wall — so the week says `reset
/// used`, never 100%, and a reading after it (3% under the same reset) is
/// the week. NEGATIVE CONTROL: the wall standing is the week's 100%.
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
    // NEGATIVE CONTROL: the wall standing.
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

/// A PER-MODEL WEEK'S DOLLARS ARE ITS OWN MODELS' (design §3.6): an Opus
/// message's dollars count in the week and in the Opus week, never in the
/// Sonnet week, whose figure the cached `/usage` body holds; a week of a
/// model no family names (`seven_day_other`, `Haiku`) has no dollars at
/// all. NEGATIVE CONTROL: the week sums every model's.
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

/// EACH WEEK OF A MODEL NO BUCKET NAMES IS ITS OWN WINDOW (design §7.2,
/// `seven_day_other` with its name in `name`): the cached `/usage` body's
/// `Sonnet` row at 10% and `Haiku` row at 85%, under one reset, are two
/// windows, each with its own name and figure — never one window named
/// `Sonnet` that holds Haiku's 85%.
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

/// A REFILL THE TAP SAW CUTS EVERY SOURCE FROM BEFORE IT (design §3.5): the
/// week warned at 97% and walled at 100%, then a limit reset used in a tab
/// (no outcome of aterm's recorded: S5's warning and the wall stay) and the
/// same process's next reading at 2% — the view's week is the 2%, never the
/// warning's 97% or the wall's 100%. NEGATIVE CONTROL: with no refill seen,
/// the wall's 100% stands.
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
    // NEGATIVE CONTROL.
    let state = walled(None);
    let l = look(state.accounts.get("main"), &p, Tap::On);
    let week = win(&view(&l), WEEK_LIMIT)
        .current(NOW)
        .cloned()
        .expect("current");
    assert_eq!((week.pct_x10, week.source.as_str()), (1000, SOURCE_WALL));
}
