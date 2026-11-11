// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fractional_deltas_accumulate_without_flipping() {
    let mut s = ScrollInputState::default();
    assert_eq!(s.add_rows(0.3), 0);
    assert_eq!(s.add_rows(0.3), 0);
    assert_eq!(s.add_rows(0.3), 0);
    assert!((s.frac_rows() - 0.9).abs() < 1e-9, "residual banks");
}

#[test]
fn row_flips_at_exactly_plus_and_minus_one() {
    let mut s = ScrollInputState::default();
    for _ in 0..3 {
        assert_eq!(s.add_rows(0.25), 0);
    }
    assert_eq!(s.add_rows(0.25), 1, "flips at exactly +1.0");
    assert_eq!(s.frac_rows(), 0.0, "the flip consumes the residual");
    for _ in 0..3 {
        assert_eq!(s.add_rows(-0.25), 0);
    }
    assert_eq!(s.add_rows(-0.25), -1, "flips at exactly -1.0");
    assert_eq!(s.frac_rows(), 0.0);
}

#[test]
fn multi_row_deltas_flip_whole_rows_and_bank_the_remainder() {
    let mut s = ScrollInputState::default();
    assert_eq!(s.add_rows(2.75), 2);
    assert_eq!(s.frac_rows(), 0.75);
    assert_eq!(s.add_rows(-4.0), -3);
    assert_eq!(s.frac_rows(), -0.25);
}

#[test]
fn direction_reversal_can_cancel_without_a_flip() {
    let mut s = ScrollInputState::default();
    assert_eq!(s.add_rows(-0.75), 0);
    assert_eq!(s.add_rows(1.0), 0);
    assert_eq!(s.frac_rows(), 0.25);
}

#[test]
fn reset_drops_the_residual() {
    let mut s = ScrollInputState::default();
    s.add_rows(0.5);
    s.reset();
    assert_eq!(s.frac_rows(), 0.0);
    assert_eq!(s.frac_px(16), 0);
}

#[test]
fn history_edges_clamp_the_empty_side_only() {
    let mut s = ScrollInputState::default();
    s.add_rows(-0.4);
    s.clamp_at_history_edges(0, 10);
    assert_eq!(s.frac_rows(), 0.0);
    s.add_rows(0.4);
    s.clamp_at_history_edges(0, 10);
    assert_eq!(s.frac_rows(), 0.4);
    s.clamp_at_history_edges(10, 10);
    assert_eq!(s.frac_rows(), 0.0);
    s.add_rows(-0.4);
    s.clamp_at_history_edges(10, 10);
    assert_eq!(s.frac_rows(), -0.4);
    s.clamp_at_history_edges(0, 0);
    assert_eq!(s.frac_rows(), 0.0);
    s.add_rows(0.4);
    s.clamp_at_history_edges(0, 0);
    assert_eq!(s.frac_rows(), 0.0);
}

#[test]
fn frac_px_negates_rounds_and_clamps_into_the_translate_domain() {
    let mut s = ScrollInputState::default();
    s.add_rows(0.5);
    assert_eq!(s.frac_px(16), -8, "toward older ⇒ band shifts DOWN");
    s.reset();
    s.add_rows(-0.5);
    assert_eq!(s.frac_px(16), 8, "toward newer ⇒ band shifts UP");
    s.reset();
    s.add_rows(0.3);
    assert_eq!(s.frac_px(16), -5, "rounds to the nearest device px (4.8)");
    s.reset();
    s.add_rows(0.999_999);
    assert_eq!(s.frac_px(16), -15, "clamped below cell_h");
    assert_eq!(s.frac_px(1), 0);
    assert_eq!(s.frac_px(0), 0);
}

#[test]
fn non_finite_deltas_are_ignored() {
    let mut s = ScrollInputState::default();
    s.add_rows(0.5);
    assert_eq!(s.add_rows(f64::NAN), 0);
    assert_eq!(s.add_rows(f64::INFINITY), 0);
    assert_eq!(s.add_rows(f64::NEG_INFINITY), 0);
    assert_eq!(s.frac_rows(), 0.5, "residual survives hostile input");
}

#[test]
fn huge_deltas_saturate_without_panicking() {
    let mut s = ScrollInputState::default();
    assert_eq!(s.add_rows(1e18), i32::MAX);
    assert_eq!(s.frac_rows(), 0.0);
    assert_eq!(s.add_rows(-1e18), i32::MIN);
}

#[test]
fn stamp_fills_the_full_grid_band_every_frame() {
    let mut s = ScrollInputState::default();
    s.add_rows(0.5);
    let mut input = RenderInput::empty();
    s.stamp(&mut input, 24, 16);
    assert_eq!(input.scroll_frac_px, -8);
    assert_eq!((input.grid_top_row, input.grid_bot_row), (0, 24));
    s.reset();
    s.stamp(&mut input, 24, 16);
    assert_eq!(input.scroll_frac_px, 0);
}

fn terminal_with_history() -> WebTerminal {
    let mut t = crate::system_font_terminal(4, 20, 16.0);
    for i in 0..12 {
        t.process(format!("line {i}\r\n").as_bytes());
    }
    t
}

#[test]
fn scroll_px_accumulates_and_flips_through_the_export() {
    let mut t = terminal_with_history();
    assert_eq!(t.display_offset(), 0);
    let half = t.cell_height() as f64 / 2.0;
    t.scroll_px(half);
    assert_eq!(t.display_offset(), 0, "half a row banks, no flip");
    assert!((t.scroll_frac_rows() - 0.5).abs() < 1e-9);
    assert!(t.scroll_frac_px() < 0, "toward older presents a DOWN shift");
    t.scroll_px(half);
    assert_eq!(t.display_offset(), 1, "±1.0 accumulated flips one row");
    assert_eq!(t.scroll_frac_rows(), 0.0, "flip consumes the residual");
    assert_eq!(t.scroll_frac_px(), 0);
    t.scroll_lines_frac(-0.5);
    assert!((t.scroll_frac_rows() + 0.5).abs() < 1e-9);
    assert!(t.scroll_frac_px() > 0, "toward newer presents an UP shift");
    t.scroll_lines_frac(-0.5);
    assert_eq!(t.display_offset(), 0);
    assert_eq!(t.scroll_frac_rows(), 0.0);
}

#[test]
fn whole_row_navigation_resets_the_residual() {
    let mut t = terminal_with_history();
    t.scroll_lines_frac(1.5);
    assert!(t.scroll_frac_rows() > 0.0);
    t.scroll_to_bottom();
    assert_eq!((t.display_offset(), t.scroll_frac_rows()), (0, 0.0));
    t.scroll_lines_frac(0.5);
    t.scroll_lines(1);
    assert_eq!(t.scroll_frac_rows(), 0.0, "whole-row jump lands aligned");
    t.scroll_lines_frac(-0.5);
    t.scroll_to_top();
    assert_eq!(t.scroll_frac_rows(), 0.0);
}

#[test]
fn history_ends_clamp_the_residual_through_the_export() {
    let mut t = terminal_with_history();
    t.scroll_lines_frac(-0.7);
    assert_eq!((t.display_offset(), t.scroll_frac_rows()), (0, 0.0));
    t.scroll_to_top();
    let top = t.display_offset();
    t.scroll_lines_frac(0.7);
    assert_eq!((t.display_offset(), t.scroll_frac_rows()), (top, 0.0));
    let mut fresh = crate::system_font_terminal(4, 20, 16.0);
    fresh.scroll_lines_frac(0.7);
    assert_eq!(fresh.scroll_frac_rows(), 0.0);
}

mod host {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/scroll_input_api_tests.rs"
    ));
}
