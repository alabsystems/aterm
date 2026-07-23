// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-scene` — the pure, deterministic animation kit. Its live role is the shared
//! **RGB mixing / easing math** ([`mix_rgb`], [`smoothstep`], [`lerp`]) and the sprite
//! **[`atlas`]** ([`Tile`]/[`Atlas`]) that the `aterm-effects` aurora/sparkle/cat engine
//! builds on.
//!
//! It ALSO carries a full "Living Panels" scene framework (below) that has **no live
//! consumer** — the GUI Scene feature was removed (it never shipped art). The framework
//! is retained as dead code pending a possible rewrite; nothing depends on it.
//!
//! ## Three layers, cleanly separated
//!
//! 1. **Signals** ([`signal`]) — a normalized, art-agnostic telemetry bus. The host
//!    samples real sources (CPU/mem/net/fps/app-fed streams) into a [`SignalSet`]; the
//!    art only ever reads this bus, never hardware. Unavailable signals stay *honest*
//!    (a missing GPU counter is `None`, never a fake 0).
//! 2. **Bindings** ([`bind`]) — a data-driven map from abstract [`Drive`]s (energy,
//!    crowd, arrivals, …) onto signal sources, so "which stat drives which behaviour"
//!    is configuration, not code. [`Binding::resolve`] turns a [`SignalSet`] into
//!    [`Drives`].
//! 3. **Scenes** ([`scene`]) — host-side animators shaped exactly like aterm's
//!    `cursor_glow` aurora: [`Scene::tick`] advances bounded entity pools from an
//!    *injected* `dt` (no wall clock → unit-testable, deterministic given a seed), and
//!    [`Scene::emit`] produces a [`SceneFrame`] of axis-aligned sprite quads (sampled
//!    from a procedurally-baked RGBA8 [`Atlas`]) plus additive light. The renderer is a
//!    dumb, parity-safe consumer; ALL art lives here.
//!
//! ## Engineering invariants (the bar this crate holds itself to)
//!
//! - **Deterministic.** Same seed + same `dt`/`Drives` stream ⇒ byte-identical frames.
//!   Randomness is a seedable xorshift ([`Rng`]); there is no `Instant`, no global state.
//! - **Bounded.** Every entity pool has a hard `CAP`; spawn is refused at the cap and
//!   the emitted quad count is bounded — defended by a `ty_model!` (Tier-0) and a
//!   pure-Rust property test (Tier-1, [`meadow`] tests).
//! - **Panic-free.** No `unwrap`/`expect`/indexing that can trap on the tick/emit path;
//!   all arithmetic is saturating/clamped (the renderer is a hot path).
//! - **Idle-to-zero.** [`Scene::is_active`] reports `false` once nothing is moving, so
//!   the host can return the event loop to 0% idle (the `cursor_glow` battery contract).

#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod atlas;
pub mod bind;
#[cfg(feature = "bridge")]
pub mod bridge;
pub mod placeholder;
pub mod raster;
pub mod registry;
pub mod scene;
pub mod signal;
pub mod vector;

pub use atlas::{Atlas, AtlasRect, Sprite, Tile};
pub use bind::{Binding, Drive, Drives, Source};
pub use placeholder::Placeholder;
pub use raster::{Canvas, composite, sample};
pub use registry::{build_scene, default_binding, scene_names};
pub use scene::{Env, LocalSprite, Palette, Scene, SceneFrame, TextPulse};
pub use signal::{AppSignal, Sig, SignalKey, SignalSet};
pub use vector::{fill_path, fill_path_fixed, parse_path, PathCmd, PathSeg, PathTransform};

// =====================================================================================
// Deterministic randomness + frame-rate-independent math (the shared kinematics kit).
// =====================================================================================

/// A small, fast, fully-deterministic xorshift32 PRNG — the same generator family the
/// `cursor_glow` aurora uses, so scenes are reproducible and *seedable* ("inputs seed
/// the generation and animation"). Never returns a degenerate stream: a `0` seed is
/// remapped to the golden-ratio constant.
#[derive(Clone, Copy, Debug)]
pub struct Rng(u32);

impl Rng {
    /// Seed the generator. `0` is remapped (xorshift's fixed point) so the stream is
    /// always non-degenerate.
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }

    /// Next raw 32-bit value (xorshift13/17/5).
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform `f32` in `[0, 1)` (24-bit mantissa precision).
    pub fn unit(&mut self) -> f32 {
        // 2⁻²⁴, folded at compile time. Multiplying by the exact reciprocal of a
        // power of two is bit-identical to dividing by it for every input, and it
        // keeps the hot path free of a division (and of its proof obligation).
        const SCALE: f32 = 1.0 / (1u32 << 24) as f32;
        (self.next_u32() >> 8) as f32 * SCALE
    }

    /// Uniform `f32` in `[lo, hi)` (or the empty/degenerate range → `lo`).
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// A Bernoulli trial: `true` with probability `p` (clamped to `[0, 1]`).
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < clampf(p, 0.0, 1.0)
    }

    /// A signed jitter in `[-mag, mag)`.
    pub fn signed(&mut self, mag: f32) -> f32 {
        self.range(-mag, mag)
    }
}

/// Clamp `v` into `[lo, hi]`. `lo <= hi` is the caller's contract; if violated the
/// result is `lo` (we clamp high first, then low), which is still finite and bounded.
#[must_use]
pub fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    let v = if v > hi { hi } else { v };
    if v < lo { lo } else { v }
}

/// Linear interpolation `a → b` by `t` (unclamped; callers pass `t ∈ [0,1]`).
#[must_use]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Smoothstep easing of `t ∈ [0,1]` (the classic `3t² − 2t³`).
#[must_use]
pub fn smoothstep(t: f32) -> f32 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// **Frame-rate-independent** exponential approach of `cur` toward `target`. `tau` is
/// the time constant in seconds (smaller = snappier); `dt` the frame delta. Equivalent
/// to `cur + (target-cur)*(1 - e^{-dt/tau})`, so the *visual* settling time is identical
/// at 30 fps or 240 fps — the key to "perfectly smooth" transitions that never stutter
/// on a missed frame. A non-positive `tau` snaps to `target`.
#[must_use]
pub fn smooth(cur: f32, target: f32, tau: f32, dt: f32) -> f32 {
    if tau <= 0.0 || dt <= 0.0 {
        return if dt <= 0.0 { cur } else { target };
    }
    let k = 1.0 - (-(dt / tau)).exp();
    cur + (target - cur) * k
}

/// Per-channel linear blend of two packed `0x00RRGGBB` colours by `t ∈ [0,1]` (the
/// shared gradient/daylight helper for every scene). `t` is clamped.
#[must_use]
pub fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = clampf(t, 0.0, 1.0);
    // Each call site shifts by a *constant* (16/8/0) and hands the closure the
    // pre-shifted channels, so every shift amount is trivially in range — same
    // math as shifting inside the closure, but provably panic-free.
    let ch = |ca: u32, cb: u32| {
        let ca = (ca & 0xff) as f32;
        let cb = (cb & 0xff) as f32;
        ((ca + (cb - ca) * t) + 0.5) as u32 & 0xff
    };
    (ch(a >> 16, b >> 16) << 16) | (ch(a >> 8, b >> 8) << 8) | ch(a, b)
}

/// A 2-D point/vector in scene-local pixels.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct V2 {
    pub x: f32,
    pub y: f32,
}

impl V2 {
    /// Construct a vector.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in scene-local pixels (`x,y` = top-left).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Construct a rectangle.
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// A rectangle centered at `(cx, cy)` with size `w × h`.
    #[must_use]
    pub fn centered(cx: f32, cy: f32, w: f32, h: f32) -> Self {
        Self {
            x: cx - w * 0.5,
            y: cy - h * 0.5,
            w,
            h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::new(12345);
        let mut b = Rng::new(12345);
        for _ in 0..10_000 {
            let x = a.unit();
            assert_eq!(x, b.unit(), "same seed ⇒ identical stream");
            assert!((0.0..1.0).contains(&x), "unit() in [0,1): {x}");
        }
    }

    #[test]
    fn rng_zero_seed_is_non_degenerate() {
        let mut r = Rng::new(0);
        // Must not get stuck at 0 (xorshift's fixed point).
        assert_ne!(r.next_u32(), 0);
        assert_ne!(r.next_u32(), 0);
    }

    #[test]
    fn smooth_is_framerate_independent() {
        // One 1/60 step vs four 1/240 steps should land at (nearly) the same place.
        let one = smooth(0.0, 1.0, 0.2, 1.0 / 60.0);
        let mut four = 0.0;
        for _ in 0..4 {
            four = smooth(four, 1.0, 0.2, 1.0 / 240.0);
        }
        assert!((one - four).abs() < 1e-3, "60fps {one} vs 240fps {four}");
    }

    #[test]
    fn smooth_converges_without_overshoot() {
        let mut v = 0.0;
        for _ in 0..600 {
            v = smooth(v, 1.0, 0.1, 1.0 / 60.0);
            assert!(v <= 1.0 + 1e-6, "never overshoots: {v}");
        }
        assert!(v > 0.99, "converges: {v}");
    }

    #[test]
    fn clamp_and_lerp_are_sane() {
        assert_eq!(clampf(5.0, 0.0, 1.0), 1.0);
        assert_eq!(clampf(-5.0, 0.0, 1.0), 0.0);
        assert_eq!(clampf(0.5, 0.0, 1.0), 0.5);
        assert!((lerp(0.0, 10.0, 0.25) - 2.5).abs() < 1e-6);
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
    }
}
