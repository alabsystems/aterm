// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TURN-END DECIDER: what the supervisor does when a worker's turn has
//! ended — nothing, wait, type a continuation, accept the worker's own
//! suggestion, type a slash command, or hand the point to a person. One pure
//! function, [`decide_turn_end`], over what the screen says
//! ([`TurnEndReading`], from `aterm-phase`'s reader) and what this policy
//! remembers of the session ([`TurnEndState`]); the loop only executes what
//! it returns (`supervise/turn_end_loop.rs`) and tells the state what it did
//! ([`TurnEndState::acted`]) and what the next point showed
//! ([`TurnEndState::observe`]). Every switch is the owner's `[harness]` table
//! ([`SupervisorConfig`]).
//!
//! **Fully automatic** (owner, 2026-09-24: *"UNLESS aterm is configured
//! otherwise, it is in FULLY AUTOMATIC mode and there cannot be any
//! interruptions because there is nobody to address the interruption"*).
//! Every point where a turn ended gets an automatic answer, and a person is
//! told only of what the owner's `[harness]` limited, or what cannot be
//! answered at all:
//!
//! * **Continue** (owner decision 2 — the harness audit's first blocker: 60%
//!   of the owner's typed prompts were a rote `keep going`). At an
//!   authoritative idle point, or a question that is an offer (`want me
//!   to…`), with no box, no wall and nothing typed in the composer: accept
//!   the worker's own suggestion when it is a continuation
//!   ([`suggestion_allowed`]), else type `cfg.continue_text` (the standing
//!   rules with it when a rules file is set).
//! * **Answer** ([`RULE_ANSWER`]). A worker that asks a person — a stop phrase
//!   ([`STOP_PHRASES`]), a choice between listed options, any other question
//!   — gets `cfg.answer_text` (decide yourself, prefer reversible steps, keep
//!   going). One that names an irreversible act — an offer to do something
//!   destructive ([`DESTRUCTIVE_WORDS`]), a question or a choice naming one
//!   anywhere ([`Said::Irreversible`]) — gets [`REVERSIBLE_ANSWER`] instead,
//!   whatever `answer_text` says: nobody is there to approve the act, and "take
//!   the option you would recommend" may be it (D1). Under `answer_questions =
//!   false` either is escalated instead.
//! * **A growing back-off, never "done"**. A turn that did less than
//!   [`TurnEndTiming::min_work`] of busy work is SHORT, and so is the first
//!   point a supervisor sees (its work unknown) — and so is the yield of a
//!   continuation or an answer that ends with the worker saying it is DONE
//!   ([`says_done`]: `nothing left to do`, `all done`, `what would you like
//!   me to work on?`), however long it worked: a worker that answers every
//!   `keep going` by re-running its checks for three minutes was continued
//!   at once, for ever (the hazards review of 2026-09-25). Each short turn
//!   in a row doubles the wait before the next act, from
//!   [`TurnEndTiming::short_backoff`] up to [`TurnEndTiming::
//!   short_backoff_max`] (2 min, 4, 8 … 60); a turn of real work that does
//!   not end done ends the streak, and the next point is answered at once.
//!   A turn the HARNESS typed — the upgrade's notice, answered READY, and a
//!   relaunched agent's carry-on, answered — is no short turn: answered
//!   short, the streak and its back-off stand as the worker's own turns left
//!   them ([`TurnEndState::host_typed`]; N1 of the live E2E of 2026-09-26:
//!   after a 2m34s stage, the READY answer and the carry-on's reply counted
//!   as two short turns and the stage's continuation waited a 4-minute
//!   back-off); answered with real work — a carry-on's answer is the
//!   worker's own work, resumed — it ends the streak as any turn of real
//!   work does. (Until 2026-09-24 a short turn was left alone and two in a
//!   row escalated "worker reports done".) `cfg.continue_per_hour` is the
//!   owner's cap: `0`, the default, is none; past a written cap the point is
//!   escalated.
//! * **A worker that says it is done is asked once, then left**
//!   ([`RULE_DONE_CHECK`]; *decided 2026-09-27 under the owner's standing
//!   direction*). A point whose last words say DONE ([`says_done`]) gets,
//!   past its back-off, the [`DONE_CHECK`] instead of the continuation or an
//!   answer: *"If the task is finished and verified, reply DONE and stop;
//!   otherwise continue"*. Its yield saying done again ENDS THE TASK: no act
//!   of any kind is typed into the session — the taskless rule's no-act path
//!   ([`TurnEndState::task_done`]) — until a person or an orchestrator types
//!   (a turn of someone else's opens it again, whatever it ended on: one that
//!   ended on a wall gets the wall's ladder). No limit key: the cost stops
//!   by itself. Until then a done worker was nudged on the back-off for
//!   good, and on a small model the nudges ended in invented work (the E2E's
//!   C of 2026-09-26).
//! * **No task, no turn end.** A session nobody has asked anything
//!   ([`TurnEndReading::taskless`]: the launch card and no message, or its
//!   host's word that no person or orchestrator has asked its conversation
//!   anything — the harness's OWN typed turns, a notice or a carry-on, are no
//!   task) is waiting for its first task, not stopped short: no act of any
//!   kind is typed into it — no continuation, no answer, no wall's retry —
//!   and its first point starts no streak (the E2E probe of 2026-09-25: a
//!   brand-new session got `keep going` two minutes after its launch; the
//!   live E2E of 2026-09-26, B 1a5299ab: after the harness's own notice,
//!   READY, restart and carry-on, a session nobody had asked anything got
//!   `keep going`, and its agent asked what it should help with). A session
//!   WITH a task is continued as below even when that task is finished —
//!   the same E2E's C, whose tester's prompt was answered, was continued,
//!   answered and carried on until haiku invented work (`git init`, `/init`,
//!   files and a commit): the done check below closes it.
//! * **A person wins.** Nothing is typed within `cfg.human_grace_s` of a
//!   person's last keystroke into the session ([`TurnEndReading::person`]:
//!   the server's `human_ms=`, or the draft in the composer last changing,
//!   whichever is later), and a turn a person stopped with Esc (`⎿
//!   Interrupted · What should Claude do instead?`) is continued only once
//!   that grace has passed since the point showed.
//! * **A draft left standing is sent** ([`TurnEndAction::Submit`]). Text in
//!   the composer that has not changed for the grace, with nobody typing,
//!   is the next message: where the policy would type its act, it submits
//!   the draft instead, under that act's rule — never types over it, and
//!   never leaves a fully automatic session stopped on it with nobody told
//!   (until 2026-09-24 it was left for ever).
//! * **An act the worker never takes is acted again.** A text act whose
//!   `❯` row stays unanswered, no spinner drawn, for
//!   [`TurnEndTiming::take_within`] is the short yield it is — the next act
//!   goes on the back-off ([`TurnEndState::observe`]); it was escalated as
//!   "not taken" until 2026-09-24.
//!
//! **Walls** ([`aterm_phase::wall`]). An API error is answered by what it
//! says went wrong ([`aterm_phase::ApiCause`]; the outage of 2026-09-27,
//! when a worker's `API Error: Can't reach the API server … (ENOTFOUND)`
//! was typed into sixteen times in an hour, is why), and never by an ask —
//! each wait is in the journal as a `WAITING` row (the tab names what went
//! wrong, not the wait: nothing carries the plan to the window):
//! * **the network never reached** — the worker is continued as soon as
//!   the session's host MEASURES the API reachable again ([`Reach::Up`]:
//!   decided at once, and seen by the loop within about a minute of the
//!   network's return — the host's probe cadence and the loop's wait
//!   step); until then it is tried on [`TurnEndTiming::net_backoff`] (1, 2,
//!   5, then every 5 min), a blind try costing only the vendor's own
//!   retries — and while the host measures the API definitely DOWN
//!   ([`Reach::Down`]), nothing is typed into it for up to
//!   [`TurnEndTiming::down_hold`] (15 min), so a wrong measure holds no one
//!   longer;
//! * **a certificate or proxy refused** — the same, except that an `Up`
//!   is no evidence for it (the host verifies against the platform's trust
//!   store, the agent against its own): it stays on the ladder;
//! * **a reply cut off** — continued at once, then on the same ladder;
//! * **the server's own failure** (a status, an overload) — wait out
//!   [`TurnEndTiming::retry_backoff`] (1, 5, 15, 30, 60 min; the last for
//!   every retry after) from each appearance and continue after each.
//!
//! Each act quotes the vendor's own line to the worker ([`wall_retry_text`])
//! rather than the rote continuation, and a retry that led to real work
//! ([`TurnEndTiming::progress`]) ends the episode: the next wall starts its
//! ladder again. A usage window or a spend limit: waited out, handled and
//! nobody told, when the vendor says it goes on by itself (`continuing
//! automatically at …`) — continued only if it has not gone on a
//! [`TurnEndTiming::reset_grace`] past its reset — and otherwise a usage
//! window or a spend limit is continued a [`TurnEndTiming::reset_grace`]
//! past the reset it names — never anything bought — and after a
//! continuation that hit the wall again, [`TurnEndTiming::limit_backoff`]. A
//! model bucket (owner decision 3): the agent RELAUNCHED on the fallback
//! model ([`Restart::Model`], `--model <fallback>` on the relaunch line,
//! session-only), and relaunched on the bucket's model again at the
//! bucket's reset ([`Restart::ModelBack`]) — never Claude's own `/model`,
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
//! ladder). A lost login — `Not logged in · …` under the gutter, or Claude
//! Code 2.1.281's `⏺ Login expired · Please run /login` error row (the
//! incident of 2026-09-27, read idle with no wall until then and continued
//! for nine hours): `/login` ONCE, then the ONE irreducible escalation —
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
//!    [`TurnEndTiming::unknown_hold`] from the hold's start when nobody read
//!    it, a start a restart carries), decided again at least every
//!    [`TurnEndTiming::limits_every`] and ended early when Codex's usage
//!    reading drops under its limit — a reading a held session no longer
//!    writes, so an early reset is seen only when ANOTHER Codex session on
//!    the account writes one (nothing is typed to ask, `/status` included);
//!    then, under `resume_limits`, `/goal resume` when the harness paused
//!    the goal, else — under `continue_policy` — a carry-on naming the model
//!    ([`resume_text`]); with either key off, nothing is typed and a person
//!    is told.
//!
//! A person's own message, `/model` or `/goal resume` releases the switch
//! (no automatic resume after it; before the hold, the model is still put
//! back) — during the hold, their `/model` to the cheaper model too (their
//! keystroke since the hold began), which is never driven back — and the
//! turn it starts is not stopped once a busy read has seen their hand in it
//! (a keystroke since the switch opened, within the turn's own work plus
//! [`PERSON_TURN_SLACK`]). What that does NOT cover, stated: a message a
//! person sent before the switch that Codex queued and starts under the
//! nudge's box is taken as Codex's own and stopped; the turn in flight when
//! a restarted loop carried the switch on is theirs only with a keystroke
//! within [`SEEDED_TURN_BOUND`] of the loop's first read of it; and a turn
//! whose first busy read came more than the slack after their last
//! keystroke is stopped once their grace has run out. A turn nobody here
//! typed is a person's only with their hand in it
//! ([`TurnEndReading::person_in_turn`]) wherever it could be Codex's goal or
//! the harness's own: a goal seen running before the model is back or during
//! the hold, and the turn in flight when a restarted loop carried the switch
//! on (a keystroke since its last row, within [`SEEDED_TURN_BOUND`] of the
//! loop's first read of it). A turn's end under a Codex BACKGROUND TERMINAL
//! (`1 background terminal running` under an ended turn, which reads busy) —
//! however it ended: idle, on a question, on a wall — is a point for the
//! switch's own steps and nothing else (the loop's), and a switch that
//! stands still through one for ten minutes is said once, naming what holds
//! it — a person's typing or draft, the terminal, or this policy's own wait
//! ([`TurnEndState::switch_hold`]) — and only said. Every edge and every note is a
//! ledger row that a later loop carries on from ([`TurnEndState::seed_wind`];
//! a wind-down in flight stays WINDING, so its end still judges the marker;
//! the switch's opening and its press, the harness's last Esc while its
//! point is fresh, the notes said and a press not yet seen landing — every
//! row of it written as `intent` — ride along). Nothing of the switch — no
//! save, no `/goal pause`, no `/model`, no resume — is typed into a thread
//! that fell into a sandbox its launch bypassed. And beside the switch: a
//! Codex whose footer says its goal is pursued is never continued — Codex
//! starts its own next turn within milliseconds, and a continuation lands in
//! it as a steer (all eleven of the incident's did); a Codex thread that fell
//! into a sandbox is typed nothing; a goal Codex's hard usage wall stopped is
//! resumed at the reset with `/goal resume`.
//!
//! **Never** while a box, the session survey or a wall's wait is up, on a
//! non-authoritative reading (a shell, a Codex screen outside its box), or
//! within a person's grace — a draft still changing is a person typing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_phase::codex::CodexGoal;
use aterm_phase::phase::{Phase, is_done_row, last_said_row, status_row};
use aterm_phase::wall::WallKind;
use aterm_phase::{ApiCause, Program};

use super::super::codex_usage::LimitRead;
use super::super::config::SupervisorConfig;

/// A continuation typed at an ordinary turn end ([`decide_turn_end`]).
pub const RULE_CONTINUE: &str = "continue@v1";
/// The [`DONE_CHECK`] typed where the worker said it is done: its yield
/// saying done again ends the task ([`TurnEndState::task_done`]).
pub const RULE_DONE_CHECK: &str = "continue-done-check@v1";
/// What a worker that says it is DONE ([`says_done`]) is typed in place of
/// the continuation: words that let it stop (decided 2026-09-27 under the
/// owner's standing direction). The reply it asks for, `DONE`, is itself a
/// done report ([`says_done`]).
pub const DONE_CHECK: &str =
    "If the task is finished and verified, reply DONE and stop; otherwise continue.";
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

/// What is typed at an API wall's act: the vendor's own line, quoted — so
/// the words cannot say more than the screen did — and what follows from
/// it. `rule` is the act's ([`RULE_API_BACK`], [`RULE_API_CUTOFF`],
/// [`RULE_API_RETRY`]); `cause` the wall's.
#[must_use]
pub fn wall_retry_text(rule: &str, cause: ApiCause, message: &str) -> String {
    let said = message.trim();
    match (rule, cause) {
        (RULE_API_BACK, _) => {
            format!(
                "Claude Code reported \"{said}\", and the API is reachable again now. {CARRY_ON}"
            )
        }
        (_, ApiCause::CutOff) => format!(
            "Claude Code reported \"{said}\", so your last reply may be incomplete. Carry on \
             from where it stopped; if the result of your last step is missing, check whether it \
             ran before you repeat it."
        ),
        (_, ApiCause::Unreachable | ApiCause::Config) => {
            format!("Claude Code reported \"{said}\"; trying again. {CARRY_ON}")
        }
        (_, ApiCause::Server) => format!("Claude Code reported \"{said}\". {CARRY_ON}"),
    }
}

/// What the session's host last MEASURED of the agent's route to its API
/// ([`crate::supervise::IdleHost::reach`]) — never read off the screen.
/// `since` is when the measure last changed to what it says.
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

/// A wall's own continuation: its next appearance is the act's to count.
fn is_wall_retry(rule: &str) -> bool {
    matches!(
        rule,
        RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_LIMIT_RESUME
    )
}
/// A continuation past a usage window's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// The agent relaunched on the fallback model on a model bucket.
pub const RULE_MODEL_FALLBACK: &str = "model-fallback@v1";
/// The agent relaunched on the bucket's model again at the bucket's reset.
pub const RULE_MODEL_RESTORE: &str = "model-restore@v1";
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
/// A turn the session's HOST typed — the live upgrade's notice, a relaunched
/// agent's carry-on ([`TurnEndState::host_typed`]): awaited as this policy's
/// own acts are, and its answer, short, is no short turn of the worker's.
pub const RULE_HOST_TURN: &str = "harness-turn@v1";

/// Why the session's host restarts the agent in its tab
/// ([`TurnEndAction::Restart`]): a point nothing typed can answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// Claude Code's critical-memory banner (D3): nothing typed into the
    /// process is read, and no reset ends it.
    Memory,
    /// A model bucket's limit (D7): relaunched on `to`, the owner's
    /// `model_fallback` — `--model` on the relaunch line, which is
    /// session-only, where Claude's own `/model` also rewrites the person's
    /// default for every new session (2.1.282).
    Model { to: String },
    /// The bucket's reset (D7): relaunched on `to`, the bucket's model
    /// again — or, `None` (the notice named no model `/model` knows), on
    /// the model the launch itself named, else Claude's default.
    ModelBack { to: Option<String> },
}

impl Restart {
    /// The one word the journal names it by (`restart:<word>`).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Restart::Memory => "memory",
            Restart::Model { .. } => "model",
            Restart::ModelBack { .. } => "model-back",
        }
    }
}

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
    // A report of finished work that asks only what comes next (the live
    // session of 2026-09-25: `Done. I've created hello.txt … What's next?`).
    "what's next?",
    "anything else?",
];

/// Whether the worker's last words say it is done ([`DONE_PHRASES`]), or
/// ARE the one word [`DONE_CHECK`] asks for — `DONE` as written there,
/// punctuation around it (`DONE.`, `**DONE**`) and nothing else. A
/// title-case `Done.` stays what it always read as: a step reported.
#[must_use]
pub fn says_done(tail: Option<&str>) -> bool {
    tail.is_some_and(|t| {
        if t.trim_matches(|c: char| !c.is_alphanumeric()) == "DONE" {
            return true;
        }
        let flat = t
            .to_lowercase()
            .replace('’', "'")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        DONE_PHRASES.iter().any(|p| flat.contains(p))
    })
}

/// The rules whose act's yield counts toward the short streak — never
/// [`RULE_HOST_TURN`], the harness's own turn.
fn is_continue_rule(rule: &str) -> bool {
    rule == RULE_CONTINUE
        || rule == RULE_SUGGESTION
        || rule == RULE_ANSWER
        || rule == RULE_DONE_CHECK
}

/// Claude Code's own `/model` aliases (2.1.280's binary: `["sonnet", "opus",
/// "haiku", "fable"]`): the only names a switch back is typed with.
pub const MODEL_ALIASES: &[&str] = &["sonnet", "opus", "haiku", "fable"];

/// The pieces a suggestion may be made of to be accepted as a continuation
/// (the owner's own accepted suggestions, measured by the audit's census:
/// `keep going`, `keep fixing forward`, `push it when green`, `keep going
/// push it when green`), joined by `,`, `;`, `.`, ` and ` or ` then `.
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

/// Words a worker asks a person for a decision with: the point is answered
/// ([`RULE_ANSWER`]), never continued — with [`REVERSIBLE_ANSWER`] when the
/// words name a destructive act ([`DESTRUCTIVE_WORDS`]) anywhere. Matched
/// lowercased, anywhere in the worker's last words.
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
    // "Your call" names the person's authority, not a list of options: it
    // stays a stop (the critique of 2026-09-25, O8). The choice asks are
    // [`CHOICE_PHRASES`].
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
    // Lane B2's review: the go-ahead, sign-off and review asks, and the
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

/// Words a worker asks a person to CHOOSE with: a stop — answered, as every
/// ask is ([`RULE_ANSWER`]) — only where the phrase ASKS ([`phrase_asks`]),
/// never in a report (`documented which option each flag maps to. All tests
/// pass.`), and a stop naming the act whenever the words after it name a
/// destructive one. Matched as [`STOP_PHRASES`] are (the critique of
/// 2026-09-25).
pub const CHOICE_PHRASES: &[&str] = &[
    "which option",
    "which would you prefer",
    "which do you prefer",
    "let me know which",
];

/// Words that make an OFFER one a person must answer: an offer, a stop, a
/// choice or a question naming one of these is answered with
/// [`REVERSIBLE_ANSWER`], never continued — `keep going` would read as
/// consent (lane B2's review: `Want me to force-push this to main?`).
/// Matched as whole words in what follows the offer's phrase. A plain
/// `push` is not among them: `push it when green` is the owner's own
/// continuation (the audit's census).
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

/// Words that make a question an OFFER to go on (it counts as a
/// continuation): `want me to take that on?`.
pub const OFFER_PHRASES: &[&str] = &[
    "want me to",
    "shall i",
    "should i continue",
    "should i proceed",
    "should i keep going",
    "should i go ahead",
    // An offer of MORE work — the live probe's `Created a.txt. Should I also
    // create b.txt?` (2026-09-24) — not a plain yes/no about the work done.
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
    /// SHORT, and backs the next act off ([`Self::short_backoff`]).
    pub min_work: Duration,
    /// The wait, from the point, before the act after ONE short turn; each
    /// short turn more in a row doubles it, up to [`Self::short_backoff_max`].
    pub short_backoff: Duration,
    pub short_backoff_max: Duration,
    /// The wait before each retry of the server's own failure (a status,
    /// an overload), counted from the wall's appearance; the last is the
    /// wait before every retry after it.
    pub retry_backoff: Vec<Duration>,
    /// The same for an API that was never reached or refused a certificate
    /// or proxy, and a reply cut off, where the reach is not measured up:
    /// short, because a blind try costs only the vendor's own retries (about
    /// 3 minutes of them in the outage of 2026-09-27) and a long rung is an
    /// agent left waiting after the network came back.
    pub net_backoff: Vec<Duration>,
    /// How long nothing is typed into an API the host measures definitely
    /// down before one try all the same — the most a wrong measure holds a
    /// worker.
    pub down_hold: Duration,
    /// The busy work between a wall's act and the wall's next appearance
    /// that ends its episode: real work, where the vendor's own retries run
    /// about 3 minutes. A reply the Mac's sleep cut off after an hour of
    /// work is a new wall, not the seventh retry of the last.
    pub progress: Duration,
    /// How long past a usage window's reset the continuation waits.
    pub reset_grace: Duration,
    /// How long past the time a notice that goes on BY ITSELF named
    /// (`continuing automatically at 1:50pm`) the vendor is given before the
    /// policy continues the worker itself: the vendor's own retry is waited
    /// for, never doubled — this is the net under it.
    pub auto_resume_grace: Duration,
    /// After a continuation at a usage wall that hit the wall again: the
    /// next is this far off (the last entry for every one after).
    pub limit_backoff: Vec<Duration>,
    /// The window the typing budget counts over.
    pub window: Duration,
    /// How long an act's point may show with no read of the worker busy
    /// before it is judged anyway ([`TurnEndState::observe`]): a reply that
    /// finished inside the `turn` verb's own settle, or an Enter that did
    /// not take, is never seen busy — and was a silent latch until lane B2's
    /// review.
    pub take_within: Duration,
    /// The pauses before consecutive tries of a restart the host could not
    /// make yet ([`TurnEndAction::Restart`]: not idle yet, a person's hand, a
    /// job the relaunch cannot see yet); the last for every try after it —
    /// never given up on.
    pub restart_backoff: Vec<Duration>,
    /// How long a save-then-wait hold whose reset nobody read lasts before
    /// the session is resumed on its own model all the same ([`WindDown`]):
    /// a model still at its limit then meets Codex's own wall, which names
    /// its reset.
    pub unknown_hold: Duration,
    /// The longest a hold waits before it is decided again — the usage
    /// reading looked at anew each time.
    pub limits_every: Duration,
    /// How long after the harness typed its `/model` the picker is answered
    /// as its own restore ([`TurnEndState::restore_target`]).
    pub restore_pick: Duration,
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
            reset_grace: Duration::from_secs(60),
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
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
}

/// What one screen says, as the turn-end policy reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndReading {
    pub phase: Phase,
    /// The reader's phase is evidence ([`aterm_phase::Reading::phase_authoritative`]).
    pub authoritative: bool,
    pub survey: bool,
    /// The wall the turn ended on.
    pub wall: Option<WallKind>,
    /// The wall's notice as said (for the model a bucket names).
    pub wall_message: String,
    /// The wall's reset as the loop's clock (the loop resolves the notice's
    /// `reset=` text with its zone and clock).
    pub reset_at: Option<Instant>,
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
    /// The last thing on the screen is a user's `❯` row the worker has not
    /// answered yet — a continuation just submitted, before its spinner is
    /// drawn: no turn has ended here.
    pub pending_input: bool,
    /// A person stopped the turn with Esc ([`aterm_phase::interrupted`]):
    /// nothing is typed here — the next message is theirs.
    pub interrupted: bool,
    /// A host restarts the agent in its tab here
    /// ([`crate::supervise::IdleHost::can_restart`]): a restart the policy
    /// asks for can be made — the loop's to set, as [`Self::upgrading`] is.
    pub restartable: bool,
    /// The line a person runs to resume the agent's OWN conversation
    /// ([`crate::supervise::IdleHost::resume_command`]: `claude --resume
    /// <id>`), named by the memory wall's escalation; `None` where no host
    /// read one — then the escalation names no command rather than `claude
    /// --continue`, which resumes the directory's newest conversation
    /// (2026-09-26). The loop's to set, as [`Self::restartable`] is.
    pub resume: Option<String>,
    /// The live upgrade owns the session's turn ends for now — the window's
    /// host says so from the upgrade's own state ([`crate::supervise::IdleHost::owns_turn_end`]:
    /// the agent answered READY and the restart is imminent), never from the
    /// screen: nothing is typed. A screen pattern (the last `❯` row the
    /// announcement) held a worker that answered without the marker, or an
    /// upgrade that gave up or was switched off, idle for ever, and was
    /// fooled by a 40-row read that cut the announcement off (the
    /// philosophy and hazards reviews of 2026-09-25). Never set while the
    /// loop's limit episode stands, and a wall — any wall — is decided by its
    /// own rule even where it is, with no relaunch asked for over the
    /// upgrade's own but the memory banner's ([`decide_turn_end`]).
    pub upgrading: bool,
    /// NO TASK YET: nobody but the harness has asked the session anything —
    /// the screen shows the launch card and no message
    /// ([`aterm_phase::Reading::fresh`]), or the session's host says its
    /// conversation holds no prompt of a person's or an orchestrator's
    /// ([`crate::supervise::IdleHost::taskless`]: the harness's own notices
    /// and carry-ons, and what the loop itself typed, are none). Nothing
    /// ended here, and nothing is typed.
    pub taskless: bool,
    /// The screen says the LOGIN IS BACK ([`aterm_phase::login_restored`]:
    /// `⎿  Login successful` under the `❯ /login` a person typed), what
    /// lifts a lost login the policy saw ([`decide_turn_end`]'s login hold).
    pub login_back: bool,
    /// What the session's host measures of the API's reach — the loop's to
    /// set, at a wall that never reached the API or was cut off, as it sets
    /// [`Self::restartable`]; [`Reach::Unknown`] from the screen.
    pub reach: Reach,
    /// How long ago a PERSON last typed or pasted into the session through
    /// its window (the server's `status human_ms=`; a control-socket write
    /// is not a person's), or the draft in the composer last changed — the
    /// later of the two, so a draft still being written holds the policy's
    /// hands on a server that does not say. `None`: neither — read as no
    /// person.
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
    /// The sandbox a Codex thread fell into from its launch's bypass — the
    /// loop's to set ([`crate::supervise::codex_usage::sandbox_fell`]):
    /// nothing is typed into it.
    pub sandbox_fell: Option<String>,
    /// The model the session's own Codex thread ran its last turn on, by its
    /// rollout's last `turn_context` (`gpt-6-luna`) — the loop's to set
    /// ([`crate::supervise::codex_usage::CodexSeen::thread_model`]), and
    /// while a save-then-wait switch is open only for a turn begun since its
    /// press ([`WindDown::pressed_at`], within a second's slack:
    /// [`crate::supervise::codex_usage::model_since`] — a turn on the cheaper
    /// model before it says nothing of where the thread is now, while the
    /// goal turn Codex runs under the nudge's box, begun between the press
    /// and the box being seen leaving, does); unknown from the screen.
    pub thread_model: Option<String>,
    /// The LIVE UPGRADE holds Codex's goal paused for its move (the tab's
    /// goal record, `harness::goal_hold`) — the loop's to set: a switch that
    /// held the session over that pause resumes the goal at its reset with
    /// `/goal resume`, never a carry-on typed over a goal left paused
    /// ([`wind_down_act`]).
    pub upgrade_goal: bool,
}

impl TurnEndReading {
    /// A PERSON'S HAND IN THE TURN THAT ENDED HERE ([`Self::person`]): a
    /// keystroke of theirs no older than the turn's own busy work (the loop's
    /// [`RunningTurn::point`]) plus [`PERSON_TURN_SLACK`] — the message they
    /// sent that began it, a steer typed into it, the Esc that stopped it.
    /// What a turn nobody here typed
    /// needs to be taken for a person's where the switch would otherwise
    /// take it for the harness's own ([`WindDown::seeded`]) or for Codex's
    /// goal ([`TurnEndState::observe`]).
    #[must_use]
    pub fn person_in_turn(&self) -> bool {
        self.person.is_some_and(|ago| {
            ago <= self
                .worked
                .unwrap_or_default()
                .saturating_add(PERSON_TURN_SLACK)
        })
    }

    /// The reading of one screen: `aterm-phase`'s [`aterm_phase::Reading`]
    /// of `rows` by the session's program's reader (read with the cursor's
    /// column) — its composer, its interrupt — whether a draft is typed (the
    /// loop's `typed_draft`), the work since the last point, the wall's reset
    /// on the loop's clock, and the rules. What only Claude Code's screen
    /// says (a submitted `❯` row not yet answered, its `Worked for` row) is
    /// read only on Claude Code's: Codex reads a message with no end under it
    /// as busy. Whether the live upgrade owns the point is the loop's to set
    /// from its host ([`Self::upgrading`]); no screen says it.
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
        let composer = if reading.composer.is_none() {
            Composer::Absent
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
        Self {
            phase: reading.phase.clone(),
            authoritative: reading.phase_authoritative,
            survey: reading.survey,
            wall: reading.wall.as_ref().map(|w| w.kind),
            resumes_by_itself: super::super::limit::resumes_by_itself(&message),
            wall_message: message,
            reset_at,
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
        }
    }

    /// A turn has ended here and nothing blocks acting on it: the phase is
    /// idle, a question or a limit — read by a reader whose word is
    /// evidence — no box or survey is up, a composer is drawn, and no input
    /// waits unanswered but one the policy judged not taken (`stale_input`,
    /// [`TurnEndState::observe`]).
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

/// A PERSON'S KEYSTROKE IN THE TURN IN FLIGHT WHEN A LOOP CARRIED A SWITCH ON
/// ([`WindDown::seeded_at`]: that turn began out of the loop's sight): typed
/// since the switch's last row (`row`) AND no older than the turn's work the
/// loop has seen (`worked`, from its first busy read) plus
/// [`SEEDED_TURN_BOUND`] — the most of a turn in flight the loop is taken not
/// to have seen. `person` is how long ago a person last typed, at `now`. A
/// keystroke since the row but older than that (a hold's last row three days
/// old, a keystroke a day ago) is no hand in the turn running now (the
/// re-review of 2026-09-28: it let an escaped goal turn run unstopped and
/// took its end for a person's `/goal resume`).
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

/// THE TURN RUNNING NOW, as the loop's busy reads keep it — the busy-read
/// twin of [`TurnEndReading::person_in_turn`], which judges a turn at its
/// end. Its SPAN is its own work so far: from its first busy read after the
/// last point ([`Self::point`]), a break the switch took as its point, or the
/// rate-limit nudge's box ([`Self::boxed`]) — the box marks the end of the
/// turn it covered, so whatever runs after it is a new turn (Codex's goal
/// starts one under the box within milliseconds, measured) and never the
/// covered one's continuation: carried across the box, the span made any
/// keystroke of the covered turn — hours of a goal run — a person's hand in
/// the goal turn Codex started on the cheaper model, which then ran unstopped
/// and untold (the re-review of 2026-09-28). The loop's `Session` keeps one;
/// the Tier-1 walk drives the same one along the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunningTurn {
    /// The first busy read of the turn running now.
    since: Option<Instant>,
    /// The span of the turn whose end the nudge's box covered, kept for the
    /// first point after it when no busy read came since (that point IS the
    /// covered turn's end, judged once the box is answered).
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

    /// THE POINT that ends the turn (or a break under a Codex background
    /// terminal the switch takes as its point), at `now`: the turn's work —
    /// its own span, else, where no busy read came after the nudge's box,
    /// the span of the turn the box covered (whose end this point is) — and
    /// a fresh span for the next; a person's hand latched in it is spent.
    pub fn point(&mut self, now: Instant) -> Option<Duration> {
        let own = self.since.take();
        let covered = self.covered.take();
        self.person = false;
        own.or(covered).map(|t| now.saturating_duration_since(t))
    }

    /// THE NUDGE'S BOX ANSWERED — its switch pressed (the save-then-wait
    /// switch opens) or its keep: the turn it covered ended under it, so its
    /// span is kept for that turn's point ([`Self::point`]) and whatever runs
    /// now is measured from its own first busy read; a person's hand latched
    /// before is none of it.
    pub fn boxed(&mut self) {
        if let Some(s) = self.since.take() {
            self.covered.get_or_insert(s);
        }
        self.person = false;
    }

    /// WHETHER THE TURN RUNNING NOW IS A PERSON'S, at a busy read at `now`:
    /// `typed` when a person last typed (the server's `human_ms`, a draft
    /// that changed), `floor` the open switch's opening
    /// ([`WindDown::opened_at`]: nothing typed before it is a hand in any
    /// turn the switch stops — the turn the nudge's box covered was the last
    /// it could begin), `seeded` the last row of a switch a loop carried on,
    /// while the turn in flight at its start runs ([`WindDown::seeded_at`]),
    /// `grace` `human_grace_s`.
    ///
    /// * A keystroke IN the turn — since the floor, and no older than the
    ///   span so far plus [`PERSON_TURN_SLACK`] (the message that began it,
    ///   however long it runs; a steer in it) or, for the seeded turn in
    ///   flight, since its row within [`SEEDED_TURN_BOUND`]
    ///   ([`seeded_hand`]) — makes it theirs, LATCHED until its point.
    /// * A keystroke since the floor within the grace holds the stop while
    ///   the grace lasts and latches nothing: typed just before a goal turn
    ///   began, it is no hand in that turn once the grace has run out (the
    ///   re-review of 2026-09-28: latched, it spared the whole turn).
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
    /// Have the session's host end the agent and relaunch it on its own
    /// conversation, then carry it on ([`Restart`]). `otherwise` is what the
    /// point gets where no restart is made (the host refused it, or none
    /// relaunches here): for the memory banner, its escalation with its
    /// remedy; for a bucket, its reset waited out; at a reset, the
    /// continuation the point was owed.
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
}

/// A model switch this policy made (owner decision 3).
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

/// Where a Codex save-then-wait switch stands ([`WindDown`], module header
/// "SAVE THEN WAIT").
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

/// THE SAVE-THEN-WAIT SWITCH of a Codex session (module header): made by the
/// approval policy's press of the rate-limit nudge's switch
/// ([`TurnEndState::nudge_switched`]), wound down, restored and held here.
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
    /// The first point judged after the switch ends the turn that ran BEFORE
    /// it (the box was up over its end): no person's turn of the switch's.
    pub pre: bool,
    /// Carried on from a previous loop's rows ([`TurnEndState::seed_wind`]):
    /// the turn that ends at the first point judged after it is the
    /// harness's own — the wind-down's, the goal turn it was stopping —
    /// unless the reading shows a person's hand in it
    /// ([`TurnEndReading::person_in_turn`]).
    pub seeded: bool,
    /// The harness's stops of Codex's goal since the switch opened — its Esc
    /// on a turn Codex ran by itself, its `/goal pause` — at most
    /// [`GOAL_STOPS`] ([`TurnEndState::goal_stop`]).
    pub stops: u32,
    /// When the harness's last Esc went, until a point shows it took
    /// ([`TurnEndState::goal_stop`] sends no second Esc into the same turn).
    pub stop_at: Option<Instant>,
    /// The harness's `/model` picker has been seen since its `/model` went
    /// ([`TurnEndState::picker_seen`]).
    pub restore_shown: bool,
    /// When a read first showed the harness's picker gone again (`None`: up,
    /// or not seen yet): its claim ends only once it has stayed gone
    /// [`PICKER_SETTLE`] ([`TurnEndState::picker_seen`]).
    pub picker_gone_at: Option<Instant>,
    /// The press is only INTENDED (`phase=intent`, ledgered before its key):
    /// a restarted loop carries it on without knowing whether the key went,
    /// so the nudge still up is decided again, nothing of the switch is
    /// stopped or typed until the footer shows the cheaper model
    /// ([`TurnEndState::footer_seen`]), and a footer on the thread's own
    /// model at a point closes it.
    pub intent: bool,
    /// Since when the points of an owed switch have waited on the footer —
    /// one that shows no model, or the thread's own model while the thread's
    /// rollout says its last turn ran the cheaper one: past
    /// [`OWED_FOOTER_BOUND`] a person is told.
    pub footer_since: Option<Instant>,
    /// When a switch carried on from the ledger was last written
    /// ([`TurnEndState::seed_wind`]), until the first point judged after it:
    /// the turn in flight then began out of the loop's sight, and a person's
    /// keystroke since that row — and within [`SEEDED_TURN_BOUND`] of the
    /// loop's first read of the turn ([`seeded_hand`]) — is their hand in it,
    /// at its end and at the loop's busy reads before it.
    pub seeded_at: Option<Instant>,
    /// When the switch OPENED (the nudge's box seen leaving, a press carried
    /// as only intended; a switch carried on from the ledger, the opening
    /// its rows name, else its last row's time): nothing a person typed
    /// before it is their hand in any turn the switch reads
    /// ([`RunningTurn::person`], [`TurnEndReading::person`] at an owed point
    /// whose footer is back on the thread's own model). `None`: not known (a
    /// pure caller's) — no floor.
    pub opened_at: Option<Instant>,
    /// When the nudge's switch was PRESSED — its intent ledgered, just
    /// before the key went (`pressed=`): the thread's rollout counts for the
    /// switch only for a turn begun since (the loop's
    /// [`TurnEndReading::thread_model`], within a second's slack) — the goal
    /// turn Codex runs on the cheaper model under the box begins between the
    /// press and the box being seen leaving (the round-4 re-review: floored
    /// at the opening, a footer lagging that turn read as the switch that did
    /// not land). `None`: not known — [`Self::opened_at`] floors it.
    pub pressed_at: Option<Instant>,
    /// The save's busy work in its turns that ended on an API error or a
    /// full context and were carried on (the turn running now is the loop's
    /// span): with it, the save's work so far is measured against
    /// [`WIND_DOWN_BOUND`] ([`TurnEndState::goal_stop`]). Not carried by the
    /// ledger: a loop restarted mid-save counts the save's turn from its own
    /// first busy read of it.
    pub wound: Duration,
    /// The harness's Esc went into the save's turn past [`WIND_DOWN_BOUND`]
    /// (once a switch): a save still running after it is said to a person.
    pub overran: bool,
}

impl WindDown {
    /// A switch the nudge's press opens (module header), owing its
    /// wind-down: `from` the thread's model and effort before it, `to` the
    /// cheaper model, `back_at` the original window's reset (`None`: not
    /// read), `marker` the line a saved wind-down answers with. Its first
    /// point ends the turn the nudge's box covered ([`Self::pre`]).
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

/// The ledger's word for where `w` stands (`phase=<word>`): `intent` for an
/// owed switch whose press is only intended ([`WindDown::intent`]) — so
/// every row written while it is (a note's, an edge's, an act's) is read
/// back as the intent it is, never as a landed switch (the round-4
/// re-review: a note's row said `owed`, and a restarted loop sent an Esc
/// into Codex's goal turn on the thread's own model) — else its phase's
/// ([`WindPhase::word`]).
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

/// How many times the harness stops Codex's goal while a switch is open — its
/// Esc on a turn Codex runs by itself (the first, on the goal turn under the
/// nudge's box, among them), its `/goal pause` at a free point — before it
/// stops trying and a person is told ([`TurnEndState::goal_stop`]).
pub const GOAL_STOPS: u32 = 3;

/// How far past a turn's own busy work a person's keystroke still counts as
/// their hand in it ([`TurnEndReading::person_in_turn`],
/// [`RunningTurn::person`]): the message typed and sent just before the
/// turn began.
pub const PERSON_TURN_SLACK: Duration = Duration::from_secs(30);

/// The most of the turn in flight when a loop carried a switch on that the
/// loop is taken not to have seen ([`seeded_hand`]): a loop restarts in
/// seconds (the live upgrade's relaunch) and a goal turn is minutes of work,
/// so a keystroke older than this before the loop's first read of the turn
/// began none of it. A HEURISTIC bound: a person's own turn that had run
/// longer than this out of sight is read as none of theirs.
pub const SEEDED_TURN_BOUND: Duration = Duration::from_secs(30 * 60);

/// The head of the note that Codex's goal runs on after every stop
/// ([`TurnEndState::goal_told`]): the loop keeps the badge that carries it up
/// past the goal's next turn.
pub const GOAL_NOTE_HEAD: &str = "Codex's goal keeps running";

/// How long the harness's `/model` picker must stay gone before its claim
/// ends ([`TurnEndState::picker_seen`]): the restore passes through two or
/// three boxes (the model, the effort, `More reasoning…`'s advanced one), and
/// a frame between two of them is no picker left.
pub const PICKER_SETTLE: Duration = Duration::from_secs(2);

/// How long an owed switch's points may wait on the footer before a person
/// is told: the save waits for the footer to show the cheaper model, and a
/// footer the reader cannot place — or one back on the thread's own model
/// while the thread's rollout says it ran the cheaper one — would hold it
/// silently.
pub const OWED_FOOTER_BOUND: Duration = Duration::from_secs(5 * 60);

/// THE SAVE'S OWN TURN IS BOUNDED (the owner's rule, 2026-09-28: the cheaper
/// model is there to commit and push the work, and only that — "not continue
/// work"): past this much busy work since the save instruction was typed,
/// the save's turn is no commit and push any longer, and the loop's busy
/// read stops it with ONE Esc ([`TurnEndState::goal_stop`]); its
/// interrupted point judges the marker (without it, a person is told), and
/// the thread's own model is put back and held. A commit and a plain push —
/// with one `git pull --no-rebase` on a rejected push, and a hook run
/// between — is minutes of work; this leaves room for a slow hook or a large
/// push. Without the bound only the cheaper model's obedience to `Then stop`
/// ended the save's turn (the round-4 re-review).
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
    /// The policy's own wait on the footer ([`TurnEndState::switch_hold`]):
    /// an owed save waits for the footer to show the cheaper model — one
    /// that shows none, or the thread's own model while its rollout says it
    /// ran the cheaper one, or a press only intended not seen landing.
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
    /// Codex's goal runs on after every stop was spent: told to a person
    /// once, and KEPT UP while the switch stands and the goal runs — the
    /// badge stays past the goal's next turn, and the session's host records
    /// it where a person looks ([`crate::supervise::IdleHost::inform`]).
    GoalNote(String),
    /// THE HOLD BEGAN: the session is back on its own model and waits for
    /// the window's reset — recorded for a person to see (an information,
    /// no badge), the loop naming the reset on its clock.
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
    /// A read since it was typed saw the agent's LIVE TURN — its spinner or
    /// its busy footer, never a break of its own background work
    /// ([`TurnEndState::took`]): the agent took it, whether or not a point has
    /// judged it yet ([`TurnEndState::act_untaken`]).
    taken: bool,
}

impl Awaited {
    /// When its point is judged with no busy read, or it is escalated as
    /// not taken: [`TurnEndTiming::take_within`] from the first point that
    /// showed no busy read, else from the act.
    fn due(&self, timing: &TurnEndTiming) -> Instant {
        later(self.unseen_since.unwrap_or(self.at), timing.take_within)
    }
}

/// What the policy remembers of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndState {
    pub timing: TurnEndTiming,
    /// When each typed act went, within the window.
    acts: VecDeque<Instant>,
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
    /// The `❯` row on the screen is an act of this policy's the worker never
    /// took (judged so at its deadline): a point, though input waits.
    stale_input: bool,
    /// Codex's save-then-wait switch, while one is open.
    wind: Option<WindDown>,
    /// The harness's own Esc stopped the turn now showing: its interrupt is
    /// no person's.
    own_interrupt: Option<Instant>,
    /// What the switch hands the loop ([`Self::take_wind_events`]).
    wind_events: Vec<WindEvent>,
    /// Done reports in a row at the points this state saw ([`says_done`]):
    /// `1`, the point is a done report and gets the [`DONE_CHECK`]; `2`, the
    /// check's own yield said done again — THE TASK IS DONE
    /// ([`Self::task_done`]) until someone else's turn.
    done: u8,
    /// The act awaited is the [`DONE_CHECK`] as this policy typed it (a draft
    /// submitted in its place is someone's own words, never the check).
    checking: bool,
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
            awaiting: None,
            followed: None,
            point_worked: Duration::ZERO,
            short: 0,
            point_at: None,
            wall: None,
            after: None,
            model: None,
            fallback_unmade: false,
            stale_input: false,
            wind: None,
            own_interrupt: None,
            wind_events: Vec::new(),
            done: 0,
            checking: false,
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

    /// The typed acts within the window ending at `now`.
    #[must_use]
    pub fn acts_in_window(&self, now: Instant) -> usize {
        self.acts
            .iter()
            .filter(|&&t| now.saturating_duration_since(t) < self.timing.window)
            .count()
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

    /// THE TASK IS DONE: the worker said so, was asked the [`DONE_CHECK`],
    /// and said so again. Nothing is typed into the session
    /// ([`decide_turn_end`]) until a turn of someone else's — a person's, an
    /// orchestrator's — ends at a point this state sees.
    #[must_use]
    pub fn task_done(&self) -> bool {
        self.done >= 2
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

    /// An act of this policy's is in flight: typed and its point not judged
    /// yet, or a command's continuation owed. A turn the HOST typed
    /// ([`Self::host_typed`]) is none: it holds this policy's acts back until
    /// its answer shows, never the host's own next step, which reads its own
    /// evidence (the READY answer in the conversation's record, the model a
    /// carry-on's answer names) — an answer no read saw busy would otherwise
    /// hold the upgrade still until the screen moved.
    #[must_use]
    pub fn act_in_flight(&self) -> bool {
        self.awaiting.is_some_and(|a| a.rule != RULE_HOST_TURN) || self.after.is_some()
    }

    /// An act of this policy's the agent has NOT TAKEN yet: typed, and no read
    /// since has seen the agent's live turn ([`Self::took`]) — or a command's
    /// continuation still owed. What holds back the host's step at a BREAK of
    /// the agent's own background work (`Session::host_steps_in_background`),
    /// where [`Self::act_in_flight`] held it until the act's POINT was judged:
    /// a break reads busy, so the act was never judged there, and an agent
    /// that orchestrates all day never reaches a true idle point — every break
    /// after the loop's own `keep going` was withheld from the live upgrade for
    /// hours (2026-09-28: s-d3346 from 14:52:45 and s-5c03a from 18:21:04, no
    /// look at either until the owner came). The guard's own purpose stands:
    /// no line of the host's is stacked on a continuation the agent has not
    /// read. Once it has — a live turn seen — the act is under way, and the
    /// break it leads to is the agent's own. The point that follows still
    /// judges it as before ([`Self::observe`]); a host line typed at the
    /// break ([`Self::host_typed`]) takes its place, and that point then
    /// judges the harness's turn with the work since the act's first busy
    /// read.
    #[must_use]
    pub fn act_untaken(&self) -> bool {
        self.awaiting
            .is_some_and(|a| a.rule != RULE_HOST_TURN && !a.taken)
            || self.after.is_some()
    }

    /// A read saw the agent's LIVE TURN (its spinner, its busy footer — never
    /// a break of its own background work): the act awaited, if any, was
    /// taken ([`Self::act_untaken`]).
    pub fn took(&mut self) {
        if let Some(a) = &mut self.awaiting {
            a.taken = true;
        }
    }

    /// The model switch in force, if any.
    #[must_use]
    pub fn model_switch(&self) -> Option<&ModelSwitch> {
        self.model.as_ref()
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

    /// Fold what a point shows into the state, BEFORE [`decide_turn_end`] is
    /// asked about it: the yield of the act typed last (a continuation or an
    /// answer that produced less than `min_work` lengthens the short streak,
    /// one that produced more ends it; a turn the host typed,
    /// [`Self::host_typed`], never lengthens it — answered short, the streak
    /// and its back-off stand — and ends it with real work), someone else's turn in between
    /// (`worked` with nothing awaited: short, or the streak's end), the
    /// first point this state sees (its work unknown: short), and the wall on
    /// the screen (a new kind opens a track; the same kind again
    /// after a retry is its next appearance). Reading the same point again
    /// (`worked` `None`, nothing awaited) changes nothing — and neither does
    /// a point after a TEXT act with no busy read since: the worker has not
    /// taken it yet (Claude Code draws a submitted message under the last
    /// done row, where it reads as queued, before its spinner comes), so
    /// that point is not the act's and the act stays awaited — for
    /// [`TurnEndTiming::take_within`] from the first such point. Past that,
    /// the point IS the act's, with no work seen: a reply that finished
    /// inside the `turn` verb's settle (`Nothing left to do.`), a wall that
    /// answered at once, an Enter that did not take — each judged as the
    /// short yield it is, never a latch. A slash command's point needs no
    /// busy read (`/model` answers at once). An act whose `❯` row stays last
    /// and unanswered, no busy read since, is waited for the same way — and
    /// past the deadline judged NOT TAKEN: the same short yield, its row
    /// then a point the next act may go at ([`decide_turn_end`]), on the
    /// back-off. Anyone else's unanswered row is never a point.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "SupervisorCodexRateNudge",
            action = "TurnEnds",
            project = "supervise_conformance_codex_rate_nudge::ph_of"
        )
    )]
    pub fn observe(&mut self, reading: &TurnEndReading, now: Instant) {
        if matches!(reading.phase, Phase::Busy | Phase::Prompt) {
            return;
        }
        if !reading.pending_input || reading.worked.is_some() {
            self.stale_input = false;
        }
        if reading.pending_input {
            let Some(a) = self.awaiting.as_mut().filter(|_| reading.worked.is_none()) else {
                return;
            };
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return;
            }
            self.stale_input = true;
        } else if let Some(a) = &mut self.awaiting
            && !a.command
            && reading.worked.is_none()
        {
            let since = *a.unseen_since.get_or_insert(now);
            if now < later(since, self.timing.take_within) {
                return;
            }
        }
        let min = self.timing.min_work;
        let checked = std::mem::take(&mut self.checking);
        let awaited = self.awaiting.take().map(|a| a.rule);
        let done = says_done(reading.said_tail.as_deref());
        let streak = |short: u32, d: Duration, done: bool| {
            if d < min || done {
                short.saturating_add(1)
            } else {
                0
            }
        };
        match (awaited, reading.worked) {
            // The harness's own turn: answered short, the streak and the
            // back-off it runs on stand as the worker's turns left them (the
            // point it counts from too); answered with real work, the streak
            // ends — a carry-on's answer is the worker's own work resumed.
            // …and its done reports stand as they were: a carry-on the agent
            // had nothing to add to is not the worker asked again.
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
                // other act of ours it is the first, and gets the check. An
                // act never taken (its `❯` row still unanswered) said
                // nothing: the reports stand.
                if reading.wall.is_none() && !reading.pending_input {
                    self.done = match (done, checked) {
                        (false, _) => 0,
                        (true, true) => 2,
                        (true, false) => 1,
                    };
                }
            }
            // Someone else's turn — a person's, an orchestrator's — is the
            // task theirs again, whatever it ended on: its done report is a
            // first one, and a turn that ended on a wall is not the worker
            // done — the wall's own ladder waits it out (a done task left
            // closed at a wall stranded it there: the review of 2026-09-27),
            // and the streak is left as it was.
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
        let retried = awaited.is_some_and(is_wall_retry);
        match reading.wall.map(class_of) {
            Some(class) => match &mut self.wall {
                Some(t) if t.class == class => {
                    if retried {
                        t.since = now;
                        // The act led to real work before the wall came
                        // back: a new episode, its ladder from the start.
                        if reading.worked.is_some_and(|w| w >= self.timing.progress) {
                            t.attempts = 0;
                            t.ups = 0;
                        }
                    }
                }
                _ => {
                    self.wall = Some(WallTrack {
                        class,
                        since: now,
                        attempts: 0,
                        ups: 0,
                    });
                    self.fallback_unmade = false;
                }
            },
            // The wall has left the screen: gone for good once the worker
            // worked (or our retry was taken). With no work — a human's
            // `/login` or `/model` output over it — a usage track stays, and
            // the continuation goes at once ([`decide_turn_end`]).
            None => {
                if reading.worked.is_some() || retried {
                    self.wall = None;
                    self.fallback_unmade = false;
                }
            }
        }
        let ours_stopped = reading.interrupted && self.own_interrupt.is_some();
        self.observe_wind(reading, awaited, ours_stopped, now);
        // The harness's own Esc is the interrupt of the turn it stopped; a
        // turn that ended otherwise since is anyone's.
        if (reading.worked.is_some() && !reading.interrupted) || self.wind.is_none() {
            self.own_interrupt = None;
        }
    }

    /// Record an act the loop TYPED (a skipped or refused one is not): the
    /// budget, the act awaited, what it leaves owed, and — for a wall's act
    /// — the track's attempts. A restart is no typed act
    /// ([`Self::restarted`]).
    pub fn acted(&mut self, action: &TurnEndAction, reading: &TurnEndReading, now: Instant) {
        let _ = reading;
        if matches!(action, TurnEndAction::Restart { .. }) {
            return;
        }
        let Some(rule) = action.rule_id() else {
            return;
        };
        // The harness's own Esc excused the turn it stopped, no later one:
        // its next act is typed now.
        self.own_interrupt = None;
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
        if matches!(
            rule,
            RULE_API_RETRY
                | RULE_API_BACK
                | RULE_API_CUTOFF
                | RULE_LIMIT_RESUME
                | RULE_CONSENT
                | RULE_COMPACT
                | RULE_LOGIN
        ) && let Some(t) = &mut self.wall
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

    /// Record a turn the session's HOST typed — the live upgrade's notice, a
    /// relaunched agent's carry-on ([`RULE_HOST_TURN`]): awaited as this
    /// policy's own acts are (the point after it is its answer's, judged at
    /// [`TurnEndTiming::take_within`] if no read sees it busy). Its answer is
    /// never a short turn of the worker's: answered short (READY, a carry-on
    /// the agent had nothing to add to), the streak and its back-off stand as
    /// the worker's own turns left them (N1 of the live E2E of 2026-09-26:
    /// the READY answer and the carry-on's reply, 2.8 s and 2.7 s, were read
    /// as two short turns of someone else's, and the continuation after a
    /// 2m34s stage waited 4 minutes); answered with real work, the streak
    /// ends as after any turn of real work — the carry-on's answer is the
    /// worker's own work, resumed (review of 2026-09-26: a 20-minute
    /// carry-on answer was otherwise backed off like the first point before
    /// it). No budget is spent: the owner's cap counts this policy's acts.
    pub fn host_typed(&mut self, now: Instant) {
        self.checking = false;
        self.awaiting = Some(Awaited {
            rule: RULE_HOST_TURN,
            command: false,
            at: now,
            unseen_since: None,
            taken: false,
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
    pub fn restarted(&mut self, why: &Restart, made: bool, reading: &TurnEndReading, now: Instant) {
        match why {
            Restart::Memory => {}
            Restart::Model { to } if made => {
                let back_at = reading.reset_at.or_else(|| {
                    self.timing
                        .limit_backoff
                        .last()
                        .and_then(|&b| now.checked_add(b))
                });
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
        }
    }
}

impl TurnEndState {
    /// THE NUDGE'S SWITCH WAS PRESSED (the approval policy's
    /// `rate-nudge-switch@v1`): the save-then-wait switch opens owing its
    /// wind-down, `from` the thread's model and effort before it, `to` the
    /// cheaper model, `back_at` the original window's reset (`None`: not
    /// read), `marker` the line a saved wind-down answers with. A switch open
    /// already is kept — the same nudge again is no new switch.
    pub fn nudge_switched(
        &mut self,
        from: CodexSetting,
        to: String,
        back_at: Option<Instant>,
        marker: String,
    ) {
        self.open_switch(WindDown::opened(from, to, back_at, marker));
    }

    /// The nudge's switch LANDED (the approval loop's press, the box seen
    /// leaving): `wind` — [`WindDown::opened`], made before the key went —
    /// opens owing its wind-down. A switch open already is kept — but one
    /// only INTENDED ([`WindDown::intent`], a restarted loop's) gives way to
    /// the press that decided the nudge again.
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

    /// A switch is open that is more than a press's intent
    /// ([`WindDown::intent`]): the nudge on the screen is kept. An intent a
    /// restarted loop carried on leaves the nudge still up to be decided
    /// again — its key may never have gone.
    #[must_use]
    pub fn switch_landed(&self) -> bool {
        self.wind.as_ref().is_some_and(|w| !w.intent)
    }

    /// A Codex footer read (any read): an INTENDED switch whose footer shows
    /// the cheaper model has landed ([`WindDown::intent`]); the rest of the
    /// switch goes on from there.
    pub fn footer_seen(&mut self, setting: &CodexSetting) {
        if let Some(w) = &mut self.wind
            && w.intent
            && setting.runs(&w.to)
        {
            w.intent = false;
        }
    }

    /// The harness's own Esc stopped Codex's goal turn right after the switch
    /// (measured 2026-09-28: Esc aborts the turn and pauses the goal in the
    /// same instant): the goal is the harness's to resume at the reset, and
    /// the interrupt now showing is no person's.
    ///
    /// The save's own turn stopped past [`WIND_DOWN_BOUND`] (WINDING) is no
    /// stop of the goal's: the goal was stopped before the save was typed,
    /// and none of [`GOAL_STOPS`] is spent — the Esc is the one the bound
    /// allows ([`WindDown::overran`]).
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

    /// The model and effort the harness's own `/model` restore puts back,
    /// while it is in flight — typed within [`TurnEndTiming::restore_pick`],
    /// its picker not yet left and no person's keystroke since
    /// ([`Self::picker_seen`]): the only picker the approval policy answers.
    #[must_use]
    pub fn restore_target(&self, now: Instant) -> Option<CodexSetting> {
        let w = self.wind.as_ref()?;
        let typed = w.restore_at?;
        (matches!(w.phase, WindPhase::Restore { .. })
            && now.saturating_duration_since(typed) < self.timing.restore_pick)
            .then(|| w.from.clone())
    }

    /// A READ OF THE SCREEN while the harness's `/model` restore is in
    /// flight: `shown` whether Codex's model picker is up (any of its boxes),
    /// `person` how long ago a person last typed ([`TurnEndReading::person`]).
    /// The harness's picker LEFT — seen, then gone on two reads at least
    /// [`PICKER_SETTLE`] apart (a frame between the model, the effort and the
    /// advanced box is no leaving), or gone at an idle point
    /// ([`Self::observe`]) — or a person's keystroke since the `/model` went
    /// ends the restore's claim on the picker ([`Self::restore_target`]): a
    /// picker a person opens after that is theirs, not driven back to the
    /// original model.
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

    /// WHAT THE HARNESS DOES ABOUT A TURN RUNNING while a switch is open
    /// (the owner: nothing goes on on the cheaper model): `busy` the running
    /// turn's busy work so far where the screen shows one running (its status
    /// row, the reader's busy guard — not the footer's cached goal alone:
    /// 0.158 draws a goal turn's composer as `»`, which reads no point — and
    /// the loop's span of it, [`RunningTurn`]), `None` where none runs,
    /// `goal` the footer's goal, `person` whether a person's hand is on the
    /// session (their grace). While the wind-down is owed, or the model owed
    /// back before the hold, ANY turn running — Codex's goal turn under the
    /// nudge's box, one its goal started again by itself after a stop — is
    /// stopped with one Esc ([`GoalStop::Esc`]); during the hold, a turn of
    /// the goal the footer shows pursued; the save's own turn (WINDING) only
    /// once its busy work since the save was typed ([`WindDown::wound`] and
    /// `busy`) reaches [`WIND_DOWN_BOUND`] — ONE Esc a switch
    /// ([`WindDown::overran`]), none of [`GOAL_STOPS`], and a save still
    /// running after it is said to a person once ([`GoalStop::Tell`]). Never
    /// a person's turn (`person`: their hand in the running turn since the
    /// switch opened, or their grace while it lasts — the loop's
    /// [`RunningTurn::person`]), never a switch only intended
    /// ([`WindDown::intent`]: the key may never have gone, and the thread
    /// then runs its own model), never twice into one turn (an Esc waits
    /// [`TurnEndTiming::take_within`] for its point), and at most
    /// [`GOAL_STOPS`] stops a switch (`/goal pause` counts): past them, a
    /// person is told ONCE ([`GoalStop::Tell`], [`Self::goal_told`]).
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

    /// The stops are spent and Codex's goal runs on ([`GoalStop::Tell`]): a
    /// person is told, once per switch — a note KEPT UP while the switch
    /// stands and the goal runs ([`WindEvent::GoalNote`]). The save's own
    /// turn still running after the harness's Esc past [`WIND_DOWN_BOUND`]:
    /// a note of its own, once per switch.
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

    /// A switch a previous loop on the session opened and did not close
    /// (the approval ledger's rows, `approvals::open_wind_down`): carried on
    /// from where it stood. The turn that ends at the first point after it —
    /// the wind-down's own, the goal turn the harness was stopping — is the
    /// harness's, never taken for a person's without their hand in it
    /// ([`WindDown::seeded`]); a wind-down whose turn was running stays
    /// WINDING, so its end still judges the marker; an interrupt the
    /// harness's own last Esc made — its row the last one, carrying `esc=`
    /// (`wind.stop_at`), in ANY phase, written within
    /// [`TurnEndTiming::take_within`] of `now` — is no person's, and no
    /// second Esc goes into that turn. An `esc=` older than that is SPENT:
    /// its point came and went unwritten (a hold's Esc'd point writes no
    /// row), and the interrupt a later loop sees is anyone's (the re-review
    /// of 2026-09-28: a restart days into a hold took a person's own Esc for
    /// the harness's). The notes said stay said (`told=`). A hold counts
    /// from the start its row names (a restart never lengthens it). A switch
    /// of this loop's own wins.
    pub fn seed_wind(&mut self, mut wind: WindDown, now: Instant) {
        if self.wind.is_some() {
            return;
        }
        // The harness's last Esc, its row the last (`esc=`) and fresh: the
        // interrupt the next point shows is its own in any phase, and the
        // turn it stopped no person's without their hand in it.
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
        self.wind = Some(wind);
    }

    /// What of THIS POLICY'S OWN holds the open switch at the point `r`
    /// (the loop's stood-still note names it — the round-4 re-review: a hold
    /// of the policy's was told as the terminal's, with a fix that fixed
    /// nothing): a thread fallen into a sandbox ([`StoodBy::Sandbox`]), an
    /// owed save whose footer does not show the cheaper model
    /// ([`StoodBy::Footer`]), a restore whose tries are spent
    /// ([`StoodBy::RestoreSpent`]). `None`: nothing of the policy's — what
    /// holds it is a person's hand, a draft or the terminal.
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

    /// The switch has stood still — owed its save, winding down, owed its
    /// model back — through a long break under a Codex background terminal
    /// (the loop's), held there by `by`: a person is told, once per switch,
    /// what holds it — a person's typing or draft, the terminal, or the
    /// policy's own wait ([`Self::switch_hold`]: the footer, a sandbox, the
    /// restores spent), each with the fix that moves it. Before the save
    /// (owed, winding) the note never advises putting the model back by hand
    /// — the work is to be saved on the cheaper model first; after it, the
    /// hand steps are named. A press only intended ([`WindDown::intent`]) is
    /// never said to have put the session on the cheaper model.
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

    /// Fold a judged point into the open switch ([`Self::observe`]'s last
    /// step): `awaited` the act the point follows (`None`: someone else's
    /// turn — `worked` says one ran).
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
        // A person's hand in the turn that ended here — for the turn in
        // flight when a loop carried the switch on, a keystroke since the
        // switch's last row within the bound of the turn's unseen start
        // ([`seeded_hand`]).
        let person = reading.person_in_turn()
            || seeded_at.is_some_and(|row| seeded_hand(reading.person, reading.worked, row, now));
        // A turn nobody here typed ran: a person's — or Codex's own goal
        // turn while the goal is pursued, the goal turn the harness's own
        // Esc just stopped, the turn whose end the nudge covered, or (the
        // switch carried on from a previous loop) the turn the harness had in
        // flight then, unless a person's hand shows in it.
        let theirs = awaited.is_none()
            && reading.worked.is_some()
            && !ours_stopped
            && !pre
            && (!seeded || person);
        let goal_runs = reading.goal == Some(CodexGoal::Pursuing);
        // A PERSON'S `/model` before the hold: the footer shows neither the
        // cheaper model the switch left it on nor its own model back — the
        // session is theirs, and nothing of the switch goes on.
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
        // Codex's goal still pursued at a point while the switch holds the
        // session, and every stop spent: a person is told, once
        // ([`Self::goal_stop`]'s `Tell` at a busy read says the same).
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
                // THE SWITCH THAT DID NOT LAND: the footer still shows the
                // thread's own model at a point past the one the press's box
                // covered (that one's footer may not have caught up) — the
                // press missed, or a restart carried on a switch whose key
                // never went — and the thread's own rollout does not say it
                // ran the cheaper model since the press either (a footer
                // that lags a turn already run there — the goal turn under
                // the box among them). Nothing of it goes on. A PERSON'S
                // keystroke since the switch opened, the footer on another
                // model than the cheaper one: their `/model` put it there —
                // the session is theirs.
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
                    // The rollout says the cheaper model, the footer the
                    // thread's own: a lag — waited on, and past the bound
                    // said (a stale thread read, or a `/model` back nobody
                    // typed a keystroke for, would hold it silently).
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
                    // A footer that shows no model holds the save back (it
                    // waits for the cheaper model to show): past the bound,
                    // said.
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
                    // A person's own message (their hand in the turn, or no
                    // goal of Codex's that could have run it): the wind-down
                    // is theirs to give; the model is still owed back.
                    if let Some(w) = &mut self.wind {
                        w.phase = WindPhase::Restore { hold: false };
                        w.goal_paused = false;
                    }
                    self.wind_edge(
                        RULE_WIND_DOWN,
                        "a person took the session over before the wind-down".to_string(),
                        None,
                    );
                } else if reading.composer == Composer::Typed
                    && !self.wind.as_ref().is_some_and(|wd| wd.intent)
                {
                    // (A press only intended is not known to have put the
                    // session on the cheaper model: its footer holds the save
                    // first, and the footer's note says so.)
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
                // The wind-down's turn ended on an API error or a full
                // context: not its end — the wall's own rule carries it on
                // ([`wind_down_act`]), and the point after that is judged.
                // Its busy work so far counts toward [`WIND_DOWN_BOUND`].
                if matches!(
                    reading.wall,
                    Some(WallKind::ApiError { .. } | WallKind::Overloaded | WallKind::Context)
                ) {
                    if let Some(wd) = &mut self.wind {
                        wd.wound = wd.wound.saturating_add(reading.worked.unwrap_or_default());
                    }
                    return;
                }
                // The wind-down's own point (or a person's steer in it) — or
                // the one the harness's Esc made, the save's turn past its
                // bound ([`Self::goal_stop`]): the marker judged there all
                // the same.
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
                // An idle point with the harness's picker seen and gone: the
                // picker has left once it has been gone [`PICKER_SETTLE`]
                // ([`Self::picker_seen`]) — an idle frame between its boxes,
                // however long the loop's own settle, is no leaving (the
                // re-review of 2026-09-28: `/model` typed again into the
                // middle of the picker's chain).
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
                // Codex's goal runs again before the model is back: a
                // person's `/goal resume` only with their hand in it — the
                // session is theirs then, its model still owed back, nothing
                // held after — otherwise the goal escaped its stop, and the
                // harness stops it again ([`Self::goal_stop`], `/goal
                // pause`).
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
                    // Codex's goal runs during the hold: a person's `/goal
                    // resume` only with their hand in it — otherwise it
                    // escaped its stop, and the harness stops it again.
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
/// 4:24 PM` → 182 s): Claude Code's own measure of the turn, which a
/// supervisor that started mid-turn saw only part of.
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

/// The worker's last words, judged ([`Said`]). `tail` comes one line per
/// SCREEN ROW ([`aterm_phase::said_tail`]), so a soft wrap is a line break
/// that means nothing: phrases are matched on the rows joined with single
/// spaces, and the question is the last SENTENCE of the last unit — the
/// rows from the last enumerated option row on, joined — never the last
/// row (lane B2's review: `Want me to keep the old API⏎or drop it?` and a
/// stop phrase split by a wrap were continued). A stop phrase anywhere
/// wins — naming the act when a destructive word is anywhere in the tail
/// (`I'll wait for your go-ahead before force-pushing.`); then an offer to do something destructive ([`DESTRUCTIVE_WORDS`]
/// anywhere after the FIRST offer phrase — a benign offer after a
/// destructive one does not hide it) is a stop, and so is any question
/// that names a destructive act, offer or not (`Delete the stale branches
/// now? (y/n)`); then a last sentence that asks (`?`, or a trailing `(y/n)`
/// / `[y/N]` / `(yes/no)`, a trailing parenthetical aside after it not
/// counting): a choice between options with no recommendation — two or more
/// listed options under an ask that CHOOSES ([`asks_to_choose`]: `Which
/// one?`, `Shall I start?`, never `Did the suite pass?`), or an either/or the
/// worker would act on ([`either_or`]: `should I … or …`, never `Did it pass
/// on your machine, or should I rerun it?`) — is a stop, and so is a
/// [`CHOICE_PHRASES`] ask where it asks ([`phrase_asks`]); ANY destructive
/// word anywhere in the tail of a choice makes it a stop that names the act
/// (the adversarial review of 2026-09-25: `1. Clean slate⏎Delete build/ and
/// target/⏎2. Incremental⏎Which do you prefer?` — the act was on a
/// description row), inflections included ([`destructive_word`]); an offer
/// is an offer, anything else a question; a tail that does not ask is an
/// offer when it opens next steps, else plain. (The safety review of
/// 2026-09-24: each of those shapes was continued, and `keep going` reads as
/// yes.) Every stop and question is ANSWERED ([`RULE_ANSWER`]) — one that
/// names an irreversible act ([`Said::Irreversible`]) only with
/// [`REVERSIBLE_ANSWER`]; the reason is what an escalation says where
/// `answer_questions = false`.
#[must_use]
pub fn classify_said(tail: Option<&str>) -> Said {
    let Some(tail) = tail else {
        return Said::Plain;
    };
    let lower = yes_no_asks(&tail.to_lowercase().replace('’', "'"));
    let flat = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(p) = STOP_PHRASES.iter().find(|p| flat.contains(*p)) {
        // A stop that names a destructive act anywhere asks consent to it
        // (`I'll wait for your go-ahead before force-pushing.`): whatever
        // answers it must consent to nothing irreversible, as a choice's.
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
    // A choice that names a destructive act ANYWHERE in the tail — an
    // option, its description row, the ask, the report before it: whatever
    // answers it consents to that act.
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

/// What, found before an either/or's ` or `, makes it one the worker would
/// act on: `should I rewrite the parser or patch the lexer?`, `want me to
/// keep the old API or rename it?`, `which would you prefer: … or …?`. A
/// question with none of them — `did the suite pass on your machine, or
/// should I rerun it?` — asks the person something only they know.
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

/// Whether the asking sentence `ask` under a list of options asks to CHOOSE
/// one — `which one?`, `which do you prefer?`, `shall I start?`, `1 or
/// 2?` — rather than something else about the work (`did the suite pass on
/// your machine?`, `does that look right?`: a question).
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

/// Whether a [`CHOICE_PHRASES`] ask `p`, found at `at` in `flat`, ASKS: the
/// sentence it is in ends `?` (`which would you prefer: the arena or the
/// slab?`), or it is in the tail's last sentence (`let me know which layout
/// you want.`), or its row introduces the enumerated options (`let me know
/// which you prefer:⏎- rebase⏎- open a new PR`). A phrase in an earlier
/// sentence of a report (`documented which option each flag maps to. All
/// tests pass.`) does not.
fn phrase_asks(flat: &str, lines: &[&str], p: &str, at: usize) -> bool {
    let rest = &flat[at..];
    let bytes = rest.as_bytes();
    // The first sentence end after the phrase: `.`, `!` or `?` followed by
    // a space or the end — a `.` after a digit (`1. drop`) is an option's
    // number, not one.
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

/// The first of [`DESTRUCTIVE_WORDS`] in `text`, as a whole word (a phrase
/// as whole words) or an inflection of one ([`inflects`]: `deletes`,
/// `dropped`, `wiping`, `force-pushed` — the adversarial review of
/// 2026-09-25: `1. Fresh start (removes node_modules)` matched no base form
/// and was answered), named by its base form.
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

/// Whether `word` is `base` or one of its regular inflections: `-s`/`-es`,
/// `-d`/`-ed` and `-ing` — a final `e` dropped before them (`deleting`,
/// `removed`), a final consonant doubled (`dropped`, `resetting`) — plus
/// `-en` after a doubled `t` (`overwritten`) and the one irregular past the
/// list needs (`overwrote`, `rewrote`). Over-matching a noun (`drops`,
/// `releases`) only stops a point that would have been answered; missing a
/// verb answers one a person had to.
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

/// What a question or a choice that names an irreversible act is answered
/// with ([`Said::Irreversible`], D1 of the reconciliation of 2026-09-25):
/// never "take the option you would recommend" — the recommendation may be
/// the act — and not the owner's `answer_text`, which may say so.
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

/// THE DECISION at one point (module header). Pure: the same state, reading,
/// policy and clock give the same action.
#[must_use]
pub fn decide_turn_end(
    state: &TurnEndState,
    reading: &TurnEndReading,
    cfg: &SupervisorConfig,
    now: Instant,
) -> TurnEndAction {
    // A point the live upgrade owns (its host's word) is the upgrade's —
    // except at a WALL: the upgrade types nothing there and its host takes
    // no step at one (the loop's `host_steps_here`), so EVERY wall is decided
    // by its own rule — a limit waited out and continued past its reset, an
    // overloaded or failed API call retried, a full context compacted, a lost
    // login typed — with no relaunch asked for over the upgrade's own (a
    // model bucket is waited out, never relaunched on its fallback). The
    // reviews of 2026-09-26: a wind-down turn that hit a limit naming its
    // reset, `resets 7:30pm`, was held here for good; and so, after that fix,
    // was one that ended on `529 Overloaded` or a full context — nobody
    // typing its retry and the host never stepping. The memory banner keeps
    // its restart: its one remedy is one, and the upgrade's own needs a
    // READY an agent past reading never gives. And the continuation a wall's
    // act OWES (`/compact`'s `then: Continue`) is paid at the idle point after
    // it, owned or not: an owed act is an act in flight, which keeps the
    // host out too, so muted here it was paid by nobody and the agent sat
    // compacted for good (review of 2026-09-27).
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
    // holds a TYPED act back — a draft, an unanswered message, the survey,
    // an act awaited, a spinner — does not hold its answer back (the host's
    // restart takes its own last looks: idle, nobody's hand). A turn a
    // person stopped is theirs for the grace below, banner or not.
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
    // No task — none yet, or one the worker said was done after the done
    // check: nothing ended here, and nothing is typed — no continuation, no
    // answer, no wall's act. Except the save-then-wait switch's own steps:
    // a switch the harness opened owns every point while it is open (below),
    // so a done task never strands the thread on the cheaper model — the
    // merge of 2026-09-29, as the live upgrade's gate above leaves an open
    // switch its points.
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
    // A draft standing in the composer (the grace above has passed since it
    // last changed): submitted in the act's place, under its rule — never
    // typed over. Never into a lost login, though: the wall would answer it
    // in milliseconds and the person's words would be spent on nothing, so
    // where `/login` was the act the point is the person's, and said.
    match act.rule_id() {
        Some(RULE_LOGIN) if reading.composer == Composer::Typed => TurnEndAction::Escalate {
            reason: format!(
                "the login is gone: {} (a draft stands in the composer; sign in with /login)",
                reading.wall_message
            ),
        },
        Some(rule_id) if reading.composer == Composer::Typed => TurnEndAction::Submit { rule_id },
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
                    "typing budget spent: {used} acts in the last {} min",
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
        // A Codex thread its launch ran outside the sandbox, now running in
        // one (`codex_usage::sandbox_fell`): it can neither commit nor push,
        // and nothing typed into it goes on with its work — the loop says so
        // once, with the fix.
        if reading.sandbox_fell.is_some() {
            return TurnEndAction::Nothing;
        }
        // Codex's goal runs its next turn by itself within milliseconds of
        // a turn's end (measured): a continuation typed now lands in that
        // turn as a steer (all eleven of 2026-09-27/28 did). A lost login
        // and a usage or spend limit — where the goal goes on by itself no
        // more — are still their walls' rules'.
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
        return wall_action(
            state,
            reading,
            cfg,
            now,
            kind,
            &budgeted,
            &continue_as,
            &type_as,
        );
    }
    // A LOST LOGIN THE POLICY SAW is waited out off the screen too: its track
    // stands (`/login` typed or not) and the worker has not worked since. The
    // wall's row gone — the `/login` dialog dismissed, a line of its output
    // under it — is no login back, and whatever is typed there is answered
    // by the wall again in milliseconds (the incident of 2026-09-27: nine
    // hours of `continue`, `keep going` and upgrade notices, each met by
    // `Login expired · Please run /login`). The vendor's own `Login
    // successful` ([`TurnEndReading::login_back`]) lets the point be acted
    // on, and so does the worker working, which ends the track
    // ([`TurnEndState::observe`]); the owner was told when the wall showed.
    if let Some(t) = &state.wall
        && t.class == WallClass::Auth
        && !reading.login_back
    {
        return TurnEndAction::Nothing;
    }
    // A command's continuation, owed once it has run.
    if let Some(rule) = state.after {
        return continue_as(rule);
    }
    // The screen left a limit it was waiting out, with no work and no act
    // of ours (a human's `/login`, `/model`): the continuation goes at once.
    if let Some(t) = &state.wall
        && (t.class == WallClass::Usage || t.class == WallClass::Model)
        && t.attempts == 0
        && cfg.resume_limits
    {
        return resume_limit(reading, &continue_as);
    }
    let cont = continue_policy(state, reading, cfg, now, &budgeted);
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

/// The continue policy at a point with no wall: the question answered or
/// the turn continued (the worker's own suggestion first) once the short
/// streak's back-off has passed; a question escalated only where the owner
/// switched the answers off.
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
    // What the worker asks of a person — nothing, after a person's Esc: the
    // point is theirs, and resuming it is a continuation. A question the
    // reader could not read the words of is never taken for "nothing asked".
    let mut irreversible = false;
    let asked = if reading.interrupted {
        None
    } else if reading.phase == Phase::Question && reading.said_tail.is_none() {
        Some("the worker asked something its last words do not show".to_string())
    } else {
        match classify_said(reading.said_tail.as_deref()) {
            Said::Irreversible(why) => {
                irreversible = true;
                Some(why)
            }
            Said::Stop(why) => Some(why),
            Said::Question => Some("the worker asked a question".to_string()),
            Said::Offer | Said::Plain => None,
        }
    };
    // A done report gets the done check — a continuation's words that let
    // the worker stop — whatever else it said: a done worker's "what's
    // next?" answered with `answer_text` is how it invented work.
    let done = state.done == 1 && !reading.interrupted;
    match &asked {
        Some(why) if !cfg.answer_questions => {
            return TurnEndAction::Escalate {
                reason: format!("{why}; answer_questions is off"),
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
        return budgeted(TurnEndAction::Type {
            text: if irreversible {
                reversible_answer(rules)
            } else {
                answer_text(cfg, rules)
            },
            rule_id: RULE_ANSWER,
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

/// A limit waited out: the continuation [`TurnEndTiming::reset_grace`] past
/// the reset the notice names — or, after a continuation that hit the wall
/// again, [`TurnEndTiming::limit_backoff`] from that appearance, whichever is
/// later; with no reset read, the longest backoff from the appearance.
fn resume_at_reset(
    state: &TurnEndState,
    reading: &TurnEndReading,
    now: Instant,
    since: Instant,
    attempts: usize,
    kind: WallKind,
    continue_as: &dyn Fn(&'static str) -> TurnEndAction,
) -> TurnEndAction {
    let back = &state.timing.limit_backoff;
    let backoff = |i: usize| crate::supervise::ladder::Ladder(back).at(i);
    let at_reset = reading.reset_at.map(|r| later(r, state.timing.reset_grace));
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

/// The wall policies (module header).
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
            let act = |rule: &'static str| type_as(rule, &wall_retry_text(rule, cause, msg));
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
                // the short ladder by the acts this measure has made (it met
                // the wall again: the measure was wrong, or the API still
                // fails). NOT a certificate or proxy refusal: the host's
                // handshake is verified against the platform's trust store,
                // the agent's against its own (`NODE_EXTRA_CA_CERTS`; the
                // vendor's `SELF_SIGNED_CERT_IN_CHAIN … an authority Claude
                // Code doesn't trust`), so on a network whose inspecting
                // root only the keychain trusts the host reads Up while the
                // agent is refused — and "the API is reachable again" would
                // be typed at once, then on every rung, and be false each
                // time (the review of 2026-09-27). It stays on the ladder.
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
                // The vendor goes on by itself: waited out — handled, nobody
                // is needed (the philosophy review of 2026-09-25: `Nothing`
                // here opened an escalated episode, a badge and a mail for a
                // wall that needed no one) — and continued only if it has
                // not gone on [`TurnEndTiming::auto_resume_grace`] past the
                // time it named (with none, the longest limit back-off).
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
        // A bucket that asks consent to go on on usage credits: accepted
        // (module header) — continued, where the vendor's confirm is a box
        // the approval policy answers. Back after that, or with the
        // continuation limited: switched off, or waited out, like any bucket.
        WallKind::ModelBucket { consent } => {
            if consent && attempts == 0 && cfg.continue_policy {
                return continue_as(RULE_CONSENT);
            }
            let Some(fallback) = &cfg.model_fallback else {
                return wait_out();
            };
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
            // errors are retried (`retry_api_errors`, on by default — the
            // window's host and `drive watch` alike).
            if !cfg.retry_api_errors {
                return esc(format!("the login is gone: {msg}"));
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
/// Codex's model picker, which the restore drives (the approval policy's
/// `model-restore-pick@v1`).
pub const MODEL_PICKER: &str = "/model";

/// `word` with its first letter upper-cased (`ultra` → `Ultra`).
fn capitalized(word: &str) -> String {
    let mut cs = word.chars();
    cs.next()
        .map(|c| c.to_uppercase().chain(cs).collect())
        .unwrap_or_default()
}

/// THE INSTRUCTION THAT SAVES THE WORK, typed once on the cheaper model
/// (the owner, 2026-09-28: the switch is "for codex to git commit and push
/// work and then wait for the original model settings"): commit the work in
/// progress on the current branch, push it plainly — one `git pull
/// --no-rebase` on a rejected push, nothing more — never force, rebase,
/// reset, stash or switch branches, say what failed rather than work around
/// it, then stop; and answer with the one-time marker line only when all of
/// it is committed and pushed. The standing rules ride with it.
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

/// THE SAVE-THEN-WAIT SWITCH'S ACT at a free point (module header): `None`
/// with no switch open, else the point is the switch's —
///
/// * NOTHING into a thread fallen into a sandbox its launch bypassed — no
///   save instruction, no `/goal pause`, no `/model`, no resume (the owner:
///   "not continue work in a sandbox"; the loop's sandbox note says it);
/// * Codex's goal pursued while the switch holds the session (owed, the
///   model owed back before the hold, holding): `/goal pause`, one of the
///   [`GOAL_STOPS`] stops (the loop's Esc on a running goal turn is another,
///   [`TurnEndState::goal_stop`]); its stops spent, nothing;
/// * owed: the instruction that saves the work — once the footer shows the
///   cheaper model (the press confirmed; one that shows the thread's own
///   model is the switch that did not land, closed by
///   [`TurnEndState::observe`]; a press only intended stops no goal before
///   it shows), never over a draft;
/// * winding down: nothing, but an API error or a full context the
///   wind-down's own turn ended on, which its wall's rule answers (the
///   save's turn running past [`WIND_DOWN_BOUND`] is stopped by the loop's
///   busy read, [`TurnEndState::goal_stop`]);
/// * restore owed: `/model` (the approval policy picks the original model
///   and effort, for this conversation only) up to [`RESTORE_TRIES`] times,
///   then nothing (the note names the hand steps); never over a draft;
/// * holding: nothing until the reset — the original window's reset plus
///   [`TurnEndTiming::reset_grace`], or [`TurnEndTiming::unknown_hold`]
///   from the hold's start when nobody read it — decided again at least
///   every [`TurnEndTiming::limits_every`], and sooner when the usage
///   reading drops back under its limit; then, under `resume_limits`,
///   `/goal resume` when the harness paused the goal, else — under
///   `continue_policy` — [`resume_text`]; with either key off, nothing is
///   typed and a person is told (escalated). Never over a draft.
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
    let draft = reading.composer == Composer::Typed;
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
                // (An intent whose footer does not show the cheaper model may
                // be a key that never went: the goal runs on the thread's
                // own model, and nothing of the switch stops it.)
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
            let due = match w.back_at {
                Some(b) => later(b, t.reset_grace),
                None => later(since, t.unknown_hold),
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
            // A goal paused under the switch — by the switch, or by the live
            // upgrade for its move, whose pause the switch then held the
            // session over (the goal-pause review of 2026-09-28: the switch
            // typed its carry-on at the reset while the upgrade's own resume
            // waited on it, and a person told by the upgrade's row to resume
            // the goal would have run it before the reset).
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
                         continue_policy is off: aterm types no carry-on — {by_hand}",
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

/// Claude Code's critical-memory banner (2026-09-24: a worker at 38.8 GiB
/// resident that read no input for 2h41m). Nothing typed into it is read
/// and no reset ends it; the one remedy is a restart, and the banner names
/// it — so it is TAKEN (D3): [`TurnEndAction::Restart`], the session's host
/// ending the agent at its idle point and relaunching it on its own
/// conversation, then telling it to carry on. The DECIDER says it at any
/// point it is asked about, ahead of the gates a typed act waits on. The
/// LOOP asks only at a point: under a running spinner — where Claude Code
/// draws the banner, and where the incident sat — it waits for the turn to
/// end, and the server's own `wall:memory` menu row is the notice there
/// (`aterm-gui`'s `status_item`). `[harness] relaunch = false` limits it to
/// the escalation with its remedy, and so does a loop no host restarts for
/// (`drive watch`), which escalates the same `reason`.
///
/// The escalation's remedy names the agent's OWN conversation
/// ([`TurnEndReading::resume`]: `claude --resume <id>`), or no command where
/// none was read, and quotes only the banner's fact
/// ([`aterm_phase::memory_banner_head`]) — never the vendor's own `claude
/// --continue`, which resumes the directory's newest conversation, a sibling
/// tab's where two share it (robustness backlog item 2, 2026-09-26).
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

/// THE WORLD AND THE MUTANTS of `SupervisorCodexRateNudge`
/// (`aterm_spec::derive::supervisor_codex_rate_nudge_model`): its
/// supervisor's actions are bound where the real deciders run — the press
/// (`approval::rate_nudge`, [`TurnEndState::open_switch`]), the stops
/// ([`TurnEndState::goal_stop`], [`TurnEndState::goal_told`]), a turn's end
/// ([`TurnEndState::observe`]) and the free point's acts (`wind_down_act`,
/// `continue_policy`); these are Codex's, the clock's and a person's steps,
/// each one the Tier-1 bind (`tests/supervise_conformance_codex_rate_nudge.rs`)
/// performs, and the `Buggy = 1` members, which no shipping code runs.
/// Compiled only for proof and test builds: nothing calls it.
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
