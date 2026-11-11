// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn configured_colors_survive_dynamic_color_set_and_reset() {
    let foreground = Rgb::new(0x11, 0x22, 0x33);
    let background = Rgb::new(0x44, 0x55, 0x66);
    let mut terminal = Terminal::new(24, 80);
    terminal.set_default_foreground(foreground);
    terminal.set_default_background(background);

    terminal.process(b"\x1b]10;rgb:aa/bb/cc\x07\x1b]11;rgb:77/88/99\x07");
    assert_ne!(terminal.default_foreground(), foreground);
    assert_ne!(terminal.default_background(), background);

    terminal.process(b"\x1b]110\x07\x1b]111\x07");
    assert_eq!(
        terminal.default_foreground(),
        foreground,
        "OSC 110 restores the configured foreground"
    );
    assert_eq!(
        terminal.default_background(),
        background,
        "OSC 111 restores the configured background"
    );
}

fn assert_host_color_mutation_marks_damage(
    setup: impl FnOnce(&mut Terminal),
    mutate: impl FnOnce(&mut Terminal),
) {
    let mut term = Terminal::new(2, 8);
    setup(&mut term);
    term.take_damage();
    let before = term.damage_epoch();
    assert!(!term.has_damage());

    mutate(&mut term);

    assert!(
        term.has_damage(),
        "host color mutation must repaint even with no glyph write"
    );
    assert!(
        term.damage_epoch() > before,
        "host color mutation must advance the frontend redraw key"
    );
}

#[test]
fn public_color_mutators_mark_full_damage_without_a_glyph_write() {
    let noop = |_: &mut Terminal| {};
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_palette_color(1, Rgb::new(0x11, 0x22, 0x33));
    });
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_default_foreground(Rgb::new(0x11, 0x22, 0x33));
    });
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_default_background(Rgb::new(0x44, 0x55, 0x66));
    });
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_default_cursor_color(Some(Rgb::new(0x77, 0x88, 0x99)));
    });
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_default_selection_background(Some(Rgb::new(0x11, 0x44, 0x77)));
    });
    assert_host_color_mutation_marks_damage(noop, |term| {
        term.set_default_selection_foreground(Some(Rgb::new(0x99, 0x66, 0x33)));
    });
}

#[test]
fn public_default_setters_seed_osc_reset_baselines() {
    let mut term = Terminal::new(2, 8);
    let foreground = Rgb::new(0x11, 0x22, 0x33);
    let background = Rgb::new(0x44, 0x55, 0x66);
    let cursor = Rgb::new(0x77, 0x88, 0x99);
    let selection_bg = Rgb::new(0x12, 0x45, 0x78);
    let selection_fg = Rgb::new(0x98, 0x76, 0x54);
    term.set_default_foreground(foreground);
    term.set_default_background(background);
    term.set_default_cursor_color(Some(cursor));
    term.set_default_selection_background(Some(selection_bg));
    term.set_default_selection_foreground(Some(selection_fg));

    term.process(
        b"\x1b]10;rgb:aa/bb/cc\x07\x1b]11;rgb:77/88/99\x07\
          \x1b]12;rgb:dd/ee/ff\x07\x1b]17;rgb:01/02/03\x07\
          \x1b]19;rgb:04/05/06\x07",
    );
    assert_ne!(term.default_foreground(), foreground);
    assert_ne!(term.default_background(), background);
    assert_ne!(term.cursor_color(), Some(cursor));
    assert_ne!(term.selection_background(), Some(selection_bg));
    assert_ne!(term.selection_foreground(), Some(selection_fg));

    term.process(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07\x1b]119\x07");
    assert_eq!(term.default_foreground(), foreground);
    assert_eq!(term.default_background(), background);
    assert_eq!(
        term.cursor_color(),
        Some(cursor),
        "OSC 112 restores the configured host cursor"
    );
    assert_eq!(term.selection_background(), Some(selection_bg));
    assert_eq!(term.selection_foreground(), Some(selection_fg));
}

#[test]
fn public_palette_setter_seeds_osc_and_ris_reset_baseline() {
    let mut term = Terminal::new(2, 8);
    term.set_allow_palette_reconfigure(true);
    let configured = Rgb::new(0x11, 0x22, 0x33);
    let transient = Rgb::new(0xaa, 0xbb, 0xcc);
    term.set_palette_color(1, configured);

    term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07");
    assert_eq!(term.palette_color(1), transient);
    term.process(b"\x1b]104;1\x07");
    assert_eq!(
        term.palette_color(1),
        configured,
        "OSC 104 restores the host-configured palette slot"
    );

    term.process(b"\x1b]4;1;rgb:aa/bb/cc\x07");
    term.process(b"\x1bc");
    assert_eq!(
        term.palette_color(1),
        configured,
        "RIS restores the same host-configured palette baseline"
    );
}
