// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor's policies: pure decisions over what a screen shows, kept
//! apart from the loop that reads screens and presses keys.
//!
//! * [`approval`]: which option, if any, to choose on an agent's approval box,
//!   under which rule and guard (`approve = "all"` by default, the proven
//!   rules alone under `"safe"`, nothing under `"none"`), or a refusal with a
//!   reason the worker acts on ([`Decision::Decline`], [`decline`]).
//! * [`question`]: a question's answer, the recommended option(s) or option 1
//!   ([`RULE_ANSWER_RECOMMENDED`]), or under `long-term`/`short-term`
//!   ([`QuestionPriority`]) the worker's pick by a fixed policy text
//!   ([`LONG_TERM_TEXT`], [`SHORT_TERM_TEXT`]); what a person began, or what
//!   aterm-phase did not read whole, is escalated.
//! * [`claude_settings`]: where a Claude Code agent's settings live, and the
//!   ask rules and PreToolUse hooks in them that may send a box to a person.
//! * [`box_facts`]: what the loop learned of a box before deciding it, and the
//!   context built from those facts alone ([`ApprovalCtx::from`]).
//! * [`box_call`]: transcript candidates matching a box's possible tool and
//!   file, which lift a settings hold only where the screen draws the box
//!   under the Edit tool's own call row too.
//! * [`box_command`]: a pending transcript command that draws the same rows
//!   as a barred Bash box. Shape and stability do not bind it to this box;
//!   its reading never removes another possible screen reading.
//! * [`git_config`]: what a git read would load that can run a program.
//! * [`probe`]: the one door the approval policy looks at the machine by
//!   ([`PolicyProbe`]), at the level of what it concluded — so a harness tape
//!   holds what a decision turned on, and never the worker's environment.
//! * [`rm_breaker`]: where every `rm` operand on a line points, for the rm
//!   circuit breaker's approval rule.
//! * [`shell_startup`]: what the worker's shell carries in from the person's
//!   startup files, which the rm rule and the classifier must model.
//! * [`guard`]: the press guard binding a keystroke to the judged row, and the
//!   generation fence.
//! * [`reset`]: a limit reset's vocabulary between the loop and its host, and
//!   the verdict on the confirmation its row opens ([`confirm_verdict`]).
//! * [`trace`]: a decision's rule trace — the checks a policy consulted and what
//!   came of each, which `aterm drive replay --why` collects from a tape
//!   (`tape::explain`). The loop keeps no steps.
//! * [`turn_end`]: the turn-end decider (continue, wait out a wall, switch a
//!   model, compact, log in, or escalate).
//! * [`needs`]: the report lines the turn-end decider reads by their head —
//!   `NEEDS YOU:` (what only a person can give: loud, never blocking) and the
//!   `Nothing left:` defense (Cruise's done claim, Land's landing when clean).
//! * [`sleeps`]: the machine's sleeps a loop knows of, which a turn's done
//!   row counts and its work does not.
//! * [`exception`], [`outlook`]: the points only a person can answer, and what
//!   becomes of each wall.

pub mod approval;
pub mod box_call;
pub mod box_command;
pub mod box_facts;
pub mod claude_settings;
pub mod exception;
pub mod git_config;
pub mod guard;
pub mod login;
pub mod needs;
pub mod notice;
pub mod outlook;
pub mod probe;
mod probe_live;
pub mod question;
pub mod reset;
pub mod rm_breaker;
pub mod shell_startup;
pub mod sleeps;
pub mod trace;
pub mod turn_end;

pub use approval::{
    Answer, AnswerTarget, ApprovalCtx, Choice, DECLINE_PREFIX, Decision, DeclineStep,
    QuestionPriority, REFUSAL_LABEL, RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM,
    RULE_ANSWER_SHORT_TERM, RULE_BREAKER_READ_ONLY, RULE_DECLINE, RULE_LIMIT_RESET,
    RULE_MODEL_CONFIRM, RULE_MODEL_SWITCH, RULE_NO_SPEND, RULE_PLAN, RULE_PROSE_LONG_TERM,
    RULE_PROSE_SHORT_TERM, RULE_READ_ONLY, RULE_READ_OUTSIDE_CWD, RULE_RM_BREAKER,
    RULE_RM_BREAKER_WARN, RULE_TALL_BOX, RULE_TRUST_ANY, RULE_TRUST_DIALOG, SecretRule, buys,
    decide, decide_screen, decide_traced, decline, decline_step, decline_text, default_secrets,
    footer_mode, rm_breaker_label, roots_from_config,
};
pub use claude_settings::{AskKind, OwnerAsk};
pub use git_config::{GitView, WorkerEnv};
pub use guard::{
    KeyAct, key_args, row_guard, server_fences_gen, server_fences_send, server_takes_act,
};
pub use probe::{LiveProbe, OnDisk, PolicyProbe, WorkerWrites};
pub use question::{
    LONG_TERM_TEXT, MAX_QUESTION_FOCUS_STEPS, RULE_ANSWER_RECOMMENDED, SHORT_TERM_TEXT,
    answer_question, is_question_rule,
};
pub use reset::{
    KeepWhy, RESET_CONTINUES_WAIT, ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOffer, ResetOutcome,
    ResetWord, Verdict, confirm_verdict,
};
pub use shell_startup::ShellStartup;

/// `word` after its indefinite article (`a write`, `an edit`): `an` before
/// `a`, `e`, `i` and `o`, `a` before anything else, `u` included, which the
/// CLI words all sound as "you" (`a usage-limit`).
#[must_use]
pub fn a_or_an(word: &str) -> String {
    let article = if word.starts_with(['a', 'e', 'i', 'o']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {word}")
}
