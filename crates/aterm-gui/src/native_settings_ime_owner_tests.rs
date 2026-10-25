// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{AppViewState, NativeApp, ViewId};
use crate::{App, WindowId};

const COMPOSED: &str = "候補";

fn state(app: &App, view: ViewId) -> &SettingsViewState {
    let Some(AppViewState::Settings(state)) = app.native_runtime.view_state(view) else {
        panic!("the Settings view is open");
    };
    state
}

fn input<'a>(
    state: &'a SettingsViewState,
    field: &str,
) -> &'a crate::native_text_input::TextInputState {
    match field {
        "settings/search" => &state.search_input,
        THEME_FILTER_ACTION => {
            &state
                .choice_picker
                .as_ref()
                .unwrap()
                .themes
                .as_ref()
                .unwrap()
                .filter
        }
        showcase_page::SHOWCASE_FILTER => &state.showcase.filter,
        showcase_page::SHOWCASE_SAVE_NAME => &state.showcase.save.as_ref().unwrap().name,
        showcase_page::SHOWCASE_PALETTE_SLOT => &state.showcase.slot.as_ref().unwrap().input,
        _ => &state.field_inputs[field.strip_prefix("settings/control/").unwrap()],
    }
}

#[test]
fn settings_copy_uses_the_text_field_that_received_select_all() {
    for (route, open, field) in [
        (
            SettingsRoute::Themes,
            showcase_page::SHOWCASE_FILTER.to_owned(),
            showcase_page::SHOWCASE_FILTER.to_owned(),
        ),
        (
            SettingsRoute::Themes,
            showcase_page::SHOWCASE_SAVE_AS.to_owned(),
            showcase_page::SHOWCASE_SAVE_NAME.to_owned(),
        ),
        (
            SettingsRoute::Themes,
            "settings/showcase/palette/0".to_owned(),
            showcase_page::SHOWCASE_PALETTE_SLOT.to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            format!("settings/set/{}", prefs::EDIT_THEME),
            THEME_FILTER_ACTION.to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            "settings/search".to_owned(),
            "settings/search".to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            format!("settings/set/{}", prefs::EDIT_FONT_VARIATION),
            format!("settings/control/{}", prefs::EDIT_FONT_VARIATION),
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(route));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(open),
                value: None,
            }),
        )
        .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::FocusChanged(Some(UiKey::new(field.as_str()))),
        )
        .unwrap();
        app.on_ime_commit(wid, "αβ chosen".into());
        let (_, view) = app.active_native_view(wid).unwrap();
        let expected = input(state(&app, view), &field).value().to_owned();
        assert!(!expected.is_empty(), "the field contains text: {field}");
        app.select_all_in(wid);
        assert_eq!(
            input(state(&app, view), &field).selected_text(),
            expected,
            "Select All belongs to {field}"
        );
        crate::control::pbcopy("untouched clipboard");
        assert!(app.native_has_selection(wid), "Copy is ready: {field}");
        assert!(app.copy_selection_in(wid), "Copy belongs to {field}");
        assert_eq!(
            crate::control::pbpaste().as_deref(),
            Some(expected.as_str()),
            "Copy exports the same field: {field}"
        );

        app.dispatch_native_event(wid, AppEvent::FocusChanged(None))
            .unwrap();
        crate::control::pbcopy("untouched clipboard");
        assert!(
            !app.native_has_selection(wid),
            "a blurred field is parked: {field}"
        );
        assert!(
            !app.copy_selection_in(wid),
            "no text field owns input: {field}"
        );
        assert_eq!(
            crate::control::pbpaste().as_deref(),
            Some("untouched clipboard")
        );
    }
}

#[test]
fn settings_ime_retirement_preserves_values_and_clears_only_the_original_projection() {
    for (route, open, field) in [
        (
            SettingsRoute::Themes,
            showcase_page::SHOWCASE_FILTER.to_owned(),
            showcase_page::SHOWCASE_FILTER.to_owned(),
        ),
        (
            SettingsRoute::Themes,
            showcase_page::SHOWCASE_SAVE_AS.to_owned(),
            showcase_page::SHOWCASE_SAVE_NAME.to_owned(),
        ),
        (
            SettingsRoute::Themes,
            "settings/showcase/palette/0".to_owned(),
            showcase_page::SHOWCASE_PALETTE_SLOT.to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            format!("settings/set/{}", prefs::EDIT_THEME),
            THEME_FILTER_ACTION.to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            "settings/search".to_owned(),
            "settings/search".to_owned(),
        ),
        (
            SettingsRoute::Appearance,
            format!("settings/set/{}", prefs::EDIT_FONT_VARIATION),
            format!("settings/control/{}", prefs::EDIT_FONT_VARIATION),
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(route));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(open),
                value: None,
            }),
        )
        .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::FocusChanged(Some(UiKey::new(field.as_str()))),
        )
        .unwrap();
        app.on_ime_commit(wid, "kept".into());
        let (instance, view) = app.active_native_view(wid).unwrap();
        assert!(matches!(
            app.native_runtime.app(instance),
            Some(NativeApp::Settings(_))
        ));
        let before = input(state(&app, view), &field).clone();
        app.on_ime_preedit(wid, COMPOSED.into(), None);
        assert!(
            input(state(&app, view), &field).preedit().is_some(),
            "{field}"
        );

        // A separate, real input's marked text must not be swept up in the cleanup.
        let Some(AppViewState::Settings(current)) = app.native_runtime.view_state_mut(view) else {
            unreachable!();
        };
        if field == showcase_page::SHOWCASE_FILTER {
            current.search_input.set_preedit("independent".into(), None);
        } else {
            current
                .showcase
                .filter
                .set_preedit("independent".into(), None);
        }
        app.dispatch_native_event(wid, AppEvent::FocusChanged(None))
            .unwrap();
        app.on_ime_preedit(wid, String::new(), None);
        app.on_ime_commit(wid, COMPOSED.into());
        let current = state(&app, view);
        let original = input(current, &field);
        assert!(original.preedit().is_none(), "retired projection: {field}");
        assert_eq!(original.value(), before.value(), "stored value: {field}");
        assert_eq!(
            original.selection(),
            before.selection(),
            "selection: {field}"
        );
        assert!(current.common.last_focus.is_none());
        assert!(
            if field == showcase_page::SHOWCASE_FILTER {
                current.search_input.preedit().is_some()
            } else {
                current.showcase.filter.preedit().is_some()
            },
            "the independent projection survives: {field}"
        );

        app.dispatch_native_event(
            wid,
            AppEvent::FocusChanged(Some(UiKey::new(field.as_str()))),
        )
        .unwrap();
        app.on_ime_preedit(wid, COMPOSED.into(), None);
        app.on_ime_preedit(wid, String::new(), None);
        app.on_ime_commit(wid, COMPOSED.into());
        assert_eq!(
            input(state(&app, view), &field).value(),
            format!("{}{COMPOSED}", before.value()),
            "fresh composition: {field}"
        );
    }
}
