// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A slow pointer stroke earns one existing petting response. The caller
//! supplies contact only while the pointer touches the eligible pet's body.
//! This detector observes inputs; it never asks the host for animation wakes.

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

#[derive(Default, Debug)]
pub(crate) struct StrokeDetector {
    last: Option<Sample>,
    travel: f32,
    cooldown_until: Option<Instant>,
}

impl StrokeDetector {
    /// `point` is an ABSOLUTE pointer position in grid cells, not a position
    /// relative to the moving pet. Only its displacement earns travel; changing
    /// the pet's body or its width under a still pointer earns nothing.
    ///
    /// `None` (leave, hidden, reduced motion, or work) forgets contact and
    /// partial travel, while retaining the earned stroke's cooldown. Reentry
    /// therefore cannot manufacture movement or bypass the rate limit.
    pub(crate) fn sample(
        &mut self,
        now: Instant,
        point: Option<(f32, f32)>,
        body_width: f32,
    ) -> bool {
        let Some(point) = point.filter(|(x, y)| {
            x.is_finite() && y.is_finite() && body_width.is_finite() && body_width > 0.0
        }) else {
            self.last = None;
            self.travel = 0.0;
            return false;
        };
        let Some(previous) = self.last else {
            self.last = Some(Sample {
                point,
                moved_at: now,
            });
            return false;
        };
        let elapsed = now.saturating_duration_since(previous.moved_at);
        if point == previous.point {
            // Frame cadence can repeat this sample hundreds of times between
            // real pointer events. Preserve the last MOVEMENT timestamp, or a
            // slow sparse event will look like a one-frame dash when it lands.
            if elapsed > MAX_GAP {
                self.travel = 0.0;
            }
            return false;
        }
        self.last = Some(Sample {
            point,
            moved_at: now,
        });
        if elapsed.is_zero() || elapsed > MAX_GAP {
            self.travel = 0.0;
            return false;
        }
        let distance = (point.0 - previous.point.0).hypot(point.1 - previous.point.1) / body_width;
        let speed = distance / elapsed.as_secs_f32();
        if !speed.is_finite()
            || !(MIN_SPEED..=MAX_SPEED).contains(&speed)
            || self.cooldown_until.is_some_and(|until| now < until)
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_still_frames_preserve_the_real_movement_clock() {
        let start = Instant::now();
        let mut sparse = StrokeDetector::default();
        let mut framed = StrokeDetector::default();
        let mut responses = 0;
        for step in 0..=8_u64 {
            let at = start + Duration::from_millis(step * 250);
            let point = (step as f32 * 0.6, 3.0);
            let expected = sparse.sample(at, Some(point), 4.0);
            assert_eq!(framed.sample(at, Some(point), 4.0), expected);
            responses += u32::from(expected);
            // The last repeated frame is 1 ms before the next real move.
            for offset in [16, 64, 128, 192, 249] {
                assert!(!framed.sample(at + Duration::from_millis(offset), Some(point), 4.0));
            }
        }
        assert_eq!(responses, 1, "the sparse input really earned a stroke");
    }

    #[test]
    fn a_still_pointer_cannot_earn_from_a_moving_or_breathing_body() {
        let start = Instant::now();
        let mut detector = StrokeDetector::default();
        for step in 0..=1000_u64 {
            let body_width = 2.0 + (step % 30) as f32 * 0.1;
            assert!(!detector.sample(
                start + Duration::from_millis(step * 16),
                Some((12.0, 4.0)),
                body_width,
            ));
        }
        assert_eq!(detector.travel, 0.0);
        assert!(detector.cooldown_until.is_none());
    }

    #[test]
    fn contact_loss_discards_partial_travel_and_cannot_bypass_cooldown() {
        let start = Instant::now();
        let mut detector = StrokeDetector::default();
        assert!(!detector.sample(start, Some((0.0, 0.0)), 4.0));
        assert!(!detector.sample(start + Duration::from_millis(250), Some((0.9, 0.0)), 4.0));
        assert!(detector.sample(start + Duration::from_millis(500), Some((1.8, 0.0)), 4.0));
        for step in 1..=5_u64 {
            let at = start + Duration::from_millis(500 + step * 250);
            assert!(!detector.sample(at, None, 4.0));
            assert!(!detector.sample(at, Some((0.0, 0.0)), 4.0));
            assert!(!detector.sample(at + Duration::from_millis(200), Some((1.8, 0.0)), 4.0));
        }
        assert!(!detector.sample(start + Duration::from_millis(2000), None, 4.0));
        assert!(!detector.sample(start + Duration::from_millis(2000), Some((0.0, 0.0)), 4.0));
        assert!(!detector.sample(start + Duration::from_millis(2250), Some((0.9, 0.0)), 4.0));
        assert!(detector.sample(start + Duration::from_millis(2500), Some((1.8, 0.0)), 4.0));
    }

    #[test]
    fn gaps_fast_sweeps_jitter_and_invalid_inputs_discard_partial_strokes() {
        let start = Instant::now();
        for (offset, point, width) in [
            (601, Some((1.8, 0.0)), 4.0),   // a pause, not one gesture
            (1, Some((1.8, 0.0)), 4.0),     // a dash, not a stroke
            (250, Some((0.901, 0.0)), 4.0), // desk jitter
            (250, Some((f32::NAN, 0.0)), 4.0),
            (250, Some((f32::INFINITY, 0.0)), 4.0),
            (250, Some((1.8, 0.0)), 0.0),
            (250, Some((1.8, 0.0)), f32::INFINITY),
            (250, None, 4.0),
        ] {
            let mut detector = StrokeDetector::default();
            assert!(!detector.sample(start, Some((0.0, 0.0)), 4.0));
            let partial = start + Duration::from_millis(250);
            assert!(!detector.sample(partial, Some((0.9, 0.0)), 4.0));
            assert!(detector.travel > 0.0);
            assert!(!detector.sample(partial + Duration::from_millis(offset), point, width));
            assert_eq!(detector.travel, 0.0);
            assert!(detector.cooldown_until.is_none());
        }
    }

    #[test]
    fn scale_changes_do_not_change_the_stroke_threshold() {
        let start = Instant::now();
        for width in [0.5, 1.0, 4.0, 20.0] {
            let mut detector = StrokeDetector::default();
            assert!(!detector.sample(start, Some((0.0, 0.0)), width));
            assert!(!detector.sample(
                start + Duration::from_millis(250),
                Some((0.23 * width, 0.0)),
                width
            ));
            assert!(detector.sample(
                start + Duration::from_millis(500),
                Some((0.46 * width, 0.0)),
                width
            ));
        }
    }

    #[test]
    fn a_stationary_pause_cannot_preserve_half_a_stroke() {
        let start = Instant::now();
        let mut detector = StrokeDetector::default();
        assert!(!detector.sample(start, Some((0.0, 0.0)), 4.0));
        assert!(!detector.sample(start + Duration::from_millis(250), Some((0.9, 0.0)), 4.0));
        for step in 1..=100_u64 {
            assert!(!detector.sample(
                start + Duration::from_millis(250 + step * 16),
                Some((0.9, 0.0)),
                4.0
            ));
        }
        assert_eq!(detector.travel, 0.0);
        assert!(!detector.sample(start + Duration::from_secs(2), Some((1.8, 0.0)), 4.0));
    }

    /// Tier-1 binds the bounded model to the real detector. `event` is the
    /// input class supplied by this trace, not a classification inferred from
    /// the returned boolean; a premature response cannot label itself lawful.
    /// The outstanding cooldown obligation comes from the observed earned
    /// response's time, independently of the detector's retained deadline.
    #[test]
    fn real_stroke_detector_transitions_conform_and_reject_negative_controls() {
        use std::collections::{BTreeMap, BTreeSet};

        use aterm_spec::{derive::pet_stroke_detector_model, verify};

        let model = pet_stroke_detector_model();
        let start = Instant::now();
        let mut detector = StrokeDetector::default();
        let mut state = model.init_state();
        let mut last_earned: Option<Instant> = None;
        let mut covered = BTreeSet::new();
        for (ms, point, action, event) in [
            (0, Some((0.0, 0.0)), "Seed", 1),
            (250, Some((0.9, 0.0)), "Partial", 2),
            (300, Some((0.9, 0.0)), "Still", 3),
            (500, Some((1.8, 0.0)), "Earn", 4),
            (750, Some((2.7, 0.0)), "CoolingMove", 5),
            (800, None, "Reset", 6),
            (850, Some((0.0, 0.0)), "Seed", 1),
            (1100, Some((1.8, 0.0)), "CoolingMove", 5),
            (1150, Some((1.8, 0.0)), "Still", 3),
            (2000, Some((1.8, 0.0)), "ExpireCooldown", 8),
            (2250, Some((3.6, 0.0)), "Reject", 7),
            (2500, Some((4.5, 0.0)), "Partial", 2),
            (2600, None, "Reset", 6),
            (2700, Some((1.0, 0.0)), "Seed", 1),
            (2701, Some((10.0, 0.0)), "Reject", 7),
            (2951, Some((10.9, 0.0)), "Partial", 2),
            (3201, Some((11.8, 0.0)), "Earn", 4),
        ] {
            let now = start + Duration::from_millis(ms);
            let emitted = detector.sample(now, point, 4.0);
            if emitted {
                last_earned = Some(now);
            }
            let post = BTreeMap::from([
                ("contact", i64::from(detector.last.is_some())),
                ("progress", i64::from(detector.travel > 0.0)),
                (
                    "cooling",
                    i64::from(detector.cooldown_until.is_some_and(|until| now < until)),
                ),
                (
                    "cooldown_due",
                    i64::from(last_earned.is_some_and(|earned| now < earned + COOLDOWN)),
                ),
                ("emitted", i64::from(emitted)),
                ("event", event),
            ]);
            let mut expected = state.clone();
            assert!(
                model.fire(action, &mut expected),
                "{action} is enabled at {state:?}"
            );
            assert_eq!(
                post, expected,
                "real detector diverged at {action}, {ms} ms"
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &post),
                    "{}",
                    invariant.name
                );
            }
            if covered.insert(action) {
                let (valid, why) = verify::validate_transition_tiered(
                    &model,
                    &[],
                    &state,
                    &post,
                    Some(action),
                    "real pointer stroke",
                );
                assert!(valid, "{action}: {why}");
            }
            if matches!(action, "Still" | "Partial" | "CoolingMove") {
                let mut premature = post.clone();
                premature.insert("emitted", 1);
                assert!(!model.check_invariant("OnlyCompleteStrokesEarn", &premature));
            }
            if action == "Reset" {
                let mut retained = post.clone();
                retained.insert("contact", 1);
                assert!(!model.check_invariant("ResetForgetsContact", &retained));
                if post[&"cooldown_due"] == 1 {
                    let mut bypass = post.clone();
                    bypass.insert("cooling", 0);
                    assert!(!model.check_invariant("CooldownCannotBeBypassed", &bypass));
                }
            }
            state = post;
        }
        assert_eq!(
            covered.len(),
            model.actions.len(),
            "every model action ran real code"
        );
    }
}
