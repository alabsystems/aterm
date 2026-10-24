// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companion router: which cat owns the frame, where the flying head sits, and what one impulse does to each body.

use aterm_time::Instant;

use crate::companion::CompanionDuty;
use crate::cursor_glow::Geom;
use crate::kitty_pet::PetFrame;
use crate::word_decorations::CatFootprint;

use super::timing;
use super::{CompanionImpulse, Dir};

/// `disp = max(disp, 0.97)`.
pub const FLYING_DISP_FLOOR: f32 = 0.97;

/// `lead = −0.30` cell at `t = 0` (§7.2, row "Whip"): the flying head is yanked back against travel on the frame the caret teleports.
pub const FLYING_LEAD_0: f32 = -0.30;

/// `LEAD_MAX = +0.22` cell.
pub const FLYING_LEAD_MAX: f32 = 0.22;

/// The 1-px look-back toward the launch lasts **250 ms** after arrival (§7.2, row "Eyes").
pub const FLYING_LOOKBACK_MS: f32 = 250.0;

/// The squint holds 200 ms past the arrival edge (§7.2, row "Eyes").
pub const FLYING_SQUINT_TAIL_MS: f32 = 200.0;

/// Which body owns this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The resident pet ([`crate::kitty_pet`]): the default, and what `rainbow kitty` / `kitty` / `rainbow kitty pet` mean.
    Pet,
    /// The flying head ([`crate::kitty_cursor`]): `rainbow kitty flying` and the bare `nyan` / `rainbow` aliases.
    Flying {
        /// The caret cell the head escorts this frame (`CompanionDuty::FlyingHead { cell }`),
        /// so seat and snap-to resolve against one cell.
        cell: (u16, u16),
    },
    /// No companion this frame; the previous body keeps whatever exit its own animator gives it.
    None,
}

/// What the host already decided about companions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionAdmission {
    /// The host's resolved companion custody for this frame.
    pub duty: CompanionDuty,
    /// Another producer already claimed this frame's single companion body (`WordDecorations::claim_companion_body`).
    pub body_claimed: bool,
}

/// Which body: nothing when the frame's body is claimed, else the host's [`CompanionDuty`] (`Idle` ⇒ none, `Pet` ⇒ the pet, `FlyingHead { cell }` ⇒ the head escorting `cell`).
#[inline]
#[must_use]
pub const fn body_for(admitted: CompanionAdmission) -> Body {
    if admitted.body_claimed {
        return Body::None;
    }
    match admitted.duty {
        CompanionDuty::Idle => Body::None,
        CompanionDuty::Pet => Body::Pet,
        CompanionDuty::FlyingHead { cell } => Body::Flying { cell },
    }
}

/// The two poses both bodies already own, unchanged by v2 (§7.2: "Delight / Wince / Oops are unchanged"), plus the bare landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// A landing with no flight behind it.
    Land,
    /// An erase or a kill — the existing "oops" pose.
    Wince,
    /// The existing Delight pose.
    Delight,
}

/// The flying head's move: §7.2(a)'s beat table as data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flight {
    /// Travel direction, dominant axis — the existing `facing_left` bank.
    pub dir: Dir,
    /// The spawn edge.
    pub t0: Instant,
    /// The arrival edge `t₀ + T` (§6.7), from [`arrival`]: read by the pin, ring, fan, flash, audio bell and landing squash.
    pub land_at: Instant,
    /// The snap-to: the landing cell, i.e. the caret the head escorts on the frame the impulse was minted (the frame the caret was first observed there).
    pub land: (u16, u16),
}

impl Flight {
    /// The spine value forced on frame 0 ([`FLYING_DISP_FLOOR`]), a floor so a cat already at full momentum is not pulled down.
    #[inline]
    #[must_use]
    pub const fn disp_floor(&self) -> f32 {
        FLYING_DISP_FLOOR
    }

    /// The whip in cells at `now`.
    #[inline]
    #[must_use]
    pub fn lead_at(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.t0).as_secs_f32();
        FLYING_LEAD_0 + (FLYING_LEAD_MAX - FLYING_LEAD_0) * timing::spring_whip(age)
    }

    /// The squint (§7.2(a), row "Eyes") holds through the flight and [`FLYING_SQUINT_TAIL_MS`] past the arrival edge.
    #[inline]
    #[must_use]
    pub fn squint_until(&self) -> Instant {
        add_ms(self.land_at, FLYING_SQUINT_TAIL_MS)
    }

    /// The 1-px look-back toward the launch ends here ([`FLYING_LOOKBACK_MS`] after arrival).
    #[inline]
    #[must_use]
    pub fn settled_at(&self) -> Instant {
        add_ms(self.land_at, FLYING_LOOKBACK_MS)
    }
}

/// One impulse, read by one body (D13): what [`impulse_for`] resolved a [`CompanionImpulse`] into for the body that owns the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyImpulse {
    /// The flying head's meteor, whole: teleport, spine impulse, whip, squash on [`Flight::land_at`].
    Fly(Flight),
    /// The pet's whole share (§7.2(b), D13): the meteor's arrival edge `t₀ + T`, offered as its `PERK_HOLD` edge.
    Perk {
        /// The arrival edge `t₀ + T`, offered.
        at: Instant,
    },
    /// A pose both bodies already own, handed over as an edge.
    React(Reaction),
    /// This impulse means nothing to this body (every impulse, when no body owns the frame).
    Ignore,
}

/// One impulse, two readings (§7.2, D13): route a [`CompanionImpulse`] to what the owning body does with it.
#[inline]
#[must_use]
pub fn impulse_for(body: Body, impulse: CompanionImpulse) -> BodyImpulse {
    match (body, impulse) {
        (Body::None, _) => BodyImpulse::Ignore,
        (Body::Flying { cell }, CompanionImpulse::Meteor { dir, t0, t_flight }) => {
            BodyImpulse::Fly(Flight {
                dir,
                t0,
                land_at: arrival(t0, t_flight),
                land: cell,
            })
        }
        (Body::Pet, CompanionImpulse::Meteor { t0, t_flight, .. }) => BodyImpulse::Perk {
            at: arrival(t0, t_flight),
        },
        (_, CompanionImpulse::Land) => BodyImpulse::React(Reaction::Land),
        (_, CompanionImpulse::Wince) => BodyImpulse::React(Reaction::Wince),
        (_, CompanionImpulse::Delight) => BodyImpulse::React(Reaction::Delight),
    }
}

/// The arrival edge `t₀ + T` (§6.7), derived once so the squash and the perk offer agree.
#[inline]
fn arrival(t0: Instant, t_flight: std::time::Duration) -> Instant {
    t0.checked_add(t_flight).unwrap_or(t0)
}

/// How many m1 heroes this impulse earns (§5.8, §7.2): one for Delight, zero otherwise.
#[inline]
#[must_use]
pub const fn heroes_earned(impulse: CompanionImpulse) -> u8 {
    match impulse {
        CompanionImpulse::Delight => 1,
        CompanionImpulse::Meteor { .. } | CompanionImpulse::Land | CompanionImpulse::Wince => 0,
    }
}

/// Which side of the caret the body took, by the body's centre against the caret cell's centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Past the caret, escorting the writing — the shipped seat.
    Ahead,
    /// Behind the caret: the ink ahead put it there (step 2 of the seat law).
    Behind,
}

/// The resolved seat of the flying head, in the [`CatFootprint`] vocabulary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seat {
    /// Sprite left edge, grid-interior px.
    pub x: i32,
    /// Sprite top edge, grid-interior px.
    pub y: i32,
    /// Sprite width in px, as the baker sized it.
    pub w: u16,
    /// Sprite height in px, as the baker sized it.
    pub h: u16,
    /// [`Self::x`] in fractional grid columns, derived after every clamp.
    pub col: f32,
    /// The grid row the body sits on, already lifted.
    pub row: u16,
    /// Which side of the caret the body's centre landed on.
    pub side: Side,
    /// Rows lifted above the caret's own row (0 or 1).
    pub lift_rows: u16,
    /// Every yield was inked (or covered the caret cell), so the body draws over the words here, as the shipped seat does on a crowded line.
    pub over_ink: bool,
}

/// One [`placement`] call's geometry.
#[derive(Clone, Copy)]
pub struct SeatQuery {
    /// Window geometry, in px — read for `cw`, `ch` and `cols` only.
    pub geom: Geom,
    /// The shipped seat as `kitty_cursor_footprint` resolved it this frame (¾-cell lead, right-margin clamp, boundary rise, hover bob).
    pub rest: CatFootprint,
    /// The shipped lead in px (`cw · KITTY_LEAD_NUM / KITTY_LEAD_DEN`), handed in by the emitter that owns the constant, so the mirror seat keeps the same clearance.
    pub lead_px: i32,
    /// The head sat in the sky seat (step 3, clear) on this caret row last frame.
    pub held_sky: bool,
}

/// The seat rule (§6.5 layer 13).
#[must_use]
pub fn placement<F>(caret: (u16, u16), query: &SeatQuery, ink: F) -> Seat
where
    F: Fn(u16, u16) -> Option<bool>,
{
    let geom = query.geom;
    let rest = query.rest;
    let (caret_row, caret_col) = caret;
    let cw = i32::try_from(geom.cw.max(1)).unwrap_or(i32::MAX);
    let ch = i32::try_from(geom.ch.max(1)).unwrap_or(i32::MAX);
    let cols = u16::try_from(geom.cols).unwrap_or(u16::MAX);
    let w = i32::from(rest.w);
    let caret_left = i32::from(caret_col).saturating_mul(cw);
    let grid_w = i32::from(cols).saturating_mul(cw);

    let ahead = census(rest.x, w, caret_row, caret, cols, cw, &ink);
    if ahead.clear() {
        return seat(query, rest.x, rest.y, caret_row, caret_left, 0, false);
    }

    let mut sky_census = None;
    if caret_row > 0
        && ahead.probe != Probe::Blind
        && (query.held_sky
            || (ahead.covers_caret
                && (caret_col.saturating_sub(WRITTEN_REACH)..caret_col)
                    .any(|c| ink(caret_row, c) == Some(true))))
    {
        let sky_row = caret_row - 1;
        let sky = census(rest.x, w, sky_row, caret, cols, cw, &ink);
        if sky.clear() {
            return seat(
                query,
                rest.x,
                rest.y.saturating_sub(ch),
                sky_row,
                caret_left,
                1,
                false,
            );
        }
        sky_census = Some(sky);
    }

    let behind_x = caret_left
        .saturating_sub(query.lead_px)
        .saturating_sub(w)
        .clamp(0, (grid_w - w).max(0));

    if ahead.probe == Probe::Blind {
        let shift_behind = ahead.covers_caret && cols > 0 && {
            let (b_first, b_last) = cell_span(behind_x, w, cw, cols);
            !(b_first..=b_last).contains(&caret_col)
        };
        let x = if shift_behind { behind_x } else { rest.x };
        return seat(query, x, rest.y, caret_row, caret_left, 0, false);
    }

    let behind = census(behind_x, w, caret_row, caret, cols, cw, &ink);
    if behind.clear() {
        return seat(query, behind_x, rest.y, caret_row, caret_left, 0, false);
    }

    if caret_row > 0 {
        let sky_row = caret_row - 1;
        let sky_y = rest.y.saturating_sub(ch);
        let sky = sky_census.unwrap_or_else(|| census(rest.x, w, sky_row, caret, cols, cw, &ink));
        return seat(query, rest.x, sky_y, sky_row, caret_left, 1, !sky.clear());
    }

    if !ahead.covers_caret {
        return seat(query, rest.x, rest.y, 0, caret_left, 0, true);
    }
    if !behind.covers_caret {
        return seat(query, behind_x, rest.y, 0, caret_left, 0, true);
    }
    seat(query, rest.x, rest.y, 0, caret_left, 0, true)
}

/// Cells before the caret in which ink makes the caret's line one being
/// written ([`placement`] step 3): wider than the mirror seat, so the last
/// keys' glyphs still unechoed do not read as blank glass.
const WRITTEN_REACH: u16 = 8;

/// What the ink probe said about the cells a span would cover, less the caret
/// cell, which needs no probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    /// Every probed cell answered `Some(false)`.
    Clear,
    /// Some probed cell answered `Some(true)`, or answered `None` beside cells
    /// that could be probed.
    Inked,
    /// Every cell answered `None`: the host has no ink information.
    Blind,
}

/// One span's census: whether it covers the caret cell (arithmetic), and what
/// the probe said about the rest of it.
#[derive(Clone, Copy, Debug)]
struct Census {
    covers_caret: bool,
    probe: Probe,
}

impl Census {
    /// A seat this span may be taken at without yielding.
    #[inline]
    const fn clear(self) -> bool {
        !self.covers_caret && matches!(self.probe, Probe::Clear)
    }
}

/// Census the cells a body `w` px wide with its left edge at `x` px covers on
/// `row`.
#[inline]
fn census<F>(x: i32, w: i32, row: u16, caret: (u16, u16), cols: u16, cw: i32, ink: &F) -> Census
where
    F: Fn(u16, u16) -> Option<bool>,
{
    if cols == 0 {
        return Census {
            covers_caret: true,
            probe: Probe::Inked,
        };
    }
    let (first, last) = cell_span(x, w, cw, cols);
    let covers_caret = row == caret.0 && (first..=last).contains(&caret.1);
    let mut inked = false;
    let mut blank = 0_u32;
    let mut unknown = 0_u32;
    for c in first..=last {
        if (row, c) == caret {
            continue;
        }
        match ink(row, c) {
            Some(true) => {
                inked = true;
                break;
            }
            Some(false) => blank += 1,
            None => unknown += 1,
        }
        if unknown > 0 && blank > 0 {
            inked = true;
            break;
        }
    }
    let probe = if inked {
        Probe::Inked
    } else if unknown > 0 {
        Probe::Blind
    } else {
        Probe::Clear
    };
    Census {
        covers_caret,
        probe,
    }
}

/// The inclusive cell columns a body `w` px wide at `x` px covers, clamped
/// into `0..cols`.
#[inline]
const fn cell_span(x: i32, w: i32, cw: i32, cols: u16) -> (u16, u16) {
    let last_col = cols.saturating_sub(1) as i32;
    let first_raw = x.div_euclid(cw);
    let first = if first_raw < 0 {
        0
    } else if first_raw > last_col {
        last_col
    } else {
        first_raw
    };
    let last_raw = (x.saturating_add(w).saturating_sub(1)).div_euclid(cw);
    let last = if last_raw < first {
        first
    } else if last_raw > last_col {
        last_col
    } else {
        last_raw
    };
    (first as u16, last as u16)
}

/// Build the [`Seat`] for a resolved `x`/`y`. `col` is read off the final `x`
/// so the struct carries one position, not two.
#[inline]
const fn seat(
    query: &SeatQuery,
    x: i32,
    y: i32,
    row: u16,
    caret_left: i32,
    lift_rows: u16,
    over_ink: bool,
) -> Seat {
    let cw_usize = if query.geom.cw > 1 { query.geom.cw } else { 1 };
    let cw = if cw_usize > i32::MAX as usize {
        i32::MAX
    } else {
        cw_usize as i32
    };
    let w = query.rest.w as i32;
    let side = if x + w / 2 > caret_left + cw / 2 {
        Side::Ahead
    } else {
        Side::Behind
    };
    Seat {
        x,
        y,
        w: query.rest.w,
        h: query.rest.h,
        col: x as f32 / cw as f32,
        row,
        side,
        lift_rows,
        over_ink,
    }
}

/// The catch's reach: how many columns along its line, from the nearest edge of its body ([`PetOnGlass::span`]), a settled cat is offered a star.
pub const CATCH_REACH_CELLS: f32 = 10.0;

/// The pet as the sky sees it, read off its last [`PetFrame`] ([`PetOnGlass::of`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetOnGlass {
    /// The body's left edge in fractional grid columns ([`PetFrame::col`], after the emitter's own clamps).
    pub col: f32,
    /// The body's width in fractional grid columns.
    pub width: f32,
    /// The grid row the pet's feet are on.
    pub row: u16,
    /// [`crate::kitty_pet::PetAction::settled`]: at rest on the ground.
    pub settled: bool,
    /// [`PetFrame::purr`]: the purr's intensity while its tell is up, else `0.0`.
    pub purr: f32,
    /// [`PetFrame::contented`]: the same ledger over `PURR_GATE`, whatever the pose.
    pub contented: bool,
}

impl PetOnGlass {
    /// Read off the pet's published frame; `None` when the pet is not on glass (`alpha == 0`) or the cell metrics are degenerate.
    #[inline]
    #[must_use]
    pub fn of(frame: &PetFrame, geom: Geom) -> Option<Self> {
        let cell_w = u16::try_from(geom.cw).ok()?;
        let cell_h = u16::try_from(geom.ch).ok()?;
        let cols = u16::try_from(geom.cols).ok()?;
        let rows = u16::try_from(geom.rows).ok()?;
        let (x0, x1, _, _) = frame.body_px(cell_w, cell_h, cols, rows)?;
        let cw = f32::from(cell_w);
        if cw <= 0.0 {
            return None;
        }
        Some(Self {
            col: x0 as f32 / cw,
            width: (x1 - x0) as f32 / cw,
            row: u16::try_from(frame.row.round().max(0.0) as u32).unwrap_or(u16::MAX),
            settled: frame.action.settled(),
            purr: frame.purr,
            contented: frame.contented,
        })
    }

    /// The body's column span, `[left, right]`, in fractional grid columns.
    #[inline]
    #[must_use]
    pub const fn span(&self) -> (f32, f32) {
        (self.col, self.col + self.width.max(0.0))
    }

    /// The offer gate: settled and contented (the purr tell, or the ledger over the pet's own gate whatever the pose), both the pet's own verdicts.
    #[inline]
    #[must_use]
    pub const fn contented(&self) -> bool {
        self.settled && (self.purr > 0.0 || self.contented)
    }

    /// Distance in columns from the body's nearest edge to grid column `col`; `0` under the body.
    #[inline]
    #[must_use]
    pub const fn columns_to(&self, col: i32) -> f32 {
        let (lo, hi) = self.span();
        let c = col as f32;
        (lo - c).max(c - hi).max(0.0)
    }
}

/// The share of a cell the pet tile's transparent border takes at each side (`kitty_pet`'s `INK_PAD`): no fur stands there.
const SHADE_BORDER_CELLS: f32 = 0.30;

/// The pet's body as the sky draws around it: a star whose centre lies on the
/// body is not drawn, so the cat flies in front of the sky and no star sits
/// on its fur (on paper, purple sky stars would stand on a flying
/// companion's face and back, `burst` t0). The pet ticks after the sky emits,
/// so the shade is the body's last box joined with that box carried on by
/// its last frame's move (the box it is about to be drawn at), each shaved
/// of the tile's transparent border at the sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PetShade {
    /// The last two drawn boxes, `(x0, x1, y0, y1)` in window pixels, newest first.
    boxes: [Option<(i32, i32, i32, i32)>; 2],
}

impl PetShade {
    /// Note the body's drawn box this frame (`None`: off glass), window pixels.
    #[inline]
    pub const fn note(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.boxes = [body, if body.is_some() { self.boxes[0] } else { None }];
    }

    /// The body box noted last (`None`: off glass, or never noted), window pixels.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn body(&self) -> Option<(i32, i32, i32, i32)> {
        self.boxes[0]
    }

    /// Whether a star centred at `(x, y)` (window pixels) lies on the body.
    #[must_use]
    pub fn hides(&self, (x, y): (i32, i32), cell_w: usize) -> bool {
        let Some(last) = self.boxes[0] else {
            return false;
        };
        let border = (SHADE_BORDER_CELLS * cell_w as f32).round() as i32;
        if x >= last.0 + border && x < last.1 - border && y >= last.2 && y < last.3 {
            return true;
        }
        let Some(prev) = self.boxes[1] else {
            return false;
        };
        let (dx, dy) = (last.0 - prev.0, last.2 - prev.2);
        if dx == 0 && dy == 0 {
            return false;
        }
        let (x0, x1, y0, y1) = (last.0 + dx, last.1 + dx, last.2 + dy, last.3 + dy);
        x >= x0 + border && x < x1 - border && y >= y0 && y < y1
    }
}

/// What the sky and the ribbon's top keep off: the frame's word icons (a
/// `dog`'s or a `fox`'s face drawn over its word) and every lettered or
/// filled cell (a powerline segment, a selection) of a row the caret is not
/// on, as window-pixel boxes `(x0, x1, y0, y1)`, right and bottom exclusive.
/// No star is drawn on them and the ribbon's body never feathers up into
/// them. The gallery's judges, 2026-10-23 (looks6 N3, N4): sky stars on the
/// dog's face and pink sparkles on the fox's, the Enter flare washing the
/// fox's white chin lilac, a bright star on the `2` of `main +2` and a green
/// one in its gap, and the bed's top feather tinting the segment's bottom
/// 3-4 px (yellow to olive, its grey bar to violet).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct KeepOut {
    /// Every box: the word icons' and the lettered or filled cells'.
    boxes: Vec<(i32, i32, i32, i32)>,
    /// The lettered or filled cells' boxes alone ([`Self::below`]).
    cells: Vec<(i32, i32, i32, i32)>,
    /// Each lettered row's span, first lettered or filled cell to last, a
    /// row tall ([`Self::behind_text`]).
    rows: Vec<(i32, i32, i32, i32)>,
}

impl Clone for KeepOut {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            boxes: self.boxes.clone(),
            cells: self.cells.clone(),
            rows: self.rows.clone(),
        }
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.boxes.clone_from(&source.boxes);
        self.cells.clone_from(&source.cells);
        self.rows.clone_from(&source.rows);
    }
}

impl KeepOut {
    /// This frame's boxes, window pixels, all of them lettered or filled
    /// cells ([`Self::set_split`] with no icon).
    #[inline]
    pub fn set(&mut self, boxes: impl IntoIterator<Item = (i32, i32, i32, i32)>) {
        self.set_split(std::iter::empty(), boxes);
    }

    /// This frame's boxes, window pixels: the word icons' sprite boxes and
    /// the lettered or filled cells' runs.
    #[inline]
    pub fn set_split(
        &mut self,
        icons: impl IntoIterator<Item = (i32, i32, i32, i32)>,
        cells: impl IntoIterator<Item = (i32, i32, i32, i32)>,
    ) {
        let real = |b: &(i32, i32, i32, i32)| b.0 < b.1 && b.2 < b.3;
        self.cells.clear();
        self.cells.extend(cells.into_iter().filter(real));
        self.boxes.clear();
        self.boxes.extend(icons.into_iter().filter(real));
        self.boxes.extend_from_slice(&self.cells);
    }

    /// This frame's lettered rows' spans, window pixels: each row's first
    /// lettered or filled cell to its last, the row's whole height.
    #[inline]
    pub fn set_rows(&mut self, rows: impl IntoIterator<Item = (i32, i32, i32, i32)>) {
        self.rows.clear();
        self.rows
            .extend(rows.into_iter().filter(|b| b.0 < b.1 && b.2 < b.3));
    }

    /// The lettered rows' spans ([`Self::set_rows`]).
    #[inline]
    #[must_use]
    pub fn rows(&self) -> &[(i32, i32, i32, i32)] {
        &self.rows
    }

    /// How much of a light at `(x, y)` shows in front of the letters: 0
    /// inside a lettered row's span between `band`'s shares of its height
    /// (its letters' bodies, the cap line to the baseline), widened by
    /// `pad` px on every side, 1 clear of it, eased over `feather` px
    /// inside its edges. A thread laid across a word passes behind its
    /// letters instead of striking them through, and with no `pad` shows
    /// again below their baseline.
    #[inline]
    #[must_use]
    pub fn behind_text(&self, (x, y): (f32, f32), band: (f32, f32), pad: f32, feather: f32) -> f32 {
        if self.rows.is_empty() {
            return 1.0;
        }
        let feather = feather.max(1.0);
        let mut k = 1.0f32;
        for &(x0, x1, y0, y1) in &self.rows {
            let (x0, x1) = (x0 as f32 - pad, x1 as f32 + pad);
            let dx = (x - x0).min(x1 - x);
            if dx <= 0.0 {
                continue;
            }
            let ch = (y1 - y0) as f32;
            let (b0, b1) = (y0 as f32 + band.0 * ch - pad, y0 as f32 + band.1 * ch + pad);
            let d = dx.min(y - b0).min(b1 - y);
            if d <= 0.0 {
                continue;
            }
            if d >= feather {
                return 0.0;
            }
            let u = (d / feather).min(1.0);
            k = k.min(1.0 - u * u * (3.0 - 2.0 * u));
        }
        k
    }

    /// The boxes, window pixels.
    #[inline]
    #[must_use]
    pub fn boxes(&self) -> &[(i32, i32, i32, i32)] {
        &self.boxes
    }

    /// Whether a mark centred at `(x, y)` that reaches `reach` px round its
    /// centre touches a box.
    #[inline]
    #[must_use]
    pub fn covers(&self, (x, y): (i32, i32), reach: i32) -> bool {
        let r = reach.max(0);
        self.boxes
            .iter()
            .any(|&(x0, x1, y0, y1)| x + r >= x0 && x - r < x1 && y + r >= y0 && y - r < y1)
    }

    /// What is left of a column's span `(y0, y1)` across `(x0, x1)` once
    /// every lettered or filled cell's box it meets is cut off its top: the
    /// span below the lowest box bottom it meets (empty when a box reaches
    /// its foot). A word icon is not among them: the frame's shade cuts a
    /// light by the face's drawn rows (`CursorGlow::shade_word_icons`), and
    /// a column cut whole by the icon's sprite box, which is wider than the
    /// face, carved a square notch out of the sky beside the dog's face
    /// (looks6 round 3).
    #[inline]
    #[must_use]
    pub fn below(&self, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> (i32, i32) {
        let top = self
            .cells
            .iter()
            .filter(|b| b.0 < x1 && x0 < b.1 && b.2 < y1 && y0 < b.3)
            .map(|b| b.3)
            .fold(y0, i32::max);
        (top.min(y1), y1)
    }

    /// How far up a body standing on a row whose top is `row_top` may reach
    /// at window x `x`, px: `row_top` under a box that stands on the row's
    /// top, easing back to `free` (no ceiling) over `ease` px beside it.
    #[inline]
    #[must_use]
    pub fn ceiling(&self, x: f32, row_top: f32, free: f32, ease: f32) -> f32 {
        if self.boxes.is_empty() || ease <= 0.0 || !ease.is_finite() {
            return free;
        }
        let above = row_top - 1.0;
        let mut d = ease;
        for &(x0, x1, y0, y1) in &self.boxes {
            if (y0 as f32) <= above && above < y1 as f32 {
                let dx = (x0 as f32 - x).max(x - x1 as f32);
                if dx <= 0.0 {
                    return row_top;
                }
                d = d.min(dx);
            }
        }
        if d >= ease {
            return free;
        }
        let u = (d / ease.max(1.0)).clamp(0.0, 1.0);
        let s = u * u * (3.0 - 2.0 * u);
        row_top + (free - row_top) * s
    }
}

/// One star offered to a contented cat, named by the triple that identifies it in the sky's pool.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarCatch {
    /// The star's home pixel, window-absolute — pinned at birth (§5.4).
    pub x: f32,
    /// The star's home pixel, window-absolute.
    pub y: f32,
    /// The birth edge, so a stale offer can never spend a new star's life.
    pub born: Instant,
    /// The grid column the star is over — what the pet aims the paw at.
    pub col: i32,
}

/// What v2 offers the resident pet this frame, minted by `Engine::pet_offer`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PetOffer {
    /// (a) The perk edge `t₀ + T` (D13), the same `Instant` the landing pin, arrival flash, caret flare and audio bell read (§6.7).
    pub perk_at: Option<Instant>,
    /// (b) The star: a gold m1 within [`CATCH_REACH_CELLS`] of a contented, settled cat.
    pub catch: Option<StarCatch>,
    /// (c) The purr's hue: the ribbon's field `t` at the cell under the pet, `None` where no ribbon light is laid, so the cat's ♪/♥ share the colour of the light there.
    pub mote_t: Option<f32>,
    /// [`Self::mote_t`] resolved to RGB, snapped to one of the seven stops (`spectrum_snap`) like every point mark.
    pub mote_rgb: Option<u32>,
}

impl PetOffer {
    /// True when this frame offers the pet nothing, the common case.
    #[must_use]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.perk_at.is_none() && self.catch.is_none() && self.mote_t.is_none()
    }
}

/// The seat law's ink probe ([`placement`]) over a frame's cells: whether
/// (`row`, `col`) holds a letter. A row the frame carries holds only its
/// written columns; the columns past them, inside the grid's `cols`, are the
/// frame's implicit blank (unwritten glass), not unknown. Read as unknown,
/// they would make every probe of a head that sits past a line's last letter
/// blind, and the blind seat would put it behind the caret, on the letters: a
/// kitten-size nyan head at the end of `... the kitty flies` typed to two
/// cells short of the margin would sit on `flies` from the last key through
/// its landing (the trail gallery's `burst` on GitHub Light, 1.1 to 2.6:1
/// under the head) where a larger head, reaching back over a letter, rises
/// into the row above. A row the frame does not carry, or a column past the
/// grid, is unknown.
#[inline]
#[must_use]
pub fn cell_ink(
    cells: &[Vec<aterm_core::terminal::RenderCell>],
    row: u16,
    col: u16,
    cols: u16,
) -> Option<bool> {
    if col >= cols {
        return None;
    }
    let line = cells.get(usize::from(row))?;
    Some(
        line.get(usize::from(col))
            .is_some_and(|cell| !matches!(cell.ch, ' ' | '\0')),
    )
}

/// `at + ms`, saturating. The module's one ms-to-`Instant` conversion, so nothing rounds to
/// whole milliseconds and lands a frame early.
#[inline]
fn add_ms(at: Instant, ms: f32) -> Instant {
    at.checked_add(std::time::Duration::from_secs_f32(ms / 1000.0))
        .unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const CW: i32 = 9;
    const CH: i32 = 18;
    /// The shipped footprint at `ch = 18`: ~6 cells wide, 1.7 `ch` tall.
    const BODY_W: u16 = 54;
    const BODY_H: u16 = 30;
    /// `cw · KITTY_LEAD_NUM / KITTY_LEAD_DEN` at `cw = 9`.
    const LEAD_PX: i32 = CW * 3 / 4;

    fn geom_cols(cols: usize) -> Geom {
        Geom {
            cw: CW as usize,
            ch: CH as usize,
            rows: 40,
            cols,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    /// The shipped seat as a fixture: `kitty_cursor_footprint`'s ¾-cell lead,
    /// right-margin clamp and boundary rise, bob 0.
    fn footprint(caret: (u16, u16), cols: u16) -> CatFootprint {
        let grid_w = i32::from(cols) * CW;
        let cursor_right = (i32::from(caret.1) + 1) * CW;
        let x = (cursor_right + LEAD_PX).min(grid_w - i32::from(BODY_W));
        assert!(x >= 0, "fixture: the grid is narrower than the cat");
        let rest_top = i32::from(caret.0) * CH + CH / 2 - i32::from(BODY_H) / 2;
        let intrusion = cursor_right - x;
        let rise = if intrusion > 0 {
            intrusion.min(CH / 2).min(rest_top.max(0))
        } else {
            0
        };
        CatFootprint {
            x,
            y: rest_top - rise,
            w: BODY_W,
            h: BODY_H,
        }
    }

    /// The head at a line's end, typed up to the margin: the line is written
    /// up to the cells the mirror seat would cover, which are blank (spaces, or
    /// keys whose echo has not landed), and the row above is blank.
    #[test]
    fn at_the_line_s_end_the_head_rises_once_and_stays() {
        let walk = |held: bool, margin: bool| -> Vec<(u16, u16, Side)> {
            let mut held_sky = false;
            let mut seats = Vec::new();
            for caret_col in 110..120_u16 {
                let caret = (7_u16, caret_col);
                let reach = if margin { 7 } else { 9 };
                let ink = |r: u16, c: u16| Some(r == 7 && c + reach < caret_col);
                let s = placement(
                    caret,
                    &SeatQuery {
                        held_sky: held && held_sky,
                        ..seat_query(caret, 120)
                    },
                    ink,
                );
                held_sky = s.lift_rows > 0 && !s.over_ink;
                seats.push((caret_col, s.lift_rows, s.side));
            }
            seats
        };
        let seats = walk(true, true);
        assert!(
            seats
                .iter()
                .all(|&(_, lift, side)| lift == 1 || side == Side::Ahead),
            "the head took the mirror at the line's end: {seats:?}"
        );
        let rose = seats.iter().position(|s| s.1 == 1).expect("it rises");
        assert!(
            seats[rose..].iter().all(|s| s.1 == 1),
            "it came down again before the line's end: {seats:?}"
        );
        let far = walk(true, false);
        assert!(
            far.iter()
                .any(|&(_, lift, side)| lift == 0 && side == Side::Behind),
            "a line not written near the caret keeps the mirror at the margin: {far:?}"
        );

        let mid = |held: bool| -> Vec<u16> {
            let mut held_sky = false;
            let mut lifts = Vec::new();
            for caret_col in 40..60_u16 {
                let caret = (7_u16, caret_col);
                let ink = |r: u16, c: u16| {
                    Some(r == 7 && (c > caret_col || (caret_col % 2 == 1 && c < caret_col)))
                };
                let s = placement(
                    caret,
                    &SeatQuery {
                        held_sky: held && held_sky,
                        ..seat_query(caret, 120)
                    },
                    ink,
                );
                held_sky = s.lift_rows > 0 && !s.over_ink;
                lifts.push(s.lift_rows);
            }
            lifts
        };
        let held = mid(true);
        let first = held.iter().position(|&l| l == 1).expect("it rises");
        assert!(
            held[first..].iter().all(|&l| l == 1),
            "mid-line, the held sky came down to the mirror: {held:?}"
        );
        assert!(
            mid(false).windows(2).any(|w| w[0] != w[1]),
            "negative control: unheld, the head goes down and up with the keys"
        );
    }

    /// A head past a line's last letter, over the row's unwritten columns,
    /// rises into the row above when the line is written up to the caret:
    /// the frame's trimmed row reads blank past its written cells
    /// ([`cell_ink`]), so the probe is not blind. Negative control: read as
    /// unknown, the columns blind the probe and the head sits behind the
    /// caret on the letters.
    #[test]
    fn a_head_past_the_last_letter_reads_the_unwritten_columns_blank() {
        use aterm_core::terminal::RenderCell;
        let cols = 120_u16;
        let caret = (7_u16, 114_u16);
        let letter = RenderCell {
            ch: 'x',
            ..RenderCell::default()
        };
        // Row 7 written up to the caret and no further; the rows above carry no cells.
        let mut cells: Vec<Vec<RenderCell>> = vec![Vec::new(); 12];
        cells[7] = vec![letter; usize::from(caret.1)];
        let query = seat_query(caret, usize::from(cols));
        let (first, _) = cell_span(query.rest.x, i32::from(query.rest.w), CW, cols);
        assert_eq!(
            first, caret.1,
            "fixture: the head's rest seat starts at the caret, past the last letter"
        );
        let seat = placement(caret, &query, |r, c| cell_ink(&cells, r, c, cols));
        assert_eq!(
            (seat.lift_rows, seat.over_ink, seat.row),
            (1, false, caret.0 - 1),
            "the head must rise clear into the row above: {seat:?}"
        );
        let blind = placement(caret, &query, |r, c| {
            cells
                .get(usize::from(r))
                .and_then(|line| line.get(usize::from(c)))
                .map(|cell| cell.ch != ' ')
        });
        let (a, b) = covered(&blind);
        assert!(
            blind.lift_rows == 0 && blind.side == Side::Behind && b < caret.1 && a < b,
            "negative control: read as unknown, the unwritten columns must blind the probe and \
             seat the head behind the caret on the letters: {blind:?}"
        );
        assert_eq!(cell_ink(&cells, 7, 113, cols), Some(true));
        assert_eq!(cell_ink(&cells, 7, 119, cols), Some(false));
        assert_eq!(cell_ink(&cells, 7, 120, cols), None, "past the grid");
        assert_eq!(cell_ink(&cells, 12, 0, cols), None, "a row the frame lacks");
    }

    fn seat_query(caret: (u16, u16), cols: usize) -> SeatQuery {
        SeatQuery {
            geom: geom_cols(cols),
            rest: footprint(caret, u16::try_from(cols).expect("fixture cols")),
            lead_px: LEAD_PX,
            held_sky: false,
        }
    }

    fn admitted(duty: CompanionDuty) -> CompanionAdmission {
        CompanionAdmission {
            duty,
            body_claimed: false,
        }
    }

    /// Blank glass everywhere.
    fn blank(_row: u16, _col: u16) -> Option<bool> {
        Some(false)
    }

    /// The inclusive cells a seat covers, from its final `x`.
    fn covered(s: &Seat) -> (u16, u16) {
        cell_span(s.x, i32::from(s.w), CW, 120)
    }

    /// L-A: the router names one body, the host duty's, and a frame whose body
    /// is already claimed draws nothing.
    #[test]
    fn a_claimed_frame_draws_no_second_body() {
        for (duty, want) in [
            (CompanionDuty::Idle, Body::None),
            (CompanionDuty::Pet, Body::Pet),
            (
                CompanionDuty::FlyingHead { cell: (2, 40) },
                Body::Flying { cell: (2, 40) },
            ),
        ] {
            assert_eq!(body_for(admitted(duty)), want, "{duty:?}");
            assert_eq!(
                body_for(CompanionAdmission {
                    duty,
                    body_claimed: true,
                }),
                Body::None,
                "{duty:?}: a claimed frame must never mint a second body"
            );
        }
    }

    /// D13: the flying head takes the meteor whole; the pet is offered the
    /// arrival edge and nothing else (no teleport, spine impulse or retiming,
    /// and no router-held cadence for the pet's own latch).
    #[test]
    fn the_pet_is_offered_the_meteor_edge_but_keeps_its_own_pounce() {
        let t0 = Instant::now();
        let t_flight = Duration::from_millis(100);
        let imp = CompanionImpulse::Meteor {
            dir: Dir::Right,
            t0,
            t_flight,
        };
        let land = t0.checked_add(t_flight).expect("100 ms fits");
        let head = Body::Flying { cell: (7, 80) };

        let BodyImpulse::Fly(flight) = impulse_for(head, imp) else {
            panic!("the flying head takes the meteor");
        };
        assert_eq!(flight.land_at, land, "the arrival edge is minted once");
        assert_eq!(flight.land, (7, 80), "the snap-to is the escorted cell");
        assert!(flight.disp_floor() >= 0.97, "§7.2: disp = max(disp, 0.97)");

        let pet = impulse_for(Body::Pet, imp);
        assert_eq!(
            pet,
            BodyImpulse::Perk { at: land },
            "the pet is offered t0 + T as its perk edge and nothing more"
        );
        assert!(
            !matches!(pet, BodyImpulse::Fly(_)),
            "a v2 meteor must never relocate the pet (owner Q14: no teleport)"
        );

        assert_eq!(impulse_for(Body::None, imp), BodyImpulse::Ignore);
        for (raw, want) in [
            (CompanionImpulse::Land, Reaction::Land),
            (CompanionImpulse::Wince, Reaction::Wince),
            (CompanionImpulse::Delight, Reaction::Delight),
        ] {
            assert_eq!(impulse_for(Body::Pet, raw), BodyImpulse::React(want));
            assert_eq!(impulse_for(head, raw), BodyImpulse::React(want));
            assert_eq!(
                impulse_for(Body::None, raw),
                BodyImpulse::Ignore,
                "a reaction with no body is not a pose"
            );
        }
    }

    /// L-D: whatever the caret column and ink, the seat never covers the caret
    /// cell or an avoidable inked cell, keeps the shipped seat on blank glass
    /// wherever it clears the caret, yields behind where the margin clamp makes
    /// it intrude, and carries one position.
    #[test]
    fn the_flying_head_never_covers_the_cell_the_caret_is_typing_into() {
        let caret_row = 7_u16;
        let inked_line = |row: u16, col: u16| -> Option<bool> {
            if row == caret_row {
                Some(col < 40)
            } else {
                Some(false)
            }
        };
        let mut intruded = 0_u32;
        for caret_col in 0..120_u16 {
            let caret = (caret_row, caret_col);
            let s = placement(caret, &seat_query(caret, 120), inked_line);
            let (first, last) = covered(&s);
            assert!(
                s.row != caret_row || !(first..=last).contains(&caret_col),
                "caret_col {caret_col}: the seat covered the caret cell \
                 (x {}, row {})",
                s.x,
                s.row
            );
            if s.row == caret_row && !s.over_ink {
                for c in first..=last {
                    assert_eq!(
                        inked_line(caret_row, c),
                        Some(false),
                        "caret_col {caret_col}: the seat covered inked cell {c}"
                    );
                }
            }
            assert!(
                ((s.col * CW as f32).round() as i32 - s.x).abs() <= 1,
                "caret_col {caret_col}: `col` disagrees with `x`"
            );
            let blank_seat = placement(caret, &seat_query(caret, 120), blank);
            let shipped = footprint(caret, 120);
            let (s_first, s_last) = cell_span(shipped.x, i32::from(shipped.w), CW, 120);
            assert_eq!(blank_seat.row, caret_row);
            assert_eq!(blank_seat.lift_rows, 0);
            assert!(!blank_seat.over_ink);
            if (s_first..=s_last).contains(&caret_col) {
                intruded += 1;
                let (first, last) = covered(&blank_seat);
                assert!(
                    !(first..=last).contains(&caret_col),
                    "caret_col {caret_col}: the margin seat covered the caret"
                );
                assert_eq!(blank_seat.side, Side::Behind);
                assert_eq!(
                    blank_seat.x,
                    i32::from(caret_col) * CW - LEAD_PX - i32::from(BODY_W),
                    "caret_col {caret_col}: the margin yields to the mirror seat"
                );
            } else {
                assert_eq!(
                    (blank_seat.x, blank_seat.y, blank_seat.w, blank_seat.h),
                    (shipped.x, shipped.y, shipped.w, shipped.h),
                    "caret_col {caret_col}: blank glass must keep the shipped seat"
                );
            }
        }
        assert!(
            intruded > 0,
            "fixture: the margin clamp must intrude somewhere for the arm to count"
        );

        let mid = placement((caret_row, 5), &seat_query((caret_row, 5), 120), inked_line);
        assert_eq!(mid.row, caret_row - 1, "the eraser case lifts one row");
        assert_eq!(mid.lift_rows, 1);
        assert!(!mid.over_ink);

        let back = placement(
            (caret_row, 38),
            &seat_query((caret_row, 38), 120),
            |r, c| Some(r == caret_row && c >= 39),
        );
        assert_eq!(back.side, Side::Behind);
        assert_eq!(back.row, caret_row);
        assert_eq!(
            back.x,
            38 * CW - LEAD_PX - i32::from(BODY_W),
            "the mirror keeps the shipped lead's clearance"
        );
        assert_eq!(
            back.y,
            footprint((caret_row, 38), 120).y,
            "the row's shipped rest"
        );
    }

    /// Seat steps 3 and 4: a fully inked row lifts the body into the sky; a
    /// fully inked sky draws over the words there; a top-row caret stays on its
    /// row without covering the caret; a grid too narrow for either side falls
    /// back to the shipped footprint.
    #[test]
    fn a_crowded_line_yields_to_the_sky_then_draws_over_the_words() {
        let all_ink = |_r: u16, _c: u16| Some(true);
        let sky_only = |r: u16, _c: u16| Some(r != 6);

        let lifted = placement((7, 5), &seat_query((7, 5), 120), sky_only);
        assert_eq!(lifted.row, 6, "a crowded line lifts one row");
        assert_eq!(lifted.lift_rows, 1);
        assert!(!lifted.over_ink, "a clear sky needs no over-exception");
        assert_eq!(
            lifted.y,
            footprint((7, 5), 120).y - CH,
            "the sky seat is the shipped rest moved up exactly one row"
        );

        let buried = placement((7, 5), &seat_query((7, 5), 120), all_ink);
        assert_eq!(
            buried.row, 6,
            "over the words in the sky, never the caret row"
        );
        assert!(buried.over_ink);

        let top = placement((0, 5), &seat_query((0, 5), 120), all_ink);
        assert_eq!(top.row, 0);
        assert_eq!(top.lift_rows, 0);
        assert!(top.over_ink);
        let (first, last) = covered(&top);
        assert!(
            !(first..=last).contains(&5),
            "row 0 must not cover the caret"
        );
        assert_eq!(top.side, Side::Ahead);

        let wall = placement((0, 117), &seat_query((0, 117), 120), all_ink);
        assert_eq!(wall.row, 0);
        assert_eq!(wall.side, Side::Behind);
        let (first, last) = cell_span(wall.x, i32::from(wall.w), CW, 120);
        assert!(
            !(first..=last).contains(&117),
            "row 0 at the wall must not cover the caret"
        );

        let narrow = placement((0, 4), &seat_query((0, 4), 10), all_ink);
        let shipped = footprint((0, 4), 10);
        assert_eq!((narrow.x, narrow.y), (shipped.x, shipped.y));
        assert!(narrow.over_ink);
    }

    /// A blind host (every probe `None`) keeps the shipped seat wherever it clears the caret and
    /// never gets the sky or an over-ink verdict; at the right margin it takes the mirror seat,
    /// and where neither side clears, the shipped footprint.
    #[test]
    fn an_ink_blind_host_keeps_the_shipped_seat_but_never_the_caret_cell() {
        let blind_probe = |_r: u16, _c: u16| -> Option<bool> { None };
        for caret in [(7_u16, 5_u16), (7, 60), (0, 5)] {
            let blind = placement(caret, &seat_query(caret, 120), blind_probe);
            let shipped = footprint(caret, 120);
            assert_eq!(
                (blind.x, blind.y, blind.row, blind.lift_rows, blind.over_ink),
                (shipped.x, shipped.y, caret.0, 0, false),
                "{caret:?}: a blind host must keep the shipped seat"
            );
        }

        let caret = (7_u16, 117_u16);
        let shipped = footprint(caret, 120);
        let (s_first, s_last) = cell_span(shipped.x, i32::from(shipped.w), CW, 120);
        assert!(
            (s_first..=s_last).contains(&caret.1),
            "fixture: the shipped seat must sit on the caret cell at the wall"
        );
        let wall = placement(caret, &seat_query(caret, 120), blind_probe);
        let (first, last) = covered(&wall);
        assert!(
            !(first..=last).contains(&caret.1),
            "a blind host must still keep off the caret cell (x {}, cells {first}..={last})",
            wall.x
        );
        assert_eq!(
            wall.x,
            i32::from(caret.1) * CW - LEAD_PX - i32::from(BODY_W),
            "…by taking the mirror seat, the shipped lead's clearance back"
        );
        assert_eq!(
            (wall.y, wall.row, wall.lift_rows, wall.side, wall.over_ink),
            (shipped.y, caret.0, 0, Side::Behind, false),
            "…on the caret's own row: no sky and no over-ink verdict from a \
             probe that answered nothing"
        );
        let sighted = placement(caret, &seat_query(caret, 120), blank);
        assert_eq!(
            (wall.x, wall.y, wall.row, wall.side),
            (sighted.x, sighted.y, sighted.row, sighted.side),
            "blind and sighted must agree wherever only the caret cell decides"
        );

        let narrow = placement((0, 4), &seat_query((0, 4), 10), blind_probe);
        let shipped = footprint((0, 4), 10);
        assert_eq!(
            (
                narrow.x,
                narrow.y,
                narrow.row,
                narrow.lift_rows,
                narrow.over_ink
            ),
            (shipped.x, shipped.y, 0, 0, false),
            "too narrow for either side: today's footprint, no over-ink verdict"
        );

        let mixed = placement((7, 5), &seat_query((7, 5), 120), |r, c| {
            (r != 7 || c != 8).then_some(false)
        });
        assert_ne!(mixed.row, 7, "a single unknowable cell in the span yields");
    }

    /// THE KEEP-OUT (looks6 N3, N4): a mark whose reach touches a word
    /// icon's or another row's lettered box is kept off it, and one beside
    /// it is not; a body standing on the row under a lettered box reaches
    /// no higher than the row's top over it, easing back to its own reach
    /// beside it. Negative control: an empty keep-out keeps nothing.
    #[test]
    fn the_keep_out_holds_marks_and_the_body_s_top_off_its_boxes() {
        let mut k = KeepOut::default();
        // A `fox` face over cells 4..6 of row 2 (16-px rows), and an empty box.
        k.set([(32, 56, 32, 48), (10, 10, 0, 5)]);
        assert_eq!(k.boxes().len(), 1, "an empty box is no box");
        assert!(k.covers((44, 40), 0), "a star on the face is kept off");
        assert!(
            k.covers((30, 40), 3),
            "a star whose arm reaches the face is kept off"
        );
        assert!(!k.covers((20, 40), 3), "a star clear of the face is drawn");
        let (row_top, free, ease) = (48.0, 40.0, 6.0);
        assert_eq!(
            k.ceiling(44.0, row_top, free, ease),
            row_top,
            "no feather under the box"
        );
        let beside = k.ceiling(59.0, row_top, free, ease);
        assert!(
            free < beside && beside < row_top,
            "the ceiling eases beside it ({beside})"
        );
        assert_eq!(
            k.ceiling(80.0, row_top, free, ease),
            free,
            "far off, the body's own reach"
        );
        assert_eq!(
            k.ceiling(44.0, 64.0, free, ease),
            free,
            "a row the box does not stand on"
        );
        assert_eq!(
            k.below((40, 48), (40, 56)),
            (48, 56),
            "a column under the box keeps only its span below it"
        );
        assert_eq!(
            k.below((40, 48), (40, 47)),
            (47, 47),
            "a column wholly in the box keeps nothing"
        );
        assert_eq!(
            k.below((60, 70), (40, 56)),
            (40, 56),
            "a column beside it keeps all"
        );
        let none = KeepOut::default();
        assert!(!none.covers((44, 40), 0), "control: nothing kept");
        assert_eq!(
            none.below((40, 48), (40, 56)),
            (40, 56),
            "control: nothing cut"
        );
        assert_eq!(
            none.ceiling(44.0, row_top, free, ease),
            free,
            "control: no ceiling"
        );
    }
}
