// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sparse 256-color indexed palette with SmallVec storage.

use aterm_alloc::SmallVec;

use crate::Rgb;

/// Sparse 256-color indexed palette, modifiable via OSC 4:
/// - 0-7: Standard ANSI colors
/// - 8-15: Bright ANSI colors
/// - 16-231: 6x6x6 color cube
/// - 232-255: Grayscale ramp (24 shades)
///
/// Every mutator keeps two representations in step: the sparse `overrides` record and the
/// dense `cache` that the per-cell `get` reads.
#[derive(Clone, PartialEq, Eq)]
pub struct ColorPalette {
    /// The authoritative record: non-default colors as (index, color) pairs, in insertion
    /// order. 16 inline entries cover a theme's ANSI colors.
    overrides: SmallVec<(u8, Rgb), 16>,
    /// Dense resolved value for every index: a pure function of `overrides` over
    /// [`DEFAULT_COLOR_TABLE`]. [`ColorPalette::get`] runs up to three times per indexed cell on
    /// the color-resolve path, and a theme sets all 16 ANSI slots, so a scan of `overrides`
    /// there would cost up to 16 (or 256) comparisons. Being a function of `overrides`, it
    /// leaves the derived `PartialEq` comparing the record: the same entries in a different
    /// insertion order are unequal.
    cache: [Rgb; 256],
}

// Hand-written to print `overrides` only, not the 256 cache entries.
impl core::fmt::Debug for ColorPalette {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ColorPalette")
            .field("overrides", &self.overrides)
            .finish()
    }
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorPalette {
    /// Standard ANSI colors (indices 0-7).
    #[rustfmt::skip]
    const ANSI_COLORS: [Rgb; 8] = [
        Rgb { r:   0, g:   0, b:   0 }, // 0: Black
        Rgb { r: 224, g: 108, b: 117 }, // 1: Red (#E06C75, ~4.5:1 on the dark bg;
                                        //    xterm's #CD0000 is ~3.2:1)
        Rgb { r:   0, g: 205, b:   0 }, // 2: Green
        Rgb { r: 205, g: 205, b:   0 }, // 3: Yellow
        Rgb { r:  59, g: 142, b: 234 }, // 4: Blue (#3B8EEA; xterm's #0000EE is
                                        //    near-invisible on a dark bg)
        Rgb { r: 198, g: 120, b: 221 }, // 5: Magenta (#C678DD ~5:1; xterm's
                                        //    #CD00CD is ~3.96:1)
        Rgb { r:   0, g: 205, b: 205 }, // 6: Cyan
        Rgb { r: 229, g: 229, b: 229 }, // 7: White
    ];

    /// Bright ANSI colors (indices 8-15).
    #[rustfmt::skip]
    // Softened from raw S=1/V=1 primaries toward a modern dark palette that keeps each hue.
    const BRIGHT_COLORS: [Rgb; 8] = [
        Rgb { r: 138, g: 143, b: 153 }, // 8:  Bright Black (#8A8F99, ~6:1: muted
                                        //     text that stays readable)
        Rgb { r: 255, g: 110, b: 103 }, // 9:  Bright Red     (#FF6E67)
        Rgb { r:  80, g: 250, b: 123 }, // 10: Bright Green   (#50FA7B)
        Rgb { r: 241, g: 250, b: 140 }, // 11: Bright Yellow  (#F1FA8C)
        Rgb { r:  92, g:  92, b: 255 }, // 12: Bright Blue    (already non-neon)
        Rgb { r: 255, g: 121, b: 198 }, // 13: Bright Magenta (#FF79C6)
        Rgb { r: 139, g: 233, b: 253 }, // 14: Bright Cyan    (#8BE9FD)
        Rgb { r: 255, g: 255, b: 255 }, // 15: Bright White
    ];

    /// Create a new color palette with default xterm colors.
    #[must_use]
    pub fn new() -> Self {
        Self {
            overrides: SmallVec::new(),
            // A copy of the compile-time table — no runtime loop.
            cache: DEFAULT_COLOR_TABLE,
        }
    }

    /// The RGB value for an indexed color: one load from the dense cache.
    #[must_use]
    pub fn get(&self, index: u8) -> Rgb {
        // `as usize`, not `usize::from`: Trust cannot see into the trait call and leaves the
        // bounds obligation unproved, while the primitive cast proves it.
        self.cache[index as usize]
    }

    /// Set the RGB value for an indexed color; setting the default removes the override.
    pub fn set(&mut self, index: u8, color: Rgb) {
        let default = Self::default_color(index);

        let pos = self.overrides.iter().position(|&(idx, _)| idx == index);

        if color == default {
            if let Some(p) = pos {
                self.overrides.swap_remove(p);
            }
        } else if let Some(p) = pos {
            // `p` came from `position` on this vec; the total `get_mut` shows the Trust gate
            // it is in bounds.
            if let Some(slot) = self.overrides.get_mut(p) {
                slot.1 = color;
            }
        } else {
            self.overrides.push((index, color));
        }

        // One write serves both arms: on removal `color == default`.
        self.cache[index as usize] = color;
    }

    /// Reset a single color to its default value.
    pub fn reset_color(&mut self, index: u8) {
        if let Some(pos) = self.overrides.iter().position(|&(idx, _)| idx == index) {
            self.overrides.swap_remove(pos);
        }
        // Keyed by color index, not position in `overrides`, so `swap_remove`'s reshuffle needs
        // no fixup.
        self.cache[index as usize] = Self::default_color(index);
    }

    /// Reset the entire palette to defaults.
    pub fn reset(&mut self) {
        self.overrides.clear();
        self.cache = DEFAULT_COLOR_TABLE;
    }

    /// Returns the number of customized (non-default) colors.
    #[must_use]
    pub fn overrides_count(&self) -> usize {
        self.overrides.len()
    }

    /// The overridden (index, color) pairs, for tests.
    #[cfg(test)]
    fn overrides(&self) -> &[(u8, Rgb)] {
        &self.overrides
    }

    /// The default color for an index; `const fn` so [`DEFAULT_COLOR_TABLE`] is built at
    /// compile time.
    const fn compute_default_color(index: u8) -> Rgb {
        match index {
            0..=7 => Self::ANSI_COLORS[index as usize],
            8..=15 => Self::BRIGHT_COLORS[index as usize - 8],
            16..=231 => {
                // 6x6x6 color cube
                let idx = index - 16;
                let r = idx / 36;
                let g = (idx % 36) / 6;
                let b = idx % 6;
                Rgb::new(
                    if r == 0 { 0 } else { 55 + 40 * r },
                    if g == 0 { 0 } else { 55 + 40 * g },
                    if b == 0 { 0 } else { 55 + 40 * b },
                )
            }
            232..=255 => {
                // Grayscale ramp
                let gray = 8 + 10 * (index - 232);
                Rgb::new(gray, gray, gray)
            }
        }
    }

    /// Get the default color for an index.
    #[must_use]
    pub fn default_color(index: u8) -> Rgb {
        DEFAULT_COLOR_TABLE[index as usize]
    }

    /// Parse an X11 color specification, or `None` if unrecognized:
    /// - `rgb:RR/GG/BB` (hex, 1-4 digits per component)
    /// - `rgbi:R.RR/G.GG/B.BB` (floating-point 0.0-1.0, per X11 Xcms)
    /// - `#RGB`, `#RRGGBB`, `#RRRGGGBBB` or `#RRRRGGGGBBBB`
    /// - X11 color names, case-insensitive: `red`, `DarkSlateGray`, ...
    #[must_use]
    pub fn parse_color_spec(spec: &str) -> Option<Rgb> {
        if let Some(rest) = spec.strip_prefix("rgbi:") {
            // rgbi:R/G/B (0.0-1.0 per component). `splitn(4, '/')` with a `None` fourth item
            // demands exactly three fields without a `collect` the verifier cannot bound.
            let mut parts = rest.splitn(4, '/');
            let (Some(rp), Some(gp), Some(bp), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return None;
            };

            let r = Self::parse_float_component(rp)?;
            let g = Self::parse_float_component(gp)?;
            let b = Self::parse_float_component(bp)?;

            Some(Rgb::new(r, g, b))
        } else if let Some(rest) = spec.strip_prefix("rgb:") {
            // rgb:RR/GG/BB (1-4 hex digits per component), split as for `rgbi:`.
            let mut parts = rest.splitn(4, '/');
            let (Some(rp), Some(gp), Some(bp), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return None;
            };

            let r = Self::parse_hex_component(rp)?;
            let g = Self::parse_hex_component(gp)?;
            let b = Self::parse_hex_component(bp)?;

            Some(Rgb::new(r, g, b))
        } else if let Some(rest) = spec.strip_prefix('#') {
            // Non-ASCII is refused: the arms below count one byte per digit.
            if !rest.is_ascii() {
                return None;
            }
            // #RGB, #RRGGBB, #RRRGGGBBB or #RRRRGGGGBBBB. Each arm's ranges are in bounds by
            // its `rest.len()` and on char boundaries by the ASCII guard, so `rest.get(..)` is
            // always `Some`; a digit is at most 15, so `saturating_mul(17)` never saturates.
            // Both spellings leave the verifier no panic path.
            match rest.len() {
                3 => {
                    // #RGB
                    let r = u8::from_str_radix(rest.get(0..1)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    let g = u8::from_str_radix(rest.get(1..2)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    let b = u8::from_str_radix(rest.get(2..3)?, 16)
                        .ok()?
                        .saturating_mul(17);
                    Some(Rgb::new(r, g, b))
                }
                6 => {
                    // #RRGGBB
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(2..4)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                9 => {
                    // #RRRGGGBBB - take high byte of each
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(3..5)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(6..8)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                12 => {
                    // #RRRRGGGGBBBB - take high byte of each
                    let r = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
                    let g = u8::from_str_radix(rest.get(4..6)?, 16).ok()?;
                    let b = u8::from_str_radix(rest.get(8..10)?, 16).ok()?;
                    Some(Rgb::new(r, g, b))
                }
                _ => None,
            }
        } else {
            // Try X11 named color lookup (case-insensitive)
            crate::x11_colors::lookup(spec)
        }
    }

    /// Parse a hex component with 1-4 digits, scaling to 8-bit.
    // Skip: `str`/radix parse absent std body; fail-closed (None on malformed).
    #[cfg_attr(trust_verify, trust::skip)]
    fn parse_hex_component(s: &str) -> Option<u8> {
        if s.is_empty() || s.len() > 4 {
            return None;
        }

        let value = u16::from_str_radix(s, 16).ok()?;

        // Scale to 8-bit based on number of digits
        let scaled = match s.len() {
            // For one digit `(value << 4) | value` is `value * 17`, with no overflow obligation.
            1 => (value << 4) | value, // 0-15 -> 0-255
            2 => value,                // 0-255 -> 0-255
            3 => value >> 4,           // 0-4095 -> 0-255
            4 => value >> 8,           // 0-65535 -> 0-255
            _ => return None,
        };

        // Every arm yields 0-255, so the fallback never fires.
        Some(scaled.try_into().unwrap_or(u8::MAX))
    }

    /// Parse a floating-point color component, clamped to \[0.0, 1.0\] and rounded to 8 bits.
    // Skip: `str::parse` absent std body; fail-closed.
    #[cfg_attr(trust_verify, trust::skip)]
    fn parse_float_component(s: &str) -> Option<u8> {
        let value: f64 = s.parse().ok()?;
        let clamped = value.clamp(0.0, 1.0);
        Some((clamped * 255.0 + 0.5) as u8)
    }

    /// Format a color as an X11 `rgb:RRRR/GGGG/BBBB` specification (16 bits per component).
    #[must_use]
    pub fn format_color_spec(color: Rgb) -> String {
        // 8-bit to 16-bit is `x * 257`, spelled `(x << 8) | x` so the Trust gate sees no
        // overflow.
        let r16 = (u16::from(color.r) << 8) | u16::from(color.r);
        let g16 = (u16::from(color.g) << 8) | u16::from(color.g);
        let b16 = (u16::from(color.b) << 8) | u16::from(color.b);
        // `push_hex4`, not `format!("{:04x}")`: runtime-argument `format_args!` cannot be
        // lowered natively.
        let mut spec = String::from("rgb:");
        crate::trust_fmt::push_hex4(&mut spec, r16);
        spec.push('/');
        crate::trust_fmt::push_hex4(&mut spec, g16);
        spec.push('/');
        crate::trust_fmt::push_hex4(&mut spec, b16);
        spec
    }
}

/// Default RGB for every 256-color index, built at compile time from
/// [`ColorPalette::compute_default_color`].
static DEFAULT_COLOR_TABLE: [Rgb; 256] = {
    let mut table = [Rgb::new(0, 0, 0); 256];
    // u16 counter: a u8 would overflow on the final increment.
    let mut i: u16 = 0;
    while i < 256 {
        table[i as usize] = ColorPalette::compute_default_color(i as u8);
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_colors_are_the_xterm_256_table() {
        for (index, rgb) in [
            (0, Rgb::new(0, 0, 0)),
            (7, Rgb::new(229, 229, 229)),
            // Bright black (gray).
            (8, Rgb::new(138, 143, 153)),
            (15, Rgb::new(255, 255, 255)),
            // The 6x6x6 cube: first, last, and r=5 g=0 b=0.
            (16, Rgb::new(0, 0, 0)),
            (231, Rgb::new(255, 255, 255)),
            (196, Rgb::new(255, 0, 0)),
            // The grayscale ramp: 8 + 10 * n.
            (232, Rgb::new(8, 8, 8)),
            (255, Rgb::new(238, 238, 238)),
        ] {
            assert_eq!(ColorPalette::default_color(index), rgb, "index {index}");
        }
    }

    #[test]
    fn parse_color_spec_reads_every_xparsecolor_form() {
        for (spec, rgb) in [
            ("rgb:ff/00/80", Rgb::new(255, 0, 128)),
            // A single hex digit scales by 17 (0xF -> 0xFF).
            ("rgb:f/0/8", Rgb::new(255, 0, 136)),
            ("#f08", Rgb::new(255, 0, 136)),
            ("#ff0080", Rgb::new(255, 0, 128)),
            // Wider groups keep their high byte.
            ("#fff000888", Rgb::new(255, 0, 136)),
            ("#ffff00008080", Rgb::new(255, 0, 128)),
            ("rgbi:1.0/0.5/0.0", Rgb::new(255, 128, 0)),
            ("rgbi:0.0/0.0/0.0", Rgb::new(0, 0, 0)),
            ("rgbi:1.0/1.0/1.0", Rgb::new(255, 255, 255)),
            ("rgbi:1/0/0", Rgb::new(255, 0, 0)),
            // Out-of-range intensities clamp; fractions round half up.
            ("rgbi:2.0/1.5/1.0", Rgb::new(255, 255, 255)),
            ("rgbi:-0.5/-1.0/0.0", Rgb::new(0, 0, 0)),
            ("rgbi:0.333/0.667/0.5", Rgb::new(85, 170, 128)),
        ] {
            assert_eq!(ColorPalette::parse_color_spec(spec), Some(rgb), "{spec}");
        }
    }

    #[test]
    fn parse_color_spec_refuses_malformed_specs() {
        for spec in [
            "",
            "#",
            "#ff",
            "#ffff",
            "rgb:",
            "rgb:ff/00",
            "rgb:gg/00/00",
            "notacolor",
            "rgbi:",
            "rgbi:1.0/0.5",
            "rgbi:abc/0.0/0.0",
            "rgbi:1.0/0.5/0.0/0.5",
            // Multi-byte UTF-8 whose byte length matches #RRGGBB / #RGB must
            // be refused, not panic on byte-index slicing.
            "#你好",
            "#你",
        ] {
            assert_eq!(ColorPalette::parse_color_spec(spec), None, "{spec:?}");
        }
    }

    #[test]
    fn parse_named_color_reads_x11_and_css_names() {
        for (name, rgb) in [
            ("black", Rgb::new(0, 0, 0)),
            ("silver", Rgb::new(192, 192, 192)),
            ("gray", Rgb::new(128, 128, 128)),
            ("white", Rgb::new(255, 255, 255)),
            ("maroon", Rgb::new(128, 0, 0)),
            ("red", Rgb::new(255, 0, 0)),
            ("purple", Rgb::new(128, 0, 128)),
            ("fuchsia", Rgb::new(255, 0, 255)),
            ("green", Rgb::new(0, 128, 0)),
            ("lime", Rgb::new(0, 255, 0)),
            ("olive", Rgb::new(128, 128, 0)),
            ("yellow", Rgb::new(255, 255, 0)),
            ("navy", Rgb::new(0, 0, 128)),
            ("blue", Rgb::new(0, 0, 255)),
            ("teal", Rgb::new(0, 128, 128)),
            ("aqua", Rgb::new(0, 255, 255)),
            ("coral", Rgb::new(255, 127, 80)),
            ("rebeccapurple", Rgb::new(102, 51, 153)),
        ] {
            assert_eq!(ColorPalette::parse_color_spec(name), Some(rgb), "{name}");
        }
    }

    /// The OSC 4/10/11 reply spelling, and parse(format(c)) == c.
    #[test]
    fn format_color_spec_is_the_16_bit_spelling_and_round_trips() {
        assert_eq!(
            ColorPalette::format_color_spec(Rgb::new(0, 0, 0)),
            "rgb:0000/0000/0000"
        );
        assert_eq!(
            ColorPalette::format_color_spec(Rgb::new(255, 255, 255)),
            "rgb:ffff/ffff/ffff"
        );
        for r in [0u8, 1, 127, 128, 255] {
            for g in [0u8, 1, 127, 128, 255] {
                for b in [0u8, 1, 127, 128, 255] {
                    let original = Rgb::new(r, g, b);
                    let spec = ColorPalette::format_color_spec(original);
                    let parsed = ColorPalette::parse_color_spec(&spec).unwrap();
                    assert_eq!(
                        parsed, original,
                        "roundtrip failed for ({r}, {g}, {b}): {spec}"
                    );
                }
            }
        }
    }

    /// The dense cache agrees with `overrides` at every index after every mutation, including
    /// removing a middle entry, where `swap_remove` moves another override into its slot.
    #[test]
    fn palette_cache_agrees_with_overrides_after_mutations() {
        fn assert_coherent(p: &ColorPalette, what: &str, overrides: usize) {
            assert_eq!(p.overrides_count(), overrides, "{what}: override count");
            for i in 0..=255u8 {
                let expected = p
                    .overrides()
                    .iter()
                    .find(|(idx, _)| *idx == i)
                    .map_or_else(|| ColorPalette::default_color(i), |(_, c)| *c);
                assert_eq!(p.get(i), expected, "{what}: index {i}");
            }
        }

        let mut p = ColorPalette::new();
        assert_coherent(&p, "fresh", 0);

        for i in [0u8, 7, 15, 42, 200, 255] {
            p.set(i, Rgb::new(i ^ 0x5A, i.wrapping_add(9), 3));
        }
        assert_coherent(&p, "after sets", 6);

        // Update in place (existing override, not a new push).
        p.set(7, Rgb::new(1, 1, 1));
        assert_eq!(p.get(7), Rgb::new(1, 1, 1));
        assert_coherent(&p, "after in-place update", 6);

        // Remove a MIDDLE entry — `swap_remove` relocates the last override.
        p.reset_color(15);
        assert_coherent(&p, "after reset_color of a middle entry", 5);

        // Set-to-default is the other removal path.
        p.set(42, ColorPalette::default_color(42));
        assert_coherent(&p, "after set-to-default", 4);

        // Resetting an index that was never overridden must be a no-op.
        p.reset_color(101);
        assert_coherent(&p, "after reset_color of a non-override", 4);

        p.reset();
        assert_coherent(&p, "after reset", 0);
    }
}
