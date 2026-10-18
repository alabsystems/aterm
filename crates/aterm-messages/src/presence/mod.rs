// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-tab agent presence row's pure core.
//!
//! A window shows who is driving its session on two surfaces from one model:
//! the rim, a colour-only inset border ([`Rim`], [`View::rim_glow`]), and the
//! row, one chrome line under the tab bar with six slots in a fixed order
//! (role · phase since · hand · mail · ctx · fabric, [`Words`]), so a glance
//! and a screen reader get the same sentence.
//!
//! The host gathers the [`Facts`] (screen, leases, fabric, clock) and paints
//! the cells; this module folds them per session ([`Slot`]), derives the
//! severity ([`Level`], [`Rim`], [`ChipLevel`]), composes and fits the words
//! ([`words`], [`Words::fit`]), paints the row ([`tones`], [`paint_row`]) and
//! keeps the per-window [`View`]. [`drive`] sequences refresh, commit, fold,
//! ripple, story and tick through the host's [`drive::Desk`].
//!
//! Laws:
//!
//! * Change-driven: nothing here reads a clock (every method takes `now`),
//!   the view is rebuilt only when a fact changes, and [`View::fp`] is `0` on
//!   a quiet window, so an idle desktop pays nothing.
//! * The rim never carries a fact the row does not say in words: [`Rim`]
//!   derives from [`Level`], and [`Level`] from the same [`Slot`] the words
//!   come from.
//! * No message body, command text, OSC title or limit notice text reaches
//!   any surface: the row is agent-readable wire text (counts, kinds, a
//!   sender token, a phase word, a reset time), and every free-text value
//!   passes [`sanitize_token`].
//!
//! What the engine cannot know (wall vocabulary, the input stall, the host's
//! own lease holder, the percent codec, the grid's cell measure) comes in
//! through [`Host`].

use core::fmt::Debug;

use crate::{Duration, Instant};

pub mod drive;
mod model;
mod paint;
mod view;
mod words;

pub use model::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, LeaseMark, Level, Link, MailFacts,
    MailLast, Rim, Slot, StopCause, StoryPoint, StoryVerb, Tone, TurnFact, WallAnswer,
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
    /// Whether the harness retries this wall on its own clock (an API error,
    /// an overload): the time the row prints beside it is the next try
    /// (`next try 14:05`), never a reset. Default: no.
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
    /// The restart command the host would carry for the stalled program, as a
    /// person runs it (aterm's: `aterm ctl @<sid> restart pid=<leader>
    /// if-stalled`), or `None`. When it names one, the spoken remedy names it,
    /// for a survivor too in place of `signal kill`.
    fn stall_restart(stall: &Self::Stall) -> Option<&str>;
    /// Whether a drive lease held under `holder`, taken by the process the
    /// kernel names `pid`, is this process's own harness's: its holder name,
    /// AND taken by this very process. The name alone is any client's to
    /// spell; a lease whose taker no kernel named (`None`) is never aterm's.
    fn is_aterms_holder(holder: &str, pid: Option<u32>) -> bool;
    /// The wire's percent encoding of a free-text token.
    fn pct_encode(s: &str) -> String;
    /// The wire's percent decoding of a token.
    fn pct_decode(s: &str) -> String;
    /// The cells the host's terminal grid lays `s` out in from a line's first
    /// column (aterm's: `aterm_grapheme::str_grid_columns`): a wide glyph two,
    /// a combining mark none. The width law, the token cap and the painter all
    /// measure with it ([`Words::fit`], [`sanitize_token`], [`paint_row`]), so
    /// a CJK role is laid out in the cells the grid gives it. This crate links
    /// no width tables and a char count is wrong, so there is no default.
    fn columns(s: &str) -> usize;
}

/// The story ring's capacity.
pub(crate) const STORY_CAP: usize = 256;

/// How long a told story point (`✓ approved`) stands in the phase slot before
/// the slot returns to the phase.
pub const TOLD_FLASH: Duration = Duration::from_secs(3);

/// The longest text `ctl story` carries, in bytes — the wire cap, checked on
/// the control thread before any wake.
pub const TOLD_TEXT_MAX_BYTES: usize = 96;

/// How long the Success tone stands after a settled turn.
pub(crate) const SETTLED_GLOW: Duration = Duration::from_secs(2);

/// The context-left percentage at and below which the row warns.
pub(crate) const CTX_WARN_PCT: u8 = 15;

/// The row's fold quiet: a committed row whose words went away folds (one PTY
/// re-grid) only after it has wanted no row for this long; a want that
/// returns first cancels the fold. Birth stays immediate. A row born and
/// folded inside one sub-millisecond lease is a net-zero re-grid pair, and
/// Claude Code, which repaints only when `SIGWINCH` reads a changed size,
/// then never repaints the alternate-screen row the shrink dropped. The value
/// is the message band's own quiet ([`crate::SHRINK_QUIET`]), far longer than
/// that flap or the other sub-second show-then-fold sources (a peer's lease, a
/// quick hold toggle, a `meta` set and unset, a fabric blip).
pub const FOLD_QUIET: Duration = crate::SHRINK_QUIET;

/// How long a driver's look can hold back a fold ([`FOLD_QUIET`] past the
/// look, never later than this past the want's drop). A client handed a
/// screen generation may name it in an `if-gen=` fence, and a fold committed
/// between that read and the fenced act would refuse the act
/// `reason=changed`; so each look restarts the quiet, and an act inside it
/// brings the want back. Bounded because `status` is polled: a client polling
/// faster than the quiet would otherwise stand the blank row forever. Ten
/// seconds covers a model judging a read (not measured); a driver that must
/// hold the geometry longer holds a drive lease.
pub const FOLD_LOOK_CAP: Duration = Duration::from_secs(10);

/// The ripple's life (a turn submit's edge flash, or a choice pulse).
pub const RIPPLE: Duration = Duration::from_millis(300);

/// How many distinct frames the ripple paints (nine, ~30 fps).
pub(crate) const RIPPLE_STEPS: u32 = 9;

/// The rim's inset-border alpha: the drop target's, so a driven window's rim
/// reads at the drag highlight's weight.
pub(crate) const RIM_BORDER_ALPHA: u8 = 235;

/// The wash alpha under a hold: faint, readable through.
pub(crate) const HOLD_WASH_ALPHA: u8 = 12;

/// The ripple's peak wash, decaying to 0 over its nine steps.
pub(crate) const RIPPLE_WASH_PEAK: u32 = 24;
