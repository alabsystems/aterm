// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **RAINBOW KITTY v2** — the rebuilt `rainbow kitty` cursor theme: pixels and
//! sound as one design.
//!
//! Design of record: `docs`' `RAINBOW-KITTY-V2.md`. Section references
//! throughout this module tree are to that document, which supersedes the
//! separate visual and audio drafts and resolves every contradiction between
//! them in its §1 rulings (D1-D20).
//!
//! > Typing lays the rainbow and a few real stars in the sky above it, one warm
//! > tine per key on a verse that advances on the clock instead of the
//! > keystroke; a long jump is a **meteor** with true anatomy — white-hot head
//! > already a quarter of the way across on the frame the caret lands, spectrum
//! > train that dies white-first, sputtering fragments, terminal flash, a pin
//! > that closes into the caret — and one gesture of sound on the same clock,
//! > tick, gliding core, whoosh, bell exactly on the landing frame, five glints
//! > raining behind it; everything is born at full brightness on the frame the
//! > caret is observed, and everything is off the glass and out of the air
//! > before your next thought.
//!
//! ## The seam (§17.2, D14)
//!
//! `CursorGlow` stays the seam OBJECT — the host reads it at 61 call sites —
//! and the delegation is **twelve enumerated points**, each one
//! `if self.v2.engaged()`. That enumeration is what makes "the other nine
//! styles are byte-identical" a checkable claim rather than a hope. Every
//! public item on [`Engine`] below exists to serve exactly one of those
//! points, and the mapping is written on each item:
//!
//! | # | `CursorGlow` seam | v2 |
//! |---|---|---|
//! | 1 | `spawn`, after the licence gate and `classify_move` | [`Event::Move`] |
//! | 2 | `note_typed` / `note_erase` / `note_kill` / `note_return` | [`Engine::on_event`] |
//! | 3 | the emit phase | [`Engine::tick`] |
//! | 4 | `rainbow_field` | [`Engine::field`] / [`Engine::field_at`] |
//! | 5 | `rainbow_head_rgb` | [`Engine::head_rgb`] |
//! | 6 | `momentum_display` | [`Engine::momentum_display`] |
//! | 7 | `rainbow_phase` | [`Engine::phase`] |
//! | 8 | `needs_frame_cadence` | [`Engine::needs_frame_cadence`] |
//! | 9 | `next_change_deadline` | [`Engine::next_change_deadline`] |
//! | 10 | `take_cursor_cat_motion_pulse` | [`Engine::take_companion_impulse`] |
//! | 11 | `drain_sound_cues` / `take_key_cue` | [`Frame::cues`] / [`Engine::drain_sound_cues`] |
//! | 12 | `reset` / `translate_scroll_state` / `trail_status` | [`Engine::reset`] / [`Engine::translate_scroll`] / [`Engine::status`] |
//!
//! `is_active` follows the v2 fingerprint when engaged. The caret block stays
//! where it is ticked today; v2 only supplies [`CaretSeam`] (§7.1).
//!
//! **Where the host side stands.** The twelve points are the CONTRACT the
//! next stage wires. As of this build no `CursorGlow` code constructs an
//! [`Engine`]: every "the host copies / drains / reads" below states what
//! that wiring must do, not wiring that exists, and the engine-level tests at
//! the bottom of this file are the only caller of the seam.
//!
//! ## Module layout (§17.1)
//!
//! * [`timing`] — the shared numbers: [`timing::flight_ms`], the `FLIGHT_*`
//!   constants, [`timing::shed_n`], [`timing::FAN_HERO_N`], the `GLINT_*`
//!   bucket constants, the seven named curves, the two coverage ceilings.
//! * [`spine`] — ONE momentum integrator and ONE display follower, replacing
//!   the five v1 shipped (§19.1).
//! * [`ribbon`] — the kept ribbon, the per-row `col → t` field index, the hot
//!   edge.
//! * [`stardust`] — the sky: magnitude classes, birth zones, the token bucket.
//! * [`meteor`] — the flight, its own arc, and the landing.
//! * palette — `crate::spectrum`, unchanged. C1: the seven-stop ROYGBIV is the
//!   ONLY colour law.
//!
//! ## The invariants this module tree is written to keep
//!
//! * **T1 — no keystroke, no light.** Geometry is born only on an observed,
//!   licensed cursor move. A keydown never pre-draws.
//! * **T2 — the frame-0 law.** Everything an event owns exists, at full
//!   brightness, on the frame the caret is first observed at its landing.
//! * **T6 — idle → zero.** [`Engine::needs_frame_cadence`] is `false` and
//!   [`Engine::next_change_deadline`] is `None` when no star, meteor, pin,
//!   ring or ribbon transient is live, and the fingerprint is `0` when nothing
//!   is on glass.
//! * **T7 — wall-clock, not frame-count.** Every curve takes `now`.
//! * **Determinism.** Every seed is hashed from `(row, col, born)` at the
//!   spawn edge; there is no per-frame RNG; a frame at the same `now` is
//!   byte-identical on CPU and GPU (§18).
//! * **No allocation on the steady-state frame path.** Fixed pools — 40 stars,
//!   2 meteors, 2 landings, a reused station scratch — and resident `Vec`s
//!   that are cleared, never rebuilt.
//! * **No `unsafe`.** Anywhere in this tree.

pub mod companion;
pub mod meteor;
pub mod ribbon;
pub mod spine;
pub mod stardust;
pub mod timing;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{BeamVertex, GlowQuad, RainHalo};

use crate::cursor_glow::{Geom, GlowConfig, SoundCue};
use crate::trail_sound::SoundKind;
use meteor::{Meteors, Spawn};
use ribbon::Ribbon;
use spine::Spine;
use stardust::{GlyphProbe, StarBudget, Stardust};
use timing::{JUMP_MIN_CELLS, SPRING_SNAP_RESPONSE_S, half_life};

/// Life of the caret's LANDING RE-LIGHT ([`Engine::caret_paint`]): a hold of
/// one [`SPRING_SNAP_RESPONSE_S`] — the flare's own relax, so the re-light
/// starts cooling on the frame the flare has settled — then eight half-lives
/// on the same constant, `2⁻⁸ = 1/256 < 1/255`, under the block fill's u8
/// resolution, so the snap to EXACTLY zero (T6) can never be seen as a step.
/// Engine-private: no other producer reads it, so `timing` (one file for the
/// numbers two producers share) is not where it belongs.
const RELIGHT_LIFE_S: f32 = SPRING_SNAP_RESPONSE_S + 8.0 * SPRING_SNAP_RESPONSE_S;

// ===========================================================================
// The event vocabulary
// ===========================================================================

/// TRAVEL DIRECTION, resolved to the DOMINANT AXIS — the same rule D15 uses to
/// pick a rasterizer, so the kitty's facing, the cue's `dir` and the train's
/// beam can never disagree about which way a gesture went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// `|dx| ≥ |dy|`, `dx > 0`.
    Right,
    /// `|dx| ≥ |dy|`, `dx ≤ 0`.
    Left,
    /// `|dy| > |dx|`, `dy < 0`.
    Up,
    /// `|dy| > |dx|`, `dy > 0`.
    Down,
}

impl Dir {
    /// The direction of a cell-space delta, by the dominant axis (D15's rule).
    #[must_use]
    pub fn of(dcol: i32, drow: i32) -> Self {
        if dcol.abs() >= drow.abs() {
            if dcol > 0 { Self::Right } else { Self::Left }
        } else if drow < 0 {
            Self::Up
        } else {
            Self::Down
        }
    }

    /// The `SoundCue::dir` sign: `+1` for a rightward / upward move, `−1` for
    /// left / down — the host's existing convention, unchanged.
    #[must_use]
    pub fn sign(self) -> i8 {
        match self {
            Self::Right | Self::Up => 1,
            Self::Left | Self::Down => -1,
        }
    }

    /// True where D15 sends the flight to `ribbon_beam_v`.
    #[must_use]
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// WHY a move was admitted — the verdict the style-agnostic licence gate
/// (`CursorGlow::move_licensed`) and `classify_move` already reached before
/// seam point 1 hands the move here.
///
/// v2 never re-derives a licence; it is told one. T1 lives entirely upstream
/// of this enum, which is what keeps "no keystroke, no light" a property of
/// the seam rather than of every producer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Licence {
    /// A fresh typed-glyph hint licensed the move (`type_hint`).
    Typed,
    /// A navigation hint licensed it — arrows, clicks, history recall
    /// (`nav_hint`).
    Nav,
    /// A cold Enter licensed by the return arm (`return_licensed`).
    Return,
    /// A scripted preview / example / benchmark (`note_synthetic_move`).
    Synthetic,
    /// **NO CREDIT** — program-driven motion under an arm that admits it, such
    /// as a PTY line-feed cascade (§8.2's "PTY line feed (no credit)", D18).
    /// Draws its meteor under the 2-cap and the retire law; its AUDIO rides
    /// the cascade lane's 1-voice / 180 ms exclusivity.
    Pty,
}

/// What kind of glyph a [`Event::Typed`] carried — the whole of what the glow
/// needs in order to deal a star (§5.8's earned-hero set).
///
/// D7 is why this enum is short: heroes are **capitals, `!`, kitty Delight,
/// the landing fan's own m1, and the 1-in-12 deal**. "Chord-tone verse crests"
/// were dropped because the glow cannot see `walk` — the seam runs glow →
/// synth, not the other way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedClass {
    /// An ordinary printable glyph.
    Glyph,
    /// A shifted capital — earns an m1 at the glyph's cell (§5.8) and, at the
    /// synth, an octave echo at +25 ms, −8 dB (§8.2).
    Capital,
    /// `!` — earns an m1 (§5.8).
    Bang,
    /// Space. Lays ribbon but deals no star; the synth's rest + bass dyad +
    /// breath (§8.2, §9.3).
    Space,
}

/// The SCALE of a kill chord — a word going, or a clause going.
///
/// Carried on [`Event::Kill`] because §5.6 prices the erase population per 3
/// cells of the kill (cap 8) and §8.2 gives the two scales different sounds
/// (`KillWord`'s poof vs `Kill`'s swoosh). The spine drains identically for
/// both (§19.1: erase weight is a counter, not a sixth integrator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillScope {
    /// `^W`, Alt-D, Alt/Ctrl-Backspace — one word.
    Word,
    /// `^U`, `^K` — a whole clause.
    Line,
}

/// ONE ENGINE EVENT (§17.1). Everything the host tells v2, in one enum.
///
/// `Copy`, because events are buffered on a resident queue and replayed to the
/// producers twice per tick (once to ingest, once to deal births against the
/// ribbon's freshly planned bands) with no allocation either time.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A typed glyph echoed forward. `cells` is how many cells the echo
    /// advanced (a coalesced burst may carry several); `shifted` rides through
    /// to `SoundCue::shifted`; `class` decides the hero deal.
    ///
    /// Seam point 2 (`note_typed`). The ONLY event that builds momentum.
    Typed {
        /// Cells the echo advanced.
        cells: u16,
        /// The key carried Shift — a capital or a shifted symbol.
        shifted: bool,
        /// What kind of glyph it was (§5.8).
        class: TypedClass,
    },
    /// One Backspace. Seam point 2 (`note_erase`). Drains the spine, retracts
    /// a ribbon cell, throws 3 m3 + 1 m2, and keeps v1's smoke poof
    /// byte-unchanged (§5.6: the poof is v1-owned vapour, explicitly not cut).
    /// **Unpitched** — no erase chimes, ruled twice (§19.2).
    Erase,
    /// A kill chord. Seam point 2 (`note_kill`). The ribbon drains
    /// farthest-first at `12·n + 240` ms; 1 star per 3 cells, cap 8.
    Kill {
        /// Cells erased.
        cells: u16,
        /// Word or line scale (§8.2).
        scope: KillScope,
    },
    /// An observed, LICENSED cursor move, handed over AFTER the licence gate
    /// and `classify_move` have run (seam point 1). This is the only event
    /// that can mint a meteor, and T1 is exactly the statement that geometry
    /// is born here and nowhere else.
    Move {
        /// `(row, col)` the caret left.
        from: (u16, u16),
        /// `(row, col)` the caret was observed at — the LANDING, and `t = 0`.
        to: (u16, u16),
        /// Why the move was admitted.
        licence: Licence,
        /// Travel direction, dominant axis.
        dir: Dir,
    },
    /// A keyed Enter — **the KEY only**, seam point 2 (`note_return`). It is
    /// inert in every producer, on purpose (T1: a keypress is not a move —
    /// minting a flight here would draw a meteor for an Enter the shell
    /// swallowed). The Enter's FLIGHT arrives as the caret's own observed
    /// motion, an [`Event::Move`] carrying [`Licence::Return`]: the meteor
    /// anatomy along the true vector with `T = flight_ms(cells)`, a landing
    /// sized by ROWS (5-7 m3 + 1 m2, reach 1.6 `ch`, **no ring, no hero, no
    /// rain**, D8), the pin (it is the caret), and the cadence's
    /// [`SoundKind::Enter`] cue.
    Return,
    /// Window focus changed. On loss the ribbon embers out in 300 ms on
    /// `spend` and the stars die; nothing sounds (§8.2).
    Focus(bool),
    /// The host's reduced-motion posture changed. Under it every mark is
    /// static and takes the theme's ONE linear fade,
    /// [`timing::REDUCED_MOTION_FADE_MS`] (§2.5, §6.11).
    ReducedMotion(bool),
    /// **A TYPED ECHO'S GLYPH CELLS** (seam point 1, 2026-09-06): the
    /// classifier licensed the same-row echo `col0 → col1` on `row` as
    /// TYPING — one key's glyph, or several the PTY delivered in one frame
    /// on banked press credits. A `Typed` key lays at the caret the tick
    /// replays with, so it owns its glyph cell only when its echo shares
    /// its tick; a batched echo's earlier cells and the glyph of a key whose
    /// echo landed a frame late stayed dark for good — the hole v1's
    /// coalescer swept and v2 did not. The ribbon lays the cells of
    /// `col0..col1` that no live cell owns, as typing, born at the echo;
    /// the keys' own cells are untouched. Nothing else reads it: no spine
    /// advance (the keys did that), no star, no meteor, no cue.
    Sweep {
        /// The echoed row.
        row: u16,
        /// The first glyph cell of the echo.
        col0: u16,
        /// The landing (exclusive): the caret the echo left the row at.
        col1: u16,
    },
}

// ===========================================================================
// Config, Frame, Ctx
// ===========================================================================

/// Everything v2 needs from the host's `GlowConfig`, and nothing else.
///
/// A narrow struct on purpose: `GlowConfig` carries 20-odd fields for ten
/// styles, and a v2 producer that could read `style`, `pack` or `beam` would
/// be one refactor away from branching on another style's state — which is the
/// property "the other nine styles are byte-identical" depends on.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Dark ground. The whole light fork (§3.3, §6.10, §5.2's light twins)
    /// hangs off this ONE operator flip: additive light becomes source-over
    /// ink, and "bigger" is bought as AREA, never as brightness (L6).
    pub dark_theme: bool,
    /// The host's effect intensity, 0..1 — a coverage multiplier, applied
    /// before every cap, never after.
    pub intensity: f32,
    /// The host's trail duration knob — scales the ribbon's cell lives. It
    /// does NOT scale [`timing::flight_ms`]: the meteor's clock is a shared
    /// coupling number, not a taste dial.
    pub duration: Duration,
    /// The shipped `tall` ribbon spelling (`RAINBOW_TALL_UP 1.10 ch`, spine at
    /// the row bottom) vs `underline`. D16: this is why star birth zones are
    /// defined against the ribbon's TOP EDGE and not in cell coordinates.
    pub ribbon_tall: bool,
    /// Theme foreground — the INK colour of the light fork and the ledger's
    /// `rainbow_ink_lift_max(fg)` input (L2).
    pub theme_fg: u32,
    /// Theme background.
    pub theme_bg: u32,
    /// Reduced motion (§6.11). Mirrored from [`Event::ReducedMotion`] so a
    /// producer reading `cfg` alone still sees it.
    pub reduced_motion: bool,
}

impl Config {
    /// Project the host's `GlowConfig` onto v2's needs. The one place the two
    /// vocabularies meet.
    #[must_use]
    pub fn from_glow(cfg: &GlowConfig, reduced_motion: bool) -> Self {
        Self {
            dark_theme: cfg.dark_theme,
            intensity: cfg.intensity,
            duration: cfg.duration,
            ribbon_tall: cfg.ribbon_tall,
            theme_fg: cfg.theme_fg,
            theme_bg: cfg.theme_bg,
            reduced_motion,
        }
    }
}

/// THE CARET SEAM (§7.1). v2 does **not** own the caret block fill; the caret
/// keeps being ticked where it is ticked today (`tick_with_family_phase`,
/// after the ledger). What v2 supplies is this record, which the host copies
/// into a new `RainbowConfig.flare_at` field.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaretSeam {
    /// The instant a meteor's frame-0 flare fired, if one did. The block fills
    /// `#FFFFFF` for exactly ONE frame, then mixes toward its field stop on
    /// `spring-snap` ([`timing::spring_snap`], response 0.18 s); the four halo
    /// rings pop radius ×1.4 on the same edge and relax τ 120 ms.
    ///
    /// Set only by a CREDITED spawn — never a [`Licence::Pty`] line feed
    /// (§8.2: no credit, no flare on program output) — and never under
    /// reduced motion (§6.11: no flash). The engine CLEARS it once the relax
    /// has settled ([`timing::SPRING_SNAP_RESPONSE_S`] after the edge), so a
    /// `Some` is always a live relax; the host ages the one white frame and
    /// the mix from this instant.
    pub flare_at: Option<Instant>,
    /// The caret's position in the field (C2) — the ribbon head, the caret and
    /// a star's halo on that cell are the SAME colour on the SAME frame
    /// because they all read this number, and it is THIS frame's: sampled
    /// after the frame's events are laid and the ribbon has planned, so it
    /// equals [`Engine::field`] on the frame it rides out with. It is the RAW
    /// walk `t` — unbounded past `1.0` on a long line — so a host folds it
    /// through [`meteor::tri`] before `spectrum`, as the ribbon and the meteor
    /// do (D4: reflected, never clamped).
    pub field_t: f32,
    /// The caret's paint level, 0..1 — **the display spine**,
    /// [`Engine::momentum_display`] (§17.1 lists caret paint among the
    /// follower's readers), floored by the LANDING RE-LIGHT: a credited spawn
    /// lifts it to exactly `1.0` on the landing frame, holds it there through
    /// the flare's relax and cools it on [`timing::half_life`] with the flare's
    /// own constant (`RELIGHT_LIFE_S`), because **the caret is never dimmed
    /// at the destination** — a dim caret reads as "not arrived" (§7.1).
    ///
    /// The host copies this into `RainbowConfig::paint`, whose contract is
    /// exactly this number: "the ribbon's own display spine … the cadence
    /// keeps the fast ATTACK and the ribbon owns the RELEASE", consumed as
    /// `max(paint, energy)`. §7.1's "cool: τ 220 ms after typing stops" is
    /// that `energy` — the host cadence's own 220 ms ignition heat — and v2
    /// does not run a second 220 ms clock beside it: a paint that cooled on
    /// its own 220 ms would collapse 4-8× faster than the ribbon's release
    /// (τ 0.85 s), which is the measured caret defect `cursor_rainbow`
    /// records. [`Engine::caret_paint`] is the same number as a pure read,
    /// for the host's own caret ticker at its own `now`.
    pub paint: f32,
}

/// ONE IMPULSE, TWO BODIES (D13, §7.2). v2 emits a single impulse and
/// whichever body `CompanionDuty` says owns the frame consumes it. Identity
/// never changes; no new sprite frames; no facing flip beyond the existing
/// `facing_left` bank.
#[derive(Clone, Copy, Debug)]
pub enum CompanionImpulse {
    /// A meteor was born. The **flying head** takes the frame-0 teleport
    /// (placement snaps, no glide), `disp = max(disp, 0.97)` directly, a
    /// `lead = −0.30` cell whip springing to `+0.22` on
    /// [`timing::spring_whip`], and `land_at = t0 + t_flight`. The
    /// **resident pet** is only OFFERED this: its owner-ruled pounce
    /// choreography is untouched and it may take `t0 + t_flight` as its perk
    /// edge. Whether the pet should teleport was A/B #14 and is RULED — the
    /// owner, 2026-09-05: "no teleport"; the pet keeps its own pounce.
    Meteor {
        /// Flight direction — the existing `facing_left` bank.
        dir: Dir,
        /// The spawn edge.
        t0: Instant,
        /// §6.2's `T`, so the squash lands with the pin and the bell (§6.7).
        t_flight: Duration,
    },
    /// A landing with no flight behind it: a credited spawn under REDUCED
    /// MOTION (§6.11 — the head is at the landing, there was no flight to
    /// follow). Both bodies take the existing landing pose as an edge; the
    /// animator applies its own reduced-motion posture to it, because v2
    /// owns no sprite.
    Land,
    /// An erase or a kill — the existing "oops" pose (§8.2's Backspace and
    /// `^W`/`^U`/`^K` rows). Minted on the event edge; a meteor minted on
    /// the same frame outranks it.
    Wince,
    /// The existing Delight pose, minted when the host reports the kitty's
    /// Delight edge through [`Engine::earn_hero`]. Additionally earns one m1
    /// (§5.8, §7.2) — the count is [`companion::heroes_earned`]'s.
    Delight,
}

/// ONE TICK'S OUTPUT (§17.1).
///
/// The three pixel streams and the cue sink are **borrowed** host scratch, not
/// owned vectors: seam point 3 says v2 writes into the same `under` / `out` /
/// `halos` the other nine styles use, and reusing the host's allocations is
/// what makes §18's "zero allocation per frame" true rather than aspirational.
/// The small outputs — [`Frame::caret`], [`Frame::companion`], [`Frame::fp`] —
/// are owned, because they are values, not buffers.
pub struct Frame<'a> {
    /// `glow_under`: composited BENEATH glyph ink, never blooms. Carries the
    /// ribbon body and the meteor's colour train, so "where I came from" reads
    /// beneath the letters without touching them (§6.5).
    pub under: &'a mut Vec<GlowQuad>,
    /// `cursor_glow_add`: above ink, ledger-held over glyph cells, blooms on
    /// GPU. Carries the meteor's white heat and shoulder, the pin, the ring,
    /// the fan, the stars and the hot edge.
    pub out: &'a mut Vec<GlowQuad>,
    /// `glow_halo`: the radial-falloff stream. The meteor's coma and nucleus,
    /// and the stars' halos. §18 caps a dark frame at ≤ 24 of `MAX_HALOS 512`
    /// and a light frame at ≤ 400.
    pub halos: &'a mut Vec<RainHalo>,
    /// Reused polyline scratch for `aterm_render::comet_beam` (the hot edge,
    /// the ring). Held by the host so no producer allocates one per frame.
    pub beams: &'a mut Vec<BeamVertex>,
    /// The frame's sound cues, appended in mint order (seam point 11). The
    /// host drains this exactly as it drains `CursorGlow`'s today.
    pub cues: &'a mut Vec<SoundCue>,
    /// What the host must copy into `RainbowConfig` for the caret (§7.1).
    pub caret: CaretSeam,
    /// The impulse minted on this frame, if any (§7.2). A REPORT: the same
    /// impulse stays pending until [`Engine::take_companion_impulse`] consumes
    /// it, so a host may read it here or take it there, but only the take
    /// transfers ownership.
    pub companion: Option<CompanionImpulse>,
    /// The frame fingerprint. **Exactly `0` when nothing is on glass** (T6) —
    /// that is the whole contract, and `is_active` follows it when engaged.
    pub fp: u64,
}

/// The per-tick read-only context handed to every producer.
///
/// One struct rather than eight arguments, so a producer's signature does not
/// change every time the frame gains a shared number — and so that everything
/// born on one frame is priced by ONE evaluation of the spine (§17.1: the
/// spine is read by ribbon width, star counts, meteor width, caret paint and
/// the kitty bank; if they read it at four different instants they are four
/// integrators again).
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// The frame's present time. Every curve in the theme takes it (T7).
    pub now: Instant,
    /// Window-space geometry, in px. All effect emissions are
    /// WINDOW-ABSOLUTE.
    pub geom: Geom,
    /// The host config, projected (see [`Config`]).
    pub cfg: &'a Config,
    /// [`spine::Spine::disp`] — the honest eased spine every "earned drama"
    /// consumer reads.
    pub disp: f32,
    /// [`spine::Spine::birth_disp`] — the resume-floored spine the BIRTH laws
    /// read, and only they.
    pub birth_disp: f32,
    /// [`spine::Spine::phase`] — the lay/flow clock.
    pub phase: f32,
    /// The caret's `(row, col)` as last observed.
    pub caret: (u16, u16),
    /// The caret's own field `t` — §6.4's `t_land`, the meteor's phase lock,
    /// and C2's shared colour.
    pub caret_t: f32,
}

/// `trail status`'s v2 rows (seam point 12).
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    /// `v2_quads=` — quads written this frame across `under` + `out`.
    pub quads: u32,
    /// `v2_halos=` — halos written this frame.
    pub halos: u32,
    /// `v2_stars=` — live stars.
    pub stars: u32,
    /// Live meteors (≤ [`timing::FLIGHT_MAX_LIVE`]).
    pub meteors: u32,
    /// Live ribbon cells.
    pub cells: u32,
    /// The eased spine.
    pub disp: f32,
    /// Glint tokens on hand.
    pub tokens: f32,
    /// The last frame's fingerprint.
    pub fp: u64,
}

// ===========================================================================
// Cadence — what a producer may ask the host for (§18, T6)
// ===========================================================================

/// **THE FRAME CADENCE** — what a producer asks for while BRISK light is
/// live: something on glass MOVES every frame (a head in flight, a thrown
/// star, a mark retracting into the caret, the ribbon head cell's 18 ms
/// `edge-in`). The design's reference tick (§18: "per tick at 120 Hz"); the
/// host substitutes its own present interval through seam point 8
/// ([`Engine::needs_frame_cadence`]) and phase-locks its own train, so this
/// number is what the engine-level laws and the bench are measured against,
/// never a rate the host is asked to run at.
pub const FRAME_CADENCE: Duration = Duration::from_micros(8_333);

/// **THE TAIL FLOOR** — the soonest a producer may ask to be woken while
/// only TAILS are live: a half-life fade, the swoosh's grace and fade, a
/// star past its hold, the spine settling under a laid ribbon. 45 ms is
/// ~22 Hz: the present gate dedups the identical frames between visible
/// steps, and a tail sampled faster buys recompose cost and nothing on
/// glass. A live twinkling star keeps the sky from going COARSER than this
/// floor plus one arm step (≤ 62 ms, §5.5) — the twinkle is a visible
/// change and is kept; it is not a reason to run at frame rate.
pub const TAIL_FLOOR: Duration = Duration::from_millis(45);

/// **NO BUSY RE-ARM** — an engine deadline is never sooner than this after
/// `now`. A producer that answered "now" (the sky once did, for every live
/// star) had the host re-arm on the turn it was asked, which is a spin
/// dressed as a deadline: `deadline_arms_by_owner cursor_effect` read 2.25
/// arms per rendered frame against v1's 1.66 for the same presents.
pub const ARM_MIN: Duration = Duration::from_millis(1);

/// A producer's answer to *"when do you next change what is on glass?"*,
/// folded from three kinds of offer:
///
/// * [`Cadence::brisk`] — per-frame MOTION is live; the answer is the next
///   frame whatever else was offered.
/// * [`Cadence::edge`] — the exact instant a mark APPEARS or a motion
///   starts (a reach beat laying a cell, the retract's first frame, the
///   swoosh's end). Offered as is: these are sparse by construction and
///   the eye reads them as events.
/// * [`Cadence::tail`] / [`Cadence::tail_at`] — a fade whose next
///   VISIBLE step (the instant its u8 coverage changes by a level,
///   computed from the envelope) is `step_s` away, or unknown; and every
///   disappearance or sub-level regime change. Never sooner than
///   [`TAIL_FLOOR`], so tails from several producers cannot interleave
///   into a finer cadence than the floor.
///
/// One fold, so every producer's cadence law is written in the same three
/// words and the engine's `min` composes them without knowing which is
/// which.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cadence {
    now: Instant,
    best: Option<Instant>,
    brisk: bool,
}

impl Cadence {
    /// An empty fold at `now`.
    #[must_use]
    pub(crate) fn at(now: Instant) -> Self {
        Self {
            now,
            best: None,
            brisk: false,
        }
    }

    /// Per-frame motion is live.
    pub(crate) fn brisk(&mut self) {
        self.brisk = true;
    }

    /// An exact instant. Instants at or before `now` are not a next change
    /// and are ignored.
    pub(crate) fn edge(&mut self, at: Instant) {
        if at > self.now && self.best.is_none_or(|b| at < b) {
            self.best = Some(at);
        }
    }

    /// A fade whose next visible step is `step_s` away — `0.0` when it is
    /// not computed. Floored at [`TAIL_FLOOR`]; a non-finite step is the
    /// floor too.
    pub(crate) fn tail(&mut self, step_s: f32) {
        let at = if step_s.is_finite() && step_s > TAIL_FLOOR.as_secs_f32() {
            self.now + Duration::from_secs_f32(step_s)
        } else {
            self.now + TAIL_FLOOR
        };
        self.edge(at);
    }

    /// A tail whose next visible step is the instant `at` — a DISAPPEARANCE
    /// (a cull, an expiry, a finish's end) or a sub-level regime change (a
    /// hold ending, a one-pixel lift), which the eye does not read as an
    /// event and which may therefore land on the floor grid up to
    /// [`TAIL_FLOOR`] late. An instant already past is the floor.
    pub(crate) fn tail_at(&mut self, at: Instant) {
        self.tail(at.saturating_duration_since(self.now).as_secs_f32());
    }

    /// The fold: the next frame when brisk, else the earliest offer.
    #[must_use]
    pub(crate) fn take(self) -> Option<Instant> {
        if self.brisk {
            Some(self.now + FRAME_CADENCE)
        } else {
            self.best
        }
    }
}

/// Seconds until a `spend` fade (`(1 − u)²`, §2.5) next changes its u8
/// level, for a mark whose full coverage is `peak` levels and whose spend
/// has run `u` of its `span_s` seconds. `None` once the level is already
/// zero. Closed form: the level `L = round(peak·(1 − u)²)` drops when
/// `(1 − u')² = (L − 0.5) / peak`.
#[must_use]
pub(crate) fn level_step_spend(peak: f32, u: f32, span_s: f32) -> Option<f32> {
    if !(peak.is_finite() && span_s.is_finite() && u.is_finite()) || peak <= 0.0 || span_s <= 0.0 {
        return None;
    }
    let u = timing::clamp01(u);
    let level = (peak * timing::spend(u)).round();
    if level < 1.0 {
        return None;
    }
    let u_next = 1.0 - ((level - 0.5) / peak).sqrt();
    Some(((u_next - u) * span_s).max(0.0))
}

// ===========================================================================
// The engine
// ===========================================================================

/// **THE v2 ENGINE** (§17.1) — the object `CursorGlow` delegates to at the
/// twelve seam points.
///
/// Everything is deterministic and clockless: every entry point takes an
/// injected `now`, and there is no per-frame RNG anywhere below (§18).
#[derive(Clone, Debug)]
pub struct Engine {
    /// Whether this engine owns the frame at all — the `if self.v2.engaged()`
    /// of every seam point. False for the other nine styles, and false for
    /// rainbow kitty while the migration flag is off (§17.3 phase 3).
    engaged: bool,
    /// Reduced-motion posture, mirrored into [`Ctx::cfg`] each tick.
    reduced_motion: bool,
    /// THE momentum integrator and its display follower (§17.1, §19.1).
    spine: Spine,
    /// The laid ribbon and the field index.
    ribbon: Ribbon,
    /// The sky, and the one glint bucket (§13).
    stardust: Stardust,
    /// The flights and their landings.
    meteor: Meteors,
    /// What the host must copy into `RainbowConfig` (§7.1).
    caret_seam: CaretSeam,
    /// The caret cell as last observed.
    caret: (u16, u16),
    /// The focused pane's `(first column, width)` in grid cells, when the
    /// host has told us (`CursorGlow::note_pane_columns`); a typed fold
    /// wraps at ITS edges, never the grid's. Survives `reset` — it is the
    /// host's layout, not the engine's state.
    pane: Option<(u16, u16)>,
    /// Events buffered since the last tick, with their own edges. Resident and
    /// reused — `clear()` keeps capacity, so the steady-state path allocates
    /// nothing (§18).
    ///
    /// Buffered rather than applied immediately because a birth needs
    /// GEOMETRY: `on_event` has no `Geom`, and §5.4's zones are relative to
    /// the ribbon's planned top edge. The host's seam points 1 and 2 run
    /// inside its own tick, before seam point 3, so a buffered event is still
    /// drawn on the frame the caret was observed (T2).
    events: Vec<(Event, Instant)>,
    /// Heroes the host EARNED since the last tick — the kitty's Delight
    /// edge, `(cell, edge)` — dealt in pass 2 beside the events, because a
    /// birth needs the planned bands. Resident and reused, like `events`.
    earned: Vec<((u16, u16), Instant)>,
    /// Cues minted and not yet flushed into a [`Frame`] — see
    /// [`Engine::drain_sound_cues`]. Resident; `append` keeps capacity.
    pending_cues: Vec<SoundCue>,
    /// The impulse awaiting [`Engine::take_companion_impulse`].
    companion: Option<CompanionImpulse>,
    /// The last CREDITED LANDING the caret's re-light floors its paint from
    /// (§7.1, [`CaretSeam::paint`]). Never a typed echo — typing paint is the
    /// spine's — and `None` once the re-light has run its
    /// `RELIGHT_LIFE_S`, so a `Some` is always a live floor.
    paint_at: Option<Instant>,
    /// The last frame's fingerprint — `is_active` follows it (§17.2).
    fp: u64,
    /// The last frame's counters (seam point 12).
    status: Status,
    /// Whether the last frame drawn carried BRISK light — per-frame motion
    /// on glass — the answer [`Engine::needs_frame_cadence`] gives. Cached
    /// at the end of [`Engine::tick`] because seam point 8 is clockless
    /// (v1's argument-free signature) and the host asks it right after the
    /// tick that drew the frame; false whenever nothing is live (T6).
    brisk: bool,
}

impl Default for Engine {
    /// The same engine [`Engine::new`] builds — there is ONE constructor
    /// contract, so a caller reaching this through `Default` gets the full
    /// glint bucket a session starts with and not a silent first capital.
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// A disengaged engine with nothing live and a FULL glint bucket, so the
    /// first capital of a session chimes (§13).
    #[must_use]
    pub fn new() -> Self {
        Self {
            engaged: false,
            reduced_motion: false,
            spine: Spine::new(),
            ribbon: Ribbon::new(),
            stardust: Stardust::new(),
            meteor: Meteors::default(),
            caret_seam: CaretSeam::default(),
            caret: (0, 0),
            pane: None,
            events: Vec::new(),
            earned: Vec::new(),
            pending_cues: Vec::new(),
            companion: None,
            paint_at: None,
            fp: 0,
            status: Status::default(),
            brisk: false,
        }
    }

    // -- the seam's gate ---------------------------------------------------

    /// **THE GATE** — `true` when this engine owns the frame. Every one of the
    /// twelve delegation points is one `if self.v2.engaged()` (D14); when it
    /// is `false`, `CursorGlow` behaves exactly as it does today, which is
    /// what makes "the other nine styles are byte-identical" checkable.
    #[must_use]
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Engage or disengage. Disengaging drops everything live — marks, the
    /// spine, and the events buffered since the last tick — through
    /// [`Engine::reset`]: a style switch hands its residue to the crossfade
    /// ghost, not to a dormant v2. An event the host reported and then did
    /// not tick before switching is discarded with the rest (its light was
    /// never drawn, so its sound is never minted); see
    /// [`Engine::drain_sound_cues`] for why nothing minted can be lost here.
    pub fn set_engaged(&mut self, on: bool) {
        if self.engaged != on {
            self.engaged = on;
            self.reset();
        }
    }

    // -- seam points 1 and 2: events ---------------------------------------

    /// **SEAM POINTS 1 AND 2** — everything the host tells v2.
    ///
    /// Point 1 is `spawn`, *after* the style-agnostic licence gate and
    /// `classify_move` (which consume `nav_hint` / `type_hint`), handing over
    /// an [`Event::Move`]. Point 2 is `note_typed` / `note_erase` /
    /// `note_kill` / `note_return`.
    ///
    /// The event's own edge is `now`. It is buffered and applied at the next
    /// [`Engine::tick`], which — because the host's seam points run inside its
    /// own tick, before the emit phase — is the SAME frame (T2). The spine's
    /// METRIC moves here, immediately, because it is geometry-free and every
    /// birth on this frame must be priced by the value the event produced;
    /// its display follower does not (it advances once, in `tick`).
    ///
    /// **No sound cue is minted here.** A cue carries `heat`, and `heat` is
    /// the display spine: minting on the edge would price the cue by LAST
    /// frame's follower while the light born for the same event is priced by
    /// this frame's, which is two evaluations of the spine on one frame —
    /// exactly what [`Ctx`] exists to forbid. Every cue, the nav tick
    /// included, is minted in [`Engine::tick`] after the one `Spine::update`.
    /// The only thing minted on the edge is the kitty's Wince on an erase or a
    /// kill (§8.2's "oops" column): a pose, priced by nothing.
    pub fn on_event(&mut self, ev: Event, now: Instant) {
        if !self.engaged {
            return;
        }
        match ev {
            Event::Typed { .. } => self.spine.advance(now),
            Event::Erase => {
                self.spine.drain_delete(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Kill { .. } => {
                self.spine.drain_kill(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Move { to, .. } => self.caret = to,
            Event::ReducedMotion(on) => self.reduced_motion = on,
            // Focus is the ribbon's and the sky's to act on (they ember and
            // die in their own `on_event`); a Return is the KEY, and inert;
            // a Sweep is the ribbon's alone.
            Event::Focus(_) | Event::Return | Event::Sweep { .. } => {}
        }
        self.events.push((ev, now));
    }

    /// The focused pane's `(first column, width)`, or `None` for the whole
    /// grid — the edges a typed fold wraps at ([`Event::Sweep`]'s sibling
    /// finding, 2026-09-06: v2 folded at the GRID edge and lit a cell in the
    /// neighbouring pane). Stored whether or not the engine is engaged.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// **THE KITTY'S DELIGHT EDGE**, reported by the host — it is not a cursor
    /// event and so has no place in [`Event`]. §5.8 lists Delight among the
    /// five things that EARN a hero: this mints [`CompanionImpulse::Delight`]
    /// for the body that owns the frame and buffers the heroes
    /// [`companion::heroes_earned`] says the impulse earns, at `(row, col)`,
    /// to be dealt on the next [`Engine::tick`] against that frame's planned
    /// bands. An earned hero is always born as an m1; it chimes only if a
    /// token is available (D5).
    pub fn earn_hero(&mut self, now: Instant, row: u16, col: u16) {
        if !self.engaged {
            return;
        }
        let impulse = CompanionImpulse::Delight;
        self.offer(impulse);
        for _ in 0..companion::heroes_earned(impulse) {
            self.earned.push(((row, col), now));
        }
    }

    /// Pin the spine to at least `floor` while a sing-along is armed — the one
    /// documented momentum bypass (§7.2's celebration arm), routed through the
    /// SAME follower so every legibility cap holds at full drive exactly as it
    /// holds at earned full momentum. Gated like every other seam point: a
    /// sing-along armed while another style owns the frame lights nothing
    /// here (D14).
    pub fn celebrate(&mut self, now: Instant, floor: f32) {
        if !self.engaged {
            return;
        }
        self.spine.drive(now, floor);
    }

    /// Offer a pose impulse for the frame. A pending
    /// [`CompanionImpulse::Meteor`] or [`CompanionImpulse::Land`] outranks
    /// it: those carry the arrival edge the body must not lose, while a pose
    /// is an edge the animator can take next frame.
    fn offer(&mut self, impulse: CompanionImpulse) {
        if !matches!(
            self.companion,
            Some(CompanionImpulse::Meteor { .. } | CompanionImpulse::Land)
        ) {
            self.companion = Some(impulse);
        }
    }

    // -- seam point 3: the frame -------------------------------------------

    /// **SEAM POINT 3** — the emit phase. Advance every clock and write this
    /// frame into `out`.
    ///
    /// ## Tick order
    ///
    /// 1. every clock advances once (spine, bucket, the flare's age);
    /// 2. the RIBBON ingests the frame's events — lays, retracts, abandons,
    ///    and moves its caret mirror;
    /// 3. the ribbon PLANS: this frame's geometry and field index, built once
    ///    (§18) — and only now is the caret's own field `t` this frame's
    ///    number, so [`Ctx::caret_t`] is sampled HERE. That is C2 made
    ///    structural (the caret, the head and a star's halo read one value on
    ///    one frame) and D4's `t_land` made true (the meteor phase-locks to
    ///    the field at the LANDING cell, not at the cell the caret left);
    /// 4. the METEOR classifies each move against that context and its spawn
    ///    hands back what the seam must wire — [`Engine::on_spawn`]; a
    ///    same-row keyed hop under the flight floor mints its NAV TICK here,
    ///    beside the mini-fan the meteor sows for it (§6.12: one edge, two
    ///    twins), priced by the same `ctx.disp`;
    /// 5. STARDUST deals the frame's births, and the host's earned heroes,
    ///    against the planned bands (§5.4);
    /// 6. emit.
    ///
    /// ## Emit order (§6.5, §18)
    ///
    /// `meteor → ribbon (+ hot edge) → stardust → caret seam`.
    ///
    /// **Bolts first.** Truncation sheds from the END of each stream, so the
    /// meteor must be in the buffer before anything else: on a monster jump
    /// the train alone can approach the quad budget, and the lightning is the
    /// star. §18's halo rule is the same law read from the other end —
    /// `halos.truncate` sheds the LAST emitter, which is stardust, and the
    /// sky is precisely the layer that may be thinned.
    ///
    /// ## Idle → zero (T6)
    ///
    /// A disengaged or fully-idle engine writes nothing at all and leaves
    /// `out.fp == 0`.
    pub fn tick(&mut self, now: Instant, geom: Geom, cfg: &Config, out: &mut Frame<'_>) {
        out.caret = self.caret_seam;
        out.companion = self.companion;
        out.fp = 0;
        if !self.engaged {
            self.events.clear();
            self.earned.clear();
            self.fp = 0;
            self.brisk = false;
            return;
        }

        // One evaluation of every clock, before anything reads them.
        self.spine.update(now);
        self.stardust.budget.refill(now);
        // §7.1: the flare is a live relax only until spring-snap has settled;
        // after that a `Some` would be a stale edge the host could not tell
        // from a fresh one. The re-light floor outlives it by its cool and is
        // dropped on the same rule, so `caret_paint` snaps to the spine
        // exactly (T6) instead of chasing a residue.
        if self.caret_seam.flare_at.is_some_and(|t| {
            now.saturating_duration_since(t).as_secs_f32() >= SPRING_SNAP_RESPONSE_S
        }) {
            self.caret_seam.flare_at = None;
        }
        if self
            .paint_at
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() >= RELIGHT_LIFE_S)
        {
            self.paint_at = None;
        }
        let cfg_live = Config {
            reduced_motion: cfg.reduced_motion || self.reduced_motion,
            ..*cfg
        };
        let mut ctx = Ctx {
            now,
            geom,
            cfg: &cfg_live,
            disp: self.spine.disp(),
            birth_disp: self.spine.birth_disp(),
            phase: self.spine.phase(),
            caret: self.caret,
            // Last frame's answer: the ribbon ingests against it and nothing
            // else reads it before it is re-sampled below.
            caret_t: self.ribbon.field_at_caret(),
        };

        // Pass 1 — the ribbon INGESTS, then PLANS. The field index exists
        // from here on, and the caret's own `t` is this frame's.
        self.ribbon.set_pane(self.pane);
        for &(ev, at) in &self.events {
            self.ribbon.on_event(&ev, at, &ctx);
        }
        self.ribbon.plan(&ctx);
        ctx.caret_t = self.ribbon.field_at_caret();

        // Pass 1b — the METEOR classifies each move against this frame's
        // field; a spawn hands back what the seam must wire (§7.2, §12, §13).
        // D4 is read per MOVE, not per frame: `t_land` is the field at THAT
        // move's landing cell, so two licensed jumps coalesced into one frame
        // each lock to their own landing rather than both to the last one's.
        // A landing off the laid ribbon falls back to the caret's answer.
        // Every event still reaches the meteor (a focus loss retires its
        // flights there); only a move re-reads the field, and only a move's
        // licence is bound — so a spawn the meteor might one day hand back
        // for something else is ignored, never a panic on the frame path.
        for i in 0..self.events.len() {
            let (ev, at) = self.events[i];
            let Event::Move {
                from,
                to,
                licence,
                dir,
            } = ev
            else {
                self.meteor.on_event(&ev, at, &ctx);
                continue;
            };
            let mut move_ctx = ctx;
            move_ctx.caret_t = self.ribbon.field_at(to.0, to.1).unwrap_or(ctx.caret_t);
            if let Some(spawn) = self.meteor.on_event(&ev, at, &move_ctx) {
                self.on_spawn(&spawn, licence, &move_ctx);
            }
            if sub_floor_hop(from, to, licence) {
                // §11, §12.3, D17: the nav tick, on the frame its mini-fan is
                // sown — `heat` is THIS frame's one spine, `hue` is C2 at the
                // landing cell (this frame's field, planned above).
                self.pending_cues.push(SoundCue {
                    kind: SoundKind::Navigation,
                    col: to.1,
                    heat: move_ctx.disp,
                    hue: move_ctx.caret_t,
                    dir: dir.sign(),
                    shifted: false,
                });
            }
        }

        // Pass 2 — DEAL. Stars are born against the planned bands (§5.4):
        // the frame's events, then the heroes the host earned (§5.8).
        for &(ev, at) in &self.events {
            self.stardust.on_event(&ev, at, &ctx, &self.ribbon);
        }
        for &(cell, at) in &self.earned {
            self.stardust.earn_hero(at, cell, &ctx, &self.ribbon);
        }
        self.events.clear();
        self.earned.clear();

        // ---- emit, in §6.5's order ----
        let (u0, o0, h0) = (out.under.len(), out.out.len(), out.halos.len());
        self.meteor.emit(&ctx, out);
        // The meteor MINTS its shed fragments, landing fan and mini-fan (§6.5
        // layers 5 and 11, §6.12) but does not draw stars: they are staged on
        // the meteor and handed to the one star producer here — after the
        // plan and the deal, before the ribbon and the sky draw — so a
        // fragment born this frame is on glass this frame. They are
        // transient-lane and zone-free, so the plan they are priced after is
        // the emit order's, not §5.4's. One star family, one drawer.
        self.meteor.sow_into(&mut self.stardust);
        self.ribbon.emit(&ctx, out);
        self.stardust.emit(&ctx, out);

        // The caret seam rides out with the frame; v2 does not own the block
        // fill (§7.1). `paint` is the pure read at this `now`, on the spine
        // this tick evaluated.
        self.caret_seam.field_t = ctx.caret_t;
        self.caret_seam.paint = self.caret_paint(now);
        out.caret = self.caret_seam;
        out.cues.append(&mut self.pending_cues);
        out.companion = self.companion;

        // The fingerprint folds ONLY what this tick appended, so a host that
        // shares the scratch with another producer still gets a v2 answer.
        self.fp = fingerprint(&out.under[u0..], &out.out[o0..], &out.halos[h0..]);
        out.fp = self.fp;
        self.status = Status {
            quads: ((out.under.len() - u0) + (out.out.len() - o0)) as u32,
            halos: (out.halos.len() - h0) as u32,
            stars: self.stardust.live() as u32,
            meteors: self.meteor.live() as u32,
            cells: self.ribbon.live_cells() as u32,
            disp: self.spine.disp(),
            tokens: self.stardust.budget.tokens(),
            fp: self.fp,
        };
        // Seam point 8's answer for THIS frame: is anything moving?
        self.brisk = self.meteor.brisk(now) || self.ribbon.brisk(now) || self.stardust.brisk(now);
    }

    /// **WHAT A SPAWN WIRES** (§6.7's arrival edge, read by everything at
    /// once): the kitty's impulse (§7.2), the glint bucket's emptying (§13),
    /// the caret's flare and re-lit paint (§7.1), and the ONE sound cue of
    /// §12 — minted here, in the engine, because only the engine holds both
    /// the meteor's own `cells` (the number the pixels flew) and the licence
    /// that decides which gesture the synth plays:
    ///
    /// | licence | cue | kitty | flare |
    /// |---|---|---|---|
    /// | `Nav` / `Synthetic` | [`SoundKind::Meteor`] — tick, core, whoosh, bell at `t₀ + T`, five rain glints | `Meteor` (or `Land` under reduced motion) | yes |
    /// | `Return` | [`SoundKind::Enter`] — the cadence at `flight_ms(cells)` (D9), never the brrrring | as above | yes |
    /// | `Pty` | [`SoundKind::Jump`] — the CASCADE lane's brrrring (D18) | none (§8.2: no credit) | no |
    ///
    /// `cells` on the cue is [`Spawn::cells`] UNCHANGED — the meteor resolves
    /// its path to that one integer at the spawn edge and computes its own
    /// `T` from it, so the synth's `flight_ms(cells)` and the pixels'
    /// `t_flight` are the same number to the bit (§8.1 no. 1): the engine
    /// forwards, it never re-rounds. `armed` is always `false` — the
    /// `MeteorArm` credit is the host's and off by default (D11), and only
    /// the host may claim it.
    fn on_spawn(&mut self, spawn: &Spawn, licence: Licence, ctx: &Ctx<'_>) {
        let cells = spawn.cells;
        let (kind, dir) = match licence {
            Licence::Nav | Licence::Synthetic => (
                SoundKind::Meteor {
                    dir: spawn.dir.sign(),
                    cells,
                    armed: false,
                },
                spawn.dir.sign(),
            ),
            Licence::Return => (SoundKind::Enter { cells }, 0),
            Licence::Pty => (SoundKind::Jump, 0),
            // A typed echo never flies (§6.1) and the meteor never spawns on
            // one; if it ever did, nothing here may fire — the keystroke's
            // own cue is the host's.
            Licence::Typed => return,
        };
        let credited = licence != Licence::Pty;
        let reduced = ctx.cfg.reduced_motion;
        if credited {
            // The arrival edge is minted ONCE and read by the pin, the ring,
            // the fan, the flash, the kitty and the bell (§6.7).
            self.companion = Some(if reduced {
                CompanionImpulse::Land
            } else {
                CompanionImpulse::Meteor {
                    dir: spawn.dir,
                    t0: spawn.t0,
                    t_flight: spawn.t_flight,
                }
            });
            // §7.1: white for one frame (not under reduced motion — no
            // flash), and never dimmed at the destination.
            if !reduced {
                self.caret_seam.flare_at = Some(spawn.t0);
            }
            self.paint_at = Some(spawn.t0);
        }
        // §13: a meteor EMPTIES the typing bucket on its move edge, so nothing
        // chimes on top of the rain (or the cadence).
        self.stardust.budget.empty();
        self.pending_cues.push(SoundCue {
            kind,
            col: spawn.landing.1,
            heat: ctx.disp,
            hue: ctx.caret_t,
            dir,
            shifted: false,
        });
    }

    // -- seam points 4-7: the reads ----------------------------------------

    /// **SEAM POINT 4** (`rainbow_field`) — the caret's own position in the
    /// field, and §6.4's `t_land`. C2: the caret, the ribbon head and a star's
    /// halo on that cell are the same colour on the same frame.
    #[must_use]
    pub fn field(&self) -> f32 {
        self.ribbon.field_at_caret()
    }

    /// **SEAM POINT 4** (`rainbow_field_at`) — the field at an arbitrary cell,
    /// or `None` where no ribbon light is laid. O(1) in columns (§18).
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.ribbon.field_at(row, col)
    }

    /// **SEAM POINT 5** (`rainbow_head_rgb`) — the source RGB of the ribbon
    /// head's body, before premultiplication: what a companion cursor inherits
    /// to meet the laid ribbon with no palette seam.
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.ribbon.head_rgb(cfg)
    }

    /// **SEAM POINT 6** (`momentum_display`) — the eased spine.
    #[must_use]
    pub fn momentum_display(&self) -> f32 {
        self.spine.disp()
    }

    /// `trail status`'s `ribbon_segments=` while v2 owns the frame: the
    /// planned boundaries LIT on the last frame ([`Ribbon::lit_segments`]) —
    /// what is on the glass, not what is resident. `Status::cells` keeps the
    /// resident count.
    #[must_use]
    pub fn ribbon_segments(&self) -> usize {
        self.ribbon.lit_segments().count()
    }

    /// `trail status`'s `ribbon_hue_bands=` while v2 owns the frame: distinct
    /// spectrum bands (twenty, v1's own bucketing) among the lit boundaries,
    /// on the REFLECTED position they are painted at (`tri`, never a clamp —
    /// §3.1). Two boundaries in two bands is what the status row's
    /// `ribbon_active` reads as a ribbon, so the paint-conformance bind can
    /// pair the claim with the pixels exactly as it does for v1.
    #[must_use]
    pub fn ribbon_hue_bands(&self) -> usize {
        let mut seen = [false; 20];
        for seg in self.ribbon.lit_segments() {
            let band = (meteor::tri(seg.t) * seen.len() as f32) as usize;
            seen[band.min(seen.len() - 1)] = true;
        }
        seen.iter().filter(|seen| **seen).count()
    }

    /// **SEAM POINT 7** (`rainbow_phase`) — the family's shared phase ring.
    /// Hosts hand this exact value to `CursorRainbow::tick_with_family_phase`
    /// so the caret, its halo and the ribbon under that cell resolve ONE
    /// spectrum position.
    #[must_use]
    pub fn phase(&self) -> f32 {
        self.spine.phase()
    }

    /// The spine itself, for the producers and for tests that need the raw
    /// metric beside its display twin.
    #[must_use]
    pub fn spine(&self) -> &Spine {
        &self.spine
    }

    /// The one glint bucket (§8.1 no. 2, §13). §17.1 lists `budget` among the
    /// engine's fields; it lives on [`Stardust`] because only stardust spends
    /// it, and this is the accessor that name refers to.
    #[must_use]
    pub fn budget(&self) -> &StarBudget {
        &self.stardust.budget
    }

    /// **THE GLYPH PROBE** (§5.4's hard gate), for the host to write before
    /// seam point 3 from the same row probes it takes for the ledger. Until a
    /// row is written every cell of it is UNKNOWN and no sky star is born
    /// over it — the law, stated as a default, not a conservative fallback.
    /// Free transients (fan, shed, mini-fan) are not gated on it; they are
    /// only kept off a cell the probe says is inked.
    pub fn probe_mut(&mut self) -> &mut GlyphProbe {
        self.stardust.probe_mut()
    }

    /// Read-only view of the frame's glyph truth.
    #[must_use]
    pub fn probe(&self) -> &GlyphProbe {
        self.stardust.probe()
    }

    /// **THE CARET'S PAINT** at `now` (§7.1, [`CaretSeam::paint`]) — the pure
    /// read behind the seam's copy: the display spine as of the last tick
    /// (the follower advances only in [`Engine::tick`]), floored by the
    /// landing re-light aged to `now` — `half_life` held for one
    /// [`SPRING_SNAP_RESPONSE_S`] then cooling on it, exactly `0` past
    /// `RELIGHT_LIFE_S` or when no landing is live. The host's caret block
    /// is ticked at its own `now`; this is the number it reads there, and
    /// with no landing live it is [`Engine::momentum_display`] to the bit.
    #[must_use]
    pub fn caret_paint(&self, now: Instant) -> f32 {
        let relight = self.paint_at.map_or(0.0, |at| {
            let age = now.saturating_duration_since(at).as_secs_f32();
            if age >= RELIGHT_LIFE_S {
                0.0
            } else {
                half_life(age, SPRING_SNAP_RESPONSE_S, SPRING_SNAP_RESPONSE_S)
            }
        });
        self.spine.disp().max(relight)
    }

    // -- seam points 8 and 9: the idle contract -----------------------------

    /// **SEAM POINT 8** — does this engine need the frame cadence?
    ///
    /// `true` while the last frame drawn carried BRISK light: a head in
    /// flight, a landing's pin or ring, a thrown or dragged star, a mark
    /// retracting into the caret, a cell inside its 18 ms `edge-in` — the
    /// per-frame MOTION the host's phase-locked train exists for. The TAILS
    /// (§18's three pools being non-empty is a weaker condition than this)
    /// — half-life fades, the swoosh's grace and fade, a star past its hold
    /// twinkling, the spine settling under the ribbon — are paced by
    /// [`Engine::next_change_deadline`] instead, at the [`TAIL_FLOOR`] or the
    /// next visible step, whichever is later. That is the host's own
    /// contract for this predicate (its ember and vapour are excluded the
    /// same way) and it is what keeps the loop from running more often than
    /// a visible change requires. `false` is T6's first half, and it is
    /// what lets the host return to 0 % idle.
    #[must_use]
    pub fn needs_frame_cadence(&self) -> bool {
        self.engaged && self.brisk
    }

    /// **SEAM POINT 9** — the next instant this engine changes what is on
    /// glass, or `None` when nothing is live. T6's second half.
    ///
    /// The `min` of the three producers' cadence laws ([`Cadence`]) plus
    /// the spine's own settle under a laid ribbon (the wave and the wedge
    /// follow `disp`, §4 — a tail on its own clock), clamped to never be
    /// sooner than [`ARM_MIN`] after `now`: a deadline at `now` is a busy
    /// re-arm, not a next change.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.engaged {
            return None;
        }
        let mut cad = Cadence::at(now);
        for d in [
            self.meteor.next_change_deadline(now),
            self.ribbon.next_change_deadline(now),
            self.stardust.next_change_deadline(now),
        ]
        .into_iter()
        .flatten()
        {
            cad.edge(d);
        }
        if let Some(step) = self.ribbon.settle_step_s(self.spine.disp()) {
            cad.tail(step);
        }
        cad.take().map(|at| at.max(now + ARM_MIN))
    }

    /// The last frame's fingerprint — `is_active` follows this when engaged
    /// (§17.2). `0` exactly when nothing is on glass.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fp
    }

    // -- seam points 10-12: drains, reset, status ---------------------------

    /// **SEAM POINT 10** (`take_cursor_cat_motion_pulse`) — read and CLEAR the
    /// pending companion impulse (§7.2). This is the transferring read;
    /// [`Frame::companion`] is a non-consuming report of the same slot.
    pub fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        self.companion.take()
    }

    /// **SEAM POINT 11** (`drain_sound_cues`) — drain the cues minted and not
    /// yet flushed into a [`Frame`].
    ///
    /// **Between ticks this is empty, by construction.** Every cue — the nav
    /// tick, the meteor, the Enter cadence, the PTY cascade, the glints — is
    /// minted INSIDE [`Engine::tick`], after the frame's one `Spine::update`
    /// (see [`Engine::on_event`] for why none may be minted on an edge), and
    /// the same tick appends the whole queue to [`Frame::cues`], the one sink.
    /// So a ticking host reads its cues off the frame and finds this empty;
    /// and a non-ticking path — a style switch, [`Engine::reset`], a
    /// shutdown — can drop nothing that was minted, because nothing minted is
    /// ever waiting. What such a path DOES discard is an event the host
    /// reported and then never ticked: its light was never drawn, so its sound
    /// is never minted, and the two stay paired. The drain is kept as seam
    /// point 11's v2 twin, and as the hook that lets a test PROVE the queue is
    /// empty after a reset.
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        self.pending_cues.drain(..)
    }

    /// **SEAM POINT 12** (`reset`) — drop everything: layout change, style
    /// switch, screen swap.
    pub fn reset(&mut self) {
        self.spine.reset();
        self.ribbon.reset();
        self.stardust.reset();
        self.meteor.reset();
        self.caret_seam = CaretSeam::default();
        // The caret is learned from the next observed move; a key before it
        // lays nowhere rather than at a cell the reset forgot the meaning of.
        self.caret = (0, 0);
        self.events.clear();
        self.earned.clear();
        self.pending_cues.clear();
        self.companion = None;
        self.paint_at = None;
        self.fp = 0;
        self.status = Status::default();
        self.brisk = false;
    }

    /// **SEAM POINT 12** (`translate_scroll_state`) — move EVERY live mark
    /// with the viewport: the ribbon's cells, the meteors and their landings,
    /// and the stars — whose centres are window-absolute px and would
    /// otherwise stay put one row below the text that earned them for the
    /// rest of their 225-460 ms lives, the common case being the PTY's one-row
    /// scroll on every prompt. Marks that leave the grid are dropped rather
    /// than clamped: light pinned to row 0 by a clamp is light on a line
    /// nobody typed. The caret cell and the sky's hero-spacing memory move by
    /// the same rows, so the hot edge, `field()` and the per-row hero cap
    /// read the post-scroll row until the host's next observed move. The
    /// glyph probe is DROPPED by the sky's half, not shifted: the host
    /// re-probes the caret's rows before the next deal, and a stale row
    /// answered as blank would be a star over ink (§5.4).
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if !self.engaged || rows == 0 {
            return;
        }
        self.ribbon.translate_scroll(rows, cell_h);
        self.meteor.translate_scroll(rows, cell_h);
        self.stardust.translate_scroll(rows, cell_h);
        // The caret is a POSITION, not a mark: it cannot be dropped, and the
        // host observes it again on its next move.
        self.caret.0 = self.caret.0.saturating_sub(rows);
    }

    /// **SEAM POINT 12** (`trail_status`) — the `v2_quads=` / `v2_halos=` /
    /// `v2_stars=` rows.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }
}

/// A SAME-ROW KEYED HOP UNDER THE FLIGHT FLOOR (§11, §12.3): `1..8` cells on
/// one row under a `Nav` or `Synthetic` licence — an arrow, a word motion, a
/// short Home/End. The nav tick's trigger. It reads the same
/// [`timing::JUMP_MIN_CELLS`] the meteor's own classifier does, so the light
/// (a mini-fan at 2-7 cells, the ribbon head cell at 1) and the sound can
/// never disagree about where the floor is; a `Typed` echo is the host's
/// keystroke and a `Return` changes row. A `Pty` hop is silent (§12.3:
/// "program-driven moves … are silent") — but note that today it still
/// DRAWS its mini-fan, because `Meteors::on_move` excludes only `Typed`,
/// while §12.3 says the light is absent too; closing that is the meteor's
/// gate, not this predicate's.
fn sub_floor_hop(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
    if !matches!(licence, Licence::Nav | Licence::Synthetic) || from.0 != to.0 {
        return false;
    }
    (1..JUMP_MIN_CELLS).contains(&to.1.abs_diff(from.1))
}

/// The frame fingerprint (§18, T6).
///
/// **Exactly `0` when nothing was appended.** Otherwise a 64-bit FNV-1a fold
/// over every emitted field, with the low bit forced so a legitimate frame can
/// never collide with the idle sentinel. Cheap, order-sensitive, and a pure
/// function of the bytes the renderers will composite — which is what makes
/// `v2_frames_are_pure_functions_of_now` a byte comparison rather than a
/// tolerance.
fn fingerprint(under: &[GlowQuad], out: &[GlowQuad], halos: &[RainHalo]) -> u64 {
    if under.is_empty() && out.is_empty() && halos.is_empty() {
        return 0;
    }
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(PRIME);
    };
    for q in under.iter().chain(out.iter()) {
        mix(u64::from(q.row));
        mix(u64::from(q.x) << 16 | u64::from(q.y));
        mix(u64::from(q.w) << 16 | u64::from(q.h));
        mix(u64::from(q.color) << 8 | u64::from(q.alpha));
    }
    for a in halos {
        mix(u64::from(a.row));
        mix(u64::from(a.x) << 16 | u64::from(a.y));
        mix(u64::from(a.w) << 16 | u64::from(a.h));
        mix(u64::from(a.cx) << 16 | u64::from(a.cy));
        mix(u64::from(a.rx) << 16 | u64::from(a.ry));
        mix(u64::from(a.color));
    }
    h | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    fn config() -> Config {
        Config {
            dark_theme: true,
            intensity: 1.0,
            duration: Duration::from_millis(900),
            ribbon_tall: true,
            theme_fg: 0x00E8_E8F0,
            theme_bg: 0x0016_161C,
            reduced_motion: false,
        }
    }

    /// A frame's worth of host scratch, reused across ticks exactly as the
    /// host reuses `CursorGlow`'s.
    #[derive(Default)]
    struct Scratch {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Scratch {
        fn frame(&mut self) -> Frame<'_> {
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    /// §20.1 `rainbow_kitty_v2_decays_to_exactly_nothing_and_idles_free`, at
    /// the contract level: with nothing live the engine writes NOTHING, its
    /// fingerprint is exactly zero, it asks for no cadence and it names no
    /// deadline. T6.
    #[test]
    fn rainbow_kitty_v2_idles_at_exactly_zero() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut sc = Scratch::default();

        let mut t = t0;
        for _ in 0..8 {
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            assert_eq!(fr.fp, 0, "an idle frame's fingerprint must be exactly 0");
            t += Duration::from_millis(16);
        }
        assert!(sc.under.is_empty(), "idle wrote under quads");
        assert!(sc.out.is_empty(), "idle wrote out quads");
        assert!(sc.halos.is_empty(), "idle wrote halos");
        assert!(sc.cues.is_empty(), "idle minted cues");
        assert_eq!(eng.fingerprint(), 0);
        assert!(!eng.needs_frame_cadence(), "idle must not arm the cadence");
        assert!(
            eng.next_change_deadline(t).is_none(),
            "idle must name no deadline"
        );
        assert_eq!(eng.status().quads, 0);
        assert_eq!(eng.status().halos, 0);
    }

    /// The gate: a DISENGAGED engine is inert at every seam point, which is
    /// the structural half of "the other nine styles are byte-identical"
    /// (D14).
    #[test]
    fn a_disengaged_engine_is_inert_at_every_seam_point() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        assert!(!eng.engaged());
        let mut sc = Scratch::default();

        eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Glyph,
            },
            t0,
        );
        eng.on_event(
            Event::Move {
                from: (3, 0),
                to: (3, 80),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            t0,
        );
        eng.celebrate(t0, 1.0);
        eng.earn_hero(t0, 3, 4);
        eng.translate_scroll(1, 18);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.fp, 0);
        assert!(sc.under.is_empty() && sc.out.is_empty() && sc.halos.is_empty());
        assert!(sc.cues.is_empty(), "a disengaged engine mints no cue");
        assert!(!eng.needs_frame_cadence());
        assert!(eng.next_change_deadline(t0).is_none());
        assert_eq!(
            eng.momentum_display(),
            0.0,
            "a disengaged spine never builds, not even under a sing-along"
        );
        assert_eq!(eng.status().stars, 0, "a disengaged engine earns no hero");
        assert!(eng.take_companion_impulse().is_none());
        assert!(eng.drain_sound_cues().next().is_none());
    }

    /// `Engine::default()` and `Engine::new()` are ONE constructor: a caller
    /// that reaches the engine through `Default` starts with the full glint
    /// bucket a session starts with, so its first capital chimes (§13).
    #[test]
    fn default_and_new_build_the_same_engine() {
        let d = Engine::default();
        let n = Engine::new();
        assert_eq!(d.budget().tokens(), timing::GLINT_CAP);
        assert_eq!(d.budget().tokens(), n.budget().tokens());
        assert!(!d.engaged() && !n.engaged());
    }

    /// An engaged engine with a blank probe over `rows`, so sky stars may be
    /// born anywhere (§5.4's gate answers `Some(false)` on every probed row).
    fn engaged() -> Engine {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        eng
    }

    fn blank_row(eng: &mut Engine, row: u16) {
        eng.probe_mut().probe_row(i32::from(row), &[false; 120]);
    }

    fn typed(n: u16) -> Event {
        Event::Typed {
            cells: n,
            shifted: false,
            class: TypedClass::Glyph,
        }
    }

    fn mv(from: (u16, u16), to: (u16, u16), licence: Licence) -> Event {
        Event::Move {
            from,
            to,
            licence,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    /// §12, §8.1 no. 1 and §6.7 at the seam: a licensed nav jump mints
    /// EXACTLY ONE `Meteor` cue on the spawn frame, at the landing column,
    /// carrying the direction the pixels flew and a `cells` whose
    /// `flight_ms` IS the `t_flight` the kitty's `land_at` (and the pin, the
    /// ring, the fan) read — one clock, one integer, both halves.
    #[test]
    fn a_licensed_jump_mints_one_meteor_cue_on_the_shared_clock() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 0), (3, 20), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);

        assert_eq!(sc.cues.len(), 1, "one gesture on the observed move");
        let cue = sc.cues[0];
        let SoundKind::Meteor { dir, cells, armed } = cue.kind else {
            panic!("a nav jump is a Meteor cue, got {:?}", cue.kind);
        };
        assert_eq!((dir, cells, armed), (1, 20, false));
        assert_eq!(cue.col, 20, "the cue pans to the landing column");
        assert_eq!(cue.dir, 1);
        assert!(!cue.shifted, "an echo has no key behind it");
        let Some(CompanionImpulse::Meteor {
            t_flight, t0: at, ..
        }) = eng.take_companion_impulse()
        else {
            panic!("the spawn frame carries the kitty's meteor impulse");
        };
        assert_eq!(at, t0);
        assert_eq!(
            timing::flight(f32::from(cells)),
            t_flight,
            "the bell's delay and the landing edge must be one number"
        );
        assert_eq!(
            eng.budget().tokens(),
            0.0,
            "a meteor empties the typing bucket on its move edge (§13)"
        );
        // …and the next frame mints nothing again: the cue is an edge.
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(8), geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1);
    }

    /// D9 / §12.3: a Return-licensed move flies the meteor anatomy but sounds
    /// the CADENCE — exactly one `Enter { cells }` cue, no `Meteor`, no
    /// `Jump`; and a PTY line feed sounds the cascade lane's `Jump` with no
    /// kitty, no flare and no paint behind it (§8.2: no credit).
    ///
    /// The Enter's vector is DIAGONAL, so its path is not a whole number of
    /// cells (`√(60² + 2²) = 60.03`): the cue must carry the one integer the
    /// meteor resolved and flew, and `flight_ms` of that integer must be the
    /// `t_flight` the kitty lands on — a cue rounded from the fractional path
    /// on the audio side alone would ring 0.03 ms off the landing frame,
    /// which is the two-clocks defect `timing` exists to make impossible.
    #[test]
    fn a_return_is_the_cadence_and_a_pty_feed_is_the_cascade() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((5, 60), (6, 0), Licence::Return), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let caret = fr.caret;
        assert_eq!(sc.cues.len(), 1);
        let SoundKind::Enter { cells } = sc.cues[0].kind else {
            panic!("a keyed Enter is the cadence, got {:?}", sc.cues[0].kind);
        };
        assert_eq!(cells, 60, "the true path of the down-left vector, in cells");
        assert_eq!(sc.cues[0].col, 0);
        let Some(CompanionImpulse::Meteor { t_flight, .. }) = eng.take_companion_impulse() else {
            panic!("a credited Enter carries the kitty's meteor impulse");
        };
        assert_eq!(
            timing::flight(f32::from(cells)),
            t_flight,
            "the cadence's delay and the landing edge are one number on a diagonal path"
        );
        assert_eq!(caret.flare_at, Some(t0), "an Enter is credited: it flares");
        assert_eq!(caret.paint, 1.0, "never dimmed at the destination");

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((10, 0), (11, 0), Licence::Pty), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let caret = fr.caret;
        assert_eq!(sc.cues.len(), 1);
        assert_eq!(sc.cues[0].kind, SoundKind::Jump, "the brrrring lane");
        assert!(
            eng.take_companion_impulse().is_none(),
            "program output offers the kitty nothing (§8.2)"
        );
        assert_eq!(caret.flare_at, None, "no credit, no flare");
        assert_eq!(caret.paint, 0.0, "no credit, no paint");
        assert!(
            eng.status().meteors > 0,
            "…but the meteor still flies (D18)"
        );
    }

    /// D17 / §11 / §12.3: a same-row keyed hop under the flight floor sounds
    /// the NAV TICK — one `Navigation` cue — on the same frame its light (a
    /// mini-fan at 2-7 cells, nothing at 1) is born, priced by THAT frame's
    /// one spine and coloured by that frame's field at the landing (§17.1:
    /// one evaluation per frame; a cue minted on the edge would carry last
    /// frame's follower); a no-op is silent (D11) and a program-driven hop is
    /// silent (whether it also draws is the meteor's gate — see
    /// `sub_floor_hop`).
    #[test]
    fn a_sub_floor_hop_is_one_nav_tick_and_a_no_op_is_silent() {
        let t0 = Instant::now();
        let cfg = config();
        let ms = Duration::from_millis;

        // Warm: 20 keys lay 20 cells and light the spine; the hop back onto
        // laid column 15 on the NEXT frame must carry that frame's spine —
        // which the update at the hop's `now` moved — and column 15's stop.
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let mut t = t0;
        for k in 0..20u16 {
            t += ms(30);
            eng.on_event(mv((3, k), (3, k + 1), Licence::Typed), t);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
        }
        let before = eng.momentum_display();
        let hop = t + ms(30);
        eng.on_event(mv((3, 20), (3, 15), Licence::Nav), hop);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(hop, geom(), &cfg, &mut fr);
        let cue = sc
            .cues
            .iter()
            .find(|c| c.kind == SoundKind::Navigation)
            .expect("the hop ticked");
        assert_eq!(
            sc.cues
                .iter()
                .filter(|c| c.kind == SoundKind::Navigation)
                .count(),
            1
        );
        assert_ne!(
            eng.momentum_display(),
            before,
            "the hop frame's update moved the spine, so the two frames differ"
        );
        assert_eq!(
            cue.heat,
            eng.momentum_display(),
            "the tick is priced by the hop frame's own spine"
        );
        assert_eq!(
            cue.hue,
            eng.field_at(3, 15).expect("column 15 is laid"),
            "C2: the tick wears the landing cell's stop"
        );
        assert_eq!((cue.col, cue.dir), (15, -1));

        // Cold: the tick and its mini-fan, and nothing else.
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 10), (3, 15), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1);
        assert_eq!(sc.cues[0].kind, SoundKind::Navigation);
        assert_eq!((sc.cues[0].col, sc.cues[0].dir), (15, 1));
        assert!(
            eng.status().stars > 0,
            "the mini-fan is on glass with its tick"
        );
        assert_eq!(eng.status().meteors, 0, "…and nothing flew");
        assert!(
            eng.take_companion_impulse().is_none(),
            "a hop is not a kitty event"
        );

        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 10), (3, 9), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1, "an arrow ticks");
        assert_eq!(sc.cues[0].kind, SoundKind::Navigation);
        assert_eq!(sc.cues[0].dir, -1);
        assert_eq!(eng.status().stars, 0, "a one-cell move draws no mini-fan");

        for (from, to, licence) in [
            ((3, 10), (3, 10), Licence::Nav),
            ((3, 10), (3, 15), Licence::Pty),
            ((3, 10), (3, 11), Licence::Typed),
        ] {
            let mut eng = engaged();
            let mut sc = Scratch::default();
            eng.on_event(mv(from, to, licence), t0);
            let mut fr = sc.frame();
            eng.tick(t0, geom(), &cfg, &mut fr);
            assert!(
                sc.cues.is_empty(),
                "{licence:?} {from:?}→{to:?} must be silent"
            );
        }
    }

    /// §8.2's kitty column: an erase or a kill is an "oops" (`Wince`), and a
    /// meteor minted on the same frame — or already pending — outranks it,
    /// because the meteor carries the arrival edge the body must not lose.
    #[test]
    fn an_erase_winces_the_kitty_and_a_meteor_outranks_it() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        eng.on_event(Event::Erase, t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(fr.companion, Some(CompanionImpulse::Wince)));
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Wince)
        ));
        assert!(
            eng.take_companion_impulse().is_none(),
            "the take transfers it"
        );

        eng.on_event(
            Event::Kill {
                cells: 6,
                scope: KillScope::Word,
            },
            t0,
        );
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Wince)
        ));

        // Same frame: the erase's wince, then a jump — the meteor wins.
        eng.on_event(Event::Erase, t0);
        eng.on_event(mv((3, 40), (3, 0), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(
            fr.companion,
            Some(CompanionImpulse::Meteor { dir: Dir::Left, .. })
        ));
        // Pending meteor, then an erase before the host takes it: still the
        // meteor.
        eng.on_event(Event::Erase, t0 + Duration::from_millis(1));
        assert!(matches!(
            eng.take_companion_impulse(),
            Some(CompanionImpulse::Meteor { .. })
        ));
    }

    /// §6.11 / §7.1: under reduced motion a credited spawn is a LANDING with
    /// no flight behind it — the kitty gets `Land`, the caret gets no flash —
    /// and the sound is unchanged (§8.2: "Reduced motion … unchanged").
    #[test]
    fn reduced_motion_lands_the_kitty_without_a_flight_or_a_flash() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(Event::ReducedMotion(true), t0);
        eng.on_event(mv((3, 0), (3, 30), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(matches!(fr.companion, Some(CompanionImpulse::Land)));
        assert_eq!(fr.caret.flare_at, None, "no flash under reduced motion");
        assert_eq!(fr.caret.paint, 1.0, "…but the caret is still not dimmed");
        assert_eq!(sc.cues.len(), 1);
        assert!(matches!(sc.cues[0].kind, SoundKind::Meteor { .. }));
    }

    /// §7.1: the flare is ONE edge. It rides out as `Some(t₀)` on the spawn
    /// frame and through the spring-snap relax, then is cleared once the
    /// relax has settled — so the host can never mistake a stale edge for a
    /// fresh one.
    #[test]
    fn the_caret_flare_is_one_edge_that_ages_out() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(mv((3, 0), (3, 30), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.flare_at, Some(t0));
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(100), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.flare_at, Some(t0), "live through the relax");
        let settled =
            t0 + Duration::from_secs_f32(SPRING_SNAP_RESPONSE_S) + Duration::from_millis(1);
        let mut fr = sc.frame();
        eng.tick(settled, geom(), &cfg, &mut fr);
        assert_eq!(
            fr.caret.flare_at, None,
            "cleared once spring-snap has settled"
        );
    }

    /// §7.1 / §17.1 and `RainbowConfig::paint`'s own contract: the caret's
    /// paint IS the display spine. After a burst and 220 ms of silence it
    /// still reads `momentum_display()` to the bit — ≥ 70 % of its typing
    /// value, on the ribbon's τ 0.85 s release — and NOT the 0.5 a private
    /// 220 ms half-life would give, which was the measured "mix collapsed
    /// 4-8× faster than the light" caret defect. A typed key does not lift it
    /// above the spine. A credited landing floors it at exactly `1.0` on the
    /// landing frame (never dimmed at the destination), holds it through the
    /// flare's relax, cools it on the flare's constant, and hands it back to
    /// the bare spine — exactly `0` on a cold engine — at its life.
    #[test]
    fn the_caret_paint_is_the_display_spine_floored_by_the_landing_relight() {
        let t0 = Instant::now();
        let cfg = config();
        let ms = Duration::from_millis;

        let mut eng = engaged();
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 0.0, "idle: nothing lit, nothing landed");

        // 30 keys at 30 ms.
        let mut t = t0;
        let mut while_typing = 0.0;
        for _ in 0..30 {
            t += ms(30);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            while_typing = fr.caret.paint;
        }
        let typing = eng.momentum_display();
        assert!(typing > 0.3, "sustained typing lit the spine: {typing}");
        assert_eq!(while_typing, typing, "while typing, paint is the spine");

        // 220 ms of silence — the number a private caret cool would halve on.
        let quiet = t + ms(220);
        let mut fr = sc.frame();
        eng.tick(quiet, geom(), &cfg, &mut fr);
        let disp = eng.momentum_display();
        assert!(disp < typing, "the release is under way");
        assert_eq!(fr.caret.paint, disp, "paint is the spine, to the bit");
        assert_eq!(eng.caret_paint(quiet), disp, "the pure read agrees");
        assert!(
            disp >= 0.7 * typing,
            "220 ms into a τ 0.85 s release keeps ≥ 70 %: {disp} of {typing}"
        );

        // One key on a cold engine does not lift the paint above the spine
        // (the old hand-edge law read 0.95 here).
        let mut eng = engaged();
        let mut sc = Scratch::default();
        eng.on_event(typed(1), t0);
        let mut fr = sc.frame();
        eng.tick(t0 + ms(16), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, eng.momentum_display());
        assert!(fr.caret.paint < 0.5, "one key is not a landing");

        // A cold Ctrl-E: the spine stays at zero (T1) and the re-light
        // carries the caret.
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let land = t0 + ms(1);
        eng.on_event(mv((3, 1), (3, 40), Licence::Nav), land);
        let mut fr = sc.frame();
        eng.tick(land, geom(), &cfg, &mut fr);
        assert_eq!(eng.momentum_display(), 0.0, "a jump builds no momentum");
        assert_eq!(fr.caret.paint, 1.0, "never dimmed at the destination");

        let hold = Duration::from_secs_f32(SPRING_SNAP_RESPONSE_S);
        let mut fr = sc.frame();
        eng.tick(land + hold - ms(1), geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 1.0, "held through the flare's relax");

        let mut fr = sc.frame();
        eng.tick(land + hold + hold, geom(), &cfg, &mut fr);
        assert!(
            (fr.caret.paint - 0.5).abs() < 1e-4,
            "half one constant after the hold: {}",
            fr.caret.paint
        );
        assert!(
            fr.caret.paint >= eng.momentum_display(),
            "the re-light is a floor under the spine, never a ceiling"
        );

        let dead = land + Duration::from_secs_f32(RELIGHT_LIFE_S) + ms(1);
        let mut fr = sc.frame();
        eng.tick(dead, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.paint, 0.0, "back to the bare spine at its life");
        assert_eq!(eng.caret_paint(dead), 0.0);
    }

    /// §5.8 / §7.2 / D5 at the seam: the kitty's Delight EARNS one m1 at the
    /// reported cell and mints the `Delight` impulse; each hero chimes once
    /// while the bucket has a token and is still BORN when it does not —
    /// light is never rationed, only sound.
    #[test]
    fn delight_earns_one_hero_that_chimes_only_with_a_token() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;

        // Four delights on four rows inside 3 ms: three tokens, three
        // glints, four stars.
        let rows = [3u16, 6, 9, 12];
        for (k, &row) in rows.iter().enumerate() {
            let at = t0 + ms(k as u64);
            blank_row(&mut eng, row - 1);
            eng.earn_hero(at, row, 5);
            let mut fr = sc.frame();
            eng.tick(at, geom(), &cfg, &mut fr);
            assert!(
                matches!(fr.companion, Some(CompanionImpulse::Delight)),
                "the frame carries the pose"
            );
            eng.take_companion_impulse();
            assert_eq!(
                eng.status().stars as usize,
                k + 1,
                "an earned hero is always born (delight {k})"
            );
        }
        assert_eq!(
            sc.cues.len(),
            3,
            "three tokens, three glints, the fourth silent"
        );
        for (cue, _) in sc.cues.iter().zip(rows) {
            assert!(matches!(cue.kind, SoundKind::Stardust { .. }));
            assert_eq!(cue.col, 5, "the glint sits at its star's column");
        }
    }

    /// §5.4 at the seam: with NO probe data a cell is unknown, and an unknown
    /// sky bears no star — the host must have looked (D16). The same hero
    /// over a probed-blank row is born.
    #[test]
    fn an_unprobed_sky_bears_no_star() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        assert!(eng.probe().is_empty());
        eng.earn_hero(t0, 3, 5);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(eng.status().stars, 0, "unknown → no star");
        assert!(sc.cues.is_empty(), "…and no glint for a star not born");

        blank_row(&mut eng, 2);
        eng.earn_hero(t0 + Duration::from_millis(1), 3, 5);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert_eq!(eng.status().stars, 1, "probed blank → born");
    }

    /// Seam point 12: a scroll moves EVERY live mark with the viewport — the
    /// stars and the hot edge by exactly one cell height per row, in BOTH
    /// streams they are drawn in (additive quads on a dark ground, ink halos
    /// on a light one, §3.3), the caret's row with them — and drops what
    /// leaves the grid rather than clamping it to row 0. Each theme's own
    /// stream is asserted NON-EMPTY first: a comparison over nothing is green
    /// with the stars standing still.
    #[test]
    fn a_scroll_moves_every_live_star_and_the_caret_with_the_viewport() {
        for dark_theme in [true, false] {
            scroll_moves_every_mark(&Config {
                dark_theme,
                ..config()
            });
        }
    }

    fn scroll_moves_every_mark(cfg: &Config) {
        let theme = if cfg.dark_theme { "dark" } else { "light" };
        let t0 = Instant::now();
        let g = geom();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;

        // A hero over row 3 and six laid cells (columns 0-5) on one frame;
        // on the next, the caret stepped back onto laid column 5 by a
        // one-cell hop — which draws no light of its own, so every quad
        // compared below is a star's or the ribbon's.
        blank_row(&mut eng, 2);
        eng.earn_hero(t0, 3, 5);
        eng.on_event(mv((3, 0), (3, 6), Licence::Typed), t0);
        eng.on_event(typed(6), t0);
        let mut fr = sc.frame();
        eng.tick(t0, g, cfg, &mut fr);
        let t0 = t0 + ms(1);
        eng.on_event(mv((3, 6), (3, 5), Licence::Nav), t0);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t0, g, cfg, &mut fr);
        // The hero plus whatever strike stars the deal put over the two typed
        // cells: how many is the sky's law, not this one's — what this test
        // pins is that EVERY one of them moves and none is lost.
        let live = eng.status().stars;
        assert!(live >= 1, "{theme}: the earned hero is on glass");
        let quads: Vec<(u16, u16)> = sc.out.iter().map(|q| (q.row, q.y)).collect();
        let halos: Vec<(u16, u16, u16)> = sc.halos.iter().map(|a| (a.row, a.y, a.cy)).collect();
        if cfg.dark_theme {
            assert!(
                !quads.is_empty(),
                "{theme}: the star and the hot edge wrote additive quads"
            );
        } else {
            assert!(!halos.is_empty(), "{theme}: the ink fork wrote its halos");
        }
        let t_caret = eng.field();
        assert_eq!(
            eng.field_at(3, 5),
            Some(t_caret),
            "the caret sits on a laid cell, so field() is that cell's t"
        );
        assert_ne!(
            eng.field_at(3, 4),
            eng.field_at(3, 5),
            "the two laid cells differ, so a stale caret would read the other"
        );

        // The viewport moves; the clock does not (T7 makes a same-`now`
        // frame legal), so the ONLY difference between the two frames is the
        // scroll — a mark still ramping in would otherwise cross a rounding
        // step on its own between two instants and fake a one-pixel drift.
        eng.translate_scroll(1, g.ch as u16);
        let mut sc2 = Scratch::default();
        let mut fr = sc2.frame();
        eng.tick(t0, g, cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            live,
            "{theme}: one row up keeps every star"
        );
        let ch = g.ch as u32;
        let quads2: Vec<(u16, u16)> = sc2.out.iter().map(|q| (q.row, q.y)).collect();
        let halos2: Vec<(u16, u16, u16)> = sc2.halos.iter().map(|a| (a.row, a.y, a.cy)).collect();
        assert_eq!(quads.len(), quads2.len(), "{theme}: every quad survived");
        assert_eq!(halos.len(), halos2.len(), "{theme}: every halo survived");
        for (b, a) in quads.iter().zip(&quads2) {
            assert_eq!(a.0 + 1, b.0, "{theme}: every quad moved up one row");
            assert_eq!(
                u32::from(a.1) + ch,
                u32::from(b.1),
                "{theme}: …by exactly ch px"
            );
        }
        for (b, a) in halos.iter().zip(&halos2) {
            assert_eq!(a.0 + 1, b.0, "{theme}: every halo moved up one row");
            assert_eq!(
                u32::from(a.1) + ch,
                u32::from(b.1),
                "{theme}: …its band by exactly ch px"
            );
            assert_eq!(
                u32::from(a.2) + ch,
                u32::from(b.2),
                "{theme}: …and its centre with it"
            );
        }
        assert_eq!(
            eng.field(),
            t_caret,
            "{theme}: the caret moved with its cell: field() still reads the same t"
        );
        assert_eq!(eng.field_at(2, 5), Some(t_caret));

        eng.translate_scroll(g.rows as u16, g.ch as u16);
        let mut fr = sc2.frame();
        eng.tick(t0 + ms(2), g, cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            0,
            "{theme}: a star that left the grid is dropped"
        );
        assert_eq!(eng.status().cells, 0, "{theme}: so is the ribbon");
    }

    /// A style switch discards what was never TICKED and can drop nothing
    /// that was MINTED: every cue is minted inside `tick`, so a hop reported
    /// and then disengaged before its frame mints no tick (its light was never
    /// drawn either — the pair stays paired), the queue is empty on both sides
    /// of the switch, and the same hop ticked before the switch rides out on
    /// the frame and leaves nothing behind for the drain.
    #[test]
    fn a_style_switch_drops_an_unticked_event_and_never_a_minted_cue() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        eng.on_event(mv((3, 10), (3, 15), Licence::Nav), t0);
        assert!(
            eng.drain_sound_cues().next().is_none(),
            "nothing is minted on the edge"
        );
        eng.set_engaged(false);
        assert!(eng.drain_sound_cues().next().is_none());
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert!(sc.cues.is_empty(), "the unticked hop went with the switch");
        eng.set_engaged(true);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert!(sc.cues.is_empty(), "…and does not resurface on re-engage");
        assert_eq!(eng.status().stars, 0, "nor does its light");

        eng.on_event(
            mv((3, 10), (3, 15), Licence::Nav),
            t0 + Duration::from_millis(2),
        );
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(2), geom(), &cfg, &mut fr);
        assert_eq!(sc.cues.len(), 1, "the ticked hop rode out on its frame");
        assert!(
            eng.drain_sound_cues().next().is_none(),
            "the frame is the one sink: nothing is left for the drain"
        );
        eng.set_engaged(false);
        assert!(eng.drain_sound_cues().next().is_none());
    }

    /// C2 at the seam, and D4: `Frame.caret.field_t` is THIS frame's field at
    /// THIS frame's caret — sampled after the events are laid and the ribbon
    /// has planned — so it equals `field()` on the same frame, and a meteor
    /// landing on a laid cell locks its phase (the cue's `hue`) to that cell,
    /// not to the cell the caret left.
    #[test]
    fn the_caret_seam_reports_this_frames_field_at_the_landing() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        // Lay 30 cells with the caret at column 30.
        eng.on_event(mv((3, 0), (3, 30), Licence::Typed), t0);
        eng.on_event(typed(30), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.caret.field_t, eng.field(), "one number on one frame");
        let stale = fr.caret.field_t;

        // Ctrl-A onto column 5 — a laid cell, whose own t is not the head's.
        let at = t0 + Duration::from_millis(20);
        eng.on_event(mv((3, 30), (3, 5), Licence::Nav), at);
        let mut fr = sc.frame();
        eng.tick(at, geom(), &cfg, &mut fr);
        let landing_t = eng.field_at(3, 5).expect("column 5 is laid");
        assert_ne!(
            landing_t, stale,
            "the landing's stop differs from the launch's"
        );
        assert_eq!(
            fr.caret.field_t, landing_t,
            "the seam reads the landing cell"
        );
        assert_eq!(fr.caret.field_t, eng.field());
        let meteor = sc
            .cues
            .iter()
            .find(|c| matches!(c.kind, SoundKind::Meteor { .. }))
            .expect("the jump minted its cue");
        assert_eq!(meteor.hue, landing_t, "the cue's hue is the landing's stop");
    }

    /// Events reach the spine on their own edge, so every birth on the frame
    /// that follows is priced by one number (§17.1) — and the whole thing
    /// still returns to exact zero afterwards (T6).
    #[test]
    fn typing_lights_the_spine_and_the_spine_returns_to_zero() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut sc = Scratch::default();

        let mut t = t0;
        for _ in 0..30 {
            t += Duration::from_millis(30);
            eng.on_event(
                Event::Typed {
                    cells: 1,
                    shifted: false,
                    class: TypedClass::Glyph,
                },
                t,
            );
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
        }
        assert!(
            eng.momentum_display() > 0.0,
            "sustained typing must light the spine"
        );
        assert!(eng.status().disp > 0.0);

        // …and a long silence drains it to EXACTLY zero.
        for _ in 0..40 {
            t += Duration::from_millis(500);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
        }
        assert_eq!(eng.momentum_display(), 0.0);
        assert!(!eng.needs_frame_cadence());
        assert_eq!(eng.fingerprint(), 0);
    }

    /// The scratch is the HOST's and is reused: a tick must never assume it
    /// starts empty, and the fingerprint must fold only what this tick wrote
    /// (§18, and the reason `is_active` can follow it).
    #[test]
    fn the_fingerprint_folds_only_this_ticks_own_marks() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let mut sc = Scratch::default();
        // A foreign producer already wrote into the shared scratch.
        sc.out.push(GlowQuad {
            row: 2,
            x: 40,
            y: 40,
            w: 3,
            h: 3,
            color: 0x0011_2233,
            alpha: 0,
        });
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        assert_eq!(fr.fp, 0, "a foreign quad is not v2's light");
        assert_eq!(sc.out.len(), 1, "and v2 must not disturb it");
    }

    /// The direction rule is the axis rule (D15), and its sign is the host's
    /// existing `SoundCue::dir` convention.
    #[test]
    fn direction_resolves_on_the_dominant_axis() {
        assert_eq!(Dir::of(80, 0), Dir::Right);
        assert_eq!(Dir::of(-80, 0), Dir::Left);
        assert_eq!(Dir::of(0, -3), Dir::Up);
        assert_eq!(Dir::of(1, 9), Dir::Down);
        assert_eq!(Dir::of(9, 9), Dir::Right, "the tie is x-major");
        assert_eq!(Dir::Right.sign(), 1);
        assert_eq!(Dir::Up.sign(), 1);
        assert_eq!(Dir::Left.sign(), -1);
        assert_eq!(Dir::Down.sign(), -1);
        assert!(Dir::Up.is_vertical() && !Dir::Right.is_vertical());
    }

    /// `Config::from_glow` is the ONE place the host's vocabulary meets v2's.
    #[test]
    fn config_projects_only_what_v2_needs() {
        let glow = GlowConfig {
            enabled: true,
            classic_mono: false,
            style: crate::cursor_glow::GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x007A_A2F7,
            duration: Duration::from_millis(1234),
            length: 18,
            intensity: 0.7,
            radius: 0.6,
            ring: true,
            dark_theme: false,
            theme_fg: 0x0012_3456,
            theme_bg: 0x0065_4321,
            beam: false,
            head_dx: 0.5,
            pack: None,
            wake_persist_s: 0.0,
            ribbon_tall: false,
        };
        let c = Config::from_glow(&glow, true);
        assert!(!c.dark_theme);
        assert!((c.intensity - 0.7).abs() < 1e-6);
        assert_eq!(c.duration, Duration::from_millis(1234));
        assert!(!c.ribbon_tall);
        assert_eq!(c.theme_fg, 0x0012_3456);
        assert_eq!(c.theme_bg, 0x0065_4321);
        assert!(c.reduced_motion);
    }

    // -- §18 cadence: what the engine asks the host for ---------------------

    /// The sim's own no-busy-loop guard. The engine's [`ARM_MIN`] clamp is
    /// one of the laws under test, so the sim cannot lean on it: a deadline
    /// at or before `now` is stepped past by this much and COUNTED, which is
    /// exactly what a host booking `deadline_arms_by_owner` would do.
    const SIM_MIN: Duration = Duration::from_millis(1);

    /// The typing row of the scripted gestures; its neighbours are probed
    /// blank so the sky may deal (§5.4).
    const SIM_ROW: u16 = 12;

    /// One turn of the sim: when it woke, what the engine named as its next
    /// change on that turn, and whether it asked for the frame cadence.
    type Turn = (Instant, Option<Instant>, bool);

    /// What the sim wakes on — the engine's fold, or one producer's alone.
    type Ask<'a> = &'a dyn Fn(&Engine, Instant) -> Option<Instant>;

    fn engine_with_sky() -> Engine {
        let mut eng = Engine::new();
        eng.set_engaged(true);
        let blank = vec![false; geom().cols];
        for r in [SIM_ROW - 1, SIM_ROW, SIM_ROW + 1] {
            eng.probe_mut().probe_row(i32::from(r), &blank);
        }
        eng
    }

    fn secs(s: f32) -> Duration {
        Duration::from_secs_f32(s)
    }

    /// 12 cps prose from column 4 of [`SIM_ROW`]: a `Typed` move and a
    /// `Typed` echo per key, as the host's seam reports them.
    fn prose_script(t0: Instant, keys: usize) -> Vec<(Instant, Event)> {
        const TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog and keeps on going ";
        let mut out = Vec::with_capacity(keys * 2);
        for k in 0..keys {
            let at = t0 + secs(k as f32 / 12.0);
            let ch = TEXT[k % TEXT.len()] as char;
            let col = 4 + k as u16;
            out.push((
                at,
                Event::Move {
                    from: (SIM_ROW, col),
                    to: (SIM_ROW, col + 1),
                    licence: Licence::Typed,
                    dir: Dir::Right,
                },
            ));
            let class = if ch == ' ' {
                TypedClass::Space
            } else if ch.is_ascii_uppercase() {
                TypedClass::Capital
            } else {
                TypedClass::Glyph
            };
            out.push((
                at,
                Event::Typed {
                    cells: 1,
                    shifted: class == TypedClass::Capital,
                    class,
                },
            ));
        }
        out
    }

    /// THE BEFORE CAPTURE'S SCHEDULE, 13 s (`capture-before`, 2026-09-05):
    /// 44 keys of prose at 10 cps from 0.5 s, two ctrl-a / ctrl-e round
    /// trips over the 44 cells, four backspaces, an Enter, then idle to the
    /// end — ~4.6 s of typing. The row the "expected arms per frame"
    /// projection is measured on.
    fn capture_script(t0: Instant) -> Vec<(Instant, Event)> {
        const TEXT: &[u8] = b"echo the quick brown fox jumps over the lazy dog";
        let mut out = Vec::new();
        let mut col: u16 = 4;
        let mut at = t0 + secs(0.5);
        for &b in &TEXT[..44] {
            let ch = b as char;
            let class = if ch == ' ' {
                TypedClass::Space
            } else {
                TypedClass::Glyph
            };
            out.push((
                at,
                Event::Move {
                    from: (SIM_ROW, col),
                    to: (SIM_ROW, col + 1),
                    licence: Licence::Typed,
                    dir: Dir::Right,
                },
            ));
            out.push((
                at,
                Event::Typed {
                    cells: 1,
                    shifted: false,
                    class,
                },
            ));
            col += 1;
            at += secs(0.1);
        }
        let end = col;
        for (t, to) in [(5.5, 4), (6.0, end), (6.6, 4), (7.1, end)] {
            let from = col;
            col = to;
            out.push((
                t0 + secs(t),
                Event::Move {
                    from: (SIM_ROW, from),
                    to: (SIM_ROW, col),
                    licence: Licence::Nav,
                    dir: if col > from { Dir::Right } else { Dir::Left },
                },
            ));
        }
        for k in 0..4u16 {
            let at = t0 + secs(8.0 + 0.083 * f32::from(k));
            out.push((
                at,
                Event::Move {
                    from: (SIM_ROW, col),
                    to: (SIM_ROW, col - 1),
                    licence: Licence::Typed,
                    dir: Dir::Left,
                },
            ));
            col -= 1;
            out.push((at, Event::Erase));
        }
        out.push((
            t0 + secs(9.0),
            Event::Move {
                from: (SIM_ROW, col),
                to: (SIM_ROW + 1, 0),
                licence: Licence::Return,
                dir: Dir::Down,
            },
        ));
        out
    }

    /// A 40-cell same-row nav jump at `t0` — a meteor with `T = 90 ms`.
    fn meteor_script(t0: Instant) -> Vec<(Instant, Event)> {
        vec![(
            t0,
            Event::Move {
                from: (SIM_ROW, 10),
                to: (SIM_ROW, 50),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
        )]
    }

    /// A HOST LOOP IN MINIATURE. It wakes at every scripted event and at the
    /// deadline `ask` names, ticks the engine on each wake exactly as the
    /// host does (scratch cleared, events applied at their own edges), and
    /// records one turn per wake — a turn whose deadline is `Some` is one
    /// booked arm. It stops at `until`, or when nothing is scripted and the
    /// engine names no deadline (idle → zero).
    fn drive(
        eng: &mut Engine,
        cfg: &Config,
        script: &[(Instant, Event)],
        from: Instant,
        until: Instant,
        ask: Ask<'_>,
    ) -> Vec<Turn> {
        let mut sc = Scratch::default();
        let mut turns = Vec::new();
        let mut next = 0usize;
        let mut now = from;
        loop {
            while let Some(&(at, ev)) = script.get(next)
                && at <= now
            {
                eng.on_event(ev, at);
                next += 1;
            }
            sc.under.clear();
            sc.out.clear();
            sc.halos.clear();
            sc.cues.clear();
            let mut fr = sc.frame();
            eng.tick(now, geom(), cfg, &mut fr);
            let d = ask(eng, now);
            turns.push((now, d, eng.needs_frame_cadence()));
            let wake = d.map(|d| d.max(now + SIM_MIN));
            let ev = script.get(next).map(|e| e.0);
            let Some(n) = (match (wake, ev) {
                (Some(w), Some(e)) => Some(w.min(e)),
                (w, e) => w.or(e),
            }) else {
                break;
            };
            if n >= until {
                break;
            }
            now = n;
        }
        turns
    }

    /// Arms booked in `[a, b)` seconds after `origin`.
    fn arms_in(turns: &[Turn], origin: Instant, a: f32, b: f32) -> usize {
        turns
            .iter()
            .filter(|(t, d, _)| d.is_some() && *t >= origin + secs(a) && *t < origin + secs(b))
            .count()
    }

    /// **THE CENSUS** (informational; `--nocapture`): how many wakes per
    /// second each producer asks for, alone and folded, during (a) 12 cps
    /// prose, (b) the 1.5 s after the last key — grace, reach, retract and
    /// fade of the swoosh separately — and (c) `T .. T + 320` of a meteor.
    /// The numbers behind the cadence law, re-measured on every run.
    #[test]
    fn cadence_census_prints_what_each_producer_asks_for() {
        let cfg = config();
        let asks: [(&str, Ask<'_>); 4] = [
            ("engine  ", &|e, now| e.next_change_deadline(now)),
            ("meteor  ", &|e, now| e.meteor.next_change_deadline(now)),
            ("ribbon  ", &|e, now| e.ribbon.next_change_deadline(now)),
            ("stardust", &|e, now| e.stardust.next_change_deadline(now)),
        ];
        println!();
        println!(
            "cadence census — arms per second: prose | +0..0.5 | grace 0.5..0.75 | reach 0.75..0.9 | retract 0.9..1.3 | fade 1.3..1.54 | tail 0..1.5 | idle at | meteor T..T+320"
        );
        for (name, ask) in asks {
            let t0 = Instant::now();
            let script = prose_script(t0, 24);
            let last = script.last().map(|e| e.0).unwrap_or(t0);
            let mut eng = engine_with_sky();
            let turns = drive(&mut eng, &cfg, &script, t0, last + secs(2.6), ask);
            let prose = arms_in(&turns, t0, 0.0, 2.0) as f32 / 2.0;
            let w = |a: f32, b: f32| arms_in(&turns, last, a, b) as f32 / (b - a);
            let idle = turns
                .iter()
                .find(|(t, d, _)| *t >= last && d.is_none())
                .map_or_else(
                    || "NEVER".to_string(),
                    |(t, _, _)| {
                        format!(
                            "{:.0} ms",
                            t.saturating_duration_since(last).as_secs_f32() * 1000.0
                        )
                    },
                );
            let tm = Instant::now();
            let mut eng = engine_with_sky();
            let flight = timing::flight(40.0).as_secs_f32();
            let turns_m = drive(
                &mut eng,
                &cfg,
                &meteor_script(tm),
                tm,
                tm + secs(flight + 0.32),
                ask,
            );
            let meteor = arms_in(&turns_m, tm, 0.0, flight + 0.32) as f32 / (flight + 0.32);
            println!(
                "  {name} | {prose:6.1} | {:6.1} | {:6.1} | {:6.1} | {:6.1} | {:6.1} | {:6.1} | {idle:>8} | {meteor:6.1}",
                w(0.0, 0.5),
                w(0.5, 0.75),
                w(0.75, 0.9),
                w(0.9, 1.3),
                w(1.3, 1.54),
                w(0.0, 1.5),
            );
        }
        // The BEFORE capture's 13 s schedule, on the engine's fold: every
        // wake is one rendered frame and books one arm, so the wake count
        // is the renders the engine asks the host for — against the
        // capture's 937 (v1) and 1252 (v2 before this law).
        let t0 = Instant::now();
        let mut eng = engine_with_sky();
        let turns = drive(
            &mut eng,
            &cfg,
            &capture_script(t0),
            t0,
            t0 + secs(13.0),
            &|e, now| e.next_change_deadline(now),
        );
        let arms = turns.iter().filter(|(_, d, _)| d.is_some()).count();
        let brisk = turns.iter().filter(|(_, _, b)| *b).count();
        println!(
            "  capture schedule 13 s: {} wakes (renders), {arms} arms, {brisk} at frame cadence — {:.1} wakes/s, {:.2} arms per wake",
            turns.len(),
            turns.len() as f32 / 13.0,
            arms as f32 / turns.len().max(1) as f32
        );
    }

    /// **THE TAIL LAW** (§18, the owner's "efficient"): after the last key,
    /// while only FADES are live — the sky's holds spent and its strike
    /// stars dead, the swoosh in its grace or its fade — the engine asks for
    /// no more than ~25 wakes a second (the [`TAIL_FLOOR`], ~22 Hz, plus a
    /// twinkle step), and once idle it asks for nothing at all and stays
    /// that way. Before the cadence law the sky answered `now` for every
    /// live star and this read ~1 000 arms a second.
    #[test]
    fn a_settling_sky_asks_for_no_more_frames_than_its_light_can_show() {
        let cfg = config();
        let t0 = Instant::now();
        let script = prose_script(t0, 24);
        let last = script.last().map(|e| e.0).unwrap_or(t0);
        let mut eng = engine_with_sky();
        let turns = drive(&mut eng, &cfg, &script, t0, last + secs(2.6), &|e, now| {
            e.next_change_deadline(now)
        });
        // 25 arms/s over the window, rounded up, plus one for a window edge.
        let budget = |a: f32, b: f32| ((b - a) * 25.0).ceil() as usize + 1;
        let grace = arms_in(&turns, last, 0.50, 0.75);
        assert!(
            grace <= budget(0.50, 0.75),
            "grace tail: {grace} arms in 250 ms (budget {})",
            budget(0.50, 0.75)
        );
        let fade = arms_in(&turns, last, 1.30, 1.54);
        assert!(
            fade <= budget(1.30, 1.54),
            "swoosh fade: {fade} arms in 240 ms (budget {})",
            budget(1.30, 1.54)
        );
        // No arm is ever booked for a deadline at or before the turn it was
        // named on (no busy re-arm).
        for (t, d, _) in &turns {
            if let Some(d) = d {
                assert!(
                    *d >= *t + ARM_MIN,
                    "a deadline {:?} before now + 1 ms at +{:.1} ms",
                    d.saturating_duration_since(*t),
                    t.saturating_duration_since(last).as_secs_f32() * 1000.0
                );
            }
        }
        // Idle → zero, and it holds: the next ask a second later is still
        // `None`, the cadence is off, the pools are empty.
        let idle_at = turns
            .iter()
            .find(|(t, d, _)| *t >= last && d.is_none())
            .map(|(t, _, _)| *t)
            .expect("the engine never reached idle");
        assert!(
            idle_at <= last + secs(2.5),
            "idle only at +{:?}",
            idle_at.saturating_duration_since(last)
        );
        assert!(
            turns.iter().all(|(t, _, _)| *t <= idle_at),
            "the sim woke after idle"
        );
        assert!(!eng.needs_frame_cadence());
        assert!(eng.next_change_deadline(idle_at + secs(1.0)).is_none());
        let st = eng.status();
        assert_eq!((st.stars, st.cells, st.meteors), (0, 0, 0));
    }

    /// **THE FOLD** ([`Cadence`]): brisk answers the next frame whatever
    /// else was offered; a tail is never sooner than the [`TAIL_FLOOR`] and
    /// keeps its own later step; a past edge is not a next change; a past
    /// tail instant is the floor; nothing offered is `None`.
    #[test]
    fn the_cadence_fold_floors_tails_and_ignores_past_edges() {
        let now = Instant::now();
        assert!(Cadence::at(now).take().is_none());
        let mut c = Cadence::at(now);
        c.edge(now - secs(0.5));
        assert!(c.take().is_none(), "a past edge is not a next change");
        let mut c = Cadence::at(now);
        c.tail(0.010);
        assert_eq!(c.take(), Some(now + TAIL_FLOOR), "a 10 ms step is floored");
        let mut c = Cadence::at(now);
        c.tail(0.080);
        assert_eq!(c.take(), Some(now + secs(0.080)), "an 80 ms step stands");
        let mut c = Cadence::at(now);
        c.tail_at(now - secs(1.0));
        assert_eq!(c.take(), Some(now + TAIL_FLOOR), "a past tail is the floor");
        let mut c = Cadence::at(now);
        c.tail(0.080);
        c.edge(now + secs(0.030));
        assert_eq!(
            c.take(),
            Some(now + secs(0.030)),
            "an edge inside a tail wins"
        );
        let mut c = Cadence::at(now);
        c.edge(now + secs(0.002));
        c.brisk();
        assert_eq!(
            c.take(),
            Some(now + FRAME_CADENCE),
            "brisk is the next frame"
        );
    }

    /// **THE FLIGHT LAW** (T2, T7, §6.2): from the spawn frame through the
    /// landing pin's close (`T .. T + 150`) the engine asks for EVERY frame —
    /// each wake exactly [`FRAME_CADENCE`] after the last, and seam point 8
    /// agreeing on every one of them. Before the cadence law the shed
    /// fragments' sky answered `now`, and the sim woke every millisecond.
    #[test]
    fn a_flight_asks_for_every_frame() {
        let cfg = config();
        let t0 = Instant::now();
        let mut eng = engine_with_sky();
        let flight = timing::flight(40.0);
        let until = t0 + flight + secs(0.150);
        let turns = drive(&mut eng, &cfg, &meteor_script(t0), t0, until, &|e, now| {
            e.next_change_deadline(now)
        });
        let expect = FRAME_CADENCE.as_secs_f32();
        assert!(
            turns.len() >= 20,
            "only {} turns over the flight",
            turns.len()
        );
        for w in turns.windows(2) {
            let gap = w[1].0.saturating_duration_since(w[0].0).as_secs_f32();
            assert!(
                (gap - expect).abs() < 1e-4,
                "a {:.2} ms wake at +{:.1} ms; the flight asks for {:.2} ms",
                gap * 1000.0,
                w[0].0.saturating_duration_since(t0).as_secs_f32() * 1000.0,
                expect * 1000.0
            );
        }
        assert!(
            turns.iter().all(|(_, _, brisk)| *brisk),
            "seam point 8 said no cadence during the flight"
        );
    }

    /// **A BATCHED ECHO SWEEPS EVERY GLYPH CELL IT SKIPPED** (seam point 1,
    /// [`Event::Sweep`], 2026-09-06). Three keys land before the PTY echoes
    /// once, three cells at a time — SSH, a busy shell, a loaded machine. The
    /// keys lay at the caret the tick replays with (the landing), so the two
    /// glyph cells before it were dark for good: the control engine below,
    /// fed the same keys and move WITHOUT the sweep, is the frame this law
    /// failed on before the event existed — it lights the landing's own cell
    /// and nothing behind it. With the sweep every glyph cell is a ribbon
    /// cell, and the per-key path is untouched (the sweep is never sent for
    /// a one-cell echo).
    #[test]
    fn a_batched_echo_sweeps_every_glyph_cell_it_skipped() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            eng.on_event(typed(1), t0);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            for k in 1..=3u64 {
                eng.on_event(typed(1), t0 + ms(k));
            }
            let echo = t0 + ms(10);
            if sweep {
                eng.on_event(
                    Event::Sweep {
                        row: 3,
                        col0: 2,
                        col1: 5,
                    },
                    echo,
                );
            }
            eng.on_event(mv((3, 2), (3, 5), Licence::Typed), echo);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(echo, g, &cfg, &mut fr);
            eng
        };
        let swept = drive(true);
        for col in 1..=4u16 {
            assert!(
                swept.field_at(3, col).is_some(),
                "glyph cell {col} of the batch is a ribbon cell"
            );
        }
        assert!(
            swept.field_at(3, 5).is_none(),
            "the landing's own cell is the caret's, not a glyph's"
        );
        let holed = drive(false);
        assert!(
            holed.field_at(3, 4).is_some(),
            "the keys lay the landing's glyph"
        );
        assert!(
            holed.field_at(3, 2).is_none() && holed.field_at(3, 3).is_none(),
            "the control: without the sweep the batch leaves two dark cells — \
             the frame this law is about"
        );
    }

    /// **A TYPED FOLD WRAPS AT THE PANE'S EDGE, NOT THE GRID'S** (2026-09-06,
    /// the second finding v1's deleted fold tests exposed). A split pane
    /// twelve columns in and forty wide: the key that wraps its line lands
    /// at `(3, 12)`, and the glyph it laid is the previous row's LAST PANE
    /// cell, `(2, 51)`. Without [`Engine::set_pane`] the ribbon knew only
    /// the grid's margin and lit `(3, 11)` — the cell before the caret, in
    /// the neighbouring pane, where no glyph ever landed — which is the
    /// control below.
    #[test]
    fn a_typed_fold_wraps_at_the_pane_edge_not_the_grid_edge() {
        let t0 = Instant::now();
        let cfg = config();
        let mut g = geom();
        g.cols = 100;
        g.win_w = 900;
        let drive = |pane: Option<(u16, u16)>| -> Engine {
            let mut eng = engaged();
            eng.set_pane(pane);
            eng.on_event(mv((2, 50), (2, 51), Licence::Typed), t0);
            eng.on_event(typed(1), t0);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            let fold = t0 + Duration::from_millis(20);
            eng.on_event(typed(1), fold);
            eng.on_event(mv((2, 51), (3, 12), Licence::Typed), fold);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(fold, g, &cfg, &mut fr);
            eng
        };
        let paned = drive(Some((12, 40)));
        assert!(
            paned.field_at(2, 51).is_some(),
            "the fold's glyph is the pane's last cell"
        );
        assert!(
            paned.field_at(2, 99).is_none(),
            "no ribbon cell leaks into the neighbouring pane"
        );
        let whole = drive(None);
        assert!(
            whole.field_at(3, 11).is_some() && whole.field_at(2, 51).is_none(),
            "the control: told nothing of the pane, the fold lights the cell \
             before the caret in the neighbouring pane — the frame this law \
             is about"
        );
    }

    /// **A KEY WHOSE ECHO LANDS A FRAME LATE STILL LIGHTS ITS GLYPH**
    /// ([`Event::Sweep`] for a one-cell echo, 2026-09-06). The key lays at
    /// the caret the tick replays with; when the PTY's echo arrives on the
    /// NEXT frame that caret is still the old one, so the key re-lit the
    /// previous glyph's cell and its own stayed dark until the next key —
    /// the last letter of every line, dark for good. The control below is
    /// that frame. With the sweep the echo's move lights the cell, and a key
    /// whose echo shares its tick is untouched (the owner check).
    #[test]
    fn a_key_whose_echo_lands_a_frame_late_still_lights_its_glyph() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            eng.on_event(typed(1), t0);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            // The key, echoed a frame later.
            eng.on_event(typed(1), t0 + ms(8));
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0 + ms(8), g, &cfg, &mut fr);
            let echo = t0 + ms(16);
            if sweep {
                eng.on_event(
                    Event::Sweep {
                        row: 3,
                        col0: 2,
                        col1: 3,
                    },
                    echo,
                );
            }
            eng.on_event(mv((3, 2), (3, 3), Licence::Typed), echo);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(echo, g, &cfg, &mut fr);
            eng
        };
        let swept = drive(true);
        assert!(
            swept.field_at(3, 2).is_some(),
            "the late-echoed glyph is a ribbon cell"
        );
        assert!(
            swept.field_at(3, 1).is_some(),
            "…and the first one still is"
        );
        let dark = drive(false);
        assert!(
            dark.field_at(3, 2).is_none(),
            "the control: without the sweep the late-echoed glyph stays dark — \
             the frame this law is about"
        );
    }
}
