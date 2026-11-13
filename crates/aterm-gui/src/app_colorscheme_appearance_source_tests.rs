// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::app_config::WindowTheme;
use aterm_types::{Appearance, Rgb};

const SPLIT: &str = "theme = \"dark:Dracula,light:GitHub Light\"\n";

fn bg_of(name: &str) -> Rgb {
    aterm_types::scheme::builtin(name)
        .expect("built-in")
        .background
}

fn rgb_u32(rgb: Rgb) -> u32 {
    crate::app_config::rgb_u32(rgb)
}

/// The reply session 0 gives a program that asks `CSI ? 996 n`.
fn dsr_996(app: &App) -> Vec<u8> {
    let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
    term.process(b"\x1b[?996n");
    term.take_response().expect("DSR 996 answers")
}

/// One `key=value` field of a status record.
fn field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
    record
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// `window_theme = "dark"` forces the window chrome while still following an OS light flip for
/// a split theme.
#[test]
fn a_forced_dark_chrome_still_follows_an_injected_light_flip() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str(&format!("window_theme = \"dark\"\n{SPLIT}")).expect("toml");
    app.window_theme = app.config.window_theme_or_default();
    assert_eq!(
        app.window_theme,
        WindowTheme::Dark,
        "the chrome is forced dark"
    );
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    assert_eq!(app.theme.bg & 0x00FF_FFFF, rgb_u32(bg_of("Dracula")));

    app.on_os_appearance(Appearance::Light);
    assert_eq!(
        app.theme.bg & 0x00FF_FFFF,
        rgb_u32(bg_of("GitHub Light")),
        "the light side is on screen under a forced dark chrome"
    );
    assert_eq!(dsr_996(&app), b"\x1b[?997;2n", "and reported");

    app.on_os_appearance(Appearance::Dark);
    assert_eq!(
        app.theme.bg & 0x00FF_FFFF,
        rgb_u32(bg_of("Dracula")),
        "negative control: the dark reading draws the dark side"
    );
    assert_eq!(dsr_996(&app), b"\x1b[?997;1n");
}

/// A launch built for a light OS reading needs no second `apply_config` fan-out at attach.
#[test]
fn a_launch_built_for_light_needs_no_second_apply_config_at_attach() {
    let launch = |read: Appearance| {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str(SPLIT).expect("toml");
        app.os_appearance = read;
        app.install_resolved_colors(super::ThemeChange::ConfigReload);
        app
    };

    let mut app = launch(Appearance::Light);
    assert_eq!(
        dsr_996(&app),
        b"\x1b[?997;2n",
        "session 0 reports light from launch"
    );
    let launched = app.engine_color_fanouts;
    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        app.engine_color_fanouts, launched,
        "attach finds the launch already on the light side"
    );
    assert_eq!(
        crate::term_lock(&app.pool.get(0).expect("session 0").term).default_background(),
        bg_of("GitHub Light"),
    );

    let mut cold = launch(Appearance::Dark);
    let launched = cold.engine_color_fanouts;
    cold.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        cold.engine_color_fanouts,
        launched + 1,
        "negative control: a launch built for dark re-applies every session at attach"
    );
}

/// `status` and the `appearance` line agree on OS reading, theme side, reported scheme, and
/// background darkness after a flip.
#[test]
fn status_appearance_fields_agree_after_a_flip() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str(SPLIT).expect("toml");
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    app.on_os_appearance(Appearance::Light);
    let record = app.session_status_record(0).expect("status");
    assert_eq!(field(&record, "os_appearance"), Some("light"), "{record}");
    assert_eq!(field(&record, "theme_side"), Some("light"), "{record}");
    assert_eq!(field(&record, "reported_scheme"), Some("light"), "{record}");
    assert_eq!(field(&record, "scheme_from"), Some("theme"), "{record}");
    assert_eq!(field(&record, "bg_dark"), Some("0"), "{record}");
    let line = app.appearance_status_line();
    assert_eq!(
        field(&line, "scheme_from"),
        None,
        "the appearance line describes the theme, not a session: {line}"
    );
    assert_eq!(field(&line, "os_appearance"), Some("light"), "{line}");
    assert_eq!(field(&line, "source"), Some("inert"), "{line}");
    assert_eq!(field(&line, "theme_side"), Some("light"), "{line}");
    assert_eq!(field(&line, "reported_scheme"), Some("light"), "{line}");
    assert_eq!(field(&line, "bg_dark"), Some("0"), "{line}");

    app.config = aterm_toml::from_str("theme = \"Dracula\"\n").expect("toml");
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    let record = app.session_status_record(0).expect("status");
    assert_eq!(field(&record, "os_appearance"), Some("light"), "{record}");
    assert_eq!(
        field(&record, "theme_side"),
        Some("-"),
        "a single theme has no side"
    );
    assert_eq!(
        field(&record, "reported_scheme"),
        Some("dark"),
        "negative control: Dracula reports dark on a light OS"
    );
    assert_eq!(field(&record, "bg_dark"), Some("1"), "{record}");

    app.config = crate::app_config::Config::default();
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    let record = app.session_status_record(0).expect("status");
    assert_eq!(app.installed_colors.id, "Default");
    assert_eq!(field(&record, "theme_side"), Some("-"), "{record}");
    assert_eq!(field(&record, "reported_scheme"), Some("dark"), "{record}");
}

/// On Linux with `window_theme = "auto"`, the chrome variant follows the drawn theme side.
#[cfg(target_os = "linux")]
#[test]
fn linux_auto_chrome_follows_the_os_through_the_theme_it_draws() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str(SPLIT).expect("toml");
    app.window_theme = app.config.window_theme_or_default();
    assert_eq!(app.window_theme, WindowTheme::Auto, "the default chrome");
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    assert_eq!(app.chrome_theme_for_apprt(), WindowTheme::Dark);

    app.on_os_appearance(Appearance::Light);
    assert_eq!(
        app.chrome_theme_for_apprt(),
        WindowTheme::Light,
        "a light desktop draws the light side, and its title bar"
    );
    app.on_os_appearance(Appearance::Dark);
    assert_eq!(app.chrome_theme_for_apprt(), WindowTheme::Dark);

    app.config = aterm_toml::from_str("theme = \"Dracula\"\n").expect("toml");
    app.install_resolved_colors(super::ThemeChange::ConfigReload);
    app.on_os_appearance(Appearance::Light);
    assert_eq!(
        app.chrome_theme_for_apprt(),
        WindowTheme::Dark,
        "a dark theme on a light desktop keeps a dark title bar over it"
    );
}
