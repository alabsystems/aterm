// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, AuxTarget};
use crate::WindowId;
use crate::native_settings::SettingsRoute;
use crate::native_ui::UiKey;

#[test]
fn update_alias_emits_the_exact_retained_native_frame() {
    {
        let (target, route, retired_prefix) =
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update ");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(route));
        assert!(app.prepare_native_input_scratch(wid));

        let (expected_ui, actionable, semantics) = {
            let frame = app.windows[&wid]
                .native_ui_compiled
                .as_ref()
                .expect("native Settings frame retained beside its raster");
            assert_eq!(frame.stamp.view, app.active_native_view(wid).unwrap().1);
            assert!(
                frame
                    .compiled
                    .semantic(&UiKey::new(format!("settings/page{}", route.path())))
                    .is_some(),
                "the retained frame owns the requested {route:?} page root"
            );
            (
                frame.compiled.controls_lines(),
                frame
                    .compiled
                    .semantics
                    .iter()
                    .filter(|node| node.action.is_some())
                    .count(),
                frame.compiled.semantics.len(),
            )
        };

        let actual = app.read_aux_controls(target);
        assert!(
            actual[0].contains("state open=true")
                && actual[0].contains("pane=native-tab")
                && actual[0].contains(&format!("route={}", route.path()))
                && actual[0].contains(&format!("page={:?}", route.label())),
            "alias identity must name the live native route: {:?}",
            actual[0]
        );
        assert!(actual.iter().any(|line| {
            line == &format!(
                "native surface=native-tab route={} controls={actionable} semantics={semantics}",
                route.path()
            )
        }));
        let actual_ui = actual
            .iter()
            .filter(|line| line.starts_with("ui "))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            actual_ui, expected_ui,
            "compatibility alias must serialize the exact semantic artifact retained for paint"
        );
        assert!(
            actual.iter().all(|line| !line.starts_with(retired_prefix)),
            "retired standalone projection leaked through {target:?}: {actual:?}"
        );
    }
}

fn split_record(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut token = String::new();
    let (mut quoted, mut escaped) = (false, false);
    for ch in line.chars() {
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch.is_whitespace() && !quoted {
            if !token.is_empty() {
                out.push(std::mem::take(&mut token));
            }
            continue;
        }
        token.push(ch);
    }
    if !token.is_empty() {
        out.push(token);
    }
    out
}

#[test]
fn controls_settings_lists_showcase_rows() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 160;
        window.rows = 50;
    }
    assert!(app.open_settings_tab(SettingsRoute::Themes));
    let lines = app.read_aux_controls(AuxTarget::Prefs);
    let listed = |lines: &[String], key: &str| {
        lines
            .iter()
            .any(|line| line.starts_with(&format!("ui key=\"{key}\" ")))
    };
    let row = lines
        .iter()
        .find(|line| {
            line.starts_with("ui key=\"settings/showcase/card/")
                && line.contains(" role=ListBoxOption ")
        })
        .unwrap_or_else(|| panic!("a row record: {lines:#?}"))
        .clone();
    let tokens = split_record(&row);
    assert!(
        tokens.iter().any(|token| token.starts_with("value=")
            && token.contains("contrast ")
            && token.contains(":1")),
        "{row}"
    );
    for key in [
        "settings/showcase/list",
        "settings/showcase/filter",
        "settings/showcase/status",
        "settings/showcase/customize",
    ] {
        assert!(listed(&lines, key), "{key}: {lines:#?}");
    }
    for key in ["settings/showcase/keep", "settings/showcase/revert"] {
        assert!(!listed(&lines, key), "idle: {key}");
    }
    let (_, instance, view, state) = app.native_settings_view_target().unwrap();
    let in_use = state.highlighted_slug();
    let other = lines
        .iter()
        .filter_map(|line| line.strip_prefix("ui key=\"settings/showcase/card/"))
        .filter_map(|rest| rest.split('"').next())
        .find(|slug| !slug.contains('/') && Some(slug.to_string()) != in_use)
        .expect("a row not in use")
        .to_string();
    let press = |app: &mut App, key: &str| {
        let _ = app.native_runtime.dispatch(
            instance,
            view,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(key),
                value: None,
            }),
        );
    };
    press(&mut app, &format!("settings/showcase/card/{other}"));
    let lines = app.read_aux_controls(AuxTarget::Prefs);
    for key in ["settings/showcase/keep", "settings/showcase/revert"] {
        assert!(listed(&lines, key), "trying: {key}");
    }
    press(&mut app, "settings/showcase/revert");
    press(&mut app, "settings/showcase/customize");
    let lines = app.read_aux_controls(AuxTarget::Prefs);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with(&format!("field key={} ", crate::prefs::EDIT_FOREGROUND))),
        "{lines:#?}"
    );
    assert!(listed(&lines, "settings/showcase/palette/0"), "{lines:#?}");
    assert!(
        !lines
            .iter()
            .any(|line| line.contains(" role=ListBoxOption ")),
        "the list leaves while the sheet stands in its place"
    );
}

#[test]
fn controls_theme_options_are_quoted_and_split_whole() {
    let mut app = App::headless_for_test();
    app.native_config_service =
        crate::native_config_service::VersionedConfigService::new("theme = \"Nord\"\n".to_string())
            .unwrap();
    let wid = WindowId(0);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 160;
        window.rows = 50;
    }
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let lines = app.read_aux_controls(AuxTarget::Prefs);
    let theme = lines
        .iter()
        .find(|line| line.starts_with(&format!("field key={} ", crate::prefs::EDIT_THEME)))
        .expect("Top Settings exposes the theme picker");
    let tokens = split_record(theme);
    let options = tokens
        .iter()
        .find_map(|token| token.strip_prefix("options="))
        .expect("theme options");
    assert!(
        options.contains("\"Solarized Dark\""),
        "a spaced name is one quoted option: {options}"
    );
    assert!(
        tokens.iter().all(|token| !token.starts_with("Dark")),
        "no option leaks a stray token: {tokens:?}"
    );
    assert!(
        tokens.iter().skip(1).all(|token| token.contains('=')),
        "{tokens:?}"
    );
    assert!(
        theme
            .split_whitespace()
            .any(|token| token.starts_with("Dark\""))
    );
}

#[test]
fn route_aliases_never_describe_a_matching_background_window() {
    {
        let (target, background_route, alias) =
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update");
        let mut app = App::headless_for_test();
        let background = WindowId(0);
        assert!(app.open_settings_tab(background_route));
        let (_, background_view) = app
            .active_native_view(background)
            .expect("background Settings route");

        let session = app.next_session_id;
        let front = app.insert_logical_window(crate::stub_session(session), 24, 80);
        assert_eq!(app.frontmost_window, Some(front));
        assert!(app.open_settings_tab(SettingsRoute::Home));
        assert!(matches!(
            app.native_runtime.view_state(background_view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == background_route
        ));
        assert!(matches!(
            app.active_native_view(front)
                .and_then(|(_, view)| app.native_runtime.view_state(view)),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == SettingsRoute::Home
        ));

        let lines = app.read_aux_controls(target);
        assert!(
            lines[0].contains("route=/home"),
            "front-route mismatch must be explicit: {:?}",
            lines[0]
        );
        assert!(lines[2].contains(&format!(
            "alias={alias} route=/home expected-route={}",
            background_route.path()
        )));
        assert!(
            lines.iter().all(|line| !line.starts_with("ui ")),
            "background semantics must not describe the front capture: {lines:?}"
        );
    }
}
