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
#[path = "hdr_tests.rs"]
mod tests;
