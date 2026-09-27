// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policies: pure decisions over what a screen shows, kept
//! apart from the loop that reads screens and presses keys.
//!
//! * [`approval`] — THE approval decider: which option, if any, to choose on
//!   an agent's approval box (read by the reader for the session's program),
//!   under which rule, behind which guard — by default full power, every box
//!   its answer (the owner's direction of 2026-09-24), owner decision 1's
//!   proven rules alone under `approve = "safe"`, nothing under `"none"` —
//!   or the refusal it amends with a reason the worker acts on
//!   ([`Decision::Decline`], built by [`decline`]), and each keystroke of
//!   that decline ([`decline_step`]).
//! * [`question`] — THE question answer: a question dialog (Claude Code's
//!   AskUserQuestion, read whole; Codex's, by its roles) answered with its
//!   recommended option(s), or option 1 when none is marked — Claude Code's
//!   by Enter on the focused row ([`RULE_ANSWER_RECOMMENDED`], under
//!   `[harness] answer_questions`, independent of `approve`; the owner
//!   directive of 2026-09-25); what a person has begun answering, and what
//!   aterm-phase did not read whole, is escalated. [`approval::decide`] hands
//!   every question to it.
//! * [`rm_breaker`] — the resolver behind the approval rule for the vendor's
//!   rm circuit breaker: where every `rm` operand on a line points.
//! * [`guard`] — the press guard that binds a keystroke to the row that was
//!   judged, and the content-sequence fence where the server has one.
//! * [`turn_end`] — THE turn-end decider: what to do when a worker's turn
//!   has ended (continue, wait out a wall, switch a model, compact, log in,
//!   or escalate).

pub mod approval;
pub mod guard;
pub mod question;
pub mod rm_breaker;
pub mod turn_end;

pub use approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, DECLINE_PREFIX, Decision, DeclineStep, FooterMode,
    REFUSAL_LABEL, RULE_ALLOW_ONCE, RULE_DECLINE, RULE_MODEL_CONFIRM, RULE_MODEL_SWITCH,
    RULE_NO_SPEND, RULE_PLAN, RULE_READ_ONLY, RULE_READ_OUTSIDE_CWD, RULE_RM_BREAKER,
    RULE_TALL_BOX, RULE_TRUST_ANY, RULE_TRUST_DIALOG, SecretRule, buys, decide, decide_screen,
    decline, decline_step, decline_text, default_secrets, footer_mode, rm_breaker_label,
    roots_from_config,
};
pub use guard::{key_args, row_guard, server_fences_gen, server_fences_send};
pub use question::{MAX_QUESTION_FOCUS_STEPS, RULE_ANSWER_RECOMMENDED, answer_question};
