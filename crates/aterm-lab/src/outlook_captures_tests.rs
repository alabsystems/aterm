// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A full series ([`full_series`]) is shaped as a compaction leaves one, `day` rows and
/// hourly figures before the raw week, near [`account_series::BOUND_BYTES`] and within it;
/// and the memo's tails hold exactly what the report's whole read holds of it
/// ([`resolver_cost`]'s check). Control: a whole read that skips the outlook's view
/// (`outlook::series_of` without `account_series::outlook_view`) holds the `day` rows no tail
/// holds, and the check fails.
#[test]
fn a_full_series_is_near_the_bound_and_its_tails_read_what_the_whole_read_reads() {
    let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
    let now = 1_792_956_600;
    seed(&world, &plan('A'), now).expect("seeded");
    let roster =
        accounts::Roster::load(&world.cfg.join("aterm").join("accounts.toml")).expect("the roster");
    let dir = harness_state(&world);
    let (state, _) = accounts::Store::at(&dir).load(now).expect("the store");
    full_series(&dir, &roster, &state, now).expect("grown");
    for row in &roster.rows {
        let path = account_series::path_of(&dir, &row.label, &state.machine).expect("a path");
        let text = std::fs::read_to_string(&path).expect("a series");
        let len = text.len() as u64;
        assert!(
            len <= account_series::BOUND_BYTES && len > account_series::BOUND_BYTES - 128 * 1024,
            "{}: {len} bytes",
            row.label
        );
        assert!(text.contains(r#""k":"day""#), "{}: day rows", row.label);
    }
    let cost = resolver_cost(2).expect("the tails' rows are the whole read's");
    assert!(cost.contains("series rows"), "{cost}");
}

/// Every scene seeds the default home's row first and active in the store (`me`, `main` in the
/// glass scenes I to K, and in L `alt1`, whose profile is a login no row names), writes only
/// inside its world, and C's `pool1` is never read; B's and C's `alt1` stand at 92%. Only the
/// card's own scenes L and M give every row an address, a pool file and this Mac its lab name:
/// the file parses and gives the profile's account its home, another Mac in L and this one in
/// M. Control: A reads `pool1`, and A has no pool file.
#[test]
fn the_seeds_are_synthetic_and_stay_in_their_world() {
    use aterm_agent::harness::ai_pool;
    for scene in SCENES {
        let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
        let now = 1_792_956_600;
        let plan = plan(scene);
        let first = match scene {
            'I'..='K' => "main",
            'L' => "alt1",
            _ => "me",
        };
        assert_eq!(plan.rows.first().map(|r| r.0), Some(first), "{scene}");
        assert_eq!(plan.outside, scene == 'L', "{scene}");
        seed(&world, &plan, now).expect("seeded");
        let profile = std::fs::read_to_string(world.home.join(".claude.json")).expect("read");
        assert!(profile.contains(ME_EMAIL), "{scene}: {profile}");
        let roster =
            std::fs::read_to_string(world.cfg.join("aterm").join("accounts.toml")).expect("read");
        let lab = matches!(scene, 'L' | 'M');
        let named = roster.matches('@').count();
        assert_eq!(
            named,
            if lab { plan.rows.len() } else { 1 },
            "{scene}: {roster}"
        );
        assert_eq!(roster.matches("@example.test").count(), named, "{scene}");
        assert_eq!(roster.contains(ME_EMAIL), scene != 'L', "{scene}: {roster}");
        let (state, _) = accounts::Store::at(&harness_state(&world))
            .load(now)
            .expect("the store loads");
        assert_eq!(state.active.as_deref(), Some(first));
        assert_eq!(
            state.swarm_label.as_deref(),
            lab.then_some(THIS_MAC),
            "{scene}"
        );
        let home = ai_pool::read_under(&world.home).map(|pool| {
            ai_pool::home_of_email(&pool, accounts::Cli::Claude, ME_EMAIL)
                .and_then(|account| account.home_mac().map(str::to_owned))
        });
        match scene {
            'L' => assert_eq!(home, Some(Some("m15".into())), "another Mac"),
            'M' => assert_eq!(home, Some(Some(THIS_MAC.into())), "this Mac"),
            _ => assert_eq!(home, None, "{scene}: no pool file"),
        }
        let read = |label: &str| state.accounts.contains_key(label);
        match scene {
            'C' => assert!(!read("pool1"), "C: pool1 is never read"),
            'A' => assert!(read("pool1"), "CONTROL: A reads pool1"),
            'H' => assert!(!read("me"), "H: me has no figure"),
            'K' => assert!(!read("main"), "K: main has no figure"),
            'L' => assert!(!read("me"), "L: no row is the profile's login"),
            _ => {}
        }
        if matches!(scene, 'B' | 'C') {
            let alt1 = &state.accounts["alt1"];
            let week = alt1.windows.get(accounts::WEEK_LIMIT).expect("a week");
            assert_eq!(week.pct_x10, 920, "{scene}");
        }
    }
}

/// The outlook a window reads from `world` at `now`, on UTC, as its memo reads it
/// ([`resolver_cost`]'s reads): the roster, the store, the logins and the series.
fn outlook_of(world: &World, now: i64) -> aterm_agent::harness::outlook::Outlook {
    use aterm_agent::harness::outlook;
    let roster =
        accounts::Roster::load(&world.cfg.join("aterm").join("accounts.toml")).expect("the roster");
    let dir = harness_state(world);
    let store = accounts::Store::at(&dir);
    let (state, _) = store.load(now).expect("the store");
    let enrolled = accounts::enrolled(&roster);
    let renewals = store
        .load_logins(now)
        .map(|(logins, _)| outlook::renewals_of(&roster, &logins))
        .unwrap_or_default();
    let series = outlook::series_of(&dir, &roster, &state.machine, now);
    outlook::outlook(&outlook::Inputs {
        roster: &roster,
        state: &state,
        enrolled: &enrolled,
        series: &series,
        renewals: &renewals,
        mode: accounts::Mode::Early,
        credits: false,
        now,
        offset: |_| 0,
    })
}

/// SCENE M'S SERIES CROSSES A WEEK'S RESET: the readings of the last eight days the outlook
/// holds of `me` are of two weeks, the earlier one's resetting a week before the later one's,
/// so the card's eight-day chart marks where the week began; the newest is the store's own
/// figure. Control: scene A's day of readings is of one week.
#[test]
fn scene_ms_series_crosses_a_weeks_reset() {
    let now = 1_792_956_600;
    let resets = |scene: char| {
        let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
        seed(&world, &plan(scene), now).expect("seeded");
        let outlook = outlook_of(&world, now);
        let reads = &outlook
            .rows
            .iter()
            .find(|row| row.label == "me")
            .expect("me's row")
            .read;
        let mut resets: Vec<i64> = reads.iter().map(|read| read.reset).collect();
        resets.dedup();
        (resets, reads.last().map(|read| read.x10))
    };
    let (m, newest) = resets('M');
    assert_eq!(m, [now + ME_WEEK - 7 * D, now + ME_WEEK]);
    assert_eq!(newest, Some(390));
    assert_eq!(resets('A').0, [now + ME_WEEK], "CONTROL");
}

/// SCENE J'S WORLD IS CASE (C)'S: J is a card scene under Cruise, and seeded at the glass
/// design's now (aterm-agent's `quota_scenes::T5`) the outlook read from its world gives
/// `main`'s pane the quota view `quota_scenes::case_c` does
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1): a projection for the tab's own
/// account, which the card's `Forecast` row says. NEGATIVE CONTROL: scene A's world, seeded at
/// the same moment, gives another view.
#[test]
fn scene_js_world_is_case_cs() {
    use aterm_agent::harness::quota_scenes;
    let plan_j = plan('J');
    assert!(CARD_SCENES.contains(&'J') && plan_j.autopilot == "cruise");
    let now = quota_scenes::T5;
    let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
    seed(&world, &plan_j, now).expect("seeded");
    let ours = outlook_of(&world, now);
    // The world's own row names the profile's address, and an account is named by its
    // email (design §2.5, 2026-11-09), so case (c)'s own row names it too.
    let mut case = quota_scenes::case_c();
    case.roster.rows[0].email = Some(ME_EMAIL.into());
    let case = case.outlook();
    let view = ours.quota("main", None, true);
    assert_eq!(view, case.quota("main", None, true));
    assert!(
        ours.rows
            .iter()
            .find(|row| row.label == "main")
            .and_then(|row| row.window(aterm_agent::harness::outlook::Window::Week))
            .is_some_and(|week| week.projection.is_some()),
        "the tab's account has a projection for the Forecast row"
    );
    let other = World::new(crate::instance::CONFIG_OFF).expect("a world");
    seed(&other, &plan('A'), now).expect("seeded");
    assert_ne!(outlook_of(&other, now).quota("me", None, true), view);
}

/// A CARD IS SHOT ONCE ITS READER AND ITS OBSERVER HAVE ANSWERED: `controls fact-menu`'s
/// words of an open Account card whose `Token`, `Budget` and `Machine` facts are there say it
/// is shown ([`card_shown`]). The same card before the account reader's answer (`Machine`
/// says `not read yet`) does not; nor while the pane's recovery observer is busy, which the
/// `Forecast` fact says over as many rows as its room wraps it to; nor does a closed card or
/// another fact's. [`card_lines`] reads a wrapped fact as one and ends it at a section.
/// NEGATIVE CONTROL: the open card's head alone, all the capture once read, takes none.
#[test]
fn a_card_is_shown_once_its_reader_and_observer_have_answered() {
    let head = "fact-menu open=true session=s-1 fact=account rows=17 selected=7";
    let answers = |machine: &str| {
        format!(
            "{head}\n\
             fact-menu label index=0 text=\"Account \\u{{b7}} tab 1\"\n\
             fact-menu fact index=2 label=\"Token\" tone=value text=\"in the vault\"\n\
             fact-menu fact index=3 label=\"Budget\" tone=value text=\"25% left \\u{{b7}} resets\"\n\
             fact-menu fact index=4 label=\"\" tone=value text=\"Sat 10:00\"\n\
             fact-menu fact index=5 label=\"Machine\" tone=label text=\"{machine}\"\n\
             fact-menu section index=6 text=\"Budget\"\n"
        )
    };
    let shown = answers("home not known \\u{b7} in use here");
    assert_eq!(
        card_lines(&shown),
        [
            CardLine::Fact("Token".into(), "in the vault".into()),
            CardLine::Fact("Budget".into(), "25% left \u{b7} resets Sat 10:00".into()),
            CardLine::Fact("Machine".into(), "home not known \u{b7} in use here".into()),
            CardLine::Section("Budget".into()),
        ]
    );
    assert!(card_shown(&shown), "{shown}");
    let pending = answers("not read yet");
    assert!(!card_shown(&pending), "{pending}");
    let busy = aterm_agent::harness::outlook::Refusal::ObserverBusy.words();
    let (first, rest) = busy.split_once(' ').expect("more than a word");
    let busy = format!(
        "{shown}fact-menu fact index=7 label=\"Forecast\" tone=label text=\"none: {first}\"\n\
         fact-menu fact index=8 label=\"\" tone=label text=\"{rest}\"\n"
    );
    assert!(!card_shown(&busy), "{busy}");
    // A fact with no name under a section is its own, never the fact before the section's.
    let apart = format!("{shown}fact-menu fact index=7 label=\"\" tone=label text=\"x\"\n");
    assert_eq!(
        card_lines(&apart).last(),
        Some(&CardLine::Fact(String::new(), "x".into()))
    );
    for not in [
        head.to_owned(),
        shown.replace("open=true", "open=false"),
        shown.replace("fact=account", "fact=model"),
    ] {
        assert!(!card_shown(&not), "{not}");
    }
}

/// The glass scenes' fixture hands its statusLine `main`'s week as the store holds it: the
/// tap's reading of the seeded payload ([`crate::fleet_fixture::status_payload`]) says the
/// store's figure and reset, so the tap's words, the lead and the chart agree (case (b) 63%,
/// case (c) 75%); case (d) seeds no `rate_limits`, so the tap says `week · no figure yet`.
/// NEGATIVE CONTROL: scene A, whose fixture runs no statusLine, seeds none either.
#[test]
fn the_tapped_scenes_hand_the_tap_the_stores_week() {
    let now = 1_792_956_600;
    let vars = tap::TapVars {
        sid: Some("s-01".into()),
        pid: Some("7".into()),
        ..tap::TapVars::default()
    };
    for (scene, x10) in [('I', Some(630)), ('J', Some(750)), ('K', None), ('A', None)] {
        let world = World::new(crate::instance::CONFIG_OFF).expect("a world");
        let plan = plan(scene);
        assert_eq!(plan.tapped, scene != 'A', "{scene}");
        seed(&world, &plan, now).expect("seeded");
        let seeded =
            std::fs::read_to_string(world.home.join(crate::fleet_fixture::RATE_LIMITS)).ok();
        let payload =
            crate::fleet_fixture::status_payload(Some("s"), &world.home, seeded.as_deref());
        let reading = tap::Reading::of_stdin(&payload, &vars, now).expect("a reading");
        let (store, _) = accounts::Store::at(&harness_state(&world))
            .load(now)
            .expect("the store loads");
        let held = store
            .accounts
            .get(plan.own())
            .and_then(|a| a.windows.get(accounts::WEEK_LIMIT))
            .map(|w| (w.pct_x10, w.reset));
        let tapped = reading.seven_day.map(|w| (w.x10(), w.resets_at));
        assert_eq!(tapped.map(|t| t.0), x10, "{scene}: {payload}");
        if x10.is_some() {
            assert_eq!(tapped, held, "{scene}: the tap's week is the store's");
        } else {
            assert!(seeded.is_none(), "{scene}: no rate_limits seeded");
            assert_eq!(reading.week_words(now, None), tap::NO_WEEK, "{scene}");
        }
    }
}
