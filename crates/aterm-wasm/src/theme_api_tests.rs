// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

const MINE: &str = "name = Mine\nbackground = #101820\nforeground = #e0e0e0\n";

fn conf(name: &str, bg: &str) -> String {
    format!("name = {name}\nbackground = {bg}\nforeground = #e0e0e0\n")
}

#[test]
fn resolve_is_atomic_and_loud() {
    let state = WebThemeState::new(Appearance::Dark);
    let nord = state.resolve("Nord", Appearance::Dark).expect("built-in");
    assert_eq!(
        (nord.id.as_str(), nord.source),
        ("Nord", WebThemeSource::Builtin)
    );
    let paired = state
        .resolve("dark:Gruvbox Dark", Appearance::Light)
        .expect("an omitted side draws the pair");
    assert_eq!(paired.id, "Gruvbox Light");
    assert_eq!(paired.side, Appearance::Light);
    assert_eq!(
        state
            .resolve("nord", Appearance::Light)
            .expect("any case")
            .id,
        "Nord"
    );
    for bad in [
        "",
        "  ",
        "dark:",
        "dark:Nord,dusk:X",
        "Nope",
        "dark:Nord,light:Nope",
    ] {
        assert!(state.resolve(bad, Appearance::Dark).is_err(), "{bad:?}");
    }
    let (native, _) = ThemeSelector::parse("dark:Nord,light:Nope");
    assert_eq!(native.name_for(Appearance::Dark), Some("Nord"));
}

#[test]
fn register_bounds_and_shadowing() {
    let mut state = WebThemeState::new(Appearance::Dark);
    let mine = state.register(MINE).expect("a minimal theme");
    assert_eq!(mine.id, "Mine");
    assert_eq!(mine.meta.appearance, Appearance::Dark);
    assert_eq!(mine.meta.appearance_source, AppearanceSource::Measured);
    state
        .register(&conf("mine", "#202020"))
        .expect("re-register");
    assert_eq!(state.registered.len(), 1, "replaced, not added");
    assert_eq!(state.registered[0].id, "mine");

    let err = state
        .register(&conf("nord", "#000000"))
        .expect_err("built-in");
    assert!(err.contains("built-in"), "{err}");
    let err = state
        .register(&conf("dark:x", "#000000"))
        .expect_err("not an id");
    assert!(!err.contains("built-in"), "{err}");
    let err = state
        .register("background = #000000\nforeground = #ffffff\n")
        .expect_err("no name");
    assert!(err.contains("name"), "{err}");
    state
        .register(&conf("Mine Too", "#000000"))
        .expect("a free name");
    let huge = "x".repeat(MAX_USER_THEME_FILE_BYTES + 1);
    let err = state.register(&huge).expect_err("too large");
    assert!(err.contains("256 KiB"), "{err}");

    let light = state.register(&conf("Paper", "#F5F5F5")).expect("light");
    assert_eq!(light.meta.appearance, Appearance::Light);
    assert_eq!(light.meta.appearance_source, AppearanceSource::Measured);

    let mut full = WebThemeState::new(Appearance::Dark);
    for i in 0..MAX_REGISTERED_THEMES {
        full.register(&conf(&format!("T{i}"), "#000000"))
            .unwrap_or_else(|e| panic!("theme {i}: {e}"));
    }
    assert_eq!(
        full.registered.len(),
        MAX_REGISTERED_THEMES,
        "the cap is reachable"
    );
    assert!(
        full.register(&conf("One More", "#000000")).is_err(),
        "past the cap"
    );
    full.register(&conf("t0", "#111111"))
        .expect("replacing at the cap is fine");
}

#[test]
fn catalog_json_lists_builtins_then_registered() {
    let mut state = WebThemeState::new(Appearance::Dark);
    state.register(MINE).expect("registered");
    let json = state.catalog_json();
    let ids: Vec<&str> = json
        .split("{\"id\":\"")
        .skip(1)
        .map(|row| row.split('"').next().unwrap_or_default())
        .collect();
    let builtins = builtin_theme_list();
    assert_eq!(ids.len(), builtins.len() + 1);
    assert_eq!(ids.first(), Some(&"Default"));
    for (id, b) in ids.iter().zip(&builtins) {
        assert_eq!(*id, b.name);
    }
    assert_eq!(ids.last(), Some(&"Mine"));
    assert!(json.starts_with("{\"unset\":\"Default\","));
    assert!(json.contains(
        "\"id\":\"Mine\",\"display_name\":\"Mine\",\"appearance\":\"dark\",\"appearance_source\":\"measured\""
    ));
    assert!(json.contains(
        "\"id\":\"Default Light\",\"display_name\":\"Default Light\",\"appearance\":\"light\""
    ));
}

#[test]
fn unregister_refuses_the_drawn_theme() {
    let mut state = WebThemeState::new(Appearance::Dark);
    state.register(MINE).expect("registered");
    state.selector = Some("dark:Mine,light:Hearth".into());
    assert!(state.unregister("mine").is_err(), "drawn by the selector");
    state.selector = Some("Nord".into());
    state.unregister("mine").expect("no longer drawn");
    assert!(state.unregister("Mine").is_err(), "already gone");
    assert!(state.resolve("Mine", Appearance::Dark).is_err());
}

const FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

fn terminal() -> WebTerminal {
    WebTerminal::new(
        4,
        20,
        FACE,
        14.0,
        0x00d0_d0d0,
        0x0010_1010,
        0x00ff_ffff,
        0x0040_4040,
    )
    .expect("the bundled face parses")
}

fn assert_draws(t: &WebTerminal, scheme: &ColorScheme, at: &str) {
    let parts = scheme.to_theme_parts();
    assert_eq!(
        t.render_face().theme(),
        aterm_render::Theme::from(parts),
        "{at}"
    );
    let base = t.term.color_baseline();
    assert_eq!(base.foreground, scheme.foreground, "{at}");
    assert_eq!(t.term.default_background(), scheme.background, "{at}");
    assert_eq!(t.term.default_foreground(), scheme.foreground, "{at}");
    assert_eq!(base.cursor, Some(unpack(parts.cursor)), "{at}");
    assert_eq!(
        base.selection_background,
        Some(unpack(parts.selection)),
        "{at}"
    );
    for i in 0..16u8 {
        assert_eq!(
            t.term.palette_color(i),
            scheme.ansi[usize::from(i)],
            "{at} slot {i}"
        );
    }
    let palette = scheme.to_color_palette();
    for i in [16u8, 100, 231, 255] {
        assert_eq!(t.term.palette_color(i), palette.get(i), "{at} slot {i}");
    }
    assert_eq!(t.term.color_scheme(), scheme.appearance, "{at}");
}

#[test]
fn set_theme_by_name_matches_the_shared_projection_for_every_builtin() {
    let mut t = terminal();
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
        for dark in [true, false] {
            t.set_color_scheme(dark);
            t.set_theme_by_name(name).expect("a built-in installs");
            assert_draws(&t, &scheme, &format!("{name} dark={dark}"));
        }
    }
    let derived = aterm_types::scheme::builtin("Aterm Night").expect("a built-in");
    assert!(derived.palette_generate);
    assert_ne!(
        derived.to_color_palette().get(16),
        aterm_core::terminal::ColorPalette::default_color(16)
    );
    let xterm = aterm_types::scheme::builtin("Default").expect("a built-in");
    assert!(!xterm.palette_generate);
    assert_eq!(
        xterm.to_color_palette().get(16),
        aterm_core::terminal::ColorPalette::default_color(16)
    );
}

#[test]
fn split_follows_set_color_scheme() {
    let mut t = terminal();
    t.process(b"\x1b[?2031h");
    while t.term.take_response().is_some() {}
    t.set_color_scheme(true);
    t.set_theme_by_name("dark:Aterm Night,light:Aterm Day")
        .expect("signature split");
    assert_eq!(
        t.web_theme.installed.as_ref().map(|d| d.id.as_str()),
        Some("Aterm Night")
    );
    while t.term.take_response().is_some() {}
    t.set_color_scheme(false);
    let day = aterm_types::scheme::builtin("Aterm Day").expect("built-in");
    assert_draws(&t, &day, "Aterm Day on light");
    assert_eq!(
        t.term.take_response().as_deref(),
        Some(&b"\x1b[?997;2n"[..])
    );
    let state = t.theme_state();
    assert!(
        state.contains(
            "\"id\":\"Aterm Day\",\"side\":\"light\",\"reported\":\"light\",\"source\":\"builtin\""
        ),
        "{state}"
    );

    t.set_theme_by_name("Nord").expect("plain");
    while t.term.take_response().is_some() {}
    let nord = aterm_types::scheme::builtin("Nord").expect("built-in");
    for dark in [true, false] {
        t.set_color_scheme(dark);
        assert_draws(&t, &nord, "Nord");
        assert_eq!(
            t.term.take_response(),
            None,
            "no 997 push for a plain theme"
        );
    }

    t.set_theme(0x0011_2233, 0x00ee_eeee, 0x0000_0000, 0x0080_8080);
    let host = t.render_face().theme();
    t.set_color_scheme(true);
    assert_eq!(t.render_face().theme(), host);
    assert!(t.theme_state().contains("\"source\":\"host\""));
}

#[test]
fn named_install_keeps_program_colours() {
    let linen = aterm_types::scheme::builtin("Linen").expect("built-in");
    let program = Rgb {
        r: 0x10,
        g: 0x11,
        b: 0x12,
    };
    let mut t = terminal();
    t.process(b"\x1b]11;rgb:10/11/12\x07");
    t.set_theme_by_name("Linen").expect("installs");
    assert_eq!(t.term.default_background(), program, "OSC 11 survives");
    assert_eq!(t.term.default_foreground(), linen.foreground, "fg follows");
    t.process(b"\x1b]111\x07");
    assert_eq!(
        t.term.default_background(),
        linen.background,
        "OSC 111 → Linen"
    );

    let mut legacy = terminal();
    legacy.process(b"\x1b]11;rgb:10/11/12\x07");
    legacy.term.set_default_background(linen.background);
    assert_ne!(legacy.term.default_background(), program);
}

#[test]
fn set_theme_after_a_named_theme_reports_the_host_appearance() {
    let mut t = terminal();
    t.set_color_scheme(true);
    t.set_theme_by_name("Linen").expect("installs");
    let _ = t.term.take_response();
    t.process(b"\x1b[?996n");
    assert_eq!(
        t.term.take_response().as_deref(),
        Some(&b"\x1b[?997;2n"[..]),
        "NEG: the named light theme reports light"
    );
    t.set_theme(0x00d0_d0d0, 0x0010_1010, 0x00ff_ffff, 0x0040_4040);
    let _ = t.term.take_response();
    t.process(b"\x1b[?996n");
    assert_eq!(
        t.term.take_response().as_deref(),
        Some(&b"\x1b[?997;1n"[..]),
        "the host's dark appearance"
    );
}

#[test]
fn set_theme_keeps_program_colours() {
    let program = Rgb {
        r: 0x10,
        g: 0x11,
        b: 0x12,
    };
    let mut t = terminal();
    t.process(b"\x1b]11;rgb:10/11/12\x07");
    t.set_theme(0x00ee_eeee, 0x00fa_fafa, 0x0000_0000, 0x00c0_c0c0);
    assert_eq!(
        t.term.default_background(),
        program,
        "OSC 11 survives set_theme"
    );
    assert_eq!(t.term.default_foreground(), unpack(0x00ee_eeee));
    t.process(b"\x1b]111\x07");
    assert_eq!(
        t.term.default_background(),
        unpack(0x00fa_fafa),
        "OSC 111 → host bg"
    );
}

#[test]
fn web_translucency_raises_the_effective_floor() {
    let mut t = terminal();
    t.set_background_opacity(0.6);
    assert_eq!(t.effective_minimum_contrast(), 4.5);
    t.set_minimum_contrast(7.0);
    assert_eq!(
        t.effective_minimum_contrast(),
        7.0,
        "a stronger floor stays"
    );
    t.set_minimum_contrast(1.0);
    t.set_background_opacity(1.0);
    assert_eq!(t.effective_minimum_contrast(), 1.0, "opaque: no raise");
}

#[test]
fn register_then_select_follows_a_re_registration() {
    let mut t = terminal();
    let out = t.register_theme(MINE).expect("registers");
    assert!(
        out.starts_with(
            "{\"id\":\"Mine\",\"appearance\":\"dark\",\"appearance_source\":\"measured\""
        ),
        "{out}"
    );
    t.set_theme_by_name("mine").expect("selects any case");
    assert_eq!(t.render_face().theme().bg, 0x0010_1820);
    assert!(t.unregister_theme("Mine").is_err(), "drawn");
    t.register_theme(&conf("Mine", "#203040"))
        .expect("re-registers");
    assert_eq!(
        t.render_face().theme().bg,
        0x0020_3040,
        "the drawn theme re-installs"
    );
    assert!(t.set_theme_by_name("Nope").is_err());
    assert_eq!(
        t.render_face().theme().bg,
        0x0020_3040,
        "an error changes nothing"
    );
}

#[test]
fn theme_installs_reopen_the_frame_gate() {
    let mut t = terminal();
    t.register_theme(MINE).expect("registers");
    t.set_theme_by_name("Mine").expect("installs");
    t.settle_frame();
    assert!(!t.needs_frame(), "settled");
    t.set_theme_by_name("Nord").expect("installs");
    assert!(t.needs_frame(), "a named install repaints");
    t.settle_frame();
    t.set_theme_by_name("dark:Mine,light:Hearth")
        .expect("installs");
    t.settle_frame();
    t.register_theme(&conf("Mine", "#304050"))
        .expect("re-registers");
    assert!(t.needs_frame(), "re-registering the drawn theme repaints");
    t.settle_frame();
    t.register_theme(&conf("Other", "#000000"))
        .expect("registers");
    assert!(!t.needs_frame());
}

#[test]
fn cursor_color_follows_osc12_and_resets_on_112() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    let theme_cursor = t.theme_cursor;
    assert_eq!(
        t.cursor_color(),
        Some(theme_cursor),
        "the host-configured cursor baseline is live at start"
    );
    t.process(b"\x1b]12;#ff8800\x07");
    assert_eq!(
        t.cursor_color(),
        Some(0x00FF_8800),
        "OSC 12 must surface as a packed 0x00RRGGBB"
    );
    t.process(b"\x1b]112\x07");
    assert_eq!(
        t.cursor_color(),
        Some(theme_cursor),
        "OSC 112 restores the host-configured cursor baseline"
    );
}
