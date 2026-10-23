// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Config reload preserves program-set OSC palette, background, and cursor colours.

use aterm_core::config::{ConfigChange, TerminalConfig};
use aterm_core::terminal::{ColorPalette, Rgb, Terminal};

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// What the program sets.
const PROGRAM_P4: Rgb = rgb(0x28, 0x2a, 0x36);
const PROGRAM_BG: Rgb = rgb(0x10, 0x11, 0x12);
const PROGRAM_CURSOR: Rgb = rgb(0xfe, 0x01, 0x80);
const PROGRAM_SETS: &[u8] =
    b"\x1b]4;4;rgb:28/2a/36\x07\x1b]11;rgb:10/11/12\x07\x1b]12;rgb:fe/01/80\x07";

/// A host theme whose every colour is derived from `seed`.
fn theme(seed: u8) -> TerminalConfig {
    let mut palette = ColorPalette::new();
    for index in 0..16u8 {
        palette.set(index, rgb(seed, index, seed.wrapping_add(index)));
    }
    TerminalConfig {
        default_foreground: rgb(seed, seed, seed),
        default_background: rgb(seed / 2, 0, seed / 3),
        cursor_color: Some(rgb(0, seed, 0)),
        selection_background: Some(rgb(seed, 0, 0)),
        custom_palette: Some(palette),
        allow_palette_reconfigure: true,
        ..TerminalConfig::default()
    }
}

const OLD: u8 = 200;
const NEW: u8 = 90;

/// A terminal on theme `OLD` running a program that set its colours.
fn running_program() -> Terminal {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&theme(OLD));
    t.process(PROGRAM_SETS);
    t
}

fn assert_program_colours(t: &Terminal, when: &str) {
    assert_eq!(t.palette_color(4), PROGRAM_P4, "OSC 4 survives {when}");
    assert_eq!(t.default_background(), PROGRAM_BG, "OSC 11 survives {when}");
    assert_eq!(
        t.cursor_color(),
        Some(PROGRAM_CURSOR),
        "OSC 12 survives {when}"
    );
}

#[test]
fn program_colours_survive_a_reload_of_the_same_config() {
    let mut t = running_program();
    let changes = t.apply_config(&theme(OLD));
    assert_program_colours(&t, "an identical reload");
    assert!(
        !changes.contains(&ConfigChange::Colors) && !changes.contains(&ConfigChange::CursorColor),
        "nothing on screen moved, so no colour change is reported: {changes:?}"
    );
}

#[test]
fn program_colours_survive_a_theme_switch_and_untouched_slots_follow_it() {
    let mut t = running_program();
    let changes = t.apply_config(&theme(NEW));
    assert_program_colours(&t, "a theme switch");

    let new = theme(NEW);
    let new_palette = new.custom_palette.as_ref().expect("theme palette");
    assert_eq!(
        t.default_foreground(),
        new.default_foreground,
        "the untouched foreground follows the new theme"
    );
    assert_eq!(
        t.selection_background(),
        new.selection_background,
        "the untouched selection colour follows the new theme"
    );
    assert_eq!(
        t.palette_color(5),
        new_palette.get(5),
        "an untouched palette entry follows the new theme"
    );
    assert!(changes.contains(&ConfigChange::Colors), "{changes:?}");
}

#[test]
fn resets_after_a_theme_switch_land_on_the_new_theme() {
    let mut t = running_program();
    t.apply_config(&theme(NEW));
    let new = theme(NEW);
    let new_palette = new.custom_palette.as_ref().expect("theme palette");

    t.process(b"\x1b]111\x07\x1b]104;4\x07\x1b]112\x07");
    assert_eq!(t.default_background(), new.default_background, "OSC 111");
    assert_eq!(t.palette_color(4), new_palette.get(4), "OSC 104;4");
    assert_eq!(t.cursor_color(), new.cursor_color, "OSC 112");

    t.process(PROGRAM_SETS);
    t.process(b"\x1bc");
    assert_eq!(t.default_background(), new.default_background, "RIS bg");
    assert_eq!(t.palette_color(4), new_palette.get(4), "RIS palette");
    assert_eq!(t.cursor_color(), new.cursor_color, "RIS cursor");
}

#[test]
fn a_colour_stack_pop_after_a_theme_switch_lands_on_the_new_theme() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&theme(OLD));
    t.process(b"\x1b]11;rgb:10/11/12\x07"); // the shell's own background
    t.process(b"\x1b]30001\x07");
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07\x1b]10;rgb:01/02/03\x07");
    t.apply_config(&theme(NEW));
    t.process(b"\x1b]30101\x07");

    let new = theme(NEW);
    let new_palette = new.custom_palette.as_ref().expect("theme palette");
    assert_eq!(
        t.default_foreground(),
        new.default_foreground,
        "the TUI's foreground is gone and the NEW theme's is back, not the old one"
    );
    assert_eq!(t.palette_color(4), new_palette.get(4), "palette 4 likewise");
    assert_eq!(
        t.palette_color(5),
        new_palette.get(5),
        "untouched entries too"
    );
    assert_eq!(
        t.default_background(),
        PROGRAM_BG,
        "what the program had set before the push comes back over the new theme"
    );
}

#[test]
fn closing_the_palette_gate_puts_the_theme_palette_back() {
    let theme_p4 = theme(OLD).custom_palette.expect("theme palette").get(4);
    let closed = TerminalConfig {
        allow_palette_reconfigure: false,
        ..theme(OLD)
    };
    let mut t = running_program();
    let changes = t.apply_config(&closed);
    assert_eq!(
        t.palette_color(4),
        theme_p4,
        "the program's palette entry is dropped with the gate"
    );
    assert_eq!(
        t.default_background(),
        PROGRAM_BG,
        "OSC 11 is ungated and stays"
    );
    assert_eq!(t.cursor_color(), Some(PROGRAM_CURSOR), "OSC 12 likewise");
    assert!(changes.contains(&ConfigChange::Colors), "{changes:?}");
    assert!(
        t.program_color_overrides().palette.is_empty(),
        "nothing of the program's palette is left to replay"
    );

    let mut open = running_program();
    open.apply_config(&TerminalConfig {
        faint_opacity: 0.4,
        ..theme(OLD)
    });
    assert_eq!(open.palette_color(4), PROGRAM_P4);
}

#[test]
fn a_pop_under_a_closed_palette_gate_leaves_the_palette_alone() {
    let theme_p4 = theme(OLD).custom_palette.expect("theme palette").get(4);
    let mut t = Terminal::new(8, 40);
    t.apply_config(&theme(OLD));
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    t.process(b"\x1b]30001\x07");
    t.process(b"\x1b]4;4;rgb:01/01/01\x07");
    let closed = TerminalConfig {
        allow_palette_reconfigure: false,
        ..theme(OLD)
    };
    t.apply_config(&closed);
    assert_eq!(
        t.palette_color(4),
        theme_p4,
        "closing the gate put the theme's entry back"
    );
    t.process(b"\x1b]30101\x07");
    assert_eq!(
        t.palette_color(4),
        theme_p4,
        "a pop is a palette write, and the gate refuses it"
    );

    let mut open = Terminal::new(8, 40);
    open.apply_config(&theme(OLD));
    open.process(b"\x1b]4;4;rgb:28/2a/36\x07\x1b]30001\x07\x1b]4;4;rgb:01/01/01\x07");
    open.process(b"\x1b]30101\x07");
    assert_eq!(open.palette_color(4), PROGRAM_P4);
}

#[test]
fn a_colour_stack_carried_across_a_handoff_pops_onto_the_successor_theme() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&theme(OLD));
    t.process(b"\x1b]30001\x07\x1b]10;rgb:01/02/03\x07");
    let cp = t.checkpoint_carry(64).expect("parser is Ground");

    let mut next = Terminal::new(8, 40);
    next.apply_config(&theme(NEW));
    next.restore_checkpoint(&cp);
    assert_eq!(
        next.default_foreground(),
        rgb(1, 2, 3),
        "the live set rides"
    );
    next.process(b"\x1b]30101\x07");
    assert_eq!(
        next.default_foreground(),
        theme(NEW).default_foreground,
        "the pop lands on the successor's theme"
    );

    let mut legacy = cp.clone();
    legacy.color.stack_overrides.clear();
    legacy.color.stack = vec![aterm_core::terminal::ColorStackEntryRepr {
        palette: (0..=u8::MAX).map(|i| next.palette_color(i)).collect(),
        foreground: theme(NEW).default_foreground,
        background: PROGRAM_BG,
        cursor: theme(NEW).cursor_color,
        selection_background: theme(NEW).selection_background,
        selection_foreground: None,
    }];
    let mut from_legacy = Terminal::new(8, 40);
    from_legacy.apply_config(&theme(NEW));
    from_legacy.restore_checkpoint(&legacy);
    from_legacy.apply_config(&theme(OLD));
    from_legacy.process(b"\x1b]30101\x07");
    assert_eq!(from_legacy.default_background(), PROGRAM_BG);
    assert_eq!(
        from_legacy.default_foreground(),
        theme(OLD).default_foreground
    );
}
