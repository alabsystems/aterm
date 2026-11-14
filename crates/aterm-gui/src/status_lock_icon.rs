// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The menu-bar item's fleet access lock (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §6.1):
//! three template images, one per level, drawn as signed-distance shapes and rasterized by
//! `aterm-template-icon`.
//!
//! | Level | Shackle | Body |
//! |---|---|---|
//! | Read Only | closed | solid |
//! | Read and Write | ajar: raised, its right leg ending clear of the body | outline, lower half solid |
//! | Full Access | open: raised, its right leg gone but for a stub | outline, empty |
//!
//! Each step up opens both cues a notch, the shackle and the fill, so any two levels differ in
//! both and a level reads at a glance and at one pixel a point. As template images AppKit tints
//! them for a light bar, a dark bar and the highlighted button.
//!
//! Each level has a second image, the same lock with a bar through it: the blocked mark, which
//! the bar shows while this Mac's lock has refused a request its person has not seen
//! (`status_item::blocked_unseen`). The bar runs from the upper left to the lower right inside a
//! clear band, so it reads on a solid body as on an empty one, and it stays clear of the right
//! leg and the lower body, which say the level. The closed lock with the bar is also the menu's
//! own mark, on a traffic row and on its header where a request was blocked.

use aterm_template_icon::Pt;
use aterm_template_icon::sdf::{ring, round_rect, segment};
use aterm_types::fleet_access::FleetAccess;

/// The icon's width and height in points.
pub(crate) const SIZE_POINTS: (f64, f64) = (14.0, 18.0);
/// The pixels-per-point scales an image holds a bitmap for.
pub(crate) const SCALES: [u32; 2] = [1, 2];

// Every edge sits on a half point, so the icon is crisp at two pixels a point and, at one,
// blurs by at most half a pixel, the same on both sides of its axis (x = 7).

/// The body, `[x0, y0, x1, y1]`.
const BODY: [f64; 4] = [1.0, 10.0, 13.0, 17.0];
/// The body's corner radius.
const BODY_RADIUS: f64 = 1.5;
/// The outlined body's wall, and the shackle's stroke.
const STROKE: f64 = 1.5;
/// Where the half-open body's fill starts: the lower half of what its wall encloses is solid.
const HALF: f64 = 13.5;
/// The shackle's legs, by their centre lines.
const LEGS: [f64; 2] = [3.75, 10.25];
/// The centre of the closed shackle's arch: its legs run from here down into the body.
const ARCH_CLOSED: f64 = 7.0;
/// The centre of a raised shackle's arch: ajar and open.
const ARCH_RAISED: f64 = 5.0;
/// Where the ajar shackle's right leg ends, 2 points clear of the body's top.
const AJAR_LEG_END: f64 = 8.0;
/// Where the open shackle's right leg ends: a stub under the arch.
const OPEN_LEG_END: f64 = 5.5;
/// The blocked mark's bar, by its centre line: from the upper left to the lower right at 45
/// degrees, through pixel centres at one pixel a point, where a diagonal draws cleanest.
const BAR: [Pt; 2] = [Pt::new(2.5, 5.5), Pt::new(11.5, 14.5)];
/// The clear band on each side of the bar, which parts it from the lock.
const BAR_CLEAR: f64 = 1.0;

/// What accessibility says each image is: the lock, and with the bar through it that a request
/// was blocked.
pub(crate) const fn description(level: FleetAccess, blocked: bool) -> &'static str {
    match (level, blocked) {
        (FleetAccess::ReadOnly, false) => "lock closed",
        (FleetAccess::ReadWrite, false) => "lock half open",
        (FleetAccess::Full, false) => "lock open",
        (FleetAccess::ReadOnly, true) => "lock closed, a request blocked",
        (FleetAccess::ReadWrite, true) => "lock half open, a request blocked",
        (FleetAccess::Full, true) => "lock open, a request blocked",
    }
}

/// The level whose lock, with the bar, is the menu's blocked mark: the closed one, since a
/// block is a lock that stayed shut. The menu is macOS's.
#[cfg(target_os = "macos")]
pub(crate) const ROW_MARK: FleetAccess = FleetAccess::ReadOnly;
/// What accessibility says the menu's blocked mark is.
#[cfg(target_os = "macos")]
pub(crate) const ROW_MARK_DESCRIPTION: &str = "blocked";

/// The pixels of the icon at `scale` pixels a point, as `(width, height)`.
pub(crate) fn pixels(scale: u32) -> (u32, u32) {
    // Whole points, so the cast is exact.
    let (w, h) = SIZE_POINTS;
    (w as u32 * scale, h as u32 * scale)
}

/// `level`'s lock at `scale` pixels a point, with the bar through it where `blocked`: one
/// coverage byte per pixel (0 clear, 255 ink).
pub(crate) fn coverage(level: FleetAccess, blocked: bool, scale: u32) -> Vec<u8> {
    aterm_template_icon::coverage(pixels(scale), SIZE_POINTS, |p| {
        if blocked {
            barred(lock(level, p), p)
        } else {
            lock(level, p)
        }
    })
}

/// [`coverage`] as a template PNG.
pub(crate) fn png(level: FleetAccess, blocked: bool, scale: u32) -> Vec<u8> {
    let (w, h) = pixels(scale);
    aterm_template_icon::png(&coverage(level, blocked, scale), w, h)
}

/// The icon's canvas with nothing drawn, as a template PNG: what a menu row with no mark holds
/// where another row has one, so their words start at the same place.
pub(crate) fn blank_png(scale: u32) -> Vec<u8> {
    let (w, h) = pixels(scale);
    aterm_template_icon::png(&vec![0; (w * h) as usize], w, h)
}

/// `lock` with the bar through it: the bar is ink, and the band beside it is clear.
fn barred(lock: bool, p: Pt) -> bool {
    let [from, to] = BAR;
    let bar = segment(p, from, to) - STROKE / 2.0;
    bar <= 0.0 || (lock && bar > BAR_CLEAR)
}

fn lock(level: FleetAccess, p: Pt) -> bool {
    let body = round_rect(p, BODY, [BODY_RADIUS; 4]);
    let outline = body.max(-(body + STROKE));
    let filled = match level {
        FleetAccess::ReadOnly => body,
        // The outline, and everything of the body from its middle down.
        FleetAccess::ReadWrite => outline.min(body.max(HALF - p.y)),
        FleetAccess::Full => outline,
    };
    let shackle = match level {
        FleetAccess::ReadOnly => shackle(p, ARCH_CLOSED, BODY[1]),
        FleetAccess::ReadWrite => shackle(p, ARCH_RAISED, AJAR_LEG_END),
        FleetAccess::Full => shackle(p, ARCH_RAISED, OPEN_LEG_END),
    };
    filled.min(shackle) <= 0.0
}

/// The shackle whose arch is centred at height `arch`: the upper half of a ring over the two
/// legs, the left one running down into the body and the right one to `right_end`.
fn shackle(p: Pt, arch: f64, right_end: f64) -> f64 {
    let half = STROKE / 2.0;
    let [left, right] = LEGS;
    let (cx, r) = (f64::midpoint(left, right), (right - left) / 2.0);
    // The ring's upper half: the ring, cut by the half-plane above its centre.
    let top = ring(p, [cx, arch, r + half, r - half]).max(p.y - arch);
    let leg = |x: f64, end: f64| round_rect(p, [x - half, arch, x + half, end], [0.0; 4]);
    top.min(leg(left, BODY[1])).min(leg(right, right_end))
}

#[cfg(test)]
#[path = "status_lock_icon_tests.rs"]
mod tests;
