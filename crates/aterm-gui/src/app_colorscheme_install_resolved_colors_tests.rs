// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ThemeChange;
use crate::App;
use crate::app_config::{Config, ThemeCatalog};
use aterm_types::{Appearance, ColorScheme, Rgb};

/// Re-resolving with only `accent_color` changed reports `chrome_changed`, clears every
/// window's strip-row cache, and updates the header accent token.
#[test]
fn a_chrome_only_edit_repaints() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let seed = |app: &mut App| {
        app.windows.get_mut(&wid).expect("window").last_strip_fp = Some((
            0xFEED,
            80,
            false,
            None,
            false,
            None,
            crate::tab_bar::VersionSlotCols::NONE,
            false,
            crate::tab_bar::CaptionSlot::NONE,
            None,
        ));
    };
    let install = |app: &mut App, text: &str| {
        app.config = aterm_toml::from_str(text).expect("valid toml");
        app.install_resolved_colors(ThemeChange::ConfigReload)
    };
    let _ = install(&mut app, "theme = \"Nord\"\n");
    seed(&mut app);
    let same = install(&mut app, "theme = \"Nord\"\n");
    assert!(!same.chrome_changed && !same.theme_changed, "{same:?}");
    assert!(app.windows[&wid].last_strip_fp.is_some(), "CONTROL: kept");
    let edited = install(&mut app, "theme = \"Nord\"\naccent_color = \"#ff00aa\"\n");
    assert!(edited.chrome_changed, "{edited:?}");
    assert!(!edited.theme_changed, "{edited:?}");
    assert!(app.windows[&wid].last_strip_fp.is_none(), "strip cache");
    let header = app
        .chrome_palette(crate::app_render::ChromeSurface::Header)
        .tokens();
    assert_eq!(header.accent, [0xff, 0x00, 0xaa]);
}

/// Apply `text` as `aterm.toml` through the prepared config generation admission path.
pub(crate) fn apply_config_text(app: &mut App, dir: &std::path::Path, text: &str) {
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, text);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false)
            .expect("observe");
    let config: Config = aterm_toml::from_str(text).expect("valid toml");
    let assets = config.resolve_asset_catalog();
    let path_feed_fps = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    app.apply_prepared_config_generation(crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps,
        sparkle,
        fonts: None,
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
        warnings: Vec::new(),
    });
}

fn scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-install-colors-{tag}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("scratch dir")
}

/// The reply session 0 gives a program that asks `CSI ? 996 n`.
fn dsr_996(app: &App) -> Vec<u8> {
    let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
    term.process(b"\x1b[?996n");
    term.take_response().expect("DSR 996 answers")
}

#[cfg(unix)]
fn bg_of(name: &str) -> Rgb {
    aterm_types::scheme::builtin(name)
        .expect("built-in")
        .background
}

/// A light theme on a dark OS reports light: the palette on screen, not the
/// desktop.
#[test]
fn a_light_theme_on_a_dark_os_reports_light() {
    let dir = scratch_dir("dsr");
    let mut app = App::headless_for_test();
    assert_eq!(app.os_appearance, Appearance::Dark);
    apply_config_text(&mut app, dir.path(), "theme = \"GitHub Light\"\n");
    assert_eq!(
        app.theme.bg & 0x00FF_FFFF,
        0x00FF_FFFF,
        "GitHub Light is on screen"
    );
    assert_eq!(dsr_996(&app), b"\x1b[?997;2n", "GitHub Light reports light");

    apply_config_text(&mut app, dir.path(), "theme = \"Dracula\"\n");
    assert_eq!(dsr_996(&app), b"\x1b[?997;1n", "Dracula reports dark");

    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(
        dsr_996(&app),
        b"\x1b[?997;1n",
        "a light OS does not make Dracula light"
    );
}

/// Read whatever the program side of `fd` holds, without blocking.
#[cfg(unix)]
pub(crate) fn drain(fd: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    out
}

/// Switching Dracula to Solarized Light pushes `?997;2n` to a mode-2031 subscriber after the
/// new palette lands, while an unrelated config edit sends no push.
#[cfg(unix)]
#[test]
fn a_theme_switch_pushes_997_after_the_new_palette_and_an_unrelated_edit_does_not() {
    use std::os::fd::IntoRawFd;
    let dir = scratch_dir("push");
    let fds = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("pipe");
    assert_eq!(
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
        0,
        "nonblocking read end"
    );
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
    let mut app = App::headless_for_test_with_sink(sink);
    apply_config_text(&mut app, dir.path(), "theme = \"Dracula\"\n");
    crate::term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b[?2031h");
    assert!(drain(fds[0]).is_empty(), "subscribing pushes nothing");

    apply_config_text(&mut app, dir.path(), "theme = \"Solarized Light\"\n");
    assert_eq!(
        drain(fds[0]),
        b"\x1b[?997;2n",
        "the subscriber is told the palette turned light"
    );
    let new_bg = bg_of("Solarized Light");
    {
        let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
        assert_eq!(term.default_background(), new_bg, "the palette landed");
        term.process(b"\x1b]11;?\x1b\\");
        let reply =
            String::from_utf8(term.take_response().expect("OSC 11 answers")).expect("utf-8 reply");
        let hex = |c: u8| format!("{c:02x}{c:02x}");
        assert!(
            reply.contains(&format!(
                "rgb:{}/{}/{}",
                hex(new_bg.r),
                hex(new_bg.g),
                hex(new_bg.b)
            )),
            "a program answering the push with OSC 11 reads the new bg: {reply:?}"
        );
    }

    apply_config_text(
        &mut app,
        dir.path(),
        "theme = \"Solarized Light\"\ncopy_on_select = true\n",
    );
    assert!(
        drain(fds[0]).is_empty(),
        "an unrelated key change sends no push"
    );
    drop(app);
    unsafe { libc::close(fds[0]) };
}

/// A program's own background outranks the theme in what its session
/// reports (DSR 996 / mode 2031 / `status`), and a theme switch that leaves
/// that background alone tells the program nothing.
#[cfg(unix)]
#[test]
fn a_program_background_outranks_the_theme_until_it_resets() {
    use std::os::fd::IntoRawFd;
    let dir = scratch_dir("program-bg");
    let fds = aterm_uds::pipe()
        .map(|(r, w)| [r.into_raw_fd(), w.into_raw_fd()])
        .expect("pipe");
    assert_eq!(
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
        0,
        "nonblocking read end"
    );
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
    let mut app = App::headless_for_test_with_sink(sink);
    app.pool.insert(crate::stub_session(1));
    apply_config_text(&mut app, dir.path(), "theme = \"Dracula\"\n");
    let field = |record: &str, key: &str| {
        record
            .split_whitespace()
            .find_map(|kv| Some(kv.strip_prefix(key)?.strip_prefix('=')?.to_string()))
    };
    let dsr = |app: &App, id: u64| {
        let mut term = crate::term_lock(&app.pool.get(id).expect("session").term);
        term.process(b"\x1b[?996n");
        term.take_response().expect("DSR 996 answers")
    };
    let process = |app: &App, bytes: &[u8]| {
        let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
        term.process(bytes);
        term.take_response()
    };

    assert_eq!(process(&app, b"\x1b[?2031h"), None);
    assert_eq!(
        process(&app, b"\x1b]11;#ffffff\x07").as_deref(),
        Some(&b"\x1b[?997;2n"[..]),
        "the reader drains the push for the program's own light background"
    );
    assert_eq!(dsr(&app, 0), b"\x1b[?997;2n");
    let record = app.session_status_record(0).expect("status");
    assert_eq!(
        field(&record, "reported_scheme").as_deref(),
        Some("light"),
        "{record}"
    );
    assert_eq!(
        field(&record, "scheme_from").as_deref(),
        Some("program"),
        "{record}"
    );
    assert_eq!(field(&record, "bg_dark").as_deref(), Some("0"), "{record}");

    apply_config_text(&mut app, dir.path(), "theme = \"Solarized Light\"\n");
    assert!(drain(fds[0]).is_empty(), "the program's view is unchanged");
    assert_eq!(
        dsr(&app, 1),
        b"\x1b[?997;2n",
        "negative control: a session without an override follows the theme"
    );
    apply_config_text(&mut app, dir.path(), "theme = \"Dracula\"\n");
    assert!(drain(fds[0]).is_empty(), "nor back to a dark theme");
    assert_eq!(dsr(&app, 0), b"\x1b[?997;2n");
    assert_eq!(dsr(&app, 1), b"\x1b[?997;1n");

    assert_eq!(
        process(&app, b"\x1b]111\x07").as_deref(),
        Some(&b"\x1b[?997;1n"[..]),
        "OSC 111 lands on Dracula's baseline, with one push"
    );
    let record = app.session_status_record(0).expect("status");
    assert_eq!(
        field(&record, "reported_scheme").as_deref(),
        Some("dark"),
        "{record}"
    );
    assert_eq!(
        field(&record, "scheme_from").as_deref(),
        Some("theme"),
        "{record}"
    );
    drop(app);
    unsafe { libc::close(fds[0]) };
}

/// A theme file nothing draws changes no projection, so it costs no
/// `apply_config` fan-out.
#[test]
fn an_unused_theme_file_sends_no_apply_config() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
    let mine = ColorScheme {
        background: Rgb::new(0x10, 0x20, 0x30),
        ..ColorScheme::default()
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        mine.clone(),
    )]));
    let before = app.engine_color_fanouts;
    assert_eq!(
        app.session_factory
            .terminal_config
            .as_ref()
            .map(|tc| tc.default_background),
        Some(mine.background),
        "the configured file is installed"
    );

    app.reload_theme_catalog(ThemeCatalog::from_schemes([
        ("Mine".to_string(), mine.clone()),
        ("Unused".to_string(), ColorScheme::default()),
    ]));
    assert_eq!(
        app.engine_color_fanouts, before,
        "an unused .conf added sends no apply_config"
    );

    let edited = ColorScheme {
        background: Rgb::new(0x11, 0x20, 0x30),
        ..mine
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([
        ("Mine".to_string(), edited),
        ("Unused".to_string(), ColorScheme::default()),
    ]));
    assert_eq!(
        app.engine_color_fanouts,
        before + 1,
        "negative control: editing the drawn file fans out once"
    );
}

/// Changing the engine projection wakes every session's frame watchers.
#[test]
fn a_projection_change_rings_every_sessions_watchers() {
    let mut app = App::headless_for_test();
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(1));
    app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
    let mine = ColorScheme {
        background: Rgb::new(0x10, 0x20, 0x30),
        ..ColorScheme::default()
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        mine.clone(),
    )]));
    let watchers: Vec<_> = app
        .pool
        .iter()
        .map(|session| {
            let mut sub = crate::session_watch::Subscription::new();
            let _ = sub.watch_frame(&session.ctx.watchers);
            sub
        })
        .collect();
    assert_eq!(watchers.len(), 2, "two sessions");
    let rang = || {
        watchers
            .iter()
            .map(|w| w.wait(std::time::Duration::ZERO))
            .collect::<Vec<_>>()
    };

    app.reload_theme_catalog(ThemeCatalog::from_schemes([
        ("Mine".to_string(), mine.clone()),
        ("Unused".to_string(), ColorScheme::default()),
    ]));
    assert_eq!(rang(), [false, false], "an unused theme file rings nobody");

    let edited = ColorScheme {
        background: Rgb::new(0x11, 0x20, 0x30),
        ..mine
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([("Mine".to_string(), edited)]));
    assert_eq!(rang(), [true, true], "every session's watchers rang");
}

/// Two themes that differ only in an ANSI slot draw the same chrome but give
/// different image-meta fingerprints.
#[test]
fn themes_that_differ_only_in_ansi_fingerprint_differently() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
    let base = ColorScheme::default();
    let mut red = base.clone();
    red.ansi[1] = Rgb::new(0xff, 0x00, 0x00);
    let mut green = base;
    green.ansi[1] = Rgb::new(0x00, 0xff, 0x00);

    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        red.clone(),
    )]));
    let red_theme = app.theme;
    let red_fp = app.image_theme_fingerprint();
    app.reload_theme_catalog(ThemeCatalog::from_schemes([("Mine".to_string(), green)]));
    assert_eq!(app.theme, red_theme, "the chrome is identical");
    assert_ne!(
        app.image_theme_fingerprint(),
        red_fp,
        "an ANSI-only change moves the fingerprint"
    );
    app.reload_theme_catalog(ThemeCatalog::from_schemes([("Mine".to_string(), red)]));
    assert_eq!(
        app.image_theme_fingerprint(),
        red_fp,
        "the same colours fingerprint the same"
    );
}

/// A theme file's `selection_foreground` reaches the renderer knob, not only
/// the engine; the config key still wins over it.
#[test]
fn a_theme_files_selection_foreground_reaches_the_renderer() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
    let mine = ColorScheme {
        selection_fg: Some(Rgb::new(0x12, 0x34, 0x56)),
        ..ColorScheme::default()
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        mine.clone(),
    )]));
    assert_eq!(app.render_knobs.selection_fg, Some(0x0012_3456));
    assert_eq!(
        app.session_factory
            .terminal_config
            .as_ref()
            .and_then(|tc| tc.selection_foreground),
        Some(Rgb::new(0x12, 0x34, 0x56)),
        "the engine and the renderer agree"
    );

    app.config = aterm_toml::from_str("theme = \"Mine\"\nselection_foreground = \"#abcdef\"\n")
        .expect("valid toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    assert_eq!(
        app.render_knobs.selection_fg,
        Some(0x00AB_CDEF),
        "the config key wins over the theme file"
    );
}
