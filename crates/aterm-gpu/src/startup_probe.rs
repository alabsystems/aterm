// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold-build sub-phase timing for the `backend_finalize` startup phase: the
//! legs every arm runs in [`crate::GpuRenderer::new_with_face`] and its
//! siblings (the context, the font thread and its join, and the renderer's
//! assembly), each recorded once per process.

use std::sync::atomic::{AtomicU64, Ordering};

use aterm_time::Duration;

/// Legs of one cold GPU-renderer build, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Leg {
    /// `GpuContext::new`: the arm's device (the Metal device and queue, the EGL
    /// or GLX display and context, or the D3D11 device).
    Context = 0,
    /// System font discovery, face parse, and ASCII prewarm on the font thread; runs in
    /// parallel with [`Leg::Context`] and is not in the exclusive sum.
    FontThread = 1,
    /// The wait at the font-thread join after [`Leg::Context`].
    FontJoin = 2,
    /// `GpuRenderer::from_parts`: the renderer assembled from its context and
    /// face. The last leg a build records.
    Assemble = 3,
}

/// How many slots [`LEG_NS`] carries (`Leg::Assemble as usize + 1`).
pub const LEG_COUNT: usize = 4;

static LEG_NS: [AtomicU64; LEG_COUNT] = [const { AtomicU64::new(0) }; LEG_COUNT];

/// Clamp a measured duration into the slot encoding: saturate at `u64::MAX` ns
/// and floor at 1 ns so a recorded leg is never read as unset.
#[inline]
fn slot_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Records `elapsed` for `leg` (first write wins) with release ordering.
#[inline]
pub fn record(leg: Leg, elapsed: Duration) {
    let _ = LEG_NS[leg as usize].compare_exchange(
        0,
        slot_ns(elapsed),
        Ordering::Release,
        Ordering::Relaxed,
    );
}

/// Reads the nanosecond duration recorded for `leg` (`0` if unset).
#[inline]
pub fn leg_ns(leg: Leg) -> u64 {
    LEG_NS[leg as usize].load(Ordering::Acquire)
}

#[cfg(test)]
#[path = "startup_probe_tests.rs"]
mod tests;
