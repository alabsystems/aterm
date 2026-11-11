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

/// The 1-px look-back toward the launch lasts 250 ms after arrival (§7.2, row "Eyes").
pub const FLYING_LOOKBACK_MS: f32 = 250.0;

/// The squint holds 200 ms past the arrival edge (§7.2, row "Eyes").
pub const FLYING_SQUINT_TAIL_MS: f32 = 200.0;

const FLYING_LOOKBACK_DUR: std::time::Duration = std::time::Duration::from_nanos(
    ((FLYING_LOOKBACK_MS * (1.0 / 1000.0)) as f64 * 1_000_000_000.0).round() as u64,
);
const FLYING_SQUINT_TAIL_DUR: std::time::Duration = std::time::Duration::from_nanos(
    ((FLYING_SQUINT_TAIL_MS * (1.0 / 1000.0)) as f64 * 1_000_000_000.0).round() as u64,
);

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
        match self.land_at.checked_add(FLYING_SQUINT_TAIL_DUR) {
            Some(t) => t,
            None => self.land_at,
        }
    }

    /// The 1-px look-back toward the launch ends here ([`FLYING_LOOKBACK_MS`] after arrival).
    #[inline]
    #[must_use]
    pub fn settled_at(&self) -> Instant {
        match self.land_at.checked_add(FLYING_LOOKBACK_DUR) {
            Some(t) => t,
            None => self.land_at,
        }
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
    match t0.checked_add(t_flight) {
        Some(t) => t,
        None => t0,
    }
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
    /// Pixels the body was lowered within its row (negative: raised) so its drawn box keeps off the lettered cells of the rows it reaches into ([`keep_off_rows_beside`]), already in [`Self::y`]; [`Self::lift`] carries it to a host's layout.
    pub drop_px: i32,
}

impl Seat {
    /// The rows a host's layout lifts the body by (`KittyCursorLayout::lift`) for cells `cell_h` px tall: the sky seat's whole rows less the drop that keeps the head off the rows beside its own.
    #[inline]
    #[must_use]
    pub const fn lift(&self, cell_h: u16) -> f32 {
        let drop = if cell_h == 0 {
            0.0
        } else {
            self.drop_px as f32 / (cell_h as f32)
        };
        (self.lift_rows as f32) - drop
    }
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

/// THE HEAD FITS ITS ROW WHERE NO SEAT HOLDS IT WHOLE. The head is taller than its row, so with
/// letters over and under it (the screen's last row under a full row, the row under it off the
/// grid) neither [`placement`] nor the placement's letter law can clear it, and it stayed over the
/// row above's letters, down to 1.00:1 (the trail gallery's `last-row`). Fitted to its row, as
/// [`crate::kitty_pet::head_law`] fits the walking companions, it stands beside the caret in the
/// blank cells ahead of it, and is smaller still where the margin leaves it less room, no smaller
/// than [`HEAD_FIT_FLOOR`], where its face still reads. Eased, at the pet's rate, and held until
/// its whole seat clears with air to spare, so it never pops or blinks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadFit {
    from: f32,
    to: f32,
    since: Option<Instant>,
}

/// How fast (its size's share a second) the head is fitted to its row and back: the pet's
/// (`kitty_pet::FIT_RATE`).
pub const HEAD_FIT_RATE: f32 = 4.5;
/// The smallest the head is fitted to: 28 px wide on the gallery's 26 px row, a face at 1x.
pub const HEAD_FIT_FLOOR: f32 = 0.40;
/// The share of its row's height a head fitted to it fills (`kitty_pet::ROW_FIT`'s).
const HEAD_ROW_SHARE: f32 = 0.92;

impl HeadFit {
    /// The head drawn whole.
    pub const WHOLE: Self = Self {
        from: 1.0,
        to: 1.0,
        since: None,
    };

    /// The share of its size the head is drawn at, at `now`.
    #[must_use]
    #[inline]
    pub fn at(&self, now: Instant) -> f32 {
        let Some(since) = self.since else {
            return self.to;
        };
        let way = self.to - self.from;
        let step = now.saturating_duration_since(since).as_secs_f32() * HEAD_FIT_RATE;
        if step >= way.abs() {
            self.to
        } else {
            self.from + way.signum() * step
        }
    }

    /// Whether the head is fitted, or on its way to it.
    #[must_use]
    #[inline]
    pub fn fitted(&self) -> bool {
        self.to < 1.0
    }

    /// Ease to `to` from where it stands at `now`; at once where the host does not animate.
    #[inline]
    pub fn aim(&mut self, to: f32, now: Instant, animate: bool) {
        if !animate {
            *self = Self {
                from: to,
                to,
                since: None,
            };
        } else if to != self.to {
            self.from = self.at(now);
            self.to = to;
            self.since = Some(now);
        }
    }
}

/// The size [`HeadFit`] takes the head to for the caret at `caret` and a whole head `rest`, and the
/// box `(x, y, w, h)` it stands in at that size: on the caret's row, its middle on the row's, as
/// near the caret as the head's ring of air allows and no further than the grid's end, which at
/// the last columns lays it across the caret and the letter behind it: fewer letters than the whole
/// head lies on there, over two rows. `None` where the cells ahead of the caret hold letters (a line
/// edited in the middle), which fitting would not clear.
#[must_use]
pub fn head_fit_seat<F>(
    caret: (u16, u16),
    rest: CatFootprint,
    (cw, ch, cols): (i32, i32, u16),
    gap: i32,
    ink: &F,
) -> Option<(f32, (i32, i32, i32, i32))>
where
    F: Fn(u16, u16) -> Option<bool>,
{
    #[cfg(any(test, debug_assertions))]
    if HEAD_FIT_UNREAD.with(std::cell::Cell::get) {
        return None;
    }
    if rest.w == 0 || rest.h == 0 || cw <= 0 || ch <= 0 {
        return None;
    }
    let grid_w = i32::from(cols).saturating_mul(cw);
    let caret_right = (i32::from(caret.1) + 1).saturating_mul(cw);
    let (rest_w, rest_h) = (f32::from(rest.w), f32::from(rest.h));
    let by_row = HEAD_ROW_SHARE * ch as f32 / rest_h;
    let by_room = (grid_w - caret_right - gap) as f32 / rest_w;
    let goal = by_row.min(by_room).clamp(HEAD_FIT_FLOOR, 1.0);
    let (w, h) = (
        (rest_w * goal).round() as i32,
        (rest_h * goal).round() as i32,
    );
    let x = (caret_right + gap).min(grid_w - w).max(0);
    let y = i32::from(caret.0) * ch + (ch - h) / 2;
    let (_, last) = cell_span(x, w, cw, cols);
    let lettered = (caret.1.saturating_add(1)..=last).any(|c| ink(caret.0, c) == Some(true));
    (!lettered).then_some((goal, (x, y, w, h)))
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Tests' negative control over [`head_fit_seat`]: on, the head is never fitted to its row.
    static HEAD_FIT_UNREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests' negative control over [`head_fit_seat`] on this thread: `true` keeps the head whole.
#[cfg(any(test, debug_assertions))]
pub fn set_head_fit_unread(unread: bool) {
    HEAD_FIT_UNREAD.with(|cell| cell.set(unread));
}

/// Whether a box `(x, y, w, h)` lies clear of the caret's cell and every lettered cell, `air` px
/// to spare above and below it. A cell the host cannot read is no letter.
#[must_use]
pub fn box_clear<F>(
    (x, y, w, h): (i32, i32, i32, i32),
    caret: (u16, u16),
    (cw, ch, rows, cols): (i32, i32, u16, u16),
    air: i32,
    ink: &F,
) -> bool
where
    F: Fn(u16, u16) -> Option<bool>,
{
    if cw <= 0 || ch <= 0 || w <= 0 || h <= 0 {
        return true;
    }
    let (first, last) = cell_span(x, w, cw, cols);
    let (top, bottom) = (y - air, y + h + air - 1);
    let (r0, r1) = (top.max(0) / ch, bottom.max(0) / ch);
    let mut r = r0;
    while r <= r1 && r < i32::from(rows) {
        let mut c = first;
        while c <= last {
            if (r as u16 == caret.0 && c == caret.1) || ink(r as u16, c) == Some(true) {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

/// The seat rule (§6.5 layer 13): the row and the side ([`seat_on_a_row`]), then the
/// body's height within its row ([`keep_off_rows_beside`]).
#[must_use]
#[inline]
pub fn placement<F>(caret: (u16, u16), query: &SeatQuery, ink: F) -> Seat
where
    F: Fn(u16, u16) -> Option<bool>,
{
    let seat = seat_on_a_row(caret, query, &ink);
    #[cfg(any(test, debug_assertions))]
    if ROWS_BESIDE_UNREAD.with(std::cell::Cell::get) {
        return seat;
    }
    keep_off_rows_beside(seat, caret, query, &ink)
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Tests' negative control over [`keep_off_rows_beside`]: on, the head keeps the seat's
    /// height whatever letters the rows beside its own hold.
    static ROWS_BESIDE_UNREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests' negative control over [`keep_off_rows_beside`] on this thread: `true` seats the head
/// at its row's height whatever letters lie beside it, as before the law.
#[cfg(any(test, debug_assertions))]
pub fn set_rows_beside_unread(unread: bool) {
    ROWS_BESIDE_UNREAD.with(|cell| cell.set(unread));
}

/// THE HEAD KEEPS OFF THE LETTERS OF THE ROWS IT REACHES INTO. The head is taller than its
/// row (`WordDecorations::COMPANION_CELL_H`) and centred on it, so its ears reach into the row
/// above and its chin into the row below, whose letters [`seat_on_a_row`] never reads: seated
/// ahead of the caret under a written line, its ears lay on that line's letters (the settings
/// card's `git c` at 1.25:1, looks9's settings round 2). Where its drawn box meets a lettered
/// cell beside its row, the body moves within its row's band, its top to the row's top or its
/// bottom to the row's bottom, whichever is nearer and leaves every cell it reaches beside its
/// own row on the grid and blank; where neither does (letters above and below, the grid's
/// edge, cells the host cannot read), or the seat already lies on ink or intrudes on the
/// caret's cell (the margin's rise owns its height), it holds.
fn keep_off_rows_beside<F>(mut seat: Seat, caret: (u16, u16), query: &SeatQuery, ink: &F) -> Seat
where
    F: Fn(u16, u16) -> Option<bool>,
{
    let geom = query.geom;
    let ch = geom.ch.clamp(1, i32::MAX as usize) as i32;
    let cw = geom.cw.clamp(1, i32::MAX as usize) as i32;
    let cols = geom.cols.min(u16::MAX as usize) as u16;
    let rows = geom.rows.min(u16::MAX as usize) as i32;
    let (w, h) = (i32::from(seat.w), i32::from(seat.h));
    if seat.over_ink || cols == 0 || rows == 0 || w <= 0 || h <= 0 {
        return seat;
    }
    let row = i32::from(seat.row);
    let (first, last) = cell_span(seat.x, w, cw, cols);
    if seat.row == caret.0 && (first..=last).contains(&caret.1) {
        return seat;
    }
    // The rows other than its own a box with its top at `y` reaches.
    let beside = |y: i32| {
        let (top, bottom) = (y.div_euclid(ch), (y + h - 1).div_euclid(ch));
        (top..=bottom).filter(move |&r| r != row)
    };
    let lettered =
        |r: i32| (0..rows).contains(&r) && (first..=last).any(|c| ink(r as u16, c) == Some(true));
    if !beside(seat.y).any(lettered) {
        return seat;
    }
    let blank =
        |r: i32| (0..rows).contains(&r) && (first..=last).all(|c| ink(r as u16, c) == Some(false));
    let (top_at_top, bottom_at_bottom) = (row * ch, (row + 1) * ch - h);
    let moved = if bottom_at_bottom >= top_at_top {
        let y = seat.y.clamp(top_at_top, bottom_at_bottom);
        (y >= 0 && beside(y).all(blank)).then_some(y)
    } else {
        let (first_y, second_y) =
            if (top_at_top - seat.y).abs() <= (bottom_at_bottom - seat.y).abs() {
                (top_at_top, bottom_at_bottom)
            } else {
                (bottom_at_bottom, top_at_top)
            };
        if first_y >= 0 && beside(first_y).all(blank) {
            Some(first_y)
        } else if second_y >= 0 && beside(second_y).all(blank) {
            Some(second_y)
        } else {
            None
        }
    };
    if let Some(y) = moved {
        seat.drop_px += y - seat.y;
        seat.y = y;
    }
    seat
}

/// The seat's row and side ([`placement`]'s first step), the body at the height its row's
/// footprint gives it.
#[inline]
fn seat_on_a_row<F>(caret: (u16, u16), query: &SeatQuery, ink: &F) -> Seat
where
    F: Fn(u16, u16) -> Option<bool>,
{
    let geom = query.geom;
    let rest = query.rest;
    let (caret_row, caret_col) = caret;
    let cw = geom.cw.clamp(1, i32::MAX as usize) as i32;
    let ch = geom.ch.clamp(1, i32::MAX as usize) as i32;
    let cols = geom.cols.min(u16::MAX as usize) as u16;
    let w = i32::from(rest.w);
    let caret_left = i32::from(caret_col).saturating_mul(cw);
    let grid_w = i32::from(cols).saturating_mul(cw);

    let ahead = census(rest.x, w, caret_row, caret, cols, cw, ink);
    if ahead.clear() {
        return seat(query, rest.x, rest.y, caret_row, caret_left, 0, false);
    }

    let mut sky_census = None;
    let mut written_before = false;
    if caret_row > 0 && ahead.probe != Probe::Blind && !query.held_sky && ahead.covers_caret {
        let mut c = caret_col.saturating_sub(WRITTEN_REACH);
        while c < caret_col {
            if ink(caret_row, c) == Some(true) {
                written_before = true;
                break;
            }
            c += 1;
        }
    }
    if caret_row > 0 && ahead.probe != Probe::Blind && (query.held_sky || written_before) {
        let sky_row = caret_row - 1;
        let sky = census(rest.x, w, sky_row, caret, cols, cw, ink);
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
            caret_col < b_first || caret_col > b_last
        };
        let x = if shift_behind { behind_x } else { rest.x };
        return seat(query, x, rest.y, caret_row, caret_left, 0, false);
    }

    let behind = census(behind_x, w, caret_row, caret, cols, cw, ink);
    if behind.clear() {
        return seat(query, behind_x, rest.y, caret_row, caret_left, 0, false);
    }

    if caret_row > 0 {
        let sky_row = caret_row - 1;
        let sky_y = rest.y.saturating_sub(ch);
        let sky = match sky_census {
            Some(s) => s,
            None => census(rest.x, w, sky_row, caret, cols, cw, ink),
        };
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
    let covers_caret = row == caret.0 && caret.1 >= first && caret.1 <= last;
    let mut inked = false;
    let mut blank = 0_u32;
    let mut unknown = 0_u32;
    let mut c = first;
    while c <= last {
        if (row, c) == caret {
            c += 1;
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
        c += 1;
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
        drop_px: 0,
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
    pub const fn of(frame: &PetFrame, geom: Geom) -> Option<Self> {
        match Self::of_with_body_px(frame, geom) {
            Some((on_glass, _)) => Some(on_glass),
            None => None,
        }
    }

    /// Read [`PetOnGlass`] and the body's window-pixel box `(x0, x1, y0, y1)` off the pet's published frame in a single pass.
    #[inline]
    #[must_use]
    pub const fn of_with_body_px(
        frame: &PetFrame,
        geom: Geom,
    ) -> Option<(Self, (i32, i32, i32, i32))> {
        if geom.cw > u16::MAX as usize
            || geom.ch > u16::MAX as usize
            || geom.cols > u16::MAX as usize
            || geom.rows > u16::MAX as usize
        {
            return None;
        }
        let cell_w = geom.cw as u16;
        let cell_h = geom.ch as u16;
        let cols = geom.cols as u16;
        let rows = geom.rows as u16;
        let Some((x0, x1, y0, y1)) = frame.body_px(cell_w, cell_h, cols, rows) else {
            return None;
        };
        let cw = cell_w as f32;
        if cw <= 0.0 {
            return None;
        }
        let inv_cw = 1.0 / cw;
        let (ox, oy) = (geom.origin_x as i32, geom.origin_y as i32);
        let row_u32 = frame.row.round().max(0.0) as u32;
        let row = if row_u32 <= u16::MAX as u32 {
            row_u32 as u16
        } else {
            u16::MAX
        };
        Some((
            Self {
                col: x0 as f32 * inv_cw,
                width: (x1 - x0) as f32 * inv_cw,
                row,
                settled: frame.action.settled(),
                purr: frame.purr,
                contented: frame.contented,
            },
            (x0 + ox, x1 + ox, y0 + oy, y1 + oy),
        ))
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

/// The pet's body as the sky draws around it: a star whose arm at its peak
/// reaches the body is not drawn, so the cat flies in front of the sky. The pet
/// ticks after the sky emits, so the shade is the body's last box joined with
/// that box carried on by its last frame's move (the box it is about to be
/// drawn at), each shaved of the tile's transparent border at the sides. Keyed
/// on the star's centre alone, a star beside the head laid its arms across
/// the paper cat's eye (looks8 K8); keyed on its peak reach, as a word icon
/// keeps the sky off ([`KeepOut::covers`]), the answer holds for the star's
/// life while it stands still, and no star blinks on a twinkling arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetShade {
    /// The last two drawn boxes, `(x0, x1, y0, y1)` in window pixels, newest first.
    boxes: [Option<(i32, i32, i32, i32)>; 2],
}

impl Default for PetShade {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl PetShade {
    /// Empty pet shade with no recorded boxes.
    pub const EMPTY: Self = Self { boxes: [None; 2] };

    /// Note the body's drawn box this frame (`None`: off glass), window pixels.
    #[inline]
    pub const fn note(&mut self, body: Option<(i32, i32, i32, i32)>) {
        self.boxes = [body, if body.is_some() { self.boxes[0] } else { None }];
    }

    /// The body box noted last (`None`: off glass, or never noted), window pixels.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn body(&self) -> Option<(i32, i32, i32, i32)> {
        self.boxes[0]
    }

    /// Whether a star centred at `(x, y)` (window pixels) whose arm reaches
    /// `reach` px round its centre at its peak touches the body.
    #[inline]
    #[must_use]
    pub const fn hides(&self, (x, y): (i32, i32), reach: i32, cell_w: usize) -> bool {
        #[inline]
        const fn meets(
            x: i32,
            y: i32,
            r: i32,
            border: i32,
            (x0, x1, y0, y1): (i32, i32, i32, i32),
        ) -> bool {
            y + r >= y0 && y - r < y1 && x + r >= x0 + border && x - r < x1 - border
        }
        let Some(last) = self.boxes[0] else {
            return false;
        };
        let r = if reach > 0 { reach } else { 0 };
        let border = (SHADE_BORDER_CELLS * cell_w as f32).round() as i32;
        if meets(x, y, r, border, last) {
            return true;
        }
        let Some(prev) = self.boxes[1] else {
            return false;
        };
        let (dx, dy) = (last.0 - prev.0, last.2 - prev.2);
        if dx == 0 && dy == 0 {
            return false;
        }
        meets(
            x,
            y,
            r,
            border,
            (last.0 + dx, last.1 + dx, last.2 + dy, last.3 + dy),
        )
    }
}

/// What the sky and the ribbon's top keep off: the frame's word icons (a
/// `dog`'s or a `fox`'s face drawn over its word) and every lettered or
/// filled cell (a powerline segment, a selection) of a row the caret is not
/// on, as window-pixel boxes `(x0, x1, y0, y1)`, right and bottom exclusive.
/// No star is drawn on them and the ribbon's body never feathers up into
/// them.
#[derive(Debug, PartialEq, Eq)]
pub struct KeepOut {
    /// Every box: the word icons' and the lettered or filled cells'.
    boxes: Vec<(i32, i32, i32, i32)>,
    /// The lettered or filled cells' boxes alone ([`Self::below`]).
    cells: Vec<(i32, i32, i32, i32)>,
    /// Each lettered row's span, first lettered or filled cell to last, a
    /// row tall ([`Self::behind_text`]).
    rows: Vec<(i32, i32, i32, i32)>,
    /// How far past its last letter a row's span reaches for a light kept
    /// clear of the letter guard's reach, px ([`Self::past`]).
    past: i32,
    /// The filled cells' runs alone, lettered or not: a ground that does
    /// not read as the page (a selection, a powerline segment;
    /// [`Self::fills`]).
    filled: Vec<(i32, i32, i32, i32)>,
    /// The cells of the rows whose glyphs the host named whose ink may
    /// reach under the baseline: a descender, a wide or non-ASCII glyph, a
    /// letter the probe did not name ([`Self::foot_share`]).
    feet: Vec<(i32, i32, i32, i32)>,
    /// Those rows' boxes: a row among them has its feet known
    /// ([`Self::knows_feet`]).
    soles: Vec<(i32, i32, i32, i32)>,
    /// The lettered cells' runs alone (a glyph, on any ground), when the
    /// host named them ([`Self::letters_meet`]); a run of glyphs that keep
    /// under the x-height begins where their ink may, a third of the cell
    /// down, its cell's top blank leading.
    letters: Option<Vec<(i32, i32, i32, i32)>>,
}

impl Default for KeepOut {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Clone for KeepOut {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            boxes: self.boxes.clone(),
            cells: self.cells.clone(),
            rows: self.rows.clone(),
            past: self.past,
            filled: self.filled.clone(),
            feet: self.feet.clone(),
            soles: self.soles.clone(),
            letters: self.letters.clone(),
        }
    }

    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.boxes.clone_from(&source.boxes);
        self.cells.clone_from(&source.cells);
        self.rows.clone_from(&source.rows);
        self.past = source.past;
        self.filled.clone_from(&source.filled);
        self.feet.clone_from(&source.feet);
        self.soles.clone_from(&source.soles);
        self.letters.clone_from(&source.letters);
    }
}

/// Push each box of `from` with a positive width and height onto `to`.
#[inline]
fn push_real(to: &mut Vec<(i32, i32, i32, i32)>, from: &[(i32, i32, i32, i32)]) {
    let mut i = 0;
    while i < from.len() {
        let b = from[i];
        if b.0 < b.1 && b.2 < b.3 {
            to.push(b);
        }
        i += 1;
    }
}

impl KeepOut {
    /// Empty keep-out set.
    pub const EMPTY: Self = Self {
        boxes: Vec::new(),
        cells: Vec::new(),
        rows: Vec::new(),
        past: 0,
        filled: Vec::new(),
        feet: Vec::new(),
        soles: Vec::new(),
        letters: None,
    };
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
        self.cells.clear();
        for b in cells {
            if b.0 < b.1 && b.2 < b.3 {
                self.cells.push(b);
            }
        }
        self.boxes.clear();
        for b in icons {
            if b.0 < b.1 && b.2 < b.3 {
                self.boxes.push(b);
            }
        }
        self.boxes.extend_from_slice(&self.cells);
    }

    /// This frame's filled cells' runs, window pixels ([`Self::fills`]).
    #[inline]
    pub fn set_filled(&mut self, filled: impl IntoIterator<Item = (i32, i32, i32, i32)>) {
        self.filled.clear();
        for b in filled {
            if b.0 < b.1 && b.2 < b.3 {
                self.filled.push(b);
            }
        }
    }

    /// This frame's feet and the rows they were read on, window pixels
    /// ([`Self::feet`], [`Self::soles`]).
    #[inline]
    pub fn set_feet(
        &mut self,
        feet: impl IntoIterator<Item = (i32, i32, i32, i32)>,
        soles: impl IntoIterator<Item = (i32, i32, i32, i32)>,
    ) {
        self.feet.clear();
        for b in feet {
            if b.0 < b.1 && b.2 < b.3 {
                self.feet.push(b);
            }
        }
        self.soles.clear();
        for b in soles {
            if b.0 < b.1 && b.2 < b.3 {
                self.soles.push(b);
            }
        }
    }

    /// Whether the glyphs of the row spanning `y0..y1` were named, so its
    /// feet are known ([`Self::foot_share`]).
    #[inline]
    #[must_use]
    pub const fn knows_feet(&self, (y0, y1): (i32, i32)) -> bool {
        let soles = self.soles.as_slice();
        let mut i = 0;
        while i < soles.len() {
            let b = soles[i];
            i += 1;
            if b.2 <= y0 && y1 <= b.3 {
                return true;
            }
        }
        false
    }

    /// The share of its light a stroke at window `(x, y)` keeps beside the
    /// feet of its row: none within `clear` px of a foot's cell, easing in
    /// to whole over the next `ease` px.
    #[inline]
    #[must_use]
    pub const fn foot_share(&self, (x, y): (f32, f32), clear: f32, ease: f32) -> f32 {
        let feet = self.feet.as_slice();
        if feet.is_empty() {
            return 1.0;
        }
        let ease = ease.max(1.0);
        let mut min_d = ease;
        let mut i = 0;
        while i < feet.len() {
            let (x0, x1, y0, y1) = feet[i];
            i += 1;
            if y < y0 as f32 || y >= y1 as f32 {
                continue;
            }
            let d = (x0 as f32 - x).max(x - x1 as f32) - clear;
            if d <= 0.0 {
                return 0.0;
            }
            if d < min_d {
                min_d = d;
            }
        }
        if min_d >= ease {
            return 1.0;
        }
        let k = (min_d * (1.0 / ease)).min(1.0);
        k * k * (3.0 - 2.0 * k)
    }

    /// Whether a filled cell's box meets the span `(x0, x1) × (y0, y1)`.
    #[inline]
    #[must_use]
    pub const fn fills(&self, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> bool {
        let filled = self.filled.as_slice();
        let mut i = 0;
        while i < filled.len() {
            let b = filled[i];
            i += 1;
            if b.0 < x1 && x0 < b.1 && b.2 < y1 && y0 < b.3 {
                return true;
            }
        }
        false
    }

    #[inline]
    pub(crate) fn set_split_slices(
        &mut self,
        icons: &[(i32, i32, i32, i32)],
        (ox, oy): (i32, i32),
        cells: &[(i32, i32, i32, i32)],
    ) {
        self.cells.clear();
        push_real(&mut self.cells, cells);
        self.boxes.clear();
        let mut i = 0;
        while i < icons.len() {
            let (x0, x1, y0, y1) = icons[i];
            let b = (x0 + ox, x1 + ox, y0 + oy, y1 + oy);
            if b.0 < b.1 && b.2 < b.3 {
                self.boxes.push(b);
            }
            i += 1;
        }
        self.boxes.extend_from_slice(&self.cells);
    }

    /// [`Self::set_filled`] from a slice.
    #[inline]
    pub(crate) fn set_filled_slice(&mut self, filled: &[(i32, i32, i32, i32)]) {
        self.filled.clear();
        push_real(&mut self.filled, filled);
    }

    /// This frame's lettered cells' runs, window pixels
    /// ([`Self::letters_meet`]).
    #[inline]
    pub(crate) fn set_letters_slice(&mut self, letters: &[(i32, i32, i32, i32)]) {
        let to = self.letters.get_or_insert_with(Vec::new);
        to.clear();
        push_real(to, letters);
    }

    /// Whether a lettered cell's box meets the span `(x0, x1) × (y0, y1)`;
    /// `None` when the host named no letters.
    #[inline]
    #[must_use]
    pub const fn letters_meet(&self, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> Option<bool> {
        let Some(letters) = self.letters.as_ref() else {
            return None;
        };
        let slice = letters.as_slice();
        let mut i = 0;
        while i < slice.len() {
            let b = slice[i];
            if b.0 < x1 && x0 < b.1 && b.2 < y1 && y0 < b.3 {
                return Some(true);
            }
            i += 1;
        }
        Some(false)
    }

    /// [`Self::set_feet`] from slices.
    #[inline]
    pub(crate) fn set_feet_slices(
        &mut self,
        feet: &[(i32, i32, i32, i32)],
        soles: &[(i32, i32, i32, i32)],
    ) {
        self.feet.clear();
        push_real(&mut self.feet, feet);
        self.soles.clear();
        push_real(&mut self.soles, soles);
    }

    /// This frame's lettered rows' spans, window pixels: each row's first
    /// lettered or filled cell to its last, the row's whole height.
    #[inline]
    pub fn set_rows(&mut self, rows: impl IntoIterator<Item = (i32, i32, i32, i32)>) {
        self.set_rows_past(rows, 0);
    }

    /// [`Self::set_rows`], each span reaching `past` px beyond its last
    /// letter for a light kept clear of the letter guard's reach
    /// ([`Self::past`]): a cell, where the row's caret stood when it was
    /// typed.
    #[inline]
    pub fn set_rows_past(
        &mut self,
        rows: impl IntoIterator<Item = (i32, i32, i32, i32)>,
        past: i32,
    ) {
        self.rows.clear();
        for b in rows {
            if b.0 < b.1 && b.2 < b.3 {
                self.rows.push(b);
            }
        }
        self.past = past.max(0);
    }

    #[inline]
    pub(crate) fn set_rows_past_slice(&mut self, rows: &[(i32, i32, i32, i32)], past: i32) {
        self.rows.clear();
        push_real(&mut self.rows, rows);
        self.past = past.max(0);
    }

    /// The lettered rows' spans ([`Self::set_rows`]), each to its last letter.
    #[inline]
    #[must_use]
    pub const fn rows(&self) -> &[(i32, i32, i32, i32)] {
        self.rows.as_slice()
    }

    /// How far past its last letter a row's span reaches for a dark train,
    /// which keeps clear of the letter guard's reach, px: the cell after the
    /// last letter, where its caret stood, so a Return's root there is held
    /// behind the line (looks6 round 3's brown stub). A paper train passes
    /// behind the letters' bodies alone: held behind that cell too, the
    /// printed line's blank cell after `fox` cut the paper Return's stick in
    /// two (looks7 M2).
    #[inline]
    #[must_use]
    pub const fn past(&self) -> i32 {
        self.past
    }

    /// How much of a light at `(x, y)` shows in front of the letters: 0
    /// inside a lettered row's span between `band`'s shares of its height
    /// (its letters' bodies, the cap line to the baseline), reaching `past`
    /// px beyond its last letter and widened by `pad` px on every side, 1
    /// clear of it, eased over `feather` px
    /// inside its edges. A thread laid across a word passes behind its
    /// letters instead of striking them through, and with no `pad` shows
    /// again below their baseline.
    #[inline]
    #[must_use]
    pub const fn behind_text(
        &self,
        (x, y): (f32, f32),
        band: (f32, f32),
        (pad, past): (f32, f32),
        feather: f32,
    ) -> f32 {
        let rows = self.rows.as_slice();
        if rows.is_empty() {
            return 1.0;
        }
        let feather = feather.max(1.0);
        let past_pad = past + pad;
        let inv_feather = 1.0 / feather;
        let mut max_d = 0.0f32;
        let mut i = 0;
        while i < rows.len() {
            let (x0, x1, y0, y1) = rows[i];
            i += 1;
            let ch = (y1 - y0) as f32;
            let b0 = y0 as f32 + band.0 * ch - pad;
            if y <= b0 {
                continue;
            }
            let b1 = y0 as f32 + band.1 * ch + pad;
            if y >= b1 {
                continue;
            }
            let (x0, x1) = (x0 as f32 - pad, x1 as f32 + past_pad);
            let dx = (x - x0).min(x1 - x);
            if dx <= 0.0 {
                continue;
            }
            let d = dx.min(y - b0).min(b1 - y);
            if d >= feather {
                return 0.0;
            }
            if d > max_d {
                max_d = d;
            }
        }
        if max_d <= 0.0 {
            return 1.0;
        }
        let u = (max_d * inv_feather).min(1.0);
        1.0 - u * u * (3.0 - 2.0 * u)
    }

    /// The boxes, window pixels.
    #[inline]
    #[must_use]
    pub const fn boxes(&self) -> &[(i32, i32, i32, i32)] {
        self.boxes.as_slice()
    }

    /// Whether a mark centred at `(x, y)` that reaches `reach` px round its
    /// centre touches a box.
    #[inline]
    #[must_use]
    pub const fn covers(&self, (x, y): (i32, i32), reach: i32) -> bool {
        let r = if reach > 0 { reach } else { 0 };
        let (x_lo, x_hi, y_lo, y_hi) = (x - r, x + r, y - r, y + r);
        let boxes = self.boxes.as_slice();
        let mut i = 0;
        while i < boxes.len() {
            let (x0, x1, y0, y1) = boxes[i];
            if y_hi >= y0 && y_lo < y1 && x_hi >= x0 && x_lo < x1 {
                return true;
            }
            i += 1;
        }
        false
    }

    /// What is left of a column's span `(y0, y1)` across `(x0, x1)` once
    /// every lettered or filled cell's box it meets is cut off its top: the
    /// span below the lowest box bottom it meets (empty when a box reaches
    /// its foot). Word icons are left out: the frame's shade cuts a light by
    /// the face's drawn rows (`CursorGlow::shade_word_icons`), and the icon's
    /// sprite box, wider than the face, would notch the sky beside it.
    #[inline]
    #[must_use]
    pub const fn below(&self, (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> (i32, i32) {
        let cells = self.cells.as_slice();
        if cells.is_empty() || y0 >= y1 || x0 >= x1 {
            return (if y0 < y1 { y0 } else { y1 }, y1);
        }
        let mut top = y0;
        let mut i = 0;
        while i < cells.len() {
            let b = cells[i];
            i += 1;
            if b.3 > top && b.2 < y1 && b.0 < x1 && x0 < b.1 {
                top = b.3;
                if top >= y1 {
                    return (y1, y1);
                }
            }
        }
        (if top < y1 { top } else { y1 }, y1)
    }

    /// Whether any box stands on a row whose top is `row_top` (`row_top - 1.0`
    /// lies in `[y0, y1)`).
    #[inline]
    #[must_use]
    pub const fn has_ceiling_at(&self, row_top: f32) -> bool {
        let above = row_top - 1.0;
        let boxes = self.boxes.as_slice();
        let mut i = 0;
        while i < boxes.len() {
            let (_, _, y0, y1) = boxes[i];
            if (y0 as f32) <= above && above < y1 as f32 {
                return true;
            }
            i += 1;
        }
        false
    }

    /// How far up a body standing on a row whose top is `row_top` may reach
    /// at window x `x`, px: `row_top` under a box that stands on the row's
    /// top, easing back to `free` (no ceiling) over `ease` px beside it.
    #[inline]
    #[must_use]
    pub const fn ceiling(&self, x: f32, row_top: f32, free: f32, ease: f32) -> f32 {
        let boxes = self.boxes.as_slice();
        if boxes.is_empty() || ease <= 0.0 || !ease.is_finite() {
            return free;
        }
        let above = row_top - 1.0;
        let mut d = ease;
        let mut i = 0;
        while i < boxes.len() {
            let (x0, x1, y0, y1) = boxes[i];
            i += 1;
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

impl Default for PetOffer {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl PetOffer {
    /// Empty pet offer.
    pub const EMPTY: Self = Self {
        perk_at: None,
        catch: None,
        mote_t: None,
        mote_rgb: None,
    };

    /// True when this frame offers the pet nothing, the common case.
    #[must_use]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.perk_at.is_none() && self.catch.is_none() && self.mote_t.is_none()
    }
}

/// The seat law's ink probe ([`placement`]) over a frame's cells: whether
/// (`row`, `col`) holds a letter. A row the frame carries holds only its
/// written columns; the columns past them, inside the grid's `cols`, are
/// blank glass, not unknown: read as unknown, they would blind every probe of
/// a head past a line's last letter, and the blind seat would put it behind
/// the caret, on the letters. A row the frame does not carry, or a column
/// past the grid, is unknown.
#[inline]
#[must_use]
pub const fn cell_ink(
    cells: &[Vec<aterm_core::terminal::RenderCell>],
    row: u16,
    col: u16,
    cols: u16,
) -> Option<bool> {
    if col >= cols {
        return None;
    }
    let r = row as usize;
    if r >= cells.len() {
        return None;
    }
    let line = cells[r].as_slice();
    let c = col as usize;
    Some(c < line.len() && !matches!(line[c].ch, ' ' | '\0'))
}

#[cfg(test)]
#[path = "companion_tests.rs"]
mod tests;
