// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENCE — the per-tab agent status row's pure core (design ruling 348).
//!
//! A window shows who is driving its session. Two surfaces, one model: the
//! RIM, a colour-only inset border ([`Rim`], [`View::rim_glow`]), and the
//! ROW, one chrome line under the tab bar with six fixed slots in a fixed
//! order — role · phase since · hand · mail · ctx · fabric ([`Words`]) — so a
//! glance reads left to right and a screen reader gets the same sentence.
//!
//! This module is everything about that row that is not SENSING: the facts a
//! host gathers ([`Facts`]), the per-session fold and its story ([`Slot`]),
//! the severity ([`Level`]) and what it shows ([`Rim`], [`ChipLevel`]), the
//! words and their width law ([`words`], [`Words::fit`]), the tones and the
//! painted cells ([`tones`], [`paint_row`]) and the per-window view the frame
//! path reads ([`View`]). The host reads the screen, the leases, the fabric
//! and the clock; it hands the facts in and paints the cells out.
//!
//! The SEQUENCING is here too, in [`drive`] (design ruling 353): the refresh
//! of a session and of a window, the row's commit, the fold law, the ripple
//! and the pulse, a told story, the one deadline and the tick. The host
//! implements [`drive::Desk`] — its clock, its window walk, its facts, its
//! on-screen test, its re-grid — and the driver calls it.
//!
//! THE LAWS, from the design (r11/presence-spec.md) and binding here:
//!
//! * The rim is CHANGE-DRIVEN. Nothing here reads a clock: every method that
//!   needs the time takes `now`, and the view is rebuilt only when a fact
//!   changes. [`View::fp`] is exactly `0` on a quiet window, so an idle
//!   desktop pays nothing for this feature.
//! * The rim never carries a fact the row does not say in words: [`Rim`] is
//!   derived from [`Level`], and [`Level`] from the same [`Slot`] the words
//!   are composed from. A colour with no sentence is unreachable.
//! * No message body, command text, OSC title or limit notice text reaches
//!   any surface: the row is agent-readable, so its text is wire text —
//!   counts, kinds, a sender token, a phase word, a reset time — and every
//!   free-text value passes [`sanitize_token`].
//!
//! # The seam
//!
//! What the engine cannot know is injected through [`Host`], implemented on
//! a host-local uninhabited marker: the vocabulary of an agent's WALL (its
//! kind type and its two spellings), the host's published INPUT STALL (its
//! type and three reads), which drive-lease holder is the host's own harness,
//! and the wire's percent codec.

use core::fmt::Debug;

use crate::{Duration, Instant};

pub mod drive;
mod model;
mod paint;
mod view;
mod words;

pub use model::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, LeaseMark, Level, Link, MailFacts,
    MailLast, Rim, Slot, StopCause, StoryPoint, StoryVerb, Tone, TurnFact,
};
pub use paint::{PresenceTones, inks, paint_row, text_cols, tones};
pub use view::{RimGlow, View, chip_of_attention, words_step};
pub use words::{
    SlotKind, Words, fmt_dur, hold_reason_words, prompt_band_word, prompt_kind_words,
    sanitize_token, short_sid, words,
};
// The engine's own laws its tests read by name; no host needs them.
#[cfg(test)]
pub(crate) use view::prints_seconds;
#[cfg(test)]
pub(crate) use words::stall_words;

/// What the host knows that the engine cannot: implemented on a host-local
/// marker type (the orphan rule forbids an impl on a foreign type).
pub trait Host: Copy + Eq + Debug {
    /// An agent wall's kind, as the host's screen readers name it.
    type Wall: Copy + Eq + Debug;
    /// The host's published input stall (the program stopped reading input).
    type Stall: Clone + Eq + Debug;
    /// The `agent=` wire word for a wall (`wall:<kind>`).
    fn wall_wire(kind: Self::Wall) -> &'static str;
    /// The word the row prints for a wall: `limited` for a usage wall, the
    /// cause of a network error, else the state it leaves the session in.
    fn wall_band(kind: Self::Wall) -> &'static str;
    /// Whether the wall is one the harness RETRIES on its own clock (an API
    /// error, an overload): the time a row prints beside it is the loop's
    /// NEXT TRY, spoken so (`next try 14:05`), never a reset. Default: no.
    fn wall_retries(kind: Self::Wall) -> bool {
        let _ = kind;
        false
    }
    /// When the stall's oldest unread byte was accepted.
    fn stall_since(stall: &Self::Stall) -> Instant;
    /// The foreground job is stopped with input queued (not frozen).
    fn stall_stopped(stall: &Self::Stall) -> bool;
    /// The program outlived its restart signal.
    fn stall_survived(stall: &Self::Stall) -> bool;
    /// Whether `holder` is the drive-lease holder name of THIS process's own
    /// harness (ruling 313).
    fn is_aterms_holder(holder: &str) -> bool;
    /// The wire's percent encoding of a free-text token.
    fn pct_encode(s: &str) -> String;
    /// The wire's percent decoding of a token.
    fn pct_decode(s: &str) -> String;
}

/// The story ring's capacity (design §4: `Ring<256, …>`).
pub(crate) const STORY_CAP: usize = 256;

/// How long a told story point stands in the PHASE slot (`✓ approved`) before
/// the slot returns to the phase (the mock: "three seconds after the watcher
/// approves it, the slot reads ✓ approved, then returns to phase").
pub const TOLD_FLASH: Duration = Duration::from_secs(3);

/// The longest text `ctl story` carries, in bytes — the wire cap, checked on
/// the control thread before any wake.
pub const TOLD_TEXT_MAX_BYTES: usize = 96;

/// How long the Success tone stands after a settled turn.
pub(crate) const SETTLED_GLOW: Duration = Duration::from_secs(2);

/// The context-left percentage at and below which the row warns.
pub(crate) const CTX_WARN_PCT: u8 = 15;

/// THE ROW'S FOLD QUIET (design ruling 394, proposed): a committed row whose
/// words went away folds — one PTY re-grid — only after it has wanted no row
/// for this long; a want that returns first cancels the fold, and no re-grid
/// is paid at all. Birth stays immediate. Measured 2026-09-28 on the owner's
/// tab: an idle Claude Code on the alternate screen went stale because the
/// window re-gridded 121x52 -> 51 -> 52 within ~0.8 ms about once a minute —
/// a row born and folded by one sub-millisecond lease, two immediate
/// re-grids. Claude Code repaints only when its `SIGWINCH` handler reads a
/// CHANGED size; after the net-zero pair it read the old one and never
/// repainted, while the alternate screen's shrink had already dropped its top
/// row. The engine's resize undo repairs that pair only when no output lands
/// between its halves. The value is the message band's own D1 quiet
/// ([`crate::SHRINK_QUIET`], 1.5 s): it outlasts the measured flap (under a
/// millisecond) by three orders of magnitude, covers the other show-then-fold
/// sources that are not a driver's own turn (a peer's cooperative lease, a
/// hold toggled quickly, a supervisor or attention `meta` set and unset, a
/// fabric blip — none measured, all sub-second by nature), and a blank row
/// standing 1.5 s is what the band already shows after its own last row goes.
pub const FOLD_QUIET: Duration = crate::SHRINK_QUIET;

/// How long a driver's LOOK can hold back a fold ([`FOLD_QUIET`] past the
/// look, never later than this past the want's drop). A client handed a
/// screen generation (`status gen=`, `text --json`) may name it in an
/// `if-gen=` fence, and a fold committed between that read and the fenced act
/// re-grids the screen under it: the act is refused `reason=changed` (2026-09-28
/// review of ruling 394 — a `meta` unset, a read, the fold's timer, the
/// refused turn; before the quiet the fold landed at the unset, ahead of the
/// read). Each look therefore restarts the quiet, so a driver that reads and
/// acts inside it never meets the fold (its act's lease brings the want back
/// and cancels it). Bounded, because `status` is a POLLED record: a client
/// polling faster than the quiet would otherwise stand the blank row until it
/// stopped. Ten seconds covers a read judged by a model before the act (a few
/// seconds; not measured); a driver that must hold the geometry longer holds
/// a drive lease, under which the count does not move at all.
pub const FOLD_LOOK_CAP: Duration = Duration::from_secs(10);

/// The ripple's life (a turn submit's edge flash, or a choice pulse).
pub const RIPPLE: Duration = Duration::from_millis(300);

/// How many distinct frames the ripple paints (nine, ~30 fps).
pub(crate) const RIPPLE_STEPS: u32 = 9;

/// The rim's inset-border alpha: the drop target's crisp frame, so a driven
/// window's teal reads at the same weight the drag highlight always has.
pub(crate) const RIM_BORDER_ALPHA: u8 = 235;

/// The wash under a HOLD (12/255, design §1): faint, readable through.
pub(crate) const HOLD_WASH_ALPHA: u8 = 12;

/// The ripple's peak wash, decaying to 0 over its nine steps.
pub(crate) const RIPPLE_WASH_PEAK: u32 = 24;
