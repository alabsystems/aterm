// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Harness ▸ AI CLIs' page (`native_settings_ai.rs`), rendered
//! through the Settings controller and read back from the compiled semantic
//! tree — what `controls prefs` and a screen reader read.

use super::*;
use crate::ai_screen::{AiChoices, AiCli, AiReading, AiState};
use crate::native_settings::ai_page::{
    AI_FLEET_FETCH, AI_OPEN_FLEET, AI_OPEN_REVIEWS, AI_REFRESH, ai_page_rows,
};
use aterm_agent::ai::state::{Mark, State};

/// One CLI as a look would judge it, wanted.
fn cli(id: &'static str, state: State, mark: Mark, fact: &str) -> AiCli {
    let row = aterm_agent::ai::connector::connector(id).expect("a connection");
    AiCli {
        id,
        product: row.product,
        claude: row.is_claude(),
        wanted: true,
        state,
        mark,
        fact: fact.to_string(),
        path: None,
        version: Some("1.2.3".to_string()),
    }
}

/// A Harness snapshot as the host publishes one, its AI CLIs part: Codex CLI
/// signed out and preferred (by the fleet document), Claude Code fine,
/// Antigravity CLI unmeasured, Muse Code not used here; reviews granted to
/// Codex CLI and Claude Code by this machine's table.
pub(super) fn fixture() -> HarnessState {
    let (local, notes) = aterm_agent::ai::config::read(
        "[ai]\nreviewers = [\"codex\", \"claude\"]\nmachine = \"m7\"\n",
    );
    let settings = aterm_agent::ai::config::resolve(
        &local,
        Some(&aterm_agent::ai::config::FleetSource::Path(
            "/dotfiles/ai-fleet.toml".into(),
        )),
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
                held: None,
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
            state.scroll_page_to(page);
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

/// THE ROUTE IS ITS SPECIAL PAGE (§10): Harness's sub-page, its own row
/// listed and selected under Harness's while it shows; the heading; THIS MACHINE's two
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
        node(&pages, "settings/nav/ai")
            .state
            .is_some_and(|state| state.selected),
        "the sub-page's own row is the selected one"
    );
    assert!(
        !node(&pages, "settings/nav/harness")
            .state
            .is_some_and(|state| state.selected),
        "its parent's row is not"
    );
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
        "Reviewers: codex, claude, but the one asking (aterm.toml)"
    );
    assert_eq!(
        node(&pages, "ai/reviews/row/permissions").label,
        "Permissions: the asking agent's own; yolo when you ask"
    );
    assert_eq!(node(&pages, "ai/fleet/row/machine").label, "Machine: m7");
    for key in [AI_REFRESH, AI_OPEN_REVIEWS, AI_OPEN_FLEET, AI_FLEET_FETCH] {
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
    assert_eq!(
        harness(&press(&mut runtime, instance, view, AI_FLEET_FETCH)),
        [crate::native_app::HarnessRequest::AiFleetFetch]
    );
    for (action, key) in [
        (AI_OPEN_REVIEWS, "ai.reviewers"),
        (AI_OPEN_FLEET, "ai.fleet"),
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

/// THE PAGE'S FACT CARDS KEEP THEIR LABELS WHOLE AT LARGE TYPE (the review
/// of the post-landing check's page fixes): WHO REVIEWS and WHAT IS HELD
/// each size their label column as Settings ▸ Harness's fact cards do
/// (`fact_label_width`), so at 2× text on the lab's 504 pt page and a wide
/// one `Permissions` and `Each review` are whole, and their values wrap
/// whole. NEGATIVE CONTROL: About's column (capped at 1.2×) paints
/// `Permissi…`.
#[test]
fn the_ai_fact_cards_keep_their_labels_whole_at_large_type() {
    at_each_text_scale(&[1.0, 2.0], "the AI page's fact cards", |scale| {
        for (width, height) in [(504.0, 628.0), (924.0, 640.0)] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let (mut runtime, instance, view) = ai_route(fixture());
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            super::harness::fact_cards_whole(&pages, &["ai/reviews/", "ai/fleet/"], &context);
        }
    });
}

/// A CLI ROW PAINTS ITS WHOLE NAME AND FACT AT EVERY WIDTH (the
/// audit): a CLI not installed by aterm whose copy lives under
/// `~/Library/Application Support/` — a path with a space, where aterm's
/// own reroute copies live — and every other CLI paint their name and fact
/// whole, wrapped to the text column (a path at its folders), on a compact
/// page, beside the button on a medium and a wide one, and in a short wide
/// window at 1×, 1.5× and 2× text: the painted lines, joined, are the
/// row's accessible words, with no ellipsis. A one-line row cut its fact to
/// one line: at 2× text in a 1564pt window it read "not installed by aterm
/// (a… …/reroute/claude)". NEGATIVE CONTROL: the fact is longer than one
/// line of the column at 2×, so a whole fact there is on lines.
#[test]
fn a_cli_row_paints_its_whole_name_and_fact_at_every_width() {
    let path = "/Users//someone/Library/Application Support/aterm/pkg/reroute/agy";
    let fact = format!(
        "not installed by aterm (a copy it does not manage is at {path}) \
         (not measured in this build)"
    );
    at_each_text_scale(&[1.0, 1.5, 2.0], "whole CLI facts", |scale| {
        let mut wrapped = false;
        for (width, height) in [
            (504.0, 628.0),
            (924.0, 640.0),
            (1_095.0, 700.0),
            (1_564.0, 182.0),
            (2_264.0, 432.0),
        ] {
            let context = format!("{scale}\u{d7} {width}\u{d7}{height}");
            let mut state = fixture();
            let reading = state.ai.reading.as_mut().expect("the fixture's reading");
            reading.clis[2] = AiCli {
                path: Some(path.to_string()),
                ..cli("agy", State::NotInstalled, Mark::Fail, &fact)
            };
            let (mut runtime, instance, view) = ai_route(state);
            let pages = every_page(&mut runtime, instance, view, &view_cx_at(width, height));
            for id in ["claude", "codex", "agy", "muse"] {
                let key = format!("ai/cli/{id}");
                let label = node(&pages, &key).label.clone();
                // Each line once, from the first page that paints it.
                let mut lines: Vec<(String, String)> = Vec::new();
                for page in &pages {
                    for painted in &page.paint {
                        let UiContent::Text(text) = &painted.content else {
                            continue;
                        };
                        let at = painted.key.as_str();
                        let part = ["title", "detail"].into_iter().any(|part| {
                            let whole = format!("{key}/{part}");
                            at == whole || at.starts_with(&format!("{whole}/line-"))
                        });
                        if part && !painted.peek && !lines.iter().any(|(seen, _)| seen == at) {
                            lines.push((at.to_string(), text.text.clone()));
                        }
                    }
                }
                let squeeze = |text: &str| text.split_whitespace().collect::<String>();
                let joined: String = lines.iter().map(|(_, text)| squeeze(text)).collect();
                assert!(
                    lines.iter().all(|(_, text)| !text.contains('\u{2026}')),
                    "{context} {id}: {lines:#?}"
                );
                assert_eq!(
                    joined,
                    squeeze(&label.replacen(": ", "", 1)),
                    "{context} {id}: {lines:#?}"
                );
                if id == "agy" && scale >= 2.0 {
                    wrapped |= lines
                        .iter()
                        .any(|(at, _)| at.starts_with("ai/cli/agy/detail/line-1"));
                }
            }
        }
        if scale >= 2.0 {
            assert!(wrapped, "{scale}\u{d7}: the long fact is on lines");
        }
    });
}
