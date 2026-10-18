// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EXCEPTIONS the turn-end policy raises: the points only a person can
//! answer, each with WHY the machine cannot and the ONE ACTION (owner,
//! 2026-09-28: *"there needs to be some clear reason why the user must be
//! engaged and what the user should do"*). Pure: [`exception_for`] reads what
//! the policy already knows and says whether one stands. The wall's
//! [`outlook`](super::outlook::outlook) is the table the tab chip reads.
//!
//! The words fit the digest's entry (design-exceptions 8.4): `why` and `action`
//! at most 90 characters each, the action ONE imperative with no ` or `, no
//! `; ` and none of `restart`, `relaunch`, `reopen`, `next launch`, `reboot`,
//! and a `why` that names no waiting or retrying (a wait with an end is never
//! an exception). A lost login's, a spend wall's and a futile limit's words
//! are the session's program's ([`TurnEndReading::program`]): Claude Code's,
//! agy's, Muse Code's, Gemini CLI's, or words that name no vendor — never a page or a command its
//! agent does not have.

use std::time::{Duration, Instant};

use aterm_phase::Program;
use aterm_phase::wall::WallKind;

use super::super::config::SupervisorConfig;
use super::outlook::ExceptionKind;
use super::turn_end::{
    RenewStep, TurnEndReading, TurnEndState, bucket_model, login_wall_moves, login_wall_stuck,
    login_wall_waits, renew_step, sign_in_how, waited_login,
};

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
    /// Why a person, and not the machine.
    pub why: String,
    /// The one thing to do.
    pub action: String,
    /// What clears it.
    pub clears: &'static str,
    /// The loop's own escalation carries it: the point's act raised the
    /// tab's badge and the manager's ask with it (the old lost login's
    /// `/login`, and its escalation under `retry_api_errors = false`), so a
    /// host says nothing more of it — nothing is said twice. `false`:
    /// nothing of the loop's tells the owner, and the host is the one that
    /// does — the digest for a `Later` one, the owner interrupted for a
    /// `Now` one (a lost login waited at with no account left to move to).
    pub escalated: bool,
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
            escalated: false,
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
            escalated: false,
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
            escalated: false,
        }
    }

    /// [`Self::context`] for the full context whose notice `message` is: a
    /// conversation of ONE EXCHANGE that the vendor says cannot be compacted
    /// (`Prompt is too long · … A single-exchange conversation cannot be
    /// compacted; reduce attached files/tools or start with less context.`,
    /// 2.1.300) has no messages to go back over, and what fills it is the
    /// tab's tools and attachments: its own remedy, done in the tab. Any
    /// other full context is [`Self::context`].
    #[must_use]
    pub fn context_of(message: &str) -> Self {
        let single = message
            .to_lowercase()
            .contains(&aterm_phase::anchor("wall.cannot_be_compacted").to_lowercase());
        if !single {
            return Self::context();
        }
        Self {
            why: "One exchange fills the context, and compaction cannot shrink it.".to_string(),
            action: "Trim this tab's MCP tools and attachments, then run /clear in it.".to_string(),
            ..Self::context()
        }
    }

    /// A REQUEST THE API REFUSES the same way each time it is sent
    /// ([`aterm_phase::ApiCause::Request`]), its slow ladder tried
    /// [`FUTILE_AFTER_TRIES`] times: what ends it is a person's change in the
    /// tab, named by the notice ([`refused_action`]). `Later`: the ladder
    /// goes on, and nothing says a person is needed this minute.
    #[must_use]
    pub fn refused(typed: Option<&str>, message: &str) -> Self {
        Self {
            kind: ExceptionKind::Refused,
            when: Urgency::Later,
            why: "Claude's API refuses this request the same way each time it is sent.".to_string(),
            action: refused_action(typed, message).to_string(),
            clears: "the agent working",
            escalated: false,
        }
    }
}

/// What a person does about a REFUSED REQUEST ([`Exception::refused`]):
/// `(typed kinds, needles, action)` — the first row one of whose typed kinds
/// is the one Claude Code wrote with the notice (its `apiError`, else its
/// `error`, read from the session's transcript:
/// [`TurnEndState::notice_typed`]), else the first whose needle the notice's
/// words hold; the last row is every other. Fixed aterm words: nothing of
/// the notice is quoted.
const REFUSED_ACTIONS: &[(&[&str], &[&str], &str)] = &[
    (
        &["tool_history_mismatch"],
        &[
            "/rewind",
            "tool use concurrency",
            "tool_result in",
            "tool_use id",
        ],
        "Use /rewind in this tab to go back before the failed step.",
    ),
    (
        &["claude_code_version_too_old"],
        &["claude update", "minimum version"],
        "Update Claude Code: run aterm pkg update claude in a shell.",
    ),
    (
        &["safety_monitor_blocked"],
        &["can't help", "safeguards"],
        "Press Esc twice in this tab and rephrase the flagged message.",
    ),
    (
        &["model_not_found"],
        &["/model", "selected model", "deployment", "pro plan"],
        "Run /model in this tab and pick a model this account can use.",
    ),
    (
        &["account_on_hold"],
        &["on hold"],
        "Open claude.ai/restricted to see why the account is held.",
    ),
    (
        &["effort_requires_thinking"],
        &["/effort", "with thinking"],
        "Run /effort high in this tab.",
    ),
    (
        &[],
        &[],
        "Open the tab and do what Claude Code's last error asks.",
    ),
];

/// The action for a refused request ([`REFUSED_ACTIONS`]): by `typed`, the
/// kind Claude Code wrote with the notice, where there is one the table
/// names, else by the words of `message`, the notice.
#[must_use]
pub fn refused_action(typed: Option<&str>, message: &str) -> &'static str {
    let lower = message.to_lowercase().replace('\u{2019}', "'");
    REFUSED_ACTIONS
        .iter()
        .find(|(kinds, _, _)| typed.is_some_and(|t| kinds.contains(&t)))
        .or_else(|| {
            REFUSED_ACTIONS
                .iter()
                .find(|(_, needles, _)| needles.iter().any(|n| lower.contains(n)))
        })
        .map_or(REFUSED_ACTIONS[REFUSED_ACTIONS.len() - 1].2, |row| row.2)
}

/// The step of a stuck RENEWABLE login's exception, up to the label it names
/// (`Run aterm harness accounts renew <label>.`): the words a host gathers
/// the labels of such asks by.
pub const RENEW_STEP: &str = "Run aterm harness accounts renew ";

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
    // The same entitlement as 2.1.300 words it (`API Error: Usage credits
    // required for 1M context · run /model to switch to standard context`).
    (
        "usage credits required for 1m context",
        "Long context needs usage credits, which this account lacks.",
        "Add usage credits in claude.ai settings.",
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

/// The `(why, action)` for a spend-class wall's message in a session of
/// `program`. Claude Code's by its notice's own words
/// ([`account_limit_text`]). Any other program's sends the person to its own
/// notice, in words that name no vendor where none was measured.
fn spend_text(program: Program, message: &str) -> (&'static str, &'static str) {
    match program {
        Program::Claude => account_limit_text(message),
        Program::Agy => (
            "Its agy account is out of quota or at a spend limit, and no time window ends that.",
            "Do what agy's notice in this tab says to lift the account's limit.",
        ),
        Program::Muse => (
            "Its Muse Code account is out of credits or at a spend limit, and no time window ends that.",
            "Do what Muse Code's notice in this tab says to lift the account's limit.",
        ),
        Program::Gemini => (
            "Its Gemini CLI account is out of quota or at a spend limit, and no time window ends that.",
            "Do what Gemini CLI's notice in this tab says to lift the account's limit.",
        ),
        Program::Codex | Program::Generic => (
            "Its account is out of credits or at a spend limit, and no time window ends that.",
            "Do what the program's notice in this tab says to lift the account's limit.",
        ),
    }
}

/// Where a person reads what holds a limit an hour past its reset, in a
/// session of `program`: Claude Code's usage page; Muse Code's `/upgrade`,
/// which its usage wall names `for increased limits` (measured on 1.4.10;
/// its `/status` and `/usage` cards draw the session's tokens, no limit);
/// Gemini CLI's own `/stats model`, which its usage-limit dialog names
/// (measured on 0.46.0);
/// any other program's account, in words that name no vendor.
fn futile_action(program: Program) -> &'static str {
    match program {
        Program::Claude => "Check claude.ai/settings/usage for what holds it.",
        Program::Agy => "Check agy's account usage for what holds it.",
        Program::Muse => {
            "Run /upgrade in this tab for increased limits, as Muse Code's usage wall says."
        }
        Program::Gemini => "Run /stats model in this tab to see which limit holds it.",
        Program::Codex | Program::Generic => "Check the program's account usage for what holds it.",
    }
}

/// A lost login's `(why, action, clears)` in a session of `program`, its
/// wall reading `message`. Claude Code's `/login`, typed at the point, opened
/// a page in the browser and says `Login successful` when done. Muse Code's step is its key wall's own remedy where it
/// names one. Any other program's step is its own notice's.
fn login_text(program: Program, message: &str) -> (&'static str, String, &'static str) {
    match program {
        Program::Claude => (
            "Its login is gone, and only a browser sign-in restores it.",
            "Finish signing in on the page /login opened in your browser.".to_string(),
            "`Login successful` on the screen, or the agent working",
        ),
        Program::Agy => (
            "Its agy login or Gemini API key was refused, and only your own sign-in or a \
             valid key restores it.",
            format!("{}.", title(&sign_in_how(program, message))),
            "the agent working",
        ),
        Program::Muse => (
            "Its Muse Code login is gone, and only your own sign-in restores it.",
            format!("{}.", title(&sign_in_how(program, message))),
            "the agent working",
        ),
        Program::Gemini => (
            "Its Gemini CLI key or login was refused, and only your own sign-in restores it.",
            format!("{}.", title(&sign_in_how(program, message))),
            "the agent working",
        ),
        Program::Codex | Program::Generic => (
            "Its login is gone, and only your own sign-in restores it.",
            "Sign the program in again as its notice in this tab says.".to_string(),
            "the agent working",
        ),
    }
}

/// The exception that stands at this point, if any.
#[must_use]
pub fn exception_for(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Exception> {
    // A lost login only waited at whose row has left the screen — a person's
    // `/status` or `/usage` in the tab — is the wall it left, as the point is
    // decided ([`waited_login`]): the tab is still stopped, and what stood
    // for it stands.
    let waited;
    let reading = match waited_login(state, reading, cfg, now) {
        Some(shown) => {
            waited = shown;
            &waited
        }
        None => reading,
    };
    let kind = reading.wall?;
    let attempts = state.wall_attempts(kind).unwrap_or(0);
    if let Some(label) = goal_cleared_label(&reading.wall_message) {
        return Some(Exception {
            kind: ExceptionKind::GoalCleared,
            when: Urgency::Later,
            why: format!("Claude cleared the /goal: {label}."),
            action: "Run /goal again in this tab.".to_string(),
            clears: "the /goal indicator back on the screen, or the agent working",
            escalated: false,
        });
    }
    // A lost login after the session's own move, waited at with NO end — the
    // host advises nothing at all ([`login_wall_stuck`]): only the owner's
    // sign-in changes it, so the owner is needed now, and the step is
    // `accounts login` of the session's own login — never the page `/login`
    // opens: none was opened. A wait with an end (below) raises nothing.
    // Nothing of the loop's escalates a wait, so the host is the one that
    // asks the owner; the wait goes on beside it, and the sign-in recorded
    // is news that decides the point again: the move to that login.
    //
    // A RENEWABLE LOGIN ([`TurnEndReading::login_label`]) is renewed, never
    // signed in where it stands: its step is `accounts renew`
    // ([`RENEW_STEP`]), which signs in to a staging directory and checks the
    // account before any session uses it.
    if kind == WallKind::Auth
        && let Some(label) = login_wall_stuck(state, reading, cfg, now)
    {
        let renews = reading.login_label.as_deref() == Some(label.as_str());
        return Some(Exception {
            kind: ExceptionKind::Login,
            when: Urgency::Now,
            why: if renews {
                "Its login has ended, and only a renewal in your browser restores it.".to_string()
            } else {
                "Its login is gone, and the roster has no other account to move it to.".to_string()
            },
            action: if renews {
                format!("{RENEW_STEP}{label}.")
            } else {
                format!("Run aterm harness accounts login {label}.")
            },
            clears: "the login signed in again, an account to move to, or the agent working",
            escalated: false,
        });
    }
    match kind {
        // A lost login read before any turn was asked under the login the
        // session's own move AT A WALL chose is the old process's notice, no
        // login gone ([`TurnEndState::login_left`]).
        WallKind::Auth if state.login_left(reading) => None,
        // A bucket the fallback's relaunch moved the session off, its notice
        // redrawn by the relaunched process ([`TurnEndState::bucket_left`]).
        WallKind::ModelBucket { .. } if state.bucket_left(reading) => None,
        // A lost login right after the session's own account move is the
        // move's, not the person's: it is answered by the next account while
        // one is advised, and nothing is raised for it ([`login_wall_moves`]).
        WallKind::Auth if login_wall_moves(state, reading, cfg, now).is_some() => None,
        // …or waited at until one is, where the wait has an end — the instant
        // the host names, the bound's room: the move goes by itself, and the
        // strike's `signed-out` line is the owner's word
        // ([`login_wall_waits`]).
        WallKind::Auth if login_wall_waits(state, reading, cfg, now).is_some() => None,
        // A renewable login not waited at (no relaunch to follow its renewal
        // with, a draft in the composer): nothing typed, and the point's act
        // is the loop's escalation, whose step is the renewal — or, renewed
        // already, what holds the relaunch onto the renewed login
        // ([`renew_step`]): no renewal reaches this process.
        WallKind::Auth
            if attempts == 0
                && let Some(step) = renew_step(reading, cfg) =>
        {
            let (why, action, clears) = match step {
                RenewStep::Renew(label) => (
                    "Its login has ended, and only a renewal in your browser restores it.",
                    format!("{RENEW_STEP}{label}."),
                    "the login renewed, or the agent working",
                ),
                RenewStep::Draft => (
                    "Its login was renewed, and the draft in its composer holds the move onto it.",
                    "Send the draft in this tab, and it moves onto the renewed login.".to_string(),
                    "the composer empty, or the agent working",
                ),
                RenewStep::ByHand(_) => (
                    "Its login was renewed, and nothing here moves this tab onto the new one.",
                    "Quit Claude Code here and start it with the line accounts renew printed."
                        .to_string(),
                    "the agent working",
                ),
            };
            Some(Exception {
                kind: ExceptionKind::Login,
                when: Urgency::Now,
                why: why.to_string(),
                action,
                clears,
                escalated: true,
            })
        }
        WallKind::Auth if attempts > 0 || !cfg.retry_api_errors => {
            let (why, action, clears) = login_text(reading.program, &reading.wall_message);
            Some(Exception {
                kind: ExceptionKind::Login,
                when: Urgency::Now,
                why: why.to_string(),
                action,
                clears,
                // The loop's own escalation (owner = supervisor) is the surface
                // for this login (design-exceptions 8.4): raised with the
                // `/login` the point typed, or as the point's act under
                // `retry_api_errors = false`.
                escalated: true,
            })
        }
        WallKind::Spend if reading.reset_at.is_none() || !cfg.resume_limits => {
            let (why, action) = spend_text(reading.program, &reading.wall_message);
            Some(Exception {
                kind: ExceptionKind::Spend,
                when: Urgency::Later,
                why: why.to_string(),
                action: action.to_string(),
                clears: "the agent working",
                escalated: false,
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
                escalated: false,
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
                action: futile_action(reading.program).to_string(),
                clears: "the agent working",
                escalated: false,
            })
        }
        // A full context that `/compact` does not lower: Claude Code itself said so
        // ([`TurnEndState::compaction_exhausted`], latched for the appearance, so a
        // request error on a later rung does not end the line) and the loop has
        // tried three times. The ladder goes on (hourly), and so does the line:
        // the `/compact` the loop types is no sign the agent works
        // ([`TurnEndState::compacting`]).
        WallKind::Context
            if attempts >= CONTEXT_EXHAUSTED_AFTER && state.compaction_exhausted() =>
        {
            Some(Exception::context_of(&reading.wall_message))
        }
        // A request the API refuses the same way each time: tried three times
        // on its slow ladder, which goes on; a person's change in the tab ends
        // it ([`Exception::refused`]).
        WallKind::ApiError {
            cause: aterm_phase::ApiCause::Request,
            ..
        } if cfg.retry_api_errors && attempts >= FUTILE_AFTER_TRIES => Some(Exception::refused(
            state.notice_typed(),
            &reading.wall_message,
        )),
        _ => None,
    }
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;
