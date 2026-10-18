// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Harness's page (`native_settings_harness.rs`), rendered through
//! the Settings controller and read back from the compiled semantic tree —
//! what `controls prefs` and a screen reader read.

use super::*;
use crate::autopilot::{AutopilotView, HarnessPosture};
use crate::harness_screen::{
    AbArmTotals, AbTotals, HarnessSession, HarnessStatsReport, HarnessStatsSlot, config_rows,
};
use crate::native_settings::harness_page::{
    HARNESS_CONFIG_TOGGLE, HARNESS_OPEN_CONFIG, HARNESS_REFRESH, harness_page_rows,
};
use aterm_agent::harness::stats::HarnessStats;
use aterm_agent::supervise::{Autopilot, SupervisorConfig};

/// A harness snapshot as the host publishes one: one supervised Claude Code
/// session on Land by its own word, a reading of the ledgers and a later one
/// that failed, A/B time in every arm, and a policy that writes `approve =
/// "safe"`.
fn fixture() -> HarnessState {
    let mut today = HarnessStats::new(0);
    today.approved = 3;
    today.answered = 1;
    today.rows = 4;
    today.human_wait_ms = 125_000;
    let mut week = HarnessStats::new(0);
    week.approved = 41;
    week.answered = 6;
    week.continued = 9;
    week.rows = 60;
    week.human_wait_ms = 7_500_000;
    week.cut = true;
    let mut policy = SupervisorConfig::default();
    policy.set("approve", "safe").expect("a known key");
    HarnessState {
        observed: true,
        words: "Automatic \u{b7} limited: approve: safe".to_string(),
        posture: HarnessPosture::On(Autopilot::Cruise),
        sessions: vec![HarnessSession {
            local_id: 7,
            sid: "s-7".to_string(),
            title: "fix the build".to_string(),
            place: "in tab 2".to_string(),
            program: "Claude Code",
            word: Some(Autopilot::Land),
            view: AutopilotView::Land,
            default: Some(Autopilot::Cruise),
            supervised: true,
            glass: crate::claude_lights::ChipGlass::default(),
            armed_at_ms: None,
        }],
        stats: HarnessStatsSlot {
            report: Some(std::sync::Arc::new(HarnessStatsReport::new(
                today,
                week,
                1_000_000_000,
                Some(0),
                &|_| Some(0),
            ))),
            collecting: false,
            unavailable: Some("the ledgers' reader failed".to_string()),
            stale: false,
        },
        ab: AbTotals {
            cruise: AbArmTotals {
                busy_ms: 97_000,
                waiting_ms: 3_000,
            },
            land: AbArmTotals {
                busy_ms: 60_000,
                waiting_ms: 3_540_000,
            },
            off: AbArmTotals {
                busy_ms: 59_000,
                waiting_ms: 41_000,
            },
            unsupervised: AbArmTotals {
                busy_ms: 30_000,
                waiting_ms: 30_000,
            },
        },
        config: config_rows(&policy),
        land_at_offer: None,
        ai: crate::ai_screen::AiState::default(),
        today: 0,
    }
}

/// A Settings controller holding `state`, its view on Settings ▸ Harness.
fn harness_route(
    state: HarnessState,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    harness_route_over(state, None)
}

/// [`harness_route`] over an aterm.toml `source`, where one is given.
fn harness_route_over(
    state: HarnessState,
    source: Option<&str>,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let (mut runtime, instance, view) = setup();
    if let Some(source) = source {
        replace_settings_source(&mut runtime, view, source.to_string());
    }
    assert!(runtime.replace_settings_harness(state, 1));
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Harness),
                value: None,
            }),
        )
        .unwrap();
    // The route's arrival asks the host to read the ledgers again.
    assert!(
        outcome.effects.iter().any(|effect| matches!(
            effect,
            AppEffect::Harness(crate::native_app::HarnessRequest::Refresh)
        )),
        "{:?}",
        outcome.effects
    );
    (runtime, instance, view)
}

/// Every page of the route at `cx`, compiled: page 0, then each page to the
/// bound the render recorded.
fn every_page(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
) -> Vec<crate::native_ui::CompiledUi> {
    let mut pages = Vec::new();
    let mut page = 0;
    loop {
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.page_scroll = page;
        }
        pages.push(compile_settings_view(runtime, instance, view, cx));
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        let limit = state
            .result_page_limit
            .get()
            .expect("the page records its bound");
        if page >= limit {
            return pages;
        }
        page += 1;
    }
}

/// The semantic node `key` on any of `pages`.
fn node<'a>(
    pages: &'a [crate::native_ui::CompiledUi],
    key: &str,
) -> &'a crate::native_ui::SemanticNode {
    pages
        .iter()
        .find_map(|page| page.semantic(&UiKey::new(key)))
        .unwrap_or_else(|| panic!("{key} is on no page"))
}

/// THE ROUTE IS ITS SPECIAL PAGE (§4): the heading, the rail's own word; the
/// STATUS card with the policy's words (no second version string: About
/// owns the build's), and the Refresh and Open aterm.toml
/// actions; the five registry rows as their ordinary `settings/control/<key>`
/// controls; the session's row with its three buttons, the mode it runs
/// under selected; the WHAT THE HARNESS DID card from the fixture's counts,
/// the person's wait in hours and minutes and the cut said; the A/B card with
/// both arms and the sentence that sets them side by side; and the effective
/// table, `default` or `yours` by key. Every page of it whole on a wide, a
/// medium and a compact host.
#[test]
fn the_harness_route_renders_its_cards_and_controls() {
    let (mut runtime, instance, view) = harness_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "settings/page-heading/coding-agent-harness").label,
        "Coding agent harness"
    );
    assert_eq!(
        node(&pages, "harness/status/row/supervision").label,
        "Supervision: Automatic \u{b7} limited: approve: safe"
    );
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/status/row/version"))
            .is_none()),
        "the build's version is About's"
    );
    assert_eq!(
        node(&pages, "harness/status/row/sessions").label,
        "Sessions: 1 of 1 supervised"
    );
    for key in [HARNESS_REFRESH, HARNESS_OPEN_CONFIG] {
        assert_eq!(
            node(&pages, key).action.as_ref().map(ActionId::as_str),
            Some(key)
        );
    }
    for key in harness_page_rows() {
        let control = node(&pages, &format!("settings/control/{key}"));
        assert_eq!(
            control.action.as_ref().map(ActionId::as_str),
            Some(format!("settings/set/{key}").as_str()),
            "{key} is the ordinary registry control"
        );
    }
    // Every agent's own switch is a row of the card, after the master switch
    // (docs/DESIGN-harness-plugins-2026-10-15.md §4).
    let rows = harness_page_rows().collect::<Vec<_>>();
    assert_eq!(
        rows[..6],
        [
            prefs::EDIT_HARNESS_ENABLED,
            prefs::EDIT_HARNESS_CLAUDE_ENABLED,
            prefs::EDIT_HARNESS_CODEX_ENABLED,
            prefs::EDIT_HARNESS_AGY_ENABLED,
            prefs::EDIT_HARNESS_MUSE_ENABLED,
            prefs::EDIT_HARNESS_GEMINI_ENABLED,
        ],
        "{rows:?}"
    );
    assert!(
        rows.contains(&prefs::EDIT_HARNESS_QUESTION_PRIORITY),
        "{rows:?}"
    );
    // The session: its title and detail whole in the row's semantics, and
    // the three buttons, Land selected; the default's (Cruise) says a press
    // follows the default.
    assert_eq!(
        node(&pages, "harness/session/7").label,
        "fix the build: Tab 2 \u{b7} Claude Code \u{b7} supervised \u{b7} autopilot land \
         \u{b7} its own setting"
    );
    for (word, selected, default) in [
        ("cruise", false, true),
        ("land", true, false),
        ("off", false, false),
    ] {
        let key = format!("harness/session/7/autopilot/{word}");
        let button = node(&pages, &key);
        assert_eq!(
            button.action.as_ref().map(ActionId::as_str),
            Some(key.as_str())
        );
        let state = button.state.expect("a control");
        assert!(state.enabled, "{key}");
        assert_eq!(state.selected, selected, "{key}");
        assert_eq!(button.label, format!("Autopilot {word} for fix the build"));
        assert_eq!(
            button
                .description
                .as_deref()
                .is_some_and(|d| d.ends_with("the default, so this follows [harness] autopilot")),
            default,
            "{key}: {:?}",
            button.description
        );
    }
    // What the harness did.
    assert_eq!(
        node(&pages, "harness/did/row/approved").label,
        "Approved: 3 today \u{b7} 41 in 7 days"
    );
    assert_eq!(
        node(&pages, "harness/did/row/waited-on-you").label,
        "Waited on you: 2m today \u{b7} 2h 5m in 7 days"
    );
    assert_eq!(
        node(&pages, "harness/did/row/partial").label,
        "Partial: only the last 1024 actions are kept"
    );
    // The reading's clock time (unix 1,000,000 s is 13:46:40 UTC), never a
    // relative age a page nobody republishes would leave stale.
    assert_eq!(
        node(&pages, "harness/did/row/checked").label,
        "Checked: at 13:46"
    );
    // The reading after it failed: said beside the counts it leaves standing.
    assert_eq!(
        node(&pages, "harness/did/row/last-check").label,
        "Last check: failed: the ledgers' reader failed"
    );
    // With and without: each state's share, and Cruise beside Off.
    assert_eq!(
        node(&pages, "harness/ab/row/cruise").label,
        "Cruise: waited for input 3% of the time \u{b7} 1m measured"
    );
    assert_eq!(
        node(&pages, "harness/ab/row/land").label,
        "Land: waited for input 98% of the time \u{b7} 1h measured"
    );
    assert_eq!(
        node(&pages, "harness/ab/row/unsupervised").label,
        "Unsupervised: waited for input 50% of the time \u{b7} 1m measured"
    );
    assert_eq!(
        node(&pages, "harness/ab/row/this-run").label,
        "This run: agents waited for input 3% of the time on Cruise vs 41% with the harness off"
    );
    assert_eq!(
        node(&pages, "harness/ab/row/finished").label,
        "Finished: an agent done with its work still waits for input; each share counts that wait"
    );
    // The effective table: the person's own key and how many more are at
    // their defaults; its disclosure opens every key.
    assert_eq!(
        node(&pages, "harness/config/row/approve").label,
        "approve: safe \u{b7} yours"
    );
    let keys = config_rows(&SupervisorConfig::default()).len();
    assert_eq!(
        node(&pages, "harness/config/row/defaults").label,
        format!("Defaults: {} more settings", keys - 1)
    );
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/config/row/enabled"))
            .is_none()),
        "a default key waits for the disclosure"
    );
    let toggle = node(&pages, HARNESS_CONFIG_TOGGLE);
    assert_eq!(toggle.label, format!("Show all {keys} settings"));
    assert_eq!(toggle.state.and_then(|state| state.expanded), Some(false));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(HARNESS_CONFIG_TOGGLE),
                value: None,
            }),
        )
        .unwrap();
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/config/row/enabled").label,
        "enabled: true \u{b7} default"
    );
    assert_eq!(
        node(&pages, "harness/config/row/rules-file").label,
        "rules_file: (none) \u{b7} default"
    );
    let toggle = node(&pages, HARNESS_CONFIG_TOGGLE);
    assert_eq!(toggle.state.and_then(|state| state.expanded), Some(true));
    assert_eq!(toggle.label, "Show only the settings you changed");

    // Whole on every page, at three hosts.
    for (width, height) in [(1_200.0, 820.0), (900.0, 600.0), (320.0, 568.0)] {
        let cx = view_cx_at(width, height);
        for (page, compiled) in every_page(&mut runtime, instance, view, &cx)
            .iter()
            .enumerate()
        {
            let context = format!("{width}\u{d7}{height} page {page}");
            assert_zero_top_paint(compiled, &context);
            assert_all_hits_material_and_contained(compiled, &context);
        }
    }
}

/// THE QUIET STATES: no agent session open says so, once (the SESSIONS
/// card, not STATUS too); an A/B arm with no time
/// says "not measured yet", and no side-by-side sentence nor the finished
/// agent's caveat is drawn; an
/// unread page says it is reading. Where the harness runs no mode (the
/// master switch off), a session's three buttons are inert and none is
/// selected.
#[test]
fn the_harness_route_says_what_it_has_not_got() {
    let mut empty = fixture();
    empty.sessions.clear();
    empty.ab = AbTotals::default();
    let (mut runtime, instance, view) = harness_route(empty);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/sessions/empty").label,
        "No Claude Code, Codex, Antigravity CLI, Muse Code or Gemini CLI session is open"
    );
    // The SESSIONS card says it; STATUS does not say it again.
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/status/row/sessions"))
            .is_none()),
        "no session open is said once, by the SESSIONS card"
    );
    for arm in ["cruise", "land", "off"] {
        assert!(
            node(&pages, &format!("harness/ab/row/{arm}"))
                .label
                .ends_with(": not measured yet"),
            "{arm}"
        );
    }
    for row in ["this-run", "finished"] {
        assert!(
            pages.iter().all(|page| page
                .semantic(&UiKey::new(format!("harness/ab/row/{row}")))
                .is_none()),
            "{row}: no side by side without both arms, no caveat without time"
        );
    }

    let (mut runtime, instance, view) = harness_route(HarnessState::unobserved());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/status/row/supervision").label,
        "Supervision: reading\u{2026}"
    );
    assert_eq!(
        node(&pages, "harness/did/row/activity").label,
        "Activity: reading\u{2026}"
    );

    let mut off = fixture();
    off.posture = HarnessPosture::MasterOff;
    off.sessions[0].view = AutopilotView::MasterOff;
    off.sessions[0].default = None;
    let (mut runtime, instance, view) = harness_route(off);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    for word in ["cruise", "land", "off"] {
        let state = node(&pages, &format!("harness/session/7/autopilot/{word}"))
            .state
            .expect("a control");
        assert!(!state.enabled && !state.selected, "{word}");
    }
}

/// THE PRESSES: a session's Land asks the host for that word, typed; a press
/// for a session the snapshot no longer lists, or under the master switch
/// off, asks for nothing and says why; Refresh asks for a reading; Open
/// aterm.toml is the Manual route's own act at the `[harness]` table, and the
/// page stays.
#[test]
fn the_harness_routes_presses_are_typed_host_requests() {
    let press = |runtime: &mut NativeRuntime,
                 instance: crate::native_app::AppInstanceId,
                 view: crate::native_app::ViewId,
                 action: &str| {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap()
            .effects
    };
    let harness = |effects: &[AppEffect]| {
        effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::Harness(request) => Some(*request),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (mut runtime, instance, view) = harness_route(fixture());
    let effects = press(
        &mut runtime,
        instance,
        view,
        "harness/session/7/autopilot/land",
    );
    assert_eq!(
        harness(&effects),
        [crate::native_app::HarnessRequest::Autopilot {
            session: 7,
            word: Autopilot::Land
        }]
    );
    let effects = press(
        &mut runtime,
        instance,
        view,
        "harness/session/8/autopilot/off",
    );
    assert!(harness(&effects).is_empty(), "no such session");
    let effects = press(&mut runtime, instance, view, HARNESS_REFRESH);
    assert_eq!(
        harness(&effects),
        [crate::native_app::HarnessRequest::Refresh]
    );
    let effects = press(&mut runtime, instance, view, HARNESS_OPEN_CONFIG);
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            AppEffect::OpenConfigEditor {
                target: Some(ConfigEditorTarget::Key(key)),
                ..
            } if key == crate::prefs::EDIT_HARNESS_ENABLED
        )),
        "{effects:?}"
    );
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(state.route, SettingsRoute::Harness, "the page stays");

    let mut off = fixture();
    off.posture = HarnessPosture::MasterOff;
    off.sessions[0].view = AutopilotView::MasterOff;
    off.sessions[0].default = None;
    let (mut runtime, instance, view) = harness_route(off);
    let effects = press(
        &mut runtime,
        instance,
        view,
        "harness/session/7/autopilot/cruise",
    );
    assert!(harness(&effects).is_empty(), "the master switch is off");
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(
        state.feedback.as_deref(),
        Some("The harness is off here, so Autopilot has nothing to steer.")
    );
}

/// THE SESSION ROW'S STATE LINE AND TIMER (§9.3.4): an armed timer reads
/// `lands at 18:00` under the row's detail (clock times only); the row's
/// `Timer` disclosure opens `1 h`, `2 h`, `4 h`, `at 18:00` (with `[harness]
/// land_at` set) and `Cancel` (while armed), each a typed host request
/// (`HarnessTimerRequest`), and closes again. NEGATIVE CONTROLS: with no
/// `land_at` there is no `at` button and with no timer armed no `Cancel`; a
/// press for a session the snapshot no longer lists, or under the master
/// switch, asks for nothing and says why; a duration the page does not offer
/// is no timer press at all.
#[test]
fn a_sessions_timer_disclosure_arms_and_cancels_its_timed_cruise() {
    use crate::autopilot_timers::TimerChoice;
    use crate::native_app::HarnessTimerRequest;
    let press = |runtime: &mut NativeRuntime,
                 instance: crate::native_app::AppInstanceId,
                 view: crate::native_app::ViewId,
                 action: &str| {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap()
            .effects
    };
    let timers = |effects: &[AppEffect]| {
        effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::HarnessTimer(request) => Some(*request),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut armed = fixture();
    let mut policy = SupervisorConfig::default();
    policy.set("land_at", "18:00").expect("a known key");
    armed.config = config_rows(&policy);
    armed.land_at_offer = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    armed.sessions[0].word = Some(Autopilot::Cruise);
    armed.sessions[0].view = AutopilotView::Cruise;
    armed.sessions[0].armed_at_ms = Some(1_792_173_600_000);
    armed.sessions[0].glass.land_at = Some(crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    });
    let (mut runtime, instance, view) = harness_route(armed);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/7/state").label,
        "lands at 18:00"
    );
    let toggle = node(&pages, "harness/session/7/timer");
    assert_eq!(toggle.label, "Timer for fix the build");
    assert_eq!(toggle.state.and_then(|s| s.expanded), Some(false));
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/session/7/land-in/60"))
            .is_none()),
        "closed: no timer buttons"
    );
    press(&mut runtime, instance, view, "harness/session/7/timer");
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/7/timer")
            .state
            .and_then(|s| s.expanded),
        Some(true)
    );
    for (key, label, choice) in [
        (
            "land-in/60",
            "Cruise for 1 hour, then land",
            TimerChoice::In(60),
        ),
        (
            "land-in/120",
            "Cruise for 2 hours, then land",
            TimerChoice::In(120),
        ),
        (
            "land-in/240",
            "Cruise for 4 hours, then land",
            TimerChoice::In(240),
        ),
        ("land-at", "Cruise until 18:00, then land", TimerChoice::At),
        (
            "land-cancel",
            "Cancel the landing time",
            TimerChoice::Cancel,
        ),
    ] {
        let action = format!("harness/session/7/{key}");
        let button = node(&pages, &action);
        assert_eq!(button.label, label, "{key}");
        assert_eq!(
            button.action.as_ref().map(ActionId::as_str),
            Some(action.as_str())
        );
        assert!(button.state.expect("a control").enabled, "{key}");
        let effects = press(&mut runtime, instance, view, &action);
        assert_eq!(
            timers(&effects),
            [HarnessTimerRequest { session: 7, choice }],
            "{key}"
        );
    }
    for gone in [
        "harness/session/8/land-in/60",
        "harness/session/7/land-in/90",
    ] {
        let effects = press(&mut runtime, instance, view, gone);
        assert!(timers(&effects).is_empty(), "{gone}");
    }
    // Whole on every host with the disclosure open.
    for (width, height) in [(1_200.0, 820.0), (900.0, 600.0), (320.0, 568.0)] {
        let cx = view_cx_at(width, height);
        for (page, compiled) in every_page(&mut runtime, instance, view, &cx)
            .iter()
            .enumerate()
        {
            let context = format!("{width}\u{d7}{height} page {page} timer open");
            assert_zero_top_paint(compiled, &context);
            assert_all_hits_material_and_contained(compiled, &context);
        }
    }
    press(&mut runtime, instance, view, "harness/session/7/timer");
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/7/timer")
            .state
            .and_then(|s| s.expanded),
        Some(false),
        "closed again"
    );

    // No `land_at`, no timer armed: no `at`, no `Cancel`; no state line.
    let (mut runtime, instance, view) = harness_route(fixture());
    press(&mut runtime, instance, view, "harness/session/7/timer");
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    for key in ["land-at", "land-cancel", "state"] {
        assert!(
            pages.iter().all(|page| page
                .semantic(&UiKey::new(format!("harness/session/7/{key}")))
                .is_none()),
            "{key}"
        );
    }
    assert!(
        node(&pages, "harness/session/7/land-in/60")
            .state
            .unwrap()
            .enabled
    );

    // Under the master switch: inert, and a press says why.
    let mut off = fixture();
    off.posture = HarnessPosture::MasterOff;
    off.sessions[0].view = AutopilotView::MasterOff;
    off.sessions[0].default = None;
    let (mut runtime, instance, view) = harness_route(off);
    let effects = press(&mut runtime, instance, view, "harness/session/7/land-in/60");
    assert!(timers(&effects).is_empty(), "the master switch is off");
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(
        state.feedback.as_deref(),
        Some("The harness is off here, so Autopilot has nothing to steer.")
    );
}

/// EACH SESSION'S BUTTONS BY ITS OWN POSTURE: with Codex's switch Off (the
/// window's posture `Restricted`), a Codex session's Cruise, Land and Off are
/// drawn inert, nothing selected, and a press of one asks the host for
/// nothing and names the switch; a Claude Code session beside it — its
/// agent admitted — stays live, its mode selected, and its Land asks for the
/// word. Judged by the window's posture alone, the Codex row looked live and
/// every press on either row was refused as "the harness is off here".
#[test]
fn a_sessions_buttons_follow_its_agents_own_switch() {
    let mut state = fixture();
    state.posture = HarnessPosture::Restricted(
        Autopilot::Cruise,
        crate::harness_host::agent_bit(aterm_phase::Program::Claude),
    );
    let mut codex = state.sessions[0].clone();
    codex.local_id = 9;
    codex.title = "review".to_string();
    codex.program = "Codex";
    codex.word = None;
    codex.view = AutopilotView::MasterOff;
    codex.default = None;
    codex.supervised = false;
    state.sessions.push(codex);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    for word in ["cruise", "land", "off"] {
        let inert = node(&pages, &format!("harness/session/9/autopilot/{word}"))
            .state
            .expect("a control");
        assert!(!inert.enabled && !inert.selected, "codex {word}");
        let live = node(&pages, &format!("harness/session/7/autopilot/{word}"))
            .state
            .expect("a control");
        assert!(live.enabled, "claude {word}");
        assert_eq!(live.selected, word == "land", "claude {word}");
    }
    let mut press = |action: &str| {
        let effects = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap()
            .effects;
        let requests: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::Harness(request) => Some(*request),
                _ => None,
            })
            .collect();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        (requests, state.feedback.clone())
    };
    assert_eq!(
        press("harness/session/7/autopilot/land").0,
        [crate::native_app::HarnessRequest::Autopilot {
            session: 7,
            word: Autopilot::Land
        }]
    );
    let (requests, feedback) = press("harness/session/9/autopilot/off");
    assert!(requests.is_empty(), "{requests:?}");
    assert_eq!(
        feedback.as_deref(),
        Some("Watch codex-alab sessions is Off, so Autopilot has nothing to steer there.")
    );
}

/// THE TIMER BY ITS SESSION'S OWN POSTURE, as the mode buttons (above): with
/// Codex's switch Off, a Codex row's `Timer` is drawn inert and its `1 h`
/// asks the host for nothing and names the switch; the Claude Code row
/// beside it stays live and its `1 h` is a typed timer request.
#[test]
fn a_sessions_timer_follows_its_agents_own_switch() {
    use crate::autopilot_timers::TimerChoice;
    use crate::native_app::HarnessTimerRequest;
    let mut state = fixture();
    state.posture = HarnessPosture::Restricted(
        Autopilot::Cruise,
        crate::harness_host::agent_bit(aterm_phase::Program::Claude),
    );
    let mut codex = state.sessions[0].clone();
    codex.local_id = 9;
    codex.title = "review".to_string();
    codex.program = "Codex";
    codex.word = None;
    codex.view = AutopilotView::MasterOff;
    codex.default = None;
    codex.supervised = false;
    state.sessions.push(codex);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        !node(&pages, "harness/session/9/timer")
            .state
            .expect("a control")
            .enabled
    );
    assert!(
        node(&pages, "harness/session/7/timer")
            .state
            .expect("a control")
            .enabled
    );
    let mut press = |action: &str| {
        let effects = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap()
            .effects;
        let requests: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::HarnessTimer(request) => Some(*request),
                _ => None,
            })
            .collect();
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        (requests, state.feedback.clone())
    };
    assert_eq!(
        press("harness/session/7/land-in/60").0,
        [HarnessTimerRequest {
            session: 7,
            choice: TimerChoice::In(60)
        }]
    );
    let (requests, feedback) = press("harness/session/9/land-in/60");
    assert!(requests.is_empty(), "{requests:?}");
    assert_eq!(
        feedback.as_deref(),
        Some("Watch codex-alab sessions is Off, so Autopilot has nothing to steer there.")
    );
}

/// AN AGENT SWITCHED OFF BY ITS OWN TABLE IS ON THE EFFECTIVE TABLE: with
/// only `[harness.codex] enabled = false` written, the SETTINGS IN
/// ATERM.TOML card lists `codex.enabled: false · yours` and counts the rest
/// as more defaults — never "all … settings" at their defaults while STATUS
/// says a written setting limits supervision. NEGATIVE CONTROL: Claude
/// Code's switch, at its default, waits for the disclosure.
#[test]
fn an_agent_switched_off_is_on_the_effective_table() {
    let policy = crate::app_config::HarnessPolicy::read("[harness.codex]\nenabled = false\n");
    let mut state = fixture();
    state.words = policy.words();
    state.config = config_rows(&policy.policy);
    let total = state.config.len();
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/config/row/codex-enabled").label,
        "codex.enabled: false \u{b7} yours"
    );
    assert_eq!(
        node(&pages, "harness/config/row/defaults").label,
        format!("Defaults: {} more settings", total - 1)
    );
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/config/row/claude-enabled"))
            .is_none()),
        "a default agent's switch waits for the disclosure"
    );
    assert_eq!(
        node(&pages, HARNESS_CONFIG_TOGGLE).label,
        format!("Show all {total} settings")
    );
    assert!(
        node(&pages, "harness/status/row/supervision")
            .label
            .contains("limited by Watch codex-alab sessions set to Off"),
        "{}",
        node(&pages, "harness/status/row/supervision").label
    );
}

/// THE PAGE IS WHOLE ON EVERY HOST it is drawn for: every page of it, at
/// every Dynamic Type rung, from the phone page and the 80×24 window's short
/// landscape (its pager beside the cards) to a roomy desktop, the effective
/// table closed and open — no text of the page's overflows, nothing is
/// materially clipped, and every hit target is material and inside the
/// window. (The rail's own labels are the rail's tests'.) Where the cards
/// outrun one page the shared pager says so, and its `n/total` stays whole
/// past nine pages.
#[test]
fn the_harness_route_is_whole_on_every_host() {
    let hosts = [320.0, 584.0, 849.0, 1_200.0]
        .into_iter()
        .flat_map(|w| [348.0, 513.0, 700.0, 900.0].map(|h| (w, h, false)))
        .chain([
            (849.0, 513.0, true),
            (584.0, 348.0, true),
            (320.0, 568.0, true),
        ]);
    let hosts: Vec<(f32, f32, bool)> = hosts.collect();
    at_each_text_scale(&[1.0, 1.25, 2.0], "Harness page", |scale| {
        let (mut runtime, instance, view) = harness_route(fixture());
        for &(width, height, open) in &hosts {
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.harness_config_open = open;
            }
            let cx = view_cx_at(width, height);
            for (page, compiled) in every_page(&mut runtime, instance, view, &cx)
                .iter()
                .enumerate()
            {
                let context =
                    format!("{scale}\u{d7} {width}\u{d7}{height} open={open} page {page}");
                compiled
                    .validate_parity()
                    .unwrap_or_else(|error| panic!("{context}: {error:?}"));
                let failures: Vec<String> = compiled
                    .fit_failures()
                    .into_iter()
                    .filter(|line| !line.contains("key=\"settings/nav/"))
                    .collect();
                assert!(
                    failures.is_empty(),
                    "{context}: text overflow or clip:\n{}",
                    failures.join("\n")
                );
                assert_all_hits_material_and_contained(compiled, &context);
            }
        }
    });
}

/// THE DEFAULT AUTOPILOT is a short choice, so it is drawn as segments in
/// its row on the page (no page-wide picker) — on whichever page its row
/// falls, under one switch per supervised agent — and a segment saves its
/// mode.
#[test]
fn the_default_autopilot_is_segments_on_the_page() {
    let (mut runtime, instance, view) = harness_route(fixture());
    let key = prefs::EDIT_HARNESS_AUTOPILOT;
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        pages.iter().any(|page| page
            .semantic(&UiKey::new(format!("settings/segments/{key}")))
            .is_some()),
        "the default autopilot is segments"
    );
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(format!("settings/segment/{key}/1")),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        outcome.effects.iter().any(|effect| matches!(
            effect,
            AppEffect::ConfigPatch { patch, .. }
                if patch.edits.iter().any(|edit| edit.key == key)
        )),
        "a segment saves its mode"
    );
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert!(state.choice_picker.is_none(), "no page-wide picker");
}

/// ONLY A CARRYING DEFAULT'S BUTTON SAYS IT FOLLOWS THE DEFAULT: under
/// `[harness] autopilot = "off"` the Off button pins Off like any other
/// press (`harness_screen::hands_back`), so no button's description claims
/// to follow `[harness] autopilot`. NEGATIVE CONTROL: under a default of
/// Land the Land button says it, and only that one.
#[test]
fn an_off_defaults_button_does_not_say_it_follows_the_default() {
    let follows = |default: Autopilot| -> Vec<&'static str> {
        let mut state = fixture();
        state.posture = HarnessPosture::On(default);
        state.sessions[0].default = Some(default);
        let (mut runtime, instance, view) = harness_route(state);
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        Autopilot::ALL
            .iter()
            .map(|mode| mode.word())
            .filter(|word| {
                node(&pages, &format!("harness/session/7/autopilot/{word}"))
                    .description
                    .as_deref()
                    .is_some_and(|d| d.ends_with("so this follows [harness] autopilot"))
            })
            .collect()
    };
    assert_eq!(follows(Autopilot::Off), Vec::<&str>::new());
    assert_eq!(follows(Autopilot::Land), ["land"]);
}

/// THE SWITCHES EXPLAIN THEMSELVES on their own page, as every page's rows
/// do ([`prefs::row_notes`]): what supervision does under Supervise coding
/// agents, what an agent's own switch does once, under the last of them,
/// Cruise/Land/Off under Default autopilot, All/Safe/None under
/// Approve permission boxes, and the question priorities once, under
/// Question priority, never under Answer questions as well. At a roomy
/// window, the 80×24 one and the 102×29 one.
#[test]
fn the_harness_switches_draw_their_notes() {
    // Every agent's switch, in the card's order: the agents' note is the
    // last one's.
    let agents: Vec<(String, &str)> = aterm_phase::Program::ALL
        .into_iter()
        .filter(|p| p.harness_table())
        .map(|p| (format!("harness.{}.enabled", p.name()), p.product()))
        .collect();
    let (last_agent, earlier_agents) = agents.split_last().expect("a supervisable agent");
    let (mut runtime, instance, view) = harness_route(fixture());
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (width, height) in [(1_200.0, 820.0), (584.0, 348.0), (743.0, 434.0)] {
        let cx = view_cx_at(width, height);
        let pages = every_page(&mut runtime, instance, view, &cx);
        // The note lines painted for `key`'s row — inside it, or as the
        // stops right after it — in line order, over every page (not as the
        // list's peek).
        let painted = |key: &str| -> String {
            let prefixes = [
                format!("settings/detail/{key}/line-"),
                format!("settings/note/{key}/line-"),
            ];
            let mut lines = std::collections::BTreeMap::new();
            for node in pages.iter().flat_map(|page| page.paint.iter()) {
                let Some(line) = prefixes
                    .iter()
                    .find_map(|prefix| node.key.as_str().strip_prefix(prefix.as_str()))
                    .and_then(|line| line.parse::<usize>().ok())
                else {
                    continue;
                };
                if let (false, UiContent::Text(spec)) = (node.peek, &node.content) {
                    lines.insert(line, spec.text.clone());
                }
            }
            lines.into_values().collect::<Vec<_>>().join(" ")
        };
        let context = format!("{width}\u{d7}{height}");
        for (key, note) in [
            (prefs::EDIT_HARNESS_ENABLED, prefs::HARNESS_NOTE),
            (last_agent.0.as_str(), prefs::HARNESS_AGENT_NOTE),
            (prefs::EDIT_HARNESS_AUTOPILOT, prefs::HARNESS_AUTOPILOT_NOTE),
            (prefs::EDIT_HARNESS_APPROVE, prefs::HARNESS_APPROVE_NOTE),
            (
                prefs::EDIT_HARNESS_QUESTION_PRIORITY,
                prefs::HARNESS_QUESTIONS_NOTE,
            ),
        ] {
            assert!(
                words(&painted(key)).contains(&words(note)),
                "{context}: {key} draws its note; painted {:?}",
                painted(key)
            );
        }
        assert!(
            !words(&painted(prefs::EDIT_HARNESS_ANSWER_QUESTIONS))
                .contains(&words(prefs::HARNESS_QUESTIONS_NOTE)),
            "{context}: the shared questions note is drawn once, under Question priority"
        );
        for (key, _) in earlier_agents {
            assert!(
                !words(&painted(key)).contains(&words(prefs::HARNESS_AGENT_NOTE)),
                "{context}: the agents' note is drawn once, under the last of them, not {key}"
            );
        }
        // An agent's switch says it is about supervising that agent's
        // sessions, never the agent itself.
        for (key, product) in &agents {
            assert!(
                node(&pages, &format!("settings/control/{key}"))
                    .label
                    .starts_with(&prefs::agent_switch_label(product)),
                "{context}: {key}"
            );
        }
    }
}

/// A ROW THAT DOES NOTHING SAYS SO: with Supervise coding agents Off the
/// harness runs no supervisor, so every finer row of the card is saved and
/// idle (`SupervisorConfig::on_for` is `enabled && !off.contains(agent)`),
/// and with Answer questions Off, Question priority is. Each idle row's
/// control says why; the master switch, and Answer questions under its own
/// Off, stay live. NEGATIVE CONTROL: at the defaults no harness row says
/// it is inactive.
#[test]
fn the_harness_rows_say_when_they_are_inactive() {
    let label = |pages: &[crate::native_ui::CompiledUi], key: &str| -> String {
        let control = node(pages, &format!("settings/control/{key}"));
        format!(
            "{} {}",
            control.label,
            control.description.as_deref().unwrap_or_default()
        )
    };
    let inactive_rows = |source: Option<&str>| -> Vec<(&'static str, String)> {
        let (mut runtime, instance, view) = harness_route_over(fixture(), source);
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        harness_page_rows()
            .map(|key| (key, label(&pages, key)))
            .filter(|(_, label)| label.contains("nactive"))
            .collect()
    };
    assert_eq!(inactive_rows(None), Vec::new(), "the defaults are live");

    let off = inactive_rows(Some("[harness]\nenabled = false\n"));
    let keys: Vec<&str> = off.iter().map(|(key, _)| *key).collect();
    assert_eq!(
        keys,
        harness_page_rows()
            .filter(|key| *key != prefs::EDIT_HARNESS_ENABLED)
            .collect::<Vec<_>>(),
        "every row but the master switch is idle: {off:?}"
    );
    for (key, label) in &off {
        assert!(
            label.contains("Supervise coding agents is Off"),
            "{key}: {label}"
        );
    }

    let answer_off = inactive_rows(Some("[harness]\nanswer_questions = false\n"));
    assert_eq!(
        answer_off.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        [prefs::EDIT_HARNESS_QUESTION_PRIORITY],
        "{answer_off:?}"
    );
    assert!(
        answer_off[0].1.contains("Answer questions is Off"),
        "{answer_off:?}"
    );
}

/// AN IDLE ROW'S CONTROL IS MUTED, every segment of it: with Supervise
/// coding agents Off, every finer row's control — Default autopilot's and
/// Approve permission boxes' segments, the agents' switches, Question
/// priority's popup — carries `muted`, so the painter draws it in quiet ink
/// rather than the live accent (`native_ui`'s
/// `a_muted_selected_segment_paints_no_accent`); the master switch does not.
/// NEGATIVE CONTROL: at the defaults no control is muted.
#[test]
fn an_idle_rows_control_is_muted_every_segment_of_it() {
    let muted = |source: Option<&str>| -> Vec<String> {
        let (mut runtime, instance, view) = harness_route_over(fixture(), source);
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        let mut keys: Vec<String> = pages
            .iter()
            .flat_map(|page| page.semantics.iter())
            .filter(|node| node.state.is_some_and(|state| state.muted))
            .map(|node| node.key.as_str().to_string())
            .filter(|key| {
                harness_page_rows().any(|row| {
                    key == &format!("settings/control/{row}")
                        || key.starts_with(&format!("settings/segment/{row}/"))
                })
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    };
    assert_eq!(muted(None), Vec::<String>::new(), "the defaults are live");
    let off = muted(Some("[harness]\nenabled = false\n"));
    for key in [
        format!("settings/control/{}", prefs::EDIT_HARNESS_CLAUDE_ENABLED),
        format!("settings/control/{}", prefs::EDIT_HARNESS_AUTOPILOT),
        format!("settings/segment/{}/1", prefs::EDIT_HARNESS_AUTOPILOT),
        format!("settings/segment/{}/2", prefs::EDIT_HARNESS_AUTOPILOT),
        format!("settings/control/{}", prefs::EDIT_HARNESS_APPROVE),
        format!("settings/segment/{}/1", prefs::EDIT_HARNESS_APPROVE),
        format!("settings/control/{}", prefs::EDIT_HARNESS_QUESTION_PRIORITY),
    ] {
        assert!(off.contains(&key), "{key} is muted: {off:?}");
    }
    assert!(
        !off.contains(&format!("settings/control/{}", prefs::EDIT_HARNESS_ENABLED)),
        "the master switch stays live: {off:?}"
    );
    assert_eq!(
        muted(Some("[harness]\nanswer_questions = false\n")),
        [format!(
            "settings/control/{}",
            prefs::EDIT_HARNESS_QUESTION_PRIORITY
        )],
        "only the row Answer questions Off idles"
    );
}

/// OFF ITS PAGE A HARNESS ROW SAYS WHOSE IT IS: Modified's card and
/// Search's heading for the supervision rows name the Harness page once
/// ("Harness supervision"), so "Watch codex-alab sessions · Off" there never
/// reads as Codex itself switched off, and never as "Harness · Harness …".
#[test]
fn the_harness_rows_name_their_page_off_it() {
    for key in harness_page_rows() {
        let group = prefs::group_of(key).0;
        assert_eq!(group, "Harness supervision", "{key}");
        assert_eq!(search_group_caption(key, group), group, "{key}");
    }
    // NEGATIVE CONTROL: a group that does not name its place is captioned
    // with it.
    let key = prefs::EDIT_CURSOR_BLINK;
    let group = prefs::group_of(key).0;
    assert_eq!(
        search_group_caption(key, group),
        format!("{} \u{b7} {group}", settings_location(key))
    );
}

/// The actual Settings controller offers a live Cancel from the raw armed
/// instant even without a local clock face; an unarmed row still has none.
#[test]
fn a_sessions_timer_without_a_local_face_still_offers_cancel() {
    use crate::autopilot_timers::TimerChoice;
    use crate::native_app::HarnessTimerRequest;
    for armed in [false, true] {
        let mut state = fixture();
        state.sessions[0].word = Some(Autopilot::Cruise);
        state.sessions[0].view = AutopilotView::Cruise;
        state.sessions[0].armed_at_ms = armed.then_some(1_792_177_200_000);
        assert_eq!(state.sessions[0].glass.land_at, None);
        let (mut runtime, instance, view) = harness_route(state);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("harness/session/7/timer"),
                    value: None,
                }),
            )
            .unwrap();
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        let cancel = UiKey::new("harness/session/7/land-cancel");
        let shown = pages.iter().find_map(|page| page.semantic(&cancel));
        assert_eq!(shown.is_some(), armed);
        if armed {
            assert!(shown.unwrap().state.unwrap().enabled);
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new("harness/session/7/land-cancel"),
                        value: None,
                    }),
                )
                .unwrap();
            assert!(outcome.effects.iter().any(|effect| matches!(
                effect,
                AppEffect::HarnessTimer(HarnessTimerRequest {
                    session: 7,
                    choice: TimerChoice::Cancel,
                })
            )));
        }
    }
}

/// The visual label a page paints on button `key` (its semantic label keeps
/// the whole sentence).
fn button_face(pages: &[crate::native_ui::CompiledUi], key: &str) -> String {
    pages
        .iter()
        .flat_map(|page| page.paint.iter())
        .find_map(|node| match &node.content {
            UiContent::Button(control) if node.key.as_str() == key => {
                Some(control.spec.visual_label.clone().unwrap_or_default())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{key} paints no button"))
}

/// THE TIMER'S `at` BUTTON SAYS THE DAY IT WOULD ARM (§9.3.4; the Together
/// run's D6, where it read `at 18:36` beside `lands Sat 18:36`): an offer for
/// today reads `at 18:00` and says `Cruise until 18:00, then land`; once
/// today's has passed it reads `at Sat 18:00` and says `Cruise until
/// Saturday 18:00, then land`, as the state line writes the armed time.
/// NEGATIVE CONTROL: the button for today names no day.
#[test]
fn the_timers_at_button_names_the_day_it_would_arm() {
    let mut policy = SupervisorConfig::default();
    policy.set("land_at", "18:00").expect("a known key");
    for (day, face, spoken) in [
        (None, "at 18:00", "Cruise until 18:00, then land"),
        (
            Some(crate::claude_lights::Weekday::Sat),
            "at Sat 18:00",
            "Cruise until Saturday 18:00, then land",
        ),
    ] {
        let mut state = fixture();
        state.config = config_rows(&policy);
        state.land_at_offer = Some(crate::claude_lights::LandAtGlass {
            clock: crate::claude_lights::Clock::of_minute(18 * 60),
            other_day: day,
        });
        let (mut runtime, instance, view) = harness_route(state);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new("harness/session/7/timer"),
                    value: None,
                }),
            )
            .unwrap();
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        let key = "harness/session/7/land-at";
        assert_eq!(node(&pages, key).label, spoken, "{day:?}");
        assert_eq!(button_face(&pages, key), face, "{day:?}");
    }
}

/// A press of `action` (with `value`) on the page, and the host requests it
/// made.
fn press_for_requests(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    action: &str,
    value: Option<crate::native_app::SemanticInput>,
) -> Vec<crate::native_app::HarnessRequest> {
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value,
            }),
        )
        .unwrap()
        .effects
        .into_iter()
        .filter_map(|effect| match effect {
            AppEffect::Harness(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// The paint node `key` on `page`, where it is drawn.
fn painted<'a>(
    page: &'a crate::native_ui::CompiledUi,
    key: &str,
) -> Option<&'a crate::native_ui::PaintNode> {
    page.paint.iter().find(|node| node.key.as_str() == key)
}

/// SESSIONS STAYS ON THE FIRST SCREEN UNDER STATUS'S TODAY (§4 item 2,
/// §10.3.3 item 6): on the 80 × 24 window's page (584 × 348) and the shipped
/// 744 × 420, at 1× and 2× text, page 0 holds SESSIONS' heading. Today's
/// lines take what the first screen has left: Hero type where it has the
/// room (744 × 420, at 1× and 2×), the page's plain type where it has not
/// (584 × 348 at 1×), and at 2× on the short page, none (WHAT THE HARNESS DID still says
/// every count today). CONTROL: on a tall page the lines are drawn whole in
/// Hero type, the label both lines whole.
#[test]
fn sessions_stays_on_the_first_screen_under_todays_lines() {
    use crate::native_ui::{StyleRef, UiContent};
    let style = |page: &crate::native_ui::CompiledUi| {
        painted(page, "harness/status/today/hero").map(|node| match &node.content {
            UiContent::Text(text) => (text.style, text.text.clone()),
            other => panic!("a text: {other:?}"),
        })
    };
    let first = "3 approved today \u{b7} 1 answered";
    for (scale, width, height, want) in [
        (1.0, 584.0, 348.0, Some(StyleRef::Primary)),
        (1.0, 744.0, 420.0, Some(StyleRef::Hero)),
        (1.0, 849.0, 900.0, Some(StyleRef::Hero)),
        (2.0, 584.0, 348.0, None),
        (2.0, 744.0, 420.0, Some(StyleRef::Hero)),
    ] {
        at_each_text_scale(&[scale], "Harness first screen", |_| {
            let (mut runtime, instance, view) = harness_route(fixture());
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let page = &pages[0];
            assert!(
                page.semantic(&UiKey::new("harness/sessions/heading"))
                    .is_some(),
                "{context}: SESSIONS is on the first screen"
            );
            assert_eq!(
                style(page).map(|(style, _)| style),
                want,
                "{context}: today's first line"
            );
            if want.is_some() {
                assert_eq!(
                    node(&pages, "harness/status/today").label,
                    format!("{first} \u{b7} waited on you 2m"),
                    "{context}"
                );
                assert_eq!(
                    style(page).map(|(_, text)| text).as_deref(),
                    Some(first),
                    "{context}"
                );
            }
        });
    }
}

/// TODAY'S LINES GIVE WAY IN STEPS (§10.3.1, §10.3.4 "A narrow page"):
/// Hero type and the second line where the budget holds both; plain type
/// first where it does not, or where the Hero line is not whole at the
/// width; then the second line goes; then the block. A plain line too wide
/// is cut at a ` · ` joint, never inside a family. NEGATIVE CONTROL: the
/// label keeps both lines whole whatever is drawn.
#[test]
fn todays_lines_give_way_hero_then_plain_then_the_second_line() {
    use crate::native_settings::harness_page::today_block;
    use crate::native_ui::{StyleRef, UiContent};
    let lines = (
        "37 approved today \u{b7} 12 kept going \u{b7} 4 answered".to_string(),
        "2 resumed \u{b7} 1 land prompt \u{b7} waited on you 4m".to_string(),
    );
    let drawn = |block: &Option<(UiNode, f32)>| -> Vec<(StyleRef, String)> {
        block
            .as_ref()
            .map(|(node, _)| {
                node.children
                    .iter()
                    .map(|child| match &child.content {
                        UiContent::Text(text) => (text.style, text.text.clone()),
                        other => panic!("a text: {other:?}"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    at_each_text_scale(&[1.0], "today's block", |_| {
        let whole = today_block(&lines, 1_000.0, 400.0);
        assert_eq!(
            drawn(&whole),
            [
                (StyleRef::Hero, lines.0.clone()),
                (StyleRef::Primary, lines.1.clone())
            ]
        );
        let label = whole.as_ref().map(|(node, _)| match &node.content {
            UiContent::Group(group) => group.label.clone().unwrap_or_default(),
            other => panic!("a group: {other:?}"),
        });
        let both = format!("{} \u{b7} {}", lines.0, lines.1);
        assert_eq!(label.as_deref(), Some(both.as_str()));
        // Short: plain first, then one line, then none.
        assert_eq!(
            drawn(&today_block(&lines, 1_000.0, 60.0)),
            [
                (StyleRef::Primary, lines.0.clone()),
                (StyleRef::Primary, lines.1.clone())
            ]
        );
        assert_eq!(
            drawn(&today_block(&lines, 1_000.0, 30.0)),
            [(StyleRef::Primary, lines.0.clone())]
        );
        assert!(today_block(&lines, 1_000.0, 20.0).is_none());
        // Narrow: no Hero line that is not whole; a plain line cut at a joint.
        let narrow = drawn(&today_block(&lines, 200.0, 400.0));
        assert_eq!(narrow[0].0, StyleRef::Primary, "{narrow:?}");
        for (_, text) in &narrow {
            assert!(
                lines.0.starts_with(text.as_str()) || lines.1.starts_with(text.as_str()),
                "{text:?} is a head of its line"
            );
            assert!(
                !text.ends_with(" \u{b7}") && !text.ends_with('\u{2026}'),
                "{text:?} is cut at a joint"
            );
        }
        assert!(narrow[0].1.len() < lines.0.len(), "cut: {narrow:?}");
        let label = today_block(&lines, 200.0, 400.0).map(|(node, _)| match node.content {
            UiContent::Group(group) => group.label.unwrap_or_default(),
            other => panic!("a group: {other:?}"),
        });
        assert_eq!(label.as_deref(), Some(both.as_str()), "the label is whole");
    });
}

/// A fixture whose session 7 has a day of its own today, and a second row
/// whose title it shares where `twin`.
fn fixture_with_a_day(twin: bool) -> HarnessState {
    use aterm_agent::harness::stats::Tally;
    let mut state = fixture();
    let report = state.stats.report.take().expect("a reading");
    let mut report = (*report).clone();
    let mut today = HarnessStats::per_session(0);
    today.approved = 3;
    today.escalated = 1;
    today.by_session.as_mut().expect("per session").insert(
        "s-7".to_string(),
        Tally {
            approved: 3,
            escalated: 1,
            human_wait_ms: 60_000,
            last_escalated_ms: Some((18 * 3_600 + 29 * 60) * 1000),
            ..Tally::default()
        },
    );
    report = HarnessStatsReport::new(
        today,
        report.week,
        report.collected_unix_ms,
        report.offset_s,
        &|_| Some(0),
    );
    state.stats.report = Some(std::sync::Arc::new(report));
    if twin {
        state.sessions[0].title = "\u{2733} Claude Code".to_string();
        state.sessions[0].place = "in tab 1, left pane".to_string();
        let mut other = state.sessions[0].clone();
        other.local_id = 8;
        other.sid = "s-8".to_string();
        other.place = "in tab 1, right pane".to_string();
        state.sessions.push(other);
    }
    state
}

/// A ROW'S DAY, ITS SHOW AND THE LANDING (§10.3.1, §10.3.2): the row says
/// its day (`harness/session/7/today`); its `Show` asks the host to raise
/// it; `harness/land` with `session/7` draws its Show selected and its card
/// on the accent, until the next press. `supervision` and `top` select no
/// row. NEGATIVE CONTROLS: a Show for a session no longer listed asks for
/// nothing and says why; a landing with no words is not the page's.
#[test]
fn a_rows_day_its_show_and_its_landing() {
    use crate::native_app::{HarnessRequest, SemanticInput};
    use crate::native_ui::{StyleRef, UiContent};
    let (mut runtime, instance, view) = harness_route(fixture_with_a_day(false));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/7/today").label,
        "today: needed you once, 1m at 18:29 \u{b7} 3 approved"
    );
    let show = node(&pages, "harness/session/7/show");
    assert_eq!(show.label, "Show fix the build");
    assert_eq!(
        show.action.as_ref().map(ActionId::as_str),
        Some("harness/session/7/show")
    );
    assert!(
        !show.state.expect("a control").selected,
        "nothing landed yet"
    );
    assert_eq!(
        press_for_requests(&mut runtime, instance, view, "harness/session/7/show", None),
        [HarnessRequest::Show { session: 7 }]
    );
    assert!(
        press_for_requests(&mut runtime, instance, view, "harness/session/9/show", None).is_empty(),
        "no such session"
    );
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        assert_eq!(
            state.feedback.as_deref(),
            Some("That session is no longer open.")
        );
    }
    let land = |runtime: &mut NativeRuntime, words: Option<&str>| {
        press_for_requests(
            runtime,
            instance,
            view,
            crate::harness_screen::HARNESS_LAND,
            words.map(|w| SemanticInput::Text(w.to_string())),
        );
        let pages = every_page(runtime, instance, view, &view_cx());
        let selected = node(&pages, "harness/session/7/show")
            .state
            .expect("a control")
            .selected;
        let accent = pages.iter().find_map(|page| painted(page, "harness/session/7")).map(|n| {
            matches!(&n.content, UiContent::Group(group) if group.style == StyleRef::Primary)
        });
        (selected, accent)
    };
    assert_eq!(
        land(&mut runtime, Some("session/7")),
        (true, Some(true)),
        "landed"
    );
    // The next press lets it go.
    press_for_requests(&mut runtime, instance, view, HARNESS_REFRESH, None);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        !node(&pages, "harness/session/7/show")
            .state
            .expect("a control")
            .selected
    );
    assert_eq!(
        land(&mut runtime, Some("supervision")),
        (false, Some(false))
    );
    assert_eq!(land(&mut runtime, Some("top")), (false, Some(false)));
    assert_eq!(
        land(&mut runtime, None),
        (false, Some(false)),
        "no words, no landing"
    );
}

/// TWO ROWS THAT SHARE A TITLE READ APART (hunt surfaces-F5): two Claude
/// Code panes of one tab are two rows whose every button is spoken by a
/// name of its own — the title and the pane. CONTROL: the two rows' titles
/// are the same, so the names are what tells them apart.
#[test]
fn two_rows_that_share_a_title_have_buttons_that_read_apart() {
    let state = fixture_with_a_day(true);
    assert_eq!(state.sessions[0].title, state.sessions[1].title);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let mut labels: Vec<String> = pages
        .iter()
        .flat_map(|page| page.semantics.iter())
        .filter(|node| {
            node.key.as_str().starts_with("harness/session/")
                && node.role == crate::native_ui::SemanticRole::Button
        })
        .map(|node| node.label.clone())
        .collect();
    labels.sort();
    labels.dedup();
    let total = pages
        .iter()
        .flat_map(|page| page.semantics.iter())
        .filter(|node| {
            node.key.as_str().starts_with("harness/session/")
                && node.role == crate::native_ui::SemanticRole::Button
        })
        .map(|node| node.key.as_str().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert!(total >= 10, "both rows' buttons: {total}");
    assert_eq!(
        labels.len(),
        total,
        "every button its own words: {labels:#?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l == "Show \u{2733} Claude Code, tab 1, left pane"),
        "{labels:#?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l == "Show \u{2733} Claude Code, tab 1, right pane"),
        "{labels:#?}"
    );
}

/// A ROW'S WORDS KEEP ITS PLACE WHOLE, AND ITS BUTTONS THEIR FACES (§10.3.4,
/// "A narrow page"; the review of v3/stats-fold, finding 4): two `✳ Claude
/// Code` rows of one tab — in the first window, and in a second — on the
/// narrowest Medium page (760), on Medium pages where a fifth fixed button
/// squeezed the words (780, 820), and on Compact pages (320, 340, 584), at
/// 1×, 1.25× and 2× text: each row paints its place (`Tab 1, left pane`,
/// `Window 2, tab 1, right pane`) whole at the head of its detail wherever
/// the page's column can hold it at all, and no face of its five buttons is
/// cut. CONTROL: on a roomy page at 1× and
/// 1.25× the words sit beside the buttons, on one line.
#[test]
fn a_rows_words_keep_its_place_whole_and_its_buttons_their_faces() {
    use crate::native_ui::UiContent;
    let faces = [
        "autopilot/cruise",
        "autopilot/land",
        "autopilot/off",
        "timer",
        "show",
    ];
    for window in ["", "window 2, "] {
        let mut state = fixture_with_a_day(true);
        for (row, side) in state.sessions.iter_mut().zip(["left", "right"]) {
            row.place = format!("in {window}tab 1, {side} pane");
        }
        let state = &state;
        at_each_text_scale(&[1.0, 1.25, 2.0], "Harness rows", |scale| {
            for width in [320.0, 340.0, 584.0, 760.0, 780.0, 820.0, 2_400.0] {
                let (mut runtime, instance, view) = harness_route(state.clone());
                let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, 900.0));
                let find = |key: &str| {
                    pages
                        .iter()
                        .find_map(|page| painted(page, key))
                        .unwrap_or_else(|| panic!("{key} is painted on no page"))
                };
                for (id, side) in [(7, "left"), (8, "right")] {
                    let context = format!("{scale}\u{d7} {width} {window:?} row {id}");
                    let place = if window.is_empty() {
                        format!("Tab 1, {side} pane")
                    } else {
                        format!("Window 2, tab 1, {side} pane")
                    };
                    let detail = find(&format!("harness/session/{id}/detail"));
                    let UiContent::Text(text) = &detail.content else {
                        panic!("{context}: the detail is a text")
                    };
                    // Where the column holds the place at all, it is whole.
                    // Only 2× text on a 320-point page is narrower than
                    // `Window 2, tab 1, left pane` itself (the row's label
                    // still says it whole).
                    let room =
                        crate::native_ui::native_status_label_width(&format!("{place}\u{2026}"));
                    if room <= detail.rect.width {
                        assert!(
                            text.text.starts_with(&place),
                            "{context}: the place whole, {:?}",
                            text.text
                        );
                    } else {
                        assert!(
                            scale >= 2.0 && width < 584.0 && !window.is_empty(),
                            "{context}: a column of {} under a place of {room}",
                            detail.rect.width
                        );
                    }
                    for face in faces {
                        let painted = button_face(&pages, &format!("harness/session/{id}/{face}"));
                        assert!(
                            !painted.ends_with('\u{2026}') && !painted.is_empty(),
                            "{context}: {face}'s face whole, {painted:?}"
                        );
                    }
                    // CONTROL: roomy, the words beside the buttons. (At 2×
                    // the page's column never holds a place beside five
                    // buttons: the rows stay stacked.)
                    if width > 2_000.0 && scale < 2.0 {
                        let cruise = find(&format!("harness/session/{id}/autopilot/cruise"));
                        assert!(
                            cruise.rect.y < detail.rect.y + detail.rect.height
                                && cruise.rect.x > detail.rect.x + detail.rect.width - 1.0,
                            "{context}: one line, {:?} beside {:?}",
                            cruise.rect,
                            detail.rect
                        );
                    }
                }
            }
        });
    }
}
