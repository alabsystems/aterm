// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPOGRAPHY R2: the `font_hinting` config key's renderer seam
//! ([`Renderer::set_font_hinting`]) — the live-settable twin of the
//! construction-time `ATERM_FONT_HINTING` read (W13).
//!
//! Laws under test, on the native hint seam (Linux and — since the grid-fit
//! wave — Windows):
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
//! On targets without the seam (macOS CoreText, wasm fontdue) the setter is
//! inert `false` and the getter reports `"off"` — HONESTLY, rather than the
//! `"full"` it used to claim on every platform whether or not a single glyph
//! was grid-fitted. Asserted here too, so the cross-platform contract is
//! machine-checked everywhere the suite runs.

#![cfg(feature = "embedded-font")]

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

const HINT_SEAM: bool = cfg!(any(all(unix, not(target_os = "macos")), windows));

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
    assert_eq!(
        r.font_hinting(),
        if HINT_SEAM { "full" } else { "off" },
        "the shipped default on the seam; an honest `off` without one",
    );
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
        let expect = if HINT_SEAM { canonical } else { "off" };
        assert_eq!(r.font_hinting(), expect, "spelling {spelling:?}");
    }
}

#[test]
fn same_value_is_free_change_reports_true() {
    let mut r = renderer();
    assert!(
        !r.set_font_hinting("full"),
        "same-value set is a free no-op"
    );
    let changed = r.set_font_hinting("light");
    assert_eq!(changed, HINT_SEAM, "a real change reports on the seam only");
    assert!(!r.set_font_hinting("light"), "and is then idempotent");
}

/// A live-set mode RIDES the font-generation handoffs: the semantic-surface
/// fork (Settings specimens, Markdown) and the sealed rebuild must render with
/// the parent's `font_hinting`, not resurrect the env-resolved default — a
/// `font_hinting = "off"` user must not meet re-hinted text in Settings. On
/// targets without the seam both getters answer `"off"` and the assertions are
/// the honesty contract itself.
#[test]
fn live_mode_rides_semantic_forks_and_rebuilds() {
    let mut r = renderer();
    r.set_font_hinting("off");
    let fork = r
        .fork_semantic_surface(12.0, Theme::default())
        .expect("unsealed fork builds");
    assert_eq!(
        fork.font_hinting(),
        "off",
        "the unsealed fork must carry the live mode"
    );
    let _ = r.seal_admitted_font_sources();
    let rebuilt = r
        .fork_semantic_surface(12.0, Theme::default())
        .expect("sealed fork (rebuild_from_admitted) builds");
    assert_eq!(
        rebuilt.font_hinting(),
        "off",
        "the sealed rebuild must carry the live mode"
    );
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
    if HINT_SEAM {
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
