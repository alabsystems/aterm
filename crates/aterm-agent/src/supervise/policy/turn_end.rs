// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The turn-end decider: what the supervisor does when a worker's turn ends —
//! nothing, wait, type a continuation, accept the worker's suggestion, type a
//! slash command, or hand the point to a person. [`decide_turn_end`] is a pure
//! function of the screen ([`TurnEndReading`], from `aterm-phase`) and this
//! policy's memory of the session ([`TurnEndState`]); the loop
//! (`supervise/turn_end_loop.rs`) executes its result and reports back through
//! [`TurnEndState::acted`] and [`TurnEndState::observe`]. Every switch is the
//! `[harness]` table ([`SupervisorConfig`]).
//!
//! The default is fully automatic: every point gets an answer, and a person is
//! told only of what the table limited or what cannot be answered.
//!
//! * **Continue.** At an authoritative idle point, or an offer (`want me
//!   to…`), with no box, wall or draft: accept the worker's suggestion when it
//!   is a continuation ([`suggestion_allowed`]), else type
//!   `cfg.continue_text` (plus the standing rules when a rules file is set).
//! * **Answer** ([`RULE_ANSWER`]). A question, stop phrase ([`STOP_PHRASES`])
//!   or choice gets `cfg.answer_text`, or the question-priority word's policy
//!   text ([`prose_answer`], [`is_answer_rule`]). One naming an irreversible
//!   act ([`DESTRUCTIVE_WORDS`], [`Said::Irreversible`]) gets
//!   [`REVERSIBLE_ANSWER`] whatever the priority. Under `answer_questions =
//!   false` the point is escalated.
//! * **Back-off.** A turn with less than [`TurnEndTiming::min_work`] of busy
//!   work, the first point seen, and a continuation's yield that says done
//!   ([`says_done`]) are short. Each short turn in a row doubles the wait from
//!   [`TurnEndTiming::short_backoff`] to [`TurnEndTiming::short_backoff_max`];
//!   a turn of real work ends the streak. Turns the host typed (upgrade notice,
//!   carry-on) leave the streak as it was ([`TurnEndState::host_typed`]).
//!   `cfg.continue_per_hour` caps continuations (`0` = no cap).
//! * **Done check** ([`RULE_DONE_CHECK`]). A point that says done gets
//!   [`DONE_CHECK`] once; a last line of [`FINISHED`] ends the task at once
//!   ([`says_finished`], [`RULE_FINISHED`]), as does a second done reply.
//!   Nothing is typed afterwards ([`TurnEndState::task_done`]) until someone
//!   else's turn reopens it.
//! * **No task, no turn end.** A session nobody has asked anything
//!   ([`TurnEndReading::taskless`]; the harness's own turns are no task) gets
//!   no act and starts no streak.
//! * **A person wins.** Nothing is typed within `cfg.human_grace_s` of a
//!   person's last keystroke ([`TurnEndReading::person`]); a turn they stopped
//!   with Esc is continued only after that grace.
//! * **A standing draft is sent** ([`TurnEndAction::Submit`]) under the rule
//!   of the act it replaces, once it has not changed for the grace.
//! * **An act not taken** within [`TurnEndTiming::take_within`] counts as a
//!   short yield and the next act goes on the back-off.
//!
//! **Walls** ([`aterm_phase::wall`]). An API error is answered by its cause
//! ([`aterm_phase::ApiCause`]), never by an ask. Each wait is a `WAITING`
//! journal row, and a retry wait's deadline ([`is_retry_wait`]) is shown on
//! the tab (`IdleHost::waiting`):
//! * **network unreachable** — continued once the host measures the API
//!   reachable ([`Reach::Up`]), else on [`TurnEndTiming::net_backoff`]; while
//!   measured down ([`Reach::Down`]) nothing is typed for up to
//!   [`TurnEndTiming::down_hold`];
//! * **certificate or proxy refused** — the same ladder, but `Up` is no
//!   evidence (the host and agent use different trust stores);
//! * **reply cut off** — continued at once, then on the same ladder;
//! * **server failure** (status, overload) — [`TurnEndTiming::retry_backoff`]
//!   from each appearance.
//!
//! Each act names the wall's kind to the worker in aterm's own words
//! ([`wall_retry_text`]: `Claude Code reported an API error (overloaded)`)
//! rather than the rote continuation — never the screen's words. Ruling
//! 2026-10-08 (H6-HARNESS-1): the act QUOTED the rows the reader took for the
//! wall, and a tool's output read as the vendor's row (an MCP result, a `!`
//! command's) was typed back as a USER turn, untrusted text carrying the
//! person's authority — and `/login`, `/compact`, a wait for a reset or an
//! account move followed from it. So no act types screen text whatever the
//! reader says, and THIS DECIDER IS HANDED ONLY A CONFIRMED WALL: the screen's
//! reader reads for recall ([`aterm_phase::wall`]: it cannot tell an MCP
//! result's second block from a wall under the call), and the loop keeps a
//! Claude Code wall in the reading only where the session's own transcript
//! ends on Claude Code's notice row of that kind (`supervise/wall_confirm.rs`,
//! before the reading reaches [`decide_turn_end`], which stays pure); a wall
//! it does not confirm is journaled and the point decided as an ordinary turn
//! end. With no transcript to read, a wall read under a tool's call
//! ([`aterm_phase::Wall::under_call`], the whole screen's rows walked) is
//! refused the same way, and any other is the screen's to decide, as on
//! main — which did act on walls under MCP and `!` calls (it did not know
//! them as calls): a loss of recall where no transcript can be found,
//! ruled 2026-10-08 to refuse the MCP and hook spoofs there
//! (`supervise/wall_confirm.rs`). The check runs at the reads a point is
//! decided on; the loop's re-reads of a decided point for its own books
//! keep that decision's verdict. A retry that led to real work
//! ([`TurnEndTiming::progress`]) ends the episode: the next wall starts its
//! ladder again. A usage window or a spend limit: waited out, handled and
//! nobody told, when the vendor says it goes on by itself (`continuing
//! automatically at …`) — continued only if it has not gone on a
//! [`TurnEndTiming::reset_grace`] past its reset — and otherwise a usage
//! window or a spend limit is continued a [`TurnEndTiming::reset_grace`]
//! past the reset it names — never anything bought — and after a
//! continuation that hit the wall again, [`TurnEndTiming::limit_backoff`].
//! Its notice leaving the screen with no work since (Esc on the vendor's own
//! auto-continue, a person's local command) moves none of that: the reset it
//! named is waited out all the same ([`still_shown`]). A turn SERVED since
//! THAT notice that no read saw busy — a person's, the limit lifted before
//! its reset; the screen draws its prompt and reply under the notice that
//! names this limit's reset ([`TurnEndReading::since_limit`]), or the
//! session's transcript holds the model's reply after this limit's own
//! notice row ([`TurnEndReading::served_in_transcript`]) — is work since,
//! and ends the wait as work seen busy does; a turn under ANOTHER limit's
//! notice (a `/resume`d conversation's history) is not. A notice cancelled
//! BEFORE ANY READ — the limit answered a turn at once (an act of this
//! policy's own, a person's prompt), a person's Esc on the countdown came
//! first, and the screen shows only `⏺ Automatic continue cancelled` under
//! it — is that turn's answer ([`TurnEndState::left_answer`]: the cancel
//! right under a notice drawn since the act, or since the policy last looked,
//! nothing served since), read by the loop in the screen's place as the wall
//! it was and waited on as any. The loop dates it by the session's
//! transcript (its notice row written since that turn was asked: a
//! `/resume`d conversation ends on its own older cancelled limit), and where
//! the screen cannot tell it from the notice the policy already saw (the
//! same limit answering again, the same reset) only that row can; with no
//! transcript the screen's new notice is waited on, never more than a
//! session window out (`Session::wall_reset_at`). A
//! model bucket (owner decision 3): the agent RELAUNCHED on the fallback
//! model ([`Restart::Model`], `--model <fallback>` on the relaunch line,
//! session-only; asked of the host again and again until it is made, and at
//! a NAMED reset the continuation goes first), and relaunched on the
//! bucket's model again at the reset the notice NAMED and that is still ahead
//! ([`Restart::ModelBack`]; a bucket that names none is an entitlement: the
//! session stays on the fallback, and a return the host cannot make yet never
//! holds a point) — never Claude's own `/model`,
//! which also saves the person's default for every new session (measured
//! on 2.1.282; D7 of the reconciliation of 2026-09-25). With no fallback,
//! once switched, or where no host relaunches it (`drive watch`), the reset
//! is waited out. A bucket that asks
//! CONSENT to go on on usage credits is accepted (owner, 2026-09-24: *"a
//! model/extra-usage consent dialog → accept"*): continued, so the vendor's
//! confirm comes up as a box, which the approval policy answers with its
//! yes — and only when that continuation hits the bucket again is the model
//! switched as above. Nothing is ever BOUGHT (the approval policy's
//! `buys`). A full context: `/compact`, then continue (again on the retry
//! ladder: a `/compact` that fails draws a row of its own, `Compaction failed
//! · …`, which the reader reads as the same wall — see [`aterm_phase::wall`]'s
//! table — so the wall's track and its attempts stand through the failure). A
//! lost login — `Not logged in · …` under the gutter, or Claude
//! Code 2.1.281's `⏺ Login expired · Please run /login` error row (read as
//! idle, it would be continued for hours): `/login` ONCE, then the ONE irreducible escalation —
//! "finish sign-in in the browser" — said as it is typed; then nothing more
//! until the screen says the login is back (`⎿  Login successful`) or the
//! worker works — never a continuation into it, never a draft sent into it
//! (escalated instead). Claude Code's critical-memory banner
//! ([`WallKind::Memory`], read even under a running spinner): the agent is
//! RESTARTED — ended and relaunched on its own conversation by the session's
//! host, which then tells it to carry on ([`TurnEndAction::Restart`], D3 of
//! the reconciliation of 2026-09-25) — ahead of every gate a typed act waits
//! on, since nothing typed into a process past saving is read. Escalated with
//! its remedy only where the restart cannot be made (no host restarts here,
//! or `[harness] relaunch = false`).
//!
//! **ANOTHER ACCOUNT** (`docs/DESIGN-claude-accounts-2026-10-01.md` §6;
//! `[harness] accounts`). A host that keeps a roster of the owner's Claude
//! Code accounts ADVISES at each point ([`TurnEndReading::account`] — data on
//! the reading, so this decider reads no roster and stays pure); with no
//! advice, accounts change no decision here. At a
//! session or weekly limit, and at a model bucket BEFORE its fallback, advice
//! to move is a relaunch under that account ([`Restart::Account`],
//! [`RULE_ACCOUNT`]) with the wall's own answer as what the point gets where
//! the relaunch is not made; advice to wait cuts the wall's wait short at the
//! instant another account becomes usable. At a plain turn end the same
//! advice is a move AHEAD of the wall, wrapped around the turn's own act as
//! the model's return is. A lost login met right after the session's own
//! move is answered by the next account, never by `/login`. Never at a spend
//! or credit wall, under `resume_limits = false` or `relaunch = false`, over
//! a draft, or where the upgrade owns the point. Three bounds, all the
//! session's own: a move not made is not asked again while its wall stands
//! (the latch is that wall's: it ends with the wall, where a new one of the
//! same class begins after real work, and when the accounts' news reaches the
//! session — [`TurnEndState::account_news`]), no move is asked ahead of a
//! wall while the last one is unanswered, and at most
//! [`ACCOUNT_MOVES_PER_WINDOW`] are made an hour. A move MADE at a wall owes
//! that wall's resume: the continuation typed under `resume_limits` by the
//! limit-resume rule at the relaunched session's first point, whatever
//! `continue_policy` says. And advice to RESUME ([`AccountAdvice::Resume`]:
//! the owner cleared the account this session stands at the wall of) is that
//! wall's own continuation typed at once, once per clearing. Advice that the
//! account's `/usage` READ IS DUE ([`AccountAdvice::Probe`]) decides nothing
//! here: every site above reads the advice PAST the probe
//! ([`TurnEndReading::advised`]), so the move a due probe stands in front of
//! is asked for whether or not anybody makes the probe — a step of the
//! loop's, behind the pure gate [`TurnEndState::probe_point`] (by the gate
//! that shut: [`TurnEndState::probe_gate`], [`ProbeGate`]), that moves
//! none of this policy's memory ([`RULE_USAGE_PROBE`]). BOUND AT TIER-1
//! to the rotation's model (`tests/supervise_conformance_account_rotation.rs`):
//! at every state of it this decider, fed the roster's advice through
//! [`AccountAdvice::of_roster_at`], asks for the move the model makes, waits
//! where it holds the session, continues it at the notice of a wall the owner
//! lifted (the model's `Resume…`), and otherwise acts as with no advice; the
//! three bounds, which that model does not have, are bound there around every
//! ask.
//! BUILT AGAINST STAND-INS: no session has been relaunched under a second
//! login (the design's §9).
//!
//! **A LIMIT RESET'S CONTINUATION**
//! (`docs/SPEC-claude-accounts-resets-2026-10-02.md`; `[harness]
//! limit_reset`, off by default). The reset itself is the loop's step, under
//! the usage-limit dialog where this policy decides nothing; what this policy
//! is told is how it ENDED ([`TurnEndState::reset_told`],
//! [`ResetOutcome`]), and what it owes then is ONE continuation, by the
//! limit-resume rule under `resume_limits`: at once where the vendor's line
//! asks for a message, where the reset was kept or failed, and where no line
//! was read (the wall, if it still stands, answers it and is then moved from
//! or waited out as any wall is); after [`RESET_CONTINUES_WAIT`] where the
//! vendor queued its own. Any typed act, a turn its host typed and any work
//! seen since pay it. While it is owed the host is asked nothing about the
//! session's account and no probe is made. A reset that cleared the account's
//! walls is that clearing answered: the host's advice to resume for it types
//! nothing more. With nothing told, resets change no decision here.
//!
//! **CODEX: SAVE THEN WAIT** ([`WindDown`], the owner, 2026-09-28: the
//! supervisor "should only use gpt-6-luna switch like that for codex to git
//! commit and push work and then wait for the original model settings, not
//! continue work in a sandbox"). The approval policy presses Codex's
//! rate-limit nudge's `Switch to <cheaper model>` only when Codex's own usage
//! reading shows the window at 90% or more (`rate-nudge-switch@v1`;
//! otherwise it keeps the model); the press's intent is ledgered before its
//! key and the switch opens once the box is seen leaving — and then this
//! policy owns every point of the session until the switch closes, typing
//! no continuation, no answer and no draft:
//! 1. OWED: Codex's goal is stopped first — the loop's Esc on any turn
//!    running on the cheaper model that is no person's (their hand in it
//!    since the switch opened, or their grace while it lasts,
//!    [`RunningTurn::person`] — a turn measured from the nudge's box on, so
//!    a person's message that began the turn the box covered spares no goal
//!    turn after it), the goal turn Codex starts under the box among them
//!    ([`TurnEndState::goal_stop`], read at every busy read), else `/goal
//!    pause` at a free point; at most [`GOAL_STOPS`] stops
//!    a switch, then a person is told once, the note kept up while the goal
//!    runs on — then, once the footer shows the cheaper model (one still
//!    showing the thread's own model past the first point, with the thread's
//!    own rollout not on the cheaper model since the switch opened either, is
//!    the switch that did not land — and with a person's keystroke since the
//!    opening, their `/model` — released; a footer that shows no model, or
//!    lags a rollout on the cheaper one, for [`OWED_FOOTER_BOUND`] is said),
//!    ONE instruction
//!    ([`wind_down_text`], [`RULE_WIND_DOWN`]): commit the work in progress
//!    on the current branch, push it plainly (one `git pull --no-rebase` on a
//!    rejected push), never force, rebase, reset, stash or switch branches,
//!    say what failed rather than work around it, stop, and answer with a
//!    one-time marker line only when all of it is pushed;
//! 2. WINDING: its turn runs; its answer's marker is the save — without it,
//!    a person is told once, quoting the agent's last words. The turn is
//!    BOUNDED (the owner's rule: the cheaper model only commits and pushes):
//!    past [`WIND_DOWN_BOUND`] of busy work since the save was typed, the
//!    loop's busy read stops it with ONE Esc ([`TurnEndState::goal_stop`]),
//!    its interrupted point judges the marker all the same, and the switch
//!    goes on to the restore and the hold — a save still running after that
//!    Esc is said to a person, once;
//! 3. RESTORE: `/model`, whose picker the approval policy drives to the
//!    thread's own model and effort, for this conversation only
//!    (`model-restore-pick@v1`) — only the harness's own picker: once it has
//!    been gone for [`PICKER_SETTLE`] (never a frame between its boxes, read
//!    or judged as an idle point — no `/model` is typed again inside the
//!    settle), or a person has typed since, a picker is the person's
//!    ([`TurnEndState::picker_seen`]) — seen back on the footer, two tries,
//!    then the hand steps are said;
//! 4. HOLDING: begun with ONE record for a person ([`WindEvent::Hold`]: the
//!    model it waits on and the reset, in plain words); nothing typed until
//!    the original window's reset (plus [`TurnEndTiming::reset_grace`];
//!    [`TurnEndTiming::unknown_hold`] of the wall clock from the hold's
//!    start when nobody read it, a start a restart carries), decided again
//!    at least every [`TurnEndTiming::limits_every`] and ended early when
//!    Codex's usage reading drops under its limit — a reading a held session no longer
//!    writes, so an early reset is seen only when ANOTHER Codex session on
//!    the account writes one (nothing is typed to ask, `/status` included);
//!    then, under `resume_limits`, `/goal resume` when the harness paused
//!    the goal, else — under `continue_policy` — a carry-on naming the model
//!    ([`resume_text`]); with either key off, nothing is typed and a person
//!    is told.
//!
//! A person's own message, `/model` or `/goal resume` releases the switch. A
//! turn is theirs only with their hand in it
//! ([`TurnEndReading::person_in_turn`], within [`PERSON_TURN_SLACK`], or
//! [`SEEDED_TURN_BOUND`] for a turn a restarted loop inherited). A stalled
//! switch is reported once after ten minutes ([`TurnEndState::switch_hold`]).
//! Every edge is a ledger row a later loop resumes from
//! ([`TurnEndState::seed_wind`]). Nothing is typed into a thread that fell
//! into a sandbox. A Codex whose footer says its goal is pursued is never
//! continued (the continuation would land as a steer); a goal stopped by the
//! hard usage wall gets `/goal resume` at the reset.
//!
//! **Never** while a box, the session survey or a wall's wait is up, on a
//! non-authoritative reading, or within a person's grace.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_phase::codex::CodexGoal;
use aterm_phase::phase::{Phase, is_done_row, last_said_row, status_row};
use aterm_phase::wall::WallKind;
use aterm_phase::{ApiCause, Program};

use super::super::codex_usage::LimitRead;
use super::super::config::SupervisorConfig;
use super::super::limit::ResetClock;
use super::approval::QuestionPriority;
use super::reset::{RESET_CONTINUES_WAIT, ResetOutcome};

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";
/// The [`DONE_CHECK`] typed where the worker said it is done: its yield
/// saying done again ends the task ([`TurnEndState::task_done`]).
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";
/// The journal's name for a task the worker ended with [`FINISHED`].
pub const RULE_FINISHED: &str = "finished@v1";
/// Typed in place of the continuation when the worker says it is done
/// ([`says_done`]). The reply it asks for is itself a done report.
pub const DONE_CHECK: &str = "If the task is finished and verified, reply with ATERM-TASK-DONE on a line by itself \
     and stop; otherwise continue.";

/// The worker's explicit done token: a last line that is exactly this
/// (Markdown punctuation allowed). Ends the task at once with no
/// [`DONE_CHECK`] ([`TurnEndState::task_done`]); taught by the aterm primer.
pub const FINISHED: &str = "ATERM-TASK-DONE";

/// Whether the worker's last words end with [`FINISHED`] on a line of its
/// own: the explicit, unambiguous done. A token quoted inside a sentence, or
/// followed by more text, is not it.
#[must_use]
pub fn says_finished(tail: Option<&str>) -> bool {
    tail.and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()))
        .is_some_and(|last| {
            last.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) == FINISHED
        })
}
/// The worker's own suggestion accepted.
pub const RULE_SUGGESTION: &str = "continue-suggestion@v1";
/// A continuation after an overload or an API error's wait (and the one
/// try at the ceiling while the network is measured down).
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// A continuation once the host measures the API reachable again, after
/// an API error that never reached it.
pub const RULE_API_BACK: &str = "api-back@v1";
/// A continuation after a reply the connection cut off.
pub const RULE_API_CUTOFF: &str = "api-cutoff@v1";

/// What every wall act ends with: where to carry on, and not to repeat a
/// step whose result the failure may have eaten.
const CARRY_ON: &str = "Carry on from where you stopped; if the result of your last step is \
missing, check whether it ran before you repeat it.";

/// Whether `why`, a [`TurnEndAction::WaitUntil`] reason, is a wall's retry
/// wait (a ladder rung or the Down hold), the only wait a tab shows as the
/// next try (`IdleHost::waiting`). Other waits (the take check, a person
/// typing, a reset, a model picker) are not. Kept in step with
/// [`decide_turn_end`]'s reasons by a test.
#[must_use]
pub fn is_retry_wait(why: &str) -> bool {
    why.starts_with("api-error") || why.starts_with("overloaded retry")
}

/// How a person signs back in to `program`: Claude Code's `/login`; Codex
/// signs in outside its TUI.
fn sign_in_how(program: Program) -> &'static str {
    if program == Program::Codex {
        "sign in to Codex again"
    } else {
        "sign in with /login"
    }
}

/// Text typed at an API wall's act: the wall's kind named in aterm's own
/// words ([`api_wall_words`]), plus what follows from it. `rule` is the
/// act's; `kind` the wall's; `program` the one whose screen drew it.
///
/// Never a row of the screen (module header, ruling 2026-10-08): what
/// [`TurnEndReading::wall_message`] holds is whatever the reader took for
/// the wall, and text typed here is submitted as the person's own turn.
#[must_use]
pub fn wall_retry_text(rule: &str, kind: WallKind, program: Program) -> String {
    let who = if program == Program::Codex {
        "Codex"
    } else {
        "Claude Code"
    };
    let what = api_wall_words(kind);
    let cause = match kind {
        WallKind::ApiError { cause, .. } => cause,
        _ => ApiCause::Server,
    };
    match (rule, cause) {
        (RULE_API_BACK, _) => {
            format!("{who} reported {what}, and the API is reachable again now. {CARRY_ON}")
        }
        (_, ApiCause::CutOff) => {
            format!("{who} reported {what}, so your last reply may be incomplete. {CARRY_ON}")
        }
        (_, ApiCause::Unreachable | ApiCause::Config) => {
            format!("{who} reported {what}; trying again. {CARRY_ON}")
        }
        (_, ApiCause::Server) => format!("{who} reported {what}. {CARRY_ON}"),
    }
}

/// An API wall in aterm's words, from its kind alone: `an API error
/// (overloaded)`, `an API error (500)`, `an API error (the API could not be
/// reached)`. The status is the reader's number, never its text.
#[must_use]
pub fn api_wall_words(kind: WallKind) -> String {
    let (code, why) = match kind {
        WallKind::Overloaded => (None, Some("overloaded")),
        WallKind::ApiError { code, cause, .. } => (
            code,
            match cause {
                ApiCause::Server => None,
                ApiCause::Unreachable => Some("the API could not be reached"),
                ApiCause::CutOff => Some("the reply was cut off"),
                ApiCause::Config => Some("a certificate or proxy refusal"),
            },
        ),
        other => return format!("{} wall", super::a_or_an(other.name())),
    };
    match (code, why) {
        (Some(code), Some(why)) => format!("an API error ({code}, {why})"),
        (Some(code), None) => format!("an API error ({code})"),
        (None, Some(why)) => format!("an API error ({why})"),
        (None, None) => "an API error".to_string(),
    }
}

/// What the session's host last measured of the agent's route to its API
/// ([`crate::supervise::IdleHost::reach`]), never read off the screen.
/// `since` is when the measure last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    /// Nobody measures it here (`drive watch`), or the route is one the
    /// host cannot reproduce (a base URL, a cloud provider, a proxy).
    #[default]
    Unknown,
    Down {
        since: Instant,
    },
    Up {
        since: Instant,
    },
}

/// The most acts of ONE wall rule this policy types in its window
/// ([`TurnEndTiming::window`], an hour), the last line against a hot loop
/// (a full context behind a shell, seen since the reader looks under
/// background work, would have been `/compact` 720 times an hour). Every
/// ladder of
/// this policy stays under it — the densest, the network's 5-minute rung, is
/// twelve in the hour, and the others one or two — so it slows no real resume:
/// the act past it waits for the oldest to leave the window, and says so
/// (`WAITING … a breaker`).
pub const WALL_ACTS_PER_WINDOW: usize = 12;

/// A rule of this policy's wall acts (the ones [`WALL_ACTS_PER_WINDOW`]
/// counts, and [`TurnEndState::acted`] counts as a wall's attempts).
fn is_wall_act(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY
            | RULE_API_BACK
            | RULE_API_CUTOFF
            | RULE_LIMIT_RESUME
            | RULE_CONSENT
            | RULE_COMPACT
            | RULE_LOGIN
    )
}

/// A wall's own continuation: its next appearance is the act's to count.
fn is_wall_retry(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_LIMIT_RESUME | RULE_COMPACT
    )
}
/// A continuation past a usage window's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// The agent relaunched on the fallback model on a model bucket.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// The agent relaunched on the bucket's model again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
/// The agent relaunched under another of the owner's Claude Code accounts
/// ([`Restart::Account`]; `docs/DESIGN-claude-accounts-2026-10-01.md` §6).
pub const RULE_ACCOUNT: &str = "account-rotate@v1";
/// THE ACCOUNTS PROBE: `/usage` typed to read the session's account's
/// windows off the Settings panel, and the Escape that closes it
/// (`docs/SPEC-claude-accounts-probe-2026-10-01.md`). A step of the LOOP's,
/// not an act of this policy's: no [`TurnEndAction`] carries the rule, it is
/// no wall act and no continue rule, and [`TurnEndState::acted`] is never
/// called for it — the probe moves none of this policy's memory (no budget,
/// no awaited act, no back-off). Here for the one name the session ledger's
/// rows and the journal's lines share, and for the gate
/// ([`TurnEndState::probe_point`]).
pub const RULE_USAGE_PROBE: &str = "usage-probe@v1";

/// THE GATE THAT SHUT ON A DUE ACCOUNTS PROBE: the roster said the account's
/// `/usage` read is due ([`AccountAdvice::Probe`]) and nothing was typed for
/// it — each reason by ONE STABLE WORD ([`Self::word`]), the one the loop's
/// `SKIPPED seq=<n> rule=usage-probe@v1 gate=<word>` line carries, said once
/// a point. Before these words a due probe that was not run said nothing
/// anywhere, and which of a dozen conditions had shut could only be inferred
/// from timings (the live check of 2026-10-02).
///
/// The first twelve are THIS policy's, in the order [`TurnEndState::probe_gate`]
/// asks them; the rest are the loop's and its host's, which only they can
/// know (the probe's spec, §3 G). Reporting only: nothing decides by a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeGate {
    /// The reading carries no probe advice: the roster did not say the read
    /// is due (a move, a wait, nothing).
    NotDue,
    /// The phase is not `Idle`: a question, a box, a turn running, a limit.
    NotIdle,
    /// The reader's phase is a guess, not evidence.
    NotAuthoritative,
    /// A survey card is up.
    Survey,
    /// A wall is on the screen, or one is being worked off it (a wall
    /// track: a limit waited out, a lost login).
    Wall,
    /// The composer is not drawn, or holds something typed.
    Composer,
    /// A submitted row waits unanswered.
    PendingInput,
    /// A person stopped the turn with Esc.
    Interrupted,
    /// The worker asked something of a person.
    Question,
    /// No plain turn end the host is asked about
    /// ([`TurnEndState::account_point`]): no task yet or the task done, the
    /// upgrade's point, no relaunch under another account askable at all,
    /// or the session's own last move not answered.
    NotTurnEnd,
    /// Something of this policy's is awaited or owed: an act awaited, a
    /// command's continuation, a save-then-wait switch open, a wall's
    /// resume after a move.
    ActOwed,
    /// A person typed within the grace (`human_grace_s`).
    Person,
    /// The loop watches behind another supervisor's claim: it acts on
    /// nothing.
    Watching,
    /// The loop keeps no session ledger: a panel it opened could not be
    /// put on record.
    NoLedger,
    /// This loop probes no more: what an earlier probe brought up was not
    /// the Usage tab, or it left the point changed.
    ProbesOff,
    /// A `/usage` panel of the harness's is still owed its close.
    PanelOwed,
    /// The turn that ended here was not read LIVE by this loop: no read
    /// showed the worker's own busy row, so no response of this process is
    /// known to be newer than the home's usage snapshot.
    NotLive,
    /// The point is not new: it is decided AGAIN (a wait that ran out, the
    /// accounts' news), and a probe goes only at the look that first takes
    /// a point.
    NotNew,
    /// The point is a person's: the policy escalated it.
    Escalated,
    /// This grid size showed only the panel's fold at an earlier probe, and
    /// is not typed into again by this loop.
    FoldLatch,
    /// The host wants the point for a step of its own (the live upgrade, a
    /// relaunched agent's carry-on), or took it.
    HostWants,
    /// A stall is held or suspected: the worker may not be reading input.
    Stalled,
    /// The Mac woke less than its settle ago (30 s and the session's share
    /// of 60 s): the panel would read the account's windows from an API the
    /// network may not reach yet.
    WakeSettle,
    /// The server lacks a fence the probe's writes carry: the read's
    /// generation or person count, `if-gen=` on `key` or `send`, the row
    /// guard, the wait on a row leaving.
    NoFence,
    /// The session's program, read fresh, is not Claude Code.
    NotClaude,
    /// The agent does not draw on the alternate screen: Claude Code's
    /// inline renderer, where `/usage` leaves rows in the transcript.
    NotFullscreen,
    /// The host refused the claim; its own word for why follows on the line
    /// (`host=<word>`).
    Claim,
}

impl ProbeGate {
    /// Every gate, for a reader of the words.
    pub const ALL: [ProbeGate; 27] = [
        ProbeGate::NotDue,
        ProbeGate::NotIdle,
        ProbeGate::NotAuthoritative,
        ProbeGate::Survey,
        ProbeGate::Wall,
        ProbeGate::Composer,
        ProbeGate::PendingInput,
        ProbeGate::Interrupted,
        ProbeGate::Question,
        ProbeGate::NotTurnEnd,
        ProbeGate::ActOwed,
        ProbeGate::Person,
        ProbeGate::Watching,
        ProbeGate::NoLedger,
        ProbeGate::ProbesOff,
        ProbeGate::PanelOwed,
        ProbeGate::NotLive,
        ProbeGate::NotNew,
        ProbeGate::Escalated,
        ProbeGate::FoldLatch,
        ProbeGate::HostWants,
        ProbeGate::Stalled,
        ProbeGate::WakeSettle,
        ProbeGate::NoFence,
        ProbeGate::NotClaude,
        ProbeGate::NotFullscreen,
        ProbeGate::Claim,
    ];

    /// The word the journal's `SKIPPED … gate=<word>` line carries. STABLE:
    /// a reader greps for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            ProbeGate::NotDue => "not-due",
            ProbeGate::NotIdle => "not-idle",
            ProbeGate::NotAuthoritative => "not-authoritative",
            ProbeGate::Survey => "survey",
            ProbeGate::Wall => "wall",
            ProbeGate::Composer => "composer",
            ProbeGate::PendingInput => "pending-input",
            ProbeGate::Interrupted => "interrupted",
            ProbeGate::Question => "question",
            ProbeGate::NotTurnEnd => "not-turn-end",
            ProbeGate::ActOwed => "act-owed",
            ProbeGate::Person => "person",
            ProbeGate::Watching => "watching",
            ProbeGate::NoLedger => "no-ledger",
            ProbeGate::ProbesOff => "probes-off",
            ProbeGate::PanelOwed => "panel-owed",
            ProbeGate::NotLive => "not-live",
            ProbeGate::NotNew => "not-new",
            ProbeGate::Escalated => "escalated",
            ProbeGate::FoldLatch => "fold-latch",
            ProbeGate::HostWants => "host-wants",
            ProbeGate::Stalled => "stalled",
            ProbeGate::WakeSettle => "wake-settle",
            ProbeGate::NoFence => "no-fence",
            ProbeGate::NotClaude => "not-claude",
            ProbeGate::NotFullscreen => "not-fullscreen",
            ProbeGate::Claim => "claim",
        }
    }
}
/// `/compact` on a full context, and the continuation after it.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// `/login` on a lost login.
pub const RULE_LOGIN: &str = "auth-login@v1";
/// `cfg.answer_text`, typed where a worker asked a person.
pub const RULE_ANSWER: &str = "answer@v1";
/// A model bucket's consent to go on on usage credits, accepted.
pub const RULE_CONSENT: &str = "consent-accept@v1";
/// The agent restarted in its tab for Claude Code's critical-memory banner.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";
/// Codex's save-then-wait switch ([`WindDown`]): the harness's own Esc or
/// `/goal pause` that stops Codex's goal, and the instruction to commit and
/// push the work and stop ([`wind_down_text`]).
pub const RULE_WIND_DOWN: &str = "model-wind-down@v1";
/// A turn the session's host typed (upgrade notice, carry-on;
/// [`TurnEndState::host_typed`]): awaited like this policy's acts; its short
/// answer is no short turn of the worker's.
pub const RULE_HOST_TURN: &str = "harness-turn@v1";

/// Why the session's host restarts the agent in its tab
/// ([`TurnEndAction::Restart`]): a point nothing typed can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// Claude Code's critical-memory banner: nothing typed into the
    /// process is read, and no reset ends it.
    Memory,
    /// A model bucket's limit: relaunched on `to`, the `model_fallback`, via
    /// session-only `--model` (Claude's `/model` also rewrites the person's
    /// default).
    Model { to: String },
    /// The bucket's reset: relaunched on `to`, or with `None` on the launch's
    /// own model, else Claude's default.
    ModelBack { to: Option<String> },
    /// ANOTHER ACCOUNT (the accounts design, §6 "The move"): relaunched on
    /// its own conversation under the login the roster's label `to` names —
    /// the label is the only name an account has here; the directory is the
    /// roster's, resolved where the relaunch is made
    /// (`crate::harness::relaunch::restart_under`). `at` says which of the
    /// two moves it is, because the loop treats a host that cannot make it
    /// yet differently for each ([`AccountAt`]).
    Account { to: String, at: AccountAt },
}

/// WHERE an account move is asked ([`Restart::Account`]). One variant would
/// not do: the loop decides by it whether a relaunch the host cannot make
/// yet (`wait:changed` while the agent's own jobs run) HOLDS the point, and
/// the two moves need opposite answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAt {
    /// The turn ended on a limit: nothing typed there is answered, so the
    /// point is held and the move asked again on
    /// [`TurnEndTiming::restart_backoff`], as a bucket's fallback is.
    Wall,
    /// A plain turn end, the wall still ahead: the session works where it
    /// is, so the move is optional — deferred as the model's return is, the
    /// turn's own act going ahead and the move asked again at the next idle
    /// point.
    Ahead,
}

impl Restart {
    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Restart::Memory => "memory",
            Restart::Model { .. } => "model",
            Restart::ModelBack { .. } => "model-back",
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => "account",
            Restart::Account {
                at: AccountAt::Ahead,
                ..
            } => "account-ahead",
        }
    }
}

/// WHAT THE SESSION'S HOST ADVISES ABOUT ITS ACCOUNT at one point
/// ([`TurnEndReading::account`], from `crate::supervise::IdleHost::account`):
/// the roster's judgment (`crate::harness::accounts::advise`) as DATA, so the
/// decider stays pure — it reads no roster, no state file and no clock of its
/// own. `None` on the reading is "stay": no roster, a session the roster does
/// not list, `[harness] accounts = "off"` — today's behaviour, byte for byte.
///
/// THE DECIDER IS TRANSPARENT TO A PROBE. [`AccountAdvice::Probe`] is the
/// roster's word that the account's `/usage` read is due; whether anybody
/// makes it is the loop's and its host's (the claim, the fences, the
/// renderer), so [`decide_turn_end`] and [`TurnEndState::acted`] never act
/// on the word itself: every site that reads the advice reads it through
/// [`AccountAdvice::past_probe`] — the probe's `otherwise` — and a probe
/// nobody makes never masks a move. The one thing here that reads the word
/// is the pure gate [`TurnEndState::probe_point`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountAdvice {
    /// THE ACCOUNT'S `/usage` READ IS DUE
    /// (`crate::harness::accounts::Advice::Probe`), at a plain turn end.
    /// `otherwise` is what the roster advises PAST the probe
    /// (`crate::harness::accounts::advise_past_probe`, mapped as any advice
    /// is): the move ahead of a wall the probe stands in front of, or
    /// `None` — stay. It is what this file decides on.
    Probe {
        otherwise: Option<Box<AccountAdvice>>,
    },
    /// Relaunch under the account labelled `to`.
    Move { to: String },
    /// No account can be run on before `until`: the wall is waited out as it
    /// would be, and decided again no later than then.
    Wait { until: Instant },
    /// THE OWNER LIFTED THIS WALL (`crate::harness::accounts::Advice::Resume`:
    /// `accounts use` cleared the session's own account after the limit row
    /// it stands at was written): continue here, now — the wall's own
    /// continuation is the probe. `cleared_at` is the clearing's instant
    /// (unix seconds), the key of "once per clearing"
    /// ([`TurnEndState::acted`]): a second advice for the same clearing types
    /// nothing more, and the wall's own ladder takes over.
    Resume { cleared_at: i64 },
}

impl AccountAdvice {
    /// The advice the decider acts on: a probe's `otherwise`, else itself.
    /// `None` for a probe with nothing behind it — stay, exactly as with no
    /// advice at all. Never a [`AccountAdvice::Probe`]: one nested in
    /// another's `otherwise` (no mapping builds that) is read through too.
    #[must_use]
    pub fn past_probe(&self) -> Option<&AccountAdvice> {
        let mut advice = self;
        while let AccountAdvice::Probe { otherwise } = advice {
            advice = otherwise.as_deref()?;
        }
        Some(advice)
    }

    /// THE ROSTER'S ADVICE AS THE POLICY TAKES IT
    /// (`crate::harness::accounts::Advice`, what `accounts::advise` answers a
    /// host with, to [`AccountAdvice`]), `clock` being the `Instant` of the
    /// unix second `now`. The ONE mapping: the window's host advises through
    /// it, and the Tier-1 walk of the rotation's model
    /// (`tests/supervise_conformance_account_rotation.rs`) feeds
    /// [`decide_turn_end`] through it at every state of that model.
    ///
    /// `past`: what `crate::harness::accounts::advise_past_probe` answered
    /// on the same state and second, handed where `advice` is `Probe` (the
    /// roster answers `Probe` AHEAD of its leave check, so the move a due
    /// probe stands in front of is asked for separately); ignored for every
    /// other advice.
    ///
    /// * `Move` is a move, whichever of the three it is (the host's commit
    ///   reads that again under its lock).
    /// * `Wait` is a wait until its instant.
    /// * `Resume` is a resume, for the clearing it names.
    /// * `Stay` for the machine's rotation bound is a wait until the bound has
    ///   room again (the accounts design, §6: "a breaker is a wait").
    /// * `Stay` for an account that has not served a turn yet, AT A WALL, is a
    ///   wait until its canary's hold ends
    ///   (`crate::harness::accounts::CANARY_HOLD_S`): a canary that answers
    ///   wakes the session sooner
    ///   ([`crate::supervise::IdleHost::account_stamp`]), and one that never
    ///   does — its tab idle with nothing to send — must not leave this
    ///   session waiting for its own limit's reset before it takes the
    ///   canary's place.
    /// * THE WAIT OF A SESSION HELD AT ITS WALL ENDS AT WHATEVER COMES FIRST
    ///   (`crate::harness::accounts::hold_until`): the hold's end or the
    ///   hour's, or `opening` — the roster's next opening
    ///   (`crate::harness::accounts::next_opening`, on the same state and
    ///   second; the caller's, which has the roster) — where that is sooner.
    ///   A reset that makes the active account followable, or the session's
    ///   own usable, then decides the point again, where it sat out the rest
    ///   of ten minutes or of the hour beside an account it could run on.
    /// * `Probe`, AT A TURN END, is [`AccountAdvice::Probe`] with `past`
    ///   mapped by these same rules as its `otherwise` (`None` where `past`
    ///   is, or maps to nothing — and a `past` that is itself `Probe`, which
    ///   `advise_past_probe` never answers, is nothing). AT A WALL (the
    ///   screen shows one the roster was asked about as a plain turn end:
    ///   a limit another process wrote) nothing is probed: `past` alone,
    ///   mapped, exactly what the point got before the probe existed.
    /// * Every other `Stay`: nothing — the point is decided as it was before
    ///   accounts existed.
    ///
    /// PURE: no clock but the two it is handed.
    #[must_use]
    pub fn of_roster_at(
        advice: &crate::harness::accounts::Advice,
        past: Option<&crate::harness::accounts::Advice>,
        state: &crate::harness::accounts::State,
        opening: Option<i64>,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        use crate::harness::accounts::{Advice, StayWhy, hold_until};
        let at = |until: i64| {
            clock + Duration::from_secs(u64::try_from(until.saturating_sub(now)).unwrap_or(0))
        };
        // Every advice but the probe, which has no word of its own here.
        let plain = |advice: &Advice| match advice {
            Advice::Move { to, .. } => Some(AccountAdvice::Move { to: to.clone() }),
            Advice::Wait { until } => Some(AccountAdvice::Wait { until: at(*until) }),
            Advice::Resume { cleared_at } => Some(AccountAdvice::Resume {
                cleared_at: *cleared_at,
            }),
            // Ahead of a wall a breaker's instant decides nothing (the turn's
            // own act goes on); it is handed over as it is.
            Advice::Stay(StayWhy::Breaker { until }) if !at_wall => {
                Some(AccountAdvice::Wait { until: at(*until) })
            }
            Advice::Stay(why @ (StayWhy::Canary | StayWhy::Breaker { .. })) if at_wall => {
                hold_until(*why, state, opening, now)
                    .map(|until| AccountAdvice::Wait { until: at(until) })
            }
            Advice::Stay(_) | Advice::Probe => None,
        };
        match advice {
            Advice::Probe => {
                let otherwise = past.and_then(plain);
                if at_wall {
                    otherwise
                } else {
                    Some(AccountAdvice::Probe {
                        otherwise: otherwise.map(Box::new),
                    })
                }
            }
            other => plain(other),
        }
    }

    /// [`Self::of_roster_at`] for a caller that has no roster's opening and
    /// no advice past a probe to hand over: a session held at its wall waits
    /// out its hold, or the hour, whole, and a due probe has nothing behind
    /// it.
    #[must_use]
    pub fn of_roster(
        advice: &crate::harness::accounts::Advice,
        state: &crate::harness::accounts::State,
        at_wall: bool,
        now: i64,
        clock: Instant,
    ) -> Option<AccountAdvice> {
        Self::of_roster_at(advice, None, state, None, at_wall, now, clock)
    }
}

/// THE POINT the host is asked about ([`crate::supervise::IdleHost::account`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPoint {
    /// The turn ended on a wall of `kind`, as the SCREEN reads it. Whether
    /// the limit is this process's own (a `quotaLimits` row it wrote, the
    /// accounts design §5) is the host's to tell: the loop reads no
    /// transcript.
    Wall { kind: WallKind },
    /// A turn that ended with none.
    TurnEnd,
}

/// WHAT THE FIRST TURN AFTER THIS SESSION'S OWN ACCOUNT MOVE WAS ANSWERED
/// WITH, when that is no served response
/// ([`crate::supervise::IdleHost::account_answered`]): the host strikes the
/// account, so the advice it gives next is the next account, or the way back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountBad {
    /// A usage limit.
    Wall,
    /// A lost login.
    SignedOut,
}

/// [`TurnEndState::account_answer`]: how the move was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAnswer {
    /// A turn asked under the new login ran and ended with no wall.
    Served,
    Bad(AccountBad),
}

/// An account move this session's host MADE and no point has answered yet
/// ([`TurnEndState::account_move`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMove {
    /// The label moved to.
    pub to: String,
    /// When the relaunch was made.
    pub at: Instant,
}

/// The most account moves ONE session makes in its window
/// ([`TurnEndTiming::window`], an hour); past it the wall is waited out as
/// it would be with no roster, and said so (`WAITING … a breaker`). WHY A
/// COUNT AT ALL: the bucket's guessed return relaunched one session twice an
/// hour for five days, 240 times ([`TurnEndState::restarted`]), and a restart
/// is counted by no other breaker (it is no typed act). WHY FOUR: the design
/// bounds a session at its roster's size plus one, and this decider is pure —
/// it never sees the roster — so the figure is the design's own three-row
/// roster plus one, which a healthy hour never reaches (a move is made only
/// onto an account that can be run on, and each costs a relaunch and an
/// uncached read of the whole context). The roster-sized bound is the
/// machine's, kept by the host (`crate::harness::accounts::rotations_spent`);
/// this one is the session's last line under a host that advises wrongly.
pub const ACCOUNT_MOVES_PER_WINDOW: usize = 4;

/// The nearest a wall's wait is cut to for an account that becomes usable
/// ([`AccountAdvice::Wait`]): an instant nearer than this is decided at this
/// distance, so a host whose instant is always "a moment from now" costs one
/// decision a minute and not a hot loop.
const ACCOUNT_WAIT_FLOOR: Duration = Duration::from_secs(60);

/// The words a worker says it is DONE with, lowercased, matched anywhere in
/// its last words ([`says_done`]): a continuation's yield that ends so is
/// short, whatever it worked (module header). What the live E2E of
/// 2026-09-25 read after its nudges (`The workflow is finished`, `nothing
/// left to do`, `what would you like me to work on?`) and their kin.
pub const DONE_PHRASES: &[&str] = &[
    "nothing left to do",
    "nothing else to do",
    "nothing more to do",
    "nothing further to do",
    "nothing left to work on",
    "no further work",
    "no remaining work",
    "no more work",
    "all done",
    "all tasks are complete",
    "all tasks complete",
    "everything is done",
    "everything is already done",
    "everything is complete",
    "already complete",
    "work is complete",
    "work is done",
    "task is complete",
    "workflow is finished",
    "workflow is complete",
    "completed successfully",
    "what would you like me to work on",
    "what would you like me to do",
    "what should i work on",
    "what else would you like",
    "let me know if you need anything",
    "let me know if there's anything else",
    // A finished report that asks only what comes next.
    "what's next?",
    "anything else?",
];

/// Whether the worker's last words say it is done ([`DONE_PHRASES`]) or are
/// exactly `DONE` (punctuation allowed). Title-case `Done.` is a step report.
///
/// Remembered for the last tail read on this thread (`LastTail`).
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    let Some(t) = tail else {
        return false;
    };
    thread_local! {
        static LAST: LastTail<bool> = const { LastTail::new() };
    }
    LAST.with(|last| last.get_or(t, || says_done_read(t)))
}

/// [`says_done`] on a tail, read afresh.
fn says_done_read(t: &str) -> bool {
    if says_finished(Some(t)) || t.trim_matches(|c: char| !c.is_alphanumeric()) == "DONE" {
        return true;
    }
    let flat = t
        .to_lowercase()
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    DONE_PHRASES.iter().any(|p| flat.contains(p))
}

/// The rules whose act's yield counts toward the short streak: an answer
/// under any priority among them ([`is_answer_rule`]) — never
/// [`RULE_HOST_TURN`], the harness's own turn.
fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE
        || rule == RULE_SUGGESTION
        || is_answer_rule(rule)
        || rule == RULE_DONE_CHECK
}

/// Whether `rule` answers a question the worker asked in prose — under any
/// [`QuestionPriority`] ([`QuestionPriority::prose_rule_id`]: [`RULE_ANSWER`],
/// `prose-long-term@v1`, `prose-short-term@v1`).
#[must_use]
pub fn is_answer_rule(rule: &str) -> bool {
    QuestionPriority::ALL
        .into_iter()
        .any(|p| p.prose_rule_id() == rule)
}

/// Claude Code's own `/model` aliases (2.1.280's binary: `["sonnet", "opus",
/// "haiku", "fable"]`): the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// Pieces a suggestion may be made of to be accepted as a continuation,
/// joined by `,`, `;`, `.`, ` and ` or ` then `.
pub const SUGGESTION_ALLOW: &[&str] = &[
    "keep going",
    "continue",
    "keep fixing forward",
    "push it when green",
    "go on",
    "carry on",
    "keep at it",
    "proceed",
    "go ahead",
];

/// Phrases a worker asks a person for a decision with: answered
/// ([`RULE_ANSWER`]), never continued; [`REVERSIBLE_ANSWER`] when a
/// destructive act ([`DESTRUCTIVE_WORDS`]) is named. Lowercased, anywhere.
pub const STOP_PHRASES: &[&str] = &[
    "need your decision",
    "needs your decision",
    "need a decision",
    "your decision on",
    "blocked on",
    "i'm blocked",
    "i am blocked",
    "waiting on you",
    "waiting for you",
    "waiting on your",
    "waiting for your",
    // Names the person's authority rather than listing options: a stop, not
    // a choice ([`CHOICE_PHRASES`]).
    "your call",
    "need your input",
    "need your approval",
    "need you to",
    "please confirm",
    "can you confirm",
    "please advise",
    "how would you like me to proceed",
    "cannot proceed without",
    "can't proceed without",
    // The go-ahead, sign-off and review asks, and the
    // ways a worker says it will wait for one.
    "your go-ahead",
    "your go ahead",
    "the go-ahead",
    "a go-ahead",
    "sign off",
    "sign-off",
    "signoff",
    "for your review",
    "ready for review",
    "ready for your review",
    "please review",
    "your approval",
    "needs approval",
    "need approval",
    "how you'd like",
    "how you would like",
    "how would you like",
    "what you'd like me to",
    "let me know how",
    "let me know whether",
    "i'll wait",
    "i will wait",
    "i'll hold off",
    "holding off until",
    "before i proceed",
    "before i continue",
    "before i go ahead",
    "before i run",
    "before i push",
    "before i force",
    "before i delete",
    "before i merge",
    "before i deploy",
    "before i release",
];

/// Phrases a worker asks a person to choose with: a stop, answered
/// ([`RULE_ANSWER`]), only where the phrase asks ([`phrase_asks`]), never in
/// a report; with the act named when a destructive word follows. Matched as
/// [`STOP_PHRASES`] are.
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an offer, stop, choice or question one a person must
/// answer: answered with [`REVERSIBLE_ANSWER`], never continued (`keep going`
/// would read as consent). Whole words after the offer's phrase. Plain `push`
/// is excluded: `push it when green` is an accepted continuation.
pub const DESTRUCTIVE_WORDS: &[&str] = &[
    "force",
    "force-push",
    "--force",
    "delete",
    "deleting",
    "deletion",
    "drop",
    "dropping",
    "remove",
    "removing",
    "removal",
    "rm",
    "deploy",
    "deploying",
    "migrate",
    "migrating",
    "publish",
    "publishing",
    "release",
    "releasing",
    "wipe",
    "purge",
    "reset",
    "overwrite",
    "truncate",
    "destroy",
    "uninstall",
    "revert",
    "rewrite history",
];

/// Words that make a question an offer to go on (it counts as a
/// continuation): `want me to take that on?`.
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    // An offer of more work, not a yes/no about the work done.
    "should i also",
    "would you like me to",
    "next steps:",
    "next step:",
    "i can also",
    "if you'd like, i can",
    "let me know if you'd like",
    "happy to",
];

/// Words that recommend one option: a choice with a recommendation is the
/// worker's to take.
pub const RECOMMEND_PHRASES: &[&str] = &[
    "i recommend",
    "i'd recommend",
    "i would recommend",
    "recommended",
    "my recommendation",
    "i suggest",
    "i'd suggest",
    "i'd go with",
    "i'll go with",
    "i'd pick",
    "i lean",
];

/// The policy's timings — the defaults are the plan's; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    /// The busy work that makes a turn real work: a turn that did less is
    /// short, and backs the next act off ([`Self::short_backoff`]).
    pub min_work: Duration,
    /// The wait, from the point, before the act after ONE short turn; each
    /// short turn more in a row doubles it, up to [`Self::short_backoff_max`].
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    /// The wait before each retry of the server's own failure (a status,
    /// an overload), counted from the wall's appearance; the last is the
    /// wait before every retry after it.
    pub retry_backoff: Vec<Duration>,
    /// The same for an API never reached, a refused certificate or proxy, and
    /// a cut-off reply while reach is not measured up: short, since a blind
    /// try costs only the vendor's own retries.
    pub net_backoff: Vec<Duration>,
    /// How long nothing is typed into an API measured down before one try
    /// anyway: the most a wrong measure holds a worker.
    pub down_hold: Duration,
    /// Busy work between a wall's act and its next appearance that ends the
    /// episode (longer than the vendor's own retries); a later wall starts a
    /// new ladder.
    pub progress: Duration,
    /// How long past a usage window's reset the continuation waits: 150 s,
    /// past the vendor's own auto-continue (Claude Code arms its resume at
    /// the reset plus 30-90 s and checks every 30 s — measured +58 s and
    /// +105 s in the owner's history), so a wall whose armed notice this
    /// build cannot read (2.1.284 draws it as two notifications whose layout
    /// is unmeasured) is still not answered twice: the continuation is the
    /// probe, sent only if the wall still stands then.
    pub reset_grace: Duration,
    /// Up to this much more, per SESSION — the loop derives
    /// it from the session and the reset ([`TurnEndReading::jitter`]) — so
    /// the sessions of one account, whose walls all lift at the same
    /// instant, do not all type at the same second (Claude Code spreads its
    /// own auto-continue over 30-90 s).
    pub reset_jitter: Duration,
    /// How long past the time a notice that goes on BY ITSELF named
    /// (`continuing automatically at 1:50pm`) the vendor is given before the
    /// policy continues the worker itself: the vendor's own retry is waited
    /// for, never doubled — this is the net under it.
    pub auto_resume_grace: Duration,
    /// After a continuation at a usage wall that hit the wall again: the
    /// next is this far off (the last entry for every one after).
    pub limit_backoff: Vec<Duration>,
    /// The same for a wall no reset ends and
    /// none names — a model bucket that "requires usage credits", a spend or
    /// credit limit, an account the admin switched off. Such a wall is an
    /// entitlement, not a window: a `keep going` every half hour at one that
    /// lasts days was about 48 blind messages a day (each a turn of its own
    /// and a synthetic error row). The first try waits the first entry from
    /// the appearance, each later one the entry after the last try, the last
    /// entry for every one after it: the safety net for an account whose
    /// owner added credits and touched nothing else.
    pub entitlement_backoff: Vec<Duration>,
    /// The window the typing budget counts over.
    pub window: Duration,
    /// How long an act's point may show with no busy read before it is
    /// judged anyway ([`TurnEndState::observe`]): a reply inside the `turn`
    /// verb's settle, or an Enter that did not take, is never seen busy.
    pub take_within: Duration,
    /// Pauses between tries of a restart the host cannot make yet
    /// ([`TurnEndAction::Restart`]); the last repeats. Never given up.
    pub restart_backoff: Vec<Duration>,
    /// How long a save-then-wait hold with an unread reset lasts before the
    /// session resumes on its own model ([`WindDown`]); a model still limited
    /// then meets Codex's own wall, which names the reset.
    pub unknown_hold: Duration,
    /// The longest a hold waits before it is decided again — the usage
    /// reading looked at anew each time.
    pub limits_every: Duration,
    /// How long after the harness typed its `/model` the picker is answered
    /// as its own restore ([`TurnEndState::restore_target`]).
    pub restore_pick: Duration,
}

/// The longest grace past a reset that a program with no auto-continue of its
/// own waits (Codex's; [`TurnEndTiming::grace_for`]).
const NO_AUTO_CONTINUE_GRACE: Duration = Duration::from_secs(60);

impl TurnEndTiming {
    /// The grace past a wall's reset for the program the wall is in. Claude
    /// Code arms its own resume 30 to 90 seconds after a reset and checks
    /// every 30, so its first continuation stays past that window
    /// ([`Self::reset_grace`], 150 s). Codex has no auto-continue to stay
    /// clear of, and its switch's return times count a minute: the grace is
    /// capped there.
    #[must_use]
    pub fn grace_for(&self, program: Program) -> Duration {
        if program == Program::Claude {
            self.reset_grace
        } else {
            self.reset_grace.min(NO_AUTO_CONTINUE_GRACE)
        }
    }
}

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: min(2),
            short_backoff: min(2),
            short_backoff_max: min(60),
            retry_backoff: vec![min(1), min(5), min(15), min(30), min(60)],
            net_backoff: vec![min(1), min(2), min(5)],
            down_hold: min(15),
            progress: min(5),
            reset_grace: Duration::from_secs(150),
            reset_jitter: Duration::from_secs(60),
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
            entitlement_backoff: vec![min(30), min(120), min(360)],
            window: min(60),
            take_within: Duration::from_secs(30),
            restart_backoff: vec![Duration::from_secs(10), min(1), min(5), min(10)],
            unknown_hold: min(5 * 60),
            limits_every: min(30),
            restore_pick: min(2),
        }
    }
}

/// The composer as the reading found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composer {
    /// No composer frame on the screen (a dialog, a shell).
    Absent,
    Empty,
    /// The worker's own suggestion, drawn DIM with nothing typed.
    Placeholder(String),
    /// Text is typed: a draft, never typed over — submitted as it stands
    /// once nobody has touched it for the grace ([`TurnEndAction::Submit`]).
    Typed,
    /// Aterm's own words are in the composer: the upgrade's notice, whole or
    /// partial ([`crate::harness::upgrade::own_draft`]), or on Codex a line its
    /// lane types whole (`codex_own_line`), left when a press stopped part-way
    /// or its Enter missed. Never typed over and never sent; neither lane takes
    /// it back (a person's key may be in flight), so the point is escalated
    /// once ([`OWN_WORDS_STAND`]).
    Own,
}

impl Composer {
    /// Whether text stands in the composer: a draft, or aterm's own words.
    #[must_use]
    pub fn drafted(&self) -> bool {
        matches!(self, Composer::Typed | Composer::Own)
    }
}

/// Escalation reason while aterm's own words stand unsent in the prompt
/// ([`Composer::Own`]): the worker is not continued until a person clears
/// them. Worded for both a partial Claude notice and a Codex line whose
/// Enter missed.
pub const OWN_WORDS_STAND: &str =
    "aterm's own words stand unsent in the prompt; clear them and the session goes on";

/// Suffix of an escalated question's reason when `answer_questions` holds it
/// (`<why>; answer_questions is off`). The loop then reads the session's own
/// `questions` word, whose priority may answer it.
pub const ANSWERS_OFF: &str = "answer_questions is off";

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    /// The reader's phase is evidence ([`aterm_phase::Reading::phase_authoritative`]).
    pub authoritative: bool,
    pub survey: bool,
    /// The wall the turn ended on.
    pub wall: Option<WallKind>,
    /// The wall's notice as said (for the model a bucket names, and the
    /// escalations a person reads). SCREEN TEXT: never typed into the worker
    /// ([`wall_retry_text`]).
    pub wall_message: String,
    /// The wall's reset as the loop's clock (the loop resolves the notice's
    /// `reset=` text with its zone and clock).
    pub reset_at: Option<Instant>,
    /// The wall's reset as its notice's clock names it (the reader's `reset=`
    /// text, [`ResetClock`]): what ties a notice the screen draws later to
    /// this limit ([`Self::since_limit`]).
    pub reset_clock: Option<ResetClock>,
    /// This session's share of
    /// [`TurnEndTiming::reset_jitter`] for this reset, on top of
    /// [`TurnEndTiming::reset_grace`] (zero: from the screen, in tests).
    pub jitter: Duration,
    /// The notice says Claude Code goes on by itself (`continuing
    /// automatically at …`).
    pub resumes_by_itself: bool,
    pub composer: Composer,
    /// The worker's last words ([`aterm_phase::said_tail`]).
    pub said_tail: Option<String>,
    /// For [`TurnEndState::observe`] only: the busy work since the previous
    /// point, `None` when no read saw the worker busy since (the same point
    /// read again).
    pub worked: Option<Duration>,
    /// The standing rules, one line, when a rules file is set and readable.
    pub rules: Option<String>,
    /// The last row is a user's `❯` row not yet answered (a continuation just
    /// submitted, before its spinner): no turn has ended.
    pub pending_input: bool,
    /// A person stopped the turn with Esc ([`aterm_phase::interrupted`]):
    /// nothing is typed here — the next message is theirs.
    pub interrupted: bool,
    /// A host can restart the agent here
    /// ([`crate::supervise::IdleHost::can_restart`]). Set by the loop.
    pub restartable: bool,
    /// The command a person runs to resume the agent's own conversation
    /// ([`crate::supervise::IdleHost::resume_command`], `claude --resume
    /// <id>`), named by the memory wall's escalation. `None` names no command
    /// (`claude --continue` would pick the directory's newest conversation).
    /// Set by the loop.
    pub resume: Option<String>,
    /// The live upgrade owns the session's turn ends
    /// ([`crate::supervise::IdleHost::owns_turn_end`]: the agent answered READY
    /// and the restart is imminent), read from the upgrade's state, never the
    /// screen: nothing is typed. Never set during the loop's limit episode; a
    /// wall is still decided by its own rule, with only the memory banner
    /// asking for a relaunch ([`decide_turn_end`]).
    pub upgrading: bool,
    /// No task yet: nobody but the harness has asked the session anything
    /// (the launch card and no message, [`aterm_phase::Reading::fresh`], or the
    /// host's [`crate::supervise::IdleHost::taskless`]). Nothing is typed.
    pub taskless: bool,
    /// The screen says the login is back ([`aterm_phase::login_restored`]),
    /// which lifts the login hold ([`decide_turn_end`]).
    pub login_back: bool,
    /// How Claude Code's own auto-continue of a usage limit ended, by the
    /// vendor's word under it ([`aterm_phase::auto_continue_end`]): read at a
    /// point the limit's notice has left ([`TurnEndState::observe`]).
    pub auto_continue: Option<aterm_phase::AutoContinue>,
    /// THE SCREEN'S NEWEST USAGE LIMIT NOTICE and whether a turn was served
    /// since it ([`aterm_phase::since_limit`]: a person's prompt below it with
    /// the model's reply under the prompt). Read at a point the limit's notice
    /// has left, where no read saw the worker busy ([`TurnEndState::observe`]):
    /// a served turn there under THE NOTICE OF THE LIMIT WAITED ON — the
    /// reset it names is that limit's ([`ResetClock`]) — is the
    /// person's work at the tab, and ends the wait as one seen busy does.
    /// Under another limit's notice (a `/resume`d conversation's history) it
    /// says nothing of this one.
    pub since_limit: Option<aterm_phase::SinceLimit>,
    /// A TURN WAS SERVED SINCE THE LIMIT'S OWN NOTICE ROW, by the session's
    /// transcript — set by the loop at a decision's read where the screen
    /// draws no limit's notice to order the turn by ([`Self::since_limit`]
    /// `None`) and the host hands it the transcript
    /// (`supervise/wall_confirm.rs`, [`TurnEndState::observe`]).
    pub served_in_transcript: bool,
    /// The host's measure of the API's reach, set by the loop at an
    /// unreachable or cut-off wall; [`Reach::Unknown`] from the screen.
    pub reach: Reach,
    /// Time since a person last typed into the session through its window
    /// (`status human_ms=`; control-socket writes excluded) or the composer's
    /// draft last changed, whichever is later. `None`: no person.
    pub person: Option<Duration>,
    /// The program whose reader read the screen.
    pub program: Program,
    /// Codex's goal as its footer names it ([`aterm_phase::codex::goal_state`]):
    /// `Pursuing`, and Codex starts its own next turn — nothing is typed.
    pub goal: Option<CodexGoal>,
    /// Codex's thread's model and effort off its footer
    /// ([`aterm_phase::codex::footer_model`]).
    pub model_field: Option<CodexSetting>,
    /// What Codex's own records say of its usage window — the loop's to set
    /// ([`crate::supervise::codex_usage::look`]); unknown from the screen.
    pub limits: LimitRead,
    /// The sandbox a Codex thread fell into despite its launch's bypass
    /// ([`crate::supervise::codex_usage::sandbox_fell`]): nothing is typed.
    pub sandbox_fell: Option<String>,
    /// The model the session's Codex thread ran its last turn on, from its
    /// rollout's last `turn_context`
    /// ([`crate::supervise::codex_usage::CodexSeen::thread_model`]). While a
    /// save-then-wait switch is open, only a turn begun since its press counts
    /// ([`WindDown::pressed_at`], [`crate::supervise::codex_usage::model_since`]).
    /// Unknown from the screen; set by the loop.
    pub thread_model: Option<String>,
    /// The live upgrade holds Codex's goal paused (`harness::goal_hold`): a
    /// switch held over that pause resumes with `/goal resume`, never a
    /// carry-on ([`wind_down_act`]). Set by the loop.
    pub upgrade_goal: bool,
    /// WHAT THE HOST ADVISES ABOUT THE SESSION'S ACCOUNT at this point
    /// ([`AccountAdvice`]; the loop's to set, asked of its host ONCE a
    /// decision: `crate::supervise::IdleHost::account`). `None` — no roster,
    /// no host that keeps one, or "stay": every decision is the one made
    /// before accounts existed. What is decided on is [`Self::advised`]: a
    /// due probe's `otherwise`, never the probe.
    pub account: Option<AccountAdvice>,
}

impl TurnEndReading {
    /// THE ADVICE THE DECIDER ACTS ON ([`AccountAdvice::past_probe`] of
    /// [`Self::account`]): the one read of it every deciding site makes, so
    /// a `Probe { otherwise: X }` is decided exactly as `X` is, and a probe
    /// with nothing behind it exactly as no advice.
    #[must_use]
    pub fn advised(&self) -> Option<&AccountAdvice> {
        self.account.as_ref().and_then(AccountAdvice::past_probe)
    }

    /// A person's hand in the turn that ended here: a keystroke
    /// ([`Self::person`]) no older than the turn's busy work
    /// ([`RunningTurn::point`]) plus [`PERSON_TURN_SLACK`]. Decides whether a
    /// turn nobody here typed is a person's rather than the harness's
    /// ([`WindDown::seeded`]) or Codex's goal ([`TurnEndState::observe`]).
    #[must_use]
    pub fn person_in_turn(&self) -> bool {
        self.person.is_some_and(|ago| {
            ago <= self
                .worked
                .unwrap_or_default()
                .saturating_add(PERSON_TURN_SLACK)
        })
    }

    /// What the worker asks of a person here and whether it names an
    /// irreversible act ([`Said::Irreversible`]); `None` when nothing is asked
    /// (including after a person's Esc). An unreadable question is never
    /// "nothing asked". [`decide_turn_end`] answers by this reading.
    #[must_use]
    pub fn asked(&self) -> Option<(String, bool)> {
        if self.interrupted {
            None
        } else if self.phase == Phase::Question && self.said_tail.is_none() {
            Some((
                "the worker asked something its last words do not show".to_string(),
                false,
            ))
        } else {
            match classify_said(self.said_tail.as_deref()) {
                Said::Irreversible(why) => Some((why, true)),
                Said::Stop(why) => Some((why, false)),
                Said::Question => Some(("the worker asked a question".to_string(), false)),
                Said::Offer | Said::Plain => None,
            }
        }
    }

    /// Reads one screen: `aterm-phase`'s [`aterm_phase::Reading`] of `rows`
    /// by the session program's reader, plus the typed draft, work since the
    /// last point, the wall's reset on the loop's clock, and the rules.
    /// Claude-Code-only signals (an unanswered `❯` row, `Worked for`) are read
    /// only on Claude Code. [`Self::upgrading`] is set by the loop.
    #[must_use]
    pub fn of(
        reading: &aterm_phase::Reading,
        rows: &[String],
        typed: bool,
        worked: Option<Duration>,
        reset_at: Option<Instant>,
        rules: Option<String>,
    ) -> Self {
        let claude = reading.program == aterm_phase::Program::Claude;
        let own = reading.composer.as_ref().is_some_and(|(_, lines)| {
            let draft = lines.concat();
            crate::harness::upgrade::own_draft(&draft)
                || (reading.program == Program::Codex && codex_own_line(&draft))
        });
        let composer = if reading.composer.is_none() {
            Composer::Absent
        } else if typed && own {
            Composer::Own
        } else if typed {
            Composer::Typed
        } else if let Some(s) = &reading.suggestion {
            Composer::Placeholder(s.clone())
        } else {
            Composer::Empty
        };
        let message = reading
            .wall
            .as_ref()
            .map(|w| w.message.clone())
            .unwrap_or_default();
        let auto_continue = claude
            .then(|| aterm_phase::auto_continue_end(rows))
            .flatten();
        Self {
            phase: reading.phase.clone(),
            authoritative: reading.phase_authoritative,
            survey: reading.survey,
            wall: reading.wall.as_ref().map(|w| w.kind),
            // ...and has not said since that it will not (the limit a cancel
            // left, read as its wall: `TurnEndState::left_answer`).
            resumes_by_itself: super::super::limit::resumes_by_itself(&message)
                && auto_continue != Some(aterm_phase::AutoContinue::Unkept),
            wall_message: message,
            reset_at,
            reset_clock: reading
                .wall
                .as_ref()
                .and_then(|w| ResetClock::of(w.reset.as_deref())),
            jitter: Duration::ZERO,
            composer,
            said_tail: reading.said_tail.clone(),
            worked: worked.map(|w| {
                let done = claude.then(|| done_row_work(rows)).flatten();
                w.max(done.unwrap_or_default())
            }),
            rules,
            pending_input: claude
                && last_said_row(rows).is_some_and(|r| r.trim_start().starts_with('❯')),
            interrupted: reading.interrupted,
            restartable: false,
            resume: None,
            upgrading: false,
            taskless: reading.fresh,
            person: None,
            login_back: claude && aterm_phase::login_restored(rows),
            auto_continue,
            since_limit: claude.then(|| aterm_phase::since_limit(rows)).flatten(),
            served_in_transcript: false,
            reach: Reach::Unknown,
            program: reading.program,
            goal: (reading.program == Program::Codex)
                .then(|| aterm_phase::codex::goal_state(rows))
                .flatten(),
            model_field: (reading.program == Program::Codex)
                .then(|| aterm_phase::codex::footer_model(rows))
                .flatten()
                .map(|(model, effort)| CodexSetting { model, effort }),
            limits: LimitRead::default(),
            sandbox_fell: None,
            thread_model: None,
            upgrade_goal: false,
            account: None,
        }
    }

    /// A turn has ended and nothing blocks acting: an idle, question or limit
    /// phase from an authoritative reader, no box or survey, a composer drawn,
    /// and no unanswered input except one judged not taken (`stale_input`).
    fn at_point(&self, stale_input: bool) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::Question | Phase::Limited { .. }
        ) && self.authoritative
            && !self.survey
            && self.composer != Composer::Absent
            && (!self.pending_input || stale_input)
    }
}

/// A person's keystroke in the turn in flight when a loop carried a switch on
/// ([`WindDown::seeded_at`]): typed since the switch's last row (`row`) and no
/// older than the turn's seen work (`worked`) plus [`SEEDED_TURN_BOUND`].
/// `person` is the time since their last keystroke, at `now`.
#[must_use]
pub fn seeded_hand(
    person: Option<Duration>,
    worked: Option<Duration>,
    row: Instant,
    now: Instant,
) -> bool {
    person.is_some_and(|ago| {
        now.checked_sub(ago).is_some_and(|typed| typed >= row)
            && ago <= worked.unwrap_or_default().saturating_add(SEEDED_TURN_BOUND)
    })
}

/// The turn running now, as the loop's busy reads track it (the busy-read
/// twin of [`TurnEndReading::person_in_turn`]). Its span starts at the first
/// busy read after the last point ([`Self::point`]) or after the rate-limit
/// nudge's box ([`Self::boxed`]): the box ends the turn it covered, and what
/// runs after it (Codex's goal restarts within milliseconds) is a new turn,
/// so keystrokes from the covered turn do not count in it. Kept by the loop's
/// `Session` and the Tier-1 walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningTurn {
    /// The first busy read of the turn running now.
    since: Option<Instant>,
    /// The span of the turn the nudge's box covered, kept for the first point
    /// after it when no busy read came since (that point is its end).
    covered: Option<Instant>,
    /// A busy read saw a person's hand IN this turn: theirs until its point.
    person: bool,
}

impl RunningTurn {
    /// A busy read of the worker's own turn at `now`: the span begins at
    /// the first.
    pub fn busy(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// The first busy read of the turn running now, where one was seen.
    #[must_use]
    pub fn since(&self) -> Option<Instant> {
        self.since
    }

    /// A busy read of it saw a person's hand in the turn running now.
    #[must_use]
    pub fn latched(&self) -> bool {
        self.person
    }

    /// The point ending the turn (or a break under a Codex background terminal
    /// the switch takes as a point), at `now`: returns the turn's work (its
    /// span, else the box-covered span) and starts a fresh span; any latched
    /// person's hand is spent.
    pub fn point(&mut self, now: Instant) -> Option<Duration> {
        let own = self.since.take();
        let covered = self.covered.take();
        self.person = false;
        own.or(covered).map(|t| now.saturating_duration_since(t))
    }

    /// The nudge's box was answered (switch pressed or kept): the covered
    /// turn's span is kept for its point, what runs now is measured from its
    /// own first busy read, and a latched person's hand is cleared.
    pub fn boxed(&mut self) {
        if let Some(s) = self.since.take() {
            self.covered.get_or_insert(s);
        }
        self.person = false;
    }

    /// Whether the turn running now is a person's, at a busy read at `now`.
    /// `typed` is when a person last typed, `floor` the switch's opening
    /// ([`WindDown::opened_at`]; nothing typed before it counts), `seeded` the
    /// last row of a carried-on switch while its in-flight turn runs
    /// ([`WindDown::seeded_at`]), `grace` `human_grace_s`.
    ///
    /// * A keystroke in the turn (since the floor and within the span plus
    ///   [`PERSON_TURN_SLACK`], or for the seeded turn [`seeded_hand`]) makes
    ///   it theirs, latched until its point.
    /// * A keystroke since the floor within the grace only holds the stop
    ///   while the grace lasts; it latches nothing.
    pub fn person(
        &mut self,
        typed: Option<Instant>,
        floor: Option<Instant>,
        seeded: Option<Instant>,
        grace: Duration,
        now: Instant,
    ) -> bool {
        let typed = typed.filter(|t| floor.is_none_or(|f| *t >= f));
        let ago = typed.map(|t| now.saturating_duration_since(t));
        let busy_for = self
            .since
            .map(|s| now.saturating_duration_since(s))
            .unwrap_or_default();
        let in_turn = ago.is_some_and(|a| a <= busy_for.saturating_add(PERSON_TURN_SLACK))
            || seeded.is_some_and(|row| seeded_hand(ago, Some(busy_for), row, now));
        self.person |= in_turn;
        self.person || ago.is_some_and(|a| a < grace)
    }
}

/// What a typed command leaves owed for the next point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Nothing more.
    Nothing,
    /// A continuation, under the command's rule, once it has run.
    Continue,
    /// Escalated as soon as the command is typed.
    Escalate(String),
}

/// What [`decide_turn_end`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEndAction {
    Nothing,
    /// Decide again at `until` (a screen change first also decides again).
    WaitUntil {
        until: Instant,
        why: String,
    },
    /// Type `text` into the composer and submit it, guarded.
    Type {
        text: String,
        rule_id: &'static str,
    },
    /// Accept the worker's own suggestion (`text`): the vendor's accept key
    /// fills it, a guarded Enter submits it.
    Accept {
        text: String,
        rule_id: &'static str,
    },
    /// Type a slash command and submit it, guarded.
    TypeCommand {
        command: String,
        rule_id: &'static str,
        then: Then,
    },
    /// Submit the draft standing in the composer, as it is, in place of the
    /// act `rule_id` names (a guarded Enter): the draft is the next message.
    Submit {
        rule_id: &'static str,
    },
    /// Hand the point to a person.
    Escalate {
        reason: String,
    },
    /// Have the host end the agent, relaunch it on its own conversation and
    /// carry it on ([`Restart`]). `otherwise` applies where no restart is made:
    /// the memory banner's escalation, a bucket's reset waited out, or the
    /// continuation a reset was owed.
    Restart {
        why: Restart,
        rule_id: &'static str,
        otherwise: Box<TurnEndAction>,
    },
}

impl TurnEndAction {
    /// The rule an act is taken under (`None` for no act).
    #[must_use]
    pub fn rule_id(&self) -> Option<&'static str> {
        match self {
            TurnEndAction::Type { rule_id, .. }
            | TurnEndAction::Accept { rule_id, .. }
            | TurnEndAction::TypeCommand { rule_id, .. }
            | TurnEndAction::Submit { rule_id }
            | TurnEndAction::Restart { rule_id, .. } => Some(rule_id),
            _ => None,
        }
    }
}

/// Which wall a track follows (a new kind is a new track).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallClass {
    Retry,
    Usage,
    Model,
    Context,
    Auth,
    Memory,
}

fn class_of(kind: WallKind) -> WallClass {
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => WallClass::Retry,
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => WallClass::Usage,
        WallKind::ModelBucket { .. } => WallClass::Model,
        WallKind::Context => WallClass::Context,
        WallKind::Auth => WallClass::Auth,
        WallKind::Memory => WallClass::Memory,
    }
}

/// A wall being worked: since when it shows (this appearance), and how many
/// acts it has had.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WallTrack {
    class: WallClass,
    since: Instant,
    attempts: usize,
    /// The acts made because the host measured the API reachable again
    /// ([`RULE_API_BACK`]): the next is spaced on the ladder by their count.
    ups: usize,
    /// A full context's track has met the vendor's own word that a `/compact`
    /// could not lower it (`Compaction failed · …`, [`compaction_exhausted_row`]).
    /// Latched for the appearance: a request error on a later rung
    /// (`Error during compaction: …`) refutes nothing the vendor already said.
    exhausted: bool,
    /// The reset the wall's notice last named while it showed: what a point
    /// the wall has LEFT is decided by ([`still_shown`]).
    named: Option<NamedReset>,
    /// The wall has shown at a POINT, where its own rule decides it — not
    /// only at readings that were no point yet (an act's take window, anyone's
    /// unanswered row; [`TurnEndState::observe`]). A lost login no point saw
    /// was never decided: no `/login` typed, nobody told. Where its row has
    /// left the screen it is decided as the wall it left ([`still_shown`]),
    /// never held as one the owner was told of ([`act_at_point`]).
    pointed: bool,
    /// The wall's words as its notice last read ([`TurnEndReading::wall_message`]):
    /// what a decision about a wall that has left the screen says of it.
    said: String,
}

/// A limit's reset as its notice named it, kept while the notice shows
/// ([`TurnEndState::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NamedReset {
    kind: WallKind,
    /// The reset on the loop's clock ([`TurnEndReading::reset_at`]).
    at: Instant,
    /// This session's share of the herd ([`TurnEndReading::jitter`]).
    jitter: Duration,
    /// The vendor said it goes on by itself ([`TurnEndReading::resumes_by_itself`]),
    /// and has not said since that it will not
    /// ([`aterm_phase::AutoContinue::Unkept`]: Esc on its countdown,
    /// `Automatic continue cancelled`).
    by_itself: bool,
    /// The reset as the notice's clock said it: what ties a notice the screen
    /// draws to THIS limit ([`Self::served_under`]); `None` where it named
    /// none the clock reads (a span), and so no notice is this one's.
    said: Option<ResetClock>,
}

impl NamedReset {
    /// [`served_under`] this limit's notice.
    fn served_under(&self, since: Option<&aterm_phase::SinceLimit>) -> bool {
        served_under(since, self.kind, self.said)
    }
}

/// Whether the screen's newest limit notice (`since`,
/// [`aterm_phase::since_limit`]) is THE NOTICE OF THE LIMIT `kind` whose
/// reset read `said` — of its class, naming the same reset — and a turn was
/// served since it: the limit lifted before its reset, and the turn is work
/// since it. Another limit's notice — an older one a `/resume`d conversation
/// redraws with turns served under it (the review of 2026-10-14, the lab on
/// 2.1.300: read as this one's, the continuation was typed into the standing
/// limit) — says nothing of this one, nor does any notice where `said` is
/// `None` (a reset no clock names).
#[must_use]
pub fn served_under(
    since: Option<&aterm_phase::SinceLimit>,
    kind: WallKind,
    said: Option<ResetClock>,
) -> bool {
    since.is_some_and(|s| {
        s.served
            && class_of(s.kind) == class_of(kind)
            && said.is_some()
            && ResetClock::of(s.reset.as_deref()) == said
    })
}

/// Whether `message`, the notice a full context's wall stands on, is the vendor's own word that a `/compact` could not
/// lower the context (its head, `Compaction failed`, from the anchor table the canary checks against the binary).
/// `Error during compaction: <detail>` is a request that failed, and no word on the conversation.
fn compaction_exhausted_row(message: &str) -> bool {
    message
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .starts_with(&aterm_phase::anchor("wall.compaction_failed").to_lowercase())
}

/// A model switch this policy made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitch {
    /// The bucket's model, as a `/model` alias (`None`: the notice named
    /// none this policy knows, and nothing is switched back).
    pub from: Option<String>,
    pub to: String,
    /// The bucket's reset (`None`: it named none).
    pub back_at: Option<Instant>,
}

/// A Codex thread's model and reasoning effort as its footer shows them
/// (`GPT-6-Astra ultra`): the display name the `/model` picker lists, the
/// effort lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSetting {
    pub model: String,
    pub effort: Option<String>,
}

impl CodexSetting {
    /// `GPT-6-Astra ultra`.
    #[must_use]
    pub fn words(&self) -> String {
        match &self.effort {
            Some(e) => format!("{} {e}", self.model),
            None => self.model.clone(),
        }
    }

    /// The same model and effort, case and `extra high`/`xhigh` aside.
    #[must_use]
    pub fn same(&self, other: &CodexSetting) -> bool {
        let e = |x: &Option<String>| {
            x.as_deref().map(|e| {
                let e = e.trim().to_lowercase();
                if e == "extra high" {
                    "xhigh".to_string()
                } else {
                    e
                }
            })
        };
        self.model.trim().eq_ignore_ascii_case(other.model.trim())
            && e(&self.effort) == e(&other.effort)
    }

    /// Whether the thread runs `model` (a slug or a display name), whatever
    /// its effort.
    #[must_use]
    pub fn runs(&self, model: &str) -> bool {
        Self::slug_runs(&self.model, model)
    }

    /// Whether two names of a model — slugs (`gpt-6-luna`, a rollout's
    /// `turn_context`) or display names (`GPT-6-Luna`, the footer) — name the
    /// same one.
    #[must_use]
    pub fn slug_runs(a: &str, b: &str) -> bool {
        let key = |m: &str| {
            m.trim()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-")
        };
        key(a) == key(b)
    }
}

/// Where a Codex save-then-wait switch stands ([`WindDown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindPhase {
    /// W1: the instruction to save the work is owed at the next free point
    /// (Codex's goal stopped first).
    Owed,
    /// W2: the wind-down's own turn runs on the cheaper model.
    Winding,
    /// W3: the thread is owed its own model and effort back — then the hold
    /// (`hold`), or nothing more (a person took the session over).
    Restore { hold: bool },
    /// W4: back on its own model, nothing typed until the window resets.
    Holding { since: Instant },
}

impl WindPhase {
    /// The ledger's word for it (`phase=<word>`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            WindPhase::Owed => "owed",
            WindPhase::Winding => "winding",
            WindPhase::Restore { hold: true } => "restore",
            WindPhase::Restore { hold: false } => "restore-free",
            WindPhase::Holding { .. } => "holding",
        }
    }
}

/// A Codex session's save-then-wait switch (module header): opened by the
/// approval policy's press of the rate-limit nudge
/// ([`TurnEndState::open_switch`]), then wound down, restored and held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindDown {
    /// The thread's own model and effort before the switch.
    pub from: CodexSetting,
    /// The cheaper model the nudge's switch named.
    pub to: String,
    /// When the original model's window resets (`None`: nobody read it).
    pub back_at: Option<Instant>,
    pub phase: WindPhase,
    /// The harness stopped Codex's goal (its Esc, or `/goal pause`): it owes
    /// `/goal resume` at the reset.
    pub goal_paused: bool,
    /// `/goal pause` was typed (once).
    pub pause_typed: bool,
    /// The wind-down's answer carried the marker (`None`: not judged yet).
    pub saved: Option<bool>,
    /// The one-time line the wind-down answers with when all is saved
    /// ([`crate::harness::upgrade::saved_marker`]).
    pub marker: String,
    /// `/model` restores typed.
    pub restore_tries: u32,
    /// When the last `/model` restore was typed.
    pub restore_at: Option<Instant>,
    /// The notes said already, by key: each is said once per switch.
    pub said: Vec<&'static str>,
    /// The first point judged after the switch ends the turn that ran before
    /// it (the box was up over its end): no person's turn of the switch's.
    pub pre: bool,
    /// Carried on from a previous loop's rows ([`TurnEndState::seed_wind`]):
    /// the turn ending at the first judged point is the harness's own unless
    /// it shows a person's hand ([`TurnEndReading::person_in_turn`]).
    pub seeded: bool,
    /// The harness's stops of Codex's goal since the switch opened (Esc or
    /// `/goal pause`), at most [`GOAL_STOPS`] ([`TurnEndState::goal_stop`]).
    pub stops: u32,
    /// When the harness's last Esc went, until a point shows it took
    /// ([`TurnEndState::goal_stop`] sends no second Esc into the same turn).
    pub stop_at: Option<Instant>,
    /// The harness's `/model` picker has been seen since its `/model` went
    /// ([`TurnEndState::picker_seen`]).
    pub restore_shown: bool,
    /// When a read first showed the harness's picker gone (`None`: up or not
    /// seen); its claim ends after [`PICKER_SETTLE`] ([`TurnEndState::picker_seen`]).
    pub picker_gone_at: Option<Instant>,
    /// The press is only intended (`phase=intent`, ledgered before its key): a
    /// restarted loop does not know whether the key went, so a nudge still up
    /// is decided again, nothing is stopped or typed until the footer shows
    /// the cheaper model ([`TurnEndState::footer_seen`]), and a footer on the
    /// thread's own model at a point closes the switch.
    pub intent: bool,
    /// Since when an owed switch's points have waited on the footer (no model
    /// shown, or the own model while the rollout says the cheaper one); past
    /// [`OWED_FOOTER_BOUND`] a person is told.
    pub footer_since: Option<Instant>,
    /// When a switch carried on from the ledger was last written
    /// ([`TurnEndState::seed_wind`]), until the first judged point: a person's
    /// keystroke since then, within [`SEEDED_TURN_BOUND`] ([`seeded_hand`]), is
    /// their hand in the turn in flight.
    pub seeded_at: Option<Instant>,
    /// When the switch opened (the box seen leaving, an intended press, or for
    /// a carried-on switch the opening its rows name, else its last row's
    /// time). Keystrokes before it are no hand in any turn the switch reads
    /// ([`RunningTurn::person`], [`TurnEndReading::person`]). `None`: no floor.
    pub opened_at: Option<Instant>,
    /// When the nudge's switch was pressed (`pressed=`, ledgered just before
    /// the key): the rollout counts only for a turn begun since
    /// ([`TurnEndReading::thread_model`], one second's slack), since Codex's
    /// goal turn on the cheaper model starts between the press and the box
    /// leaving. `None`: [`Self::opened_at`] is the floor.
    pub pressed_at: Option<Instant>,
    /// The save's busy work in turns that ended on an API error or full
    /// context and were carried on, added to the current span when measured
    /// against [`WIND_DOWN_BOUND`] ([`TurnEndState::goal_stop`]). Not in the
    /// ledger: a restarted loop counts from its own first busy read.
    pub wound: Duration,
    /// The harness's Esc went into the save's turn past [`WIND_DOWN_BOUND`]
    /// (once a switch): a save still running after it is said to a person.
    pub overran: bool,
}

impl WindDown {
    /// A switch opened by the nudge's press, owing its wind-down: `from` the
    /// thread's model and effort, `to` the cheaper model, `back_at` the
    /// original window's reset (`None`: not read), `marker` the line a saved
    /// wind-down answers with. Its first point ends the box-covered turn
    /// ([`Self::pre`]).
    #[must_use]
    pub fn opened(
        from: CodexSetting,
        to: String,
        back_at: Option<Instant>,
        marker: String,
    ) -> Self {
        Self {
            from,
            to,
            back_at,
            phase: WindPhase::Owed,
            goal_paused: false,
            pause_typed: false,
            saved: None,
            marker,
            restore_tries: 0,
            restore_at: None,
            said: Vec::new(),
            pre: true,
            seeded: false,
            stops: 0,
            stop_at: None,
            restore_shown: false,
            picker_gone_at: None,
            intent: false,
            footer_since: None,
            seeded_at: None,
            opened_at: None,
            pressed_at: None,
            wound: Duration::ZERO,
            overran: false,
        }
    }
}

/// The ledger's `phase=<word>` for `w`: `intent` for an owed switch whose
/// press is only intended ([`WindDown::intent`]), so every row written then
/// reads back as intent, never a landed switch; else [`WindPhase::word`].
#[must_use]
pub fn wind_phase_word(w: &WindDown) -> &'static str {
    if w.intent && w.phase == WindPhase::Owed {
        "intent"
    } else {
        w.phase.word()
    }
}

/// How many `/model` restores are typed before the restore is handed to a
/// person with its steps.
pub const RESTORE_TRIES: u32 = 2;

/// How many times the harness stops Codex's goal while a switch is open (Esc
/// or `/goal pause`) before a person is told ([`TurnEndState::goal_stop`]).
pub const GOAL_STOPS: u32 = 3;

/// How far past a turn's busy work a person's keystroke still counts as their
/// hand in it (the message sent just before the turn began).
pub const PERSON_TURN_SLACK: Duration = Duration::from_secs(30);

/// The most of a carried-on switch's in-flight turn the restarted loop is
/// assumed not to have seen ([`seeded_hand`]): loops restart in seconds and a
/// goal turn runs minutes. Heuristic: a person's own turn that ran longer
/// than this out of sight is not read as theirs.
pub const SEEDED_TURN_BOUND: Duration = Duration::from_secs(30 * 60);

/// The head of the note that Codex's goal runs on after every stop
/// ([`TurnEndState::goal_told`]): the loop keeps the badge that carries it up
/// past the goal's next turn.
pub const GOAL_NOTE_HEAD: &str = "Codex's goal keeps running";

/// How long the harness's `/model` picker must stay gone before its claim
/// ends ([`TurnEndState::picker_seen`]): the restore passes through several
/// boxes, and a frame between them is no picker left.
pub const PICKER_SETTLE: Duration = Duration::from_secs(2);

/// How long an owed switch's points may wait on the footer (showing no model,
/// or the own model while the rollout says the cheaper one) before a person
/// is told.
pub const OWED_FOOTER_BOUND: Duration = Duration::from_secs(5 * 60);

/// The save's turn bound: past this much busy work since the save instruction
/// was typed, the loop stops it with one Esc ([`TurnEndState::goal_stop`]);
/// its interrupted point still judges the marker, and the own model is
/// restored and held. The cheaper model only commits and pushes; this leaves
/// room for a slow hook or a large push (owner, 2026-09-28).
pub const WIND_DOWN_BOUND: Duration = Duration::from_secs(15 * 60);

/// A note of the switch's that a restarted loop reads back as said
/// ([`WindDown::said`], the ledger's `told=`): its key as the `'static` word
/// the switch keeps, `None` for a word no note of this build's has.
#[must_use]
pub fn said_key(word: &str) -> Option<&'static str> {
    [
        "goal",
        "unsaved",
        "restore",
        "draft",
        "footer",
        "background",
        "winding",
    ]
    .into_iter()
    .find(|k| *k == word)
}

/// What holds a save-then-wait switch still through a break under a Codex
/// background terminal ([`TurnEndState::switch_stood_still`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoodBy {
    /// A person typed within their grace: the switch's act waits for them.
    Typing,
    /// A draft stands in the composer: nothing is typed over it.
    Draft,
    /// Nothing of a person's: the terminal holds the session busy, and what
    /// the switch typed there has not moved it.
    Terminal,
    /// The policy's own wait for the footer to show the cheaper model
    /// ([`TurnEndState::switch_hold`]).
    Footer,
    /// The policy's own: the thread fell into a sandbox its launch bypassed,
    /// and nothing of the switch is typed into it.
    Sandbox,
    /// The policy's own: its `/model` restores are spent
    /// ([`RESTORE_TRIES`]), and the hand steps were said.
    RestoreSpent,
}

/// What the harness does about a turn running while a switch is open
/// ([`TurnEndState::goal_stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStop {
    /// Stop it: one Esc guarded on its status row.
    Esc,
    /// The stops are spent and it runs on: a person is told, once.
    Tell,
}

/// What the save-then-wait machine hands its loop ([`TurnEndState::take_wind_events`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindEvent {
    /// A switch edge no typed act made (the save judged, the model seen
    /// back, a person's release, the end): ledgered `skipped` under `rule`,
    /// `what`, with the switch as `wind` leaves it and its phase word.
    Edge {
        rule: &'static str,
        what: String,
        wind: WindDown,
        phase: &'static str,
    },
    /// A typed act of the switch's: its ledger row carries the switch as it
    /// stands after it (`phase`).
    Typed { wind: WindDown, phase: &'static str },
    /// Told to a person, once.
    Note(String),
    /// Codex's goal runs on after every stop was spent: told once and kept up
    /// while the switch stands, recorded by the host where a person looks
    /// ([`crate::supervise::IdleHost::inform`]).
    GoalNote(String),
    /// The hold began: back on the own model, waiting for the window's reset.
    /// Recorded for a person as information, no badge.
    Hold { wind: WindDown },
}

/// An act typed and its point not judged yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Awaited {
    rule: &'static str,
    /// A slash command, whose effect needs no turn.
    command: bool,
    /// When it was typed.
    at: Instant,
    /// When a point after it first showed with no busy read since
    /// ([`TurnEndState::observe`]).
    unseen_since: Option<Instant>,
    /// A read since it was typed saw the agent's live turn (spinner or busy
    /// footer, not a background-work break; [`TurnEndState::took`]): the act
    /// was taken, judged or not ([`TurnEndState::act_untaken`]).
    taken: bool,
    /// A WALL ANSWERED IT: a reading since it was typed, no point yet (its
    /// take window open), showed a wall, and no read has seen it taken since
    /// ([`TurnEndState::observe`]). The point that judges it judges a retry
    /// the wall answered MET, whatever that point shows: its notice gone by
    /// the window's end (Esc on the vendor's new countdown) left the wall
    /// standing, and never says the retry got past it.
    met: bool,
    /// THE LIMIT A CANCEL LEFT on the screen the act was typed at
    /// ([`left_key`]), if one stood there: a notice the screen draws after
    /// the act is its answer only where it is not that one
    /// ([`TurnEndState::left_answer`]).
    left: Option<LeftKey>,
}

/// What tells one left limit's notice from another's: its class and the
/// reset its clock names ([`ResetClock`]).
type LeftKey = (WallClass, Option<ResetClock>);

/// THE LIMIT A CANCEL LEFT, by what the screen draws of it: the vendor's word
/// that its auto-continue will not go on (`auto`,
/// [`aterm_phase::AutoContinue::Unkept`]) under the newest usage limit
/// notice (`since`, [`aterm_phase::since_limit`]) that names its reset and
/// that no turn has been served since. `None` for anything else.
fn left_key(
    since: Option<&aterm_phase::SinceLimit>,
    auto: Option<aterm_phase::AutoContinue>,
) -> Option<LeftKey> {
    since
        .filter(|s| {
            auto == Some(aterm_phase::AutoContinue::Unkept)
                && !s.served
                && s.reset.is_some()
                && class_of(s.kind) == WallClass::Usage
        })
        .map(|s| (class_of(s.kind), ResetClock::of(s.reset.as_deref())))
}

/// A LIMIT A CANCEL LEFT, read as the answer to a turn asked since the policy
/// last saw the screen ([`TurnEndState::left_answer`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftAnswer {
    /// The wall its notice was before the cancel ([`aterm_phase::left_wall`]).
    pub wall: aterm_phase::Wall,
    /// When the turn it answered was asked, at the earliest: the act
    /// awaited, else the policy's last look. Its notice, to be that turn's
    /// answer, was written no earlier.
    pub since: Instant,
    /// The SCREEN tells it from what the policy already saw: the notice it
    /// draws is not the one the act was typed over (else the one the last
    /// look drew). Where it is — the same limit, the same reset — a screen
    /// that has not moved since draws the same, and only a transcript row
    /// written since `since` tells an answer from it.
    pub screen_tells: bool,
}

impl Awaited {
    /// When its point is judged with no busy read, or it is escalated as
    /// not taken: [`TurnEndTiming::take_within`] from the first point that
    /// showed no busy read, else from the act.
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// The continuation a limit reset left owed ([`TurnEndState::reset_told`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetResume {
    /// When the outcome was told: work that began at or after it pays the
    /// continuation, work that began before it is the turn that met the wall.
    told: Instant,
    /// From when the continuation is typed.
    due: Instant,
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    /// When each typed act went, within the window.
    acts: VecDeque<Instant>,
    /// The wall acts typed within the window, by rule
    /// ([`WALL_ACTS_PER_WINDOW`]).
    wall_acts: VecDeque<(Instant, &'static str)>,
    /// The act typed last — this policy's, or a turn its host typed
    /// ([`Self::host_typed`]) — until the next point shows what it did.
    awaiting: Option<Awaited>,
    /// The act the current point follows (`None`: someone else's turn).
    followed: Option<&'static str>,
    /// The busy work of the turn that ended at the current point.
    point_worked: Duration,
    /// Short turns in a row ([`TurnEndTiming::min_work`]): this policy's
    /// acts' yields and anyone else's turns alike.
    short: u32,
    /// When the current point first showed: the back-off and a person's
    /// Esc grace count from it.
    point_at: Option<Instant>,
    wall: Option<WallTrack>,
    /// A command's rule whose continuation is owed at the next point.
    after: Option<&'static str>,
    model: Option<ModelSwitch>,
    /// The fallback relaunch for the bucket being worked could not be made
    /// ([`TurnEndState::restarted`]): its reset is waited out instead.
    fallback_unmade: bool,
    /// THE ACCOUNT MOVE THIS SESSION MADE and no point has answered yet
    /// ([`Self::restarted`], [`Self::account_answer`]): from the relaunch
    /// until a turn asked under the new login is served, or — after a wall or
    /// a lost login answered it — until the worker works again. While it
    /// stands no move is asked AHEAD of a wall (the move made is answered
    /// first), and a lost login is the move's, not the person's
    /// ([`login_wall_moves`]).
    account: Option<AccountMove>,
    /// A turn has been asked of the agent since that move — the host's
    /// carry-on, an act of this policy's, a turn of anyone's that ran: only
    /// then does a usage limit on the screen say anything about the NEW
    /// account (a relaunch resumes the conversation, and what it repaints of
    /// the old account's notice was never measured).
    account_asked: bool,
    /// The host was told what answered the move ([`Self::account_told`]):
    /// never told twice.
    account_told: bool,
    /// The relaunch of that move has shown no point yet: the FIRST point
    /// after it is the resumed process's own start-up, whatever work it
    /// shows, and no turn anyone asked ([`Self::observe`]).
    account_startup: bool,
    /// THE WALL'S RESUME IS OWED: the move was made AT A WALL
    /// ([`AccountAt::Wall`]) and no turn has been asked under the new login
    /// since. The continuation the wall would have had at its reset is typed
    /// at the relaunched session's first point ([`act_at_point`],
    /// [`wall_action`]) — under `resume_limits`, by [`RULE_LIMIT_RESUME`],
    /// whatever `continue_policy` says. The host's carry-on, an act of this
    /// policy's or a turn of anyone's pays it.
    account_resume: bool,
    /// That resume was SEEDED from the ledger ([`Self::seed_account`]) and
    /// no point has been folded in since: owed only if the first point this
    /// state sees is the notice the session left ([`Self::observe`]).
    account_resume_seed: bool,
    /// The owner's clearing ([`AccountAdvice::Resume`]'s `cleared_at`) this
    /// policy last typed the wall's continuation for — or the unix second a
    /// loop before this one typed it ([`Self::seed_account_resumed`]): every
    /// clearing at or before it is answered, one continuation a clearing
    /// ([`account_first`]).
    account_resumed: Option<i64>,
    /// A LIMIT RESET'S CONTINUATION IS OWED ([`Self::reset_told`]): the
    /// step the loop made under the usage-limit dialog ended, and the session
    /// is to be continued once — by [`RULE_LIMIT_RESUME`] under
    /// `resume_limits`, whatever `continue_policy` says ([`act_at_point`],
    /// [`wall_action`]) — from the instant it names. Paid by any act typed, a
    /// turn the host typed, and any work seen since the outcome was told
    /// ([`Self::took`], [`Self::observe`]).
    reset_resume: Option<ResetResume>,
    /// An account move asked for could not be made — the host refused it, or
    /// the relaunch failed ([`Self::restarted`]): not asked again while the
    /// wall being worked stands. THE LATCH IS THAT WALL'S: cleared where
    /// [`Self::fallback_unmade`] is (a new wall, the wall gone with the
    /// worker working), where a new episode of the same class begins after
    /// real work ([`Self::observe`]), by a move made, and by the accounts'
    /// news ([`Self::account_news`]).
    account_unmade: bool,
    /// That latch was SEEDED from the ledger ([`Self::seed_account_unmade`])
    /// and no point has been folded in since: the first wall this state sees
    /// is the wall the earlier loop left standing, and takes the latch up
    /// where a wall of this loop's own finding would clear it.
    account_unmade_seed: bool,
    /// The label of a move asked AHEAD of a wall that could not be made, and
    /// when: not asked again for that label — the refusal is the launch's (a
    /// flag the relaunch cannot carry, a line that cannot hold the login) or
    /// the roster's, and neither changes from one turn end to the next,
    /// where the wall's latch would be cleared by the very turn that
    /// follows. A move made, or advice naming another account, is a new ask
    /// — and so is the same account once the window has passed
    /// ([`Self::account_ahead_refused`]): no wall stands here whose end
    /// would end the latch, and a refusal of the moment (a login found
    /// signed out and signed in since, a store that could not be read, a
    /// restart of another kind that was in flight) kept for the life of the
    /// loop left the owner's `accounts use` unheard by this one tab.
    account_ahead_unmade: Option<(String, Instant)>,
    /// When each account move this session MADE went, within the window
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]).
    account_moves: VecDeque<Instant>,
    /// The `❯` row on the screen is an act of this policy's the worker never
    /// took (judged so at its deadline): a point, though input waits.
    stale_input: bool,
    /// Codex's save-then-wait switch, while one is open.
    wind: Option<WindDown>,
    /// How long the Mac slept since the switch's hold began ([`Self::slept`]):
    /// time on the hold's bound for a reset nobody read
    /// ([`TurnEndTiming::unknown_hold`]), which is five hours of the wall
    /// clock's. The hold's start ([`WindPhase::Holding`]) is not moved — a
    /// person's keystroke since it is read on an awake clock — so the bound
    /// takes the sleep from here.
    hold_slept: Duration,
    /// The harness's own Esc stopped the turn now showing: its interrupt is
    /// no person's.
    own_interrupt: Option<Instant>,
    /// What the switch hands the loop ([`Self::take_wind_events`]).
    wind_events: Vec<WindEvent>,
    /// Done reports in a row ([`says_done`]): `1` gets the [`DONE_CHECK`];
    /// `2` means the task is done ([`Self::task_done`]) until someone else's
    /// turn.
    done: u8,
    /// The act awaited is the [`DONE_CHECK`] as this policy typed it (a draft
    /// submitted in its place is someone's own words, never the check).
    checking: bool,
    /// The task ended by the worker's own [`FINISHED`] token (not the check).
    finished: bool,
    /// THE POLICY'S LAST LOOK: when it last folded a reading in
    /// ([`Self::observe`]), and the limit a cancel left on that screen if one
    /// stood there ([`left_key`]). A cancelled limit the screen draws after
    /// it answered a turn asked since — a person's, the host's, the one an
    /// act of ours led to ([`Self::left_answer`]).
    looked: Option<(Instant, Option<LeftKey>)>,
}

impl Default for TurnEndState {
    fn default() -> Self {
        Self::new(TurnEndTiming::default())
    }
}

impl TurnEndState {
    #[must_use]
    pub fn new(timing: TurnEndTiming) -> Self {
        Self {
            timing,
            acts: VecDeque::new(),
            wall_acts: VecDeque::new(),
            awaiting: None,
            followed: None,
            point_worked: Duration::ZERO,
            short: 0,
            point_at: None,
            wall: None,
            after: None,
            model: None,
            fallback_unmade: false,
            account: None,
            account_asked: false,
            account_told: false,
            account_startup: false,
            account_resume: false,
            account_resume_seed: false,
            account_resumed: None,
            reset_resume: None,
            account_unmade: false,
            account_unmade_seed: false,
            account_ahead_unmade: None,
            account_moves: VecDeque::new(),
            stale_input: false,
            wind: None,
            hold_slept: Duration::ZERO,
            own_interrupt: None,
            wind_events: Vec::new(),
            done: 0,
            checking: false,
            finished: false,
            looked: None,
        }
    }

    /// A model switch a previous loop on the session made and did not undo
    /// (the approval ledger's open `/model` row): switched back at its reset
    /// as if this loop had made it. A switch of this loop's own wins.
    pub fn seed_model(&mut self, switch: ModelSwitch) {
        if self.model.is_none() {
            self.model = Some(switch);
        }
    }

    /// The account moves a previous loop on the session MADE, `ages` ago
    /// (the approval ledger's `typed` rows of [`RULE_ACCOUNT`]): counted by
    /// [`ACCOUNT_MOVES_PER_WINDOW`] like this loop's own while they are
    /// within the window — a loop its host starts again (each seamless
    /// update, each fault) is no fresh four.
    pub fn seed_account_moves(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.account_moves.drain(..));
        at.sort_unstable();
        self.account_moves = at.into();
        while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
            self.account_moves.pop_front();
        }
    }

    /// An account move a previous loop on the session made, `ago` ago, that
    /// its ledger does not show served (the approval ledger's open `relaunch
    /// under account …` row): still to be answered, as if this loop had made
    /// it — `asked` where a turn has been asked under the new login since
    /// (a `typed` row follows it), `told` where the host was told of a wall
    /// or a lost login that answered it (never told twice, by any loop). A
    /// move of this loop's own wins, and one older than the window is
    /// history.
    ///
    /// THE WALL'S RESUME ([`Self::account_resume_owed`]) of a move made AT A
    /// WALL (`at`) that the ledger shows neither asked nor told is taken up
    /// AS FAR AS THE NOTICE GOES: owed if the first point this state sees is
    /// the usage limit the session left ([`Self::observe`]) — the loop
    /// started right after the relaunch, or takes the agent a host's carry
    /// landed, and the resumed conversation repaints the old account's
    /// notice: not owed there, the host's carry-on was withheld until THAT
    /// account's reset. At any other first point it is not taken up: the
    /// relaunch's own carry-on is the host's to keep owed, and a turn the
    /// host typed leaves no row to say it went.
    pub fn seed_account(
        &mut self,
        to: String,
        at: AccountAt,
        ago: Duration,
        asked: bool,
        told: bool,
        now: Instant,
    ) {
        if self.account.is_none() && ago < self.timing.window {
            self.account = Some(AccountMove {
                to,
                at: now.checked_sub(ago).unwrap_or(now),
            });
            self.account_asked = asked;
            self.account_told = told;
            self.account_startup = false;
            self.account_resume = at == AccountAt::Wall && !asked && !told;
            self.account_resume_seed = self.account_resume;
        }
    }

    /// An account move a previous loop asked for could not be made, and
    /// nothing has ended the wall it was asked at since (the approval
    /// ledger's `skipped` row of [`RULE_ACCOUNT`], no act and no wall's end
    /// after it): not asked again by this loop either, while that wall
    /// stands.
    pub fn seed_account_unmade(&mut self) {
        self.account_unmade = true;
        self.account_unmade_seed = true;
    }

    /// Acts a previous loop on the session typed, `ages` ago (the approval
    /// ledger's `typed` rows): counted by the budget like this loop's own
    /// while they are within the window. Nothing else is restored — the
    /// short streak and a wall's attempts are read from the screen again.
    pub fn seed_acts(&mut self, ages: &[Duration], now: Instant) {
        let mut at: Vec<Instant> = ages
            .iter()
            .filter(|&&a| a < self.timing.window)
            .filter_map(|&a| now.checked_sub(a))
            .collect();
        at.extend(self.acts.drain(..));
        at.sort_unstable();
        self.acts = at.into();
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
    }

    /// A usage-resume a previous loop on the session typed
    /// `ago` ago (the approval ledger's `typed` row of [`RULE_LIMIT_RESUME`]):
    /// the wall it met again is worked as this loop's own retry would be —
    /// its next try the back-off from that act — so a loop restarted at a wall
    /// (each seamless update, each fault) does not type its continuation
    /// again the moment it attaches. Nothing is restored for an act older
    /// than the ladder's last rung: that wall is a new appearance. A track
    /// the screen contradicts (another kind of wall, work) is replaced by
    /// [`Self::observe`] as any other.
    pub fn seed_usage_attempt(&mut self, ago: Duration, now: Instant) {
        if self.wall.is_some() || self.timing.limit_backoff.last().is_none_or(|&b| ago >= b) {
            return;
        }
        self.wall = Some(WallTrack {
            class: WallClass::Usage,
            since: now.checked_sub(ago).unwrap_or(now),
            attempts: 1,
            ups: 0,
            exhausted: false,
            named: None,
            // A loop before this one acted at it.
            pointed: true,
            said: String::new(),
        });
    }

    /// A usage limit a previous loop on the session waited on and no loop has
    /// ended (the approval ledger's open `wall-wait@v1` row,
    /// [`crate::supervise::approvals::open_wall`], written at `since` on
    /// this loop's clock): its notice named `kind`'s reset at `at` — its
    /// clock `said` ([`ResetClock`]) — this session's `jitter` on top, and
    /// said the vendor goes on by itself, or not (`by_itself`). Waited on
    /// as this loop's own would be, its notice on the screen or not
    /// ([`still_shown`]): a loop restarted while it is off the screen (Esc on
    /// the vendor's countdown leaves an idle screen with no wall) waits for
    /// that reset still, where with nothing seeded the continue policy typed
    /// into the standing limit. The usage-resume a previous loop typed at it
    /// ([`Self::seed_usage_attempt`]) keeps its attempt; another track — and
    /// any track the screen contradicts — is [`Self::observe`]'s as ever.
    pub fn seed_named_reset(
        &mut self,
        kind: WallKind,
        at: Instant,
        jitter: Duration,
        by_itself: bool,
        said: Option<ResetClock>,
        since: Instant,
    ) {
        if class_of(kind) != WallClass::Usage {
            return;
        }
        let named = Some(NamedReset {
            kind,
            at,
            jitter,
            by_itself,
            said,
        });
        match &mut self.wall {
            Some(t) if t.class == WallClass::Usage => {
                t.named = t.named.or(named);
            }
            Some(_) => {}
            None => {
                self.wall = Some(WallTrack {
                    class: WallClass::Usage,
                    since,
                    attempts: 0,
                    ups: 0,
                    exhausted: false,
                    named,
                    // A loop before this one waited on it.
                    pointed: true,
                    said: String::new(),
                });
            }
        }
    }

    /// Whether a usage limit whose notice named its reset is waited on: its
    /// track stands — the worker has not worked since, the vendor has not
    /// gone on — whether its notice shows or has left the screen
    /// ([`still_shown`]). While it is, the loop keeps the wall open for a loop
    /// that follows ([`Self::seed_named_reset`]).
    #[must_use]
    pub fn waits_on_named_reset(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Usage && t.named.is_some())
    }

    /// A usage-resume a previous loop on the session typed at the unix
    /// second `typed_unix` (the approval ledger's last `typed` row of
    /// [`RULE_LIMIT_RESUME`]): every clearing of the owner's made at or
    /// before it has had its continuation ([`AccountAdvice::Resume`]: one a
    /// clearing, whichever loop typed it). A clearing made after it is a new
    /// one. Which clearing was answered is this state's memory alone, and a
    /// loop restarted at the same notice typed that continuation again.
    pub fn seed_account_resumed(&mut self, typed_unix: i64) {
        self.account_resumed = Some(
            self.account_resumed
                .map_or(typed_unix, |typed| typed.max(typed_unix)),
        );
    }

    /// The typed acts within the window ending at `now`.
    #[must_use]
    pub fn acts_in_window(&self, now: Instant) -> usize {
        self.acts
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < self.timing.window)
            .count()
    }

    /// How many acts of wall rule `rule` this policy typed within the window
    /// ending at `now`, and when the oldest of them leaves it
    /// ([`WALL_ACTS_PER_WINDOW`]).
    #[must_use]
    pub fn wall_acts_of(&self, rule: &str, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &(t, r) in &self.wall_acts {
            if r == rule && now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(t + window));
            }
        }
        (n, oldest)
    }

    /// Short turns in a row (the conformance walk's projection).
    #[must_use]
    pub fn short_streak(&self) -> u32 {
        self.short
    }

    /// Done reports in a row, the check's yield included (`0`, `1`, `2`: the
    /// conformance walk's projection).
    #[must_use]
    pub fn done_reports(&self) -> u8 {
        self.done
    }

    /// The task is done: the worker said so, got the [`DONE_CHECK`], and said
    /// so again. Nothing is typed ([`decide_turn_end`]) until someone else's
    /// turn ends at a point this state sees.
    #[must_use]
    pub fn task_done(&self) -> bool {
        self.done >= 2
    }

    /// The task is done because the worker ended its report with
    /// [`FINISHED`] (rather than by answering the [`DONE_CHECK`]).
    #[must_use]
    pub fn finished_by_worker(&self) -> bool {
        self.task_done() && self.finished
    }

    /// The back-off before the next act after the short streak: none after
    /// real work, else [`TurnEndTiming::short_backoff`] doubled for each
    /// short turn after the first, at most [`TurnEndTiming::
    /// short_backoff_max`].
    #[must_use]
    pub fn short_wait(&self) -> Option<Duration> {
        let n = self.short.checked_sub(1)?;
        let t = &self.timing;
        Some(crate::supervise::ladder::doubling(
            t.short_backoff,
            n,
            t.short_backoff_max,
        ))
    }

    /// An act of this policy's is in flight: typed and its point not judged,
    /// or a command's continuation owed. A host-typed turn
    /// ([`Self::host_typed`]) is not: the host's next step reads its own
    /// evidence.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some_and(|a| a.rule != RULE_HOST_TURN) || self.after.is_some()
    }

    /// An act of this policy's the agent has not taken yet: typed with no
    /// live turn seen since ([`Self::took`]), or a command's continuation
    /// owed. Holds back the host's step at a break of the agent's background
    /// work (`Session::host_steps_in_background`), so no host line is stacked
    /// on an unread continuation; once a live turn is seen, the break is the
    /// agent's own. The following point still judges the act
    /// ([`Self::observe`]); a host line typed at the break
    /// ([`Self::host_typed`]) takes its place.
    #[must_use]
    pub fn act_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.rule != RULE_HOST_TURN && !a.taken)
            || self.after.is_some()
    }

    /// A `/compact` of this policy's is running: typed, and its point not
    /// judged yet. The spinner a busy read shows meanwhile is the compaction's
    /// own run, no sign that the agent works ([`Self::observe`] counts it no
    /// work either), so it ends no exception that stands over the wall
    /// ([`exception_for`](super::exception::exception_for)). The continuation
    /// that follows it is a text act, and its spinner is the agent's.
    #[must_use]
    pub fn compacting(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.command && a.rule == RULE_COMPACT)
    }

    /// When the act this policy typed last went, while it stands UNTAKEN: no
    /// read since saw the agent's live turn ([`Self::took`]) — inside its
    /// take window or past it. A slash command (no turn of its own) and a
    /// turn the host typed are not such an act. `None` otherwise.
    ///
    /// A usage wall on the screen at such a read ANSWERED the act: Claude Code
    /// holds a line typed into a standing limit and runs no turn for it, and
    /// takes one typed after the limit lifted (a busy read). The limit stands
    /// NOW, which is what the loop's placement of the wall's reset reads it as
    /// (a print it watched, `Session::turn_end_reading`): a bare clock is the
    /// next one, never a reset the session rule guessed passed. Inside the
    /// window as past it (round 16's review): the stub's 429 answers the line
    /// at once, and the wall read there is tracked ([`Self::observe`]), so a
    /// placement made there is the one its notice leaves behind.
    #[must_use]
    pub fn untaken_act(&self) -> Option<Instant> {
        self.awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken)
            .map(|a| a.at)
    }

    /// THE LIMIT A CANCEL LEFT, ANSWERING A TURN NO READ SAW ANSWERED (round
    /// 16's residual (a), the lab's `tw16after` on 2.1.300; round 17's residual (a)): a
    /// limit answered a turn at once, and a person's Esc on the vendor's
    /// countdown came before the loop's first read after it — so that read
    /// shows only `⏺ Automatic continue cancelled` under the notice, no wall,
    /// and the limit was never tracked: the continue policy typed `keep
    /// going` into it. The limit a cancel left on `rows`
    /// ([`aterm_phase::left_wall`]: the cancel the last thing said, right
    /// under the newest notice, which names its reset and has no turn served
    /// under it) answered
    ///
    /// * an ACT OF THIS POLICY'S OWN standing untaken ([`Self::untaken_act`])
    ///   that NO WALL HAS ANSWERED yet ([`Awaited::met`]: from then on the
    ///   notice is the track's to decide, [`still_shown`]) — asked when it
    ///   was typed; the notice is new where it is not the one the act was
    ///   typed over ([`Awaited::left`]);
    /// * else ANYONE'S TURN — a person's prompt, the host's carry-on, the
    ///   turn an act of ours led to — asked since the policy LAST LOOKED
    ///   ([`Self::observe`]), while it waits on no limit of its own (whose
    ///   notice is the track's); the notice is new where that look drew no
    ///   such notice. The FIRST look says nothing: a cancelled limit found
    ///   standing is no answer of anyone's (the left-wall rule of 7057c6acf:
    ///   a notice says something of the limit it names, and of no other).
    ///
    /// Handed back: that wall, which the loop reads in the screen's place
    /// (`Session::turn_end_reading`) so that it is confirmed, placed,
    /// remembered and folded in ([`Self::observe`]) as a wall read there is;
    /// WHEN its turn was asked, at the earliest — its notice, to be its
    /// answer, was written no earlier, and a conversation `/resume`d since
    /// ends on its own older cancelled limit of the same class, which only
    /// the session's transcript dates; and whether the SCREEN tells it from
    /// what the policy already saw ([`LeftAnswer::screen_tells`]). `None`
    /// everywhere else.
    #[must_use]
    pub fn left_answer(&self, rows: &[String]) -> Option<LeftAnswer> {
        let wall = aterm_phase::left_wall(rows)?;
        let key = left_key(
            aterm_phase::since_limit(rows).as_ref(),
            aterm_phase::auto_continue_end(rows),
        )?;
        let own = self
            .awaiting
            .as_ref()
            .filter(|a| a.rule != RULE_HOST_TURN && !a.command && !a.taken);
        let (since, seen) = match own {
            Some(a) if a.met => return None,
            Some(a) => (a.at, a.left),
            None if self.waits_on_named_reset() => return None,
            None => self.looked?,
        };
        Some(LeftAnswer {
            wall,
            since,
            screen_tells: seen != Some(key),
        })
    }

    /// A read saw the agent's LIVE TURN (its spinner, its busy footer — never
    /// a break of its own background work): the act awaited, if any, was
    /// taken ([`Self::act_untaken`]).
    ///
    /// The worker works: a limit reset's continuation, if one was owed
    /// ([`Self::reset_told`]), is paid — the vendor's own queued one started,
    /// or someone asked a turn.
    pub fn took(&mut self) {
        if let Some(a) = &mut self.awaiting {
            a.taken = true;
            // A wall read before it was the screen the act was typed at, not
            // the act's answer.
            a.met = false;
        }
        self.reset_resume = None;
    }

    /// THE MAC SLEPT `by`, as the loop measured it (the wall clock ran ahead
    /// of the monotonic one by that much: `Instant` stops while the Mac
    /// sleeps, the wall clock does not). Every stamp kept across the sleep
    /// reads `by` too recent, so a pause counted from one would be stretched
    /// by the night: the stamps this policy's pauses and windows count from,
    /// and the resets it keeps as `Instant`s, move `by` earlier, and a pause
    /// a sleep went through is over at the wake, as on the wall clock:
    ///
    /// * the wall being worked: its appearance (every ladder's rung, the down
    ///   hold, the exceptions' hours) and the reset its notice named
    ///   ([`still_shown`]);
    /// * the point's first showing (the short-turn back-off, a person's Esc
    ///   grace);
    /// * the owner's hour: the typing budget (`continue_per_hour`), and a
    ///   move refused ahead of a wall, asked again an hour on;
    /// * the reset the model switch and the Codex switch go back at, and the
    ///   Codex hold's bound for a reset nobody read
    ///   ([`TurnEndTiming::unknown_hold`], [`Self::hold_slept`]).
    ///
    /// NOT the worker's own clocks, which the sleep stopped too: the time an
    /// act has to be taken ([`TurnEndTiming::take_within`]) and the Codex
    /// switch's own settles (its picker, footer, Esc, `/model`). NOR a stamp
    /// a person's keystroke is set against — the switch's floors and its
    /// hold's start, the reset continuation's `told`, the harness's own Esc:
    /// a keystroke's age is read on an awake clock, and moving one side of
    /// the comparison would make an old keystroke new (so the hold's bound
    /// counts the sleep apart from its start). NOR the last lines against a
    /// hot loop — the wall breaker ([`WALL_ACTS_PER_WINDOW`]) and the account
    /// moves ([`ACCOUNT_MOVES_PER_WINDOW`]): no healthy hour reaches them
    /// (every ladder paces itself under them), so they count the hours the
    /// harness ran, and a wall clock STEPPED forward — which reads as a sleep
    /// here — never reopens them. A stamp the clock cannot hold that far back
    /// is left as it is.
    pub fn slept(&mut self, by: Duration) {
        let back = |t: &mut Instant| {
            if let Some(earlier) = t.checked_sub(by) {
                *t = earlier;
            }
        };
        for t in &mut self.acts {
            back(t);
        }
        if let Some((_, t)) = &mut self.account_ahead_unmade {
            back(t);
        }
        if let Some(t) = &mut self.point_at {
            back(t);
        }
        if let Some(w) = &mut self.wall {
            back(&mut w.since);
            if let Some(n) = &mut w.named {
                back(&mut n.at);
            }
        }
        if let Some(t) = self.model.as_mut().and_then(|m| m.back_at.as_mut()) {
            back(t);
        }
        if let Some(t) = self.wind.as_mut().and_then(|w| w.back_at.as_mut()) {
            back(t);
        }
        if self
            .wind
            .as_ref()
            .is_some_and(|w| matches!(w.phase, WindPhase::Holding { .. }))
        {
            self.hold_slept = self.hold_slept.saturating_add(by);
        }
    }

    /// THE LOOP'S LIMIT-RESET STEP ENDED, and its host read `outcome`
    /// (`crate::supervise::IdleHost::reset_told`): what the session is owed
    /// now is ONE continuation, typed by the limit-resume rule under
    /// `resume_limits` at the next free point ([`act_at_point`],
    /// [`wall_action`]):
    ///
    /// * [`ResetOutcome::Spent`] and [`ResetOutcome::Lifted`] whose line ASKS
    ///   for a message, [`ResetOutcome::Kept`], [`ResetOutcome::Failed`] and
    ///   [`ResetOutcome::Unread`]: owed from `now`. After a reset kept, failed
    ///   or unread the wall may still stand: the continuation is the probe
    ///   that costs no relaunch, and a wall that answers it is rotated from
    ///   or waited out by its own rule;
    /// * `Spent` and `Lifted` whose line asks for nothing (the vendor queued
    ///   its own continuation): owed from [`RESET_CONTINUES_WAIT`] past
    ///   `now`, so the vendor's is never doubled;
    /// * [`ResetOutcome::Pending`] is no end: nothing is owed.
    ///
    /// `Spent` and `Lifted` CLEARED THE ACCOUNT'S WALLS (`cleared_at`), which
    /// the host then advises as it does the owner's own clearing
    /// ([`AccountAdvice::Resume`]): that clearing is answered by this
    /// continuation, so the advice types nothing more for it
    /// ([`account_first`]).
    ///
    /// While it is owed the host is not asked about the account
    /// ([`Self::account_point`]), and so no probe is made
    /// ([`Self::probe_gate`]). Any act typed ([`Self::acted`]), a turn the
    /// host typed ([`Self::host_typed`]) and work seen since `now`
    /// ([`Self::took`], [`Self::observe`]) pay it.
    pub fn reset_told(&mut self, outcome: &ResetOutcome, now: Instant) {
        let (asks, cleared_at) = match *outcome {
            ResetOutcome::Pending => return,
            ResetOutcome::Spent {
                asks, cleared_at, ..
            }
            | ResetOutcome::Lifted { asks, cleared_at } => (asks, Some(cleared_at)),
            ResetOutcome::Kept | ResetOutcome::Failed { .. } | ResetOutcome::Unread => (true, None),
        };
        self.reset_resume = Some(ResetResume {
            told: now,
            due: if asks {
                now
            } else {
                later(now, RESET_CONTINUES_WAIT)
            },
        });
        if let Some(cleared_at) = cleared_at {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(cleared_at, |typed| typed.max(cleared_at)),
            );
        }
    }

    /// From when the continuation a limit reset left is typed, while one is
    /// owed ([`Self::reset_told`]).
    #[must_use]
    pub fn reset_resume_owed(&self) -> Option<Instant> {
        self.reset_resume.map(|owed| owed.due)
    }

    /// The model switch in force, if any.
    #[must_use]
    pub fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
    }

    /// The acts made at the wall being worked, if it is of
    /// `kind`'s class ([`exception_for`]'s and the conformance walks'
    /// projection).
    #[must_use]
    pub fn wall_attempts(&self, kind: WallKind) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.attempts)
    }

    /// Since when the wall being worked shows (this
    /// appearance; a retry moves it), if it is of `kind`'s class.
    #[must_use]
    pub fn wall_since(&self, kind: WallKind) -> Option<Instant> {
        self.wall
            .as_ref()
            .filter(|t| t.class == class_of(kind))
            .map(|t| t.since)
    }

    /// Whether the full context being worked has met the vendor's own word
    /// that a `/compact` could not lower it: the wall's attempts say how often
    /// the loop tried, this says Claude Code answered that it cannot be done.
    #[must_use]
    pub fn compaction_exhausted(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|t| t.class == WallClass::Context && t.exhausted)
    }

    /// Whether the fallback relaunch for the bucket being worked was
    /// refused for good ([`Self::restarted`] with `made` false): its reset,
    /// or the entitlement ladder, is what is waited on.
    #[must_use]
    pub fn fallback_refused(&self) -> bool {
        self.fallback_unmade
    }

    /// The account move made and not answered yet, if any.
    #[must_use]
    pub fn account_move(&self) -> Option<&AccountMove> {
        self.account.as_ref()
    }

    /// Whether an account move asked for at the wall being worked could not
    /// be made ([`Self::restarted`] with `made` false): the wall is waited
    /// out as it would be with no roster.
    #[must_use]
    pub fn account_refused(&self) -> bool {
        self.account_unmade
    }

    /// THE ACCOUNTS' NEWS REACHED THIS SESSION (the host's stamp moved:
    /// `crate::supervise::IdleHost::account_stamp` — the owner signed an
    /// account in and checked it, said `accounts use`, another session proved
    /// or struck one): what the host refused may be possible now, so both
    /// latches end — the wall's ([`Self::account_refused`]) and the one ahead
    /// of a wall — and the next point asks again. A refusal repeated with no
    /// news is still not asked again.
    pub fn account_news(&mut self) {
        self.account_unmade = false;
        self.account_unmade_seed = false;
        self.account_ahead_unmade = None;
    }

    /// Whether the wall's resume is owed after a move this session's host
    /// made at a wall ([`Self::account_resume`]): until then a usage limit
    /// on the screen is the notice of the account it LEFT.
    #[must_use]
    pub fn account_resume_owed(&self) -> bool {
        self.account_resume
    }

    /// Whether a move AHEAD of a wall to the account labelled `to` was asked
    /// and could not be made within the window ending at `now`
    /// ([`Self::restarted`] with `made` false): not asked again for that
    /// account until the window has passed — one refused relaunch an hour at
    /// the most, where a refusal that stands for the launch's life would
    /// otherwise be asked at every turn end.
    fn account_ahead_refused(&self, to: &str, now: Instant) -> bool {
        self.account_ahead_unmade
            .as_ref()
            .is_some_and(|(label, at)| {
                label == to && now.saturating_duration_since(*at) < self.timing.window
            })
    }

    /// How many account moves this session made within the window ending at
    /// `now`, and when the oldest of them leaves it
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]).
    #[must_use]
    pub fn account_moves_of(&self, now: Instant) -> (usize, Option<Instant>) {
        let window = self.timing.window;
        let mut n = 0;
        let mut oldest = None;
        for &t in &self.account_moves {
            if now.saturating_duration_since(t) < window {
                n += 1;
                oldest = oldest.or(Some(later(t, window)));
            }
        }
        (n, oldest)
    }

    /// WHAT THIS POINT SAYS OF THE ACCOUNT MOVE NOT ANSWERED YET, read
    /// BEFORE the point is folded in ([`Self::observe`]: the fold is what
    /// marks a turn of someone else's as asked) and before the host is asked
    /// for its advice — so a host told of a bad answer has struck the account
    /// by the time it advises. Pure; `None` where the point says nothing, or
    /// the host was told already.
    ///
    /// * SERVED: a turn asked under the new login (`account_asked`) ran — a
    ///   read saw the agent busy — and ended at an idle point or a question,
    ///   no wall and no person's Esc. A reply that came and went inside one
    ///   read is not counted: the next turn proves the account.
    /// * A LOST LOGIN: at once, a turn asked or not — the notice is the new
    ///   process's own, and no turn can be asked under it.
    /// * A USAGE LIMIT (session, week, model bucket): only after a turn was
    ///   asked. A relaunch RESUMES the conversation, whose last row may be
    ///   the old account's limit; whether the new process repaints it was
    ///   never measured (the design's §9), so a limit on the screen before
    ///   any turn says nothing about the new account. THE SCREEN IS ALL THIS
    ///   READS: that the limit is the new process's own is the host's to
    ///   check (the transcript row's time against the process's start) before
    ///   it strikes anything. NOR WHILE THE TURN ASKED IS STILL AWAITED with
    ///   no busy read since: the continuation is typed AT the notice the old
    ///   account left (the wall's resume, the host's carry-on), and that
    ///   notice stands until the agent takes it — the point is the act's
    ///   only once [`Self::observe`] has judged it so.
    ///
    /// A spend or credit wall is no account's window: neither.
    #[must_use]
    pub fn account_answer(&self, reading: &TurnEndReading) -> Option<AccountAnswer> {
        self.account.as_ref()?;
        if self.account_told || !reading.at_point(self.stale_input) {
            return None;
        }
        match reading.wall {
            Some(WallKind::Auth) => Some(AccountAnswer::Bad(AccountBad::SignedOut)),
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if self.account_asked && !(self.awaiting.is_some() && reading.worked.is_none()) =>
            {
                Some(AccountAnswer::Bad(AccountBad::Wall))
            }
            None if self.account_asked
                && reading.worked.is_some()
                && !reading.interrupted
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountAnswer::Served)
            }
            _ => None,
        }
    }

    /// THE POINT THE HOST IS ASKED ABOUT ([`AccountPoint`]), or `None` where
    /// its advice could decide nothing — so a host that reads a roster and a
    /// state file to advise is asked only where [`decide_turn_end`] would act
    /// on the answer: a turn's end nothing blocks, in a session with a task,
    /// where a relaunch under another account may be asked at all
    /// ([`account_rotates`]) and the upgrade does not own the point; at a
    /// session or weekly limit or a model bucket; at a lost login only while
    /// the session's own move is unanswered; at a plain turn end only once
    /// it is answered. Pure.
    ///
    /// READ AFTER THE POINT IS FOLDED IN ([`Self::observe`]; the loop's
    /// order, and the Tier-1 walk's): what this memory says of the task, of
    /// the latch and of the move not answered is then THIS point's. The fold
    /// is what opens a finished task again (someone's turn, with work, is the
    /// task theirs) and what ends a latch with its wall; read before it, a
    /// finished session a person gave new work to would not be asked about at
    /// the limit that work ends on, and a session whose refused move's wall
    /// has ended would be passed over for one more point — a whole second
    /// wall.
    ///
    /// Nor at a usage limit while the wall's resume is owed
    /// ([`Self::account_resume`]): that notice is the account's the session
    /// left, and the point's answer is the continuation, whatever is advised.
    ///
    /// Nor anywhere while a limit reset's continuation is owed
    /// ([`Self::reset_told`]): the point's answer is that continuation, and
    /// an account whose reset just ended is not moved from before it has
    /// answered.
    ///
    /// A usage limit whose notice LEFT the screen with no work since (Esc on
    /// the vendor's countdown) is that limit's point still, decided as if its
    /// notice showed ([`still_shown`]): asked about as the wall, never as a
    /// turn's end.
    #[must_use]
    pub fn account_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
    ) -> Option<AccountPoint> {
        if !reading.at_point(self.stale_input)
            || reading.taskless
            || reading.upgrading
            || self.task_done()
            || self.reset_resume.is_some()
            || !account_rotates(self, reading, cfg)
        {
            return None;
        }
        let wall = reading.wall.or_else(|| {
            self.wall
                .as_ref()
                .and_then(|t| left_named(t, reading))
                .map(|n| n.kind)
        });
        match wall {
            Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
                if self.account_resume =>
            {
                None
            }
            Some(
                kind @ (WallKind::UsageSession
                | WallKind::UsageWeekly
                | WallKind::ModelBucket { .. }),
            ) => Some(AccountPoint::Wall { kind }),
            Some(kind @ WallKind::Auth) if self.account.is_some() => {
                Some(AccountPoint::Wall { kind })
            }
            None if self.account.is_none()
                && matches!(reading.phase, Phase::Idle | Phase::Question) =>
            {
                Some(AccountPoint::TurnEnd)
            }
            _ => None,
        }
    }

    /// WHETHER THIS POINT MAY TAKE THE ACCOUNTS PROBE ([`RULE_USAGE_PROBE`];
    /// `docs/SPEC-claude-accounts-probe-2026-10-01.md`, P6, P8): the ONE
    /// pure gate, so each condition can be flipped alone. `true` only when
    /// ALL hold:
    ///
    /// * the reading's advice is [`AccountAdvice::Probe`] — the roster says
    ///   the read is due — at a plain turn end the host is asked about
    ///   ([`Self::account_point`] is [`AccountPoint::TurnEnd`]: a task, no
    ///   upgrade, a relaunch under another account askable at all, the
    ///   session's own last move answered, no limit reset's continuation
    ///   owed);
    /// * the phase is `Idle`, read by a reader whose word is evidence, with
    ///   no survey card up. NOT a question, although the host is asked
    ///   there: nothing is typed over what the worker asked a person;
    /// * the composer is drawn and holds nothing typed (empty, or the
    ///   worker's own dim suggestion), and no submitted row waits unanswered
    ///   — not even one this policy judged not taken;
    /// * no wall is on the screen and none is being worked (no wall track: a
    ///   limit waited out off the screen, a lost login);
    /// * nobody stopped the turn with Esc, and the worker asked nothing of a
    ///   person ([`TurnEndReading::asked`]);
    /// * nothing of this policy's is awaited or owed: no act awaited, no
    ///   command's continuation, no save-then-wait switch open, no wall's
    ///   resume owed after a move;
    /// * no person typed within `cfg.human_grace_s`.
    ///
    /// It is the loop's gate and decides nothing here: [`decide_turn_end`]
    /// reads the advice past the probe whatever this says, and a probe the
    /// loop makes moves none of this memory ([`Self::acted`] is not called
    /// for it). What only the loop knows is the loop's to add — that the
    /// turn was read live, the renderer, the fences, the host's claim.
    /// PURE: `now` is taken so a rule about this memory's own clocks can be
    /// stated here without a new signature, and none is today.
    #[must_use]
    pub fn probe_point(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> bool {
        self.probe_gate(reading, cfg, now).is_none()
    }

    /// [`Self::probe_point`], BY THE GATE THAT SHUT: `None` where the point
    /// may take the probe, else the FIRST of its conditions that does not
    /// hold ([`ProbeGate`]), for the loop's one line about a due probe it
    /// did not run. The conditions are a conjunction, so the order decides
    /// only which word is said, and it is the order a reader wants: the
    /// narrow condition before the wide one that would hide it — the phase,
    /// a wall and the composer before "no turn end the host is asked about",
    /// which is false at each of them too. PURE, as [`Self::probe_point`]
    /// is.
    #[must_use]
    pub fn probe_gate(
        &self,
        reading: &TurnEndReading,
        cfg: &SupervisorConfig,
        now: Instant,
    ) -> Option<ProbeGate> {
        let _ = now;
        let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
        let owed = self.awaiting.is_some()
            || self.after.is_some()
            || self.wind.is_some()
            || self.account_resume;
        let checks = [
            (
                !matches!(reading.account, Some(AccountAdvice::Probe { .. })),
                ProbeGate::NotDue,
            ),
            (reading.phase != Phase::Idle, ProbeGate::NotIdle),
            (!reading.authoritative, ProbeGate::NotAuthoritative),
            (reading.survey, ProbeGate::Survey),
            (
                reading.wall.is_some() || self.wall.is_some(),
                ProbeGate::Wall,
            ),
            (
                !matches!(reading.composer, Composer::Empty | Composer::Placeholder(_)),
                ProbeGate::Composer,
            ),
            (reading.pending_input, ProbeGate::PendingInput),
            (reading.interrupted, ProbeGate::Interrupted),
            (reading.asked().is_some(), ProbeGate::Question),
            (
                self.account_point(reading, cfg) != Some(AccountPoint::TurnEnd),
                ProbeGate::NotTurnEnd,
            ),
            (owed, ProbeGate::ActOwed),
            (
                reading.person.is_some_and(|ago| ago < grace),
                ProbeGate::Person,
            ),
        ];
        checks
            .into_iter()
            .find_map(|(shut, gate)| shut.then_some(gate))
    }

    /// The loop told its host what answered the move ([`Self::account_answer`]):
    /// served, the move is over; answered by a wall or a lost login, it
    /// stands — told, so never told again — until the worker works past it
    /// ([`Self::observe`]) or another move is made.
    pub fn account_told(&mut self, answer: AccountAnswer) {
        match answer {
            AccountAnswer::Served => self.account_over(),
            AccountAnswer::Bad(_) => self.account_told = true,
        }
    }

    /// The move made is behind the session.
    fn account_over(&mut self) {
        self.account = None;
        self.account_asked = false;
        self.account_told = false;
        self.account_startup = false;
        self.account_resume = false;
    }

    /// The wall's words a decision says: the screen's, else — the notice
    /// gone from it — the track's, as its notice last read
    /// ([`WallTrack::said`]).
    fn wall_words<'a>(&'a self, reading: &'a TurnEndReading) -> &'a str {
        match &self.wall {
            Some(t) if reading.wall.is_none() => &t.said,
            _ => &reading.wall_message,
        }
    }

    /// The lost login's track, if one stands — the wall seen and the worker
    /// not working since — and how many `/login`s it has had (the
    /// conformance walk's projection).
    #[must_use]
    pub fn login_track(&self) -> Option<usize> {
        self.wall
            .as_ref()
            .filter(|t| t.class == WallClass::Auth)
            .map(|t| t.attempts)
    }

    /// Folds what a point shows into the state, before [`decide_turn_end`]:
    ///
    /// * the last act's yield: under `min_work` lengthens the short streak,
    ///   more ends it; a host-typed turn ([`Self::host_typed`]) never
    ///   lengthens it and ends it with real work;
    /// * someone else's turn in between (`worked` with nothing awaited);
    /// * the first point seen (work unknown: short);
    /// * the wall on screen (a new kind opens a track; the same kind after a
    ///   retry is its next appearance).
    ///
    /// Reading the same point again changes nothing. After a text act with no
    /// busy read since, the point is not the act's yet (Claude Code draws a
    /// submitted message before its spinner) until
    /// [`TurnEndTiming::take_within`] from the first such point; past that it
    /// is judged a short yield with no work, and an unanswered `❯` row is
    /// judged not taken and becomes a point the next act may go at. A slash
    /// command's point needs no busy read. Anyone else's unanswered row is
    /// never a point.
    ///
    /// A WALL IS TRACKED WHEREVER IT IS READ, a point or not
    /// ([`Self::track_wall`]): a reading that is no point yet — the act's take
    /// window still open, anyone's unanswered row — leaves the act's own
    /// bookkeeping (taken or not, its yield, the streak) to the point that
    /// judges it, but the wall it shows is folded in at once. Folded only at
    /// that point, a limit that answered the act and whose notice left the
    /// screen inside the window (Esc on the vendor's countdown) was never
    /// seen: the window's end read the act's short turn, and the continue
    /// policy typed into the standing limit at its back-off (round 15's run,
    /// 2026-10-14). The reading's wall ANSWERED the act awaited
    /// ([`Awaited::met`]), and the point that judges it keeps that: a retry
    /// of ours the wall answered is the wall's next appearance, never taken
    /// because the notice left before the window's end (round 16's review).
    /// Nor is a wall read there one a point DECIDED ([`WallTrack::pointed`]):
    /// a lost login no point saw is decided as the wall it left, never held
    /// as one the owner was told of. A BUSY or BOX reading is no point and folds nothing: the
    /// reader hands a wall under a live turn only for the memory banner,
    /// which [`decide_turn_end`] acts on by the reading itself, and the turn
    /// running ends whatever track stands by its work at its own point.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "TurnEnds",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        // The policy LOOKED at this screen: a limit a cancel left that a
        // later screen draws, and this one did not, answered a turn asked
        // since ([`Self::left_answer`]).
        self.looked = Some((
            now,
            left_key(reading.since_limit.as_ref(), reading.auto_continue),
        ));
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        // A TURN NO READ SAW BUSY, ended where a usage limit this policy
        // waits on has left the screen: the vendor's own continuation
        // (`Usage limit reset · continuing automatically`,
        // [`aterm_phase::AutoContinue::Fired`]), or a turn SERVED since THIS
        // limit's notice — a person's, the limit lifted before its reset (the
        // ruling of 2026-10-14, a person at the tab who works drops the
        // wait): its prompt and the model's reply drawn under the notice that
        // names this limit's reset ([`NamedReset::served_under`]), or, with
        // no limit's notice on the screen to order it by, the model's reply
        // written after this limit's own notice row in the session's
        // transcript ([`TurnEndReading::served_in_transcript`]) — its reply
        // come and gone between two reads. Folded in as a turn of someone
        // else's that ended here, its work unmeasured: the limit is over, as
        // any work ends it, and the point is the continue policy's, its
        // back-off counted from here. News once: the track it ends is what
        // makes it so. Not while a prompt is unanswered (its turn may meet
        // the limit again): the unanswered input's arm below returns before
        // the wall is looked at whenever work is read, this fold's included.
        let fired;
        let served = reading.served_in_transcript
            || self
                .wall
                .as_ref()
                .and_then(|t| t.named)
                .is_some_and(|n| n.served_under(reading.since_limit.as_ref()));
        let reading = if reading.wall.is_none()
            && reading.worked.is_none()
            && (reading.auto_continue == Some(aterm_phase::AutoContinue::Fired) || served)
            && self
                .wall
                .as_ref()
                .is_some_and(|t| t.class == WallClass::Usage)
        {
            fired = TurnEndReading {
                worked: Some(Duration::ZERO),
                ..reading.clone()
            };
            &fired
        } else {
            reading
        };
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        let awaited_command = self.awaiting.is_some_and(|a| a.command);
        // Real work before the wall showed: the worker's, after an act of
        // ours or a message of anyone's. Not when the act was a COMMAND
        // (`/compact`): the spinner of its own run is no work of the
        // worker's (a compaction that ran six minutes and ended on the same
        // wall started the ladder over, 40 `/compact` in four hours).
        let worked_to_wall =
            !awaited_command && reading.worked.is_some_and(|w| w >= self.timing.progress);
        if !self.point_judged(reading, now) {
            // No point yet, and no act judged: the wall it shows is tracked
            // all the same, taking up a latch a loop before this one left as
            // the first wall this state sees does. An act awaited is ANSWERED
            // by it ([`Awaited::met`]): the first wall read since a retry is
            // the retry's next appearance, as at its point.
            if let Some(kind) = reading.wall {
                let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
                let retried = self.awaiting.as_mut().is_some_and(|a| {
                    let first = !std::mem::replace(&mut a.met, true);
                    first && is_wall_retry(a.rule)
                });
                let at_point = false;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    now,
                );
            }
            return;
        }
        let min = self.timing.min_work;
        let checked = std::mem::take(&mut self.checking);
        let met = self.awaiting.is_some_and(|a| a.met);
        let awaited = self.awaiting.take().map(|a| a.rule);
        let done = says_done(reading.said_tail.as_deref());
        let finished = says_finished(reading.said_tail.as_deref())
            && reading.wall.is_none()
            && !reading.pending_input;
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        match (awaited, reading.worked) {
            // The harness's own turn: a short answer leaves the streak, its
            // back-off and the done reports as they were; real work ends the
            // streak (a carry-on's answer is the worker's work resumed).
            (Some(RULE_HOST_TURN), worked) => {
                let d = worked.unwrap_or_default();
                if d >= min && !done {
                    self.followed = Some(RULE_HOST_TURN);
                    self.point_worked = d;
                    self.point_at = Some(now);
                    self.short = 0;
                    self.done = 0;
                } else if self.point_at.is_none() {
                    self.point_at = Some(now);
                }
            }
            (Some(rule), worked) => {
                let d = worked.unwrap_or_default();
                self.followed = Some(rule);
                self.point_worked = d;
                self.point_at = Some(now);
                if is_continue_rule(rule) {
                    self.short = streak(self.short, d, done);
                }
                // A done report after the check ends the task; after any
                // other act it is the first and gets the check. An act never
                // taken said nothing.
                if reading.wall.is_none() && !reading.pending_input {
                    self.done = match (done, checked) {
                        (false, _) => 0,
                        (true, true) => 2,
                        (true, false) => 1,
                    };
                }
            }
            // Someone else's turn makes the task theirs again: its done
            // report is a first one; one ending on a wall is left to the
            // wall's ladder; the streak stands.
            (None, Some(d)) => {
                self.followed = None;
                self.point_worked = d;
                self.point_at = Some(now);
                self.done = u8::from(done && reading.wall.is_none());
                if reading.wall.is_none() {
                    self.short = streak(self.short, d, false);
                }
            }
            // The first point seen, its work unknown: a supervisor that
            // attaches to an idle worker backs off before it types. A
            // session with no task yet ended nothing: no streak begins.
            (None, None) if self.point_at.is_none() && !reading.taskless => {
                self.point_at = Some(now);
                self.short = 1;
                self.done = u8::from(done && reading.wall.is_none());
            }
            (None, None) => {}
        }
        // The worker's own FINISHED ends the task at once, whoever's turn it
        // answered: no check round trip.
        if finished {
            self.done = 2;
            self.finished = true;
        } else if self.done < 2 {
            self.finished = false;
        }
        let retried = awaited.is_some_and(is_wall_retry);
        // A retry was TAKEN where no wall answered it: none on the screen at
        // its point, and none read inside its window ([`Awaited::met`]).
        let retry_taken = retried && !met;
        // A latch a previous loop left ([`Self::seed_account_unmade`]) is
        // the first wall's, and no later one's.
        let unmade_seeded = std::mem::take(&mut self.account_unmade_seed);
        match reading.wall {
            Some(kind) => {
                let at_point = true;
                self.track_wall(
                    reading,
                    kind,
                    retried,
                    worked_to_wall,
                    unmade_seeded,
                    at_point,
                    now,
                );
            }
            // The wall has left the screen: gone for good once the worker
            // worked (the vendor's own continuation included, above) or our
            // retry was taken. With no work — Esc on the vendor's own
            // auto-continue, a person's `/usage`, `/login` or `/model` output
            // under it — the track stays, and the point is decided by the
            // reset it named ([`still_shown`]). So too after a retry the wall
            // answered inside its window, its notice gone by the window's end
            // (round 16's review: read as taken, the track was dropped and the
            // continue policy typed into the standing limit). Where the vendor
            // says its auto-continue will not go on (`Automatic continue
            // cancelled`), nobody goes on by itself now.
            None => {
                if reading.worked.is_some() || retry_taken || unmade_seeded {
                    self.account_unmade = false;
                }
                if reading.worked.is_some() || retry_taken {
                    self.wall = None;
                    self.fallback_unmade = false;
                } else if reading.auto_continue == Some(aterm_phase::AutoContinue::Unkept)
                    && let Some(n) = self.wall.as_mut().and_then(|t| t.named.as_mut())
                {
                    n.by_itself = false;
                }
            }
        }
        // THE ACCOUNT MOVE NOT ANSWERED YET ([`Self::account_answer`], which
        // the loop asks BEFORE this fold): a turn that ran and ended with no
        // wall. After a bad answer the host was told of, the worker works
        // again — the limit lifted, the login is back — and the move is
        // behind the session. Before any answer, the turn was someone's (no
        // act of the harness's asked it): a turn HAS run under the new login
        // now, so the next one that is served proves it — and it was the
        // continuation the wall's resume owed. NOT THE FIRST POINT AFTER THE
        // RELAUNCH: work shown there is the resumed process's own start-up
        // (drawing its conversation, read busy), a turn nobody asked — read
        // as one, it armed the tell, and the next busy read told the host
        // `served` for an account that had answered nothing.
        let startup = std::mem::take(&mut self.account_startup);
        // A resume a loop before this one left owed ([`Self::seed_account`])
        // is owed AT THE NOTICE THE SESSION LEFT, and nowhere else: a host's
        // turn leaves no row, so at a point that shows no usage limit the
        // ledger cannot say whether the carry-on went since — and that point
        // is the host's carry-on's, or the continue policy's, as it was.
        if std::mem::take(&mut self.account_resume_seed)
            && !matches!(
                reading.wall,
                Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. })
            )
        {
            self.account_resume = false;
        }
        // ANY turn that ran since the relaunch pays the wall's resume,
        // whatever it ended on: a limit a turn of someone's worked up to is
        // the NEW account's wall, the host's to be asked about — kept owed,
        // the continuation was typed into it at once.
        if self.account.is_some() && reading.worked.is_some() && !reading.pending_input && !startup
        {
            self.account_resume = false;
        }
        if self.account.is_some()
            && reading.wall.is_none()
            && reading.worked.is_some()
            && !reading.pending_input
        {
            if self.account_told {
                self.account_over();
            } else if !startup {
                self.account_asked = true;
            }
        }
        // A LIMIT RESET'S CONTINUATION ([`Self::reset_told`]) is paid by a
        // turn that ran SINCE its outcome was told, whatever it ended on. Not
        // by the work of the first point after the dialog where no busy read
        // came since: that span began before the dialog, and is the turn
        // that met the wall — read as work since, the continuation a kept or
        // failed reset owes was dropped at the very point it is typed at.
        if let Some(owed) = self.reset_resume
            && reading
                .worked
                .and_then(|worked| now.checked_sub(worked))
                .is_some_and(|began| began >= owed.told)
        {
            self.reset_resume = None;
        }
        let ours_stopped = reading.interrupted && self.own_interrupt.is_some();
        self.observe_wind(reading, awaited, ours_stopped, now);
        // The harness's own Esc is the interrupt of the turn it stopped; a
        // turn that ended otherwise since is anyone's.
        if (reading.worked.is_some() && !reading.interrupted) || self.wind.is_none() {
            self.own_interrupt = None;
        }
    }

    /// Whether the reading is a POINT the awaited act is judged at
    /// ([`Self::observe`]): not anyone's unanswered `❯` row but the act's own
    /// past its window (then judged not taken, [`Self::stale_input`]), and
    /// not a point after a text act no busy read has followed, until
    /// [`TurnEndTiming::take_within`] from the first such point. Stamps that
    /// first point; judges nothing.
    fn point_judged(&mut self, reading: &TurnEndReading, now: Instant) -> bool {
        if reading.pending_input {
            let Some(a) = self.awaiting.as_mut().filter(|_| reading.worked.is_none()) else {
                return false;
            };
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
            self.stale_input = true;
        } else if let Some(a) = &mut self.awaiting
            && !a.command
            && reading.worked.is_none()
        {
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return false;
            }
        }
        true
    }

    /// THE WALL A READING SHOWS, folded into its track ([`Self::observe`],
    /// at a point or not — `at_point`, [`WallTrack::pointed`]): another class
    /// opens a track — its appearance `now`, the fallback's and the account
    /// move's latches its own (the latter `unmade_seeded`, a latch a loop
    /// before this one left for the first wall this state sees) — and the
    /// same class after a retry of ours (`retried`) is its next appearance, a
    /// new episode with its ladder from the start where real work came first
    /// (`worked`). The reset the notice names, and its words, are kept for
    /// when it leaves ([`still_shown`]).
    #[allow(clippy::too_many_arguments)]
    fn track_wall(
        &mut self,
        reading: &TurnEndReading,
        kind: WallKind,
        retried: bool,
        worked: bool,
        unmade_seeded: bool,
        at_point: bool,
        now: Instant,
    ) {
        let class = class_of(kind);
        match &mut self.wall {
            Some(t) if t.class == class => {
                t.pointed |= at_point;
                if retried {
                    t.since = now;
                    // The act led to that work: a new episode, its ladder
                    // from the start.
                    if worked {
                        t.attempts = 0;
                        t.ups = 0;
                        t.exhausted = false;
                    }
                }
                // A LATCH IS ITS WALL'S: the move refused at the wall before
                // this work is asked again at this one. Kept, a session whose
                // continuation worked half an hour and met its limit again
                // waited that whole second wall out beside an account it
                // could run on.
                if worked {
                    self.account_unmade = false;
                }
            }
            _ => {
                self.wall = Some(WallTrack {
                    class,
                    since: now,
                    attempts: 0,
                    ups: 0,
                    exhausted: false,
                    named: None,
                    pointed: at_point,
                    said: String::new(),
                });
                self.fallback_unmade = false;
                self.account_unmade = unmade_seeded;
            }
        }
        if let Some(t) = &mut self.wall {
            if class == WallClass::Context && compaction_exhausted_row(&reading.wall_message) {
                t.exhausted = true;
            }
            t.said.clone_from(&reading.wall_message);
            // The reset the notice names, kept for when it leaves.
            if let Some(at) = reading.reset_at {
                t.named = Some(NamedReset {
                    kind,
                    at,
                    jitter: reading.jitter,
                    by_itself: reading.resumes_by_itself,
                    said: reading.reset_clock,
                });
            }
        }
    }

    /// Records an act the loop typed (not a skipped or refused one): the
    /// budget, the awaited act, what it leaves owed, and a wall track's
    /// attempts. Restarts go through [`Self::restarted`].
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        // The harness's own Esc excused the turn it stopped, no later one:
        // its next act is typed now.
        self.own_interrupt = None;
        // A turn asked under the login an account move chose
        // ([`Self::account_answer`]) — and the wall's resume, if one was
        // owed, is paid by it.
        self.account_asked |= self.account.is_some();
        self.account_resume = false;
        // …and so is the continuation a limit reset left owed
        // ([`Self::reset_told`]): any act typed is a turn asked.
        self.reset_resume = None;
        // The wall's continuation typed on the owner's clearing
        // ([`AccountAdvice::Resume`]): one a clearing.
        if rule == RULE_LIMIT_RESUME
            && let Some(AccountAdvice::Resume { cleared_at }) = reading.advised()
        {
            self.account_resumed = Some(
                self.account_resumed
                    .map_or(*cleared_at, |typed| typed.max(*cleared_at)),
            );
        }
        self.acts.push_back(now);
        while self.acts.len() > 64 {
            self.acts.pop_front();
        }
        self.awaiting = Some(Awaited {
            rule,
            command: matches!(action, TurnEndAction::TypeCommand { .. }),
            at: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: left_key(reading.since_limit.as_ref(), reading.auto_continue),
        });
        self.checking = matches!(
            action,
            TurnEndAction::Type {
                rule_id: RULE_DONE_CHECK,
                ..
            }
        );
        self.after = match action {
            TurnEndAction::TypeCommand {
                then: Then::Continue,
                ..
            } => Some(rule),
            _ => None,
        };
        if is_wall_act(rule) {
            let window = self.timing.window;
            self.wall_acts
                .retain(|&(t, _)| now.saturating_duration_since(t) < window);
            self.wall_acts.push_back((now, rule));
            while self.wall_acts.len() > 4 * WALL_ACTS_PER_WINDOW {
                self.wall_acts.pop_front();
            }
        }
        if is_wall_act(rule)
            && let Some(t) = &mut self.wall
        {
            t.attempts += 1;
            if rule == RULE_API_BACK {
                t.ups += 1;
            }
        }
        self.acted_wind(action, rule, now);
    }

    /// A typed act of the save-then-wait switch's ([`Self::acted`]): the
    /// switch moves with it, and the act's ledger row carries it.
    fn acted_wind(&mut self, action: &TurnEndAction, rule: &'static str, now: Instant) {
        let Some(w) = &mut self.wind else {
            return;
        };
        let command = matches!(action, TurnEndAction::TypeCommand { .. });
        match rule {
            RULE_WIND_DOWN if command => {
                w.pause_typed = true;
                w.goal_paused = true;
                w.stops += 1;
            }
            RULE_WIND_DOWN => w.phase = WindPhase::Winding,
            RULE_MODEL_RESTORE if command => {
                w.restore_tries += 1;
                w.restore_at = Some(now);
            }
            RULE_LIMIT_RESUME if matches!(w.phase, WindPhase::Holding { .. }) => {
                let closed = w.clone();
                self.wind = None;
                self.own_interrupt = None;
                self.wind_events.push(WindEvent::Typed {
                    wind: closed,
                    phase: "done",
                });
                return;
            }
            _ => return,
        }
        let phase = wind_phase_word(w);
        let wind = w.clone();
        self.wind_events.push(WindEvent::Typed { wind, phase });
    }

    /// Records a turn the host typed (upgrade notice, carry-on;
    /// [`RULE_HOST_TURN`]), awaited like this policy's acts. A short answer
    /// leaves the streak and back-off as the worker's turns left them; real
    /// work ends the streak (the carry-on's answer is the worker's work
    /// resumed). No budget is spent.
    pub fn host_typed(&mut self, now: Instant) {
        self.checking = false;
        // The carry-on of an account move is the first turn asked under the
        // new login ([`Self::account_answer`]), and the continuation the
        // wall's resume owed.
        self.account_asked |= self.account.is_some();
        self.account_resume = false;
        self.reset_resume = None;
        self.awaiting = Some(Awaited {
            rule: RULE_HOST_TURN,
            command: false,
            at: now,
            unseen_since: None,
            taken: false,
            met: false,
            left: None,
        });
    }
}

impl TurnEndState {
    /// Record a restart the loop asked its host for
    /// ([`TurnEndAction::Restart`]): MADE (`made`, the new process the
    /// loop's) — a bucket's fallback is the switch to undo at the reset
    /// ([`Self::model_switch`], back at the reset the notice named, else
    /// after the longest limit back-off), the reset's relaunch undoes it,
    /// and the bucket's track counts the act; or NOT made (the host refused
    /// it, or none relaunches here) — a bucket's reset is waited out
    /// instead, and a switch that cannot be undone is let go (it was
    /// session-only: nothing else moved). The memory banner's leaves nothing
    /// to remember.
    ///
    /// AN ACCOUNT MOVE made ([`Restart::Account`]) is remembered until it is
    /// answered ([`Self::account_answer`]) and counted
    /// ([`ACCOUNT_MOVES_PER_WINDOW`]); the wall it left is the OLD account's,
    /// so its track ends with it (a limit the new account answers with is a
    /// new wall, its ladder from the start). It also CLEARS AN OPEN MODEL
    /// SWITCH: that switch's return was owed at the old account's bucket's
    /// reset, which says nothing about the account the session runs on now —
    /// kept, it would relaunch the session again at an instant that no
    /// longer means anything. The relaunch itself comes back on the model
    /// the fallback replaced (`crate::harness::relaunch::model_restart`), so
    /// no return is owed at all. MADE AT A WALL it owes that wall's resume
    /// ([`Self::account_resume_owed`]): the continuation the wall would have
    /// had at its reset, at the relaunched session's first point. NOT made —
    /// refused, or failed — it is not asked again while the wall stands
    /// ([`Self::account_refused`]); one asked ahead of a wall is not asked
    /// again for that account within the window
    /// ([`Self::account_ahead_refused`]).
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        match why {
            Restart::Memory => {}
            Restart::Model { to } if made => {
                // Back at the reset the notice named, and only a reset still
                // AHEAD. A bucket that
                // names none — `requires usage credits` is an entitlement,
                // not a window — is not lifted by any clock: the session
                // stays on the fallback (session-only: the owner's default is
                // untouched), where a guessed half hour sent it back to the
                // bucket, into the same wall, and out again — two relaunches
                // an hour, 240 in the five days the account's Fable wall
                // stood. A reset already past that the wall still stands
                // under is no reset either: back at it would be back at
                // once, on every turn end.
                let back_at = reading.reset_at.filter(|&r| r > now);
                self.model = Some(ModelSwitch {
                    from: bucket_model(&reading.wall_message),
                    to: to.clone(),
                    back_at,
                });
                if let Some(t) = &mut self.wall {
                    t.attempts += 1;
                }
            }
            Restart::Model { .. } => self.fallback_unmade = true,
            Restart::ModelBack { .. } => self.model = None,
            Restart::Account { to, at } if made => {
                self.account = Some(AccountMove {
                    to: to.clone(),
                    at: now,
                });
                self.account_asked = false;
                self.account_told = false;
                self.account_startup = true;
                self.account_resume = *at == AccountAt::Wall;
                self.account_unmade = false;
                self.account_ahead_unmade = None;
                let window = self.timing.window;
                self.account_moves
                    .retain(|&t| now.saturating_duration_since(t) < window);
                self.account_moves.push_back(now);
                while self.account_moves.len() > 4 * ACCOUNT_MOVES_PER_WINDOW {
                    self.account_moves.pop_front();
                }
                self.wall = None;
                self.model = None;
                self.fallback_unmade = false;
            }
            Restart::Account {
                at: AccountAt::Wall,
                ..
            } => self.account_unmade = true,
            Restart::Account {
                to,
                at: AccountAt::Ahead,
            } => self.account_ahead_unmade = Some((to.clone(), now)),
        }
    }
}

impl TurnEndState {
    /// The nudge's switch landed (press made, box seen leaving): `wind`
    /// ([`WindDown::opened`]) opens owing its wind-down. An open switch is
    /// kept, except one only intended ([`WindDown::intent`]), which gives way.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "PressSwitch",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    pub fn open_switch(&mut self, wind: WindDown) {
        if self.wind.as_ref().is_some_and(|w| !w.intent) {
            return;
        }
        self.after = None;
        self.wind = Some(wind);
    }

    /// A switch beyond a press's intent is open: the nudge on screen is kept.
    /// A carried-on intent leaves the nudge to be decided again, as its key
    /// may never have gone.
    #[must_use]
    pub fn switch_landed(&self) -> bool {
        self.wind.as_ref().is_some_and(|w| !w.intent)
    }

    /// A Codex footer read: an intended switch whose footer shows the cheaper
    /// model has landed ([`WindDown::intent`]).
    pub fn footer_seen(&mut self, setting: &CodexSetting) {
        if let Some(w) = &mut self.wind
            && w.intent
            && setting.runs(&w.to)
        {
            w.intent = false;
        }
    }

    /// The harness's own Esc stopped Codex's goal turn after the switch (Esc
    /// aborts the turn and pauses the goal together): the goal is the
    /// harness's to resume at the reset, and the interrupt shown is no
    /// person's.
    ///
    /// Stopping the save's own turn past [`WIND_DOWN_BOUND`] spends none of
    /// [`GOAL_STOPS`]; it is the one Esc the bound allows ([`WindDown::overran`]).
    pub fn own_esc(&mut self, now: Instant) {
        if let Some(w) = &mut self.wind {
            if w.phase == WindPhase::Winding {
                w.overran = true;
            } else {
                w.goal_paused = true;
                w.stops += 1;
            }
            w.stop_at = Some(now);
            self.own_interrupt = Some(now);
            self.wind_events.push(WindEvent::Typed {
                wind: w.clone(),
                phase: wind_phase_word(w),
            });
        }
    }

    /// A save-then-wait switch is open (owed, winding down, restoring or
    /// holding).
    #[must_use]
    pub fn switch_open(&self) -> bool {
        self.wind.is_some()
    }

    /// The switch holds the session on its own model until the reset.
    #[must_use]
    pub fn holding(&self) -> bool {
        self.wind
            .as_ref()
            .is_some_and(|w| matches!(w.phase, WindPhase::Holding { .. }))
    }

    /// The open switch, if any (the conformance walk's projection, the
    /// ledger's reason).
    #[must_use]
    pub fn wind(&self) -> Option<&WindDown> {
        self.wind.as_ref()
    }

    /// The model and effort the harness's `/model` restore puts back while in
    /// flight (within [`TurnEndTiming::restore_pick`], picker not left, no
    /// keystroke since; [`Self::picker_seen`]): the only picker the approval
    /// policy answers.
    #[must_use]
    pub fn restore_target(&self, now: Instant) -> Option<CodexSetting> {
        let w = self.wind.as_ref()?;
        let typed = w.restore_at?;
        (matches!(w.phase, WindPhase::Restore { .. })
            && now.saturating_duration_since(typed) < self.timing.restore_pick)
            .then(|| w.from.clone())
    }

    /// A screen read during the harness's `/model` restore: `shown` whether
    /// Codex's model picker is up, `person` time since a person last typed.
    /// The restore's claim on the picker ends once the picker has left (gone
    /// on two reads [`PICKER_SETTLE`] apart, or gone at an idle point) or a
    /// person has typed since the `/model`; later pickers are theirs
    /// ([`Self::restore_target`]).
    pub fn picker_seen(&mut self, shown: bool, person: Option<Duration>, now: Instant) {
        let Some(w) = &mut self.wind else {
            return;
        };
        let Some(at) = w.restore_at else {
            return;
        };
        let persons =
            person.is_some_and(|ago| now.checked_sub(ago).is_some_and(|typed| typed > at));
        if persons {
            w.restore_at = None;
            w.restore_shown = false;
            w.picker_gone_at = None;
        } else if shown {
            w.restore_shown = true;
            w.picker_gone_at = None;
        } else if w.restore_shown {
            let gone = *w.picker_gone_at.get_or_insert(now);
            if now >= later(gone, PICKER_SETTLE) {
                w.restore_at = None;
                w.restore_shown = false;
                w.picker_gone_at = None;
            }
        }
    }

    /// What to do about a running turn while a switch is open. `busy` is the
    /// running turn's work so far ([`RunningTurn`]) or `None`, `goal` the
    /// footer's goal, `person` whether a person's hand is on the session.
    ///
    /// * Owed wind-down or owed model: any running turn gets one Esc
    ///   ([`GoalStop::Esc`]).
    /// * Holding: a pursued goal's turn gets one.
    /// * Winding: the save's turn gets one Esc only once its work
    ///   ([`WindDown::wound`] plus `busy`) reaches [`WIND_DOWN_BOUND`]
    ///   ([`WindDown::overran`]); still running after it, a person is told.
    ///
    /// Never a person's turn ([`RunningTurn::person`]), never an intended-only
    /// switch ([`WindDown::intent`]), never twice into one turn (an Esc waits
    /// [`TurnEndTiming::take_within`]), and at most [`GOAL_STOPS`] per switch
    /// (`/goal pause` counts); past them a person is told once
    /// ([`GoalStop::Tell`], [`Self::goal_told`]).
    #[must_use]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "StopGoal",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "StopWindDown",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    pub fn goal_stop(
        &self,
        busy: Option<Duration>,
        goal: Option<CodexGoal>,
        person: bool,
        now: Instant,
    ) -> Option<GoalStop> {
        let w = self.wind.as_ref()?;
        let worked = busy?;
        if person || w.intent {
            return None;
        }
        let stoppable = match w.phase {
            WindPhase::Owed | WindPhase::Restore { hold: true } => true,
            WindPhase::Holding { .. } => goal == Some(CodexGoal::Pursuing),
            WindPhase::Winding => w.wound.saturating_add(worked) >= WIND_DOWN_BOUND,
            WindPhase::Restore { hold: false } => false,
        };
        if !stoppable
            || w.stop_at
                .is_some_and(|t| now < later(t, self.timing.take_within))
        {
            return None;
        }
        if w.phase == WindPhase::Winding {
            return if !w.overran {
                Some(GoalStop::Esc)
            } else if w.said.contains(&"winding") {
                None
            } else {
                Some(GoalStop::Tell)
            };
        }
        if w.stops < GOAL_STOPS {
            Some(GoalStop::Esc)
        } else if w.said.contains(&"goal") {
            None
        } else {
            Some(GoalStop::Tell)
        }
    }

    /// The stops are spent and the goal runs on ([`GoalStop::Tell`]): a person
    /// is told once per switch, the note kept up ([`WindEvent::GoalNote`]). A
    /// save still running after its bound's Esc gets its own note, once.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "Tell",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    pub fn goal_told(&mut self) {
        let Some(w) = &mut self.wind else {
            return;
        };
        if w.phase == WindPhase::Winding {
            let text = format!(
                "{to} was switched in only to commit and push the work, but its turn has run past \
                 {bound} min and aterm's Esc did not stop it: stop it by hand (Esc) — aterm then \
                 puts the session back on {back} and waits for the limit to reset",
                to = w.to,
                bound = WIND_DOWN_BOUND.as_secs() / 60,
                back = w.from.words()
            );
            self.wind_note("winding", text);
            return;
        }
        if w.said.contains(&"goal") {
            return;
        }
        w.said.push("goal");
        let on = match w.phase {
            WindPhase::Holding { .. } => w.from.words(),
            _ => w.to.clone(),
        };
        self.wind_events.push(WindEvent::GoalNote(format!(
            "{GOAL_NOTE_HEAD} on {on} after aterm stopped it {GOAL_STOPS} times; stop it by hand \
             (Esc, then /goal pause) — aterm types nothing into it until it stops"
        )));
    }

    /// Carries on a switch a previous loop opened and did not close
    /// (`approvals::open_wind_down`). The turn ending at the first point after
    /// it is the harness's unless a person's hand shows ([`WindDown::seeded`]);
    /// a running wind-down stays winding so its end still judges the marker.
    /// An interrupt from the harness's last Esc (last row carrying `esc=`,
    /// written within [`TurnEndTiming::take_within`] of `now`) is no person's
    /// and gets no second Esc; an older `esc=` is spent. Notes said stay said
    /// (`told=`); a hold counts from its row's start. This loop's own switch
    /// wins.
    pub fn seed_wind(&mut self, mut wind: WindDown, now: Instant) {
        if self.wind.is_some() {
            return;
        }
        // The harness's last Esc, fresh: the interrupt the next point shows
        // is its own, and the turn it stopped no person's without their hand.
        let take = self.timing.take_within;
        let esc = wind
            .seeded_at
            .or(wind.stop_at)
            .is_some_and(|row| now < later(row, take))
            && wind.stop_at.is_some();
        if !esc {
            wind.stop_at = None;
        }
        wind.seeded = esc || matches!(wind.phase, WindPhase::Owed | WindPhase::Winding);
        wind.pre = false;
        wind.restore_at = None;
        wind.restore_shown = false;
        wind.picker_gone_at = None;
        wind.footer_since = None;
        if esc || (wind.phase == WindPhase::Owed && wind.goal_paused && !wind.pause_typed) {
            self.own_interrupt = Some(now);
        }
        // A carried hold's start is placed from its row's unix time: the
        // sleeps before this loop are in it already.
        self.hold_slept = Duration::ZERO;
        self.wind = Some(wind);
    }

    /// What of this policy's own holds the open switch at point `r`, for the
    /// loop's stood-still note: a sandboxed thread ([`StoodBy::Sandbox`]), an
    /// owed save whose footer lacks the cheaper model ([`StoodBy::Footer`]),
    /// or spent restore tries ([`StoodBy::RestoreSpent`]). `None`: a person's
    /// hand, a draft or the terminal holds it.
    #[must_use]
    pub fn switch_hold(&self, r: &TurnEndReading) -> Option<StoodBy> {
        let w = self.wind.as_ref()?;
        if r.sandbox_fell.is_some() {
            return Some(StoodBy::Sandbox);
        }
        match w.phase {
            WindPhase::Owed
                if w.intent || !r.model_field.as_ref().is_some_and(|m| m.runs(&w.to)) =>
            {
                Some(StoodBy::Footer)
            }
            WindPhase::Restore { .. } if w.restore_tries >= RESTORE_TRIES => {
                Some(StoodBy::RestoreSpent)
            }
            _ => None,
        }
    }

    /// The switch stood still (owed, winding or owing the model back) through
    /// a long break under a Codex background terminal, held by `by`: a person
    /// is told once per switch what holds it and the fix
    /// ([`Self::switch_hold`]). Before the save the note never advises
    /// restoring the model by hand; after it, the steps are named. An
    /// intended-only press ([`WindDown::intent`]) is never said to have
    /// switched the model.
    pub fn switch_stood_still(&mut self, by: StoodBy) {
        let Some(w) = &self.wind else {
            return;
        };
        let what = match w.phase {
            WindPhase::Owed => "the save instruction is still owed",
            WindPhase::Winding => "the save's answer is still owed",
            _ => "the thread's own model is still owed back",
        };
        let saved = !matches!(w.phase, WindPhase::Owed | WindPhase::Winding);
        let by_hand = format!(
            "switch the model back by hand (/model → {})",
            w.from.words()
        );
        let held = match by {
            StoodBy::Typing => "a person is typing into the session".to_string(),
            StoodBy::Draft => "a draft stands in the composer".to_string(),
            StoodBy::Terminal => {
                "a Codex background terminal keeps the session busy after its turn ended"
                    .to_string()
            }
            StoodBy::Footer => format!(
                "Codex's footer has not shown {} since aterm pressed its switch",
                w.to
            ),
            StoodBy::Sandbox => {
                "the thread fell into a sandbox its launch bypassed, and aterm types nothing \
                 into it"
                    .to_string()
            }
            StoodBy::RestoreSpent => format!(
                "aterm's /model did not put the thread back on {}",
                w.from.words()
            ),
        };
        let fix = match (by, saved) {
            (StoodBy::Typing | StoodBy::Draft, false) => {
                "send or clear it and aterm goes on with the save".to_string()
            }
            (StoodBy::Typing | StoodBy::Draft, true) => {
                format!("send or clear it and aterm goes on, or {by_hand}")
            }
            (StoodBy::Terminal, false) => {
                "stop the terminal (/ps, /stop) and aterm goes on with the save".to_string()
            }
            (StoodBy::Terminal, true) => format!("stop the terminal (/ps, /stop), or {by_hand}"),
            (StoodBy::Footer, _) => format!(
                "check the session's model (/model): once the footer shows {}, aterm goes on with \
                 the save",
                w.to
            ),
            (StoodBy::Sandbox, _) => {
                "quit Codex and resume the thread with its launch flags (codex resume \
                 --dangerously-bypass-approvals-and-sandbox <thread>)"
                    .to_string()
            }
            (StoodBy::RestoreSpent, _) => by_hand,
        };
        let on = if by == StoodBy::Footer {
            "aterm cannot tell whether the switch landed".to_string()
        } else if w.intent && w.phase == WindPhase::Owed {
            format!(
                "aterm pressed the switch to {} but has not seen it land",
                w.to
            )
        } else {
            format!("the session stays on {} meanwhile", w.to)
        };
        let text = format!("{held}, and {what}: {on} — {fix}");
        self.wind_note("background", text);
    }

    /// What the switch left for the loop to ledger and to say, oldest first.
    pub fn take_wind_events(&mut self) -> Vec<WindEvent> {
        std::mem::take(&mut self.wind_events)
    }

    /// A note, said once per switch under `key`.
    fn wind_note(&mut self, key: &'static str, text: String) {
        if let Some(w) = &mut self.wind
            && !w.said.contains(&key)
        {
            w.said.push(key);
            self.wind_events.push(WindEvent::Note(text));
        }
    }

    /// An edge of the switch, ledgered (`skipped`) with the switch as it
    /// stands — or, `closed`, as it stood when it closed with `phase`.
    fn wind_edge(
        &mut self,
        rule: &'static str,
        what: String,
        closed: Option<(WindDown, &'static str)>,
    ) {
        let (wind, phase) = match closed {
            Some(c) => c,
            None => match &self.wind {
                Some(w) => (w.clone(), wind_phase_word(w)),
                None => return,
            },
        };
        self.wind_events.push(WindEvent::Edge {
            rule,
            what,
            wind,
            phase,
        });
    }

    /// The switch ends: a person took the session over (`released`) or the
    /// session is back on its model and going on (`done`).
    fn close_wind(&mut self, rule: &'static str, what: String, phase: &'static str) {
        if let Some(w) = self.wind.take() {
            self.wind_edge(rule, what, Some((w, phase)));
        }
    }

    /// Folds a judged point into the open switch (last step of
    /// [`Self::observe`]): `awaited` is the act the point follows (`None`:
    /// someone else's turn).
    fn observe_wind(
        &mut self,
        reading: &TurnEndReading,
        awaited: Option<&'static str>,
        ours_stopped: bool,
        now: Instant,
    ) {
        let Some(w) = self.wind.clone() else {
            return;
        };
        // A point: the harness's last Esc has had its answer.
        let (pre, seeded, seeded_at) = self.wind.as_mut().map_or((false, false, None), |w| {
            w.stop_at = None;
            (
                std::mem::take(&mut w.pre),
                std::mem::take(&mut w.seeded),
                w.seeded_at.take(),
            )
        });
        // A person's hand in the turn that ended here ([`seeded_hand`] for a
        // carried-on switch's in-flight turn).
        let person = reading.person_in_turn()
            || seeded_at.is_some_and(|row| seeded_hand(reading.person, reading.worked, row, now));
        // A turn nobody here typed: a person's, unless it could be Codex's
        // goal, the turn the harness's Esc stopped, the box-covered turn, or
        // a carried-on switch's in-flight turn, with no person's hand in it.
        let theirs = awaited.is_none()
            && reading.worked.is_some()
            && !ours_stopped
            && !pre
            && (!seeded || person);
        let goal_runs = reading.goal == Some(CodexGoal::Pursuing);
        // A person's `/model` before the hold: the footer shows neither the
        // cheaper nor the own model; the session is theirs.
        let persons_model = reading
            .model_field
            .as_ref()
            .filter(|m| !m.runs(&w.to) && !m.runs(&w.from.model))
            .filter(|_| !matches!(w.phase, WindPhase::Holding { .. }));
        if let Some(m) = persons_model {
            self.close_wind(
                RULE_MODEL_RESTORE,
                format!("a person switched the thread to {}", m.words()),
                "released",
            );
            return;
        }
        // Goal still pursued while held and every stop spent: a person is
        // told once (as [`Self::goal_stop`]'s `Tell` does at a busy read).
        let stoppable = matches!(
            w.phase,
            WindPhase::Owed | WindPhase::Restore { hold: true } | WindPhase::Holding { .. }
        );
        if stoppable && goal_runs && !person && w.stops >= GOAL_STOPS {
            self.goal_told();
        }
        match w.phase {
            WindPhase::Owed => {
                // An intended press whose footer shows the cheaper model has
                // landed ([`Self::footer_seen`]).
                if let Some(m) = &reading.model_field {
                    self.footer_seen(m);
                }
                // The switch that did not land: past the box-covered point
                // the footer still shows the own model and the rollout does
                // not say the cheaper one ran since the press. Nothing of it
                // goes on. With a person's keystroke since the opening and the
                // footer on another model, their `/model` put it there: the
                // session is theirs.
                let on_to = reading
                    .thread_model
                    .as_deref()
                    .is_some_and(|t| CodexSetting::slug_runs(t, &w.to));
                let persons_since = reading.person.is_some_and(|ago| {
                    now.checked_sub(ago)
                        .is_some_and(|t| w.opened_at.is_some_and(|o| t >= o))
                });
                let off_to = reading.model_field.as_ref().filter(|m| !m.runs(&w.to));
                if !pre && let Some(m) = off_to {
                    if !on_to || persons_since {
                        let what = if on_to {
                            format!("a person put the thread back on {}", m.words())
                        } else {
                            format!(
                                "the thread is still on {}: the switch to {} did not land",
                                m.words(),
                                w.to
                            )
                        };
                        self.close_wind(RULE_WIND_DOWN, what, "released");
                        return;
                    }
                    // Rollout says cheaper, footer says own: a lag, waited on
                    // and reported past the bound.
                    let since = self
                        .wind
                        .as_mut()
                        .map_or(now, |wd| *wd.footer_since.get_or_insert(now));
                    if now >= later(since, OWED_FOOTER_BOUND) {
                        self.wind_note(
                            "footer",
                            format!(
                                "{} is close to its usage limit and aterm pressed Codex's switch \
                                 to {}, but the footer shows {} while the thread's last turn ran \
                                 {}, so aterm cannot tell which model the session is on: it types \
                                 nothing — check the session's model (/model)",
                                w.from.model,
                                w.to,
                                m.words(),
                                w.to
                            ),
                        );
                    }
                } else if reading.model_field.is_none() {
                    // A footer showing no model holds the save; reported past
                    // the bound.
                    let since = self
                        .wind
                        .as_mut()
                        .map_or(now, |wd| *wd.footer_since.get_or_insert(now));
                    if now >= later(since, OWED_FOOTER_BOUND) {
                        self.wind_note(
                            "footer",
                            format!(
                                "{} is close to its usage limit and aterm pressed Codex's switch \
                                 to {}, but the footer shows no model, so aterm cannot tell \
                                 whether the switch landed: it types nothing — check the \
                                 session's model (/model)",
                                w.from.model, w.to
                            ),
                        );
                    }
                } else if let Some(wd) = &mut self.wind {
                    wd.footer_since = None;
                }
                if theirs && (!goal_runs || person) {
                    // A person's own message: the wind-down is theirs to give;
                    // the model is still owed back.
                    if let Some(w) = &mut self.wind {
                        w.phase = WindPhase::Restore { hold: false };
                        w.goal_paused = false;
                    }
                    self.wind_edge(
                        RULE_WIND_DOWN,
                        "a person took the session over before the wind-down".to_string(),
                        None,
                    );
                } else if reading.composer.drafted()
                    && !self.wind.as_ref().is_some_and(|wd| wd.intent)
                {
                    // An intended-only press is not known to have switched the
                    // model; its footer holds the save and the note says so.
                    self.wind_note(
                        "draft",
                        format!(
                            "{} is close to its usage limit and the session is on {} to save the \
                             work, but a draft stands in the composer: the save instruction \
                             waits until it is sent or cleared",
                            w.from.model, w.to
                        ),
                    );
                }
            }
            WindPhase::Winding => {
                // The wind-down's turn ended on an API error or full context:
                // not its end; the wall's rule carries it on
                // ([`wind_down_act`]). Its work counts toward [`WIND_DOWN_BOUND`].
                if matches!(
                    reading.wall,
                    Some(WallKind::ApiError { .. } | WallKind::Overloaded | WallKind::Context)
                ) {
                    if let Some(wd) = &mut self.wind {
                        wd.wound = wd.wound.saturating_add(reading.worked.unwrap_or_default());
                    }
                    return;
                }
                // The wind-down's own point, a person's steer in it, or the
                // harness's bound Esc ([`Self::goal_stop`]): the marker is
                // judged here.
                let saved = reading.said_tail.as_deref().is_some_and(|t| {
                    t.lines()
                        .any(|l| l.trim().trim_matches('`') == w.marker.as_str())
                });
                let hit_wall = matches!(
                    reading.wall,
                    Some(
                        WallKind::UsageSession
                            | WallKind::UsageWeekly
                            | WallKind::ModelBucket { .. }
                    )
                );
                if let Some(wd) = &mut self.wind {
                    wd.saved = Some(saved);
                    wd.phase = WindPhase::Restore { hold: true };
                    if hit_wall && let Some(r) = reading.reset_at {
                        wd.back_at = Some(wd.back_at.map_or(r, |b| b.max(r)));
                    }
                }
                // The note first, so the edge's row carries it as said.
                if !saved {
                    let said = reading
                        .said_tail
                        .as_deref()
                        .map(|t| {
                            let flat = t.split_whitespace().collect::<Vec<_>>().join(" ");
                            let cut: String = flat.chars().take(240).collect();
                            format!(": its last words were \"{cut}\"")
                        })
                        .unwrap_or_default();
                    let why = if hit_wall {
                        format!(" — {} hit the usage limit too", w.to)
                    } else if ours_stopped {
                        format!(
                            " — aterm stopped its turn after {} min of work, since it was switched \
                             in only to commit and push",
                            WIND_DOWN_BOUND.as_secs() / 60
                        )
                    } else {
                        String::new()
                    };
                    self.wind_note(
                        "unsaved",
                        format!(
                            "{} did not confirm that the work is committed and pushed{why}{said}. \
                             aterm puts the session back on {} and waits for the limit to reset; \
                             check the repository",
                            w.to,
                            w.from.words()
                        ),
                    );
                }
                let judged = if saved { "saved" } else { "not saved" };
                self.wind_edge(
                    RULE_WIND_DOWN,
                    if ours_stopped {
                        format!(
                            "{judged}: its turn stopped past {} min",
                            WIND_DOWN_BOUND.as_secs() / 60
                        )
                    } else {
                        judged.to_string()
                    },
                    None,
                );
            }
            WindPhase::Restore { hold } => {
                // An idle point with the harness's picker seen and gone: it has
                // left once gone [`PICKER_SETTLE`] ([`Self::picker_seen`]); an
                // idle frame between its boxes is not leaving.
                if let Some(wd) = &mut self.wind
                    && wd.restore_shown
                {
                    let gone = *wd.picker_gone_at.get_or_insert(now);
                    if now >= later(gone, PICKER_SETTLE) {
                        wd.restore_at = None;
                        wd.restore_shown = false;
                        wd.picker_gone_at = None;
                    }
                }
                if reading
                    .model_field
                    .as_ref()
                    .is_some_and(|m| m.same(&w.from))
                {
                    if hold {
                        if let Some(wd) = &mut self.wind {
                            wd.phase = WindPhase::Holding { since: now };
                            wd.restore_at = None;
                            wd.restore_shown = false;
                        }
                        self.hold_slept = Duration::ZERO;
                        self.wind_edge(
                            RULE_MODEL_RESTORE,
                            format!("back on {}", w.from.words()),
                            None,
                        );
                        if let Some(wd) = &self.wind {
                            self.wind_events.push(WindEvent::Hold { wind: wd.clone() });
                        }
                    } else {
                        self.close_wind(
                            RULE_MODEL_RESTORE,
                            format!("back on {}; the session is the person's", w.from.words()),
                            "done",
                        );
                    }
                    return;
                }
                // Codex's goal runs before the model is back: a person's
                // `/goal resume` only with their hand in it (then the session
                // is theirs, model still owed back); otherwise it escaped its
                // stop and is stopped again ([`Self::goal_stop`]).
                if hold && goal_runs {
                    if person {
                        if let Some(wd) = &mut self.wind {
                            wd.phase = WindPhase::Restore { hold: false };
                            wd.goal_paused = false;
                        }
                        self.wind_edge(
                            RULE_MODEL_RESTORE,
                            "a person resumed Codex's goal; its model is still owed back"
                                .to_string(),
                            None,
                        );
                    }
                    return;
                }
                if theirs && !goal_runs && hold {
                    if let Some(wd) = &mut self.wind {
                        wd.phase = WindPhase::Restore { hold: false };
                        wd.goal_paused = false;
                    }
                    self.wind_edge(
                        RULE_MODEL_RESTORE,
                        "a person took the session over; its model is still owed back".to_string(),
                        None,
                    );
                }
                if w.restore_tries >= RESTORE_TRIES {
                    let effort = w.from.effort.as_deref().map_or_else(String::new, |e| {
                        let e = e.to_lowercase();
                        if matches!(e.as_str(), "max" | "ultra") {
                            format!(
                                " → {} → {}",
                                aterm_phase::anchor("codex.pick.more"),
                                capitalized(&e)
                            )
                        } else {
                            format!(" → {}", capitalized(&e))
                        }
                    });
                    self.wind_note(
                        "restore",
                        format!(
                            "aterm could not put the session back on {}: switch it by hand — \
                             /model → {}{effort}, pressing s (this conversation) on the effort — \
                             and it then waits for the limit to reset",
                            w.from.words(),
                            w.from.model
                        ),
                    );
                }
            }
            WindPhase::Holding { since } => {
                let moved = reading.model_field.as_ref().filter(|m| !m.same(&w.from));
                // A person's keystroke since the hold began: a model the
                // footer shows then is theirs, the cheaper one included.
                let persons_since = person
                    || reading
                        .person
                        .is_some_and(|ago| now.checked_sub(ago).is_some_and(|t| t >= since));
                if goal_runs {
                    // The goal runs during the hold: a person's `/goal resume`
                    // only with their hand in it; otherwise it is stopped again.
                    if person {
                        self.close_wind(
                            RULE_MODEL_RESTORE,
                            "a person resumed Codex's goal during the hold".to_string(),
                            "released",
                        );
                    }
                } else if theirs {
                    self.close_wind(
                        RULE_MODEL_RESTORE,
                        "a person's message ended the hold".to_string(),
                        "released",
                    );
                } else if let Some(m) = moved {
                    if m.runs(&w.to) && !persons_since {
                        if let Some(wd) = &mut self.wind {
                            wd.phase = WindPhase::Restore { hold: true };
                        }
                        self.wind_edge(
                            RULE_MODEL_RESTORE,
                            format!("the thread is on {} again; restored again", m.words()),
                            None,
                        );
                    } else {
                        self.close_wind(
                            RULE_MODEL_RESTORE,
                            format!("a person switched the thread to {}", m.words()),
                            "released",
                        );
                    }
                }
            }
        }
    }
}

/// The `/model` alias a bucket's notice names (`You've reached your Fable
/// limit` → `fable`, `Opus 4.1 limit reached` → `opus`): the first word that
/// is one of [`MODEL_ALIASES`].
#[must_use]
pub fn bucket_model(message: &str) -> Option<String> {
    message
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .find(|w| MODEL_ALIASES.contains(&w.as_str()))
}

/// The work a finished turn's done row reports (`✻ Cooked for 3m 2s · done
/// 4:24 PM` → 182 s): Claude Code's own measure, which a supervisor that
/// attached mid-turn saw only part of.
#[must_use]
pub fn done_row_work(rows: &[String]) -> Option<Duration> {
    let row = status_row(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" for ")?;
    let span = rest.split(" · ").next()?.trim();
    let mut secs = 0u64;
    for word in span.split_whitespace() {
        let unit = word.chars().last()?;
        let n: u64 = word[..word.len() - unit.len_utf8()].parse().ok()?;
        secs += n * match unit {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    Some(Duration::from_secs(secs))
}

/// The clock time of a turn's DONE ROW (`✻ Sautéed for 1d 2h
/// 58m · done 12:19 PM · 1 shell still running` → `12:19 PM`): Claude Code's
/// own stamp of when the turn ended — for a turn that ended on a wall, when
/// the wall was drawn. `None` without a done row on the screen.
#[must_use]
pub fn done_row_clock(rows: &[String]) -> Option<String> {
    let row = status_row(rows).filter(|r| is_done_row(r))?;
    let (_, rest) = row.split_once(" done ")?;
    let clock = rest.split(" · ").next()?.trim();
    (clock.starts_with(|c: char| c.is_ascii_digit())).then(|| clock.to_string())
}

/// Whether a suggestion is a continuation: made only of
/// [`SUGGESTION_ALLOW`] pieces, case and closing punctuation aside.
#[must_use]
pub fn suggestion_allowed(text: &str) -> bool {
    let lower = text
        .to_lowercase()
        .replace(" and ", ",")
        .replace(" then ", ",");
    let mut any = false;
    for piece in lower.split([',', ';', '.', '!']) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if !SUGGESTION_ALLOW.contains(&piece) {
            return false;
        }
        any = true;
    }
    any
}

/// What the worker's last words ask of a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A report, or nothing: the point may be continued.
    Plain,
    /// An offer to go on (`want me to take that on?`): continued.
    Offer,
    /// A question for a person: answered.
    Question,
    /// A decision asked for: answered, with why.
    Stop(String),
    /// A decision that names an irreversible act ([`destructive_word`]) —
    /// an offer to do it, a question or a choice that names it anywhere:
    /// answered only with [`REVERSIBLE_ANSWER`], with why.
    Irreversible(String),
}

/// Judges the worker's last words ([`Said`]). `tail` is one line per screen
/// row ([`aterm_phase::said_tail`]), so phrases are matched on the rows joined
/// with spaces, and the question is the last sentence of the last unit (from
/// the last enumerated option row on), never the last row. In order:
///
/// 1. A stop phrase anywhere is a stop; it names the act when a destructive
///    word is anywhere in the tail.
/// 2. An offer with a destructive word after the first offer phrase, or any
///    question naming a destructive act, is a stop.
/// 3. A last sentence that asks (`?`, or a trailing `(y/n)`, `[y/N]`,
///    `(yes/no)`, ignoring a trailing aside): a choice between two or more
///    listed options under an ask that chooses ([`asks_to_choose`]), an
///    either/or the worker would act on ([`either_or`]), or a
///    [`CHOICE_PHRASES`] ask ([`phrase_asks`]) is a stop; any destructive word
///    ([`destructive_word`], inflections included) in a choice's tail names
///    the act. An offer stays an offer, anything else is a question.
/// 4. A tail that does not ask is an offer when it opens next steps, else
///    plain.
///
/// Every stop and question is answered ([`RULE_ANSWER`]); one naming an
/// irreversible act ([`Said::Irreversible`]) only with [`REVERSIBLE_ANSWER`].
/// The reason is the escalation's text under `answer_questions = false`.
///
/// Remembered for the last tail read on this thread (`LastTail`).
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    thread_local! {
        static LAST: LastTail<Said> = const { LastTail::new() };
    }
    LAST.with(|last| last.get_or(tail, || classify_said_read(tail)))
}

/// THE LAST TAIL A PURE READING OF THE WORKER'S WORDS WAS MADE OF, and what
/// it read, kept per thread: [`classify_said`] and [`says_done`] are pure
/// functions of the tail, and one point of the loop asks them of the same
/// tail several times (`decide_turn_end` up to three times, the reading's
/// `asked` around it, `observe`'s done check). Each asking is answered
/// exactly as a fresh read would answer it: the read is reused only for the
/// same text, compared whole.
struct LastTail<T>(std::cell::RefCell<Option<(String, T)>>);

impl<T: Clone> LastTail<T> {
    const fn new() -> Self {
        LastTail(std::cell::RefCell::new(None))
    }

    /// What `read` gives for `tail`: the remembered answer when `tail` is the
    /// last tail read, else `read()`, remembered.
    fn get_or(&self, tail: &str, read: impl FnOnce() -> T) -> T {
        if let Some((seen, answer)) = &*self.0.borrow()
            && seen == tail
        {
            return answer.clone();
        }
        let answer = read();
        *self.0.borrow_mut() = Some((tail.to_owned(), answer.clone()));
        answer
    }
}

/// [`classify_said`] on a tail, read afresh.
fn classify_said_read(tail: &str) -> Said {
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        // A stop naming a destructive act anywhere asks consent to it; its
        // answer must consent to nothing irreversible.
        if let Some(w) = destructive_word(&flat) {
            return Said::Irreversible(format!("the worker said \"{p}\" of an act that would {w}"));
        }
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    let recommends = RECOMMEND_PHRASES.iter().any(|p| flat.contains(p));
    let offer_at = OFFER_PHRASES.iter().filter_map(|p| flat.find(p)).min();
    if let Some(at) = offer_at
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker offers to {w}"));
    }
    if let Some(w) = questions(&flat).into_iter().find_map(destructive_word) {
        return Said::Irreversible(format!("the worker asks whether to {w}"));
    }
    let offer = offer_at.is_some();
    let lines: Vec<&str> = lower
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let unit_start = lines.iter().rposition(|l| is_option_line(l)).unwrap_or(0);
    let unit = lines[unit_start..].join(" ");
    let unit = without_trailing_aside(&unit).trim_end_matches([')', '"', '*', '`', ' ']);
    let asks = unit.ends_with('?');
    let listed = lines.iter().filter(|l| is_option_line(l)).count();
    let ask = last_sentence(unit);
    // A choice between options: two or more listed and a choosing ask, or an
    // either/or the worker would act on.
    let listed_choice = listed >= 2 && asks_to_choose(ask);
    let chooses = asks && (listed_choice || either_or(ask));
    let choice_phrase = CHOICE_PHRASES
        .iter()
        .filter_map(|p| flat.find(p).map(|at| (*p, at)))
        .min_by_key(|&(_, at)| at);
    let phrase_choice = choice_phrase.filter(|&(p, at)| phrase_asks(&flat, &lines, p, at));
    // A choice naming a destructive act anywhere in the tail (option,
    // description row, ask, preceding report): its answer consents to it.
    if (chooses || phrase_choice.is_some())
        && let Some(w) = destructive_word(&flat)
    {
        return Said::Irreversible(format!("the worker offers a choice that would {w}"));
    }
    if chooses && !recommends {
        return Said::Stop(if listed_choice {
            "the worker listed options with no recommendation".to_string()
        } else {
            "the worker asks to choose between options".to_string()
        });
    }
    if let Some((p, _)) = phrase_choice {
        return Said::Stop(format!("the worker said \"{p}\""));
    }
    // A choice phrase that does not ask (a report that says `which option`)
    // is no choice — but an act named after it is still a person's.
    if let Some((_, at)) = choice_phrase
        && let Some(w) = destructive_word(&flat[at..])
    {
        return Said::Irreversible(format!("the worker asks to choose whether to {w}"));
    }
    if !asks {
        return if offer { Said::Offer } else { Said::Plain };
    }
    if offer { Said::Offer } else { Said::Question }
}

/// The yes/no markers a worker closes a question with, lowercased.
const YES_NO_MARKERS: &[&str] = &["(y/n)", "[y/n]", "(yes/no)", "[yes/no]", "(y/n)?", "[y/n]?"];

/// `text` (lowercased) with each yes/no marker ([`YES_NO_MARKERS`]) read as
/// the `?` it means: `delete them now? (y/n)` and `proceed (y/n)` both ask.
fn yes_no_asks(text: &str) -> String {
    let mut out = text.to_string();
    for m in YES_NO_MARKERS {
        out = out.replace(m, "?");
    }
    out
}

/// Every sentence of `flat` that asks — ends in a `?` followed by a space or
/// the end — whole, from the last sentence end before it.
fn questions(flat: &str) -> Vec<&str> {
    let bytes = flat.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        let ends = matches!(bytes.get(i + 1), None | Some(b' '));
        if !ends {
            continue;
        }
        match b {
            b'?' => {
                out.push(flat[start..=i].trim());
                start = i + 1;
            }
            b'.' | b'!' | b';' | b':' => start = i + 1,
            _ => {}
        }
    }
    out
}

/// `unit` without a parenthetical aside after its last sentence
/// (`Should I rename it? (I have not touched it yet.)` asks).
fn without_trailing_aside(unit: &str) -> &str {
    let mut t = unit.trim_end();
    while t.ends_with(')') {
        let Some(open) = t.rfind('(') else {
            break;
        };
        let before = t[..open].trim_end();
        if !before.ends_with(['?', '.', '!']) {
            break;
        }
        t = before;
    }
    t
}

/// The last sentence of `text`: what follows the last `.`, `!`, `?`, `:` or
/// `;` that is followed by a space (so `v1.2` and `a.rs` do not end one).
fn last_sentence(text: &str) -> &str {
    let body = text.trim_end_matches('?');
    let mut start = 0;
    let bytes = body.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?' | b':' | b';') && bytes.get(i + 1) == Some(&b' ') {
            start = i + 1;
        }
    }
    text[start..].trim()
}

/// Leads that, before an either/or's ` or `, make it a choice the worker
/// would act on (`should I … or …?`, `which would you prefer: … or …?`).
/// Without one (`did the suite pass on your machine, or should I rerun it?`)
/// it asks something only the person knows.
pub const CHOICE_LEADS: &[&str] = &[
    "should i",
    "shall i",
    "should we",
    "shall we",
    "want me to",
    "do you want",
    "would you like",
    "prefer",
    "which",
];

/// Whether the asking sentence `ask` (lowercased, its `?` trimmed) is an
/// either/or the worker would act on ([`CHOICE_LEADS`] before its ` or `).
fn either_or(ask: &str) -> bool {
    ask.find(" or ")
        .is_some_and(|at| CHOICE_LEADS.iter().any(|l| ask[..at].contains(l)))
}

/// Whether the asking sentence under a list of options asks to choose one
/// (`which one?`, `shall I start?`, `1 or 2?`) rather than something else
/// (`does that look right?`).
fn asks_to_choose(ask: &str) -> bool {
    let words: Vec<&str> = ask
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    either_or(ask)
        || CHOICE_LEADS.iter().any(|l| ask.contains(l))
        || words.iter().any(|w| {
            matches!(
                *w,
                "which" | "option" | "options" | "choose" | "pick" | "either" | "or"
            )
        })
}

/// Whether a [`CHOICE_PHRASES`] ask `p` at `at` in `flat` asks: its sentence
/// ends `?`, it is in the tail's last sentence, or its row introduces the
/// enumerated options. A phrase in an earlier sentence of a report does not.
fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
    // The first sentence end after the phrase: `.`, `!` or `?` then a space
    // or the end; a `.` after a digit (`1. drop`) is an option number.
    let end = bytes.iter().enumerate().position(|(i, b)| {
        matches!(b, b'.' | b'!' | b'?')
            && matches!(bytes.get(i + 1), None | Some(b' '))
            && !(*b == b'.' && i > 0 && bytes[i - 1].is_ascii_digit())
    });
    let asks_here = end.is_none_or(|i| bytes[i] == b'?' || i + 1 == bytes.len());
    let introduces = lines
        .iter()
        .position(|l| l.contains(p))
        .is_some_and(|k| lines.get(k + 1).is_some_and(|l| is_option_line(l)));
    asks_here || introduces
}

/// The first of [`DESTRUCTIVE_WORDS`] in `text` as a whole word (or phrase),
/// or an inflection of one ([`inflects`]), returned as its base form.
pub(crate) fn destructive_word(text: &str) -> Option<&'static str> {
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('-'))
        .collect();
    DESTRUCTIVE_WORDS.iter().copied().find(|d| {
        let want: Vec<&str> = d.split(' ').map(|w| w.trim_start_matches('-')).collect();
        words.windows(want.len()).any(|win| {
            win.iter()
                .zip(want.iter())
                .all(|(w, base)| inflects(w, base))
        })
    })
}

/// Whether `word` is `base` or a regular inflection: `-s`/`-es`, `-d`/`-ed`,
/// `-ing` (dropping a final `e`, doubling a final consonant), `-en` after a
/// doubled `t` (`overwritten`), and `overwrote`/`rewrote`. Over-matching a
/// noun only escalates a point that would have been answered; missing a verb
/// answers one a person had to.
fn inflects(word: &str, base: &str) -> bool {
    if word == base {
        return true;
    }
    let Some(rest) = word.strip_prefix(base) else {
        // A final `e` dropped: `delet` + `ing`/`ed`; `overwrit` + `ten`.
        let stem = base.strip_suffix('e').unwrap_or(base);
        return (stem.len() < base.len()
            && word
                .strip_prefix(stem)
                .is_some_and(|rest| matches!(rest, "ing" | "ed" | "ten")))
            || (base.ends_with("write") && word == format!("{}ote", &base[..base.len() - 3]));
    };
    if matches!(rest, "s" | "es" | "d" | "ed" | "ing" | "ten") {
        return true;
    }
    // A final consonant doubled: `drop` + `ped`, `reset` + `ting`.
    let last = base.chars().last().unwrap_or(' ');
    rest.strip_prefix(last)
        .is_some_and(|rest| matches!(rest, "ed" | "ing" | "en"))
}

/// An enumerated option row: `1.`, `2)`, `(a)`, `a)`, `- `, `• `,
/// `option a`/`option 1`.
fn is_option_line(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(['.', ')']) {
        return true;
    }
    let mut cs = t.chars();
    if let (Some(a), Some(b)) = (cs.next(), cs.next())
        && a.is_ascii_lowercase()
        && b == ')'
    {
        return true;
    }
    t.starts_with("(a)")
        || t.starts_with("(b)")
        || t.starts_with("- ")
        || t.starts_with("• ")
        || t.starts_with("option ")
}

/// What to type as a continuation: `cfg.continue_text`, with the standing
/// rules when there are any.
#[must_use]
pub fn continuation_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.continue_text, rules)
}

/// What to type where the worker said it is done: [`DONE_CHECK`], with the
/// standing rules when there are any.
#[must_use]
pub fn done_check_text(rules: Option<&str>) -> String {
    with_rules(DONE_CHECK, rules)
}

/// What to type as an answer to a question for a person: `cfg.answer_text`,
/// with the standing rules when there are any.
#[must_use]
pub fn answer_text(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    with_rules(&cfg.answer_text, rules)
}

/// The answer to a prose question under `cfg.question_priority`:
/// [`answer_text`] under [`QuestionPriority::Recommended`], else that
/// priority's policy text (the same a question dialog's free-text row gets),
/// with the standing rules when there are any. Never for an irreversible act
/// ([`reversible_answer`]).
#[must_use]
pub fn prose_answer(cfg: &SupervisorConfig, rules: Option<&str>) -> String {
    match cfg.question_priority.text() {
        Some(text) => with_rules(text, rules),
        None => answer_text(cfg, rules),
    }
}

/// The answer to a question or choice naming an irreversible act
/// ([`Said::Irreversible`]): never "take the option you would recommend" (the
/// recommendation may be the act), and never `answer_text`, which may say so.
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`REVERSIBLE_ANSWER`], with the standing rules when there are any.
#[must_use]
pub fn reversible_answer(rules: Option<&str>) -> String {
    with_rules(REVERSIBLE_ANSWER, rules)
}

fn with_rules(text: &str, rules: Option<&str>) -> String {
    match rules.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("{text} (standing rules: {r})"),
        None => text.to_string(),
    }
}

/// `at + d`, saturating at `at` when the sum overflows the clock.
fn later(at: Instant, d: Duration) -> Instant {
    at.checked_add(d).unwrap_or(at)
}

/// The decision at one point (module header). Pure: the same state, reading,
/// policy and clock give the same action.
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    // A point the live upgrade owns is the upgrade's, except at a wall: the
    // upgrade types nothing there and its host takes no step
    // (`host_steps_here`), so every wall is decided by its own rule, with no
    // relaunch over the upgrade's own (a model bucket is waited out). The
    // memory banner keeps its restart: the upgrade needs a READY an agent
    // past reading never gives. A continuation a wall's act owes
    // (`/compact`'s `then: Continue`) is paid here too, owned or not, since
    // an owed act also keeps the host out.
    if reading.upgrading && state.wind.is_none() {
        match reading.wall {
            None if state.after.is_none() => return TurnEndAction::Nothing,
            None => {}
            Some(WallKind::Memory) => {}
            Some(_) if reading.restartable => {
                let unrestartable = TurnEndReading {
                    restartable: false,
                    ..reading.clone()
                };
                return decide_turn_end(state, &unrestartable, cfg, now);
            }
            Some(_) => {}
        }
    }
    // Claude Code's critical-memory banner: nothing is typed at it, so what
    // holds a typed act back (draft, unanswered message, survey, awaited
    // act, spinner) does not hold its restart; the host's restart checks
    // idle and nobody's hand itself. A turn a person stopped still gets
    // their grace.
    if reading.wall == Some(WallKind::Memory) && !reading.interrupted {
        return memory_restart(cfg, reading);
    }
    // An act typed and its point not judged yet ([`TurnEndState::observe`],
    // which judges it at its deadline — taken or not): decided again then.
    if let Some(a) = &state.awaiting {
        if !reading.authoritative
            || reading.survey
            || matches!(reading.phase, Phase::Busy | Phase::Prompt)
        {
            return TurnEndAction::Nothing;
        }
        let due = a.due(&state.timing);
        if now < due {
            return TurnEndAction::WaitUntil {
                until: due,
                why: format!("the worker to take the {} act", a.rule),
            };
        }
    }
    // No task (none yet, or done after the done check): nothing is typed,
    // except an open save-then-wait switch's own steps, so a done task never
    // strands the thread on the cheaper model.
    if !reading.at_point(state.stale_input)
        || reading.taskless
        || (state.task_done() && state.wind.is_none())
    {
        return TurnEndAction::Nothing;
    }
    // A person at the keyboard wins: hands off for the grace after their last
    // keystroke, and after a turn they stopped with Esc (itself a keystroke,
    // whether or not the server says so) from the point on.
    let grace = Duration::from_secs(u64::from(cfg.human_grace_s));
    // The harness's own Esc (the save-then-wait switch's) is no person's.
    let persons_esc = reading.interrupted && state.own_interrupt.is_none();
    let held = match (reading.person, persons_esc) {
        (Some(ago), _) if ago < grace => Some((later(now, grace - ago), "a person is typing")),
        (_, true) => Some((
            later(state.point_at.unwrap_or(now), grace),
            "a person stopped the turn with Esc",
        )),
        _ => None,
    };
    if let Some((until, why)) = held.filter(|(until, _)| now < *until) {
        return TurnEndAction::WaitUntil {
            until,
            why: why.to_string(),
        };
    }
    // Codex's save-then-wait switch owns every point while it is open: no
    // continuation, no answer and no draft sent on the cheaper model.
    if let Some(act) = wind_down_act(state, reading, cfg, now) {
        return act;
    }
    let act = act_at_point(state, reading, cfg, now);
    // A standing draft (unchanged past the grace) is submitted in the act's
    // place under its rule, never typed over. Never into a lost login: the
    // wall would swallow it, so where `/login` was the act the point is
    // escalated.
    match act.rule_id() {
        Some(RULE_LOGIN) if reading.composer.drafted() => TurnEndAction::Escalate {
            reason: format!(
                "the login is gone: {} (a draft stands in the composer; {})",
                state.wall_words(reading),
                sign_in_how(reading.program)
            ),
        },
        Some(rule_id) if reading.composer == Composer::Typed => TurnEndAction::Submit { rule_id },
        // Aterm's own words: never sent, never typed over, never taken back
        // (a person's key may be in flight); escalated once, for any program.
        Some(_) if reading.composer == Composer::Own => TurnEndAction::Escalate {
            reason: OWN_WORDS_STAND.to_string(),
        },
        _ => act,
    }
}

/// The act at a free point ([`decide_turn_end`], past a person's grace):
/// a wall's, an owed continuation, a switch back, or the continue policy's.
fn act_at_point(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    // `continue_per_hour = 0` is no cap.
    let budgeted = |act: TurnEndAction| {
        let used = state.acts_in_window(now);
        if cfg.continue_per_hour != 0 && used >= cfg.continue_per_hour as usize {
            TurnEndAction::Escalate {
                reason: format!(
                    "continue_per_hour = {} reached: {used} typed in the last {} min",
                    cfg.continue_per_hour,
                    state.timing.window.as_secs() / 60
                ),
            }
        } else {
            act
        }
    };
    let continue_as = |rule: &'static str| {
        budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    let type_as = |rule: &'static str, text: &str| {
        budgeted(TurnEndAction::Type {
            text: with_rules(text, reading.rules.as_deref()),
            rule_id: rule,
        })
    };
    if reading.program == Program::Codex {
        // A Codex thread that fell into a sandbox cannot commit or push, so
        // nothing typed helps; the loop says so once, with the fix.
        if reading.sandbox_fell.is_some() {
            return TurnEndAction::Nothing;
        }
        // Codex's goal starts its next turn within milliseconds of a turn's
        // end, so a continuation would land as a steer. A lost login and a
        // usage or spend limit, which stop the goal, keep their walls' rules.
        let own_wall = matches!(
            reading.wall,
            Some(
                WallKind::Auth
                    | WallKind::UsageSession
                    | WallKind::UsageWeekly
                    | WallKind::Spend
                    | WallKind::ModelBucket { .. }
            )
        );
        if reading.goal == Some(CodexGoal::Pursuing) && !own_wall {
            return TurnEndAction::Nothing;
        }
    }
    if let Some(kind) = reading.wall {
        let act = wall_action(
            state,
            reading,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        return wall_breaker(state, now, act);
    }
    // A lost login the policy saw is waited out off the screen too, while its
    // track stands and the worker has not worked: the wall's row gone (the
    // `/login` dialog dismissed) is no login back, and anything typed would
    // meet the wall again. `Login successful`
    // ([`TurnEndReading::login_back`]) or the worker working
    // ([`TurnEndState::observe`]) lifts it; the owner was told when it showed
    // at a point. One NO POINT DECIDED — read only inside an act's take
    // window, its row gone before the window's end — is decided as the wall
    // it left ([`still_shown`]): `/login` once, the owner told with the
    // wall's words, which the screen no longer shows (round 16's review:
    // held as told, nothing was typed and nobody was told).
    if let Some(t) = &state.wall
        && t.class == WallClass::Auth
        && !reading.login_back
    {
        let Some((kind, shown)) = still_shown(t, reading) else {
            return TurnEndAction::Nothing;
        };
        let act = wall_action(
            state,
            &shown,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
        return wall_breaker(state, now, told_with_words(act, &shown.wall_message));
    }
    // A command's continuation, owed once it has run.
    if let Some(rule) = state.after {
        return continue_as(rule);
    }
    // A LIMIT RESET'S CONTINUATION ([`TurnEndState::reset_told`]): the
    // vendor's answer line is said after the wall, so the screen reads idle
    // with no wall, and the step under the dialog opened no wall track — the
    // point would be the continue policy's, or nobody's with it off.
    if let Some(act) = reset_continuation(state, reading, cfg, now, &continue_as) {
        return act;
    }
    // The screen left a limit it was waiting out, with no work since: a
    // usage window whose notice named its reset is decided as if the notice
    // still showed ([`still_shown`]) — by the wall's policies whole, the move
    // to another account in front of its own rule ([`wall_action`]), and
    // under `resume_limits = false` by nothing, as at the wall: the limit
    // episode told the person. One that named none, or a model bucket (a
    // person's `/model` is its way out), with no act of ours, goes at once.
    if let Some(t) = &state.wall
        && (t.class == WallClass::Usage || t.class == WallClass::Model)
    {
        if let Some((kind, shown)) = still_shown(t, reading) {
            if !cfg.resume_limits {
                return TurnEndAction::Nothing;
            }
            let act = wall_action(
                state,
                &shown,
                cfg,
                now,
                kind,
                &budgeted,
                &continue_as,
                &type_as,
            );
            return wall_breaker(state, now, act);
        }
        if t.attempts == 0 && cfg.resume_limits {
            return resume_limit(reading, &continue_as);
        }
    }
    // THE WALL'S RESUME AFTER A MOVE MADE AT IT (the accounts design, §6
    // "The move"): the limit the session left would have been continued at
    // its reset under `resume_limits`, and the move took that reset away —
    // so the same continuation, by the same rule, goes at the relaunched
    // session's first point. Not the continue policy's: under
    // `continue_policy = false` a moved session was typed nothing, where
    // with no roster it was resumed.
    if state.account_resume && cfg.resume_limits {
        return resume_limit(reading, &continue_as);
    }
    let cont = continue_policy(state, reading, cfg, now, &budgeted);
    // THE ACCOUNT MOVE AHEAD OF A WALL (the accounts design, §6): the host
    // advises another account at a plain turn end — the owner's `accounts
    // use`, the account the machine moved to and has seen served. Decided
    // where the model's return is, and as that is: it wraps the turn's own
    // act (`otherwise`: a move the host cannot make yet never holds the
    // point), and is never asked over an escalation. Ahead of the model's
    // return: a move made relaunches on the bucket's model itself and lets
    // the switch go ([`TurnEndState::restarted`]) — one relaunch, not two.
    // Not while this session's own last move is still unanswered: that one
    // is proven, or struck, first. Nor to an account a move ahead of the wall
    // could not be made to within the last window: that is asked again at
    // the wall, or an hour on, never at every turn end.
    if let Some(AccountAdvice::Move { to }) = reading.advised()
        && state.account.is_none()
        && !state.account_ahead_refused(to, now)
        && account_rotates(state, reading, cfg)
        && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return TurnEndAction::Restart {
            why: Restart::Account {
                to: to.clone(),
                at: AccountAt::Ahead,
            },
            rule_id: RULE_ACCOUNT,
            otherwise: Box::new(cont),
        };
    }
    // The bucket's reset has come: relaunched on the bucket's model first,
    // which carries it on — or, where no relaunch is made, the point's own
    // act, the model left as it is (session-only: nothing else moved).
    if let Some(m) = &state.model
        && let Some(at) = m.back_at
        && now >= at
        && reading.restartable
        && !matches!(cont, TurnEndAction::Escalate { .. })
    {
        return TurnEndAction::Restart {
            why: Restart::ModelBack { to: m.from.clone() },
            rule_id: RULE_MODEL_RESTORE,
            otherwise: Box::new(cont),
        };
    }
    cont
}

/// The continue policy at a point with no wall: the question answered or the
/// turn continued (the worker's suggestion first) once the short streak's
/// back-off passes; a question escalated only when answers are switched off.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "Continue",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
fn continue_policy(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
) -> TurnEndAction {
    let (asked, irreversible) = match reading.asked() {
        Some((why, irreversible)) => (Some(why), irreversible),
        None => (None, false),
    };
    // A done report gets the done check whatever else it said: answering a
    // done worker's "what's next?" with `answer_text` invites invented work.
    let done = state.done == 1 && !reading.interrupted;
    match &asked {
        Some(why) if !cfg.answer_questions => {
            return TurnEndAction::Escalate {
                reason: format!("{why}; {ANSWERS_OFF}"),
            };
        }
        _ if done && !cfg.continue_policy => return TurnEndAction::Nothing,
        None if !cfg.continue_policy => return TurnEndAction::Nothing,
        _ => {}
    }
    if let (Some(wait), Some(at)) = (state.short_wait(), state.point_at) {
        let due = later(at, wait);
        if now < due {
            let secs = wait.as_secs();
            return TurnEndAction::WaitUntil {
                until: due,
                why: if secs < 120 {
                    format!("{} short turn(s) in a row: {secs} s back-off", state.short)
                } else {
                    format!(
                        "{} short turn(s) in a row: {} min back-off",
                        state.short,
                        secs / 60
                    )
                },
            };
        }
    }
    if done {
        return budgeted(TurnEndAction::Type {
            text: done_check_text(reading.rules.as_deref()),
            rule_id: RULE_DONE_CHECK,
        });
    }
    if asked.is_some() {
        let rules = reading.rules.as_deref();
        // An irreversible act gets the reversible answer under `answer@v1`
        // whatever the priority; any other question, the priority's answer.
        return budgeted(if irreversible {
            TurnEndAction::Type {
                text: reversible_answer(rules),
                rule_id: RULE_ANSWER,
            }
        } else {
            TurnEndAction::Type {
                text: prose_answer(cfg, rules),
                rule_id: cfg.question_priority.prose_rule_id(),
            }
        });
    }
    match &reading.composer {
        Composer::Placeholder(s) if suggestion_allowed(s) => budgeted(TurnEndAction::Accept {
            text: s.clone(),
            rule_id: RULE_SUGGESTION,
        }),
        _ => budgeted(TurnEndAction::Type {
            text: continuation_text(cfg, reading.rules.as_deref()),
            rule_id: RULE_CONTINUE,
        }),
    }
}

/// A limit waited out: continue [`TurnEndTiming::reset_grace`] past the
/// notice's reset, or [`TurnEndTiming::limit_backoff`] after a continuation
/// that hit the wall again, whichever is later; with no reset, the longest
/// back-off from the appearance.
fn resume_at_reset(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    since: Instant,
    attempts: usize,
    kind: WallKind,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    // A wall no reset ends and none names is an entitlement: its own, slower
    // ladder. A usage window with no readable
    // reset still lifts by itself, so it keeps the short one.
    if reading.reset_at.is_none() && matches!(kind, WallKind::Spend | WallKind::ModelBucket { .. })
    {
        let due = crate::supervise::ladder::Ladder(&state.timing.entitlement_backoff)
            .at(attempts)
            .map(|w| later(since, w));
        return match due {
            Some(due) if now < due => TurnEndAction::WaitUntil {
                until: due,
                why: format!("{} (no reset named)", kind.name()),
            },
            Some(_) => continue_as(RULE_LIMIT_RESUME),
            None => TurnEndAction::Nothing,
        };
    }
    let back = &state.timing.limit_backoff;
    let backoff = |i: usize| crate::supervise::ladder::Ladder(back).at(i);
    let at_reset = reading
        .reset_at
        .map(|r| later(r, state.timing.grace_for(reading.program) + reading.jitter));
    let after_retry = if attempts == 0 {
        None
    } else {
        backoff(attempts - 1).map(|b| later(since, b))
    };
    let due = match (at_reset, after_retry) {
        (Some(r), Some(b)) => r.max(b),
        (Some(r), None) => r,
        (None, Some(b)) => b,
        // No reset the loop could read: the longest backoff.
        (None, None) => match back.last() {
            Some(&b) => later(since, b),
            None => return TurnEndAction::Nothing,
        },
    };
    if now < due {
        return TurnEndAction::WaitUntil {
            until: due,
            why: format!("{} resets", kind.name()),
        };
    }
    resume_limit(reading, continue_as)
}

/// THE WALL A LIMIT'S NOTICE LEFT ON THE SCREEN with no work since
/// ([`act_at_point`]): the point's reading with that wall back on it, as its
/// notice last read ([`TurnEndState::observe`] keeps the reset it named), for
/// the wall's own rule to decide. A usage window stands until the reset its
/// notice named, whatever the screen shows since. Measured on Claude Code
/// 2.1.295 (the sweep of 2026-10-09): Esc on `Continuing automatically at …
/// · esc to cancel` leaves `⏺ Automatic continue cancelled ·
/// /rate-limit-options to re-arm`, no wall, and the continuation typed at
/// once met the standing limit minutes before its reset, which put the real
/// resume a back-off past it. Decided as the wall, it waits for the reset and
/// the grace: the vendor said it will not go on by itself. Where the notice
/// left with no word of the vendor's, its countdown is taken at its word, and
/// [`TurnEndTiming::auto_resume_grace`] past the reset is the net under a
/// vendor that did not go on; where the vendor went on (`Usage limit reset ·
/// continuing automatically`), the limit is over and no track is left
/// ([`TurnEndState::observe`]). A LOST LOGIN NO POINT DECIDED is the wall
/// it left too ([`left_undecided_login`]): its row gone with the login not
/// back is no login, and its own rule has not had its say. `None`: the notice
/// named no reset, the track is a model bucket or a login a point decided,
/// or the screen says the login is back (`/login`, perhaps to another
/// account).
fn still_shown(t: &WallTrack, reading: &TurnEndReading) -> Option<(WallKind, TurnEndReading)> {
    if t.class == WallClass::Auth {
        return left_undecided_login(t, reading).then(|| {
            (
                WallKind::Auth,
                TurnEndReading {
                    wall: Some(WallKind::Auth),
                    wall_message: t.said.clone(),
                    ..reading.clone()
                },
            )
        });
    }
    let n = left_named(t, reading)?;
    Some((
        n.kind,
        TurnEndReading {
            wall: Some(n.kind),
            reset_at: Some(n.at),
            jitter: n.jitter,
            resumes_by_itself: n.by_itself,
            ..reading.clone()
        },
    ))
}

/// The reset a track's notice named, where a point its notice has left is
/// decided as if it still showed ([`still_shown`]): a usage window's, while
/// the screen does not say the login is back.
fn left_named(t: &WallTrack, reading: &TurnEndReading) -> Option<NamedReset> {
    t.named
        .filter(|_| t.class == WallClass::Usage && !reading.login_back)
}

/// A LOST LOGIN NO POINT DECIDED, its row gone from the screen
/// ([`WallTrack::pointed`]): read only where no point was yet — the answer to
/// an act inside its take window, under anyone's unanswered row — and the
/// login not back. Decided as the wall it left ([`still_shown`]); one a point
/// saw was decided there, and is held ([`act_at_point`]).
fn left_undecided_login(t: &WallTrack, reading: &TurnEndReading) -> bool {
    t.class == WallClass::Auth && !t.pointed && !reading.login_back
}

/// What a point decides of a lost login whose row has LEFT the screen
/// ([`still_shown`]), told with the wall's words `said`: the escalation that
/// goes with `/login` names them, since the point it is raised at no longer
/// shows them (a relaunch's fallback alike).
fn told_with_words(act: TurnEndAction, said: &str) -> TurnEndAction {
    match act {
        TurnEndAction::TypeCommand {
            command,
            rule_id,
            then: Then::Escalate(why),
        } if !said.is_empty() => TurnEndAction::TypeCommand {
            command,
            rule_id,
            then: Then::Escalate(format!("{said}; {why}")),
        },
        TurnEndAction::Restart {
            why,
            rule_id,
            otherwise,
        } => TurnEndAction::Restart {
            why,
            rule_id,
            otherwise: Box::new(told_with_words(*otherwise, said)),
        },
        act => act,
    }
}

/// THE LAST LINE AGAINST A HOT LOOP ([`WALL_ACTS_PER_WINDOW`]): a wall's act
/// past the cap waits for the oldest of its rule to leave the window, and
/// says so; anything else — an act under the cap, a wait, a restart (no
/// wall act) — is `act` as it is. Applied to what a wall's point decides
/// ([`act_at_point`]) and, ahead of that, to what an account move falls back
/// to ([`account_first`], [`wall_action`]): a restart is looked at by its own
/// rule, so its `otherwise` would pass the cap unseen.
fn wall_breaker(state: &TurnEndState, now: Instant, act: TurnEndAction) -> TurnEndAction {
    if let Some(rule) = act.rule_id().filter(|r| is_wall_act(r))
        && let (n, Some(free)) = state.wall_acts_of(rule, now)
        && n >= WALL_ACTS_PER_WINDOW
    {
        return TurnEndAction::WaitUntil {
            until: free,
            why: format!(
                "a breaker: {n} acts of {rule} in the last {} min",
                state.timing.window.as_secs() / 60
            ),
        };
    }
    act
}

/// THE CONTINUATION A LIMIT RESET LEFT OWED ([`TurnEndState::reset_told`]),
/// under `resume_limits`: typed by the limit-resume rule once it is due
/// ([`resume_limit`]: budgeted, and counted as a wall's act), and waited for
/// until then — the vendor queued its own, and a turn seen running before the
/// wait is out pays it ([`TurnEndState::took`]). `None` with nothing owed, or
/// with `resume_limits` off: the point is then decided as it was before
/// resets existed.
fn reset_continuation(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> Option<TurnEndAction> {
    let owed = state.reset_resume.filter(|_| cfg.resume_limits)?;
    Some(if now < owed.due {
        TurnEndAction::WaitUntil {
            until: owed.due,
            why: "the limit reset's own continuation".to_string(),
        }
    } else {
        resume_limit(reading, continue_as)
    })
}

/// The act at a limit's reset: a Codex goal the wall stopped (`Goal hit
/// usage limits (/goal resume)` on its footer) is resumed with `/goal
/// resume`, which goes on with the goal itself; anything else is continued.
fn resume_limit(
    reading: &TurnEndReading,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    if reading.program == Program::Codex && reading.goal == Some(CodexGoal::UsageLimited) {
        return TurnEndAction::TypeCommand {
            command: GOAL_RESUME.to_string(),
            rule_id: RULE_LIMIT_RESUME,
            then: Then::Nothing,
        };
    }
    continue_as(RULE_LIMIT_RESUME)
}

/// The retry ladder's wait before the attempt after `attempts`: its entry,
/// the last for every attempt past it.
fn retry_wait(state: &TurnEndState, attempts: usize) -> Option<Duration> {
    crate::supervise::ladder::Ladder(&state.timing.retry_backoff).at(attempts)
}

/// WHETHER A RELAUNCH UNDER ANOTHER ACCOUNT MAY BE ASKED HERE AT ALL (the
/// accounts design, §6 "Which walls rotate"): never under `[harness]
/// accounts = "off"`, `resume_limits = false` or `relaunch = false`, never
/// where no host relaunches the agent ([`TurnEndReading::restartable`] —
/// which [`decide_turn_end`] also forces off at a wall the live upgrade's
/// turn end covers), never for an agent that is not Claude Code, and not
/// again after a move that could not be made while its wall stands
/// ([`TurnEndState::account_refused`]). Nor over a DRAFT: the relaunch needs
/// an empty composer, and a restart decided there would send the person's
/// words into the wall at once under this rule's name, where today they
/// wait for the reset ([`decide_turn_end`]'s `Submit`).
fn account_rotates(state: &TurnEndState, reading: &TurnEndReading, cfg: &SupervisorConfig) -> bool {
    cfg.accounts != crate::harness::accounts::Mode::Off
        && cfg.resume_limits
        && cfg.relaunch
        && reading.restartable
        && reading.program == Program::Claude
        && !state.account_unmade
        && !reading.composer.drafted()
}

/// THE ACCOUNT MOVE AT A WALL, in front of what the wall's own rule answers
/// (`today`, which is the restart's `otherwise` — the point is exactly as it
/// was with no roster wherever the move is not made). `attempts`: the acts
/// the wall has had.
///
/// The move is asked where the wall's own answer is a WAIT — the reset, the
/// vendor's own auto-continue (a relaunch cancels it: the move is the better
/// answer than waiting for either) — or another restart (a bucket's
/// fallback: the account first, it keeps the model), or a retry at a wall
/// that has already answered one. Where the wall has had no act and its own
/// answer is to TYPE — its reset has come, a bucket's consent — that goes
/// first, as it does before a bucket's fallback: the wall may have lifted,
/// and the continuation is the probe that costs no relaunch.
///
/// [`AccountAdvice::Wait`]: the wall's own wait, cut short at the instant
/// another account becomes usable (the point is decided again then, and the
/// host advises anew) — never lengthened, and never cut to an instant already
/// past. Past [`ACCOUNT_MOVES_PER_WINDOW`] the wall is waited out, and said.
///
/// [`AccountAdvice::Resume`]: THE OWNER LIFTED THIS WALL. The wall's own
/// continuation is typed at once in the wait's place (`continue_as`, the
/// limit-resume rule: counted as the wall's attempt and by its breaker as
/// any is), ONCE A CLEARING — a second advice for a clearing already
/// answered ([`TurnEndState::acted`]) leaves the wall's own ladder as it is.
/// If the account answers with a limit again, its new row is an ordinary
/// wall to the roster, and the advice asked next is the move. (A clearing
/// whose instant moves with the clock — one stamped ahead of it, read as
/// made now at every read — is no key that can be held: the wall's breaker,
/// [`WALL_ACTS_PER_WINDOW`], is what bounds it.)
fn account_first(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    attempts: usize,
    today: TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    let Some(advice) = reading.advised() else {
        return today;
    };
    // What the point gets with no roster is the wall's answer PAST ITS
    // BREAKER ([`wall_breaker`], which [`act_at_point`] applies to whatever
    // this returns — but to a restart by the restart's own rule): wrapped as
    // it stood, a relaunch not made typed the wall's act past the cap.
    let today = wall_breaker(state, now, today);
    let waits = matches!(
        today,
        TurnEndAction::WaitUntil { .. } | TurnEndAction::Restart { .. }
    ) || (attempts > 0 && today.rule_id().is_some());
    if !waits || !account_rotates(state, reading, cfg) {
        return today;
    }
    match (advice, today) {
        (AccountAdvice::Move { to }, today) => {
            let (n, free) = state.account_moves_of(now);
            if n < ACCOUNT_MOVES_PER_WINDOW {
                return TurnEndAction::Restart {
                    why: Restart::Account {
                        to: to.clone(),
                        at: AccountAt::Wall,
                    },
                    rule_id: RULE_ACCOUNT,
                    otherwise: Box::new(today),
                };
            }
            match (today, free) {
                // The breaker is named only where it sets the wait: a reset
                // that comes first keeps the wall's own words.
                (TurnEndAction::WaitUntil { until, .. }, Some(free)) if free < until => {
                    TurnEndAction::WaitUntil {
                        until: free,
                        why: format!(
                            "a breaker: {n} account moves in the last {} min",
                            state.timing.window.as_secs() / 60
                        ),
                    }
                }
                (today, _) => today,
            }
        }
        (AccountAdvice::Wait { until }, TurnEndAction::WaitUntil { until: due, why })
            if *until > now =>
        {
            let at = (*until).max(later(now, ACCOUNT_WAIT_FLOOR));
            if at < due {
                TurnEndAction::WaitUntil {
                    until: at,
                    why: "another account can be used then".to_string(),
                }
            } else {
                TurnEndAction::WaitUntil { until: due, why }
            }
        }
        (AccountAdvice::Wait { .. }, today) => today,
        (AccountAdvice::Resume { cleared_at }, today) => {
            if state
                .account_resumed
                .is_some_and(|typed| typed >= *cleared_at)
            {
                today
            } else {
                resume_limit(reading, continue_as)
            }
        }
        // Never reached: `advised` reads through a probe. A wall is no
        // point a probe is made at; the wall's own answer stands.
        (AccountAdvice::Probe { .. }, today) => today,
    }
}

/// A LOST LOGIN RIGHT AFTER THIS SESSION'S OWN ACCOUNT MOVE is not the
/// person's (the accounts design, §6): the account the move chose is signed
/// out, the host has struck it ([`AccountBad::SignedOut`]) and advises the
/// next one, or the way back — the label this answers. Then no `/login` is
/// typed and nothing is escalated: the relaunch is the answer, and the
/// wall's own rule only its `otherwise`. `None` — the wall's own rule, as
/// ever — with no such advice, once the move is behind the session
/// ([`TurnEndState::account_move`]), once `/login` has been typed at this
/// wall, wherever no account move may be asked ([`account_rotates`]: a
/// draft in the composer included, which the login rule hands to its
/// person), and past [`ACCOUNT_MOVES_PER_WINDOW`].
pub(super) fn login_wall_moves<'a>(
    state: &TurnEndState,
    reading: &'a TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<&'a str> {
    let Some(AccountAdvice::Move { to }) = reading.advised() else {
        return None;
    };
    (reading.wall == Some(WallKind::Auth)
        && state.account.is_some()
        && state.wall_attempts(WallKind::Auth).unwrap_or(0) == 0
        && account_rotates(state, reading, cfg)
        && state.account_moves_of(now).0 < ACCOUNT_MOVES_PER_WINDOW)
        .then_some(to.as_str())
}

/// The wall policies (module header): each wall's own rule
/// ([`wall_own_act`]), and in front of it, where the session's host advises
/// one, the move to another account ([`account_first`], [`login_wall_moves`])
/// — at a session or weekly limit and a model bucket, and at a lost login
/// right after a move; never at a spend or credit wall, which is no
/// account's window. With no advice ([`TurnEndReading::account`] `None`) and
/// no move of the session's own behind it, this IS the wall's own rule.
#[allow(clippy::too_many_arguments)]
fn wall_action(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let today = wall_own_act(
        state,
        reading,
        cfg,
        now,
        kind,
        budgeted,
        continue_as,
        type_as,
    );
    match kind {
        // The notice of the account the session LEFT, repainted by the
        // conversation it resumed: the wall's resume is owed
        // ([`TurnEndState::account_resume_owed`]) and goes at once — that
        // notice's reset is the old account's, and waiting for it would
        // resume the move later than no roster would have.
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. }
            if state.account_resume && cfg.resume_limits =>
        {
            resume_limit(reading, continue_as)
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::ModelBucket { .. } => {
            // The notice a limit reset left on the screen: the reset's own
            // continuation is this wall's ([`TurnEndState::reset_told`]) —
            // after a reset that went through the notice is stale, and after
            // one kept or failed the continuation is the wall's one try
            // before it is moved from or waited out.
            if let Some(act) = reset_continuation(state, reading, cfg, now, continue_as) {
                return act;
            }
            let attempts = state.wall_attempts(kind).unwrap_or(0);
            account_first(state, reading, cfg, now, attempts, today, continue_as)
        }
        WallKind::Auth => match login_wall_moves(state, reading, cfg, now) {
            Some(to) => TurnEndAction::Restart {
                why: Restart::Account {
                    to: to.to_string(),
                    at: AccountAt::Wall,
                },
                rule_id: RULE_ACCOUNT,
                otherwise: Box::new(wall_breaker(state, now, today)),
            },
            None => today,
        },
        WallKind::Spend
        | WallKind::Overloaded
        | WallKind::ApiError { .. }
        | WallKind::Context
        | WallKind::Memory => today,
    }
}

/// Each wall's own rule (module header): what the point gets with no account
/// to move to.
#[allow(clippy::too_many_arguments)]
fn wall_own_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
    kind: WallKind,
    budgeted: &dyn Fn(TurnEndAction) -> TurnEndAction,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
    type_as: &dyn Fn(&'static str, &str) -> TurnEndAction,
) -> TurnEndAction {
    let track = state.wall.as_ref().filter(|t| t.class == class_of(kind));
    let since = track.map_or(now, |t| t.since);
    let attempts = track.map_or(0, |t| t.attempts);
    let esc = |reason: String| TurnEndAction::Escalate { reason };
    let msg = &reading.wall_message;
    let wait_out = || {
        if cfg.resume_limits {
            resume_at_reset(state, reading, now, since, attempts, kind, continue_as)
        } else {
            esc(format!("{}: {msg} (resume_limits is off)", kind.name()))
        }
    };
    match kind {
        WallKind::Overloaded | WallKind::ApiError { .. } => {
            if !cfg.retry_api_errors {
                return esc(format!("{}: {msg} (retry_api_errors is off)", kind.name()));
            }
            let cause = match kind {
                WallKind::ApiError { cause, .. } => cause,
                _ => ApiCause::Server,
            };
            let ups = track.map_or(0, |t| t.ups);
            let t = &state.timing;
            let act =
                |rule: &'static str| type_as(rule, &wall_retry_text(rule, kind, reading.program));
            let wait_then = |due: Instant, why: String, act: TurnEndAction| {
                if now < due {
                    TurnEndAction::WaitUntil { until: due, why }
                } else {
                    act
                }
            };
            // A ladder's rung for the act after `n`, from this appearance.
            let rung = |ladder: &[Duration], n: usize| {
                crate::supervise::ladder::Ladder(ladder)
                    .at(n)
                    .map_or(now, |w| later(since, w))
            };
            match (cause, reading.reach) {
                // Measured reachable again: continued at once, then spaced on
                // the short ladder by this measure's acts (meeting the wall
                // again means the measure was wrong or the API still fails).
                // Not for a certificate or proxy refusal: the host verifies
                // against the platform trust store and the agent against its
                // own (`NODE_EXTRA_CA_CERTS`), so the host can read Up while
                // the agent is refused. That stays on the ladder.
                (ApiCause::Unreachable, Reach::Up { .. }) => wait_then(
                    ups.checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error back but met again: retry {}", ups + 1),
                    act(RULE_API_BACK),
                ),
                // Measured definitely down: nothing typed into it for up to
                // the hold, then one try all the same.
                (
                    ApiCause::Unreachable | ApiCause::Config | ApiCause::CutOff,
                    Reach::Down { .. },
                ) => wait_then(
                    later(since, t.down_hold),
                    "api-error: the API is unreachable; continuing when it is reachable, or \
                         at this try"
                        .to_string(),
                    act(RULE_API_RETRY),
                ),
                (ApiCause::Unreachable, Reach::Unknown)
                | (ApiCause::Config, Reach::Unknown | Reach::Up { .. }) => wait_then(
                    rung(&t.net_backoff, attempts),
                    format!("api-error retry {}", attempts + 1),
                    act(RULE_API_RETRY),
                ),
                // A reply cut off: continued at once, then on the short
                // ladder.
                (ApiCause::CutOff, _) => wait_then(
                    attempts
                        .checked_sub(1)
                        .map_or(since, |n| rung(&t.net_backoff, n)),
                    format!("api-error cut off: retry {}", attempts + 1),
                    act(RULE_API_CUTOFF),
                ),
                // The server's own failure: its ladder, whatever the reach.
                (ApiCause::Server, _) => wait_then(
                    rung(&t.retry_backoff, attempts),
                    format!("{} retry {}", kind.name(), attempts + 1),
                    act(RULE_API_RETRY),
                ),
            }
        }
        WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => {
            if reading.resumes_by_itself && cfg.resume_limits {
                // The vendor resumes by itself: waited out with nobody told,
                // and continued only if it has not resumed
                // [`TurnEndTiming::auto_resume_grace`] past the named time
                // (with none, the longest limit back-off).
                let t = &state.timing;
                let due = match reading.reset_at {
                    Some(at) => later(at, t.auto_resume_grace),
                    None => later(since, t.limit_backoff.last().copied().unwrap_or_default()),
                };
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("{} goes on by itself", kind.name()),
                    };
                }
                return continue_as(RULE_LIMIT_RESUME);
            }
            if reading.resumes_by_itself {
                return TurnEndAction::Nothing;
            }
            if !cfg.resume_limits && kind != WallKind::Spend {
                // The limit episode tells the person (the loop's
                // `limit_after`).
                return TurnEndAction::Nothing;
            }
            wait_out()
        }
        // A bucket asking consent to use usage credits: accepted by
        // continuing (the vendor's confirm box is the approval policy's).
        // Hitting it again, or with continuation limited: switched or waited
        // out like any bucket.
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy {
                return continue_as(RULE_CONSENT);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
            // The bucket's reset has come and nothing was tried at it yet:
            // the wall may have lifted, and the continuation is the probe. A relaunch the host keeps
            // deferring (the agent's own background work runs) is no reason
            // to sit past a reset; if the wall answers, the fallback is
            // asked for again.
            if attempts == 0
                && reading.reset_at.is_some_and(|r| {
                    now >= later(r, state.timing.grace_for(reading.program) + reading.jitter)
                })
            {
                return wait_out();
            }
            if state.model.is_some()
                || state.fallback_unmade
                || !reading.restartable
                || bucket_model(msg).as_deref() == Some(fallback.as_str())
            {
                // Switched already, a relaunch that could not be made or no
                // host to make one, or the fallback is the model at its
                // limit: the reset is waited out.
                return wait_out();
            }
            TurnEndAction::Restart {
                why: Restart::Model {
                    to: fallback.clone(),
                },
                rule_id: RULE_MODEL_FALLBACK,
                otherwise: Box::new(wait_out()),
            }
        }
        WallKind::Context => {
            if !cfg.compact_on_context_wall {
                return esc(format!(
                    "context full: {msg} (compact_on_context_wall is off)"
                ));
            }
            if let Some(wait) = attempts.checked_sub(1).and_then(|n| retry_wait(state, n)) {
                let due = later(since, wait);
                if now < due {
                    return TurnEndAction::WaitUntil {
                        until: due,
                        why: format!("context still full after /compact ({attempts})"),
                    };
                }
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/compact".to_string(),
                rule_id: RULE_COMPACT,
                then: Then::Continue,
            })
        }
        // Decided ahead of every gate in `decide_turn_end`; the arm keeps
        // the match exhaustive with the same act.
        WallKind::Memory => memory_restart(cfg, reading),
        WallKind::Auth => {
            if attempts > 0 {
                return TurnEndAction::Nothing;
            }
            // An API failure the worker cannot retry past: typed wherever API
            // errors are retried (`retry_api_errors`, on by default).
            if !cfg.retry_api_errors {
                let how = sign_in_how(reading.program);
                return esc(format!(
                    "the login is gone: {msg} (retry_api_errors is off; {how})"
                ));
            }
            budgeted(TurnEndAction::TypeCommand {
                command: "/login".to_string(),
                rule_id: RULE_LOGIN,
                then: Then::Escalate("finish sign-in in the browser".to_string()),
            })
        }
    }
}

/// Codex's command that stops its goal from starting the next turn.
pub const GOAL_PAUSE: &str = "/goal pause";
/// Codex's command that goes on with a paused or limited goal.
pub const GOAL_RESUME: &str = "/goal resume";
/// Codex's command that ends its TUI: the upgrade's restart types it.
pub const CODEX_EXIT: &str = "/exit";
/// Codex's model picker, which the restore drives (the approval policy's
/// `model-restore-pick@v1`).
pub const MODEL_PICKER: &str = "/model";

/// Whether a Codex draft is exactly a line the upgrade's lane types whole
/// ([`CODEX_EXIT`], [`GOAL_PAUSE`], [`GOAL_RESUME`]), compared across the
/// composer's wrapping with whitespace and the input mark dropped: what the
/// lane's guarded Enter left when it missed. Sending it as a draft would end
/// Codex with no relaunch or leave a goal paused. A person's identical line is
/// also escalated, never sent; anything more (`/exit please`) is a draft.
fn codex_own_line(draft: &str) -> bool {
    let compact = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
    let draft = compact(draft);
    let draft = draft.trim_start_matches(aterm_phase::codex::CARETS);
    [CODEX_EXIT, GOAL_PAUSE, GOAL_RESUME]
        .iter()
        .any(|line| compact(line) == draft)
}

/// `word` with its first letter upper-cased (`ultra` → `Ultra`).
fn capitalized(word: &str) -> String {
    let mut cs = word.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

/// The save instruction, typed once on the cheaper model: commit the work in
/// progress on the current branch, push plainly (one `git pull --no-rebase`
/// on a rejected push), never force, rebase, reset, stash or switch branches,
/// report failures instead of working around them, then stop and answer with
/// the one-time marker line only once everything is pushed. The standing
/// rules ride with it (owner, 2026-09-28).
#[must_use]
pub fn wind_down_text(w: &WindDown, rules: Option<&str>) -> String {
    let text = format!(
        "[aterm harness] {from} is close to its usage limit, so aterm switched this session to \
         {to} for one job only: saving your work. Do not continue the task, do not start anything \
         new, and do not resume the goal. In each repository you changed: 1) commit your work in \
         progress on the current branch (stage only the files you changed; say in the message \
         that it is work in progress; skip this if nothing changed); 2) push it with a plain `git \
         push` (`git push -u origin HEAD` if the branch has no upstream). If the push is rejected \
         because the remote has new commits, run `git pull --no-rebase` once and push again; if \
         that needs any conflict resolution, stop instead. Never force-push, rebase, reset, stash \
         or switch branches. If a commit or a push fails for any reason (sandbox, permissions, \
         network, a hook, a conflict), do not work around it: say what failed. Then stop. Only if \
         everything is committed and pushed, reply with {marker} on a line by itself. aterm will \
         put this session back on {back} and tell you when to continue, once the limit resets.",
        from = w.from.model,
        to = w.to,
        marker = w.marker,
        back = w.from.words(),
    );
    with_rules(&text, rules)
}

/// What is typed at the reset when the harness did not pause Codex's goal:
/// the window has reset and the session is back on its own model; carry on.
#[must_use]
pub fn resume_text(w: &WindDown, rules: Option<&str>) -> String {
    with_rules(
        &format!(
            "[aterm harness] {model}'s usage limit has reset and this session is back on {back}. \
             {CARRY_ON}",
            model = w.from.model,
            back = w.from.words()
        ),
        rules,
    )
}

/// The save-then-wait switch's act at a free point (module header): `None`
/// with no switch open, else:
///
/// * nothing into a thread that fell into a sandbox;
/// * Codex's goal pursued while the switch holds the session: `/goal pause`,
///   one of the [`GOAL_STOPS`] ([`TurnEndState::goal_stop`]); stops spent,
///   nothing;
/// * owed: the save instruction once the footer shows the cheaper model
///   (a footer on the own model is the switch that did not land, closed by
///   [`TurnEndState::observe`]); never over a draft;
/// * winding: nothing, except the wall's rule for an API error or full
///   context (the bound's Esc is [`TurnEndState::goal_stop`]'s);
/// * restore owed: `/model` up to [`RESTORE_TRIES`] times (the approval
///   policy picks the original model and effort), then nothing; never over a
///   draft;
/// * holding: nothing until the reset plus [`TurnEndTiming::reset_grace`] (or
///   [`TurnEndTiming::unknown_hold`]), re-decided every
///   [`TurnEndTiming::limits_every`] and sooner when usage drops under its
///   limit; then `/goal resume` if the harness paused the goal (under
///   `resume_limits`), else [`resume_text`] (under `continue_policy`); with
///   either key off, escalated. Never over a draft.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "StopGoal",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "TypeWindDown",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "Restore",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "SupervisorCodexRateNudge",
        action = "Resume",
        project = "supervise_conformance_codex_rate_nudge::ph_of"
    )
)]
fn wind_down_act(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> Option<TurnEndAction> {
    let w = state.wind.as_ref()?;
    let t = &state.timing;
    let draft = reading.composer.drafted();
    let sandboxed = reading.sandbox_fell.is_some();
    let goal_runs = reading.goal == Some(CodexGoal::Pursuing);
    // Codex's goal stopped first, a stop of the switch's.
    let pause = || {
        if w.stops < GOAL_STOPS && !draft && !sandboxed {
            TurnEndAction::TypeCommand {
                command: GOAL_PAUSE.to_string(),
                rule_id: RULE_WIND_DOWN,
                then: Then::Nothing,
            }
        } else {
            TurnEndAction::Nothing
        }
    };
    Some(match w.phase {
        WindPhase::Owed => {
            let landed = reading.model_field.as_ref().is_some_and(|m| m.runs(&w.to));
            if sandboxed {
                TurnEndAction::Nothing
            } else if goal_runs && (landed || !w.intent) {
                // An intent whose footer lacks the cheaper model may be a key
                // that never went: the goal runs on the own model, untouched.
                pause()
            } else if !landed {
                TurnEndAction::WaitUntil {
                    until: later(now, t.take_within),
                    why: format!("the footer to show {} before the work is saved", w.to),
                }
            } else if draft {
                TurnEndAction::Nothing
            } else {
                TurnEndAction::Type {
                    text: wind_down_text(w, reading.rules.as_deref()),
                    rule_id: RULE_WIND_DOWN,
                }
            }
        }
        WindPhase::Winding => match reading.wall {
            Some(WallKind::ApiError { .. } | WallKind::Overloaded | WallKind::Context) => {
                return None;
            }
            _ => TurnEndAction::Nothing,
        },
        WindPhase::Restore { hold } => {
            // The harness's picker left less than a settle ago: a frame
            // between its boxes, never a point to type `/model` again at.
            let settling = w
                .picker_gone_at
                .filter(|_| w.restore_shown)
                .map(|gone| later(gone, PICKER_SETTLE))
                .filter(|&until| now < until);
            if sandboxed {
                TurnEndAction::Nothing
            } else if hold && goal_runs {
                pause()
            } else if w.restore_tries >= RESTORE_TRIES || draft {
                TurnEndAction::Nothing
            } else if let Some(until) = settling {
                TurnEndAction::WaitUntil {
                    until,
                    why: "the /model picker's next box, or its leaving".to_string(),
                }
            } else {
                TurnEndAction::TypeCommand {
                    command: MODEL_PICKER.to_string(),
                    rule_id: RULE_MODEL_RESTORE,
                    then: Then::Nothing,
                }
            }
        }
        WindPhase::Holding { since } => {
            if goal_runs {
                return Some(pause());
            }
            // The bound for a reset nobody read is the wall clock's five
            // hours: the sleep since the hold began is time on it.
            let due = match w.back_at {
                Some(b) => later(b, t.grace_for(reading.program)),
                None => later(since, t.unknown_hold.saturating_sub(state.hold_slept)),
            };
            let reset = matches!(reading.limits, LimitRead::Far { .. });
            if now < due && !reset {
                return Some(TurnEndAction::WaitUntil {
                    until: due.min(later(now, t.limits_every)),
                    why: format!(
                        "holding on {} until its usage limit resets{}",
                        w.from.words(),
                        if w.back_at.is_none() {
                            " (its reset unread: tried again after a bound)"
                        } else {
                            ""
                        }
                    ),
                });
            }
            // A goal paused under the switch, by the switch or by the live
            // upgrade's move: resumed with `/goal resume` at the reset, never
            // a carry-on.
            let goal_paused = w.goal_paused || reading.upgrade_goal;
            let by_hand = if goal_paused {
                "resume its goal with /goal resume"
            } else {
                "tell it to carry on"
            };
            if sandboxed || draft {
                TurnEndAction::Nothing
            } else if !cfg.resume_limits {
                TurnEndAction::Escalate {
                    reason: format!(
                        "{}'s usage limit has reset and the session is back on {}, but \
                         resume_limits is off: aterm types nothing — {by_hand}",
                        w.from.model,
                        w.from.words()
                    ),
                }
            } else if goal_paused {
                TurnEndAction::TypeCommand {
                    command: GOAL_RESUME.to_string(),
                    rule_id: RULE_LIMIT_RESUME,
                    then: Then::Nothing,
                }
            } else if !cfg.continue_policy {
                TurnEndAction::Escalate {
                    reason: format!(
                        "{}'s usage limit has reset and the session is back on {}, but \
                         continue is off: aterm types no carry-on — {by_hand}",
                        w.from.model,
                        w.from.words()
                    ),
                }
            } else {
                TurnEndAction::Type {
                    text: resume_text(w, reading.rules.as_deref()),
                    rule_id: RULE_LIMIT_RESUME,
                }
            }
        }
    })
}

/// Claude Code's critical-memory banner: nothing typed is read and no reset
/// ends it, so the remedy is a restart ([`TurnEndAction::Restart`]): the host
/// ends the agent at its idle point, relaunches it on its own conversation and
/// tells it to carry on. The decider returns this at any point, ahead of the
/// gates typed acts wait on; the loop asks only at a point (under a running
/// spinner the server's `wall:memory` menu row is the notice).
/// `[harness] relaunch = false`, or a loop no host restarts for (`drive
/// watch`), escalates with the remedy instead.
///
/// The remedy names the agent's own conversation ([`TurnEndReading::resume`],
/// `claude --resume <id>`) or no command, and quotes only the banner's fact
/// ([`aterm_phase::memory_banner_head`]); never `claude --continue`, which
/// resumes the directory's newest conversation.
///
/// The loop also sets this wall at an idle point when the host measures the
/// agent past `[harness] memory_ceiling` while the machine is under memory
/// pressure and this agent holds the most (`supervise/memory.rs`,
/// `MemoryOver::acts`). Past the ceiling alone is only a warning.
fn memory_restart(cfg: &SupervisorConfig, reading: &TurnEndReading) -> TurnEndAction {
    let banner = aterm_phase::memory_banner_head(&reading.wall_message);
    let reason = match reading.resume.as_deref() {
        // The clause the escalation keeps whole or drops, never cuts
        // ([`crate::harness::resume::never_cut`]).
        Some(resume) => format!(
            "memory critical: restart it{}{resume}: {banner}",
            crate::harness::resume::THEN_RESUME_WITH
        ),
        None => format!("memory critical: restart it: {banner}"),
    };
    if !cfg.relaunch {
        return TurnEndAction::Escalate {
            reason: format!("{reason} (relaunch is off)"),
        };
    }
    if !reading.restartable {
        return TurnEndAction::Escalate { reason };
    }
    TurnEndAction::Restart {
        why: Restart::Memory,
        rule_id: RULE_MEMORY_RESTART,
        otherwise: Box::new(TurnEndAction::Escalate { reason }),
    }
}

/// The world and mutants of `SupervisorCodexRateNudge`
/// (`aterm_spec::derive::supervisor_codex_rate_nudge_model`): the supervisor's
/// actions bind to the real deciders (`approval::rate_nudge`,
/// [`TurnEndState::open_switch`], [`TurnEndState::goal_stop`],
/// [`TurnEndState::goal_told`], [`TurnEndState::observe`], `wind_down_act`,
/// `continue_policy`); these are Codex's, the clock's and a person's steps for
/// the Tier-1 bind (`tests/supervise_conformance_codex_rate_nudge.rs`), plus
/// the `Buggy = 1` members. Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "GoalTurn",
        reason = "Codex starting its goal's next turn by itself: the vendor's step. The Tier-1 bind reads it busy under the footer's goal exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "GoalEnds",
        reason = "Codex's goal achieved or cleared: the vendor's step. The Tier-1 bind drops the footer's goal exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "GoalEscapes",
        reason = "A goal the harness paused resuming by itself: the vendor's step. The Tier-1 bind puts the footer's `Pursuing goal` back exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "Pend",
        reason = "Codex deciding to show its rate-limit nudge at the turn's end: the vendor's step. The Tier-1 bind shows the box at the next end exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "WindDownRunsLong",
        reason = "The save's own turn running past `WIND_DOWN_BOUND`: the cheaper model's step and the clock. The Tier-1 bind advances the walk's clock past the bound exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "Far",
        reason = "Codex's usage reading dropping back under its limit: the vendor's records. The Tier-1 bind reads the window at 1% exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "NearAgain",
        reason = "Codex's usage reading rising to its limit: the vendor's records. The Tier-1 bind reads the window at 99% exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "SandboxFalls",
        reason = "A thread falling into a sandbox: the vendor's rollout. The Tier-1 bind reads the fall off the rollouts exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "PersonTurn",
        reason = "A person's message starting a turn: a person's step. The Tier-1 bind stamps the keystroke on the walk's clock exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "PersonResumes",
        reason = "A person's `/goal resume` during the hold: a person's step. The Tier-1 bind stamps the keystroke and the goal's turn exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "ContinueWhileSwitched",
        reason = "Buggy=1 negative control only; a continuation typed while the switch is open (the day's `keep going` 8 s after the press), which `wind_down_act` owns every point against"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "SwitchKeepsGoal",
        reason = "Buggy=1 negative control only; the switch pressed and Codex's goal left running on the cheaper model, which `goal_stop` stops"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "NudgeRewinds",
        reason = "Buggy=1 negative control only; a stale nudge at the wind-down's end taken for a new switch, which `rate_nudge` keeps while one is open"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "HoldBeforeRestore",
        reason = "Buggy=1 negative control only; held on the cheaper model and restored only at the reset, where `wind_down_act` restores first"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "ResumeOnCheap",
        reason = "Buggy=1 negative control only; carried on on the cheaper model at the reset, which the restore phase never does"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "ForgetOnRestart",
        reason = "Buggy=1 negative control only; a loop restart that reads no switch back, where the ledger rows are read back (`open_wind_down`)"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "PressAtFar",
        reason = "Buggy=1 negative control only; the switch pressed at a stale nudge far from the limit (the 03:30Z press), which `rate_nudge` keeps"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "ContinueInSandbox",
        reason = "Buggy=1 negative control only; a continuation typed into a thread fallen into a sandbox, which nothing types into"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "WindDownInSandbox",
        reason = "Buggy=1 negative control only; the save instruction typed into a sandboxed thread, which `wind_down_act` never types"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "ContinueUnderGoal",
        reason = "Buggy=1 negative control only; a continuation typed while Codex's goal runs its own turns, which the turn-end policy never types"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "StopPersonsTurn",
        reason = "Buggy=1 negative control only; the busy read's Esc into a person's turn once their grace ran out, which `goal_stop` spares"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "CoveredHandSparesGoal",
        reason = "Buggy=1 negative control only; the round-3 regression, a person's keystroke in the turn the nudge covered read as their hand in the goal turn under it, which `goal_stop` stops"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "SupervisorCodexRateNudge",
        action = "WindDownRunsOn",
        reason = "Buggy=1 negative control only; the save's own turn left running past its bound, which `goal_stop` stops with one Esc"
    )
)]
#[doc(hidden)]
pub fn codex_rate_nudge_environment() {}

#[cfg(test)]
#[path = "turn_end_tests.rs"]
mod tests;
