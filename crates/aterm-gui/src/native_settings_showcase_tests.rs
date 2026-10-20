// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Settings ▸ Themes: the chooser (highlight is the trial), the strip that
//! never moves, the trial lane's two endings (one write on Keep; no write,
//! byte for byte, on every way of reverting), community themes kept through
//! the pack, Save as theme's round trip, the Customize sheet, and the page's
//! rows in the compiled tree.

use super::*;
use crate::settings_preview::theme_card::Section;
use showcase_page::{
    ShowcaseState, ShowcaseTrial, carried_override_keys, demo_font_candidate, palette_reset,
    palette_with, save_as_text, save_name_problem,
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
/// falls back to the old configured theme in between; the live region then
/// names the saved theme, the one the patch wrote, not the trial it ended.
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
    let settled = showcase.settled.clone().expect("announced");
    assert!(settled.ends_with(&showcase.label_of(&id)), "{settled}");
    assert!(!settled.ends_with(&showcase.label_of("Nord")), "{settled}");
}

/// A row the arrows reach after Save, before the catalog lists the saved
/// file, is a new choice, as a click is: the save is not selected over it
/// and the trial is not ended by a write that did not keep it. Negative
/// control: with no arrow the save is selected.
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

/// Keep and Keep colours only pressed from the keyboard leave it on a live
/// control once they land (the kept row), where the next Tab goes on from;
/// the strip's buttons are gone by then. Negative control: a keyboard
/// elsewhere (on a row) stays where it is.
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

/// While a Keep is landing the highlight holds, and so does the keyboard:
/// it never lands on a row the highlight is not on. Negative control: once
/// the Keep lands the arrow moves both.
#[test]
fn an_arrow_during_keeping_leaves_the_keyboard_on_the_kept_row() {
    let (_service, mut runtime, instance, view) = themes_view("");
    let nord = card_action(&runtime, view, "Nord");
    let _ = act(&mut runtime, instance, view, &nord, None);
    let kept = act(&mut runtime, instance, view, "settings/showcase/keep", None);
    let (_, operation) = config_patches(&kept)[0].clone();
    let _ = compile_settings_view(&runtime, instance, view, &view_cx_at(1_284.0, 714.0));
    focus_on(&mut runtime, view, &nord);
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    let focus = |runtime: &NativeRuntime| {
        state(runtime, view)
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_string())
    };
    assert_eq!(focus(&runtime).as_deref(), Some(nord.as_str()));
    finish_patch(&mut runtime, instance, view, operation, true);
    runtime
        .dispatch(instance, view, AppEvent::StepFocus(1))
        .unwrap();
    assert_ne!(focus(&runtime).as_deref(), Some(nord.as_str()));
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

/// Return in the filter keeps the trial only while the filter lists its
/// row: a trial the typed text hid writes nothing. Negative control: a
/// filter that still lists it keeps it, in one write.
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

/// The control at `key` in `compiled`, enabled or not.
fn enabled(compiled: &crate::native_ui::CompiledUi, key: &str) -> Option<bool> {
    compiled
        .semantic(&UiKey::new(key))
        .map(|node| node.state.is_none_or(|state| state.enabled))
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

/// A refused Keep goes back to trying, so the strip never claims a write
/// that did not happen, and its second line says why.
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

/// The page's rows are in the compiled tree like every other row, at every
/// shape: the list (a ListBox) of options named by the theme with its
/// appearance and contrast as the value, the filter, the strip, the demo
/// and the footer; in Customize, the registry rows and the palette.
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
        // Compact lists the footer as the list's last rows, out of view.
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

/// The palette slot editor is drawn wherever its swatch is, and Save as
/// draws its name row: neither opens out of sight.
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

/// A Yours row in use still says its whole name beside Open, and Open sits
/// inside the row.
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

/// The chooser is one list: ↓ walks its rows in order, trying each, and the
/// focused row is always in the tree (scrolled into view); ↑ walks back.
#[test]
fn list_arrows_walk_rows_and_the_list_follows() {
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let cx = view_cx_at(1_074.0, 684.0);
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    let first_frame = compile_settings_view(&runtime, instance, view, &cx);
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

/// Esc unwinds the page deepest first (§5.5): the slot editor, then the
/// Customize sheet; the filter's text while it has the keyboard, then the
/// trial. With nothing left to cancel it does nothing: the page stays
/// (navigation is the sidebar's, and on a compact page All Settings').
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
    // ↓ from the filter tries the first match and keeps the keyboard there.
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

/// The ids a showcase lists.
fn listed_ids(showcase: &ShowcaseState) -> BTreeSet<String> {
    showcase
        .visible()
        .into_iter()
        .map(|index| showcase.cards[index].id.clone())
        .collect()
}

/// COMMUNITY lists exactly the pack themes the folder has no file for, and
/// only while it is open. Negative control: once the folder holds "Kanagawa
/// Wave" its community row goes (its own row is under Yours).
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

/// A typography row says what it sets, in its value, and the filter finds
/// it by its font or by "font". Negative control: a colours-only row sets
/// nothing and says none of it.
#[test]
fn a_typography_card_says_what_type_it_sets() {
    let cards = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(STUDIO_TYPE));
    let studio = cards
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert!(studio.typography.is_some() && studio.typography_spoken.is_some());
    assert!(studio.matches("berkeley") && studio.matches("font"));
    let plain = crate::settings_preview::theme_card::showcase_cards(&studio_catalog(""));
    let plain = plain
        .iter()
        .find(|card| card.id == "Studio")
        .expect("Studio");
    assert!(plain.typography.is_none() && plain.typography_spoken.is_none());
    assert!(!plain.matches("berkeley"));
    assert!(!plain.matches("font"));
    let spoken = studio.typography_spoken.clone().unwrap_or_default();
    let value = studio.row_value(false, false);
    assert!(value.ends_with(&format!(", {spoken}")), "{value}");
    assert_eq!(
        studio.row_value(false, false).len(),
        plain.row_value(false, false).len() + spoken.len() + 2,
        "the plain row's value is the same less the type"
    );
}

/// The demo of a tried theme whose family is `display:` draws those display
/// faces over the automatic font, as Keep would leave them
/// (`Config::display_faces_request`): the candidate's font is never the
/// display family, which no resolver answers with a face (the demo fell back
/// to the current font and drew no display face at all). The config's own
/// `display_font` still wins. Negative control: a real family is the font.
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

/// Trying a typography theme draws its colours only and lists, by key, what
/// its type settings would change, with the value each takes. Keep ▾'s Keep
/// colours only writes ONE patch: the theme and each changed setting pinned
/// at its current value. Negative control: a colours-only theme lists
/// nothing and offers no second Keep.
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
    // The line height now is the platform default (`app_config::DEFAULT_LINE_HEIGHT`).
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
    // The pins really keep the face: the config they make resolves to no
    // theme typography at all.
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
    assert_eq!(enabled(&compiled, "settings/showcase/keep-more"), None);
    assert_eq!(enabled(&compiled, SHOWCASE_KEEP_COLOURS_KEY), None);
}

/// Trying a colours-only theme while the configured theme sets the line
/// height is a type change too: Keep returns the line height to aterm's own,
/// so the demo draws that line height, and Keep colours only is offered and
/// pins the configured theme's. Negative control: between two colours-only
/// themes nothing changes and only Keep is offered.
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

    // Negative control.
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

/// A theme that sets `font_weight` cannot be kept as colours only: no
/// aterm.toml value keeps the font's own weight. Keep ▾'s chevron is
/// absent, and pressing Keep colours only anyway (the palette) writes
/// nothing. Negative control: the same theme without the weight offers it.
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

/// Save as theme draws the type switch's own words beside it (a plain
/// switch paints none) while there is type to include, and Save's
/// description says what the save writes: with the switch on, the type
/// settings too. Negative control: with no type in force, no switch.
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

// -----------------------------------------------------------------------
// The chooser, the strip and the demo (design §2–§8)
// -----------------------------------------------------------------------

const ROW: &str = "settings/showcase/card/";
const STATUS: &str = "settings/showcase/status";
const STAGE: &str = "settings/showcase/stage";
/// The widths the page takes its three shapes at: Compact, Medium, Wide.
const SHAPES: [f32; 3] = [556.0, 934.0, 1_284.0];

/// Put the keyboard on `key`, as Tab or a click would.
fn focus_on(runtime: &mut NativeRuntime, view: crate::native_app::ViewId, key: &str) {
    let Some(AppViewState::Settings(state)) = runtime.view_state_mut(view) else {
        unreachable!()
    };
    state.common.last_focus = Some(UiKey::new(key));
    state.common.focus_visible = true;
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

/// The theme id behind a row's slug.
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

/// The chooser's rows in the compiled tree, by key.
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

/// Whether an outcome writes anything: a config patch or a themes-folder
/// request.
fn writes(outcome: &crate::native_app::DispatchOutcome) -> bool {
    outcome.effects.iter().any(|effect| {
        matches!(
            effect,
            AppEffect::ConfigPatch { .. } | AppEffect::Themes { .. }
        )
    })
}

/// The highlight IS the trial: ↓ tries the next row, ↓ back onto the theme
/// in use ends the trial, and Return (a press on the focused row) keeps.
/// Negative control: in the filter, ← and → move the caret and try nothing.
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

    // Negative control: the filter's caret keys stay text editing.
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

/// From the Community disclosure the keys move the highlight as they do on
/// a row, and the keyboard goes with it: Down tries the first community
/// theme, Up the row over the disclosure, End the last row. Negative
/// control: plain focus landing on a row tries nothing (`hover_never_tries`).
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

/// Pointer hover is host paint (a tint) and reaches no reducer; the one
/// passive event a row gets is focus landing on it (Tab, a screen reader's
/// cursor). Landing on five rows tries nothing. Negative control: a press
/// on the same row tries it.
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

/// A community theme is tried from the pack, never the folder: trying and
/// reverting write nothing (no patch, no themes-folder request). Negative
/// control: Keep adds the file, then writes `theme` naming it once the
/// catalog lists it.
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

    // Negative control: Keep writes the file, then the theme.
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

/// A community add that fails keeps the trial, and the strip says why (in
/// its spoken label and its second line). A later catalog that lists the
/// theme writes nothing.
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

/// With Match system on and the Light side chosen, Keep writes that side
/// alone; the Dark side stays. Negative control: with one theme for both,
/// Keep writes the plain name.
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

/// Keep ▾'s chevron is there for a theme that carries type and can be kept
/// as colours alone, and opening it offers Keep colours only. Negative
/// control: trying a colours-only theme draws no chevron.
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

/// The strip holds one rect through every state, at every shape: Idle,
/// Idle with overrides, Trying, Trying a typography theme, Keeping, a
/// refused Keep, and Customize.
#[test]
fn the_strip_never_moves() {
    for width in SHAPES {
        let cx = view_cx_at(width, 714.0);
        // The strip, and where the name on its first line starts (its width
        // gives way to the buttons): a trial starting or ending moves neither.
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

/// Starting a trial moves no row: the list's first row is where it was.
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

/// A row the list holds off its screen says so in `controls` and comes into
/// view when a reader asks to scroll it there, with the keyboard left where
/// it was. Negative control: a key no list holds scrolls nothing.
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

/// Every theme option is a direct child of the list box, and nothing of a
/// row's face reaches the tree (its option carries the name, the value and
/// the state): under an option there is only a Yours row's file actions.
/// Negative control: those file actions are there, as controls.
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

/// The demo is on the page at every shape, dark and light, at least 180 px
/// tall and 40 columns wide; and on a short Compact body the list still
/// shows three rows.
#[test]
fn the_demo_is_present_at_every_shape() {
    for source in ["theme = \"Dracula\"\n", "theme = \"Solarized Light\"\n"] {
        for width in SHAPES {
            let (_service, runtime, instance, view) = themes_view(source);
            let cx = view_cx_at(width, 714.0);
            let compiled = compile_settings_view(&runtime, instance, view, &cx);
            let stage = rect_of(&compiled, STAGE);
            assert!(stage.height >= 180.0, "{source} {width}: {stage:?}");
            // 40 cells of the 12 px terminal face (0.6 em advance).
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

/// THE STACKED CHOOSER HOLDS A WHOLE STOP at the least window, whatever the
/// terminal's line height makes the demo's rows: the demo gives up rows
/// rather than leave the list short of its tallest stop. (Six rows on
/// Windows' default 1.1 line left the list 19pt at 584×348, and the
/// Community disclosure was cut to 25 of its 32pt.) Every screen draws its
/// controls inside the chooser, under a demo that still shows, each live
/// over exactly what shows of it and cut, if at all, only by the list's
/// edge (THE LIST'S EDGE RULE), and holds one at least whole.
#[test]
fn the_stacked_chooser_holds_a_whole_stop_at_every_line_height() {
    for line_height in ["1.0", "1.1", "1.5"] {
        let (_service, mut runtime, instance, view) =
            themes_view(&format!("line_height = {line_height}\n"));
        let cx = view_cx_at(584.0, 348.0);
        each_screen(&mut runtime, instance, view, &cx, |compiled, top| {
            let context = format!("line height {line_height}, screen {top}");
            assert!(
                rect_of(compiled, STAGE).height > 0.0,
                "{context}: the demo shows"
            );
            let chooser = rect_of(compiled, "settings/showcase/chooser");
            let list = rect_of(compiled, showcase_page::SHOWCASE_LIST);
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
                        paint.rect.y < list.y - 0.01 || paint.rect.bottom() > list.bottom() + 0.01,
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

/// No node crosses its column's edge, and while the list scrolls its rows
/// stop short of the scroll thumb's gutter.
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
                // A row and its parts.
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

/// The text a button paints, if it paints words.
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

/// Every control on the page, in every state, is a node with a key, a
/// label and an action, and a label holds the words the control shows
/// (WCAG 2.5.3; a count after " · " is the value's).
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

/// Every control's action is its key, so `act <key>` read off the live
/// tree reaches it, and pressing it is handled.
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

/// The filter is in the tree with no query; a query finds community
/// themes with the disclosure closed, and sections with no match go.
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
                .any(|card| card.id == id && card.matches("kanagawa"))
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

/// The demo's spoken value names the theme and why it rests, never the
/// frame: it holds across ten tape moments, and moves with a trial and with
/// Reduce Motion.
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

/// Customize stands in the chooser's place: its rows join the tree only
/// while it is open, and the list's rows leave it meanwhile.
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

/// The strip's live label says a trial once: it holds still across frames
/// while the trial stands, and says the settled word when it ends.
/// Negative control: focus landing on rows (the passive move) changes
/// nothing it says.
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

/// A community theme's trial ends when a file by its name lands in the
/// folder and does not load: the pack no longer stands in for it (Keep could
/// not write over that file), and the page says why. Negative control: a
/// file that loads keeps the trial.
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

/// A theme file that did not load is listed under NEEDS ATTENTION with its
/// reason and Open, and its row tries nothing. Negative control: a file
/// that loads is an ordinary row.
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
    // Negative control: the file that loads is a row like any other.
    let harbour = card_action(&runtime, view, "Harbour");
    let _ = act(&mut runtime, instance, view, &harbour, None);
    assert_eq!(intent(&runtime, view), show("Harbour"));
}

/// Beside the demo the chooser panel holds every part inside its inset, and
/// its footer sits on the panel's bottom edge whether the list is full or a
/// filter leaves it one row.
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

/// A section's caption starts on the rows' name column, and so do the
/// no-match line and the footer's label: one left edge for the chooser's
/// words.
#[test]
fn captions_and_names_share_one_column() {
    for width in SHAPES {
        let cx = view_cx_at(width, 934.0);
        let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
        let compiled = compile_settings_view(&runtime, instance, view, &cx);
        // The name is paint only: the row's option carries it.
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

/// A Customize colour row left to the theme shows the theme's colour in its
/// well (the button beside the field that opens the colour picker); one the
/// person set shows theirs.
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

/// Each Customize colour row is one line in every shape: its name, its
/// field and its Reset share the row's band, and the fields keep one right
/// edge whether or not a row has a Reset. A stacked row (the name over a
/// page-wide field) fails it.
#[test]
fn colour_rows_are_one_line_with_one_field_column() {
    let paint_rect = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == key)
            .map(|node| node.rect)
    };
    // Tall enough for the whole sheet, and short enough that it scrolls.
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
            // A row off the sheet's screen is in the tree where scrolling
            // brings it; its painted name is checked where it is drawn.
            let field = rect_of(&compiled, &format!("settings/control/{key}"));
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
    }
}

/// The chooser's names are its words, in the primary ink; its section
/// captions are the quieter voice over them. A name in the captions' ink
/// (the flat hierarchy this replaced) fails it.
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

/// While the theme follows the system, the ✓ marks the one theme the
/// segment's side draws, and moves with the segment: two ✓ would not say
/// which theme Keep replaces.
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

/// The filter shows a Clear over its end only while it holds a query, the
/// query's text stops short of it, and pressing it empties the filter and
/// gives the field the keyboard.
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

/// Every theme the chooser lists is in the tree, the rows off the list's
/// screen included, at the bounds scrolling would give them: a screen
/// reader and `controls` see the whole list. Only the screen's rows are
/// hit or in the Tab order, and only they and the peek the list's edge
/// cuts are painted. A list that holds only its screen (its rows off it
/// absent) fails it.
#[test]
fn every_listed_theme_is_in_the_tree_and_only_the_screen_is_live() {
    let cx = view_cx_at(1_284.0, 684.0);
    let (_service, mut runtime, instance, view) = themes_view("theme = \"Dracula\"\n");
    let _ = compile_settings_view(&runtime, instance, view, &cx);
    let order = state(&runtime, view).showcase.listed();
    assert!(order.len() > 12, "the list scrolls: {} rows", order.len());
    // Off the screen below it at the list's top, and above it at its end.
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
                // A row the edge cuts is painted as the list's peek; one
                // wholly off the screen is not painted at all.
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
    check(&compile_settings_view(&runtime, instance, view, &cx), "top");
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

/// A community theme's row under a closed Community disclosure is reached
/// as a hand reaches it: the disclosure opens and the row is listed. A row
/// no section holds is not.
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
