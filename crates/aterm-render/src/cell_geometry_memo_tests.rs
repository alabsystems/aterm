// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Renderer::cell_geometry`] keeps its answer per px, and every write to one
//! of its inputs forgets what was kept: asked again, it answers what the face
//! derives now. Counted by [`crate::test_counters`], never timed.

use super::*;
use crate::test_counters::take_cell_geometry_derivations;

const WGHT: u32 = u32::from_be_bytes(*b"wght");

/// The sizes the kept answers are checked at: three windows' worth.
const SIZES: [f32; 3] = [12.0, 16.0, 24.0];

/// The checked-in variable face (Cinzel, `wght` 400..900) as a user's own
/// font file, instantiated at 700 so the derivation takes the variable path:
/// a face parse per derivation. Its bytes carry one byte of trailing padding,
/// so they are not the bundled `display:engraved` face.
fn variable_primary() -> Renderer {
    let mut bytes = include_bytes!("../assets/game/Cinzel-var.ttf").to_vec();
    bytes.push(0);
    assert!(display_face_fit(&bytes).is_none(), "not the display face");
    let mut r = Renderer::from_bytes(&bytes, 16.0, Theme::default()).expect("Cinzel parses");
    assert!(r.set_font_variations(&[(WGHT, 700.0)], 0.0), "instantiated");
    assert!(r.variation_coords().is_some(), "a variable primary");
    r
}

/// What `r.cell_geometry` answers at every size in [`SIZES`].
fn asked(r: &Renderer) -> Vec<(usize, usize, i32)> {
    SIZES.iter().map(|&px| r.cell_geometry(px)).collect()
}

/// What the face derives at every size in [`SIZES`] now.
fn derived(r: &Renderer) -> Vec<(usize, usize, i32)> {
    SIZES
        .iter()
        .map(|&px| r.derive_window_cell_geometry(px).expect("metrics"))
        .collect()
}

/// Ask at every size (so each answer is kept), `mutate`, and ask again: the
/// answers are the face's new derivations, and they moved — so a kept answer
/// that survived the write would have been caught.
fn forgets_on(label: &str, r: &mut Renderer, mutate: impl FnOnce(&mut Renderer)) {
    let before = asked(r);
    assert_eq!(before, asked(r), "{label}: kept");
    mutate(r);
    let now = derived(r);
    assert_ne!(before, now, "{label}: the write moves the geometry");
    assert_eq!(asked(r), now, "{label}: asked again, the new geometry");
}

/// The answer is derived once per size and kept: asked again and again it
/// costs no derivation (each one a face parse for this variable primary),
/// and it is the derivation's own answer. Four sizes are kept at once, so
/// windows on displays of different scales asking in turn never evict each
/// other; a fifth evicts the oldest.
///
/// NEGATIVE CONTROL: the first ask at each size derives (the counter
/// counts), and an evicted size derives again. Without the memo every ask
/// derives, and the first assertion reads 8 (mutation-checked).
#[test]
fn the_cell_geometry_is_derived_once_per_size() {
    let r = variable_primary();
    let _ = take_cell_geometry_derivations();
    let first = r.cell_geometry(16.0);
    for _ in 0..7 {
        assert_eq!(r.cell_geometry(16.0), first, "the kept answer");
    }
    assert_eq!(take_cell_geometry_derivations(), 1, "derived once");
    assert_eq!(
        Some(first),
        r.derive_window_cell_geometry(16.0),
        "the derivation's own answer"
    );
    let _ = take_cell_geometry_derivations();
    for _ in 0..8 {
        for px in [16.0, 12.0, 24.0, 32.0] {
            let _ = r.cell_geometry(px);
        }
    }
    assert_eq!(
        take_cell_geometry_derivations(),
        3,
        "four sizes kept at once: each new one derived once"
    );
    // CONTROL: a fifth size evicts the oldest kept (16), which derives again.
    let _ = r.cell_geometry(40.0);
    assert_eq!(take_cell_geometry_derivations(), 1, "CONTROL: a new size");
    assert_eq!(r.cell_geometry(16.0), first, "the same answer");
    assert_eq!(
        take_cell_geometry_derivations(),
        1,
        "CONTROL: the evicted size derives again"
    );
}

/// Every write to an input of the derivation forgets what was kept: the
/// variation instance (`var_coords`, written by `refresh_variations`), the
/// line height (`line_height_scale`), the baseline escape hatch
/// (`baseline_adjust`) and the primary face with what is read off it
/// (`set_primary_font`: `font`, `rb_primary_bytes`, `typo_lm`,
/// `letter_descent`, and `var_coords` dropped to `None`). Each write below
/// moves the geometry, so a kept answer surviving it is caught.
///
/// Mutation-checked: removing any one `forget_cell_geometry` call fails the
/// case for its write. The face swap lands on an unvaried face, whose
/// instance re-resolves to the `None` it was just set to, so
/// `refresh_variations` forgets nothing there and the swap's own forget is
/// the one under test.
#[test]
fn every_input_of_the_cell_geometry_forgets_what_was_kept() {
    let mut r = variable_primary();
    forgets_on("the instance", &mut r, |r| {
        assert!(r.set_font_variations(&[(WGHT, 400.0)], 0.0));
    });
    forgets_on("the line height", &mut r, |r| r.set_line_height(1.4));
    forgets_on("the baseline", &mut r, |r| r.set_adjust_baseline(3));
    forgets_on("the primary face", &mut r, |r| {
        r.set_primary_font(embedded_font())
            .expect("DejaVu Sans Mono");
        assert!(r.variation_coords().is_none(), "an unvaried face");
    });
}

/// Activating another size is not an input: the kept answers stay kept, so
/// windows of different scales drawing in turn derive nothing. Neither is a
/// zoom's `set_px` on the same renderer: the px is the key.
///
/// NEGATIVE CONTROL: a write that is an input (the line height) derives once
/// per size again.
#[test]
fn activating_a_size_keeps_what_was_kept() {
    let mut r = variable_primary();
    let before = asked(&r);
    let _ = take_cell_geometry_derivations();
    for &px in SIZES.iter().cycle().take(9) {
        r.activate_px(px);
        assert_eq!(asked(&r), before, "the kept answers at {px}");
    }
    r.set_px(SIZES[0]);
    assert_eq!(asked(&r), before, "the kept answers after set_px");
    assert_eq!(take_cell_geometry_derivations(), 0, "none derived");
    r.set_line_height(1.2);
    let _ = asked(&r);
    assert_eq!(
        take_cell_geometry_derivations(),
        SIZES.len(),
        "CONTROL: an input's write derives each size again"
    );
}
