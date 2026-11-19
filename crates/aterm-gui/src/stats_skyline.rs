// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The strip box (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3): the one painter of every box
//! of every strip, the deck's rows and the lens's traces alike.
//!
//! A box is a plate in `track`. A column rises from its foot to the box's mean; a one-pixel peak
//! tick stands at its largest reading, where it clears the column by a pixel or more; and an over
//! cap, outside the plate above it, is as long as the share of the box's read time that was over,
//! each reading weighed by the time it stands for. A box with no reading is `hollow`. A box's
//! value is its height, so a strip reads without colour.
//!
//! The column and the tick take their ink by [`BoxHeat`]. A rate of work's box (CPU, GPU,
//! Network, Tokens, Processes) draws each in the heat of its own height; a lens trace in the heat
//! of the share its lens names. A held amount's box (Memory, Disk: an amount, not a rate of work)
//! draws both in its state's ink, `machine` at rest and `warn` or `critical` in a state, so a
//! full but calm strip is quiet (SD-80; the 2026-11-14 review of the live band, departing from
//! A1 §3's ruling 3 for those two rows).

use aterm_vitals::ink::{Rgb, VitalsInks};
use aterm_vitals::model::State;

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

/// The ink a box's column and tick are drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum BoxHeat {
    /// Each in the heat of its own height: a rate of work's box.
    #[default]
    Height,
    /// Each in the heat of a share of capacity where that is not its height: a lens trace stands
    /// on its own axis and keeps the heat of the share its lens names
    /// (`docs/DESIGN-stats-deck-units-2026-11-04.md` §7). `None` takes the height's share.
    Shares {
        /// The column's heat.
        mean: Option<f32>,
        /// The tick's heat.
        max: Option<f32>,
    },
    /// Both in the ink of the box's state ([`held_ink`]): a held amount's box (Memory, Disk),
    /// whose height is its level and whose ink is its state, not its level.
    State(Option<State>),
}

/// A held amount's box ink in `state`: `machine` at rest (`machine_ic` under Increase Contrast,
/// `ic`), the ink A1 §4.3 and A2 §3.1 give a held amount that is not load; `warn` or `critical`
/// in those states.
pub(crate) const fn held_ink(inks: &VitalsInks, state: Option<State>, ic: bool) -> Rgb {
    match state {
        Some(State::Critical) => inks.critical,
        Some(State::Warn) => inks.warn,
        _ if ic => inks.machine_ic,
        _ => inks.machine,
    }
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
    /// The column from the foot to the mean, in its [`BoxHeat`] ink.
    pub(crate) column: Option<(Rect, Rgb)>,
    /// The peak tick, one device px tall, at the largest reading, in its [`BoxHeat`] ink.
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

/// [`skyline_box`] with the column and tick in the ink `heat` names.
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
    // The ink of a part `height` tall whose lens share, where it names one, is `share`.
    let ink = |share: Option<f32>, height: f32| match heat {
        BoxHeat::Height => inks.cell_at(height, ic),
        BoxHeat::Shares { .. } => inks.cell_at(share.unwrap_or(height), ic),
        BoxHeat::State(state) => held_ink(inks, state, ic),
    };
    let (mean_share, max_share) = match heat {
        BoxHeat::Shares { mean, max } => (mean, max),
        _ => (None, None),
    };
    let ch = rise(mean, h);
    let column = Some(((x, y + h - ch, w, ch), ink(mean_share, mean)));
    let tick = v.max.filter(|m| m.is_finite()).and_then(|m| {
        let th = rise(m, h);
        (th >= ch + 2.0).then(|| {
            let inset = if w >= 5.0 { 1.0 } else { 0.0 };
            let ink = ink(max_share, m);
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
