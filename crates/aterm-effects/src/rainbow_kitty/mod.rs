// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow kitty v2: the `rainbow kitty` cursor theme, pixels and sound as one
//! design. Design of record: `docs/design/RAINBOW-KITTY-V2.md`; section
//! references in this module tree are to it, and its §1 rulings are D1-D20.
//!
//! Typing lays the rainbow and a few stars in the sky above it; a long jump is
//! a meteor ([`meteor`]) with one gesture of sound on the same clock.
//! Everything is born at full brightness on the frame the caret is observed
//! and is gone shortly after.
//!
//! ## The seam (§17.2, D14)
//!
//! `CursorGlow` is the seam object, and the delegation is twelve enumerated
//! points, each `if self.v2.engaged()`, which keeps the other styles
//! byte-identical. Every public item on [`Engine`] serves one of them:
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
//! `is_active` follows the v2 fingerprint when engaged. The caret block is
//! ticked by the host; v2 supplies only [`CaretSeam`] (§7.1).
//!
//! ## Diagnostics: the hole tripwire
//!
//! [`Engine::trace_holes`] arms a sensor that makes one record per dark
//! column inside a lit typed line — the engine's state on the frame the hole
//! opened — for the host to take with [`Engine::drain_hole_records`]
//! (`holes.rs` has the walk and the record's fields). `aterm-gui` arms it
//! under the development seam `ATERM_TRACE_TRAIL_HOLES` and prints the
//! records to stderr; a shipped binary never does.
//!
//! ## Module layout (§17.1)
//!
//! * [`timing`]: the shared numbers ([`timing::flight_ms`], the `FLIGHT_*`
//!   constants, [`timing::shed_n`], [`timing::FAN_HERO_N`], the `GLINT_*`
//!   bucket constants, the named curves, the two coverage ceilings).
//! * [`spine`]: one momentum integrator and one display follower (§19.1).
//! * [`ribbon`]: the ribbon, the per-row `col → t` field index, the hot edge.
//! * [`stardust`]: the sky: magnitude classes, birth zones, the token bucket.
//! * [`meteor`]: the flight, its own arc, and the landing.
//! * palette: `crate::spectrum`. C1: the seven-stop ROYGBIV is the only colour
//!   law.
//!
//! ## Invariants
//!
//! * **T1, no keystroke, no light.** Geometry is born only on an observed,
//!   licensed cursor move; a keydown never pre-draws.
//! * **T2, the frame-0 law.** Everything an event owns exists, at full
//!   brightness, on the frame the caret is first observed at its landing.
//! * **T6, idle → zero.** [`Engine::needs_frame_cadence`] is `false` and
//!   [`Engine::next_change_deadline`] is `None` when nothing is live, and the
//!   fingerprint is `0` when nothing is on glass.
//! * **T7, wall-clock, not frame-count.** Every curve takes `now`.
//! * **Determinism.** Every seed is hashed from `(row, col, born)` at the
//!   spawn edge; there is no per-frame RNG; a frame at the same `now` is
//!   byte-identical on CPU and GPU (§18).
//! * **No allocation on the steady-state frame path.** Fixed pools and
//!   resident `Vec`s that are cleared, never rebuilt.
//! * **No `unsafe`** anywhere in this tree.

pub mod companion;
pub mod meteor;
pub mod ribbon;
pub mod spine;
pub mod stardust;
pub mod timing;
pub mod witness;

mod holes;

/// The flow state, re-exported at the theme's root because it is the theme's
/// published state: it rides [`Ctx`], [`Status`] and the `trail status` row.
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
#[cfg(test)]
use stardust::StarBudget;
use stardust::{GlyphProbe, Stardust};
use timing::{JUMP_MIN_CELLS, SPRING_SNAP_RESPONSE_S, half_life};
use witness::{RowSample, Witness};

/// Life of the caret's landing re-light ([`Engine::caret_paint`]): a hold of
/// one [`SPRING_SNAP_RESPONSE_S`] (the flare's own relax), then eight
/// half-lives on the same constant; `2⁻⁸ < 1/255`, so the snap to exactly zero
/// (T6) is below the block fill's u8 resolution.
const RELIGHT_LIFE_S: f32 = SPRING_SNAP_RESPONSE_S + 8.0 * SPRING_SNAP_RESPONSE_S;

// ===========================================================================
// The event vocabulary
// ===========================================================================

/// Travel direction, resolved to the dominant axis: the same rule D15 uses to
/// pick a rasterizer, so the kitty's facing, the cue's `dir` and the train's
/// beam always agree.
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

/// Why a move was admitted: the verdict the licence gate
/// (`CursorGlow::move_licensed`) and `classify_move` reached before seam point
/// 1 hands the move here. v2 never re-derives a licence, so T1 lives entirely
/// upstream of this enum.
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
    /// A delivered insert (a file drop, a paste, a Tab completion, a ⌃V):
    /// licensed at delivery (the host's completed write) and handed over
    /// behind its own `Sweep` of the whole span. An echo like `Typed`
    /// ([`Licence::is_echo`]); it differs only at the echo ledger, which pays a
    /// hole before the insert from the older presses and charges the insert's
    /// own span to nothing.
    Insert,
    /// **NO credit** — program-driven motion under an arm that admits it, such
    /// as a PTY line-feed cascade (§8.2's "PTY line feed (no credit)", D18).
    /// Draws its meteor under the 2-cap and the retire law; its audio rides
    /// the cascade lane's 1-voice / 180 ms exclusivity.
    Pty,
}

impl Licence {
    /// The hand's own echo — a typed key's or a delivered insert's. Inert
    /// everywhere `Typed` is inert: no wake, no abandon, no meteor, no
    /// mini-fan, no nav tick, no star deal; only the echo ledger tells the
    /// two apart.
    #[must_use]
    pub fn is_echo(self) -> bool {
        matches!(self, Self::Typed | Self::Insert)
    }

    /// A keyed jump or hop — an arrow, a word motion, a scripted preview:
    /// the licences that earn a wake, a scrub, a meteor cue and the nav
    /// tick.
    #[must_use]
    pub fn is_keyed_nav(self) -> bool {
        matches!(self, Self::Nav | Self::Synthetic)
    }
}

/// What kind of glyph an [`Event::Typed`] carried: what the glow needs to deal
/// a star (§5.8's earned-hero set). D7: heroes are capitals, `!`, kitty
/// Delight, the landing fan's own m1, and the 1-in-12 deal; the glow cannot
/// see the synth's `walk`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedClass {
    /// An ordinary printable glyph.
    Glyph,
    /// A capital (by Shift or Caps Lock; the glyph is asked): earns an m1 at
    /// the glyph's cell (§5.8) and, at the synth, the octave ring at +60 ms,
    /// −6 dB, and the ×4 sparkle (§10.4). The keyed seam's `click_shifted`
    /// agrees.
    Capital,
    /// `!` — earns an m1 (§5.8).
    Bang,
    /// Space. Lays ribbon but deals no star; the synth's rest + bass dyad +
    /// breath (§8.2, §9.3).
    Space,
}

/// The scale of a kill chord: a word going, or a clause going. §5.6 prices
/// the erase population per 3 cells of the kill (cap 8) and §8.2 gives the
/// scales different sounds (`KillWord`'s poof, `Kill`'s swoosh). The spine
/// drains identically for both (§19.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillScope {
    /// `^W`, Alt-D, Alt/Ctrl-Backspace — one word.
    Word,
    /// `^U`, `^K` — a whole clause.
    Line,
}

/// One engine event (§17.1): everything the host tells v2. `Copy`, because
/// events are buffered on a resident queue and replayed to the producers twice
/// per tick (once to ingest, once to deal births against the ribbon's planned
/// bands) without allocating.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A typed glyph echoed forward. `cells` is how many cells the echo
    /// advanced (a coalesced burst may carry several); `shifted` rides through
    /// to `SoundCue::shifted`; `class` decides the hero deal.
    ///
    /// Seam point 2 (`note_typed`). The only event that builds momentum.
    Typed {
        /// Cells the echo advanced.
        cells: u16,
        /// The key carried Shift — a capital or a shifted symbol.
        shifted: bool,
        /// What kind of glyph it was (§5.8).
        class: TypedClass,
    },
    /// One Backspace. Seam point 2 (`note_erase`). Drains the spine, retracts
    /// a ribbon cell, throws 3 m3 + 1 m2, and keeps the classic smoke poof
    /// unchanged (§5.6). Unpitched: no erase chimes (§19.2).
    Erase,
    /// A kill chord. Seam point 2 (`note_kill`). The ribbon drains
    /// farthest-first at `12·n + 240` ms; 1 star per 3 cells, cap 8.
    Kill {
        /// Cells erased.
        cells: u16,
        /// Word or line scale (§8.2).
        scope: KillScope,
    },
    /// An observed, licensed cursor move, handed over after the licence gate
    /// and `classify_move` have run (seam point 1). This is the only event
    /// that can mint a meteor, and T1 is exactly the statement that geometry
    /// is born here and nowhere else.
    Move {
        /// `(row, col)` the caret left: the host's last observed cell,
        /// licensed or not. The engine's mirror advances only on licensed
        /// moves, and [`Engine::echo_bridge`] reads the hop between the two,
        /// so anything but the observed origin would invent holes.
        from: (u16, u16),
        /// `(row, col)` the caret was observed at — the landing, and `t = 0`.
        to: (u16, u16),
        /// Why the move was admitted.
        licence: Licence,
        /// Travel direction, dominant axis.
        dir: Dir,
    },
    /// A keyed Enter, the key only (seam point 2, `note_return`). Inert in
    /// every producer (T1: a keypress is not a move; the shell may swallow
    /// it). The Enter's flight arrives as the caret's observed motion, an
    /// [`Event::Move`] with [`Licence::Return`]: the meteor along the true
    /// vector with `T = flight_ms(cells)`, a landing sized by rows (5-7 m3 +
    /// 1 m2, reach 1.6 `ch`, no burst, hero or rain, D8), the pin, and the
    /// [`SoundKind::Enter`] cue.
    Return,
    /// A composer line break (Shift+Enter), stamped at the key before its
    /// repaint. Like `Return`, this starts a fresh ribbon walk without
    /// minting a flight. Its own typed stamp has the same clock and must not
    /// spend the fresh-line gate before the first character on that line.
    ComposerNewline,
    /// A later non-typing gesture superseded a composer break before its
    /// caret home was observed. The ribbon drops only that pending gate;
    /// this key edge mints no light or movement of its own.
    CancelComposerNewline,
    /// Window focus changed. On loss the ribbon embers out in 300 ms on
    /// `spend` and the stars die; nothing sounds (§8.2).
    Focus(bool),
    /// The host's reduced-motion posture changed. Under it every mark is
    /// static and takes the theme's one linear fade,
    /// [`timing::REDUCED_MOTION_FADE_MS`] (§2.5, §6.11).
    ReducedMotion(bool),
    /// A typed echo's glyph cells (seam point 1): the classifier licensed the
    /// same-row echo `col0 → col1` on `row` as typing, one key's glyph or
    /// several the PTY delivered in one frame on banked press credits. A
    /// `Typed` key lays at the caret the tick replays with, so it owns its
    /// glyph cell only when its echo shares its tick; a batched echo's earlier
    /// cells and a late echo's glyph come from this sweep alone. The ribbon
    /// lays the cells of `col0..col1` that no live cell owns, as typing, born
    /// at the echo. Nothing else reads it: no spine advance, star, meteor or
    /// cue.
    ///
    /// The engine mints one itself for a late echo (a key's cells the seam
    /// refused because the echo came after its `0.25 s` window), from the
    /// mirror to the next licensed typed move, paid press by press from the
    /// [`EchoLedger`] ([`Engine::echo_bridge`]).
    Sweep {
        /// The echoed row.
        row: u16,
        /// The first glyph cell of the echo.
        col0: u16,
        /// The landing (exclusive): the caret the echo left the row at.
        col1: u16,
    },
    /// The insert's rewrite (seam point 1): the program pulled the caret back
    /// inside a span a delivered insert laid (for example an agent replacing
    /// a dropped image path with `[Image #1] `; `CursorGlow::insert_span`).
    /// The ribbon retracts the row's suffix from `col` farthest-first at
    /// `12·n + 240` ms, the kill's law, and moves its caret; the sky finishes
    /// the retracted cells' field stars on the same span; the engine's caret
    /// mirror moves. Nothing is born, winces or sounds; the spine and the echo
    /// ledger are untouched (T1, T5). Minted only by the seam's insert-scoped
    /// rewrite arm, never from a key.
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

/// Everything v2 needs from the host's `GlowConfig`, and nothing else. Narrow
/// on purpose, so no v2 producer can branch on another style's state.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Dark ground. The whole light fork (§3.3, §6.10, §5.2's light twins)
    /// hangs off this one operator flip: additive light becomes source-over
    /// ink, and "bigger" is bought as area, never as brightness (L6).
    pub dark_theme: bool,
    /// The host's effect intensity, 0..1 — a coverage multiplier, applied
    /// before every cap, never after.
    pub intensity: f32,
    /// The host's trail duration knob — scales the ribbon's cell lives. It
    /// does not scale [`timing::flight_ms`]: the meteor's clock is a shared
    /// coupling number, not a taste dial.
    pub duration: Duration,
    /// The shipped `tall` ribbon spelling (`RAINBOW_TALL_UP 1.10 ch`, spine at
    /// the row bottom) vs `underline`. D16: this is why star birth zones are
    /// defined against the ribbon's top edge and not in cell coordinates.
    pub ribbon_tall: bool,
    /// The explicit `... flat` spelling (`GlowConfig::ribbon_flat`): the flat
    /// body instead of the default comet body and its vivid rail
    /// (`RAINBOW-KITTY-V2.md` §30). Read by the ribbon alone.
    pub ribbon_flat: bool,
    /// Theme foreground — the ink colour of the light fork and the ledger's
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
            ribbon_flat: cfg.ribbon_flat,
            theme_fg: cfg.theme_fg,
            theme_bg: cfg.theme_bg,
            reduced_motion,
        }
    }
}

/// The caret seam (§7.1). v2 does not own the caret block fill; the host ticks
/// the caret (`tick_with_family_phase`, after the ledger) and copies this
/// record into `RainbowConfig`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaretSeam {
    /// The instant a meteor's frame-0 flare fired, if one did. The block fills
    /// `#FFFFFF` for one frame, then mixes toward its field stop on
    /// `spring-snap` ([`timing::spring_snap`], response 0.18 s); the halo rings
    /// pop radius ×1.4 on the same edge and relax τ 120 ms.
    ///
    /// Set only by a credited spawn, never a [`Licence::Pty`] line feed (§8.2)
    /// and never under reduced motion (§6.11). Cleared once the relax has
    /// settled ([`timing::SPRING_SNAP_RESPONSE_S`] after the edge), so `Some`
    /// is always a live relax.
    pub flare_at: Option<Instant>,
    /// The caret's position in the field (C2): the ribbon head, the caret and
    /// a star's halo on that cell read this one number and so share a colour.
    /// Sampled after the frame's events are laid and the ribbon has planned,
    /// so it equals [`Engine::field`] on the same frame. The raw walk `t`,
    /// unbounded past `1.0`; a host folds it through [`meteor::tri`] before
    /// `spectrum` (D4).
    pub field_t: f32,
    /// The caret's paint level, 0..1: the display spine
    /// ([`Engine::momentum_display`]) floored by the landing re-light. A
    /// credited spawn lifts it to exactly `1.0` on the landing frame, holds it
    /// through the flare's relax and cools it on [`timing::half_life`] with the
    /// flare's constant (`RELIGHT_LIFE_S`): the caret is never dimmed at the
    /// destination (§7.1).
    ///
    /// The host copies this into `RainbowConfig::paint`, consumed as
    /// `max(paint, energy)`: the host cadence's 220 ms `energy` gives the fast
    /// attack (§7.1's "cool: τ 220 ms") and this number owns the release, on
    /// the ribbon's own τ 0.85 s. [`Engine::caret_paint`] is the same number
    /// as a pure read at the host's own `now`.
    pub paint: f32,
}

/// One impulse, two bodies (D13, §7.2): v2 emits a single impulse and
/// whichever body `CompanionDuty` says owns the frame consumes it. Identity
/// never changes; no new sprite frames; no facing flip beyond `facing_left`.
#[derive(Clone, Copy, Debug)]
pub enum CompanionImpulse {
    /// A meteor was born. The flying head takes the frame-0 teleport
    /// (placement snaps), `disp = max(disp, 0.97)`, a `lead = −0.30` cell whip
    /// springing to `+0.22` on `timing::spring_whip`, and
    /// `land_at = t0 + t_flight`. The resident pet is only offered this: it
    /// keeps its own pounce (no teleport) and may use `t0 + t_flight` as its
    /// perk edge (owner, 2026-09-05).
    Meteor {
        /// Flight direction — the existing `facing_left` bank.
        dir: Dir,
        /// The spawn edge.
        t0: Instant,
        /// §6.2's `T`, so the squash lands with the pin and the bell (§6.7).
        t_flight: Duration,
    },
    /// A landing with no flight behind it: a credited spawn under reduced
    /// motion (§6.11 — the head is at the landing, there was no flight to
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

/// One tick's output (§17.1). The three pixel streams and the cue sink are
/// borrowed host scratch (seam point 3: the same `under` / `out` / `halos` the
/// other styles use), so the frame allocates nothing (§18). The small outputs
/// ([`Frame::caret`], [`Frame::companion`], [`Frame::fp`]) are owned values.
pub struct Frame<'a> {
    /// `glow_under`: composited beneath glyph ink, never blooms. Carries the
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
    /// The impulse minted on this frame, if any (§7.2). A report: the impulse
    /// stays pending until [`Engine::take_companion_impulse`] consumes it, and
    /// only the take transfers ownership.
    pub companion: Option<CompanionImpulse>,
    /// The frame fingerprint. **Exactly `0` when nothing is on glass** (T6) —
    /// that is the whole contract, and `is_active` follows it when engaged.
    pub fp: u64,
}

/// The mend mark for this tick (§23 "The mend"): the typed key this frame
/// carries is a typo fix, within [`spine::MEND_WINDOW_S`] of a Backspace run of
/// at most [`spine::MEND_MAX_DELETES`] deletes ([`spine::Spine::mend`]). `Some`
/// only on the tick that births the fix.
///
/// Consumers: the ribbon prices the fix's births at `max(birth_disp, disp)`
/// ([`Ctx::birth_disp`] floored by [`Mend::disp`]); stardust may birth the
/// fix's m2 from the erased cell's sky position. The meteor and the caret
/// ignore it: a mend is a birth price, not a gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mend {
    /// The last delete's edge ([`spine::EraseMark::at`]).
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

/// The per-tick read-only context handed to every producer. One struct, so
/// everything born on a frame is priced by one evaluation of the spine
/// (§17.1: ribbon width, star counts, meteor width, caret paint and the kitty
/// bank all read it).
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// The frame's present time. Every curve in the theme takes it (T7).
    pub now: Instant,
    /// Window-space geometry, in px. All effect emissions are window-absolute.
    pub geom: Geom,
    /// The host config, projected (see [`Config`]).
    pub cfg: &'a Config,
    /// [`spine::Spine::disp`] — the honest eased spine every "earned drama"
    /// consumer reads.
    pub disp: f32,
    /// [`spine::Spine::birth_disp`] — the resume-floored spine the birth laws
    /// read, and only they.
    pub birth_disp: f32,
    /// [`spine::Spine::phase`] — the lay/flow clock.
    pub phase: f32,
    /// The caret's `(row, col)` as last observed.
    pub caret: (u16, u16),
    /// The caret's own field `t` — §6.4's `t_land`, the meteor's phase lock,
    /// and C2's shared colour.
    pub caret_t: f32,
    /// The band's walk at the caret, `(d, t_zero)` of the cohort that owns the
    /// caret's cell ([`ribbon::Ribbon::walk_origin_at`]): the walk's distance
    /// at the caret cell and its `t` at distance zero. Set beside
    /// [`Ctx::caret_t`] on a move's own ctx only; `None` elsewhere and where
    /// nothing is laid at the landing. It carries the band's pace past the
    /// landing cell (`d/16` within the first sixteen cells, `1/36` after),
    /// which the landing's marks continue (`meteor::LandingWalk`).
    pub caret_walk: Option<(f32, f32)>,
    /// The mend mark, on the one tick that births a typo fix (see [`Mend`]).
    pub mend: Option<Mend>,
    /// [`spine::Spine::surge`] — how much of the crisp edge's stretch the
    /// last typed key bought, 0..1. A birth price, like [`Ctx::birth_disp`]:
    /// only [`ribbon::edge_cells`] reads it, and only where a key lays.
    pub surge: f32,
    /// The flow state ([`spine::Spine::flow`]): the run, its high-water mark
    /// and the open ramp. Flow draws nothing of its own; every consumer lerps
    /// from identity at `heat == 0`, so a cold-hand frame is byte-identical to
    /// one with no flow.
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
    /// `v2_bridged=`: cells the echo ledger has laid since engagement that the
    /// host's sweeps did not (a refused late echo's glyphs, relit by the next
    /// licensed one, [`Engine::echo_bridge`]). The admission ring still reads
    /// `declined no-fresh-hint` for the late move; this row says a hole was
    /// repaired. Cumulative.
    pub bridged: u32,
    /// `ribbon_retired=`: ribbon cells taken off by content over this
    /// engine's life. The host's witness saw the glyph under them replaced
    /// (fast melt) or gone (the swoosh) ([`Engine::witness_rows`]).
    /// Cumulative across [`Engine::reset`], unlike `bridged`, so a
    /// control-socket reader can tell "its text moved or went" from "the band
    /// expired".
    pub retired: u64,
    /// `ribbon_followed=`: ribbon cells carried with their text over this
    /// engine's life: to another row by the follow pass (the run's glyphs
    /// stand a row away as a block, as when a bottom-anchored composer grows
    /// without a scroll; [`Engine::follow_rows`],
    /// [`ribbon::Ribbon::translate_run`]), or along their own row by the shift
    /// pass when a mid-line insert pushes the tail right
    /// ([`ribbon::Ribbon::shift_run`]). Cumulative across [`Engine::reset`].
    pub followed: u64,
    /// `ribbon_follow_missed=`: the follow pass's misses over this engine's
    /// life: armed cells whose glyphs left their row while the run's text
    /// arrived on a neighbouring row the pass did not carry
    /// ([`witness::FollowScore::missed`]). Each is then judged by the content
    /// witness where it stood. The healthy number is `0`. Arrivals are weighed
    /// as the follow weighs them, with twins and a word a wrap relaid at the
    /// start of the run's row left out ([`witness::Witness::follow_runs`])
    /// (owner, 2026-09-23).
    /// Cumulative across [`Engine::reset`].
    pub follow_missed: u64,
    /// The eased spine.
    pub disp: f32,
    /// The flow row (`flow=` / `combo=` / `combo_best=`): what a turn-based
    /// agent reads off the control socket to tell that the human is mid-flow
    /// and hold its turn.
    pub flow: Flow,
    /// Glint tokens on hand.
    pub tokens: f32,
    /// The last frame's fingerprint.
    pub fp: u64,
}

// ===========================================================================
// Cadence — what a producer may ask the host for (§18, T6)
// ===========================================================================

/// The frame cadence: what a producer asks for while brisk light is live
/// (something on glass moves every frame: a head in flight, a thrown star, a
/// mark retracting into the caret, the ribbon head cell's 18 ms `edge-in`).
/// The design's reference tick (§18, 120 Hz); the host substitutes its own
/// present interval through seam point 8 ([`Engine::needs_frame_cadence`]), so
/// this is what engine-level laws are measured against, not a rate the host
/// must run at.
pub const FRAME_CADENCE: Duration = Duration::from_micros(8_333);

/// The tail floor: the soonest a producer may ask to be woken while only
/// tails are live (a half-life fade, the swoosh's grace and fade, a star past
/// its hold, the spine settling under a laid ribbon). 45 ms is about 22 Hz;
/// the present gate dedups identical frames between visible steps. A live
/// twinkling star keeps the sky no coarser than this floor plus one arm step
/// (≤ 62 ms, §5.5).
pub const TAIL_FLOOR: Duration = Duration::from_millis(45);

/// No busy re-arm: an engine deadline is never sooner than this after `now`.
/// A producer answering "now" would have the host re-arm on the turn it was
/// asked, which is a spin dressed as a deadline.
pub const ARM_MIN: Duration = Duration::from_millis(1);

/// A producer's answer to "when do you next change what is on glass?", folded
/// from three kinds of offer:
///
/// * [`Cadence::brisk`]: per-frame motion is live; the answer is the next
///   frame whatever else was offered.
/// * [`Cadence::edge`]: the exact instant a mark appears or a motion starts
///   (a reach beat laying a cell, the retract's first frame, the swoosh's
///   end). Offered as is; these are sparse by construction.
/// * [`Cadence::tail`] / [`Cadence::tail_at`]: a fade whose next visible
///   step (its u8 coverage changing by a level) is `step_s` away, or unknown,
///   and every disappearance or sub-level regime change. Never sooner than
///   [`TAIL_FLOOR`], so tails from several producers cannot interleave into
///   a finer cadence.
///
/// The engine's `min` composes every producer's answer without knowing which
/// kind it is.
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

    /// A tail whose next visible step is the instant `at` — a disappearance
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

/// The echo patience, seconds: how long a press waits on the ledger for its
/// echo before it is forgotten as swallowed. An alias of the host's in-flight
/// patience (`CursorGlow::IN_FLIGHT_PATIENCE_S`), so one press has one
/// patience whichever layer is asked.
pub const ECHO_PATIENCE_S: f32 = crate::cursor_glow::IN_FLIGHT_PATIENCE_S;

/// The most cells one observed move may lay from the ledger — the host's
/// coalesced-sweep cap by alias (`TYPED_STAMP_DEPTH`): more presses than one
/// move can pair with are presses no move can spend, so the ledger never
/// holds more either.
pub const ECHO_LEDGER_DEPTH: usize = crate::cursor_glow::TYPED_STAMP_DEPTH;

/// The ribbon's birth floor, seconds. A host sweep is dated at its licensing
/// key's clock (for a stalled batch, the oldest unpaid press) because the
/// ledger partitions by it, but the ribbon is born no earlier than one stamp
/// window before the echo (the host's `TYPE_HINT_FRESH`); otherwise a batch
/// could be born already faded into a stale cohort. A per-key sweep's key is
/// inside the window, so `max` is a no-op there.
pub const SWEEP_BIRTH_FLOOR_S: f32 = crate::cursor_glow::CursorGlow::TYPE_HINT_FRESH;

/// The echo ledger: typed presses whose caret advance the engine has not seen
/// yet, oldest first, each `(press, cells)`.
///
/// One press is one cell of ribbon: a licensed typed move spends the ledger,
/// oldest first, for every cell it or the host lays ([`Engine::echo_bridge`]),
/// and a press the caret never answers inside [`ECHO_PATIENCE_S`] is dropped
/// as swallowed. Fixed-size and `Copy`, like the host's `TypedStamps`, so the
/// frame path allocates nothing (§18); [`ECHO_LEDGER_DEPTH`] is the most
/// presses one move could pair with.
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
    // is [`ECHO_LEDGER_DEPTH`] deep.
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
/// conserves, read off the real ledger by
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
    /// sum — the host prices that at the key). When full, the oldest press is
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
    /// A press at exactly the patience is kept.
    fn expire(&mut self, now: Instant) {
        let stale = |at: Instant| now.saturating_duration_since(at).as_secs_f32() > ECHO_PATIENCE_S;
        for slot in &mut self.slots {
            if slot.is_some_and(|(at, _)| stale(at)) {
                #[cfg(test)]
                {
                    self.tally.expired += slot.map_or(0, |(_, c)| u32::from(c));
                }
                *slot = None;
            }
        }
        self.repack();
    }

    /// Restore the packing after slots were emptied in place: every `Some`
    /// precedes every `None`, oldest first, so a hole in the middle cannot
    /// hide the presses behind it.
    fn repack(&mut self) {
        let mut kept = [None; ECHO_LEDGER_DEPTH];
        for (n, entry) in self.slots.into_iter().flatten().enumerate() {
            kept[n] = Some(entry);
        }
        self.slots = kept;
    }

    /// Cells of the banked presses whose clock passes `keep`.
    fn cells_where(&self, keep: impl Fn(Instant) -> bool) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|&&(at, _)| keep(at))
            .map(|&(_, cells)| usize::from(cells))
            .sum()
    }

    /// Cells the banked presses can still pay for.
    fn total(&self) -> usize {
        self.cells_where(|_| true)
    }

    /// Cells of the presses banked strictly before `key` — the ones whose
    /// echoes could lie to the left of that key's own cell.
    fn older_than(&self, key: Instant) -> usize {
        self.cells_where(|at| at < key)
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
        self.repack();
    }

    /// Forget every press as forfeited.
    fn clear(&mut self) {
        #[cfg(test)]
        {
            self.tally.forfeited += self.total() as u32;
        }
        self.slots = [None; ECHO_LEDGER_DEPTH];
    }

    /// Forget every press: the stale ones are tallied as expired (swallowed
    /// before this edge), the fresh ones as forfeited — none survive. The
    /// order is the ledger model's `LedgerForgotten`: a row change, an edit
    /// or a focus loss expires first, so a stale press is never booked as
    /// forfeited.
    fn forget(&mut self, now: Instant) {
        self.expire(now);
        self.clear();
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

/// The v2 engine (§17.1): the object `CursorGlow` delegates to at the twelve
/// seam points. Deterministic and clockless: every entry point takes an
/// injected `now`, and there is no per-frame RNG (§18).
#[derive(Clone, Debug)]
pub struct Engine {
    /// Whether this engine owns the frame at all — the `if self.v2.engaged()`
    /// of every seam point. False for the other nine styles, and false for
    /// rainbow kitty while the migration flag is off (§17.3 phase 3).
    engaged: bool,
    /// Reduced-motion posture, mirrored into [`Ctx::cfg`] each tick.
    reduced_motion: bool,
    /// The momentum integrator and its display follower (§17.1, §19.1).
    spine: Spine,
    /// The laid ribbon and the field index.
    ribbon: Ribbon,
    /// The sky, and the one glint bucket (§13).
    stardust: Stardust,
    /// The flights and their landings.
    meteor: Meteors,
    /// What the host must copy into `RainbowConfig` (§7.1).
    caret_seam: CaretSeam,
    /// The caret mirror: the last licensed move's landing (or a row-change
    /// observation's), the cell a `Typed` key lays against.
    caret: (u16, u16),
    /// Whether the mirror has been learned from an observed move since the
    /// last [`Engine::reset`]. Until then the ledger reads no hole and pays
    /// only a same-row echo's own advance, from the host's `from`
    /// ([`Engine::echo_bridge`]).
    caret_known: bool,
    /// A caret the host observed on another row without a licence
    /// ([`Engine::observe_caret`]), waiting for the end of the tick it was
    /// reported in: the events buffered before it were typed at the caret
    /// the engine held then, and are laid there.
    caret_seen: Option<(u16, u16)>,
    /// The echo ledger (see [`EchoLedger`]).
    echo: EchoLedger,
    /// Cells the ledger has laid that the host did not sweep — [`Status::bridged`].
    bridged: u32,
    /// The clock of the host's own sweep for the last observed move — the
    /// licensing key the ledger partitioned by — or `None` when the host
    /// sent none. Test-only: the Tier-1 twin asserts it IS the key's clock
    /// on every move, so the partition it projects is the engine's own.
    #[cfg(test)]
    last_host_sweep_at: Option<Instant>,
    /// Every [`Event::Kill`] received. Test-only: the seam's one-kill-one-
    /// `Kill` pin counts them.
    #[cfg(test)]
    kills: u32,
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
    /// wraps at its edges, never the grid's. Survives `reset` — it is the
    /// host's layout, not the engine's state.
    pane: Option<(u16, u16)>,
    /// The focused pane's `(first row, rows)` — the companion to
    /// [`Engine::pane`], for the reach below a row
    /// ([`ribbon::Ribbon::pane_rows`]). Stored whether or not engaged.
    pane_rows: Option<(u16, u16)>,
    /// Events buffered since the last tick, with their own edges. Resident and
    /// reused (`clear()` keeps capacity), so the steady path allocates nothing
    /// (§18).
    ///
    /// Buffered because a birth needs geometry: `on_event` has no `Geom`, and
    /// §5.4's zones are relative to the ribbon's planned top edge. Seam points
    /// 1 and 2 run inside the host's tick before seam point 3, so a buffered
    /// event is still drawn on the frame the caret was observed (T2).
    events: Vec<(Event, Instant)>,
    /// The frame's first one-shot move (a move whose licence is not an echo,
    /// or an erase's retreat, [`Engine::is_erase_retreat`]) as `(its index in
    /// events, the caret before it)`. A `Typed` buffered before it (type-ahead
    /// before the response landed) is replayed at that caret
    /// ([`Engine::replay_events`]). The caret is `None` when no licensed move
    /// has seeded the mirror ([`Engine::caret_known`]); such a key is held
    /// with no cell. The index follows the buffer (a bridge sweep inserted at
    /// the front shifts it) and the caret follows the text: it is translated at
    /// the scroll and band seams and forgotten when its row goes
    /// ([`Engine::translate_pre_move`]). Taken every tick, cleared by
    /// [`Engine::reset`].
    pre_move: Option<(usize, Option<(u16, u16)>)>,
    /// Heroes the host earned since the last tick (the kitty's Delight edge,
    /// `(cell, edge)`), dealt in pass 2 beside the events because a birth
    /// needs the planned bands. Resident and reused, like `events`.
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
    /// The last credited landing the caret's re-light floors its paint from
    /// (§7.1, [`CaretSeam::paint`]). Never a typed echo (typing paint is the
    /// spine's); `None` once the re-light has run its `RELIGHT_LIFE_S`.
    paint_at: Option<Instant>,
    /// The last frame's fingerprint — `is_active` follows it (§17.2).
    fp: u64,
    /// The last frame's counters (seam point 12).
    status: Status,
    /// Whether the last frame drawn carried brisk light (per-frame motion on
    /// glass), the answer [`Engine::needs_frame_cadence`] gives. Cached at the
    /// end of [`Engine::tick`] because seam point 8 takes no clock and the
    /// host asks right after the tick; false whenever nothing is live (T6).
    brisk: bool,
    /// The content witness ([`witness`]): what glyph each resident ribbon cell
    /// was first seen over, so an overwrite or a blanking can retire it
    /// ([`Engine::witness_rows`]).
    witness: Witness,
    /// The cells one witness walk named for retirement, by identity
    /// (`(row, col, born)` — a position can hold a stale cell under a fresh
    /// one) — resident scratch, cleared by the walk, so a frame that
    /// retires nothing allocates nothing (§18).
    retire_scratch: Vec<(u16, u16, Instant)>,
    /// The witness's other verdict, resident beside it: the cells whose
    /// glyph went and nothing replaced it, released to the swoosh
    /// ([`Engine::witness_rows`], [`ribbon::Ribbon::release_cells`]).
    release_scratch: Vec<(u16, u16, Instant)>,
    restore_scratch: Vec<(u32, Option<Instant>)>,
    restored_scratch: Vec<u32>,
    /// [`Status::retired`]'s tally. Survives [`Engine::reset`].
    retired: u64,
    /// [`Status::followed`]'s tally. Survives [`Engine::reset`].
    followed: u64,
    /// [`Status::follow_missed`]'s tally. Survives [`Engine::reset`].
    follow_missed: u64,
    /// The follow pass's verdicts this tick ([`Engine::follow_rows`]) —
    /// resident scratch, cleared by the search.
    follow_scratch: Vec<witness::FollowRun>,
    /// The shift pass's verdicts this tick ([`Engine::follow_rows`], the
    /// band following its text along its row) — resident scratch, cleared
    /// by the search.
    shift_scratch: Vec<witness::ShiftRun>,
    /// The identities the follow pass moved this tick, for the witness's
    /// re-keying — resident scratch.
    moved_scratch: Vec<(u16, u16, Instant)>,
    /// The rows the host was asked for this frame and did not deliver —
    /// the first `withheld_n` ([`Engine::follow_rows`] finds them,
    /// [`Engine::witness_rows`] hands them to the walk and clears them).
    withheld: [u16; witness::WITNESS_ROWS + witness::ARMING_ROWS],
    withheld_n: usize,
    /// The grid's row count as of the last tick, so
    /// [`Engine::ribbon_rows_for`] names no neighbour row past the grid.
    grid_rows: u16,
    /// A held park's clock while its flush is being judged
    /// ([`Engine::set_flush_clock`], [`Engine::on_event`]).
    flush_clock: Option<Instant>,
    /// The soft-wrapped caret the seam judged this frame, with its key's clock
    /// ([`Engine::note_soft_wrap`]); taken by the replay.
    soft_wrap: Option<(ribbon::SoftWrap, Option<Instant>)>,
    /// The lifted word the seam judged this frame, with its key's clock
    /// ([`Engine::note_lift`]); taken by the replay.
    lift: Option<(ribbon::Lift, Option<Instant>)>,
    /// The seam's verdict that the soft-wrapped word came down, for the `Move`
    /// from the carry's landing to `.1` ([`Engine::note_reflow`]); taken by
    /// the replay.
    reflow: Option<(ribbon::SoftWrap, (u16, u16))>,
    /// The revive: the seam's verdict that the `Sweep` it pushed re-lays a
    /// retired typed run, with that run's walk ([`Engine::note_revive`]);
    /// taken by the replay.
    revive: Option<ribbon::Revive>,
    /// Hidden for one frame ([`Engine::hide_next_frame`]): the next tick runs
    /// every clock and writes nothing while the viewport shows history. Taken
    /// by the tick.
    hidden: bool,
    /// **THE HOLE TRIPWIRE** (the module doc's diagnostics): `None` unless
    /// a host armed it ([`Engine::trace_holes`]) — so the frame path pays
    /// nothing for it but one `is_some` per tick. Survives
    /// [`Engine::reset`]: arming is the host's choice, not frame state.
    holes: Option<Box<holes::HoleTrace>>,
}

impl Default for Engine {
    /// The same engine [`Engine::new`] builds — there is one constructor
    /// contract, so a caller reaching this through `Default` gets the full
    /// glint bucket a session starts with and not a silent first capital.
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    /// A disengaged engine with nothing live and a full glint bucket, so the
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
            caret_seen: None,
            echo: EchoLedger::default(),
            bridged: 0,
            #[cfg(test)]
            last_host_sweep_at: None,
            #[cfg(test)]
            kills: 0,
            erased: (0, 0),
            mend: None,
            pane: None,
            pane_rows: None,
            events: Vec::new(),
            pre_move: None,
            earned: Vec::new(),
            pending_cues: Vec::new(),
            companion: None,
            paint_at: None,
            fp: 0,
            status: Status::default(),
            brisk: false,
            party: None,
            witness: Witness::new(),
            retire_scratch: Vec::new(),
            release_scratch: Vec::new(),
            restore_scratch: Vec::new(),
            restored_scratch: Vec::new(),
            retired: 0,
            followed: 0,
            follow_missed: 0,
            follow_scratch: Vec::new(),
            shift_scratch: Vec::new(),
            moved_scratch: Vec::new(),
            withheld: [0; witness::WITNESS_ROWS + witness::ARMING_ROWS],
            withheld_n: 0,
            grid_rows: 0,
            flush_clock: None,
            soft_wrap: None,
            lift: None,
            reflow: None,
            revive: None,
            hidden: false,
            holes: None,
        }
    }

    // -- the seam's gate ---------------------------------------------------

    /// The gate: `true` when this engine owns the frame. Every one of the
    /// twelve delegation points is one `if self.v2.engaged()` (D14); when
    /// `false`, `CursorGlow` behaves as it does for the other styles.
    #[must_use]
    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Engage or disengage. Disengaging drops everything live (marks, the
    /// spine, and events buffered since the last tick) through
    /// [`Engine::reset`]: a style switch hands its residue to the crossfade
    /// ghost. An event reported but not ticked before the switch is discarded
    /// (its light was never drawn, so no sound is minted; see
    /// [`Engine::drain_sound_cues`]).
    pub fn set_engaged(&mut self, on: bool) {
        if self.engaged != on {
            self.engaged = on;
            self.reset();
        }
    }

    // -- seam points 1 and 2: events ---------------------------------------

    /// Seam points 1 and 2: everything the host tells v2. Point 1 is `spawn`,
    /// after the licence gate and `classify_move`, handing over an
    /// [`Event::Move`]; point 2 is `note_typed` / `note_erase` / `note_kill` /
    /// `note_return`.
    ///
    /// The event is buffered and applied at the next [`Engine::tick`], which is
    /// the same frame (T2). The spine's metric moves here, immediately, since
    /// it is geometry-free and this frame's births must be priced by it; its
    /// display follower advances once, in `tick`.
    ///
    /// No sound cue is minted here: a cue carries `heat` (the display spine),
    /// and minting on the edge would price it by last frame's follower while
    /// the light is priced by this frame's. Every cue is minted in
    /// [`Engine::tick`] after the one `Spine::update`. Only the kitty's Wince
    /// on an erase or kill (§8.2) is minted on the edge: a pose, priced by
    /// nothing.
    ///
    /// The mend is read on the edge (§23): a typed key asks the spine whether
    /// it is a typo fix ([`spine::Spine::mend`]) before advancing the metric
    /// (which closes the erase run), and a `Some` is held for the tick that
    /// births the key, which publishes it as [`Ctx::mend`].
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
                self.echo.forget(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Kill { .. } => {
                #[cfg(test)]
                {
                    self.kills += 1;
                }
                self.spine.drain_kill(now);
                self.echo.forget(now);
                self.offer(CompanionImpulse::Wince);
            }
            Event::Move {
                from, to, licence, ..
            } => self.on_move(from, to, licence, now),
            Event::ReducedMotion(on) => self.reduced_motion = on,
            Event::Focus(false) => self.echo.forget(now),
            // Focus is the ribbon's and the sky's to act on; a Return is the
            // key, and inert; a Sweep is the ribbon's alone. The insert's
            // rewrite moves the mirror and tells the witness the row left of
            // the caret was re-laid by the insert (`Witness::forget_left_of`),
            // and nothing else: no drain, no wince, the ledger kept.
            Event::Rewrite { row, col, .. } => {
                self.caret = (row, col);
                self.caret_known = true;
                self.witness.forget_left_of(row, col);
            }
            Event::Focus(true)
            | Event::Return
            | Event::ComposerNewline
            | Event::CancelComposerNewline
            | Event::Sweep { .. } => {}
        }
        // A flushed park's events belong before the keys pressed since. The
        // park is judged on the tick of the next key, whose `Typed` was banked
        // at its press, before the flush's `Move` and landing `Sweep` were
        // minted. Replayed in bank order, a Space after the wrap would re-anchor
        // the moved word from the wrong space and drop the landing cell the
        // flush swept (`tests/composer_box_growth_wrap.rs`). The flush's events
        // go in before the first `Typed` pressed after the park's clock.
        //
        // Consequence: a key whose echo was a parked one-cell retreat replays
        // after it, finds no echo in the frame and is held
        // ([`Engine::replay_events`]), so the witness sees the row's last glyph
        // blank under an armed cell and releases it
        // (`tests/wrapped_composer_band.rs`, `c2_bs`). A partial release takes
        // no space from between standing letters (`Witness::walk`).
        if let Some(park) = self.flush_clock
            && let Some(i) = self
                .events
                .iter()
                .position(|(e, at)| matches!(e, Event::Typed { .. }) && *at > park)
        {
            self.events.insert(i, (ev, now));
            return;
        }
        self.events.push((ev, now));
    }

    /// The clock a held park's flush is judged at ([`Engine::on_event`]):
    /// while `Some`, every event the seam pushes is ordered before the
    /// `Typed` events pressed after it. The host sets it around the flush
    /// and clears it after; `None` is the ordinary append.
    pub fn set_flush_clock(&mut self, at: Option<Instant>) {
        self.flush_clock = at;
    }

    /// The soft-wrapped caret ([`ribbon::SoftWrap`]): the seam's verdict that
    /// the licensed typed `Move` it is about to push, `origin → landing` one
    /// row down, is a key drawn at the origin while the caret alone wrapped to
    /// the continuation row's indent. `key_at` is that key's clock. The replay
    /// lays the key's `Typed` at the origin when it is in the same frame, and
    /// the ribbon keeps the word for the next key, which reflows it down.
    pub fn note_soft_wrap(&mut self, sw: ribbon::SoftWrap, key_at: Option<Instant>) {
        if self.engaged {
            self.soft_wrap = Some((sw, key_at));
        }
    }

    /// The lifted word ([`ribbon::Lift`]): the seam's verdict that the
    /// licensed typed `Move` it is about to push, `origin → landing` backward
    /// on one row, is the composer re-wrapping the word between them up onto
    /// the end of the row above. `key_at` is the clock of the Space that split
    /// it. The replay hands the verdict to the ribbon with that `Move` and
    /// holds a same-frame key with no cell (its glyph is the blank the wrap ate
    /// at the end of the row above).
    pub fn note_lift(&mut self, lift: ribbon::Lift, key_at: Option<Instant>) {
        if self.engaged {
            self.lift = Some((lift, key_at));
        }
    }

    /// The soft-wrapped word the ribbon keeps for the key that reflows it
    /// ([`ribbon::SoftWrap`]), if any: what the seam reads the glass for
    /// before it names a reflow ([`Engine::note_reflow`]).
    #[must_use]
    pub fn soft_wrap_carry(&self) -> Option<ribbon::SoftWrap> {
        self.ribbon.carry()
    }

    /// The soft-wrapped word came down: the seam's verdict that the licensed
    /// forward `Move` it is about to push, from `sw`'s landing to `to`, is the
    /// echo that carried `sw`'s word off its row (those columns are blank now).
    /// Without it the ribbon relays nothing, since a hop past a word also looks
    /// like a burst after a short word.
    pub fn note_reflow(&mut self, sw: ribbon::SoftWrap, to: (u16, u16)) {
        if self.engaged {
            self.reflow = Some((sw, to));
        }
    }

    /// The revive ([`ribbon::Revive`]): the seam's verdict that the `Sweep`
    /// `rv.col0..rv.col1` on `rv.row` it has just pushed re-lays a retired
    /// typed run's unchanged text, with that run's walk. The replay hands the
    /// walk to the ribbon with that `Sweep`.
    pub fn note_revive(&mut self, rv: ribbon::Revive) {
        if self.engaged {
            self.revive = Some(rv);
        }
    }

    /// A licensed move, seam point 1's own event. The ledger reads the mirror
    /// before the move lands on it, and the mirror follows the landing at the
    /// end. In order: the host's sweep ([`Engine::host_sweep_for`]), what the
    /// hole owes ([`Engine::echo_bridge`]), the birth floor, the bridge
    /// ([`Engine::lay_bridge`]), the type-ahead record
    /// ([`Engine::record_pre_move`]), then the mirror.
    fn on_move(&mut self, from: (u16, u16), to: (u16, u16), licence: Licence, now: Instant) {
        let host_sweep = self.host_sweep_for(from, to);
        #[cfg(test)]
        {
            self.last_host_sweep_at = host_sweep;
        }
        let bridge = self.echo_bridge(from, to, licence, host_sweep, now);
        // The ribbon's birth floor ([`SWEEP_BIRTH_FLOOR_S`]): born no earlier
        // than one stamp window before the echo, whatever clock the ledger
        // partitioned by.
        let floor = now
            .checked_sub(Duration::from_secs_f32(SWEEP_BIRTH_FLOOR_S))
            .unwrap_or(now);
        let birth = host_sweep.map(|at| at.max(floor));
        if let (Some(birth), Some((Event::Sweep { .. }, at))) = (birth, self.events.last_mut()) {
            *at = birth;
        }
        if let Some(col0) = bridge
            && (col0 < from.1 || host_sweep.is_none())
        {
            self.lay_bridge(from, to, col0, birth.unwrap_or(now), host_sweep.is_some());
        }
        self.record_pre_move(from, to, licence);
        self.caret = to;
        self.caret_known = true;
    }

    /// The host's own sweep for the move `from → to`, if it sent one: the
    /// event just before the move, covering `from..to` on the licensing key's
    /// clock. A hole takes the same clock, so the frame that echoes the key
    /// shows the whole run lit.
    fn host_sweep_for(&self, from: (u16, u16), to: (u16, u16)) -> Option<Instant> {
        match self.events.last() {
            Some(&(Event::Sweep { row, col0, col1 }, at))
                if row == to.0 && col0 == from.1 && col1 == to.1 =>
            {
                Some(at)
            }
            _ => None,
        }
    }

    /// The bridge is laid first: the hole's sweep (`col0..to` on `to`'s row,
    /// born at `birth`) goes to the front of the frame's events. The key's
    /// `Typed` and the host's sweep lay cells to the right of the hole, and a
    /// cell laid past a cohort's end mints a new cohort; laid first, the hole's
    /// cells join the cohort and every later lay joins them. The insert shifts
    /// every buffered event, and the one-shot move record with them.
    /// `host_swept` says whether the host's sweep covers `from..to` (the bridge
    /// owes only the hole) or the bridge covers the move's advance too.
    fn lay_bridge(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        col0: u16,
        birth: Instant,
        host_swept: bool,
    ) {
        let host_end = if host_swept { from.1 } else { to.1 };
        self.bridged = self.bridged.saturating_add(u32::from(host_end - col0));
        self.events.insert(
            0,
            (
                Event::Sweep {
                    row: to.0,
                    col0,
                    col1: to.1,
                },
                birth,
            ),
        );
        if let Some((index, _)) = &mut self.pre_move {
            *index += 1;
        }
    }

    /// The replay, pass 1 of the tick: the ribbon ingests the frame's events in
    /// order. Where a `Typed` key replays is the hold law:
    ///
    /// * after the frame's last echo, it is held: its cell comes with its echo
    ///   from the seam's sweep, at the mirror advanced past earlier held keys;
    /// * before the frame's first one-shot move, at the caret it was typed at
    ///   ([`Engine::pre_move`]);
    /// * the key a soft-wrapped caret echoed, at its glyph on the origin row
    ///   ([`Engine::note_soft_wrap`]), and a key typed after it but before
    ///   that move, held at the landing (the reflow lays it);
    /// * otherwise at the frame's caret.
    ///
    /// A key typed at a caret the engine never learned is held with no cell.
    /// Only [`Engine::pre_move`] carries state across the scroll and band seams
    /// ([`Engine::translate_pre_move`]).
    fn replay_events(&mut self, ctx: &Ctx<'_>) {
        // The partial releases revoked on the last tick had that tick's
        // content read to come back in; they go now, before this tick's
        // events can revoke anything else (`Ribbon::settle_revoked_parts`).
        self.ribbon.settle_revoked_parts();
        self.ribbon.set_pane(self.pane);
        self.ribbon.set_pane_rows(self.pane_rows);
        let pre_move = self.pre_move.take();
        let tail_start = self.events.len() - self.unechoed_tail().len();
        // The soft-wrapped caret the seam named ([`Engine::note_soft_wrap`]):
        // its `Move`, and the key whose echo it is, matched by clock, taking
        // the first key (the seam's stamp is the oldest in flight). A key held
        // on an earlier frame is not in this buffer; the seam's sweep lays its
        // cell at the origin.
        let soft = self.soft_wrap.take().and_then(|(sw, key_at)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to, .. } if from == sw.origin && to == sw.landing)
            })?;
            let at_key = key_at.and_then(|key| {
                self.events[..at_move]
                    .iter()
                    .position(|&(e, at)| matches!(e, Event::Typed { .. }) && at == key)
            });
            Some((sw, at_move, at_key))
        });
        // The lifted word the seam named ([`Engine::note_lift`]): its `Move`
        // and its key, matched the same way.
        let lift = self.lift.take().and_then(|(lift, key_at)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to, .. } if from == lift.origin && to == lift.landing)
            })?;
            let at_key = key_at.and_then(|key| {
                self.events[..at_move]
                    .iter()
                    .rposition(|&(e, at)| matches!(e, Event::Typed { .. }) && at == key)
            });
            Some((lift, at_move, at_key))
        });
        // The reflow the seam saw on the glass ([`Engine::note_reflow`]): its
        // `Move`, matched the same way.
        let reflow = self.reflow.take().and_then(|(sw, to)| {
            let at_move = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Move { from, to: t, .. } if from == sw.landing && t == to)
            })?;
            Some((sw, at_move))
        });
        // The revive the seam named ([`Engine::note_revive`]): its `Sweep`,
        // matched by its span.
        let revive = self.revive.take().and_then(|rv| {
            let at_sweep = self.events.iter().position(|(e, _)| {
                matches!(*e, Event::Sweep { row, col0, col1 }
                    if row == rv.row && col0 == rv.col0 && col1 == rv.col1)
            })?;
            Some((rv, at_sweep))
        });
        let mut held_cells: u16 = 0;
        let mut soft_held: u16 = 0;
        for (index, &(ev, at)) in self.events.iter().enumerate() {
            let Event::Typed { cells, .. } = ev else {
                match (soft, lift, reflow, revive) {
                    (Some((sw, at_move, _)), _, _, _) if at_move == index => {
                        self.ribbon.soft_wrap_move(&ev, at, ctx, sw);
                    }
                    (_, Some((lift, at_move, _)), _, _) if at_move == index => {
                        self.ribbon.lift_move(&ev, at, ctx, lift);
                    }
                    (_, _, Some((sw, at_move)), _) if at_move == index => {
                        self.ribbon.reflow_move(&ev, at, ctx, sw);
                    }
                    (_, _, _, Some((rv, at_sweep))) if at_sweep == index => {
                        self.ribbon.revive_sweep(&ev, at, ctx, rv);
                    }
                    _ => self.ribbon.on_event(&ev, at, ctx),
                }
                if matches!(ev, Event::Sweep { .. })
                    && let Some((row, col)) = swept_space_cell(&self.events, index)
                {
                    self.ribbon.observe_space_cell(row, col);
                }
                continue;
            };
            if index >= tail_start {
                // No echo after it: held at the mirror advanced past the keys
                // held before it — or nowhere, at an unlearned mirror.
                let cell = self
                    .caret_known
                    .then(|| (self.caret.0, self.caret.1.saturating_add(held_cells)));
                self.ribbon.hold_typed(&ev, at, ctx, cell);
                held_cells = held_cells.saturating_add(cells);
            } else if let Some((first_move, caret)) = pre_move
                && index < first_move
            {
                // Pressed before the frame's first one-shot move: replayed at
                // the caret before it, or held if that caret was never learned.
                match caret {
                    Some(caret) => self.ribbon.on_event(&ev, at, &Ctx { caret, ..*ctx }),
                    None => self.ribbon.hold_typed(&ev, at, ctx, None),
                }
            } else if let Some((sw, _, Some(at_key))) = soft
                && at_key == index
            {
                // The key's glyph stands at the origin, not left of the
                // frame's caret (the blank indent): replayed as if the
                // caret had advanced past the origin on its row.
                let caret = (sw.origin.0, sw.origin.1.saturating_add(cells));
                self.ribbon.on_event(&ev, at, &Ctx { caret, ..*ctx });
            } else if let Some((sw, at_move, Some(at_key))) = soft
                && at_key < index
                && index < at_move
            {
                // Typeahead across the wrap: a key pressed after the wrap key
                // and before its `Move` has no echo yet; its glyph comes with
                // the reflow that carries the word down. Held at the landing,
                // advanced past the keys held there before it, rather than
                // laid on the blank indent.
                let cell = (sw.landing.0, sw.landing.1.saturating_add(soft_held));
                self.ribbon.hold_typed(&ev, at, ctx, Some(cell));
                soft_held = soft_held.saturating_add(cells);
            } else if let Some((_, _, Some(at_key))) = lift
                && at_key == index
            {
                // The Space that lifted the word: its blank was eaten at the
                // end of the row above, and left of the frame's caret is the
                // blank indent — held, with no cell.
                self.ribbon.hold_typed(&ev, at, ctx, None);
            } else {
                self.ribbon.on_event(&ev, at, ctx);
            }
        }
    }

    /// The buffered events after the frame's last move or rewrite — the
    /// keys no echo has answered yet, and the edits beside them.
    fn unechoed_tail(&self) -> &[(Event, Instant)] {
        let start = self
            .events
            .iter()
            .rposition(|(ev, _)| matches!(ev, Event::Move { .. } | Event::Rewrite { .. }))
            .map_or(0, |i| i + 1);
        &self.events[start..]
    }

    /// An erase's retreat: a `Typed`-licensed same-row backward move with an
    /// `Erase` or `Kill` in the unechoed tail. The seam maps it to `Typed` so
    /// the ribbon takes its erase-retreat arm; for the replay it is a one-shot
    /// move, so a glyph typed before the retreat replays at the caret before
    /// it.
    fn is_erase_retreat(&self, from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
        licence == Licence::Typed
            && to.0 == from.0
            && to.1 < from.1
            && self
                .unechoed_tail()
                .iter()
                .any(|(ev, _)| matches!(ev, Event::Erase | Event::Kill { .. }))
    }

    /// Record the frame's first one-shot move (non-echo licence, or an erase's
    /// retreat) as `(its index in events, the caret before it)`
    /// ([`Engine::pre_move`]). Recorded after the bridge's insert, so the index
    /// is the one this move takes when pushed, and only once a licensed move
    /// has set the mirror.
    fn record_pre_move(&mut self, from: (u16, u16), to: (u16, u16), licence: Licence) {
        let one_shot = !licence.is_echo() || self.is_erase_retreat(from, to, licence);
        if one_shot && self.pre_move.is_none() {
            self.pre_move = Some((self.events.len(), self.caret_known.then_some(self.caret)));
        }
    }

    /// The late echo's cells: what a licensed typed move owes the presses
    /// before it.
    ///
    /// The seam licenses a caret move only within `0.25 s` of a keypress and
    /// refuses a later echo (it cannot tell one from program output), so a key
    /// a stalled TUI echoed late would leave a dark hole in the rainbow. This
    /// engine can tell: every [`Event::Typed`] is a real press, banked on the
    /// [`EchoLedger`] until a caret advance pays for it, and every licensed
    /// move carries the host's `from` (the last observed cell) beside the
    /// engine's mirror (the last licensed one). When a licensed typed move
    /// lands forward on the mirror's row with `from` past the mirror, and the
    /// hop plus the move's own advance is paid cell for cell by ledger presses,
    /// those cells are laid as one [`Event::Sweep`] from the mirror to the
    /// landing, on the move's own clock (T2), joining their cohort. The same
    /// sweep covers a licensed multi-cell echo the host did not sweep itself.
    ///
    /// Limits:
    ///
    /// * T1 holds: geometry is born only on this observed, licensed move.
    /// * A cell is laid only against a press that could have produced it. The
    ///   host's sweep rides the licensing press's clock (`typed_at`); younger
    ///   presses' glyphs lie right of its cell, so the hole to its left is
    ///   paid only by older presses, exactly.
    /// * A hop the older presses do not explain exactly (vim's `w`, a mouse
    ///   click, program output, a swallowed key beside a real hole) buys
    ///   nothing and forgets the ledger, so a swallowed press never rolls
    ///   forward as a phantom credit.
    /// * An unswept move is laid from the oldest presses only when it starts
    ///   at the mirror.
    /// * An unset mirror ([`Engine::caret_known`]) measures no hole: the first
    ///   licensed same-row echo is read from its own `from` and paid by the
    ///   presses in flight, so a composer's coalesced first repaint is laid; a
    ///   cold backward move keeps the presses waiting; a cold `Insert` returns
    ///   `Some(from.1)` with `spend(0)`.
    /// * The bound is the host's coalesce cap ([`ECHO_LEDGER_DEPTH`]); a press
    ///   older than [`ECHO_PATIENCE_S`] was swallowed and buys nothing.
    /// * A non-typed licence, an erase or kill, a focus loss, a row change, a
    ///   scroll and a reset all clear the ledger.
    ///
    /// A stalled batch is swept by the host on its oldest press's clock, so the
    /// whole batch is spent; a batch drained across two frames keeps its tail
    /// here for the next key. The ribbon's birth is floored separately
    /// ([`SWEEP_BIRTH_FLOOR_S`], in [`Engine::on_move`]).
    ///
    /// `key_at` is the host sweep's clock when the host swept this move.
    /// Returns the first cell of a sweep from there to the landing when the
    /// ledger paid, or `None`. The ribbon lays only what no live cell owns, so
    /// overlapping the host's sweep lays nothing twice and keeps one cohort.
    fn echo_bridge(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        licence: Licence,
        key_at: Option<Instant>,
        now: Instant,
    ) -> Option<u16> {
        self.echo.expire(now);
        // The cold start: a mirror nobody has set measures no hole, so the
        // hop is read from the host's own `from` (the move's advance only) and
        // paid as an unswept move starting at the mirror is. This keeps a
        // composer's coalesced first keys lit.
        let known = self.caret_known;
        let (mrow, mcol) = if known { self.caret } else { from };
        let insert = licence == Licence::Insert;
        if !licence.is_echo() || from.0 != mrow || to.0 != mrow {
            self.echo.clear();
            return None;
        }
        if to.1 <= from.1 {
            // A retreat, a re-anchor, a rewrite landing left of its launch:
            // not an echo's shape. The presses keep waiting.
            return None;
        }
        let Some(gap) = from.1.checked_sub(mcol) else {
            // The caret went back without a licensed move and came forward
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
            // A delivered insert swept `from..to` on the delivery clock: the
            // span is the insert's and charges nothing; the hole before it (a
            // key whose late echo was refused just before the drop) is the
            // older presses', exactly.
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

    /// Every [`Event::Kill`] this engine has received (test-only).
    #[cfg(test)]
    pub(crate) fn kills(&self) -> u32 {
        self.kills
    }

    /// The focused pane's `(first column, width)`, or `None` for the whole
    /// grid — the edges a typed fold wraps at: folded at the grid edge, a
    /// split pane's wrap lit a cell in the neighbouring pane. Stored whether
    /// or not the engine is engaged.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// The focused pane's `(first row, rows)`, or `None` for the whole grid:
    /// the floor the band's lower lobe and vivid rail may reach to, so neither
    /// crosses a split's divider.
    pub fn set_pane_rows(&mut self, pane_rows: Option<(u16, u16)>) {
        self.pane_rows = pane_rows;
    }

    /// The mirror follows the observed caret across a row. A caret relocation
    /// the licence gate declines (a TUI re-laying its input box a row up with
    /// no key behind it, a hidden→visible warp) moves the caret mirror, so the
    /// next key lays on the row under the hand.
    ///
    /// Row changes only: the same-row hop is the echo ledger's
    /// ([`Engine::echo_bridge`]), which needs the mirror to measure a refused
    /// echo's hole. A row change is where the ledger forgets anyway, so the
    /// presses are forgotten with the move ([`EchoLedger::forget`]).
    ///
    /// No event is buffered, no licence minted, no light born, no clock moved
    /// (T1). The mirror moves at the end of the tick this is reported in
    /// ([`Engine::tick`]), so a `Typed` already buffered is laid at the caret
    /// it was typed at. Inert while disengaged and until a licensed move has
    /// seeded the mirror.
    pub fn observe_caret(&mut self, to: (u16, u16)) {
        if !self.engaged || !self.caret_known || to.0 == self.caret.0 {
            return;
        }
        self.caret_seen = Some(to);
    }

    /// The content witness's walk ([`witness`]): read every resident ribbon
    /// cell against this frame's `rows` (sampled under the terminal lock after
    /// the PTY batch), arm cells first seen over a glyph, then act on the two
    /// verdicts: cells whose glyph was replaced (or whose run was judged
    /// moved) are retired on the fast melt ([`ribbon::Ribbon::retire_cells`]),
    /// and cells whose glyph went are released to the swoosh
    /// ([`ribbon::Ribbon::release_cells`]).
    ///
    /// Run right after [`Engine::tick`], so a cell laid this tick is armed on
    /// the frame it is born. Each cell is counted once on [`Status::retired`]
    /// (re-verdicts on a released cell that is then overwritten are taken off
    /// the count). Returns how many cells were newly retired or released; 0
    /// while disengaged or with no rows. The ledger is untouched.
    ///
    /// Runs moved as a block a row or two away were already carried by
    /// [`Engine::follow_rows`] at the start of the tick, and are read here on
    /// their new row; a row whose text is intact under a caret that warped
    /// away is not retired.
    pub fn witness_rows(&mut self, rows: &[RowSample<'_>], now: Instant) -> u32 {
        // This frame's withheld rows ([`Engine::follow_rows`]) are read once.
        let withheld_n = std::mem::take(&mut self.withheld_n);
        if !self.engaged || rows.is_empty() {
            return 0;
        }
        self.witness
            .restored_runs(self.ribbon.cells(), rows, &mut self.restore_scratch);
        self.ribbon
            .restore_releases(&self.restore_scratch, &mut self.restored_scratch);
        self.witness
            .restored(&self.restored_scratch, self.ribbon.cells());
        let recounted = self.witness.walk(
            self.ribbon.cells(),
            rows,
            &self.withheld[..withheld_n],
            now,
            &mut self.retire_scratch,
            &mut self.release_scratch,
        );
        let mut n = 0u32;
        if !self.retire_scratch.is_empty() {
            n = n.saturating_add(
                u32::try_from(self.ribbon.retire_cells(&self.retire_scratch, now))
                    .unwrap_or(u32::MAX),
            );
        }
        if !self.release_scratch.is_empty() {
            n = n.saturating_add(
                u32::try_from(self.ribbon.release_cells(&self.release_scratch, now))
                    .unwrap_or(u32::MAX),
            );
        }
        // A re-verdict stamps a cell an earlier walk released and this
        // count already took: the melt changed its clock, not the count.
        n = n.saturating_sub(u32::try_from(recounted).unwrap_or(u32::MAX));
        if n > 0 {
            self.retired += u64::from(n);
            self.status.retired = self.retired;
        }
        n
    }

    /// The laid ribbon, read-only — what the host's witness and the
    /// paint-conformance bind read the resident cells from.
    #[must_use]
    pub fn ribbon(&self) -> &Ribbon {
        &self.ribbon
    }

    /// The rows the witness wants, written into `out`; returns how many. The
    /// host samples exactly these under its terminal lock before the tick and
    /// hands them to [`Engine::follow_rows`] and [`Engine::witness_rows`]. `0`
    /// while disengaged; allocates nothing.
    ///
    /// * The bands' rows fill at most the first [`witness::WITNESS_ROWS`]
    ///   slots: the hand's run and the rows above and below it first, then
    ///   other resident rows and their neighbours (deduped, within the grid).
    ///   The neighbours let the follow pass see where text went when a
    ///   bottom-anchored box grew a row. Counted per cohort, matching the
    ///   walk; past the budget the surplus keeps its own clocks. The focused
    ///   row is the cohort on the caret's row, else the most recently alive.
    /// * The arming rows follow, up to [`witness::ARMING_ROWS`] more: the row
    ///   a waiting key will lay its cell on this frame (`arming`, known to the
    ///   host from `CursorGlow::ribbon_rows`) and the rows one above and below
    ///   it, so a copy of its glyph there is seen as a twin at once
    ///   (`tests/erased_under_its_twin.rs`). They have their own slots, so the
    ///   two groups never crowd each other out; the host gives
    ///   [`crate::cursor_glow::CURSOR_WITNESS_ROWS`] slots (eight bands', three
    ///   arming, the caret's row).
    ///
    /// No row two away is named: a copy of the line two rows off (a re-sorted
    /// fzf list, a quoted composer) must not be taken for where it went
    /// (`tests/copies_are_not_moves.rs`). Such a row is still sampled when it
    /// is the caret's or another band's neighbour. The host records the list
    /// it handed out, and [`Engine::follow_rows`] counts as withheld only rows
    /// of that list.
    pub fn ribbon_rows_for(&self, arming: Option<u16>, out: &mut [u16]) -> usize {
        if !self.engaged {
            return 0;
        }
        // `0` is a grid not measured yet (no tick): nothing bounds a row.
        let add = |out: &mut [u16], n: &mut usize, row: u16| {
            if (self.grid_rows == 0 || row < self.grid_rows)
                && *n < out.len()
                && !out[..*n].contains(&row)
            {
                out[*n] = row;
                *n += 1;
            }
        };
        let budget = out.len().min(witness::WITNESS_ROWS);
        let mut n = self.band_rows(&mut out[..budget], add);
        if let Some(row) = arming {
            for r in [Some(row), row.checked_sub(1), row.checked_add(1)]
                .into_iter()
                .flatten()
            {
                add(out, &mut n, r);
            }
        }
        n
    }

    /// The bands' rows of [`Engine::ribbon_rows_for`], into `out` through
    /// its `add` (deduped, bounded by the grid and by `out.len()`).
    fn band_rows(&self, out: &mut [u16], add: impl Fn(&mut [u16], &mut usize, u16)) -> usize {
        let mut n = 0usize;
        let focus = self
            .ribbon
            .cohorts()
            .iter()
            .max_by_key(|coh| (self.caret_known && coh.row == self.caret.0, coh.alive_at))
            .map(|coh| coh.row);
        if let Some(focus) = focus {
            add(out, &mut n, focus);
            for row in [focus.checked_sub(1), focus.checked_add(1)]
                .into_iter()
                .flatten()
            {
                add(out, &mut n, row);
            }
        }
        for coh in self.ribbon.cohorts() {
            add(out, &mut n, coh.row);
            if n == out.len() {
                return n;
            }
        }
        for coh in self.ribbon.cohorts() {
            for nb in [coh.row.checked_sub(1), coh.row.checked_add(1)] {
                if n == out.len() {
                    return n;
                }
                let Some(nb) = nb else {
                    continue;
                };
                add(out, &mut n, nb);
            }
        }
        n
    }

    /// The follow pass: the band follows its text. Run by the host at the
    /// start of the tick on this frame's samples (the rows
    /// [`Engine::ribbon_rows_for`] named), before the tick replays the frame's
    /// events and so before a composer's re-anchor
    /// ([`ribbon::Ribbon::re_anchor`]) stamps the old row.
    ///
    /// * Along the row first: a mid-line insert that pushed the tail right on
    ///   its own row ([`witness::Witness::shift_runs`]) moves its light with
    ///   it ([`ribbon::Ribbon::shift_run`]), so a shifted tail is never
    ///   mistaken for text that left.
    /// * Then across rows: every run whose armed glyphs left their row and
    ///   stand one row away at their own columns as a block
    ///   ([`witness::Witness::follow_runs`]) is translated there with every
    ///   clock, price and `t` intact ([`ribbon::Ribbon::translate_run`]). A
    ///   run carried away from the caret's row is set flowing
    ///   ([`ribbon::Cohort::flow`]), as a shell wrap does; a flowing run
    ///   carried back onto the caret's row is re-wet, and one carried back
    ///   onto the new-line seam's row takes the seam with it.
    ///
    /// Both passes count on [`Status::followed`]; arrivals no offset named are
    /// counted on [`Status::follow_missed`]. Returns how many cells moved.
    /// Deterministic in `(records, samples)`; allocates nothing on the steady
    /// path.
    ///
    /// `asked` is the list the host was asked for this frame (recorded by
    /// `CursorGlow::ribbon_rows`). Before either pass, rows of it missing from
    /// `rows` but inside the grid and the focused pane are kept as withheld,
    /// and [`Engine::witness_rows`] defers judging runs beside them for one
    /// walk (`witness::Witness::find_deferred_runs`); a composed host can drop
    /// rows when a PTY chunk lands between its locks. A run the shift pass
    /// carried stands under its glyphs and is not deferred. A content-proved
    /// short held park may borrow the move of its source row.
    pub fn follow_rows(&mut self, rows: &[RowSample<'_>], asked: &[u16], now: Instant) -> u32 {
        self.follow_rows_with_park(rows, asked, now, None).0
    }

    /// The same content follow, with the one exact short held park the host
    /// may be waiting to judge. The bool says this park's source ribbon run
    /// actually moved, so its delayed judgement can use re-anchor
    /// morphology without widening the ordinary one-cell park rule.
    pub(crate) fn follow_rows_with_park(
        &mut self,
        rows: &[RowSample<'_>],
        asked: &[u16],
        now: Instant,
        park: Option<((u16, u16), (u16, u16))>,
    ) -> (u32, bool) {
        self.withheld_n = 0;
        if !self.engaged || rows.is_empty() {
            return (0, false);
        }
        for &r in asked {
            let readable = r < self.grid_rows
                && self
                    .pane_rows
                    .is_none_or(|(first, n)| r >= first && r - first < n);
            if readable
                && !rows.iter().any(|s| s.row == r)
                && !self.withheld[..self.withheld_n].contains(&r)
                && self.withheld_n < self.withheld.len()
            {
                self.withheld[self.withheld_n] = r;
                self.withheld_n += 1;
            }
        }
        self.note_rewrap_starts(rows);
        // Along the row first: the caret is the one the last tick observed,
        // the column the hand edited at before this frame's echo moved it.
        let caret = self.caret_known.then_some(self.caret);
        self.witness
            .shift_runs(self.ribbon.cells(), rows, caret, &mut self.shift_scratch);
        let mut n = 0u32;
        let mut park_proved = false;
        for k in 0..self.shift_scratch.len() {
            let run = self.shift_scratch[k];
            self.moved_scratch.clear();
            let moved = self.ribbon.shift_run(run, &mut self.moved_scratch);
            if moved == 0 {
                continue;
            }
            self.witness.shift_cells(&mut self.moved_scratch, run.dc);
            n = n.saturating_add(u32::try_from(moved).unwrap_or(u32::MAX));
        }
        let score = self.witness.follow_runs_with_short_park(
            self.ribbon.cells(),
            rows,
            park,
            &mut self.follow_scratch,
        );
        if score.missed > 0 {
            self.follow_missed += u64::from(score.missed);
            self.status.follow_missed = self.follow_missed;
        }
        if self.follow_scratch.is_empty() {
            if n > 0 {
                self.followed += u64::from(n);
                self.status.followed = self.followed;
            }
            return (n, false);
        }
        // Read the park's source run once, before any follow mutates the
        // ribbon. A composer may carry only its short kept prefix to another
        // row while its long trailing word stays for the parked echo to
        // relay. The moved span can therefore be far from the caret, but it
        // must share the live run of the glyph immediately behind the park.
        let park_owner = park.and_then(|(from, to)| {
            if from.0 != to.0 || !(1..=2).contains(&from.1.saturating_sub(to.1)) {
                return None;
            }
            let last_col = from.1.checked_sub(1)?;
            self.ribbon
                .cells()
                .iter()
                .find(|cell| !cell.leaving() && cell.row == from.0 && cell.col == last_col)
                .map(|cell| (from.0, cell.cohort))
        });
        for k in 0..self.follow_scratch.len() {
            let run = self.follow_scratch[k];
            let Some(target) = run.row.checked_add_signed(run.dr) else {
                continue;
            };
            let away = !self.caret_known || target != self.caret.0;
            let park_owns_run =
                park_owner.is_some_and(|(row, cohort)| row == run.row && cohort == run.cohort);
            self.moved_scratch.clear();
            let moved = self
                .ribbon
                .translate_run(run, now, away, &mut self.moved_scratch);
            if moved == 0 {
                continue;
            }
            if away && park_owns_run {
                park_proved = true;
            }
            // The seam (`ribbon::Ribbon::seam`): the text left the caret's row
            // with the box that grew, so the input continues on the caret's
            // row, which is the new line.
            if away && self.caret_known && run.row == self.caret.0 {
                self.ribbon.arm_seam(self.caret.0, now);
            }
            self.witness
                .translate_cells(&mut self.moved_scratch, run.dr);
            // The echo that crossed with its text: a key typed as the composer
            // grew a row in the same frame arrives as the cross-row move
            // `(R, c) → (R+dr, c')`, so neither the host's sweep nor the
            // bridge laid it. In the text's own frame it is the same-row
            // advance `c → c'` on the row the run now stands on; that sweep is
            // laid here, on the move's clock (owner, 2026-09-27).
            if self.caret_known && self.caret.0 == target {
                self.lay_crossed_echo(run, target);
            }
            n = n.saturating_add(u32::try_from(moved).unwrap_or(u32::MAX));
        }
        if n > 0 {
            self.followed += u64::from(n);
            self.status.followed = self.followed;
        }
        (n, park_proved)
    }

    /// Bind one content-proved held park to the move its flush just queued.
    /// The ribbon consumes the exact pair during the next replay and clears
    /// any unused proof when that frame ends.
    pub(crate) fn prove_short_reanchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        self.ribbon.prove_short_reanchor(from, to, at);
    }

    /// The frame's buffered echo move that crossed rows with its run
    /// ([`Engine::follow_rows`]): from `(run.row, c)` (at the run or one past
    /// its end) forward to `(target, c')`, with no sweep already laid on
    /// `target` over `c..c'`. Its advance is laid at the front of the frame
    /// like the bridge ([`Engine::lay_bridge`]), on the move's clock, and the
    /// move is re-seated on the row its text now stands on.
    fn lay_crossed_echo(&mut self, run: witness::FollowRun, target: u16) {
        let found = self.events.iter().position(|(ev, _)| match *ev {
            Event::Move {
                from, to, licence, ..
            } => {
                licence.is_echo()
                    && from.0 == run.row
                    && to.0 == target
                    && to.1 > from.1
                    && (run.lo..=run.hi.saturating_add(1)).contains(&from.1)
            }
            _ => false,
        });
        let Some(k) = found else {
            return;
        };
        let (Event::Move { from, to, .. }, at) = self.events[k] else {
            return;
        };
        let swept = self.events.iter().any(|(ev, _)| {
            matches!(*ev, Event::Sweep { row, col0, col1 }
                if row == target && col0 <= from.1 && col1 >= to.1)
        });
        if let (Event::Move { from: f, .. }, _) = &mut self.events[k] {
            f.0 = target;
        }
        if swept {
            return;
        }
        self.bridged = self.bridged.saturating_add(u32::from(to.1 - from.1));
        self.events.insert(
            0,
            (
                Event::Sweep {
                    row: target,
                    col0: from.1,
                    col1: to.1,
                },
                at,
            ),
        );
        if let Some((index, _)) = &mut self.pre_move {
            *index += 1;
        }
    }

    /// For the frame's buffered same-row typed re-anchor (a composer's wrap
    /// landing left of its origin), tell the ribbon where the landing row's
    /// sample holds its first text: the first column at or past the pane's
    /// left edge that is neither blank nor a wide glyph's continuation, left
    /// of the landing ([`ribbon::Ribbon::note_rewrap_start`]). A row the host
    /// did not sample says nothing.
    fn note_rewrap_starts(&mut self, rows: &[RowSample<'_>]) {
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let found = self.events.iter().rev().find_map(|(ev, _)| match *ev {
            Event::Move {
                from, to, licence, ..
            } if licence == Licence::Typed && from.0 == to.0 && to.1 < from.1 => Some((from, to)),
            _ => None,
        });
        let Some((from, to)) = found else {
            return;
        };
        let Some(sample) = rows.iter().find(|r| r.row == to.0) else {
            return;
        };
        let start = (pane_col0..to.1).find(|&c| {
            sample
                .cols
                .get(usize::from(c))
                .is_some_and(|&ch| ch != ' ' && ch != '\0')
        });
        if let Some(start) = start {
            self.ribbon.note_rewrap_start(from, to, start);
        }
    }

    /// The verdict: a shell command came back green after a long run, reported
    /// by the host on the OSC 133/633 `D` it already dedupes. Not a cursor
    /// event, so not an [`Event`].
    ///
    /// No light is born here: every light has a keystroke behind it. This only
    /// opens [`spine::Spine::note_verdict`]'s door, so the first key typed
    /// inside [`spine::VERDICT_WINDOW_S`] is born at full momentum. Gated on
    /// `engaged`.
    pub fn note_verdict(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.spine.note_verdict(now);
    }

    /// The kitty's Delight edge, reported by the host (not a cursor event).
    /// §5.8 lists Delight among the things that earn a hero: this mints
    /// [`CompanionImpulse::Delight`] and buffers the heroes
    /// [`companion::heroes_earned`] says it earns, at `(row, col)`, to be dealt
    /// on the next [`Engine::tick`]. An earned hero is always an m1; it chimes
    /// only if a token is available (D5).
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

    /// Pin the spine to at least `floor` while a sing-along is armed: the one
    /// momentum bypass (§7.2's celebration arm), routed through the same
    /// follower so every legibility cap holds. Gated like every seam point
    /// (D14).
    pub fn celebrate(&mut self, now: Instant, floor: f32) {
        if !self.engaged {
            return;
        }
        self.spine.drive(now, floor);
    }

    /// The party (§27): the host reports a sing-along bar (or the armed
    /// celebration's drop), `n` stars fanned ROYGBIV from the caret with the
    /// burst `ring` on every fourth bar, and the next tick mints it at the
    /// caret ([`meteor::Meteors::party`]). One per frame (a later call wins);
    /// inert unless engaged. The keystroke behind it is the one that armed the
    /// sing-along.
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

    /// Seam point 3, the emit phase: advance every clock and write this frame
    /// into `out`.
    ///
    /// ## Tick order
    ///
    /// 1. every clock advances once (spine, bucket, the flare's age);
    /// 2. the ribbon ingests the frame's events (lays, retracts, abandons,
    ///    moves its caret mirror);
    /// 3. the ribbon plans this frame's geometry and field index (§18), and
    ///    [`Ctx::caret_t`] is sampled here, so the caret, the head and a
    ///    star's halo read one value (C2) and the meteor locks to the landing
    ///    cell's field (D4);
    /// 4. the meteor classifies each move against that context and its spawn
    ///    hands back what the seam must wire ([`Engine::on_spawn`]); a same-row
    ///    keyed hop under the flight floor mints its nav tick here beside its
    ///    mini-fan (§6.12), priced by the same `ctx.disp`;
    /// 5. stardust deals the frame's births and earned heroes against the
    ///    planned bands (§5.4);
    /// 6. emit.
    ///
    /// ## Emit order (§6.5, §18)
    ///
    /// `meteor → ribbon (+ hot edge) → stardust → caret seam`. Truncation
    /// sheds from the end of each stream, so the meteor goes first (on a
    /// monster jump the train alone can approach the quad budget), and the
    /// sky, the layer that may be thinned, is the last emitter of halos.
    ///
    /// ## Idle → zero (T6)
    ///
    /// A disengaged or idle engine writes nothing and leaves `out.fp == 0`.
    pub fn tick(&mut self, now: Instant, geom: Geom, cfg: &Config, out: &mut Frame<'_>) {
        out.caret = self.caret_seam;
        out.companion = self.companion;
        out.fp = 0;
        if !self.engaged {
            self.events.clear();
            self.pre_move = None;
            self.soft_wrap = None;
            self.lift = None;
            self.reflow = None;
            self.revive = None;
            self.earned.clear();
            // A bar banked while the style was live is a fact about a surface
            // that is gone: dropped with the events, so re-engaging can never
            // open on a celebration nobody is still holding a key for.
            self.party = None;
            self.mend = None;
            self.caret_seen = None;
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
        // The grid's height, for the neighbour rows [`Engine::ribbon_rows_for`]
        // names before the next tick.
        self.grid_rows = u16::try_from(geom.rows).unwrap_or(u16::MAX);
        // The cell a Backspace emptied is the caret as of its tick — the same
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
        // The flow run climbs here (§23 "Flow state"), after the one
        // `Spine::update` and before anything reads the spine, so the keys are
        // priced at the number their own light is bought with.
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
            caret_walk: None,
            mend,
            surge: self.spine.surge(),
            flow: self.spine.flow(),
        };

        // Pass 1 — the ribbon ingests, then plans. The field index exists
        // from here on, and the caret's own `t` is this frame's.
        self.replay_events(&ctx);
        self.ribbon.plan(&ctx);
        ctx.caret_t = self.ribbon.field_at_caret();

        // The hand's floor (`ribbon::Ribbon::hand_floor`), read once per frame
        // after the ribbon replayed this frame's events (so it includes the
        // jump's own relocation): the landing minted below is bounded by where
        // the hand has been on that row (`meteor::spark_room`).
        self.meteor.set_hand_floor(self.ribbon.hand_floor_cell());

        // Pass 1b: the meteor classifies each move against this frame's field;
        // a spawn hands back what the seam must wire (§7.2, §12, §13). D4 is
        // read per move: `t_land` is the field at that move's landing cell, so
        // two jumps coalesced into one frame each lock to their own landing. A
        // landing off the laid ribbon falls back to the caret's answer. Every
        // event reaches the meteor (a focus loss retires its flights); only a
        // move re-reads the field and binds a licence.
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
            // The band's walk at the landing, so the landing's marks continue
            // the band at its own pace.
            move_ctx.caret_walk = self.ribbon.walk_origin_at(to.0, to.1);
            if let Some(spawn) = self.meteor.on_event(&ev, at, &move_ctx) {
                self.on_spawn(&spawn, licence, &move_ctx);
            }
            if sub_floor_hop(from, to, licence) {
                // §11, §12.3, D17: the nav tick, on the frame its mini-fan is
                // sown — `heat` is this frame's one spine, `hue` is C2 at the
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

        // Pass 2 — deal. Stars are born against the planned bands (§5.4):
        // the frame's events, then the heroes the host earned (§5.8).
        for &(ev, at) in &self.events {
            self.stardust.on_event(&ev, at, &ctx, &self.ribbon);
        }
        for &(cell, at) in &self.earned {
            self.stardust.earn_hero(at, cell, &ctx, &self.ribbon);
        }
        // The party (§27): minted here, beside `earned` and before
        // `meteor::emit` and `meteor::sow_into`, so a ring bar's burst draws
        // and the fan's stars reach the sky on the bar's own frame. Gated on
        // `caret_known`: a fan at a caret nobody observed would land top-left
        // with no key behind it.
        if let Some((at, n, ring)) = self.party.take()
            && self.caret_known
        {
            self.meteor.party(at, n, ring, &ctx);
        }
        // The combo ladder's top rung (§23 "Flow state"): every 64th key after
        // the first gold hero fires the one-frame caret flare (§7.1), never
        // under reduced motion. The first 64th pays its gold m1 in the sky and
        // does not flash.
        if keys > 0
            && !cfg_live.reduced_motion
            && ctx.flow.combo > stardust::LADDER_GOLD_KEYS
            && ctx.flow.combo.is_multiple_of(stardust::LADDER_GOLD_KEYS)
        {
            self.caret_seam.flare_at = Some(now);
        }
        // The hole tripwire's event ring: the frame's events, bridge sweeps
        // included, before they are dropped. Nothing when unarmed.
        self.remember_for_holes();
        self.events.clear();
        self.earned.clear();
        // An unlicensed caret observation on another row
        // ([`Engine::observe_caret`]) takes effect now that this tick's events
        // are laid at the caret they were typed at. The presses waiting for
        // their echo were typed on the row it left and are forgotten, as a
        // licensed row change forgets them.
        if let Some(seen) = self.caret_seen.take() {
            self.caret = seen;
            self.echo.forget(now);
        }

        // ---- emit, in §6.5's order ----
        let (u0, o0, h0) = (out.under.len(), out.out.len(), out.halos.len());
        let hidden = std::mem::take(&mut self.hidden);
        self.meteor.emit(&ctx, out);
        // The meteor mints its shed fragments, landing fan and mini-fan (§6.5
        // layers 5 and 11, §6.12) and hands them to the one star producer
        // here, after the plan and the deal and before the ribbon and sky
        // draw, so a fragment born this frame is on glass this frame.
        self.meteor.sow_into(&mut self.stardust, &ctx);
        self.ribbon.emit(&ctx, out);
        self.stardust.emit(&ctx, out);
        if hidden {
            // Hidden (A4): every clock ran; nothing reaches the glass.
            out.under.truncate(u0);
            out.out.truncate(o0);
            out.halos.truncate(h0);
        }
        // The hole tripwire, on the planned frame. Nothing when unarmed.
        self.sense_holes_after_emit(now, geom);

        // The caret seam rides out with the frame; v2 does not own the block
        // fill (§7.1). `paint` is the pure read at this `now`, on the spine
        // this tick evaluated.
        self.caret_seam.field_t = ctx.caret_t;
        self.caret_seam.paint = self.caret_paint(now);
        out.caret = self.caret_seam;
        out.cues.append(&mut self.pending_cues);
        out.companion = self.companion;

        // The fingerprint folds only what this tick appended, so a host that
        // shares the scratch with another producer still gets a v2 answer.
        self.fp = liveness(&out.under[u0..], &out.out[o0..], &out.halos[h0..]);
        out.fp = self.fp;
        self.status = Status {
            quads: ((out.under.len() - u0) + (out.out.len() - o0)) as u32,
            halos: (out.halos.len() - h0) as u32,
            stars: self.stardust.live() as u32,
            meteors: self.meteor.live() as u32,
            cells: self.ribbon.live_cells() as u32,
            bridged: self.bridged,
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            disp: self.spine.disp(),
            flow: ctx.flow,
            tokens: self.stardust.budget.tokens(),
            fp: self.fp,
        };
        // Seam point 8's answer for this frame: is anything moving?
        self.brisk = self.meteor.brisk(now) || self.ribbon.brisk(now) || self.stardust.brisk(now);
    }

    /// What a spawn wires (§6.7's arrival edge): the kitty's impulse (§7.2),
    /// the glint bucket's emptying (§13), the caret's flare and re-lit paint
    /// (§7.1), and the one sound cue of §12. Minted in the engine, which alone
    /// holds both the meteor's `cells` and the licence that picks the gesture:
    ///
    /// | licence | cue | kitty | flare |
    /// |---|---|---|---|
    /// | `Nav` / `Synthetic` | [`SoundKind::Meteor`]: tick, core, whoosh, bell at `t₀ + T`, five rain glints | `Meteor` (or `Land` under reduced motion) | yes |
    /// | `Return` | [`SoundKind::Enter`]: the cadence at `flight_ms(cells)` (D9) | as above | yes |
    /// | `Pty` | [`SoundKind::Jump`]: the cascade lane (D18) | none (§8.2: no credit) | no |
    ///
    /// `cells` on the cue is [`Spawn::cells`] unchanged, so the synth's
    /// `flight_ms(cells)` and the pixels' `t_flight` agree to the bit (§8.1
    /// no. 1). `armed` is always `false`: the `MeteorArm` credit is the host's
    /// and off by default (D11).
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
            // The arrival edge is minted once and read by the pin, the ring,
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
        // §13: a meteor empties the typing bucket on its move edge, so nothing
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

    /// Seam point 4 (`rainbow_field`): the caret's own position in the field,
    /// and §6.4's `t_land`. C2: the caret, the ribbon head and a star's halo on
    /// that cell share a colour on the same frame.
    #[must_use]
    pub fn field(&self) -> f32 {
        self.ribbon.field_at_caret()
    }

    /// Seam point 4 (`rainbow_field_at`): the field at an arbitrary cell, or
    /// `None` where no ribbon light is laid. O(1) in columns (§18).
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.ribbon.field_at(row, col)
    }

    /// Seam point 5 (`rainbow_head_rgb`): the source RGB of the ribbon head's
    /// body, before premultiplication, which a companion cursor inherits.
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.ribbon.head_rgb(cfg)
    }

    /// Seam point 6 (`momentum_display`): the eased spine.
    #[must_use]
    pub fn momentum_display(&self) -> f32 {
        self.spine.disp()
    }

    /// `trail status`'s `ribbon_segments=` while v2 owns the frame: the
    /// planned boundaries lit on the last frame ([`Ribbon::lit_segments`]) —
    /// what is on the glass, not what is resident. `Status::cells` keeps the
    /// resident count.
    #[must_use]
    pub fn ribbon_segments(&self) -> usize {
        self.ribbon.lit_segments().count()
    }

    /// `trail status`'s `ribbon_drawn=` while v2 owns the frame: how many of
    /// the last frame's quads composite to band ink by the glass census
    /// ([`Ribbon::ink_quads`]), measured on the pixels beside
    /// [`Engine::ribbon_segments`]'s modelled claim.
    #[must_use]
    pub fn ribbon_drawn(&self) -> usize {
        self.ribbon.ink_quad_count()
    }

    /// `trail status`'s `ribbon_curtain_ms=`: milliseconds left in a falling
    /// curtain, `None` when none is.
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        self.ribbon.curtain_left_ms(now)
    }

    /// The ribbon's laid cells, live or retracting — the count a test reads
    /// to pin "laid once".
    #[cfg(test)]
    pub(crate) fn laid_cells(&self) -> usize {
        self.ribbon.live_cells()
    }

    /// The caret mirror (the last licensed move's landing) — the cell a
    /// `Typed` key lays against; what a test reads to pin where the ledger
    /// and the keys' replay stand.
    #[cfg(test)]
    pub(crate) fn caret_mirror(&self) -> (u16, u16) {
        self.caret
    }

    /// The events buffered for the next frame, in the order the host handed
    /// them over — read before the tick that drains them.
    #[cfg(test)]
    pub(crate) fn pending_events(&self) -> &[(Event, Instant)] {
        &self.events
    }

    /// `trail status`'s `ribbon_hue_bands=` while v2 owns the frame: distinct
    /// spectrum bands (twenty buckets) among the lit boundaries, on the
    /// reflected position they are painted at (`tri`, §3.1). Two boundaries in
    /// two bands is what `ribbon_active` reads as a ribbon.
    #[must_use]
    pub fn ribbon_hue_bands(&self) -> usize {
        let mut seen = [false; 20];
        for seg in self.ribbon.lit_segments() {
            let band = (meteor::tri(seg.t) * seen.len() as f32) as usize;
            seen[band.min(seen.len() - 1)] = true;
        }
        seen.iter().filter(|seen| **seen).count()
    }

    /// Seam point 7 (`rainbow_phase`): the family's shared phase ring. Hosts
    /// pass it to `CursorRainbow::tick_with_family_phase` so the caret, its
    /// halo and the ribbon under that cell resolve one spectrum position.
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
    #[cfg(test)]
    pub fn budget(&self) -> &StarBudget {
        &self.stardust.budget
    }

    /// The glyph probe (§5.4's hard gate), written by the host before seam
    /// point 3 from the same row probes it takes for the ledger. An unwritten
    /// row is unknown and no sky star is born over it. Free transients (fan,
    /// shed, mini-fan) are only kept off a cell the probe says is inked.
    pub fn probe_mut(&mut self) -> &mut GlyphProbe {
        self.stardust.probe_mut()
    }

    /// Read-only view of the frame's glyph truth.
    #[must_use]
    #[cfg(test)]
    pub fn probe(&self) -> &GlyphProbe {
        self.stardust.probe()
    }

    /// The caret's paint at `now` (§7.1, [`CaretSeam::paint`]): the display
    /// spine as of the last tick, floored by the landing re-light aged to
    /// `now` (held for one [`SPRING_SNAP_RESPONSE_S`], then cooling on it,
    /// exactly `0` past `RELIGHT_LIFE_S`). With no landing live it equals
    /// [`Engine::momentum_display`] to the bit.
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

    /// Seam point 8: whether this engine needs the frame cadence. `true` while
    /// the last frame carried brisk light: per-frame motion (a head in flight,
    /// a landing's pin or burst, a thrown or dragged star, a mark retracting
    /// into the caret, a cell inside its 18 ms `edge-in`). Tails (half-life
    /// fades, the swoosh, a twinkling star past its hold, the spine settling)
    /// are paced by [`Engine::next_change_deadline`] instead, no sooner than
    /// [`TAIL_FLOOR`]. `false` is T6's first half and lets the host idle.
    #[must_use]
    pub fn needs_frame_cadence(&self) -> bool {
        self.engaged && self.brisk
    }

    /// Seam point 9: the next instant this engine changes what is on glass, or
    /// `None` when nothing is live (T6's second half). The `min` of the three
    /// producers' cadence laws ([`Cadence`]) plus the spine's own settle under
    /// a laid ribbon, never sooner than [`ARM_MIN`] after `now`.
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

    /// The last frame's liveness, which `is_active` follows when engaged
    /// (§17.2): `0` exactly when nothing is on glass. Not a content hash (see
    /// [`liveness`]); compare the streams themselves to compare frames.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fp
    }

    // -- seam points 10-12: drains, reset, status ---------------------------

    /// Seam point 10 (`take_cursor_cat_motion_pulse`): read and clear the
    /// pending companion impulse (§7.2). [`Frame::companion`] is a
    /// non-consuming report of the same slot.
    pub fn take_companion_impulse(&mut self) -> Option<CompanionImpulse> {
        self.companion.take()
    }

    /// The resident pet's receiving end (D13): the perk edge, the star within
    /// reach, and the ribbon's colour under the cat, as one value. The host
    /// reads it after its own pet tick:
    ///
    /// ```ignore
    /// let offer = v2.pet_offer(geom, companion::PetOnGlass::of(&pet_frame, geom));
    /// if let Some(star) = pet.note_v2_offer(&offer) { v2.catch_star(star, now); }
    /// ```
    ///
    /// A pure read that arms nothing: every field is state a producer already
    /// owns this frame, so an idle engine offers `PetOffer::default()` (T6).
    /// The cost is one scan of the ≤ 48-slot star pool, no allocation (§18).
    ///
    /// `pet` is the cat as its own last frame published it, or `None`. v2
    /// never derives a pet position (§7.2(b): the pet seats itself).
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
        // scan only notices it. The newest match wins — the panel's offer is
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

    /// The cat catches the star: spend the offered star's remaining life on
    /// the frame the paw lands, on the sky's 40 ms finish
    /// ([`stardust::CATCH_FINISH_MS`]). Returns whether the star was still
    /// there.
    ///
    /// `star` is the value [`Engine::pet_offer`] handed over and `at` is the
    /// paw's landing instant, on the pet's clock. The star is found by identity
    /// and finished once ([`stardust::Stardust::catch`]); a stale offer, dead
    /// star or second call does nothing. No light is born (T1).
    pub fn catch_star(&mut self, star: companion::StarCatch, at: Instant) -> bool {
        self.stardust.catch(star.x, star.y, star.born, at)
    }

    /// Seam point 11 (`drain_sound_cues`): drain cues minted but not yet
    /// flushed into a [`Frame`].
    ///
    /// Empty between ticks by construction: every cue is minted inside
    /// [`Engine::tick`], after the frame's one `Spine::update` (see
    /// [`Engine::on_event`]), and the same tick appends the queue to
    /// [`Frame::cues`]. A non-ticking path (style switch, [`Engine::reset`],
    /// shutdown) therefore drops nothing minted; an event reported but never
    /// ticked has neither light nor sound. Kept as seam point 11's twin and so
    /// tests can prove the queue empty after a reset.
    pub fn drain_sound_cues(&mut self) -> std::vec::Drain<'_, SoundCue> {
        self.pending_cues.drain(..)
    }

    /// The curtain: the coordinate space ended (alt-screen entry or exit, tab
    /// switch, a resize without a reflow map, a content invalidation). Instead
    /// of [`Engine::reset`]'s one-frame cut, the ribbon's cohorts are drawn
    /// into the caret over `ribbon::CURTAIN_S` (0.24 s) with nothing laid
    /// until it is over ([`ribbon::Ribbon::curtain`]). Everything else is what
    /// a reset does: the spine, the sky and the flights, the caret mirror, the
    /// echo ledger, the buffered events and the witness's records. Inert while
    /// disengaged.
    pub fn curtain(&mut self, now: Instant) {
        if !self.engaged {
            return;
        }
        self.ribbon.curtain(now);
        // A hide armed and not yet taken belongs to a frame that is not
        // coming (see [`Engine::hide_next_frame`]).
        self.hidden = false;
        self.spine.reset();
        self.stardust.reset();
        self.meteor.reset();
        self.caret_seam = CaretSeam::default();
        self.caret = (0, 0);
        self.caret_known = false;
        self.caret_seen = None;
        self.echo.clear();
        self.bridged = 0;
        self.erased = (0, 0);
        self.mend = None;
        self.events.clear();
        // And the record that indexed them, as [`Engine::reset`] drops it:
        // `pre_move` indexes the buffer just cleared and holds a caret of the
        // space that ended.
        self.pre_move = None;
        self.soft_wrap = None;
        self.lift = None;
        self.reflow = None;
        self.revive = None;
        self.earned.clear();
        self.party = None;
        self.pending_cues.clear();
        self.companion = None;
        self.paint_at = None;
        self.fp = 0;
        self.witness.clear();
        self.retire_scratch.clear();
        self.status = Status {
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            ..Status::default()
        };
    }

    /// Hide the next frame: the viewport is showing history, whose rows are
    /// not the rows the ribbon was laid on. The next [`Engine::tick`] runs
    /// every clock and writes nothing (no quad, no halo, a zero fingerprint);
    /// state is kept, so on return the band is where its clocks say.
    ///
    /// Arm it only where a tick follows in the same frame: the flag survives
    /// to whatever tick comes next. [`Engine::reset`] and [`Engine::curtain`]
    /// drop an un-taken arm, and the host's non-ticking history path checks
    /// [`Engine::engaged`] instead (`TornBandLaw::Keep`).
    pub fn hide_next_frame(&mut self) {
        if self.engaged {
            self.hidden = true;
        }
    }

    /// Whether a hide is armed and not yet taken — the twins' witness for
    /// the paragraph above.
    #[must_use]
    pub fn hide_armed(&self) -> bool {
        self.hidden
    }

    pub fn reset(&mut self) {
        // A hide armed and not yet taken belongs to a frame that is not
        // coming (see [`Engine::hide_next_frame`]).
        self.hidden = false;
        self.spine.reset();
        self.ribbon.reset();
        self.stardust.reset();
        self.meteor.reset();
        self.caret_seam = CaretSeam::default();
        // The caret is learned from the next observed move; a key before it
        // lays nowhere rather than at a cell the reset forgot the meaning of.
        self.caret = (0, 0);
        self.caret_known = false;
        self.caret_seen = None;
        self.echo.clear();
        self.bridged = 0;
        self.erased = (0, 0);
        self.mend = None;
        self.events.clear();
        // …with the record that indexed them.
        self.pre_move = None;
        self.soft_wrap = None;
        self.lift = None;
        self.reflow = None;
        self.revive = None;
        self.earned.clear();
        // …and a banked bar, for the same reason this seam forgets the caret
        // two lines up: `reset` is the coordinate-space cut, so a party held
        // across it would mint at `(0, 0)` in the new space — a fan, and
        // every fourth bar a shockwave, in the corner, with no key behind it.
        self.party = None;
        self.pending_cues.clear();
        self.companion = None;
        self.paint_at = None;
        self.fp = 0;
        // The witness's records are about cells that are gone with the
        // coordinate space; its tally is the window's, like `spawns`.
        self.witness.clear();
        self.retire_scratch.clear();
        self.status = Status {
            retired: self.retired,
            followed: self.followed,
            follow_missed: self.follow_missed,
            ..Status::default()
        };
        self.brisk = false;
    }

    /// Seam point 12 (`translate_scroll_state`): move every live mark with the
    /// viewport: the ribbon's cells, the meteors and their landings, and the
    /// stars (window-absolute px that would otherwise hang a row off their
    /// text). Marks that leave the grid are dropped, not clamped. The caret
    /// cell moves by the same rows until the host's next observed move. The
    /// glyph probe is dropped by the sky, not shifted; the host re-probes
    /// before the next deal (§5.4).
    pub fn translate_scroll(&mut self, rows: u16, cell_h: u16) {
        if !self.engaged || rows == 0 {
            return;
        }
        self.ribbon.translate_scroll(rows);
        // The rows the scroll brought in are at the bottom of the region
        // that scrolled: the focused pane's, in a split, not the window's.
        let bottom = self
            .pane_rows
            .map_or(self.grid_rows, |(first, n)| first.saturating_add(n));
        self.witness.translate(rows, bottom);
        self.meteor.translate_scroll(rows, cell_h);
        self.stardust.translate_scroll(rows, cell_h);
        // The caret is a position, not a mark: it cannot be dropped, and the
        // host observes it again on its next move. The presses waiting for
        // their echo were on a row that just moved: forgotten, not shifted.
        self.caret.0 = self.caret.0.saturating_sub(rows);
        self.echo.clear();
        self.translate_events(|r| r.checked_sub(rows), |r| r.saturating_sub(rows));
        self.translate_pre_move(|r| r.checked_sub(rows));
    }

    /// The buffered events move with the marks: a park flushed by the scroll
    /// or band hook buffers events in pre-translation rows. A buffered `Sweep`
    /// or `Rewrite` is a mark (`mark`: translated, dropped past the edge); a
    /// `Move`'s ends are positions (`pos`: saturated, like the caret);
    /// coordinate-free events are untouched.
    fn translate_events(&mut self, mark: impl Fn(u16) -> Option<u16>, pos: impl Fn(u16) -> u16) {
        self.events.retain_mut(|(ev, _)| match ev {
            Event::Sweep { row, .. } | Event::Rewrite { row, .. } => match mark(*row) {
                Some(r) => {
                    *row = r;
                    true
                }
                None => false,
            },
            Event::Move { from, to, .. } => {
                from.0 = pos(from.0);
                to.0 = pos(to.0);
                true
            }
            _ => true,
        });
    }

    /// The pre-move record is a cell, so it rides the seams too.
    /// [`Engine::pre_move`] holds `(index, the caret before the frame's first
    /// one-shot move)` and [`Engine::replay_events`] lays every `Typed`
    /// buffered before that index at that caret. `CursorGlow::note_scroll`
    /// flushes a park it cannot carry before it translates, so the record can
    /// hold a pre-scroll caret.
    ///
    /// A record carried off is forgotten, not clamped (the mark law, as in
    /// [`ribbon::Ribbon::translate_scroll`]): nobody re-observes it, so a
    /// saturated one would replay the key onto a line nobody typed. It keeps
    /// its index and the key is held with no cell until its echo pays for it;
    /// dropping the whole record would let the keys fall through to the
    /// post-move caret.
    fn translate_pre_move(&mut self, mark: impl Fn(u16) -> Option<u16>) {
        if let Some((_, caret)) = &mut self.pre_move
            && let Some((row, col)) = *caret
        {
            *caret = mark(row).map(|row| (row, col));
        }
    }

    /// Seam point 12 (`translate_band_state`), the band twin of
    /// [`Engine::translate_scroll`]: screen rows `top..=bottom` moved by
    /// `delta` ([`crate::cursor_glow::band_row`]), so every live mark on those
    /// rows moves with its text (ribbon cells, cohorts and field index;
    /// meteors, landings and sown fans; stars and veils), marks outside stand
    /// still, and marks carried past the band's edge are dropped. The caret
    /// and the erased cell are positions ([`crate::cursor_glow::band_pos`]),
    /// saturated at the edge. Earned cells follow the mark law.
    ///
    /// The spine is untouched: a program scrolling lines past the composer
    /// while the hand types must not reset momentum. The buffered events ride
    /// the band ([`Engine::translate_events`]) and the glyph probe is dropped
    /// as on a scroll; the host re-probes the caret's rows before the next
    /// deal.
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
        self.witness.translate_band(top, bottom, delta);
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
        self.translate_events(
            |r| band_row(r, top, bottom, delta),
            |r| band_pos(r, top, bottom, delta),
        );
        self.translate_pre_move(|r| band_row(r, top, bottom, delta));
    }

    /// Seam point 12 (`trail_status`): the `v2_quads=` / `v2_halos=` /
    /// `v2_stars=` rows.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }
}

/// Locate the last Space in an exactly accounted, same-row typed echo.
///
/// Every key is replayed against the final frame caret. A Space followed by
/// another key in that frame therefore cannot use `caret.col - 1` as its
/// position: that is the later glyph. The ordered keys immediately before a
/// sweep supply widths, and the immediately following typed move authenticates
/// its endpoints. Only an exact match can refine the ribbon's word boundary.
/// Partial echoes, inserts, navigation, intervening edits, and unmatched widths
/// retain the ordinary path; none gains guessed glyph positions or credit.
///
/// This reads frame-local events, allocates nothing and retains no new state.
/// Each sweep visits only its preceding typed run, so the frame pass is linear.
fn swept_space_cell(events: &[(Event, Instant)], index: usize) -> Option<(u16, u16)> {
    let Event::Sweep { row, col0, col1 } = events.get(index)?.0 else {
        return None;
    };
    let Event::Move {
        from,
        to,
        licence: Licence::Typed,
        ..
    } = events.get(index.checked_add(1)?)?.0
    else {
        return None;
    };
    if from != (row, col0) || to != (row, col1) || col1 <= col0 {
        return None;
    }
    let mut remaining = col1 - col0;
    let mut space = None;
    for &(event, _) in events[..index].iter().rev() {
        let Event::Typed { cells, class, .. } = event else {
            break;
        };
        if cells == 0 || (class == TypedClass::Space && cells != 1) {
            return None;
        }
        remaining = remaining.checked_sub(cells)?;
        if space.is_none() && class == TypedClass::Space {
            space = Some(col0 + remaining);
        }
    }
    if remaining == 0 {
        space.map(|col| (row, col))
    } else {
        None
    }
}

#[cfg(test)]
mod swept_space_tests {
    use super::*;

    fn key(cells: u16, class: TypedClass) -> Event {
        Event::Typed {
            cells,
            shifted: false,
            class,
        }
    }

    fn batch(keys: &[Event], width: u16, licence: Licence) -> Vec<(Event, Instant)> {
        let now = Instant::now();
        let mut events: Vec<_> = keys.iter().map(|&event| (event, now)).collect();
        events.push((
            Event::Sweep {
                row: 4,
                col0: 40,
                col1: 40 + width,
            },
            now,
        ));
        events.push((
            Event::Move {
                from: (4, 40),
                to: (4, 40 + width),
                licence,
                dir: Dir::Right,
            },
            now,
        ));
        events
    }

    #[test]
    fn a_swept_space_uses_the_last_space_and_counts_wide_glyph_cells() {
        let keys = [
            key(2, TypedClass::Glyph),
            key(1, TypedClass::Space),
            key(1, TypedClass::Glyph),
            key(1, TypedClass::Space),
            key(2, TypedClass::Glyph),
        ];
        let events = batch(&keys, 7, Licence::Typed);
        assert_eq!(swept_space_cell(&events, keys.len()), Some((4, 44)));
    }

    #[test]
    fn a_swept_space_requires_the_complete_key_run_and_typed_move() {
        let keys = [key(1, TypedClass::Space), key(1, TypedClass::Glyph)];
        for licence in [
            Licence::Insert,
            Licence::Nav,
            Licence::Return,
            Licence::Pty,
            Licence::Synthetic,
        ] {
            assert_eq!(swept_space_cell(&batch(&keys, 2, licence), 2), None);
        }
        for width in [0, 1, 3] {
            assert_eq!(
                swept_space_cell(&batch(&keys, width, Licence::Typed), 2),
                None
            );
        }
        for malformed in [
            vec![key(0, TypedClass::Space), key(2, TypedClass::Glyph)],
            vec![key(2, TypedClass::Space)],
            vec![keys[0], Event::Erase, keys[1]],
        ] {
            assert_eq!(
                swept_space_cell(&batch(&malformed, 2, Licence::Typed), malformed.len()),
                None
            );
        }
        let mut wrong_move = batch(&keys, 2, Licence::Typed);
        wrong_move[3].0 = Event::Move {
            from: (4, 39),
            to: (4, 42),
            licence: Licence::Typed,
            dir: Dir::Right,
        };
        assert_eq!(swept_space_cell(&wrong_move, 2), None);
        wrong_move.pop();
        assert_eq!(swept_space_cell(&wrong_move, 2), None);
    }
}

/// A same-row keyed hop under the flight floor (§11, §12.3): `1..8` cells on
/// one row under a `Nav` or `Synthetic` licence (an arrow, a word motion, a
/// short Home/End); the nav tick's trigger. It reads the same
/// [`timing::JUMP_MIN_CELLS`] as the meteor's classifier, so light and sound
/// agree on the floor. A `Typed` echo is the host's keystroke and a `Return`
/// changes row. A `Pty` hop is silent (§12.3); the meteor still draws its
/// mini-fan, since `Meteors::on_move` excludes only echo licences.
fn sub_floor_hop(from: (u16, u16), to: (u16, u16), licence: Licence) -> bool {
    if !licence.is_keyed_nav() || from.0 != to.0 {
        return false;
    }
    (1..JUMP_MIN_CELLS).contains(&to.1.abs_diff(from.1))
}

/// The frame's liveness (§18, T6): `0` exactly when nothing was appended, and
/// a fixed non-zero sentinel otherwise. Every consumer only asks `== 0` or
/// `!= 0` (`is_active`, the idle early-out, the status row), so a content
/// hash would be wasted work on the frame path.
fn liveness(under: &[GlowQuad], out: &[GlowQuad], halos: &[RainHalo]) -> u64 {
    // The low bit is set for the same reason the fold forced it: a live frame
    // can never collide with the idle sentinel.
    u64::from(!(under.is_empty() && out.is_empty() && halos.is_empty()))
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
            ribbon_flat: false,
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
    /// the contract level: with nothing live the engine writes nothing, its
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

    /// The gate: a disengaged engine is inert at every seam point, which is
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

    /// `Engine::default()` and `Engine::new()` are one constructor: a caller
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

    /// The tripwire's records since the last drain.
    fn hole_records(eng: &mut Engine) -> Vec<String> {
        eng.drain_hole_records().collect()
    }

    /// **THE HOLE TRIPWIRE RECORDS THE FIRST FRAME OF A HOLE**
    /// (`holes.rs`; written 2026-09-21, ported 2026-09-30): three typed
    /// cells, the middle one retired by identity and melted — a column no
    /// typing cell owns between two lit typing cells. One record, on the
    /// frame the hole opens, its header naming the frame's density
    /// (`slabs=3` here), with the cohort table, the row's cells, the
    /// witness and the events with their licences; the next frame adds
    /// nothing. NEGATIVE CONTROLS: a whole lit line is no hole, and neither
    /// is its LAST cell gone dark — the line ends there, nothing lit
    /// beyond; an engine nobody armed keeps nothing. The one-slab
    /// configuration is the test after this one.
    #[test]
    fn the_hole_tripwire_records_the_first_frame_of_a_hole_inside_a_lit_typed_line() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        // Retire the cell at `col` of a fresh three-cell line and melt it.
        let retire_at = |col: u16, armed: bool| {
            let mut eng = engaged();
            assert!(eng.holes.is_none(), "a new engine is unarmed");
            eng.trace_holes(armed);
            let t = three_typed_cells(&mut eng, t0);
            assert!(
                hole_records(&mut eng).is_empty(),
                "a whole lit line is no hole"
            );
            let born = eng
                .ribbon()
                .cells()
                .iter()
                .find(|c| c.row == 3 && c.col == col)
                .expect("the typed cell")
                .born;
            eng.ribbon.retire_cells(&[(3, col, born)], t + ms(50));
            let melted = t + ms(250);
            tick_at(&mut eng, melted);
            (eng, melted)
        };
        let (mut unarmed, melted) = retire_at(3, false);
        tick_at(&mut unarmed, melted + ms(16));
        assert!(unarmed.holes.is_none() && hole_records(&mut unarmed).is_empty());

        let (mut end, melted) = retire_at(4, true);
        tick_at(&mut end, melted + ms(16));
        assert!(
            hole_records(&mut end).is_empty(),
            "the line's last cell dark is no hole"
        );

        let (mut eng, melted) = retire_at(3, true);
        let log = hole_records(&mut eng).concat();
        assert_eq!(
            log.matches("== trail hole").count(),
            1,
            "one record for one new hole:\n{log}"
        );
        assert!(log.contains(" row=3 col=3 "), "{log}");
        assert!(
            log.contains(" slabs=3 dropped=0"),
            "the header names the density: {log}"
        );
        assert!(
            log.contains("cohorts: (id=")
                && log.contains("cells@row 3:")
                && log.contains(" (col=2 cohort=")
                && log.contains(" (col=4 cohort=")
                && log.contains("witness:")
                && log.contains("events (oldest first"),
            "{log}"
        );
        assert!(
            log.contains("licence: Typed"),
            "the events carry their licences: {log}"
        );
        tick_at(&mut eng, melted + ms(16));
        assert!(
            hole_records(&mut eng).is_empty(),
            "the same hole is not recorded again"
        );
        eng.trace_holes(true);
        assert!(eng.holes.is_some(), "arming an armed engine keeps it");
        eng.trace_holes(false);
        assert!(eng.holes.is_none(), "disarming drops it");
    }

    /// A geometry that forces the long-line clamp with three cells: a cell
    /// 1000 px tall prices the tall body at ~1460 device rows a slab, so
    /// four cells (three and the attach) overrun `RIBBON_QUAD_BUDGET` at
    /// two slabs and `Ribbon::slabs_for` clamps to ONE — the density a
    /// ~78-cell retina composer line plans at (`ribbon.rs`, the one-slab
    /// pin), reached here by height instead of length.
    fn one_slab_geom() -> Geom {
        Geom {
            cw: 9,
            ch: 1000,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 40_000,
            head: 0,
        }
    }

    /// **THE HOLE TRIPWIRE FIRES AT ONE SLAB PER CELL** (2026-09-21, the
    /// review: blind at one slab). At `slabs_per_cell() == 1` a run's only
    /// vertices sit on the cell edges — no interior sample — and a census
    /// that dropped edge-aligned samples read every column dark and could
    /// never fire on exactly the long composer lines it exists for. A
    /// sample belongs to the column whose `[x0, x1)` holds it, and at one
    /// slab the OWNER census decides: a column no non-leaving typing cell
    /// owns, between two owned columns, is a hole (the census rule itself is
    /// [`at_one_slab_the_owner_census_alone_decides_a_hole`]'s). The same
    /// gesture as the three-slab test in a geometry that clamps to one slab:
    /// one record, its header saying `slabs=1`.
    #[test]
    fn the_hole_tripwire_records_a_hole_at_one_slab_per_cell() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let g = one_slab_geom();
        let tick_in = |eng: &mut Engine, t: Instant| {
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t, g, &config(), &mut fr);
        };
        let mut eng = engaged();
        eng.trace_holes(true);
        blank_row(&mut eng, 2);
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_in(&mut eng, t0);
        let mut t = t0;
        for col in 2..5u16 {
            t += ms(100);
            eng.on_event(typed(1), t);
            tick_in(&mut eng, t);
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
            tick_in(&mut eng, echo);
            t = echo;
        }
        assert_eq!(
            eng.ribbon().slabs_per_cell(),
            1,
            "the geometry forces the long-line clamp"
        );
        for col in 2..5u16 {
            assert!(eng.field_at(3, col).is_some(), "cell (3, {col}) is lit");
        }
        assert!(
            hole_records(&mut eng).is_empty(),
            "a whole lit line is no hole at one slab"
        );
        let born = eng
            .ribbon()
            .cells()
            .iter()
            .find(|c| c.row == 3 && c.col == 3)
            .expect("the middle cell")
            .born;
        let at = t + ms(50);
        eng.ribbon.retire_cells(&[(3, 3, born)], at);
        let melted = at + ms(200);
        tick_in(&mut eng, melted);
        assert_eq!(eng.ribbon().slabs_per_cell(), 1);
        let log = hole_records(&mut eng).concat();
        assert_eq!(
            log.matches("== trail hole").count(),
            1,
            "one record for the hole at one slab per cell:\n{log}"
        );
        assert!(log.contains(" row=3 col=3 "), "{log}");
        assert!(
            log.contains(" slabs=1"),
            "the header names the density: {log}"
        );
        tick_in(&mut eng, melted + ms(16));
        assert!(
            hole_records(&mut eng).is_empty(),
            "the same hole is not recorded again"
        );
    }

    /// **AT ONE SLAB THE OWNER CENSUS ALONE DECIDES A HOLE** (`holes.rs`,
    /// `row_holes`): three owned columns whose middle one holds no light are
    /// a whole line at one slab per cell, where a column's one sample sits on
    /// its edge, and a hole above one slab, where a column's samples say what
    /// it shows. NEGATIVE CONTROLS, at both densities: a column no typing cell
    /// owns between two owned ones is a hole, its light or none; a lit owned
    /// line is none; the line's last column unowned is none (nothing owned
    /// beyond it).
    #[test]
    fn at_one_slab_the_owner_census_alone_decides_a_hole() {
        fn census(cov: &[u8], slabs: usize, owned: &[usize]) -> Vec<usize> {
            super::holes::row_holes(cov, slabs, |col| owned.contains(&col)).collect()
        }
        let dim_middle: [u8; 6] = [0, 0, 200, 0, 200, 0];
        let lit_line: [u8; 6] = [0, 0, 200, 200, 200, 0];
        assert_eq!(
            census(&dim_middle, 1, &[2, 3, 4]),
            Vec::<usize>::new(),
            "one slab: the owner census decides"
        );
        assert_eq!(
            census(&dim_middle, 3, &[2, 3, 4]),
            [3],
            "three slabs: the light census decides too"
        );
        for slabs in [1, 3] {
            assert_eq!(census(&lit_line, slabs, &[2, 4]), [3], "{slabs} slabs");
            assert_eq!(census(&dim_middle, slabs, &[2, 4]), [3], "{slabs} slabs");
            assert!(
                census(&lit_line, slabs, &[2, 3, 4]).is_empty(),
                "{slabs} slabs"
            );
            assert!(
                census(&lit_line, slabs, &[2, 3]).is_empty(),
                "{slabs} slabs"
            );
        }
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
    /// control restores only that old ledger after the same real transform.
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

                // The program nudged 5 -> 6 without a press. A new key at 6
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

    /// A key whose echo the seam refused is relit by the next licensed echo
    /// ([`Engine::echo_bridge`]). A key echoed about 300 ms after its press is
    /// refused (`no-fresh-hint`); the next licensed echo carries `from` one
    /// past the engine's mirror, the press on the ledger pays for that cell,
    /// and it joins its cohort (the walk continues a sixteenth per cell). The
    /// control is the same hop with no press behind it (program output), which
    /// lays nothing (T1).
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

    /// Three keys typed into a stall are relit together: keys echoed with a
    /// stalled key in one refused move leave a three-cell hole; the next
    /// licensed echo's `from` sits three past the mirror and three presses pay
    /// for three cells.
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

    /// A cold engine's first coalesced echo lays the keys it held. A composer
    /// under load coalesces the line's first two keys into one repaint: the
    /// keys are typed at a caret the engine never learned and held with no
    /// cell ([`Engine::replay_events`]), and the echo is one licensed two-cell
    /// move the host does not sweep. The move's advance is paid from `from` by
    /// the presses in flight, so the line's first cells are laid
    /// (`tests/phrase_pause_gaps.rs`). The controls keep T1 and one press per
    /// cell: no press lays nothing, one press cannot pay two cells.
    #[test]
    fn a_cold_engine_s_first_coalesced_echo_lays_the_keys_it_held() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let drive = |presses: u16| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 4);
            let mut t = t0;
            for _ in 0..presses {
                eng.on_event(typed(1), t);
                tick_at(&mut eng, t);
                t += ms(83);
            }
            // The composer's one repaint of the keys: licensed, unswept,
            // two cells from a caret the engine never learned.
            let echo = t + ms(30);
            eng.on_event(mv((5, 2), (5, 4), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            eng
        };
        let cold = drive(2);
        assert!(
            cold.field_at(5, 2).is_some() && cold.field_at(5, 3).is_some(),
            "both held keys' cells are laid by the line's first echo: {:?}",
            cold.ribbon()
                .cells()
                .iter()
                .map(|c| (c.row, c.col, c.typing))
                .collect::<Vec<_>>()
        );
        assert!(
            cold.field_at(5, 4).is_none(),
            "the landing is the caret's own cell, not a glyph's"
        );
        assert_eq!(cold.status().bridged, 2, "two cells laid from the ledger");
        assert_eq!(cold.echo.total(), 0, "both presses spent");
        assert_eq!(cold.caret_mirror(), (5, 4), "…and the mirror is seeded");
        // T1: the same echo with no press behind it lays nothing.
        let control = drive(0);
        assert!(
            control.field_at(5, 2).is_none() && control.field_at(5, 3).is_none(),
            "a cold hop no press explains is not typing"
        );
        assert_eq!(control.status().bridged, 0);
        // One press cannot pay for two cells.
        let short = drive(1);
        assert!(
            short.field_at(5, 2).is_none() && short.field_at(5, 3).is_none(),
            "one press, two cells: refused"
        );
        assert_eq!(short.status().bridged, 0);
    }

    /// A cold backward typed echo keeps the press. A fresh engine with one
    /// press banked sees a same-row backward `Typed` move, (5,4) → (5,3): a
    /// retreat is not an echo, so the press keeps waiting (`total() == 1`).
    /// The unswept forward move that follows, (5,3) → (5,4), is laid from the
    /// ledger (`bridged == 1`). Control: after the same backward move, a
    /// two-cell forward hop one press cannot pay for is refused and clears the
    /// ledger.
    #[test]
    fn a_cold_backward_typed_echo_keeps_the_press_for_the_forward_move_after_it() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cold = || -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 4);
            eng.on_event(typed(1), t0);
            tick_at(&mut eng, t0);
            let back = t0 + ms(30);
            eng.on_event(mv((5, 4), (5, 3), Licence::Typed), back);
            tick_at(&mut eng, back);
            eng
        };
        let mut eng = cold();
        assert_eq!(
            eng.echo.total(),
            1,
            "a backward typed move at cold keeps the press waiting"
        );
        assert_eq!(eng.status().bridged, 0);
        let fwd = t0 + ms(60);
        eng.on_event(mv((5, 3), (5, 4), Licence::Typed), fwd);
        tick_at(&mut eng, fwd);
        assert!(
            eng.field_at(5, 3).is_some(),
            "the unswept forward move is laid from the ledger: {:?}",
            eng.ribbon()
                .cells()
                .iter()
                .map(|c| (c.row, c.col, c.typing))
                .collect::<Vec<_>>()
        );
        assert!(
            eng.field_at(5, 4).is_none(),
            "the landing is the caret's own cell, not a glyph's"
        );
        assert_eq!(eng.status().bridged, 1, "one cell laid from the ledger");
        assert_eq!(eng.echo.total(), 0, "the press is spent");
        // The control: a hop the one press cannot pay for.
        let mut eng = cold();
        eng.on_event(mv((5, 3), (5, 5), Licence::Typed), fwd);
        tick_at(&mut eng, fwd);
        assert!(
            eng.field_at(5, 3).is_none() && eng.field_at(5, 4).is_none(),
            "one press, two cells: refused"
        );
        assert_eq!(eng.status().bridged, 0);
        assert_eq!(eng.echo.total(), 0, "…and the ledger is cleared");
    }

    /// A licensed echo the host did not sweep is laid from its presses: a
    /// two-cell echo licensed but unswept (a non-coalesced re-anchor, or a
    /// batch its press credits could not pay for) gets both cells from the
    /// engine's ledger.
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

    /// A stale press cannot fund a later stray hop (the anti-stray law). A
    /// refused press survives on the ledger, then the caret hops five cells
    /// with no press behind it (vim's `w`, a click): the hop buys nothing and
    /// forgets the press. A later one-cell program nudge followed by a key's
    /// echo therefore stays dark.
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

    /// A press older than the echo patience buys no cell: a swallowed key is
    /// not a late one.
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

    /// A navigation licence, a row change and a reset forget the presses:
    /// afterwards a one-cell unlicensed nudge followed by a licensed key is
    /// not paid for by the press that was waiting.
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

    /// Presses younger than the licensing key never pay for a cell to its
    /// left. Program output nudges the caret one cell (no press), then three
    /// keys come fast. The first key's echo lands one past the mirror with two
    /// presses in flight behind it; their glyphs are to the right, so counting
    /// them would light the program's cell. Only presses older than the host
    /// sweep's clock may pay, and there are none.
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

    /// A swallowed press never rolls forward as a phantom credit: the next
    /// ordinary echo finds a press older than its own key with no hole to
    /// explain, and forgets it.
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

    /// The insert's rewrite ([`Event::Rewrite`]): the program pulled the caret
    /// back inside a delivered insert's span. No impulse (no Wince), no spine
    /// drain, no star, no cue, the echo ledger kept, the caret mirror moved
    /// (so the next typed echo bridges nothing), and the ribbon retracts the
    /// suffix.
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

    /// A host sweep under a typed move beside banked presses the hop does not
    /// explain: the whole span (`5..67`) is laid as one sweep behind an inert
    /// typed move, the older presses are forfeited by the ledger's mismatch
    /// rule, and nothing is bridged. (A delivered insert uses
    /// `Licence::Insert`, which pays such a hole:
    /// `an_inserts_sweep_pays_the_hole_a_late_key_left_before_it`.)
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

    /// A delivered insert's sweep pays the hole a late key left before it: a
    /// key pressed just before the drop, whose echo was not laid, leaves the
    /// mirror one cell behind the insert's origin. Under `Licence::Insert` the
    /// ledger pays that cell from the key's press and charges the insert's span
    /// nothing; under `Typed` the mismatch would forget the ledger and the cell
    /// would stay dark.
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

    /// A sweep on the origin row before a fold move (a stalled key whose echo
    /// wraps the row): the seam sweeps the origin row's last column, then
    /// hands the fold's `Move`. The sweep is on a different row, so the bridge
    /// treats the row change as usual (ledger cleared, nothing bridged), the
    /// ribbon lays the swept cell, and the mirror lands on the new row.
    #[test]
    fn a_sweep_on_the_origin_row_before_a_fold_move_lays_the_last_column_and_clears_the_ledger() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // The stalled key: on the ledger, its echo not yet seen.
        let key = t + ms(100);
        eng.on_event(typed(1), key);
        tick_at(&mut eng, key);
        assert_eq!(eng.echo.total(), 1, "PRECONDITION: one press on the ledger");
        let echo = key + ms(400);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 39,
                col1: 40,
            },
            echo - ms(250),
        );
        eng.on_event(mv((3, 39), (4, 0), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(3, 39).is_some(),
            "the origin row's last column is lit by the seam's sweep"
        );
        assert_eq!(eng.echo.total(), 0, "a row change clears the ledger");
        assert_eq!(eng.status().bridged, 0, "…and bridges nothing");
        assert_eq!(eng.caret, (4, 0), "the mirror lands on the new row");
    }

    /// Tier-1 conformance for the echo ledger: the real `Engine`, driven
    /// through the shapes `EchoLedgerBridge` names (a refused echo relit on the
    /// press's clock, the program-nudge-then-burst break, the swallowed-press
    /// phantom, the unswept batch, fresh and stale retreats, the navigation
    /// hop, vim's `w`, patience whole and per press, and the three in-place
    /// clears), with every observed step validated as a model transition and
    /// a forged step at each law the model must refuse.
    ///
    /// Every projected term is read off the engine: the ledger's buckets
    /// through its own partition by the licensing key's clock, its four
    /// tallies, its mirror against the host's caret (the hole),
    /// `Status::bridged`, and the clock the engine partitioned by
    /// (`last_host_sweep_at`), which must be the key's. `laid_hole` is the
    /// bridged delta less the cells an unswept move laid at and past `from`.
    /// `stale_gone` is `expired + stale` by definition; its law
    /// (`ExpiredPressesBuyNothing`) is enforced per transition (the forged
    /// stale-press-pays control).
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
        // A licensed move at `at`, under the host's sweep of `from..to` on the
        // key's clock when `key` is `Some`, unswept otherwise: the partition
        // clock must be the key's, the bridged delta is the move's, and the
        // part left of `from` is `laid_hole`.
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

        // ---- the screenshot: press, echo, a refused echo, and the relight ----
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

        // The next key: the stalled press is now older than the licensing key.
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

        // ---- the adversary's break: a program nudge, then a fast burst ----
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

        // ---- the phantom: a swallowed press, an ordinary key, a nudge ----
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

        // ---- the retreats, the navigation hop and the in-place clears ----
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
        // expires before it looks at the shape, so the retreat drops it.
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

        // ---- the in-place clears: erase, kill, focus loss ----
        // Each expires before it clears (a stale press is dropped as expired,
        // a fresh one forfeited) and moves nothing.
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
        // the hole is untouched — nothing moved the mirror.
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

        // ---- the patience: a press the program swallowed goes stale ----
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

        // ---- the partial expiry: the older press stale, the key fresh ----
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

        // ---- the stale press that must not pay ----
        // The honest successor drops the stale press and spends the key; the
        // forged one spends the stale press and keeps the key waiting.
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

        // ---- the unswept batch: two presses, a two-cell licensed echo ----
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

        // ---- the declined relocation to another row ----
        // A press, a refused hop, then the host observes the caret on another
        // row with no licence (`Engine::observe_caret`). The mirror follows at
        // the end of the tick and the presses are forgotten (the model's
        // `LedgerForgotten`), so the hole reads 0 and no credit survives.
        let v0 = Instant::now();
        let mut eng = seeded(v0, 5);
        let mut last = Last::default();
        let o0 = project(&eng, v0, v0, 5, last);
        let v1 = v0 + ms(100);
        press(&mut eng, v1);
        let o1 = project(&eng, v1, v1, 5, last);
        bind(&o0, &o1, "KeyPressed", "Engine press before a relocation");
        let o2 = project(&eng, v1 + ms(50), v1, 6, last);
        bind(
            &o1,
            &o2,
            "UnlicensedAdvance",
            "Engine hop before a relocation",
        );
        let v2 = v1 + ms(100);
        let before = eng.status().bridged;
        eng.observe_caret((4, 2));
        tick_at(&mut eng, v2);
        assert_eq!(
            eng.caret_mirror(),
            (4, 2),
            "the mirror followed the observed row"
        );
        last = Last {
            laid_hole: 0,
            bridged_delta: i64::from(eng.status().bridged - before),
            last_older: o2["older"],
            ..last
        };
        let o3 = project(&eng, v2, v1, 2, last);
        assert_eq!(
            (o3["older"] + o3["younger"], o3["forfeited"], o3["hole"]),
            (0, 1, 0),
            "the press is forfeited and the hole is measured on the new row"
        );
        bind(&o2, &o3, "LedgerForgotten", "Engine declined relocation");
        // A stale press is tallied as expired by the same step, never as
        // forfeited: the relocation expires before it clears.
        let v3 = v2 + ms(100);
        press(&mut eng, v3);
        let o4 = project(&eng, v3, v3, 2, last);
        bind(
            &o3,
            &o4,
            "KeyPressed",
            "Engine press that goes stale before a relocation",
        );
        let v4 = v3 + patience + ms(100);
        let o5 = project(&eng, v4, v3, 2, last);
        bind(
            &o4,
            &o5,
            "TimePasses",
            "Engine patience before a relocation",
        );
        eng.observe_caret((6, 0));
        tick_at(&mut eng, v4);
        last = Last {
            laid_hole: 0,
            bridged_delta: 0,
            last_older: o5["older"],
            ..last
        };
        let o6 = project(&eng, v4, v3, 0, last);
        assert_eq!((o6["expired"], o6["forfeited"], o6["stale"]), (1, 1, 0));
        bind(
            &o5,
            &o6,
            "LedgerForgotten",
            "Engine relocation over a stale press",
        );
    }

    /// The mirror follows a declined row change, after the tick's ingest. A
    /// composer re-lays its input box a row up with no key behind it; the
    /// seam declines and reports the caret (`Engine::observe_caret`). A key
    /// buffered before the observation is held at the old caret; the next
    /// key's echo lays at the caret's true cell on the new row, and nothing
    /// at the box's inset marker left of it.
    #[test]
    fn a_declined_row_change_moves_the_mirror_after_the_ingest_and_the_next_key_lays_on_the_new_row()
     {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        assert_eq!(eng.caret_mirror(), (3, 5));
        // A key pressed, then — inside the same tick — the box relocates the
        // caret to (5, 2) without a licence: the key is held where it was
        // typed, and lays nothing on either row.
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        eng.observe_caret((5, 2));
        assert_eq!(
            eng.caret_mirror(),
            (3, 5),
            "the mirror moves at the END of the tick"
        );
        tick_at(&mut eng, t1);
        assert!(
            eng.field_at(3, 4).is_none(),
            "the buffered key is held at the caret it was typed at, not laid"
        );
        assert!(
            eng.field_at(5, 1).is_none(),
            "…and nothing on the new row yet"
        );
        assert_eq!(
            eng.caret_mirror(),
            (5, 2),
            "from the next tick the mirror is the observed caret"
        );
        assert_eq!(eng.echo.total(), 0, "the press was forgotten with the row");
        // The next key: held until its echo, which lays at the true caret
        // on row 5 — never at the marker's cell `(5, 1)`, never at the
        // abandoned row.
        let t2 = t1 + Duration::from_millis(100);
        eng.on_event(typed(1), t2);
        tick_at(&mut eng, t2);
        assert!(
            eng.field_at(5, 1).is_none(),
            "a key whose echo has not landed lays nothing — not the marker's cell"
        );
        assert!(
            eng.field_at(3, 5).is_none() && eng.field_at(3, 6).is_none(),
            "…and nothing new on the row the caret left"
        );
        // Its echo, swept by the host on the key's clock: no hole, nothing
        // bridged — the forgotten press bought nothing on the new row.
        let e2 = t2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 5,
                col0: 2,
                col1: 3,
            },
            t2,
        );
        eng.on_event(mv((5, 2), (5, 3), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(eng.status().bridged, 0, "no phantom credit lit a cell");
        assert!(
            eng.field_at(5, 2).is_some(),
            "the host's sweep laid the glyph cell"
        );
        assert!(
            eng.field_at(5, 1).is_none(),
            "the key after the relocation lays on the new row, at its own cell only"
        );
    }

    /// A same-row unlicensed hop is the echo ledger's to explain, so an
    /// observation on the mirror's own row moves nothing: the next licensed
    /// echo still measures the hole from the mirror and pays it from the older
    /// press.
    #[test]
    fn a_same_row_observation_leaves_the_mirror_for_the_ledger_to_measure() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        tick_at(&mut eng, t1);
        // The key's echo lands late: the seam refuses it and only observes
        // the caret one cell on.
        eng.observe_caret((3, 6));
        tick_at(&mut eng, t1 + Duration::from_millis(300));
        assert_eq!(
            eng.caret_mirror(),
            (3, 5),
            "a same-row observation is not a move"
        );
        assert_eq!(eng.echo.total(), 1, "…and the press keeps waiting");
        // The next key: its licensed echo pays the hole from the older press.
        let t2 = t1 + Duration::from_millis(400);
        eng.on_event(typed(1), t2);
        tick_at(&mut eng, t2);
        let e2 = t2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            t2,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(
            eng.status().bridged,
            1,
            "the late echo's cell was relit by the ledger"
        );
        assert!(eng.field_at(3, 5).is_some(), "…at the hole");
    }

    /// A fresh engine ignores an observed caret until a licensed move has
    /// seeded the mirror: a key struck before the first licensed move lays
    /// nothing (`Engine::reset`'s law), and a mirror seeded from an
    /// observation at column 0 would fold that key's neighbour cell onto
    /// the row above.
    #[test]
    fn a_fresh_engine_ignores_an_observed_caret_until_a_licensed_move_seeds_the_mirror() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.observe_caret((5, 0));
        tick_at(&mut eng, t0);
        assert_eq!(eng.caret_mirror(), (0, 0), "an observation seeds nothing");
        let t1 = t0 + Duration::from_millis(100);
        eng.on_event(typed(1), t1);
        tick_at(&mut eng, t1);
        assert_eq!(
            eng.status().cells,
            0,
            "a key before the first licensed move lays nothing — not on row 4's last cell either"
        );
    }

    /// The witness's engine half: retirement counts on `Status::retired`,
    /// survives a reset, and a ledger payout beside a retired cell lays a
    /// fresh cell without reviving the retired one.
    #[test]
    fn a_witness_retirement_is_counted_survives_a_reset_and_is_never_undone_by_a_payout() {
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for col in 2..5u16 {
            t = type_key_at(&mut eng, t + Duration::from_millis(100), col);
        }
        // The three swept glyph cells 2..=4 — and nothing at column 1: a
        // key is held until its echo, never laid at caret−1
        // on the frame it was typed.
        assert_eq!(eng.status().cells, 3, "three cells: `abc`");
        assert!(
            eng.field_at(3, 1).is_none(),
            "no neighbour cell before the run"
        );
        // The witness sees `abc`, then the `c` overwritten (the run's last
        // cell, a suffix): a change strictly inside a standing run is not
        // evidence (`Witness::shape_verdicts`).
        let abc: Vec<char> = "  abc".chars().collect();
        let axc: Vec<char> = "  abX".chars().collect();
        let seen_abc = [RowSample { row: 3, cols: &abc }];
        let seen_axc = [RowSample { row: 3, cols: &axc }];
        assert_eq!(
            eng.witness_rows(&seen_abc, t),
            0,
            "the first glyph seen arms"
        );
        assert_eq!(
            eng.witness_rows(&seen_abc, t),
            0,
            "the same glyph retires nothing"
        );
        let fired = t + Duration::from_millis(50);
        assert_eq!(
            eng.witness_rows(&seen_axc, fired),
            1,
            "c -> X retires the `c` cell"
        );
        assert_eq!(eng.status().retired, 1);
        let leaving = |eng: &Engine| -> Vec<u16> {
            let mut v: Vec<u16> = eng
                .ribbon()
                .cells()
                .iter()
                .filter(|c| c.leaving())
                .map(|c| c.col)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(
            leaving(&eng),
            vec![4],
            "`c` is on the melt; `a` and `b` stand"
        );
        // A press whose echo the seam refused, then the licensed echo of
        // the next key: the ledger pays the hole at (3, 5) — a fresh cell
        // beside the melting ones, which stay on their clock.
        let p1 = fired + Duration::from_millis(10);
        eng.on_event(typed(1), p1);
        tick_at(&mut eng, p1);
        let p2 = p1 + Duration::from_millis(20);
        eng.on_event(typed(1), p2);
        tick_at(&mut eng, p2);
        let e2 = p2 + Duration::from_millis(8);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 6,
                col1: 7,
            },
            p2,
        );
        eng.on_event(mv((3, 6), (3, 7), Licence::Typed), e2);
        tick_at(&mut eng, e2);
        assert_eq!(eng.status().bridged, 1, "the hole was paid");
        assert!(eng.field_at(3, 5).is_some() && eng.field_at(3, 6).is_some());
        assert_eq!(
            leaving(&eng),
            vec![4],
            "the payout did not revive the retired cell"
        );
        // Past the melt the retired cells are gone; the rest stand.
        tick_at(&mut eng, e2 + Duration::from_millis(200));
        assert!(
            leaving(&eng).is_empty(),
            "the melt ended: nothing is leaving"
        );
        assert!(
            eng.field_at(3, 4).is_none(),
            "…and the retired cell is out of the pool"
        );
        assert!(
            eng.field_at(3, 2).is_some()
                && eng.field_at(3, 3).is_some()
                && eng.field_at(3, 5).is_some(),
            "…while `a`, `b` and the paid cell stand"
        );
        // The tally survives a reset; the records do not.
        eng.reset();
        assert_eq!(
            eng.status().retired,
            1,
            "the window's tally survives a reset"
        );
        assert!(eng.witness.is_empty());
    }

    /// Three short, disjoint typed runs need nine distinct content samples:
    /// each run's own row and both possible one-row landing rows. The third
    /// run is the one under the hand. If the fixed sample budget drops its
    /// lower neighbor, a composer rewrite that moves its text down one row
    /// leaves the ribbon on the old row instead of carrying it with the text.
    #[test]
    fn three_disjoint_cohorts_keep_the_hands_lower_follow_row_in_the_witness() {
        let ms = Duration::from_millis;
        let (mut eng, t) = typed_runs(&[3, 7, 11]);
        let text: Vec<char> = "  ab".chars().collect();
        let blank: Vec<char> = "    ".chars().collect();
        let mut wanted = [0u16; witness::WITNESS_ROWS];
        let n = eng.ribbon_rows_for(None, &mut wanted);
        assert!(
            wanted[..n].contains(&12),
            "the hand's run on row 11 needs row 12 to follow its text; requested {:?}",
            &wanted[..n]
        );
        // Arm the three runs on the rows the engine asked for, as the host
        // samples them: row 12 is seen blank, so text found there later
        // arrived.
        let original = wanted[..n]
            .iter()
            .map(|&row| RowSample {
                row,
                cols: if row == 3 || row == 7 || row == 11 {
                    &text
                } else {
                    &blank
                },
            })
            .collect::<Vec<_>>();
        assert_eq!(eng.witness_rows(&original, t), 0, "arm the three runs");
        let moved = wanted[..n]
            .iter()
            .map(|&row| RowSample {
                row,
                cols: if row == 12 || row == 3 || row == 7 {
                    &text
                } else {
                    &blank
                },
            })
            .collect::<Vec<_>>();
        assert_eq!(eng.follow_rows(&moved, &wanted[..n], t + ms(5)), 2);
        tick_at(&mut eng, t + ms(5));
        assert!(
            eng.field_at(12, 2).is_some() && eng.field_at(12, 3).is_some(),
            "the lit run followed its text onto row 12"
        );
    }

    /// A held one-cell park may borrow a text follow only when the moved
    /// run belonged to the caret's short retreat. A separate run far to the
    /// left can move off the same source row without proving this park.
    #[test]
    fn an_unrelated_follow_on_the_parked_row_does_not_prove_the_park() {
        let model = aterm_spec::derive::rainbow_short_wrap_park_model();
        let held = model.successors("Hold", &model.init_state())[0].clone();
        for (origin, landing, local_run, should_prove) in
            [(4, 3, false, true), (32, 31, true, false)]
        {
            let (mut eng, mut t) = typed_runs(&[3]);
            if local_run {
                // Another cohort at the parked caret: the far-left run
                // still follows, but it is not the text this hand just left.
                eng.observe_caret((3, 31));
                tick_at(&mut eng, t);
                let key = t + Duration::from_millis(10);
                eng.on_event(typed(1), key);
                tick_at(&mut eng, key);
                let echo = key + Duration::from_millis(1);
                eng.on_event(
                    Event::Sweep {
                        row: 3,
                        col0: 31,
                        col1: 32,
                    },
                    key,
                );
                eng.on_event(mv((3, 31), (3, 32), Licence::Typed), echo);
                tick_at(&mut eng, echo);
                t = echo;
                let far = eng
                    .ribbon()
                    .cells()
                    .iter()
                    .find(|cell| cell.row == 3 && cell.col == 2)
                    .expect("the moving run stands at col 2")
                    .cohort;
                let local = eng
                    .ribbon()
                    .cells()
                    .iter()
                    .find(|cell| cell.row == 3 && cell.col == 31)
                    .expect("the parked caret has its own typed run")
                    .cohort;
                assert_ne!(far, local, "premise: only the distant run moves");
            } else {
                eng.observe_caret((3, origin));
                tick_at(&mut eng, t);
            }
            let mut moving_text = vec![' '; 36];
            moving_text[2] = 'a';
            moving_text[3] = 'b';
            let mut text = moving_text.clone();
            if local_run {
                text[31] = 'z';
            }
            let blank = vec![' '; 36];
            let mut remaining = blank.clone();
            if local_run {
                remaining[31] = 'z';
            }
            let before = [
                RowSample {
                    row: 3,
                    cols: &text,
                },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ];
            assert_eq!(eng.witness_rows(&before, t), 0);
            let after = [
                RowSample {
                    row: 3,
                    cols: &remaining,
                },
                RowSample {
                    row: 4,
                    cols: &moving_text,
                },
            ];
            let (moved, proved) = eng.follow_rows_with_park(
                &after,
                &[3, 4],
                t + Duration::from_millis(5),
                Some(((3, origin), (3, landing))),
            );
            assert_eq!(moved, 2, "the unrelated run still follows its text");
            let action = if should_prove {
                "FollowExact"
            } else {
                "FollowOther"
            };
            let projected = model.successors(action, &held)[0].clone();
            assert_eq!(projected.get("content_moved"), Some(&1));
            assert_eq!(
                proved,
                projected.get("exact") == Some(&1),
                "the parked origin {origin} must borrow only its own run"
            );
        }
    }

    /// An engaged engine with a typed two-cell ribbon run on each of `rows`
    /// (the caret moved to each row by observation, then two keys echoed at
    /// columns 2 and 3), and the instant of the last echo.
    fn typed_runs(rows: &[u16]) -> (Engine, Instant) {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        eng.on_event(mv((rows[0], 0), (rows[0], 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for &row in rows {
            if row != rows[0] {
                eng.observe_caret((row, 2));
                tick_at(&mut eng, t);
            }
            for col in 2..4u16 {
                t += ms(12);
                eng.on_event(typed(1), t);
                tick_at(&mut eng, t);
                let echo = t + ms(1);
                eng.on_event(
                    Event::Sweep {
                        row,
                        col0: col,
                        col1: col + 1,
                    },
                    t,
                );
                eng.on_event(mv((row, col), (row, col + 1), Licence::Typed), echo);
                tick_at(&mut eng, echo);
                t = echo;
            }
            assert!(
                eng.field_at(row, 2).is_some(),
                "fixture: a typed ribbon run on row {row}"
            );
        }
        (eng, t)
    }

    /// A waiting key's arming rows have their own slots
    /// ([`Engine::ribbon_rows_for`]). Four runs on rows 3, 7, 11 and 15 want
    /// twelve samples against the bands' [`witness::WITNESS_ROWS`]; a key
    /// waiting to lay on row 19 adds rows 19, 18 and 20 in the
    /// [`witness::ARMING_ROWS`] slots after the bands' eight. A caller with
    /// only eight slots gets the bands' rows and no arming row. No row two
    /// away from the arming row is named, and an arming row on the grid's
    /// last row names nothing past the grid.
    #[test]
    fn a_waiting_key_s_arming_rows_have_slots_of_their_own() {
        let (eng, _) = typed_runs(&[3, 7, 11, 15]);
        let mut bands = [0u16; witness::WITNESS_ROWS];
        let b = eng.ribbon_rows_for(None, &mut bands);
        assert_eq!(
            b,
            witness::WITNESS_ROWS,
            "fixture: the bands fill their budget"
        );
        assert_eq!(
            bands[..3],
            [15, 14, 16],
            "the hand's run and its landing rows lead: {bands:?}"
        );
        let mut out = [0u16; witness::WITNESS_ROWS + witness::ARMING_ROWS];
        let n = eng.ribbon_rows_for(Some(19), &mut out);
        assert_eq!(n, witness::WITNESS_ROWS + witness::ARMING_ROWS);
        assert_eq!(out[..b], bands, "the bands' rows are untouched: {out:?}");
        assert_eq!(
            out[b..n],
            [19, 18, 20],
            "the arming row and its neighbours in their own slots: {out:?}"
        );
        assert!(
            !out.contains(&17) && !out.contains(&21),
            "no row two away from the arming row: {out:?}"
        );
        let mut short = [0u16; witness::WITNESS_ROWS];
        assert_eq!(
            eng.ribbon_rows_for(Some(19), &mut short),
            witness::WITNESS_ROWS
        );
        assert_eq!(short, bands, "a short caller keeps every band's row");
        // The grid's last row (`geom()` is 40 rows): nothing past it.
        let last = u16::try_from(geom().rows - 1).expect("a small grid");
        let n = eng.ribbon_rows_for(Some(last), &mut out);
        assert_eq!(
            out[b..n],
            [last, last - 1],
            "the rows past the grid are not neighbours: {:?}",
            &out[..n]
        );
    }

    /// No row two away is named ([`Engine::ribbon_rows_for`]): one band on
    /// row 10 names its row and its neighbours, and a key waiting on row 10
    /// adds nothing two away (`tests/copies_are_not_moves.rs`).
    #[test]
    fn no_row_two_away_is_named() {
        let (eng, _) = typed_runs(&[10]);
        let mut out = [0u16; witness::WITNESS_ROWS + witness::ARMING_ROWS];
        let n = eng.ribbon_rows_for(None, &mut out);
        assert_eq!(&out[..n], &[10, 9, 11]);
        let n = eng.ribbon_rows_for(Some(10), &mut out);
        assert_eq!(&out[..n], &[10, 9, 11]);
    }

    /// Withheld means asked for and not delivered ([`Engine::follow_rows`]).
    /// Delivering every row of the asked list withholds nothing; one missing
    /// row of it is withheld. A row outside the focused pane or past the grid
    /// is not withheld, delivered or not.
    #[test]
    fn withheld_is_what_the_host_was_asked_for_and_did_not_deliver() {
        let (mut eng, t) = typed_runs(&[3, 7, 11, 15]);
        let blank: Vec<char> = "    ".chars().collect();
        let deliver = |rows: &[u16]| {
            rows.iter()
                .map(|&row| RowSample { row, cols: &blank })
                .collect::<Vec<_>>()
        };
        let withheld = |eng: &Engine| eng.withheld[..eng.withheld_n].to_vec();
        let mut asked = [0u16; witness::WITNESS_ROWS + witness::ARMING_ROWS];
        let n = eng.ribbon_rows_for(Some(19), &mut asked);
        let asked = &asked[..n];
        assert_eq!(
            asked[n - 3..],
            [19, 18, 20],
            "fixture: the arming rows are asked for"
        );
        eng.follow_rows(&deliver(asked), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "every row asked for came"
        );
        let far = asked[n - 1];
        let without_far: Vec<u16> = asked.iter().copied().filter(|&r| r != far).collect();
        eng.follow_rows(&deliver(&without_far), asked, t);
        assert_eq!(withheld(&eng), vec![far], "one row asked for did not come");
        // The focused pane is rows 0..=16: the arming rows lie outside it.
        eng.set_pane_rows(Some((0, 17)));
        let in_pane: Vec<u16> = asked.iter().copied().filter(|&r| r <= 16).collect();
        eng.follow_rows(&deliver(&in_pane), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "rows outside the pane are not withheld"
        );
        eng.follow_rows(&deliver(&without_far), asked, t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "row 20 is outside the pane"
        );
        let inside = asked[0];
        let without_inside: Vec<u16> = in_pane.iter().copied().filter(|&r| r != inside).collect();
        eng.follow_rows(&deliver(&without_inside), asked, t);
        assert_eq!(
            withheld(&eng),
            vec![inside],
            "…and a row inside it still is"
        );
        eng.set_pane_rows(None);
        let past = u16::try_from(geom().rows).expect("a small grid");
        eng.follow_rows(&deliver(&[15]), &[15, past, past + 1], t);
        assert_eq!(
            withheld(&eng),
            Vec::<u16>::new(),
            "rows past the grid are not withheld"
        );
    }

    /// §12, §8.1 no. 1 and §6.7 at the seam: a licensed nav jump mints exactly
    /// one `Meteor` cue on the spawn frame, at the landing column, carrying the
    /// direction the pixels flew and a `cells` whose `flight_ms` is the
    /// `t_flight` the kitty's `land_at` (and the pin and fan) read.
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

    /// D9 / §12.3: a Return-licensed move flies the meteor but sounds the
    /// cadence (exactly one `Enter { cells }` cue, no `Meteor`, no `Jump`), and
    /// a PTY line feed sounds the cascade lane's `Jump` with no kitty, flare or
    /// paint (§8.2). The Enter's vector is diagonal (`√(60² + 2²) = 60.03`), so
    /// the cue must carry the integer the meteor flew, and `flight_ms` of it
    /// must equal the kitty's `t_flight`.
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
    /// one `Navigation` cue on the frame its light (a mini-fan at 2-7 cells,
    /// nothing at 1) is born, priced by that frame's spine and coloured by its
    /// field at the landing (§17.1). A no-op is silent (D11), and a
    /// program-driven hop is silent (see `sub_floor_hop`).
    #[test]
    fn a_sub_floor_hop_is_one_nav_tick_and_a_no_op_is_silent() {
        let t0 = Instant::now();
        let cfg = config();
        let ms = Duration::from_millis;

        // Warm: 20 keys lay 20 cells and light the spine; the hop back onto
        // laid column 15 on the next frame must carry that frame's spine —
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
    /// clock, reporting the events scheduled on a tick before that tick, as
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

    /// A run of fast clean keys opens the theme and a delete closes it (§23
    /// "Flow state"), on the real engine at the design's two cadences. The run
    /// counts only keys born at [`spine::FLOW_KEY_DISP`] or above, so entry
    /// lands [`spine::FLOW_ENTRY_KEYS`] keys after the hand is at speed.
    /// `heat` is exactly zero for the first twenty of those keys, eases over
    /// the last four, and is exactly 1.0 from the 24th. One Backspace takes the
    /// combo and heat to zero on the same edge and leaves the high-water mark.
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

            // …and a delete closes it, on the delete's own edge.
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

            // A kill is the second exit, and a key born under the floor the
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

    /// Flow draws nothing of its own: with the hand off the keys, an open
    /// theme is byte-identical to a cold one (nothing written, fingerprint
    /// zero, no cadence, no deadline), while `heat` is still 1.0 because the
    /// run is unbroken. No keystroke, no light.
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

        // The twin, at heat 0 on the same events: a celebration drive whose
        // floor is under the live metric holds the combo at zero without
        // changing the spine, so the only difference is the run. Once both are
        // dark their cadence and deadline agree.
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

    /// One frame at `now`, read back as the `RainbowExitSampling` model sees
    /// it: whether it put any light on glass, and whether the engine then asks
    /// the host for another frame (a cadence or a named deadline).
    fn exit_sample(eng: &mut Engine, now: Instant) -> (i64, i64) {
        let mut sc = Scratch::default();
        let mut fr = sc.frame();
        eng.tick(now, geom(), &config(), &mut fr);
        let fp = fr.fp;
        let visible = fp != 0
            || !sc.under.is_empty()
            || !sc.out.is_empty()
            || !sc.halos.is_empty()
            || !sc.beams.is_empty();
        let active = eng.needs_frame_cadence() || eng.next_change_deadline(now).is_some();
        (i64::from(visible), i64::from(active))
    }

    /// A fault injected into the engine at the late observation's instant,
    /// just before that frame runs — or none.
    type ExitFault = fn(&mut Engine, Instant);

    /// A real typing shape: drive it from a start instant, return its last key's.
    type TypingShape = fn(&mut Engine, Instant) -> Instant;

    /// The shipped engine, untouched.
    fn no_exit_fault(_: &mut Engine, _: Instant) {}

    /// The model's `Buggy = 1`, injected into the real engine: a lifecycle that
    /// advances per callback rather than by time restarts every swoosh that ran
    /// out unobserved at the first late observation
    /// (`Ribbon::restart_unobserved_swooshes`), and the real frame then lays
    /// the reach and arms the retract from there.
    fn callback_relative_swoosh(eng: &mut Engine, late: Instant) {
        eng.ribbon.restart_unobserved_swooshes(late);
    }

    /// Walk `model`'s whole space on the real engine, from three real typing
    /// shapes (a single key, a slow three-key word, a 44-key flow run) and
    /// three silences past the whole swoosh (5 s, 30 s, 10 min): the ribbon is
    /// lit at the last key, logical time passes with NO callback, and one
    /// late tick is observed — with `fault` injected into the engine at it.
    fn exit_sampling_walk(
        model: &aterm_spec::derive::Model,
        fault: ExitFault,
    ) -> Result<usize, String> {
        let shapes: [(&str, TypingShape); 3] = [
            ("one key", |eng, t0| {
                blank_row(eng, 2);
                eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
                tick_at(eng, t0);
                type_key_at(eng, t0 + Duration::from_millis(100), 2)
            }),
            ("a slow word", three_typed_cells),
            ("a flow run", |eng, t0| {
                flow_run(eng, &mut Scratch::default(), t0, 44, 83);
                t0 + Duration::from_millis(83) * 44
            }),
        ];
        let mut observed = 0usize;
        for (shape, type_it) in shapes {
            for silence in [5u64, 30, 600] {
                let at =
                    |state: &aterm_spec::interp::State| format!("{shape}, {silence} s: {state:?}");
                let mut eng = engaged();
                let last = type_it(&mut eng, Instant::now());
                let (visible, active) = exit_sample(&mut eng, last);
                let lit = std::collections::BTreeMap::from([
                    ("logical_done", 0),
                    ("sampled", 0),
                    ("visible", visible),
                    ("active", active),
                ]);
                if lit != model.init_state() {
                    return Err(format!(
                        "the last key does not leave a live ribbon: {}",
                        at(&lit)
                    ));
                }
                // Logical time passes the whole grace + reach + retract with no
                // callback at all: nothing on the engine moves.
                let mut elapsed = lit.clone();
                elapsed.insert("logical_done", 1);
                if model.successors("ElapseDone", &lit) != vec![elapsed.clone()] {
                    return Err(format!(
                        "the silence is not the model's ElapseDone: {}",
                        at(&lit)
                    ));
                }
                // The first observation after it.
                let late = last + Duration::from_secs(silence);
                fault(&mut eng, late);
                let (visible, active) = exit_sample(&mut eng, late);
                let mut sampled = elapsed.clone();
                sampled.insert("sampled", 1);
                sampled.insert("visible", visible);
                sampled.insert("active", active);
                let expected = model.successors("ObserveDone", &elapsed);
                if expected != vec![sampled.clone()] {
                    return Err(format!(
                        "real ObserveDone took {} to {sampled:?}, the model's ObserveDone to \
                         {expected:?}",
                        at(&elapsed)
                    ));
                }
                observed += 1;
            }
        }
        Ok(observed)
    }

    /// Tier-1 conformance for `RainbowExitSampling`
    /// (`aterm_spec::derive::rainbow_exit_sampling_model`): the exit swoosh is
    /// a function of time, not of callbacks. A live ribbon whose whole
    /// `grace + reach + retract + fade` passes with no frame is settled at the
    /// one late tick (nothing on glass, nothing armed), on the real engine, for
    /// every typing shape and silence.
    ///
    /// Negative control: the model's `Buggy = 1` injected into the same engine
    /// (every unobserved swoosh restarts at the late tick) is refused by the
    /// healthy model and matches the buggy one; the shipped engine is refused
    /// by the buggy model.
    #[test]
    fn a_late_tick_samples_the_settled_exit_swoosh() {
        let model = aterm_spec::derive::rainbow_exit_sampling_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let observed =
            exit_sampling_walk(&model, no_exit_fault).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(observed, 9, "three typing shapes, three silences each");
        let refused = exit_sampling_walk(&model, callback_relative_swoosh)
            .expect_err("the healthy model must refuse a callback-relative swoosh");
        assert!(refused.contains("real ObserveDone"), "{refused}");
        exit_sampling_walk(&buggy, callback_relative_swoosh).unwrap_or_else(|why| {
            panic!("the buggy model must describe the callback-relative swoosh: {why}")
        });
        exit_sampling_walk(&buggy, no_exit_fault)
            .expect_err("the shipped engine must not conform to the callback-relative model");
    }

    /// The live (not retracting) ribbon cell at `(row, col)`.
    fn live_cell(eng: &Engine, row: u16, col: u16) -> Option<ribbon::Cell> {
        eng.ribbon
            .cells()
            .iter()
            .find(|c| c.row == row && c.col == col && c.retract_at.is_none())
            .copied()
    }

    /// The typo script (§23 "The mend"): eight keys at 8 cps from column 4 of
    /// row 3 (every 15th tick), a Backspace one key-slot later, then the fix
    /// key one slot after that. Returns the schedule and the ticks of the erase
    /// and the fix.
    fn typo_script(
        erase_after: u64,
        deletes: u16,
        fix_after: u64,
    ) -> (Vec<(u64, Event)>, u64, u64) {
        let mut out = Vec::new();
        let mut col = 4u16;
        // The key, then its echo: a `Typed` with no move after it in the
        // frame is held, not laid.
        for k in 0..8u64 {
            out.push((15 * k, typed(1)));
            out.push((15 * k, mv((3, col), (3, col + 1), Licence::Typed)));
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
        out.push((fix, typed(1)));
        out.push((fix, mv((3, col), (3, col + 1), Licence::Typed)));
        (out, last_erase, fix)
    }

    /// The mend (§23 "The mend"): momentum resumes after a typo fix rather
    /// than starting over. Eight keys at 8 cps, a Backspace, then the fix key
    /// one slot later, 500 ms later, and after a 250 ms pause with two
    /// deletes: the fix cell is born at the momentum the deleting interrupted
    /// (the birth spine on the frame before the first Backspace), so its
    /// `cov0` and life match the run it re-joins and the bed shows no dip.
    /// The erased cell's retract still starts on the erase edge (T5), the fix
    /// key owns its light on its own frame, and the mark is spent by the fix.
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
    /// three deletes, is not a fix; it is priced at the live `birth_disp`, and
    /// the mark (still there as a value) is not a mend when the key reads it.
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

    /// A value, not a timer (T6 at the mark): one key, one Backspace, and the
    /// glass goes dark inside the mend window; while the mend is live the
    /// engine asks for no cadence, names no deadline and fingerprints zero,
    /// and nothing wakes when the window closes. A key typed over the dark
    /// glass inside the window is mended: born at the mark, above the live
    /// price.
    #[test]
    fn the_mend_mark_is_a_value_not_a_timer() {
        let t0 = Instant::now();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let sched = [
            (0, typed(1)),
            (0, mv((3, 4), (3, 5), Licence::Typed)),
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
        let fix_sched = [(fix, typed(1)), (fix, mv((3, 4), (3, 5), Licence::Typed))];
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

    /// §6.11 / §7.1: under reduced motion a credited spawn is a landing with
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

    /// §7.1: the flare is one edge. It rides out as `Some(t₀)` on the spawn
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

    /// §7.1 / §17.1: the caret's paint is the display spine. After a burst and
    /// 220 ms of silence it reads `momentum_display()` to the bit (≥ 70 % of
    /// its typing value, on the ribbon's τ 0.85 s release), not the 0.5 a
    /// private 220 ms half-life would give. A typed key does not lift it above
    /// the spine. A credited landing floors it at exactly `1.0` on the landing
    /// frame, holds it through the flare's relax, cools it on the flare's
    /// constant, and hands it back to the bare spine (exactly `0` on a cold
    /// engine) at its life.
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

    /// §5.8 / §7.2 / D5 at the seam: the kitty's Delight earns one m1 at the
    /// reported cell and mints the `Delight` impulse; each hero chimes once
    /// while the bucket has a token and is still born when it does not —
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

    /// The party reaches the sky (§27): a bar reported through
    /// [`Engine::party`] mints its fan on the next tick. Three-sided, for the
    /// three ways this can be wrong: a silent no-op, a fan nobody asked for,
    /// and a fan in a coordinate space the caret no longer means anything in.
    #[test]
    fn a_reported_bar_mints_its_fan_and_a_withdrawn_one_mints_nothing() {
        let t0 = Instant::now();
        let cfg = config();
        let mut sc = Scratch::default();

        // A. engaged, caret known, sky probed: the bar is on glass.
        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.on_event(mv((3, 5), (3, 6), Licence::Typed), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        let before = eng.status().stars;
        eng.party(
            t0 + Duration::from_millis(1),
            crate::kitty_sing::BAR_FAN_CAP,
            true,
        );
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(2), geom(), &cfg, &mut fr);
        let after = eng.status().stars;
        assert!(
            after > before,
            "the host's bar minted NOTHING: stars {before} -> {after}"
        );

        // B. The same bar across a coordinate-space cut mints nothing: `reset`
        // forgets the caret, so a bar held across it would fan at (0, 0) in
        // the new space with no key behind it.
        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.on_event(mv((3, 5), (3, 6), Licence::Typed), t0);
        let mut fr = sc.frame();
        eng.tick(t0, geom(), &cfg, &mut fr);
        eng.party(
            t0 + Duration::from_millis(1),
            crate::kitty_sing::BAR_FAN_CAP,
            true,
        );
        eng.reset();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        // …and the caret is learned again in the new space, so without the
        // clear at the `reset` seam the stale bar would fan at it.
        eng.on_event(
            mv((9, 40), (9, 41), Licence::Typed),
            t0 + Duration::from_millis(2),
        );
        let before = eng.status().stars;
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(3), geom(), &cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            before,
            "a bar banked before a space cut fanned into the new space"
        );

        // C. A bar with no observed caret mints nothing either (the law
        // `earn_hero` obeys), so B holds even without the `reset` clear.
        let mut eng = engaged();
        for row in 0..26u16 {
            blank_row(&mut eng, row);
        }
        eng.party(t0, crate::kitty_sing::BAR_FAN_CAP, true);
        let mut fr = sc.frame();
        eng.tick(t0 + Duration::from_millis(1), geom(), &cfg, &mut fr);
        assert_eq!(
            eng.status().stars,
            0,
            "a bar laid before the caret was ever observed"
        );
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

    /// Seam point 12, the band path: a band move is a coordinate transform,
    /// never a reset. Six cells typed on row 3 with the caret at (3, 6) ride
    /// a band `[2..10]` down to row 4 with the field intact (`field_at(4, 5)`
    /// answers what `field_at(3, 5)` did; the vacated cell answers nothing),
    /// the erased cell and earned heroes move with their rows, and the spine
    /// is untouched (the control is a reset, which restarts momentum). A band
    /// outside the caret's row leaves everything alone; a band the caret's
    /// row leaves saturates the caret at its edge and drops the light.
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

        // Control: the reset this path replaces restarts the momentum.
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
        // A band the caret's row leaves: the position saturates at the edge,
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
        // pins is that every one of them moves and none is lost.
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
        // frame legal), so the only difference between the two frames is the
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

    /// The curtain through the engine: the ribbon eases out over 0.24 s while
    /// everything else resets (caret mirror unknown, ledger empty), and a
    /// hidden frame runs every clock and writes nothing.
    #[test]
    fn a_curtain_keeps_the_ribbon_easing_while_the_rest_resets_and_a_hidden_frame_writes_nothing() {
        let cfg = config();
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        tick_at(&mut eng, t);
        let cells = eng.status().cells;
        assert!(cells >= 3, "three cells laid ({cells})");
        let ms = |base: Instant, ms: u64| base + Duration::from_millis(ms);
        // Hidden: the same state, nothing written.
        eng.hide_next_frame();
        // A fresh scratch per frame: the host's is appended to, never cleared.
        let (under, out_n, halos, quads, fp) = {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(ms(t, 8), geom(), &cfg, &mut f);
            (
                f.under.len(),
                f.out.len(),
                f.halos.len(),
                eng.status().quads,
                eng.fingerprint(),
            )
        };
        assert_eq!(
            (under, out_n, halos, quads, fp),
            (0, 0, 0, 0, 0),
            "a hidden frame writes nothing and reports nothing written"
        );
        assert_eq!(eng.status().cells, cells, "…and keeps every cell");
        // Back: the next frame draws it again.
        let drawn = {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(ms(t, 16), geom(), &cfg, &mut f);
            !f.under.is_empty()
        };
        assert!(
            drawn && eng.status().quads > 0,
            "unhidden, the band is back"
        );
        // The curtain: cells kept and easing; the caret forgotten; a key
        // during it lays nothing; gone at 0.24 s.
        let fell = ms(t, 24);
        eng.curtain(fell);
        assert!(
            !eng.caret_known,
            "the caret is learned from the next observed move"
        );
        assert_eq!(
            eng.ribbon().cells().len(),
            cells as usize,
            "the ribbon's cells are kept"
        );
        eng.on_event(typed(1), ms(fell, 30));
        // The fall is staggered (`ribbon::CURTAIN_STAGGER_S`): the cell
        // farthest from the hand starts at the curtain's instant and the one
        // under the hand at half of `CURTAIN_S`, so the band's total light,
        // not its peak, falls from the first frame.
        let band = |eng: &mut Engine, at: Instant| -> (u8, u32) {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(at, geom(), &cfg, &mut f);
            (
                f.under.iter().map(|q| q.alpha).max().unwrap_or(0),
                f.under.iter().map(|q| u32::from(q.alpha)).sum(),
            )
        };
        let mut prev = band(&mut eng, ms(fell, 8)).1;
        for at in [60u64, 120, 180] {
            let (_, sum) = band(&mut eng, ms(fell, at));
            assert!(
                sum < prev,
                "+{at} ms: the band's light falls every frame of the curtain ({prev} → {sum})"
            );
            prev = sum;
        }
        let (mid, _) = band(&mut eng, ms(fell, 200));
        assert!(
            mid > 0 && mid < 150,
            "five sixths through, the band is spending ({mid})"
        );
        assert_eq!(
            eng.status().cells,
            cells,
            "…and the key under the curtain laid nothing"
        );
        let left = {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(ms(fell, 260), geom(), &cfg, &mut f);
            f.under.len()
        };
        assert_eq!(
            (left, eng.status().cells),
            (0, 0),
            "gone at the curtain's end"
        );
        assert!(
            eng.ribbon().at_rest() && eng.ribbon().next_change_deadline(ms(fell, 260)).is_none(),
            "the ribbon is idle → zero after it (the key's own spine and ledger are the engine's)"
        );
    }

    /// An un-taken hide is dropped, never carried: [`Engine::hide_next_frame`]
    /// is taken by the tick, so a style switch, teardown or curtain between
    /// the arm and the tick must drop it.
    #[test]
    fn a_hide_is_taken_by_one_frame_and_an_un_taken_one_is_dropped_by_a_reset_or_a_curtain() {
        let cfg = config();
        let ms = |base: Instant, ms: u64| base + Duration::from_millis(ms);
        let drawn = |eng: &mut Engine, at: Instant| -> usize {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(at, geom(), &cfg, &mut f);
            f.under.len()
        };

        // One frame, and the arm is spent by it.
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0, "fixture: a band is drawn");
        eng.hide_next_frame();
        assert!(eng.hide_armed());
        assert_eq!(
            drawn(&mut eng, ms(t, 16)),
            0,
            "the hidden frame writes nothing"
        );
        assert!(!eng.hide_armed(), "…and the tick took the arm");
        assert!(drawn(&mut eng, ms(t, 24)) > 0, "the next frame draws again");

        // A reset between the arm and the tick drops it.
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0);
        eng.hide_next_frame();
        eng.reset();
        assert!(!eng.hide_armed(), "a reset drops an un-taken hide");

        // …and so does a curtain, whose own frames must be drawn.
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        assert!(drawn(&mut eng, ms(t, 8)) > 0);
        eng.hide_next_frame();
        eng.curtain(ms(t, 12));
        assert!(!eng.hide_armed(), "a curtain drops an un-taken hide");
        assert!(
            drawn(&mut eng, ms(t, 20)) > 0,
            "the curtain's first frame was eaten by a hide armed before it"
        );
    }

    /// The status verb tells the truth while the curtain falls.
    /// `ribbon_segments` is floored at `ribbon::STATUS_LIT_COV`, a bound on
    /// what the row may claim, so it reaches zero while dim quads are still
    /// composited. `ribbon_drawn` answers per stop against each boundary's own
    /// composite, and `ribbon_curtain_ms` says why the claim is under its
    /// floor.
    #[test]
    fn a_falling_curtain_is_reported_drawn_on_every_frame_it_composites() {
        let cfg = config();
        let ms = |base: Instant, ms: u64| base + Duration::from_millis(ms);
        let t0 = Instant::now();
        let mut eng = engaged();
        let t = three_typed_cells(&mut eng, t0);
        // `(quads emitted, does any of them composite to something the glass
        // census would count as band ink)` — the second read straight off the
        // frame's own pixels, through the same predicate the status row uses,
        // so the pin compares the verb with the pixels and not with itself.
        let quads = |eng: &mut Engine, at: Instant| -> (usize, bool) {
            let mut sc = Scratch::default();
            let mut f = sc.frame();
            eng.tick(at, geom(), &cfg, &mut f);
            let ink = f.under.iter().any(|q| {
                ribbon::reads_as_ink(aterm_render::over_premul(cfg.theme_bg, q.color, q.alpha))
            });
            (f.under.len(), ink)
        };
        assert!(quads(&mut eng, ms(t, 8)).1);
        let fell = ms(t, 16);
        eng.curtain(fell);
        let mut lied = Vec::new();
        let mut honest = 0;
        let mut covered = 0;
        for at in (0..=240u64).step_by(8) {
            let (n, ink) = quads(&mut eng, ms(fell, at));
            let drawn = eng.ribbon_drawn();
            let left = eng.curtain_left_ms(ms(fell, at));
            if n > 0 {
                assert!(
                    left.is_some(),
                    "+{at} ms: {n} quads composited and the row says no curtain is falling"
                );
            }
            // The law: the row answers `ribbon_drawn=0` only when the pixels
            // it is answering about do not read as band ink. A frame whose
            // own composite the census would count, reported as nothing, is
            // the bound returned as a fact.
            if ink {
                if drawn == 0 {
                    lied.push((at, n));
                } else {
                    honest += 1;
                }
                if eng.ribbon_segments() == 0 {
                    covered += 1;
                    assert!(
                        left.is_some_and(|ms| ms > 0),
                        "+{at} ms: the claim is at zero over a visible band and the row does \
                         not say a curtain is why"
                    );
                }
            }
        }
        assert!(
            honest >= 10,
            "the sweep never saw a visible curtain frame ({honest})"
        );
        assert!(
            lied.is_empty(),
            "the row reported `ribbon_drawn=0` on frames whose own pixels read as band ink: \
             {lied:?}"
        );
        // And there are frames where the claim has given up
        // (`ribbon_segments=0`) while the band is still on glass; those frames
        // must carry both the count and the reason.
        assert!(
            covered >= 2,
            "the sweep never reached the window the defect lives in: \
             {covered} frames with the claim at zero over a visible band"
        );
    }

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

    /// C2 at the seam, and D4: `Frame.caret.field_t` is this frame's field at
    /// this frame's caret — sampled after the events are laid and the ribbon
    /// has planned — so it equals `field()` on the same frame, and a meteor
    /// landing on a laid cell locks its phase (the cue's `hue`) to that cell,
    /// not to the cell the caret left.
    #[test]
    fn the_caret_seam_reports_this_frames_field_at_the_landing() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();

        // Lay 30 cells with the caret at column 30: the keys, then their
        // echo.
        eng.on_event(typed(30), t0);
        eng.on_event(mv((3, 0), (3, 30), Licence::Typed), t0);
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

    // -----------------------------------------------------------------------
    // panel #10 — the pet and the sky (D13's wiring gap, and two offers more)
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

    /// Panel #10(a), D13: `Engine::pet_offer` reads `BodyImpulse::Perk { at }`
    /// minted for every credited meteor, and the instant it hands the resident
    /// is the landing's (the same `Instant` as the pin, the arrival flash, the
    /// caret's flare and the audio bell, §6.7, §8.1 no. 3). A settled pet perks
    /// exactly there and is not moved.
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

        // What the pixels committed to on this frame.
        assert_eq!(eng.meteor.live.len(), 1, "the jump must have flown");
        let arrival = eng.meteor.live[0].arrival();
        let flight = arrival.saturating_duration_since(t0);
        assert!(
            (60..=120).contains(&flight.as_millis()),
            "fixture: T must be a real flight, got {flight:?}"
        );

        // What the router hands the resident, through the one published call.
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

        // The offer is the router's answer: the same impulse routed with
        // `Body::Pet` says the same thing and still refuses to relocate the
        // cat (no teleport).
        assert_eq!(
            companion::impulse_for(companion::Body::Pet, imp),
            companion::BodyImpulse::Perk { at: arrival }
        );

        // A pet-less host is still told the edge (the offer is about the
        // meteor, not about where the cat is standing) …
        assert_eq!(eng.pet_offer(geom(), None).perk_at, Some(arrival));

        // … and reduced motion offers no edge at all: there was no flight to
        // wait out, so the impulse is a `Land` pose, not an instant.
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

    /// Panel #10(b), the cat catches a star. A gold m1 (about one key in
    /// eighty) born within [`companion::CATCH_REACH_CELLS`] of a settled,
    /// contented cat is offered to it; on the paw's landing frame that one
    /// star's life is spent on the sky's 40 ms finish. The star is still
    /// keystroke-born (T1), the cat's reaching draws nothing, and exactly one
    /// life moves.
    #[test]
    fn a_gold_star_near_a_contented_cat_is_offered_and_caught() {
        let t0 = Instant::now();
        let cfg = config();
        let mut eng = engaged();
        let mut sc = Scratch::default();
        let ms = Duration::from_millis;
        blank_row(&mut eng, 2);

        // Earn heroes along row 3 until the deal hands out a gold one. The
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

        // The offer. A settled, purring cat six cells wide, its nose within
        // ten columns of the star.
        let near = cat(3, f32::from(col) + 4.0, true, 0.4);
        let offer = eng.pet_offer(geom(), Some(near));
        let star = offer.catch.expect("a gold m1 in reach is offered");
        assert_eq!(star.born, born_at, "the offer names the star's own edge");
        assert!(
            near.columns_to(star.col) <= companion::CATCH_REACH_CELLS,
            "the offered star must be inside the reach it was offered under"
        );

        // The gates. A cat mid-pounce is busy; a cold cat is not contented;
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

        // The catch. The paw lands 200 ms after the star was born — well
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

        // … no light was born (T1: the pet's own motion is not light) …
        let (q, h, cues) = (sc.out.len(), sc.halos.len(), sc.cues.len());
        let mut fr = sc.frame();
        eng.tick(paw, geom(), &cfg, &mut fr);
        assert!(
            eng.status().stars <= stars_before,
            "the catch minted a star"
        );
        assert_eq!(sc.cues.len(), cues, "the catch minted a sound");
        assert!(sc.out.len() >= q && sc.halos.len() >= h, "scratch shrank");

        // … and NO wake was bought: a life that ends sooner can only bring
        // the sky's next deadline forward, never push it out.
        if let (Some(before), Some(after)) = (deadline_before, eng.next_change_deadline(paw)) {
            assert!(
                after <= before,
                "the catch may not ask for a later frame than the sky already wanted"
            );
        }

        // A second paw on a spent offer finds nothing to lengthen.
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

    /// Panel #10(c), C2 extended to the cat: a contented resident's ♪ and ♥
    /// take the ribbon's field at the cat's cell, snapped (C1: a mote is a
    /// point mark). Where no ribbon light is under the cat the offer is `None`
    /// and the pet keeps its own colour.
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

    /// The scratch is the host's and is reused: a tick must never assume it
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
            color2: 0x0011_2233,
            alpha2: 0,
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

    /// `Config::from_glow` is the one place the host's vocabulary meets v2's.
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
            audible: true,
            radius: 0.6,
            ring: true,
            dark_theme: false,
            theme_fg: 0x0012_3456,
            theme_bg: 0x0065_4321,
            beam: false,
            head_dx: 0.5,
            pack: None,
            ribbon_tall: false,
            ribbon_flat: false,
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

    /// The sim's own no-busy-loop guard. [`ARM_MIN`] is under test, so the sim
    /// cannot lean on it: a deadline at or before `now` is stepped past by this
    /// much and counted, as a host booking `deadline_arms_by_owner` would.
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

    /// A host loop in miniature: wakes at every scripted event and at the
    /// deadline `ask` names, ticks the engine on each wake as the host does,
    /// and records one turn per wake (a `Some` deadline is one booked arm).
    /// Stops at `until`, or when nothing is scripted and no deadline is named.
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

    /// The tail law (§18): after the last key, while only fades are live, the
    /// engine asks for no more than about 25 wakes a second ([`TAIL_FLOOR`]
    /// plus a twinkle step), and once idle it asks for nothing and stays so.
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
        // The swoosh's fade is its last 0.24 s (`RETRACT_FADE_S`), wherever
        // the phrase rest puts the swoosh's end.
        let (f0, f1) = (ribbon::SWOOSH_TOTAL_S - 0.24, ribbon::SWOOSH_TOTAL_S);
        let fade = arms_in(&turns, last, f0, f1);
        assert!(
            fade <= budget(f0, f1),
            "swoosh fade: {fade} arms in 240 ms (budget {})",
            budget(f0, f1)
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

    /// The fold ([`Cadence`]): brisk answers the next frame whatever else was
    /// offered; a tail is never sooner than [`TAIL_FLOOR`] and keeps its own
    /// later step; a past edge is not a next change; a past tail instant is
    /// the floor; nothing offered is `None`.
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

    /// The flight law (T2, T7, §6.2): from the spawn frame through the landing
    /// pin's close (`T .. T + 150`) the engine asks for every frame, each wake
    /// exactly [`FRAME_CADENCE`] after the last, with seam point 8 agreeing.
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

    /// A batched echo sweeps every glyph cell it skipped (seam point 1,
    /// [`Event::Sweep`]): three keys land before the PTY echoes once, three
    /// cells at a time (SSH, a busy shell). Keys lay at the landing, so the
    /// two cells before it come from the sweep (or from the ledger when the
    /// host sends none, [`Engine::echo_bridge`]). The per-key path is
    /// untouched. The control is the same batch with no presses behind it
    /// (program output), which lays nothing.
    #[test]
    fn a_batched_echo_sweeps_every_glyph_cell_it_skipped() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool, pressed: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            // A first key, then its echo onto column 2 (a key with no move
            // after it in the frame is held, not laid).
            if pressed {
                eng.on_event(typed(1), t0);
            }
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
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

    /// A typed fold wraps at the pane's edge, not the grid's: in a split pane
    /// twelve columns in and forty wide, the key that wraps its line lands at
    /// `(3, 12)` and its glyph is the previous row's last pane cell, `(2, 51)`.
    /// Without [`Engine::set_pane`] the ribbon would light `(3, 11)` in the
    /// neighbouring pane (the control).
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

    /// A key whose echo lands a frame late still lights its glyph
    /// ([`Event::Sweep`] for a one-cell echo). The key is held on the frame
    /// it is typed rather than re-laying the previous cell, and its cell comes
    /// from the echo's sweep; a key whose echo shares its tick is untouched.
    /// The ledger also pays it without the host's sweep, so the control is a
    /// one-cell move with no press behind it.
    #[test]
    fn a_key_whose_echo_lands_a_frame_late_still_lights_its_glyph() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let cfg = config();
        let g = geom();
        let drive = |sweep: bool, pressed: bool| -> Engine {
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            // A first key, then its echo onto column 2 (a key with no move
            // after it in the frame is held, not laid).
            if pressed {
                eng.on_event(typed(1), t0);
            }
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            let mut sc = Scratch::default();
            let mut fr = sc.frame();
            eng.tick(t0, g, &cfg, &mut fr);
            // The key, echoed a frame later: held on the frame it is typed
            // (its cell comes with the echo below), never laid at the
            // previous glyph's cell.
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

    /// A press waits on the ledger for the whole patience: a key refused at
    /// the seam and the next key nine seconds later (inside
    /// `ECHO_PATIENCE_S`), with the host's sweep on its clock; the stalled
    /// press pays for its cell.
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

    /// Forty presses typed into a stall fit the ledger: forty `Typed` banked,
    /// the host's sweep 5..45 on the oldest press's clock and the move from
    /// the mirror; the ledger spends forty (tallied as spent, not forfeited).
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

    /// A batch sweep on the oldest press's clock is born no earlier than the
    /// birth floor: one stamp window before the echo, so the echoing frame
    /// and the next both show the run lit.
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

    // ───────────── `pre_move` and the held Space ─────────────

    /// A scroll carries the pre-move caret with the text it names.
    /// [`Engine::pre_move`] holds the caret the frame's type-ahead keys replay
    /// at, and `CursorGlow::note_scroll` can flush a park (recording the
    /// pre-scroll mirror) before it translates, so the record must be
    /// translated with the events ([`Engine::translate_pre_move`]). The shape:
    /// a glyph typed before an arrow's hop lands, then the PTY's one-row
    /// scroll before their frame. The seed lays no cell of its own, so the
    /// `typing` cell asserted is the replayed key.
    ///
    /// A band move (`[0..=10] −1`, moving row 3 to row 2 as the scroll does)
    /// carries it too; one fixture, a row per seam.
    #[test]
    fn a_scroll_or_a_band_move_carries_the_pre_move_caret_with_the_text_it_names() {
        /// The seam, and the move it makes before the buffered frame.
        type Seam = (&'static str, fn(&mut Engine));
        let seams: [Seam; 2] = [
            ("scroll", |eng| eng.translate_scroll(1, geom().ch as u16)),
            ("band", |eng| {
                eng.translate_band(0, 10, -1, geom().ch as u16, 0)
            }),
        ];
        for (seam, shift) in seams {
            let ms = Duration::from_millis;
            let t0 = Instant::now();
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            blank_row(&mut eng, 3);
            eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            // A key and a one-shot move buffered: `pre_move` names the caret
            // `(3, 5)`, and the key belongs one column left of it.
            eng.on_event(typed(1), t0 + ms(100));
            eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
            // …and the seam moves the text before their frame.
            shift(&mut eng);
            tick_at(&mut eng, t0 + ms(120));
            assert!(
                eng.ribbon()
                    .cells()
                    .iter()
                    .any(|c| c.typing && (c.row, c.col) == (2, 4)),
                "the key rides the {seam} with its glyph: {:?}",
                typing_cells(&eng)
            );
            assert!(
                eng.ribbon().cells().iter().all(|c| !c.typing || c.row != 3),
                "…and nothing of it is left a row below ({seam}): {:?}",
                typing_cells(&eng)
            );
        }
    }

    /// A pre-move caret carried off the top is forgotten, not clamped (the
    /// mark law): the key is held with no cell until its echo pays for it,
    /// and the record keeps its index so the key does not fall through to the
    /// post-move caret. Four rows, so the mirror's row 3 is off the top.
    #[test]
    fn a_pre_move_caret_carried_off_the_top_is_forgotten_not_clamped() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 0);
        blank_row(&mut eng, 3);
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        eng.on_event(typed(1), t0 + ms(100));
        eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
        eng.translate_scroll(4, geom().ch as u16);
        tick_at(&mut eng, t0 + ms(120));
        assert!(
            eng.ribbon().cells().iter().all(|c| !c.typing),
            "the key whose row scrolled away lays nowhere — not at row 0, not \
             on the row it was typed on: {:?}",
            typing_cells(&eng)
        );
    }

    /// The cells the hand's own keys laid, for the three assertions above.
    fn typing_cells(eng: &Engine) -> Vec<(u16, u16)> {
        eng.ribbon()
            .cells()
            .iter()
            .filter(|c| c.typing)
            .map(|c| (c.row, c.col))
            .collect()
    }

    /// The pre-move index survives a bridge's insert: a later typed move in
    /// the same frame whose echo the ledger pays inserts its bridge `Sweep`
    /// at index 0. The shape: a glyph typed before an arrow's hop lands, then
    /// a second glyph whose one-cell echo the host did not sweep, in one
    /// frame; the first glyph replays at the caret before the hop, `(3, 4)`.
    #[test]
    fn a_bridge_sweep_inserted_after_a_one_shot_move_does_not_shift_the_pre_move_caret() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let k = t0 + ms(100);
        eng.on_event(typed(1), k);
        eng.on_event(mv((3, 5), (3, 20), Licence::Nav), k + ms(10));
        eng.on_event(typed(1), k + ms(20));
        eng.on_event(mv((3, 20), (3, 21), Licence::Typed), k + ms(30));
        tick_at(&mut eng, k + ms(30));
        assert_eq!(
            eng.status().bridged,
            1,
            "the second glyph's unswept echo is the ledger's bridge — the insert at index 0"
        );
        assert!(
            eng.field_at(3, 4).is_some(),
            "the glyph typed before the arrow's hop replays at the caret before it"
        );
    }

    /// A reset or a curtain forgets the pre-move caret with the events it
    /// indexed, so a key typed in the new coordinate space is never replayed
    /// at the old space's caret. One fixture, a row per seam.
    #[test]
    fn a_reset_or_a_curtain_forgets_the_pre_move_caret_with_the_events_it_indexed() {
        let ms = Duration::from_millis;
        /// The seam, the forgetting call, and when the next key lands.
        type Seam = (&'static str, fn(&mut Engine, Instant), u64);
        let seams: [Seam; 2] = [
            ("reset", |eng, _| eng.reset(), 200),
            // Well past the curtain's own easing, so nothing it draws can
            // answer for the assertion below.
            (
                "curtain",
                |eng, t0| eng.curtain(t0 + Duration::from_millis(120)),
                900,
            ),
        ];
        for (seam, forget, key_ms) in seams {
            let t0 = Instant::now();
            let mut eng = engaged();
            blank_row(&mut eng, 2);
            eng.on_event(mv((3, 0), (3, 5), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            // A key and a one-shot move buffered, then the seam before their
            // frame.
            eng.on_event(typed(1), t0 + ms(100));
            eng.on_event(mv((3, 5), (3, 20), Licence::Nav), t0 + ms(110));
            forget(&mut eng, t0);
            // The next frame, in the new space: a key and its own echo.
            let k = t0 + ms(key_ms);
            eng.on_event(typed(1), k);
            eng.on_event(mv((7, 0), (7, 10), Licence::Typed), k + ms(8));
            tick_at(&mut eng, k + ms(8));
            assert!(
                eng.field_at(3, 4).is_none(),
                "the key is not replayed at a caret the {seam} forgot"
            );
            assert!(
                eng.field_at(7, 9).is_some(),
                "…it lays at its own echo ({seam})"
            );
        }
    }

    /// A key typed at a caret the engine never learned is held: before the
    /// first licensed move since a reset the record carries no caret, and the
    /// `Typed` before the one-shot move is held, never replayed at the unset
    /// mirror or the post-move caret's neighbour. Its cell comes with its
    /// echo, from the seam's sweep.
    #[test]
    fn a_key_typed_before_the_first_licensed_move_is_held_not_laid_at_an_unset_mirror() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(typed(1), t0);
        eng.on_event(mv((3, 10), (5, 2), Licence::Nav), t0 + ms(10));
        tick_at(&mut eng, t0 + ms(10));
        assert!(
            eng.ribbon().cells().iter().all(|c| !c.typing),
            "nothing typed is lit — not at the unset mirror, not at the landing's neighbour: {:?}",
            eng.ribbon()
                .cells()
                .iter()
                .map(|c| (c.row, c.col, c.typing))
                .collect::<Vec<_>>()
        );
        // Its echo, the next frame: the sweep lays its cell.
        let echo = t0 + ms(30);
        eng.on_event(
            Event::Sweep {
                row: 5,
                col0: 2,
                col1: 3,
            },
            t0,
        );
        eng.on_event(mv((5, 2), (5, 3), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        assert!(
            eng.field_at(5, 2).is_some(),
            "the key's cell comes with its echo"
        );
    }

    /// A held Space remembers its own cell: a Space held a frame before its
    /// echo stands on the mirror, not left of it, and the echo's sweep cannot
    /// correct `last_space` (`swept_space_cell` reads only the frame's own
    /// keys). The shape: `ab`, a Space echoed a frame late, `cd`, and the wrap
    /// key that moves `cd` down; `cd` must be relaid as a two-cell word.
    #[test]
    fn a_held_space_measures_the_word_the_composer_moves_from_its_own_cell() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut eng = engaged();
        blank_row(&mut eng, 2);
        eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
        tick_at(&mut eng, t0);
        let mut t = t0;
        for col in 2..4u16 {
            t += ms(100);
            t = type_key_at(&mut eng, t, col);
        }
        // The Space, held on its own frame; its echo the frame after.
        let space_at = t + ms(100);
        eng.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Space,
            },
            space_at,
        );
        tick_at(&mut eng, space_at);
        let echo = space_at + ms(16);
        eng.on_event(
            Event::Sweep {
                row: 3,
                col0: 4,
                col1: 5,
            },
            space_at,
        );
        eng.on_event(mv((3, 4), (3, 5), Licence::Typed), echo);
        tick_at(&mut eng, echo);
        let mut t = echo;
        for col in 5..7u16 {
            t += ms(100);
            t = type_key_at(&mut eng, t, col);
        }
        // The wrap key: `cd` moves down with the caret, which lands after
        // `cde` at `(4, 5)`; the word is relaid from the Space's measure.
        let k = t + ms(100);
        eng.on_event(typed(1), k);
        eng.on_event(mv((3, 7), (4, 5), Licence::Typed), k + ms(8));
        tick_at(&mut eng, k + ms(8));
        assert!(
            eng.field_at(4, 2).is_some() && eng.field_at(4, 3).is_some(),
            "the moved word's two cells are relaid"
        );
        assert!(
            eng.field_at(4, 1).is_none(),
            "the word is `cd`, two cells — not three from a Space measured one cell left"
        );
    }

    /// The pool is handed the ribbon's floor (the wiring half of
    /// `meteor::spark_room`; the law itself is
    /// `the_shower_stops_where_the_hand_stopped`). The hand types row 6 from
    /// column 28 and presses `^A`: on the tick that carries the jump the pool
    /// already holds `(6, 28)`, since the ribbon replays the frame's events
    /// before the meteor classifies them.
    #[test]
    fn the_meteor_pool_is_handed_the_hand_s_floor() {
        let t0 = Instant::now();
        let mut eng = engaged();
        for row in 5..=7 {
            blank_row(&mut eng, row);
        }
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            None,
            "a pool that has seen no hand is bounded by nothing"
        );

        let mut t = t0;
        for col in 28..43_u16 {
            eng.on_event(typed(1), t);
            tick_at(&mut eng, t);
            let echo = t + Duration::from_millis(8);
            eng.on_event(
                Event::Sweep {
                    row: 6,
                    col0: col,
                    col1: col + 1,
                },
                t,
            );
            eng.on_event(mv((6, col), (6, col + 1), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            t = echo + Duration::from_millis(60);
        }
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            Some((6, 28)),
            "the typing's own leftmost column is the floor"
        );

        eng.on_event(mv((6, 43), (6, 28), Licence::Nav), t);
        tick_at(&mut eng, t);
        assert_eq!(
            eng.meteor.hand_floor_cell(),
            Some((6, 28)),
            "the ^A's landing is the hand reaching a column, and the pool has it on \
             the tick the flight is born"
        );
    }

    /// The new line's first key, pressed before its row change was seen,
    /// starts the walk again through the engine's own replay
    /// (`ribbon::Ribbon::fresh_line`). Thirty keys on row 3 from column 2,
    /// then the key that starts a new line (a main-screen Return,
    /// [`Event::Return`], or a composer's newline via the move-only fallback),
    /// the next glyph pressed and held, the box's `Licence::Return` move
    /// `(3, 32) → (4, 2)` observed after it, and the glyph's echo; then
    /// eighteen more keys. Timings: the Return with the key at +40 ms and the
    /// move at +60 ms; the newline with the key at +120 ms and the move at
    /// +150 ms. Row 4 starts its walk on the colour row 3 reached (`d0 = 0`),
    /// every cell `t_next + walk_t(col − 2)`, `1/16` a cell on the fast leg,
    /// whatever the repaint latency.
    ///
    /// The newline here is a top-anchored box's, which changes row. A
    /// bottom-anchored composer's same-row newline is pinned by `ribbon`'s
    /// `a_new_line_after_a_same_row_composer_newline_continues_the_colour_on_the_fast_leg`
    /// and `tests/composer_box_growth_wrap.rs`.
    #[test]
    fn a_new_line_s_first_key_pressed_before_the_row_change_was_seen_starts_the_walk_again() {
        fn key_on(eng: &mut Engine, t: Instant, row: u16, col: u16) -> Instant {
            eng.on_event(typed(1), t);
            tick_at(eng, t);
            let echo = t + Duration::from_millis(8);
            eng.on_event(
                Event::Sweep {
                    row,
                    col0: col,
                    col1: col + 1,
                },
                t,
            );
            eng.on_event(mv((row, col), (row, col + 1), Licence::Typed), echo);
            tick_at(eng, echo);
            echo
        }
        let ms = Duration::from_millis;
        for (what, main_screen, key_ms, seen_ms) in [
            ("a Return, key +40 ms, move seen +60 ms", true, 40, 60),
            (
                "a composer's newline, key +120 ms, move seen +150 ms",
                false,
                120,
                150,
            ),
            (
                "control: a Return, move seen +60 ms, key +80 ms",
                true,
                80,
                60,
            ),
            (
                "control: a newline, move seen +150 ms, key +170 ms",
                false,
                170,
                150,
            ),
        ] {
            let t0 = Instant::now();
            let mut eng = engaged();
            for row in 2..6 {
                blank_row(&mut eng, row);
            }
            eng.on_event(mv((3, 0), (3, 2), Licence::Typed), t0);
            tick_at(&mut eng, t0);
            let mut t = t0;
            for col in 2..32u16 {
                t = key_on(&mut eng, t + ms(60), 3, col);
            }
            let line = *eng
                .ribbon()
                .cohorts()
                .iter()
                .find(|k| k.row == 3 && !k.wake)
                .expect("row 3's line");
            assert_eq!(
                (line.anchor_col, line.col1, line.d0),
                (2, 32, 0.0),
                "{what}: the premise, a cold thirty-cell line"
            );
            let t_next = line.t_at(32);
            let gesture = t + ms(100);
            eng.on_event(if main_screen { Event::Return } else { typed(1) }, gesture);
            tick_at(&mut eng, gesture);
            let key = gesture + ms(key_ms);
            let seen = gesture + ms(seen_ms);
            let press = |eng: &mut Engine| {
                eng.on_event(typed(1), key);
                tick_at(eng, key);
            };
            let observe = |eng: &mut Engine| {
                eng.on_event(mv((3, 32), (4, 2), Licence::Return), seen);
                tick_at(eng, seen);
            };
            if key < seen {
                press(&mut eng);
                observe(&mut eng);
            } else {
                observe(&mut eng);
                press(&mut eng);
            }
            let echo = key.max(seen) + ms(50);
            eng.on_event(
                Event::Sweep {
                    row: 4,
                    col0: 2,
                    col1: 3,
                },
                key,
            );
            eng.on_event(mv((4, 2), (4, 3), Licence::Typed), echo);
            tick_at(&mut eng, echo);
            let mut t = echo;
            for col in 3..21u16 {
                t = key_on(&mut eng, t + ms(60), 4, col);
            }
            let new_line = *eng
                .ribbon()
                .cohorts()
                .iter()
                .filter(|k| k.row == 4 && !k.wake)
                .min_by_key(|k| k.anchor_col)
                .expect("row 4's line");
            assert_eq!(
                (new_line.anchor_col, new_line.t0.to_bits(), new_line.d0),
                (2, t_next.to_bits(), 0.0),
                "{what}: row 4 continues the colour ({} against {t_next}) and starts its walk \
                 (d0 {})",
                new_line.t0,
                new_line.d0
            );
            for col in 2..21u16 {
                let want = t_next + ribbon::walk_t(f32::from(col - 2));
                let got = eng.field_at(4, col);
                assert_eq!(
                    got.map(f32::to_bits),
                    Some(want.to_bits()),
                    "{what}: col {col} is {got:?}, the mint before the odometer laid {want}"
                );
            }
        }
    }
}
