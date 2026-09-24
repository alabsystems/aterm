// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-phase** — what a Claude Code worker is doing RIGHT NOW, from one
//! screen read, as pure functions over the screen's rows.
//!
//! * [`phase::worker_phase`] — busy | prompt | limited | idle | question, read
//!   from the LIVE ZONE around the composer (the status row above its top
//!   rule, the hints under it, the footer under its bottom rule), never from
//!   the transcript above it;
//! * [`phase::context_left`] — the `<n>% until auto-compact` indicator, when
//!   Claude Code shows it;
//! * [`phase::survey_open`], [`phase::transcript_end`], [`phase::last_said_row`]
//!   — the rest of the live-zone geometry the supervisor's report views use;
//! * [`prompt::parse_prompt`] — the approval box, parsed; [`prompt::parse_prompt_v2`]
//!   — the same box with its title, command rows, option roles and focus, how
//!   an option is chosen and what cancelling does;
//! * [`wall::wall`] — the wall a turn ended on, by kind (a usage window, a
//!   model bucket, spend, a full context, an expired login, an API error,
//!   overload), where `worker_phase` has only `limited` for the usage kinds
//!   and reads the rest `idle`;
//! * [`turn`] — what a finished turn leaves for the next one: Claude Code's
//!   own suggestion, an armed `/goal`, the worker's last words, a stalled
//!   token count;
//! * [`reader::identify`] / [`reader::read`] — the reader for the session's
//!   PROGRAM (Claude Code, Codex, anything else), and one screen read whole;
//! * [`anchors::ANCHORS`] — the literal strings other crates guard on.
//!
//! ## Why a crate of its own
//!
//! Until round 13 this reader lived in `aterm-agent` (`supervise/phase.rs`),
//! where `aterm drive phase` reads it. aterm's server now classifies every
//! agent session's screen and publishes the verdict (`status agent=`, the
//! `EVENT <sid> agent …` push), which the fabric bridge (`aterm-link`)
//! relays as `phase=` without reading a screen — and the design's rule is
//! ONE reader for every face: a verdict the server computed differently
//! from the one `aterm drive phase` prints would be two answers to one
//! question. So the reader lives here, with no dependencies; the server
//! (`aterm-gui`) and `aterm-agent` link it, and `aterm-agent` re-exports it
//! under its old paths so nothing above it moved.
//!
//! Every rule in it was paid for by a misclassification in a real session
//! (the measurements are in the doc comments and the tests); the fixtures
//! under [`prompt::fixtures`] are the screens those sessions showed, each
//! newer one with its program, version and provenance on its first line.

pub mod anchors;
pub mod phase;
pub mod prompt;
pub mod reader;
pub mod turn;
pub mod wall;

pub use anchors::{ANCHORS, Anchor, AnchorKind, anchor};
pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use prompt::{
    Cancel, CancelEffect, Opt, Prompt, PromptKind, PromptV2, Role, Select, parse_prompt,
    parse_prompt_v2, prompt_box_first_row, prompt_box_span,
};
pub use reader::{
    AGENT_RUNTIMES, ClaudeReader, CodexReader, GenericReader, Program, Reading, ScreenReader,
    identify, may_host_agent, program_of, read,
};
pub use turn::{
    Progress, continuation_suggestion, goal_active, interrupted, said_tail, status_row_progress,
    status_row_stall,
};
pub use wall::{Placement, Wall, WallKind, classify_wall, wall};
