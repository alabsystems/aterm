// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `font_hinting` renderer seam ([`Renderer::set_font_hinting`]).

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
        ("native", "light"), // retired; see `native_renders_as_light`
        ("off", "off"),
        ("none", "off"),
        ("0", "off"),
        ("false", "off"),
        ("full", "full"),
        ("anything-else", "full"), // forgiving toward the DEFAULT
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
    r.set_font_hinting("full");
    let full_again = render_stems(&mut r);
    assert_eq!(full.pixels, full_again.pixels, "mode flips are reversible");
}

#[test]
fn native_renders_as_light() {
    let mut r = renderer();
    r.set_font_hinting("light");
    let light = render_stems(&mut r);
    assert!(
        !r.set_font_hinting("native"),
        "native over light changes nothing, so drops no atlas"
    );
    assert_eq!(r.font_hinting(), if HINT_SEAM { "light" } else { "off" });
    assert_eq!(render_stems(&mut r).pixels, light.pixels);

    r.set_font_hinting("full");
    let full = render_stems(&mut r);
    assert_eq!(
        r.set_font_hinting("native"),
        HINT_SEAM,
        "native over full is a real change on the seam"
    );
    assert_eq!(render_stems(&mut r).pixels, light.pixels);
    if HINT_SEAM {
        assert_ne!(
            full.pixels, light.pixels,
            "full and light must paint the row differently (the control)"
        );
    }
}
