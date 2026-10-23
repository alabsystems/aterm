// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence's pure core: the NEEDS BORDER, its titlebar PILL and the TAB
//! MARKS (`docs/DESIGN-owner-surfaces-2026-10-16.md` §5), and the `status`
//! record of who drives a session.
//!
//! A window tells its owner, in colour and in words, that something in it
//! needs them, and offers the one act: the border ([`Rim`],
//! [`View::rim_glow`]) draws the window's most pressing session — any tab,
//! any pane ([`drive::Desk::pressing_session`]). The pill ([`Pill`],
//! [`pill_words`], [`pill_layout`]) draws the most pressing need with
//! [`NeedFact::show_pill`], and each tab's chip its own mark ([`ChipLevel`]).
//! A host can retain a reported need on the border and chip without a pill.
//! All three read the host's PUBLISHED need verdict
//! ([`Facts::need`], [`NeedFact`]); nothing here classifies a need again.
//!
//! The host gathers the [`Facts`] (the published need, the person's hold,
//! screen, leases, fabric, clock); this module folds them per session
//! ([`Slot`]), keeps the wire's level ([`Level`], `status level= why= hand=
//! story=`), composes the pill and the hover's state line ([`state_line`]),
//! and keeps the per-window [`View`]. [`drive`] sequences refresh, story,
//! deadline and tick through the host's [`drive::Desk`].
//!
//! Laws:
//!
//! * PRESENCE NEVER RE-GRIDS A PANE: it owns no chrome row. The border is an
//!   overlay, the pill sits in the titlebar, and who drives is in chips and
//!   hovers.
//! * Change-driven: nothing here reads a clock (every method takes `now`),
//!   the view is rebuilt only when a fact changes, and [`View::fp`] is `0` on
//!   a quiet window, so an idle desktop pays nothing.
//! * Steady: no pulse, no ripple, no wash, no sound.
//! * No message body, command text the host did not already show, OSC
//!   title control or limit notice text reaches any surface unfiltered:
//!   every free-text value passes [`sanitize_token`] or [`pill_text`].
//!
//! What the engine cannot know (wall vocabulary, the input stall, the host's
//! own lease holder, the percent codec, the grid's cell measure) comes in
//! through [`Host`].

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

/// What the host knows that the engine cannot: implemented on a host-local
/// marker type (the orphan rule forbids an impl on a foreign type).
pub trait Host: Copy + Eq + Debug {
    /// An agent wall's kind, as the host's screen readers name it.
    type Wall: Copy + Eq + Debug;
    /// The host's published input stall (the program stopped reading input).
    type Stall: Clone + Eq + Debug;
    /// The `agent=` wire word for a wall (`wall:<kind>`).
    fn wall_wire(kind: Self::Wall) -> &'static str;
    /// The word a person reads for a wall: `limited` for a usage wall, the
    /// cause of a network error, else the state it leaves the session in.
    fn wall_band(kind: Self::Wall) -> &'static str;
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
    /// a combining mark none. The token cap and the in-grid pill measure with
    /// it ([`sanitize_token`], [`pill_layout`]). This crate links no width
    /// tables and a char count is wrong, so there is no default.
    fn columns(s: &str) -> usize;
}

/// The story ring's capacity.
pub(crate) const STORY_CAP: usize = 256;

/// The longest text `ctl story` carries, in bytes — the wire cap, checked on
/// the control thread before any wake.
pub const TOLD_TEXT_MAX_BYTES: usize = 96;

/// The context-left percentage at and below which the hover line warns.
pub(crate) const CTX_WARN_PCT: u8 = 15;

/// The border's inset alpha: the drop target's, so a lit window reads at the
/// drag highlight's weight.
pub(crate) const RIM_BORDER_ALPHA: u8 = 235;

/// The yours outline's thickness in 1/16 units of the host's size law: the
/// law gives 2..6 px, so this is under one pixel and the host's floor makes
/// it exactly one ([`RimGlow::border_scale_q4`]).
pub const YOURS_BORDER_SCALE_Q4: u8 = 1;
