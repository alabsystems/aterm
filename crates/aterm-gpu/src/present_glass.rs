// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENT → GLASS: the span after application present-return that the
//! frontend's `metrics` ledger could not see.
//!
//! `aterm-gui`'s `present_latency` and `input_present` both stop at application
//! present-return: `Frame::present` registers `presentDrawable:`, commits, and
//! returns. Whether a slow reading was the window server holding the drawable
//! or a parked main thread was structurally unanswerable — the 2026-09 audit's
//! 560 ms `max_present_latency_ms` beside `present_drops=0` could not be
//! attributed to either.
//!
//! On the shipped macOS Metal path, `Frame::present` hangs an
//! `addPresentedHandler:` on the drawable whenever a sink is installed. The
//! handler reads `-[MTLDrawable presentedTime]` — the host time the frame
//! reached the display, on CoreAnimation's `CACurrentMediaTime` clock — and
//! reports the interval from the `presentDrawable:` registration (read on the
//! same clock immediately before it) to that instant. A drawable the compositor
//! skipped or never showed (an occluded window, a frame replaced before its
//! vsync, an unparented layer) reports `presentedTime == 0`, delivered as
//! [`GlassSample::Skipped`] rather than as a fabricated duration.
//!
//! This crate publishes nothing itself: the frontend installs one sink with
//! [`install_sink`] and folds the samples into its ledger. No sink, no handler —
//! a process that never installs one pays nothing per present.
//!
//! The handler runs on a Metal/CoreAnimation-owned thread, so a sink must be
//! lock-free and must not block or unwind (the frontend's is a histogram bucket
//! increment).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// One presented handler's reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassSample {
    /// The frame reached the display `ns` after `presentDrawable:` was registered.
    OnGlass {
        /// Registration → `presentedTime`, nanoseconds.
        ns: u64,
    },
    /// `presentedTime` was 0: the drawable was never shown.
    Skipped,
}

/// The frontend's receiver. Called on a framework thread; must not block.
pub type GlassSink = fn(GlassSample);

static SINK: OnceLock<GlassSink> = OnceLock::new();
static DELIVERED: AtomicU64 = AtomicU64::new(0);

/// Install the process's one sink. First install wins; `false` when a sink was
/// already installed.
pub fn install_sink(sink: GlassSink) -> bool {
    SINK.set(sink).is_ok()
}

/// Whether a sink is installed — the swapchain registers a handler only then.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS Metal present registers handlers")
)]
pub(crate) fn sink_installed() -> bool {
    SINK.get().is_some()
}

/// Presented handlers delivered since process start, both kinds.
#[must_use]
pub fn delivered() -> u64 {
    DELIVERED.load(Ordering::Relaxed)
}

/// Hand one sample to the installed sink.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS Metal present registers handlers")
)]
pub(crate) fn deliver(sample: GlassSample) {
    DELIVERED.fetch_add(1, Ordering::Relaxed);
    if let Some(sink) = SINK.get() {
        sink(sample);
    }
}

/// Classify one presented handler's reading. Both arguments are
/// `CACurrentMediaTime` seconds: `registered_s` read just before
/// `presentDrawable:`, `presented_s` the drawable's `presentedTime`.
#[must_use]
pub fn classify(registered_s: f64, presented_s: f64) -> GlassSample {
    if presented_s.is_nan() || presented_s <= 0.0 {
        return GlassSample::Skipped;
    }
    let secs = presented_s - registered_s;
    // The registration read and the display time come from one clock, so a
    // negative span is not expected; clamp rather than wrap if one ever lands.
    let ns = if secs.is_finite() && secs > 0.0 {
        Duration::from_secs_f64(secs).as_nanos()
    } else {
        0
    };
    GlassSample::OnGlass {
        ns: u64::try_from(ns).unwrap_or(u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::{GlassSample, classify};

    #[test]
    fn a_zero_presented_time_is_a_skip_not_a_duration() {
        assert_eq!(classify(12.5, 0.0), GlassSample::Skipped);
        assert_eq!(classify(12.5, f64::NAN), GlassSample::Skipped);
        assert_eq!(classify(12.5, -1.0), GlassSample::Skipped);
    }

    #[test]
    fn an_on_glass_frame_reports_registration_to_display() {
        let GlassSample::OnGlass { ns } = classify(100.0, 100.016) else {
            panic!("a nonzero presentedTime is on glass");
        };
        assert!(
            (15_999_000..=16_001_000).contains(&ns),
            "16 ms span read as {ns} ns"
        );
    }

    #[test]
    fn a_display_time_before_registration_clamps_to_zero() {
        assert_eq!(classify(100.0, 99.0), GlassSample::OnGlass { ns: 0 });
    }
}
