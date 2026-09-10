// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE COMPANION SEAM** — *which* cat, *where* the flying head sits, and
//! *what* one impulse does to each body.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §7.2 ("Kitty — two bodies, one
//! impulse", **D13**), §6.5 layer 13, §6.7 (the arrival edge), §20.1's two
//! kitty rows, and the [`CompanionImpulse`] doc on this module's parent.
//! Section references are to that document.
//!
//! ## Why the companion gets its own file
//!
//! The spelling ruling (`cursor_glow.rs:205-225`) is that **`rainbow kitty`
//! and the bare `kitty` mean the RESIDENT PET** ([`crate::kitty_pet`]);
//! `… flying` is the rare, earned flying head ([`crate::kitty_cursor`]). The
//! pet is therefore the DEFAULT companion of the very style v2 replaces, and a
//! v2 that only drove the flying head would silently delete the cat most users
//! actually see. Owner, today: *"make sure that we also have the kitty pet,
//! too."* So the routing lives here, in one small pure module, and the PET is
//! first-class in every function below.
//!
//! ## What this module is, and is not
//!
//! It is the **router**. It decides the body, the flying head's seat and the
//! impulse, and it returns values. It is **not** the animator:
//! [`crate::kitty_pet`] and [`crate::kitty_cursor`] are owner-tuned and stay
//! byte-unchanged. Nothing here reimplements a pose, a gait, an arc or a frame
//! of art, and nothing here retimes one. D13 is explicit that the pet's
//! crouch/coil/pounce is untouched, and this file's only coupling to it is an
//! **offer** ([`BodyImpulse::Perk`]).
//!
//! ## WIRING STATUS — read this before trusting any claim below
//!
//! Nothing in the host calls this router yet. Neither render arm
//! (`App::tick_cursor_fx`, `app_render.rs:22418`, nor
//! `App::compose_cursor_companion`, `app_render.rs:31307`) calls [`duty`], and
//! the shipped pet is still fed by v1's `CompanionOwner::sense`
//! (`companion.rs:1020`). Everything here is therefore a **receiver-facing
//! value**: the four §7.2(a) beats are exposed as DATA on [`Flight`] —
//! the snap-to cell ([`Flight::land`]), the spine floor
//! ([`Flight::disp_floor`]), the whip ([`Flight::lead_at`]) and the arrival
//! edge ([`Flight::land_at`]) — and the seams that would CONSUME them do not
//! exist yet. Naming them, so the next stage cannot mistake this file for the
//! finished feature:
//!
//! * **Teleport** — the placement follower (`word_decorations.rs:3805-3826`)
//!   snaps only on a dead continuity or a > 2·`ch` row jump; the §7.2(a)
//!   `|Δx| > 8·cw` arm that sets `settle = Some(rest)` on a [`BodyImpulse::Fly`]
//!   frame is host-stage work in that file.
//! * **Spine impulse** — `CursorCat::advance_spine` is private
//!   (`kitty_cursor.rs:1302`) and the head has no public `disp` setter; a
//!   `CursorCat::on_meteor(&Flight)` seam is animator-stage work.
//! * **Whip** — `kitty_cursor.rs:1344` computes `lead = LEAD_MAX·bank`, never
//!   negative; the same seam must add [`Flight::lead_at`] in its place.
//! * **Landing squash** — `CursorCat::land_at` is private
//!   (`kitty_cursor.rs:585`) and set only to the animator's own `now`; the
//!   same seam must set it to [`Flight::land_at`].
//! * **The pet's perk offer** — `PetBrain` has no perk-edge entry (its
//!   latches are `note_bell` / `note_command_done` / `note_petted` /
//!   `note_peek`, `kitty_pet.rs:3407-3462`); [`BodyImpulse::Perk`] is an offer
//!   with no mailbox until the owner rules on §22's question 14.
//!
//! Consequently §20.1's
//! `the_flying_head_is_at_the_landing_on_frame_zero_and_squashes_on_arrival`
//! cannot yet be written end-to-end; the router's half of it is
//! `the_router_seats_the_flying_head_at_the_landing_on_the_spawn_frame` below.
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
//!   pet never teleports here — that is open owner question **14**, which the
//!   spec's §22 sheet leaves outside v2 with `keeps its own pounce` in bold.
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
//! ## KNOWN GAP — a companion silently disappears in SPLITS (host stage)
//!
//! The v1 companion decision is made inside the single-pane arm
//! (`App::tick_cursor_fx`) while the composed arm draws its companion from
//! `App::compose_cursor_companion`. Two arms, one decision — so an impulse
//! minted for a split pane can reach a body that the other arm drew, or no
//! body at all. [`duty`] is shaped so both arms can call it with their own
//! query and ink probe; wiring either arm is host-stage work (see WIRING
//! STATUS above — today neither does).
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
use crate::kitty_pet::{PetFrame, PetSense};
use crate::kitty_registry::KittyLook;
use crate::word_decorations::CatFootprint;

use super::{CompanionImpulse, Ctx, Dir, timing};

// ===========================================================================
// 1. The constants (§7.2's beat table)
// ===========================================================================

/// **`disp = max(disp, 0.97)`** — the flying head's frame-0 spine impulse
/// (§7.2, row "Spine impulse"), applied DIRECTLY and bypassing the head's own
/// `DISP_TAU 0.13` follower.
///
/// Why 0.97 and not 1.0: the head's bank runs `BANK_LO 0.32 / HI 0.96`, so
/// 0.97 is the first value that puts the bank fully on with a hair of headroom
/// left, and a value at the rail would make a subsequent earned peak invisible.
/// Why bypass the follower at all: T2. A follower that eases to 0.97 over
/// 130 ms banks the cat two frames after the meteor has already landed, which
/// is exactly the lag tell v2 exists to delete.
pub const FLYING_DISP_FLOOR: f32 = 0.97;

/// **`lead = −0.30` cell at `t = 0`** (§7.2, row "Whip") — the flying head is
/// yanked backwards on the frame the caret teleports.
///
/// Negative because it is measured AGAINST travel: the cat did not choose to
/// go, it was pulled, and the body arrives before the pose does. This is the
/// one place in the theme where a mark starts behind where it will settle, and
/// it is legal under T3 because the mark is already at full brightness — only
/// its offset moves.
pub const FLYING_LEAD_0: f32 = -0.30;

/// **`LEAD_MAX = +0.22`** cell — the flying head's resting lead once the whip
/// has settled (§7.2). The head leans into travel at rest, which is what makes
/// a stationary cat still read as "going that way".
pub const FLYING_LEAD_MAX: f32 = 0.22;

/// The flying head squints (`HAPPY_GATE`) for the flight **plus 200 ms**
/// (§7.2, row "Eyes"). A pose parameter on existing frames; no new art.
pub const FLYING_SQUINT_MS: f32 = 200.0;

/// The 1-px look-back toward the launch lasts **250 ms** after arrival (§7.2,
/// row "Eyes"). This is the LONGEST thing v2 itself still computes after the
/// arrival edge, so it — and not the landing squash — is what
/// [`BodyImpulse::deadline`] asks the cadence for. The squash (`LAND_DUR 0.42`)
/// belongs to the head's own animator, which keeps its own cadence; v2 does not
/// hold the frame open for a clock it does not own.
pub const FLYING_LOOKBACK_MS: f32 = 250.0;

// ===========================================================================
// 2. Which body (§7.2, D13; the host's CompanionDuty)
// ===========================================================================

/// WHICH BODY owns this frame. The exclusivity of L-A is the enum itself: there
/// is no variant that means "both".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The **resident pet** ([`crate::kitty_pet`]) — the default install, and
    /// what `rainbow kitty` / `kitty` / `rainbow kitty pet` all mean. It seats
    /// itself; the router never hands it a position.
    Pet,
    /// The **flying head** ([`crate::kitty_cursor`]) — `rainbow kitty flying`,
    /// the bare `nyan` / `rainbow` aliases, and the pet-mode sing-along's
    /// singing face.
    Flying {
        /// The caret cell the head escorts this frame — the host's
        /// `CompanionDuty::FlyingHead { cell }`, carried through so the seat
        /// and the snap-to are resolved against ONE cell.
        cell: (u16, u16),
    },
    /// No companion this frame. Not an error and not a fade: a frame with no
    /// admitted body simply draws no cat, and the body that was there keeps
    /// whatever exit its own animator gives it.
    None,
}

impl Body {
    /// True when this body puts pixels on glass — the predicate a host uses to
    /// spend its single companion claim.
    #[must_use]
    pub fn on_glass(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// What the host already decided about companions before v2 was asked.
///
/// Both fields are verdicts the host reaches for the other nine styles too,
/// so v2 re-derives neither. `duty` is the host's own custody law
/// (`cursor_companion_duty`, `companion.rs:564`), which already folds the
/// companion opt-in, presentability, serious mode, the spelling ruling
/// (`GlowStyle::style_names_any_pet` → `pet_companion_admitted` /
/// `flying_kitty_admitted`), the sing-along drive, the shed envelope and the
/// caret's visibility into ONE value. Taking that value — rather than its
/// inputs — is what keeps this file from being a second definition of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionAdmission {
    /// The host's resolved companion custody for this frame.
    pub duty: CompanionDuty,
    /// Another producer already claimed this frame's single companion body
    /// (`WordDecorations::claim_companion_body`, `word_decorations.rs:4002`).
    pub body_claimed: bool,
}

/// **WHICH BODY** — the host's [`CompanionDuty`] restated at the router, with
/// the claim applied (§7.2, D13; L-A, L-B).
///
/// 1. **The claim** — L-A. A frame's single body is already taken ⇒ nothing.
/// 2. **The duty** — `Idle` ⇒ nothing; `Pet` ⇒ the pet; `FlyingHead { cell }`
///    ⇒ the head, escorting `cell`.
///
/// No taste knob may delete a companion: `intensity`, `duration`, the theme
/// and reduced motion change how a companion *looks* or *moves*, never
/// whether it *exists*. That law is structural here — this function has no
/// [`super::Config`] input at all.
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

// ===========================================================================
// 3. What the impulse means to that body (§7.2, D13; §6.7's arrival edge)
// ===========================================================================

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

/// THE FLYING HEAD'S MOVE, resolved — §7.2(a)'s beat table (Teleport, Spine
/// impulse, Whip, Facing, Eyes, Landing squash) **as data** for the animator
/// seams the WIRING STATUS names. Nothing here is applied; every field and
/// method is a value a receiver reads.
///
/// A `Flight` is minted for every CREDITED spawn: §6.1's same-row jump at or
/// past [`timing::JUMP_MIN_CELLS`] — the same 8 cells §7.2(a)'s snap clause
/// names (`|Δx| > 8·cw`) — and §6.11's Return-licensed vertical variant,
/// whose path may be a single row. The snap VERDICT is the follower's, not
/// the router's: the follower holds the previous rest the router never sees,
/// and it keeps the shipped `Δy > 2·ch` row-jump rule beside the new `Δx`
/// clause, so a one-row Enter still glides exactly as it does today. The
/// router hands over the cell ([`Flight::land`]), never the verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flight {
    /// Travel direction, dominant axis — the existing `facing_left` bank.
    pub dir: Dir,
    /// The spawn edge. `t = 0` of every curve below.
    pub t0: Instant,
    /// **THE ARRIVAL EDGE** `t₀ + T` (§6.7) — read by the pin, the ring, the
    /// fan, the flash and the audio bell. The landing squash lands ON this
    /// instant, so the squash, the pin and the bell are one event. Derived
    /// once in this file by [`arrival`], from the `(t0, t_flight)` the
    /// contract carries; carrying the `Instant` itself on
    /// [`CompanionImpulse::Meteor`] is the contract's follow-up.
    pub land_at: Instant,
    /// **THE SNAP-TO** (§7.2(a), row "Teleport"): the landing cell — the
    /// caret the head escorts on the frame the impulse was minted, which by
    /// T2 is the frame the caret was first observed at its landing.
    /// [`Duty::seat`] on that frame is the placement follower's rest for THIS
    /// cell, and where the follower's snap clause holds (`|Δx| > 8·cw`, or
    /// the shipped `Δy > 2·ch`) the receiver SETS the follower there
    /// (`settle = Some(rest)`) instead of gliding from the launch: a cat
    /// flying the path arrives late; a cat at the landing on frame 0 is the
    /// meteor's proof of speed.
    pub land: (u16, u16),
}

impl Flight {
    /// Draw the body mirrored — the existing `facing_left` bank, and no other
    /// facing state (§7.2: "no facing flip beyond the existing bank").
    #[must_use]
    pub fn facing_left(&self) -> bool {
        matches!(self.dir, Dir::Left)
    }

    /// The spine value to force on frame 0 — [`FLYING_DISP_FLOOR`], applied as
    /// a floor (`max`) so a cat already at full earned momentum is not pulled
    /// DOWN by its own meteor.
    #[must_use]
    pub fn disp_floor(&self) -> f32 {
        FLYING_DISP_FLOOR
    }

    /// THE WHIP, in cells at `now`: [`FLYING_LEAD_0`] springing to
    /// [`FLYING_LEAD_MAX`] on [`timing::spring_whip`] (§2.5's `spring-whip`,
    /// ω 24 rad/s, ζ 0.6 — under 1, so it deliberately OVERSHOOTS its rest
    /// before settling ≈ 180 ms in).
    ///
    /// One curve, one call: the lead is a pure function of `now − t₀`, so a
    /// 60 Hz panel and a 120 Hz panel show the same lean at the same wall time
    /// (T7), and a dropped frame costs nothing but the frame.
    #[must_use]
    pub fn lead_at(&self, now: Instant) -> f32 {
        let age = now.saturating_duration_since(self.t0).as_secs_f32();
        FLYING_LEAD_0 + (FLYING_LEAD_MAX - FLYING_LEAD_0) * timing::spring_whip(age)
    }

    /// True once the caret's own landing frame has passed — the edge the
    /// squash, the pin, the ring and the bell all key on.
    #[must_use]
    pub fn landed(&self, now: Instant) -> bool {
        now >= self.land_at
    }

    /// The squint holds through the flight and [`FLYING_SQUINT_MS`] past it.
    #[must_use]
    pub fn squint_until(&self) -> Instant {
        add_ms(self.land_at, FLYING_SQUINT_MS)
    }

    /// The 1-px look-back toward the launch ends here
    /// ([`FLYING_LOOKBACK_MS`] after arrival) — and this is the last thing v2
    /// itself still computes for the flight, hence
    /// [`BodyImpulse::deadline`]'s answer.
    #[must_use]
    pub fn settled_at(&self) -> Instant {
        add_ms(self.land_at, FLYING_LOOKBACK_MS)
    }
}

/// ONE IMPULSE, READ BY ONE BODY (D13). What [`impulse_for`] resolved a
/// [`CompanionImpulse`] into for the body that actually owns the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyImpulse {
    /// The FLYING head's meteor, whole: teleport, spine impulse, whip, squash
    /// on [`Flight::land_at`].
    Fly(Flight),
    /// **THE PET'S OFFER, AND THE WHOLE OF IT** (§7.2(b), D13). `at` is the
    /// meteor's arrival edge `t₀ + T`, offered to the pet as its `PERK_HOLD`
    /// edge. It is an OFFER, not a command, and it carries nothing else:
    ///
    /// * no placement — a v2 meteor **does not relocate the pet**;
    /// * no spine impulse — the pet's chase runs on its own `vhat` estimator,
    ///   and injecting v2's spine would drive owner-tuned thresholds
    ///   (`RHYTHM_MOVES`, `LEAD_TIME`) from a second integrator, which is a
    ///   retiming of the pounce by another name;
    /// * no retiming — `POUNCE_JUMP 6` → `PERK_HOLD 0.30` → `CROUCH_DUR 0.10`
    ///   → scaled coil → `clamp(0.021·cells, 0.16, 0.42)` flight, and the
    ///   big-jump show at `BIG_JUMP_COLS 24`, are all exactly what they were.
    ///
    /// **There is no receiver for it in this stage** — `PetBrain` has no
    /// perk-edge entry (WIRING STATUS). Whether the pet should TELEPORT on a
    /// long jump is open owner question **14** (§22, default in bold: *keeps
    /// its own pounce*), deliberately outside v2. Answering it is a change to
    /// this one variant.
    Perk {
        /// The arrival edge `t₀ + T`, offered.
        at: Instant,
    },
    /// A pose both bodies already own, handed over as an EDGE.
    React(Reaction),
    /// This impulse means nothing to this body — including every impulse when
    /// no body owns the frame.
    Ignore,
}

impl BodyImpulse {
    /// The instant after which **v2 itself** is done computing for this
    /// impulse, or `None` when v2 was done the moment it handed it over.
    ///
    /// The distinction is the whole of [`Duty::needs_frames`]: v2 holds the
    /// frame cadence open only for the things v2 is still evaluating — the
    /// whip's lead, the squint and the look-back of a [`BodyImpulse::Fly`].
    /// A [`BodyImpulse::React`] is an EDGE, and so is a [`BodyImpulse::Perk`]:
    /// the offer is a value, v2 computes nothing after minting it, and a
    /// router that armed up to 120 ms of cadence for a mailbox that does not
    /// exist would be holding the frame for nobody. The pose clocks belong to
    /// the animators (the pet's `PetFrame::fp`, the head's
    /// `CursorCat::is_active`), and a router that also armed the cadence for
    /// them would be a second, disagreeing answer to "is anything animating".
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Fly(flight) => Some(flight.settled_at()),
            Self::Perk { .. } | Self::React(_) | Self::Ignore => None,
        }
    }
}

/// **ONE IMPULSE, TWO READINGS** (§7.2, D13) — route a [`CompanionImpulse`] to
/// what the body that owns the frame actually does with it.
///
/// | impulse | [`Body::Flying`] | [`Body::Pet`] | [`Body::None`] |
/// |---|---|---|---|
/// | `Meteor` | `Fly` — snap to `cell`, `disp = max(disp, 0.97)`, `lead −0.30 → +0.22`, squash at `t₀+T` | `Perk` at `t₀+T`, and nothing else | ignored |
/// | `Land` | `React(Land)` | `React(Land)` | ignored |
/// | `Wince` | `React(Wince)` | `React(Wince)` | ignored |
/// | `Delight` | `React(Delight)` | `React(Delight)` | ignored |
///
/// The three reactions do not fork: they are poses both animators already own
/// and v2 changes neither. Only the meteor forks, and it forks exactly once —
/// which is what "one impulse, two bodies" means.
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

/// The arrival edge `t₀ + T` (§6.7), derived ONCE in this file from the shape
/// the contract hands over. Both readings of a meteor take it from here, so
/// the squash and the perk offer can never disagree about the landing frame.
fn arrival(t0: Instant, t_flight: std::time::Duration) -> Instant {
    t0.checked_add(t_flight).unwrap_or(t0)
}

/// How many m1 heroes this impulse EARNS (§5.8, §7.2: "Delight additionally
/// earns one m1").
///
/// Exactly one, for Delight, and zero for everything else. It takes the
/// IMPULSE and not the body on purpose: the impulse is minted once per frame by
/// [`super::Engine`], so the hero cannot be double-counted by two bodies, and a
/// frame whose body was vetoed still earns its hero — D5's "light is never
/// rationed" applied to the companion. An earned hero is always born as an m1
/// and chimes only if the [`super::stardust::StarBudget`] has a token.
#[must_use]
pub fn heroes_earned(impulse: CompanionImpulse) -> u8 {
    match impulse {
        CompanionImpulse::Delight => 1,
        CompanionImpulse::Meteor { .. } | CompanionImpulse::Land | CompanionImpulse::Wince => 0,
    }
}

// ===========================================================================
// 4. Where the flying head sits (§6.5 layer 13) — and the occlusion fix
// ===========================================================================

/// Which side of the caret the body took, by the body's centre against the
/// caret cell's centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Past the caret, escorting the writing — the shipped seat.
    Ahead,
    /// Behind the caret: the ink ahead put it there (step 2 of the seat law).
    Behind,
}

/// The resolved seat of the FLYING head — in the vocabulary of the
/// [`CatFootprint`] it was resolved from.
///
/// `x`/`y` are **grid-interior px** (pad-relative), exactly as
/// `kitty_cursor_footprint` and `FreeSprite` speak them — NOT the glow
/// streams' window-absolute px. The host stamps the sprite and adds the pad
/// itself, so a seat that already carried `origin_x` would be drawn a pad too
/// far right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seat {
    /// Sprite LEFT edge, grid-interior px.
    pub x: i32,
    /// Sprite TOP edge, grid-interior px. The shipped footprint's `y` — bob
    /// and boundary rise included — moved by whole rows and never re-derived,
    /// so a row-1 sky seat may hang in from above row 0 exactly as the
    /// shipped top-row rest already does.
    pub y: i32,
    /// Sprite width in px, as the baker sized it.
    pub w: u16,
    /// Sprite height in px, as the baker sized it.
    pub h: u16,
    /// [`Self::x`] read in fractional grid columns — DERIVED from `x` after
    /// every clamp, never a second position.
    pub col: f32,
    /// The grid row the body sits on, already lifted.
    pub row: u16,
    /// Which side of the caret the body's centre landed on.
    pub side: Side,
    /// Rows lifted above the caret's own row (0 or 1).
    pub lift_rows: u16,
    /// Every yield was inked (or covered the caret cell), and the body is
    /// drawn OVER the words at this seat — the loss the shipped `OverText`
    /// seat already accepts on every crowded line today. Never set when the
    /// host could not probe: v2 relocates a body on evidence, not on a guess.
    pub over_ink: bool,
}

/// One [`placement`] call's geometry. No `Debug`: [`Geom`] carries none, and
/// a query is an input the host built from values it can already print.
#[derive(Clone, Copy)]
pub struct SeatQuery {
    /// Window geometry, in px — read for `cw`, `ch` and `cols` only.
    pub geom: Geom,
    /// **THE SHIPPED SEAT**, as `kitty_cursor_footprint` resolved it for this
    /// frame: the ¾-cell lead, the right-margin clamp, the boundary rise and
    /// the hover bob, byte-for-byte. Step 1 of the seat law IS this value; the
    /// router restates none of its constants.
    pub rest: CatFootprint,
    /// The shipped lead in px (`cw · KITTY_LEAD_NUM / KITTY_LEAD_DEN`), handed
    /// in by the emitter that owns the constant, so the mirror seat of step 2
    /// keeps the same clearance the ahead seat has and this file spells no
    /// second `¾`.
    pub lead_px: i32,
}

/// **THE SEAT LAW** (§6.5 layer 13) — where the FLYING head sits, such that
/// **it never covers the cell the caret is typing into and never covers inked
/// cells it could have avoided** (L-D). The pet is not seated here (§7.2(b)).
///
/// ## The four steps, in order
///
/// 1. **Ahead** — [`SeatQuery::rest`], the shipped footprint, verbatim. Taken
///    when every cell it spans is blank. This is the normal case — you type
///    at the end of a line and the cat escorts into empty glass — and it is
///    byte-identical to today.
/// 2. **Behind** — the mirror seat, [`SeatQuery::lead_px`] clear of the caret
///    cell's LEFT edge, taken when the ahead span is inked (or, at the right
///    margin, sits on the caret) and the mirror's is not. Mid-line, the text
///    ahead of the caret is the text you are reading; the text behind it is
///    the text you already wrote. Yielding backwards is the cheaper loss.
/// 3. **Sky** — one row up, at the step-1 `x`, when both seats on the caret's
///    row are inked. Available from row 1; a top-row caret has no sky, and the
///    shipped rise law already refuses to lift a top-row body off-grid.
/// 4. **Over** — no yield cleared the ink, so the body draws OVER the words
///    with [`Seat::over_ink`] set: on rows ≥ 1 the sky seat (never the
///    caret's own row); on row 0 the ahead seat, or the behind seat, whichever
///    leaves the caret cell clear; and when the grid is too narrow for either
///    side, the shipped footprint itself — today's answer, boundary rise and
///    all. **The head never disappears to solve an occlusion.** A cat that
///    blinks out to avoid a letter is a worse bug than the letter being
///    covered, and it is exactly the stray-disappearance class the owner has
///    vetoed elsewhere. There is no under-ink mode: §6.5 lists the head as a
///    `FreeSprite` drawn `OverText`, and no renderer has an under lane for it.
///
/// ## The ink probe
///
/// `ink(row, col)` answers `Some(true)` inked, `Some(false)` blank, `None`
/// **unprobeable**. A `None` among probed cells counts as INKED — never cover
/// what you cannot see (D16's rule for a star's birth cell, applied to a
/// body). A probe that answers `None` for EVERY cell of the shipped span is a
/// host with no ink information at all, and that host gets no INK yield: D16
/// is about birthing stars on a guess, not about relocating a body that is
/// already on glass, and an ink-blind first wiring must not move the
/// owner-tuned seat one pixel on the strength of a probe it does not have. A
/// blind host therefore never sees step 3, and never an over-ink verdict.
///
/// The caret cell is covered or not by ARITHMETIC, not by the probe, and that
/// half of L-D holds for a blind host exactly as for a sighted one: where the
/// shipped seat's own margin clamp has pushed it back over the caret cell,
/// the blind host takes step 2's mirror; only where neither side clears the
/// caret (a grid too narrow for the body on either side) does it keep the
/// shipped footprint, today's answer. The blind arm moves the body on the one
/// fact it has, never on a guess — and the seat it moves to is the same seat
/// the sighted law resolves on blank glass.
///
/// Pure and allocation-free: the probe is borrowed, and each loop is bounded
/// by the body's own width in cells.
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

    // Step 2 — the mirror: the body's RIGHT edge `lead_px` before the caret
    // cell's left edge, held on the grid like the shipped seat is.
    let behind_x = caret_left
        .saturating_sub(query.lead_px)
        .saturating_sub(w)
        .clamp(0, (grid_w - w).max(0));
    let behind = census(behind_x, w, caret_row, caret, cols, cw, &ink);

    // An ink-blind host: the INK half of the yield is off (no evidence), the
    // CARET half is arithmetic and stays on. The shipped seat wherever it
    // clears the caret cell; the mirror where the margin clamp pushed the
    // shipped seat onto the caret and the mirror clears it; the shipped seat
    // again when neither side clears (a grid too narrow for the body on
    // either side). Never the sky, never an over-ink verdict.
    if ahead.probe == Probe::Blind {
        return if ahead.covers_caret && !behind.covers_caret {
            seat(query, behind_x, rest.y, caret_row, caret_left, 0, false)
        } else {
            seat(query, rest.x, rest.y, caret_row, caret_left, 0, false)
        };
    }
    if behind.clear() {
        return seat(query, behind_x, rest.y, caret_row, caret_left, 0, false);
    }

    // Steps 3 and 4, rows ≥ 1 — the sky, then the sky over the words. A sky
    // seat is one row up and cannot cover the caret cell.
    if caret_row > 0 {
        let sky_row = caret_row - 1;
        let sky_y = rest.y.saturating_sub(ch);
        let sky = census(rest.x, w, sky_row, caret, cols, cw, &ink);
        return seat(query, rest.x, sky_y, sky_row, caret_left, 1, !sky.clear());
    }

    // Step 4, row 0 — no sky. Over the words on the caret's row, on whichever
    // side leaves the caret cell clear; the shipped footprint when neither
    // does (a grid too narrow for the body on either side of the caret).
    if !ahead.covers_caret {
        return seat(query, rest.x, rest.y, 0, caret_left, 0, true);
    }
    if !behind.covers_caret {
        return seat(query, behind_x, rest.y, 0, caret_left, 0, true);
    }
    seat(query, rest.x, rest.y, 0, caret_left, 0, true)
}

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
    let mut covers_caret = false;
    let mut inked = false;
    let mut blank = 0_u32;
    let mut unknown = 0_u32;
    for c in first..=last {
        if (row, c) == caret {
            covers_caret = true;
            continue;
        }
        match ink(row, c) {
            Some(true) => inked = true,
            Some(false) => blank += 1,
            None => unknown += 1,
        }
    }
    let probe = if inked || (unknown > 0 && blank > 0) {
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

// ===========================================================================
// 5. What the pet senses (§7.2(b))
// ===========================================================================

/// The frame facts only the HOST holds — the four things [`PetSense`] wants
/// that v2 genuinely cannot see.
///
/// Kept deliberately small. Everything else on a `PetSense` is geometry and
/// posture v2 already carries on its [`Ctx`], and anything the pet can observe
/// for itself is not put on this struct: that is `kitty_pet.rs`'s own-sensor
/// doctrine (`kitty_pet.rs:1897`), which exists because a pet that stopped
/// behaving when a *trail* was turned down would be a bug with no explanation.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostSense {
    /// **THE GRID CARET** — the visible caret cell `(row, col)`, or `None`
    /// when the cursor is hidden (DECTCEM) or the viewport is scrolled into
    /// history: `TerminalFacts::caret` (`host.rs:160`), read from the grid
    /// every frame. This is deliberately NOT [`Ctx::caret`]: that is the last
    /// LICENSED landing (`Engine::on_event` writes it only from a licensed
    /// `Event::Move`), so it never sees a program-driven cursor, a TUI
    /// repaint or an alt-screen app — and a pet fed from it would sit at a
    /// stale station and then Startle at a phantom delta on the next licensed
    /// move. The pet is its own move sensor; it must diff the real caret.
    ///
    /// The host applies the SONG's caret law before filling this: v1 hands
    /// the pet `facts.caret` only while `pet_caret_admitted` holds
    /// (`companion.rs:1020`), withholding it through the sing-along's face
    /// swap so the pet fades out holding position. A host that migrates to
    /// [`sense`] hands v2 that same post-law value, never the raw grid read.
    pub caret: Option<(u16, u16)>,
    /// The emulator wrapped the caret since the last host read. A FACT from
    /// the grid, never a heuristic: it is the only thing separating a
    /// bottom-row scrolled wrap from `Home` at the last column, which look
    /// byte-identical on the grid.
    pub wrapped: bool,
    /// The focused pane is genuinely streaming this frame (scroll or content
    /// clock advanced AND the shell is in its OSC 133/633 Execute phase).
    pub output_burst: bool,
    /// The mouse pointer in fractional grid cells of the pet's pane, `None`
    /// outside it. The pet's brain diffs it itself.
    pub pointer: Option<(f32, f32)>,
}

/// **WHAT THE PET SENSES** (§7.2(b)) — build the [`PetSense`] the pet's
/// `PetBrain::tick` (`kitty_pet.rs:3494`) wants, out of the host facts above
/// plus the geometry and posture on v2's [`Ctx`].
///
/// §7.2(b) defines no pet projection at all — the pet's choreography is
/// "untouched" and the ONLY coupling is the offered impulse — so this is a
/// PROJECTION of the same facts v1's `CompanionOwner::sense`
/// (`companion.rs:1020`) feeds it, and nothing more. In particular:
///
/// * the caret is the host's grid caret ([`HostSense::caret`]), never
///   [`Ctx::caret`] — see the field doc for why;
/// * the spine ([`Ctx::disp`], [`Ctx::birth_disp`], [`Ctx::phase`]) is
///   deliberately NOT injected: the pet runs its own `vhat` velocity estimate
///   against its own owner-tuned thresholds (`RHYTHM_MOVES`, `LEAD_TIME`,
///   `LEAD_MAX`), and a second momentum signal would retime the chase and the
///   pounce — precisely what D13 forbids.
///
/// `reduced_motion` comes from the LIVE config the engine assembles each tick
/// (`cfg.reduced_motion || Event::ReducedMotion`), so a posture change reaches
/// the pet on the same frame it reaches the ribbon.
#[must_use]
pub fn sense(ctx: &Ctx<'_>, host: HostSense) -> PetSense {
    PetSense {
        now: ctx.now,
        caret: host.caret,
        wrapped: host.wrapped,
        rows: u16::try_from(ctx.geom.rows).unwrap_or(u16::MAX),
        cols: u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
        cell_w: u16::try_from(ctx.geom.cw).unwrap_or(u16::MAX),
        cell_h: u16::try_from(ctx.geom.ch).unwrap_or(u16::MAX),
        reduced_motion: ctx.cfg.reduced_motion,
        output_burst: host.output_burst,
        pointer: host.pointer,
    }
}

// ===========================================================================
// 5b. THE PET AND THE SKY (panel #10) — the resident's whole receiving end
// ===========================================================================
//
// D13 left the pet ONE coupling — the offered perk edge — and no mailbox for
// it (WIRING STATUS). Panel #10 keeps the coupling one-way and makes it
// three offers instead of one, all riding the SAME per-frame value:
//
//   (a) the perk        — `t₀ + T`, the landing pin's own instant (D13);
//   (b) the catch       — a gold m1 born within reach of a settled, contented
//                         cat, offered to it; the paw's landing shortens that
//                         one star's life to `stardust::CATCH_FINISH_MS`;
//   (c) the purr's hue  — the ribbon's field under the cat, so the ♪/♥ of a
//                         contented resident wear the rainbow at its own
//                         position (C2, extended to the pet).
//
// **Every one is an OFFER, and none of them is light.** v2 mints nothing for
// the pet, moves nothing of the pet, and holds no frame open for it: a
// [`PetOffer`] is a pure read of state three other producers already own
// (the impulse slot, the sky's pool, the ribbon's field index), so the whole
// of #10 costs the frame one scan of a ≤ 48-slot pool and no wake at all
// (T6). D13's ruling stands untouched: the pet is offered, never driven, and
// it is never relocated.
//
// **T1 stands too.** The catch spends a star a KEYSTROKE made — the sky's own
// 1-in-12 m1 deal crossed with §5.3's 15 % gold, which is the ~1-key-in-80
// the panel names — and the cat's reaching for it draws nothing. If the pet
// never looks, the star lives its own life out; if it does, the only thing
// that changes is when that life ends.

/// **THE CATCH'S REACH: TEN COLUMNS** (panel #10(b)) — how far along its own
/// line a settled cat is offered a star, measured from the NEAREST EDGE of
/// its body ([`PetOnGlass::span`]), not from a point: a six-cell cat whose
/// nose is one column from a star is one column from it.
///
/// The vertical half of the reach is not a number but a law — the star must
/// be in the sky of the pet's own row ([`super::stardust::Star::in_sky_of`])
/// — because that band IS the strip of glass directly over the cat, whichever
/// ribbon spelling the host runs (§5.4, D16). A paw reaches sideways and up
/// one line; it does not reach three lines up.
pub const CATCH_REACH_CELLS: f32 = 10.0;

/// **THE PET, AS THE SKY SEES IT** — the five facts an offer is resolved
/// against, and nothing else.
///
/// Filled from the pet's own last [`PetFrame`] ([`PetOnGlass::of`]), which
/// means the sky reads the cat ONE frame stale. That is deliberate and it is
/// the whole reason #10 needs no new plumbing: the pet's brain ticks where it
/// ticks today, publishes the frame it always published, and v2 reads that
/// value the next time it is asked. Nothing here can retime a pounce, because
/// nothing here is upstream of one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetOnGlass {
    /// The body's LEFT edge in fractional grid columns ([`PetFrame::col`],
    /// after the emitter's own clamps).
    pub col: f32,
    /// The body's width in fractional grid columns.
    pub width: f32,
    /// The grid row the pet's feet are on.
    pub row: u16,
    /// [`crate::kitty_pet::PetAction::settled`] — at rest on the ground.
    /// A cat mid-pounce is not offered a star: it is already busy, and an
    /// offer it could not take would be an offer that lies.
    pub settled: bool,
    /// [`PetFrame::purr`] — the purr's own intensity, which the pet publishes
    /// as `content` while its TELL is up and `0.0` otherwise
    /// (`kitty_pet.rs:8436`). CONTENTMENT IS THE PET'S WORD, not v2's: this
    /// file invents no second threshold beside the pet's `PURR_GATE`, it just
    /// asks whether the cat says it is purring.
    pub purr: f32,
}

impl PetOnGlass {
    /// Read the five facts off the pet's own published frame. `None` when the
    /// pet is not on glass at all (`alpha == 0`) or the cell metrics are
    /// degenerate — the same frames its emitter draws nothing for, and the
    /// frames on which the sky must offer nothing.
    ///
    /// The body span is taken from [`PetFrame::body_px`], which is the dest
    /// rect the emitter actually draws (squash, lift and all four clamps
    /// folded in), so the reach is measured against the cat that is on the
    /// glass rather than a model of it.
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
        })
    }

    /// The body's column span, `[left, right]`, in fractional grid columns.
    #[must_use]
    pub fn span(&self) -> (f32, f32) {
        (self.col, self.col + self.width.max(0.0))
    }

    /// **THE OFFER GATE** — a cat is offered a star only while it is SETTLED
    /// and CONTENTED (§7.2(b)'s posture, and the pet's own purr tell). Both
    /// halves are the pet's own verdicts, restated; v2 adds no third.
    #[must_use]
    pub fn contented(&self) -> bool {
        self.settled && self.purr > 0.0
    }

    /// Distance in COLUMNS from the body's nearest edge to grid column `col`
    /// — `0` for a column the body is standing on.
    #[must_use]
    pub fn columns_to(&self, col: i32) -> f32 {
        let (lo, hi) = self.span();
        let c = col as f32;
        (lo - c).max(c - hi).max(0.0)
    }
}

/// **ONE STAR, OFFERED** (panel #10(b)) — the gold m1 within reach of a
/// contented cat, named by the triple that identifies it in the sky's pool.
///
/// The receiver hands this VALUE BACK on the paw's landing frame
/// (`Engine::catch_star`), which is what makes the catch impossible to
/// mis-address: v2 never publishes an index into a pool that reorders, and
/// the pet never has to know what a star is.
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

/// **WHAT V2 OFFERS THE RESIDENT PET THIS FRAME** — the whole of panel #10,
/// in one value, minted by `Engine::pet_offer`.
///
/// Every field is `Option`, and `PetOffer::default()` (all `None`) is exactly
/// what a frame with no meteor, no star and no ribbon under the cat produces
/// — so a receiver that reads it every frame does nothing on almost all of
/// them, and a host that never calls it changes nothing at all.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PetOffer {
    /// **(a) THE PERK EDGE** — `t₀ + T`, D13's offer, and the same `Instant`
    /// the landing pin, the arrival flash, the caret flare and the audio bell
    /// read (§6.7, §8.1 no. 3). A settled pet should perk exactly THERE, with
    /// no relocation: this is the arrival, not the launch.
    ///
    /// It is [`BodyImpulse::Perk`]'s `at`, routed through [`impulse_for`]
    /// with [`Body::Pet`] — the router is not bypassed, it is finally read.
    /// `None` under reduced motion (the impulse is a [`CompanionImpulse::Land`]
    /// there, and a landing is a pose, not an edge to wait for) and on every
    /// frame with no live meteor impulse.
    pub perk_at: Option<Instant>,
    /// **(b) THE STAR** — a gold m1 within [`CATCH_REACH_CELLS`] of a
    /// contented cat. `None` unless the cat is settled and purring.
    pub catch: Option<StarCatch>,
    /// **(c) THE PURR'S HUE** — the ribbon's field `t` at the cell under the
    /// pet, or `None` where no ribbon light is laid there. C2 says the caret,
    /// the ribbon head and a star's halo on a cell are the same colour on the
    /// same frame; #10(c) adds the cat's own ♪/♥ to that list.
    pub mote_t: Option<f32>,
    /// [`Self::mote_t`] resolved to an RGB, so the receiver needs no colour
    /// law of its own. A mote is a POINT MARK, so C1 snaps it to one of the
    /// seven stops (`spectrum_snap`) exactly as a star's tint and a pin's
    /// arms are snapped — a mote sampling the bed's continuous walk would be
    /// the one un-snapped point mark in the theme.
    pub mote_rgb: Option<u32>,
}

impl PetOffer {
    /// True when this frame offers the pet nothing — the common case, and the
    /// one-branch early-out a receiver keys on.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.perk_at.is_none() && self.catch.is_none() && self.mote_t.is_none()
    }
}

// ===========================================================================
// 6. The one call the host makes
// ===========================================================================

/// THE LAUNCH IDENTITY, threaded through the router untouched.
///
/// **Identity is a hard law**: the kitty this process launched with never
/// changes mid-process except by a deliberate favourite pin. Never re-roll a
/// look — not on a style reparse, not on a meteor, not on a resize, not on a
/// seat that had to yield. The router therefore carries the look and never
/// *derives* one: there is no code path in this file that constructs a
/// [`KittyLook`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The look this process launched with, or the one a favourite pin
    /// replaced it with.
    pub look: KittyLook,
    /// True when [`Self::look`] came from a deliberate favourite pin rather
    /// than the launch roll — the ONE sanctioned way it may differ from the
    /// launch value.
    pub pinned: bool,
}

/// One [`duty`] call's inputs — the whole of what the host knows this frame.
/// No `Debug`, for [`SeatQuery`]'s reason.
#[derive(Clone, Copy)]
pub struct DutyQuery {
    /// The host's already-reached companion verdicts.
    pub admitted: CompanionAdmission,
    /// This frame's impulse, if [`super::Engine`] minted one.
    pub impulse: Option<CompanionImpulse>,
    /// The flying head's shipped footprint and geometry, or `None` when the
    /// host has none this frame (`kitty_cursor_footprint` returned `None`:
    /// kitty disabled, or a grid narrower than the cat) — then no seat is
    /// minted and the head is not drawn, exactly as today. Unread for the
    /// pet, which seats itself.
    pub seat: Option<SeatQuery>,
    /// The launch identity, passed through.
    pub identity: Identity,
    /// The frame's present time (T7).
    pub now: Instant,
}

/// EVERYTHING THE HOST NEEDS, in one value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Duty {
    /// Which body to draw (L-A: exactly one, possibly none).
    pub body: Body,
    /// What this frame's impulse means to that body.
    pub impulse: BodyImpulse,
    /// The FLYING head's seat, or `None` — for the pet (it seats itself,
    /// §7.2(b)), for no body, and for a frame with no footprint. A seat for a
    /// body nobody draws is a number the host would have to remember to
    /// ignore. On a [`BodyImpulse::Fly`] frame this is the snap-to
    /// ([`Flight::land`]).
    pub seat: Option<Seat>,
    /// The launch identity, unchanged.
    pub identity: Identity,
    /// m1 heroes this frame's impulse earned (§5.8) — 0 or 1, counted per
    /// IMPULSE and therefore never twice.
    pub heroes: u8,
    /// v2 is still computing something for this impulse at
    /// [`DutyQuery::now`]. The body's animator ORs its own answer in; this is
    /// v2's half only (see [`BodyImpulse::deadline`]).
    pub needs_frames: bool,
    /// The instant v2's own half of the cadence can stop, or `None`.
    pub deadline: Option<Instant>,
}

/// **THE SEAM'S ONE CALL** — body, impulse, seat, identity and cadence, from
/// one query.
///
/// Deliberately a free function over plain values with no state of its own, so
/// that both host render arms — the single-pane present and the composed one
/// — can call it with their own geometry and ink probe and draw whatever comes
/// back (the KNOWN GAP in this module's header). Today neither arm does.
///
/// The order is fixed and each step depends on the last: the body first
/// (nothing else is meaningful without it), then the impulse for that body,
/// then the seat — computed only for the flying head, so an ink-probing loop
/// is never paid for the pet or for a frame that draws no cat.
#[must_use]
pub fn duty<F>(query: &DutyQuery, ink: F) -> Duty
where
    F: Fn(u16, u16) -> Option<bool>,
{
    let body = body_for(query.admitted);
    let impulse = query
        .impulse
        .map_or(BodyImpulse::Ignore, |imp| impulse_for(body, imp));
    let seat = match (body, query.seat) {
        (Body::Flying { cell }, Some(q)) => Some(placement(cell, &q, ink)),
        _ => None,
    };
    let deadline = impulse.deadline();
    Duty {
        body,
        impulse,
        seat,
        identity: query.identity,
        heroes: query.impulse.map_or(0, heroes_earned),
        needs_frames: deadline.is_some_and(|at| query.now < at),
        deadline,
    }
}

/// `at + ms`, saturating rather than panicking on an absurd offset. The one
/// place milliseconds become an `Instant` in this module, so nobody rounds on
/// the way and lands a frame early (§8.1's reasoning for [`timing::flight`]).
fn add_ms(at: Instant, ms: f32) -> Instant {
    at.checked_add(std::time::Duration::from_secs_f32(ms / 1000.0))
        .unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::{cursor_companion_duty, flying_kitty_admitted, pet_companion_admitted};
    use crate::cursor_glow::GlowStyle;
    use crate::kitty_pet::{PetBrain, PetFrame};
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

    fn geom() -> Geom {
        geom_cols(120)
    }

    fn config() -> super::super::Config {
        super::super::Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            theme_fg: 0x00E8_E8F0,
            theme_bg: 0x0016_161C,
            reduced_motion: false,
        }
    }

    /// THE SHIPPED SEAT, mirrored as a fixture: `kitty_cursor_footprint`'s
    /// ¾-cell lead, right-margin clamp and boundary rise
    /// (`word_decorations.rs:3415-3439`), bob 0.
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

    fn seat_query(caret: (u16, u16), cols: usize) -> SeatQuery {
        SeatQuery {
            geom: geom_cols(cols),
            rest: footprint(caret, u16::try_from(cols).expect("fixture cols")),
            lead_px: LEAD_PX,
        }
    }

    fn identity() -> Identity {
        Identity {
            look: KittyLook::default(),
            pinned: false,
        }
    }

    /// The host's custody chain for a raw style string with the head fully
    /// lit and the pet fully faded in: `style_names_any_pet` →
    /// `pet_companion_admitted` / `flying_kitty_admitted` →
    /// `cursor_companion_duty`.
    fn host_duty(spelling: &str, sing: f32, caret: Option<(u16, u16)>) -> CompanionDuty {
        let pet_mode = GlowStyle::style_names_any_pet(spelling);
        let pet_on_glass = pet_companion_admitted(pet_mode, sing);
        let kitty_alpha = if flying_kitty_admitted(pet_mode, sing) {
            255
        } else {
            0
        };
        cursor_companion_duty(pet_on_glass, kitty_alpha, caret)
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

    /// THE SPELLING RULING (`cursor_glow.rs:205-225`), routed through the
    /// HOST's own custody chain: every spelling the pet predicate owns —
    /// `rainbow kitty` and the bare `kitty` included — resolves to the
    /// RESIDENT PET, which is the default install and the cat most users see.
    /// A change to that chain next door surfaces here.
    #[test]
    fn rainbow_kitty_and_bare_kitty_mean_the_pet() {
        for spelling in [
            "rainbow kitty",
            "kitty",
            "rainbow kitty pet",
            "kitty pet",
            "pet kitty",
            "rainbow kitty underline",
            "rainbow dog pet",
            "dog pet",
        ] {
            assert!(
                GlowStyle::style_names_any_pet(spelling),
                "{spelling:?} must name a pet upstream"
            );
            assert_eq!(
                body_for(admitted(host_duty(spelling, 0.0, Some((4, 9))))),
                Body::Pet,
                "{spelling:?} must draw the resident pet"
            );
        }
    }

    /// …and the flying head is reachable only through its OWN spelling (or the
    /// bare `nyan`/`rainbow` aliases, which name no geometry), or through a
    /// pet-mode sing-along, which is the singing face and not an ordinary
    /// flight — and it escorts exactly the cell the host's duty names.
    #[test]
    fn the_flying_head_needs_its_own_spelling() {
        for spelling in [
            "rainbow kitty flying",
            "flying kitty",
            "kitty flying",
            "nyan",
            "rainbow",
            "nyan rainbow",
        ] {
            assert!(
                !GlowStyle::style_names_any_pet(spelling),
                "{spelling:?} must not name a pet"
            );
            assert_eq!(
                body_for(admitted(host_duty(spelling, 0.0, Some((4, 9))))),
                Body::Flying { cell: (4, 9) },
                "{spelling:?} must draw the flying head at the caret"
            );
        }
        // The one way pet mode yields the head: the sing-along holds the frame.
        assert_eq!(
            body_for(admitted(host_duty("rainbow kitty", 0.4, Some((4, 9))))),
            Body::Flying { cell: (4, 9) }
        );
        assert_eq!(
            body_for(admitted(host_duty("rainbow kitty", 0.0, Some((4, 9))))),
            Body::Pet,
            "admission ends exactly when the drive drains"
        );
        // The head has no independent placement: no visible caret, no head.
        assert_eq!(
            body_for(admitted(host_duty("nyan", 0.0, None))),
            Body::None,
            "the host's duty is Idle without a caret, and the router agrees"
        );
    }

    /// L-A. The router names ONE body, the host's duty's, and a frame whose
    /// single body is already claimed draws nothing at all — the exclusivity
    /// `claim_companion_body` enforces, moved ahead of both animators.
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

    /// D13, both halves. The FLYING head takes the meteor whole; the PET is
    /// offered the arrival edge and NOTHING else — no teleport, no spine
    /// impulse, no retiming of the pounce, and no cadence held for a mailbox
    /// that does not exist. Open owner question 14 stays open.
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
        assert!(!flight.facing_left());
        assert!(flight.disp_floor() >= 0.97, "§7.2: disp = max(disp, 0.97)");
        assert!(!flight.landed(t0));
        assert!(flight.landed(land));

        // The pet: an OFFER of the same edge, carrying nothing else. The
        // variant has exactly one field, which is the law made structural.
        let pet = impulse_for(Body::Pet, imp);
        assert_eq!(
            pet,
            BodyImpulse::Perk { at: land },
            "the pet is offered t0 + T as its perk edge and nothing more"
        );
        assert!(
            !matches!(pet, BodyImpulse::Fly(_)),
            "a v2 meteor must never relocate the pet (open question 14)"
        );
        assert_eq!(
            pet.deadline(),
            None,
            "the offer is an edge: v2 computes nothing after minting it"
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

    /// §20.1's flying-head law, the ROUTER's half (the animator seams are
    /// named in WIRING STATUS): on the spawn frame the head's seat is resolved
    /// at the LANDING — the shipped footprint for that cell, byte-for-byte on
    /// blank glass — the impulse names that cell as the snap-to, `disp ≥ 0.97`
    /// and `lead == −0.30` at `t₀`, and `land_at == t₀ + T`.
    #[test]
    fn the_router_seats_the_flying_head_at_the_landing_on_the_spawn_frame() {
        let t0 = Instant::now();
        let t_flight = Duration::from_millis(90);
        let landing = (7, 80);
        let d = duty(
            &DutyQuery {
                admitted: admitted(CompanionDuty::FlyingHead { cell: landing }),
                impulse: Some(CompanionImpulse::Meteor {
                    dir: Dir::Right,
                    t0,
                    t_flight,
                }),
                seat: Some(seat_query(landing, 120)),
                identity: identity(),
                now: t0,
            },
            blank,
        );
        let BodyImpulse::Fly(flight) = d.impulse else {
            panic!("the head takes the meteor, got {:?}", d.impulse);
        };
        assert_eq!(flight.land, landing);
        assert!(flight.disp_floor() >= 0.97);
        assert!((flight.lead_at(t0) - (-0.30)).abs() < 1e-6);
        assert_eq!(flight.land_at, t0 + t_flight);
        let seat = d.seat.expect("the head has a seat on the spawn frame");
        let shipped = footprint(landing, 120);
        assert_eq!(
            (seat.x, seat.y, seat.w, seat.h),
            (shipped.x, shipped.y, shipped.w, shipped.h),
            "blank glass: the snap-to is the shipped seat at the landing"
        );
        assert_eq!(seat.row, landing.0);
        assert!(!seat.over_ink);
        assert!(d.needs_frames, "the whip is still being computed at t0");
    }

    /// L-D, and the measured occlusion defect. Whatever the caret column and
    /// whatever the ink, the resolved seat NEVER covers the cell the caret is
    /// typing into, never covers an inked cell it could have avoided, keeps
    /// the shipped seat byte-for-byte on blank glass wherever that seat
    /// itself clears the caret cell, yields BEHIND where the margin clamp
    /// makes it intrude, and carries ONE position.
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
            // Blank glass: the shipped footprint, byte-for-byte, at every
            // column where that footprint itself clears the caret cell — the
            // margin clamp and the boundary rise included. In the last
            // columns the clamp pushes the shipped body back OVER the caret
            // cell (the rise lifts it half a cell, not off the row), and
            // there L-D wins: behind, same row, the mirror's clearance.
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

        // The capture's own case: caret mid-line at column 5 with text ahead.
        // The shipped ¾-cell lead sat on columns 6..12 — the `cho t` of
        // `echo the`. Behind is off the grid, so v2 lifts into the sky.
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

    /// Steps 3 and 4 of the seat law: a fully inked row lifts the body into
    /// the sky; a fully inked sky draws OVER the words there; a top-row caret
    /// has no sky and stays on its own row without covering the caret cell;
    /// and a grid too narrow for either side falls back to the shipped
    /// footprint. The head never disappears, and it never draws "under".
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

        // A 10-column pane with a top-row caret at column 4: neither side of
        // the caret can hold a 6-cell body, so the shipped footprint — rise
        // and all — is today's answer and stays it.
        let narrow = placement((0, 4), &seat_query((0, 4), 10), all_ink);
        let shipped = footprint((0, 4), 10);
        assert_eq!((narrow.x, narrow.y), (shipped.x, shipped.y));
        assert!(narrow.over_ink);
    }

    /// An ink-blind host — every probe `None` — is NOT a crowded line: it gets
    /// the shipped seat byte-for-byte wherever that seat clears the caret
    /// cell, mid-line included, and never the sky or an over-ink verdict. But
    /// L-D's first half is arithmetic, not a probe verdict: at the right
    /// margin, where the shipped clamp pushes the body back OVER the caret
    /// cell, a blind host yields to the mirror seat exactly as a sighted one
    /// does; and where neither side clears the caret (a grid too narrow for
    /// the body on either side) the shipped footprint is still the answer.
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

        // The right margin. The fixture must actually intrude, or the arm is
        // not being exercised: the shipped seat at column 117 is clamped back
        // over cells 114..=119.
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

        // A 10-column pane, top-row caret at column 4: neither side of the
        // caret can hold the body, so the shipped footprint stays — a blind
        // host is never left without a seat, and it is never told the seat is
        // over ink it could not see.
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

        // …but ONE unknowable cell beside probeable ones still counts as inked.
        let mixed = placement((7, 5), &seat_query((7, 5), 120), |r, c| {
            (r != 7 || c != 8).then_some(false)
        });
        assert_ne!(mixed.row, 7, "a single unknowable cell in the span yields");
    }

    /// §5.8 / §7.2: a Delight earns EXACTLY ONE m1. The earn rides the impulse,
    /// not the body, so it cannot be counted twice by two bodies and it still
    /// happens on a frame whose body was vetoed (D5: light is never rationed).
    #[test]
    fn a_delight_earns_exactly_one_hero() {
        let t0 = Instant::now();
        assert_eq!(heroes_earned(CompanionImpulse::Delight), 1);
        assert_eq!(heroes_earned(CompanionImpulse::Land), 0);
        assert_eq!(heroes_earned(CompanionImpulse::Wince), 0);
        assert_eq!(
            heroes_earned(CompanionImpulse::Meteor {
                dir: Dir::Left,
                t0,
                t_flight: Duration::from_millis(90),
            }),
            0
        );

        let mut total = 0_u32;
        for adm in [
            admitted(CompanionDuty::Pet),
            admitted(CompanionDuty::FlyingHead { cell: (3, 10) }),
            admitted(CompanionDuty::Idle),
        ] {
            let d = duty(
                &DutyQuery {
                    admitted: adm,
                    impulse: Some(CompanionImpulse::Delight),
                    seat: Some(seat_query((3, 10), 120)),
                    identity: identity(),
                    now: t0,
                },
                blank,
            );
            assert_eq!(d.heroes, 1, "one hero per Delight, whatever the body");
            total += u32::from(d.heroes);
        }
        assert_eq!(total, 3, "one per frame — never one per body");
    }

    /// §7.2's whip: the head starts BEHIND its rest (yanked), overshoots on
    /// `spring-whip` (ζ 0.6, under 1 on purpose) and is settled ≈ 180 ms in.
    #[test]
    fn the_whip_starts_against_travel_and_springs_past_its_rest() {
        let t0 = Instant::now();
        let flight = Flight {
            dir: Dir::Right,
            t0,
            land_at: t0 + Duration::from_millis(100),
            land: (3, 40),
        };
        assert!(
            (flight.lead_at(t0) - FLYING_LEAD_0).abs() < 1e-6,
            "frame 0 is a full −0.30 cell of yank"
        );
        let mut peak = f32::MIN;
        for ms in 0..400 {
            peak = peak.max(flight.lead_at(t0 + Duration::from_millis(ms)));
        }
        assert!(
            peak > FLYING_LEAD_MAX,
            "spring-whip overshoots its rest (ζ < 1)"
        );
        // "Settled ≈ 180 ms" is the spec's own reading of `e^(−ζωt) = e^(−2.6)
        // ≈ 0.07`: a residual under a tenth of the whip's 0.52-cell travel.
        let settled = flight.lead_at(t0 + Duration::from_millis(180));
        let travel = FLYING_LEAD_MAX - FLYING_LEAD_0;
        assert!(
            (settled - FLYING_LEAD_MAX).abs() < 0.15 * travel,
            "settled by ≈180 ms, got {settled}"
        );
        // Facing is the existing `facing_left` bank and nothing else.
        let leftward = Flight {
            dir: Dir::Left,
            ..flight
        };
        assert!(leftward.facing_left());
        assert!(!flight.facing_left());
    }

    /// The pet's caret is the HOST's grid caret, never v2's licensed landing:
    /// with the two deliberately different, the pet gets the host's; a hidden
    /// caret is `None`, never a stale cell. The rest of the sense is v2's own
    /// geometry and live posture, and no spine rides along.
    #[test]
    fn the_pet_senses_the_grid_caret_not_v2s_licensed_landing() {
        let now = Instant::now();
        let cfg = super::super::Config {
            reduced_motion: true,
            ..config()
        };
        let ctx = Ctx {
            now,
            geom: geom(),
            cfg: &cfg,
            disp: 0.9,
            birth_disp: 0.9,
            phase: 12.0,
            caret: (7, 33),
            caret_t: 0.5,
            mend: None,
            surge: 0.0,
            flow: Default::default(),
        };
        let s = sense(
            &ctx,
            HostSense {
                caret: Some((2, 9)),
                wrapped: true,
                output_burst: true,
                pointer: Some((4.0, 5.0)),
            },
        );
        assert_eq!(s.now, now);
        assert_eq!(
            s.caret,
            Some((2, 9)),
            "the grid caret, not the last licensed landing (7, 33)"
        );
        assert!(s.wrapped && s.output_burst);
        assert_eq!((s.rows, s.cols, s.cell_w, s.cell_h), (40, 120, 9, 18));
        assert!(s.reduced_motion, "the live posture reaches the pet");
        assert_eq!(s.pointer, Some((4.0, 5.0)));

        let hidden = sense(&ctx, HostSense::default());
        assert_eq!(hidden.caret, None, "a hidden caret is None, never (7, 33)");
        assert!(!hidden.wrapped && !hidden.output_burst);
    }

    /// §20.1, the pet row: the pet's pose stream under v2 is byte-identical to
    /// v1's for the same move — including a meteor-sized jump and program
    /// motion v2 never licensed — because [`sense`] projects the same host
    /// facts v1 feeds it and nothing of v2's own. The v2 brain is fed through
    /// [`sense`] with `Ctx::caret` frozen at a stale licensed landing; the v1
    /// brain is fed the host facts directly. Every frame must agree.
    #[test]
    fn the_pet_keeps_its_own_pounce_under_a_v2_meteor() {
        fn key(
            f: &PetFrame,
        ) -> (
            u8,
            u8,
            crate::kitty_pet::PetAction,
            u32,
            u32,
            u32,
            bool,
            u32,
            u32,
            bool,
        ) {
            (
                f.alpha,
                f.lane_alpha,
                f.action,
                f.col.to_bits(),
                f.row.to_bits(),
                f.lift.to_bits(),
                f.facing_left,
                f.scale_x.to_bits(),
                f.scale_y.to_bits(),
                f.under_ink,
            )
        }
        let start = Instant::now();
        let cfg = config();
        let mut v1 = PetBrain::default();
        let mut v2 = PetBrain::default();
        // The host's caret walk, one entry per 16 ms frame: half a second at
        // (3, 5); a 12-key run; a 40-cell jump (a meteor by any measure); a
        // program-driven hop to another row that no licence gate would pass;
        // then a settle.
        let mut walk: Vec<Option<(u16, u16)>> = Vec::new();
        walk.extend(std::iter::repeat_n(Some((3, 5)), 30));
        for k in 0..12_u16 {
            walk.extend(std::iter::repeat_n(Some((3, 6 + k)), 3));
        }
        walk.extend(std::iter::repeat_n(Some((3, 58)), 40));
        walk.extend(std::iter::repeat_n(Some((9, 2)), 40));
        walk.extend(std::iter::repeat_n(None, 10));
        walk.extend(std::iter::repeat_n(Some((9, 2)), 60));

        let mut max_col = f32::MIN;
        let mut min_col = f32::MAX;
        for (i, &caret) in walk.iter().enumerate() {
            let now = start + Duration::from_millis(16 * i as u64);
            let a = v1.tick(PetSense {
                now,
                caret,
                wrapped: false,
                rows: 40,
                cols: 120,
                cell_w: 9,
                cell_h: 18,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
            });
            let ctx = Ctx {
                now,
                geom: geom(),
                cfg: &cfg,
                disp: 0.9,
                birth_disp: 0.9,
                phase: 3.0,
                // Frozen on purpose: the last landing the licence gate passed.
                caret: (3, 5),
                caret_t: 0.25,
                mend: None,
                surge: 0.0,
                flow: Default::default(),
            };
            let b = v2.tick(sense(
                &ctx,
                HostSense {
                    caret,
                    wrapped: false,
                    output_burst: false,
                    pointer: None,
                },
            ));
            assert_eq!(key(&a), key(&b), "frame {i}: the pet's pose stream forked");
            max_col = max_col.max(a.col);
            min_col = min_col.min(a.col);
        }
        assert!(
            max_col > 40.0 && min_col < 20.0,
            "fixture: the pet must actually chase the jump and the hop \
             (cols {min_col}..{max_col}) for the comparison to mean anything"
        );
    }

    /// The cadence contract: v2 arms the frame clock only for what v2 is still
    /// computing — the flight's whip, squint and look-back — never for a pose
    /// the animator owns, and never for the pet's offer. And the pet is never
    /// handed a seat.
    #[test]
    fn v2_holds_the_cadence_only_for_its_own_clocks() {
        let t0 = Instant::now();
        let flight = CompanionImpulse::Meteor {
            dir: Dir::Right,
            t0,
            t_flight: Duration::from_millis(100),
        };
        let mk = |adm, imp, now| {
            duty(
                &DutyQuery {
                    admitted: adm,
                    impulse: imp,
                    seat: Some(seat_query((3, 10), 120)),
                    identity: identity(),
                    now,
                },
                blank,
            )
        };
        let head = admitted(CompanionDuty::FlyingHead { cell: (3, 10) });
        let live = mk(head, Some(flight), t0);
        assert_eq!(live.body, Body::Flying { cell: (3, 10) });
        assert!(live.needs_frames, "the whip is still being computed");
        assert_eq!(
            live.deadline,
            Some(t0 + Duration::from_millis(350)),
            "flight 100 ms + the 250 ms look-back"
        );
        assert!(live.seat.is_some(), "the head always has a seat");

        let done = mk(head, Some(flight), t0 + Duration::from_millis(400));
        assert!(!done.needs_frames, "v2 is finished; the head keeps its own");

        let pose = mk(head, Some(CompanionImpulse::Wince), t0);
        assert_eq!(pose.deadline, None, "a reaction is an edge, not a clock");
        assert!(!pose.needs_frames);

        let quiet = mk(head, None, t0);
        assert_eq!(quiet.impulse, BodyImpulse::Ignore);
        assert_eq!(quiet.heroes, 0);
        assert!(!quiet.needs_frames);

        // The pet: offered the edge, no cadence for it, and NO seat.
        let pet = mk(admitted(CompanionDuty::Pet), Some(flight), t0);
        assert_eq!(pet.body, Body::Pet);
        assert!(matches!(pet.impulse, BodyImpulse::Perk { .. }));
        assert!(!pet.needs_frames, "an offer holds no cadence");
        assert!(pet.seat.is_none(), "the pet seats itself (§7.2(b))");

        // No body ⇒ no seat, so a host can never draw a cat it was not given.
        let none = mk(
            CompanionAdmission {
                body_claimed: true,
                ..head
            },
            Some(flight),
            t0,
        );
        assert_eq!(none.body, Body::None);
        assert!(none.seat.is_none());
        assert_eq!(none.impulse, BodyImpulse::Ignore);

        // No footprint ⇒ no seat, even for the head: the host has nothing to
        // draw this frame (`kitty_cursor_footprint` returned `None`).
        let unseated = duty(
            &DutyQuery {
                admitted: head,
                impulse: None,
                seat: None,
                identity: identity(),
                now: t0,
            },
            blank,
        );
        assert!(unseated.seat.is_none());
    }
}
