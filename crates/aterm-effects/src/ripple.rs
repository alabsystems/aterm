// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glint wave field for the mascot and harness glass ([`GLINT_PERIOD_MS`](crate::ripple::GLINT_PERIOD_MS), [`FLOW_STEP_MS`](crate::ripple::FLOW_STEP_MS),
//! [`GLINT_CELLS_PER_S`](crate::ripple::GLINT_CELLS_PER_S), [`GLINT_TAIL_CELLS`](crate::ripple::GLINT_TAIL_CELLS), [`GLINT_RISE_CELLS`](crate::ripple::GLINT_RISE_CELLS), [`GLINT_ECHO_CELLS`](crate::ripple::GLINT_ECHO_CELLS),
//! [`GLINT_ECHO_GAIN`](crate::ripple::GLINT_ECHO_GAIN), [`GLINT_LEVELS`](crate::ripple::GLINT_LEVELS), [`GLINT_ROW_ASPECT`](crate::ripple::GLINT_ROW_ASPECT), [`cell_distance`](crate::ripple::cell_distance),
//! [`claude_glint`](crate::ripple::claude_glint), [`GLINT_LAP`](crate::ripple::GLINT_LAP), [`glint`](crate::ripple::glint), [`TWINKLE_FLASH_BOUND_HZ`]).

use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;

/// The step the field is drawn on, in ms.
pub const FLOW_STEP_MS: u64 = 128;

/// One ripple cycle in ms.
pub const GLINT_PERIOD_MS: u64 = 3072;

/// How often Claude Code 2.1.304 repaints its mascot's wand while its ripple runs, at the median.
pub const CLAUDE_REPAINT_MEDIAN_MS: u64 = 134;

/// The same at its 90th percentile (p10 118 ms).
pub const CLAUDE_REPAINT_P90_MS: u64 = 142;

/// How fast a ring's front runs outward, in cells a second.
pub const GLINT_CELLS_PER_S: f32 = 7.0;

/// The ring's tail behind its front.
pub const GLINT_TAIL_CELLS: f32 = 1.8;

/// The ring's rise ahead of its front.
pub const GLINT_RISE_CELLS: f32 = 0.9;

/// How far the echo runs behind its ring, in cells.
pub const GLINT_ECHO_CELLS: f32 = 3.5;

/// The echo's strength against its ring's.
pub const GLINT_ECHO_GAIN: f32 = 0.4;

/// A cell row's height in cells of width, for [`cell_distance`].
pub const GLINT_ROW_ASPECT: f32 = 2.0;

/// The field's levels: it is quantised to `1 / GLINT_LEVELS`.
pub const GLINT_LEVELS: f32 = 8.0;

/// How far a whole glint mixes a mark toward white on a dark ground.
pub const GLINT_CREST_DARK: f32 = 0.8;

/// How far a whole glint deepens a mark's print on a light ground.
pub const GLINT_CREST_LIGHT: f32 = 0.7;

/// The distance between two rings of the train, in cells.
pub const GLINT_LAP: f32 = GLINT_CELLS_PER_S * GLINT_PERIOD_MS as f32 / 1000.0;

const _: () = {
    assert!(
        GLINT_PERIOD_MS.is_multiple_of(FLOW_STEP_MS),
        "the step grid divides the cycle: Claude's 24 frames"
    );
    assert!(
        2.0 * 1000.0 / (GLINT_PERIOD_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the ripple glints under the photosensitivity bound"
    );
};

/// A ring's profile at `x = T − d` cells: its tail behind the front, its
/// rise ahead of it, `1` at the front.
#[inline]
const fn profile(x: f32) -> f32 {
    const INV_TAIL: f32 = -1.0 / GLINT_TAIL_CELLS;
    const INV_RISE: f32 = 1.0 / GLINT_RISE_CELLS;
    if x >= 5.0 || x <= -2.5 {
        0.0
    } else if x >= 0.0 {
        crate::color_math::exp_f32(x * INV_TAIL)
    } else {
        crate::color_math::exp_f32(x * INV_RISE)
    }
}

/// The current ring's distance from the origin at `ms`, in cells: `0` at
/// each cycle's start, under [`GLINT_LAP`].
#[inline]
const fn front(ms: u64) -> f32 {
    const CELLS_PER_MS: f32 = GLINT_CELLS_PER_S / 1000.0;
    (ms % GLINT_PERIOD_MS) as f32 * CELLS_PER_MS
}

/// `v` on the field's levels.
#[inline]
const fn quantise(v: f32) -> f32 {
    if v == 0.0 {
        return 0.0;
    }
    const INV_LEVELS: f32 = 1.0 / GLINT_LEVELS;
    (v * GLINT_LEVELS).round() * INV_LEVELS
}

/// Claude's own field, literally.
#[must_use]
#[inline]
pub const fn claude_glint(d_cells: f32, ms: u64) -> f32 {
    let x = front(ms) - d_cells;
    if x <= -2.5 || x >= 5.0 + GLINT_ECHO_CELLS {
        return 0.0;
    }
    let main = profile(x);
    let level = if main >= GLINT_ECHO_GAIN || x <= GLINT_ECHO_CELLS - 2.5 {
        main
    } else {
        main.max(GLINT_ECHO_GAIN * profile(x - GLINT_ECHO_CELLS))
    };
    quantise(level)
}

/// The ring train at `x = T − d`, the rings born so far one [`GLINT_LAP`]
/// apart outward of the current one (no ring is drawn before it is born):
/// behind the current front (`x ≥ 0`), the current ring's tail; ahead of it,
/// the two rings about the point — the one inside it by its rise (the
/// current ring itself, `x` exactly, within a lap of its front) and the one
/// outside it by its tail.
#[inline]
const fn train(x: f32) -> f32 {
    const INV_LAP: f32 = -1.0 / GLINT_LAP;
    if x >= 0.0 {
        return profile(x);
    }
    let k = (x * INV_LAP).ceil();
    let inner = x + (k - 1.0) * GLINT_LAP;
    if inner > -2.5 {
        profile(inner)
    } else {
        profile(inner + GLINT_LAP)
    }
}

/// The harness's field: the glint (`0..=1`, in eighths) at `d` cells from the wand at `ms`.
#[must_use]
#[inline]
pub const fn glint(d_cells: f32, ms: u64) -> f32 {
    let x = front(ms) - d_cells;
    if x >= 5.0 + GLINT_ECHO_CELLS {
        return 0.0;
    }
    let main = train(x);
    let level = if main >= GLINT_ECHO_GAIN {
        main
    } else {
        main.max(GLINT_ECHO_GAIN * train(x - GLINT_ECHO_CELLS))
    };
    quantise(level)
}

/// The field's distance, in cells, of a point `dx_cells` columns and `dy_rows` rows from the wand.
#[must_use]
#[inline]
pub const fn cell_distance(dx_cells: f32, dy_rows: f32) -> f32 {
    crate::color_math::hypot_f32(dx_cells, GLINT_ROW_ASPECT * dy_rows)
}

#[cfg(test)]
#[path = "ripple_tests.rs"]
mod tests;
