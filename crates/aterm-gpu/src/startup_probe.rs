// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold-build sub-phase timing: the breakdown of the frontend's
//! `backend_finalize` startup phase.
//!
//! The frontend's startup ledger (`aterm-gui::metrics`) splits Rust-main to
//! first-present into exclusive phases; `backend_finalize` is one
//! `handle.join()` on the backend-build worker, so the worker's work (wgpu
//! instance/adapter/device, the parallel font thread, render pipelines) is one
//! opaque number there. Normally the worker has finished before the join, its
//! work hidden under window creation. These legs record the split where the
//! work happens; `aterm-gui` folds them into its ledger. This crate publishes
//! nothing itself.
//!
//! ## Rules
//!
//! * Nanoseconds: every slot is a `u64` duration; the wire converts to ms.
//! * First write wins: a process has one cold build, so a later rebuild (GPU
//!   loss recovery, a context recreated on reload, a second renderer in a test)
//!   never overwrites it; [`record`] admits a leg only while its slot is zero.
//! * Zero means unset: a sub-ns leg records as 1 ns, and a leg that never ran
//!   (the CPU fallback) reads 0, making the frontend's `startup_gpu_valid`
//!   false.
//! * Durations, not stamps: legs run on two threads with no shared epoch. How
//!   much of the worker was done when the join was reached is computed on the
//!   frontend side, where both ends share a clock.
//!
//! Cost: one `Instant::now()` pair and one relaxed compare-exchange per leg,
//! about 30 per process, none on a frame path.

use std::sync::atomic::{AtomicU64, Ordering};

use aterm_time::Duration;
#[cfg(wgpu_arm)]
use aterm_time::Instant;

/// The exclusive legs of one cold GPU-renderer build, in execution order.
/// [`Leg::FontThread`] runs in parallel on the font thread spawned by
/// `GpuRenderer::new_with_family` and is not part of the exclusive sum;
/// [`Leg::FontJoin`] is the wait the GPU leg paid for it.
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
    /// PARALLEL: system font discovery, face parse, and the ASCII prewarm, on
    /// the font thread. Overlaps every GPU leg; not in the exclusive sum.
    FontThread = 4,
    /// The GPU leg's wait at the font-thread join: the part of
    /// [`Leg::FontThread`] that did not fit under the GPU legs.
    FontJoin = 5,
    /// The main cell shader module (one WGSL parse/compile submission).
    PipeShader = 6,
    /// Uniform buffer/bind-group and glyph-atlas layout + sampler.
    PipeUniformAtlas = 7,
    /// The cell render pipelines a cold build creates (see
    /// [`cell_pipeline_ns`]). Effect pipelines are built on first use
    /// (`renderer::EffectPipelines`).
    PipeCell = 8,
    /// Blit shader, layouts, NEAREST sampler, invert uniform.
    PipeBlit = 9,
    /// Tray shader, layouts, LINEAR sampler, placement uniform.
    PipeTray = 10,
    /// Bloom layouts, sampler, uniform, and the thirteenth render pipeline.
    PipeBloom = 11,
    /// Static vertex/index buffer upload.
    PipeVertexBuffers = 12,
    /// `from_parts` struct assembly after the last resource is built.
    PipeTail = 13,
    /// The whole of `from_parts` — the "pipeline construction" number. Parent
    /// of every `Pipe*` leg above.
    PipeTotal = 14,
}

/// How many slots [`LEG_NS`] carries. Kept as a `const` (not `Leg::PipeTotal as
/// usize + 1`) so the array length is visible at its definition.
pub const LEG_COUNT: usize = 15;

/// Cell render pipelines `build_cell_pipelines` builds and times individually:
/// background fill, mono glyphs and colour emoji, the only ones a default frame
/// binds. Effect pipelines are built on demand (`renderer::EffectPipelines`);
/// if this grows, launch is paying for effect pipelines again.
pub const CELL_PIPELINE_COUNT: usize = 3;

/// Names of the [`CELL_PIPELINE_COUNT`] slots, in build order. Read by the
/// frontend so the wire can label the split without duplicating the order.
pub const CELL_PIPELINE_NAMES: [&str; CELL_PIPELINE_COUNT] = ["bg", "glyph", "color_glyph"];

static LEG_NS: [AtomicU64; LEG_COUNT] = [const { AtomicU64::new(0) }; LEG_COUNT];
static CELL_PIPELINE_NS: [AtomicU64; CELL_PIPELINE_COUNT] =
    [const { AtomicU64::new(0) }; CELL_PIPELINE_COUNT];

/// Clamp a measured duration into the slot encoding: saturate at `u64::MAX` ns
/// and floor at 1 ns so a recorded leg is never read as unset.
fn slot_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Record `elapsed` for `leg`, first write wins (see the module docs).
pub fn record(leg: Leg, elapsed: Duration) {
    let _ = LEG_NS[leg as usize].compare_exchange(
        0,
        slot_ns(elapsed),
        Ordering::Relaxed,
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

/// Close the cell pipeline at `index` against `since` and return the stamp the
/// next pipeline closes against. A running split, since wrapping each
/// descriptor literal in a closure would restate them all.
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

/// Read one leg. 0 means the leg never ran in this process (see the module
/// docs' UNSET rule).
pub fn leg_ns(leg: Leg) -> u64 {
    LEG_NS[leg as usize].load(Ordering::Relaxed)
}

/// Read the per-cell-pipeline split, in [`CELL_PIPELINE_NAMES`] order.
pub fn cell_pipeline_ns() -> [u64; CELL_PIPELINE_COUNT] {
    std::array::from_fn(|index| CELL_PIPELINE_NS[index].load(Ordering::Relaxed))
}

/// Effect pipelines this process has compiled on demand, and the wall time
/// spent. Process-global, since a fail-soft rebuild can create a second
/// renderer. Accumulates (unlike the legs), because an effect enabled later is
/// worth seeing. A default launch that draws no effect reads zero.
static EFFECT_BUILDS: AtomicU64 = AtomicU64::new(0);
static EFFECT_BUILD_NS: AtomicU64 = AtomicU64::new(0);
/// Which effect slots were built, as a bitmask over
/// `renderer::EffectPipeline as usize`, so a reading says what was paid for.
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

/// `(count, nanoseconds, slot bitmask)` of demand-built effect pipelines;
/// `(0, 0, 0)` for a launch with default settings.
pub fn effect_build_ledger() -> (u64, u64, u64) {
    (
        EFFECT_BUILDS.load(Ordering::Relaxed),
        EFFECT_BUILD_NS.load(Ordering::Relaxed),
        EFFECT_BUILT_MASK.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::{Duration, LEG_COUNT, Leg, slot_ns};

    #[test]
    fn leg_count_covers_every_variant() {
        assert_eq!(LEG_COUNT, Leg::PipeTotal as usize + 1);
    }

    #[test]
    fn a_recorded_leg_is_never_the_unset_sentinel() {
        assert_eq!(slot_ns(Duration::ZERO), 1);
        assert_eq!(slot_ns(Duration::from_nanos(7)), 7);
    }

    #[test]
    fn an_unrepresentable_duration_saturates_instead_of_wrapping() {
        assert_eq!(slot_ns(Duration::MAX), u64::MAX);
    }
}
