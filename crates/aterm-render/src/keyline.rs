// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A block caret's keyline: the one-pixel ring just inside the block that the rainbow caret's
//! innermost halo ring becomes ([`aterm_core::render::RenderInput::cursor_keyline_tint`]).
//!
//! The renderer draws it in a colour of its own working ([`block_keyline_colour`]) from the fill
//! it draws the block in, after its minimum-contrast floor, and the ground under the caret: the
//! ring's tint over the fill, moved in light until the line reads against the fill
//! ([`KEYLINE_FILL_MIN_CONTRAST`]) while the caret still reads against the page
//! ([`CARET_PAGE_MIN_CONTRAST`]). Both renderers (this crate's and the GPU's) call it, so the
//! window and the films draw one line.

use crate::{add_sat, linear_luminance, linear_to_srgb, srgb8_to_linear};

/// The least WCAG contrast an effect-owned caret keeps against the ground under it; the keyline is
/// part of the caret and keeps it too, where the fill does.
pub const CARET_PAGE_MIN_CONTRAST: f32 = 4.5;

/// The least WCAG contrast the keyline's colour keeps against the block's fill. Aimed past
/// [`KEYLINE_GLASS_MIN_CONTRAST`]: the halo and the glow lay light over the caret after it, which
/// draws the two together on glass (on Dark's burst a keyline aimed at 1.35:1 read 1.29:1).
pub const KEYLINE_FILL_MIN_CONTRAST: f32 = 1.45;

/// The least WCAG contrast the keyline reads against the fill beside it on glass: the old outside
/// rim's lift over the light beside it on Dark read 1.29:1 at its median (1.34:1 in the burst);
/// the keyline reads a little over that, so the outline shows as well as the rim it replaced.
pub const KEYLINE_GLASS_MIN_CONTRAST: f32 = 1.35;

/// The margin each target is aimed past, so the 8-bit rounding of the colour keeps it.
const AIM: f32 = 1.01;

#[inline]
fn contrast(la: f32, lb: f32) -> f32 {
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `rgb` moved in linear light to relative luminance `target`: scaled toward black when darker,
/// mixed toward white when lighter, so its hue holds.
fn with_luminance(rgb: u32, target: f32) -> u32 {
    let l = linear_luminance(rgb);
    let target = target.clamp(0.0, 1.0);
    let ch = |sh: u32| srgb8_to_linear(((rgb >> sh) & 0xff) as u8);
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let (r, g, b) = if target <= l {
        let k = if l > 0.0 { target / l } else { 0.0 };
        (r * k, g * k, b * k)
    } else {
        let t = if l < 1.0 {
            (target - l) / (1.0 - l)
        } else {
            0.0
        };
        (r + (1.0 - r) * t, g + (1.0 - g) * t, b + (1.0 - b) * t)
    };
    let enc = |v: f32| {
        (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    (enc(r) << 16) | (enc(g) << 8) | enc(b)
}

/// The keyline's colour (`0x00RRGGBB`) on a block drawn in `fill` over `page`, from the halo
/// ring's premultiplied additive `tint`.
///
/// The ring's tint over the fill where that already reads [`KEYLINE_FILL_MIN_CONTRAST`] against
/// the fill and keeps the caret's page floor (the lesser of [`CARET_PAGE_MIN_CONTRAST`] and the
/// fill's own contrast against the page). Otherwise that colour moved in light, darker or lighter
/// than the fill, to the fill floor, never past the page floor: of the two, one that reaches the
/// fill floor and reads farther from the page, else the one that reads farther from the fill. On
/// a light page that is a darker line (the tint alone lightened the edge toward the page, 3.5:1
/// against it in the burst on GitHub Light); on a dark page's light fill, where a tint can add
/// only a few levels, it is a darker line too (1.09:1 against the fill on Dark, an outline lost).
#[must_use]
pub fn block_keyline_colour(fill: u32, tint: u32, page: u32) -> u32 {
    let fill = fill & 0x00FF_FFFF;
    let page = page & 0x00FF_FFFF;
    let tinted = add_sat(fill, tint & 0x00FF_FFFF);
    let (lf, lp) = (linear_luminance(fill), linear_luminance(page));
    let page_floor = CARET_PAGE_MIN_CONTRAST.min(contrast(lf, lp));
    let lt = linear_luminance(tinted);
    if keyline_as_tinted()
        || (contrast(lt, lf) >= KEYLINE_FILL_MIN_CONTRAST && contrast(lt, lp) >= page_floor)
    {
        return tinted;
    }
    let fill_aim = KEYLINE_FILL_MIN_CONTRAST * AIM;
    let page_aim = (page_floor * AIM).min(contrast(lf, lp));
    // Darker than the fill: down to the fill floor, and no nearer a darker page than its floor.
    let mut dark = (lf + 0.05) / fill_aim - 0.05;
    if lp < lf {
        dark = dark.max((lp + 0.05) * page_aim - 0.05);
    }
    // Lighter: up to the fill floor, and no nearer a lighter page than its floor.
    let mut light = fill_aim * (lf + 0.05) - 0.05;
    if lp > lf {
        light = light.min((lp + 0.05) / page_aim - 0.05);
    }
    let candidates = [
        with_luminance(tinted, dark.clamp(0.0, lf)),
        with_luminance(tinted, light.clamp(lf, 1.0)),
    ];
    let read = |c: u32| {
        let l = linear_luminance(c);
        (contrast(l, lf), contrast(l, lp))
    };
    let [a, b] = candidates;
    let ((fa, pa), (fb, pb)) = (read(a), read(b));
    let (a_ok, b_ok) = (
        fa >= KEYLINE_FILL_MIN_CONTRAST,
        fb >= KEYLINE_FILL_MIN_CONTRAST,
    );
    match (a_ok, b_ok) {
        (true, true) => {
            if pa >= pb {
                a
            } else {
                b
            }
        }
        (true, false) => a,
        (false, true) => b,
        (false, false) => {
            if fa >= fb {
                a
            } else {
                b
            }
        }
    }
}

/// The pixel rects (`[x, y, w, h]`) of a block caret's keyline: the one-pixel ring just inside the
/// block at `(x0, y0)` of `block_w` x `cell_h`, top, bottom, left and right, none overlapping;
/// `None` for a block too small to hold a ring.
#[must_use]
pub const fn block_keyline_rects(
    x0: usize,
    y0: usize,
    block_w: usize,
    cell_h: usize,
) -> Option<[[usize; 4]; 4]> {
    if block_w < 3 || cell_h < 3 {
        return None;
    }
    Some([
        [x0, y0, block_w, 1],
        [x0, y0 + cell_h - 1, block_w, 1],
        [x0, y0 + 1, 1, cell_h - 2],
        [x0 + block_w - 1, y0 + 1, 1, cell_h - 2],
    ])
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static KEYLINE_AS_TINTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`with_keyline_as_tinted`] holds on this thread: always `false` in a release build.
#[inline]
fn keyline_as_tinted() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        KEYLINE_AS_TINTED.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// A test's negative control: `f` run with [`block_keyline_colour`] the ring's tint over the fill
/// and nothing more, as the keyline was first drawn: lighter than the fill toward a light page
/// (under the caret's floor there) and all but the fill on a dark page's light block. Debug and
/// test builds only; the flag is this thread's and is put back as it was when `f` returns or
/// panics.
#[cfg(any(test, debug_assertions))]
pub fn with_keyline_as_tinted<R>(f: impl FnOnce() -> R) -> R {
    struct Back(bool);
    impl Drop for Back {
        fn drop(&mut self) {
            KEYLINE_AS_TINTED.with(|c| c.set(self.0));
        }
    }
    let _back = Back(KEYLINE_AS_TINTED.with(|c| c.replace(true)));
    f()
}

#[cfg(test)]
#[path = "keyline_tests.rs"]
mod tests;
