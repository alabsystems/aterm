// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session theme and `theme palette` tests.

#[cfg(unix)]
use std::sync::Arc;
use std::time::Instant;

use aterm_types::{Appearance, ColorScheme, Rgb};

use super::{SessionTheme, SessionThemeOrigin};
use crate::App;
use crate::app_colorscheme::ThemeChange;
use crate::app_config::ThemeCatalog;
use crate::control_theme::{PreviewHold, ThemeCtlOp};

fn insert(app: &mut App, session: crate::Session) {
    if let Some(tc) = &app.session_factory.terminal_config {
        crate::term_lock(&session.term).apply_config(tc);
    }
    app.pool_insert(session);
}

fn two_sessions(toml: &str) -> App {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str(toml).expect("valid toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    insert(&mut app, crate::stub_session(1));
    app
}

fn run_on(app: &mut App, op: ThemeCtlOp, session: Option<u64>) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.theme_control(op, session, Instant::now(), tx);
    rx.try_recv().expect("answered at once")
}

fn set(app: &mut App, sid: u64, selector: &str) -> Result<String, String> {
    run_on(
        app,
        ThemeCtlOp::SessionSet {
            selector: selector.to_string(),
        },
        Some(sid),
    )
}

fn bg(app: &App, sid: u64) -> Rgb {
    crate::term_lock(&app.pool.get(sid).expect("live session").term).default_background()
}

fn scheme(app: &App, sid: u64) -> Appearance {
    crate::term_lock(&app.pool.get(sid).expect("live session").term).color_scheme()
}

fn builtin(name: &str) -> ColorScheme {
    aterm_types::scheme::builtin(name).expect("built-in")
}

fn field(line: &str, key: &str) -> Option<String> {
    line.split_whitespace()
        .find_map(|kv| Some(kv.strip_prefix(key)?.strip_prefix('=')?.to_string()))
}

#[test]
fn theme_palette_rows_name_each_slots_origin() {
    let rows = |toml: &str| {
        let mut app = two_sessions(toml);
        let body = run_on(&mut app, ThemeCtlOp::Palette { session: false }, None)
            .expect("palette answers");
        body.lines().map(str::to_string).collect::<Vec<_>>()
    };
    let unset = rows("");
    assert_eq!(unset.len(), 256);
    assert!(unset.iter().all(|row| row.starts_with("slot index=")));
    assert!(
        unset[16..]
            .iter()
            .all(|row| field(row, "origin").as_deref() == Some("xterm")),
        "{unset:?}"
    );
    let night = rows("theme = \"Aterm Night\"\npalette_overrides = { 100 = \"#123456\" }\n");
    assert_eq!(field(&night[100], "origin").as_deref(), Some("config"));
    assert_eq!(field(&night[100], "rgb").as_deref(), Some("#123456"));
    assert_eq!(field(&night[101], "origin").as_deref(), Some("generated"));
    assert_eq!(field(&night[1], "origin").as_deref(), Some("theme"));
}

#[test]
fn session_theme_draws_only_that_session() {
    let mut app = two_sessions("theme = \"Aterm Night\"\n");
    let night = builtin("Aterm Night").background;
    assert_eq!((bg(&app, 0), bg(&app, 1)), (night, night));

    let line = set(&mut app, 1, "Aterm Day").expect("a built-in draws");
    assert_eq!(field(&line, "origin").as_deref(), Some("ctl"));
    assert_eq!(field(&line, "reported").as_deref(), Some("light"));
    assert_eq!(bg(&app, 1), builtin("Aterm Day").background);
    assert_eq!(scheme(&app, 1), Appearance::Light);
    assert_eq!(bg(&app, 0), night, "the other session keeps the instance's");
    assert_eq!(scheme(&app, 0), Appearance::Dark);
    let status = app.session_status_record(1).expect("status");
    assert_eq!(
        field(&status, "session_theme").as_deref(),
        Some("Aterm%20Day")
    );
    assert_eq!(field(&status, "theme_origin").as_deref(), Some("ctl"));
    assert_eq!(field(&status, "bg_dark").as_deref(), Some("0"));
    let other = app.session_status_record(0).expect("status");
    assert_eq!(field(&other, "theme_origin").as_deref(), Some("instance"));

    let fanouts = app.engine_color_fanouts;
    let cleared = run_on(&mut app, ThemeCtlOp::SessionClear, Some(1)).expect("clear");
    assert_eq!(field(&cleared, "origin").as_deref(), Some("instance"));
    assert_eq!(bg(&app, 1), night, "re-based on the instance theme");
    assert_eq!(scheme(&app, 1), Appearance::Dark);
    assert_eq!(
        app.engine_color_fanouts, fanouts,
        "the instance never moved"
    );
    assert!(run_on(&mut app, ThemeCtlOp::SessionClear, Some(1)).is_ok());
    assert_eq!(bg(&app, 1), night);
}

#[test]
fn a_session_theme_that_does_not_draw_is_refused() {
    let mut app = two_sessions("");
    let before = bg(&app, 1);
    let error = set(&mut app, 1, "No Such Theme").expect_err("refused");
    assert!(
        error.starts_with("theme session \"No Such Theme\""),
        "{error}"
    );
    assert_eq!(bg(&app, 1), before);
    assert!(app.pool.theme(1).is_none());
    assert!(
        run_on(&mut app, ThemeCtlOp::SessionStatus, Some(99)).is_err(),
        "an unknown session is refused"
    );
}

#[test]
fn a_pack_theme_the_themes_folder_lacks_is_refused_until_installed() {
    let id = aterm_theme_pack::themes()
        .iter()
        .find(|theme| aterm_types::scheme::builtin_theme(theme.id).is_none())
        .expect("the pack holds a theme that is not a built-in")
        .id;
    let mut app = two_sessions("");
    let before = bg(&app, 1);
    assert_eq!(
        app.theme_preview_unavailable(id),
        None,
        "a preview draws it"
    );
    for selector in [
        id.to_string(),
        format!("dark:{id},light:Aterm Day"),
        format!("dark:Aterm Night,light:{id}"),
    ] {
        let error =
            set(&mut app, 1, &selector).expect_err("a session cannot draw it on both sides");
        assert!(
            error.starts_with(&format!("theme session {selector:?}: unavailable (")),
            "{error}"
        );
        assert!(app.pool.theme(1).is_none(), "{selector}: nothing kept");
        assert_eq!(bg(&app, 1), before, "{selector}: nothing drawn");
        let error = app
            .spawn_theme(Some(selector.clone()))
            .expect_err("spawn refuses it too");
        assert!(
            error.starts_with(&format!("theme {selector:?}: unavailable (")),
            "{error}"
        );
    }

    let (_, package) = crate::app_config::pack_package(id).expect("the pack parses");
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        id.to_string(),
        package.colors.clone(),
    )]));
    let line = set(&mut app, 1, id).expect("installed, it draws");
    assert_eq!(field(&line, "source").as_deref(), Some("file"), "{line}");
    assert_eq!(field(&line, "miss").as_deref(), Some("-"), "{line}");
    assert_eq!(bg(&app, 1), package.colors.background);
}

#[test]
fn preview_does_not_reach_pinned_sessions() {
    let mut app = two_sessions("");
    set(&mut app, 1, "Aterm Day").expect("pinned");
    let far = Instant::now() + std::time::Duration::from_secs(60);
    app.begin_theme_preview("Dracula", PreviewHold::Until(far))
        .expect("Dracula previews");
    assert_eq!(bg(&app, 0), builtin("Dracula").background);
    assert_eq!(bg(&app, 1), builtin("Aterm Day").background);
    let status = app.theme_status_line(Instant::now());
    assert_eq!(field(&status, "pinned").as_deref(), Some("1"), "{status}");
    run_on(&mut app, ThemeCtlOp::SessionClear, Some(1)).expect("clear");
    assert_eq!(bg(&app, 1), builtin("Dracula").background);
    let status = app.theme_status_line(Instant::now());
    assert_eq!(field(&status, "pinned").as_deref(), Some("0"), "{status}");
}

#[test]
fn unchanged_reload_costs_no_session_apply() {
    let mut app = two_sessions("theme = \"Nord\"\n");
    set(&mut app, 1, "Hearth").expect("pinned");
    let after_pin = app.session_color_installs;
    app.install_resolved_colors(ThemeChange::ConfigReload);
    app.install_resolved_colors(ThemeChange::CatalogReload);
    assert_eq!(app.session_color_installs, after_pin);
    set(&mut app, 1, "Linen").expect("re-pinned");
    assert_eq!(app.session_color_installs, after_pin + 1);
    let appearance = app.appearance_status_line();
    assert_eq!(
        field(&appearance, "session_installs"),
        Some((after_pin + 1).to_string()),
        "{appearance}"
    );
}

#[test]
fn os_flip_moves_a_split_session_theme() {
    let mut app = two_sessions("theme = \"Nord\"\n");
    set(&mut app, 1, "dark:Aterm Night,light:Aterm Day").expect("pinned");
    assert_eq!(bg(&app, 1), builtin("Aterm Night").background);
    app.sync_app_theme_to_appearance(Appearance::Light);
    assert_eq!(bg(&app, 1), builtin("Aterm Day").background);
    assert_eq!(
        bg(&app, 0),
        builtin("Nord").background,
        "the instance's single theme does not follow the OS"
    );
}

/// Read whatever the program side of `fd` holds, without blocking.
#[cfg(unix)]
fn drain(fd: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        // SAFETY: `fd` is an open, non-blocking pipe end this test owns.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
    }
    out
}

/// A pipe whose write end a session's sink owns; the read end is the program's side.
#[cfg(unix)]
fn pipe_sink() -> (Arc<aterm_session::sink::SinkWriter>, i32) {
    let mut fds = [0; 2];
    // SAFETY: `fds` is a two-int array, as `pipe` requires.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: The read end was just opened.
    assert_eq!(
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    (
        Arc::new(aterm_session::sink::SinkWriter::new(fds[1])),
        fds[0],
    )
}

/// Spec `session-theme-install`: the mode-2031 push is per session.
#[cfg(unix)]
#[test]
fn mode_2031_push_is_per_session() {
    let (sink0, read0) = pipe_sink();
    let (sink1, read1) = pipe_sink();
    let mut app = App::headless_for_test_with_sink(sink0);
    app.config = aterm_toml::from_str("theme = \"Aterm Night\"\n").expect("toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    insert(&mut app, crate::stub_session_with_sink(1, sink1));
    for sid in [0, 1] {
        crate::term_lock(&app.pool.get(sid).expect("session").term).process(b"\x1b[?2031h");
    }
    let (_, _) = (drain(read0), drain(read1));
    set(&mut app, 1, "Aterm Day").expect("pinned");
    assert_eq!(drain(read1), b"\x1b[?997;2n", "the pinned session is told");
    assert!(drain(read0).is_empty(), "the other session hears nothing");
    set(&mut app, 1, "Aterm Day").expect("again");
    assert!(drain(read1).is_empty());
    drop(app);
    // SAFETY: The read ends are this test's own.
    unsafe {
        libc::close(read0);
        libc::close(read1);
    }
}

#[test]
fn missing_session_theme_falls_back_to_instance() {
    let mut app = two_sessions("theme = \"Nord\"\n");
    let mine = ColorScheme {
        name: "Mine".to_string(),
        background: Rgb::new(0x10, 0x20, 0x30),
        ..ColorScheme::default()
    };
    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        mine.clone(),
    )]));
    set(&mut app, 1, "Mine").expect("the file draws");
    assert_eq!(bg(&app, 1), mine.background);

    app.reload_theme_catalog(ThemeCatalog::from_schemes(
        Vec::<(String, ColorScheme)>::new(),
    ));
    assert_eq!(bg(&app, 1), builtin("Nord").background, "the instance's");
    let line = run_on(&mut app, ThemeCtlOp::SessionStatus, Some(1)).expect("status");
    assert_eq!(field(&line, "theme").as_deref(), Some("\"Mine\""), "{line}");
    assert_eq!(
        field(&line, "origin").as_deref(),
        Some("instance"),
        "{line}"
    );
    assert_ne!(field(&line, "miss").as_deref(), Some("-"), "{line}");
    assert_eq!(
        app.pool.theme(1).map(|t| t.selector.as_str()),
        Some("Mine"),
        "the selector is kept"
    );

    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        "Mine".to_string(),
        mine.clone(),
    )]));
    assert_eq!(bg(&app, 1), mine.background, "it comes back with the file");
}

#[test]
fn program_colours_survive_a_session_theme_change() {
    let mut app = two_sessions("theme = \"Nord\"\n");
    set(&mut app, 1, "Hearth").expect("pinned");
    let program = Rgb::new(0x12, 0x34, 0x56);
    {
        let mut term = crate::term_lock(&app.pool.get(1).expect("session").term);
        term.process(b"\x1b]11;#123456\x07");
        let _ = term.take_response();
    }
    assert_eq!(bg(&app, 1), program);
    set(&mut app, 1, "Aterm Day").expect("re-pinned");
    assert_eq!(bg(&app, 1), program, "the program's background holds");
    {
        let mut term = crate::term_lock(&app.pool.get(1).expect("session").term);
        term.process(b"\x1b]111\x07");
        let _ = term.take_response();
    }
    assert_eq!(bg(&app, 1), builtin("Aterm Day").background);
    assert_ne!(bg(&app, 1), builtin("Nord").background);
}

#[test]
fn a_new_shell_is_told_the_side_of_the_theme_it_will_draw() {
    let mut app = App::headless_for_test();
    app.config = aterm_toml::from_str("theme = \"Nord\"\n").expect("toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    assert_eq!(
        app.installed_colors.reported,
        Appearance::Dark,
        "a dark instance"
    );
    let side = |theme: Option<&str>| app.spawn_shell_appearance(theme);
    assert_eq!(side(Some("Default Light")), Some(Appearance::Light));
    assert_eq!(side(Some("Nord")), Some(Appearance::Dark));
    assert_eq!(side(Some("Nope")), None, "a miss draws the instance's");
    assert_eq!(side(None), None);
    #[cfg(windows)]
    {
        assert_eq!(
            app.session_factory.shell_colorfgbg().1,
            "15;0",
            "the instance's"
        );
        let light = app.shell_factory(Some("Default Light"));
        assert_eq!(light.shell_colorfgbg().1, "0;15", "the light tab's own");
        assert_ne!(
            light.spawn_fingerprint(),
            app.session_factory.spawn_fingerprint(),
            "a spare started on the instance's side is not this shell"
        );
    }
}

#[test]
fn spawn_with_bad_theme_spawns_nothing() {
    let mut app = App::headless_for_test();
    let before = app.pool.ids().len();
    let (tx, rx) = std::sync::mpsc::channel();
    app.spawn_session_aimed(
        crate::control::SpawnAim::Front,
        None,
        None,
        false,
        Some("NoSuchTheme".to_string()),
        None,
        tx,
    );
    let error = rx
        .try_recv()
        .expect("answered at once")
        .expect_err("refused");
    assert!(error.starts_with("theme \"NoSuchTheme\""), "{error}");
    assert_eq!(app.pool.ids().len(), before);
    assert!(app.tab_spawns.is_empty(), "nothing booked");
    assert_eq!(
        app.spawn_theme(Some("Hearth".to_string())),
        Ok(Some(SessionTheme {
            selector: "Hearth".to_string(),
            origin: SessionThemeOrigin::Spawn,
        }))
    );
}
