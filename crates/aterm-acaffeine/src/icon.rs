// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Menu-bar coffee-mug icon: its shapes, rasterized as coverage and encoded as a template PNG
//! by `aterm-template-icon`.

use aterm_template_icon::Pt;
use aterm_template_icon::sdf::{ring, round_rect, segment};

/// The icon's width and height in points.
pub const SIZE_POINTS: f64 = 18.0;

/// The icon at `px` × `px` pixels: one coverage byte per pixel (0 clear, 255 ink).
#[must_use]
pub fn coverage(lit: bool, px: u32) -> Vec<u8> {
    aterm_template_icon::coverage((px, px), (SIZE_POINTS, SIZE_POINTS), |p| ink(lit, p))
}

/// [`coverage`] as an 8-bit RGBA template PNG (black with coverage as alpha).
#[must_use]
pub fn png(lit: bool, px: u32) -> Vec<u8> {
    aterm_template_icon::png(&coverage(lit, px), px, px)
}

const BODY: [f64; 4] = [2.0, 6.0, 13.0, 17.0];
const BODY_RADII: [f64; 2] = [1.0, 3.0];
const WALL: f64 = 1.5;
const HANDLE: [f64; 4] = [13.0, 11.0, 3.0, 1.5];
const LETTER: [f64; 5] = [5.0, 9.0, 5.0, 5.0, 1.0];
const LETTER_TURN: f64 = 1.0;
const STEAM_X: [f64; 3] = [4.5, 7.5, 10.5];
const STEAM_LINE: [f64; 3] = [1.5, 4.5, 0.75];
const STEAM_STROKE: f64 = 0.9;

#[inline]
fn ink(lit: bool, p: Pt) -> bool {
    let body = body(p);
    let handle = ring(p, HANDLE).max(-body);
    let a = letter_a(p);
    let d = if lit {
        body.max(-a).min(handle).min(steam(p))
    } else {
        let wall = body.max(-(body + WALL));
        wall.min(handle).min(a)
    };
    d <= 0.0
}

#[inline]
fn body(p: Pt) -> f64 {
    let [rim, base] = BODY_RADII;
    round_rect(p, BODY, [rim, rim, base, base])
}

#[inline]
fn letter_a(p: Pt) -> f64 {
    let [left, top, width, height, stroke] = LETTER;
    let half = stroke / 2.0;
    let r = LETTER_TURN;
    let bar_y = top + half;
    let stem_x = left + width - half;
    let bowl_top = top + 2.0 * stroke + half;
    let bowl_foot = top + height - half;
    let bar = segment(
        p,
        Pt::new(left + stroke + half, bar_y),
        Pt::new(stem_x - r, bar_y),
    );
    let turn = quarter_arc(p, Pt::new(stem_x - r, bar_y + r), r);
    let arch = bar.min(turn) - half;
    let stem = round_rect(
        p,
        [stem_x - half, bar_y + r, stem_x + half, top + height],
        [0.0; 4],
    );
    let bowl = round_rect(
        p,
        [left + half, bowl_top, stem_x, bowl_foot],
        [r, 0.0, 0.0, r],
    )
    .abs()
        - half;
    arch.min(stem).min(bowl)
}

fn steam(p: Pt) -> f64 {
    let [top, bottom, swing] = STEAM_LINE;
    let half = STEAM_STROKE / 2.0;
    STEAM_X
        .iter()
        .map(|&cx| {
            let bx = (p.x - cx).abs() - swing;
            let by = (p.y - f64::midpoint(top, bottom)).abs() - (bottom - top) / 2.0;
            let to_box = bx.max(0.0).hypot(by.max(0.0));
            let d = if to_box > half { to_box } else { wisp(p, cx) };
            d - half
        })
        .fold(f64::INFINITY, f64::min)
}

fn wisp(p: Pt, cx: f64) -> f64 {
    const STEPS: u32 = 24;
    let [top, bottom, swing] = STEAM_LINE;
    let at = |i: u32| {
        let t = f64::from(i) / f64::from(STEPS);
        Pt::new(
            cx - swing * (t * std::f64::consts::TAU).sin(),
            top + t * (bottom - top),
        )
    };
    (0..STEPS)
        .map(|i| segment(p, at(i), at(i + 1)))
        .fold(f64::INFINITY, f64::min)
}

#[inline]
fn quarter_arc(p: Pt, c: Pt, r: f64) -> f64 {
    let (dx, dy) = (p.x - c.x, p.y - c.y);
    if dx >= 0.0 && dy <= 0.0 {
        (dx.hypot(dy) - r).abs()
    } else {
        let right = Pt::new(c.x + r, c.y);
        let up = Pt::new(c.x, c.y - r);
        segment(p, right, right).min(segment(p, up, up))
    }
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod tests;
