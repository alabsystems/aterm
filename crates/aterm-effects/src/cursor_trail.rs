// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cursor motion trail: pure animation state.
//!
//! A cursor move with a correlated authored-input candidate spawns a comet of
//! fading [`TrailCell`]s along the swept path, brightest at the head (nearest
//! the new cursor) and dimming toward the tail. Bare cursor deltas and
//! timestamp-only input hints stay dark: neither proves the landing came from
//! that input. The comet fades within a few hundred milliseconds, and an idle
//! screen returns to zero work (no live sparks, no animation timer).
//!
//! This module owns only the math: it takes the observed cursor cell and an
//! injected [`Instant`] each frame and produces the current trail cells in
//! terminal coordinates (the host offsets them into window space). The
//! renderer only consumes the resulting [`TrailCell`]s.

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::TrailCell;

use crate::cursor_glow::{TypedStamps, band_row, hint_fresh, take_hint_fresh};

/// Coverage of the comet head (the cell nearest the cursor) at a gentle move:
/// a lone keystroke barely tints the glyph it lands on. The continuous AA beam
/// carries the visible streak, so the cell bed is a faint ember layer under
/// [`READABLE_ALPHA_CAP`].
const HEAD_ALPHA_GENTLE: u32 = 42;
/// Coverage of the comet head at full ignition (fast, sustained typing),
/// brighter but under [`READABLE_ALPHA_CAP`]. Ignition shows through tail
/// length, hot colour, the beam and the GPU bloom halo, not opacity over
/// glyphs.
const HEAD_ALPHA_IGNITED: u32 = 70;
/// Coverage of the comet TAIL (the cell farthest from the cursor) at a GENTLE move
/// — faint, so a lone jump reads as a directional whisper rather than a solid bar.
const TAIL_ALPHA_GENTLE: u32 = 12;
/// Coverage of the comet tail at full ignition: a brighter streak behind the
/// cursor (older, already-read cells), without piling opacity on the freshest
/// glyph.
const TAIL_ALPHA_IGNITED: u32 = 44;
/// Readability guard: the hard ceiling on any swept cell's coverage at any
/// ignition level. The trail is a background tint drawn under the glyph, so
/// capping it (~27%) keeps typed text legible even when a fast burst covers a
/// run of fresh cells. Pinned by a unit test for every intensity.
const READABLE_ALPHA_CAP: u8 = 70;
/// At full ignition a spark lives this many times longer than the configured fade,
/// so a fast sustained burst leaves a LONGER visible comet (more overlapping live
/// sparks) while a lone keystroke fades in the base time.
const IGNITED_LIFE_MULT: f32 = 1.6;
/// Typing continuity: at full sub-ignition warmth ([`TypingCadence::warmth`])
/// a spark lives up to this many extra base durations, so keystrokes at human
/// cadence (~150-350 ms apart) chain into a continuous ember bed (at 350 ms
/// cadence the converged life is ~454 ms). Warmth is 0 for a lone keystroke,
/// which keeps the plain fade.
const CHAIN_LIFE_MULT: f32 = 1.5;

/// ConPTY hide-bridge window, shared by the trail and glow engines: a cursor
/// that reappears within this long of last being visible is treated as having
/// moved from the last visible cell. Windows' conhost hides the cursor for
/// ~20-35 ms during every echo repaint; 150 ms bridges that while staying well
/// under deliberate hides (alt-screen apps drawing with the cursor hidden).
pub const HIDE_BRIDGE_MS: u64 = 150;
/// The bridge applies only when the reappear position is a plausible typing
/// move from the last visible cell (Chebyshev distance ≤ this): echo advances,
/// backspaces and wrap-adjacent steps bridge; a far teleport mid-hide (a
/// full-screen repaint parking the cursor elsewhere) stays suppressed.
pub const HIDE_BRIDGE_MAX_DIST: u16 = 2;
/// The bridge's wider reach while a typed/backspace key hint is fresh: batched
/// echoes (PTY coalescing under fast typing or key repeat) may still use their
/// last visible cell as a source. This only classifies the source; the
/// correlated movement candidate must still admit the landing. Unhinted moves
/// keep [`HIDE_BRIDGE_MAX_DIST`].
pub const HIDE_BRIDGE_TYPED_MAX_DIST: u16 = 8;
/// Rows a nav-witnessed landing may cross. A navigation gesture moves along
/// the logical line being edited, which spans one row unless the input box
/// has wrapped, so rows are capped at a wrapped box: enough for Ctrl-A/Home on
/// a multi-row composer, far short of a spinner repaint relocating the caret
/// seven rows away with no key behind it.
pub const HIDE_BRIDGE_NAV_MAX_ROWS: u16 = 4;

/// Columns a nav-witnessed landing may cross when the host never named a pane
/// (every shipped path does, via `note_pane_columns`): the classic terminal
/// width, never the whole grid.
pub const HIDE_BRIDGE_NAV_UNPANED_COLS: u16 = 80;

/// The window after a navigation press in which the caret may still be seen at
/// rest without that proving the key moved nothing. An echo is a PTY round
/// trip (12-40 ms in the pinned laws); once a presented frame sees the caret
/// still visible this long after the press, the press did not move it (e.g.
/// Ctrl-E at the end of the line), and a following hidden relocation belongs
/// to the program. Only the bridge closes; the hint keeps licensing ordinary
/// visible moves for its full window.
pub const NAV_NOOP_SETTLE_MS: u64 = 80;

/// How long after a navigation press a hidden→visible landing may still be
/// that press's echo. The nav hint lives [`CursorTrail::NAV_HINT_FRESH`]
/// (250 ms, so a second press gets its own licence); the bridge claims "this
/// landing is that key's echo", a PTY round trip plus a repaint. 150 ms is four
/// times the slowest landing any pinned law needs.
pub const NAV_BRIDGE_LANDING_MS: u64 = 150;

/// May a nav-witnessed hidden→visible landing use `seen` as its source cell?
///
/// The question is whether the caret was still visible around the time the key
/// was pressed: a sample within one hide window before the press is the cell it
/// moved from; an older one belongs to an earlier state. Both sides are
/// bounded: a sample much newer than the press proves the key moved nothing
/// ([`NAV_NOOP_SETTLE_MS`]). With the nav hint capped at `NAV_HINT_FRESH`, the
/// source is at most `NAV_HINT_FRESH + HIDE_BRIDGE_MS` old. Shared by both
/// engines.
#[must_use]
pub fn nav_bridge_source_ok(key: Instant, seen: Instant) -> bool {
    key.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS
        && seen.saturating_duration_since(key).as_millis() as u64 <= NAV_NOOP_SETTLE_MS
}

/// The whole nav-bridge admission, called by both engines: the source must be
/// a cell this key could have moved from ([`nav_bridge_source_ok`]) and the
/// landing must arrive while the echo could still be outstanding
/// ([`NAV_BRIDGE_LANDING_MS`]). Two separate claims (start cell vs arrival),
/// kept as separate bounds.
#[must_use]
pub fn nav_bridge_admits(key: Instant, seen: Instant, now: Instant) -> bool {
    nav_bridge_source_ok(key, seen)
        && now.saturating_duration_since(key).as_millis() as u64 <= NAV_BRIDGE_LANDING_MS
}

/// How far one hidden→visible reappear may sit from its source, in cells, and
/// on the nav lane what shape a row-crossing landing must have.
///
/// Rows and columns are judged separately: the typed and classic lanes keep a
/// Chebyshev ball (`rows == cols`); the nav lane is wide across and short down.
///
/// A nav landing one row off is a wrap hop or arrow and may sit at any column
/// of the pane. Two or more rows off, only Home/Ctrl-A lands where the engines
/// can know (the box's own start, upward); End lands wherever the text ends,
/// indistinguishable from a program re-laying its input box, so it is not
/// bridged across two or more rows (it is still licensed whenever the caret
/// was visible).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeReach {
    /// Columns the landing may sit from the source.
    pub cols: u16,
    /// Rows the landing may sit from the source.
    pub rows: u16,
    /// `Some(first column of the focused pane)` while the NAV lane is open: a
    /// landing two or more rows from its source must be Home/Ctrl-A's —
    /// upward, and within [`HIDE_BRIDGE_TYPED_MAX_DIST`] of that column.
    /// `None` on the typed and classic balls, which carry no line gesture.
    pub line_start: Option<u16>,
}

impl BridgeReach {
    /// Does this reach admit a hidden→visible landing at `to` from `from`
    /// (`(row, col)` each)?
    #[must_use]
    pub fn admits(self, from: (u16, u16), to: (u16, u16)) -> bool {
        let dr = to.0.abs_diff(from.0);
        let dc = to.1.abs_diff(from.1);
        if dr > self.rows || dc > self.cols {
            return false;
        }
        match self.line_start {
            // Two or more rows: Home/Ctrl-A, up to the box's first column.
            Some(col0) if dr >= 2 => {
                to.0 < from.0 && to.1.saturating_sub(col0) <= HIDE_BRIDGE_TYPED_MAX_DIST
            }
            _ => true,
        }
    }

    const fn ball(d: u16) -> Self {
        Self {
            cols: d,
            rows: d,
            line_start: None,
        }
    }
}

/// The bridge reach for one reappear: the focused pane's width and a wrapped
/// input box's height while a navigation hint is fresh, the Chebyshev typed
/// ball while a typed/backspace hint is fresh, the classic ball otherwise.
/// Shared by the trail and glow engines so they agree on every move.
///
/// `pane` is the focused pane's `(first column, width)` (`note_pane_columns`):
/// a word or line motion cannot leave the pane, so a wider landing was never
/// that gesture, and a landing two or more rows off must be Home's, at the
/// pane's first column ([`BridgeReach::line_start`]). In a single full-window
/// pane the column ceiling is the window width; rows and the line-gesture
/// shape do the work there.
#[must_use]
pub fn hide_bridge_reach(
    typed_hint_fresh: bool,
    nav_hint_fresh: bool,
    pane: Option<(u16, u16)>,
) -> BridgeReach {
    if nav_hint_fresh {
        let (col0, cols) = pane
            .filter(|(_, cols)| *cols > 0)
            .unwrap_or((0, HIDE_BRIDGE_NAV_UNPANED_COLS));
        BridgeReach {
            cols: cols.max(HIDE_BRIDGE_TYPED_MAX_DIST),
            rows: HIDE_BRIDGE_NAV_MAX_ROWS,
            line_start: Some(col0),
        }
    } else if typed_hint_fresh {
        BridgeReach::ball(HIDE_BRIDGE_TYPED_MAX_DIST)
    } else {
        BridgeReach::ball(HIDE_BRIDGE_MAX_DIST)
    }
}

/// Hidden→visible relocations the trail's hide bridge has judged, each once:
/// `bridged + declined` counts reappearances at a cell other than the last
/// seen. A nav-paired move lays no comet, so this is the trail's only
/// observable of the bridge decision, letting a test check the two engines
/// decided the same.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeTally {
    /// Relocations the bridge admitted a source cell for.
    pub bridged: u64,
    /// Relocations the bridge declined (no source cell: `spawn` never ran).
    pub declined: u64,
}

/// Integer linear interpolation `a + (b-a)*q` for `q` in `0..=1` (endpoints
/// inclusive). `a`/`b` are small coverage values, so `f32` is exact enough.
fn lerp_u32(a: u32, b: u32, q: f32) -> u32 {
    let q = q.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * q).round() as u32
}

/// Fingerprint the exact trail presentation handed to the renderer.
///
/// Colour is folded only while at least one cell is visible, preserving the
/// trail's `0 == idle` contract. The sentinel keeps a black trail colour from
/// becoming an empty multiply-add.
fn trail_presentation_fingerprint(cells: &[TrailCell], color: u32) -> u64 {
    let mut fp: u64 = 0;
    for cell in cells {
        fp = fp
            .wrapping_mul(1_000_003)
            .wrapping_add(((cell.row as u64) << 24) ^ ((cell.col as u64) << 8) ^ cell.alpha as u64);
    }
    if !cells.is_empty() {
        fp = fp
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(color) | (1 << 32));
    }
    fp
}

/// Apply a host presentation-opacity envelope to emitted trail cells and
/// return the fingerprint of the attenuated output.
///
/// Downstream of [`CursorTrail::tick`]: ignition is baked into spark heat and
/// lifetime at birth, so scaling [`TrailConfig::intensity`] would affect only
/// new sparks. This attenuates every live cell uniformly without mutating
/// engine state, so a recovery reveals the still-decaying bed continuously.
/// Non-finite input fails closed; zero gives the empty/zero presentation.
#[must_use]
pub fn project_trail_presentation(cells: &mut Vec<TrailCell>, color: u32, opacity: f32) -> u64 {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    cells.retain_mut(|cell| {
        cell.alpha = (f32::from(cell.alpha) * opacity).round() as u8;
        cell.alpha > 0
    });
    trail_presentation_fingerprint(cells, color)
}

/// Resolved tunables for the trail effect. `Copy` so the host can read it out of
/// `self` before borrowing per-window state.
#[derive(Clone, Copy, Debug)]
pub struct TrailConfig {
    /// Master on/off (config `cursor_trail`, default on).
    pub enabled: bool,
    /// How long a swept cell takes to fade fully out (config `cursor_trail_ms`).
    pub duration: Duration,
    /// Maximum comet length in cells: on a long jump only the brightest `max_len`
    /// cells nearest the cursor are kept (config `cursor_trail_length`).
    pub max_len: usize,
    /// Fully-resolved trail colour `0x00RRGGBB` (config `cursor_trail_color`, else
    /// the theme cursor colour). The host heats this toward the comet-core tint as
    /// `intensity` rises (see [`ignite`]).
    pub color: u32,
    /// Ignition intensity in `0.0..=1.0` from the host's typing cadence
    /// ([`TypingCadence`]): `0.0` is a gentle, short, dim wake; `1.0` ignites
    /// the comet (brighter capped core, longer hotter tail). Baked at spawn, so
    /// an in-flight comet keeps its birth heat.
    pub intensity: f32,
    /// Sub-ignition warmth in `0.0..=1.0` ([`TypingCadence::warmth`]): stretches
    /// spark lifetimes only (never colour or alpha) so human-cadence keystrokes
    /// chain into a continuous ember bed. `0.0` for a lone keystroke.
    pub warmth: f32,
}

/// One swept cell, fading from `born`.
#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    /// Coverage at birth (head bright, tail faint), scaled by time as it fades.
    born_alpha: u8,
    born: Instant,
    /// How long this spark takes to fade out, baked at spawn from the fade time
    /// and ignition intensity.
    life_ms: u64,
}

/// One slot of the trail's resident dedup table: maps a packed cell key to its
/// slot in the `out` Vec built this tick. A slot is current only while its
/// `epoch` matches `CursorTrail::dedup_epoch`; stale epochs read as empty, so
/// the per-tick clear is one counter bump.
#[derive(Clone, Copy, Default)]
struct DedupSlot {
    /// The `dedup_epoch` value of the tick that claimed this slot.
    epoch: u64,
    /// Packed cell `(row << 16) | col` this slot indexes.
    key: u32,
    /// The cell's index in this tick's `out`.
    out_idx: u32,
}

/// Per-window trail animation state.
#[derive(Default)]
pub struct CursorTrail {
    /// Live sparks, oldest first. Bounded by [`Self::MAX_SPARKS`] and by decay.
    sparks: Vec<Spark>,
    /// Last observed cursor cell (terminal coords), to detect a move. `None` when
    /// the cursor was hidden, so a hide/show gap never spawns a spurious comet.
    last: Option<(u16, u16)>,
    /// The last visible cursor cell and when it was seen, for the ConPTY hide
    /// bridge ([`HIDE_BRIDGE_MS`]): conhost hides the cursor during every echo
    /// repaint, which would otherwise drop the spark for nearly every key. When
    /// the cursor reappears within the window at a plausible typing distance
    /// (≤ [`HIDE_BRIDGE_MAX_DIST`]), an admitted move may use this cell as its
    /// source; a long hide or far teleport stays suppressed.
    last_visible: Option<((u16, u16), Instant)>,
    /// Fresh typed-echo key hints ([`Self::note_typed`]), the trail twin of
    /// `CursorGlow`'s typed/backspace hints: armed on a plain
    /// Character/Space/Backspace key (never Enter/Tab/nav/modified chords), it
    /// classifies a paired one-row move beyond the typed advance as a TUI
    /// repaint re-anchor (an Ink prompt rewrapping its input box), so `spawn`
    /// lays no comet across those cells. Banked per press, one stamp per echo,
    /// in lockstep with the glow engine's [`TypedStamps`]. Revoked in lockstep
    /// too (`revoke_input_hints_at`): the glow's press credit goes with the
    /// stamp only on a failed write; a queued key keeps its credit and has its
    /// stamp re-banked at delivery.
    type_hint: TypedStamps,
    /// Fresh-navigation veto twin of `CursorGlow::nav_hint` (same host site): a
    /// Ctrl-A/E leap never re-anchors even with a typed hint and blink live, so
    /// the two engines agree on a nav jump.
    nav_hint: Option<Instant>,
    /// One-shot LICENSE stamp for a cursor movement that is not a typed-glyph
    /// echo: Return, Tab/paste, reflow, and the scripted-preview seams. A fresh
    /// stamp licenses the following move exactly as `type_hint` does; consumed
    /// on the first observed move, fresh or stale.
    move_hint: Option<Instant>,
    /// Fresh repaint-blink hint ([`Self::note_repaint_blink`]), twin of
    /// `CursorGlow`'s: the focused terminal's hide-inside-DEC-2026 repaint
    /// bracket was seen. On the alt screen ([`Self::ctx_alt`]) the re-anchor
    /// also requires it, so vim/less moves (which never blink) keep their
    /// one-row comets while Claude Code re-anchors.
    blink_hint: Option<Instant>,
    /// Per-frame alt-screen context ([`Self::note_context`]). `false` (main
    /// screen or an unwired host) leaves the re-anchor classifier unchanged.
    ctx_alt: bool,
    /// Focused pane columns in this engine's composed/window coordinate space:
    /// `(first_col, width)`. Native hosts stamp it beside [`Self::note_context`]
    /// before every tick. `None` keeps direct/legacy embedders conservative:
    /// without physical-margin authority, a large one-row move remains a dark
    /// repaint re-anchor rather than guessing a fold.
    pane_columns: Option<(u16, u16)>,
    /// Hidden→visible relocations the hide bridge admitted or declined, for
    /// the engine's life — see [`BridgeTally`]. Never reset.
    bridge_tally: BridgeTally,
    /// The resident cell→`out`-slot probe table behind [`Self::tick`]'s overlap
    /// merge: allocated on the first lit tick (idle windows never pay the
    /// 16 KiB), then reused. Slots are validated by `dedup_epoch`.
    dedup: Vec<DedupSlot>,
    /// The tick stamp that validates `dedup` slots; a slot with a different
    /// epoch reads as empty. Bumped once per lit tick. `u64` cannot wrap in a
    /// process lifetime, and starting at 1 means a zeroed table reads empty.
    dedup_epoch: u64,
    /// Resident bounded Bresenham scratch used by [`Self::spawn`]. Walking
    /// backward from the destination keeps both work and retained capacity at
    /// O(`max_len`) even for a hostile full-u16 coordinate jump, and avoids a
    /// heap allocation on every ordinary typed move after warm-up.
    spawn_path: Vec<(i32, i32)>,
}

impl CursorTrail {
    /// Hard cap on live sparks (defends against a pathological flood of moves):
    /// far more than a few hundred ms of typing or one capped jump can produce.
    const MAX_SPARKS: usize = 512;
    /// Slot count of the dedup probe table: a power of two at 2 ×
    /// [`Self::MAX_SPARKS`], so even a saturated tick (512 distinct cells)
    /// keeps load ≤ 1/2 and probes short.
    const DEDUP_SLOTS: usize = 1024;
    /// A typed-echo key-hint stays armed this long for its paired move
    /// (seconds) — mirrors `CursorGlow::TYPE_HINT_FRESH`.
    const TYPE_HINT_FRESH: f32 = 0.25;
    /// A repaint-blink hint stays fresh this long (seconds) — mirrors
    /// `CursorGlow::BLINK_HINT_FRESH` (the blink is noted at the PREVIOUS
    /// burst's present, so it must outlive one human inter-key gap).
    const BLINK_HINT_FRESH: f32 = 0.5;
    /// A navigation key-hint stays armed this long for its paired move
    /// (seconds) — mirrors `CursorGlow::NAV_HINT_FRESH`.
    const NAV_HINT_FRESH: f32 = 0.25;
    /// Generic-movement classifier window (seconds). Admission still requires
    /// a matching correlated candidate.
    const MOVE_HINT_FRESH: f32 = 0.25;

    /// Last visible source owned by this engine. Hosts compare it with
    /// CursorGlow's before arming a candidate; an unseeded or reset pair fails
    /// closed.
    #[must_use]
    #[cfg(test)]
    pub fn bridge_tally(&self) -> BridgeTally {
        self.bridge_tally
    }

    #[must_use]
    pub fn cursor_anchor(&self) -> Option<(u16, u16)> {
        self.last
            .or_else(|| self.last_visible.map(|(cell, _)| cell))
    }

    /// Host key hint: one plain typed-glyph or Backspace echo keypress (never
    /// Enter/Tab/nav/modified chords). Arms only the [`Self::type_hint`]
    /// re-anchor classifier; it cannot admit a comet.
    ///
    /// A typed press closes nothing here: the nav and generic-move one-shots
    /// survive it, since their echoes are still owed (a glyph typed before an
    /// Enter's response lands is type-ahead). What the glow's
    /// `supersede_typed_press` closes lives in this engine's single
    /// [`Self::move_hint`] slot, so there is nothing to mirror here.
    pub fn note_typed(&mut self, now: Instant) {
        self.type_hint.stamp(now);
    }

    /// Host coalesced typing: an embedder may batch several key events between
    /// two render callbacks. One or two cells fit the typed classifier; a
    /// larger batch uses the generic morphology so three typed cells are not
    /// mistaken for a repaint teleport.
    pub fn note_typed_batch(&mut self, now: Instant, keys: u32) {
        if keys == 0 {
            return;
        }
        if keys <= u32::from(HIDE_BRIDGE_MAX_DIST) {
            self.note_typed(now);
        } else {
            self.note_move(now);
        }
    }

    /// Host repaint blink (see `CursorGlow::note_repaint_blink`): the focused
    /// terminal's `repaint_blink_epoch` advanced (the app brackets redraws in
    /// DECTCEM-hide-inside-DEC-2026, as Claude Code does per keystroke).
    /// Feeds the alt-screen re-anchor classifier; not movement evidence.
    pub fn note_repaint_blink(&mut self, now: Instant) {
        self.blink_hint = Some(now);
    }

    /// Drop the blink hint: a tab/pane switch pointed this window at a
    /// different terminal.
    pub fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// Host per-frame context (see `CursorGlow::note_context`): whether the
    /// probed terminal is on the alt screen this frame.
    pub fn note_context(&mut self, alt: bool) {
        self.ctx_alt = alt;
    }

    /// Stamp the focused pane's exact columns in the coordinate space handed
    /// to [`Self::tick`]. This changes morphology only; the normal fresh key
    /// license remains mandatory at [`Self::spawn`].
    pub fn note_pane_columns(&mut self, first_col: u16, cols: usize) {
        self.pane_columns = u16::try_from(cols)
            .ok()
            .filter(|cols| *cols > 0)
            .map(|cols| (first_col, cols));
    }

    /// Disarm dangling movement state (see `CursorGlow::clear_typed`): a newer
    /// key with its own semantics (plain Enter, nav, kills, Tab) supersedes a
    /// swallowed typed or generic move.
    pub fn clear_typed(&mut self) {
        self.type_hint.clear();
        self.nav_hint = None;
        self.move_hint = None;
    }

    /// Arm the navigation veto — see `CursorGlow::note_navigation`. It licenses
    /// the following move (a key was pressed) and classifies it as scrubbing,
    /// which the comet deliberately does not paint.
    pub fn note_navigation(&mut self, now: Instant) {
        self.clear_typed();
        self.nav_hint = Some(now);
    }

    /// Arm one generic movement LICENSE, superseding any swallowed
    /// typed/navigation classifier.
    fn note_move(&mut self, now: Instant) {
        self.clear_typed();
        self.move_hint = Some(now);
    }

    /// Classify a Tab completion or non-empty paste, in lockstep with
    /// `CursorGlow::note_user_gesture`: banked typed stamps are real keys whose
    /// echoes are in flight, so the bank is kept. It closes the nav class
    /// (`a_user_gesture_establishes_one_class_and_licenses_its_landing`), where
    /// the glow keeps its nav one-shot; the two differ only for an arrow whose
    /// hop is still in flight when Tab is pressed.
    pub fn note_user_gesture(&mut self, now: Instant) {
        self.nav_hint = None;
        self.move_hint = Some(now);
    }

    /// Classify a plain Return, bank-preserving like
    /// [`Self::note_user_gesture`], in lockstep with `CursorGlow::note_return`.
    pub fn note_return(&mut self, now: Instant) {
        self.move_hint = Some(now);
    }

    /// Classify a settled user resize/re-grid.
    #[cfg(test)]
    pub fn note_reflow(&mut self, now: Instant) {
        self.note_move(now);
    }

    /// Explicit test/preview driver for a deliberately scripted move.
    #[doc(hidden)]
    pub fn note_synthetic_move(&mut self, now: Instant) {
        self.note_move(now);
    }

    /// Scripted typed-move twin for previews, examples and benchmarks that
    /// deliberately exercise the typing classifier without a live terminal.
    #[doc(hidden)]
    pub fn note_synthetic_typed(&mut self, now: Instant) {
        self.note_typed(now);
    }

    /// Delivery re-stamp for every class using this engine's generic move slot
    /// (a queued Enter's [`Self::note_return`], a queued Tab's or ⌃V's
    /// [`Self::note_user_gesture`]): the licence revoked at enqueue is put back
    /// at the writer's completed write (`CursorGlow::note_delivered`'s twin).
    pub fn note_move_delivered(&mut self, at: Instant) {
        self.move_hint = Some(at);
    }

    /// The nav-slot twin of [`Self::note_move_delivered`]: a queued
    /// navigation key's licence, put back at delivery.
    pub fn note_motion_delivered(&mut self, at: Instant) {
        self.nav_hint = Some(at);
    }

    /// Cancel only the license written by one not-yet-egressed dispatch. See
    /// `CursorGlow::revoke_input_hints_at` for the host contract.
    pub fn revoke_input_hints_at(&mut self, at: Instant) {
        self.type_hint.revoke_at(at);
        for hint in [&mut self.nav_hint, &mut self.move_hint] {
            if *hint == Some(at) {
                *hint = None;
            }
        }
    }

    /// PTY scroll translation (see `CursorGlow::note_scroll`): every live
    /// terminal-coordinate member, classifier anchors and visible cells alike,
    /// moves up `rows`. Anything pushed above row 0 is retired rather than
    /// clamped there.
    pub fn note_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.remap_rows(|row| row.checked_sub(rows));
    }

    /// Row-band translation, the twin of `CursorGlow::note_band_move` (see
    /// `crate::cursor_glow::band_row`): members on rows `top..=bottom` move by
    /// `delta`, members outside stay, and one carried past the band edge is
    /// retired rather than clamped. Both engines must agree on the replay.
    pub fn note_band_move(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.remap_rows(|row| band_row(row, top, bottom, delta));
    }

    /// Move every live terminal-coordinate member — the classifier anchors
    /// and the visible sparks — through `map`; `None` retires the member.
    fn remap_rows(&mut self, map: impl Fn(u16) -> Option<u16>) {
        self.last = self
            .last
            .and_then(|(row, col)| map(row).map(|row| (row, col)));
        self.last_visible = self
            .last_visible
            .and_then(|((row, col), at)| map(row).map(|row| ((row, col), at)));
        self.sparks.retain_mut(|spark| match map(spark.row) {
            Some(row) => {
                spark.row = row;
                true
            }
            None => false,
        });
    }

    /// Whether any spark is still alive (so the host should keep the animation
    /// timer armed). Cheap; consulted every `about_to_wait`.
    pub fn is_active(&self) -> bool {
        !self.sparks.is_empty()
    }

    /// Drop the in-flight comet and forget the last cursor position (twin of
    /// [`crate::cursor_glow::CursorGlow::reset`]), used when the cursor's
    /// coordinate space changes.
    pub fn reset(&mut self) {
        self.sparks.clear();
        self.last = None;
        self.last_visible = None;
        self.clear_typed();
        self.blink_hint = None;
        self.ctx_alt = false;
        self.pane_columns = None;
    }

    /// Advance the trail one frame: observe the cursor at `cur` (`None` when
    /// hidden), spawn a comet only when a one-shot movement candidate admits
    /// the delta, decay expired sparks, write the current trail cells (terminal
    /// coords) into `out`, and return their fingerprint (`0` when idle; it
    /// changes on every visible change).
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &TrailConfig,
        out: &mut Vec<TrailCell>,
    ) -> u64 {
        out.clear();
        if !cfg.enabled {
            // Disabled: keep tracking the cursor (so enabling it mid-session does
            // not spawn one giant spurious comet) but draw nothing.
            self.sparks.clear();
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.clear_typed();
            self.blink_hint = None;
            return 0;
        }

        // Source for a move between two visible positions, bridging ConPTY's
        // per-echo hide: a cursor visible within HIDE_BRIDGE_MS that reappears
        // at a plausible typing distance may start from the last visible cell.
        // The bridge never substitutes for the license gate in `spawn`.
        let spawn_from = self.last.or_else(|| {
            let ((r, c), seen) = self.last_visible?;
            let fresh = now.saturating_duration_since(seen).as_millis() as u64 <= HIDE_BRIDGE_MS;
            // A fresh typed hint (peeked; `spawn` owns consumption) widens the
            // source reach for batched echoes; unhinted moves keep the classic law.
            let typed_fresh = self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH);
            // A fresh navigation hint outranks the hide window (lockstep with
            // `CursorGlow::tick`): the landing is the key's echo however long the
            // caret was hidden. The reach is the focused pane's width and a wrapped
            // box's height, never the grid. `spawn` still owns and consumes the
            // license and refuses a stale one, and `nav_bridge_source_ok` bounds
            // the source on both sides of the press.
            let nav_bridge = hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
                && self
                    .nav_hint
                    .is_some_and(|key| nav_bridge_admits(key, seen, now));
            let reach = hide_bridge_reach(typed_fresh, nav_bridge, self.pane_columns);
            let plausible = cur.is_some_and(|to| reach.admits((r, c), to));
            ((fresh || nav_bridge) && plausible).then_some((r, c))
        });
        let hidden_relocation = self.last.is_none()
            && cur
                .zip(self.last_visible.map(|(cell, _)| cell))
                .is_some_and(|(current, previous)| current != previous);
        let declined_hidden_relocation = spawn_from.is_none() && hidden_relocation;
        if hidden_relocation {
            if spawn_from.is_some() {
                self.bridge_tally.bridged += 1;
            } else {
                self.bridge_tally.declined += 1;
            }
        }
        let completed_hidden_reappearance =
            self.last.is_none() && cur.is_some() && self.last_visible.is_some();
        let unseeded_visible = self.last.is_none() && self.last_visible.is_none() && cur.is_some();
        if let (Some((pr, pc)), Some((cr, cc))) = (spawn_from, cur)
            && (pr != cr || pc != cc)
        {
            self.spawn(pr, pc, cr, cc, now, cfg);
        } else if declined_hidden_relocation {
            // A far or stale hidden→visible relocation declined the bridge, so
            // `spawn` never consumed its one-shot license. Retire it before the
            // new cell becomes `last`, or the next unrelated program move could
            // borrow it. The resident bed is untouched: earned light is never
            // destroyed by someone else's output.
            self.clear_typed();
        } else if completed_hidden_reappearance {
            // A visible→hidden→visible cycle returning to the same cell still
            // completes one-shot input state: consume the hints dark so the next
            // unrelated delta cannot borrow them. Except a fresh nav hint (lockstep
            // with `CursorGlow::retire_hidden_movement_provenance`): its echo may
            // not have landed yet (a scrolled viewport snapping back, the hidden
            // half of an Ink repaint). A stale nav hint retires.
            let fresh_nav = self.nav_hint.filter(|t| {
                now.saturating_duration_since(*t).as_secs_f32() <= Self::NAV_HINT_FRESH
            });
            self.clear_typed();
            self.nav_hint = fresh_nav;
        } else if unseeded_visible {
            // A fresh or reset engine has no source cell for this landing. Seed
            // the visible anchor and spend every one-shot license now.
            self.clear_typed();
        }
        self.last = cur;
        if let Some(c) = cur {
            self.last_visible = Some((c, now));
        }

        // Decay and emission in one ordered walk: drop sparks past their own
        // baked lifetime, and resolve survivors' coverage into `out`, merging
        // overlapping cells (keep the brightest) via one O(1) probe of the
        // epoch-stamped table. Up to MAX_SPARKS = 512 live sparks can occupy
        // hundreds of distinct cells, so a linear rescan would be quadratic.
        // A first-seen cell claims its slot at first touch and later overlaps
        // max() in place, so `out`'s contents and order (and the fingerprint)
        // match a linear scan (`dedup_index_matches_the_linear_scan_byte_for_byte`).
        if !self.sparks.is_empty() {
            if self.dedup.is_empty() {
                // First lit tick of this window: size the table once. Idle and
                // disabled windows never reach this (their sparks stay empty).
                self.dedup = vec![DedupSlot::default(); Self::DEDUP_SLOTS];
            }
            // One counter bump invalidates every slot at once — the O(1)
            // whole-table "clear" that replaces walking 1024 slots per frame.
            self.dedup_epoch += 1;
        }
        let epoch = self.dedup_epoch;
        let dedup = &mut self.dedup;
        self.sparks.retain(|s| {
            let age_ms = now.saturating_duration_since(s.born).as_millis() as u64;
            if age_ms >= s.life_ms {
                return false;
            }
            // time fade in 0..=255 (linear), then scale the birth coverage.
            let tf = ((s.life_ms - age_ms) * 255 / s.life_ms) as u32;
            let alpha = ((s.born_alpha as u32 * tf) / 255) as u8;
            if alpha == 0 {
                return true;
            }
            let key = (u32::from(s.row) << 16) | u32::from(s.col);
            // Fibonacci-hash the packed cell (top log2(DEDUP_SLOTS) bits of the
            // golden-ratio product), then linear-probe. At most 512 keys are
            // claimed per epoch against 1024 slots, so an empty slot is always
            // reached before a wrap.
            let mask = Self::DEDUP_SLOTS - 1;
            let shift = 32 - Self::DEDUP_SLOTS.trailing_zeros();
            let mut i = (key.wrapping_mul(0x9E37_79B9) >> shift) as usize;
            loop {
                let slot = &mut dedup[i];
                if slot.epoch != epoch {
                    // First touch this tick: claim the slot and push (first
                    // spark wins the position).
                    *slot = DedupSlot {
                        epoch,
                        key,
                        out_idx: out.len() as u32,
                    };
                    out.push(TrailCell {
                        row: s.row as usize,
                        col: s.col as usize,
                        alpha,
                    });
                    break;
                }
                if slot.key == key {
                    // Already emitted this tick: merge exactly as before —
                    // the brightest overlapping spark wins.
                    let cell = &mut out[slot.out_idx as usize];
                    cell.alpha = cell.alpha.max(alpha);
                    break;
                }
                i = (i + 1) & mask;
            }
            true
        });

        // Include the resolved colour: `ignite` re-heats it from live cadence
        // each frame, so colour can change while cells are identical. The shared
        // helper is also the host's presentation-opacity seam.
        trail_presentation_fingerprint(out, cfg.color)
    }

    /// The license, twin of `CursorGlow::move_licensed`: did a human touch the
    /// keyboard just now? See `docs/design/EFFECTS-LICENSE-REDESIGN.md`.
    ///
    /// This engine keeps the glow's hint set under three stamps, so both
    /// license exactly the same moves:
    /// * `type_hint`: typed-glyph and Backspace echoes (glow `type_hint` +
    ///   `quench_hint`);
    /// * `nav_hint`: navigation/motion;
    /// * `move_hint`: Return, Tab/paste, reflow and scripted preview notes
    ///   (glow `return_hint` + `newline_hint` + `user_gesture_hint` +
    ///   `synthetic_note_pending`).
    ///
    /// Peeks, never consumes: `spawn` owns consumption (one hint, one echo).
    #[must_use]
    pub fn move_licensed(&self, now: Instant) -> bool {
        self.type_hint.any_fresh(now, Self::TYPE_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.move_hint, now, Self::MOVE_HINT_FRESH)
    }

    /// Try to spawn a comet for a licensed move from `(pr,pc)` to `(cr,cc)`:
    /// the cells on the straight line from origin toward the destination,
    /// excluding the destination (the live cursor draws there). On a long jump
    /// only the `max_len` cells nearest the destination survive.
    fn spawn(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &TrailConfig,
    ) -> bool {
        // The license seam (see [`Self::move_licensed`]), as in the glow: an
        // unlicensed move mints nothing and clears nothing. Earned light leaves
        // by decay, scroll translation or reset, never by someone else's output.
        if !self.move_licensed(now) {
            return false;
        }
        // Typed re-anchor (twin of the `CursorGlow` wrap classifier): a fresh
        // typed/backspace hint paired with a one-row move beyond the typed
        // advance is a TUI repaint re-anchoring its caret (Ink rewraps its input
        // box per keystroke), so lay no comet and return. `dr_abs <= 1` covers
        // wrap-down, backspace wrap-back and box growth (a bottom-anchored box
        // growing up keeps the caret's row, dr == 0 with a large column delta);
        // `raw_dist > 2` keeps every hide-bridgeable move unchanged. The hint is
        // consumed on any paired move.
        let dr_abs = (cr as i32 - pr as i32).abs();
        let raw_dist = dr_abs.max((cc as i32 - pc as i32).abs());
        let physical_fold = self.pane_columns.is_some_and(|(col0, cols)| {
            crate::trail_sweep::physical_margin_fold((pr, pc), (cr, cc), col0, cols)
        });
        // A one-shot's own move is never the glyph's (lockstep with
        // `CursorGlow`'s `return_paired`): an Enter's late response can arrive
        // beside a type-ahead glyph's stamp. Any shape but a same-row forward
        // hop is the licence's own move: it takes the licence and leaves the
        // stamp for the glyph's echo.
        let move_paired =
            !(cr == pr && cc > pc) && hint_fresh(self.move_hint, now, Self::MOVE_HINT_FRESH);
        let paired = !move_paired
            && self
                .type_hint
                .take_fresh(now, Self::TYPE_HINT_FRESH)
                .is_some();
        // Alt-screen blink requirement (glow twin): on the alt screen only a
        // repaint-blinking app re-anchors; vim's one-row motions keep their
        // comet.
        let blink_fresh = hint_fresh(self.blink_hint, now, Self::BLINK_HINT_FRESH);
        let nav_paired = take_hint_fresh(&mut self.nav_hint, now, Self::NAV_HINT_FRESH).is_some();
        // The generic move license is consumed here too — one stamp, one echo.
        let _ = self.move_hint.take();
        // Navigation (Ctrl-A/E, Home/End, arrows, word/line motions): a move
        // paired with a fresh nav hint is scrubbing, so lay no comet, matching
        // `CursorGlow`. The live cursor draws itself, and the resident bed
        // survives (a scrub mints and clears nothing).
        if nav_paired {
            return false;
        }
        if paired && !physical_fold && dr_abs <= 1 && raw_dist > 2 && (!self.ctx_alt || blink_fresh)
        {
            return false;
        }
        // Keep at most `max_len` cells NEAREST the destination (the path's tail).
        let max_len = cfg.max_len.max(1);
        self.spawn_path.clear();
        if physical_fold {
            // The glyph materialized at the departure margin cell; light only
            // that cell, never a diagonal through columns the caret did not
            // visit.
            self.spawn_path.push((i32::from(pr), i32::from(pc)));
        } else {
            crate::trail_sweep::line_cells_tail(
                &mut self.spawn_path,
                (i32::from(pr), i32::from(pc)),
                (i32::from(cr), i32::from(cc)),
                max_len,
                false,
            );
        }
        if self.spawn_path.is_empty() {
            return false;
        }
        // Ignition: the cadence intensity (baked at spawn) scales head and tail
        // coverage between their gentle and ignited endpoints and stretches the
        // lifetime. Warmth stretches lifetime only, so human-cadence typing
        // chains the bed while a lone keystroke keeps the plain fade.
        let q = cfg.intensity.clamp(0.0, 1.0);
        let head = lerp_u32(HEAD_ALPHA_GENTLE, HEAD_ALPHA_IGNITED, q);
        let tail = lerp_u32(TAIL_ALPHA_GENTLE, TAIL_ALPHA_IGNITED, q);
        let base_dur = cfg.duration.as_millis().max(1) as f32;
        let life_ms = (base_dur
            * (1.0 + CHAIN_LIFE_MULT * cfg.warmth.clamp(0.0, 1.0) + (IGNITED_LIFE_MULT - 1.0) * q))
            .max(1.0) as u64;
        let n = self.spawn_path.len() as u32;
        // index 0 = farthest from the cursor (faintest), n-1 = nearest (brightest).
        for (i, (r, c)) in self.spawn_path.iter().copied().enumerate() {
            if r < 0 || c < 0 {
                continue;
            }
            let frac = (i as u32 + 1) * 255 / n; // 1/n .. 1, scaled to 0..=255
            // Readability guard: clamp birth coverage to the readable ceiling.
            let born_alpha =
                (tail + (head - tail) * frac / 255).min(READABLE_ALPHA_CAP as u32) as u8;
            self.sparks.push(Spark {
                row: r as u16,
                col: c as u16,
                born_alpha,
                born: now,
                life_ms,
            });
        }
        // Bound total live sparks (drop the oldest beyond the cap).
        if self.sparks.len() > Self::MAX_SPARKS {
            let drop = self.sparks.len() - Self::MAX_SPARKS;
            self.sparks.drain(0..drop);
        }
        true
    }
}

// ===========================================================================
// Typing-cadence ignition
// ===========================================================================
//
// The comet ignites only when typing is fast and sustained. Each keystroke adds
// a fixed quantum of heat, which decays with a fixed half-life between keys:
// fast typing compounds toward a hot steady state, slow typing never climbs. A
// knee mapping ([`ignition_intensity`]) turns heat into the `0.0..=1.0`
// intensity, with a high lower knee so a few characters stay gentle. Clockless
// (injected `now`), so it is unit-testable without sleeping.

/// Tunables for the typing-cadence heat → ignition mapping. The [`Default`] is
/// what the host uses: fast sustained typing ignites in ~8-10 keys, a few keys
/// stay calm, a pause cools in ~½ s.
#[derive(Clone, Copy, Debug)]
pub struct CadenceParams {
    /// Heat added by one keystroke (arbitrary units).
    pub gain: f32,
    /// Heat half-life: the time for accumulated heat to halve with no typing.
    pub half_life: Duration,
    /// Heat where ignition BEGINS to ramp. Below it → intensity 0 (a few keys /
    /// slow typing live here), so the effect stays gentle.
    pub knee_lo: f32,
    /// Heat where ignition is FULL (intensity 1). Fast, sustained typing reaches it.
    pub knee_hi: f32,
}

impl Default for CadenceParams {
    fn default() -> Self {
        Self {
            gain: 1.0,
            half_life: Duration::from_millis(220),
            knee_lo: 2.0,
            knee_hi: 6.5,
        }
    }
}

/// Decay `heat` forward by `dt` under a `half_life` (exponential). Pure + total;
/// `dt == 0` returns `heat` unchanged, and a non-positive half-life is floored so
/// the power is finite.
fn decay_heat(heat: f32, dt: Duration, half_life: Duration) -> f32 {
    let hl = half_life.as_secs_f32().max(1e-3);
    heat * 0.5f32.powf(dt.as_secs_f32() / hl)
}

/// The PURE cadence→intensity mapping the whole feature turns on: a smoothstep of
/// accumulated `heat` between the two knees, clamped to `0.0..=1.0`. Below `knee_lo`
/// → `0.0` (gentle: a few keys never ignite); above `knee_hi` → `1.0` (ignited:
/// fast sustained typing); a smooth S-curve between. Total for all finite inputs.
#[must_use]
pub fn ignition_intensity(heat: f32, knee_lo: f32, knee_hi: f32) -> f32 {
    // A positive valid-interval test: NaN, inverted or equal knees fall to the
    // `else` and step at the single threshold rather than dividing by zero.
    if knee_hi > knee_lo {
        let t = ((heat - knee_lo) / (knee_hi - knee_lo)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    } else if heat >= knee_hi {
        1.0
    } else {
        0.0
    }
}

/// Typing-cadence tracker → comet ignition intensity. Clockless: both
/// [`Self::on_keystroke`] and [`Self::intensity`] take an injected `now`. The
/// host keeps one per window and reads [`Self::intensity`] each frame.
///
/// scope-waiver: heat is an aesthetic intensity, not a safety budget; a second
/// tracker costs a second lazily decayed `f32` and a differently lit comet.
/// (The ignition limiter that is a budget lives in `word_decorations`, under
/// the `flash-limiter` scope claim.)
#[derive(Clone, Debug)]
pub struct TypingCadence {
    params: CadenceParams,
    /// Accumulated heat as of `last` (decayed lazily on the next read/keystroke).
    heat: f32,
    /// When `heat` was last updated. `None` until the first keystroke.
    last: Option<Instant>,
}

impl Default for TypingCadence {
    fn default() -> Self {
        Self::new(CadenceParams::default())
    }
}

impl TypingCadence {
    /// A cold tracker (no heat) with the given tunables.
    #[must_use]
    pub fn new(params: CadenceParams) -> Self {
        Self {
            params,
            heat: 0.0,
            last: None,
        }
    }

    /// Register one keystroke at `now`: decay the standing heat to `now`, then add
    /// one `gain` quantum (capped at `knee_hi` so a long hold saturates cleanly and
    /// decay from the top reaches the gentle band predictably).
    pub fn on_keystroke(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let dt = now.saturating_duration_since(last);
            self.heat = decay_heat(self.heat, dt, self.params.half_life);
        }
        self.heat = (self.heat + self.params.gain).min(self.params.knee_hi);
        self.last = Some(now);
    }

    /// The standing heat decayed forward to `now`, the one lazy read every
    /// accessor derives from, so [`Self::sample`] pays the decay (`powf`) once
    /// for both channels while [`Self::intensity`] / [`Self::warmth`] stay
    /// bit-identical.
    fn heat_at(&self, now: Instant) -> f32 {
        match self.last {
            Some(last) => {
                let dt = now.saturating_duration_since(last);
                decay_heat(self.heat, dt, self.params.half_life)
            }
            None => 0.0,
        }
    }

    /// The heat→warmth mapping shared by [`Self::warmth`] and [`Self::sample`];
    /// see [`Self::warmth`].
    fn warmth_of(&self, heat: f32) -> f32 {
        let span = (self.params.knee_lo - self.params.gain).max(f32::EPSILON);
        ((heat - self.params.gain) / span).clamp(0.0, 1.0)
    }

    /// The ignition intensity `0.0..=1.0` as of `now` — the heat decayed forward to
    /// `now`, then run through [`ignition_intensity`]. Non-mutating (safe to call
    /// every frame): a long idle simply reads as a fully-decayed, gentle `0.0`.
    #[must_use]
    pub fn intensity(&self, now: Instant) -> f32 {
        ignition_intensity(self.heat_at(now), self.params.knee_lo, self.params.knee_hi)
    }

    /// Sub-ignition warmth `0.0..=1.0` as of `now`: `0` at or below one key's
    /// worth of heat (so a lone keystroke contributes nothing), ramping to `1`
    /// at the ignition knee. Continuity fuel, not brightness: the host feeds it
    /// into trail lifetime only (see [`ignite`] / `TrailConfig::warmth`), so
    /// human-cadence typing chains into a continuous bed while colour and alpha
    /// stay gentle until real ignition. Non-mutating.
    #[must_use]
    pub fn warmth(&self, now: Instant) -> f32 {
        self.warmth_of(self.heat_at(now))
    }

    /// Both per-frame channels from one lazy decay: `(intensity, warmth)`,
    /// bit-for-bit what the two accessors return for the same `now`, with one
    /// `powf` instead of two.
    #[must_use]
    pub fn sample(&self, now: Instant) -> (f32, f32) {
        let heat = self.heat_at(now);
        (
            ignition_intensity(heat, self.params.knee_lo, self.params.knee_hi),
            self.warmth_of(heat),
        )
    }
}

/// Blend `base` toward the comet-core hot tint by `q` (`0` = base, `1` = the
/// max blend): a warm white-amber, partial so the trail keeps the theme's
/// identity and never becomes pure white.
fn heat_color(base: u32, q: f32) -> u32 {
    /// The comet-core tint an ignited trail heats toward (warm white-amber).
    const HOT: u32 = 0x00FF_E6B0;
    /// Even at full ignition the colour is only this far toward [`HOT`] — tasteful,
    /// and it keeps enough of the base hue that the wake still "matches the theme".
    const HEAT_COLOR_MAX: f32 = 0.5;
    let t = (q.clamp(0.0, 1.0) * HEAT_COLOR_MAX).clamp(0.0, 1.0);
    let lerp = |sh: u32| {
        let a = ((base >> sh) & 0xff) as f32;
        let b = ((HOT >> sh) & 0xff) as f32;
        ((a + (b - a) * t).round() as u32).min(255)
    };
    (lerp(16) << 16) | (lerp(8) << 8) | lerp(0)
}

/// Stamp the current typing-cadence `intensity` and sub-ignition `warmth` onto a
/// resolved [`TrailConfig`] and heat its colour toward the comet core. The host
/// calls this each frame just before the trail tick with
/// [`TypingCadence::intensity`]/[`TypingCadence::warmth`]: fast sustained typing
/// ignites the comet, while normal cadence only chains spark lifetimes. Pure
/// and total; intensity `0.0` leaves the colour untouched, warmth `0.0` leaves
/// the base life.
pub fn ignite(cfg: &mut TrailConfig, intensity: f32, warmth: f32) {
    let q = intensity.clamp(0.0, 1.0);
    cfg.intensity = q;
    cfg.warmth = warmth.clamp(0.0, 1.0);
    cfg.color = heat_color(cfg.color, q);
}

/// Integer cell-line (Bresenham) from `(r0,c0)` to `(r1,c1)` INCLUSIVE of both
/// endpoints — the cells a straight cursor move sweeps through. Pure + total.
#[cfg(test)]
fn line_cells(r0: i32, c0: i32, r1: i32, c1: i32) -> Vec<(i32, i32)> {
    let (dr, dc) = ((r1 - r0).abs(), (c1 - c0).abs());
    let (sr, sc) = (if r0 < r1 { 1 } else { -1 }, if c0 < c1 { 1 } else { -1 });
    let mut err = dc - dr;
    let (mut r, mut c) = (r0, c0);
    let mut out = Vec::with_capacity((dr.max(dc) + 1) as usize);
    loop {
        out.push((r, c));
        if r == r1 && c == c1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dr {
            err -= dr;
            c += sc;
        }
        if e2 < dc {
            err += dc;
            r += sr;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool) -> TrailConfig {
        TrailConfig {
            enabled,
            duration: Duration::from_millis(200),
            max_len: 8,
            color: 0x0050_FA7B,
            intensity: 0.0,
            warmth: 0.0,
        }
    }

    #[test]
    fn no_trail_until_a_move() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        // First observation establishes `last`; no move yet → nothing.
        assert_eq!(t.tick(Some((0, 0)), now, &cfg(true), &mut out), 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    #[test]
    fn presentation_opacity_attenuates_resident_cells_and_fingerprints_exact_output() {
        let now = Instant::now();
        let mut trail = CursorTrail::default();
        trail.sparks.push(Spark {
            row: 3,
            col: 7,
            born_alpha: 200,
            born: now,
            life_ms: 1_000,
        });
        let config = cfg(true);
        let mut full = Vec::new();
        let full_fp = trail.tick(None, now, &config, &mut full);
        assert_eq!(
            full.as_slice(),
            &[TrailCell {
                row: 3,
                col: 7,
                alpha: 200
            }]
        );
        assert_ne!(full_fp, 0, "negative control: a resident cell is emitted");

        for (opacity, expected_alpha) in [(1.0, 200), (0.75, 150), (0.5, 100), (0.25, 50)] {
            let mut presented = full.clone();
            let fp = project_trail_presentation(&mut presented, config.color, opacity);
            assert_eq!(
                presented.as_slice(),
                &[TrailCell {
                    row: 3,
                    col: 7,
                    alpha: expected_alpha,
                }],
                "the envelope must scale already-resident output at {opacity}"
            );
            assert_ne!(fp, 0);
            assert_eq!(
                fp,
                trail_presentation_fingerprint(&presented, config.color),
                "the RepaintKey witness describes the attenuated bytes"
            );
            assert_eq!(
                fp == full_fp,
                opacity == 1.0,
                "only the identity projection keeps the original fingerprint"
            );
        }

        let mut hidden = full.clone();
        assert_eq!(
            project_trail_presentation(&mut hidden, config.color, 0.0),
            0
        );
        assert!(hidden.is_empty());
        assert!(
            trail.is_active(),
            "presentation shedding must not rewrite resident engine state"
        );

        let mut non_finite = full;
        assert_eq!(
            project_trail_presentation(&mut non_finite, config.color, f32::NAN),
            0
        );
        assert!(non_finite.is_empty(), "non-finite opacity fails closed");

        let mut disabled = cfg(false);
        disabled.intensity = 1.0;
        let mut off = Vec::new();
        assert_eq!(trail.tick(None, now, &disabled, &mut off), 0);
        assert!(
            off.is_empty() && !trail.is_active(),
            "the accessibility/master hard-zero path still retires the comet"
        );
    }

    /// Band twin of the scroll test below, in lockstep with
    /// `CursorGlow::note_band_move`: a viewport `[8..20]` riding down one row
    /// carries the anchors and the comet cell in it by one row, leaves a cell
    /// outside the band alone, and drops the cell on the band's bottom row. A
    /// degenerate band is a no-op.
    #[test]
    fn a_band_move_translates_the_trail_inside_the_band_and_drops_what_leaves() {
        let now = Instant::now();
        let mut trail = CursorTrail {
            last: Some((10, 4)),
            last_visible: Some(((10, 4), now)),
            ..CursorTrail::default()
        };
        for row in [10u16, 1, 20] {
            trail.sparks.push(Spark {
                row,
                col: 3,
                born_alpha: 42,
                born: now,
                life_ms: 500,
            });
        }
        trail.note_band_move(8, 20, 1);
        assert_eq!(
            trail.last,
            Some((11, 4)),
            "classifier anchor rides its band"
        );
        assert_eq!(
            trail.last_visible.map(|(cell, _)| cell),
            Some((11, 4)),
            "hide-bridge anchor rides its band"
        );
        let rows: Vec<u16> = trail.sparks.iter().map(|s| s.row).collect();
        assert_eq!(
            rows,
            vec![11, 1],
            "inside moved, outside untouched, past the edge gone"
        );
        trail.note_band_move(0, 5, 0);
        trail.note_band_move(9, 8, -1);
        assert_eq!(trail.last, Some((11, 4)));
        trail.note_band_move(11, 11, -1);
        assert_eq!(
            trail.last, None,
            "an anchor carried out of its band has no honest cell"
        );
        assert_eq!(trail.sparks.len(), 1);
    }

    /// PTY scrolls carry the classic terminal-cell trail just like the glow
    /// engine's pixel/cell pools. A survivor moves by the exact row delta,
    /// while visible light and classifier anchors that cross the top retire;
    /// none may clamp to row 0 and later seed an unrelated move.
    #[test]
    fn scroll_translates_every_live_trail_cell_and_retires_off_top_state() {
        let now = Instant::now();
        let mut trail = CursorTrail {
            last: Some((10, 4)),
            last_visible: Some(((10, 4), now)),
            ..CursorTrail::default()
        };
        trail.sparks.push(Spark {
            row: 10,
            col: 3,
            born_alpha: 42,
            born: now,
            life_ms: 500,
        });
        trail.sparks.push(Spark {
            row: 1,
            col: 2,
            born_alpha: 42,
            born: now,
            life_ms: 500,
        });

        trail.note_scroll(2);
        assert_eq!(trail.last, Some((8, 4)), "classifier anchor follows");
        assert_eq!(
            trail.last_visible.map(|(cell, _)| cell),
            Some((8, 4)),
            "hide-bridge anchor follows"
        );
        assert_eq!(trail.sparks.len(), 1, "off-top light retires");
        assert_eq!(trail.sparks[0].row, 8, "surviving light follows exactly");

        // Tier 1: bind the genuine CursorTrail transition to the same derived
        // all-effect scroll decision used by CursorGlow. The concrete asserts
        // above cover every position-bearing field in this engine.
        let base_model = aterm_spec::derive::cursor_effect_scroll_model();
        let overrides = [("StartY", 10), ("Delta", 2)];
        let model = aterm_spec::interp::with_consts(&base_model, &overrides);
        let mut source = model.init_state();
        source.insert("survivor_y", 10);
        let mut projected = source.clone();
        projected.insert("survivor_y", i64::from(trail.sparks[0].row));
        projected.insert("off_top_alive", 0);
        // This engine owns no row-probe proof; absence projects to the
        // family-wide post-scroll cleared state.
        projected.insert("stale_proof_alive", 0);
        projected.insert("scrolled", 1);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &projected,
            Some("Scroll"),
            "CursorTrail all-family scroll translation",
        );
        assert!(admitted, "real trail scroll transition rejected: {why}");

        // Negative controls prove the bind observes both decisions rather than
        // accepting any post-state with the right action name.
        let mut stranded = projected.clone();
        stranded.insert("survivor_y", 10);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &stranded,
            Some("Scroll"),
            "CursorTrail stranded-scroll negative control",
        );
        assert!(!admitted, "an untranslated trail cell must be rejected");
        let mut retained = projected;
        retained.insert("off_top_alive", 1);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &overrides,
            &source,
            &retained,
            Some("Scroll"),
            "CursorTrail off-top-retention negative control",
        );
        assert!(!admitted, "an off-top trail cell must be rejected");

        trail.note_scroll(20);
        assert!(
            trail.sparks.is_empty(),
            "remaining trail scrolls off cleanly"
        );
        assert!(
            trail.last.is_none() && trail.last_visible.is_none(),
            "off-top anchors retire instead of forging a row-0 source"
        );
    }

    /// Paste/Tab supersedes swallowed typed/navigation classifiers, but its
    /// text-blind timestamp is not movement evidence. A scripted candidate
    /// proves the identical geometry is non-vacuously drawable.
    #[test]
    fn a_user_gesture_establishes_one_class_and_licenses_its_landing() {
        let now = Instant::now();
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        let c = cfg(true);
        trail.tick(Some((2, 20)), now, &c, &mut out);
        trail.note_typed(now);
        trail.note_navigation(now);

        trail.note_user_gesture(now);
        assert!(
            !trail.type_hint.armed() && trail.nav_hint.is_none(),
            "the gesture establishes one unambiguous generic class"
        );
        let moved = now + Duration::from_millis(10);
        assert_ne!(
            trail.tick(Some((3, 3)), moved, &c, &mut out),
            0,
            "a fresh paste/Tab stamp licenses the landing that follows it"
        );
        assert!(!out.is_empty());

        // …and the SAME landing with no gesture behind it is dark.
        let mut cold = CursorTrail::default();
        cold.tick(Some((2, 20)), now, &c, &mut out);
        assert_eq!(
            cold.tick(Some((3, 3)), moved, &c, &mut out),
            0,
            "unlicensed program motion mints nothing"
        );
        assert!(out.is_empty());

        // Tier 1: the licence stamp is one slot, so a gesture press disposes of
        // whatever classes were live (the model's `superseded` disposition);
        // both superseded classes must be gone.
        let base_model = aterm_spec::derive::cursor_hint_license_model();
        let model = aterm_spec::interp::with_consts(&base_model, &[]);
        let mut source = model.init_state();
        source.insert("hint", 1);
        source.insert("arms", 1);
        source.insert("credit_arms", 1);
        let mut projected = source.clone();
        projected.insert("hint", 1);
        projected.insert("arms", 2);
        projected.insert("credit_arms", 2);
        projected.insert(
            "superseded",
            i64::from(!trail.type_hint.armed() && trail.nav_hint.is_none()),
        );
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &[],
            &source,
            &projected,
            Some("PressArmsLicense"),
            "CursorTrail gesture-class supersession",
        );
        assert!(admitted, "real trail supersession rejected: {why}");

        let mut ambiguous = projected;
        ambiguous.insert("superseded", 0);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base_model,
            &[],
            &source,
            &ambiguous,
            Some("PressArmsLicense"),
            "CursorTrail stale-typed negative control",
        );
        assert!(!admitted, "a retained typed class must be rejected");

        // The same stale classes are observably harmful without supersession:
        // each one independently suppresses this large one-row move.
        for arm in [CursorTrail::note_typed, CursorTrail::note_navigation] {
            let mut stale = CursorTrail::default();
            stale.tick(Some((2, 20)), now, &c, &mut out);
            arm(&mut stale, now);
            assert_eq!(stale.tick(Some((3, 3)), moved, &c, &mut out), 0);
            assert!(out.is_empty(), "negative control must suppress the move");
        }
    }

    #[test]
    fn composed_physical_margin_fold_lights_only_departure_material() {
        let t0 = Instant::now();
        let at = t0 + Duration::from_millis(8);
        let origin = (4, 51);
        let target = (5, 12);
        let c = cfg(true);
        let mut out = Vec::new();

        let mut physical = CursorTrail::default();
        physical.note_pane_columns(12, 40);
        physical.tick(Some(origin), t0, &c, &mut out);
        physical.note_typed(at);
        assert_ne!(physical.tick(Some(target), at, &c, &mut out), 0);
        assert_eq!(
            out.iter()
                .map(|cell| (cell.row, cell.col))
                .collect::<Vec<_>>(),
            vec![(usize::from(origin.0), usize::from(origin.1))],
            "the fold lights its typed margin cell, never an interpolated diagonal"
        );

        let mut unwired = CursorTrail::default();
        unwired.tick(Some(origin), t0, &c, &mut out);
        unwired.note_typed(at);
        assert_eq!(unwired.tick(Some(target), at, &c, &mut out), 0);
        assert!(
            out.is_empty(),
            "without pane-bound authority the same large one-row move stays a dark re-anchor"
        );

        let mut wrong_pane = CursorTrail::default();
        wrong_pane.note_pane_columns(11, 40);
        wrong_pane.tick(Some(origin), t0, &c, &mut out);
        wrong_pane.note_typed(at);
        assert_eq!(wrong_pane.tick(Some(target), at, &c, &mut out), 0);
        assert!(
            out.is_empty(),
            "an adjacent pane origin cannot borrow the fold"
        );
    }

    /// Tier-1 provenance bind for the classic opaque comet. Program/TUI cursor
    /// deltas and timestamp-only input classes stay dark even while the
    /// configured colour/heat is warm; an explicit candidate admits exactly
    /// one move and is consumed whether fresh or stale.
    #[test]
    fn classic_trail_requires_one_shot_correlated_candidate() {
        let now = Instant::now();
        let mut hot = cfg(true);
        hot.intensity = 1.0;
        hot.warmth = 1.0;

        let mut cold = CursorTrail::default();
        assert!(
            !cold.spawn(0, 0, 0, 1, now, &hot),
            "warm configuration is amplitude, not user provenance"
        );
        assert!(cold.sparks.is_empty());

        // Every key class licenses the move that follows it. Navigation
        // licenses and then classifies itself dark: a scrub is not a comet.
        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_return,
            CursorTrail::note_reflow,
            CursorTrail::note_user_gesture,
        ] {
            let mut pressed = CursorTrail::default();
            arm(&mut pressed, now);
            assert!(
                pressed.spawn(0, 0, 0, 1, now + Duration::from_millis(1), &hot),
                "a fresh key stamp licenses its own echo"
            );
            assert!(!pressed.sparks.is_empty());
        }

        let mut witnessed = CursorTrail {
            last: Some((0, 0)),
            last_visible: Some(((0, 0), now)),
            ..CursorTrail::default()
        };
        witnessed.note_synthetic_move(now);
        assert!(witnessed.spawn(0, 0, 0, 1, now, &hot));
        assert!(!witnessed.sparks.is_empty());

        let mut one_shot = CursorTrail {
            last: Some((0, 0)),
            ..CursorTrail::default()
        };
        one_shot.note_synthetic_move(now);
        assert!(one_shot.spawn(0, 0, 0, 1, now, &hot));
        let earned: Vec<(u16, u16)> = one_shot.sparks.iter().map(|s| (s.row, s.col)).collect();
        assert!(
            !one_shot.spawn(0, 1, 0, 2, now + Duration::from_millis(1), &hot),
            "one candidate cannot fund a second program delta"
        );
        // The denied delta is unlicensed (the gesture stamp went with the first
        // move), so it mints and destroys nothing: the admitted comet keeps
        // every cell (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
        assert_eq!(
            one_shot
                .sparks
                .iter()
                .map(|s| (s.row, s.col))
                .collect::<Vec<_>>(),
            earned,
            "an unlicensed relocation neither adds to nor retires the earned comet bed"
        );

        let mut stale = CursorTrail {
            last: Some((0, 0)),
            ..CursorTrail::default()
        };
        stale.note_synthetic_move(now);
        assert!(
            !stale.spawn(0, 0, 0, 1, now + Duration::from_millis(251), &hot),
            "an expired stamp licenses nothing"
        );
        // The unlicensed return consumes nothing, and need not: every stamp
        // read is freshness-checked, so an expired one is inert. Prove the
        // inertness.
        assert!(
            !stale.spawn(0, 0, 0, 1, now + Duration::from_millis(400), &hot),
            "an expired stamp stays inert on every later delta"
        );
        assert!(stale.sparks.is_empty());

        let mut nav = CursorTrail::default();
        nav.note_navigation(now);
        assert!(
            !nav.spawn(0, 0, 0, 1, now, &hot),
            "a scrub is licensed but classified dark — the comet is a typing effect"
        );
        assert!(nav.sparks.is_empty());

        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_navigation,
            CursorTrail::note_return,
            CursorTrail::note_user_gesture,
        ] {
            let mut hidden = CursorTrail::default();
            let mut out = Vec::new();
            hidden.tick(Some((0, 0)), now, &hot, &mut out);
            hidden.tick(None, now + Duration::from_millis(1), &hot, &mut out);
            arm(&mut hidden, now + Duration::from_millis(2));
            hidden.tick(
                Some((0, 20)),
                now + Duration::from_millis(3),
                &hot,
                &mut out,
            );
            assert!(out.is_empty(), "far reappearance declines the bridge");
            assert!(
                !hidden.type_hint.armed()
                    && hidden.nav_hint.is_none()
                    && hidden.move_hint.is_none(),
                "declined hidden relocation consumes every movement class"
            );
            hidden.tick(
                Some((0, 21)),
                now + Duration::from_millis(4),
                &hot,
                &mut out,
            );
            assert!(
                out.is_empty() && !hidden.is_active(),
                "the next cold program move cannot borrow a declined hint"
            );
        }

        // Same-cell completion is a distinct no-spawn shape: the hide bridge
        // resolves back to its source, so neither `spawn` nor the decline arm
        // runs. Typed, Return and Tab/paste must still spend their one-shot.
        for arm in [
            CursorTrail::note_typed,
            CursorTrail::note_return,
            CursorTrail::note_user_gesture,
        ] {
            let mut hidden = CursorTrail::default();
            let mut out = Vec::new();
            hidden.tick(Some((2, 4)), now, &hot, &mut out);
            hidden.tick(None, now + Duration::from_millis(1), &hot, &mut out);
            arm(&mut hidden, now + Duration::from_millis(2));
            hidden.tick(Some((2, 4)), now + Duration::from_millis(3), &hot, &mut out);
            assert!(out.is_empty(), "same-cell reappearance paints nothing");
            assert!(
                !hidden.type_hint.armed()
                    && hidden.nav_hint.is_none()
                    && hidden.move_hint.is_none(),
                "same-cell hidden completion consumes its movement class"
            );
            hidden.tick(Some((2, 5)), now + Duration::from_millis(4), &hot, &mut out);
            assert!(
                out.is_empty() && !hidden.is_active(),
                "the next cold CUP move cannot borrow a same-cell hide hint"
            );
        }

        // A fresh or reset first draw has no source anchor; input or reflow may
        // have landed before it (tab switch, effect enable, synchronous resize),
        // so the seed tick consumes the one-shot dark.
        for reset_first in [false, true] {
            for arm in [
                CursorTrail::note_typed,
                CursorTrail::note_navigation,
                CursorTrail::note_return,
                CursorTrail::note_user_gesture,
                CursorTrail::note_reflow,
            ] {
                let mut unseeded = CursorTrail::default();
                let mut out = Vec::new();
                if reset_first {
                    unseeded.tick(Some((4, 4)), now, &hot, &mut out);
                    unseeded.reset();
                }
                arm(&mut unseeded, now + Duration::from_millis(1));
                unseeded.tick(
                    Some((4, 10)),
                    now + Duration::from_millis(2),
                    &hot,
                    &mut out,
                );
                assert!(out.is_empty(), "a source-less seed paints nothing");
                assert!(
                    !unseeded.type_hint.armed()
                        && unseeded.nav_hint.is_none()
                        && unseeded.move_hint.is_none(),
                    "a source-less seed consumes every movement class"
                );
                unseeded.tick(
                    Some((4, 11)),
                    now + Duration::from_millis(3),
                    &hot,
                    &mut out,
                );
                assert!(
                    out.is_empty() && !unseeded.is_active(),
                    "the next cold CUP cannot borrow a pre-seed class"
                );
            }
        }

        // Tier 1 against the licence model: a hidden→visible completion with no
        // `spawn` decision consumes every class without licensing anything
        // (`SwallowedKeyClearsLicense`); a retained stamp is the negative
        // control.
        let model = aterm_spec::derive::cursor_hint_license_model();
        let mut hidden_source = model.init_state();
        hidden_source.insert("hint", 1);
        hidden_source.insert("arms", 1);
        hidden_source.insert("credit_arms", 1);
        let mut hidden_projected = hidden_source.clone();
        hidden_projected.insert("hint", 0);
        hidden_projected.insert("cleared", 1);
        hidden_projected.insert("swallowed", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &hidden_source,
            &hidden_projected,
            Some("SwallowedKeyClearsLicense"),
            "CursorTrail declined hidden relocation",
        );
        assert!(ok, "real hidden-decline transition rejected: {why}");
        let mut hidden_sticky = hidden_projected.clone();
        hidden_sticky.insert("hint", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &hidden_source,
            &hidden_sticky,
            Some("SwallowedKeyClearsLicense"),
            "CursorTrail declined-hidden negative control",
        );
        assert!(!ok, "a declined hidden hint must not remain armed");

        // The cold move: no stamp, so the seam returns before `classify_move`,
        // the ring scores one decline and nothing is born. Negative control: a
        // fall-through that mints light off a bare cursor delta.
        let mut source = model.init_state();
        source.insert("resident", 1);
        let mut cold_projected = source.clone();
        cold_projected.insert("spawns", 1);
        cold_projected.insert("declined_tally", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &source,
            &cold_projected,
            Some("ColdMoveDeclines"),
            "CursorTrail cold program denial",
        );
        assert!(ok, "real cold-trail decision rejected: {why}");
        let mut leaked = cold_projected.clone();
        leaked.insert("births", 1);
        leaked.insert("licensed_tally", 1);
        leaked.insert("declined_tally", 0);
        leaked.insert("cold_admitted", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &source,
            &leaked,
            Some("ColdMoveDeclines"),
            "CursorTrail cold program negative control",
        );
        assert!(!ok, "model must reject the historical cold fall-through");

        // The other half: the same cold move must not destroy the light this
        // engine's earned sparks hold.
        let mut wiped = cold_projected;
        wiped.insert("resident", 0);
        wiped.insert("wiped", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &source,
            &wiped,
            Some("ColdMoveOverEarnedLight"),
            "CursorTrail retention negative control",
        );
        assert!(!ok, "a declined move may not wipe earned trail light");

        // A press arms the licence, and its one paired echo spends it.
        let mut armed = model.init_state();
        armed.insert("hint", 1);
        armed.insert("arms", 1);
        armed.insert("credit_arms", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &model.init_state(),
            &armed,
            Some("PressArmsLicense"),
            "CursorTrail press arms the licence",
        );
        assert!(ok, "real press arm rejected: {why}");

        let mut echoed = armed.clone();
        echoed.insert("hint", 0);
        echoed.insert("consumed", 1);
        echoed.insert("admissions", 1);
        // The echo spends the press's credit.
        echoed.insert("spent", 1);
        echoed.insert("spawns", 1);
        echoed.insert("births", 1);
        echoed.insert("licensed_tally", 1);
        echoed.insert("resident", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &echoed,
            Some("LicensedTypedMoveMintsLight"),
            "CursorTrail licensed echo mints light",
        );
        assert!(ok, "real licensed-trail decision rejected: {why}");

        let mut reused = echoed;
        reused.insert("hint", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &armed,
            &reused,
            Some("LicensedTypedMoveMintsLight"),
            "CursorTrail one-hint-one-echo negative control",
        );
        assert!(!ok, "a spent licence must not survive its own echo");
    }

    #[test]
    fn same_row_jump_spawns_a_bounded_comet_nearest_the_cursor() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), now, &c, &mut out); // seed last = (0,0)
        // A deliberately scripted jump to col 40: capped to max_len cells.
        t.note_synthetic_move(now);
        let fp = t.tick(Some((0, 40)), now, &c, &mut out);
        assert_ne!(fp, 0);
        assert!(t.is_active());
        // Bounded to max_len, all on row 0, none on the destination column 40.
        assert!(out.len() <= c.max_len, "comet bounded to max_len");
        assert!(out.iter().all(|t| t.row == 0 && t.col != 40));
        // The cell nearest the cursor (col 39) is the brightest.
        let head = out.iter().max_by_key(|t| t.col).unwrap();
        let tail = out.iter().min_by_key(|t| t.col).unwrap();
        assert!(head.alpha >= tail.alpha, "head brighter than tail");
        assert_eq!(head.col, 39);
    }

    /// The fingerprint folds the resolved trail colour (`ignite` re-heats it
    /// each frame), so a colour-only change over identical cells perturbs the
    /// fp; idle stays fp 0 whatever the colour.
    #[test]
    fn fingerprint_folds_the_trail_colour() {
        let run = |color: u32| {
            let mut t = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let mut c = cfg(true);
            c.color = color;
            // Deterministic seed → identical cells for both colours; the
            // single shared `now` keeps the alphas byte-identical too.
            t.tick(Some((0, 0)), now, &c, &mut out);
            t.note_synthetic_move(now);
            let fp = t.tick(Some((0, 5)), now, &c, &mut out);
            (fp, out.clone())
        };
        let (fp_green, cells_green) = run(0x0050_FA7B);
        let (fp_amber, cells_amber) = run(0x00FF_B020);
        assert_ne!(fp_green, 0);
        assert_ne!(fp_amber, 0);
        assert_ne!(
            fp_green, fp_amber,
            "identical cells, different colour ⇒ different fp"
        );
        // The cells themselves ARE identical — only the colour differs.
        assert_eq!(cells_green.len(), cells_amber.len());
        // And an idle (empty) trail keeps fp 0 regardless of colour.
        let mut t = CursorTrail::default();
        let mut out = Vec::new();
        let mut c = cfg(true);
        c.color = 0x00FF_B020;
        assert_eq!(t.tick(Some((0, 0)), Instant::now(), &c, &mut out), 0);
    }

    #[test]
    fn comet_fades_to_empty_after_the_duration() {
        let mut t = CursorTrail::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), t0, &c, &mut out);
        t.note_synthetic_move(t0);
        t.tick(Some((0, 5)), t0, &c, &mut out);
        assert!(t.is_active());
        // Halfway: still alive, dimmer.
        let mid = t.tick(Some((0, 5)), t0 + Duration::from_millis(100), &c, &mut out);
        assert_ne!(mid, 0);
        // Past the duration: fully faded, no sparks, fingerprint 0.
        let gone = t.tick(Some((0, 5)), t0 + Duration::from_millis(250), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    /// Warmth is 0 for a lone key (the first key's heat equals `gain`, zeroed by
    /// the `-gain` shift) and ramps with sustained human-cadence typing while
    /// staying below full ignition (intensity ~0 at 300 ms cadence).
    #[test]
    fn warmth_is_zero_for_a_lone_key_and_ramps_with_cadence() {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        cad.on_keystroke(t0);
        assert_eq!(cad.warmth(t0), 0.0, "first key of a burst contributes 0");
        // Ten keys at a human 300ms cadence: warmth converges well above 0…
        let mut t = t0;
        for _ in 0..10 {
            t += Duration::from_millis(300);
            cad.on_keystroke(t);
        }
        let w = cad.warmth(t);
        assert!(w > 0.4, "300ms-cadence warmth converges high (got {w})");
        // …while real IGNITION stays essentially off at that cadence.
        assert!(
            cad.intensity(t) < 0.1,
            "300ms cadence must not ignite (got {})",
            cad.intensity(t)
        );
        // A pause cools warmth back toward 0 (continuity fuel drains).
        assert!(
            cad.warmth(t + Duration::from_millis(1500)) < 0.1,
            "warmth decays after a pause"
        );
    }

    /// With warmth an ember outlives a human inter-key gap (alive at +350 ms);
    /// at warmth 0 the same spark is gone by +250 ms.
    #[test]
    fn warm_chain_outlives_the_gap() {
        let mut t = CursorTrail::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let c = TrailConfig {
            warmth: 0.5,
            ..cfg(true)
        };
        t.tick(Some((0, 0)), t0, &c, &mut out);
        t.note_synthetic_typed(t0);
        t.tick(Some((0, 1)), t0, &c, &mut out); // typing advance, warmth 0.5
        // life = 200ms * (1 + 1.5*0.5) = 350ms → still alive at +340ms…
        let alive = t.tick(Some((0, 1)), t0 + Duration::from_millis(340), &c, &mut out);
        assert_ne!(alive, 0, "warm ember chains across a 300ms+ inter-key gap");
        assert!(t.is_active());
        // …and fully gone at +400ms (crisp tail after the last key).
        let gone = t.tick(Some((0, 1)), t0 + Duration::from_millis(400), &c, &mut out);
        assert_eq!(gone, 0);
        assert!(!t.is_active());
    }

    #[test]
    fn disabled_draws_nothing_but_tracks_cursor() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(false);
        t.tick(Some((0, 0)), now, &c, &mut out);
        let fp = t.tick(Some((0, 40)), now, &c, &mut out);
        assert_eq!(fp, 0);
        assert!(out.is_empty());
        assert!(!t.is_active());
    }

    /// Typed re-anchor (twin of the `CursorGlow` classifier): a candidate-backed
    /// typed one-row move beyond the typed advance (an Ink box rewrapping its
    /// prompt) lays no comet, while the same move with a scripted generic
    /// candidate keeps its comet, and a scripted typed advance still trails.
    #[test]
    fn hinted_ink_wrap_lays_no_comet_but_typing_still_trails() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        // Hinted Ink wrap: (0,30) -> (1,3) — dr=1, dc=-27. No cells.
        let mut t = CursorTrail::default();
        t.tick(Some((0, 30)), t0, &c, &mut out);
        t.note_synthetic_typed(t0 + Duration::from_millis(5));
        let fp = t.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_eq!(fp, 0, "a typed re-anchor sweeps no comet");
        assert!(out.is_empty());
        assert!(!t.is_active());

        // A scripted generic candidate owns the same sweep without borrowing
        // the typed repaint classifier.
        let mut generic = CursorTrail::default();
        generic.tick(Some((0, 30)), t0, &c, &mut out);
        generic.note_synthetic_move(t0 + Duration::from_millis(5));
        let fp = generic.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "a candidate-backed generic jump keeps its comet");
        assert!(!out.is_empty());

        // A hinted PLAIN advance (dist 1) still lays its typing spark.
        let mut typing = CursorTrail::default();
        typing.tick(Some((0, 3)), t0, &c, &mut out);
        typing.note_synthetic_typed(t0 + Duration::from_millis(5));
        let fp = typing.tick(Some((0, 4)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(fp, 0, "candidate-backed typing still trails");
        assert!(!out.is_empty());
    }

    /// Enter then type-ahead, in lockstep with the glow: a glyph typed before
    /// the Enter's response lands (SSH, a slow prompt) keeps the one-shots, and
    /// a fresh generic licence paired with any move but a same-row forward hop
    /// takes that move and leaves the stamp for the glyph's echo. So the
    /// response is not drawn as the glyph's re-anchor, and the glyph's +1 is
    /// licensed.
    #[test]
    fn a_glyph_typed_before_the_enters_response_lands_keeps_its_stamp_for_its_own_echo() {
        let c = cfg(true);
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut trail = CursorTrail::default();
        trail.tick(Some((2, 20)), t0, &c, &mut out);
        // Enter: the Return class (a typed press supersedes nothing in this
        // engine; the glow's supersede targets live in the one generic slot).
        let enter = t0 + ms(100);
        trail.note_return(enter);
        // The next command's first glyph, before the prompt came back.
        let k = enter + ms(120);
        trail.note_typed(k);
        assert_eq!(
            trail.move_hint,
            Some(enter),
            "the glyph keeps the Enter's licence: its response is still owed"
        );
        // The response — a row down, back to the prompt — is the Enter's
        // move and spends the Enter's licence, not the glyph's stamp.
        let resp = k + ms(30);
        let fp = trail.tick(Some((3, 2)), resp, &c, &mut out);
        assert_ne!(fp, 0, "the Enter's response is licensed");
        assert!(trail.move_hint.is_none(), "…and spends the Enter's licence");
        assert!(
            trail
                .type_hint
                .any_fresh(resp, CursorTrail::TYPE_HINT_FRESH),
            "…leaving the glyph's stamp for the glyph's own echo"
        );
        // The glyph's own echo.
        let echo = resp + ms(20);
        let fp = trail.tick(Some((3, 3)), echo, &c, &mut out);
        assert_ne!(fp, 0, "the glyph's +1 is licensed by its own stamp");
        assert!(
            out.iter().any(|cell| cell.row == 3 && cell.col == 2),
            "…and lights its cell: {:?}",
            out.iter()
                .map(|cell| (cell.row, cell.col))
                .collect::<Vec<_>>()
        );
        assert!(
            !trail
                .type_hint
                .any_fresh(echo, CursorTrail::TYPE_HINT_FRESH),
            "one stamp, one echo"
        );
        // A keyless program +1 after that is refused: the stamp is spent and
        // the Enter's licence went with its response.
        assert!(!trail.move_licensed(echo + ms(50)));

        // CONTROL — the same response with NO glyph behind it draws exactly
        // the same: the Enter's move is the Enter's, glyph or no glyph.
        let mut bare = CursorTrail::default();
        bare.tick(Some((2, 20)), t0, &c, &mut out);
        bare.note_return(enter);
        let mut bare_out = Vec::new();
        let bare_fp = bare.tick(Some((3, 2)), resp, &c, &mut bare_out);
        let mut with_glyph = CursorTrail::default();
        with_glyph.tick(Some((2, 20)), t0, &c, &mut out);
        with_glyph.note_return(enter);
        with_glyph.note_typed(k);
        let mut glyph_out = Vec::new();
        let glyph_fp = with_glyph.tick(Some((3, 2)), resp, &c, &mut glyph_out);
        assert_eq!(
            (bare_fp, bare_out),
            (glyph_fp, glyph_out),
            "the Enter's response draws the same with a type-ahead glyph behind it"
        );
    }

    /// On the alt screen a typed classifier alone does not re-anchor: without a
    /// fresh repaint blink (vim never hides inside DEC-2026) the dr==1 big-dc
    /// move keeps its comet; with the blink (Claude Code's per-keystroke
    /// bracket) the same admitted move lays none.
    #[test]
    fn alt_without_blink_keeps_the_jump_comet() {
        let c = cfg(true);
        let t0 = Instant::now();
        let mut out = Vec::new();

        // vim (alt ctx, hint armed, NO blink): the jump comet flies.
        let mut vim = CursorTrail::default();
        vim.note_context(true);
        vim.tick(Some((0, 30)), t0, &c, &mut out);
        vim.note_synthetic_typed(t0 + Duration::from_millis(5));
        vim.note_context(true);
        let fp = vim.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_ne!(
            fp, 0,
            "alt + typed candidate + NO blink: the jump keeps its comet"
        );
        assert!(!out.is_empty());

        // Claude Code (alt ctx, hint armed, FRESH blink): the wrap re-anchors.
        let mut cc = CursorTrail::default();
        cc.note_context(true);
        cc.tick(Some((0, 30)), t0, &c, &mut out);
        cc.note_synthetic_typed(t0 + Duration::from_millis(5));
        cc.note_repaint_blink(t0 + Duration::from_millis(8));
        cc.note_context(true);
        let fp = cc.tick(Some((1, 3)), t0 + Duration::from_millis(10), &c, &mut out);
        assert_eq!(
            fp, 0,
            "alt + hint + fresh blink: the re-anchor lays no comet"
        );
        assert!(out.is_empty());
        assert!(!cc.is_active());
    }

    #[test]
    fn hidden_cursor_does_not_spawn_on_reappear() {
        let mut t = CursorTrail::default();
        let now = Instant::now();
        let mut out = Vec::new();
        let c = cfg(true);
        t.tick(Some((0, 0)), now, &c, &mut out); // seen at (0,0)
        t.tick(None, now, &c, &mut out); // cursor hidden
        let fp = t.tick(Some((0, 40)), now, &c, &mut out); // reappears far away
        assert_eq!(fp, 0, "no comet across a hide/show gap");
        assert!(out.is_empty());
    }

    #[test]
    fn bounded_tail_matches_the_legacy_full_sweep() {
        for (origin, destination) in [
            ((0, 0), (0, 40)),
            ((0, 40), (0, 0)),
            ((1, 2), (37, 9)),
            ((37, 9), (1, 2)),
            ((3, 4), (31, 45)),
            ((31, 45), (3, 4)),
        ] {
            for limit in 1..=12 {
                let mut legacy = line_cells(origin.0, origin.1, destination.0, destination.1);
                legacy.pop();
                if legacy.len() > limit {
                    legacy.drain(..legacy.len() - limit);
                }
                let mut bounded = Vec::new();
                crate::trail_sweep::line_cells_tail(
                    &mut bounded,
                    origin,
                    destination,
                    limit,
                    false,
                );
                assert_eq!(bounded, legacy, "{origin:?}->{destination:?}, {limit}");
            }
        }
    }

    #[test]
    fn full_u16_jump_reuses_bounded_spawn_scratch() {
        let now = Instant::now();
        let c = cfg(true);
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        trail.tick(Some((0, 0)), now, &c, &mut out);
        trail.note_synthetic_move(now);
        trail.tick(
            Some((0, u16::MAX)),
            now + Duration::from_millis(1),
            &c,
            &mut out,
        );
        assert_eq!(trail.spawn_path.len(), c.max_len);
        let (ptr, capacity) = (trail.spawn_path.as_ptr(), trail.spawn_path.capacity());

        trail.note_synthetic_move(now + Duration::from_millis(2));
        trail.tick(Some((0, 0)), now + Duration::from_millis(3), &c, &mut out);
        assert_eq!(trail.spawn_path.len(), c.max_len);
        assert_eq!(trail.spawn_path.capacity(), capacity);
        assert_eq!(
            trail.spawn_path.as_ptr(),
            ptr,
            "warm move does not allocate"
        );
    }

    // ---- typing-cadence ignition -----------------------------------------

    /// A `TrailConfig` at a chosen ignition level (else the standard test cfg).
    fn cfg_q(q: f32) -> TrailConfig {
        TrailConfig {
            intensity: q,
            ..cfg(true)
        }
    }

    /// Drive `keys` keystrokes `interval` apart into a fresh tracker and return the
    /// intensity immediately after the last one.
    fn burst(keys: u32, interval: Duration) -> f32 {
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..keys {
            cad.on_keystroke(t);
            t += interval;
        }
        // `t` advanced one interval past the last key; read AT the last key.
        cad.intensity(t - interval)
    }

    #[test]
    fn ignition_intensity_is_a_clamped_smoothstep() {
        // Below the low knee → gentle 0; above the high knee → full 1; monotone between.
        assert_eq!(ignition_intensity(0.0, 2.0, 6.5), 0.0);
        assert_eq!(ignition_intensity(2.0, 2.0, 6.5), 0.0);
        assert_eq!(ignition_intensity(6.5, 2.0, 6.5), 1.0);
        assert_eq!(ignition_intensity(100.0, 2.0, 6.5), 1.0);
        let mid = ignition_intensity(4.25, 2.0, 6.5); // exact midpoint → 0.5
        assert!((mid - 0.5).abs() < 1e-4, "smoothstep midpoint {mid}");
        // Monotone non-decreasing across the ramp.
        let mut prev = -1.0;
        for i in 0..=45 {
            let h = i as f32 * 0.2;
            let v = ignition_intensity(h, 2.0, 6.5);
            assert!(v >= prev - 1e-6, "monotone at heat {h}: {v} < {prev}");
            assert!((0.0..=1.0).contains(&v));
            prev = v;
        }
        // Degenerate knees don't divide by zero — they step at the threshold.
        assert_eq!(ignition_intensity(5.0, 3.0, 3.0), 1.0);
        assert_eq!(ignition_intensity(2.0, 3.0, 3.0), 0.0);
    }

    #[test]
    fn a_few_keys_stay_gentle() {
        // One, two, even three quick keys must not ignite (the user's "don't make it
        // intense for a few characters"). 50 ms apart = a brisk but short tap.
        let iv = Duration::from_millis(50);
        assert_eq!(burst(1, iv), 0.0, "a single key never ignites");
        assert!(
            burst(2, iv) < 0.05,
            "two keys stay gentle: {}",
            burst(2, iv)
        );
        assert!(
            burst(3, iv) < 0.15,
            "three keys stay gentle: {}",
            burst(3, iv)
        );
    }

    #[test]
    fn fast_sustained_burst_ignites() {
        // A sustained fast burst (a real touch-typist, ~40 ms/key) crosses into
        // ignition — the comet lights up.
        let hot = burst(16, Duration::from_millis(40));
        assert!(hot > 0.85, "fast sustained typing ignites (got {hot})");
    }

    #[test]
    fn slow_typing_never_ignites() {
        // Hunt-and-peck (400 ms/key) never accumulates heat, however long you go.
        let cold = burst(24, Duration::from_millis(400));
        assert!(cold < 0.1, "slow typing stays gentle (got {cold})");
    }

    #[test]
    fn a_pause_decays_the_ignition() {
        // Ignite with a fast burst, then stop: within about half a second the comet
        // has cooled back to gentle.
        let mut cad = TypingCadence::default();
        let t0 = Instant::now();
        let mut t = t0;
        for _ in 0..16 {
            cad.on_keystroke(t);
            t += Duration::from_millis(40);
        }
        let peak = cad.intensity(t);
        assert!(peak > 0.85, "burst ignited (got {peak})");
        // Mid-pause it is already dropping; by ~600 ms it is essentially out.
        assert!(
            cad.intensity(t + Duration::from_millis(600)) < 0.05,
            "cooled after a pause"
        );
        // Non-mutating read: querying far in the future still reports gentle.
        assert_eq!(cad.intensity(t + Duration::from_secs(10)), 0.0);
    }

    /// [`TypingCadence::sample`] is bit-identical to the two separate reads at
    /// the same instant: cold (`last: None`), at every key of an igniting
    /// burst, and across the decay tail. The gui driver relies on this to do
    /// one decay per frame.
    #[test]
    fn sample_matches_the_separate_reads_bit_for_bit() {
        fn check(cad: &TypingCadence, now: Instant, tag: &str) {
            let (i, w) = cad.sample(now);
            assert_eq!(
                i.to_bits(),
                cad.intensity(now).to_bits(),
                "{tag}: intensity"
            );
            assert_eq!(w.to_bits(), cad.warmth(now).to_bits(), "{tag}: warmth");
        }
        let t0 = Instant::now();
        let mut cad = TypingCadence::default();
        check(&cad, t0, "cold, untouched tracker");
        let mut t = t0;
        for k in 0..16 {
            cad.on_keystroke(t);
            check(&cad, t, &format!("burst key {k}"));
            t += Duration::from_millis(40);
        }
        for ms in [1u64, 100, 300, 600, 5000] {
            check(
                &cad,
                t + Duration::from_millis(ms),
                &format!("decay +{ms} ms"),
            );
        }
    }

    #[test]
    fn readability_cap_bounds_every_cell_at_any_intensity() {
        // The non-negotiable guard: no swept cell ever exceeds the readable ceiling,
        // for ANY ignition level — so the glyph drawn on top always stays legible.
        for &q in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut tr = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let c = cfg_q(q);
            tr.tick(Some((0, 0)), now, &c, &mut out); // seed
            tr.note_synthetic_move(now);
            tr.tick(Some((0, 40)), now, &c, &mut out); // long jump → many cells
            assert!(!out.is_empty());
            for cell in &out {
                assert!(
                    cell.alpha <= READABLE_ALPHA_CAP,
                    "cell alpha {} exceeds readable cap {} at intensity {q}",
                    cell.alpha,
                    READABLE_ALPHA_CAP
                );
            }
        }
    }

    #[test]
    fn ignited_comet_is_brighter_and_longer_lived_than_gentle() {
        let seed_and_jump = |q: f32| -> (u8, bool) {
            let mut tr = CursorTrail::default();
            let now = Instant::now();
            let mut out = Vec::new();
            let c = cfg_q(q);
            tr.tick(Some((0, 0)), now, &c, &mut out);
            tr.note_synthetic_move(now);
            tr.tick(Some((0, 5)), now, &c, &mut out); // same-row jump
            let head = out.iter().map(|t| t.alpha).max().unwrap();
            // The base fade is 200 ms; look 250 ms out — past the gentle life, but
            // inside the ignited (1.6×) life.
            let alive = tr.tick(Some((0, 5)), now + Duration::from_millis(250), &c, &mut out);
            (head, alive != 0)
        };
        let (gentle_head, gentle_alive) = seed_and_jump(0.0);
        let (hot_head, hot_alive) = seed_and_jump(1.0);
        assert!(
            hot_head > gentle_head,
            "ignited core brighter ({hot_head} > {gentle_head})"
        );
        assert!(hot_head <= READABLE_ALPHA_CAP, "…but still readable");
        assert!(!gentle_alive, "gentle comet faded by 250 ms");
        assert!(
            hot_alive,
            "ignited comet still streaming at 250 ms (longer tail)"
        );
    }

    #[test]
    fn ignite_stamps_intensity_and_heats_colour() {
        // Zero intensity is a no-op on colour (byte-identical to the pre-ignition path).
        let mut c = cfg(true);
        let base = c.color;
        ignite(&mut c, 0.0, 0.0);
        // (warmth/chain law tests live below: warmth_is_zero_for_a_lone_key…,
        // warm_chain_outlives_the_gap)
        assert_eq!(c.intensity, 0.0);
        assert_eq!(c.warmth, 0.0);
        assert_eq!(c.color, base, "gentle keeps the base colour");
        // Full ignition warms the colour toward the hot core and clamps intensity.
        let mut c = cfg(true);
        ignite(&mut c, 2.0, 2.0); // out-of-range clamps to 1.0
        assert_eq!(c.intensity, 1.0);
        assert_eq!(c.warmth, 1.0);
        assert_ne!(c.color, base, "ignited colour is heated");
        // Heated toward warm white-amber: red channel rises above the cool base.
        assert!(
            (c.color >> 16) & 0xff > (base >> 16) & 0xff,
            "red channel warmed"
        );
    }

    /// Navigation suppression: a move paired only with a nav timestamp
    /// (Ctrl-A/E, Home/End, arrows) lays no comet, matching the glow engine; the
    /// same geometry with a scripted candidate proves the positive path.
    #[test]
    fn navigation_lays_no_comet() {
        let now = Instant::now();
        let c = cfg(true);
        // Baseline: an explicitly candidate-backed same-row jump spawns.
        let mut plain = CursorTrail::default();
        let mut out = Vec::new();
        plain.tick(Some((0, 30)), now, &c, &mut out); // seed last
        plain.note_synthetic_move(now);
        let fp = plain.tick(Some((0, 0)), now, &c, &mut out); // Ctrl-A-shaped move
        assert_ne!(fp, 0, "a candidate-backed jump lays a comet");
        assert!(plain.is_active());
        // With a fresh nav hint the identical move lays nothing.
        let mut nav = CursorTrail::default();
        out.clear();
        nav.tick(Some((0, 30)), now, &c, &mut out); // seed last
        nav.note_navigation(now);
        let fp = nav.tick(Some((0, 0)), now, &c, &mut out); // Ctrl-A to home
        assert_eq!(fp, 0, "navigation lays no comet");
        assert!(out.is_empty());
        assert!(!nav.is_active());
    }

    /// The probe-table merge in `tick` emits byte-identically (contents and
    /// order) to a linear `out.iter_mut().find(..)` rescan, the reference fold
    /// below, over the same live sparks. The trace covers a serpentine walk
    /// (hundreds of distinct cells, saturating MAX_SPARKS), a same-row
    /// ping-pong (hundreds of sparks stacked on ~48 cells), and the decay tail
    /// to empty.
    #[test]
    fn dedup_index_matches_the_linear_scan_byte_for_byte() {
        let c = TrailConfig {
            enabled: true,
            duration: Duration::from_millis(260),
            max_len: 24,
            color: 0x0050_FA7B,
            intensity: 1.0,
            warmth: 0.0,
        };
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        let mut now = Instant::now();
        for f in 0u64..420 {
            now += Duration::from_millis(16);
            let cur = if f < 180 {
                // Serpentine: 24-cell hops, 10 per 240-column row — cells
                // revisit only after 600 frames, far past any spark lifetime.
                (((f / 10) % 60) as u16, ((f % 10) * 24) as u16)
            } else if f < 360 {
                // Ping-pong: a 200-column scripted move every frame.
                (4, if f.is_multiple_of(2) { 5 } else { 205 })
            } else {
                // Stop moving: the population decays out to empty.
                (4, 205)
            };
            if trail.cursor_anchor().is_some_and(|anchor| anchor != cur) {
                trail.note_synthetic_move(now);
            }
            let fp = trail.tick(Some(cur), now, &c, &mut out);
            // The reference: a linear-scan merge.
            let mut want: Vec<TrailCell> = Vec::new();
            for s in &trail.sparks {
                let life_ms = s.life_ms.max(1);
                let age_ms = now.saturating_duration_since(s.born).as_millis() as u64;
                let tf = ((life_ms - age_ms) * 255 / life_ms) as u32;
                let alpha = ((s.born_alpha as u32 * tf) / 255) as u8;
                if alpha == 0 {
                    continue;
                }
                match want
                    .iter_mut()
                    .find(|t| t.row == s.row as usize && t.col == s.col as usize)
                {
                    Some(existing) => existing.alpha = existing.alpha.max(alpha),
                    None => want.push(TrailCell {
                        row: s.row as usize,
                        col: s.col as usize,
                        alpha,
                    }),
                }
            }
            assert_eq!(out, want, "frame {f}: emitted cells diverged");
            assert_eq!(fp == 0, out.is_empty(), "frame {f}: fp/emptiness law");
        }
    }

    // ---- the nav bridge is gesture-shaped --------------------------------

    /// The reach a fresh nav hint opens is the focused pane's, never the grid's
    /// (an unbounded reach would make any landing a plausible word hop). The
    /// typed and classic lanes are 8 and 2 in both axes.
    #[test]
    fn the_nav_bridge_reaches_one_pane_across_and_a_wrapped_box_down() {
        let pane = Some((0u16, 100u16));
        let ball = |d| BridgeReach {
            cols: d,
            rows: d,
            line_start: None,
        };
        assert_eq!(
            hide_bridge_reach(false, false, pane),
            ball(2),
            "an unhinted reappear keeps the pinned classic ball"
        );
        assert_eq!(
            hide_bridge_reach(true, false, pane),
            ball(8),
            "a typed echo keeps the pinned batched-echo ball"
        );
        let nav = hide_bridge_reach(false, true, pane);
        assert_eq!(
            nav,
            BridgeReach {
                cols: 100,
                rows: HIDE_BRIDGE_NAV_MAX_ROWS,
                line_start: Some(0),
            },
            "a nav hint reaches one pane across and a wrapped box down"
        );
        // The shapes this bound separates. From `(4,42)`, the second row of a
        // wrapped Ink composer whose box starts at column 1:
        let from = (4u16, 42u16);
        assert!(
            nav.admits(from, (2, 1)),
            "Ctrl-A: up two rows to the box's start"
        );
        assert!(nav.admits(from, (3, 90)), "Option+Left wrapping up one row");
        assert!(
            nav.admits(from, (5, 3)),
            "Option+Right wrapping down one row"
        );
        assert!(nav.admits(from, (5, 42)), "Down: one row, the same column");
        assert!(nav.admits(from, (4, 1)), "Ctrl-A along the caret's own row");
        assert!(
            !nav.admits(from, (11, 3)),
            "the measured stray, seven rows down"
        );
        assert!(
            !nav.admits(from, (6, 90)),
            "two rows DOWN to an arbitrary column: a re-laid input box, or End \
             — the two are one shape, and neither is bridged"
        );
        assert!(
            !nav.admits(from, (8, 3)),
            "four rows DOWN to the box's start is not Home either (Home goes up)"
        );
        assert!(
            !nav.admits(from, (2, 42)),
            "two rows UP at the same column: a box that moved, not a line gesture"
        );
        assert!(
            !nav.admits(from, (2, 30)),
            "two rows up to an interior column is not Home"
        );
        // The typed and classic balls carry no line gesture: a two-row typed
        // echo inside its ball is admitted whatever its column.
        assert!(hide_bridge_reach(true, false, pane).admits(from, (6, 44)));
        // A narrower pane narrows the gesture with it — and a pane that starts
        // at column 50 puts Home's landing at column 50, not at 0.
        assert_eq!(hide_bridge_reach(false, true, Some((0, 40))).cols, 40);
        let right = hide_bridge_reach(false, true, Some((50, 50)));
        assert!(
            right.admits((4, 80), (2, 52)),
            "Home in the right-hand pane"
        );
        assert!(
            !right.admits((4, 80), (2, 2)),
            "…and column 2 is the LEFT pane's start, not this pane's"
        );
        // A pane nobody named still never reaches the grid.
        assert_eq!(
            hide_bridge_reach(false, true, None),
            BridgeReach {
                cols: HIDE_BRIDGE_NAV_UNPANED_COLS,
                rows: HIDE_BRIDGE_NAV_MAX_ROWS,
                line_start: Some(0),
            }
        );
        // A one-column pane must not be TIGHTER than the typed lane.
        assert_eq!(
            hide_bridge_reach(false, true, Some((0, 1))).cols,
            HIDE_BRIDGE_TYPED_MAX_DIST
        );
    }

    /// The source a nav key may reach back to is bounded on both sides of the
    /// press: older than one hide window is a cell an earlier state left
    /// behind; newer by more than the settle window proves the key moved
    /// nothing (the no-op Ctrl-E case).
    #[test]
    fn a_caret_seen_still_at_rest_after_the_press_proves_the_key_moved_nothing() {
        let key = Instant::now();
        let ms = Duration::from_millis;
        assert!(
            nav_bridge_source_ok(key, key - ms(HIDE_BRIDGE_MS)),
            "one hide window before the press is the cell it moved from"
        );
        assert!(
            !nav_bridge_source_ok(key, key - ms(HIDE_BRIDGE_MS + 1)),
            "older than that belongs to an earlier state"
        );
        assert!(
            nav_bridge_source_ok(key, key + ms(NAV_NOOP_SETTLE_MS)),
            "a sighting inside the echo window is the key's own in-flight echo"
        );
        assert!(
            !nav_bridge_source_ok(key, key + ms(NAV_NOOP_SETTLE_MS + 1)),
            "a caret still at rest past the echo window: the key moved nothing"
        );
        // The landing is bounded too: later than `NAV_BRIDGE_LANDING_MS` after
        // the press is not its echo (the nav hint's 250 ms lets a second press
        // get its own licence; the bridge is a narrower claim).
        let seen = key - ms(20);
        assert!(nav_bridge_admits(
            key,
            seen,
            key + ms(NAV_BRIDGE_LANDING_MS)
        ));
        assert!(!nav_bridge_admits(
            key,
            seen,
            key + ms(NAV_BRIDGE_LANDING_MS + 1)
        ));
        assert!(
            !nav_bridge_admits(key, key + ms(NAV_NOOP_SETTLE_MS + 1), key + ms(100)),
            "the source bound still applies inside the landing window"
        );
    }
}
