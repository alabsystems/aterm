// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a WORKER (a Claude Code
//! session in another aterm tab) moving without rubber-stamping it. Three pure,
//! unit-tested judgments and one loop:
//!
//! * [`classify::classify_command`] — is this shell line read-only? (The
//!   corpus that shaped it is the test.)
//! * [`prompt::parse_prompt`] — the approval box on the screen, parsed.
//! * [`phase::worker_phase`] — busy / prompt / limited / idle / question,
//!   from one read (busy only from the live zone around the composer);
//!   [`phase::survey_open`] — the session survey parked above it;
//!   [`phase::context_left`] — how much context is left before auto-compact.
//! * [`run::Session`] — `await-turn`, `supervise` and `watch`, over the
//!   control verbs.
//! * [`report`] — `report`: what the worker said since the manager's turn,
//!   the rows a fullscreen app scrolled off (`offscreen`) joined with the
//!   screen's.
//!
//! It moved here from a scratchpad script because every rule in it was paid for
//! by a misclassification in a real session; a supervisor that is itself an
//! agent should not have to rediscover them.

pub mod classify;
pub mod phase;
pub mod prompt;
pub mod report;
pub mod run;
pub mod screen;

pub use classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command, classify_command_with};
pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use prompt::{Prompt, PromptKind, parse_prompt};
pub use report::{DEFAULT_MAX_ROWS, Mark, Marker, Reason, Report, ReportOpts};
pub use run::{
    Caps, Ctl, CtlReply, EXIT_TIMEOUT, ReportBrief, Session, SuperviseOpts, Turn, event_line,
    exit_reason, render_phase, render_phase_and_survey, render_result, reported_event_line,
};
pub use screen::{Screen, parse_text_json};
