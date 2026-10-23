// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `theme` control verb and transient theme previews.

use std::time::{Duration, Instant};

use aterm_types::{Appearance, ColorScheme, Rgb};

use super::{THEME_PREVIEW_DEFAULT_TTL, ThemeCtlOp};
use crate::App;
use crate::app_colorscheme::ThemeChange;
use crate::app_config::ThemeCatalog;
use crate::control::control_media::{ThemeForm, parse_theme_form};

fn app_with(toml: &str) -> App {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str(toml).expect("valid toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    app
}

fn builtin(name: &str) -> ColorScheme {
    aterm_types::scheme::builtin(name).expect("built-in")
}

fn engine_colors(app: &App) -> (Vec<Rgb>, Rgb, Rgb) {
    let term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
    let palette = (0..=255u8).map(|i| term.palette_color(i)).collect();
    (
        palette,
        term.default_foreground(),
        term.default_background(),
    )
}

fn visible(app: &App) -> (aterm_render::Theme, (Vec<Rgb>, Rgb, Rgb), u64) {
    (app.theme, engine_colors(app), app.image_theme_fingerprint())
}

fn durable(app: &App) -> (String, u64, usize) {
    let snapshot = app.native_config_service.snapshot();
    (
        snapshot.text.to_string(),
        snapshot.revision,
        app.native_config_pending.len(),
    )
}

fn run(app: &mut App, op: ThemeCtlOp, now: Instant) -> Result<String, String> {
    run_on(app, op, None, now)
}

fn run_on(
    app: &mut App,
    op: ThemeCtlOp,
    session: Option<u64>,
    now: Instant,
) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.theme_control(op, session, now, tx);
    rx.try_recv().expect("answered at once")
}

fn preview(app: &mut App, id: &str, ttl: Duration, now: Instant) -> Result<String, String> {
    run(
        app,
        ThemeCtlOp::Preview {
            id: id.to_string(),
            ttl,
        },
        now,
    )
}

#[test]
fn control_theme_preview_draws_without_writing_and_revert_restores_exactly() {
    let mut app = app_with("theme = \"Dracula\"\n");
    let now = Instant::now();
    let before = visible(&app);
    let durable_before = durable(&app);

    let status = preview(&mut app, "Solarized Light", Duration::from_secs(30), now)
        .expect("a built-in previews");
    assert!(
        status.contains("id=\"Solarized Light\"") && status.contains("preview=\"Solarized Light\""),
        "{status}"
    );
    assert!(status.contains("restore=\"Dracula\""), "{status}");
    let light = builtin("Solarized Light");
    assert_eq!(app.theme, aterm_render::Theme::from(&light));
    let (palette, fg, bg) = engine_colors(&app);
    assert_eq!(bg, light.background, "the session draws the preview");
    assert_eq!(fg, light.foreground);
    assert_eq!(
        &palette[..16],
        &light.ansi[..],
        "the whole ANSI palette moved"
    );
    assert_eq!(durable(&app), durable_before, "a preview writes nothing");

    run(&mut app, ThemeCtlOp::Revert, now).expect("revert answers");
    assert!(app.theme_preview.is_none());
    assert_eq!(visible(&app), before, "revert restores what was drawn");
    assert_eq!(durable(&app), durable_before);

    let refused = preview(&mut app, "Nrod", Duration::from_secs(30), now)
        .expect_err("negative control: an unavailable name is refused");
    assert!(refused.contains("unavailable"), "{refused}");
    assert!(app.theme_preview.is_none());
    assert_eq!(visible(&app), before, "a refused preview changes nothing");
    let refused = preview(&mut app, "dark:Nord,light:", Duration::from_secs(30), now)
        .expect_err("a selector with an issue is refused");
    assert!(refused.contains("theme preview"), "{refused}");
    assert_eq!(visible(&app), before);
}

#[test]
fn control_theme_preview_expires_on_the_injected_clock() {
    let mut app = app_with("theme = \"Dracula\"\n");
    let before = visible(&app);
    let t0 = Instant::now();
    let ttl = Duration::from_secs(5);
    preview(&mut app, "Nord", ttl, t0).expect("previews");
    assert_eq!(app.next_theme_preview_wake(), Some(t0 + ttl));

    assert!(!app.expire_theme_preview(t0 + ttl - Duration::from_millis(1)));
    assert_eq!(app.theme, aterm_render::Theme::from(&builtin("Nord")));
    let status = run(&mut app, ThemeCtlOp::Status, t0 + Duration::from_secs(2)).unwrap();
    assert!(status.contains("ttl_ms=3000"), "{status}");

    assert!(app.expire_theme_preview(t0 + ttl), "due at its instant");
    assert!(app.theme_preview.is_none());
    assert_eq!(app.next_theme_preview_wake(), None, "nothing left to arm");
    assert_eq!(visible(&app), before, "the configured theme is back");
    assert!(!app.expire_theme_preview(t0 + ttl * 2), "expires once");
}

#[test]
fn control_theme_preview_ends_at_a_reload_and_follows_an_os_flip() {
    let mut app = app_with("theme = \"Dracula\"\n");
    let now = Instant::now();
    preview(
        &mut app,
        "dark:Nord,light:GitHub Light",
        THEME_PREVIEW_DEFAULT_TTL,
        now,
    )
    .expect("a split previews");
    assert_eq!(app.installed_colors.id, "Nord");
    app.sync_app_theme_to_appearance(Appearance::Light);
    assert!(app.theme_preview.is_some(), "an OS flip keeps the preview");
    assert_eq!(
        app.installed_colors.id, "GitHub Light",
        "and draws its light side"
    );

    preview(&mut app, "Nord", THEME_PREVIEW_DEFAULT_TTL, now).expect("previews again");
    assert_eq!(
        app.theme_preview.as_ref().map(|p| p.restore.as_str()),
        Some("Dracula"),
        "a preview over a preview keeps what to restore"
    );

    app.install_resolved_colors(ThemeChange::CatalogReload);
    assert!(
        app.theme_preview.is_some(),
        "a catalog reload keeps a preview that still draws"
    );
    assert_eq!(app.installed_colors.id, "Nord");

    let colors = app
        .config
        .resolved_colors(app.os_appearance, &app.config_assets.themes, None);
    app.install_colors(colors, ThemeChange::ConfigReload);
    assert!(app.theme_preview.is_none(), "a config reload ends it");
    assert_eq!(app.installed_colors.id, "Dracula");

    preview(&mut app, "Nord", THEME_PREVIEW_DEFAULT_TTL, now).expect("previews");
    app.install_resolved_colors(ThemeChange::Rollback);
    assert_eq!(app.installed_colors.id, "Nord");
}

#[test]
fn a_catalog_reload_redraws_a_preview_and_ends_one_whose_file_left() {
    let scheme = |bg| ColorScheme {
        background: bg,
        ..ColorScheme::default()
    };
    let first = Rgb::new(0x10, 0x20, 0x30);
    let saved = Rgb::new(0x40, 0x50, 0x60);
    let catalog = |bg| ThemeCatalog::from_schemes([("Work".to_string(), scheme(bg))]);
    for hold in [
        super::PreviewHold::Until(Instant::now() + THEME_PREVIEW_DEFAULT_TTL),
        super::PreviewHold::Committing,
        super::PreviewHold::Settings { keeping: true },
    ] {
        let mut app = app_with("theme = \"Dracula\"\n");
        app.reload_theme_catalog(catalog(first));
        app.begin_theme_preview("Work", hold).expect("previews");
        assert_eq!(engine_colors(&app).2, first);

        app.reload_theme_catalog(catalog(saved));
        assert_eq!(
            app.theme_preview.as_ref().map(|p| p.hold),
            Some(hold),
            "{hold:?}: the save keeps the preview"
        );
        assert_eq!(engine_colors(&app).2, saved, "{hold:?}: and redraws it");

        app.reload_theme_catalog(ThemeCatalog::empty());
        assert!(app.theme_preview.is_none(), "{hold:?}: the file left");
        assert_eq!(app.installed_colors.id, "Dracula");
    }
}

#[cfg(unix)]
#[test]
fn a_config_reload_keeps_a_settings_trial_without_a_bounce() {
    let dir = std::env::temp_dir().join(format!("aterm-trial-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    assert_eq!(
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
    let mut app = App::headless_for_test_with_sink(sink);
    let apply = crate::app_colorscheme::install_resolved_colors::apply_config_text;
    let drain = crate::app_colorscheme::install_resolved_colors::drain;
    apply(&mut app, &dir, "theme = \"Dracula\"\n");
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    crate::term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b[?2031h");
    let now = Instant::now();
    set_settings_trial(&mut app, trying("GitHub Light"));
    app.sync_settings_theme_trial(now);
    assert_eq!(drain(fds[0]), b"\x1b[?997;2n", "the trial is light");

    let fanouts = app.engine_color_fanouts;
    apply(
        &mut app,
        &dir,
        "theme = \"Dracula\"\ncopy_on_select = true\n",
    );
    app.sync_settings_theme_trial(now);
    assert_eq!(app.installed_colors.id, "GitHub Light");
    assert_eq!(
        app.theme_preview.as_ref().map(|p| p.hold),
        Some(super::PreviewHold::Settings { keeping: false })
    );
    assert!(drain(fds[0]).is_empty(), "no light-dark-light bounce");
    assert!(
        app.engine_color_fanouts <= fanouts + 1,
        "one install at most"
    );

    set_settings_trial(
        &mut app,
        crate::native_settings::showcase_page::ShowcaseTrial::Idle,
    );
    app.sync_settings_theme_trial(now);
    assert_eq!(drain(fds[0]), b"\x1b[?997;1n", "the trial reverted");
    preview(&mut app, "GitHub Light", THEME_PREVIEW_DEFAULT_TTL, now).expect("previews");
    assert_eq!(drain(fds[0]), b"\x1b[?997;2n");
    apply(&mut app, &dir, "theme = \"Dracula\"\n");
    assert!(
        app.theme_preview.is_none(),
        "negative control: a ctl preview ends"
    );
    assert_eq!(drain(fds[0]), b"\x1b[?997;1n");
    drop(app);
    unsafe { libc::close(fds[0]) };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn control_theme_program_osc11_survives_preview_then_revert() {
    let mut app = app_with("theme = \"Dracula\"\n");
    let wid = crate::WindowId(0);
    let other = app.split_active_stub_tab(wid);
    let program_bg = Rgb::new(0x12, 0x34, 0x56);
    crate::term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b]11;#123456\x1b\\");
    let bg = |app: &App, session| {
        crate::term_lock(&app.pool.get(session).expect("session").term).default_background()
    };
    assert_eq!(bg(&app, 0), program_bg, "the program's set landed");

    let now = Instant::now();
    preview(&mut app, "GitHub Light", THEME_PREVIEW_DEFAULT_TTL, now).expect("previews");
    assert_eq!(
        bg(&app, 0),
        program_bg,
        "the preview keeps the program's bg"
    );
    assert_eq!(
        bg(&app, other),
        builtin("GitHub Light").background,
        "negative control: an untouched session draws the preview"
    );

    run(&mut app, ThemeCtlOp::Revert, now).expect("reverts");
    assert_eq!(bg(&app, 0), program_bg, "and so does the revert");
    assert_eq!(bg(&app, other), builtin("Dracula").background);

    crate::term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b]111\x1b\\");
    assert_eq!(bg(&app, 0), builtin("Dracula").background);
}

#[test]
fn control_theme_status_names_source_side_miss_and_palette() {
    let mut app = App::headless_for_test();
    let work = ColorScheme {
        background: Rgb::new(0x10, 0x20, 0x30),
        ..ColorScheme::default()
    };
    app.config = aterm_toml::from_str("theme = \"dark:Work,light:Nrod\"\n").expect("valid toml");
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Work".to_string(),
        work.clone(),
    )]));
    let now = Instant::now();
    let status = run(&mut app, ThemeCtlOp::Status, now).expect("status");
    for want in [
        "id=\"Work\"",
        "configured=\"dark:Work,light:Nrod\"",
        "side=dark",
        "source=file path=",
        "reported=dark",
        "os_appearance=dark",
        "preview=-",
        "bg=#102030",
    ] {
        assert!(status.contains(want), "{want} in {status}");
    }
    assert!(
        status.contains("miss=\"theme: \\\"Nrod\\\" is unavailable"),
        "the light side's miss is named: {status}"
    );
    let palette = status
        .split_whitespace()
        .find_map(|field| field.strip_prefix("palette="))
        .expect("palette field");
    let entries: Vec<&str> = palette.split(',').collect();
    assert_eq!(entries.len(), 16, "{palette}");
    assert_eq!(
        entries[1],
        format!(
            "#{:02x}{:02x}{:02x}",
            work.ansi[1].r, work.ansi[1].g, work.ansi[1].b
        )
    );

    app.sync_app_theme_to_appearance(Appearance::Light);
    let status = run(&mut app, ThemeCtlOp::Status, now).expect("status");
    for want in ["id=\"Default\"", "side=light", "source=default"] {
        assert!(status.contains(want), "{want} in {status}");
    }

    app.config = aterm_toml::from_str("theme = \"Nord\"\n").expect("valid toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    let status = run(&mut app, ThemeCtlOp::Status, now).expect("status");
    assert!(status.contains("miss=- "), "{status}");
    assert!(status.contains("source=builtin"), "{status}");
}

#[test]
fn control_theme_list_marks_the_drawn_theme_and_quotes_labels() {
    let mut app = app_with("theme = \"GitHub Light\"\n");
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Work\nInjected".to_string(),
        ColorScheme::default(),
    )]));
    let body = run(&mut app, ThemeCtlOp::List, Instant::now()).expect("list");
    let rows: Vec<&str> = body.lines().collect();
    assert!(
        rows.iter().any(
            |row| row.starts_with("theme id=\"GitHub Light\" label=\"GitHub Light\"")
                && row.contains(" drawn=1 ")
        ),
        "{body}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains(" drawn=1 ")).count(),
        1,
        "{body}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("id=\"Work\\nInjected\"")),
        "a control character is escaped, never a row break: {body}"
    );
    assert!(
        rows.last().is_some_and(|row| row.starts_with("folder ")),
        "{body}"
    );
    assert!(
        rows.iter()
            .all(|row| row.starts_with("theme ") || row.starts_with("folder "))
    );
}

#[test]
fn control_theme_commit_without_a_preview_is_refused() {
    let mut app = app_with("theme = \"Dracula\"\n");
    let error = run(&mut app, ThemeCtlOp::Commit, Instant::now()).expect_err("refused");
    assert_eq!(error, "no theme preview to commit");
    assert!(app.native_config_pending.is_empty());
}

#[test]
fn control_theme_form_grammar() {
    let op = |op| Ok(ThemeForm::Op(op));
    assert_eq!(parse_theme_form(""), op(ThemeCtlOp::Status));
    assert_eq!(parse_theme_form(" status "), op(ThemeCtlOp::Status));
    assert_eq!(parse_theme_form("list"), op(ThemeCtlOp::List));
    assert_eq!(parse_theme_form("revert"), op(ThemeCtlOp::Revert));
    assert_eq!(parse_theme_form("commit"), op(ThemeCtlOp::Commit));
    assert_eq!(
        parse_theme_form("set dark:Work,light:GitHub Light"),
        Ok(ThemeForm::Set("dark:Work,light:GitHub Light".to_string()))
    );
    let preview = |id: &str, secs| {
        Ok(ThemeForm::Op(ThemeCtlOp::Preview {
            id: id.to_string(),
            ttl: Duration::from_secs(secs),
        }))
    };
    assert_eq!(
        parse_theme_form("preview GitHub Light"),
        preview("GitHub Light", THEME_PREVIEW_DEFAULT_TTL.as_secs())
    );
    assert_eq!(
        parse_theme_form("preview ttl=9 \"Solarized Light\""),
        preview("Solarized Light", 9)
    );
    assert_eq!(
        parse_theme_form("preview Nord ttl=3600"),
        preview("Nord", 3600)
    );
    assert_eq!(
        parse_theme_form("palette"),
        op(ThemeCtlOp::Palette { session: false })
    );
    assert_eq!(
        parse_theme_form("palette session"),
        op(ThemeCtlOp::Palette { session: true })
    );
    assert_eq!(parse_theme_form("session"), op(ThemeCtlOp::SessionStatus));
    assert_eq!(
        parse_theme_form("session status"),
        op(ThemeCtlOp::SessionStatus)
    );
    assert_eq!(
        parse_theme_form("session clear"),
        op(ThemeCtlOp::SessionClear)
    );
    assert_eq!(
        parse_theme_form("session set \"Harbour Night\""),
        op(ThemeCtlOp::SessionSet {
            selector: "Harbour Night".to_string()
        })
    );
    assert_eq!(
        parse_theme_form("session set dark:Aterm Night,light:Aterm Day"),
        op(ThemeCtlOp::SessionSet {
            selector: "dark:Aterm Night,light:Aterm Day".to_string()
        })
    );
    for junk in [
        "session set",
        "session set \"\"",
        "session bogus",
        "session clear now",
        "session status x",
        "palette x",
        "bogus",
        "status now",
        "list all",
        "set",
        "preview",
        "preview ttl=5",
        "preview Nord ttl=0",
        "preview Nord ttl=3601",
        "preview Nord ttl=five",
        "preview Nord ttl=5 ttl=6",
        "preview \"Nord",
    ] {
        let error = parse_theme_form(junk).expect_err(junk);
        assert!(error.starts_with("usage: theme"), "{junk}: {error}");
    }
}

fn set_settings_trial(app: &mut App, trial: crate::native_settings::showcase_page::ShowcaseTrial) {
    let (_, _, view, _) = app.native_settings_view_target().expect("a Settings view");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        unreachable!()
    };
    state.navigate(crate::native_settings::SettingsRoute::Themes);
    state.showcase.trial = trial;
}

fn trying(id: &str) -> crate::native_settings::showcase_page::ShowcaseTrial {
    crate::native_settings::showcase_page::ShowcaseTrial::Trying { id: id.to_string() }
}

#[test]
fn settings_trial_draws_without_writing_and_every_ending_reverts() {
    let mut app = app_with("theme = \"Hearth\"\n");
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let now = Instant::now();
    app.sync_settings_theme_trial(now);
    let before = visible(&app);
    let durable_before = durable(&app);
    for ending in ["revert", "leave", "close"] {
        if ending == "close" {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
        }
        set_settings_trial(&mut app, trying("Nord"));
        app.sync_settings_theme_trial(now);
        assert_eq!(
            app.installed_colors.id, "Nord",
            "{ending}: the trial is drawn"
        );
        assert_eq!(
            app.theme_preview.as_ref().map(|preview| preview.hold),
            Some(super::PreviewHold::Settings { keeping: false })
        );
        assert_ne!(visible(&app), before);
        match ending {
            "revert" => set_settings_trial(
                &mut app,
                crate::native_settings::showcase_page::ShowcaseTrial::Idle,
            ),
            "leave" => {
                let (_, _, view, _) = app.native_settings_view_target().unwrap();
                let Some(crate::native_app::AppViewState::Settings(state)) =
                    app.native_runtime.view_state_mut(view)
                else {
                    unreachable!()
                };
                state.navigate(crate::native_settings::SettingsRoute::Home);
            }
            _ => {
                assert!(app.close_settings_tabs());
            }
        }
        app.sync_settings_theme_trial(now);
        assert!(app.theme_preview.is_none(), "{ending}: reverted");
        assert_eq!(visible(&app), before, "{ending}: exactly what was drawn");
        assert_eq!(durable(&app), durable_before, "{ending}: nothing written");
    }
}

#[test]
fn settings_trial_never_takes_over_a_ctl_preview_and_refuses_a_missing_theme() {
    let mut app = app_with("");
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let now = Instant::now();
    preview(&mut app, "Dracula", THEME_PREVIEW_DEFAULT_TTL, now).expect("ctl preview");
    set_settings_trial(&mut app, trying("Nord"));
    app.sync_settings_theme_trial(now);
    assert_eq!(app.installed_colors.id, "Dracula", "the ctl preview stands");
    {
        let (_, _, _, state) = app.native_settings_view_target().unwrap();
        assert_eq!(
            state.showcase.trial,
            crate::native_settings::showcase_page::ShowcaseTrial::Idle,
            "the view no longer says it is trying Nord"
        );
        assert!(
            state
                .feedback
                .as_deref()
                .is_some_and(|line| line.contains("Nord") && line.contains("Dracula")),
            "{:?}",
            state.feedback
        );
    }
    run(&mut app, ThemeCtlOp::Revert, now).expect("revert");
    app.sync_settings_theme_trial(now);
    set_settings_trial(&mut app, trying("Nord"));
    app.sync_settings_theme_trial(now);
    assert_eq!(app.installed_colors.id, "Nord", "negative control: drawn");

    set_settings_trial(&mut app, trying("No Such Theme"));
    app.sync_settings_theme_trial(now);
    let (_, _, _, state) = app.native_settings_view_target().unwrap();
    assert_eq!(
        state.showcase.trial,
        crate::native_settings::showcase_page::ShowcaseTrial::Idle,
        "the view is told"
    );
    assert!(
        state
            .feedback
            .as_deref()
            .is_some_and(|line| line.contains("No Such Theme"))
    );
}

fn field<'a>(status: &'a str, key: &str) -> &'a str {
    status
        .split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{key} in {status}"))
}

#[test]
fn theme_status_line_reports_chrome() {
    let now = Instant::now();
    let mut dracula = app_with("theme = \"Dracula\"\n");
    let status = run(&mut dracula, ThemeCtlOp::Status, now).expect("status");
    assert_eq!(field(&status, "accent_source"), "blue");
    let blue = builtin("Dracula").ansi[4];
    assert_eq!(
        field(&status, "accent"),
        format!("#{:02x}{:02x}{:02x}", blue.r, blue.g, blue.b)
    );
    for key in [
        "accent_from",
        "border_from",
        "search_match_from",
        "active_tab_from",
    ] {
        assert_eq!(field(&status, key), "default", "{key}");
    }
    assert_eq!(field(&status, "active_tab"), "-");

    let mut default = app_with("");
    let status = run(&mut default, ThemeCtlOp::Status, now).expect("status");
    assert_eq!(field(&status, "accent_source"), "cursor");
    assert_eq!(field(&status, "accent"), "#50fa7b");

    let mut authored = app_with(
        "theme = \"Dracula\"\naccent_color = \"#ff00aa\"\nactive_tab_color = \"#123456\"\n",
    );
    let status = run(&mut authored, ThemeCtlOp::Status, now).expect("status");
    assert_eq!(field(&status, "accent_source"), "authored");
    assert_eq!(field(&status, "accent"), "#ff00aa");
    assert_eq!(field(&status, "accent_from"), "config");
    assert_eq!(field(&status, "active_tab"), "#123456");
    assert_eq!(field(&status, "active_tab_from"), "config");
    assert_eq!(field(&status, "border_from"), "default");
}

fn quoted_field(status: &str, key: &str) -> String {
    let start = status
        .find(&format!(" {key}=\""))
        .unwrap_or_else(|| panic!("{key} in {status}"))
        + key.len()
        + 3;
    let mut out = String::new();
    let mut chars = status[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next().expect("an escaped char")),
            '"' => return out,
            c => out.push(c),
        }
    }
    panic!("{key} is unterminated in {status}");
}

#[test]
fn theme_status_line_reports_typography() {
    let now = Instant::now();
    let parsed = aterm_types::scheme::parse_theme_str(
        "fg = #ffffff\nbg = #000000\nfont_family = Odd \"Quote\" Mono\nline_height = 1.2\n",
    )
    .expect("a typography theme");
    let mut app = App::headless_for_test();
    app.reload_theme_catalog(crate::app_config::ThemeCatalog::from_parsed([(
        "Studio".to_string(),
        parsed,
    )]));
    app.config = aterm_toml::from_str("theme = \"Studio\"\nline_height = 1.0\n").expect("toml");
    app.typography = app.config.resolved_typography_with_flag(
        aterm_types::Appearance::Dark,
        &app.config_assets.themes,
        None,
    );
    let status = run(&mut app, ThemeCtlOp::Status, now).expect("status");
    assert!(
        quoted_field(&status, "typography").contains("Odd \"Quote\" Mono"),
        "{status}"
    );
    assert_eq!(quoted_field(&status, "typography_keys"), "font_family");
    assert_eq!(
        quoted_field(&status, "typography_shadowed"),
        "line_height:config"
    );
    assert_eq!(field(&status, "font_size_scale"), "1.0");
    assert_eq!(field(&status, "palette").split(',').count(), 16);

    let mut plain = app_with("theme = \"Dracula\"\n");
    let status = run(&mut plain, ThemeCtlOp::Status, now).expect("status");
    for key in ["typography", "typography_keys", "typography_shadowed"] {
        assert_eq!(field(&status, key), "-", "{key}");
    }
}

fn pack_only_id() -> &'static str {
    aterm_theme_pack::themes()
        .iter()
        .find(|theme| aterm_types::scheme::builtin_theme(theme.id).is_none())
        .expect("the pack holds a theme that is not a built-in")
        .id
}

fn scratch_themes(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-gui-pack-preview-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn pack_ids_resolve_for_preview_only() {
    use crate::resolved_colors::ThemeSource;
    let id = pack_only_id();
    let themes = ThemeCatalog::default();
    let config = crate::app_config::Config::default();
    let preview = config.resolved_colors_previewing(Appearance::Dark, &themes, Some(id));
    assert_eq!(preview.source, ThemeSource::Pack(id.to_string()));
    let (_, package) = crate::app_config::pack_package(id).expect("the pack parses");
    assert_eq!(preview.scheme.background, package.colors.background);
    let configured: crate::app_config::Config =
        aterm_toml::from_str(&format!("theme = {id:?}\n")).expect("valid toml");
    assert!(matches!(
        configured
            .resolved_colors_previewing(Appearance::Dark, &themes, None)
            .source,
        ThemeSource::DefaultFallback { .. }
    ));
    assert!(matches!(
        config
            .resolved_colors(Appearance::Dark, &themes, Some(id))
            .source,
        ThemeSource::DefaultFallback { .. }
    ));
    let dir = aterm_tempfile::tempdir().expect("a themes folder");
    std::fs::write(
        dir.path().join(format!("{id}.conf")),
        "foreground = \"#zz\"\n=\n",
    )
    .expect("theme file");
    let broken = ThemeCatalog::discover_in(dir.path());
    assert!(broken.lists(id) && broken.resolve(id).is_err());
    assert!(matches!(
        config
            .resolved_colors_previewing(Appearance::Dark, &broken, Some(id))
            .source,
        ThemeSource::DefaultFallback { .. }
    ));
}

#[test]
fn theme_preview_unavailable_refuses_an_unknown_id() {
    let mut app = app_with("");
    assert!(
        app.theme_preview_unavailable("No Such Theme Anywhere")
            .is_some()
    );
    let id = pack_only_id();
    assert_eq!(app.theme_preview_unavailable(id), None);
    let now = Instant::now();
    let status = preview(&mut app, id, Duration::from_secs(60), now).expect("previewed");
    assert_eq!(field(&status, "source"), "pack", "{status}");
    assert_eq!(durable(&app).2, 0, "a pack preview writes nothing");
}

#[test]
fn committing_a_pack_preview_installs_then_commits() {
    let id = pack_only_id();
    let dir = scratch_themes("commit");
    let mut app = app_with("");
    let now = Instant::now();
    preview(&mut app, id, Duration::from_secs(60), now).expect("previewed");
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
    let (tx, rx) = std::sync::mpsc::channel();
    let mut queued = None;
    app.commit_theme_preview_into(Some(&dir), tx, |app, queued_id, request_id, _reply| {
        queued = Some((
            queued_id,
            app.config_assets.themes.resolve_package(id).is_ok(),
        ));
        Some(request_id)
    });
    assert_eq!(
        queued,
        Some((id.to_string(), true)),
        "installed before the write"
    );
    assert!(rx.try_recv().is_err(), "the write is pending, not refused");
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
