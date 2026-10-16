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
            title: "fix the build".to_string(),
            place: "in tab 2".to_string(),
            program: "Claude Code",
            word: Some(Autopilot::Land),
            view: AutopilotView::Land,
            default: Some(Autopilot::Cruise),
            supervised: true,
            glass: crate::claude_lights::ChipGlass::default(),
        }],
        stats: HarnessStatsSlot {
            report: Some(Box::new(HarnessStatsReport {
                today,
                week,
                collected_unix_ms: 1_000_000_000,
                offset_s: Some(0),
            })),
            collecting: false,
            unavailable: Some("the ledgers' reader failed".to_string()),
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
        ai: crate::ai_screen::AiState::default(),
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
        rows[..4],
        [
            prefs::EDIT_HARNESS_ENABLED,
            prefs::EDIT_HARNESS_CLAUDE_ENABLED,
            prefs::EDIT_HARNESS_CODEX_ENABLED,
            prefs::EDIT_HARNESS_AGY_ENABLED,
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
        "No Claude Code, Codex or Antigravity CLI session is open"
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
        Some("Supervise Codex sessions is Off, so Autopilot has nothing to steer there.")
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
            .contains("limited by Supervise Codex sessions set to Off"),
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
/// its row on the page (no page-wide picker), and a segment saves its mode.
#[test]
fn the_default_autopilot_is_segments_on_the_page() {
    let (mut runtime, instance, view) = harness_route(fixture());
    let key = prefs::EDIT_HARNESS_AUTOPILOT;
    let cx = view_cx();
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new(format!("settings/segments/{key}")))
            .is_some(),
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
        .filter(|p| p.supervisable())
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
                    .starts_with(&format!("Supervise {product} sessions")),
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
/// ("Harness supervision"), so "Supervise Codex sessions · Off" there never
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
