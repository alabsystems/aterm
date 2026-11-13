// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Harness agent upgrade driver (`aterm agent upgrade`, live relaunch, and transcript inspection).
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use aterm_json::{Map, Value};
use aterm_types::control_verbs::ScreenGen;

use super::agent_image::{self, Lane};
use super::relaunch::{
    Launch, NoPlan, Plan, PromptLook, PromptMark, Relaunch, RelaunchLineError, STALE_S, TAB_TAKEN,
    WAIT_HAND_LIFTS, await_new, carry_on, confirm, expired, hand_waits_out, launched, overtaken_by,
    overtaken_line, pause, plan, recorded, relaunch, relaunch_of, resume_mode, screen_mode,
    tab_front, type_relaunch_line_with, unplanned, wait_now, wait_until,
};
use super::upgrade::{self, Candidate, Facts, Phase, Request, SessionFile, Source, Step, Version};
use crate::RelayClient;
use crate::supervise::approvals;
use crate::supervise::policy::git_config::claude_dir_at;
use crate::supervise::run::composer_guard;
use crate::supervise::screen::HumanInput;

#[path = "upgrade_status.rs"]
mod upgrade_status;
pub(super) use upgrade_status::{last_command, wait_words_of};

#[path = "codex_alab_drive.rs"]
mod codex_alab_drive;

#[path = "upgrade_muse_drive.rs"]
mod muse;
pub(crate) use self::parse_exit_status as shell_exit_status;
pub(super) use muse::carry_in_flight as muse_carry_in_flight;

#[path = "upgrade_agy_drive.rs"]
mod agy;
pub(super) use agy::carry_in_flight as agy_carry_in_flight;

#[path = "upgrade_watch.rs"]
mod watch;

#[path = "upgrade_build_start.rs"]
mod build_start;
pub use build_start::BUILD_FAILS;
/// Run the actual managed start probe in an isolated native qualification world.
#[cfg(all(any(test, debug_assertions), unix))]
pub fn first_exec_probe_for_test(
    exe: &std::path::Path,
    limit: std::time::Duration,
    holds: &std::path::Path,
) -> &'static str {
    build_start::probe_within(exe, limit, Some(holds)).word()
}
#[cfg(all(test, unix))]
pub(crate) use upgrade_status::pin_awake_since;
#[cfg(any(test, debug_assertions))]
pub use upgrade_status::progress_projection;
pub(super) use upgrade_status::runs_under;
pub use upgrade_status::{
    AGENT_OFF, ATTENTION_OWNER, AUTOPILOT_OFF, Aside, Ask, AskScope, Attention, CLAIMED,
    DIGEST_AFTER_S, HUNG_EXIT_AFTER_S, HeldBy, LIMIT_GRACE_S, Level, MOVE_BUDGET_S, NEED_THEN_MAX,
    NEED_WHY_MAX, NOT_FOREGROUND, NOW_QUIETS_S, Need, NeedSeverity, PRESS_CLAIMED, REFUSED_STOPPED,
    Remedy, Row, STALLED_AFTER_S, SUSPENDED_AFTER_S, TELL_S, UNWATCHED_AFTER_S, View,
    WAKE_CATCH_UP_S, WAKE_GRACE_S, ask, ask_for, ask_for_scope, awake_since, elapsed_words,
    refusal_sentence, refused_stopped, rows, status_rows, system_awake_since, tab_row, tab_rows,
    unwatched_since, word_marker,
};
#[cfg(all(test, unix))]
use watch::watch_with;
pub use watch::{Watched, Watcher, wall_due, watch};

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnStarts",
        reason = "A turn of the session's own starting: Claude Code's step. The Tier-1 bind reads it busy exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "TurnEnds",
        reason = "A turn of the session's own ending: the agent's step. The Tier-1 bind reads its end exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Repaint",
        reason = "A redraw with no new words (a footer clock): the vendor's screen. The Tier-1 bind repaints the clock exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Still",
        reason = "The screen's own quiet standing: the vendor's screen and the clock. The Tier-1 bind holds the screen still exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "Type",
        reason = "A person's keystroke in the tab: a person's step. The Tier-1 bind stamps the keystroke exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysPause",
        reason = "A person pausing past the keys gap but still near: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "KeysAway",
        reason = "A person leaving the tab past the grace: a person's step and the clock. The Tier-1 bind ages the stamp exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftTyped",
        reason = "A draft left in the composer: a person's step. The Tier-1 bind types one exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "DraftCleared",
        reason = "That draft cleared: a person's step. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxUp",
        reason = "A dialog drawn over the composer: the vendor's step. The Tier-1 bind draws a measured box exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "BoxDown",
        reason = "That dialog answered: the vendor's step. The Tier-1 bind takes it down exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "SeqSettle",
        reason = "Buggy=1 negative control only; a look at a pause the ladder counts that waits `settling` for a screen that only repainted (the incident's 120 `wait:settling`), which `St::still` does not"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "AttendedWithoutLadder",
        reason = "Buggy=1 negative control only; a person merely near the tab holding the move at every rung, where `gate` narrows the grace to the keys gap from KeysOnly"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "NowWaivesThePerson",
        reason = "Buggy=1 negative control only; the owner's `--now` waiving the person outright, where `gate` keeps the keys gap at the last rung"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverADraft",
        reason = "Buggy=1 negative control only; a move typed over a draft, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveOverABox",
        reason = "Buggy=1 negative control only; a move typed over a dialog, which `gate` never makes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeLadder",
        action = "MoveMidTurn",
        reason = "Buggy=1 negative control only; a session ended while its own turn runs, which `gate` holds"
    )
)]
#[doc(hidden)]
pub fn ladder_environment() {}

/// The ladder's projection: the model variables a look's real [`Facts`] stand for.
#[cfg(any(test, debug_assertions))]
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
        ("work", flag(f.status != "idle" || f.busy_footer)),
        ("quiet", flag(f.status_age_s >= quiet && f.quiet_s >= quiet)),
        ("seen", if f.still_s >= quiet { 2 } else { 0 }),
        ("keys", if f.typing { 2 } else { flag(f.attended) }),
        ("draft", flag(!f.composer_empty)),
        ("dialog", flag(f.approval_box)),
    ])
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "TurnRuns",
        reason = "A turn of the agent's own starting: Claude Code's step (the loop's `NotIdle`/`NotBusy`, no point offered). The Tier-1 binds read it as a busy status and footer (the watch's read) exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "BreakSettles",
        reason = "A settled break of the agent's own background work: the agent's and its supervisor loop's (`host_steps_in_background`). The Tier-1 binds offer the break step (`background_point`) there, and read the screen as the watch's dry read does."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "IdlePoint",
        reason = "An idle point: the agent's turn ending and the loop offering it (`host_steps_here`). The Tier-1 binds read an idle, settled screen exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WallStands",
        reason = "A usage limit's notice on the screen: Claude Code's and the API's step. The Tier-1 binds read `Facts::limited` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PersonHolds",
        reason = "A person's box or draft standing: a person's step (the loop's `Prompt` guard). The Tier-1 binds read an approval box exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "ResetPasses",
        reason = "The wall's named reset passing, its notice still on the screen: the clock's and the vendor's step, which the record keeps as the transcript names it (`St::limit_until`) and the view only READS (`Row::limit_passed`, `LIMIT_GRACE_S`). The Tier-1 view bind builds it through `limit_until`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkStarts",
        reason = "A process starting under the agent: the agent's step. The Tier-1 reducer bind names it in `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WorkEnds",
        reason = "That process ending: the agent's step. The Tier-1 reducer bind empties `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Answers",
        reason = "The agent's READY to the round's notice: the agent's step (`heard`). The Tier-1 reducer bind hands the reducer `ready` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Window",
        reason = "The round's clock running out: the clock's step (`REASK_S` since the notice or the pending round's progress, `RETRY_S` since the stop), which the reducer and the view only READ. The Tier-1 binds build it through `at_s`, `failed_s`/`retry_at` and `progress_at`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "NoLookWithoutAPoint",
        reason = "Buggy=1 negative control only; main before `136ff0735`, whose view named no `watch_at` instant, so nothing looked at a record off a point. The host's wake at `watch_at` is bound by the existing T1-h tests."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RecordedWordStands",
        reason = "Buggy=1 negative control only; main until 2026-09-29, whose watch recorded a word only for a read that waits, so a read that would act left the last word. The real read now says what holds the point of a give-up or a re-arm (`upgrade::held_point`) and the watch records it, else `upgrade::NEXT_POINT` (`upgrade_watch::look_word`), for a Claude Code record: a Codex record's act read records nothing still, and the settled-break half is a re-decision of the law's truth, not a change of code. The Tier-1 binds replay the old look against the real read, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "WatchTypes",
        reason = "Buggy=1 negative control only; the watch with hands, which the watch's read-only connections refuse before a byte is written (`a_dry_runs_connection_reads_and_has_no_hands`) and the Tier-1 watch tests refuse at the control socket (zero input requests)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RestartOverWork",
        reason = "Buggy=1 negative control only; the restart over processes under the agent, where the real `gate_restart` waits `background`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "Press",
        reason = "The owner's press of the top rule's ⬆️: the window's step (`upgrade_status::Ask::Press`, a Now that stamps `pressed_at` for the build it was shown). The Tier-1 binds build the standing press through `Facts::press` and the row's `pressed_at`/`pressed_to`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PressLapses",
        reason = "The press's span running out (`upgrade::PRESS_S`), its Now cleared, or its build retargeted: the clock's and the record's step, which the reducer and the view only READ (`upgrade_status::press_stands`)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "OwnerAsks",
        reason = "The owner's Now from the tab menu, the upgrade's message or `aterm harness upgrade --now` (`upgrade_status::Ask::Now`): the owner's step. The Tier-1 binds hand the reducer `Request::Now` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "NowAsPress",
        reason = "Buggy=1 negative control only; the owner's Now acting as a press, restarting over the agent's background work, where the real `requested_step` with no press standing waits `background`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PressPassesInput",
        reason = "Buggy=1 negative control only; a press restarting over a held tab, a box or a draft, where the real `press_gate` waits `held`, `box` or `draft`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "PressReadAsNow",
        reason = "Buggy=1 negative control only; a standing press read as the plain Now it writes, so its lane waits for background work and the watch records the Now's word. The Tier-1 binds replay that read against the real one, and aterm-spec's liveness sweep breaks `APressMovesPastRunningWork` with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "LookClockPasses",
        reason = "Time passing a finished restart's look: its window (`relaunch::RESTORE_LOOK`), then the bound a warned look's line is typed within (`relaunch::RESTORE_TYPE_WITHIN_S`). The Tier-1 binds set `now` against `confirm_by` exactly so."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "ResumedBuildWarns",
        reason = "Claude Code's own row in the conversation: the resumed build warning that something did not come back (`upgrade::resumed_build_warned`). The Tier-1 binds read it as the `warned` of `relaunch::look_at`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "NewerBuildLands",
        reason = "A newer build installed behind a finished move: atpkg's step. The Tier-1 binds read it as a target newer than the record's."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "OnlyIdleClosesALook",
        reason = "Buggy=1 negative control only; the lane before the look had a clock of its own, where only an idle point closed it (`St::confirming` alone held the visit and the owed continuation), so a point past the look's clock waits `confirming` behind it instead of taking the newer build. The Tier-1 binds replay that read against the real `relaunch::look_holds`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "LineTypedAtBreak",
        reason = "Buggy=1 negative control only; a step that types the look's line wherever the agent rests, a break of its own background work included, where the real `relaunch::look_types` refuses a background step. The Tier-1 binds replay that read against the real one at both points."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RestsAtABreak",
        reason = "The agent of a finished move comes to rest at a break of its own background work: the agent's step, read by the loop as the point it offers (`Opts::background`). The Tier-1 binds read it as the `background` of `relaunch::look_types`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeProgress",
        action = "RestsIdle",
        reason = "The agent of a finished move comes to rest idle at its prompt: the agent's step, read by the loop as an idle point. The Tier-1 binds read it as a step that is not `background`."
    )
)]
#[doc(hidden)]
pub fn progress_environment() {}

/// The quiet round's projection ([`upgrade::tell_due`], [`upgrade::quiet_move`],
/// [`upgrade::own_yields`]): the `HarnessUpgradeQuiet` variables a look's real [`Facts`] stand
/// for.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn quiet_projection(f: &Facts) -> std::collections::BTreeMap<&'static str, i64> {
    let flag = |b: bool| i64::from(b);
    // A status that only lags past a turn end ([`upgrade::status_lags`]) is the agent's own
    // in-process work keeping it busy (a workflow), at an idle screen; the owner's `--now` reads
    // it as none.
    let lags = upgrade::status_lags(f);
    let posture = if f.limited || f.login {
        3
    } else if f.typing || f.held || upgrade::person_hold(f).is_some() {
        4
    } else if f.background_point {
        1
    } else if f.status == "idle" || lags {
        2
    } else {
        0
    };
    std::collections::BTreeMap::from([
        ("posture", posture),
        (
            "work",
            if f.waits_only {
                1
            } else if !f.background.is_empty() {
                // Follows alone end losing nothing that stands; any other process is real work.
                if f.follows_only { 3 } else { 2 }
            } else if lags && !f.owner_now {
                // The status alone names it, no process: a finished session moves over it as it
                // does over follows (`upgrade::Yield::Outlasted`).
                3
            } else {
                0
            },
        ),
        ("near", flag(f.attended)),
        ("held", flag(f.own_hold_s >= upgrade::WORK_TELL_S)),
        // The line is the process's while its deadline stands: one it was told about an earlier
        // target counts, and one a look voided does not (`upgrade::deadline_voids`).
        ("tells", i64::from(f.tells).max(flag(f.deadline_set))),
        ("due", flag(f.work_due)),
        // No autopilot carries the session at this look: a person's hold on the carry. A
        // manager's wait is none (the window's `upgrade_tell` never reads it as one).
        ("off", flag(!f.may_tell)),
        ("silent", flag(f.silent)),
        ("outlasted", flag(f.own_hold_s >= upgrade::SILENT_HOLD_S)),
        // The wall has an age, from a row of its transcript ([`wall_age`]); it has aged where
        // each of its parts has stood its bound with nobody seen (`upgrade::login_holds`,
        // `upgrade::limit_holds`). One nothing dates reads no age and never ages.
        (
            "dated",
            flag((f.login && f.login_s > 0) || (f.limited && f.limit_s > 0)),
        ),
        ("aged", flag(upgrade::walls_outlasted(f))),
        ("moved", 0),
    ])
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TurnRuns",
        reason = "A turn of the agent's own running: Claude Code's step. The Tier-1 binds read it as a busy status and footer off any break, where the quiet round neither moves nor tells (`gate_announce` waits `not-idle`)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "BreakSettles",
        reason = "A settled break of the agent's own background work: the agent's and its supervisor loop's (`host_steps_in_background`, `IdleHost::at_background_on`). The Tier-1 binds offer the break step (`background_point`, `break_step`) there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "IdlePoint",
        reason = "An idle point: the agent's turn ending and the loop offering it (`host_steps_here`). The Tier-1 binds read an idle, settled screen exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WallStands",
        reason = "A usage limit or the login wall on the screen: the API's and Claude Code's step. The Tier-1 binds read `Facts::limited` exactly there, which `quiet_step` waits on first."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "PersonHolds",
        reason = "A person's box, draft, keys or hold: a person's step. The Tier-1 binds read an approval box exactly there, which `gate_announce` and `quiet_move` wait on."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "PersonNear",
        reason = "A person's key within their grace: a person's step. The Tier-1 binds read `Facts::attended` exactly there, which `upgrade::person_seen` holds the tell on and the land rung's move does not."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "PersonLeaves",
        reason = "That grace running out: the clock's step. The Tier-1 binds clear `Facts::attended` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WaitStarts",
        reason = "A background wait starting under the agent with nothing else: the agent's step. The Tier-1 binds name its shell in `Facts::background` and set `Facts::waits_only` exactly there, as the drive does where `upgrade::own_of` proves every shell a wait."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "RealWorkStarts",
        reason = "Work starting under the agent (a process `upgrade::own_of` does not prove a wait and `upgrade::follows_only` does not read as follows, work its screen names): the agent's step. The Tier-1 binds name it in `Facts::background` with `Facts::waits_only` and `Facts::follows_only` clear exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "FollowStarts",
        reason = "A follow starting under the agent (a tail that follows a file or a development server, `upgrade::follow_command`, beside nothing but waits: `upgrade::follows_only`), or a status the agent's in-process work keeps busy past its turn: the agent's step. The Tier-1 binds name the follow in `Facts::background` with `Facts::follows_only` set, or leave `Facts::background` empty under a lagging status, exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WorkEnds",
        reason = "What ran under the agent ending: the agent's step. The Tier-1 binds empty `Facts::background` exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "HoldOutlasts",
        reason = "The agent's real work holding the round past `upgrade::WORK_TELL_S`: the clock's step over `St::own_since`, which `St::note_wait` starts at the first own-work wait. The Tier-1 binds build it through `Facts::own_hold_s`, and the drive's hold-clock test binds the record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "HoldBreaks",
        reason = "A look that finds a person or a wall: the drive clears `St::own_since` where `upgrade::person_seen` holds, and `St::note_wait` on a limit's wait. The Tier-1 binds build it through `Facts::own_hold_s`, and the drive's hold-clock tests bind the record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "DeadlinePasses",
        reason = "The deadline a tell named passing: the clock's step (`upgrade::WORK_DEADLINE_S` past the tell's spend, `St::deadline_at`, for the process `St::deadline_pid` names). The Tier-1 binds build it through `Facts::work_due`, and the drive's deadline test binds the record."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellOffAPoint",
        reason = "Buggy=1 negative control only; a tell typed while a turn runs or a wall stands, which `quiet_step` (the wall) and `gate_announce` (a turn off a break) refuse. The Tier-1 bind's control reads a running turn as a break and drops the wall."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellOverAPerson",
        reason = "Buggy=1 negative control only; a tell typed over a person's box, draft, keys or a key within their grace, which `upgrade::person_seen` refuses. The Tier-1 bind's control hides the person from the reducer."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellsEarly",
        reason = "Buggy=1 negative control only; a tell before the agent's work held the round `upgrade::WORK_TELL_S`. The Tier-1 bind's control drops that bound."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellsPastTheCap",
        reason = "Buggy=1 negative control only; a second line to a process whose deadline stands (`upgrade::MAX_TELLS`, `Facts::deadline_set`). The Tier-1 bind's control drops both."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "MoveOverWork",
        reason = "Buggy=1 negative control only; the move over work under the agent before any deadline it was told has passed, where `quiet_move` waits `background`. The Tier-1 bind's control clears `Facts::background`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "MoveAtABreak",
        reason = "Buggy=1 negative control only; the move at a break of the agent's background work that is no proved wait, before any deadline, where `quiet_step` waits `background`. The Tier-1 bind's control reads the break as an idle point."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WaitsDropped",
        reason = "Buggy=1 negative control only; the move over the agent's waits with their commands left behind. The Tier-1 drive bind (`a_round_over_waits_alone_moves_at_a_break_and_carries_their_commands`) reads each command in the continuation typed after the resume, and its control empties `St::ended_waits` before that line."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "DeadlineUntold",
        reason = "Buggy=1 negative control only; a deadline in force that no line named. The drive stamps `St::deadline_at` only for a line the host took whole (`tell`), and `Facts::work_due` reads only that stamp; the Tier-1 drive bind's control sets the fact with no stamp."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellNeverTyped",
        reason = "Buggy=1 negative control only; a round that owes its tell and never types it. The Tier-1 drive bind (`a_quiet_round_held_by_its_own_work_is_told_its_deadline_once`) types it, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "OwnWorkHolds",
        reason = "Buggy=1 negative control only; the agent's own work holding the round with no bound, as it did before `upgrade::own_yields`. The Tier-1 bind's control clears `Facts::waits_only` and `Facts::work_due`, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "CarryHeld",
        reason = "A person's hold on the carry, the environment's: the tab's Autopilot switched off, a person's grace or another driver's hand, under Land the person's own exchange. The window reads it into `Tell::Off` (`upgrade_tell`) and the Tier-1 bind projects it as `Facts::may_tell` clear."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "CarryBack",
        reason = "The person's hold on the carry lifted, the environment's; the Tier-1 bind projects it as `Facts::may_tell` set."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "ManagerWaits",
        reason = "A manager awaiting the worker's task, the supervise loop's (`Guard::Managed`, standing for the loop's life). The window's bind (`the_quiet_tell_follows_the_tabs_autopilot_its_guards_and_its_restart_offer`) holds `upgrade_tell` to a tell under both autopilots there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "ManagerHoldsCarry",
        reason = "Buggy=1 negative control only; a manager's wait read as a person's hold on the carry, as the window read it under Land before. The window's bind fails on that mapping, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TaskLands",
        reason = "The session's task finishing or landing, the supervise loop's (`IdlePoint::done_under_land`, `FinishedScan`). The window reads it into `Tell::Silent` and the Tier-1 bind projects it as `Facts::silent`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TaskAsked",
        reason = "Someone else's turn reopening a finished task, the supervise loop's; the Tier-1 bind projects it as `Facts::silent` clear."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "SilentHoldOutlasts",
        reason = "The clock's step over `St::own_since` past `upgrade::SILENT_HOLD_S`, the hold `HoldOutlasts` starts; the Tier-1 bind projects it as `Facts::own_hold_s` at that bound."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "SilentWorkHolds",
        reason = "Buggy=1 negative control only; a finished or landed session's own work holding the round with no bound, as it did before `upgrade::Yield::Outlasted`. The Tier-1 bind's control shortens `Facts::own_hold_s` below the bound, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "SilentMovesEarly",
        reason = "Buggy=1 negative control only; the move over a finished session's work before its hold outlasted the bound. The Tier-1 bind asserts the real step never moves where it is enabled."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "TellsASilentSession",
        reason = "Buggy=1 negative control only; a line typed into a session whose task finished or landed. The Tier-1 bind's control clears `Facts::silent` and the real reducer tells."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "DatedWallStands",
        reason = "Claude Code's sign-in wall or usage limit on the screen, read by `aterm_phase` into `Facts::login` or `Facts::limited`; the Tier-1 bind projects it as a wall its transcript dates, within its bound."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WallAges",
        reason = "The clock's step past the wall's bound (`upgrade::LOGIN_WALL_S`, `upgrade::LIMIT_WALL_S`) from its first transcript row (`upgrade::LoginScan::since`, `upgrade::StandingScan::since`), restarted by a key of the person's after it and read as 0 at a look that sees a person (`upgrade_drive::wall_age`); the Tier-1 bind projects it as `Facts::login_s` or `Facts::limit_s` at that bound."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "MovesAtALiveWall",
        reason = "Buggy=1 negative control only; the move at a wall still within its bound. The Tier-1 bind's control ages `Facts::login_s` or `Facts::limit_s` past it and the real reducer moves."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "HarnessUpgradeQuiet",
        action = "WallHoldsForGood",
        reason = "Buggy=1 negative control only; a wall nobody lifts holding an idle round with no bound, as a login wall did before `upgrade::LOGIN_WALL_S` and a usage limit before `upgrade::LIMIT_WALL_S`. The Tier-1 bind's control keeps the wall's age below its bound, and aterm-spec's liveness sweep breaks the law with it."
    )
)]
#[doc(hidden)]
pub fn quiet_environment() {}

/// What one sweep is told.
#[derive(Clone, Debug)]
pub struct Opts {
    pub home: PathBuf,
    pub state: PathBuf,
    /// The trusted local account namespace used when constructing or executing Claude commands.
    pub account_roster: Option<PathBuf>,
    pub sock: Option<String>,
    /// The window harness's own token, presented on [`Self::sock`] alone ([`harness_client`]):
    /// a window's sweep passes it, so what the drive types reads `by=harness`; the CLI's is
    /// `None`, the token beside the socket.
    pub token: Option<HarnessToken>,
    pub only_sid: Option<String>,
    pub dry_run: bool,
    pub human_grace_s: u32,
    pub hand_back: bool,
    pub background: bool,
    pub aterm_state: Option<PathBuf>,
    pub ask: bool,
    pub usage_tap: Option<super::tap::Armed>,
    pub instances: Instances,
    pub agents_off: Vec<aterm_phase::Program>,
    pub land: bool,
    pub timings: super::Timings,
    /// Whether a quiet round may tell the agent what waits ([`upgrade::Step::Tell`]).
    pub tell: Tell,
    /// The sweep is the lane of the owner's press of the ⬆️ ([`press`]): `None` for every other.
    pub press: Option<PressOpts>,
}

/// What a quiet round may type to an agent whose own work holds its move ([`Opts::tell`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tell {
    /// Nothing: no autopilot carries the session at this look (the tab's Autopilot off, a guard
    /// of its loop's holding the point, a lane that reads neither), so a person reads what is
    /// typed. The agent's own work holds the round there, and nothing moves over it.
    #[default]
    Off,
    /// No line, under an autopilot that carries the session: its task finished or landed, and
    /// nothing is typed into it before a move. The round still moves over proved waits, past a
    /// deadline the process was told before, and over its own work once that has held the round
    /// [`upgrade::SILENT_HOLD_S`] unbroken, and the line after the resume says what ended
    /// ([`upgrade::Facts::silent`]).
    Silent,
    /// The line, under an autopilot that carries the session.
    Fact,
    /// The line with the restart the agent may run itself (`aterm ctl @self restart`), where that
    /// restart is offered and promised for the tab.
    Verb,
}

/// What the lane of the owner's press of the ⬆️ is told ([`press`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressOpts {
    /// The tab's harness is off: the restart takes the quiet flow, typing only the relaunch line.
    pub off: bool,
}

/// What [`press`] answers for a tab whose record holds no standing press
/// ([`St::press_stands`]): the lane ends, and nothing is stepped for it.
pub const PRESS_LAPSED: &str = "skip:no-press";

/// Source of live `aterm` control sockets for a CLI upgrade sweep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instances {
    Machine,
    Listed(Vec<String>),
}

fn claude_profile(user_home: &Path, claude_dir: &Path) -> PathBuf {
    if claude_dir == user_home.join(".claude") {
        user_home.join(".claude.json")
    } else {
        claude_dir.join(".claude.json")
    }
}

/// What a sweep did with one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The effective urgency rung read by this step's gate, when the step reached one.
    pub rung: Option<upgrade::Rung>,
    pub pid: u32,
    pub tab: String,
    pub session: String,
    pub from: String,
    pub to: String,
    pub step: String,
}

impl Report {
    /// Whether this report is something the sweep did or recorded, as opposed to a wait, a skip or `current`.
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

    /// The same line under another name, so a log never calls a relaunch an upgrade.
    #[must_use]
    pub fn line_as(&self, kind: &str) -> String {
        format!(
            "{kind} pid={} tab={} session={} from={} to={} step={}",
            self.pid, self.tab, self.session, self.from, self.to, self.step
        )
    }
}

thread_local! {
    static GIVEN_NOW: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub(super) fn now_s() -> u64 {
    GIVEN_NOW
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| aterm_time::unix_secs().unwrap_or(0))
}

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

#[cfg(all(not(windows), not(test), not(target_os = "macos")))]
const PS_WAIT: Duration = Duration::from_secs(10);
#[cfg(all(not(windows), test))]
const PS_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// `ps`, which Windows does not have: nothing there compiles this, and each reader that asks it
/// elsewhere has a Windows arm that reads the kernel in process.
#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
fn ps(args: &[&str]) -> Option<String> {
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"));
    cmd.args(args).env("LC_ALL", "C").env("TZ", "UTC");
    let now = Instant::now();
    let deadline = now
        .checked_add(PS_WAIT)
        .unwrap_or_else(|| now + Duration::from_secs(60));
    let out = aterm_uds::deadline::output_until(&mut cmd, deadline).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(super) fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn kernel_start(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        aterm_uds::process::read_creation_filetime(i32::try_from(pid).ok()?)
            .map(|ticks| ticks.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::kproc::record(pid).map(|r| super::footer::lstart_utc(r.start_s))
    }
    // `ps -o lstart`'s second in `lstart_utc`'s spelling, which is `ps`'s own under `TZ=UTC`
    // squashed, so a record an earlier build took by `ps` names the same start.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        aterm_uds::process::read_process_start_s(i32::try_from(pid).ok()?)
            .map(super::footer::lstart_utc)
    }
    #[cfg(not(any(
        windows,
        target_os = "macos",
        target_os = "linux",
        target_os = "android"
    )))]
    {
        let s = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()])?);
        (!s.is_empty()).then_some(s)
    }
}

pub(super) fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let Ok(p) = i32::try_from(pid) else {
            return false;
        };
        aterm_uds::process::process_presence(p) == aterm_uds::process::ProcessPresence::Present
    }
    #[cfg(not(unix))]
    {
        aterm_uds::process::pid_alive(pid)
    }
}

pub(super) fn ids(pid: u32) -> Option<(u32, i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        let r = aterm_sysprobe::kproc::record(pid)?;
        Some((r.ppid, i64::from(r.pgid), i64::from(r.tpgid)))
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        aterm_uds::process::read_process_ids(i32::try_from(pid).ok()?)
    }
    // Windows has no process group and no terminal foreground group, so no process there has
    // these ids: a reader of the parent alone asks [`parent_of`].
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
    #[cfg(not(any(
        windows,
        target_os = "macos",
        target_os = "linux",
        target_os = "android"
    )))]
    {
        let out = ps(&["-o", "ppid=,pgid=,tpgid=", "-p", &pid.to_string()])?;
        let mut it = out.split_whitespace();
        Some((
            crate::parse_uint(it.next()?)?,
            crate::parse_int(it.next()?)?,
            crate::parse_int(it.next()?)?,
        ))
    }
}

/// `pid`'s parent, `None` when `pid` is gone or unread. On Windows it is the process table's
/// ([`table`]), whose parent is `0` where the pid the kernel recorded now names a younger
/// process; elsewhere it is [`ids`]'s.
pub(super) fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(windows)]
    {
        kernel_rows()
            .into_iter()
            .find(|r| r.pid == pid)
            .map(|r| r.ppid)
    }
    #[cfg(not(windows))]
    {
        ids(pid).map(|(ppid, _, _)| ppid)
    }
}

pub(super) fn process_group(pid: u32) -> Option<i64> {
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

pub(super) type ProcRow = (u32, u32, Comm);

#[derive(Clone)]
pub(super) struct Comm {
    name: std::sync::OnceLock<String>,
    #[cfg(target_os = "macos")]
    record: Option<aterm_sysprobe::kproc::Record>,
}

impl Comm {
    #[cfg(target_os = "macos")]
    const fn named_when_asked(r: aterm_sysprobe::kproc::Record) -> Self {
        Self {
            name: std::sync::OnceLock::new(),
            record: Some(r),
        }
    }

    pub(super) fn as_str(&self) -> &str {
        self.name.get_or_init(|| {
            #[cfg(target_os = "macos")]
            if let Some(r) = &self.record {
                return comm_name(r);
            }
            String::new()
        })
    }
}

impl From<String> for Comm {
    fn from(name: String) -> Self {
        Self {
            name: std::sync::OnceLock::from(name),
            #[cfg(target_os = "macos")]
            record: None,
        }
    }
}

impl From<&str> for Comm {
    fn from(name: &str) -> Self {
        Self::from(name.to_string())
    }
}

impl std::ops::Deref for Comm {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<str> for Comm {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Comm {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq for Comm {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl std::fmt::Debug for Comm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::fmt::Display for Comm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(super) fn table() -> Vec<ProcRow> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::kproc::records()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !r.zombie)
            .map(|r| (r.pid, r.ppid, Comm::named_when_asked(r)))
            .collect()
    }
    #[cfg(windows)]
    {
        kernel_rows()
            .into_iter()
            .map(|r| (r.pid, r.ppid, image_word(&r.name).into()))
            .collect()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        ps_table()
    }
}

/// Every process on Windows, from one in-process read of the kernel's table
/// ([`aterm_sysprobe::processes`]): no child process is started, where the `ps` this module asks
/// elsewhere is, on Windows, whatever MSYS tool `PATH` holds or nothing. A row's parent is `0`
/// where the pid the kernel recorded as its parent now names a process created after it
/// (Windows reuses pids and never rewrites the recorded parent), so a walk over parents never
/// crosses into a process that is no ancestor. Empty where the read fails.
#[cfg(windows)]
fn kernel_rows() -> Vec<aterm_sysprobe::ProcessReading> {
    aterm_sysprobe::processes()
        .map(|sweep| sweep.procs)
        .unwrap_or_default()
}

/// A Windows image name as the word the other platforms' `comm` is: lowercase (Windows file
/// names differ in case alone, so `Claude.EXE` and `claude.exe` are one program) with its `.exe`
/// dropped, so `claude.exe` is the `claude` every reader of a [`ProcRow`] matches by name.
#[cfg(windows)]
fn image_word(name: &str) -> String {
    let word = name.to_ascii_lowercase();
    match word.strip_suffix(".exe") {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => word,
    }
}

#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
fn ps_table() -> Vec<ProcRow> {
    let Some(out) = ps(&["-A", "-o", "pid=,ppid=,comm="]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = crate::parse_uint(it.next()?)?;
            let ppid = crate::parse_uint(it.next()?)?;
            Some((
                pid,
                ppid,
                base_name(&it.collect::<Vec<_>>().join(" ")).into(),
            ))
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn comm_name(r: &aterm_sysprobe::kproc::Record) -> String {
    base_name(&squash(&aterm_sysprobe::kproc::ps_comm(
        r,
        aterm_sysprobe::kproc::Locale::C,
    )))
}

#[cfg(target_os = "macos")]
fn tty_word(tty: Option<i32>) -> String {
    tty.map_or_else(|| "??".to_string(), |dev| format!("dev{dev}"))
}

pub(super) fn descendants_in(table: &[ProcRow], root: u32) -> Option<Vec<(u32, String)>> {
    if table.is_empty() {
        return None;
    }
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid, name) in table {
            if *ppid == parent
                && *pid != root
                && name != aterm_sysprobe::kproc::DEFUNCT
                && !name.ends_with(" <defunct>")
                && !found.iter().any(|(p, _)| p == pid)
            {
                found.push((*pid, name.to_string()));
                frontier.push(*pid);
            }
        }
    }
    Some(found)
}

fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

/// The `ps` column that names a process's controlling terminal. On macOS it is `tdev`, the device
/// number `ps` prints from the kernel's record: its `tty` name comes from `devname(3)`, a scan of
/// `/dev` that skips a live terminal's node while an earlier node is removed, so under another
/// process's pty churn it prints `??` for a process that holds a terminal.
#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
const PS_TTY: &str = if cfg!(target_os = "macos") {
    "tdev"
} else {
    "tty"
};

#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
fn tty_table() -> Vec<(u32, u32, String, String)> {
    let Some(out) = ps(&["-A", "-o", &format!("pid=,ppid=,{PS_TTY}=,comm=")]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = crate::parse_uint(it.next()?)?;
            let ppid = crate::parse_uint(it.next()?)?;
            let tty = it.next()?.to_string();
            Some((pid, ppid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

#[cfg(not(windows))]
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

fn live_terminal(pid: u32) -> Option<(u32, String)> {
    #[cfg(target_os = "macos")]
    {
        let rows = aterm_sysprobe::kproc::records()?;
        let walk: Vec<(u32, u32, String, String)> = rows
            .iter()
            .map(|r| (r.pid, r.ppid, tty_word(r.tty), String::new()))
            .collect();
        let (owner, _) = terminal_owner(pid, &walk)?;
        let r = rows.iter().find(|r| r.pid == owner)?;
        Some((owner, comm_name(r)))
    }
    // No process on Windows has a controlling terminal: a pseudoconsole is not an identity the
    // kernel records against the processes attached to it.
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        terminal_owner(pid, &tty_table()).map(|(owner, name)| (owner, name.to_string()))
    }
}

fn opener(agent: u32, parent: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let tty = aterm_sysprobe::kproc::record(agent)?.tty?;
        let parent = aterm_sysprobe::kproc::record(parent)?;
        (parent.tty != Some(tty)).then(|| comm_name(&parent))
    }
    #[cfg(windows)]
    {
        let _ = (agent, parent);
        None
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        ps_opener(agent, parent)
    }
}

#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
fn ps_opener(agent: u32, parent: u32) -> Option<String> {
    // One pid per `ps`: asked for two or more at once, a `ps` whose own device read is
    // wedged never returns, while each pid asked alone answers — the oracle is the same
    // `ps`, read a pid at a time.
    let mut rows: Vec<(u32, String, String)> = Vec::new();
    for p in [agent, parent] {
        let out = ps(&["-o", &format!("pid=,{PS_TTY}=,comm="), "-p", &p.to_string()])?;
        rows.extend(out.lines().filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = crate::parse_uint(it.next()?)?;
            let tty = it.next()?.to_string();
            Some((pid, tty, base_name(&it.collect::<Vec<_>>().join(" "))))
        }));
    }
    let row = |p: u32| rows.iter().find(|(q, _, _)| *q == p);
    let (_, tty, _) = row(agent)?;
    if !tty.chars().any(|c| c.is_ascii_digit()) {
        return None; // `??`, `?`, `-`: no controlling terminal at all.
    }
    let (_, parent_tty, name) = row(parent)?;
    (parent_tty != tty).then(|| name.clone())
}

pub(super) fn owned_by_aterm(owner: (u32, &str)) -> bool {
    owner.0 == 1 || owner.1 == "aterm" || owner.1.starts_with("aterm-")
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Job {
    Foreground,
    Background,
    TabProgram { foreground: bool },
    NoJobControl,
}

fn job(agent: u32, agent_pgid: i64, tpgid: i64, parent_pgid: i64, aterm_opened: bool) -> Job {
    if agent_pgid != i64::from(agent) {
        Job::NoJobControl
    } else if aterm_opened {
        Job::TabProgram {
            foreground: tpgid == agent_pgid,
        }
    } else if parent_pgid == agent_pgid {
        Job::NoJobControl
    } else if tpgid != agent_pgid {
        Job::Background
    } else {
        Job::Foreground
    }
}

fn job_of<K: Kernel + ?Sized>(k: &K, agent: u32) -> Option<(Job, u32)> {
    let (parent, pgid, tpgid) = k.ids(agent)?;
    let (_, parent_pgid, _) = k.ids(parent)?;
    let opened = k
        .opener(agent, parent)
        .is_some_and(|name| owned_by_aterm((parent, name.as_str())));
    Some((job(agent, pgid, tpgid, parent_pgid, opened), parent))
}

pub(super) const fn foreground_shell(job: Option<(Job, u32)>) -> Result<u32, &'static str> {
    match job {
        Some((Job::Foreground, shell)) => Ok(shell),
        Some((Job::Background, _)) => Err("not-foreground"),
        Some((Job::NoJobControl | Job::TabProgram { .. }, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

pub(super) const fn leads_terminal(job: Option<(Job, u32)>) -> Result<(), &'static str> {
    match job {
        Some((Job::Foreground | Job::TabProgram { foreground: true }, _)) => Ok(()),
        Some((Job::Background | Job::TabProgram { foreground: false }, _)) => Err("not-foreground"),
        Some((Job::NoJobControl, _)) => Err("not-a-shell-job"),
        None => Err("ids"),
    }
}

pub(super) const fn not_a_shell_job(job: Option<(Job, u32)>) -> Option<Job> {
    match job {
        Some((job @ (Job::NoJobControl | Job::TabProgram { .. }), _)) => Some(job),
        Some((Job::Foreground | Job::Background, _)) | None => None,
    }
}

pub(super) trait Kernel {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        job_of(self, agent)
    }
    fn ids(&self, pid: u32) -> Option<(u32, i64, i64)> {
        ids(pid)
    }
    fn parent(&self, pid: u32) -> Option<u32>;
    fn terminal(&self, pid: u32) -> Option<(u32, String)>;
    fn opener(&self, agent: u32, parent: u32) -> Option<String> {
        self.terminal(agent)
            .filter(|(owner, _)| *owner == parent)
            .map(|(_, name)| name)
    }
    /// `pid`'s kernel start ([`kernel_start`]; `None`: gone or unread), which the last look before
    /// a signal holds to the start its session file names. Required, as
    /// [`Self::restart_background`] is, so that no kernel reads the machine at that look unless it
    /// says so.
    fn process_start(&self, pid: u32) -> Option<String>;
    /// Optional exact liveness evidence. An injected kernel is inert unless it explicitly
    /// supplies a reader; the live kernel captures this around the snapshot's process facts.
    fn process_lifetime(&self, _pid: u32) -> Option<super::relaunch::ProcessLifetime> {
        None
    }
    /// Shell rendering facts from the same injected platform source as the restart's job.
    fn shell_dialect(&self, shell: u32, processes: &[ProcRow]) -> Option<super::upgrade::Dialect> {
        super::relaunch::shell_dialect(shell, processes)
    }
    /// The work under `pid` a restart would end ([`live_background`]; [`UNREADABLE_TABLE`] where
    /// its processes were not read), which the last look before a signal requires to be none
    /// unless the owner pressed the restart.
    fn restart_background(&self, pid: u32) -> Vec<String>;
    /// The shells under `pid` as the last look before a signal reads them again, each with its
    /// seat and command ([`held_read`], on macOS [`held_by_children`]; `None`: its processes or a
    /// row of them were not read). A move
    /// over the agent's waits ([`upgrade::Yield::Waits`]) signals only where these are still
    /// waits it carries. Required, as [`Self::restart_background`] is.
    fn restart_held(&self, pid: u32) -> Option<Vec<upgrade::Held>>;
    fn image(&self, pid: u32) -> Option<PathBuf> {
        exe_of(pid)
    }
    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        if shell == 0 {
            return None;
        }
        let (_, pgid, tpgid) = self.ids(shell)?;
        Some(pgid == i64::from(shell) && pgid == tpgid)
    }
    /// What each of `procs`, the shells under `agent` in table `t`, is: its age, its seat and,
    /// for a direct child, its command. The default reads no machine: every one a top with no
    /// command, which is never a wait ([`upgrade::own_of`]).
    fn describe(&self, agent: u32, procs: &[(u32, String)], t: &[ProcRow]) -> Vec<upgrade::Held> {
        let _ = (agent, t);
        procs
            .iter()
            .map(|(pid, name)| upgrade::Held {
                pid: *pid,
                name: name.clone(),
                seat: upgrade::Seat::Top,
                ..upgrade::Held::default()
            })
            .collect()
    }
}

pub(super) struct Live;

impl Kernel for Live {
    fn parent(&self, pid: u32) -> Option<u32> {
        if !alive(pid) {
            return None;
        }
        parent_of(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        live_terminal(pid)
    }

    fn opener(&self, agent: u32, parent: u32) -> Option<String> {
        opener(agent, parent)
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        kernel_start(pid)
    }

    fn process_lifetime(&self, pid: u32) -> Option<super::relaunch::ProcessLifetime> {
        super::relaunch::ProcessLifetime::capture(pid)
    }

    fn restart_background(&self, pid: u32) -> Vec<String> {
        live_background(pid)
    }

    fn restart_held(&self, pid: u32) -> Option<Vec<upgrade::Held>> {
        // Read as the last look's work is ([`live_background`]): on macOS from the agent's own
        // children, never the whole table, whose read a busy machine holds past its bound.
        #[cfg(target_os = "macos")]
        {
            held_by_children(self, pid, aterm_sysprobe::jobs::child_pids, live_child_row)
        }
        #[cfg(not(target_os = "macos"))]
        {
            held_read(self, pid, &table())
        }
    }

    fn describe(&self, agent: u32, procs: &[(u32, String)], t: &[ProcRow]) -> Vec<upgrade::Held> {
        held_of(agent, procs, t)
    }
}

/// Every shell under `agent` in table `t` as `k` describes it, or `None` where the table was not
/// read or a row of it went unread (a shell that left between the table and its own read): what
/// [`upgrade::own_of`] may class. An unread row is never a wait.
pub(super) fn held_read(
    k: &(impl Kernel + ?Sized),
    agent: u32,
    t: &[ProcRow],
) -> Option<Vec<upgrade::Held>> {
    if t.is_empty() {
        return None;
    }
    held_in(k, agent, t)
}

/// The shells under `agent` as `k` describes them, read from `agent`'s own descendants
/// ([`descendant_rows`]) and never the whole table, whose read a busy machine holds past its bound
/// ([`live_background`]): `None` where those or a shell's own row were not read, and nothing held
/// where nothing is under `agent`.
#[cfg(any(all(test, unix), target_os = "macos"))]
pub(super) fn held_by_children(
    k: &(impl Kernel + ?Sized),
    agent: u32,
    children: impl Fn(u32) -> Option<Vec<u32>>,
    row: impl Fn(u32) -> std::io::Result<Option<(u32, Comm)>>,
) -> Option<Vec<upgrade::Held>> {
    held_in(k, agent, &descendant_rows(agent, children, row)?)
}

/// Every shell under `agent` in rows `t` as `k` describes it, or `None` where a shell's own row
/// went unread (one that left between `t` and its own read).
fn held_in(k: &(impl Kernel + ?Sized), agent: u32, t: &[ProcRow]) -> Option<Vec<upgrade::Held>> {
    let procs = background_procs(agent, t);
    let held = k.describe(agent, &procs, t);
    (held.len() == procs.len()).then_some(held)
}

/// Where the shell `pid` sits under `agent` in `t`, and the names of all under it where it is a
/// top ([`upgrade::Seat`]).
fn seat_of(agent: u32, pid: u32, t: &[ProcRow]) -> (upgrade::Seat, Vec<String>) {
    let parent = |p: u32| t.iter().find(|(c, _, _)| *c == p).map(|(_, pp, _)| *pp);
    if parent(pid) == Some(agent) {
        let mut under = Vec::new();
        let mut frontier = vec![pid];
        let mut seen = 0;
        while let Some(p) = frontier.pop() {
            seen += 1;
            if seen > 4096 {
                // A table too deep to walk names something this list does not: no wait.
                under.push(UNREADABLE_TABLE.to_string());
                break;
            }
            for (c, pp, name) in t {
                if *pp == p {
                    frontier.push(*c);
                    under.push(name.to_string());
                }
            }
        }
        return (upgrade::Seat::Top, under);
    }
    // Under a top: the agent's child above it is one of its held shells.
    let mut p = pid;
    for _ in 0..64 {
        let Some(pp) = parent(p) else { break };
        if pp == agent {
            let held = t.iter().any(|(c, _, name)| {
                *c == p && BACKGROUND.contains(&name.as_str()) && name != OWN_KEEP_AWAKE
            });
            if held {
                return (upgrade::Seat::Under, Vec::new());
            }
            break;
        }
        p = pp;
    }
    (upgrade::Seat::Loose, Vec::new())
}

/// Whether the command `raw` carries ([`command_head`]: Claude Code's Bash tool's `eval` payload,
/// or the whole of `raw` where it has none), `raw` a process's arguments as the kernel gave them
/// and `ps` prints them, is the command it was started with, byte for byte: no escape `ps` wrote
/// for a byte it does not print in that command (a newline, a tab, a byte above ASCII; a
/// backslash of the command's own reads the same) and no run of blanks [`squash`] folds. The
/// Bash tool's own words around its payload (`\builtin unalias …`, which every shell it starts
/// carries) are no part of the command, so their backslashes judge nothing.
#[cfg(target_os = "macos")]
fn exact_args(raw: &str) -> bool {
    !typed_part(raw).contains('\\') && raw == squash(raw)
}

/// The part of `raw` [`command_head`] reads the command from, before it undoes its quoting.
#[cfg(target_os = "macos")]
fn typed_part(raw: &str) -> &str {
    raw.split_once("&& eval '").map_or(raw, |(_, payload)| {
        payload
            .rsplit_once("' < /dev/null")
            .map_or(payload, |(head, _)| head)
    })
}

const BACKGROUND: &[&str] = upgrade::BACKGROUND;

const OWN_KEEP_AWAKE: &str = "caffeinate";

pub(super) fn background(pid: u32, t: &[ProcRow]) -> Vec<String> {
    background_procs(pid, t)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// The agent's own work a restart would end: the shells under it ([`background`]), and the work
/// Claude Code's screen names that no process row names ([`aterm_phase::phase::unlisted_work`],
/// read whatever rule the reader reads first): the work it runs in its own process (its status
/// row's `Waiting for …`, the wait it carries above its own wake, a done row's `… still running`),
/// and any work its footer's pill counts that is no shell or monitor counted alone (`1 MCP task`,
/// `2 background tasks`, `◇ 1 cloud session`). The shells and monitors the screen names are the
/// process table's to name, read here, not off the screen.
pub(super) fn own_work(pid: u32, t: &[ProcRow], rows: &[String]) -> Vec<String> {
    let mut work = background(pid, t);
    if let Some(rule) = aterm_phase::phase::unlisted_work(rows) {
        work.push(format!("screen:{}", rule.replace(' ', "-")));
    }
    work
}

pub(super) fn background_procs(pid: u32, t: &[ProcRow]) -> Vec<(u32, String)> {
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
                    found.push((*c, name.to_string()));
                }
            }
        }
    }
    found
}

pub(super) const UNREADABLE_TABLE: &str = "ps-unreadable";

/// What opens a record's `ended_work` where one is owed ([`St::ended_work`]), so that work the
/// process table named nothing of still reads as owed.
const ENDED_WORK_MARK: &str = "ended:";

/// The work under `pid` a restart would end ([`background`]; [`UNREADABLE_TABLE`] where it was not
/// read). On macOS it is read from `pid`'s own descendants ([`background_by_children`]): each
/// parent's children by themselves (libproc's `proc_listchildpids`,
/// [`aterm_sysprobe::jobs::child_pids`]) and each child's record by its pid, never a read of the
/// whole table. The kernel answers a `KERN_PROC_ALL` read only once every process it lists is
/// through an exec in progress, and [`table`] gives that read one second; on a busy machine (a
/// test run, a build, anything that execs freshly signed programs) it misses that bound, and the
/// restart's last look read the unread table as work and refused a restart it had already taken
/// the tab's hand for. Elsewhere the table's rows are walked as before.
pub(super) fn live_background(pid: u32) -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        background_by_children(pid, aterm_sysprobe::jobs::child_pids, live_child_row)
    }
    #[cfg(not(target_os = "macos"))]
    {
        background_or_unreadable(pid, &table())
    }
}

/// One child's parent and name as the live kernel records them by its pid, for
/// [`descendant_rows`]: `Ok(None)` where it is gone or a zombie, which runs nothing.
#[cfg(target_os = "macos")]
fn live_child_row(child: u32) -> std::io::Result<Option<(u32, Comm)>> {
    aterm_sysprobe::kproc::record_read(child).map(|record| {
        record
            .filter(|r| !r.zombie)
            .map(|r| (r.ppid, Comm::named_when_asked(r)))
    })
}

/// [`background`] over `pid`'s descendants ([`descendant_rows`]). Descendants that were not read
/// make the whole read [`UNREADABLE_TABLE`], as an unread table does: no work can be ruled out.
#[cfg(any(all(test, unix), target_os = "macos"))]
pub(super) fn background_by_children(
    pid: u32,
    children: impl Fn(u32) -> Option<Vec<u32>>,
    row: impl Fn(u32) -> std::io::Result<Option<(u32, Comm)>>,
) -> Vec<String> {
    descendant_rows(pid, children, row).map_or_else(
        || vec![UNREADABLE_TABLE.to_string()],
        |rows| background(pid, &rows),
    )
}

/// `pid`'s descendants as table rows, found one parent at a time: `children` lists a parent's
/// children (`None`: not read), and `row` reads one child's parent and name (`Ok(None)`: gone, or
/// a zombie, which runs nothing; `Err`: not read). `None` where a list or a row does not read. A
/// child that names another parent by the time its row is read left `pid`'s tree and is not
/// walked, and the walk stops past [`background_procs`]'s 4096 parents, reading as unread. No rows
/// is a process with nothing under it, which an empty whole table never is.
#[cfg(any(all(test, unix), target_os = "macos"))]
pub(super) fn descendant_rows(
    pid: u32,
    children: impl Fn(u32) -> Option<Vec<u32>>,
    row: impl Fn(u32) -> std::io::Result<Option<(u32, Comm)>>,
) -> Option<Vec<ProcRow>> {
    let mut rows: Vec<ProcRow> = Vec::new();
    let mut frontier = vec![pid];
    let mut seen = 0;
    while let Some(parent) = frontier.pop() {
        seen += 1;
        if seen > 4096 {
            return None;
        }
        for child in children(parent)? {
            match row(child) {
                Ok(Some((ppid, name))) if ppid == parent => {
                    frontier.push(child);
                    rows.push((child, parent, name));
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }
    Some(rows)
}

#[cfg(any(test, not(target_os = "macos")))]
pub(super) fn background_or_unreadable(pid: u32, t: &[ProcRow]) -> Vec<String> {
    if t.is_empty() {
        return vec![UNREADABLE_TABLE.to_string()];
    }
    background(pid, t)
}

pub(super) fn held_of(agent: u32, procs: &[(u32, String)], t: &[ProcRow]) -> Vec<upgrade::Held> {
    if procs.is_empty() {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    {
        use aterm_sysprobe::kproc;
        let now = aterm_time::unix_secs().unwrap_or(0);
        procs
            .iter()
            .filter_map(|(p, name)| {
                let r = kproc::record(*p)?;
                let (seat, under) = seat_of(agent, *p, t);
                let raw = (r.ppid == agent).then(|| kproc::ps_args(&r, kproc::Locale::C));
                Some(upgrade::Held {
                    pid: *p,
                    name: name.clone(),
                    age_s: now.saturating_sub(r.start_s),
                    command: raw
                        .as_deref()
                        .map(|raw| command_head(&squash(raw)))
                        .unwrap_or_default(),
                    // The kernel's parent and the table's agree, or the seat is not known.
                    seat: if (r.ppid == agent) == (seat == upgrade::Seat::Top) {
                        seat
                    } else {
                        upgrade::Seat::Loose
                    },
                    under,
                    exact: raw.as_deref().is_some_and(exact_args),
                })
            })
            .collect()
    }
    // One sweep names each process's parent and creation time; the command line of the agent's
    // own children is read from each one's process parameters, empty where that read is refused.
    #[cfg(windows)]
    {
        let rows = kernel_rows();
        let now_ms = aterm_time::since_epoch().and_then(|d| i64::try_from(d.as_millis()).ok());
        procs
            .iter()
            .filter_map(|(p, name)| {
                let r = rows.iter().find(|r| r.pid == *p)?;
                let age_ms = now_ms?.saturating_sub(r.birth_ms?);
                let (seat, under) = seat_of(agent, *p, t);
                Some(upgrade::Held {
                    pid: *p,
                    name: name.clone(),
                    age_s: u64::try_from(age_ms / 1000).unwrap_or(0),
                    command: if r.ppid == agent {
                        atpkg::caller_shell::process_args(*p)
                            .map(|a| command_head(&squash(&a.argv.join(" "))))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    },
                    // This sweep's parent and the table's agree, or the seat is not known.
                    seat: if (r.ppid == agent) == (seat == upgrade::Seat::Top) {
                        seat
                    } else {
                        upgrade::Seat::Loose
                    },
                    under,
                    // The words are the parsed arguments joined with blanks, not the command
                    // line Windows keeps as one string: never the command byte for byte, so
                    // never handed back to start again.
                    exact: false,
                })
            })
            .collect()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        ps_held_of(agent, procs, t)
    }
}

#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
fn ps_held_of(agent: u32, procs: &[(u32, String)], t: &[ProcRow]) -> Vec<upgrade::Held> {
    // On macOS this is only the tests' oracle (production reads the kernel), and there
    // `ps` is read a pid at a time, as in [`ps_opener`]: asked for a list at once, a
    // `ps` whose own device read is wedged never returns at all. Elsewhere one call
    // reads the whole list, as it always has.
    let out = if cfg!(target_os = "macos") {
        procs
            .iter()
            .filter_map(|(p, _)| ps(&["-o", "pid=,ppid=,etime=,args=", "-p", &p.to_string()]))
            .collect::<Vec<_>>()
            .join("")
    } else {
        let list = procs
            .iter()
            .map(|(p, _)| p.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let Some(out) = ps(&["-o", "pid=,ppid=,etime=,args=", "-p", &list]) else {
            return Vec::new();
        };
        out
    };
    procs
        .iter()
        .filter_map(|(p, name)| {
            let line = out
                .lines()
                .find(|l| l.split_whitespace().next() == Some(p.to_string().as_str()))?;
            let mut it = line.split_whitespace();
            let _ = it.next();
            let ppid: u32 = crate::parse_uint(it.next()?)?;
            let age_s = parse_etime(it.next()?)?;
            let args = it.collect::<Vec<_>>().join(" ");
            let (seat, under) = seat_of(agent, *p, t);
            Some(upgrade::Held {
                pid: *p,
                name: name.clone(),
                age_s,
                command: if ppid == agent {
                    command_head(&args)
                } else {
                    String::new()
                },
                seat,
                under,
                // `ps` may cut its `args` column and these words were split on blanks: never
                // the command byte for byte, so never handed back to start again.
                exact: false,
            })
        })
        .collect()
}

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
        (upgrade::Agent::Muse, _) => {
            "Muse Code's live upgrade asks nothing (its quiet flow alone): it is restarted onto \
             the new build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
        (upgrade::Agent::Agy, _) => {
            "agy's live upgrade asks nothing (its quiet flow alone): it is restarted onto the new \
             build at its next genuinely idle point, and nothing is typed into it"
                .to_string()
        }
    };
    let list = upgrade::held_list(held);
    if list.is_empty() {
        why
    } else {
        format!("{why}; still running under the agent: {list}")
    }
}

#[cfg(all(not(windows), any(not(target_os = "macos"), test)))]
pub(super) fn parse_etime(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.split_once('-') {
        Some((d, rest)) => (crate::parse_uint::<u64>(d)?, rest),
        None => (0, etime),
    };
    let parts = clock
        .split(':')
        .map(crate::parse_uint::<u64>)
        .collect::<Option<Vec<_>>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, sec] => (0, *m, *sec),
        [h, m, sec] => (*h, *m, *sec),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

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

/// Whether `pid` is this process or one of its ancestors, which no sweep signals. Each link of
/// this process's chain comes from `t` or, where `t` lacks it, from the kernel's record of that
/// one process ([`link_above`]). That record answers while [`table`] gives up on a whole-table read that
/// an exec elsewhere holds past its bound and returns no rows. A link neither gives, and a chain
/// deeper than 64, count as an ancestor: a sweep never signals what it cannot prove is not its
/// own.
///
/// On Windows the kernel reparents nothing: a process whose parent exited still names it. So
/// there a pid with no row in a whole table is a process that is gone, where this process's
/// chain ends, and only a table that was not read whole leaves a link ungiven.
pub(super) fn is_our_ancestor(pid: u32, t: &[ProcRow]) -> bool {
    let mut p = std::process::id();
    for _ in 0..64 {
        if p == pid {
            return true;
        }
        let parent = t
            .iter()
            .find(|(c, _, _)| *c == p)
            .map(|(_, pp, _)| *pp)
            .or_else(|| link_above(p));
        match parent {
            Some(pp) if pp != p && pp != 0 => p = pp,
            Some(_) => return false,
            None => return true,
        }
    }
    true
}

/// The parent of `p`, a link of this process's chain its caller's table lacks
/// ([`is_our_ancestor`]): `0` where the chain ends at `p`, `None` where the link is not known.
fn link_above(p: u32) -> Option<u32> {
    #[cfg(windows)]
    {
        let whole = aterm_sysprobe::processes()
            .filter(|sweep| !sweep.truncated && !sweep.procs.is_empty())?;
        Some(
            whole
                .procs
                .iter()
                .find(|r| r.pid == p)
                .map_or(0, |r| r.ppid),
        )
    }
    #[cfg(not(windows))]
    {
        ids(p).map(|(pp, _, _)| pp)
    }
}

pub(super) fn cwd_of(pid: u32) -> Option<String> {
    aterm_sysprobe::process_cwd(pid).map(|p| p.to_string_lossy().into_owned())
}

pub(super) fn exe_of(pid: u32) -> Option<PathBuf> {
    aterm_sysprobe::process_path(pid)
}

/// Every file `pid` holds open through a descriptor, `None` when unreadable or exited, read in
/// process ([`aterm_uds::holders::open_files`]).
pub(super) fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    aterm_uds::holders::open_files(pid).ok()
}

/// Returns `scan.holders` when `scan.unreadable` is empty; sealed tables (`HolderScan::sealed`) do not make the scan incomplete.
fn complete_file_holders(scan: aterm_uds::holders::HolderScan) -> Option<Vec<u32>> {
    scan.unreadable.is_empty().then_some(scan.holders)
}

/// Every process holding `file` open through a descriptor, `None` when unknown: only a
/// complete in-process scan ([`aterm_uds::holders::scan_file_holders`]) is an answer.
pub(super) fn file_holders(file: &Path) -> Option<Vec<u32>> {
    complete_file_holders(aterm_uds::holders::scan_file_holders(file, &[]).ok()?)
}

/// `lsof -t`'s holders of `file` among `pids` alone: the oracle [`file_holders`] is held to.
/// lsof describes every descriptor of each process it reads, a socket or kqueue too, and that
/// read drains one its holder closes meanwhile, so it reads only `pids`: quiet children that
/// have exec'd and hold no socket.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn lsof_holders(file: &Path, pids: &[u32]) -> Option<Vec<u32>> {
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new(
        aterm_uds::os_path!("lsof"),
    )));
    cmd.args(["-n", "-P", "-t", "-a", "-p", &list.join(","), "--"])
        .arg(file);
    let now = Instant::now();
    let deadline = now
        .checked_add(aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|| now + Duration::from_secs(60));
    let out = aterm_uds::deadline::output_until(&mut cmd, deadline).ok()?;
    lsof_holder_pids(&out)
}

#[cfg(all(test, target_os = "macos"))]
fn lsof_holder_pids(out: &std::process::Output) -> Option<Vec<u32>> {
    if !out.stderr.is_empty() {
        return None;
    }
    match out.status.code()? {
        0 => std::str::from_utf8(&out.stdout)
            .ok()?
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<u32>, _>>()
            .ok(),
        1 if out.stdout.is_empty() => Some(Vec::new()),
        _ => None,
    }
}

pub(super) fn blocks_of(head: &str, body: &str) -> Option<Vec<Value>> {
    if !crate::is_ok_reply(head) {
        return None;
    }
    let json = if body.trim().is_empty() {
        head.strip_prefix("OK ")?.to_string()
    } else {
        body.to_string()
    };
    let v: Value = aterm_json::from_str(json.trim()).ok()?;
    Some(v.get("blocks")?.as_array()?.clone())
}

pub(super) fn running_block(c: &mut Client, tab: &str) -> Option<u64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 1 --json")).ok()?;
    let blocks = blocks_of(&head, &body)?;
    let last = blocks.last()?;
    (last.get("state").and_then(Value::as_str) == Some("executing"))
        .then(|| last.get("id").and_then(Value::as_u64))
        .flatten()
}

pub(super) fn exit_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let (head, body) = c.request_counted(&format!("@{tab} blocks 2 --json")).ok()?;
    parse_exit_status(&head, &body, block)
}

pub(crate) fn parse_exit_status(head: &str, body: &str, block: Option<u64>) -> Option<i64> {
    let blocks = blocks_of(head, body)?;
    let (last, rest) = blocks.split_last()?;
    if last.get("state").and_then(Value::as_str) != Some("entering") {
        return None;
    }
    let prompt = last.get("prompt").and_then(Value::as_u64)?;
    let done = rest.last()?;
    (done.get("state").and_then(Value::as_str) == Some("complete")
        && done.get("end").and_then(Value::as_u64) == Some(prompt)
        && block.is_none_or(|block| done.get("id").and_then(Value::as_u64) == Some(block)))
    .then(|| done.get("exit").and_then(Value::as_i64))
    .flatten()
}

pub(super) const LINE_BLOCKS: u32 = 8;

pub(super) fn block_exit(head: &str, body: &str, block: u64) -> Option<i64> {
    blocks_of(head, body)?
        .iter()
        .find(|b| b.get("id").and_then(Value::as_u64) == Some(block))
        .filter(|b| b.get("state").and_then(Value::as_str) == Some("complete"))?
        .get("exit")
        .and_then(Value::as_i64)
}

pub(super) fn line_status(c: &mut Client, tab: &str, block: Option<u64>) -> Option<i64> {
    let block = block?;
    let (head, body) = c
        .request_counted(&format!("@{tab} blocks {LINE_BLOCKS} --json"))
        .ok()?;
    block_exit(&head, &body, block)
}

pub(super) fn started_ms(pid: u32) -> Option<u64> {
    // The kernel's own creation time, to the millisecond, where the others subtract an age in
    // whole seconds from now.
    #[cfg(windows)]
    {
        let ticks = aterm_uds::process::read_creation_filetime(i32::try_from(pid).ok()?)?;
        u64::try_from(aterm_sysprobe::procs::birth_of_filetime(ticks)?).ok()
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "macos")]
        let age_s = {
            let r = aterm_sysprobe::kproc::record(pid)?;
            aterm_time::unix_secs()?.saturating_sub(r.start_s)
        };
        #[cfg(not(target_os = "macos"))]
        let age_s = parse_etime(ps(&["-o", "etime=", "-p", &pid.to_string()])?.trim())?;
        let now_ms = u64::try_from(aterm_time::since_epoch()?.as_millis()).ok()?;
        Some(now_ms.saturating_sub(age_s.saturating_mul(1000)))
    }
}

pub(super) fn shell_dialect_of(
    shell: u32,
    args: Option<atpkg::caller_shell::ProcArgs>,
) -> Option<upgrade::Dialect> {
    args.map(|a| a.exec_path)
        .or_else(|| {
            table()
                .into_iter()
                .find(|(p, _, _)| *p == shell)
                .map(|(_, _, n)| n.to_string())
        })
        .as_deref()
        .map(base_name)
        .as_deref()
        .and_then(upgrade::Dialect::from_exe_name)
}

/// The build directory's name where `program` is `name`'s executable in an atpkg store
/// (`…/store/<name>/<build>/bin/<exe>`).
fn store_build_dir<'a>(program: &'a Path, name: &str) -> Option<&'a str> {
    let plain = program.components().all(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::Normal(_)
        )
    });
    if !program.has_root() || !plain || !names_program(program, name) {
        return None;
    }
    let mut up = program.components().rev().skip(1).map(|c| match c {
        std::path::Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let (bin, build, tool, store) = (up.next()??, up.next()??, up.next()??, up.next()??);
    (bin == "bin" && tool == name && store == "store").then_some(build)
}

/// The `version` the store record beside the build directory `build` names
/// (`store/<program>/<build>.vendor`), `None` when the record is unread or names none.
fn record_version(build: &Path) -> Option<String> {
    let record = build.with_file_name(format!("{}.vendor", build.file_name()?.to_str()?));
    let text = crate::harness::accounts::read_bounded(&record, 64 * 1024)
        .ok()
        .flatten()?;
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("version")?.trim_start();
        let v = v.strip_prefix('=')?.trim().trim_matches('"');
        (!v.is_empty() && !v.chars().any(char::is_control)).then(|| v.to_string())
    })
}

/// The version of the store build `program` is (Muse Code's, agy's or Claude Code's), as its
/// store record names it and its build directory agrees; nothing is executed.
pub(super) fn store_version(program: &Path) -> Option<String> {
    if !super::upgrade_muse::store_build(program)
        && !super::upgrade_agy::store_build(program)
        && store_build_dir(program, "claude").is_none()
    {
        return None;
    }
    let build = program.parent()?.parent()?;
    let dir = build.file_name()?.to_str()?;
    let version = record_version(build)?;
    super::upgrade_muse_live::build_version(&version, Some(dir)).map(|_| version)
}

/// The Claude Code build `exe` is, read from where it lies and never by running it. A store
/// build (`…/store/claude/<build>/bin/claude`) is named by its store record
/// ([`record_version`]) and by its build directory where that is the version's own number
/// (`atpkg::vendor_direct::Version::build_id`): either alone names it, and two that disagree
/// name none. A legacy index build has neither (its directory is the index's build number), and
/// is named by the version atpkg read from its signed manifest and kept in the program's stamp
/// ([`legacy_build_version`]). A native build (`…/.local/share/claude/versions/<version>`) is
/// named by its file name. `None` for any other path.
pub(super) fn claude_build_version(exe: &Path) -> Option<Version> {
    if let Some(dir) = store_build_dir(exe, "claude") {
        let build_dir = exe.parent()?.parent()?;
        let build = Some(dir)
            .filter(|dir| !(dir.len() > 1 && dir.starts_with('0')))
            .and_then(crate::parse_uint::<u64>);
        let numbered = build
            .and_then(atpkg::vendor_direct::Version::from_build_id)
            .map(|version| version.to_string());
        let named = match (record_version(build_dir), numbered) {
            (Some(recorded), Some(numbered)) if recorded != numbered => return None,
            (None, None) => legacy_build_version(build_dir, build?)?,
            (recorded, numbered) => recorded.or(numbered)?,
        };
        return Version::parse(&named);
    }
    exe.parent()
        .filter(|versions| versions.ends_with(".local/share/claude/versions"))
        .and(exe.file_name())
        .and_then(|name| Version::parse(&name.to_string_lossy()))
}

/// The version of Claude Code's legacy index build `build` at `build_dir`
/// (`<prefix>/store/claude/<build>`), as atpkg's stamp under that prefix names it
/// (`atpkg::vendor_direct::legacy_version`): `None` until a pass of atpkg's lane has read the
/// build's signed manifest; a legacy build atpkg vouches for and cannot name, its lane replaces
/// with the vendor's own (`InstallReason::ReplacesLegacy`).
fn legacy_build_version(build_dir: &Path, build: u64) -> Option<String> {
    let layout = atpkg::store::Layout {
        prefix: build_dir.parent()?.parent()?.parent()?.to_path_buf(),
    };
    atpkg::vendor_direct::legacy_version(&layout, "claude", build)
        .map(|version| version.to_string())
}

/// The build `sf`'s session runs: the version its record names, and for a record that names
/// none readable, the build its process's executable is ([`claude_build_version`]).
pub(super) fn running_version(sf: &SessionFile) -> Option<Version> {
    running_version_with(sf, exe_of)
}

fn running_version_with(
    sf: &SessionFile,
    exe: impl FnOnce(u32) -> Option<PathBuf>,
) -> Option<Version> {
    Version::parse(&sf.version).or_else(|| claude_build_version(&exe(sf.pid)?))
}

/// Whether `exe` is `program`'s executable by its file name, as atpkg names one in a build's
/// `bin/` (`atpkg::store::ToolName::exe_file`: `codex`, `codex.exe` on Windows).
pub(super) fn names_program(exe: &Path, program: &str) -> bool {
    atpkg::store::ToolName::new(program)
        .is_some_and(|tool| exe.file_name() == Some(std::ffi::OsStr::new(&tool.exe_file())))
}

/// The build a managed twin `twin` of `program` execs ([`super::twin::twin_binary`]: a `sh` twin's
/// body, or on Windows a `.cmd` twin's), named by its executable's name ([`names_program`]) and a
/// file.
pub(super) fn twin_build(twin: &Path, program: &str) -> Option<PathBuf> {
    let text = crate::harness::accounts::read_bounded(twin, 64 * 1024)
        .ok()
        .flatten()?;
    let exe = super::twin::twin_binary(&text)?;
    (names_program(&exe, program) && exe.is_file()).then_some(exe)
}

/// `twin` when it execs `program`'s own build ([`twin_build`]).
pub(super) fn twin_naming(twin: &Path, program: &str) -> Option<PathBuf> {
    twin_build(twin, program).map(|_| twin.to_path_buf())
}

/// `program`'s managed twin ([`twin_naming`]) in the `agents/` directory `agents`, where atpkg
/// lays it (`atpkg::store::Layout::agent_shim`: `agents/<program>`, `agents\<program>.cmd` on
/// Windows).
pub(super) fn agent_twin_in(agents: &Path, program: &str) -> Option<PathBuf> {
    let tool = atpkg::store::ToolName::new(program)?;
    twin_naming(&agents.join(tool.shim_file()), program)
}

/// `program`'s managed twin ([`agent_twin_in`]) in the configured atpkg layout's `agents/`.
pub(super) fn managed_agent_twin(program: &str) -> Option<PathBuf> {
    agent_twin_in(&agents_dir()?, program)
}

/// Whether `exe` is where atpkg lays `program`'s managed twin in the configured layout's
/// `agents/`, whether or not the build it execs can be read ([`managed_agent_twin`] reads it).
pub(super) fn is_managed_twin_path(exe: &Path, program: &str) -> bool {
    agents_dir()
        .zip(atpkg::store::ToolName::new(program))
        .is_some_and(|(agents, tool)| agents.join(tool.shim_file()) == exe)
}

#[cfg(not(test))]
fn agents_dir() -> Option<PathBuf> {
    Some(atpkg::store::resolve_configured()?.agents_dir())
}

#[cfg(test)]
thread_local! {
    pub(super) static AGENTS_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn agents_dir() -> Option<PathBuf> {
    AGENTS_DIR.with(|dir| dir.borrow().clone())
}

pub(super) fn twin_version(twin: &Path) -> Option<String> {
    let text = crate::harness::accounts::read_bounded(twin, 64 * 1024)
        .ok()
        .flatten()?;
    let exe = super::twin::twin_binary(&text)?;
    store_version(&exe)
}

/// The bound of a build's start probe ([`super::upgrade_build_start`]), the one place a target
/// build is executed before a session moves onto it.
#[cfg(not(test))]
pub(super) const VERSION_WAIT: Duration = Duration::from_secs(5);
#[cfg(test)]
pub(super) const VERSION_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(super) fn bounded_output(
    program: &Path,
    args: &[&std::ffi::OsStr],
    limit: Duration,
    cap: u64,
) -> Option<String> {
    let cap = usize::try_from(cap).ok()?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(args);
    let out = super::align::capture_bounded(cmd, limit, cap.checked_add(1)?).ok()?;
    if !out.success() || out.stdout.len() > cap {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

pub(super) fn native_root(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// `program`'s managed twin under `layout`, where atpkg lays it
/// (`atpkg::store::Layout::agent_shim`: `agents/<program>`, `agents\<program>.cmd` on Windows):
/// the twin itself where `program` is not dev-linked, else only while the build it execs
/// ([`super::twin::twin_binary`]) is in the store. The native half ([`Targets::read_with`]) reads the
/// `~/.local/bin/claude` link alone, not Windows' `claude.exe`.
fn managed_twin(layout: &atpkg::store::Layout, program: &str) -> Option<PathBuf> {
    let twin = layout.agent_shim(&atpkg::store::ToolName::new(program)?);
    if !atpkg::linkmode::is_linked(layout, program) {
        return Some(twin);
    }
    let text = crate::harness::accounts::read_bounded(&twin, 64 * 1024)
        .ok()
        .flatten()?;
    let exe = super::twin::twin_binary(&text)?;
    exe.starts_with(layout.prefix.join("store")).then_some(twin)
}

pub(super) struct Targets {
    managed: Option<Candidate>,
    native: Option<Candidate>,
    /// A managed twin lies there and the build it execs was not read ([`TargetRead::Unread`]).
    /// The managed build is every session's target ([`Targets::for_session`]), so no session
    /// is known current while this holds.
    unanswered: bool,
}

impl Targets {
    /// Cache the catalog baked into each target build under `<state>/upgrade/builds`
    /// ([`super::switch_top::baked_cached`], one scan per build), where a session's model card
    /// and effort slider read it ([`super::switch_top::catalog`]). A dry run writes nothing; a
    /// cache that cannot be written is left for the next sweep.
    fn cache_catalogs(&self, opts: &Opts) {
        if opts.dry_run {
            return;
        }
        let Ok(dir) = ensure_upgrade_tree(&opts.state, Some("builds")) else {
            return;
        };
        let managed = self
            .managed
            .as_ref()
            .and_then(|m| twin_build(&m.exe, "claude"));
        let native = self.native.as_ref().map(|n| n.exe.clone());
        for build in managed.into_iter().chain(native) {
            let _ = super::switch_top::baked_cached(&build, &dir);
        }
    }

    /// The targets under `opts.home`, each read from where its build lies and none executed
    /// ([`managed_target`], [`claude_build_version`]). Nothing is remembered between reads: a
    /// read that failed is read again by the next look. The native target is the file
    /// `~/.local/bin/claude` resolves to under [`native_root`], which Claude Code's installer
    /// names by its version: a link that resolves to nothing there, or to a file of another
    /// name (`latest`, a download in flight), is no native target, and that is an answer.
    pub(super) fn read(opts: &Opts) -> Targets {
        Targets::read_with(
            atpkg::store::resolve_configured().and_then(|layout| managed_twin(&layout, "claude")),
            &opts.home,
        )
    }

    fn read_with(twin: Option<PathBuf>, home: &Path) -> Targets {
        let mut unanswered = false;
        let managed = twin.and_then(|twin| match managed_target(&twin) {
            TargetRead::Version(version) => Some(Candidate {
                exe: twin,
                version,
                source: Source::Managed,
            }),
            TargetRead::Absent => None,
            TargetRead::Unread(why) => {
                aterm_log::debug!("upgrade target unread twin={} why={why}", twin.display());
                unanswered = true;
                None
            }
        });
        let native = std::fs::canonicalize(home.join(".local/bin/claude"))
            .ok()
            .filter(|target| target.starts_with(native_root(home)))
            .and_then(|target| {
                let Some(version) = claude_build_version(&target) else {
                    aterm_log::debug!(
                        "upgrade no native target: {} names no version",
                        target.display()
                    );
                    return None;
                };
                Some(Candidate {
                    exe: target,
                    version,
                    source: Source::Native,
                })
            });
        Targets {
            managed,
            native,
            unanswered,
        }
    }

    /// What a visit says of a session none of these targets is ahead of: `current`, or, while
    /// the managed build is unread, `wait:no-version` ([`UNREAD_VERSION`]), because a session
    /// behind a build nobody read is not known to be current.
    fn untargeted_word(&self) -> String {
        if self.unanswered {
            format!("wait:{UNREAD_VERSION}")
        } else {
            "current".to_string()
        }
    }

    pub(super) fn for_session(&self, running_native: bool) -> Vec<Candidate> {
        self.managed
            .iter()
            .chain(self.native.iter().filter(|_| running_native))
            .cloned()
            .collect()
    }
}

/// What a read of the managed twin's build gave ([`managed_target`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetRead {
    /// No managed copy is installed: no twin lies there.
    Absent,
    /// The build the twin execs.
    Version(Version),
    /// A twin lies there and its build was not read; the word says where the read stopped.
    /// Never an answer that there is no version.
    Unread(&'static str),
}

/// The build the managed twin `twin` execs, read with nothing executed: the twin's own last line
/// names the store build ([`super::twin::twin_binary`]), and the store names its version
/// ([`claude_build_version`]).
fn managed_target(twin: &Path) -> TargetRead {
    match std::fs::metadata(twin) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return TargetRead::Absent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return TargetRead::Absent,
        Err(_) => return TargetRead::Unread("twin-unreadable"),
    }
    let Some(text) = crate::harness::accounts::read_bounded(twin, 64 * 1024)
        .ok()
        .flatten()
    else {
        return TargetRead::Unread("twin-unreadable");
    };
    let Some(exe) = super::twin::twin_binary(&text) else {
        return TargetRead::Unread("twin-names-no-build");
    };
    if !exe.is_file() {
        return TargetRead::Unread("build-missing");
    }
    claude_build_version(&exe).map_or(TargetRead::Unread("build-unversioned"), TargetRead::Version)
}

pub(super) type Client = RelayClient<aterm_uds::CtlStream>;

pub(super) fn connect(opts: &Opts, sid: &str) -> Result<Client, String> {
    let sock = match &opts.sock {
        Some(s) => s.clone(),
        None => aterm_ctl::resolve_sock_for(Some(sid)).map_err(|e| format!("{e}"))?,
    };
    connect_at(opts, &sock)
}

pub(super) fn connect_at(opts: &Opts, sock: &str) -> Result<Client, String> {
    let token = opts
        .token
        .as_ref()
        .filter(|_| opts.sock.as_deref() == Some(sock));
    let c = harness_client_within(sock, token, opts.timings.drive_reply)
        .map_err(|e| format!("{sock}: {e}"))?;
    Ok(if opts.dry_run { c.reads_only() } else { c })
}

/// The window harness's in-memory control token, which reads `by=harness` on the window's own
/// socket. Its `Debug` never prints it, so no `Opts` dump logs it.
#[derive(Clone, PartialEq, Eq)]
pub struct HarnessToken(String);

impl HarnessToken {
    /// The token `token`, trimmed.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self(token.trim().to_string())
    }
}

impl std::fmt::Debug for HarnessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HarnessToken(redacted)")
    }
}

/// A connection to `sock` that presents `token`, probed with `version`. A token the server
/// refuses (`ERR auth`, or a connection closed before any probe reply) is replaced once by the token
/// beside the socket, every client's: a seamless successor that rebound the socket knows only
/// its own. `None` presents the token beside the socket, unprobed. Every exchange on it, the
/// probe first, is bounded by the build's [`super::Timings::drive_reply`]
/// ([`harness_client_within`]).
/// # Errors
/// The socket could not be reached, the token beside it read, or the probe's reply read.
/// A partial or malformed reply is an error, never evidence for authentication fallback.
pub fn harness_client(sock: &str, token: Option<&HarnessToken>) -> std::io::Result<Client> {
    harness_client_within(sock, token, super::Timings::for_build().drive_reply)
}

/// [`harness_client`] with each validated request/reply exchange bounded by `wait`
/// ([`RelayClient::bounded`]). An unanswered probe or later request is an error, and the
/// connection refuses every request after it ([`RelayClient::reply_unread`]). A caller reads that
/// as the socket's silence and asks again at its next step, on a new connection: a carry that
/// typed nothing waits (`wait:no-socket`), a typed line left unanswered is read for what it did
/// ([`Typed::Unanswered`]), and nothing is recorded as the server's answer. Socket dialing,
/// token-file reading and the initial AUTH write remain separate connection setup; this is not
/// a measured total connection-establishment bound.
/// # Errors
/// As [`harness_client`], and a probe unanswered within `wait`.
pub fn harness_client_within(
    sock: &str,
    token: Option<&HarnessToken>,
    wait: Duration,
) -> std::io::Result<Client> {
    let beside = || RelayClient::connect_local_beside(sock).map(|c| c.bounded(wait));
    let Some(token) = token else {
        return beside();
    };
    probe_client(
        RelayClient::connect_local(sock, &token.0)?.bounded(wait),
        beside,
    )
}

fn probe_client(
    mut c: Client,
    beside: impl FnOnce() -> std::io::Result<Client>,
) -> std::io::Result<Client> {
    // Classify the peer before error cleanup shuts our end: local shutdown can
    // imitate peer closure and discard the queued bytes that forbid auth recovery.
    // The failed client remains unread/unsendable and is dropped after classification.
    let hang_up = c.hang_up;
    c.hang_up = |_| {};
    let probe = c.request_counted("version");
    c.hang_up = hang_up;
    match probe {
        Ok((head, _)) if head == "ERR auth" => {
            drop(c);
            beside()
        }
        Ok(_) => Ok(c),
        // An empty EOF has already inspected the reply stream. Truncated replies
        // have set reply_started and cannot authorize a second authentication.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            if c.closed_before_reply() {
                drop(c);
                beside()
            } else {
                Err(e)
            }
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) || (e.kind() == std::io::ErrorKind::NotConnected
                && e.raw_os_error().is_some()) =>
        {
            if !c.closed_before_reply() {
                return Err(e);
            }
            #[cfg(unix)]
            let ended = match aterm_uds::hangup::peer_state(c.transport()) {
                aterm_uds::hangup::PeerState::Gone => true,
                aterm_uds::hangup::PeerState::HalfClosed => {
                    e.kind() != std::io::ErrorKind::NotConnected
                }
                // Readable can mean queued bytes or a pending error. Neither
                // certifies an empty reply, and an idle peer could park a read.
                aterm_uds::hangup::PeerState::Readable | aterm_uds::hangup::PeerState::Idle => {
                    false
                }
            };
            #[cfg(not(unix))]
            let ended = aterm_uds::hangup::peer_closed(c.transport());
            if ended && c.closed_peer_without_reply() {
                drop(c);
                beside()
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

pub(super) fn roster_connection(sock: &str) -> Option<Client> {
    RelayClient::connect_local_beside(sock)
        .ok()
        .map(|c| c.bounded(CLAIMS_WAIT).reads_only())
}

/// A read-only [`connect`] for the claims and roster reads, its probe bounded as they are
/// ([`CLAIMS_WAIT`]).
pub(super) fn connect_reads(opts: &Opts, sid: &str) -> Result<Client, String> {
    connect(
        &Opts {
            dry_run: true,
            timings: super::Timings {
                drive_reply: opts.timings.drive_reply.min(CLAIMS_WAIT),
                ..opts.timings
            },
            ..opts.clone()
        },
        sid,
    )
}

pub(super) fn roster(c: &mut Client) -> Vec<String> {
    let Ok((head, body)) = c.request_counted("sessions") else {
        return Vec::new();
    };
    if !crate::is_ok_reply(&head) {
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

pub(super) fn roster_aside(c: &mut Client, agents_off: &[aterm_phase::Program]) -> Option<Aside> {
    roster_aside_within(c, agents_off, CLAIMS_WAIT)
}

fn roster_aside_within(
    c: &mut Client,
    agents_off: &[aterm_phase::Program],
    wait: Duration,
) -> Option<Aside> {
    c.bound_by(wait);
    aside_read(c, agents_off)
}

/// The host roster's [`Aside`] read on `c` with its bound as it stands: a click's re-read of its
/// tab's claim on the hand's own connection, just before the signal (`super::relaunch`), where a
/// claims bound would cut the waits that connection makes after.
pub(super) fn aside_read(c: &mut Client, agents_off: &[aterm_phase::Program]) -> Option<Aside> {
    let (head, body) = c.request_counted("sessions").ok()?;
    if !crate::is_ok_reply(&head) {
        return None;
    }
    let mut aside = Aside::default();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        let Some(sid) = words.nth(1).filter(|s| s.starts_with("s-")) else {
            continue;
        };
        let (mut holder, mut autopilot, mut program) = (None, None, None);
        for word in words.skip(3) {
            if let Some(h) = word.strip_prefix("supervisor=") {
                holder = holder.or(Some(h));
            } else if let Some(a) = word.strip_prefix("autopilot=") {
                autopilot = autopilot.or(Some(a));
            } else if let Some(p) = word.strip_prefix("program=") {
                program = program.or(Some(p));
            }
        }
        if program
            .and_then(aterm_phase::program_of)
            .is_some_and(|p| agents_off.contains(&p))
        {
            aside.agent_off.insert(sid.to_string());
        }
        if holder.is_some_and(|h| h != "-" && !is_host_holder(&aterm_codec::pct::decode(h))) {
            aside.claimed.insert(sid.to_string());
        }
        if autopilot == Some("off") {
            aside.off.insert(sid.to_string());
        }
    }
    Some(aside)
}

pub(super) fn aside_of(
    clients: impl IntoIterator<Item = Client>,
    agents_off: &[aterm_phase::Program],
) -> Aside {
    aside_of_within(clients, agents_off, CLAIMS_WAIT)
}

/// [`aside_of`], each instance's roster read given `wait` in place of [`CLAIMS_WAIT`].
fn aside_of_within(
    clients: impl IntoIterator<Item = Client>,
    agents_off: &[aterm_phase::Program],
    wait: Duration,
) -> Aside {
    clients
        .into_iter()
        .filter_map(|mut c| roster_aside_within(&mut c, agents_off, wait))
        .fold(Aside::default(), |mut all, one| {
            all.claimed.extend(one.claimed);
            all.off.extend(one.off);
            all.agent_off.extend(one.agent_off);
            all
        })
}

const CLAIMS_WAIT: Duration = Duration::from_secs(2);

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
    if head.split_whitespace().count() != 2 {
        return None;
    }
    let count = crate::reply_count(head).ok()??;
    let rows = roster_rows(body)?;
    (rows.len() == count).then(|| rows.into_iter().map(|(_, tab)| tab).collect())
}

pub(crate) fn roster_rows(body: &str) -> Option<Vec<(u64, LiveTab)>> {
    let mut rows: Vec<(u64, LiveTab)> = Vec::new();
    for row in body.lines() {
        let mut words = row.split_whitespace();
        let local = crate::parse_uint::<u64>(words.next()?)?;
        let sid = words.next()?.to_string();
        if !sid.starts_with("s-") || rows.iter().any(|(_, t)| t.sid == sid) {
            return None;
        }
        let fgpgid = row
            .split_whitespace()
            .find_map(|word| word.strip_prefix("fgpgid="))
            .and_then(crate::parse_uint::<i64>)
            .filter(|&group| group > 0);
        rows.push((local, LiveTab { sid, fgpgid }));
    }
    Some(rows)
}

fn tab_is_live(tabs: &[LiveTab], sid: &str) -> bool {
    tabs.iter().any(|tab| tab.sid == sid)
}

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

pub(super) struct Screen {
    pub(super) rows: Vec<String>,
    pub(super) cursor: Option<(usize, usize)>,
    pub(super) seq: u64,
    pub(super) first: usize,
    pub(super) generation: Option<String>,
    pub(super) human: HumanInput,
    /// The program's title read with `rows` (`"title"`); `None` where the host sent no such string.
    /// Claude Code's `◐`/`◑` there runs a turn its screen may not show ([`turn_shown`]).
    pub(super) title: Option<String>,
    /// The person's input count the host took with `rows` (`"human_seq"`), which a turn fences on
    /// (`turn if-human=`): a key since that moves no fenced row stops it. `None` where the host sent
    /// none.
    pub(super) human_seq: Option<u64>,
}

pub(super) fn screen(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(c, &format!("@{sid} text --json"))
}

const CONTINUATION_TAIL_ROWS: usize = 20;

fn screen_tail(c: &mut Client, sid: &str) -> Option<Screen> {
    screen_query(
        c,
        &format!("@{sid} text --json tail={CONTINUATION_TAIL_ROWS}"),
    )
}

fn screen_query(c: &mut Client, request: &str) -> Option<Screen> {
    let (head, body) = c.request_counted(request).ok()?;
    if !crate::is_ok_reply(&head) {
        return None;
    }
    parse_screen(if body.trim().is_empty() {
        head.strip_prefix("OK ")?
    } else {
        &body
    })
}

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
    let human = match v.get("human_ms") {
        None => HumanInput::Unknown,
        Some(Value::Null) => HumanInput::never(v.get("unkeyed_ms").and_then(Value::as_u64)),
        Some(h) => h.as_u64().map_or(HumanInput::Unknown, HumanInput::Ago),
    };
    Some(Screen {
        rows,
        cursor,
        seq,
        first: usize::try_from(first).ok()?,
        generation,
        human,
        title: v.get("title").and_then(Value::as_str).map(str::to_owned),
        human_seq: v.get("human_seq").and_then(Value::as_u64),
    })
}

fn is_generation(g: &str) -> bool {
    ScreenGen::parse(g).is_some()
}

pub(super) fn composer_empty(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    let dim = dim_placeholder(c, sid, scr);
    upgrade::composer_is_empty(&scr.rows, scr.cursor, dim)
}

pub(super) fn dim_placeholder(c: &mut Client, sid: &str, scr: &Screen) -> bool {
    dim_cell(scr).is_some_and(|at| dim_at(c, sid, at))
}

/// Whether the composer's text on screen row `row` is drawn dim: the cell of its first glyph, or
/// of the glyph after it where the program draws its caret over the first (the loop's read,
/// [`crate::supervise::screen::composer_glyph_cell`]).
fn dim_at(c: &mut Client, tab: &str, row: usize) -> bool {
    crate::supervise::screen::composer_glyph_cell(|col| {
        c.request_line(&format!("@{tab} cell {row} {col}")).ok()
    })
    .is_some_and(|line| cell_is_dim(&line))
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    Gone,
    Kept(&'static str),
}

pub(super) fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What one read of a composer that holds nothing but a start of aterm's own text says to
/// [`take_back`] ([`left_read`]).
struct Own {
    /// The backspaces that take back what the read shows of the text.
    keys: usize,
    /// The person's input count of the read (`"human_seq"`), which each backspace is fenced on.
    human_seq: Option<u64>,
    /// When the person last gave the tab input, by the read.
    human: HumanInput,
    /// The cursor stands behind the last glyph of the composer's last row: where the text's keys
    /// left it, so a backspace takes the text's own last glyph.
    caret_ends: bool,
    /// The composer opens with a whole head of aterm's ([`upgrade::own_words`]), which no person
    /// types; a shorter start may be a person's few glyphs.
    headed: bool,
}

/// How many of `text`'s characters the composer holds when it shows the first `drawn` of its
/// non-blank ones: those, the blanks among them, and the blanks the next one follows (a blank
/// typed last shows nothing). A backspace more than the composer holds takes nothing.
fn own_keys(text: &str, drawn: usize) -> usize {
    let mut seen = 0;
    text.chars()
        .take_while(|ch| {
            if !ch.is_whitespace() {
                if seen == drawn {
                    return false;
                }
                seen += 1;
            }
            true
        })
        .count()
}

fn caret_ends(scr: &Screen) -> bool {
    let Some((top, bottom)) = aterm_phase::phase::composer_rules(&scr.rows) else {
        return false;
    };
    scr.cursor.is_some_and(|(row, col)| {
        row > top
            && row.checked_add(1) == Some(bottom)
            && scr
                .rows
                .get(row)
                .is_some_and(|text| col >= text.trim_end().chars().count())
    })
}

/// What stands of `text`, a line aterm typed into Claude Code's composer and did not submit,
/// and, where the composer holds nothing but a start of it, what that read proves of it.
fn left_read(c: &mut Client, tab: &str, text: &str) -> (Left, Option<Own>) {
    let ours = compact(text);
    let mark = compact(upgrade::HARNESS_MARK);
    let Some(scr) = screen(c, tab) else {
        return (Left::Kept("screen-unreadable"), None);
    };
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return (Left::Kept("box"), None);
    }
    let draft = aterm_phase::phase::composer_rules(&scr.rows)
        .and_then(|_| aterm_phase::phase::composer_draft(&scr.rows));
    let Some((_, lines)) = draft else {
        return (Left::Kept("no-composer"), None);
    };
    if composer_empty(c, tab, &scr) {
        return (Left::Gone, None);
    }
    let shown = lines.concat();
    let draft = compact(&shown);
    if ours.starts_with(&draft) {
        let own = Own {
            keys: own_keys(text, draft.chars().count()),
            human_seq: scr.human_seq,
            human: scr.human,
            caret_ends: caret_ends(&scr),
            headed: upgrade::own_words(&shown),
        };
        (Left::Kept("unsent"), Some(own))
    } else if draft.starts_with(&mark) {
        (Left::Kept("mixed"), None)
    } else {
        (Left::Gone, None)
    }
}

pub(super) fn left_standing(c: &mut Client, tab: &str, text: &str) -> Left {
    left_read(c, tab, text).0
}

/// How long the person must have left the tab alone before [`take_back`] presses on the word of a
/// read alone ([`Unkeyed::For`]): the hang detector of one echo ([`PRESSED_ECHO_HANG`]), past
/// which a key of theirs the window counted is drawn where the read would show it.
const TAKE_BACK_QUIET_S: u32 = 60;

/// How many times [`take_back`] reads the composer and presses: keys of the text still undrawn
/// at one read are taken back at the next.
const TAKE_BACK_LOOKS: usize = 3;

/// What proves no key of a person's stands among aterm's words in the composer ([`take_back`]).
#[derive(Clone, Copy)]
pub(super) enum Unkeyed {
    /// The person's input count the text's own turn was fenced on (`turn if-human=`), read over
    /// a composer that held nothing: while it is still the count, every glyph typed since is
    /// the turn's, however short what stands is.
    Since(u64),
    /// The person gave the tab no input for this many seconds (their grace), and for
    /// [`TAKE_BACK_QUIET_S`]: what stands is aterm's where it opens with a whole head of
    /// aterm's, and no hand, hold or unread input is on the tab.
    For(u32),
}

/// Whether the host's `key` fences on the person (`if-human=`), as [`turn_takes_human`].
fn key_takes_human(c: &mut Client) -> bool {
    c.help_counted("key")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-human="))
}

/// ATERM TAKES BACK WHAT IT TYPED AND COULD NOT SUBMIT. A turn the host stopped after its keys
/// ([`Typed::Cut`]) leaves a start of `text` in Claude Code's composer, where it reads as a
/// person's draft, holds every later line and is nobody's to send. This backspaces it out, and
/// never a glyph of a person's: it presses only while one read shows a composer and no box,
/// nothing in the composer but a start of `text`, and the cursor behind its last glyph
/// ([`Own::caret_ends`]); only under `proof` that no person's key is among it; and each
/// backspace is fenced on that read's count of the person's input (`key if-human=`), under the
/// window's own lock, so a key of theirs since stops it before it writes. A press the host
/// refuses ends it, and a look that took nothing back ends it. What a person typed over, after
/// or among aterm's words is theirs ([`Left::Kept`]`("mixed")`), and stays. Each backspace is
/// `lane`'s write, the lane that typed `text`, and names it ([`lane_words`]): the one observe
/// gate takes it where it took the text.
pub(super) fn take_back(
    c: &mut Client,
    tab: &str,
    text: &str,
    proof: Unkeyed,
    authority: Option<(super::contract::Scope, u64)>,
    every: Duration,
    lane: Lane,
) -> Left {
    let mut pressed: Option<usize> = None;
    for _ in 0..TAKE_BACK_LOOKS {
        let (left, own) = left_read(c, tab, text);
        let Some(own) = own else {
            return left;
        };
        let Some(human) = own.human_seq.filter(|_| own.caret_ends) else {
            return left;
        };
        if pressed.is_some_and(|before| own.keys >= before) {
            return left;
        }
        let unkeyed = match proof {
            Unkeyed::Since(fenced) => fenced == human && turn_takes_human(c),
            Unkeyed::For(grace_s) => {
                own.headed
                    && !upgrade::attended_by(own.human, grace_s.max(TAKE_BACK_QUIET_S))
                    && !held(c, tab, grace_s)
            }
        };
        if !unkeyed || !key_takes_human(c) {
            return left;
        }
        let Ok(authority) = authority_words(c, tab, authority) else {
            return left;
        };
        let lane = lane_words(c, "key", lane);
        let request = format!("@{tab} key {lane}{authority}if-human={human} backspace");
        for _ in 0..own.keys {
            let went = c.request_line(&request).is_ok_and(|reply| {
                crate::is_ok_reply(&reply) && !reply.split_whitespace().any(|w| w == "skipped")
            });
            if !went {
                return left_standing(c, tab, text);
            }
        }
        pressed = Some(own.keys);
        std::thread::sleep(every);
    }
    left_standing(c, tab, text)
}

fn left_cut(st: &mut St, c: &mut Client, tab: &str, text: &str) {
    st.left_typed = text.to_string();
    let _ = settle_left(st, c, tab, left_standing);
}

fn settle_left(
    st: &mut St,
    c: &mut Client,
    tab: &str,
    standing: fn(&mut Client, &str, &str) -> Left,
) -> Left {
    let left = standing(c, tab, &st.left_typed);
    if left == Left::Gone {
        st.left_typed.clear();
    }
    left
}

/// What stands of a line aterm typed and could not submit ([`St::left_typed`]) is settled before
/// anything else is typed: aterm takes its own words back where a read proves them its own and
/// the person away ([`take_back`], [`Unkeyed::For`]), and the step waits on whatever still
/// stands (`wait:left-typed:<why>`).
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
    let mut left = settle_left(st, c, tab, left_standing);
    if left == Left::Kept("unsent") && !opts.dry_run {
        let text = st.left_typed.clone();
        left = under_hand(c, tab, None, |c| {
            take_back(
                c,
                tab,
                &text,
                Unkeyed::For(opts.human_grace_s),
                None,
                opts.timings.fence_every,
                // What `left_typed` records is a line of the live upgrade's ([`left_cut`]).
                Lane::Upgrade,
            )
        })
        .unwrap_or(left);
        if left == Left::Gone {
            st.left_typed.clear();
            ledger(
                opts,
                &said(r.clone(), "left-typed-cleared"),
                "a line aterm typed into the composer and could not submit stood there unsent: \
                 aterm took its own words back, a backspace for each, with nothing of a \
                 person's among them and no input of theirs since",
            );
            save(opts, &sf.session_id, st);
        }
    }
    let r = match left {
        Left::Gone => return None,
        Left::Kept(why) => said(r.clone(), format!("wait:left-typed:{why}")),
    };
    st.note_step(&r.step, &sf.status, now);
    save(opts, &sf.session_id, st);
    Some(r)
}

pub(super) fn held(c: &mut Client, sid: &str, grace_s: u32) -> bool {
    status_line(c, sid).is_none_or(|line| held_by_status(&line, grace_s))
}

/// [`held`]'s one `status` read, and what it says of the person's hour: `None` where the tab is
/// held, else whether their last key holds the go-ahead
/// ([`super::relaunch::go_ahead_held_by_status`]), which a carried done check then leaves unsaid.
pub(super) fn unheld_hour(c: &mut Client, sid: &str, grace_s: u32) -> Option<bool> {
    let line = status_line(c, sid)?;
    (!held_by_status(&line, grace_s)).then(|| super::relaunch::go_ahead_held_by_status(&line))
}

pub(super) fn status_line(c: &mut Client, sid: &str) -> Option<String> {
    c.request_line(&format!("@{sid} status")).ok()
}

/// What [`step`] answers for a tab whose agent supervises itself under a proven self-claim ([`self_claimed_by_status`]).
pub const SKIP_SELF_CLAIMED: &str = "skip:self-claimed";

/// Whether `status` says a proven claim stands (`supervisor_proven=1`): codex-alab's v2
/// self-claim, or a rostered harness's contract v3 scoped claim. `status` carries no scope, so a
/// scoped claim stands this lane down whatever its scope (`docs/DESIGN-harness-repos-2026-10-24.md`
/// §11 D29, 2026-10-25 amendment): two actors never move one tab, and the only rostered row
/// (grok-alab) always claims `upgrade` for a program this lane has no upgrade for. A row whose
/// scope may leave `upgrade` to aterm, for a program this lane upgrades, puts the scope on
/// `status` first.
#[must_use]
pub(super) fn self_claimed_by_status(line: &str) -> bool {
    crate::is_ok_reply(line) && line.split_whitespace().any(|t| t == "supervisor_proven=1")
}

pub(super) fn held_by_status(line: &str, grace_s: u32) -> bool {
    !crate::is_ok_reply(line)
        || super::relaunch::hand_on_tab(line)
        || input_pending(line)
        || super::relaunch::person_present(line, grace_s)
}

/// Whether one `status` reply says the tab holds input its program has not read (`input=`).
pub(super) fn input_pending(line: &str) -> bool {
    line.split_whitespace().any(|t| {
        t.strip_prefix("input=")
            .is_some_and(|input| !matches!(input, "-" | "clear"))
    })
}

/// How long a hand lasts unless renewed (`lease acquire ttl=`).
pub(crate) const HAND_TTL_MS: u64 = 60_000;

/// How often a holder renews its hand: well inside [`HAND_TTL_MS`], so a
/// renewal a starved holder makes late still lands before the hand lapses.
pub(crate) const HAND_RENEW: Duration = Duration::from_secs(20);

/// The holder name of this process's hand ([`Hand`]).
#[must_use]
pub fn hand_name() -> String {
    hand_name_of(std::process::id())
}

/// The holder name of process `pid`'s hand: aterm's built-in host in that aterm (`status
/// supervisor=aterm-harness@<pid>` while its own lanes hold a tab).
#[must_use]
pub fn hand_name_of(pid: u32) -> String {
    format!("{HOST_HOLDER}{pid}")
}

const HOST_HOLDER: &str = "aterm-harness@";

/// The request that takes (or renews) this process's hand on a tab, without its `@<tab>` selector.
#[must_use]
pub fn hand_take_request() -> String {
    format!(
        "lease acquire ttl={HAND_TTL_MS} holder={} hard",
        hand_name()
    )
}

/// The request that gives this process's hand back.
#[must_use]
pub fn hand_give_back_request() -> String {
    format!("lease release holder={}", hand_name())
}

/// [`hand_take_request`] kept to the asking connection (`lease acquire … here`).
#[must_use]
pub fn hand_take_here_request() -> String {
    hand_take_here_request_as(&hand_name())
}

/// [`hand_take_here_request`] under the hand name `holder`.
#[must_use]
pub fn hand_take_here_request_as(holder: &str) -> String {
    format!("lease acquire ttl={HAND_TTL_MS} holder={holder} hard here")
}

/// [`hand_give_back_request`] kept to the asking connection (`lease release … here`, [`hand_take_here_request`]'s pair).
#[must_use]
pub fn hand_give_back_here_request() -> String {
    hand_give_back_here_request_as(&hand_name())
}

/// [`hand_give_back_here_request`] under the hand name `holder`.
#[must_use]
pub fn hand_give_back_here_request_as(holder: &str) -> String {
    format!("lease release holder={holder} here")
}

/// Whether a claim's holder (percent-decoded) is a window's own host.
#[must_use]
pub fn is_host_holder(holder: &str) -> bool {
    holder.starts_with(HOST_HOLDER)
}

#[must_use]
pub(super) fn our_hand(hand: &str) -> bool {
    hand.strip_prefix("lease:") == Some(hand_name().as_str())
}

#[derive(Debug)]
pub(super) struct Hand {
    tab: String,
    held: bool,
    renewed: Instant,
}

impl Hand {
    pub(super) fn take(c: &mut Client, tab: &str) -> Option<Hand> {
        let reply = c.request_line(&format!("@{tab} {}", hand_take_request()));
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

    pub(super) fn take_or_none(c: &mut Client, tab: &str) -> Hand {
        Hand::take(c, tab).unwrap_or_else(|| Hand {
            tab: tab.to_string(),
            held: false,
            renewed: Instant::now(),
        })
    }

    pub(super) fn keep(&mut self, c: &mut Client) {
        if self.held && self.renewed.elapsed() >= HAND_RENEW {
            self.held = Hand::take(c, &self.tab).is_some_and(|h| h.held);
            self.renewed = Instant::now();
        }
    }

    pub(super) fn give_back(&mut self, c: &mut Client) {
        if std::mem::take(&mut self.held) {
            let _ = c.request_line(&format!("@{} {}", self.tab, hand_give_back_request()));
        }
    }
}

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

pub(super) fn quiet_s(status: Option<&str>, st: &mut St, seq: u64, now: u64) -> u64 {
    if seq != st.last_seq {
        st.last_seq = seq;
        st.seq_since_s = now;
    }
    let field = |key: &str| status?.split_whitespace().find_map(|t| t.strip_prefix(key));
    let since_ms = field("agent_since_ms=").and_then(crate::parse_uint::<u64>);
    match (field("agent="), since_ms) {
        (Some("idle" | "question"), Some(ms)) => ms / 1000,
        (Some(word), Some(_)) if word != "-" && word != "unknown" => 0,
        _ => now.saturating_sub(st.seq_since_s),
    }
}

/// The `harness-lane=<word> ` a write of `lane`'s names itself by (THE ONE OBSERVE GATE weighs
/// it, `agent_image::withholds`), where the host's `help <verb>` names the option: a host older
/// than the gate would read the word as the write's text.
fn lane_words(c: &mut Client, verb: &str, lane: Lane) -> String {
    let named = c.help_counted(verb).is_ok_and(|(head, body)| {
        crate::is_ok_reply(&head) && (head + &body).contains(agent_image::LANE_OPTION)
    });
    if named {
        format!("{}{} ", agent_image::LANE_OPTION, lane.word())
    } else {
        String::new()
    }
}

/// Type `line` and Enter into tab `sid` as `lane`'s write ([`lane_words`]).
pub(super) fn type_line(c: &mut Client, sid: &str, line: &str, lane: Lane) -> Result<(), Typed> {
    let send = lane_words(c, "send", lane);
    let key = lane_words(c, "key", lane);
    for request in [
        format!("@{sid} send {send}-- {line}"),
        format!("@{sid} key {key}enter"),
    ] {
        let reply = c.request_line(&request).map_err(|e| Typed::of_io(&e))?;
        if !crate::is_ok_reply(&reply) {
            return Err(Typed::Refused(reply));
        }
    }
    Ok(())
}

/// A read the window left unanswered, or a connection it never gave ([`RelayClient::reply_unread`]):
/// asked again at the next step (`wait:no-socket`), never read as what the window would say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NoSocket;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Typed {
    Changed,
    Yielded,
    /// The person made a gesture since the read it was fenced on (`if-human=`), and nothing of it
    /// was put: the person's grace is the caller's to wait out before it types again.
    Person,
    Cut(String),
    Refused(String),
    /// Its reply did not come within its wait ([`crate::unanswered`]): the server may still type
    /// it, so a caller reads what it did before it decides, never a refusal.
    Unanswered,
}

impl Typed {
    pub(super) fn word(&self) -> String {
        match self {
            Typed::Changed => "changed".to_string(),
            Typed::Yielded => "yield".to_string(),
            Typed::Person => "person".to_string(),
            Typed::Cut(why) => format!("cut-{why}"),
            Typed::Refused(e) => first_word(e),
            Typed::Unanswered => "unanswered".to_string(),
        }
    }

    /// A typed request's I/O error: [`Typed::Unanswered`] for a reply past its wait, else a
    /// refusal.
    pub(super) fn of_io(e: &std::io::Error) -> Self {
        if crate::unanswered(e) {
            Typed::Unanswered
        } else {
            Typed::Refused(format!("{e}"))
        }
    }
}

pub(super) const TURN_WAIT: &str = "idle=1 timeout=10000";

fn turn_takes_gen(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-gen="))
}

fn turn_takes_cap(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("cap=<n>"))
}

const NOTICE_KEYS: &str = "typed=1 cadence=2 cap=4096 ";

/// How long one of a pressed text's echoes may lag its keys' pace before the turn's deadline
/// cuts it. The host waits before each digit, and before the guarded submit, until the agent
/// has drawn the key before it, and gives each wait back to the deadline once it ends, so a
/// starved agent that keeps drawing is never cut: this is a hang detector for one echo, never a
/// render budget nor one for their sum (a host older than that give-back holds their sum to
/// it). What a cut leaves of aterm's words in the prompt aterm takes back ([`take_back`]).
const PRESSED_ECHO_HANG: Duration = Duration::from_secs(60);

/// The `idle= timeout=` words of a turn that types `text` with `keys` ([`TURN_WAIT`] for a paste).
fn turn_wait(keys: &str, text: &str) -> String {
    if keys.is_empty() {
        return TURN_WAIT.to_string();
    }
    let value = |words: &str, key: &str| {
        words
            .split_whitespace()
            .find_map(|w| crate::parse_uint::<u64>(w.strip_prefix(key)?))
            .unwrap_or(0)
    };
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    let pressed = count(text.chars().count()).min(value(keys, "cap="));
    let echoes = u64::try_from(PRESSED_ECHO_HANG.as_millis()).unwrap_or(u64::MAX);
    let timeout = value(TURN_WAIT, "timeout=")
        .saturating_add(pressed.saturating_mul(value(keys, "cadence=")))
        .saturating_add(echoes);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Put {
    Paste,
    Pressed,
}

fn turn_takes_guard(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("guarded:"))
}

fn turn_takes_rows(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-rows="))
}

/// Whether the host's `turn` fences on the person (`if-human=`): an older one reads the word as the
/// turn's text, so it is sent only where `help turn` names it.
pub(super) fn turn_takes_human(c: &mut Client) -> bool {
    c.help_counted("turn")
        .is_ok_and(|(head, body)| crate::is_ok_reply(&head) && (head + &body).contains("if-human="))
}

/// One turn of `text`, fenced on `generation` (the screen's, or the composer's rows) and on `human`,
/// the person's input count of the same read: a key since, drawn or not, stops it. `lane` names
/// the write ([`lane_words`]).
pub(super) fn turn_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    human: Option<u64>,
    lane: Lane,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, human, Put::Paste, lane)
}

#[cfg(all(test, unix))]
pub(super) fn notice_fenced(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
) -> Result<(), Typed> {
    checked_turn(c, sid, text, generation, None, Put::Pressed, Lane::Upgrade)
}

fn checked_turn(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    human: Option<u64>,
    put: Put,
    lane: Lane,
) -> Result<(), Typed> {
    checked_turn_for_duty(c, sid, text, generation, human, put, None, lane)
}

#[allow(clippy::too_many_arguments)]
fn checked_turn_for_duty(
    c: &mut Client,
    sid: &str,
    text: &str,
    generation: Option<&str>,
    human: Option<u64>,
    put: Put,
    authority: Option<(super::contract::Scope, u64)>,
    lane: Lane,
) -> Result<(), Typed> {
    let lane = lane_words(c, "turn", lane);
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
    let person = human
        .filter(|_| turn_takes_human(c))
        .map(|n| format!("if-human={n} "))
        .unwrap_or_default();
    let keys = if put == Put::Pressed && turn_takes_cap(c) {
        NOTICE_KEYS
    } else {
        ""
    };
    let submit = if !keys.is_empty() && turn_takes_guard(c) {
        format!("submit=guarded:{} ", composer_guard('❯', text))
    } else {
        String::new()
    };
    let wait = turn_wait(keys, text);
    let authority_words = authority_words(c, sid, authority)?;
    let (head, _) = c
        .request_counted(&format!(
            "@{sid} turn {lane}{authority_words}{fence}{person}{keys}{submit}yield=0.2 {wait} {text}"
        ))
        .map_err(|e| Typed::of_io(&e))?;
    turn_verdict(&head)
}

/// The leading words that claim `authority`, a built-in duty at the revision `status` still
/// names, for one input request to `sid`; none where the request claims no duty.
fn authority_words(
    c: &mut Client,
    sid: &str,
    authority: Option<(super::contract::Scope, u64)>,
) -> Result<String, Typed> {
    let Some((duty, revision)) = authority else {
        return Ok(String::new());
    };
    let (head, body) = c
        .request_counted(&format!("@{sid} status"))
        .map_err(|e| Typed::of_io(&e))?;
    let observed = crate::supervise::BuiltinAuthority::from_status(&(head + &body), &hand_name());
    if observed.revision() != Some(revision) || !observed.keeps(duty) {
        return Err(Typed::Refused("supervisor input authority".to_string()));
    }
    Ok(format!(
        "if-supervisor={} if-supervisor-revision={revision} harness-duty={} ",
        hand_name(),
        duty.word()
    ))
}

fn turn_verdict(head: &str) -> Result<(), Typed> {
    if head.starts_with("ERR yield timeout") {
        return Err(Typed::Yielded);
    }
    if !crate::is_ok_reply(head) {
        return Err(Typed::Refused(head.to_string()));
    }
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.contains(&"skipped") {
        let field = |key: &str| words.iter().find_map(|w| w.strip_prefix(key));
        let reason = field("reason=").unwrap_or_default();
        // Keys typed or a paste put: the text stands where the turn left it.
        let typed = field("typed=")
            .and_then(crate::parse_uint::<u64>)
            .max(field("pasted=").and_then(crate::parse_uint::<u64>));
        return Err(match (reason, typed) {
            (why, Some(1..)) => {
                Typed::Cut(if why.is_empty() { "unknown" } else { why }.to_string())
            }
            ("guard", _) => Typed::Cut("guard".to_string()),
            ("changed", _) => Typed::Changed,
            ("person", _) => Typed::Person,
            ("deadline", _) => Typed::Yielded,
            _ => Typed::Refused(head.to_string()),
        });
    }
    Ok(())
}

/// Every settings document a relaunch line of `argv` in `cwd` would have Claude Code read
/// for the tap's [`super::tap::gate`]: under the host's home, the user's `settings.json` and
/// `cowork_settings.json` alike (the line sets no cowork flag of its own, and the shell it is
/// typed into may), the project's, the managed ones.
pub(super) fn tap_settings(
    opts: &Opts,
    argv: &[String],
    cwd: &str,
) -> Vec<(String, super::netwatch::Source)> {
    tap_settings_under(&opts.home, argv, cwd)
}

/// WHETHER A RELAUNCH LINE OF `argv` IN `cwd` UNDER THE USER HOME `home` FINDS ITS SETTINGS
/// ADMIT the usage tap ([`super::tap::settings_admit`] over [`tap_settings`]'s documents):
/// the one judgment the relaunch's gate and the Claude Code footer's reason for a missing
/// week share, so the footer never promises a restart the relaunch's line would not tap.
#[must_use]
pub fn tap_settings_admit(home: &Path, argv: &[String], cwd: &str) -> bool {
    let user = tap_user_settings(home);
    super::tap::settings_admit(&tap_settings_under(home, argv, cwd), Some(&user))
}

/// The name [`tap_settings`] gives the user's own `settings.json` under the user home `home`
/// (`<home>/.claude/settings.json`, as `claude_settings::sources_under` names it), the one
/// document whose statusLine the tap carries ([`super::tap::settings_line`]).
#[must_use]
pub fn tap_user_settings(home: &Path) -> String {
    home.join(".claude")
        .join("settings.json")
        .display()
        .to_string()
}

/// [`tap_settings`] under the user home `home`: every settings document a Claude Code line of
/// `argv` in `cwd` reads, for the tap's gate at a relaunch and at a launch
/// ([`super::tap::launch_value`]).
#[must_use]
pub fn tap_settings_under(
    home: &Path,
    argv: &[String],
    cwd: &str,
) -> Vec<(String, super::netwatch::Source)> {
    tap_settings_for_directory(home, None, argv, cwd)
}

/// Read the same settings chain in an observed HOME/CLAUDE_CONFIG_DIR namespace.
#[must_use]
pub fn tap_settings_for_directory(
    home: &Path,
    config: Option<&str>,
    argv: &[String],
    cwd: &str,
) -> Vec<(String, super::netwatch::Source)> {
    use crate::supervise::policy::claude_settings;
    use crate::supervise::policy::git_config::WorkerEnv;
    let Ok(mut flags) = upgrade::rewrite_argv(argv, "-") else {
        return Vec::new();
    };
    flags.truncate(flags.len().saturating_sub(2));
    let carried: Vec<String> = argv.first().cloned().into_iter().chain(flags).collect();
    let account = ["USER", "LOGNAME"].iter().filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| format!("{key}={value}"))
    });
    let worker = |cowork: bool| {
        let env = [format!(
            "{}={}",
            super::accounts::HOMEDIR_ENV,
            home.display()
        )]
        .into_iter()
        .chain(config.map(|config| format!("{}={config}", super::accounts::HOME_ENV)))
        .chain(account.clone())
        .chain(cowork.then(|| "CLAUDE_CODE_USE_COWORK_PLUGINS=1".to_string()))
        .collect();
        WorkerEnv::new(atpkg::caller_shell::ProcArgs {
            argv: carried.clone(),
            env,
            ..Default::default()
        })
    };
    let managed = managed_settings_dir();
    let mut out: Vec<(String, super::netwatch::Source)> = Vec::new();
    for cowork in [false, true] {
        let worker = worker(cowork);
        let sources =
            claude_settings::sources_under(&worker, Path::new(cwd), managed.as_deref(), Some(home))
                .into_iter()
                .chain(
                    managed_preferences_dir()
                        .map(|dir| claude_settings::managed_preferences(&worker, &dir))
                        .unwrap_or_default(),
                );
        for source in sources {
            if !out.iter().any(|(name, _)| *name == source.name) {
                out.push((source.name, source.text.into()));
            }
        }
    }
    out
}

pub(super) fn tap_profile_store(opts: &Opts) -> bool {
    tap_profile_store_at(&opts.home)
}

/// Whether the vendor's profile store under the user home `home` (or the host's
/// `XDG_CONFIG_HOME`) holds a config, which keeps the tap off ([`super::tap::GateFacts::profile_store`]).
#[must_use]
pub fn tap_profile_store_at(home: &Path) -> bool {
    tap_profile_store_in(home, host_xdg_config_home().as_deref())
}

/// [`tap_profile_store_at`] with the host's `XDG_CONFIG_HOME` passed in.
fn tap_profile_store_in(home: &Path, host_xdg: Option<&str>) -> bool {
    use super::accounts::{HOMEDIR_ENV, profile_configs_dir, profile_configured};
    let home = home.display().to_string();
    let by_home = profile_configs_dir([(HOMEDIR_ENV, home.as_str())]);
    let by_xdg = host_xdg.and_then(|xdg| profile_configs_dir([("XDG_CONFIG_HOME", xdg)]));
    by_home
        .into_iter()
        .chain(by_xdg)
        .any(|configs| profile_configured(&configs))
}

#[cfg(not(test))]
fn host_xdg_config_home() -> Option<String> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
fn host_xdg_config_home() -> Option<String> {
    None
}

#[cfg(not(test))]
fn managed_settings_dir() -> Option<PathBuf> {
    crate::supervise::policy::claude_settings::MANAGED_DIR.map(PathBuf::from)
}

#[cfg(not(test))]
fn managed_preferences_dir() -> Option<PathBuf> {
    Some(PathBuf::from(
        crate::supervise::policy::claude_settings::MANAGED_PREFERENCES_DIR,
    ))
}

#[cfg(test)]
thread_local! {
    pub(super) static MANAGED_SETTINGS: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    pub(super) static MANAGED_PREFERENCES: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn managed_settings_dir() -> Option<PathBuf> {
    MANAGED_SETTINGS.with(|dir| dir.borrow().clone())
}

#[cfg(test)]
fn managed_preferences_dir() -> Option<PathBuf> {
    MANAGED_PREFERENCES.with(|dir| dir.borrow().clone())
}

/// The resume line a person is told to run for the conversation `session` of the launch `argv`:
/// the launch's own flags, its `--model` among them, rewritten for `--resume`
/// ([`super::resume::command`]).
#[must_use]
pub fn resume_command_kept(argv: &[String], session: &str) -> Option<String> {
    if !upgrade::is_session_id(session) {
        return None;
    }
    super::resume::command(argv, session)
}

pub(super) enum Terminated {
    Sent,
    Refused(String),
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only unix sends the signal itself")
    )]
    Failed,
}

pub(super) fn terminate(c: &mut Client, tab: &str, pid: i32, grace_s: u32) -> Terminated {
    match c.request_line(&format!("@{tab} signal term pid={pid} quiet={grace_s}")) {
        Ok(reply) if crate::is_ok_reply(&reply) => Terminated::Sent,
        Ok(reply) if reply.starts_with("ERR bad signal argument: quiet=") => {
            if held(c, tab, grace_s) {
                return Terminated::Refused("held".to_string());
            }
            match c.request_line(&format!("@{tab} signal term pid={pid}")) {
                Ok(reply) if crate::is_ok_reply(&reply) => Terminated::Sent,
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

/// One row of the wait log ([`St::waits`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitNote {
    pub at: u64,
    pub word: String,
    pub note: String,
}

/// How many [`WaitNote`]s a record keeps, newest last.
pub const WAITS_KEPT: usize = 24;

#[derive(Debug)]
pub(super) struct Unsent {
    phase: Phase,
    release: String,
    marker: String,
    markers: Vec<String>,
}

pub(super) const SAME_PROGRAM: &str = "same";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct St {
    pub(super) phase: Phase,
    pub(super) from: String,
    pub(super) to: String,
    pub(super) source: String,
    pub(super) marker: String,
    pub(super) markers: Vec<String>,
    pub(super) release: String,
    pub(super) asked: Vec<String>,
    pub(super) salt: u64,
    pub(super) last_seq: u64,
    pub(super) seq_since_s: u64,
    /// The agent process the record names. It is written only with [`Self::pid_start`]
    /// ([`Self::bind`]), so its exit is judged by identity ([`Self::agent_exit`]).
    pub(super) pid: u32,
    /// The kernel start of the process [`Self::pid`] names ([`kernel_start`]'s spelling), read
    /// when the record bound it: what proves that process's exit once its pid is free or names
    /// a process born at another time (a reboot reuses pids). Empty only in a record an earlier
    /// build wrote, which takes the start of a snapshot naming its pid ([`Self::adopt_start`]).
    pub(super) pid_start: String,
    pub(super) notice_pid: u32,
    pub(super) notice_start: String,
    pub(super) shell: u32,
    pub(super) tab: String,
    pub(super) tabs: Vec<String>,
    pub(super) tabs_until: Vec<(String, u64)>,
    pub(super) line: String,
    /// Exact original native preparation and its recovered capability survive reconnects.
    pub(super) native_prepare: String,
    /// Persisted before any native dispatch; terminal receipts never restore raw fallback.
    pub(super) native_coordination: bool,
    pub(super) native_record: super::native_record::Baseline,
    /// This relaunch is a fresh plan for an agent that ended under an earlier aterm process,
    /// admitted now and proven gone by its kernel start, with no native operation unresolved
    /// ([`super::relaunch::host_ended_fresh`]): no native authority outlived that process, so
    /// the record's [`Self::native_coordination`] (kept, never cleared) gates no typed line of
    /// it. Never written to the record: it holds for this one relaunch.
    pub(super) host_ended_fresh: bool,
    /// A NotGranted retirement, never a launch grant.
    pub(super) native_retired_unissued: bool,
    /// Unconfirmed account successor; the original binding and recipe remain above.
    pub(super) native_account_candidate: Option<super::native_account_handoff::Pending>,
    /// Completed data is never an active grant; later crash entry mints a new operation.
    pub(super) native_completed_prepare: Option<Box<super::native_act::Prepare>>,
    /// The latest positively unexecuted attempt, independent of its live predecessor.
    pub(super) native_noexec_prepare: Option<Box<super::native_act::Prepare>>,
    pub(super) native_observation: Option<super::native_observe::SavedObservation>,
    pub(super) native_rollback: bool,
    pub(super) native: Option<super::native_act::Binding>,
    /// The directory [`Self::line`] runs the agent in when it carries no `cd` of its own
    /// ([`Plan`]'s `line_dir`; empty: it does, or none was read): once the shell stands elsewhere,
    /// the line is typed with a `cd` back to it ([`super::relaunch::line_at_shell`]).
    pub(super) line_dir: String,
    /// How long a person's keys hold [`Self::line`] ([`super::relaunch::line_grace`]): the grace
    /// the gate that let aterm's end of the agent go waited out (every claim aterm signals under
    /// names it), or the whole grace once a relaunched build ended on its own
    /// ([`Self::roll_back`]); `None`: none recorded (no end of aterm's, or an earlier build's
    /// record), and the line keeps the whole grace.
    pub(super) line_grace_s: Option<u32>,
    /// The input already accepted by an explicit restart; only its original host can use it.
    pub(super) line_input: Option<super::restart_stamp::Input>,
    pub(super) back_line: String,
    pub(super) prompt: Option<PromptMark>,
    pub(super) line_block: Option<u64>,
    pub(super) noted: String,
    pub(super) cause: String,
    /// Which ask of a restart in place claimed this record ([`super::relaunch::RestartAsk::word`];
    /// empty for every record no restart in place claimed): an ask of that restart meets its own
    /// record by it ([`super::relaunch::its_own`]).
    pub(super) ask: String,
    pub(super) model_before: String,
    pub(super) mark: u64,
    pub(super) launch_model: String,
    pub(super) login: Option<String>,
    /// Exact forward account attribution. Missing legacy attribution never grants execution.
    pub(super) account_intent: Option<super::accounts_pool::ExecutionIntent>,
    /// Independent account attribution of the retained rollback command.
    pub(super) back_account_intent: Option<super::accounts_pool::ExecutionIntent>,
    pub(super) home: Option<String>,
    pub(super) home_profile: Option<String>,
    pub(super) failed_start_kept: Option<FailedStart>,
    pub(super) confirm_by: u64,
    pub(super) look_word: String,
    pub(super) resumed_on: String,
    pub(super) resumed_pid: u32,
    pub(super) relaunched_pid: u32,
    pub(super) relaunched_start: String,
    pub(super) relaunched_at: u64,
    pub(super) moved_at: u64,
    pub(super) moved_login: Option<String>,
    pub(super) hold_since_s: u64,
    pub(super) hold_seen_s: u64,
    pub(super) ready_since: u64,
    pub(super) pending_since: u64,
    pub(super) proc_start: String,
    pub(super) waits: Vec<WaitNote>,
    pub(super) wait: String,
    pub(super) wait_since: u64,
    pub(super) wait_seen: u64,
    pub(super) held_by: Vec<HeldBy>,
    pub(super) idle_looks: u32,
    pub(super) idle_pid: u32,
    pub(super) idle_at: u64,
    pub(super) still_print: String,
    pub(super) still_since: u64,
    pub(super) still_at: u64,
    pub(super) request: Request,
    pub(super) request_tab: String,
    pub(super) request_at: u64,
    /// When the owner pressed the top rule's ⬆️ for this record (`0`: no press), and the build
    /// that press was for ([`Self::press_stands`]).
    pub(super) pressed_at: u64,
    pub(super) pressed_to: String,
    pub(super) outcome: String,
    pub(super) done_at: u64,
    pub(super) agent: upgrade::Agent,
    pub(super) thread: String,
    pub(super) codex_home: String,
    pub(super) codex_home_env: Option<String>,
    pub(super) codex_auth_path: Option<String>,
    pub(super) argv: Vec<String>,
    pub(super) cwd: String,
    pub(super) twin: String,
    pub(super) muse_data: String,
    pub(super) muse_env: Vec<String>,
    pub(super) agy_config: String,
    pub(super) agy_env: Vec<String>,
    pub(super) resume_argv: Vec<String>,
    pub(super) resume_home: String,
    pub(super) resume_start: String,
    pub(super) left_typed: String,
    pub(super) exited_at: u64,
    pub(super) hand_seen_s: u64,
    pub(super) failed_at: u64,
    pub(super) noticed_at: u64,
    pub(super) last_stop: String,
    pub(super) streak_why: String,
    pub(super) stop_streak: u32,
    pub(super) notices: u32,
    pub(super) own_work_at: u64,
    /// Since when the agent's own work has held this round, unbroken by a person or a limit
    /// ([`St::note_wait`]); 0 for none.
    pub(super) own_since: u64,
    /// How many times [`Self::told_to`] was told to the agent ([`upgrade::Step::Tell`]).
    pub(super) tells: u32,
    pub(super) told_at: u64,
    pub(super) told_to: String,
    /// When the restart stops waiting for the real work of the agent process
    /// [`Self::deadline_pid`] ([`upgrade::WORK_DEADLINE_S`] past the line that told it, stamped
    /// once that line's turn was taken whole); 0: none stands. It is the process's, whatever
    /// target the line named: a newer build never starts the clock again. A look that finds the
    /// hold broken voids it ([`Self::void_deadline`]).
    pub(super) deadline_at: u64,
    pub(super) deadline_pid: u32,
    /// The commands of the background waits this restart ends, each to start again after the
    /// resume ([`upgrade::Yield::Waits`]); owed to the continuation until it is typed.
    pub(super) ended_waits: Vec<String>,
    /// That this restart came at the deadline and ended the agent's work
    /// ([`upgrade::Yield::Deadline`], or [`upgrade::Yield::Outlasted`] with
    /// [`Self::ended_held_s`]), and what ran under it then as the process table named it (empty:
    /// nothing it names); owed to the continuation until it is typed.
    pub(super) ended_work: Option<String>,
    /// How long the agent's own work had held the round when a restart of a session nothing was
    /// typed into ended it ([`upgrade::Yield::Outlasted`]); 0: none, or the deadline's form.
    pub(super) ended_held_s: u64,
    pub(super) work_end_at: Vec<u64>,
    pub(super) goal: bool,
    pub(super) firsts: Vec<(String, u64)>,
    pub(super) progress_at: u64,
    pub(super) progress_key: String,
    pub(super) looked_at: u64,
    pub(super) looked_by: String,
    pub(super) point_at: u64,
    pub(super) guard: String,
    pub(super) refused: u32,
    pub(super) refused_at: u64,
    pub(super) work_seen: bool,
    pub(super) limit_until: u64,
    pub(super) limit_spend: bool,
    pub(super) quiet: bool,
    pub(super) restart_look: bool,
    /// The watch's last dry look found the move the aged wall's own ([`WALL_RESTART`],
    /// [`WALL_FRESH`]): the one look at a wall the window's loop offers a point for
    /// ([`wall_due`]). A wet step clears it ([`Self::note_step`]).
    pub(super) wall_move: bool,
    /// The relaunch's process was found on the conversation and gone before it came up, and its
    /// stop waits on its native operation's retirement: no look finds that process again, so the
    /// next step ([`super::relaunch::await_new`]) reads that end here.
    pub(super) found_gone: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retarget {
    Kept,
    InPlace,
    Anew,
    Rested,
}

pub(super) const fn retarget_how(how: Retarget, st: &St) -> &'static str {
    match (how, &st.phase) {
        (Retarget::Kept | Retarget::InPlace, Phase::Announced { .. }) => {
            "its notice and any READY still count"
        }
        (Retarget::Kept | Retarget::InPlace, Phase::Failed(_)) => "kept as it stopped",
        (Retarget::Kept | Retarget::InPlace, _) => "still pending",
        (Retarget::Anew, _) => "started over",
        (Retarget::Rested, _) => "kept stopped: that build failed to start for this conversation",
    }
}

/// How many distinct wait words [`St::firsts`] keeps.
pub const FIRSTS_KEPT: usize = 32;

/// How often a look that sees the agent's own work restamps [`St::own_work_at`].
pub const OWN_WORK_RESTAMP_S: u64 = upgrade_status::OWN_WORK_HOLD_S / 4;

/// How often a look that finds a hand on a restart's tab RESTAMPS [`St::hand_seen_s`].
pub const HAND_RESTAMP_S: u64 = STALE_S / 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FailedStart {
    to: String,
    source: String,
    failed_at: u64,
    stop_streak: u32,
}

impl FailedStart {
    fn of(&self, to: &Candidate) -> bool {
        self.is(&to.version, to.source.as_str())
    }

    fn is(&self, version: &Version, source: &str) -> bool {
        self.source == source && Version::parse(&self.to).as_ref() == Some(version)
    }

    fn is_for(&self, agent: upgrade::Agent, version: &str, source: &str) -> bool {
        self.source == source && upgrade::same_build(agent, &self.to, version)
    }

    const fn rests(&self, now: u64) -> bool {
        now < self.failed_at.saturating_add(upgrade::RETRY_S)
    }

    fn word(&self) -> String {
        format!(
            "{} {} {} {}",
            self.to, self.source, self.failed_at, self.stop_streak
        )
    }

    fn parse(word: &str) -> Option<FailedStart> {
        let mut it = word.split_whitespace();
        let read = FailedStart {
            to: it.next()?.to_string(),
            source: it.next()?.to_string(),
            failed_at: crate::parse_uint(it.next()?)?,
            stop_streak: crate::parse_uint(it.next()?)?,
        };
        it.next().is_none().then_some(read)
    }
}

struct History {
    streak_why: String,
    stop_streak: u32,
    notices: u32,
    waits: Vec<WaitNote>,
    own_work_at: u64,
    own_since: u64,
    tells: u32,
    told_at: u64,
    told_to: String,
    deadline_at: u64,
    deadline_pid: u32,
    firsts: Vec<(String, u64)>,
    work_end_at: Vec<u64>,
    goal: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Proc<'a> {
    pub(super) start: &'a str,
    pub(super) started_s: u64,
}
/// The slack past a step's own deadline before the watch reads its record ([`due_by`]).
pub const WATCH_SLACK: u64 = 5 * 60;

/// The least time between two watches of one record ([`watch_at`]).
pub const WATCH_GAP: u64 = upgrade::REASK_S;

impl St {
    pub(super) fn agent_records(&self, user_home: &Path) -> Records {
        Records::of_recorded(
            user_home,
            self.home.as_deref(),
            self.home_profile.as_deref(),
        )
    }

    pub(super) fn keep_home(&mut self, records: &Records, user_home: &Path) -> bool {
        let (home, profile) = (records.home_word(user_home), records.profile_word());
        let changed = self.home != home || self.home_profile != profile;
        self.home = home;
        self.home_profile = profile;
        changed
    }

    pub(super) fn relaunched(&mut self, at_s: u64) {
        self.phase = Phase::Relaunched { at_s };
        self.relaunched_at = at_s;
        self.pressed_at = 0;
    }

    /// Whether the owner's press of the ⬆️ stands for tab `tab` at `now`: pressed within
    /// [`upgrade::PRESS_S`], for the build this record still moves to, its Now still standing,
    /// and the move not begun.
    pub(super) fn press_stands(&self, tab: &str, now: u64) -> bool {
        upgrade_status::press_stands(
            self.agent,
            self.pressed_at,
            &self.pressed_to,
            &self.to,
            &self.request_for(tab),
            &self.phase,
            now,
        )
    }

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

    fn legacy_progress_at(&self) -> u64 {
        let at = match &self.phase {
            Phase::Announced { at_s, .. }
            | Phase::Exiting { at_s }
            | Phase::Relaunched { at_s } => *at_s,
            Phase::Pending | Phase::Done | Phase::Failed(_) => 0,
        };
        at.max(self.failed_at).max(self.salt)
    }

    pub(super) const fn time_ready(&mut self, ready: bool, facts: &mut Facts, now: u64) {
        self.ready_since = upgrade::ready_since(self.ready_since, ready, facts, now);
        if self.ready_since != 0 {
            facts.ready_s = now.saturating_sub(self.ready_since);
        }
    }

    pub(super) const fn time_failed(&self, facts: &mut Facts, now: u64) {
        facts.failed_s = self.failed_for(now);
    }

    pub(super) fn time_work(&self, facts: &mut Facts, now: u64) {
        facts.work_ended = self.work_seen
            && self.open_round()
            && !facts.background_point
            && facts.background.is_empty()
            && self.work_end_open(now);
    }

    fn work_end_open(&self, now: u64) -> bool {
        let since = match &self.phase {
            Phase::Announced { at_s, .. } => now.saturating_sub((*at_s).max(self.noticed_at)),
            Phase::Failed(_) => now.saturating_sub(self.stopped_at()),
            _ => return false,
        };
        since >= upgrade::WORK_END_FLOOR_S && self.work_end_left(now) > 0
    }

    fn work_end_left(&self, now: u64) -> u32 {
        let spent = self
            .work_end_at
            .iter()
            .filter(|&&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S)
            .count();
        upgrade::WORK_END_DAILY.saturating_sub(u32::try_from(spent).unwrap_or(u32::MAX))
    }

    fn spend_work_end(&mut self, now: u64) {
        self.work_end_at
            .retain(|&at| now.saturating_sub(at) < upgrade::WORK_END_WINDOW_S);
        self.work_end_at.push(now);
        let keep = usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX);
        if self.work_end_at.len() > keep {
            self.work_end_at.remove(0);
        }
    }

    fn open_round(&self) -> bool {
        matches!(&self.phase, Phase::Announced { .. })
            || matches!(&self.phase, Phase::Failed(why) if why == upgrade::GAVE_UP)
    }

    fn saw_work(&mut self, facts: &Facts) {
        if self.open_round() && (facts.background_point || !facts.background.is_empty()) {
            self.work_seen = true;
        }
    }

    pub(super) const fn failed_for(&self, now: u64) -> u64 {
        if matches!(self.phase, Phase::Failed(_)) {
            now.saturating_sub(self.failed_at)
        } else {
            0
        }
    }

    /// Release an announced round no build is ahead to restart onto: its build this session
    /// already runs or has passed, or, where every target was read (`answered`), one no target
    /// offers any more.
    fn release_untargeted(&mut self, running: &Version, answered: bool, now: u64) -> bool {
        let reached = Version::parse(&self.to).is_some_and(|to| to <= *running);
        if !(reached || answered) || !matches!(self.phase, Phase::Announced { .. }) {
            return false;
        }
        self.withdraw("no-target", now);
        true
    }

    /// Whether a notice this record's round typed still stands with its agent: an announced
    /// round the agent was asked in, or a release owed for one. A restart in flight is none: its
    /// agent is ended, and what it is owed is its carry-on.
    fn notice_stands(&self) -> bool {
        !self.native_retired_unissued
            && !self.in_flight()
            && (!self.release.is_empty()
                || (matches!(self.phase, Phase::Announced { .. }) && self.notified()))
    }

    /// Release an announced round `[harness] upgrade` was switched off under
    /// ([`SWITCHED_OFF`]): the notice told its agent to stop and wait for a restart no step
    /// will now make. `false`, the record untouched, for any other.
    fn release_switched_off(&mut self, now: u64) -> bool {
        if !(matches!(self.phase, Phase::Announced { .. }) && self.notified()) {
            return false;
        }
        self.withdraw(SWITCHED_OFF, now);
        true
    }

    /// Takes an announced round back to pending in a new round, its agent owed the release line
    /// (`why`): nothing restarts it without asking again first, as that line says.
    fn withdraw(&mut self, why: &str, now: u64) {
        self.owe_release(why);
        self.new_round(now);
        self.phase = Phase::Pending;
        self.forget_markers();
        self.noted.clear();
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

    fn same_process(&self, running: &Version, who: Proc<'_>) -> bool {
        if !self.proc_start.is_empty() && !who.start.is_empty() {
            return self.proc_start == who.start;
        }
        Version::parse(&self.from).is_none_or(|from| from == *running)
    }

    fn carried_failed_start(&self) -> Option<FailedStart> {
        match self.agent {
            upgrade::Agent::Claude | upgrade::Agent::Muse | upgrade::Agent::Agy => {}
            upgrade::Agent::Codex => return None,
        }
        match &self.phase {
            Phase::Failed(why) if why == "no-resume" && self.source != SAME_PROGRAM => {
                Some(FailedStart {
                    to: self.to.clone(),
                    source: self.source.clone(),
                    failed_at: self.failed_at,
                    stop_streak: self.stop_streak,
                })
            }
            _ => self.failed_start_kept.clone(),
        }
    }

    pub(super) fn failed_start(&self) -> Option<FailedStart> {
        match self.agent {
            upgrade::Agent::Claude => self.carried_failed_start(),
            upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => None,
        }
    }

    fn keep_stop(&mut self, kept: FailedStart) {
        self.phase = Phase::Failed("no-resume".to_string());
        self.failed_at = kept.failed_at;
        "no-resume".clone_into(&mut self.streak_why);
        self.stop_streak = kept.stop_streak;
    }

    /// Turns a restart whose relaunched build ended, unregistered or found gone before it came
    /// up, back to the build it ran ([`Self::back_line`]), at `now`. No gate let that end go, so a
    /// person's keys hold the line back for the whole `grace_s` ([`Self::line_grace_s`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeRetirementDebt",
            action = "Rollback",
            project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
        )
    )]
    pub(super) fn roll_back(&mut self, now: u64, grace_s: u32) -> bool {
        if self.native_retired_unissued {
            return false;
        }
        let claude = match self.agent {
            upgrade::Agent::Claude => true,
            upgrade::Agent::Codex | upgrade::Agent::Agy | upgrade::Agent::Muse => false,
        };
        if !claude || self.back_line.is_empty() || self.source == SAME_PROGRAM {
            return false;
        }
        self.fail("no-resume", now);
        self.failed_start_kept = self.failed_start();
        SAME_PROGRAM.clone_into(&mut self.source);
        self.to.clone_from(&self.from);
        self.line = std::mem::take(&mut self.back_line);
        self.account_intent = self.back_account_intent.take();
        self.native_rollback = self.native.is_some() || !self.native_prepare.is_empty();
        self.line_grace_s = Some(grace_s);
        self.line_input = None;
        self.prompt = None;
        self.phase = Phase::Exiting { at_s: now };
        true
    }

    fn history(&self) -> History {
        History {
            streak_why: self.streak_why.clone(),
            stop_streak: self.stop_streak,
            notices: self.notices,
            waits: self.waits.clone(),
            own_work_at: self.own_work_at,
            own_since: self.own_since,
            tells: self.tells,
            told_at: self.told_at,
            told_to: self.told_to.clone(),
            deadline_at: self.deadline_at,
            deadline_pid: self.deadline_pid,
            firsts: self.firsts.clone(),
            work_end_at: self.work_end_at.clone(),
            goal: self.goal,
        }
    }

    fn adopt(&mut self, history: History) {
        self.streak_why = history.streak_why;
        self.stop_streak = history.stop_streak;
        self.notices = history.notices;
        self.waits = history.waits;
        self.own_work_at = history.own_work_at;
        self.own_since = history.own_since;
        self.tells = history.tells;
        self.told_at = history.told_at;
        self.told_to = history.told_to;
        self.deadline_at = history.deadline_at;
        self.deadline_pid = history.deadline_pid;
        self.firsts = history.firsts;
        self.work_end_at = history.work_end_at;
        self.goal = history.goal;
    }

    fn for_target(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        who: Proc<'_>,
        now: u64,
    ) -> St {
        Self::retarget(prior, from, to, who, now).0
    }

    fn retarget(
        prior: Option<St>,
        from: &Version,
        to: &Candidate,
        who: Proc<'_>,
        now: u64,
    ) -> (St, Retarget) {
        if let Some(old) = prior.as_ref().filter(|old| {
            old.native_retired_unissued
                || old.native_account_candidate.is_some()
                || old.native.is_some()
                || !old.native_prepare.is_empty()
        }) {
            let mut kept = old.clone();
            kept.native_coordination = true;
            return (kept, Retarget::Kept);
        }
        let native_coordination = prior
            .as_ref()
            .is_some_and(super::native_restart::coordinated);
        let native_record = prior.as_ref().map(|st| st.native_record.clone());
        // Retargeting changes the proposed build, not the last positively completed
        // preparation. This bounded data is only a predecessor name; the host must
        // independently prove its settlement and current authority for each new move.
        let native_completed_prepare = prior
            .as_ref()
            .and_then(|st| st.native_completed_prepare.clone());
        let native_noexec_prepare = prior
            .as_ref()
            .and_then(|st| st.native_noexec_prepare.clone());
        let left = prior
            .as_ref()
            .map(|p| p.left_typed.clone())
            .unwrap_or_default();
        let home = prior
            .as_ref()
            .map(|p| (p.home.clone(), p.home_profile.clone()));
        let (mut st, how) = match prior {
            Some(mut st)
                if st.to == to.version.to_string()
                    && !(matches!(st.phase, Phase::Failed(_)) && !st.same_process(from, who)) =>
            {
                // Managed and native builds can share a version, and which one
                // the session moves to follows its launch: until the restart
                // begins, the label names the build the sweep would start now.
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
                    st.source = to.source.as_str().to_string();
                }
                let same = st.same_process(from, who);
                if st.phase == Phase::Pending && !same {
                    st.pending_since = now;
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
                    st.stop_streak = 0;
                    st.streak_why.clear();
                    st.notices = 0;
                    st.last_stop.clear();
                    st.refused = 0;
                    st.refused_at = 0;
                    st.waits.clear();
                    st.firsts.clear();
                    st.own_work_at = 0;
                    st.own_since = 0;
                    st.tells = 0;
                    st.told_at = 0;
                    st.told_to.clear();
                    st.deadline_at = 0;
                    st.deadline_pid = 0;
                    st.work_end_at.clear();
                    st.goal = false;
                } else if st.proc_start.is_empty()
                    && who.started_s != 0
                    && st.pending_since != 0
                    && st.pending_since < st.salt
                    && st.pending_since < who.started_s
                {
                    st.pending_since = st.salt.max(who.started_s);
                }
                if !who.start.is_empty() && (same || st.phase == Phase::Pending) {
                    st.proc_start = who.start.to_string();
                }
                (st, Retarget::Kept)
            }
            Some(mut st)
                if matches!(st.phase, Phase::Pending | Phase::Announced { .. })
                    && Version::parse(&st.from).as_ref() == Some(from)
                    && st.same_process(from, who) =>
            {
                st.to = to.version.to_string();
                st.source = to.source.as_str().to_string();
                st.pressed_at = 0;
                if st.proc_start.is_empty() && !who.start.is_empty() {
                    st.proc_start = who.start.to_string();
                }
                let word = st.word_onto(to, now, true);
                (st.request, st.request_tab, st.request_at) = word;
                (st, Retarget::InPlace)
            }
            Some(mut old) if matches!(old.phase, Phase::Pending | Phase::Announced { .. }) => {
                old.owe_release("retargeted");
                let same = old.same_process(from, who);
                let pending_since = if same { old.behind_since() } else { now };
                let history = same.then(|| old.history());
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
            Some(old) if matches!(old.phase, Phase::Failed(_)) => {
                let same = old.same_process(from, who);
                let rested = old.failed_start().filter(|f| f.of(to));
                let history = same.then(|| old.history());
                let pending_since = if same { old.behind_since() } else { now };
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
                if let Some(kept) = rested {
                    st.keep_stop(kept);
                    (st, Retarget::Rested)
                } else {
                    (st, Retarget::Anew)
                }
            }
            Some(old) => {
                let kept =
                    old.source == SAME_PROGRAM && Version::parse(&old.to).as_ref() == Some(from);
                let hold = (kept
                    && old
                        .request
                        .holds_build_for(old.agent, &to.version.to_string(), now))
                .then(|| (old.request.clone(), old.request_tab.clone(), old.request_at));
                let rested = old.failed_start().filter(|f| f.of(to));
                let mut st = St {
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
                if let Some(kept) = rested {
                    st.keep_stop(kept);
                    (st, Retarget::Rested)
                } else {
                    (st, Retarget::Anew)
                }
            }
            None => (St::fresh(from, to, who, now), Retarget::Anew),
        };
        st.native_coordination |= native_coordination;
        if let Some(native_record) = native_record {
            st.native_record = native_record;
        }
        st.keep_completed_native(native_completed_prepare);
        if st.native_noexec_prepare.is_none() {
            st.native_noexec_prepare = native_noexec_prepare;
        }
        if st.left_typed.is_empty() {
            st.left_typed = left;
        }
        if st.home.is_none()
            && let Some((home, profile)) = home
        {
            st.home = home;
            st.home_profile = profile;
        }
        (st, how)
    }

    pub(super) fn request_for(&self, tab: &str) -> Request {
        if self.request_tab.is_empty() || self.request_tab == tab {
            self.request.clone()
        } else {
            Request::None
        }
    }

    /// A saved word holding this actual managed move, before a quiet worker claims it. The
    /// agent, tab, conversation, source and both validated versions must still be its own;
    /// recovery after a signal does not enter this path.
    pub(super) fn held_managed_move(
        &self,
        agent: upgrade::Agent,
        tab: &str,
        thread: &str,
        versions: (&str, &str),
        now: u64,
    ) -> Option<&'static str> {
        if self.agent != agent
            || self.tab != tab
            || thread.is_empty()
            || self.thread != thread
            || !upgrade::same_build(agent, &self.from, versions.0)
        {
            return None;
        }
        let upgrade_claim = self.source == "managed"
            && self.is_upgrade()
            && matches!(
                self.phase,
                Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
            )
            && upgrade::same_build(agent, &self.to, versions.1);
        let same_build_restart = !self.is_upgrade()
            && matches!(self.source.as_str(), "managed" | SAME_PROGRAM)
            && matches!(self.phase, Phase::Failed(_) | Phase::Done)
            && upgrade::same_build(agent, &self.to, versions.0);
        if !upgrade_claim && !same_build_restart {
            return None;
        }
        let request = self.request_for(tab);
        request.holds_build_for(agent, versions.1, now).then_some(
            if matches!(request, Request::Skip(_)) {
                "skipped"
            } else {
                "deferred"
            },
        )
    }

    fn word_onto(&self, to: &Candidate, now: u64, same: bool) -> (Request, String, u64) {
        match &self.request {
            Request::Skip(_)
                if !self
                    .request
                    .holds_build_for(self.agent, &to.version.to_string(), now) =>
            {
                (Request::None, String::new(), 0)
            }
            Request::Now if !same => (Request::None, String::new(), 0),
            word => (word.clone(), self.request_tab.clone(), self.request_at),
        }
    }

    fn stop_kept_onto(&self) -> String {
        match self.agent {
            upgrade::Agent::Codex => {}
            upgrade::Agent::Claude | upgrade::Agent::Muse | upgrade::Agent::Agy => {
                return String::new();
            }
        }
        match &self.phase {
            Phase::Failed(why) if why == upgrade::GAVE_UP => why.clone(),
            _ => self.last_stop.clone(),
        }
    }

    pub(super) fn owners_hold(&self, tab: &str) -> Option<Request> {
        match self.request_for(tab) {
            word @ (Request::Skip(_) | Request::DeferUntil(_)) => Some(word),
            Request::None | Request::Now => None,
        }
    }

    pub(super) fn holds_off(&self, tab: &str, to: &Candidate, now: u64) -> bool {
        self.owners_hold(tab)
            .is_some_and(|word| word.holds_build_for(self.agent, &to.version.to_string(), now))
            || self
                .failed_start()
                .is_some_and(|failed| failed.of(to) && failed.rests(now))
    }

    pub(super) fn retried_by(&self, st: &mut St) {
        if self.preserve_unresolved_native(st) {
            return;
        }
        st.native_coordination |= super::native_restart::coordinated(self);
        st.native_record = self.native_record.clone();
        self.carry_noexec(st);
        if self.agent == st.agent
            && let Some(failed) = self
                .carried_failed_start()
                .filter(|f| f.is_for(st.agent, &st.to, &st.source))
        {
            "no-resume".clone_into(&mut st.streak_why);
            st.stop_streak = failed.stop_streak;
        }
    }

    /// Passive predecessor identity survives a new plan; current authority never does.
    fn keep_completed_native(&mut self, completed: Option<Box<super::native_act::Prepare>>) {
        if self.native_completed_prepare.is_none() {
            self.native_completed_prepare = completed;
        }
    }

    pub(super) fn kept_by_restart(&self, tab: &str, st: &mut St) {
        if self.preserve_unresolved_native(st) {
            return;
        }
        st.native_coordination |= super::native_restart::coordinated(self);
        st.native_record = self.native_record.clone();
        self.carry_noexec(st);
        st.keep_completed_native(self.native_completed_prepare.clone());
        if let Some(word) = self.owners_hold(tab) {
            st.request = word;
            st.request_tab.clone_from(&self.request_tab);
            st.request_at = self.request_at;
        }
        st.failed_start_kept = self.carried_failed_start();
        let behind = match &self.phase {
            Phase::Pending | Phase::Announced { .. } => self.is_upgrade(),
            Phase::Failed(_) => self.is_upgrade() || self.source == SAME_PROGRAM,
            Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                self.source == SAME_PROGRAM
            }
        };
        let same_build = self.agent == st.agent
            && upgrade::same_build(self.agent, self.build_behind(), &st.from);
        if behind && same_build && self.pending_since != 0 {
            st.pending_since = self.pending_since;
        }
    }

    fn carry_noexec(&self, st: &mut St) {
        // Rebuilding ordinary retry state is not a new native preparation.
        // Keep the positive result and its predecessor context until one is.
        if self.native_noexec_prepare.is_some()
            && st.native.is_none()
            && st.native_prepare.is_empty()
            && st.native_completed_prepare.is_none()
            && st.native_noexec_prepare.is_none()
        {
            st.native_noexec_prepare = self.native_noexec_prepare.clone();
            st.native_completed_prepare = self.native_completed_prepare.clone();
            st.native_observation = self.native_observation.clone();
            st.native_rollback = self.native_rollback;
        }
    }

    fn build_behind(&self) -> &str {
        if self.is_upgrade() {
            &self.from
        } else {
            &self.to
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeRetirementDebt",
            action = "Retarget",
            project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
        )
    )]
    fn preserve_unresolved_native(&self, st: &mut St) -> bool {
        if !self.native_retired_unissued
            && self.native_account_candidate.is_none()
            && self.native.is_none()
            && self.native_prepare.is_empty()
        {
            return false;
        }
        // A new target/record cannot replace the only retained id/capability/recipe/budget.
        // Reconcile the original first, or remain held with that original intact.
        *st = self.clone();
        st.native_coordination = true;
        true
    }

    fn new_round(&mut self, now: u64) {
        if self.native_retired_unissued || self.native_account_candidate.is_some() {
            return;
        }
        self.account_intent = None;
        self.line_input = None;
        self.back_account_intent = None;
        if self.pending_since == 0 {
            self.pending_since = self.salt;
        }
        self.refused = 0;
        self.refused_at = 0;
        self.pressed_at = 0;
        self.salt = now.max(
            self.salt
                .saturating_add(u64::from(upgrade::MAX_ASKS))
                .saturating_add(1),
        );
    }

    pub(super) const fn behind_since(&self) -> u64 {
        if self.pending_since == 0 {
            self.salt
        } else {
            self.pending_since
        }
    }

    pub(super) fn rung_at(&self, now: u64) -> upgrade::Rung {
        match self.behind_since() {
            0 => upgrade::Rung::Prefer,
            since => upgrade::rung(now.saturating_sub(since)),
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        if !self.still_runs(&print, now) {
            self.still_print = print;
            self.still_since = now;
        }
        self.still_at = now;
        now.saturating_sub(self.still_since)
    }

    /// Whether a look at `print` at `now` goes on with the still run: the look before it (an idle one,
    /// the only kind that has a print) read the same words above the composer, at most
    /// [`IDLE_RUN_GAP_S`] before.
    pub(super) fn still_runs(&self, print: &str, now: u64) -> bool {
        self.still_since != 0
            && self.still_print == print
            && now.saturating_sub(self.still_at) <= IDLE_RUN_GAP_S
    }

    /// Voids the deadline the agent process was told, and the line that named it
    /// ([`upgrade::deadline_voids`]): the own-work hold starts again from nothing.
    pub(super) fn void_deadline(&mut self) {
        self.deadline_at = 0;
        self.deadline_pid = 0;
        self.tells = 0;
        self.told_at = 0;
        self.told_to.clear();
        self.own_since = 0;
    }

    pub(super) fn note_wait(&mut self, word: &str, now: u64) {
        if self.wait != word {
            self.wait = word.to_string();
            self.wait_since = now;
            self.log_wait(word, now);
        }
        self.wait_seen = if word == "attended" { now } else { 0 };
        if upgrade_status::holds_by_own_work(&self.wait, false) {
            self.saw_own_work(now);
        }
        match upgrade_status::wait_class(word) {
            upgrade_status::WaitClass::OwnWork if self.own_since == 0 => self.own_since = now,
            upgrade_status::WaitClass::Person | upgrade_status::WaitClass::Limit => {
                self.own_since = 0;
            }
            _ => {}
        }
    }

    pub(super) const fn saw_hand(&mut self, now: u64) -> bool {
        let due = self.hand_seen_s == 0 || now.saturating_sub(self.hand_seen_s) >= HAND_RESTAMP_S;
        if due {
            self.hand_seen_s = now;
        }
        due
    }

    const fn saw_own_work(&mut self, now: u64) {
        if self.own_work_at == 0 || now.saturating_sub(self.own_work_at) >= OWN_WORK_RESTAMP_S {
            self.own_work_at = now;
        }
    }

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

    fn annotate_wait(&mut self, now: u64, note: &str) {
        if let Some(last) = self.waits.last_mut()
            && last.at == now
            && last.note.is_empty()
        {
            last.note = super::one_line(note, 240);
        }
    }

    fn note_step(&mut self, step: &str, status: &str, now: u64) {
        self.restart_look = false;
        self.wall_move = false;
        if refused(step) {
            if self.refused == 0 {
                self.refused_at = now;
            }
            self.refused = self.refused.saturating_add(1);
        }
        if step.strip_prefix("wait:") == Some(upgrade::PRESSED) {
            // The press's lane records what holds the move: an automatic step that stood aside
            // for it leaves that word.
        } else if let Some(why) = step
            .strip_prefix("wait:")
            .or_else(|| step.strip_prefix("held-back:"))
            .or_else(|| step.strip_prefix("told:"))
        {
            self.note_wait(&upgrade::wait_word(why, status), now);
        } else if !step.starts_with("skip:") && !step.starts_with("would-") {
            if !self.wait.is_empty() || step.starts_with("announced") {
                self.log_wait(step, now);
            }
            self.wait.clear();
            self.wait_since = 0;
            self.wait_seen = 0;
            self.refused = 0;
            self.refused_at = 0;
        }
    }

    pub(super) fn held_past_go(&self, read: &str) -> bool {
        let act = read.split_once(AT_POINT).map_or(read, |(act, _)| act);
        if act == "would-announce" {
            refused(&format!("wait:{}", self.wait))
        } else {
            (act == "would-restart" || act.starts_with("would-restart:")) && self.restart_look
        }
    }

    fn note_held(&mut self, step: &str, held: &[upgrade::Held], now: u64) {
        if !step.starts_with("wait:") && !step.starts_with("told:") {
            self.held_by.clear();
            return;
        }
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
        if upgrade_status::holds_by_own_work(&self.wait, !self.held_by.is_empty()) {
            self.saw_own_work(now);
        }
    }

    pub(super) fn to_json(&self) -> String {
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
            ("pid_start", &self.pid_start),
            ("notice_start", &self.notice_start),
            ("relaunched_start", &self.relaunched_start),
            ("tab", &self.tab),
            ("line", &self.line),
            ("native_prepare", &self.native_prepare),
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
            ("ask", &self.ask),
            ("model_before", &self.model_before),
            ("launch_model", &self.launch_model),
            ("resumed_on", &self.resumed_on),
            ("wait", &self.wait),
            ("request", &self.request.word()),
            ("request_tab", &self.request_tab),
            ("pressed_to", &self.pressed_to),
            ("outcome", &self.outcome),
            ("agent", self.agent.word()),
            ("thread", &self.thread),
            ("codex_home", &self.codex_home),
            ("cwd", &self.cwd),
            ("twin", &self.twin),
            ("resume_home", &self.resume_home),
            ("resume_start", &self.resume_start),
            ("left_typed", &self.left_typed),
            ("release", &self.release),
            ("last_stop", &self.last_stop),
            ("told_to", &self.told_to),
            (
                "ended_work",
                &self
                    .ended_work
                    .as_ref()
                    .map(|list| format!("{ENDED_WORK_MARK}{list}"))
                    .unwrap_or_default(),
            ),
            ("streak_why", &self.streak_why),
            ("still_print", &self.still_print),
            ("proc_start", &self.proc_start),
            ("progress_key", &self.progress_key),
            ("looked_by", &self.looked_by),
            ("guard", &self.guard),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        if self.native_rollback {
            o.insert("native_rollback".into(), Value::Bool(true));
        }
        if super::native_restart::coordinated(self) {
            o.insert("native_coordination".into(), Value::Bool(true));
            o.insert(
                "native_generation".into(),
                Value::from(super::native_record::generation(self)),
            );
            if let Some(id) = super::native_record::write_id(self) {
                o.insert("native_write_id".into(), Value::from(id));
            }
        }
        if self.native_retired_unissued || self.native_account_candidate.is_some() {
            o.insert("native_retired_unissued".into(), Value::Bool(true));
        }
        if let Some(candidate) = &self.native_account_candidate {
            candidate.write_into(&mut o);
        }
        if let Some(encoded) = self.native_completed_prepare.as_ref().and_then(|prepare| {
            // A still-running older supervisor can read this completed
            // observation too. Preserve the exact common representation
            // where the entire preparation predates token routes.
            let request = super::native_act::Request::Prepare(prepare.clone());
            request.encode_legacy_cold().or_else(|| request.encode())
        }) {
            o.insert("native_completed_prepare".into(), Value::from(encoded));
        }
        if let Some(encoded) = self
            .native_noexec_prepare
            .as_ref()
            .and_then(|prepare| super::native_act::Request::Prepare(prepare.clone()).encode())
        {
            o.insert("native_noexec_prepare".into(), Value::from(encoded));
        }
        if let Some(encoded) = self
            .native_observation
            .as_ref()
            .and_then(super::native_observe::SavedObservation::encode)
        {
            o.insert("native_observation".into(), Value::from(encoded));
        }
        if let Some(login) = &self.login {
            o.insert("login".into(), Value::from(login.as_str()));
        }
        if let Some(intent) = &self.account_intent {
            o.insert("account_intent".into(), intent.to_value());
        }
        if let Some(intent) = &self.back_account_intent {
            o.insert("back_account_intent".into(), intent.to_value());
        }
        if let Some(binding) = &self.native {
            o.insert("native_id".into(), Value::from(binding.id.as_str()));
            o.insert(
                "native_cap".into(),
                Value::from(binding.capability.as_str()),
            );
        }
        if let Some(home) = &self.home {
            o.insert("home".into(), Value::from(home.as_str()));
        }
        if let Some(home) = &self.codex_home_env {
            o.insert("codex_home_env".into(), Value::from(home.as_str()));
        }
        if let Some(auth) = &self.codex_auth_path {
            o.insert("codex_auth_path".into(), Value::from(auth.as_str()));
        }
        if !self.muse_data.is_empty() {
            o.insert("muse_data".into(), Value::from(self.muse_data.as_str()));
        }
        if !self.agy_config.is_empty() {
            o.insert("agy_config".into(), Value::from(self.agy_config.as_str()));
        }
        if let Some(profile) = &self.home_profile {
            o.insert("home_profile".into(), Value::from(profile.as_str()));
        }
        if let Some(failed) = &self.failed_start_kept {
            o.insert("failed_start_kept".into(), Value::from(failed.word()));
        }
        if !self.back_line.is_empty() {
            o.insert("back_line".into(), Value::from(self.back_line.as_str()));
        }
        if !self.line_dir.is_empty() {
            o.insert("line_dir".into(), Value::from(self.line_dir.as_str()));
        }
        if let Some(grace) = self.line_grace_s {
            o.insert("line_grace".into(), Value::from(u64::from(grace)));
        }
        if let Some(input) = &self.line_input {
            o.insert("line_input".into(), input.to_value());
        }
        if let Some(block) = self.line_block {
            o.insert("line_block".into(), Value::from(block));
        }
        if self.moved_at != 0 {
            o.insert("moved_at".into(), Value::from(self.moved_at));
        }
        if let Some(login) = &self.moved_login {
            o.insert("moved_login".into(), Value::from(login.as_str()));
        }
        if !self.look_word.is_empty() {
            o.insert("look_word".into(), Value::from(self.look_word.as_str()));
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
            ("relaunched_at", self.relaunched_at),
            ("hold_since", self.hold_since_s),
            ("hold_seen", self.hold_seen_s),
            ("ready_since", self.ready_since),
            ("pending_since", self.pending_since),
            ("wait_since", self.wait_since),
            ("wait_seen", self.wait_seen),
            ("done_at", self.done_at),
            ("request_at", self.request_at),
            ("pressed_at", self.pressed_at),
            ("exited_at", self.exited_at),
            ("hand_seen", self.hand_seen_s),
            ("idle_looks", u64::from(self.idle_looks)),
            ("idle_pid", u64::from(self.idle_pid)),
            ("idle_at", self.idle_at),
            ("failed_at", self.failed_at),
            ("noticed_at", self.noticed_at),
            ("stop_streak", u64::from(self.stop_streak)),
            ("notices", u64::from(self.notices)),
            ("still_since", self.still_since),
            ("still_at", self.still_at),
            ("progress_at", self.progress_at),
            ("looked_at", self.looked_at),
            ("point_at", self.point_at),
            ("refused", u64::from(self.refused)),
            ("refused_at", self.refused_at),
            ("work_seen", u64::from(self.work_seen)),
            ("own_work_at", self.own_work_at),
            ("own_since", self.own_since),
            ("tells", u64::from(self.tells)),
            ("told_at", self.told_at),
            ("deadline_at", self.deadline_at),
            ("deadline_pid", u64::from(self.deadline_pid)),
            ("ended_held_s", self.ended_held_s),
            ("goal", u64::from(self.goal)),
            ("limit_until", self.limit_until),
            ("quiet", u64::from(self.quiet)),
            ("limit_spend", u64::from(self.limit_spend)),
            ("restart_look", u64::from(self.restart_look)),
            ("wall_move", u64::from(self.wall_move)),
            ("found_gone", u64::from(self.found_gone)),
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
        let tabs_until: Vec<String> = self
            .tabs_until
            .iter()
            .map(|(tab, until)| format!("{until} {tab}"))
            .collect();
        for (k, list) in [
            ("argv", &self.argv),
            ("muse_env", &self.muse_env),
            ("agy_env", &self.agy_env),
            ("tabs", &self.tabs),
            ("tabs_until", &tabs_until),
            ("resume_argv", &self.resume_argv),
            ("ended_waits", &self.ended_waits),
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

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeRetirementDebt",
            action = "Reload",
            project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeAccountClientSlot",
            action = "Reload",
            project = "aterm_agent::harness::native_account_handoff::tests::transition"
        )
    )]
    pub(super) fn from_json(text: &str) -> Option<St> {
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
        let agent = match v.get("agent") {
            None => upgrade::Agent::Claude,
            Some(word) => upgrade::Agent::parse(word.as_str()?)?,
        };
        if super::lanes::lanes(agent.program()) == super::lanes::Lanes::NONE {
            return None;
        }
        let relaunched_at = match (&phase, v.get("relaunched_at")) {
            (Phase::Relaunched { at_s }, None) => *at_s,
            _ => n("relaunched_at"),
        };
        let mut st = St {
            native_record: super::native_record::Baseline::default(),
            host_ended_fresh: false,
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
            pid_start: s("pid_start"),
            notice_pid: small("notice_pid"),
            notice_start: s("notice_start"),
            shell: small("shell"),
            tab: s("tab"),
            line: s("line"),
            native_prepare: s("native_prepare"),
            native_coordination: v
                .get("native_coordination")
                .map_or(Some(false), Value::as_bool)?
                || !s("native_prepare").is_empty()
                || v.get("native_id").is_some()
                || v.get("native_completed_prepare").is_some()
                || v.get("native_noexec_prepare").is_some()
                || v.get("native_observation").is_some()
                || v.get("native_account_candidate").is_some()
                || v.get("native_generation").is_some()
                || v.get("native_write_id").is_some()
                || v.get("native_rollback").and_then(Value::as_bool) == Some(true)
                || v.get("native_retired_unissued").and_then(Value::as_bool) == Some(true),
            native_retired_unissued: v
                .get("native_retired_unissued")
                .map_or(Some(false), Value::as_bool)?
                || v.get("native_account_candidate").is_some(),
            native_account_candidate: v
                .get("native_account_candidate")
                .map(super::native_account_handoff::Pending::read),
            native_completed_prepare: match v.get("native_completed_prepare") {
                None => None,
                Some(value) => {
                    let super::native_act::Request::Prepare(prepare) =
                        super::native_act::Request::parse(value.as_str()?)?
                    else {
                        return None;
                    };
                    Some(prepare)
                }
            },
            native_noexec_prepare: match v.get("native_noexec_prepare") {
                None => None,
                Some(value) => {
                    let wire = value.as_str()?;
                    let request = super::native_act::Request::parse(wire)?;
                    if !request.matches_retained(wire) {
                        return None;
                    }
                    let super::native_act::Request::Prepare(prepare) = request else {
                        return None;
                    };
                    Some(prepare)
                }
            },
            native_observation: match v.get("native_observation") {
                None => None,
                Some(value) => Some(super::native_observe::SavedObservation::parse(
                    value.as_str()?,
                )?),
            },
            native_rollback: v
                .get("native_rollback")
                .map_or(Some(false), Value::as_bool)?,
            native: super::native_restart::read(&v)?,
            line_dir: s("line_dir"),
            line_grace_s: v
                .get("line_grace")
                .and_then(Value::as_u64)
                .and_then(|g| u32::try_from(g).ok()),
            line_input: v
                .get("line_input")
                .and_then(super::restart_stamp::Input::from_value),
            back_line: s("back_line"),
            prompt: PromptMark::parse(&s("prompt")),
            line_block: v.get("line_block").and_then(Value::as_u64),
            noted: s("noted"),
            cause: s("cause"),
            ask: s("ask"),
            model_before: s("model_before"),
            mark: n("mark"),
            launch_model: s("launch_model"),
            login: v.get("login").and_then(Value::as_str).map(str::to_owned),
            account_intent: v
                .get("account_intent")
                .and_then(super::accounts_pool::ExecutionIntent::from_value),
            back_account_intent: v
                .get("back_account_intent")
                .and_then(super::accounts_pool::ExecutionIntent::from_value),
            home: v.get("home").and_then(Value::as_str).map(str::to_owned),
            home_profile: v
                .get("home_profile")
                .and_then(Value::as_str)
                .map(str::to_owned),
            failed_start_kept: v
                .get("failed_start_kept")
                .and_then(Value::as_str)
                .and_then(FailedStart::parse),
            confirm_by: n("confirm_by"),
            look_word: s("look_word"),
            resumed_on: s("resumed_on"),
            resumed_pid: small("resumed_pid"),
            relaunched_pid: small("relaunched_pid"),
            relaunched_start: s("relaunched_start"),
            relaunched_at,
            moved_at: n("moved_at"),
            moved_login: v
                .get("moved_login")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tabs: list("tabs"),
            tabs_until: list("tabs_until")
                .iter()
                .filter_map(|w| {
                    let (until, tab) = w.split_once(' ')?;
                    Some((tab.to_string(), crate::parse_uint(until)?))
                })
                .collect(),
            hold_since_s: n("hold_since"),
            hold_seen_s: n("hold_seen"),
            ready_since: n("ready_since"),
            pending_since: n("pending_since"),
            wait: s("wait"),
            wait_since: n("wait_since"),
            wait_seen: n("wait_seen"),
            held_by: list("held_by")
                .iter()
                .filter_map(|h| {
                    let mut it = h.split_whitespace();
                    let held = HeldBy {
                        pid: crate::parse_uint(it.next()?)?,
                        name: it.next()?.to_string(),
                        since: crate::parse_uint(it.next()?)?,
                    };
                    it.next().is_none().then_some(held)
                })
                .collect(),
            idle_looks: small("idle_looks"),
            idle_pid: small("idle_pid"),
            idle_at: n("idle_at"),
            request: Request::parse_for(agent, &s("request")).unwrap_or_default(),
            request_tab: s("request_tab"),
            request_at: n("request_at"),
            pressed_at: n("pressed_at"),
            pressed_to: s("pressed_to"),
            outcome: s("outcome"),
            done_at: n("done_at"),
            agent,
            thread: s("thread"),
            codex_home: s("codex_home"),
            codex_home_env: v
                .get("codex_home_env")
                .and_then(Value::as_str)
                .map(str::to_owned),
            codex_auth_path: v
                .get("codex_auth_path")
                .and_then(Value::as_str)
                .map(str::to_owned),
            argv: list("argv"),
            cwd: s("cwd"),
            twin: s("twin"),
            muse_data: s("muse_data"),
            muse_env: list("muse_env"),
            agy_config: s("agy_config"),
            agy_env: list("agy_env"),
            resume_argv: list("resume_argv"),
            resume_home: s("resume_home"),
            resume_start: s("resume_start"),
            left_typed: s("left_typed"),
            exited_at: n("exited_at"),
            hand_seen_s: n("hand_seen"),
            failed_at: n("failed_at"),
            noticed_at: n("noticed_at"),
            last_stop: s("last_stop"),
            streak_why: s("streak_why"),
            stop_streak: small("stop_streak"),
            notices: small("notices"),
            still_print: s("still_print"),
            still_since: n("still_since"),
            still_at: n("still_at"),
            own_work_at: n("own_work_at"),
            own_since: n("own_since"),
            tells: small("tells"),
            told_at: n("told_at"),
            told_to: s("told_to"),
            deadline_at: n("deadline_at"),
            deadline_pid: small("deadline_pid"),
            ended_waits: list("ended_waits"),
            ended_work: s("ended_work")
                .strip_prefix(ENDED_WORK_MARK)
                .map(str::to_owned),
            ended_held_s: n("ended_held_s"),
            proc_start: s("proc_start"),
            waits: list("waits")
                .iter()
                .filter_map(|w| {
                    let mut it = w.splitn(3, ' ');
                    Some(WaitNote {
                        at: crate::parse_uint(it.next()?)?,
                        word: it.next()?.to_string(),
                        note: it.next().unwrap_or_default().to_string(),
                    })
                })
                .collect(),
            work_end_at: list("work_end_at")
                .iter()
                .filter_map(|t| crate::parse_uint(t))
                .take(usize::try_from(upgrade::WORK_END_DAILY).unwrap_or(usize::MAX))
                .collect(),
            goal: n("goal") != 0,
            firsts: list("firsts")
                .iter()
                .filter_map(|w| {
                    let (at, word) = w.split_once(' ')?;
                    Some((word.to_string(), crate::parse_uint(at)?))
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
            quiet: n("quiet") != 0,
            limit_spend: n("limit_spend") != 0,
            restart_look: n("restart_look") != 0,
            wall_move: n("wall_move") != 0,
            found_gone: n("found_gone") != 0,
        };
        if v.get("progress_key").is_none() {
            st.progress_key = st.progress_key();
            st.progress_at = st.legacy_progress_at();
        }
        if v.get("notices").is_none() {
            st.notices = upgrade_status::round_notices(&st.phase, &st.streak_why, st.stop_streak);
        }
        if v.get("pid_start").is_none() {
            // An earlier build's codex-alab, Muse Code or agy record wrote its agent's start as
            // `proc_start`, beside `pid`, from the one snapshot it was filed on, and no step of
            // those lanes rebinds either: the start that names its agent ([`Self::pid_start`]).
            // A Claude Code record's `proc_start` is its upgrade notice's process, so it takes
            // a start only from a snapshot of its pid ([`Self::adopt_start`]).
            match st.agent {
                upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => {
                    st.pid_start = squash(&st.proc_start);
                }
                upgrade::Agent::Claude => {}
            }
        }
        let generation = v.get("native_generation").map_or(Some(0), Value::as_u64)?;
        let write_id = match v.get("native_write_id") {
            None => None,
            Some(value) => {
                let id = value.as_str()?;
                if generation == 0 || !aterm_codec::hex::is_lower(id, 32) {
                    return None;
                }
                Some(id.to_owned())
            }
        };
        super::native_record::loaded(&st, generation, write_id);
        Some(st)
    }

    pub(super) const fn in_flight(&self) -> bool {
        self.native_retired_unissued
            || matches!(self.phase, Phase::Exiting { .. } | Phase::Relaunched { .. })
    }

    /// Whether the agent the relaunch started ([`Self::relaunched_pid`]) is gone: its pid free,
    /// or, where its start was recorded with it ([`Self::relaunched_start`]), born again as
    /// another process.
    /// `alive` and `start_of` are the kernel's answers for a pid.
    pub(super) fn relaunched_gone(
        &self,
        alive: impl Fn(u32) -> bool,
        start_of: impl Fn(u32) -> Option<String>,
    ) -> bool {
        self.relaunched_pid != 0
            && (!alive(self.relaunched_pid)
                || !self.relaunched_start.is_empty()
                    && start_of(self.relaunched_pid)
                        .is_some_and(|now| squash(&now) != self.relaunched_start))
    }

    /// Bind the agent process `pid`, born at `start` ([`kernel_start`]'s spelling): the writer of
    /// [`Self::pid`] beside its start, so a record names its agent by identity. A stamp of an
    /// exit ([`Self::exited_at`]) spoke of the agent bound before, so it goes with another one.
    pub(super) fn bind(&mut self, pid: u32, start: &str) {
        let start = squash(start);
        if (self.pid, self.pid_start.as_str()) != (pid, start.as_str()) {
            self.exited_at = 0;
            self.line_input = None;
        }
        self.pid = pid;
        self.pid_start = start;
    }

    /// Take `start` as the start of [`Self::pid`] where a snapshot of `pid`, born at `start`,
    /// names it and this record, written by an earlier build, recorded none: the identity the
    /// host kept for its agent (the layout's, after a restore), as RS1 carries an earlier
    /// host's restart by identity. A record that recorded its own start keeps it.
    pub(super) fn adopt_start(&mut self, pid: u32, start: &str) {
        if pid != 0 && self.pid == pid && self.pid_start.is_empty() {
            self.pid_start = squash(start);
        }
    }

    /// Whether this record's agent is the process `pid` born at `start`: the same pid, and the
    /// same start where the record recorded one.
    pub(super) fn names(&self, pid: u32, start: &str) -> bool {
        self.pid == pid && (self.pid_start.is_empty() || self.pid_start == squash(start))
    }

    /// What the kernel says now of the agent this record names ([`super::relaunch::agent_exit`]).
    pub(super) fn agent_exit(&self) -> super::relaunch::AgentExit {
        self.agent_exit_by(alive, kernel_start)
    }

    /// [`Self::agent_exit`] by `alive` and `start_of`, the kernel's answers for a pid.
    pub(super) fn agent_exit_by(
        &self,
        alive: impl Fn(u32) -> bool,
        start_of: impl Fn(u32) -> Option<String>,
    ) -> super::relaunch::AgentExit {
        if self.pid == 0 {
            // The record names no process: there is no exit to wait on.
            return super::relaunch::AgentExit::Gone;
        }
        let live = alive(self.pid);
        // Both starts in one spelling ([`Self::bind`] squashes the one it records).
        let now = live
            .then(|| start_of(self.pid))
            .flatten()
            .map(|now| squash(&now));
        // A start unread because the process went between the two reads (reaped as it was
        // asked) is its exit, not an unreadable start: only a pid that still lives after the
        // start's read failed leaves it unproven.
        let live = live && (now.is_some() || alive(self.pid));
        super::relaunch::agent_exit(&self.pid_start, live, now.as_deref())
    }

    pub(super) fn close_overtaken(&mut self, why: &str, now: u64) {
        if self.native_retired_unissued {
            return;
        }
        self.exited_at = 0;
        self.stop(why, now);
    }

    pub(super) fn landed_after_stop(&self, sf: &SessionFile, k: &dyn Kernel) -> bool {
        match self.agent {
            upgrade::Agent::Claude => {}
            upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => return false,
        }
        matches!(&self.phase, Phase::Failed(why) if why == "no-resume")
            && self.exited_at != 0
            && super::relaunch::ends_at_once(&self.cause)
            && self.started_by_line(sf)
            && relaunch_of(k, sf, &sf.session_id, self.pid, self.shell) == Relaunch::Ours
    }

    fn started_by_line(&self, sf: &SessionFile) -> bool {
        let stop = self.stopped_at();
        stop != 0
            && super::unix_time::parse_lstart(&sf.proc_start).is_some_and(|start| start <= stop)
            && (self.to.is_empty() || self.to == "-" || sf.version == self.to)
    }

    pub(super) fn reopen_landed(&mut self, now: u64) {
        if let Phase::Failed(why) = &self.phase
            && *why == self.streak_why
        {
            self.stop_streak = self.stop_streak.saturating_sub(1);
            if self.stop_streak == 0 {
                self.streak_why.clear();
            }
        }
        self.phase = Phase::Relaunched { at_s: now };
        self.failed_at = 0;
    }

    pub(super) fn relaunched_on_exit_of(&self, pid: u32) -> bool {
        self.pid == pid
            && (self.cause == super::relaunch::CAUSE_EXIT
                || self.cause == super::relaunch::CAUSE_FRESH)
    }

    pub(super) fn stuck(&self, now: u64) -> Option<&'static str> {
        if self.native_retired_unissued {
            return None;
        }
        upgrade_status::stuck_on(&self.phase, self.exited_at, now)
    }

    pub(super) fn is_upgrade(&self) -> bool {
        self.cause.is_empty() || self.cause == super::relaunch::CAUSE_UPGRADE_FRESH
    }

    /// The harness lane this record's writes name themselves by ([`lane_words`]): the
    /// codex-alab, agy or Muse Code drive by its agent, and for Claude Code the live upgrade or
    /// a relaunch by its cause.
    pub(super) fn lane(&self) -> Lane {
        match self.agent {
            upgrade::Agent::Codex => Lane::CodexAlab,
            upgrade::Agent::Agy => Lane::Agy,
            upgrade::Agent::Muse => Lane::Muse,
            upgrade::Agent::Claude if self.is_upgrade() => Lane::Upgrade,
            upgrade::Agent::Claude => Lane::Relaunch,
        }
    }

    pub(super) fn seen_gone(&mut self, now: u64) -> bool {
        if self.native_retired_unissued
            || self.exited_at != 0
            || self.pid == 0
            || self.agent_exit() != super::relaunch::AgentExit::Gone
        {
            return false;
        }
        self.exited_at = now;
        true
    }

    pub(super) fn carried_through_hand(&mut self, now: u64) -> bool {
        if self.native_retired_unissued {
            return false;
        }
        let ends = match self.agent {
            upgrade::Agent::Claude | upgrade::Agent::Muse | upgrade::Agent::Agy => true,
            upgrade::Agent::Codex => false,
        };
        if !ends
            || !matches!(self.phase, Phase::Exiting { .. })
            || self.pid == 0
            || self.agent_exit() != super::relaunch::AgentExit::Gone
        {
            return false;
        }
        self.exited_at = now;
        true
    }

    pub(super) fn confirming(&self) -> bool {
        !self.native_retired_unissued && self.phase == Phase::Done && self.confirm_by != 0
    }

    /// Ends this record's look in memory, for a read that writes nothing (the watch's), where
    /// its clock already has (`relaunch::look_holds`). A step closes one through
    /// `relaunch::confirm`, which says its last word.
    fn close_look(&mut self) {
        if self.confirming() {
            self.confirm_by = 0;
            self.look_word.clear();
        }
    }

    fn notice_belongs_to(&self, sf: &SessionFile, tab: &str) -> bool {
        self.tab == tab
            && self.notice_pid == sf.pid
            && !self.notice_start.is_empty()
            && self.notice_start == squash(&sf.proc_start)
    }

    fn notified(&self) -> bool {
        driven_here(self.agent)
            && self.cause.is_empty()
            && match &self.phase {
                Phase::Announced { .. } => true,
                Phase::Failed(why) => why == upgrade::GAVE_UP,
                Phase::Pending | Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => {
                    false
                }
            }
    }

    fn hears_ready(&self) -> bool {
        match &self.phase {
            Phase::Announced { .. } => true,
            Phase::Failed(why) => why == upgrade::GAVE_UP,
            _ => false,
        }
    }

    pub(super) fn round_markers(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.marker)
            .chain(&self.markers)
            .filter(|m| !m.is_empty())
    }

    pub(super) fn owe_release(&mut self, why: &str) {
        if self.release.is_empty() && self.notified() {
            self.release = why.to_string();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeRetirementDebt",
            action = "Expire",
            project = "aterm_agent::harness::native_restart::retirement_debt_tests::transition"
        )
    )]
    pub(super) fn stop(&mut self, why: &str, now: u64) {
        if self.native_retired_unissued {
            return;
        }
        self.owe_release(why);
        self.forget_markers();
        self.found_gone = false;
        self.fail(why, now);
    }

    pub(super) fn fail(&mut self, why: &str, now: u64) {
        if self.native_retired_unissued {
            return;
        }
        if self.streak_why == why {
            self.stop_streak = self.stop_streak.saturating_add(1);
        } else {
            why.clone_into(&mut self.streak_why);
            self.stop_streak = 1;
        }
        self.phase = Phase::Failed(why.to_string());
        self.failed_at = now.saturating_add(self.rest_extension());
    }

    pub(super) fn stopped_at(&self) -> u64 {
        if self.failed_at == 0 || !matches!(self.phase, Phase::Failed(_)) {
            return 0;
        }
        self.failed_at.saturating_sub(self.rest_extension())
    }

    fn rest_extension(&self) -> u64 {
        if self.streak_why == upgrade::GAVE_UP {
            upgrade::unanswered_rest_extension(self.stop_streak)
        } else {
            upgrade::rest_extension(self.stop_streak)
        }
    }

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
        self.line_block = None;
        self.exited_at = 0;
        self.resumed_pid = 0;
        self.relaunched_pid = 0;
        self.relaunched_start.clear();
        self.noted.clear();
        self.cause.clear();
        self.line_input = None;
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

    pub(super) fn signalled(
        &mut self,
        pid: u32,
        start: &str,
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
        };
        self.bind(pid, start);
        self.shell = shell;
        tab.clone_into(&mut self.tab);
        self.line = line;
        // A new command never inherits account authority from the prior round.
        // Its constructor attaches the freshly captured intent after this reset.
        self.account_intent = None;
        self.native_rollback = false;
        self.line_dir.clear();
        self.line_grace_s = None;
        self.line_input = None;
        self.back_line.clear();
        self.back_account_intent = None;
        self.exited_at = 0;
        self.hand_seen_s = 0;
        back
    }

    pub(super) fn unsent(&mut self, back: Unsent) {
        self.ended_waits.clear();
        self.ended_work = None;
        self.ended_held_s = 0;
        self.account_intent = None;
        self.phase = back.phase;
        self.release = back.release;
        self.marker = back.marker;
        self.markers = back.markers;
        self.back_line.clear();
        self.back_account_intent = None;
    }

    pub(super) fn signal_failed(&mut self, back: Unsent, now: u64) {
        self.unsent(back);
        self.stop("signal-refused", now);
    }

    fn give_up(&mut self, now: u64) {
        self.owe_release("gave-up");
        self.fail(upgrade::GAVE_UP, now);
        if self.request == Request::Now {
            self.request = Request::None;
            self.request_tab.clear();
            self.request_at = 0;
        }
    }

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

    fn spend_ask(&mut self, asks: u32, marker: &str) {
        let before = match self.phase {
            Phase::Announced { asks, .. } => asks,
            _ => 0,
        };
        if asks > before && !self.asked.iter().any(|m| m == marker) {
            self.notices = self.notices.saturating_add(1);
        }
    }

    fn announced(&mut self, marker: String, now: u64, asks: u32) {
        // A first notice opens its round: nothing earlier is its answer.
        if self.phase == Phase::Pending {
            self.markers.clear();
            self.asked.clear();
        }
        self.spend_ask(asks, &marker);
        if !self.markers.contains(&marker) {
            self.markers.push(marker.clone());
        }
        if !self.asked.contains(&marker) {
            self.asked.push(marker.clone());
        }
        self.marker = marker;
        self.phase = Phase::Announced { at_s: now, asks };
        self.noticed_at = now;
        self.quiet = false;
        self.release.clear();
        self.ready_since = 0;
        self.last_stop.clear();
        self.work_seen = false;
    }

    fn released(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    fn dropped(&mut self) {
        self.release.clear();
        self.forget_markers();
    }

    fn notice_gone(&mut self, tab: &str, now: u64) -> String {
        let dropped = std::mem::take(&mut self.release);
        self.forget_markers();
        self.asked.clear();
        self.new_round(now);
        self.phase = Phase::Pending;
        self.notice_pid = 0;
        self.notice_start.clear();
        self.noted.clear();
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

    pub(super) fn forget_markers(&mut self) {
        self.marker.clear();
        self.markers.clear();
        self.ready_since = 0;
    }
}

pub(super) fn state_dir(opts: &Opts) -> PathBuf {
    opts.state.join("upgrade")
}

/// Validates that `dir`, if present, is a plain non-symlink directory owned by this user.
pub(super) fn check_owned_plain_dir_existing(dir: &Path) -> std::io::Result<bool> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: is a symbolic link", dir.display()),
        ));
    }
    if !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{}: is not a directory", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: owned by another user", dir.display()),
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if meta.file_attributes() & 0x0000_0400 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: is a reparse point", dir.display()),
            ));
        }
    }
    Ok(true)
}

fn create_owned_plain_dir(dir: &Path) -> std::io::Result<()> {
    if check_owned_plain_dir_existing(dir)? {
        return Ok(());
    }
    let create = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new().mode(0o700).create(dir)
        }
        #[cfg(not(unix))]
        {
            std::fs::DirBuilder::new().create(dir)
        }
    };
    match create {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    if check_owned_plain_dir_existing(dir)? {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{}: disappeared after create", dir.display()),
        ))
    }
}

/// Validates `<state>`, `<state>/upgrade`, and optional child `sub` without creating missing components.
pub(super) fn check_upgrade_tree(
    state: &Path,
    sub: Option<&str>,
) -> std::io::Result<Option<PathBuf>> {
    if !check_owned_plain_dir_existing(state)? {
        return Ok(None);
    }
    let upgrade = state.join("upgrade");
    if !check_owned_plain_dir_existing(&upgrade)? {
        return Ok(None);
    }
    let Some(sub) = sub else {
        return Ok(Some(upgrade));
    };
    let child = upgrade.join(sub);
    if !check_owned_plain_dir_existing(&child)? {
        return Ok(None);
    }
    Ok(Some(child))
}

/// Opens `<state>/upgrade` for listing after validating `<state>` and `<state>/upgrade`.
pub(super) fn read_upgrade_dir(state: &Path) -> std::io::Result<std::fs::ReadDir> {
    match check_upgrade_tree(state, None)? {
        Some(upgrade) => std::fs::read_dir(upgrade),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{}: absent", state.join("upgrade").display()),
        )),
    }
}

/// Ensures `<state>` (creating missing parents first) and optional child `sub` are owned plain
/// directories. On Unix, the producer-owned `<state>/upgrade` is private for durable native records.
pub(super) fn ensure_upgrade_tree(state: &Path, sub: Option<&str>) -> std::io::Result<PathBuf> {
    if !check_owned_plain_dir_existing(state)? {
        if let Some(parent) = state.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        create_owned_plain_dir(state)?;
    }
    let upgrade = state.join("upgrade");
    #[cfg(unix)]
    aterm_uds::private_file::ensure_private_dir(&upgrade)?;
    #[cfg(not(unix))]
    create_owned_plain_dir(&upgrade)?;
    let Some(sub) = sub else {
        return Ok(upgrade);
    };
    let child = upgrade.join(sub);
    create_owned_plain_dir(&child)?;
    Ok(child)
}

/// Small regular record files; a FIFO must never park the synchronous upgrade watcher.
fn read_record_text(path: &Path) -> std::io::Result<Option<String>> {
    super::accounts::read_bounded(path, 2 * 1024 * 1024)
}

/// A pending round this session already reached can close without touching its agent.
/// A person's live deferral and every unresolved native operation keep their custody.
fn pending_can_settle(st: &St, running: &Version, now: u64) -> bool {
    !st.native_retired_unissued
        && st.phase == Phase::Pending
        && st.release.is_empty()
        && !matches!(st.request, Request::DeferUntil(t) if t > now)
        && Version::parse(&st.to).is_some_and(|to| to <= *running)
        && !(super::native_restart::coordinated(st)
            && (st.native.is_some()
                || !st.native_prepare.is_empty()
                || st.native_account_candidate.is_some()))
}

fn forget_settled_pending(
    opts: &Opts,
    session: &str,
    prior: Option<&St>,
    running: &Version,
) -> Option<St> {
    let now = now_s();
    let st = prior.filter(|st| pending_can_settle(st, running, now))?;
    if opts.dry_run {
        return None;
    }
    if super::native_restart::coordinated(st) {
        let mut closed = st.clone();
        closed.phase = Phase::Done;
        closed.done_at = now;
        save_checked(opts, session, &closed).ok()?;
    } else {
        remove(opts, session);
    }
    Some(st.clone())
}

pub(super) const STATE_KEEP_S: u64 = 7 * 24 * 60 * 60;

pub(super) fn settled(st: &St) -> bool {
    if st.native_retired_unissued {
        return false;
    }
    match st.phase {
        Phase::Done | Phase::Failed(_) => true,
        Phase::Pending => st.request == Request::None,
        _ => false,
    }
}

const fn driven_here(agent: upgrade::Agent) -> bool {
    match agent {
        upgrade::Agent::Claude => true,
        upgrade::Agent::Codex | upgrade::Agent::Muse | upgrade::Agent::Agy => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabLife {
    Live,
    Gone,
    Unknown,
}

pub(super) fn registry_life<'a>(
    graph: Option<&'a Path>,
    known_live: &'a [String],
) -> impl Fn(&str) -> TabLife + 'a {
    fn entry(graph: &Path, sid: &str) -> Option<bool> {
        let hex = sid.strip_prefix("s-")?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match std::fs::symlink_metadata(graph.join(sid)) {
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        }
    }
    let vouched = graph.filter(|graph| {
        std::fs::symlink_metadata(graph).is_ok_and(|m| m.is_dir())
            && !known_live.is_empty()
            && known_live.iter().all(|sid| entry(graph, sid) == Some(true))
    });
    move |tab: &str| {
        if known_live.iter().any(|sid| sid == tab) {
            return TabLife::Live;
        }
        match vouched.and_then(|graph| entry(graph, tab)) {
            Some(true) => TabLife::Live,
            Some(false) => TabLife::Gone,
            None => TabLife::Unknown,
        }
    }
}

pub(super) const fn registry_vouches_for_state(
    private_registry: bool,
    private_state: bool,
) -> bool {
    private_registry == private_state
}

pub(super) fn registry_graph() -> Option<PathBuf> {
    registry_vouches_for_state(
        aterm_uds::control_socket_dir_is_private(),
        aterm_types::dirs::runs_under_private_state_root(),
    )
    .then(aterm_uds::control_socket_dir)
    .flatten()
    .map(|dir| dir.join("graph"))
}

pub(super) fn sat_a_week(path: &Path, now: u64) -> bool {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|m| m.modified().ok())
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|at| now.saturating_sub(at.as_secs()) >= STATE_KEEP_S)
}

fn prune_states(opts: &Opts, files: &[SessionFile], now: u64) {
    if opts.dry_run {
        return;
    }
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
        return;
    };
    let live: std::collections::BTreeSet<&str> =
        files.iter().map(|f| f.session_id.as_str()).collect();
    for entry in dir.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str());
        if name.is_some_and(|name| super::restart_stamp::prune(&opts.state, name, now)) {
            continue;
        }
        if let Some(id) = name.and_then(|name| name.strip_suffix(".writes")) {
            if !live.contains(id) && note_names_nothing(&path, id) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if let Some(id) = name.and_then(|name| name.strip_suffix(".move")) {
            record::prune_kept_move(opts, id, now);
            continue;
        }
        let Some(id) = name.and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if live.contains(id) || !sat_a_week(&path, now) {
            continue;
        }
        let Some(st) = read_record_text(&path)
            .ok()
            .flatten()
            .and_then(|text| St::from_json(&text))
        else {
            continue;
        };
        let held_elsewhere = st.home.is_some()
            && st
                .agent_records(&opts.home)
                .session_files()
                .is_none_or(|files| conversation_live(&files, id));
        if driven_here(st.agent)
            && settled(&st)
            && !super::native_restart::coordinated(&st)
            && !held_elsewhere
        {
            prune_if_unchanged(opts, id, &st, now);
        }
    }
}

fn note_names_nothing(note: &Path, session: &str) -> bool {
    note_names_nothing_with(
        read_record_text(note).and_then(|opt| {
            opt.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
        }),
        session,
        |path| {
            std::fs::read_dir(path)
                .map(|entries| entries.map(|entry| entry.map(|entry| entry.path())))
        },
        Path::try_exists,
    )
}

fn note_names_nothing_with<I>(
    text: std::io::Result<String>,
    session: &str,
    projects: impl FnOnce(&Path) -> std::io::Result<I>,
    mut exists: impl FnMut(&Path) -> std::io::Result<bool>,
) -> bool
where
    I: IntoIterator<Item = std::io::Result<PathBuf>>,
{
    let Ok(text) = text else {
        return false;
    };
    let noted = PathBuf::from(text.trim_end());
    if noted.as_os_str().is_empty() {
        return true;
    }
    match projects(&noted.join("projects")) {
        Ok(projects) => {
            for project in projects {
                let Ok(project) = project else {
                    return false;
                };
                match exists(&project.join(format!("{session}.jsonl"))) {
                    Ok(false) => {}
                    Ok(true) | Err(_) => return false,
                }
            }
            true
        }
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

pub(super) fn save(opts: &Opts, session: &str, st: &St) {
    save_at(opts, session, st, now_s());
}

/// Persist a transition before its caller dispatches native work. On Unix,
/// the directory must be private and success includes its durability barrier.
/// An error may follow rename, so retain the transition and retry it.
pub(super) fn save_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
    if opts.dry_run {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "dry-run cannot acknowledge a native record save",
        ));
    }
    record::save_at_checked(opts, session, st, now_s())
}

/// Restore request metadata without reporting new progress. Native records
/// still require a successful durability barrier before this returns.
pub(super) fn restore_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
    if opts.dry_run {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "dry-run cannot acknowledge a native record restore",
        ));
    }
    record::restore_checked(opts, session, st)
}

pub(super) fn build_starts(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    moving: bool,
) -> Result<(), &'static str> {
    build_start::starts(opts, r, to, now, owner_now, moving)
}

pub(super) fn build_proved(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
) -> Result<(), &'static str> {
    build_start::proved(opts, r, to, now)
}

pub(super) fn observed_build_start(
    opts: &Opts,
    r: &Report,
    st: &St,
    sf: &SessionFile,
    now: u64,
    confirm: impl FnOnce() -> bool,
) {
    build_start::observed_start(opts, r, st, sf, now, confirm);
}

mod record {
    use super::{Opts, St};
    use std::path::Path;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum WriteKind {
        Save,
        CheckedSave,
        Restore,
        CheckedRestore,
    }

    #[derive(Debug)]
    struct OtherAgentRecord;

    impl std::fmt::Display for OtherAgentRecord {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("record belongs to an unsupported non-native agent")
        }
    }

    impl std::error::Error for OtherAgentRecord {}

    fn lock(opts: &Opts, session: &str) -> std::io::Result<aterm_uds::file_lock::LockFile> {
        let dir = super::ensure_upgrade_tree(&opts.state, None)?;
        aterm_uds::file_lock::LockFile::take(
            &dir.join(format!("{session}.lock")),
            std::time::Instant::now(),
            "native session record is held",
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeAccountClientSlot",
            action = "StaleSave",
            project = "aterm_agent::harness::native_account_handoff::tests::transition"
        )
    )]
    fn check_pending(previous: Option<&St>, caller: &St, next: &St) -> std::io::Result<()> {
        if previous.is_some_and(|previous| {
            !super::super::native_account_handoff::preserves_pending(previous, next)
                && !super::super::native_record::permits_cold_retirement(caller, previous, next)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "native account candidate requires exact recovery",
            ));
        }
        Ok(())
    }

    fn replace_text(path: &Path, text: &str) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write as _;
            let (tmp, mut file) = aterm_uds::atomic_file::create_temp_beside(path, 0o666)?;
            let written = file.write_all(text.as_bytes());
            drop(file);
            let result = written.and_then(|()| std::fs::rename(&tmp, path));
            if result.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            result
        }
        #[cfg(not(unix))]
        {
            crate::harness::atomic_write::write_atomic(path, text)
        }
    }

    pub(in super::super) fn kept_move(opts: &Opts, session: &str) -> Option<St> {
        let dir = super::check_upgrade_tree(&opts.state, None).ok()??;
        St::from_json(&super::read_record_text(&dir.join(format!("{session}.move"))).ok()??)
            .filter(|st| st.moved_at != 0)
    }

    fn keep_move(opts: &Opts, session: &str, disk: &St) {
        let moved = super::super::relaunch::last_move_at(disk);
        if moved == 0 || kept_move(opts, session).is_some_and(|kept| kept.moved_at >= moved) {
            return;
        }
        let Ok(dir) = super::ensure_upgrade_tree(&opts.state, None) else {
            return;
        };
        let moved_login = super::super::relaunch::last_move_target(disk);
        let kept = St {
            phase: super::Phase::Done,
            agent: disk.agent,
            moved_at: moved,
            moved_login,
            ..St::default()
        };
        let path = dir.join(format!("{session}.move"));
        let _ = replace_text(&path, &kept.to_json());
    }

    pub(in super::super) fn prune_kept_move(opts: &Opts, session: &str, now: u64) -> bool {
        let Some(kept) = kept_move(opts, session) else {
            return false;
        };
        let spent = kept
            .moved_at
            .saturating_add(super::upgrade_status::LONGEST_LIMIT_S)
            .saturating_add(super::upgrade_status::LIMIT_GRACE_S);
        if now < spent || opts.dry_run {
            return true;
        }
        if let Ok(Some(dir)) = super::check_upgrade_tree(&opts.state, None) {
            let _ = std::fs::remove_file(dir.join(format!("{session}.move")));
        }
        false
    }

    pub(in super::super) fn load_record(opts: &Opts, session: &str) -> std::io::Result<Option<St>> {
        let Some(dir) = super::check_upgrade_tree(&opts.state, None)? else {
            return Ok(None);
        };
        match super::read_record_text(&dir.join(format!("{session}.json")))? {
            None => Ok(None),
            Some(text) => St::from_json(&text).map(Some).ok_or_else(|| {
                if other_non_native_agent(&text) {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, OtherAgentRecord)
                } else {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "existing session record is unreadable",
                    )
                }
            }),
        }
    }

    fn other_non_native_agent(text: &str) -> bool {
        let Ok(value) = aterm_json::from_str::<aterm_json::Value>(text) else {
            return false;
        };
        let Some(agent) = value.get("agent").and_then(aterm_json::Value::as_str) else {
            return false;
        };
        if super::upgrade::Agent::parse(agent).is_some_and(|agent| {
            super::super::lanes::lanes(agent.program()) != super::super::lanes::Lanes::NONE
        }) {
            return false;
        }
        // Unknown agents still quarantine if any native authority or debt may
        // be present. Empty strings/false flags carry no native operation.
        ![
            "native_id",
            "native_cap",
            "native_completed_prepare",
            "native_noexec_prepare",
            "native_observation",
            "native_account_candidate",
            "native_generation",
            "native_write_id",
        ]
        .iter()
        .any(|key| value.get(key).is_some())
            && [
                "native_coordination",
                "native_retired_unissued",
                "native_rollback",
            ]
            .iter()
            .all(|key| value.get(key).is_none_or(|v| v.as_bool() == Some(false)))
            && value
                .get("native_prepare")
                .is_none_or(|v| v.as_str() == Some(""))
    }

    pub(in super::super) fn save_at(opts: &Opts, session: &str, st: &St, now: u64) {
        let _ = save_at_with(opts, session, st, now, WriteKind::Save);
    }

    pub(in super::super) fn save_at_checked(
        opts: &Opts,
        session: &str,
        st: &St,
        now: u64,
    ) -> std::io::Result<()> {
        save_at_with(opts, session, st, now, WriteKind::CheckedSave)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "AttemptBefore",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "AttemptAfter",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "Rival",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "Retry",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "LateRetry",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeWriteIdentity",
            action = "Fresh",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::identity::transition"
        )
    )]
    fn save_at_with(
        opts: &Opts,
        session: &str,
        st: &St,
        now: u64,
        kind: WriteKind,
    ) -> std::io::Result<()> {
        if opts.dry_run {
            return Ok(());
        }
        // Atomic rename protects readers, but not two read/modify/write callers.
        // A separate stable inode serializes every producer of this record.
        let _held = lock(opts, session)?;
        let key = st.progress_key();
        let on_disk = load_record(opts, session)?;
        let mut out = st.clone();
        // Match the serializer's conservative interpretation before capturing
        // the acknowledged baseline.
        out.native_retired_unissued |= out.native_account_candidate.is_some();
        out.native_coordination |= super::super::native_restart::coordinated(st)
            || on_disk
                .as_ref()
                .is_some_and(super::super::native_restart::coordinated);
        check_pending(on_disk.as_ref(), st, &out)?;
        super::super::native_record::check(on_disk.as_ref(), st, &out)?;
        let restoring = matches!(kind, WriteKind::Restore | WriteKind::CheckedRestore);
        if !restoring {
            match &on_disk {
                Some(disk) if disk.progress_key == key => out.progress_at = disk.progress_at,
                _ => out.progress_at = now,
            }
            out.progress_key = key;
        }
        let kept = on_disk
            .is_none()
            .then(|| kept_move(opts, session))
            .flatten();
        for disk in on_disk.iter().chain(kept.iter()) {
            let moved = super::super::relaunch::last_move_at(disk);
            if moved != 0 && moved >= out.moved_at {
                out.moved_login = super::super::relaunch::last_move_target(disk);
            }
            out.moved_at = out.moved_at.max(moved);
        }
        if let Some(disk) = &on_disk {
            out.tabs = super::earlier_tabs(
                &out.tab,
                disk.tabs
                    .iter()
                    .chain(std::iter::once(&disk.tab))
                    .chain(out.tabs.iter()),
            );
            let mut until = disk.tabs_until.clone();
            for (tab, at) in &out.tabs_until {
                super::stamp_tab(&mut until, tab, *at);
            }
            if !disk.tab.is_empty() && disk.tab != out.tab {
                super::stamp_tab(&mut until, &disk.tab, now);
            }
            until.retain(|(tab, _)| out.tabs.contains(tab));
            out.tabs_until = until;
        }
        // A status rewrite must not weaken a previously acknowledged native
        // record, including a transition that clears its last native field.
        write_record(
            opts,
            session,
            &out,
            on_disk.as_ref(),
            matches!(kind, WriteKind::CheckedSave | WriteKind::CheckedRestore),
        )?;
        super::super::native_record::acknowledge(st, &out);
        if !restoring {
            super::build_start::note(opts, session, on_disk.as_ref(), st, now);
        }
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeAccountClientSlot",
            action = "Rewrite",
            project = "aterm_agent::harness::native_account_handoff::tests::transition"
        )
    )]
    fn write_record(
        opts: &Opts,
        session: &str,
        st: &St,
        previous: Option<&St>,
        checked: bool,
    ) -> std::io::Result<()> {
        if checked
            || super::super::native_restart::coordinated(st)
            || previous.is_some_and(super::super::native_restart::coordinated)
        {
            write_verbatim_checked(opts, session, st)
        } else {
            write_verbatim(opts, session, st)
        }
    }

    fn write_verbatim(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
        if opts.dry_run {
            return Ok(());
        }
        let dir = super::ensure_upgrade_tree(&opts.state, None)?;
        replace_text(&dir.join(format!("{session}.json")), &st.to_json())
    }

    fn write_verbatim_checked(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
        if opts.dry_run {
            return Ok(());
        }
        let dir = super::ensure_upgrade_tree(&opts.state, None)?;
        let name = format!("{session}.json");
        let text = st.to_json();
        if super::super::native_restart::coordinated(st) {
            super::super::native_record::validate_encoding(st, &text)?;
        }
        #[cfg(unix)]
        {
            aterm_journal::write_private_durable(&dir, &name, text.as_bytes())
        }
        #[cfg(not(unix))]
        {
            // Keep the existing portable replacement route, returning its
            // failure to the checked caller instead of hiding it.
            crate::harness::atomic_write::write_atomic(&dir.join(name), &text)
        }
    }

    pub(in super::super) fn restore(opts: &Opts, session: &str, prior: Option<&St>) {
        if opts.dry_run {
            return;
        }
        match prior {
            Some(st) => {
                let _ = restore_record(opts, session, st);
            }
            None => remove(opts, session),
        }
    }

    fn restore_record(opts: &Opts, session: &str, st: &St) -> std::io::Result<()> {
        save_at_with(opts, session, st, super::now_s(), WriteKind::Restore)
    }

    pub(in super::super) fn restore_checked(
        opts: &Opts,
        session: &str,
        st: &St,
    ) -> std::io::Result<()> {
        save_at_with(opts, session, st, super::now_s(), WriteKind::CheckedRestore)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeRecord",
            action = "Remove",
            project = "aterm_agent::harness::native_account_handoff::tests::record_tests::transition"
        )
    )]
    pub(in super::super) fn remove(opts: &Opts, session: &str) {
        remove_when(opts, session, |_, _| true);
    }

    pub(in super::super) fn prune_if_unchanged(
        opts: &Opts,
        session: &str,
        expected: &St,
        now: u64,
    ) {
        let expected = expected.to_json();
        remove_when(opts, session, |disk, path| {
            disk.to_json() == expected && super::sat_a_week(path, now)
        });
    }

    fn remove_when(opts: &Opts, session: &str, eligible: impl FnOnce(&St, &Path) -> bool) {
        if opts.dry_run {
            return;
        }
        let Ok(_held) = lock(opts, session) else {
            return;
        };
        let Ok(previous) = load_record(opts, session) else {
            return;
        };
        let Ok(Some(dir)) = super::check_upgrade_tree(&opts.state, None) else {
            return;
        };
        let path = dir.join(format!("{session}.json"));
        if let Some(disk) = previous {
            if super::super::native_restart::coordinated(&disk) || !eligible(&disk, &path) {
                return;
            }
            keep_move(opts, session, &disk);
        }
        let _ = std::fs::remove_file(path);
    }

    #[cfg(all(test, unix))]
    pub(in super::super) fn write_verbatim_for_test(opts: &Opts, session: &str, st: &St) {
        let _ = write_verbatim(opts, session, st);
    }

    #[cfg(all(test, unix))]
    pub(in super::super) fn ordinary_write_for_test(
        opts: &Opts,
        session: &str,
        st: &St,
        restore: bool,
    ) -> std::io::Result<()> {
        if restore {
            restore_record(opts, session, st)
        } else {
            save_at_with(opts, session, st, super::now_s(), WriteKind::Save)
        }
    }

    #[cfg(all(test, unix))]
    pub(in super::super) fn native_writer_for_test(
        opts: &Opts,
        session: &str,
        st: &St,
        previous: Option<&St>,
        best_effort: bool,
    ) -> std::io::Result<()> {
        if best_effort {
            let dir = super::ensure_upgrade_tree(&opts.state, None)?;
            aterm_uds::private_file::write_private(
                &dir,
                &format!("{session}.json"),
                st.to_json().as_bytes(),
            )
        } else {
            write_record(opts, session, st, previous, false)
        }
    }
}

#[cfg(all(test, unix))]
use record::write_verbatim_for_test as write_verbatim;
pub(super) use record::{kept_move, load_record, prune_if_unchanged, remove, restore, save_at};

/// Read an expected-readable unit-test fixture through the production decoder.
///
/// # Panics
/// The fixture exists but cannot be read as a valid record.
#[cfg(test)]
pub(super) fn load(opts: &Opts, session: &str) -> Option<St> {
    load_record(opts, session).expect("read the fixture record")
}
#[cfg(all(test, unix))]
pub(super) use record::{native_writer_for_test, ordinary_write_for_test};

/// Best-effort event row, framed and appended under the ledger-cut path lock.
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
    let Ok(dir) = ensure_upgrade_tree(&opts.state, None) else {
        return;
    };
    let path = dir.join("ledger.jsonl");
    let Ok(row) = aterm_json::to_string(&Value::Object(o)) else {
        return;
    };
    if aterm_uds::line_log::append(&path, &row).is_err() {
        return;
    }
    let _ = super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS);
}

/// `<state>/upgrade/ledger.jsonl` — what `aterm harness ledger upgrade` reads.
#[must_use]
pub fn ledger_path(opts: &Opts) -> PathBuf {
    state_dir(opts).join("ledger.jsonl")
}

const LEDGER_MAX_BYTES: u64 = super::cli::DISK_LEDGER_MAX_BYTES;

const LEDGER_KEEP_ROWS: usize = super::cli::DISK_LEDGER_KEEP_ROWS;

#[cfg(test)]
thread_local! {
    static SESSION_FILE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn session_files(home: &Path) -> Option<Vec<SessionFile>> {
    session_files_in(&home.join(".claude"))
}

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
            .and_then(crate::parse_uint::<u32>)
        else {
            continue;
        };
        let file = read_record_text(&path)
            .ok()
            .flatten()
            .and_then(|text| upgrade::parse_session_file(&text).ok());
        match file {
            Some(file) if file.pid == file_pid => files.push(SessionFile {
                claude_dir: Some(claude_dir.to_path_buf()),
                ..file
            }),
            Some(file)
                if !may_have_written(file_pid, &path) && !may_have_written(file.pid, &path) => {}
            None if !may_have_written(file_pid, &path) => {}
            _ => return None,
        }
    }
    Some(files)
}

/// Whether `pid` may be the live writer of the session file at `path` ([`session_files_in`]):
/// [`writer_may_live`], and the process at `pid` started no later than the file was last
/// written, in whole seconds (an unread start or write time may), since a process that took the
/// number after that never wrote it. Claude Code's own sweep removes a stale file only on
/// `ESRCH`, so a file a crash left half-written stays while any process holds its number.
fn may_have_written(pid: u32, path: &Path) -> bool {
    if !writer_may_live(pid) {
        return false;
    }
    let written = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_secs());
    match (started_s(pid), written) {
        (Some(started), Some(written)) => started <= written,
        _ => true,
    }
}

/// Whether `pid` may be this user's live Claude Code, writing the session file it names
/// ([`session_files_in`], [`may_have_written`]): a signal reaches it, where [`alive`] also
/// counts `EPERM`, another user's process (root's included) that took the number and keeps the
/// file from being judged; and, on macOS, it runs as this effective user and is no zombie,
/// which `kill` answers as alive and [`aterm_uds::process::read_short_info`] does not find.
pub(super) fn writer_may_live(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if p <= 0 {
            return false;
        }
        // SAFETY: signal 0 checks existence and permission; nothing is delivered.
        if unsafe { libc::kill(p, 0) } != 0 {
            return false;
        }
        #[cfg(target_vendor = "apple")]
        {
            // SAFETY: `geteuid` is a side-effect-free getter.
            let me = unsafe { libc::geteuid() };
            aterm_uds::process::read_short_info(p).is_some_and(|info| info.pbsi_uid == me)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            true
        }
    }
    #[cfg(not(unix))]
    {
        alive(pid)
    }
}

pub(super) const SESSION_ROSTER_RESCAN: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct SessionRosterCache {
    stamp: Option<SystemTime>,
    scanned_at: Option<Instant>,
    files: Option<Vec<SessionFile>>,
}

impl SessionRosterCache {
    pub(super) fn files(&mut self, records: &Records, now: Instant) -> Option<&[SessionFile]> {
        let dir = records.dir()?;
        let stamp = std::fs::metadata(dir.join("sessions"))
            .and_then(|m| m.modified())
            .ok();
        self.files_at(stamp, now, || records.session_files())
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
            self.stamp = stamp;
            self.scanned_at = Some(now);
            self.files = scan();
        }
        self.files.as_deref()
    }
}

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

fn preliminary_unique_owner(
    files: Option<&[SessionFile]>,
    wanted: &SessionFile,
) -> Result<(), &'static str> {
    let files = files.ok_or("session-files-unreadable")?;
    unique_live_owner(files, wanted)
        .then_some(())
        .ok_or("conversation-owner-ambiguous")
}

pub(super) fn require_unique_owner(home: &Path, wanted: &SessionFile) -> Result<(), &'static str> {
    Records::of_file(home, wanted).unique_owner(wanted)
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct HostClaim {
    tab: String,
    group: i64,
    parent: u32,
}

type Claimed<'a> = (&'a SessionFile, atpkg::caller_shell::ProcArgs, HostClaim);

fn host_candidates<'a>(
    files: &'a [SessionFile],
    live_tabs: &[LiveTab],
    mut needs_inspection: impl FnMut(&SessionFile, &str) -> bool,
    mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    mut read_group: impl FnMut(u32) -> Option<i64>,
    mut read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Vec<Result<Claimed<'a>, Report>> {
    aterm_log::debug!(
        "upgrade candidates: records={} tabs={}",
        files.len(),
        live_tabs.len()
    );
    if !live_tabs.iter().any(|tab| tab.fgpgid.is_some()) {
        aterm_log::debug!(
            "upgrade candidates: no foreground tab, records={}",
            files.len()
        );
        return Vec::new();
    }
    files
        .iter()
        .filter(|sf| {
            let supported = sf.kind == "interactive"
                && sf.entrypoint == "cli"
                && upgrade::is_session_id(&sf.session_id);
            if !supported {
                aterm_log::debug!("upgrade candidate pid={} rejected=record-kind", sf.pid);
            }
            supported
        })
        .filter_map(|sf| {
            let Some(group) = read_group(sf.pid) else {
                aterm_log::debug!("upgrade candidate pid={} rejected=group-unread", sf.pid);
                return None;
            };
            let Some(tab) = unique_tab_for_group(live_tabs, group) else {
                aterm_log::debug!("upgrade candidate pid={} rejected=tab-attribution", sf.pid);
                return None;
            };
            if !needs_inspection(sf, tab) {
                aterm_log::debug!("upgrade candidate pid={} rejected=no-inspection", sf.pid);
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
                aterm_log::debug!("upgrade candidate pid={} rejected=parent-session", sf.pid);
                return None;
            }
            aterm_log::debug!(
                "upgrade candidate pid={} accepted tab={tab} group={group}",
                sf.pid
            );
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

fn needs_upgrade_inspection(sf: &SessionFile, targets: &Targets, prior: Option<&St>) -> bool {
    // A restart in flight, a release owed whatever build the agent runs now
    // ([`release_visit`]), and an announced round: its READY must still be acted on, or the
    // round released once no build is ahead of the one it runs
    // ([`St::release_untargeted`]). A gave-up round's late READY acts only while its release is
    // owed, which this lets through.
    if prior.is_some_and(|st| {
        st.in_flight() || !st.release.is_empty() || matches!(st.phase, Phase::Announced { .. })
    }) {
        return true;
    }
    if prior.is_some_and(|st| st.landed_after_stop(sf, &Live)) {
        return true;
    }
    let Some(running) = running_version(sf) else {
        return true;
    };
    // The pending record's last wait ages until a real step closes it. Reaching its build by
    // another route owes that one visit even when no newer target exists.
    if prior.is_some_and(|st| pending_can_settle(st, &running, now_s())) {
        return true;
    }
    let needed = targets
        .managed
        .iter()
        .chain(targets.native.iter())
        .any(|target| target.version > running);
    aterm_log::debug!(
        "upgrade eligibility pid={} record={} managed={:?} native={:?} needed={needed}",
        sf.pid,
        sf.version,
        targets.managed.as_ref().map(|target| &target.version),
        targets.native.as_ref().map(|target| &target.version),
    );
    needed
}

/// Whether a tab's step visits `sf`: a session the host would inspect
/// ([`needs_upgrade_inspection`]), or any session while the managed build is unread
/// ([`Targets::unanswered`]). No read target is ahead of a session behind a build nobody read, so
/// the first alone drops it and its tab's step falls back to `current`; visited, it says
/// `wait:no-version` ([`Targets::untargeted_word`]), which the watch records as its wait. The
/// host's own look keeps the narrower question and answers [`Due::Unread`] for the same session
/// ([`due_among`]).
fn step_visits(sf: &SessionFile, targets: &Targets, prior: Option<&St>) -> bool {
    needs_upgrade_inspection(sf, targets, prior) || targets.unanswered
}

fn claim_still_live(c: &mut Client, pid: u32, claim: &HostClaim) -> bool {
    process_in_tab(c, pid, &claim.tab, Some(claim.group), Some(claim.parent))
}

#[cfg(all(test, unix))]
pub(super) fn session_file_of(home: &Path, pid: u32) -> Option<SessionFile> {
    session_file_in(&home.join(".claude"), pid)
}

pub(super) fn session_file_in(claude_dir: &Path, pid: u32) -> Option<SessionFile> {
    let t = read_record_text(&claude_dir.join(format!("sessions/{pid}.json"))).ok()??;
    let file = upgrade::parse_session_file(&t).ok()?;
    Some(SessionFile {
        claude_dir: Some(claude_dir.to_path_buf()),
        ..file
    })
}

pub(super) fn transcript_exists(home: &Path, session: &str) -> Option<bool> {
    transcript_exists_in(&home.join(".claude"), session)
}

pub(super) fn transcript_exists_in(claude_dir: &Path, session: &str) -> Option<bool> {
    let name = format!("{session}.jsonl");
    match std::fs::read_dir(claude_dir.join("projects")) {
        Ok(dirs) => Some(
            dirs.flatten()
                .any(|e| crate::harness::entry_is_dir(&e) && e.path().join(&name).is_file()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && claude_dir.is_dir() => Some(false),
        Err(_) => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Records {
    dir: Option<PathBuf>,
    profile: Option<PathBuf>,
}

pub(super) fn profile_apart(
    user_home: &Path,
    vars: HomeVars<'_>,
    dir: Option<&Path>,
) -> Option<PathBuf> {
    match vars.config {
        Some("") => Some(homedir(user_home, vars.home).join(".claude.json")),
        Some(_) => dir
            .filter(|dir| *dir == user_home.join(".claude"))
            .map(|dir| dir.join(".claude.json")),
        None => dir
            .filter(|dir| *dir != user_home.join(".claude"))
            .and_then(Path::parent)
            .map(|home| home.join(".claude.json")),
    }
}

pub(super) fn profile_in(user_home: &Path, vars: HomeVars<'_>, dir: &Path) -> PathBuf {
    profile_apart(user_home, vars, Some(dir)).unwrap_or_else(|| claude_profile(user_home, dir))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HomeVars<'a> {
    pub(super) config: Option<&'a str>,
    pub(super) home: Option<&'a str>,
}

impl<'a> HomeVars<'a> {
    pub(super) fn of(args: &'a atpkg::caller_shell::ProcArgs) -> Self {
        Self {
            config: args.env_var(super::accounts::HOME_ENV),
            home: args.env_var(super::accounts::HOMEDIR_ENV),
        }
    }

    pub(super) const fn config(config: Option<&'a str>) -> Self {
        Self { config, home: None }
    }
}

fn homedir(user_home: &Path, home: Option<&str>) -> PathBuf {
    home.map(Path::new)
        .filter(|home| home.is_absolute())
        .map_or_else(|| user_home.to_path_buf(), Path::to_path_buf)
}

pub(super) fn claude_home_dir(
    fallback: Option<&Path>,
    vars: HomeVars<'_>,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match vars.config {
        Some(config) => home_dir_at(config, cwd),
        None => vars
            .home
            .map(Path::new)
            .filter(|home| home.is_absolute())
            .or(fallback)
            .map(|home| home.join(".claude")),
    }
}

pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

pub(super) fn home_dir_at(home: &str, cwd: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    let base = if Path::new(home).is_absolute() {
        PathBuf::from("/")
    } else {
        cwd().filter(|cwd| cwd.is_absolute())?
    };
    claude_dir_at(Some(home), None, &base, || None).ok()
}

impl Records {
    pub(super) fn at(
        user_home: &Path,
        vars: HomeVars<'_>,
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> Self {
        let dir = claude_home_dir(Some(user_home), vars, cwd);
        Self::in_dir(user_home, vars, dir)
    }

    pub(super) fn of_process(
        user_home: &Path,
        vars: HomeVars<'_>,
        pid: u32,
        cwd: impl FnOnce() -> Option<PathBuf>,
        started_in: Option<&str>,
    ) -> Self {
        let Some(relative) = vars.config.filter(|home| Path::new(home).is_relative()) else {
            return Self::at(user_home, vars, || None);
        };
        let vars = HomeVars {
            config: Some(relative),
            ..vars
        };
        let now = cwd();
        let started = started_in
            .map(Path::new)
            .filter(|dir| dir.is_absolute())
            .and_then(|dir| std::fs::canonicalize(dir).ok());
        let start = std::cell::OnceCell::new();
        for base in [now.clone(), started].into_iter().flatten() {
            let records = Self::at(user_home, vars, || Some(base));
            let its_own = records.session_file(pid).is_some_and(|sf| {
                start.get_or_init(|| kernel_start(pid)).as_deref()
                    == Some(squash(&sf.proc_start).as_str())
            });
            if its_own {
                return records;
            }
        }
        Self::at(user_home, vars, || now)
    }

    pub(super) fn left_by(
        &self,
        user_home: &Path,
        home: Option<&str>,
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> bool {
        let Some(relative) = home.filter(|home| Path::new(home).is_relative()) else {
            return false;
        };
        match (
            self.dir(),
            Self::at(user_home, HomeVars::config(Some(relative)), cwd).dir(),
        ) {
            (Some(filed), Some(now)) => !same_dir(filed, now),
            _ => true,
        }
    }

    fn in_dir(user_home: &Path, vars: HomeVars<'_>, dir: Option<PathBuf>) -> Self {
        Self {
            profile: profile_apart(user_home, vars, dir.as_deref()),
            dir,
        }
    }

    pub(super) fn of(user_home: &Path, home: Option<&str>) -> Self {
        Self::at(user_home, HomeVars::config(home), || None)
    }

    pub(super) fn of_recorded(user_home: &Path, home: Option<&str>, profile: Option<&str>) -> Self {
        let of = Self::of(user_home, home);
        Self {
            profile: profile.map(PathBuf::from).or(of.profile),
            dir: of.dir,
        }
    }

    pub(super) fn of_snapshot(user_home: &Path, snap: &super::relaunch::Snapshot) -> Self {
        match &snap.claude_dir {
            Some(dir) => Self::in_dir(
                user_home,
                HomeVars::config(snap.home.as_deref()),
                Some(dir.clone()),
            ),
            None => Self::at(user_home, HomeVars::config(snap.home.as_deref()), || {
                Some(PathBuf::from(&snap.cwd))
            }),
        }
    }

    pub(super) fn of_file(user_home: &Path, sf: &SessionFile) -> Self {
        Self {
            dir: Some(
                sf.claude_dir
                    .clone()
                    .unwrap_or_else(|| user_home.join(".claude")),
            ),
            profile: sf.profile.clone(),
        }
    }

    pub(super) fn home_word(&self, user_home: &Path) -> Option<String> {
        self.dir
            .as_ref()
            .filter(|dir| **dir != user_home.join(".claude"))
            .map(|dir| dir.to_string_lossy().into_owned())
    }

    pub(super) fn profile_word(&self) -> Option<String> {
        self.profile
            .as_ref()
            .map(|profile| profile.to_string_lossy().into_owned())
    }

    pub(super) fn session_file(&self, pid: u32) -> Option<SessionFile> {
        session_file_in(self.dir.as_deref()?, pid).map(|sf| self.stamped(sf))
    }

    pub(super) fn session_files(&self) -> Option<Vec<SessionFile>> {
        session_files_in(self.dir.as_deref()?)
            .map(|files| files.into_iter().map(|sf| self.stamped(sf)).collect())
    }

    fn stamped(&self, sf: SessionFile) -> SessionFile {
        SessionFile {
            profile: self.profile.clone(),
            ..sf
        }
    }

    pub(super) fn profile(&self, user_home: &Path) -> Option<PathBuf> {
        self.profile.clone().or_else(|| {
            self.dir
                .as_deref()
                .map(|dir| claude_profile(user_home, dir))
        })
    }

    pub(super) fn billing_context(
        &self,
        user_home: &Path,
        args: &atpkg::caller_shell::ProcArgs,
    ) -> Option<super::profile::Context> {
        let dir = self.dir()?;
        super::profile::Context::of_exec_env(user_home, dir, &args.env)
    }

    pub(super) fn transcript(&self, session: &str) -> Option<PathBuf> {
        transcript_in(self.dir.as_deref()?, session)
    }

    pub(super) fn transcript_exists(&self, session: &str) -> Option<bool> {
        transcript_exists_in(self.dir.as_deref()?, session)
    }

    pub(super) fn unique_owner(&self, wanted: &SessionFile) -> Result<(), &'static str> {
        preliminary_unique_owner(self.session_files().as_deref(), wanted)
    }

    pub(super) fn tasked(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        Tasked::default().of_in(self.dir.as_deref()?, session, ours)
    }

    pub(super) fn finished(&self, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.finished_in(&LOOK_MEMOS, session, ours)
    }

    pub(super) fn finished_in(
        &self,
        memos: &std::sync::Mutex<Vec<Finished>>,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        let dir = self.dir.as_deref()?;
        let held = |m: &Finished| m.session == session && m.dir.as_deref() == Some(dir);
        let taken = memos.lock().ok().and_then(|mut memos| {
            let at = memos.iter().position(held)?;
            Some(memos.remove(at))
        });
        let mut memo = taken.unwrap_or_default();
        let finished = memo.of_in_from(dir, session, ours, FINISHED_ONCE_BYTES);
        if let Ok(mut memos) = memos.lock() {
            memos.retain(|m| !held(m));
            memos.push(memo);
            let over = memos.len().saturating_sub(LOOK_MEMOS_KEPT);
            memos.drain(..over);
        }
        finished
    }

    pub(super) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
}

/// Whether conversation `session` has a task ([`upgrade::TaskScan`]), read once.
#[must_use]
pub fn tasked(home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
    Tasked::default().of(home, session, ours)
}

/// What the supervisor of tab `tab` typed into it.
/// # Errors
/// The conversation's existing ledger or lineage cannot be read as bounded regular text.
pub fn supervisor_typed(opts: &Opts, tab: &str) -> std::io::Result<Vec<approvals::Typed>> {
    conversation_typed(opts, tab, None, &[])
}

pub(super) const EARLIER_TABS: usize = 8;

/// What the harness typed into a conversation, keyed to the conversation rather than its current tab.
/// # Errors
/// An existing ledger or lineage file cannot be read as bounded regular text. The caller
/// must retain an unknown task or hold rather than treating this as no harness acts.
pub fn conversation_typed(
    opts: &Opts,
    tab: &str,
    session: Option<&str>,
    extra: &[String],
) -> std::io::Result<Vec<approvals::Typed>> {
    let record = session.map(|s| load_record(opts, s)).transpose()?.flatten();
    typed_through(opts, tab, session, record.as_ref(), extra, &[])
}

pub(super) fn conversation_typed_by(
    opts: &Opts,
    tab: &str,
    session: &str,
    st: &St,
) -> std::io::Result<Vec<approvals::Typed>> {
    let record = load_record(opts, session)?;
    typed_through(
        opts,
        tab,
        Some(session),
        record.as_ref(),
        &st.tabs,
        &st.tabs_until,
    )
}

fn stamp_tab(stamps: &mut Vec<(String, u64)>, tab: &str, until: u64) {
    match stamps.iter_mut().find(|(t, _)| t == tab) {
        Some((_, at)) => *at = (*at).max(until),
        None => stamps.push((tab.to_string(), until)),
    }
}

fn typed_through(
    opts: &Opts,
    tab: &str,
    session: Option<&str>,
    record: Option<&St>,
    extra: &[String],
    until: &[(String, u64)],
) -> std::io::Result<Vec<approvals::Typed>> {
    let Some(root) = opts.aterm_state.as_deref() else {
        return Ok(Vec::new());
    };
    let ledgered = if let Some(session) = session {
        let text = match check_upgrade_tree(&opts.state, None)? {
            Some(dir) => {
                super::accounts::read_bounded(&dir.join("ledger.jsonl"), 16 * 1024 * 1024)?
                    .unwrap_or_default()
            }
            None => String::new(),
        };
        ledger_tabs(&text, session, tab)
    } else {
        Vec::new()
    };
    let mut stamps: Vec<(String, u64)> = Vec::new();
    for (t, at) in until
        .iter()
        .chain(record.iter().flat_map(|st| st.tabs_until.iter()))
    {
        stamp_tab(&mut stamps, t, *at);
    }
    for (t, at) in &ledgered {
        if let Some(at) = at {
            stamp_tab(&mut stamps, t, *at);
        }
    }
    let extra: Vec<String> = ledgered
        .into_iter()
        .map(|(t, _)| t)
        .chain(extra.iter().cloned())
        .collect();
    let mut typed = Vec::new();
    for t in tabs_through(root, tab, record, &extra)? {
        let rows = approvals::read_typed_texts(&approvals::ledger_under(root, Some(&t)), Some(&t))?;
        let last_ms = (t != tab)
            .then(|| stamps.iter().find(|(s, _)| s == &t))
            .flatten()
            .map(|(_, at)| {
                i64::try_from(
                    at.saturating_mul(1000)
                        .saturating_add(upgrade::TYPED_WITHIN_MS),
                )
                .unwrap_or(i64::MAX)
            });
        typed.extend(
            rows.into_iter()
                .filter(|r| last_ms.is_none_or(|last| r.at_ms <= last)),
        );
    }
    Ok(typed)
}

pub(super) fn ledger_tabs(body: &str, session: &str, tab: &str) -> Vec<(String, Option<u64>)> {
    let rows: Vec<(u64, String, String)> = body
        .lines()
        .filter_map(|line| {
            let v: Value = aterm_json::from_str(line).ok()?;
            let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
            Some((v.get("t")?.as_u64()?, field("tab")?, field("session")?))
        })
        .collect();
    let mut last: Vec<(String, usize)> = Vec::new();
    for (i, (_, t, s)) in rows.iter().enumerate() {
        if s == session && t != tab && super::tap::is_tab_id(t) {
            last.retain(|(x, _)| x != t);
            last.push((t.clone(), i));
        }
    }
    last.into_iter()
        .map(|(t, i)| {
            let left = rows.iter().skip(i + 1).find(|(_, rt, rs)| {
                (*rt == t && rs != session && rs != "-") || (rs == session && *rt != t && rt != "-")
            });
            let until = left.map(|(at, _, _)| *at);
            (t, until)
        })
        .collect()
}

fn tabs_through(
    root: &Path,
    tab: &str,
    record: Option<&St>,
    extra: &[String],
) -> std::io::Result<Vec<String>> {
    let refused = std::cell::RefCell::new(None);
    let tabs = tabs_through_read(
        root,
        tab,
        record,
        extra,
        &|p| match super::accounts::read_bounded(p, 1024 * 1024) {
            Ok(body) => body,
            Err(why) => {
                refused.borrow_mut().get_or_insert(why);
                None
            }
        },
        &|_| false,
    );
    match refused.into_inner() {
        Some(why) => Err(why),
        None => Ok(tabs),
    }
}

fn tabs_through_read(
    root: &Path,
    tab: &str,
    record: Option<&St>,
    extra: &[String],
    read: &dyn Fn(&Path) -> Option<String>,
    ends: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut tabs = vec![tab.to_string()];
    tabs.extend(earlier_tabs(
        tab,
        extra.iter().chain(
            record
                .iter()
                .flat_map(|st| st.tabs.iter().chain(std::iter::once(&st.tab))),
        ),
    ));
    let mut at = 0;
    while at < tabs.len() {
        if at > 0 && ends(&tabs[at]) {
            at += 1;
            continue;
        }
        for t in noted_earlier_tabs_read(root, &tabs[at], read) {
            if tabs.len() > EARLIER_TABS {
                break;
            }
            if !tabs.contains(&t) {
                tabs.push(t);
            }
        }
        at += 1;
    }
    tabs
}

pub(super) fn lefts_through(
    opts: &Opts,
    tab: &str,
    session: &str,
    st: &St,
) -> std::io::Result<Vec<String>> {
    let Some(root) = opts.aterm_state.as_deref() else {
        return Ok(Vec::new());
    };
    let record = load_record(opts, session)?;
    let mut lefts = Vec::new();
    for t in tabs_through(root, tab, record.as_ref(), &st.tabs)? {
        if let Some(key) =
            approvals::read_open_left(&approvals::ledger_under(root, Some(&t)), Some(&t))?
        {
            lefts.push(key);
        }
    }
    Ok(lefts)
}

/// [`lefts_through`] for a LOOP that keeps its ledger at `ledger` for session `sid`.
#[must_use]
pub fn lefts_beside(ledger: &Path, sid: Option<&str>) -> Vec<String> {
    let read = |p: &Path| read_record_text(p).ok().flatten();
    lefts_beside_in(ledger, &read(ledger).unwrap_or_default(), sid, &read)
}

/// [`lefts_beside`] for a loop that read its own ledger at `ledger` once and reads every file beside it through `read`.
#[must_use]
pub fn lefts_beside_in(
    ledger: &Path,
    body: &str,
    sid: Option<&str>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<String> {
    let earlier: Vec<(String, String)> = earlier_ledgers_in(ledger, sid, read)
        .into_iter()
        .filter_map(|(t, p)| Some((t, read(&p)?)))
        .collect();
    lefts_of(body, sid, &earlier)
}

/// [`lefts_beside_in`] over the earlier tabs' ledgers already read (`earlier`: each tab and its ledger's text).
#[must_use]
pub fn lefts_of(body: &str, sid: Option<&str>, earlier: &[(String, String)]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let open = approvals::open_left_in(body, sid).into_iter().chain(
        earlier
            .iter()
            .filter_map(|(t, body)| approvals::open_left_in(body, Some(t))),
    );
    for key in open {
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The earlier tabs' ledgers beside the loop ledger at `ledger` of session `sid`.
#[must_use]
pub fn earlier_ledgers_in(
    ledger: &Path,
    sid: Option<&str>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<(String, PathBuf)> {
    let root = ledger
        .parent()
        .filter(|d| d.file_name().is_some_and(|n| n == "drive"))
        .and_then(Path::parent);
    let (Some(root), Some(tab)) = (root, sid.map(|s| s.trim_start_matches('@'))) else {
        return Vec::new();
    };
    let ends = |t: &str| {
        read(&approvals::ledger_under(root, Some(t))).is_some_and(|body| {
            approvals::holds_rows_of(&body, t) && !approvals::took_up_earlier(&body, t)
        })
    };
    tabs_through_read(root, tab, None, &[], read, &ends)
        .into_iter()
        .skip(1)
        .map(|t| {
            let path = approvals::ledger_under(root, Some(&t));
            (t, path)
        })
        .collect()
}

fn earlier_tabs_path(root: &Path, tab: &str) -> PathBuf {
    let ledger = approvals::ledger_under(root, Some(tab));
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.earlier-tabs"))
}

fn noted_earlier_tabs(root: &Path, tab: &str) -> Vec<String> {
    noted_earlier_tabs_read(root, tab, &|p| read_record_text(p).ok().flatten())
}

fn noted_earlier_tabs_read(
    root: &Path,
    tab: &str,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Vec<String> {
    read(&earlier_tabs_path(root, tab))
        .map(|body| {
            body.lines()
                .filter(|t| super::tap::is_tab_id(t) && *t != tab)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Note that a cold restore reopened tab `tab` from tab `was`.
pub fn note_earlier_tab(opts: &Opts, tab: &str, was: &str) {
    if let Some(root) = opts.aterm_state.as_deref() {
        note_earlier_tab_under(root, tab, was);
    }
}

pub(crate) fn note_earlier_tab_under(root: &Path, tab: &str, was: &str) {
    if !super::tap::is_tab_id(tab) || !super::tap::is_tab_id(was) || was == tab {
        return;
    }
    let Some(body) = earlier_tab_note(tab, was, noted_earlier_tabs(root, tab)) else {
        return;
    };
    let _ = super::atomic_write::write_atomic_ordered(&earlier_tabs_path(root, tab), &body);
}

fn earlier_tab_note(tab: &str, was: &str, mut noted: Vec<String>) -> Option<String> {
    if !super::tap::is_tab_id(tab)
        || !super::tap::is_tab_id(was)
        || was == tab
        || noted.iter().any(|t| t == was)
    {
        return None;
    }
    noted.push(was.to_string());
    let over = noted.len().saturating_sub(EARLIER_TABS);
    noted.drain(..over);
    Some(noted.iter().map(|t| format!("{t}\n")).collect())
}

pub(super) fn earlier_tabs<'a>(
    current: &str,
    tabs: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tabs {
        if !super::tap::is_tab_id(t) || t == current {
            continue;
        }
        out.retain(|o| o != t);
        out.push(t.clone());
    }
    let over = out.len().saturating_sub(EARLIER_TABS);
    out.drain(..over);
    out
}

/// [`tasked`] kept up for one conversation at a time, read on from where the last read ended.
#[derive(Clone, Debug, Default)]
pub struct Tasked {
    session: String,
    path: Option<PathBuf>,
    read_to: u64,
    scan: upgrade::TaskScan,
}

const TASK_READ_BYTES: u64 = 8 << 20;

impl Tasked {
    /// [`tasked`], for `session`.
    pub fn of(&mut self, home: &Path, session: &str, ours: &[approvals::Typed]) -> Option<bool> {
        self.of_in(&home.join(".claude"), session, ours)
    }

    /// [`Tasked::of`] of the conversation the relaunch snapshot `snap` names.
    pub fn of_snapshot(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        let session = snap.session.as_deref()?;
        let records = Records::of_snapshot(user_home, snap);
        self.of_in(records.dir()?, session, ours)
    }

    pub(super) fn of_in(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
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
            self.path = transcript_in(claude_dir, session);
        }
        let Some(path) = self.path.as_ref() else {
            return transcript_exists_in(claude_dir, session)
                .and_then(|any| (!any).then_some(false));
        };
        let (mut f, meta) = aterm_uds::regular_file::open(path).ok()?;
        let len = meta.len();
        if len < self.read_to {
            self.read_to = 0;
            self.scan = upgrade::TaskScan::default();
            self.scan.supervisor_typed(ours);
        }
        f.seek(SeekFrom::Start(self.read_to)).ok()?;
        let rem = len.saturating_sub(self.read_to).min(TASK_READ_BYTES);
        let mut buf = Vec::with_capacity(usize::try_from(rem).unwrap_or(0).saturating_add(1));
        f.take(TASK_READ_BYTES).read_to_end(&mut buf).ok()?;
        let at_end = self.read_to.saturating_add(buf.len() as u64) >= len;
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        buf.truncate(whole);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf).lines() {
            if self.scan.line(line) && self.scan.settled() {
                return Some(true);
            }
        }
        if self.scan.tasked() {
            return Some(true);
        }
        at_end.then_some(false)
    }
}

/// Whether conversation `session`'s task is finished by its own record ([`upgrade::FinishedScan`]).
#[derive(Clone, Debug, Default)]
pub struct Finished {
    session: String,
    path: Option<PathBuf>,
    dir: Option<PathBuf>,
    begun: bool,
    read_to: u64,
    at_end: bool,
    scan: upgrade::FinishedScan,
}

impl Finished {
    /// `Some(finished)` for `session` by the record of the agent the relaunch snapshot `snap` names.
    pub fn of_snapshot(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_snapshot_at(user_home, snap, ours, || live_cwd(snap))
    }

    /// The turn the conversation ends on.
    #[must_use]
    pub fn last_turn_key(&self) -> Option<String> {
        self.at_end.then(|| self.scan.last_turn_key()).flatten()
    }

    /// The turn the conversation ends on.
    #[must_use]
    pub fn last_turn(&self) -> Option<upgrade::LastTurn> {
        self.at_end.then(|| self.scan.last_turn())
    }

    /// The Claude Code directory this memo last read its conversation in.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub(super) fn of_snapshot_at(
        &mut self,
        user_home: &Path,
        snap: &super::relaunch::Snapshot,
        ours: &[approvals::Typed],
        cwd: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<bool> {
        let session = snap.session.as_deref()?;
        let dir = conversation_dir(user_home, snap, session, cwd)?;
        self.of_in_from(&dir, session, ours, FINISHED_ONCE_BYTES)
    }

    #[cfg(all(test, unix))]
    pub(super) fn of_in(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
    ) -> Option<bool> {
        self.of_in_from(claude_dir, session, ours, TAIL_BYTES)
    }

    fn of_in_from(
        &mut self,
        claude_dir: &Path,
        session: &str,
        ours: &[approvals::Typed],
        back: u64,
    ) -> Option<bool> {
        if self.session != session {
            *self = Finished {
                session: session.to_string(),
                ..Finished::default()
            };
        }
        self.scan.supervisor_typed(ours);
        if self.dir.as_deref() != Some(claude_dir)
            || self.path.as_ref().is_none_or(|p| !p.is_file())
        {
            let found = transcript_in(claude_dir, session);
            if found != self.path {
                self.begun = false;
            }
            self.path = found;
            self.dir = Some(claude_dir.to_path_buf());
        }
        self.at_end = false;
        let path = self.path.as_ref()?;
        let (mut f, meta) = aterm_uds::regular_file::open(path).ok()?;
        let len = meta.len();
        if self.begun && len >= self.read_to {
            let at_end = self.fold_on(&mut f, len, false)?;
            self.at_end = at_end;
            return at_end.then(|| self.scan.finished());
        }
        self.begun = true;
        let mut back = back.min(TASK_READ_BYTES - 1);
        loop {
            self.scan = upgrade::FinishedScan::default();
            self.scan.supervisor_typed(ours);
            self.read_to = len.saturating_sub(back).saturating_sub(1);
            let from = self.read_to;
            let mut skip_cut = from != 0;
            loop {
                let before = self.read_to;
                if self.fold_on(&mut f, len, std::mem::take(&mut skip_cut))? {
                    break;
                }
                if self.read_to == before {
                    return None;
                }
            }
            if from == 0 || self.scan.settled() {
                break;
            }
            back = back.saturating_mul(2);
        }
        self.at_end = true;
        Some(self.scan.finished())
    }

    fn fold_on(&mut self, f: &mut std::fs::File, len: u64, skip_cut: bool) -> Option<bool> {
        f.seek(SeekFrom::Start(self.read_to)).ok()?;
        let rem = len.saturating_sub(self.read_to).min(TASK_READ_BYTES);
        let mut buf = Vec::with_capacity(usize::try_from(rem).unwrap_or(0).saturating_add(1));
        Read::take(&mut *f, TASK_READ_BYTES)
            .read_to_end(&mut buf)
            .ok()?;
        let at_end = self.read_to.saturating_add(buf.len() as u64) >= len;
        let cut = if skip_cut {
            buf.iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |i| i + 1)
        } else {
            0
        };
        let whole = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let whole = whole.max(cut);
        self.read_to = self.read_to.saturating_add(whole as u64);
        for line in lossy(buf[cut..whole].to_vec()).lines() {
            self.scan.line(line);
        }
        Some(at_end)
    }
}

fn read_writes_note(opts: &Opts, session: &str) -> Option<String> {
    let dir = check_upgrade_tree(&opts.state, None).ok()??;
    read_record_text(&dir.join(format!("{session}.writes")))
        .ok()
        .flatten()
}

/// Record the directory a moved agent writes its conversation in while it lives.
pub fn note_writes(opts: &Opts, snap: &super::relaunch::Snapshot, dir: &Path) {
    let Some(session) = snap.session.as_deref() else {
        return;
    };
    let filed = Records::of_snapshot(&opts.home, snap);
    if filed.dir().is_none_or(|f| same_dir(f, dir)) {
        return;
    }
    let text = dir.to_string_lossy();
    if read_writes_note(opts, session).is_some_and(|t| t.trim_end() == text) {
        return;
    }
    let Ok(upgrade_dir) = ensure_upgrade_tree(&opts.state, None) else {
        return;
    };
    let _ = super::atomic_write::write_atomic(
        &upgrade_dir.join(format!("{session}.writes")),
        &format!("{text}\n"),
    );
}

pub(super) fn finish_dir(opts: &Opts, records: &Records, session: &str) -> Option<PathBuf> {
    let filed = records.dir().map(Path::to_path_buf);
    let noted = read_writes_note(opts, session)
        .map(|t| PathBuf::from(t.trim_end()))
        .filter(|d| !d.as_os_str().is_empty());
    let newest = |dir: &Path| transcript_in(dir, session).and_then(|p| newest_row_at(&p));
    noted
        .filter(|n| {
            newest(n).is_some_and(|w| filed.as_deref().and_then(newest).is_none_or(|f| w > f))
        })
        .or(filed)
}

pub(super) fn note_moved_writes(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    session: &str,
    cwd: &str,
) {
    let Some(home) = snap
        .home
        .as_deref()
        .filter(|home| Path::new(home).is_relative())
    else {
        return;
    };
    let filed = Records::of_snapshot(&opts.home, snap);
    let Some(filed_dir) = filed.dir().map(Path::to_path_buf) else {
        return;
    };
    let newest = |dir: &Path| transcript_in(dir, session).and_then(|p| newest_row_at(&p));
    let start = Path::new(cwd);
    let mut bases: Vec<PathBuf> = live_cwd(snap).into_iter().collect();
    if start.is_absolute() {
        let inside = repo_root(start).and_then(|root| {
            start
                .strip_prefix(root)
                .ok()
                .map(Path::to_path_buf)
                .filter(|rel| !rel.as_os_str().is_empty())
        });
        for wt in repo_worktrees(start) {
            if let Some(rel) = inside.as_deref() {
                bases.push(wt.join(rel));
            }
            bases.push(wt);
        }
    }
    let noted = read_writes_note(opts, session)
        .map(|t| PathBuf::from(t.trim_end()))
        .filter(|d| !d.as_os_str().is_empty());
    let floor = [Some(filed_dir.clone()), noted]
        .into_iter()
        .flatten()
        .filter_map(|d| newest(&d))
        .max();
    let found = bases
        .into_iter()
        .filter_map(|base| {
            Records::at(&opts.home, HomeVars::config(Some(home)), || Some(base))
                .dir()
                .map(Path::to_path_buf)
        })
        .filter(|dir| !same_dir(dir, &filed_dir))
        .filter_map(|dir| newest(&dir).map(|at| (at, dir)))
        .filter(|(at, _)| floor.is_none_or(|f| *at > f))
        .max_by_key(|(at, _)| *at);
    if let Some((_, dir)) = found {
        note_writes(opts, snap, &dir);
    }
}

fn repo_root(dir: &Path) -> Option<&Path> {
    dir.ancestors()
        .take(REPO_ANCESTORS)
        .find(|d| std::fs::symlink_metadata(d.join(".git")).is_ok())
}

fn repo_worktrees(dir: &Path) -> Vec<PathBuf> {
    let Some(top) = repo_root(dir) else {
        return Vec::new();
    };
    let dot_git = top.join(".git");
    let common = if dot_git.is_dir() {
        dot_git
    } else {
        let Some(gitdir) = super::footer::read_small(&dot_git, 4096).and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("gitdir:"))
                .map(|g| top.join(g.trim()))
        }) else {
            return Vec::new();
        };
        super::footer::read_small(&gitdir.join("commondir"), 4096)
            .map_or_else(|| gitdir.clone(), |c| gitdir.join(c.trim()))
    };
    let mut out: Vec<PathBuf> = common
        .file_name()
        .filter(|n| *n == ".git")
        .and(common.parent())
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    if let Ok(linked) = std::fs::read_dir(common.join("worktrees")) {
        out.extend(
            linked
                .flatten()
                .take(REPO_WORKTREES)
                .filter_map(|e| super::footer::read_small(&e.path().join("gitdir"), 4096))
                .map(|t| PathBuf::from(t.trim()))
                .filter(|p| p.is_absolute())
                .filter_map(|p| p.parent().map(Path::to_path_buf)),
        );
    }
    out.retain(|wt| std::fs::symlink_metadata(wt).is_ok_and(|m| m.is_dir()));
    out.sort();
    out.dedup();
    out
}

const REPO_ANCESTORS: usize = 32;

const REPO_WORKTREES: usize = 64;

fn newest_row_at(path: &Path) -> Option<u64> {
    let (tail, _) = tail_to_end(path, TAIL_BYTES);
    tail.lines().rev().find_map(|line| {
        let v: Value = aterm_json::from_str(line).ok()?;
        super::unix_time::parse_utc(v.get("timestamp")?.as_str()?)
    })
}

pub(super) fn finished_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
    ours: &[approvals::Typed],
) -> Option<bool> {
    WhereWritten::new(opts, records, session, ours).finished()
}

pub(super) struct WhereWritten<'a> {
    opts: &'a Opts,
    records: &'a Records,
    session: &'a str,
    ours: &'a [approvals::Typed],
    scan: std::cell::OnceCell<Option<upgrade::FinishedScan>>,
}

impl<'a> WhereWritten<'a> {
    pub(super) const fn new(
        opts: &'a Opts,
        records: &'a Records,
        session: &'a str,
        ours: &'a [approvals::Typed],
    ) -> Self {
        Self {
            opts,
            records,
            session,
            ours,
            scan: std::cell::OnceCell::new(),
        }
    }

    pub(super) const fn records(&self) -> &'a Records {
        self.records
    }

    pub(super) const fn session(&self) -> &'a str {
        self.session
    }

    pub(super) const fn ours(&self) -> &'a [approvals::Typed] {
        self.ours
    }

    fn scan(&self) -> Option<&upgrade::FinishedScan> {
        self.scan
            .get_or_init(|| read_where_written(self.opts, self.records, self.session, self.ours))
            .as_ref()
    }

    pub(super) fn finished(&self) -> Option<bool> {
        self.scan().map(upgrade::FinishedScan::finished)
    }

    pub(super) fn said_done(&self) -> Option<(bool, bool)> {
        self.scan()
            .map(|scan| (scan.said_done(), scan.said_irreversible()))
    }

    /// Whether the agent's last reply is a clean defense of its work (`Nothing left:`) or says it
    /// could not finish (`Could not finish:`), each the tab's loop's to read.
    pub(super) fn defended(&self) -> Option<bool> {
        self.scan().map(upgrade::FinishedScan::defended)
    }

    /// Whether a prompt no supervisor typed was written at or after unix second `since`: a person
    /// reached the relaunched agent first.
    pub(super) fn asked_since(&self, since: u64) -> Option<bool> {
        self.scan()
            .map(|scan| since != 0 && scan.theirs_at().is_some_and(|at| at >= since))
    }
}

pub(super) fn turn_key_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
) -> Option<upgrade::LastTurn> {
    Some(read_where_written(opts, records, session, &[])?.last_turn())
}

pub(super) fn left_to_person(
    opts: &Opts,
    records: &Records,
    tab: &str,
    session: &str,
    st: &St,
) -> std::io::Result<bool> {
    let lefts = lefts_through(opts, tab, session, st)?;
    // A standing marker is not lifted by a transcript whose current turn cannot be named.
    Ok(!lefts.is_empty()
        && turn_key_where_written(opts, records, session)
            .is_none_or(|now| lefts.iter().any(|k| now.names(k))))
}

fn read_where_written(
    opts: &Opts,
    records: &Records,
    session: &str,
    ours: &[approvals::Typed],
) -> Option<upgrade::FinishedScan> {
    let mut memo = Finished::default();
    memo.of_in_from(
        &finish_dir(opts, records, session)?,
        session,
        ours,
        FINISHED_ONCE_BYTES,
    )?;
    Some(memo.scan)
}

static LOOK_MEMOS: std::sync::Mutex<Vec<Finished>> = std::sync::Mutex::new(Vec::new());

const LOOK_MEMOS_KEPT: usize = 64;

pub(super) const TAIL_BYTES: u64 = 262_144;

pub(super) const FINISHED_ONCE_BYTES: u64 = TASK_READ_BYTES / 2;

#[cfg(all(test, unix))]
pub(super) fn transcript(home: &Path, session: &str) -> Option<PathBuf> {
    transcript_in(&home.join(".claude"), session)
}

pub(super) fn transcript_in(claude_dir: &Path, session: &str) -> Option<PathBuf> {
    let name = format!("{session}.jsonl");
    let mut found = None;
    for e in std::fs::read_dir(claude_dir.join("projects"))
        .ok()?
        .flatten()
    {
        if !crate::harness::entry_is_dir(&e) {
            continue;
        }
        let p = e.path().join(&name);
        if p.is_file() {
            if found.is_some() {
                return None;
            }
            found = Some(p);
        }
    }
    found
}

fn snapshot_tail(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_tail_at(user_home, snap, || live_cwd(snap))
}

pub(super) fn snapshot_tail_at(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<String> {
    let session = snap.session.as_deref()?;
    let path = transcript_in(&conversation_dir(user_home, snap, session, cwd)?, session)?;
    Some(tail_to_end(&path, TAIL_BYTES).0)
}

fn conversation_dir(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    session: &str,
    cwd: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let filed = Records::of_snapshot(user_home, snap)
        .dir()
        .map(Path::to_path_buf);
    let writing = snap
        .home
        .as_deref()
        .filter(|home| Path::new(home).is_relative())
        .and_then(|home| {
            Records::at(user_home, HomeVars::config(Some(home)), cwd)
                .dir()
                .map(Path::to_path_buf)
        })
        .filter(|w| filed.as_deref().is_none_or(|f| !same_dir(w, f)))
        .filter(|w| transcript_in(w, session).is_some());
    writing.or(filed)
}

fn live_cwd(snap: &super::relaunch::Snapshot) -> Option<PathBuf> {
    (kernel_start(snap.pid).as_deref() == Some(snap.start.as_str()))
        .then(|| aterm_sysprobe::process_cwd(snap.pid))
        .flatten()
}

/// Read the standing usage-limit reset timestamp for `snap`'s conversation.
#[must_use]
pub fn named_limit_reset(user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<u64> {
    match snap.run.agent() {
        upgrade::Agent::Agy => agy_limit_reset(user_home, snap),
        upgrade::Agent::Claude | upgrade::Agent::Codex | upgrade::Agent::Muse => {
            upgrade::transcript_limit_reset(&snapshot_tail(user_home, snap)?)
        }
    }
}

fn agy_limit_reset(_user_home: &Path, snap: &super::relaunch::Snapshot) -> Option<u64> {
    let run = snap.agy()?;
    let screen_model = (!snap.tab.is_empty())
        .then(|| {
            let sock = aterm_ctl::resolve_sock_for(Some(&snap.tab)).ok()?;
            let mut c = harness_client(&sock, None).ok()?.reads_only();
            agy::live::wall_screen_model_on(&mut c, &snap.tab).flatten()
        })
        .flatten();
    let argv_model = super::upgrade_agy::argv_model(&snap.argv);
    let flags = super::upgrade_agy::config_flags(&snap.argv);
    let now = crate::supervise::limit::unix_now();
    agy_limit_reset_with(
        argv_model.as_deref(),
        screen_model.as_deref(),
        now,
        |asked| {
            super::agy_quota_store::rotate_worker_at_wall(
                asked,
                &snap.program,
                &run.env,
                &flags,
                now,
            )
        },
    )
}

pub(super) fn agy_limit_reset_with(
    argv_model: Option<&str>,
    screen_model: Option<&str>,
    now: i64,
    mut pick_at_wall: impl FnMut(Option<&str>) -> Option<super::agy_quota_store::LaunchPick>,
) -> Option<u64> {
    let screen_clean = screen_model
        .map(str::trim)
        .filter(|m| !super::agy_quota::family(m).is_empty());
    let mut chosen = pick_at_wall(screen_clean.or(argv_model));
    if screen_clean.is_some()
        && matches!(
            chosen.as_ref().map(|c| &c.pick),
            Some(super::agy_quota::Pick::Unknown { .. })
        )
    {
        chosen = pick_at_wall(argv_model);
    }
    let chosen = chosen?;
    let past_grace = i64::try_from(LIMIT_GRACE_S)
        .unwrap_or(3_600)
        .saturating_add(60);
    let at = match &chosen.pick {
        super::agy_quota::Pick::Wait {
            until: Some(at), ..
        } => Some(*at),
        super::agy_quota::Pick::Rotate { from, .. } => chosen
            .quota
            .group_of(from)
            .and_then(super::agy_quota::Group::wall_refills_at),
        super::agy_quota::Pick::Keep { .. }
            if chosen.named
                || !chosen
                    .quota
                    .groups
                    .iter()
                    .any(super::agy_quota::Group::blocked) =>
        {
            Some(now.saturating_sub(past_grace))
        }
        super::agy_quota::Pick::Keep { .. }
        | super::agy_quota::Pick::Wait { until: None, .. }
        | super::agy_quota::Pick::Unknown { .. } => None,
    }?;
    u64::try_from(at).ok()
}

/// The tail of the transcript Claude Code process `pid` writes now.
#[must_use]
pub fn process_transcript_tail(
    config_dir: Option<&str>,
    home: Option<&str>,
    started_in: Option<&str>,
    pid: u32,
    start: &str,
) -> Option<String> {
    let vars = (config_dir, home, started_in);
    process_transcript_end(vars, pid, start, TAIL_BYTES).map(|(_, tail)| tail)
}

/// [`process_transcript_tail`] with the file it read, and more of its end ([`super::result::TRANSCRIPT_READ_BYTES`]).
#[must_use]
pub fn process_transcript(
    config_dir: Option<&str>,
    home: Option<&str>,
    started_in: Option<&str>,
    pid: u32,
    start: &str,
) -> Option<(PathBuf, String)> {
    let vars = (config_dir, home, started_in);
    process_transcript_end(vars, pid, start, super::result::TRANSCRIPT_READ_BYTES)
}

fn process_transcript_end(
    (config_dir, home, started_in): (Option<&str>, Option<&str>, Option<&str>),
    pid: u32,
    start: &str,
    bytes: u64,
) -> Option<(PathBuf, String)> {
    let cwd = aterm_sysprobe::process_cwd(pid);
    let vars = HomeVars {
        config: config_dir,
        home,
    };
    let user_home = home
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .or_else(atpkg::platform::account_home);
    let user_home = match (config_dir, user_home) {
        (_, Some(user_home)) => user_home,
        (Some(_), None) => PathBuf::new(),
        (None, None) => return None,
    };
    let filed = Records::of_process(&user_home, vars, pid, || cwd.clone(), started_in);
    let writes = Records::at(&user_home, vars, || cwd.clone());
    let (filed, writes) = (filed.dir()?.to_path_buf(), writes.dir()?.to_path_buf());
    let started = super::unix_time::parse_lstart(start)?;
    let entry = super::footer::session_of_pid(&filed, pid, Some(started))?;
    let entry = super::footer::SessionEntry {
        cwd: cwd.unwrap_or(entry.cwd),
        ..entry
    };
    let path = super::footer::sole_transcript_path(&writes, &entry)?;
    let mut f = super::footer::open_regular(&path)?;
    let len = f.metadata().ok()?.len();
    let tail = tail_window(&mut f, len, bytes).0;
    Some((path, tail))
}

/// [`process_transcript_tail`] of the Claude Code `snap` names.
#[must_use]
pub fn snapshot_transcript_tail(snap: &super::relaunch::Snapshot) -> Option<String> {
    snapshot_transcript_end(snap, TAIL_BYTES).map(|(_, tail)| tail)
}

/// [`snapshot_transcript_tail`] with the file it read, and as much of its end as [`process_transcript`] reads.
#[must_use]
pub fn snapshot_transcript(snap: &super::relaunch::Snapshot) -> Option<(PathBuf, String)> {
    snapshot_transcript_end(snap, super::result::TRANSCRIPT_READ_BYTES)
}

fn snapshot_transcript_end(
    snap: &super::relaunch::Snapshot,
    bytes: u64,
) -> Option<(PathBuf, String)> {
    match &snap.run {
        super::relaunch::AgentRun::Claude => {}
        super::relaunch::AgentRun::Codex(_)
        | super::relaunch::AgentRun::Muse(_)
        | super::relaunch::AgentRun::Agy(_) => return None,
    }
    let args = atpkg::caller_shell::process_args(snap.pid);
    let home = aterm_types::dirs::home_dir();
    let (config_dir, home, started_in) = match &args {
        Some(args) => (
            args.env_var(super::accounts::HOME_ENV),
            args.env_var(super::accounts::HOMEDIR_ENV),
            args.env_var("PWD"),
        ),
        None => (None, home.as_deref().and_then(Path::to_str), None),
    };
    process_transcript_end((config_dir, home, started_in), snap.pid, &snap.start, bytes)
}

/// Read the standing quota wall for `snap`'s conversation.
#[must_use]
pub fn named_standing_wall(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
) -> Option<upgrade::StandingWall> {
    upgrade::transcript_standing_wall(&snapshot_tail(user_home, snap)?)
}

/// Reads the environment only while the process identity and captured login agree.
///
/// The identity is sampled on both sides of the read. The snapshot's legacy start
/// has whole-second precision on Unix; this cannot distinguish a prior same-second
/// PID reuse with identical routing, nor an exec preserving routing on a kernel
/// without an exec version. This is a local observation, not a process lease.
#[must_use]
pub fn snapshot_login_env(snap: &super::relaunch::Snapshot) -> Option<Vec<String>> {
    let pid = i32::try_from(snap.pid).ok()?;
    snapshot_login_env_with(
        snap,
        || aterm_uds::process::read_identity(pid),
        || kernel_start(snap.pid),
        || atpkg::caller_shell::process_args(snap.pid),
        || exe_of(snap.pid),
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TranscriptProcess",
        action = "AnotherCase",
        reason = "Select a process identity, routing or unavailable-read fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TranscriptProcess",
        action = "Choose",
        reason = "Finish selecting the sampled process fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "TranscriptProcess",
        action = "Read",
        project = "harness::upgrade_drive::snapshot_env_tests::process_transition"
    )
)]
fn snapshot_login_env_with(
    snap: &super::relaunch::Snapshot,
    mut identity: impl FnMut() -> Option<aterm_uds::process::Identity>,
    start: impl FnOnce() -> Option<String>,
    args: impl FnOnce() -> Option<atpkg::caller_shell::ProcArgs>,
    image: impl FnOnce() -> Option<PathBuf>,
) -> Option<Vec<String>> {
    let before = identity()?;
    if before.pid != i32::try_from(snap.pid).ok()? || start().as_deref() != Some(&snap.start) {
        return None;
    }
    let args = args()?;
    let image = image()?;
    let (program, argv) = launched(&args.argv, Some(&image), Path::new(""), true)?;
    let matches_run = match &snap.run {
        super::relaunch::AgentRun::Claude => {
            let who = super::accounts::ProcessLogin::of_exec_env(&args.env)?;
            args.env_var(super::accounts::LOGIN_ENV) == snap.login.as_deref()
                && args.env_var(super::accounts::HOME_ENV) == snap.home.as_deref()
                && who.route == snap.route
        }
        super::relaunch::AgentRun::Codex(codex) => {
            !args.env.is_empty()
                && args.env_var(super::codex_alab::HOME_ENV) == codex.home_env.as_deref()
                && codex.self_claimed.as_ref().is_none_or(|resume| {
                    args.env_var(super::codex_alab::AUTH_PATH_ENV)
                        == resume.codex_auth_path.as_deref()
                })
        }
        super::relaunch::AgentRun::Muse(muse) => {
            !args.env.is_empty()
                && muse
                    .env
                    .iter()
                    .all(|(k, v)| args.env_var(k) == Some(v.as_str()))
        }
        super::relaunch::AgentRun::Agy(agy) => {
            !args.env.is_empty()
                && agy
                    .env
                    .iter()
                    .all(|(k, v)| args.env_var(k) == Some(v.as_str()))
        }
    };
    if program != snap.program || argv != snap.argv || !matches_run || identity()? != before {
        return None;
    }
    Some(args.env)
}

/// When the newest answer of the conversation `snap` names came.
#[must_use]
pub fn named_last_answer(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    by: u64,
) -> Option<u64> {
    upgrade::transcript_last_answer(&snapshot_tail(user_home, snap)?, by)
}

/// Read the final usage-limit dialog reset line for `snap`'s conversation.
#[must_use]
pub fn named_reset_line(
    user_home: &Path,
    snap: &super::relaunch::Snapshot,
    since: u64,
) -> Option<String> {
    upgrade::transcript_reset_line(&snapshot_tail(user_home, snap)?, since)
}

/// Whether conversation `session` is kept under `home`.
#[must_use]
pub fn conversation_under(home: &Path, session: &str) -> bool {
    upgrade::is_session_id(session) && transcript_exists(home, session) == Some(true)
}

pub(super) fn tail_to_end(path: &Path, bytes: u64) -> (String, u64) {
    let Ok((mut f, meta)) = aterm_uds::regular_file::open(path) else {
        return (String::new(), 0);
    };
    tail_window(&mut f, meta.len(), bytes)
}

fn tail_window(f: &mut (impl Read + Seek), len: u64, bytes: u64) -> (String, u64) {
    let start = len.saturating_sub(bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (String::new(), 0);
    }
    let rem = len.saturating_sub(start).min(bytes);
    let mut buf = Vec::with_capacity(usize::try_from(rem).unwrap_or(0).saturating_add(1));
    let _ = f.take(bytes).read_to_end(&mut buf);
    let end = start.saturating_add(buf.len() as u64);
    (lossy(buf), end)
}

pub(super) fn since(path: &Path, from: u64, cap: u64) -> String {
    let Ok((mut f, meta)) = aterm_uds::regular_file::open(path) else {
        return String::new();
    };
    let len = meta.len();
    if len < from || f.seek(SeekFrom::Start(from)).is_err() {
        return String::new();
    };
    let rem = len.saturating_sub(from).min(cap);
    let mut buf = Vec::with_capacity(usize::try_from(rem).unwrap_or(0).saturating_add(1));
    let _ = f.take(cap).read_to_end(&mut buf);
    lossy(buf)
}

fn lossy(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// One sweep.
#[must_use]
pub fn sweep(opts: &Opts) -> Vec<Report> {
    sweep_with_roster(opts, None)
}

/// The window's step for one tab (`opts.only_sid` on the instance `opts.sock`).
#[must_use]
pub fn step(opts: &Opts) -> Report {
    let tab = opts.only_sid.clone().unwrap_or_else(|| "-".to_string());
    let mut self_claimed = false;
    let tabs = connect(opts, &tab).ok().and_then(|mut c| {
        self_claimed = status_line(&mut c, &tab).is_some_and(|l| self_claimed_by_status(&l));
        host_roster(&mut c)
    });
    let current = || Report {
        rung: None,
        pid: 0,
        tab: tab.clone(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "current".to_string(),
    };
    if self_claimed {
        return Report {
            step: SKIP_SELF_CLAIMED.to_string(),
            ..current()
        };
    }
    let Some(tabs) = tabs.filter(|tabs| tab_is_live(tabs, &tab)) else {
        return Report {
            step: "wait:tab-not-live".to_string(),
            ..current()
        };
    };
    let reports = sweep_with_roster(opts, Some(&tabs));
    pick(&reports, &tab).cloned().unwrap_or_else(current)
}

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

/// The lane of the owner's press of the ⬆️ for tab `opts.only_sid`: one [`step`] of Claude
/// Code's lane alone that acts only on the press ([`upgrade::Press::Acting`]), restarting the
/// agent once [`upgrade::press_gate`] lets it, whatever only defers an automatic move (its
/// Autopilot or its agent switched off, its background work, a person's recent keys); a record
/// with no press standing answers [`PRESS_LAPSED`] and is not stepped.
#[must_use]
pub fn press(opts: &Opts, off: bool) -> Report {
    step(&Opts {
        press: Some(PressOpts { off }),
        human_grace_s: 0,
        agents_off: Vec::new(),
        ..opts.clone()
    })
}

/// Whether tab `opts.only_sid` has an upgrade to take.
#[must_use]
pub fn due(opts: &Opts) -> Due {
    let Some(tab) = opts.only_sid.as_deref() else {
        return Due::No;
    };
    if !in_flight(opts).is_empty() || confirming(opts) {
        return Due::Yes;
    }
    let Ok(mut c) = connect(opts, tab) else {
        return Due::Unread(UNREAD_SOCKET);
    };
    if status_line(&mut c, tab).is_some_and(|l| self_claimed_by_status(&l)) {
        aterm_log::debug!("upgrade due tab={tab} result=No reason=self-claimed");
        return Due::No;
    }
    let Some(tabs) = host_roster(&mut c) else {
        return Due::Unread(UNREAD_SOCKET);
    };
    let due = due_after_lanes(opts, &tabs, &|agent| lane_due(opts, &tabs, agent));
    aterm_log::debug!("upgrade due tab={tab} result={due:?}");
    due
}

fn lane_due(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent) -> Option<Due> {
    match agent {
        upgrade::Agent::Codex => None,
        upgrade::Agent::Claude => None,
        upgrade::Agent::Muse => muse::live::upgrade_due(opts, tabs),
        upgrade::Agent::Agy => agy::live::upgrade_due(opts, tabs),
    }
}

fn due_after_lanes(
    opts: &Opts,
    tabs: &[LiveTab],
    lane_due: &dyn Fn(upgrade::Agent) -> Option<Due>,
) -> Due {
    for &agent in upgrade::Agent::others() {
        if let Some(due) = lane_due(agent) {
            let enabled = lane_on(opts, agent);
            aterm_log::debug!("upgrade due lane={agent:?} enabled={enabled} observed={due:?}");
            return if enabled { due } else { Due::No };
        }
    }
    let Some(tab) = opts.only_sid.as_deref() else {
        aterm_log::debug!("upgrade due result=No reason=no-only-sid");
        return Due::No;
    };
    if !lane_on(opts, upgrade::Agent::Claude) {
        aterm_log::debug!("upgrade due tab={tab} result=No reason=claude-lane-disabled");
        return Due::No;
    }
    let homes = Homes::read(
        opts,
        Some(tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return Due::Unread(UNREAD_FILES);
    };
    aterm_log::debug!(
        "upgrade due tab={tab} records={} tabs={}",
        files.len(),
        tabs.len()
    );
    match due_among(
        opts,
        &files,
        tabs,
        || Targets::read(opts),
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        Due::Unread(UNREAD_RECORD) if homes.unnamed(tab) => Due::Unread(UNREAD_PROCESS),
        Due::Unread(UNREAD_RECORD) => no_record(tabs, tab, started_s, alive, now_s()),
        due => due,
    }
}

fn claude_records(home: &Path) -> Option<Vec<SessionFile>> {
    session_files(home).or_else(|| (!home.join(".claude/sessions").is_dir()).then(Vec::new))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Homes {
    dirs: Vec<Records>,
    unnamed: Vec<String>,
}

impl Homes {
    pub(super) fn read(
        opts: &Opts,
        tabs: Option<&[LiveTab]>,
        mut read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
        mut read_cwd: impl FnMut(u32) -> Option<PathBuf>,
    ) -> Self {
        let mut homes = Homes::default();
        let every;
        let (tabs, learned) = match tabs {
            Some(tabs) => (tabs, false),
            None => {
                every = instance_tabs(opts);
                (every.as_slice(), true)
            }
        };
        let ours = tabs
            .iter()
            .filter(|tab| opts.only_sid.as_deref().is_none_or(|only| only == tab.sid));
        for tab in ours {
            let Some(leader) = tab.fgpgid.and_then(|group| u32::try_from(group).ok()) else {
                continue;
            };
            let unnamed = |homes: &mut Homes| {
                if !learned && alive(leader) {
                    homes.unnamed.push(tab.sid.clone());
                }
            };
            let Some(args) = read_args(leader) else {
                unnamed(&mut homes);
                continue;
            };
            let records = Records::of_process(
                &opts.home,
                HomeVars::of(&args),
                leader,
                || read_cwd(leader),
                args.env_var("PWD"),
            );
            if records.dir().is_some() {
                homes.add(&opts.home, records);
            } else {
                unnamed(&mut homes);
            }
        }
        if learned {
            for records in recorded_homes(opts) {
                homes.add(&opts.home, records);
            }
        }
        homes
    }

    fn add(&mut self, user_home: &Path, records: Records) {
        let Some(dir) = records.dir() else {
            return;
        };
        if !same_dir(dir, &user_home.join(".claude"))
            && !self
                .dirs
                .iter()
                .filter_map(Records::dir)
                .any(|d| same_dir(d, dir))
        {
            self.dirs.push(records);
        }
    }

    pub(super) fn of_records(opts: &Opts, sessions: &std::collections::BTreeSet<String>) -> Self {
        let mut homes = Homes::default();
        for session in sessions {
            if let Ok(Some(st)) = load_record(opts, session)
                && st.home.is_some()
            {
                homes.add(&opts.home, st.agent_records(&opts.home));
            }
        }
        homes
    }

    #[cfg(all(test, unix))]
    pub(super) fn dirs(&self) -> Vec<PathBuf> {
        self.dirs
            .iter()
            .filter_map(Records::dir)
            .map(Path::to_path_buf)
            .collect()
    }

    pub(super) fn unnamed(&self, sid: &str) -> bool {
        self.unnamed.iter().any(|tab| tab == sid)
    }

    pub(super) fn roster(&self, first: Option<Vec<SessionFile>>) -> Option<Vec<SessionFile>> {
        let mut files = first?;
        for records in &self.dirs {
            match records.session_files() {
                Some(more) => files.extend(more),
                None if records.dir().is_some_and(|dir| {
                    matches!(
                        std::fs::symlink_metadata(dir.join("sessions")),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound
                    )
                }) => {}
                None => return None,
            }
        }
        Some(files)
    }
}

fn instance_tabs(opts: &Opts) -> Vec<LiveTab> {
    let roster = |mut c: Client| {
        c.bound_by(CLAIMS_WAIT);
        host_roster(&mut c)
    };
    let one = || {
        connect_reads(opts, opts.only_sid.as_deref().unwrap_or(""))
            .ok()
            .and_then(roster)
            .unwrap_or_default()
    };
    if opts.sock.is_some() {
        return one();
    }
    let socks = match &opts.instances {
        Instances::Machine => match aterm_ctl::local_instances() {
            Ok(instances) if !instances.is_empty() => {
                instances.into_iter().map(|(_, sock)| sock).collect()
            }
            _ => return one(),
        },
        Instances::Listed(socks) => socks.clone(),
    };
    socks
        .iter()
        .filter_map(|sock| roster_connection(sock))
        .filter_map(roster)
        .flatten()
        .collect()
}

fn recorded_homes(opts: &Opts) -> Vec<Records> {
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| St::from_json(&read_record_text(&e.path()).ok()??))
        .filter(|st| driven_here(st.agent) && st.home.is_some())
        .map(|st| st.agent_records(&opts.home))
        .collect()
}

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
    let unanswered = std::cell::Cell::new(false);
    let unread_record = std::cell::Cell::new(false);
    let wants = |sf: &SessionFile| {
        let prior = match load_record(opts, &sf.session_id) {
            Ok(prior) => prior,
            Err(_) => {
                unread_record.set(true);
                return false;
            }
        };
        if prior.as_ref().is_some_and(|st| st.native_retired_unissued) {
            return false;
        }
        let tg = targets.get_or_init(&read_targets);
        let wanted = needs_upgrade_inspection(sf, tg, prior.as_ref());
        unanswered.set(!wanted && tg.unanswered);
        wanted
    };
    let found = tab_agent(tab, files, tabs, wants, read_args, read_group, read_ids);
    if unread_record.get() {
        return Due::Unread(UNREAD_SAVED_RECORD);
    }
    match found {
        TabAgent::Claimed(_) => Due::Yes,
        TabAgent::Nothing if unanswered.get() => Due::Unread(UNREAD_VERSION),
        TabAgent::Nothing => Due::No,
        TabAgent::Unread => Due::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => Due::Unread(UNREAD_RECORD),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TabAgent<'a> {
    Claimed(&'a SessionFile),
    Nothing,
    Unread,
    NoRecord,
}

fn tab_agent<'a>(
    tab: &str,
    files: &'a [SessionFile],
    tabs: &[LiveTab],
    mut wants: impl FnMut(&SessionFile) -> bool,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    mut read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> TabAgent<'a> {
    let front = tabs.iter().find(|t| t.sid == tab).and_then(|t| t.fgpgid);
    let mut leads = false;
    // A supported record of this tab's own foreground that the attribution could not settle: its
    // group unread where its pid leads the tab's foreground group, or that group shared by another
    // tab of the roster. It is undecided, never "no record", which a look past the launch takes
    // as final ([`no_record`]).
    let mut unsettled = false;
    let seen = host_candidates(
        files,
        tabs,
        |sf, t| {
            let ours = t == tab;
            leads |= ours;
            ours && wants(sf)
        },
        read_args,
        |pid| {
            let group = read_group(pid);
            unsettled |= front.is_some_and(|front| match group {
                None => i64::from(pid) == front,
                Some(group) => group == front,
            });
            group
        },
        read_ids,
    );
    if let Some(sf) = seen
        .iter()
        .find_map(|c| c.as_ref().ok().map(|(sf, _, _)| *sf))
    {
        TabAgent::Claimed(sf)
    } else if !seen.is_empty() || (!leads && unsettled) {
        TabAgent::Unread
    } else if leads {
        TabAgent::Nothing
    } else {
        TabAgent::NoRecord
    }
}

/// How long a tab's agent may run with no record of its own before a look takes that as final ([`no_record`]).
pub const RECORD_WINDOW_S: u64 = 30;

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

pub(super) fn started_s(pid: u32) -> Option<u64> {
    super::unix_time::parse_lstart(&kernel_start(pid)?)
}

/// What [`due`] read of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    Yes,
    No,
    Unread(&'static str),
}

/// [`Due::Unread`]'s word for a look the instance's socket did not answer.
pub const UNREAD_SOCKET: &str = "no-socket";
/// [`Due::Unread`]'s word for a look that found no record of the tab's agent among Claude Code's session records while it may still be written.
pub const UNREAD_RECORD: &str = "no-record";

/// An existing record is unreadable and cannot authorize a restart.
pub const UNREAD_SAVED_RECORD: &str = "record-unreadable";
/// [`Due::Unread`]'s word for a look that could not read the tab agent's process whole, for Claude Code and Codex alike.
pub const UNREAD_PROCESS: &str = "no-process";
/// [`Due::Unread`]'s word for a look that could not read Claude Code's session records whole.
pub const UNREAD_FILES: &str = "session-files-unreadable";
/// `Due::Wait` reason when a behind agent has moved away from its recorded home.
pub const HOME_MOVED: &str = "home-moved";
/// [`Due::Unread`]'s word for a look that could not read the build a target runs ([`TargetRead::Unread`]): nothing is decided, and the next look reads it again.
pub const UNREAD_VERSION: &str = "no-version";

/// Mints the session's upgrade state the moment its host sees it behind. Everything is read
/// before the sweep lock is taken, which covers the mint alone ([`behind_after_lanes`]): a socket
/// that answers late, or a target's read, holds no other session's sweep.
pub fn note_behind(opts: &Opts, since: u64) -> Behind {
    let Some(tab) = opts.only_sid.as_deref().filter(|_| !opts.dry_run) else {
        return Behind::Nothing;
    };
    let Some(tabs) = connect(opts, tab)
        .ok()
        .and_then(|mut c| host_roster(&mut c))
    else {
        return Behind::Unread(UNREAD_SOCKET);
    };
    behind_after_lanes(opts, tab, &tabs, since, &|agent| {
        lane_noted(opts, &tabs, agent, since)
    })
}

fn lane_noted(opts: &Opts, tabs: &[LiveTab], agent: upgrade::Agent, _since: u64) -> Option<Behind> {
    match agent {
        upgrade::Agent::Codex => None,
        upgrade::Agent::Claude => None,
        upgrade::Agent::Muse => muse::live::leads(opts, tabs).then_some(Behind::Nothing),
        upgrade::Agent::Agy => agy::live::leads(opts, tabs).then_some(Behind::Nothing),
    }
}

fn behind_after_lanes(
    opts: &Opts,
    tab: &str,
    tabs: &[LiveTab],
    since: u64,
    lane_noted: &dyn Fn(upgrade::Agent) -> Option<Behind>,
) -> Behind {
    for &agent in upgrade::Agent::others() {
        if let Some(noted) = lane_noted(agent) {
            return noted;
        }
    }
    let homes = Homes::read(
        opts,
        Some(tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return Behind::Unread(UNREAD_FILES);
    };
    let sf = match tab_agent(
        tab,
        &files,
        tabs,
        |_| true,
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    ) {
        TabAgent::Claimed(sf) => sf,
        TabAgent::Nothing => return Behind::Nothing,
        TabAgent::Unread => return Behind::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord if homes.unnamed(tab) => return Behind::Unread(UNREAD_PROCESS),
        TabAgent::NoRecord => {
            return match no_record(tabs, tab, started_s, alive, now_s()) {
                Due::Unread(word) => Behind::Unread(word),
                Due::Yes | Due::No => Behind::Nothing,
            };
        }
    };
    let native = exe_of(sf.pid).is_some_and(|exe| exe.starts_with(native_root(&opts.home)));
    let targets = Targets::read(opts);
    let Ok(_held) = sweep_lock(opts) else {
        return Behind::Busy;
    };
    if mint_behind(opts, sf, tab, &targets, native, since) {
        Behind::Noted
    } else {
        Behind::Nothing
    }
}

/// What [`note_behind`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    Noted,
    Nothing,
    Busy,
    Unread(&'static str),
}

/// Whether a newer build `to` is minted over `st`. A look still unclosed is never minted over,
/// held or past its clock: its last word (a line lost, in the ledger and to the host) is said by
/// the step that closes it, and that step's visit mints the newer target ([`due`] stays `Yes`
/// while a look is unclosed).
pub(super) fn behind_anew(st: &St, to: &Version) -> bool {
    matches!(st.phase, Phase::Done | Phase::Failed(_))
        && !st.confirming()
        && Version::parse(&st.to).as_ref() != Some(to)
}

fn mint_behind(
    opts: &Opts,
    sf: &SessionFile,
    tab: &str,
    targets: &Targets,
    native: bool,
    since: u64,
) -> bool {
    let Some(running) = running_version(sf) else {
        return false;
    };
    let Some(target) = upgrade::choose_target(&running, &targets.for_session(native)) else {
        return false;
    };
    let Ok(prior) = load_record(opts, &sf.session_id) else {
        return false;
    };
    if let Some(st) = prior
        .as_ref()
        .filter(|st| !behind_anew(st, &target.version))
    {
        return moved_tabs(st, tab) && retab(opts, sf, tab, prior, &running, &target);
    }
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: super::unix_time::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let mut st = St::for_target(prior, &running, &target, who, now_s());
    st.pending_since = st.pending_since.min(since);
    st.tab = tab.to_string();
    st.keep_home(&Records::of_file(&opts.home, sf), &opts.home);
    save(opts, &sf.session_id, &st);
    true
}

fn moved_tabs(st: &St, tab: &str) -> bool {
    st.tab != tab && matches!(st.phase, Phase::Pending | Phase::Failed(_))
}

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
        started_s: super::unix_time::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let now = now_s();
    let (mut st, how) = St::retarget(prior, running, target, who, now);
    if matches!(st.phase, Phase::Pending | Phase::Failed(_)) {
        tab.clone_into(&mut st.tab);
    }
    st.keep_home(&Records::of_file(&opts.home, sf), &opts.home);
    save(opts, &sf.session_id, &st);
    if how == Retarget::Rested {
        let r = Report {
            rung: None,
            pid: sf.pid,
            tab: tab.to_string(),
            session: sf.session_id.clone(),
            from: running.to_string(),
            to: format!("{}({})", target.version, target.source.as_str()),
            step: String::new(),
        };
        rested_said(opts, &r, &st, now);
    }
    if Version::parse(&old).as_ref() != Some(&target.version) {
        let r = Report {
            rung: None,
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

fn rested_said(opts: &Opts, r: &Report, st: &St, now: u64) {
    if opts.dry_run {
        return;
    }
    let Phase::Failed(why) = &st.phase else {
        return;
    };
    save(opts, &r.session, st);
    let rests = st
        .failed_at
        .saturating_add(upgrade::RETRY_S)
        .saturating_sub(now);
    ledger(
        opts,
        &said(r.clone(), format!("kept-stopped:{why}")),
        &format!(
            "a new process holds the conversation (resumed by hand after the stop), running {}; \
             {} failed to start for this conversation, so it is not tried again for it until the \
             stop's rest ends in {} (a newer build is tried at once)",
            st.from,
            st.to,
            upgrade::span(rests)
        ),
    );
}

/// What the window's host does after one [`step`] on a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    NextIdle,
    Later(std::time::Duration),
    Finished,
    Broken,
}

/// The pauses before consecutive looks at a session whose step waited.
pub const LATER: [std::time::Duration; 4] = [
    std::time::Duration::from_secs(upgrade::QUIET_S),
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(300),
    std::time::Duration::from_secs(600),
];

/// [`LATER`]'s first rung as this process runs it.
#[must_use]
pub fn first_look() -> std::time::Duration {
    std::time::Duration::from_secs(upgrade::quiet_s())
}

fn later_rungs() -> [std::time::Duration; 4] {
    let mut rungs = LATER;
    rungs[0] = first_look();
    rungs
}

/// The longest gap between two looks reading the same process's screen idle for them to count in a row ([`St::idle_looks`]).
pub const IDLE_RUN_GAP_S: u64 = LATER[LATER.len() - 1].as_secs() + upgrade::QUIET_S;

/// How soon a session whose step waited on a person's hold ([`upgrade::person_hold`]) is looked at again, however long it has waited.
pub const HOLD_LOOK: std::time::Duration = std::time::Duration::from_secs(60);

/// How soon a session whose step found the upgrade's own state unusable ([`After::Broken`]) is
/// looked at again: the ladder's slowest rung, so a folder unwritable for a while holds an idle
/// session's move a rung at a time, never until its next activation notice.
pub const BROKEN_LOOK: std::time::Duration = LATER[LATER.len() - 1];

const _: () = assert!(upgrade::HOLD_GAP_S >= 2 * HOLD_LOOK.as_secs());

/// [`After`] from one [`step`]'s word, `waits` the session's consecutive waits before it.
#[must_use]
pub fn after(step: &str, waits: u32) -> After {
    known_after(step, waits).unwrap_or(After::Later(LATER[LATER.len() - 1]))
}

/// Whether [`after`] was never taught `step`.
#[must_use]
pub fn unknown_step(step: &str) -> bool {
    known_after(step, 0).is_none()
}

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
    let unbegun = match &st.phase {
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_) => true,
        Phase::Exiting { .. } | Phase::Relaunched { .. } | Phase::Done => false,
    };
    match st.request_for(&st.tab) {
        Request::None | Request::Now => step,
        Request::Skip(v) => {
            let this_target = upgrade::same_build(st.agent, &v, &st.to);
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

pub(super) fn watch_at(st: &St) -> Option<u64> {
    let step = due_by(st)?;
    let moved = st.behind_since().saturating_add(MOVE_BUDGET_S);
    Some(step.min(moved).max(st.looked_at.saturating_add(WATCH_GAP)))
}

fn is_last_word(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "current" | "done" | "ended" | "skip" | "terminated" | "relaunched" | "exited"
    ) || step.starts_with("would-")
        || step == "wait:skipped"
}

fn known_after(step: &str, waits: u32) -> Option<After> {
    let later = || ladder(waits);
    Some(match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" | "continued" | "adopted" => After::NextIdle,
        "told" => later(),
        "gave-up" | "drain-expired" => After::NextIdle,
        "released" | "rearmed" => later(),
        _ if is_last_word(step) => After::Finished,
        _ if step == "wait:failed" => later(),
        "failed" | "refused" => later(),
        "busy" if matches!(step, "busy:state-unwritable" | "busy:lock-unopenable") => After::Broken,
        _ if matches!(step, "wait:box" | "wait:draft") => After::Later(HOLD_LOOK),
        _ if attended(step) => After::Later(first_look()),
        "wait" | "busy" | "held-back" | "left-typed" => later(),
        "reopened" => later(),
        _ => return None,
    })
}

fn ladder(waits: u32) -> After {
    After::Later(
        crate::supervise::ladder::Ladder(&later_rungs())
            .step(usize::try_from(waits).unwrap_or(usize::MAX)),
    )
}

/// Whether one [`step`]'s word is a person at the tab holding the act due at this point.
#[must_use]
pub fn attended(step: &str) -> bool {
    matches!(step, "held-back:attended" | "wait:attended")
}

/// [`After`] an attended wait ([`attended`]).
#[must_use]
pub fn after_attended(looks: u32, same: u32, grace_s: u32) -> After {
    if attended_owned(looks, grace_s) {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Whether one [`step`]'s word says it typed a turn into the agent ([`crate::supervise::HostStep::typed`]).
#[must_use]
pub fn typed(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "announced" | "continued" | "released" | "told"
    )
}

/// Which turn one [`step`]'s word typed ([`typed`]), for the turn-end policy ([`crate::supervise::HostStep::typed`]).
#[must_use]
pub fn host_turn(step: &str) -> Option<crate::supervise::policy::turn_end::HostTurn> {
    use crate::supervise::policy::turn_end::HostTurn;
    typed(step).then(|| {
        if step == "continued:restore" {
            HostTurn::Restore
        } else if step.split_once(':').map_or(step, |(head, _)| head) == "announced" {
            HostTurn::Notice
        } else if step == "continued:check" {
            HostTurn::Check
        } else if step.split_once(':').map_or(step, |(head, _)| head) == "told" {
            HostTurn::Fact
        } else {
            HostTurn::CarryOn
        }
    })
}

/// Whether one [`step`]'s word says it moved the session ([`crate::supervise::HostStep::moved`]).
#[must_use]
pub fn moved(step: &str) -> bool {
    typed(step)
        || matches!(
            step.split_once(':').map_or(step, |(head, _)| head),
            "adopted" | "relaunched" | "terminated" | "exited"
        )
        || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Whether one [`step`]'s word says a NEW AGENT PROCESS came up where the harness ended the old one.
#[must_use]
pub fn started(step: &str) -> bool {
    matches!(
        step.split_once(':').map_or(step, |(head, _)| head),
        "adopted" | "relaunched"
    ) || matches!(step, "done:fresh" | "done:unconfirmed")
}

/// Holds the upgrade's clocks through a limit episode the window's loop saw.
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
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
        return true;
    };
    let mut complete = true;
    for e in dir.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Some(session) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let Ok(record) = load_record(opts, &session) else {
            complete = false;
            continue;
        };
        if let Some(mut st) = record.filter(|st| st.tab == tab)
            && st.hold_clock(until)
        {
            save(opts, &session, &st);
        }
    }
    complete
}

/// Maximum consecutive looks the upgrade waits for background work under its own gate.
pub const OWNED_BACKGROUND_LOOKS: u32 = 3;

/// How many quick looks in a row the upgrade may own the session's turn ends for.
pub const OWNED_SETTLE_LOOKS: u32 = 2;

/// How many turn ends a quiet round's settle may hold at each higher urgency rung
/// ([`quiet_settle_owned`]).
pub const QUIET_SETTLE_POINTS: u32 = 3;

/// Whether a quiet round's settle (`wait:settling` under the quiet flow) owns the session's turn
/// end after a look. `looks` counts the bounded waits at this point before it (since the last
/// turn, any word's), `held` the turn ends held since this round's last urgency rise, and `this_held` says
/// whether this point is one of them. Each turn end is held for at most [`OWNED_SETTLE_LOOKS`]
/// looks, each [`first_look`] apart ([`after_quiet_settle`]), and each higher rung grants at most
/// [`QUIET_SETTLE_POINTS`] turn ends, including this point if already held. A rise renews no
/// point's looks; a turn starts the looks over without renewing the turn-end budget. The round's
/// next act, word or nothing due starts both counts over. Refines `UpgradeQuietSettleTurnEnd`.
#[must_use]
pub const fn quiet_settle_owned(looks: u32, held: u32, this_held: bool) -> bool {
    looks < OWNED_SETTLE_LOOKS && (this_held || held < QUIET_SETTLE_POINTS)
}

/// [`After`] a quiet round's settle: at the first rung while the look owned the turn end, so the
/// Autopilot's continuation is held for at most [`OWNED_SETTLE_LOOKS`] times [`first_look`] at a
/// point, and up the ladder once it let go.
#[must_use]
pub fn after_quiet_settle(owned: bool, same: u32) -> After {
    if owned {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Return whether the upgrade owns the session's turn ends after `step`.
#[must_use]
pub fn owns_turn_ends(step: &str, looks: u32, grace_s: u32) -> bool {
    if attended(step) {
        return attended_owned(looks, grace_s);
    }
    match step.split_once(':').map_or(step, |(head, _)| head) {
        "announced" => true,
        "wait" => match step {
            "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded" => looks < OWNED_SETTLE_LOOKS,
            _ if log_settles(step) => looks < OWNED_SETTLE_LOOKS,
            _ if release_settles(step) => looks < OWNED_SETTLE_LOOKS,
            "wait:background" => looks < OWNED_BACKGROUND_LOOKS,
            _ if refused(step) => looks < OWNED_REFUSED_LOOKS,
            _ => false,
        },
        _ => false,
    }
}

/// [`owns_turn_ends`] under the flow the step was taken in.
#[must_use]
pub fn owns_turn_ends_as(step: &str, looks: u32, grace_s: u32, quiet: bool) -> bool {
    !(quiet && drains(step)) && owns_turn_ends(step, looks, grace_s)
}

/// [`bounded_wait`] under the flow the step was taken in.
#[must_use]
pub fn bounded_wait_as(step: &str, quiet: bool) -> bool {
    !(quiet && drains(step)) && bounded_wait(step)
}

fn drains(step: &str) -> bool {
    step == "wait:background"
}

/// Whether a wait counts toward the bound its word is owned for ([`owns_turn_ends`]).
#[must_use]
pub fn bounded_wait(step: &str) -> bool {
    matches!(
        step,
        "wait:pressed"
            | "wait:settling"
            | "wait:not-idle"
            | "wait:busy"
            | "wait:status-stale"
            | "wait:ready-unrecorded"
            | "wait:superseded"
            | "wait:background"
    ) || release_settles(step)
        || log_settles(step)
}

/// Return whether `step` is a quiet Muse Code or agy restart waiting on its record to settle.
#[must_use]
pub fn log_settles(step: &str) -> bool {
    matches!(step, "wait:turn-open" | "wait:queued-work")
}

/// Whether `step` is an owed release waiting out the settle at its point (`wait:release:settling`, `…:not-idle`, `…:busy`, `…:status-stale`).
#[must_use]
pub fn release_settles(step: &str) -> bool {
    step.strip_prefix("wait:release:")
        .is_some_and(|why| matches!(why, "settling" | "not-idle" | "busy" | "status-stale"))
}

/// Whether one [`step`]'s word is a notice its own fence refused (`wait:announce-refused:<why>`).
#[must_use]
pub fn refused(step: &str) -> bool {
    step.starts_with("wait:announce-refused:")
}

/// Maximum refused looks before an upgrade-owned session stands down.
pub const OWNED_REFUSED_LOOKS: u32 = 3;

/// [`After`] a refused notice ([`refused`]).
#[must_use]
pub fn after_refused(looks: u32, same: u32) -> After {
    if looks < OWNED_REFUSED_LOOKS {
        After::Later(first_look())
    } else {
        ladder(same)
    }
}

/// Maximum attempts to type a notice when the input fence reports `changed`.
pub const NOTICE_FENCE_TRIES: usize = 3;

/// Whether an attended wait ([`owns_turn_ends`]) still owns the session's turn ends after `looks` attended looks in a row at this point.
#[must_use]
pub fn attended_owned(looks: u32, grace_s: u32) -> bool {
    u64::from(looks).saturating_mul(first_look().as_secs())
        <= u64::from(grace_s).saturating_add(ATTENDED_OWNED_S)
}

/// How long past the person's grace an attended wait may keep owning the session's turn ends ([`attended_owned`]).
pub const ATTENDED_OWNED_S: u64 = 30 * 60;

/// Whether a step taken at a break of the agent's background work (`IdleHost::at_background`) gives back the turn ends a notice claimed there.
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

fn sweep_with_roster(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    sweep_with_lanes(opts, live_tabs, &|agent| match agent {
        upgrade::Agent::Codex => codex_alab_drive::sweep(opts, live_tabs),
        upgrade::Agent::Claude => claude_pass(opts, live_tabs),
        upgrade::Agent::Muse => muse::live::upgrade_pass(opts, live_tabs),
        upgrade::Agent::Agy => agy::live::upgrade_pass(opts, live_tabs),
    })
}

/// [`sweep_with_roster`] over each lane's pass `pass`, under the sweep lock: the confirmations
/// are closed first and reported after the lanes' reports, so [`pick`] names a tab's live
/// conversation's act before the closed look of a conversation whose agent ended. A tab's own
/// step ends on the last word of its live agent's look that its host must hear
/// ([`look_ends_step`]): the lanes go at the tab's next look, so no act of theirs is said in that
/// word's place. Any other close of a look (`done:resumed`, a model confirmed) yields in a tab's
/// own step to what the lanes say of that tab, an act or a wait: the close is in the record and
/// the ledger already, and a wait it stood before would lose its host the wait's next look.
fn sweep_with_lanes(
    opts: &Opts,
    live_tabs: Option<&[LiveTab]>,
    pass: &dyn Fn(upgrade::Agent) -> Vec<Report>,
) -> Vec<Report> {
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => {
            return vec![Report {
                rung: None,
                pid: 0,
                tab: "-".to_string(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("busy:{why}"),
            }];
        }
    };
    // A break's step closes a look too: closing types nothing, and the line a look owes stays
    // an idle point's (`relaunch::look_types`).
    let confirmed = if opts.press.is_some() {
        Vec::new()
    } else {
        confirmations(opts)
    };
    if opts.only_sid.is_some()
        && confirmed
            .iter()
            .any(|r| look_ends_step(&r.step) && alive(r.pid))
    {
        return confirmed;
    }
    muse::sweep_prune(opts, live_tabs);
    agy::sweep_prune(opts, live_tabs);
    let mut reports = lanes(opts, live_tabs, pass);
    let mut confirmed = confirmed;
    if opts.only_sid.is_some() {
        confirmed.retain(|c| {
            look_ends_step(&c.step)
                || !reports
                    .iter()
                    .any(|r| r.tab == c.tab && r.step != "current")
        });
    }
    reports.extend(confirmed);
    reports
}

/// Whether `step` is a look's last word that ends its tab's step: its restore line typed into
/// the agent, which starts a turn, or the line recorded lost, which the host records where a
/// person reads it.
fn look_ends_step(step: &str) -> bool {
    matches!(step, "continued:restore" | "done:lost")
}

fn lanes(
    opts: &Opts,
    live_tabs: Option<&[LiveTab]>,
    pass: &dyn Fn(upgrade::Agent) -> Vec<Report>,
) -> Vec<Report> {
    let swept = |agent: upgrade::Agent| {
        if lane_on(opts, agent) {
            pass(agent)
        } else {
            Vec::new()
        }
    };
    let others: Vec<Report> = upgrade::Agent::others()
        .iter()
        .filter(|_| opts.press.is_none())
        .flat_map(|&agent| swept(agent))
        .collect();
    let others_tab = opts
        .only_sid
        .as_ref()
        .is_some_and(|sid| live_tabs.is_some() && others.iter().any(|r| r.tab == *sid));
    let mut reports = Vec::new();
    if !others_tab {
        reports.extend(swept(upgrade::Agent::Claude));
    }
    reports.extend(others);
    reports
}

fn lane_on(opts: &Opts, agent: upgrade::Agent) -> bool {
    let written = super::lanes::lanes(agent.program());
    (written.upgrade || (written.relaunch && super::lanes::sweep_carries_relaunch(agent.program())))
        && !opts.agents_off.contains(&agent.program())
}

pub(super) fn codex_carry_in_flight(opts: &Opts, r: Report, st: St, session: &str) -> Report {
    codex_alab_drive::carry_in_flight(opts, r, st, session)
}

pub(super) fn codex_alab_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    codex_alab_drive::snapshot(opts, tab, pid, start, shell, args, &Live)
}

pub(super) fn codex_alab_snapshot_with_claim(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
    claim: Option<&super::self_claimed::Copy>,
) -> Result<super::relaunch::Snapshot, &'static str> {
    match claim {
        Some(claim) => codex_alab_drive::snapshot_with_claim(
            opts,
            tab,
            pid,
            start,
            shell,
            args,
            Some(claim),
            &Live,
        ),
        None => codex_alab_snapshot(opts, tab, pid, start, shell, args),
    }
}

/// Relaunch a proven self-claimed codex-alab after exit.
#[must_use]
pub fn self_claimed_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    resume: &super::self_claimed::Resume,
    asked: Option<super::restart_stamp::Cause>,
) -> Report {
    let cause = match asked {
        Some(super::restart_stamp::Cause::Stall) => super::relaunch::ExitCause::Stall,
        Some(super::restart_stamp::Cause::Asked) => super::relaunch::ExitCause::Asked,
        None => super::relaunch::ExitCause::Exit,
    };
    self_claimed_relaunch(opts, snap, resume, cause)
}

pub(super) fn self_claimed_relaunch(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    resume: &super::self_claimed::Resume,
    cause: super::relaunch::ExitCause,
) -> Report {
    codex_alab_drive::self_claimed_after_exit(opts, snap, resume, cause, &Live)
}

pub(super) fn muse_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    muse::snapshot(opts, tab, pid, start, shell, args, &Live)
}

pub(super) fn muse_follow(snap: &mut super::relaunch::Snapshot) -> bool {
    muse::follow(snap)
}

pub(super) fn muse_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    muse::look_at_exit(opts, snap, &Live)
}

pub(super) fn muse_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    muse::after_exit(opts, snap, left, upgrade, cause, &Live)
}

pub(super) fn agy_snapshot(
    opts: &Opts,
    tab: &str,
    pid: u32,
    start: String,
    shell: u32,
    args: &atpkg::caller_shell::ProcArgs,
) -> Result<super::relaunch::Snapshot, &'static str> {
    agy::snapshot(opts, tab, pid, start, shell, args, &Live)
}

pub(super) fn agy_conversation_held(config: &Path, pid: u32) -> Option<Option<String>> {
    agy::conversation_held(config, pid, &Live)
}

pub(super) fn agy_look_at_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
) -> super::relaunch::ExitLook {
    agy::look_at_exit(opts, snap, &Live)
}

pub(super) fn agy_after_exit(
    opts: &Opts,
    snap: &super::relaunch::Snapshot,
    left: &super::relaunch::ExitRecord,
    upgrade: bool,
    cause: super::relaunch::ExitCause,
) -> Report {
    agy::after_exit(opts, snap, left, upgrade, cause, &Live)
}

pub(super) fn agy_carries_restart(user_home: &Path, snap: &super::relaunch::Snapshot) -> bool {
    let twin = super::upgrade_agy::store_build(&snap.program)
        .then(|| managed_agent_twin(super::upgrade_agy::PROGRAM))
        .flatten();
    agy::carries_restart_onto(snap, user_home, twin)
}

fn claude_pass(opts: &Opts, live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    if live_tabs.is_some_and(<[LiveTab]>::is_empty) {
        return Vec::new();
    }
    let homes = Homes::read(
        opts,
        live_tabs,
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return vec![Report {
            rung: None,
            pid: 0,
            tab: "-".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: format!("wait:{UNREAD_FILES}"),
        }];
    };
    if !opts.background {
        prune_states(opts, &files, now_s());
    }
    let mut reports = Vec::new();
    reports.extend(if let Some(tabs) = live_tabs {
        roster_reports(
            opts,
            &files,
            tabs,
            || Targets::read(opts),
            atpkg::caller_shell::process_args,
            process_group,
            ids,
        )
    } else if files.is_empty() {
        Vec::new()
    } else {
        let t = table();
        let targets = Targets::read(opts);
        targets.cache_catalogs(opts);
        files
            .iter()
            .map(|sf| visit_build(opts, sf, Some(&files), &t, &targets, &Live, None, None))
            .collect()
    });
    for sid in &homes.unnamed {
        if !reports.iter().any(|r| r.tab == *sid) {
            reports.push(Report {
                rung: None,
                pid: 0,
                tab: sid.clone(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("wait:{UNREAD_PROCESS}"),
            });
        }
    }
    if !opts.background {
        reports.extend(orphans(opts, &files, live_tabs));
    }
    reports
}

/// Claude Code's reports over the live roster `tabs`: each session its tab claims that
/// [`step_visits`] lets through is visited ([`visit_build`]) against the targets `read_targets`
/// reads, at most once and only where a session is asked of; a claim whose process cannot be
/// read whole says its wait.
fn roster_reports(
    opts: &Opts,
    files: &[SessionFile],
    tabs: &[LiveTab],
    read_targets: impl Fn() -> Targets,
    read_args: impl FnMut(u32) -> Option<atpkg::caller_shell::ProcArgs>,
    read_group: impl FnMut(u32) -> Option<i64>,
    read_ids: impl FnMut(u32) -> Option<(u32, i64, i64)>,
) -> Vec<Report> {
    let targets = std::cell::OnceCell::new();
    let candidates = host_candidates(
        files,
        tabs,
        |sf, tab| {
            let tg = targets.get_or_init(&read_targets);
            if opts.only_sid.as_deref().is_some_and(|only| only != tab) {
                return false;
            }
            match load_record(opts, &sf.session_id) {
                Err(_) => true,
                Ok(prior) => step_visits(sf, tg, prior.as_ref()),
            }
        },
        read_args,
        read_group,
        read_ids,
    );
    let (mut claimed, mut waits) = (Vec::new(), Vec::new());
    for candidate in candidates {
        match candidate {
            Ok(c) => claimed.push(c),
            Err(wait) => waits.push(wait),
        }
    }
    if !claimed.is_empty() {
        let t = table();
        let targets = targets.get_or_init(&read_targets);
        targets.cache_catalogs(opts);
        waits.extend(claimed.into_iter().map(|(sf, args, claim)| {
            visit_build(
                opts,
                sf,
                Some(files),
                &t,
                targets,
                &Live,
                Some(&args),
                Some(&claim),
            )
        }));
    }
    waits
}

pub(super) fn sweep_lock(opts: &Opts) -> Result<Option<SweepLock>, &'static str> {
    if opts.dry_run {
        return Ok(None);
    }
    let dir = ensure_upgrade_tree(&opts.state, None).map_err(|_| "state-unwritable")?;
    let mut open = std::fs::OpenOptions::new();
    open.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        open.custom_flags(0x0020_0000);
    }
    let lock: std::fs::File =
        aterm_uds::kept_out(|| open.open(dir.join("sweep.lock"))).map_err(|_| "lock-unopenable")?;
    let metadata = lock.metadata().map_err(|_| "lock-unopenable")?;
    if !metadata.is_file() {
        return Err("lock-unopenable");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x0000_0400 != 0 {
            return Err("lock-unopenable");
        }
    }
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

/// Retarget a pending managed record through the sweep's real transition and save path.
///
/// # Errors
/// Returns an error if its lock, record, pending managed move, or version is unavailable.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn retarget_pending_for_test(
    opts: &Opts,
    session: &str,
    running: &str,
    proc_start: &str,
) -> Result<(), &'static str> {
    let _held = sweep_lock(opts)?;
    let prior = load_record(opts, session)
        .map_err(|_| UNREAD_SAVED_RECORD)?
        .ok_or("no-record")?;
    if prior.phase != Phase::Pending || prior.source != "managed" {
        return Err("not-pending-managed");
    }
    let running = Version::parse(running).ok_or("invalid-running-build")?;
    let target = Candidate {
        exe: PathBuf::new(),
        version: Version::parse(&prior.to).ok_or("invalid-target-build")?,
        source: Source::Managed,
    };
    let who = Proc {
        start: proc_start,
        started_s: 0,
    };
    let (st, _) = St::retarget(Some(prior), &running, &target, who, now_s());
    save(opts, session, &st);
    Ok(())
}

pub(super) struct SweepLock(std::fs::File);

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn confirming(opts: &Opts) -> bool {
    read_upgrade_dir(&opts.state).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && read_record_text(&path)
                    .ok()
                    .flatten()
                    .and_then(|text| St::from_json(&text))
                    .is_some_and(|st| {
                        st.confirming() && opts.only_sid.as_ref().is_none_or(|s| *s == st.tab)
                    })
        })
    })
}

/// List conversations left mid-restart by an earlier upgrade sweep.
#[must_use]
pub fn in_flight(opts: &Opts) -> Vec<String> {
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
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
            let st = load_record(opts, &session)
                .ok()
                .flatten()
                .filter(|st| st.in_flight() && !st.native_retired_unissued)?;
            let ours =
                opts.only_sid.as_ref().is_none_or(|s| *s == st.tab) && lane_on(opts, st.agent);
            ours.then_some(session)
        })
        .collect();
    out.sort();
    out
}

/// The agent the first conversation [`in_flight`] names ran (its record's agent).
#[must_use]
pub fn in_flight_agent(opts: &Opts) -> Option<upgrade::Agent> {
    in_flight(opts)
        .first()
        .and_then(|session| load_record(opts, session).ok().flatten())
        .map(|st| st.agent)
}

fn blank(sf: &SessionFile) -> Report {
    Report {
        rung: None,
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
    t: &[ProcRow],
    targets: &Targets,
    k: &dyn Kernel,
) -> Report {
    let files = Records::of_file(&opts.home, sf).session_files();
    visit_build(opts, sf, files.as_deref(), t, targets, k, None, None)
}

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

#[allow(clippy::too_many_arguments)]
fn visit_build(
    opts: &Opts,
    sf: &SessionFile,
    initial_files: Option<&[SessionFile]>,
    t: &[ProcRow],
    targets: &Targets,
    k: &dyn Kernel,
    prefetched_args: Option<&atpkg::caller_shell::ProcArgs>,
    host_claim: Option<&HostClaim>,
) -> Report {
    let mut r = blank(sf);
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
    let mut prior = match load_record(opts, &sf.session_id) {
        Ok(prior) => prior,
        Err(_) => return said(r, format!("wait:{UNREAD_SAVED_RECORD}")),
    };
    if prior.as_ref().is_some_and(|st| st.native_retired_unissued) {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    if opts.press.is_some()
        && !prior
            .as_ref()
            .is_some_and(|st| st.in_flight() || st.press_stands(&tab, now_s()))
    {
        return said(r, PRESS_LAPSED);
    }
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
    if let Some(st) = prior
        .as_mut()
        .filter(|st| st.tab == tab && st.landed_after_stop(sf, k))
    {
        let reopen = Report {
            to: format!("{}({})", st.to, st.source),
            ..r.clone()
        };
        if opts.dry_run {
            return said(reopen, "would-reopen:relaunch-landed");
        }
        st.reopen_landed(now_s());
        save(opts, &sf.session_id, st);
        ledger(
            opts,
            &said(reopen, "reopened:relaunch-landed"),
            "the relaunch stopped as never picked up came up after all: the process its \
             shell started holds the conversation, so it is carried on as a relaunch that \
             landed",
        );
    }
    if let Some(mut st) = prior.clone().filter(St::in_flight) {
        r.to = format!("{}({})", st.to, st.source);
        if st.tab != tab {
            return said(r, "wait:conversation-in-other-tab");
        }
        // The live process holding the conversation is the agent the restart waits on only by
        // its pid and kernel start: one born at another time at that pid is another process
        // (a reboot reuses pids), never the exit to wait for.
        if st.names(sf.pid, &sf.proc_start) {
            // A record an earlier build wrote names its agent by pid alone: the live process at
            // that pid may be it or one born since, so it is neither waited on as the agent nor
            // taken for another's resume of the conversation.
            let r = said(
                r,
                if st.pid_start.is_empty() {
                    super::relaunch::WAIT_EXIT_UNPROVEN
                } else {
                    "wait:exiting"
                },
            );
            note_stuck(opts, &r, &mut st, &sf.session_id, now_s());
            return r;
        }
        if matches!(st.phase, Phase::Exiting { .. }) {
            return resumed_by_another(opts, r, &mut st, sf);
        }
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
    let Some(running) = running_version(sf) else {
        return said(r, "wait:version-unreadable");
    };
    let exe = exe_of(sf.pid).unwrap_or_default();
    let running_native = exe.starts_with(native_root(&opts.home));
    let target = upgrade::choose_target(&running, &targets.for_session(running_native));
    let Some(target) = target else {
        if let Some(st) = prior.as_mut()
            && st.release_untargeted(&running, !targets.unanswered, now_s())
            && !opts.dry_run
        {
            save(opts, &sf.session_id, st);
        }
        if let Some(st) = prior.clone().filter(|st| !st.release.is_empty()) {
            return release_visit(opts, r, sf, st, &tab, t, k, host_claim);
        }
        if let Some(gone) = forget_settled_pending(opts, &sf.session_id, prior.as_ref(), &running) {
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
        return said(r, targets.untargeted_word());
    };
    r.to = format!("{}({})", target.version, target.source.as_str());
    if Records::of_file(&opts.home, sf).left_by(
        &opts.home,
        args.env_var(super::accounts::HOME_ENV),
        || aterm_sysprobe::process_cwd(sf.pid),
    ) {
        return said(r, format!("wait:{HOME_MOVED}"));
    }
    let now = now_s();
    if prior
        .as_ref()
        .is_some_and(|st| super::relaunch::look_holds(opts, &sf.session_id, st, now))
    {
        return said(r, "wait:confirming");
    }
    // A look past its own clock hides no newer build. A step's confirmations closed it before
    // this visit; a press's lane, whose sweep runs none, closes it here, its last word in the
    // ledger; a read that writes nothing (the watch's) treats it as closed.
    if let Some(st) = prior.as_mut().filter(|st| st.confirming()) {
        if opts.dry_run {
            st.close_look();
        } else {
            let look = look_report(st, &sf.session_id);
            let closed = confirm(opts, &look, st, &sf.session_id, now);
            if st.confirming() {
                return said(r, "wait:confirming");
            }
            if opts.press.is_none()
                && let Some(last) = closed.filter(|c| look_ends_step(&c.step))
            {
                return last;
            }
        }
    }
    let who = squash(&sf.proc_start);
    let who = Proc {
        start: &who,
        started_s: super::unix_time::parse_lstart(&sf.proc_start).unwrap_or(0),
    };
    let retargeted_from = prior
        .as_ref()
        .filter(|p| {
            matches!(
                p.phase,
                Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
            ) && Version::parse(&p.to).as_ref() != Some(&target.version)
        })
        .map(|p| p.to.clone());
    let (mut st, how) = St::retarget(prior, &running, &target, who, now);
    if st.keep_home(&Records::of_file(&opts.home, sf), &opts.home) {
        save(opts, &sf.session_id, &st);
    }
    if how == Retarget::Rested {
        rested_said(opts, &r, &st, now);
    }
    if st.phase == Phase::Pending {
        st.tab.clone_from(&tab);
    }
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
    if matches!(st.phase, Phase::Pending | Phase::Announced { .. }) {
        let job = k.job(sf.pid);
        if let Some(job) = not_a_shell_job(job) {
            let r = not_a_job(opts, r, &mut st, job);
            save(opts, &sf.session_id, &st);
            return r;
        }
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
    facts.press = press_of(opts, &st, &tab, now);
    if t.is_empty() {
        // The sweep's process table was not read (it has no row): what runs under the agent is
        // unknown, which is work, held and clocked as work is, never nothing to restart over.
        facts.background.push(UNREADABLE_TABLE.to_string());
    }
    // What runs under the agent, classed once at this look: a quiet round moves over proved
    // waits and carries their commands, and tells real work its deadline.
    let own_held = if facts.quiet && !facts.background.is_empty() {
        held_read(k, sf.pid, t)
    } else {
        Some(Vec::new())
    };
    let own = own_at(&facts, own_held.as_deref());
    facts.follows_only = follows_at(&facts, own_held.as_deref());
    let own_held = own_held.unwrap_or_default();
    facts.waits_only = matches!(own, upgrade::Own::Waits(_));
    facts.deadline_set = st.deadline_at != 0 && st.deadline_pid == sf.pid;
    if upgrade::deadline_voids(&facts) {
        // The hold the line named is broken (a person, a wall, or nothing running): the
        // deadline is void, and work that holds the round again is told its own.
        let was = st.deadline_at;
        st.void_deadline();
        facts.deadline_set = false;
        facts.own_hold_s = 0;
        ledger(
            opts,
            &Report {
                step: "deadline-void".to_string(),
                ..r.clone()
            },
            &format!(
                "the deadline aterm told the agent ({}) is void: this look found {}; work of its \
                 own that holds the upgrade again is told a deadline of its own",
                if was > now {
                    format!("{} ahead", upgrade::span(was - now))
                } else {
                    format!("{} past", upgrade::span(now - was))
                },
                if facts.limited || facts.login {
                    "a wall"
                } else if upgrade::person_seen(&facts) {
                    "a person at the session"
                } else {
                    "nothing of its own running"
                },
            ),
        );
    }
    facts.work_due = facts.deadline_set && now >= st.deadline_at;
    if !facts.deadline_set {
        // A line counts only while its deadline stands: one that never reached the agent, one
        // an older build typed with no deadline and one a look voided are none, and the work
        // that holds the round is told.
        facts.tells = 0;
    }
    if let Some(r) = left_first(opts, &r, &mut st, &mut c, &tab, sf, now) {
        return r;
    }
    let records = Records::of_file(&opts.home, sf);
    let recent = records
        .transcript(&sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES));
    let tail = recent.as_ref().map(|(text, _)| text.as_str());
    let rearmed = login_facts(
        &records,
        &sf.session_id,
        &mut st,
        &mut facts,
        tail,
        looked_hand,
        now,
    );
    let asked = st.request_for(&tab);
    let requeued = queue_facts(&mut st, &mut facts, tail, now, &asked);
    facts.limit_s = wall_age(
        facts.limited,
        &facts,
        tail.and_then(upgrade::transcript_limit_since),
        looked_hand,
        now,
    );
    let ready = heard(&st, sf, &tab, tail);
    ready_facts(&st, sf, &tab, tail, ready, &mut facts);
    st.time_ready(ready, &mut facts, now);
    st.time_failed(&mut facts, now);
    st.time_work(&mut facts, now);
    let request = st.request_for(&tab);
    let mut asked_about = match rearmed {
        Some(phase) if !ready => {
            facts.undelivered = false;
            phase
        }
        _ => st.phase.clone(),
    };
    let taken_back = asked_about != st.phase;
    r.rung = Some(upgrade::rung_in_force(&upgrade::requested_facts(
        &request, &facts,
    )));
    let mut step = upgrade::requested_step(&request, &asked_about, &facts, ready, now, &st.to);
    if opts.background {
        step = upgrade::break_step(step, &facts);
    }
    let mut rearmed_here = None;
    if step == Step::Rearm
        && facts.work_ended
        && matches!(&asked_about, Phase::Failed(why) if why == upgrade::GAVE_UP)
        && !opts.dry_run
    {
        rearmed_here = Some(rearm(opts, r.clone(), &mut st, now));
        let request = st.request_for(&tab);
        r.rung = Some(upgrade::rung_in_force(&upgrade::requested_facts(
            &request, &facts,
        )));
        step = upgrade::requested_step(&request, &st.phase, &facts, false, now, &st.to);
        asked_about = st.phase.clone();
    }
    // The relaunch the move would type from the foreground shell `shell`: the notice carries its
    // line, and a tell, which promises the move, goes only where it can be made.
    let plan_move = if super::native_restart::coordinated(&st) {
        super::relaunch::plan_native_upgrade
    } else {
        plan
    };
    let move_plan = |shell: u32| -> Result<Plan, NoPlan> {
        let home = args.env_var(super::accounts::HOME_ENV);
        let homedir = home_carried(&opts.home, home, &sf.cwd, &records)?;
        let mode = resume_mode(
            &records,
            &sf.session_id,
            shown_mode.clone(),
            super::unix_time::parse_lstart(&sf.proc_start),
        );
        let launch_record = super::accounts::LaunchEnv::of_exec_env(&args.env);
        plan_move(
            opts,
            &Launch {
                homedir: homedir.as_deref(),
                launch_env: launch_record.as_ref(),
                ..launch_of(
                    sf,
                    &args.argv,
                    &target,
                    mode.as_deref(),
                    true,
                    home,
                    args.env_var(super::accounts::LOGIN_ENV),
                    route_of(&args.env),
                    !args.env.is_empty(),
                )
            },
            shell,
            t,
        )
    };
    // A tell names the newer build waiting and promises aterm's move onto it, so it waits its own
    // work's word where the relaunch the move would type cannot be planned now (its shell, the
    // HOME it carries), before the build's start is probed for a line it will not type.
    if let Step::Tell(why) = step
        && foreground_shell(k.job(sf.pid))
            .ok()
            .map_or(Err(NoPlan::Wait("ids")), move_plan)
            .is_err()
    {
        step = Step::Wait(why);
    }
    let mut proven = None;
    if matches!(step, Step::Announce | Step::Terminate | Step::Fresh) {
        match foreground_shell(k.job(sf.pid)) {
            Ok(shell) => proven = Some(shell),
            Err(why) => step = Step::Wait(why),
        }
    }
    // A tell promises the move (and, offered, the agent's own restart), so it is held by the
    // build's start as the move would be.
    let moving = matches!(
        step,
        Step::Announce | Step::Terminate | Step::Fresh | Step::Tell(_)
    );
    if (upgrade::asked_for_a_move(&asked_about, ready) || moving)
        && build_starts(opts, &r, &target, now, request == Request::Now, moving).is_err()
    {
        step = upgrade::build_hold(step, &asked_about, ready);
    }
    // A view types nothing, and a release owed goes first: the line it types says the upgrade is
    // off, which a tell beside it would contradict.
    if let Step::Tell(why) = step
        && (opts.dry_run || !st.release.is_empty())
    {
        step = Step::Wait(why);
    }
    let held_point = opts
        .dry_run
        .then(|| upgrade::held_point(&request, &step, &facts, now, &st.to))
        .flatten()
        .map(|w| upgrade::wait_word(w, &facts.status));
    let result = match step {
        Step::Wait("attended") => attended_once(opts, r, &mut st),
        Step::Wait(why) => said(r, format!("wait:{why}")),
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
                    &k.describe(sf.pid, &background_procs(sf.pid, t), t),
                    facts.quiet,
                ),
            );
            r
        }
        Step::Void(BUILD_FAILS) => build_held(opts, r, &mut st, now),
        Step::Void(why) => drain_expired(opts, r, &mut st, why, &facts, now),
        Step::Announce => {
            if let Err(why) = require_unique_owner(&opts.home, sf) {
                return said(r, format!("wait:{why}"));
            }
            let planned = proven.map_or(Err(NoPlan::Wait("ids")), move_plan);
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
            let retry = Retry {
                looked: looked_hand,
                owner_now: st.request_for(&tab) == Request::Now,
                at_break: facts.background_point,
                grace_s: upgrade::person_grace(upgrade::rung_in_force(&facts), opts.human_grace_s),
            };
            let mut cut: Option<(String, String)> = None;
            let result = announce(opts, r, &mut st, planned, now, asks, again, |asks| {
                let marker = upgrade::ready_marker(
                    &sf.session_id,
                    &target.version,
                    salt.wrapping_add(u64::from(asks)),
                );
                let text = upgrade::with_running(
                    &upgrade::prepare_prompt(&running, &target.version, target.source, &marker),
                    &upgrade::running_clause(&k.describe(sf.pid, &background_procs(sf.pid, t), t)),
                );
                require_unique_owner(&opts.home, sf).map_err(str::to_string)?;
                if host_claim.is_some_and(|claim| !claim_still_live(&mut c, sf.pid, claim)) {
                    return Err("tab-ownership-changed".to_string());
                }
                match type_fenced(
                    &mut c,
                    &tab,
                    &text,
                    Some(&retry),
                    Put::Pressed,
                    opts.timings.fence_every,
                    Lane::Upgrade,
                ) {
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
                st.line = planned_line.unwrap_or_default();
                st.login = args.env_var(super::accounts::LOGIN_ENV).map(str::to_owned);
                st.tab.clone_from(&tab);
                st.notice_pid = sf.pid;
                st.notice_start = squash(&sf.proc_start);
            }
            result
        }
        Step::Tell(why) => {
            // The agent's own restart is named only where its relaunch takes the target: a build
            // proved to start (`build_proved`), as that relaunch requires.
            let verb = opts.tell == Tell::Verb && build_proved(opts, &r, &target, now).is_ok();
            tell(
                opts,
                r,
                &mut st,
                &mut c,
                &Told {
                    tab: &tab,
                    sf,
                    host_claim,
                    why,
                    from: &running.to_string(),
                    held_s: facts.own_hold_s,
                    held: &own_held,
                    verb,
                },
                k,
                now,
            )
        }
        Step::Terminate if opts.dry_run && upgrade::walls_outlasted(&facts) => {
            said(r, WALL_RESTART)
        }
        Step::Fresh if opts.dry_run && upgrade::walls_outlasted(&facts) => said(r, WALL_FRESH),
        Step::Terminate if opts.dry_run => said(r, "would-restart"),
        Step::Fresh if opts.dry_run => said(r, "would-restart:fresh"),
        Step::Terminate | Step::Fresh => {
            let fresh = step == Step::Fresh;
            // A press restarts in the quiet flow where nothing was asked, or where the tab's
            // harness is off; one past an announced round carries on as the ask flow does.
            let pressed = facts.press == upgrade::Press::Acting;
            let unasked = !fresh
                && ((facts.quiet && asked_about == Phase::Pending)
                    || (pressed
                        && (asked_about == Phase::Pending || opts.press.is_some_and(|p| p.off))));
            let owed_here = !st.release.is_empty() && st.notice_belongs_to(sf, &tab);
            // A press past the agent's own background work ends that work, so the agent is told:
            // with the tab's harness on, the relaunch owes its carry-on. A quiet round's own move
            // over that work (`upgrade::own_yields`) ends it too: the carry-on is owed, with the
            // waits' commands or the work that ended at its deadline. Every other quiet move ends
            // no work (`upgrade::quiet_move`), and the loop's own first point goes on.
            let over = if fresh || pressed {
                None
            } else {
                upgrade::own_yields(&facts)
            };
            let ended_work =
                (pressed && !facts.background.is_empty() && !opts.press.is_some_and(|p| p.off))
                    || over.is_some();
            // A move at a wall past its bound (`upgrade::login_holds`, `upgrade::limit_holds`)
            // resumes as it was: nothing is typed after it into a build that starts at the same
            // wall, and the limit lane carries the session on past a limit's reset.
            let quiet = (unasked || facts.login || facts.limited) && !owed_here;
            st.quiet = quiet && !ended_work;
            // The waits are this look's, so one that ends by itself before the signal is still
            // started again; the last look before the signal holds what runs then to this list.
            st.ended_waits = match (&over, &own) {
                (Some(upgrade::Yield::Waits), upgrade::Own::Waits(commands)) => commands.clone(),
                _ => Vec::new(),
            };
            st.ended_work = matches!(
                over,
                Some(upgrade::Yield::Deadline | upgrade::Yield::Outlasted)
            )
            .then(|| upgrade::held_list(&own_held));
            st.ended_held_s = if over == Some(upgrade::Yield::Outlasted) {
                facts.own_hold_s
            } else {
                0
            };
            if !fresh {
                (st.model_before, st.mark) = model_and_mark(recent.as_ref());
            }
            st.launch_model = upgrade::launch_model(&args.argv).unwrap_or_default();
            let launch_record = super::accounts::LaunchEnv::of_exec_env(&args.env);
            let restarting = Restarting {
                argv: &args.argv,
                target: &target,
                host_claim,
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
                pressed,
                over,
                lagging: !fresh
                    && facts.quiet
                    && request == Request::Now
                    && upgrade::status_lags(&facts),
                mode: shown_mode.as_deref(),
                login: args.env_var(super::accounts::LOGIN_ENV),
                home: args.env_var(super::accounts::HOME_ENV),
                route: route_of(&args.env),
                auth_observed: !args.env.is_empty(),
                launch_env: launch_record.as_ref(),
            };
            let before = st.phase.clone();
            let r = restart(opts, r, &mut st, &mut c, &tab, sf, t, k, &restarting);
            if st.phase == before {
                // No signal went: nothing ended, and nothing is owed a continuation.
                st.ended_waits.clear();
                st.ended_work = None;
                st.ended_held_s = 0;
            }
            r
        }
    };
    let restart_word = (!opts.dry_run && matches!(step, Step::Terminate | Step::Fresh))
        .then(|| result.step.clone());
    let result = if release_next(&step, &result.step, &st, &facts, ready) {
        let ready = heard(&st, sf, &tab, tail);
        release(
            opts, result, &mut st, &mut c, &tab, sf, host_claim, k, &facts, ready, tail,
        )
    } else {
        result
    };
    let result = match rearmed_here {
        Some(rearmed) if !result.step.starts_with("announced:") => rearmed,
        _ => result,
    };
    st.saw_work(&facts);
    st.note_step(&result.step, &facts.status, now);
    if upgrade::person_seen(&facts) {
        // Whatever the step's word (the agent's work outranks a person in it), a look that
        // finds a person restarts the own-work hold ([`upgrade::person_seen`]).
        st.own_since = 0;
    }
    st.restart_look =
        result.step.starts_with("wait:") && restart_word.as_deref() == Some(result.step.as_str());
    let copy = if running_native { "foreign" } else { "managed" };
    st.annotate_wait(
        now,
        &wait_note(
            &facts,
            &format!("{seen} copy={copy}"),
            facts.background.len(),
        ),
    );
    let held = if facts.background.is_empty()
        || !(result.step.starts_with("wait:") || result.step.starts_with("told:"))
    {
        Vec::new()
    } else {
        k.describe(sf.pid, &background_procs(sf.pid, t), t)
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

/// What runs under the agent at look `f`, classed ([`upgrade::own_of`]): `held` its shells as
/// read ([`held_read`]; `None`: unread, which is work), the work its screen names off
/// [`Facts::background`] ([`own_work`]).
fn own_at(f: &Facts, held: Option<&[upgrade::Held]>) -> upgrade::Own {
    let screen = f.background.iter().any(|w| w.starts_with("screen:"));
    upgrade::own_of(held.unwrap_or_default(), screen, held.is_none())
}

/// Whether all that runs under the agent at look `f` is follows and proved waits
/// ([`upgrade::follows_only`]), read from the same rows as [`own_at`].
fn follows_at(f: &Facts, held: Option<&[upgrade::Held]>) -> bool {
    let screen = f.background.iter().any(|w| w.starts_with("screen:"));
    upgrade::follows_only(held.unwrap_or_default(), screen, held.is_none())
}

pub(super) const AT_POINT: char = '@';

/// A dry look's word for the move past a wall that outlasted its bound
/// ([`upgrade::walls_outlasted`]), the agent's conversation kept: the only move the window's loop
/// offers a point at a wall for ([`St::wall_move`]).
pub(super) const WALL_RESTART: &str = "would-restart:wall";

/// [`WALL_RESTART`] for a session with no task, restarted afresh.
pub(super) const WALL_FRESH: &str = "would-restart:fresh-wall";

/// Whether `word`, a dry look's step, is the move past an outlasted wall ([`WALL_RESTART`],
/// [`WALL_FRESH`]).
pub(super) fn wall_step(word: &str) -> bool {
    let act = word.split_once(AT_POINT).map_or(word, |(act, _)| act);
    act == WALL_RESTART || act == WALL_FRESH
}

pub(super) fn rounds_step(word: &str) -> bool {
    let act = word.split_once(AT_POINT).map_or(word, |(act, _)| act);
    act == "would-announce"
        || act == "gave-up"
        || act == "would-restart"
        || act.starts_with("would-restart:")
        || act.starts_with("would-rearm:")
}

pub(super) fn at_point(word: &str, held: &str) -> String {
    format!("{word}{AT_POINT}{held}")
}

/// What the owner's press of the ⬆️ is to this step ([`upgrade::Facts::press`]): its lane
/// ([`press`]) and the watch's dry read act on a standing press, and every other step stands
/// aside for it.
fn press_of(opts: &Opts, st: &St, tab: &str, now: u64) -> upgrade::Press {
    if !st.press_stands(tab, now) {
        upgrade::Press::None
    } else if opts.press.is_some() || opts.dry_run {
        upgrade::Press::Acting
    } else {
        upgrade::Press::Stands
    }
}

pub(super) fn ladder_look(
    st: &mut St,
    agent: &str,
    scr: &Screen,
    pid: u32,
    grace_s: u32,
    now: u64,
) -> LadderLook {
    let reading = aterm_phase::read_seen(
        Some(agent),
        &scr.rows,
        scr.cursor.map(|(row, _)| row),
        scr.cursor.map(|(_, col)| col),
        scr.title.as_deref(),
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
    let still_run = print.as_deref().is_some_and(|p| st.still_runs(p, now));
    LadderLook {
        rung: st.rung_at(now),
        attended: upgrade::attended_by(scr.human, grace_s),
        typing: scr
            .human
            .within(upgrade::person_grace(upgrade::Rung::KeysOnly, grace_s))
            == Some(true),
        idle_read,
        still_run,
        still_s: st.still(print, now),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LadderLook {
    pub(super) rung: upgrade::Rung,
    pub(super) attended: bool,
    pub(super) typing: bool,
    pub(super) idle_read: bool,
    /// The look read the words above the composer the idle look before it read ([`St::still_runs`]).
    pub(super) still_run: bool,
    pub(super) still_s: u64,
}

/// Whether `scr` shows a Claude Code turn running ([`turn_runs`] over its rows and title).
pub(super) fn turn_shown(scr: &Screen) -> bool {
    turn_runs(&scr.rows, scr.title.as_deref())
}

/// Whether Claude Code's screen `rows` under window title `title` show a turn running: its busy
/// footer ([`busy_footer`]), or its title running one the screen does not show
/// (`aterm_phase::phase::title_live`: a reply drawn under a status line, which hides the footer).
/// A done row, a wall, a box or delegated work leave it to the footer, so a finished turn never
/// reads as running for the title.
#[must_use]
pub fn turn_runs(rows: &[String], title: Option<&str>) -> bool {
    use aterm_phase::ScreenReader as _;
    busy_footer(rows) || aterm_phase::phase::title_live(rows, aterm_phase::ClaudeReader.runs(title))
}

/// Whether Claude Code's footer shows its turn running: the interrupt hint on its status line,
/// or on a row below its composer. [`turn_runs`] reads the window title beside it.
#[must_use]
pub fn busy_footer(rows: &[String]) -> bool {
    let Some((caret, draft)) = aterm_phase::phase::composer_draft(rows) else {
        return false;
    };
    let busy = aterm_phase::anchor!("busy.interrupt");
    aterm_phase::phase::status_line(rows).is_some_and(|line| line.contains(busy))
        || rows
            .iter()
            .skip(caret.saturating_add(draft.len()))
            .any(|row| row.contains(busy))
}

fn look(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    host_claim: Option<&HostClaim>,
    t: &[ProcRow],
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
    let status = status_line(&mut c, tab);
    let read = TabRead {
        scr: &scr,
        status: status.as_deref(),
        dim: dim_placeholder(&mut c, tab, &scr),
    };
    let (facts, seen) = look_facts(opts, sf, st, tab, &read, t, now)?;
    Ok((
        c,
        facts,
        screen_mode(&scr.rows),
        HandAt::of(scr.human),
        seen,
    ))
}

pub(super) struct TabRead<'a> {
    pub(super) scr: &'a Screen,
    pub(super) status: Option<&'a str>,
    pub(super) dim: bool,
}

/// The facts [`upgrade::press_gate`] reads, off tab read `read` and the session's registry status
/// `status`, a held tab read at grace 0: what a step's look ([`look_facts`]) and a click's look
/// (`super::permission_click`) read alike. Every other fact is its default.
pub(super) fn press_facts(status: &str, read: &TabRead<'_>) -> Facts {
    let TabRead {
        scr,
        status: line,
        dim,
    } = *read;
    Facts {
        status: status.to_string(),
        composer_empty: upgrade::composer_is_empty(&scr.rows, scr.cursor, dim),
        // A standing `/login` dialog (its picker, the browser or paste hand-off) is the person's
        // or the login lane's, never a wall a clock ages ([`upgrade::login_holds`]).
        approval_box: aterm_phase::prompt::prompt_box_span(&scr.rows).is_some()
            || aterm_phase::login_screen(&scr.rows).is_some(),
        busy_footer: turn_shown(scr),
        held: line.is_none_or(|line| held_by_status(line, 0)),
        limited: upgrade::limited(&scr.rows),
        login: upgrade::login_wall(upgrade::Agent::Claude, &scr.rows),
        ..Facts::default()
    }
}

pub(super) fn look_facts(
    opts: &Opts,
    sf: &SessionFile,
    st: &mut St,
    tab: &str,
    read: &TabRead<'_>,
    t: &[ProcRow],
    now: u64,
) -> Result<(Facts, String), &'static str> {
    let TabRead { scr, status, .. } = *read;
    let ours = conversation_typed(opts, tab, Some(&sf.session_id), &[])
        .map_err(|_| "ledger-evidence-unreadable")?;
    let records = Records::of_file(&opts.home, sf);
    let person_left = matches!(st.phase, Phase::Pending | Phase::Announced { .. })
        && left_to_person(opts, &records, tab, &sf.session_id, st)
            .map_err(|_| "ledger-evidence-unreadable")?;
    let ladder = ladder_look(st, "claude", scr, sf.pid, opts.human_grace_s, now);
    // A turn over under the agent's own background work publishes the phase it rests at
    // (`aterm_phase::background_rest`), not busy: that work runs on, so its word is no quiet.
    let quiet = match quiet_s(status, st, scr.seq, now) {
        _ if aterm_phase::background_rest(&scr.rows).is_some() => 0,
        quiet => quiet,
    };
    let mut facts = Facts {
        status_age_s: now.saturating_sub(sf.status_updated_at_ms / 1000),
        background: own_work(sf.pid, t, &scr.rows),
        quiet_s: quiet,
        hold_s: 0,
        owner_now: false,
        attended: ladder.attended,
        background_point: opts.background,
        taskless: matches!(st.phase, Phase::Pending | Phase::Announced { .. })
            && records.tasked(&sf.session_id, &ours) == Some(false),
        undelivered: false,
        queued: false,
        ready_s: 0,
        idle_looks: 0,
        failed_s: 0,
        rung: ladder.rung,
        typing: ladder.typing,
        still_s: ladder.still_s,
        work_ended: false,
        ready_unrecorded: ladder.idle_read
            && !opts.background
            && aterm_phase::said_tail(&scr.rows)
                .is_some_and(|said| st.round_markers().any(|m| upgrade::says_ready(&said, m))),
        ready_superseded: false,
        quiet: !opts.ask
            || (st.phase == Phase::Pending
                && records.finished(&sf.session_id, &ours) == Some(true))
            || person_left,
        unnoticed: now.saturating_sub(st.behind_since()) < upgrade::quiet_s(),
        own_hold_s: if st.own_since == 0 {
            0
        } else {
            now.saturating_sub(st.own_since)
        },
        tells: if st.told_to == st.to { st.tells } else { 0 },
        may_tell: opts.tell != Tell::Off,
        silent: opts.tell == Tell::Silent,
        press: upgrade::Press::None,
        ..press_facts(&sf.status, read)
    };
    let idle_read = ladder.idle_read;
    // An idle look is in a row only over the words the one before it read. Under a status line a reply
    // being drawn shows no busy row. A title that runs it holds its looks busy ([`ladder_look`]);
    // under no title, or one that does not run it, its looks read idle, but the words above its
    // composer grow between them: two such looks are never the lag ([`upgrade::status_lags`])
    // `--now` ends a turn at.
    let in_a_row = ladder.still_run
        && st.idle_pid == sf.pid
        && now.saturating_sub(st.idle_at) <= IDLE_RUN_GAP_S;
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
    let render = status
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|t| t.strip_prefix("render="))
        })
        .unwrap_or("-");
    let place = |at: Option<usize>| at.map_or_else(|| "-".to_string(), |n| n.to_string());
    st.goal = aterm_phase::goal_active(&scr.rows);
    let seen = format!(
        "render={render} cursor={} caret={} goal={}",
        scr.cursor
            .map_or_else(|| "-".to_string(), |(row, col)| format!("{row},{col}")),
        place(aterm_phase::phase::composer_draft(&scr.rows).map(|(caret, _)| caret)),
        u8::from(st.goal),
    );
    Ok((facts, seen))
}

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

const GAVE_UP_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// How long a wall that `stands` at look `f` (its login wall, [`upgrade::Facts::login_s`]; its
/// usage limit, [`upgrade::Facts::limit_s`]) has stood with nobody at the session: since the first
/// row of the wall its transcript stands at (`since`: [`upgrade::LoginScan::since`],
/// [`upgrade::StandingScan::since`]), every look (the watch's dry ones too) reading the same age,
/// or since a key of the person's (`hand`) after it. Where the key record covers only the time
/// the host has served the session ([`HandAt::Since`]: aterm started serving it again, say after
/// its own update), the age runs from that start at the earliest, since a key before it is not
/// known. A look that sees a person reads 0, keys that cannot be read never age it, and nor does a
/// wall its transcript does not date (one on the screen alone).
pub(super) fn wall_age(stands: bool, f: &Facts, since: Option<u64>, hand: HandAt, now: u64) -> u64 {
    if !stands || upgrade::person_seen(f) {
        return 0;
    }
    let keyed = match hand {
        HandAt::At(at) | HandAt::Since(at) => now.saturating_sub(at.elapsed().as_secs()),
        HandAt::Never => 0,
        HandAt::Unknown => return 0,
    };
    since.map_or(0, |since| now.saturating_sub(since.max(keyed)))
}

fn login_facts(
    records: &Records,
    session: &str,
    st: &mut St,
    f: &mut Facts,
    tail: Option<&str>,
    hand: HandAt,
    now: u64,
) -> Option<Phase> {
    let said = tail.map_or_else(upgrade::TranscriptLogin::default, |t| {
        upgrade::transcript_login(&st.phase, &st.marker, t)
    });
    if said.stands {
        f.login = true;
        st.phase = upgrade::clock_held(&st.phase, f, now);
    }
    f.login_s = wall_age(f.login, f, said.since, hand, now);
    if let Some(lifted) = said.lifted_at {
        let _ = st.hold_clock(lifted);
    }
    let gave_up =
        matches!(&st.phase, Phase::Failed(why) if why == upgrade::GAVE_UP) && !st.marker.is_empty();
    if !gave_up {
        f.undelivered = said.undelivered == Some(true);
        return None;
    }
    let deep = records
        .transcript(session)
        .map(|p| tail_to_end(&p, GAVE_UP_TAIL_BYTES).0);
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
    let standing = upgrade::StandingScan::of(text);
    let (until, reset) = (standing.until(), standing.reset());
    st.limit_until = reset.unwrap_or(0);
    st.limit_spend = standing.spend();
    if f.quiet && reset.is_some_and(|reset| now >= reset.saturating_add(LIMIT_GRACE_S)) {
        f.limited = false;
    }
    // In the quiet flow, which asks nothing again, a limit that names no reset stands as long as
    // its row does: that is how long the window's loop withholds its points for it
    // (`StandingLimit::until` none), so its age ends it for the upgrade (`upgrade::limit_holds`),
    // not the re-ask past its row that the ask flow's notice keeps (`REASK_S`).
    let standing_unnamed = f.quiet && reset.is_none() && standing.limit().is_some();
    if until.is_some_and(|until| until > now) || standing_unnamed {
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
    if let Err(why) = leads_terminal(k.job(sf.pid)) {
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
    let text = upgrade::release_prompt(st.agent, f.quiet);
    match type_fenced(
        c,
        tab,
        &text,
        None,
        Put::Pressed,
        opts.timings.fence_every,
        Lane::Upgrade,
    ) {
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

/// What one [`tell`] says, and to which tab.
struct Told<'a> {
    tab: &'a str,
    sf: &'a SessionFile,
    host_claim: Option<&'a HostClaim>,
    why: &'static str,
    from: &'a str,
    held_s: u64,
    /// What runs under the agent at this look, as the process table names it.
    held: &'a [upgrade::Held],
    /// The line names `aterm ctl @self restart` ([`Tell::Verb`], the target proved to start).
    verb: bool,
}

/// Types the quiet round's line to the agent whose own work (`told.why`) holds its move
/// ([`Step::Tell`]): what waits, and the deadline after which the restart goes at its first turn
/// end ([`upgrade::WORK_DEADLINE_S`]). The deadline is stamped only for a line the host took
/// whole, submit included: nothing moves over work on a line that never reached the agent. Every
/// other end names no deadline and counts no line, so the work that holds the round is told again:
/// - a refusal before the first key, or a turn the server reports untyped (`changed`, a yield to
///   a person), changes nothing, and the next look types it;
/// - a line cut short, refused after its keys or unanswered may stand in the composer or may
///   have landed, so the own-work hold starts again from now ([`St::own_since`], saved once the
///   fence is read, right before the first key, so a crash mid-type starts it again too): what
///   stands is settled first ([`left_first`]), and the line is typed again only once the work
///   has held the round [`upgrade::WORK_TELL_S`] more. A save that fails types nothing.
fn tell(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    told: &Told<'_>,
    k: &dyn Kernel,
    now: u64,
) -> Report {
    let Told {
        tab,
        sf,
        host_claim,
        why,
        from,
        held_s,
        held,
        verb,
    } = *told;
    let waits = |r| said(r, format!("wait:{why}"));
    if leads_terminal(k.job(sf.pid)).is_err()
        || !k
            .terminal(sf.pid)
            .as_ref()
            .is_some_and(|(pid, name)| owned_by_aterm((*pid, name.as_str())))
        || require_unique_owner(&opts.home, sf).is_err()
        || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
    {
        return waits(r);
    }
    let text = upgrade::tell_prompt(
        st.agent,
        &upgrade::Telling {
            from,
            to: &st.to,
            held_s,
            deadline_s: upgrade::WORK_DEADLINE_S,
            held,
            verb,
        },
    );
    let held_since = st.own_since;
    // Whether the first key may have gone: the fence was read and the record saved.
    let mut keyed = false;
    let typed = {
        let st = &mut *st;
        let keyed = &mut keyed;
        type_fenced_then(
            c,
            tab,
            &text,
            None,
            Put::Pressed,
            opts.timings.fence_every,
            &mut || {
                st.own_since = now;
                if save_checked(opts, &sf.session_id, st).is_err() {
                    st.own_since = held_since;
                    return Err(Typed::Refused("unsaved".to_string()));
                }
                *keyed = true;
                Ok(())
            },
            Lane::Upgrade,
        )
    };
    match typed {
        Ok(()) => {
            st.own_since = held_since;
            st.tells = 1;
            st.told_at = now;
            st.told_to.clone_from(&st.to);
            st.deadline_at = now.saturating_add(upgrade::WORK_DEADLINE_S);
            st.deadline_pid = sf.pid;
            let r = said(r, format!("told:{why}"));
            ledger(
                opts,
                &r,
                &format!(
                    "the agent's own running work held the quiet upgrade to {} for {}: one line \
                     told it what waits and that the restart goes at its first turn end {} from \
                     now, ending what still runs under it then; nothing was ended or asked of it \
                     now",
                    st.to,
                    upgrade::span(held_s),
                    upgrade::span(upgrade::WORK_DEADLINE_S),
                ),
            );
            r
        }
        Err(e) => {
            if !keyed || matches!(e, Typed::Changed | Typed::Yielded | Typed::Person) {
                // Nothing was typed: the hold stands as it was, and the next look types it.
                st.own_since = held_since;
            } else {
                // Keys may have gone: what stands of the line is settled before anything else
                // is typed, and no deadline is named by a line the agent may never have read.
                left_cut(st, c, tab, &text);
            }
            said(r, format!("wait:{}", e.word()))
        }
    }
}

fn owed_word(r: Report, why: &str) -> Report {
    if r.is_act() && after(&r.step, 0) == After::NextIdle {
        r
    } else {
        said(r, format!("wait:release:{why}"))
    }
}

#[allow(clippy::too_many_arguments)]
fn release_visit(
    opts: &Opts,
    r: Report,
    sf: &SessionFile,
    mut st: St,
    tab: &str,
    t: &[ProcRow],
    k: &dyn Kernel,
    host_claim: Option<&HostClaim>,
) -> Report {
    let now = now_s();
    let recent = Records::of_file(&opts.home, sf)
        .transcript(&sf.session_id)
        .map(|p| tail_to_end(&p, TAIL_BYTES));
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

fn release_void(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> Option<&'static str> {
    if !st.notice_belongs_to(sf, tab) {
        return Some("other-process");
    }
    tail.is_some_and(|t| upgrade::directed_since_ready(t, &st.asked))
        .then_some("directed")
}

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

pub(super) fn model_and_mark(recent: Option<&(String, u64)>) -> (String, u64) {
    recent.map_or((String::new(), 0), |(text, end)| {
        (upgrade::transcript_model(text).unwrap_or_default(), *end)
    })
}

/// The step that closes a restart whose relaunch line was not typed since the agent's last end,
/// once a process aterm did not start holds the conversation in its tab ([`resumed_by_another`]).
pub const RESUMED_BY_ANOTHER: &str = "done:resumed-by-another";

/// Why a relaunch whose agy holds no lock on its conversation is failed: it started a fresh one, or
/// another agy has that conversation open (`refused:started-fresh`).
pub const STARTED_FRESH: &str = "started-fresh";

/// Why a restart whose conversation came back in its tab on an older build than its target, by a
/// launch it never typed, is stopped ([`resumed_by_another`]).
pub const RESUMED_BEHIND: &str = "resumed-by-another";

/// An in-flight restart whose relaunch line was not typed since the agent's last end
/// ([`Phase::Exiting`]: aterm's own, or a relaunched build's that ended on its own,
/// [`St::roll_back`]) while `sf`, a process in its own tab, holds the conversation: someone else
/// started it (a person at the prompt, or another driver). It is never adopted as aterm's relaunch, nothing is typed into it,
/// and the record and ledger say who resumed it: [`RESUMED_BY_ANOTHER`], or a stop
/// ([`RESUMED_BEHIND`]) when an upgrade's conversation came back on an older build than its
/// target.
fn resumed_by_another(opts: &Opts, mut r: Report, st: &mut St, sf: &SessionFile) -> Report {
    if opts.dry_run {
        return said(r, "would-close:resumed-by-another");
    }
    let now = now_s();
    let waited = match st.phase {
        Phase::Exiting { at_s } if !st.wait.is_empty() && st.wait_since >= at_s => {
            let words = wait_words_of(st.agent, &st.wait)
                .map(|words| format!(": {words}"))
                .unwrap_or_default();
            format!(" waited (`wait:{}`{words}) and was", st.wait)
        }
        _ => " was".to_string(),
    };
    st.seen_gone(now);
    let before = recorded(&st.model_before)
        .map(|m| format!("; it ran {m} before the restart"))
        .unwrap_or_default();
    let version = crate::supervise::limit::one_line(&sf.version);
    let detail = format!(
        "claude {version} (pid {}) holds the conversation in its tab, started by someone else, \
         not by aterm: the relaunch line aterm owed since the agent's last end{waited} never \
         typed{before}",
        sf.pid
    );
    st.resumed_on = version.clone();
    st.resumed_pid = sf.pid;
    st.relaunched_pid = 0;
    st.confirm_by = 0;
    let behind = st.is_upgrade()
        && matches!(
            (Version::parse(&sf.version), Version::parse(&st.to)),
            (Some(running), Some(to)) if running < to
        );
    if behind {
        st.stop(RESUMED_BEHIND, now);
        let r = said(r, format!("failed:{RESUMED_BEHIND}"));
        ledger(
            opts,
            &r,
            &format!("{detail}; it runs an older build than {}", st.to),
        );
        save(opts, &sf.session_id, st);
        return r;
    }
    st.phase = Phase::Done;
    r.to = format!("{version}({})", st.source);
    let r = said(r, RESUMED_BY_ANOTHER);
    st.outcome = detail;
    st.done_at = now;
    ledger(opts, &r, &st.outcome);
    save(opts, &sf.session_id, st);
    r
}

fn orphans(opts: &Opts, files: &[SessionFile], live_tabs: Option<&[LiveTab]>) -> Vec<Report> {
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
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
        let Some(mut st) = load_record(opts, &session)
            .ok()
            .flatten()
            .filter(St::in_flight)
        else {
            continue;
        };
        if !driven_here(st.agent) {
            continue;
        }
        if live_tabs.is_some_and(|tabs| !tab_is_live(tabs, &st.tab)) {
            continue;
        }
        if conversation_live(files, &session) {
            continue; // `visit` has it.
        }
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let held = if st.home.is_some() {
            st.agent_records(&opts.home)
                .session_files()
                .is_some_and(|files| conversation_live(&files, &session))
        } else {
            registered_since(&fresh, &opts.home, &session)
        };
        if held {
            continue; // `visit` has it, or the loop's `resume`.
        }
        let r = Report {
            rung: None,
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.clone(),
            from: st.from.clone(),
            to: format!("{}({})", st.to, st.source),
            step: String::new(),
        };
        let mut native_client = None;
        if !st.native_retired_unissued
            && (st.native.is_some() || !st.native_prepare.is_empty())
            && !opts.dry_run
        {
            let tab = st.tab.clone();
            let Ok(mut client) = connect(opts, &tab) else {
                out.push(said(r, "wait:native-recovery"));
                continue;
            };
            match super::native_restart::refresh_retirement_hold(&mut client, &tab, &mut st) {
                Ok(true) => save(opts, &session, &st),
                Ok(false) => {}
                Err(why) => {
                    out.push(said(r, format!("wait:{why}")));
                    continue;
                }
            }
            native_client = Some(client);
        }
        if st.native_retired_unissued {
            if opts.press.is_none() {
                out.push(said(r, super::native_restart::WAIT_RETIRED_UNISSUED));
            }
            continue;
        }
        // Held only by the agent the record names by pid and kernel start, never by another
        // process at its pid (a reboot reuses them): that one would hold it here for good.
        if st.agent_exit() != super::relaunch::AgentExit::Gone {
            note_stuck(opts, &r, &mut st, &session, now_s());
            continue;
        }
        let front = tab_front(live_tabs, &st.tab, st.shell);
        if let Some(sf) = overtaken_by(&st, &session, files, front) {
            if opts.dry_run {
                if opts.press.is_none() {
                    out.push(said(r, format!("would-fail:{TAB_TAKEN}")));
                }
                continue;
            }
            if let Some(client) = native_client.as_mut()
                && let Some(wait) =
                    super::relaunch::native_retirement_wait(opts, &r, &mut st, &session, client)
            {
                out.push(wait);
                continue;
            }
            let line = overtaken_line(sf.pid, &sf.session_id);
            st.close_overtaken(TAB_TAKEN, now_s());
            let r = said(r, format!("failed:{TAB_TAKEN}"));
            ledger(opts, &r, &line);
            save(opts, &session, &st);
            if opts.press.is_none() {
                out.push(r);
            }
            continue;
        }
        if opts.press.is_some() && !st.is_upgrade() {
            continue;
        }
        // An exit read past its restart's signal deadline is settled before anything here stamps
        // the exit or a hand's sight, either of which would let the restart claim the person's
        // own end ([`super::relaunch::settle_past_signal`]), as the host's first sight settles it.
        if super::relaunch::exit_past_signal(&st, now_s()) {
            if let Some(client) = native_client.as_mut()
                && let Some(wait) =
                    super::relaunch::native_retirement_wait(opts, &r, &mut st, &session, client)
            {
                out.push(wait);
                continue;
            }
            out.push(super::relaunch::settle_past_signal(
                opts,
                r,
                &mut st,
                &session,
                now_s(),
            ));
            continue;
        }
        if hand_waits_out(opts, &mut st, &session, alive) {
            if st.seen_gone(now_s()) {
                save(opts, &session, &st);
            }
            out.push(said(r, WAIT_HAND_LIFTS));
            continue;
        }
        let stale = expired(&st, now_s());
        if !opts.dry_run && st.seen_gone(now_s()) {
            save(opts, &session, &st);
        }
        if let Some((why, detail)) = stale {
            if opts.dry_run {
                out.push(said(r, format!("would-fail:{why}")));
                continue;
            }
            if let Some(client) = native_client.as_mut()
                && let Some(wait) =
                    super::relaunch::native_retirement_wait(opts, &r, &mut st, &session, client)
            {
                out.push(wait);
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

fn answered(st: &St, tail: Option<&str>) -> bool {
    let Some(text) = tail else {
        return false;
    };
    st.round_markers()
        .any(|m| upgrade::transcript_has_ready(text, m))
}

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

fn heard(st: &St, sf: &SessionFile, tab: &str, tail: Option<&str>) -> bool {
    st.hears_ready() && st.notice_belongs_to(sf, tab) && answered(st, tail)
}

fn release_is_next(step: &Step, word: &str) -> bool {
    match step {
        Step::GiveUp | Step::Void(_) => true,
        Step::Wait(why) | Step::Tell(why) => upgrade::release_is_next(why),
        Step::Announce | Step::Terminate | Step::Fresh => {
            word.starts_with("failed:") || word.starts_with("refused:")
        }
        Step::Rearm => false,
    }
}

fn release_next(step: &Step, word: &str, st: &St, f: &Facts, ready: bool) -> bool {
    !st.release.is_empty()
        && (release_is_next(step, word)
            || (*step == Step::Wait("background") && unanswered_at_break(st, f, ready))
            || quiet_release_due(step, st, f))
}

fn quiet_release_due(step: &Step, st: &St, f: &Facts) -> bool {
    f.quiet
        && st.phase == Phase::Pending
        && matches!(step, Step::Wait(w) if upgrade::own_work_holds(w))
}

const fn unanswered_at_break(st: &St, f: &Facts, ready: bool) -> bool {
    f.background_point && !ready && matches!(st.phase, Phase::Announced { .. })
}

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
        "status-stale" => (
            "Claude Code's own status, never back to idle over an idle screen,",
            f.ready_s,
        ),
        _ => ("work of the agent's own still running", f.ready_s),
    };
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

fn build_held(opts: &Opts, r: Report, st: &mut St, now: u64) -> Report {
    if opts.dry_run {
        return said(r, format!("would-fail:{BUILD_FAILS}"));
    }
    st.stop(BUILD_FAILS, now);
    let r = said(r, format!("failed:{BUILD_FAILS}"));
    ledger(
        opts,
        &r,
        "the build this round would move the agent onto failed to start and is not proved to \
         start since, so no move comes: the round stops, the agent it asked is owed its \
         release, and the next round asks nothing until the build is proved to start or a \
         newer one is the target",
    );
    r
}

fn not_a_job(opts: &Opts, r: Report, st: &mut St, job: Job) -> Report {
    if opts.dry_run {
        return said(r, "would-refuse:not-a-shell-job");
    }
    st.stop("not-a-shell-job", now_s());
    if job == Job::NoJobControl {
        st.release.clear();
    }
    let r = said(r, "refused:not-a-shell-job");
    ledger(
        opts,
        &r,
        "the agent is no job of an interactive shell: nothing would take the terminal back to relaunch it",
    );
    r
}

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

pub(super) fn note_stuck(opts: &Opts, r: &Report, st: &mut St, session: &str, now: u64) {
    if opts.dry_run || !driven_here(st.agent) || !st.is_upgrade() {
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
        Err(e) if e == TURN_RUNNING => said(r, format!("wait:{TURN_RUNNING}")),
        Err(e) => said(r, format!("wait:announce-refused:{}", first_word(&e))),
    }
}

#[allow(clippy::too_many_arguments)]
fn launch_of<'a>(
    sf: &'a SessionFile,
    argv: &'a [String],
    target: &'a Candidate,
    mode: Option<&'a str>,
    resume: bool,
    home: Option<&'a str>,
    login: Option<&'a str>,
    route: super::accounts::AuthRoute,
    auth_observed: bool,
) -> Launch<'a> {
    Launch {
        session: &sf.session_id,
        resume,
        cwd: &sf.cwd,
        agent: sf.pid,
        argv,
        exe: &target.exe,
        mode,
        home,
        homedir: None,
        login,
        route,
        auth_observed,
        launch_env: None,
        serve: crate::harness::relaunch::Serve::Judge,
    }
}

pub(crate) fn homedir_holding(dir: &Path) -> Option<String> {
    let parent = dir
        .parent()
        .filter(|_| dir.file_name().is_some_and(|n| n == ".claude"))?;
    parent
        .is_absolute()
        .then(|| parent.to_str().map(str::to_owned))
        .flatten()
}

/// How a process whose exec environment is `env` authenticates: an unread one is outside. An
/// upgrade is a launch path, so it carries a token route.
fn route_of(env: &[String]) -> super::accounts::AuthRoute {
    super::accounts::ProcessLogin::of_exec_env(env)
        .map_or(super::accounts::AuthRoute::Outside, |who| who.route)
}

pub(super) fn home_carried(
    user_home: &Path,
    home: Option<&str>,
    cwd: &str,
    records: &Records,
) -> Result<Option<String>, NoPlan> {
    let Some(home) = home else {
        return match records.dir() {
            Some(dir) if !same_dir(dir, &user_home.join(".claude")) => homedir_holding(dir)
                .map(Some)
                .ok_or_else(|| NoPlan::Refused {
                    why: "line:home".to_string(),
                    what: "home".to_string(),
                    detail: format!(
                        "it runs under another HOME than its shell's (its records are in {}), \
                         and no HOME the relaunch line could carry names them",
                        dir.display()
                    ),
                }),
            _ => Ok(None),
        };
    };
    if Path::new(home).is_absolute() {
        return Ok(None);
    }
    if cwd.is_empty() {
        return Ok(None);
    }
    let at = home_dir_at(home, || Some(PathBuf::from(cwd)));
    if at.is_some() && at.as_deref() == records.dir() {
        return Ok(None);
    }
    let named = if home.is_empty() {
        "is set and empty, which names the directory it runs in".to_string()
    } else {
        format!("({home}) is relative")
    };
    Err(NoPlan::Refused {
        why: "line:home".to_string(),
        what: "home".to_string(),
        detail: format!(
            "its CLAUDE_CONFIG_DIR {named}, and the relaunch line would start it in {cwd}, under \
             another Claude home than the one its records are in"
        ),
    })
}

/// The last look before typing into Claude Code's composer: the screen's generation, and the
/// person's input count of the same read (`turn if-human=`).
pub(super) fn typing_fence(
    c: &mut Client,
    tab: &str,
) -> Result<(Option<String>, Option<u64>), &'static str> {
    typing_fence_read(c, tab).map(|(generation, read)| (generation, read.human_seq))
}

fn typing_fence_read(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let _ = turn_takes_gen(c);
    let Some(first) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    let Some(dim_at_row) = dim_cell(&first) else {
        return fence_verdict(&first, false).map(|generation| (generation, first));
    };
    let dim = dim_at(c, tab, dim_at_row);
    let Some(scr) = screen(c, tab) else {
        return Err("screen-unreadable");
    };
    if !same_composer(&first, &scr) {
        return Err("changed");
    }
    fence_verdict(&scr, dim).map(|generation| (generation, scr))
}

const ROWS_FENCE: &str = "rows:";

const TURN_RUNNING: &str = "busy";

fn claude_fence(c: &mut Client, tab: &str) -> Result<(Option<String>, Screen), &'static str> {
    let (generation, read) = typing_fence_read(c, tab)?;
    if turn_shown(&read) {
        return Err(TURN_RUNNING);
    }
    let narrow = composer_rows(&read)
        .filter(|_| turn_takes_rows(c))
        .map(|fence| format!("{ROWS_FENCE}{fence}"));
    Ok((narrow.or(generation), read))
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandAt {
    Unknown,
    /// No key since the host began serving the session, with no span reported.
    Never,
    /// No key since this instant, when the host began serving the session
    /// ([`HumanInput::NeverFor`]); what came before it is not known.
    Since(Instant),
    At(Instant),
}

impl HandAt {
    pub(super) fn of(human: HumanInput) -> HandAt {
        match human {
            HumanInput::Unknown => HandAt::Unknown,
            HumanInput::Never => HandAt::Never,
            HumanInput::NeverFor(ms) => Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .map_or(HandAt::Unknown, HandAt::Since),
            HumanInput::Ago(ms) => Instant::now()
                .checked_sub(Duration::from_millis(ms))
                .map_or(HandAt::Unknown, HandAt::At),
        }
    }

    pub(super) fn still(self, later: HandAt) -> bool {
        match (self, later) {
            (_, HandAt::Never | HandAt::Since(_)) => true,
            (HandAt::At(then), HandAt::At(now)) => now <= then + HAND_SLACK,
            _ => false,
        }
    }
}

const HAND_SLACK: Duration = Duration::from_millis(500);

pub(super) struct Retry {
    pub(super) looked: HandAt,
    pub(super) owner_now: bool,
    pub(super) at_break: bool,
    pub(super) grace_s: u32,
}

pub(super) fn retry_open(read: &Screen, retry: &Retry) -> bool {
    let hand = HandAt::of(read.human);
    if !retry.looked.still(hand) {
        return false;
    }
    if !retry.owner_now && upgrade::attended_by(read.human, retry.grace_s) {
        return false;
    }
    if turn_shown(read) {
        return false;
    }
    if retry.at_break {
        return true;
    }
    reads_idle(read)
}

fn reads_idle(read: &Screen) -> bool {
    let reading = aterm_phase::read_seen(
        Some("claude"),
        &read.rows,
        read.cursor.map(|(row, _)| row),
        read.cursor.map(|(_, col)| col),
        read.title.as_deref(),
    );
    reading.phase == aterm_phase::Phase::Idle
        && reading.phase_authoritative
        && reading.wall.is_none()
        && reading.prompt.is_none()
}

/// Whether `read` shows the turn over for an automatic restart's signal: none of the agent's own
/// work that no process row names pending on it ([`aterm_phase::phase::unlisted_work`]: the work
/// Claude Code runs in its own process, any other work its footer's pill counts; the process
/// table the signal re-reads names none of it), and, where the status lags, an idle reading with
/// no turn running ([`turn_shown`]: its busy footer, or its title running a reply the screen
/// does not show).
fn turn_over_shown(read: &Screen, lagging: bool) -> bool {
    aterm_phase::phase::unlisted_work(&read.rows).is_none()
        && (!lagging || (!turn_shown(read) && reads_idle(read)))
}

/// Whether `read` lets a press's signal go: no turn running ([`turn_shown`]) and no box open on
/// it. A click's signal reads it too (`super::relaunch`'s `at_rest_for`).
pub(super) fn press_lets_signal(read: &Screen) -> bool {
    !turn_shown(read) && aterm_phase::prompt::prompt_box_span(&read.rows).is_none()
}

/// The screen read when it shows no box and an empty composer, else `None`.
pub(super) fn composer_clear(c: &mut Client, tab: &str) -> Option<Screen> {
    screen(c, tab).filter(|scr| {
        aterm_phase::prompt::prompt_box_span(&scr.rows).is_none() && composer_empty(c, tab, scr)
    })
}

fn fence_verdict(scr: &Screen, dim: bool) -> Result<Option<String>, &'static str> {
    if aterm_phase::prompt::prompt_box_span(&scr.rows).is_some() {
        return Err("box");
    }
    if !upgrade::composer_is_empty(&scr.rows, scr.cursor, dim) {
        return Err("draft");
    }
    Ok(scr.generation.clone())
}

fn dim_cell(scr: &Screen) -> Option<usize> {
    upgrade::composer_dim_row(&scr.rows, scr.cursor).and_then(|caret| scr.first.checked_add(caret))
}

fn same_composer(a: &Screen, b: &Screen) -> bool {
    fn caret(s: &Screen) -> Option<&str> {
        aterm_phase::phase::composer_draft(&s.rows)
            .and_then(|(caret, _)| s.rows.get(caret))
            .map(String::as_str)
    }
    a.cursor == b.cursor && a.first == b.first && caret(a) == caret(b)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn type_fenced(
    c: &mut Client,
    tab: &str,
    text: &str,
    retry: Option<&Retry>,
    put: Put,
    fence_every: Duration,
    lane: Lane,
) -> Result<(), Typed> {
    type_fenced_then(c, tab, text, retry, put, fence_every, &mut || Ok(()), lane)
}

/// A private built-in semantic input operation. Its exact duty/revision is pinned across
/// screen checks and retries; every actual `turn` remains one server-side logical batch, named
/// as `lane`'s write ([`lane_words`]).
pub(super) fn type_fenced_for_duty(
    c: &mut Client,
    tab: &str,
    text: &str,
    duty: super::contract::Scope,
    put: Put,
    fence_every: Duration,
    lane: Lane,
) -> Result<(), Typed> {
    if !matches!(
        duty,
        super::contract::Scope::TurnEnd
            | super::contract::Scope::Approvals
            | super::contract::Scope::Questions
            | super::contract::Scope::Walls
    ) {
        return Err(Typed::Refused("non-typing duty".to_string()));
    }
    let (head, body) = c
        .request_counted(&format!("@{tab} status"))
        .map_err(|e| Typed::of_io(&e))?;
    let observed = crate::supervise::BuiltinAuthority::from_status(&(head + &body), &hand_name());
    let Some(revision) = observed.revision().filter(|_| observed.keeps(duty)) else {
        return Err(Typed::Refused("supervisor input authority".to_string()));
    };
    type_fenced_then_for_duty(
        c,
        tab,
        text,
        None,
        put,
        fence_every,
        &mut || Ok(()),
        Some((duty, revision)),
        lane,
    )
}

/// [`type_fenced`], calling `before_keys` once the fence is read and right before each try's
/// keys are sent: an error from it types nothing and is the result. Every refusal before it
/// typed nothing; an error after it may follow keys the server took.
#[allow(clippy::too_many_arguments)]
pub(super) fn type_fenced_then(
    c: &mut Client,
    tab: &str,
    text: &str,
    retry: Option<&Retry>,
    put: Put,
    fence_every: Duration,
    before_keys: &mut dyn FnMut() -> Result<(), Typed>,
    lane: Lane,
) -> Result<(), Typed> {
    type_fenced_then_for_duty(
        c,
        tab,
        text,
        retry,
        put,
        fence_every,
        before_keys,
        None,
        lane,
    )
}

#[allow(clippy::too_many_arguments)]
fn type_fenced_then_for_duty(
    c: &mut Client,
    tab: &str,
    text: &str,
    retry: Option<&Retry>,
    put: Put,
    fence_every: Duration,
    before_keys: &mut dyn FnMut() -> Result<(), Typed>,
    authority: Option<(super::contract::Scope, u64)>,
    lane: Lane,
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
                    std::thread::sleep(fence_every);
                    continue;
                }
                Err(why) => return Err(Typed::Refused(why.to_string())),
            };
            if tries > 1 && !retry.is_some_and(|retry| retry_open(&read, retry)) {
                return Err(Typed::Changed);
            }
            if put == Put::Pressed && aterm_phase::survey::survey_block(&read.rows).is_some() {
                return Err(Typed::Refused("survey".to_string()));
            }
            before_keys()?;
            let typed = checked_turn_for_duty(
                c,
                tab,
                text,
                generation.as_deref(),
                read.human_seq,
                put,
                authority,
                lane,
            );
            match typed {
                Ok(()) => return Ok(()),
                Err(Typed::Changed) if again => std::thread::sleep(fence_every),
                Err(e) => {
                    if let (Typed::Cut(_), Some(fenced)) = (&e, read.human_seq) {
                        // Still under the hand: what the stopped turn typed is taken back
                        // while its own fence proves every glyph of it the turn's.
                        let proof = Unkeyed::Since(fenced);
                        let _ = take_back(c, tab, text, proof, authority, fence_every, lane);
                    }
                    return Err(e);
                }
            }
        }
    })();
    hand.give_back(c);
    out
}

struct Restarting<'a> {
    argv: &'a [String],
    target: &'a Candidate,
    host_claim: Option<&'a HostClaim>,
    grace: u32,
    fresh: bool,
    quiet: bool,
    /// The owner's press moves the tab ([`upgrade::Press::Acting`]): the signal waits only for
    /// what [`upgrade::press_gate`] waits for, read again on the screen before it is sent.
    pressed: bool,
    /// The quiet round moves over the agent's own background work ([`upgrade::own_yields`]):
    /// the signal goes with that work running, over waits only where the last look still reads
    /// nothing but the waits the continuation carries.
    over: Option<upgrade::Yield>,
    lagging: bool,
    mode: Option<&'a str>,
    login: Option<&'a str>,
    home: Option<&'a str>,
    route: super::accounts::AuthRoute,
    auth_observed: bool,
    /// What the agent's own exec environment records of its shell's
    /// ([`super::accounts::LaunchEnv`]).
    launch_env: Option<&'a super::accounts::LaunchEnv>,
}

fn status_lets_signal(a: &SessionFile, looked: &SessionFile, lagging: bool) -> bool {
    a.status == "idle" || (lagging && status_unmoved(a, looked))
}

/// Whether the session file `a` holds the status `looked` held, not written since.
fn status_unmoved(a: &SessionFile, looked: &SessionFile) -> bool {
    a.status == looked.status && a.status_updated_at_ms == looked.status_updated_at_ms
}

#[allow(clippy::too_many_arguments)]
fn restart(
    opts: &Opts,
    r: Report,
    st: &mut St,
    c: &mut Client,
    tab: &str,
    sf: &SessionFile,
    t: &[ProcRow],
    k: &dyn Kernel,
    how: &Restarting<'_>,
) -> Report {
    if st.native_retired_unissued {
        return said(r, super::native_restart::WAIT_RETIRED_UNISSUED);
    }
    let Restarting {
        argv,
        target,
        host_claim,
        grace,
        fresh,
        quiet,
        pressed,
        over,
        lagging,
        mode: shown_mode,
        login,
        home,
        route,
        auth_observed,
        launch_env,
    } = *how;
    let records = Records::of_file(&opts.home, sf);
    let fresh = fresh || records.transcript_exists(&sf.session_id) == Some(false);
    let fresh_tasked = if fresh {
        let ours = match conversation_typed(opts, tab, Some(&sf.session_id), &[]) {
            Ok(ours) => ours,
            Err(_) => return said(r, super::relaunch::WAIT_LEDGER_EVIDENCE),
        };
        records.tasked(&sf.session_id, &ours)
    } else {
        None
    };
    // A press restarts over the agent's own background work and a status that has not caught
    // up with its turn's end; what it waits for, it reads off the screen.
    // A move over the agent's own background work goes with its status still `shell` or `busy`
    // for that work, so it reads the turn's end off the screen, and over waits requires that the
    // screen name no work of its own. Its status must be idle or the very one the step looked
    // at, unwritten since: a turn that started after that look (a wait ended and its notice
    // reached the model) rewrites it before its footer is drawn, and nobody consented to ending
    // that turn, as a person's press does.
    let status_holds = |a: &SessionFile| {
        pressed || status_lets_signal(a, sf, lagging) || (over.is_some() && status_unmoved(a, sf))
    };
    let turn_over = |read: &Screen| match over {
        _ if pressed => press_lets_signal(read),
        Some(upgrade::Yield::Waits) => {
            press_lets_signal(read) && aterm_phase::phase::unlisted_work(&read.rows).is_none()
        }
        Some(upgrade::Yield::Deadline | upgrade::Yield::Outlasted) => press_lets_signal(read),
        None => turn_over_shown(read, lagging),
    };
    // What runs under the agent at the last look: nothing, unless a press or the deadline moves
    // over it, or only waits whose commands the continuation carries.
    let own_lets_signal = |st: &St| match over {
        _ if pressed => true,
        Some(upgrade::Yield::Deadline | upgrade::Yield::Outlasted) => true,
        Some(upgrade::Yield::Waits) => {
            k.restart_held(sf.pid)
                .is_some_and(|held| match upgrade::own_of(&held, false, false) {
                    upgrade::Own::None => true,
                    upgrade::Own::Waits(now) => upgrade::waits_carried(&now, &st.ended_waits),
                    upgrade::Own::Work => false,
                })
        }
        None => k.restart_background(sf.pid).is_empty(),
    };
    let ours = |st: &St, a: &SessionFile| {
        if fresh {
            fresh_tasked == Some(false)
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
    let job = k.job(sf.pid);
    if let Some(job) = not_a_shell_job(job) {
        return not_a_job(opts, r, st, job);
    }
    let proven = match foreground_shell(job) {
        Ok(shell) => shell,
        Err(why) => return said(r, format!("wait:{why}")),
    };
    let mode = resume_mode(
        &records,
        &sf.session_id,
        shown_mode.map(str::to_owned),
        super::unix_time::parse_lstart(&sf.proc_start),
    );
    let launch = Launch {
        launch_env,
        ..launch_of(
            sf,
            argv,
            target,
            mode.as_deref(),
            !fresh,
            home,
            login,
            route,
            auth_observed,
        )
    };
    let planned = home_carried(&opts.home, home, &sf.cwd, &records).and_then(|homedir| {
        let launch = Launch {
            homedir: homedir.as_deref(),
            ..launch
        };
        if super::native_restart::coordinated(st) {
            super::relaunch::plan_native_upgrade(opts, &launch, proven, t)
        } else {
            plan(opts, &launch, proven, t)
        }
    });
    let Plan {
        shell,
        line,
        tap_dropped,
        token_dropped,
        line_dir,
        command,
        ..
    } = match planned {
        Ok(p) => p,
        Err(no) => return unplanned(opts, r, st, no),
    };
    let back_plan = k
        .image(sf.pid)
        .and_then(|image| launched(argv, Some(&image), &native_root(&opts.home), true))
        .map(|(program, _)| program)
        .filter(|program| *program != target.exe)
        .and_then(|program| {
            // The rollback execs the replaced process's own image, which a native restart holds it
            // to: it is typed on the row's stored login, never through the token wrapper.
            let back = Launch {
                exe: &program,
                serve: super::relaunch::Serve::Never,
                ..launch
            };
            home_carried(&opts.home, home, &sf.cwd, &records)
                .and_then(|homedir| {
                    let back = Launch {
                        homedir: homedir.as_deref(),
                        ..back
                    };
                    plan(opts, &back, proven, t)
                })
                .ok()
        })
        .filter(|back| back.shell == shell)
        .map(|back| (back.line, back.command));
    let (back_line, back_command) = back_plan.unwrap_or_default();
    let Some(mut hand) = Hand::take(c, tab) else {
        return said(r, "wait:held");
    };
    let aborted: Option<Report> = 'signal: {
        let again = records.session_file(sf.pid);
        let scr = screen(c, tab);
        let still = again.is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(&a)
                && ours(st, &a)
                && k.process_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        }) && foreground_shell(k.job(sf.pid)) == Ok(shell)
            && scr.is_some_and(|s| turn_over(&s) && composer_empty(c, tab, &s))
            && own_lets_signal(st)
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
        let back = st.signalled(sf.pid, &sf.proc_start, shell, tab, line, now_s());
        st.back_line.clone_from(&back_line);
        st.account_intent = command
            .as_ref()
            .and_then(|command| command.account_intent.clone());
        st.back_account_intent = back_command
            .as_ref()
            .and_then(|command| command.account_intent.clone());
        st.line_dir = line_dir.unwrap_or_default();
        st.line_grace_s = Some(grace);
        st.login = login.map(str::to_owned);
        if fresh {
            super::relaunch::CAUSE_UPGRADE_FRESH.clone_into(&mut st.cause);
        }
        if save_checked(opts, &sf.session_id, st).is_err() {
            st.unsent(back);
            st.cause.clear();
            break 'signal Some(said(r.clone(), "wait:claim-unwritten"));
        }
        let current = records.session_file(sf.pid);
        let owns_notice = current.as_ref().is_some_and(|a| {
            a.session_id == sf.session_id
                && status_holds(a)
                && ours(st, a)
                && k.process_start(sf.pid).as_deref() == Some(squash(&a.proc_start).as_str())
        });
        let owner = k.terminal(sf.pid);
        let owner = owner.as_ref().map(|(pid, name)| (*pid, name.as_str()));
        if !owns_notice
            || require_unique_owner(&opts.home, sf).is_err()
            || foreground_shell(k.job(sf.pid)) != Ok(shell)
            || !owner.is_some_and(owned_by_aterm)
            || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
            || !typing_fence_read(c, tab).is_ok_and(|(_, read)| turn_over(&read))
            || held(c, tab, grace)
        {
            st.unsent(back);
            st.cause.clear();
            save(opts, &sf.session_id, st);
            break 'signal Some(said(r.clone(), "wait:changed-before-signal"));
        }
        let admission = super::native_restart::begin(
            opts,
            c,
            st,
            &sf.session_id,
            super::contract::Scope::Upgrade,
            command
                .as_ref()
                .map_or(target.exe.as_path(), |command| command.target.as_path()),
            command.as_ref(),
            back_command.as_ref(),
        );
        // Every admission names its arm: only an admitted or legacy one reaches the signal step,
        // whose legacy arm ends the agent with no native binding.
        match admission {
            Ok(
                super::native_restart::Recovery::Admitted | super::native_restart::Recovery::Legacy,
            ) => {}
            Ok(super::native_restart::Recovery::Completed) => {
                break 'signal Some(said(r.clone(), "wait:native-completed"));
            }
            Ok(super::native_restart::Recovery::Retired) => {
                break 'signal Some(said(r.clone(), "failed:native-cohort-retired"));
            }
            Ok(super::native_restart::Recovery::Held) => {
                break 'signal Some(said(
                    r.clone(),
                    super::native_restart::WAIT_RETIRED_UNISSUED,
                ));
            }
            Ok(super::native_restart::Recovery::Lapsed) => {
                break 'signal Some(super::native_restart::end_lapsed(
                    opts,
                    &r,
                    st,
                    &sf.session_id,
                ));
            }
            Ok(
                super::native_restart::Recovery::Cancelled
                | super::native_restart::Recovery::Unadmitted,
            )
            | Err(_) => {
                let why = super::native_restart::refusal_word(admission);
                // The exact plan/id was saved before dispatch. Preserve it on an unknown ACK;
                // only a proved pre-mutation abort, or the host's proof that it never admitted
                // the plan, permits a later fresh constructor.
                let ended = st.native_prepare.is_empty()
                    || match super::native_restart::abort(opts, &sf.session_id, c, tab, st) {
                        Some(super::native_restart::Recovery::Lapsed) => {
                            break 'signal Some(super::native_restart::end_lapsed(
                                opts,
                                &r,
                                st,
                                &sf.session_id,
                            ));
                        }
                        ended => ended.is_some(),
                    };
                if ended {
                    st.unsent(back);
                }
                save(opts, &sf.session_id, st);
                if st.native_retired_unissued {
                    break 'signal Some(said(
                        r.clone(),
                        super::native_restart::WAIT_RETIRED_UNISSUED,
                    ));
                }
                break 'signal Some(said(r.clone(), format!("wait:{why}")));
            }
        }
        if st.native.is_some()
            && (!typing_fence_read(c, tab).is_ok_and(|(_, read)| turn_over(&read))
                || foreground_shell(k.job(sf.pid)) != Ok(shell)
                || held(c, tab, grace)
                || host_claim.is_some_and(|claim| !claim_still_live(c, sf.pid, claim))
                || require_unique_owner(&opts.home, sf).is_err())
        {
            match super::native_restart::abort(opts, &sf.session_id, c, tab, st) {
                Some(super::native_restart::Recovery::Lapsed) => {
                    break 'signal Some(super::native_restart::end_lapsed(
                        opts,
                        &r,
                        st,
                        &sf.session_id,
                    ));
                }
                Some(_) => st.unsent(back),
                None => {}
            }
            save(opts, &sf.session_id, st);
            if st.native_retired_unissued {
                break 'signal Some(said(
                    r.clone(),
                    super::native_restart::WAIT_RETIRED_UNISSUED,
                ));
            }
            break 'signal Some(said(r.clone(), "wait:changed-after-native-admission"));
        }
        // The raw relaunch admits its account at the shell only once the agent is gone: a route
        // it would refuse there waits here, before the signal, as the restart in place's does
        // (`relaunch::relaunch_account_admits`), so an upgrade never ends an agent it cannot
        // start again.
        if st.native.is_none()
            && let Err(why) = super::relaunch::relaunch_account_admits(opts, st, shell)
        {
            aterm_log::info!(
                "harness @{tab}: the upgrade waits before its signal: its relaunch's account \
                 would wait: {why}"
            );
            st.unsent(back);
            st.cause.clear();
            save(opts, &sf.session_id, st);
            break 'signal Some(said(r.clone(), super::relaunch::WAIT_ACCOUNT));
        }
        let signal = if let Some(binding) = st.native.as_ref() {
            super::native_restart::signal(c, tab, binding)
        } else {
            terminate(c, tab, pid, grace)
        };
        match signal {
            Terminated::Sent => None,
            Terminated::Refused(why) => {
                if st.native.is_some() {
                    save(opts, &sf.session_id, st);
                    break 'signal Some(said(r.clone(), format!("wait:signal-{why}")));
                }
                st.unsent(back);
                st.cause.clear();
                save(opts, &sf.session_id, st);
                Some(said(r.clone(), format!("wait:signal-{why}")))
            }
            Terminated::Failed => {
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
        } else if pressed && st.quiet {
            "SIGTERM (the owner pressed the update arrow: resumed as it was, nothing typed but \
             the relaunch line; any background shell ends with it)"
        } else if pressed {
            "SIGTERM (the owner pressed the update arrow: any background shell ends with it)"
        } else if over == Some(upgrade::Yield::Waits) {
            "SIGTERM (only background waits ran under it: they end with it, and the continuation \
             carries each one's command to start again)"
        } else if over == Some(upgrade::Yield::Deadline) {
            "SIGTERM (the deadline it was told has passed: what still ran under it ends with it, \
             and the continuation names it)"
        } else if over == Some(upgrade::Yield::Outlasted) {
            "SIGTERM (its task finished, so no line was typed: its own background work held the \
             upgrade past the bound a told deadline would have; what still ran under it ends with \
             it, and the continuation names it)"
        } else if quiet {
            "SIGTERM (idle: nothing asked or typed; resumed as it was)"
        } else {
            "SIGTERM"
        },
    );
    super::relaunch::note_dropped(opts, &r, tap_dropped.as_deref(), token_dropped.as_deref());
    relaunch(opts, r, st, c, &sf.session_id, k)
}

fn confirmations(opts: &Opts) -> Vec<Report> {
    if opts.dry_run {
        return Vec::new();
    }
    let Ok(dir) = read_upgrade_dir(&opts.state) else {
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
        let Some(mut st) = load_record(opts, &session)
            .ok()
            .flatten()
            .filter(St::confirming)
        else {
            continue;
        };
        if opts.only_sid.as_ref().is_some_and(|s| *s != st.tab) {
            continue;
        }
        let r = look_report(&st, &session);
        out.extend(confirm(opts, &r, &mut st, &session, now));
    }
    out
}

/// Tab `opts.only_sid`'s looks past their clock, closed where no upgrade step runs to close them
/// (`[harness] upgrade` off, whose sweep never comes): its idle point's own close. It only
/// records, as a break's sweep does ([`super::relaunch::look_types`]): a typed continuation's
/// model is read into its `done` row, and a warned look's line is never typed, the look recorded
/// lost once the bound it holds its conversation for has passed. Nothing is typed and nothing
/// restarts, whatever the tab's Autopilot. `None` where nothing closed (a look still inside its
/// clock, no look); `busy:<why>` where a look stands and the sweep lock was refused (another
/// sweep at work), which its host looks at again later, as at any step the lock refuses: with
/// the switch off no other step of its comes back to the look; else the close's report,
/// `done:lost` first, which its host records where a person reads it.
#[must_use]
pub fn close_looks(opts: &Opts) -> Option<Report> {
    let tab = opts.only_sid.as_ref()?;
    if !confirming(opts) {
        return None;
    }
    let recording = Opts {
        background: true,
        press: None,
        ..opts.clone()
    };
    let _held = match sweep_lock(&recording) {
        Ok(lock) => lock,
        Err(why) => {
            return Some(Report {
                rung: None,
                pid: 0,
                tab: tab.clone(),
                session: "-".to_string(),
                from: "-".to_string(),
                to: "-".to_string(),
                step: format!("busy:{why}"),
            });
        }
    };
    let mut closed = confirmations(&recording);
    let lost = closed.iter().position(|r| r.step == "done:lost");
    lost.map(|at| closed.swap_remove(at))
        .or_else(|| closed.into_iter().next())
}

/// Why a release is owed to an agent whose announced round `[harness] upgrade` was switched off
/// under ([`release_notice`]): `release=switched-off`, `released:switched-off`.
pub const SWITCHED_OFF: &str = "switched-off";

/// Whether a notice the upgrade typed into tab `opts.only_sid`'s agent still stands in one of
/// the tab's records ([`St::notice_stands`]): what [`release_notice`] gives back. It reads the
/// records alone and asks no socket, so a host may ask it where no idle point is yet owed (a
/// worker that starts with `[harness] upgrade` off, which no step parks for its next idle point).
#[must_use]
pub fn notice_stands(opts: &Opts) -> bool {
    let Some(tab) = opts.only_sid.as_deref() else {
        return false;
    };
    read_upgrade_dir(&opts.state).is_ok_and(|dir| {
        dir.flatten().any(|e| {
            let path = e.path();
            path.extension().is_some_and(|x| x == "json")
                && path
                    .file_stem()
                    .is_some_and(|session| notice_stands_in(opts, tab, &session.to_string_lossy()))
        })
    })
}

/// Whether session `session`'s record is tab `tab`'s and a notice of its stands.
fn notice_stands_in(opts: &Opts, tab: &str, session: &str) -> bool {
    load_record(opts, session)
        .ok()
        .flatten()
        .is_some_and(|st| st.tab == tab && st.notice_stands())
}

/// What a notice already typed into tab `opts.only_sid`'s agent is owed where no upgrade step
/// runs to pay it (`[harness] upgrade` off): the release line. The notice told the agent to stop
/// its watch loops, start no long-running work and wait for a restart; with the switch off no
/// step makes that restart, hears its READY or gives the round up, so its idle point gives the
/// conversation back here. An announced round is taken back to pending, its release owed
/// ([`SWITCHED_OFF`]), and a release already owed (a round that gave up, was voided or stopped)
/// is typed as it stands, each under every gate a release is typed under at an upgrade's step
/// ([`release_visit`]: an idle, settled tab, no person, box or draft, the process the notice
/// reached). It begins nothing: no notice, no tell, no restart, and a round that asked its agent
/// nothing (pending, or stopped with no release owed) is left as it lay, with no socket asked.
/// `None` where nothing is owed; `released:<why>` where the line was typed; `wait:release:<why>`
/// or `busy:<why>` where it stands owed, which its host looks at again later: with the switch
/// off no other step comes back to it.
#[must_use]
pub fn release_notice(opts: &Opts) -> Option<Report> {
    let tab = opts.only_sid.as_deref()?;
    let stands = |session: &str| notice_stands_in(opts, tab, session);
    if opts.dry_run || !notice_stands(opts) || !lane_on(opts, upgrade::Agent::Claude) {
        return None;
    }
    let owed = |why: &str| Report {
        pid: 0,
        tab: tab.to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: why.to_string(),
        rung: None,
    };
    let Ok(mut c) = connect(opts, tab) else {
        return Some(owed("wait:release:no-socket"));
    };
    if status_line(&mut c, tab).is_some_and(|l| self_claimed_by_status(&l)) {
        return None;
    }
    let Some(tabs) = host_roster(&mut c).filter(|tabs| tab_is_live(tabs, tab)) else {
        return Some(owed("wait:release:tab-not-live"));
    };
    drop(c);
    let _held = match sweep_lock(opts) {
        Ok(lock) => lock,
        Err(why) => return Some(owed(&format!("busy:{why}"))),
    };
    let homes = Homes::read(
        opts,
        Some(&tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let Some(files) = homes.roster(claude_records(&opts.home)) else {
        return Some(owed(&format!("wait:release:{UNREAD_FILES}")));
    };
    let candidates = host_candidates(
        &files,
        &tabs,
        |sf, its| its == tab && stands(&sf.session_id),
        atpkg::caller_shell::process_args,
        process_group,
        ids,
    );
    let mut t = None;
    let mut reports = Vec::new();
    for candidate in candidates {
        match candidate {
            Ok((sf, _, claim)) => {
                let t = t.get_or_insert_with(table);
                reports.extend(release_off(
                    opts,
                    sf,
                    Some(&files),
                    t,
                    &Live,
                    &claim.tab,
                    Some(&claim),
                ));
            }
            Err(wait) => reports.push(said(wait, format!("wait:release:{UNREAD_PROCESS}"))),
        }
    }
    let act = reports.iter().position(Report::is_act);
    act.map(|at| reports.swap_remove(at))
        .or_else(|| reports.into_iter().next())
}

/// [`release_notice`]'s visit of session `sf`, the agent tab `tab` runs (`host_claim`, the tab's
/// claim on its process, where the host's roster made it): the release its record's notice is
/// owed, typed or left owed ([`release_visit`]). `None`, the
/// record as it lay, where no notice of its stands ([`St::notice_stands`]) or the notice an
/// announced round typed reached another process than this one: nothing was asked of this one,
/// and the step a switch turned back on takes reopens that round.
fn release_off(
    opts: &Opts,
    sf: &SessionFile,
    files: Option<&[SessionFile]>,
    t: &[ProcRow],
    k: &dyn Kernel,
    tab: &str,
    host_claim: Option<&HostClaim>,
) -> Option<Report> {
    let r = Report {
        tab: tab.to_string(),
        ..blank(sf)
    };
    if !alive(sf.pid) || kernel_start(sf.pid).as_deref() != Some(squash(&sf.proc_start).as_str()) {
        return None;
    }
    let mut st = load_record(opts, &sf.session_id)
        .ok()
        .flatten()
        .filter(|st| st.tab == tab && st.notice_stands())?;
    let r = Report {
        to: format!("{}({})", st.to, st.source),
        ..r
    };
    if let Err(why) =
        preliminary_unique_owner(files, sf).and_then(|()| require_unique_owner(&opts.home, sf))
    {
        return Some(said(r, format!("wait:release:{why}")));
    }
    if st.release.is_empty() {
        if !st.notice_belongs_to(sf, tab) {
            return None;
        }
        if !st.release_switched_off(now_s()) {
            return None;
        }
        if save_checked(opts, &sf.session_id, &st).is_err() {
            return Some(said(r, "wait:release:unsaved"));
        }
    }
    Some(release_visit(opts, r, sf, st, tab, t, k, host_claim))
}

/// The report a look's close is said under: its own conversation and resumed agent.
fn look_report(st: &St, session: &str) -> Report {
    Report {
        rung: None,
        pid: st.resumed_pid,
        tab: st.tab.clone(),
        session: session.to_string(),
        from: st.from.clone(),
        to: format!("{}({})", st.resumed_on, st.source),
        step: String::new(),
    }
}

#[cfg(all(test, unix))]
#[path = "upgrade_drive_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_constructor_state_tests.rs"]
mod native_constructor_state_tests;

#[cfg(all(test, unix))]
#[path = "upgrade_drive_hang_tests.rs"]
mod hang_tests;

#[cfg(test)]
#[path = "upgrade_snapshot_env_tests.rs"]
mod snapshot_env_tests;

#[cfg(all(test, windows))]
#[path = "upgrade_drive_kernel_start_win_tests.rs"]
mod kernel_start_win_tests;

#[cfg(all(test, windows))]
#[path = "upgrade_drive_table_win_tests.rs"]
mod table_win_tests;
