// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPOGRAPHY R2: the `font_hinting` config key's renderer seam
//! ([`Renderer::set_font_hinting`]) — the live-settable twin of the
//! construction-time `ATERM_FONT_HINTING` read (W13).
//!
//! Laws under test, on the Linux hint seam:
//! * the DEFAULT is `full` and the getter round-trips every canonical spelling;
//! * a same-value set is a free `false` (no atlas invalidation), a real change
//!   is `true`;
//! * a mode change actually changes rasterized COVERAGE (non-vacuity: `full`
//!   vs `off` differ at the desktop 12px) while cell GEOMETRY stays fixed
//!   (the hinted-seam contract: advances stay linear, so the grid never
//!   moves);
//! * an unrecognized spelling resolves to the default (`full`) — the same
//!   forgiving shape the env always had.
//!
//! On targets without the seam (macOS CoreText, Windows/wasm fontdue) the
//! setter is inert `false` and the getter pins `"full"` — asserted here too,
//! so the cross-platform contract is machine-checked everywhere the suite
//! runs.

#![cfg(feature = "embedded-font")]

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

const LINUX_SEAM: bool = cfg!(all(unix, not(target_os = "macos")));

fn renderer() -> Renderer {
    Renderer::from_bytes(aterm_render::embedded_font(), 12.0, Theme::default())
        .expect("embedded DejaVu parses")
}

fn render_stems(r: &mut Renderer) -> aterm_render::Frame {
    let mut term = Terminal::new(2, 10);
    term.process(b"milk illl");
    let input = term.cell_frame(2, 10);
    r.render_input(&input)
}

#[test]
fn default_is_full_and_spellings_round_trip() {
    let mut r = renderer();
    assert_eq!(r.font_hinting(), "full", "the shipped default");
    for (spelling, canonical) in [
        ("light", "light"),
        ("native", "native"),
        ("off", "off"),
        ("none", "off"),
        ("0", "off"),
        ("false", "off"),
        ("full", "full"),
        ("anything-else", "full"), // forgiving, like the env read
    ] {
        r.set_font_hinting(spelling);
        let expect = if LINUX_SEAM { canonical } else { "full" };
        assert_eq!(r.font_hinting(), expect, "spelling {spelling:?}");
    }
}

#[test]
fn same_value_is_free_change_reports_true() {
    let mut r = renderer();
    assert!(!r.set_font_hinting("full"), "same-value set is a free no-op");
    let changed = r.set_font_hinting("light");
    assert_eq!(changed, LINUX_SEAM, "a real change reports on the seam only");
    assert!(!r.set_font_hinting("light"), "and is then idempotent");
}

#[test]
fn mode_change_moves_coverage_but_never_geometry() {
    let mut r = renderer();
    let cell = r.cell_size();
    let full = render_stems(&mut r);
    r.set_font_hinting("off");
    assert_eq!(r.cell_size(), cell, "hinting never moves the cell box");
    let off = render_stems(&mut r);
    assert_eq!(full.pixels.len(), off.pixels.len());
    if LINUX_SEAM {
        assert_ne!(
            full.pixels, off.pixels,
            "full vs off must rasterize differently at 12px (non-vacuity)"
        );
    } else {
        assert_eq!(
            full.pixels, off.pixels,
            "no hint seam: the setter must be pixel-inert"
        );
    }
    // And back: the glyph caches re-rasterize on the restored mode, so the
    // original bytes return exactly (determinism of the hinted raster).
    r.set_font_hinting("full");
    let full_again = render_stems(&mut r);
    assert_eq!(full.pixels, full_again.pixels, "mode flips are reversible");
}
