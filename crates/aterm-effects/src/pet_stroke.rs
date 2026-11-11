// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pointer-stroke detector for pet responses; observes contact samples without
//! requesting animation wakes.

use aterm_time::{Duration, Instant};

const STROKE_TRAVEL: f32 = 0.45;
const MIN_SPEED: f32 = 0.04;
const MAX_SPEED: f32 = 2.5;
const MAX_GAP: Duration = Duration::from_millis(600);
const COOLDOWN: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug)]
struct Sample {
    point: (f32, f32),
    moved_at: Instant,
}

#[derive(Debug)]
pub(crate) struct StrokeDetector {
    last: Option<Sample>,
    travel: f32,
    cooldown_until: Option<Instant>,
}

impl Default for StrokeDetector {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl StrokeDetector {
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            last: None,
            travel: 0.0,
            cooldown_until: None,
        }
    }

    /// `point` is the absolute pointer position in grid cells, not one relative to the pet:
    /// only its displacement earns travel, so a body moving or resizing under a still pointer
    /// earns nothing.
    ///
    /// `None` (leave, hidden, reduced motion, or work) forgets contact and partial travel but
    /// keeps the earned stroke's cooldown, so reentry cannot bypass the rate limit.
    #[inline]
    pub(crate) fn sample(
        &mut self,
        now: Instant,
        point: Option<(f32, f32)>,
        body_width: f32,
    ) -> bool {
        let Some(point) = point else {
            self.last = None;
            self.travel = 0.0;
            return false;
        };
        if !(point.0.is_finite()
            && point.1.is_finite()
            && body_width.is_finite()
            && body_width > 0.0)
        {
            self.last = None;
            self.travel = 0.0;
            return false;
        }
        let Some(previous) = self.last else {
            self.last = Some(Sample {
                point,
                moved_at: now,
            });
            return false;
        };
        if point == previous.point {
            // Frames repeat this sample between real pointer events: keep the
            // last movement's timestamp, or a slow sparse event lands as a dash.
            if self.travel > 0.0 && now.saturating_duration_since(previous.moved_at) > MAX_GAP {
                self.travel = 0.0;
            }
            return false;
        }
        let elapsed = now.saturating_duration_since(previous.moved_at);
        self.last = Some(Sample {
            point,
            moved_at: now,
        });
        if elapsed.is_zero()
            || elapsed > MAX_GAP
            || matches!(self.cooldown_until, Some(until) if now < until)
        {
            self.travel = 0.0;
            return false;
        }
        let distance =
            crate::color_math::hypot_f32(point.0 - previous.point.0, point.1 - previous.point.1)
                / body_width;
        let speed = distance / elapsed.as_secs_f32();
        if !(MIN_SPEED..=MAX_SPEED).contains(&speed) {
            self.travel = 0.0;
            return false;
        }
        self.travel += distance;
        if self.travel < STROKE_TRAVEL {
            return false;
        }
        self.travel = 0.0;
        self.cooldown_until = Some(now + COOLDOWN);
        true
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "pet_stroke_tests.rs"]
mod tests;
