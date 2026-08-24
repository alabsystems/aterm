// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LINUX SUBPIXEL stage 1: the `font_subpixel` config key's renderer seam
//! ([`Renderer::set_font_subpixel`]) — the live-settable twin of the
//! construction-time `ATERM_FONT_SUBPIXEL` read, mirroring the
//! `font_hinting_config` suite.
//!
//! Laws under test, on the Linux subpixel seam:
//! * the DEFAULT is `off` and the getter round-trips every canonical spelling;
//! * a same-value set is a free `false`, a real change is `true`;
//! * OFF renders NO chroma on an all-grey theme (every pixel R == G == B —
//!   grayscale coverage of grey ink over grey ground cannot make colour), and
//!   the flag defaulting OFF means the shipped pixels are untouched;
//! * ON renders chroma-fringed stems (some pixel R != B): the actual win the
//!   RFC's stage 1 exists to judge;
//! * the OPAQUE-FRAME gate: `background_opacity < 1` falls back to grayscale
//!   for the whole frame even with the mode on;
//! * flips are reversible byte-for-byte (the overlay never contaminates the
//!   shared grayscale store).
//!
//! On targets without the seam the setter is inert `false` and the getter pins
//! `"off"` — asserted so the cross-platform contract is machine-checked
//! everywhere the suite runs (macOS/Windows byte-identical is the lane law).

#![cfg(feature = "embedded-font")]

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

const LINUX_SEAM: bool = cfg!(all(unix, not(target_os = "macos")));

/// All-grey theme: greyscale rendering of it is monochrome BY CONSTRUCTION,
/// so any R != B pixel in a frame is subpixel chroma and nothing else.
fn grey_theme() -> Theme {
    Theme {
        fg: 0x00FF_FFFF,
        bg: 0x0000_0000,
        cursor: 0x0080_8080,
        selection: 0x0040_4040,
    }
}

fn renderer() -> Renderer {
    Renderer::from_bytes(aterm_render::embedded_font(), 15.0, grey_theme())
        .expect("embedded DejaVu parses")
}

fn render_stems(r: &mut Renderer) -> aterm_render::Frame {
    let mut term = Terminal::new(2, 10);
    term.process(b"milk illl");
    let input = term.cell_frame(2, 10);
    r.render_input(&input)
}

fn chroma_pixels(f: &aterm_render::Frame) -> usize {
    f.pixels
        .iter()
        .filter(|&&p| {
            let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
            r != g || g != b
        })
        .count()
}

#[test]
fn default_is_off_and_spellings_round_trip() {
    let mut r = renderer();
    assert_eq!(r.font_subpixel(), "off", "the shipped default");
    for (spelling, canonical) in [
        ("rgb", "rgb"),
        ("on", "rgb"),
        ("1", "rgb"),
        ("true", "rgb"),
        ("bgr", "bgr"),
        ("off", "off"),
        ("0", "off"),
        ("none", "off"),
        ("false", "off"),
        ("anything-else", "off"), // forgiving toward the DEFAULT, like the env read
    ] {
        r.set_font_subpixel(spelling);
        let expect = if LINUX_SEAM { canonical } else { "off" };
        assert_eq!(r.font_subpixel(), expect, "spelling {spelling:?}");
    }
}

#[test]
fn same_value_is_free_change_reports_true() {
    let mut r = renderer();
    assert!(
        !r.set_font_subpixel("off"),
        "same-value set is a free no-op"
    );
    let changed = r.set_font_subpixel("rgb");
    assert_eq!(
        changed, LINUX_SEAM,
        "a real change reports on the seam only"
    );
    assert!(!r.set_font_subpixel("rgb"), "and is then idempotent");
}

#[test]
fn subpixel_fringes_appear_only_when_on_and_opaque() {
    let mut r = renderer();
    let cell = r.cell_size();

    // OFF (the default): an all-grey theme renders zero chroma.
    let off = render_stems(&mut r);
    assert_eq!(
        chroma_pixels(&off),
        0,
        "grayscale rendering of a grey theme must be monochrome"
    );

    // ON: stems grow the coloured fringes (the whole point), and cell
    // geometry never moves (the raster changes coverage, not metrics).
    r.set_font_subpixel("rgb");
    assert_eq!(r.cell_size(), cell, "subpixel never moves the cell box");
    let on = render_stems(&mut r);
    if LINUX_SEAM {
        assert!(
            chroma_pixels(&on) > 0,
            "subpixel-on must chroma-fringe stem edges (non-vacuity)"
        );
    } else {
        assert_eq!(
            on.pixels, off.pixels,
            "no subpixel seam: the setter must be pixel-inert"
        );
    }

    // The OPAQUE-FRAME gate: translucency falls back to grayscale wholesale.
    r.set_background_opacity(0.9);
    let translucent = render_stems(&mut r);
    assert_eq!(
        chroma_pixels(&translucent),
        0,
        "a translucent frame must render pure grayscale even with the mode on"
    );
    r.set_background_opacity(1.0);

    // And back OFF: byte-identical to the first frame — the overlay cache
    // never contaminates the shared grayscale store.
    r.set_font_subpixel("off");
    let off_again = render_stems(&mut r);
    assert_eq!(off.pixels, off_again.pixels, "mode flips are reversible");
}
