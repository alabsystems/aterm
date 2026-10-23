// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn settings_trial(
    app: &mut App,
    id: &str,
) -> (
    crate::tab_model::AppInstanceId,
    crate::tab_model::ViewId,
    String,
) {
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let (_, instance, view, state) = app.native_settings_view_target().unwrap();
    let slug = state
        .showcase
        .cards
        .iter()
        .find(|card| card.id == id)
        .unwrap()
        .slug
        .clone();
    let action = format!("settings/showcase/card/{slug}");
    app.native_runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(action.as_str()),
                value: None,
            }),
        )
        .unwrap();
    app.sync_settings_theme_trial(std::time::Instant::now());
    assert_eq!(app.installed_colors.id, id);
    (instance, view, action)
}

fn keep_settings_trial(
    app: &mut App,
    instance: crate::tab_model::AppInstanceId,
    view: crate::tab_model::ViewId,
    action: &str,
) {
    app.native_config_inflight = true;
    let effects = app
        .native_runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(action),
                value: None,
            }),
        )
        .unwrap()
        .effects;
    let effect = effects
        .into_iter()
        .find(|effect| matches!(effect, AppEffect::ConfigPatch { .. }))
        .expect("Keep emits its real patch");
    app.execute_native_effect(WindowId(0), instance, view, effect)
        .unwrap();
    app.sync_settings_theme_trial(std::time::Instant::now());
}

fn complete_settings_save(app: &mut App, rejected: bool) {
    let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
    app.native_config_inflight = false;
    if rejected {
        app.publish_native_config_origin(
            origin,
            ConfigPatchOutcome::Rejected {
                message: "owned refusal".to_string(),
            },
            None,
            false,
            None,
        );
    } else {
        let (snapshot, undo) = match app.reduce_native_config_work(work).unwrap() {
            ConfigPatchResult::Applied { snapshot, undo } => (snapshot, undo),
            other => panic!("Settings Keep applies: {other:?}"),
        };
        app.publish_native_config_origin(
            origin,
            ConfigPatchOutcome::Applied {
                revision: snapshot.revision,
                undo: Some(undo.get()),
            },
            Some(snapshot),
            false,
            None,
        );
    }
}

fn app() -> App {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "theme = \"Dracula\"\n".to_string(),
    )
    .unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    app
}

#[test]
fn settings_keep_waits_for_its_completion_after_the_view_closes() {
    for rejected in [false, true] {
        let mut app = app();
        let (instance, view, action) = settings_trial(&mut app, "Nord");
        keep_settings_trial(&mut app, instance, view, &action);
        assert!(app.close_settings_tabs());
        app.sync_settings_theme_trial(std::time::Instant::now());
        assert_eq!(
            app.next_theme_preview_wake(),
            None,
            "a real queued save has no TTL"
        );
        assert!(!app.expire_theme_preview(
            std::time::Instant::now() + std::time::Duration::from_secs(86_400)
        ));
        assert_eq!(app.installed_colors.id, "Nord");
        complete_settings_save(&mut app, rejected);
        assert!(
            app.theme_preview.is_none(),
            "closed-view completion retires its own hold"
        );
        assert_eq!(
            app.config.theme.as_deref(),
            Some(if rejected { "Dracula" } else { "Nord" })
        );
        assert_eq!(
            app.installed_colors.id,
            if rejected { "Dracula" } else { "Nord" }
        );
    }
}

#[test]
fn settings_keep_completion_preserves_a_newer_preview_and_saved_truth() {
    for rejected in [false, true] {
        let mut app = app();
        let (instance, view, action) = settings_trial(&mut app, "Nord");
        keep_settings_trial(&mut app, instance, view, &action);
        assert!(app.close_settings_tabs());
        let now = std::time::Instant::now();
        app.preview_theme("GitHub Light", std::time::Duration::from_secs(60), now)
            .unwrap();
        complete_settings_save(&mut app, rejected);
        let saved = if rejected { "Dracula" } else { "Nord" };
        assert_eq!(app.config.theme.as_deref(), Some(saved));
        assert_eq!(
            app.installed_colors.id, "GitHub Light",
            "A cannot retire or flash through B"
        );
        assert_eq!(app.theme_preview.as_ref().unwrap().restore, saved);
        app.revert_theme_preview(now);
        assert_eq!(app.installed_colors.id, saved);
    }
}

#[test]
fn settings_keep_is_newer_intent_than_an_older_queued_theme_set() {
    let mut app = app();
    let (instance, view, action) = settings_trial(&mut app, "Nord");
    let (reply, answer) = std::sync::mpsc::channel();
    app.enqueue_control_settings_field_intent(
        crate::prefs::EDIT_THEME.to_string(),
        Some("GitHub Light".to_string()),
        reply,
    );
    keep_settings_trial(&mut app, instance, view, &action);
    complete_queued_control_theme_field(&mut app);
    answer.try_recv().unwrap().unwrap();
    assert_eq!(app.config.theme.as_deref(), Some("GitHub Light"));
    assert_eq!(
        app.installed_colors.id, "Nord",
        "the later Keep still owns the overlay"
    );
    assert!(app.theme_preview.as_ref().unwrap().commit_request.is_some());
}

#[test]
fn settings_keep_completion_does_not_reclaim_a_new_settings_trial() {
    for id in ["GitHub Light", "Nord"] {
        for rejected in [false, true] {
            let mut app = app();
            let (instance, view, action) = settings_trial(&mut app, "Nord");
            keep_settings_trial(&mut app, instance, view, &action);
            assert!(app.close_settings_tabs());
            settings_trial(&mut app, id);
            complete_settings_save(&mut app, rejected);
            let saved = if rejected { "Dracula" } else { "Nord" };
            assert_eq!(app.config.theme.as_deref(), Some(saved));
            let preview = app.theme_preview.as_ref().expect("the later trial remains");
            assert_eq!(preview.id, id);
            assert_eq!(preview.restore, saved);
            assert!(
                preview.commit_request.is_none(),
                "this trial did not ask to save"
            );
            assert_eq!(app.installed_colors.id, id);
        }
    }
}

fn pending_font_text_save() -> (App, crate::tab_model::ViewId) {
    let mut app = App::headless_for_test();
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        "font_family = \"First Mono\"\n".to_string(),
    )
    .unwrap();
    app.config = (*app.native_config_service.snapshot().config).clone();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let (_, view) = app.active_native_view(wid).unwrap();
    let key = crate::native_ui::UiKey::new("settings/control/font_family");
    app.dispatch_native_event(wid, AppEvent::FocusChanged(Some(key.clone())))
        .unwrap();
    assert!(app.native_input_event(
        wid,
        &crate::input::InputEvent::Text("Saved Mono".to_string())
    ));
    assert!(app.native_input_event(
        wid,
        &crate::input::InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Enter),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    ));
    let before = app.compiled_native_ui(wid).unwrap();
    let field = before
        .semantic(&key)
        .expect("the font text field is visible");
    assert_eq!(field.role, crate::native_ui::SemanticRole::TextField);
    assert!(field.state.is_none_or(|state| state.enabled));

    app.native_config_inflight = true;
    app.dispatch_native_event(
        wid,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/set/font_family"),
            value: Some(crate::native_app::SemanticInput::Text(
                "Saved Mono".to_string(),
            )),
        }),
    )
    .unwrap();
    assert_eq!(app.native_config_pending.len(), 1);
    let pending = app.compiled_native_ui(wid).unwrap();
    let field = pending
        .semantic(&key)
        .expect("the pending field stays visible");
    assert!(
        field
            .state
            .is_some_and(|state| state.enabled && !state.busy)
    );
    (app, view)
}

fn font_input(
    app: &App,
    view: crate::tab_model::ViewId,
) -> Option<crate::native_text_input::TextInputState> {
    match app.native_runtime.view_state(view).unwrap() {
        crate::native_app::AppViewState::Settings(state) => state
            .field_inputs
            .get(crate::prefs::EDIT_FONT_FAMILY)
            .cloned(),
        _ => panic!("Settings view"),
    }
}

#[test]
fn text_save_completion_preserves_newer_same_text_undo_history() {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

    let (mut app, view) = pending_font_text_save();
    let submitted = font_input(&app, view).unwrap();
    let wid = WindowId(0);
    assert!(app.native_input_event(wid, &crate::input::InputEvent::Text("X".to_string())));
    assert!(app.native_input_event(
        wid,
        &crate::input::InputEvent::Key {
            key: Key::Character('z'),
            mods: Modifiers::CTRL,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    ));
    let newer = font_input(&app, view).unwrap();
    assert_eq!(newer.value(), submitted.value());
    assert_ne!(newer, submitted, "the person owns newer redo history");

    complete_settings_save(&mut app, false);
    assert_eq!(
        app.native_config_service
            .snapshot()
            .config
            .font_family
            .as_deref(),
        Some("Saved Mono")
    );
    assert_eq!(font_input(&app, view), Some(newer));
}

#[test]
fn text_save_completion_retires_unchanged_draft_only_on_success() {
    for rejected in [false, true] {
        let (mut app, view) = pending_font_text_save();
        let submitted = font_input(&app, view).unwrap();
        complete_settings_save(&mut app, rejected);
        assert_eq!(font_input(&app, view), rejected.then_some(submitted));
        assert_eq!(
            app.native_config_service
                .snapshot()
                .config
                .font_family
                .as_deref(),
            Some(if rejected { "First Mono" } else { "Saved Mono" })
        );
    }
}

#[test]
fn a_held_arrow_installs_once_per_frame() {
    let mut app = app();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let _ = app.read_aux_controls(crate::app_introspect::AuxTarget::Prefs);
    let (_, instance, view, state) = app.native_settings_view_target().unwrap();
    let order = state.showcase.listed();
    let home = state.highlighted_slug().expect("the in-use row");
    let at = order.iter().position(|slug| *slug == home).expect("listed");
    let landing = order[at + 3].clone();
    let landing_id = state
        .showcase
        .cards
        .iter()
        .find(|card| card.slug == landing)
        .unwrap()
        .id
        .clone();
    let row = |slug: &str| crate::native_ui::UiKey::new(format!("settings/showcase/card/{slug}"));
    let _ = app
        .native_runtime
        .dispatch(instance, view, AppEvent::FocusChanged(Some(row(&home))))
        .unwrap();
    for _ in 0..3 {
        let _ = app
            .native_runtime
            .dispatch(instance, view, AppEvent::StepFocus(1))
            .unwrap();
    }
    assert_eq!(
        app.installed_colors.id, "Dracula",
        "a step installs nothing"
    );
    app.sync_settings_theme_trial(std::time::Instant::now());
    assert_eq!(app.installed_colors.id, landing_id);
    assert_eq!(
        app.theme_preview
            .as_ref()
            .map(|preview| preview.id.as_str()),
        Some(landing_id.as_str())
    );
}
