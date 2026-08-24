// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! COLD-BUILD SUB-PHASE PROBE — the inside of the one startup phase that had no
//! drill-down.
//!
//! The frontend's startup ledger (`aterm-gui::metrics`) partitions
//! Rust-main → first-present into exclusive phases, and its `backend_finalize`
//! phase is a single `handle.join()` on the backend-build worker. Everything the
//! worker does — wgpu instance/adapter/device acquisition, the parallel font
//! thread, and every render pipeline — therefore collapses into ONE opaque
//! number. That number was recorded at 300.83 ms median on macOS on 2026-07-30
//! and read as the largest phase of startup; three separate optimizations were
//! proposed inside it and all three were refused for being unsizeable.
//!
//! **THE 300 ms PHASE IS NOT THERE, AND THIS PROBE IS WHAT PROVED IT.** With
//! the drill-down wired, `backend_finalize` measures **0.01 ms median, 0.02 ms
//! max over 40 fresh processes**, and the worker had already finished before
//! the join was reached in EVERY sample — the build's 35.76 ms of real work is
//! entirely hidden under window creation
//! (`docs/measured/arena/2026-08-23-start-backend-finalize-drilldown-dev-smoke.md`;
//! Apple M5 Max, **DEV-SMOKE / NON-PUBLISHABLE**, single-arm). The 300.83 ms
//! figure comes from the 2026-07-30 font-seal A/B in the same lane and the same
//! class, and is superseded: do not size work against it again.
//!
//! This module is the ns-resolution split of that construction, recorded WHERE
//! the work happens (this crate) and read back by the frontend, which cannot see
//! in here. It publishes nothing itself: `aterm-gui` folds these legs into its
//! `metrics` ledger beside the phases they explain.
//!
//! The module keeps earning its place regardless of how small the phase turned
//! out to be. A phase nobody can see is a phase people invent numbers for —
//! this one collected three — and the legs below are the standing instrument
//! that would catch it growing teeth for real: a slower adapter, a fatter
//! pipeline set, or a launch path fast enough that the join starts waiting
//! again. This probe reports the cost; it does not assert the cost is large.
//!
//! ## Rules, matching the startup ledger's own conventions
//!
//! * **Nanoseconds.** Every slot is a `u64` ns duration, like every other
//!   startup number; the wire converts to ms once, at the emitter.
//! * **First-write-wins.** A process has ONE cold build. A later rebuild — a
//!   GPU-loss recovery, a live font/theme reload that recreates a context, a
//!   second `GpuRenderer` in a test — must never overwrite the launch timeline,
//!   so [`record`] admits a leg only while its slot is still zero.
//! * **Zero means UNSET, never "instant".** A genuinely sub-ns leg records as
//!   1 ns so the sentinel stays unambiguous, and a leg that never ran (the CPU
//!   fallback path takes no GPU leg at all) reads 0 and makes the frontend's
//!   `startup_gpu_valid` false rather than fabricating a partition.
//! * **Durations, not stamps.** The legs run on two threads (the worker and the
//!   font thread it spawns) with no shared epoch, and a duration needs none.
//!   The one cross-thread QUESTION — how much of the worker was already done
//!   when the join was reached — is answered on the frontend side, where both
//!   ends of that interval are `Instant`s on one clock.
//!
//! Cost: one `Instant::now()` pair and one relaxed compare-exchange per leg,
//! ~30 for the entire process lifetime, none of them on a frame path.

use std::sync::atomic::{AtomicU64, Ordering};

use web_time::{Duration, Instant};

/// The exclusive legs of one cold GPU-renderer build.
///
/// Ordered as the worker executes them. [`Leg::FontThread`] is the one
/// PARALLEL leg — it runs on the font thread spawned by
/// `GpuRenderer::new_with_family` and overlaps the four GPU legs above it, so it
/// is deliberately NOT part of the exclusive sum; [`Leg::FontJoin`] is the
/// exclusive slice, the wait the GPU leg actually paid for it.
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
    /// The GPU leg's wait at the font-thread join — the EXCLUSIVE cost of the
    /// font work, i.e. however much of [`Leg::FontThread`] did not fit under
    /// the GPU legs.
    FontJoin = 5,
    /// The main cell shader module (one WGSL parse/compile submission).
    PipeShader = 6,
    /// Uniform buffer/bind-group and glyph-atlas layout + sampler.
    PipeUniformAtlas = 7,
    /// All twelve cell render pipelines (see [`cell_pipeline_ns`] for the
    /// per-pipeline split).
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

/// How many cell render pipelines `build_cell_pipelines` builds, each timed
/// individually. Twelve of the thirteen pipelines a cold build creates (the
/// thirteenth is the bloom composite, under [`Leg::PipeBloom`]); at a vertex and
/// a fragment entry point apiece that is 24 of the 26 cold shader compiles.
pub const CELL_PIPELINE_COUNT: usize = 12;

/// Names of the [`CELL_PIPELINE_COUNT`] slots, in build order. Read by the
/// frontend so the wire can label the split without duplicating the order.
pub const CELL_PIPELINE_NAMES: [&str; CELL_PIPELINE_COUNT] = [
    "bg",
    "cursor_blend",
    "glyph",
    "color_glyph",
    "glow_add",
    "rain_glow",
    "rain_glow_over",
    "fire_add",
    "fire_over",
    "deco_over",
    "deco_add",
    "sprite_over",
];

static LEG_NS: [AtomicU64; LEG_COUNT] = [const { AtomicU64::new(0) }; LEG_COUNT];
static CELL_PIPELINE_NS: [AtomicU64; CELL_PIPELINE_COUNT] =
    [const { AtomicU64::new(0) }; CELL_PIPELINE_COUNT];

/// Clamp a measured duration into the slot encoding: saturate at `u64::MAX` ns
/// (295 years — unreachable, but a cast must not wrap) and floor at 1 ns so a
/// recorded leg is never mistaken for an unset one.
fn slot_ns(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Record `elapsed` for `leg`, first write wins.
///
/// A slot that is already non-zero belongs to the cold build and is left alone;
/// see the module docs for why later rebuilds must not overwrite it.
pub fn record(leg: Leg, elapsed: Duration) {
    let _ = LEG_NS[leg as usize].compare_exchange(
        0,
        slot_ns(elapsed),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// Time `build` as `leg` and return its value. The scoped form of [`record`].
pub(crate) fn timed<T>(leg: Leg, build: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let built = build();
    record(leg, started.elapsed());
    built
}

/// Close the cell pipeline at `index` against `since` and return the stamp the
/// NEXT pipeline should close against.
///
/// A running split rather than a wrapper around each `create_render_pipeline`
/// call: the twelve descriptors are 40-line literals, and threading a closure
/// through each one would restate every one of them.
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

#[cfg(test)]
mod tests {
    use super::{
        CELL_PIPELINE_COUNT, CELL_PIPELINE_NAMES, Duration, LEG_COUNT, Leg, slot_ns,
    };

    #[test]
    fn leg_count_covers_every_variant() {
        assert_eq!(LEG_COUNT, Leg::PipeTotal as usize + 1);
    }

    #[test]
    fn cell_pipeline_names_match_slot_count() {
        assert_eq!(CELL_PIPELINE_NAMES.len(), CELL_PIPELINE_COUNT);
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
