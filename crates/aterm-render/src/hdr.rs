// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EDR ("HDR glow") present math.

/// Upper cap for a sanitized EDR maximum.
pub const EDR_MAX_CAP: f32 = 100.0;

/// Linear-light boost on the aurora's premultiplied colour in the EDR additive pass: `1.0` re-adds the glow's own energy (up to a doubling where the panel has headroom), bright enough to read as light and never clipped-looking.
pub const HDR_GLOW_BOOST: f32 = 1.0;

/// sRGB-encoded channel (0..=1) → linear light: the standard piecewise decode, with the same constants as the WGSL `s2l` (aterm-gpu `renderer.rs`) and the CPU blend helpers, so CPU and shader agree float-for-float.
#[must_use]
#[inline]
pub fn srgb_channel_to_linear(c: f32) -> f32 {
    let c = if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) };
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Grid (non-additive) HDR encode: what the present blit writes for one stored offscreen byte on the `Rgba16Float` extended-linear swapchain.
#[must_use]
#[inline]
pub fn hdr_grid_encode(b: u8) -> f32 {
    srgb_channel_to_linear(f32::from(b) / 255.0).clamp(0.0, 1.0)
}

/// Sanitize a queried Windows SDR-white scale (`SDR-white nits / 80`, the scRGB reference): non-finite or `< 1.0` (the unset per-window default `0.0`, macOS, an SDR desktop) means no scaling → `1.0`, the same rule as `WindowGpu::sdr_white_scale`.
#[must_use]
#[inline]
pub const fn sanitize_sdr_white_scale(v: f32) -> f32 {
    if v.is_finite() && v >= 1.0 { v } else { 1.0 }
}

/// scRGB present of one stored byte, grid and remainder band alike, on the Windows `Rgba16Float` swapchain: [`hdr_grid_encode`] scaled to the desktop's SDR white.
#[must_use]
#[inline]
pub fn scrgb_present_channel(b: u8, sdr_white_scale: f32) -> f32 {
    hdr_grid_encode(b) * sanitize_sdr_white_scale(sdr_white_scale)
}

/// Sanitize a queried `NSScreen.maximumExtendedDynamicRangeColorComponentValue`: non-finite or `< 1.0` (including the unset per-window default `0.0`) means no headroom → `1.0`; a corrupt huge value is capped at [`EDR_MAX_CAP`].
#[must_use]
#[inline]
pub const fn sanitize_edr_max(v: f32) -> f32 {
    if v.is_finite() && v >= 1.0 {
        v.min(EDR_MAX_CAP)
    } else {
        1.0
    }
}

/// The additive headroom above SDR reference white: `sanitize(edr_max) - 1.0`.
#[must_use]
#[inline]
pub const fn additive_headroom(edr_max: f32) -> f32 {
    sanitize_edr_max(edr_max) - 1.0
}

/// The additive clamp core: bound a boosted linear emission `x` to `[0, max(headroom, 0)]`, the op the EDR glow fragment performs after `s2l(colour) * boost`.
#[must_use]
#[inline]
pub const fn clamp_add(x: f32, headroom: f32) -> f32 {
    x.min(headroom.max(0.0)).max(0.0)
}

/// One channel of the EDR glow pass's fragment output: decode the premultiplied sRGB-space aurora colour to linear, boost it, and clamp to the headroom.
#[must_use]
#[cfg(test)]
pub fn hdr_additive_encode(chan: f32, boost: f32, headroom: f32) -> f32 {
    clamp_add(srgb_channel_to_linear(chan) * boost, headroom)
}

/// Ceiling of the SDR glow-boost budget (raw swapchain code-value units, `0..=1`): the most the additive crown on an SDR (non-f16) present adds per channel, at full strength on black. 0.35 ≈ +89/255 peak: reads as light, far below washout.
pub const SDR_GLOW_BUDGET_BASE: f32 = 0.35;

/// The SDR glow-boost budget for a theme background: how much additive light (raw code-value units) the swapchain-side SDR crown may add per channel.
#[must_use]
#[allow(clippy::manual_clamp)]
#[inline]
pub const fn sdr_glow_budget(bg_luma: f32, strength: f32) -> f32 {
    let l = bg_luma.min(1.0).max(0.0);
    let s = strength.max(0.0).min(1.0);
    let rolloff = (1.0 - l) * (1.0 - l);
    clamp_add(SDR_GLOW_BUDGET_BASE * s * rolloff, SDR_GLOW_BUDGET_BASE)
}

/// WCAG 2 relative luminance where the one dark/light classifier ([`crate::theme_is_dark`], the inverse of `aterm_messages::ink::bg_is_light`) turns a page light: the pole where black and white ink tie, `√0.0525 − 0.05 ≈ 0.1791`.
pub const LIGHT_PAGE_LUMINANCE: f64 = 0.179_128_784_747_792;

/// The share of the cursor-trail bloom's configured strength a page `bg` (`0x00RRGGBB`) keeps (`0..=1`).
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "a share in 0..=1 narrowed to the strength's f32"
)]
#[inline]
pub fn light_page_bloom_share(bg: u32) -> f32 {
    if crate::theme_is_dark(bg) {
        return 1.0;
    }
    let [_, r, g, b] = bg.to_be_bytes();
    let l = aterm_messages::ink::contrast([0, 0, 0], [r, g, b]).mul_add(0.05, -0.05);
    let k = ((1.0 - l) / (1.0 - LIGHT_PAGE_LUMINANCE)).clamp(0.0, 1.0);
    (k * k) as f32
}

/// Relative luma (`0..=1`, Rec.601 on the raw sRGB bytes) of a packed `0x00RRGGBB` colour — the SDR glow budget's background-darkness input.
#[must_use]
#[inline]
pub const fn packed_luma(bg: u32) -> f32 {
    let r = ((bg >> 16) & 0xff) as f32;
    let g = ((bg >> 8) & 0xff) as f32;
    let b = (bg & 0xff) as f32;
    (0.299 * r + 0.587 * g + 0.114 * b) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of the 256 storable bytes encodes into [0, 1], monotonically, with exact
    /// endpoints (0 → 0.0, 255 → 1.0: SDR white is EDR reference white).
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
        assert!(
            hdr_grid_encode(255) + hdr_additive_encode(1.0, HDR_GLOW_BOOST, 1.0) > 1.0,
            "an EDR emission over white must exceed 1.0 (else the feature is vacuous)"
        );
    }

    /// scRGB present: scale 1.0 is the grid encode exactly, white maps to the scale, every byte
    /// lands in [0, scale], and a poisoned scale degrades to 1.0.
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

    /// Every channel byte × boost (0 and a corrupt huge one included) × edr_max (SDR, real
    /// panels, corrupt values): the encode stays within the sanitized headroom, so blit (≤ 1) +
    /// additive ≤ sanitize(edr_max).
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
                    assert!(
                        hdr_grid_encode(b as u8) + v <= sanitize_edr_max(edr) + f32::EPSILON,
                        "presented pixel would exceed the panel's EDR max"
                    );
                }
            }
        }
    }

    /// `clamp_add` over the values that decide `f32::min`/`max`, every pair as emission and as
    /// headroom: always in `[0, max(h, 0)]`, so a NaN, negative or `-inf` headroom passes nothing.
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
                let bound = h.max(0.0);
                assert!(
                    v >= 0.0 && v <= bound,
                    "clamp_add({x:e}, {h:e}) = {v:e} outside [0, {bound:e}]"
                );
            }
        }
        for x in lattice {
            assert_eq!(clamp_add(x, f32::NAN), 0.0, "clamp_add({x:e}, NaN)");
        }
    }

    /// With real headroom the boost emits above zero and an oversized one is cut at the
    /// headroom: the bound is load-bearing, not slack.
    #[test]
    fn additive_clamp_is_load_bearing() {
        let lit = hdr_additive_encode(1.0, HDR_GLOW_BOOST, additive_headroom(2.0));
        assert!(
            lit > 0.9,
            "full-coverage glow must emit near its boost ({lit})"
        );
        let clamped = hdr_additive_encode(1.0, 50.0, additive_headroom(1.6));
        let h = additive_headroom(1.6);
        assert_eq!(clamped, h, "over-boost must clamp AT the headroom");
        assert_eq!(hdr_additive_encode(1.0, 50.0, additive_headroom(1.0)), 0.0);
    }

    /// `sanitize_edr_max` on the values that decide its `is_finite`/`>=`/`min`: unset or
    /// corrupt → 1.0, real → itself, huge → capped, every special float inside the range.
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

    /// The bloom's page share: exactly 1 on every page the classifier calls
    /// dark (the shipped bloom untouched), continuous at the pole, monotone
    /// falling to 0 on white, and inside `0..=1` everywhere.
    #[test]
    fn the_bloom_keeps_dark_pages_and_fades_out_on_white() {
        for bg in [0x0011_1318u32, 0x001A_1B26, 0x0028_2A36, 0x0000_2B36, 0] {
            assert_eq!(
                light_page_bloom_share(bg),
                1.0,
                "dark page {bg:06x} keeps the configured bloom exactly"
            );
        }
        assert_eq!(light_page_bloom_share(0x00FF_FFFF), 0.0);
        assert!(light_page_bloom_share(0x00FD_F6E3) < 0.05);
        assert_eq!(light_page_bloom_share(0x0075_7575), 1.0);
        let first_light = light_page_bloom_share(0x0076_7676);
        assert!((0.99..1.0).contains(&first_light), "{first_light}");
        let mut last = 1.0f32;
        for v in 0..=255u32 {
            let grey = v * 0x0001_0101;
            let s = light_page_bloom_share(grey);
            assert!((0.0..=1.0).contains(&s), "{grey:06x}: {s}");
            if crate::theme_is_dark(grey) {
                assert_eq!(s, 1.0, "{grey:06x}");
            } else {
                assert!(s < 1.0 && s <= last, "{grey:06x}: {s} after {last}");
            }
            last = s;
        }
    }

    /// The SDR glow budget stays in `[0, SDR_GLOW_BUDGET_BASE]` for any luma × strength,
    /// corrupt values included; light themes are sub-visible, poisoned inputs and strength 0
    /// are off.
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
        let ship = sdr_glow_budget(0.05, 0.35);
        assert!(
            (0.05..=0.20).contains(&ship),
            "ship point is subtle: {ship}"
        );
        assert_eq!(packed_luma(0x000000), 0.0);
        assert_eq!(packed_luma(0xFFFFFF), 1.0);
        assert!(packed_luma(0x1e1e1e) < 0.2, "dark editor bg reads dark");
    }
}
