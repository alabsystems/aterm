// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policies: pure decisions over what a screen shows, kept
//! apart from the loop that reads screens and presses keys.
//!
//! * [`approval`]: the approval decider: which option, if any, to choose on an
//!   agent's approval box, under which rule and guard (full power by default,
//!   the proven rules alone under `approve = "safe"`, nothing under `"none"`),
//!   or a refusal with a reason the worker acts on ([`Decision::Decline`],
//!   [`decline`], [`decline_step`]) (owner, 2026-09-24; 2026-09-25).
//! * [`question`]: the question answer, with the recommended option(s) or
//!   option 1 ([`RULE_ANSWER_RECOMMENDED`], under `[harness]
//!   answer_questions`), or under `long-term`/`short-term`
//!   ([`QuestionPriority`]) by the worker from a fixed policy text
//!   ([`LONG_TERM_TEXT`], [`SHORT_TERM_TEXT`]); what a person began, or what
//!   aterm-phase did not read whole, is escalated.
//! * [`claude_settings`]: where a Claude Code agent's own settings live, and
//!   the bare-tool ask rules in them that send a file box to a person.
//! * [`git_config`]: what a git read would load that can run a program.
//! * [`rm_breaker`]: where every `rm` operand on a line points, for the rm
//!   circuit breaker's approval rule.
//! * [`shell_startup`]: what the worker's shell carries in from the person's
//!   startup, which makes the rm rule and the classifier prove nothing when
//!   they do not model it.
//! * [`guard`]: the press guard binding a keystroke to the judged row, and the
//!   generation fence where the server has one.
//! * [`reset`]: a limit reset's vocabulary between the loop and its host, and
//!   the verdict on the confirmation its row opens ([`confirm_verdict`]:
//!   Enter only where every condition holds).
//! * [`turn_end`]: the turn-end decider (continue, wait out a wall, switch a
//!   model, compact, log in, or escalate).

pub mod approval;
pub mod claude_settings;
pub mod exception;
pub mod git_config;
pub mod guard;
pub mod outlook;
pub mod question;
pub mod reset;
pub mod rm_breaker;
pub mod shell_startup;
pub mod turn_end;

pub use approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, DECLINE_PREFIX, Decision, DeclineStep, FooterMode,
    NudgeCtx, QuestionPriority, REFUSAL_LABEL, RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM,
    RULE_ANSWER_SHORT_TERM, RULE_DECLINE, RULE_GOAL_RESUME, RULE_LIMIT_RESET, RULE_MODEL_CONFIRM,
    RULE_MODEL_RESTORE_PICK, RULE_MODEL_SWITCH, RULE_NO_SPEND, RULE_PLAN, RULE_PROSE_LONG_TERM,
    RULE_PROSE_SHORT_TERM, RULE_RATE_NUDGE_KEEP, RULE_RATE_NUDGE_SWITCH, RULE_READ_ONLY,
    RULE_READ_OUTSIDE_CWD, RULE_RM_BREAKER, RULE_RM_BREAKER_WARN, RULE_TALL_BOX, RULE_TRUST_ANY,
    RULE_TRUST_DIALOG, SecretRule, buys, decide, decide_screen, decline, decline_step,
    decline_text, default_secrets, footer_mode, rm_breaker_label, roots_from_config,
};
pub use claude_settings::AskRule;
pub use git_config::{GitView, WorkerEnv};
pub use guard::{key_args, row_guard, server_fences_gen, server_fences_send};
pub use question::{
    LONG_TERM_TEXT, MAX_QUESTION_FOCUS_STEPS, RULE_ANSWER_RECOMMENDED, SHORT_TERM_TEXT,
    answer_question, is_question_rule,
};
pub use reset::{
    KeepWhy, RESET_CONTINUES_WAIT, ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOutcome, ResetWord,
    Verdict, confirm_verdict,
};
pub use shell_startup::ShellStartup;

/// `word` after its indefinite article (`a write`, `an edit`, `an auth`), for
/// a reason that names a kind by its CLI word: `an` before `a`, `e`, `i` and
/// `o`; `a` before anything else, `u` included, which those words all sound
/// as "you" (`a usage-limit`).
#[must_use]
pub fn a_or_an(word: &str) -> String {
    let article = if word.starts_with(['a', 'e', 'i', 'o']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {word}")
}
