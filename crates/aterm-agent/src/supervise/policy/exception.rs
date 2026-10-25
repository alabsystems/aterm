// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Turn-end exceptions ([`exception_for`]) and [`outlook`](super::outlook::outlook) projections.

use std::time::{Duration, Instant};

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginScreen, Program};

use super::super::background::BackgroundCall;
use super::super::config::SupervisorConfig;
use super::notice::{self, Notice, NoticeText};
use super::outlook::ExceptionKind;
use super::turn_end::{
    Composer, ContextFill, RULE_LOGIN, RosterWord, SAME_REPLY, TurnEndAction, TurnEndReading,
    TurnEndState, Unmade, bucket_model, decide_turn_end, decide_turn_end_traced, login_wall_moves,
    login_wall_waits, renewed_draft_holds, sign_in_how, waited_login,
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
    pub const fn word(self) -> &'static str {
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
    /// The loop's own escalation carries it.
    pub escalated: bool,
    /// The owner's four-part notice ([`super::notice`]).
    pub notice: Option<Notice>,
}

impl Exception {
    /// A notice's exception ([`super::notice`]).
    #[must_use]
    pub fn of_notice(
        text: &NoticeText,
        label: Option<&str>,
        why: Option<&str>,
        clears: &'static str,
    ) -> Self {
        Self::of_notice_reset(text, label, why, None, clears)
    }

    /// [`Self::of_notice`] for a usage limit whose reset is known and past which aterm continues.
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

    /// A limit held unread past its end ([`exception_for`]).
    #[must_use]
    pub fn unread_limit() -> Self {
        Self {
            kind: ExceptionKind::Futile,
            when: Urgency::Later,
            why: "The limit has passed, but this tab's conversation can't be read, so nothing is typed."
                .to_string(),
            action: "Type keep going in this tab if its conversation is the one the limit stopped."
                .to_string(),
            clears: "the conversation read, or the agent working",
            escalated: false,
            notice: None,
        }
    }

    /// A stall no reader names.
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

    /// The same no-work reply to aterm's acts, held ([`TurnEndState::repeats`]).
    #[must_use]
    pub fn repeats() -> Self {
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why: "Its program answers aterm's acts with the same reply and no work, so aterm types nothing more."
                .to_string(),
            action: "Open the tab and do what its last reply asks.".to_string(),
            clears: "a different reply, a person's turn a point saw busy, a lead's new prompt above Claude Code's reply, or the agent working",
            escalated: false,
            notice: None,
        }
    }

    /// A read limit that answered its tries the same, held ([`TurnEndState::repeats`]).
    #[must_use]
    pub fn limit_repeats(program: Program, reset_read: bool) -> Self {
        Self {
            kind: ExceptionKind::Futile,
            when: Urgency::Later,
            why: if reset_read {
                "The limit still stands after its reset and answers each of aterm's tries the same."
            } else {
                "The limit names no reset aterm can read and answers each of aterm's tries the same."
            }
            .to_string(),
            action: futile_action(program).to_string(),
            clears: "a new reset or wall, a person's turn a point saw busy, or the agent working",
            escalated: false,
            notice: None,
        }
    }

    /// The background call that makes no progress.
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

    /// The background work that does not end.
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

    /// A dialog no reader names.
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

    /// Claude Code's own Settings panel, still open at the dialog canary's third rung and opened
    /// by a hand other than the harness's: [`Self::dialog`]'s kind, in words that name the panel.
    #[must_use]
    pub fn settings_panel() -> Self {
        Self {
            why: "Claude Code's Settings panel stays open in this tab, and aterm did not open it."
                .to_string(),
            action: "Press Esc in this tab to close the panel.".to_string(),
            ..Self::dialog()
        }
    }

    /// A conversation no `/compact` can shorten ([`exception_for`]'s full-context arm).
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

    /// [`Self::context`] for the full context described by `message` and `fill`.
    #[must_use]
    pub fn context_of(message: &str, fill: Option<ContextFill>) -> Self {
        let single = message
            .to_lowercase()
            .contains(&aterm_phase::anchor!("wall.cannot_be_compacted").to_lowercase());
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

    /// A request the API refuses the same way each time it is sent.
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

/// Action for a refused request ([`Exception::refused`], [`TurnEndState::notice_typed`]).
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

/// The action for a refused request ([`REFUSED_ACTIONS`]).
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

/// `Goal cleared after an unrecoverable error (<label>)`'s label, from a wall's message.
#[must_use]
pub fn goal_cleared_label(message: &str) -> Option<&str> {
    let head = aterm_phase::anchor!("wall.goal_cleared");
    let at = message.find(head)? + head.len();
    let rest = message[at..].trim_start().strip_prefix('(')?;
    rest.split(')').next().filter(|l| !l.is_empty())
}

/// Tries at a wall after its reset that the wall answered before it is called futile.
pub const FUTILE_AFTER_TRIES: usize = 3;
/// `/compact`s at a full context.
pub const CONTEXT_EXHAUSTED_AFTER: usize = 3;
/// And how long past the reset.
pub const FUTILE_AFTER: Duration = Duration::from_secs(60 * 60);
/// How long a bucket no reset ends stands, its fallback not made, before it is an exception.
pub const BUCKET_STUCK_AFTER: Duration = Duration::from_secs(60 * 60);

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

fn title(alias: &str) -> String {
    let mut c = alias.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

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
        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => (
            "Its account is out of credits or at a spend limit, and no time window ends that.",
            "Do what the program's notice in this tab says to lift the account's limit.",
        ),
    }
}

const fn futile_action(program: Program) -> &'static str {
    match program {
        Program::Claude => "Check claude.ai/settings/usage for what holds it.",
        Program::Agy => "Check agy's account usage for what holds it.",
        Program::Muse => {
            "Run /upgrade in this tab for increased limits, as Muse Code's usage wall says."
        }
        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => {
            "Check the program's account usage for what holds it."
        }
    }
}

/// Lost-login details for `program` ([`super::login::carried`], [`claude_login`]).
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
        Program::Claude | Program::Codex | Program::Grok | Program::Kimi | Program::Generic => (
            "Its login is gone, and only your own sign-in restores it.",
            "Sign the program in again as its notice in this tab says.".to_string(),
            "the agent working",
        ),
    }
}

const LOGIN_CLEARS: &str = "`Login successful` on the screen, or the agent working";

const fn draft_notice_text(text: &NoticeText, composer: &Composer) -> NoticeText {
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

/// Whether a prompt draft is the only thing holding back a ready Claude account switch.
#[must_use]
pub(crate) fn draft_holds_limit_switch(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
) -> bool {
    if !super::login::carried(reading.program)
        || state.wall_attempts(WallKind::UsageWeekly).is_none()
        || reading.advised().is_some()
        || reading.taskless
        || state.task_done()
    {
        return false;
    }
    let step = limit_roster_step(
        reading.roster.as_ref(),
        limit_switch(reading, cfg),
        cfg.resume_limits,
        state.account_refusal(),
        Some(&reading.composer).filter(|c| c.drafted() && !reading.upgrading),
    );
    matches!(
        step,
        Some(Some((text, _, _))) if notice::DRAFT_HOLDS.contains(&text.id)
    )
}

/// Exception when the owner is in `/login` at a usage limit ([`notice::LOGIN_STOPPED`]).
fn claude_limit_login(
    state: &TurnEndState,
    reading: &TurnEndReading,
    _cfg: &SupervisorConfig,
    _now: Instant,
) -> Option<Option<Exception>> {
    if !super::login::carried(reading.program)
        || state.wall_attempts(WallKind::UsageWeekly).is_none()
        || reading.advised().is_some()
        || reading.taskless
        || state.task_done()
    {
        return None;
    }
    match &reading.login_screen {
        Some(
            LoginScreen::Success { .. }
            | LoginScreen::Picker(_)
            | LoginScreen::ConsoleMenu
            | LoginScreen::Browser,
        ) => Some(None),
        Some(LoginScreen::Failed) => Some(Some(Exception::of_notice(
            &notice::LOGIN_STOPPED,
            None,
            None,
            LOGIN_CLEARS,
        ))),
        None => None,
    }
}

/// Whether another account can take over ([`limit_roster_step`], [`notice::stay_why`]).
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

/// A notice a usage limit's tab is told ([`limit_roster_step`]).
type LimitStep<'a> = (&'static NoticeText, Option<&'a str>, Option<&'static str>);

/// Notice for a usage-limit tab on a roster row ([`claude_limit_login`], [`RosterWord::Unlisted`]).
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
        // The next row to sign in asks no one: aterm's sign-in ladder signs a row with an email
        // in first, as a walled session's successor (`crate::harness::accounts_sync`'s tier 0),
        // and the tab is decided again at the accounts' news; where no route can, the routes it
        // tried are on record, as a signed-out row with no email is.
        (Ok(()), None, Some(_)) => None,
    })
}

/// CLAUDE CODE'S LOST LOGIN ([`super::login`]).
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
    let own = state.login_typed_at().is_some();
    let roster = reading
        .login_label
        .as_deref()
        .or_else(|| state.account_move().map(|m| m.to.as_str()));
    let told = |text: &NoticeText, label: Option<&str>, why: Option<&str>| {
        Some(Some(Exception::of_notice(text, label, why, LOGIN_CLEARS)))
    };
    match &reading.login_screen {
        Some(LoginScreen::Success { .. }) => return Some(None),
        // The dialog of the `/login` aterm typed whose sign-in nobody needs (a roster row's
        // login, one back in another tab, or one a followed renewal replaced): aterm backs out of
        // it by itself and the tab goes on (it waits on the row's sign-in, continues its turn, or
        // relaunches onto the renewed login), so nobody is told to sign in there or to close it.
        Some(screen)
            if super::login::backs_out(state, reading, cfg, screen, now)
                || state.login_closing(now).is_some() =>
        {
            return Some(None);
        }
        // A person's own dialog (one drawn after the dialog aterm backed out of was read gone
        // included): theirs, told nothing.
        Some(LoginScreen::Picker(_) | LoginScreen::ConsoleMenu | LoginScreen::Browser) if !own => {
            return Some(None);
        }
        // A dialog a person is in whose login came back in another tab, which aterm presses
        // nothing in: Console's menu, which only a person's pick at aterm's picker opens, or the
        // message a person's own `/login` stopped on. Every other dialog of aterm's own `/login`
        // over a login back elsewhere is backed out of above.
        Some(_) if super::turn_end::login_back_elsewhere(state, reading) => {
            return told(&notice::LOGIN_BACK_DIALOG, None, None);
        }
        // aterm's own `/login` whose sign-in nobody needs, at a state aterm presses nothing in
        // (Console's menu, which only a person's pick at aterm's picker opens): no person is told
        // to sign the login in there either.
        Some(_) if own && super::login::nobody_signs_in(state, reading, cfg) => return Some(None),
        Some(LoginScreen::Picker(picker)) => {
            return match super::login::picks(picker, reading.login_method) {
                Ok(_) => Some(None),
                Err(_) => told(&notice::LOGIN_CHOOSE, None, None),
            };
        }
        Some(LoginScreen::Browser) => {
            return match roster {
                Some(label) => told(&notice::LOGIN_SIGN_IN_AS, Some(label), None),
                None => told(&notice::LOGIN_SIGN_IN, None, None),
            };
        }
        Some(LoginScreen::ConsoleMenu) => return told(&notice::LOGIN_CHOOSE, None, None),
        Some(LoginScreen::Failed) => return told(&notice::LOGIN_STOPPED, None, None),
        None => {}
    }
    if super::turn_end::login_back_elsewhere(state, reading) {
        return Some(None);
    }
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
        return if state.login_opening(now).is_some() {
            Some(None)
        } else {
            told(&notice::LOGIN_CANCELLED, roster, None)
        };
    }
    if !walled {
        return Some(None);
    }
    if renewed_draft_holds(reading, cfg) {
        return Some(Some(Exception::of_notice(
            &draft_notice_text(&notice::RENEWED_DRAFT, &reading.composer),
            reading.login_label.as_deref(),
            None,
            "the composer empty, or the agent working",
        )));
    }
    match decide_turn_end(state, reading, cfg, now) {
        TurnEndAction::TypeCommand {
            rule_id: RULE_LOGIN,
            ..
        }
        | TurnEndAction::WaitUntil { .. }
        | TurnEndAction::Restart { .. } => Some(None),
        TurnEndAction::Escalate { .. } if reading.composer == Composer::Absent => {
            told(&notice::LOGIN_STOPPED, None, None)
        }
        TurnEndAction::Escalate { .. } if reading.composer.drafted() => {
            told(&notice::LOGIN_NOT_RUN_DRAFT, roster, None)
        }
        TurnEndAction::Escalate { .. } => {
            let why = if cfg.retry_api_errors {
                notice::WHY_BUDGET
            } else {
                notice::WHY_RETRY_OFF
            };
            told(&notice::LOGIN_NOT_RUN, roster, Some(why))
        }
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
        _ => Some(None),
    }
}

/// The exception that stands at this point.
#[must_use]
pub fn exception_for(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<Exception> {
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
    if state
        .held_unread_from(reading)
        .is_some_and(|from| now.saturating_duration_since(from) >= FUTILE_AFTER)
    {
        return Some(Exception::unread_limit());
    }
    let held = state.replies().holds() && {
        let mut steps = Vec::new();
        let _ = decide_turn_end_traced(state, reading, cfg, now, &mut steps);
        super::trace::deciding(&steps).is_some_and(|s| s.check == SAME_REPLY)
    };
    let Some(kind) = reading.wall else {
        // Held at the screen's reply, or at the wall a track still stands for: a limit's, or another's.
        return held.then(|| {
            if state.repeats(reading) || !state.has_limit_track() {
                Exception::repeats()
            } else {
                Exception::limit_repeats(reading.program, state.waits_on_named_reset())
            }
        });
    };
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
        WallKind::ModelBucket { .. } if state.bucket_left(reading) => None,
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
                clears: "a new reset or wall, a person's turn a point saw busy, or the agent working",
                escalated: false,
                notice: None,
            })
        }
        WallKind::Context
            if attempts >= CONTEXT_EXHAUSTED_AFTER && state.compaction_exhausted() =>
        {
            Some(Exception::context_of(
                &reading.wall_message,
                state.context_fill(),
            ))
        }
        WallKind::ApiError {
            cause: aterm_phase::ApiCause::Request,
            ..
        } if cfg.retry_api_errors && attempts >= FUTILE_AFTER_TRIES => Some(Exception::refused(
            state.notice_typed(),
            &reading.wall_message,
        )),
        _ => held.then(|| match kind {
            WallKind::UsageSession
            | WallKind::UsageWeekly
            | WallKind::Spend
            | WallKind::ModelBucket { .. } => {
                Exception::limit_repeats(reading.program, reading.reset_at.is_some())
            }
            _ => Exception::repeats(),
        }),
    }
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;
