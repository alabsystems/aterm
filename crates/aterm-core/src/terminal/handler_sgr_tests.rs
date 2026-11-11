// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! conhost does not forward the semicolon form (its `_ApplyGraphicsOption`
//! has no `UnderlineColor` arm, so `58;5;196` arrives as separate SGRs); under
//! ConPTY only the colon forms `58:2::r:g:b` and `58:5:n` pass through.
use super::super::Terminal;
use super::super::render_cells::UnderlineStyle;
use super::*;

/// `0x01_RRGGBB`: the packed form `parse_underline_color` stores for RGB.
const RED: u32 = 0x01_FF00_00;
const GREEN: u32 = 0x01_00FF_00;
/// `0x02_0000NN`: the packed form for a palette index.
const IDX_196: u32 = 0x02_0000_C4;

type Drawn = Vec<(char, UnderlineStyle, Option<[u8; 3]>)>;

/// `(char, underline style, resolved underline colour)` for the first `n` cells of row 0.
fn drawn(term: &mut Terminal, n: usize) -> Drawn {
    term.render_row(0)
        .iter()
        .take(n)
        .map(|c| (c.ch, c.underline, c.underline_color))
        .collect()
}

fn single(text: &str, color: [u8; 3]) -> Drawn {
    text.chars()
        .map(|ch| (ch, UnderlineStyle::Single, Some(color)))
        .collect()
}

#[test]
fn semicolon_indexed_after_sgr_4_colours_the_underline_from_the_palette() {
    let mut term = Terminal::new(2, 16);
    term.process(b"\x1b[4m\x1b[58;5;196m");
    assert_eq!(
        term.transient.current_underline_color,
        Some(IDX_196),
        "58;5;196 is one palette index"
    );
    assert!(term.style.flags.contains(CellFlags::UNDERLINE));
    assert!(
        !term.style.flags.contains(CellFlags::BLINK),
        "5 is the colour form, not SGR 5"
    );
    term.process(b"TEXT\x1b[0m");
    let c = term.color_palette().get(196);
    assert_eq!(drawn(&mut term, 4), single("TEXT", [c.r, c.g, c.b]));
}

#[test]
fn semicolon_rgb_with_zero_components_keeps_the_underline_on() {
    let mut term = Terminal::new(2, 16);
    term.process(b"\x1b[4m\x1b[58;2;0;255;0m");
    assert_eq!(term.transient.current_underline_color, Some(GREEN));
    assert!(
        term.style.flags.contains(CellFlags::UNDERLINE),
        "the underline must survive its own colour"
    );
    assert!(
        !term.style.flags.contains(CellFlags::DIM),
        "2 is the colour space, not SGR 2"
    );
    assert!(
        term.style.fg == PackedColor::DEFAULT_FG && term.style.bg == PackedColor::DEFAULT_BG,
        "no component of the triple is a reset"
    );
    term.process(b"TEXT\x1b[0m");
    assert_eq!(drawn(&mut term, 4), single("TEXT", [0, 255, 0]));
}

#[test]
fn semicolon_colour_split_at_every_read_boundary_is_one_colour() {
    for (bytes, want) in [
        (&b"\x1b[4m\x1b[58;2;0;255;0mTEXT\x1b[0m"[..], [0, 255, 0]),
        (&b"\x1b[4;58;2;255;0;0mTEXT\x1b[0m"[..], [255, 0, 0]),
    ] {
        let mut term = Terminal::new(2, 16);
        for b in bytes {
            term.process(std::slice::from_ref(b));
        }
        assert_eq!(
            drawn(&mut term, 4),
            single("TEXT", want),
            "{:?} fed one byte at a time",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn colon_semicolon_and_combined_forms_draw_the_same_cells() {
    let mut semicolon = Terminal::new(2, 16);
    semicolon.process(b"\x1b[4m\x1b[58;2;255;0;0mTEXT\x1b[0m");
    let mut colon = Terminal::new(2, 16);
    colon.process(b"\x1b[58:2::255:0:0m\x1b[4mTEXT\x1b[0m");
    let mut combined = Terminal::new(2, 16);
    combined.process(b"\x1b[4;58;2;255;0;0mTEXT\x1b[0m");
    let want = single("TEXT", [255, 0, 0]);
    assert_eq!(drawn(&mut semicolon, 4), want, "58;2;r;g;b");
    assert_eq!(drawn(&mut colon, 4), want, "58:2::r:g:b");
    assert_eq!(drawn(&mut combined, 4), want, "4;58;2;r;g;b");
}

#[test]
fn sgr_59_clears_the_colour_but_not_the_underline() {
    let mut term = Terminal::new(2, 16);
    term.process(b"\x1b[4m\x1b[58;2;255;0;0mA\x1b[59m");
    assert_eq!(
        term.transient.current_underline_color, None,
        "59 resets the colour"
    );
    assert!(
        term.style.flags.contains(CellFlags::UNDERLINE),
        "59 leaves the underline itself alone"
    );
    term.process(b"B\x1b[0m");
    assert_eq!(
        drawn(&mut term, 2),
        vec![
            ('A', UnderlineStyle::Single, Some([255, 0, 0])),
            ('B', UnderlineStyle::Single, None),
        ]
    );
}

#[test]
fn a_param_after_the_semicolon_colour_is_applied_on_its_own() {
    let mut term = Terminal::new(2, 16);
    term.process(b"\x1b[58;2;255;0;0;1m");
    assert_eq!(term.transient.current_underline_color, Some(RED));
    assert!(term.style.flags.contains(CellFlags::BOLD), "58;2;r;g;b;1");
    term.process(b"\x1b[0m\x1b[58;5;196;3m");
    assert_eq!(term.transient.current_underline_color, Some(IDX_196));
    assert!(term.style.flags.contains(CellFlags::ITALIC), "58;5;n;3");
    term.process(b"\x1b[0m\x1b[38;2;1;2;3;4m");
    assert!(term.style.fg == PackedColor::rgb(1, 2, 3), "38;2;r;g;b");
    assert!(
        term.style.flags.contains(CellFlags::UNDERLINE),
        "38;2;r;g;b;4"
    );
    term.process(b"\x1b[0m\x1b[48;5;7;9m");
    assert!(term.style.bg == PackedColor::indexed(7), "48;5;n");
    assert!(
        term.style.flags.contains(CellFlags::STRIKETHROUGH),
        "48;5;n;9"
    );
    term.process(b"\x1b[0m\x1b[4;58;2;255;0;0;39m");
    assert_eq!(term.transient.current_underline_color, Some(RED));
    assert!(term.style.fg == PackedColor::DEFAULT_FG, "4;58;2;r;g;b;39");
}

/// A colour the engine refuses still consumes what xterm's
/// `parse_extended_colors` consumes, CMY and CMYK aside (their own test below): the selector,
/// and the values it needs, present or not. Found by the astream-term differential oracle
/// (aterm-conformance `tests/vt_oracle`): a truncated `38;5` turned on
/// blink, because the refused colour returned its tail to the SGR loop.
#[test]
fn a_refused_extended_colour_consumes_what_xterm_consumes() {
    let mut term = Terminal::new(2, 16);
    for (seq, what) in [
        (&b"\x1b[31;44m\x1b[38;5m"[..], "38;5 with no index"),
        (b"\x1b[31;44m\x1b[48;5m", "48;5 with no index"),
        (b"\x1b[31;44m\x1b[58;5m", "58;5 with no index"),
        (b"\x1b[31;44m\x1b[38;2;1;2m", "38;2 one value short"),
        (b"\x1b[31;44m\x1b[48;2m", "48;2 with no values"),
    ] {
        term.process(b"\x1b[0m");
        term.process(seq);
        assert_eq!(
            term.style.flags,
            CellFlags::empty(),
            "{what}: no stray blink, faint or bold"
        );
        assert!(term.style.fg == PackedColor::indexed(1), "{what}: fg kept");
        assert!(term.style.bg == PackedColor::indexed(4), "{what}: bg kept");
        assert_eq!(term.transient.current_underline_color, None, "{what}");
    }
    term.process(b"\x1b[0m\x1b[38;7;1m");
    assert_eq!(term.style.flags, CellFlags::BOLD, "38;7;1");
    term.process(b"\x1b[0m\x1b[4;38m");
    assert_eq!(term.style.flags, CellFlags::UNDERLINE, "4;38");
    assert!(term.style.fg == PackedColor::DEFAULT_FG, "4;38");
}

/// The id is an id wherever the first colon falls, and three values are the channels: at the
/// end of the list too, where a trailing colon is an empty blue, and with the parameter cap
/// cutting the group. `tests/vt_oracle/colours.rs` holds every spelling to written answers on
/// both engines.
#[test]
fn a_colour_space_id_is_never_a_channel_or_a_code() {
    let mut term = Terminal::new(2, 16);
    for seq in [
        "38;2:3:10:20:",
        "38:2:3:10:20:",
        "38;2::10:20:",
        "38:2::10:20:",
    ] {
        term.process(format!("\x1b[0m\x1b[{seq}m").as_bytes());
        assert!(term.style.fg == PackedColor::rgb(10, 20, 0), "{seq}");
    }
    let cap = "7;".repeat(19);
    for seq in ["38:2:3:10:20:9", "38;2:3:10:20:9", "38:2::10:20:9"] {
        term.process(format!("\x1b[0m\x1b[{cap}{seq}m").as_bytes());
        assert!(term.style.fg == PackedColor::DEFAULT_FG, "{seq} at the cap");
    }
    term.process(format!("\x1b[0m\x1b[{cap}38:2:10:20:9m").as_bytes());
    assert!(
        term.style.fg == PackedColor::rgb(10, 20, 9),
        "three channels ending at the cap are whole"
    );

    for seq in [
        "38;2:3:10:20:9",
        "38:2:3:10:20:9",
        "38;2::10:20:9",
        "38:2::10:20:9",
        "38;2:10:20:9",
        "38:2:10:20:9",
        "38;2;10;20;9",
    ] {
        term.process(format!("\x1b[0m\x1b[{seq};1m").as_bytes());
        assert!(term.style.fg == PackedColor::rgb(10, 20, 9), "{seq}");
        assert_eq!(term.style.flags, CellFlags::BOLD, "{seq}");
    }
    term.process(b"\x1b[0m\x1b[58;2:3:10:20:9;1m");
    assert_eq!(term.transient.current_underline_color, Some(0x010A_1409));
    assert_eq!(term.style.flags, CellFlags::BOLD, "58;2:3:10:20:9");
}

/// CMY and CMYK render nowhere: no colour moves and no value lands as a code, in any spelling,
/// and the attribute after them applies.
#[test]
fn cmy_and_cmyk_consume_their_values_and_change_nothing() {
    let mut term = Terminal::new(2, 16);
    for seq in [
        "38;3;4;5;9",
        "48;4;4;5;9;7",
        "58;3;4;5;9",
        "38:3:3:4:5:9",
        "48:4:3:4:5:9:7",
        "38;3:3:4:5:9",
        "48;4:3:4:5:9:7",
        "58;4::4:5:9:7",
    ] {
        term.process(format!("\x1b[0;31;44m\x1b[{seq};1m").as_bytes());
        assert!(term.style.fg == PackedColor::indexed(1), "{seq}");
        assert!(term.style.bg == PackedColor::indexed(4), "{seq}");
        assert_eq!(term.transient.current_underline_color, None, "{seq}");
        assert_eq!(term.style.flags, CellFlags::BOLD, "{seq}");
    }
}
