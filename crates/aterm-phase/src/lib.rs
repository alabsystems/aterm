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
//! * [`prompt::parse_prompt`] — the approval box, parsed.
//!
//! ## Why a crate of its own
//!
//! Until round 13 this reader lived in `aterm-agent` (`supervise/phase.rs`),
//! where `aterm drive phase` reads it. The fabric bridge (`aterm-link`) now
//! publishes `phase=` on every session's presence row — the word a manager
//! needs without reading a screen — and the design's rule for that is ONE
//! reader for both faces: a phase the bridge computed differently from the
//! one `aterm drive phase` prints would be two answers to one question. So
//! the reader moved here, with no dependencies, and both crates link it;
//! `aterm-agent` re-exports it under its old paths so nothing above it moved.
//!
//! Every rule in it was paid for by a misclassification in a real session
//! (the measurements are in the doc comments and the tests); the fixtures
//! under [`prompt::fixtures`] are the screens those sessions showed.

pub mod phase;
pub mod prompt;

pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use prompt::{Prompt, PromptKind, parse_prompt, prompt_box_span};
