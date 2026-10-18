// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes (the Theme Showcase): the gallery's order and filters,
//! the trial lane's two endings (one write on Keep; no write, byte for byte,
//! on every way of reverting), Save as theme's round trip, the page's rows in
//! the compiled tree, and the card tiles' paint discipline.

use super::*;
use crate::settings_preview::theme_card::ShowcaseTier;
use showcase_page::{
    ShowcaseChip, ShowcaseState, ShowcaseTrial, carried_override_keys, palette_reset, palette_with,
    save_as_text, save_name_problem,
};

/// A Settings view on the Themes page over `source`, and the config service
/// that owns `source`.
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
            AppEffect::ConfigPatch { patch, reply } => Some((patch.clone(), reply.operation)),
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

/// Dark and Light split the gallery between them with nothing in both;
/// Yours is exactly the person's files; the filter narrows each chip.
#[test]
fn showcase_chips_filter_by_appearance_and_origin() {
    let themes = crate::app_config::ThemeCatalog::from_schemes([
        user_scheme("harbour", aterm_types::Appearance::Dark),
        user_scheme("paper", aterm_types::Appearance::Light),
    ]);
    let mut showcase = ShowcaseState::new(&themes);
    let shown = |showcase: &mut ShowcaseState, chip| {
        showcase.chip = chip;
        showcase
            .visible()
            .into_iter()
            .map(|index| showcase.cards[index].id.clone())
            .collect::<BTreeSet<_>>()
    };
    let all = shown(&mut showcase, ShowcaseChip::All);
    let dark = shown(&mut showcase, ShowcaseChip::Dark);
    let light = shown(&mut showcase, ShowcaseChip::Light);
    let yours = shown(&mut showcase, ShowcaseChip::Yours);
    let drawable = showcase
        .cards
        .iter()
        .filter(|card| card.tier != ShowcaseTier::Pack)
        .count();
    assert_eq!(all.len(), drawable);
    assert!(dark.is_disjoint(&light));
    assert_eq!(dark.union(&light).cloned().collect::<BTreeSet<_>>(), all);
    assert_eq!(
        yours,
        ["harbour", "paper"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert!(dark.contains("Nord") && light.contains("Solarized Light"));

    showcase.filter = crate::native_text_input::TextInputState::new("solarized".to_string());
    for chip in ShowcaseChip::ALL {
        let filtered = shown(&mut showcase, chip);
        let unfiltered = {
            let filter = std::mem::replace(
                &mut showcase.filter,
                crate::native_text_input::TextInputState::new(String::new()),
            );
            let all = shown(&mut showcase, chip);
            showcase.filter = filter;
            all
        };
        assert!(filtered.is_subset(&unfiltered), "{chip:?}");
        assert!(
            filtered.iter().all(|id| id.contains("Solarized")),
            "{chip:?}: {filtered:?}"
        );
    }
    showcase.filter = crate::native_text_input::TextInputState::new("light".to_string());
    assert!(shown(&mut showcase, ShowcaseChip::All).contains("paper"));
}

/// Trying a theme writes nothing and asks the host to draw it; Keep (here,
/// activating the tried card again) writes `theme` exactly once, and the
/// trial ends when that write lands.
#[test]
fn showcase_try_then_keep_writes_config_once() {
    // One theme always, so Keep writes the name (a theme that follows the
    // system keeps one side: below).
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
    // A second Keep while it is in flight writes nothing more.
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

/// While the theme follows the system, Keep writes only the side the page
/// shows; the other side stays as configured.
#[test]
fn showcase_keep_on_a_split_writes_only_the_shown_side() {
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Nord,light:Solarized Light\"\n");
    // The view has seen a light OS: the light side is the one shown.
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

/// With no theme set (Default, one theme), Keep writes the kept theme's own
/// name, for whichever side the page shows, and the status names it alone:
/// keeping never starts following the OS. Negative control: a pair the
/// person chose keeps having only its shown side replaced.
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

    // Once it lands, the status names the one theme.
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

/// A Settings search typed on the Themes page shows its results in the
/// page's place, so the card the keyboard was on is no longer what the
/// preview draws: the search page's preview names the configured theme, not
/// a card that is off screen. Negative control: on the page, it does.
#[test]
fn a_search_from_the_themes_page_previews_no_card() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(1_074.0, 684.0);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let card = card_action(&runtime, view, "Aterm Day");
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.last_focus = Some(UiKey::new(card));
    }
    assert_eq!(
        state(&runtime, view).showcase_preview_id().as_deref(),
        Some("Aterm Day"),
        "on the page, the focused card is the preview"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/search",
        Some("light theme"),
    );
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(state(&runtime, view).showcase_preview_id(), None);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let preview = compiled
        .semantics
        .iter()
        .find(|node| node.key.as_str() == "settings/preview/search")
        .and_then(|node| match &node.value {
            SemanticValue::Text(value) => Some(value.clone()),
            _ => None,
        })
        .expect("the search page previews the theme row");
    assert!(
        !preview.contains("Aterm Day"),
        "the search page previews the configured theme: {preview}"
    );
}

/// A refused Keep goes back to trying, so the bar never claims a write that
/// did not happen.
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
}

/// Every way of ending a trial without Keep — Revert, Esc, leaving the
/// page — writes nothing: aterm.toml stays byte for byte, and the view stops
/// asking for the theme.
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

/// Save as theme writes a file that reads back to exactly the colours the
/// stage shows (theme plus overrides), and once the catalog lists it, one
/// patch selects it and clears the overrides it now carries.
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
    // The pure builder: the text parses to the resolved scheme, renamed.
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

    // Through the page: Save as… then Save.
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
    // The catalog lists the new file: its snapshot selects it.
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
    // Selected and cleared, the terminal draws what the stage showed.
    let after = published
        .config
        .resolved_colors(resolved.side, &published.assets.themes, None);
    assert_eq!(after.scheme.foreground, resolved.scheme.foreground);
    assert_eq!(after.scheme.ansi, resolved.scheme.ansi);
    assert_eq!(after.scheme.selection_fg, resolved.scheme.selection_fg);
}

/// A save name is refused before anything is written when it is empty, a
/// built-in's, already taken or not a theme name.
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

/// A refused file write cannot consume the colour/palette drafts whose
/// values it tried to save, including their selection and undo state.
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

/// A saved file's delayed appearance must not rebase its old selection and
/// override removal onto values committed after the Save click.
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

/// The watcher and worker are independent: observing the file before its
/// successful completion must still select it once that completion arrives.
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

/// Leaving a page cancels its pending auto-selection, not the file write
/// already admitted. Its later completion cannot change the new page's theme.
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

/// The select a save sends lets go of exactly the submitted authoring state.
/// Retyping the same text with new selection/history is still a newer draft.
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

/// Other folder work may finish while the saved file is waiting for the
/// watcher. Its failure does not revoke this different Save's selection.
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

/// The positional `palette` list is edited only where it reaches: a slot
/// past its end is not written into it (it goes to `palette_overrides`), and
/// resetting drops trailing slots equal to the theme's, down to no key.
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

/// The page's rows are in the compiled tree like every other row: the
/// cards (labelled `<name>, dark|light, text contrast X:1`), the chips, the
/// filter, and — in Customize — the registry rows and the palette chips.
#[test]
fn showcase_page_compiles_its_rows_into_the_semantic_tree() {
    let (_service, mut runtime, instance, view) = themes_view("");
    for (width, height) in [(1_200.0, 900.0), (900.0, 700.0), (560.0, 900.0)] {
        let cx = view_cx_at(width, height);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let cards: Vec<_> = compiled
            .semantics
            .iter()
            .filter(|node| {
                node.key.as_str().starts_with("settings/showcase/card/")
                    && node.role == SemanticRole::Button
            })
            .collect();
        assert!(!cards.is_empty(), "{width}x{height}: cards");
        for card in &cards {
            let (_, rest) = card.label.split_once(", ").expect("name, appearance");
            assert!(
                rest.starts_with("dark, text contrast ")
                    || rest.starts_with("light, text contrast "),
                "{}",
                card.label
            );
            assert!(card.label.ends_with(":1"), "{}", card.label);
        }
        let first = state(&runtime, view).showcase.cards[0].slug.clone();
        assert!(
            compiled
                .semantic(&UiKey::new(format!("settings/showcase/card/{first}")))
                .is_some(),
            "{width}x{height}: a signature theme leads"
        );
        for key in [
            "settings/showcase/filter",
            "settings/showcase/chip/dark",
            "settings/showcase/view/customize",
        ] {
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
        "settings/showcase/view/customize",
        None,
    );
    let cx = view_cx_at(1_200.0, 1_400.0);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    for key in [
        format!("settings/control/{}", prefs::EDIT_FONT_FAMILY),
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

/// Cards are panels and text: no card draws a terminal specimen (the stage
/// is the one, beside a Wide gallery and in Customize), and hovering a card
/// changes no card's paint — it moves the stage onto that card's theme.
#[test]
fn theme_card_paint_never_emits_terminal_specimen() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let medium = compile_settings_view(&runtime, instance, view, &view_cx_at(900.0, 900.0));
    assert!(
        !medium
            .paint
            .iter()
            .any(|node| matches!(node.content, UiContent::SettingsPreview(_))),
        "a Medium gallery has no stage"
    );
    let cx = view_cx_at(1_200.0, 900.0);
    let tiles = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .paint
            .iter()
            .filter_map(|node| match &node.content {
                UiContent::Custom(custom)
                    if custom.audit_id == crate::native_ui::THEME_CARD_AUDIT =>
                {
                    Some((node.key.clone(), custom.value.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let previews = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .paint
            .iter()
            .filter(|node| matches!(node.content, UiContent::SettingsPreview(_)))
            .count()
    };
    let before = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(previews(&before), 1, "the stage beside a Wide gallery");
    let tiles_before = tiles(&before);
    assert!(!tiles_before.is_empty());
    let second = state(&runtime, view).showcase.cards[1].clone();
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.hovered = Some(UiKey::new(format!(
            "settings/showcase/card/{}",
            second.slug
        )));
    }
    let after = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(tiles(&after), tiles_before, "hover re-keys no card");
    assert_eq!(
        state(&runtime, view).showcase_preview_id(),
        Some(second.id.clone()),
        "the stage draws the hovered card"
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/view/customize",
        None,
    );
    let customize = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(previews(&customize), 1, "the stage is the one specimen");
}

/// Esc unwinds the page deepest first: the palette slot editor, then the
/// trial, then the filter; only then does it leave the page, one level up:
/// to Appearance, then Home.
#[test]
fn showcase_escape_unwinds_slot_trial_filter_then_leaves() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("nord"),
    );
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let escape = |runtime: &mut NativeRuntime| {
        runtime
            .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
            .unwrap();
    };
    escape(&mut runtime);
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Themes);
    escape(&mut runtime);
    assert!(state(&runtime, view).showcase.filter.value().is_empty());
    assert_eq!(state(&runtime, view).route, SettingsRoute::Themes);
    escape(&mut runtime);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Appearance);
    escape(&mut runtime);
    assert_eq!(state(&runtime, view).route, SettingsRoute::Home);
}

/// While Themes shows, the rail lists its own row right under Appearance's,
/// indented, and that row is the selected one — in the semantic tree and in
/// `controls` alike. Back on Appearance the row is gone and Appearance is
/// selected: the row is transient and the selection follows the route.
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
    // Directly under Appearance: the column's one gap between them, the
    // same gap Wallpaper sits under Themes by.
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

    // Negative control: on Appearance the row is gone and Appearance is
    // the selected one.
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

/// A Medium page at large type is as narrow, in its own type, as a Compact
/// one: where the chips do not fit beside the filter they take a row of
/// their own, so the filter keeps its placeholder whole (at 2× on an
/// 1698pt window the chips squeezed it to 132 of its 148pt). The negative
/// control: at 1× on a Wide window the chips share the toolbar's row.
#[test]
fn a_narrow_toolbar_puts_the_chips_on_their_own_row_at_any_width() {
    at_each_text_scale(&[2.0], "the Themes toolbar", |_| {
        let (_service, runtime, instance, view) = themes_view("");
        let cx = view_cx_at(1_698.0, 1_002.0);
        assert_eq!(SettingsWidth::for_viewport(1_698.0), SettingsWidth::Medium);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        assert!(
            compiled
                .semantic(&UiKey::new("settings/showcase/chips"))
                .is_some(),
            "the chips' own row"
        );
        let failures = compiled.fit_failures();
        assert!(failures.is_empty(), "{failures:#?}");
    });
    let (_service, runtime, instance, view) = themes_view("");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 820.0));
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/chips"))
            .is_none()
    );
}

/// Put `schemes` in the view's catalog, as a folder rescan does.
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
            revision: 0,
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

/// Save as theme lets go of nothing before the write lands: a failed write
/// reopens the name row with the name and the reason, and the colour and
/// palette drafts are still there. Once the saved theme is selected, the
/// drafts it carries go (the control: they do go on success).
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

/// A save made while a theme is tried keeps that trial standing until the
/// patch selecting the saved theme lands (as Keep does), so the screen never
/// falls back to the old configured theme in between.
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
        vec![(id, scheme)],
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
}

/// A name the theme file spells differently (a space before `#`) still
/// saves: the scheme is named as the file writes it.
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

/// Tuning one palette slot with no `palette` list writes that slot alone
/// (`palette_overrides.<slot>`): every other slot keeps following the theme
/// tried, and the slot's Reset removes just its key.
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
    // The notice names the slot as its chip does and says the colour, never
    // the raw key as an unknown one.
    let said = state_now.feedback.clone().unwrap_or_default();
    assert!(
        said.starts_with("Palette colour 12, bright blue "),
        "{said}"
    );
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

/// The bar over the gallery says when the person's colour settings draw on
/// top of every theme, and leads to Customize (the control: no note without
/// one). The view segment pressed keeps the keyboard.
#[test]
fn showcase_discloses_overrides_and_keeps_segment_focus() {
    let key = UiKey::new("settings/showcase/show-overrides");
    let cx = view_cx_at(1_200.0, 900.0);
    let (_service, runtime, instance, view) = themes_view("");
    assert!(
        compile_settings_view(&runtime, instance, view, &cx)
            .semantic(&key)
            .is_none()
    );
    let (_service, mut runtime, instance, view) = themes_view("background = \"#000000\"\n");
    assert!(
        compile_settings_view(&runtime, instance, view, &cx)
            .semantic(&key)
            .is_some()
    );
    let _ = act(&mut runtime, instance, view, key.as_str(), None);
    assert!(state(&runtime, view).showcase.customize);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/view/gallery",
        None,
    );
    assert_eq!(
        state(&runtime, view)
            .common
            .last_focus
            .as_ref()
            .map(UiKey::as_str),
        Some("settings/showcase/view/gallery")
    );
}

/// Customize carries the look rows, and a colour row's Reset names the
/// setting by the row's own label.
#[test]
fn customize_lists_look_rows_and_names_resets_by_label() {
    let (_service, mut runtime, instance, view) = themes_view("foreground = \"#fafafa\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/view/customize",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 2_400.0));
    for key in [
        prefs::EDIT_MINIMUM_CONTRAST,
        prefs::EDIT_BOLD_IS_BRIGHT,
        prefs::EDIT_BACKGROUND_OPACITY,
    ] {
        if state(&runtime, view).field_by_key(key).is_some() {
            assert!(
                compiled
                    .semantic(&UiKey::new(format!("settings/control/{key}")))
                    .is_some(),
                "{key}"
            );
        }
    }
    let label = state(&runtime, view)
        .field_by_key(prefs::EDIT_FOREGROUND)
        .expect("a foreground row")
        .label;
    let reset = compiled
        .semantic(&UiKey::new(format!(
            "settings/reset/{}",
            prefs::EDIT_FOREGROUND
        )))
        .expect("its Reset");
    assert!(reset.label.contains(label), "{}", reset.label);
}

/// A theme of the person's own carries Open and Trash. Trash asks once and
/// moves on the second press; the theme in use is never moved.
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

/// A palette slot set in `[palette_overrides]` is carried by the saved
/// theme, so the select that names the new theme also clears that table, and
/// the real config service accepts the pair.
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
    // Over the unset theme the select is the saved theme's own name.
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
    // The notice says where the palette comes from now, by the theme's name,
    // never the raw key's label and "its default".
    finish_patch(&mut runtime, instance, view, patches[0].1, true);
    runtime
        .dispatch(instance, view, AppEvent::ConfigChanged(snapshot))
        .unwrap();
    let said = state(&runtime, view).feedback.clone().unwrap_or_default();
    assert!(
        said.contains(&format!("Palette colours now come from {id}")),
        "{said}"
    );
    assert!(!said.contains("its default"), "{said}");
}

/// Save as over a pair that draws one theme on both sides selects the saved
/// theme by Keep's rule, once the catalog knows what the saved theme is made
/// for: a dark theme saved on a light OS takes both sides. The name alone
/// (what the Save click knew) would have changed only the shown side,
/// leaving the old theme on the other.
#[test]
fn save_as_over_a_one_theme_pair_selects_by_keeps_rule() {
    let (mut service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Hearth,light:Hearth\"\n");
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
    let published = publish_saved_catalog(&mut service, &mut runtime, instance, view, &id, scheme);
    let patches = config_patches(&published);
    assert_eq!(patches.len(), 1, "one select patch");
    let (snapshot, _) = apply_real_config_patch(&mut service, &patches[0].0);
    assert_eq!(
        snapshot.config.theme.as_deref(),
        Some(format!("dark:{id},light:{id}").as_str())
    );
    // The control: with the saved theme unknown (the Save click's view), the
    // rule would have kept Hearth on the dark side.
    let raw = Some("dark:Hearth,light:Hearth");
    let unknown = crate::app_config::ThemeCatalog::empty();
    assert_eq!(
        prefs::theme_choice_value(raw, aterm_types::Appearance::Light, &id, &unknown).0,
        format!("dark:Hearth,light:{id}")
    );
}

fn hex(color: aterm_types::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

/// The stage's preview spec on a compiled page, if the page draws one.
fn stage_spec(
    compiled: &crate::native_ui::CompiledUi,
) -> Option<crate::settings_preview::SettingsPreviewSpec> {
    compiled.paint.iter().find_map(|node| match &node.content {
        UiContent::SettingsPreview(spec) => Some((**spec).clone()),
        _ => None,
    })
}

/// The stage's badge names the theme its colours come from: the hovered
/// card, else the configured theme on the shown side — never the raw
/// `dark:…,light:…` value of the field.
#[test]
fn the_stage_names_the_theme_it_draws() {
    let (_service, mut runtime, instance, view) =
        themes_view("theme = \"dark:Nord,light:Default Light\"\n");
    let cx = view_cx_at(1_200.0, 900.0);
    let idle = stage_spec(&compile_settings_view(&runtime, instance, view, &cx)).unwrap();
    assert_eq!(idle.focused_key, prefs::EDIT_THEME);
    let configured = state(&runtime, view).showcase_stage_theme_id().unwrap();
    assert!(!configured.contains(':'), "{configured}");
    assert_eq!(idle.focused_value, configured);
    let second = state(&runtime, view).showcase.cards[1].clone();
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.hovered = Some(UiKey::new(format!(
            "settings/showcase/card/{}",
            second.slug
        )));
    }
    let hovered = stage_spec(&compile_settings_view(&runtime, instance, view, &cx)).unwrap();
    assert_eq!(hovered.focused_value, second.id);
    assert_ne!(second.id, configured, "the control: the badge moved");
}

/// With `theme` unset the stage's badge names Default, the theme it draws,
/// as the theme row does; a card the stage draws reads as a sample, never
/// as the theme in use. After a Revert the caption still names the focused
/// card as a sample only, not a preview of the window.
#[test]
fn the_stage_names_an_unset_theme_and_samples_apart() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(1_200.0, 900.0);
    let caption = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Text(text) if node.key.as_str() == "settings/showcase/stage-caption" => {
                    Some(text.text.clone())
                }
                _ => None,
            })
            .expect("the stage caption")
    };
    let idle = stage_spec(&compile_settings_view(&runtime, instance, view, &cx)).unwrap();
    let drawn = state(&runtime, view).showcase_stage_theme_id().unwrap();
    assert_eq!(drawn, "Default");
    assert_eq!(idle.focused_value, drawn);
    assert!(!idle.theme_sample);
    // The spoken value agrees with the badge and names no sample.
    let spoken = idle.semantic_value();
    assert!(
        spoken.starts_with(&format!("Color theme: {}, ", idle.focused_value)),
        "{spoken}"
    );
    // The control: a configured theme is named by itself.
    let (_service2, runtime2, instance2, view2) = themes_view("theme = \"Nord\"\n");
    let named = stage_spec(&compile_settings_view(&runtime2, instance2, view2, &cx)).unwrap();
    assert_eq!(named.focused_value, "Nord");

    let hearth = card_action(&runtime, view, "Hearth");
    let _ = act(&mut runtime, instance, view, &hearth, None);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.last_focus = Some(UiKey::new(hearth.clone()));
    }
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let sampled = stage_spec(&compiled).unwrap();
    assert!(sampled.theme_sample);
    assert_eq!(sampled.focused_value, "Hearth");
    let spoken = sampled.semantic_value();
    assert!(
        spoken.starts_with("Color theme sample: Hearth, "),
        "{spoken}"
    );
    let said = caption(&compiled);
    assert!(said.contains("Hearth"), "{said}");
    assert_eq!(said, showcase_page::stage_caption(Some("Hearth"), false));
    assert_ne!(
        said,
        showcase_page::stage_caption(None, false),
        "the control: a caption with no sample differs"
    );
}

/// Customize's font family opens its picker ON the Themes page (it was
/// opened and never drawn), with the stage turned to the type sample; Done
/// brings the rows back.
#[test]
fn customize_draws_the_font_picker_over_a_type_stage() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/view/customize",
        None,
    );
    let cx = view_cx_at(1_200.0, 900.0);
    let choice = format!("settings/choice/{}/1", prefs::EDIT_FONT_FAMILY);
    let rows = compile_settings_view(&runtime, instance, view, &cx);
    assert!(rows.semantic(&UiKey::new(choice.as_str())).is_none());
    let _ = act(
        &mut runtime,
        instance,
        view,
        &format!("settings/font-picker/{}", prefs::EDIT_FONT_FAMILY),
        None,
    );
    let picker = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        picker.semantic(&UiKey::new(choice.as_str())).is_some(),
        "the open picker's rows are drawn"
    );
    assert_eq!(
        stage_spec(&picker).unwrap().focused_key,
        prefs::EDIT_FONT_FAMILY
    );
    assert!(
        picker.fit_failures().is_empty(),
        "{:?}",
        picker.fit_failures()
    );
}

/// Every Customize page that draws a palette swatch also draws the slot
/// editor once a swatch is chosen, and Save as turns to the page with its
/// name row: neither opens on a page not shown.
#[test]
fn customize_editors_open_on_the_page_shown() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(1_200.0, 700.0);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/view/customize",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/palette/4",
        None,
    );
    let mut pages = 0;
    let mut drawn = false;
    loop {
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let swatch = compiled
            .semantic(&UiKey::new("settings/showcase/palette/4"))
            .is_some();
        let editor = compiled
            .semantic(&UiKey::new("settings/showcase/palette-slot"))
            .is_some();
        assert_eq!(swatch, editor, "page {pages}");
        drawn |= editor;
        let page = state(&runtime, view).page_scroll;
        let _ = act(&mut runtime, instance, view, "settings/page-down", None);
        let _ = compile_settings_view(&runtime, instance, view, &cx);
        pages += 1;
        if state(&runtime, view).page_scroll == page || pages > 8 {
            break;
        }
    }
    assert!(drawn, "the editor is drawn somewhere");
    assert!(pages > 1, "the control: Customize pages at this height");
    assert!(state(&runtime, view).page_scroll > 0);
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert_eq!(state(&runtime, view).page_scroll, 0);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/showcase/save-name"))
            .is_some()
    );
}

/// A Yours card in use, three to a row, still says its whole name: Open and
/// Trash take a row of their own, so they never squeeze the name to `…`,
/// and they sit inside the card under its line.
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
        "settings/showcase/chip/yours",
        None,
    );
    let card = card_action(&runtime, view, "Harbor Dusk");
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 900.0));
    let name = compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            UiContent::Text(text) if node.key.as_str() == format!("{card}/name") => {
                Some(text.text.clone())
            }
            _ => None,
        })
        .expect("the card's name is painted");
    assert_eq!(name, "Harbor Dusk");
    let rect = |key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{key}"))
            .rect
    };
    let frame = rect(&card);
    let open = rect(&format!("{card}/open"));
    assert!(open.y >= frame.y && open.bottom() <= frame.bottom() + 0.01);
    assert!(
        compiled.fit_failures().is_empty(),
        "{:?}",
        compiled.fit_failures()
    );
}

/// Off the Themes page too, a preview over a Follow system pair names the
/// side it draws, not `Dark:Nord,light:…`.
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

/// The gallery is one scrolling list: Left and Right step through the cards
/// in order, Up and Down move straight down or up a row (to the nearest card
/// where a row is short), and the focused card is always on screen.
#[test]
fn gallery_arrows_walk_rows_and_the_list_follows() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(1_074.0, 684.0);
    let focused = |runtime: &NativeRuntime| {
        state(runtime, view)
            .common
            .last_focus
            .as_ref()
            .and_then(|key| key.as_str().strip_prefix("settings/showcase/card/"))
            .map(str::to_string)
    };
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let grid = state(&runtime, view).showcase.drawn_rows();
    assert!(grid.len() > 2, "the gallery lays its cards out in rows");
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.common.last_focus =
            Some(UiKey::new(format!("settings/showcase/card/{}", grid[0][0])));
    }
    // Right steps along the first row, then on into the next.
    let first_row = grid[0].len();
    for step in 1..=first_row {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::TextInput(TextInputEvent::Right { extend: false }),
            )
            .unwrap();
        let expected = grid.iter().flatten().nth(step).cloned();
        assert_eq!(focused(&runtime), expected, "Right {step}");
    }
    // Down from every row keeps the column, or the nearest card in a short
    // row, and the list scrolls the card into view.
    let mut row = 1;
    let mut column = 0;
    while row + 1 < grid.len() {
        runtime
            .dispatch(instance, view, AppEvent::StepFocus(1))
            .unwrap();
        row += 1;
        column = column.min(grid[row].len() - 1);
        assert_eq!(
            focused(&runtime).as_deref(),
            Some(grid[row][column].as_str()),
            "Down to row {row}"
        );
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        assert!(
            compiled
                .semantic(&UiKey::new(format!(
                    "settings/showcase/card/{}",
                    grid[row][column]
                )))
                .is_some(),
            "row {row}: the focused card is on screen"
        );
    }
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(-1))
        .unwrap();
    assert_eq!(
        focused(&runtime).as_deref(),
        Some(grid[row - 1][column.min(grid[row - 1].len() - 1)].as_str()),
        "Up a row"
    );
}

/// A gallery card's caption says Dark or Light and its description, WHOLE:
/// the tier is the heading over its row, so no caption repeats "Signature",
/// and a row's captions wrap to the lines its longest one needs instead of
/// ending in an ellipsis — at 102x29, 150x48 and 220x60.
#[test]
fn gallery_captions_read_whole_without_the_tier_word() {
    let (_service, mut runtime, instance, view) = themes_view("");
    for (width, height) in [(738.0, 420.0), (1_074.0, 684.0), (1_580.0, 900.0)] {
        let cx = view_cx_at(width, height);
        let mut captions: std::collections::BTreeMap<String, Vec<(usize, String)>> =
            std::collections::BTreeMap::new();
        for page in 0..40 {
            {
                let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
                    unreachable!()
                };
                state.page_scroll = page;
            }
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            for node in compiled.paint.iter().filter(|node| !node.peek) {
                let Some((card, line)) = node
                    .key
                    .as_str()
                    .strip_prefix("settings/showcase/card/")
                    .and_then(|rest| rest.split_once("/meta/line-"))
                else {
                    continue;
                };
                let UiContent::Text(spec) = &node.content else {
                    continue;
                };
                let entry = captions.entry(card.to_string()).or_default();
                let line = line.parse::<usize>().unwrap();
                if !entry.iter().any(|(seen, _)| *seen == line) {
                    entry.push((line, spec.text.clone()));
                }
            }
        }
        let cards = &state(&runtime, view).showcase.cards;
        assert!(!captions.is_empty(), "{width}x{height}: captions paint");
        for (slug, mut lines) in captions {
            lines.sort();
            let painted = lines
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let card = cards.iter().find(|card| card.slug == slug).unwrap();
            assert!(
                !painted.contains('\u{2026}'),
                "{width}x{height} {slug}: {painted}"
            );
            assert!(
                !painted.contains("Signature"),
                "{width}x{height} {slug}: {painted}"
            );
            let expected = format!(
                "{} \u{b7} {}",
                if card.appearance == aterm_types::Appearance::Dark {
                    "Dark"
                } else {
                    "Light"
                },
                card.description
            );
            assert_eq!(
                painted.split_whitespace().collect::<Vec<_>>(),
                expected.split_whitespace().collect::<Vec<_>>(),
                "{width}x{height} {slug}: the whole description"
            );
        }
    }
}

/// A small window's gallery opens on cards, not on its toolbar alone
/// (724x434 once drew only the toolbar and chips on its first screen):
/// under every chip the first screen draws at least one whole card, the
/// filter and chips are there, and no control of theirs is clipped. A
/// Compact row wide enough seats the chips beside the filter
/// (`gallery_toolbar_fits_chips`), which leaves the cards that room.
#[test]
fn a_small_gallery_opens_on_a_whole_card() {
    let (_service, mut runtime, instance, view) = themes_view("");
    for (width, height) in [(724.0, 434.0), (724.0, 380.0), (584.0, 350.0)] {
        let cx = view_cx_at(width, height);
        for chip in ["all", "dark", "light"] {
            let _ = act(
                &mut runtime,
                instance,
                view,
                &format!("settings/showcase/chip/{chip}"),
                None,
            );
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let whole_cards = compiled
                .semantics
                .iter()
                .filter(|node| {
                    node.key.as_str().starts_with("settings/showcase/card/")
                        && node.role == SemanticRole::Button
                        && node.rect.y >= 0.0
                        && node.rect.y + node.rect.height <= height
                })
                .count();
            assert!(
                whole_cards >= 1,
                "{width}x{height} {chip}: the first screen draws no whole card"
            );
            for key in ["settings/showcase/filter", "settings/showcase/chip/light"] {
                assert!(
                    compiled.semantic(&UiKey::new(key)).is_some(),
                    "{width}x{height} {chip}: {key}"
                );
            }
            let clipped: Vec<String> = compiled
                .fit_failures()
                .into_iter()
                .filter(|failure| !failure.contains("settings/showcase/card/"))
                .collect();
            assert!(clipped.is_empty(), "{width}x{height} {chip}: {clipped:?}");
        }
    }
    // The fit itself: 724pt seats the chips on the filter's row; a phone
    // width does not.
    assert!(showcase_page::gallery_toolbar_fits_chips(724.0 - 32.0));
    assert!(!showcase_page::gallery_toolbar_fits_chips(320.0 - 32.0));
}

/// THE GALLERY'S SCROLL THUMB PAINTS IN A GUTTER OF ITS OWN: where the
/// gallery scrolls, its rows stop short of the list's trailing edge, so the
/// thumb's bar (3pt at 7pt in, down its track) crosses no Filter field, chip,
/// card or caption, at the compact 80x24 and 102x29 windows and at the
/// roomier 150x48 and 220x60 ones. CONTROL: the thumb is there at all.
#[test]
fn the_gallery_thumb_crosses_no_control_or_card() {
    let (_service, runtime, instance, view) = themes_view("");
    for (width, height) in [
        (584.0, 348.0),
        (738.0, 420.0),
        (1_074.0, 686.0),
        (1_564.0, 854.0),
    ] {
        let cx = view_cx_at(width, height);
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        let thumb = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/list/thumb")
            .unwrap_or_else(|| panic!("{width}x{height}: the gallery scrolls"))
            .rect;
        let bar = LogicalRect::new(
            thumb.right() - 7.0,
            thumb.y + 10.0,
            3.0,
            (thumb.height - 20.0).max(0.0),
        );
        let crossed: Vec<&str> = compiled
            .paint
            .iter()
            .filter(|node| {
                node.key.as_str().starts_with("settings/showcase/")
                    && !matches!(node.content, UiContent::Group(_))
                    && node
                        .clip
                        .intersect(bar)
                        .is_some_and(|both| !both.is_empty())
            })
            .map(|node| node.key.as_str())
            .collect();
        assert!(crossed.is_empty(), "{width}x{height}: {crossed:?}");
    }
}

/// A CARD OVER THE GALLERY'S TOP KEEPS ITS FRAME: on the last screen at
/// 80x24 the top edge cuts the row of cards above the footer, and a cut card
/// is dead there, but it is drawn whole as far as it shows, its frame under
/// its tile and caption, never a tile floating with no card around it.
/// CONTROL: the screen has such a cut card at all.
#[test]
fn a_card_cut_over_the_gallerys_top_keeps_its_frame() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let cx = view_cx_at(584.0, 348.0);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let limit = state(&runtime, view)
        .result_page_limit
        .get()
        .expect("the gallery scrolls");
    {
        let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
            unreachable!()
        };
        state.page_scroll = limit;
    }
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    let tiles: Vec<&str> = compiled
        .paint
        .iter()
        .filter_map(|node| {
            node.key
                .as_str()
                .strip_prefix("settings/showcase/card/")?
                .strip_suffix("/tile")
        })
        .collect();
    assert!(
        compiled
            .paint
            .iter()
            .any(|node| node.key.as_str().ends_with("/tile")
                && compiled
                    .semantic(&UiKey::new(node.key.as_str().trim_end_matches("/tile")))
                    .is_none()),
        "the last screen cuts a card over its top: {tiles:?}"
    );
    for slug in tiles {
        let frame = format!("settings/showcase/card/{slug}");
        assert!(
            compiled.paint.iter().any(|node| node.key.as_str() == frame),
            "{slug}: its card is drawn under its tile"
        );
    }
}

/// Revert's feedback bar must leave every filtered card reachable whole by
/// the real pager, including its caption, at the 80x24 native viewport.
#[test]
fn a_small_gallery_keeps_a_whole_card_reachable_after_revert() {
    fn page_to_control(
        runtime: &mut NativeRuntime,
        instance: crate::native_app::AppInstanceId,
        view: crate::native_app::ViewId,
        key: &str,
        direction: i32,
        phase: &str,
    ) {
        let cx = view_cx_at(584.0, 348.0);
        let mut last = Vec::new();
        // This fixture has one filtered card and only its bars and footer.
        // Exercise the actual pager rather than assigning a scroll offset.
        for _ in 0..16 {
            let compiled = compile_settings_view(runtime, instance, view, &cx);
            last = compiled
                .fit_failures()
                .into_iter()
                .filter(|failure| failure.contains(key))
                .collect();
            if let Some(paint) = compiled
                .paint
                .iter()
                .find(|paint| paint.key.as_str() == key && !paint.peek)
                && compiled.semantic(&UiKey::new(key)).is_some()
                && compiled.hits.iter().any(|hit| hit.key.as_str() == key)
                && paint.clip.x <= paint.rect.x + 0.01
                && paint.clip.y <= paint.rect.y + 0.01
                && paint.clip.right() + 0.01 >= paint.rect.right()
                && paint.clip.bottom() + 0.01 >= paint.rect.bottom()
                && last.is_empty()
            {
                return;
            }
            runtime
                .dispatch(instance, view, AppEvent::ScrollPage(direction))
                .unwrap();
        }
        panic!("{phase}: {key} never fits whole after real paging: {last:?}");
    }

    let source = "theme = \"Default Light\"\nforeground = \"#ffaa00\"\npalette_overrides = { \"12\" = \"#33cc99\" }\n";
    let (service, mut runtime, instance, view) = themes_view(source);
    let before = service.snapshot().text.to_string();
    act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("Nord"),
    );
    let nord = card_action(&runtime, view, "Nord");
    page_to_control(&mut runtime, instance, view, &nord, 1, "before trial");
    assert!(state(&runtime, view).feedback.is_none());
    act(&mut runtime, instance, view, &nord, None);
    assert!(state(&runtime, view).theme_trial_intent().is_some());
    page_to_control(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        -1,
        "trial controls",
    );
    let reverted = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/revert",
        None,
    );
    assert!(config_patches(&reverted).is_empty());
    assert_eq!(state(&runtime, view).feedback.as_deref(), Some("Reverted"));
    assert!(state(&runtime, view).theme_trial_intent().is_none());
    assert_eq!(service.snapshot().text.to_string(), before);
    page_to_control(
        &mut runtime,
        instance,
        view,
        &nord,
        1,
        "after Revert feedback",
    );
}

fn ids_under(showcase: &mut ShowcaseState, chip: ShowcaseChip) -> BTreeSet<String> {
    showcase.chip = chip;
    showcase
        .visible()
        .into_iter()
        .map(|index| showcase.cards[index].id.clone())
        .collect()
}

/// Get More lists exactly the pack themes the folder has no file for, as
/// pack cards, and no other chip lists a pack card. Negative control: once
/// the folder holds "Kanagawa Wave" its pack card goes (its own card is
/// under Yours).
#[test]
fn get_more_lists_only_the_pack_themes_the_folder_lacks() {
    let pack: BTreeSet<String> = aterm_theme_pack::themes()
        .iter()
        .map(|theme| theme.id.to_string())
        .collect();
    let mut bare = ShowcaseState::new(&crate::app_config::ThemeCatalog::from_schemes([]));
    assert_eq!(ids_under(&mut bare, ShowcaseChip::More), pack);
    for chip in [ShowcaseChip::All, ShowcaseChip::Dark, ShowcaseChip::Light] {
        bare.chip = chip;
        assert!(
            bare.visible()
                .into_iter()
                .all(|index| bare.cards[index].tier != ShowcaseTier::Pack),
            "{chip:?}"
        );
    }
    bare.chip = ShowcaseChip::More;
    assert!(
        bare.visible()
            .into_iter()
            .all(|index| bare.cards[index].tier == ShowcaseTier::Pack)
    );

    let themes = crate::app_config::ThemeCatalog::from_schemes([user_scheme(
        "Kanagawa Wave",
        aterm_types::Appearance::Dark,
    )]);
    let mut installed = ShowcaseState::new(&themes);
    let more = ids_under(&mut installed, ShowcaseChip::More);
    assert!(!more.contains("Kanagawa Wave"));
    assert_eq!(more.len(), pack.len() - 1);
    assert!(ids_under(&mut installed, ShowcaseChip::Yours).contains("Kanagawa Wave"));
    assert_eq!(
        installed
            .cards
            .iter()
            .filter(|card| card.id == "Kanagawa Wave")
            .count(),
        1
    );
}

/// The one AddPack request a pack card's press sends, if it sent one.
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

/// A pack card's press asks the host to add the theme and writes no
/// config; once the add reports and the catalog lists the file, the theme
/// is tried with the keyboard on Keep, and aterm.toml is untouched.
#[test]
fn a_pack_card_adds_its_theme_then_tries_it() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let before = service.snapshot().text.to_string();
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/chip/more",
        None,
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("Iceberg"),
    );
    let card = card_action(&runtime, view, "Iceberg Light");
    // To the eye, the card says a press adds it.
    let cx = view_cx_at(1_200.0, 900.0);
    let kicker = |runtime: &NativeRuntime, card: &str| {
        compile_settings_view(runtime, instance, view, &cx)
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Text(text) if node.key.as_str() == format!("{card}/kicker") => {
                    Some(text.text.clone())
                }
                _ => None,
            })
    };
    assert_eq!(kicker(&runtime, &card).as_deref(), Some("+ ADD"));
    let pressed = act(&mut runtime, instance, view, &card, None);
    let requests = add_pack_request(&pressed);
    assert_eq!(requests.len(), 1, "{:?}", pressed.effects);
    assert_eq!(requests[0].0, "Iceberg Light");
    assert!(config_patches(&pressed).is_empty());
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);

    let finished = runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation: requests[0].1,
                outcome: ThemesOutcome::Added {
                    id: "Iceberg Light".to_string(),
                },
            },
        )
        .unwrap();
    assert!(config_patches(&finished).is_empty());
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        None,
        "not tried before the catalog lists it"
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
    assert!(config_patches(&listed).is_empty());
    assert_eq!(
        state(&runtime, view).theme_trial_intent(),
        Some(crate::control_theme::ThemeTrialIntent::Show(
            "Iceberg Light".to_string()
        ))
    );
    assert_eq!(
        state(&runtime, view).common.last_focus,
        Some(UiKey::new("settings/showcase/keep"))
    );
    assert_eq!(service.snapshot().text.to_string(), before);
    // Get More no longer lists it: the page shows it among Yours, drawn as
    // the theme being tried.
    assert_eq!(state(&runtime, view).showcase.chip, ShowcaseChip::Yours);
    let added = card_action(&runtime, view, "Iceberg Light");
    assert!(
        compile_settings_view(&runtime, instance, view, &cx)
            .semantic(&UiKey::new(added.as_str()))
            .is_some(),
        "the tried card is on the page"
    );
    assert_eq!(kicker(&runtime, &added).as_deref(), Some("TRYING"));
}

/// An add that fails lets the try go: a later catalog that lists the
/// theme does not try it.
#[test]
fn a_failed_pack_add_tries_nothing() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/chip/more",
        None,
    );
    let card = card_action(&runtime, view, "Iceberg Light");
    let pressed = act(&mut runtime, instance, view, &card, None);
    let operation = add_pack_request(&pressed)[0].1;
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Failed {
                    message: "a theme named \"Iceberg Light\" is already in the themes folder"
                        .to_string(),
                },
            },
        )
        .unwrap();
    assert!(state(&runtime, view).showcase.pending_try.is_none());
    let text = aterm_theme_pack::find("Iceberg Light").unwrap().text;
    let scheme = aterm_types::scheme::parse_theme_str(text).unwrap().scheme;
    let _ = publish_saved_catalog(
        &mut service,
        &mut runtime,
        instance,
        view,
        "Iceberg Light",
        scheme,
    );
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    assert_eq!(state(&runtime, view).showcase.trial, ShowcaseTrial::Idle);
}

/// A pack theme aterm.toml already names (its file was missing) is drawn
/// once added, not tried: there is nothing to Keep. Negative control: the
/// same add with another theme configured is tried
/// (`a_pack_card_adds_its_theme_then_tries_it`).
#[test]
fn adding_the_configured_pack_theme_does_not_try_it() {
    let (mut service, mut runtime, instance, view) = themes_view("theme = \"Iceberg Light\"\n");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/chip/more",
        None,
    );
    let card = card_action(&runtime, view, "Iceberg Light");
    let pressed = act(&mut runtime, instance, view, &card, None);
    let operation = add_pack_request(&pressed)[0].1;
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::ThemesFinished {
                operation,
                outcome: ThemesOutcome::Added {
                    id: "Iceberg Light".to_string(),
                },
            },
        )
        .unwrap();
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
    assert_eq!(state(&runtime, view).theme_trial_intent(), None);
    assert!(state(&runtime, view).showcase.pending_try.is_none());
}

/// Get More sits among the toolbar's chips where the row has room for a
/// fifth, and only in the footer where it does not (the four filter chips
/// keep their row, so a small window still opens on a card). The footer's
/// button shows the same filter.
#[test]
fn get_more_joins_the_toolbar_only_where_it_fits() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let more = UiKey::new("settings/showcase/chip/more");
    let wide = compile_settings_view(&runtime, instance, view, &view_cx_at(724.0, 434.0));
    assert!(wide.semantic(&more).is_some(), "a wide row seats Get More");
    let narrow = compile_settings_view(&runtime, instance, view, &view_cx_at(584.0, 350.0));
    assert!(
        narrow.semantic(&more).is_none(),
        "a narrow row keeps four chips"
    );
    assert!(
        narrow
            .semantic(&UiKey::new("settings/showcase/chip/yours"))
            .is_some()
    );
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/footer/get-more",
        None,
    );
    assert_eq!(state(&runtime, view).showcase.chip, ShowcaseChip::More);
}

/// A catalog of one typography theme `Studio` (and the built-ins).
fn studio_catalog(extra: &str) -> std::sync::Arc<crate::app_config::ThemeCatalog> {
    let text = format!("foreground = #e0e0e0\nbackground = #101418\n{extra}");
    let parsed = aterm_types::scheme::parse_theme_str(&text).expect("a theme");
    crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
}

const STUDIO_TYPE: &str = "font_family = Berkeley Mono\nline_height = 1.15\nfont_weight = 450\n";

/// The Themes page over `source` with `themes` admitted.
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

/// The stage's badge names a themes-folder theme by its `name =`, as its
/// card does, never by its file id. Negative control: a built-in's id is its
/// name (`the_stage_names_an_unset_theme_and_samples_apart`).
#[test]
fn the_stage_names_a_folder_theme_by_its_name() {
    let parsed = aterm_types::scheme::parse_theme_str(
        "name = Harbour Chrome\nforeground = #d8dee9\nbackground = #101820\n",
    )
    .expect("a theme");
    let themes =
        crate::app_config::ThemeCatalog::from_parsed([("harbour-chrome".to_string(), parsed)]);
    let (_service, runtime, instance, view) =
        typography_view("theme = \"harbour-chrome\"\n", themes);
    let cx = view_cx_at(1_200.0, 900.0);
    let spec = stage_spec(&compile_settings_view(&runtime, instance, view, &cx)).unwrap();
    assert_eq!(spec.focused_value, "Harbour Chrome");
    let spoken = spec.semantic_value();
    assert!(
        spoken.starts_with("Color theme: Harbour Chrome, "),
        "{spoken}"
    );
}

/// A typography card says what it sets, to the eye (a meta line) and in
/// its accessible label, and the filter finds it by its font or by "font".
/// Negative control: a colours-only card sets nothing and says none of it.
#[test]
fn a_typography_card_says_what_type_it_sets() {
    let cards = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(STUDIO_TYPE));
    let studio = cards
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert_eq!(studio.type_keys, 3);
    assert!(studio.typography.is_some() && studio.typography_spoken.is_some());
    assert!(studio.matches("berkeley") && studio.matches("font"));
    let plain = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(""));
    let plain = plain
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert_eq!(plain.type_keys, 0);
    assert!(plain.typography.is_none() && plain.typography_spoken.is_none());
    assert!(!plain.matches("berkeley"));
    assert!(!plain.accessible_label().is_empty());
    assert_ne!(studio.accessible_label(), plain.accessible_label());
    assert_ne!(studio.tile_value(false), plain.tile_value(false));
}

/// The control at `key` in `compiled`, enabled or not.
fn enabled(compiled: &crate::native_ui::CompiledUi, key: &str) -> Option<bool> {
    compiled
        .semantic(&UiKey::new(key))
        .map(|node| node.state.is_none_or(|state| state.enabled))
}

/// Trying a typography theme draws its colours only and lists, by key, what
/// its type settings would change: each side in one vocabulary (a switch
/// reads on/off on both). Keep colours only writes ONE patch: the theme and
/// each changed setting pinned at its current value. Negative control: a
/// colours-only theme lists nothing and offers no second Keep.
#[test]
fn trying_a_typography_theme_offers_keep_colours_only() {
    let studio_type = "font_family = Berkeley Mono\nline_height = 1.15\nligatures = false\n";
    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(studio_type));
    let cx = view_cx_at(1_200.0, 900.0);
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let trial = state(&runtime, view).trial_typography("Studio");
    let changes: Vec<(&str, &str, &str)> = trial
        .changes
        .iter()
        .map(|change| (change.key, change.theme.as_str(), change.current.as_str()))
        .collect();
    assert_eq!(
        changes,
        vec![
            (prefs::EDIT_FONT_FAMILY, "Berkeley Mono", "aterm's own face"),
            (prefs::EDIT_LINE_HEIGHT, "1.15", "1.0"),
            (prefs::EDIT_LIGATURES, "off", "on"),
        ]
    );
    assert!(trial.keeps_colours_only());
    let compiled = compile_settings_view(&runtime, instance, view, &cx);
    assert!(
        compiled
            .semantic(&UiKey::new("settings/top/showcase-typography"))
            .is_some()
    );
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
            (prefs::EDIT_LINE_HEIGHT.to_string(), Some("1.0".to_string())),
            (prefs::EDIT_LIGATURES.to_string(), Some("true".to_string())),
        ]),
    );
    // The pins really keep the face: the config they make resolves to no
    // theme typography at all.
    let pinned: Config = aterm_toml::from_str(
        "theme = \"Studio\"\nfont_family = \"\"\nline_height = 1.0\nligatures = true\n",
    )
    .expect("toml");
    let typo = pinned.resolved_typography_with_flag(
        aterm_types::Appearance::Dark,
        &studio_catalog(studio_type),
        None,
    );
    assert!(typo.applied.keys().is_empty(), "{:?}", typo.applied.keys());

    // Negative control.
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
    assert!(
        compiled
            .semantic(&UiKey::new("settings/top/showcase-typography"))
            .is_none()
    );
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), None);
}

/// A theme that sets `font_weight` cannot be kept as colours only: no
/// aterm.toml value keeps the font's own weight. The button is drawn
/// disabled, and pressing it anyway (the palette) writes nothing. Negative
/// control: the same theme without the weight is offered.
#[test]
fn keep_colours_only_is_withheld_when_a_change_cannot_be_pinned() {
    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(STUDIO_TYPE));
    let cx = view_cx_at(1_200.0, 900.0);
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
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), Some(false));
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
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), Some(true));
}

/// A theme setting your `display_font` beats is listed as kept by
/// `display_font`, with its value. Negative control: with `font_family`
/// set, `font_family` is what keeps it.
#[test]
fn a_kept_type_setting_names_what_wins() {
    for (config, by, value) in [
        ("display_font = \"pixel\"\n", "display_font", "pixel"),
        ("font_family = \"Menlo\"\n", "font_family", "Menlo"),
    ] {
        let (_service, mut runtime, instance, view) = typography_view(
            &format!("theme = \"Dracula\"\n{config}"),
            studio_catalog("font_family = Berkeley Mono\nline_height = 1.15\n"),
        );
        let studio = card_action(&runtime, view, "Studio");
        let _ = act(&mut runtime, instance, view, &studio, None);
        let trial = state(&runtime, view).trial_typography("Studio");
        let kept: Vec<(&str, &str, &str)> = trial
            .kept
            .iter()
            .map(|kept| (kept.key, kept.by, kept.value.as_str()))
            .collect();
        assert_eq!(kept, vec![(prefs::EDIT_FONT_FAMILY, by, value)], "{config}");
    }
}

const SHOWCASE_KEEP_COLOURS_KEY: &str = "settings/showcase/keep-colours";

/// A font row the config leaves unset reads the theme's value "(from theme
/// X)", its picker's reset reads "Use theme's (…)", and a row the config sets
/// says it overrides the theme while its Reset returns to the theme's.
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
    // Negative control: without the theme the row is aterm's default.
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

/// Save as theme with "Include type settings" writes the type the page shows
/// and reads it back whole, and carries those keys; without it the file
/// carries none.
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

/// Five type keys: a tall typography delta.
const STUDIO_FIVE: &str = "font_family = Menlo\nline_height = 1.2\nligatures = false\n\
                           font_weight = 500\nfont_size_scale = 1.1\n";

/// While a typed theme is tried on a Wide page, the trial bar and toolbar
/// are laid out for the width they are drawn in: pinned over the page, or
/// moved into the gallery column beside the stage on a short window. The
/// filter in force stays on screen and no chip is cut off; and at an
/// ordinary window the bar's Keep, Keep colours only and Revert stay in
/// view with the delta (its lines are text lines, not control rows).
#[test]
fn a_typed_trial_keeps_the_bar_and_toolbar_whole_on_a_wide_page() {
    let (_service, mut runtime, instance, view) =
        typography_view("theme = \"Dracula\"\n", studio_catalog(STUDIO_FIVE));
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/filter",
        Some("Studio"),
    );
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    for (width, height) in [(1_074.0, 658.0), (1_074.0, 560.0), (1_120.0, 380.0)] {
        assert_eq!(SettingsWidth::for_viewport(width), SettingsWidth::Wide);
        let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(width, height));
        let at = format!("{width}x{height}");
        assert!(
            compiled
                .semantic(&UiKey::new("settings/showcase/filter"))
                .is_some(),
            "{at}: the active filter is on screen"
        );
        let failures = compiled.fit_failures();
        assert!(failures.is_empty(), "{at}: {failures:#?}");
    }
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_074.0, 658.0));
    for key in [
        "settings/showcase/keep",
        SHOWCASE_KEEP_COLOURS_KEY,
        "settings/showcase/revert",
    ] {
        assert!(compiled.semantic(&UiKey::new(key)).is_some(), "{key}");
    }
}

/// Save as theme draws the type switch's own words beside it (the Setting
/// switch paints only On/Off), and the note under the row says what the
/// save writes: with the switch on, the type settings too.
#[test]
fn save_as_theme_labels_its_type_switch() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let _ = act(
        &mut runtime,
        instance,
        view,
        "settings/showcase/save-as",
        None,
    );
    let cx = view_cx_at(1_200.0, 900.0);
    let painted = |runtime: &NativeRuntime, key: &str| {
        compile_settings_view(runtime, instance, view, &cx)
            .paint
            .iter()
            .find_map(|node| match &node.content {
                UiContent::Text(text) if node.key.as_str() == key => Some(text.text.clone()),
                _ => None,
            })
    };
    assert_eq!(
        painted(&runtime, "settings/showcase/save-type-label").as_deref(),
        Some("Include type settings")
    );
    let off = painted(&runtime, "settings/showcase/save-note").expect("a note");
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
    let on = painted(&runtime, "settings/showcase/save-note").expect("a note");
    assert_ne!(on, off, "the note follows the switch");
}

/// Every chip's and Get More's accessible name starts with the words it
/// shows (WCAG 2.5.3), as do the trial bar's Keep theme and Keep colours
/// only.
#[test]
fn showcase_buttons_are_named_by_their_visible_words() {
    let (_service, mut runtime, instance, view) = typography_view(
        "theme = \"Dracula\"\n",
        studio_catalog("font_family = Menlo\n"),
    );
    let studio = card_action(&runtime, view, "Studio");
    let _ = act(&mut runtime, instance, view, &studio, None);
    let compiled = compile_settings_view(&runtime, instance, view, &view_cx_at(1_200.0, 900.0));
    let mut checked = 0;
    for (key, visible) in [
        ("settings/showcase/chip/all", "All"),
        ("settings/showcase/chip/dark", "Dark"),
        ("settings/showcase/chip/light", "Light"),
        ("settings/showcase/chip/yours", "Yours"),
        ("settings/showcase/chip/more", "Get More"),
        ("settings/showcase/footer/get-more", "Get More"),
        ("settings/showcase/keep", "Keep theme"),
        (SHOWCASE_KEEP_COLOURS_KEY, "Keep colours only"),
    ] {
        let Some(node) = compiled.semantic(&UiKey::new(key)) else {
            continue;
        };
        checked += 1;
        assert!(
            node.label
                .to_lowercase()
                .starts_with(&visible.to_lowercase()),
            "{key}: {:?}",
            node.label
        );
    }
    assert!(checked >= 6, "the controls are on the page");
}
