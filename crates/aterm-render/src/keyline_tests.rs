// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::contrast_ratio;

/// The six film themes' pages: Dark, GitHub Light, Default Light, Dim Grey, Solarized Dark and
/// Paper Grey.
const PAGES: [u32; 6] = [
    0x0011_1318,
    0x00FF_FFFF,
    0x00F6_F7F9,
    0x0011_1318,
    0x0000_2B36,
    0x00FF_FFFF,
];

/// Halo tints the rainbow block's innermost ring carries: the films' (GitHub Light's burst
/// `0x131b13`, Dark's typing `0x090b06`), a strong one, and none.
const TINTS: [u32; 4] = [0x0013_1B13, 0x0009_0B06, 0x0040_2060, 0];

/// Fills a block may wear on `page`: a hue sweep at many lightnesses, each that reads at least
/// the caret's floor against the page.
fn fills_on(page: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for h in 0..24u32 {
        for v in 1..=16u32 {
            let (r, g, b) = hue_rgb(h as f32 / 24.0);
            let k = v as f32 / 16.0;
            let c = |x: f32| ((x * k * 255.0).round() as u32).min(255);
            let fill = (c(r) << 16) | (c(g) << 8) | c(b);
            if contrast_ratio(fill, page) >= CARET_PAGE_MIN_CONTRAST {
                out.push(fill);
            }
            // The same hue toward white, for a dark page's light fills.
            let w = |x: f32| (((x + (1.0 - x) * k) * 255.0).round() as u32).min(255);
            let pale = (w(r) << 16) | (w(g) << 8) | w(b);
            if contrast_ratio(pale, page) >= CARET_PAGE_MIN_CONTRAST {
                out.push(pale);
            }
        }
    }
    out
}

fn hue_rgb(h: f32) -> (f32, f32, f32) {
    let x = h * 6.0;
    let f = x - x.floor();
    match x as u32 % 6 {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    }
}

/// Every keyline on every film page reads the caret's floor against the page and
/// [`KEYLINE_FILL_MIN_CONTRAST`] against its fill, for any fill that reads the floor itself.
/// The control is the tint over the fill alone ([`with_keyline_as_tinted`]): lighter toward a
/// light page, under the floor there, and lost on a dark page's light block.
#[test]
fn the_keyline_reads_against_the_page_and_the_fill() {
    let judge = || {
        let (mut page_worst, mut fill_worst) = ((f32::MAX, 0, 0, 0), (f32::MAX, 0, 0, 0));
        let mut n = 0usize;
        for page in PAGES {
            for fill in fills_on(page) {
                for tint in TINTS {
                    let line = block_keyline_colour(fill, tint, page);
                    let (p, f) = (contrast_ratio(line, page), contrast_ratio(line, fill));
                    if p < page_worst.0 {
                        page_worst = (p, page, fill, tint);
                    }
                    if f < fill_worst.0 {
                        fill_worst = (f, page, fill, tint);
                    }
                    n += 1;
                }
            }
        }
        (n, page_worst, fill_worst)
    };
    let (n, page_worst, fill_worst) = judge();
    eprintln!("{n} keylines: worst on the page {page_worst:x?}, on the fill {fill_worst:x?}");
    assert!(n > 1_000, "fixture: {n} keylines judged");
    assert!(
        page_worst.0 >= CARET_PAGE_MIN_CONTRAST,
        "a keyline reads {:.2}:1 on page {:06x} (fill {:06x}, tint {:06x})",
        page_worst.0,
        page_worst.1,
        page_worst.2,
        page_worst.3
    );
    assert!(
        fill_worst.0 >= KEYLINE_FILL_MIN_CONTRAST,
        "a keyline reads {:.2}:1 on its fill {:06x} (page {:06x}, tint {:06x})",
        fill_worst.0,
        fill_worst.2,
        fill_worst.1,
        fill_worst.3
    );
    let (_, page_was, fill_was) = with_keyline_as_tinted(judge);
    eprintln!("control: worst on the page {page_was:x?}, on the fill {fill_was:x?}");
    assert!(
        page_was.0 < CARET_PAGE_MIN_CONTRAST && fill_was.0 < KEYLINE_FILL_MIN_CONTRAST,
        "control: the tint alone breaks both floors ({:.2}:1 on the page, {:.2}:1 on the fill)",
        page_was.0,
        fill_was.0
    );
    // The films' own: GitHub Light's burst keyline as first drawn (0x159f15 over 0x028402,
    // 3.50:1 on the page) and Dark's (0xfffad9 over 0xf6efd3, 1.09:1 on the fill).
    for (fill, tint, page) in [
        (0x0002_8402, 0x0013_1B13, 0x00FF_FFFF),
        (0x00F6_EFD3, 0x0009_0B06, 0x0011_1318),
    ] {
        let line = block_keyline_colour(fill, tint, page);
        eprintln!(
            "fill {fill:06x} on {page:06x}: keyline {line:06x}, {:.2}:1 on the page, {:.2}:1 on \
             the fill",
            contrast_ratio(line, page),
            contrast_ratio(line, fill)
        );
    }
}

/// A fill already under the caret's floor (a theme cursor, a pinned colour the renderer floors
/// to nothing) keeps a keyline no nearer the page than the fill.
#[test]
fn a_keyline_never_reads_nearer_the_page_than_its_fill() {
    for page in PAGES {
        for v in 0..=255u32 {
            let fill = (v << 16) | (v << 8) | v;
            for tint in TINTS {
                let line = block_keyline_colour(fill, tint, page);
                let floor = CARET_PAGE_MIN_CONTRAST.min(contrast_ratio(fill, page));
                assert!(
                    contrast_ratio(line, page) >= floor * 0.995,
                    "fill {fill:06x} on {page:06x}: keyline {line:06x} reads {:.2}:1 on the \
                     page, the fill {:.2}:1",
                    contrast_ratio(line, page),
                    contrast_ratio(fill, page)
                );
            }
        }
    }
}
