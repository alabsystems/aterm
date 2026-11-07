// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Turn-end exceptions ([`exception_for`]) and [`outlook`](super::outlook::outlook) projections.

use std::time::{Duration, Instant};

use aterm_phase::wall::WallKind;
use aterm_phase::{LoginScreen, Program};

use super::super::background::BackgroundCall;
use super::super::config::{Autopilot, SupervisorConfig};
use super::notice::{self, Notice, NoticeText};
use super::outlook::ExceptionKind;
use super::turn_end::{
    Composer, ContextFill, Diversion, GO_AHEAD_REFUSED, RULE_LOGIN, RosterWord, SAME_REPLY,
    TurnEndAction, TurnEndReading, TurnEndState, Unmade, bucket_model, decide_turn_end,
    decide_turn_end_traced, login_wall_moves, login_wall_waits, renewed_draft_holds, sign_in_how,
    waited_login,
};
use crate::harness::acts::HeldBy;

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

/// What clears [`Exception::login_over_limit`].
pub const LOGIN_OVER_LIMIT_CLEARS: &str = "the sign-in closed, or the agent working";

/// The recorded reason for recovering from a refused standing decision.
const REFUSES_GO_AHEAD: &str = "The agent repeated a request; recovery will change approach.";

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
    /// The repeated-reply classification, or `None` for another exception.
    pub held: Option<HeldBy>,
}

/// What a person's turn lifts a hold by, in its clears ([`Exception::repeats_by`]).
macro_rules! busy_turn {
    () => {
        "a person's turn a point saw busy"
    };
}

/// What else lifts a hold, after the person's turn ([`Exception::repeats_by`]).
macro_rules! held_clears_tail {
    () => {
        ", a lead's new prompt above Claude Code's reply, or the agent working"
    };
}

/// The person's switch to Land, beside their turn, where it asks once more (v6 §14.17 item 3).
macro_rules! their_switch {
    () => {
        " or their switch to Land"
    };
}

const SAME_REPLY_CLEARS: &str = concat!("a different reply, ", busy_turn!(), held_clears_tail!());
const SAME_REPLY_SWITCH_CLEARS: &str = concat!(
    "a different reply, ",
    busy_turn!(),
    their_switch!(),
    held_clears_tail!()
);
const NO_WORK_CLEARS: &str = concat!(busy_turn!(), held_clears_tail!());
const NO_WORK_SWITCH_CLEARS: &str = concat!(busy_turn!(), their_switch!(), held_clears_tail!());

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
            held: None,
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
            held: None,
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
            held: None,
        }
    }

    /// The same no-work reply to aterm's acts, held ([`TurnEndState::repeats`]), by `held`'s arm
    /// ([`TurnEndState::held_by`]): a fact for later (limit 3 of §14.0).
    /// The same-reply arm's why keeps no count, because a read wall's spent tries hold by it too
    /// ([`super::turn_end::Kept::holds`]); the no-work arm's names its count ([`HeldBy::answered`]),
    /// as no reply came again, and a different reply does not lift it.
    #[must_use]
    pub fn repeats(held: HeldBy) -> Self {
        Self::repeats_by(held, false)
    }

    /// [`Self::repeats`], and where `switch`, the person's switch to Land is among what clears it:
    /// it counts as their turn, and Land asks once more (v6 §14.17 item 3,
    /// [`TurnEndState::switch_would_ask`]). The stop's words on the glass name the switch from
    /// this one fact ([`Self::lifted_by_switch`]).
    #[must_use]
    pub fn repeats_by(held: HeldBy, switch: bool) -> Self {
        let (why, clears) = match held {
            HeldBy::SameReply => (
                "Its program answers aterm's acts with the same reply and no work, so aterm types nothing more."
                    .to_string(),
                if switch {
                    SAME_REPLY_SWITCH_CLEARS
                } else {
                    SAME_REPLY_CLEARS
                },
            ),
            HeldBy::NoWork => (
                format!(
                    "Its program answered aterm's acts {}, so aterm types nothing more.",
                    held.answered()
                ),
                if switch {
                    NO_WORK_SWITCH_CLEARS
                } else {
                    NO_WORK_CLEARS
                },
            ),
        };
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why,
            action: "Revisit the recorded work and continue available tasks.".to_string(),
            clears,
            escalated: false,
            notice: None,
            held: Some(held),
        }
    }

    /// WHETHER THE PERSON'S SWITCH TO LAND CLEARS THIS HOLD ([`Self::repeats_by`]): what the
    /// stop's words on the glass read to name the switch (`aterm_agent::harness::acts::stop_words`).
    #[must_use]
    pub fn lifted_by_switch(&self) -> bool {
        self.held.is_some()
            && matches!(
                self.clears,
                SAME_REPLY_SWITCH_CLEARS | NO_WORK_SWITCH_CLEARS
            )
    }

    /// A refused standing decision is recorded while the harness recovers autonomously.
    #[must_use]
    pub fn refuses_go_ahead() -> Self {
        Self {
            kind: ExceptionKind::Stalled,
            when: Urgency::Later,
            why: REFUSES_GO_AHEAD.to_string(),
            action: "Choose another approach and continue available work.".to_string(),
            clears: "the agent working outside a break of its own background work, a reply that \
                     no longer waits on a go-ahead, or a person typing in the tab",
            escalated: false,
            notice: None,
            held: None,
        }
    }

    /// Whether this is the go-ahead's refusal ([`Self::refuses_go_ahead`]), which puts the lines
    /// the go-ahead answered back on the menu bar ([`super::turn_end::GoAheadVerdict::Refused`]).
    #[must_use]
    pub fn refuses(&self) -> bool {
        self.why == REFUSES_GO_AHEAD
    }

    /// Whether this is [`Self::login_over_limit`]: the person told now to close their own
    /// `/login` standing over a usage limit, which is how the loop answers that limit until they
    /// do, so no second notice tells the limit as unhandled (`Session::limit_after`).
    #[must_use]
    pub fn tells_login_over_limit(&self) -> bool {
        self.kind == ExceptionKind::Login && self.clears == LOGIN_OVER_LIMIT_CLEARS
    }

    /// Whether the agent working on a busy screen ends this exception, `at_break` a break of its
    /// own background work: every exception ends so but the refusal of the standing go-ahead
    /// ([`Self::refuses_go_ahead`]) at such a break. There, where the carry can go (Cruise, the
    /// continuation on, an unattended run, a window host), answers are on and no continuation a
    /// `/compact` owes goes first, a park still spent records the refusal
    /// (`recovering:go-ahead-refused`, [`super::turn_end::GO_AHEAD_REFUSED`];
    /// `Session::break_go_ahead`) unless the window's host withholds the session's decisions,
    /// where no refusal is recorded and nothing is typed; the carry's change of approach goes in
    /// the go-ahead's place where its ladder, its budget and the write's guards let it
    /// (`Session::cruise_carry`).
    #[must_use]
    pub fn ends_at_work(&self, at_break: bool) -> bool {
        !(at_break && self.why == REFUSES_GO_AHEAD)
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
            held: None,
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
            held: None,
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
            held: None,
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
            held: None,
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

    /// Claude Code's own `/login` dialog, still open at the dialog canary's third rung:
    /// [`Self::dialog`]'s kind, in words that name the sign-in, which a reader here reads
    /// ([`aterm_phase::login_screen`]).
    #[must_use]
    pub fn login_dialog() -> Self {
        Self {
            why: "A Claude Code sign-in has stood open in this tab, and only you can finish it."
                .to_string(),
            action: "Press Esc in this tab to close the sign-in.".to_string(),
            ..Self::dialog()
        }
    }

    /// A PERSON'S OWN `/login` STILL DRAWN OVER A USAGE LIMIT WHOSE ACT IS DUE after aterm's Escs
    /// at it left it standing ([`super::login::over_limit_unclosed`]): the dialog covers the
    /// composer the limit's resume, and Claude Code's own continue, would reach, and one key of the
    /// person's closes it, so it is told now.
    #[must_use]
    pub fn login_over_limit() -> Self {
        Self {
            kind: ExceptionKind::Login,
            when: Urgency::Now,
            why: "A sign-in left open in this tab keeps its usage limit's reset from resuming it."
                .to_string(),
            action: "Press Esc in this tab to close the sign-in.".to_string(),
            clears: LOGIN_OVER_LIMIT_CLEARS,
            escalated: false,
            notice: None,
            held: None,
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
            held: None,
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

    /// A request the API refuses the same way each time it is sent, told of Claude Code
    /// ([`Self::refused_by`]).
    #[must_use]
    pub fn refused(typed: Option<&str>, message: &str) -> Self {
        Self::refused_by(Program::Claude, typed, message)
    }

    /// A request the API `program` calls refuses the same way each time it is sent: the why and
    /// the action name `program`, and only Claude Code's refusals take Claude Code's remedies
    /// ([`refused_action`]).
    #[must_use]
    pub fn refused_by(program: Program, typed: Option<&str>, message: &str) -> Self {
        let (why, action) = match program {
            Program::Claude => (
                "Claude's API refuses this request the same way each time it is sent.".to_string(),
                refused_action(typed, message).to_string(),
            ),
            other @ (Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Grok
            | Program::Kimi
            | Program::Generic) => (
                format!(
                    "The API {} calls refuses this request the same way each time it is sent.",
                    other.product()
                ),
                format!(
                    "Open the tab and do what {}'s last error asks.",
                    other.product()
                ),
            ),
        };
        Self {
            kind: ExceptionKind::Refused,
            when: Urgency::Later,
            why,
            action,
            clears: "the agent working",
            escalated: false,
            notice: None,
            held: None,
        }
    }
}

/// Action for a request Claude Code's API refused ([`Exception::refused_by`],
/// [`TurnEndState::notice_typed`]): each remedy is Claude Code's own.
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

/// What clears a login exception when aterm's own Esc left the `/login` it typed at its method
/// picker ([`super::turn_end::TurnEndState::login_backed_out_since`]): the `CLEARED` line names
/// that back-out, not [`LOGIN_CLEARS`].
pub const LOGIN_BACKED_OUT: &str =
    "aterm backed out of the /login it typed (its dialog's Esc, no method picked)";

/// What clears a login exception when the turn end backs out of the `/login` aterm typed past
/// its picker (the browser hand-off, or the dialog's error: a method was picked there), the
/// login's sign-in now nobody's to make ([`super::login::backs_out`]): the `CLEARED` line,
/// written as that decision's Esc goes in, names the back-out, not [`LOGIN_CLEARS`].
pub const LOGIN_BACKING_OUT: &str = "aterm backs out of the /login it typed (its dialog's Esc)";

const fn draft_notice_text(text: &NoticeText, composer: &Composer) -> NoticeText {
    NoticeText {
        todo: match composer {
            Composer::Diverted(by) => by.todo(),
            Composer::Own => "Clear aterm's unsent text in the tab.",
            Composer::Command(..) => notice::COMMAND_TODO,
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

/// Exception when the owner is in `/login` at a usage limit ([`notice::LOGIN_STOPPED`]): their
/// sign-in under way ends the chain with none, and one that failed is told. A managed worker's
/// tab follows the same rule while it waits for quota.
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
                    Composer::Diverted(_) => &notice::LIMIT_DRAFT_MENU,
                    Composer::Command(..) => &notice::LIMIT_DRAFT_COMMAND,
                    _ => &notice::LIMIT_DRAFT,
                };
                (text, Some(ready.label.as_str()), None)
            }),
        },
        // A row still to sign in raises no notice: nothing here signs in a row no session here
        // runs on or ran on within a week (`crate::harness::sign_in_ladder`), a report a person
        // reads names its sign-in, and the tab is decided again at the accounts' news or resumes
        // at the reset.
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
    // A pinned login whose notice left the screen before its point is told as on screen: the
    // point was decided off the track, `/login` left to the person.
    let left = state.pinned_login_left(reading);
    let reading = left.as_ref().unwrap_or(reading);
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
        // aterm's own picker: never answered by aterm (its pick opens the browser, which only a
        // person brings forward), so the choice is told, quietly, once.
        Some(LoginScreen::Picker(_)) => return told(&notice::LOGIN_CHOOSE, None, None),
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
    let not_run = if reading.composer == Composer::Diverted(Diversion::Search) {
        &LOGIN_NOT_RUN_SEARCH
    } else {
        &notice::LOGIN_NOT_RUN
    };
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
        // What stands in or over the composer asks its own question first (`Composer::asks`),
        // decided again once it goes: the login is told then, not as a draft that is none.
        TurnEndAction::Escalate { reason } if reading.composer.asks() == Some(reason.as_str()) => {
            Some(None)
        }
        // Text standing in the composer, which the person clears before `/login`: theirs (a menu
        // open over it or not), aterm's own, or an old prompt the search left. The search over
        // the composer is no draft: what held `/login` back (the budget, `retry_api_errors` off)
        // is told as over an empty composer, the search closed first.
        TurnEndAction::Escalate { .. }
            if reading.composer.drafted()
                && reading.composer != Composer::Diverted(Diversion::Search) =>
        {
            told(&notice::LOGIN_NOT_RUN_DRAFT, roster, None)
        }
        // Over the vault token, `/login` replaces nothing: the way back is a new token.
        TurnEndAction::Escalate { .. }
            if cfg.retry_api_errors && reading.login_pin == super::login::LoginPin::Token =>
        {
            told(&notice::LOGIN_TOKEN_REFUSED, roster, None)
        }
        TurnEndAction::Escalate { .. } => {
            let why = if !cfg.retry_api_errors {
                notice::WHY_RETRY_OFF
            } else if let Some(why) = reading.login_pin.why() {
                why
            } else {
                notice::WHY_BUDGET
            };
            told(not_run, roster, Some(why))
        }
        TurnEndAction::Nothing if reading.taskless => {
            told(not_run, roster, Some(notice::WHY_NO_TASK))
        }
        TurnEndAction::Nothing if state.landed() => {
            told(not_run, roster, Some(notice::WHY_TASK_LANDED))
        }
        TurnEndAction::Nothing if state.task_done() => {
            told(not_run, roster, Some(notice::WHY_TURN_YOURS))
        }
        _ => Some(None),
    }
}

/// [`notice::LOGIN_NOT_RUN`] with the prompt-history search open over the composer, whose query a
/// typed `/login` would go into (`super::turn_end::SEARCH_DIVERTS`): the search is closed first.
const LOGIN_NOT_RUN_SEARCH: NoticeText = NoticeText {
    todo: "Close the prompt-history search in the tab, then type /login and sign in again.",
    todo_as: Some("Sign in to {label}: close the prompt-history search, then type /login."),
    ..notice::LOGIN_NOT_RUN
};

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
    if super::login::over_limit_unclosed(state, reading, cfg, now) {
        return Some(Exception::login_over_limit());
    }
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
    let spent = state.go_ahead_owed(reading, now).is_some() && state.go_ahead_spent(now);
    let decided = (state.replies().holds() || spent).then(|| {
        let mut steps = Vec::new();
        let _ = decide_turn_end_traced(state, reading, cfg, now, &mut steps);
        super::trace::deciding(&steps).map(|s| s.check)
    });
    if decided.flatten() == Some(GO_AHEAD_REFUSED) {
        return Some(Exception::refuses_go_ahead());
    }
    let held = state.replies().holds() && decided.flatten() == Some(SAME_REPLY);
    if held
        && reading.autopilot == Autopilot::Cruise
        && reading.wall.is_none()
        && !state.has_limit_track()
        && state.replies().last().is_some_and(|reply| !reply.walled)
    {
        // Recovery is scheduled and recorded in the ledger, not delegated to the person.
        return None;
    }
    let Some(kind) = reading.wall else {
        // Held at the screen's reply, or at the wall a track still stands for: a limit's, or another's.
        return held.then(|| {
            if state.repeats(reading) || !state.has_limit_track() {
                held_repeats(state, reading, cfg, now)
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
            held: None,
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
                held: None,
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
                held: None,
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
                held: None,
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
                held: None,
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
        } if cfg.retry_api_errors && attempts >= FUTILE_AFTER_TRIES => Some(Exception::refused_by(
            reading.program,
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
            _ => Exception::repeats(state.held_by().unwrap_or(HeldBy::SameReply)),
        }),
    }
}

/// Classify a held report for the journal without raising an alert.
fn held_repeats(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Exception {
    let held = state.held_by().unwrap_or(HeldBy::SameReply);
    if cfg.answer_questions && state.go_ahead_parked(reading, now).is_some() {
        return Exception {
            held: Some(held),
            ..Exception::refuses_go_ahead()
        };
    }
    Exception::repeats_by(held, state.switch_would_ask(reading))
}

#[cfg(test)]
#[path = "exception_tests.rs"]
mod tests;
