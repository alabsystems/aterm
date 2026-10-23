// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`Renderer::cell_geometry`] memoization and invalidation.

use super::*;
use crate::test_counters::take_cell_geometry_derivations;

const WGHT: u32 = u32::from_be_bytes(*b"wght");

const SIZES: [f32; 3] = [12.0, 16.0, 24.0];

fn variable_primary() -> Renderer {
    let mut bytes = include_bytes!("../assets/game/Cinzel-var.ttf").to_vec();
    bytes.push(0);
    assert!(display_face_fit(&bytes).is_none(), "not the display face");
    let mut r = Renderer::from_bytes(&bytes, 16.0, Theme::default()).expect("Cinzel parses");
    assert!(r.set_font_variations(&[(WGHT, 700.0)], 0.0), "instantiated");
    assert!(r.variation_coords().is_some(), "a variable primary");
    r
}

fn asked(r: &Renderer) -> Vec<(usize, usize, i32)> {
    SIZES.iter().map(|&px| r.cell_geometry(px)).collect()
}

fn derived(r: &Renderer) -> Vec<(usize, usize, i32)> {
    SIZES
        .iter()
        .map(|&px| r.derive_window_cell_geometry(px).expect("metrics"))
        .collect()
}

fn forgets_on(label: &str, r: &mut Renderer, mutate: impl FnOnce(&mut Renderer)) {
    let before = asked(r);
    assert_eq!(before, asked(r), "{label}: kept");
    mutate(r);
    let now = derived(r);
    assert_ne!(before, now, "{label}: the write moves the geometry");
    assert_eq!(asked(r), now, "{label}: asked again, the new geometry");
}

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
    let _ = r.cell_geometry(40.0);
    assert_eq!(take_cell_geometry_derivations(), 1, "CONTROL: a new size");
    assert_eq!(r.cell_geometry(16.0), first, "the same answer");
    assert_eq!(
        take_cell_geometry_derivations(),
        1,
        "CONTROL: the evicted size derives again"
    );
}

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
