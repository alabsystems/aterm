// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EDR ("HDR glow") present math — M3 phase B.
//!
//! When config `hdr_glow` is on and the swapchain is `Rgba16Float` tagged
//! extended-linear-sRGB, the GPU present blit DECODES the offscreen's
//! sRGB-encoded bytes to linear light (the GRID stays reference-white SDR), and
//! a second additive pass re-emits the LUMEN cursor-aurora quads with values
//! ABOVE 1.0 — bounded by the screen's queried EDR headroom
//! (`NSScreen.maximumExtendedDynamicRangeColorComponentValue`). This module is
//! the PURE half of that pipeline: every clamp/encode the WGSL performs has its
//! float twin here, so the two laws the feature pins are machine-checkable
//! without a GPU:
//!
//! # Invariants (checked)
//!
//! * **Grid clamp law** — the non-additive (blit) stream never exceeds 1.0 in
//!   the float pipeline: [`hdr_grid_encode`] maps every byte into `[0, 1]`
//!   (exhaustive 256-byte sweep below; the WGSL twin adds the same clamp).
//! * **Additive clamp law** — the additive stream's fragment output is bounded
//!   by the queried EDR headroom at encode: [`clamp_add`] lands in
//!   `[0, max(headroom, 0)]` for every `f32` input including NaN/∞ (the
//!   lattice tests below, over the special values that decide `min`/`max`),
//!   and [`sanitize_edr_max`] keeps `headroom = edr_max − 1 ≥ 0`. Together with the
//!   grid law the presented pixel is `≤ 1.0 + headroom = edr_max` under the
//!   One/One additive blend.
//!
//! The BOOLEAN gate deciding when any of this runs lives in
//! `aterm-gpu::format_plan` (`hdr_present_plan`), whose abstract twin is the
//! `HdrPresentGate` ty model (aterm-spec `derive.rs`); Tier-1 is the exhaustive
//! enumeration in aterm-gpu `tests/hdr_gate.rs`.

/// Upper cap for a sanitized EDR maximum. Real panels report ≤ 16 (Pro Display
/// XDR); the cap only bounds a corrupt query so the headroom stays finite.
pub const EDR_MAX_CAP: f32 = 100.0;

/// Linear-light BOOST applied to the aurora's premultiplied colour in the EDR
/// additive pass: `1.0` re-adds the glow's own energy in linear light (up to a
/// doubling where the panel has headroom) — bright enough to read as light,
/// conservative enough to never look clipped.
pub const HDR_GLOW_BOOST: f32 = 1.0;

/// sRGB-encoded channel (0..=1) → linear light: the standard piecewise decode,
/// SAME constants as the WGSL `s2l` (renderer.rs) and the CPU blend helpers, so
/// the CPU twin and the shader agree float-for-float. Input is clamped to
/// `[0, 1]` first (total on every f32; NaN clamps to 0).
#[must_use]
pub fn srgb_channel_to_linear(c: f32) -> f32 {
    let c = if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) };
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// GRID (non-additive) HDR encode: what the present blit writes for one stored
/// offscreen byte on the `Rgba16Float` extended-linear swapchain.
///
/// # Invariant (checked)
/// `0.0 <= hdr_grid_encode(b) <= 1.0` for every byte, with
/// `hdr_grid_encode(255) == 1.0` — SDR reference white maps EXACTLY to EDR
/// reference white, so the grid renders identically to the SDR path and only
/// the additive pass may exceed it. Exhaustively swept (all 256 bytes) in the
/// tests below; the WGSL twin (`fs_blit`'s `b.hdr` arm) applies the same
/// decode + clamp.
#[must_use]
pub fn hdr_grid_encode(b: u8) -> f32 {
    srgb_channel_to_linear(f32::from(b) / 255.0).clamp(0.0, 1.0)
}

/// Sanitize a queried Windows SDR-white scale (`SDR-white nits / 80`, the scRGB
/// reference): non-finite or `< 1.0` (the unset per-window default `0.0`, macOS,
/// an SDR desktop) means NO scaling → `1.0`. The GPU side
/// (`WindowGpu::sdr_white_scale`) applies the same rule, so the float twin and
/// the uniform the shader reads agree.
#[must_use]
pub fn sanitize_sdr_white_scale(v: f32) -> f32 {
    if v.is_finite() && v >= 1.0 { v } else { 1.0 }
}

/// scRGB PRESENT of one stored byte — grid and remainder band alike — on the
/// Windows `Rgba16Float` swapchain: [`hdr_grid_encode`] scaled to the desktop's
/// SDR-white. The float twin of BOTH `fs_blit` `hdr` arms (content and bands).
///
/// # Invariant (measured on glass, pinned below)
/// On a Windows HDR desktop DWM composes every SDR-drawn pixel (GDI, the
/// caption tint) as `srgb_piecewise_to_linear(byte) * SDR-white/80` — measured
/// exactly `3.0 ×` on a 240-nit SDR-white desktop, the sRGB piecewise curve, not
/// a 2.2 power. This function is therefore the value that makes an aterm byte
/// compose IDENTICALLY to the same byte drawn by DWM. `scale 1.0` is exactly
/// [`hdr_grid_encode`]; byte `255` maps to exactly `scale`; every byte lands in
/// `[0, scale]`. (A GDI screen grab — `BitBlt`/`CopyFromScreen` — reads an f16
/// swapchain back at 80 nits == white, i.e. `l2s(value)` with NO `/scale`, so it
/// reports this present `scale×` lifted in linear light while reading SDR
/// windows byte-exact; verify the EDR present with an FP16 capture instead.)
#[must_use]
pub fn scrgb_present_channel(b: u8, sdr_white_scale: f32) -> f32 {
    hdr_grid_encode(b) * sanitize_sdr_white_scale(sdr_white_scale)
}

/// Sanitize a queried `NSScreen.maximumExtendedDynamicRangeColorComponentValue`:
/// non-finite or `< 1.0` (including the unset per-window default `0.0`) means NO
/// headroom → `1.0`; a corrupt huge value is capped at [`EDR_MAX_CAP`].
///
/// # Invariant (checked)
/// `1.0 <= sanitize_edr_max(v) <= EDR_MAX_CAP` for every `f32`
/// (`sanitize_edr_boundaries`: NaN, ±∞, ±0, subnormal, the extremes).
#[must_use]
pub fn sanitize_edr_max(v: f32) -> f32 {
    if v.is_finite() && v >= 1.0 {
        v.min(EDR_MAX_CAP)
    } else {
        1.0
    }
}

/// The additive HEADROOM above SDR reference white: `sanitize(edr_max) - 1.0`.
/// `0.0` on an SDR panel (the boost pass then adds nothing and is skipped).
///
/// # Invariant (checked)
/// `0.0 <= additive_headroom(v) <= EDR_MAX_CAP - 1.0` for every `f32`.
#[must_use]
pub fn additive_headroom(edr_max: f32) -> f32 {
    sanitize_edr_max(edr_max) - 1.0
}

/// The additive clamp CORE: bound an (arbitrary) boosted linear emission `x`
/// to `[0, max(headroom, 0)]`. This is the exact op the EDR glow fragment
/// performs after its `s2l(colour) * boost` — factored transcendental-free so
/// the bound holds whatever the decode produced (NaN and ±∞ included:
/// `f32::min`/`max` return the other operand for NaN, so a NaN emission lands
/// on the headroom bound and a NaN headroom degrades to 0).
///
/// # Invariant (checked)
/// `0.0 <= clamp_add(x, h) <= max(h, 0)` for every pair, emission and
/// headroom alike, from the values that decide `f32::min`/`max`: NaN, ±∞,
/// ±0, ± the smallest subnormal, the smallest normal, ±1, ±`f32::MAX` and
/// ordinary values (`clamp_add_bounded_over_the_special_value_lattice`); and
/// through the encode for every channel byte
/// (`additive_encode_bounded_by_edr_lattice`, `additive_clamp_is_load_bearing`);
/// hence, over the One/One blend, presented pixel
/// `<= hdr_grid_encode(byte) + clamp_add(..) <= 1.0 + headroom = edr_max`.
#[must_use]
pub fn clamp_add(x: f32, headroom: f32) -> f32 {
    x.min(headroom.max(0.0)).max(0.0)
}

/// One channel of the EDR glow pass's fragment output: decode the
/// premultiplied sRGB-space aurora colour to linear, boost it, and clamp to the
/// headroom — the float twin of the WGSL `fs_hdr_glow`. The bound holds
/// REGARDLESS of what the decode/boost produce (see [`clamp_add`]).
#[must_use]
#[cfg(test)]
pub fn hdr_additive_encode(chan: f32, boost: f32, headroom: f32) -> f32 {
    clamp_add(srgb_channel_to_linear(chan) * boost, headroom)
}

/// Ceiling of the SDR glow-boost budget (raw swapchain code-value units,
/// `0..=1`): the swapchain-side additive crown on an SDR (non-f16) present may
/// add at most this much per channel, at full configured strength on a black
/// background. 0.35 ≈ +89/255 peak — bright enough to read as light, bounded
/// far below washout.
pub const SDR_GLOW_BUDGET_BASE: f32 = 0.35;

/// The SDR glow-boost BUDGET for a given theme background: how much additive
/// light (raw code-value units) the swapchain-side SDR crown may add per
/// channel. `bg_luma` is the theme background's relative luma in `0..=1`
/// (Rec.601 over the raw sRGB bytes — the same convention as the dark-caption
/// pick); `strength` is the config `cursor_glow_sdr_boost` knob.
///
/// The `(1 - luma)^2` rolloff makes light themes compute a sub-visible budget
/// (luma 0.9 → ≤ 0.35% of base, under 1/255) — additive light over a bright
/// background reads as washout, so it degrades itself away — while a dark
/// terminal (luma ≤ 0.1) keeps ~81% of base.
///
/// # Invariant
/// `0.0 <= sdr_glow_budget(l, s) <= SDR_GLOW_BUDGET_BASE` for ALL `f32` pairs
/// (NaN/±∞ included): both inputs are clamped through `min`/`max` chains whose
/// NaN semantics land on the finite bound, and the product of three `0..=1`
/// factors with the base cannot exceed the base. Unit-pinned below.
#[must_use]
// NOT `.clamp()`: `f32::clamp` PROPAGATES NaN, but the min/max ORDERING here is
// load-bearing fail-safe handling (`f32::min`/`max` return the OTHER operand for
// NaN) — see below. `clamp(NaN,0,1) == NaN` would poison the render path.
#[allow(clippy::manual_clamp)]
pub fn sdr_glow_budget(bg_luma: f32, strength: f32) -> f32 {
    // NaN directions are deliberate (`f32::min`/`max` return the OTHER operand
    // for a NaN input): luma clamps min-FIRST so NaN lands on 1.0 → rolloff 0 →
    // budget 0 (fail dark-off); strength clamps max-FIRST so NaN lands on 0.0
    // (fail off). Either way a poisoned input DISABLES the boost.
    let l = bg_luma.min(1.0).max(0.0);
    let s = strength.max(0.0).min(1.0);
    let rolloff = (1.0 - l) * (1.0 - l);
    clamp_add(SDR_GLOW_BUDGET_BASE * s * rolloff, SDR_GLOW_BUDGET_BASE)
}

/// Relative luma (`0..=1`, Rec.601 on the raw sRGB bytes) of a packed
/// `0x00RRGGBB` colour — the SDR glow budget's background-darkness input.
#[must_use]
pub fn packed_luma(bg: u32) -> f32 {
    let r = ((bg >> 16) & 0xff) as f32;
    let g = ((bg >> 8) & 0xff) as f32;
    let b = (bg & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GRID CLAMP LAW, exhaustively: every one of the 256 storable bytes lands
    /// in [0, 1] — the non-additive stream can NEVER exceed SDR reference white
    /// in the float pipeline. Monotone, and the endpoints are exact (0 → 0.0,
    /// 255 → 1.0: SDR white == EDR reference white, the tone anchor).
    #[test]
    fn grid_encode_bounded_and_anchored() {
        let mut prev = -1.0f32;
        for b in 0u16..=255 {
            let v = hdr_grid_encode(b as u8);
            assert!((0.0..=1.0).contains(&v), "byte {b} escaped [0,1]: {v}");
            assert!(v >= prev, "byte {b}: decode must be monotone");
            prev = v;
        }
        assert_eq!(hdr_grid_encode(0), 0.0, "black anchors at 0.0");
        assert_eq!(
            hdr_grid_encode(255),
            1.0,
            "SDR white anchors EXACTLY at 1.0"
        );
        // NON-VACUITY: the law has teeth — a boosted ADDITIVE emission from the
        // same byte DOES exceed 1.0 given headroom, so the grid/additive split
        // is what keeps the grid at reference white, not the inputs.
        assert!(
            hdr_grid_encode(255) + hdr_additive_encode(1.0, HDR_GLOW_BOOST, 1.0) > 1.0,
            "an EDR emission over white must exceed 1.0 (else the feature is vacuous)"
        );
    }

    /// scRGB PRESENT (grid AND bands): scale 1.0 is the grid encode exactly; white
    /// maps to exactly the scale; every byte lands in [0, scale]; a poisoned scale
    /// degrades to 1.0 (no scaling). Anchored on the on-glass measurement: byte
    /// `0x11` on a 240-nit SDR-white desktop composes at linear 0.01682 — the
    /// value DWM itself draws for the same byte.
    #[test]
    fn scrgb_present_scales_reference_white_exactly() {
        for b in 0u16..=255 {
            let b = b as u8;
            assert_eq!(scrgb_present_channel(b, 1.0), hdr_grid_encode(b));
            for &s in &[1.0f32, 1.5, 3.0, 12.5] {
                let v = scrgb_present_channel(b, s);
                assert!(
                    (0.0..=s).contains(&v),
                    "byte {b} scale {s} escaped [0,{s}]: {v}"
                );
            }
        }
        assert_eq!(
            scrgb_present_channel(255, 3.0),
            3.0,
            "SDR white composes EXACTLY at the SDR-white scale"
        );
        assert_eq!(scrgb_present_channel(0, 3.0), 0.0, "black stays black");
        for &bad in &[
            0.0f32,
            0.5,
            -3.0,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            assert_eq!(
                sanitize_sdr_white_scale(bad),
                1.0,
                "scale {bad} must degrade to 1.0"
            );
        }
        let v = scrgb_present_channel(0x11, 3.0);
        assert!(
            (v - 0.016_82).abs() < 1e-4,
            "byte 0x11 at 3.0x must match DWM's measured 0.01682: {v}"
        );
    }

    /// ADDITIVE CLAMP LAW over a deliberate lattice: all 256 channel bytes ×
    /// boosts (incl. 0 and a corrupt huge one) × edr_max values (SDR 1.0, real
    /// panel 1.6/16.0, corrupt 0.0/-3.0/NaN/∞): the encode never exceeds the
    /// sanitized headroom, so blit(≤1) + additive ≤ sanitize(edr_max).
    #[test]
    fn additive_encode_bounded_by_edr_lattice() {
        let boosts = [0.0f32, 0.5, HDR_GLOW_BOOST, 4.0, 1.0e30];
        let edrs = [
            f32::NEG_INFINITY,
            -3.0,
            0.0,
            1.0,
            1.6,
            16.0,
            1.0e9,
            f32::INFINITY,
            f32::NAN,
        ];
        for b in 0u16..=255 {
            let chan = b as f32 / 255.0;
            for &boost in &boosts {
                for &edr in &edrs {
                    let h = additive_headroom(edr);
                    assert!(
                        (0.0..=EDR_MAX_CAP - 1.0).contains(&h),
                        "headroom for edr={edr} escaped: {h}"
                    );
                    let v = hdr_additive_encode(chan, boost, h);
                    assert!(
                        v >= 0.0 && v <= h,
                        "encode(chan={chan}, boost={boost}, edr={edr}) = {v} > headroom {h}"
                    );
                    // The COMPOSED present bound: grid + additive <= edr_max.
                    assert!(
                        hdr_grid_encode(b as u8) + v <= sanitize_edr_max(edr) + f32::EPSILON,
                        "presented pixel would exceed the panel's EDR max"
                    );
                }
            }
        }
    }

    /// THE ADDITIVE CLAMP CORE over the values that decide `f32::min`/`max`,
    /// every pair as emission and as headroom: the result is a number in
    /// `[0, max(h, 0)]` — a NaN or infinite emission lands on a bound, and a
    /// NaN, negative or `-inf` headroom lets nothing through.
    #[test]
    fn clamp_add_bounded_over_the_special_value_lattice() {
        let subnormal = f32::from_bits(1);
        let lattice = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            subnormal,
            -subnormal,
            f32::MIN_POSITIVE,
            1.0,
            -1.0,
            f32::MAX,
            f32::MIN,
            0.5,
            3.0,
            -2.5,
        ];
        for x in lattice {
            for h in lattice {
                let v = clamp_add(x, h);
                // `f32::max` returns the other operand for NaN: a NaN
                // headroom's bound is 0.
                let bound = h.max(0.0);
                assert!(
                    v >= 0.0 && v <= bound,
                    "clamp_add({x:e}, {h:e}) = {v:e} outside [0, {bound:e}]"
                );
            }
        }
        // The NaN-headroom clause by name: whatever the emission, nothing.
        for x in lattice {
            assert_eq!(clamp_add(x, f32::NAN), 0.0, "clamp_add({x:e}, NaN)");
        }
    }

    /// NON-VACUITY for the additive law: with real headroom the boost genuinely
    /// emits above zero AND is genuinely cut by the clamp when it would blow
    /// past the headroom — the bound is load-bearing, not trivially slack.
    #[test]
    fn additive_clamp_is_load_bearing() {
        // A bright aurora channel on a 2.0x panel: emits real light...
        let lit = hdr_additive_encode(1.0, HDR_GLOW_BOOST, additive_headroom(2.0));
        assert!(
            lit > 0.9,
            "full-coverage glow must emit near its boost ({lit})"
        );
        // ...and an oversized boost IS clamped at the headroom exactly.
        let clamped = hdr_additive_encode(1.0, 50.0, additive_headroom(1.6));
        let h = additive_headroom(1.6);
        assert_eq!(clamped, h, "over-boost must clamp AT the headroom");
        // SDR panel (headroom 0): the pass provably adds nothing.
        assert_eq!(hdr_additive_encode(1.0, 50.0, additive_headroom(1.0)), 0.0);
    }

    /// `sanitize_edr_max` totality on the boundary values that decide its
    /// `is_finite`/`>=`/`min`: unset/corrupt → 1.0, real → itself, huge →
    /// capped, and every special float inside `[1, EDR_MAX_CAP]`.
    #[test]
    fn sanitize_edr_boundaries() {
        assert_eq!(sanitize_edr_max(0.0), 1.0, "unset per-window default");
        assert_eq!(sanitize_edr_max(-1.0), 1.0);
        assert_eq!(sanitize_edr_max(f32::NAN), 1.0);
        assert_eq!(sanitize_edr_max(f32::INFINITY), 1.0);
        assert_eq!(sanitize_edr_max(1.0), 1.0);
        assert_eq!(sanitize_edr_max(1.6), 1.6, "a real MacBook panel value");
        assert_eq!(sanitize_edr_max(16.0), 16.0, "Pro Display XDR");
        assert_eq!(
            sanitize_edr_max(1.0e9),
            EDR_MAX_CAP,
            "corrupt huge → capped"
        );
        // The values a float's min/max/is_finite decide on: signed zeros, the
        // smallest normal, a subnormal, the extremes, and both infinities.
        for v in [
            -0.0f32,
            f32::MIN_POSITIVE,
            f32::from_bits(1),
            f32::MAX,
            f32::MIN,
            f32::NEG_INFINITY,
            f32::NAN,
        ] {
            let s = sanitize_edr_max(v);
            assert!((1.0..=EDR_MAX_CAP).contains(&s), "{v:e} → {s}");
            let h = additive_headroom(v);
            assert!(
                (0.0..=EDR_MAX_CAP - 1.0).contains(&h),
                "{v:e} → headroom {h}"
            );
        }
    }

    /// SDR GLOW BUDGET LAW over a hostile lattice: the swapchain-side SDR crown's
    /// per-channel budget never escapes `[0, SDR_GLOW_BUDGET_BASE]` for any luma ×
    /// strength (corrupt values included), light themes compute a sub-visible
    /// budget, poisoned inputs disable the boost, and strength 0 is EXACTLY off.
    #[test]
    fn sdr_glow_budget_bounded_and_shaped() {
        for &l in &[
            -3.0f32,
            0.0,
            0.05,
            0.1,
            0.5,
            0.9,
            1.0,
            7.0,
            f32::NAN,
            f32::INFINITY,
        ] {
            for &s in &[-1.0f32, 0.0, 0.35, 1.0, 9.0, f32::NAN, f32::INFINITY] {
                let b = sdr_glow_budget(l, s);
                assert!(
                    (0.0..=SDR_GLOW_BUDGET_BASE).contains(&b),
                    "budget escaped bounds: luma={l} strength={s} -> {b}"
                );
            }
        }
        // Shape: dark keeps most of the base; light degrades to sub-visible.
        assert!(
            sdr_glow_budget(0.05, 1.0) > 0.3,
            "near-black keeps ~90% of base"
        );
        assert!(
            sdr_glow_budget(0.9, 1.0) < 1.0 / 255.0,
            "light theme (luma .9) is under one code value — visually off"
        );
        assert_eq!(sdr_glow_budget(0.0, 0.0), 0.0, "strength 0 is exactly off");
        assert_eq!(sdr_glow_budget(f32::NAN, 1.0), 0.0, "NaN luma fails OFF");
        assert_eq!(
            sdr_glow_budget(0.0, f32::NAN),
            0.0,
            "NaN strength fails OFF"
        );
        // Default ship point: dark theme, strength 0.35 — subtle but present.
        let ship = sdr_glow_budget(0.05, 0.35);
        assert!(
            (0.05..=0.20).contains(&ship),
            "ship point is subtle: {ship}"
        );
        // packed_luma anchors.
        assert_eq!(packed_luma(0x000000), 0.0);
        assert_eq!(packed_luma(0xFFFFFF), 1.0);
        assert!(packed_luma(0x1e1e1e) < 0.2, "dark editor bg reads dark");
    }
}
