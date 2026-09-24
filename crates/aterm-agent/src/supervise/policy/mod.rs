// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policies: pure decisions over what a screen shows, kept
//! apart from the loop that reads screens and presses keys.
//!
//! * [`approval`] — THE approval decider: which option, if any, to choose on
//!   an agent's approval box (read by the reader for the session's program),
//!   under which rule, behind which guard.
//! * [`rm_breaker`] — the resolver behind the approval rule for the vendor's
//!   rm circuit breaker: where every `rm` operand on a line points.
//! * [`guard`] — the press guard that binds a keystroke to the row that was
//!   judged, and the content-sequence fence where the server has one.
//! * [`turn_end`] — THE turn-end decider: what to do when a worker's turn
//!   has ended (continue, wait out a wall, switch a model, compact, log in,
//!   or escalate).

pub mod approval;
pub mod guard;
pub mod rm_breaker;
pub mod turn_end;

pub use approval::{
    ApprovalCtx, ApprovalToggles, Choice, Decision, FooterMode, RULE_READ_ONLY,
    RULE_READ_OUTSIDE_CWD, RULE_RM_BREAKER, RULE_TRUST_DIALOG, SecretRule, decide, decide_screen,
    default_secrets, footer_mode, roots_from_config,
};
pub use guard::{key_args, row_guard, server_fences_gen, server_fences_send};
pub use rm_breaker::{RmScope, ScratchRoot, resolve_rm_line};
pub use turn_end::{
    Composer, ModelSwitch, Said, Then, TurnEndAction, TurnEndReading, TurnEndState, TurnEndTiming,
    classify_said, decide_turn_end, suggestion_allowed,
};
