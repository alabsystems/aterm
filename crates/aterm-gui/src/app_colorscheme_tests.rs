// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::theme_to_appearance;
use aterm_types::Appearance;
use aterm_window::window::Theme;

#[test]
fn os_scheme_reaches_terminal_leaves_inside_native_splits() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (mixed_session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(
        !app.windows[&wid]
            .layouts
            .iter()
            .any(|tree| tree.contains(mixed_session)),
        "negative control: the retired terminal-only projection cannot see this leaf"
    );

    app.config =
        aterm_toml::from_str(r#"theme = "dark:Dracula,light:GitHub Light""#).expect("split theme");
    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        crate::term_lock(&app.pool.get(0).unwrap().term).color_scheme(),
        Appearance::Light,
        "ordinary background terminal tabs still update"
    );
    assert_eq!(
        crate::term_lock(&app.pool.get(mixed_session).unwrap().term).color_scheme(),
        Appearance::Light,
        "a heterogeneous terminal leaf receives the same reported scheme"
    );
}

/// With the rendered theme unchanged, an OS appearance flip nudges every window, while an
/// equal-appearance reading nudges none (`NEGATIVE CONTROL:`).
#[test]
fn appearance_flip_nudges_every_window() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(9);
    app.install_window_state(wid, crate::stub_session(99), 38, 132);
    app.frontmost_window = Some(crate::WindowId(0));
    app.config = aterm_toml::from_str(r#"theme = "Dracula""#).expect("plain theme");
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    let before_theme = app.theme;
    let baseline = (
        app.windows[&crate::WindowId(0)]
            .redraw_requests_for_test
            .get(),
        app.windows[&wid].redraw_requests_for_test.get(),
    );

    app.sync_app_theme_to_appearance(Appearance::Dark);
    assert_eq!(app.os_appearance, Appearance::Dark);
    assert_eq!(
        (
            app.windows[&crate::WindowId(0)]
                .redraw_requests_for_test
                .get(),
            app.windows[&wid].redraw_requests_for_test.get(),
        ),
        baseline,
        "negative control: an unchanged OS appearance requests no redraw"
    );

    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(app.os_appearance, Appearance::Light);
    assert_eq!(app.theme, before_theme, "plain theme stays unchanged");
    assert_eq!(
        app.windows[&crate::WindowId(0)]
            .redraw_requests_for_test
            .get(),
        baseline.0 + 1,
        "front window nudged"
    );
    assert_eq!(
        app.windows[&wid].redraw_requests_for_test.get(),
        baseline.1 + 1,
        "background window nudged"
    );
}

/// The scheme baseline the session factory's projection carries.
fn factory_scheme(app: &crate::App) -> Appearance {
    app.session_factory
        .terminal_config
        .as_ref()
        .and_then(|tc| tc.color_scheme)
        .unwrap_or_default()
}

/// An OS light/dark flip that changes the palette appearance updates the session factory's
/// scheme baseline for newly spawned sessions.
#[test]
fn os_flip_updates_the_session_factory_appearance() {
    let mut app = crate::App::headless_for_test();
    app.config =
        aterm_toml::from_str(r#"theme = "dark:Dracula,light:GitHub Light""#).expect("split theme");
    assert_eq!(
        factory_scheme(&app),
        Appearance::Dark,
        "the factory is seeded to the engine default"
    );
    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        factory_scheme(&app),
        Appearance::Light,
        "BROKEN-2: the factory tracks the light side the OS picked"
    );

    let mut plain = crate::App::headless_for_test();
    plain.config = aterm_toml::from_str(r#"theme = "Dracula""#).expect("plain theme");
    plain.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        factory_scheme(&plain),
        Appearance::Dark,
        "negative control: a dark theme on a light OS still reports dark"
    );
}

#[test]
fn theme_maps_light_dark_and_unknown_to_default() {
    assert_eq!(theme_to_appearance(Some(Theme::Light)), Appearance::Light);
    assert_eq!(theme_to_appearance(Some(Theme::Dark)), Appearance::Dark);
    assert_eq!(theme_to_appearance(None), Appearance::Dark);
    assert_eq!(theme_to_appearance(None), Appearance::default());
}
