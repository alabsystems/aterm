// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A DEGENERATE SENSE NEVER PUTS THE PET NOWHERE** (2026-09-15).
//!
//! The pet's body is fractional cells carried in `f32`, and the brain does 115
//! divisions, roots and trig against numbers the host supplies — against only
//! ten finiteness guards. A zero cell metric or a zero-sized grid is not
//! hypothetical: it is what a host holds between a resize and the next surface
//! attach, and a NaN there would draw the cat nowhere or at a garbage position,
//! which is exactly the shape a person reports as the cat "flashing".
//!
//! This walks the legal-but-degenerate corners and asserts the body stays
//! finite and on the grid whenever it is drawn at all. It passes today; it is
//! here so it keeps passing.
use aterm_effects::kitty_pet::{PetBrain, PetSense};
use std::time::{Duration, Instant};

fn s(now: Instant, rows: u16, cols: u16, cw: u16, ch: u16, caret: Option<(u16, u16)>) -> PetSense {
    PetSense {
        caret_drawn: true,
        now,
        caret,
        wrapped: false,
        rows,
        cols,
        cell_w: cw,
        cell_h: ch,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
    }
}

/// One degenerate corner: its name, the grid rows and columns, the cell width and
/// height in pixels, and the caret (row, column) if there is one.
type Shape<'a> = (&'a str, u16, u16, u16, u16, Option<(u16, u16)>);

#[test]
fn no_legal_sense_makes_the_pet_non_finite_or_off_grid() {
    let t0 = Instant::now();
    let shapes: Vec<Shape> = vec![
        ("zero cell metrics", 50, 151, 0, 0, Some((10, 10))),
        ("zero cell width", 50, 151, 0, 28, Some((10, 10))),
        ("zero cell height", 50, 151, 15, 0, Some((10, 10))),
        ("zero grid", 0, 0, 15, 28, Some((0, 0))),
        ("one by one", 1, 1, 15, 28, Some((0, 0))),
        ("caret past the grid", 24, 80, 15, 28, Some((9999, 9999))),
        ("max caret", 24, 80, 15, 28, Some((u16::MAX, u16::MAX))),
        ("huge grid", u16::MAX, u16::MAX, 1, 1, Some((0, 0))),
        ("no caret", 24, 80, 15, 28, None),
    ];
    let mut bad = Vec::new();
    for (name, rows, cols, cw, ch, caret) in shapes {
        let mut pet = PetBrain::default();
        let mut ms = 0u64;
        for step in 0..40u64 {
            ms += 16;
            let f = pet.tick(s(t0 + Duration::from_millis(ms), rows, cols, cw, ch, caret));
            if !f.col.is_finite() || !f.row.is_finite() {
                bad.push(format!(
                    "{name}: step {step} NON-FINITE col={} row={}",
                    f.col, f.row
                ));
                break;
            }
            // A body that is drawn must be somewhere a person could see.
            if f.alpha > 0
                && (f.col < -64.0
                    || f.row < -64.0
                    || f.col > f32::from(cols) + 64.0
                    || f.row > f32::from(rows) + 64.0)
            {
                bad.push(format!(
                    "{name}: step {step} OFF-GRID col={:.1} row={:.1} (grid {cols}x{rows}) alpha={}",
                    f.col, f.row, f.alpha
                ));
                break;
            }
        }
    }
    assert!(
        bad.is_empty(),
        "the pet left the finite, visible world:\n  {}",
        bad.join("\n  ")
    );
}
