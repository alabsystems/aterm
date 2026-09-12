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
//!   from one read (busy only from the live zone around the composer).
//! * [`run::Session`] — `await-turn`, `supervise` and `watch`, over the
//!   control verbs.
//!
//! It moved here from a scratchpad script because every rule in it was paid for
//! by a misclassification in a real session; a supervisor that is itself an
//! agent should not have to rediscover them.

pub mod classify;
pub mod phase;
pub mod prompt;
pub mod run;
pub mod screen;

pub use classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command, classify_command_with};
pub use phase::{Busy, Phase, Zone, busy_signal, is_placeholder, limit_notice, worker_phase};
pub use prompt::{Prompt, PromptKind, parse_prompt};
pub use run::{
    Caps, Ctl, CtlReply, EXIT_TIMEOUT, Session, SuperviseOpts, Turn, event_line, exit_reason,
    render_phase, render_result,
};
pub use screen::{Screen, parse_text_json};
