// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Color math utilities: sRGB ↔ linear transfer, hex parsing, GPU helpers.
//!
//! Canonical implementations shared across all crates. Transfer functions
//! match `TerminalRenderer.metal` `srgbToLinear` / `linearToSrgb` exactly.

/// Decode sRGB component ([0, 1]) to linear light.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Encode linear-light component ([0, 1]) to sRGB.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert sRGB [`Rgb`](crate::Rgb) to linear-light `[f32; 4]` (alpha = 1.0).
///
/// Metal/wgpu render targets with `*Srgb` framebuffer formats apply automatic
/// linear→sRGB encoding on write. Feeding sRGB values directly would
/// double-encode, producing washed-out colors (#5734).
pub fn rgb_to_f32(rgb: crate::Rgb) -> [f32; 4] {
    [
        srgb_to_linear(f32::from(rgb.r) / 255.0),
        srgb_to_linear(f32::from(rgb.g) / 255.0),
        srgb_to_linear(f32::from(rgb.b) / 255.0),
        1.0,
    ]
}

/// Parse a hex color string into [`Rgb`](crate::Rgb).
///
/// Accepts `#RGB`, `#RRGGBB`, and `#RRGGBBAA` (alpha byte ignored).
/// The `#` prefix and surrounding whitespace are optional.
/// Returns `None` for non-ASCII input or invalid hex digits.
// Skip: `str::parse` absent std body; every malformed input returns None (fail-closed).
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_hex_color(s: &str) -> Option<crate::Rgb> {
    let hex = s.trim().trim_start_matches('#');
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        3 => {
            // Trust gate: total `.get(..)` slices (bounds proven by construction)
            // and nibble-doubling as `(x << 4) | x` instead of `x * 17`. For a
            // single hex digit `x` is 0..=15, so `x << 4` is 0..=240 with a zero
            // low nibble and `| x` equals `x * 17` byte-for-byte, without the
            // (unprovable-in-isolation) multiply-overflow obligation.
            let r = u8::from_str_radix(hex.get(0..1)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(1..2)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(2..3)?, 16).ok()?;
            Some(crate::Rgb::new((r << 4) | r, (g << 4) | g, (b << 4) | b))
        }
        6 | 8 => {
            let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
            Some(crate::Rgb::new(r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_to_f32_black() {
        let f = rgb_to_f32(crate::Rgb::new(0, 0, 0));
        assert_eq!(f, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn rgb_to_f32_white() {
        let f = rgb_to_f32(crate::Rgb::new(255, 255, 255));
        for c in &f[..3] {
            assert!((*c - 1.0).abs() < 1e-5);
        }
        assert_eq!(f[3], 1.0);
    }

    #[test]
    fn parse_hex_6digit() {
        assert_eq!(
            parse_hex_color("#1E1E1E"),
            Some(crate::Rgb::new(0x1E, 0x1E, 0x1E))
        );
    }

    #[test]
    fn parse_hex_8digit_ignores_alpha() {
        assert_eq!(
            parse_hex_color("#FFFFFF33"),
            Some(crate::Rgb::new(0xFF, 0xFF, 0xFF))
        );
    }

    #[test]
    fn parse_hex_3digit_short() {
        assert_eq!(
            parse_hex_color("#F00"),
            Some(crate::Rgb::new(0xFF, 0x00, 0x00))
        );
    }

    #[test]
    fn parse_hex_no_prefix() {
        assert_eq!(parse_hex_color("00FF00"), Some(crate::Rgb::new(0, 0xFF, 0)));
    }

    #[test]
    fn parse_hex_invalid() {
        assert_eq!(parse_hex_color("#ZZZZZZ"), None);
        assert_eq!(parse_hex_color("#1E1E"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn parse_hex_non_ascii_rejected() {
        assert_eq!(parse_hex_color("#你好世界ab"), None);
    }

    #[test]
    fn round_trip_near_zero() {
        let v = 0.02;
        let rt = linear_to_srgb(srgb_to_linear(v));
        assert!((rt - v).abs() < 1e-6, "round-trip at {v}: got {rt}");
    }

    #[test]
    fn round_trip_midrange() {
        let v = 0.5;
        let rt = linear_to_srgb(srgb_to_linear(v));
        assert!((rt - v).abs() < 1e-6, "round-trip at {v}: got {rt}");
    }

    #[test]
    fn round_trip_one() {
        let rt = linear_to_srgb(srgb_to_linear(1.0));
        assert!((rt - 1.0).abs() < 1e-6, "round-trip at 1.0: got {rt}");
    }

    #[test]
    fn boundaries() {
        assert!((srgb_to_linear(0.0)).abs() < 1e-9);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        assert!((linear_to_srgb(0.0)).abs() < 1e-9);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
    }
}
