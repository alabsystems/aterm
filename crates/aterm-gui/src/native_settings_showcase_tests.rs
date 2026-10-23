// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for Settings ▸ Themes chooser, trial lane, community themes, Save as theme, and Customize sheet.

use super::*;
use crate::settings_preview::theme_card::Section;
use showcase_page::{
    ShowcaseState, ShowcaseTrial, carried_override_keys, demo_font_candidate, palette_reset,
    palette_with, save_as_text, save_name_problem, suggested_save_name,
};

fn themes_view(
    source: &str,
) -> (
    VersionedConfigService,
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let service = VersionedConfigService::new(source.to_string()).expect("valid fixture");
    let (mut runtime, instance, view) = setup();
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    **state = SettingsViewState::from_snapshot(&service.snapshot()).expect("snapshot");
    state.navigate(SettingsRoute::Themes);
    (service, runtime, instance, view)
}

fn act(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    action: &str,
    value: Option<&str>,
) -> crate::native_app::DispatchOutcome {
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value: value.map(|text| SemanticInput::Text(text.to_string())),
            }),
        )
        .unwrap()
}

fn state(runtime: &NativeRuntime, view: crate::native_app::ViewId) -> &SettingsViewState {
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    state
}

fn card_action(runtime: &NativeRuntime, view: crate::native_app::ViewId, id: &str) -> String {
    let slug = state(runtime, view)
        .showcase
        .cards
        .iter()
        .find(|card| card.id == id)
        .unwrap_or_else(|| panic!("{id} has a card"))
        .slug
        .clone();
    format!("settings/showcase/card/{slug}")
}

fn config_patches(outcome: &crate::native_app::DispatchOutcome) -> Vec<(ConfigPatch, OperationId)> {
    outcome
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::ConfigPatch { patch, reply, .. } => Some((patch.clone(), reply.operation)),
            _ => None,
        })
        .collect()
}

fn user_scheme(
    name: &str,
    appearance: aterm_types::Appearance,
) -> (String, aterm_types::ColorScheme) {
    let background = match appearance {
        aterm_types::Appearance::Dark => aterm_types::Rgb::new(0x10, 0x12, 0x18),
        aterm_types::Appearance::Light => aterm_types::Rgb::new(0xf4, 0xf1, 0xea),
    };
    let foreground = match appearance {
        aterm_types::Appearance::Dark => aterm_types::Rgb::new(0xdd, 0xdd, 0xdd),
        aterm_types::Appearance::Light => aterm_types::Rgb::new(0x22, 0x22, 0x22),
    };
    (
        name.to_string(),
        aterm_types::ColorScheme {
            name: name.to_string(),
            appearance,
            background,
            foreground,
            ..aterm_types::ColorScheme::default()
        },
    )
}

#[test]
fn showcase_try_then_keep_writes_config_once() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let nord = card_action(&runtime, view, "Nord");
    let tried = act(&mut runtime, instance, view, &nord, None);
    assert!(config_patches(&tried).is_empty(), "trying writes nothing");
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Show(
            "Nord".to_string()
        ))
    );
    let kept = act(&mut runtime, instance, view, &nord, None);
    let patches = config_patches(&kept);
    assert_eq!(patches.len(), 1, "one write");
    let (patch, operation) = patches[0].clone();
    assert_eq!(patch.edits.len(), 1);
    assert_eq!(patch.edits[0].key, prefs::EDIT_THEME);
    assert_eq!(patch.edits[0].value.as_deref(), Some("Nord"));
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Keeping(
            "Nord".to_string()
        )),
        "the trial stands while the write is in flight"
    );
    assert!(
        config_patches(&act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/keep",
            None
        ))
        .is_empty()
    );

    let before = service.snapshot().revision;
    let (published, undo) = apply_real_config_patch(&mut service, &patch);
    assert_eq!(published.revision, before + 1);
    assert_eq!(published.config.theme.as_deref(), Some("Nord"));
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Applied {
                    revision: published.revision,
                    undo,
                },
            },
        )
        .unwrap();
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap();
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
}

#[test]
fn showcase_keep_on_a_split_writes_only_the_shown_side() {
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Nord,light:Solarized Light\"\n");
    let card = card_action(&runtime, view, "Gruvbox Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let patches = config_patches(&kept);
    assert_eq!(patches.len(), 1);
    assert_eq!(
        patches[0].0.edits[0].value.as_deref(),
        Some("dark:Nord,light:Gruvbox Light")
    );
}

#[test]
fn showcase_keep_with_no_theme_writes_the_plain_name() {
    let keep = |source: &str, id: &str| {
        let (mut service, mut runtime, instance, view) = themes_view(source);
        {
            let Some(AppViewState::Settings(shown)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            shown.theme_preview_side = Some(aterm_types::Appearance::Dark);
        }
        let card = card_action(&runtime, view, id);
        let _ = act(&mut runtime, instance, view, &card, None);
        let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
        let patches = config_patches(&kept);
        assert_eq!(patches.len(), 1, "{source:?} {id}");
        let feedback = state(&runtime, view).feedback.clone();
        let (published, _) = apply_real_config_patch(&mut service, &patches[0].0);
        (published.config.theme.clone(), feedback)
    };
    for id in ["Aterm Day", "Nord"] {
        assert_eq!(
            keep("", id),
            (Some(id.to_string()), Some(format!("Keeping {id}\u{2026}")))
        );
    }
    assert_eq!(
        keep("theme = \"dark:Nord,light:Default Light\"\n", "Aterm Day").0,
        Some("dark:Aterm Day,light:Default Light".to_string())
    );

    let mut lab = super::modified::Lab::new("");
    lab.state_mut().navigate(SettingsRoute::Themes);
    let slug = lab
        .state()
        .showcase
        .cards
        .iter()
        .find(|card| card.id == "Aterm Day")
        .expect("Aterm Day has a card")
        .slug
        .clone();
    let _ = lab.invoke(&format!("settings/showcase/card/{slug}"), None);
    lab.commit("settings/showcase/keep", None);
    assert_eq!(lab.text(), "theme = \"Aterm Day\"\n");
    let feedback = lab.state().feedback.clone().unwrap_or_default();
    assert!(
        feedback.starts_with("Color theme set to Aterm Day"),
        "the status names the one theme: {feedback}"
    );
}

#[test]
fn showcase_try_then_revert_leaves_toml_byte_equal() {
    let source = "# mine\ntheme = \"Hearth\"  # warm\nforeground = \"#c0c0c0\"\n";
    for ending in ["revert", "escape", "navigate"] {
        let (service, mut runtime, instance, view) = themes_view(source);
        let before = service.snapshot().text.to_string();
        let nord = card_action(&runtime, view, "Nord");
        let mut effects = act(&mut runtime, instance, view, &nord, None).effects;
        assert!(state(&runtime, view).theme_trial_intent().is_some());
        match ending {
            "revert" => {
                effects.extend(
                    act(
                        &mut runtime,
                        instance,
                        view,
                        "settings/showcase/revert",
                        None,
                    )
                    .effects,
                );
            }
            "escape" => {
                effects.extend(
                    runtime
                        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
                        .unwrap()
                        .effects,
                );
            }
            _ => {
                effects.extend(
                    act(
                        &mut runtime,
                        instance,
                        view,
                        route_action(SettingsRoute::Home).as_str(),
                        None,
                    )
                    .effects,
                );
            }
        }
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
            "{ending}: no write"
        );
        assert_eq!(state(&runtime, view).theme_trial_intent(), None, "{ending}");
        assert_eq!(service.snapshot().text.to_string(), before, "{ending}");
    }
}

#[test]
fn a_search_or_the_category_list_over_the_themes_page_ends_the_trial() {
    let source = "theme = \"Dracula\"\n";
    let (service, mut runtime, instance, view) = themes_view(source);
    let before = service.snapshot().text.to_string();
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.showcase.filter = crate::native_text_input::TextInputState::new("o".to_string());
    }
    let hearth = card_action(&runtime, view, "Hearth");
    let mut effects = act(&mut runtime, instance, view, &hearth, None).effects;
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Show(
            "Hearth".to_string()
        )),
        "premise: Hearth is tried"
    );
    focus_key(&mut runtime, instance, view, "settings/search");
    assert!(
        state(&runtime, view).theme_trial_intent().is_some(),
        "focus alone keeps the trial: the page is still on glass"
    );
    effects.extend(
        runtime
            .dispatch(instance, view, AppEvent::InsertText("bold".to_string()))
            .unwrap()
            .effects,
    );
    let replaced = state(&runtime, view);
    assert_eq!(
        replaced.search, "bold",
        "premise: Search Results replace the page"
    );
    assert_eq!(replaced.theme_trial_intent(), None, "the trial ended");
    assert_eq!(replaced.showcase_preview_id(), None);
    let cx = view_cx_at(1_564.0, 854.0);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    for node in &compiled.semantics {
        if node.key.as_str().starts_with("settings/preview/") {
            assert!(
                !format!("{:?}", node.value).contains("Hearth"),
                "{}: the search preview draws the tried theme: {:?}",
                node.key.as_str(),
                node.value
            );
        }
    }
    runtime
        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
        .unwrap();
    let state_now = state(&runtime, view);
    assert!(state_now.search.is_empty(), "Escape cleared the search");
    assert_eq!(state_now.showcase.filter.value(), "o", "the filter stays");
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::ConfigPatch { .. })),
        "nothing is written"
    );
    assert_eq!(service.snapshot().text.to_string(), before);

    let (_service, mut runtime, instance, view) = themes_view(source);
    let hearth = card_action(&runtime, view, "Hearth");
    act(&mut runtime, instance, view, &hearth, None);
    assert!(state(&runtime, view).theme_trial_intent().is_some());
    act(
        &mut runtime,
        instance,
        view,
        "settings/compact-navigation",
        None,
    );
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    runtime
        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
        .unwrap();
    assert!(
        !state(&runtime, view).compact_navigation,
        "Escape closed the list, not a trial it hid"
    );
}

#[test]
fn the_themes_page_keeps_its_heading_and_its_filter_stays_put() {
    for (width, height) in [
        (584.0, 292.0),
        (584.0, 348.0),
        (584.0, 350.0),
        (724.0, 380.0),
        (724.0, 434.0),
        (738.0, 418.0),
        (1_074.0, 684.0),
    ] {
        let cx = view_cx_at(width, height);
        let mut filter_y = None;
        for filter in ["", "Nord", "Nordz"] {
            let context = format!("{width}x{height} filter {filter:?}");
            let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.showcase.filter =
                    crate::native_text_input::TextInputState::new(filter.to_string());
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            assert_zero_top_paint(&compiled, &context);
            let heading = compiled
                .semantic(&UiKey::new("settings/page-heading/themes"))
                .unwrap_or_else(|| panic!("{context}: the page names itself"));
            assert_eq!(heading.role, SemanticRole::Heading, "{context}");
            let y = compiled
                .semantic(&UiKey::new("settings/showcase/filter"))
                .unwrap_or_else(|| panic!("{context}: the filter"))
                .rect
                .y;
            match filter_y {
                None => filter_y = Some(y),
                Some(was) => assert_eq!(y, was, "{context}: the filter moved"),
            }
        }
    }
}

fn own_top(compiled: &crate::native_ui::CompiledUi, node: &crate::native_ui::SemanticNode) -> f32 {
    compiled
        .paint
        .iter()
        .find(|paint| paint.key == node.key)
        .map_or(node.rect.y, |paint| paint.rect.y)
}

#[test]
fn every_themes_screen_names_the_section_it_starts_in() {
    let mut inside = 0;
    for (width, height) in [(584.0, 350.0), (738.0, 418.0), (1_074.0, 684.0)] {
        let cx = view_cx_at(width, height);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let cards = state(&runtime, view).showcase.cards.clone();
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let list = rect_of(compiled, "settings/showcase/list");
            if list.height < 24.0 + 8.0 + 32.0 {
                return;
            }
            let first = compiled
                .semantics
                .iter()
                .filter(|node| node.role == SemanticRole::ListBoxOption && !node.offscreen)
                .filter(|node| node.rect.y >= list.y - 0.5)
                .min_by(|a, b| a.rect.y.total_cmp(&b.rect.y));
            let Some(first) = first else {
                return;
            };
            let slug = first.key.as_str().trim_start_matches(ROW);
            let Some(card) = cards.iter().find(|card| card.slug == slug) else {
                return;
            };
            let Some(caption) = card.section.caption() else {
                return;
            };
            if card.section == Section::Community {
                return;
            }
            let heading = compiled.semantics.iter().find(|node| {
                node.role == SemanticRole::Heading
                    && !node.offscreen
                    && node.label == caption
                    && node.rect.y <= first.rect.y
            });
            assert!(
                heading.is_some(),
                "{width}x{height} screen {top}: {slug} with no {caption} over it"
            );
            inside += 1;
        });
    }
    assert!(inside > 0, "some screen starts inside a captioned section");

    let mut inside = 0;
    for (width, height) in [(584.0, 350.0), (738.0, 418.0)] {
        let cx = view_cx_at(width, height);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let Some(list) = compiled
                .semantic(&UiKey::new("settings/showcase/sheet-list"))
                .map(|node| node.rect)
            else {
                return;
            };
            let first = compiled
                .semantics
                .iter()
                .filter(|node| !node.offscreen && own_top(compiled, node) >= list.y - 0.5)
                .filter(|node| {
                    showcase_page::SHOWCASE_COLOR_KEYS
                        .iter()
                        .any(|key| node.key.as_str() == format!("settings/control/{key}"))
                })
                .min_by(|a, b| a.rect.y.total_cmp(&b.rect.y));
            let Some(first) = first else {
                return;
            };
            if first.key.as_str()
                == format!("settings/control/{}", showcase_page::SHOWCASE_COLOR_KEYS[0])
                || list.height < 24.0 + 4.0 + 36.0
            {
                return;
            }
            assert!(
                compiled
                    .semantics
                    .iter()
                    .any(|node| node.role == SemanticRole::Heading
                        && !node.offscreen
                        && node.label == "COLORS"
                        && node.rect.y <= first.rect.y),
                "{width}x{height} sheet screen {top}: {} with no COLORS over it",
                first.key.as_str()
            );
            inside += 1;
        });
    }
    let _ = inside;
}

#[test]
fn save_as_writes_conf_that_parses_to_resolved_scheme() {
    let source = "theme = \"Nord\"\nforeground = \"#fafafa\"\nselection_foreground = \"#101010\"\npalette = [\"#ff0000\", \"#00ff00\"]\n";
    let (mut service, mut runtime, instance, view) = themes_view(source);
    let resolved = {
        let state = state(&runtime, view);
        preview_resolved_colors_with(state, state.presented_motion.get().system_dark, None)
    };
    assert_eq!(
        resolved.scheme.foreground,
        aterm_types::Rgb::new(0xfa, 0xfa, 0xfa)
    );
    assert_eq!(resolved.scheme.ansi[0], aterm_types::Rgb::new(0xff, 0, 0));
    let text = save_as_text(
        &resolved,
        "Nord Custom",
        "Nord",
        &aterm_types::scheme::ThemeTypography::NONE,
    )
    .expect("serializes");
    let parsed = aterm_types::scheme::parse_theme_str(&text).expect("parses");
    let mut expected = resolved.scheme.clone();
    expected.name = "Nord Custom".to_string();
    expected.appearance = resolved.reported;
    assert_eq!(parsed.scheme, expected);
    assert_eq!(parsed.scheme.selection_fg, resolved.scheme.selection_fg);
    assert_eq!(
        carried_override_keys(&resolved, &aterm_types::scheme::ThemeTypography::NONE),
        vec![
            prefs::EDIT_FOREGROUND.to_string(),
            prefs::EDIT_SELECTION_FOREGROUND.to_string(),
            prefs::EDIT_PALETTE.to_string(),
        ]
    );

    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-name",
        Some("Nord Custom"),
    );
    let saved = act(&mut runtime, instance, view, "settings/showcase/save", None);
    let (id, written, operation) = saved
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Themes {
                request: ThemesRequest::Save { id, text },
                reply,
            } => Some((id.clone(), text.clone(), reply.operation)),
            _ => None,
        })
        .expect("a save request");
    assert_eq!(id, "Nord Custom");
    assert_eq!(
        aterm_types::scheme::parse_theme_str(&written)
            .unwrap()
            .scheme,
        expected
    );
    assert!(
        config_patches(&saved).is_empty(),
        "nothing selected before the file exists"
    );
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let (name, scheme) = (id.clone(), parsed.scheme);
    let published =
        service.replace_theme_catalog(crate::app_config::ThemeCatalog::from_schemes([(
            name, scheme,
        )]));
    let selected = runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap();
    let patches = config_patches(&selected);
    assert_eq!(patches.len(), 1, "one patch selects it");
    let edits: BTreeMap<String, Option<String>> = patches[0]
        .0
        .edits
        .iter()
        .map(|edit| (edit.key.clone(), edit.value.clone()))
        .collect();
    assert_eq!(
        edits,
        BTreeMap::from([
            (
                prefs::EDIT_THEME.to_string(),
                Some("Nord Custom".to_string())
            ),
            (prefs::EDIT_FOREGROUND.to_string(), None),
            (prefs::EDIT_SELECTION_FOREGROUND.to_string(), None),
            (prefs::EDIT_PALETTE.to_string(), None),
        ])
    );
    let (published, _) = apply_real_config_patch(&mut service, &patches[0].0);
    let after = published
        .config
        .resolved_colors(resolved.side, &published.assets.themes, None);
    assert_eq!(after.scheme.foreground, resolved.scheme.foreground);
    assert_eq!(after.scheme.ansi, resolved.scheme.ansi);
    assert_eq!(after.scheme.selection_fg, resolved.scheme.selection_fg);
}

#[test]
fn save_as_refuses_names_that_cannot_be_new_themes() {
    let themes = crate::app_config::ThemeCatalog::from_schemes([user_scheme(
        "harbour",
        aterm_types::Appearance::Dark,
    )]);
    for bad in ["", "Nord", "nord", "Harbour", "a/b", "dark:x"] {
        assert!(save_name_problem(bad, &themes).is_some(), "{bad:?}");
    }
    assert!(save_name_problem("Harbour Custom", &themes).is_none());
}

fn start_save_as(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
) -> (String, aterm_types::ColorScheme, OperationId) {
    let _ = act(runtime, instance, view, "settings/showcase/save-as", None);
    let _ = act(
        runtime,
        instance,
        view,
        "settings/showcase/save-name",
        Some("Saved Custom"),
    );
    let outcome = act(runtime, instance, view, "settings/showcase/save", None);
    outcome
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            AppEffect::Themes {
                request: ThemesRequest::Save { id, text },
                reply,
            } => Some((
                id,
                aterm_types::scheme::parse_theme_str(&text).unwrap().scheme,
                reply.operation,
            )),
            _ => None,
        })
        .expect("the real page admitted a theme save")
}

fn publish_saved_catalog(
    service: &mut VersionedConfigService,
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    id: &str,
    scheme: aterm_types::ColorScheme,
) -> crate::native_app::DispatchOutcome {
    let snapshot =
        service.replace_theme_catalog(crate::app_config::ThemeCatalog::from_schemes([(
            id.to_string(),
            scheme,
        )]));
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
        .unwrap()
}

#[test]
fn save_as_failure_preserves_exact_unsaved_colour_and_palette_drafts() {
    for outcome in [
        ThemesOutcome::Failed {
            message: "read-only themes directory".into(),
        },
        ThemesOutcome::Cancelled,
    ] {
        let (_, mut runtime, instance, view) =
            themes_view("theme = \"Nord\"\nforeground = \"#fafafa\"\n");
        let drafts = {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            for (key, text) in [
                (prefs::EDIT_FOREGROUND, "#123456"),
                (prefs::EDIT_PALETTE, "#654321, #aabbcc"),
            ] {
                let mut input = crate::native_text_input::TextInputState::new(text.into());
                input.set_selection(1, 3);
                state.field_inputs.insert(key.into(), input);
            }
            state.editing_field = Some(prefs::EDIT_FOREGROUND.into());
            state.field_inputs.clone()
        };
        let (_, scheme, operation) = start_save_as(&mut runtime, instance, view);
        assert_eq!(scheme.foreground, aterm_types::Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(scheme.ansi[0], aterm_types::Rgb::new(0x65, 0x43, 0x21));
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished { operation, outcome },
            )
            .unwrap();
        assert_eq!(
            state(&runtime, view).field_inputs,
            drafts,
            "a failed save lost the person's draft"
        );
    }
}

#[test]
fn save_as_delayed_catalog_preserves_newer_theme_or_colour_values() {
    for (key, value) in [
        (prefs::EDIT_THEME, "Dracula"),
        (prefs::EDIT_FOREGROUND, "#123456"),
    ] {
        let (mut service, mut runtime, instance, view) =
            themes_view("theme = \"Nord\"\nforeground = \"#fafafa\"\n");
        let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished {
                    operation,
                    outcome: ThemesOutcome::Saved { id: id.clone() },
                },
            )
            .unwrap();
        let patch = ConfigPatch {
            base_revision: service.snapshot().revision,
            edits: vec![ConfigEdit {
                key: key.into(),
                expected: ExpectedConfigValue::Exact(state(&runtime, view).raw_value(key)),
                value: Some(value.into()),
            }],
        };
        let (snapshot, _) = apply_real_config_patch(&mut service, &patch);
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        let catalog =
            publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
        assert!(
            config_patches(&catalog).is_empty(),
            "the old Save issued a patch over newer {key}"
        );
        assert_eq!(state(&runtime, view).raw_value(key).as_deref(), Some(value));
    }
}

#[test]
fn save_as_catalog_before_completion_selects_after_its_own_success() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    assert!(
        config_patches(&publish_saved_catalog(
            &mut service,
            &mut runtime,
            instance,
            view,
            &id,
            scheme
        ))
        .is_empty()
    );
    let finished = runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let patches = config_patches(&finished);
    assert_eq!(
        patches.len(),
        1,
        "a ready catalog lost the successful Save completion"
    );
    let (snapshot, _) = apply_real_config_patch(&mut service, &patches[0].0);
    assert_eq!(snapshot.config.theme.as_deref(), Some(id.as_str()));
}

#[test]
fn save_as_leaving_the_page_cancels_delayed_auto_selection() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    let Some(AppViewState::Settings(view_state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    view_state.navigate(SettingsRoute::Appearance);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let catalog = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    assert!(
        config_patches(&catalog).is_empty(),
        "a departed page selected its old save"
    );
    assert_eq!(
        state(&runtime, view)
            .raw_value(prefs::EDIT_THEME)
            .as_deref(),
        Some("Nord")
    );
}

#[test]
fn save_as_success_retires_only_its_unchanged_submitted_drafts() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        for (key, text) in [
            (prefs::EDIT_FOREGROUND, "#123456"),
            (prefs::EDIT_PALETTE, "#654321"),
        ] {
            state.field_inputs.insert(
                key.into(),
                crate::native_text_input::TextInputState::new(text.into()),
            );
        }
    }
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    let newer = {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        let mut input = crate::native_text_input::TextInputState::new("#123456".into());
        input.select_all();
        input.insert("#123456");
        input.set_selection(1, 3);
        state
            .field_inputs
            .insert(prefs::EDIT_FOREGROUND.into(), input.clone());
        input
    };
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let selected = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    let (_, select) = config_patches(&selected)[0].clone();
    finish_patch(&mut runtime, instance, view, select, true);
    assert_eq!(
        state(&runtime, view)
            .field_inputs
            .get(prefs::EDIT_FOREGROUND),
        Some(&newer)
    );
    assert!(
        !state(&runtime, view)
            .field_inputs
            .contains_key(prefs::EDIT_PALETTE),
        "a successful save did not consume its own unchanged palette draft"
    );
}

#[test]
fn save_as_retires_only_the_exact_submitted_sparse_slot_input() {
    for newer_input in [false, true] {
        let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/palette/12",
            None,
        );
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/palette-slot",
            Some("#123456"),
        );
        let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
        assert_eq!(
            hex(scheme.ansi[12]),
            "#123456",
            "the submitted file carries the draft"
        );
        let expected = {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            let slot = state
                .showcase
                .slot
                .as_mut()
                .expect("the submitted editor stays open");
            if newer_input {
                slot.input.select_all();
                slot.input.insert("#123456");
                slot.input.set_selection(1, 3);
            }
            slot.input.clone()
        };
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished {
                    operation,
                    outcome: ThemesOutcome::Saved { id: id.clone() },
                },
            )
            .unwrap();
        let selected =
            publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
        let (_, operation) = config_patches(&selected)[0].clone();
        finish_patch(&mut runtime, instance, view, operation, true);
        let remaining = state(&runtime, view).showcase.slot.as_ref();
        if newer_input {
            assert_eq!(
                remaining.map(|slot| &slot.input),
                Some(&expected),
                "the newer same-colour edit and undo history belong to the person"
            );
        } else {
            assert!(
                remaining.is_none(),
                "the exact submitted draft is carried by the saved theme"
            );
        }
    }
}

fn select_saved_theme_snapshot(
    source: &str,
) -> (
    std::sync::Arc<crate::app_config::Config>,
    std::sync::Arc<crate::app_config::Config>,
) {
    let (mut service, mut runtime, instance, view) = themes_view(source);
    let before = service.snapshot().config.clone();
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let selected = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    let patches = config_patches(&selected);
    assert_eq!(patches.len(), 1, "the actual saved theme is selected");
    let (after, _) = apply_real_config_patch(&mut service, &patches[0].0);
    (before, after.config.clone())
}

#[test]
fn save_as_keeps_extended_sparse_palette_values_the_file_cannot_carry() {
    let (before, after) =
        select_saved_theme_snapshot("[palette_overrides]\n4 = \"#123456\"\n200 = \"#abcdef\"\n");
    assert_eq!(
        after.palette_overrides, before.palette_overrides,
        "a sixteen-colour theme cannot carry slot 200 or authorize clearing its source table"
    );
}

#[test]
fn save_as_keeps_an_extended_positional_palette_the_file_cannot_carry() {
    let source = format!("palette = [{}\"#abcdef\"]\n", "\"#123456\", ".repeat(16));
    let (before, after) = select_saved_theme_snapshot(&source);
    assert_eq!(
        after.palette, before.palette,
        "a sixteen-colour theme cannot carry slot 16 or authorize clearing its source list"
    );
}

#[test]
fn save_as_keeps_drafts_shadowed_by_retained_extended_palette_sources() {
    for positional in [true, false] {
        let source = if positional {
            format!("palette = [{}\"#fedcba\"]\n", "\"#123456\", ".repeat(16))
        } else {
            "[palette_overrides]\n12 = \"#123456\"\n200 = \"#fedcba\"\n".to_string()
        };
        let (mut service, mut runtime, instance, view) = themes_view(&source);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/palette/12",
            None,
        );
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/palette-slot",
            Some("#abcdef"),
        );
        let input = state(&runtime, view)
            .showcase
            .slot
            .as_ref()
            .unwrap()
            .input
            .clone();
        let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
        assert_eq!(hex(scheme.ansi[12]), "#abcdef");
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished {
                    operation,
                    outcome: ThemesOutcome::Saved { id: id.clone() },
                },
            )
            .unwrap();
        let selected =
            publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
        let (patch, operation) = config_patches(&selected)[0].clone();
        let (snapshot, _) = apply_real_config_patch(&mut service, &patch);
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
            .unwrap();
        finish_patch(&mut runtime, instance, view, operation, true);
        let state = state(&runtime, view);
        assert_eq!(
            state.showcase.slot.as_ref().map(|slot| &slot.input),
            Some(&input),
            "positional={positional}: the retained config still shadows the saved colour, so its input remains a draft"
        );
        let shown =
            preview_resolved_colors_with(state, state.presented_motion.get().system_dark, None);
        assert_eq!(
            hex(shown.scheme.ansi[12]),
            "#abcdef",
            "positional={positional}: the stage retains what the person typed"
        );
    }
}

#[test]
fn save_as_ignores_another_theme_operations_failure() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let folder = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/open-folder",
        None,
    );
    let other = folder
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Themes {
                request: ThemesRequest::OpenFolder,
                reply,
            } => Some(reply.operation),
            _ => None,
        })
        .expect("an independent folder operation");
    assert_ne!(operation, other);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation: other,
                outcome: ThemesOutcome::Failed {
                    message: "could not open folder".into(),
                },
            },
        )
        .unwrap();
    let published = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    let patches = config_patches(&published);
    assert_eq!(
        patches.len(),
        1,
        "an unrelated operation erased the saved file's selection"
    );
    let (snapshot, _) = apply_real_config_patch(&mut service, &patches[0].0);
    assert_eq!(snapshot.config.theme.as_deref(), Some(id.as_str()));
}

#[test]
fn palette_list_edits_in_place_and_never_grows() {
    let theme = aterm_types::ColorScheme::default().ansi;
    let hex = |c: aterm_types::Rgb| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    assert_eq!(palette_with(&[], 2, "#123456"), None);
    let list = vec![hex(theme[0]), hex(theme[1]), "#123456".to_string()];
    assert_eq!(palette_with(&list, 3, "#abcdef"), None);
    let two = palette_with(&list, 0, "#abcdef").expect("the list reaches slot 0");
    assert_eq!(two[0], "#abcdef");
    assert_eq!(two.len(), list.len());
    assert!(palette_reset(&list, 2, &theme).is_empty());
    assert_eq!(palette_reset(&two, 2, &theme), vec!["#abcdef".to_string()]);
}

#[test]
fn the_rail_selects_the_themes_row_under_appearance() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(1_200.0, 820.0);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let row = |key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{key} on the rail"))
    };
    let selected =
        |node: &crate::native_ui::SemanticNode| node.state.is_some_and(|state| state.selected);
    let themes = row("settings/nav/themes");
    let appearance = row("settings/nav/appearance");
    assert!(selected(themes), "the sub-page's own row is selected");
    assert!(!selected(appearance), "its parent's is not");
    assert_eq!(
        themes.action.as_ref().map(ActionId::as_str),
        Some(route_action(SettingsRoute::Themes).as_str())
    );
    let navigation = row("settings/navigation");
    let wallpaper = row("settings/nav/wallpaper");
    let gap = wallpaper.rect.y - themes.rect.bottom();
    assert!(gap >= 0.0, "{gap}");
    assert!(
        (themes.rect.y - appearance.rect.bottom() - gap).abs() < 0.01,
        "{:?} under {:?}",
        themes.rect,
        appearance.rect
    );
    assert!(themes.rect.x > appearance.rect.x, "indented");
    assert!(themes.rect.bottom() <= navigation.rect.bottom() + 0.01);
    let lines = compiled.controls_lines();
    let line = |key: &str| {
        let quoted = format!("key=\"{key}\"");
        lines
            .iter()
            .find(|line| line.contains(&quoted))
            .unwrap_or_else(|| panic!("{key} in controls: {lines:#?}"))
            .clone()
    };
    assert!(line("settings/nav/themes").contains("selected:true"));
    assert!(line("settings/nav/appearance").contains("selected:false"));

    let _ = act(
        &mut runtime,
        instance,
        view,
        route_action(SettingsRoute::Appearance).as_str(),
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/nav/themes"))
            .is_none()
    );
    assert!(
        compiled
            .semantic(&UiKey::new("settings/nav/appearance"))
            .is_some_and(selected)
    );
    assert_eq!(
        SettingsRoute::from_path("/themes"),
        Some(SettingsRoute::Themes)
    );
}

fn admit_catalog(
    service: &mut VersionedConfigService,
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    schemes: Vec<(String, aterm_types::ColorScheme)>,
) -> crate::native_app::DispatchOutcome {
    let published =
        service.replace_theme_catalog(crate::app_config::ThemeCatalog::from_schemes(schemes));
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap()
}

fn themes_request(
    outcome: &crate::native_app::DispatchOutcome,
) -> Option<(ThemesRequest, OperationId)> {
    outcome.effects.iter().find_map(|effect| match effect {
        AppEffect::Themes { request, reply } => Some((request.clone(), reply.operation)),
        _ => None,
    })
}

fn finish_patch(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    operation: OperationId,
    applied: bool,
) {
    let outcome = if applied {
        ConfigPatchOutcome::Applied {
            revision: state(runtime, view).snapshot_revision + 1,
            undo: None,
        }
    } else {
        ConfigPatchOutcome::Rejected {
            message: "no".to_string(),
        }
    };
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ConfigPatchFinished { operation, outcome },
        )
        .unwrap();
}

#[test]
fn save_as_keeps_drafts_and_name_until_the_theme_is_selected() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.field_inputs.insert(
            prefs::EDIT_FOREGROUND.to_string(),
            crate::native_text_input::TextInputState::new("#fafafa".to_string()),
        );
    }
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/12",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette-slot",
        Some("#123456"),
    );
    let save = |runtime: &mut NativeRuntime| {
        let _ = act(runtime, instance, view, "settings/showcase/save-as", None);
        let _ = act(
            runtime,
            instance,
            view,
            "settings/showcase/save-name",
            Some("Nord Mine"),
        );
        let saved = act(runtime, instance, view, "settings/showcase/save", None);
        themes_request(&saved).expect("a save request")
    };
    let drafted = |runtime: &NativeRuntime| {
        let state = state(runtime, view);
        (
            state
                .field_inputs
                .get(prefs::EDIT_FOREGROUND)
                .map(|input| input.value().to_string()),
            state
                .showcase
                .slot
                .as_ref()
                .map(|slot| slot.input.value().to_string()),
        )
    };
    let both = (Some("#fafafa".to_string()), Some("#123456".to_string()));
    let (request, operation) = save(&mut runtime);
    let ThemesRequest::Save { text, .. } = request else {
        panic!("{request:?}")
    };
    assert_eq!(drafted(&runtime), both, "drafts stay while the write runs");
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Failed {
                    message: "the folder is read-only".to_string(),
                },
            },
        )
        .unwrap();
    assert_eq!(drafted(&runtime), both, "a failed write loses no draft");
    let reopened = state(&runtime, view)
        .showcase
        .save
        .as_ref()
        .expect("reopened");
    assert_eq!(reopened.name.value(), "Nord Mine");
    assert!(reopened.error.is_some());

    let (_, operation) = save(&mut runtime);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved {
                    id: "Nord Mine".to_string(),
                },
            },
        )
        .unwrap();
    assert_eq!(drafted(&runtime), both, "drafts stay until it is selected");
    let parsed = aterm_types::scheme::parse_theme_str(&text).expect("parses");
    let selected = admit_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        vec![("Nord Mine".to_string(), parsed.scheme)],
    );
    let (_, select) = config_patches(&selected)[0].clone();
    assert_eq!(
        drafted(&runtime),
        both,
        "drafts stay while the select is in flight"
    );
    finish_patch(&mut runtime, instance, view, select, true);
    assert_eq!(
        drafted(&runtime),
        (None, None),
        "the selected theme carries them"
    );
}

#[test]
fn save_as_during_a_trial_holds_it_until_the_select_lands() {
    let (mut service, mut runtime, instance, view) = themes_view("");
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let saved = act(&mut runtime, instance, view, "settings/showcase/save", None);
    let (ThemesRequest::Save { id, text }, operation) = themes_request(&saved).expect("a save")
    else {
        panic!("not a save")
    };
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let scheme = aterm_types::scheme::parse_theme_str(&text).unwrap().scheme;
    let selected = admit_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        vec![(id.clone(), scheme)],
    );
    let (_, select) = config_patches(&selected)[0].clone();
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Keeping(
            "Nord".to_string()
        ))
    );
    finish_patch(&mut runtime, instance, view, select, true);
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    let showcase = &state(&runtime, view).showcase;
    let settled = showcase.settled.clone().expect("announced").words;
    assert!(settled.ends_with(&showcase.label_of(&id)), "{settled}");
    assert!(!settled.ends_with(&showcase.label_of("Nord")), "{settled}");
}

#[test]
fn a_highlight_move_after_save_drops_the_select() {
    for arrow in [true, false] {
        let (mut service, mut runtime, instance, view) = themes_view("");
        let nord = card_action(&runtime, view, "Nord");
        let _ = act(&mut runtime, instance, view, &nord, None);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/save-as",
            None,
        );
        let saved = act(&mut runtime, instance, view, "settings/showcase/save", None);
        let (ThemesRequest::Save { id, text }, operation) = themes_request(&saved).expect("a save")
        else {
            panic!("not a save")
        };
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished {
                    operation,
                    outcome: ThemesOutcome::Saved { id: id.clone() },
                },
            )
            .unwrap();
        assert!(state(&runtime, view).showcase.pending_select.is_some());
        if arrow {
            if state(&runtime, view).showcase.sheet_open {
                runtime
                    .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
                    .unwrap();
            }
            let _ = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
            focus_on(&mut runtime, view, &nord);
            runtime
                .dispatch(instance, view, AppEvent::StepFocus(1))
                .unwrap();
        }
        let tried = state(&runtime, view).showcase.trial.clone();
        let scheme = aterm_types::scheme::parse_theme_str(&text).unwrap().scheme;
        let selected = admit_catalog(
            &mut service,
            &mut runtime,
            instance,
            view,
            vec![(id, scheme)],
        );
        assert_eq!(config_patches(&selected).is_empty(), arrow, "arrow {arrow}");
        if arrow {
            assert_eq!(state(&runtime, view).showcase.trial, tried, "still tried");
        }
    }
}

#[test]
fn keep_from_the_keyboard_lands_on_the_kept_row() {
    let cx = view_cx_at(1_284.0, 714.0);
    for (button, on_strip) in [
        ("settings/showcase/keep", true),
        (SHOWCASE_KEEP_COLOURS_KEY, true),
        ("settings/showcase/keep", false),
    ] {
        let (_service, mut runtime, instance, view) = typography_view(
            "theme = \"Dracula\"\n",
            studio_catalog("font_family = Berkeley Mono\n"),
        );
        let studio = card_action(&runtime, view, "Studio");
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let _ = act(&mut runtime, instance, view, &studio, None);
        if button == SHOWCASE_KEEP_COLOURS_KEY {
            let _ = act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/keep-more",
                None,
            );
        }
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let focus = if on_strip { button } else { studio.as_str() };
        focus_on(&mut runtime, view, focus);
        let kept = act(&mut runtime, instance, view, button, None);
        let (_, operation) = config_patches(&kept)[0].clone();
        finish_patch(&mut runtime, instance, view, operation, true);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let landed = state(&runtime, view)
            .common
            .last_focus
            .clone()
            .expect("the keyboard is somewhere");
        assert_eq!(landed.as_str(), studio, "{button} {on_strip}");
        assert!(
            compiled.focus_order.contains(&landed),
            "{button}: {} is a live stop",
            landed.as_str()
        );
    }
}

#[test]
fn an_arrow_during_keeping_tries_the_next_row() {
    let (mut service, mut runtime, instance, view) = themes_view("");
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let (patch, operation) = config_patches(&kept)[0].clone();
    let _ = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    focus_on(&mut runtime, view, &nord);
    let _ = act(&mut runtime, instance, view, &nord, None);
    assert!(matches!(
        state(&runtime, view).showcase.trial,
        ShowcaseTrial::Keeping { .. }
    ));
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    let focus = state(&runtime, view)
        .common
        .last_focus
        .as_ref()
        .map(|key| key.as_str().to_string())
        .expect("the keyboard is somewhere");
    assert_ne!(focus, nord, "the arrow moved the keyboard");
    let ShowcaseTrial::Trying { id: tried } = state(&runtime, view).showcase.trial.clone() else {
        panic!("the arrow tries the row it lands on");
    };
    assert_eq!(
        focus,
        card_action(&runtime, view, &tried),
        "keyboard and highlight together"
    );
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Show(tried.clone()))
    );
    let (published, undo) = apply_real_config_patch(&mut service, &patch);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Applied {
                    revision: published.revision,
                    undo,
                },
            },
        )
        .unwrap();
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap();
    assert_eq!(service.snapshot().config.theme.as_deref(), Some("Nord"));
    assert_eq!(
        state(&runtime, view).showcase.trial,
        ShowcaseTrial::Trying { id: tried },
        "the landed Keep leaves the newer trial standing"
    );
}

fn takes_press(
    runtime: &NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    key: &str,
) -> (bool, bool) {
    let compiled = compile_settings_view(runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    let node = compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} is drawn"));
    let state = node.state.unwrap_or_default();
    (state.enabled, state.busy)
}

fn write_follows(outcome: &crate::native_app::DispatchOutcome) -> Vec<Vec<OperationId>> {
    outcome
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::ConfigPatch { follows, .. } => Some(follows.clone()),
            _ => None,
        })
        .collect()
}

fn land_patch(
    service: &mut VersionedConfigService,
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    (patch, operation): (&ConfigPatch, OperationId),
    follows_landed: Option<u64>,
) -> u64 {
    let mut patch = patch.clone();
    if let Some(landed) = follows_landed {
        patch.base_revision = patch.base_revision.max(landed);
    }
    let (published, undo) = apply_real_config_patch(service, &patch);
    let revision = published.revision;
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Applied {
                    revision: published.revision,
                    undo,
                },
            },
        )
        .unwrap();
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap();
    revision
}

#[test]
fn keep_and_revert_take_presses_while_the_keep_saves() {
    let source = "theme = \"Dracula\"\n";
    for escape in [false, true] {
        let (mut service, mut runtime, instance, view) = themes_view(source);
        let nord = card_action(&runtime, view, "Nord");
        let _ = act(&mut runtime, instance, view, &nord, None);
        let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
        let (keep, keep_op) = config_patches(&kept)[0].clone();
        for key in ["settings/showcase/keep", "settings/showcase/revert"] {
            assert_eq!(
                takes_press(&runtime, instance, view, key),
                (true, false),
                "{key} takes a press while the Keep saves"
            );
        }
        let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
        let detail =
            painted_text(&compiled, "settings/showcase/status/detail").expect("the strip's detail");
        assert_eq!(detail, "was Dracula", "while the Keep saves");
        assert!(
            config_patches(&act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/keep",
                None
            ))
            .is_empty(),
            "a second Keep writes nothing the first does not"
        );
        let reverted = if escape {
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel),
                )
                .unwrap()
        } else {
            act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/revert",
                None,
            )
        };
        let back = config_patches(&reverted);
        assert_eq!(back.len(), 1, "escape {escape}: Revert is a write");
        assert_eq!(
            write_follows(&reverted),
            vec![vec![keep_op]],
            "it follows the Keep"
        );
        let (back, back_op) = back[0].clone();
        assert_eq!(back.edits.len(), 1);
        assert_eq!(back.edits[0].key, prefs::EDIT_THEME);
        assert_eq!(back.edits[0].value.as_deref(), Some("Dracula"));
        assert_eq!(
            back.edits[0].expected,
            ExpectedConfigValue::Exact(Some("Nord".to_string())),
            "composed over the Keep"
        );
        let page = state(&runtime, view);
        assert_eq!(page.showcase.trial, ShowcaseTrial::Idle, "shown at once");
        assert_eq!(page.configured_theme_id(), "Dracula");
        assert_eq!(page.theme_trial_intent(), None);
        assert_eq!(
            page.showcase.settled.as_ref().map(|s| s.words.as_str()),
            Some("Reverted to Dracula")
        );
        let landed = land_patch(
            &mut service,
            &mut runtime,
            instance,
            view,
            (&keep, keep_op),
            None,
        );
        land_patch(
            &mut service,
            &mut runtime,
            instance,
            view,
            (&back, back_op),
            Some(landed),
        );
        assert_eq!(
            service.snapshot().text.as_ref(),
            source,
            "the file ends as it began"
        );
        let page = state(&runtime, view);
        assert_eq!(page.showcase.trial, ShowcaseTrial::Idle);
        assert_eq!(page.configured_theme_id(), "Dracula");
    }
}

#[test]
fn keep_colours_only_follows_a_keep_on_its_way() {
    let (mut service, mut runtime, instance, view) = typography_view(
        "theme = \"Dracula\"\n",
        studio_catalog("font_family = Berkeley Mono\n"),
    );
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let (keep, keep_op) = config_patches(&kept)[0].clone();
    assert_eq!(
        takes_press(&runtime, instance, view, "settings/showcase/keep-more"),
        (true, false)
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let pinned = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    let pins = config_patches(&pinned);
    assert_eq!(pins.len(), 1, "Keep colours only is a write");
    assert_eq!(write_follows(&pinned), vec![vec![keep_op]]);
    let (pins, pins_op) = pins[0].clone();
    assert!(
        pins.edits
            .iter()
            .any(|edit| edit.key == prefs::EDIT_FONT_FAMILY && edit.value.as_deref() == Some("")),
        "{pins:?}"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    assert!(
        config_patches(&act(
            &mut runtime,
            instance,
            view,
            SHOWCASE_KEEP_COLOURS_KEY,
            None
        ))
        .is_empty(),
        "a second press writes nothing the first does not"
    );
    let landed = land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&keep, keep_op),
        None,
    );
    land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&pins, pins_op),
        Some(landed),
    );
    let config = service.snapshot().config.clone();
    assert_eq!(config.theme.as_deref(), Some("Studio"));
    assert_eq!(config.font_family.as_deref(), Some(""));
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
}

#[test]
fn a_revert_takes_back_a_keep_and_its_keep_colours_only() {
    let source = "theme = \"Dracula\"\n";
    let (mut service, mut runtime, instance, view) =
        typography_view(source, studio_catalog(PINNABLE_TYPE));
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let (keep, keep_op) = config_patches(&kept)[0].clone();
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let pinned = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    let (pins, pins_op) = config_patches(&pinned)[0].clone();
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    assert_eq!(
        painted_text(&compiled, "settings/showcase/status/detail").as_deref(),
        Some("was Dracula"),
        "the strip names what the trial replaces"
    );
    let reverted = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    let back = config_patches(&reverted);
    assert_eq!(back.len(), 1, "Revert is one write");
    let (back, back_op) = back[0].clone();
    let mut follows = write_follows(&reverted);
    for follows in &mut follows {
        follows.sort();
    }
    let mut both = vec![keep_op, pins_op];
    both.sort();
    assert_eq!(follows, vec![both], "it follows both Keeps");
    let theme = back
        .edits
        .iter()
        .find(|edit| edit.key == prefs::EDIT_THEME)
        .expect("Revert writes the theme back");
    assert_eq!(theme.value.as_deref(), Some("Dracula"));
    assert_eq!(state(&runtime, view).configured_theme_id(), "Dracula");
    let landed = land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&keep, keep_op),
        None,
    );
    let landed = land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&pins, pins_op),
        Some(landed),
    );
    land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&back, back_op),
        Some(landed),
    );
    assert_eq!(
        service.snapshot().text.as_ref(),
        source,
        "the file ends as it began"
    );
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
}

#[test]
fn a_landed_revert_says_it_reverted() {
    let source = "theme = \"Dracula\"\n";
    for colours_only in [false, true] {
        let (mut service, mut runtime, instance, view) =
            typography_view(source, studio_catalog(PINNABLE_TYPE));
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
        let mut writes = vec![config_patches(&kept)[0].clone()];
        if colours_only {
            let _ = act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/keep-more",
                None,
            );
            let pinned = act(
                &mut runtime,
                instance,
                view,
                SHOWCASE_KEEP_COLOURS_KEY,
                None,
            );
            writes.push(config_patches(&pinned)[0].clone());
        }
        let reverted = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/revert",
            None,
        );
        let back = config_patches(&reverted);
        assert_eq!(back.len(), 1, "Revert is one write");
        assert_eq!(
            back[0].0.edits.len(),
            if colours_only { 3 } else { 1 },
            "the take-back writes each key the Keeps wrote"
        );
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some("Reverting to Dracula\u{2026}"),
            "while it saves"
        );
        writes.extend(back);
        let mut landed = None;
        for (patch, operation) in &writes {
            landed = Some(land_patch(
                &mut service,
                &mut runtime,
                instance,
                view,
                (patch, *operation),
                landed,
            ));
        }
        assert_eq!(service.snapshot().text.as_ref(), source);
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some("Reverted to Dracula"),
            "once it lands (colours only: {colours_only})"
        );
    }
}

#[test]
fn a_refused_revert_says_it_did_not_revert() {
    for colours_only in [false, true] {
        let (mut service, mut runtime, instance, view) =
            typography_view("theme = \"Dracula\"\n", studio_catalog(PINNABLE_TYPE));
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
        let mut writes = vec![config_patches(&kept)[0].clone()];
        if colours_only {
            let _ = act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/keep-more",
                None,
            );
            let pinned = act(
                &mut runtime,
                instance,
                view,
                SHOWCASE_KEEP_COLOURS_KEY,
                None,
            );
            writes.push(config_patches(&pinned)[0].clone());
        }
        let reverted = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/revert",
            None,
        );
        let back = config_patches(&reverted);
        assert_eq!(back.len(), 1, "Revert is one write");
        let mut landed = None;
        for (patch, operation) in &writes {
            landed = Some(land_patch(
                &mut service,
                &mut runtime,
                instance,
                view,
                (patch, *operation),
                landed,
            ));
        }
        finish_patch(&mut runtime, instance, view, back[0].1, false);
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some("Couldn\u{2019}t revert to Dracula: no"),
            "colours only: {colours_only}"
        );
    }
}

#[test]
fn keep_colours_only_says_what_it_kept() {
    for applied in [true, false] {
        let (mut service, mut runtime, instance, view) =
            typography_view("theme = \"Dracula\"\n", studio_catalog(PINNABLE_TYPE));
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/keep-more",
            None,
        );
        let pinned = act(
            &mut runtime,
            instance,
            view,
            SHOWCASE_KEEP_COLOURS_KEY,
            None,
        );
        let (patch, operation) = config_patches(&pinned)[0].clone();
        assert!(patch.edits.len() > 2, "the theme and its type pins");
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some("Keeping Studio's colors and your type settings\u{2026}"),
            "while it saves"
        );
        if applied {
            land_patch(
                &mut service,
                &mut runtime,
                instance,
                view,
                (&patch, operation),
                None,
            );
        } else {
            finish_patch(&mut runtime, instance, view, operation, false);
        }
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some(if applied {
                "Kept Studio's colors and your type settings"
            } else {
                "Couldn\u{2019}t keep Studio's colors: no"
            }),
        );
    }
}

#[test]
fn keep_after_keep_colours_only_takes_the_pins_off() {
    let (mut service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(PINNABLE_TYPE));
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let pinned = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    let (pins, pins_op) = config_patches(&pinned)[0].clone();
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let keep = config_patches(&kept);
    assert_eq!(keep.len(), 1, "Keep over Keep colours only is a write");
    assert_eq!(write_follows(&kept), vec![vec![pins_op]]);
    let (keep, keep_op) = keep[0].clone();
    let mut pinned: Vec<(&str, Option<&str>)> = pins
        .edits
        .iter()
        .filter(|edit| edit.key != prefs::EDIT_THEME)
        .map(|edit| (edit.key.as_str(), None))
        .collect();
    pinned.sort_unstable();
    assert!(pinned.len() > 1, "{pins:?}");
    let mut unpinned: Vec<(&str, Option<&str>)> = keep
        .edits
        .iter()
        .map(|edit| (edit.key.as_str(), edit.value.as_deref()))
        .collect();
    unpinned.sort_unstable();
    assert_eq!(unpinned, pinned, "{keep:?}");
    assert!(
        config_patches(&act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/keep",
            None
        ))
        .is_empty(),
        "a third press asks nothing more"
    );
    let landed = land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&pins, pins_op),
        None,
    );
    land_patch(
        &mut service,
        &mut runtime,
        instance,
        view,
        (&keep, keep_op),
        Some(landed),
    );
    assert_eq!(
        service.snapshot().text.as_ref(),
        "theme = \"Studio\"\n",
        "the theme, and no pin"
    );
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
}

#[test]
fn save_as_takes_a_second_save_while_the_first_is_on_its_way() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let (first, _, first_op) = start_save_as(&mut runtime, instance, view);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-name",
        Some("Saved Again"),
    );
    let second = act(&mut runtime, instance, view, "settings/showcase/save", None);
    let Some((ThemesRequest::Save { id, .. }, second_op)) = themes_request(&second) else {
        panic!(
            "the second Save is a request: {:?}",
            state(&runtime, view).feedback
        );
    };
    assert_eq!(
        (first.as_str(), id.as_str()),
        ("Saved Custom", "Saved Again")
    );
    assert_ne!(first_op, second_op);
    assert_eq!(
        state(&runtime, view).feedback.as_deref(),
        Some("Saving Saved Again\u{2026}")
    );
}

#[test]
fn save_as_text_accepts_a_name_the_file_respells() {
    let (_service, runtime, _instance, view) = themes_view("theme = \"Nord\"\n");
    let state = state(&runtime, view);
    let resolved =
        preview_resolved_colors_with(state, state.presented_motion.get().system_dark, None);
    assert!(
        save_as_text(
            &resolved,
            "Nord #2",
            "Nord",
            &aterm_types::scheme::ThemeTypography::NONE
        )
        .is_ok()
    );
    assert!(
        save_as_text(
            &resolved,
            &"N".repeat(300),
            "Nord",
            &aterm_types::scheme::ThemeTypography::NONE
        )
        .is_ok()
    );
}

#[test]
fn return_in_the_filter_keeps_only_a_listed_trial() {
    for (typed, writes) in [("nord", 0), ("drac", 1)] {
        let (_service, mut runtime, instance, view) = themes_view("");
        let dracula = card_action(&runtime, view, "Dracula");
        let _ = act(&mut runtime, instance, view, &dracula, None);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/filter",
            Some(typed),
        );
        let submitted = runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Submit))
            .unwrap();
        assert_eq!(config_patches(&submitted).len(), writes, "{typed}");
    }
}

#[test]
fn palette_slot_edit_writes_one_sparse_key() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/12",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette-slot",
        Some("#123456"),
    );
    let submitted = runtime
        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Submit))
        .unwrap();
    let patches = config_patches(&submitted);
    assert_eq!(patches.len(), 1);
    let edits: Vec<(String, Option<String>)> = patches[0]
        .0
        .edits
        .iter()
        .map(|edit| (edit.key.clone(), edit.value.clone()))
        .collect();
    assert_eq!(
        edits,
        vec![(
            "palette_overrides.12".to_string(),
            Some("#123456".to_string())
        )]
    );
    let (published, _) = apply_real_config_patch(&mut service, &patches[0].0);
    assert_eq!(published.config.palette, None);
    finish_patch(&mut runtime, instance, view, patches[0].1, true);
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(published))
        .unwrap();
    let state_now = state(&runtime, view);
    let said = state_now.feedback.clone().unwrap_or_default();
    assert!(said.starts_with("Palette color 12, bright blue "), "{said}");
    assert!(said.contains("#123456"), "{said}");
    assert!(!said.contains("palette_overrides"), "{said}");
    assert!(!said.contains("Unknown key"), "{said}");
    let dark = state_now.presented_motion.get().system_dark;
    for theme in ["Nord", "Dracula"] {
        let shown = preview_resolved_colors_with(state_now, dark, Some(theme));
        let config = Config {
            theme: Some(theme.to_string()),
            ..Config::default()
        };
        let alone = config.resolved_colors(
            shown.side,
            &crate::app_config::ThemeCatalog::default(),
            None,
        );
        assert_eq!(
            shown.scheme.ansi[12],
            aterm_types::Rgb::new(0x12, 0x34, 0x56)
        );
        assert_eq!(shown.scheme.ansi[..12], alone.scheme.ansi[..12], "{theme}");
    }
    let reset = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette-slot-reset",
        None,
    );
    let edits: Vec<(String, Option<String>)> = config_patches(&reset)[0]
        .0
        .edits
        .iter()
        .map(|edit| (edit.key.clone(), edit.value.clone()))
        .collect();
    assert_eq!(edits, vec![("palette_overrides.12".to_string(), None)]);
}

#[test]
fn yours_cards_open_and_trash_their_file() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"paper\"\n");
    let _ = admit_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        vec![
            user_scheme("harbour", aterm_types::Appearance::Dark),
            user_scheme("paper", aterm_types::Appearance::Light),
        ],
    );
    let harbour = card_action(&runtime, view, "harbour");
    let opened = act(
        &mut runtime,
        instance,
        view,
        &format!("{harbour}/open"),
        None,
    );
    assert_eq!(
        themes_request(&opened).map(|(request, _)| request),
        Some(ThemesRequest::OpenFile {
            id: "harbour".to_string()
        })
    );
    let (opened, operation) = themes_request(&opened).unwrap();
    let _ = opened;
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Opened,
            },
        )
        .unwrap();
    let trash = format!("{harbour}/trash");
    assert!(themes_request(&act(&mut runtime, instance, view, &trash, None)).is_none());
    assert_eq!(
        themes_request(&act(&mut runtime, instance, view, &trash, None)).map(|(r, _)| r),
        Some(ThemesRequest::Trash {
            id: "harbour".to_string()
        })
    );
    let paper = format!("{}/trash", card_action(&runtime, view, "paper"));
    for _ in 0..2 {
        assert!(
            themes_request(&act(&mut runtime, instance, view, &paper, None)).is_none(),
            "the theme in use stays"
        );
    }
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 900.0));
    let nord = card_action(&runtime, view, "Nord");
    assert!(
        compiled
            .semantic(&UiKey::new(format!("{nord}/trash")))
            .is_none(),
        "a built-in has no file actions"
    );
}

#[test]
fn save_as_over_a_slot_override_selects_the_theme_and_clears_the_slot() {
    let (mut service, mut runtime, instance, view) =
        themes_view("[palette_overrides]\n4 = \"#4a7bd9\"\n");
    let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
    assert_eq!(hex(scheme.ansi[4]), "#4a7bd9", "the file carries the slot");
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Saved { id: id.clone() },
            },
        )
        .unwrap();
    let published = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    let patches = config_patches(&published);
    assert_eq!(patches.len(), 1, "one select patch");
    let (snapshot, _) = apply_real_config_patch(&mut service, &patches[0].0);
    assert_eq!(snapshot.config.theme.as_deref(), Some(id.as_str()));
    assert!(
        snapshot
            .config
            .palette_overrides
            .as_ref()
            .is_none_or(|table| table.is_empty()),
        "{:?}",
        snapshot.config.palette_overrides
    );
    finish_patch(&mut runtime, instance, view, patches[0].1, true);
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
        .unwrap();
    let said = state(&runtime, view).feedback.clone().unwrap_or_default();
    assert!(
        said.contains(&format!("Palette colors now come from {id}")),
        "{said}"
    );
    assert!(!said.contains("its default"), "{said}");
}

#[test]
fn save_as_over_a_one_theme_pair_selects_by_keeps_rule() {
    let select = |segment: Option<&str>| {
        let (mut service, mut runtime, instance, view) =
            themes_view("theme = \"dark:Hearth,light:Hearth\"\n");
        if let Some(segment) = segment {
            let _ = act(&mut runtime, instance, view, segment, None);
        }
        let (id, scheme, operation) = start_save_as(&mut runtime, instance, view);
        assert_eq!(scheme.appearance, aterm_types::Appearance::Dark);
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::ThemesFinished {
                    operation,
                    outcome: ThemesOutcome::Saved { id: id.clone() },
                },
            )
            .unwrap();
        let published =
            publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
        let patches = config_patches(&published);
        assert_eq!(patches.len(), 1, "one select patch");
        let (snapshot, _) = apply_real_config_patch(&mut service, &patches[0].0);
        finish_patch(&mut runtime, instance, view, patches[0].1, true);
        runtime
            .dispatch(instance, view, AppEvent::ConfigChanged(snapshot.clone()))
            .unwrap();
        let highlighted = state(&runtime, view).highlighted_slug();
        let saved = state(&runtime, view)
            .showcase
            .card_by_slug(highlighted.as_deref().unwrap_or_default())
            .map(|card| card.id.clone());
        (id, snapshot.config.theme.clone(), saved)
    };
    let (id, theme, highlighted) = select(None);
    assert_eq!(theme, Some(format!("dark:{id},light:Hearth")));
    assert_eq!(
        highlighted.as_deref(),
        Some(id.as_str()),
        "the page shows it"
    );
    let (id, theme, highlighted) = select(Some("settings/showcase/side/light"));
    assert_eq!(theme, Some(format!("dark:Hearth,light:{id}")));
    assert_eq!(highlighted.as_deref(), Some(id.as_str()));
}

fn hex(color: aterm_types::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn stage_spec(
    compiled: &crate::native_ui::CompiledUi,
) -> Option<crate::settings_preview::SettingsPreviewSpec> {
    compiled.paint.iter().find_map(|node| match &node.content {
        UiContent::SettingsPreview(spec) => Some((**spec).clone()),
        _ => None,
    })
}

#[test]
fn a_pair_preview_names_the_side_it_draws() {
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Nord,light:Default Light\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/route/appearance",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 900.0));
    let spec = stage_spec(&compiled).expect("the Appearance preview");
    assert_eq!(spec.focused_key, prefs::EDIT_THEME);
    assert!(
        ["Nord", "Default Light"].contains(&spec.focused_value.as_str()),
        "{}",
        spec.focused_value
    );
}

fn add_pack_request(outcome: &crate::native_app::DispatchOutcome) -> Vec<(String, OperationId)> {
    outcome
        .effects
        .iter()
        .filter_map(|effect| match effect {
            AppEffect::Themes {
                request: ThemesRequest::AddPack { id },
                reply,
            } => Some((id.clone(), reply.operation)),
            _ => None,
        })
        .collect()
}

fn studio_catalog(extra: &str) -> std::sync::Arc<crate::app_config::ThemeCatalog> {
    let text = format!("foreground = #e0e0e0\nbackground = #101418\n{extra}");
    let parsed = aterm_types::scheme::parse_theme_str(&text).expect("a theme");
    crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
}

const PINNABLE_TYPE: &str = "font_family = Berkeley Mono\nline_height = 1.15\n";

const STUDIO_TYPE: &str = "font_family = Berkeley Mono\nline_height = 1.15\nfont_weight = 450\n";

fn typography_view(
    source: &str,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
) -> (
    VersionedConfigService,
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
) {
    let mut service = VersionedConfigService::new(source.to_string()).expect("valid fixture");
    let snapshot = service.replace_theme_catalog(themes);
    let (mut runtime, instance, view) = setup();
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    **state = SettingsViewState::from_snapshot(&snapshot).expect("snapshot");
    state.navigate(SettingsRoute::Themes);
    (service, runtime, instance, view)
}

fn enabled(compiled: &crate::native_ui::CompiledUi, key: &str) -> Option<bool> {
    compiled
        .semantic(&UiKey::new(key))
        .map(|node| node.state.is_none_or(|state| state.enabled))
}

const SHOWCASE_KEEP_COLOURS_KEY: &str = "settings/showcase/keep-colours";

#[test]
fn font_rows_name_the_themes_values() {
    let (_service, runtime, _instance, view) = typography_view(
        "theme = \"Studio\"\nline_height = 1.4\n",
        studio_catalog(STUDIO_TYPE),
    );
    let view_state = state(&runtime, view);
    let family = view_state
        .field_by_key(prefs::EDIT_FONT_FAMILY)
        .expect("family row");
    assert_eq!(family.placeholder, "Berkeley Mono (from theme Studio)");
    let picker = font_choice_picker(view_state, prefs::EDIT_FONT_FAMILY);
    assert_eq!(picker.options[0].label, "Use theme's (Berkeley Mono)");
    let line = view_state
        .field_by_key(prefs::EDIT_LINE_HEIGHT)
        .expect("line height row");
    assert_eq!(line.seed.as_deref(), Some("1.4"), "the config wins");
    assert_eq!(
        view_state.theme_fallback_for(prefs::EDIT_LINE_HEIGHT),
        Some(("1.15".to_string(), "Studio".to_string()))
    );
    let (_service, runtime, _instance, view) =
        typography_view("line_height = 1.4\n", studio_catalog(STUDIO_TYPE));
    let view_state = state(&runtime, view);
    assert_eq!(view_state.theme_fallback_for(prefs::EDIT_LINE_HEIGHT), None);
    assert_eq!(
        view_state
            .field_by_key(prefs::EDIT_FONT_FAMILY)
            .expect("family row")
            .placeholder,
        "Automatic"
    );
}

#[test]
fn a_reset_to_the_themes_type_is_named_by_its_words() {
    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Studio\"\nline_height = 1.4\nfont_family = \"\"\n",
        studio_catalog(STUDIO_TYPE),
    );
    let Some(AppViewState::Settings(page)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    page.navigate(SettingsRoute::TextFonts);
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    for (key, value) in [
        (prefs::EDIT_LINE_HEIGHT, "1.15"),
        (prefs::EDIT_FONT_FAMILY, "Berkeley Mono"),
    ] {
        let reset = compiled
            .semantic(&UiKey::new(format!("settings/reset/{key}")))
            .unwrap_or_else(|| panic!("{key}'s Reset is drawn"));
        let name = reset.label.as_str();
        assert!(
            crate::native_ui::visible_label_in_name("Use theme's", name),
            "{key}: {name:?}"
        );
        assert!(
            name.contains(value) && name.contains("Studio"),
            "{key}: {name:?}"
        );
    }
}

#[test]
fn a_reset_to_the_themes_type_says_the_themes_value() {
    for (source, words) in [
        (
            "theme = \"Studio\"\nline_height = 1.4\n",
            "Line height reset to Studio\u{2019}s 1.15\u{d7}",
        ),
        ("line_height = 1.4\n", "Line height reset to 1.0\u{d7}"),
    ] {
        let (mut service, mut runtime, instance, view) =
            typography_view(source, studio_catalog(PINNABLE_TYPE));
        let Some(AppViewState::Settings(page)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        page.navigate(SettingsRoute::TextFonts);
        let reset = format!("settings/reset/{}", prefs::EDIT_LINE_HEIGHT);
        let pressed = act(&mut runtime, instance, view, &reset, None);
        let (patch, operation) = config_patches(&pressed)[0].clone();
        land_patch(
            &mut service,
            &mut runtime,
            instance,
            view,
            (&patch, operation),
            None,
        );
        assert_eq!(
            state(&runtime, view).feedback.as_deref(),
            Some(words),
            "{source:?}"
        );
    }
}

#[test]
fn save_as_theme_includes_type_settings_on_request() {
    let resolved = Config::default().resolved_colors(
        aterm_types::Appearance::Dark,
        &crate::app_config::ThemeCatalog::empty(),
        None,
    );
    let typography = aterm_types::scheme::parse_theme_str(
        "fg = #ffffff\nbg = #000000\nfont_family = Berkeley Mono\nline_height = 1.15\n",
    )
    .expect("theme")
    .typography;
    let text = save_as_text(&resolved, "Mine", "Default", &typography).expect("saves");
    let parsed = aterm_types::scheme::parse_theme_str(&text).expect("reads back");
    assert_eq!(parsed.typography, typography);
    assert!(
        carried_override_keys(&resolved, &typography)
            .iter()
            .any(|key| key == prefs::EDIT_LINE_HEIGHT)
    );
    let plain = save_as_text(
        &resolved,
        "Mine",
        "Default",
        &aterm_types::scheme::ThemeTypography::NONE,
    )
    .expect("saves");
    assert!(
        aterm_types::scheme::parse_theme_str(&plain)
            .expect("reads back")
            .typography
            .is_empty()
    );
}

#[test]
fn showcase_refused_keep_returns_to_the_trial() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let (_, operation) = config_patches(&kept)[0].clone();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Rejected {
                    message: "no".to_string(),
                },
            },
        )
        .unwrap();
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Show(
            "Nord".to_string()
        ))
    );
    assert!(state(&runtime, view).showcase.error.is_some());
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    let status = compiled
        .semantic(&UiKey::new("settings/showcase/status"))
        .expect("the strip");
    assert!(status.label.starts_with("Nord: "), "{}", status.label);
    assert_eq!(
        enabled(&compiled, "settings/showcase/keep"),
        Some(true),
        "Keep again"
    );
}

#[test]
fn showcase_page_compiles_its_rows_into_the_semantic_tree() {
    let (_service, mut runtime, instance, view) = themes_view("");
    for (width, height) in [(1_284.0, 714.0), (934.0, 714.0), (556.0, 900.0)] {
        let cx = view_cx_at(width, height);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let list = compiled
            .semantic(&UiKey::new("settings/showcase/list"))
            .expect("the list");
        assert_eq!(list.role, SemanticRole::ListBox, "{width}x{height}");
        let rows: Vec<_> = compiled
            .semantics
            .iter()
            .filter(|node| node.role == SemanticRole::ListBoxOption)
            .collect();
        assert!(!rows.is_empty(), "{width}x{height}: rows");
        for row in &rows {
            assert!(row.key.as_str().starts_with("settings/showcase/card/"));
            let SemanticValue::Text(value) = &row.value else {
                panic!("{}: {:?}", row.label, row.value)
            };
            assert!(
                value.starts_with("dark, contrast ") || value.starts_with("light, contrast "),
                "{value}"
            );
            assert!(!row.label.contains(", "), "the name alone: {}", row.label);
        }
        assert!(
            rows.iter()
                .filter(|row| row.state.is_some_and(|state| state.selected))
                .count()
                == 1,
            "{width}x{height}: one highlighted row"
        );
        let footer = (width >= 640.0).then_some("settings/showcase/follow-system");
        for key in [
            "settings/showcase/filter",
            "settings/showcase/status",
            "settings/showcase/stage",
            "settings/showcase/customize",
        ]
        .into_iter()
        .chain(footer)
        {
            assert!(
                compiled.semantic(&UiKey::new(key)).is_some(),
                "{width}x{height}: {key}"
            );
        }
        assert!(
            compiled.fit_failures().is_empty(),
            "{width}x{height}: {:?}",
            compiled.fit_failures()
        );
    }
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize",
        None,
    );
    let cx = view_cx_at(1_284.0, 1_400.0);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    for key in [
        format!("settings/control/{}", prefs::EDIT_FOREGROUND),
        "settings/showcase/palette/0".to_string(),
        "settings/showcase/save-as".to_string(),
    ] {
        assert!(
            compiled.semantic(&UiKey::new(key.as_str())).is_some(),
            "{key}"
        );
    }
    assert!(
        compiled.fit_failures().is_empty(),
        "{:?}",
        compiled.fit_failures()
    );
}

#[test]
fn customize_editors_open_on_the_page_shown() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/4",
        None,
    );
    let mut drawn = false;
    for height in [714.0, 1_000.0] {
        let compiled =
            compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, height));
        let swatch = compiled
            .semantic(&UiKey::new("settings/showcase/palette/4"))
            .is_some();
        let editor = compiled
            .semantic(&UiKey::new("settings/showcase/palette-slot"))
            .is_some();
        assert_eq!(swatch, editor, "{height}");
        drawn |= editor;
    }
    assert!(drawn, "the editor is drawn");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette-slot-close",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/save-name"))
            .is_some()
    );
}

#[test]
fn a_yours_card_keeps_its_name_beside_its_file_actions() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Harbor Dusk\"\n");
    let _ = admit_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        vec![user_scheme("Harbor Dusk", aterm_types::Appearance::Dark)],
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("harbor"),
    );
    let card = card_action(&runtime, view, "Harbor Dusk");
    for width in [556.0, 1_284.0] {
        let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(width, 714.0));
        let name = compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Text(text) if node.key.as_str() == format!("{card}/name") => {
                    Some(text.text.clone())
                }
                _ => None,
            })
            .expect("the row's name is painted");
        assert_eq!(name, "Harbor Dusk", "{width}");
        let rect = |key: &str| {
            compiled
                .semantic(&UiKey::new(key))
                .unwrap_or_else(|| panic!("{key}"))
                .rect
        };
        let frame = rect(&card);
        let open = rect(&format!("{card}/open"));
        assert!(open.y >= frame.y - 0.01 && open.bottom() <= frame.bottom() + 0.01);
        assert!(open.right() <= frame.right() + 0.01);
        assert!(
            compiled.fit_failures().is_empty(),
            "{:?}",
            compiled.fit_failures()
        );
    }
}

#[test]
fn list_arrows_walk_rows_and_the_list_follows() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let cx = view_cx_at(1_074.0, 684.0);
    let first_frame = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    assert!(
        order.len()
            > first_frame
                .semantics
                .iter()
                .filter(|node| node.role == SemanticRole::ListBoxOption && !node.offscreen)
                .count(),
        "the control: the list scrolls at this size"
    );
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.last_focus = Some(UiKey::new(format!("settings/showcase/card/{}", order[0])));
    }
    let focused = |runtime: &NativeRuntime| {
        state(runtime, view)
            .common
            .last_focus
            .as_ref()
            .and_then(|key| key.as_str().strip_prefix("settings/showcase/card/"))
            .map(str::to_string)
    };
    for (step, slug) in order.iter().enumerate().skip(1) {
        runtime
            .dispatch(instance, view, AppEvent::StepFocus(1))
            .unwrap();
        assert_eq!(
            focused(&runtime).as_deref(),
            Some(slug.as_str()),
            "↓ {step}"
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/showcase/card/{slug}")))
                .is_some_and(|node| !node.offscreen),
            "row {step}: the focused row is on screen"
        );
    }
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    assert_eq!(
        focused(&runtime).as_deref(),
        order.last().map(String::as_str),
        "the last row holds"
    );
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(-1))
        .unwrap();
    assert_eq!(
        focused(&runtime).as_deref(),
        Some(order[order.len() - 2].as_str()),
        "↑"
    );
}

#[test]
fn showcase_escape_unwinds_slot_sheet_filter_trial_then_stays() {
    let escape = |runtime: &mut NativeRuntime,
                  instance: crate::native_app::AppInstanceId,
                  view: crate::native_app::ViewId| {
        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
    };
    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/4",
        None,
    );
    assert!(state(&runtime, view).showcase.slot.is_some());
    escape(&mut runtime, instance, view);
    assert!(state(&runtime, view).showcase.slot.is_none());
    assert!(state(&runtime, view).showcase.sheet_open);
    escape(&mut runtime, instance, view);
    assert!(!state(&runtime, view).showcase.sheet_open);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Themes);

    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("nord"),
    );
    let _ = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    assert!(matches!(
        state(&runtime, view).showcase.trial,
        ShowcaseTrial::Trying { .. }
    ));
    escape(&mut runtime, instance, view);
    assert!(state(&runtime, view).showcase.filter.value().is_empty());
    assert!(matches!(
        state(&runtime, view).showcase.trial,
        ShowcaseTrial::Trying { .. }
    ));
    escape(&mut runtime, instance, view);
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Themes);
    escape(&mut runtime, instance, view);
    escape(&mut runtime, instance, view);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Themes);
}

fn listed_ids(showcase: &ShowcaseState) -> BTreeSet<String> {
    showcase
        .visible()
        .into_iter()
        .map(|index| showcase.cards[index].id.clone())
        .collect()
}

#[test]
fn an_earlier_releases_pack_copy_has_no_community_twin() {
    let renamed = aterm_theme_pack::themes()
        .iter()
        .find(|theme| theme.name != theme.id)
        .expect("premise: a theme shown under another name");
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    for earlier in [false, true] {
        if earlier {
            let text = renamed.text.replacen(
                &format!("\nname = {}\n", renamed.name),
                &format!("\nname = {}\n", renamed.id),
                1,
            );
            assert_ne!(text, renamed.text, "premise: the name line differs");
            std::fs::write(dir.path().join(format!("{}.conf", renamed.id)), text).unwrap();
        }
        let mut showcase =
            ShowcaseState::new(&crate::app_config::ThemeCatalog::discover_in(dir.path()));
        showcase.pack_open = true;
        let rows: Vec<(Section, String)> = showcase
            .visible()
            .into_iter()
            .map(|index| &showcase.cards[index])
            .filter(|card| {
                card.id.eq_ignore_ascii_case(renamed.id)
                    || card.label.eq_ignore_ascii_case(renamed.name)
            })
            .map(|card| (card.section, card.label.clone()))
            .collect();
        let want = if earlier {
            (Section::Yours, renamed.id.to_string())
        } else {
            (Section::Community, renamed.name.to_string())
        };
        assert_eq!(rows, vec![want], "earlier copy: {earlier}");
    }
}

#[test]
fn community_lists_only_the_pack_themes_the_folder_lacks() {
    let pack: BTreeSet<String> = aterm_theme_pack::themes()
        .iter()
        .map(|theme| theme.id.to_string())
        .collect();
    let mut bare = ShowcaseState::new(&crate::app_config::ThemeCatalog::from_schemes([]));
    let community = |showcase: &ShowcaseState| -> BTreeSet<String> {
        showcase
            .visible()
            .into_iter()
            .filter(|&index| showcase.cards[index].section == Section::Community)
            .map(|index| showcase.cards[index].id.clone())
            .collect()
    };
    assert!(community(&bare).is_empty(), "closed, it lists none");
    bare.pack_open = true;
    assert_eq!(community(&bare), pack);

    let themes = crate::app_config::ThemeCatalog::from_schemes([user_scheme(
        "Kanagawa Wave",
        aterm_types::Appearance::Dark,
    )]);
    let mut installed = ShowcaseState::new(&themes);
    installed.pack_open = true;
    let more = community(&installed);
    assert!(!more.contains("Kanagawa Wave"));
    assert_eq!(more.len(), pack.len() - 1);
    assert!(listed_ids(&installed).contains("Kanagawa Wave"));
    assert_eq!(
        installed
            .cards
            .iter()
            .filter(|card| card.id == "Kanagawa Wave")
            .count(),
        1
    );
    assert!(
        installed
            .cards
            .iter()
            .any(|card| card.id == "Kanagawa Wave" && card.section == Section::Yours)
    );
}

#[test]
fn a_typography_card_says_what_type_it_sets() {
    let cards = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(STUDIO_TYPE));
    let studio = cards
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert!(studio.typography.is_some() && studio.typography_spoken.is_some());
    assert!(
        studio.matches("berkeley", MatchReach::Words) && studio.matches("font", MatchReach::Words)
    );
    let plain = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(""));
    let plain = plain
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert!(plain.typography.is_none() && plain.typography_spoken.is_none());
    assert!(!plain.matches("berkeley", MatchReach::Words));
    assert!(!plain.matches("font", MatchReach::Words));
    let spoken = studio.typography_spoken.clone().unwrap_or_default();
    let value = studio.row_value(false, false);
    assert!(value.ends_with(&format!(", {spoken}")), "{value}");
    assert_eq!(
        studio.row_value(false, false).len(),
        plain.row_value(false, false).len() + spoken.len() + 2,
        "the plain row's value is the same less the type"
    );
}

#[test]
fn a_tried_display_family_is_the_demos_display_layer() {
    let tried = |source: &str, theme_type: &str| {
        let (_service, mut runtime, instance, view) =
            typography_view(source, studio_catalog(theme_type));
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        demo_font_candidate(state(&runtime, view))
    };
    let layered = tried(
        "theme = \"Dracula\"\n",
        "font_family = display:pixel+engraved\n",
    );
    assert_eq!(layered.regular, None);
    assert_eq!(layered.display, ["pixel", "engraved"]);
    let own = tried(
        "theme = \"Dracula\"\ndisplay_font = \"chunky\"\n",
        "font_family = display:pixel\n",
    );
    assert_eq!(own.regular, None);
    assert_eq!(own.display, ["chunky"], "the config's display_font wins");
    let font = tried("theme = \"Dracula\"\n", "font_family = Berkeley Mono\n");
    assert_eq!(font.regular.as_deref(), Some("Berkeley Mono"));
    assert!(font.display.is_empty());
}

#[test]
fn trying_a_typography_theme_offers_keep_colours_only() {
    let studio_type = "font_family = Berkeley Mono\nline_height = 1.15\nligatures = false\n";
    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(studio_type));
    let cx = view_cx_at(1_284.0, 714.0);
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let trial = state(&runtime, view).trial_typography("Studio");
    let changes: Vec<(&str, &str)> = trial
        .changes
        .iter()
        .map(|change| (change.key, change.theme.as_str()))
        .collect();
    let line = if cfg!(windows) { "1.1" } else { "1.0" };
    assert_eq!(
        changes,
        vec![
            (prefs::EDIT_FONT_FAMILY, "Berkeley Mono"),
            (prefs::EDIT_LINE_HEIGHT, "1.15"),
            (prefs::EDIT_LIGATURES, "false"),
        ]
    );
    assert!(trial.keeps_colours_only());
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), Some(true));

    let kept = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    let patches = config_patches(&kept);
    assert_eq!(patches.len(), 1, "one write");
    let edits: BTreeMap<String, Option<String>> = patches[0]
        .0
        .edits
        .iter()
        .map(|edit| (edit.key.clone(), edit.value.clone()))
        .collect();
    assert_eq!(
        edits,
        BTreeMap::from([
            (prefs::EDIT_THEME.to_string(), Some("Studio".to_string())),
            (prefs::EDIT_FONT_FAMILY.to_string(), Some(String::new())),
            (prefs::EDIT_LINE_HEIGHT.to_string(), Some(line.to_string())),
            (prefs::EDIT_LIGATURES.to_string(), Some("true".to_string())),
        ]),
    );
    let pinned: Config = aterm_toml::from_str(&format!(
        "theme = \"Studio\"\nfont_family = \"\"\nline_height = {line}\nligatures = true\n",
    ))
    .expect("toml");
    let typo = pinned.resolved_typography_with_flag(
        aterm_types::Appearance::Dark,
        &studio_catalog(studio_type),
        None,
    );
    assert!(typo.applied.keys().is_empty(), "{:?}", typo.applied.keys());

    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(""));
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    assert!(
        state(&runtime, view)
            .trial_typography("Studio")
            .changes
            .is_empty()
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, "settings/showcase/keep-more"), None);
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), None);
}

#[test]
fn trying_away_from_a_typography_theme_shows_and_offers_its_type_change() {
    let cx = view_cx_at(1_284.0, 714.0);
    let line_of = |runtime: &NativeRuntime, view| {
        showcase_page::demo_spec(
            state(runtime, view),
            0,
            crate::native_app::ViewMotionCx::default(),
            14.0,
            1.0,
            None,
        )
        .expect("the demo")
        .line_height
    };
    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Studio\"\n",
        studio_catalog("line_height = 1.4\n"),
    );
    assert!((line_of(&runtime, view) - 1.4).abs() < 1e-6, "Studio's own");
    let dracula = card_action(&runtime, view, "Dracula");
    let _ = act(&mut runtime, instance, view, &dracula, None);
    let trial = state(&runtime, view).trial_typography("Dracula");
    let line = aterm_types::scheme_typography::milli_text(
        (crate::app_config::DEFAULT_LINE_HEIGHT * 1000.0).round() as u16,
    );
    assert_eq!(
        trial.changes,
        vec![showcase_page::TypeChange {
            key: prefs::EDIT_LINE_HEIGHT,
            theme: line,
            pin: Some("1.4".to_string()),
        }]
    );
    assert!(trial.keeps_colours_only());
    assert!(
        (line_of(&runtime, view) - crate::app_config::DEFAULT_LINE_HEIGHT).abs() < 1e-6,
        "the demo draws the line height Keep leaves"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), Some(true));
    let kept = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    let patches = config_patches(&kept);
    assert_eq!(patches.len(), 1, "one write");
    assert!(patches[0].0.edits.iter().any(|edit| {
        edit.key == prefs::EDIT_LINE_HEIGHT && edit.value.as_deref() == Some("1.4")
    }));

    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Studio\"\n", studio_catalog(""));
    let dracula = card_action(&runtime, view, "Dracula");
    let _ = act(&mut runtime, instance, view, &dracula, None);
    let trial = state(&runtime, view).trial_typography("Dracula");
    assert!(trial.changes.is_empty());
    assert!(!trial.keeps_colours_only());
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, "settings/showcase/keep-more"), None);
}

#[test]
fn keep_colours_only_is_withheld_when_a_change_cannot_be_pinned() {
    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(STUDIO_TYPE));
    let cx = view_cx_at(1_284.0, 714.0);
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let trial = state(&runtime, view).trial_typography("Studio");
    assert!(
        trial
            .changes
            .iter()
            .any(|change| change.key == prefs::EDIT_FONT_WEIGHT && change.pin.is_none())
    );
    assert!(!trial.keeps_colours_only());
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, "settings/showcase/keep-more"), None);
    assert_eq!(enabled(&compiled, "settings/showcase/keep"), Some(true));
    let pressed = act(
        &mut runtime,
        instance,
        view,
        SHOWCASE_KEEP_COLOURS_KEY,
        None,
    );
    assert!(config_patches(&pressed).is_empty(), "nothing is written");
    assert!(matches!(
        state(&runtime, view).showcase.trial,
        ShowcaseTrial::Trying { .. }
    ));

    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Dracula\"\n",
        studio_catalog("font_family = Berkeley Mono\n"),
    );
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(
        enabled(&compiled, "settings/showcase/keep-more"),
        Some(true)
    );
}

#[test]
fn save_as_theme_labels_its_type_switch() {
    let cx = view_cx_at(1_284.0, 714.0);
    let painted = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled.paint.iter().find_map(|node| match &node.content {
            UiContent::Text(text) if node.key.as_str() == key => Some(text.text.clone()),
            _ => None,
        })
    };
    let description = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .semantic(&UiKey::new("settings/showcase/save"))
            .and_then(|node| node.description.clone())
            .expect("Save says what it writes")
    };
    let (_service, mut runtime, instance, view) = themes_view("line_height = 1.4\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        painted(&compiled, "settings/showcase/save-type-label")
            .is_some_and(|label| !label.trim().is_empty()),
        "the switch paints its words"
    );
    let off = description(&compiled);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-type",
        None,
    );
    assert!(
        state(&runtime, view)
            .showcase
            .save
            .as_ref()
            .is_some_and(|save| save.include_type)
    );
    let on = description(&compile_settings_view(&runtime, instance, view, &cx));
    assert_ne!(on, off, "the description follows the switch");

    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    assert!(!state(&runtime, view).save_type_in_force());
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/save-type"))
            .is_none()
    );
}

const ROW: &str = "settings/showcase/card/";
const STATUS: &str = "settings/showcase/status";
const STAGE: &str = "settings/showcase/stage";
const SHAPES: [f32; 3] = [556.0, 934.0, 1_284.0];

fn focus_on(runtime: &mut NativeRuntime, view: crate::native_app::ViewId, key: &str) {
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    state.common.last_focus = Some(UiKey::new(key));
    state.common.focus_visible = true;
}

fn list_at_top(runtime: &mut NativeRuntime, view: crate::native_app::ViewId) {
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    state.scroll_page_to(0);
    state.page_reveal = None;
    state.scroll_record.replace(None);
}

fn intent(
    runtime: &NativeRuntime,
    view: crate::native_app::ViewId,
) -> Option<crate::control_theme::ThemeTrialIntent> {
    state(runtime, view).theme_trial_intent()
}

fn show(id: &str) -> Option<crate::control_theme::ThemeTrialIntent> {
    Some(crate::control_theme::ThemeTrialIntent::Show(id.to_string()))
}

fn id_of(runtime: &NativeRuntime, view: crate::native_app::ViewId, slug: &str) -> String {
    state(runtime, view)
        .showcase
        .cards
        .iter()
        .find(|card| card.slug == slug)
        .unwrap_or_else(|| panic!("{slug} is a row"))
        .id
        .clone()
}

fn rect_of(compiled: &crate::native_ui::CompiledUi, key: &str) -> LogicalRect {
    compiled
        .semantic(&UiKey::new(key))
        .unwrap_or_else(|| panic!("{key} is in the tree"))
        .rect
}

fn whole_in_list(compiled: &crate::native_ui::CompiledUi, key: &str) -> bool {
    let list = rect_of(compiled, "settings/showcase/list");
    compiled.semantic(&UiKey::new(key)).is_some_and(|row| {
        !row.offscreen && row.rect.y >= list.y - 0.5 && row.rect.bottom() <= list.bottom() + 0.5
    })
}

fn drawn_rows(compiled: &crate::native_ui::CompiledUi) -> Vec<String> {
    compiled
        .semantics
        .iter()
        .filter(|node| node.role == SemanticRole::ListBoxOption)
        .map(|node| node.key.as_str().to_string())
        .collect()
}

fn dispatch(
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    event: AppEvent,
) -> crate::native_app::DispatchOutcome {
    runtime.dispatch(instance, view, event).unwrap()
}

fn writes(outcome: &crate::native_app::DispatchOutcome) -> bool {
    outcome.effects.iter().any(|effect| {
        matches!(
            effect,
            AppEffect::ConfigPatch { .. } | AppEffect::Themes { .. }
        )
    })
}

#[test]
fn highlight_is_the_trial() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let cx = view_cx_at(1_284.0, 714.0);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    let home = state(&runtime, view)
        .highlighted_slug()
        .expect("the in-use row");
    assert_eq!(id_of(&runtime, view, &home), "Nord");
    let at = order.iter().position(|slug| *slug == home).expect("listed");
    let next = order[at + 1].clone();
    focus_on(&mut runtime, view, &format!("{ROW}{home}"));
    assert_eq!(intent(&runtime, view), None, "opening tries nothing");

    let _ = dispatch(&mut runtime, instance, view, AppEvent::StepFocus(1));
    let next_id = id_of(&runtime, view, &next);
    assert_eq!(intent(&runtime, view), show(&next_id));
    assert_eq!(
        state(&runtime, view).highlighted_slug().as_deref(),
        Some(next.as_str())
    );
    assert_eq!(
        state(&runtime, view).common.last_focus,
        Some(UiKey::new(format!("{ROW}{next}"))),
        "the keyboard goes with the highlight"
    );
    let _ = dispatch(&mut runtime, instance, view, AppEvent::StepFocus(-1));
    assert_eq!(intent(&runtime, view), None, "back on the theme in use");

    let _ = dispatch(&mut runtime, instance, view, AppEvent::StepFocus(1));
    let kept = act(&mut runtime, instance, view, &format!("{ROW}{next}"), None);
    assert_eq!(config_patches(&kept).len(), 1, "Return keeps: one write");
    assert_eq!(
        intent(&runtime, view),
        Some(crate::control_theme::ThemeTrialIntent::Keeping(next_id))
    );

    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("no"),
    );
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let caret = |runtime: &NativeRuntime| state(runtime, view).showcase.filter.selection().range();
    let before = caret(&runtime);
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::TextInput(TextInputEvent::Left { extend: false }),
    );
    assert_ne!(caret(&runtime), before, "← moved the caret");
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::TextInput(TextInputEvent::Right { extend: false }),
    );
    assert_eq!(caret(&runtime), before, "→ moved it back");
    assert_eq!(intent(&runtime, view), None);
}

#[test]
fn keys_on_the_community_disclosure_move_the_highlight() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let cx = view_cx_at(1_284.0, 714.0);
    let _ = act(&mut runtime, instance, view, "settings/showcase/pack", None);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    let first = order
        .iter()
        .position(|slug| {
            state(&runtime, view)
                .showcase
                .card_by_slug(slug)
                .is_some_and(|card| card.section == Section::Community)
        })
        .expect("the open disclosure lists community rows");
    assert!(first > 0);
    for (event, at) in [
        (AppEvent::StepFocus(1), first),
        (AppEvent::StepFocus(-1), first - 1),
        (
            AppEvent::TextInput(TextInputEvent::End { extend: false }),
            order.len() - 1,
        ),
    ] {
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/revert",
            None,
        );
        focus_on(&mut runtime, view, "settings/showcase/pack");
        let _ = dispatch(&mut runtime, instance, view, event);
        let slug = &order[at];
        assert_eq!(
            state(&runtime, view).common.last_focus,
            Some(UiKey::new(format!("{ROW}{slug}"))),
            "the keyboard goes to {slug}"
        );
        assert_eq!(intent(&runtime, view), show(&id_of(&runtime, view, slug)));
    }
}

#[test]
fn hover_never_tries() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    let rows = drawn_rows(&compiled);
    assert!(rows.len() >= 5, "{rows:?}");
    for row in &rows[..5] {
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::FocusChanged(Some(UiKey::new(row.as_str()))),
        );
        assert_eq!(intent(&runtime, view), None, "{row}");
    }
    let last = rows[..5]
        .iter()
        .rev()
        .find(|row| row.strip_prefix(ROW) != state(&runtime, view).highlighted_slug().as_deref())
        .expect("a row not in use")
        .clone();
    let _ = act(&mut runtime, instance, view, &last, None);
    assert_eq!(
        intent(&runtime, view),
        show(&id_of(&runtime, view, last.strip_prefix(ROW).unwrap()))
    );
}

#[test]
fn trying_a_pack_theme_writes_no_file() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let card = card_action(&runtime, view, "Iceberg Light");
    let tried = act(&mut runtime, instance, view, &card, None);
    assert!(!writes(&tried), "trying writes nothing");
    assert_eq!(intent(&runtime, view), show("Iceberg Light"));
    let reverted = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    assert!(!writes(&reverted), "reverting writes nothing");
    assert_eq!(intent(&runtime, view), None);

    let _ = act(&mut runtime, instance, view, &card, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let added = add_pack_request(&kept);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].0, "Iceberg Light");
    assert!(config_patches(&kept).is_empty(), "no theme before its file");
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ThemesFinished {
            operation: added[0].1,
            outcome: ThemesOutcome::Added {
                id: "Iceberg Light".to_string(),
            },
        },
    );
    let text = aterm_theme_pack::find("Iceberg Light").unwrap().text;
    let scheme = aterm_types::scheme::parse_theme_str(text).unwrap().scheme;
    let listed = publish_saved_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        "Iceberg Light",
        scheme,
    );
    let patches = config_patches(&listed);
    assert_eq!(patches.len(), 1, "one theme write");
    assert_eq!(patches[0].0.edits[0].key, prefs::EDIT_THEME);
    assert_eq!(
        patches[0].0.edits[0].value.as_deref(),
        Some("Iceberg Light")
    );
}

#[test]
fn a_failed_pack_add_keeps_the_trial() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let card = card_action(&runtime, view, "Iceberg Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let operation = add_pack_request(&kept)[0].1;
    let message = "the themes folder is read-only";
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ThemesFinished {
            operation,
            outcome: ThemesOutcome::Failed {
                message: message.to_string(),
            },
        },
    );
    assert_eq!(intent(&runtime, view), show("Iceberg Light"));
    assert!(state(&runtime, view).showcase.pending_keep.is_none());
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    let status = compiled.semantic(&UiKey::new(STATUS)).expect("the strip");
    assert!(
        status.label.to_lowercase().contains(message),
        "{}",
        status.label
    );
    let detail = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            UiContent::Text(text) if node.key.as_str() == "settings/showcase/status/detail" => {
                Some((text.text.clone(), text.style))
            }
            _ => None,
        })
        .expect("line 2");
    assert!(detail.0.contains("read-only"), "{}", detail.0);
    assert_eq!(detail.1, StyleRef::Danger);

    let text = aterm_theme_pack::find("Iceberg Light").unwrap().text;
    let scheme = aterm_types::scheme::parse_theme_str(text).unwrap().scheme;
    let listed = publish_saved_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        "Iceberg Light",
        scheme,
    );
    assert!(config_patches(&listed).is_empty());
}

#[test]
fn match_system_keep_writes_only_the_chosen_side() {
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Nord,light:Solarized Light\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/side/light"))
            .is_some(),
        "the segment shows while following"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/side/light",
        None,
    );
    let card = card_action(&runtime, view, "Gruvbox Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let patches = config_patches(&kept);
    assert_eq!(patches.len(), 1);
    assert_eq!(
        patches[0].0.edits[0].value.as_deref(),
        Some("dark:Nord,light:Gruvbox Light")
    );

    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/side/light"))
            .is_none()
    );
    let card = card_action(&runtime, view, "Gruvbox Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    assert_eq!(
        config_patches(&kept)[0].0.edits[0].value.as_deref(),
        Some("Gruvbox Light")
    );
}

#[test]
fn keep_files_a_theme_by_the_side_it_is_made_for() {
    let mut cx = view_cx_at(1_284.0, 714.0);
    cx.motion.system_dark = true;
    let open = || {
        let (service, mut runtime, instance, view) =
            themes_view("theme = \"dark:Nord,light:Solarized Light\"\n");
        let Some(AppViewState::Settings(shown)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        shown.presented_motion.set(cx.motion);
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        (service, runtime, instance, view)
    };
    let segment = |runtime: &NativeRuntime, instance, view| -> Vec<&'static str> {
        let compiled = compile_settings_view(runtime, instance, view, &cx);
        ["dark", "light"]
            .into_iter()
            .filter(|word| {
                compiled
                    .semantic(&UiKey::new(format!("settings/showcase/side/{word}")))
                    .and_then(|node| node.state)
                    .is_some_and(|state| state.selected)
            })
            .collect()
    };
    let strip = |runtime: &NativeRuntime, instance, view| -> String {
        let compiled = compile_settings_view(runtime, instance, view, &cx);
        compiled
            .semantic(&UiKey::new(STATUS))
            .map(|node| node.label.clone())
            .unwrap_or_default()
    };
    let keep = |runtime: &mut NativeRuntime, instance, view| -> Option<String> {
        let kept = act(runtime, instance, view, "settings/showcase/keep", None);
        config_patches(&kept)
            .first()
            .and_then(|(patch, _)| patch.edits[0].value.clone())
    };

    let (_service, mut runtime, instance, view) = open();
    assert_eq!(
        segment(&runtime, instance, view),
        ["dark"],
        "the OS side first"
    );
    let card = card_action(&runtime, view, "GitHub Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    assert_eq!(segment(&runtime, instance, view), ["light"]);
    let words = strip(&runtime, instance, view);
    assert!(
        words.contains("for Light") && words.contains("Solarized Light"),
        "{words}"
    );
    assert_eq!(
        keep(&mut runtime, instance, view).as_deref(),
        Some("dark:Nord,light:GitHub Light")
    );

    let (_service, mut runtime, instance, view) = open();
    let solarized = card_action(&runtime, view, "Solarized Light");
    let _ = act(&mut runtime, instance, view, &solarized, None);
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
    assert_eq!(segment(&runtime, instance, view), ["light"]);
    let dracula = card_action(&runtime, view, "Dracula");
    let _ = act(&mut runtime, instance, view, &dracula, None);
    assert_eq!(segment(&runtime, instance, view), ["dark"]);
    assert_eq!(
        keep(&mut runtime, instance, view).as_deref(),
        Some("dark:Dracula,light:Solarized Light")
    );

    let (_service, mut runtime, instance, view) = open();
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/side/dark",
        None,
    );
    let card = card_action(&runtime, view, "GitHub Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    assert_eq!(segment(&runtime, instance, view), ["dark"]);
    assert_eq!(
        keep(&mut runtime, instance, view).as_deref(),
        Some("dark:GitHub Light,light:Solarized Light")
    );

    let (_service, mut runtime, instance, view) = open();
    let card = card_action(&runtime, view, "GitHub Light");
    let _ = act(&mut runtime, instance, view, &card, None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    assert_eq!(segment(&runtime, instance, view), ["dark"]);
    assert_eq!(
        state(&runtime, view).highlighted_slug().as_deref(),
        Some("nord")
    );
}

#[test]
fn the_chooser_comes_on_glass_on_the_row_in_use_whole() {
    let whole = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled.paint.iter().any(|node| {
            node.key.as_str() == key
                && !node.peek
                && node.clip.y <= node.rect.y + 0.01
                && node.clip.bottom() + 0.01 >= node.rect.bottom()
                && node.clip.width + 0.01 >= node.rect.width
        })
    };
    for (width, height) in [
        (1_564.0, 852.0),
        (584.0, 348.0),
        (1_284.0, 714.0),
        (900.0, 520.0),
        (700.0, 420.0),
    ] {
        let cx = view_cx_at(width, height);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"GitHub Light\"\n");
        let row = card_action(&runtime, view, "GitHub Light");
        let on_glass = |runtime: &mut NativeRuntime, step: &str| {
            let compiled = compile_settings_view(runtime, instance, view, &cx);
            assert!(
                whole(&compiled, &row),
                "{width}x{height} {step}: {row} whole"
            );
        };
        on_glass(&mut runtime, "opening");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/route/appearance",
            None,
        );
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let _ = act(&mut runtime, instance, view, "settings/route/themes", None);
        on_glass(&mut runtime, "back from Appearance");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/search",
            Some("nord"),
        );
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let _ = act(&mut runtime, instance, view, "settings/search-clear", None);
        on_glass(&mut runtime, "back from Search Results");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize-done",
            None,
        );
        on_glass(&mut runtime, "Customize closed");
    }
    let (_service, mut runtime, instance, view) = themes_view("theme = \"GitHub Light\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("nord"),
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/route/appearance",
        None,
    );
    let _ = act(&mut runtime, instance, view, "settings/route/themes", None);
    assert_eq!(state(&runtime, view).showcase_landing(), None);
    assert_eq!(state(&runtime, view).page_reveal, None);
}

#[test]
fn a_landing_the_keyboard_took_over_is_let_go_as_drawn() {
    const PACK: &str = "settings/showcase/pack";
    let cx = view_cx_at(1_284.0, 714.0);
    let on_screen = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .is_some_and(|node| !node.offscreen)
    };
    let (_service, runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let in_use = card_action(&runtime, view, "Nord");
    let landed = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        on_screen(&landed, &in_use) && !on_screen(&landed, PACK),
        "the control: the page lands on {in_use}, the disclosure off its screen"
    );
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let _ = act(&mut runtime, instance, view, PACK, None);
    let first = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        on_screen(&first, PACK),
        "the first screen shows the disclosure, where the keyboard is"
    );
    assert!(
        compile_settings_view(&runtime, instance, view, &cx) == first,
        "the next frame draws the same screen"
    );
}

fn set_text_scale(scale: f32) {
    crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences {
            text_scale: scale,
            ..crate::native_appearance::current_preferences()
        },
    );
}

fn shown_of(compiled: &crate::native_ui::CompiledUi, key: &str) -> (f32, f32) {
    let part = compiled
        .paint
        .iter()
        .filter(|node| node.key.as_str() == key)
        .map(|node| {
            let top = node.rect.y.max(node.clip.y);
            (node.rect.bottom().min(node.clip.bottom()) - top).max(0.0)
        })
        .fold(0.0_f32, f32::max);
    (part, rect_of(compiled, key).height)
}

#[test]
fn a_landing_the_keyboard_took_over_stays_let_go_when_the_list_is_measured_again() {
    const PACK: &str = "settings/showcase/pack";
    let shown = |compiled: &crate::native_ui::CompiledUi| shown_of(compiled, PACK);
    let mut cases = Vec::new();
    for (width, tall, shorter) in [
        (
            1_284.0,
            714.0,
            &[700.0, 600.0, 520.0, 462.0, 418.0, 380.0][..],
        ),
        (1_144.0, 700.0, &[600.0, 520.0, 462.0, 418.0, 380.0][..]),
        (864.0, 700.0, &[600.0, 520.0, 462.0, 418.0, 380.0][..]),
    ] {
        cases.extend(
            shorter
                .iter()
                .map(|&short| (width, (tall, 1.0), (short, 1.0))),
        );
        cases.push((width, (tall, 1.0), (tall, 2.0)));
    }
    on_workers(&cases, "landings the keyboard took over", |case| {
        let &(width, first, then) = case;
        set_text_scale(first.1);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
        let _ = act(&mut runtime, instance, view, PACK, None);
        let mut last = compile_settings_view(&runtime, instance, view, &view_cx_at(width, first.0));
        assert!(
            shown(&last).0 > 0.0,
            "{case:?}: the first screen shows the disclosure"
        );
        for (frame, (height, scale)) in [(1, then), (2, then), (3, first)] {
            set_text_scale(scale);
            let screen =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
            let ((was, was_tall), (now, tall)) = (shown(&last), shown(&screen));
            if frame == 2 {
                assert!(
                    screen == last,
                    "{case:?}: frame {frame}, drawn again, draws the same screen"
                );
            } else if was + 0.5 >= was_tall {
                assert!(
                    now + 0.5 >= tall,
                    "{case:?}: frame {frame} keeps the disclosure whole ({now} of {tall}pt shown)"
                );
            } else {
                assert!(
                    now + 0.5 >= was,
                    "{case:?}: frame {frame} shows no less of the disclosure ({was}pt, then \
                     {now}pt)"
                );
            }
            last = screen;
        }
    });
}

#[test]
fn a_status_while_a_landing_waits_leaves_the_keyboards_control_whole() {
    const PACK: &str = "settings/showcase/pack";
    set_text_scale(2.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let _ = act(&mut runtime, instance, view, PACK, None);
    let cx = view_cx_at(178.0, 348.0);
    let first = compile_settings_view(&runtime, instance, view, &cx);
    let (shown, tall) = shown_of(&first, PACK);
    assert!(
        shown + 0.5 >= tall,
        "the first screen shows the disclosure whole: {shown} of {tall}pt"
    );
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!();
    };
    state.feedback = Some(
        "/Volumes/Shared Drive/Users//someone/dotfiles/config/aterm isn\u{2019}t writable \
         \u{b7} check its permissions"
            .to_string(),
    );
    let screen = compile_settings_view(&runtime, instance, view, &cx);
    let (shown, tall) = shown_of(&screen, PACK);
    assert!(
        shown + 0.5 >= tall,
        "under the status the disclosure stays whole: {shown} of {tall}pt"
    );
}

#[test]
fn the_keyboards_control_stays_in_view_where_a_frame_lists_what_the_last_pinned() {
    const DONE: &str = "settings/showcase/customize-done";
    const FILTER: &str = "settings/showcase/filter";
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Also {
        Nothing,
        DrawnFirst,
        Settled,
    }
    let mut cases = Vec::new();
    for theme in ["Dracula", "GitHub Light"] {
        let short = (444.0, 180.0, 2.0);
        for wide in [(1_095.0, 700.0, 2.0), (584.0, 348.0, 2.0)] {
            cases.push((theme, DONE, vec![short, wide, short], Also::Nothing));
            cases.push((theme, DONE, vec![wide, short], Also::Nothing));
        }
    }
    for short in [
        (444.0, 180.0, 1.0),
        (444.0, 180.0, 2.0),
        (300.0, 160.0, 2.0),
    ] {
        cases.push((
            "Nord",
            FILTER,
            vec![short, (1_284.0, 714.0, short.2), short],
            Also::Nothing,
        ));
    }
    for short in [
        (444.0, 180.0, 1.0),
        (444.0, 180.0, 2.0),
        (300.0, 160.0, 2.0),
        (584.0, 348.0, 2.0),
    ] {
        for also in [Also::DrawnFirst, Also::Settled] {
            cases.push(("Nord", FILTER, vec![(1_284.0, 714.0, short.2), short], also));
        }
    }
    on_workers(&cases, "pinned controls listed", |case| {
        let (theme, key, frames, also) = case;
        let put_on = |runtime: &mut NativeRuntime, instance, view| {
            let _ = dispatch(
                runtime,
                instance,
                view,
                AppEvent::FocusChanged(Some(UiKey::new(*key))),
            );
        };
        let opened = |scale: f32| {
            set_text_scale(scale);
            let (service, mut runtime, instance, view) =
                themes_view(&format!("theme = \"{theme}\"\n"));
            if *key == DONE {
                let _ = act(
                    &mut runtime,
                    instance,
                    view,
                    "settings/showcase/customize",
                    None,
                );
            } else {
                put_on(&mut runtime, instance, view);
            }
            (service, runtime, instance, view)
        };
        let draw = |page: &(VersionedConfigService, NativeRuntime, _, _),
                    &(width, height, scale): &(f32, f32, f32)| {
            set_text_scale(scale);
            compile_settings_view(&page.1, page.2, page.3, &view_cx_at(width, height))
        };
        let mut page = opened(frames[0].2);
        let mut last = draw(&page, &frames[0]);
        let (first, tall) = shown_of(&last, key);
        assert!(
            first + 0.5 >= tall,
            "{case:?}: the first screen shows {key} whole ({first} of {tall}pt shown)"
        );
        if *also == Also::Settled {
            put_on(&mut page.1, page.2, page.3);
        }
        for (frame, size) in frames.iter().enumerate().skip(1) {
            let screen = draw(&page, size);
            let ((was, was_tall), (now, tall)) = (shown_of(&last, key), shown_of(&screen, key));
            if was + 0.5 >= was_tall {
                assert!(
                    now + 0.5 >= tall,
                    "{case:?}: frame {frame} keeps {key} whole ({now} of {tall}pt shown)"
                );
            } else {
                assert!(
                    now + 0.5 >= was,
                    "{case:?}: frame {frame} shows no less of {key} ({was}pt, then {now}pt)"
                );
            }
            last = screen;
        }
        let size = frames.last().expect("frames");
        assert!(
            draw(&page, size) == last,
            "{case:?}: the last frame, drawn again, draws the same screen"
        );
        if *also == Also::DrawnFirst {
            assert!(
                draw(&opened(size.2), size) == last,
                "{case:?}: the last frame draws the screen its window draws first"
            );
        }
    });
}

fn rows_whole(compiled: &crate::native_ui::CompiledUi) -> Vec<String> {
    drawn_rows(compiled)
        .into_iter()
        .filter(|key| whole_in_list(compiled, key))
        .map(|key| {
            key.trim_start_matches("settings/showcase/card/")
                .to_string()
        })
        .collect()
}

fn wheeled_past_the_filter(
    at: (f32, f32, f32),
) -> (
    VersionedConfigService,
    NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::native_app::ViewId,
    crate::native_ui::CompiledUi,
) {
    let (width, height, scale) = at;
    set_text_scale(scale);
    let (service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::FocusChanged(Some(UiKey::new("settings/showcase/filter"))),
    );
    let cx = view_cx_at(width, height);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ScrollAt {
            lines: 4,
            key: UiKey::new("settings/showcase/list"),
        },
    );
    let wheeled = compile_settings_view(&runtime, instance, view, &cx);
    (service, runtime, instance, view, wheeled)
}

#[test]
fn a_control_a_window_pinned_comes_back_whole_from_the_wheels_place() {
    const FILTER: &str = "settings/showcase/filter";
    let cases = [
        ((444.0, 180.0, 1.0), vec![(584.0, 348.0)]),
        ((444.0, 180.0, 1.0), vec![(444.0, 420.0)]),
        (
            (444.0, 180.0, 1.0),
            vec![(520.0, 260.0), (584.0, 348.0), (520.0, 260.0)],
        ),
        ((444.0, 180.0, 1.0), vec![(1_284.0, 714.0)]),
        ((444.0, 180.0, 2.0), vec![(1_284.0, 714.0)]),
        ((444.0, 180.0, 2.0), vec![(864.0, 500.0)]),
        ((300.0, 160.0, 2.0), vec![(1_284.0, 714.0)]),
        ((584.0, 348.0, 2.0), vec![(864.0, 500.0)]),
        ((584.0, 348.0, 2.0), vec![(1_284.0, 714.0)]),
    ];
    on_workers(&cases, "wheel round trips", |case| {
        let &((width, height, scale), ref trip) = case;
        let (_service, runtime, instance, view, wheeled) =
            wheeled_past_the_filter((width, height, scale));
        let (shown, tall) = shown_of(&wheeled, FILTER);
        assert!(
            shown + 0.5 < tall,
            "{case:?}: the wheel takes the filter out of view ({shown} of {tall}pt shown)"
        );
        let wheel_top = state(&runtime, view)
            .scroll_record
            .borrow()
            .as_ref()
            .map(scroll::ScrollRecord::top);
        let pinned = trip
            .iter()
            .map(|&(width, height)| {
                let screen =
                    compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
                let plan = state(&runtime, view).scroll_plan();
                let (shown, tall) = shown_of(&screen, FILTER);
                plan.is_some_and(|plan| plan.stop_of(&UiKey::new(FILTER)).is_none())
                    && shown + 0.5 >= tall
            })
            .collect::<Vec<_>>();
        assert!(
            pinned.contains(&true),
            "{case:?}: a window on the way pins the filter whole"
        );
        let back = compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
        let (shown, tall) = shown_of(&back, FILTER);
        assert!(
            shown + 0.5 >= tall,
            "{case:?}: the first window again shows the filter whole: {shown} of {tall}pt, rows \
             {:?}",
            rows_whole(&back)
        );
        let state = state(&runtime, view);
        let filter = state
            .scroll_plan()
            .and_then(|plan| plan.stop_of(&UiKey::new(FILTER)));
        let top = state
            .scroll_record
            .borrow()
            .as_ref()
            .map(scroll::ScrollRecord::top);
        assert!(
            filter.is_some() && top == filter && filter < wheel_top,
            "{case:?}: the list leaves the wheel's place (scroll {wheel_top:?}) no further than \
             the filter's stop ({filter:?}): scroll {top:?}"
        );
    });
}

#[test]
fn a_wheel_over_a_pinned_control_leaves_it_whole_in_a_shorter_window() {
    const FILTER: &str = "settings/showcase/filter";
    on_workers(&[1.0, 2.0], "wheels over a pinned control", |&scale| {
        let (_service, runtime, instance, view, wheeled) =
            wheeled_past_the_filter((1_284.0, 714.0, scale));
        let shorter = compile_settings_view(&runtime, instance, view, &view_cx_at(444.0, 180.0));
        let (shown, tall) = shown_of(&shorter, FILTER);
        assert!(
            shown + 0.5 >= tall,
            "at {scale}x text, 444x180 keeps the filter the wheeled 1284x714 screen pinned \
             whole: {shown} of {tall}pt shown"
        );
        let back = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
        assert!(
            back == wheeled,
            "at {scale}x text, 1284x714 again draws the screen the wheel left: rows {:?}, \
             then {:?}",
            rows_whole(&wheeled),
            rows_whole(&back)
        );
    });
}

#[test]
fn a_resize_before_any_event_keeps_the_theme_in_use_in_view() {
    let set_scale = |scale: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
    };
    let mut cases = Vec::new();
    for theme in ["One Dark", "Nord", "Dracula"] {
        for width in [864.0, 1_144.0] {
            for tall in [900.0, 700.0, 600.0, 520.0] {
                for short in [462.0, 418.0, 380.0, 348.0] {
                    cases.push((theme, width, (tall, 1.0), (short, 1.0)));
                }
            }
            for height in [900.0, 700.0, 520.0] {
                cases.push((theme, width, (height, 1.0), (height, 2.0)));
            }
        }
    }
    let lost = std::sync::Mutex::new(Vec::new());
    on_workers(&cases, "landings measured again", |case| {
        let &(theme, width, first, then) = case;
        for let_go in [false, true] {
            set_scale(first.1);
            let (_service, mut runtime, instance, view) =
                themes_view(&format!("theme = \"{theme}\"\n"));
            let key = card_action(&runtime, view, theme);
            let landed =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, first.0));
            assert!(
                whole_in_list(&landed, &key),
                "{case:?}: the page lands on {key}"
            );
            if let_go {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.settle_page_scroll();
            }
            set_scale(then.1);
            let compiled =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, then.0));
            if !whole_in_list(&compiled, &key) {
                lost.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((let_go, *case));
            }
        }
    });
    let lost = lost
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let kept: Vec<_> = lost.iter().filter(|(let_go, _)| !let_go).collect();
    assert!(
        kept.is_empty(),
        "a frame measured again before any event keeps the theme in use whole: {kept:?}"
    );
    for (what, rescaled) in [("a resize", false), ("a text size", true)] {
        assert!(
            lost.iter()
                .any(|(let_go, (_, _, first, then))| *let_go && (first.1 != then.1) == rescaled),
            "the control: {what} after the landing was let go loses the row"
        );
    }
}

#[test]
fn every_part_of_the_themes_page_is_whole_on_some_screen() {
    let part = |key: &str| {
        key == "settings/showcase/stage"
            || key == "settings/showcase/status/title"
            || key == "settings/showcase/customize"
            || key == "settings/showcase/customize-done"
            || key == "settings/showcase/filter"
            || key == "settings/showcase/side"
            || key == "settings/showcase/pack"
            || key == "settings/showcase/footer/follow"
            || key == "settings/showcase/footer/links"
            || key == "settings/showcase/sheet-title"
            || key == "settings/showcase/colours/heading"
            || (key.starts_with("settings/showcase/card/") && key.matches('/').count() == 3)
            || (key.starts_with("settings/row/") && key.contains("showcase"))
    };
    let whole = |node: &crate::native_ui::PaintNode| {
        !node.peek
            && node.clip.y <= node.rect.y + 0.5
            && node.clip.bottom() + 0.5 >= node.rect.bottom()
    };
    at_each_text_scale(&[1.0, 2.0], "themes screens", |scale| {
        for source in [
            "theme = \"Dracula\"\n",
            "theme = \"dark:Nord,light:GitHub Light\"\n",
        ] {
            for sheet in [false, true] {
                for (width, height) in [
                    (584.0, 348.0),
                    (584.0, 292.0),
                    (584.0, 236.0),
                    (444.0, 180.0),
                    (1_284.0, 236.0),
                    (1_564.0, 236.0),
                    (1_564.0, 320.0),
                ] {
                    let (_service, mut runtime, instance, view) = themes_view(source);
                    if sheet {
                        let _ = act(
                            &mut runtime,
                            instance,
                            view,
                            "settings/showcase/customize",
                            None,
                        );
                    }
                    let cx = view_cx_at(width, height);
                    let mut seen = std::collections::BTreeSet::new();
                    let mut shown_whole = std::collections::BTreeSet::new();
                    each_screen(&mut runtime, instance, view, &cx, |compiled, _| {
                        for node in &compiled.paint {
                            let key = node.key.as_str();
                            if !part(key) || node.clip.height <= 0.0 {
                                continue;
                            }
                            seen.insert(key.to_string());
                            if whole(node) {
                                shown_whole.insert(key.to_string());
                            }
                        }
                    });
                    let never: Vec<&String> = seen.difference(&shown_whole).collect();
                    assert!(
                        never.is_empty(),
                        "{width}×{height} at {scale}× over {source:?} (sheet {sheet}): \
                         never whole on any screen: {never:?}"
                    );
                    assert!(
                        seen.contains("settings/showcase/stage"),
                        "{width}×{height} at {scale}×: the demo is drawn"
                    );
                }
            }
        }
    });
}

#[test]
fn keep_more_exists_only_for_type_carrying_themes() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Dracula\"\n",
        studio_catalog("font_family = Berkeley Mono\n"),
    );
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let more = compiled
        .semantic(&UiKey::new("settings/showcase/keep-more"))
        .expect("the chevron");
    assert_eq!(more.state.and_then(|state| state.expanded), Some(false));
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), Some(true));

    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(compiled.semantic(&UiKey::new(STATUS)).is_some());
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/keep-more"))
            .is_none()
    );
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), None);
}

#[test]
fn the_strip_never_moves() {
    for width in SHAPES {
        let cx = view_cx_at(width, 714.0);
        let mut seen: Vec<(&str, (LogicalRect, [f32; 3]))> = Vec::new();
        let mut take = |name: &'static str,
                        runtime: &NativeRuntime,
                        instance: crate::native_app::AppInstanceId,
                        view: crate::native_app::ViewId| {
            let compiled = compile_settings_view(runtime, instance, view, &cx);
            let title = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/showcase/status/title")
                .map(|node| [node.rect.x, node.rect.y, node.rect.height])
                .expect("the strip's first line is painted");
            seen.push((name, (rect_of(&compiled, STATUS), title)));
        };
        let (_service, mut runtime, instance, view) =
            typography_view("theme = \"Dracula\"\n", studio_catalog(STUDIO_TYPE));
        take("idle", &runtime, instance, view);
        let nord = card_action(&runtime, view, "Nord");
        let _ = act(&mut runtime, instance, view, &nord, None);
        take("trying", &runtime, instance, view);
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        take("trying type", &runtime, instance, view);
        let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
        take("keeping", &runtime, instance, view);
        let (_, operation) = config_patches(&kept)[0].clone();
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Rejected {
                    message: "the file changed under the write".to_string(),
                },
            },
        );
        take("refused", &runtime, instance, view);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/revert",
            None,
        );
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        take("customize", &runtime, instance, view);
        let (_service, runtime, instance, view) =
            themes_view("theme = \"Dracula\"\nforeground = \"#fafafa\"\n");
        take("idle with overrides", &runtime, instance, view);
        let (first, rect) = seen[0];
        for (name, other) in &seen[1..] {
            assert_eq!(*other, rect, "{width}: {name} against {first}");
        }
    }
}

#[test]
fn the_list_does_not_move_on_a_trial() {
    for width in SHAPES {
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let cx = view_cx_at(width, 714.0);
        let idle = compile_settings_view(&runtime, instance, view, &cx);
        let rows = drawn_rows(&idle);
        let first = rows.first().expect("a row").clone();
        let other = rows
            .iter()
            .find(|row| {
                row.strip_prefix(ROW) != state(&runtime, view).highlighted_slug().as_deref()
            })
            .expect("a row not in use")
            .clone();
        let _ = act(&mut runtime, instance, view, &other, None);
        let trying = compile_settings_view(&runtime, instance, view, &cx);
        assert_eq!(rect_of(&trying, &first), rect_of(&idle, &first), "{width}");
    }
}

#[test]
fn a_reader_scrolls_an_offscreen_row_into_view() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let cx = view_cx_at(1_074.0, 684.0);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let below = compiled
        .semantics
        .iter()
        .rev()
        .find(|node| node.role == SemanticRole::ListBoxOption && node.offscreen)
        .expect("the list scrolls at this size")
        .key
        .clone();
    let record = compiled
        .controls_lines()
        .into_iter()
        .find(|line| line.starts_with(&format!("ui key={:?} ", below.as_str())))
        .expect("its record");
    assert!(record.ends_with(" offscreen=true"), "{record}");
    let first_shown = compiled
        .semantics
        .iter()
        .find(|node| node.role == SemanticRole::ListBoxOption && !node.offscreen)
        .expect("a row on screen")
        .key
        .clone();
    assert!(!compiled.controls_lines().iter().any(|line| {
        line.starts_with(&format!("ui key={:?} ", first_shown.as_str()))
            && line.contains("offscreen")
    }),);
    let focus = state(&runtime, view).common.last_focus.clone();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::RevealControl(UiKey::new("settings/showcase/no-such-row")),
        )
        .unwrap();
    let unmoved = compile_settings_view(&runtime, instance, view, &cx);
    assert!(unmoved.semantic(&below).is_some_and(|node| node.offscreen));
    runtime
        .dispatch(instance, view, AppEvent::RevealControl(below.clone()))
        .unwrap();
    let revealed = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        revealed
            .semantic(&below)
            .is_some_and(|node| !node.offscreen)
    );
    assert_eq!(state(&runtime, view).common.last_focus, focus);
}

#[test]
fn each_option_is_the_lists_child_and_its_only_node() {
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(
        dir.path().join("Harbour.conf"),
        "foreground = #dddddd\nbackground = #101218\nfont_family = Menlo\n",
    )
    .unwrap();
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let published = service.replace_theme_catalog(std::sync::Arc::new(
        crate::app_config::ThemeCatalog::discover_in(dir.path()),
    ));
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ConfigChanged(published),
    );
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 934.0));
    let options: Vec<&crate::native_ui::SemanticNode> = compiled
        .semantics
        .iter()
        .filter(|node| node.role == SemanticRole::ListBoxOption)
        .collect();
    assert!(options.len() > 3);
    let mut file_actions = 0;
    for option in options {
        assert_eq!(
            option.parent.as_ref().map(UiKey::as_str),
            Some(showcase_page::SHOWCASE_LIST),
            "{}",
            option.key.as_str()
        );
        let inner = format!("{}/", option.key.as_str());
        for node in compiled
            .semantics
            .iter()
            .filter(|node| node.key.as_str().starts_with(&inner))
        {
            let actions = format!("{inner}actions");
            assert!(
                node.key.as_str() == actions
                    || node.parent.as_ref().map(UiKey::as_str) == Some(actions.as_str()),
                "{}",
                node.key.as_str()
            );
            file_actions += usize::from(node.action.is_some());
        }
    }
    assert!(file_actions >= 2, "the Yours row's Open and Trash");
}

#[test]
fn the_demo_is_present_at_every_shape() {
    for source in ["theme = \"Dracula\"\n", "theme = \"Solarized Light\"\n"] {
        for width in SHAPES {
            let (_service, runtime, instance, view) = themes_view(source);
            let cx = view_cx_at(width, 714.0);
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let stage = rect_of(&compiled, STAGE);
            assert!(stage.height >= 180.0, "{source} {width}: {stage:?}");
            assert!(
                stage.width >= 40.0 * 12.0 * 0.6,
                "{source} {width}: {stage:?}"
            );
        }
    }
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(556.0, 520.0));
    assert!(compiled.semantic(&UiKey::new(STAGE)).is_some());
    let rows = drawn_rows(&compiled);
    assert!(rows.len() >= 3, "{rows:?}");
}

#[test]
fn the_stacked_chooser_holds_a_whole_stop_at_every_line_height() {
    for (line_height, width) in ["1.0", "1.1", "1.5"]
        .into_iter()
        .flat_map(|line_height| [(line_height, 556.0), (line_height, 584.0)])
    {
        let (_service, mut runtime, instance, view) =
            themes_view(&format!("line_height = {line_height}\n"));
        let cx = view_cx_at(width, 348.0);
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let context = format!("{width}x348 line height {line_height}, screen {top}");
            assert!(
                rect_of(compiled, STAGE).height > 0.0,
                "{context}: the demo shows"
            );
            let chooser = rect_of(compiled, "settings/showcase/chooser");
            let list = rect_of(compiled, showcase_page::SHOWCASE_LIST);
            let rows_top = compiled
                .paint
                .iter()
                .find(|paint| paint.key.as_str().ends_with("/scrolled-rows"))
                .map_or(list.y, |paint| paint.rect.y);
            let mut whole = 0;
            for hit in &compiled.hits {
                let paint = compiled
                    .paint
                    .iter()
                    .find(|paint| paint.key == hit.key)
                    .unwrap_or_else(|| panic!("{context}: {:?} has no paint", hit.key));
                assert_eq!(
                    paint.clip, hit.rect,
                    "{context}: {:?} live over what shows",
                    hit.key
                );
                if paint.rect == hit.rect {
                    whole += 1;
                } else {
                    assert!(
                        paint.rect.y < rows_top - 0.01
                            || paint.rect.bottom() > list.bottom() + 0.01,
                        "{context}: {:?} {:?} is cut inside the list {list:?}",
                        hit.key,
                        paint.rect
                    );
                }
            }
            assert!(whole > 0, "{context}: no control shows whole");
            assert!(
                list.bottom() <= chooser.bottom() + 0.01,
                "{context}: the list {list:?} runs past the chooser {chooser:?}"
            );
        });
    }
}

#[test]
fn no_node_crosses_its_column_edge() {
    for width in SHAPES {
        for height in [714.0, 934.0] {
            let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
            let compiled =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
            let list = rect_of(&compiled, showcase_page::SHOWCASE_LIST);
            let rows = drawn_rows(&compiled);
            let scrolls = rows.len() < state(&runtime, view).showcase.listed().len();
            for node in compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("settings/showcase/"))
            {
                assert!(
                    node.rect.right() <= width + 0.01,
                    "{width}x{height}: {} {:?}",
                    node.key.as_str(),
                    node.rect
                );
                if node.key.as_str().starts_with(ROW) {
                    let edge = if scrolls {
                        list.right() - showcase_page::THUMB_GUTTER
                    } else {
                        list.right()
                    };
                    assert!(
                        node.rect.x >= list.x - 0.01 && node.rect.right() <= edge + 0.01,
                        "{width}x{height}: {} {:?} in {list:?}",
                        node.key.as_str(),
                        node.rect
                    );
                }
            }
        }
    }
}

fn visible_words(compiled: &crate::native_ui::CompiledUi, key: &str) -> Option<String> {
    compiled.paint.iter().find_map(|node| match &node.content {
        UiContent::Button(control) if node.key.as_str() == key => {
            let words = control
                .spec
                .visual_label
                .clone()
                .unwrap_or_else(|| control.spec.label.clone());
            (!words.trim().is_empty()).then_some(words)
        }
        _ => None,
    })
}

#[test]
fn every_interactive_node_has_key_label_and_action() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Dracula\"\n",
        studio_catalog("font_family = Menlo\n"),
    );
    let mut frames = vec![compile_settings_view(&runtime, instance, view, &cx)];
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/keep-more",
        None,
    );
    frames.push(compile_settings_view(&runtime, instance, view, &cx));
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/4",
        None,
    );
    let slot_open = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        slot_open
            .semantic(&UiKey::new("settings/showcase/palette-slot-close"))
            .is_some(),
        "the slot editor's buttons are on screen"
    );
    frames.push(slot_open);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette-slot-close",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    frames.push(compile_settings_view(&runtime, instance, view, &cx));
    let mut checked = 0;
    for compiled in &frames {
        for node in compiled.semantics.iter().filter(|node| {
            node.key.as_str().starts_with("settings/showcase/") && node.state.is_some()
        }) {
            let key = node.key.as_str();
            assert!(!node.label.trim().is_empty(), "{key}: no label");
            assert!(node.action.is_some(), "{key}: no action");
            if let Some(words) = visible_words(compiled, key) {
                let words = words.split(" \u{b7} ").next().unwrap_or_default();
                assert!(
                    node.label.to_lowercase().contains(&words.to_lowercase()),
                    "{key}: {:?} shows {words:?}",
                    node.label
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked} controls");
}

#[test]
fn every_showcase_key_round_trips_through_act() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let mut keys = Vec::new();
    for node in compiled
        .semantics
        .iter()
        .filter(|node| node.key.as_str().starts_with("settings/showcase/"))
    {
        let Some(action) = node.action.as_ref() else {
            continue;
        };
        assert_eq!(action.as_str(), node.key.as_str());
        keys.push(node.key.as_str().to_string());
    }
    assert!(keys.iter().any(|key| key.starts_with(ROW)));
    for key in &keys {
        if key.ends_with("/trash") || key.ends_with("open-folder") || key.ends_with("import") {
            continue;
        }
        let (_service, mut fresh, instance, view) = themes_view("theme = \"Dracula\"\n");
        let _ = compile_settings_view(&fresh, instance, view, &cx);
        let before = state(&fresh, view).common.presentation_revision;
        let pressed = act(&mut fresh, instance, view, key, None);
        let after = state(&fresh, view).common.presentation_revision;
        assert!(
            after != before
                || !pressed.effects.is_empty()
                || state(&fresh, view).common.last_focus.is_some(),
            "{key}: the press did nothing"
        );
    }
}

#[test]
fn filter_finds_pack_themes_and_collapses_empty_sections() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/filter"))
            .is_some()
    );
    assert!(!state(&runtime, view).showcase.pack_open);
    let kanagawa = card_action(&runtime, view, "Kanagawa Wave");
    assert!(compiled.semantic(&UiKey::new(kanagawa.as_str())).is_none());
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("kanagawa"),
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(compiled.semantic(&UiKey::new(kanagawa.as_str())).is_some());
    let rows = drawn_rows(&compiled);
    assert!(rows.iter().all(|row| {
        let id = id_of(&runtime, view, row.strip_prefix(ROW).unwrap());
        id.to_lowercase().contains("kanagawa")
            || state(&runtime, view)
                .showcase
                .cards
                .iter()
                .any(|card| card.id == id && card.matches("kanagawa", MatchReach::Words))
    }));
    for word in ["signature", "builtin", "yours"] {
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/showcase/section/{word}")))
                .is_none(),
            "{word}: no match, no heading"
        );
    }
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("zzzz no such theme"),
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(drawn_rows(&compiled).is_empty());
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/empty"))
            .is_some()
    );
}

#[test]
fn the_stage_value_changes_only_with_the_theme() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let value_at = |runtime: &NativeRuntime, phase: u64, reduced: bool| {
        let mut cx = view_cx_at(1_284.0, 714.0);
        cx.animation_phase_ms = phase;
        cx.motion.system_reduced = reduced;
        compile_settings_view(runtime, instance, view, &cx)
            .semantic(&UiKey::new(STAGE))
            .expect("the demo")
            .value
            .clone()
    };
    let first = value_at(&runtime, 720, false);
    for step in 1..10 {
        assert_eq!(
            value_at(&runtime, 720 + step * 1_300, false),
            first,
            "{step}"
        );
    }
    assert_ne!(value_at(&runtime, 720, true), first, "Reduce Motion");
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    assert_ne!(value_at(&runtime, 720, false), first, "a trial");
}

#[test]
fn sheet_rows_join_the_tree_only_while_open() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let has = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled.semantic(&UiKey::new(key)).is_some()
    };
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(!drawn_rows(&compiled).is_empty());
    assert!(!has(&compiled, "settings/showcase/sheet"));
    assert!(!has(&compiled, "settings/showcase/palette/0"));
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(drawn_rows(&compiled).is_empty(), "the list is not painted");
    assert!(!has(&compiled, showcase_page::SHOWCASE_LIST));
    for key in [
        "settings/showcase/sheet",
        "settings/showcase/customize-done",
        "settings/showcase/palette/0",
    ] {
        assert!(has(&compiled, key), "{key}");
    }
    assert!(has(
        &compiled,
        &format!("settings/control/{}", prefs::EDIT_FOREGROUND)
    ));
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/customize-done",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(!drawn_rows(&compiled).is_empty());
    assert!(!has(&compiled, "settings/showcase/sheet"));
}

#[test]
fn the_live_region_announces_once_per_settled_trial() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let label_at = |runtime: &NativeRuntime, phase: u64| {
        let mut cx = view_cx_at(1_284.0, 714.0);
        cx.animation_phase_ms = phase;
        let compiled = compile_settings_view(runtime, instance, view, &cx);
        let node = compiled.semantic(&UiKey::new(STATUS)).expect("the strip");
        assert_eq!(node.role, SemanticRole::Status);
        node.label.clone()
    };
    let idle = label_at(&runtime, 720);
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    for row in drawn_rows(&compiled).into_iter().take(4) {
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::FocusChanged(Some(UiKey::new(row))),
        );
        assert_eq!(label_at(&runtime, 720), idle, "a passive move says nothing");
    }
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let trying = label_at(&runtime, 720);
    let card = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0))
        .semantic(&UiKey::new(nord.as_str()))
        .map(|node| node.label.clone())
        .expect("the tried row");
    assert!(trying.contains(&card), "{trying} names {card}");
    assert_ne!(trying, idle, "the trial is said");
    for phase in [2_000, 9_000, 30_000] {
        assert_eq!(label_at(&runtime, phase), trying, "said once");
    }
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    let reverted = label_at(&runtime, 720);
    assert!(!reverted.trim().is_empty());
    assert_ne!(reverted, trying, "the revert is said");
}

#[test]
fn a_community_trial_ends_when_its_name_turns_up_broken() {
    for (text, ends) in [
        ("foreground = \"#zz\"\n=\n", true),
        ("foreground = #dddddd\nbackground = #101218\n", false),
    ] {
        let (mut service, mut runtime, instance, view) = themes_view("");
        let id = state(&runtime, view)
            .showcase
            .cards
            .iter()
            .find(|card| card.section == Section::Community)
            .expect("a community row")
            .id
            .clone();
        let row = card_action(&runtime, view, &id);
        let _ = act(&mut runtime, instance, view, &row, None);
        assert_eq!(intent(&runtime, view), show(&id));
        let dir = aterm_tempfile::tempdir().expect("a themes folder");
        std::fs::write(dir.path().join(format!("{id}.conf")), text).unwrap();
        let published = service.replace_theme_catalog(std::sync::Arc::new(
            crate::app_config::ThemeCatalog::discover_in(dir.path()),
        ));
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::ConfigChanged(published),
        );
        let showcase = &state(&runtime, view).showcase;
        assert_eq!(showcase.trial == ShowcaseTrial::Idle, ends, "{text}");
        assert_eq!(showcase.error.is_some(), ends, "{text}");
    }
}

#[test]
fn failed_theme_files_list_under_needs_attention() {
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(dir.path().join("Broken.conf"), "foreground = \"#zz\"\n=\n").unwrap();
    std::fs::write(
        dir.path().join("Harbour.conf"),
        "foreground = #dddddd\nbackground = #101218\n",
    )
    .unwrap();
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let published = service.replace_theme_catalog(std::sync::Arc::new(
        crate::app_config::ThemeCatalog::discover_in(dir.path()),
    ));
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ConfigChanged(published),
    );
    let attention = state(&runtime, view).showcase.attention.clone();
    assert_eq!(attention.len(), 1, "{attention:?}");
    assert_eq!(attention[0].id, "Broken");
    let row = format!("{ROW}{}", attention[0].slug);
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/section/attention"))
            .is_some()
    );
    let node = compiled
        .semantic(&UiKey::new(row.as_str()))
        .expect("the row");
    assert!(node.label.starts_with("Broken, "), "{}", node.label);
    assert_ne!(node.role, SemanticRole::ListBoxOption);
    assert!(
        compiled
            .semantic(&UiKey::new(format!("{row}/open")))
            .is_some()
    );
    let pressed = act(&mut runtime, instance, view, &row, None);
    assert!(!writes(&pressed));
    assert_eq!(intent(&runtime, view), None, "not a theme to try");
    let harbour = card_action(&runtime, view, "Harbour");
    let _ = act(&mut runtime, instance, view, &harbour, None);
    assert_eq!(intent(&runtime, view), show("Harbour"));
}

#[test]
fn needs_attention_holds_only_the_files_that_did_not_load() {
    const ATTENTION: &str = "NEEDS ATTENTION";
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(dir.path().join("Broken.conf"), "foreground = \"#zz\"\n=\n").unwrap();
    let failing = |failed: bool| {
        let (mut service, mut runtime, instance, view) = themes_view("theme = \"Aterm Day\"\n");
        if failed {
            let published = service.replace_theme_catalog(std::sync::Arc::new(
                crate::app_config::ThemeCatalog::discover_in(dir.path()),
            ));
            let _ = dispatch(
                &mut runtime,
                instance,
                view,
                AppEvent::ConfigChanged(published),
            );
        }
        (service, runtime, instance, view)
    };
    let own = |section: Section| section.caption_after_rows(true);
    let mut signature_captioned = 0;
    for (width, height) in [(864.0, 574.0), (584.0, 350.0), (1_284.0, 714.0)] {
        let cx = view_cx_at(width, height);
        let (_service, mut runtime, instance, view) = failing(true);
        assert_eq!(
            state(&runtime, view).showcase.attention.len(),
            1,
            "premise: one file failed"
        );
        let cards = state(&runtime, view).showcase.cards.clone();
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let Some(list) = compiled
                .semantic(&UiKey::new(showcase_page::SHOWCASE_LIST))
                .map(|node| node.rect)
            else {
                return;
            };
            let on_list = |rect: LogicalRect| {
                rect.x >= list.x - 0.5
                    && rect.x < list.right() - 0.5
                    && rect.y >= list.y - 0.5
                    && rect.y < list.bottom() - 0.5
            };
            let captions: Vec<_> = compiled
                .semantics
                .iter()
                .filter(|node| {
                    node.role == SemanticRole::Heading && !node.offscreen && on_list(node.rect)
                })
                .collect();
            for row in compiled.semantics.iter().filter(|node| {
                node.role == SemanticRole::ListBoxOption && !node.offscreen && on_list(node.rect)
            }) {
                let slug = row.key.as_str().trim_start_matches(ROW);
                let Some(card) = cards.iter().find(|card| card.slug == slug) else {
                    continue;
                };
                if card.section == Section::Community {
                    continue;
                }
                let Some(over) = captions
                    .iter()
                    .filter(|caption| caption.rect.y <= row.rect.y)
                    .max_by(|a, b| a.rect.y.total_cmp(&b.rect.y))
                else {
                    continue;
                };
                assert_ne!(
                    over.label, ATTENTION,
                    "{width}x{height} screen {top}: {} under NEEDS ATTENTION",
                    card.id
                );
                assert_eq!(
                    Some(over.label.as_str()),
                    own(card.section),
                    "{width}x{height} screen {top}: {} under {}",
                    card.id,
                    over.label
                );
                if card.section == Section::Signature {
                    signature_captioned += 1;
                }
            }
        });
    }
    assert!(
        signature_captioned > 0,
        "premise: a signature row is listed under a caption"
    );

    let cx = view_cx_at(864.0, 574.0);
    let (_service, mut runtime, instance, view) = failing(false);
    list_at_top(&mut runtime, view);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/section/signature"))
            .is_none(),
        "the signature themes lead uncaptioned"
    );

    let (_service, runtime, _instance, view) = failing(true);
    let state = state(&runtime, view);
    let failed = format!("{ROW}{}", state.showcase.attention[0].slug);
    let attention_rows = |blocks: Vec<(UiNode, f32, Option<String>)>| {
        showcase_page::section_items(blocks, 300.0, true)
            .into_iter()
            .find_map(|item| match item {
                scroll::ListItem::Split(card)
                    if card.key == "settings/showcase/section-rows/attention" =>
                {
                    Some(
                        card.rows
                            .iter()
                            .map(|(node, _)| node.key.as_str().to_string())
                            .collect::<Vec<_>>(),
                    )
                }
                _ => None,
            })
            .expect("the attention card")
    };
    let blocks = showcase_page::list_blocks(state, 300.0, false);
    assert_eq!(attention_rows(blocks.clone()), vec![failed.clone()]);
    let uncaptioned: Vec<_> = blocks
        .into_iter()
        .filter(|(node, ..)| {
            !node
                .key
                .as_str()
                .starts_with("settings/showcase/section/signature")
        })
        .collect();
    let misread = attention_rows(uncaptioned);
    assert!(
        misread.len() > 1 && misread[0] == failed,
        "the control: {misread:?}"
    );
}

#[test]
fn the_chooser_holds_its_footer_on_its_bottom_edge() {
    const INSET: f32 = 8.0;
    for width in [934.0, 1_284.0] {
        for filter in [None, Some("dracula")] {
            let cx = view_cx_at(width, 714.0);
            let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
            if let Some(text) = filter {
                let _ = act(
                    &mut runtime,
                    instance,
                    view,
                    "settings/showcase/filter",
                    Some(text),
                );
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let panel = rect_of(&compiled, "settings/showcase/chooser");
            let links = rect_of(&compiled, "settings/showcase/footer/links");
            assert!(
                (links.bottom() - (panel.bottom() - INSET)).abs() < 0.5,
                "{width} {filter:?}: footer {links:?} in panel {panel:?}"
            );
            for key in [
                "settings/showcase/filter",
                "settings/showcase/footer/follow",
                "settings/showcase/footer/links",
            ] {
                let rect = rect_of(&compiled, key);
                assert!(
                    rect.y >= panel.y + INSET - 0.01
                        && rect.bottom() <= panel.bottom() - INSET + 0.01,
                    "{width} {filter:?}: {key} {rect:?} leaves {panel:?}'s inset"
                );
            }
        }
    }
}

#[test]
fn captions_and_names_share_one_column() {
    for width in SHAPES {
        let cx = view_cx_at(width, 934.0);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let name = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == format!("{ROW}dracula/name"))
            .map(|node| node.rect)
            .expect("the row's name");
        let caption = rect_of(&compiled, "settings/showcase/section/builtin");
        assert!(
            (caption.x - name.x).abs() < 0.5,
            "{width}: caption {caption:?} beside name {name:?}"
        );
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/filter",
            Some("nomatchxyz"),
        );
        let empty = rect_of(
            &compile_settings_view(&runtime, instance, view, &cx),
            "settings/showcase/empty",
        );
        assert!(
            (empty.x - name.x).abs() < 0.5,
            "{width}: no-match line {empty:?} beside name {name:?}"
        );
        if width >= 934.0 {
            let label = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/showcase/footer/follow-label")
                .map(|node| node.rect)
                .expect("the footer's label");
            assert!(
                (label.x - name.x).abs() < 0.5,
                "{width}: footer label {label:?} beside name {name:?}"
            );
        }
    }
}

#[test]
fn colour_rows_show_the_colour_they_draw() {
    let cx = view_cx_at(1_284.0, 934.0);
    let swatch = |source: &str| {
        let (_service, mut runtime, instance, view) = themes_view(source);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        compiled.paint.iter().find_map(|node| match &node.content {
            UiContent::Button(control) if node.key.as_str() == "settings/color-open/background" => {
                Some(control.spec.chip)
            }
            _ => None,
        })
    };
    let dracula = aterm_types::scheme::builtin("Dracula").expect("built-in");
    let bg = dracula.background;
    assert_eq!(
        swatch("theme = \"Dracula\"\n"),
        Some(Some([bg.r, bg.g, bg.b])),
        "the theme's background"
    );
    assert_eq!(
        swatch("theme = \"Dracula\"\nbackground = \"#102030\"\n"),
        Some(Some([0x10, 0x20, 0x30])),
        "the person's own background, not the theme's"
    );
}

#[test]
fn colour_rows_are_one_line_with_one_field_column() {
    let paint_rect = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == key)
            .map(|node| node.rect)
    };
    for (width, height) in SHAPES
        .into_iter()
        .flat_map(|width| [(width, 934.0), (width, 560.0)])
    {
        let cx = view_cx_at(width, height);
        let (_service, mut runtime, instance, view) =
            themes_view("theme = \"Dracula\"\ncursor_color = \"#ff8800\"\n");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let mut right = None;
        for key in showcase_page::SHOWCASE_COLOR_KEYS {
            let control = format!("settings/control/{key}");
            let field =
                paint_rect(&compiled, &control).unwrap_or_else(|| rect_of(&compiled, &control));
            if let Some(label) = paint_rect(&compiled, &format!("settings/label/{key}")) {
                assert!(
                    (label.y - field.y).abs() < 0.5 && (label.height - field.height).abs() < 0.5,
                    "{width}x{height}: {key}'s name {label:?} beside its field {field:?}"
                );
                assert!(
                    label.right() <= field.x,
                    "{width}x{height}: {key}'s name {label:?} runs into its field {field:?}"
                );
            }
            let edge = *right.get_or_insert(field.right());
            assert!(
                (field.right() - edge).abs() < 0.5,
                "{width}x{height}: {key}'s field ends at {} where the others end at {edge}",
                field.right()
            );
        }
        let reset = rect_of(&compiled, "settings/reset/cursor_color");
        let field = rect_of(&compiled, "settings/control/cursor_color");
        assert!(
            reset.y >= field.y - 0.5 && reset.bottom() <= field.bottom() + 0.5,
            "{width}x{height}: Reset {reset:?} beside its field {field:?}"
        );
        assert!(
            compiled
                .semantic(&UiKey::new("settings/reset/background"))
                .is_none(),
            "{width}x{height}: a colour left to the theme has no Reset"
        );
        let control = "settings/control/background";
        {
            let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                unreachable!()
            };
            state.common.last_focus = Some(UiKey::new(control));
            state.focus_field(Some(&UiKey::new(control)));
            assert_eq!(state.editing_field.as_deref(), Some(prefs::EDIT_BACKGROUND));
        }
        let focused = compile_settings_view(&runtime, instance, view, &cx);
        let line = focused
            .text_fit_audit_line(&UiKey::new(control))
            .expect("the field paints words");
        assert!(
            line.contains("overflow=false") && !line.contains('\u{2026}'),
            "{width}x{height}: the focused field {line}"
        );
        let dracula = aterm_types::scheme::builtin("Dracula").expect("built-in");
        let bg = dracula.background;
        assert!(
            line.contains(&format!(
                "painted=\"#{:02x}{:02x}{:02x}\"",
                bg.r, bg.g, bg.b
            )),
            "{width}x{height}: the focused field paints the theme's hex: {line}"
        );
        let field = rect_of(&focused, control);
        assert!(
            crate::native_ui::text_field_width_for("Theme (default)", true) > field.width + 0.5,
            "{width}x{height}: the control: \"Theme (default)\" fits {field:?}"
        );
    }
}

#[test]
fn row_names_outrank_their_captions() {
    let cx = view_cx_at(1_284.0, 934.0);
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let style_of = |key: &str| {
        compiled.paint.iter().find_map(|node| match &node.content {
            UiContent::Text(text) if node.key.as_str() == key => Some(text.style),
            _ => None,
        })
    };
    let names: Vec<&str> = compiled
        .paint
        .iter()
        .map(|node| node.key.as_str())
        .filter(|key| key.starts_with(ROW) && key.ends_with("/name"))
        .collect();
    assert!(names.len() > 3, "the chooser paints its rows' names");
    for key in names {
        assert_eq!(style_of(key), Some(StyleRef::Primary), "{key}");
    }
    assert_ne!(
        style_of("settings/showcase/section/builtin"),
        Some(StyleRef::Primary),
        "a caption is quieter than the names under it"
    );
}

#[test]
fn one_check_marks_the_side_the_segment_shows() {
    let cx = view_cx_at(1_284.0, 934.0);
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Hearth,light:Linen\"\n");
    for (side, theme) in [("dark", "hearth"), ("light", "linen")] {
        let _ = act(
            &mut runtime,
            instance,
            view,
            &format!("settings/showcase/side/{side}"),
            None,
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let checked: Vec<&str> = compiled
            .paint
            .iter()
            .filter_map(|node| match &node.content {
                UiContent::Text(text)
                    if node.key.as_str().ends_with("/check") && !text.text.is_empty() =>
                {
                    Some(node.key.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            checked,
            vec![format!("{ROW}{theme}/check").as_str()],
            "{side} side"
        );
    }
}

#[test]
fn the_filter_clears_from_its_own_end() {
    let cx = view_cx_at(1_284.0, 934.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let clear_key = UiKey::new("settings/showcase/filter-clear");
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled.semantic(&clear_key).is_none(),
        "an empty filter has nothing to clear"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("a query long enough to run the whole width of the field"),
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let field = rect_of(&compiled, "settings/showcase/filter");
    let clear = rect_of(&compiled, "settings/showcase/filter-clear");
    assert!(
        clear.x > field.x && clear.right() <= field.right() && clear.y >= field.y,
        "Clear {clear:?} sits over the field's end {field:?}"
    );
    let text_right = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            UiContent::TextField(control) if node.key.as_str() == "settings/showcase/filter" => {
                Some(crate::native_ui::text_field_geometry(control, node.rect, 13.0).text_right)
            }
            _ => None,
        })
        .expect("the filter is painted");
    assert!(
        text_right <= clear.x,
        "the query's text ends at {text_right}, under Clear at {}",
        clear.x
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter-clear",
        None,
    );
    let Some(AppViewState::Settings(state)) = runtime.view_state(view) else {
        unreachable!()
    };
    assert!(state.showcase.filter.value().is_empty(), "Clear empties it");
    assert_eq!(
        state.common.last_focus.as_ref().map(UiKey::as_str),
        Some("settings/showcase/filter"),
        "the field takes the keyboard"
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled.semantic(&clear_key).is_none(),
        "Clear goes with the query"
    );
}

#[test]
fn every_listed_theme_is_in_the_tree_and_only_the_screen_is_live() {
    let cx = view_cx_at(1_284.0, 684.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let top = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    assert!(order.len() > 12, "the list scrolls: {} rows", order.len());
    let check = |compiled: &crate::native_ui::CompiledUi, at: &str| {
        let list = rect_of(compiled, "settings/showcase/list");
        let mut off = 0;
        let mut previous: Option<LogicalRect> = None;
        for slug in &order {
            let key = UiKey::new(format!("{ROW}{slug}"));
            let node = compiled
                .semantic(&key)
                .unwrap_or_else(|| panic!("{at}: {key:?} is in the tree"));
            assert_eq!(node.role, SemanticRole::ListBoxOption, "{at}: {key:?}");
            if let Some(previous) = previous {
                assert!(
                    node.rect.y >= previous.bottom() - 0.5,
                    "{at}: {key:?} {:?} follows the row before it {previous:?}",
                    node.rect
                );
            }
            previous = Some(node.rect);
            let painted = compiled.paint.iter().any(|paint| paint.key == key);
            let hit = compiled.hits.iter().any(|hit| hit.key == key);
            let tabbed = compiled.focus_order.contains(&key);
            if node.offscreen {
                off += 1;
                let whole_on_screen =
                    node.rect.y >= list.y - 0.5 && node.rect.bottom() <= list.bottom() + 0.5;
                assert!(
                    !whole_on_screen,
                    "{at}: {key:?} {:?} is off the list's screen {list:?}, or cut by its edge",
                    node.rect
                );
                let cut = node.rect.y < list.bottom() && node.rect.bottom() > list.y;
                assert!(
                    painted == cut,
                    "{at}: {key:?}: painted {painted}, cut {cut}"
                );
                assert!(!hit && !tabbed, "{at}: {key:?} off screen is live");
            } else {
                assert!(painted, "{at}: {key:?} on screen is painted");
            }
        }
        assert!(off > 0, "{at}: some rows are off the screen");
        compiled
            .validate_parity()
            .expect("the screen keeps paint and hit parity");
    };
    check(&top, "top");
    focus_on(&mut runtime, view, &format!("{ROW}{}", order[0]));
    for _ in 0..order.len() {
        runtime
            .dispatch(instance, view, AppEvent::StepFocus(1))
            .unwrap();
    }
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let first = compiled
        .semantic(&UiKey::new(format!("{ROW}{}", order[0])))
        .expect("the first row is in the tree");
    assert!(
        first.offscreen,
        "at the end, the first row is off the screen"
    );
    check(&compiled, "end");
}

#[test]
fn a_closed_disclosure_opens_for_a_row_it_holds() {
    let cx = view_cx_at(1_284.0, 934.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    assert!(!state.showcase.pack_open, "the disclosure starts closed");
    assert!(
        !state.open_disclosure_holding(&UiKey::new(format!("{ROW}no-such-theme"))),
        "no section holds an unknown row"
    );
    assert!(!state.showcase.pack_open);
    let pack_row = UiKey::new(format!("{ROW}tokyonight-storm"));
    assert!(state.open_disclosure_holding(&pack_row));
    assert!(state.showcase.pack_open, "the disclosure opens");
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled.semantic(&pack_row).is_some(),
        "the community row is listed"
    );
}

#[test]
fn rows_added_over_the_keyboards_row_hold_its_place_and_list_each_row_once() {
    let cx = view_cx_at(1_284.0, 684.0);
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let _ = act(&mut runtime, instance, view, "settings/showcase/pack", None);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    list_at_top(&mut runtime, view);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    let community = |runtime: &NativeRuntime, slug: &str| {
        state(runtime, view)
            .showcase
            .card_by_slug(slug)
            .is_some_and(|card| card.section == Section::Community)
    };
    let first = order
        .iter()
        .position(|slug| community(&runtime, slug))
        .expect("the open disclosure lists Community");
    let _ = dispatch(
        &mut runtime,
        instance,
        view,
        AppEvent::ScrollLines(i32::try_from(first).unwrap() - 2),
    );
    let scrolled = compile_settings_view(&runtime, instance, view, &cx);
    let shown: Vec<String> = scrolled
        .semantics
        .iter()
        .filter(|node| node.role == SemanticRole::ListBoxOption && !node.offscreen)
        .map(|node| node.key.as_str().to_string())
        .filter(|key| community(&runtime, key.strip_prefix(ROW).unwrap_or(key)))
        .collect();
    let target = shown.get(2).expect("Community rows on the screen").clone();
    let _ = act(&mut runtime, instance, view, &target, None);
    let before = compile_settings_view(&runtime, instance, view, &cx);
    let at = rect_of(&before, &target);
    assert!(
        !before
            .semantic(&UiKey::new(target.as_str()))
            .unwrap()
            .offscreen
    );

    let _ = admit_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        vec![user_scheme("Harbour", aterm_types::Appearance::Dark)],
    );
    let after = compile_settings_view(&runtime, instance, view, &cx);
    let listed = state(&runtime, view).showcase.listed();
    let harbour = listed
        .iter()
        .position(|slug| id_of(&runtime, view, slug) == "Harbour")
        .expect("the new theme is listed");
    let held = listed
        .iter()
        .position(|slug| format!("{ROW}{slug}") == target)
        .expect("the row is still listed");
    assert!(harbour < held, "the new row is over the keyboard's");
    let node = after
        .semantic(&UiKey::new(target.as_str()))
        .expect("in the tree");
    assert!(!node.offscreen, "{target} stays on the screen");
    assert_eq!(node.rect, at, "{target} holds its place");
    assert_eq!(
        drawn_rows(&after).len(),
        listed.len(),
        "each listed row is in the tree once"
    );

    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    let (plan, shown) = state.scroll_shown().expect("the render recorded it");
    let asked = state.page_scroll;
    let reseated = state.seat_scroll(&mut plan.clone());
    assert_eq!(
        reseated.start, shown.start,
        "a second seating before any event keeps the place"
    );
    state.scroll_page_to(asked);
    let scrolled = state.seat_scroll(&mut plan.clone());
    assert_ne!(
        scrolled.start, shown.start,
        "the control: scrolled back to where it was asked, the list moves"
    );
}

fn painted_text(compiled: &crate::native_ui::CompiledUi, key: &str) -> Option<String> {
    compiled.paint.iter().find_map(|node| match &node.content {
        UiContent::Text(text) if node.key.as_str() == key => Some(text.text.clone()),
        _ => None,
    })
}

fn has_focus(compiled: &crate::native_ui::CompiledUi, key: &str) -> bool {
    compiled
        .semantic(&UiKey::new(key))
        .and_then(|node| node.state)
        .is_some_and(|state| state.focused)
}

#[test]
fn undo_of_a_keep_takes_the_strip_and_the_keyboard_to_the_restored_theme() {
    let (width, height) = (1_284.0, 714.0);
    let mut lab = super::modified::Lab::new("theme = \"Dracula\"\n");
    lab.state_mut().navigate(SettingsRoute::Themes);
    let _ = lab.compile(width, height);
    let row_of = |lab: &super::modified::Lab, id: &str| {
        let slug = lab
            .state()
            .showcase
            .cards
            .iter()
            .find(|card| card.id == id)
            .map(|card| card.slug.clone())
            .unwrap_or_else(|| panic!("{id} has a row"));
        format!("{ROW}{slug}")
    };
    let nord = row_of(&lab, "Nord");
    let dracula = row_of(&lab, "Dracula");
    let status = |lab: &super::modified::Lab| {
        lab.compile(width, height)
            .semantic(&UiKey::new(STATUS))
            .expect("the strip")
            .label
            .clone()
    };
    lab.dispatch(AppEvent::FocusChanged(Some(UiKey::new(nord.as_str()))));
    assert!(lab.invoke(&nord, None).is_none(), "Return tries");
    let _ = lab.commit(&nord, None);
    assert_eq!(lab.state().configured_theme_id(), "Nord");
    let settled = lab
        .state()
        .showcase
        .settled
        .clone()
        .expect("the Keep settles the strip");
    assert_eq!(settled.id, "Nord");
    assert!(status(&lab).starts_with(&settled.words), "{}", status(&lab));
    assert!(has_focus(&lab.compile(width, height), &nord));

    lab.undo().expect("the Keep is offered for Undo");
    assert_eq!(lab.state().configured_theme_id(), "Dracula");
    let said = status(&lab);
    assert!(!said.contains(&settled.words), "{said}");
    assert!(!said.contains("Nord"), "{said}");
    assert!(
        said.contains(&lab.state().showcase.label_of("Dracula")),
        "{said}"
    );
    assert_eq!(
        lab.state()
            .highlighted_slug()
            .map(|slug| format!("{ROW}{slug}")),
        Some(dracula.clone())
    );
    let compiled = lab.compile(width, height);
    assert!(has_focus(&compiled, &dracula), "the keyboard is on Dracula");
    assert!(!has_focus(&compiled, &nord));
}

#[test]
fn saving_puts_the_keyboard_back_on_save_as_theme() {
    let cx = view_cx_at(1_284.0, 714.0);
    for by_return in [true, false] {
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Nord\"\n");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/save-as",
            None,
        );
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/save-name",
            Some("Nord Mine"),
        );
        let open = compile_settings_view(&runtime, instance, view, &cx);
        assert!(
            has_focus(&open, "settings/showcase/save-name"),
            "{by_return}"
        );
        let saved = if by_return {
            dispatch(
                &mut runtime,
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Submit),
            )
        } else {
            act(&mut runtime, instance, view, "settings/showcase/save", None)
        };
        assert!(
            matches!(
                themes_request(&saved),
                Some((ThemesRequest::Save { .. }, _))
            ),
            "{by_return}: it saves"
        );
        let after = compile_settings_view(&runtime, instance, view, &cx);
        assert!(
            after
                .semantic(&UiKey::new("settings/showcase/save-name"))
                .is_none()
        );
        assert!(
            has_focus(&after, "settings/showcase/save-as"),
            "{by_return}: the keyboard is on Save as theme"
        );
    }
}

#[test]
fn trashing_a_row_puts_the_keyboard_on_its_neighbour() {
    let cx = view_cx_at(1_284.0, 934.0);
    let mine = || {
        vec![
            user_scheme("harbour", aterm_types::Appearance::Dark),
            user_scheme("lagoon", aterm_types::Appearance::Dark),
            user_scheme("paper", aterm_types::Appearance::Light),
        ]
    };
    for (trashed, neighbour) in [("lagoon", "paper"), ("paper", "lagoon")] {
        let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let _ = admit_catalog(&mut service, &mut runtime, instance, view, mine());
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        let row = card_action(&runtime, view, trashed);
        let trash = format!("{row}/trash");
        let _ = act(&mut runtime, instance, view, &trash, None);
        let (request, operation) =
            themes_request(&act(&mut runtime, instance, view, &trash, None)).expect("moved");
        assert!(matches!(request, ThemesRequest::Trash { .. }));
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Trashed {
                    id: trashed.to_string(),
                },
            },
        );
        assert_eq!(
            state(&runtime, view)
                .common
                .last_focus
                .as_ref()
                .map(UiKey::as_str),
            Some(trash.as_str()),
            "{trashed}: listed until the folder is read"
        );
        let left: Vec<_> = mine().into_iter().filter(|(id, _)| id != trashed).collect();
        let _ = admit_catalog(&mut service, &mut runtime, instance, view, left);
        let next = card_action(&runtime, view, neighbour);
        assert_eq!(
            state(&runtime, view)
                .common
                .last_focus
                .as_ref()
                .map(UiKey::as_str),
            Some(next.as_str()),
            "{trashed}"
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        assert!(compiled.semantic(&UiKey::new(row.as_str())).is_none());
        assert!(has_focus(&compiled, &next), "{trashed}: drawn focused");
    }
}

#[test]
fn choosing_a_side_shows_that_sides_theme() {
    let cx = view_cx_at(1_284.0, 684.0);
    let (_service, runtime, instance, view) = themes_view("");
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let dark = state(&runtime, view)
        .showcase
        .listed()
        .into_iter()
        .rev()
        .map(|slug| id_of(&runtime, view, &slug))
        .find(|id| {
            aterm_types::scheme::builtin_theme(id)
                .is_some_and(|theme| theme.appearance == aterm_types::Appearance::Dark)
        })
        .expect("a dark built-in");
    let (_service, mut runtime, instance, view) =
        themes_view(&format!("theme = \"dark:{dark},light:Default Light\"\n"));
    let row = card_action(&runtime, view, &dark);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    list_at_top(&mut runtime, view);
    let before = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        before
            .semantic(&UiKey::new(row.as_str()))
            .is_some_and(|node| node.offscreen),
        "the control: {dark} starts off the screen"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/side/dark",
        None,
    );
    let after = compile_settings_view(&runtime, instance, view, &cx);
    let node = after.semantic(&UiKey::new(row.as_str())).expect("listed");
    assert!(!node.offscreen, "{dark} is on the screen");
    assert!(
        node.state.is_some_and(|state| state.selected),
        "highlighted"
    );
    assert!(
        painted_text(&after, &format!("{row}/check")).is_some_and(|words| words == "\u{2713}"),
        "its ✓ is drawn"
    );
    assert_eq!(
        state(&runtime, view)
            .common
            .last_focus
            .as_ref()
            .map(UiKey::as_str),
        Some("settings/showcase/side/dark")
    );
}

#[test]
fn save_as_suggests_the_next_number_for_a_saved_custom_theme() {
    let none = crate::app_config::ThemeCatalog::from_schemes([]);
    let saved = crate::app_config::ThemeCatalog::from_schemes([user_scheme(
        "Dracula Custom",
        aterm_types::Appearance::Dark,
    )]);
    assert_eq!(suggested_save_name("Dracula", &none), "Dracula Custom");
    assert_eq!(suggested_save_name("Dracula", &saved), "Dracula Custom 2");
    assert_eq!(
        suggested_save_name("Dracula Custom", &saved),
        "Dracula Custom 2"
    );
    assert_eq!(
        suggested_save_name("Dracula Custom 3", &saved),
        "Dracula Custom 2"
    );
    assert_eq!(suggested_save_name("Base 2", &none), "Base 2 Custom");
}

#[test]
fn community_is_a_caption_while_a_filter_is_typed() {
    let cx = view_cx_at(1_284.0, 714.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let open = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        open.semantic(&UiKey::new("settings/showcase/pack"))
            .is_some()
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("nightfox"),
    );
    let filtered = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        filtered
            .semantic(&UiKey::new("settings/showcase/pack"))
            .is_none()
    );
    let caption = filtered
        .semantic(&UiKey::new("settings/showcase/section/community"))
        .expect("Community's caption");
    assert_eq!(caption.role, SemanticRole::Heading);
    let nightfox = card_action(&runtime, view, "Nightfox");
    assert!(filtered.semantic(&UiKey::new(nightfox.as_str())).is_some());
}

#[test]
fn a_needs_attention_row_reads_at_the_columns_edge() {
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(
        dir.path().join("Broken Sunset.conf"),
        "foreground = #dddddd\nbackground = #zzz\n",
    )
    .unwrap();
    for (name, text) in [
        ("From kitty", "foreground #c0caf5\nbackground #1a1b26\n"),
        ("From Ghostty", "background = 1a1b26\npalette = 0=#15161e\n"),
        (
            "From Alacritty",
            "[colors.primary]\nbackground = '#1a1b26'\n",
        ),
        (
            "From iTerm2",
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict/>\n</plist>\n",
        ),
        (
            "From Windows Terminal",
            "{\n  \"name\": \"Night\",\n  \"background\": \"#1a1b26\"\n}\n",
        ),
    ] {
        std::fs::write(dir.path().join(format!("{name}.conf")), text).unwrap();
    }
    for width in SHAPES {
        let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let published = service.replace_theme_catalog(std::sync::Arc::new(
            crate::app_config::ThemeCatalog::discover_in(dir.path()),
        ));
        let _ = dispatch(
            &mut runtime,
            instance,
            view,
            AppEvent::ConfigChanged(published),
        );
        let rows = state(&runtime, view).showcase.attention.clone();
        assert_eq!(rows.len(), 6, "premise: every file needs attention");
        let sunset = rows
            .iter()
            .find(|row| row.id == "Broken Sunset")
            .expect("premise: the broken file's row");
        let row = format!("{ROW}{}", sunset.slug);
        list_at_top(&mut runtime, view);
        let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(width, 934.0));
        for other in rows.iter() {
            let words = painted_text(&compiled, &format!("{ROW}{}/reason", other.slug));
            assert_eq!(
                words.as_deref(),
                Some(other.summary.as_str()),
                "{width}: {}'s few words are cut ({})",
                other.id,
                other.reason
            );
        }
        let painted = |key: &str| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .map(|node| node.rect)
                .unwrap_or_else(|| panic!("{width}: {key} painted"))
        };
        let caption = rect_of(&compiled, "settings/showcase/section/attention");
        let name = painted(&format!("{row}/name"));
        let reason = painted(&format!("{row}/reason"));
        for (what, rect) in [("name", name), ("reason", reason)] {
            assert!(
                (rect.x - caption.x).abs() < 0.5,
                "{width}: {what} {rect:?} beside the caption {caption:?}"
            );
        }
        assert!(
            reason.y >= name.bottom() - 0.5,
            "{width}: the reason is under the name"
        );
        let words = painted_text(&compiled, &format!("{row}/reason")).expect("the reason's words");
        assert!(words.starts_with("Line 2: background"), "{width}: {words}");
        assert!(!words.contains("parse error"), "{width}: {words}");
        let label = &compiled
            .semantic(&UiKey::new(row.as_str()))
            .expect("the row")
            .label;
        assert!(label.contains("theme parse error on line 2"), "{label}");
    }
}

#[test]
fn the_sheet_says_its_colors_draw_on_every_theme() {
    let cx = view_cx_at(1_284.0, 714.0);
    for (background, warned) in [("#fff8e7", true), ("#202020", false)] {
        let (_service, mut runtime, instance, view) = themes_view(&format!(
            "theme = \"dark:Default,light:Default Light\"\nbackground = \"{background}\"\n"
        ));
        let customize = compile_settings_view(&runtime, instance, view, &cx)
            .semantic(&UiKey::new("settings/showcase/customize"))
            .expect("Customize")
            .clone();
        let one = showcase_page::custom_colors(1);
        assert!(customize.label.contains(&one), "{}", customize.label);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        let sheet = compile_settings_view(&runtime, instance, view, &cx);
        let title = &sheet
            .semantic(&UiKey::new("settings/showcase/sheet"))
            .expect("the sheet")
            .label;
        assert_eq!(title, showcase_page::SHEET_TITLE);
        let warning = sheet.semantic(&UiKey::new("settings/showcase/other-side/block"));
        assert_eq!(warning.is_some(), warned, "{background}");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize-done",
            None,
        );
        let nord = card_action(&runtime, view, "Nord");
        let _ = act(&mut runtime, instance, view, &nord, None);
        let strip = compile_settings_view(&runtime, instance, view, &cx)
            .semantic(&UiKey::new(STATUS))
            .expect("the strip")
            .label
            .clone();
        let note = showcase_page::override_note(&state(&runtime, view).showcase_colors())
            .expect("premise: one custom color");
        assert!(note.contains(&one), "{note}");
        assert!(strip.contains(&note), "{strip}");
    }
}

#[test]
fn a_chooser_rows_marks_are_whole_at_every_text_size() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "chooser marks", |scale| {
        let marks = ["/check", "/aa", "/low"];
        let mut drawn = std::collections::BTreeSet::new();
        for (width, height) in [(584.0, 700.0), (1_284.0, 900.0)] {
            let theme = |text: &str| aterm_types::scheme::parse_theme_str(text).expect("a theme");
            let themes = crate::app_config::ThemeCatalog::from_parsed([
                (
                    "Studio".to_string(),
                    theme(
                        "foreground = #e0e0e0\nbackground = #101418\nfont_family = Berkeley Mono\n",
                    ),
                ),
                (
                    "Murky".to_string(),
                    theme("foreground = #555555\nbackground = #444444\nline_height = 1.2\n"),
                ),
            ]);
            let (_service, mut runtime, instance, view) =
                typography_view("theme = \"Murky\"\n", themes);
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.showcase.pack_open = true;
            }
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(width, height),
                |compiled, top| {
                    for node in &compiled.paint {
                        let key = node.key.as_str();
                        let Some(mark) = marks
                            .iter()
                            .find(|mark| key.starts_with(ROW) && key.ends_with(*mark))
                        else {
                            continue;
                        };
                        if let crate::native_ui::UiContent::Text(text) = &node.content
                            && !text.text.is_empty()
                        {
                            drawn.insert(*mark);
                        }
                        let line = compiled.text_fit_audit_line(&node.key).unwrap_or_default();
                        assert!(
                            !line.contains("overflow=true"),
                            "{width}×{height} at {scale}× screen {top}: {line}"
                        );
                    }
                },
            );
        }
        assert_eq!(
            drawn.len(),
            marks.len(),
            "premise: every mark drawn: {drawn:?}"
        );
    });
}

#[test]
fn the_strip_draws_its_title_and_every_button_whole() {
    at_each_text_scale(&[1.0, 1.25, 2.0], "the strip", |scale| {
        for width in [178.0, 206.0, 262.0, 318.0, 444.0, 584.0, 760.0, 1_284.0] {
            let mut trial_heights = std::collections::BTreeSet::new();
            for step in ["idle", "nord", "studio", "more"] {
                let (_service, mut runtime, instance, view) = typography_view(
                    "theme = \"Dracula\"\n",
                    studio_catalog("font_family = Berkeley Mono\nline_height = 1.15\n"),
                );
                let cx = view_cx_at(width, 600.0);
                if step != "idle" {
                    let theme = if step == "nord" { "Nord" } else { "Studio" };
                    let row = card_action(&runtime, view, theme);
                    let _ = act(&mut runtime, instance, view, &row, None);
                }
                if step == "more" {
                    let _ = act(
                        &mut runtime,
                        instance,
                        view,
                        "settings/showcase/keep-more",
                        None,
                    );
                }
                list_at_top(&mut runtime, view);
                {
                    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                        unreachable!()
                    };
                    state.common.last_focus = None;
                }
                let compiled = compile_settings_view(&runtime, instance, view, &cx);
                let context = format!("{width} at {scale}× {step}");
                let title = compiled
                    .paint
                    .iter()
                    .find(|node| node.key.as_str() == "settings/showcase/status/title")
                    .and_then(|node| match &node.content {
                        crate::native_ui::UiContent::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                assert!(!title.is_empty(), "{context}: the strip names the theme");
                let cut: Vec<String> = compiled
                    .fit_failures()
                    .into_iter()
                    .filter(|line| {
                        line.contains("key=\"settings/showcase/status")
                            || line.contains("key=\"settings/showcase/customize")
                            || line.contains("key=\"settings/showcase/keep")
                            || line.contains("key=\"settings/showcase/revert")
                    })
                    .collect();
                assert!(cut.is_empty(), "{context}:\n{}", cut.join("\n"));
                if step == "nord" || step == "studio" {
                    trial_heights.insert(
                        rect_of(&compiled, "settings/showcase/status")
                            .height
                            .to_bits(),
                    );
                }
            }
            assert_eq!(
                trial_heights.len(),
                1,
                "{width} at {scale}×: one tried theme to another keeps the strip's height"
            );
        }
    });
}

#[test]
fn a_chooser_rows_name_comes_before_its_slab() {
    at_each_text_scale(&[1.0, 1.5, 2.0], "row names", |scale| {
        for width in [178.0, 206.0, 262.0, 318.0, 444.0, 584.0, 1_284.0] {
            let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.showcase.pack_open = true;
            }
            let labels: std::collections::BTreeMap<String, String> = state(&runtime, view)
                .showcase
                .cards
                .iter()
                .map(|card| (card.slug.clone(), card.label.clone()))
                .collect();
            let mut slabbed = std::collections::BTreeSet::new();
            each_screen(
                &mut runtime,
                instance,
                view,
                &view_cx_at(width, 900.0),
                |compiled, top| {
                    for node in &compiled.paint {
                        let Some(slug) = node
                            .key
                            .as_str()
                            .strip_prefix(ROW)
                            .and_then(|rest| rest.strip_suffix("/name"))
                        else {
                            continue;
                        };
                        let crate::native_ui::UiContent::Text(text) = &node.content else {
                            continue;
                        };
                        if node.clip.height + 0.5 < node.rect.height {
                            continue;
                        }
                        let slab = compiled
                            .paint
                            .iter()
                            .any(|other| other.key.as_str() == format!("{ROW}{slug}/slab"));
                        let first = labels
                            .get(slug)
                            .and_then(|label| label.split_whitespace().next())
                            .unwrap_or_default();
                        assert!(
                            !(slab && !text.text.starts_with(first)),
                            "{width} at {scale}× screen {top}: {slug} paints {:?} beside its slab",
                            text.text
                        );
                        slabbed.insert(slab);
                    }
                },
            );
            assert!(
                slabbed.len() <= 1,
                "{width} at {scale}×: every row keeps one form"
            );
        }
    });
}

#[test]
fn the_themes_page_opens_on_the_theme_in_use() {
    for (width, height) in [(864.0, 462.0), (1_144.0, 462.0)] {
        let cx = view_cx_at(width, height);
        for source in ["theme = \"One Dark\"\n", ""] {
            let (_service, mut runtime, instance, view) = themes_view(source);
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let slug = state(&runtime, view)
                .highlighted_slug()
                .expect("a highlighted row");
            let key = format!("settings/showcase/card/{slug}");
            assert!(
                whole_in_list(&compiled, &key),
                "{width}x{height} {source:?}: {key} is whole in the list"
            );
            if source.is_empty() {
                continue;
            }
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.scroll_page_to(0);
                state.page_reveal = None;
                state.scroll_record.replace(None);
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            assert!(
                !whole_in_list(&compiled, &key),
                "{width}x{height}: the control shows {key} from the top"
            );
        }
    }
}

#[test]
fn the_page_keys_show_every_chooser_row_whole() {
    let whole = |node: &crate::native_ui::PaintNode| {
        !node.peek
            && node.clip.y <= node.rect.y + 0.5
            && node.clip.bottom() + 0.5 >= node.rect.bottom()
    };
    at_each_text_scale(&[1.0, 2.0], "chooser page keys", |scale| {
        for (width, height) in [
            (738.0, 418.0),
            (584.0, 348.0),
            (1_074.0, 684.0),
            (1_564.0, 852.0),
        ] {
            let cx = view_cx_at(width, height);
            let (_service, mut runtime, instance, view) = themes_view("theme = \"Default\"\n");
            let _ = compile_settings_view(&runtime, instance, view, &cx);
            let shown = state(&runtime, view);
            let order: Vec<String> = shown
                .showcase
                .visible()
                .into_iter()
                .map(|index| shown.showcase.cards[index].slug.clone())
                .collect();
            let _ = act(
                &mut runtime,
                instance,
                view,
                &format!("settings/showcase/card/{}", order[0]),
                None,
            );
            for direction in [1, -1] {
                let mut seen = std::collections::BTreeSet::new();
                let place = |runtime: &NativeRuntime| {
                    let shown = state(runtime, view);
                    (shown.highlighted_slug(), shown.common.last_focus.clone())
                };
                let mut at = place(&runtime);
                let start = at.0.clone();
                let mut scroll = state(&runtime, view).page_scroll;
                let look =
                    |runtime: &NativeRuntime, seen: &mut std::collections::BTreeSet<String>| {
                        let compiled = compile_settings_view(runtime, instance, view, &cx);
                        for node in &compiled.paint {
                            let key = node.key.as_str();
                            if let Some(slug) = key.strip_prefix("settings/showcase/card/")
                                && !slug.contains('/')
                                && whole(node)
                            {
                                seen.insert(slug.to_string());
                            }
                            if key == "settings/showcase/pack" && whole(node) {
                                seen.insert(key.to_string());
                            }
                        }
                    };
                look(&runtime, &mut seen);
                for _ in 0..=order.len() + 4 {
                    runtime
                        .dispatch(instance, view, AppEvent::ScrollPage(direction))
                        .unwrap();
                    look(&runtime, &mut seen);
                    let next = place(&runtime);
                    let turned = state(&runtime, view).page_scroll;
                    if next == at && turned == scroll {
                        break;
                    }
                    (at, scroll) = (next, turned);
                }
                let end = if direction > 0 {
                    &order[order.len() - 1]
                } else {
                    &order[0]
                };
                assert_eq!(
                    at.0.as_ref(),
                    Some(end),
                    "{width}×{height} at {scale}×: paging {direction} ends on the list's end"
                );
                let never: Vec<&String> = order
                    .iter()
                    .filter(|slug| direction > 0 || Some(*slug) != start.as_ref())
                    .filter(|slug| !seen.contains(*slug))
                    .collect();
                assert!(
                    never.is_empty(),
                    "{width}×{height} at {scale}×, paging {direction}: never whole: {never:?}"
                );
                if direction > 0 {
                    assert!(
                        seen.contains("settings/showcase/pack"),
                        "{width}×{height} at {scale}×: Page Down past the rows shows the disclosure"
                    );
                }
            }
        }
    });
}

fn whole_and_cut_rows(compiled: &crate::native_ui::CompiledUi) -> (Vec<String>, Vec<String>) {
    let list = rect_of(compiled, showcase_page::SHOWCASE_LIST);
    let options = drawn_rows(compiled);
    let (mut whole, mut cut) = (Vec::new(), Vec::new());
    for paint in compiled
        .paint
        .iter()
        .filter(|paint| options.iter().any(|key| key == paint.key.as_str()))
        .filter(|paint| paint.clip.height > 0.0)
    {
        let inside = paint.rect.y >= list.y - 0.01 && paint.rect.bottom() <= list.bottom() + 0.01;
        if paint.clip == paint.rect && inside {
            whole.push(paint.key.as_str().to_string());
        } else {
            cut.push(paint.key.as_str().to_string());
        }
    }
    (whole, cut)
}

#[test]
fn the_default_window_shows_four_whole_theme_rows_beside_the_demo() {
    for source in ["theme = \"Default\"\n", "theme = \"Dracula\"\n"] {
        let (_service, mut runtime, instance, view) = themes_view(source);
        let cx = view_cx_at(584.0, 350.0);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let (whole, cut) = whole_and_cut_rows(&compiled);
        assert!(whole.len() >= 4, "{source}: {whole:?}");
        assert!(cut.is_empty(), "{source}: rows cut mid-row: {cut:?}");
        let stage = rect_of(&compiled, STAGE);
        let chooser = rect_of(&compiled, "settings/showcase/chooser");
        let strip = rect_of(&compiled, STATUS);
        assert!(
            stage.x >= chooser.right(),
            "{source}: {stage:?} {chooser:?}"
        );
        assert!(stage.width >= 40.0 * 12.0 * 0.6, "{source}: {stage:?}");
        assert!(stage.bottom() <= strip.y, "{source}: {stage:?} {strip:?}");
        assert!(
            (strip.bottom() - chooser.bottom()).abs() < 0.01,
            "{source}: {strip:?} {chooser:?}"
        );
        let order = drawn_rows(&compiled);
        let at = order
            .iter()
            .position(|key| *key == whole[0])
            .expect("a whole row is drawn");
        let next = compiled
            .semantic(&UiKey::new(order[at + 1].as_str()))
            .expect("the row under it")
            .label
            .clone();
        focus_on(&mut runtime, view, &whole[0]);
        let _ = dispatch(&mut runtime, instance, view, AppEvent::StepFocus(1));
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let demo = compiled.semantic(&UiKey::new(STAGE)).expect("the demo");
        assert!(
            format!("{:?}", demo.value).contains(next.as_str()),
            "{source}: the demo draws {next}: {:?}",
            demo.value
        );
        assert!(rect_of(&compiled, STAGE).x >= chooser.right());
    }
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(584.0, 574.0));
    let stage = rect_of(&compiled, STAGE);
    let chooser = rect_of(&compiled, "settings/showcase/chooser");
    assert!(stage.bottom() <= chooser.y, "tall: {stage:?} {chooser:?}");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_144.0, 434.0));
    let list = rect_of(&compiled, showcase_page::SHOWCASE_LIST);
    let follow = rect_of(&compiled, "settings/showcase/footer/follow");
    assert!(
        rect_of(&compiled, STAGE).x >= rect_of(&compiled, "settings/showcase/chooser").right(),
        "wide"
    );
    assert!(follow.y >= list.bottom(), "wide: {follow:?} under {list:?}");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(514.0, 348.0));
    let (whole, _) = whole_and_cut_rows(&compiled);
    assert!(
        rect_of(&compiled, STAGE).bottom() <= rect_of(&compiled, "settings/showcase/chooser").y,
        "the control is stacked"
    );
    assert!(whole.len() < 4, "the control: {whole:?}");
}

fn page_shape(compiled: &crate::native_ui::CompiledUi) -> &'static str {
    let chooser = rect_of(compiled, "settings/showcase/chooser");
    let list = rect_of(compiled, showcase_page::SHOWCASE_LIST);
    let beside = compiled
        .semantic(&UiKey::new(STAGE))
        .is_some_and(|stage| stage.rect.x >= chooser.right() - 0.01);
    let pinned = compiled
        .semantic(&UiKey::new("settings/showcase/footer/follow"))
        .is_some_and(|follow| !follow.offscreen && follow.rect.y >= list.bottom() - 0.5);
    match (beside, pinned) {
        (true, true) => "Beside",
        (true, false) => "Short",
        (false, _) => "Stacked",
    }
}

const CHOOSER_ROW: f32 = 32.0;

#[test]
fn the_themes_page_changes_shape_once_as_the_window_grows_taller() {
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    for (width, after) in [
        (584.0_f32, "Stacked"),
        (668.0, "Stacked"),
        (1_144.0, "Beside"),
    ] {
        let mut runs: Vec<&str> = Vec::new();
        for height in (240..=760).step_by(2).map(|h| h as f32) {
            let compiled =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
            let shape = page_shape(&compiled);
            if runs.is_empty() && shape == "Stacked" {
                continue;
            }
            if runs.last() != Some(&shape) {
                assert!(
                    !runs.contains(&shape),
                    "{width}x{height}: {shape} again after {runs:?}"
                );
                runs.push(shape);
            }
            if shape == "Stacked" {
                let list = rect_of(&compiled, showcase_page::SHOWCASE_LIST);
                assert!(
                    list.height + 0.5 >= 4.0 * CHOOSER_ROW,
                    "{width}x{height}: the stacked list {list:?} keeps four rows' room"
                );
            }
        }
        assert_eq!(runs, ["Short", after], "{width}: the shapes, short to tall");
    }
}

#[test]
fn widening_the_window_never_takes_rows_from_the_chooser() {
    let (_service, runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    for height in [348.0_f32, 434.0] {
        let mut beside_from = None;
        let mut last: Option<(f32, f32)> = None;
        for width in (514..=1_300).step_by(7).map(|w| w as f32) {
            let at = format!("{width}x{height}");
            let compiled =
                compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
            if page_shape(&compiled) == "Stacked" {
                assert!(
                    beside_from.is_none(),
                    "{at}: stacked again after {beside_from:?}"
                );
                last = None;
                continue;
            }
            beside_from.get_or_insert(width);
            let stage = rect_of(&compiled, STAGE);
            assert!(stage.width + 0.01 >= 40.0 * 12.0 * 0.6, "{at}: {stage:?}");
            let list = rect_of(&compiled, showcase_page::SHOWCASE_LIST);
            let rows = list.height / CHOOSER_ROW;
            assert!(
                (rows - rows.round()).abs() < 0.01,
                "{at}: the list {list:?} is whole rows tall"
            );
            let (_, cut) = whole_and_cut_rows(&compiled);
            assert!(cut.is_empty(), "{at}: rows cut: {cut:?}");
            let top = rect_of(&compiled, "settings/showcase/chooser").y;
            if let Some((was_top, was_rows)) =
                last.filter(|(was_top, _)| (was_top - top).abs() < 0.01)
            {
                assert!(
                    rows.round() >= was_rows.min(4.0),
                    "{at}: {rows} rows where {was_rows} stood narrower ({was_top})"
                );
            }
            last = Some((top, rows.round()));
        }
        assert_eq!(
            beside_from,
            Some(528.0),
            "{height}: the demo beside from 72 columns"
        );
    }
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(514.0, 348.0));
    assert_eq!(page_shape(&compiled), "Stacked");
}

fn cut_by_the_list(compiled: &crate::native_ui::CompiledUi, key: &str) -> Vec<String> {
    let list = rect_of(compiled, key);
    compiled
        .paint
        .iter()
        .filter(|node| {
            !node.clip.is_empty()
                && node.key.as_str() != key
                && !node.key.as_str().starts_with(&format!("{key}/"))
                && node.clip.x >= list.x - 0.5
                && node.clip.right() <= list.right() + 0.5
                && node.clip.y >= list.y - 0.5
                && node.clip.bottom() <= list.bottom() + 0.5
        })
        .filter(|node| node.rect.y < list.y - 0.5 || node.rect.bottom() > list.bottom() + 0.5)
        .map(|node| format!("{} {:?}", node.key.as_str(), node.rect))
        .collect()
}

fn cut_under_a_caption(compiled: &crate::native_ui::CompiledUi, key: &str) -> Vec<String> {
    let list = rect_of(compiled, key);
    let inside = |rect: &LogicalRect| {
        rect.x >= list.x - 0.5
            && rect.right() <= list.right() + 0.5
            && rect.y >= list.y - 0.5
            && rect.bottom() <= list.bottom() + 0.5
    };
    let Some(rows) = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str().ends_with("/scrolled-rows") && inside(&node.rect))
        .map(|node| node.rect)
    else {
        return Vec::new();
    };
    compiled
        .paint
        .iter()
        .filter(|node| !node.clip.is_empty() && inside(&node.clip))
        .filter(|node| node.rect.y < rows.y - 0.5 && node.clip.y >= rows.y - 0.5)
        .map(|node| format!("{} {:?} under {rows:?}", node.key.as_str(), node.rect))
        .collect()
}

fn captions_over_no_colour(compiled: &crate::native_ui::CompiledUi, key: &str) -> Vec<String> {
    let list = rect_of(compiled, key);
    let painted: Vec<&str> = compiled
        .paint
        .iter()
        .filter(|node| {
            !node.clip.is_empty()
                && node.clip.x >= list.x - 0.5
                && node.clip.right() <= list.right() + 0.5
                && node.clip.y >= list.y - 0.5
                && node.clip.bottom() <= list.bottom() + 0.5
        })
        .map(|node| node.key.as_str())
        .collect();
    let colour_rows: Vec<String> = showcase_page::SHOWCASE_COLOR_KEYS
        .iter()
        .map(|key| format!("settings/control/{key}"))
        .collect();
    let sections: [(&str, Vec<String>); 2] = [
        (
            "settings/showcase/palette-section/heading",
            vec!["settings/showcase/palette/".to_string()],
        ),
        ("settings/showcase/colours/heading", colour_rows),
    ];
    sections
        .into_iter()
        .filter(|(heading, _)| painted.contains(heading))
        .filter(|(_, rows)| {
            !painted.iter().any(|key| {
                rows.iter().any(|row| {
                    key.strip_prefix(row.as_str()).is_some_and(|rest| {
                        row.ends_with('/') || rest.is_empty() || rest.starts_with('/')
                    })
                })
            })
        })
        .map(|(heading, _)| format!("{heading} over none of its rows: {painted:?}"))
        .collect()
}

#[test]
fn no_stop_is_cut_on_any_screen() {
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(dir.path().join("Broken.conf"), "foreground = \"#zz\"\n=\n").unwrap();
    std::fs::write(
        dir.path().join("Harbour.conf"),
        "foreground = #dddddd\nbackground = #101218\n",
    )
    .unwrap();
    let check = |width: f32, height: f32, form: &str, stacked: bool| -> (Vec<String>, [bool; 3]) {
        let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let cx = view_cx_at(width, height);
        let mut list = showcase_page::SHOWCASE_LIST;
        let mut panel = "settings/showcase/chooser";
        match form {
            "community" => {
                let _ = act(&mut runtime, instance, view, "settings/showcase/pack", None);
            }
            "attention" => {
                let published = service.replace_theme_catalog(std::sync::Arc::new(
                    crate::app_config::ThemeCatalog::discover_in(dir.path()),
                ));
                let _ = dispatch(
                    &mut runtime,
                    instance,
                    view,
                    AppEvent::ConfigChanged(published),
                );
                assert!(!state(&runtime, view).showcase.attention.is_empty());
            }
            "sheet" => {
                let _ = act(
                    &mut runtime,
                    instance,
                    view,
                    "settings/showcase/customize",
                    None,
                );
                list = "settings/showcase/sheet-list";
                panel = "settings/showcase/sheet";
            }
            _ => {}
        }
        let at = format!("{width}x{height} {form}");
        let mut faults = Vec::new();
        let mut rooms = Vec::new();
        let mut last_bare = Vec::new();
        let mut last_palette = false;
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let stage = rect_of(compiled, STAGE);
            assert_eq!(
                stage.bottom() <= rect_of(compiled, panel).y + 0.5,
                stacked,
                "{at}: the demo over the list"
            );
            faults.extend(
                cut_by_the_list(compiled, list)
                    .into_iter()
                    .chain(cut_under_a_caption(compiled, list))
                    .map(|node| format!("{at} screen {top}: {node}")),
            );
            rooms.push((top, rect_of(compiled, list).height));
            if form == "sheet" {
                last_bare = captions_over_no_colour(compiled, list)
                    .into_iter()
                    .map(|caption| format!("{at} last screen {top}: {caption}"))
                    .collect();
                last_palette = compiled.paint.iter().any(|node| {
                    node.key.as_str() == "settings/showcase/palette-section/heading"
                        && !node.clip.is_empty()
                });
            }
        });
        faults.extend(last_bare);
        let plan = state(&runtime, view)
            .scroll_plan()
            .expect("the list's plan");
        let cuts = |plan: &scroll::ScrollPlan| {
            (0..=plan.limit())
                .map(|top| (top, plan.screen(top)))
                .filter(|(_, screen)| screen.drawn != screen.view || screen.offset.abs() > 0.01)
                .map(|(top, screen)| {
                    format!("{at} screen {top} draws more than it shows: {screen:?}")
                })
                .collect::<Vec<_>>()
        };
        faults.extend(cuts(&plan));
        let fits = |stops: std::ops::Range<usize>, room: f32| plan.span(stops) <= room + 0.5;
        let mut short_flagged = false;
        for (top, room) in rooms {
            let drawn = plan.screen(top).drawn;
            if drawn.end < plan.len() && fits(drawn.start..drawn.end + 1, room) {
                faults.push(format!(
                    "{at} screen {top} leaves stop {} undrawn, laid out ending {} down a \
                     list {room} tall",
                    drawn.end,
                    plan.span(drawn.start..drawn.end + 1)
                ));
            }
            assert!(
                drawn.len() < 2 || fits(drawn.clone(), room),
                "{at} screen {top}: {drawn:?} laid out ends past a list {room} tall"
            );
            short_flagged |= drawn.len() >= 2;
        }
        let mut peek = plan;
        peek.set_foot(scroll::ListFoot::Peek);
        (
            faults,
            [!cuts(&peek).is_empty(), short_flagged, last_palette],
        )
    };
    let mut faults = Vec::new();
    let mut controls = Vec::new();
    let mut short_flagged = false;
    for (width, height, stacked) in [
        (584.0, 350.0, false),
        (675.0, 448.0, false),
        (766.0, 348.0, false),
        (1_144.0, 448.0, false),
        (514.0, 348.0, true),
        (584.0, 516.0, true),
        (584.0, 530.0, true),
        (584.0, 558.0, true),
    ] {
        let mut peek_cuts = false;
        for form in ["open", "community", "attention", "sheet"] {
            let (cut, [peek, short, last_palette]) = check(width, height, form, stacked);
            faults.extend(cut);
            peek_cuts |= peek;
            short_flagged |= short;
            if form == "sheet"
                && [(766.0, 348.0), (584.0, 530.0)].contains(&(width, height))
                && !last_palette
            {
                controls.push(format!(
                    "{width}x{height}'s last sheet screen paints PALETTE"
                ));
            }
        }
        if !peek_cuts {
            controls.push(format!("{width}x{height} ending on a peek cuts a stop"));
        }
    }
    if !short_flagged {
        controls.push("some screen one stop short leaves out a stop that fits".to_string());
    }
    assert!(faults.is_empty(), "{faults:#?}");
    assert!(controls.is_empty(), "the controls: {controls:#?}");
}

fn land_writes(
    service: &mut VersionedConfigService,
    runtime: &mut NativeRuntime,
    instance: crate::native_app::AppInstanceId,
    view: crate::native_app::ViewId,
    outcome: &crate::native_app::DispatchOutcome,
) {
    for (patch, operation) in config_patches(outcome) {
        let (published, undo) = apply_real_config_patch(service, &patch);
        let _ = dispatch(
            runtime,
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: ConfigPatchOutcome::Applied {
                    revision: published.revision,
                    undo,
                },
            },
        );
        let _ = dispatch(runtime, instance, view, AppEvent::ConfigChanged(published));
    }
}

#[test]
fn opening_customize_never_squeezes_or_moves_the_demo() {
    let painted = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled.paint.iter().find_map(|node| match &node.content {
            UiContent::Text(text) if node.key.as_str() == key => {
                Some((text.text.clone(), node.rect))
            }
            _ => None,
        })
    };
    let forms = |compiled: &crate::native_ui::CompiledUi, at: &str| {
        let mut forms = Vec::new();
        for key in showcase_page::SHOWCASE_COLOR_KEYS {
            let control = format!("settings/control/{key}");
            let field = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == control)
                .map(|node| node.rect);
            let (Some((name, label)), Some(field)) =
                (painted(compiled, &format!("settings/label/{key}")), field)
            else {
                continue;
            };
            assert!(
                !name.is_empty() && !name.contains('\u{2026}'),
                "{at}: {key}'s name painted {name:?}"
            );
            let beside = (label.y - field.y).abs() < 0.5 && label.right() <= field.x + 0.5;
            let over = label.bottom() <= field.y + 0.5 && label.x <= field.x + 0.5;
            assert!(
                beside || over,
                "{at}: {key}'s name {label:?} beside or over its field {field:?}"
            );
            forms.push((key.to_string(), beside));
        }
        forms
    };
    let unmoved = |before: LogicalRect, after: LogicalRect| {
        (after.x - before.x).abs() < 0.01
            && (after.y - before.y).abs() < 0.01
            && (after.width - before.width).abs() < 0.01
            && (after.height - before.height).abs() < 0.01
    };
    let set = "theme = \"Dracula\"\ncursor_color = \"#ff8800\"\n";
    let mut seen_forms = std::collections::BTreeMap::new();
    for source in ["theme = \"Dracula\"\n", set] {
        for (width, height) in [
            (528.0_f32, 348.0_f32),
            (556.0, 348.0),
            (584.0, 348.0),
            (766.0, 348.0),
            (885.0, 348.0),
            (1_152.0, 420.0),
            (1_074.0, 684.0),
        ] {
            let at = format!(
                "{width}x{height}{}",
                if source == set { " set" } else { "" }
            );
            let (mut service, mut runtime, instance, view) = themes_view(source);
            let cx = view_cx_at(width, height);
            let chooser = compile_settings_view(&runtime, instance, view, &cx);
            assert_ne!(page_shape(&chooser), "Stacked", "{at}: premise");
            let before = rect_of(&chooser, STAGE);
            let panel = rect_of(&chooser, "settings/showcase/chooser");
            if (width, height) == (885.0, 348.0) || (width, height) == (1_152.0, 420.0) {
                assert!(panel.width > 232.0 + 1.0, "{at}: premise: {panel:?}");
            }
            let _ = act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/customize",
                None,
            );
            let _ = compile_settings_view(&runtime, instance, view, &cx);
            let sheet = compile_settings_view(&runtime, instance, view, &cx);
            let after = rect_of(&sheet, STAGE);
            assert!(
                unmoved(before, after),
                "{at}: the demo {after:?} where the chooser left it {before:?}"
            );
            assert!(
                after.width + 0.01 >= 40.0 * 12.0 * 0.6,
                "{at}: the demo {after:?} keeps 40 cells"
            );
            let opened = forms(&sheet, &at);
            assert!(!opened.is_empty(), "{at}: the sheet draws colour rows");
            seen_forms.insert((width as u32, height as u32), opened.clone());
            let (title, _) =
                painted(&sheet, "settings/showcase/sheet-title").expect("the sheet's title");
            assert!(!title.contains('\u{2026}'), "{at}: the title {title:?}");
            let steps = if source == set {
                [
                    ("settings/reset/cursor_color", None),
                    ("settings/set/cursor_color", Some("#ff8800")),
                ]
            } else {
                [
                    ("settings/set/cursor_color", Some("#ff8800")),
                    ("settings/reset/cursor_color", None),
                ]
            };
            for (action, value) in steps {
                let outcome = act(&mut runtime, instance, view, action, value);
                assert!(
                    !config_patches(&outcome).is_empty(),
                    "{at}: {action} writes"
                );
                land_writes(&mut service, &mut runtime, instance, view, &outcome);
                let _ = compile_settings_view(&runtime, instance, view, &cx);
                let now = compile_settings_view(&runtime, instance, view, &cx);
                let moved = rect_of(&now, STAGE);
                assert!(
                    unmoved(before, moved),
                    "{at} after {action}: the demo {moved:?} where it stood {before:?}"
                );
                let now_forms = forms(&now, &at);
                let both: Vec<_> = now_forms
                    .iter()
                    .filter(|row| opened.iter().any(|was| was.0 == row.0))
                    .collect();
                assert!(
                    !both.is_empty(),
                    "{at} after {action}: {now_forms:?} {opened:?}"
                );
                for (key, beside) in both {
                    assert!(
                        opened.contains(&(key.clone(), *beside)),
                        "{at} after {action}: {key}'s form changed ({opened:?} then {now_forms:?})"
                    );
                }
                assert_eq!(
                    now.semantic(&UiKey::new("settings/reset/cursor_color"))
                        .is_some(),
                    action.starts_with("settings/set/"),
                    "{at} after {action}: Reset shows while the colour is set"
                );
            }
        }
    }
    let all =
        |size: (u32, u32), beside: bool| seen_forms[&size].iter().all(|(_, form)| *form == beside);
    assert!(
        all((584, 348), false),
        "584x348: {:?}",
        seen_forms[&(584, 348)]
    );
    assert!(
        all((885, 348), true),
        "885x348: {:?}",
        seen_forms[&(885, 348)]
    );
}

#[test]
fn no_screen_of_the_sheet_is_mostly_blank() {
    let list = "settings/showcase/sheet-list";
    let blanks = |width: f32, slot: bool| {
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let cx = view_cx_at(width, 348.0);
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        if slot {
            let _ = act(
                &mut runtime,
                instance,
                view,
                "settings/showcase/palette/4",
                None,
            );
        }
        let (mut caption, mut row) = (0.0_f32, 0.0_f32);
        let mut most = (0.0_f32, 0);
        let screens = each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let room = rect_of(compiled, list);
            if let Some(foreground) = compiled.semantics.iter().find(|node| {
                node.key.as_str().starts_with("settings/row/")
                    && node.key.as_str().ends_with(prefs::EDIT_FOREGROUND)
            }) {
                row = row.max(foreground.rect.height);
            }
            if let Some(heading) = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == "settings/showcase/colours/heading")
            {
                caption = caption.max(heading.rect.height);
            }
            let drawn_to = compiled
                .paint
                .iter()
                .filter(|node| {
                    !node.clip.is_empty()
                        && node.key.as_str() != list
                        && !node.key.as_str().starts_with(&format!("{list}/"))
                        && node.clip.x >= room.x - 0.5
                        && node.clip.right() <= room.right() + 0.5
                        && node.clip.y >= room.y - 0.5
                        && node.clip.bottom() <= room.bottom() + 0.5
                })
                .map(|node| node.clip.bottom())
                .fold(room.y, f32::max);
            let blank = room.bottom() - drawn_to;
            if blank > most.0 {
                most = (blank, top);
            }
        });
        assert!(screens > 2, "{width}: the sheet scrolls");
        assert!(caption > 0.0 && row > 0.0, "{width}: COLORS over its rows");
        let bound = caption + 4.0 + row + 4.0 + 6.0;
        (most, bound)
    };
    for width in [528.0_f32, 584.0, 766.0, 885.0] {
        let ((blank, top), bound) = blanks(width, false);
        assert!(
            blank < bound,
            "{width}x348: screen {top} ends {blank}pt short of its foot (bound {bound})"
        );
    }
    let ((blank, top), bound) = blanks(584.0, true);
    assert!(
        blank >= bound,
        "the control: screen {top} ends {blank}pt short (bound {bound})"
    );
}

#[test]
fn palette_chips_stand_a_target_apart() {
    let chips_in = |compiled: &crate::native_ui::CompiledUi| -> Vec<(usize, LogicalRect)> {
        compiled
            .hits
            .iter()
            .filter_map(|hit| {
                let index = hit
                    .key
                    .as_str()
                    .strip_prefix("settings/showcase/palette/")?
                    .parse::<usize>()
                    .ok()?;
                Some((index, hit.rect))
            })
            .collect()
    };
    let open = |width: f32, height: f32| {
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let _ = act(
            &mut runtime,
            instance,
            view,
            "settings/showcase/customize",
            None,
        );
        (runtime, instance, view, view_cx_at(width, height))
    };
    for width in [528.0_f32, 556.0] {
        let (mut runtime, instance, view, cx) = open(width, 348.0);
        let mut whole = std::collections::BTreeSet::new();
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let chips = chips_in(compiled);
            for (index, rect) in &chips {
                // Live over its whole height: no edge of the list cuts it.
                if rect.height + 0.01 >= 24.0 {
                    whole.insert(*index);
                }
                for (other, near) in &chips {
                    if other <= index {
                        continue;
                    }
                    let (dx, dy) = (
                        (rect.x + rect.width / 2.0) - (near.x + near.width / 2.0),
                        (rect.y + rect.height / 2.0) - (near.y + near.height / 2.0),
                    );
                    let pitch = dx.hypot(dy);
                    assert!(
                        pitch + 0.01 >= 24.0,
                        "{width}x348 screen {top}: chips {index} and {other} {pitch}pt apart: \
                         {rect:?} {near:?}"
                    );
                }
            }
            if width == 528.0
                && let Some(row) = compiled.semantic(&UiKey::new("settings/showcase/palette-row/0"))
            {
                assert!(
                    (row.rect.width + 6.0) / 8.0 < 24.0,
                    "premise: {:?}",
                    row.rect
                );
            }
        });
        assert_eq!(whole.len(), 16, "{width}x348: chips drawn whole {whole:?}");
    }
    let (mut runtime, instance, view, cx) = open(1_144.0, 448.0);
    let mut most = 0;
    each_screen(&mut runtime, instance, view, &cx, |compiled, _| {
        let chips = chips_in(compiled);
        for (_, rect) in &chips {
            let in_row = chips
                .iter()
                .filter(|(_, other)| (other.y - rect.y).abs() < 0.5)
                .count();
            most = most.max(in_row);
        }
    });
    assert_eq!(most, 8, "the control: eight chips to a row");
}

/// The settings list's keeping rules, swept.
#[path = "native_settings_showcase_keeping_tests.rs"]
mod keeping;
