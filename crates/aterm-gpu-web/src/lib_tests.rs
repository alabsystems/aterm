// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use aterm_core::render::FrameRefill;

use super::*;

/// The bundled face, injected as a JS host would.
const BUNDLED_FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

/// A link's columns and text come from the cells it is drawn in, whatever
/// precedes it on the row: two ideographs, or two flag halves written out of order.
#[test]
fn a_link_after_wide_characters_is_read_from_its_cells() {
    let mut t = AtermGpuTerminal::new(4, 60, BUNDLED_FACE, 16.0, 0, 0, 0, 0)
        .expect("the bundled face parses");
    t.process("\u{65e5}\u{672c} https://example.com/a\r\n".as_bytes());
    t.process("\x1b[2;3H\u{1f1e7}\x1b[2;1H\u{1f1e6}\x1b[2;5Hhttps://example.com/b".as_bytes());
    for (row, col, url, start, end) in [
        (0, 5, "https://example.com/a", 5, 26),
        (0, 25, "https://example.com/a", 5, 26),
        (1, 4, "https://example.com/b", 4, 25),
        (1, 24, "https://example.com/b", 4, 25),
    ] {
        let hit = t
            .link_at(row, col)
            .unwrap_or_else(|| panic!("({row}, {col}) is over a URL"));
        assert_eq!(
            (hit.url(), hit.start_col(), hit.end_col(), hit.kind()),
            (url.to_string(), start, end, 1),
            "link at ({row}, {col})"
        );
    }
    assert!(t.link_at(1, 2).is_none(), "a flag half is no link");
}

/// `init` builds its GPU face from the host's current settings
/// (`gpu_face`), not the constructor's px: a `set_px` before `init`, or a
/// re-`init` after one, must draw at the size the cells were measured at.
#[test]
fn the_gpu_face_is_the_face_the_host_set() {
    let mut t = AtermGpuTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
        .expect("the bundled face parses");
    t.set_px(21.0).expect("admitted geometry");
    t.set_line_height(1.25);
    t.set_chrome(4, 2).expect("admitted geometry");
    t.set_line_height(f32::INFINITY);
    let face = t.gpu_face().expect("the bundled face");
    assert_eq!(face.px(), 21.0);
    assert_eq!(face.cell_size(), t.cpu.cell_size());
    assert_eq!((face.pad(), face.head()), (t.cpu.pad(), t.cpu.head()));

    t.process(b"selected text\r\n\x1b[1 q");
    t.selection_start(0, 0);
    t.selection_extend(0, 8);
    t.selection_finish();
    t.set_selection_fg(Some(0x00ff_8040));
    t.set_selection_inactive_bg(Some(0x0012_3456));
    for (inactive, hollow, blink) in [
        (false, false, false),
        (false, true, true),
        (true, false, true),
    ] {
        t.set_selection_inactive(inactive);
        t.set_cursor_hollow(hollow);
        t.set_cursor_blink_phase(blink);
        let mut face = t.gpu_face().expect("the bundled face");
        let input = t.term.cell_frame(t.rows, t.cols);
        let expected = t.cpu.render_input(&input);
        let actual = face.render_input(&input);
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(
            actual.pixels, expected.pixels,
            "init must retain inactive={inactive}, hollow={hollow}, blink={blink}"
        );
    }
}

#[test]
fn console_input_api_counts_commits_without_replaying_their_echoes() {
    use aterm_effects::kitty_pet::PetInputKind;

    let mut t = AtermGpuTerminal::new(8, 48, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
        .expect("the bundled face parses");
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 0);
    let generation = t.host_visual_gen;
    assert!(!t.note_console_input("unknown"));
    assert_eq!(t.host_visual_gen, generation);
    t.note_keystroke();
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 0);
    assert!(t.note_typed_char('x'));
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 1);
    t.process(b"x\r");
    t.process(b"program repaint\r");
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 1);

    assert!(t.note_console_input("text"));
    t.process("中文🙂".as_bytes());
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 2);
    assert!(!t.note_typed_char('\u{0301}'));
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 3);

    let paste = t.format_paste("one pasted sentence");
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 3);
    let generation = t.host_visual_gen;
    assert!(t.note_console_input("paste"));
    assert!(
        t.host_visual_gen > generation,
        "no-caret-travel input must wake one frame"
    );
    t.process(&paste);
    assert_eq!(t.effects.cursor_pet_console_input_seq(), 4);
    assert_eq!(
        t.effects.cursor_pet_console_input_kind(),
        Some(PetInputKind::Paste)
    );
    for (name, kind) in [
        ("navigate", PetInputKind::Navigate),
        ("delete", PetInputKind::Delete),
        ("submit", PetInputKind::Submit),
    ] {
        let before = t.effects.cursor_pet_console_input_seq();
        assert!(t.note_console_input(name));
        assert_eq!(t.effects.cursor_pet_console_input_seq(), before + 1);
        assert_eq!(t.effects.cursor_pet_console_input_kind(), Some(kind));
    }
}

/// The web export is the engine's paste formatter: sanitize-then-wrap makes
/// a planted `ESC[201~` (or 8-bit `0x9B 2 0 1 ~`) inert before the closing
/// guard, so it cannot end its own bracket.
#[test]
fn paste_export_wraps_only_sanitized_text_so_a_planted_terminator_stays_inert() {
    let mut t = AtermGpuTerminal::new(24, 80, BUNDLED_FACE, 14.0, 0, 0, 0, 0)
        .expect("the bundled face parses");

    assert!(!t.bracketed_paste_mode());
    assert_eq!(
        t.format_paste("a\x1b[31mb\u{009d}c\nd"),
        b"a[31mbc\rd",
        "unbracketed paste is sanitized too"
    );

    t.process(b"\x1b[?2004h");
    assert!(t.bracketed_paste_mode());

    let out = t.format_paste("safe\x1b[201~rm -rf ~");
    assert_eq!(out, b"\x1b[200~safe[201~rm -rf ~\x1b[201~");
    assert_eq!(
        out.windows(6).filter(|w| *w == b"\x1b[201~").count(),
        1,
        "exactly one closing guard on the wire"
    );
    assert!(out.ends_with(b"\x1b[201~"), "and it is the last thing sent");

    assert_eq!(
        t.format_paste("a\u{009b}201~b"),
        b"\x1b[200~a201~b\x1b[201~",
        "the 8-bit CSI spelling of the terminator is stripped as well"
    );

    let mut engine = Terminal::new(24, 80);
    engine.process(b"\x1b[?2004h");
    for probe in [
        "",
        "\t",
        "x\r\ny\nz",
        "safe\x1b[201~rm -rf ~",
        "a\u{009b}201~b",
        "a\u{0003}b\u{0004}c\u{001a}d\u{007f}e",
        "中文🙂",
    ] {
        assert_eq!(
            t.format_paste(probe),
            engine.format_paste(probe),
            "web export must equal Terminal::format_paste for {probe:?}"
        );
    }
}

/// The `effects_api` mutators' `note_host_visual_change` bumps must be live:
/// an effects config change that fails to reopen the gate would serve a
/// stale frame.
#[test]
fn effects_config_mutators_bump_the_host_visual_generation() {
    let mut t = crate::system_font_terminal(6, 40, 14.0);
    let start = t.host_visual_gen;
    t.advance_effects(16.0);
    assert_eq!(
        t.host_visual_gen, start,
        "advance_effects must not bump — it runs every rAF tick"
    );

    let mut last = start;
    for (label, mutate) in [
        (
            "set_effects_focused",
            (|t: &mut AtermGpuTerminal| t.set_effects_focused(true)) as fn(&mut AtermGpuTerminal),
        ),
        ("set_effects_visibility", |t| {
            t.set_effects_visibility("hidden")
        }),
        ("note_keystroke", |t| t.note_keystroke()),
        ("note_typed_char", |t| {
            let _ = t.note_typed_char('x');
        }),
        ("set_cursor_glow", |t| {
            t.set_cursor_glow(true, "lumen", None, None, 300, 8, 1.0, 0.0, false)
        }),
        ("set_cursor_trail", |t| {
            t.set_cursor_trail(true, 300, 8, None)
        }),
        ("set_matrix_rain_enabled", |t| {
            t.set_matrix_rain_enabled(true)
        }),
        ("note_matrix_rain_bell", |t| t.note_matrix_rain_bell()),
        ("set_sparkle_words_enabled", |t| {
            t.set_sparkle_words_enabled(true)
        }),
        ("set_sparkle_reduced_motion", |t| {
            t.set_sparkle_reduced_motion(true)
        }),
    ] {
        mutate(&mut t);
        assert!(
            t.host_visual_gen > last,
            "{label} must bump the host-visual generation (hook is a no-op?)"
        );
        last = t.host_visual_gen;
    }
}

/// A screenful of content, settled: the next `render_headless` gates.
fn settled(rows: u16, cols: u16) -> AtermGpuTerminal {
    let mut t = crate::system_font_terminal(rows, cols, 14.0);
    for r in 0..rows {
        t.process(format!("line {r} of selectable text\r\n").as_bytes());
    }
    t.render_headless().expect("renderable frame");
    t.render_headless().expect("renderable frame");
    assert!(t.last_render_skipped(), "setup must reach a settled frame");
    t
}

/// A tick refused before `init`, or one whose present fails, puts nothing on glass, so it
/// leaves the frame gate open; only a presented frame settles it.
#[test]
fn only_a_presented_frame_consumes_the_frame_gate() {
    let mut t = settled(8, 40);
    t.process(b"!");
    let mut presented = false;
    assert_eq!(
        t.present_frame(false, |_| {
            presented = true;
            Ok(())
        }),
        Err("render() before init()".to_string())
    );
    assert!(!presented, "a refusal before init presents nothing");
    assert!(t.needs_frame(), "and leaves the gate open");

    assert_eq!(
        t.present_frame(true, |_| Err("surface lost".to_string())),
        Err("surface lost".to_string())
    );
    assert!(t.needs_frame(), "a failed present leaves the gate open");

    assert_eq!(t.render_headless(), Ok(false), "the next tick draws");
    assert!(!t.needs_frame(), "a presented frame settles the gate");
    assert_eq!(t.render_headless(), Ok(true));
}

/// The frame gate, both sides: a settled tick skips the whole frame, and
/// every class of change the damage epoch cannot see reopens it.
#[test]
fn frame_gate_skips_settled_ticks_and_reopens_on_every_change_class() {
    let mut t = settled(8, 40);
    assert!(!t.needs_frame(), "needs_frame must agree with the gate");

    t.process(b"!");
    assert!(t.needs_frame(), "damage must reopen the gate");
    assert!(
        !t.render_headless().expect("renderable frame"),
        "an echo tick must draw"
    );
    assert!(
        t.render_headless().expect("renderable frame"),
        "and then re-settle"
    );

    t.set_cursor_blink_phase(false);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a blink flip must draw"
    );
    t.set_cursor_blink_phase(false);
    assert!(
        t.render_headless().expect("renderable frame"),
        "an idempotent blink re-assert must gate"
    );

    t.selection_start(2, 1);
    t.selection_extend(2, 8);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a selection change must draw"
    );
    t.selection_extend(2, 8);
    assert!(
        t.render_headless().expect("renderable frame"),
        "a cell-identical extend must be gated away"
    );
    t.selection_extend(2, 9);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a one-cell move must draw"
    );
    t.selection_clear();
    assert!(
        !t.render_headless().expect("renderable frame"),
        "clearing a selection must draw"
    );
    t.render_headless().expect("renderable frame");

    t.scroll_lines(3);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a viewport scroll must draw"
    );
    t.render_headless().expect("renderable frame");
    t.scroll_to_bottom();
    assert!(
        !t.render_headless().expect("renderable frame"),
        "and scrolling back must draw"
    );
    t.render_headless().expect("renderable frame");

    t.set_theme(0x00FF_0000, 0x0000_2200, 0x00FF_FFFF, 0x0033_3366);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a theme change must draw"
    );
    t.render_headless().expect("renderable frame");
    t.set_palette_color(1, 9, 9, 9);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "a palette change must draw"
    );
    t.render_headless().expect("renderable frame");

    t.set_matrix_rain_enabled(true);
    assert!(
        !t.render_headless().expect("renderable frame"),
        "an ignited effect must draw"
    );
    let mut saw_active = false;
    for i in 0u32..60 {
        // Real output now and then: rain holds still on a pane that is only
        // being read, where closing the gate is correct.
        if i.is_multiple_of(10) {
            t.process(b"x");
        }
        t.advance_effects(16.0);
        let active = t.effects.is_active();
        saw_active |= active;
        let gated = t.render_headless().expect("renderable frame");
        if active {
            assert!(!gated, "an active effects pipeline must never gate");
            assert!(t.needs_frame(), "and needs_frame must say so");
        }
    }
    assert!(
        saw_active,
        "the rain session never went active — this case proved nothing"
    );
    t.set_matrix_rain_enabled(false);
    let mut settled = false;
    for _ in 0..10 {
        if t.render_headless().expect("renderable frame") {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "an idle effects pipeline must settle back into the gate"
    );
}

#[test]
fn pending_budget_share_shrink_reopens_the_frame_gate_and_agrees_with_render() {
    let mut t = crate::system_font_terminal(6, 40, 14.0);
    for i in 0..3_000 {
        t.process(format!("tier line {i}\r\n").as_bytes());
    }
    while t.drain_scrollback_backlog(0) > 0 {}
    t.scroll_to_top();
    assert!(!t.render_headless().expect("initial frame"));
    assert!(!t.needs_frame(), "settled after rendering scrolled history");
    assert!(t.render_headless().expect("gated frame"));

    // Non-evicting pending share update (as from raising the global budget).
    t.budget_share.set_configured(128 * 1024 * 1024);
    assert!(
        !t.needs_frame(),
        "a non-evicting pending share leaves damage_epoch untouched"
    );
    assert!(
        t.render_headless().expect("still gated"),
        "render_headless agrees with needs_frame == false"
    );

    let before = t
        .term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
    assert!(before > 0);

    // Evicting pending share shrink (as from lowering the global budget).
    t.budget_share.set_configured(4_096);
    assert!(
        t.needs_frame(),
        "an evicting pending share enforces budget and reopens the frame gate"
    );
    let after = t
        .term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
    assert!(
        after < before,
        "needs_frame applied the shrunk budget and evicted store lines ({before} -> {after})"
    );
    assert_eq!(
        t.term
            .scrollback()
            .map(aterm_core::scrollback::ScrollbackStorage::memory_budget),
        Some(4_096)
    );
    assert!(
        !t.render_headless().expect("eviction repaint"),
        "render_headless agrees with needs_frame == true"
    );
    assert!(
        !t.needs_frame(),
        "re-settled after presenting the clamped view"
    );
}

/// Mutator audit: every `pub fn` that changes what a frame looks like must
/// bump `host_visual_gen`, or the gate serves a stale frame.
#[test]
fn every_visual_mutator_bumps_the_host_visual_generation() {
    let mut t = settled(8, 40);
    let mut last = t.host_visual_gen;
    for (label, mutate) in [
        (
            "set_fallback_font",
            (|t: &mut AtermGpuTerminal| {
                let _ = t.set_fallback_font(b"");
            }) as fn(&mut AtermGpuTerminal),
        ),
        ("add_fallback_font", |t| {
            let _ = t.add_fallback_font(b"");
        }),
        ("set_emoji_font", |t| {
            let _ = t.set_emoji_font(b"");
        }),
        ("set_bold_font", |t| {
            let _ = t.set_bold_font(b"");
        }),
        ("set_symbol_font", |t| {
            let _ = t.set_symbol_font(b"");
        }),
        ("set_primary_font", |t| {
            let _ = t.set_primary_font(b"");
        }),
        ("set_fallback_font_registered", |t| {
            let _ = t.set_fallback_font_registered(0);
        }),
        ("add_fallback_font_registered", |t| {
            let _ = t.add_fallback_font_registered(0);
        }),
        ("set_emoji_font_registered", |t| {
            let _ = t.set_emoji_font_registered(0);
        }),
        ("set_bold_font_registered", |t| {
            let _ = t.set_bold_font_registered(0);
        }),
        ("set_symbol_font_registered", |t| {
            let _ = t.set_symbol_font_registered(0);
        }),
        ("set_line_height", |t| t.set_line_height(1.2)),
        ("set_ligatures", |t| t.set_ligatures(true)),
        ("set_font_features", |t| t.set_font_features("+liga")),
        ("set_palette_color", |t| t.set_palette_color(3, 1, 2, 3)),
        ("set_default_foreground", |t| {
            t.set_default_foreground(1, 2, 3)
        }),
        ("set_default_background", |t| {
            t.set_default_background(4, 5, 6)
        }),
        ("set_scrollback_limit", |t| t.set_scrollback_limit(500)),
        ("set_theme", |t| {
            t.set_theme(0x0011_2233, 0x0044_5566, 0x0077_8899, 0x000A_0B0C)
        }),
        ("set_selection_fg", |t| {
            t.set_selection_fg(Some(0x0012_3456))
        }),
        ("set_minimum_contrast", |t| t.set_minimum_contrast(4.5)),
        ("set_background_opacity", |t| t.set_background_opacity(0.8)),
        ("set_chrome", |t| {
            t.set_chrome(4, 2).expect("admitted geometry")
        }),
        ("set_cursor_opacity", |t| t.set_cursor_opacity(0.5)),
        ("set_selection_inactive", |t| t.set_selection_inactive(true)),
        ("set_selection_inactive_bg", |t| {
            t.set_selection_inactive_bg(Some(0x0020_2020))
        }),
        ("set_px", |t| t.set_px(15.0).expect("admitted geometry")),
        ("set_spill_include_veils", |t| {
            t.set_spill_include_veils(true)
        }),
        ("set_default_cursor_style", |t| {
            t.set_default_cursor_style(3)
        }),
        ("set_color_scheme", |t| t.set_color_scheme(true)),
        ("set_cursor_blink_phase", |t| {
            t.set_cursor_blink_phase(false)
        }),
        ("set_cursor_hollow", |t| t.set_cursor_hollow(true)),
        ("selection_start", |t| t.selection_start(2, 1)),
        ("selection_extend", |t| t.selection_extend(2, 7)),
        ("selection_finish", |t| t.selection_finish()),
        ("selection_word", |t| {
            let _ = t.selection_word(3, 2);
        }),
        ("selection_line", |t| {
            let _ = t.selection_line(4, 0);
        }),
        ("selection_clear", |t| t.selection_clear()),
        ("resize", |t| t.resize(8, 40).expect("admitted geometry")),
    ] {
        mutate(&mut t);
        assert!(
            t.host_visual_gen > last,
            "{label} must bump the host-visual generation, or the frame \
             gate will serve a stale frame after it"
        );
        assert!(
            t.needs_frame(),
            "{label} must reopen the frame gate end to end"
        );
        last = t.host_visual_gen;
        t.render_headless().expect("renderable frame");
    }
}

#[test]
fn host_theme_colors_are_the_dynamic_color_reset_baseline() {
    let mut term = Terminal::new(2, 4);
    apply_terminal_theme_colors(
        &mut term,
        0x0011_2233,
        0x0044_5566,
        0x0077_8899,
        0x000A_0B0C,
    );
    term.process(b"\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\");
    term.process(b"\x1b]11;rgb:dddd/eeee/ffff\x1b\\");
    term.process(b"\x1b]12;rgb:1111/2222/3333\x1b\\");
    term.process(b"\x1b]17;rgb:0101/0202/0303\x1b\\");
    term.process(b"\x1b]19;rgb:0404/0505/0606\x1b\\");
    assert_eq!(term.default_foreground(), Rgb::new(0xaa, 0xbb, 0xcc));
    assert_eq!(term.default_background(), Rgb::new(0xdd, 0xee, 0xff));
    assert_eq!(
        term.selection_background(),
        Some(Rgb::new(0x01, 0x02, 0x03))
    );
    assert_eq!(
        term.selection_foreground(),
        Some(Rgb::new(0x04, 0x05, 0x06))
    );

    term.process(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07");
    assert_eq!(term.default_foreground(), Rgb::new(0x11, 0x22, 0x33));
    assert_eq!(term.default_background(), Rgb::new(0x44, 0x55, 0x66));
    assert_eq!(term.cursor_color(), Some(Rgb::new(0x77, 0x88, 0x99)));
    assert_eq!(
        term.selection_background(),
        Some(Rgb::new(0x0a, 0x0b, 0x0c))
    );
    assert_eq!(term.selection_foreground(), None);
}

#[test]
fn frame_scratch_tracks_live_sparse_blank_and_cursor_colors() {
    let mut t = crate::system_font_terminal(3, 8, 16.0);
    t.set_default_foreground(0x11, 0x22, 0x33);
    t.set_default_background(0x44, 0x55, 0x66);
    t.set_selection_fg(Some(0x0012_3456));
    t.process(b"X");

    t.refill_frame_scratch();
    assert!(
        t.frame_scratch.cells[0].len() < t.cols,
        "the regression requires an unmaterialized sparse row tail"
    );
    assert_eq!(t.frame_scratch.default_bg, 0x0044_5566);
    assert_eq!(t.frame_scratch.cells[0][0].bg, [0x44, 0x55, 0x66]);
    assert_eq!(
        t.frame_scratch.cursor_color, t.theme_cursor,
        "the configured OSC 12 baseline matches the host theme"
    );
    assert_eq!(t.frame_scratch.selection_bg, Theme::default().selection);
    assert_eq!(t.frame_scratch.selection_fg, 0x0012_3456);

    t.process(b"\x1b]11;rgb:7777/8888/9999\x1b\\");
    t.process(b"\x1b]12;rgb:aaaa/bbbb/cccc\x1b\\");
    t.process(b"\x1b]17;rgb:0101/0202/0303\x1b\\");
    t.process(b"\x1b]19;rgb:0404/0505/0606\x1b\\");
    t.refill_frame_scratch();
    assert_eq!(t.frame_scratch.default_bg, 0x0077_8899);
    assert_eq!(t.frame_scratch.cells[0][0].bg, [0x77, 0x88, 0x99]);
    assert_eq!(t.frame_scratch.cursor_color, 0x00aa_bbcc);
    assert_eq!(t.frame_scratch.selection_bg, 0x0001_0203);
    assert_eq!(t.frame_scratch.selection_fg, 0x0004_0506);

    t.process(b"\x1b[?5h");
    t.refill_frame_scratch();
    assert_eq!(
        t.frame_scratch.default_bg, 0x0011_2233,
        "DECSCNM makes the live default foreground the effective blank background"
    );
    assert_eq!(t.frame_scratch.cells[0][0].bg, [0x11, 0x22, 0x33]);

    t.process(b"\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07\x1b[?5l");
    t.refill_frame_scratch();
    assert_eq!(
        t.frame_scratch.default_bg, 0x0044_5566,
        "OSC 111 restores the configured background"
    );
    assert_eq!(
        t.frame_scratch.cursor_color, t.theme_cursor,
        "OSC 112 restores the host-configured cursor baseline"
    );
    assert_eq!(t.frame_scratch.selection_bg, Theme::default().selection);
    assert_eq!(t.frame_scratch.selection_fg, 0x0012_3456);

    t.process(b"\x1b]21;cursor=\x07\x1b]10;#DEADBE\x07");
    t.refill_frame_scratch();
    assert_eq!(t.term.cursor_color(), None);
    assert_eq!(
        t.frame_scratch.cursor_color, 0x00DE_ADBE,
        "OSC 21 cursor= makes the embedded cursor follow live OSC 10"
    );
    t.process(b"\x1b]10;#1234AB\x07");
    t.refill_frame_scratch();
    assert_eq!(
        t.frame_scratch.cursor_color, 0x0012_34AB,
        "a later OSC 10 recolors the still-dynamic cursor"
    );
}

#[test]
fn webgl_present_result_propagates_every_surface_failure() {
    assert_eq!(webgl_present_result(Ok(())), Ok(()));
    for failure in [
        SurfacePresentFailure::Reconfigured,
        SurfacePresentFailure::Timeout,
        SurfacePresentFailure::Occluded,
        SurfacePresentFailure::Validation,
    ] {
        assert!(webgl_present_result(Err(failure)).is_err(), "{failure:?}");
    }
}

/// `search_meta` agrees with `search` and carries the `incomplete` signal
/// `search` drops.
#[test]
fn search_meta_agrees_with_search_and_is_silent_on_empty_or_bad_input() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.process(b"alpha beta\r\nalpha gamma\r\nno match here\r\n");
    let matches = t.search("alpha", true, false);
    let meta = t.search_meta("alpha", true, false);
    assert_eq!(meta.match_count() as usize, matches.len() / 3);
    assert_eq!(meta.match_count(), 2);
    assert!(!meta.incomplete());
    let empty = t.search_meta("", true, false);
    assert_eq!((empty.incomplete(), empty.match_count()), (false, 0));
    let bad = t.search_meta("(", true, true);
    assert_eq!((bad.incomplete(), bad.match_count()), (false, 0));
}

/// Reject an oversized frame before constructing or changing a grid.
#[test]
fn oversized_frames_are_refused_before_state_changes() {
    let new = |rows, cols| {
        AtermGpuTerminal::new(
            rows,
            cols,
            BUNDLED_FACE,
            16.0,
            0xffffff,
            0,
            0xffffff,
            0x444444,
        )
    };
    assert!(
        new(u16::MAX, u16::MAX).is_err(),
        "constructor refuses before allocating the oversized grid"
    );
    let mut t = new(0, 0).expect("zero dimensions clamp up");
    assert_eq!((t.rows, t.cols), (1, 1));
    t.resize(2, 8).expect("small resize");
    t.process(b"kept");
    t.render_headless().expect("first frame");
    t.render_headless().expect("settled frame");
    let frame_rows = t.frame_scratch.rows;
    let frame_cols = t.frame_scratch.cols;
    let metrics = (
        t.cell_width(),
        t.cell_height(),
        t.chrome_pad(),
        t.chrome_head(),
    );
    let key = t.last_frame_key;
    let generation = t.host_visual_gen;
    type Change = fn(&mut AtermGpuTerminal) -> Result<(), String>;
    let attempts: [(&str, Change); 3] = [
        ("resize", |t| t.resize(u16::MAX, u16::MAX)),
        ("chrome", |t| t.set_chrome(u16::MAX, u16::MAX)),
        ("zoom", |t| t.set_px(1_000_000.0)),
    ];
    for (name, change) in attempts {
        assert!(change(&mut t).is_err(), "{name}");
        assert_eq!((t.rows, t.cols), (2, 8), "{name}");
        assert_eq!(t.row_text(0).as_deref(), Some("kept"), "{name}");
        assert_eq!(
            (t.term.grid().rows(), t.term.grid().cols()),
            (2, 8),
            "{name}"
        );
        assert_eq!(
            (
                t.cell_width(),
                t.cell_height(),
                t.chrome_pad(),
                t.chrome_head()
            ),
            metrics,
            "{name}"
        );
        assert_eq!(t.host_visual_gen, generation, "{name}");
        assert_eq!(t.last_frame_key, key, "{name}");
        assert!(
            !t.needs_frame(),
            "{name} did not consume or invalidate the settled frame"
        );
        assert_eq!(
            (t.frame_scratch.rows, t.frame_scratch.cols),
            (frame_rows, frame_cols),
            "{name}: previous frame remains"
        );
    }
    t.set_line_height(1_000_000.0);
    assert!(
        t.render_headless().is_err(),
        "oversized geometry is refused before building a frame"
    );
    assert_eq!(
        (t.frame_scratch.rows, t.frame_scratch.cols),
        (frame_rows, frame_cols)
    );
    t.set_line_height(1.0);
    t.render_headless()
        .expect("corrected geometry builds again");
    t.resize(0, 0).expect("zero resize clamps up");
    assert_eq!((t.rows, t.cols), (1, 1));

    let mut banded =
        AtermGpuTerminal::new(2, 1, BUNDLED_FACE, 6_000.0, 0xffffff, 0, 0xffffff, 0x444444)
            .expect("the grid alone fits the frame budget");
    assert!(banded.checked_surface_size().is_ok());
    let reply = banded.notice_at(
        "post ci sev=warn Attention",
        aterm_time::Instant::now(),
        aterm_messages::WallStamp { unix_ms: 0 },
    );
    assert!(reply.starts_with("OK"), "{reply}");
    assert_eq!(banded.band_rows(), 1);
    assert!(
        banded.render_headless().is_err(),
        "the added physical band row exceeds the budget"
    );
    assert_eq!(banded.frame_scratch.rows, 0, "no frame was built");
}

/// Shrinking retention invalidates the search index like any content
/// change, so a search never returns just-evicted absolute rows.
#[test]
fn retention_shrink_search_returns_no_evicted_rows() {
    let mut t = crate::system_font_terminal(5, 40, 16.0);
    for i in 0..300 {
        t.process(format!("needle-{i}\r\n").as_bytes());
    }
    assert_eq!(t.search("needle-", true, false).len() / 3, 300);

    t.set_scrollback_limit(50);
    let hits = t.search("needle-", true, false);
    assert_eq!(hits.len() / 3, 54, "evicted rows no longer match");
    assert_eq!(hits[0], 246, "the first match is the oldest RETAINED row");
}

/// `set_scrollback_limit` is one total bound across ring + staged + store:
/// shrink evicts oldest immediately; grow widens the store share.
#[test]
fn set_scrollback_limit_governs_ring_retention() {
    let mut t = crate::system_font_terminal(5, 40, 16.0);
    t.set_scrollback_limit(200);
    for i in 0..500 {
        t.process(format!("line-{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    assert_eq!(
        t.display_offset(),
        200,
        "a 200-line limit really caps retention (was a silent no-op)"
    );
    t.scroll_to_bottom();
    t.set_scrollback_limit(12_000);
    for i in 500..11_500 {
        t.process(format!("line-{i}\r\n").as_bytes());
    }
    t.scroll_to_top();
    assert_eq!(
        t.display_offset(),
        200 + 11_000,
        "retention grows past the old 10k ring cap"
    );
}

/// win32-input-mode is the host's to grant: conhost's `?9001h` changes
/// nothing until the embedder says a ConPTY is behind the terminal
/// (`set_win32_input_enabled`).
#[test]
fn a_conpty_host_opts_in_to_win32_input_records() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.process(b"\x1b[?9001h");
    assert_eq!(
        t.encode_key("Enter", 1, 0, None).as_deref(),
        Some(&[0x0a][..]),
        "program output alone grants nothing"
    );
    t.set_win32_input_enabled(true);
    t.process(b"\x1b[?9001h");
    let pair = b"\x1b[13;28;13;1;16;1_\x1b[13;28;13;0;16;1_";
    assert_eq!(
        t.encode_key("Enter", 1, 0, None).as_deref(),
        Some(&pair[..])
    );
}

/// The stateless encoder over mirrored `keyboard_mode_bits` (the worker path).
#[test]
fn encode_key_with_mode_matches_the_instance_encoder() {
    assert_eq!(
        encode_key_with_mode("ArrowUp", 0, 0, None, 0).as_deref(),
        Some(&b"\x1b[A"[..])
    );
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    assert_eq!(t.keyboard_mode_bits(), 0, "fresh terminal: no mode bits");
    t.process(b"\x1b[?1h");
    assert_eq!(
        encode_key_with_mode("ArrowUp", 0, 0, None, t.keyboard_mode_bits()).as_deref(),
        Some(&b"\x1bOA"[..]),
        "snapshot bits under DECCKM must be SS3"
    );
    t.process(b"\x1b[?1l");
    t.process(b"\x1b[>1u");
    assert_eq!(
        encode_key_with_mode("Enter", 1, 0, None, t.keyboard_mode_bits()).as_deref(),
        Some(&b"\x1b[13;2u"[..]),
        "snapshot bits under pushed kitty must be CSI-u Shift+Enter"
    );
    for (key, mods, event_type) in [
        ("ArrowUp", 0u8, 0u8),
        ("ArrowUp", 1, 0),
        ("Enter", 0, 0),
        ("Enter", 1, 0),
        ("a", 4, 0),
        ("Shift", 0, 0),
        ("ArrowUp", 0, 2),
    ] {
        assert_eq!(
            t.encode_key(key, mods, event_type, None),
            encode_key_with_mode(key, mods, event_type, None, t.keyboard_mode_bits()),
            "instance and stateless encoders must agree on {key:?} mods={mods} type={event_type}"
        );
    }
}

/// `build_frame` re-resolves one row per echo with the damage-reading
/// effects on: the refill consumes the damage session and the effects
/// pipeline does not re-take it.
#[test]
fn build_frame_echo_refills_one_row_with_sparkle_and_rain_on() {
    let mut t = crate::system_font_terminal(8, 40, 16.0);
    t.set_sparkle_words_enabled(true);
    t.set_matrix_rain_enabled(true);
    t.process(b"$ ");
    t.build_frame();
    assert!(
        matches!(t.last_refill, Some(FrameRefill::Full { .. })),
        "the first fill is full by construction"
    );
    for (i, ch) in b"hello world".iter().enumerate() {
        t.process(std::slice::from_ref(ch));
        t.advance_effects(16.0);
        t.build_frame();
        assert_eq!(
            t.last_refill,
            Some(FrameRefill::Scoped { rows_refilled: 1 }),
            "echo #{i} must refill exactly its one row"
        );
    }
}

/// Every built frame's engine channels equal a full extraction across a
/// mutation stream with the damage-reading effects on, and both refill
/// arms are reached.
#[test]
fn build_frame_scoped_refill_matches_a_full_extraction_over_a_mutation_stream() {
    let mut t = crate::system_font_terminal(8, 40, 16.0);
    t.set_sparkle_words_enabled(true);
    t.set_matrix_rain_enabled(true);
    let steps: &[&[u8]] = &[
        b"$ ",
        b"e",
        b"cho hello cat",
        b"\r\n",
        b"\x1b[31mred\x1b[0m and plain",
        b"\x1b[2K\rerased and retyped",
        b"\x1b]11;#102030\x07",
        b"a line long enough to wrap past the forty column edge of this grid",
        b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n",
        b"\x1b[?1049h",
        b"alt screen text",
        b"\x1b[?1049l",
        b"x",
    ];
    let (mut scoped, mut full) = (0usize, 0usize);
    let mut check = |t: &mut AtermGpuTerminal, what: &str| {
        let (rows, cols) = (t.rows, t.cols);
        let mut oracle = t.frame_scratch.clone();
        t.term.cell_frame_into(&mut oracle, rows, cols);
        assert!(
            t.frame_scratch == oracle,
            "the built frame diverged from a full extraction after: {what}"
        );
        match t.last_refill {
            Some(FrameRefill::Scoped { .. }) => scoped += 1,
            Some(FrameRefill::Full { .. }) => full += 1,
            None => panic!("build_frame must refill"),
        }
    };
    for step in steps {
        t.process(step);
        t.advance_effects(16.0);
        t.build_frame();
        check(&mut t, &String::from_utf8_lossy(step));
    }
    t.scroll_lines(3);
    t.build_frame();
    check(&mut t, "history viewport");
    t.scroll_lines(-3);
    t.build_frame();
    check(&mut t, "back to the live bottom");
    t.resize(10, 30).expect("admitted geometry");
    t.build_frame();
    check(&mut t, "resize");
    t.process(b"after resize");
    t.build_frame();
    check(&mut t, "echo after resize");
    assert!(scoped >= 5, "the scoped arm must be reached ({scoped})");
    assert!(full >= 2, "the full arm must be reached ({full})");
}

/// Native stand-in for the effects render path (GPU init is wasm-only):
/// refill the snapshot, apply the effects pipeline, refresh the spill band.
fn fill_frame(t: &mut AtermGpuTerminal) {
    t.refill_frame_scratch();
    let (cw, ch) = t.cpu.cell_size();
    t.effects.apply(&mut t.term, &mut t.frame_scratch, cw, ch);
    t.spill.update(&t.cpu, &t.frame_scratch);
}

/// Snapshot equality across two engine instances: each publishes its own
/// fx layer contents (identity is per instance), so the layers compare by
/// value (their arts by content id) and the rest by `RenderInput`'s own
/// equality.
fn same_snapshot_across_instances(a: &RenderInput, b: &RenderInput) -> bool {
    use aterm_core::render::{FxContent, FxItem, FxLayer};
    fn content_value_eq(x: &FxContent, y: &FxContent) -> bool {
        x.palette() == y.palette()
            && x.items().len() == y.items().len()
            && x.items().iter().zip(y.items()).all(|pair| match pair {
                (FxItem::Rect(p), FxItem::Rect(q)) => p == q,
                (FxItem::Ramps(p), FxItem::Ramps(q)) => p == q,
                (FxItem::Sprite(p), FxItem::Sprite(q)) => {
                    (p.x, p.y, p.dw, p.dh, p.flip_x, p.ink, p.clip)
                        == (q.x, q.y, q.dw, q.dh, q.flip_x, q.ink, q.clip)
                        && p.art.id() == q.art.id()
                }
                _ => false,
            })
    }
    fn layer_value_eq(x: &FxLayer, y: &FxLayer) -> bool {
        (
            x.id, x.slot, x.space, x.blend, x.bloom, x.knock, x.offset, x.clip, x.chrome,
        ) == (
            y.id, y.slot, y.space, y.blend, y.bloom, y.knock, y.offset, y.clip, y.chrome,
        ) && content_value_eq(&x.content, &y.content)
    }
    if a.fx_layers.len() != b.fx_layers.len()
        || !a
            .fx_layers
            .iter()
            .zip(&b.fx_layers)
            .all(|(x, y)| layer_value_eq(x, y))
    {
        return false;
    }
    let mut aligned = b.clone();
    aligned.fx_layers.clone_from(&a.fx_layers);
    *a == aligned
}

/// Spill band on the GPU engine's CPU face: the water jump splash on the
/// top row surfaces band bytes whose source-over composite onto the theme
/// bg equals the CPU face's composed band exactly.
#[test]
fn spill_exports_surface_band_content_on_the_cpu_face() {
    let mut t = crate::system_font_terminal(12, 40, 16.0);
    fill_frame(&mut t);
    assert_eq!(t.spill_len(), 0, "0/0 chrome: identity — no band, no bytes");
    assert_eq!(t.spill_rev(), 0);

    const TRAIL_MS: u32 = 400;
    const FRAME_MS: f64 = 16.0;
    let (pad, head) = (12usize, 30usize);
    t.set_chrome(pad as u16, head as u16)
        .expect("admitted geometry");
    t.set_cursor_glow(true, "water", None, None, TRAIL_MS, 64, 1.0, 2.0, true);
    fill_frame(&mut t);
    for (ch, bytes) in [('a', b"a"), ('b', b"b"), ('c', b"c")] {
        assert!(
            t.note_typed_char(ch),
            "a witnessed simple scalar must arm movement provenance"
        );
        t.process(bytes);
        t.advance_effects(30.0);
        fill_frame(&mut t);
    }
    let mut lit_frame = false;
    for _ in 0..40 {
        if t.spill_rev() > 0
            && t.spill_rect_count() > 0
            && t.spill
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] != 0)
        {
            lit_frame = true;
            break;
        }
        t.advance_effects(FRAME_MS);
        fill_frame(&mut t);
    }
    let (w, h) = t.cpu.frame_size(t.rows, t.cols);
    let grid_h = h - 2 * pad - head;
    assert_eq!(
        t.spill_len(),
        (w * (pad + head) + w * pad + 2 * pad * grid_h) * 4,
        "spill buffer sized to the four band strips"
    );
    assert!(
        lit_frame,
        "witnessed splash droplets must reach the band live"
    );
    assert!(t.spill_rev() > 0, "splash droplets must reach the band");
    assert!(t.spill_rect_count() > 0, "band content must report rects");

    let input = t.frame_scratch.clone();
    let mut win = aterm_render::WindowCpu::new();
    let view = t.cpu.render_input_cached(&mut win, &input);
    assert_eq!((view.width(), view.height()), (w, h));
    let pixels = view.pixels();
    let bg = Theme::default().bg & 0x00FF_FFFF;
    let strips = [
        (0usize, 0usize, w, pad + head),
        (0, h - pad, w, pad),
        (0, pad + head, pad, grid_h),
        (w - pad, pad + head, pad, grid_h),
    ];
    let buf = t.spill.rgba();
    let mut lit = 0usize;
    let mut off = 0usize;
    for (sx, sy, sw, sh) in strips {
        for yy in 0..sh {
            for xx in 0..sw {
                let px = &buf[(off + yy * sw + xx) * 4..][..4];
                let (x, y) = (sx + xx, sy + yy);
                let composed = if px[3] == 0 {
                    bg
                } else {
                    lit += 1;
                    aterm_render::over_rgb(
                        bg,
                        (u32::from(px[0]) << 16) | (u32::from(px[1]) << 8) | u32::from(px[2]),
                        px[3],
                    )
                };
                assert_eq!(
                    composed,
                    pixels[y * w + x] & 0x00FF_FFFF,
                    "spill ∘ bg must equal the frame band at ({x},{y})"
                );
            }
        }
        off += sw * sh;
    }
    assert!(lit > 0, "the splash must light band pixels (non-vacuous)");

    t.effects.set_serious(true);
    t.process(b"\x1b[1;30Hz");
    let max_life_s = aterm_effects::cursor_glow::scaled_typed_life(
        aterm_effects::cursor_glow::CursorGlow::WATER_CHAIN_LIFE_MAX,
        aterm_effects::cursor_glow::typed_life_scale(std::time::Duration::from_millis(u64::from(
            TRAIL_MS,
        ))),
    );
    let drain_frames = ((f64::from(max_life_s) * 1000.0) / FRAME_MS).ceil() as usize + 1;
    let mut drained_at = None;
    let mut last_quads = usize::MAX;
    for i in 0..drain_frames {
        t.advance_effects(FRAME_MS);
        fill_frame(&mut t);
        let band_lit = t
            .spill
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] != 0)
            .count();
        assert_eq!(
            band_lit, 0,
            "frame {i} after an unwitnessed leap put {band_lit} lit pixels in the \
             band — cold program movement must not splash the chrome"
        );
        last_quads = t
            .frame_scratch
            .cursor_glow_add_ramps
            .laid(t.frame_scratch.cursor_glow_add.len());
        if last_quads == 0 {
            drained_at = Some(i);
            break;
        }
    }
    assert!(
        drained_at.is_some(),
        "the earned aurora still had {last_quads} quads after {drain_frames} frames \
         ({} ms): an unwitnessed leap retires the wake and the remaining light must \
         fade on its own envelope, not persist",
        (drain_frames as f64) * FRAME_MS
    );

    let ptr = t.spill_ptr();
    t.advance_effects(FRAME_MS);
    fill_frame(&mut t);
    assert_eq!(t.spill_ptr(), ptr, "same geometry ⇒ the export never moves");

    let rev = t.spill_rev();
    fill_frame(&mut t);
    assert_eq!(t.spill_rev(), rev, "idle re-frame must not tick the rev");
    assert_eq!(t.spill_rect_count(), 0);

    t.set_chrome(0, 0).expect("admitted geometry");
    fill_frame(&mut t);
    assert_eq!(t.spill_len(), 0);
    let rev = t.spill_rev();
    fill_frame(&mut t);
    assert_eq!(t.spill_rev(), rev, "steady 0/0 chrome stays still");
}

/// `set_chrome` before GPU init sticks on the CPU face (init re-applies it
/// and sizes the swapchain to the padded frame); 0/0 restores exact-fit.
#[test]
fn set_chrome_sticks_pre_init_and_zero_restores() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    assert_eq!((t.cpu.pad(), t.cpu.head()), (0, 0), "exact-fit by default");
    t.set_chrome(8, 24).expect("admitted geometry");
    assert_eq!((t.cpu.pad(), t.cpu.head()), (8, 24));
    let (cw, ch) = t.cpu.cell_size();
    let (w, h) = t.cpu.frame_size(24, 80);
    assert_eq!(w, 80 * cw + 16, "frame gains 2*pad in width");
    assert_eq!(h, 24 * ch + 16 + 24, "frame gains 2*pad + head in height");
    t.set_chrome(0, 0).expect("admitted geometry");
    assert_eq!(t.cpu.frame_size(24, 80), (80 * cw, 24 * ch));
}

/// Two instances fed the same bytes and dt stream produce identical
/// `RenderInput`s (cells and every overlay channel), and turning everything
/// off restores the untouched-instance snapshot exactly.
#[test]
fn effects_snapshots_are_deterministic_and_restore_on_off() {
    let mut a = crate::system_font_terminal(24, 80, 16.0);
    let mut b = crate::system_font_terminal(24, 80, 16.0);
    let mut plain = crate::system_font_terminal(24, 80, 16.0);
    for t in [&mut a, &mut b] {
        t.set_sparkle_words_enabled(true);
        t.set_cursor_glow(
            true,
            "fire",
            Some(0x00FF_8844),
            None,
            400,
            24,
            0.9,
            0.8,
            true,
        );
    }
    for step in 0..30u32 {
        for t in [&mut a, &mut b, &mut plain] {
            t.process(b"kitty fuck x");
            t.advance_effects(33.0);
            fill_frame(t);
        }
        assert!(
            same_snapshot_across_instances(&a.frame_scratch, &b.frame_scratch),
            "same bytes + dt stream must produce identical snapshots (step {step})"
        );
    }
    assert!(
        a.is_effects_active()
            || !a.frame_scratch.word_decorations.is_empty()
            || !a.frame_scratch.ink.is_empty(),
        "effects actually emitted something while on"
    );
    for t in [&mut a, &mut b] {
        t.set_sparkle_words_enabled(false);
        t.set_cursor_glow(false, "lumen", None, None, 260, 24, 0.7, 0.6, true);
    }
    for t in [&mut a, &mut b, &mut plain] {
        t.advance_effects(33.0);
        fill_frame(t);
    }
    assert!(
        same_snapshot_across_instances(&a.frame_scratch, &plain.frame_scratch),
        "toggled off must restore the byte-identical snapshot"
    );
}

/// The word-separator setting reshapes double-click words, and clearing it
/// restores the default class-based word logic exactly.
#[test]
fn set_word_separators_reshapes_double_click_words() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.process(b"foo-bar baz");
    assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
    t.set_word_separators(Some(" ".to_string()));
    assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo-bar"));
    t.set_word_separators(None);
    assert_eq!(t.selection_word(0, 1).as_deref(), Some("foo"));
}

/// Swap the engine for one whose bulk history lives in the tiered store
/// (tiny ring → nearly all scroll-off spills to tiered) and feed `lines`
/// numbered lines short enough not to wrap at the test widths.
fn install_tiered_history(t: &mut AtermGpuTerminal, rows: u16, cols: u16, lines: usize) {
    let sb = aterm_core::scrollback::Scrollback::new(64, 512, 64_000_000);
    t.term = Terminal::with_scrollback(rows, cols, 8, sb);
    t.rows = t.term.grid().rows() as usize;
    t.cols = t.term.grid().cols() as usize;
    let mut buf = Vec::new();
    for i in 0..lines {
        buf.extend_from_slice(format!("L{i}-hist\r\n").as_bytes());
    }
    t.term.process(&buf);
}

#[test]
fn resize_without_tiered_store_never_defers() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.term = Terminal::new(24, 80);
    for i in 0..50 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    t.resize(24, 40).expect("admitted geometry");
    assert!(!t.reflow_pending(), "no tiered store → no deferred job");
    assert!(!t.pump_reflow(), "nothing to pump");
    assert_eq!(t.cols, 40, "resize applied synchronously");
}

#[test]
fn scrolled_wrapped_row_len_and_wrap_flag_are_tier_aware() {
    let mut t = crate::system_font_terminal(3, 40, 16.0);
    let a = "A".repeat(30);
    let b = "B".repeat(30);
    let c = "C".repeat(30);
    t.process(format!("{a}\r\n").as_bytes());
    t.process(format!("{b}\r\n").as_bytes());
    t.process(c.as_bytes());

    t.resize(3, 20).expect("admitted geometry");
    while t.pump_reflow() {}

    t.scroll_to_top();
    assert_eq!(t.row_text(0).as_deref(), Some("A".repeat(20).as_str()));
    assert_eq!(t.row_text(1).as_deref(), Some("A".repeat(10).as_str()));

    assert_eq!(t.row_len(0), Some(20), "history head-row length");
    assert_eq!(t.row_is_wrapped(0), Some(false), "history head not wrapped");
    assert_eq!(
        t.row_len(1),
        Some(10),
        "scrolled wrapped continuation must report its materialized length, not None"
    );
    assert_eq!(
        t.row_is_wrapped(1),
        Some(true),
        "scrolled wrapped continuation must report is_wrapped=true, not None"
    );
    assert_eq!(t.row_len(99), None);
    assert_eq!(t.row_is_wrapped(99), None);
}

#[test]
fn large_tiered_history_defers_rewrap_and_stepped_pumps_reattach_it() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
    let before = t.term.grid().scrollback_lines();
    assert!(before > INLINE_REFLOW_MAX_LINES, "precondition: {before}");

    t.resize(24, 40).expect("admitted geometry");
    assert!(t.reflow_pending(), "a deep history defers its rewrap");
    assert_eq!(t.cols, 40, "the visible grid resized synchronously");
    let during = t.term.grid().scrollback_lines();
    assert!(
        during < before / 4,
        "during the detach window only the bounded ring is visible \
         (got {during} of {before})"
    );

    let mut lcg = 0x0600_60DD_u64;
    let mut pumps = 0usize;
    loop {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
        t.pump_reflow_budget(1 + ((lcg >> 33) % 3_000) as u32);
        pumps += 1;
        assert!(pumps < 100_000, "stepped pumping must terminate");
        if !t.pump_reflow() {
            break;
        }
        assert!(
            t.reflow_pending(),
            "work remains while pump_reflow returns true"
        );
    }
    assert!(
        pumps > 4,
        "small budgets must actually chunk ({pumps} pumps)"
    );
    assert!(!t.reflow_pending(), "window closed after the final step");
    assert!(!t.pump_reflow(), "further pumps are no-ops");
    let after = t.term.grid().scrollback_lines();
    assert!(
        after >= before,
        "history preserved across detach→stepped pumps→re-attach \
         (before={before}, after={after})"
    );
}

#[test]
fn resize_mid_stepping_supersedes_without_losing_history() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
    let before = t.term.grid().scrollback_lines();

    t.resize(24, 40).expect("admitted geometry");
    assert!(t.reflow_pending());
    t.pump_reflow_budget(500);
    for _ in 0..3 {
        assert!(t.pump_reflow(), "mid-stepping: work must remain");
    }
    assert!(t.reflow_pending(), "job is half-stepped");

    t.resize(24, 60).expect("admitted geometry");
    assert!(
        t.reflow_pending(),
        "the half-stepped job survives a re-resize"
    );
    assert_eq!(t.cols, 60, "the newest geometry wins for the grid");

    let mut pumps = 0usize;
    while t.pump_reflow() {
        pumps += 1;
        assert!(pumps < 100_000, "stepping must terminate");
    }
    let after = t.term.grid().scrollback_lines();
    assert!(
        after >= before,
        "no history lost across the mid-stepping supersede \
         (before={before}, after={after})"
    );
}

#[test]
fn small_tiered_history_rewraps_inline_on_resize() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    install_tiered_history(&mut t, 24, 80, 500);
    let before = t.term.grid().scrollback_lines();
    assert!(before > 100, "precondition: deep history ({before} lines)");
    t.resize(24, 40).expect("admitted geometry");
    assert!(
        !t.reflow_pending(),
        "≤ INLINE_REFLOW_MAX_LINES rewraps inline, mirroring the native bound"
    );
    assert!(
        t.term.grid().scrollback_lines() >= before,
        "inline rewrap preserves history"
    );
}

#[test]
fn never_pumped_host_completes_via_the_render_grace_net() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
    let before = t.term.grid().scrollback_lines();
    t.resize(24, 40).expect("admitted geometry");
    assert!(t.reflow_pending());
    t.pump_reflow_on_render_tick();
    assert!(t.reflow_pending(), "no pump inside the grace window");
    for _ in 0..REFLOW_PUMP_GRACE_RENDERS {
        t.pump_reflow_on_render_tick();
    }
    assert!(
        t.reflow_pending(),
        "a multi-step job is still converging right after grace \
         (one bounded step per frame — not one catch-up frame)"
    );
    let mut frames = 0usize;
    while t.reflow_pending() {
        t.pump_reflow_on_render_tick();
        frames += 1;
        assert!(
            frames < 10_000,
            "the render net must converge across frames"
        );
    }
    assert!(
        t.term.grid().scrollback_lines() >= before,
        "history restored by the per-frame safety-net steps"
    );
}

#[test]
fn never_pumped_host_streaming_output_converges_via_the_backlog_net() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    install_tiered_history(&mut t, 24, 80, INLINE_REFLOW_MAX_LINES + 5_000);
    t.resize(24, 40).expect("admitted geometry");
    assert!(t.reflow_pending());
    let mut buf = Vec::new();
    for i in 0..(REFLOW_BACKLOG_MAX_LINES + 2_000) {
        buf.extend_from_slice(format!("W{i}-window\r\n").as_bytes());
    }
    t.process(&buf);
    let mut calls = 0usize;
    while t.reflow_pending() {
        t.process(format!("T{calls}-tail\r\n").as_bytes());
        calls += 1;
        assert!(
            calls < 1_000,
            "the backlog net must converge within finitely many process \
             calls (still pending after {calls})"
        );
    }
    assert!(
        t.term.grid().scrollback_lines() > INLINE_REFLOW_MAX_LINES,
        "history survived the stream-while-detached window"
    );
}

/// Budgeted search: resumed slices complete to the one-shot triplets, a
/// superseded cursor restarts, and empty-query / invalid-regex edges give
/// an empty complete result.
#[test]
fn search_budgeted_matches_one_shot_and_restarts_when_superseded() {
    let mut t = crate::system_font_terminal(6, 40, 16.0);
    for i in 0..60 {
        t.process(format!("row {i} NEEDLE-{i} tail\r\n").as_bytes());
    }
    let one_shot = t.search("NEEDLE", true, false);
    let mut cursor = None;
    let mut all_matches = Vec::new();
    let mut search_id = None;
    let step = loop {
        let step = t.search_budgeted("NEEDLE", true, false, cursor, 9);
        if step.reset() {
            assert!(search_id.is_none(), "only the first turn resets here");
            all_matches.clear();
            search_id = step.search_id();
        } else {
            assert_eq!(step.search_id(), search_id);
        }
        all_matches.extend(step.matches());
        if step.complete() {
            break step;
        }
        assert!(step.cursor().is_some(), "incomplete step carries a cursor");
        cursor = step.cursor();
    };
    assert_eq!(step.cursor(), None, "complete step drops the cursor");
    assert_eq!(all_matches, one_shot, "budgeted equals one-shot");
    assert_eq!(step.lowest_retained_line(), 0);

    let first = t.search_budgeted("NEEDLE", true, false, None, 5);
    assert!(!first.complete());
    assert!(first.reset());
    let first_id = first.search_id();
    let switched = t.search_budgeted("row", true, false, first.cursor(), 5);
    assert!(switched.rows_fed() <= 5, "superseded cursor restarted");
    assert!(switched.reset());
    assert_ne!(switched.search_id(), first_id);
    t.search_budgeted_cancel();
    let empty = t.search_budgeted("", true, false, switched.cursor(), 5);
    assert!(empty.complete() && empty.matches().is_empty());
    assert!(empty.reset() && empty.search_id().is_none());
    let bad = t.search_budgeted("f(oo", false, true, None, 5);
    assert!(bad.complete() && bad.matches().is_empty());
    assert!(bad.reset() && bad.search_id().is_none());
}
