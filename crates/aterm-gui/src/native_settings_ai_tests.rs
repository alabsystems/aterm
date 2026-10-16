// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Harness ▸ AI CLIs' page (`native_settings_ai.rs`), rendered
//! through the Settings controller and read back from the compiled semantic
//! tree — what `controls prefs` and a screen reader read.

use super::*;
use crate::ai_screen::{AiChoices, AiCli, AiReading, AiState};
use crate::native_settings::ai_page::{AI_OPEN_FLEET, AI_OPEN_REVIEWS, AI_REFRESH, ai_page_rows};
use aterm_agent::ai::state::{Mark, State};

/// One CLI as a look would judge it: measured as its row is, wanted.
fn cli(id: &'static str, state: State, mark: Mark, fact: &str) -> AiCli {
    let row = aterm_agent::ai::connector::connector(id).expect("a connection");
    AiCli {
        id,
        product: row.product,
        claude: row.is_claude(),
        measured: row.measured,
        wanted: true,
        state,
        mark,
        fact: fact.to_string(),
        version: Some("1.2.3".to_string()),
    }
}

/// A Harness snapshot as the host publishes one, its AI CLIs part: Codex CLI
/// signed out and preferred (by the fleet document), Claude Code fine,
/// Antigravity CLI unmeasured, Muse Code not used here; reviews granted to
/// Codex CLI and Claude Code by this machine's table.
fn fixture() -> HarnessState {
    let (local, notes) = aterm_agent::ai::config::read(
        "[ai]\nreviewers = [\"codex\", \"claude\"]\nmachine = \"m7\"\n",
    );
    let settings = aterm_agent::ai::config::resolve(
        &local,
        Some(std::path::Path::new("/dotfiles/ai-fleet.toml")),
        Some(aterm_agent::ai::config::read_fleet(
            "schema = 1\npreferred = \"codex\"\n",
        )),
        notes,
    );
    HarnessState {
        observed: true,
        ai: AiState {
            enabled: true,
            looking: false,
            choices: AiChoices::of(&settings),
            reading: Some(AiReading {
                as_of: 1_000_000,
                offset_s: Some(0),
                preferred: "codex",
                clis: vec![
                    cli(
                        "claude",
                        State::Ok,
                        Mark::Ok,
                        "signed in (Max) \u{b7} served 2 h ago",
                    ),
                    cli("codex", State::SignedOut, Mark::Fail, "no login is stored"),
                    cli(
                        "agy",
                        State::Unproven,
                        Mark::Note,
                        "it has run here; nothing has shown a login yet",
                    ),
                    AiCli {
                        wanted: false,
                        ..cli("muse", State::Off, Mark::Note, "not used on this machine")
                    },
                ],
                store_note: None,
            }),
            unavailable: None,
        },
        ..HarnessState::unobserved()
    }
}

/// A Settings controller holding `state`, its view on Settings ▸ Harness ▸ AI
/// CLIs; the route's arrival asks the host to look again.
fn ai_route(
    state: HarnessState,
) -> (
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let (mut runtime, instance, view) = setup();
    assert!(runtime.replace_settings_harness(state, 1));
    let effects = press(
        &mut runtime,
        instance,
        view,
        route_action(SettingsRoute::Ai).as_str(),
    );
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            AppEffect::Harness(crate::native_app::HarnessRequest::AiLook)
        )),
        "{effects:?}"
    );
    (runtime, instance, view)
}

/// Press `action` on the view; the effects it asks of the host.
fn press(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    action: &str,
) -> Vec<AppEffect> {
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

/// Whether `key` is on any of `pages`.
fn shown(pages: &[crate::native_ui::CompiledUi], key: &str) -> bool {
    pages
        .iter()
        .any(|page| page.semantic(&UiKey::new(key)).is_some())
}

/// THE ROUTE IS ITS SPECIAL PAGE (§10): Harness's sub-page, the rail's
/// Harness row selected while it shows; the heading; THIS MACHINE's two
/// registry rows; the CLIS card's health line, one status row per CLI with a
/// Sign in only on the one that needs a person, and each "Use on this
/// machine" switch as its ordinary registry control; REVIEWS and FLEET
/// DOCUMENT as facts with their Open aterm.toml; Refresh. Every page of it
/// whole on a wide, a medium and a compact host, and at the lab's two sizes.
#[test]
fn the_ai_route_renders_its_cards_and_controls() {
    assert_eq!(
        SettingsRoute::Ai.rail_parent(),
        Some(SettingsRoute::Harness)
    );
    assert_eq!(SettingsRoute::from_path("/ai"), Some(SettingsRoute::Ai));
    let (mut runtime, instance, view) = ai_route(fixture());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    assert_eq!(
        node(&pages, "settings/page-heading/ai-clis").label,
        "AI CLIs"
    );
    assert!(
        node(&pages, "settings/nav/harness")
            .state
            .is_some_and(|state| state.selected),
        "the rail's Harness row stands for its sub-page"
    );
    assert!(!shown(&pages, "settings/nav/ai"), "no rail row of its own");
    for key in ai_page_rows() {
        let control = node(&pages, &format!("settings/control/{key}"));
        assert_eq!(
            control.action.as_ref().map(ActionId::as_str),
            Some(format!("settings/set/{key}").as_str()),
            "{key} is the ordinary registry control"
        );
    }
    assert_eq!(
        node(&pages, "ai/clis/health").label,
        "Codex CLI needs you \u{b7} looked at 13:46"
    );
    assert_eq!(
        node(&pages, "ai/cli/codex").label,
        "Codex CLI 1.2.3 \u{b7} preferred: signed-out \u{b7} no login is stored"
    );
    let sign_in = node(&pages, "ai/sign-in/codex");
    assert_eq!(
        sign_in.action.as_ref().map(ActionId::as_str),
        Some("ai/sign-in/codex")
    );
    assert_eq!(sign_in.label, "Sign in: Codex CLI");
    for id in ["claude", "agy", "muse"] {
        assert!(shown(&pages, &format!("ai/cli/{id}")), "{id}'s row");
        assert!(
            !shown(&pages, &format!("ai/sign-in/{id}")),
            "{id} needs nobody"
        );
    }
    assert_eq!(
        node(&pages, "ai/reviews/row/reviewers").label,
        "Reviewers: codex, claude"
    );
    assert_eq!(
        node(&pages, "ai/reviews/row/from").label,
        "From: aterm.toml"
    );
    assert_eq!(node(&pages, "ai/fleet/row/machine").label, "Machine: m7");
    for key in [AI_REFRESH, AI_OPEN_REVIEWS, AI_OPEN_FLEET] {
        assert_eq!(
            node(&pages, key).action.as_ref().map(ActionId::as_str),
            Some(key)
        );
    }

    for (width, height) in [
        (1_200.0, 820.0),
        (900.0, 600.0),
        (320.0, 568.0),
        (774.0, 782.0),
        (1_419.0, 908.0),
    ] {
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

/// THE PAGE'S PRESSES ARE TYPED HOST REQUESTS: a Sign in names its CLI, a
/// press for one that needs nobody asks for nothing and says why; Refresh
/// asks for a look; each Open aterm.toml is the Manual route's own act at the
/// key its card shows, and the page stays. The Harness page's AI CLIs button
/// is this route, and its SUPERVISION card draws none of this page's rows.
#[test]
fn the_ai_routes_presses_are_typed_host_requests() {
    let harness = |effects: &[AppEffect]| {
        effects
            .iter()
            .filter_map(|effect| match effect {
                AppEffect::Harness(request) => Some(*request),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (mut runtime, instance, view) = ai_route(fixture());
    assert_eq!(
        harness(&press(&mut runtime, instance, view, "ai/sign-in/codex")),
        [crate::native_app::HarnessRequest::AiSignIn { id: "codex" }]
    );
    assert!(
        harness(&press(&mut runtime, instance, view, "ai/sign-in/claude")).is_empty(),
        "a working login is never signed in again from here"
    );
    assert!(
        harness(&press(&mut runtime, instance, view, "ai/sign-in/nonesuch")).is_empty(),
        "no such CLI"
    );
    assert_eq!(
        harness(&press(&mut runtime, instance, view, AI_REFRESH)),
        [crate::native_app::HarnessRequest::AiLook]
    );
    for (action, key) in [
        (AI_OPEN_REVIEWS, "ai.reviewers"),
        (AI_OPEN_FLEET, "ai.machine"),
    ] {
        let effects = press(&mut runtime, instance, view, action);
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                AppEffect::OpenConfigEditor {
                    target: Some(ConfigEditorTarget::Key(k)),
                    ..
                } if k == key
            )),
            "{action}: {effects:?}"
        );
        // The editor's completion lets the next Open through.
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.pending.clear();
    }
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert_eq!(state.route, SettingsRoute::Ai, "the page stays");

    // The way in from Settings ▸ Harness, whose own rows are its own.
    let effects = press(
        &mut runtime,
        instance,
        view,
        route_action(SettingsRoute::Harness).as_str(),
    );
    assert!(!harness(&effects).is_empty());
    let pages = every_page(&mut runtime, instance, view, &view_cx());
    let button = node(&pages, "harness/ai-clis");
    assert_eq!(
        button.action.as_ref(),
        Some(&route_action(SettingsRoute::Ai))
    );
    for key in ai_page_rows() {
        assert!(
            !shown(&pages, &format!("settings/control/{key}")),
            "{key} is the AI CLIs page's row, not the Harness page's"
        );
    }
    assert!(shown(
        &pages,
        &format!("settings/control/{}", prefs::EDIT_HARNESS_CODEX_ENABLED)
    ));
}

#[test]
fn a_stale_sign_in_action_cannot_override_the_current_use_switch() {
    let mut state = fixture();
    let (mut runtime, instance, view) = ai_route(state.clone());
    let asks_sign_in = |effects: Vec<AppEffect>| {
        effects.into_iter().any(|effect| {
            matches!(
                effect,
                AppEffect::Harness(crate::native_app::HarnessRequest::AiSignIn { .. })
            )
        })
    };
    assert!(
        asks_sign_in(press(&mut runtime, instance, view, "ai/sign-in/codex")),
        "ordinary wanted sign-in"
    );
    let (local, notes) = aterm_agent::ai::config::read("[ai.codex]\nuse = false\n");
    state.ai.choices = AiChoices::of(&aterm_agent::ai::config::resolve(&local, None, None, notes));
    assert!(
        state
            .ai
            .reading
            .as_ref()
            .unwrap()
            .cli("codex")
            .unwrap()
            .wanted,
        "the old reading is deliberately retained"
    );
    assert!(runtime.replace_settings_harness(state, 2));
    assert!(
        !asks_sign_in(press(&mut runtime, instance, view, "ai/sign-in/codex")),
        "a queued action cannot restore a disabled CLI"
    );
}
