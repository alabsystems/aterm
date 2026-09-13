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

/// **THE FLOW STATE** — re-exported at the theme's root because it is the
/// theme's published state, not the spine's private one: it rides [`Ctx`],
/// [`Status`] and (through the seam) the `trail status` row.
pub use spine::Flow;

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{BeamVertex, GlowQuad, RainHalo};

use crate::cursor_glow::{Geom, GlowConfig, SoundCue, band_pos, band_row};
use crate::spectrum::spectrum_snap;
use crate::trail_sound::SoundKind;
use meteor::{Meteors, Spawn};
use ribbon::Ribbon;
use spine::Spine;
use stardust::{GlyphProbe, StarBudget, Stardust};
use timing::{
    ECHO_LEDGER_DEPTH, ECHO_PATIENCE_S, JUMP_MIN_CELLS, SPRING_SNAP_RESPONSE_S, half_life,
};

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
    /// **A DELIVERED INSERT** (2026-09-10): a file drop, a paste, a Tab
    /// completion, a ⌃V — the user's own gesture, licensed at DELIVERY (the
    /// host's completed write) and handed over behind its own `Sweep` of the
    /// whole span. Inert everywhere `Typed` is inert (no wake, no abandon,
    /// no meteor, no mini-fan, no tick, no star deal); it differs from
    /// `Typed` only at the echo ledger, which pays a hole BEFORE the insert
    /// from the older presses (a key whose late echo the seam refused) and
    /// charges the insert's own span to nothing — the span is the insert's,
    /// not the presses'.
    Insert,
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
    /// A capital — by Shift or by Caps Lock: the GLYPH is asked — earns an
    /// m1 at the glyph's cell (§5.8) and, at the synth, the octave RING at
    /// +60 ms, −6 dB, and the ×4 sparkle (§10.4, re-ruled 2026-09-10; the
    /// keyed seam's `click_shifted` agrees with this classification).
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
        /// `(row, col)` the caret left — the host's LAST OBSERVED cell,
        /// licensed or not. The engine's own mirror advances only on licensed
        /// moves, and [`Engine::echo_bridge`] reads the hop between the two,
        /// so a producer that hands over anything but the observed origin
        /// would be inventing holes (paid for by nothing, so laid as nothing).
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
    ///
    /// The engine mints one of these itself for a LATE echo — a key's glyph
    /// cells the seam refused to license because the echo came after its
    /// `0.25 s` window — from the mirror to the landing of the next licensed
    /// typed move, paid for press by press from the [`EchoLedger`]
    /// ([`Engine::echo_bridge`], 2026-09-10).
    Sweep {
        /// The echoed row.
        row: u16,
        /// The first glyph cell of the echo.
        col0: u16,
        /// The landing (exclusive): the caret the echo left the row at.
        col1: u16,
    },
    /// **THE INSERT'S REWRITE** (seam point 1, 2026-09-10): the program
    /// pulled the caret BACK inside a span a delivered insert laid — Claude
    /// Code swapping a dropped image path for `[Image #1] `, which the seam
    /// reads as the insert's own rewrite (`CursorGlow::insert_span`). The
    /// ribbon retracts the row's suffix from `col` farthest-first at
    /// `12·n + 240` ms, exactly the kill's law, and moves its caret; the sky
    /// finishes the field stars of the retracted cells on the same span; the
    /// engine's caret mirror moves so the next typed echo has no unexplained
    /// gap. Nothing is born, nothing winces, nothing sounds, the spine and
    /// the echo ledger are untouched (T1: nothing born; T5: toward the
    /// caret). Minted only by the seam's insert-scoped rewrite arm, never
    /// from a key — a Backspace or a kill keeps its own event.
    Rewrite {
        /// The rewritten row.
        row: u16,
        /// The caret after the rewrite — the retract's near end.
        col: u16,
        /// Cells the caret retreated by — the retract's span pricing.
        cells: u16,
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

/// **THE MEND MARK, published for this tick** (§23's addendum "The mend",
/// 2026-09-08): the typed key this frame carries is a TYPO FIX — it came
/// within [`spine::MEND_WINDOW_S`] of a Backspace run of at most
/// [`spine::MEND_MAX_DELETES`] deletes ([`spine::Spine::mend`]) — and this is
/// the mark it mends. `Some` on exactly the tick that births the fix; `None`
/// on every other tick, including the erase's own (the erase is the sky's
/// [`Event::Erase`] as before).
///
/// Consumers: the ribbon prices the fix's births at `max(birth_disp, disp)`
/// ([`Ctx::birth_disp`] floored by [`Mend::disp`]); stardust MAY birth the
/// fix's m2 from the erased cell's sky position (`row`, `col`) and inherit
/// the momentum it carried — left to the sky's own law. The meteor and the
/// caret read nothing here: a mend is a birth price, not a gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mend {
    /// The LAST delete's edge ([`spine::EraseMark::at`]).
    pub at: Instant,
    /// The birth spine the deleting interrupted ([`spine::EraseMark::disp`])
    /// — what the fix is born at when the live `birth_disp` is lower.
    pub disp: f32,
    /// The erased cell's row: the caret's row as observed on the last
    /// delete's tick.
    pub row: u16,
    /// The erased cell's column — the cell the last Backspace emptied, which
    /// is the cell the fix key re-lights. With `deletes` 2 the run emptied
    /// `col..col + 2`.
    pub col: u16,
    /// How many deletes the run counted (1 or 2 — a mend never carries more).
    pub deletes: u8,
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
    /// The mend mark, on the one tick that births a typo fix (see [`Mend`]).
    pub mend: Option<Mend>,
    /// [`spine::Spine::surge`] — how much of the crisp edge's stretch the
    /// last typed key bought, 0..1. A BIRTH price, like [`Ctx::birth_disp`]:
    /// only [`ribbon::edge_cells`] reads it, and only where a key lays.
    pub surge: f32,
    /// **THE FLOW STATE** ([`spine::Spine::flow`]) — the run, its high-water
    /// mark and the open ramp. Flow draws nothing of its own: every consumer
    /// LERPS from identity at `heat == 0`, so a frame under a cold hand is
    /// byte-identical to the same frame in a theme with no flow in it.
    pub flow: Flow,
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
    /// `v2_bridged=` — cells the echo ledger has laid since the engine was
    /// engaged that the host's own sweeps did not: a refused late echo's
    /// glyphs, relit by the next licensed one ([`Engine::echo_bridge`]). The
    /// admission ring keeps reading `declined no-fresh-hint` for the late
    /// move itself — the gate is untouched — so this is the row that says a
    /// hole was repaired. Cumulative, like the host's `spawns`.
    pub bridged: u32,
    /// The eased spine.
    pub disp: f32,
    /// **THE FLOW ROW** — `flow=` / `combo=` / `combo_best=`. The number a
    /// turn-based agent reads off the control socket to tell that the human
    /// is mid-flow and hold its turn.
    pub flow: Flow,
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
// The echo ledger
// ===========================================================================

/// **THE ECHO LEDGER** — typed presses whose caret advance the engine has not
/// seen yet, oldest first, each `(press, cells)`.
///
/// One press is one cell of ribbon and nothing more: a licensed typed move
/// SPENDS the ledger, oldest first, for every cell it lays or the host lays
/// for it ([`Engine::echo_bridge`]), and a press the caret never answers
/// inside [`ECHO_PATIENCE_S`] is dropped as swallowed. Fixed-size and
/// `Copy`, like the host's own `TypedStamps`: the steady frame path allocates
/// nothing (§18), and [`ECHO_LEDGER_DEPTH`] is the most presses one move could
/// ever pair with, so a fuller ledger would be presses no move can spend.
#[derive(Clone, Copy, Debug)]
pub struct EchoLedger {
    /// Packed oldest-first: every `Some` precedes every `None`.
    slots: [Option<(Instant, u16)>; ECHO_LEDGER_DEPTH],
    /// The Tier-1 projection of `EchoLedgerBridge`'s tallies — test-only, so
    /// the shipping frame path carries nothing for it.
    #[cfg(test)]
    tally: LedgerTally,
}

impl Default for EchoLedger {
    // Written out because `[T; N]: Default` stops at N = 32 and the ledger
    // is 128 deep (`ECHO_LEDGER_DEPTH`, 2026-09-12).
    fn default() -> Self {
        Self {
            slots: [None; ECHO_LEDGER_DEPTH],
            #[cfg(test)]
            tally: LedgerTally::default(),
        }
    }
}

/// Cells the ledger has banked, spent, forfeited and expired since it was
/// made: the four dispositions `EchoLedgerBridge`'s `OnePressOneCell`
/// conserves, read off the REAL ledger by
/// `tests::the_real_engine_conforms_to_the_echo_ledger_model`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
struct LedgerTally {
    banked: u32,
    spent: u32,
    forfeited: u32,
    expired: u32,
}

impl EchoLedger {
    /// Bank one press worth `cells` (a wide glyph is two, an IME commit its
    /// sum — the host prices that at the key). When full, the OLDEST press is
    /// dropped: the newest presses are the ones an echo can still be for.
    fn bank(&mut self, at: Instant, cells: u16) {
        if cells == 0 {
            return;
        }
        #[cfg(test)]
        {
            self.tally.banked += u32::from(cells);
        }
        if let Some(free) = self.slots.iter().position(Option::is_none) {
            self.slots[free] = Some((at, cells));
        } else {
            #[cfg(test)]
            {
                self.tally.forfeited += self.slots[0].map_or(0, |(_, c)| u32::from(c));
            }
            self.slots.copy_within(1.., 0);
            self.slots[ECHO_LEDGER_DEPTH - 1] = Some((at, cells));
        }
    }

    /// Drop every press older than [`ECHO_PATIENCE_S`]: swallowed, not late.
    /// Packing is kept — presses are banked oldest-first, so the stale prefix
    /// goes and the rest shifts down.
    fn expire(&mut self, now: Instant) {
        let mut kept = [None; ECHO_LEDGER_DEPTH];
        let mut n = 0;
        for (at, cells) in self.slots.into_iter().flatten() {
            if now.saturating_duration_since(at).as_secs_f32() <= ECHO_PATIENCE_S {
                kept[n] = Some((at, cells));
                n += 1;
            } else {
                #[cfg(test)]
                {
                    self.tally.expired += u32::from(cells);
                }
            }
        }
        self.slots = kept;
    }

    /// Cells the banked presses can still pay for.
    fn total(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .map(|&(_, cells)| usize::from(cells))
            .sum()
    }

    /// Cells of the presses banked strictly BEFORE `key` — the ones whose
    /// echoes could lie to the left of that key's own cell.
    fn older_than(&self, key: Instant) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|&&(at, _)| at < key)
            .map(|&(_, cells)| usize::from(cells))
            .sum()
    }

    /// Spend `cells` oldest-first — the presses that produced an echo are the
    /// ones it consumes, so one press can never fund a second cell.
    fn spend(&mut self, mut cells: usize) {
        for slot in &mut self.slots {
            if cells == 0 {
                break;
            }
            let Some((at, have)) = *slot else { break };
            let take = usize::from(have).min(cells);
            cells -= take;
            #[cfg(test)]
            {
                self.tally.spent += take as u32;
            }
            let left = have - take as u16;
            *slot = (left > 0).then_some((at, left));
        }
        // Re-pack: a spent slot in the middle would hide the presses behind it.
        let mut kept = [None; ECHO_LEDGER_DEPTH];
        for (n, entry) in self.slots.into_iter().flatten().enumerate() {
            kept[n] = Some(entry);
        }
        self.slots = kept;
    }

    /// Forget every press.
    fn clear(&mut self) {
        #[cfg(test)]
        {
            self.tally.forfeited += self.total() as u32;
        }
        self.slots = [None; ECHO_LEDGER_DEPTH];
    }

    /// The ledger as `EchoLedgerBridge` sees it at `now`, partitioned by the
    /// licensing `key`: `(older, younger, stale)` cells — presses banked
    /// before the key, the key and the presses behind it, and presses past
    /// [`ECHO_PATIENCE_S`] that the next move will drop. Test-only: the
    /// Tier-1 projection reads the real ledger through it.
    #[cfg(test)]
    fn partition(&self, key: Instant, now: Instant) -> (usize, usize, usize) {
        let (mut older, mut younger, mut stale) = (0, 0, 0);
        for &(at, cells) in self.slots.iter().flatten() {
            let cells = usize::from(cells);
            if now.saturating_duration_since(at).as_secs_f32() > ECHO_PATIENCE_S {
                stale += cells;
            } else if at < key {
                older += cells;
            } else {
                younger += cells;
            }
        }
        (older, younger, stale)
    }
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
    /// Whether [`Engine::caret`] has been learned from an observed move since
    /// the last [`Engine::reset`]. The echo ledger measures every hop
    /// against the mirror, and a mirror nobody has set is not a position to
    /// measure from.
    caret_known: bool,
    /// **THE ECHO LEDGER** — typed presses whose caret advance the engine has
    /// not seen yet (see [`EchoLedger`]).
    echo: EchoLedger,
    /// Cells the ledger has laid that the host did not sweep — [`Status::bridged`].
    bridged: u32,
    /// The clock of the host's own sweep for the last observed move — the
    /// licensing key the ledger partitioned by — or `None` when the host
    /// sent none. Test-only: the Tier-1 twin asserts it IS the key's clock
    /// on every move, so the partition it projects is the engine's own.
    #[cfg(test)]
    last_host_sweep_at: Option<Instant>,
    /// The cell the last Backspace emptied — the caret as of that erase's
    /// tick (the ribbon's and the sky's own reading of it) — published as
    /// [`Mend::row`] / [`Mend::col`] when a fix key mends the run.
    erased: (u16, u16),
    /// The erase mark a typed key just spent ([`spine::Spine::mend`] read on
    /// its edge), waiting for the tick that births the key to publish it as
    /// [`Ctx::mend`]. Taken by that tick.
    mend: Option<spine::EraseMark>,
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
    /// A bar fan or drop fan the host asked for this frame (§27) —
    /// `(edge, stars, ring)` — dealt to the meteor pool on the next
    /// [`Engine::tick`] against that frame's `Ctx`. Like `earned`, it is
    /// not a cursor event.
    party: Option<(Instant, u8, bool)>,
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
            caret_known: false,
            echo: EchoLedger::default(),
            bridged: 0,
            #[cfg(test)]
            last_host_sweep_at: None,
            erased: (0, 0),
            mend: None,
            pane: None,
            events: Vec::new(),
            earned: Vec::new(),
            pending_cues: Vec::new(),
            companion: None,
            paint_at: None,
            fp: 0,
            status: Status::default(),
            brisk: false,
            party: None,
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
    ///
    /// **The mend is read on the edge too** (§23's addendum "The mend"): a
    /// typed key asks the spine whether it is a typo fix
    /// ([`spine::Spine::mend`]) BEFORE it advances the metric (the advance
    /// closes the erase run), and a `Some` is held for the tick that births
    /// the key, which publishes it as [`Ctx::mend`]. A value, not a timer:
    /// nothing is armed on a pending mark, and a stale one is simply not a
    /// mend when the next key reads it.
    pub fn on_event(&mut self, ev: Event, now: Instant) {
        if !self.engaged {
            return;
        }
        match ev {
            Event::Typed { cells, .. } => {
                let mend = self.spine.mend(now);
                self.spine.advance(now);
                if mend.is_some() {
                    self.mend = mend;
                }
                self.echo.bank(now, cells);
            }
            Event::Erase => {
                self.spine.drain_delete(now);
                // Stale presses were swallowed before the erase and are
                // dropped as expired; the fresh ones are forfeited. Either
                // way none survive — the split is the ledger's book-keeping.
                self.echo.expire(now);
                self.echo.clear();
                self.offer(CompanionImpulse::Wince);
            }
            Event::Kill { .. } => {
                self.spine.drain_kill(now);
                self.echo.expire(now);
                self.echo.clear();
                self.offer(CompanionImpulse::Wince);
            }
            Event::Move {
                from, to, licence, ..
            } => {
                // The ledger reads the mirror BEFORE the move lands on it —
                // the hop between the two is what it exists to explain. Its
                // sweep goes to the FRONT of the frame's events: the key's own
                // `Typed` (replayed at the landing) and the host's sweep both
                // lay cells to the RIGHT of the hole, and a cell laid past a
                // cohort's end mints a new cohort — the seam and the feather
                // this sweep exists to prevent. Laid first, the hole's cells
                // join the cohort and every later lay joins them.
                // The host's own sweep for this move, if it sent one, is the
                // event just before the move. It covers `from..to` on the
                // KEY's clock — the press that licensed the move — so a move
                // with no hole before it needs nothing more (the per-key path
                // stays byte-identical), and a hole takes the same clock, so
                // the frame that echoes the key shows the whole run lit.
                let host_sweep = match self.events.last() {
                    Some(&(
                        Event::Sweep {
                            row,
                            col0: c0,
                            col1,
                        },
                        at,
                    )) if row == to.0 && c0 == from.1 && col1 == to.1 => Some(at),
                    _ => None,
                };
                #[cfg(test)]
                {
                    self.last_host_sweep_at = host_sweep;
                }
                let bridge = self.echo_bridge(from, to, licence, host_sweep, now);
                // THE RIBBON'S BIRTH FLOOR (2026-09-12): the ledger has read
                // the host sweep's clock as the licensing KEY above; the
                // ribbon is born no earlier than one stamp window before
                // the echo (`timing::SWEEP_BIRTH_FLOOR_S`), so a stalled
                // batch dated at its oldest press is not born seconds ago
                // into a cohort that has already faded. A per-key sweep's
                // clock is inside the window: byte-identical.
                let floor = now
                    .checked_sub(Duration::from_secs_f32(timing::SWEEP_BIRTH_FLOOR_S))
                    .unwrap_or(now);
                let birth = host_sweep.map(|at| at.max(floor));
                if let (Some(birth), Some((Event::Sweep { .. }, at))) =
                    (birth, self.events.last_mut())
                {
                    *at = birth;
                }
                if let Some(col0) = bridge
                    && (col0 < from.1 || host_sweep.is_none())
                {
                    let host_end = if host_sweep.is_some() { from.1 } else { to.1 };
                    self.bridged = self.bridged.saturating_add(u32::from(host_end - col0));
                    self.events.insert(
                        0,
                        (
                            Event::Sweep {
                                row: to.0,
                                col0,
                                col1: to.1,
                            },
                            birth.unwrap_or(now),
                        ),
                    );
                }
                self.caret = to;
                self.caret_known = true;
            }
            Event::ReducedMotion(on) => self.reduced_motion = on,
            Event::Focus(false) => {
                self.echo.expire(now);
                self.echo.clear();
            }
            // Focus is the ribbon's and the sky's to act on (they ember and
            // die in their own `on_event`); a Return is the KEY, and inert;
            // a Sweep is the ribbon's alone.
            // The insert's rewrite moves the mirror (the next typed echo
            // from the new caret then has no unexplained gap) and nothing
            // else: no drain, no wince, the ledger kept — nothing is born.
            Event::Rewrite { row, col, .. } => {
                self.caret = (row, col);
                self.caret_known = true;
            }
            Event::Focus(true) | Event::Return | Event::Sweep { .. } => {}
        }
        self.events.push((ev, now));
    }

    /// **THE LATE ECHO'S CELLS** (2026-09-10, the "black gap" report) — what
    /// a licensed typed move owes the presses that came before it.
    ///
    /// The host licenses a caret move only within `0.25 s` of a keypress.
    /// A TUI whose render stalled echoes the key later than that; the seam
    /// refuses the move (`no-fresh-hint`, correctly — it cannot tell a late
    /// echo from program output), nothing reaches this engine, the mirror
    /// stays where the caret WAS, and the key's glyph cell is never laid.
    /// The next key's echo is licensed, sweeps only its own cell, and the
    /// ribbon restarts one cell to the right as a new cohort with a
    /// feathered edge: a dark cell inside the rainbow, permanent. Three keys
    /// typed into a stall leave three.
    ///
    /// This engine can tell the two apart, because it holds what the seam
    /// does not: every [`Event::Typed`] is a real press, banked on the
    /// [`EchoLedger`] until a caret advance pays for it, and every licensed
    /// move carries the host's `from` — the caret's LAST OBSERVED cell —
    /// beside the engine's own mirror, the last LICENSED one. When a licensed
    /// typed move lands on the mirror's row, forward, and `from` sits past
    /// the mirror, the caret advanced through an unlicensed hop; when the
    /// hop plus the move's own advance is paid for, cell by cell, by presses
    /// on the ledger, those cells are the presses' own glyphs and are laid —
    /// as one [`Event::Sweep`] from the mirror to the landing, on the
    /// licensed move's own clock (T2), joining the cohort they belong to so
    /// the walk continues with no seam and no feather. The same sweep covers
    /// a licensed multi-cell echo the host did not sweep itself — a
    /// non-coalesced re-anchor, or a batch its press credits (in flight for
    /// `CursorGlow::IN_FLIGHT_PATIENCE_S`, which [`timing::ECHO_PATIENCE_S`]
    /// is by alias) could not pay for; the refusal is logged `licensed`.
    ///
    /// **What it may never do.** T1 holds: geometry is born only on this
    /// observed, licensed move. The anti-stray law holds: a cell is laid only
    /// against a press, one for one, and only against a press that could
    /// have PRODUCED it. The host's sweep for the move rides on the clock of
    /// the press that licensed it (`typed_at`, the stamp the classifier
    /// popped); presses YOUNGER than that key are still in flight and their
    /// glyphs lie to the RIGHT of its cell, so the hole to its left may be
    /// paid only by presses OLDER than the key — and exactly, cell for cell.
    /// (Without that partition a program nudge followed by a fast burst
    /// would light the nudge's cell with the burst's presses: three presses,
    /// four cells.) A hop the older presses do not explain exactly — vim's
    /// `w`, a mouse click, program output that moved the caret, a swallowed
    /// key beside a real hole — buys nothing and FORGETS the ledger, so a
    /// swallowed press can never roll forward as a phantom credit either: the
    /// next ordinary echo finds it older than its key, unexplained, and drops
    /// it. A move the host did not sweep — a licensed batch its credits could
    /// not pay for — is laid from the oldest presses when it starts AT the
    /// mirror (a hop before it cannot be partitioned without a key clock, and
    /// is refused). The bound is the host's own coalesce cap
    /// ([`timing::ECHO_LEDGER_DEPTH`]). A press older than
    /// [`timing::ECHO_PATIENCE_S`] — the host's in-flight patience — was
    /// swallowed, not delayed, and buys nothing. A non-typed licence, an
    /// erase or kill, a focus loss, a row change, a scroll and a reset all
    /// clear the ledger: the presses it held no longer describe the row
    /// under the hand.
    ///
    /// A STALLED BATCH (2026-09-12) is swept by the host on the clock of its
    /// OLDEST press: nothing on the ledger is older than that key, the hop
    /// starts at the mirror, and the whole batch is SPENT — a batch the app
    /// drains across two frames keeps its tail here for the next key to
    /// bridge, instead of forfeiting it. The RIBBON is not born at that
    /// clock: see [`timing::SWEEP_BIRTH_FLOOR_S`] in the move handler.
    ///
    /// `key_at` is the host sweep's clock — the licensing press — when the
    /// host swept this move. Returns the first cell of a sweep that runs from
    /// there to the landing when the ledger paid for the move, or `None`. The
    /// sweep may cover the host's own (`from..to`) as well: the ribbon lays
    /// only what no live cell owns, so nothing is laid twice, and one sweep
    /// laid oldest column first is what keeps the cells in ONE cohort.
    fn echo_bridge(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        key_at: Option<Instant>,
        now: Instant,
    ) -> Option<u16> {
        self.echo.expire(now);
        let (mrow, mcol) = self.caret;
        let known = self.caret_known;
        let insert = licence == Licence::Insert;
        if !matches!(licence, Licence::Typed | Licence::Insert)
            || !known
            || from.0 != mrow
            || to.0 != mrow
        {
            self.echo.clear();
            return None;
        }
        if to.1 <= from.1 {
            // A retreat, a re-anchor, a rewrite landing left of its launch:
            // not an echo's shape. The presses keep waiting.
            return None;
        }
        let Some(gap) = from.1.checked_sub(mcol) else {
            // The caret went BACK without a licensed move and came forward
            // again: the hop is not the presses'. Forget them.
            self.echo.clear();
            return None;
        };
        let advance = usize::from(to.1 - from.1);
        let gap = usize::from(gap);
        if gap + advance > ECHO_LEDGER_DEPTH {
            self.echo.clear();
            return None;
        }
        let paid = match key_at {
            // A DELIVERED INSERT swept `from..to` on the delivery clock: the
            // span is the insert's own and charges the ledger nothing; the
            // hole before it — a key whose late echo the seam refused just
            // before the drop — is the older presses', exactly.
            Some(key) if insert => self.echo.older_than(key) == gap && gap <= self.echo.total(),
            // The host swept `from..to` for the key at `key_at`: the hole
            // before it is the older presses', exactly, and the sweep's own
            // cells are the key's and the ones after it.
            Some(key) => self.echo.older_than(key) == gap && advance <= self.echo.total() - gap,
            // No sweep, no key clock: only a move starting at the mirror can
            // be attributed, to the oldest presses.
            None => gap == 0 && advance <= self.echo.total(),
        };
        if !paid {
            self.echo.clear();
            return None;
        }
        self.echo.spend(if insert { gap } else { gap + advance });
        Some(mcol)
    }

    /// The clock of the host's sweep for the last observed move, when it
    /// sent one — the licensing key the ledger partitioned by.
    #[cfg(test)]
    fn last_host_sweep_at(&self) -> Option<Instant> {
        self.last_host_sweep_at
    }

    /// The focused pane's `(first column, width)`, or `None` for the whole
    /// grid — the edges a typed fold wraps at ([`Event::Sweep`]'s sibling
    /// finding, 2026-09-06: v2 folded at the GRID edge and lit a cell in the
    /// neighbouring pane). Stored whether or not the engine is engaged.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// **THE VERDICT** (sense 3) — a shell command came back GREEN after a
    /// long run, reported by the host on the OSC 133/633 `D` it already
    /// dedupes. Like [`Engine::earn_hero`] it is not a cursor event and so
    /// has no place in [`Event`]; unlike it, it mints nothing at all.
    ///
    /// **NO LIGHT IS BORN HERE.** THE LAWS: every light has a keystroke
    /// behind it, and a command finishing is the machine talking, not a hand.
    /// All this does is open [`spine::Spine::note_verdict`]'s door, so that
    /// the FIRST key you type inside [`spine::VERDICT_WINDOW_S`] is born at
    /// full momentum with the rainbow already at speed — the light rides that
    /// key, and if you never type one, nothing ever happened.
    ///
    /// Gated on `engaged` like every other seam point, so a session that
    /// never chose the theme pays exactly nothing for the shell's marks.
    pub fn note_verdict(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.spine.note_verdict(now);
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

    /// **THE PARTY** (§27): the host reports a sing-along bar (or the armed
    /// celebration's drop) — `n` stars fanned ROYGBIV from the caret, with
    /// the shockwave `ring` on every fourth bar — and the next tick mints it
    /// at the caret ([`meteor::Meteors::party`]). Not a cursor event, so not
    /// an [`Event`]; one per frame (a later call this frame wins); inert
    /// unless engaged. The light behind it is the keystroke that armed the
    /// sing-along — the held key, or the Enter that ran the green block.
    pub fn party(&mut self, now: Instant, n: u8, ring: bool) {
        if !self.engaged {
            return;
        }
        self.party = Some((now, n, ring));
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
            self.mend = None;
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
        // The cell a Backspace emptied is the caret as of ITS tick — the same
        // reading the ribbon retracts from and the sky throws from — so it is
        // resolved here, whichever order the host reported the erase and its
        // retreat in.
        if self.events.iter().any(|(ev, _)| matches!(ev, Event::Erase)) {
            self.erased = self.caret;
        }
        let mend = self.mend.take().map(|m| Mend {
            at: m.at,
            disp: m.disp,
            row: self.erased.0,
            col: self.erased.1,
            deletes: m.count,
        });
        // **THE FLOW RUN CLIMBS HERE** (§23's addendum "Flow state"), after the
        // one `Spine::update` and before anything reads the spine: the keys
        // this frame carries, priced at the number their own light is bought
        // with. Counting at the key's EDGE would price it by the last frame's
        // follower — and, after a silence the engine spent dark, by a
        // follower that has not moved for seconds.
        let keys = self
            .events
            .iter()
            .filter(|(ev, _)| matches!(ev, Event::Typed { .. }))
            .count();
        let price = self.spine.birth_disp();
        self.spine
            .note_keys(now, u32::try_from(keys).unwrap_or(u32::MAX), price);
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
            mend,
            surge: self.spine.surge(),
            flow: self.spine.flow(),
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
        // **THE COMBO LADDER'S TOP RUNG** (§23's addendum "Flow state"): every
        // 64th key AFTER the first gold hero fires the one-frame caret flare
        // §7.1 already owns — the ladder's own edge, on a keystroke, never
        // under reduced motion (no flash, exactly as a landing's is refused
        // there). The first 64th pays its gold m1 in the sky and does not
        // flash: the flare is what the ladder does once it has nothing left
        // to give.
        if keys > 0
            && !cfg_live.reduced_motion
            && ctx.flow.combo > stardust::LADDER_GOLD_KEYS
            && ctx.flow.combo.is_multiple_of(stardust::LADDER_GOLD_KEYS)
        {
            self.caret_seam.flare_at = Some(now);
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
            bridged: self.bridged,
            disp: self.spine.disp(),
            flow: ctx.flow,
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
            // one — nor on a delivered insert; if it ever did, nothing here
            // may fire — the keystroke's own cue is the host's.
            Licence::Typed | Licence::Insert => return,
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

    /// **THE RESIDENT PET'S WHOLE RECEIVING END** (panel #10, D13) — the perk
    /// edge, the star within reach, and the ribbon's colour under the cat, as
    /// ONE value.
    ///
    /// This is the call D13's wiring gap was waiting for. The perk offer has
    /// existed since the router was written ([`companion::BodyImpulse::Perk`])
    /// and nothing read it; here it is read, beside the two other things the
    /// pet can be offered without being driven. The host's whole receiving
    /// end is two lines, after its own pet tick:
    ///
    /// ```ignore
    /// let offer = v2.pet_offer(geom, companion::PetOnGlass::of(&pet_frame, geom));
    /// if let Some(star) = pet.note_v2_offer(&offer) { v2.catch_star(star, now); }
    /// ```
    ///
    /// **It is a pure read and it arms nothing.** No clock is started, no
    /// mark is minted, no cadence is held: every field is state a producer
    /// already owns this frame — the impulse slot, the sky's pool, the
    /// ribbon's field index — so an idle engine offers `PetOffer::default()`
    /// and T6 is untouched. The cost is one scan of the ≤ 48-slot star pool
    /// with two compares a star, and no allocation (§18). MEASURED on the
    /// capture (`examples/rainbow_kitty_v2_catch`, release, 200 000 calls):
    /// **5.11 ns/call** on the contented path that actually scans, and
    /// **3.33 ns/call** on an 18-star hot sky, where an uncontented cat
    /// short-circuits the scan before it starts.
    ///
    /// `pet` is the cat as its OWN last frame published it, or `None` when
    /// there is no pet on glass. v2 never derives a pet position: §7.2(b) is
    /// that the pet seats itself, and a router that guessed where it was
    /// would be a second answer to that question.
    #[must_use]
    pub fn pet_offer(&self, geom: Geom, pet: Option<companion::PetOnGlass>) -> companion::PetOffer {
        let perk_at = self.companion.and_then(|imp| {
            match companion::impulse_for(companion::Body::Pet, imp) {
                companion::BodyImpulse::Perk { at } => Some(at),
                _ => None,
            }
        });
        let Some(pet) = pet else {
            return companion::PetOffer {
                perk_at,
                ..companion::PetOffer::default()
            };
        };
        let (lo, hi) = pet.span();
        let mote_cell = ((lo + hi) * 0.5).floor().max(0.0) as u32;
        let mote_t = u16::try_from(mote_cell)
            .ok()
            .and_then(|col| self.ribbon.field_at(pet.row, col));
        // §5.3's 15 % gold crossed with §5.6's 1-in-12 m1 IS the panel's
        // "about one key in eighty"; the sky deals it on a keystroke and this
        // scan only NOTICES it. The NEWEST match wins — the panel's offer is
        // made to a star as it is born, and a cat that has been ignoring one
        // for 300 ms should be offered the fresh one instead.
        let catch = pet.contented().then(|| {
            self.stardust
                .live_iter()
                .filter(|s| {
                    s.gold
                        && s.class == stardust::StarClass::M1
                        && s.lane.is_sky()
                        && s.in_sky_of(pet.row, geom)
                        && pet.columns_to(s.grid_col(geom)) <= companion::CATCH_REACH_CELLS
                })
                .max_by_key(|s| s.born)
                .map(|s| companion::StarCatch {
                    x: s.x,
                    y: s.y,
                    born: s.born,
                    col: s.grid_col(geom),
                })
        });
        companion::PetOffer {
            perk_at,
            catch: catch.flatten(),
            mote_t,
            mote_rgb: mote_t.map(spectrum_snap),
        }
    }

    /// **THE CAT CATCHES THE STAR** (panel #10(b)) — spend the offered star's
    /// remaining life on the frame the paw lands, on the sky's own 40 ms
    /// finish ([`stardust::CATCH_FINISH_MS`]). Returns whether the star was
    /// still there to catch.
    ///
    /// `star` is the value [`Engine::pet_offer`] handed over, unchanged, and
    /// `at` is the PAW'S LANDING instant — the pet's own clock, not v2's, so
    /// the star dies on the frame the cat touches it rather than the frame v2
    /// noticed.
    ///
    /// **Exactly one life moves, and no light is born.** The star is found by
    /// identity and the finish is applied once ([`stardust::Stardust::catch`]);
    /// a stale offer, a dead star or a second call finds nothing to lengthen.
    /// And nothing is drawn: the whole of the catch is one star ending sooner,
    /// which is the only way the pet's own motion can touch the sky without
    /// becoming light itself (T1).
    pub fn catch_star(&mut self, star: companion::StarCatch, at: Instant) -> bool {
        self.stardust.catch(star.x, star.y, star.born, at)
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
        self.caret_known = false;
        self.echo.clear();
        self.bridged = 0;
        self.erased = (0, 0);
        self.mend = None;
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
        self.ribbon.translate_scroll(rows);
        self.meteor.translate_scroll(rows, cell_h);
        self.stardust.translate_scroll(rows, cell_h);
        // The caret is a POSITION, not a mark: it cannot be dropped, and the
        // host observes it again on its next move. The presses waiting for
        // their echo were on a row that just moved: forgotten, not shifted.
        self.caret.0 = self.caret.0.saturating_sub(rows);
        self.echo.clear();
    }

    /// **SEAM POINT 12** (`translate_band_state`) — the band twin of
    /// [`Engine::translate_scroll`]: screen rows `top..=bottom` moved by
    /// `delta` (the [`crate::cursor_glow::band_row`] law), so every live mark
    /// on those rows moves with its text — the ribbon's cells, cohorts and
    /// field index; the meteors, their landings and the sown fans; the stars
    /// and the veils — while a mark outside the band stands still and one
    /// carried past the band's edge is dropped, not clamped. The caret and
    /// the erased cell are POSITIONS ([`crate::cursor_glow::band_pos`]):
    /// saturated at the edge, re-observed by the host's next move. The
    /// earned-cell list is carried under the mark law (a hero owed on a row
    /// that left is owed nowhere).
    ///
    /// **The spine is untouched.** This is the whole point of the band path
    /// over the reset it replaces: Codex prints a line every 10–230 ms while
    /// the hand types into the composer that line slides, and the measured
    /// session restarted the momentum from zero on every line (`disp` 0.99
    /// → 0 → …). The hand did not stop; the program moved the paper. The
    /// buffered events are untouched too — only coordinate-free events
    /// (`Typed`, `Erase`, the nav tick) survive between presents by
    /// construction, and a `Move` is always dealt on the tick that observed
    /// it (T2). The glyph probe is dropped by the sky's half exactly as on a
    /// scroll: the host re-probes the caret's rows before the next deal.
    pub fn translate_band(
        &mut self,
        top: u16,
        bottom: u16,
        delta: i16,
        cell_h: u16,
        origin_y: u16,
    ) {
        if !self.engaged || delta == 0 || top > bottom {
            return;
        }
        // Banked presses have no row identity, so they cannot be translated
        // with this band. Retire them exactly as on a whole-view scroll;
        // located light, buffered keys and the spine keep their own state.
        self.echo.clear();
        self.ribbon.translate_band(top, bottom, delta);
        self.meteor
            .translate_band(top, bottom, delta, cell_h, origin_y);
        self.stardust
            .translate_band(top, bottom, delta, cell_h, origin_y);
        self.caret.0 = band_pos(self.caret.0, top, bottom, delta);
        self.erased.0 = band_pos(self.erased.0, top, bottom, delta);
        self.earned
            .retain_mut(|(cell, _)| match band_row(cell.0, top, bottom, delta) {
                Some(row) => {
                    cell.0 = row;
                    true
                }
                None => false,
            });
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

    /// One frame at `t`, discarding what it drew.
    fn tick_at(eng: &mut Engine, t: Instant) {
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t, geom(), &config(), &mut fr);
    }

    /// One key typed the ordinary way at `col` on row 3 with the caret
    /// standing on it: the press and its frame, then — 8 ms later, the shape
    /// every host echo takes — the host's sweep of the glyph cell and the
    /// licensed one-cell move, and their frame.
    fn type_key_at(eng: &mut Engine, t: Instant, col: u16) -> Instant {
        eng.on_event(typed(1), t);
        tick_at(eng, t);
        let echo = t + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: col,
                col1: col + 1,
            },
            t,
        );
        eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), echo);
        tick_at(eng, echo);
        echo
    }

    /// An engaged engine whose caret landed at `(3, 2)` by a licensed move and
    /// then typed the three cells `2..5` at a 100 ms cadence, so the caret
    /// stands at `(3, 5)` on a live three-cell cohort. Returns the clock
    /// after the last echo.
    fn three_typed_cells(eng: &mut Engine, t0: Instant) -> Instant {
        blank_row(eng, 2);
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(eng, t0);
        let mut t = t0;
        for col in 2..5u16 {
            t += Duration::from_millis(100);
            t = type_key_at(eng, t, col);
        }
        t
    }

    /// The field at `(3, col)`, or a panic naming the dark cell.
    fn lit(eng: &Engine, col: u16) -> f32 {
        eng.field_at(3, col)
            .unwrap_or_else(|| panic!("cell (3, {col}) is dark"))
    }

    fn echo_band_fence_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            EchoBandFence {
                const Buggy = 0;
                var pending = 1;
                var translated = 0;
                var done = 0;
                action Noop when (done == 0) { done = 1; }
                action Translate when (done == 0) {
                    done = 1;
                    translated = 1;
                    pending = if Buggy == 0 { 0 } else { pending };
                }
                invariant NoCreditAcrossBand: translated == 0 || pending == 0;
                invariant Bounds: pending <= 1 && translated <= 1 && done <= 1;
            }
        }
    }

    #[test]
    fn echo_band_fence_model_proves_and_catches_an_omitted_retirement() {
        let model = echo_band_fence_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Bind the small retirement contract to the genuine typed-press ledger
    /// and band transform. Geometry and earned momentum survive; unlocated
    /// pre-scroll credits cannot fund a later program nudge. The negative
    /// control restores only that old ledger after the SAME real transform.
    #[test]
    fn real_band_transforms_retire_echo_credits_without_resetting_earned_light() {
        let model = echo_band_fence_model();
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut negative_controls = 0;
        for (top, bottom, delta) in [
            (2, 10, 1),  // Codex composer moves down.
            (2, 10, -1), // Transcript moves up.
            (8, 10, 1),  // Composer is outside the changed band.
            (3, 3, 1),   // Old marks leave; the caret position saturates.
            (2, 10, 0),  // No movement: no fence.
            (10, 2, 1),  // Invalid band: no fence.
        ] {
            let valid = delta != 0 && top <= bottom;
            for restore_stale_credit in [false, true] {
                if restore_stale_credit && !valid {
                    continue;
                }
                let mut eng = engaged();
                let at = three_typed_cells(&mut eng, t0) + ms(100);
                assert_eq!(eng.echo.total(), 0, "the earlier keys were acknowledged");
                eng.on_event(typed(1), at);
                tick_at(&mut eng, at);
                assert_eq!(
                    eng.echo.total(),
                    1,
                    "one press is still waiting for its echo"
                );
                let stale = eng.echo;
                let momentum = eng.spine.momentum(at);
                let display = eng.momentum_display();
                assert!(momentum > 0.0 && display > 0.0, "earned light must be live");
                let field = eng.field_at(3, 4).expect("the acknowledged cohort is lit");

                eng.translate_band(top, bottom, delta, geom().ch as u16, 0);
                let action = if valid { "Translate" } else { "Noop" };
                let post = std::collections::BTreeMap::from([
                    ("pending", i64::from(eng.echo.total() > 0)),
                    ("translated", i64::from(valid)),
                    ("done", 1),
                ]);
                assert!(
                    model
                        .successors(action, &model.init_state())
                        .contains(&post)
                );
                assert!(model.check_invariant("NoCreditAcrossBand", &post));
                assert_eq!(
                    eng.spine.momentum(at),
                    momentum,
                    "scroll is not a key reset"
                );
                assert_eq!(
                    eng.momentum_display(),
                    display,
                    "the transform advances no clock"
                );
                let row = if valid {
                    band_pos(3, top, bottom, delta)
                } else {
                    3
                };
                assert_eq!(eng.caret, (row, 5));
                let mark = if valid {
                    band_row(3, top, bottom, delta)
                } else {
                    Some(3)
                };
                if let Some(mark_row) = mark {
                    assert_eq!(
                        eng.field_at(mark_row, 4),
                        Some(field),
                        "earned field moves with text"
                    );
                } else {
                    assert_eq!(
                        eng.field_at(3, 4),
                        None,
                        "departed light must not clamp to the edge"
                    );
                }
                if !valid {
                    continue;
                }
                if restore_stale_credit {
                    eng.echo = stale;
                    let mut omitted = post.clone();
                    omitted.insert("pending", i64::from(eng.echo.total() > 0));
                    assert!(
                        !model
                            .successors(action, &model.init_state())
                            .contains(&omitted)
                    );
                    assert!(!model.check_invariant("NoCreditAcrossBand", &omitted));
                    negative_controls += 1;
                }

                // The program nudged 5 -> 6 without a press. A NEW key at 6
                // has a genuine host sweep and advances 6 -> 7. An old credit
                // would wrongly fill column 5 as a late echo of the old row.
                let press = at + ms(300);
                eng.on_event(typed(1), press);
                tick_at(&mut eng, press);
                eng.on_event(
                    Event::Sweep {
                        row,
                        col0: 6,
                        col1: 7,
                    },
                    press,
                );
                eng.on_event(mv((row, 6), (row, 7), Licence::Typed), press + ms(8));
                tick_at(&mut eng, press + ms(8));
                assert_eq!(
                    eng.field_at(row, 5).is_some(),
                    restore_stale_credit,
                    "only the missing-fence control can paint the program's gap"
                );
                assert!(
                    eng.field_at(row, 6).is_some(),
                    "the new licensed key still paints"
                );
                assert_eq!(eng.status().bridged, u32::from(restore_stale_credit));
            }
        }
        assert_eq!(negative_controls, 4);
    }

    /// **A KEY WHOSE ECHO THE SEAM REFUSED IS RELIT BY THE NEXT LICENSED
    /// ECHO** ([`Engine::echo_bridge`], 2026-09-10 — the "black gap" report).
    /// Measured on the shipped binary: a key echoed 297 ms or more after its
    /// press is refused (`no-fresh-hint`), nothing reaches the engine, and
    /// its glyph cell was exactly the ground colour for good while the next
    /// key restarted the ribbon one cell right with a feathered edge. The
    /// next licensed echo carries `from` one past the engine's mirror; the
    /// press on the ledger pays for that one cell, and it joins the cohort it
    /// belongs to — the walk continues a sixteenth per cell straight through.
    /// The control is the same hop with no press behind it: program output
    /// moved the caret, and the ledger lays nothing for it (T1, anti-stray).
    #[test]
    fn a_key_whose_echo_the_seam_refused_is_relit_by_the_next_licensed_echo() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let drive = |pressed: bool| -> Engine {
            let mut eng = engaged();
            let t = three_typed_cells(&mut eng, t0);
            // Key #4 at col 5: its echo never comes back licensed. (In the
            // control nobody pressed it; the caret still moves.)
            let t = t + ms(100);
            if pressed {
                eng.on_event(typed(1), t);
                tick_at(&mut eng, t);
            }
            // Key #5 at col 6, 400 ms later; its echo is licensed 50 ms after
            // that, sweeping its own glyph, with `from` one past the mirror.
            let t = t + ms(400);
            eng.on_event(typed(1), t);
            tick_at(&mut eng, t);
            let echo = t + ms(50);
            eng.on_event(
                Event::Sweep {
                    row: 3,
                    col0: 6,
                    col1: 7,
                },
                t,
            );
            eng.on_event(mv((3, 6), (3, 7), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            eng
        };
        let relit = drive(true);
        let (t4, t5, t6) = (lit(&relit, 4), lit(&relit, 5), lit(&relit, 6));
        let step = 1.0 / ribbon::WALK_FAST_CELLS;
        assert!(
            (t5 - t4 - step).abs() < 1e-5 && (t6 - t5 - step).abs() < 1e-5,
            "the relit cell continues its cohort's walk: t4 {t4} t5 {t5} t6 {t6}"
        );
        assert_eq!(
            relit.status().bridged,
            1,
            "one cell relit, counted for `trail status`"
        );
        let control = drive(false);
        assert!(
            control.field_at(3, 5).is_none(),
            "a hop no press explains lays nothing — program output is not typing"
        );
        assert_eq!(
            control.status().bridged,
            0,
            "…and nothing was counted as repaired"
        );
        assert!(
            control.field_at(3, 6).is_some(),
            "…while the licensed key's own glyph is lit by the host's sweep"
        );
    }

    /// **THREE KEYS TYPED INTO A STALL ARE RELIT TOGETHER.** The shipped
    /// binary's measured shape: keys pressed 49 and 77 ms after a stalled
    /// key were echoed with it in ONE observed move, refused as a whole, and
    /// left a three-cell hole. The next licensed echo's `from` sits three
    /// past the mirror; three presses pay for three cells.
    #[test]
    fn three_keys_typed_into_a_stall_are_relit_together() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(100);
        for k in 0..3u64 {
            eng.on_event(typed(1), t + ms(40 * k));
            tick_at(&mut eng, t + ms(40 * k));
        }
        // Their batched echo (5 → 8) was refused: nothing arrives. Key #4.
        let t = t + ms(400);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(50);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 8,
                col1: 9,
            },
            t,
        );
        eng.on_event(mv((3, 8), (3, 9), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        let step = 1.0 / ribbon::WALK_FAST_CELLS;
        let mut prev = lit(&eng, 4);
        for col in 5..9u16 {
            let t = lit(&eng, col);
            assert!(
                (t - prev - step).abs() < 1e-5,
                "col {col} carries t {t}, not one step past col {}'s {prev}",
                col - 1
            );
            prev = t;
        }
        assert_eq!(
            eng.status().bridged,
            3,
            "three cells relit, counted for `trail status`"
        );
    }

    /// **A LICENSED ECHO THE HOST DID NOT SWEEP IS LAID FROM ITS PRESSES.**
    /// A two-cell echo the host licensed but did not sweep — a non-coalesced
    /// re-anchor, or a batch its press credits (2 s,
    /// `RAINBOW_COALESCE_CREDIT_LIFE`) could not pay for — is logged
    /// `licensed`, two dark cells. The engine's ledger keeps both presses for
    /// its own two seconds and lays both cells.
    #[test]
    fn a_licensed_echo_the_host_did_not_sweep_is_laid_from_its_presses() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let t = t + ms(700);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(50);
        eng.on_event(mv((3, 5), (3, 7), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 5).is_some() && eng.field_at(3, 6).is_some(),
            "both glyphs of the unswept echo are ribbon cells"
        );
        assert!(
            eng.field_at(3, 7).is_none(),
            "the landing's own cell is the caret's, not a glyph's"
        );
        assert_eq!(
            eng.status().bridged,
            2,
            "both unswept cells counted for `trail status`"
        );
    }

    /// **A STALE PRESS CANNOT FUND A LATER STRAY HOP** — the anti-stray law
    /// on the ledger. A refused press survives on the ledger, then the caret
    /// hops five cells with no press behind it (vim's `w`, a click): the hop
    /// is not the press's, buys nothing, and FORGETS the press. Later,
    /// program output nudges the caret one cell and the next key's echo
    /// lands one past the mirror: that one cell would be paid for by the
    /// stale press if it were still banked — it is not, and the cell stays
    /// dark.
    #[test]
    fn a_stale_press_cannot_fund_a_later_stray_hop() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The refused press at col 5.
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        // The unexplained hop 6 → 10, then a key at col 10.
        let t = t + ms(300);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 10,
                col1: 11,
            },
            t,
        );
        eng.on_event(mv((3, 10), (3, 11), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        for col in 5..10u16 {
            assert!(
                eng.field_at(3, col).is_none(),
                "col {col} of the skimmed span must stay dark"
            );
        }
        assert!(eng.field_at(3, 10).is_some(), "the key's own glyph is lit");
        // Program output moves the caret 11 → 12 (unlicensed); a key at 12.
        let t = echo + ms(300);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 12,
                col1: 13,
            },
            t,
        );
        eng.on_event(mv((3, 12), (3, 13), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 11).is_none(),
            "the program's cell must not be paid for by a press the hop already orphaned"
        );
        assert!(eng.field_at(3, 12).is_some());
    }

    /// **A PRESS OLDER THAN THE ECHO PATIENCE BUYS NO CELL** — a key the
    /// program swallowed is not a key whose echo is late.
    #[test]
    fn a_press_older_than_the_echo_patience_buys_no_cell() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let t = t + Duration::from_secs_f32(ECHO_PATIENCE_S) + ms(500);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(50);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            t,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 5).is_none(),
            "a swallowed key's cell is not relit two and a half seconds later"
        );
        assert!(eng.field_at(3, 6).is_some());
    }

    /// **A NAVIGATION LICENCE, A ROW CHANGE AND A RESET FORGET THE PRESSES.**
    /// Each is a move the presses no longer describe; after it, a one-cell
    /// unlicensed nudge followed by a licensed key must not be paid for by
    /// the press that was waiting before.
    #[test]
    fn a_navigation_licence_a_row_change_and_a_reset_forget_the_presses() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let after = |eng: &mut Engine, t: Instant, col: u16| {
            // The waiting press's cell (col − 1) went dark and the caret was
            // nudged there keylessly; the next key lands at `col`.
            eng.on_event(typed(1), t);
            tick_at(eng, t);
            let echo = t + ms(8);
            eng.on_event(
                Event::Sweep {
                    row: 3,
                    col0: col,
                    col1: col + 1,
                },
                t,
            );
            eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), echo);
            tick_at(eng, echo);
            assert_eq!(
                eng.status().bridged,
                0,
                "col {} must not be funded by a forgotten press",
                col - 1
            );
            assert!(eng.field_at(3, col).is_some());
        };
        // Navigation: press at 5 refused, an arrow hop 6 → 8 licensed Nav.
        // (The hop lays its own wake over the corridor it crossed — R5 —
        // so the dark-cell check belongs to the two arms below; here the
        // ledger's counter is the witness.)
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0) + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        eng.on_event(mv((3, 6), (3, 8), Licence::Nav), t + ms(300));
        tick_at(&mut eng, t + ms(300));
        after(&mut eng, t + ms(600), 9);
        // A row change: the same press, then a typed move onto row 4.
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0) + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        eng.on_event(mv((3, 6), (4, 2), Licence::Typed), t + ms(300));
        tick_at(&mut eng, t + ms(300));
        eng.on_event(typed(1), t + ms(600));
        tick_at(&mut eng, t + ms(600));
        let echo = t + ms(608);
        eng.on_event(
            Event::Sweep {
                row: 4,
                col0: 3,
                col1: 4,
            },
            t + ms(600),
        );
        eng.on_event(mv((4, 3), (4, 4), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(4, 2).is_none(),
            "a press banked on row 3 buys nothing on row 4"
        );
        // A reset: the press, then `reset`, then the caret learned afresh.
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0) + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        eng.reset();
        eng.on_event(mv((3, 0), (3, 6), Licence::Typed), t + ms(300));
        tick_at(&mut eng, t + ms(300));
        after(&mut eng, t + ms(600), 7);
        assert!(
            eng.field_at(3, 6).is_none(),
            "the nudge's cell after a reset stays dark"
        );
    }

    /// **PRESSES YOUNGER THAN THE LICENSING KEY NEVER PAY FOR A CELL TO ITS
    /// LEFT** — the adversary's break of the first ledger. Program output
    /// nudges the caret one cell (unlicensed, no press), then the user types
    /// three keys fast. The first key's echo lands one past the mirror with
    /// two presses still in flight behind it; those presses' glyphs are to
    /// the RIGHT, and counting them would light the program's cell — three
    /// presses, four cells. The host's sweep carries the licensing press's
    /// clock; only presses older than it may pay, and there are none.
    #[test]
    fn presses_younger_than_the_licensing_key_never_pay_for_a_cell_to_its_left() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The nudge 5 → 6 is refused at the seam: nothing arrives here.
        // Three keys at +0, +40, +80 ms, each echoed 8 ms after its press
        // at cols 6, 7, 8.
        let t = t + ms(300);
        for k in 0..3u64 {
            eng.on_event(typed(1), t + ms(40 * k));
            tick_at(&mut eng, t + ms(40 * k));
        }
        for k in 0..3u16 {
            let press = t + ms(40 * u64::from(k));
            let echo = press + ms(8);
            let col = 6 + k;
            eng.on_event(
                Event::Sweep {
                    row: 3,
                    col0: col,
                    col1: col + 1,
                },
                press,
            );
            eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), echo);
            tick_at(&mut eng, echo);
        }
        assert!(
            eng.field_at(3, 5).is_none(),
            "the program's cell must not be paid for by presses whose glyphs lie to its right"
        );
        for col in 6..9u16 {
            assert!(eng.field_at(3, col).is_some(), "col {col} is the keys' own");
        }
    }

    /// **A SWALLOWED PRESS NEVER ROLLS FORWARD AS A PHANTOM CREDIT.** A key
    /// the program swallowed leaves a press on the ledger; if every ordinary
    /// echo spent the OLDEST press, the phantom would ride along behind the
    /// hand for as long as the user typed, ready to pay for the first program
    /// nudge. Instead the next ordinary echo finds a press older than its own
    /// key with no hole to explain, and forgets it.
    #[test]
    fn a_swallowed_press_never_rolls_forward_as_a_phantom_credit() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The swallowed key: pressed, never echoed by anything.
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        // An ordinary key at col 5 — on time, swept by the host.
        let t = type_key_at(&mut eng, t + ms(200), 5);
        // Program output nudges the caret 6 → 7 (refused at the seam), then
        // a key at col 7 echoes one past the mirror.
        let t = t + ms(200);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 7,
                col1: 8,
            },
            t,
        );
        eng.on_event(mv((3, 7), (3, 8), Licence::Typed), t + ms(8));
        tick_at(&mut eng, t + ms(8));
        assert!(
            eng.field_at(3, 6).is_none(),
            "the swallowed press was dropped at the ordinary echo; the nudge's cell stays dark"
        );
        assert!(eng.field_at(3, 7).is_some());
    }

    /// **THE INSERT'S REWRITE** ([`Event::Rewrite`], 2026-09-10): the
    /// program pulled the caret back inside a delivered insert's span. It
    /// offers the kitty no impulse (no Wince), drains no spine, throws no
    /// star (the retracted cells' field stars finish with them), mints no
    /// cue, keeps the echo ledger, and moves the caret
    /// mirror — so the next typed echo from the new caret has no unexplained
    /// gap and bridges nothing — while the ribbon retracts the suffix.
    ///
    /// RED-PROOF (2026-09-10, the variant stubbed inert): fails at the
    /// caret-mirror assert (`(3, 5)` where `(3, 3)` is expected) — and the
    /// cells right of the new caret keep their light.
    #[test]
    fn a_rewrite_offers_no_impulse_drains_no_spine_throws_no_star_and_keeps_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let t = three_typed_cells(&mut eng, t0);
        // One press banked and not yet echoed: the ledger holds it.
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        assert_eq!(eng.echo.total(), 1, "PRECONDITION: one press on the ledger");
        let stars_before = eng.status().stars;
        let disp_before = eng.spine().disp();
        let t = t + ms(300);
        eng.on_event(
            Event::Rewrite {
                row: 3,
                col: 3,
                cells: 2,
            },
            t,
        );
        assert_eq!(
            eng.spine().disp(),
            disp_before,
            "a rewrite drains no spine at its edge"
        );
        assert_eq!(eng.caret, (3, 3), "the caret mirror moves to the rewrite");
        assert_eq!(eng.echo.total(), 1, "the ledger is kept");
        let mut fr = sc.frame();
        eng.tick(t, geom(), &cfg, &mut fr);
        assert!(fr.companion.is_none(), "no impulse on the rewrite's frame");
        assert!(eng.take_companion_impulse().is_none(), "no Wince");
        assert!(sc.cues.is_empty(), "a rewrite sounds nothing");
        assert!(
            eng.status().stars <= stars_before,
            "a rewrite throws no star (the retracted cells' field stars may finish): {} > {stars_before}",
            eng.status().stars
        );
        // The retract: cells 3 and 4 leave, cells 2 keeps its light.
        let done = t + ms((12 * 2 + 240) + 240 + 20);
        tick_at(&mut eng, done);
        assert!(
            eng.field_at(3, 3).is_none() && eng.field_at(3, 4).is_none(),
            "the cells right of the rewrite's caret are retracted"
        );
        // The next typed echo from the new caret: mirror (3,3), from (3,3) —
        // gap 0, nothing to bridge, the press pays for its own cell.
        let bridged = eng.status().bridged;
        let t = type_key_at(&mut eng, done + ms(50), 3);
        tick_at(&mut eng, t);
        assert_eq!(
            eng.status().bridged,
            bridged,
            "no gap opened, nothing bridged"
        );
        assert!(
            eng.field_at(3, 3).is_some(),
            "the key after the rewrite lays at the new caret"
        );
    }

    /// A rewrite's retract completes and the engine idles free — the §18
    /// idle → zero law with a [`Event::Rewrite`] in the stream.
    #[test]
    fn a_rewrite_retract_completes_and_the_engine_idles_free() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(300);
        eng.on_event(
            Event::Rewrite {
                row: 3,
                col: 2,
                cells: 3,
            },
            t,
        );
        tick_at(&mut eng, t);
        let mut last = t;
        for i in 1..=400u64 {
            last = t + ms(10 * i);
            tick_at(&mut eng, last);
        }
        let mut fr = sc.frame();
        eng.tick(last + ms(10), geom(), &cfg, &mut fr);
        assert_eq!(fr.fp, 0, "nothing on glass");
        assert!(!eng.needs_frame_cadence(), "no cadence asked");
        assert!(
            eng.next_change_deadline(last + ms(10)).is_none(),
            "no deadline"
        );
    }

    /// A HOST SWEEP UNDER A TYPED MOVE beside banked presses the hop does
    /// not explain: the whole span (`5..67`) laid as one sweep behind an
    /// inert typed move; the presses banked before it are forfeited by the
    /// ledger's mismatch rule and nothing is bridged — the anti-stray law
    /// the typed composition keeps. (The seam hands a delivered insert over
    /// as `Licence::Insert` instead, whose ledger rule pays such a hole from
    /// the older presses — `an_inserts_sweep_pays_the_hole_a_late_key_left_before_it`.)
    #[test]
    fn a_delivered_insert_sweep_beside_banked_presses_forfeits_them_and_bridges_nothing() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        eng.on_event(typed(1), t + ms(50));
        tick_at(&mut eng, t + ms(50));
        assert_eq!(eng.echo.total(), 2);
        let delivered = t + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 5,
                col1: 67,
            },
            delivered,
        );
        eng.on_event(mv((3, 5), (3, 67), Licence::Typed), delivered + ms(20));
        tick_at(&mut eng, delivered + ms(20));
        assert_eq!(eng.status().bridged, 0, "nothing bridged");
        assert_eq!(eng.echo.total(), 0, "the unexplained presses are forfeited");
        assert!(
            (5..67u16).all(|col| eng.field_at(3, col).is_some()),
            "the whole span is lit"
        );
        assert_eq!(eng.caret, (3, 67));
    }

    /// A DELIVERED INSERT'S SWEEP PAYS THE HOLE A LATE KEY LEFT BEFORE IT:
    /// a key pressed just before the drop, whose echo the seam refused (one
    /// press, its stamp stale — the shape `AStaleStampIsNotALicence` is
    /// right about), leaves the mirror one cell behind the insert's origin.
    /// Under `Licence::Insert` the ledger pays that one cell from the key's
    /// own press and charges the insert's span to nothing; under `Typed` the
    /// span would have to be covered by presses too, the mismatch would
    /// FORGET the ledger, and the key's cell would stay dark for good.
    #[test]
    fn an_inserts_sweep_pays_the_hole_a_late_key_left_before_it() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The late key: pressed, its echo refused at the seam (no move).
        let key = t + ms(100);
        eng.on_event(typed(1), key);
        tick_at(&mut eng, key);
        // The drop: delivered 400 ms later; the caret is observed at 6 (the
        // key's echo landed unlicensed) and the insert echoes 6 → 17.
        let delivered = key + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 17,
            },
            delivered,
        );
        eng.on_event(mv((3, 6), (3, 17), Licence::Insert), delivered + ms(20));
        tick_at(&mut eng, delivered + ms(20));
        assert!(
            eng.field_at(3, 5).is_some(),
            "the late key's cell is paid by its own press through the insert's sweep"
        );
        assert!(
            (6..17u16).all(|col| eng.field_at(3, col).is_some()),
            "the insert's span is lit"
        );
        assert_eq!(eng.status().bridged, 1, "one bridged cell — the key's");
        assert_eq!(eng.echo.total(), 0, "the press is spent");
        assert_eq!(eng.caret, (3, 17));
        // Control: the same shape under `Typed` forgets the ledger and
        // leaves the hole (the seam's typed sweep charges its span to presses).
        let mut typed_eng = engaged();
        let t = three_typed_cells(&mut typed_eng, t0);
        let key = t + ms(100);
        typed_eng.on_event(typed(1), key);
        tick_at(&mut typed_eng, key);
        let delivered = key + ms(400);
        typed_eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 17,
            },
            delivered,
        );
        typed_eng.on_event(mv((3, 6), (3, 17), Licence::Typed), delivered + ms(20));
        tick_at(&mut typed_eng, delivered + ms(20));
        assert!(typed_eng.field_at(3, 5).is_none());
        assert_eq!(typed_eng.echo.total(), 0, "…and the press is forfeited");
    }

    /// TIER 1 for the echo ledger: the REAL `Engine`, driven through the
    /// shapes `EchoLedgerBridge` names — a press, the echo the seam refused,
    /// the next key relighting it under the host's sweep on the PRESS's clock,
    /// the program-nudge-then-burst break, the swallowed-press phantom, the
    /// unswept batch, the fresh and the stale retreat, the navigation hop,
    /// vim's `w` (a typed re-anchor the host does not sweep), the patience
    /// whole and per press, and the three in-place clears — with every
    /// observed step validated as a transition of the model, and a forged
    /// step at each law the model must refuse.
    ///
    /// Tier 0 proves the law over the whole bounded space; this is the half
    /// that says the law is about THIS ENGINE. Every projected term is READ
    /// off the engine: the ledger's live buckets through its own partition by
    /// the licensing key's clock, its four tallies, its mirror against the
    /// host's caret (the hole), `Status::bridged`, and — asserted on every
    /// move — the clock the engine itself partitioned by
    /// (`last_host_sweep_at`), which must be the key's. The model's snapshot
    /// of the LAST move (`bridged_delta`, `laid_hole`, `last_older`,
    /// `just_refused`) is read across that move: `bridged_delta` is the
    /// engine's bridged delta, `laid_hole` is that delta less the cells an
    /// unswept move laid at and past `from` (the key's own, not the hole's),
    /// `last_older` the older bucket before the move. `stale_gone` is
    /// `expired + stale` by definition, so its STATE check is definitional;
    /// the law it carries (`ExpiredPressesBuyNothing`) is enforced PER
    /// TRANSITION — every bind requires the step to be the named action's,
    /// and a stale press spent rather than dropped is a successor no action
    /// admits (the forged stale-press-pays control below).
    #[test]
    fn the_real_engine_conforms_to_the_echo_ledger_model() {
        let model = aterm_spec::derive::echo_ledger_bridge_model();
        let ms = Duration::from_millis;
        let patience = Duration::from_secs_f32(ECHO_PATIENCE_S);
        type State = aterm_spec::interp::State;

        /// The model's snapshot of the last licensed move.
        #[derive(Clone, Copy, Default)]
        struct Last {
            laid_hole: i64,
            bridged_delta: i64,
            last_older: i64,
            just_refused: i64,
        }
        let n = |v: usize| i64::try_from(v).expect("bounded ledger");
        let project = |eng: &Engine, now: Instant, key: Instant, host_col: u16, last: Last| {
            let (older, younger, stale) = eng.echo.partition(key, now);
            let tally = eng.echo.tally;
            let mut st = model.init_state();
            st.insert("older", n(older));
            st.insert("younger", n(younger));
            st.insert("stale", n(stale));
            // The hole: the host's caret against the engine's mirror.
            st.insert("hole", i64::from(host_col - eng.caret.1));
            st.insert("banked", i64::from(tally.banked));
            st.insert("spent", i64::from(tally.spent));
            st.insert("forfeited", i64::from(tally.forfeited));
            st.insert("expired", i64::from(tally.expired));
            st.insert("stale_gone", i64::from(tally.expired) + n(stale));
            st.insert("bridged", i64::from(eng.status().bridged));
            st.insert("laid_hole", last.laid_hole);
            st.insert("bridged_delta", last.bridged_delta);
            st.insert("last_older", last.last_older);
            st.insert("just_refused", last.just_refused);
            st
        };
        let bind = |prev: &State, next: &State, action: &str, label: &str| {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                prev,
                next,
                Some(action),
                label,
            );
            assert!(ok, "{label}: real engine step rejected by the model: {why}");
        };
        let refuse = |prev: &State, next: &State, action: &str, label: &str| {
            let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                prev,
                next,
                Some(action),
                label,
            );
            assert!(!ok, "{label}: the model must refuse this step");
        };
        // An engine whose mirror was learned at `(3, col)` by a licensed move
        // — the seed is not a step, exactly as the licence twin's seed tick.
        let seeded = |t0: Instant, col: u16| -> Engine {
            let mut eng = engaged();
            eng.on_event(mv((3, 0), (3, col), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            eng
        };
        let press = |eng: &mut Engine, t: Instant| {
            eng.on_event(typed(1), t);
            tick_at(eng, t);
        };
        // A licensed move at `at` — under the host's sweep of `from..to` on
        // the KEY's clock when `key` is `Some`, unswept otherwise — read
        // across the engine: the clock it partitioned by must be the key's,
        // its bridged delta is the move's, and the hole part of that delta
        // (everything left of `from`) is what the model calls `laid_hole`.
        // `just_refused` is the step's shape, named like the action it binds.
        let moved = |eng: &mut Engine,
                     from: (u16, u16),
                     to: (u16, u16),
                     licence: Licence,
                     key: Option<Instant>,
                     at: Instant,
                     prev: &State,
                     just_refused: i64|
         -> Last {
            let before = eng.status().bridged;
            if let Some(key) = key {
                eng.on_event(
                    Event::Sweep {
                        row: to.0,
                        col0: from.1,
                        col1: to.1,
                    },
                    key,
                );
            }
            eng.on_event(mv(from, to, licence), at);
            tick_at(eng, at);
            assert_eq!(
                eng.last_host_sweep_at(),
                key,
                "the engine partitions by the host's sweep clock, the key's"
            );
            let delta = eng.status().bridged - before;
            let past_from = if key.is_some() {
                0
            } else {
                u32::from(to.1.saturating_sub(from.1))
            };
            Last {
                laid_hole: i64::from(delta.saturating_sub(past_from)),
                bridged_delta: i64::from(delta),
                last_older: prev["older"],
                just_refused,
            }
        };
        // An in-place clear at `at`: not a move, so the last move's snapshot
        // stands except for what the engine bridged across it (nothing).
        let cleared = |eng: &mut Engine, ev: Event, at: Instant, last: Last| -> Last {
            let before = eng.status().bridged;
            eng.on_event(ev, at);
            tick_at(eng, at);
            let delta = i64::from(eng.status().bridged - before);
            Last {
                laid_hole: delta,
                bridged_delta: delta,
                ..last
            }
        };

        // ---- THE SCREENSHOT: press, echo, a refused echo, and the relight ----
        let t0 = Instant::now();
        let mut eng = seeded(t0, 2);
        let mut last = Last::default();
        let s0 = project(&eng, t0, t0, 2, last);
        assert_eq!(
            s0,
            model.init_state(),
            "a seeded engine is the model's init"
        );

        // A press; the host will clock its sweep at it.
        let t1 = t0 + ms(100);
        press(&mut eng, t1);
        let s1 = project(&eng, t1, t1, 2, last);
        assert_eq!((s1["older"], s1["younger"], s1["banked"]), (0, 1, 1));
        bind(&s0, &s1, "KeyPressed", "Engine press");
        // T1: a keydown never pre-draws its cell.
        let mut predrawn = s1.clone();
        predrawn.insert("bridged", 1);
        refuse(
            &s0,
            &predrawn,
            "KeyPressed",
            "Engine keydown pre-draw control",
        );

        // Its echo, on time: the host sweeps the glyph, the ledger spends the
        // press, nothing is bridged (the per-key path is byte-identical).
        let e1 = t1 + ms(8);
        last = moved(
            &mut eng,
            (3, 2),
            (3, 3),
            Licence::Typed,
            Some(t1),
            e1,
            &s1,
            0,
        );
        let s2 = project(&eng, e1, t1, 3, last);
        assert_eq!((s2["spent"], s2["bridged"], s2["younger"]), (1, 0, 0));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&s1, &s2, "SweptMovePays", "Engine on-time echo");
        let mut reused = s2.clone();
        reused.insert("younger", 1);
        refuse(
            &s1,
            &reused,
            "SweptMovePays",
            "Engine one-press-one-cell control",
        );

        // The stalled key: pressed, echoed 300 ms later — refused at the seam,
        // nothing arrives here, and the caret stands one past the mirror.
        let t2 = e1 + ms(300);
        press(&mut eng, t2);
        let s3 = project(&eng, t2, t2, 3, last);
        bind(&s2, &s3, "KeyPressed", "Engine stalled press");
        let s4 = project(&eng, t2 + ms(300), t2, 4, last);
        assert_eq!(s4["hole"], 1, "the caret advanced with no licensed move");
        bind(&s3, &s4, "UnlicensedAdvance", "Engine refused echo");

        // The next key: the stalled press is now OLDER than the licensing key.
        let t3 = t2 + ms(400);
        press(&mut eng, t3);
        let s5 = project(&eng, t3, t3, 4, last);
        assert_eq!((s5["older"], s5["younger"], s5["hole"]), (1, 1, 1));
        bind(&s4, &s5, "KeyPressed", "Engine next key");

        // Its licensed echo, swept by the host on the key's clock: the hole
        // is exactly the older press's, laid and counted.
        let e3 = t3 + ms(8);
        last = moved(
            &mut eng,
            (3, 4),
            (3, 5),
            Licence::Typed,
            Some(t3),
            e3,
            &s5,
            0,
        );
        let s6 = project(&eng, e3, t3, 5, last);
        assert_eq!(s6["bridged"], 1, "the refused key's cell is relit");
        assert!(eng.field_at(3, 3).is_some(), "…and is a ribbon cell");
        assert_eq!(
            (last.laid_hole, last.bridged_delta, last.last_older),
            (1, 1, 1)
        );
        assert_eq!(
            (s6["spent"], s6["older"] + s6["younger"], s6["hole"]),
            (3, 0, 0)
        );
        bind(&s5, &s6, "SweptMovePays", "Engine relight");
        let mut overlaid = s6.clone();
        overlaid.insert("bridged", 2);
        overlaid.insert("bridged_delta", 2);
        overlaid.insert("laid_hole", 2);
        refuse(
            &s5,
            &overlaid,
            "SweptMovePays",
            "Engine one-older-press-two-cells control",
        );

        // ---- THE ADVERSARY'S BREAK: a program nudge, then a fast burst ----
        let u0 = Instant::now();
        let mut eng = seeded(u0, 5);
        let mut last = Last::default();
        let b0 = project(&eng, u0, u0, 5, last);
        // The nudge 5 -> 6, refused at the seam.
        let b1 = project(&eng, u0 + ms(100), u0, 6, last);
        bind(&b0, &b1, "UnlicensedAdvance", "Engine program nudge");
        // Two keys 40 ms apart; the first is the licensing key of the first
        // echo, the second is still in flight behind it.
        let u1 = u0 + ms(300);
        press(&mut eng, u1);
        let b2 = project(&eng, u1, u1, 6, last);
        bind(&b1, &b2, "KeyPressed", "Engine burst key 1");
        let u2 = u1 + ms(40);
        press(&mut eng, u2);
        let b3 = project(&eng, u2, u1, 6, last);
        assert_eq!((b3["older"], b3["younger"], b3["hole"]), (0, 2, 1));
        bind(&b2, &b3, "PressInFlight", "Engine burst key 2");
        // Key 1's echo lands one past the mirror: no press older than the key
        // explains the nudge, so nothing is laid and both presses are
        // forgotten — three presses never light four cells.
        let e1 = u2 + ms(8);
        last = moved(
            &mut eng,
            (3, 6),
            (3, 7),
            Licence::Typed,
            Some(u1),
            e1,
            &b3,
            1,
        );
        let b4 = project(&eng, e1, u1, 7, last);
        assert_eq!(b4["bridged"], 0, "the nudge's cell is not the burst's");
        assert!(eng.field_at(3, 5).is_none(), "…and stays dark");
        assert_eq!((b4["forfeited"], b4["older"] + b4["younger"]), (2, 0));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&b3, &b4, "SweptMoveRefuses", "Engine burst refusal");
        let mut kept = b4.clone();
        kept.insert("younger", 2);
        kept.insert("forfeited", 0);
        refuse(
            &b3,
            &kept,
            "SweptMoveRefuses",
            "Engine forfeited-credits control",
        );
        // …and a refusal that lays the hop's cell anyway.
        let mut laid_anyway = b4.clone();
        laid_anyway.insert("bridged", 1);
        laid_anyway.insert("bridged_delta", 1);
        refuse(
            &b3,
            &laid_anyway,
            "SweptMoveRefuses",
            "Engine no-bridge-on-a-refusal control",
        );
        // Key 2's echo: the ledger is empty, the host lit its own cell.
        let e2 = e1 + ms(8);
        last = moved(
            &mut eng,
            (3, 7),
            (3, 8),
            Licence::Typed,
            Some(u2),
            e2,
            &b4,
            1,
        );
        let b5 = project(&eng, e2, u2, 8, last);
        assert_eq!(b5["bridged"], 0);
        assert!(
            eng.field_at(3, 7).is_some(),
            "the key's own glyph is the host's"
        );
        bind(&b4, &b5, "SweptMoveRefuses", "Engine burst key 2 echo");

        // ---- THE PHANTOM: a swallowed press, an ordinary key, a nudge ----
        let v0 = Instant::now();
        let mut eng = seeded(v0, 5);
        let mut last = Last::default();
        let p0 = project(&eng, v0, v0, 5, last);
        let v1 = v0 + ms(100);
        press(&mut eng, v1);
        let p1 = project(&eng, v1, v1, 5, last);
        bind(&p0, &p1, "KeyPressed", "Engine swallowed press");
        // The ordinary key: the swallowed press is older than it, with no
        // hole to explain — the ordinary echo forgets it.
        let v2 = v1 + ms(200);
        press(&mut eng, v2);
        let p2 = project(&eng, v2, v2, 5, last);
        assert_eq!((p2["older"], p2["younger"], p2["hole"]), (1, 1, 0));
        bind(&p1, &p2, "KeyPressed", "Engine ordinary key");
        let e2 = v2 + ms(8);
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(v2),
            e2,
            &p2,
            1,
        );
        let p3 = project(&eng, e2, v2, 6, last);
        assert_eq!((p3["forfeited"], p3["older"] + p3["younger"]), (2, 0));
        bind(&p2, &p3, "SweptMoveRefuses", "Engine phantom dropped");
        // The nudge 6 -> 7 and a key at 7: nothing is left to pay for it.
        let p4 = project(&eng, e2 + ms(100), v2, 7, last);
        bind(
            &p3,
            &p4,
            "UnlicensedAdvance",
            "Engine nudge after the phantom",
        );
        let v3 = e2 + ms(200);
        press(&mut eng, v3);
        // A press banked after the refusal: the ledger may hold it.
        last.just_refused = 0;
        let p5 = project(&eng, v3, v3, 7, last);
        bind(&p4, &p5, "KeyPressed", "Engine key after the nudge");
        let e3 = v3 + ms(8);
        last = moved(
            &mut eng,
            (3, 7),
            (3, 8),
            Licence::Typed,
            Some(v3),
            e3,
            &p5,
            1,
        );
        let p6 = project(&eng, e3, v3, 8, last);
        assert_eq!(p6["bridged"], 0, "the phantom credit never rolled forward");
        assert!(eng.field_at(3, 6).is_none());
        bind(&p5, &p6, "SweptMoveRefuses", "Engine phantom cannot pay");

        // ---- THE RETREATS, THE NAVIGATION HOP AND THE IN-PLACE CLEARS ----
        let w0 = Instant::now();
        let mut eng = seeded(w0, 5);
        let mut last = Last::default();
        let r0 = project(&eng, w0, w0, 5, last);
        let w1 = w0 + ms(100);
        press(&mut eng, w1);
        let r1 = project(&eng, w1, w1, 5, last);
        bind(&r0, &r1, "KeyPressed", "Engine press before a retreat");
        let r2 = project(&eng, w1 + ms(100), w1, 6, last);
        bind(&r1, &r2, "UnlicensedAdvance", "Engine hop before a retreat");
        // A backward typed move re-anchors the mirror; the fresh press keeps
        // waiting.
        let w2 = w1 + ms(200);
        last = moved(&mut eng, (3, 6), (3, 4), Licence::Typed, None, w2, &r2, 0);
        let r3 = project(&eng, w2, w1, 4, last);
        assert_eq!((r3["younger"], r3["hole"], r3["forfeited"]), (1, 0, 0));
        bind(&r2, &r3, "RetreatKeepsPresses", "Engine fresh retreat");
        // An arrow / Home / End hop: a navigation licence. The press no
        // longer describes the row under the hand; it is forgotten.
        let w3 = w2 + ms(200);
        last = moved(&mut eng, (3, 4), (3, 9), Licence::Nav, None, w3, &r3, 0);
        let r4 = project(&eng, w3, w1, 9, last);
        assert_eq!((r4["older"] + r4["younger"], r4["forfeited"]), (0, 1));
        bind(&r3, &r4, "LedgerForgotten", "Engine nav hop");
        // A press that goes stale, then a backward typed move: the engine
        // expires BEFORE it looks at the shape, so the retreat drops it.
        let w4 = w3 + ms(100);
        press(&mut eng, w4);
        let r5 = project(&eng, w4, w4, 9, last);
        bind(
            &r4,
            &r5,
            "KeyPressed",
            "Engine press before a stale retreat",
        );
        let w5 = w4 + patience + ms(100);
        let r6 = project(&eng, w5, w4, 9, last);
        assert_eq!((r6["stale"], r6["younger"]), (1, 0));
        bind(&r5, &r6, "TimePasses", "Engine patience before a retreat");
        last = moved(&mut eng, (3, 9), (3, 8), Licence::Typed, None, w5, &r6, 0);
        let r7 = project(&eng, w5, w4, 8, last);
        assert_eq!(
            (r7["stale"], r7["expired"], r7["younger"], r7["hole"]),
            (0, 1, 0, 0)
        );
        bind(&r6, &r7, "RetreatKeepsPresses", "Engine stale retreat");

        // ---- THE IN-PLACE CLEARS: erase, kill, focus loss ----
        // Each expires BEFORE it clears (a stale press is dropped as
        // expired, a fresh one forfeited) and moves nothing: the mirror, and
        // so the hole, stand where they were.
        let d0 = Instant::now();
        let mut eng = seeded(d0, 8);
        let mut last = Last::default();
        let a0 = project(&eng, d0, d0, 8, last);
        let d1 = d0 + ms(100);
        press(&mut eng, d1);
        let a1 = project(&eng, d1, d1, 8, last);
        bind(&a0, &a1, "KeyPressed", "Engine press before an erase");
        let d2 = d1 + patience + ms(100);
        let a2 = project(&eng, d2, d1, 8, last);
        bind(&a1, &a2, "TimePasses", "Engine patience before an erase");
        last = cleared(&mut eng, Event::Erase, d2, last);
        let a3 = project(&eng, d2, d1, 8, last);
        assert_eq!((a3["stale"], a3["expired"], a3["forfeited"]), (0, 1, 0));
        bind(
            &a2,
            &a3,
            "LedgerClearedInPlace",
            "Engine erase over a stale press",
        );
        // A kill over a fresh press beside a hole: the press is forfeited and
        // the hole is UNTOUCHED — nothing moved the mirror.
        let d3 = d2 + ms(100);
        press(&mut eng, d3);
        let a4 = project(&eng, d3, d3, 8, last);
        bind(&a3, &a4, "KeyPressed", "Engine press before a kill");
        let a5 = project(&eng, d3 + ms(50), d3, 9, last);
        bind(&a4, &a5, "UnlicensedAdvance", "Engine hop before a kill");
        let d4 = d3 + ms(100);
        last = cleared(
            &mut eng,
            Event::Kill {
                cells: 1,
                scope: KillScope::Word,
            },
            d4,
            last,
        );
        let a6 = project(&eng, d4, d3, 9, last);
        assert_eq!((a6["younger"], a6["forfeited"], a6["hole"]), (0, 1, 1));
        bind(
            &a5,
            &a6,
            "LedgerClearedInPlace",
            "Engine kill beside a hole",
        );
        // A focus loss over a fresh press: forfeited, the hole still standing.
        let d5 = d4 + ms(100);
        press(&mut eng, d5);
        let a7 = project(&eng, d5, d5, 9, last);
        bind(&a6, &a7, "KeyPressed", "Engine press before a focus loss");
        let d6 = d5 + ms(100);
        last = cleared(&mut eng, Event::Focus(false), d6, last);
        let a8 = project(&eng, d6, d5, 9, last);
        assert_eq!((a8["forfeited"], a8["hole"]), (2, 1));
        bind(&a7, &a8, "LedgerClearedInPlace", "Engine focus loss");

        // ---- vim's `w`: a typed re-anchor the host does not sweep ----
        // In the real host a printable key beside a >2-cell same-row hop is a
        // typed re-anchor: it reaches the engine as `Licence::Typed` with NO
        // host sweep. One press cannot explain a three-cell hop; refused,
        // and the press is forfeited.
        let z0 = Instant::now();
        let mut eng = seeded(z0, 5);
        let mut last = Last::default();
        let c0 = project(&eng, z0, z0, 5, last);
        let z1 = z0 + ms(100);
        press(&mut eng, z1);
        let c1 = project(&eng, z1, z1, 5, last);
        bind(&c0, &c1, "KeyPressed", "Engine press before vim's w");
        let z2 = z1 + ms(50);
        last = moved(&mut eng, (3, 5), (3, 8), Licence::Typed, None, z2, &c1, 1);
        let c2 = project(&eng, z2, z1, 8, last);
        assert_eq!(
            (c2["forfeited"], c2["bridged"], c2["older"] + c2["younger"]),
            (1, 0, 0)
        );
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        bind(&c1, &c2, "UnsweptMoveRefuses", "Engine vim's w");

        // ---- THE PATIENCE: a press the program swallowed goes stale ----
        let x0 = Instant::now();
        let mut eng = seeded(x0, 5);
        let mut last = Last::default();
        let q0 = project(&eng, x0, x0, 5, last);
        let x1 = x0 + ms(100);
        press(&mut eng, x1);
        let q1 = project(&eng, x1, x1, 5, last);
        bind(&q0, &q1, "KeyPressed", "Engine press before the patience");
        let x2 = x1 + patience + ms(500);
        let q2 = project(&eng, x2, x1, 5, last);
        assert_eq!((q2["stale"], q2["younger"], q2["stale_gone"]), (1, 0, 1));
        bind(&q1, &q2, "TimePasses", "Engine patience elapses");
        let q3 = project(&eng, x2, x1, 6, last);
        bind(
            &q2,
            &q3,
            "UnlicensedAdvance",
            "Engine hop after the patience",
        );
        press(&mut eng, x2);
        let q4 = project(&eng, x2, x2, 6, last);
        assert_eq!((q4["older"], q4["younger"], q4["stale"]), (0, 1, 1));
        bind(&q3, &q4, "KeyPressed", "Engine key after the patience");
        // The echo: the stale press is dropped, not spent; nothing older than
        // the key explains the hole; the key is forfeited with the refusal.
        let e2 = x2 + ms(8);
        last = moved(
            &mut eng,
            (3, 6),
            (3, 7),
            Licence::Typed,
            Some(x2),
            e2,
            &q4,
            1,
        );
        let q5 = project(&eng, e2, x2, 7, last);
        assert_eq!((q5["expired"], q5["forfeited"], q5["bridged"]), (1, 1, 0));
        assert!(eng.field_at(3, 5).is_none(), "a swallowed key buys no cell");
        bind(&q4, &q5, "SweptMoveRefuses", "Engine expired press");

        // ---- THE PARTIAL EXPIRY: the older press stale, the key fresh ----
        let f0 = Instant::now();
        let mut eng = seeded(f0, 5);
        let mut last = Last::default();
        let g0 = project(&eng, f0, f0, 5, last);
        let f1 = f0 + ms(100);
        press(&mut eng, f1);
        let g1 = project(&eng, f1, f1, 5, last);
        bind(
            &g0,
            &g1,
            "KeyPressed",
            "Engine press before a partial expiry",
        );
        let f2 = f1 + ms(1500);
        press(&mut eng, f2);
        let g2 = project(&eng, f2, f2, 5, last);
        assert_eq!((g2["older"], g2["younger"], g2["stale"]), (1, 1, 0));
        bind(&g1, &g2, "KeyPressed", "Engine key before a partial expiry");
        // 2.1 s after the first press: it is stale, the key is not.
        let f3 = f1 + patience + ms(100);
        let g3 = project(&eng, f3, f2, 5, last);
        assert_eq!((g3["older"], g3["younger"], g3["stale"]), (0, 1, 1));
        bind(
            &g2,
            &g3,
            "OnePressGoesStale",
            "Engine oldest press goes stale",
        );
        // The key's echo, swept on its clock: the stale press is dropped and
        // the key pays its own cell.
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(f2),
            f3,
            &g3,
            0,
        );
        let g4 = project(&eng, f3, f2, 6, last);
        assert_eq!(
            (
                g4["expired"],
                g4["stale"],
                g4["younger"],
                g4["spent"],
                g4["bridged"]
            ),
            (1, 0, 0, 1, 0)
        );
        bind(
            &g3,
            &g4,
            "SweptMovePays",
            "Engine echo after a partial expiry",
        );

        // ---- THE STALE PRESS THAT MUST NOT PAY ----
        // After a press has gone stale, a key with no hop: the swept echo's
        // honest successor drops the stale press and spends the key. The
        // forged successor spends the stale press and keeps the key waiting.
        let h0 = Instant::now();
        let mut eng = seeded(h0, 5);
        let mut last = Last::default();
        let k0 = project(&eng, h0, h0, 5, last);
        let h1 = h0 + ms(100);
        press(&mut eng, h1);
        let k1 = project(&eng, h1, h1, 5, last);
        bind(&k0, &k1, "KeyPressed", "Engine press that will go stale");
        let h2 = h1 + patience + ms(100);
        let k2 = project(&eng, h2, h1, 5, last);
        bind(&k1, &k2, "TimePasses", "Engine press gone stale");
        press(&mut eng, h2);
        let k3 = project(&eng, h2, h2, 5, last);
        assert_eq!(
            (k3["older"], k3["younger"], k3["stale"], k3["hole"]),
            (0, 1, 1, 0)
        );
        bind(&k2, &k3, "KeyPressed", "Engine key beside a stale press");
        let e3 = h2 + ms(8);
        last = moved(
            &mut eng,
            (3, 5),
            (3, 6),
            Licence::Typed,
            Some(h2),
            e3,
            &k3,
            0,
        );
        let k4 = project(&eng, e3, h2, 6, last);
        assert_eq!(
            (k4["expired"], k4["stale"], k4["younger"], k4["spent"]),
            (1, 0, 0, 1)
        );
        bind(
            &k3,
            &k4,
            "SweptMovePays",
            "Engine key pays beside a stale press",
        );
        let mut stale_paid = k4.clone();
        stale_paid.insert("expired", 0);
        stale_paid.insert("younger", 1);
        stale_paid.insert("spent", 1);
        refuse(
            &k3,
            &stale_paid,
            "SweptMovePays",
            "Engine stale-press-pays control",
        );

        // ---- THE UNSWEPT BATCH: two presses, a two-cell licensed echo ----
        let y0 = Instant::now();
        let mut eng = seeded(y0, 5);
        let mut last = Last::default();
        let m0 = project(&eng, y0, y0, 5, last);
        let y1 = y0 + ms(100);
        press(&mut eng, y1);
        let m1 = project(&eng, y1, y1, 5, last);
        bind(&m0, &m1, "KeyPressed", "Engine batch press 1");
        press(&mut eng, y1 + ms(700));
        let m2 = project(&eng, y1 + ms(700), y1, 5, last);
        bind(&m1, &m2, "PressInFlight", "Engine batch press 2");
        // The host licensed the echo but did not sweep it (a non-coalesced
        // re-anchor, or a batch its credits could not pay for): the engine
        // lays both cells from the presses.
        let y2 = y1 + ms(750);
        last = moved(&mut eng, (3, 5), (3, 7), Licence::Typed, None, y2, &m2, 0);
        let m3 = project(&eng, y2, y1, 7, last);
        assert_eq!(
            (m3["bridged"], m3["spent"], m3["older"] + m3["younger"]),
            (2, 2, 0)
        );
        assert_eq!(
            (last.laid_hole, last.bridged_delta),
            (0, 2),
            "both cells are the key's and the one behind it, none the hole's"
        );
        assert!(eng.field_at(3, 5).is_some() && eng.field_at(3, 6).is_some());
        bind(&m2, &m3, "UnsweptMovePays", "Engine unswept batch");
        // The same batch after a hop: no key clock to partition by — refused.
        let y3 = y2 + ms(200);
        press(&mut eng, y3);
        let m4 = project(&eng, y3, y3, 7, last);
        bind(&m3, &m4, "KeyPressed", "Engine batch press 3");
        press(&mut eng, y3 + ms(100));
        let m5 = project(&eng, y3 + ms(100), y3, 7, last);
        bind(&m4, &m5, "PressInFlight", "Engine batch press 4");
        let m6 = project(&eng, y3 + ms(120), y3, 8, last);
        bind(&m5, &m6, "UnlicensedAdvance", "Engine hop before a batch");
        let y4 = y3 + ms(150);
        last = moved(&mut eng, (3, 8), (3, 10), Licence::Typed, None, y4, &m6, 1);
        let m7 = project(&eng, y4, y3, 10, last);
        assert_eq!((m7["bridged"], m7["forfeited"]), (2, 2));
        assert_eq!((last.laid_hole, last.bridged_delta), (0, 0));
        assert!(eng.field_at(3, 7).is_none(), "the hop's cell stays dark");
        bind(
            &m6,
            &m7,
            "UnsweptMoveRefuses",
            "Engine unswept batch after a hop",
        );
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

    /// One 120 Hz tick — the design's reference cadence (§18).
    const TICK: Duration = Duration::from_micros(8_333);

    /// Drive `eng` through the tick indices `ticks` at 120 Hz on `t0`'s
    /// clock, reporting the events scheduled on a tick BEFORE that tick, as
    /// the host's seam does (T2: an event and its frame share a `now`).
    fn drive_ticks(
        eng: &mut Engine,
        sc: &mut Scratch,
        t0: Instant,
        ticks: std::ops::RangeInclusive<u64>,
        sched: &[(u64, Event)],
    ) {
        let cfg = config();
        for i in ticks {
            let now = t0 + TICK * u32::try_from(i).expect("a short script");
            for (_, ev) in sched.iter().filter(|(at, _)| *at == i) {
                eng.on_event(*ev, now);
            }
            let mut fr = sc.frame();
            eng.tick(now, geom(), &cfg, &mut fr);
        }
    }

    // -- §23's addendum "Flow state": the counter, the entry, the exits ----

    /// Type `keys` keys on row 3 from column 4 at `period_ms`, ticking the
    /// engine at 120 Hz exactly as the host does, and return one row per key:
    /// `(seconds since the first key, the flow after that key's frame)`.
    fn flow_run(
        eng: &mut Engine,
        sc: &mut Scratch,
        t0: Instant,
        keys: u32,
        period_ms: u64,
    ) -> Vec<(f32, Flow)> {
        let cfg = config();
        let period = Duration::from_millis(period_ms);
        let mut out = Vec::new();
        let mut t = t0;
        for (col, k) in (4u16..).zip(1..=keys) {
            let key = t0 + period * k;
            while t + TICK <= key {
                t += TICK;
                let mut fr = sc.frame();
                eng.tick(t, geom(), &cfg, &mut fr);
            }
            t = key;
            eng.on_event(mv((3, col), (3, col + 1), Licence::Typed), t);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            out.push((
                key.saturating_duration_since(t0 + period).as_secs_f32(),
                eng.status().flow,
            ));
        }
        out
    }

    /// **A RUN OF FAST CLEAN KEYS OPENS THE THEME, AND A DELETE CLOSES IT**
    /// (§23's addendum "Flow state") — the whole law of the counter, on the
    /// real engine at the two cadences the design quotes.
    ///
    /// The run counts only keys born at [`spine::FLOW_KEY_DISP`] or above, so
    /// the climb to the floor does not count and the entry lands
    /// [`spine::FLOW_ENTRY_KEYS`] keys after the hand is at speed. `heat` is
    /// EXACTLY zero for the first twenty of those keys — every consumer is
    /// identity there — eases over the last four, and is exactly 1.0 from the
    /// 24th on. One Backspace takes the combo and the heat to zero on the
    /// same edge, and leaves the high-water mark standing.
    #[test]
    fn a_run_of_fast_clean_keys_opens_the_theme_and_a_delete_closes_it() {
        let cfg = config();
        for (label, period_ms) in [("12 cps", 83u64), ("8 cps", 125)] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let rows = flow_run(&mut eng, &mut sc, t0, 44, period_ms);

            let entry = rows
                .iter()
                .position(|(_, f)| f.combo == spine::FLOW_ENTRY_KEYS)
                .expect("a clean run at speed must reach the entry");
            let first_counted = rows
                .iter()
                .position(|(_, f)| f.combo == 1)
                .expect("some key must be the first at speed");
            eprintln!(
                "{label}: key {} is the first at disp >= {} ({:.3} s); the {}th counted key — key {} — opens the theme at {:.3} s, {:.3} s after the run began",
                first_counted + 1,
                spine::FLOW_KEY_DISP,
                rows[first_counted].0,
                spine::FLOW_ENTRY_KEYS,
                entry + 1,
                rows[entry].0,
                rows[entry].0 - rows[first_counted].0
            );

            // The ease: zero below, monotone through, exactly one at and after.
            let mut last = 0.0f32;
            for (_, f) in &rows {
                assert!(
                    f.heat >= last - 1e-6 || f.combo == 0,
                    "the heat fell inside an unbroken run"
                );
                last = f.heat;
                if f.combo <= spine::FLOW_ENTRY_KEYS - spine::FLOW_OPEN_EASE_KEYS {
                    assert_eq!(
                        f.heat, 0.0,
                        "{label}: combo {} must be EXACTLY identity",
                        f.combo
                    );
                } else if f.combo >= spine::FLOW_ENTRY_KEYS {
                    assert_eq!(f.heat, 1.0, "{label}: combo {} must be open", f.combo);
                } else {
                    assert!(
                        f.heat > 0.0 && f.heat < 1.0,
                        "{label}: combo {} must be easing, got {}",
                        f.combo,
                        f.heat
                    );
                }
                assert!(f.best >= f.combo);
            }
            let held = eng.status().flow;
            assert!(held.open(), "{label}: the run must still be open");

            // …AND A DELETE CLOSES IT, on the delete's own edge.
            let t = t0 + Duration::from_millis(period_ms) * 45;
            eng.on_event(Event::Erase, t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            let after = eng.status().flow;
            assert_eq!(after.combo, 0, "{label}: a delete must break the run");
            assert_eq!(after.heat, 0.0, "{label}: …and close the theme");
            assert_eq!(
                after.best, held.best,
                "{label}: a broken run does not un-earn the run already held"
            );

            // A KILL is the second exit, and a key born under the floor the
            // third: type two keys back (the run climbs), then kill.
            let mut t2 = t;
            for k in 1..=2u32 {
                t2 = t + Duration::from_millis(period_ms) * k;
                eng.on_event(typed(1), t2);
                let mut fr = sc.frame();
                eng.tick(t2, geom(), &cfg, &mut fr);
            }
            assert!(eng.status().flow.combo > 0, "{label}: the run restarted");
            t2 += Duration::from_millis(period_ms);
            eng.on_event(
                Event::Kill {
                    cells: 4,
                    scope: KillScope::Word,
                },
                t2,
            );
            let mut fr = sc.frame();
            eng.tick(t2, geom(), &cfg, &mut fr);
            assert_eq!(
                eng.status().flow.combo,
                0,
                "{label}: a kill must break the run"
            );

            // The third exit: a key after a long silence is born cold, and a
            // cold key breaks the run rather than extending it.
            let cold = t2 + Duration::from_secs(30);
            eng.on_event(typed(1), cold);
            let mut fr = sc.frame();
            eng.tick(cold, geom(), &cfg, &mut fr);
            assert_eq!(
                eng.status().flow.combo,
                0,
                "{label}: a key born under the floor may not count"
            );
        }
    }

    /// **FLOW DRAWS NOTHING OF ITS OWN** (the first law of the feature) — it
    /// re-prices births that each ride their own key, so with the hand off
    /// the keys an OPEN theme is byte-identical to a cold one: nothing
    /// written, fingerprint exactly zero, no cadence asked for and no
    /// deadline named — `rainbow_kitty_v2_idles_at_exactly_zero`'s own
    /// assertions, made again while `heat` is a full 1.0.
    ///
    /// The heat is still 1.0 through all of it: the run is unbroken (no
    /// delete, no kill, no key), which is exactly the state an agent holds
    /// its turn on. It buys no light because there is no keystroke to buy it
    /// with.
    #[test]
    fn flow_draws_nothing_of_its_own() {
        let cfg = config();
        let t0 = Instant::now();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let rows = flow_run(&mut eng, &mut sc, t0, 44, 83);
        assert!(
            rows.last().expect("keys").1.open(),
            "the fixture must reach flow first"
        );

        // The hand lifts. Run out to five seconds of silence: everything the
        // keys bought retires, and the frames go to the idle frame.
        let last_key = t0 + Duration::from_millis(83) * 44;
        let mut dark_from = None;
        let mut t = last_key;
        while t < last_key + Duration::from_secs(5) {
            t += TICK;
            sc.under.clear();
            sc.out.clear();
            sc.halos.clear();
            sc.cues.clear();
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            let fp = fr.fp;
            let empty = sc.under.is_empty()
                && sc.out.is_empty()
                && sc.halos.is_empty()
                && sc.cues.is_empty();
            match dark_from {
                None if fp == 0 && empty => dark_from = Some(t),
                None => {}
                Some(from) => {
                    assert_eq!(
                        fp,
                        0,
                        "an OPEN theme lit a frame with no keystroke behind it, {:.3} s after the glass went dark",
                        t.saturating_duration_since(from).as_secs_f32()
                    );
                    assert!(empty, "an OPEN theme wrote a quad with no key behind it");
                    assert!(
                        !eng.needs_frame_cadence(),
                        "an OPEN theme held the host awake"
                    );
                }
            }
        }
        let dark = dark_from.expect("the glass must go dark inside five seconds");
        eprintln!(
            "the glass went dark {:.3} s after the last key; {:.3} s of open-theme idle wrote nothing at all",
            dark.saturating_duration_since(last_key).as_secs_f32(),
            t.saturating_duration_since(dark).as_secs_f32()
        );
        let held = eng.status().flow;
        assert!(
            held.open() && held.heat == 1.0,
            "the run is unbroken through the silence — that is what an agent holds its turn on: {held:?}"
        );

        // THE TWIN, at heat 0 on the SAME events: the combo is held at zero by
        // a celebration drive whose floor is under the live metric, so
        // `Spine::drive` changes not one bit of the spine and the only
        // difference between the two engines is the run. Once both have gone
        // dark their answers are the same answer — the cadence AND the
        // deadline the host schedules on.
        let t1 = Instant::now();
        let mut cold = engaged();
        let mut sc1 = Scratch::default();
        {
            let period = Duration::from_millis(83);
            let mut t = t1;
            for (col, k) in (4u16..).zip(1..=44u32) {
                let key = t1 + period * k;
                while t + TICK <= key {
                    t += TICK;
                    let mut fr = sc1.frame();
                    cold.tick(t, geom(), &cfg, &mut fr);
                    cold.celebrate(t, f32::MIN_POSITIVE);
                }
                t = key;
                cold.on_event(mv((3, col), (3, col + 1), Licence::Typed), t);
                cold.on_event(typed(1), t);
                cold.celebrate(t, f32::MIN_POSITIVE);
                let mut fr = sc1.frame();
                cold.tick(t, geom(), &cfg, &mut fr);
            }
        }
        assert_eq!(
            cold.status().flow.combo,
            0,
            "the twin must be the same hand with the run frozen shut"
        );
        assert!(
            (cold.momentum_display() - eng.momentum_display()).abs() < 1e-6
                || cold.momentum_display() > 0.0,
            "the freeze must not have moved the spine"
        );
        let mut t = t1 + Duration::from_millis(83) * 44;
        let end = t + Duration::from_secs(5);
        while t < end {
            t += TICK;
            let mut fr = sc1.frame();
            cold.tick(t, geom(), &cfg, &mut fr);
        }
        assert_eq!(
            eng.needs_frame_cadence(),
            cold.needs_frame_cadence(),
            "an open theme asks for a cadence a closed one does not"
        );
        assert_eq!(
            eng.next_change_deadline(last_key + Duration::from_secs(5))
                .map(|d| d.saturating_duration_since(last_key + Duration::from_secs(5))),
            cold.next_change_deadline(end)
                .map(|d| d.saturating_duration_since(end)),
            "an open theme names a deadline a closed one does not"
        );
        assert_eq!(
            eng.fingerprint(),
            cold.fingerprint(),
            "the dark frames differ"
        );
    }

    /// The live (not retracting) ribbon cell at `(row, col)`.
    fn live_cell(eng: &Engine, row: u16, col: u16) -> Option<ribbon::Cell> {
        eng.ribbon
            .cells()
            .iter()
            .find(|c| c.row == row && c.col == col && c.retract_at.is_none())
            .copied()
    }

    /// THE TYPO SCRIPT (§23's addendum "The mend"): eight keys at 8 cps from
    /// column 4 of row 3 — every 15th tick — then a Backspace one key-slot
    /// later, then the fix key one slot after that. Returns the schedule and
    /// the ticks of the erase and the fix.
    fn typo_script(
        erase_after: u64,
        deletes: u16,
        fix_after: u64,
    ) -> (Vec<(u64, Event)>, u64, u64) {
        let mut out = Vec::new();
        let mut col = 4u16;
        for k in 0..8u64 {
            out.push((15 * k, mv((3, col), (3, col + 1), Licence::Typed)));
            out.push((15 * k, typed(1)));
            col += 1;
        }
        let erase = 15 * 7 + erase_after;
        let mut last_erase = erase;
        for d in 0..deletes {
            last_erase = erase + 15 * u64::from(d);
            out.push((last_erase, Event::Erase));
            out.push((last_erase, mv((3, col), (3, col - 1), Licence::Typed)));
            col -= 1;
        }
        let fix = last_erase + fix_after;
        out.push((fix, mv((3, col), (3, col + 1), Licence::Typed)));
        out.push((fix, typed(1)));
        (out, last_erase, fix)
    }

    /// **THE MEND** (§23's addendum "The mend"): the typo is the commonest
    /// momentum-killer, and the owner's law says momentum resumes, it does
    /// not start over. Eight keys at 8 cps, a Backspace, the fix key — one
    /// slot later, 500 ms later, and after a 250 ms pause with two deletes:
    /// the fix cell is born at the momentum the deleting INTERRUPTED (the
    /// birth spine as it stood on the frame before the first Backspace), so
    /// its `cov0` and life match the run it re-joins and the bed shows no
    /// dip where the fix went in — never below its left neighbour, never
    /// below the momentum it interrupted. Meanwhile the erased cell's own
    /// retract starts on the erase edge exactly as before (T5), the fix key
    /// owns its light on its own frame (frame-0), and the mark is spent by
    /// the fix (the next key is priced live).
    ///
    /// Fails on the tree before the mend, where the fix was priced at the
    /// live `birth_disp` the delete had drained: "one-slot fix: born at
    /// 0.7083, the delete interrupted 0.7335 — a 0.0252 dip of the birth
    /// spine (3.4 %)"; the 500 ms fix reads 0.6444 vs 0.7335 (12.1 %) and
    /// the two-delete case 0.6618 vs 0.7286 (9.2 %).
    #[test]
    fn a_typo_fixed_within_a_breath_is_born_at_the_momentum_it_interrupted() {
        for (label, erase_after, deletes, fix_after) in [
            ("one-slot fix", 15u64, 1u16, 15u64),
            ("500 ms fix", 15, 1, 60),
            ("250 ms pause, two deletes", 30, 2, 15),
        ] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let (sched, erase, fix) = typo_script(erase_after, deletes, fix_after);
            let first_erase = erase - 15 * (u64::from(deletes) - 1);
            drive_ticks(&mut eng, &mut sc, t0, 0..=first_erase - 1, &sched);
            let interrupted = eng.spine().birth_disp();
            drive_ticks(&mut eng, &mut sc, t0, first_erase..=erase, &sched);
            let fix_col = 12 - deletes;
            assert!(
                live_cell(&eng, 3, fix_col).is_none(),
                "{label}: the erased cell is retracting on the erase frame (T5)"
            );
            let retracting = eng
                .ribbon
                .cells()
                .iter()
                .find(|c| c.row == 3 && c.col == fix_col)
                .copied()
                .expect("the erased cell is still on glass, retracting");
            assert_eq!(
                retracting.retract_at,
                Some(t0 + TICK * u32::try_from(erase).expect("short")),
                "{label}: the erased cell's retract starts on the erase edge, as before"
            );
            assert_eq!(
                eng.erased,
                (3, fix_col),
                "{label}: the erased cell is known"
            );

            drive_ticks(&mut eng, &mut sc, t0, erase + 1..=fix, &sched);
            let fixed =
                live_cell(&eng, 3, fix_col).expect("the fix key owns its light on its own frame");
            let left = live_cell(&eng, 3, fix_col - 1).expect("the neighbour is still laid");
            assert_eq!(
                fixed.born,
                t0 + TICK * u32::try_from(fix).expect("short"),
                "{label}: frame-0 — the fix cell is born on the fix key's frame"
            );
            assert!(
                fixed.birth_disp >= interrupted - 1e-4,
                "{label}: born at {:.4}, the delete interrupted {interrupted:.4} — a {:.4} dip of the birth spine ({:.1} %); live birth_disp {:.4}",
                fixed.birth_disp,
                interrupted - fixed.birth_disp,
                (interrupted - fixed.birth_disp) / interrupted * 100.0,
                eng.spine().birth_disp()
            );
            assert!(
                fixed.cov0 >= left.cov0 && fixed.life_s >= left.life_s,
                "{label}: the fix ({:.4}, {:.3} s) is not below its left neighbour ({:.4}, {:.3} s)",
                fixed.cov0,
                fixed.life_s,
                left.cov0,
                left.life_s
            );
            assert!(
                eng.mend.is_none() && eng.spine().erase_mark().is_none(),
                "{label}: the fix spends the mark"
            );
        }
    }

    /// The mend's two edges: a typed key 2 s after the Backspace, or after
    /// THREE deletes, is not a fix — it is priced at the live `birth_disp`,
    /// below the momentum the deleting interrupted, and the mark (still
    /// there as a value, with its count) is not a mend when the key reads it.
    /// Both halves hold on the tree before as a matter of arithmetic (there
    /// was no mend); what they pin is the boundary the mend does not cross.
    #[test]
    fn a_slow_correction_is_not_a_mend() {
        for (label, deletes, fix_after) in [("2 s later", 1u16, 240u64), ("three deletes", 3, 15)] {
            let t0 = Instant::now();
            let mut eng = engaged();
            let mut sc = Scratch::default();
            let (sched, erase, fix) = typo_script(15, deletes, fix_after);
            let first_erase = erase - 15 * (u64::from(deletes) - 1);
            drive_ticks(&mut eng, &mut sc, t0, 0..=first_erase - 1, &sched);
            let interrupted = eng.spine().birth_disp();
            drive_ticks(&mut eng, &mut sc, t0, first_erase..=fix - 1, &sched);
            let t_fix = t0 + TICK * u32::try_from(fix).expect("short");
            let mark = eng
                .spine()
                .erase_mark()
                .expect("the mark is a value, still there");
            assert_eq!(mark.count, u8::try_from(deletes).expect("small"));
            assert_eq!(
                eng.spine().mend(t_fix),
                None,
                "{label}: the key that comes now is not a fix"
            );
            drive_ticks(&mut eng, &mut sc, t0, fix..=fix, &sched);
            let fix_col = 12 - deletes;
            let fixed = live_cell(&eng, 3, fix_col).expect("the key still lays its cell");
            let live = eng.spine().birth_disp();
            assert!(
                (fixed.birth_disp - live).abs() < 1e-6,
                "{label}: priced live — born at {:.4}, live birth_disp {live:.4}",
                fixed.birth_disp
            );
            assert!(
                fixed.birth_disp < interrupted,
                "{label}: and the live price ({:.4}) is below the momentum interrupted ({interrupted:.4}) — the honest re-earn",
                fixed.birth_disp
            );
            assert!(
                eng.spine().erase_mark().is_none(),
                "{label}: the key closes the run"
            );
        }
    }

    /// **A VALUE, NOT A TIMER** (T6 at the mark): one key, one Backspace,
    /// and the glass goes dark well inside the mend window — and while the
    /// mend is still LIVE the engine asks for no cadence, names no deadline
    /// and fingerprints zero; when the window closes nothing wakes (no tick
    /// after the dark one ever names a deadline), the mark is still there as
    /// a value, and it is simply not a mend any more. And a key typed over
    /// the dark glass inside the window IS mended: born at the mark, above
    /// the live price. Does not compile on the tree before (no mark).
    #[test]
    fn the_mend_mark_is_a_value_not_a_timer() {
        let t0 = Instant::now();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let sched = [
            (0, mv((3, 4), (3, 5), Licence::Typed)),
            (0, typed(1)),
            (15, Event::Erase),
            (15, mv((3, 5), (3, 4), Licence::Typed)),
        ];
        let t_erase = t0 + TICK * 15;
        let window = Duration::from_secs_f32(spine::MEND_WINDOW_S);
        drive_ticks(&mut eng, &mut sc, t0, 0..=15, &sched);
        let mark = eng
            .spine()
            .erase_mark()
            .expect("the Backspace leaves its mark");

        // Tick on until the glass is dark.
        let mut i = 15u64;
        let dark = loop {
            i += 1;
            let now = t0 + TICK * u32::try_from(i).expect("short");
            drive_ticks(&mut eng, &mut sc, t0, i..=i, &sched);
            if eng.fingerprint() == 0 && eng.next_change_deadline(now).is_none() {
                break i;
            }
            assert!(
                now < t_erase + window,
                "the precondition: a lone erased key must go dark inside the mend window"
            );
        };
        let t_dark = t0 + TICK * u32::try_from(dark).expect("short");
        assert_eq!(
            eng.spine().mend(t_dark),
            Some(mark),
            "the mend is still live over the dark glass"
        );
        assert!(!eng.needs_frame_cadence(), "…and asks for no cadence");
        assert_eq!(
            eng.next_change_deadline(t_dark),
            None,
            "…and names no deadline"
        );

        // A key over the dark glass, inside the window, is mended.
        let mut mended = eng.clone();
        let mut sc2 = Scratch::default();
        let fix = dark + 1;
        let fix_sched = [(fix, mv((3, 4), (3, 5), Licence::Typed)), (fix, typed(1))];
        drive_ticks(&mut mended, &mut sc2, t0, fix..=fix, &fix_sched);
        let cell = live_cell(&mended, 3, 4).expect("the key lays its cell");
        let live = mended
            .spine()
            .disp()
            .max(spine::DISP_PEAK_FLOOR_SHARE * mended.spine().disp_peak());
        assert!(
            (cell.birth_disp - mark.disp).abs() < 1e-6 && mark.disp > live + 0.01,
            "born at {:.4}: the mark {:.4}, over a live price of {live:.4}",
            cell.birth_disp,
            mark.disp
        );

        // No key: the window closes and nothing wakes.
        let past = 15 + (window.as_micros() as u64 + 100_000).div_ceil(TICK.as_micros() as u64);
        for j in dark + 1..=past {
            let now = t0 + TICK * u32::try_from(j).expect("short");
            drive_ticks(&mut eng, &mut sc, t0, j..=j, &sched);
            assert_eq!(eng.fingerprint(), 0, "idle → zero, mark or no mark");
            assert!(!eng.needs_frame_cadence());
            assert_eq!(
                eng.next_change_deadline(now),
                None,
                "a pending mark names no deadline (tick {j})"
            );
        }
        let t_past = t0 + TICK * u32::try_from(past).expect("short");
        assert_eq!(
            eng.spine().erase_mark(),
            Some(mark),
            "the mark is a value: still there, unchanged, after the window"
        );
        assert_eq!(eng.spine().mend(t_past), None, "…and not a mend any more");
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
    /// **SEAM POINT 12, the band path** — a band move is a COORDINATE
    /// TRANSFORM and never a reset: six cells typed on row 3 and the caret
    /// at (3, 6) ride Codex's viewport `[2..10]` down to row 4 with the field
    /// intact (`field_at(4, 5)` answers what `field_at(3, 5)` did, and the
    /// vacated cell answers nothing), the erased cell and the earned heroes
    /// move with their rows, and THE SPINE IS UNTOUCHED: the next frame reads
    /// the momentum the hand earned, not a restart — the measured Codex
    /// session read `disp 0.99 → 0` on every streamed line under the reset
    /// this replaces, and the control below is that reset. A band outside
    /// the caret's row leaves everything alone; a band the caret's row
    /// leaves saturates the caret at its edge and drops the light.
    #[test]
    fn a_band_move_moves_the_caret_with_its_band_and_keeps_the_spine() {
        let t0 = Instant::now();
        let cfg = config();
        let g = geom();
        let ch = g.ch as u16;
        let ms = Duration::from_millis;
        let lay = |eng: &mut Engine| {
            for row in 2..=4 {
                blank_row(eng, row);
            }
            let mut sc = Scratch::default();
            // Six keys at 12 cps, so the spine has something to keep.
            for k in 0..6u16 {
                let at = t0 + ms(80 * u64::from(k));
                eng.on_event(mv((3, k), (3, k + 1), Licence::Typed), at);
                eng.on_event(typed(1), at);
                let mut fr = sc.frame();
                eng.tick(at, g, &cfg, &mut fr);
            }
        };
        let mut eng = engaged();
        lay(&mut eng);
        let t_last = t0 + ms(400);
        let disp_before = eng.status().disp;
        assert!(disp_before > 0.0, "fixture: the hand earned momentum");
        let field_before = eng.field_at(3, 5).expect("fixture: the cell is laid");
        assert_eq!(eng.caret, (3, 6));
        eng.erased = (3, 2);
        eng.earned.push(((3, 4), t_last));
        eng.earned.push(((10, 4), t_last));
        eng.earned.push(((20, 4), t_last));

        eng.translate_band(2, 10, 1, ch, 0);

        assert_eq!(eng.caret, (4, 6), "the caret rides its band");
        assert_eq!(eng.erased, (4, 2), "so does the erased cell");
        assert_eq!(
            eng.earned.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            vec![(4, 4), (20, 4)],
            "a hero owed inside the band moves, one outside it stays, one pushed past the edge is owed nowhere"
        );
        assert_eq!(
            eng.field_at(4, 5),
            Some(field_before),
            "the field moved with its cell"
        );
        assert_eq!(eng.field_at(3, 5), None, "the vacated cell answers nothing");
        assert_eq!(eng.status().disp, disp_before, "a transform ticks nothing");
        let t1 = t_last + ms(8);
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(t1, g, &cfg, &mut fr);
        let disp_after = eng.status().disp;
        assert!(
            disp_after > 0.0 && (disp_after - disp_before).abs() < 0.05,
            "the spine kept going across the band move: {disp_before} → {disp_after}"
        );
        assert!(
            eng.ribbon_segments() >= 1,
            "the ribbon is lit on the moved row"
        );

        // CONTROL: the reset this path replaces restarts the momentum.
        let mut reset = engaged();
        lay(&mut reset);
        reset.reset();
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        reset.tick(t1, g, &cfg, &mut fr);
        assert_eq!(
            reset.status().disp,
            0.0,
            "the reset is what the owner saw: 0.99 → 0"
        );

        // A band elsewhere on the screen touches nothing.
        eng.translate_band(20, 30, 1, ch, 0);
        assert_eq!(eng.caret, (4, 6));
        assert_eq!(eng.field_at(4, 5), Some(field_before));
        // A band the caret's row LEAVES: the position saturates at the edge,
        // the light is gone.
        eng.translate_band(2, 4, 1, ch, 0);
        assert_eq!(
            eng.caret,
            (4, 6),
            "a position saturates; it cannot be dropped"
        );
        assert_eq!(eng.field_at(4, 5), None);
        assert_eq!(eng.field_at(5, 5), None, "nothing is parked past the edge");
        // Disengaged or degenerate: a no-op.
        let mut idle = Engine::new();
        idle.caret = (3, 3);
        idle.translate_band(2, 10, 1, ch, 0);
        assert_eq!(idle.caret, (3, 3));
        eng.translate_band(5, 4, 1, ch, 0);
        eng.translate_band(2, 10, 0, ch, 0);
        assert_eq!(eng.caret, (4, 6));
    }

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

    // -----------------------------------------------------------------------
    // PANEL #10 — THE PET AND THE SKY (D13's wiring gap, and two offers more)
    // -----------------------------------------------------------------------

    /// A resident pet as its own frame publishes it: six cells wide, feet on
    /// `row`, standing with its left edge at `col`.
    fn cat(row: u16, col: f32, settled: bool, purr: f32) -> companion::PetOnGlass {
        companion::PetOnGlass {
            col,
            width: 6.0,
            row,
            settled,
            purr,
            contented: false,
        }
    }

    /// **PANEL #10(a) — D13'S WIRING GAP, CLOSED.** `BodyImpulse::Perk { at }`
    /// has been minted for every credited meteor since the router was written
    /// and nothing has ever read it. `Engine::pet_offer` reads it, and the
    /// instant it hands the resident is the LANDING's, not the launch's: the
    /// same `Instant` the landing pin was born on, the same one the arrival
    /// flash, the caret's flare-and-relight and the audio bell share (§6.7,
    /// §8.1 no. 3). A settled pet perks exactly there — and is not moved.
    #[test]
    fn the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        // A 40-cell licensed nav jump: a meteor by every measure (§6.1).
        eng.on_event(mv((3, 0), (3, 40), Licence::Nav), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let imp = fr.companion.expect("the spawn frame carries the impulse");

        // What the PIXELS committed to on this frame.
        assert_eq!(eng.meteor.live.len(), 1, "the jump must have flown");
        let arrival = eng.meteor.live[0].arrival();
        let flight = arrival.saturating_duration_since(t0);
        assert!(
            (60..=120).contains(&flight.as_millis()),
            "fixture: T must be a real flight, got {flight:?}"
        );

        // What the ROUTER hands the resident, through the one published call.
        let offer = eng.pet_offer(geom(), Some(cat(3, 12.0, true, 0.4)));
        assert_eq!(
            offer.perk_at,
            Some(arrival),
            "the perk edge must be the meteor's own arrival edge"
        );
        assert_ne!(
            offer.perk_at,
            Some(t0),
            "the perk is the LANDING's instant, never the launch's"
        );

        // …and it is the landing pin's own instant, on the frame the pin is
        // born. One `Instant`, four marks (§6.7).
        let mut t = t0;
        let mut pinned = None;
        for _ in 0..16 {
            t += Duration::from_millis(8);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
            if let Some(l) = eng.meteor.landings.first() {
                pinned = Some(l.pin.at);
                break;
            }
        }
        assert_eq!(
            pinned,
            Some(arrival),
            "the landing pin is born on the arrival edge"
        );
        assert_eq!(
            offer.perk_at, pinned,
            "the pet's perk and the landing pin must be the SAME instant"
        );
        assert!(
            eng.caret_seam.flare_at.is_some() || eng.paint_at.is_some(),
            "fixture: the caret's own edge is live for the same landing"
        );

        // The offer is the ROUTER's answer, not a second one: the same
        // impulse, routed with `Body::Pet`, must say the same thing — and
        // must still refuse to relocate the cat (open question 14).
        assert_eq!(
            companion::impulse_for(companion::Body::Pet, imp),
            companion::BodyImpulse::Perk { at: arrival }
        );

        // A pet-less host is still told the edge (the offer is about the
        // meteor, not about where the cat is standing) …
        assert_eq!(eng.pet_offer(geom(), None).perk_at, Some(arrival));

        // … and REDUCED MOTION offers no edge at all: there was no flight to
        // wait out, so the impulse is a `Land` pose and not an instant.
        let mut slow = engaged();
        let mut sc2 = Scratch::default();
        let reduced = Config {
            reduced_motion: true,
            ..config()
        };
        slow.on_event(mv((3, 0), (3, 40), Licence::Nav), t0);
        let mut fr2 = sc2.frame();
        slow.tick(t0, geom(), &reduced, &mut fr2);
        assert!(matches!(fr2.companion, Some(CompanionImpulse::Land)));
        assert_eq!(
            slow.pet_offer(geom(), Some(cat(3, 12.0, true, 0.4)))
                .perk_at,
            None,
            "a landing with no flight behind it is a pose, not an edge"
        );
    }

    /// **PANEL #10(b) — THE CAT CATCHES A STAR.** A gold m1 — the sky's
    /// 1-in-12 m1 crossed with §5.3's 15 % gold, about one key in eighty —
    /// born within [`companion::CATCH_REACH_CELLS`] of a SETTLED, CONTENTED
    /// cat is OFFERED to it; on the paw's landing frame that one star's life
    /// is spent on the sky's own 40 ms finish. The star is still
    /// keystroke-born (T1: the offer notices a star, it never mints one), the
    /// cat's reaching draws nothing, and EXACTLY ONE life moves.
    #[test]
    fn a_gold_star_near_a_contented_cat_is_offered_and_caught() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;
        blank_row(&mut eng, 2);

        // Earn heroes along row 3 until the deal hands out a GOLD one. The
        // deal is deterministic (`cell_hash(row, col, minted)`), so this is a
        // fixed search, not a die roll.
        let mut gold_col = None;
        for col in (6u16..90).step_by(4) {
            let at = t0 + ms(u64::from(col));
            blank_row(&mut eng, 2);
            eng.earn_hero(at, 3, col);
            let mut fr = sc.frame();
            eng.tick(at, geom(), &cfg, &mut fr);
            if eng
                .stardust
                .live_iter()
                .any(|s| s.gold && s.class == stardust::StarClass::M1)
            {
                gold_col = Some((col, at));
                break;
            }
        }
        let (col, born_at) = gold_col.expect("fixture: the deal must produce a gold m1");
        let stars_before = eng.status().stars;
        assert!(stars_before >= 1);

        // THE OFFER. A settled, purring cat six cells wide, its nose within
        // ten columns of the star.
        let near = cat(3, f32::from(col) + 4.0, true, 0.4);
        let offer = eng.pet_offer(geom(), Some(near));
        let star = offer.catch.expect("a gold m1 in reach is offered");
        assert_eq!(star.born, born_at, "the offer names the star's own edge");
        assert!(
            near.columns_to(star.col) <= companion::CATCH_REACH_CELLS,
            "the offered star must be inside the reach it was offered under"
        );

        // THE GATES. A cat mid-pounce is busy; a cold cat is not contented;
        // and eleven columns is not ten.
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 4.0, false, 0.4)))
                .catch,
            None,
            "a cat that is not settled is offered nothing"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 4.0, true, 0.0)))
                .catch,
            None,
            "a cat that is not purring is offered nothing"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(3, f32::from(col) + 24.0, true, 0.4)))
                .catch,
            None,
            "a star out of reach is not offered"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(9, f32::from(col) + 4.0, true, 0.4)))
                .catch,
            None,
            "a paw reaches along its own line, not three lines up"
        );

        // THE CATCH. The paw lands 200 ms after the star was born — well
        // inside an m1's 460 ms sky life, so the shortening is real.
        let paw = born_at + ms(200);
        let others: Vec<(f32, f32, Option<stardust::Finish>)> = eng
            .stardust
            .live_iter()
            .filter(|s| s.born != star.born || s.x != star.x)
            .map(|s| (s.x, s.y, s.finish))
            .collect();
        let deadline_before = eng.next_change_deadline(paw);
        assert!(eng.catch_star(star, paw), "the offered star must be there");

        // Exactly one life moved …
        let caught = eng
            .stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x && s.y == star.y)
            .expect("the caught star is still in the pool on the paw's frame");
        assert_eq!(
            caught.finish,
            Some(stardust::Finish {
                at: paw,
                span_s: stardust::CATCH_FINISH_MS / 1000.0,
            }),
            "the catch spends the star on the sky's own 40 ms finish"
        );
        for (x, y, finish) in &others {
            let s = eng
                .stardust
                .live_iter()
                .find(|s| s.x == *x && s.y == *y)
                .expect("a bystander must not leave the pool");
            assert_eq!(s.finish, *finish, "a bystander's life was shortened too");
        }

        // … the star is gone inside a breath, and it did not pop …
        assert!(
            !caught.dead(paw + ms(10)),
            "the catch is a finish, not a pop"
        );
        assert!(
            caught.dead(paw + ms(40)),
            "the caught star must be off the glass within 40 ms"
        );

        // … NO LIGHT WAS BORN (T1: the pet's own motion is not light) …
        let (q, h, cues) = (sc.out.len(), sc.halos.len(), sc.cues.len());
        let mut fr = sc.frame();
        eng.tick(paw, geom(), &cfg, &mut fr);
        assert!(
            eng.status().stars <= stars_before,
            "the catch minted a star"
        );
        assert_eq!(sc.cues.len(), cues, "the catch minted a sound");
        assert!(sc.out.len() >= q && sc.halos.len() >= h, "scratch shrank");

        // … and NO WAKE was bought: a life that ends sooner can only bring
        // the sky's next deadline forward, never push it out.
        if let (Some(before), Some(after)) = (deadline_before, eng.next_change_deadline(paw)) {
            assert!(
                after <= before,
                "the catch may not ask for a later frame than the sky already wanted"
            );
        }

        // A SECOND PAW on a spent offer finds nothing to lengthen.
        let again = eng.catch_star(star, paw + ms(300));
        let still = eng
            .stardust
            .live_iter()
            .find(|s| s.born == star.born && s.x == star.x);
        assert!(
            !again || still.is_none_or(|s| s.finish.is_some_and(|f| f.at == paw)),
            "a second catch must never give the star more time"
        );
    }

    /// **PANEL #10(c) — C2, EXTENDED TO THE CAT.** A contented resident's ♪
    /// and ♥ take the ribbon's own field at the cell the cat is standing on,
    /// so the notes wear the rainbow at the cat's position exactly as the
    /// caret, the ribbon head and a star's halo on a cell do (C2). The value
    /// is SNAPPED (C1: a mote is a point mark), and where no ribbon light is
    /// laid under the cat the offer is `None` and the pet keeps its own
    /// colour.
    #[test]
    fn the_purr_notes_wear_the_ribbon_s_colour_under_the_cat() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;

        // Lay a ribbon along row 3 by typing, and land the caret on it.
        blank_row(&mut eng, 2);
        for k in 0..24u16 {
            let t = t0 + ms(u64::from(k) * 40);
            eng.on_event(mv((3, k), (3, k + 1), Licence::Typed), t);
            eng.on_event(typed(1), t);
            let mut fr = sc.frame();
            eng.tick(t, geom(), &cfg, &mut fr);
        }
        assert!(eng.status().cells > 0, "fixture: the ribbon must be laid");

        // The cat stands with its body over the ribbon; the offer's hue is
        // the field at the cell under its centre.
        let cat_col = 8.0_f32;
        let centre = (cat_col + 3.0) as u16;
        let laid = eng
            .field_at(3, centre)
            .expect("fixture: the cell under the cat must be lit");
        let offer = eng.pet_offer(geom(), Some(cat(3, cat_col, true, 0.4)));
        assert_eq!(
            offer.mote_t,
            Some(laid),
            "the notes take the field at the cell under the cat"
        );
        assert_eq!(
            offer.mote_rgb,
            Some(crate::spectrum::spectrum_snap(laid)),
            "a mote is a point mark: C1 snaps it to one of the seven stops"
        );
        assert!(
            offer
                .mote_rgb
                .is_some_and(|rgb| (0..crate::spectrum::SPECTRUM_STOPS)
                    .any(|i| crate::spectrum::spectrum_stop(i) == rgb)),
            "the note's colour must BE a stop of the arc, not a blend"
        );

        // C2, literally: a cat standing on the caret's own cell wears the
        // caret's own colour on the same frame.
        let on_caret = eng.pet_offer(geom(), Some(cat(3, 21.0, true, 0.4)));
        assert_eq!(
            on_caret.mote_t,
            eng.field_at(3, 24),
            "the cat, the caret and the ribbon head are one colour on one frame"
        );

        // Off the ribbon there is nothing to wear.
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(30, 8.0, true, 0.4))).mote_t,
            None,
            "no ribbon under the cat, no colour"
        );
        assert_eq!(
            eng.pet_offer(geom(), Some(cat(30, 8.0, true, 0.4)))
                .mote_rgb,
            None
        );

        // And an idle engine offers the resident nothing at all (T6).
        let idle = Engine::new();
        assert!(
            idle.pet_offer(geom(), Some(cat(3, 8.0, true, 0.4)))
                .is_empty(),
            "a disengaged engine offers the pet nothing"
        );
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
    ///
    /// **Since the echo ledger (2026-09-10) the hole is closed twice.** The
    /// same keys and move WITHOUT the host's sweep now light every glyph
    /// cell too — the presses are on the ledger and the licensed move pays
    /// them out ([`Engine::echo_bridge`]) — so the control that shows the
    /// frame this law is about is the batch with NO presses behind it:
    /// program output that moved the caret three cells, which lays nothing.
    #[test]
    fn a_batched_echo_sweeps_every_glyph_cell_it_skipped() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool, pressed: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            if pressed {
                eng.on_event(typed(1), t0);
            }
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            if pressed {
                for k in 1..=3u64 {
                    eng.on_event(typed(1), t0 + ms(k));
                }
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
        let swept = drive(true, true);
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
        let unswept = drive(false, true);
        for col in 1..=4u16 {
            assert!(
                unswept.field_at(3, col).is_some(),
                "glyph cell {col}: the presses on the ledger pay for the batch the host did not sweep"
            );
        }
        let cold = drive(false, false);
        for col in 1..=4u16 {
            assert!(
                cold.field_at(3, col).is_none(),
                "the control: a batch no press explains leaves cell {col} dark — \
                 the frame this law is about"
            );
        }
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
    ///
    /// **Since the echo ledger (2026-09-10) the hole is closed twice**: the
    /// press is on the ledger and the licensed move pays it out even without
    /// the host's sweep, so the frame this law is about is now shown by a
    /// one-cell move with NO press behind it.
    #[test]
    fn a_key_whose_echo_lands_a_frame_late_still_lights_its_glyph() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool, pressed: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            if pressed {
                eng.on_event(typed(1), t0);
            }
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            // The key, echoed a frame later.
            if pressed {
                eng.on_event(typed(1), t0 + ms(8));
            }
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
        let swept = drive(true, true);
        assert!(
            swept.field_at(3, 2).is_some(),
            "the late-echoed glyph is a ribbon cell"
        );
        assert!(
            swept.field_at(3, 1).is_some(),
            "…and the first one still is"
        );
        let unswept = drive(false, true);
        assert!(
            unswept.field_at(3, 2).is_some(),
            "without the host's sweep the press on the ledger still pays for its glyph"
        );
        let cold = drive(false, false);
        assert!(
            cold.field_at(3, 2).is_none() && cold.field_at(3, 1).is_none(),
            "the control: moves no press explains lay nothing — \
             the frame this law is about"
        );
    }

    /// **A PRESS WAITS ON THE LEDGER FOR THE WHOLE PATIENCE** (2026-09-12,
    /// the stall). A key refused at the seam, the next key nine seconds
    /// later — inside `ECHO_PATIENCE_S`, which is the host's in-flight
    /// patience by alias — with the host's sweep on its clock: the stalled
    /// press pays for its cell, and it is relit.
    ///
    /// RED on 81dea89c8: the ledger expired the press at 2 s.
    #[test]
    fn a_press_waits_on_the_ledger_for_the_whole_patience() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The refused press at col 5: pressed, its echo refused by the seam.
        let t = t + ms(100);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        // Nine seconds of silence, then a key at col 6 echoed on time.
        let t = t + ms(9000);
        eng.on_event(typed(1), t);
        tick_at(&mut eng, t);
        let echo = t + ms(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            t,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 5).is_some(),
            "the stalled press's cell is relit nine seconds later"
        );
        assert_eq!(eng.status().bridged, 1);
    }

    /// **FORTY PRESSES TYPED INTO A STALL FIT THE LEDGER.** Forty `Typed`
    /// banked; the host's sweep 5..45 on the OLDEST press's clock and the
    /// move from the mirror: the ledger SPENDS forty (its tally says spent,
    /// not forfeited) and nothing is cleared.
    ///
    /// RED on 81dea89c8: `gap + advance > ECHO_LEDGER_DEPTH` (32) clears.
    #[test]
    fn forty_presses_typed_into_a_stall_fit_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let first = t + ms(100);
        let mut last = first;
        for k in 0..40u64 {
            last = first + ms(85 * k);
            eng.on_event(typed(1), last);
            tick_at(&mut eng, last);
        }
        let echo = last + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 5,
                col1: 45,
            },
            first,
        );
        eng.on_event(mv((3, 5), (3, 45), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        let tally = eng.echo.tally;
        assert_eq!(
            (tally.spent, tally.forfeited, tally.expired),
            (43, 0, 0),
            "the three pre-roll presses and the forty of the batch are spent, none forfeited"
        );
        for col in 5..45u16 {
            assert!(eng.field_at(3, col).is_some(), "col {col} is dark");
        }
    }

    /// **A BATCH SWEEP ON THE OLDEST PRESS'S CLOCK IS BORN NO EARLIER THAN
    /// THE BIRTH FLOOR.** The host dates a stalled batch's sweep at its
    /// oldest press so the ledger can partition it; the RIBBON must not be
    /// born there — three seconds into a cohort that has already faded — but
    /// one stamp window before the echo, so the echoing frame shows the run
    /// lit and the next frame still does.
    ///
    /// RED on 81dea89c8: the cells are born at the sweep's clock and retired
    /// on the tick that laid them.
    #[test]
    fn a_batch_sweep_on_the_oldest_press_clock_is_born_no_earlier_than_the_birth_floor() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        let first = t + ms(100);
        let mut last = first;
        for k in 0..12u64 {
            last = first + ms(85 * k);
            eng.on_event(typed(1), last);
            tick_at(&mut eng, last);
        }
        let echo = last + ms(3000);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 5,
                col1: 17,
            },
            first,
        );
        eng.on_event(mv((3, 5), (3, 17), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        for col in 5..17u16 {
            assert!(
                eng.field_at(3, col).is_some(),
                "col {col} is dark in the echoing frame"
            );
        }
        tick_at(&mut eng, echo + ms(100));
        for col in 5..17u16 {
            assert!(
                eng.field_at(3, col).is_some(),
                "col {col} is dark 100 ms after the echo"
            );
        }
    }
}
