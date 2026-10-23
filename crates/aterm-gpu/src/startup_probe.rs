// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold-build sub-phase timing for the `backend_finalize` startup phase.

use std::sync::atomic::{AtomicU64, Ordering};

use aterm_time::Duration;
#[cfg(wgpu_arm)]
use aterm_time::Instant;

/// Exclusive legs of one cold GPU-renderer build, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Leg {
    /// `wgpu::Instance::new` — backend enumeration and driver load.
    GpuInstance = 0,
    /// `request_adapter` + `get_info`.
    GpuAdapter = 1,
    /// `request_device` — the blocking driver round-trip that yields a queue.
    GpuDevice = 2,
    /// Downlevel-capability read, device-lost callback, context assembly.
    GpuContextTail = 3,
    /// System font discovery, face parse, and ASCII prewarm on the font thread; runs in
    /// parallel with the `Gpu*` legs and is not in the exclusive sum.
    FontThread = 4,
    /// GPU leg wait time at the font-thread join.
    FontJoin = 5,
    /// The main cell shader module (one WGSL parse/compile submission).
    PipeShader = 6,
    /// Uniform buffer/bind-group and glyph-atlas layout + sampler.
    PipeUniformAtlas = 7,
    /// The cell render pipelines a cold build creates (see [`cell_pipeline_ns`]).
    PipeCell = 8,
    /// Blit shader, layouts, NEAREST sampler, invert uniform.
    PipeBlit = 9,
    /// Tray shader, layouts, LINEAR sampler, placement uniform.
    PipeTray = 10,
    /// Bloom layouts, sampler, uniform and render pipeline.
    PipeBloom = 11,
    /// Static vertex/index buffer upload.
    PipeVertexBuffers = 12,
    /// `from_parts` struct assembly after the last resource is built.
    PipeTail = 13,
    /// The whole of `from_parts`; parent of every `Pipe*` leg above.
    PipeTotal = 14,
}

/// How many slots [`LEG_NS`] carries (`Leg::PipeTotal as usize + 1`; a test pins it).
pub const LEG_COUNT: usize = 15;

/// Number of cell render pipelines built and timed during cold startup.
pub const CELL_PIPELINE_COUNT: usize = 3;

/// Names of the [`CELL_PIPELINE_COUNT`] slots in build order.
pub const CELL_PIPELINE_NAMES: [&str; CELL_PIPELINE_COUNT] = ["bg", "glyph", "color_glyph"];

static LEG_NS: [AtomicU64; LEG_COUNT] = [const { AtomicU64::new(0) }; LEG_COUNT];
static CELL_PIPELINE_NS: [AtomicU64; CELL_PIPELINE_COUNT] =
    [const { AtomicU64::new(0) }; CELL_PIPELINE_COUNT];

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

/// Time `build` as `leg` and return its value. The scoped form of [`record`].
#[cfg(wgpu_arm)]
pub(crate) fn timed<T>(leg: Leg, build: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let built = build();
    record(leg, started.elapsed());
    built
}

/// Records the elapsed time since `since` for cell pipeline `index` and returns `Instant::now()`.
#[cfg(wgpu_arm)]
pub(crate) fn split_cell_pipeline(index: usize, since: Instant) -> Instant {
    let now = Instant::now();
    if let Some(slot) = CELL_PIPELINE_NS.get(index) {
        let _ = slot.compare_exchange(
            0,
            slot_ns(now.duration_since(since)),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
    now
}

/// Reads the nanosecond duration recorded for `leg` (`0` if unset).
#[inline]
pub fn leg_ns(leg: Leg) -> u64 {
    LEG_NS[leg as usize].load(Ordering::Acquire)
}

/// Read the per-cell-pipeline split, in [`CELL_PIPELINE_NAMES`] order.
#[inline]
pub fn cell_pipeline_ns() -> [u64; CELL_PIPELINE_COUNT] {
    std::array::from_fn(|index| CELL_PIPELINE_NS[index].load(Ordering::Relaxed))
}

static EFFECT_BUILDS: AtomicU64 = AtomicU64::new(0);
static EFFECT_BUILD_NS: AtomicU64 = AtomicU64::new(0);
static EFFECT_BUILT_MASK: AtomicU64 = AtomicU64::new(0);

/// Book one demand-built effect pipeline (slot `index`) into the ledger.
#[cfg(wgpu_arm)]
pub(crate) fn record_effect_build(index: usize, elapsed: Duration) {
    EFFECT_BUILDS.fetch_add(1, Ordering::Relaxed);
    EFFECT_BUILD_NS.fetch_add(slot_ns(elapsed), Ordering::Relaxed);
    if index < u64::BITS as usize {
        EFFECT_BUILT_MASK.fetch_or(1u64 << index, Ordering::Relaxed);
    }
}

/// Returns `(count, nanoseconds, slot_bitmask)` of demand-built effect pipelines.
#[inline]
pub fn effect_build_ledger() -> (u64, u64, u64) {
    (
        EFFECT_BUILDS.load(Ordering::Relaxed),
        EFFECT_BUILD_NS.load(Ordering::Relaxed),
        EFFECT_BUILT_MASK.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::{Duration, slot_ns};

    #[test]
    fn slot_ns_avoids_the_sentinel_and_saturates() {
        assert_eq!(slot_ns(Duration::ZERO), 1);
        assert_eq!(slot_ns(Duration::from_nanos(7)), 7);
        assert_eq!(slot_ns(Duration::MAX), u64::MAX);
    }
}
