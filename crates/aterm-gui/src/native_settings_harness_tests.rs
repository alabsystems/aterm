// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Harness's page (`native_settings_harness.rs`), rendered through the Settings controller and read back from the compiled semantic tree.

use super::*;
use crate::autopilot::{AutopilotView, HarnessPosture};
use crate::harness_ab::{AbArm, AbArmTotals};
use crate::harness_screen::{
    AbWeek, HarnessSession, HarnessStatsReport, HarnessStatsSlot, config_rows,
};
use crate::native_settings::harness_page::{
    HARNESS_CONFIG_TOGGLE, HARNESS_OPEN_CONFIG, HARNESS_REFRESH, harness_page_rows,
};
use aterm_agent::harness::stats::HarnessStats;
use aterm_agent::supervise::{Autopilot, SupervisorConfig};

/// SESSIONS' `Every session` row (design §11.5), with this module's
/// fixtures and readers.
#[path = "native_settings_harness_every_tests.rs"]
mod every;

/// A harness snapshot as the host publishes one.
pub(super) fn fixture() -> HarnessState {
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
            stopped: None,
            waiting: None,
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
            set_by: None,
            need: None,
            hold: None,
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
        ab: ab_week(),
        config: config_rows(&policy),
        land_at_offer: None,
        ai: crate::ai_screen::AiState::default(),
        tabs: crate::harness_screen::TabControlState::default(),
        decided_open: Vec::new(),
        today: 0,
    }
}

fn ab_week() -> AbWeek {
    let mut week = AbWeek::default();
    week.arms[AbArm::Cruise.index()] = AbArmTotals {
        busy_ms: 97_000,
        at_prompt_ms: 3_000,
        ..AbArmTotals::default()
    };
    week.arms[AbArm::Land.index()] = AbArmTotals {
        busy_ms: 60_000,
        at_prompt_ms: 3_540_000,
        landed_ms: 3_000_000,
        ..AbArmTotals::default()
    };
    week.arms[AbArm::Off.index()] = AbArmTotals {
        busy_ms: 59_000,
        at_box_ms: 1_000,
        at_prompt_ms: 40_000,
        ..AbArmTotals::default()
    };
    week.arms[AbArm::Unsupervised.index()] = AbArmTotals {
        busy_ms: 30_000,
        at_prompt_ms: 30_000,
        ..AbArmTotals::default()
    };
    week.holds[0][0][1] = 3;
    week.holds[2][0][9] = 4;
    week.first = Some((0, 0));
    week
}

fn fixture_led(both: bool) -> HarnessState {
    let mut state = fixture();
    let mut week = ab_week();
    week.arms[AbArm::Cruise.index()] = AbArmTotals {
        busy_ms: 369 * 60_000,
        at_box_ms: 60_000,
        at_prompt_ms: 40 * 60_000,
        ..AbArmTotals::default()
    };
    week.arms[AbArm::Off.index()] = if both {
        AbArmTotals {
            busy_ms: 21 * 60_000 + 30_000,
            at_box_ms: 3 * 60_000 + 30_000,
            at_prompt_ms: 5 * 60_000,
            ..AbArmTotals::default()
        }
    } else {
        AbArmTotals::default()
    };
    week.ends.landed = 5;
    week.ends.landing[crate::harness_ab::bucket(6 * 60_000)] = 5;
    state.ab = week;
    state
}

fn harness_route(
    state: HarnessState,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    harness_route_over(state, None)
}

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

fn every_page(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
) -> Vec<crate::native_ui::CompiledUi> {
    pages_while(runtime, instance, view, cx, |_| true)
}

/// The pages from the first through the first that paints nothing keyed under `card` after one
/// that did: what a law of that card reads. The route's other cards are
/// [`the_harness_route_is_whole_on_every_host`]'s.
fn pages_through(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
    card: &str,
) -> Vec<crate::native_ui::CompiledUi> {
    let mut drawn = false;
    pages_while(runtime, instance, view, cx, |page| {
        let draws = page
            .paint
            .iter()
            .any(|paint| paint.key.as_str().starts_with(card));
        let on = draws || !drawn;
        drawn |= draws;
        on
    })
}

/// The pages from the first, each compiled, through the last or the first `more` says ends
/// the walk.
fn pages_while(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    cx: &ViewCx<'_>,
    mut more: impl FnMut(&crate::native_ui::CompiledUi) -> bool,
) -> Vec<crate::native_ui::CompiledUi> {
    let mut pages = Vec::new();
    let mut page = 0;
    loop {
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.scroll_page_to(page);
            state.page_reveal = None;
        }
        let compiled = compile_settings_view(runtime, instance, view, cx);
        let on = more(&compiled);
        pages.push(compiled);
        let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
            unreachable!()
        };
        let limit = state
            .result_page_limit
            .get()
            .expect("the page records its bound");
        if !on || page >= limit {
            return pages;
        }
        page += 1;
    }
}

fn node<'a>(
    pages: &'a [crate::native_ui::CompiledUi],
    key: &str,
) -> &'a crate::native_ui::SemanticNode {
    pages
        .iter()
        .find_map(|page| page.semantic(&UiKey::new(key)))
        .unwrap_or_else(|| panic!("{key} is on no page"))
}

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
    for harness in aterm_phase::CliHarness::ALL {
        let cli = harness.cli();
        let harness_build = node(&pages, &format!("harness/updates/row/alab-{cli}-harness"));
        let own = format!(
            "v{} (built-in) · in aterm {}",
            harness.version_str(),
            crate::build_info::version()
        );
        assert!(
            harness_build.label.contains(&format!("alab {cli} harness"))
                && harness_build.label.contains(&own),
            "{harness:?}: its name as its top rule reads it, its own version, then the aterm it \
             ships in: {:?}",
            harness_build.label
        );
        assert!(
            harness_build
                .label
                .contains(crate::build_info::git_commit())
        );
    }
    for cli in ["claude", "codex"] {
        assert!(
            node(&pages, &format!("harness/updates/row/{cli}-cli"))
                .label
                .contains("not looked at"),
            "a product name alone cannot imply an installed or observed CLI"
        );
    }
    for (key, route) in [
        ("software", SettingsRoute::SoftwareUpdate),
        ("packages", SettingsRoute::Packages),
        ("versions", SettingsRoute::Ai),
    ] {
        assert_eq!(
            node(&pages, &format!("harness/updates/{key}"))
                .action
                .as_ref(),
            Some(&route_action(route)),
            "update links must open the existing page without an install action"
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
    let rows = harness_page_rows().collect::<Vec<_>>();
    assert_eq!(
        rows[..5],
        [
            prefs::EDIT_HARNESS_ENABLED,
            prefs::EDIT_HARNESS_CLAUDE_ENABLED,
            prefs::EDIT_HARNESS_CODEX_ENABLED,
            prefs::EDIT_HARNESS_AGY_ENABLED,
            prefs::EDIT_HARNESS_MUSE_ENABLED,
        ],
        "{rows:?}"
    );
    assert!(
        rows.contains(&prefs::EDIT_HARNESS_QUESTION_PRIORITY),
        "{rows:?}"
    );
    assert_eq!(
        node(&pages, "harness/session/7").label,
        "fix the build: Tab 2 \u{b7} Claude Code \u{b7} supervised \u{b7} autopilot land"
    );
    assert_eq!(
        node(&pages, "harness/session/7/setting").label,
        "its own setting"
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
    assert_eq!(
        node(&pages, "harness/did/row/approved").label,
        "Approved: 3 today \u{b7} 41 in 7 days"
    );
    assert_eq!(
        node(&pages, "harness/did/row/waited-for-you").label,
        "Waited for you: 0 today, 2m \u{b7} 0 in 7 days, 2h 5m"
    );
    assert_eq!(
        node(&pages, "harness/did/row/partial").label,
        "Partial: only the last 1024 actions are kept"
    );
    assert_eq!(
        node(&pages, "harness/did/row/checked").label,
        "Checked: at 13:46"
    );
    assert_eq!(
        node(&pages, "harness/did/row/last-check").label,
        "Last check: failed: the ledgers' reader failed"
    );
    for (row, label) in [
        (
            "boxes",
            "Boxes: about 0.7 s on Cruise \u{b7} about 3m off \u{b7} median of 3 and 4 boxes seen",
        ),
        ("questions", "Questions: none seen yet"),
        ("held", "Held: 0% of working time on Cruise \u{b7} 2% off"),
        (
            "at-its-prompt",
            "At its prompt: Cruise 3% of 1m: pausing 0% \u{b7} Off 40% of 1m",
        ),
        ("land", "Land: held 0% \u{b7} at its prompt 98%, landed 83%"),
        (
            "unsupervised",
            "Unsupervised: at its prompt 50% of 1m, in no comparison",
        ),
        ("measured", "Measured: today"),
    ] {
        assert_eq!(node(&pages, &format!("harness/ab/row/{row}")).label, label);
    }
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
    assert_eq!(toggle.label, "Show only yours: the settings you changed");

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

    let mut observed = fixture();
    observed.ai.enabled = true;
    let version = "captured-claude-build";
    let cli = |id, product, state, version, fact: &str| crate::ai_screen::AiCli {
        id,
        product,
        claude: id == "claude",
        wanted: true,
        state,
        mark: aterm_agent::ai::state::Mark::Note,
        fact: fact.to_string(),
        path: None,
        version,
        offers_fix: true,
        mend: None,
    };
    observed.ai.reading = Some(crate::ai_screen::AiReading {
        as_of: 1_000_000,
        offset_s: Some(0),
        preferred: "claude",
        clis: vec![
            cli(
                "claude",
                "Claude Code",
                aterm_agent::ai::state::State::Ok,
                Some(version.to_string()),
                "captured login",
            ),
            cli(
                "codex",
                "Codex CLI",
                aterm_agent::ai::state::State::NotInstalled,
                None,
                "not installed",
            ),
        ],
        store_note: None,
        held: None,
    });
    assert!(runtime.replace_settings_harness(observed.clone(), 2));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "harness/updates/row/claude-cli")
            .label
            .contains(version)
    );
    assert!(
        node(&pages, "harness/updates/row/codex-cli")
            .label
            .contains("not installed")
    );
    observed.ai.reading = None;
    observed.ai.unavailable = Some("captured collector refusal".to_string());
    assert!(runtime.replace_settings_harness(observed, 3));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "harness/updates/row/cli-observations")
            .label
            .contains("captured collector refusal")
    );
    assert!(
        !node(&pages, "harness/updates/row/claude-cli")
            .label
            .contains(version)
    );
}

#[test]
fn the_harness_route_says_what_it_has_not_got() {
    let mut empty = fixture();
    empty.sessions.clear();
    empty.ab = AbWeek::default();
    let (mut runtime, instance, view) = harness_route(empty);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/sessions/empty").label,
        "No Claude Code, Codex, Antigravity CLI or Muse Code session is open"
    );
    assert!(
        pages.iter().all(|page| page
            .semantic(&UiKey::new("harness/status/row/sessions"))
            .is_none()),
        "no session open is said once, by the SESSIONS card"
    );
    for (row, words) in [
        ("boxes", "none seen yet"),
        ("questions", "none seen yet"),
        ("held", "not measured yet"),
        ("at-its-prompt", "not measured yet"),
        ("land", "not measured yet"),
        ("ends", "none yet"),
        ("unsupervised", "none"),
        ("measured", "nothing yet"),
    ] {
        assert!(
            node(&pages, &format!("harness/ab/row/{row}"))
                .label
                .ends_with(&format!(": {words}")),
            "{row}"
        );
    }
    for row in ["this-run", "finished"] {
        assert!(
            pages.iter().all(|page| page
                .semantic(&UiKey::new(format!("harness/ab/row/{row}")))
                .is_none()),
            "{row}: the run's rows are gone"
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
        ("land-at", "Cruise, then land at 18:00", TimerChoice::At),
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
        let (mut runtime, instance, view) = harness_route(fixture_led(true));
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

fn painted_text(pages: &[crate::native_ui::CompiledUi], key: &str) -> String {
    let node = pages
        .iter()
        .find_map(|page| page.paint.iter().find(|n| n.key.as_str() == key))
        .unwrap_or_else(|| panic!("{key} is painted on no page"));
    if let crate::native_ui::UiContent::Text(text) = &node.content {
        return text.text.clone();
    }
    let lines = pages
        .iter()
        .flat_map(|page| page.paint.iter())
        .filter(|n| {
            n.key
                .as_str()
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix("/line-"))
                .is_some_and(|index| index.parse::<usize>().is_ok())
        })
        .filter_map(|n| match &n.content {
            crate::native_ui::UiContent::Text(text) => {
                Some((n.key.as_str().to_string(), text.text.clone()))
            }
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut lines = lines.into_iter().collect::<Vec<_>>();
    lines.sort_by_key(|(key, _)| {
        key.rsplit_once("/line-")
            .and_then(|(_, index)| index.parse::<usize>().ok())
            .unwrap_or(0)
    });
    let lines = lines.into_iter().map(|(_, text)| text).collect::<Vec<_>>();
    painted_words(&lines.join("\n"))
}

#[test]
fn a_rows_setting_line_is_whole_in_its_label_on_every_host() {
    use crate::autopilot::{SetByWords, Surface};
    let mut state = fixture();
    state.today = 20_400;
    state.sessions[0].set_by = Some(SetByWords {
        by: Surface::EverySession.phrase().to_string(),
        at: Some(crate::harness_glass::Stamp {
            clock: crate::claude_lights::Clock::of_minute(14 * 60 + 5),
            day: 20_000,
        }),
    });
    let row = state.projection().sessions[0].clone();
    let words =
        "its own setting, set 4 Oct 2024 14:05 from Settings \u{25b8} Harness, every session";
    assert_eq!(row.setting, words);
    assert_eq!(
        row.detail,
        "Tab 2 \u{b7} Claude Code \u{b7} supervised \u{b7} autopilot land"
    );
    let cut = std::sync::atomic::AtomicBool::new(false);
    at_each_text_scale(&[1.0, 2.0], "the setting line", |scale| {
        let (mut runtime, instance, view) = harness_route(state.clone());
        for (width, height) in [
            (1_200.0, 820.0),
            (320.0, 348.0),
            (320.0, 568.0),
            (584.0, 348.0),
            (744.0, 420.0),
            (849.0, 513.0),
        ] {
            let cx = view_cx_at(width, height);
            let pages = pages_through(&mut runtime, instance, view, &cx, "harness/session");
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            assert_eq!(
                node(&pages, "harness/session/7/setting").label,
                words,
                "{context}: the label is the words whole"
            );
            let shown = painted_text(&pages, "harness/session/7/setting/text");
            if pages.iter().any(|page| {
                page.paint
                    .iter()
                    .any(|n| n.key.as_str() == "harness/session/7/setting/text/line-1")
            }) {
                cut.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            assert_eq!(shown, words, "{context}: the painted words are whole");
            for (page, compiled) in pages.iter().enumerate() {
                let context = format!("{context} page {page}");
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
    assert!(
        cut.load(std::sync::atomic::Ordering::Relaxed),
        "control: the painted words wrap somewhere"
    );
}

/// A stacked SESSIONS row with its setting, state and day lines is whole on some page, or has
/// its buttons on one line, at 2× text on every height from 300 to 760 pt, 4 pt apart, on a
/// phone's width and a narrow window's; and the pages through SESSIONS ([`pages_through`])
/// clip nothing. NEGATIVE CONTROL: a stacked row's height that leaves out its setting line
/// clips the row at 584 × 300.
#[test]
fn a_stacked_rows_flow_counts_its_setting_line() {
    use crate::harness_glass::Stamp;
    use aterm_agent::harness::acts::Act;
    let mut state = fixture_with_a_day(true);
    state.sessions[0].glass.last = Some((
        Act::Approved,
        Stamp {
            clock: crate::claude_lights::Clock::of_minute(14 * 60 + 5),
            day: 0,
        },
    ));
    state.sessions[0].set_by = Some(crate::autopilot::SetByWords {
        by: crate::autopilot::Surface::Menu.phrase().to_string(),
        at: Some(Stamp {
            clock: crate::claude_lights::Clock::of_minute(14 * 60 + 5),
            day: 0,
        }),
    });
    let row = state.projection().sessions[0].clone();
    assert!(
        row.state_line().is_some() && row.day_line.is_some(),
        "{row:?}"
    );
    const RUNTIMES: usize = 8;
    let hosts: Vec<(f32, f32)> = [320.0, 584.0]
        .into_iter()
        .flat_map(|width| {
            (300..=760)
                .step_by(4)
                .map(move |height| (width, f32::from(u16::try_from(height).expect("small"))))
        })
        .collect();
    let dealt = std::sync::atomic::AtomicUsize::new(0);
    at_each_text_scale(&[2.0; RUNTIMES], "the stacked row's flow", |scale| {
        let hand = dealt.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (mut runtime, instance, view) = harness_route(state.clone());
        for &(width, height) in hosts.iter().skip(hand).step_by(RUNTIMES) {
            let cx = view_cx_at(width, height);
            let pages = pages_through(&mut runtime, instance, view, &cx, "harness/session");
            assert!(
                pages.iter().all(|page| page
                    .semantic(&UiKey::new("harness/session/7/head"))
                    .is_none()),
                "{width}: stacked"
            );
            let blocks: std::collections::BTreeSet<&str> = pages
                .iter()
                .flat_map(|page| page.paint.iter())
                .map(|paint| paint.key.as_str())
                .filter(|key| key.starts_with("harness/session/") && key.ends_with("/block"))
                .collect();
            assert!(
                blocks.contains("harness/session/7/block"),
                "{width}\u{d7}{height}: {blocks:?}"
            );
            let inset = crate::native_settings::harness_page::CARD_GAP / 2.0;
            for block in blocks {
                let row = block.trim_end_matches("block");
                let button_lines = pages
                    .iter()
                    .map(|page| {
                        page.semantics
                            .iter()
                            .filter(|node| {
                                node.key.as_str().starts_with(row)
                                    && node.role == SemanticRole::Button
                            })
                            .map(|node| node.rect.y.round().to_bits())
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                    })
                    .max()
                    .unwrap_or(0);
                let whole = pages
                    .iter()
                    .flat_map(|page| page.paint.iter())
                    .any(|paint| {
                        paint.key.as_str() == block
                            && paint.clip.y <= paint.rect.y + 1.0
                            && paint.clip.y + paint.clip.height
                                >= paint.rect.y + paint.rect.height - inset - 1.0
                    });
                assert!(
                    whole || button_lines <= 1,
                    "{scale}\u{d7} {width}\u{d7}{height}: {block}, its buttons on \
                     {button_lines} lines, is whole on no page"
                );
            }
            for (page, compiled) in pages.iter().enumerate() {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height} page {page}");
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

#[test]
fn the_harness_switches_draw_their_notes() {
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
        let lines_of = |prefixes: &[String]| -> String {
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
        let painted = |key: &str| {
            lines_of(&[
                format!("settings/detail/{key}/line-"),
                format!("settings/note/{key}/line-"),
            ])
        };
        let shared = |key: &str| lines_of(&[format!("settings/shared-note/{key}/line-")]);
        let context = format!("{width}\u{d7}{height}");
        for (key, note) in [
            (prefs::EDIT_HARNESS_ENABLED, prefs::HARNESS_NOTE),
            (
                prefs::EDIT_HARNESS_AUTOPILOT,
                prefs::HARNESS_AUTOPILOT_NOTE.as_str(),
            ),
            (prefs::EDIT_HARNESS_APPROVE, prefs::HARNESS_APPROVE_NOTE),
        ] {
            assert!(
                words(&painted(key)).contains(&words(note)),
                "{context}: {key} draws its note; painted {:?}",
                painted(key)
            );
        }
        for (key, note) in [
            (last_agent.0.as_str(), prefs::HARNESS_AGENT_NOTE),
            (
                prefs::EDIT_HARNESS_QUESTION_PRIORITY,
                prefs::HARNESS_QUESTIONS_NOTE,
            ),
        ] {
            assert!(
                words(&shared(key)).contains(&words(note)),
                "{context}: the run {key} closes draws its note after it; shared {:?}",
                shared(key)
            );
            assert!(
                !words(&painted(key)).contains(&words(note)),
                "{context}: the run's note is not {key}'s own lines"
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
        [
            format!("settings/control/{}", prefs::EDIT_HARNESS_QUESTION_PRIORITY),
            format!(
                "settings/segment/{}/1",
                prefs::EDIT_HARNESS_QUESTION_PRIORITY
            ),
            format!(
                "settings/segment/{}/2",
                prefs::EDIT_HARNESS_QUESTION_PRIORITY
            ),
        ],
        "only the row Answer questions Off idles, every segment of it"
    );
}

#[test]
fn the_harness_rows_name_their_page_off_it() {
    for key in harness_page_rows() {
        let group = prefs::group_of(key).0;
        assert_eq!(group, "Harness supervision", "{key}");
        assert_eq!(search_group_caption(key, group), group, "{key}");
    }
    let key = prefs::EDIT_CURSOR_BLINK;
    let group = prefs::group_of(key).0;
    assert_eq!(
        search_group_caption(key, group),
        format!("{} \u{b7} {group}", settings_location(key))
    );
}

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
        assert_eq!(
            node(&pages, key).label,
            format!("Cruise, then land {face}"),
            "{day:?}"
        );
        assert_eq!(
            node(&pages, key).description.as_deref(),
            Some(spoken),
            "{day:?}"
        );
        assert_eq!(button_face(&pages, key), face, "{day:?}");
    }
}

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

fn painted<'a>(
    page: &'a crate::native_ui::CompiledUi,
    key: &str,
) -> Option<&'a crate::native_ui::PaintNode> {
    page.paint.iter().find(|node| node.key.as_str() == key)
}

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
    for (scale, width, height, want, face_decides) in [
        (1.0, 584.0, 348.0, Some(StyleRef::Hero), false),
        (1.0, 744.0, 420.0, Some(StyleRef::Hero), false),
        (1.0, 849.0, 900.0, Some(StyleRef::Hero), false),
        (2.0, 584.0, 348.0, None, false),
        (2.0, 744.0, 420.0, Some(StyleRef::Hero), true),
    ] {
        at_each_text_scale(&[scale], "Harness first screen", |_| {
            let (mut runtime, instance, view) = harness_route(fixture());
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let page = &pages[0];
            let want = if face_decides && cfg!(target_os = "linux") {
                let line = painted(page, "harness/status/today/hero")
                    .unwrap_or_else(|| panic!("{context}: today's first line is drawn"));
                let (px, face) = crate::native_ui::text_paint_metrics(
                    crate::native_ui::SemanticRole::Heading,
                    StyleRef::Hero,
                );
                let hero = crate::tray_raster::ui_text_width_for(face, first, px);
                Some(if hero <= line.rect.width {
                    StyleRef::Hero
                } else {
                    StyleRef::Primary
                })
            } else {
                want
            };
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
                    format!("{first} \u{b7} waited for you 2m"),
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

#[test]
fn todays_lines_give_way_hero_then_plain_then_the_second_line() {
    use crate::native_settings::harness_page::today_block;
    use crate::native_ui::{StyleRef, UiContent};
    let lines = (
        "37 approved today \u{b7} 12 kept going \u{b7} 4 answered".to_string(),
        "2 resumed \u{b7} 1 land prompt \u{b7} waited for you 4m".to_string(),
    );
    let drawn = |block: &Option<(UiNode, f32)>| -> Vec<(StyleRef, String)> {
        block
            .as_ref()
            .map(|(node, _)| {
                node.children
                    .iter()
                    .map(|child| match &child.content {
                        UiContent::Text(text) => (text.style, text.text.clone()),
                        UiContent::Group(group) => (
                            child
                                .children
                                .iter()
                                .map(|line| match &line.content {
                                    UiContent::Text(text) => text.style,
                                    other => panic!("a line: {other:?}"),
                                })
                                .reduce(|one, other| {
                                    assert_eq!(one, other, "one style for every line");
                                    one
                                })
                                .unwrap_or(StyleRef::Plain),
                            crate::native_ui::joined_as_named(
                                &child
                                    .children
                                    .iter()
                                    .map(|line| match &line.content {
                                        UiContent::Text(text) => text.text.clone(),
                                        other => panic!("a line: {other:?}"),
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                group.label.as_deref().unwrap_or_default(),
                            ),
                        ),
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
        let block = today_block(&lines, 200.0, 400.0);
        assert_eq!(
            drawn(&block),
            [
                (StyleRef::Primary, lines.0.clone()),
                (StyleRef::Primary, lines.1.clone())
            ]
        );
        assert!(
            block.as_ref().is_some_and(|(node, _)| node
                .children
                .iter()
                .all(|line| matches!(line.content, UiContent::Group(_)))),
            "wrapped"
        );
        let note = (
            crate::harness_screen::TODAY_NOTHING_YET.to_string(),
            String::new(),
        );
        assert_eq!(
            drawn(&today_block(&note, 60.0, 400.0)),
            [(StyleRef::Primary, note.0.clone())]
        );
        let label = today_block(&lines, 200.0, 400.0).map(|(node, _)| match node.content {
            UiContent::Group(group) => group.label.unwrap_or_default(),
            other => panic!("a group: {other:?}"),
        });
        assert_eq!(label.as_deref(), Some(both.as_str()), "the label is whole");
    });
}

#[test]
fn todays_notes_are_drawn_in_the_pages_plain_type() {
    use crate::harness_screen::{TODAY_NOTHING_YET, TODAY_READING};
    use crate::native_settings::harness_page::today_block;
    use crate::native_ui::{StyleRef, UiContent};
    let drawn = |first: &str, inner: f32, budget: f32| {
        let (node, height) =
            today_block(&(first.to_string(), String::new()), inner, budget).expect("a block");
        let styles: Vec<(SemanticRole, StyleRef, f32)> = node
            .children
            .iter()
            .map(|child| match &child.content {
                UiContent::Text(text) => (
                    text.role,
                    text.style,
                    match child.layout.height {
                        crate::native_ui::Length::Fixed(height) => height,
                        other => panic!("a fixed height: {other:?}"),
                    },
                ),
                other => panic!("a text: {other:?}"),
            })
            .collect();
        (styles, height)
    };
    let sessions_at = |state: HarnessState| {
        let (runtime, instance, view) = harness_route(state);
        let page = compile_settings_view(&runtime, instance, view, &view_cx_at(849.0, 900.0));
        let style = painted(&page, "harness/status/today/hero").map(|n| match &n.content {
            UiContent::Text(text) => text.style,
            other => panic!("a text: {other:?}"),
        });
        let heading = painted(&page, "harness/sessions/heading").map(|n| n.rect.y);
        (style, heading.expect("SESSIONS is drawn"))
    };
    let reading = || {
        let mut state = fixture();
        state.stats.report = None;
        state.stats.collecting = true;
        state.stats.unavailable = None;
        state
    };
    let read = |approved: u64| {
        let mut today = HarnessStats::new(0);
        today.approved = approved;
        today.rows = approved;
        let mut state = fixture();
        state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
            today.clone(),
            today,
            1_000_000_000,
            Some(0),
            &|_| Some(0),
        )));
        state.stats.unavailable = None;
        state
    };
    at_each_text_scale(&[1.0, 2.0], "today's notes", |scale| {
        let (counts, hero_height) = drawn("3 approved today", 1_000.0, 400.0);
        assert_eq!(
            counts,
            [(SemanticRole::Heading, StyleRef::Hero, hero_height)],
            "control"
        );
        for note in [TODAY_NOTHING_YET, TODAY_READING] {
            let (styles, height) = drawn(note, 1_000.0, 400.0);
            assert_eq!(
                styles,
                [(SemanticRole::Text, StyleRef::Primary, hero_height)],
                "{note}: plain type in the Hero line's slot"
            );
            assert_eq!(height, hero_height, "{note}: the counts' room");
            let short = hero_height - 1.0;
            let (_, plain) = drawn("3 approved today", 1_000.0, short);
            assert_eq!(drawn(note, 1_000.0, short).1, plain, "{note}: short");
            assert!(plain < hero_height, "{note}: {plain} under {hero_height}");
            let (px, face) =
                crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
            let narrow = crate::tray_raster::ui_text_width_for(face, note, px) + 1.0;
            assert!(
                !fits_in_hero(note, narrow),
                "{note}: Hero type is wider than {narrow}"
            );
            assert_eq!(drawn(note, narrow, 400.0).1, plain, "{note}: narrow");
        }
        let context = format!("{scale}\u{d7}");
        let (style, before) = sessions_at(reading());
        assert_eq!(
            style,
            Some(StyleRef::Primary),
            "{context}: reading… is plain"
        );
        let (style, empty) = sessions_at(read(0));
        assert_eq!(
            style,
            Some(StyleRef::Primary),
            "{context}: nothing yet is plain"
        );
        let (style, after) = sessions_at(read(3));
        assert_eq!(
            style,
            Some(StyleRef::Hero),
            "{context}: the counts are Hero"
        );
        assert_eq!(
            (before, empty),
            (after, after),
            "{context}: SESSIONS stands where it stood as the counts come"
        );
    });
}

fn fits_in_hero(text: &str, width: f32) -> bool {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Hero);
    crate::tray_raster::ui_text_width_for(face, text, px) <= width
}

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

#[test]
fn a_rows_day_its_show_and_its_landing() {
    use crate::native_app::{HarnessRequest, SemanticInput};
    use crate::native_ui::{StyleRef, UiContent};
    let (mut runtime, instance, view) = harness_route(fixture_with_a_day(false));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/session/7/today").label,
        "today: waited for you once, 1m at 18:29 \u{b7} 3 approved"
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
        let _ = compile_settings_view(runtime, instance, view, &view_cx());
        let focus = {
            let Some(AppViewState::Settings(settings)) = runtime.view_state(view) else {
                unreachable!()
            };
            settings.common.last_focus.clone()
        };
        runtime
            .dispatch(instance, view, AppEvent::FocusChanged(focus))
            .unwrap();
        let pages = every_page(runtime, instance, view, &view_cx());
        let selected = node(&pages, "harness/session/7/show")
            .state
            .expect("a control")
            .selected;
        let accent = pages.iter().find_map(|page| painted(page, "harness/session/7/block")).map(|n| {
            matches!(&n.content, UiContent::Group(group) if group.style == StyleRef::Primary)
        });
        (selected, accent)
    };
    assert_eq!(
        land(&mut runtime, Some("session/7")),
        (true, Some(true)),
        "landed"
    );
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

#[test]
fn a_landed_rows_card_holds_its_whole_block_clear_of_its_words() {
    use crate::harness_glass::Stamp;
    use crate::native_app::SemanticInput;
    use crate::native_ui::{StyleRef, UiContent};
    use aterm_agent::harness::acts::Act;
    let bare = fixture();
    let mut full = fixture_with_a_day(true);
    full.sessions[0].glass.last = Some((
        Act::Approved,
        Stamp {
            clock: crate::claude_lights::Clock::of_minute(14 * 60 + 5),
            day: 0,
        },
    ));
    let shapes = std::sync::Mutex::new(std::collections::BTreeSet::new());
    for (name, state, timer) in [("bare", &bare, false), ("full", &full, true)] {
        at_each_text_scale(&[1.0, 2.0], "the landed row", |scale| {
            for width in [320.0, 584.0, 760.0, 1_200.0, 2_400.0] {
                let context = format!("{name} {scale}\u{d7} {width}");
                let (mut runtime, instance, view) = harness_route(state.clone());
                if timer {
                    press_for_requests(
                        &mut runtime,
                        instance,
                        view,
                        "harness/session/7/timer",
                        None,
                    );
                }
                let cx = view_cx_at(width, 1_400.0);
                let rects = |pages: &[crate::native_ui::CompiledUi]| {
                    pages
                        .iter()
                        .flat_map(|page| page.paint.iter())
                        .filter(|n| n.key.as_str().starts_with("harness/session/7/"))
                        .map(|n| (n.key.as_str().to_string(), n.rect))
                        .collect::<std::collections::BTreeMap<_, _>>()
                };
                let accent = |pages: &[crate::native_ui::CompiledUi]| {
                    pages
                        .iter()
                        .flat_map(|page| page.paint.iter())
                        .filter(|n| {
                            matches!(&n.content, UiContent::Group(g) if g.style == StyleRef::Primary)
                                && n.key.as_str().starts_with("harness/session/")
                        })
                        .map(|n| n.key.as_str().to_string())
                        .collect::<Vec<_>>()
                };
                let before = [compile_settings_view(&runtime, instance, view, &cx)];
                assert!(
                    accent(&before).is_empty(),
                    "{context}: control, nothing landed"
                );
                let unlanded = rects(&before);
                press_for_requests(
                    &mut runtime,
                    instance,
                    view,
                    crate::harness_screen::HARNESS_LAND,
                    Some(SemanticInput::Text("session/7".to_string())),
                );
                let pages = [compile_settings_view(&runtime, instance, view, &cx)];
                assert_eq!(
                    accent(&pages),
                    ["harness/session/7/block"],
                    "{context}: the block alone is on the accent"
                );
                let block = pages
                    .iter()
                    .find_map(|page| painted(page, "harness/session/7/block"))
                    .expect("the block is drawn");
                assert_eq!(block.clip, block.rect, "{context}: the card is drawn whole");
                let landed = rects(&pages);
                assert_eq!(landed, unlanded, "{context}: nothing moved when it landed");
                let inside = |key: &str| landed.keys().any(|k| k == key);
                let mut lines = vec!["title", "detail", "show"];
                if name == "full" {
                    lines.extend(["state", "today", "timer-row/0"]);
                }
                for line in lines {
                    let key = format!("harness/session/7/{line}");
                    assert!(inside(&key), "{context}: {key} is drawn");
                }
                let (b, r) = (block.rect, block.rect.right());
                for (key, rect) in &landed {
                    if key.ends_with("/block") {
                        continue;
                    }
                    assert!(
                        rect.x >= b.x + 8.0 - 0.01
                            && rect.right() <= r - 8.0 + 0.01
                            && rect.y >= b.y + 3.0 - 0.01
                            && rect.y + rect.height <= b.y + b.height - 3.0 + 0.01,
                        "{context}: {key} {rect:?} clear of the card's outline {b:?}"
                    );
                }
                let heading = pages
                    .iter()
                    .find_map(|page| painted(page, "harness/sessions/heading"))
                    .expect("SESSIONS' heading");
                let title = landed["harness/session/7/title"];
                assert!(
                    (title.x - heading.rect.x).abs() < 0.01,
                    "{context}: the title at the heading's edge, {title:?} {:?}",
                    heading.rect
                );
                let cruise = landed["harness/session/7/autopilot/cruise"];
                shapes
                    .lock()
                    .unwrap()
                    .insert(if cruise.y >= title.y + title.height - 0.01 {
                        "stacked"
                    } else {
                        "one line"
                    });
            }
        });
    }
    assert_eq!(
        shapes.into_inner().unwrap().into_iter().collect::<Vec<_>>(),
        ["one line", "stacked"],
        "both row layouts were drawn"
    );
}

#[test]
fn a_landing_draws_all_of_the_block_it_selects() {
    use crate::native_app::SemanticInput;
    let full = fixture_with_a_day(true);
    let tall = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[1.0, 2.0], "the landed block", |scale| {
        for (width, height) in [
            (584.0, 348.0),
            (744.0, 420.0),
            (1_200.0, 348.0),
            (584.0, 600.0),
            (744.0, 480.0),
            (744.0, 528.0),
            (744.0, 600.0),
            (1_200.0, 456.0),
        ] {
            for timer in [false, true] {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height}, timer open={timer}");
                let (mut runtime, instance, view) = harness_route(full.clone());
                if timer {
                    press_for_requests(
                        &mut runtime,
                        instance,
                        view,
                        "harness/session/7/timer",
                        None,
                    );
                }
                press_for_requests(
                    &mut runtime,
                    instance,
                    view,
                    crate::harness_screen::HARNESS_LAND,
                    Some(SemanticInput::Text("session/7".to_string())),
                );
                let page =
                    compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
                let block = painted(&page, "harness/session/7/block")
                    .unwrap_or_else(|| panic!("{context}: the landed block is drawn"));
                let list = painted(&page, "settings/list").expect("the page's list");
                assert!(
                    (block.clip.y - block.rect.y).abs() < 0.01,
                    "{context}: the block's top is drawn, {:?} of {:?}",
                    block.clip,
                    block.rect
                );
                let whole = block.clip == block.rect && !block.peek && block.fade.is_none();
                if !whole {
                    assert!(
                        block.rect.height > list.clip.height - 60.0
                            && block.rect.y < list.clip.y + 60.0,
                        "{context}: a block {:?} (drawn {:?}, peek={}, fade={:?}) is whole in \
                         the list's room, or about as tall as the list {:?} and at its top",
                        block.rect,
                        block.clip,
                        block.peek,
                        block.fade,
                        list.clip
                    );
                    *tall.lock().unwrap() += 1;
                }
                let b = page
                    .paint
                    .iter()
                    .filter(|n| {
                        n.key
                            .as_str()
                            .strip_prefix("harness/session/7/block")
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with("/part-"))
                    })
                    .map(|n| n.rect)
                    .fold(block.rect, |outline, part| outline.union(part));
                for node in page.paint.iter().filter(|n| {
                    let key = n.key.as_str();
                    key.starts_with("harness/session/7/")
                        && !key.ends_with("/block")
                        && !key.contains("/block/part-")
                }) {
                    let r = node.rect;
                    assert!(
                        r.x >= b.x + 4.0 - 0.01 && r.right() <= b.right() - 4.0 + 0.01,
                        "{context}: {} {r:?} clear of the outline {b:?}",
                        node.key.as_str()
                    );
                }
            }
        }
    });
    assert!(
        *tall.lock().unwrap() < 16,
        "most of the hosts hold the whole block (32 drawn)"
    );
}

#[test]
fn a_landing_draws_all_of_a_block_that_heads_a_run_of_its_card() {
    use crate::native_app::SemanticInput;
    let mut state = fixture_with_a_day(true);
    let mut third = state.sessions[1].clone();
    third.local_id = 9;
    third.sid = "s-9".to_string();
    third.place = "in tab 2".to_string();
    state.sessions.push(third);
    let mut policy = SupervisorConfig::default();
    policy.set("land_at", "18:00").expect("a known key");
    state.config = config_rows(&policy);
    let at = crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: None,
    };
    state.land_at_offer = Some(at);
    for session in &mut state.sessions {
        session.word = Some(Autopilot::Cruise);
        session.view = AutopilotView::Cruise;
        session.armed_at_ms = Some(1_792_173_600_000);
        session.glass.land_at = Some(at);
    }
    let headed = std::sync::Mutex::new(0_usize);
    at_each_text_scale(&[2.0], "the run's landed block", |scale| {
        for height in [600.0, 628.0, 660.0] {
            for id in [7_u64, 8, 9] {
                let context = format!("{scale}\u{d7} 504\u{d7}{height}, session {id}");
                let (mut runtime, instance, view) = harness_route(state.clone());
                press_for_requests(
                    &mut runtime,
                    instance,
                    view,
                    &format!("harness/session/{id}/timer"),
                    None,
                );
                press_for_requests(
                    &mut runtime,
                    instance,
                    view,
                    crate::harness_screen::HARNESS_LAND,
                    Some(SemanticInput::Text(format!("session/{id}"))),
                );
                let page =
                    compile_settings_view(&runtime, instance, view, &view_cx_at(504.0, height));
                let room = {
                    let Some(AppViewState::Settings(settings)) = runtime.view_state(view) else {
                        unreachable!()
                    };
                    settings.scroll_plan().expect("the page's list").room()
                };
                let key = format!("harness/session/{id}/block");
                let block = painted(&page, &key)
                    .unwrap_or_else(|| panic!("{context}: the landed block is drawn"));
                {
                    let Some(AppViewState::Settings(settings)) = runtime.view_state(view) else {
                        unreachable!()
                    };
                    assert_eq!(
                        settings.harness_timer_open,
                        Some(id),
                        "{context}: its Timer is open"
                    );
                }
                let card = page
                    .paint
                    .iter()
                    .filter(|n| n.key.as_str() == "harness/sessions")
                    .find(|n| {
                        n.rect.y <= block.rect.y + 0.01
                            && n.rect.y + n.rect.height >= block.rect.y + block.rect.height - 0.01
                    })
                    .unwrap_or_else(|| panic!("{context}: the block's card is drawn"));
                let lead = block.rect.y - card.rect.y;
                if lead < 60.0
                    && lead + block.rect.height > room + 0.01
                    && block.rect.height <= room + 0.01
                {
                    *headed.lock().unwrap() += 1;
                }
                if block.rect.height <= room + 0.01 {
                    assert!(
                        block.clip == block.rect && !block.peek && block.fade.is_none(),
                        "{context}: a block {:?} no taller than the room ({room}) is drawn \
                         whole (drawn {:?}, peek={}, fade={:?}; its card {} {:?})",
                        block.rect,
                        block.clip,
                        block.peek,
                        block.fade,
                        card.key.as_str(),
                        card.rect
                    );
                }
                let (drawn, clip) = (block.rect, block.clip);
                let focus = {
                    let Some(AppViewState::Settings(settings)) = runtime.view_state(view) else {
                        unreachable!()
                    };
                    settings.common.last_focus.clone()
                };
                runtime
                    .dispatch(instance, view, AppEvent::FocusChanged(focus))
                    .unwrap();
                {
                    let Some(AppViewState::Settings(settings)) = runtime.view_state(view) else {
                        unreachable!()
                    };
                    assert!(
                        settings.page_reveal.is_none(),
                        "{context}: the landing let go once drawn"
                    );
                }
                let mut moved = state.clone();
                let other = moved
                    .sessions
                    .iter_mut()
                    .find(|session| session.local_id != id)
                    .expect("another session");
                other.word = Some(Autopilot::Land);
                other.view = AutopilotView::Land;
                assert!(runtime.replace_settings_harness(moved, 2));
                let again =
                    compile_settings_view(&runtime, instance, view, &view_cx_at(504.0, height));
                let block = painted(&again, &key)
                    .unwrap_or_else(|| panic!("{context}: the landed block is drawn again"));
                assert_eq!(
                    (block.rect, block.clip),
                    (drawn, clip),
                    "{context}: the landed block holds its place through a republish"
                );
            }
        }
    });
    assert!(
        *headed.lock().unwrap() >= 1,
        "some landed block heads its card's run, and with the caption's lead is taller than \
         the room"
    );
}

fn fixture_long_facts() -> HarnessState {
    let mut state = fixture_led(true);
    state.words = "Automatic, limited by 2 settings in aterm.toml".to_string();
    state.ab.ends.landed = 4;
    let mut today = HarnessStats::new(0);
    today.rows = 300;
    today.escalated = 322;
    today.human_wait_ms = 18 * 3_600_000 + 47 * 60_000;
    let mut week = HarnessStats::new(0);
    week.rows = 900;
    week.approved = 41;
    week.escalated = 799;
    week.human_wait_ms = 1123 * 3_600_000 + 53 * 60_000;
    state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
        today,
        week,
        1_000_000_000,
        Some(0),
        &|_| Some(0),
    )));
    state
}

/// The fact cards keyed by `cards` on `pages`, read as they paint.
pub(super) fn fact_cards_whole<'a>(
    pages: &[crate::native_ui::CompiledUi],
    cards: &[&'a str],
    context: &str,
) -> std::collections::BTreeMap<&'a str, f32> {
    fact_cards_read(pages, cards, true, context)
}

/// [`fact_cards_whole`], its values' lines held whole only where `values` says so.
pub(super) fn fact_cards_read<'a>(
    pages: &[crate::native_ui::CompiledUi],
    cards: &[&'a str],
    values: bool,
    context: &str,
) -> std::collections::BTreeMap<&'a str, f32> {
    use crate::native_ui::UiContent;
    let mut labels = 0;
    let mut value_x: std::collections::BTreeMap<&str, std::collections::BTreeSet<i64>> =
        std::collections::BTreeMap::new();
    for node in pages.iter().flat_map(|page| page.paint.iter()) {
        let key = node.key.as_str();
        let Some(card) = cards.iter().find(|card| key.starts_with(*card)) else {
            continue;
        };
        let UiContent::Text(text) = &node.content else {
            continue;
        };
        let label = key.starts_with(&format!("{card}label/"));
        let value = key.starts_with(&format!("{card}value/"));
        if (label || (value && values)) && !text.text.is_empty() {
            assert!(
                !text.text.contains('\u{2026}'),
                "{context}: {key} paints {:?} whole",
                text.text
            );
        }
        if label && !text.text.is_empty() {
            labels += 1;
        }
        if value && !key.contains("/line-") {
            value_x
                .entry(*card)
                .or_default()
                .insert((node.rect.x * 2.0).round() as i64);
        }
    }
    assert!(
        labels >= cards.len(),
        "{context}: the cards' labels are drawn ({labels})"
    );
    assert_eq!(value_x.len(), cards.len(), "{context}: every card's values");
    value_x
        .into_iter()
        .map(|(card, xs)| {
            assert_eq!(xs.len(), 1, "{context}: one value column in {card}, {xs:?}");
            (card, *xs.iter().next().expect("a value") as f32 / 2.0)
        })
        .collect()
}

#[test]
fn the_fact_cards_labels_are_whole_at_large_type() {
    let state = fixture_long_facts();
    at_each_text_scale(&[1.0, 1.5, 2.0], "the fact cards' labels", |scale| {
        for (width, height) in [
            (504.0, 628.0),
            (584.0, 348.0),
            (744.0, 420.0),
            (924.0, 640.0),
            (1_200.0, 820.0),
        ] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let (mut runtime, instance, view) = harness_route(state.clone());
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            let x_of = fact_cards_whole(
                &pages,
                &["harness/status/", "harness/ab/", "harness/did/"],
                &context,
            );
            if scale > 1.2 {
                // A card whose labels leave its values no column stacks each value under its
                // label (the Linux lane's Noto Sans stacks DID at 2x in 504pt): STATUS's column
                // is then its own however wide DID's labels are.
                let label_x = |card: &str| {
                    pages
                        .iter()
                        .flat_map(|page| page.paint.iter())
                        .find(|node| node.key.as_str().starts_with(&format!("{card}label/")))
                        .map(|node| node.rect.x)
                };
                let did_stacked =
                    label_x("harness/did/").is_some_and(|x| (x_of["harness/did/"] - x).abs() < 0.5);
                assert!(
                    did_stacked || x_of["harness/status/"] + 1.0 < x_of["harness/did/"],
                    "{context}: STATUS's column is its own, {x_of:?} (DID's labels at {:?})",
                    label_x("harness/did/")
                );
            }
            if (scale - 1.0).abs() < 0.01 {
                assert!(
                    x_of.values()
                        .all(|x| (x - x_of["harness/status/"]).abs() < 0.01),
                    "{context}: About's column in every card at 1\u{d7}, {x_of:?}"
                );
                let x = x_of["harness/status/"];
                let label = pages
                    .iter()
                    .flat_map(|page| page.paint.iter())
                    .find(|n| n.key.as_str().starts_with("harness/status/label/"))
                    .expect("a STATUS label")
                    .rect;
                assert!(
                    (x - label.x - 104.0 - 10.0).abs() < 0.5,
                    "{context}: About's column at 1\u{d7}, {label:?} then {x}"
                );
            }
        }
    });
}

#[test]
fn tab_controls_summary_keeps_its_labels_whole_at_large_type() {
    use crate::harness_screen::{TabComputer, TabControlState};
    use crate::native_app::HarnessRequest;
    let mut state = fixture();
    state.tabs = TabControlState {
        computers: vec![TabComputer {
            id: "local".to_string(),
            name: "MacBook-Pro (this Mac)".to_string(),
            address: "local".to_string(),
            os_version: Some("macos".to_string()),
            status: "online".to_string(),
            vitals: "CPU 14% \u{b7} GPU 6% \u{b7} RAM 38.2/128.0 GiB (30%) \u{b7} Disk 1.12/7.28 \
                     TiB (15%) \u{b7} up 5d 11h"
                .to_string(),
            instances: Some(1),
            tabs: Some(2),
            ai_clis: Some(1),
            active_ai: Some(1),
            session_scope: Default::default(),
        }],
        tabs: Vec::new(),
        fleet: crate::harness_screen::FleetScope::Account,
        lan_waits: None,
    };
    at_each_text_scale(&[1.0, 2.0], "Tab Control's summary", |scale| {
        for (width, height) in [(504.0, 628.0), (924.0, 640.0)] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let (mut runtime, instance, view) = setup();
            assert!(runtime.replace_settings_harness(state.clone(), 1));
            let outcome = runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: route_action(SettingsRoute::Tabs),
                        value: None,
                    }),
                )
                .unwrap();
            assert!(
                outcome
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, AppEffect::Harness(HarnessRequest::Refresh)))
            );
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            assert_eq!(
                node(&pages, "tabs/summary/row/system-vitals").label,
                format!(
                    "System Vitals: this Mac: {} \u{2014} fleet telemetry: 1 full kernel/SSH \
                     \u{b7} 0 live LAN RTT",
                    state.tabs.computers[0].vitals
                ),
                "{context}"
            );
            fact_cards_read(&pages, &["tabs/summary/"], width > 600.0, &context);
        }
    });
}

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

#[test]
fn a_rows_words_keep_its_place_whole_and_its_buttons_their_faces() {
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
                let pages = pages_through(
                    &mut runtime,
                    instance,
                    view,
                    &view_cx_at(width, 900.0),
                    "harness/session",
                );
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
                    let text = painted_text(&pages, &format!("harness/session/{id}/detail"));
                    assert!(
                        text.starts_with(&place),
                        "{context}: the place whole, {text:?}"
                    );
                    for face in faces {
                        let painted = button_face(&pages, &format!("harness/session/{id}/{face}"));
                        assert!(
                            !painted.ends_with('\u{2026}') && !painted.is_empty(),
                            "{context}: {face}'s face whole, {painted:?}"
                        );
                    }
                    if width > 2_000.0 && scale < 1.25 {
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

fn fixture_deciding(n: usize, total: u64) -> HarnessState {
    use aterm_agent::harness::stats::{Decision, DecisionKind};
    let mut state = fixture();
    let report = state.stats.report.take().expect("a reading");
    let mut report = (*report).clone();
    let mut today = HarnessStats::per_session(0);
    today.declined = total;
    let decided = today.decided.as_mut().expect("per session");
    decided.total = total;
    decided.rows = (0..n)
        .map(|k| Decision {
            t_ms: (18 * 3_600 + i64::try_from(k).expect("few") * 60) * 1_000,
            sid: if k % 2 == 0 { "s-7" } else { "s-gone" }.to_string(),
            kind: DecisionKind::Declined,
            asked: Box::from(format!("rm -rf build-{k}")),
            said: false,
            did: Box::from(""),
            why: Some(Box::from("Nothing outside the worktree is removed.")),
            times: 1,
        })
        .collect();
    report.today = today;
    report = HarnessStatsReport::new(
        report.today,
        report.week,
        report.collected_unix_ms,
        report.offset_s,
        &|_| Some(0),
    );
    state.stats.report = Some(std::sync::Arc::new(report));
    state
}

fn decided_keys(state: &HarnessState) -> Vec<String> {
    state
        .stats
        .report
        .as_deref()
        .and_then(|report| report.today.decided.as_ref())
        .map(|decided| {
            decided
                .rows
                .iter()
                .map(|decision| format!("harness/decided/{}", decision.id()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn decided_for_you_lists_its_keys_after_sessions() {
    use crate::native_app::HarnessRequest;
    let state = fixture_deciding(5, 5);
    let k = decided_keys(&state);
    let (mut runtime, instance, view) = harness_route(state);
    let keys = |pages: &[crate::native_ui::CompiledUi]| -> Vec<String> {
        pages
            .iter()
            .flat_map(|page| page.semantics.iter())
            .map(|node| node.key.as_str().to_string())
            .collect()
    };
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let all = keys(&pages);
    let at = |key: &str| all.iter().position(|k| k == key);
    let sessions = at("harness/sessions").expect("SESSIONS");
    let decided = at("harness/decided").expect("DECIDED FOR YOU");
    let switches = at("harness/switches").expect("SUPERVISION");
    assert!(sessions < decided && decided < switches, "{all:#?}");
    assert_eq!(
        node(&pages, "harness/decided/heading").label,
        "DECIDED FOR YOU"
    );
    for key in &k[2..] {
        assert!(at(key).is_some(), "row {key}");
    }
    assert!(at(&k[1]).is_none() && at(&k[0]).is_none());
    assert_eq!(
        node(&pages, &k[4]).label,
        "18:04 fix the build: declined rm -rf build-4: Nothing outside the worktree is removed."
    );
    assert_eq!(
        node(&pages, &k[3]).label,
        "18:03 A closed session: declined rm -rf build-3: Nothing outside the worktree is removed."
    );
    let show = node(&pages, &format!("{}/show", k[4]));
    assert_eq!(show.label, "Show tab of fix the build");
    assert_eq!(
        show.action.as_ref().map(ActionId::as_str),
        Some("harness/session/7/show"),
        "the SESSIONS row's own action, behind the prefix's Owner fence"
    );
    assert!(at(&format!("{}/show", k[3])).is_none(), "CONTROL: closed");
    let disclosure = node(&pages, "harness/decided/all");
    assert_eq!(disclosure.label, "Show all 5 choices made for you today");
    assert_eq!(disclosure.state.expect("a control").expanded, Some(false));
    assert!(
        node(&pages, "harness/status/today")
            .label
            .ends_with("5 choices made for you"),
        "{}",
        node(&pages, "harness/status/today").label
    );
    assert_eq!(
        press_for_requests(&mut runtime, instance, view, "harness/session/7/show", None),
        [HarnessRequest::Show { session: 7 }]
    );
    press_for_requests(
        &mut runtime,
        instance,
        view,
        crate::native_settings::harness_page::HARNESS_DECIDED_ALL,
        None,
    );
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let all = keys(&pages);
    for key in &k {
        assert!(all.contains(key), "row {key}: {all:#?}");
    }
    let disclosure = node(&pages, "harness/decided/all");
    assert_eq!(disclosure.state.expect("a control").expanded, Some(true));
    assert_eq!(
        disclosure.label,
        "Show the newest 3 choices made for you, only"
    );
}

/// SHOWING ALL OF DECIDED FOR YOU ADDS ROWS UNDER THE ONES SHOWN AND RE-LAYS NONE OF THEM: the
/// card's columns (the clock's width, room for `Show tab`) and its padding are measured from every
/// choice it holds, so the newest three keep their width, their lines and their place when the
/// older ones open under them. Before, the columns were measured from the rows shown: older
/// choices with a live tab (or a wider clock) narrowed the sentence column the moment they
/// opened, so the newest rows wrapped again and grew (one line to two at 584pt, two to three at
/// 2x) under the pointer, and the disclosure pressed moved with them.
#[test]
fn showing_all_decided_choices_re_lays_none_of_the_ones_shown() {
    use aterm_agent::harness::stats::{Decision, DecisionKind};
    let mut state = fixture();
    let report = state.stats.report.take().expect("a reading");
    let mut report = (*report).clone();
    let mut today = HarnessStats::per_session(0);
    today.declined = 5;
    let decided = today.decided.as_mut().expect("per session");
    decided.total = 5;
    // The two oldest choices were made at 9 in a session whose tab is still open; the newest
    // three at 18 in one that closed.
    decided.rows = (0..5_i64)
        .map(|k| Decision {
            t_ms: (if k < 2 { 9 } else { 18 } * 3_600 + k * 60) * 1_000,
            sid: if k < 2 { "s-7" } else { "s-gone" }.to_string(),
            kind: DecisionKind::Declined,
            asked: Box::from(format!("rm -rf build-{k}")),
            said: false,
            did: Box::from(""),
            why: Some(Box::from("Nothing outside the worktree is removed.")),
            times: 1,
        })
        .collect();
    report.today = today;
    report = HarnessStatsReport::new(
        report.today,
        report.week,
        report.collected_unix_ms,
        report.offset_s,
        &|_| Some(0),
    );
    state.stats.report = Some(std::sync::Arc::new(report));
    let k = decided_keys(&state);
    let relaid = std::sync::Mutex::new(Vec::<String>::new());
    at_each_text_scale(&[1.0, 2.0], "showing all decided choices", |scale| {
        for width in [320.0_f32, 504.0, 584.0, 849.0, 1_200.0] {
            let context = format!("{scale}\u{d7} {width}");
            let cx = view_cx_at(width, 2_400.0);
            let (mut runtime, instance, view) = harness_route(state.clone());
            // Each shown row's parts as laid inside their card: where they stand and how large.
            let shape = |ui: &crate::native_ui::CompiledUi| {
                let card = ui
                    .paint
                    .iter()
                    .find(|n| n.key.as_str() == "harness/decided")
                    .map(|n| n.rect)
                    .unwrap_or_else(|| panic!("{context}: DECIDED FOR YOU is drawn"));
                ui.paint
                    .iter()
                    .filter(|n| {
                        k[2..]
                            .iter()
                            .any(|key| n.key.as_str().starts_with(key.as_str()))
                    })
                    .map(|n| {
                        (
                            n.key.as_str().to_string(),
                            (
                                n.rect.x - card.x,
                                n.rect.y - card.y,
                                n.rect.width,
                                n.rect.height,
                            ),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let closed = compile_settings_view(&runtime, instance, view, &cx);
            let before = shape(&closed);
            assert!(
                k[..2]
                    .iter()
                    .all(|key| closed.semantic(&UiKey::new(key.as_str())).is_none())
                    && before.len() >= 3,
                "{context}: PRECONDITION: the newest three are shown, the older two are not"
            );
            press_for_requests(
                &mut runtime,
                instance,
                view,
                crate::native_settings::harness_page::HARNESS_DECIDED_ALL,
                None,
            );
            let open = compile_settings_view(&runtime, instance, view, &cx);
            assert!(
                k[..2]
                    .iter()
                    .all(|key| open.semantic(&UiKey::new(key.as_str())).is_some()),
                "{context}: PRECONDITION: Show all opens the older two"
            );
            let after = shape(&open);
            if after != before {
                relaid.lock().unwrap().push(format!(
                    "{context}: {:?}",
                    before
                        .iter()
                        .zip(&after)
                        .filter(|(b, a)| b != a)
                        .collect::<Vec<_>>()
                ));
            }
        }
    });
    let relaid = relaid.into_inner().unwrap();
    assert!(
        relaid.is_empty(),
        "the rows shown were re-laid when the rest opened under them:\n{}",
        relaid.join("\n")
    );
}

#[test]
fn decided_for_you_says_the_newest_of_how_many() {
    let (mut runtime, instance, view) = harness_route(fixture_deciding(64, 80));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/decided/all").label,
        "Show the newest 64 of 80 choices made for you today"
    );
    let state = fixture_deciding(3, 3);
    let k = decided_keys(&state);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        pages
            .iter()
            .all(|page| page.semantic(&UiKey::new("harness/decided/all")).is_none()),
        "CONTROL: no disclosure for three"
    );
    assert!(
        pages
            .iter()
            .any(|page| page.semantic(&UiKey::new(k[0].as_str())).is_some())
    );
}

#[test]
fn no_decided_card_on_a_day_without_a_choice() {
    let (mut runtime, instance, view) = harness_route(fixture_deciding(0, 0));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        pages
            .iter()
            .flat_map(|page| page.semantics.iter())
            .all(|node| !node.key.as_str().starts_with("harness/decided")),
        "no card"
    );
    let (mut runtime, instance, view) = harness_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        pages
            .iter()
            .all(|page| page.semantic(&UiKey::new("harness/decided")).is_none()),
        "a reading with no decisions kept"
    );
    let (mut runtime, instance, view) = harness_route(fixture_deciding(1, 1));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        pages
            .iter()
            .any(|page| page.semantic(&UiKey::new("harness/decided")).is_some()),
        "CONTROL: one decision, one card"
    );
}

#[test]
fn a_decided_show_tab_raises_a_session_sessions_does_not_list() {
    use crate::native_app::HarnessRequest;
    let mut state = fixture_deciding(2, 2);
    state.decided_open = vec![crate::harness_screen::OpenSession {
        sid: "s-gone".to_string(),
        local_id: 9,
        title: "zsh".to_string(),
    }];
    let k = decided_keys(&state);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let show = node(&pages, &format!("{}/show", k[1]));
    assert_eq!(show.label, "Show tab of zsh");
    assert_eq!(
        show.action.as_ref().map(ActionId::as_str),
        Some("harness/session/9/show")
    );
    assert_eq!(
        press_for_requests(&mut runtime, instance, view, "harness/session/9/show", None),
        [HarnessRequest::Show { session: 9 }]
    );
    assert!(
        press_for_requests(
            &mut runtime,
            instance,
            view,
            "harness/session/11/show",
            None
        )
        .is_empty(),
        "CONTROL: a session neither lists"
    );
}

#[test]
fn decided_for_you_is_whole_on_every_host() {
    use aterm_agent::harness::stats::{Decision, DecisionKind};
    let long = "Should divide() return true division (float) or floor division, and what about \
                negative operands, rounding toward zero or toward negative infinity?";
    let mut state = fixture_deciding(0, 0);
    let report = state.stats.report.take().expect("a reading");
    let mut report = (*report).clone();
    let decided = report.today.decided.as_mut().expect("decisions");
    decided.rows = (0..12)
        .map(|k| Decision {
            t_ms: (9 * 3_600 + k * 60) * 1_000,
            sid: if k % 3 == 0 { "s-gone" } else { "s-7" }.to_string(),
            kind: DecisionKind::Chose,
            asked: aterm_agent::harness::stats::agent_words(long),
            said: false,
            did: aterm_agent::harness::stats::agent_words(long),
            why: Some(Box::from(
                "no option was marked Recommended, so it took the first",
            )),
            times: 1,
        })
        .collect();
    decided.total = 12;
    report.today.answered = 12;
    report = HarnessStatsReport::new(
        report.today,
        report.week,
        report.collected_unix_ms,
        report.offset_s,
        &|_| Some(0),
    );
    state.stats.report = Some(std::sync::Arc::new(report));
    let k = decided_keys(&state);
    let hosts = [
        (320.0, 568.0),
        (584.0, 348.0),
        (849.0, 513.0),
        (1_200.0, 900.0),
    ];
    at_each_text_scale(&[1.0, 1.25, 2.0], "Decided for you", |scale| {
        let (mut runtime, instance, view) = harness_route(state.clone());
        {
            let Some(AppViewState::Settings(view_state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            view_state.harness_decided_open = true;
        }
        for &(width, height) in &hosts {
            let cx = view_cx_at(width, height);
            let pages = pages_through(&mut runtime, instance, view, &cx, "harness/decided");
            for (page, compiled) in pages.iter().enumerate() {
                let context = format!("{scale}\u{d7} {width}\u{d7}{height} page {page}");
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
            for key in &k {
                assert!(
                    pages
                        .iter()
                        .any(|page| page.semantic(&UiKey::new(key.as_str())).is_some()),
                    "{scale}\u{d7} {width}\u{d7}{height}: row {key}"
                );
            }
            let wrapped = pages
                .iter()
                .any(|page| painted(page, &format!("{}/line/1", k[11])).is_some());
            assert!(
                wrapped,
                "{scale}\u{d7} {width}\u{d7}{height}: the sentence wraps"
            );
        }
        {
            let Some(AppViewState::Settings(view_state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            view_state.harness_decided_open = false;
        }
        let pages = pages_through(&mut runtime, instance, view, &view_cx(), "harness/decided");
        let listed = k
            .iter()
            .filter(|key| {
                pages
                    .iter()
                    .any(|page| page.semantic(&UiKey::new(key.as_str())).is_some())
            })
            .count();
        assert_eq!(listed, 3);
    });
}

#[test]
fn tab_control_route_renders_computers_sessions_and_live_ai_cli_status() {
    use crate::harness_screen::{TabComputer, TabControlState, TabSpreadsheetRow};
    use crate::native_app::HarnessRequest;
    let mut state = fixture();
    state.tabs = TabControlState {
        computers: vec![
            TabComputer {
                id: "local".to_string(),
                name: "MacBook-Pro (this Mac)".to_string(),
                address: "local".to_string(),
                os_version: Some("macos".to_string()),
                status: "online".to_string(),
                vitals: "CPU 14% ▂▂▃▂ · GPU 6% ▁▁▂▁ · RAM 38.2/128.0 GiB (30%) · Disk 1.12/7.28 TiB (15%) · up 5d 11h".to_string(),
                instances: Some(1),
                tabs: Some(2),
                ai_clis: Some(1),
                active_ai: Some(1),
                session_scope: Default::default(),
            },
            TabComputer {
                id: "studio-m4".to_string(),
                name: "studio-m4".to_string(),
                address: "192.168.1.42".to_string(),
                os_version: Some("macOS 15.4".to_string()),
                status: "online".to_string(),
                vitals: String::new(),
                instances: Some(1),
                tabs: Some(1),
                ai_clis: Some(1),
                active_ai: Some(0),
                session_scope: Default::default(),
            },
        ],
        tabs: vec![
            TabSpreadsheetRow {
                key: "local/7".to_string(),
                computer_id: "local".into(),
                computer: "this Mac".to_string(),
                instance: "this aterm (pid 1000)".to_string(),
                place: "Tab 2".to_string(),
                sid: "s-7".to_string(),
                title: "fix the build".to_string(),
                state: "Running".to_string(),
                program: "Claude Code".to_string(),
                is_ai_cli: true,
                active_ai: Some(true),
                ai_status: "working: Bash · targo test (14s)".to_string(),
                autopilot: "land (supervised)".to_string(),
                human: "12s ago".to_string(),
                vitals: "CPU 8.4% · RSS 142 MiB".to_string(),
                show_local_id: Some(7),
                cached_observation: None,
            },
            TabSpreadsheetRow {
                key: "remote/studio-m4/4120/1".to_string(),
                computer_id: "studio-m4".into(),
                computer: "studio-m4".to_string(),
                instance: "aterm-4120".to_string(),
                place: "Tab 1*".to_string(),
                sid: "s-1".to_string(),
                title: "verify fleet".to_string(),
                state: "Idle".to_string(),
                program: "Codex".to_string(),
                is_ai_cli: true,
                active_ai: Some(false),
                ai_status: "waiting for a person".to_string(),
                autopilot: "cruise".to_string(),
                human: "3m ago".to_string(),
                vitals: String::new(),
                show_local_id: None,
                cached_observation: None,
            },
        ],
        fleet: crate::harness_screen::FleetScope::Account,
        lan_waits: None,
    };
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(state.clone(), 1));
    let outcome = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Tabs),
                value: None,
            }),
        )
        .unwrap();
    assert!(
        outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::Harness(HarnessRequest::Refresh))),
        "{:?}",
        outcome.effects
    );
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "tabs/summary/row/computers").label,
        "Computers: 2 known · 2 online"
    );
    assert_eq!(
        node(&pages, "tabs/summary/row/sessions").label,
        "Sessions: 2 aterm instances · 2 open tabs"
    );
    assert_eq!(
        node(&pages, "tabs/summary/row/ai-clis").label,
        "AI CLIs: 2 running · 1 active"
    );
    assert!(
        node(&pages, "tabs/computer/local")
            .label
            .contains("CPU 14% ▂▂▃▂ · GPU 6% ▁▁▂▁ · RAM 38.2/128.0 GiB (30%)"),
        "{}",
        node(&pages, "tabs/computer/local").label
    );
    assert!(
        node(&pages, "tabs/computer/studio-m4")
            .label
            .contains("1 aterm instance · 1 open tab · 1 AI CLI (0 active)")
    );
    assert!(
        node(&pages, "tabs/row/local/7")
            .label
            .contains("working: Bash · targo test (14s)")
            && node(&pages, "tabs/row/local/7")
                .label
                .contains("vitals: CPU 8.4% · RSS 142 MiB"),
        "{}",
        node(&pages, "tabs/row/local/7").label
    );
    assert!(
        node(&pages, "tabs/row/remote/studio-m4/4120/1")
            .label
            .contains("waiting for a person"),
        "{}",
        node(&pages, "tabs/row/remote/studio-m4/4120/1").label
    );
    let show = node(&pages, "tabs/show/7");
    assert_eq!(
        show.action.as_ref().map(ActionId::as_str),
        Some("tabs/show/7")
    );
    assert_eq!(
        press_for_requests(&mut runtime, instance, view, "tabs/show/7", None),
        [HarnessRequest::Show { session: 7 }]
    );
    for (width, height) in [(1_200.0, 820.0), (849.0, 513.0), (320.0, 568.0)] {
        let cx = view_cx_at(width, height);
        for (page, compiled) in every_page(&mut runtime, instance, view, &cx)
            .iter()
            .enumerate()
        {
            let context = format!("tabs {width}\u{d7}{height} page {page}");
            assert_zero_top_paint(compiled, &context);
            assert_all_hits_material_and_contained(compiled, &context);
        }
    }
    let mut duplicate_names = state.clone();
    for computer in &mut duplicate_names.tabs.computers {
        computer.name = "bench".into();
    }
    for tab in &mut duplicate_names.tabs.tabs {
        tab.computer = "bench".into();
        tab.instance = "aterm-4120".into();
    }
    assert!(runtime.replace_settings_harness(duplicate_names, 2));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_ne!(
        node(&pages, "tabs/session/0").key,
        node(&pages, "tabs/session/1").key,
        "different computer identities retain separate instance headers under equal display names"
    );
    state.tabs.computers.push(TabComputer {
        id: "lan/advertised-peer".to_string(),
        name: "advertised-peer".to_string(),
        address: "observed-peer.local".to_string(),
        os_version: Some("OS version unknown".to_string()),
        status: "advertised (lan)".to_string(),
        vitals: String::new(),
        instances: Some(0),
        tabs: Some(0),
        ai_clis: Some(0),
        active_ai: Some(0),
        session_scope: Default::default(),
    });
    assert!(runtime.replace_settings_harness(state, 3));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "tabs/computer/lan/advertised-peer").label,
        "advertised-peer · advertised (lan) · observed-peer.local — OS version unknown · sessions unobserved"
    );
    assert_eq!(
        node(&pages, "tabs/summary/row/computers").label,
        "Computers: 3 known · 2 online"
    );
    assert_eq!(
        node(&pages, "tabs/summary/row/sessions").label,
        "Sessions: 2 observed aterm instances · 2 open tabs observed"
    );
    assert_eq!(
        node(&pages, "tabs/summary/row/ai-clis").label,
        "AI CLIs: 2 observed · 1 active"
    );
}

#[test]
fn tab_group_activity_uses_typed_observations() {
    use crate::harness_screen::{TabControlState, TabSpreadsheetRow};
    let mut state = fixture();
    state.tabs = TabControlState {
        tabs: [(Some(true), "idle"), (Some(false), "working")]
            .into_iter()
            .enumerate()
            .map(|(index, (active_ai, display))| TabSpreadsheetRow {
                key: format!("local/{index}"),
                computer_id: "local".into(),
                computer: "local".into(),
                instance: "aterm-7".into(),
                program: "Claude Code".into(),
                is_ai_cli: true,
                active_ai,
                ai_status: display.into(),
                ..TabSpreadsheetRow::default()
            })
            .collect(),
        ..TabControlState::default()
    };
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(state.clone(), 1));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Tabs),
                value: None,
            }),
        )
        .unwrap();
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "tabs/summary/row/ai-clis")
            .label
            .ends_with("1 active")
    );
    assert!(node(&pages, "tabs/session/0").label.contains("(1 active)"));

    state.tabs.tabs[1].active_ai = None;
    assert!(runtime.replace_settings_harness(state.clone(), 2));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "tabs/summary/row/ai-clis")
            .label
            .ends_with("activity unobserved")
    );
    assert!(
        node(&pages, "tabs/session/0")
            .label
            .contains("(activity unobserved)")
    );

    for row in &mut state.tabs.tabs {
        row.active_ai = Some(false);
    }
    assert!(runtime.replace_settings_harness(state.clone(), 3));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "tabs/summary/row/ai-clis")
            .label
            .ends_with("0 active")
    );
    assert!(node(&pages, "tabs/session/0").label.contains("(0 active)"));

    let mut busy = state.tabs.tabs[0].clone();
    busy.active_ai = Some(true);
    let mut unknown = state.tabs.tabs[1].clone();
    unknown.is_ai_cli = false;
    unknown.cached_observation = Some(Default::default());
    state.tabs.tabs = vec![unknown];
    for (revision, add_busy) in [(4, false), (5, true)] {
        if add_busy {
            state.tabs.tabs.push(busy.clone());
        }
        assert!(runtime.replace_settings_harness(state.clone(), revision));
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        assert!(
            node(&pages, "tabs/summary/row/ai-clis")
                .label
                .contains("count unobserved")
        );
        assert!(
            node(&pages, "tabs/session/0")
                .label
                .contains("AI CLIs unobserved (activity unobserved)")
        );
    }
    state.tabs.tabs[0].cached_observation = None;
    state.tabs.tabs[0].program = "sh".into();
    assert!(runtime.replace_settings_harness(state, 6));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert!(
        node(&pages, "tabs/summary/row/ai-clis")
            .label
            .ends_with("1 active")
    );
    assert!(
        node(&pages, "tabs/session/0")
            .label
            .contains("1 AI CLI (1 active)")
    );
}

fn semantic_keys(pages: &[crate::native_ui::CompiledUi]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| page.semantics.iter())
        .map(|node| node.key.as_str().to_string())
        .collect()
}

#[test]
fn effect_comes_right_after_the_pages_actions() {
    let (mut runtime, instance, view) = harness_route(fixture_deciding(5, 5));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let all = semantic_keys(&pages);
    let at = |key: &str| {
        all.iter()
            .position(|k| k == key)
            .unwrap_or_else(|| panic!("{key}: {all:#?}"))
    };
    let order = [
        "harness/sessions",
        "harness/decided",
        "harness/actions",
        "harness/ab",
        "harness/switches",
        "harness/did",
        "harness/config",
        "harness/updates",
        "harness/update-actions",
    ]
    .map(at);
    assert!(order.is_sorted(), "{order:?}: {all:#?}");
    let list = node(&pages, "harness/actions").parent.clone();
    let mut cards: Vec<&str> = Vec::new();
    for node in pages.iter().flat_map(|page| page.semantics.iter()) {
        if node.parent == list && !cards.contains(&node.key.as_str()) {
            cards.push(node.key.as_str());
        }
    }
    let next = cards
        .iter()
        .position(|key| *key == "harness/actions")
        .and_then(|at| cards.get(at + 1));
    assert_eq!(next, Some(&"harness/ab"), "{cards:#?}");
}

#[test]
fn the_lead_says_what_it_measures_and_the_held_row_gives_way() {
    use crate::native_ui::{StyleRef, UiContent};
    let drawn = |pages: &[crate::native_ui::CompiledUi], key: &str| {
        pages
            .iter()
            .find_map(|page| painted(page, key))
            .map(|node| match &node.content {
                UiContent::Text(text) => (text.style, text.text.clone()),
                other => panic!("a text: {other:?}"),
            })
    };
    let (mut runtime, instance, view) = harness_route(fixture_led(true));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/ab/lead").label,
        "Working time held at a box or a question: <1% of 6h 10m on Cruise \u{b7} 14% of \
         25m off"
    );
    assert_eq!(
        drawn(&pages, "harness/ab/lead/caption"),
        Some((
            StyleRef::Primary,
            "Working time held at a box or a question".to_string()
        ))
    );
    for (key, want) in [
        ("cruise/share", (StyleRef::Hero, "<1%")),
        ("cruise/of", (StyleRef::Primary, "of 6h 10m on Cruise")),
        ("off/share", (StyleRef::Hero, "14%")),
        ("off/of", (StyleRef::Primary, "of 25m off")),
    ] {
        assert_eq!(
            drawn(&pages, &format!("harness/ab/lead/{key}")),
            Some((want.0, want.1.to_string())),
            "{key}"
        );
    }
    let all = semantic_keys(&pages);
    assert!(!all.iter().any(|k| k == "harness/ab/row/held"), "{all:#?}");
    assert_eq!(
        node(&pages, "harness/ab/row/ends").label,
        "Ends: Land landed 5 \u{b7} Land to landed: about 6m (5)"
    );
    assert!(
        all.iter().position(|k| k == "harness/ab/lead")
            < all.iter().position(|k| k == "harness/ab/row/boxes"),
        "the lead is the card's first block"
    );
    let (mut runtime, instance, view) = harness_route(fixture_led(false));
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "harness/ab/lead").label,
        crate::harness_ab::LEAD_NOTE
    );
    assert_eq!(
        drawn(&pages, "harness/ab/lead/line-0"),
        Some((StyleRef::Primary, crate::harness_ab::LEAD_NOTE.to_string()))
    );
    assert!(drawn(&pages, "harness/ab/lead/caption").is_none());
    assert_eq!(
        node(&pages, "harness/ab/row/held").label,
        "Held: <1% of working time on Cruise \u{b7} not measured yet off"
    );
    let (mut runtime, instance, view) = harness_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let all = semantic_keys(&pages);
    assert!(!all.iter().any(|k| k == "harness/ab/lead"));
    assert!(all.iter().any(|k| k == "harness/ab/row/held"));
}

#[test]
fn the_lead_gives_way_hero_then_plain() {
    use crate::harness_ab::{AbLead, LeadShare, ab_lead};
    use crate::native_settings::harness_page::ab_lead_block;
    use crate::native_ui::{StyleRef, UiContent};
    fn texts(node: &UiNode, out: &mut Vec<(StyleRef, String, f32)>) {
        if let UiContent::Text(text) = &node.content {
            let height = match node.layout.height {
                crate::native_ui::Length::Fixed(height) => height,
                other => panic!("a fixed height: {other:?}"),
            };
            out.push((text.style, text.text.clone(), height));
        }
        for child in &node.children {
            texts(child, out);
        }
    }
    let drawn = |lead: &AbLead, inner: f32| {
        let (node, height) = ab_lead_block(lead, inner);
        let mut out = Vec::new();
        texts(&node, &mut out);
        (out, height)
    };
    let both = AbLead::Both {
        cruise: LeadShare {
            share: "<1%".to_string(),
            of: "of 6h 10m on Cruise".to_string(),
        },
        off: LeadShare {
            share: "14%".to_string(),
            of: "of 25m off".to_string(),
        },
    };
    at_each_text_scale(&[1.0, 2.0], "the lead", |scale| {
        let plain = 28.0_f32.max(18.0 * scale);
        let (wide, _) = drawn(&both, 2_000.0);
        assert_eq!(
            wide.iter()
                .map(|(style, text, _)| (*style, text.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    StyleRef::Primary,
                    "Working time held at a box or a question"
                ),
                (StyleRef::Hero, "<1%"),
                (StyleRef::Primary, "of 6h 10m on Cruise"),
                (StyleRef::Hero, "14%"),
                (StyleRef::Primary, "of 25m off"),
            ],
            "{scale}\u{d7}"
        );
        let hero_at = |inner: f32| {
            drawn(&both, inner)
                .0
                .iter()
                .any(|(s, _, _)| *s == StyleRef::Hero)
        };
        let (mut low, mut high) = (1.0_f32, 2_000.0_f32);
        while high - low > 1.0 {
            let mid = (low + high) / 2.0;
            if hero_at(mid) { high = mid } else { low = mid }
        }
        assert!(
            hero_at(high) && !hero_at(low),
            "{scale}\u{d7}: an edge at {high}"
        );
        for inner in [low, 300.0_f32.min(low), 200.0_f32.min(low)] {
            let (narrow, height) = drawn(&both, inner);
            assert!(
                narrow
                    .iter()
                    .all(|(style, _, h)| *style == StyleRef::Primary && *h == plain),
                "{scale}\u{d7} {inner}: {narrow:?}"
            );
            let lines = narrow
                .iter()
                .map(|(_, text, _)| text.as_str())
                .collect::<Vec<_>>();
            let words = |text: &str| {
                text.replace(" \u{b7} ", " ")
                    .split(' ')
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                words(&lines.join(" ")),
                words(&both.words()),
                "{scale}\u{d7} {inner}: whole, {lines:?}"
            );
            assert!(
                lines
                    .iter()
                    .all(|line| !line.ends_with('\u{b7}') && !line.contains('\u{2026}')),
                "{scale}\u{d7} {inner}: {lines:?}"
            );
            let n = lines.len() as f32;
            assert_eq!(height, n * plain + (n - 1.0) * 4.0);
        }
        for inner in [2_000.0, 200.0] {
            let (note, height) = drawn(&AbLead::NoOff, inner);
            assert!(
                note.iter()
                    .all(|(style, _, h)| *style == StyleRef::Primary && *h == plain),
                "{scale}\u{d7} {inner}: {note:?}"
            );
            if inner > 1_000.0 {
                assert_eq!(height, plain, "one line");
            }
        }
        assert!(ab_lead(&crate::harness_ab::AbWeek::default()).is_none());
    });
    at_each_text_scale(&[2.0], "the lead on a narrow page", |_| {
        for (width, height) in [(584.0, 348.0), (320.0, 568.0)] {
            let (mut runtime, instance, view) = harness_route(fixture_led(true));
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            assert!(
                pages
                    .iter()
                    .all(|page| painted(page, "harness/ab/lead/cruise/share").is_none()),
                "{width}\u{d7}{height}: no Hero share"
            );
            assert!(
                pages
                    .iter()
                    .any(|page| painted(page, "harness/ab/lead/line-0").is_some()),
                "{width}\u{d7}{height}: plain"
            );
            for (index, page) in pages.iter().enumerate() {
                let failures = page
                    .fit_failures()
                    .into_iter()
                    .filter(|line| !line.contains("key=\"settings/nav/"))
                    .collect::<Vec<_>>();
                assert!(
                    failures.is_empty(),
                    "{width}\u{d7}{height} page {index}: {failures:#?}"
                );
            }
        }
    });
}

#[test]
fn the_leads_words_stand_on_its_figures_baseline() {
    use crate::native_ui::{SemanticRole, StyleRef, UiContent, text_paint_metrics};
    at_each_text_scale(&[1.0, 2.0], "the lead's baseline", |scale| {
        let (mut runtime, instance, view) = harness_route(fixture_led(true));
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        let keys = [
            "caption",
            "cruise/share",
            "cruise/of",
            "off/share",
            "off/of",
        ];
        let page = pages
            .iter()
            .find(|page| {
                keys.iter()
                    .all(|key| painted(page, &format!("harness/ab/lead/{key}")).is_some())
            })
            .unwrap_or_else(|| panic!("{scale}\u{d7}: the lead is painted whole"));
        let baseline = |key: &str| {
            let node = painted(page, &format!("harness/ab/lead/{key}"))
                .unwrap_or_else(|| panic!("{scale}\u{d7}: {key} is painted"));
            let UiContent::Text(text) = &node.content else {
                panic!("{key}: a text");
            };
            let role = if text.style == StyleRef::Hero {
                SemanticRole::Heading
            } else {
                SemanticRole::Text
            };
            let (px, _) = text_paint_metrics(role, text.style);
            crate::tray_raster::row_baseline(node.rect.y, node.rect.height, px)
        };
        let share = baseline("cruise/share");
        for key in ["cruise/of", "off/share", "off/of"] {
            assert!(
                (baseline(key) - share).abs() <= 0.5,
                "{scale}\u{d7}: {key} at {} against the share's {share}",
                baseline(key)
            );
        }
        assert!(
            baseline("caption") + 8.0 < share,
            "{scale}\u{d7}: the caption stands above the shares"
        );
    });
}

#[test]
fn a_background_landing_moves_no_switch() {
    let controls = |state: HarnessState, (width, height): (f32, f32)| {
        let (mut runtime, instance, view) = harness_route(state);
        let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
        let mut out = Vec::new();
        for (index, page) in pages.iter().enumerate() {
            for node in &page.semantics {
                let key = node.key.as_str();
                if key.starts_with("settings/control/") || key == "harness/ab" {
                    let rect = node.rect;
                    let at = (rect.x, rect.y, rect.width, rect.height);
                    out.push((index, key.to_string(), at));
                }
            }
        }
        out
    };
    let before = fixture();
    let mut after = fixture();
    after.ab.ends.landed = 1;
    after.ab.ends.landing[crate::harness_ab::bucket(2 * 60_000)] = 1;
    at_each_text_scale(&[1.0], "a background landing", |_| {
        for host in [(744.0, 420.0), (584.0, 348.0)] {
            let (was, now) = (
                controls(before.clone(), host),
                controls(after.clone(), host),
            );
            assert!(
                was.iter()
                    .any(|(_, key, _)| key == "settings/control/harness.enabled"),
                "{host:?}: {was:#?}"
            );
            assert_eq!(was, now, "{host:?}");
        }
    });
}

/// [`fixture`] with landings (design §12.4): three sessions under Land.
pub(super) fn fixture_landings() -> HarnessState {
    use aterm_agent::harness::acts::TaskFace;
    use aterm_agent::supervise::policy::turn_end::RULE_DONE_CHECK;
    let mut state = fixture();
    let first = state.sessions[0].clone();
    let with = |id: u64, task: TaskFace| {
        let mut session = first.clone();
        session.local_id = id;
        session.sid = format!("s-{id}");
        session.title = format!("task {id}");
        session.place = format!("in tab {}", id - 5);
        session.glass.task = Some(task);
        session
    };
    state.sessions = vec![
        first.clone(),
        with(8, TaskFace::Landed),
        with(9, TaskFace::Theirs),
    ];
    crate::harness_glass::hold_tests::held_session(&mut state.sessions[0]);
    let mut today = HarnessStats::per_session(0);
    today.landed = 2;
    today.approved = 31;
    today.unproven = 1;
    today.continued = 4;
    today.land_prompts = 3;
    today.escalated = 2;
    today.human_wait_ms = 14 * 60_000;
    today.needs_told = 2;
    today.by_rule.insert(RULE_DONE_CHECK.to_string(), 1);
    today.rows = 45;
    today.by_session.as_mut().expect("per session").insert(
        "s-8".to_string(),
        aterm_agent::harness::stats::Tally {
            landed: 1,
            land_prompts: 2,
            approved: 31,
            ..aterm_agent::harness::stats::Tally::default()
        },
    );
    let mut week = HarnessStats::new(0);
    week.landed = 9;
    week.approved = 120;
    week.unproven = 2;
    week.continued = 30;
    week.land_prompts = 11;
    week.escalated = 5;
    week.human_wait_ms = 62 * 60_000;
    week.needs_told = 2;
    week.by_rule.insert(RULE_DONE_CHECK.to_string(), 12);
    week.rows = 187;
    state.stats = HarnessStatsSlot {
        report: Some(std::sync::Arc::new(HarnessStatsReport::new(
            today,
            week,
            1_000_000_000,
            Some(0),
            &|_| Some(0),
        ))),
        collecting: false,
        unavailable: None,
        stale: false,
    };
    state
}

#[test]
fn the_page_draws_its_landings_and_the_done_claims_checked() {
    let (mut runtime, instance, view) = harness_route(fixture_landings());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let says =
        |words: &str, part: &str| words.split([':', ',', ';']).any(|said| said.trim() == part);
    let every = node(&pages, "harness/session/all").label.clone();
    let cruise = node(&pages, "harness/session/all/autopilot/cruise")
        .description
        .clone()
        .unwrap_or_default();
    for part in ["1 landing", "1 landed", "1 quiet"] {
        assert!(says(&every, part), "{part}: {every}");
        assert!(says(&cruise, part), "{part}: {cruise}");
    }
    let landed = node(&pages, "harness/did/row/landed").label.clone();
    assert!(says(&landed, "2 today \u{b7} 9 in 7 days"), "{landed}");
    let checked = node(&pages, "harness/did/row/past--done").label.clone();
    assert!(says(&checked, "1 today \u{b7} 12 in 7 days"), "{checked}");
    node(&pages, "harness/did/row/no-safe-rule");
    node(&pages, "harness/did/row/land-prompts");
    let today = node(&pages, "harness/status/today").label.clone();
    assert!(today.starts_with("2 landed today \u{b7} "), "{today}");
    let day = node(&pages, "harness/session/8/today").label.clone();
    assert!(day.starts_with("today: 1 landed \u{b7} "), "{day}");
    let (mut runtime, instance, view) = harness_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    for key in ["harness/did/row/landed", "harness/did/row/past--done"] {
        assert!(
            pages
                .iter()
                .all(|page| page.semantic(&UiKey::new(key)).is_none()),
            "{key}"
        );
    }
}

/// WHAT THE HARNESS DID'S PLAIN COUNTS ON THE COMPILED PAGE (§14.6): `past “done”`, `no safe
/// rule`, `Waited for you`, `Needs, carried on` and `None in 7 days` are each a fact of their own
/// key (the compile refuses two of one key), and no fact of v5's words is drawn: `of them`, `of
/// those`, `Needed you`, `Needs told`, `Waited on you`, or a count's `0 today · 0 in 7 days`.
#[test]
fn the_page_draws_its_plain_counts() {
    use crate::harness_screen::{
        DONE_CHECKED_OF_KEPT_GOING, NEEDED_YOU, NEEDS_TOLD, NONE_IN_7_DAYS, UNPROVEN_OF_APPROVED,
    };
    for state in [fixture(), fixture_landings()] {
        let (mut runtime, instance, view) = harness_route(state.clone());
        let pages = every_page(&mut runtime, instance, view, &view_cx());
        let rows = state.projection().did_rows;
        for (label, value) in &rows {
            let key = format!("harness/did/row/{}", key_fragment(label));
            assert_eq!(node(&pages, &key).label, format!("{label}: {value}"));
        }
        for old in [
            "of-them",
            "of-those",
            "needed-you",
            "needs-told",
            "waited-on-you",
        ] {
            let key = format!("harness/did/row/{old}");
            assert!(
                pages
                    .iter()
                    .all(|page| page.semantic(&UiKey::new(key.as_str())).is_none()),
                "{key}"
            );
        }
        assert!(
            rows.iter()
                .all(|(_, value)| value != "0 today \u{b7} 0 in 7 days"),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|(label, _)| label == NONE_IN_7_DAYS),
            "{rows:?}"
        );
    }
    let rows = fixture_landings().projection().did_rows;
    for label in [
        DONE_CHECKED_OF_KEPT_GOING,
        UNPROVEN_OF_APPROVED,
        NEEDED_YOU.label,
        NEEDS_TOLD.label,
    ] {
        assert!(rows.iter().any(|(l, _)| l == label), "{label}: {rows:?}");
    }
}

#[test]
fn every_mode_sentence_comes_from_mode_words() {
    use crate::autopilot::{MODE_WORDS, TIMED_CRUISE_CHIP, TIMED_CRUISE_RECORD};
    use crate::claude_lights::{ChipGlass, LandAtGlass, TitleWords, autopilot_title};
    const MANUAL: &str = include_str!("../../aterm-cli/src/manual.rs");
    let at = MANUAL
        .find("\nAUTOPILOT \u{2014} ")
        .expect("the AUTOPILOT section");
    let section = &MANUAL[at + 1..];
    let end = section[1..]
        .find("\n\n")
        .map_or(section.len(), |end| end + 1);
    let manual = section[..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let (mut runtime, instance, view) = harness_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let note = prefs::footnote_of(prefs::EDIT_HARNESS_AUTOPILOT).expect("the default's note");
    for words in MODE_WORDS {
        let mode = words.mode;
        let shown = AutopilotView::from_mode(mode);
        let (title, _) = autopilot_title(
            shown,
            &ChipGlass::default(),
            "",
            None,
            TitleWords::default(),
        );
        assert!(
            title.starts_with(&format!(
                "autopilot: {} \u{2014} {} \u{b7} click: ",
                mode.word(),
                words.chip
            )),
            "{mode:?}'s chip: {title}"
        );
        let record =
            crate::message_reporters::autopilot_set("s-1", "in tab 2", shown, true, None, None);
        assert_eq!(
            record.detail.first().map(String::as_str),
            Some(format!("Tab 2: {}", words.record).as_str()),
            "{mode:?}'s record"
        );
        let button = node(
            &pages,
            &format!("harness/session/7/autopilot/{}", mode.word()),
        );
        assert!(
            button
                .description
                .as_deref()
                .is_some_and(|d| d.starts_with(words.button)),
            "{mode:?}'s button: {:?}",
            button.description
        );
        assert!(
            manual.contains(&format!("`{}`", words.chip)),
            "{mode:?}'s sentence in the manual's AUTOPILOT: {manual}"
        );
        assert!(
            note.contains(words.record),
            "{mode:?}'s sentence in the default autopilot's note: {note}"
        );
    }
    let timed = ChipGlass {
        land_at: Some(LandAtGlass {
            clock: crate::claude_lights::Clock::of_minute(18 * 60),
            other_day: None,
        }),
        ..ChipGlass::default()
    };
    let (title, _) = autopilot_title(
        AutopilotView::Cruise,
        &timed,
        "",
        None,
        TitleWords::default(),
    );
    assert!(
        title.contains(&format!(", then land \u{2014} {TIMED_CRUISE_CHIP} \u{b7} ")),
        "{title}"
    );
    let record = crate::message_reporters::autopilot_timer_set(
        "s-1",
        "in tab 2",
        timed.land_at.expect("the timer"),
        None,
    );
    assert!(
        record
            .detail
            .iter()
            .any(|line| line == &format!("Tab 2: {TIMED_CRUISE_RECORD}")),
        "{:?}",
        record.detail
    );
    assert!(
        manual.contains(&format!("`{TIMED_CRUISE_CHIP}`")),
        "the timed sentence in the manual: {manual}"
    );
}

#[test]
fn the_occupancy_row_shows_the_mode_and_nothing_writes_it() {
    let rows = config_rows(&SupervisorConfig::default());
    let row = rows
        .iter()
        .find(|row| row.key == "occupancy")
        .expect("an occupancy row");
    assert_eq!((row.value.as_str(), row.is_default), ("observe", true));

    let policy = crate::app_config::HarnessPolicy::read("[harness]\noccupancy = \"prompt\"\n");
    let mut state = fixture();
    state.words = policy.words();
    state.config = config_rows(&policy.policy);
    let (mut runtime, instance, view) = harness_route(state);
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let shown = node(&pages, "harness/config/row/occupancy");
    assert!(
        shown.label.starts_with("occupancy: prompt"),
        "{}",
        shown.label
    );
    assert!(shown.action.is_none(), "a fact row: {:?}", shown.action);
    let acting = |word: &str| {
        pages
            .iter()
            .flat_map(|page| &page.semantics)
            .filter(|node| {
                node.action
                    .as_ref()
                    .is_some_and(|action| action.as_str().contains(word))
            })
            .count()
    };
    assert_eq!(acting("occupancy"), 0, "no control writes it");
    assert!(acting("autopilot") > 0, "an editable key's control acts");
}

/// A refresh that rewrites a Tab Control row's words in fewer lines leaves the row at the height
/// it was laid out at, so the rows under it stay where they stand for a still reader: a row once
/// lost and gained a line every few seconds as its vitals' digits moved a wrap point, and every
/// row under it moved with it. Refresh lays the page out afresh.
#[test]
fn a_live_row_keeps_its_height_while_the_person_reads() {
    use crate::harness_screen::{FleetScope, TabComputer, TabControlState};
    use crate::native_settings::tabs_page::TABS_REFRESH;
    let computer = |id: &str, vitals: &str| TabComputer {
        id: id.to_string(),
        name: id.to_string(),
        address: "192.168.1.90 \u{b7} m111.local".to_string(),
        os_version: Some("Mac16,5 \u{b7} macOS (Darwin 26) \u{b7} OpenSSH_10.3".to_string()),
        status: "online".to_string(),
        vitals: vitals.to_string(),
        instances: Some(1),
        tabs: Some(1),
        ai_clis: Some(0),
        active_ai: Some(0),
        session_scope: Default::default(),
    };
    let long = "CPU 79% \u{2595}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{258f} \
                \u{2586}\u{2586}\u{2586}\u{2587}\u{2587}\u{2587} (14c \u{b7} Performance:88% \
                Efficiency:56%) \u{b7} GPU 0% (Apple M4 Max 32c) \u{b7} RAM 29.5 GiB/36.0 GiB \
                (82% \u{b7} warn \u{b7} wired 3.7 GiB \u{b7} comp 15.5 GiB \u{b7} swap 2.6 GiB) \
                \u{b7} Disk 666 GiB/926 GiB (71% \u{b7} 260 GiB free)";
    let short = "CPU 9% \u{b7} GPU 0% \u{b7} RAM 29.5 GiB/36.0 GiB";
    let with = |vitals: &str| {
        let mut state = fixture();
        state.tabs = TabControlState {
            computers: vec![computer("local", vitals), computer("lan/m2", "")],
            tabs: Vec::new(),
            fleet: FleetScope::Account,
            lan_waits: None,
        };
        state
    };
    let cx = view_cx_at(504.0, 1_600.0);
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(with(long), 1));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Tabs),
                value: None,
            }),
        )
        .unwrap();
    let rect = |runtime: &mut NativeRuntime, key: &str| {
        compile_settings_view(runtime, instance, view, &cx)
            .paint
            .iter()
            .find(|paint| paint.key.as_str() == key)
            .unwrap_or_else(|| panic!("{key} is painted"))
            .rect
    };
    let local = rect(&mut runtime, "tabs/computer/local");
    let detail = rect(&mut runtime, "tabs/computer/local/detail");
    let under = rect(&mut runtime, "tabs/computer/lan/m2");
    assert!(runtime.replace_settings_harness(with(short), 2));
    assert_eq!(
        rect(&mut runtime, "tabs/computer/local"),
        local,
        "the row keeps its height"
    );
    assert_eq!(
        rect(&mut runtime, "tabs/computer/local/detail"),
        detail,
        "the row's line under its vitals stays where it stood: the room the vitals took is \
         kept under them, not at the row's foot"
    );
    assert_eq!(
        rect(&mut runtime, "tabs/computer/lan/m2"),
        under,
        "the row under it stays where it stood"
    );
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(TABS_REFRESH),
                value: None,
            }),
        )
        .unwrap();
    let afresh = rect(&mut runtime, "tabs/computer/local");
    assert!(
        afresh.height < local.height,
        "CONTROL: measured afresh, the shorter words take less: {afresh:?} against {local:?}"
    );
    assert!(rect(&mut runtime, "tabs/computer/lan/m2").y < under.y);
}

/// A tab whose title a refresh rewrites in fewer lines (a program's spinner glyph, a shorter
/// title) keeps its status line where it stood, and its row keeps its height: the lines the title
/// took are kept under it while the person reads. Refresh lays the row out afresh.
#[test]
fn a_tabs_status_line_keeps_its_place_while_its_title_changes() {
    use crate::harness_screen::{FleetScope, TabControlState, TabSpreadsheetRow};
    use crate::native_settings::tabs_page::TABS_REFRESH;
    let tab = |title: &str| TabSpreadsheetRow {
        key: "local/0".to_string(),
        computer_id: "local".into(),
        computer: "m111".to_string(),
        instance: "this aterm (pid 7)".to_string(),
        place: "Tab 1 (80\u{d7}24)".to_string(),
        sid: "s-5f90d5e126a626f5f5b0".to_string(),
        title: title.to_string(),
        state: "alive".to_string(),
        program: "claude".to_string(),
        is_ai_cli: true,
        active_ai: Some(true),
        ai_status: "working".to_string(),
        autopilot: "land".to_string(),
        human: "3m ago".to_string(),
        vitals: "CPU 12% \u{b7} RAM 300 MiB".to_string(),
        show_local_id: None,
        cached_observation: None,
    };
    let long = "\u{2733} Settings page audit and cursor styles, the lab isolation and tooling \
                cluster, reviewed adversarially before it lands";
    let short = "\u{2733} Settings";
    let with = |title: &str| {
        let mut state = fixture();
        state.tabs = TabControlState {
            computers: Vec::new(),
            tabs: vec![tab(title)],
            fleet: FleetScope::Account,
            lan_waits: None,
        };
        state
    };
    let cx = view_cx_at(504.0, 1_600.0);
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(with(long), 1));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Tabs),
                value: None,
            }),
        )
        .unwrap();
    let status_and_row = |runtime: &NativeRuntime| {
        let paint = compile_settings_view(runtime, instance, view, &cx).paint;
        let status = paint
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("tabs/row/local/0/words/")
                    && matches!(&node.content, UiContent::Text(text) if text.text.starts_with("AI CLI:"))
            })
            .expect("the tab's status line is painted")
            .rect;
        let row = paint
            .iter()
            .find(|node| node.key.as_str() == "tabs/row/local/0")
            .expect("the tab's row is painted")
            .rect;
        (status, row)
    };
    let (status, row) = status_and_row(&runtime);
    assert!(runtime.replace_settings_harness(with(short), 2));
    assert_eq!(
        status_and_row(&runtime),
        (status, row),
        "the status line and the row stay where they stood"
    );
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(TABS_REFRESH),
                value: None,
            }),
        )
        .unwrap();
    let (afresh, _) = status_and_row(&runtime);
    assert!(
        afresh.y < status.y,
        "CONTROL: laid out afresh, the shorter title takes fewer lines: {afresh:?} against \
         {status:?}"
    );
}

/// A computer the fleet lists above the screen moves nothing on it, and the next render with no
/// event between draws that same screen: one render once re-anchored the list by what its
/// screen showed and the next drew the old stop number in the new numbering, a computer further
/// on (Tab Control paged into its LAN computers, m14 drawn where m13 stood).
#[test]
fn a_computer_listed_above_the_screen_moves_nothing_on_it() {
    use crate::harness_screen::{FleetScope, TabComputer, TabControlState};
    let computer = |n: usize| TabComputer {
        id: format!("lan/m{n}"),
        name: format!("m{n}-MacBook-Pro-M4-Max-36GB-1TB"),
        address: format!("192.168.1.{n} \u{b7} m{n}.local"),
        os_version: Some("Mac16,5 \u{b7} Darwin 26 \u{b7} OpenSSH_10.3".to_string()),
        status: "advertised (lan)".to_string(),
        vitals: String::new(),
        instances: Some(0),
        tabs: Some(0),
        ai_clis: Some(0),
        active_ai: Some(0),
        session_scope: Default::default(),
    };
    let with = |numbers: &[usize]| {
        let mut state = fixture();
        state.tabs = TabControlState {
            computers: numbers.iter().map(|n| computer(*n)).collect(),
            tabs: Vec::new(),
            fleet: FleetScope::Account,
            lan_waits: None,
        };
        state
    };
    let fleet: Vec<usize> = (1..=14).collect();
    let cx = view_cx_at(320.0, 420.0);
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(with(&fleet), 1));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: route_action(SettingsRoute::Tabs),
                value: None,
            }),
        )
        .unwrap();
    let screen = |runtime: &NativeRuntime| {
        compile_settings_view(runtime, instance, view, &cx)
            .paint
            .iter()
            .filter(|paint| {
                paint.key.as_str().starts_with("tabs/computer/lan/")
                    && paint.key.as_str().matches('/').count() == 3
            })
            .map(|paint| (paint.key.as_str().to_string(), paint.rect))
            .collect::<Vec<_>>()
    };
    let mut page = 0;
    let before = loop {
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.scroll_page_to(page);
        }
        let shown = screen(&runtime);
        let past_m6 = |(key, _): &(String, crate::native_ui::LogicalRect)| {
            key.strip_prefix("tabs/computer/lan/m")
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| n >= 6)
        };
        if !shown.is_empty() && shown.iter().all(past_m6) {
            break shown;
        }
        page += 1;
        assert!(page < 200, "a screen deep in the computers");
    };
    let mut grown = vec![0];
    grown.extend(&fleet);
    assert!(runtime.replace_settings_harness(with(&grown), 2));
    let first = screen(&runtime);
    let second = screen(&runtime);
    assert_eq!(first, before, "the screen holds its place");
    assert_eq!(second, first, "and the next render draws it again");
}
