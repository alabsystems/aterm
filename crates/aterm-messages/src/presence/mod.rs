// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence's pure core: the needs border ([`Rim`]), titlebar pill ([`Pill`]),
//! tab marks ([`ChipLevel`]), and `status` record of who drives a session.
//!
//! Folds host [`Facts`] per session ([`Slot`]), composes the pill and hover
//! [`state_line`], and maintains the per-window [`View`] without re-gridding
//! panes or reading a clock directly.

use core::fmt::Debug;

pub mod drive;
mod model;
mod paint;
mod pill;
mod view;
mod words;

pub use model::{
    AgentPhase, AgentReading, ChipLevel, Facts, Hand, HoldFact, Level, Link, MailFacts, MailLast,
    NeedFact, NeedSeverity, Rim, Slot, StopCause, StoryPoint, StoryVerb, TurnFact, WallAnswer,
};
pub use paint::{PresenceTones, inks, tones};
pub use pill::{Pill, PillAct, PillHit, PillKind, PillLayout, Span, pill_layout};
pub use view::{RimGlow, View, chip_of_attention};
pub use words::{
    PillWords, fmt_dur, hold_reason_words, pill_age, pill_age_room, pill_due, pill_text,
    pill_words, prompt_band_word, prompt_kind_words, sanitize_token, short_sid, state_line,
};

/// Host-specific vocabulary, lease ownership, percent encoding, and grid width hooks.
pub trait Host: Copy + Eq + Debug {
    /// An agent wall's kind, as the host's screen readers name it.
    type Wall: Copy + Eq + Debug;
    /// The host's published input stall (the program stopped reading input).
    type Stall: Clone + Eq + Debug;
    /// The `agent=` wire word for a wall (`wall:<kind>`).
    fn wall_wire(kind: Self::Wall) -> &'static str;
    /// The user-facing word for a wall (`limited`, network error cause, or resulting state).
    fn wall_band(kind: Self::Wall) -> &'static str;
    /// Whether a drive lease held under `holder` by `pid` belongs to this process's own harness.
    fn is_aterms_holder(holder: &str, pid: Option<u32>) -> bool;
    /// The wire's percent encoding of a free-text token.
    fn pct_encode(s: &str) -> String;
    /// The wire's percent decoding of a token.
    fn pct_decode(s: &str) -> String;
    /// Terminal grid cell width of `s` from column 0.
    fn columns(s: &str) -> usize;
}

/// The story ring's capacity.
pub(crate) const STORY_CAP: usize = 256;

/// Maximum byte length of text carried by `ctl story`.
pub const TOLD_TEXT_MAX_BYTES: usize = 96;

/// The context-left percentage at and below which the hover line warns.
pub(crate) const CTX_WARN_PCT: u8 = 15;

/// The border's inset alpha.
pub(crate) const RIM_BORDER_ALPHA: u8 = 235;

/// The `yours` outline thickness in 1/16 units of the host's size law ([`RimGlow::border_scale_q4`]).
pub const YOURS_BORDER_SCALE_Q4: u8 = 1;
