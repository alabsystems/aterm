// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE'S RIPPLE: the glint wave Claude Code 2.1.304's mascot shimmers
//! with, as decoded from its bundle, for the harness's glass to shimmer in
//! step with it (owner, 2026-10-19: "Make the rainbow at the banner continue
//! to shimmer and flow continuously, which is also aligned with how the
//! claude alien shimmers"). The card's continuous flow, the composer's
//! border and the agents panel read it; this module is the field alone,
//! pure — no clock is read here: time comes in as milliseconds on the
//! caller's clock.
//!
//! # The field
//!
//! Every [`GLINT_PERIOD_MS`] (3072 ms, [`FLOW_STEP_MS`] steps of 128 ms: 24
//! frames a cycle) a ring is born at the origin — the mascot's wand — and
//! runs outward at [`GLINT_CELLS_PER_S`] (7 cells a second). Its profile
//! over `x = T − d` (`T` the ring's distance from the origin, `d` the
//! point's, both in cells) is `Ch(x) = e^(−x/1.8)` behind the front
//! (`x ≥ 0`, [`GLINT_TAIL_CELLS`]) and `e^(x/0.9)` ahead of it
//! ([`GLINT_RISE_CELLS`]); an echo runs [`GLINT_ECHO_CELLS`] behind at
//! [`GLINT_ECHO_GAIN`]; the field is the larger of the two, quantised to
//! eighths ([`GLINT_LEVELS`]). Distance is measured with a cell row
//! [`GLINT_ROW_ASPECT`] cells tall ([`cell_distance`]), as a cell is about
//! twice as tall as it is wide.
//!
//! MEASURED on the real 2.1.304 (lab M1): in the fullscreen renderer's
//! workspace mascot the crest runs from the wand LEFTWARD across the mascot,
//! column 8 to column 1 in about a second (seven cells a second), toward
//! white (#efe7ff) on dark, an echo about three cells behind. Only that
//! renderer's key-on mascot shimmers — the inline renderer and the plain
//! mascot are static — it stops on blur and resumes at the same phase, and
//! its clock is the process's, not the wall clock (lab M2).
//!
//! Claude's own formula ([`claude_glint`]) resets `T` every cycle, so its
//! ring only ever reaches [`GLINT_LAP`] (21.5 cells; its rise glints to 24)
//! before the next is born at the wand: nothing past a mascot's width and a
//! half would glint. The harness's ([`glint`]) is a RING TRAIN: every ring
//! born keeps travelling outward, one [`GLINT_LAP`] behind the last, so the
//! wave crosses a whole banner. A ring is drawn only once it is born, so
//! within 14.5 cells of the wand — the mascot and its card's first columns
//! — the two agree exactly, at every instant; further out the earlier rings
//! the train keeps, and Claude's formula drops at its reset, glint on.
//!
//! # Photosensitivity
//!
//! A point glints at most twice a cycle (the ring, then its echo): 0.65 Hz,
//! under the crate's certified [`TWINKLE_FLASH_BOUND_HZ`] (const-asserted).

use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;

/// The step the field is drawn on, in ms: the frame grid of Claude's
/// ripple as decoded (24 frames a cycle). Claude does not repaint on it:
/// MEASURED (lab M2), its wand repaints every [`CLAUDE_REPAINT_MEDIAN_MS`]
/// or so, each repaint showing the frame its clock is in.
pub const FLOW_STEP_MS: u64 = 128;

/// One ripple cycle in ms: a ring is born at the wand every this many ms
/// (MEASURED flare to flare: 3063–3084 ms).
pub const GLINT_PERIOD_MS: u64 = 3072;

/// How often Claude Code 2.1.304 repaints its mascot's wand while its ripple
/// runs, at the median, in ms: MEASURED on a PTY take on `CLOCK_MONOTONIC`
/// (lab M2), unaligned with the [`FLOW_STEP_MS`] grid. A flare (frames 0 and
/// 1 of a cycle) holds two repaints.
pub const CLAUDE_REPAINT_MEDIAN_MS: u64 = 134;

/// The same at its 90th percentile (p10 118 ms): the slow repaints a phase
/// lock must not take for drift.
pub const CLAUDE_REPAINT_P90_MS: u64 = 142;

/// How fast a ring's front runs outward, in cells a second.
pub const GLINT_CELLS_PER_S: f32 = 7.0;

/// The ring's tail behind its front: its glint falls by `1/e` every this
/// many cells.
pub const GLINT_TAIL_CELLS: f32 = 1.8;

/// The ring's rise ahead of its front: its glint falls by `1/e` every this
/// many cells.
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

/// The distance between two rings of the train, in cells: how far a ring
/// runs in one cycle (21.504).
pub const GLINT_LAP: f32 = GLINT_CELLS_PER_S * GLINT_PERIOD_MS as f32 / 1000.0;

const _: () = {
    assert!(
        GLINT_PERIOD_MS.is_multiple_of(FLOW_STEP_MS),
        "the step grid divides the cycle: Claude's 24 frames"
    );
    // Each point glints twice a cycle: the ring, then its echo.
    assert!(
        2.0 * 1000.0 / (GLINT_PERIOD_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the ripple glints under the photosensitivity bound"
    );
};

/// A ring's profile at `x = T − d` cells: its tail behind the front, its
/// rise ahead of it, `1` at the front.
#[inline]
fn profile(x: f32) -> f32 {
    if x >= 0.0 {
        (-x / GLINT_TAIL_CELLS).exp()
    } else {
        (x / GLINT_RISE_CELLS).exp()
    }
}

/// The current ring's distance from the origin at `ms`, in cells: `0` at
/// each cycle's start, under [`GLINT_LAP`].
#[inline]
const fn front(ms: u64) -> f32 {
    GLINT_CELLS_PER_S * (ms % GLINT_PERIOD_MS) as f32 / 1000.0
}

/// `v` on the field's levels.
#[inline]
fn quantise(v: f32) -> f32 {
    (v * GLINT_LEVELS).round() / GLINT_LEVELS
}

/// CLAUDE'S OWN FIELD, literally: the glint (`0..=1`, in eighths) at `d`
/// cells from the wand `ms` into Claude's clock — one ring per cycle, its
/// front reset to the wand at each cycle's start, and its echo. Past
/// [`GLINT_LAP`] and its rise (24 cells) it never glints.
#[must_use]
#[inline]
pub fn claude_glint(d_cells: f32, ms: u64) -> f32 {
    let x = front(ms) - d_cells;
    quantise(profile(x).max(GLINT_ECHO_GAIN * profile(x - GLINT_ECHO_CELLS)))
}

/// The ring train at `x = T − d`, the rings born so far one [`GLINT_LAP`]
/// apart outward of the current one (no ring is drawn before it is born):
/// behind the current front (`x ≥ 0`), the current ring's tail; ahead of it,
/// the two rings about the point — the one inside it by its rise (the
/// current ring itself, `x` exactly, within a lap of its front) and the one
/// outside it by its tail. Every farther ring is fainter than these two.
#[inline]
fn train(x: f32) -> f32 {
    if x >= 0.0 {
        return profile(x);
    }
    let k = (-x / GLINT_LAP).ceil();
    let inner = x + (k - 1.0) * GLINT_LAP;
    profile(inner).max(profile(inner + GLINT_LAP))
}

/// THE HARNESS'S FIELD: the glint (`0..=1`, in eighths) at `d` cells from the
/// wand `ms` into the ripple's clock — Claude's ([`claude_glint`]), with
/// every ring born travelling on outward past [`GLINT_LAP`], so the wave
/// crosses the banner. Equal to Claude's within 14.5 cells of the wand at
/// every instant.
#[must_use]
#[inline]
pub fn glint(d_cells: f32, ms: u64) -> f32 {
    let x = front(ms) - d_cells;
    quantise(train(x).max(GLINT_ECHO_GAIN * train(x - GLINT_ECHO_CELLS)))
}

/// The field's distance, in cells, of a point `dx_cells` columns and
/// `dy_rows` rows from the wand: a row is [`GLINT_ROW_ASPECT`] cells tall.
#[must_use]
#[inline]
pub fn cell_distance(dx_cells: f32, dy_rows: f32) -> f32 {
    dx_cells.hypot(GLINT_ROW_ASPECT * dy_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distances `0..=max` cells, a twentieth apart.
    fn reach(max: f32) -> impl Iterator<Item = f32> {
        (0..=(max * 20.0) as u32).map(|i| i as f32 / 20.0)
    }

    /// Instants through one cycle, 8 ms apart (sixteen to a step).
    fn cycle() -> impl Iterator<Item = u64> {
        (0..GLINT_PERIOD_MS).step_by(8)
    }

    /// INSIDE CLAUDE'S REACH THE FIELD IS CLAUDE'S: within 14.5 cells of the
    /// wand, and within 14.5 cells ahead of Claude's own front, the train
    /// glints exactly as Claude's formula does, at every sampled instant of a
    /// cycle — on its own clock and a later cycle's. CONTROL: further out,
    /// inside the lap, they part (the earlier ring the train keeps).
    #[test]
    fn the_glint_is_claudes_ripple_inside_its_reach() {
        for ms in cycle() {
            for later in [0, 5 * GLINT_PERIOD_MS] {
                let at = ms + later;
                let reach_now = 14.5 + front(at);
                for d in reach(reach_now) {
                    assert_eq!(glint(d, at), claude_glint(d, at), "{d} cells at {at} ms");
                }
            }
        }
        let parted = cycle()
            .flat_map(|ms| reach(GLINT_LAP).map(move |d| (d, ms)))
            .filter(|&(d, ms)| glint(d, ms) != claude_glint(d, ms))
            .count();
        assert!(
            parted > 0,
            "CONTROL: past its reach the train is not Claude's"
        );
    }

    /// THE RING TRAVELS OUTWARD AT SEVEN CELLS A SECOND, across cycles: the
    /// ring born at 0 ms is at `7·t` cells `t` seconds on — its whole-glint
    /// band centred there to a tenth of a cell — for eight seconds, nearly
    /// three laps out.
    #[test]
    fn the_ring_travels_outward_at_seven_cells_a_second() {
        for ms in (200..=8000).step_by(300) {
            let at = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
            let whole: Vec<f32> = (0..=7000)
                .map(|i| i as f32 / 100.0)
                .filter(|d| (d - at).abs() < GLINT_LAP / 2.0 && glint(*d, ms) == 1.0)
                .collect();
            assert!(!whole.is_empty(), "{ms} ms: no whole glint near {at}");
            let centre = whole.iter().sum::<f32>() / whole.len() as f32;
            assert!(
                (centre - at).abs() < 0.1,
                "{ms} ms: the front is at {centre}, not {at}"
            );
        }
    }

    /// THE CREST CROSSES CLAUDE'S MASCOT AS MEASURED (lab M1, the fullscreen
    /// workspace mascot, its wand `✦` at column 10): leftward from the wand,
    /// column 8 to column 1 in about a second (7 cells/s), the echo about
    /// three cells behind. Read off Claude's field every 4 ms: each of
    /// columns 8 down to 1 first glints whole later than the one right of
    /// it, and column 1 does so 900 to 1100 ms after column 8; the echo
    /// trails the front by 2.5 to 4 cells. NEGATIVE CONTROL: the field at
    /// half and at twice the pace (its clock run at half and at twice the
    /// speed) misses the measured second.
    #[test]
    fn the_crest_crosses_the_mascot_leftward_at_the_measured_pace() {
        const WAND: u64 = 10;
        let crossing = |pace: f64| {
            let whole_at = |col: u64| {
                (0..GLINT_PERIOD_MS)
                    .step_by(4)
                    .find(|&ms| claude_glint((WAND - col) as f32, (ms as f64 * pace) as u64) == 1.0)
            };
            let times: Vec<u64> = (1..=8u64)
                .rev()
                .map(|col| whole_at(col).expect("the crest reaches every column"))
                .collect();
            let leftward = times.windows(2).all(|w| w[1] > w[0]);
            (leftward, times[7] - times[0])
        };
        let (leftward, took) = crossing(1.0);
        assert!(leftward, "the crest runs leftward from the wand");
        assert!((900..=1100).contains(&took), "column 8 to 1 in {took} ms");
        assert!(
            (2.5..=4.0).contains(&GLINT_ECHO_CELLS),
            "the echo about three cells behind"
        );
        for pace in [0.5, 2.0] {
            let (_, took) = crossing(pace);
            assert!(
                !(900..=1100).contains(&took),
                "CONTROL: at {pace}x the crossing takes {took} ms"
            );
        }
    }

    /// PAST CLAUDE'S LAP THE TRAIN GLINTS: from 25 cells to 60 every point
    /// reaches a whole glint in the train within a cycle (sampled every
    /// 8 ms). NEGATIVE CONTROL: Claude's own formula, its ring reset at the
    /// wand each cycle, never glints there at all.
    #[test]
    fn past_the_lap_only_the_train_glints() {
        for d in (25..=60).map(|d| d as f32) {
            let train = cycle().map(|ms| glint(d, ms)).fold(0.0, f32::max);
            assert_eq!(train, 1.0, "{d} cells: the train's ring passes");
            let claude = cycle().map(|ms| claude_glint(d, ms)).fold(0.0, f32::max);
            assert_eq!(claude, 0.0, "CONTROL: {d} cells: Claude's never gets there");
        }
    }

    /// The field is quantised to eighths and lies in `0..=1`, everywhere.
    #[test]
    fn the_field_is_on_its_eighths() {
        for ms in cycle().chain([GLINT_PERIOD_MS * 7 + 5]) {
            for d in reach(60.0) {
                for g in [glint(d, ms), claude_glint(d, ms)] {
                    assert!((0.0..=1.0).contains(&g), "{g} at {d} cells, {ms} ms");
                    assert_eq!((g * 8.0).fract(), 0.0, "{g} at {d} cells, {ms} ms");
                }
            }
        }
        assert_eq!(glint(0.0, 0), 1.0, "a ring is born whole at the wand");
        assert_eq!(cell_distance(3.0, 2.0), 5.0, "a row is two cells tall");
    }

    /// THE RIPPLE STAYS UNDER THE PHOTOSENSITIVITY BOUND: at points from the
    /// wand to sixty cells out, drawn on the step grid for a minute, a point
    /// glints (a peak of its level, a plateau counted once) at most twice a
    /// cycle — under [`TWINKLE_FLASH_BOUND_HZ`]. CONTROL: it does glint, at
    /// least once a cycle, everywhere.
    #[test]
    fn the_ripple_stays_under_the_photosensitivity_bound() {
        let steps = 60_000 / FLOW_STEP_MS;
        // Whole cycles in the minute, and cycles it touches.
        let cycles = (60_000 / GLINT_PERIOD_MS) as usize;
        let touched = 60_000u64.div_ceil(GLINT_PERIOD_MS) as usize;
        for d in [0.0, 1.0, 3.0, 7.5, 14.0, 21.5, 30.0, 45.0, 60.0] {
            let mut levels: Vec<f32> = (0..steps).map(|k| glint(d, k * FLOW_STEP_MS)).collect();
            levels.dedup();
            let peaks = levels
                .windows(3)
                .filter(|w| w[1] > w[0] && w[1] > w[2])
                .count();
            assert!(
                peaks as f32 / 60.0 <= TWINKLE_FLASH_BOUND_HZ,
                "{d} cells: {peaks} glints a minute"
            );
            assert!(
                peaks <= 2 * touched,
                "{d} cells: {peaks} glints in {touched} cycles"
            );
            assert!(peaks >= cycles - 1, "CONTROL: {d} cells glints ({peaks})");
        }
    }
}
