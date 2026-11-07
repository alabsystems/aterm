// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The strip box (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3): the one painter of every box
//! of every strip, the deck's rows and the lens's traces alike.
//!
//! A box is a plate in `track`. A heat column rises from its foot to the box's mean, in the heat
//! of the mean; a one-pixel peak tick stands at its largest reading, in the heat of that, where it
//! clears the column by a pixel or more; and an over cap, outside the plate above it, is as long
//! as the share of the box's read time that was over, each reading weighed by the time it stands
//! for. A box with no reading is `hollow`. A box's value is its height, so a strip reads without
//! colour.

use aterm_vitals::ink::{Rgb, VitalsInks};

use crate::stats_paint::{Rect, panel};
use crate::widget::{DrawPrim, Rgba, rgba};

/// What one box holds, each a share of its row's capacity now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BoxValue {
    /// Its mean, the column; `None` where nothing was read (hollow).
    pub(crate) mean: Option<f32>,
    /// Its largest reading, the peak tick.
    pub(crate) max: Option<f32>,
    /// The share of its read time that was over the resource's maximum, `0..=1`, the over cap.
    pub(crate) over: f32,
}

/// The heat a box's column and tick are drawn in, each a share of capacity, where that is not
/// their height: a lens trace stands on its own axis and keeps the heat of the share its lens names
/// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §7). `None` takes the height's share.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BoxHeat {
    /// The column's heat.
    pub(crate) mean: Option<f32>,
    /// The tick's heat.
    pub(crate) max: Option<f32>,
}

/// An unread box's mark in `hollow`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hollow {
    /// A box 4 device px or more each way: its one-pixel outline.
    Outline(Rect),
    /// A smaller box: a one-pixel line along its foot.
    Foot(Rect),
}

/// Where each part of one box draws, device px, and in what ink.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Skyline {
    /// The plate, the whole box in `track`; none for an unread box.
    pub(crate) plate: Option<Rect>,
    /// The column from the foot to the mean, in the heat of the mean.
    pub(crate) column: Option<(Rect, Rgb)>,
    /// The peak tick, one device px tall, in the heat of the largest reading.
    pub(crate) tick: Option<(Rect, Rgb)>,
    /// The over cap above the plate, in `over`.
    pub(crate) cap: Option<Rect>,
    /// An unread box's outline or foot.
    pub(crate) hollow: Option<Hollow>,
}

/// The over cap's clear gap above the plate and its height, device px, at `scale`: 1 and 2 at
/// 2×, 0 and 1 at 1×.
pub(crate) fn cap_px(scale: f64) -> (f32, f32) {
    let s = scale.max(0.5);
    ((s / 2.0).floor() as f32, s.round().max(1.0) as f32)
}

/// The height from the foot, whole device px, of share `v` in a box `h` px tall: at least the one
/// px of a foot, at most the box.
fn rise(v: f32, h: f32) -> f32 {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    (v * h).round().clamp(1.0, h.max(1.0))
}

/// Lays one box at `at` (device px) holding `v`, at `scale`, in `inks` (`heat_ic` under
/// Increase Contrast, `ic`): each part in the heat of its own height.
pub(crate) fn skyline_box(
    at: Rect,
    v: BoxValue,
    scale: f64,
    inks: &VitalsInks,
    ic: bool,
) -> Skyline {
    skyline_box_in(at, v, BoxHeat::default(), scale, inks, ic)
}

/// [`skyline_box`] with the column and tick in `heat` where it names a share.
pub(crate) fn skyline_box_in(
    at: Rect,
    v: BoxValue,
    heat: BoxHeat,
    scale: f64,
    inks: &VitalsInks,
    ic: bool,
) -> Skyline {
    let (x, y, w, h) = at;
    if w <= 0.0 || h <= 0.0 {
        return Skyline::default();
    }
    let Some(mean) = v.mean.filter(|m| !m.is_nan()) else {
        let hollow = if w < 4.0 || h < 4.0 {
            Hollow::Foot((x, y + h - 1.0, w, 1.0))
        } else {
            Hollow::Outline(at)
        };
        return Skyline {
            hollow: Some(hollow),
            ..Skyline::default()
        };
    };
    let ch = rise(mean, h);
    let column = Some((
        (x, y + h - ch, w, ch),
        inks.cell_at(heat.mean.unwrap_or(mean), ic),
    ));
    let tick = v.max.filter(|m| m.is_finite()).and_then(|m| {
        let th = rise(m, h);
        (th >= ch + 2.0).then(|| {
            let inset = if w >= 5.0 { 1.0 } else { 0.0 };
            let ink = inks.cell_at(heat.max.unwrap_or(m), ic);
            ((x + inset, y + h - th, w - 2.0 * inset, 1.0), ink)
        })
    });
    let cap = (v.over.is_finite() && v.over > 0.0).then(|| {
        let (clear, cap_h) = cap_px(scale);
        let len = (v.over.min(1.0) * w).round().clamp(1.0, w);
        let cx = x + ((w - len) / 2.0).round();
        (cx, y - clear - cap_h, len, cap_h)
    });
    Skyline {
        plate: Some(at),
        column,
        tick,
        cap,
        hollow: None,
    }
}

impl Skyline {
    /// The box as draw prims, in order (plate, column, tick, cap, or the hollow mark), each moved
    /// by `(ox, oy)`; `radius` rounds the plate and a column tall enough to take it.
    pub(crate) fn prims(
        &self,
        inks: &VitalsInks,
        radius: f32,
        (ox, oy): (f32, f32),
    ) -> Vec<DrawPrim> {
        let mut out = Vec::with_capacity(4);
        let fill =
            |(x, y, w, h): Rect, r: f32, ink: Rgb| panel(ox + x, oy + y, w, h, r, rgba(ink, 0xFF));
        if let Some(plate) = self.plate {
            out.push(fill(plate, radius, inks.track));
        }
        if let Some((rect, ink)) = self.column {
            let r = if rect.3 >= 2.0 * radius { radius } else { 0.0 };
            out.push(fill(rect, r, ink));
        }
        if let Some((rect, ink)) = self.tick {
            out.push(fill(rect, 0.0, ink));
        }
        if let Some(rect) = self.cap {
            out.push(fill(rect, 0.0, inks.over));
        }
        match self.hollow {
            Some(Hollow::Foot(rect)) => out.push(fill(rect, 0.0, inks.hollow)),
            Some(Hollow::Outline(rect)) => {
                out.push(outline(rect, radius, (ox, oy), rgba(inks.hollow, 0xFF)));
            }
            None => {}
        }
        out
    }
}

/// An unread box's one-pixel outline on the box's own edge px, the px its plate covers when it is
/// read: every strip box's, an unread core's and the "not read" key's (`Deck::hollow_cell`). A
/// rounded stroke is centred on its rect's edge, so it takes the box half a px in; an unrounded
/// one (no radius, or a box too small to round, [`crate::tray_raster::rounded_stroke`]) is drawn
/// inside its rect, so it takes the box itself. Half a px in, an unrounded stroke rounds its left
/// and top edges a px inward, inside the box's own (A1 R3's land stage, 2026-11-05).
pub(crate) fn outline(
    (x, y, w, h): Rect,
    radius: f32,
    (ox, oy): (f32, f32),
    color: Rgba,
) -> DrawPrim {
    let (x, y) = (ox + x, oy + y);
    if crate::tray_raster::rounded_stroke(radius, w - 1.0, h - 1.0, 1.0) {
        DrawPrim::Stroke {
            x: x + 0.5,
            y: y + 0.5,
            w: w - 1.0,
            h: h - 1.0,
            radius,
            width: 1.0,
            color,
        }
    } else {
        DrawPrim::Stroke {
            x,
            y,
            w,
            h,
            radius: 0.0,
            width: 1.0,
            color,
        }
    }
}

#[cfg(test)]
#[path = "stats_skyline_tests.rs"]
mod tests;
