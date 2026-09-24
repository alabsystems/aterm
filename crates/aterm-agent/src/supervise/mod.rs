// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor: what a manager agent needs to keep a WORKER (a Claude Code
//! session in another aterm tab) moving without rubber-stamping it. Three pure,
//! unit-tested judgments and one loop:
//!
//! * [`classify::classify_command`] — is this shell line read-only? (The
//!   corpus that shaped it is the test.)
//! * [`policy::approval::decide`] — THE approval decider: which option, if
//!   any, to press on an approval box, under which rule and behind which
//!   guard ([`policy`]).
//! * [`prompt::parse_prompt`] — the approval box on the screen, parsed.
//! * [`phase::worker_phase`] — busy / prompt / limited / idle / question,
//!   from one read (busy only from the live zone around the composer);
//!   [`phase::survey_open`] — the session survey parked above it;
//!   [`phase::context_left`] — how much context is left before auto-compact.
//!   (Both modules are the `aterm-phase` crate, re-exported. aterm's
//!   server reads the screen with the same crate and publishes `agent=`;
//!   the fabric bridge relays that word and reads no screen of its own.)
//! * [`run::Session`] — `await-turn`, `supervise` and `watch`, over the
//!   control verbs.
//! * [`report`] — `report`: what the worker said since the manager's turn,
//!   the rows a fullscreen app scrolled off (`offscreen`) joined with the
//!   screen's.
//! * [`mail`] — `watch --mail`'s lane on the manager's inbox and `task`: the
//!   worker's report comes by mail, one line per worker turn.
//! * [`limit`] — when a limit notice says it resets, as a Unix time: the
//!   watcher waits for it, stretches its budget past it and probes the worker
//!   (`watch --resume`).
//!
//! It moved here from a scratchpad script because every rule in it was paid for
//! by a misclassification in a real session; a supervisor that is itself an
//! agent should not have to rediscover them.

pub mod approvals;
pub mod blocks;
pub mod classify;
pub mod config;
// THE PHASE READER AND THE PROMPT PARSER LIVE IN `aterm-phase` since round 13
// (the fabric bridge publishes `phase=` from the same reader `aterm drive phase`
// prints), re-exported here under the paths they always had.
pub use aterm_phase::{phase, prompt};
pub mod journal;
pub mod ledger;
pub mod ledger_html;
pub mod limit;
pub mod mail;
pub mod policy;
pub mod report;
pub mod run;
pub mod screen;
pub mod transport;

pub use blocks::{Block, BlockKind, View, blocks, view_rows};
pub use classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command, classify_command_with};
pub use config::SupervisorConfig;
pub use journal::{Journal, JournalRecord, read_journal};
pub use ledger::{
    ClockAnchor, Format as LedgerFormat, Ledger, LedgerHost, LedgerOpts, gather, parse_since,
    render as render_ledger,
};
pub use limit::{ResetSpec, parse_reset, reset_at};
pub use mail::{DEFAULT_IDLE_GRACE, DEFAULT_REPORT_WINDOW, MailOpts, MailRow, TaskOpts, task};
pub use phase::{
    Busy, Phase, Zone, busy_signal, context_left, is_placeholder, limit_notice, survey_open,
    transcript_end, worker_phase,
};
pub use policy::approval::ApprovalToggles;
pub use prompt::{Prompt, PromptKind, parse_prompt};
pub use report::{DEFAULT_MAX_ROWS, Mark, Marker, Reason, Report, ReportOpts};
pub use run::{
    ATTENTION_OWNER, ApprovalEnv, CLAIM_HELD, Caps, Ctl, CtlReply, EXIT_TIMEOUT, Fold, Interrupter,
    NoLane, ReportBrief, Session, SuperviseOpts, Turn, UNBOUNDED, event_line, exit_reason,
    render_phase, render_phase_and_survey, render_result, render_result_mail, reported_event_line,
};
pub use screen::{Screen, parse_text_json};
pub use transport::{Endpoint, RelayCtl, Transport};
