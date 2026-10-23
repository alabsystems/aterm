// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input admission and [`ScratchBudget`] bounds for hosted momentum glow ([`MOMENTUM_PROFILE`],
//! [`MOMENTUM_ENVELOPE`], [`GlowQuad`](aterm_render::GlowQuad)).
//!
//! Validates tick inputs ([`admit_tick`], [`admit_tau`], [`admit_frame_interval`], [`admit_time`])
//! and reports refusals via [`AdmitRefusal::Geometry`], [`AdmitRefusal::Radius`],
//! [`AdmitRefusal::Intensity`], and [`AdmitRefusal::GlyphSpan`].

use std::fmt;
use std::time::Duration;

use super::{HALO_ROW_DIV, HALO_SEGMENTS, MomentumGlowConfig};
use crate::cursor_glow::{Geom, LIGHT_MARK_QUADS};

/// The limits hosted momentum inputs are admitted under.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MomentumProfile {
    /// The largest window-pixel coordinate the grid may reach, on each axis.
    pub max_extent_px: u32,
    /// The largest `MomentumGlowConfig::radius_cells`.
    pub max_radius_cells: f32,
    /// The largest τ, in seconds, for a key or a tick.
    pub max_tau_s: f32,
    /// The longest frame interval a schedule query may name.
    pub max_frame_interval: Duration,
    /// The latest logical time, as a distance from the epoch.
    pub horizon: Duration,
}

/// The profile aterm hosts the momentum glow under.
pub const MOMENTUM_PROFILE: MomentumProfile = MomentumProfile {
    max_extent_px: 65_535,
    max_radius_cells: 4.0,
    max_tau_s: 60.0,
    max_frame_interval: Duration::from_secs(1),
    horizon: Duration::from_secs(30 * 24 * 60 * 60),
};

/// The widest profile the engine's arithmetic was checked for.
pub const MOMENTUM_ENVELOPE: MomentumProfile = MomentumProfile {
    max_extent_px: u16::MAX as u32,
    max_radius_cells: 64.0,
    max_tau_s: 3_600.0,
    max_frame_interval: Duration::from_secs(60),
    horizon: Duration::from_secs(10 * 365 * 24 * 60 * 60),
};

const _: () = {
    assert!(MOMENTUM_PROFILE.max_extent_px <= MOMENTUM_ENVELOPE.max_extent_px);
    assert!(MOMENTUM_PROFILE.max_radius_cells <= MOMENTUM_ENVELOPE.max_radius_cells);
    assert!(MOMENTUM_PROFILE.max_tau_s <= MOMENTUM_ENVELOPE.max_tau_s);
    assert!(
        MOMENTUM_PROFILE.max_frame_interval.as_nanos()
            <= MOMENTUM_ENVELOPE.max_frame_interval.as_nanos()
    );
    assert!(MOMENTUM_PROFILE.horizon.as_nanos() <= MOMENTUM_ENVELOPE.horizon.as_nanos());
};

/// The most quads one tick may append to each scratch stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScratchBudget {
    /// The additive `cursor_glow_add` stream (the dark page's halo).
    pub out: usize,
    /// The under-ink stream (the light page's glow).
    pub under: usize,
}

/// Which geometry precondition failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryFault {
    /// `cw == 0`.
    CellWidthZero,
    /// `ch == 0`.
    CellHeightZero,
    /// `origin_x + max(cols, 1) · cw` overflows or passes `limit`.
    GridWidth {
        origin_x: u16,
        cols: usize,
        cw: usize,
        limit: u32,
    },
    /// `origin_y + max(rows, 1) · ch` overflows or passes `limit`.
    GridHeight {
        origin_y: u16,
        rows: usize,
        ch: usize,
        limit: u32,
    },
    /// `head > origin_y`: the effects box would start above the window.
    HeadAboveOrigin { head: u16, origin_y: u16 },
    /// The scratch budget's checked arithmetic overflowed.
    Unbounded,
}

/// Why an input was refused before it reached the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmitRefusal {
    /// A τ (`f32` bits) that is not finite or not in `(0, max_tau_s]`.
    Tau(u32),
    /// A `radius_cells` (`f32` bits) that is not finite or not in `[0, max_radius_cells]`.
    Radius(u32),
    /// An `intensity` (`f32` bits) that is not finite.
    Intensity(u32),
    /// The geometry breaks a precondition.
    Geometry(GeometryFault),
    /// A glyph span `(first, end)` outside `0 ≤ first ≤ end ≤ cols`.
    GlyphSpan(i32, i32),
    /// A frame interval past `max_frame_interval`.
    FrameInterval(Duration),
    /// A time (signed ns from the epoch) before the epoch or past the horizon.
    Horizon(i128),
    /// A time earlier than the previous one.
    TimeWentBack { last: i128, at: i128 },
}

impl fmt::Display for GeometryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::CellWidthZero => f.write_str("the cell width is zero"),
            Self::CellHeightZero => f.write_str("the cell height is zero"),
            Self::GridWidth {
                origin_x,
                cols,
                cw,
                limit,
            } => write!(
                f,
                "the grid's right edge {origin_x} + {cols} x {cw} px passes {limit} px"
            ),
            Self::GridHeight {
                origin_y,
                rows,
                ch,
                limit,
            } => write!(
                f,
                "the grid's bottom edge {origin_y} + {rows} x {ch} px passes {limit} px"
            ),
            Self::HeadAboveOrigin { head, origin_y } => write!(
                f,
                "the head band {head} px is taller than the grid origin {origin_y} px"
            ),
            Self::Unbounded => f.write_str("the scratch budget overflowed its arithmetic"),
        }
    }
}

impl fmt::Display for AdmitRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Tau(bits) => write!(
                f,
                "momentum input refused: tau {} s is not in the profile's range",
                f32::from_bits(bits)
            ),
            Self::Radius(bits) => write!(
                f,
                "momentum input refused: radius {} cells is not in the profile's range",
                f32::from_bits(bits)
            ),
            Self::Intensity(bits) => write!(
                f,
                "momentum input refused: intensity {} is not finite",
                f32::from_bits(bits)
            ),
            Self::Geometry(fault) => write!(f, "momentum input refused: {fault}"),
            Self::GlyphSpan(first, end) => write!(
                f,
                "momentum input refused: glyph span {first}..{end} is not inside the grid's columns"
            ),
            Self::FrameInterval(d) => write!(
                f,
                "momentum input refused: frame interval {d:?} passes the profile's limit"
            ),
            Self::Horizon(at) => write!(
                f,
                "momentum input refused: time {at} ns from the epoch is outside the horizon"
            ),
            Self::TimeWentBack { last, at } => write!(
                f,
                "momentum input refused: time {at} ns comes before the previous input's {last} ns"
            ),
        }
    }
}

impl std::error::Error for AdmitRefusal {}

/// Admits a τ, either `on_key`'s or a tick's `cfg.tau_s`.
///
/// # Errors
/// [`AdmitRefusal::Tau`].
#[inline]
pub const fn admit_tau(p: &MomentumProfile, tau_s: f32) -> Result<(), AdmitRefusal> {
    let ok = tau_s.is_finite()
        && tau_s > 0.0
        && tau_s <= p.max_tau_s
        && tau_s <= MOMENTUM_ENVELOPE.max_tau_s;
    if ok {
        Ok(())
    } else {
        Err(AdmitRefusal::Tau(tau_s.to_bits()))
    }
}

/// Admits a schedule query's frame interval.
///
/// # Errors
/// [`AdmitRefusal::FrameInterval`].
#[inline]
pub const fn admit_frame_interval(p: &MomentumProfile, d: Duration) -> Result<(), AdmitRefusal> {
    let ns = d.as_nanos();
    if ns <= p.max_frame_interval.as_nanos()
        && ns <= MOMENTUM_ENVELOPE.max_frame_interval.as_nanos()
    {
        Ok(())
    } else {
        Err(AdmitRefusal::FrameInterval(d))
    }
}

/// Admits an input's logical time `at` (signed nanoseconds from the epoch) after an input.
///
/// # Errors
/// [`AdmitRefusal::Horizon`] or [`AdmitRefusal::TimeWentBack`].
#[inline]
pub const fn admit_time(
    p: &MomentumProfile,
    last: Option<i128>,
    at: i128,
) -> Result<(), AdmitRefusal> {
    let p_ns = p.horizon.as_nanos();
    let env_ns = MOMENTUM_ENVELOPE.horizon.as_nanos();
    let horizon_ns = if p_ns < env_ns { p_ns } else { env_ns };
    let within = horizon_ns <= i128::MAX as u128 && at >= 0 && at <= horizon_ns as i128;
    if !within {
        return Err(AdmitRefusal::Horizon(at));
    }
    match last {
        Some(last) if at < last => Err(AdmitRefusal::TimeWentBack { last, at }),
        _ => Ok(()),
    }
}

/// Admits one tick's inputs and returns the scratch it may push.
///
/// # Errors
/// [`AdmitRefusal::Geometry`], [`AdmitRefusal::Tau`], [`AdmitRefusal::Radius`].
#[inline]
pub fn admit_tick(
    p: &MomentumProfile,
    cur: Option<(u16, u16)>,
    geom: &Geom,
    cfg: &MomentumGlowConfig,
) -> Result<ScratchBudget, AdmitRefusal> {
    let grid = admit_geometry(p, geom)?;
    admit_tau(p, cfg.tau_s)?;
    let r = cfg.radius_cells;
    let radius_ok = r.is_finite()
        && r >= 0.0
        && r <= p.max_radius_cells
        && r <= MOMENTUM_ENVELOPE.max_radius_cells;
    if !radius_ok {
        return Err(AdmitRefusal::Radius(r.to_bits()));
    }
    if !cfg.intensity.is_finite() {
        return Err(AdmitRefusal::Intensity(cfg.intensity.to_bits()));
    }
    if let Some((first, end)) = cfg.glyph_span
        && !(0 <= first && first <= end && i64::from(end) <= i64::from(grid.cols))
    {
        return Err(AdmitRefusal::GlyphSpan(first, end));
    }
    let Some((cr, cc)) = cur else {
        return Ok(ScratchBudget::default());
    };
    if !cfg.enabled || cfg.intensity <= 0.0 || cr >= grid.rows || cc >= grid.cols {
        return Ok(ScratchBudget::default());
    }
    let unbounded = AdmitRefusal::Geometry(GeometryFault::Unbounded);
    if cfg.dark_theme {
        let out = dark_halo_budget((cr, cc), geom, &grid, r).ok_or(unbounded)?;
        Ok(ScratchBudget { out, under: 0 })
    } else {
        let under = light_halo_budget((cr, cc), geom, &grid, r).ok_or(unbounded)?;
        Ok(ScratchBudget { out: 0, under })
    }
}

/// The admitted geometry, narrowed: every extent fits `u16`, so the budget's
/// arithmetic starts from small integers.
#[derive(Clone, Copy, Debug)]
struct Grid {
    cw: u16,
    ch: u16,
    rows: u16,
    cols: u16,
    origin_x: u16,
    origin_y: u16,
    head: u16,
}

const fn admit_geometry(p: &MomentumProfile, geom: &Geom) -> Result<Grid, AdmitRefusal> {
    if geom.cw == 0 {
        return Err(AdmitRefusal::Geometry(GeometryFault::CellWidthZero));
    }
    if geom.ch == 0 {
        return Err(AdmitRefusal::Geometry(GeometryFault::CellHeightZero));
    }
    let limit = if p.max_extent_px < MOMENTUM_ENVELOPE.max_extent_px {
        p.max_extent_px
    } else {
        MOMENTUM_ENVELOPE.max_extent_px
    };
    const fn fits(origin: u16, cells: usize, cell: usize, limit: u32) -> Option<(u16, u16)> {
        let cells_floor = if cells > 1 { cells } else { 1 };
        let Some(prod) = cells_floor.checked_mul(cell) else {
            return None;
        };
        let Some(edge) = prod.checked_add(origin as usize) else {
            return None;
        };
        if edge as u64 > limit as u64 || cells > u16::MAX as usize || cell > u16::MAX as usize {
            return None;
        }
        Some((cells as u16, cell as u16))
    }
    let Some((cols, cw)) = fits(geom.origin_x, geom.cols, geom.cw, limit) else {
        return Err(AdmitRefusal::Geometry(GeometryFault::GridWidth {
            origin_x: geom.origin_x,
            cols: geom.cols,
            cw: geom.cw,
            limit,
        }));
    };
    let Some((rows, ch)) = fits(geom.origin_y, geom.rows, geom.ch, limit) else {
        return Err(AdmitRefusal::Geometry(GeometryFault::GridHeight {
            origin_y: geom.origin_y,
            rows: geom.rows,
            ch: geom.ch,
            limit,
        }));
    };
    if geom.head > geom.origin_y {
        return Err(AdmitRefusal::Geometry(GeometryFault::HeadAboveOrigin {
            head: geom.head,
            origin_y: geom.origin_y,
        }));
    }
    Ok(Grid {
        cw,
        ch,
        rows,
        cols,
        origin_x: geom.origin_x,
        origin_y: geom.origin_y,
        head: geom.head,
    })
}

/// The amplitude at which the halo is widest: `smoothstep(255 / 255) = 1`.
const WIDEST: f32 = 1.0;

/// `soft_ellipse`'s `STEPS`: the light mark's vertex count is this plus one.
const LIGHT_ELLIPSE_SEGMENTS: i64 = 24;

/// Each `push_fx_ramp` call per working dark-halo row: both sides of every
/// knot pair.
const DARK_CALLS_PER_ROW: usize = 2 * HALO_SEGMENTS;

/// `emit_halo`'s worst case over every amplitude: the
/// engine's row grid at `s = 1`, eight calls per working row, one quad per
/// `ch`-band each call's clipped row meets.
fn dark_halo_budget((cr, cc): (u16, u16), geom: &Geom, g: &Grid, radius: f32) -> Option<usize> {
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (_, cy) = geom.cell_center(cr, cc);
    let rx = cw * (0.55 + radius * WIDEST);
    let ry = ch * (0.45 + 0.6 * radius * WIDEST);
    if !(rx >= 1.0 && ry >= 1.0) {
        // Narrower at every amplitude: `emit_halo` returns before any push.
        return Some(0);
    }
    let row_h = i64::from(((ch / HALO_ROW_DIV) as i32).max(2));
    let half_row_h = row_h as f32 * 0.5;
    let band = i64::from(g.ch);
    let oy = i64::from(g.origin_y);
    let cell_y0 = oy.checked_add(i64::from(cr).checked_mul(band)?)?;
    let top = i64::from((cy - ry).floor() as i32);
    let bot = i64::from((cy + ry).ceil() as i32);
    let fx_top = oy.checked_sub(i64::from(g.head))?;
    let fx_bot = oy.checked_add(i64::from(g.rows).checked_mul(band)?)?;
    let back = cell_y0
        .checked_sub(top)?
        .checked_add(row_h)?
        .checked_sub(1)?
        .div_euclid(row_h);
    let mut y = cell_y0.checked_sub(back.checked_mul(row_h)?)?;
    let mut total = 0usize;
    while y < bot {
        let v = (y as f32 + half_row_h - cy) / ry;
        let v2 = v * v;
        let w2 = 1.0 - v2;
        if w2 > 0.0 {
            let lo = y.max(fx_top);
            let hi = y.checked_add(row_h)?.min(fx_bot);
            if hi > lo {
                let first = lo.checked_sub(oy)?.div_euclid(band);
                let last = hi.checked_sub(1)?.checked_sub(oy)?.div_euclid(band);
                let bands = usize::try_from(last.checked_sub(first)?.checked_add(1)?).ok()?;
                total = total.checked_add(DARK_CALLS_PER_ROW.checked_mul(bands)?)?;
            }
        }
        y = y.checked_add(row_h)?;
    }
    Some(total)
}

/// `emit_light_halo`'s worst case over every amplitude: the
/// slabs that can meet the clip at `s = 1` times the rows a slab can cover,
/// capped by `ribbon_beam`'s own budget.
fn light_halo_budget((cr, cc): (u16, u16), geom: &Geom, g: &Grid, radius: f32) -> Option<usize> {
    let cw = geom.cw as f32;
    let (cx, _) = geom.cell_center(cr, cc);
    let rx_full = cw * (0.55 + radius * WIDEST);
    let x_first = cx - rx_full;
    let x_last = cx + rx_full;
    let (start, end) = (
        i64::from(x_first.ceil() as i32),
        i64::from(x_last.ceil() as i32),
    );
    let width = end.checked_sub(start)?.max(0);
    let left = i64::from(g.origin_x);
    let right = left.checked_add(i64::from(g.cols).checked_mul(i64::from(g.cw))?)?;
    let in_clip = end.min(right).checked_sub(start.max(left))?.max(0);
    let stride = i64::from((g.cw / 8).max(1));
    let every = width
        .checked_add(LIGHT_ELLIPSE_SEGMENTS.checked_mul(stride.checked_sub(1)?)?)?
        .div_euclid(stride);
    let meeting = if in_clip == 0 {
        0
    } else {
        in_clip
            .checked_add(
                LIGHT_ELLIPSE_SEGMENTS.checked_mul(stride.checked_mul(2)?.checked_sub(2)?)?,
            )?
            .div_euclid(stride)
    };
    let slabs = every.min(meeting);
    let oy = i64::from(g.origin_y);
    let band = i64::from(g.ch);
    let cell_top = oy.checked_add(i64::from(cr).checked_mul(band)?)?;
    let fx_top = oy.checked_sub(i64::from(g.head))?;
    let fx_bot = oy.checked_add(i64::from(g.rows).checked_mul(band)?)?;
    let rows_lo = cell_top.checked_sub(1)?.max(fx_top);
    let rows_hi = cell_top.checked_add(band)?.checked_add(1)?.min(fx_bot);
    let rows = rows_hi.checked_sub(rows_lo)?.max(0);
    let quads = usize::try_from(slabs.checked_mul(rows)?).ok()?;
    Some(quads.min(LIGHT_MARK_QUADS.checked_sub(1)?))
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
