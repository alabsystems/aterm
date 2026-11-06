// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps of `SourceEpisode`, `OwnerTell` and `WorkflowStep`
//! (`aterm_models_harness::{source_episode_model, owner_tell_model, workflow_step_model}`) that no
//! function here performs yet. The code's own bind where it runs: a read on
//! [`super::source_episode::SourceBook::look`] (`Look`, the close of a move undone in that same
//! read), the restart on [`super::source_episode::SourceBook::restore`] (`Up`), the tell's consent
//! on [`super::owner_rules::RuleState::of`] (`Tell`) and the yes's write on
//! [`super::owner_rules::Snapshot::still_current`] (`ConfirmWrite`), each projected by the Tier-1
//! bind (`tests/conformance_sources.rs`). Proof and test builds only.

/// The world, the board's steps not built yet and the mutants of `SourceEpisode`: the source's
/// moves, the arming a confirmed rule gives, aterm going down, and each `Buggy` member.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "Move",
        reason = "The source moving on (a ref's tip, the tree entry under its path, an index's pin): the world's, allowed while aterm is down (the gap). The Tier-1 bind moves the value the next read returns exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "MoveBack",
        reason = "The source moving the other way (a force-push back, a revert): the world's. The Tier-1 bind moves the value the next read returns exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "Arm",
        reason = "A rule naming the source confirmed live or dry-run, the owner's mode not off: the board's arming from the GUI's confirmed record (SourceBoard::adopt, the slice-3 plan's 3.3, its writer 3.5a). The Tier-1 bind reads only an armed source."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "Disarm",
        reason = "The last rule naming the source dropped, edited unconfirmed or the owner's mode off: the board's disarming (3.3), which cuts a read in flight. The Tier-1 bind reads nothing while disarmed."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "Down",
        reason = "aterm quitting or crashing: the environment's. The board wrote sources.json after the last read that changed the book (one atomic write, 3.3), so nothing between a read and its save is lost; the Tier-1 bind keeps that file for Up."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "QuietRestarts",
        reason = "Buggy=1 liveness mutant only; a read with no edge that restarts the quiet count, which SourceBook::look never does (the Tier-1 bind plants Fault::QuietRestarts)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "CloseOnFirstQuiet",
        reason = "Buggy=2 negative control only; an edge that supersedes an episode at its first quiet read, which SourceBook::dwelt refuses (the Tier-1 bind plants Fault::DwellCloseEarly)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "RebaseOnRestart",
        reason = "Buggy=3 negative control only; a restart within the day that re-bases the book, which SourceBook::restore never does (the Tier-1 bind plants Fault::RebaseOnRestart)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "ReadUnconfirmed",
        reason = "Buggy=4 negative control only; a read of a source no confirmed rule names, which the board's arming refuses (3.3's ReadUnconfirmed fault plants it there)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SourceEpisode",
        action = "UndoAtFirstBase",
        reason = "Buggy=5 negative control only; a move undone that closes its episode before the dwell, which SourceBook::look never does (its close waits for SourceBook::dwelt)"
    )
)]
#[doc(hidden)]
pub fn source_episode_environment() {}

/// The world, the loop's and the GUI's steps not built yet and the mutants of `OwnerTell`: the
/// episodes the board opens and closes, the owner's edit, a person's press, the guards and the
/// pace other models own, the hour, the decider's would row and lapse, and each `Buggy` member.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "Open",
        reason = "An episode opening on the source the rule names: SourceEpisode's edge, and whether it opened after the session's hosted agent did (fresh, judged by the GUI from the agent's kernel birth, the slice-3 plan's 3.4). The Tier-1 bind opens the rule's due line exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "Close",
        reason = "The episode closing (undone for the dwell, or superseded): SourceEpisode's. Its due line stays until a look lapses it. The Tier-1 bind closes it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "EditTable",
        reason = "The owner's aterm.toml edited (a field, a source, mode, or the resolved URL): a new rev. The Tier-1 bind swaps the table in force exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "Press",
        reason = "A person's press of Settings > Harness > Owner rules > Confirm, which snapshots the rule's name, whole rev and rendering (Snapshot::of; the press's provenance fence and the alert are the slice-3 plan's 3.5a). The Tier-1 bind takes the snapshot exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "GuardOn",
        reason = "A guard holding at the break (a person's hand, a brake, an untaken act, the upgrade's notice): OccupancyDoor's. The Tier-1 bind sets that guard exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "GuardOff",
        reason = "That guard lifted: OccupancyDoor's. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "PaceOn",
        reason = "The session's 20-minute pace running since its last line: OccupancyDoor's. The Tier-1 bind sets it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "PaceOff",
        reason = "That pace run out: OccupancyDoor's. The Tier-1 bind clears it exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "HourPasses",
        reason = "An hour passing since the rule's first urgent tell: the clock's. The GUI counts urgent tells by the whole rev and persists the count in sources.json (the slice-3 plan's 3.5b). The Tier-1 bind empties the count exactly there."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "Would",
        reason = "A confirmed rule's episode booked as a would row under observe or for a dry-run rule: owner::decide's, not built yet (the slice-3 plan's 3.4). The Tier-1 bind reads the consent it needs on RuleState::of at every state."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "Lapse",
        reason = "A due line dropped untold (its episode closed, older than the session's agent, or its rev no longer confirmed): owner::decide's, not built yet (3.4/3.5b). The Tier-1 bind reads the consent it needs on RuleState::of at every state."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        reason = "The persisted urgent count is Tier-1 only: an urgent count that restarts with the board (UrgentCountResets) is a GUI fault of the slice-3 plan's 3.5b, since the restore is GUI code a model of the tell need not carry. A Land owner line behind the upgrade's notice is a projection, not a row: an owner line is OccupancyDoor's due value 1 under Cruise, and OccupancyDoor (near the interpreter's MAX_STATES) gains no variable for it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "TellKeepsDue",
        reason = "Buggy=1 negative control only; a tell that keeps its episode due, which owner::decide's book of told episodes refuses (the slice-3 plan's 3.5b plants TellTwiceSource)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "ConfirmByName",
        reason = "Buggy=2 negative control only; a tell under any yes on record, which RuleState::of refuses (the Tier-1 bind plants Fault::ConfirmByName)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "ConfirmStaleSnapshot",
        reason = "Buggy=3 negative control only; a yes written for a snapshot the table in force no longer hashes to, which Snapshot::still_current refuses (the Tier-1 bind plants Fault::ConfirmStaleSnapshot)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "TypeUnderDryRun",
        reason = "Buggy=4 negative control only; a dry-run rule typed, which RuleState::of's DryRun refuses at the decider (3.4)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "TypeUnderObserve",
        reason = "Buggy=5 negative control only; a tell with no PromptGrant, which the loop's door and the GUI's prompt_now refuse (3.5b's T-obs3)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "UrgentUncapped",
        reason = "Buggy=6 negative control only; an urgent tell past its cap an hour, which the GUI's urgent count refuses (3.5b plants UrgentUncapped)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "UrgentPassesGuard",
        reason = "Buggy=7 negative control only; an urgent tell that passes a guard, which urgent never does: it passes the pace alone (3.5b plants UrgentPassesGuard)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "TellUndone",
        reason = "Buggy=8 negative control only; a closed episode's due line told, which the GUI's source_claim refuses (episode open and holding, 3.5b)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "OwnerTell",
        action = "TellOlder",
        reason = "Buggy=9 negative control only; an episode older than the session's hosted agent told, which the GUI's fresh bit refuses (3.4 plants TellOlder)"
    )
)]
#[doc(hidden)]
pub fn owner_tell_environment() {}

/// The steps of `WorkflowStep`, none built yet: its reader, its decider and its Tier-1 bind are
/// the slice-3 plan's 3.6b (`RuleId::WorkflowStep` on the existing write path), from the
/// journal fixtures 3.6a captures.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "ReturnA",
        reason = "A track's step returning: Claude Code's, read off its workflow journal (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "ReturnB",
        reason = "The other track's step returning: Claude Code's, read off its workflow journal (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "OpenA",
        reason = "A returned step's line opened while the other track runs, only under a captured format: the journal reader's (workflow_journal's FORMATS, 3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "OpenB",
        reason = "The other track's returned step opened: the journal reader's (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "TellA",
        reason = "The step's one line under prompt, while the other track runs: decide's RuleId::WorkflowStep on the existing occupancy_write path (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "TellB",
        reason = "The other track's line: decide's RuleId::WorkflowStep (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "WouldA",
        reason = "The step booked a would row under observe: decide's (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "WouldB",
        reason = "The other track's would row: decide's (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "LapseA",
        reason = "A step whose run ended before its line went, dropped untold: decide's (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "LapseB",
        reason = "The other track's step dropped untold: decide's (3.6b)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "TellKeepsDue",
        reason = "Buggy=1 negative control only; a step told twice (3.6b's book of told steps refuses it)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "TellAfterEnd",
        reason = "Buggy=2 negative control only; a step told after its run ended, which the GUI's occupancy_pace re-judges (a track still runs, 3.6b)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "ReadUncaptured",
        reason = "Buggy=3 negative control only; a step opened under a format no capture matched, which workflow_journal's reader_for refuses (3.6b plants ReadUncaptured)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "WorkflowStep",
        action = "TypeUnderObserve",
        reason = "Buggy=4 negative control only; a step typed with no PromptGrant, which occupancy_write refuses (3.6b's T-obs3)"
    )
)]
#[doc(hidden)]
pub fn workflow_step_environment() {}
