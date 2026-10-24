// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WCAG 2 relative luminance and contrast of `0x00RRGGBB` pixels, in `f64`: the render
//! suite's one measure of a colour pair. Each 8-bit channel's linear value is computed once,
//! so a sweep that measures every pixel of every frame reads a table instead of `powf`.

use std::sync::LazyLock;

static LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
    let mut linear = [0.0; 256];
    for (c, out) in (0..=255u8).zip(linear.iter_mut()) {
        let s = f64::from(c) / 255.0;
        *out = if s <= 0.040_45 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        };
    }
    linear
});

/// WCAG relative luminance of a pixel.
pub fn luma(p: u32) -> f64 {
    let lin = &*LINEAR;
    let ch = |shift: u32| lin[((p >> shift) & 0xff) as usize];
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// WCAG contrast ratio (`>= 1`) of two pixels.
pub fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (luma(a), luma(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
