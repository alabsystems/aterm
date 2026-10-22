// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input admission for hosted momentum glow, and the scratch budget each tick
//! may push into.
//!
//! When the glow runs as an Aos task, every input passes these checks before
//! it reaches the engine. Inside the admitted profile every engine call is
//! total and bounded: no integer overflow, no runaway loop, no
//! `Instant + Duration` that panics, and no push beyond a budget that is
//! reserved before the tick. Outside it, the input is refused before the call,
//! so the engine's state and the scratch vectors are untouched. The
//! functions are pure: they read their arguments and nothing else.
//!
//! # The profile
//!
//! [`MOMENTUM_PROFILE`] is the profile the host runs under:
//!
//! | limit | value | what it bounds |
//! |---|---|---|
//! | `max_extent_px` | 65 535 | `origin + cells · cell` on each axis: [`GlowQuad`](aterm_render::GlowQuad)'s `u16` pixels, and every `i32` the halo computes |
//! | `max_radius_cells` | 4.0 | the halo's reach, so both halo loops scale with the cell size (production: 1.6) |
//! | `max_tau_s` | 60 s | the settle schedule's addend `τ · ln 50 ≤ 235 s` (production: 1.2 s) |
//! | `max_frame_interval` | 1 s | `now + frame_interval` in `next_change_deadline` |
//! | `horizon` | 30 days | logical time, as a signed offset from the epoch the host took at attach |
//!
//! The derivations below hold only inside [`MOMENTUM_ENVELOPE`], the widest
//! profile the engine's arithmetic was checked for. A check passes only when
//! the value is inside both the caller's profile and the envelope. So a
//! hand-built profile that is wider than the envelope, or that has NaN
//! limits, cannot widen what is admitted.
//!
//! # What is refused
//!
//! * [`admit_tick`] runs these checks in this order:
//!   * geometry ([`AdmitRefusal::Geometry`]): `1 ≤ cw`, `1 ≤ ch`,
//!     `origin_x + max(cols, 1) · cw ≤ extent`,
//!     `origin_y + max(rows, 1) · ch ≤ extent` (an empty grid still names a
//!     cell that must fit), and `head ≤ origin_y`. All of it uses checked
//!     arithmetic;
//!   * `cfg.tau_s` ([`admit_tau`]);
//!   * `cfg.radius_cells`, which must be finite and in `[0, max]`
//!     ([`AdmitRefusal::Radius`]);
//!   * `cfg.intensity`, which must be finite ([`AdmitRefusal::Intensity`]);
//!     the engine clamps it to `[0, 1]`;
//!   * a glyph span, which must satisfy `0 ≤ first ≤ end ≤ cols`
//!     ([`AdmitRefusal::GlyphSpan`]).
//!
//!   The window extent (`win_w`, `win_h`) and the colours are never read by
//!   the engine, and any value is total, so they are admitted as they are. A
//!   cursor outside the grid is not refused either: the engine treats it as
//!   off.
//! * [`admit_tau`]: τ must be finite and in `(0, max_tau_s]`. This covers both
//!   `on_key`'s τ and the τ a tick latches.
//! * [`admit_frame_interval`]: the interval must be `≤ max_frame_interval`.
//! * [`admit_time`]: an input's time, as a signed nanosecond offset from the
//!   epoch, must be in `[0, horizon]` and no earlier than the previous input's.
//!
//! Every instant the engine forms is then at most
//! `epoch + horizon + max(frame_interval, τ · ln 50 + 1 ms)`. That is far
//! inside `Instant`'s range on every std target when the epoch is a real
//! `Instant::now()`, so the engine's unchecked additions
//! (`next_change_deadline`, `decays_to`) cannot overflow.
//!
//! # The scratch budget
//!
//! A hot tick pushes into exactly one stream: the dark page's halo goes to
//! `out` (`emit_halo`) and the light page's ink to `under` (`emit_light_halo`).
//! An inert tick pushes nothing. [`admit_tick`] returns a [`ScratchBudget`]
//! that bounds the pushes of every tick these inputs can make, whatever the
//! engine's state and the time. The host reserves it before the tick, so a
//! push never reallocates.
//!
//! *Every amplitude at once.* The halo's size depends on the amplitude only
//! through `s = smoothstep(q / 255) ≤ 1`, and `rx`, `ry` and `rx_full` grow with
//! `s`. IEEE rounding is monotone in each operation, so the computed floats
//! grow with `s` too. The row grid and the slab tiling are anchored where `s`
//! does not reach: the caret row's top edge, and `ceil` of the vertex `x`. So
//! the rows and slabs at any amplitude are a subset of those at `s = 1`, and
//! the budget is computed once, at `s = 1`, with the engine's own float
//! expressions.
//!
//! *Dark* (`emit_halo`): the rows lie on a grid `row_h = max(⌊ch / 12⌋, 2)`
//! apart, anchored at the caret row's top edge. A row works when
//! `1 − v² > 0`, and that set shrinks as `ry` shrinks. A working row makes 4
//! knot pairs × 2 sides = 8 `push_fx_ramp` calls. Each call clips to the
//! effects box `[fx_top, fx_bot)` and pushes one quad per `ch`-band, anchored
//! at `origin_y`, that the clipped row meets. The budget is
//! `Σ 8 · bands(row)` over the working rows at `s = 1`, counted by walking the
//! engine's row grid. It is never above the closed form
//! `16 · min(⌈2·ry/row_h⌉, ⌈(fx_bot − fx_top)/row_h⌉ + 1)`, because a row meets
//! at most two bands (`row_h ≤ ch`, or `row_h = 2` at `ch = 1`).
//!
//! *Light* (`emit_light_halo` → `soft_ellipse` → `ribbon_beam`): 25 vertices
//! span `[cx − rx_full, cx + rx_full]`. The 24 segments tile
//! `[⌈x₀⌉, ⌈x₂₄⌉)` (`W` px), and inside each segment the slabs are
//! `stride = max(⌊cw/8⌋, 1)` px wide. Only a slab that meets
//! `[fx_left, fx_right)` (`ℓ` px of the tiling) can push.
//! * Slabs: `min(⌊(W + 24·(stride − 1)) / stride⌋, ⌊(ℓ + 24·(2·stride − 2)) / stride⌋)`.
//!   The first term counts every slab. The second counts, per segment, the
//!   slabs that meet the clip, `⌈(ℓᵢ − 1)/stride⌉ + 1`.
//! * Rows per slab: one quad per pixel row, at most one. A vertex's reach is
//!   at most `ch / 2` (`push_ink_ellipse_shaped` holds the mark inside the
//!   caret row), and a lerp of two reaches overshoots by a few ulps at most,
//!   far under a pixel. So the rows lie in `[cell_top − 1, cell_top + ch + 1)`,
//!   clipped to `[fx_top, fx_bot)`.
//! * Total: `min(4095, slabs · rows)`. 4095 is the cap `ribbon_beam` enforces
//!   itself, a relative budget of `LIGHT_MARK_QUADS = 4096` checked as
//!   `len + 2 > max` before each row.
//!
//! The budget bounds pushes, not work. A light slab outside the clip still
//! walks its rows without pushing, so a light tick's row steps are bounded by
//! the whole tiling: about `(2·rx_full / stride + 24) · (ch + 2)`. That is
//! finite inside the profile, and it scales with the cell, not with the
//! window.
//!
//! The property sweep (`every_admitted_tick_pushes_within_its_budget_and_never_reallocates`)
//! checks both bounds against the real engine across geometries, radii,
//! amplitudes, themes and cursor positions.

use std::fmt;
use std::time::Duration;

use super::{HALO_ROW_DIV, HALO_SEGMENTS, MomentumGlowConfig};
use crate::cursor_glow::{Geom, LIGHT_MARK_QUADS};

/// The limits hosted momentum inputs are admitted under (the module docs'
/// table).
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

/// The widest profile the engine's arithmetic was checked for. Admission
/// holds every profile to it.
/// * Extent `u16::MAX`: `GlowQuad` pixels are `u16`.
/// * Radius 64 cells: `ry ≤ 38.9 · ch` and `rx ≤ 64.6 · cw` stay below 2²² px,
///   so every `f32 → i32` in the halo is exact and unsaturated.
/// * τ one hour, frame interval one minute, horizon ten years: every instant
///   the engine forms stays within about ten years of the epoch.
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

/// The most quads one tick may append to each scratch stream. The host
/// reserves this capacity before the tick.
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
    /// The scratch budget's checked arithmetic overflowed. Inside the envelope
    /// this cannot happen. It is refused rather than guessed.
    Unbounded,
}

/// Why an input was refused before it reached the engine. A refusal leaves
/// the engine's state and the scratch streams untouched.
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
    /// A time earlier than the previous input's.
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

/// Admits a τ, either `on_key`'s or a tick's `cfg.tau_s`: finite and in
/// `(0, max_tau_s]`.
///
/// # Errors
///
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

/// Admits a schedule query's frame interval: at most `max_frame_interval`.
///
/// # Errors
///
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

/// Admits an input's logical time `at` (signed nanoseconds from the epoch)
/// after an input at `last`. The time must be inside `[0, horizon]` and
/// nondecreasing. The range is checked first.
///
/// # Errors
///
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
    // Every `Duration` is below 2^94 ns, so its nanoseconds fit in `i128`.
    let within = horizon_ns <= i128::MAX as u128 && at >= 0 && at <= horizon_ns as i128;
    if !within {
        return Err(AdmitRefusal::Horizon(at));
    }
    match last {
        Some(last) if at < last => Err(AdmitRefusal::TimeWentBack { last, at }),
        _ => Ok(()),
    }
}

/// Admits one tick's inputs and returns the scratch it may push (the module
/// docs list the checks and derive the budget). A tick that cannot be hot gets
/// a zero budget: no cursor, a cursor outside the grid, disabled, or
/// intensity ≤ 0.
///
/// # Errors
///
/// [`AdmitRefusal::Geometry`], [`AdmitRefusal::Tau`], [`AdmitRefusal::Radius`],
/// [`AdmitRefusal::Intensity`] or [`AdmitRefusal::GlyphSpan`], in that order.
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
    // The engine's static inert arm: nothing can be pushed.
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
    // `origin + max(cells, 1) · cell ≤ limit`, checked; a pass bounds every
    // factor by `limit ≤ u16::MAX`.
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

/// `emit_halo`'s worst case over every amplitude (the module docs): the
/// engine's row grid at `s = 1`, eight calls per working row, one quad per
/// `ch`-band each call's clipped row meets. Mirrors `emit_halo`'s float
/// expressions exactly; `None` only if checked arithmetic overflows.
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

/// `emit_light_halo`'s worst case over every amplitude (the module docs): the
/// slabs that can meet the clip at `s = 1` times the rows a slab can cover,
/// capped by `ribbon_beam`'s own budget. `None` only if checked arithmetic
/// overflows.
fn light_halo_budget((cr, cc): (u16, u16), geom: &Geom, g: &Grid, radius: f32) -> Option<usize> {
    let cw = geom.cw as f32;
    let (cx, _) = geom.cell_center(cr, cc);
    let rx_full = cw * (0.55 + radius * WIDEST);
    // `soft_ellipse`'s first and last vertices, `cx + u · rx_full` at `u = -1`
    // and `u = 1` exactly; `±1 · rx_full` is exact, so these are the same floats.
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
