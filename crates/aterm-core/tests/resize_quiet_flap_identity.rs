// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression net for the alternate screen's resize undo AND for 24-bit colour
//! across a resize (R14): a flap with nothing written between its halves puts
//! every row back as it was drawn, cell for cell.
//!
//! The samples cover what a full-screen program paints: plain text, wide CJK,
//! ZWJ emoji, combining marks, SGR colour and weight, curly underline with an
//! underline colour, an OSC 8 link, a truecolor background, and blank rows. The
//! comparison is the renderer's own row (`render_row`: text, foreground, background
//! and attributes per cell), plus the links and the cursor, so a colour the
//! resize drops or a row it shifts cannot pass. Deterministic seed; the screens
//! are random, the test is not.

use aterm_core::terminal::Terminal;

const COLS: u16 = 40;

fn next(s: &mut u64) -> u32 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*s >> 33) as u32
}

const SAMPLES: &[&str] = &[
    "the quick brown fox jumps over the lazy dog",
    "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8} wide",
    "\u{1f680}\u{1f600}\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} zwj",
    "e\u{0301}n\u{0303}o\u{0308}a\u{030a} comb",
    "\x1b[31mred\x1b[1mbold\x1b[0m plain",
    "\x1b[4:3m\x1b[58;2;255;0;0mcurly\x1b[0m under",
    "\x1b]8;;https://example.com/x\x07link text\x1b]8;;\x07 after",
    "\x1b[48;2;10;20;30mrgb bg\x1b[0m",
    "\x1b[38;2;200;100;50;48;2;1;2;3mrgb fg and bg\x1b[0m tail",
    "",
];

/// A full alternate screen of random samples, the cursor parked on a random row.
fn paint(s: &mut u64, t: &mut Terminal, rows: u16) {
    t.process(b"\x1b[?1049h\x1b[2J");
    for r in 0..rows {
        t.process(format!("\x1b[{};1H", r + 1).as_bytes());
        let pick = SAMPLES[(next(s) as usize) % SAMPLES.len()];
        t.process(pick.as_bytes());
    }
    let row = 1 + next(s) % u32::from(rows);
    let col = 1 + next(s) % 20;
    t.process(format!("\x1b[{row};{col}H").as_bytes());
}

/// Everything a flap may not change, as text a failure can print.
fn picture(t: &Terminal) -> Vec<String> {
    let mut out: Vec<String> = (0..usize::from(t.rows()))
        .map(|r| format!("{:?}", t.render_row(r)))
        .collect();
    for r in 0..t.rows() {
        for c in 0..t.cols() {
            if let Some(l) = t.hyperlink_at_visible(r, c) {
                out.push(format!("link {r},{c} {l}"));
            }
        }
    }
    let cur = t.cursor();
    out.push(format!("cursor {},{}", cur.row, cur.col));
    out
}

fn first_difference(a: &[String], b: &[String]) -> String {
    a.iter().zip(b).position(|(x, y)| x != y).map_or_else(
        || format!("lengths {} vs {}", a.len(), b.len()),
        |i| {
            let short = |x: &str| x.chars().take(160).collect::<String>();
            format!(
                "entry {i}: before {} / after {}",
                short(&a[i]),
                short(&b[i])
            )
        },
    )
}

#[test]
fn a_quiet_flap_is_a_styled_identity() {
    let mut seed = 0x57_71_1E_D5_u64;
    for i in 0..2500 {
        let rows = 8 + (next(&mut seed) % 30) as u16;

        // One flap of 1..=9 rows.
        let mut t = Terminal::new(rows, COLS);
        paint(&mut seed, &mut t, rows);
        let before = picture(&t);
        let k = 1 + (next(&mut seed) % u32::from(rows.min(10) - 1)) as u16;
        t.resize(rows - k, COLS);
        t.resize(rows, COLS);
        let after = picture(&t);
        assert!(
            after == before,
            "screen {i} (rows {rows}, flap of {k}): {}",
            first_difference(&before, &after)
        );

        // Two shrinks and one grow back.
        let mut t = Terminal::new(rows, COLS);
        paint(&mut seed, &mut t, rows);
        let before = picture(&t);
        let k1 = 1 + (next(&mut seed) % 3) as u16;
        let k2 = 1 + (next(&mut seed) % 3) as u16;
        t.resize(rows - k1, COLS);
        t.resize(rows - k1 - k2, COLS);
        t.resize(rows, COLS);
        let after = picture(&t);
        assert!(
            after == before,
            "screen {i} (rows {rows}, shrinks {k1}+{k2}): {}",
            first_difference(&before, &after)
        );
    }
}

/// The point of the harvest, in the renderer's own words: a truecolor colour on
/// screen is the same after a same-size resize, a flap the engine undoes, a
/// shrink nothing undoes (the coloured rows are found where they moved) and an
/// alternate-screen width change.
#[test]
fn resize_keeps_truecolor_in_render_row() {
    for (name, sizes) in [
        ("same size", vec![(12, COLS)]),
        ("height only and back", vec![(10, COLS), (12, COLS)]),
        ("height only, never back", vec![(11, COLS)]),
        ("narrower, alternate screen", vec![(12, COLS - 5)]),
    ] {
        let mut t = Terminal::new(12, COLS);
        t.process(b"\x1b[?1049h\x1b[2J\x1b[1;1H\x1b[48;2;10;20;30mRGBROW\x1b[0m tail");
        t.process(b"\x1b[3;1H\x1b[38;2;200;100;50mfg row\x1b[0m");
        // Park the cursor on the bottom row: a shrink demotes the top rows.
        t.process(b"\x1b[12;1H");
        assert_eq!(t.render_row(0)[0].bg, [10, 20, 30], "{name}: fixture bg");
        let fg = t.render_row(2)[0].fg;
        assert_eq!(fg, [200, 100, 50], "{name}: fixture fg");
        for (rows, cols) in sizes {
            t.resize(rows, cols);
        }
        let row_of = |t: &Terminal, want: &str| {
            (0..usize::from(t.rows())).find(|&r| t.row_text(r).is_some_and(|x| x.starts_with(want)))
        };
        if let Some(r) = row_of(&t, "RGBROW") {
            assert_eq!(t.render_row(r)[0].bg, [10, 20, 30], "{name}: background");
        } else {
            // Demoted off the top of a shrink nothing undid: gone with its row.
            assert!(name.ends_with("never back"), "{name}: the bg row vanished");
        }
        let r = row_of(&t, "fg row").unwrap_or_else(|| panic!("{name}: fg row vanished"));
        assert_eq!(t.render_row(r)[0].fg, fg, "{name}: foreground");
    }
}
