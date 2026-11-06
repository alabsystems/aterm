// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OWNER SOURCES AND RULES (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.1 "Episodes", §3.3,
//! §3.5; the slice-3 plan's §6), in three models, so none carries another's variables.
//!
//! The board (`SourceEpisode`): one value source, its book and the world's moves. A read
//! (`Look`) whose value differs from the last read's is an edge: with no episode open it opens
//! one, its base the last read's value; inside the dwell it joins the open one; past the dwell it
//! supersedes it, the new one's base the old one's latest. A read with no edge counts a quiet
//! read, up to the dwell (`Dwell` quiet reads: the code's `DWELL_READS`, the last of them a dwell
//! after the edge), and the read that holds a move undone at its base for the dwell closes the
//! episode, never told (in that same read, as `SourceBook::look` does). `seen = 3` is a book with
//! no base: the next read is the base (`Step::Based`). Only an armed source is read (a confirmed
//! rule names it).
//! The board writes sources.json after every read that changed the book, so a restart (`Down`,
//! the world moving while aterm is down, then `Up`, `SourceBook::restore`) keeps what was read: a
//! move made in the gap is an edge at the next read. With `Stale = 1` the restart is past a day
//! and re-bases the book.
//!
//! The consent and the tell (`OwnerTell`): one rule, one session, one episode stream. The owner's
//! table hashes to `rev` (0 or 1: an edit flips it); `crev` is the rev the owner's last yes
//! confirmed (2: none), `snap` the snapshot a person's press of Confirm took (2: none). A yes
//! writes the snapshot's rev only while the table in force still hashes to it. An episode is
//! told to the session once, only while it is open, only when it opened after the session's
//! hosted agent did (`fresh`; with `Older = 1` every episode is older), only under `prompt`, only
//! for a live rule whose current whole rev is confirmed, only where no guard holds, and within the
//! session's pace unless the rule is `urgent`, whose tells are capped (`Cap` an hour). Under
//! `observe` or `dry-run` it is booked a would row instead. A closed episode keeps its due line
//! until a look lapses it (so `TellUndone` is live). The ghosts record a tell each law refuses.
//!
//! The workflow step (`WorkflowStep`): one run of two tracks, one session. A track that returns
//! while the other still runs opens its step's line, only where the running Claude Code build's
//! journal format was captured; it is told once, only while the other track runs, only under
//! `prompt` (else a would row); a step whose run ended lapses. `due_*` is 0 before the step
//! returned, 1 due, 2 settled. Each committed open and tell writes its laws' ghosts from the state
//! it fires in, as its mutant does, so the laws judge both tracks' committed steps in every state
//! (the mutants drop a guard term on track A only). Its Tier-1 bind lands with the reader (the
//! slice-3 plan's 3.6b).

use aterm_spec::derive::{Liveness, Model};

/// THE BOARD: one source's book.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn source_episode_model() -> Model {
    aterm_spec::ty_model! {
        SourceEpisode {
            const Buggy = 0;
            const Dwell = 2;
            const Moves = 3;
            const Stale = 0;
            scenario Stale = 1;
            var v = 0;
            var seen = 3;
            var base = 0;
            var latest = 0;
            var open = 0;
            var quiet = 0;
            var moved = 0;
            var armed = 1;
            var down = 0;
            var early = 0;
            var resetq = 0;
            var lost = 0;
            var rogue = 0;
            var undone_early = 0;

            action Move when (Moves > moved) {
                v = if (v == 2) { 0 } else { v + 1 };
                moved = moved + 1;
            }
            action MoveBack when (Moves > moved) {
                v = if (v == 0) { 2 } else { v - 1 };
                moved = moved + 1;
            }
            action Arm when (armed == 0) { armed = 1; }
            action Disarm when (armed == 1) { armed = 0; }
            action Down when (down == 0) { down = 1; }

            action Look when (down == 0 && armed == 1) {
                open = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { open }
                } else {
                    if (seen == 3) { open } else { 1 }
                };
                base = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { base }
                } else {
                    if (seen == 3) {
                        base
                    } else {
                        if (open == 0) {
                            seen
                        } else {
                            if (quiet == Dwell) { latest } else { base }
                        }
                    }
                };
                latest = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { latest }
                } else {
                    if (seen == 3) { latest } else { v }
                };
                quiet = if (seen == v) {
                    if (Dwell > quiet) { quiet + 1 } else { quiet }
                } else {
                    0
                };
                seen = v;
            }
            action Up when (down == 1) {
                down = 0;
                seen = if (Stale == 1) { 3 } else { seen };
                open = if (Stale == 1) { 0 } else { open };
                base = if (Stale == 1) { 0 } else { base };
                latest = if (Stale == 1) { 0 } else { latest };
                quiet = if (Stale == 1) { 0 } else { quiet };
            }

            action QuietRestarts when (Buggy == 1 && down == 0 && armed == 1 && seen == v) {
                quiet = 0;
                resetq = 1;
            }
            action CloseOnFirstQuiet when (
                Buggy == 2 && down == 0 && armed == 1 && 3 > seen && (seen > v || v > seen)
                    && open == 1 && quiet > 0 && Dwell > quiet
            ) {
                base = latest;
                latest = v;
                quiet = 0;
                seen = v;
                early = 1;
            }
            action RebaseOnRestart when (Buggy == 3 && down == 1 && Stale == 0) {
                down = 0;
                seen = 3;
                open = 0;
                base = 0;
                latest = 0;
                quiet = 0;
                lost = if (3 > seen && (seen > v || v > seen)) { 1 } else { lost };
            }
            action ReadUnconfirmed when (Buggy == 4 && down == 0 && armed == 0) {
                open = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { open }
                } else {
                    if (seen == 3) { open } else { 1 }
                };
                base = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { base }
                } else {
                    if (seen == 3) {
                        base
                    } else {
                        if (open == 0) {
                            seen
                        } else {
                            if (quiet == Dwell) { latest } else { base }
                        }
                    }
                };
                latest = if (seen == v) {
                    if (open == 1 && base == v && Dwell <= quiet + 1) { 0 } else { latest }
                } else {
                    if (seen == 3) { latest } else { v }
                };
                quiet = if (seen == v) {
                    if (Dwell > quiet) { quiet + 1 } else { quiet }
                } else {
                    0
                };
                seen = v;
                rogue = 1;
            }
            action UndoAtFirstBase when (
                Buggy == 5 && down == 0 && armed == 1 && seen == v && open == 1 && base == v
                    && Dwell > quiet + 1
            ) {
                open = 0;
                base = 0;
                latest = 0;
                quiet = quiet + 1;
                undone_early = 1;
            }

            invariant AFlapInsideTheDwellIsOneEpisode: early == 0;
            invariant TheDwellCountsQuietReads: resetq == 0;
            invariant AMoveAcrossARestartIsSeen: lost == 0;
            invariant NoReadUnconfirmed: rogue == 0;
            invariant AnUndoNeedsTheDwell: undone_early == 0;
        }
    }
}

/// `TheDwellEnds`, `[]<>(quiet == Dwell || open == 0 || moved < Moves || armed == 0 ||
/// down == 1)`: once the world's moves are spent and an armed source goes on being read, every
/// open episode dwells out (or closes). Only the reads need to be fair: the goal holds while
/// aterm is down, so the restart's own fairness assumes nothing.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(source_episode_model)]
pub fn source_episode_liveness() -> Liveness {
    use aterm_spec::derive::{cst, eq, gt, int, or_, var};
    Liveness {
        name: "TheDwellEnds",
        goal: or_(
            or_(
                or_(eq(var("quiet"), cst("Dwell")), eq(var("open"), int(0))),
                gt(cst("Moves"), var("moved")),
            ),
            or_(eq(var("armed"), int(0)), eq(var("down"), int(1))),
        ),
        weak: vec!["Look"],
        strong: vec![],
        mutants: vec!["QuietRestarts"],
    }
}

/// THE CONSENT AND THE TELL: one rule's episodes, told to one session.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn owner_tell_model() -> Model {
    aterm_spec::ty_model! {
        OwnerTell {
            const Buggy = 0;
            const Prompt = 1;
            const Dry = 0;
            const Urgent = 0;
            const Cap = 1;
            const Older = 0;
            scenario Prompt = 0;
            scenario Dry = 1;
            scenario Urgent = 1;
            scenario Older = 1;
            var open = 0;
            var ep = 0;
            var fresh = 0;
            var due = 0;
            var told = 0;
            var rev = 0;
            var crev = 2;
            var snap = 2;
            var guard = 0;
            var pace = 0;
            var urg = 0;
            var twice = 0;
            var unconf = 0;
            var stalesnap = 0;
            var obs = 0;
            var dry = 0;
            var overcap = 0;
            var overguard = 0;
            var stale = 0;
            var older = 0;

            action Open when (open == 0 && 2 > ep) {
                open = 1;
                ep = ep + 1;
                due = 1;
                told = 0;
                fresh = if (Older == 1) { 0 } else { 1 };
            }
            action Close when (open == 1) { open = 0; }
            action EditTable { rev = if (rev == 0) { 1 } else { 0 }; }
            action Press when (snap == 2) { snap = rev; }
            action GuardOn when (guard == 0) { guard = 1; }
            action GuardOff when (guard == 1) { guard = 0; }
            action PaceOn when (pace == 0) { pace = 1; }
            action PaceOff when (pace == 1) { pace = 0; }
            action HourPasses when (urg > 0) { urg = 0; }

            action ConfirmWrite when (2 > snap) {
                crev = if (snap == rev) { snap } else { crev };
                snap = 2;
            }
            action Tell when (
                due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                twice = if (told == 1) { 1 } else { twice };
            }
            action Would when (
                due == 1 && open == 1 && fresh == 1 && (Prompt == 0 || Dry == 1) && crev == rev
            ) {
                due = 0;
            }
            action Lapse when (due == 1 && (open == 0 || fresh == 0 || crev > rev || rev > crev)) {
                due = 0;
            }

            action TellKeepsDue when (
                Buggy == 1 && due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && crev == rev
            ) {
                told = 1;
                urg = if (pace == 1) { urg + 1 } else { urg };
                twice = if (told == 1) { 1 } else { twice };
            }
            action ConfirmByName when (
                Buggy == 2 && due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && 2 > crev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                unconf = if (crev == rev) { unconf } else { 1 };
            }
            action ConfirmStaleSnapshot when (Buggy == 3 && 2 > snap) {
                crev = snap;
                snap = 2;
                stalesnap = if (snap == rev) { stalesnap } else { 1 };
            }
            action TypeUnderDryRun when (
                Buggy == 4 && due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                dry = if (Dry == 1) { 1 } else { dry };
            }
            action TypeUnderObserve when (
                Buggy == 5 && due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Dry == 0 && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                obs = if (Prompt == 0) { 1 } else { obs };
            }
            action UrgentUncapped when (
                Buggy == 6 && due == 1 && open == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || Urgent == 1) && Prompt == 1 && Dry == 0 && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                overcap = if (pace == 1 && Cap <= urg) { 1 } else { overcap };
            }
            action UrgentPassesGuard when (
                Buggy == 7 && due == 1 && open == 1 && fresh == 1 && (guard == 0 || Urgent == 1)
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                overguard = if (guard == 1) { 1 } else { overguard };
            }
            action TellUndone when (
                Buggy == 8 && due == 1 && fresh == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                stale = if (open == 0) { 1 } else { stale };
            }
            action TellOlder when (
                Buggy == 9 && due == 1 && open == 1 && guard == 0
                    && (pace == 0 || (Urgent == 1 && Cap > urg)) && Prompt == 1 && Dry == 0
                    && crev == rev
            ) {
                told = 1;
                due = 0;
                urg = if (pace == 1) { urg + 1 } else { urg };
                older = if (fresh == 0) { 1 } else { older };
            }

            invariant OncePerEpisode: twice == 0;
            invariant NothingTypedUnconfirmed: unconf == 0;
            invariant AStaleSnapshotConfirmsNothing: stalesnap == 0;
            invariant DryRunTypesNothing: dry == 0;
            invariant NothingTypedUnderObserve: obs == 0;
            invariant UrgentCapHolds: overcap == 0;
            invariant UrgentIsNeverAGuard: overguard == 0;
            invariant NeverToldStale: stale == 0;
            invariant NeverToldAnOlderEpisode: older == 0;
        }
    }
}

/// THE WORKFLOW STEP: one run of two tracks, told to one session.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn workflow_step_model() -> Model {
    aterm_spec::ty_model! {
        WorkflowStep {
            const Buggy = 0;
            const Captured = 1;
            const Prompt = 1;
            scenario Captured = 0;
            scenario Prompt = 0;
            var a = 0;
            var b = 0;
            var due_a = 0;
            var due_b = 0;
            var told_a = 0;
            var told_b = 0;
            var twice = 0;
            var stale = 0;
            var uncaptured = 0;
            var obs = 0;

            action ReturnA when (a == 0) { a = 1; }
            action ReturnB when (b == 0) { b = 1; }

            action OpenA when (a == 1 && b == 0 && due_a == 0 && Captured == 1) {
                due_a = 1;
                uncaptured = if (Captured == 0) { 1 } else { uncaptured };
            }
            action OpenB when (b == 1 && a == 0 && due_b == 0 && Captured == 1) {
                due_b = 1;
                uncaptured = if (Captured == 0) { 1 } else { uncaptured };
            }
            action TellA when (due_a == 1 && b == 0 && Prompt == 1) {
                told_a = 1;
                due_a = 2;
                twice = if (told_a == 1) { 1 } else { twice };
                stale = if (b == 1) { 1 } else { stale };
                obs = if (Prompt == 0) { 1 } else { obs };
            }
            action TellB when (due_b == 1 && a == 0 && Prompt == 1) {
                told_b = 1;
                due_b = 2;
                twice = if (told_b == 1) { 1 } else { twice };
                stale = if (a == 1) { 1 } else { stale };
                obs = if (Prompt == 0) { 1 } else { obs };
            }
            action WouldA when (due_a == 1 && b == 0 && Prompt == 0) { due_a = 2; }
            action WouldB when (due_b == 1 && a == 0 && Prompt == 0) { due_b = 2; }
            action LapseA when (due_a == 1 && b == 1) { due_a = 2; }
            action LapseB when (due_b == 1 && a == 1) { due_b = 2; }

            action TellKeepsDue when (Buggy == 1 && due_a == 1 && b == 0 && Prompt == 1) {
                told_a = 1;
                twice = if (told_a == 1) { 1 } else { twice };
            }
            action TellAfterEnd when (Buggy == 2 && due_a == 1 && Prompt == 1) {
                told_a = 1;
                due_a = 2;
                stale = if (b == 1) { 1 } else { stale };
            }
            action ReadUncaptured when (Buggy == 3 && a == 1 && b == 0 && due_a == 0) {
                due_a = 1;
                uncaptured = if (Captured == 0) { 1 } else { uncaptured };
            }
            action TypeUnderObserve when (Buggy == 4 && due_a == 1 && b == 0) {
                told_a = 1;
                due_a = 2;
                obs = if (Prompt == 0) { 1 } else { obs };
            }

            invariant OnceAStep: twice == 0;
            invariant NeverToldAfterTheRunEnds: stale == 0;
            invariant UnreadIsQuiet: uncaptured == 0;
            invariant NothingTypedUnderObserve: obs == 0;
        }
    }
}
