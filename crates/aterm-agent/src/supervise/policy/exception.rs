// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EXCEPTIONS the turn-end policy raises: the points only a person can
//! answer, each with WHY the machine cannot and the ONE ACTION (owner,
//! 2026-09-28: *"there needs to be some clear reason why the user must be
//! engaged and what the user should do"*). Pure: [`exception_for`] reads what
//! the policy already knows and says whether one stands. The wall's
//! [`outlook`](super::outlook::outlook) — the table the tab chip reads — is the
//! earlier module's.
//!
//! The words fit the digest's entry (design-exceptions 8.4): `why` and `action`
//! at most 90 characters each, the action ONE imperative with no ` or `, no
//! `; ` and none of `restart`, `relaunch`, `reopen`, `next launch`, `reboot`,
//! and a `why` that names no waiting or retrying (a wait is never an
//! exception).

use std::time::{Duration, Instant};

use aterm_phase::wall::WallKind;

use super::super::config::SupervisorConfig;
use super::outlook::ExceptionKind;
use super::turn_end::{TurnEndReading, TurnEndState, bucket_model, login_wall_moves};

/// Whether the person is needed now or only when they next look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Now,
    Later,
}

impl Urgency {
    /// The word the journal's `EXCEPTION … when=<word>` carries.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Urgency::Now => "now",
            Urgency::Later => "later",
        }
    }
}

/// One exception: the digest's item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exception {
    pub kind: ExceptionKind,
    pub when: Urgency,
    /// Why a person, and not the machine (at most [`WHY_MAX`] characters).
    pub why: String,
    /// The one thing to do (at most [`ACTION_MAX`] characters).
    pub action: String,
    /// What clears it.
    pub clears: &'static str,
}

impl Exception {
    /// THE STALL NO READER NAMES (the canary's third rung,
    /// `wall_memory::Session::stuck_busy_note`): the screen has read busy on
    /// the agent's own background work alone for hours while Claude's own title
    /// says idle. Nothing the policy reads explains it, so the one thing left
    /// is the owner's: open the tab and say `keep going`. Not raised by
    /// [`exception_for`] (it reads a decided point, and a stall like this
    /// reaches none): the loop's canary builds it. `Later`: the loop cannot
    /// tell what the tab is doing, and is not asking for the owner NOW.
    #[must_use]
    pub fn stalled() -> Self {
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why: "the screen reads busy, Claude's title says idle, and nothing here explains it"
                .to_string(),
            action: "Open the tab and type keep going".to_string(),
            clears: "the screen moving on, or the agent working",
        }
    }

    /// THE DIALOG NO READER NAMES (the canary's third rung for a screen with
    /// no composer, box or busy row, `wall_memory::Session::dialog_note`): its
    /// words have stood unchanged for an hour while Claude's own title says
    /// idle, so it is a dialog this build does not read. The loop types no key
    /// it cannot place (the Enter that continues one notice accepts the wrong
    /// option on another), so answering it is the owner's, in the tab. Not
    /// raised by [`exception_for`] (it needs a wall, and this screen shows
    /// none): the loop's canary builds it. `Later`: nothing says a person is
    /// needed this minute.
    #[must_use]
    pub fn dialog() -> Self {
        Self {
            kind: ExceptionKind::Dialog,
            when: Urgency::Later,
            why: "Claude is stuck on a dialog aterm cannot read, so only you can answer it."
                .to_string(),
            action: "Open the tab and answer the dialog.".to_string(),
            clears: "the screen moving on, or the agent working",
        }
    }

    /// A CONVERSATION NO `/compact` CAN SHORTEN ([`exception_for`]'s arm for a
    /// full context): Claude Code itself said, three `/compact`s running, that
    /// it cannot be reduced. What is left drops part of the conversation, so
    /// how much is the owner's to choose, in the tab: the way back Claude Code
    /// names (`Conversation too long. Press esc twice to go up a few messages
    /// and try again.`). `/clear`, which drops all of it, is not offered.
    #[must_use]
    pub fn context() -> Self {
        Self {
            kind: ExceptionKind::Context,
            when: Urgency::Later,
            why: "Compaction cannot shrink this conversation, so only you can choose what to drop."
                .to_string(),
            action: "Press Esc twice in this tab and go back a few messages.".to_string(),
            clears: "the wall leaving the screen, or the agent working",
        }
    }
}

/// The longest `why`, in characters.
pub const WHY_MAX: usize = 90;
/// The longest `action`, in characters.
pub const ACTION_MAX: usize = 90;

/// `Goal cleared after an unrecoverable error (<label>)`'s label, from a
/// wall's message.
#[must_use]
pub fn goal_cleared_label(message: &str) -> Option<&str> {
    let head = aterm_phase::anchor("wall.goal_cleared");
    let at = message.find(head)? + head.len();
    let rest = message[at..].trim_start().strip_prefix('(')?;
    rest.split(')').next().filter(|l| !l.is_empty())
}

/// Tries at a wall after its reset that the wall answered before it is called
/// futile.
pub const FUTILE_AFTER_TRIES: usize = 3;
/// `/compact`s at a full context, once Claude Code has said it cannot be reduced
/// (`Compaction failed · …`, [`TurnEndState::compaction_exhausted`]), before it is
/// the owner's. (Not [`FUTILE_AFTER_TRIES`]: retuning one is not retuning the other.)
pub const CONTEXT_EXHAUSTED_AFTER: usize = 3;
/// And how long past the reset.
pub const FUTILE_AFTER: Duration = Duration::from_secs(60 * 60);
/// How long a bucket no reset ends stands, with its fallback not made, before
/// it is an exception: the tab's own jobs end in minutes, usually, and the
/// switch is made then.
pub const BUCKET_STUCK_AFTER: Duration = Duration::from_secs(60 * 60);

/// What a person does about an account limit that is no time window and no
/// balance, by the words of the vendor's notice (its own list, both binaries):
/// `(needle, why, action)` — the first row whose needle the message holds. The
/// last row is every spend limit and credit balance.
const ACCOUNT_LIMITS: &[(&str, &str, &str)] = &[
    (
        "seat type doesn't include",
        "Your seat does not include usage credits.",
        "Ask your Claude admin for a seat with usage credits.",
    ),
    (
        "usage allocation has been disabled",
        "Your admin disabled your usage allocation.",
        "Ask your Claude admin to turn your usage allocation back on.",
    ),
    (
        "group's usage limit is set to",
        "Your group's usage limit is set to $0.",
        "Ask your Claude admin to raise your group's usage limit.",
    ),
    (
        "this service is disabled for your org",
        "Your organization turned Claude off for you.",
        "Ask your Claude admin to turn Claude back on for you.",
    ),
    (
        "your organization has disabled claude subscription access",
        "Your organization turned Claude subscription access off.",
        "Ask your Claude admin to turn subscription access back on.",
    ),
    (
        "extra usage is required for long context",
        "Long context needs extra usage, which is not on.",
        "Turn extra usage on in claude.ai settings.",
    ),
    (
        "spend limit",
        "A spend limit, not a time window, stopped it.",
        "Raise the spend limit in claude.ai settings.",
    ),
    (
        "budget",
        "A shared budget, not a time window, stopped it.",
        "Raise the shared budget in claude.ai settings.",
    ),
    (
        "",
        "Its usage credits are used up, and no time window ends that.",
        "Add usage credits in claude.ai settings.",
    ),
];

/// The `(why, action)` for a spend-class wall's message.
#[must_use]
pub fn account_limit_text(message: &str) -> (&'static str, &'static str) {
    let lower = message.to_lowercase().replace('\u{2019}', "'");
    let row = ACCOUNT_LIMITS
        .iter()
        .find(|(needle, _, _)| lower.contains(needle))
        .unwrap_or(&ACCOUNT_LIMITS[ACCOUNT_LIMITS.len() - 1]);
    (row.1, row.2)
}

/// `fable` → `Fable`.
fn title(alias: &str) -> String {
    let mut c = alias.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// The exception that stands at this point, if any.
#[must_use]
pub fn exception_for(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Exception> {
    let kind = reading.wall?;
    let attempts = state.wall_attempts(kind).unwrap_or(0);
    if let Some(label) = goal_cleared_label(&reading.wall_message) {
        return Some(Exception {
            kind: ExceptionKind::GoalCleared,
            when: Urgency::Later,
            why: format!("Claude cleared the /goal: {label}."),
            action: "Run /goal again in this tab.".to_string(),
            clears: "the /goal indicator back on the screen, or the agent working",
        });
    }
    match kind {
        // A lost login right after the session's own account move is the
        // move's, not the person's: it is answered by the next account while
        // one is advised, and nothing is raised for it
        // ([`login_wall_moves`]).
        WallKind::Auth if login_wall_moves(state, reading, cfg, now).is_some() => None,
        WallKind::Auth if attempts > 0 || !cfg.retry_api_errors => Some(Exception {
            kind: ExceptionKind::Login,
            // The loop's own escalation (owner = supervisor) is the surface for a
            // login (design-exceptions 8.4): `Now` says "raised already", and the
            // digest takes only `Later`.
            when: Urgency::Now,
            why: "Its login is gone, and only a browser sign-in restores it.".to_string(),
            action: "Finish signing in on the page /login opened in your browser.".to_string(),
            clears: "`Login successful` on the screen, or the agent working",
        }),
        WallKind::Spend if reading.reset_at.is_none() || !cfg.resume_limits => {
            let (why, action) = account_limit_text(&reading.wall_message);
            Some(Exception {
                kind: ExceptionKind::Spend,
                when: Urgency::Later,
                why: why.to_string(),
                action: action.to_string(),
                clears: "the agent working",
            })
        }
        // A bucket no reset ends, and the tab has not switched off it an hour
        // on: either its own running jobs hold the relaunch, or there is
        // nowhere to switch to. (The switch, when it can be made, is made — the
        // supervisor asks the host again and again, and types nothing blind.)
        WallKind::ModelBucket { .. }
            if reading.reset_at.is_none()
                && state.model_switch().is_none()
                && state
                    .wall_since(kind)
                    .is_some_and(|s| now.saturating_duration_since(s) >= BUCKET_STUCK_AFTER) =>
        {
            let from = bucket_model(&reading.wall_message);
            let name = from
                .as_deref()
                .map_or_else(|| "The model".to_string(), title);
            let can_switch = cfg.relaunch
                && reading.restartable
                && !state.fallback_refused()
                && cfg
                    .model_fallback
                    .as_deref()
                    .is_some_and(|f| Some(f) != from.as_deref());
            let (why, action) = match (&cfg.model_fallback, can_switch) {
                (Some(to), true) => (
                    format!(
                        "{name} has no reset time, and this tab's running jobs keep it from switching to {to}."
                    ),
                    format!("Run /model in this tab and pick {to}."),
                ),
                _ => (
                    format!(
                        "{name} has no reset time, and this tab has no other model to fall back on."
                    ),
                    "Run /model in this tab and pick another model.".to_string(),
                ),
            };
            Some(Exception {
                kind: ExceptionKind::Bucket,
                when: Urgency::Later,
                why,
                action,
                clears: "the agent working",
            })
        }
        WallKind::UsageSession
        | WallKind::UsageWeekly
        | WallKind::Spend
        | WallKind::ModelBucket { .. }
            if attempts >= FUTILE_AFTER_TRIES
                && reading
                    .reset_at
                    .is_some_and(|r| now.saturating_duration_since(r) >= FUTILE_AFTER) =>
        {
            Some(Exception {
                kind: ExceptionKind::Futile,
                when: Urgency::Later,
                why: "The limit still stands an hour after its reset.".to_string(),
                action: "Check claude.ai/settings/usage for what holds it.".to_string(),
                clears: "the agent working",
            })
        }
        // A full context that `/compact` does not lower: Claude Code itself said so
        // ([`TurnEndState::compaction_exhausted`], latched for the appearance, so a
        // request error on a later rung does not end the line) and the loop has
        // tried three times. The ladder goes on (hourly), and so does the line:
        // the `/compact` the loop types is no sign the agent works
        // ([`TurnEndState::compacting`]). A Codex tab's `ran out of room in the
        // model's context window` never says it, so Codex raises nothing here.
        WallKind::Context
            if attempts >= CONTEXT_EXHAUSTED_AFTER && state.compaction_exhausted() =>
        {
            Some(Exception::context())
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;
