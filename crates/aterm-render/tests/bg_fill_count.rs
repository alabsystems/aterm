// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FRM-1 AS A COUNT: a repainted band is filled ONCE, not twice.
//!
//! # The hole this closes
//!
//! `tests/bg_base_elision.rs` is FRM-1's correctness gate and it is a good one —
//! it pins the four fixtures where a naive elision predicate breaks, byte for
//! byte. What it cannot do is notice the elision GOING AWAY. Painting a band's
//! base colour and then painting the identical colour over it again is
//! byte-identical output; every pixel assertion in that file stays green while
//! the frame pays for a whole extra framebuffer of stores. The measured win
//! (keystroke_echo 43.24 -> 41.33 us, scrolled_back_wheel 102.38 -> 96.65 us,
//! 7/7 rounds) would be handed back in silence.
//!
//! Only the frame's own bench could see it, and the bench is not in the merge
//! contract — `xtask gate perf` is a TIMING gate that needs release builds of
//! several harnesses and has not run green in a month. A COUNT has none of those
//! problems: it is exact, machine-independent, cannot flake under load, and
//! rides `cargo test` — hence `tools/verify.sh --fast` and the pre-push hook —
//! at zero marginal cost.
//!
//! WHAT A COUNT CANNOT CATCH: a constant-factor slowdown with the counts
//! unchanged. If `fill_rect` itself became 3x slower this file stays green. It
//! guards the STRUCTURE of the win — how many fills a frame emits — not its
//! cost.
//!
//! # The identity being pinned
//!
//! With no wallpaper live, every background run whose colour is the band's own
//! base is skipped, so
//!
//! ```text
//!     emitted + at_base == total
//! ```
//!
//! and, for the fixture to prove anything, `at_base > 0` — the redundant state
//! must actually be reached. A giveback (the `.filter(|_| run_bg != band_base)`
//! conjuncts deleted) reads `emitted == total` with `at_base` unchanged, and
//! fails here.
//!
//! The other side is pinned too: under a live WALLPAPER the band's base is
//! backdrop texels rather than one scalar, nothing may be elided, and the same
//! identity degenerates to `emitted == total` with `at_base == 0`. A predicate
//! that dropped the `!wallpaper` conjunct — punching the picture through a
//! selection band — moves this count before it moves a pixel anyone reports.

use aterm_core::render::SceneAtlas;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu, rgb_to_u32};

const ROWS: usize = 6;
const COLS: usize = 20;

/// A terminal whose LIVE default background is the renderer theme's — the state
/// the shipping app is always in, and the only state in which "the base clear
/// and a default cell are the same colour" is true at all. A `Terminal::new`
/// fixture publishes `default_bg = COLOR_UNSET` and renders VT-spec BLACK over a
/// theme clear, which is the black-backed-text arrangement the product does not
/// ship — and which reaches none of this.
fn themed_term() -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let bg = Theme::default().bg;
    term.process(
        format!(
            "\x1b]11;#{:02x}{:02x}{:02x}\x07",
            (bg >> 16) & 0xff,
            (bg >> 8) & 0xff,
            bg & 0xff
        )
        .as_bytes(),
    );
    term
}

#[test]
fn a_plain_frame_never_fills_a_band_with_the_colour_it_already_carries() {
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    term.process(b"hello");
    let input = term.cell_frame(ROWS, COLS);
    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);

    let (total, at_base) = r.last_bg_runs();
    let emitted = r.last_bg_fills();

    // REACH, first: without redundant runs in the frame there is nothing to
    // elide and the identity below is satisfied by doing nothing.
    assert!(total > 0, "the frame resolved no background run at all");
    assert!(
        at_base > 0,
        "no run in this frame carried the band's base colour ({at_base}/{total}) \
         — the fixture never reaches the state FRM-1 removes, so the count \
         identity would hold vacuously"
    );

    // THE CLAIM: every base-coloured run was SKIPPED.
    assert_eq!(
        emitted + at_base,
        total,
        "of {total} background run(s), {at_base} carried the band's base colour \
         and {emitted} fill(s) were emitted. FRM-1 says those two partition the \
         runs — a band is filled ONCE. `emitted == total` means every repainted \
         band is being filled twice with byte-identical colour again, which no \
         pixel assertion can see."
    );
}

#[test]
fn under_a_live_wallpaper_no_background_run_is_elided() {
    // The `!wallpaper` conjunct, from the cost side. Under a wallpaper the base
    // is backdrop texels, so `band_base` is `None`, nothing compares equal to
    // it, and every resolved run must be paid for.
    let theme_bg = Theme::default().bg;
    let theme = Theme {
        selection: theme_bg,
        ..Theme::default()
    };
    let Some(mut r) = Renderer::from_system(18.0, theme) else {
        eprintln!("SKIP: no system font");
        return;
    };
    let mut term = themed_term();
    term.process(b"selected text here");
    {
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 7, SelectionSide::Right);
        sel.complete_selection();
    }
    let (w, h) = r.frame_size(ROWS, COLS);
    let backdrop = [0x40u8, 0x20, 0x60];
    assert_ne!(
        rgb_to_u32(backdrop),
        theme_bg,
        "fixture: the backdrop must differ from the theme bg"
    );
    let mut rgba = vec![0u8; w * h * 4];
    for px in rgba.as_chunks_mut::<4>().0 {
        px[0] = backdrop[0];
        px[1] = backdrop[1];
        px[2] = backdrop[2];
        px[3] = 0xff;
    }
    let mut input = term.cell_frame(ROWS, COLS);
    input.wallpaper = Some(std::sync::Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    }));
    assert!(
        input.selection_contains_cell(0, 1, false, false),
        "fixture: the selection must actually reach the band being counted"
    );

    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &input);
    let (total, at_base) = r.last_bg_runs();
    assert!(total > 0, "the wallpaper frame resolved no background run");
    assert_eq!(
        at_base, 0,
        "under a wallpaper there is no scalar band base, so no run can be AT it"
    );
    assert_eq!(
        r.last_bg_fills(),
        total,
        "a wallpapered frame elided a background run — the selection band whose \
         colour coincides with the theme background would show the picture \
         through it"
    );
}
