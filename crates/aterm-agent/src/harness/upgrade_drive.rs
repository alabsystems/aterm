// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The I/O half of [`super::upgrade`]: advances Claude Code sessions one step
//! at a time through the live upgrade, and Codex sessions through the same
//! step's Codex branch (`upgrade_codex_drive.rs`, over
//! [`super::upgrade_codex`]): the shared daemon first by the vendor's verb,
//! then the tab's TUI by a typed `/exit` and `codex resume`, never a signal.
//! A tab whose foreground job is a Codex belongs to that branch alone.
//!
//! Two callers take the same steps. The window's host drives each session:
//! its worker asks [`due`] when the session is attached, when atpkg's
//! activation notice says a newer build was installed
//! ([`super::upgrade_wake`]), and on a short retry ladder while a look could
//! not read what decides it ([`Due::Unread`]: the socket, Claude Code's
//! records, the agent's process, a launch record, a build's `--version`
//! within [`VERSION_WAIT`]). On [`Due::Yes`] the worker asks its loop for
//! the next idle point and takes one [`step`] there: announce, or after
//! READY end and relaunch the agent ([`super::relaunch`]), handing the new
//! process back to its supervisor ([`Opts::hand_back`]), whose next idle
//! point types the continuation ([`super::relaunch::resume`]). A person or
//! script runs [`sweep`] (`aterm harness upgrade`): one step for every
//! session, the continuation typed in the same step.
//!
//! Every fact comes from a source that cannot be stale for its process:
//! Claude's session file (checked against the kernel start time, so a recycled
//! pid never reads as the session), `KERN_PROCARGS2` for argv and any tab id
//! (`ATERM_PARENT_SESSION_ID`), the host's owner-only PTY foreground-group
//! roster where macOS omits the environment, the process table (what runs
//! under the agent, its terminal, whether it is its shell's job), and the
//! tab's screen and hold over the control socket (a person's `human_ms=`
//! included). What cannot be read is a wait, never a guess; nothing types or
//! signals unless [`super::upgrade::next_step`] says go, and every step taken
//! is one ledger line.
//!
//! State is one small JSON file per conversation under
//! `<harness state>/upgrade/`, so the next step resumes where the last
//! stopped, including across the agent's own exit while that restart is
//! fresh and its shell lives ([`super::relaunch::expired`]). The same files
//! are the owner's view and carry the owner's word (`upgrade_status`:
//! [`rows`], [`View`], [`ask`]; `aterm harness upgrade <sid>
//! --now|--defer|--skip`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_json::{Map, Value};
use aterm_types::control_verbs::ScreenGen;

use super::relaunch::{
    Launch, NoPlan, Plan, PromptLook, PromptMark, Relaunch, RelaunchLineError, STALE_S, await_new,
    carry_on, confirm, end_asked_at, expired, fallback_carried, fallback_origin, plan, recorded,
    relaunch, relaunch_of, resume_mode, screen_mode, type_relaunch_line_with, unplanned,
    wait_until,
};
use super::upgrade::{self, Candidate, Facts, Phase, Request, SessionFile, Source, Step, Version};
use super::upgrade_catalog::{self as catalog, Baked};
use super::upgrade_models::{self as models, ModelRecord, ModelVerdict, Priority};
use crate::RelayClient;
use crate::supervise::approvals;
use crate::supervise::policy::server_fences_gen;
use crate::supervise::run::composer_guard;
use crate::supervise::screen::HumanInput;

#[path = "upgrade_status.rs"]
mod upgrade_status;
pub(super) use upgrade_status::{last_command, wait_words_of};

#[path = "upgrade_codex_drive.rs"]
mod codex;

#[path = "upgrade_watch.rs"]
mod watch;
#[cfg(test)]
pub(crate) use upgrade_status::pin_awake_since;
#[cfg(any(test, feature = "spec-anchors"))]
pub use upgrade_status::progress_projection;
pub(super) use upgrade_status::runs_under;
pub use upgrade_status::{
    ATTENTION_OWNER, Ask, Attention, CLAIMED, DIGEST_AFTER_S, HUNG_EXIT_AFTER_S, HeldBy,
    LIMIT_GRACE_S, Level, MOVE_BUDGET_S, NEED_THEN_MAX, NEED_WHY_MAX, NOT_FOREGROUND, NOW_QUIETS_S,
    Need, NeedSeverity, REFUSED_STOPPED, Remedy, Row, STALLED_AFTER_S, SUSPENDED_AFTER_S, TELL_S,
    UNWATCHED_AFTER_S, View, WAKE_CATCH_UP_S, WAKE_GRACE_S, ask, ask_for, awake_since,
    elapsed_words, refusal_sentence, refused_stopped, rows, status_rows, system_awake_since,
    unwatched_since, word_marker,
};
pub use watch::{Watched, Watcher, watch};
#[cfg(all(test, unix))]
use watch::{watch_in, watch_with};

/// The world and mutants of `HarnessUpgradeLadder`
/// (`aterm_spec::derive::harness_upgrade_ladder_model`): the lane's actions
/// bind where the real rule runs (`upgrade::rung`, `upgrade::gate`,
/// `St::still`, `upgrade_codex::daemon_turn`, `upgrade_codex::daemon_step`,
/// projected by [`ladder_projection`]); these are the agent's, daemon's,
/// person's and clock's steps for the Tier-1 bind
/// (`codex::ladder_tests::the_real_ladder_is_the_derived_machine`), plus the
/// `Buggy = 1` members. Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnStarts",
        reason = "A turn of this client's own root starting: Codex's or Claude Code's step. The Tier-1 bind reads it busy (or streaming under a goal) exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnEnds",
        reason = "A turn of this client's own root ending: the agent's step. The Tier-1 bind reads its end exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "HiddenStarts",
        reason = "A turn the screen never draws starting (a subagent's, or a root's with this client the daemon's only Codex): the daemon's step. The Tier-1 bind puts a running thread on the daemon exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "HiddenEnds",
        reason = "That hidden turn ending: the daemon's step. The Tier-1 bind idles the thread exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "OtherStarts",
        reason = "A turn in another session on the same daemon starting: another tab's step. The Tier-1 bind runs another root with another client attached exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "OtherEnds",
        reason = "That other session's turn ending: another tab's step. The Tier-1 bind idles it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "GoalPursued",
        reason = "Codex's footer starting to say a goal is pursued: the vendor's step. The Tier-1 bind draws `Pursuing goal (…)` exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "GoalPaused",
        reason = "Codex's footer no longer saying a goal is pursued — paused, achieved or cleared, by a person, by the goal's own end, or by Codex taking aterm's typed `/goal pause` (that pause bound in HarnessUpgradeGoalPause::GoalPause at `goal_step`): the footer's change is the vendor's step. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Repaint",
        reason = "A redraw with no new words (a footer clock, a goal's counter): the vendor's screen. The Tier-1 bind repaints the counter exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Still",
        reason = "The screen's own quiet standing: the vendor's screen and the clock. The Tier-1 bind holds the screen still exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Type",
        reason = "A person's keystroke in the tab: a person's step. The Tier-1 bind stamps the keystroke exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysPause",
        reason = "A person pausing past the keys gap but still near: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysAway",
        reason = "A person leaving the tab past the grace: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftTyped",
        reason = "A draft left in the composer: a person's step. The Tier-1 bind types one exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftCleared",
        reason = "That draft cleared: a person's step. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxUp",
        reason = "A dialog drawn over the composer: the vendor's step. The Tier-1 bind draws a measured box exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxDown",
        reason = "That dialog answered: the vendor's step. The Tier-1 bind takes it down exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MarkChanges",
        reason = "The input line's mark changing between `›` and 0.158.0's `»`: the vendor's build. The Tier-1 bind swaps the mark exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "SeqSettle",
        reason = "Buggy=1 negative control only; a look at a pause the ladder counts that waits `settling` for a screen that only repainted (the incident's 120 `wait:settling`), which `St::still` does not"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "AttendedWithoutLadder",
        reason = "Buggy=1 negative control only; a person merely near the tab holding the move at every rung, where `gate` narrows the grace to the keys gap from KeysOnly"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BlindReader",
        reason = "Buggy=1 negative control only; the reader blind to 0.158.0's `»` input line, which the one Codex composer reader reads"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "OtherTurnHolds",
        reason = "Buggy=1 negative control only; every thread of the daemon holding the client, where `daemon_turn` lets a placed session's turn go"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "PinDropsDue",
        reason = "Buggy=1 negative control only; a current client on an unpinned daemon let go, where `daemon_step` keeps it due for the pin"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "NowWaivesThePerson",
        reason = "Buggy=1 negative control only; the owner's `--now` waiving the person outright, where `gate` keeps the keys gap at the last rung"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverADraft",
        reason = "Buggy=1 negative control only; a move typed over a draft, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverABox",
        reason = "Buggy=1 negative control only; a move typed over a dialog, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveMidTurn",
        reason = "Buggy=1 negative control only; a daemon-mode client ended while its own thread runs a turn, which `daemon_turn` holds"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "PlacedUnderAGoal",
        reason = "Buggy=1 negative control only; the still run placing this client's own goal turn, where nothing is placed under a goal"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "PlacedAsItRan",
        reason = "Buggy=1 negative control only; the still run placing whatever ran through two looks, where only another session's root with another client attached is placed"
    )
)]
#[doc(hidden)]
pub fn ladder_environment() {}

/// The ladder's projection (`aterm_spec::derive::harness_upgrade_ladder_model`;
/// `#[refines]` anchors on `upgrade::gate`, `upgrade::rung`, `St::still`,
/// `upgrade_codex::daemon_turn`, `upgrade_codex::daemon_step`): the model
/// variables a look's real [`Facts`] stand for.
///
/// * `rung`: 0 Prefer … 3 Land;
/// * `work`: a turn the gate waits for (Claude's status or busy footer, or a
///   Codex daemon thread the facts hold, [`Facts::daemon_turn`]: its own
///   conversation's, subagents included, or one not placed elsewhere);
/// * `quiet`: the screen's own settle;
/// * `seen`: the reader's run of idle looks at the same words (2 once it stood
///   [`upgrade::QUIET_S`]);
/// * `keys`: 0 nobody near, 1 a person within the grace who has not typed
///   within [`upgrade::KEYS_GAP_S`], 2 one who has;
/// * `draft`, `dialog`.
///
/// Another session's root turn is the model's `other`, which no fact of this
/// client carries; the footer's goal is the model's `goal`, carried only
/// through what it holds ([`upgrade::DaemonTurn::Goal`], an unplaced turn).
/// Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[doc(hidden)]
#[must_use]
pub fn ladder_projection(f: &Facts) -> std::collections::BTreeMap<&'static str, i64> {
    let flag = |b: bool| i64::from(b);
    let quiet = upgrade::quiet_s();
    std::collections::BTreeMap::from([
        (
            "rung",
            match f.rung {
                upgrade::Rung::Prefer => 0,
                upgrade::Rung::Settled => 1,
                upgrade::Rung::KeysOnly => 2,
                upgrade::Rung::Land => 3,
            },
        ),
        (
            "work",
            flag(f.status != "idle" || f.busy_footer || f.daemon_turn.wait().is_some()),
        ),
        ("quiet", flag(f.status_age_s >= quiet && f.quiet_s >= quiet)),
        ("seen", if f.still_s >= quiet { 2 } else { 0 }),
        ("keys", if f.typing { 2 } else { flag(f.attended) }),
        ("draft", flag(!f.composer_empty)),
        ("dialog", flag(f.approval_box)),
    ])
}

/// THE WORLD AND THE MUTANTS of `HarnessUpgradeProgress`
/// (`aterm_spec::derive::harness_upgrade_progress_model`, design record
/// 2026-09-28 §4): its lane's actions are bound where the real code runs
/// (`upgrade::next_step`, `upgrade::break_step` and the watch's `watch_with`,
/// all projected by [`progress_projection`]); these are the session's, a
/// person's, the agent's and the clocks' steps, each one the Tier-1 binds
/// build, and the `Buggy = 1` members — one of which was main until
/// 2026-09-29, the model's gap `RecordedWordStands` (`upgrade::held_point`,
/// `upgrade::NEXT_POINT`), closed that day. What the owner is TOLD of a
/// record is not the machine's: `Row::attention` is bound over the same
/// states in `upgrade_progress_tests.rs`. Compiled only for proof and test
/// builds: nothing calls it.
#[cfg(any(test, feature = "spec-anchors"))]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "TurnRuns",
        reason = "A turn of the agent's own starting: Claude Code's step (the loop's `NotIdle`/`NotBusy`, no point offered). The Tier-1 binds read it as a busy status and footer (the watch's read) exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "BreakSettles",
        reason = "A settled break of the agent's own background work: the agent's and its supervisor loop's (`host_steps_in_background`). The Tier-1 binds offer the break step (`background_point`) there, and read the screen as the watch's dry read does."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "IdlePoint",
        reason = "An idle point: the agent's turn ending and the loop offering it (`host_steps_here`). The Tier-1 binds read an idle, settled screen exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WallStands",
        reason = "A usage limit's notice on the screen: Claude Code's and the API's step. The Tier-1 binds read `Facts::limited` exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PersonHolds",
        reason = "A person's box or draft standing: a person's step (the loop's `Prompt` guard). The Tier-1 binds read an approval box exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "ResetPasses",
        reason = "The wall's named reset passing, its notice still on the screen: the clock's and the vendor's step, which the record keeps as the transcript names it (`St::limit_until`) and the view only READS (`Row::limit_passed`, `LIMIT_GRACE_S`). The Tier-1 view bind builds it through `limit_until`."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkStarts",
        reason = "A process starting under the agent: the agent's step. The Tier-1 reducer bind names it in `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkEnds",
        reason = "That process ending: the agent's step. The Tier-1 reducer bind empties `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Answers",
        reason = "The agent's READY to the round's notice: the agent's step (`heard`). The Tier-1 reducer bind hands the reducer `ready` exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Window",
        reason = "The round's clock running out: the clock's step (`REASK_S` since the notice or the pending round's progress, `RETRY_S` since the stop), which the reducer and the view only READ. The Tier-1 binds build it through `at_s`, `failed_s`/`retry_at` and `progress_at`."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "NoLookWithoutAPoint",
        reason = "Buggy=1 negative control only; main before `136ff0735`, whose view named no `watch_at` instant, so nothing looked at a record off a point. The host's wake at `watch_at` is bound by the existing T1-h tests."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RecordedWordStands",
        reason = "Buggy=1 negative control only; main until 2026-09-29, whose watch recorded a word only for a read that waits, so a read that would act left the last word. The real read now says what holds the point of a give-up or a re-arm (`upgrade::held_point`) and the watch records it, else `upgrade::NEXT_POINT` (`upgrade_watch::look_word`), for a Claude Code record: a Codex record's act read records nothing still, and the settled-break half is a re-decision of the law's truth, not a change of code (`derived_harness_upgrade_progress_break_is_a_redecision`). The Tier-1 binds replay the old look against the real read, and `derived_harness_upgrade_progress_gaps_are_closed_on_main` pins the law it breaks."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WatchTypes",
        reason = "Buggy=1 negative control only; the watch with hands, which `tools/grep_guard.sh` H1 fences lexically and the Tier-1 watch tests refuse at the control socket (zero input requests)."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RestartOverWork",
        reason = "Buggy=1 negative control only; the restart over processes under the agent, where the real `gate_restart` waits `background`."
    )
)]
#[doc(hidden)]
pub fn progress_environment() {}

/// The world and mutants of `HarnessUpgradeGoalPause`
/// (`aterm_spec::derive::harness_upgrade_goal_pause_model`): the lane's actions
/// bind where the real rule runs (`upgrade_codex::goal_step`,
/// `upgrade_codex::goal_owed`, and `upgrade_codex::daemon_turn` for its move,
/// projected by [`goal_projection`]); these are Codex's, a person's, the
/// supervisor's switch's and the clock's steps for the Tier-1 bind
/// (`codex::ladder_tests::goal::the_real_goal_pause_is_the_derived_machine`),
/// plus the `Buggy = 1` members.
///
/// The clock's `Advance` (Land rung), `MoveOverdue` (the hold's bound) and
/// `Rested` (the rest) are thresholds passing, not lane steps: the lane only
/// reads the clock (`St::rung_at`, `goal_hold_past_bound`, `goal_rest_until`),
/// and the Tier-1 bind ages each look through that reader, so none carries a
/// `#[refines]`. Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "Advance",
        reason = "The ladder's clock reaching its Land rung, or the owner's `--now` (a person's word, the goal look's `hurried`): the clock and a person, which no step of the lane makes; the lane only READS the rung (`St::rung_at` over `upgrade::rung`, whose thresholds are bound as HarnessUpgradeLadder::Advance). The Tier-1 bind builds `land` through the real `St::rung_at` at every rung, `--now` aside, and Advance is enabled exactly where it stands short of Land."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "TurnEnds",
        reason = "A goal turn ending, and the next beginning at its head at once while the goal is pursued: Codex's step. The Tier-1 bind reads the goal thread's rollout exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "TurnWorks",
        reason = "A goal turn's first work landing (a reasoning item, a message, a tool call): Codex's step. The Tier-1 bind writes the rollout past its head exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "PauseTakes",
        reason = "Codex taking a typed `/goal pause` (its footer then reads `Goal paused`): the vendor's step. The Tier-1 bind draws the paused footer exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "PersonResumes",
        reason = "A person resuming the goal: a person's step. The Tier-1 bind draws `Pursuing goal` and stamps their hand exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "PersonPauses",
        reason = "A person pausing the goal: a person's step. The Tier-1 bind draws `Goal paused` and stamps their hand exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "SwitchOpens",
        reason = "A save-then-wait switch opening on the tab: the supervisor's press, never this lane's (bound in its own machine, SupervisorCodexRateNudge::PressSwitch at `approval::rate_nudge` and `TurnEndState::open_switch`); this lane only reads the switch's ledger row (`switch_open`, `approvals::open_wind_down`). The Tier-1 bind opens it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "SwitchCloses",
        reason = "That switch closing — at its reset's resume, or a person's own turn: the supervisor's step, never this lane's (bound in SupervisorCodexRateNudge, where its phase returns free: Restore and Resume at `wind_down_act`, a person's turn at `observe`); this lane only reads the ledger row that closes it (`approvals::open_wind_down`: `done` or `released`). The Tier-1 bind closes it exactly there."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "MoveOverdue",
        reason = "Another floor of the ladder's (a draft, a box, a keystroke, other work) holding the move past the hold's bound: the ladder's floors and the clock, which no step of the lane makes; the lane only READS the bound (`goal_hold_past_bound`, `GOAL_HOLD_BOUND_S`, which makes `goal_step`'s resume due, bound as GoalResume). The Tier-1 bind ages the paused hold past `GOAL_HOLD_BOUND_S` exactly there: the aged look projects onto the model's successor (`overdue` through that same reader), and the real `goal_step` decides there, `Resume(\"bound\")`, what the model enables at it."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "Rested",
        reason = "The rest after a hold that ended without its move running out: the clock, which no step of the lane makes; the lane only READS it (`goal_rest_until`, which keeps `goal_step`'s next pause off, bound as GoalPause). The Tier-1 bind ages the hold past `GOAL_REST_S` exactly there, and the real `goal_rest_until` ends the rest."
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "GoalHoldsTheTab",
        reason = "Buggy=1 negative control only; the rule before the owner's decision, a goal holding its tab like any turn, where `goal_step` pauses it at Land"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "EscMidWork",
        reason = "Buggy=1 negative control only; the Esc pressed past a goal turn's head, where `goal_step` presses it only at a head"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "PauseUnderSwitch",
        reason = "Buggy=1 negative control only; the pause made under an open save-then-wait switch, where `goal_step` makes none"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "PauseAtFirstSight",
        reason = "Buggy=1 negative control only; the pause made before the ladder's Land rung, where `goal_step` waits for it"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "ResumeTwice",
        reason = "Buggy=1 negative control only; a resume made again over a goal a person paused after aterm's resume, where `goal_step` owes nothing once resumed"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "ResumeOverPerson",
        reason = "Buggy=1 negative control only; the goal resumed after a person took it from aterm, where `goal_step` releases it"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "LetGoPaused",
        reason = "Buggy=1 negative control only; the host letting a current tab go with its goal left paused, where `goal_owed` keeps it due"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "HarnessUpgradeGoalPause",
        action = "ContinuePaused",
        reason = "Buggy=1 negative control only; the supervisor typing a turn onto the paused thread, where every wait the lane words at a held goal is `goal-held`, which owns the turn end (`goal_wait_word`, `owns_turn_ends`)"
    )
)]
#[doc(hidden)]
pub fn goal_environment() {}

/// The goal pause's projection
/// (`aterm_spec::derive::harness_upgrade_goal_pause_model`; `#[refines]`
/// anchors on `upgrade_codex::goal_step`, `upgrade_codex::goal_owed`,
/// `upgrade_codex::daemon_turn`): the model variables one real look
/// ([`super::upgrade_codex::GoalLook`]) stands for: `behind`, `land`, `goal`
/// (1 pursued, 2 paused), `turn` (0 none, 1 at its head, 2 past it), `held`
/// (0 none or rested, 1 pausing, 2 paused, 3 resumed, 4 released), `overdue`
/// (the move will not come now: abandoned, or the hold past its bound,
/// [`super::upgrade_codex::goal_hold_past_bound`]; either makes the resume
/// due), `switch` and `person`. Proof and test builds only.
#[cfg(any(test, feature = "spec-anchors"))]
#[doc(hidden)]
#[must_use]
pub fn goal_projection(
    look: &super::upgrade_codex::GoalLook,
) -> std::collections::BTreeMap<&'static str, i64> {
    use super::goal_hold::{Owner, Stage};
    let flag = |b: bool| i64::from(b);
    let held = match look.hold.filter(|h| h.owner == Owner::Upgrade) {
        Some(h) if h.stage == Stage::Pausing => 1,
        Some(h) if h.stage.owes() => 2,
        Some(h) if super::upgrade_codex::goal_rest_until(h, look.now).is_some() => {
            if h.stage == Stage::Released { 4 } else { 3 }
        }
        Some(h) if h.stage == Stage::Resumed && !look.behind => 3,
        _ => 0,
    };
    std::collections::BTreeMap::from([
        ("behind", flag(look.behind)),
        ("land", flag(look.land)),
        (
            "goal",
            if look.goal == Some(aterm_phase::codex::CodexGoal::Pursuing) {
                1
            } else {
                2
            },
        ),
        (
            "turn",
            match look.head {
                Some(true) => 1,
                _ if look.goal_holds => 2,
                _ => 0,
            },
        ),
        ("held", held),
        (
            "overdue",
            flag(
                look.abandoned.is_some()
                    || look.hold.is_some_and(|h| {
                        h.owner == Owner::Upgrade
                            && h.stage == Stage::Paused
                            && super::upgrade_codex::goal_hold_past_bound(h, look.now)
                    }),
            ),
        ),
        ("switch", flag(look.switch)),
        ("person", flag(look.person_since)),
    ])
}

/// What one sweep is told.
#[derive(Clone, Debug)]
pub struct Opts {
    /// The user's home (Claude's `~/.claude`, the native install, the hooks).
    pub home: PathBuf,
    /// The harness state directory; this module writes under `upgrade/` in it.
    pub state: PathBuf,
    /// A control socket to use instead of the one each tab's id resolves to.
    pub sock: Option<String>,
    /// Only this tab (`s-<hex>`), when set.
    pub only_sid: Option<String>,
    /// Plan and print; type, signal and write nothing.
    pub dry_run: bool,
    /// `[harness] human_grace_s`: a tab a person typed into this recently
    /// (`status` `human_ms=`) is held: nothing typed, agent not signalled.
    pub human_grace_s: u32,
    /// A supervisor loop takes the relaunched agent (the window's host): a
    /// relaunch ends once the new process holds the conversation (`adopted`),
    /// and the continuation is typed at the loop's next idle point
    /// ([`super::relaunch::resume`]), after the loop has answered whatever the
    /// new process opened with (trust dialog, resume choice, model consent).
    /// `false` (the hand-run verb, no loop) types it in the same step.
    pub hand_back: bool,
    /// The step is taken at a break of the agent's own background work
    /// ([`crate::supervise::IdleHost::at_background`]), not an idle point.
    /// Only a line that ends nothing may be typed: a notice (the first, or a
    /// re-ask after [`upgrade::REASK_S`]) or the release line a give-up, void
    /// or stop owes (`release_next`). Past [`upgrade::MAX_ASKS`] the upgrade
    /// gives up here ([`upgrade::Facts::background_point`],
    /// [`upgrade::next_step`]). Nothing is ended, no restart in flight is
    /// carried on, no Codex daemon is moved.
    pub background: bool,
    /// The aterm state root under which each tab's supervisor keeps its
    /// approval ledger (`<root>/drive/<sid>.jsonl`): what its loop typed is the
    /// harness's own, no task ([`upgrade::TaskScan`], [`supervisor_typed`]).
    /// `None`: every unmarked turn reads as someone's.
    pub aterm_state: Option<PathBuf>,
    /// `[harness] upgrade_ask`: the upgrade asks first, typing its notice and
    /// restarting only on READY (the flow [`super::upgrade`] describes).
    /// `false`, the default: nothing is typed before the move, which comes only
    /// at a genuinely idle point ([`upgrade::Facts::quiet`]), and nothing is
    /// typed after the resume either ([`St::quiet`]).
    pub ask: bool,
    /// TWO OR MORE CLAUDE CODE ACCOUNTS ARE LISTED for this home (the
    /// accounts roster beside `aterm.toml`, [`super::accounts::several_listed`];
    /// `docs/DESIGN-claude-accounts-2026-10-01.md` §1, §9): the home keeps
    /// ONE profile, and every Claude Code start writes ITS login's model
    /// caches into it (READ in the 2.1.286 bundle) — so under two logins the
    /// served catalog and the access caches are whichever login started
    /// last, and say nothing about the session being judged. They are then
    /// NOT READ: the model evidence is the unknown ([`model_evidence`]), and
    /// a model is judged by the build's own catalog alone. `false` — one
    /// login, the packaged state — reads them as ever.
    pub several_logins: bool,
}

/// CLAUDE CODE'S OWN FILES ABOUT MODELS, for one sweep's judgments
/// ([`models::read_evidence`]) — or THE UNKNOWN ([`models::Evidence`]'s
/// default: no served catalog, no denial, no disabled option, no
/// announcement) while two or more accounts are listed
/// ([`Opts::several_logins`]). NEVER OBSERVED with two real logins: that the
/// caches then follow the login that started last is read in the bundle, not
/// measured.
fn model_evidence(opts: &Opts) -> models::Evidence {
    if opts.several_logins {
        return models::Evidence::default();
    }
    models::read_evidence(&opts.home.join(".claude"), &opts.home.join(".claude.json"))
}

/// What a sweep did with one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The agent's pid.
    pub pid: u32,
    /// The aterm tab (`s-<hex>`), or `-`.
    pub tab: String,
    /// The conversation.
    pub session: String,
    /// The running version.
    pub from: String,
    /// The target (`<version>(<source>)`), or `-`.
    pub to: String,
    /// The step taken or the reason for waiting.
    pub step: String,
}

impl Report {
    /// Whether this report is something the sweep did or recorded (typed,
    /// signalled, relaunched, gave up, refused, held back), as opposed to a
    /// wait, a skip or `current`.
    #[must_use]
    pub fn is_act(&self) -> bool {
        !(self.step == "current"
            || self.step.starts_with("wait:")
            || self.step.starts_with("skip:")
            || self.step.starts_with("busy:")
            || self.step.starts_with("would-"))
    }

    /// One line: `upgrade pid=… tab=… session=… from=… to=… step=…`.
    #[must_use]
    pub fn line(&self) -> String {
        self.line_as("upgrade")
    }

    /// The same line under another name (`relaunch` for a relaunch on exit,
    /// `carry-on` for a relaunched agent's continuation), so a log never
    /// calls a relaunch an upgrade.
    #[must_use]
    pub fn line_as(&self, kind: &str) -> String {
        format!(
            "{kind} pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

thread_local! {
    /// The second a pass on this thread reads as now ([`at_second`]): `None`,
    /// the wall clock.
    static GIVEN_NOW: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Now, in unix seconds: the second this thread's pass was given
/// ([`at_second`]), else the wall clock.
pub(super) fn now_s() -> u64 {
    GIVEN_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| aterm_time::unix_secs().unwrap_or(0))
}

/// `f`, with every [`now_s`] it reads on this thread answering `now`: a pass
/// handed a time (the watch, [`watch::watch_with`]) decides at that time
/// throughout, its read included, never at the wall clock's. The previous
/// second is restored after, a panic included.
pub(super) fn at_second<R>(now: u64, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            GIVEN_NOW.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(GIVEN_NOW.with(|c| c.replace(Some(now))));
    f()
}

// ---------------------------------------------------------------- processes

/// `ps` in the C locale and UTC, the rendering Claude's `procStart` uses.
///
/// This file's `ps` children are built through `aterm_uds::spawnfd::command`: on Windows
/// they run only when such a tool is on `PATH` (Git for Windows, MSYS), a
/// console image, and the sweep runs in the window's harness host, which has no
/// console, so a bare one would flash a console window.
fn ps(args: &[&str]) -> Option<String> {
    let out = aterm_uds::spawnfd::command("ps")
        .args(args)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The kernel's start time of `pid`, rendered as Claude renders `procStart`.
pub(super) fn kernel_start(pid: u32) -> Option<String> {
    let s = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()])?);
    (!s.is_empty()).then_some(s)
}

pub(super) fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 checks existence and permission; nothing is delivered.
        let rc = unsafe { libc::kill(p, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    // Windows: the workspace's one `OpenProcess` probe, the same contract
    // (a pid that exists but is not ours reads alive, like `EPERM`).
    #[cfg(not(unix))]
    {
        aterm_uds::process::pid_alive(pid)
    }
}

/// Whether `pid` leads its terminal: alive, and its process group is the
/// terminal's foreground group. The supervising loop's check that the Codex
/// the upgrade relaunched is still the one in its tab
/// (`goal_hold::box_is_upgrades`).
#[must_use]
pub fn leads_its_terminal(pid: u32) -> bool {
    pid > 1 && alive(pid) && ids(pid).is_some_and(|(_, pgid, front)| pgid > 0 && pgid == front)
}

/// `(ppid, pgid, tpgid)` of `pid`.
pub(super) fn ids(pid: u32) -> Option<(u32, i64, i64)> {
    let out = ps(&["-o", "ppid=,pgid=,tpgid=", "-p", &pid.to_string()])?;
    let mut it = out.split_whitespace();
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// A cheap kernel group lookup before any process-table sweep. The focused
/// `ids` read later checks the controlling terminal's foreground group too.
fn process_group(pid: u32) -> Option<i64> {
    #[cfg(unix)]
    {
        let pid = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: getpgid reads one process's kernel identity without changing it.
        let group = unsafe { libc::getpgid(pid) };
        (group > 0).then_some(i64::from(group))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// The whole table: `(pid, ppid, command basename)`.
pub(super) fn table() -> Vec<(u32, u32, String)> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// A `comm` column's basename, a login shell's leading `-` dropped.
fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

/// The whole table with each process's controlling terminal:
/// `(pid, ppid, tty, command basename)`, the tty `??` (macOS) or `?` (Linux)
/// for none.
fn tty_table() -> Vec<(u32, u32, String, String)> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,tty=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let tty = it.next()?.to_string();
            Some((pid, ppid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// The process that owns `pid`'s terminal: walking up from `pid`, the first
/// ancestor not on `pid`'s tty, which opened the pty and holds its master.
/// `(pid, basename)`, or `None` when `pid` has no terminal or the walk breaks.
/// This tells an aterm tab from a pane: a multiplexer (tmux, screen, zellij,
/// dtach) opens its own pty for every pane.
fn terminal_owner(pid: u32, t: &[(u32, u32, String, String)]) -> Option<(u32, &str)> {
    let row = |p: u32| t.iter().find(|(q, _, _, _)| *q == p);
    let tty = row(pid)?.2.as_str();
    if !tty.chars().any(|c| c.is_ascii_digit()) {
        return None; // `??`, `?`, `-`: no controlling terminal at all.
    }
    let mut p = pid;
    for _ in 0..64 {
        let (_, pp, _, _) = row(p)?;
        let (q, _, qtty, name) = row(*pp)?;
        if qtty != tty {
            return Some((*q, name.as_str()));
        }
        p = *q;
    }
    None
}

/// Whether the terminal `owner` ([`terminal_owner`]) opened is an aterm tab:
/// the owner is an aterm process (`aterm` or an `aterm-<tool>` alias), or pid
/// 1 (the aterm that opened the pty handed its tabs to a successor in a live
/// update and exited, reparenting the shell). A multiplexer cannot exit while
/// its pane lives, so pid 1 never stands for one. `script`, `ssh`, `sudo`'s
/// pty and multiplexers read as not the tab's: typing reaches what they show,
/// not the agent.
pub(super) fn owned_by_aterm(owner: (u32, &str)) -> bool {
    owner.0 == 1 || owner.1 == "aterm" || owner.1.starts_with("aterm-")
}

/// The owner's name as one report word: `[A-Za-z0-9._-]`, at most 32 bytes.
fn owner_word(owner: Option<(u32, &str)>) -> String {
    let word: String = owner
        .map(|(_, name)| name)
        .unwrap_or("none")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if word.is_empty() {
        "?".to_string()
    } else {
        word
    }
}

/// Where the agent stands among its terminal's jobs, from its `(pgid, tpgid)`
/// and its parent's `pgid`. A job-control shell starts each job in its own
/// process group and hands it the terminal; a script, `sh -c`, `make`, an IDE
/// task or a shell without job control runs its child in its own group. The
/// restart waits for the parent's `pgid == tpgid` ("the shell took the
/// terminal back"), which for a parent sharing the agent's group already holds
/// while the agent runs, so the relaunch line would go to whatever the
/// launcher runs next, or nowhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    /// A job of a job-control shell, and the terminal's foreground now.
    Foreground,
    /// A job, but not the terminal's foreground right now (suspended, or put in
    /// the background): what is typed would reach the shell, not the agent.
    Background,
    /// Not a job of a job-control shell: nothing will take the terminal back
    /// when it ends. Refused for good, before anything is typed.
    NoJobControl,
}

/// [`Job`] from the numbers `ps` reports.
fn job(agent: u32, agent_pgid: i64, tpgid: i64, parent_pgid: i64) -> Job {
    if agent_pgid != i64::from(agent) || parent_pgid == agent_pgid {
        Job::NoJobControl
    } else if tpgid != agent_pgid {
        Job::Background
    } else {
        Job::Foreground
    }
}

/// [`job`] read from the kernel now, with the parent it was read against.
fn job_of(agent: u32) -> Option<(Job, u32)> {
    let (parent, pgid, tpgid) = ids(agent)?;
    let (_, parent_pgid, _) = ids(parent)?;
    Some((job(agent, pgid, tpgid, parent_pgid), parent))
}

/// What one [`Job`] read lets through: `Ok(shell)` when the agent is its
/// shell's foreground job (`shell` the parent read against), else the wait
/// word. Asked before every announce and SIGTERM, so a ctrl-z between looks is
/// caught.
pub(super) fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
    match job {
        Some((Job::Foreground, shell)) => Ok(shell),
        Some((Job::Background, _)) => Err("not-foreground"),
        Some((Job::NoJobControl, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

/// The process facts every act rests on, each read from the kernel when asked.
/// The job is read again before the announce, the SIGTERM and the last look,
/// since the agent can be suspended or backgrounded in between. A trait so a
/// test can script what each read answers and prove that each re-read guards
/// its act.
pub(super) trait Kernel {
    /// Where the agent stands among its terminal's jobs, and its parent
    /// ([`job_of`]).
    fn job(&self, agent: u32) -> Option<(Job, u32)>;
    /// `pid`'s parent, `None` when it cannot be read NOW: the process is gone,
    /// or `ps` could not be run. Never a verdict about the process.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// The program that opened `pid`'s terminal ([`terminal_owner`]).
    fn terminal(&self, pid: u32) -> Option<(u32, String)>;
    /// Whether `shell` has its terminal back (a job-control shell leading its
    /// own process group, which is the terminal's foreground); `None` when it
    /// cannot be read now. The default reads the kernel, as [`Live`] does.
    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        if shell == 0 {
            return None;
        }
        let (_, pgid, tpgid) = ids(shell)?;
        Some(pgid == i64::from(shell) && pgid == tpgid)
    }
    /// The processes of `procs` (`(pid, basename)` under `agent`,
    /// [`background_procs`]) named for the notice ([`upgrade::running_clause`]).
    /// The default uses the table alone (no age, no command); [`Live`] reads
    /// both from `ps`.
    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        let _ = agent;
        procs
            .iter()
            .map(|(pid, name)| upgrade::Held {
                pid: *pid,
                name: name.clone(),
                age_s: 0,
                command: String::new(),
            })
            .collect()
    }
}

/// The kernel itself: `ps` and `kill(pid, 0)`.
pub(super) struct Live;

impl Kernel for Live {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        job_of(agent)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        if !alive(pid) {
            return None;
        }
        ids(pid).map(|(ppid, _, _)| ppid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        terminal_owner(pid, &tty_table()).map(|(owner, name)| (owner, name.to_string()))
    }

    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        held_of(agent, procs)
    }
}

/// Shells and the keep-awake helper: work running under the agent (a Bash
/// tool, background shell, build, a command's `caffeinate`). Not MCP servers
/// (a resume restarts them) nor the agent's own keep-awake ([`OWN_KEEP_AWAKE`]).
const BACKGROUND: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "caffeinate",
];

/// The agent's own keep-awake is not work: Claude Code starts `caffeinate -i
/// -t 300` as its own child every turn and stops it some 30 s after the turn
/// ends. It holds nothing a restart could lose (the relaunched agent starts
/// its own), and `-t` ends it anyway. Only as the agent's direct child: a
/// `caffeinate` under a Bash tool's shell or any other helper stays work.
const OWN_KEEP_AWAKE: &str = "caffeinate";

/// The background work under `pid`, by basename: every [`BACKGROUND`] program
/// at any depth, except the agent's own keep-awake ([`OWN_KEEP_AWAKE`], its
/// direct child), whose descendants are still walked. The one reading of
/// "something runs under the agent" for every restart (the live upgrade's
/// gate and last look, the model-only restart, a Codex embedded session).
pub(super) fn background(pid: u32, t: &[(u32, u32, String)]) -> Vec<String> {
    background_procs(pid, t)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// [`background`] with each process's pid: `(pid, basename)`, in the order
/// the walk met them.
pub(super) fn background_procs(pid: u32, t: &[(u32, u32, String)]) -> Vec<(u32, String)> {
    let mut frontier = vec![pid];
    let mut found = Vec::new();
    let mut seen = 0;
    while let Some(p) = frontier.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        for (c, pp, name) in t {
            if *pp == p {
                frontier.push(*c);
                let own_keep_awake = p == pid && name == OWN_KEEP_AWAKE;
                if BACKGROUND.contains(&name.as_str()) && !own_keep_awake {
                    found.push((*c, name.clone()));
                }
            }
        }
    }
    found
}

/// The word [`live_background`] answers with when the process table cannot be
/// read: one entry of work, so a gate that asks "does anything run?" waits.
pub(super) const UNREADABLE_TABLE: &str = "ps-unreadable";

/// [`background`] over the live process table, failing closed: a real table
/// always holds this process, so an empty one (`ps` failed) reads as
/// [`UNREADABLE_TABLE`] (something may run) rather than "nothing runs".
pub(super) fn live_background(pid: u32) -> Vec<String> {
    background_or_unreadable(pid, &table())
}

/// [`live_background`] over a table already read: an empty one reads as
/// [`UNREADABLE_TABLE`].
pub(super) fn background_or_unreadable(pid: u32, t: &[(u32, u32, String)]) -> Vec<String> {
    if t.is_empty() {
        return vec![UNREADABLE_TABLE.to_string()];
    }
    background(pid, t)
}

/// What runs under the agent, named for the notice
/// ([`upgrade::running_clause`], [`Kernel::describe`]): each of `procs` with
/// its age from one `ps` read, plus its command when its parent is `agent`
/// (the agent's own Bash-tool shell, [`upgrade::Held::command`]). Processes
/// gone since the walk are left out; an unreadable read names nothing.
pub(super) fn held_of(agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
    if procs.is_empty() {
        return Vec::new();
    }
    let list = procs
        .iter()
        .map(|(p, _)| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Some(out) = ps(&["-o", "pid=,ppid=,etime=,args=", "-p", &list]) else {
        return Vec::new();
    };
    procs
        .iter()
        .filter_map(|(p, name)| {
            let line = out
                .lines()
                .find(|l| l.split_whitespace().next() == Some(p.to_string().as_str()))?;
            let mut it = line.split_whitespace();
            let _ = it.next();
            let ppid: u32 = it.next()?.parse().ok()?;
            let age_s = parse_etime(it.next()?)?;
            let args = it.collect::<Vec<_>>().join(" ");
            Some(upgrade::Held {
                pid: *p,
                name: name.clone(),
                age_s,
                command: if ppid == agent {
                    command_head(&args)
                } else {
                    String::new()
                },
            })
        })
        .collect()
}

/// THE GIVE-UP'S LEDGER ROW, for the owner: why it stopped asking, what it
/// owes the agent now ([`upgrade::GAVE_UP`]: the release, and a late READY
/// still honoured before it), when the next round asks again
/// ([`upgrade::RETRY_S`]), and what still ran under the agent then
/// ([`upgrade::held_list`]). The release is OWED here, not typed: the
/// `released:<why>` row that follows says it was (2026-09-27: this row said
/// "releases the agent" over a release a break never typed). The Codex lane
/// types no release and hears no READY once it stopped: its row says so
/// little. Under the QUIET flow (`quiet`, [`upgrade::Facts::quiet`]) a round
/// gives up only when an older aterm (or `upgrade_ask`) announced it and the
/// agent's own work held its restart past the notice's window
/// ([`upgrade::quiet_step`], round eight, #4) — it asked nothing again — and
/// the round after it moves the session at its next genuinely idle point,
/// asking nothing.
pub(super) fn gave_up_words(agent: upgrade::Agent, held: &[upgrade::Held], quiet: bool) -> String {
    let span = upgrade::span(upgrade::RETRY_S);
    let why = match (agent, quiet) {
        (upgrade::Agent::Claude, false) => format!(
            "no READY answer it could act on after the last notice: this round stops asking, \
             owes the agent the line that releases it (typed wherever a notice may go, a break \
             of its own work included), and still honours a READY that comes before that line; \
             a new round asks again in {span}"
        ),
        (upgrade::Agent::Claude, true) => format!(
            "the agent's own work held the restart past the window of the notice an earlier \
             build typed, and the quiet upgrade asks nothing again: this round stops, owes the \
             agent the line that releases it (typed wherever a notice may go, a break of its \
             own work included), and still honours a READY that comes before that line; in \
             {span} a new round moves the session at its next idle point, asking nothing"
        ),
        (upgrade::Agent::Codex, false) => format!(
            "no READY answer it could act on after the last notice: this round stops asking; a \
             new round asks again in {span}"
        ),
        (upgrade::Agent::Codex, true) => format!(
            "the agent's own work held the move past the window of the notice an earlier build \
             typed, and the quiet upgrade asks nothing again: this round stops; in {span} a new \
             round moves the session at its next idle point, asking nothing"
        ),
    };
    let list = upgrade::held_list(held);
    if list.is_empty() {
        why
    } else {
        format!("{why}; still running under the agent: {list}")
    }
}

/// When `pid` started, unix milliseconds (to the second: `ps`'s elapsed
/// time), while it runs.
pub(super) fn started_ms(pid: u32) -> Option<u64> {
    let etime = ps(&["-o", "etime=", "-p", &pid.to_string()])?;
    let age_s = parse_etime(etime.trim())?;
    let now_ms = u64::try_from(aterm_time::since_epoch()?.as_millis()).ok()?;
    Some(now_ms.saturating_sub(age_s.saturating_mul(1000)))
}

/// `ps`'s `etime` (`[[dd-]hh:]mm:ss`) in seconds.
pub(super) fn parse_etime(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, etime),
    };
    let parts = clock
        .split(':')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, sec] => (0, *m, *sec),
        [h, m, sec] => (*h, *m, *sec),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

/// The part of a background shell's command line that says what it does.
/// Claude Code's Bash tool runs every command as `zsh -c source <snapshot> …
/// && eval '<command>' < /dev/null && pwd -P >| <cwd file>`, whose opening is
/// the same boilerplate every time, so this takes the `eval` payload with the
/// shell's `'"'"'` quoting undone and `ps`'s `\012` escapes read as spaces.
/// Any other command line is its own head.
pub(super) fn command_head(args: &str) -> String {
    let args = args.replace("\\012", " ");
    let Some((_, payload)) = args.split_once("&& eval '") else {
        return args;
    };
    let payload = payload
        .rsplit_once("' < /dev/null")
        .map_or(payload, |(head, _)| head);
    payload.replace("'\"'\"'", "'")
}

/// Whether `pid` is this process or one of its ancestors: ending it would end
/// the sweep that is driving it.
pub(super) fn is_our_ancestor(pid: u32, t: &[(u32, u32, String)]) -> bool {
    let mut p = std::process::id();
    for _ in 0..64 {
        if p == pid {
            return true;
        }
        match t.iter().find(|(c, _, _)| *c == p) {
            Some((_, pp, _)) if *pp != p && *pp != 0 => p = *pp,
            _ => return false,
        }
    }
    false
}

/// The kernel's working directory of `pid`.
pub(super) fn cwd_of(pid: u32) -> Option<String> {
    aterm_sysprobe::process_cwd(pid).map(|p| p.to_string_lossy().into_owned())
}

/// The executable the kernel mapped for `pid` (its text), canonical.
pub(super) fn exe_of(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_path(pid)
}

/// How long `<exe> --version` may take before it is killed and read as no
/// version. A healthy build answers in well under a second; the bound guards
/// against one that never does (a quarantined download at a first-run dialog,
/// a wrapper waiting on a lock), which would otherwise hold `sweep.lock` and
/// every Claude upgrade on the machine.
const VERSION_WAIT: Duration = Duration::from_secs(5);

/// What one `<exe> --version` probe found ([`version_within`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Probed {
    /// It answered, naming this version.
    Version(Version),
    /// It finished within the bound and named no version (failed, could not
    /// run, or printed something else): a property of the file, remembered
    /// while it is unchanged ([`cached_version`]).
    NoVersion,
    /// It did not finish within the bound (the child, or something holding
    /// its output, still ran) and its process group was killed: as much about
    /// the moment as the file, so it is asked again on a back-off
    /// ([`VERSION_RETRY_FIRST`]), never written off.
    TimedOut,
}

/// What `<exe> --version` says ([`Probed`]): its leading version, no version
/// (a child that fails or prints none), or no answer within [`VERSION_WAIT`].
fn version_of(exe: &Path) -> Probed {
    version_within(exe, VERSION_WAIT)
}

/// [`version_of`] with an explicit bound covering the whole probe: the
/// child's exit and the read of its output. The child runs in its own process
/// group and its output is read on a thread, so a chatty child cannot stall
/// on a full pipe. The read ends only when the last writer closes the pipe,
/// which may be a grandchild left running (`sleep 12 & echo 2.1.282; exit
/// 0`), so the read is waited for against the same deadline; at the deadline
/// the group is killed and the probe answers [`Probed::TimedOut`] without
/// waiting for the reader (which ends when the group's last writer dies, or,
/// for a writer that left via `setsid`, whenever it exits).
fn version_within(exe: &Path, limit: Duration) -> Probed {
    let deadline = Instant::now() + limit;
    // From the window's harness host (no console) a bare `claude --version`
    // opened a visible console window on Windows; `spawnfd::command` decides
    // its console.
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg("--version")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let Ok(mut child) = cmd.spawn() else {
        return Probed::NoVersion;
    };
    // Unix kills the probe's whole group at the deadline; Windows has no group kill
    // here, so only the child itself is killed (a grandchild holding the pipe is
    // abandoned with the reader thread, which the timeout already does not wait on).
    #[cfg(unix)]
    let group = libc::pid_t::try_from(child.id()).ok();
    let kill_group = || {
        #[cfg(unix)]
        if let Some(group) = group {
            // SAFETY: a signal to the process group this spawn made
            // (`process_group(0)`: pgid is the child's pid). After the child
            // is reaped it is sent only while a group member still holds the
            // pipe, and a group id is not reused while a member lives.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        kill_group();
        let _ = child.kill();
        let _ = child.wait();
        return Probed::NoVersion;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let _reader = std::thread::Builder::new()
        .name("agent-version-probe".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    let succeeded = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Probed::TimedOut;
            }
        }
    };
    let Ok(out) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
        // The child is gone and something of its group still holds the pipe.
        kill_group();
        return Probed::TimedOut;
    };
    if !succeeded {
        return Probed::NoVersion;
    }
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace()
        .next()
        .and_then(Version::parse)
        .map_or(Probed::NoVersion, Probed::Version)
}

// ---------------------------------------------------------------- targets

/// Where Claude's native installer keeps its builds.
pub(super) fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// The builds a session could move to, read once per sweep.
pub(super) struct Targets {
    /// aterm's managed twin (`<prefix>/agents/claude`).
    managed: Option<Candidate>,
    /// The native install's current build (what `~/.local/bin/claude` names).
    native: Option<Candidate>,
    /// A build whose `--version` did not answer in time is no candidate yet
    /// ([`Asked::Unanswered`]): what it is stays unknown until the version
    /// cache asks again, and a look that found nothing else to take has read
    /// nothing whole ([`UNREAD_VERSION`]).
    unanswered: bool,
}

impl Targets {
    pub(super) fn read(home: &Path) -> Targets {
        Targets::read_with(
            atpkg::store::resolve_configured().map(|layout| layout.agents_dir().join("claude")),
            home,
            cached_version,
        )
    }

    /// [`Targets::read`] of the managed `twin` (`None`: no store), each
    /// build's version asked of `ask` ([`cached_version`] in the product).
    fn read_with(twin: Option<PathBuf>, home: &Path, ask: impl Fn(&Path) -> Asked) -> Targets {
        let unanswered = std::cell::Cell::new(false);
        let version = |exe: &Path| match ask(exe) {
            Asked::Answered(v) => v,
            Asked::Unanswered => {
                unanswered.set(true);
                None
            }
        };
        let managed = twin.and_then(|twin| {
            let version = version(&twin)?;
            Some(Candidate {
                exe: twin,
                version,
                source: Source::Managed,
            })
        });
        let native = std::fs::canonicalize(home.join(".local/bin/claude"))
            .ok()
            .filter(|target| target.starts_with(native_root(home)))
            .and_then(|target| {
                // The native installer names each build's file by its version.
                let version = target
                    .file_name()
                    .and_then(|n| Version::parse(&n.to_string_lossy()))
                    .or_else(|| version(&target))?;
                Some(Candidate {
                    exe: target,
                    version,
                    source: Source::Native,
                })
            });
        Targets {
            managed,
            native,
            unanswered: unanswered.get(),
        }
    }

    /// What this session could move to: the managed twin always, the native
    /// build only for a session that runs native (a managed session is never
    /// moved onto a vendor-updated copy).
    pub(super) fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

/// How long after a timed-out probe ([`Probed::TimedOut`]) the same file is
/// probed again, doubling per consecutive timeout up to
/// [`VERSION_RETRY_MAX`]. A timeout is no verdict on the build, but neither
/// should it be re-run every minute while holding `sweep.lock` for
/// [`VERSION_WAIT`]. Until then [`due`] reads the build as unanswered
/// ([`UNREAD_VERSION`]) and looks in between cost no probe.
const VERSION_RETRY_FIRST: Duration = Duration::from_secs(2 * 60);
const VERSION_RETRY_MAX: Duration = Duration::from_secs(60 * 60);

/// What [`cached_version`] remembers of one file as it was (size, mtime).
#[derive(Clone, Debug)]
enum Seen {
    /// The probe finished: its answer, until the file changes.
    Answered(Option<Version>),
    /// It timed out `streak` times in a row; not asked again before `retry`.
    TimedOut { streak: u32, retry: Instant },
}

/// What [`cached_version`] can say of a build.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    /// Its probe finished: the version it named, or none.
    Answered(Option<Version>),
    /// Its probe did not answer within [`VERSION_WAIT`], this time or the
    /// last: what it is stays unknown until the back-off lets it be asked
    /// again ([`VERSION_RETRY_FIRST`]).
    Unanswered,
}

/// [`version_of`], asked again only when the file's size or mtime moved (the
/// twin is re-rendered whenever its build changes, and the host asks at every
/// worker start and activation notice). A version or clean failure is
/// remembered while the file is unchanged; a timeout is asked again on a
/// back-off ([`VERSION_RETRY_FIRST`]).
fn cached_version(exe: &Path) -> Asked {
    cached_version_with(exe, Instant::now(), version_of)
}

/// [`cached_version`] at `now`, over the `probe` it caches.
fn cached_version_with(exe: &Path, now: Instant, probe: impl FnOnce(&Path) -> Probed) -> Asked {
    type Cache = std::collections::HashMap<PathBuf, (u64, SystemTime, Seen)>;
    static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);
    let Some(meta) = std::fs::metadata(exe)
        .ok()
        .filter(std::fs::Metadata::is_file)
    else {
        return Asked::Answered(None);
    };
    let Ok(modified) = meta.modified() else {
        return Asked::Answered(None);
    };
    let stamp = (meta.len(), modified);
    let before = CACHE.lock().ok().and_then(|cache| {
        cache
            .as_ref()
            .and_then(|c| c.get(exe))
            .filter(|(len, at, _)| (*len, *at) == stamp)
            .map(|(_, _, seen)| seen.clone())
    });
    let streak = match before {
        Some(Seen::Answered(v)) => return Asked::Answered(v),
        Some(Seen::TimedOut { retry, .. }) if now < retry => return Asked::Unanswered,
        Some(Seen::TimedOut { streak, .. }) => streak,
        None => 0,
    };
    let (seen, asked) = match probe(exe) {
        Probed::Version(v) => (Seen::Answered(Some(v.clone())), Asked::Answered(Some(v))),
        Probed::NoVersion => (Seen::Answered(None), Asked::Answered(None)),
        Probed::TimedOut => {
            let wait = VERSION_RETRY_FIRST
                .saturating_mul(1 << streak.min(16))
                .min(VERSION_RETRY_MAX);
            let retry = now.checked_add(wait).unwrap_or(now);
            (
                Seen::TimedOut {
                    streak: streak.saturating_add(1),
                    retry,
                },
                Asked::Unanswered,
            )
        }
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache
            .get_or_insert_with(Cache::new)
            .insert(exe.to_path_buf(), (stamp.0, stamp.1, seen));
    }
    asked
}

// ---------------------------------------------------------------- the tab

pub(super) type Client = RelayClient<aterm_uds::CtlStream>;

pub(super) fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    RelayClient::connect_local_beside(&sock).map_err(|e| format!("{sock}: {e}"))
}

/// The instance's live tab ids.
pub(super) fn roster(c: &mut Client) -> Vec<String> {
    let Ok((head, body)) = c.request_counted("sessions") else {
        return Vec::new();
    };
    if !head.starts_with("OK") {
        return Vec::new();
    }
    body.lines()
        .filter_map(|l| {
            l.split_whitespace()
                .find(|t| t.starts_with("s-"))
                .map(str::to_owned)
        })
        .collect()
}

/// The tabs another supervisor's claim holds, from one `sessions` read: each
/// row's live claim `supervisor=<holder|->` naming a holder other than a
/// window's own host ([`HOST_HOLDER`]; a predecessor's carried claim lapses
/// by itself). For `--status` ([`status_rows`]), which reads from outside the
/// window. Bounded by [`CLAIMS_WAIT`]; `None` for no roster.
pub(super) fn foreign_claims(c: &mut Client) -> Option<std::collections::BTreeSet<String>> {
    c.transport().set_read_timeout(Some(CLAIMS_WAIT)).ok()?;
    let (head, body) = c.request_counted("sessions").ok()?;
    if !head.starts_with("OK") {
        return None;
    }
    Some(
        body.lines()
            .filter_map(|row| {
                let mut words = row.split_whitespace();
                let sid = words.nth(1).filter(|s| s.starts_with("s-"))?;
                // Past the parent, the state and the title, every column is
                // `key=value`: a title never reads as the claim.
                let holder = words.skip(3).find_map(|w| w.strip_prefix("supervisor="))?;
                (holder != "-" && !is_host_holder(&aterm_codec::pct::decode(holder)))
                    .then(|| sid.to_string())
            })
            .collect(),
    )
}

/// THE CLAIMS OF EVERY INSTANCE `clients` reach, as one set
/// ([`foreign_claims`] of each, unioned; tab ids are unique across
/// instances): an instance that answers no roster adds nothing and holds no
/// other back. `--status`'s view of a machine running several aterms
/// (2026-10-01).
pub(super) fn claims_of(
    clients: impl IntoIterator<Item = Client>,
) -> std::collections::BTreeSet<String> {
    clients
        .into_iter()
        .filter_map(|mut c| foreign_claims(&mut c))
        .flatten()
        .collect()
}

/// How long [`foreign_claims`] waits for the roster: the 2 s a discovery
/// client gives a peer (`aterm ctl ls`), four times the instance's own
/// bound on the one hop its `sessions` reply takes.
const CLAIMS_WAIT: Duration = Duration::from_secs(2);

/// `who` reads the owner-only registry without the `sessions`
/// window-placement hop. Each PTY master's foreground process group is a
/// kernel-backed tab claim, available even when macOS returns argv without
/// another process's environment. Both the upgrade drivers and the
/// supervisor's worker binding (`supervise`'s `Session::worker_env`) read it
/// through [`roster_rows`] and judge a group by [`unique_tab_for_group`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTab {
    pub(crate) sid: String,
    pub(crate) fgpgid: Option<i64>,
}

pub(super) fn host_roster(c: &mut Client) -> Option<Vec<LiveTab>> {
    let (head, body) = c.request_counted("who").ok()?;
    parse_host_roster(&head, &body)
}

fn parse_host_roster(head: &str, body: &str) -> Option<Vec<LiveTab>> {
    if !head.starts_with("OK") {
        return None;
    }
    Some(roster_rows(body)?.into_iter().map(|(_, tab)| tab).collect())
}

/// A `who` reply's rows (`<local> <s-id> … fgpgid=<group|->`), each with the
/// instance-local id it leads with. `None` — no roster at all — for a row
/// that is not that shape, and for an `s-…` id two rows claim.
pub(crate) fn roster_rows(body: &str) -> Option<Vec<(u64, LiveTab)>> {
    let mut rows: Vec<(u64, LiveTab)> = Vec::new();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        let local = words.next()?.parse::<u64>().ok()?;
        let sid = words.next()?.to_string();
        if !sid.starts_with("s-") || rows.iter().any(|(_, t)| t.sid == sid) {
            return None;
        }
        let fgpgid = row
            .split_whitespace()
            .find_map(|word| word.strip_prefix("fgpgid="))
            .and_then(|word| word.parse::<i64>().ok())
            .filter(|&group| group > 0);
        rows.push((local, LiveTab { sid, fgpgid }));
    }
    Some(rows)
}

fn tab_is_live(tabs: &[LiveTab], sid: &str) -> bool {
    tabs.iter().any(|tab| tab.sid == sid)
}

/// A duplicated foreground group is ambiguity, never permission to choose
/// whichever tab happened to be listed first.
pub(crate) fn unique_tab_for_group(tabs: &[LiveTab], group: i64) -> Option<&str> {
    let mut found = None;
    for tab in tabs.iter().filter(|tab| tab.fgpgid == Some(group)) {
        if found.is_some() {
            return None;
        }
        found = Some(tab.sid.as_str());
    }
    found
}

/// Recheck the unique PTY claim before a delayed typed or signalled step.
pub(super) fn process_in_tab(
    c: &mut Client,
    pid: u32,
    tab: &str,
    expected_group: Option<i64>,
    expected_parent: Option<u32>,
) -> bool {
    let Some(tabs) = host_roster(c) else {
        return false;
    };
    let Some(group) = process_group(pid) else {
        return false;
    };
    expected_group.is_none_or(|expected| expected == group)
        && unique_tab_for_group(&tabs, group) == Some(tab)
        && ids(pid).is_some_and(|(parent, pgid, tpgid)| {
            expected_parent.is_none_or(|expected| expected == parent)
                && pgid == group
                && tpgid == group
        })
}

/// The tab's visible rows, the cursor within them, and its content sequence.
pub(super) struct Screen {
    pub(super) rows: Vec<String>,
    pub(super) cursor: Option<(usize, usize)>,
    pub(super) seq: u64,
    /// The screen row `rows[0]` is: the coordinates the `cell` verb takes.
    pub(super) first: usize,
    /// The screen generation (`gen`, `<epoch>.<seq>`) the read was taken at:
    /// what a fenced `turn if-gen=` names ([`turn_fenced`]). `None` from a
    /// host that sends none.
    pub(super) generation: Option<String>,
    /// When a person last gave the session input, as the server stamped it
    /// at the read (`"human_ms"`): the one presence fact
    /// ([`upgrade::attended_by`], [`upgrade::Facts::attended`]).
    pub(super) human: HumanInput,
}

pub(super) fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(c, &format!("@{sid} text --json"))
}

/// Claude's composer is pinned near the bottom. The continuation wait needs
/// only that slice while a draft is present; a positive result is confirmed
/// against the whole screen before it can authorize a turn.
const CONTINUATION_TAIL_ROWS: usize = 20;

fn screen_tail(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(
        c,
        &format!("@{sid} text --json tail={CONTINUATION_TAIL_ROWS}"),
    )
}

fn screen_query(c: &mut Client, request: &str) -> Option<Screen> {
    let (head, body) = c.request_counted(request).ok()?;
    if !head.starts_with("OK") {
        return None;
    }
    parse_screen(if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    })
}

/// One `text --json` object: its rows, the cursor made relative to them (the
/// cursor row is absolute; `first` is the row the reply starts at), and `seq`.
fn parse_screen(json: &str) -> Option<Screen> {
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    let rows: Vec<String> = v
        .get("rows")?
        .as_array()?
        .iter()
        .map(|r| r.as_str().unwrap_or("").to_string())
        .collect();
    let first = v.get("first").and_then(Value::as_u64).unwrap_or(0);
    let cursor = v.get("cursor").and_then(|cur| {
        let row = cur.get("row")?.as_u64()?.checked_sub(first)?;
        let col = cur.get("col")?.as_u64()?;
        Some((usize::try_from(row).ok()?, usize::try_from(col).ok()?))
    });
    let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let generation = v
        .get("gen")
        .and_then(Value::as_str)
        .filter(|g| is_generation(g))
        .map(str::to_owned);
    // Absent (an older host) is unknown; `null` is never; a number is its
    // age; anything else is unknown, as the supervisor reads it.
    let human = match v.get("human_ms") {
        None => HumanInput::Unknown,
        Some(Value::Null) => HumanInput::Never,
        Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
    };
    Some(Screen {
        rows,
        cursor,
        seq,
        first: usize::try_from(first).ok()?,
        generation,
        human,
    })
}

/// Whether `g` is a screen generation the server accepts as the `if-gen=`
/// fence, by the server's own parser ([`ScreenGen::parse`], shared through
/// `aterm_types::control_verbs`), never a local copy of its shape. Anything
/// else is never put on a fenced request line.
fn is_generation(g: &str) -> bool {
    ScreenGen::parse(g).is_some()
}

/// [`upgrade::composer_is_empty`] over `scr`, with the one fact its rows cannot
/// carry: whether the text at the CARET row's column 2 is drawn DIM (Claude's
/// placeholder suggestion) or not (a typed draft whose caret was moved home).
/// Read with the `cell` verb where [`upgrade::composer_dim_row`] says a read
/// can change the answer — at the composer's own row, wherever the cursor is
/// (2026-09-28: read at the CURSOR's row, a grid one row displaced by a
/// net-zero re-grid read the rule's glyph below the composer, not dim, and
/// Claude's own suggestion was a person's draft for 51 minutes). A reply that
/// is not an `OK` naming `dim` — an `ERR`, a lost connection, an unexpected
/// shape — reads NOT dim: a suggestion then waits as a draft would, and
/// nothing is ever typed over a draft.
pub(super) fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = dim_cell(scr).is_some_and(|at| {
        c.request_line(&format!("@{sid} cell {at} 2"))
            .is_ok_and(|line| cell_is_dim(&line))
    });
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

/// The idle continuation can wait minutes with a person's draft in the
/// composer, so these repeated polls read only the bottom rows. A missing
/// cursor or caret makes the slice inconclusive, and an unreadable tail (an
/// older host) falls back to the full screen. A positive tail never suffices
/// to type: the full screen must independently show an empty composer.
pub(super) fn continuation_composer_empty(
    c: &mut Client,
    sid: &str,
    tail_available: &mut bool,
) -> bool {
    if *tail_available {
        match screen_tail(c, sid) {
            Some(scr)
                if scr.cursor.is_some_and(|(row, _)| row < scr.rows.len())
                    && aterm_phase::phase::composer_index(&scr.rows).is_some() =>
            {
                if !composer_empty(c, sid, &scr) {
                    return false;
                }
            }
            Some(_) => {}
            None => *tail_available = false,
        }
    }
    screen(c, sid).is_some_and(|scr| composer_empty(c, sid, &scr))
}

use crate::supervise::screen::cell_is_dim;

/// What stands of text a lane typed and did not send, read in Claude Code's
/// prompt ([`left_standing`]) or Codex's composer
/// (`upgrade_codex_drive::left_standing`). Never taken back by either lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    /// It is no longer there: the person cleared it, or submitted it.
    Gone,
    /// Still there, and why: alone and unsent (`unsent`), among a person's
    /// typing (`mixed`, theirs now), or hidden (a box, no composer, no read).
    Kept(&'static str),
}

/// `s` with all whitespace dropped, to compare a composer's text across its
/// wrapping (which may split a word) and its rows' indentation.
pub(super) fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether the host's `key` takes the `if-gen=` fence (its `help key` names
/// it, [`server_fences_gen`]).
pub(super) fn key_takes_gen(c: &mut Client) -> bool {
    c.request_counted("help key")
        .is_ok_and(|(head, body)| head.starts_with("OK") && server_fences_gen(&body))
}

/// What stands of what the Claude lane left typed: a notice or release the
/// host stopped part-way ([`Typed::Cut`]: deadline, screen switch, a person's
/// gesture) or typed whole with its guarded Enter unpressed (`guard`). Read
/// only: aterm never takes its words back, since a fenced `ctrl+u` cannot see
/// a person's key the host counted but Claude Code has not drawn, and could
/// erase it (or, after `ctrl-z`, hand the key to the shell). The text is
/// recorded ([`St::left_typed`]) so no look reads it as a person's draft and
/// the supervisor never sends it (`upgrade::own_draft`); the owner is told,
/// and a person sends or clears it.
///
/// aterm's part alone in the prompt (compared from the text's start with
/// whitespace dropped, [`compact`]) is `unsent`; with a person's keys among
/// it, `mixed`; a prompt holding anything else, or nothing, means `Gone`. A
/// box or no drawn prompt keeps the record as is.
pub(super) fn left_standing(c: &mut Client, tab: &str, text: &str) -> Left {
    let ours = compact(text);
    let mark = compact(upgrade::HARNESS_MARK);
    let Some(scr) = screen(c, tab) else {
        return Left::Kept("screen-unreadable");
    };
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Left::Kept("box");
    }
    let draft = aterm_phase::phase::composer_rules(&scr.rows)
        .and_then(|_| aterm_phase::phase::composer_draft(&scr.rows));
    let Some((_, lines)) = draft else {
        return Left::Kept("no-composer");
    };
    if composer_empty(c, tab, &scr) {
        return Left::Gone;
    }
    let draft = compact(&lines.concat());
    if ours.starts_with(&draft) {
        Left::Kept("unsent")
    } else if draft.starts_with(&mark) {
        Left::Kept("mixed")
    } else {
        Left::Gone
    }
}

/// A pressed text the host stopped ([`Typed::Cut`]): aterm's words, part or
/// all (`guard`) of `text`, stand in Claude Code's prompt. Recorded
/// ([`St::left_typed`]) so later looks read them first ([`left_first`]) and
/// never as a person's draft. Never taken back ([`left_standing`]); a person
/// sends or clears them.
fn left_cut(st: &mut St, c: &mut Client, tab: &str, text: &str) {
    st.left_typed = text.to_string();
    let _ = settle_left(st, c, tab);
}

/// One read of what stands of [`St::left_typed`] in Claude Code's prompt
/// ([`left_standing`], which presses nothing): the record goes once the text
/// is gone — a person sent or cleared it.
fn settle_left(st: &mut St, c: &mut Client, tab: &str) -> Left {
    let text = st.left_typed.clone();
    let left = left_standing(c, tab, &text);
    if left == Left::Gone {
        st.left_typed.clear();
    }
    left
}

/// Aterm's own words left in the prompt ([`St::left_typed`]) come first at a
/// look: read, never pressed ([`settle_left`]); while they stand the look
/// waits `wait:left-typed:<why>`, never `draft`. `None` when nothing of
/// aterm's is recorded or it is gone. A dry run reads them the same way.
fn left_first(
    opts: &Opts,
    r: &Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    now: u64,
) -> Option<Report> {
    if st.left_typed.is_empty() {
        return None;
    }
    let r = match settle_left(st, c, tab) {
        Left::Gone => return None,
        Left::Kept(why) => said(r.clone(), format!("wait:left-typed:{why}")),
    };
    st.note_step(&r.step, &sf.status, now);
    save(opts, &sf.session_id, st);
    Some(r)
}

/// Whether someone other than this sweep holds the tab: a halt, a hand on it
/// (a turn, lease or driver other than this process's [`Hand`]), unread input
/// (`input=`: a first prompt a signal would drop), or a person who typed
/// within `grace_s` seconds. `0` asks only the hold, hands and input (the
/// gates read the person as [`upgrade::Facts::attended`], which `--now`
/// waives).
pub(super) fn held(c: &mut Client, sid: &str, grace_s: u32) -> bool {
    status_line(c, sid).is_none_or(|line| held_by_status(&line, grace_s))
}

/// The tab's `status` line, or `None` when it cannot be read.
pub(super) fn status_line(c: &mut Client, sid: &str) -> Option<String> {
    c.request_line(&format!("@{sid} status")).ok()
}

/// [`held`] over one `status` reply; anything but `OK` reads as held. A
/// `human_ms=` under the grace is a person at the keyboard, who wins; `-` or
/// no field (an older server) is nobody.
pub(super) fn held_by_status(line: &str, grace_s: u32) -> bool {
    !line.starts_with("OK")
        || line.split_whitespace().any(|t| {
            t == "hold=1"
                || t.strip_prefix("hand=")
                    .is_some_and(|hand| hand != "-" && !our_hand(hand))
                || t.strip_prefix("input=")
                    .is_some_and(|input| !matches!(input, "-" | "clear"))
        })
        || super::relaunch::person_present(line, grace_s)
}

/// The harness's hand on a tab whose agent it restarts. From the signal to
/// the relaunched agent's first idle the tab is a bare shell and then a
/// booting agent, and an orchestrator's first prompt sent then would run as a
/// shell command line or die with the old process. So a restart takes the
/// server's hard drive lease ([`HAND_TTL_MS`], holder [`hand_name`], `lease
/// acquire … hard`) before its last look and holds it, renewed through every
/// wait ([`Hand::keep`]), through the signal, the shell's return and the
/// relaunch line, to the agent's first idle (`relaunch::first_idle`). `status`
/// and `who` show it (`hand=lease:aterm-harness@<pid>`), and every other
/// connection's write (`send`, `key`, `paste`, `feed`, `turn`) is refused
/// `ERR busy lease=…` while this connection's writes pass, so no one else's
/// keys land around the relaunch line.
///
/// A person's keyboard is never blocked: a keystroke before the signal stops
/// the restart ([`held`], the signal's `quiet=`, [`terminate`]); after it, a
/// person at the returned prompt holds the relaunch line. A step that dies
/// holding the hand leaves it to lapse; an aterm without hard leases holds
/// nothing, and the restart relies on its last looks and fences.
pub(super) const HAND_TTL_MS: u64 = 60_000;

/// How often a wait under the hand renews it ([`Hand::keep`]): well inside
/// [`HAND_TTL_MS`], so no relaunch wait (exit 30 s, shell prompt 15 s, new
/// process 90 s, first idle 15 s) outlasts it.
const HAND_RENEW: Duration = Duration::from_secs(20);

/// The holder name of this process's hand ([`Hand`]): the name its supervisor
/// loops claim sessions under, spelled the same as the window's host
/// (`aterm-gui`'s `harness_host::holder`), by whose [`HOST_HOLDER`] `--status`
/// tells a window's own claim from another supervisor's ([`foreign_claims`]).
#[must_use]
pub fn hand_name() -> String {
    format!("{HOST_HOLDER}{}", std::process::id())
}

/// What every window's host's holder name begins with ([`hand_name`]).
const HOST_HOLDER: &str = "aterm-harness@";

/// Whether a claim's holder (percent-decoded) is a window's own host —
/// this one's or a predecessor's, whose carried claim names a process that
/// is gone and lapses by itself ([`HOST_HOLDER`]) — and so no other
/// supervisor's. The one spelling `--status` ([`foreign_claims`]) and the
/// window's host (`aterm-gui`'s `harness_host::claimed_by_another`) read a
/// `supervisor=` by.
#[must_use]
pub fn is_host_holder(holder: &str) -> bool {
    holder.starts_with(HOST_HOLDER)
}

/// Whether one `status` `hand=` value is this process's own [`Hand`]: no
/// one else's hand on the tab.
#[must_use]
pub(super) fn our_hand(hand: &str) -> bool {
    hand.strip_prefix("lease:") == Some(hand_name().as_str())
}

/// This process's hand on one tab (see [`HAND_TTL_MS`]).
#[derive(Debug)]
pub(super) struct Hand {
    tab: String,
    /// The server holds it for this process (it took no lease, or it
    /// lapsed and another driver took the tab: not held).
    held: bool,
    renewed: Instant,
}

impl Hand {
    /// Takes (or renews) this process's hand on `tab`: `None` when another
    /// driver holds it (another holder's live lease, or a live `turn`), which
    /// a restart waits out. From a server without hard leases, a hand that
    /// holds nothing.
    pub(super) fn take(c: &mut Client, tab: &str) -> Option<Hand> {
        let reply = c.request_line(&format!(
            "@{tab} lease acquire ttl={HAND_TTL_MS} holder={} hard",
            hand_name()
        ));
        let held = match reply {
            Ok(reply) if reply.starts_with("OK lease acquired") => true,
            Ok(reply) if reply.starts_with("ERR busy") || reply.starts_with("ERR lease held") => {
                return None;
            }
            _ => false,
        };
        Some(Hand {
            tab: tab.to_string(),
            held,
            renewed: Instant::now(),
        })
    }

    /// [`Self::take`] for a relaunch already under way (the agent ended):
    /// another driver's hold is no reason to leave a bare shell, so the hand
    /// is simply not held and the line relies on its own fences.
    pub(super) fn take_or_none(c: &mut Client, tab: &str) -> Hand {
        Hand::take(c, tab).unwrap_or_else(|| Hand {
            tab: tab.to_string(),
            held: false,
            renewed: Instant::now(),
        })
    }

    /// Renew the hand once [`HAND_RENEW`] has passed since it was last taken:
    /// called through every wait under it.
    pub(super) fn keep(&mut self, c: &mut Client) {
        if self.held && self.renewed.elapsed() >= HAND_RENEW {
            self.held = Hand::take(c, &self.tab).is_some_and(|h| h.held);
            self.renewed = Instant::now();
        }
    }

    /// Gives the hand back, never anyone else's (the server matches the
    /// holder and never releases a `turn`). Best effort: otherwise it lapses
    /// ([`HAND_TTL_MS`]).
    pub(super) fn give_back(&mut self, c: &mut Client) {
        if std::mem::take(&mut self.held) {
            let _ = c.request_line(&format!(
                "@{} lease release holder={}",
                self.tab,
                hand_name()
            ));
        }
    }
}

/// RUN ONE TYPED ACT UNDER THE HARNESS'S HAND, whichever hand it is: `hand`
/// when the caller already holds one on the tab (kept, never given back
/// here: its caller does), else a hand taken for this act alone and given
/// back the moment it is over. `None` when the tab is another driver's
/// (`Hand::take` refused): the act types nothing and the caller answers
/// `held`. This is what makes a typed act aterm's own for the window's
/// presence layer (ruling 313: aterm's own hand never shows a row), and
/// what [`type_fenced`] does for the Claude lane's notice and release; the
/// Codex lane's notice, goal keys and continuation, and the Claude
/// continuation a later visit finds owed, go through it too
/// (`every_typed_act_in_the_harness_is_under_a_hand` fails the build for a
/// typed act that does not). `act` runs its own fence read and its `turn`
/// with nothing of aterm's between them.
pub(super) fn under_hand<T>(
    c: &mut Client,
    tab: &str,
    hand: Option<&mut Hand>,
    act: impl FnOnce(&mut Client) -> T,
) -> Option<T> {
    let mut own = match hand {
        Some(_) => None,
        None => Some(Hand::take(c, tab)?),
    };
    if let Some(hand) = own.as_mut().or(hand) {
        hand.keep(c);
    }
    let out = act(c);
    if let Some(mut own) = own {
        own.give_back(c);
    }
    Some(out)
}

/// HOW LONG THE SESSION HAS BEEN QUIET, for [`upgrade::Facts::quiet_s`]:
/// since the agent's verdict last moved, as the server publishes it on
/// `status` (`agent=… agent_since_ms=…`, the verdict the supervisor's own
/// `await agent` wakes on) — a turn end (`idle`, `question`) that long ago,
/// any other verdict not quiet at all — so a repaint nothing reads a verdict
/// from (Claude Code's dim `❯ continue` suggestion, a ticking footer) never
/// starts it over. D3 of the live E2E of 2026-09-26: quiet was counted from
/// the look that first saw a new screen `seq`, the suggestion drawn between
/// two looks started it over, and the notice waited five more minutes. A
/// server that publishes no verdict, or one with no evidence (`unknown`), is
/// measured as before, from the looks' own samples of the screen's `seq`
/// (kept in `st` either way).
pub(super) fn quiet_s(status: Option<&str>, st: &mut St, seq: u64, now: u64) -> u64 {
    if seq != st.last_seq {
        st.last_seq = seq;
        st.seq_since_s = now;
    }
    let field = |key: &str| status?.split_whitespace().find_map(|t| t.strip_prefix(key));
    let since_ms = field("agent_since_ms=").and_then(|ms| ms.parse::<u64>().ok());
    match (field("agent="), since_ms) {
        (Some("idle" | "question"), Some(ms)) => ms / 1000,
        // A verdict with no evidence behind it (`unknown`: a Codex screen
        // whose last message scrolled away) or none says nothing either way.
        (Some(word), Some(_)) if word != "-" && word != "unknown" => 0,
        _ => now.saturating_sub(st.seq_since_s),
    }
}

/// Types one line at a shell's prompt as keystrokes (raw `send`, then Enter),
/// never as a paste. `Ok` only when both answer `OK`.
///
/// A paste is framed for bracketed paste whenever the terminal's mode says
/// so, and a crashed agent never restores the modes it set; a bracketed
/// paste into a shell without support (macOS `/bin/bash` 3.2) leaves the
/// closing frame's `01~` glued to `--resume <id>`. Raw bytes are read as typed
/// whatever the modes; Enter stays a carriage return under the kitty
/// `disambiguate` flag an agent leaves.
pub(super) fn type_line(c: &mut Client, sid: &str, line: &str) -> Result<(), String> {
    for request in [
        format!("@{sid} send -- {line}"),
        format!("@{sid} key enter"),
    ] {
        let reply = c.request_line(&request).map_err(|e| format!("{e}"))?;
        if !reply.starts_with("OK") {
            return Err(reply);
        }
    }
    Ok(())
}

/// Why a typed turn did not go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Typed {
    /// The screen moved between the judging read and the paste, and the host
    /// typed nothing (`turn if-gen=`'s `skipped reason=changed`). A wait: the
    /// next look judges the screen as it is.
    Changed,
    /// A person kept typing through the whole park (`yield=`) and the host
    /// typed nothing (`ERR yield timeout`). A wait like [`Typed::Changed`],
    /// never a refusal, which would fail an upgrade whose agent already ended.
    Yielded,
    /// The host pressed part of the text and stopped without Enter (`skipped
    /// reason=<why> typed=<k>`, k > 0: a person's key or a screen switch
    /// between keys, `person`/`changed`, or the deadline), or pressed all of
    /// it and its guarded Enter missed (`guard`: the cursor's row no longer the
    /// composer's). Not delivered, not recorded as typed, not retried at once:
    /// the caller records the standing words ([`left_cut`]) and never takes
    /// them back ([`left_standing`] only reads), so no later look reads them
    /// as a person's draft and the supervisor never sends them.
    Cut(String),
    /// Anything else the host answered (or the connection failed with).
    Refused(String),
}

impl Typed {
    /// The report word: `changed`, `yield`, `cut-<why>`, or the refusal's
    /// first words.
    pub(super) fn word(&self) -> String {
        match self {
            Typed::Changed => "changed".to_string(),
            Typed::Yielded => "yield".to_string(),
            Typed::Cut(why) => format!("cut-{why}"),
            Typed::Refused(e) => first_word(e),
        }
    }
}

/// How a harness-typed turn waits: for its verified submit and nothing more
/// (`idle=1` settles once the Enter is seen taken, never on the answer), with
/// `timeout=` bounding the park on a person's typing (`yield=`), the echo and
/// the submit's tries. A long wait would hold the tab (`hand=turn:<id>`) and
/// hide the answer from the loop; instead the loop awaits the answer as the
/// harness's own turn ([`typed`], `crate::supervise::HostStep::typed`), so
/// nothing is continued before it ends and it does not count as worker work
/// for the continue back-off.
pub(super) const TURN_WAIT: &str = "idle=1 timeout=10000";

/// Type one turn into the tab. `Ok` only on an `OK` reply.
pub(super) fn turn(c: &mut Client, sid: &str, text: &str) -> Result<(), String> {
    turn_fenced(c, sid, text, None).map_err(|e| match e {
        Typed::Refused(e) => e,
        other => other.word(),
    })
}

/// Whether the host's `turn` takes the `if-gen=` fence (its `help turn` names
/// it). An older host would type a leading `if-gen=…` as text, so the fence
/// is never sent unprobed. Asked once per connection
/// ([`RelayClient::help_counted`]) and by [`typing_fence`] before its read, so
/// nothing is asked between the read and the paste.
fn turn_takes_gen(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| head.starts_with("OK") && (head + &body).contains("if-gen="))
}

/// Whether the host's `turn` takes `cap=<n>` (per `help turn`): room to press
/// a long text key by key. An older host would type a leading `cap=…`, so it
/// is never sent unprobed; the answer is kept with the rest of the help.
fn turn_takes_cap(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| head.starts_with("OK") && (head + &body).contains("cap=<n>"))
}

/// HOW A NOTICE IS PUT IN FRONT OF THE AGENT: pressed one key at a time, not
/// pasted. A plain `turn` is one bracketed paste, and Claude Code hands the
/// model a long paste inside `<pasted_content>` with the warning that it may
/// hold instructions its person did not write — so it declined to act on this
/// harness's own upgrade notice (measured 2026-09-29 on Claude Code 2.1.284:
/// the same 963 characters, sent as one paste or as one raw write, arrived
/// wrapped; as 100-character writes, or pressed key by key at 2 ms, they
/// arrived as plain text, and a 130-character paste arrived plain too). The
/// notice is one line with no control characters, so pressing it is exact;
/// 2 ms a key is ~2 s for a notice, and `cap=4096` is room for far more than
/// one (the turn's `timeout=` bounds the typing, grown to cover it,
/// [`turn_wait`]; the host never submits a text it could not press whole). A
/// host without `cap=` gets the paste it always got.
///
/// Only the agent's own words are pressed ([`notice_fenced`]: notice and
/// release). The relaunch line and the continuation stay one paste
/// ([`turn_fenced`]): a shell reads a paste whole, past its bindings, and a
/// person's Enter during key-by-key typing would run a partial line.
const NOTICE_KEYS: &str = "typed=1 cadence=2 cap=4096 ";

/// THE RENDER A PRESSED DIGIT WAITS FOR, in milliseconds: the host holds
/// each ASCII digit of a pressed text until the key before it has echoed
/// (the caret anchor, `aterm-gui`'s `TypingHold`, landed 2026-09-30) —
/// about one Claude Code render, ~150 ms at p99 under a build — and a
/// notice carries 60-100 digits (its versions, the READY marker, the
/// running clause's pids and ages).
const PRESSED_DIGIT_MS: u64 = 150;

/// HOW A PRESSED TEXT'S TURN WAITS ([`checked_turn`]): [`TURN_WAIT`], its
/// `timeout=` — the whole turn's budget: the yield's park, the typing, the
/// submit seen taken — grown by the text's keys at [`NOTICE_KEYS`]' cadence
/// and its digits' renders ([`PRESSED_DIGIT_MS`]). At 10 s a notice of up to
/// ~2000 keys and ~100 held digits under a build ran past the deadline, and
/// the host cuts a text there part-way (`reason=deadline`, [`Typed::Cut`]),
/// whose words stand until a person clears them. The turn returns once its
/// submit is taken, so the larger budget holds nothing longer when the keys
/// go as they should. A pasted text (`keys` empty) waits [`TURN_WAIT`].
fn turn_wait(keys: &str, text: &str) -> String {
    if keys.is_empty() {
        return TURN_WAIT.to_string();
    }
    let value = |words: &str, key: &str| {
        words
            .split_whitespace()
            .find_map(|w| w.strip_prefix(key)?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let pressed = count(text.chars().count()).min(value(keys, "cap="));
    let digits = count(text.chars().filter(char::is_ascii_digit).count());
    let timeout = value(TURN_WAIT, "timeout=")
        .saturating_add(pressed.saturating_mul(value(keys, "cadence=")))
        .saturating_add(digits.saturating_mul(PRESSED_DIGIT_MS));
    TURN_WAIT
        .split_whitespace()
        .map(|w| {
            if w.starts_with("timeout=") {
                format!("timeout={timeout}")
            } else {
                w.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a checked turn puts its text in ([`checked_turn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Put {
    /// One paste: the relaunch line, the continuation.
    Paste,
    /// Key by key where the host takes `cap=` ([`NOTICE_KEYS`]): the notice
    /// and the release.
    Pressed,
}

/// Whether the host's `turn` takes the guarded submit
/// (`submit=guarded:<re>`, per `help turn`): Enter pressed only while the
/// cursor's row still matches. Never sent unprobed (an older host would type
/// it); the answer is kept with the rest of the help.
fn turn_takes_guard(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| head.starts_with("OK") && (head + &body).contains("guarded:"))
}

/// Whether the host's `turn` takes the narrow `if-rows=` fence (per `help
/// turn`). The same kept answer [`turn_takes_gen`] asks.
fn turn_takes_rows(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| head.starts_with("OK") && (head + &body).contains("if-rows="))
}

/// The checked type: one turn into the tab, parked first on a person's typing
/// momentum (`yield=0.2`, the loop's own floor) and, with a `generation`,
/// pasted only while the screen is still the one it names, checked by the host
/// under its terminal lock right before the paste (`turn if-gen=`), returning
/// once its submit is seen taken ([`TURN_WAIT`]). This closes the window
/// between judging the screen and pasting, where a person's first keystroke
/// could merge with the harness's text. `None`, or a host without the fence,
/// types unfenced. One paste ([`Put::Paste`]) for the relaunch line and the
/// continuation; the agent's own words go through [`notice_fenced`].
pub(super) fn turn_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Paste)
}

/// [`turn_fenced`] pressed key by key ([`NOTICE_KEYS`]), for words the agent
/// must read as its person's (notice and release). The host holds the fence
/// between keys, never submits a text it could not press whole, and presses
/// Enter only while the cursor's row is still the composer's ending in the
/// text's last word (`submit=guarded:`); each miss is a [`Typed::Cut`] whose
/// words the caller records and leaves standing.
pub(super) fn notice_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, Put::Pressed)
}

/// The checked type's one request ([`turn_fenced`], [`notice_fenced`]).
fn checked_turn(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    put: Put,
) -> Result<(), Typed> {
    // A `rows:` fence ([`claude_fence`]) is the narrow one; anything else is
    // the read's generation.
    let fence = match generation {
        Some(g) if g.starts_with(ROWS_FENCE) => g
            .strip_prefix(ROWS_FENCE)
            .and_then(aterm_types::control_verbs::RowsFence::parse)
            .filter(|_| turn_takes_rows(c))
            .map(|f| format!("if-rows={f} "))
            .unwrap_or_default(),
        Some(g) if is_generation(g) && turn_takes_gen(c) => format!("if-gen={g} "),
        _ => String::new(),
    };
    let keys = if put == Put::Pressed && turn_takes_cap(c) {
        NOTICE_KEYS
    } else {
        ""
    };
    // A pressed text's Enter is guarded. The host stops typing on a person's
    // key or a screen switch ([`Typed::Cut`]), and on an `if-rows=` fence
    // when a box takes the composer frame's place, but keys in flight before
    // a box's first frame still reach it, and the `if-gen=` fallback has no
    // frame at all; an unguarded Enter could confirm a box's focused `1. Yes`.
    // So Enter is pressed only while the cursor's row is still the composer's
    // ending in the text's last word ([`composer_guard`], the supervisor's own
    // guard); a miss presses nothing ([`Typed::Cut`]`("guard")`). A pasted
    // notice (host without `cap=`) keeps a plain Enter: Claude Code draws a
    // long paste as `[Pasted text …]`, which no text row would match.
    let submit = if !keys.is_empty() && turn_takes_guard(c) {
        format!("submit=guarded:{} ", composer_guard('❯', text))
    } else {
        String::new()
    };
    let wait = turn_wait(keys, text);
    let (head, _) = c
        .request_counted(&format!(
            "@{sid} turn {fence}{keys}{submit}yield=0.2 {wait} {text}"
        ))
        .map_err(|e| Typed::Refused(format!("{e}")))?;
    turn_verdict(&head)
}

/// Judges one `turn` reply's head. A `skipped` verdict pressed no Enter: one
/// that typed part of the text (`typed=<k>`, k > 0), or a guarded Enter that
/// missed (`reason=guard`), is [`Typed::Cut`] by its reason; one that typed
/// nothing is [`Typed::Changed`] (`reason=changed`) or a wait (`reason=person`,
/// `reason=deadline`), like a yield that outlived the timeout
/// ([`Typed::Yielded`]). Any other `OK` typed.
fn turn_verdict(head: &str) -> Result<(), Typed> {
    if head.starts_with("ERR yield timeout") {
        return Err(Typed::Yielded);
    }
    if !head.starts_with("OK") {
        return Err(Typed::Refused(head.to_string()));
    }
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.contains(&"skipped") {
        let field = |key: &str| words.iter().find_map(|w| w.strip_prefix(key));
        let reason = field("reason=").unwrap_or_default();
        let typed = field("typed=").and_then(|k| k.parse::<u64>().ok());
        return Err(match (reason, typed) {
            (why, Some(1..)) => {
                Typed::Cut(if why.is_empty() { "unknown" } else { why }.to_string())
            }
            ("guard", _) => Typed::Cut("guard".to_string()),
            ("changed", _) => Typed::Changed,
            ("person" | "deadline", _) => Typed::Yielded,
            _ => Typed::Refused(head.to_string()),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------- models

/// The step's model half, read once per pass: the priority list (grown from
/// Claude Code's own recommendations), the managed build's baked catalog,
/// every model a restart on it may ask for ([`models::offered`]; each
/// conversation moves to the newest of its own family first, then by the
/// list, [`models::model_due`]), and the person's saved default.
pub(super) struct ModelCtx {
    list: Priority,
    baked: Option<Baked>,
    offered: Vec<String>,
    /// The person's saved default model (`~/.claude/settings.json` `model`):
    /// a choice of theirs, which the list never moves across families.
    default_model: Option<String>,
    /// The native build's own baked catalog and offer, for a session running
    /// the vendor's native install rather than the managed twin. `None` when no
    /// native build is installed.
    native: Option<(Option<Baked>, Vec<String>)>,
}

impl ModelCtx {
    /// The catalog and the offer a session is judged by: the native build's
    /// for a session that runs it (when one is installed), else the managed
    /// one's.
    fn view(&self, native: bool) -> (Option<&Baked>, &[String]) {
        match (&self.native, native) {
            (Some((baked, offered)), true) => (baked.as_ref(), offered),
            _ => (self.baked.as_ref(), &self.offered),
        }
    }

    /// No model half at all (a test's pass, or a list that cannot be read):
    /// nothing is ever due.
    pub(super) fn none() -> ModelCtx {
        ModelCtx {
            list: Priority::seed(0),
            baked: None,
            offered: Vec::new(),
            default_model: None,
            native: None,
        }
    }

    fn read(opts: &Opts, targets: &Targets) -> ModelCtx {
        let now = i64::try_from(now_s()).unwrap_or(0);
        let path = state_dir(opts).join("models.json");
        let Ok(mut list) = (if opts.dry_run {
            std::fs::read_to_string(&path).map_or_else(
                |_| Ok(Priority::seed(now)),
                |t| Priority::parse(&t).ok_or_else(String::new),
            )
        } else {
            models::load_or_seed(&path, now)
        }) else {
            return ModelCtx::none();
        };
        let managed = targets.managed.as_ref();
        let baked = managed
            .and_then(|m| std::fs::read_to_string(&m.exe).ok())
            .and_then(|twin| catalog::twin_binary(&twin))
            .and_then(|bin| catalog::baked_cached(&bin, &state_dir(opts).join("builds")));
        let label = managed.map(|m| m.version.to_string()).unwrap_or_default();
        let ev = model_evidence(opts);
        let recs = models::recommendations(&ev, baked.as_ref(), &label);
        let changes = models::ingest(&mut list, &recs, baked.as_ref(), now);
        if !changes.is_empty() && !opts.dry_run {
            let _ = models::write_atomic(&path, &list.render());
            for ch in &changes {
                let r = Report {
                    pid: 0,
                    tab: "-".to_string(),
                    session: "-".to_string(),
                    from: "-".to_string(),
                    to: ch.id.clone(),
                    step: "models-changed".to_string(),
                };
                ledger(opts, &r, &format!("{} ({})", ch.placed, ch.source));
            }
        }
        let user = std::fs::read_to_string(opts.home.join(".claude").join("settings.json"))
            .map(|t| models::parse_user_settings(&t))
            .unwrap_or_default();
        let offered = models::offered(&list, &label, &ev, baked.as_ref(), user.allowed.as_deref());
        // The native build is judged by its own version and catalog (the two
        // builds may know different models); its file is the binary itself.
        let native = targets.native.as_ref().map(|n| {
            let nb = catalog::baked_cached(&n.exe, &state_dir(opts).join("builds"));
            let no = models::offered(
                &list,
                &n.version.to_string(),
                &ev,
                nb.as_ref(),
                user.allowed.as_deref(),
            );
            (nb, no)
        });
        ModelCtx {
            list,
            baked,
            offered,
            default_model: user.default,
            native,
        }
    }
}

fn model_record_path(opts: &Opts, session: &str) -> PathBuf {
    model_record_in(&opts.state, session)
}

/// Where `session`'s [`ModelRecord`] lives under the harness's state root
/// `state` ([`Opts::state`]; [`state_dir`]'s `upgrade`).
fn model_record_in(state: &Path, session: &str) -> PathBuf {
    state
        .join("upgrade")
        .join("models")
        .join(format!("{session}.json"))
}

/// THE RESUME LINE A PERSON IS TOLD TO RUN, under the relaunch's own model
/// rule (2026-10-01, the review of 12af1fe65): [`super::resume::command`]
/// carries the process's argv as it stands, so a `--model` the
/// conversation's record marks FAILED — the harness's own ask that did not
/// take — was printed for a person to start on again. It is left off
/// ([`models::launch_model_kept`]), as every relaunch line leaves it off
/// ([`line_model_of`]); a person's own `--model` is kept. `state` is the
/// harness's state root ([`Opts::state`]; `None`: no record to read, the
/// line as launched).
///
/// EVERY SUCH LINE IS THIS ONE (2026-10-02): the window's mark of an agent
/// it cannot bring back, its memory warning, frozen mail and turn-end
/// escalation, and the footer's frozen-program remedy and menu-bar row
/// ([`super::resume::of_entry`]) — so no two of them tell a person different
/// lines for one conversation. The record is read only where the launch
/// names a `--model`.
#[must_use]
pub fn resume_command_kept(state: Option<&Path>, argv: &[String], session: &str) -> Option<String> {
    let launch = upgrade::launch_model(argv);
    let dropped = launch.as_deref().zip(state).is_some_and(|(m, state)| {
        let record = std::fs::read_to_string(model_record_in(state, session))
            .ok()
            .and_then(|t| ModelRecord::parse(&t))
            .unwrap_or_default();
        models::launch_model_kept(Some(m), &record).is_none()
    });
    if dropped {
        super::resume::command(
            &super::relaunch::with_model(argv.to_vec(), Some("")),
            session,
        )
    } else {
        super::resume::command(argv, session)
    }
}

fn load_model_record(opts: &Opts, session: &str) -> ModelRecord {
    std::fs::read_to_string(model_record_path(opts, session))
        .ok()
        .and_then(|t| ModelRecord::parse(&t))
        .unwrap_or_default()
}

fn save_model_record(opts: &Opts, session: &str, rec: &ModelRecord) {
    if opts.dry_run {
        return;
    }
    let _ = models::write_atomic(&model_record_path(opts, session), &rec.render());
}

/// One conversation's model, read once per visit.
struct ModelRead {
    verdict: ModelVerdict,
    /// The conversation's prompt cache is cold (no answer for
    /// [`models::CACHE_COLD_S`]).
    cold: bool,
    /// Seconds the current due move has been due (0 when none is) — the clock
    /// [`models::MODEL_WARM_MAX_S`] bounds.
    due_for_s: u64,
    /// Why the model a notice carried no longer rides its round's restart
    /// ([`models::ride_bar`]), or `None`: it rides, or none was carried.
    ride_barred: Option<&'static str>,
}

/// Read what the conversation runs now and decide, judged as the managed build
/// (the tests' seam; a visit reads the build first, [`model_read_for`]).
#[cfg(test)]
fn model_read(opts: &Opts, sf: &SessionFile, launch: Option<&str>, mctx: &ModelCtx) -> ModelRead {
    model_read_for(opts, sf, launch, mctx, false, None)
}

/// Read what the conversation runs NOW and decide, judged by the build the
/// session RUNS (`native`: the vendor's native install, [`ModelCtx::view`]).
/// A model this harness asked for that now runs is recorded as APPLIED (never
/// asked for again). `carried`: the model a notice of the upgrade carried
/// ([`St::carried_model`]), re-judged by the same read ([`models::ride_bar`]).
fn model_read_for(
    opts: &Opts,
    sf: &SessionFile,
    launch: Option<&str>,
    mctx: &ModelCtx,
    native: bool,
    carried: Option<&str>,
) -> ModelRead {
    let before = load_model_record(opts, &sf.session_id);
    let mut record = before.clone();
    let tail = std::cell::OnceCell::new();
    let (read, rows) = model_judge(
        opts,
        sf,
        &mut record,
        &tail,
        launch,
        mctx,
        native,
        now_s(),
        carried,
    );
    model_read_kept(opts, sf, &before, &record, &rows);
    read
}

/// A ledger row one read owes — its step, the model, the detail — written
/// only where the read is kept ([`model_read_kept`]).
type ModelRow = (&'static str, String, &'static str);

/// Keep one read: its record where it changed, and the rows it owes.
fn model_read_kept(
    opts: &Opts,
    sf: &SessionFile,
    before: &ModelRecord,
    record: &ModelRecord,
    rows: &[ModelRow],
) {
    if record != before {
        save_model_record(opts, &sf.session_id, record);
    }
    for (step, model, detail) in rows {
        let mut r = blank(sf);
        r.to = format!("model:{model}");
        ledger(opts, &said(r, *step), detail);
    }
}

/// One read's JUDGEMENT under one view, on `record` in memory, at `now`:
/// [`models::model_read_step`] — the early return, the settle step, a
/// person's `/model`, the rule, the ride's guard over a `carried` model
/// ([`models::ride_bar`]), the cache and the due clock, in that order,
/// bound at Tier 1 to the derived `HarnessModelSwitch` — over the view's
/// catalog and offer and the transcript's tail. It READS (the transcript,
/// once per `tail`, and only past the early return) and WRITES NOTHING — the
/// record and the rows it owes are the caller's to keep
/// ([`model_read_kept`]), so the host's pre-filter can judge under two views
/// and keep what both agree on ([`model_wants_a_look`]).
#[allow(clippy::too_many_arguments)]
fn model_judge(
    opts: &Opts,
    sf: &SessionFile,
    record: &mut ModelRecord,
    tail: &std::cell::OnceCell<String>,
    launch: Option<&str>,
    mctx: &ModelCtx,
    native: bool,
    now: u64,
    carried: Option<&str>,
) -> (ModelRead, Vec<ModelRow>) {
    let (baked, offered) = mctx.view(native);
    let view = models::ModelView {
        list: &mctx.list,
        baked,
        offered,
        launch,
        default_model: mctx.default_model.as_deref(),
        started_s: models::parse_lstart(&sf.proc_start),
        now,
        carried,
    };
    let step = models::model_read_step(record, &view, || {
        tail.get_or_init(|| {
            transcript(&opts.home, &sf.session_id)
                .map(|p| tail_to_end(&p, TAIL_BYTES).0)
                .unwrap_or_default()
        })
        .as_str()
    });
    let mut rows = Vec::new();
    if let Some(model) = step.settled.verified {
        rows.push(("model-verified", model, "the conversation runs it"));
    }
    if let Some(model) = step.settled.failed {
        rows.push((
            "model-failed",
            model,
            "asked for on a relaunch and not what runs",
        ));
    }
    let read = ModelRead {
        verdict: step.verdict,
        cold: step.cold,
        due_for_s: step.due_for_s,
        ride_barred: step.ride_barred,
    };
    (read, rows)
}

/// The model that rides a restart made for another reason (a relaunch on
/// exit, a restart in place): the model the rule moves the conversation
/// (`sf`, launched with `argv`) to ([`models::model_due`]) when a move is due,
/// taken now because the restart happens anyway ([`models::model_moves_now`]'s
/// `build-restart`). `None` when nothing is due or the move is not the
/// harness's. It is recorded as asked right before the relaunch line that
/// carries it is typed ([`record_asked`], `relaunch::relaunch`).
pub(super) fn riding_model_in(
    opts: &Opts,
    sf: &SessionFile,
    argv: &[String],
    mctx: &ModelCtx,
) -> Option<String> {
    let launch = upgrade::launch_model(argv);
    // A native session is judged by the native build's own catalog.
    let native = runs_native(opts, Some(sf.pid), argv);
    let mread = model_read_for(opts, sf, launch.as_deref(), mctx, native, None);
    models::model_to(None, &mread.verdict, mread.cold, true, mread.due_for_s)
}

/// Whether the build process `pid` runs is native, read off the process or,
/// once it has exited (relaunch on exit), off `argv[0]`, which the relaunch
/// runs again.
fn runs_native(opts: &Opts, pid: Option<u32>, argv: &[String]) -> bool {
    pid.and_then(exe_of)
        .or_else(|| argv.first().map(PathBuf::from))
        .is_some_and(|e| e.starts_with(native_root(&opts.home)))
}

/// The models a restart made for another reason (a memory banner's restart in
/// place, an exit's relaunch) carries for `session`, launched with `argv`: the
/// riding model ([`riding_model_in`], only when `ride`, only off the live
/// record `sf`), and otherwise the `/model` a person chose by hand
/// ([`hand_model_in`]). The process started at the record's start, else at
/// `start` (its `ps` start, [`kernel_start`]). One catalog read for both, none
/// when neither can be asked.
pub(super) fn restart_models(
    opts: &Opts,
    sf: Option<&SessionFile>,
    start: &str,
    session: &str,
    argv: &[String],
    ride: bool,
) -> (Option<String>, Option<String>) {
    let ride = ride && sf.is_some();
    if !ride && upgrade::launch_model(argv).is_none() {
        return (None, None);
    }
    let mctx = ModelCtx::read(opts, &Targets::read(&opts.home));
    let riding = sf
        .filter(|_| ride)
        .and_then(|sf| riding_model_in(opts, sf, argv, &mctx));
    let native = runs_native(opts, sf.map(|sf| sf.pid), argv);
    let started = sf
        .and_then(|sf| models::parse_lstart(&sf.proc_start))
        .or_else(|| models::parse_lstart(start));
    let hand = hand_model_in(opts, session, argv, started, &mctx, native);
    (riding, hand)
}

/// The model a person chose by hand for `session` (launched with `argv` by a
/// process started at `started_s`, [`models::hand_model`]), read through the
/// catalog of the build it runs (`native`) against the record's remembered
/// `/model` choice. `None` when the launch named no model (Claude's `/model`
/// then saved the choice as the default the relaunch starts on) or nobody
/// chose one.
pub(super) fn hand_model_in(
    opts: &Opts,
    session: &str,
    argv: &[String],
    started_s: Option<u64>,
    mctx: &ModelCtx,
    native: bool,
) -> Option<String> {
    let launch = upgrade::launch_model(argv)?;
    let (tail, _) = tail_to_end(&transcript(&opts.home, session)?, TAIL_BYTES);
    let (baked, _) = mctx.view(native);
    let human = load_model_record(opts, session).human;
    models::hand_model(&tail, baked, &human, &launch, started_s)
}

/// The launch's `--model` a relaunch of `session`, launched with `argv`,
/// keeps ([`models::launch_model_kept`], over the record as it stands): what
/// a restart records as [`St::launch_model`].
pub(super) fn launch_kept(opts: &Opts, session: &str, argv: &[String]) -> Option<String> {
    let launch = upgrade::launch_model(argv)?;
    models::launch_model_kept(Some(&launch), &load_model_record(opts, session)).map(str::to_owned)
}

/// The model the relaunch line of `session`, launched with `argv`, asks for
/// ([`models::line_model`]): `asks`, else nothing kept of a `--model` the
/// record marks failed. The record is read only where the line asks no model
/// and the launch names one.
pub(super) fn line_model_of<'a>(
    opts: &Opts,
    session: &str,
    argv: &[String],
    asks: Option<&'a str>,
) -> Option<&'a str> {
    if asks.is_some() {
        return asks;
    }
    let launch = upgrade::launch_model(argv)?;
    models::line_model(None, Some(&launch), &load_model_record(opts, session))
}

/// Remember `model` as the one the relaunch of `session` asks for, right
/// BEFORE the line that asks it is typed, so it is never asked for twice (a
/// model then not taken is recorded as failed, [`model_read`]). The ask, for
/// [`take_back_ask`] should that line not be typed; `None` where nothing was
/// recorded.
pub(super) fn record_asked(opts: &Opts, session: &str, model: &str) -> Option<models::Asked> {
    if opts.dry_run || model.is_empty() {
        return None;
    }
    let mut rec = load_model_record(opts, session);
    let asked = rec.asked(model, now_s());
    save_model_record(opts, session, &rec);
    Some(asked)
}

/// The build a session runs, as a relaunch target for a model-only restart:
/// the managed twin when it runs that (every managed relaunch goes through
/// the twin), the native build for a native session, else the process's own
/// executable.
fn same_build(
    targets: &Targets,
    exe: &Path,
    running: &Version,
    running_native: bool,
) -> Option<Candidate> {
    let pick = if running_native {
        targets.native.as_ref()
    } else {
        targets.managed.as_ref()
    };
    if let Some(c) = pick.filter(|c| c.version == *running) {
        return Some(c.clone());
    }
    (!exe.as_os_str().is_empty()).then(|| Candidate {
        exe: exe.to_path_buf(),
        version: running.clone(),
        source: if running_native {
            Source::Native
        } else {
            Source::Managed
        },
    })
}

/// The cheap pre-filter [`due`] and the pass ask: whether this session's
/// model is due (the launch's own `--model` is not read here — it is in
/// [`visit_with_claim`]).
fn model_wants_a_look(opts: &Opts, sf: &SessionFile, mctx: &ModelCtx) -> bool {
    // Which build the session runs is not read here, to stay cheap: a look is
    // wanted when a move is due under either view, and the visit judges it by
    // the real build.
    let views: Vec<bool> = std::iter::once(false)
        .chain(mctx.native.is_some().then_some(true))
        .filter(|&native| !mctx.view(native).1.is_empty())
        .collect();
    let before = load_model_record(opts, &sf.session_id);
    let tail = std::cell::OnceCell::new();
    let now = now_s();
    let reads: Vec<(ModelRecord, ModelRead, Vec<ModelRow>)> = views
        .iter()
        .map(|&native| {
            let mut judged = before.clone();
            let (read, rows) =
                model_judge(opts, sf, &mut judged, &tail, None, mctx, native, now, None);
            (judged, read, rows)
        })
        .collect();
    // Reads are kept only where every view left the record alike (always,
    // with one view). With two, a clock stepped under the view the session
    // does not run would restart the real one on every sweep; the visit,
    // which knows the build, steps it.
    if let Some((first, _, rows)) = reads.first()
        && reads.iter().all(|(judged, _, _)| judged == first)
    {
        model_read_kept(opts, sf, &before, first, rows);
    }
    reads
        .iter()
        .any(|(_, read, _)| matches!(read.verdict, ModelVerdict::Due { .. }))
}

/// `aterm harness upgrade models`: the list best first, each model's
/// availability on the managed build (and why not), every model a restart may
/// ask for (a conversation moves to the newest of its own family first), the
/// list's target, and what Claude Code recommends. Read-only (`opts.dry_run`
/// is honoured by [`ModelCtx::read`]).
#[must_use]
pub fn models_report(opts: &Opts) -> String {
    use std::fmt::Write as _;
    let targets = Targets::read(&opts.home);
    let ctx = ModelCtx::read(opts, &targets);
    let label = targets
        .managed
        .as_ref()
        .map_or_else(|| "?".to_string(), |m| m.version.to_string());
    let ev = model_evidence(opts);
    let (_, verdicts) = models::target(&ctx.list, &label, &ev, ctx.baked.as_ref());
    let mut out = format!("Claude Code (managed): {label}\n");
    for (i, (id, a)) in verdicts.iter().enumerate() {
        let name = ctx
            .baked
            .as_ref()
            .and_then(|b| b.model(id))
            .map_or(String::new(), |m| format!(" ({})", m.display_name));
        let verdict = match a {
            models::Availability::Yes(why) => format!("available — {why}"),
            models::Availability::No(why) => format!("NOT available — {why}"),
        };
        let _ = writeln!(out, "  {}. {id}{name}: {verdict}", i + 1);
    }
    let _ = writeln!(
        out,
        "on offer (a session moves to the newest of its own family first): {}",
        if ctx.offered.is_empty() {
            "none".to_string()
        } else {
            ctx.offered.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "target (then, for a model nobody chose, up this list): {}",
        models::list_target(&ctx.list, &ctx.offered).unwrap_or("none available")
    );
    if let (Some(n), Some((_, offered))) = (targets.native.as_ref(), ctx.native.as_ref()) {
        let _ = writeln!(
            out,
            "native Claude Code {} (a session on it is judged by its own catalog): on offer {}; target {}",
            n.version,
            if offered.is_empty() {
                "none".to_string()
            } else {
                offered.join(", ")
            },
            models::list_target(&ctx.list, offered).unwrap_or("none available")
        );
    }
    for (id, source) in models::recommendations(&ev, ctx.baked.as_ref(), &label) {
        let _ = writeln!(out, "  recommended by Claude Code: {id} ({source})");
    }
    out
}

/// `aterm harness upgrade models set <id>,<id>,...`: replace the list, best
/// first; every entry a Claude model id, no repeats. One ledger row.
///
/// # Errors
/// An entry that is not a model id, a repeat, or a write that failed.
pub fn set_models(opts: &Opts, list: &str) -> Result<Vec<String>, String> {
    let now = i64::try_from(now_s()).unwrap_or(0);
    let path = state_dir(opts).join("models.json");
    let ids: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut p = models::load_or_seed(&path, now).unwrap_or_else(|_| Priority::seed(now));
    let candidate = Priority {
        ids: ids.clone(),
        history: p.history.clone(),
    };
    if ids.is_empty() || Priority::parse(&candidate.render()).is_none() {
        return Err(
            "every entry must be a Claude model id (claude-<family>-<n>[-<n>]), no repeats"
                .to_string(),
        );
    }
    p.ids.clone_from(&ids);
    p.history.push(models::Change {
        id: list.to_string(),
        placed: "set".into(),
        source: "owner".into(),
        at: now,
    });
    models::write_atomic(&path, &p.render()).map_err(|e| format!("{}: {e}", path.display()))?;
    let r = Report {
        pid: 0,
        tab: "-".to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: ids.join(","),
        step: "models-set".to_string(),
    };
    ledger(opts, &r, "the priority list was set by hand");
    Ok(ids)
}

/// How the one signal went.
pub(super) enum Terminated {
    Sent,
    /// Not sent, and why (a hold, or the process no longer leads the
    /// foreground group): a wait, not a failure.
    Refused(String),
    /// The kernel refused a signal we did send.
    Failed,
}

/// SIGTERM to the agent as one server-side decision: `signal term pid=<pid>
/// quiet=<grace_s>` reaches exactly that process, only while it leads the
/// tab's foreground group, and is refused under a hold, while a person gave
/// input within the grace, or while written input sits unread, so nothing
/// after the last `status` read goes unseen. An aterm without `quiet=`
/// answers `ERR bad signal argument: quiet=…`, one without `pid=` `ERR
/// unknown signal`, sending nothing; then the hold is re-read and the older
/// form sent, or the kernel asked directly.
pub(super) fn terminate(c: &mut Client, tab: &str, pid: i32, grace_s: u32) -> Terminated {
    match c.request_line(&format!("@{tab} signal term pid={pid} quiet={grace_s}")) {
        Ok(reply) if reply.starts_with("OK") => Terminated::Sent,
        Ok(reply) if reply.starts_with("ERR bad signal argument: quiet=") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            match c.request_line(&format!("@{tab} signal term pid={pid}")) {
                Ok(reply) if reply.starts_with("OK") => Terminated::Sent,
                Ok(reply) if reply.starts_with("ERR halted") => {
                    Terminated::Refused("held".to_string())
                }
                Ok(_) | Err(_) => Terminated::Refused("refused".to_string()),
            }
        }
        Ok(reply) if reply.starts_with("ERR unknown signal") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            #[cfg(unix)]
            {
                // SAFETY: a plain signal to one verified pid of our own uid.
                if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                    Terminated::Sent
                } else {
                    Terminated::Failed
                }
            }
            // Windows has no SIGTERM for a console process, and this drive
            // never sends `TerminateProcess`: a wait, never a kill. No Windows
            // step reaches here (every job read is `ps -o …`).
            #[cfg(not(unix))]
            {
                let _ = pid;
                Terminated::Refused("unsupported".to_string())
            }
        }
        Ok(reply) if reply.starts_with("ERR halted") || reply.starts_with("ERR busy") => {
            Terminated::Refused("held".to_string())
        }
        Ok(_) | Err(_) => Terminated::Refused("refused".to_string()),
    }
}

// ---------------------------------------------------------------- state

/// ONE ROW OF THE WAIT LOG ([`St::waits`]): when a session's upgrade began to
/// wait on `word` (or acted: `announced:1`, `gave-up`), and the facts that
/// decided it (`note`, from [`wait_note`]): the screen's state, the person
/// stamp, the composer's verdict, the render state, what runs under the
/// agent. Kept so the next "why has this tab not been upgraded" is one read
/// of the record, not a reconstruction from a transcript, a journal and a
/// process table (2026-09-28: the visit-by-visit history of a 7-hour wait
/// was NOT recoverable from disk).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitNote {
    /// Unix seconds.
    pub at: u64,
    /// The wait word, or the act's step word.
    pub word: String,
    /// The deciding facts, one line; empty where none were read.
    pub note: String,
}

/// How many [`WaitNote`]s a record keeps, newest last.
pub const WAITS_KEPT: usize = 24;

/// What a restart's record took when it was written ([`St::signalled`]) and
/// puts back if its signal is never sent ([`St::unsent`]).
#[derive(Debug)]
pub(super) struct Unsent {
    phase: Phase,
    release: String,
    marker: String,
    markers: Vec<String>,
    /// [`St::fallback_from`] as it stood: a restart that carries a hand
    /// choice makes that the origin ([`restart`]), and one never sent did not.
    fallback_from: Option<String>,
}

/// [`St::fallback_from`] on disk for a fallback whose launch named no model.
/// Never a model id.
const FALLBACK_NO_MODEL: &str = "-";

/// [`St::source`] of a restart's or a relaunch's record that kept the
/// session on the program it ran: no build was moved to.
pub(super) const SAME_PROGRAM: &str = "same";

/// What the sweep remembers about one conversation's upgrade. Everything a
/// relaunch needs is written before the agent is signalled, so a sweep that
/// dies between exit and relaunch leaves enough for the next to finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct St {
    pub(super) phase: Phase,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) source: String,
    /// The READY marker of the latest notice ([`upgrade::ready_marker`]).
    pub(super) marker: String,
    /// Every READY marker this round typed and still honours, the latest
    /// included ([`answered`]): notices queued behind a usage limit arrive at
    /// once, and a READY to any of them answers this upgrade. Forgotten with
    /// `marker` ([`St::forget_markers`]) at a void, release or owner's word.
    pub(super) markers: Vec<String>,
    /// The release owed ([`upgrade::release_prompt`]): why the upgrade
    /// abandoned a notice without restarting the agent (`gave-up`, `void`,
    /// `skipped`, `deferred`, a stop's reason), or empty. Set once per
    /// abandonment ([`St::owe_release`]); cleared when typed or dropped
    /// ([`release_void`]), when a new notice supersedes it, or when the
    /// restart ends the agent (its carry-on says continue).
    pub(super) release: String,
    /// Every READY marker this round typed, honoured or not, since
    /// [`St::markers`] forgets them at a void, stop or release while an owed
    /// release still needs them. Never an answer: a READY to one marks the
    /// agent's last answer to the upgrade, after which a direction counts
    /// ([`release_void`], [`upgrade::directed_since_ready`]). Opened by the
    /// round's first notice ([`St::announced`]) and carried with an owed
    /// release onto a new target's state ([`St::for_target`]).
    pub(super) asked: Vec<String>,
    pub(super) salt: u64,
    pub(super) last_seq: u64,
    pub(super) seq_since_s: u64,
    /// The agent that was signalled.
    pub(super) pid: u32,
    /// The process whose tab received the READY notice. `pid` becomes the
    /// process signalled at restart, so notice ownership needs its own fence.
    pub(super) notice_pid: u32,
    pub(super) notice_start: String,
    /// The shell that gets its prompt back.
    pub(super) shell: u32,
    /// The tab the shell lives in.
    pub(super) tab: String,
    /// The line typed at that prompt — once announced and before the
    /// signal, the line the announcement was planned on.
    pub(super) line: String,
    /// Where that prompt left the cursor when a relaunch attempt first read it
    /// ([`PromptMark`]): kept across attempts, so a person's typing at the
    /// prompt is judged against the prompt as it came back, never against
    /// their own half-typed command.
    pub(super) prompt: Option<PromptMark>,
    /// The last held-back reason the ledger was told ([`held_back`]), so it is
    /// said once, or the stuck restart it reported (`stuck:<what>`,
    /// [`note_stuck`]).
    pub(super) noted: String,
    /// Why the agent is being relaunched: empty for this module's upgrade,
    /// [`super::relaunch::CAUSE_EXIT`] for a relaunch on exit — which of the
    /// two continuations the new process is typed.
    pub(super) cause: String,
    /// The model the agent's last turn named when it answered READY
    /// ([`upgrade::transcript_model`] over the tail that proved the answer), or
    /// empty when no turn named one. Said in the continuation and in the
    /// outcome, never acted on: the relaunch carries the launch's own flags.
    pub(super) model_before: String,
    /// Where that tail read ended in the transcript, in bytes: the resumed
    /// session's first turn is the first one written past it. `0`: no mark was
    /// taken (a restart an older build began), and the resumed model is then
    /// unconfirmed rather than read from the start of the conversation.
    pub(super) mark: u64,
    /// The `--model` the relaunch line keeps from the launch
    /// ([`launch_kept`]), or empty when the launch named none or one the
    /// record marks failed: what decides the model a resumed session comes
    /// back on, said in the outcome.
    pub(super) launch_model: String,
    /// The model the relaunch line asks for (the newest of the conversation's
    /// family, else the list's, [`super::upgrade_models`]), or empty when it
    /// keeps the launch's own `--model` or names none. Said in the outcome.
    pub(super) model_list: String,
    /// The fallback origin: the model the launch named before a bucket's
    /// fallback relaunch replaced it (`None`: no fallback stands; `Some("")`:
    /// one stands and the launch named none), what the way back at the
    /// bucket's reset returns to ([`super::relaunch::fallback_origin`]). Set by
    /// the fallback's restart, carried over other restarts while it stands
    /// (replaced by a hand choice such a restart carries), cleared by the way
    /// back. Stored as empty, [`FALLBACK_NO_MODEL`], or the model.
    pub(super) fallback_from: Option<String>,
    /// THE LOGIN THE RELAUNCH LINE CARRIES ([`super::relaunch::Launch::login`]):
    /// the value of `CLAUDE_SECURESTORAGE_CONFIG_DIR` the planner required on
    /// [`St::line`] — the one the ended process had, or an account move's
    /// target — `None` when it required none. The line already holds it once
    /// planned, and a restart left in flight retypes that line; this is the
    /// same fact as a field, for the owner's view and for a host that checks
    /// the new process against it, so neither parses a shell line. On disk
    /// the key `login` is WRITTEN ONLY WHEN SET (empty included): a record
    /// with none is byte for byte what it was, and an older record reads as
    /// none.
    pub(super) login: Option<String>,
    /// Set once the continuation is typed while the resumed session's first
    /// answer is still unread ([`super::relaunch::confirm`]): the epoch second
    /// past which a sweep still finding none records the model unconfirmed.
    /// `0`: nothing pending. The phase is already done, so nothing types the
    /// continuation twice.
    pub(super) confirm_by: u64,
    /// The build the relaunched process runs — its session file's `version`,
    /// the one its transcript rows carry — and its pid, for the `done` row. A
    /// relaunched Codex's pid is stamped as its supervisor ADOPTS it
    /// (`upgrade_codex_drive::adopt`): from then a Codex leaving the tab is
    /// that TUI, never the restart's exit (`relaunch::restarted`). A Claude
    /// Code's is stamped only as its continuation is typed (`continued`).
    pub(super) resumed_on: String,
    pub(super) resumed_pid: u32,
    /// The TUI a Codex restart's relaunch line brought up, as the look that
    /// FOUND it recorded it (`upgrade_codex_drive::relaunched_by_us`), adopted
    /// or not; `0`: none found yet. Once it is gone, a Codex leaving the tab
    /// was that TUI — a person's `/exit`, a crash — never the restart's exit
    /// (`relaunch::restarted`; the review of 2026-09-27: only an adoption
    /// stamped it, and a TUI up in the tab whose adoption ran out of time, or
    /// whose carry-on found a person at the keys, was read as the restart's).
    /// A relaunched Claude Code's is stamped as its supervisor ADOPTS it
    /// (`relaunch::await_new`, cleared as its relaunch line is typed): the
    /// adoption is said once, however many steps find that process (the live
    /// test of 2026-09-30) — never [`St::resumed_pid`], which says its
    /// continuation was typed.
    pub(super) relaunched_pid: u32,
    /// The look that first saw the person's hold standing now
    /// ([`upgrade::person_hold`]), every look since seeing it too, none more
    /// than [`upgrade::HOLD_GAP_S`] apart; `0`: none.
    pub(super) hold_since_s: u64,
    /// The last look that saw that hold ([`upgrade::hold_since`]); `0`: none.
    pub(super) hold_seen_s: u64,
    /// When the READY answer the upgrade acts on was first heard, held at
    /// the session's limit ([`upgrade::ready_since`]); `0`: none now. What
    /// bounds how long the agent's own background work may hold a READY
    /// restart ([`upgrade::Facts::ready_s`]).
    pub(super) ready_since: u64,
    // The owner's view, read by [`upgrade_status`]: none of it gates an act
    // except `request`, the owner's own word.
    /// When the session was first found behind the build it is being moved
    /// to, carried onto a newer target while nothing has begun ([`St::for_target`]):
    /// the age is the session's, not the build's. `0` in a state an older
    /// build wrote, read as `salt` ([`St::behind_since`]).
    pub(super) pending_since: u64,
    /// THE PROCESS THAT AGE COUNTS: the kernel start (`ps lstart`, squashed)
    /// of the Claude Code process this record was minted behind — an age
    /// belongs to ONE process (the owner's tab of 2026-09-28: a
    /// `Pending(to=2.1.283)` record outlived the owner's own restart onto
    /// 2.1.283, and when 2.1.284 came [`St::for_target`] carried its
    /// three-day-old age onto a process that had run the newest build for a
    /// day: "behind for 2 days", a warn row, 11 s after the build appeared).
    /// Empty in a record an older build wrote, read against the build it was
    /// minted behind ([`St::same_process`]).
    pub(super) proc_start: String,
    /// THE WAIT LOG: the last [`WAITS_KEPT`] changes of wait and acts, newest
    /// last ([`WaitNote`]). Gates nothing.
    pub(super) waits: Vec<WaitNote>,
    /// The last wait a step recorded ([`upgrade::wait_word`]); empty once it
    /// acts. The host's watch also records one off any point: its read's wait
    /// or what holds the point its step waits for, else
    /// [`upgrade::NEXT_POINT`] (`upgrade_watch::look_word`).
    pub(super) wait: String,
    /// When that wait began: the first step that recorded the same word.
    pub(super) wait_since: u64,
    /// For a person at the tab (`attended`), when a look last recorded it; a
    /// wait not confirmed since is the last point's word
    /// ([`upgrade_status::Row::wait_seen`]). `0` for every other wait and in
    /// older states.
    pub(super) wait_seen: u64,
    /// What ran under the agent at the last waiting look, by pid, name and
    /// start ([`HeldBy`], [`St::note_held`]): what the owner is told holds the
    /// move. Empty once a step acts.
    pub(super) held_by: Vec<HeldBy>,
    /// How many looks in a row read the screen authoritatively idle
    /// ([`upgrade::Facts::idle_looks`]); 0 after any other look. One process's
    /// run: the looks are of [`St::idle_pid`], each within [`IDLE_RUN_GAP_S`]
    /// of the last ([`St::idle_at`]); another process's screen, or a long gap,
    /// begins the run again.
    pub(super) idle_looks: u32,
    /// The process whose screen those looks read; `0`: none.
    pub(super) idle_pid: u32,
    /// When the last of them was taken (unix seconds); `0`: none.
    pub(super) idle_at: u64,
    /// The repaint-proof settle's run ([`St::still`],
    /// [`upgrade::Facts::still_s`]): the print of the last words the last
    /// authoritatively idle look read ([`upgrade::still_print`]), when the run
    /// of that print began, and its latest look; empty and `0` with no run.
    pub(super) still_print: String,
    pub(super) still_since: u64,
    pub(super) still_at: u64,
    /// Codex: the client daemon's threads found running during that run, each
    /// with its first look (`<thread>@<unix s>`, space-separated; empty with no
    /// run or while the footer shows a goal pursued). One still running
    /// [`upgrade::QUIET_S`] into this screen's still run
    /// ([`codex`]'s `cx::still_through`) is another session's, where it is a
    /// root and another Codex is attached (`cx::daemon_turn`).
    pub(super) still_busy: String,
    /// A wait no rung passes (`upgrade_status::BLOCKERS`) and since when it
    /// has blocked the move ([`St::note_blocked`]): met when the gate passed
    /// and the act was refused (`no-shell-integration`), or when the screen
    /// could not be read (`screen-unreadable`). Kept across other waits
    /// between two such refusals, cleared only by a look that finds it gone.
    /// Empty and `0` while nothing blocks.
    pub(super) blocked: String,
    pub(super) blocked_since: u64,
    /// The owner's word ([`Request`]), written by `aterm harness upgrade <sid>
    /// --now|--defer|--skip` under the sweep lock ([`upgrade_status::ask`]).
    pub(super) request: Request,
    /// The tab the owner named with that word: the word is on the tab, so the
    /// conversation resumed elsewhere carries none of it ([`St::request_for`]).
    /// Empty: no word, or an older record.
    pub(super) request_tab: String,
    /// When (unix seconds) the owner's word was written; `0`: none or older
    /// record. Bounds how long a `--now` keeps an overdue upgrade from reading
    /// stalled ([`upgrade_status::Row::stall`]).
    pub(super) request_at: u64,
    /// The owner's outcome line of a finished restart
    /// ([`super::relaunch::confirm`]) and when it was written — what the window
    /// records once, as the restart's record.
    pub(super) outcome: String,
    pub(super) done_at: u64,
    /// Which agent this upgrade moves ([`upgrade::Agent`]): a Codex state is
    /// filed as `codex-<tab>` and driven by the Codex lane ([`codex`]) alone —
    /// every Claude loop here passes it by.
    pub(super) agent: upgrade::Agent,
    /// Codex: how the TUI ran — `daemon` (a client of the shared daemon),
    /// `embedded` (it held its thread), `fresh` (no conversation yet).
    pub(super) mode: String,
    /// Codex: the thread the TUI held (embedded, by its writer lock) or the
    /// one its exit named (daemon mode); empty until known.
    pub(super) thread: String,
    /// Codex: the `$CODEX_HOME` the TUI ran with.
    pub(super) codex_home: String,
    /// Codex: the TUI's argv, its working directory and the managed twin, as
    /// they were at the exit — what the relaunch is planned from once the
    /// exit names the thread, by which time the TUI is gone.
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) twin: String,
    /// Text the lane typed into the composer and did not submit: on Codex, a
    /// guarded Enter that missed (`turn … reason=guard`); on Claude Code, a
    /// notice or release stopped part-way or with its guarded Enter unpressed
    /// ([`Typed::Cut`]). Kept so later visits say `left-typed` and never read
    /// it as a person's draft; neither lane takes it back ([`left_standing`]
    /// only reads). Carried across a retarget ([`St::for_target`]). Empty:
    /// nothing of the lane's there.
    pub(super) left_typed: String,
    /// Codex: when the lane last left text typed (unix seconds; `0`: never).
    /// The act that missed is not retried for [`upgrade::REASK_S`], so a
    /// standing cause does not type into the person's composer every sweep.
    /// The owner's `--now` lifts it.
    pub(super) left_at: u64,
    /// When the agent a restart ended was seen gone (unix seconds): the Codex
    /// TUI `/exit` was typed into, or the Claude Code SIGTERM was sent to. `0`
    /// while it lives or nothing was sent. Stamped where the agent is seen
    /// gone ([`St::seen_gone`]), never at the signal. A move failing after it
    /// has no process to vet it by; its record is the tab's
    /// (`upgrade_status::Row::exited_at`).
    pub(super) exited_at: u64,
    /// When this round stopped ([`Phase::Failed`], unix seconds), stamped by
    /// every stop ([`St::fail`]) and by the void of a given-up round's late
    /// READY ([`St::void`]); the [`upgrade::RETRY_S`] rest counts from it
    /// ([`St::time_failed`], [`upgrade::Step::Rearm`]). `0` for other phases
    /// and for an older build's stop, which then reads as long ago and is
    /// re-armed at its first look.
    pub(super) failed_at: u64,
    /// When the latest notice was TYPED (unix seconds; `0`: none recorded,
    /// as in a record an older build wrote). Not the re-ask clock: an
    /// announced round's `at_s` is moved on by a usage limit's episode
    /// ([`St::hold_clock`]) and by a void's release ([`St::void`]), when
    /// nothing was asked, so the owner's "last asked" (the Messages row,
    /// `upgrade_status::Row::noticed_at`) reads this instead (review of
    /// 2026-09-28: after a limit the row named the episode's end as the time
    /// a notice was typed). Stamped by every notice of either lane
    /// ([`St::announced`], the Codex lane's own), never moved otherwise, and
    /// carried onto a newer build's round by the retarget of a round that
    /// never restarted its agent ([`St::retarget`]). The Codex lane's quiet move reads
    /// it too: a TUI a notice once reached is carried on after it, not
    /// resumed as if never asked (round eight, #23).
    pub(super) noticed_at: u64,
    /// Why the round BEFORE this one stopped, set by the re-arm that started
    /// this one ([`St::rearm`]) — on a Codex record, by a retarget of a round
    /// that gave up too ([`St::stop_kept_onto`]) — and cleared by its first
    /// notice ([`St::announced`]): the owner's view keeps a refusal's stall through
    /// the new round's first look (`upgrade_status::Row::last_stop`). Gates
    /// one thing: the Codex lane's quiet move of a round whose round before
    /// GAVE UP on its notices is carried on, not resumed as if never asked
    /// (that lane types no release; round eight, #23) — beside
    /// [`St::noticed_at`], for a stop an older build recorded with no stamp.
    pub(super) last_stop: String,
    /// The same stop in a row ([`St::fail`]): why the latest stop happened and
    /// how many consecutive stops had that reason. A failure repeating every
    /// round rests longer each time ([`upgrade::rest_extension`]). Never
    /// terminal.
    pub(super) streak_why: String,
    pub(super) stop_streak: u32,
    /// THE LAST LOOK THAT SAW THE AGENT'S OWN WORK HOLD THE MOVE (unix
    /// seconds; `0`: none): a wait word that is work (`background`,
    /// `not-idle:busy`, …), or processes under the agent
    /// ([`upgrade_status::holds_by_own_work`]). Stamped by the looks and NEVER
    /// cleared by an act: the digest's class is a property of the ROUND, and
    /// an act (a notice, a give-up, a re-arm) clears the wait and the holders
    /// the last look recorded, so a class read from the last look alone
    /// dipped to quiet at every act — a dozen flips in a round, each a new
    /// log line and a dismissed entry back (the safety review of
    /// 2026-09-28). Read against [`upgrade_status::OWN_WORK_HOLD_S`].
    pub(super) own_work_at: u64,
    /// WHEN THE END OF THE AGENT'S OWN WORK LAST ASKED (unix seconds, oldest
    /// first, at most [`upgrade::WORK_END_DAILY`]): the ask that end leads to
    /// is exempt from [`upgrade::MAX_ASKS`], so the driver bounds it — at most
    /// [`upgrade::WORK_END_DAILY`] in any [`upgrade::WORK_END_WINDOW_S`],
    /// [`upgrade::WORK_END_FLOOR_S`] apart ([`St::time_work`]).
    pub(super) work_end_at: Vec<u64>,
    /// A `/goal` WAS IN FORCE at the last look that read the screen (Claude
    /// Code's `◎ /goal active` footer): the owner-facing words name it as what
    /// keeps the agent busy. Gates nothing.
    pub(super) goal: bool,
    /// THE FIRST TIME EACH DISTINCT WAIT WORD WAS RECORDED, for the record's
    /// life (at most [`FIRSTS_KEPT`]): the wait log keeps the last
    /// [`WAITS_KEPT`] rows, six to eight hours of a session asked every half
    /// hour, and a multi-day stall's first cause scrolled off it (the review of
    /// 2026-09-29). Carried across a retarget while the same process is behind
    /// ([`St::adopt`]). Gates nothing.
    pub(super) firsts: Vec<(String, u64)>,
    // THE WATCH'S FIELDS (design record 2026-09-28, "No upgrade stuck
    // forever", §3.2 C1): tab #1 sat on 2.1.280 for three days because every
    // bound the upgrade has lives in a step, and a step runs only at a point
    // the session offers — the thing keeping the time was the event being
    // waited for. These say when the record last MOVED, who last looked at
    // it off a point, and which of the loop's guards withheld the last
    // point, so a deadline ([`due_by`], [`watch_at`]) can be computed from
    // the record alone. None of them gates an act.
    /// When the record's PROGRESS KEY ([`St::progress_key`]) last changed,
    /// in unix seconds: stamped by the one writer ([`save`]) and by nothing
    /// else — no caller can forget to stamp it, and none can stamp it by
    /// accident; [`restore`] puts an earlier record back WITHOUT stamping
    /// ([`write_verbatim`]), so a rolled-back act never restarts the step's
    /// clock. A record an older build wrote reads as `max(at, failed_at,
    /// salt)` ([`St::from_json`]): past due at the first look of a build
    /// that watches.
    pub(super) progress_at: u64,
    /// The key [`St::progress_at`] was stamped for ([`St::progress_key`]):
    /// what the writer compares the record on disk with.
    pub(super) progress_key: String,
    /// When a look off a point (the watch) last read this record (unix
    /// seconds, `0`: never), and the aterm build that looked. Written by the
    /// watch alone; carried by every other save.
    pub(super) looked_at: u64,
    pub(super) looked_by: String,
    /// The last point the session's loop offered the host (an idle point or a
    /// settled break, unix seconds; `0`: none), and the guard that withheld
    /// the latest point it did not offer ([`crate::supervise::Guard::word`];
    /// empty: none). Copied from the window's worker by the watch; nothing
    /// here decides by them.
    pub(super) point_at: u64,
    pub(super) guard: String,
    /// The notice refused in a row: how many looks since the upgrade's last act
    /// could not type the notice (`wait:announce-refused:<why>`,
    /// [`St::note_step`]), first at [`St::refused_at`]. A refused notice spends
    /// no ask, so without this count nothing would follow. Any act resets it;
    /// other waits leave it. Past `upgrade_status::TELL_S` the owner is told
    /// (`upgrade_status::Row::refused_notice`).
    pub(super) refused: u32,
    /// When the first of those refusals was (unix seconds; `0`: none now).
    pub(super) refused_at: u64,
    /// THE AGENT'S OWN WORK WAS SEEN RUNNING since the round's latest notice —
    /// a look at a break of it, or one with processes under the agent — while
    /// the round is open (announced, or resting after it gave up). Cleared by
    /// every notice ([`St::announced`], set again by that very visit when its
    /// look saw work) and by a new round. What makes the first idle point with
    /// nothing under the agent THE END OF ITS WORK ([`St::time_work`],
    /// [`upgrade::Facts::work_ended`]): the point its "not yet" waited for.
    /// Recorded and read by Claude Code's lane alone (the Codex lane re-arms
    /// no resting round at the end of its TUI's work); the owner's view reads
    /// it of a round that gave up (`upgrade_status::Row::work_seen`: a quiet
    /// give-up the agent's own work held, which `--now` does not move — round
    /// eight, #4's review, and its review).
    pub(super) work_seen: bool,
    /// The usage limit's named reset as the transcript said it at the last
    /// look that read it (unix seconds, [`upgrade::transcript_limit_reset`]:
    /// the reset a row names, never the wait
    /// [`upgrade::transcript_limit_until`] assigns a row naming none; for Codex
    /// its screen wall's, `codex::keep_wall`; `0`: none named or never read).
    /// Kept exactly as the latest read says, past or future, and cleared when
    /// the model answers again (a Codex wall's same words keep their first
    /// placement, [`St::limit_text`]), by visit ([`queue_facts`]) and watch
    /// alike, so a later limit's reset is never an earlier one's. Gates
    /// nothing; the owner's view reads a `limited` wait by it
    /// (`upgrade_status::Row::limit_passed`).
    pub(super) limit_until: u64,
    /// The words a Codex wall named [`St::limit_until`] by (its `try again at
    /// <time>`, `codex::wall_reset_text`; empty for none and for Claude Code):
    /// the same words at a later look are the same reset, kept as first placed
    /// (`codex::keep_wall_reset`), since a bare clock time re-placed after
    /// midnight would read as the next day's.
    pub(super) limit_text: String,
    /// The standing limit is a spend or credit wall
    /// ([`aterm_phase::WallKind::Spend`]), as the last look reading
    /// [`St::limit_until`] found it (Codex's screen wall,
    /// `codex::wall_spends`; Claude Code's transcript row,
    /// [`upgrade::transcript_limit_spend`]). Nothing ends it by itself, and
    /// with no named reset no week bounds it; only the account holder lifts
    /// it. Gates nothing; the owner's view reads it
    /// (`upgrade_status::Row::spend_held`).
    pub(super) limit_spend: bool,
    /// The restart under way is quiet ([`upgrade::Facts::quiet`]): nothing was
    /// typed before it and nothing is after; the agent was idle and resumes
    /// idle, the tab's loop deciding its next point as if nothing moved
    /// (`done:resumed`, `super::relaunch::carry_on`). Set with the signal from
    /// a round that typed nothing and owes no release; a round an older aterm
    /// or `upgrade_ask` announced is carried on as before.
    pub(super) quiet: bool,
    /// The restart's own last look recorded the wait: the reducer said go and
    /// [`restart`]'s later checks (the hand, `held`; the last look, `changed`,
    /// `changed-before-signal`; the signal, `signal-<why>`; the plan; the
    /// ownership) waited. A dry read stops at `would-restart`, so the host's
    /// watch keeps this word rather than record that nothing holds the move
    /// ([`St::held_past_go`]). Set by the visit that recorded it; cleared by
    /// every later step record ([`St::note_step`]) and the watch's own. The
    /// notice's fence needs no such mark: `announce-refused:<why>` is unique.
    pub(super) restart_look: bool,
}

/// Which arm of [`St::retarget`] made a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retarget {
    /// The record as it was: already on the build, or its model's.
    Kept,
    /// Moved onto the newer build in place (a round not yet
    /// restarting, for the same model, on the build the session still runs).
    InPlace,
    /// Rebuilt: a new pending round, any release the old notice owes carried.
    Anew,
}

/// HOW A RETARGET LEFT THE ROUND, in the words the ledger says it in — the
/// one wording of the visit's row, the host's re-tab and the watch's
/// (`retargeted:<from>-><to>`), by the arm of [`St::retarget`] that made the
/// record `st`: its notice and any READY still count for an announced round kept
/// or moved in place, STILL PENDING for a pending one (it has neither to
/// keep), STARTED OVER for one rebuilt (any release the old notice owes is
/// carried, unsaid). The review of
/// 2026-09-30: the visit said "pending again" of every retarget it made, an
/// announced round it kept in place with its READY included — and, told by
/// phase and marker, then said "in place" of a pending round the abandoning
/// arm rebuilt (a model move due, a session moved off its round's build),
/// its recorded wait, its last stop and its salt dropped. A STOPPED round the
/// first arm kept rests as it stopped (2026-10-01: a dropped ride says it,
/// [`DroppedRide::say`], and "still pending" was false of it).
pub(super) fn retarget_how(how: Retarget, st: &St) -> &'static str {
    match (how, &st.phase) {
        (Retarget::Kept | Retarget::InPlace, Phase::Announced { .. }) => {
            "its notice and any READY still count"
        }
        (Retarget::Kept | Retarget::InPlace, Phase::Failed(_)) => "kept as it stopped",
        (Retarget::Kept | Retarget::InPlace, _) => "still pending",
        (Retarget::Anew, _) => "started over",
    }
}

/// How many distinct wait words [`St::firsts`] keeps.
pub const FIRSTS_KEPT: usize = 32;

/// How often a look that sees the agent's own work RESTAMPS
/// [`St::own_work_at`]: a quarter of the hold it feeds
/// ([`upgrade_status::OWN_WORK_HOLD_S`]). Stamped at every look, the row changed
/// at every look — one summary to the window and a wake on its GUI thread a
/// minute at a break — the per-minute wake the 2026-09-25 review removed (the
/// safety review of 2026-09-29).
pub const OWN_WORK_RESTAMP_S: u64 = upgrade_status::OWN_WORK_HOLD_S / 4;

/// What one process has answered ([`St::history`]), carried onto the record
/// for the next build while it is the same process ([`St::adopt`]).
struct History {
    streak_why: String,
    stop_streak: u32,
    waits: Vec<WaitNote>,
    own_work_at: u64,
    firsts: Vec<(String, u64)>,
    work_end_at: Vec<u64>,
    goal: bool,
}

/// THE CLAUDE CODE (OR CODEX) PROCESS A RECORD IS ABOUT, as the visit that
/// writes it reads it: its kernel start (`ps lstart`, squashed) and that same
/// instant in unix seconds (`0`: unknown). What [`St::for_target`] carries an
/// age against, and stamps into the record ([`St::proc_start`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Proc<'a> {
    /// The kernel start, squashed; empty: unknown.
    pub(super) start: &'a str,
    /// The same, in unix seconds; `0`: unknown.
    pub(super) started_s: u64,
}
/// The slack past a step's own deadline before the watch reads its record
/// ([`due_by`]): the step that keeps the deadline runs at the session's next
/// point, and a point minutes late is no stall.
pub const WATCH_SLACK: u64 = 5 * 60;

/// The least time between two watches of one record ([`watch_at`]): the
/// re-ask interval, so a stuck record is read at most once per
/// [`upgrade::REASK_S`] and a waking machine climbs one step, not several.
pub const WATCH_GAP: u64 = upgrade::REASK_S;

impl St {
    /// The progress key: what, changed, means the upgrade moved: the phase
    /// (its word carries asks and a stop's reason), the phase's stamp
    /// (`at_s`), the round's salt, the target and the owed release. Wait
    /// words, looks, held-by lists and the READY clock change while nothing
    /// moves and must not restart the step clock. Neither do refused notices
    /// ([`St::refused`], [`St::refused_at`]) or seen work ([`St::work_seen`]):
    /// counting a refusal would push back the deadline the watch needs to see
    /// that very failure. The notice that finally lands moves the phase.
    pub(super) fn progress_key(&self) -> String {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        format!(
            "{} at={at} salt={} to={} release={}",
            self.phase.word(),
            self.salt,
            self.to,
            self.release
        )
    }

    /// [`St::progress_at`] for a record written before it: the latest stamp
    /// the record carries of its own — the phase's `at_s`, the stop's
    /// `failed_at`, the round's salt.
    fn legacy_progress_at(&self) -> u64 {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        at.max(self.failed_at).max(self.salt)
    }

    /// The READY clock, stamped ([`upgrade::ready_since`]) and read into
    /// `facts` ([`upgrade::Facts::ready_s`]): how long the acted-on READY has
    /// stood, held at a limit, restarted by every notice ([`St::announced`]),
    /// cleared with the markers. Both drivers keep it: it bounds the agent's
    /// work under the answer (by an announced upgrade's re-ask, a given-up
    /// one's void).
    pub(super) fn time_ready(&mut self, ready: bool, facts: &mut Facts, now: u64) {
        self.ready_since = upgrade::ready_since(self.ready_since, ready, facts, now);
        if self.ready_since != 0 {
            facts.ready_s = now.saturating_sub(self.ready_since);
        }
    }

    /// The rest's clock read into `facts` ([`upgrade::Facts::failed_s`]): how
    /// long ago a stopped round stopped ([`St::failed_at`]); an unstamped older
    /// stop reads as long ago. Past [`upgrade::RETRY_S`] the reducer re-arms
    /// ([`upgrade::retry_due`]).
    pub(super) fn time_failed(&self, facts: &mut Facts, now: u64) {
        facts.failed_s = self.failed_for(now);
    }

    /// THE END OF THE AGENT'S OWN WORK, read into `facts`
    /// ([`upgrade::Facts::work_ended`]): its work was seen running since the
    /// round's latest notice ([`St::work_seen`]), and this look is an idle
    /// point — no break — with nothing under the agent. Claude Code's lane
    /// alone (the Codex lane's reducer asks nothing of it).
    pub(super) fn time_work(&self, facts: &mut Facts, now: u64) {
        facts.work_ended = self.work_seen
            && self.open_round()
            && !facts.background_point
            && facts.background.is_empty()
            && self.work_end_open(now);
    }

    /// WHETHER THE END OF THE AGENT'S OWN WORK MAY ASK NOW: the ask it leads to
    /// is exempt from the round's [`upgrade::MAX_ASKS`] and from its clock, so
    /// it is bounded here (the safety review of 2026-09-29: an agent that
    /// answered "not yet" by starting a short job and idling was asked again
    /// at the end of every one, each a full model turn on its whole context,
    /// and the round never gave up). Two bounds: [`upgrade::WORK_END_FLOOR_S`]
    /// since the latest notice (for a resting round, since it stopped — NOT
    /// since the end of a repeated stop's longer rest, so the ladder costs an
    /// agent whose work ends nothing), and at most
    /// [`upgrade::WORK_END_DAILY`] asks in any window. Outside them the fact
    /// reads false and the ordinary clock decides: the point goes where it
    /// always went, and the round gives up, rests and reaches the digest.
    fn work_end_open(&self, now: u64) -> bool {
        let since = match &self.phase {
            Phase::Announced { at_s, .. } => now.saturating_sub((*at_s).max(self.noticed_at)),
            // From the STOP, not from the end of a repeated stop's longer
            // rest: `failed_at` sits `rest_extension()` past the stop, and
            // the end of the agent's work asks at once whatever the rest
            // (the ladder binds the clock's asks, never this one).
            Phase::Failed(_) => {
                now.saturating_sub(self.failed_at.saturating_sub(self.rest_extension()))
            }
            _ => return false,
        };
        since >= upgrade::WORK_END_FLOOR_S && self.work_end_left(now) > 0
    }

    /// How many asks the end of the agent's work has left: the day's budget
    /// less the asks of the last [`upgrade::WORK_END_WINDOW_S`] — a rolling
    /// window, so two bursts either side of a boundary are not twelve.
    fn work_end_left(&self, now: u64) -> u32 {
        let spent = self
            .work_end_at
            .iter()
            .filter(|&&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S)
            .count();
        upgrade::WORK_END_DAILY.saturating_sub(u32::try_from(spent).unwrap_or(u32::MAX))
    }

    /// One ask the end of the agent's work led to, spent at `now`.
    fn spend_work_end(&mut self, now: u64) {
        self.work_end_at
            .retain(|&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S);
        self.work_end_at.push(now);
        let keep = usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX);
        if self.work_end_at.len() > keep {
            self.work_end_at.remove(0);
        }
    }

    /// A round the agent's own work may hold open: announced, or resting after
    /// it gave up ([`upgrade::GAVE_UP`]) — the rounds whose "not yet" the end
    /// of that work answers.
    fn open_round(&self) -> bool {
        matches!(&self.phase, Phase::Announced { .. })
            || matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
    }

    /// What this look saw of the agent's own work, kept for the end of it
    /// ([`St::work_seen`]): a break of it, or processes under the agent, while
    /// the round is open.
    fn saw_work(&mut self, facts: &Facts) {
        if self.open_round() && (facts.background_point || !facts.background.is_empty()) {
            self.work_seen = true;
        }
    }

    /// Seconds since this round stopped ([`St::time_failed`]); `0` for a
    /// round that has not.
    pub(super) fn failed_for(&self, now: u64) -> u64 {
        if matches!(self.phase, Phase::Failed(_)) {
            now.saturating_sub(self.failed_at)
        } else {
            0
        }
    }

    /// THE MODEL AN ANNOUNCED ROUND PROMISED ([`St::model_list`] of a
    /// [`Phase::Announced`] round; `None` for any other round, and for one
    /// that promised none): the model its READY consented to, which a
    /// retarget onto a newer build keeps — the model [`St::for_target`] is
    /// handed wherever the record is retargeted, by a visit (whose model
    /// decision returns it first, `models::model_to`) and by the watch alike,
    /// so the watch makes the record what the next visit would (the review
    /// of 2026-09-30: the watch handed it none, and an announced round whose
    /// model rode the restart was abandoned off a point — its READY lost, a
    /// release owed, its asks begun again — where a visit kept it in place).
    /// Only while it still rides: a visit whose read bars it
    /// ([`models::ride_bar`]) drops it from the record ([`ride_dropped`]), so
    /// the visit, the watch and the restart all read the one verdict.
    pub(super) fn promised_model(&self) -> Option<&str> {
        (matches!(self.phase, Phase::Announced { .. }) && !self.model_list.is_empty())
            .then_some(self.model_list.as_str())
    }

    /// THE MODEL A NOTICE OF THIS ROUND CARRIED, which a restart of the round
    /// would still ask for ([`St::model_list`]): an announced round's (its
    /// READY's restart, [`St::promised_model`]) or a gave-up one's (a late
    /// READY still restarts it, [`St::give_up`], and `restart` asks the
    /// record's model) — `None` for any other round, and one that carried
    /// none. What every visit re-judges ([`models::ride_bar`], 2026-10-01):
    /// before, both rode whatever the visit decided.
    pub(super) fn carried_model(&self) -> Option<&str> {
        let restarts = match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        };
        (restarts && !self.model_list.is_empty()).then_some(self.model_list.as_str())
    }

    /// AN ANNOUNCED ROUND LEFT WITH NOTHING TO MOVE at `now` — its model's
    /// ride dropped ([`ride_dropped`]) with the build current and nothing
    /// else due: the agent it asked to wind down is RELEASED
    /// ([`St::owe_release`]) and the round is pending again in a new round,
    /// its markers forgotten so no READY given to it acts — as the owner's
    /// hold ends an announcement (`upgrade_status`'s `--skip`/`--defer`).
    /// Kept announced, its READY would have stood unacted on, the agent
    /// waiting on a restart that never came. Whether it was announced.
    fn release_unridden(&mut self, now: u64) -> bool {
        if !matches!(self.phase, Phase::Announced { .. }) {
            return false;
        }
        self.owe_release("model-dropped");
        self.new_round(now);
        self.phase = Phase::Pending;
        self.forget_markers();
        self.noted.clear();
        true
    }

    fn fresh(from: &Version, to: &Candidate, who: Proc<'_>, now: u64) -> St {
        St {
            from: from.to_string(),
            to: to.version.to_string(),
            source: to.source.as_str().to_string(),
            salt: now,
            seq_since_s: now,
            pending_since: now,
            proc_start: who.start.to_string(),
            ..St::default()
        }
    }

    /// WHETHER THE AGE THIS RECORD CARRIES BELONGS TO THE PROCESS NOW HOLDING
    /// THE CONVERSATION (`holder`: its kernel start, squashed; `running`: the
    /// build it runs). By the process's own start when both are known; for a
    /// record an older build wrote — no process named — by the build the
    /// record was minted behind: a session that runs another build than the
    /// one it was found behind was restarted since, by hand or otherwise, and
    /// the time it spent behind a build it no longer runs is not its age. A
    /// record naming neither is carried, as it always was.
    fn same_process(&self, running: &Version, who: Proc<'_>) -> bool {
        if !self.proc_start.is_empty() && !who.start.is_empty() {
            return self.proc_start == who.start;
        }
        // Unknown is not different: a record naming no build is carried as
        // it always was.
        Version::parse(&self.from).is_none_or(|from| from == *running)
    }

    /// WHAT A PROCESS HAS ANSWERED, to carry onto the next build's record
    /// while the same process is still behind: the stops in a row (the
    /// notices it left unanswered — a whole round given up on is the digest's
    /// premise — and any other stop that repeats), the wait log (the evidence
    /// of why it has not moved), the first time each wait was seen, the last
    /// look that saw its work, the day's asks at the end of that work and the
    /// goal in force.
    fn history(&self) -> History {
        History {
            // The stop in a row, whatever its reason: every `Failed` reason is
            // about the process or its launch line (the first arm of
            // [`St::for_target`] says so), so the streak continues with the
            // process across a new build.
            streak_why: self.streak_why.clone(),
            stop_streak: self.stop_streak,
            waits: self.waits.clone(),
            own_work_at: self.own_work_at,
            firsts: self.firsts.clone(),
            work_end_at: self.work_end_at.clone(),
            goal: self.goal,
        }
    }

    /// Take a process's [`History`] onto this fresh record.
    fn adopt(&mut self, history: History) {
        self.streak_why = history.streak_why;
        self.stop_streak = history.stop_streak;
        self.waits = history.waits;
        self.own_work_at = history.own_work_at;
        self.firsts = history.firsts;
        self.work_end_at = history.work_end_at;
        self.goal = history.goal;
    }

    /// The state for moving onto `to` (and, when the model rule moves the
    /// conversation, onto `model`): the prior one when it
    /// names that version and — once announced — that model, else a fresh one
    /// that keeps what still holds while nothing has begun — how long the
    /// session has been behind, and the owner's word — and, whatever has
    /// begun, the model a bucket's fallback standing replaced
    /// ([`St::fallback_from`]): an upgrade minted over the fallback's record
    /// left the way back nothing to return to. A skip is of ONE
    /// version, so it does not follow onto a newer one. An announcement that named another model is
    /// announced again: the READY it asked for was consent to a different
    /// restart. A round not yet restarting, for the same model, is moved onto
    /// a newer BUILD in place, its READY kept (the owner's ruling of
    /// 2026-09-29: a READY covers any version).
    fn for_target(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        model: Option<&str>,
        who: Proc<'_>,
        now: u64,
    ) -> St {
        Self::retarget(prior, from, to, model, who, now).0
    }

    /// [`St::for_target`], plus which of its arms made the record
    /// ([`Retarget`]), for [`retarget_how`]'s ledger wording (phase and marker
    /// alone cannot tell a rebuilt pending record from one kept in place).
    /// Aterm's own text left in the prompt ([`St::left_typed`]) rides every
    /// arm.
    fn retarget(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        model: Option<&str>,
        who: Proc<'_>,
        now: u64,
    ) -> (St, Retarget) {
        let (left, left_at) = prior
            .as_ref()
            .map(|p| (p.left_typed.clone(), p.left_at))
            .unwrap_or_default();
        let (mut st, how) = match prior {
            // Reused when still Pending, for the same model, or Failed: every
            // `Failed` reason concerns the process or its launch line, so the
            // round rests for the build whatever model would ride it
            // (re-minting on a model comparison would repeat the same refusal
            // every sweep). The reducer re-arms it after `upgrade::RETRY_S`
            // (`St::rearm`).
            Some(mut st)
                if st.to == to.version.to_string()
                    && (matches!(st.phase, Phase::Pending | Phase::Failed(_))
                        || st.model_list == model.unwrap_or_default())
                    // A STOPPED ROUND BELONGS TO ITS PROCESS TOO: the session
                    // the owner restarted by hand onto this still-behind build
                    // never had those rounds — its age, its unanswered rounds
                    // and the processes recorded under the one before it are
                    // not its own (the review of 2026-09-28). It is minted
                    // afresh below, the release its predecessor is owed
                    // carried as any stopped record's is.
                    && !(matches!(st.phase, Phase::Failed(_)) && !st.same_process(from, who)) =>
            {
                // Managed and native builds can share a version, and which one
                // the session moves to follows its launch: until the restart
                // begins, the label names the build the sweep would start now.
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
                    st.source = to.source.as_str().to_string();
                }
                // A PENDING RECORD FOLLOWS ITS PROCESS: the session the
                // owner restarted by hand onto another build (still behind
                // this target) is behind since the restart, not since the
                // record was minted for the process before it.
                let same = st.same_process(from, who);
                if st.phase == Phase::Pending && !same {
                    st.pending_since = now;
                    // Its minting too: the classifier's backstop and no-look
                    // horizons count from the salt ([`upgrade_status::Row::minted_at`]),
                    // and a process restarted minutes ago has not been
                    // stuck for the days its predecessor was. Kept past
                    // every marker the old salt could have made, as a new
                    // round's is ([`St::new_round`]).
                    st.salt = now.max(
                        st.salt
                            .saturating_add(u64::from(upgrade::MAX_ASKS))
                            .saturating_add(1),
                    );
                    st.from = from.to_string();
                    st.wait.clear();
                    st.wait_since = 0;
                    st.wait_seen = 0;
                    st.held_by.clear();
                    // What the old process answered is not this one's
                    // ([`St::history`]): its stop streak and refusals would
                    // read a hand restart as a stop that repeats, and tell the
                    // owner again to do what they just did.
                    st.stop_streak = 0;
                    st.streak_why.clear();
                    st.last_stop.clear();
                    st.refused = 0;
                    st.refused_at = 0;
                    st.waits.clear();
                    st.firsts.clear();
                    st.own_work_at = 0;
                    st.work_end_at.clear();
                    st.goal = false;
                } else if st.proc_start.is_empty()
                    && who.started_s != 0
                    && st.pending_since != 0
                    && st.pending_since < st.salt
                    && st.pending_since < who.started_s
                {
                    // A RECORD AN OLDER BUILD WROTE whose age predates both
                    // its own minting (a retarget carried it) and the process
                    // now holding the conversation cannot be that process's
                    // age (the owner's tab of 2026-09-28: its announced record
                    // still said "behind for 3 days" over a process a day
                    // old): it begins when this target was first seen — the
                    // record's own salt — or when the process began, whichever
                    // is later.
                    st.pending_since = st.salt.max(who.started_s);
                }
                // The record takes the holder's start only where the age it
                // carries is the holder's (or is being reset above): an
                // ANNOUNCED record of another process keeps its own process's
                // start, so the retarget arm below mints the next build's
                // record afresh instead of finding it "the same process" one
                // visit later.
                if !who.start.is_empty() && (same || st.phase == Phase::Pending) {
                    st.proc_start = who.start.to_string();
                }
                (st, Retarget::Kept)
            }
            // A READY COVERS ANY VERSION (the owner's ruling of 2026-09-29,
            // design record 2026-09-28 §3.2 C7d): a round that has not begun
            // restarting, for the SAME model and on the build the session
            // still runs, is RETARGETED IN PLACE onto a newer build. It keeps
            // its phase, its asks, its notice and marker and any READY given
            // to it — a READY says "my work is at a stopping point", not "move
            // me to that version number" — and changes only what it moves to.
            // Before, a newer build abandoned the round: the agent was typed
            // a release line and then a fresh notice, and asked again from the
            // start (two turns typed into it, and the whole round's wait over
            // again, for every vendor build — about daily). A change of the
            // MODEL it promised is still a new consent (the next arm), and so
            // is a session that has moved off the build the round began on.
            // The owner's `--skip` names ONE build, so it does not follow onto
            // another (as the next arm drops it, [`St::word_onto`]).
            //
            // ONLY FOR THE PROCESS THE ROUND WAS ABOUT ([`St::same_process`]):
            // a session the owner restarted by hand onto the same build never
            // had this round's age, notice or READY, so it falls to the next
            // arm, which mints it afresh from now (the review of 2026-09-28).
            Some(mut st)
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    && st.model_list == model.unwrap_or_default()
                    && Version::parse(&st.from).as_ref() == Some(from)
                    && st.same_process(from, who) =>
            {
                st.to = to.version.to_string();
                st.source = to.source.as_str().to_string();
                // A record an older build wrote learns its process here.
                if st.proc_start.is_empty() && !who.start.is_empty() {
                    st.proc_start = who.start.to_string();
                }
                // The same process, as this arm's guard says.
                let word = st.word_onto(to, now, true);
                (st.request, st.request_tab, st.request_at) = word;
                (st, Retarget::InPlace)
            }
            // A notice for another build or model is abandoned here: the agent
            // it reached is owed its release, carried onto the new state until
            // a new notice supersedes it — and what tells the Codex lane, which
            // types no release, that a notice once reached that TUI: its stamp
            // ([`St::noticed_at`]) and a give-up before it
            // ([`St::stop_kept_onto`]; round eight, #23).
            Some(mut old) if matches!(old.phase, Phase::Pending | Phase::Announced { .. }) => {
                old.owe_release("retargeted");
                // The age is the PROCESS's, not the build's: carried onto a
                // newer target only while the same process is still behind
                // (a session the owner restarted onto the old target by hand
                // has been behind the new one since it appeared, which is
                // now, as far as anything here can say).
                let same = old.same_process(from, who);
                let pending_since = if same { old.behind_since() } else { now };
                let history = same.then(|| old.history());
                let fallback_from = fallback_origin(&old);
                let (request, request_tab, request_at) = old.word_onto(to, now, same);
                // What THIS process was told and how its round ended ride
                // with it only ([`St::stop_kept_onto`]): a process restarted
                // by hand was told nothing.
                let (noticed_at, last_stop) = if same {
                    (old.noticed_at, old.stop_kept_onto())
                } else {
                    (0, String::new())
                };
                let mut st = St {
                    pending_since,
                    request,
                    request_tab,
                    request_at,
                    fallback_from,
                    tab: old.tab,
                    release: old.release,
                    asked: old.asked,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    noticed_at,
                    last_stop,
                    ..St::fresh(from, to, who, now)
                };
                if let Some(history) = history {
                    st.adopt(history);
                }
                (st, Retarget::Anew)
            }
            // A stopped upgrade's release still owed follows onto the new one,
            // with the notice's fence: it is typed only to the process that
            // notice reached ([`release_void`]).
            //
            // ONE RULE FOR A STOPPED ROUND, retargeted when a vendor build
            // about daily lands. It is keyed on the PROCESS, not the build:
            // [`St::same_process`] says it by the kernel start where both are
            // known, and, for a record an older build wrote, by the build the
            // session was found behind. The same process, still behind, keeps
            // what a new build does not change: how long it has been BEHIND
            // (a round that gives up and re-arms for ever must not start its
            // clock over with each build), THE SAME STOP IN A ROW (every
            // `Failed` reason is about the process or its launch line, as the
            // first arm says, so a new build is no reason to rest it less),
            // what it has answered ([`St::history`]) and — as the arm above
            // carries them — the notice's stamp and a give-up for the Codex
            // lane's carry-on (round eight, #23: a quiet give-up of a round an
            // older build announced, a newer Codex landed during its rest, and
            // the TUI told to wind down was resumed with nothing typed). A
            // process the round was not about — the session the owner
            // restarted by hand onto this still-behind build never had those
            // rounds — is behind from now, asked afresh. THE OWNER'S WORD is
            // the session's, not the process's or the build's
            // ([`St::word_onto`]): a `--defer` dropped here let the next point
            // announce inside it; a skip names its build, so one of another is
            // not carried — one of the new target, a stopped restart's, is —
            // and a standing `--now` is carried only to the process it
            // hurried.
            Some(old) if matches!(old.phase, Phase::Failed(_)) => {
                let same = old.same_process(from, who);
                let history = same.then(|| old.history());
                let pending_since = if same { old.behind_since() } else { now };
                let fallback_from = fallback_origin(&old);
                let (request, request_tab, request_at) = old.word_onto(to, now, same);
                let (noticed_at, last_stop) = if same {
                    (old.noticed_at, old.stop_kept_onto())
                } else {
                    (0, String::new())
                };
                let mut st = St {
                    pending_since,
                    request,
                    request_tab,
                    request_at,
                    fallback_from,
                    release: old.release,
                    asked: old.asked,
                    tab: old.tab,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    noticed_at,
                    last_stop,
                    ..St::fresh(from, to, who, now)
                };
                if let Some(history) = history {
                    st.adopt(history);
                }
                (st, Retarget::Anew)
            }
            // A finished move, or a restart's own record (a relaunch, a
            // restart in place). One that kept the session ON ITS OWN PROGRAM
            // (`source` [`SAME_PROGRAM`], the session still on that build)
            // moved nothing, so what it kept of the upgrade's record
            // ([`St::kept_by_restart`]) still stands (round eight, #24): how
            // long the session has been behind, and the owner's hold that
            // still keeps it off THIS build — a skip of it, a deferral
            // standing — or a new round would move the session onto the very
            // build the owner skipped. A finished MOVE, or an under-way one,
            // begins a new wait, its word gone, whatever process holds the
            // conversation.
            Some(old) => {
                let kept =
                    old.source == SAME_PROGRAM && Version::parse(&old.to).as_ref() == Some(from);
                let hold = (kept && old.request.holds_build(&to.version.to_string(), now))
                    .then(|| (old.request.clone(), old.request_tab.clone(), old.request_at));
                let mut st = St {
                    fallback_from: fallback_origin(&old),
                    release: old.release,
                    asked: old.asked,
                    tab: old.tab,
                    notice_pid: old.notice_pid,
                    notice_start: old.notice_start,
                    ..St::fresh(from, to, who, now)
                };
                if kept && old.pending_since != 0 {
                    st.pending_since = old.pending_since;
                }
                if let Some((request, request_tab, request_at)) = hold {
                    (st.request, st.request_tab, st.request_at) =
                        (request, request_tab, request_at);
                }
                (st, Retarget::Anew)
            }
            None => (St::fresh(from, to, who, now), Retarget::Anew),
        };
        if st.left_typed.is_empty() {
            st.left_typed = left;
            st.left_at = left_at;
        }
        (st, how)
    }

    /// The owner's word as it applies to this conversation in `tab`: none
    /// when the owner named another tab. `--now` waives a wait for the tab
    /// the owner looked at; resumed elsewhere, the conversation is asked
    /// afresh (review of 2026-09-25: the word was keyed by conversation alone
    /// and followed it into any tab).
    pub(super) fn request_for(&self, tab: &str) -> Request {
        if self.request_tab.is_empty() || self.request_tab == tab {
            self.request.clone()
        } else {
            Request::None
        }
    }

    /// THE OWNER'S WORD A RETARGET ONTO `to` KEEPS of this record at `now`
    /// ([`St::retarget`]'s in-place, abandoning and stopped arms; `same`: the
    /// process holding the conversation is the one the record was about,
    /// [`St::same_process`]): the word, its tab and its stamp — every word but
    /// A SKIP OF ANOTHER BUILD and a `--now` of another process. A word is the
    /// SESSION's: `--defer` ("Not today") follows it to the next build and to
    /// a process the owner restarted by hand — a snooze that ended with the
    /// build it was pressed on let the next point announce inside it (the
    /// safety review of 2026-09-29). A standing `--now` is PERMISSION (it
    /// waives the wait for a person at the tab), so it follows only the
    /// process it hurried: a session the owner has just restarted by hand is
    /// one they are at. A skip names ONE build, so it does not follow onto a
    /// newer one; but one that names `to` itself still holds it
    /// ([`Request::holds_build`]; round eight, #24), whatever process holds
    /// the conversation. A restart's or a relaunch's record keeps the session
    /// on its own program with the owner's skip of the very build the next
    /// retarget moves to ([`St::kept_by_restart`]), and when that restart
    /// STOPPED — a relaunch overtaken or out of time, resumed elsewhere,
    /// refused — the stopped arm dropped every skip unasked: a new round with
    /// no word, which the quiet flow moved onto the skipped build at its next
    /// idle point. An upgrade round's own skip names the build that round
    /// moved to, never a newer one, so for it nothing changes.
    fn word_onto(&self, to: &Candidate, now: u64, same: bool) -> (Request, String, u64) {
        match &self.request {
            Request::Skip(_) if !self.request.holds_build(&to.version.to_string(), now) => {
                (Request::None, String::new(), 0)
            }
            Request::Now if !same => (Request::None, String::new(), 0),
            word => (word.clone(), self.request_tab.clone(), self.request_at),
        }
    }

    /// THE STOP BEFORE THE ROUND A RETARGET OF THIS RECORD BEGINS
    /// ([`St::last_stop`], [`St::retarget`]'s abandoning and stopped arms) —
    /// a Codex record's alone (round eight, #23): a round that GAVE UP is
    /// that stop, as the rest's re-arm would record it ([`St::rearm`]); any
    /// other keeps the stop before it. The Codex lane types no release, so
    /// this and the notice's stamp ([`St::noticed_at`]) are what carry the
    /// TUI's quiet move on after a give-up, not resume it as if never asked;
    /// dropped by a retarget, the TUI told to wind down was resumed with
    /// nothing typed. A Claude Code record's release carries that instead,
    /// and its owner's view of a new build's round is worded as before.
    fn stop_kept_onto(&self) -> String {
        if self.agent != upgrade::Agent::Codex {
            return String::new();
        }
        match &self.phase {
            Phase::Failed(why) if why == upgrade::GAVE_UP => why.clone(),
            _ => self.last_stop.clone(),
        }
    }

    /// THE OWNER'S HOLD this record carries for `tab` ([`St::request_for`]):
    /// a `--skip` or a `--defer` ([`Request::holds_build`]) — `None` for no
    /// word, and for `--now`, which hurries a move and holds none.
    pub(super) fn owners_hold(&self, tab: &str) -> Option<Request> {
        match self.request_for(tab) {
            word @ (Request::Skip(_) | Request::DeferUntil(_)) => Some(word),
            Request::None | Request::Now => None,
        }
    }

    /// WHAT A RESTART OR A RELAUNCH OF THE CONVERSATION ON ITS OWN PROGRAM
    /// KEEPS of this record in `tab`, written onto its own record `st`
    /// (round eight, #24): the owner's hold ([`St::owners_hold`]) — a skip of
    /// the build it did not move to, a deferral still standing — and how long
    /// the session has been behind ([`St::behind_since`]: a round not yet
    /// moved, or a record of the same kind that kept it). Neither moved the
    /// session, so neither is over. Until then the restart in place (a memory
    /// banner, a bucket's fallback) and the relaunch on exit wrote a fresh
    /// record over the upgrade's: the next visit minted a new round with no
    /// word and a move clock started over, and under the quiet flow moved
    /// the session onto the very build the owner had skipped.
    ///
    /// THE MOVE CLOCK ONLY OF A RECORD ABOUT THE BUILD THE RESTART KEEPS
    /// (`st.from`; round eight, #24's review): an upgrade round's `from`, the
    /// build it would move the session off, and a restart's own record's
    /// `to`, the build it kept the session on ([`St::build_behind`]). A
    /// session that has MOVED since — resumed by hand on a newer build, its
    /// round stopped `resumed-elsewhere`, or an older restart's record left
    /// behind — is behind anew from now, as [`St::retarget`]'s stopped and
    /// finished arms rule; copied, its old clock started the next build's
    /// round at the ladder's last rung (no settle, a person's grace cut to a
    /// keystroke's, the quiet restart at the first idle point), its row
    /// overdue at once. A STOPPED restart of the same program is still behind
    /// (a relaunch overtaken or out of time, as [`St::retarget`]'s stopped
    /// arm keeps its clock). The owner's hold is kept whatever build the
    /// record is about: a skip names its own build ([`Request::holds_build`])
    /// and a deferral its own end, and where the retarget's arms differ on a
    /// word after a move by hand, holding the move until the deferral runs
    /// out is the side that never moves a session against the owner's word
    /// (a stopped round's skip of the build it names holds it in the
    /// retarget's first arm, whatever the session has moved to since).
    pub(super) fn kept_by_restart(&self, tab: &str, st: &mut St) {
        if let Some(word) = self.owners_hold(tab) {
            st.request = word;
            st.request_tab.clone_from(&self.request_tab);
            st.request_at = self.request_at;
        }
        let behind = match &self.phase {
            Phase::Pending | Phase::Announced { .. } => self.is_upgrade(),
            Phase::Failed(_) => self.is_upgrade() || self.source == SAME_PROGRAM,
            Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                self.source == SAME_PROGRAM
            }
        };
        let same_build = self
            .build_behind()
            .is_some_and(|build| Version::parse(&st.from).as_ref() == Some(&build));
        if behind && same_build && self.pending_since != 0 {
            st.pending_since = self.pending_since;
        }
    }

    /// THE BUILD THIS RECORD SAYS THE SESSION IS BEHIND ON, for what a
    /// restart keeps of it ([`St::kept_by_restart`]): an upgrade round's
    /// `from` — the build the round would move it off — and any other
    /// record's `to`, the build its restart or relaunch brought the session
    /// up on (for one on its own program, the build it ran). `None` where
    /// that is no version.
    fn build_behind(&self) -> Option<Version> {
        Version::parse(if self.is_upgrade() {
            &self.from
        } else {
            &self.to
        })
    }

    /// A NEW ROUND of this upgrade: the owner's word arms it afresh
    /// ([`upgrade_status::ask`]). The salt is minted again, past every marker
    /// the old one could have made (`salt + asks`, asks at most
    /// [`upgrade::MAX_ASKS`]), so no READY given before the word can answer a
    /// notice typed after it (review of 2026-09-25: a re-armed notice reused
    /// its marker). How long the session has been behind is kept: a state an
    /// older build wrote read that off the salt.
    ///
    /// The refused-notice count ([`St::refused`]) starts over too, so an
    /// `Upgrade now` pressed on a refused-notice row does not immediately read
    /// the same stall.
    fn new_round(&mut self, now: u64) {
        if self.pending_since == 0 {
            self.pending_since = self.salt;
        }
        self.refused = 0;
        self.refused_at = 0;
        self.salt = now.max(
            self.salt
                .saturating_add(u64::from(upgrade::MAX_ASKS))
                .saturating_add(1),
        );
    }

    /// Since when the session has been behind: [`Self::pending_since`], or
    /// the second the state was minted for a state an older build wrote.
    pub(super) fn behind_since(&self) -> u64 {
        if self.pending_since == 0 {
            self.salt
        } else {
            self.pending_since
        }
    }

    /// Where this upgrade stands on the ladder at `now` ([`upgrade::rung`] of
    /// time behind, [`Self::behind_since`]). A state with no known start (no
    /// record, no salt) is at the first rung, since an unknown age relaxes
    /// nothing. The visit reads the ladder's clock here ([`ladder_look`], the
    /// Codex visit's looks), including the goal pause's Land (with `--now`);
    /// as a read of the clock, `HarnessUpgradeGoalPause::Advance` is waived as
    /// the clock's step (`goal_environment`) and aged through this reader in
    /// its Tier-1 bind.
    pub(super) fn rung_at(&self, now: u64) -> upgrade::Rung {
        match self.behind_since() {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

    /// The repaint-proof settle ([`upgrade::Facts::still_s`]): carries this
    /// look's `print` of the last words ([`upgrade::still_print`]; `None` for a
    /// look not authoritatively idle) into the run of looks at the same print,
    /// each within [`IDLE_RUN_GAP_S`] of the last, and returns the seconds since
    /// the run began. A new print, a longer gap, or a non-idle look restarts it
    /// (`0`). Repaints that leave the words (a footer clock, a goal counter, a
    /// dim suggestion) are not in the print and restart nothing.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "HarnessUpgradeLadder",
            action = "Look",
            project = "aterm_agent::harness::upgrade_drive::ladder_projection"
        )
    )]
    pub(super) fn still(&mut self, print: Option<String>, now: u64) -> u64 {
        let Some(print) = print else {
            self.still_print.clear();
            (self.still_since, self.still_at) = (0, 0);
            return 0;
        };
        let run = self.still_since != 0
            && self.still_print == print
            && now.saturating_sub(self.still_at) <= IDLE_RUN_GAP_S;
        if !run {
            self.still_print = print;
            self.still_since = now;
        }
        self.still_at = now;
        now.saturating_sub(self.still_since)
    }

    /// A wait no rung passes, met or found gone ([`St::blocked`]): `Some` the
    /// blocker a look met (its clock kept while the same one stands), `None` a
    /// look that found the move no longer blocked by it (the gate passed and
    /// the act went past it, or the screen read). A look that says neither (a
    /// floor held the gate) calls nothing and the clock stands, so waits that
    /// alternate with the blocker do not reset it.
    pub(super) fn note_blocked(&mut self, blocker: Option<&str>, now: u64) {
        match blocker {
            Some(word) if self.blocked == word => {}
            Some(word) => {
                self.blocked = word.to_string();
                self.blocked_since = now;
            }
            None => {
                self.blocked.clear();
                self.blocked_since = 0;
            }
        }
    }

    /// Record the wait `word` a sweep found, keeping when it began while the
    /// same word repeats.
    pub(super) fn note_wait(&mut self, word: &str, now: u64) {
        if self.wait != word {
            self.wait = word.to_string();
            self.wait_since = now;
            self.log_wait(word, now);
        }
        self.wait_seen = if word == "attended" { now } else { 0 };
        // The word alone says the agent's own work holds the move (a turn, a
        // shell, a workflow): stamped at every look, never by an act.
        if upgrade_status::holds_by_own_work(&self.wait, false) {
            self.saw_own_work(now);
        }
    }

    /// The agent's own work was seen holding the move at `now`
    /// ([`St::own_work_at`]): restamped at most every [`OWN_WORK_RESTAMP_S`], so
    /// the record — and the row the window is sent — does not change at every
    /// look.
    fn saw_own_work(&mut self, now: u64) {
        if self.own_work_at == 0 || now.saturating_sub(self.own_work_at) >= OWN_WORK_RESTAMP_S {
            self.own_work_at = now;
        }
    }

    /// Add `word` to the wait log ([`St::waits`]), the oldest going past
    /// [`WAITS_KEPT`].
    fn log_wait(&mut self, word: &str, now: u64) {
        self.waits.push(WaitNote {
            at: now,
            word: word.to_string(),
            note: String::new(),
        });
        if self.waits.len() > WAITS_KEPT {
            self.waits.remove(0);
        }
        if self.firsts.len() < FIRSTS_KEPT && !self.firsts.iter().any(|(w, _)| w == word) {
            self.firsts.push((word.to_string(), now));
        }
    }

    /// Attach the facts a look decided on to the wait log's newest row, when
    /// that row is this look's (`now`): only a CHANGE of wait wrote one.
    fn annotate_wait(&mut self, now: u64, note: &str) {
        if let Some(last) = self.waits.last_mut()
            && last.at == now
            && last.note.is_empty()
        {
            last.note = super::one_line(note, 240);
        }
    }

    /// What a visit's report says about waiting: a `wait:` step is recorded
    /// ([`upgrade::wait_word`], with Claude's `status` beside `not-idle`); an
    /// act clears it; `skip:`/`busy:`/`would-` change nothing. A
    /// `held-back:<why>` here is the first sweep of a wait said once
    /// ([`attended_once`]: `held-back:attended`, then `wait:attended`), so it is
    /// recorded as that wait, never as an act.
    fn note_step(&mut self, step: &str, status: &str, now: u64) {
        // Whose word this is, the visit says after (`St::restart_look`).
        self.restart_look = false;
        if refused(step) {
            if self.refused == 0 {
                self.refused_at = now;
            }
            self.refused = self.refused.saturating_add(1);
        }
        if let Some(why) = step
            .strip_prefix("wait:")
            .or_else(|| step.strip_prefix("held-back:"))
        {
            self.note_wait(&upgrade::wait_word(why, status), now);
        } else if !step.starts_with("skip:") && !step.starts_with("would-") {
            if !self.wait.is_empty() || step.starts_with("announced") {
                self.log_wait(step, now);
            }
            self.wait.clear();
            self.wait_since = 0;
            self.wait_seen = 0;
            // An act: the notice owed is typed, or no longer owed.
            self.refused = 0;
            self.refused_at = 0;
        }
    }

    /// Whether the recorded wait is one the act's own last checks recorded,
    /// past where a dry read of that act stops. A dry `would-announce` is
    /// answered before the notice's fence reads the screen, and
    /// `would-restart…` before [`restart`] takes the hand, looks last and
    /// signals. Where the last real visit went past the go and one of those
    /// checks waited (the fence refused, `announce-refused:<why>`, or the
    /// restart's last look waited, [`St::restart_look`]), the dry `read` cannot
    /// see what held it, and recording `upgrade::NEXT_POINT` would drop the
    /// `TELL_S` warning (`Row::refused_notice`). A read of another act (a READY
    /// heard since a refused re-ask) gets its own word.
    pub(super) fn held_past_go(&self, read: &str) -> bool {
        let act = read.split_once(AT_POINT).map_or(read, |(act, _)| act);
        if act == "would-announce" {
            refused(&format!("wait:{}", self.wait))
        } else {
            (act == "would-restart" || act.starts_with("would-restart:")) && self.restart_look
        }
    }

    /// What holds the move, for the owner ([`St::held_by`]): what runs under the
    /// agent at a look whose step waited (`held`, named by [`Kernel::describe`]
    /// at `now`), by pid, name and start, never its command. An act, or a look
    /// finding nothing running, clears it.
    fn note_held(&mut self, step: &str, held: &[upgrade::Held], now: u64) {
        if !step.starts_with("wait:") {
            self.held_by.clear();
            return;
        }
        // `ps` ages to the second at a different moment than `now`: keep the
        // start a process already had so it reads the same at every look.
        let kept = |pid: u32, name: &str, since: u64| {
            self.held_by
                .iter()
                .find(|o| o.pid == pid && o.name == name && o.since.abs_diff(since) <= 2)
                .map_or(since, |o| o.since)
        };
        self.held_by = held
            .iter()
            .map(|h| HeldBy {
                pid: h.pid,
                name: h.name.clone(),
                since: kept(h.pid, &h.name, now.saturating_sub(h.age_s)),
            })
            .collect();
        // Processes under the agent at a look that waited: its own work,
        // remembered past the acts that clear the list ([`St::own_work_at`]).
        if upgrade_status::holds_by_own_work(&self.wait, !self.held_by.is_empty()) {
            self.saw_own_work(now);
        }
    }

    fn to_json(&self) -> String {
        let (name, at, asks, why) = match &self.phase {
            Phase::Pending => ("pending", 0, 0, ""),
            Phase::Announced { at_s, asks } => ("announced", *at_s, *asks, ""),
            Phase::Exiting { at_s } => ("exiting", *at_s, 0, ""),
            Phase::Relaunched { at_s } => ("relaunched", *at_s, 0, ""),
            Phase::Done => ("done", 0, 0, ""),
            Phase::Failed(w) => ("failed", 0, 0, w.as_str()),
        };
        let mut o = Map::new();
        for (k, v) in [
            ("phase", name),
            ("why", why),
            ("from", &self.from),
            ("to", &self.to),
            ("source", &self.source),
            ("marker", &self.marker),
            ("notice_start", &self.notice_start),
            ("tab", &self.tab),
            ("line", &self.line),
            (
                "prompt",
                &self
                    .prompt
                    .as_ref()
                    .map(PromptMark::word)
                    .unwrap_or_default(),
            ),
            ("noted", &self.noted),
            ("cause", &self.cause),
            ("model_before", &self.model_before),
            ("launch_model", &self.launch_model),
            ("model_list", &self.model_list),
            (
                "fallback_from",
                match self.fallback_from.as_deref() {
                    None => "",
                    Some("") => FALLBACK_NO_MODEL,
                    Some(m) => m,
                },
            ),
            ("resumed_on", &self.resumed_on),
            ("wait", &self.wait),
            ("request", &self.request.word()),
            ("request_tab", &self.request_tab),
            ("outcome", &self.outcome),
            ("agent", self.agent.word()),
            ("mode", &self.mode),
            ("thread", &self.thread),
            ("codex_home", &self.codex_home),
            ("cwd", &self.cwd),
            ("twin", &self.twin),
            ("left_typed", &self.left_typed),
            ("release", &self.release),
            ("last_stop", &self.last_stop),
            ("streak_why", &self.streak_why),
            ("still_print", &self.still_print),
            ("still_busy", &self.still_busy),
            ("blocked", &self.blocked),
            ("proc_start", &self.proc_start),
            ("progress_key", &self.progress_key),
            ("looked_by", &self.looked_by),
            ("guard", &self.guard),
            ("limit_text", &self.limit_text),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        if let Some(login) = &self.login {
            o.insert("login".into(), Value::from(login.as_str()));
        }
        for (k, v) in [
            ("at", at),
            ("asks", u64::from(asks)),
            ("salt", self.salt),
            ("last_seq", self.last_seq),
            ("seq_since", self.seq_since_s),
            ("pid", u64::from(self.pid)),
            ("notice_pid", u64::from(self.notice_pid)),
            ("shell", u64::from(self.shell)),
            ("mark", self.mark),
            ("confirm_by", self.confirm_by),
            ("resumed_pid", u64::from(self.resumed_pid)),
            ("relaunched_pid", u64::from(self.relaunched_pid)),
            ("hold_since", self.hold_since_s),
            ("hold_seen", self.hold_seen_s),
            ("ready_since", self.ready_since),
            ("pending_since", self.pending_since),
            ("wait_since", self.wait_since),
            ("wait_seen", self.wait_seen),
            ("done_at", self.done_at),
            ("request_at", self.request_at),
            ("exited_at", self.exited_at),
            ("left_at", self.left_at),
            ("idle_looks", u64::from(self.idle_looks)),
            ("idle_pid", u64::from(self.idle_pid)),
            ("idle_at", self.idle_at),
            ("failed_at", self.failed_at),
            ("noticed_at", self.noticed_at),
            ("stop_streak", u64::from(self.stop_streak)),
            ("still_since", self.still_since),
            ("still_at", self.still_at),
            ("blocked_since", self.blocked_since),
            ("progress_at", self.progress_at),
            ("looked_at", self.looked_at),
            ("point_at", self.point_at),
            ("refused", u64::from(self.refused)),
            ("refused_at", self.refused_at),
            ("work_seen", u64::from(self.work_seen)),
            ("own_work_at", self.own_work_at),
            ("goal", u64::from(self.goal)),
            ("limit_until", self.limit_until),
            ("quiet", u64::from(self.quiet)),
            ("limit_spend", u64::from(self.limit_spend)),
            ("restart_look", u64::from(self.restart_look)),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        let held_by: Vec<String> = self
            .held_by
            .iter()
            .map(|h| format!("{} {} {}", h.pid, h.name, h.since))
            .collect();
        let waits: Vec<String> = self
            .waits
            .iter()
            .map(|w| format!("{} {} {}", w.at, w.word, w.note))
            .collect();
        let firsts: Vec<String> = self
            .firsts
            .iter()
            .map(|(word, at)| format!("{at} {word}"))
            .collect();
        let work_end_at: Vec<String> = self.work_end_at.iter().map(u64::to_string).collect();
        for (k, list) in [
            ("argv", &self.argv),
            ("markers", &self.markers),
            ("asked", &self.asked),
            ("held_by", &held_by),
            ("waits", &waits),
            ("firsts", &firsts),
            ("work_end_at", &work_end_at),
        ] {
            if !list.is_empty() {
                o.insert(
                    k.into(),
                    Value::Array(list.iter().map(|a| Value::from(a.as_str())).collect()),
                );
            }
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<St> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let small = |k: &str| u32::try_from(n(k)).unwrap_or(0);
        let list = |k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let phase = match s("phase").as_str() {
            "pending" => Phase::Pending,
            "announced" => Phase::Announced {
                at_s: n("at"),
                asks: u32::try_from(n("asks")).unwrap_or(u32::MAX),
            },
            "exiting" => Phase::Exiting { at_s: n("at") },
            "relaunched" => Phase::Relaunched { at_s: n("at") },
            "done" => Phase::Done,
            "failed" => Phase::Failed(s("why")),
            _ => return None,
        };
        let mut st = St {
            phase,
            from: s("from"),
            to: s("to"),
            source: s("source"),
            marker: s("marker"),
            markers: list("markers"),
            release: s("release"),
            asked: list("asked"),
            salt: n("salt"),
            last_seq: n("last_seq"),
            seq_since_s: n("seq_since"),
            pid: small("pid"),
            notice_pid: small("notice_pid"),
            notice_start: s("notice_start"),
            shell: small("shell"),
            tab: s("tab"),
            line: s("line"),
            prompt: PromptMark::parse(&s("prompt")),
            noted: s("noted"),
            cause: s("cause"),
            model_before: s("model_before"),
            mark: n("mark"),
            launch_model: s("launch_model"),
            model_list: s("model_list"),
            fallback_from: match s("fallback_from") {
                m if m.is_empty() => None,
                m if m == FALLBACK_NO_MODEL => Some(String::new()),
                m => Some(m),
            },
            login: v.get("login").and_then(Value::as_str).map(str::to_owned),
            confirm_by: n("confirm_by"),
            resumed_on: s("resumed_on"),
            resumed_pid: small("resumed_pid"),
            relaunched_pid: small("relaunched_pid"),
            hold_since_s: n("hold_since"),
            hold_seen_s: n("hold_seen"),
            ready_since: n("ready_since"),
            pending_since: n("pending_since"),
            wait: s("wait"),
            wait_since: n("wait_since"),
            wait_seen: n("wait_seen"),
            // `<pid> <name> <since>` each; a malformed one is left out.
            held_by: list("held_by")
                .iter()
                .filter_map(|h| {
                    let mut it = h.split_whitespace();
                    let held = HeldBy {
                        pid: it.next()?.parse().ok()?,
                        name: it.next()?.to_string(),
                        since: it.next()?.parse().ok()?,
                    };
                    it.next().is_none().then_some(held)
                })
                .collect(),
            idle_looks: small("idle_looks"),
            idle_pid: small("idle_pid"),
            idle_at: n("idle_at"),
            request: Request::parse(&s("request")).unwrap_or_default(),
            request_tab: s("request_tab"),
            request_at: n("request_at"),
            outcome: s("outcome"),
            done_at: n("done_at"),
            agent: upgrade::Agent::parse(&s("agent")),
            mode: s("mode"),
            thread: s("thread"),
            codex_home: s("codex_home"),
            argv: list("argv"),
            cwd: s("cwd"),
            twin: s("twin"),
            left_typed: s("left_typed"),
            left_at: n("left_at"),
            exited_at: n("exited_at"),
            failed_at: n("failed_at"),
            noticed_at: n("noticed_at"),
            last_stop: s("last_stop"),
            streak_why: s("streak_why"),
            stop_streak: small("stop_streak"),
            still_print: s("still_print"),
            still_since: n("still_since"),
            still_at: n("still_at"),
            still_busy: s("still_busy"),
            blocked: s("blocked"),
            blocked_since: n("blocked_since"),
            own_work_at: n("own_work_at"),
            proc_start: s("proc_start"),
            // `<at> <word> <note>` each; a malformed one is left out.
            waits: list("waits")
                .iter()
                .filter_map(|w| {
                    let mut it = w.splitn(3, ' ');
                    Some(WaitNote {
                        at: it.next()?.parse().ok()?,
                        word: it.next()?.to_string(),
                        note: it.next().unwrap_or_default().to_string(),
                    })
                })
                .collect(),
            work_end_at: list("work_end_at")
                .iter()
                .filter_map(|t| t.parse().ok())
                .take(usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX))
                .collect(),
            goal: n("goal") != 0,
            firsts: list("firsts")
                .iter()
                .filter_map(|w| {
                    let (at, word) = w.split_once(' ')?;
                    Some((word.to_string(), at.parse().ok()?))
                })
                .take(FIRSTS_KEPT)
                .collect(),
            progress_at: n("progress_at"),
            progress_key: s("progress_key"),
            looked_at: n("looked_at"),
            looked_by: s("looked_by"),
            point_at: n("point_at"),
            guard: s("guard"),
            refused: small("refused"),
            refused_at: n("refused_at"),
            work_seen: n("work_seen") != 0,
            limit_until: n("limit_until"),
            limit_text: s("limit_text"),
            quiet: n("quiet") != 0,
            limit_spend: n("limit_spend") != 0,
            restart_look: n("restart_look") != 0,
        };
        // A record from before the progress stamp ([`St::progress_at`]): its
        // key is what it holds now and it last moved no later than its own
        // latest stamp, so the writer stamps it only once it changes and an
        // old record is past due at once.
        if v.get("progress_key").is_none() {
            st.progress_key = st.progress_key();
            st.progress_at = st.legacy_progress_at();
        }
        Some(st)
    }

    pub(super) fn in_flight(&self) -> bool {
        matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    /// THE PROCESS THIS RESTART'S RELAUNCH BROUGHT UP IS GONE ALREADY: the
    /// one it found or adopted ([`St::relaunched_pid`]) is no process `alive`
    /// finds. The ONE rule both lanes' relaunch on exit closes such a record
    /// in flight by, as one overtaken (`relaunch::after_exit_as`,
    /// `upgrade_codex_drive::after_exit`): the window's host tries an exit
    /// again on the agent before it when the agent its relaunch started
    /// ended before anything read it (round two of the joint audit of
    /// 2026-10-01), and carried on, that try held the tab for a dead process
    /// until the record went stale — the Codex lane's 90 seconds at a time,
    /// the harness's hand on the tab throughout.
    pub(super) fn relaunched_gone(&self, alive: impl Fn(u32) -> bool) -> bool {
        self.relaunched_pid != 0 && !alive(self.relaunched_pid)
    }

    /// THIS RESTART IN FLIGHT, OVERTAKEN, IS CLOSED at `now` for `why`: an
    /// exit of ANOTHER process than the one it names — the agent its
    /// relaunch brought up, or a later one — or of one it brought up gone
    /// already ([`Self::relaunched_gone`]). Never the end of the agent this
    /// record names, which its relaunch had already seen gone: no failure
    /// after ITS exit ([`Self::exited_at`] cleared,
    /// `upgrade_status::Row::failed_after_exit`), so the owner's view vets
    /// the stop against a holder as any stop's. The ONE close both lanes'
    /// relaunch on exit makes of such a record (`relaunch::after_exit_as`,
    /// `upgrade_codex_drive::after_exit`): the Claude lane's since the review
    /// of 2026-09-27; the Codex lane kept the old TUI's stamp until round
    /// three of the joint audit of 2026-10-01, and an upgrade's relaunch
    /// whose Codex then ended by its own orderly exit read "The upgrade ended
    /// it and aterm could not start it again" for a day, marked and
    /// notified.
    pub(super) fn close_overtaken(&mut self, why: &str, now: u64) {
        self.exited_at = 0;
        self.stop(why, now);
    }

    /// WHETHER THIS RECORD IS A RELAUNCH OF AGENT `pid`'S OWN EXIT: a
    /// relaunch on exit ([`super::relaunch::CAUSE_EXIT`], or
    /// [`super::relaunch::CAUSE_FRESH`] for one with nothing to resume)
    /// naming that very process — filed only once an attempt of that exit
    /// read it as a crash (a Claude Code's record left, a Codex's shell
    /// status). Both lanes' relaunch on exit decide on it an attempt of the
    /// same exit that read nothing as it was seen
    /// ([`super::relaunch::ExitRecord::Unread`]) and comes after that
    /// relaunch landed and was gone again before anything read the agent it
    /// started (the window's host tries the exit again, rounds two and three
    /// of the joint audit of 2026-10-01): what the crash left is gone by then
    /// — the relaunched Claude Code's start sweeps a dead agent's record, and
    /// a TUI's command block is no longer its tab's newest — and read again
    /// it said a graceful exit, or none, and the crash an earlier attempt of
    /// this same exit relaunched was dropped with nothing said.
    pub(super) fn relaunched_on_exit_of(&self, pid: u32) -> bool {
        self.pid == pid
            && (self.cause == super::relaunch::CAUSE_EXIT
                || self.cause == super::relaunch::CAUSE_FRESH)
    }

    /// A restart under way that has not moved at `now`: `exiting`, `exited` or
    /// `relaunched` past [`STALE_S`] ([`upgrade_status::stuck_on`], shared with
    /// the owner's view so the ledger's note, [`note_stuck`], and `--status`'s
    /// `stalled=stuck:<what>` agree); `None` otherwise.
    pub(super) fn stuck(&self, now: u64) -> Option<&'static str> {
        upgrade_status::stuck_on(&self.phase, self.exited_at, now)
    }

    /// Whether this record is an upgrade's (listed in the owner's view), not a
    /// relaunch's filed beside it ([`St::cause`]): an agent relaunched after an
    /// exit, memory or model restart is no upgrade; the upgrade's own afresh
    /// restart of a taskless conversation
    /// ([`super::relaunch::CAUSE_UPGRADE_FRESH`]) is.
    pub(super) fn is_upgrade(&self) -> bool {
        self.cause.is_empty() || self.cause == super::relaunch::CAUSE_UPGRADE_FRESH
    }

    /// The signalled agent seen gone at `now` ([`St::exited_at`]): kept from
    /// the first look that saw it, never stamped while [`St::pid`] lives (a
    /// refused signal or an unfinished exit is no exit). Returns whether it
    /// was newly seen.
    pub(super) fn seen_gone(&mut self, now: u64) -> bool {
        if self.exited_at != 0 || self.pid == 0 || alive(self.pid) {
            return false;
        }
        self.exited_at = now;
        true
    }

    /// Whether a typed continuation's model is still to be read
    /// ([`super::relaunch::confirm`]).
    pub(super) fn confirming(&self) -> bool {
        self.phase == Phase::Done && self.confirm_by != 0
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }

    // The upgrade's own transitions: what every step that types, gives up,
    // voids or stops does to the record, one place each so the release a
    // notice owes is never forgotten on some path
    // (`harness_upgrade_never_strands_model` is bound to these).

    /// Whether a notice of this round of a Claude upgrade reached an agent
    /// still alive to hold for it: announced, or given up. A restart past its
    /// signal has no such agent (its conversation is the carry-on's, or a
    /// person's who resumed it elsewhere, where "nothing will restart this
    /// session" would be false). A relaunch record ([`St::cause`]) typed no
    /// notice, and a Codex record is its own lane's.
    fn notified(&self) -> bool {
        self.agent == upgrade::Agent::Claude
            && self.cause.is_empty()
            && match &self.phase {
                Phase::Announced { .. } => true,
                Phase::Failed(why) => why == upgrade::GAVE_UP,
                Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                    false
                }
            }
    }

    /// Whether this phase acts on a READY to the round's markers: announced, or
    /// given up but still hearing a late answer ([`upgrade::next_step`]). No
    /// other phase may let one hold the release ([`upgrade::gate_release`],
    /// [`heard`]).
    fn hears_ready(&self) -> bool {
        match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        }
    }

    /// The READY markers of THIS round ([`St::marker`], [`St::markers`]): the
    /// ones an answer counts for ([`answered`]). None typed yet, or forgotten
    /// by a void or a release, is none.
    pub(super) fn round_markers(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.marker)
            .chain(&self.markers)
            .filter(|m| !m.is_empty())
    }

    /// The upgrade abandons its notice for `why`: one release line is owed
    /// ([`St::release`]), once per abandonment with the first reason kept, if a
    /// notice of this round reached the agent.
    pub(super) fn owe_release(&mut self, why: &str) {
        if self.release.is_empty() && self.notified() {
            self.release = why.to_string();
        }
    }

    /// The upgrade stops at `now` for `why` (a refusal, a failed signal or
    /// relaunch, a conversation resumed elsewhere): [`Phase::Failed`] stamped
    /// ([`St::fail`]; the round rests [`upgrade::RETRY_S`], then re-arms), the
    /// owed release if the agent was asked, and the round's markers forgotten:
    /// no stopped phase acts on a READY, and a standing READY would otherwise
    /// hold the release (`wait:release:ready`) for ever.
    pub(super) fn stop(&mut self, why: &str, now: u64) {
        self.owe_release(why);
        self.forget_markers();
        self.fail(why, now);
    }

    /// The round stops at `now` for `why`: [`Phase::Failed`] and the stamp its
    /// rest counts from ([`St::failed_at`]). Every stop of either lane comes
    /// through here.
    pub(super) fn fail(&mut self, why: &str, now: u64) {
        if self.streak_why == why {
            self.stop_streak = self.stop_streak.saturating_add(1);
        } else {
            why.clone_into(&mut self.streak_why);
            self.stop_streak = 1;
        }
        self.phase = Phase::Failed(why.to_string());
        // The rest counts from here; a stop that repeats pushes it later
        // ([`Self::rest_extension`]).
        self.failed_at = now.saturating_add(self.rest_extension());
    }

    /// HOW MUCH LATER THAN [`upgrade::RETRY_S`] the next round starts, for the
    /// stop the streak is on: a whole round the agent declined rests by the
    /// ladder of its own ([`upgrade::unanswered_rest_extension`]: 2 h, 4 h,
    /// 8 h, a day), every other stop that repeats by
    /// [`upgrade::rest_extension`] (2 h, 4 h, 8 h at the cap). ONE function
    /// for the stop ([`Self::fail`]) and the void that begins the rest again
    /// ([`Self::void`]), so the two never stretch one rest by two ladders.
    fn rest_extension(&self) -> u64 {
        if self.streak_why == upgrade::GAVE_UP {
            upgrade::unanswered_rest_extension(self.stop_streak)
        } else {
            upgrade::rest_extension(self.stop_streak)
        }
    }

    /// A NEW ROUND of a stopped upgrade ([`upgrade::Step::Rearm`]), at `now`:
    /// pending again, with a fresh salt ([`St::new_round`], so no READY given
    /// to the stopped round answers the new one), its markers forgotten, its
    /// asks reset (a pending round's first notice is ask 1), the owner's
    /// `--now` spent (it hurried the round that stopped; the new one keeps
    /// every wait a person is owed), the stop's stamp cleared and why it
    /// stopped kept as [`St::last_stop`] until the new round's first notice.
    /// The stopped round's saved prompt position ([`St::prompt`]) goes with
    /// it: a relaunch that was refused left the prompt as it stood then, and
    /// the new round's relaunch reads the prompt it finds (the no-stall
    /// review of 2026-09-27). A release still owed stays owed: the new
    /// round's first notice supersedes it, and a hold of the owner's that
    /// waits it types it. A relaunch's record re-armed is the upgrade's own
    /// from here. The new round inherits nothing of the old one's restart:
    /// neither when its agent was seen gone ([`St::exited_at`]) nor which
    /// process it brought up or adopted ([`St::relaunched_pid`],
    /// [`St::resumed_pid`]) — kept, a later stop before any signal read as
    /// one after the agent's exit, and a re-armed Codex restart's exit as
    /// that old TUI's own. Answers why the round had stopped, for the ledger
    /// (`rearmed:<why>`).
    pub(super) fn rearm(&mut self, now: u64) -> String {
        let why = match std::mem::take(&mut self.phase) {
            Phase::Failed(why) => why,
            other => other.word(),
        };
        self.last_stop.clone_from(&why);
        self.forget_markers();
        self.new_round(now);
        self.failed_at = 0;
        self.prompt = None;
        self.exited_at = 0;
        self.resumed_pid = 0;
        self.relaunched_pid = 0;
        self.noted.clear();
        // A fallback's own record an older build wrote kept its origin in
        // its cause alone: kept past the cause.
        self.fallback_from = fallback_origin(self);
        self.cause.clear();
        self.hold_since_s = 0;
        self.hold_seen_s = 0;
        self.work_seen = false;
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
        why
    }

    /// The restart's record, written before its SIGTERM ([`restart`]): the
    /// agent `pid`, the `shell` whose prompt returns in `tab`, the relaunch
    /// `line`, and [`Phase::Exiting`] at `now`, consuming the READY it acts on
    /// (the markers, no longer honoured once signalled) and any release owed
    /// by an earlier notice (the carry-on says continue). Returns what was
    /// taken, for [`St::unsent`] if the signal is never sent.
    pub(super) fn signalled(
        &mut self,
        pid: u32,
        shell: u32,
        tab: &str,
        line: String,
        now: u64,
    ) -> Unsent {
        let back = Unsent {
            phase: std::mem::replace(&mut self.phase, Phase::Exiting { at_s: now }),
            release: std::mem::take(&mut self.release),
            marker: std::mem::take(&mut self.marker),
            markers: std::mem::take(&mut self.markers),
            fallback_from: self.fallback_from.clone(),
        };
        self.pid = pid;
        self.shell = shell;
        tab.clone_into(&mut self.tab);
        self.line = line;
        // A new signal: its agent is not seen gone yet.
        self.exited_at = 0;
        back
    }

    /// The signal a restart [`St::signalled`] recorded was never sent (the
    /// session changed, the server refused): the phase, READY, release and
    /// fallback origin it took are put back as they were.
    pub(super) fn unsent(&mut self, back: Unsent) {
        self.phase = back.phase;
        self.release = back.release;
        self.marker = back.marker;
        self.markers = back.markers;
        self.fallback_from = back.fallback_from;
    }

    /// The kernel refused the signal of a recorded restart at `now`: the agent
    /// answered READY and lives on, stopped, so the round stops
    /// (`signal-refused`, [`St::stop`]) and the agent is owed its release.
    pub(super) fn signal_failed(&mut self, back: Unsent, now: u64) {
        self.unsent(back);
        self.stop("signal-refused", now);
    }

    /// [`Step::GiveUp`] at `now`: the round stops asking ([`upgrade::GAVE_UP`],
    /// stamped by [`St::fail`]), the agent is owed its release, and the markers
    /// stay honoured until the release is typed (a READY first restarts it);
    /// [`upgrade::RETRY_S`] later a new round asks ([`St::rearm`]). The owner's
    /// `--now` is spent with this round, so a late READY hours later restarts
    /// under the ordinary waits, not with the settle and attended guard waived.
    fn give_up(&mut self, now: u64) {
        self.owe_release("gave-up");
        self.fail(upgrade::GAVE_UP, now);
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
    }

    /// [`Step::Void`] at `now`: the READY answer a person held past the drain
    /// (or, for an upgrade that gave up asking, the agent's own background
    /// work) is void — the markers forgotten,
    /// so no answer given to them can end the agent later — and the agent
    /// released, with an announced upgrade's re-ask clock restarted, so the
    /// next notice comes [`upgrade::REASK_S`] after the release, not on its
    /// heels. A gave-up upgrade stays given up, its release still owed, and
    /// its rest before the next round begins again at the void
    /// ([`St::failed_at`]): the release the void owes is typed and stands
    /// a whole [`upgrade::RETRY_S`] before a new round's notice follows it —
    /// stretched as a stop that repeats stretches it
    /// ([`Self::rest_extension`]: a whole round declined by its ladder, any
    /// other stop by [`upgrade::rest_extension`]), and never sooner
    /// than the rest the stop already earned (round six, F27: the void
    /// stamped `now` over a rest still six hours out, so a session whose own
    /// work outlived every round's late READY was asked four times every two
    /// and a half hours, the back-off never applied to exactly the stop that
    /// repeats most). A lane that types no release (Codex) keeps its clock:
    /// its re-ask is what moves the agent on.
    fn void(&mut self, now: u64) {
        self.owe_release("void");
        self.forget_markers();
        if let Phase::Announced { asks, .. } = self.phase
            && !self.release.is_empty()
        {
            self.phase = Phase::Announced { at_s: now, asks };
        }
        if matches!(self.phase, Phase::Failed(_)) {
            let rest = now.saturating_add(self.rest_extension());
            self.failed_at = self.failed_at.max(rest);
        }
    }

    /// The `asks`-th notice, READY marker `marker`, typed at `now`: it asks
    /// afresh, so it supersedes a release still owed. The round's earlier
    /// markers stay honoured beside it.
    fn announced(&mut self, marker: String, now: u64, asks: u32) {
        // A first notice opens its round: nothing earlier is its answer.
        if self.phase == Phase::Pending {
            self.markers.clear();
            self.asked.clear();
        }
        if !self.markers.contains(&marker) {
            self.markers.push(marker.clone());
        }
        if !self.asked.contains(&marker) {
            self.asked.push(marker.clone());
        }
        self.marker = marker;
        self.phase = Phase::Announced { at_s: now, asks };
        self.noticed_at = now;
        // A round that typed a notice is carried on after its restart.
        self.quiet = false;
        self.release.clear();
        self.ready_since = 0;
        self.last_stop.clear();
        // Work seen before this notice is no answer to it: the visit that
        // typed it stamps what it saw ([`St::time_work`]).
        self.work_seen = false;
    }

    /// The release line was typed: nothing is owed, and the agent was told
    /// the upgrade is off for now — so no READY to this round's notices may.
    fn released(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The owed release was dropped ([`release_void`]): nothing is owed, and
    /// the round is over as if the line had been typed, markers forgotten.
    fn dropped(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    /// The notice's process is gone ([`notice_process_gone`]) and a process the
    /// upgrade never asked holds the conversation in `tab` (resumed by hand
    /// after the notified agent exited or crashed). The upgrade becomes pending
    /// in a new round ([`St::new_round`]) in the holder's tab, and the visit
    /// continues as a pending upgrade's, asking that holder afresh only while
    /// the conversation is still behind.
    ///
    /// Forgotten: the markers, the round's `asked`, the notice's fence and the
    /// gone process's looks ([`St::idle_looks`], [`St::held_by`]); the salt is
    /// minted past every old marker, since both processes share one transcript
    /// and the old READY must never answer a notice to the new one. The release
    /// owed to the gone process (which [`release_void`] would drop anyway) is
    /// returned (empty: none) for the caller to say once. Time behind is kept.
    /// The owner's word stays on its named tab ([`St::request_for`]), so a
    /// `--skip`/`--defer` keeps its fence (`upgrade_status::Row::of`); a `--now`
    /// is spent with the finished round, as at a give-up or re-arm
    /// ([`St::give_up`], [`St::rearm`]). Person-hold clocks start again.
    fn notice_gone(&mut self, tab: &str, now: u64) -> String {
        let dropped = std::mem::take(&mut self.release);
        self.forget_markers();
        self.asked.clear();
        self.new_round(now);
        self.phase = Phase::Pending;
        self.notice_pid = 0;
        self.notice_start.clear();
        self.noted.clear();
        // The gone process's looks are not the holder's: its idle run counts
        // toward no lag of the new one, and what ran under it holds nothing.
        self.idle_looks = 0;
        self.idle_pid = 0;
        self.idle_at = 0;
        self.held_by.clear();
        self.work_seen = false;
        tab.clone_into(&mut self.tab);
        self.hold_since_s = 0;
        self.hold_seen_s = 0;
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
        dropped
    }

    /// Holds the upgrade's clocks through a limit that stood until `until`
    /// ([`hold_clock`]): an announced upgrade's re-ask clock
    /// ([`upgrade::clock_held_until`]) and the READY clock ([`St::ready_since`])
    /// start no sooner. Returns whether either moved.
    fn hold_clock(&mut self, until: u64) -> bool {
        let phase = upgrade::clock_held_until(&self.phase, until);
        let ready_since = if self.ready_since == 0 {
            0
        } else {
            self.ready_since.max(until)
        };
        let moved = phase != self.phase || ready_since != self.ready_since;
        self.phase = phase;
        self.ready_since = ready_since;
        moved
    }

    /// Forget every READY marker this round issued ([`answered`] then finds
    /// no answer, whatever the transcript holds).
    pub(super) fn forget_markers(&mut self) {
        self.marker.clear();
        self.markers.clear();
        self.ready_since = 0;
    }
}

pub(super) fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

fn state_path(opts: &Opts, session: &str) -> PathBuf {
    state_dir(opts).join(format!("{session}.json"))
}

/// A PENDING RECORD THE SESSION HAS SETTLED, for a session with nothing to
/// move (`current`): its state goes, and the ledger says why once. Two
/// records qualify, neither begun (no notice, nothing owed):
///
/// * a SAME-BUILD restart (`from == to`: the build current, the priority
///   list's model behind) for a session whose model the list no longer moves
///   (a person's `/model`, a list set since): nothing is owed;
/// * a record for a build the session now RUNS OR PASSED (`to <= running`):
///   the owner restarted it by hand, or Claude Code's own updater did. Kept,
///   it lay in wait for the NEXT build and lent it its age (the owner's tab
///   of 2026-09-28: `for_target` carried a three-day-old age onto 2.1.284
///   and posted a "behind for 2 days" warn row 11 s after the build
///   appeared), and read PENDING to the owner's view for as long as the
///   conversation lives.
///
/// A build upgrade for a newer build is never forgotten here; nor is an
/// announced one, which the agent was already asked about, nor a stopped
/// round (its release may be owed, and its stop is the record of why), nor a
/// record that holds THE OWNER'S WORD still in force (a `--defer` whose time
/// has not come: "Not today" is about this session, not the build it was
/// pressed on — the retarget arm carries it to the next build, so closing
/// the record on the way must not delete it; the fin-upgrade2 review of
/// 2026-09-29). Answers whether a record was dropped for having been reached
/// by hand.
fn forget_settled_pending(
    opts: &Opts,
    session: &str,
    prior: Option<&St>,
    running: &Version,
) -> Option<St> {
    let model_only = |st: &St| st.from == running.to_string() && st.to == running.to_string();
    let reached = |st: &St| Version::parse(&st.to).is_some_and(|to| to <= *running);
    let now = now_s();
    let word_in_force = |st: &St| matches!(st.request, Request::DeferUntil(t) if t > now);
    let st = prior
        .filter(|st| st.phase == Phase::Pending && st.release.is_empty())
        .filter(|st| !word_in_force(st))
        .filter(|st| model_only(st) || reached(st))?;
    if opts.dry_run {
        return None;
    }
    remove(opts, session);
    (!model_only(st)).then(|| st.clone())
}

/// How long a settled upgrade's state outlives the last live Claude that held
/// its conversation ([`prune_states`]), or the tab its Codex ran in
/// ([`codex::prune`]).
pub(super) const STATE_KEEP_S: u64 = 7 * 24 * 60 * 60;

/// Whether an upgrade's record is settled (done, failed, or pending with no
/// owner's word on it): the one test both lanes' prunes use ([`prune_states`],
/// [`codex::prune`]). A restart in flight (the orphan pass finishes it), an
/// announcement (it may still be answered) and an owner's
/// `--now`/`--defer`/`--skip` (shown in `--status`) are not.
pub(super) fn settled(st: &St) -> bool {
    match st.phase {
        Phase::Done | Phase::Failed(_) => true,
        Phase::Pending => st.request == Request::None,
        _ => false,
    }
}

/// Whether the state file `entry` has sat unwritten for [`STATE_KEEP_S`] at
/// `now` (unix seconds). A file whose write time cannot be read has not.
pub(super) fn sat_a_week(entry: &std::fs::DirEntry, now: u64) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|at| now.saturating_sub(at.as_secs()) >= STATE_KEEP_S)
}

/// Removes the state of conversations nothing will visit again: no live
/// session file names it, the file has not been written for [`STATE_KEEP_S`],
/// and the upgrade is [`settled`]. A conversation resumed later is asked
/// afresh, as a week-old state would anyway. Called under the sweep lock and
/// only with a session-file list that was read (an unreadable one proves
/// nothing dead). The Codex lane's per-tab records are [`codex::prune`]'s.
fn prune_states(opts: &Opts, files: &[SessionFile], now: u64) {
    if opts.dry_run {
        return;
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return;
    };
    let live: std::collections::BTreeSet<&str> =
        files.iter().map(|f| f.session_id.as_str()).collect();
    for entry in dir.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if live.contains(id) || !sat_a_week(&entry, now) {
            continue;
        }
        let Some(st) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| St::from_json(&text))
        else {
            continue;
        };
        if st.agent == upgrade::Agent::Claude && settled(&st) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(super) fn load(opts: &Opts, session: &str) -> Option<St> {
    St::from_json(&std::fs::read_to_string(state_path(opts, session)).ok()?)
}

/// The one writer of `session`'s record: [`save_at`] at this second.
pub(super) fn save(opts: &Opts, session: &str, st: &St) {
    save_at(opts, session, st, now_s());
}

/// Writes `st` as `session`'s record at `now`, stamping its progress: when its
/// progress key ([`St::progress_key`]) differs from the record on disk (or
/// there is none) it is written as having moved at `now`
/// ([`St::progress_at`]); otherwise the stamp on disk is kept, so a stale copy
/// saved again restarts no clock. Detecting progress at the single writer
/// means no step can forget or fake the stamp. A dry run writes nothing.
pub(super) fn save_at(opts: &Opts, session: &str, st: &St, now: u64) {
    if opts.dry_run {
        return;
    }
    let key = st.progress_key();
    let on_disk = load(opts, session);
    let mut out = st.clone();
    match on_disk {
        Some(disk) if disk.progress_key == key => out.progress_at = disk.progress_at,
        _ => out.progress_at = now,
    }
    out.progress_key = key;
    write_verbatim(opts, session, &out);
}

/// Writes `st` as `session`'s record exactly as it is, progress stamp
/// included. [`save_at`] writes through it after stamping; [`restore`] uses
/// it alone, so putting back a record after an act that did not go ahead is
/// no progress. A dry run writes nothing.
fn write_verbatim(opts: &Opts, session: &str, st: &St) {
    if opts.dry_run {
        return;
    }
    let path = state_path(opts, session);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, st.to_json()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Puts back `session`'s record from before an act that did not go ahead:
/// `prior` as read, progress stamp included ([`write_verbatim`]), or no
/// record.
pub(super) fn restore(opts: &Opts, session: &str, prior: Option<&St>) {
    if opts.dry_run {
        return;
    }
    match prior {
        Some(st) => write_verbatim(opts, session, st),
        None => remove(opts, session),
    }
}

/// The one remover of `session`'s record: a record owed nothing more goes
/// here, as does the absence [`restore`] puts back. The record's path
/// ([`state_path`]) is reached only by this, [`load`] and [`write_verbatim`]
/// (`tools/grep_guard.sh` S1 fences every other harness function), so no step
/// writes or removes a record past the one writer that stamps progress
/// ([`save_at`]). A dry run removes nothing.
pub(super) fn remove(opts: &Opts, session: &str) {
    if opts.dry_run {
        return;
    }
    let _ = std::fs::remove_file(state_path(opts, session));
}

/// One ledger line per step taken: `<state>/upgrade/ledger.jsonl`. `detail`
/// is the step's own: for a `relaunched`, an `exited` and a
/// `failed:relaunch:…` row it is THE LINE TYPED, whole — its `cd '<cwd>'`
/// and, for a session on one of the owner's listed accounts, the login
/// directory the line carries (`CLAUDE_SECURESTORAGE_CONFIG_DIR='<dir>'`:
/// a path under the owner's state directory, never a credential). So an
/// account's directory IS in this ledger, by path; its label alone is in
/// the supervisor's journal and approval ledger.
pub(super) fn ledger(opts: &Opts, r: &Report, detail: &str) {
    if opts.dry_run {
        return;
    }
    let mut o = Map::new();
    o.insert("t".into(), Value::from(now_s()));
    o.insert("pid".into(), Value::from(r.pid));
    for (k, v) in [
        ("tab", &r.tab),
        ("session", &r.session),
        ("from", &r.from),
        ("to", &r.to),
        ("step", &r.step),
    ] {
        o.insert(k.into(), Value::from(v.as_str()));
    }
    o.insert("detail".into(), Value::from(detail));
    let dir = state_dir(opts);
    let _ = std::fs::create_dir_all(&dir);
    let path = ledger_path(opts);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{}",
            aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
        );
    }
    // Cut to its newest rows past the bound, like the disk journal
    // ([`super::cli::bound_ledger`]); every write holds the sweep lock, so no
    // other sweeper appends between the cut's read and its rename.
    let _ = super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS);
}

/// `<state>/upgrade/ledger.jsonl` — what `aterm harness ledger upgrade` reads.
#[must_use]
pub fn ledger_path(opts: &Opts) -> PathBuf {
    state_dir(opts).join("ledger.jsonl")
}

/// The ledger size past which it is cut to its newest [`LEDGER_KEEP_ROWS`]
/// rows: the disk journal's bound ([`super::cli::DISK_LEDGER_MAX_BYTES`]).
const LEDGER_MAX_BYTES: u64 = super::cli::DISK_LEDGER_MAX_BYTES;

/// How many rows survive a cut ([`LEDGER_MAX_BYTES`]).
const LEDGER_KEEP_ROWS: usize = super::cli::DISK_LEDGER_KEEP_ROWS;

// ---------------------------------------------------------------- claude files

#[cfg(test)]
thread_local! {
    /// Counts complete directory scans, not candidates, in one test thread.
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A partial roster cannot prove a conversation has only one live owner: a
/// sibling JSON that may be mid-write, unreadable or malformed invalidates the
/// scan until the next sweep, if a live process may stand behind it.
///
/// Only a live process can own a conversation. Claude Code writes
/// `<pid>.json` for its own process, so a file not named `<digits>.json` is
/// not one of its records, and an unreadable one whose pid is not running (a
/// dead agent's truncated file, an old shape) names no holder: neither is
/// waited on, so one stale sibling cannot block every upgrade. A record whose
/// file name and `pid` disagree still vetoes while either process runs.
pub(super) fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    session_files_in(&home.join(".claude"))
}

/// [`session_files`] of the Claude Code directory `claude_dir` itself —
/// `CLAUDE_CONFIG_DIR`, or `~/.claude`: its `sessions/`.
pub(super) fn session_files_in(claude_dir: &Path) -> Option<Vec<SessionFile>> {
    #[cfg(test)]
    SESSION_FILE_SCANS.with(|count| count.set(count.get() + 1));
    let dir = std::fs::read_dir(claude_dir.join("sessions")).ok()?;
    let mut files = Vec::new();
    for entry in dir {
        let path = entry.ok()?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(file_pid) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u32>().ok())
        else {
            continue;
        };
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| upgrade::parse_session_file(&text).ok());
        match file {
            Some(file) if file.pid == file_pid => files.push(file),
            Some(file) if !alive(file_pid) && !alive(file.pid) => {}
            None if !alive(file_pid) => {}
            _ => return None,
        }
    }
    Some(files)
}

/// The new Claude process creates a new `<pid>.json` in this directory. During
/// its launch, checking every session file every 250 ms reparses the whole
/// history even when the directory has not changed. Keep a complete successful
/// snapshot until the directory moves, and re-read it at least once a second:
/// an in-place write to an existing JSON file does not change the directory's
/// mtime. An unreadable or incomplete snapshot is retried on the next poll.
pub(super) const SESSION_ROSTER_RESCAN: Duration = Duration::from_secs(1);

/// [`session_files`] behind [`SESSION_ROSTER_RESCAN`]'s cache: what the
/// relaunch's wait for the new process reads (`relaunch::await_new`).
#[derive(Default)]
pub(super) struct SessionRosterCache {
    stamp: Option<SystemTime>,
    scanned_at: Option<Instant>,
    files: Option<Vec<SessionFile>>,
}

impl SessionRosterCache {
    pub(super) fn files(&mut self, home: &Path, now: Instant) -> Option<&[SessionFile]> {
        let stamp = std::fs::metadata(home.join(".claude/sessions"))
            .and_then(|m| m.modified())
            .ok();
        self.files_at(stamp, now, || session_files(home))
    }

    fn files_at(
        &mut self,
        stamp: Option<SystemTime>,
        now: Instant,
        scan: impl FnOnce() -> Option<Vec<SessionFile>>,
    ) -> Option<&[SessionFile]> {
        if self.files.is_none()
            || stamp.is_none()
            || stamp != self.stamp
            || self
                .scanned_at
                .is_none_or(|at| now.saturating_duration_since(at) >= SESSION_ROSTER_RESCAN)
        {
            // Record the stamp before scanning. A file created during the
            // scan then forces another read on the next poll.
            self.stamp = stamp;
            self.scanned_at = Some(now);
            self.files = scan();
        }
        self.files.as_deref()
    }
}

/// State is keyed by conversation, so a READY marker can be acted on only
/// when exactly one kernel-verified live process holds that conversation.
/// An unreadable kernel start is ambiguous and fails closed.
fn unique_live_owner(files: &[SessionFile], wanted: &SessionFile) -> bool {
    let mut owner = None;
    for sf in files.iter().filter(|sf| sf.session_id == wanted.session_id) {
        if !alive(sf.pid) {
            continue;
        }
        let Some(start) = kernel_start(sf.pid) else {
            return false;
        };
        if start != squash(&sf.proc_start) {
            continue;
        }
        if owner.replace(sf.pid).is_some() {
            return false;
        }
    }
    owner == Some(wanted.pid)
}

/// Preliminary owner check against the sweep's already-complete roster. It may
/// skip an irrelevant or ambiguous candidate, but it never authorizes an act:
/// those still re-read the whole directory at their own decision point.
fn preliminary_unique_owner(
    files: Option<&[SessionFile]>,
    wanted: &SessionFile,
) -> Result<(), &'static str> {
    let files = files.ok_or("session-files-unreadable")?;
    unique_live_owner(files, wanted)
        .then_some(())
        .ok_or("conversation-owner-ambiguous")
}

/// Fresh, complete directory read for every decision that can type, signal or
/// consume a notice. The initial sweep roster is only a candidate index.
pub(super) fn require_unique_owner(home: &Path, wanted: &SessionFile) -> Result<(), &'static str> {
    let files = session_files(home);
    preliminary_unique_owner(files.as_deref(), wanted)
}

pub(super) fn conversation_live(files: &[SessionFile], session: &str) -> bool {
    files
        .iter()
        .filter(|sf| sf.session_id == session)
        .any(|sf| {
            alive(sf.pid)
                && kernel_start(sf.pid).is_none_or(|start| start == squash(&sf.proc_start))
        })
}

/// The host prefilters with one cheap group lookup. Only a process whose
/// group uniquely matches a local PTY foreground group incurs focused `ps`
/// and argv reads. The later visit still applies the newer job-control and
/// terminal-owner guards; a group match alone never authorizes an act.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostClaim {
    tab: String,
    group: i64,
    parent: u32,
}

/// A record whose agent leads a tab, its process read whole: what the host
/// inspects further ([`host_candidates`]).
type Claimed<'a> = (&'a SessionFile, atpkg::caller_shell::ProcArgs, HostClaim);

/// The records whose agent leads a tab and that `needs_inspection` asks
/// about, each claimed, or, where its process could not be read whole (ids or
/// argv unread, or the tab's foreground moving between roster and ids), the
/// step's wait (`wait:`[`UNREAD_PROCESS`]): nothing was read to decide on, so
/// nothing is decided, rather than silently dropping it. A record whose
/// readable environment names another tab is left out.
fn host_candidates<'a>(
    files: &'a [SessionFile],
    live_tabs: &[LiveTab],
    mut needs_inspection: impl FnMut(&SessionFile, &str) -> bool,
    mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    mut read_group: impl FnMut(u32) -> Option<i64>,
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Vec<Result<Claimed<'a>, Report>> {
    if !live_tabs.iter().any(|tab| tab.fgpgid.is_some()) {
        return Vec::new();
    }
    files
        .iter()
        .filter(|sf| {
            sf.kind == "interactive"
                && sf.entrypoint == "cli"
                && upgrade::is_session_id(&sf.session_id)
        })
        .filter_map(|sf| {
            let group = read_group(sf.pid)?;
            let tab = unique_tab_for_group(live_tabs, group)?;
            if !needs_inspection(sf, tab) {
                return None;
            }
            let read = read_ids(sf.pid)
                .filter(|&(_, checked_group, tty_front)| {
                    checked_group == group && tty_front == group
                })
                .and_then(|(parent, _, _)| Some((parent, read_args(sf.pid)?)));
            let Some((parent, args)) = read else {
                return Some(Err(Report {
                    tab: tab.to_string(),
                    step: format!("wait:{UNREAD_PROCESS}"),
                    ..blank(sf)
                }));
            };
            if args
                .env_var("ATERM_PARENT_SESSION_ID")
                .is_some_and(|env_tab| env_tab != tab)
            {
                return None;
            }
            Some(Ok((
                sf,
                args,
                HostClaim {
                    tab: tab.to_string(),
                    group,
                    parent,
                },
            )))
        })
        .collect()
}

/// The host need not inspect a process or build a whole process table when
/// every known target is already at or below the version Claude reported.
/// A native target remains a possibility until the process is examined, and
/// an in-flight restart must keep its recovery path even if its new process
/// already reports the target version. Unknown versions take the full path.
fn needs_upgrade_inspection(sf: &SessionFile, targets: &Targets, prior: Option<&St>) -> bool {
    // A restart in flight, and a release owed to the agent whatever build it
    // runs now ([`release_visit`]). AND AN ANNOUNCED ROUND WHOSE NOTICE
    // CARRIED A MODEL ([`St::promised_model`]; review of 2026-10-01): the
    // ride is the round's own state, not what the rule says now, so the build
    // current and the rule keeping the model (a `/model` typed since the
    // notice, the model recorded failed, a tail with no answer in it) left it
    // unvisited — its READY never acted on, a barred ride never dropped
    // ([`ride_dropped`]), the agent the notice asked to wind down never
    // released, and the round announced for good. A gave-up round's late
    // READY acts only while its release is owed, which the line above lets
    // through already; past that it is visited at an attach, as every
    // give-up is.
    if prior
        .is_some_and(|st| st.in_flight() || !st.release.is_empty() || st.promised_model().is_some())
    {
        return true;
    }
    let Some(running) = Version::parse(&sf.version) else {
        return true;
    };
    targets
        .managed
        .iter()
        .chain(targets.native.iter())
        .any(|target| target.version > running)
}

fn claim_still_live(c: &mut Client, pid: u32, claim: &HostClaim) -> bool {
    process_in_tab(c, pid, &claim.tab, Some(claim.group), Some(claim.parent))
}

pub(super) fn session_file_of(home: &Path, pid: u32) -> Option<SessionFile> {
    let t = std::fs::read_to_string(home.join(format!(".claude/sessions/{pid}.json"))).ok()?;
    upgrade::parse_session_file(&t).ok()
}

/// Whether any transcript of `session` exists (`None`: the projects directory
/// cannot be read). Claude files a transcript only once the conversation has
/// a message, and one without cannot be resumed (`--resume` answers "No
/// conversation found with session ID").
pub(super) fn transcript_exists(home: &Path, session: &str) -> Option<bool> {
    let name = format!("{session}.jsonl");
    let dirs = std::fs::read_dir(home.join(".claude/projects")).ok()?;
    Some(dirs.flatten().any(|e| e.path().join(&name).is_file()))
}

/// WHETHER CONVERSATION `session` HAS A TASK ([`upgrade::TaskScan`]), read
/// once: `Some(false)` when nobody but the harness has asked it anything —
/// no transcript at all (no message yet) among readable projects, or one
/// holding only the harness's own turns — `Some(true)` once it holds a
/// prompt of someone else's, and `None` when nobody can say (the projects cannot
/// be read — none yet on a machine that never held a conversation — two
/// transcripts claim it, or it is longer than [`Tasked`] reads at once): the
/// caller then does what it did before it asked.
/// `ours`: what the session's supervisor typed, and when
/// ([`supervisor_typed`]) — a row of its, recorded as it was typed, is no one
/// else's prompt.
#[must_use]
pub fn tasked(home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
    Tasked::default().of(home, session, ours)
}

/// What the supervisor of tab `tab` typed into it, and when, from its loop's
/// ledger ([`crate::supervise::approvals::typed_texts`] under
/// [`Opts::aterm_state`]): none when that root is not known. Into EVERY
/// conversation the tab has held: [`upgrade::TaskScan`] places each text in
/// one by its time.
#[must_use]
pub fn supervisor_typed(opts: &Opts, tab: &str) -> Vec<approvals::Typed> {
    opts.aterm_state.as_deref().map_or_else(Vec::new, |root| {
        approvals::typed_texts(&approvals::ledger_under(root, Some(tab)), Some(tab))
    })
}

/// [`tasked`] kept up for one conversation at a time, read on from where the
/// last read ended — a transcript is only appended to — so a caller that asks
/// at every turn end reads each row once. A conversation that has a task has
/// it for good: nothing is read for it again.
#[derive(Clone, Debug, Default)]
pub struct Tasked {
    /// The conversation this is about.
    session: String,
    /// Its transcript, once found (the projects are scanned for it once).
    path: Option<PathBuf>,
    /// How far into its transcript the rows are folded in (a whole line's end).
    read_to: u64,
    scan: upgrade::TaskScan,
}

/// The most of a transcript one [`Tasked::of`] reads: a task shows at the
/// first answered prompt near the head, and a taskless conversation is a few
/// harness turns; a longer record with none shown is read on at the next ask.
const TASK_READ_BYTES: u64 = 8 << 20;

impl Tasked {
    /// [`tasked`], for `session` — read on from where the last read of the
    /// same conversation ended, from its start for another one or for a
    /// transcript that shrank (it was rewritten). `ours` is read afresh at
    /// every ask: the supervisor records a turn as it types it, before the
    /// row it wrote can be read here.
    pub fn of(&mut self, home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        if self.session != session {
            *self = Tasked {
                session: session.to_string(),
                ..Tasked::default()
            };
        }
        if self.scan.settled() {
            return Some(true);
        }
        self.scan.supervisor_typed(ours);
        if self.path.as_ref().is_none_or(|p| !p.is_file()) {
            self.path = transcript(home, session);
        }
        let Some(path) = self.path.as_ref() else {
            // No message yet among projects that can be read: no task.
            return transcript_exists(home, session).and_then(|any| (!any).then_some(false));
        };
        let mut f = std::fs::File::open(path).ok()?;
        let len = f.metadata().ok()?.len();
        if len < self.read_to {
            self.read_to = 0;
            self.scan = upgrade::TaskScan::default();
            self.scan.supervisor_typed(ours);
        }
        f.seek(SeekFrom::Start(self.read_to)).ok()?;
        let mut buf = Vec::new();
        f.take(TASK_READ_BYTES).read_to_end(&mut buf).ok()?;
        let at_end = self.read_to.saturating_add(buf.len() as u64) >= len;
        // Whole lines only: one caught half-written is read next time.
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(whole);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf).lines() {
            if self.scan.line(line) && self.scan.settled() {
                return Some(true);
            }
        }
        // A command of someone else's still waiting for its turn is a task
        // already; nothing of anyone's is none only once the end was read.
        if self.scan.tasked() {
            return Some(true);
        }
        at_end.then_some(false)
    }
}

/// How much of a transcript's end is read for the READY answer and the model
/// the agent ran.
pub(super) const TAIL_BYTES: u64 = 262_144;

/// The one transcript holding `session`.
pub(super) fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
    let mut found = None;
    for e in std::fs::read_dir(home.join(".claude/projects"))
        .ok()?
        .flatten()
    {
        let p = e.path().join(format!("{session}.jsonl"));
        if p.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(p);
        }
    }
    found
}

/// The reset conversation `session`'s standing usage limit names now
/// ([`upgrade::transcript_limit_reset`] over its transcript tail, past or
/// future; `None`: none named or no transcript read). A supervisor loop whose
/// limit episode read no reset of its own, or a clock time with no day, asks
/// its host for this (`supervise::IdleHost::limit_until`): a notice left on
/// screen [`LIMIT_GRACE_S`] past its reset counts as an idle point.
#[must_use]
pub fn named_limit_reset(home: &Path, session: &str) -> Option<u64> {
    let path = transcript(home, session)?;
    upgrade::transcript_limit_reset(&tail_to_end(&path, TAIL_BYTES).0)
}

/// THE QUOTA WALL conversation `session` stands at now
/// ([`upgrade::transcript_standing_wall`] over its transcript's tail, the
/// same bounded read as [`named_limit_reset`]; `None`: no quota rejection
/// stands last, or no transcript read) — what the window's host hands the
/// accounts recorder (`accounts::record_wall`) with the facts of the
/// session's process. It reads; it records nothing.
#[must_use]
pub fn named_standing_wall(home: &Path, session: &str) -> Option<upgrade::StandingWall> {
    let path = transcript(home, session)?;
    upgrade::transcript_standing_wall(&tail_to_end(&path, TAIL_BYTES).0)
}

/// THE LINE conversation `session`'s usage-limit dialog ended with since
/// `since` (unix seconds; [`upgrade::transcript_reset_line`] over its
/// transcript's tail, the same bounded read as [`named_standing_wall`];
/// `None`: no such row yet, or no transcript read) — what the window's host
/// reads a claimed limit reset's outcome from, `since` being the second the
/// attempt was claimed. It reads; it classifies and records nothing.
#[must_use]
pub fn named_reset_line(home: &Path, session: &str, since: u64) -> Option<String> {
    let path = transcript(home, session)?;
    upgrade::transcript_reset_line(&tail_to_end(&path, TAIL_BYTES).0, since)
}

/// WHETHER CONVERSATION `session` IS KEPT UNDER `home`: a transcript of that
/// name is among the projects of the Claude home `home` is the default one
/// of (`<home>/.claude/projects/*/<session>.jsonl`). What a host with NO
/// PROCESS TO READ decides "the agent's Claude home was the default one" on
/// (a reopened layout's agent, the accounts design §6): a Claude Code under
/// another home (`CLAUDE_CONFIG_DIR`) files its conversations there, and
/// `--resume <session>` of one finds nothing here. `false` for a name that
/// is no session id (it is joined into a path), for a conversation with no
/// message yet (no transcript is filed for one), and where the projects
/// cannot be listed. It lists directory names; no transcript is opened.
#[must_use]
pub fn conversation_under(home: &Path, session: &str) -> bool {
    upgrade::is_session_id(session) && transcript_exists(home, session) == Some(true)
}

/// THE RESET A CODEX'S WALL NAMES, placed as its upgrade places it (the
/// review of 2026-09-30; `codex::place_wall`): `text` is the wall's `try
/// again at` words, placed at `now` in the zone the Codex `pid` (its
/// `$CODEX_HOME` `home`) printed them in — its own `TZ`, else the machine's
/// zone, never this process's — a bare clock time as its print day's. What
/// the window's host places the supervise loop's reading of that wall by
/// (`supervise::IdleHost::place_reset`), so the loop's limit episode, its
/// continuation and the upgrade's visit read one reset. `None`: words the
/// grammar cannot read, or a zone that cannot be read — never guessed.
#[must_use]
pub fn codex_wall_reset(pid: u32, home: &Path, text: &str, now: u64) -> Option<u64> {
    codex::place_wall(&Live, pid, home, text, now)
}

/// The last `bytes` of `path`, decoded lossily: a cut inside a multi-byte
/// character would make a strict decode refuse the whole window, hiding a
/// READY on the last line. Lossy spoils only the already-cut first line (a
/// U+FFFD that [`upgrade::transcript_has_ready`] skips as not JSON) and a
/// last line caught half-written.
///
/// Returns the byte offset where the read ended, the mark a later read starts
/// from ([`since`]), taken from what was read rather than a separately asked
/// length, so an append between the two is never skipped.
pub(super) fn tail_to_end(path: &Path, bytes: u64) -> (String, u64) {
    let Ok(mut f) = std::fs::File::open(path) else {
        return (String::new(), 0);
    };
    let len = f.metadata().map_or(0, |m| m.len());
    tail_window(&mut f, len, bytes)
}

/// Bounds the read itself too, since a writer may append after `len` was
/// measured. The returned mark is the end actually read, so the next `since`
/// read picks up every byte left behind by the cap.
fn tail_window(f: &mut (impl Read + Seek), len: u64, bytes: u64) -> (String, u64) {
    let start = len.saturating_sub(bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (String::new(), 0);
    }
    let mut buf = Vec::new();
    let _ = f.take(bytes).read_to_end(&mut buf);
    let end = start.saturating_add(buf.len() as u64);
    (lossy(buf), end)
}

/// At most `cap` bytes of `path` from byte `from` on, decoded lossily like
/// [`tail_to_end`]. Empty when the file is shorter than `from`: a transcript
/// that shrank was rewritten, and nothing in it can be told to come after the
/// mark.
pub(super) fn since(path: &Path, from: u64, cap: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.metadata().map_or(0, |m| m.len()) < from || f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(cap).read_to_end(&mut buf);
    lossy(buf)
}

fn lossy(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

// ---------------------------------------------------------------- the sweep

/// One sweep: every live Claude Code session advanced by at most one step
/// (a restart counts as one: exit, relaunch and continue, each bounded), then
/// every restart a previous sweep left in flight carried on.
///
/// One actor at a time, machine-wide: a hand-run `aterm harness upgrade`, the
/// window's host ([`step`]) and a relaunch on exit
/// ([`super::relaunch::after_exit`]) share `<state>/upgrade/sweep.lock`; one
/// that finds it held reports `busy` and does nothing, or two would type the
/// notice twice.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
}

/// The window's step for one tab (`opts.only_sid` on the instance
/// `opts.sock`): its foreground Claude Code, or a restart left in flight,
/// advanced one step at an idle point its supervisor parked at (the host's
/// worker calls this when [`due`] says so). Every proof the sweep makes is
/// made here: the instance's PTY roster names the tab uniquely, and every act
/// re-reads it. `current` when there is nothing to do.
#[must_use]
pub fn step(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let tabs = connect(opts, &tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c));
    let current = || Report {
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "current".to_string(),
    };
    let Some(tabs) = tabs.filter(|tabs| tab_is_live(tabs, &tab)) else {
        return Report {
            step: "wait:tab-not-live".to_string(),
            ..current()
        };
    };
    let reports = sweep_with_roster(opts, Some(&tabs));
    pick(&reports, &tab).cloned().unwrap_or_else(current)
}

/// The one word [`step`] answers with: the tab's own act, else its wait, then
/// any act, then any wait. A Codex tab's pass also reports its daemon
/// (`tab=-`), whose act must never stand for the client's wait (the client
/// would be left unmoved until the next activation notice); a current client
/// leaves the daemon's word.
fn pick<'a>(reports: &'a [Report], tab: &str) -> Option<&'a Report> {
    let own = |r: &&Report| r.tab == tab;
    reports
        .iter()
        .filter(own)
        .find(|r| r.is_act())
        .or_else(|| reports.iter().filter(own).find(|r| r.step != "current"))
        .or_else(|| reports.iter().find(|r| r.is_act()))
        .or_else(|| reports.iter().find(|r| r.step != "current"))
}

/// Whether tab `opts.only_sid` has an upgrade to take: its foreground Codex
/// runs an older managed build (or its daemon is behind), its foreground
/// Claude Code runs an older build or a model the model rule moves
/// ([`super::upgrade_models`]), or a restart of this tab is in flight.
/// Read-only and cheap enough to ask at worker start, every activation notice
/// and every look: one roster read, session files when needed (never under a
/// Codex foreground), cached build versions, the model list. A failed read
/// decides nothing ([`Due::Unread`]), neither parking nor releasing on a
/// guess. A whole read with nothing to act on is [`Due::No`]: nothing to move
/// to, a Codex of another home or unnamed build, an agent past its launch with
/// no record of its own ([`RECORD_WINDOW_S`]).
#[must_use]
pub fn due(opts: &Opts) -> Due {
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    if !in_flight(opts).is_empty() || confirming(opts) {
        return Due::Yes;
    }
    let Some(tabs) = connect(opts, tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c))
    else {
        return Due::Unread(UNREAD_SOCKET);
    };
    // The Codex branch: a Codex leads the tab ([`codex::due`]) — or the tab
    // is the Claude Code half's to read.
    due_after_codex(opts, &tabs, codex::due(opts, &tabs))
}

/// The Codex lane owns a tab whose foreground leader is Codex, even when its
/// build is current or its process could not be read whole
/// ([`UNREAD_PROCESS`]); only an unknown or other foreground (`codex_due` =
/// `None`) can have a Claude session record that makes an upgrade due. A
/// separate seam so the read-heavy Claude roster is provably skipped on every
/// Codex answer.
fn due_after_codex(opts: &Opts, tabs: &[LiveTab], codex_due: Option<Due>) -> Due {
    if let Some(due) = codex_due {
        return due;
    }
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    let Some(files) = claude_records(&opts.home) else {
        return Due::Unread(UNREAD_FILES);
    };
    match due_among(
        opts,
        &files,
        tabs,
        || Targets::read(&opts.home),
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        Due::Unread(UNREAD_RECORD) => no_record(tabs, tab, started_s, alive, now_s()),
        due => due,
    }
}

/// Claude Code's session records read whole ([`session_files`]): none where it
/// keeps none (no `.claude/sessions`, e.g. another `CLAUDE_CONFIG_DIR`), and
/// `None` when one could not be read whole (half-written, or gone between the
/// listing and the read).
fn claude_records(home: &Path) -> Option<Vec<SessionFile>> {
    session_files(home).or_else(|| (!home.join(".claude/sessions").is_dir()).then(Vec::new))
}

/// [`due`]'s Claude Code half for tab `opts.only_sid`, over a complete read of
/// the session records (`files`) and the instance's tabs, with the candidate
/// builds read by `read_targets` (at most once) and the process reads
/// injected ([`tab_agent`]). `Yes` when the record whose agent leads the tab
/// is behind, `No` when one leads and is not, [`UNREAD_VERSION`] when a
/// candidate build's `--version` did not answer in time, [`UNREAD_PROCESS`]
/// when a behind leader's process could not be read whole, and
/// [`UNREAD_RECORD`] when no record is the tab's agent's (for [`no_record`]).
fn due_among(
    opts: &Opts,
    files: &[SessionFile],
    tabs: &[LiveTab],
    read_targets: impl Fn() -> Targets,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Due {
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    let targets = std::cell::OnceCell::new();
    let mctx = std::cell::OnceCell::new();
    // The tab's agent wanted nothing of what answered, and a build did not.
    let unanswered = std::cell::Cell::new(false);
    let wants = |sf: &SessionFile| {
        let tg = targets.get_or_init(&read_targets);
        let wanted = needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
            || model_wants_a_look(opts, sf, mctx.get_or_init(|| ModelCtx::read(opts, tg)));
        unanswered.set(!wanted && tg.unanswered);
        wanted
    };
    match tab_agent(tab, files, tabs, wants, read_args, read_group, read_ids) {
        TabAgent::Claimed(_) => Due::Yes,
        TabAgent::Nothing if unanswered.get() => Due::Unread(UNREAD_VERSION),
        TabAgent::Nothing => Due::No,
        TabAgent::Unread => Due::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => Due::Unread(UNREAD_RECORD),
    }
}

/// What a look read of the Claude Code that leads one tab ([`tab_agent`]).
#[derive(Debug, PartialEq, Eq)]
enum TabAgent<'a> {
    /// The record whose agent leads the tab, one the look asked about, its
    /// process read whole.
    Claimed(&'a SessionFile),
    /// A record whose agent leads the tab was read, and there is nothing to
    /// ask: the look wants none of it (a current build), or its environment
    /// names another tab.
    Nothing,
    /// A record the look asked about leads the tab, but its process could
    /// not be read whole ([`host_candidates`]'s wait).
    Unread,
    /// No record's agent leads the tab.
    NoRecord,
}

/// What a look at tab `tab` reads of its Claude Code among the session
/// records (`files`) and the instance's tabs ([`host_candidates`]): the
/// leading records asked about as `wants` says, with process reads injected.
/// A record counts as read only where it decides (claimed whole, wanted by
/// nothing, another tab's by its environment), never where its process could
/// not be read, so a failed `ps` never reads as "not due".
fn tab_agent<'a>(
    tab: &str,
    files: &'a [SessionFile],
    tabs: &[LiveTab],
    mut wants: impl FnMut(&SessionFile) -> bool,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> TabAgent<'a> {
    let mut leads = false;
    let seen = host_candidates(
        files,
        tabs,
        |sf, t| {
            let ours = t == tab;
            leads |= ours;
            ours && wants(sf)
        },
        read_args,
        read_group,
        read_ids,
    );
    if let Some(sf) = seen
        .iter()
        .find_map(|c| c.as_ref().ok().map(|(sf, _, _)| *sf))
    {
        TabAgent::Claimed(sf)
    } else if !seen.is_empty() {
        TabAgent::Unread
    } else if leads {
        TabAgent::Nothing
    } else {
        TabAgent::NoRecord
    }
}

/// How long a tab's agent may run with no record of its own before a look
/// takes that as final ([`no_record`]). Claude Code writes
/// `sessions/<pid>.json` shortly after starting, so an agent running this long
/// without one keeps none a look can find (another `CLAUDE_CONFIG_DIR`, a
/// non-interactive session), and the step, which acts only on a record, has
/// nothing to take.
pub const RECORD_WINDOW_S: u64 = 30;

/// A look that found no record of the tab's agent: a wait ([`UNREAD_RECORD`])
/// while the tab's foreground job (its group leader, started at the unix
/// second `started` reads) is younger than [`RECORD_WINDOW_S`] at `now`, and
/// nothing to take once older. A leader whose start cannot be read is a
/// process not read ([`UNREAD_PROCESS`]) while it lives (`alive`), and nothing
/// to take once gone; likewise a tab the instance no longer has.
fn no_record(
    tabs: &[LiveTab],
    tab: &str,
    started: impl FnOnce(u32) -> Option<u64>,
    alive: impl FnOnce(u32) -> bool,
    now: u64,
) -> Due {
    let Some(live) = tabs.iter().find(|t| t.sid == tab) else {
        return Due::No;
    };
    let Some(leader) = live.fgpgid.and_then(|group| u32::try_from(group).ok()) else {
        return Due::Unread(UNREAD_PROCESS);
    };
    match started(leader) {
        Some(at) if now.saturating_sub(at) >= RECORD_WINDOW_S => Due::No,
        Some(_) => Due::Unread(UNREAD_RECORD),
        None if alive(leader) => Due::Unread(UNREAD_PROCESS),
        None => Due::No,
    }
}

/// The unix second `pid` started, as the kernel says ([`kernel_start`]).
fn started_s(pid: u32) -> Option<u64> {
    models::parse_lstart(&kernel_start(pid)?)
}

/// What [`due`] read of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// An upgrade to take: the tab's agent is behind, a restart of it is in
    /// flight, or its carry-on's model is still to be read.
    Yes,
    /// Read whole, and nothing to take.
    No,
    /// A read failed, so nothing is decided: the socket did not answer
    /// ([`UNREAD_SOCKET`]), Claude Code's records could not be read whole
    /// ([`UNREAD_FILES`]), the agent's process could not be
    /// ([`UNREAD_PROCESS`]), a launch record is not written yet
    /// ([`UNREAD_RECORD`], for at most [`RECORD_WINDOW_S`]), or a candidate
    /// build's `--version` did not answer in time ([`UNREAD_VERSION`], until
    /// the version cache asks again). The word is the look's wait
    /// (`wait:<word>`), and the host looks again soon rather than treating the
    /// session as not due.
    Unread(&'static str),
}

/// [`Due::Unread`]'s word for a look the instance's socket did not answer.
pub const UNREAD_SOCKET: &str = "no-socket";
/// [`Due::Unread`]'s word for a look that found no record of the tab's
/// agent among Claude Code's session records while it may still be written
/// ([`RECORD_WINDOW_S`]).
pub const UNREAD_RECORD: &str = "no-record";
/// [`Due::Unread`]'s word for a look that could not read the tab agent's
/// process whole (ids, argv or start; a `ps` that could not spawn, or the
/// foreground moving between reads), for Claude Code and Codex alike.
pub const UNREAD_PROCESS: &str = "no-process";
/// [`Due::Unread`]'s word for a look that could not read Claude Code's
/// session records whole: the sweep's own wait for it.
pub const UNREAD_FILES: &str = "session-files-unreadable";
/// [`Due::Unread`]'s word for a look whose tab agent is behind nothing that
/// answered while a build it could move to did not answer `--version` within
/// [`VERSION_WAIT`] (a first-run check, a loaded machine): unknown until the
/// version cache asks again ([`VERSION_RETRY_FIRST`], doubling). As "not due"
/// it would wait for the next activation notice.
pub const UNREAD_VERSION: &str = "no-version";

/// The session is behind from the moment its host sees it: at the worker's
/// attach and at every activation notice, an agent that is tab
/// `opts.only_sid`'s foreground job on an older build than one it may move to
/// gets its upgrade state minted as the first step would (`Pending`, target,
/// tab, `pending_since` = `since`), so the owner's view counts time behind
/// from then rather than from the first step after a busy turn. A Claude Code
/// state is minted where none stands, or where the standing one finished with
/// another build ([`behind_anew`]); a Codex TUI's likewise
/// (`codex::note_behind`). A state in flight or already on this build is
/// left alone. Nothing is typed, read off the screen or signalled.
/// [`Behind::Busy`] when another actor holds the lock and [`Behind::Unread`]
/// when a read failed ([`due`]'s words; at a fresh launch the record may not
/// be written yet): both asked again. An agent past its launch with no record
/// ([`RECORD_WINDOW_S`]) has nothing to note.
pub fn note_behind(opts: &Opts, since: u64) -> Behind {
    let Some(tab) = opts.only_sid.as_deref().filter(|_| !opts.dry_run) else {
        return Behind::Nothing;
    };
    let Ok(_held) = sweep_lock(opts) else {
        return Behind::Busy;
    };
    let Some(tabs) = connect(opts, tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c))
    else {
        return Behind::Unread(UNREAD_SOCKET);
    };
    if let Some(noted) = codex::note_behind(opts, &tabs, since) {
        return noted;
    }
    let Some(files) = claude_records(&opts.home) else {
        return Behind::Unread(UNREAD_FILES);
    };
    let sf = match tab_agent(
        tab,
        &files,
        &tabs,
        |_| true,
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        TabAgent::Claimed(sf) => sf,
        TabAgent::Nothing => return Behind::Nothing,
        TabAgent::Unread => return Behind::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => {
            return match no_record(&tabs, tab, started_s, alive, now_s()) {
                Due::Unread(word) => Behind::Unread(word),
                Due::Yes | Due::No => Behind::Nothing,
            };
        }
    };
    let native = exe_of(sf.pid).is_some_and(|exe| exe.starts_with(native_root(&opts.home)));
    if mint_behind(opts, sf, tab, &Targets::read(&opts.home), native, since) {
        Behind::Noted
    } else {
        Behind::Nothing
    }
}

/// What [`note_behind`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    /// The session's state was minted behind, its age from the `since`
    /// asked with.
    Noted,
    /// Nothing to note: no agent behind in the tab, or its state already
    /// says so (or is in flight).
    Nothing,
    /// Another actor holds the state's lock (a step under way): asked again
    /// once it lets go.
    Busy,
    /// A read failed ([`Due::Unread`]'s words): nothing noted, and nothing
    /// decided — asked again.
    Unread(&'static str),
}

/// Whether the state standing for a session leaves it to be noted behind anew
/// on build `to` ([`note_behind`]): finished (done, its carry-on's model no
/// longer owed, or failed) with another build. A state in flight, pending or
/// announced is the step's, as is one finished with `to` itself (done, or a
/// stopped round the step re-arms after [`upgrade::RETRY_S`]).
pub(super) fn behind_anew(st: &St, to: &Version) -> bool {
    matches!(st.phase, Phase::Done | Phase::Failed(_))
        && !st.confirming()
        && Version::parse(&st.to).as_ref() != Some(to)
}

/// [`note_behind`]'s record for `sf` in `tab` among `targets` (`native`: runs
/// the native install): minted where a build it may move to is newer and no
/// state stands except one [`behind_anew`], as [`St::for_target`] mints it,
/// behind since `since` (never later than now).
fn mint_behind(
    opts: &Opts,
    sf: &SessionFile,
    tab: &str,
    targets: &Targets,
    native: bool,
    since: u64,
) -> bool {
    let Some(running) = Version::parse(&sf.version) else {
        return false;
    };
    let Some(target) = upgrade::choose_target(&running, &targets.for_session(native)) else {
        return false;
    };
    let prior = load(opts, &sf.session_id);
    if let Some(st) = prior
        .as_ref()
        .filter(|st| !behind_anew(st, &target.version))
    {
        return moved_tabs(st, tab) && retab(opts, sf, tab, prior, &running, &target);
    }
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let mut st = St::for_target(prior, &running, &target, None, who, now_s());
    st.pending_since = st.pending_since.min(since);
    st.tab = tab.to_string();
    save(opts, &sf.session_id, &st);
    true
}

/// Whether record `st` of a conversation just proven held in `tab` waits in
/// another tab while nothing but the tab ties it there: pending, or a stopped
/// round resting. Never a notice out (the tab is its fence,
/// [`St::notice_belongs_to`]; a visit reopens a notice whose process is gone,
/// [`St::notice_gone`]), nor a restart under way or finished.
fn moved_tabs(st: &St, tab: &str) -> bool {
    st.tab != tab && matches!(st.phase, Phase::Pending | Phase::Failed(_))
}

/// A conversation resumed in another tab: its record `prior` still names its
/// first tab, and the watch, the window's view and `--status` find records by
/// tab, so until the new tab's first point nothing showed, watched or marked
/// it. As the host attaches and the conversation is proven held in `tab`, the
/// record is made what that first visit would make it: onto the build it
/// would choose ([`St::for_target`], ledgered once as
/// `retargeted:<from>-><to>` where the build changes) and into this tab,
/// keeping its round and time behind. Nothing is typed. Returns `true`
/// (noted).
fn retab(
    opts: &Opts,
    sf: &SessionFile,
    tab: &str,
    prior: Option<St>,
    running: &Version,
    target: &Candidate,
) -> bool {
    let Some(old) = prior.as_ref().map(|p| p.to.clone()) else {
        return false;
    };
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let (mut st, how) = St::retarget(prior, running, target, None, who, now_s());
    if matches!(st.phase, Phase::Pending | Phase::Failed(_)) {
        tab.clone_into(&mut st.tab);
    }
    save(opts, &sf.session_id, &st);
    if Version::parse(&old).as_ref() != Some(&target.version) {
        let r = Report {
            pid: sf.pid,
            tab: tab.to_string(),
            session: sf.session_id.clone(),
            from: running.to_string(),
            to: format!("{}({})", target.version, target.source.as_str()),
            step: format!("retargeted:{old}->{}", target.version),
        };
        ledger(
            opts,
            &r,
            &format!(
                "upgrade target changed: {old} -> {}, found in another tab; {}",
                target.version,
                retarget_how(how, &st)
            ),
        );
    }
    true
}

/// What the window's host does after one [`step`] on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// Park again at the session's next idle point: the step acted and the
    /// next waits on the agent's answer at a turn end.
    NextIdle,
    /// Look again after this pause: the step waited (busy, held, a person,
    /// unsettled, no READY yet, another actor on the lock) or held the session
    /// back. Each consecutive wait pauses longer ([`LATER`]).
    Later(std::time::Duration),
    /// Nothing more for this build: done, current, a round's own stop just
    /// said (`failed:…`, `refused:…`; the next look finds it resting), or
    /// skipped by the owner. A resting round (`wait:failed`) is not finished:
    /// it is looked at again ([`upgrade::RETRY_S`]).
    Finished,
    /// The step cannot run at all — its state directory cannot hold the lock
    /// — and says so once; nothing more until the next activation notice.
    Broken,
}

/// The pauses before consecutive looks at a session whose step waited: the
/// first long enough for a session that just went idle to settle
/// ([`upgrade::QUIET_S`]), the last a ten-minute look for as long as it
/// waits. At run time its first rung is the settle this process runs
/// ([`first_look`]).
pub const LATER: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(upgrade::QUIET_S),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(600),
];

/// [`LATER`]'s first rung as this process runs it: the settle
/// ([`upgrade::quiet_s`], [`upgrade::QUIET_S`] in every shipped binary), so
/// the look after a settle's wait comes when the settle can have stood.
#[must_use]
pub fn first_look() -> std::time::Duration {
    std::time::Duration::from_secs(upgrade::quiet_s())
}

/// [`LATER`] with its first rung [`first_look`]: the ladder [`after`] climbs.
fn later_rungs() -> [std::time::Duration; 4] {
    let mut rungs = LATER;
    rungs[0] = first_look();
    rungs
}

/// The longest gap between two looks reading the same process's screen idle
/// for them to count in a row ([`St::idle_looks`]): the ladder's longest pause
/// ([`LATER`]) plus a settle's slack. Past it the run begins again: an idle
/// screen from hours before says nothing of Claude's status now.
pub const IDLE_RUN_GAP_S: u64 = LATER[LATER.len() - 1].as_secs() + upgrade::QUIET_S;

/// How soon a session whose step waited on a person's hold
/// ([`upgrade::person_hold`]) is looked at again, however long it has waited:
/// often enough to measure the hold's dwell ([`upgrade::hold_since`]), so a
/// READY it holds past [`upgrade::DRAIN_S`] is voided, not acted on when the
/// person lets go hours later.
pub const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// A look that runs a little late never begins a person's hold again: the gap
/// the drain's dwell tolerates is at least two looks.
const _: () = assert!(upgrade::HOLD_GAP_S >= 2 * HOLD_LOOK.as_secs());

/// [`After`] from one [`step`]'s word, `waits` the session's consecutive waits
/// before it. An attended wait's word alone reads as while it owns the point
/// (the first rung); the window's host, which counts its looks, asks
/// [`after_attended`] instead.
///
/// An untaught word is looked at again, never taken as the last word: every
/// last word is named ([`is_last_word`]), and anything else is
/// [`unknown_step`], looked at again at the ladder's longest pause (so it does
/// not poll), with a one-time warning from the window's host. Otherwise a new
/// step word would silently stop the host looking at the session.
#[must_use]
pub fn after(step: &str, waits: u32) -> After {
    known_after(step, waits).unwrap_or(After::Later(LATER[LATER.len() - 1]))
}

/// Whether [`after`] was never taught `step`: it reads [`After::Later`] as a
/// guess, and the window's host says so once (`harness_host`'s
/// `UpgradeRun::after`), so the word is added here rather than read as done.
#[must_use]
pub fn unknown_step(step: &str) -> bool {
    known_after(step, 0).is_none()
}

/// The step deadline of a record: the unix second by which the step that
/// moves `st` on should have run, from the record alone, or `None` for a
/// record owed nothing until a newer build or the owner's next word. Pure (no
/// clock, screen or process), so a timer can keep it where no point comes.
///
/// One exhaustive match over [`Phase`] and [`Request`] with no wildcard arm,
/// so a new phase or word does not compile without a budget, and wait words
/// ([`St::wait`]) never decide one.
///
/// | phase | deadline |
/// |---|---|
/// | `pending` | [`St::progress_at`] + [`upgrade::REASK_S`]; refused notices ([`St::refused`]) are no progress, so they never push it back |
/// | `announced` | `at_s` + `REASK_S` + [`WATCH_SLACK`]: the re-ask's clock, held by a limit ([`St::hold_clock`]) |
/// | `failed` | `failed_at` + [`upgrade::RETRY_S`] + `WATCH_SLACK`: the re-arm (`failed_at` already carries a repeated stop's longer rest, [`St::fail`]) |
/// | `exiting`, `relaunched` | `at_s` + [`STALE_S`] + 1: when the owner's view reads it stuck ([`St::stuck`]) |
/// | `done` | `confirm_by` + `WATCH_SLACK` while the model confirm is owed ([`St::confirming`]); else none |
///
/// The owner's word: a `--skip` of this target holds a round not yet
/// restarting (pending, announced, or stopped awaiting re-arm,
/// [`upgrade::rearm_held`]) for good; a `--defer` holds the same until its
/// second, the deadline being the later of that plus `WATCH_SLACK` and the
/// phase's own; a restart in flight is held by neither ([`Request::holds`]).
/// The word is the one on the record's own tab ([`St::request_for`]).
#[deny(clippy::wildcard_enum_match_arm)]
pub(super) fn due_by(st: &St) -> Option<u64> {
    let step = match &st.phase {
        Phase::Pending => Some(st.progress_at.saturating_add(upgrade::REASK_S)),
        Phase::Announced { at_s, .. } => Some(
            at_s.saturating_add(upgrade::REASK_S)
                .saturating_add(WATCH_SLACK),
        ),
        Phase::Failed(_) => Some(
            st.failed_at
                .saturating_add(upgrade::RETRY_S)
                .saturating_add(WATCH_SLACK),
        ),
        Phase::Exiting { at_s } | Phase::Relaunched { at_s } => {
            Some(at_s.saturating_add(STALE_S).saturating_add(1))
        }
        Phase::Done => st
            .confirming()
            .then(|| st.confirm_by.saturating_add(WATCH_SLACK)),
    };
    // Whether the word holds a round that has not begun restarting, by the
    // phase: pending, announced, and a stopped round's re-arm.
    let unbegun = match &st.phase {
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_) => true,
        Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    match st.request_for(&st.tab) {
        Request::None | Request::Now => step,
        Request::Skip(v) => {
            let this_target = Version::parse(&v).is_some_and(|v| Some(v) == Version::parse(&st.to));
            if this_target && unbegun { None } else { step }
        }
        Request::DeferUntil(t) => {
            if unbegun {
                step.map(|s| s.max(t.saturating_add(WATCH_SLACK)))
            } else {
                step
            }
        }
    }
}

/// When the host watches a record: the earlier of the step deadline
/// ([`due_by`]) and the move clock (behind its target for [`MOVE_BUDGET_S`],
/// [`St::behind_since`], kept across retargets and rounds, the one bound an
/// endlessly re-arming round cannot reset), but never sooner than
/// [`WATCH_GAP`] after the last watch ([`St::looked_at`]). `None` for a record
/// [`due_by`] owes nothing (finished, or held by `--skip`).
pub(super) fn watch_at(st: &St) -> Option<u64> {
    let step = due_by(st)?;
    let moved = st.behind_since().saturating_add(MOVE_BUDGET_S);
    Some(step.min(moved).max(st.looked_at.saturating_add(WATCH_GAP)))
}

/// The last words: nothing more for this build until the next activation
/// notice or owner's word ([`After::Finished`]). `current`, `done` and
/// `done:<how>`, `ended:<how>` (the relaunch lane: the agent ended by itself,
/// nothing resumed), `skip:<why>` (not this step's session), `daemon-updated`
/// (the Codex daemon's pass; its clients step on their own), a dry run's
/// `would-<act>`, the owner's `wait:skipped`, and the restart's acts that end
/// the process this worker watches ([`moved`]): `terminated`, `relaunched`,
/// and Codex's `exit-typed`/`exited`. The process that follows is a new
/// session with its own worker.
fn is_last_word(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "current"
            | "done"
            | "ended"
            | "skip"
            | "daemon-updated"
            | "terminated"
            | "relaunched"
            | "exit-typed"
            | "exited"
    ) || step.starts_with("would-")
        || step == "wait:skipped"
}

/// [`after`] for a word it was taught; `None` for one it was not.
fn known_after(step: &str, waits: u32) -> Option<After> {
    let later = || ladder(waits);
    Some(match step.split_once(':').map_or(step, |(head, _)| head) {
        // `continued`: the resumed session's answer (the model it came back
        // on) is read at its turn's end (Claude Code; a Codex carry-on is
        // done as typed). `adopted`: the relaunched agent is its loop's, and
        // the continuation is typed at the loop's next idle point.
        "announced" | "continued" | "adopted" => After::NextIdle,
        // `gave-up`, `drain-expired`: the upgrade stopped asking or voided a
        // READY and owes the agent its release, and a given-up upgrade still
        // honours a late READY, which comes at a turn's end: the next idle
        // point's, never the last word.
        "gave-up" | "drain-expired" => After::NextIdle,
        // The goal pause (`upgrade_codex::goal_step`): goal paused or its
        // head's Esc pressed; the move comes at the next idle point, once the
        // paused goal's last turn has ended.
        "goal-paused" | "goal-esc" => After::NextIdle,
        _ if step == "wait:goal-held" => After::NextIdle,
        // The goal resumed, released, or its pause given up: the upgrade goes
        // on as a look finds it.
        "goal-resumed" | "goal-released" | "goal-pause-refused" | "goal-paused-seen" => later(),
        // `released`: the line is typed; a voided upgrade asks again later, and
        // one that gave up is looked at again, resting until its next round.
        // `rearmed`: a new round, whose first notice the next look types.
        "released" | "rearmed" => later(),
        // The owner's `--skip` holds it for this build: nothing more until a
        // newer one is installed, or the owner's next word (both wake the
        // host). A `--defer` runs out on its own, so it is looked at again.
        // So are the other last words ([`is_last_word`]).
        _ if is_last_word(step) => After::Finished,
        // A stopped round with nothing owed: `wait:failed` is said only when no
        // release is owed (that waits as `wait:release:<why>`,
        // [`release_is_next`]) and no READY can move it. It is the upgrade's
        // quiet word, which the derived model's `Look` is bound to
        // (`harness_upgrade_never_strands_model`). Not a last word: the round
        // rests `upgrade::RETRY_S` and re-arms, so it is looked at again. (The
        // Codex lane says `wait:failed:<why>` while resting, and
        // `wait:skipped`/`wait:deferred` when held by the owner's word.)
        _ if step == "wait:failed" => later(),
        // A round's own stop, said as it stops (`failed:<why>`,
        // `refused:<what>`, and Codex's `exit-refused`, its `/exit` refused
        // and its phase put back): the next look finds it resting, as above.
        "failed" | "refused" | "exit-refused" => later(),
        "busy" if matches!(step, "busy:state-unwritable" | "busy:lock-unopenable") => After::Broken,
        // A person's hold: its dwell is sampled ([`HOLD_LOOK`]).
        _ if matches!(step, "wait:box" | "wait:draft") => After::Later(HOLD_LOOK),
        // A person at the tab: looked at again at the first rung while the
        // point is the upgrade's ([`after_attended`], with the host's look
        // count), so the look finding the grace lapsed lands close enough to
        // take the act due at this point ([`owns_turn_ends`]) before the
        // loop's own continuation does.
        _ if attended(step) => After::Later(first_look()),
        // `left-typed-cleared` (an older record's word from when the Codex
        // branch cleared its own text) and `left-typed:<what>` (the text
        // recorded and standing) are looked at again like any wait.
        "wait" | "busy" | "held-back" | "left-typed-cleared" | "left-typed" => later(),
        // `reopened:notice-process-gone`: the notice's process is gone and the
        // round is pending for the conversation's holder. The visit ledgers
        // it and goes on, so no step returns it today; taught anyway as a
        // pending round's word.
        "reopened" => later(),
        _ => return None,
    })
}

/// The re-look ladder ([`LATER`]) at its `waits`-th rung (the last once past it).
fn ladder(waits: u32) -> After {
    After::Later(
        crate::supervise::ladder::Ladder(&later_rungs())
            .step(usize::try_from(waits).unwrap_or(usize::MAX)),
    )
}

/// Whether one [`step`]'s word is a person at the tab holding the act due at
/// this point: `held-back:attended` (once per round), then `wait:attended`.
#[must_use]
pub fn attended(step: &str) -> bool {
    matches!(step, "held-back:attended" | "wait:attended")
}

/// [`After`] an attended wait ([`attended`]): `looks` its attended looks in a
/// row at this point, this one included ([`attended_owned`]), `same` its waits
/// on the same word before it. At the first rung while the next look still
/// owns the point, so the look that finds the grace lapsed takes the act.
/// Past the backstop the point is the loop's again, and it returns to the
/// ladder rather than a costly look (status read, process scan, state save,
/// journal row) every twenty seconds.
#[must_use]
pub fn after_attended(looks: u32, same: u32, grace_s: u32) -> After {
    if attended_owned(looks, grace_s) {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Whether one [`step`]'s word says it typed a turn into the agent
/// ([`crate::supervise::HostStep::typed`]): a notice (`announced:<n>`), a
/// carry-on (`continued`, both lanes; Claude Code's record then awaits the
/// model its answer names, while a Codex's is done as typed), or the release
/// line (`released:<why>`). Its answer is the harness's own turn; a short one
/// is no short turn of the worker's.
#[must_use]
pub fn typed(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "announced" | "continued" | "released"
    )
}

/// Whether one [`step`]'s word says it moved the session
/// ([`crate::supervise::HostStep::moved`]): it typed a turn ([`typed`]) or
/// ended the agent (`terminated`, `relaunched`, `adopted`, `done:fresh`;
/// Codex's typed `/exit`, `exit-typed`, `exited`, and `done:unconfirmed` for a
/// new TUI whose composer never came up). Every other word (waits, `current`,
/// skips, last words such as `done`, `done:model-unconfirmed`, `gave-up`,
/// `failed:…`, `done:taskless`, `done:no-continue`, a cleared Codex text)
/// typed and ended nothing: its point still stands.
#[must_use]
pub fn moved(step: &str) -> bool {
    typed(step)
        || matches!(
            step.split_once(':').map_or(step, |(head, _)| head),
            "adopted" | "relaunched" | "terminated" | "exit-typed" | "exited"
        )
        || matches!(step, "done:fresh" | "done:unconfirmed")
        // The goal's own: its head's Esc stopped the turn, and a resumed goal
        // starts its next one — the point is gone either way.
        || matches!(
            step.split_once(':').map_or(step, |(head, _)| head),
            "goal-esc" | "goal-resumed"
        )
}

/// Holds the upgrade's clocks through a limit episode the window's loop saw:
/// tab `opts.only_sid`'s records have their re-ask and READY clocks start no
/// sooner than `until`, the last unix second the loop knew the session
/// limited ([`St::hold_clock`]). Called by the host as the episode opens and
/// closes (`IdleHost::limited`), since the upgrade's own look holds the clock
/// only when it finds the limit ([`upgrade::clock_held`]) and the host takes
/// no step during an episode. Files only, under the sweep lock; `false` while
/// another sweep holds it (the host retries before its next step), `true`
/// once applied or with nothing to apply.
#[must_use]
pub fn hold_clock(opts: &Opts, until: u64) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return true;
    };
    let _held = match sweep_lock(opts) {
        Ok(held) => held,
        Err("another-sweep") => return false,
        Err(_) => return true,
    };
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return true;
    };
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        if let Some(mut st) = load(opts, &session).filter(|st| st.tab == tab)
            && st.hold_clock(until)
        {
            save(opts, &session, &st);
        }
    }
    true
}

/// How many consecutive looks the upgrade may wait for background work under
/// a READY agent before it stops owning the session's turn ends
/// ([`owns_turn_ends`]); past it the worker is continued. This bounds only
/// ownership; the wait itself is bounded by the reducer's re-ask
/// ([`upgrade::REASK_S`]) and give-up ([`upgrade::MAX_ASKS`],
/// [`upgrade::next_step`]).
pub const OWNED_BACKGROUND_LOOKS: u32 = 3;

/// How many quick looks in a row (the upgrade's own gate settling: the
/// screen quiet [`upgrade::QUIET_S`]) the upgrade may own the session's turn
/// ends for.
pub const OWNED_SETTLE_LOOKS: u32 = 2;

/// Whether the upgrade owns the session's turn ends after the step that said
/// `step` (`looks`: for an attended word, [`attended`], the attended looks in
/// a row at this point; for others the waits in a row). The window's host
/// answers its loop with this (`IdleHost::owns_turn_end`) so the turn-end
/// policy types nothing over a wind-down: an announcement out, and while READY
/// is given and the restart's gate only waits (its settle for
/// [`OWNED_SETTLE_LOOKS`] looks, a background drain for
/// [`OWNED_BACKGROUND_LOOKS`]). An answer without READY (`awaiting-ready`), a
/// person's hold, an owed release (`wait:release:<why>`), and every last word
/// own nothing. It lives beside [`after`] because a step's word is this
/// module's to interpret.
///
/// A person at the tab (`held-back:attended`, then `wait:attended`) owns them
/// for as long as the person's grace (`grace_s`, `[harness] human_grace_s`,
/// renewed by each touch) runs, bounded only as a backstop
/// ([`attended_owned`]) on the attended looks at this point, never on all
/// waits in a row. That wait arises only where the upgrade's act (notice, or
/// restart after READY) is due at this very point and only the person holds
/// it; the loop also holds off for the same grace, so ownership decides who
/// takes the point when the grace lapses, and not the loop's `keep going`
/// over a READY.
#[must_use]
pub fn owns_turn_ends(step: &str, looks: u32, grace_s: u32) -> bool {
    if attended(step) {
        return attended_owned(looks, grace_s);
    }
    match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" => true,
        // A goal paused for the move (`upgrade_codex::goal_step`): its turn
        // ends are the move's.
        "goal-paused" | "goal-esc" => true,
        "wait" if step.starts_with("wait:goal-") => matches!(
            step,
            "wait:goal-held" | "wait:goal-pausing" | "wait:goal-resume" | "wait:goal-resuming"
        ),
        "wait" => match step {
            // `status-stale`: Claude's status catching up with its idle screen
            // ([`upgrade::gate_announce`]), a settle like `not-idle`.
            // `ready-unrecorded`: READY on screen, its transcript row still to
            // be written; the point is the restart's, and a continuation would
            // void the answer. `superseded`: the re-ask owed to a superseded
            // READY, due here once the turn settles. Both bounded as the settle
            // is.
            "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded" => looks < OWNED_SETTLE_LOOKS,
            // A Codex that answered READY waits out its background terminal
            // as Claude Code's restart waits out a shell under it.
            "wait:background" | "wait:background-terminal" => looks < OWNED_BACKGROUND_LOOKS,
            // The notice was due at this very point and its own fence refused
            // it — a repaint, a keystroke's echo ([`refused`]).
            _ if refused(step) => looks < OWNED_REFUSED_LOOKS,
            _ => false,
        },
        _ => false,
    }
}

/// [`owns_turn_ends`] under the flow the step was taken in (round eight,
/// #25): under the QUIET flow (`quiet`, [`upgrade::Facts::quiet`], the
/// default) a wait on the agent's own work — `wait:background`,
/// `wait:background-terminal` — OWNS NOTHING. That ownership exists for an
/// agent that answered READY and stopped while its background work drains:
/// the loop's `keep going` must not be typed over the answer. The quiet flow
/// asks for no READY and acts on none: its restart simply waits for the
/// work to end ([`upgrade::quiet_step`]), so every pending round with a shell
/// under its agent read `wait:background` at an idle point and held the
/// loop's continuation for [`OWNED_BACKGROUND_LOOKS`] looks (about six
/// minutes, [`LATER`]) for a restart that could not come before the work
/// ended. Every other word owns as [`owns_turn_ends`] says, in both flows:
/// the settle the quiet restart is due at, a person's hold, a notice.
#[must_use]
pub fn owns_turn_ends_as(step: &str, looks: u32, grace_s: u32, quiet: bool) -> bool {
    !(quiet && drains(step)) && owns_turn_ends(step, looks, grace_s)
}

/// [`bounded_wait`] under the flow the step was taken in: a wait that owns
/// nothing bounds nothing, so under the QUIET flow the waits on the agent's
/// own work ([`owns_turn_ends_as`]) spend none of the settle's looks — the
/// settle the quiet restart is due at once that work ends still owns its
/// point (round eight, #25).
#[must_use]
pub fn bounded_wait_as(step: &str, quiet: bool) -> bool {
    !(quiet && drains(step)) && bounded_wait(step)
}

/// The DRAIN'S words: a wait on the agent's own work under it
/// (`wait:background`, and a Codex's `wait:background-terminal`).
fn drains(step: &str) -> bool {
    matches!(step, "wait:background" | "wait:background-terminal")
}

/// Whether a WAIT counts toward the bound its word is owned for
/// ([`owns_turn_ends`]): the settle's (`settling`, `not-idle`, `busy`,
/// `status-stale`, `ready-unrecorded`, `superseded`) and the drain's
/// (`background`, `background-terminal`), the waits of a gate whose act is
/// due at the point. Others count nothing: a person at the tab (counted at its
/// point, [`attended_owned`]), `awaiting-ready`, a resting round (`failed`), a
/// box, a draft, a limit; counting them would spend the settle's looks before
/// the READY came. An agent's turn never restarts the count, only the
/// upgrade's acts. A notice its own fence refused ([`refused`]) has its own
/// count, which the owner's word also restarts.
#[must_use]
pub fn bounded_wait(step: &str) -> bool {
    matches!(
        step,
        "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded"
            | "wait:background"
            | "wait:background-terminal"
    )
}

/// Whether one [`step`]'s word is a notice its own fence refused
/// (`wait:announce-refused:<why>`): the gate said go and the host's `turn`
/// typed nothing, because the screen moved between the fence's read and the
/// paste (`changed`), a person typed through the park (`yield`), or it was
/// refused outright. Retried within the visit first ([`NOTICE_FENCE_TRIES`]);
/// then the point is owned for [`OWNED_REFUSED_LOOKS`] first-rung looks
/// ([`after_refused`]) and the refusals are counted for the owner
/// (`St::refused`).
#[must_use]
pub fn refused(step: &str) -> bool {
    step.starts_with("wait:announce-refused:")
}

/// How many refused looks since the upgrade's last act or owner's word a
/// refused notice ([`refused`]) may own the session's turn ends for
/// ([`owns_turn_ends`]): three, [`LATER`]'s first rung apart
/// ([`after_refused`]), each typing the notice up to [`NOTICE_FENCE_TRIES`]
/// times. Counted apart from the settle's waits ([`bounded_wait`]). An act or
/// an owner's word renews it; an agent's turn does not. Without it the loop's
/// `keep going` would take the idle point the notice needed.
pub const OWNED_REFUSED_LOOKS: u32 = 3;

/// [`After`] a refused notice ([`refused`]): `looks` its waits since the last
/// act (as [`owns_turn_ends`] read), `same` its waits on the same word.
/// Looked at again at the first rung while the point is still the upgrade's,
/// so the notice is retried within it; back on the ladder once it is the
/// loop's.
#[must_use]
pub fn after_refused(looks: u32, same: u32) -> After {
    if looks < OWNED_REFUSED_LOOKS {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// How many times one visit types a notice its fence refused as `changed` (a
/// repaint, a keystroke's echo) before waiting
/// (`wait:announce-refused:changed`). Each try reads the composer afresh and is
/// fenced on that read, so a draft or box stops it at once, and a person
/// typing through the park (`yield`) is never typed at again. A later try
/// types only where that read still meets the look's gates ([`retry_open`]:
/// no person's hand since the look, no running turn, the reader's idle point).
pub const NOTICE_FENCE_TRIES: usize = 3;

/// How far apart those tries are ([`NOTICE_FENCE_TRIES`]).
const NOTICE_FENCE_EVERY: Duration = Duration::from_millis(400);

/// Whether an attended wait ([`owns_turn_ends`]) still owns the session's turn
/// ends after `looks` attended looks in a row at this point: while they span
/// ([`LATER`]'s first rung apart, [`after_attended`]) no more than the
/// person's grace (`grace_s`) plus [`ATTENDED_OWNED_S`]. Each look re-reads the
/// person's stamp, so a person still present keeps the point theirs and the
/// look after their grace lapses takes the act. The bound is a backstop, not
/// the grace (a person reading for longer than one grace renews it with every
/// touch); it exists for a stamp that fails closed ([`upgrade::attended_by`])
/// and would read attended for ever.
///
/// The looks are counted at one point by the window's host: any other step or
/// a turn in between (the person's, or the agent's READY) starts them over,
/// so the READY's point still goes to the upgrade.
#[must_use]
pub fn attended_owned(looks: u32, grace_s: u32) -> bool {
    u64::from(looks).saturating_mul(first_look().as_secs())
        <= u64::from(grace_s).saturating_add(ATTENDED_OWNED_S)
}

/// How long past the person's grace an attended wait may keep owning the
/// session's turn ends ([`attended_owned`]), holding the agent's act for a
/// person reading, scrolling or clicking.
pub const ATTENDED_OWNED_S: u64 = 30 * 60;

/// Whether a step taken at a break of the agent's background work
/// (`IdleHost::at_background`) gives back the turn ends a notice claimed
/// there. A session whose screen stays busy never reaches the idle point that
/// would recompute the claim ([`owns_turn_ends`]), so these give it back at
/// once: the owner's `--skip`/`--defer`, a person at the tab, aterm's hold,
/// the limit, a full queue of untaken notices, a give-up, a void, a release
/// typed or owed, and every stop. A wait on the agent's own work keeps the
/// claim, since the agent was asked to finish up.
#[must_use]
pub fn released_at_break(step: &str) -> bool {
    matches!(
        step,
        "gave-up"
            | "wait:skipped"
            | "wait:deferred"
            | "wait:attended"
            | "wait:held"
            | "wait:limited"
            | "wait:queued"
            | "wait:failed"
            | "wait:done"
    ) || [
        "failed",
        "refused",
        "held-back",
        "drain-expired",
        "released",
        "wait:release",
    ]
    .iter()
    .any(|head| step.starts_with(head))
}

/// A window supplies its own live PTY roster; an unscoped CLI keeps its
/// report-only wait when macOS omits another process's environment.
fn sweep_with_roster(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => {
            return vec![Report {
                pid: 0,
                tab: "-".to_string(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("busy:{why}"),
            }];
        }
    };
    // Files only, plus whatever else this sweep finds first; never at a break
    // of the agent's background work, where only a line that ends nothing is
    // typed (a notice, or an owed release).
    let mut reports = if opts.background {
        Vec::new()
    } else {
        confirmations(opts)
    };
    // The Codex branch, under the same lock: every Codex home's daemon, every
    // Codex TUI in a tab, then Codex restarts in flight ([`codex::sweep`]);
    // for the window's step, the tab's TUI and its daemon. No Claude session
    // file needed.
    let codex = codex::sweep(opts, live_tabs);
    // A tab whose foreground job is Codex belongs to the Codex branch alone:
    // the Claude pass's machine-wide waits (no `~/.claude` on a Codex-only
    // machine) are not that tab's.
    let codex_tab = opts
        .only_sid
        .as_ref()
        .is_some_and(|sid| live_tabs.is_some() && codex.iter().any(|r| r.tab == *sid));
    if !codex_tab {
        reports.extend(claude_pass(opts, live_tabs));
    }
    reports.extend(codex);
    reports
}

/// A Codex restart in flight carried on from where it stopped
/// ([`codex::carry_in_flight`]): the relaunch primitive's continuation step
/// ([`super::relaunch::resume`]) for a Codex record.
pub(super) fn codex_carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    codex::carry_in_flight(opts, r, st, session)
}

/// A Codex TUI's [`super::relaunch::Snapshot`], read while it runs
/// ([`codex::snapshot`]).
pub(super) fn codex_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    codex::snapshot(opts, tab, pid, start, shell, args, &Live)
}

/// One look at a Codex that left its tab ([`codex::look_at_exit`]).
pub(super) fn codex_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    codex::look_at_exit(opts, snap, &Live)
}

/// A Codex relaunch on exit ([`codex::after_exit`]).
pub(super) fn codex_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    codex::after_exit(opts, snap, left, upgrade, cause, &Live)
}

/// The Claude lane's share of one sweep: every live Claude Code session
/// advanced one step, then its restarts left in flight carried on.
fn claude_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
    let Some(files) = session_files(&opts.home) else {
        return vec![Report {
            pid: 0,
            tab: "-".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: format!("wait:{UNREAD_FILES}"),
        }];
    };
    // Bookkeeping under the lock, never at a break of the agent's background
    // work, where only a line that ends nothing is typed.
    if !opts.background {
        prune_states(opts, &files, now_s());
    }
    let mut reports = Vec::new();
    reports.extend(if let Some(tabs) = live_tabs {
        // Read installed versions only after a cheap PTY-group match. A window
        // with no Claude job does not execute a managed twin just to ask its
        // version; a matched but current job stops before `ps` and argv reads.
        let targets = std::cell::OnceCell::new();
        let mctx = std::cell::OnceCell::new();
        let candidates = host_candidates(
            &files,
            tabs,
            |sf, tab| {
                let tg = targets.get_or_init(|| Targets::read(&opts.home));
                opts.only_sid.as_deref().is_none_or(|only| only == tab)
                    && (needs_upgrade_inspection(sf, tg, load(opts, &sf.session_id).as_ref())
                        || model_wants_a_look(
                            opts,
                            sf,
                            mctx.get_or_init(|| ModelCtx::read(opts, tg)),
                        ))
            },
            atpkg::caller_shell::process_args,
            process_group,
            ids,
        );
        // A process not read whole is the step's wait, said: the next look
        // reads it again.
        let (mut claimed, mut waits) = (Vec::new(), Vec::new());
        for candidate in candidates {
            match candidate {
                Ok(c) => claimed.push(c),
                Err(wait) => waits.push(wait),
            }
        }
        if !claimed.is_empty() {
            let t = table();
            let targets = targets.get_or_init(|| Targets::read(&opts.home));
            let mctx = mctx.get_or_init(|| ModelCtx::read(opts, targets));
            waits.extend(claimed.into_iter().map(|(sf, args, claim)| {
                visit_models(
                    opts,
                    sf,
                    Some(&files),
                    &t,
                    targets,
                    &Live,
                    Some(&args),
                    Some(&claim),
                    mctx,
                )
            }));
        }
        waits
    } else if files.is_empty() {
        Vec::new()
    } else {
        let t = table();
        let targets = Targets::read(&opts.home);
        let mctx = ModelCtx::read(opts, &targets);
        files
            .iter()
            .map(|sf| {
                visit_models(
                    opts,
                    sf,
                    Some(&files),
                    &t,
                    &targets,
                    &Live,
                    None,
                    None,
                    &mctx,
                )
            })
            .collect()
    });
    if !opts.background {
        reports.extend(orphans(opts, &files, live_tabs));
    }
    reports
}

/// Takes `<state>/upgrade/sweep.lock` without waiting. `Ok(None)` for a dry
/// run, which acts on nothing.
///
/// One try decides, because the guard is released by `LOCK_UN`
/// ([`SweepLock`]), like atpkg's store lock (`atpkg::lock::Flock`). An
/// `flock(2)` lives on the open file description, and a child mid-spawn on any
/// thread holds a copy of every descriptor until its exec, so a lock released
/// only by close would still read held for that long. `LOCK_UN` strips the
/// lock from the description itself, every inherited copy included, so a lock
/// that reads held is really a sweep's.
pub(super) fn sweep_lock(opts: &Opts) -> Result<Option<SweepLock>, &'static str> {
    if opts.dry_run {
        return Ok(None);
    }
    let dir = state_dir(opts);
    std::fs::create_dir_all(&dir).map_err(|_| "state-unwritable")?;
    let lock: std::fs::File = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sweep.lock"))
        .map_err(|_| "lock-unopenable")?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the take lands in a SweepLock, whose Drop releases by LOCK_UN"
    )]
    let taken = lock.try_lock();
    match taken {
        Ok(()) => Ok(Some(SweepLock(lock))),
        Err(_) => Err("another-sweep"),
    }
}

/// A held [`sweep_lock`], released by `LOCK_UN` on drop rather than by close,
/// so the next sweep finds it free at once whatever another thread is
/// spawning (`a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives`).
pub(super) struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a restart of the tab `opts.only_sid` still owes the model its
/// resumed session came back on ([`St::confirming`]).
fn confirming(opts: &Opts) -> bool {
    std::fs::read_dir(state_dir(opts)).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| St::from_json(&text))
                    .is_some_and(|st| {
                        st.confirming() && opts.only_sid.as_ref().is_none_or(|s| *s == st.tab)
                    })
        })
    })
}

/// The conversations an earlier sweep left mid-restart (agent signalled, new
/// process not yet relaunched or carried on) that a sweep with `opts` would
/// resume ([`orphans`]' filter, without its liveness checks). A sweep that
/// stands down strands them, and says which.
#[must_use]
pub fn in_flight(opts: &Opts) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                return None;
            }
            let session = path.file_stem()?.to_string_lossy().into_owned();
            let st = load(opts, &session).filter(St::in_flight)?;
            let ours = opts.only_sid.as_ref().is_none_or(|s| *s == st.tab);
            ours.then_some(session)
        })
        .collect();
    out.sort();
    out
}

fn blank(sf: &SessionFile) -> Report {
    Report {
        pid: sf.pid,
        tab: "-".to_string(),
        session: sf.session_id.clone(),
        from: sf.version.clone(),
        to: "-".to_string(),
        step: String::new(),
    }
}

pub(super) fn first_word(s: &str) -> String {
    s.split_whitespace().take(2).collect::<Vec<_>>().join("-")
}

pub(super) fn said(mut r: Report, step: impl Into<String>) -> Report {
    r.step = step.into();
    r
}

#[cfg(all(test, unix))]
fn visit(
    opts: &Opts,
    sf: &SessionFile,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
) -> Report {
    let files = session_files(&opts.home);
    visit_with_claim(opts, sf, files.as_deref(), t, targets, k, None, None)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn visit_with_claim(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
) -> Report {
    visit_models(
        opts,
        sf,
        initial_files,
        t,
        targets,
        k,
        prefetched_args,
        host_claim,
        &ModelCtx::none(),
    )
}

/// THE MODEL A NOTICE CARRIED NO LONGER RIDES ITS ROUND (`why`,
/// [`models::ride_bar`]; 2026-10-01), dropped from the round's record `st`
/// before anything reads it: the visit's retarget ([`St::for_target`]), a
/// gave-up round's late READY (`restart` asks [`St::model_list`]) and the
/// watch's retarget ([`St::promised_model`]) all read the record, so all
/// three take the one verdict. Saved at once; SAID ONCE in the ledger by
/// [`DroppedRide::say`], when the visit knows how the round goes on. An
/// ANNOUNCED round with nothing else to restart it (`idle`: the build current,
/// and nothing the rule moves now) had only the model to move, and is
/// released ([`St::release_unridden`]). A dry run changes the record in memory
/// alone, so it says what a visit would do.
fn ride_dropped(
    opts: &Opts,
    session: &str,
    st: &mut St,
    why: &'static str,
    idle: bool,
    now: u64,
) -> DroppedRide {
    let model = std::mem::take(&mut st.model_list);
    let released = idle && st.release_unridden(now);
    if !opts.dry_run {
        save(opts, session, st);
    }
    DroppedRide {
        why,
        model,
        released,
    }
}

/// A RIDE THE VISIT DROPPED ([`ride_dropped`]): the model, the rule's word
/// that barred it, and whether its round was released.
struct DroppedRide {
    why: &'static str,
    model: String,
    released: bool,
}

impl DroppedRide {
    /// THE DROP'S ONE LEDGER ROW (`model-ride-dropped`), with how the round
    /// goes on — `round`, as the visit's retarget left it by `how`, in the
    /// words a retarget's own row says it in ([`retarget_how`]). Said only
    /// once that is known (review of 2026-10-01): written at the drop, it
    /// said "the round is kept" of an announced round the same visit then
    /// abandoned, the model the rule decided now another one — a new consent
    /// ([`St::retarget`]'s abandoning arm), its release owed and its READY
    /// lost, with no row at the same version naming it.
    fn say(&self, opts: &Opts, r: &Report, round: &St, how: Retarget) {
        if opts.dry_run {
            return;
        }
        let row = Report {
            to: format!("model:{}", self.model),
            ..said(r.clone(), "model-ride-dropped")
        };
        let then = if self.released {
            "with the build current and nothing else due, nothing is left to restart for: the \
             agent the notice asked to wind down is released, and the upgrade is pending again"
                .to_string()
        } else {
            format!("the round goes on without it, {}", retarget_how(how, round))
        };
        ledger(
            opts,
            &row,
            &format!(
                "{}: the model the notice carried is not one the model rule may move this \
                 conversation to now, so it no longer rides the restart; {then}",
                self.why
            ),
        );
    }
}

/// Whether the process an announcement's notice reached ([`St::notice_pid`],
/// [`St::notice_start`]) is proven gone, and how: `exited` (its pid names no
/// process) or `recycled` (a process started under that pid at another time).
/// A notice without a fence proves nothing, and an unreadable start (`ps`
/// failed, or the process went between reads) is no verdict: each is waited
/// on like a live notified process, which may still answer.
fn notice_process_gone(st: &St) -> Option<&'static str> {
    if st.notice_pid == 0 || st.notice_start.is_empty() {
        return None;
    }
    if !alive(st.notice_pid) {
        return Some("exited");
    }
    kernel_start(st.notice_pid)
        .is_some_and(|start| start != st.notice_start)
        .then_some("recycled")
}

/// One session advanced one step, with the pass's model half ([`ModelCtx`]):
/// a session whose model the rule moves (newest of its own family first, then
/// up the list) restarts when the model ladder ([`models::model_moves_now`])
/// says the move is due, onto a newer build if there is one or else the same
/// build, with the model on the relaunch line (`--model`, session-only; never
/// Claude's `/model`, which also saves the person's default).
#[allow(clippy::too_many_arguments)]
fn visit_models(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[(u32, u32, String)],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
    mctx: &ModelCtx,
) -> Report {
    let mut r = blank(sf);
    // The session file names this process, not a recycled pid.
    if !alive(sf.pid) || kernel_start(sf.pid).as_deref() != Some(squash(&sf.proc_start).as_str()) {
        return said(r, "wait:stale-file");
    }
    if sf.kind != "interactive" || sf.entrypoint != "cli" || !upgrade::is_session_id(&sf.session_id)
    {
        return said(r, "wait:not-an-interactive-cli");
    }
    let fetched_args;
    let args = if let Some(args) = prefetched_args {
        args
    } else {
        let Some(args) = atpkg::caller_shell::process_args(sf.pid) else {
            return said(r, "wait:argv-unreadable");
        };
        fetched_args = args;
        &fetched_args
    };
    let Some(tab) = host_claim
        .map(|claim| claim.tab.clone())
        .or_else(|| args.env_var("ATERM_PARENT_SESSION_ID").map(str::to_owned))
    else {
        // macOS may yield argv without env for another process. The unscoped
        // CLI has no PTY roster claim and must wait rather than guess a tab.
        return said(r, "wait:no-aterm-tab");
    };
    if args
        .env_var("ATERM_PARENT_SESSION_ID")
        .is_some_and(|env_tab| env_tab != tab)
    {
        return said(r, "wait:tab-identity-conflict");
    }
    r.tab.clone_from(&tab);
    if opts.only_sid.as_ref().is_some_and(|s| *s != tab) {
        return said(r, "skip:not-selected");
    }
    if let Err(why) = preliminary_unique_owner(initial_files, sf) {
        return said(r, format!("wait:{why}"));
    }
    let mut prior = load(opts, &sf.session_id);
    // An announcement belongs to its notice's process: nothing here acts on it
    // from another process or tab, and while that process lives this one
    // waits. Once it is proven gone the upgrade is reopened, pending in a new
    // round ([`St::notice_gone`], ledgered once), and this visit goes on as
    // the pending upgrade's. The row promises no ask: the holder may not be
    // behind, a job the tab reaches, or past its gates. It goes under every
    // gate a first notice does, with a READY marker of its own, and a
    // taskless conversation is restarted afresh instead ([`Step::Fresh`]).
    if let Some(st) = prior
        .as_mut()
        .filter(|st| matches!(st.phase, Phase::Announced { .. }) && !st.notice_belongs_to(sf, &tab))
    {
        let Some(how) = notice_process_gone(st) else {
            return said(r, "wait:notice-owned-by-other-process");
        };
        let reopen = Report {
            to: format!("{}({})", st.to, st.source),
            ..r.clone()
        };
        if opts.dry_run {
            return said(reopen, "would-reopen:notice-process-gone");
        }
        if let Err(why) = require_unique_owner(&opts.home, sf) {
            return said(r, format!("wait:{why}"));
        }
        let (gone_pid, now) = (st.notice_pid, now_s());
        let dropped = st.notice_gone(&tab, now);
        let reopen = said(reopen, "reopened:notice-process-gone");
        st.note_step(&reopen.step, "", now);
        save(opts, &sf.session_id, st);
        ledger(
            opts,
            &reopen,
            &format!(
                "the process the notice reached is gone (pid {gone_pid} {how}), and this one, \
                 never asked, holds the conversation: the upgrade is pending again in a new \
                 round, and asks it only as a pending upgrade asks — while the conversation is \
                 still behind — under every gate a first notice is{}",
                if dropped.is_empty() {
                    String::new()
                } else {
                    format!("; the release owed to the gone process (`{dropped}`) is dropped")
                }
            ),
        );
    }
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        if sf.pid == st.pid {
            // Still exiting: never a harder signal. A shutdown that hangs
            // past the bound is said once on the ledger, and waited on.
            let r = said(r, "wait:exiting");
            note_stuck(opts, &r, &mut st, &sf.session_id, now_s());
            return r;
        }
        // A new process holds the conversation. It is the relaunch only if the
        // shell the line was typed at started it; resumed by hand elsewhere,
        // it is not this upgrade's to tell. The failure is permanent, so it is
        // recorded only from a parent actually read; an unreadable one is a
        // wait.
        match relaunch_of(k, sf, &sf.session_id, st.pid, st.shell) {
            Relaunch::Ours => {}
            Relaunch::Unread => return said(r, "wait:ids"),
            Relaunch::NotOurs if opts.dry_run => return said(r, "would-fail:resumed-elsewhere"),
            Relaunch::NotOurs => {
                st.stop("resumed-elsewhere", now_s());
                let r = said(r, "failed:resumed-elsewhere");
                ledger(
                    opts,
                    &r,
                    "a process this upgrade did not relaunch holds the conversation",
                );
                save(opts, &sf.session_id, &st);
                return r;
            }
        }
        // The relaunch landed.
        if opts.dry_run {
            return said(r, "would-continue");
        }
        let Ok(mut c) = connect(opts, &tab) else {
            return said(r, "wait:no-socket");
        };
        if !roster(&mut c).contains(&tab) {
            return said(r, "wait:tab-not-live");
        }
        if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
            return said(r, "wait:tab-ownership-changed");
        }
        return carry_on(opts, r, &mut st, &mut c, &sf.session_id.clone(), sf, None);
    }
    let Some(running) = Version::parse(&sf.version) else {
        return said(r, "wait:version-unreadable");
    };
    let exe = exe_of(sf.pid).unwrap_or_default();
    let running_native = exe.starts_with(native_root(&opts.home));
    // The model half: what the conversation runs now against the list's best
    // available model. A change prefers a cold cache (a switch re-reads the
    // whole history) but never waits on one without bound
    // (`models::model_moves_now`).
    let launch = upgrade::launch_model(&args.argv);
    // THE RIDE (2026-10-01): the model a notice of this round carried, which
    // a restart of it would ask for ([`St::carried_model`]), is re-judged by
    // this very read ([`models::ride_bar`]).
    let carried = prior.as_ref().and_then(St::carried_model);
    let mread = model_read_for(opts, sf, launch.as_deref(), mctx, running_native, carried);
    // A model a person chose by hand since the launch, which a restart the
    // list moves to no model carries in place of the launch's `--model`:
    // read only where a line is planned, not on every visit.
    let hand_model = || {
        hand_model_in(
            opts,
            &sf.session_id,
            &args.argv,
            models::parse_lstart(&sf.proc_start),
            mctx,
            running_native,
        )
    };
    // Whether a newer build is due regardless of the model — decided before the
    // model, because a restart already happening is the moment the model rides.
    let build_target = upgrade::choose_target(&running, &targets.for_session(running_native));
    // Once announced, the model rides the upgrade's own state: the READY
    // answer warms the cache again, and a decision re-taken then would drop
    // the model half-way. Only while the rule's laws still hold for it: a
    // ride this read BARRED is dropped from the round below ([`ride_dropped`]).
    let announced_model = prior
        .as_ref()
        .and_then(St::promised_model)
        .filter(|_| mread.ride_barred.is_none());
    let model_to = models::model_to(
        announced_model,
        &mread.verdict,
        mread.cold,
        build_target.is_some(),
        mread.due_for_s,
    );
    // A newer build, or with the build current and the model behind the same
    // build again, relaunched with `--model`: a command-line model is
    // session-only, while Claude's `/model <id>` also rewrites the person's
    // default for new sessions.
    let target = build_target.or_else(|| {
        model_to
            .as_ref()
            .and_then(|_| same_build(targets, &exe, &running, running_native))
    });
    let dropped = match (mread.ride_barred, prior.as_mut()) {
        (Some(why), Some(st)) => Some(ride_dropped(
            opts,
            &sf.session_id,
            st,
            why,
            target.is_none(),
            now_s(),
        )),
        _ => None,
    };
    // The model it runs, which the announcement's reason is read against
    // ([`models::move_why`]).
    let running_model = match &mread.verdict {
        ModelVerdict::Due { from, .. } => Some(from.as_str()),
        ModelVerdict::Keep(_) => None,
    };
    let Some(target) = target else {
        // A dropped ride's round, as the drop left it: released, or a gave-up
        // round kept as it stopped (no retarget follows).
        if let (Some(dropped), Some(st)) = (&dropped, prior.as_ref()) {
            dropped.say(opts, &r, st, Retarget::Kept);
        }
        // Nothing to move, and still a release owed: the conversation runs
        // the build its upgrade stopped short of (resumed by hand on it after
        // a relaunch that failed), and the agent in it was asked to wind down.
        if let Some(st) = prior.clone().filter(|st| !st.release.is_empty()) {
            return release_visit(opts, r, sf, st, &tab, t, k, host_claim);
        }
        return match &mread.verdict {
            ModelVerdict::Due { .. } => said(r, "wait:model-cache-warm"),
            ModelVerdict::Keep(_) => {
                if let Some(gone) =
                    forget_settled_pending(opts, &sf.session_id, prior.as_ref(), &running)
                {
                    let reached = Report {
                        to: gone.to.clone(),
                        ..said(r.clone(), "reached:by-hand")
                    };
                    ledger(
                        opts,
                        &reached,
                        &format!(
                            "the session runs {running}, the build this upgrade was moving it to \
                             ({} at its worst): it got there by another route, so the record is \
                             closed and nothing ages toward the next build",
                            gone.to
                        ),
                    );
                }
                said(r, "current")
            }
        };
    };
    r.to = format!("{}({})", target.version, target.source.as_str());
    // The restart before this one has not said its model yet: a new upgrade
    // would start from a fresh state and lose that outcome's `done` row.
    if prior.as_ref().is_some_and(St::confirming) {
        return said(r, "wait:confirming");
    }
    let now = now_s();
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: models::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    // A record for another build than the one this look chose (an older
    // one, or one no longer offered), not yet begun restarting: the visit
    // RETARGETS it ([`St::for_target`]).
    let retargeted_from = prior
        .as_ref()
        .filter(|p| {
            matches!(
                p.phase,
                Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
            ) && Version::parse(&p.to).as_ref() != Some(&target.version)
        })
        .map(|p| p.to.clone());
    let (mut st, how) = St::retarget(prior, &running, &target, model_to.as_deref(), who, now);
    // A dropped ride is said with how this retarget left its round.
    if let Some(dropped) = &dropped {
        dropped.say(opts, &r, &st, how);
    }
    // The tab a PENDING upgrade waits in: what the owner names (`aterm
    // harness upgrade <sid> --now`) and what the window shows it under. From
    // the announcement on, `tab` is the notice's fence and is set there.
    if st.phase == Phase::Pending {
        st.tab.clone_from(&tab);
    }
    // The visit's own retarget, ledgered once like the watch's
    // (`retargeted:<from>-><to>`, worded by [`retarget_how`]). Saved here,
    // before any look: a look that cannot read the tab returns without
    // saving, and the retarget would otherwise be said again at every such
    // visit. A dry run writes and says nothing.
    if let Some(old) = retargeted_from {
        save(opts, &sf.session_id, &st);
        let word = format!("retargeted:{old}->{}", target.version);
        ledger(
            opts,
            &Report {
                step: word,
                ..r.clone()
            },
            &format!(
                "upgrade target changed: {old} -> {}; {}",
                target.version,
                retarget_how(how, &st)
            ),
        );
    }
    if matches!(st.request, Request::DeferUntil(t) if now >= t) {
        st.request = Request::None;
        st.request_tab.clear();
        st.request_at = 0;
    }
    // The process proofs, before any socket is dialled and while the upgrade
    // can still act. An environment tab id rides into tmux panes unchanged,
    // and the host's PTY-group match is only a first filter; the job-control
    // and terminal-owner checks prove typing reaches this agent.
    if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
        let job = k.job(sf.pid);
        if matches!(job, Some((Job::NoJobControl, _))) {
            let r = not_a_job(opts, r, &mut st);
            save(opts, &sf.session_id, &st);
            return r;
        }
        // Suspended or put in the background, the shell holds the terminal,
        // and nothing the screen shows is the agent's.
        if let Err(why) = foreground_shell(job) {
            st.note_wait(why, now);
            save(opts, &sf.session_id, &st);
            return said(r, format!("wait:{why}"));
        }
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        if !owner.is_some_and(owned_by_aterm) {
            let why = format!("terminal:{}", owner_word(owner));
            st.note_wait(&why, now);
            let r = held_back(opts, r, &mut st, &sf.session_id, &why);
            save(opts, &sf.session_id, &st);
            return r;
        }
    }
    let (mut c, mut facts, shown_mode, looked_hand, seen) =
        match look(opts, sf, &mut st, &tab, host_claim, t, now) {
            Ok(look) => look,
            Err(why) => return said(r, format!("wait:{why}")),
        };
    // Aterm's own words left in the prompt, first ([`left_first`]).
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, &tab, sf, now) {
        return r;
    }
    // The transcript tail, read every visit: for the login wall
    // ([`login_facts`]); once announced or with a release owed, for READY; at
    // a restart, for the model the agent ran and where the resumed session's
    // turns will start.
    let recent = transcript(&opts.home, &sf.session_id).map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    let rearmed = login_facts(opts, &sf.session_id, &mut st, &mut facts, tail, now);
    // A usage limit the transcript says stands, and a notice queued behind
    // one: what the screen alone cannot say ([`queue_facts`]).
    let asked = st.request_for(&tab);
    let requeued = queue_facts(&mut st, &mut facts, tail, now, &asked);
    let ready = heard(&st, sf, &tab, tail);
    // A READY the screen shows before the transcript records it, and one the
    // agent was moved on from since.
    ready_facts(&st, sf, &tab, tail, ready, &mut facts);
    // How long that READY has stood unacted on, held at the limit: what
    // bounds the agent's own background work under it.
    st.time_ready(ready, &mut facts, now);
    // How long a stopped round has rested: past RETRY_S a new one starts.
    st.time_failed(&mut facts, now);
    // Whether the agent's own work, seen since the latest notice, has ended.
    st.time_work(&mut facts, now);
    let request = st.request_for(&tab);
    // A give-up spent on notices that never reached the model is asked about
    // as the upgrade would be had they spent no ask ([`upgrade::rearmed`]),
    // unless a READY came first. The refund is in that phase, so the notice
    // it leads to is the next ask.
    let mut asked_about = match rearmed {
        Some(phase) if !ready => {
            facts.undelivered = false;
            phase
        }
        _ => st.phase.clone(),
    };
    let taken_back = asked_about != st.phase;
    let mut step = upgrade::requested_step(&request, &asked_about, &facts, ready, now, &st.to);
    // At a break of the agent's own background work nothing is ended,
    // whatever the plan says.
    if opts.background {
        step = upgrade::break_step(step);
    }
    // The end of the agent's own work re-arms a given-up round and asks in the
    // same visit ([`upgrade::Facts::work_ended`]), so the loop never gets the
    // point in between. The re-arm is ledgered as usual, and the new round's
    // first notice goes at this point under every first-notice gate (which
    // the reducer's open restart gate already vouched for). As with
    // `St::notice_gone`, a round carried on in the visit that found it.
    let mut rearmed_here = None;
    if step == Step::Rearm
        && facts.work_ended
        && matches!(&asked_about, Phase::Failed(why) if why == upgrade::GAVE_UP)
        && !opts.dry_run
    {
        rearmed_here = Some(rearm(opts, r.clone(), &mut st, now));
        let request = st.request_for(&tab);
        step = upgrade::requested_step(&request, &st.phase, &facts, false, now, &st.to);
        asked_about = st.phase.clone();
    }
    // The last read before anything is typed or signalled: the agent holds its
    // terminal. If suspended or backgrounded since the first look, the shell
    // holds it while the agent's last frame may still read as an idle empty
    // composer. The shell proven here is the one the plan is made against.
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate | Step::Fresh) {
        match foreground_shell(k.job(sf.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    // Where a dry read that would act stands ([`upgrade::held_point`]): the
    // give-up and re-arm are taken whatever the session shows, so a read off
    // any point (the host's watch) says what holds the point they wait for,
    // after the act's word ([`at_point`]), as a wait word
    // ([`upgrade::wait_word`]). A real visit at a point takes the step.
    let held_point = opts
        .dry_run
        .then(|| upgrade::held_point(&request, &step, &facts, now, &st.to))
        .flatten()
        .map(|w| upgrade::wait_word(w, &facts.status));
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        Step::Wait(why) => said(r, format!("wait:{why}")),
        // No stop is for good: the round that stopped has rested, and a new
        // one starts — its first notice at the next look, under every gate.
        Step::Rearm => rearm(opts, r, &mut st, now),
        Step::GiveUp => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            st.give_up(now);
            let r = said(r, "gave-up");
            ledger(
                opts,
                &r,
                &gave_up_words(
                    upgrade::Agent::Claude,
                    &k.describe(sf.pid, &background_procs(sf.pid, t)),
                    facts.quiet,
                ),
            );
            r
        }
        Step::Void(why) => drain_expired(opts, r, &mut st, why, &facts, now),
        Step::Announce => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            // The look's pill: no read of its own, the upgrade's exchanges
            // with the host are fenced and counted.
            let mode = resume_mode(opts, &sf.session_id, shown_mode.clone());
            let model = model_to.clone().or_else(hand_model);
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), |shell| {
                plan(
                    opts,
                    &launch_of(
                        sf,
                        &args.argv,
                        &target,
                        model.as_deref(),
                        mode.as_deref(),
                        true,
                        // The login this very process runs under, off the
                        // exec environment the visit already read.
                        args.env_var(super::accounts::LOGIN_ENV),
                    ),
                    shell,
                    t,
                )
            });
            // The line the announcement was planned on: what the restart after
            // READY is to type (it plans its own again from its own look).
            let planned_line = planned.as_ref().map(|p| p.line.clone()).ok();
            let salt = st.salt;
            let asks = upgrade::announce_asks(&asked_about, facts.undelivered);
            let again = if taken_back {
                Some(
                    "again: the upgrade had given up on notices that never reached the model, \
                     each answered by the login wall",
                )
            } else if requeued {
                Some(
                    "again: the notice before it never reached the model, its turn answered by \
                     a usage limit that is over",
                )
            } else if facts.undelivered {
                Some(
                    "again: the notice before it never reached the model, its turn answered by \
                     the login wall",
                )
            } else {
                None
            };
            // What a later try must still find on its fenced read
            // ([`retry_open`]): the look's gates, as the owner's word left them.
            let retry = Retry {
                looked: looked_hand,
                owner_now: st.request_for(&tab) == Request::Now,
                at_break: facts.background_point,
                // The person's grace at the look's rung: a person merely near
                // the tab holds a retry no longer than they held the look.
                grace_s: upgrade::person_grace(upgrade::rung_in_force(&facts), opts.human_grace_s),
            };
            // A notice the host stopped part-way, or typed whole and did not
            // send: its text and why ([`left_cut`]).
            let mut cut: Option<(String, String)> = None;
            let result = announce(opts, r, &mut st, planned, now, asks, again, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
                // What runs under the agent, named, so it can tell live work
                // from a wait that can never end ([`upgrade::STOPPING_POINT`]).
                let text = upgrade::with_running(
                    &upgrade::prepare_prompt_with_model(
                        &running,
                        &target.version,
                        target.source,
                        &marker,
                        model_to.as_deref(),
                        running_model,
                    ),
                    &upgrade::running_clause(&k.describe(sf.pid, &background_procs(sf.pid, t))),
                );
                require_unique_owner(&opts.home, sf).map_err(str::to_string)?;
                if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
                    return Err("tab-ownership-changed".to_string());
                }
                // The last look before typing: a FRESH read whose composer is
                // still empty and with no box up, and its generation, which
                // the typed turn is fenced on (`turn if-gen=`): a screen that
                // moved since that read typed nothing, and is read and fenced
                // again, up to NOTICE_FENCE_TRIES times — a repaint is over in
                // a moment (2026-09-28, s-d3346: three refusals at three
                // looks, and the loop took the point). A draft, a box, a
                // person typing through the park or a refusal is never tried
                // again here — and a try after the first types only where the
                // read it is fenced on still shows what the look's gates
                // required ([`retry_open`]): no person's hand since the look,
                // no turn running, the reader's idle point. The screen that
                // moved was the program's own output or a person's keys, and
                // either may have started the turn a notice must not be
                // queued into (the review of 2026-09-28). All of it under
                // aterm's own hand ([`type_fenced`]), fenced on Claude Code's
                // input box where the host takes `turn if-rows=`
                // ([`claude_fence`]), the words pressed key by key.
                match type_fenced(&mut c, &tab, &text, Some(&retry), Put::Pressed) {
                    Ok(()) => Ok(marker),
                    Err(Typed::Cut(why)) => {
                        let word = Typed::Cut(why.clone()).word();
                        cut = Some((text, why));
                        Err(word)
                    }
                    Err(e) => Err(e.word()),
                }
            });
            if let Some((text, _)) = cut {
                left_cut(&mut st, &mut c, &tab, &text);
            }
            if result.step.starts_with("announced:") {
                // The ask the end of the agent's work led to is spent from
                // the day's budget ([`St::work_end_open`]).
                if facts.work_ended {
                    st.spend_work_end(now);
                }
                st.model_list = model_to.clone().unwrap_or_default();
                st.line = planned_line.unwrap_or_default();
                st.login = args.env_var(super::accounts::LOGIN_ENV).map(str::to_owned);
                st.tab.clone_from(&tab);
                st.notice_pid = sf.pid;
                st.notice_start = squash(&sf.proc_start);
            }
            result
        }
        Step::Terminate if opts.dry_run => said(r, "would-restart"),
        Step::Fresh if opts.dry_run => said(r, "would-restart:fresh"),
        // The restart: after READY, or afresh for a taskless conversation,
        // with nothing told and nothing resumed.
        Step::Terminate | Step::Fresh => {
            let fresh = step == Step::Fresh;
            // A quiet restart ([`upgrade::Facts::quiet`]) of a round that typed
            // nothing: its model rides the restart as a fresh one's does, and
            // nothing is typed after it. A pending round that still owes this
            // agent a release did type it a notice once (abandoned by a
            // retarget, `--defer` or re-armed give-up, [`St::owe_release`]),
            // so the agent may have answered READY and stopped: it is carried
            // on as an announced round's is. A release owed to another
            // process is not this one's ([`release_void`]).
            let unasked = !fresh && facts.quiet && asked_about == Phase::Pending;
            let owed_here = !st.release.is_empty() && st.notice_belongs_to(sf, &tab);
            let quiet = unasked && !owed_here;
            st.quiet = quiet;
            if fresh || unasked {
                st.model_list = model_to.clone().unwrap_or_default();
            }
            if !fresh {
                // What the agent ran, from the tail that just proved READY, and
                // where that read ended (the resumed session's turns come after
                // it). Recorded with the rest of the relaunch before the
                // signal; a restart that waits instead takes both again.
                (st.model_before, st.mark) = model_and_mark(recent.as_ref());
            }
            st.launch_model = launch_kept(opts, &sf.session_id, &args.argv).unwrap_or_default();
            let hand = st.model_list.is_empty().then(hand_model).flatten();
            let restarting = Restarting {
                argv: &args.argv,
                target: &target,
                host_claim,
                // The person's grace at this rung: `[harness] human_grace_s`,
                // then KEYS_GAP_S from KeysOnly and at once under `--now`
                // (the last rung). Never zero.
                grace: upgrade::person_grace(
                    if request == Request::Now {
                        upgrade::Rung::Land
                    } else {
                        facts.rung
                    },
                    opts.human_grace_s,
                ),
                fresh,
                quiet,
                // The quiet step reads a status that only lags past on the
                // owner's `--now` alone (round eight, #5): the last look
                // before the signal reads it past the same way, or the
                // owner's word moved nothing (`wait:changed` at every look).
                lagging: !fresh
                    && facts.quiet
                    && request == Request::Now
                    && upgrade::status_lags(&facts),
                mode: shown_mode.as_deref(),
                model: hand.as_deref(),
                login: args.env_var(super::accounts::LOGIN_ENV),
            };
            restart(opts, r, &mut st, &mut c, &tab, sf, t, k, &restarting)
        }
    };
    // The word the restart's own checks said, past the reducer's go
    // ([`St::restart_look`]): kept only where it is the visit's last word.
    let restart_word = (!opts.dry_run && matches!(step, Step::Terminate | Step::Fresh))
        .then(|| result.step.clone());
    // The owed release: an upgrade that abandoned its notice types one line
    // telling the agent to go on, here if it is the next act and may be typed
    // here, else at the next point ([`release`]). A background-work break
    // counts: the line ends nothing. The READY it waits behind is re-read off
    // the record as the step left it (a stop in this step forgot its markers,
    // and a READY nothing acts on holds nothing).
    let result = if release_next(&step, &result.step, &st, &facts, ready) {
        let ready = heard(&st, sf, &tab, tail);
        release(
            opts, result, &mut st, &mut c, &tab, sf, host_claim, k, &facts, ready, tail,
        )
    } else {
        result
    };
    // A work-end re-arm whose notice did not go this visit is still the
    // visit's act, reported as the re-arm so the host restarts its counts.
    let result = match rearmed_here {
        Some(rearmed) if !result.step.starts_with("announced:") => rearmed,
        _ => result,
    };
    // What this look saw of the agent's own work, for the end of it.
    st.saw_work(&facts);
    st.note_step(&result.step, &facts.status, now);
    st.restart_look =
        result.step.starts_with("wait:") && restart_word.as_deref() == Some(result.step.as_str());
    // Whose copy runs: the vendor's own install (its updater may already have
    // laid the newer build down — "Restart to update" in its footer) or the
    // managed twin (which exports DISABLE_AUTOUPDATER).
    let copy = if running_native { "foreign" } else { "managed" };
    st.annotate_wait(
        now,
        &wait_note(
            &facts,
            &format!("{seen} copy={copy}"),
            facts.background.len(),
        ),
    );
    // What holds a move that waits, named to the owner (`held_by=`): described
    // only where something runs and the step waited.
    let held = if facts.background.is_empty() || !result.step.starts_with("wait:") {
        Vec::new()
    } else {
        k.describe(sf.pid, &background_procs(sf.pid, t))
    };
    st.note_held(&result.step, &held, now);
    save(opts, &sf.session_id, &st);
    match held_point {
        Some(held) if rounds_step(&result.step) => {
            let word = at_point(&result.step, &held);
            said(result, word)
        }
        _ => result,
    }
}

/// The separator between a dry read's act and what holds its point
/// ([`at_point`]): `gave-up@box`, `would-rearm:unanswered@not-idle:busy`. No
/// step or wait word contains it ([`upgrade::wait_word`] cuts a status to
/// `[a-z-]`).
pub(super) const AT_POINT: char = '@';

/// Whether a dry read's word is the round's own next step: the reducer's
/// announce (`would-announce`), restart (`would-restart`,
/// `would-restart:fresh`, `would-restart:model=…`), give-up (`gave-up`, the
/// one act a dry run reports as done) or re-arm (`would-rearm:<why>`),
/// whatever [`at_point`] appended. Not the lane's edges (an owed release, a
/// void, a refusal, a restart carried on), for which the watch records no
/// word (`upgrade_watch::look_word`).
pub(super) fn rounds_step(word: &str) -> bool {
    let act = word.split_once(AT_POINT).map_or(word, |(act, _)| act);
    act == "would-announce"
        || act == "gave-up"
        || act == "would-restart"
        || act.starts_with("would-restart:")
        || act.starts_with("would-rearm:")
}

/// A dry read's act `word` with what holds its point (`held`, a wait word):
/// `<word>@<held>` ([`AT_POINT`]). Said by the visit's dry run for a give-up
/// or re-arm read off any point ([`upgrade::held_point`]) and recorded by the
/// host's watch.
pub(super) fn at_point(word: &str, held: &str) -> String {
    format!("{word}{AT_POINT}{held}")
}

/// The ladder's reading of one look, shared by both lanes' looks (Claude
/// Code's [`look`], the Codex visit) and driven by the ladder's Tier-1 bind,
/// so the bind reads a screen exactly as a look does. From one read of `scr`
/// of agent `agent` (the reader's name: `claude`, `codex`) run by `pid`: the
/// rung ([`St::rung_at`]); a person near the tab within `grace_s`
/// ([`upgrade::attended_by`]) and a keystroke within [`upgrade::KEYS_GAP_S`]
/// (an unknown stamp is none here; `attended` fails closed below KeysOnly and
/// typing shows as a draft); the reader's idle-as-evidence reading with no
/// wall or prompt; and the repaint-proof settle ([`St::still`],
/// [`upgrade::still_print`]).
pub(super) fn ladder_look(
    st: &mut St,
    agent: &str,
    scr: &Screen,
    pid: u32,
    grace_s: u32,
    now: u64,
) -> LadderLook {
    let reading = aterm_phase::read_at(
        Some(agent),
        &scr.rows,
        scr.cursor.map(|(row, _)| row),
        scr.cursor.map(|(_, col)| col),
    );
    let idle_read = reading.phase == aterm_phase::Phase::Idle
        && reading.phase_authoritative
        && reading.wall.is_none()
        && reading.prompt.is_none();
    let print = reading
        .composer
        .as_ref()
        .filter(|_| idle_read)
        .map(|(caret, _)| upgrade::still_print(pid, &scr.rows, *caret));
    LadderLook {
        rung: st.rung_at(now),
        attended: upgrade::attended_by(scr.human, grace_s),
        typing: scr
            .human
            .within(upgrade::person_grace(upgrade::Rung::KeysOnly, grace_s))
            == Some(true),
        idle_read,
        still_s: st.still(print, now),
    }
}

/// [`ladder_look`]'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LadderLook {
    /// [`upgrade::Facts::rung`].
    pub(super) rung: upgrade::Rung,
    /// [`upgrade::Facts::attended`].
    pub(super) attended: bool,
    /// [`upgrade::Facts::typing`].
    pub(super) typing: bool,
    /// The one reader read the screen idle as evidence, no wall, no prompt:
    /// what a run of idle looks counts ([`St::idle_looks`]), and where the
    /// window's loop would take its step at all.
    pub(super) idle_read: bool,
    /// [`upgrade::Facts::still_s`].
    pub(super) still_s: u64,
}

/// Whether a turn runs, by the `esc to interrupt` hint
/// ([`upgrade::Facts::busy_footer`]), read only below the composer (its bottom
/// rule and the footer, where Claude Code draws it during a turn) and on the
/// live status row just above the composer's top rule (the spinner row, e.g.
/// `✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)`), located by
/// [`aterm_phase::phase::status_row`]. A footer quoted in the transcript is
/// therefore no running turn. No composer means a box replaced it, which the
/// `box` gate waits on.
pub(super) fn busy_footer(rows: &[String]) -> bool {
    let Some((caret, draft)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    let busy = aterm_phase::anchors::anchor("busy.interrupt");
    aterm_phase::phase::status_row(rows).is_some_and(|row| row.contains(busy))
        || rows
            .iter()
            .skip(caret.saturating_add(draft.len()))
            .any(|row| row.contains(busy))
}

/// One look at the session's tab, everything the gates are asked over: the
/// tab dialled and proven live (and, for the window's step, still the PTY
/// group's claim), its screen read once, and that read's [`Facts`] (person
/// stamp, hold, composer, box, busy footer, the limit ([`upgrade::limited`]),
/// work under the agent), with the screen's quiet and a person's hold timed
/// across looks on `st`, an announced upgrade's re-ask clock held while
/// limited ([`upgrade::clock_held`]), and the permission mode the read's pill
/// shows ([`screen_mode`]), which a relaunch resumes in. `Err` names the
/// wait.
fn look(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    host_claim: Option<&HostClaim>,
    t: &[(u32, u32, String)],
    now: u64,
) -> Result<(Client, Facts, Option<String>, HandAt, String), &'static str> {
    let mut c = connect(opts, tab).map_err(|_| "no-socket")?;
    if !roster(&mut c).iter().any(|live| live == tab) {
        return Err("tab-not-live");
    }
    if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
        return Err("tab-ownership-changed");
    }
    let scr = screen(&mut c, tab).ok_or("screen-unreadable")?;
    // A person at the tab (the server's per-session person stamp on this read,
    // within `human_grace_s`) and the ladder: the rung, the keystroke within
    // KEYS_GAP_S every rung keeps, and the session reader's reading of this
    // screen as the loop vets an idle point, with its last words across looks
    // ([`ladder_look`]).
    let ladder = ladder_look(st, "claude", &scr, sf.pid, opts.human_grace_s, now);
    // One `status` read: the hold and hands, and the agent's verdict the quiet
    // is measured from ([`quiet_s`]).
    let status = status_line(&mut c, tab);
    let quiet = quiet_s(status.as_deref(), st, scr.seq, now);
    let mut facts = Facts {
        status: sf.status.clone(),
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        composer_empty: composer_empty(&mut c, tab, &scr),
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some(),
        busy_footer: busy_footer(&scr.rows),
        background: background(sf.pid, t),
        // The hold and the hands alone: a person's keystroke is `attended`,
        // which the owner's `--now` waives.
        held: status.as_deref().is_none_or(|line| held_by_status(line, 0)),
        quiet_s: quiet,
        hold_s: 0,
        owner_now: false,
        attended: ladder.attended,
        background_point: opts.background,
        // Asked only where it can move the step: before anything is ended.
        taskless: matches!(st.phase, Phase::Pending | Phase::Announced { .. })
            && tasked(&opts.home, &sf.session_id, &supervisor_typed(opts, tab)) == Some(false),
        limited: upgrade::limited(upgrade::Agent::Claude, &scr.rows),
        // The screen's word; the transcript's is added by the visit, which
        // reads the tail ([`visit_models`]).
        login: upgrade::login_wall(upgrade::Agent::Claude, &scr.rows),
        undelivered: false,
        // The transcript's word, added by the visit ([`queue_facts`]).
        queued: false,
        ready_s: 0,
        idle_looks: 0,
        // Stamped by the visit ([`St::time_failed`]).
        failed_s: 0,
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        // Claude Code's own work shows in its status and under it.
        daemon_turn: upgrade::DaemonTurn::None,
        // Stamped by the visit ([`St::time_work`]).
        work_ended: false,
        // The screen's half: the agent's last words on an idle-as-evidence
        // screen answer READY to this round. The visit keeps it only where the
        // transcript does not yet hear that READY ([`visit_models`]).
        ready_unrecorded: ladder.idle_read
            && !opts.background
            && aterm_phase::said_tail(&scr.rows)
                .is_some_and(|said| st.round_markers().any(|m| upgrade::says_ready(&said, m))),
        // The transcript's word, added by the visit.
        ready_superseded: false,
        // `[harness] upgrade_ask = false`, the default: nothing is typed
        // before the move ([`upgrade::Facts::quiet`]).
        quiet: !opts.ask,
        // The owner's waiting record has stood a look before a move that
        // types nothing first ([`upgrade::Facts::unnoticed`]).
        unnoticed: now.saturating_sub(st.behind_since()) < upgrade::quiet_s(),
    };
    // The session's reader, as the loop vets an idle point: what Claude's
    // status is read against ([`upgrade::IDLE_LOOKS`]). A run is one
    // process's consecutive looks ([`St::idle_looks`]).
    let idle_read = ladder.idle_read;
    let in_a_row = st.idle_pid == sf.pid && now.saturating_sub(st.idle_at) <= IDLE_RUN_GAP_S;
    st.idle_looks = match (idle_read, in_a_row) {
        (false, _) => 0,
        (true, true) => st.idle_looks.saturating_add(1),
        (true, false) => 1,
    };
    (st.idle_pid, st.idle_at) = if idle_read { (sf.pid, now) } else { (0, 0) };
    facts.idle_looks = st.idle_looks;
    (st.hold_since_s, st.hold_seen_s) =
        upgrade::hold_since((st.hold_since_s, st.hold_seen_s), &facts, now);
    if st.hold_since_s != 0 {
        facts.hold_s = now.saturating_sub(st.hold_since_s);
    }
    st.phase = upgrade::clock_held(&st.phase, &facts, now);
    // What the look saw that no fact carries, for the wait log ([`wait_note`]):
    // the engine's render state, and where the cursor and the composer's caret
    // stood — the two a displaced grid parts.
    let render = status
        .as_deref()
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix("render="))
        })
        .unwrap_or("-");
    let place = |at: Option<usize>| at.map_or_else(|| "-".to_string(), |n| n.to_string());
    // A `/goal` in force keeps the agent's status busy at every turn end.
    st.goal = aterm_phase::goal_active(&scr.rows);
    let seen = format!(
        "render={render} cursor={} caret={} goal={}",
        scr.cursor
            .map_or_else(|| "-".to_string(), |(row, col)| format!("{row},{col}")),
        place(aterm_phase::phase::composer_draft(&scr.rows).map(|(caret, _)| caret)),
        u8::from(st.goal),
    );
    Ok((
        c,
        facts,
        screen_mode(&scr.rows),
        HandAt::of(scr.human),
        seen,
    ))
}

/// THE FACTS A WAIT WAS DECIDED ON, one line, for the wait log
/// ([`St::waits`]): the gates' inputs by name, `seen` what the look read
/// beside them ([`look`]), and how many processes ran under the agent.
fn wait_note(f: &Facts, seen: &str, holders: usize) -> String {
    let b = |v: bool| u8::from(v);
    format!(
        "status={} attended={} held={} composer_empty={} box={} busy_footer={} limited={} \
         login={} queued={} quiet_s={} idle_looks={} ready_s={} hold_s={} background={} {seen} \
         holders={holders}",
        f.status,
        b(f.attended),
        b(f.held),
        b(f.composer_empty),
        b(f.approval_box),
        b(f.busy_footer),
        b(f.limited),
        b(f.login),
        b(f.queued),
        f.quiet_s,
        f.idle_looks,
        f.ready_s,
        f.hold_s,
        f.background.len(),
    )
}

/// How far back a give-up's last notice is looked for when the ordinary tail
/// ([`TAIL_BYTES`]) no longer holds it ([`login_facts`]); a conversation can
/// run on for hundreds of KB after the login returns.
const GAVE_UP_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// The login wall from the transcript `tail` ([`upgrade::Facts::login`],
/// [`upgrade::Facts::undelivered`]), which the screen alone cannot give:
///
/// * the wall still standing after its row left the screen
///   ([`upgrade::transcript_login_wall`]): the facts wait `login` and the
///   re-ask clock is held;
/// * the wall lifted since the latest notice: the window restarts from the
///   lift ([`upgrade::login_lifted_at`], [`St::hold_clock`]), so a notice read
///   before the login went is not re-asked or given up on at once;
/// * whether the latest notice reached the model ([`upgrade::notice_fate`]),
///   via [`upgrade::transcript_login`].
///
/// For a given-up upgrade ([`upgrade::GAVE_UP`]) the round's notices are read
/// further back ([`GAVE_UP_TAIL_BYTES`]; give-ups are visited only at attach or
/// activation), and where its last notice was the wall's the give-up is taken
/// back ([`upgrade::rearmed`]), counting the notices the model did receive by
/// the recorded markers or, for an older state, as they were minted
/// ([`upgrade::round_markers`]).
fn login_facts(
    opts: &Opts,
    session: &str,
    st: &mut St,
    f: &mut Facts,
    tail: Option<&str>,
    now: u64,
) -> Option<Phase> {
    let said = tail.map_or_else(upgrade::TranscriptLogin::default, |t| {
        upgrade::transcript_login(&st.phase, &st.marker, t)
    });
    if said.stands {
        f.login = true;
        st.phase = upgrade::clock_held(&st.phase, f, now);
    }
    if let Some(lifted) = said.lifted_at {
        let _ = st.hold_clock(lifted);
    }
    let gave_up =
        matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && !st.marker.is_empty();
    if !gave_up {
        f.undelivered = said.undelivered == Some(true);
        return None;
    }
    let deep = transcript(&opts.home, session).map(|p| tail_to_end(&p, GAVE_UP_TAIL_BYTES).0);
    let text = deep.as_deref().or(tail)?;
    f.undelivered = upgrade::notice_fate(text, &st.marker) == Some(true);
    let markers = if st.asked.is_empty() {
        Version::parse(&st.to)
            .map_or_else(Vec::new, |to| upgrade::round_markers(session, &to, st.salt))
    } else {
        st.asked.clone()
    };
    upgrade::rearmed(
        &st.phase,
        f.undelivered,
        upgrade::notices_received(text, &markers),
    )
}

/// A usage limit, and a notice queued behind one, from the transcript `tail`:
///
/// * The session's last word is Claude Code's limit row and its reset (or, if
///   it names none, [`upgrade::REASK_S`] after the row) is still to come
///   ([`upgrade::transcript_limit_until`]; a later finished `/login` ends it):
///   the session is limited ([`upgrade::Facts::limited`]) whatever the screen
///   shows. Under the quiet upgrade, once a named reset is [`LIMIT_GRACE_S`]
///   past, the screen's notice is no limit either.
///
/// And of the conversation's latest upgrade notice (this ask's for an
/// announced upgrade, any notice's otherwise, since retargets and new rounds
/// leave notices with markers the state no longer names), in one scan
/// ([`upgrade::notice_scan`]):
///
/// * Answered by the limit and not taken since
///   ([`upgrade::NoticeFate::Queued`]): while the limit holds by the
///   transcript ([`upgrade::Scan::queued_until`]) the session is limited,
///   nothing is typed behind it and its clock is held ([`upgrade::clock_held`]).
///   Once that runs out with no limit on screen, this ask's notice never
///   reached the model ([`upgrade::Facts::undelivered`], retyped as the same
///   ask), and another's leaves a pending upgrade free to follow it. But with
///   more than [`upgrade::REQUEUE_MAX`] untaken notices
///   ([`upgrade::Scan::queued_copies`]) the queue is full
///   ([`upgrade::Facts::queued`]): nothing typed, clock held, waiting `queued`
///   until the model writes a row of its own, the queue has rested
///   ([`upgrade::Scan::rests_until`]), or the owner's `--now` comes after the
///   latest notice (`request`, since `st.request_at`), never while a limit
///   stands. Each of those lets exactly one more notice go.
/// * Taken later ([`upgrade::NoticeFate::Taken`]): its window opens when the
///   model took it ([`St::hold_clock`]).
///
/// `harness_upgrade_limit_queue_model` (aterm-spec) is bound to this fold,
/// [`upgrade::next_step`], [`upgrade::requested_step`] and
/// [`upgrade::announce_asks`] over every reachable state
/// (`upgrade_queued_tests.rs`).
fn queue_facts(
    st: &mut St,
    f: &mut Facts,
    tail: Option<&str>,
    now: u64,
    request: &Request,
) -> bool {
    let Some(text) = tail else {
        return false;
    };
    // Until when the limit holds the session (its named reset, else a re-ask's
    // wait after a row naming none), and the named reset alone, kept for the
    // owner's view whether past or future ([`St::limit_until`]). A row naming
    // none keeps `0` there, and the view bounds it by the week
    // (`upgrade_status::LONGEST_LIMIT_S`), so a wait is never shown as a
    // passed reset.
    let until = upgrade::transcript_limit_until(text);
    let reset = upgrade::transcript_limit_reset(text);
    st.limit_until = reset.unwrap_or(0);
    // And whether that limit is money, no usage window ([`St::limit_spend`]).
    st.limit_spend = upgrade::transcript_limit_spend(text);
    // The quiet upgrade: a notice left on screen [`LIMIT_GRACE_S`] past the
    // reset its row names is no limit. The session can take a turn, its loop
    // offers the point (`supervise::run`'s `limit_spent`), and the restart,
    // which types nothing, goes there. A future reset, or a row naming none,
    // leaves the screen's word: a reset is never guessed. The ask flow
    // (`upgrade_ask = true`) keeps the screen's word; whether a notice may be
    // typed there is `HarnessUpgradeLimitQueue`'s (`shown`), pinned by its bind.
    if f.quiet && reset.is_some_and(|reset| now >= reset.saturating_add(LIMIT_GRACE_S)) {
        f.limited = false;
    }
    if until.is_some_and(|until| until > now) {
        f.limited = true;
        st.phase = upgrade::clock_held(&st.phase, f, now);
    }
    let ours = matches!(st.phase, Phase::Announced { .. }) && !st.marker.is_empty();
    let marker = if ours {
        st.marker.as_str()
    } else {
        upgrade::READY_PREFIX
    };
    let Some(scan) = upgrade::notice_scan(text, marker) else {
        return false;
    };
    match scan.fate {
        upgrade::NoticeFate::Queued => {
            if scan.queued_until().is_some_and(|until| until > now) {
                f.limited = true;
                st.phase = upgrade::clock_held(&st.phase, f, now);
                return false;
            }
            if f.limited {
                return false;
            }
            let owner_asked = *request == Request::Now
                && scan.typed_at.is_some_and(|typed| st.request_at > typed);
            if scan.queued_copies() > upgrade::REQUEUE_MAX
                && !owner_asked
                && scan.rests_until().is_none_or(|rested| now < rested)
            {
                f.queued = true;
                st.phase = upgrade::clock_held(&st.phase, f, now);
            }
            if ours {
                f.undelivered = true;
            }
            ours
        }
        upgrade::NoticeFate::Taken(Some(at)) if ours => {
            let _ = st.hold_clock(at.min(now));
            false
        }
        _ => false,
    }
}

/// The release, typed if it may be now ([`upgrade::gate_release`]): one line
/// ([`upgrade::release_prompt`]) into the session of an upgrade that abandoned
/// its notice ([`St::release`]), under every fence the notice is typed under
/// (the agent its shell's foreground job on a terminal the tab owns, the
/// conversation's one live owner, the tab still the PTY group's claim, a fresh
/// read with no box or draft whose generation the turn is fenced on). Typed:
/// nothing owed, markers forgotten ([`St::released`]), ledgered once
/// (`released:<why>`). Not typed: still owed; `r` is kept when it is an act
/// the host looks on after (`gave-up`, `drain-expired:…`), else the wait says
/// what holds it (`wait:release:<why>`), never a last word ([`owed_word`]).
/// Called only where the release is the next act ([`release_is_next`]). A
/// release no longer this upgrade's ([`release_void`]: the notified process no
/// longer holds the conversation here, or the agent took up later direction)
/// is dropped instead, said once ([`drop_release`]), and `r` kept.
#[allow(clippy::too_many_arguments)]
fn release(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    host_claim: Option<&HostClaim>,
    k: &dyn Kernel,
    f: &Facts,
    ready: bool,
    tail: Option<&str>,
) -> Report {
    let owed = owed_word;
    if let Some(why) = release_void(st, sf, tab, tail) {
        drop_release(opts, &r, st, why);
        return r;
    }
    if let upgrade::Gate::Wait(why) = upgrade::gate_release(f, ready) {
        return owed(r, why);
    }
    if let Err(why) = foreground_shell(k.job(sf.pid)) {
        return owed(r, why);
    }
    let owner = k.terminal(sf.pid);
    if !owner
        .as_ref()
        .is_some_and(|(pid, name)| owned_by_aterm((*pid, name.as_str())))
    {
        return owed(r, "terminal");
    }
    if opts.dry_run {
        let why = st.release.clone();
        return if r.is_act() {
            r
        } else {
            said(r, format!("would-release:{why}"))
        };
    }
    if let Err(why) = require_unique_owner(&opts.home, sf) {
        return owed(r, why);
    }
    if host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim)) {
        return owed(r, "tab-ownership-changed");
    }
    let text = upgrade::release_prompt_as(st.agent, f.quiet);
    match type_fenced(c, tab, &text, None, Put::Pressed) {
        Ok(()) => {
            let why = st.release.clone();
            st.released();
            let r = said(r, format!("released:{why}"));
            ledger(
                opts,
                &r,
                if f.quiet {
                    "the upgrade abandoned the notice it gave the agent without restarting it: one \
                     line told it the upgrade is off for now, nothing the notice asked of it \
                     still applies, and to carry on as it would have without it"
                } else {
                    "the upgrade abandoned the notice it gave the agent without restarting it: one \
                     line told it the upgrade is off for now, nothing will restart the session \
                     without asking it again first, nothing the notice asked of it still \
                     applies, and to carry on as it would have without it"
                },
            );
            r
        }
        Err(e) => {
            if matches!(e, Typed::Cut(_)) {
                left_cut(st, c, tab, &text);
            }
            owed(r, &e.word())
        }
    }
}

/// The word of a step that leaves a release owed, `why` what holds it: `r` when
/// it is an act of this step the host parks at the next idle point on
/// (`gave-up`, `drain-expired:…`), else `wait:release:<why>`, which the host
/// looks at again ([`after`]). Never a last word while a release is owed, or
/// the window would never look for it again.
fn owed_word(r: Report, why: &str) -> Report {
    if r.is_act() && after(&r.step, 0) == After::NextIdle {
        r
    } else {
        said(r, format!("wait:release:{why}"))
    }
}

/// A conversation owed a release with no upgrade to take now ([`release`]):
/// its live process runs the build the upgrade stopped short of and was asked
/// to wind down, because the target it was asked for is gone (rolled back,
/// uninstalled). Any other process holding the conversation (resumed by hand
/// anywhere, often on the new build) is not this upgrade's to tell, so the
/// release is dropped ([`release_void`]), as it is once the agent took up
/// later direction. Otherwise one look, and the release if it may be typed.
/// Either way the step is recorded ([`St::note_step`]); a drop records
/// `current`, clearing a stale `wait=release:<gate>` an earlier look left.
#[allow(clippy::too_many_arguments)]
fn release_visit(
    opts: &Opts,
    r: Report,
    sf: &SessionFile,
    mut st: St,
    tab: &str,
    t: &[(u32, u32, String)],
    k: &dyn Kernel,
    host_claim: Option<&HostClaim>,
) -> Report {
    let now = now_s();
    let recent = transcript(&opts.home, &sf.session_id).map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    if let Some(why) = release_void(&st, sf, tab, tail) {
        if opts.dry_run {
            return said(r, format!("would-drop-release:{why}"));
        }
        drop_release(opts, &r, &mut st, why);
        let r = said(r, "current");
        st.note_step(&r.step, &sf.status, now);
        save(opts, &sf.session_id, &st);
        return r;
    }
    let (mut c, facts, _, _, _) = match look(opts, sf, &mut st, tab, host_claim, t, now) {
        Ok(look) => look,
        Err(why) => return said(r, format!("wait:release:{why}")),
    };
    // Aterm's own words left in the prompt, first ([`left_first`]): a release
    // the host stopped part-way included.
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, tab, sf, now) {
        return r;
    }
    let r = release(
        opts,
        said(r, "wait:release"),
        &mut st,
        &mut c,
        tab,
        sf,
        host_claim,
        k,
        &facts,
        false,
        tail,
    );
    st.note_step(&r.step, &facts.status, now);
    save(opts, &sf.session_id, &st);
    r
}

/// Why an owed release is no longer this upgrade's to type into `sf` in `tab`,
/// if so: `other-process` (the notified process no longer holds the
/// conversation in its tab, [`St::notice_belongs_to`]; the line would reach an
/// agent never asked) or `directed` (someone spoke to the agent after its last
/// answer to the upgrade and it took that up, [`upgrade::directed_since_ready`]
/// over `tail`, after a READY to any marker this round typed, [`St::asked`];
/// the line would pull it off that newer direction). A direction the agent
/// answered with READY, or has not answered yet, drops nothing: the agent
/// holds for the restart and only the release can tell it to go on.
fn release_void(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> Option<&'static str> {
    if !st.notice_belongs_to(sf, tab) {
        return Some("other-process");
    }
    tail.is_some_and(|t| upgrade::directed_since_ready(t, &st.asked))
        .then_some("directed")
}

/// Drops an owed release that is no longer this upgrade's ([`release_void`],
/// `why`): nothing owed, markers forgotten (the round is over as if the line
/// had been typed), ledgered once (`release-dropped:<why>`). A dry run
/// changes nothing.
fn drop_release(opts: &Opts, r: &Report, st: &mut St, why: &str) {
    if opts.dry_run {
        return;
    }
    st.dropped();
    ledger(
        opts,
        &said(r.clone(), format!("release-dropped:{why}")),
        if why == "directed" {
            "someone directed the conversation after the agent's last answer to the upgrade, and \
             the agent took that up: the line telling it to carry on is not typed over that"
        } else {
            "the process the notice reached no longer holds the conversation in its tab: the line \
             is not typed into another"
        },
    );
}

/// What a restart records of the transcript before its signal, from the tail
/// the READY answer was read in ([`tail_to_end`]): the model the agent's last
/// turn named (empty when none did) and where that read ended (`0` for no
/// read) — the mark past which the resumed session's first turn is looked for
/// ([`resumed_model`]).
pub(super) fn model_and_mark(recent: Option<&(String, u64)>) -> (String, u64) {
    recent.map_or((String::new(), 0), |(text, end)| {
        (upgrade::transcript_model(text).unwrap_or_default(), *end)
    })
}

/// Restarts a previous sweep left between the exit and the new process: the
/// agent is gone and no live process holds its conversation yet — by the
/// pass's own list (`files`), and by a FRESH READ of Claude's session files
/// wherever that list says none does ([`registered_since`]).
fn orphans(opts: &Opts, files: &[SessionFile], live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let fresh = std::cell::OnceCell::new();
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::in_flight) else {
            continue;
        };
        // A Codex restart is the Codex lane's to carry on ([`codex`]).
        if st.agent != upgrade::Agent::Claude {
            continue;
        }
        if live_tabs.is_some_and(|tabs| !tab_is_live(tabs, &st.tab)) {
            continue;
        }
        // A live process holding the conversation is `visit`'s; a session file
        // left by a dead owner holds nothing and must not strand the restart.
        if conversation_live(files, &session) {
            continue; // `visit` has it.
        }
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        if registered_since(&fresh, &opts.home, &session) {
            continue; // `visit` has it, or the loop's `resume`.
        }
        let r = Report {
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            step: String::new(),
        };
        // The agent is still exiting (a SIGTERMed Claude Code that removed its
        // session file and hung): never a harder signal, nothing acted on;
        // past the bound it is noted once in the ledger.
        if alive(st.pid) {
            note_stuck(opts, &r, &mut st, &session, now_s());
            continue;
        }
        // Expiry is decided on the exit as an earlier look saw it (the host's
        // carry, the relaunch's wait), else on the signal: this pass's first
        // sighting may come long after the exit and says nothing of when it
        // came (`relaunch::STALE_S`).
        let stale = expired(&st, now_s());
        // Gone: the tab's record from here, stamped before the expiry is acted
        // on and before a socket is dialled.
        if !opts.dry_run && st.seen_gone(now_s()) {
            save(opts, &session, &st);
        }
        if let Some((why, detail)) = stale {
            if opts.dry_run {
                out.push(said(r, format!("would-fail:{why}")));
                continue;
            }
            st.stop(why, now_s());
            let r = said(r, format!("failed:{why}"));
            ledger(opts, &r, &detail);
            save(opts, &session, &st);
            out.push(r);
            continue;
        }
        if opts.dry_run {
            out.push(said(r, format!("would-resume:{}", st.phase.word())));
            continue;
        }
        let tab = st.tab.clone();
        let Ok(mut c) = connect(opts, &tab) else {
            out.push(said(r, "wait:no-socket"));
            continue;
        };
        if !roster(&mut c).contains(&tab) {
            out.push(said(r, "wait:tab-not-live"));
            continue;
        }
        let r = match st.phase {
            Phase::Exiting { .. } => relaunch(opts, r, &mut st, &mut c, &session, &Live),
            _ => await_new(opts, r, &mut st, &mut c, &session, &Live),
        };
        save(opts, &session, &st);
        out.push(r);
    }
    out
}

/// HELD SINCE THE PASS'S LIST WAS READ (the review of the live test of
/// 2026-09-30): [`claude_pass`] reads Claude's session files once, BEFORE its
/// visits act, and a visit that ends its agent relaunches it in the same pass
/// — the new process registers after that list. Read from the list alone,
/// the restart looked orphaned, and the orphan pass carried it again in the
/// same second: the harness's hand on the tab again, the agent's first idle
/// point waited for again, and a second `adopted` row. Whether a live process
/// holds `session` NOW, by one fresh read per pass (`fresh`), taken only for
/// a restart the list shows unheld; a read that fails decides nothing, and
/// the list stands.
fn registered_since(
    fresh: &std::cell::OnceCell<Option<Vec<SessionFile>>>,
    home: &Path,
    session: &str,
) -> bool {
    fresh
        .get_or_init(|| session_files(home))
        .as_deref()
        .is_some_and(|files| conversation_live(files, session))
}

/// Whether the agent has answered a READY marker of THIS round of the upgrade
/// ([`St::marker`], [`St::markers`]) in `tail`, the transcript's tail — as its
/// last word after the latest notice ([`upgrade::transcript_has_ready`]). No
/// marker — none typed yet, or ones the drain voided ([`drain_expired`]) or a
/// release forgot — is no answer, whatever the tail holds.
fn answered(st: &St, tail: Option<&str>) -> bool {
    let Some(text) = tail else {
        return false;
    };
    st.round_markers()
        .any(|m| upgrade::transcript_has_ready(text, m))
}

/// The two READY facts the transcript decides, once the visit has read `tail`
/// and whether the upgrade hears a READY there (`ready`, [`heard`]); both only
/// for a phase that acts on one, from the process and tab its notice reached
/// ([`St::hears_ready`], [`St::notice_belongs_to`]).
///
/// * [`upgrade::Facts::ready_unrecorded`]: kept from the look where the
///   screen's last words answer READY to this round and the transcript, read,
///   does not yet. An unreadable transcript holds nothing.
/// * [`upgrade::Facts::ready_superseded`]: an announced round's READY the
///   agent was directed on from since ([`upgrade::ready_superseded`]).
fn ready_facts(
    st: &St,
    sf: &SessionFile,
    tab: &str,
    tail: Option<&str>,
    ready: bool,
    f: &mut Facts,
) {
    let hearing = !ready && st.hears_ready() && st.notice_belongs_to(sf, tab);
    f.ready_unrecorded &= hearing && tail.is_some();
    f.ready_superseded = hearing
        && matches!(st.phase, Phase::Announced { .. })
        && tail.is_some_and(|t| {
            let markers: Vec<String> = st.round_markers().cloned().collect();
            upgrade::ready_superseded(t, &markers)
        });
}

/// The READY the upgrade will act on, given to the reducer
/// ([`upgrade::next_step`]) and holding the release back
/// ([`upgrade::gate_release`]): the agent [`answered`] a marker of this round,
/// in a phase that acts on one ([`St::hears_ready`]), from the very process
/// and tab its notice reached ([`St::notice_belongs_to`]); resumed elsewhere,
/// the answer is no consent. A READY no phase acts on is none, so it cannot
/// hold the release for ever.
fn heard(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> bool {
    st.hears_ready() && st.notice_belongs_to(sf, tab) && answered(st, tail)
}

/// Whether, after this visit's `step` said `word`, the owed release is the
/// upgrade's next act ([`release`]): it stopped asking or voided the READY
/// (`gave-up`, `drain-expired`), stopped in this very step (`failed:…`,
/// `refused:…`), or waits on nothing it would type or signal
/// ([`upgrade::release_is_next`]). A wait of the notice's or restart's own
/// gate is not: what it waits for goes first ([`owns_turn_ends`]).
fn release_is_next(step: &Step, word: &str) -> bool {
    match step {
        Step::GiveUp | Step::Void(_) => true,
        Step::Wait(why) => upgrade::release_is_next(why),
        Step::Announce | Step::Terminate | Step::Fresh => {
            word.starts_with("failed:") || word.starts_with("refused:")
        }
        // A new round: its first notice supersedes a release still owed.
        Step::Rearm => false,
    }
}

/// Whether the visit tries the owed release now ([`release`]): one is owed
/// ([`St::release`]) and it is the next act ([`release_is_next`]), at a break
/// as at an idle point. At a break an announced upgrade with no READY
/// ([`unanswered_at_break`]) waits `background` where an idle point waits
/// `awaiting-ready`: the notice's window, which a release a void owes goes
/// before. And under the quiet flow, a pending round whose restart its
/// agent's own work holds ([`quiet_release_due`]). `ready` is the READY the
/// step was taken on ([`heard`]).
fn release_next(step: &Step, word: &str, st: &St, f: &Facts, ready: bool) -> bool {
    !st.release.is_empty()
        && (release_is_next(step, word)
            || (*step == Step::Wait("background") && unanswered_at_break(st, f, ready))
            || quiet_release_due(step, st, f))
}

/// A QUIET PENDING ROUND THAT OWES ITS AGENT A RELEASE, ITS RESTART HELD BY
/// THE AGENT'S OWN WORK (round eight, #23): a notice reached the agent once
/// — typed by an older aterm, or under `upgrade_ask` — and the round that
/// typed it was abandoned since (a retarget, a give-up re-armed, the owner's
/// word run out), so the agent was asked to wind down and is owed the line
/// that releases it. Under the ask flow the round's next notice supersedes
/// that line; the quiet flow types none, and its restart — whose carry-on
/// would tell the agent to go on — waits on the work under the agent, a
/// break of it, or a status its idle screen does not bear out
/// ([`upgrade::own_work_holds`]): work that may never end, so the release is
/// the round's next act there, typed under its own gate. Any other wait
/// (the settle, a person, a limit) is the restart's to wait out, and its
/// carry-on goes then.
fn quiet_release_due(step: &Step, st: &St, f: &Facts) -> bool {
    f.quiet
        && st.phase == Phase::Pending
        && matches!(step, Step::Wait(w) if upgrade::own_work_holds(w))
}

/// A break of the agent's own work ([`upgrade::Facts::background_point`])
/// with a notice standing unanswered: announced, and no READY the step was
/// taken on.
fn unanswered_at_break(st: &St, f: &Facts, ready: bool) -> bool {
    f.background_point && !ready && matches!(st.phase, Phase::Announced { .. })
}

/// A new round of a stopped upgrade ([`upgrade::Step::Rearm`]) at `now`: the
/// record re-armed ([`St::rearm`]) and ledgered once as `rearmed:<why>` (why
/// it stopped, how long it rested). Both lanes take it here. Nothing is typed
/// or signalled; the new round's first notice is the next look's. A dry run
/// only says it (`would-rearm:<why>`).
pub(super) fn rearm(opts: &Opts, r: Report, st: &mut St, now: u64) -> Report {
    let stamped = st.failed_at != 0;
    let rested = st.failed_for(now);
    if opts.dry_run {
        let why = match &st.phase {
            Phase::Failed(why) => why.clone(),
            other => other.word(),
        };
        return said(r, format!("would-rearm:{why}"));
    }
    let why = st.rearm(now);
    let r = said(r, format!("rearmed:{why}"));
    let when = if stamped {
        format!("{} ago", upgrade::span(rested))
    } else {
        "before its time was recorded".to_string()
    };
    ledger(
        opts,
        &r,
        &format!(
            "no stop is for good: the round that stopped ({why}) {when} has rested its {}, and \
             a new round starts — new READY markers, its asks reset; its first notice is typed \
             at the next point every gate lets one go",
            upgrade::span(upgrade::RETRY_S)
        ),
    );
    r
}

/// The drain's bound ([`upgrade::DRAIN_S`]): past it, a person's state held for
/// [`upgrade::HOLD_S`] (a box or draft, `why`, [`upgrade::person_hold`]), the
/// agent's background work running [`upgrade::DRAIN_S`] after the answer
/// (`background`), or Claude's status claiming work over an idle screen that
/// long (`status-stale`) still holds the restart gate, so the READY is void
/// ([`St::void`] at `now`). The markers are forgotten so the old answer can
/// never end the agent later, the agent is owed its release, and the ledger
/// says why once, from `f`'s clocks. An announced upgrade stays announced with
/// its clock restarted (re-asked after [`upgrade::REASK_S`], still bounded by
/// [`upgrade::MAX_ASKS`]); a given-up one stays given up, its rest begun again,
/// and a new round asks after [`upgrade::RETRY_S`]. Nothing is signalled or
/// killed. A dry run only says it.
fn drain_expired(opts: &Opts, r: Report, st: &mut St, why: &str, f: &Facts, now: u64) -> Report {
    if opts.dry_run {
        return said(r, format!("would-void:{why}"));
    }
    let asks_again = matches!(st.phase, Phase::Announced { .. });
    st.void(now);
    let r = said(r, format!("drain-expired:{why}"));
    let (what, held_s) = match why {
        "box" => ("a box nobody answered", f.hold_s),
        "draft" => ("a draft nobody sent", f.hold_s),
        // Claude's own `busy`/`shell` over an idle screen: the restart
        // never takes it for idle ([`upgrade::gate_restart`]).
        "status-stale" => (
            "Claude Code's own status, never back to idle over an idle screen,",
            f.ready_s,
        ),
        _ => ("work of the agent's own still running", f.ready_s),
    };
    // The release is owed here, and typed only where the step may type it
    // ([`release`]); a lane that types none (Codex) says nothing of one.
    let owed = if st.release.is_empty() {
        ""
    } else {
        "the agent is owed its release, and "
    };
    ledger(
        opts,
        &r,
        &format!(
            "the READY answer is void: {} min after the announcement {what} had held the \
             restart for {} min; {owed}{}",
            upgrade::DRAIN_S / 60,
            held_s / 60,
            if asks_again {
                "it is asked again later".to_string()
            } else {
                format!(
                    "a new round asks it again in {}",
                    upgrade::span(upgrade::RETRY_S)
                )
            }
        ),
    );
    r
}

/// Records that the agent is no job of a job-control shell
/// ([`Job::NoJobControl`]): the upgrade stops for this (session, target),
/// ledgered once. No release is owed: the line is typed under the notice's
/// fences, which require a foreground job, so it could never be typed.
fn not_a_job(opts: &Opts, r: Report, st: &mut St) -> Report {
    if opts.dry_run {
        return said(r, "would-refuse:not-a-shell-job");
    }
    st.stop("not-a-shell-job", now_s());
    st.release.clear();
    let r = said(r, "refused:not-a-shell-job");
    ledger(
        opts,
        &r,
        "the agent is no job of an interactive shell: nothing would take the terminal back to relaunch it",
    );
    r
}

/// The agent's terminal is not the tab's ([`owned_by_aterm`]): typing into the
/// tab would not reach it, so the session is not moved. A wait (it may run in
/// the tab next time), said once per (session, target, reason) in the ledger
/// and as an act the host logs, so the reason is visible. A dry run only says
/// the wait.
fn held_back(opts: &Opts, r: Report, st: &mut St, session: &str, why: &str) -> Report {
    if opts.dry_run || st.noted == why {
        return said(r, format!("wait:{why}"));
    }
    st.noted = why.to_string();
    let r = said(r, format!("held-back:{why}"));
    ledger(
        opts,
        &r,
        "the agent's terminal is not the tab's (a multiplexer pane, `script`, ssh): nothing typed into the tab would reach it, so it is not moved",
    );
    save(opts, session, st);
    r
}

/// A restart under way that has not moved ([`St::stuck`]: `exiting`, `exited`
/// or `relaunched` past [`STALE_S`]), noted once in the ledger as
/// `stuck:<what>`. A SIGTERMed Claude Code hung in shutdown, or a relaunched
/// agent whose carry-on never finds it idle, waits with no bound, and the
/// ledger is where the owner diagnoses a strand. Keyed on [`St::noted`] like
/// [`held_back`]: the same kind again writes nothing; another kind is its own
/// row. A note only: never a failure (a failed record would leave a
/// later-exiting agent un-relaunched), a signal or typing; the step's wait
/// word stands and the window's band row is the live view. Claude Code
/// upgrade records only (Codex bounds and ledgers its own; a relaunch record
/// is in no `--status`), never from the read-only `--status`. A dry run
/// writes nothing.
pub(super) fn note_stuck(opts: &Opts, r: &Report, st: &mut St, session: &str, now: u64) {
    if opts.dry_run || st.agent != upgrade::Agent::Claude || !st.is_upgrade() {
        return;
    }
    let Some(what) = st.stuck(now) else {
        return;
    };
    let (Phase::Exiting { at_s } | Phase::Relaunched { at_s }) = st.phase else {
        return;
    };
    let key = format!("stuck:{what}");
    if st.noted == key {
        return;
    }
    st.noted.clone_from(&key);
    ledger(
        opts,
        &said(r.clone(), key),
        &format!(
            "under way for {}, past its {} bound: {}",
            upgrade::span(now.saturating_sub(at_s)),
            upgrade::span(STALE_S),
            upgrade_status::stuck_words(st.agent, what)
        ),
    );
    save(opts, session, st);
}

/// The tab is attended ([`upgrade::Facts::attended`]) and `--now` is not in
/// force: the upgrade waits for the person to step away. Said once per
/// upgrade like [`held_back`], in the ledger and as an act the host logs
/// (`held-back:attended`), since the host notes only acts; later steps say
/// `wait:attended`. A dry run only says the wait.
fn attended_once(opts: &Opts, r: Report, st: &mut St) -> Report {
    const WHY: &str = "attended";
    if opts.dry_run || st.noted == WHY {
        return said(r, format!("wait:{WHY}"));
    }
    st.noted = WHY.to_string();
    let r = said(r, format!("held-back:{WHY}"));
    ledger(
        opts,
        &r,
        "a person is at this tab (a person gave this session input within `[harness] \
         human_grace_s`, by aterm's per-session stamp `status human_ms=`): nothing is typed into \
         it on the upgrade's own judgment until they step away, or the owner's `aterm harness \
         upgrade <sid> --now`",
    );
    r
}
/// The announcement, for a restart that can be carried out. `planned` is
/// [`plan`]'s answer, asked first: a notice for a launch the signal would then
/// refuse interrupts the agent for nothing. `send` types the notice as the
/// `asks`-th ([`upgrade::announce_asks`]) and answers its READY marker;
/// `again` is why it is typed again (a notice the login wall answered, a
/// give-up taken back), ledgered beside the marker.
#[allow(clippy::too_many_arguments)]
fn announce(
    opts: &Opts,
    r: Report,
    st: &mut St,
    planned: Result<Plan, NoPlan>,
    now: u64,
    asks: u32,
    again: Option<&str>,
    send: impl FnOnce(u32) -> Result<String, String>,
) -> Report {
    if let Err(no) = planned {
        return unplanned(opts, r, st, no);
    }
    if opts.dry_run {
        return said(r, "would-announce");
    }
    match send(asks) {
        Ok(marker) => {
            st.announced(marker, now, asks);
            let r = said(r, format!("announced:{asks}"));
            let detail = match again {
                Some(why) => format!("{} ({why})", st.marker),
                None => st.marker.clone(),
            };
            ledger(opts, &r, &detail);
            r
        }
        // A turn found running at the fence's fresh read: the gate's own
        // wait, the notice never tried ([`claude_fence`]).
        Err(e) if e == TURN_RUNNING => said(r, format!("wait:{TURN_RUNNING}")),
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
}

/// What the upgrade's relaunch line is made from: the live session's
/// conversation and directory, its argv, the target build, the model the
/// conversation moves to, if any, the permission mode it last ran in, and
/// whether it is resumed (`false`: a conversation nobody has asked anything,
/// started afresh).
fn launch_of<'a>(
    sf: &'a SessionFile,
    argv: &'a [String],
    target: &'a Candidate,
    model: Option<&'a str>,
    mode: Option<&'a str>,
    resume: bool,
    login: Option<&'a str>,
) -> Launch<'a> {
    Launch {
        session: &sf.session_id,
        resume,
        cwd: &sf.cwd,
        agent: sf.pid,
        argv,
        exe: &target.exe,
        model,
        mode,
        login,
    }
}

/// The last look before typing into Claude's composer: a fresh read with an
/// empty composer and no box, and that read's generation, which the typed turn
/// is fenced on ([`turn_fenced`]).
///
/// The server checks the generation after its `yield=0.2` park and types
/// nothing if any output came since the read, a repaint included, so nothing
/// of ours may sit between the read and the `turn`. The fenced `turn`'s `help
/// turn` probe is asked here first (kept per connection, [`turn_takes_gen`]);
/// a read that needs the `cell` [`composer_empty`] asks at column 2 asks it
/// and reads the screen once more, and the turn is fenced on that last read if
/// its composer row and cursor match what the `cell` judged, else `changed`.
/// A screen that repaints faster than the park still refuses; the narrower
/// rows fence is [`claude_fence`].
pub(super) fn typing_fence(c: &mut Client, tab: &str) -> Result<Option<String>, &'static str> {
    typing_fence_read(c, tab).map(|(generation, _)| generation)
}

/// [`typing_fence`] plus the read its generation names, which a later notice
/// try is judged on ([`retry_open`]), so no read sits between that judgment
/// and the paste's fence.
fn typing_fence_read(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let _ = turn_takes_gen(c);
    let Some(first) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    let Some(dim_at) = dim_cell(&first) else {
        return fence_verdict(&first, false).map(|generation| (generation, first));
    };
    let dim = c
        .request_line(&format!("@{tab} cell {dim_at} 2"))
        .is_ok_and(|line| cell_is_dim(&line));
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    if !same_composer(&first, &scr) {
        return Err("changed");
    }
    fence_verdict(&scr, dim).map(|generation| (generation, scr))
}

/// The prefix [`claude_fence`] marks its narrow fence with, in the string a
/// generation otherwise rides in ([`turn_fenced`] reads it back).
const ROWS_FENCE: &str = "rows:";

/// [`claude_fence`]'s word for a read that shows A TURN RUNNING: the gate's
/// own `busy` — the notice was never tried, so it is no refusal of aterm's
/// fence ([`announce`] says it as the wait it is, `wait:busy`; the release
/// owes it, `wait:release:busy`).
const TURN_RUNNING: &str = "busy";

/// [`typing_fence_read`] for CLAUDE CODE'S input box (the owner's ruling of
/// 2026-09-29, design record 2026-09-28 §3.2 C7b): the same read and verdict,
/// fenced on the box's three rows ([`composer_rows`]) where the host takes
/// `turn if-rows=`, instead of on the whole screen's generation. `if-gen=`
/// refused on ANY output since the read, and Claude Code's screen repaints
/// itself — its spinner, a workflow's ticking timer, rows above the box — so
/// tab #1's notice was refused at every look (2026-09-28). What the fence
/// exists for lands in the box: a person's keystroke echoed on its caret row,
/// a box drawn over it. A screen whose box is not that shape, or a host
/// without the fence, keeps the generation. The help answer is the one
/// [`typing_fence_read`] asked before its read, so nothing of ours sits
/// between the read and the `turn`.
///
/// NO TURN RUNNING ON THE READ IT IS TAKEN ON (round eight, #21): the rows
/// fence sees nothing above or below the box, so the spinner and the `esc to
/// interrupt` of a turn that began after the look — a person's Enter, a
/// background task's end that woke the agent — fall outside it, and the
/// notice or the release was pasted and submitted into that turn. The
/// whole-screen `if-gen=` refused such a screen only where it repainted in
/// the moment after the read. So the read is held to the look's own turn
/// fact ([`busy_footer`], [`upgrade::Facts::busy_footer`]), whichever fence
/// rides it: [`TURN_RUNNING`].
fn claude_fence(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let (generation, read) = typing_fence_read(c, tab)?;
    if busy_footer(&read.rows) {
        return Err(TURN_RUNNING);
    }
    let narrow = composer_rows(&read)
        .filter(|_| turn_takes_rows(c))
        .map(|fence| format!("{ROWS_FENCE}{fence}"));
    Ok((narrow.or(generation), read))
}

/// Claude Code's input box in one read, as a rows fence: the caret row and
/// the two rules around it (`────…` / `❯` / `────…`), in grid rows, hashed by
/// the definition the host shares
/// ([`aterm_types::control_verbs::RowsFence::hash_rows`]). `None` when the
/// cursor's row is not the caret row of the frame
/// [`aterm_phase::phase::composer_rules`] reads, with its rules directly above
/// and below (another program's composer, a dialog, a short read).
///
/// Uses the one frame reader, which accepts a labelled top rule (`──── xhigh
/// ─`, `sandboxed`). The label is hashed with the rows; it changes only when a
/// person changes a setting, and a fence refused then is refused rightly.
fn composer_rows(scr: &Screen) -> Option<aterm_types::control_verbs::RowsFence> {
    let (caret, _) = scr.cursor?;
    let above = caret.checked_sub(1)?;
    let below = caret.checked_add(1)?;
    if aterm_phase::phase::composer_rules(&scr.rows) != Some((above, below)) {
        return None;
    }
    let rows = scr.rows.get(above..=below)?;
    Some(aterm_types::control_verbs::RowsFence {
        first: scr.first.checked_add(above)?,
        last: scr.first.checked_add(below)?,
        hash: aterm_types::control_verbs::RowsFence::hash_rows(rows.iter().map(String::as_str)),
    })
}

/// When a person last gave the tab input, as one read stamped it, on this
/// process's clock ([`HumanInput`] is an age at the read), compared by a later
/// notice try ([`retry_open`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandAt {
    /// The host sent no stamp: nothing can be proved of a person's hand.
    Unknown,
    /// No person since the host began serving the session.
    Never,
    /// About then.
    At(Instant),
}

impl HandAt {
    /// The stamp of a read taken just now.
    pub(super) fn of(human: HumanInput) -> HandAt {
        match human {
            HumanInput::Unknown => HandAt::Unknown,
            HumanInput::Never => HandAt::Never,
            HumanInput::Ago(ms) => Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .map_or(HandAt::Unknown, HandAt::At),
        }
    }

    /// Whether `later` (a later read's stamp) proves no person gave input
    /// since this one: the same keystroke within [`HAND_SLACK`] (two ages off
    /// one clock at two replies), or still nobody. An unknown stamp on either
    /// side fails closed.
    pub(super) fn still(self, later: HandAt) -> bool {
        match (self, later) {
            (_, HandAt::Never) => true,
            (HandAt::At(then), HandAt::At(now)) => now <= then + HAND_SLACK,
            _ => false,
        }
    }
}

/// How far apart two reads may place the same keystroke ([`HandAt::still`]):
/// each age is the server's at its read, excluding the reply's trip back.
const HAND_SLACK: Duration = Duration::from_millis(500);

/// What a notice try after the first ([`NOTICE_FENCE_TRIES`]) is judged
/// against ([`retry_open`]): the look's person stamp, whether `--now` waives a
/// person at the tab, whether the look was at a background-work break, and
/// the person's grace at the look's rung ([`upgrade::person_grace`]).
pub(super) struct Retry {
    pub(super) looked: HandAt,
    pub(super) owner_now: bool,
    pub(super) at_break: bool,
    pub(super) grace_s: u32,
}

/// Whether a notice its fence refused as `changed` may be typed again in the
/// same visit ([`NOTICE_FENCE_TRIES`]): `read`, the fenced read, still meets
/// the look's gates, since the movement may have been a person or the program
/// starting a turn. It requires no person's hand since the look (a message
/// submitted in between would have the notice land inside its turn) and none
/// within the grace unless `--now` waives it; no running turn (busy footer or
/// live status row, [`busy_footer`]); and at an idle point, the session reader
/// authoritatively idle with no wall or box (a woken agent reads busy). At a
/// background-work break the footer reads busy by definition, and the gate's
/// own turn fact applies. Otherwise the refusal stands and the next look
/// judges the screen.
pub(super) fn retry_open(read: &Screen, retry: &Retry) -> bool {
    let hand = HandAt::of(read.human);
    if !retry.looked.still(hand) {
        return false;
    }
    if !retry.owner_now && upgrade::attended_by(read.human, retry.grace_s) {
        return false;
    }
    if busy_footer(&read.rows) {
        return false;
    }
    if retry.at_break {
        return true;
    }
    reads_idle(read)
}

/// Whether Claude Code's own reader reads `read` AT AN IDLE POINT: idle,
/// authoritatively, no wall, no box — what a look counts as an idle look
/// ([`ladder_look`]'s `idle_read`).
fn reads_idle(read: &Screen) -> bool {
    let reading = aterm_phase::read_at(
        Some("claude"),
        &read.rows,
        read.cursor.map(|(row, _)| row),
        read.cursor.map(|(_, col)| col),
    );
    reading.phase == aterm_phase::Phase::Idle
        && reading.phase_authoritative
        && reading.wall.is_none()
        && reading.prompt.is_none()
}

/// Whether a read at the restart's last look (`read`) SHOWS THE TURN OVER
/// where the step read past a status that only lags (`lagging`,
/// [`Restarting::lagging`]; round eight, #5): no busy footer
/// ([`busy_footer`]) and the session's own reader at an idle point
/// ([`reads_idle`]) — the turn facts that look's gate required
/// ([`upgrade::status_lags`]: idle looks in a row; the gate's `busy`).
/// Claude Code need not restamp a status it does not change
/// (`statusUpdatedAt` stood still for the whole of each wait measured,
/// docs/measured/claude-code-dialogs-2026-09-24.md), so a turn begun since
/// the look over a stale `busy` leaves the status exactly as the look read
/// it, and only the screen says it runs — a background task's end that woke
/// the agent, signalled mid-turn, and its turn's work dropped unsaid (the
/// quiet flow types nothing after the relaunch). Not lagging: Claude's own
/// `idle` is the turn fact, as before.
fn turn_over_shown(read: &Screen, lagging: bool) -> bool {
    !lagging || (!busy_footer(&read.rows) && reads_idle(read))
}

/// ONE read's word on the composer, no fence: no box up and the composer
/// empty (its `cell` asked after the read, as [`composer_empty`] does). For
/// a line typed UNFENCED — a continuation whose screen never held still
/// (`relaunch::type_continuation`) — where no generation is carried, so the
/// order of the reads buys nothing.
pub(super) fn composer_clear(c: &mut Client, tab: &str) -> bool {
    screen(c, tab).is_some_and(|scr| {
        aterm_phase::prompt::prompt_box_span(&scr.rows).is_none() && composer_empty(c, tab, &scr)
    })
}

/// [`typing_fence`]'s verdict over one read, `dim` the `cell` answer for its
/// caret row (`false`: not asked, or not dim).
fn fence_verdict(scr: &Screen, dim: bool) -> Result<Option<String>, &'static str> {
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Err("box");
    }
    if !upgrade::composer_is_empty(&scr.rows, scr.cursor, dim) {
        return Err("draft");
    }
    Ok(scr.generation.clone())
}

/// The absolute row whose column 2 [`composer_empty`] asks the `cell` of: the
/// composer's CARET row, wherever the cursor is, when a read can change the
/// answer ([`upgrade::composer_dim_row`]) — not the cursor's row, which a
/// grid displaced by a net-zero re-grid parts from the composer (2026-09-28:
/// the rule's glyph below it, not dim, read as a person's draft for 51
/// minutes).
fn dim_cell(scr: &Screen) -> Option<usize> {
    upgrade::composer_dim_row(&scr.rows, scr.cursor).and_then(|caret| scr.first.checked_add(caret))
}

/// Whether two reads show the same composer — the cursor at the same place,
/// and the same text on the composer's caret row — so a `cell` asked between
/// them judged the later one's caret row too. The rows above it (a spinner, a
/// timer) may have moved.
fn same_composer(a: &Screen, b: &Screen) -> bool {
    fn caret(s: &Screen) -> Option<&str> {
        aterm_phase::phase::composer_draft(&s.rows)
            .and_then(|(caret, _)| s.rows.get(caret))
            .map(String::as_str)
    }
    a.cursor == b.cursor && a.first == b.first && caret(a) == caret(b)
}

/// A LINE THE HARNESS TYPES INTO THE AGENT'S COMPOSER, UNDER THE HARNESS'S
/// OWN HAND and tried again when the screen moved: the notice, the release.
///
/// The hand ([`Hand`], a hard drive lease under the holder the supervisor
/// loops claim sessions under) is what the window's presence layer recognises
/// as aterm's own (ruling 313: aterm's own hand never shows a row). An
/// unleased `turn` is read as somebody driving the tab: a `Level::Driven` row
/// is born and, when the fence refuses the turn and its lease drops a
/// millisecond later, folded — two PTY resizes 0.75 ms apart, net zero, which
/// desynced the owner's tab for 51 minutes (2026-09-28 17:07:36, the harness's
/// own first eligible notice; 47 refused notices in 6 h had each done it).
/// Under the hand the tab shows no row, so no refusal re-grids the screen it
/// was judged on, and the fence is not invalidated by the row the turn itself
/// raised - BEST-EFFORT until the render-integrity stream's R02 and R15 (the
/// lease keeping its hand for the whole turn, the driver hold for the driver's
/// own window) land; they wait for the presence fold-quiet branch. A tab another driver holds (`Hand::take` refused) is a wait
/// (`held`); a server that takes no hard lease (older) holds nothing, and the
/// line is typed as before.
///
/// Each try is a fresh read ([`claude_fence`]) and a fenced turn
/// ([`turn_fenced`], [`notice_fenced`]: `put` says whether the words are pasted whole or pressed
/// key by key, [`Put`]). With a `retry` (the notice: the look's stamp and gates),
/// only the fence's own `changed` is tried again, up to [`NOTICE_FENCE_TRIES`]
/// times [`NOTICE_FENCE_EVERY`] apart, and a try after the first types only
/// where [`retry_open`] finds the read it is fenced on still what the look
/// required; a box, a draft or a person's typing is the read's answer, and a
/// yield that timed out means a person is typing. Without one (the release,
/// which never retried) it is one try under the hand. `Err` is the last try's
/// verdict, whose [`Typed::word`] is the report word; a [`Typed::Cut`] is the
/// caller's to record ([`left_cut`]).
pub(super) fn type_fenced(
    c: &mut Client,
    tab: &str,
    text: &str,
    retry: Option<&Retry>,
    put: Put,
) -> Result<(), Typed> {
    let mut hand = Hand::take(c, tab).ok_or_else(|| Typed::Refused("held".to_string()))?;
    let out = (|| {
        let mut tries = 0;
        loop {
            tries += 1;
            let again = retry.is_some() && tries < NOTICE_FENCE_TRIES;
            let (generation, read) = match claude_fence(c, tab) {
                Ok(fenced) => fenced,
                Err("changed") if again => {
                    std::thread::sleep(NOTICE_FENCE_EVERY);
                    continue;
                }
                Err(why) => return Err(Typed::Refused(why.to_string())),
            };
            if tries > 1 && !retry.is_some_and(|retry| retry_open(&read, retry)) {
                return Err(Typed::Changed);
            }
            // A ONE-KEY CARD OVER THE COMPOSER IS THE PERSON'S (measured
            // 2026-10-01: the session survey reads the composer's value, so
            // no key of a pressed text answers it — but its first key closes
            // the card unanswered, as a person's first key would). A pressed
            // notice or release is not typed while one is open: refused
            // `survey`, tried again at the next quiet moment, the card left
            // to the person (`aterm_phase::survey_block`).
            if put == Put::Pressed && aterm_phase::survey::survey_block(&read.rows).is_some() {
                return Err(Typed::Refused("survey".to_string()));
            }
            let typed = match put {
                Put::Paste => turn_fenced(c, tab, text, generation.as_deref()),
                Put::Pressed => notice_fenced(c, tab, text, generation.as_deref()),
            };
            match typed {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) if again => std::thread::sleep(NOTICE_FENCE_EVERY),
                Err(e) => return Err(e),
            }
        }
    })();
    hand.give_back(c);
    out
}

/// What one [`restart`] is made of beyond the session's own facts: the
/// launch's argv, the build it moves to, the tab's claim, the person's grace
/// at the signal ([`upgrade::person_grace`] of the rung the upgrade stands
/// at: `[harness] human_grace_s` until KeysOnly, then
/// [`upgrade::KEYS_GAP_S`] — and KEYS_GAP_S under the owner's `--now`, the
/// last rung; never nothing, as `--now` was until 2026-09-28), whether it starts the
/// agent AFRESH — a conversation with no task ([`Step::Fresh`]): no notice was
/// typed for it to own, nothing is resumed, and nothing is typed after — and
/// the permission mode the visit's look read off the tab's pill
/// ([`screen_mode`]; `None`: no pill was drawn), and the model a person chose
/// by hand ([`hand_model_in`]), which the line asks for when the list moves
/// the conversation to none.
struct Restarting<'a> {
    argv: &'a [String],
    target: &'a Candidate,
    host_claim: Option<&'a HostClaim>,
    grace: u32,
    fresh: bool,
    /// A quiet restart ([`upgrade::Facts::quiet`]): no notice was typed, so
    /// none is the process's to own — the process looked at is the one ended.
    quiet: bool,
    /// The step read past A STATUS THAT ONLY LAGS on the owner's `--now`
    /// under the quiet flow ([`upgrade::quiet_step`], round eight, #5): the
    /// last look takes that same status, unrewritten since the look, for
    /// Claude's own `idle` — and only on a screen that still shows the turn
    /// over ([`turn_over_shown`]), at the last look and at the re-read
    /// before the signal: a turn begun since over a stale `busy` need not
    /// rewrite it. `false`: only `idle` lets the signal go.
    lagging: bool,
    mode: Option<&'a str>,
    model: Option<&'a str>,
    /// The login the process runs under, as its exec environment names it
    /// (the raw `CLAUDE_SECURESTORAGE_CONFIG_DIR`; `None`: unset): the
    /// upgrade moves a session onto a newer build, never onto another
    /// account, so its line re-applies exactly this
    /// ([`super::relaunch::Launch::login`]).
    login: Option<&'a str>,
}

/// Whether Claude's own status, read again at the restart's last look (`a`),
/// lets the signal go: its `idle` — or, where the step read past a status
/// that only lags on the owner's word (`lagging`, [`Restarting::lagging`];
/// round eight, #5), the very status the visit's look read (`looked`),
/// unrewritten since. A turn begun since may leave it as it was (a stale
/// `busy` it need not restamp), so where a lag is read past the screen read
/// beside it must show the turn over too ([`turn_over_shown`]).
fn status_lets_signal(a: &SessionFile, looked: &SessionFile, lagging: bool) -> bool {
    a.status == "idle"
        || (lagging
            && a.status == looked.status
            && a.status_updated_at_ms == looked.status_updated_at_ms)
}

#[allow(clippy::too_many_arguments)]
fn restart(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    t: &[(u32, u32, String)],
    k: &dyn Kernel,
    how: &Restarting<'_>,
) -> Report {
    let Restarting {
        argv,
        target,
        host_claim,
        grace,
        fresh,
        quiet,
        lagging,
        mode: shown_mode,
        model: hand_model,
        login,
    } = *how;
    let status_holds = |a: &SessionFile| status_lets_signal(a, sf, lagging);
    // The notice this restart answers is the process's own — or, afresh,
    // there is none, and the conversation must still have no task — or,
    // quietly, there is none either, and the process is the one the visit
    // looked at, in its tab.
    let ours = |st: &St, a: &SessionFile| {
        if fresh {
            tasked(&opts.home, &sf.session_id, &supervisor_typed(opts, tab)) == Some(false)
        } else if quiet {
            st.tab == tab && a.pid == sf.pid && squash(&a.proc_start) == squash(&sf.proc_start)
        } else {
            st.notice_belongs_to(a, tab)
        }
    };
    if !ours(st, sf) {
        return said(
            r,
            if fresh {
                "wait:tasked"
            } else {
                "wait:notice-owned-by-other-process"
            },
        );
    }
    if is_our_ancestor(sf.pid, t) {
        return said(r, "wait:self");
    }
    // Everything the relaunch needs is read before the agent is ended, first
    // that the parent is a job-control shell holding the agent as its
    // foreground job ([`Job`]): only then does waiting for the parent's `pgid
    // == tpgid` mean its prompt is back. The line is planned again against the
    // shell just proven, since hooks or directories may have changed since
    // the announcement.
    let proven = match k.job(sf.pid) {
        Some((Job::NoJobControl, _)) => return not_a_job(opts, r, st),
        job => match foreground_shell(job) {
            Ok(shell) => shell,
            Err(why) => return said(r, format!("wait:{why}")),
        },
    };
    // The model the list moves it to, else a model a person chose by hand
    // since the launch: its `--model` would undo that `/model`.
    let model = recorded(&st.model_list).or(hand_model);
    let mode = resume_mode(opts, &sf.session_id, shown_mode.map(str::to_owned));
    let launch = launch_of(sf, argv, target, model, mode.as_deref(), !fresh, login);
    let Plan { shell, line, .. } = match plan(opts, &launch, proven, t) {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    // The harness's hand on the tab ([`Hand`]): taken before the last look, so
    // from here to the relaunched agent's first idle no other connection's
    // write lands in the dying agent or the bare shell. Given back on every
    // exit before the signal; the relaunch renews it and returns it after the
    // new agent's first idle ([`super::relaunch::relaunch`]).
    let Some(mut hand) = Hand::take(c, tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        // The last look before the irreversible act: same process, still idle,
        // still its shell's foreground job, composer empty, nothing running
        // under it, no hand and no unread input (a person's within `grace`,
        // never zero).
        let again = session_file_of(&opts.home, sf.pid);
        let scr = screen(c, tab);
        let still = again.is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(&a)
                && ours(st, &a)
                && kernel_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(sf.pid)) == Ok(shell)
            && scr.is_some_and(|s| turn_over_shown(&s, lagging) && composer_empty(c, tab, &s))
            && live_background(sf.pid).is_empty()
            && !held(c, tab, grace)
            && host_claim.is_none_or(|claim| claim_still_live(c, sf.pid, claim));
        if !still {
            break 'signal Some(said(r.clone(), "wait:changed"));
        }
        if let Err(why) = require_unique_owner(&opts.home, sf) {
            break 'signal Some(said(r.clone(), format!("wait:{why}")));
        }
        let Ok(pid) = i32::try_from(sf.pid) else {
            break 'signal Some(said(r.clone(), "wait:pid"));
        };
        // A restart carries the agent on: the READY it acts on and any release
        // an earlier notice owed are taken off the record with the exit
        // ([`St::signalled`]), and put back if the signal is never sent.
        let back = st.signalled(sf.pid, shell, tab, line, now_s());
        // The login that line carries, beside it ([`St::login`]).
        st.login = login.map(str::to_owned);
        // Afresh: the new process holds a conversation of its own, and
        // nothing is carried on (`relaunch::await_new`).
        if fresh {
            super::relaunch::CAUSE_UPGRADE_FRESH.clone_into(&mut st.cause);
        }
        // The fallback origin this restart carries ([`fallback_carried`]):
        // put back with the rest if the signal is never sent.
        st.fallback_from =
            fallback_carried(fallback_origin(st), recorded(&st.model_list), hand_model);
        save(opts, &sf.session_id, st);
        // A second owner, a job-control change or a moved terminal during the
        // state write must veto the signal: re-read the session file and
        // kernel claims right before SIGTERM.
        let current = session_file_of(&opts.home, sf.pid);
        let owns_notice = current.as_ref().is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(a)
                && ours(st, a)
                && kernel_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        });
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        // The composer, hands and input are read again, last, on the same
        // connection: a draft begun or a prompt sent during the state write is
        // seen before the irreversible act (which would drop unread input).
        if !owns_notice
            || require_unique_owner(&opts.home, sf).is_err()
            || foreground_shell(k.job(sf.pid)) != Ok(shell)
            || !owner.is_some_and(owned_by_aterm)
            || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
            || !typing_fence_read(c, tab).is_ok_and(|(_, read)| turn_over_shown(&read, lagging))
            || held(c, tab, grace)
        {
            st.unsent(back);
            st.cause.clear();
            save(opts, &sf.session_id, st);
            break 'signal Some(said(r.clone(), "wait:changed-before-signal"));
        }
        // The model the relaunch asks for (`st.model_list`) is recorded as
        // asked by the relaunch line that asks it (`relaunch::relaunch`), not
        // here: a signal never sent asks nothing.
        match terminate(c, tab, pid, grace) {
            // The agent is ended: its carry-on, not a release, tells it to go
            // on.
            Terminated::Sent => None,
            Terminated::Refused(why) => {
                st.unsent(back);
                st.cause.clear();
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), format!("wait:signal-{why}")))
            }
            Terminated::Failed => {
                // The agent answered READY and lives on, stopped: owed its
                // release, and its READY no longer acted on.
                st.signal_failed(back, now_s());
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), "failed:signal-refused"))
            }
        }
    };
    if let Some(r) = aborted {
        hand.give_back(c);
        return r;
    }
    ledger(
        opts,
        &said(r.clone(), "terminated"),
        if fresh {
            "SIGTERM (no task: restarted afresh, nothing typed or resumed)"
        } else if quiet {
            "SIGTERM (idle: nothing asked or typed; resumed as it was)"
        } else {
            "SIGTERM"
        },
    );
    relaunch(opts, r, st, c, &sf.session_id, k)
}

/// THE ASK OF A RELAUNCH LINE THAT WAS NOT TYPED, taken back from the
/// conversation's model record ([`ModelRecord::take_back`]; 2026-10-01): no
/// relaunch asked it, so it must not settle as a model that failed.
pub(super) fn take_back_ask(opts: &Opts, session: &str, asked: Option<models::Asked>) {
    if let Some(asked) = asked {
        let mut rec = load_model_record(opts, session);
        rec.take_back(asked);
        save_model_record(opts, session, &rec);
    }
}

/// THE CONFIRMATIONS a carry-on left pending ([`confirm`]): each restart whose
/// continuation was typed before its resumed session had answered is read
/// again, and says its `done` row once the answer is written or its time is
/// up. Only files are read and written — no tab is asked or typed into — so
/// every sweep runs it first, whatever else it finds; a hand-run sweep for one
/// tab (`only_sid`) confirms only that tab's.
fn confirmations(opts: &Opts) -> Vec<Report> {
    if opts.dry_run {
        return Vec::new();
    }
    let Ok(dir) = std::fs::read_dir(state_dir(opts)) else {
        return Vec::new();
    };
    let now = now_s();
    let mut out = Vec::new();
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(mut st) = load(opts, &session).filter(St::confirming) else {
            continue;
        };
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let r = Report {
            pid: st.resumed_pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.resumed_on, st.source),
            step: String::new(),
        };
        out.extend(confirm(opts, &r, &mut st, &session, now));
    }
    out
}

// The upgrade tests drive real processes through Unix APIs (process groups, modes, inodes).
#[cfg(all(test, unix))]
#[path = "upgrade_drive_tests.rs"]
mod tests;
