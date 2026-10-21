// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exceptions the turn-end policy raises: the points only a person can
//! answer, each with why the machine cannot and the one action. Pure:
//! [`exception_for`] reads what the policy already knows. The wall's
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

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginScreen, Program};

use super::super::background::BackgroundCall;
use super::super::config::SupervisorConfig;
use super::notice::{self, Notice, NoticeText};
use super::outlook::ExceptionKind;
use super::turn_end::{
    AccountAdvice, Composer, ContextFill, RULE_LOGIN, RenewStep, RosterWord, TurnEndAction,
    TurnEndReading, TurnEndState, Unmade, bucket_model, decide_turn_end, login_wall_moves,
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
    /// `Now` one (Claude Code's lost login the harness carries up to the
    /// sign-in, [`super::notice`]).
    pub escalated: bool,
    /// THE OWNER'S FOUR-PART NOTICE ([`super::notice`]): `why` is what
    /// happened and `action` the one thing to do now; this carries what
    /// aterm already did and what to expect after, and the title and id the
    /// host's ask goes by. `None`: an exception of the older kinds, which
    /// say why and what alone.
    pub notice: Option<Notice>,
}

impl Exception {
    /// A notice's exception ([`super::notice`]): `Now`, and no escalation of
    /// the loop's carries it — the host raises the one standing ask on the
    /// glass for it, keyed by the tab and [`NoticeText::code`], and withdraws it
    /// once the point no longer holds it. `label` and `why` fill its
    /// placeholders; `clears` is what ends it.
    #[must_use]
    pub fn of_notice(
        text: &NoticeText,
        label: Option<&str>,
        why: Option<&str>,
        clears: &'static str,
    ) -> Self {
        Self::of_notice_reset(text, label, why, None, clears)
    }

    /// [`Self::of_notice`] for a usage limit whose reset is known and past
    /// which aterm continues the tab: `reset` (`on Oct 23 at 9am`) fills
    /// the expectation that names it ([`NoticeText::fill_reset`]).
    #[must_use]
    pub fn of_notice_reset(
        text: &NoticeText,
        label: Option<&str>,
        why: Option<&str>,
        reset: Option<&str>,
        clears: &'static str,
    ) -> Self {
        let filled = text.fill_reset(label, why, reset);
        Self {
            kind: ExceptionKind::Login,
            when: Urgency::Now,
            why: filled.happened,
            action: filled.todo,
            clears,
            escalated: false,
            notice: Some(Notice {
                id: text.id,
                code: text.code,
                title: text.title,
                short_title: text.short_title,
                did: filled.did,
                expect: filled.expect,
                command: filled.command,
                short: filled.short,
                undo: filled.undo,
                back: text.back,
            }),
        }
    }

    /// THE STALL NO READER NAMES (the canary's third rung,
    /// `wall_memory::Session::stuck_busy_note`): the screen has read busy on
    /// background work alone for hours while Claude's title says idle. Built
    /// by the canary, not [`exception_for`]: such a stall reaches no decided
    /// point.
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
            notice: None,
        }
    }

    /// THE BACKGROUND CALL THAT MAKES NO PROGRESS (the canary's third rung
    /// where the host's jobs reading explains the screen,
    /// `wall_memory::Session::stuck_busy_note`; the hung-call design's R1):
    /// the agent waits on a call of its own that has made no progress since
    /// the canary's `STALLED`, one unit ago. The fact is the why
    /// ([`BackgroundCall::why`], template words only, the digest's 90
    /// characters), never "nothing here explains it". The one thing to do is
    /// to tell the agent, which started the call and alone knows whether to
    /// end it (the hung-call design §4: aterm ends nothing on a timer). Not
    /// raised by [`exception_for`]; `Later`, as [`Self::stalled`].
    #[must_use]
    pub fn no_progress(call: &BackgroundCall) -> Self {
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why: call.why(),
            action: format!(
                "Tell the agent in the tab that pid {} makes no progress",
                call.pid
            ),
            clears: "the screen moving on, or the agent working",
            escalated: false,
            notice: None,
        }
    }

    /// THE BACKGROUND WORK THAT DOES NOT END (the canary's third rung on agy's
    /// or Muse Code's own background work,
    /// `wall_memory::Session::stuck_busy_note`): the turn ended, on a question
    /// perhaps, and the agent's own work (a task, a timer, a subagent, a
    /// backgrounded shell, a monitor) has run for hours under words that
    /// stood still. The loop types nothing into a session the agent wakes
    /// itself when that work ends, so a server or a daemon left running holds
    /// it for ever: the owner's to look at. Not raised by [`exception_for`];
    /// `Later`, as [`Self::stalled`].
    #[must_use]
    pub fn stalled_on_work() -> Self {
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why: "Its turn ended hours ago, and its own background work still runs and holds it."
                .to_string(),
            action: "Open the tab and type keep going".to_string(),
            clears: "the screen moving on, or the agent working",
            escalated: false,
            notice: None,
        }
    }

    /// A dialog no reader names (the canary's third rung for a screen with no
    /// composer, box or busy row, `wall_memory::Session::dialog_note`): its
    /// words have stood unchanged for an hour while Claude's title says idle.
    /// The loop types no key it cannot place (the Enter that continues one
    /// notice accepts the wrong option on another), so the owner answers it.
    /// Built by the canary, not [`exception_for`], which needs a wall.
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
            notice: None,
        }
    }

    /// A conversation no `/compact` can shorten ([`exception_for`]'s
    /// full-context arm). Going back drops part of it, so how much is the
    /// owner's choice, by the way Claude Code names (`Conversation too long.
    /// Press esc twice to go up a few messages and try again.`). `/clear`,
    /// which drops all of it, is not offered.
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
            notice: None,
        }
    }

    /// [`Self::context`] for the full context whose notice `message` is, and
    /// what its episode's own notice said fills it (`fill`,
    /// [`TurnEndState::context_fill`]: by the third `/compact` the vendor's
    /// `Compaction failed` row stands in the notice's place). A conversation
    /// of ONE EXCHANGE that the vendor says cannot be compacted (`Prompt is too
    /// long · … A single-exchange conversation cannot be compacted; reduce
    /// attached files/tools or start with less context.`, 2.1.300) has no
    /// messages to go back over, and what fills it is the tab's tools and
    /// attachments: its own remedy, done in the tab. A context an IMAGE or
    /// ATTACHMENTS fill ([`ContextFill`], by `message` or `fill`) is cleared by
    /// going back to before them, as the vendor's own notice says
    /// (ATERM-KI-1016-1: told `go back a few messages`, which need not reach
    /// them). Any other full context is [`Self::context`].
    #[must_use]
    pub fn context_of(message: &str, fill: Option<ContextFill>) -> Self {
        let single = message
            .to_lowercase()
            .contains(&aterm_phase::anchor("wall.cannot_be_compacted").to_lowercase());
        if single {
            return Self {
                why: "One exchange fills the context, and compaction cannot shrink it.".to_string(),
                action: "Trim this tab's MCP tools and attachments, then run /clear in it."
                    .to_string(),
                ..Self::context()
            };
        }
        match ContextFill::of(message).or(fill) {
            Some(ContextFill::Images) => Self {
                why: "An image in this conversation is too large, and compaction cannot remove it."
                    .to_string(),
                action: "Press Esc twice in this tab and go back to before the image was added."
                    .to_string(),
                ..Self::context()
            },
            Some(ContextFill::Attachments) => Self {
                why: "Its images and attachments make this request too large, and compaction cannot shrink it."
                    .to_string(),
                action: "Press Esc twice in this tab and go back to before the attachments."
                    .to_string(),
                ..Self::context()
            },
            None => Self::context(),
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
            notice: None,
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
/// `/compact`s at a full context, after Claude Code has said it cannot be
/// reduced (`Compaction failed · …`, [`TurnEndState::compaction_exhausted`]),
/// before it is the owner's. Separate from [`FUTILE_AFTER_TRIES`] so each
/// retunes alone.
pub const CONTEXT_EXHAUSTED_AFTER: usize = 3;
/// And how long past the reset.
pub const FUTILE_AFTER: Duration = Duration::from_secs(60 * 60);
/// How long a bucket no reset ends stands, its fallback not made, before it is
/// an exception: the tab's own jobs usually end within minutes, and the switch
/// is made then.
pub const BUCKET_STUCK_AFTER: Duration = Duration::from_secs(60 * 60);

/// What a person does about a Claude Code spend-class wall, by its notice's
/// words: `(needle, why, action)`, the first row whose needle the lowercased
/// message holds; the last row's empty needle catches the rest.
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

/// The `(why, action)` for a Claude Code spend-class wall's message.
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

/// A lost login's `(why, action, clears)` in a session of `program` the
/// harness does not carry ([`super::login::carried`]: Claude Code's is
/// [`claude_login`]'s, every point of it), its wall reading `message`. Muse
/// Code's step is its key wall's own remedy where it names one. Any other
/// program's step is its own notice's.
fn login_text(program: Program, message: &str) -> (&'static str, String, &'static str) {
    match program {
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
        Program::Claude | Program::Codex | Program::Generic => (
            "Its login is gone, and only your own sign-in restores it.",
            "Sign the program in again as its notice in this tab says.".to_string(),
            "the agent working",
        ),
    }
}

/// What ends a lost login's notice: the login back, or the agent working.
const LOGIN_CLEARS: &str = "`Login successful` on the screen, or the agent working";

/// What ends a usage limit's notice that a draft holds the switch off
/// ([`notice::LIMIT_DRAFT`] and its kin, [`notice::DRAFT_HOLDS`]): the draft
/// gone from the prompt, the switch then asked, or the agent working.
const DRAFT_CLEARS: &str = "the prompt empty, or the agent working";

/// A draft's notice asks for a step that can actually empty its prompt.
/// A completion menu must be closed before sending, and aterm's own text
/// is cleared rather than submitted as the person's draft.
fn draft_notice_text(text: &NoticeText, composer: &Composer) -> NoticeText {
    NoticeText {
        todo: match composer {
            Composer::Diverted => "Close the completion menu, then send the draft in the tab.",
            Composer::Own => "Clear aterm's unsent text in the tab.",
            _ => text.todo,
        },
        todo_as: None,
        ..*text
    }
}

/// How far off a weekly usage limit's reset must be before a tab with no
/// account rotation available raises [`notice::LIMIT_SIGN_IN`] (above a 5-hour
/// session window).
const WEEKLY_LIMIT_NOTICE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// CLAUDE CODE'S USAGE LIMIT WITH NO ACCOUNT ROTATION AVAILABLE, OR A
/// PERSON'S `/login` AT A USAGE LIMIT: when Claude Code hits a multi-day
/// weekly usage limit (`reset_at` > 6 hours away) and the account roster has
/// no move, wait, or resume to offer (`reading.advised().is_none()`), the
/// owner is told so the tab does not sit silent for days — what to do
/// chosen by WHOSE THE LOGIN IS ([`TurnEndReading::roster`], the review's
/// acct-1 and acct-7):
///
/// * no roster row's ([`RosterWord::Unlisted`], or no word): `/login` in the
///   tab with another account ([`notice::LIMIT_SIGN_IN`]);
/// * a roster row's: NEVER `/login` in the tab — it would sign another
///   account into that row's own login, and every session on it would
///   follow — but the owner's `accounts use` of another row ready to run now
///   that the roster does not move the tab onto by itself
///   ([`notice::LIMIT_USE`]; nothing where it does, save while a draft
///   holds the move, [`notice::LIMIT_DRAFT`], or a restart of this launch
///   was refused, [`notice::LIMIT_UNMADE`]: [`limit_roster_step`]);
///   else the sign-in by name of the first other row
///   not signed in ([`notice::LIMIT_RENEW`], [`notice::LIMIT_LOGIN`]), after
///   which the tab moves by itself — or, where the roster holds the tab at
///   this limit, aterm names the next step ([`limit_roster_step`]); with no
///   such row, another account added ([`notice::LIMIT_ROSTER`]). Each of the
///   first three promises a switch, so each is told only where one can
///   follow ([`limit_switch`]): where the owner's aterm.toml keeps aterm from
///   switching the tab, the lines that let it ([`notice::LIMIT_STAY`]);
///   where nothing here relaunches the tab at all, [`notice::LIMIT_ROSTER`]
///   too;
/// * not known, the roster refused or the state unread
///   ([`RosterWord::Unread`]): the roster report ([`notice::LIMIT_UNREAD`]).
///
/// Those last two say the tab continues after the reset only where aterm
/// continues it past the reset (`resume_limits`; else
/// [`notice::LIMIT_ROSTER_KEPT`], [`notice::LIMIT_UNREAD_KEPT`]).
///
/// A MULTI-DAY WAIT THE ROSTER ADVISES (an [`AccountAdvice::Wait`] past the
/// same 6 hours) on a roster row with another row to sign in is told the
/// same sign-in by name (the review's acct-4: a synced row never signed in
/// left the tab waiting silently for days). While the owner is in `/login`
/// at a usage limit, its failure is surfaced just like a lost login's, and
/// its cancellation as the limit's own notice — the roster's step on a
/// roster row, else the sign-in to another account ([`notice::LIMIT_SIGN_IN`];
/// the review of the login carry, r5h-4: told a lost login's "sign in again",
/// the owner signed the same account in, whose limit still stood).
fn claude_limit_login(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Option<Exception>> {
    let far = now + WEEKLY_LIMIT_NOTICE_AFTER;
    let roster_step = limit_roster_step(
        reading.roster.as_ref(),
        limit_switch(reading, cfg),
        cfg.resume_limits,
        state.account_refusal(),
        // A draft holds the switch's ask; an upgrade holding the point asks
        // nothing either, and is no draft's to answer.
        Some(&reading.composer).filter(|c| c.drafted() && !reading.upgrading),
    );
    let waits_on_sign_in =
        matches!(
            reading.advised(),
            Some(AccountAdvice::Wait { until }) if *until > far
        ) && matches!(reading.roster, Some(RosterWord::Row { next: Some(_), .. }));
    if !super::login::carried(reading.program)
        || state.wall_attempts(WallKind::UsageWeekly).is_none()
        || (reading.advised().is_some() && !waits_on_sign_in)
        || reading.taskless
        || state.task_done()
    {
        return None;
    }
    // WHEN IT ENDS BY ITSELF: under `resume_limits` aterm continues the tab
    // past the limit's reset whatever the owner does, so a limit's notice
    // names the reset where its notice says one (the review of the band,
    // 2026-10-20: the notices never said when, or that the tab goes on) —
    // the row's on the screen, else, at a `/login` left over the limit, its
    // row drawn over, the reset the limit's track still waits on (r6h-3:
    // told the plain sign-in, though the tab went on at the reset all the
    // same).
    let left = reading.login_interrupted || state.login_cancelled_on_limit();
    let shown = reading.reset_clock.filter(|_| {
        reading.wall == Some(WallKind::UsageWeekly) && reading.reset_at.is_some_and(|at| at > now)
    });
    let reset = shown
        .or_else(|| left.then(|| state.named_reset_clock(now)).flatten())
        .filter(|_| cfg.resume_limits)
        .map(|clock| clock.words());
    let told = |step: Option<LimitStep<'_>>| {
        Some(step.map(|(text, label, why)| {
            let clears = if notice::DRAFT_HOLDS.contains(&text.id) {
                DRAFT_CLEARS
            } else {
                LOGIN_CLEARS
            };
            Exception::of_notice_reset(text, label, why, reset.as_deref(), clears)
        }))
    };
    match &reading.login_screen {
        // The person's own `/login`, theirs as they go through it — Console's
        // menu included, which they chose and nothing stopped (as at a lost
        // login, [`claude_login`]).
        Some(
            LoginScreen::Success { .. }
            | LoginScreen::Picker(_)
            | LoginScreen::ConsoleMenu
            | LoginScreen::Browser,
        ) => {
            return Some(None);
        }
        Some(LoginScreen::Failed) => {
            return told(Some((&notice::LOGIN_STOPPED, None, None)));
        }
        None => {}
    }
    // No row is the login's: a sign-in in the tab — to ANOTHER account,
    // whatever `/login` left before it (r5h-4: the same account signed in
    // again meets the same limit).
    let sign_in = || Some((&notice::LIMIT_SIGN_IN, None, None));
    if left {
        return told(roster_step.unwrap_or_else(sign_in));
    }
    if cfg.resume_limits
        && reading.wall == Some(WallKind::UsageWeekly)
        && reading.reset_at.is_some_and(|at| at > far)
    {
        return told(roster_step.unwrap_or_else(sign_in));
    }
    None
}

/// WHETHER ANOTHER ACCOUNT CAN TAKE THIS TAB OVER AT ITS USAGE LIMIT, as
/// [`limit_roster_step`] needs it (the review of the login carry, r3h-2):
/// `Ok` where a switch can follow the owner's step — the rotation on, and a
/// relaunch that reaches the tab (`relaunch` and `resume_limits` on, Claude
/// Code, a host that relaunches it or an upgrade holding the point for now:
/// what `super::turn_end::account_rotates` asks of a move, a draft and a
/// refused relaunch aside, which pass); `Err(Some(lines))` where the owner's
/// aterm.toml keeps aterm from switching it, with EVERY line of its
/// `[harness]` table that does ([`notice::stay_why`]: `accounts` off,
/// `relaunch` off, `resume_limits` off, named together — the review of the
/// login carry, r4h-2: named one at a time, the line the notice named was
/// set and the next notice named another); `Err(None)` where nothing here
/// relaunches the tab whatever aterm.toml says. The owner's `relaunch` is
/// the host's where it reads the key live (`super::turn_end::relaunch_on`,
/// r4h-1: the loop's config always said `true`). PURE.
fn limit_switch(
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> Result<(), Option<&'static str>> {
    if !super::turn_end::relaunch_lane(reading) {
        return Err(None);
    }
    match notice::stay_why(
        cfg.accounts == crate::harness::accounts::Mode::Off,
        !super::turn_end::relaunch_on(reading, cfg),
        !cfg.resume_limits,
    ) {
        Some(lines) => Err(Some(lines)),
        None => Ok(()),
    }
}

/// A notice a usage limit's tab is told ([`limit_roster_step`]): the text,
/// the label it names and its `{why}`.
type LimitStep<'a> = (&'static NoticeText, Option<&'a str>, Option<&'static str>);

/// The notice a usage limit's tab is told where its login is a roster row's,
/// or whose it is is not known ([`claude_limit_login`]), with the label it
/// names and its `{why}`; `None` where no row is the login's and a sign-in in
/// the tab is the owner's way on ([`RosterWord::Unlisted`], or no word).
/// Another row ready to run now comes first ([`notice::LIMIT_USE`], its
/// `{why}` what holds the tab where it is: `super::turn_end::HeldBy`): the tab
/// is told to go there, never that no listed account can take over
/// (acct3-5). A step whose expectation is a switch — that, or another row's
/// sign-in by name — is told only where one can follow (`switch`,
/// [`limit_switch`]; r3h-2: under `accounts = "off"` the owner ran `accounts
/// use`, which pinned the account, the tab never moved and the notice asked
/// again): where aterm.toml keeps the tab where it is, the lines that let it
/// switch ([`notice::LIMIT_STAY`], naming the row it could switch to); where
/// nothing here relaunches the tab, [`notice::LIMIT_ROSTER`].
///
/// A SWITCH BY ITSELF IS PROMISED ONLY WHERE THE ROSTER MAKES ONE (the
/// review of the login carry, r4h-4): where its own advice holds the tab at
/// this limit even once another row is ready (the row's `held`: a limit from
/// before this Claude Code started, the owner's pin, a limit reset kept), a
/// sign-in by name, and the lines of aterm.toml, are told with an
/// expectation that names the next step instead ([`notice::LIMIT_RENEW_HELD`],
/// [`notice::LIMIT_LOGIN_HELD`], [`notice::LIMIT_STAY_HELD`]) — the sign-in
/// leads to [`notice::LIMIT_USE`]. So are the lines of aterm.toml where no
/// row is ready yet (r5h-2): with them set, the next step is that row's
/// sign-in, not a switch. And where aterm continues nothing past the
/// limit's reset (`resumes` false: `resume_limits` off, at a person's
/// `/login` left at the limit), the roster's two reports say so
/// ([`notice::LIMIT_ROSTER_KEPT`], [`notice::LIMIT_UNREAD_KEPT`]; r4h-3:
/// they said the session continues by itself once an account is ready).
///
/// A READY ROW NOTHING HOLDS THE TAB OFF IS THE ROSTER'S OWN MOVE (`held`
/// `None`; r5h-3): advised, it is that move and no notice stands; where the
/// point was not asked about — an upgrade holding it, or read in the moment
/// before the advice — the move follows once it is, and nothing is told
/// (`Some(None)`): never the limit from before this Claude Code, which this
/// process may well have hit itself, nor a pin of every session that the
/// roster's own move makes needless. A DRAFT in the prompt (`draft`) is no
/// such moment: while it stands the move is never asked, and the tab waits
/// for the reset, days away, beside the ready row — told so, with the step
/// that lets the move follow ([`notice::LIMIT_DRAFT`]; r6h-1): never "send"
/// for aterm's own words, which are cleared ([`notice::LIMIT_DRAFT_OWN`]),
/// nor for a draft under Claude Code's menu, whose Enter runs the menu's
/// entry ([`notice::LIMIT_DRAFT_MENU`]; r7h-4).
///
/// A relaunch onto a ready row the host refused (`refused`,
/// [`TurnEndState::account_refusal`]) is the rotation FAILED, told by what
/// the host's word says of asking again ([`Unmade`], r6h-2): a refusal a
/// second ask can pass is the ready row's `accounts use` naming that cause
/// ([`notice::WHY_USE_UNMADE`]: the step is the accounts' news that ends the
/// refusal, and the move is asked again — told so where the roster holds the
/// tab off too); one of THIS LAUNCH, whatever holds the tab, is
/// [`notice::LIMIT_UNMADE`] — no `accounts use`, which would move every
/// other session and leave this one where it is, and no switch promised of
/// this launch — nor by the sign-in of a row still to sign in, nor by the
/// lines of aterm.toml: that row signed in, or those lines set, the move is
/// asked, refused again, and [`notice::LIMIT_UNMADE`] follows, so the
/// expectation names the next step (`*_HELD`; r7h-2). PURE.
fn limit_roster_step<'a>(
    word: Option<&'a RosterWord>,
    switch: Result<(), Option<&'static str>>,
    resumes: bool,
    refused: Option<Unmade>,
    draft: Option<&Composer>,
) -> Option<Option<LimitStep<'a>>> {
    let (next, ready, held) = match word? {
        RosterWord::Unlisted => return None,
        RosterWord::Unread => {
            let text = if resumes {
                &notice::LIMIT_UNREAD
            } else {
                &notice::LIMIT_UNREAD_KEPT
            };
            return Some(Some((text, None, None)));
        }
        RosterWord::Row { next, ready, held } => (next.as_ref(), ready.as_ref(), *held),
    };
    // A restart of THIS LAUNCH was refused: no switch follows any step here.
    let unmovable = matches!(refused, Some(Unmade::NoShell | Unmade::Launch));
    Some(match (switch, ready, next) {
        (_, None, None) | (Err(None), ..) => Some((
            if resumes {
                &notice::LIMIT_ROSTER
            } else {
                &notice::LIMIT_ROSTER_KEPT
            },
            None,
            None,
        )),
        (Err(Some(lines)), ..) => Some((
            if held.is_none() && ready.is_some() && !unmovable {
                &notice::LIMIT_STAY
            } else {
                &notice::LIMIT_STAY_HELD
            },
            ready
                .map(|ready| ready.label.as_str())
                .or_else(|| next.map(|next| next.label.as_str())),
            Some(lines),
        )),
        (Ok(()), Some(ready), _) => match (held, refused) {
            (_, Some(Unmade::NoShell)) => Some((
                &notice::LIMIT_UNMADE,
                Some(ready.label.as_str()),
                Some(notice::WHY_UNMADE_NO_SHELL),
            )),
            (_, Some(Unmade::Launch)) => Some((
                &notice::LIMIT_UNMADE,
                Some(ready.label.as_str()),
                Some(notice::WHY_USE_UNMADE),
            )),
            (Some(held), _) => Some((
                &notice::LIMIT_USE,
                Some(ready.label.as_str()),
                Some(held.why()),
            )),
            (None, Some(Unmade::Again)) => Some((
                &notice::LIMIT_USE,
                Some(ready.label.as_str()),
                Some(notice::WHY_USE_UNMADE),
            )),
            (None, None) => draft.map(|composer| {
                let text = match composer {
                    Composer::Own => &notice::LIMIT_DRAFT_OWN,
                    Composer::Diverted => &notice::LIMIT_DRAFT_MENU,
                    _ => &notice::LIMIT_DRAFT,
                };
                (text, Some(ready.label.as_str()), None)
            }),
        },
        (Ok(()), None, Some(next)) => Some((
            match (next.renew, held.is_none() && !unmovable) {
                (true, true) => &notice::LIMIT_RENEW,
                (false, true) => &notice::LIMIT_LOGIN,
                (true, false) => &notice::LIMIT_RENEW_HELD,
                (false, false) => &notice::LIMIT_LOGIN_HELD,
            },
            Some(next.label.as_str()),
            None,
        )),
    })
}

/// CLAUDE CODE'S LOST LOGIN, CARRIED BY THE HARNESS UP TO THE SIGN-IN
/// ([`super::login`], the owner's ruling of 2026-10-19): `Some` at a point of
/// such an episode — the wall's row, `/login`'s dialog over the lost login
/// this loop tracks, `Login interrupted` under it, or the row off the screen
/// while its track stands with `/login` typed — with the notice that stands
/// there, `None` inside while aterm is still working on it; `None` for any
/// other point, a `/login` a person opened on a working session included
/// (nothing was lost).
///
/// THE OWNER IS TOLD WHAT HAPPENED, NOT WHAT MIGHT (the owner, 2026-10-19:
/// *"if the auto login and account rotation is working be FAILED, then I
/// need to see that"*): each notice is chosen by what aterm did and decides
/// at this very point ([`decide_turn_end`], pure, on the same reading), so
/// none says `/login` ran where it did not. Nothing is told while aterm still
/// works on its own — `/login` about to be typed or held a moment for a
/// person's keys, the method picker it answers, the success Enter, an
/// account move or its wait — nor over a `/login` a person is going through
/// themselves. Told:
///
/// | point | notice |
/// | --- | --- |
/// | the browser hand-off of aterm's `/login` | [`notice::LOGIN_SIGN_IN`], or [`notice::LOGIN_SIGN_IN_AS`] naming the roster row the session's login is |
/// | any dialog of aterm's `/login` still standing once the login came back in another tab | [`notice::LOGIN_BACK_DIALOG`] |
/// | any dialog of aterm's `/login` still standing once a renewal the tab follows replaced the login | [`notice::LOGIN_RENEWED_DIALOG`] |
/// | the method picker of aterm's `/login`, which it does not answer ([`super::login::picks`]), or Console's menu behind it | [`notice::LOGIN_CHOOSE`] |
/// | an error, an account on hold, anyone's `/login` | [`notice::LOGIN_STOPPED`] |
/// | aterm's `/login` left (`Login interrupted`, or its dialog gone with no login back) | [`notice::LOGIN_CANCELLED`] |
/// | the login back in another tab, this tab's turn delivered again | none |
/// | aterm's `/login` left once a renewal stood ready, the tab moved onto the renewed login | none |
/// | a person's own `/login` left | [`notice::LOGIN_LEFT`] |
/// | `/login` not typed and not about to be | [`notice::LOGIN_NOT_RUN`], with why ([`notice::WHYS`]); [`notice::LOGIN_NOT_RUN_DRAFT`] where a draft holds it |
/// | a renewed login the tab cannot move onto by itself | [`notice::RENEWED_DRAFT`], [`notice::RENEWED_BY_HAND`] |
///
/// On a roster row's login, a notice whose step has the owner type `/login`
/// themselves names the row whose own account to sign in with
/// ([`NoticeText::todo_as`]): nothing at the hand-off of their own `/login`
/// says it.
fn claude_login(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Option<Exception>> {
    if !super::login::carried(reading.program) {
        return None;
    }
    let track = state.wall_attempts(WallKind::Auth);
    let typed = track.is_some_and(|n| n > 0);
    let walled = reading.wall == Some(WallKind::Auth);
    let episode = walled
        || (track.is_some() && reading.login_screen.is_some())
        || (track.is_some() && reading.login_interrupted)
        || (typed && reading.wall.is_none() && !reading.login_back);
    if !episode {
        return None;
    }
    // aterm's own `/login` into this loss, not left: the dialog's steps are
    // aterm's to carry, and what the owner is told says aterm ran it.
    let own = state.login_typed_at().is_some();
    // The roster row the session's login is (the renewable login it runs
    // on, or the account a move put it on): a sign-in in the tab must be
    // that row's own account, which the browser hand-off of aterm's
    // `/login` says, and every step that has the owner type `/login`
    // themselves says in its place ([`NoticeText::todo_as`]: nothing at the
    // hand-off of their own `/login` does).
    let roster = reading
        .login_label
        .as_deref()
        .or_else(|| state.account_move().map(|m| m.to.as_str()));
    let told = |text: &NoticeText, label: Option<&str>, why: Option<&str>| {
        Some(Some(Exception::of_notice(text, label, why, LOGIN_CLEARS)))
    };
    match &reading.login_screen {
        // The login is back; its one Enter is aterm's, and a success that
        // stayed up after it is a login back all the same.
        Some(LoginScreen::Success { .. }) => return Some(None),
        // A person's own `/login`: theirs, as they go through it, Console's
        // menu included (they chose Console there).
        Some(LoginScreen::Picker(_) | LoginScreen::ConsoleMenu | LoginScreen::Browser) if !own => {
            return Some(None);
        }
        // THE LOGIN CAME BACK IN ANOTHER TAB while this dialog stands
        // ([`super::turn_end::login_back_elsewhere`]): nothing is left to
        // sign in, and nobody is told to (the model's
        // `NoSignInAskedOfALoginBack`); the dialog takes no keys of the
        // loop's, so the owner closes it, and the turn is delivered again
        // once it is left.
        Some(_) if super::turn_end::login_back_elsewhere(state, reading) => {
            return told(&notice::LOGIN_BACK_DIALOG, None, None);
        }
        // A RENEWAL MADE WHILE aterm'S OWN `/login` STANDS, one the tab
        // follows by itself ([`super::login::renewal_followed`]): nothing is
        // left to sign in (a browser sign-in would bring back the login the
        // renewal replaced), and nobody is told to (the model's
        // `NoSignInAskedOverARenewal`); the dialog takes no keys of the
        // loop's, so the owner closes it, and the tab moves onto the renewed
        // login once it is left (acct3-3).
        Some(_)
            if own
                && super::login::renewal_followed(
                    reading.login_superseded,
                    cfg,
                    reading.restartable || reading.upgrading,
                ) =>
        {
            return told(
                &notice::LOGIN_RENEWED_DIALOG,
                reading.login_label.as_deref(),
                None,
            );
        }
        // aterm answers its own picker with the method the session already
        // uses where it can say which; any other is the owner's.
        Some(LoginScreen::Picker(picker)) => {
            return match super::login::picks(picker, reading.login_method) {
                Ok(_) => Some(None),
                Err(_) => told(&notice::LOGIN_CHOOSE, None, None),
            };
        }
        // The browser sign-in is open: the one step only the owner can
        // take, as the roster row's own account where the login is one.
        Some(LoginScreen::Browser) => {
            return match roster {
                Some(label) => told(&notice::LOGIN_SIGN_IN_AS, Some(label), None),
                None => told(&notice::LOGIN_SIGN_IN, None, None),
            };
        }
        // Console's own menu behind aterm's `/login` (a person moved the
        // picker onto Console): how to sign in is still the owner's choice,
        // and nothing has stopped (one of its options mints an API key, which
        // aterm never presses).
        Some(LoginScreen::ConsoleMenu) => return told(&notice::LOGIN_CHOOSE, None, None),
        Some(LoginScreen::Failed) => return told(&notice::LOGIN_STOPPED, None, None),
        None => {}
    }
    // The login back in another tab, this tab's turn about to be delivered
    // again ([`super::turn_end::login_back_elsewhere`]): nothing has failed,
    // the carrier's own `/login` left included.
    if super::turn_end::login_back_elsewhere(state, reading) {
        return Some(None);
    }
    // A lost login read before any turn was asked under the login the
    // session's own move AT A WALL chose is the old process's notice, no
    // login gone ([`TurnEndState::login_left`]); right after the session's
    // own account move, or on a login a renewal replaced, it is the move's,
    // answered by the next account (the renewed login) while one is advised
    // ([`login_wall_moves`]), or waited at until one is, where the wait has an
    // end — the instant the host names, the bound's room
    // ([`login_wall_waits`]): nothing is raised for it, a person's own
    // `/login` left before it included (the move signs the session in).
    if state.login_left(reading)
        || login_wall_moves(state, reading, cfg, now).is_some()
        || login_wall_waits(state, reading, cfg, now).is_some()
    {
        return Some(None);
    }
    if state.login_cancelled() || (track.is_some() && reading.login_interrupted) {
        return if typed {
            told(&notice::LOGIN_CANCELLED, roster, None)
        } else {
            told(&notice::LOGIN_LEFT, roster, None)
        };
    }
    if typed {
        // `/login` typed into this loss, and neither its dialog nor the
        // login back on the screen: inside `/login`'s take window its dialog
        // may still be drawn ([`TurnEndState::login_opening`]; the point is
        // decided again at the window's end); past it the dialog was left,
        // and the policy never types `/login` again into this loss.
        return if state.login_opening(now).is_some() {
            Some(None)
        } else {
            told(&notice::LOGIN_CANCELLED, roster, None)
        };
    }
    if !walled {
        // The row off the screen with nothing typed (a person's `/status`):
        // nothing has failed yet.
        return Some(None);
    }
    // A RENEWED login the tab cannot move onto by itself ([`renew_step`]):
    // nothing typed, the owner told what holds it — a draft by what it is:
    // never "send" for aterm's own words, which are cleared, nor for a draft
    // under Claude Code's menu, whose Enter runs the menu's entry (r8h-1, as
    // r7h-4 at a usage limit).
    let label = reading.login_label.as_deref().filter(|_| cfg.logins);
    match renew_step(reading, cfg) {
        Some(RenewStep::Draft) => {
            return Some(Some(Exception::of_notice(
                &draft_notice_text(&notice::RENEWED_DRAFT, &reading.composer),
                label,
                None,
                "the composer empty, or the agent working",
            )));
        }
        Some(RenewStep::ByHand(why)) => return told(&notice::RENEWED_BY_HAND, label, Some(why)),
        None => {}
    }
    // What the point decides of the wall, nothing typed into it yet.
    match decide_turn_end(state, reading, cfg, now) {
        // `/login` about to go, a wait with an end (a person's keys, the
        // quiet after the prompt the wall answered, the breaker), a move or
        // a relaunch: aterm is still at it.
        TurnEndAction::TypeCommand {
            rule_id: RULE_LOGIN,
            ..
        }
        | TurnEndAction::WaitUntil { .. }
        | TurnEndAction::Restart { .. } => Some(None),
        // A screen of the program's own drawn in the composer's place over
        // the wall, no dialog this reader knows: the sign-in stopped on it.
        TurnEndAction::Escalate { .. } if reading.composer == Composer::Absent => {
            told(&notice::LOGIN_STOPPED, None, None)
        }
        // `/login` held back: the owner is told why, and types it — the
        // draft cleared first where one holds it (typed onto the draft,
        // `/login` is no command).
        TurnEndAction::Escalate { .. } if reading.composer.drafted() => {
            told(&notice::LOGIN_NOT_RUN_DRAFT, roster, None)
        }
        TurnEndAction::Escalate { .. } => {
            // The other two that hold it (the wall's own rule under
            // `retry_api_errors = false`, the hour's typing budget); the
            // budget is the last left.
            let why = if cfg.retry_api_errors {
                notice::WHY_BUDGET
            } else {
                notice::WHY_RETRY_OFF
            };
            told(&notice::LOGIN_NOT_RUN, roster, Some(why))
        }
        // Nothing typed into a tab with no task, a landed one, or a
        // person's own exchange left to them (the task's two ends under
        // FOREVER CRUISE / FOREVER LAND).
        TurnEndAction::Nothing if reading.taskless => {
            told(&notice::LOGIN_NOT_RUN, roster, Some(notice::WHY_NO_TASK))
        }
        TurnEndAction::Nothing if state.landed() => told(
            &notice::LOGIN_NOT_RUN,
            roster,
            Some(notice::WHY_TASK_LANDED),
        ),
        TurnEndAction::Nothing if state.task_done() => {
            told(&notice::LOGIN_NOT_RUN, roster, Some(notice::WHY_TURN_YOURS))
        }
        // A point not decided yet (an act still to be taken, a read that is
        // no point), or one no act of the wall's answers: nothing has failed.
        _ => Some(None),
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
    // A lost login only waited at whose row has left the screen (a person's
    // `/status` or `/usage` in the tab) is decided as the wall it left
    // ([`waited_login`]): the tab is still stopped.
    let waited;
    let reading = match waited_login(state, reading, cfg, now) {
        Some(shown) => {
            waited = shown;
            &waited
        }
        None => reading,
    };
    if let Some(told) = claude_login(state, reading, cfg, now) {
        return told;
    }
    if let Some(told) = claude_limit_login(state, reading, cfg, now) {
        return told;
    }
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
            notice: None,
        });
    }
    match kind {
        // A bucket the fallback's relaunch moved the session off, its notice
        // redrawn by the relaunched process ([`TurnEndState::bucket_left`]).
        WallKind::ModelBucket { .. } if state.bucket_left(reading) => None,
        // Any other program's lost login (Claude Code's is [`claude_login`]'s,
        // and only Claude Code moves accounts or waits at a lost login): its
        // own sign-in, told with the loop's escalation (owner = supervisor,
        // design-exceptions 8.4), raised as the point's act.
        WallKind::Auth if attempts > 0 || !cfg.retry_api_errors => {
            let (why, action, clears) = login_text(reading.program, &reading.wall_message);
            Some(Exception {
                kind: ExceptionKind::Login,
                when: Urgency::Now,
                why: why.to_string(),
                action,
                clears,
                escalated: true,
                notice: None,
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
                notice: None,
            })
        }
        // A bucket no reset ends, still unswitched an hour on: the tab's own
        // running jobs hold the relaunch, or there is nowhere to switch to.
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
                notice: None,
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
                notice: None,
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
            Some(Exception::context_of(
                &reading.wall_message,
                state.context_fill(),
            ))
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
