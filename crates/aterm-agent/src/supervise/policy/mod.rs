// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor policies: pure decisions over parsed screens for approvals,
//! questions, turn ends, limit resets, and safety circuit breakers.

pub mod approval;
pub mod box_call;
pub mod box_command;
pub mod box_facts;
pub mod claude_settings;
pub mod cruise_carry;
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
    Answer, AnswerTarget, ApprovalCtx, Choice, DECLINE_PREFIX, DISK_WORKER_DECLINE,
    DISK_WORKER_FIRST_BASH, DISK_WORKER_ONE_ROW, DISK_WORKER_SHELL_UNREAD, DISK_WORKER_WRAPPED,
    Decision, DeclineStep, QuestionPriority, REFUSAL_LABEL, RULE_ALLOW_ONCE, RULE_ANSWER_LONG_TERM,
    RULE_ANSWER_SHORT_TERM, RULE_BREAKER_READ_ONLY, RULE_DECLINE, RULE_DISK_WORKER,
    RULE_LIMIT_RESET, RULE_MODEL_CONFIRM, RULE_MODEL_SWITCH, RULE_NO_SPEND, RULE_PLAN,
    RULE_PROSE_LONG_TERM, RULE_PROSE_SHORT_TERM, RULE_READ_ONLY, RULE_READ_OUTSIDE_CWD,
    RULE_RM_BREAKER, RULE_RM_BREAKER_WARN, RULE_TALL_BOX, RULE_TRUST_ANY, RULE_TRUST_DIALOG,
    SecretRule, buys, decide, decide_screen, decide_traced, decline, decline_step, decline_text,
    default_secrets, footer_mode, rm_breaker_label, roots_from_config,
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

/// Prefix `word` with its indefinite article (`a` or `an`).
#[must_use]
pub fn a_or_an(word: &str) -> String {
    let article = if word.starts_with(['a', 'e', 'i', 'o']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {word}")
}
