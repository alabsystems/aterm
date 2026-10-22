// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The companion router: which cat owns the frame, where the flying head sits, and what one
//! impulse does to each body. Design of record: `RAINBOW-KITTY-V2.md` §7.2 (D13), §6.5 layer
//! 13, §6.7 (the arrival edge) and §20.1; section references are to that document.
//!
//! `rainbow kitty` and bare `kitty` mean the resident pet ([`crate::kitty_pet`]); `… flying` is
//! the flying head ([`crate::kitty_cursor`]). The pet is the default companion, so it is
//! first-class in every function here.
//!
//! This module routes and returns values; it does not animate. Nothing here reimplements or
//! retimes a pose, gait, arc or frame of art: the head's meteor beats are its own receiver
//! (`CursorCat::on_meteor`), fed the [`Flight`] returned here, and the pet's only coupling is an
//! offer ([`BodyImpulse::Perk`]).
//!
//! Both render arms (single pane and split, each with its own geometry and ink probe) reach it
//! through `EffectsPipeline::route_flying_head`, which runs [`body_for`], [`impulse_for`] and
//! [`placement`] in `duty`'s order. On a horizontal [`BodyImpulse::Fly`] it rebases the
//! placement follower so the next placement snaps to [`Flight::land`] and, when the head
//! animates, hands it the meteor. The resident pet is fed by `CompanionOwner::prepare`, never by
//! this module, and takes the perk edge through `Engine::pet_offer` → `PetBrain::note_v2_offer`
//! (`EffectsPipeline::offer_pet_v2`).
//!
//! The four laws:
//!
//! * L-A, one body per frame: [`body_for`] returns exactly one [`Body`], and [`Body::None`]
//!   when [`CompanionAdmission::body_claimed`] (`WordDecorations::claim_companion_body`) says
//!   the frame's body is taken, so exclusivity holds before either animator draws.
//! * L-B, the host's [`CompanionDuty`] (from `cursor_companion_duty`) decides the body; v2
//!   re-derives none of it, so a body the host is not drawing is never routed an impulse.
//! * L-C, one impulse, two readings: the flying head takes the meteor whole (teleport, spine
//!   impulse, whip, squash on the arrival edge); the pet is offered the arrival edge as a perk
//!   and never teleports (§22).
//! * L-D, the flying head never covers the cell being typed into whenever the grid is wide
//!   enough for the footprint on either side ([`placement`]). The pet seats itself
//!   (`PetBrain::tick`); a v2 meteor does not relocate it (§7.2(b)).
//!
//! The shipped seat (`kitty_cursor_footprint`) is a fixed lead, the body's left edge ¾ cell
//! past the caret cell's right edge, ~6 cells wide and drawn `OverText`: right at the end of a
//! line, an eraser mid-line. [`placement`] takes that footprint as handed in as step 1 and adds
//! the yields.
//!
//! Both host render arms reach this router through ONE seam,
//! [`crate::pipeline::EffectsPipeline::route_flying_head`] — the single-pane
//! arm (the live present and both capture splices) and the split path —
//! which runs [`body_for`], [`impulse_for`] and [`placement`] in that
//! order. On a [`BodyImpulse::Fly`] it applies the TELEPORT by rebasing the
//! placement follower on a horizontal flight, so the next placement snaps to
//! [`Flight::land`], and hands the head the meteor
//! (`CursorCat::on_meteor`): the spine floor, the whip, the squint and the
//! landing squash on [`Flight::land_at`], re-resolved into the frame the
//! caret teleported on. The FACING is the animator's existing `facing_left`
//! bank (§7.2: "no facing flip beyond the existing bank"). The 1-px
//! look-back toward the launch is not drawn: decided 2026-09-30 under the
//! owner's standing direction, because the eye family is baked per
//! `EyesFrame` and a one-pixel gaze would add a bake key for a beat under a
//! pixel; [`Flight::settled_at`] still bounds the flight. The §20.1 row is
//! `the_flying_head_is_at_the_landing_on_frame_zero_and_squashes_on_arrival`
//! (`pipeline_native.rs`). The resident pet is fed by
//! `CompanionOwner::prepare` (`crate::companion`) on every host, never by
//! this module, and it takes the perk edge through `Engine::pet_offer` →
//! `PetBrain::note_v2_offer` (`EffectsPipeline::offer_pet_v2`).
//!
//! ## The four laws
//!
//! * **L-A — one body per frame, never two.** [`body_for`] returns exactly one
//!   [`Body`], and it yields [`Body::None`] the moment
//!   [`CompanionAdmission::body_claimed`] says the frame's single body is
//!   already spoken for. That is `WordDecorations::claim_companion_body`
//!   (`word_decorations.rs:4002`) restated at the router, so the exclusivity
//!   holds *before* either animator is asked for a frame rather than after.
//! * **L-B — the host's `CompanionDuty` decides the body.** §7.2: *"whichever
//!   body `CompanionDuty` says owns the frame consumes it."* The router takes
//!   the host's [`CompanionDuty`] — already resolved from `pet_on_glass`,
//!   `kitty_alpha` and the caret by `cursor_companion_duty`
//!   (`companion.rs:564`) — and re-derives none of it. v2 invents no second
//!   gate, so a body the host is not drawing is never routed an impulse or
//!   charged a seat.
//! * **L-C — one impulse, two readings.** The flying head takes the meteor
//!   whole (teleport, spine impulse, whip, squash on the arrival edge); the pet
//!   is *offered* the arrival edge as a perk edge and takes nothing else. A
//!   pet never teleports — owner question **14**, RULED 2026-09-05: *"no
//!   teleport"*; the pet keeps its own pounce (§22).
//! * **L-D — the flying head never covers the cell you are typing into.** See
//!   [`placement`], which is where the measured occlusion defect is fixed. The
//!   pet is NOT seated here: it seats itself (`PetBrain::tick`), and §7.2(b)
//!   says a v2 meteor does not relocate it.
//!
//! ## The measured defect this file fixes (the flying head's seat)
//!
//! The shipped seat is a fixed lead: `kitty_cursor_footprint`
//! (`word_decorations.rs:3381`) puts the body's LEFT edge
//! `KITTY_LEAD_NUM/KITTY_LEAD_DEN` = ¾ of a cell past the caret cell's RIGHT
//! edge, and the body is ~6 cells wide and drawn `OverText`. With the caret at
//! the end of a line that is perfect — the cat escorts into blank glass. With
//! the caret MID-LINE it is an eraser: in the v2 capture it covered `cho t` of
//! `echo the` for 0.7 s. The lead is not wrong; the *unconditional* lead is.
//! [`placement`] takes the shipped footprint AS HANDED IN — byte-for-byte,
//! margin clamp and boundary rise included — as step 1, and adds the yields.
//!
//! ## Both render arms, one decision
//!
//! The single-pane and the composed (split) arm both route through
//! `EffectsPipeline::route_flying_head`, each with its own geometry and ink
//! probe, so an impulse minted for a split pane reaches the body that pane
//! draws.
//!
//! ## Determinism and allocation
//!
//! Every function here is **pure**: no clock read (`now` is injected), no
//! entropy, no interior state, no allocation. The ink probe is a borrowed
//! closure, not a collected span, so a seat costs a bounded loop over at most
//! the body's own width in cells and never touches the heap (§18).

use aterm_time::Instant;

use crate::companion::CompanionDuty;
use crate::cursor_glow::Geom;
use crate::kitty_pet::PetFrame;
use crate::word_decorations::CatFootprint;

use super::timing;
use super::{CompanionImpulse, Dir};

/// `disp = max(disp, 0.97)`: the flying head's frame-0 spine impulse (§7.2, row "Spine
/// impulse"), applied directly, bypassing the head's `DISP_TAU 0.13` follower, which would bank
/// the cat two frames after the meteor landed. 0.97 puts the bank (`BANK_LO 0.32 / HI 0.96`)
/// fully on with headroom, so a later earned peak still shows.
pub const FLYING_DISP_FLOOR: f32 = 0.97;

/// `lead = −0.30` cell at `t = 0` (§7.2, row "Whip"): the flying head is yanked back against
/// travel on the frame the caret teleports. Legal under T3 because the mark is already at full
/// brightness; only its offset moves.
pub const FLYING_LEAD_0: f32 = -0.30;

/// `LEAD_MAX = +0.22` cell: the flying head's resting lead once the whip has settled (§7.2),
/// so a stationary cat still reads as going that way.
pub const FLYING_LEAD_MAX: f32 = 0.22;

/// The 1-px look-back toward the launch lasts **250 ms** after arrival (§7.2,
/// row "Eyes"). This is the LONGEST thing v2 itself still computes after the
/// arrival edge, so it — and not the landing squash — is what
/// [`Flight::settled_at`] asks the cadence for. The squash (`LAND_DUR 0.42`)
/// belongs to the head's own animator, which keeps its own cadence; v2 does not
/// hold the frame open for a clock it does not own.
pub const FLYING_LOOKBACK_MS: f32 = 250.0;

/// The squint holds 200 ms past the arrival edge (§7.2, row "Eyes").
pub const FLYING_SQUINT_TAIL_MS: f32 = 200.0;

/// Which body owns this frame. L-A's exclusivity is the enum itself: no
/// variant means "both".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The resident pet ([`crate::kitty_pet`]): the default, and what
    /// `rainbow kitty` / `kitty` / `rainbow kitty pet` mean. It seats itself.
    Pet,
    /// The flying head ([`crate::kitty_cursor`]): `rainbow kitty flying` and the bare
    /// `nyan` / `rainbow` aliases.
    Flying {
        /// The caret cell the head escorts this frame (`CompanionDuty::FlyingHead { cell }`),
        /// so seat and snap-to resolve against one cell.
        cell: (u16, u16),
    },
    /// No companion this frame; the previous body keeps whatever exit its own animator gives it.
    None,
}

/// What the host already decided about companions. `duty` is the host's resolved custody
/// (`cursor_companion_duty`); taking it rather than its inputs keeps this file from redefining
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionAdmission {
    /// The host's resolved companion custody for this frame.
    pub duty: CompanionDuty,
    /// Another producer already claimed this frame's single companion body
    /// (`WordDecorations::claim_companion_body`).
    pub body_claimed: bool,
}

/// Which body: nothing when the frame's body is claimed, else the host's [`CompanionDuty`]
/// (`Idle` ⇒ none, `Pet` ⇒ the pet, `FlyingHead { cell }` ⇒ the head escorting `cell`). No
/// taste knob (intensity, duration, theme, reduced motion) may delete a companion, so this
/// takes no [`super::Config`].
#[inline]
#[must_use]
pub fn body_for(admitted: CompanionAdmission) -> Body {
    if admitted.body_claimed {
        return Body::None;
    }
    match admitted.duty {
        CompanionDuty::Idle => Body::None,
        CompanionDuty::Pet => Body::Pet,
        CompanionDuty::FlyingHead { cell } => Body::Flying { cell },
    }
}

/// The two poses both bodies already own, unchanged by v2 (§7.2: "Delight /
/// Wince / Oops are unchanged"), plus the bare landing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// A landing with no flight behind it.
    Land,
    /// An erase or a kill — the existing "oops" pose.
    Wince,
    /// The existing Delight pose. Earns one m1 (see [`heroes_earned`]).
    Delight,
}

/// The flying head's move: §7.2(a)'s beat table as data. The route applies the teleport
/// ([`Flight::land`], via the placement follower's rebase); facing is the animator's existing
/// bank; the spine floor, whip, squint and landing squash are read off this value by
/// `CursorCat::on_meteor`.
///
/// Minted for every credited spawn: a same-row jump at or past
/// `timing::JUMP_MIN_CELLS` (8 cells), and the Return-licensed vertical
/// variant. The route rebases the placement follower on a horizontal flight
/// (`EffectsPipeline::route_flying_head`), so the head snaps to the landing;
/// the follower itself snaps only on a target more than two rows away
/// (`Δy > 2·ch`) or a broken continuity, so a one-row Enter still glides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flight {
    /// Travel direction, dominant axis — the existing `facing_left` bank.
    pub dir: Dir,
    /// The spawn edge. `t = 0` of every curve below.
    pub t0: Instant,
    /// The arrival edge `t₀ + T` (§6.7), from [`arrival`]: read by the pin, ring, fan, flash,
    /// audio bell and landing squash.
    pub land_at: Instant,
    /// The snap-to: the landing cell, i.e. the caret the head escorts on the
    /// frame the impulse was minted (the frame the caret was first observed
    /// there). A horizontal flight rebases the follower, which then seats the
    /// head at this cell's rest instead of gliding from the launch; a vertical
    /// one keeps the follower's own rule (a snap past two rows).
    pub land: (u16, u16),
}

impl Flight {
    /// The spine value to force on frame 0 ([`FLYING_DISP_FLOOR`]), as a floor
    /// so a cat already at full momentum is not pulled down.
    #[inline]
    #[must_use]
    pub fn disp_floor(&self) -> f32 {
        FLYING_DISP_FLOOR
    }

    /// The whip in cells at `now`: [`FLYING_LEAD_0`] springing to [`FLYING_LEAD_MAX`] on
    /// [`timing::spring_whip`] (overshoots, settled ≈ 180 ms in). A pure function of
    /// `now − t₀`, so every refresh rate shows the same lean at the same wall time.
    #[inline]
    #[must_use]
    pub fn lead_at(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.t0).as_secs_f32();
        FLYING_LEAD_0 + (FLYING_LEAD_MAX - FLYING_LEAD_0) * timing::spring_whip(age)
    }

    /// The squint (§7.2(a), row "Eyes") holds through the flight and
    /// [`FLYING_SQUINT_TAIL_MS`] past the arrival edge.
    #[inline]
    #[must_use]
    pub fn squint_until(&self) -> Instant {
        add_ms(self.land_at, FLYING_SQUINT_TAIL_MS)
    }

    /// The 1-px look-back toward the launch ends here
    /// ([`FLYING_LOOKBACK_MS`] after arrival) — and this is the last thing v2
    /// itself still computes for the flight.
    #[inline]
    #[must_use]
    pub fn settled_at(&self) -> Instant {
        add_ms(self.land_at, FLYING_LOOKBACK_MS)
    }
}

/// One impulse, read by one body (D13): what [`impulse_for`] resolved a
/// [`CompanionImpulse`] into for the body that owns the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyImpulse {
    /// The flying head's meteor, whole: teleport, spine impulse, whip, squash on
    /// [`Flight::land_at`].
    Fly(Flight),
    /// The pet's whole share (§7.2(b), D13): the meteor's arrival edge `t₀ + T`, offered as its
    /// `PERK_HOLD` edge. No placement (a meteor does not relocate the pet), no spine impulse
    /// (the chase runs on its own `vhat` estimator; a second would retime `RHYTHM_MOVES` and
    /// `LEAD_TIME`), and no retiming of the pounce. The receiver is `PetBrain::note_v2_offer`,
    /// via `Engine::pet_offer`'s `perk_at`.
    Perk {
        /// The arrival edge `t₀ + T`, offered.
        at: Instant,
    },
    /// A pose both bodies already own, handed over as an edge.
    React(Reaction),
    /// This impulse means nothing to this body (every impulse, when no body owns the frame).
    Ignore,
}

impl BodyImpulse {}

/// One impulse, two readings (§7.2, D13): route a [`CompanionImpulse`] to what
/// the owning body does with it.
///
/// | impulse | [`Body::Flying`] | [`Body::Pet`] | [`Body::None`] |
/// |---|---|---|---|
/// | `Meteor` | `Fly`: snap to `cell`, `disp = max(disp, 0.97)`, `lead −0.30 → +0.22`, squash at `t₀+T` | `Perk` at `t₀+T`, nothing else | ignored |
/// | `Land` | `React(Land)` | `React(Land)` | ignored |
/// | `Wince` | `React(Wince)` | `React(Wince)` | ignored |
/// | `Delight` | `React(Delight)` | `React(Delight)` | ignored |
///
/// The three reactions are poses both animators already own; only the meteor
/// forks, exactly once.
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

/// How many m1 heroes this impulse earns (§5.8, §7.2): one for Delight, zero
/// otherwise. Keyed on the impulse (minted once per frame by
/// [`super::Engine`]), not the body, so it cannot be double-counted and a
/// frame whose body was vetoed still earns it. An earned hero is born as an m1
/// and chimes only if the [`super::stardust::StarBudget`] has a token.
#[inline]
#[must_use]
pub fn heroes_earned(impulse: CompanionImpulse) -> u8 {
    match impulse {
        CompanionImpulse::Delight => 1,
        CompanionImpulse::Meteor { .. } | CompanionImpulse::Land | CompanionImpulse::Wince => 0,
    }
}

/// Which side of the caret the body took, by the body's centre against the
/// caret cell's centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Past the caret, escorting the writing — the shipped seat.
    Ahead,
    /// Behind the caret: the ink ahead put it there (step 2 of the seat law).
    Behind,
}

/// The resolved seat of the flying head, in the [`CatFootprint`] vocabulary.
/// `x`/`y` are grid-interior px (pad-relative), as `kitty_cursor_footprint`
/// and `FreeSprite` use them, not window-absolute; the host adds the pad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seat {
    /// Sprite left edge, grid-interior px.
    pub x: i32,
    /// Sprite top edge, grid-interior px: the shipped footprint's `y` (bob and
    /// boundary rise included) moved by whole rows, so a row-1 sky seat may hang
    /// in from above row 0 like the shipped top-row rest.
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
    /// Every yield was inked (or covered the caret cell), so the body draws
    /// over the words here, as the shipped seat does on a crowded line. Never
    /// set when the host could not probe.
    pub over_ink: bool,
}

/// One [`placement`] call's geometry. No `Debug`: [`Geom`] carries none, and
/// a query is an input the host built from values it can already print.
#[derive(Clone, Copy)]
pub struct SeatQuery {
    /// Window geometry, in px — read for `cw`, `ch` and `cols` only.
    pub geom: Geom,
    /// The shipped seat as `kitty_cursor_footprint` resolved it this frame
    /// (¾-cell lead, right-margin clamp, boundary rise, hover bob). Step 1 of
    /// the seat rule is this value.
    pub rest: CatFootprint,
    /// The shipped lead in px (`cw · KITTY_LEAD_NUM / KITTY_LEAD_DEN`), handed
    /// in by the emitter that owns the constant, so the mirror seat keeps the
    /// same clearance.
    pub lead_px: i32,
    /// The head sat in the sky seat (step 3, clear) on this caret row last
    /// frame: it keeps it while the sky stays clear and the shipped seat does
    /// not ([`placement`]).
    pub held_sky: bool,
}

/// The seat rule (§6.5 layer 13): whenever the grid is wide enough for either side, the flying
/// head never covers the cell being typed into, nor ink it could avoid (L-D). In order:
///
/// 1. Ahead: [`SeatQuery::rest`], the shipped footprint, when every cell it
///    spans is blank (the usual end-of-line case).
/// 2. Behind: the mirror seat, [`SeatQuery::lead_px`] clear of the caret
///    cell's left edge, when the ahead span is inked (or, at the right margin,
///    sits on the caret) and the mirror's is not. Text behind the caret is
///    already written; text ahead is being read.
/// 3. Sky: one row up at the step-1 `x`, when both seats on the caret's row
///    are inked. From row 1 only. Taken before the mirror where the head
///    already sits there ([`SeatQuery::held_sky`]), and where the margin clamp
///    pushed the shipped seat onto the caret of a line being written (ink in
///    the [`WRITTEN_REACH`] cells before the caret): the mirror there is the
///    text the hand is typing, blank only until its echo lands, and the head
///    glided back to it at the line's end and up into the sky at the next key,
///    40 px back, 40 px on and a row up within 400 ms (2026-10-20).
/// 4. Over: no yield cleared the ink, so the body draws over the words with
///    [`Seat::over_ink`] set: the sky seat on rows ≥ 1; on row 0 whichever side leaves the
///    caret clear; the shipped footprint when the grid is too narrow for either. The head
///    never disappears to avoid an occlusion.
///
/// `ink(row, col)` answers `Some(true)` inked, `Some(false)` blank, `None` unprobeable. A
/// `None` among probed cells counts as inked; `None` for every cell of the shipped span is a
/// blind host, which gets no ink yield (no sky, no over-ink). Covering the caret cell is
/// arithmetic, not a probe, so that half of L-D holds for a blind host too.
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

    // Step 1 — the shipped footprint, verbatim.
    let ahead = census(rest.x, w, caret_row, caret, cols, cw, &ink);
    if ahead.clear() {
        return seat(query, rest.x, rest.y, caret_row, caret_left, 0, false);
    }

    // Step 3 first, where the sky is held or the margin clamp put the shipped
    // seat on the caret, and the sky is clear.
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

    // Step 2 — the mirror: the body's RIGHT edge `lead_px` before the caret
    // cell's left edge, held on the grid like the shipped seat is.
    let behind_x = caret_left
        .saturating_sub(query.lead_px)
        .saturating_sub(w)
        .clamp(0, (grid_w - w).max(0));

    // A blind host keeps the caret yield (arithmetic) but gets no ink yield: never the sky,
    // never over-ink.
    if ahead.probe == Probe::Blind {
        let behind_covers_caret = cols == 0 || {
            let (b_first, b_last) = cell_span(behind_x, w, cw, cols);
            (b_first..=b_last).contains(&caret_col)
        };
        return if ahead.covers_caret && !behind_covers_caret {
            seat(query, behind_x, rest.y, caret_row, caret_left, 0, false)
        } else {
            seat(query, rest.x, rest.y, caret_row, caret_left, 0, false)
        };
    }

    let behind = census(behind_x, w, caret_row, caret, cols, cw, &ink);
    if behind.clear() {
        return seat(query, behind_x, rest.y, caret_row, caret_left, 0, false);
    }

    // Steps 3 and 4, rows ≥ 1 — the sky, then the sky over the words. A sky
    // seat is one row up and cannot cover the caret cell.
    if caret_row > 0 {
        let sky_row = caret_row - 1;
        let sky_y = rest.y.saturating_sub(ch);
        let sky = sky_census.unwrap_or_else(|| census(rest.x, w, sky_row, caret, cols, cw, &ink));
        return seat(query, rest.x, sky_y, sky_row, caret_left, 1, !sky.clear());
    }

    // Step 4, row 0 (no sky): over the words on whichever side leaves the caret
    // clear; the shipped footprint when neither does.
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
    fn clear(self) -> bool {
        !self.covers_caret && self.probe == Probe::Clear
    }
}

/// Census the cells a body `w` px wide with its left edge at `x` px covers on
/// `row`. Bounded by the body's width in cells; allocation-free.
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
/// into `0..cols`. A body 5.2 cells wide occludes six cells, and a fractional
/// left edge one more.
#[inline]
fn cell_span(x: i32, w: i32, cw: i32, cols: u16) -> (u16, u16) {
    let last_col = i32::from(cols.saturating_sub(1));
    let first = x.div_euclid(cw).clamp(0, last_col);
    let last = (x.saturating_add(w).saturating_sub(1))
        .div_euclid(cw)
        .clamp(first, last_col);
    (
        u16::try_from(first).unwrap_or(u16::MAX),
        u16::try_from(last).unwrap_or(u16::MAX),
    )
}

/// Build the [`Seat`] for a resolved `x`/`y`. `col` is read off the final `x`
/// so the struct carries one position, not two.
#[inline]
fn seat(
    query: &SeatQuery,
    x: i32,
    y: i32,
    row: u16,
    caret_left: i32,
    lift_rows: u16,
    over_ink: bool,
) -> Seat {
    let cw = i32::try_from(query.geom.cw.max(1)).unwrap_or(i32::MAX);
    let w = i32::from(query.rest.w);
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

// The pet's receiving end. The coupling is one-way: three offers in one per-frame value
// ([`PetOffer`]): the perk at `t₀ + T` (D13); the catch, a gold m1 born within reach of a
// settled, contented cat, whose life the paw's landing shortens to `stardust::CATCH_FINISH_MS`;
// and the purr's hue, the ribbon's field under the cat. v2 mints, moves and holds nothing for
// the pet: an offer is a pure read of state other producers own, one scan of the ≤ 48-slot
// star pool and no wake. If the pet never looks, the star lives out its life.

/// The catch's reach: how many columns along its line, from the nearest edge of its body
/// ([`PetOnGlass::span`]), a settled cat is offered a star. Vertically the star must be in the
/// sky of the pet's own row ([`super::stardust::Star::in_sky_of`]).
pub const CATCH_REACH_CELLS: f32 = 10.0;

/// The pet as the sky sees it, read off its last [`PetFrame`] ([`PetOnGlass::of`]): one frame
/// stale, which keeps the sky downstream of the pet's brain so nothing here can retime a pounce.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetOnGlass {
    /// The body's left edge in fractional grid columns ([`PetFrame::col`],
    /// after the emitter's own clamps).
    pub col: f32,
    /// The body's width in fractional grid columns.
    pub width: f32,
    /// The grid row the pet's feet are on.
    pub row: u16,
    /// [`crate::kitty_pet::PetAction::settled`]: at rest on the ground. A cat mid-pounce is
    /// not offered a star.
    pub settled: bool,
    /// [`PetFrame::purr`]: the purr's intensity while its tell is up, else `0.0`. Contentment
    /// is the pet's verdict (its `PURR_GATE`); no second threshold here.
    pub purr: f32,
    /// [`PetFrame::contented`]: the same ledger over `PURR_GATE`, whatever the pose. A key
    /// resets the seat on the frame it mints a star, so the tell alone would never see a
    /// key-born star.
    pub contented: bool,
}

impl PetOnGlass {
    /// Read off the pet's published frame; `None` when the pet is not on glass (`alpha == 0`)
    /// or the cell metrics are degenerate. The span is [`PetFrame::body_px`], the dest rect
    /// actually drawn.
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
    pub fn span(&self) -> (f32, f32) {
        (self.col, self.col + self.width.max(0.0))
    }

    /// The offer gate: settled and contented (the purr tell, or the ledger over the pet's own
    /// gate whatever the pose), both the pet's own verdicts. On the ledger the catch fires for
    /// the first slow key after any run of thirteen cells.
    #[inline]
    #[must_use]
    pub fn contented(&self) -> bool {
        self.settled && (self.purr > 0.0 || self.contented)
    }

    /// Distance in columns from the body's nearest edge to grid column `col`; `0` under the
    /// body.
    #[inline]
    #[must_use]
    pub fn columns_to(&self, col: i32) -> f32 {
        let (lo, hi) = self.span();
        let c = col as f32;
        (lo - c).max(c - hi).max(0.0)
    }
}

/// One star offered to a contented cat, named by the triple that identifies it in the sky's
/// pool. The receiver hands it back on the paw's landing frame (`Engine::catch_star`), so no
/// index into a reordering pool is published.
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
/// `PetOffer::default()` (all `None`) is a frame with no meteor, star or ribbon under the cat.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PetOffer {
    /// (a) The perk edge `t₀ + T` (D13), the same `Instant` the landing pin, arrival flash,
    /// caret flare and audio bell read (§6.7): [`BodyImpulse::Perk`]'s `at`. A settled pet perks
    /// there without relocating. `None` under reduced motion (the impulse is then a
    /// [`CompanionImpulse::Land`]) and on frames with no live meteor.
    pub perk_at: Option<Instant>,
    /// (b) The star: a gold m1 within [`CATCH_REACH_CELLS`] of a contented, settled cat.
    pub catch: Option<StarCatch>,
    /// (c) The purr's hue: the ribbon's field `t` at the cell under the pet, `None` where no
    /// ribbon light is laid, so the cat's ♪/♥ share the colour of the light there.
    pub mote_t: Option<f32>,
    /// [`Self::mote_t`] resolved to RGB, snapped to one of the seven stops (`spectrum_snap`)
    /// like every point mark.
    pub mote_rgb: Option<u32>,
}

impl PetOffer {
    /// True when this frame offers the pet nothing, the common case.
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.perk_at.is_none() && self.catch.is_none() && self.mote_t.is_none()
    }
}

/// `at + ms`, saturating rather than panicking on an absurd offset. The one
/// place milliseconds become an `Instant` in this module, so nobody rounds on
/// the way and lands a frame early (§8.1's reasoning for [`timing::flight`]).
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
    /// keys whose echo has not landed), and the row above is blank. As
    /// the caret walks the last ten columns the head never takes the mirror,
    /// and once it has risen to the sky it stays there to the line's end, even
    /// where the mirror is blank glass: no glide back and on again (until
    /// 2026-10-20 it took the mirror from column 114). A line not written near
    /// the caret still takes the mirror at the margin, the shipped L-D seat.
    /// And mid-line, words ahead of the caret and the cells behind it blank
    /// on one key and written on the next, a head in the sky stays there
    /// (held), where it went down to the mirror and up again on every key.
    #[test]
    fn at_the_line_s_end_the_head_rises_once_and_stays() {
        let walk = |held: bool, margin: bool| -> Vec<(u16, u16, Side)> {
            let mut held_sky = false;
            let mut seats = Vec::new();
            for caret_col in 110..120_u16 {
                let caret = (7_u16, caret_col);
                // The mirror covers the seven cells before the caret.
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

        // The head: the whole beat table, as data.
        let BodyImpulse::Fly(flight) = impulse_for(head, imp) else {
            panic!("the flying head takes the meteor");
        };
        assert_eq!(flight.land_at, land, "the arrival edge is minted once");
        assert_eq!(flight.land, (7, 80), "the snap-to is the escorted cell");
        assert!(flight.disp_floor() >= 0.97, "§7.2: disp = max(disp, 0.97)");

        // The pet: an offer of the same edge and nothing else (the variant's one field).
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

        // No body ⇒ nothing, and the three reactions never fork.
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
        // A prompt line: `echo the` written across columns 0..40, blank after.
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
            // Blank glass: the shipped footprint wherever it clears the caret
            // cell. In the last columns the margin clamp pushes it back over the
            // caret, and L-D takes the mirror seat on the same row.
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

        // Caret mid-line at column 5 with text ahead: the shipped seat would
        // cover columns 6..12; behind is off the grid, so it lifts into the
        // sky.
        let mid = placement((caret_row, 5), &seat_query((caret_row, 5), 120), inked_line);
        assert_eq!(mid.row, caret_row - 1, "the eraser case lifts one row");
        assert_eq!(mid.lift_rows, 1);
        assert!(!mid.over_ink);

        // Text ahead, room behind: the mirror seat, the same clearance back.
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
    /// back to the shipped footprint. The head never disappears.
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

        // Row 0 cannot lift: it stays on row 0, over the words, with the caret
        // cell clear — ahead where the grid allows it.
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

        // Row 0 at the right margin: the shipped seat intrudes on the caret,
        // so the body goes behind — still on row 0, still clear of the caret.
        let wall = placement((0, 117), &seat_query((0, 117), 120), all_ink);
        assert_eq!(wall.row, 0);
        assert_eq!(wall.side, Side::Behind);
        let (first, last) = cell_span(wall.x, i32::from(wall.w), CW, 120);
        assert!(
            !(first..=last).contains(&117),
            "row 0 at the wall must not cover the caret"
        );

        // A 10-column pane, top-row caret at column 4: neither side holds a
        // 6-cell body, so the shipped footprint stands.
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

        // The right margin; the fixture must intrude: the shipped seat at
        // column 117 is clamped back over cells 114..=119.
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
        // Blindness changes the ink half of the yield, never the caret half:
        // the sighted margin seat on blank glass is the same seat.
        let sighted = placement(caret, &seat_query(caret, 120), blank);
        assert_eq!(
            (wall.x, wall.y, wall.row, wall.side),
            (sighted.x, sighted.y, sighted.row, sighted.side),
            "blind and sighted must agree wherever only the caret cell decides"
        );

        // 10-column pane, top-row caret at column 4: a blind host keeps the
        // shipped footprint and is never told it is over ink.
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

        // …but one unknowable cell beside probeable ones still counts as inked.
        let mixed = placement((7, 5), &seat_query((7, 5), 120), |r, c| {
            (r != 7 || c != 8).then_some(false)
        });
        assert_ne!(mixed.row, 7, "a single unknowable cell in the span yields");
    }
}
