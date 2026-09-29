// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded models for the aterm wrapper's harness core
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md` §11 item 7).
//!
//! One scalar projection of a machine that ships in `aterm_agent::harness`: the
//! bounded child runner's worker lifecycle (`harness::align::capture_bounded`).
//! As everywhere in this crate the model is hand-written Rust DESCRIBING that
//! code, not extracted from it: Tier 0 here says the description holds over its
//! whole bounded space, and only the Tier-1 bind in `align`'s own tests makes it
//! a statement about the program that compiled.
//!
//! The ledger ring (`HarnessLedgerRing`, §11 item 5) and the grid spine's turn
//! machine (`HarnessTurnObservation`, §4.2/§5.8.1) were modelled here too.
//! Their subsystems, `harness::ring` and `harness::observe`, were deleted with
//! the second harness stack on 2026-09-23, and a model of code that no longer
//! exists proves nothing about anything; they went in the same change. The
//! limit-recovery ladder's model (`HarnessFailureRecovery`, §11 item 7) followed
//! on 2026-09-25, when the ladder itself was deleted from `harness::limits`.

use super::*;

/// A bounded harness capture may time out and reap its direct child while an
/// escaped descendant still holds stdout or stdin open. The capture only
/// returns after the reader and writer workers have both finished or been
/// cancelled and joined. Tier-1 runs the real `align::capture_bounded` with
/// escaped descendants and observes both worker completions at return.
/// `Buggy=1` recovers the historical early-return shape: child exit alone is
/// treated as enough, leaking a worker past the caller's deadline.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_capture_worker_lifecycle_model() -> Model {
    crate::ty_model! {
        HarnessCaptureWorkerLifecycle {
            const Buggy = 0;
            var child_reaped = 0;
            var reader_done = 0;
            var writer_done = 0;
            var deadline = 0;
            var returned = 0;

            action ChildExit when (child_reaped == 0 && returned == 0) {
                child_reaped = 1;
            }
            // The direct child may have exited while an escaped descendant
            // still holds a pipe. Its worker deadline remains meaningful.
            action Deadline when (deadline == 0 && returned == 0) {
                deadline = 1;
            }
            action TimeoutKill when (child_reaped == 0 && returned == 0) {
                deadline = 1;
                child_reaped = 1;
            }
            action ReaderFinish when (reader_done == 0 && returned == 0) {
                reader_done = 1;
            }
            action WriterFinish when (writer_done == 0 && returned == 0) {
                writer_done = 1;
            }
            action Return when (
                returned == 0 && child_reaped == 1 &&
                ((reader_done == 1 && writer_done == 1) || Buggy == 1)
            ) {
                returned = 1;
            }

            invariant NoReturnBeforeWorkersJoin:
                returned == 0 ||
                (child_reaped == 1 && reader_done == 1 && writer_done == 1);
        }
    }
}

/// THE CODEX DAEMON'S MOVE (`aterm_agent::harness::upgrade_codex::daemon_step`,
/// carried out by `upgrade_drive`'s `codex` module): Codex 0.157 runs one
/// shared app-server daemon per `$CODEX_HOME` that holds every conversation,
/// and the vendor's own verb that moves it onto the managed build restarts it
/// ("may interrupt running work"). A thread's turn runs in the daemon whether
/// a TUI in a tab shows it (`attached_busy`) or none does — a detached "Run
/// in background" thread (`detached_busy`), which only the daemon's own
/// writer locks reveal. A BACKGROUND TERMINAL a finished turn left running
/// (`terminal`: a unified-exec process in a session of its own under the
/// daemon) runs there too, its thread idle, and dies with the restart
/// (measured, review of 2026-09-26). The owner's `--skip`/`--defer` on a tab
/// whose conversation the daemon runs (`owner_held`) keeps that conversation
/// where it is, and a Codex attached to the daemon that the sweep does not
/// list (`unseen`: another aterm instance, a pane, an unreadable TUI) was
/// asked nothing. `version` is what the daemon runs: `0` older than the
/// managed build, `1` the managed build, `2` AHEAD of it (the vendor's
/// updater installed a newer one first, `VendorAhead`, which only an
/// unpinned daemon's updater does; `InstalledFromManaged` is the owner's first
/// `codex` laying the managed build in, unpinned). `Update` is the sweep's
/// act; it pins (`pinned`) and lands on the managed build.
/// `NoRunningWorkInterrupted`: no update while any thread's turn, or any
/// background terminal, runs. `NeverOntoAnOlderBuild`: never from a newer
/// build back onto the managed one. `NeverAgainstTheOwnersWord`: never while
/// the owner's word holds a tab it serves. `NeverPastAnUnseenClient`: never
/// while a client nobody asked is attached. `Buggy=1` is the rule the
/// research and the review warned against: a sweep that asks only the tabs
/// it can see (an idle TUI) and not the daemon — it interrupts a detached
/// thread's turn and a background terminal, "pins" a daemon the vendor
/// already moved ahead back onto an older build, overrides a `--skip`, and
/// restarts a daemon under a client it never saw.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_codex_daemon_update_model() -> Model {
    crate::ty_model! {
        HarnessCodexDaemonUpdate {
            const Buggy = 0;
            var version = 0;
            var pinned = 0;
            var attached_busy = 0;
            var detached_busy = 0;
            var terminal = 0;
            var owner_held = 0;
            var unseen = 0;
            var updated = 0;
            var interrupted = 0;
            var downgraded = 0;
            var overruled = 0;
            var blind = 0;

            action StartAttached when (attached_busy == 0) {
                attached_busy = 1;
            }
            action EndAttached when (attached_busy == 1) {
                attached_busy = 0;
            }
            action StartDetached when (detached_busy == 0) {
                detached_busy = 1;
            }
            action EndDetached when (detached_busy == 1) {
                detached_busy = 0;
            }
            action StartTerminal when (terminal == 0) {
                terminal = 1;
            }
            action EndTerminal when (terminal == 1) {
                terminal = 0;
            }
            action OwnerHolds when (owner_held == 0) {
                owner_held = 1;
            }
            action OwnerReleases when (owner_held == 1) {
                owner_held = 0;
            }
            action ClientUnseen when (unseen == 0) {
                unseen = 1;
            }
            action ClientGone when (unseen == 1) {
                unseen = 0;
            }
            action VendorAhead when (version == 0 && pinned == 0 && updated == 0) {
                version = 2;
            }
            // The owner's first `codex` from the managed build installs the
            // daemon at that build, UNPINNED (the vendor's follow-latest
            // marker): current, and still to be pinned.
            action InstalledFromManaged when (version == 0 && pinned == 0 && updated == 0) {
                version = 1;
            }
            action Update when (
                updated == 0 && attached_busy == 0 &&
                (Buggy == 1 ||
                    (detached_busy == 0 && terminal == 0 && owner_held == 0 && unseen == 0 &&
                     version <= 1 && (version == 0 || pinned == 0)))
            ) {
                interrupted = if attached_busy == 1 || detached_busy == 1 || terminal == 1 { 1 } else { 0 };
                downgraded = if version == 2 { 1 } else { 0 };
                overruled = if owner_held == 1 { 1 } else { 0 };
                blind = if unseen == 1 { 1 } else { 0 };
                updated = 1;
                version = 1;
                pinned = 1;
            }

            invariant NoRunningWorkInterrupted: interrupted == 0;
            invariant NeverOntoAnOlderBuild: downgraded == 0;
            invariant NeverAgainstTheOwnersWord: overruled == 0;
            invariant NeverPastAnUnseenClient: blind == 0;
        }
    }
}

/// THE LIVE UPGRADE'S LADDER (the owner's decision of 2026-09-28, after a
/// Codex upgrade sat behind in tab 1 for ten hours and the owner asked "do I
/// need to do something? … I want upgrades to be applied automatically"):
/// "Time ladder — prefer a natural pause; the longer it has been behind, the
/// less it waits: after 20 min a pause that only looks quiet counts, after 1 h
/// 'you're at the tab' means only that you typed in the last 20 s, and by
/// about 2 h it moves at the first such pause. It never types over a draft, a
/// dialog or running work." `aterm_agent::harness::upgrade::gate` and
/// `upgrade::rung`, `upgrade_drive::St::still`, `upgrade_codex::daemon_turn`
/// and `upgrade_codex::daemon_step`, as the daemon-mode Codex client the
/// incident was (its move is one typed `/exit`).
///
/// THE LANE. `rung` (0 Prefer, 1 Settled, 2 KeysOnly, 3 Land) climbs with the
/// clock (`Advance`) while a move is owed (`behind`). `Move` is the step: never
/// while a turn runs that may be THIS client's own (`work`: 1 its root's —
/// the agent's own on its screen, its own thread's in its Codex daemon — or
/// 2 one this screen never draws and the lane never places, below), a draft
/// stands (`draft`) or a box is up (`dialog`); never while the screen's mark (`glyph`: 0 `›`, 1 Codex 0.158.0's `»`) is one the reader
/// cannot read (`reads`); a person holds it — anyone near the tab (`keys` 1:
/// within `human_grace_s`) until KeysOnly, then only a keystroke within 20 s
/// (`keys` 2); and it waits for a settle — the screen's own quiet (`quiet`,
/// which a repaint ends: `Repaint`), or from Settled the reader's run of idle
/// looks at the same last words (`seen` 2, two `Look`s; a repaint leaves it),
/// and at Land none. A turn in ANOTHER session on the same daemon (`other`
/// 1: another conversation's ROOT, with another Codex attached to the
/// daemon) holds this client only until it is PLACED there: the kernel names
/// a client's own conversation only where every thread of its daemon hangs
/// from one root and it is the daemon's one client, so with another session
/// on the daemon the lane places a running root by this screen — two looks of
/// one still run, `QUIET_S` (20 s) apart, that found it running (`other_seen`
/// 2) — a turn of this client's own root would have drawn on it (a status
/// row, streaming words, a finished turn's rows), so a root that ran through
/// 20 s of it is another session's, and holds nothing more. What the lane
/// can NEVER place holds it for as long as it runs (`work` 2, `HiddenStarts`):
/// a SUBAGENT's turn — its turns draw on no main screen, its own
/// conversation's included; the third review of 2026-09-28 found three
/// subagents of the owner's tab's own conversation on its daemon, running on
/// their own after their spawn — whether the kernel names this client's
/// conversation (then it IS this client's) or not; and a root's with this
/// client the daemon's only Codex (a thread no tab shows — or this client's
/// own conversation while its screen shows another thread, a subagent's or
/// a `/side` fork's — not provably another session's). The screen reads idle
/// at the same words through it, so the looks go on (`Look`).
/// THAT PLACEMENT IS AN ASSUMPTION, and the model states where it fails:
/// under a GOAL this screen's footer shows (`goal`: `Pursuing goal (…)`), a
/// turn of this client's own may run where the screen reads idle — the goal's
/// next turn streams under the last turn's end row with no status row, the
/// last words standing (measured 2026-09-28, `GOAL_NEXT_TURN_0_158`) — so
/// such a turn (`TurnStarts` under `goal`) keeps the still run going and the
/// looks reading it, and the lane places NOTHING under a goal (`other_seen`
/// counts no look there). `Move`'s own-work clause is the real rule as it
/// sees it — no turn of this client's, or one the still run placed with no
/// goal on the screen — so `NoRunningWorkInterrupted` rests on the
/// assumption, not on the model's knowledge of whose turn it is: a turn of
/// this client's own that no goal hides draws on its screen and begins the
/// run again (`TurnStarts` without `goal`), and the goal's footer stands
/// until its hidden turn has ended (`GoalPaused` between turns).
/// Where a root IS placed the assumption stays one: that this client's screen
/// draws its own root's turn — not true where it shows a subagent's or a
/// `/side` fork's thread, or pursues a goal with no status line configured
/// (its footer draws the goal only through one); unmeasured, and not in this
/// model, whose `TurnStarts` without a goal always draws.
/// `Pin` is the daemon's pin (it restarts the daemon, so it waits
/// for every thread idle, this client's and every other's); a current client
/// on an unpinned daemon keeps its tab due for it (`kept`) — main's rule,
/// which the owner never dropped (the vendor's armed updater restarted that
/// daemon mid-turn at 06:05:45Z). The environment: turns start and end
/// (`TurnStarts`, `TurnEnds` — a goal-mode Codex starts the next at once;
/// `HiddenStarts`, `HiddenEnds`; `OtherStarts`, `OtherEnds`),
/// a goal is pursued and paused (`GoalPursued`,
/// `GoalPaused`), repaints, a person types, pauses and leaves, drafts and
/// boxes come and go, and the mark changes.
///
/// SAFETY, each with its `Buggy` member (the ratchet's non-vacuity, and each
/// dead action caught ALONE — `verify::audit_dead_negative_controls`):
/// `NeverOverADraft` (`MoveOverADraft`), `NeverOverABox` (`MoveOverABox`),
/// `NeverWithinKeysGap` (`NowWaivesThePerson`: the owner's `--now` as it was
/// until 2026-09-28, which waived the person outright), and
/// `NoRunningWorkInterrupted` (`MoveMidTurn`: a daemon-mode client ended while
/// its own thread runs a turn — the report's proposed repaint-proof move;
/// Codex 0.158.0's binary holds `Disconnected from this task. The current
/// turn was stopped.` for some case of an `/exit`, unmeasured which; and
/// `PlacedUnderAGoal`: the still run placing this client's own goal turn —
/// the second review of 2026-09-28, which drove the incident's goal looks
/// with one more thread on the daemon through that rule, and it typed
/// `/exit` at 06:12:42Z, mid-goal; and `PlacedAsItRan`: the still run
/// placing whatever ran through two of its looks — a subagent of this
/// client's own conversation, another's subagent, a root with no other Codex
/// attached — the third review of 2026-09-28, whose probe drove the owner's
/// shape, one conversation and its subagents under one TUI, through that
/// rule, and it typed `/exit` while a subagent of the tab's own ran) — the
/// moves record what stood when they were taken (`at_*`).
/// `QuietPauseCounts`
/// (`SeqSettle`): a look at a pause the ladder counts from Settled never waits
/// `settling` for a screen that only repainted — the incident's 120
/// `wait:settling`. And each of the LIVELOCKS below is also a bad STATE the
/// moment its mutant decides it, so no mutant is caught by the liveness alone:
/// `TheGraceNarrowsAtKeysOnly` (`AttendedWithoutLadder`: a pause from
/// KeysOnly on, nothing standing but a person merely near the tab, waited
/// on), `EitherMarkIsRead` (`BlindReader`: an idle `»` screen not read),
/// `AnotherSessionsTurnHoldsNothing` (`OtherTurnHolds`: this branch's first
/// cut, which held an idle client for every thread of its daemon — another
/// tab's goal included, placed or not — deciding so where no goal shows here)
/// and `AnUnpinnedDaemonKeepsItsTabDue` (`PinDropsDue`:
/// this branch's first cut again, which let a current client on an unpinned
/// daemon go, so nothing in the window pinned it). Each mutant fires only
/// from a state no mutant has touched (`slipped`): one slip per behaviour —
/// what "caught ALONE" asks — which also keeps the `Buggy = 1` space inside
/// the interpreter's budget (`non_vacuity_ratchet`'s
/// `every_buggy_space_fits_the_interpreter`).
///
/// LIVENESS: [`harness_upgrade_ladder_liveness`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_ladder_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeLadder {
            const Buggy = 0;
            const LandRung = 3;
            var rung = 0;
            var behind = 1;
            var pinned = 0;
            var kept = 1;
            var work = 0;
            var other = 0;
            var other_seen = 0;
            var goal = 0;
            var scoped = 1;
            var quiet = 0;
            var seen = 0;
            var keys = 0;
            var draft = 0;
            var dialog = 0;
            var glyph = 0;
            var reads = 1;
            var narrows = 1;
            var moved = 0;
            var missed = 0;
            var at_draft = 0;
            var at_box = 0;
            var at_keys = 0;
            var at_work = 0;
            var slipped = 0;

            // The wall clock, while a move is owed: the ladder's rung.
            action Advance when (behind == 1 && rung <= LandRung - 1) {
                rung = rung + 1;
            }
            // A turn of this client's own root: on its screen, or its own
            // thread's in its daemon. Not guarded on `behind`: a thread runs
            // on after its client moved. It draws on its screen (a status row,
            // streaming words), which begins the still run again — except
            // under a goal, whose next turn may run where the screen reads the
            // last one ENDED at the same words.
            action TurnStarts when (work == 0) {
                work = 1;
                quiet = 0;
                seen = if goal == 1 { seen } else { 0 };
                other_seen = 0;
            }
            action TurnEnds when (work == 1) {
                work = 0;
            }
            // A turn the lane can NEVER place: a SUBAGENT's — one this
            // client's conversation spawned (the owner's daemon, 2026-09-28:
            // three of them, their turns begun after the spawn), or another
            // conversation's — or a root's with this client the daemon's only
            // Codex. It never draws on this screen, which reads idle at the
            // same words look after look: nothing of it moves. Before the
            // move (after it, a turn anywhere is `TurnStarts` or `OtherStarts`
            // to the pin).
            action HiddenStarts when (behind == 1 && work == 0) {
                work = 2;
            }
            action HiddenEnds when (work == 2) {
                work = 0;
            }
            // A turn in another session on the same daemon (a goal's thread
            // in another tab: it ends and starts again at once), another Codex
            // attached: nothing of this client's screen moves.
            action OtherStarts when (other == 0) {
                other = 1;
            }
            action OtherEnds when (other == 1) {
                other = 0;
            }
            // The footer says a goal is pursued (`Pursuing goal (…)`), and
            // stops saying so — paused, achieved, cleared — between its turns:
            // a hidden turn ends first (an Esc interrupts it, which draws).
            // Nothing is placed under a goal, so its run of placing looks ends.
            action GoalPursued when (behind == 1 && goal == 0) {
                goal = 1;
                other_seen = 0;
            }
            action GoalPaused when (behind == 1 && goal == 1 && work == 0) {
                goal = 0;
            }
            // A redraw with no new words (a footer clock, a goal's counter).
            action Repaint when (behind == 1) {
                quiet = 0;
            }
            action Still when (behind == 1 && work == 0 && quiet == 0) {
                quiet = 1;
            }
            // A look at an idle screen the reader trusts — while a turn it
            // never draws runs too (`work` 2), and under a goal while its own
            // root's turn does (neither shows): the run of looks at the same
            // last words grows, and so does the run of looks that found
            // another session's root running through it (`other_seen`: two
            // PLACE it — a turn of this client's own root would have drawn on
            // its screen, which stood still); a look that finds none running
            // ends that run, and so does one under a goal, which places
            // nothing.
            action Look when (
                behind == 1 && (work == 0 || work == 2 || goal == 1) &&
                (glyph == 0 || reads == 1) &&
                (seen <= 1 || (other == 1 && goal == 0 && other_seen <= 1))
            ) {
                seen = if seen <= 1 { seen + 1 } else { 2 };
                other_seen = if other == 1 && goal == 0 { other_seen + 1 } else { 0 };
            }
            action Type when (behind == 1) {
                keys = 2;
            }
            action KeysPause when (behind == 1 && keys == 2) {
                keys = 1;
            }
            action KeysAway when (behind == 1 && keys == 1) {
                keys = 0;
            }
            action DraftTyped when (behind == 1 && draft == 0) {
                draft = 1;
            }
            action DraftCleared when (behind == 1 && draft == 1) {
                draft = 0;
            }
            action BoxUp when (behind == 1 && dialog == 0) {
                dialog = 1;
            }
            action BoxDown when (behind == 1 && dialog == 1) {
                dialog = 0;
            }
            action MarkChanges when (behind == 1) {
                glyph = if glyph == 0 { 1 } else { 0 };
            }
            // The daemon's pin: it restarts the daemon, so every thread idle,
            // this client's and every other session's; taken by a step of a
            // tab that is due — for its move, or, current on an unpinned
            // daemon, for the pin alone (`kept`).
            action Pin when (
                pinned == 0 && work == 0 && other == 0 && (behind == 1 || kept == 1)
            ) {
                pinned = 1;
            }
            // The step, as the real rule sees the daemon: no turn of this
            // client's own — or its root's the still run placed, which it does
            // only with no goal on the screen (a subagent's, never) — and
            // another session's turn holds it unless the still run placed it,
            // which it does only for a root with another Codex attached and no
            // goal on the screen.
            action Move when (
                behind == 1 && (work == 0 || (work == 1 && goal == 0 && seen == 2)) &&
                draft == 0 && dialog == 0 &&
                (glyph == 0 || reads == 1) &&
                (keys == 0 || (rung > 1 && narrows == 1 && keys <= 1)) &&
                (other == 0 || (scoped == 1 && goal == 0 && other_seen == 2)) &&
                (rung == LandRung || quiet == 1 || (rung > 0 && seen == 2))
            ) {
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            // THE MUTANTS, each its own dead action. The incident's settle: a
            // look at a pause the ladder counts that waits `settling` because
            // the screen repainted.
            action SeqSettle when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                (glyph == 0 || reads == 1) &&
                (keys == 0 || (rung > 1 && narrows == 1 && keys <= 1)) &&
                rung > 0 && rung <= LandRung - 1 && seen == 2 && quiet == 0 && missed == 0
            ) {
                slipped = 1;
                missed = 1;
            }
            // A person near the tab holds it at every rung (`human_grace_s`
            // never narrows): the upgrade before the ladder, deciding so at a
            // pause from KeysOnly on where nothing else stands.
            action AttendedWithoutLadder when (
                Buggy == 1 && slipped == 0 &&
                narrows == 1 && behind == 1 && rung > 1 && keys == 1 &&
                work == 0 && draft == 0 && dialog == 0
            ) {
                slipped = 1;
                narrows = 0;
            }
            // The reader of the incident: `›` alone, blind to 0.158.0's `»`,
            // deciding so at an idle `»` screen.
            action BlindReader when (
                Buggy == 1 && slipped == 0 && reads == 1 && behind == 1 && glyph == 1 && work == 0
            ) {
                slipped = 1;
                reads = 0;
            }
            // Every thread of the daemon holds the client (this branch's first
            // cut), deciding so at an idle client whose screen has placed
            // another session's running turn.
            action OtherTurnHolds when (
                Buggy == 1 && slipped == 0 &&
                scoped == 1 && behind == 1 && other == 1 && other_seen == 2 &&
                goal == 0 && work == 0
            ) {
                slipped = 1;
                scoped = 0;
            }
            // A current client on an unpinned daemon let go (this branch's
            // first cut): nothing in the window pins that daemon.
            action PinDropsDue when (
                Buggy == 1 && slipped == 0 && kept == 1 && behind == 0 && pinned == 0
            ) {
                slipped = 1;
                kept = 0;
            }
            // The owner's `--now` as it was: the person waived outright.
            action NowWaivesThePerson when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 0 && dialog == 0 &&
                keys == 2 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            action MoveOverADraft when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && draft == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            action MoveOverABox when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 0 && dialog == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            // A daemon-mode client ended while its own thread runs a turn.
            action MoveMidTurn when (
                Buggy == 1 && slipped == 0 &&
                behind == 1 && work == 1 && rung == LandRung
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }
            // The still run placing through a goal (the round before the
            // second review of 2026-09-28): the goal's own hidden turn ran
            // through two looks of this screen's run, as another session's
            // would, and the client was ended mid-goal.
            action PlacedUnderAGoal when (
                Buggy == 1 && slipped == 0 && behind == 1 && work == 1 && goal == 1 && seen == 2 &&
                draft == 0 && dialog == 0 && keys == 0 && rung > 0
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }

            // The still run placing whatever ran through two of its looks (the
            // round before the third review of 2026-09-28): a turn the lane
            // can never place — a subagent of this client's own conversation,
            // whose turns never drew here, another's subagent, a root with no
            // other Codex attached — taken for another session's, and the
            // client ended over it. The model counts no looks of such a turn
            // (`other_seen` counts another session's root's), so here it slips
            // wherever one runs under a still run of two looks.
            action PlacedAsItRan when (
                Buggy == 1 && slipped == 0 && behind == 1 && goal == 0 &&
                work == 2 && other == 0 && seen == 2 &&
                draft == 0 && dialog == 0 && keys == 0 && rung > 0 &&
                (glyph == 0 || reads == 1)
            ) {
                slipped = 1;
                behind = 0;
                moved = 1;
                at_draft = draft;
                at_box = dialog;
                at_keys = keys;
                at_work = work;
            }

            invariant NeverOverADraft: at_draft == 0;
            invariant NeverOverABox: at_box == 0;
            invariant NeverWithinKeysGap: at_keys <= 1;
            invariant NoRunningWorkInterrupted: at_work == 0;
            invariant QuietPauseCounts: missed == 0;
            invariant TheGraceNarrowsAtKeysOnly: narrows == 1;
            invariant EitherMarkIsRead: reads == 1;
            invariant AnotherSessionsTurnHoldsNothing: scoped == 1;
            invariant AnUnpinnedDaemonKeepsItsTabDue: kept == 1;
        }
    }
}

/// "THE UPGRADE LANDS" — the ladder's law as a property of whole behaviours
/// (the owner, 2026-09-28: "I want upgrades to be applied automatically"; the
/// law of aterm's own update ladder: ACTIVITY DELAYS; IT NEVER DISABLES):
/// `[]<>(moved-and-pinned \/ a-floor-stands)` — again and again, the move is
/// done and the daemon pinned (`behind = 0 /\ pinned = 1`); or, while the
/// move is owed, one of the floors no rung relaxes stands: a turn running
/// that may be this client's own (`work > 0`: its root's, or one the lane
/// never places — a subagent's, a root's with this client the daemon's only
/// Codex), a draft, a box, a keystroke within 20 s (`keys = 2`), or another
/// session's turn while this screen's footer shows a goal of its own
/// (`goal = 1 /\ other = 1`: nothing is placed under a goal, whose own turns
/// run unseen) — never another session's root's turn otherwise, which the
/// lane places and passes; or, the move done, the pin's own floor stands: a
/// turn running anywhere on the daemon (`work`, `other`). A behaviour that
/// leaves it for
/// good is one with no floor ever again and still no move or no pin — the
/// ladder waiting on comfort for ever, an idle client held for another
/// session's turn, or an unpinned daemon nobody visits.
///
/// THE ENVIRONMENT IS NOT FAIR: turns (this client's and other sessions'),
/// repaints, keystrokes, a person coming back, drafts, boxes and the mark are
/// the world's, and any may go on for ever — a goal-mode Codex turn included,
/// which is why a floor that keeps standing satisfies the goal (the upgrade
/// does not move over this client's running work: a daemon-mode client in
/// goal mode waits for its goal to pause, which the owner's decision of
/// 2026-09-28 will let aterm do briefly itself — the named `goal` wait is
/// that seam). What the verdict ASSUMES, and nothing more:
///
/// * `Advance`, weakly fair — the wall clock moves;
/// * `Look`, weakly fair — the lane keeps looking at an idle screen it can
///   read (the host looks again at every idle point, and on its re-look
///   ladder), which is what places another session's running turn;
/// * `Move`, STRONGLY fair — the look IS the move wherever the gate it
///   computes is open, so a gate the world keeps re-opening is caught open:
///   another session's goal idle between its turns, say, where the look that
///   finds its thread idle moves (the model splits the look from the move;
///   the apply ladder's `Park` is strongly fair for the same reason);
/// * `Pin`, weakly fair — a tab kept due is looked at again (at most every
///   `upgrade_drive::codex::PIN_LOOK_S` for the pin alone), and its step pins
///   a daemon every thread of which stays idle.
///
/// The mutants that break it, each alone: `AttendedWithoutLadder` (a person
/// back within `human_grace_s` for ever holds the move for ever),
/// `BlindReader` (a `»` the reader never reads: no step at all — the
/// incident's last hour), `OtherTurnHolds` (an idle client held for as long
/// as another tab's goal runs) and `PinDropsDue` (a current, unpinned daemon
/// nobody's step visits: the vendor's updater stays armed). Livelocks all:
/// nothing is ever stuck, so no deadlock check sees them.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_ladder_liveness() -> Liveness {
    Liveness {
        name: "TheUpgradeLands",
        goal: or_(
            and_(
                eq(var("behind"), int(0)),
                or_(
                    eq(var("pinned"), int(1)),
                    or_(gt(var("work"), int(0)), eq(var("other"), int(1))),
                ),
            ),
            and_(
                eq(var("behind"), int(1)),
                or_(
                    or_(
                        or_(gt(var("work"), int(0)), eq(var("draft"), int(1))),
                        or_(eq(var("dialog"), int(1)), eq(var("keys"), int(2))),
                    ),
                    and_(eq(var("goal"), int(1)), eq(var("other"), int(1))),
                ),
            ),
        ),
        weak: vec!["Advance", "Look", "Pin"],
        strong: vec!["Move"],
        mutants: vec![
            "AttendedWithoutLadder",
            "BlindReader",
            "OtherTurnHolds",
            "PinDropsDue",
        ],
    }
}

/// A READY answer is usable only by the process and tab that received the
/// notice, with a unique live owner of that conversation and a complete
/// session-file scan. The process-start token represents the kernel PID-reuse
/// guard.
///
/// AND A NOTICE WHOSE PROCESS IS GONE HOLDS NOTHING FOR EVER (2026-09-27).
/// Before this, an announced upgrade whose notified process had exited or
/// crashed without a relaunch waited on it for good once a person resumed the
/// conversation by hand, in the same tab or another: every visit said
/// `wait:notice-owned-by-other-process` before any step, and the owner's
/// `--now` could not move it. Once the kernel proves that process gone (its
/// pid names no process, or another one: `gone`), the live process holding the
/// conversation, which was never asked, is ASKED AFRESH (`Reask`). The upgrade
/// is pending again in a NEW ROUND: the round's markers are forgotten and the
/// salt minted again, so the READY the gone process left in the transcript the
/// two share can never answer a notice typed to the new one. While the
/// notified process lives, a process it never reached still waits.
///
/// The processes: A, whose tab is 1, pid 1, start 1, got the first notice. B
/// (pid 2, start 2) resumes the conversation in A's tab (`Resume`) or in tab 2
/// (`OtherTab`). A process the kernel started later under A's pid (start 3)
/// holds it in A's tab (`Recycled`). `pid == 0`: nobody holds the conversation
/// (A exited, and nobody has resumed it yet). `ready_pid`/`ready_start`: the
/// process whose notice the transcript's READY answers. `Look` is a visit with
/// its proofs complete (one live owner, a whole scan) that finds nothing the
/// upgrade does, derived from the reducer's own guards (enabled exactly where
/// neither `Reask` nor `Terminate` is). The ghost `stranded` records that such
/// a wait was on a notice whose process is gone, a wait that can never end on
/// its own.
///
/// Properties: `OnlyIssuerSignaled`, `NoDuplicateOwnerSignal`,
/// `NoPartialScanSignal`; `OnlyOnItsOwnAnswer`, meaning a restart acts only on
/// a READY to a notice typed to the process it signals; and
/// `NeverWaitsOnAGoneNotice`. ONE KNOB PER DEFECT: `NoReask` is the reducer
/// before this fix, which never asks afresh (the re-arm after a rest reaches
/// only a stopped round, never an announced one waiting on a gone notice).
/// `KeepRound` is the tempting wrong fix,
/// which asks afresh but keeps the round, so the gone process's READY answers
/// the new notice's marker. `Buggy=1` is the reducers before the fix at once:
/// the old conversation-keyed one, which accepts READY in another tab or after
/// a partial scan, and main's, which never asks afresh. Outside the model: the
/// release a gone process was owed, which nothing can type to it
/// (`upgrade_drive::St::notice_gone` drops it and the ledger says so). Tier-0
/// is aterm-spec's `derived_harness_upgrade_notice_owner`; Tier-1 is
/// aterm-agent's `harness::upgrade_drive` tests, over the real visit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_notice_owner_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeNoticeOwner {
            const Buggy = 0;
            const NoReask = 0;
            const KeepRound = 0;
            var phase = 0;
            var tab = 1;
            var pid = 1;
            var start = 1;
            var owner_tab = 0;
            var owner_pid = 0;
            var owner_start = 0;
            var owners = 1;
            var scan_complete = 1;
            var ready = 0;
            var ready_pid = 0;
            var ready_start = 0;
            var gone = 0;
            var signaled = 0;
            var stranded = 0;

            action Announce when (phase == 0 && pid > 0 && owners == 1 && scan_complete == 1) {
                phase = 1;
                owner_tab = tab;
                owner_pid = pid;
                owner_start = start;
                gone = 0;
            }
            // The READY answers the notice it was typed after: the fence's.
            action Ready when (phase == 1 && ready == 0) {
                ready = 1;
                ready_pid = owner_pid;
                ready_start = owner_start;
            }
            action OtherTab when (phase == 1 && tab == 1) {
                tab = 2;
                pid = 2;
                start = 2;
            }
            // Resumed by hand in the notice's own tab: after its process
            // exited, or with it alive and no longer holding the conversation.
            action Resume when (phase == 1 && tab == 1 && pid <= 1) {
                pid = 2;
                start = 2;
            }
            // The process the notice reached exits, or crashes, with no
            // relaunch. The conversation is nobody's until a process resumes
            // it, unless one already holds it.
            action OwnerExits when (
                phase == 1 && gone == 0 && owner_pid == 1 && owner_start == 1
            ) {
                gone = 1;
                pid = if (pid == owner_pid && start == owner_start) { 0 } else { pid };
                start = if (pid == owner_pid && start == owner_start) { 0 } else { start };
            }
            // The notice's pid, recycled: another process holds the
            // conversation under it.
            action Recycled when (phase == 1 && gone == 1 && tab == 1 && pid == 0) {
                pid = 1;
                start = 3;
            }
            action DuplicateOwner when (phase == 1 && owners == 1) {
                owners = 2;
            }
            action IncompleteScan when (scan_complete == 1 && phase <= 1) {
                scan_complete = 0;
            }
            action Terminate when (
                phase == 1 && pid > 0 && ready == 1 &&
                (Buggy == 1 ||
                    (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                     owner_pid == pid && owner_start == start))
            ) {
                phase = 2;
                signaled = 1;
            }
            // THE NOTICE'S PROCESS IS GONE and one it never reached holds the
            // conversation: asked afresh, pending again in a new round, the
            // round's markers forgotten (`KeepRound`: kept, and with them the
            // gone process's READY). Its fence goes, and nothing is owed to it.
            action Reask when (
                phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && gone == 1 &&
                Buggy == 0 && NoReask == 0 &&
                (if (owner_tab == tab && owner_pid == pid && owner_start == start) { 0 }
                 else { 1 }) == 1
            ) {
                phase = 0;
                ready = if KeepRound == 1 { ready } else { 0 };
                ready_pid = if KeepRound == 1 { ready_pid } else { 0 };
                ready_start = if KeepRound == 1 { ready_start } else { 0 };
                owner_tab = 0;
                owner_pid = 0;
                owner_start = 0;
            }
            // A VISIT that finds nothing to do: neither guard above holds. It
            // waits on a notice whose process is gone (`stranded`) or on one
            // that can still be answered.
            action Look when (
                phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && stranded == 0 &&
                (if (
                    phase == 1 && pid > 0 && owners == 1 && scan_complete == 1 && gone == 1 &&
                    Buggy == 0 && NoReask == 0 &&
                    (if (owner_tab == tab && owner_pid == pid && owner_start == start) { 0 }
                     else { 1 }) == 1
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && pid > 0 && ready == 1 &&
                    (Buggy == 1 ||
                        (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                         owner_pid == pid && owner_start == start))
                ) { 1 } else { 0 }) == 0
            ) {
                stranded = gone;
            }

            invariant OnlyIssuerSignaled:
                signaled == 0 ||
                (owner_tab == tab && owner_pid == pid && owner_start == start);
            invariant NoDuplicateOwnerSignal: signaled == 0 || owners == 1;
            invariant NoPartialScanSignal: signaled == 0 || scan_complete == 1;
            invariant OnlyOnItsOwnAnswer:
                signaled == 0 || (ready_pid == pid && ready_start == start);
            invariant NeverWaitsOnAGoneNotice: stranded == 0;
        }
    }
}

/// THE UPGRADE DRAIN'S BOUNDS (`aterm_agent::harness::upgrade`, `DRAIN_S`,
/// `HOLD_S`, `REASK_S`, `MAX_ASKS`). After a notice, each look takes one step:
/// wait, void the READY answer, end the agent, ask again, or give up. Between
/// looks, a person (a box nobody answers, a draft nobody sends, standing
/// `HOLD_S`), the agent's own work, its READY answer, and a break of its
/// background work come and go as they please. `waited` counts looks since
/// the latest notice and `aged` looks since its READY answer. Both saturate at
/// `Bound`, the drain and re-ask bound (`DRAIN_S == REASK_S`). `asks` counts
/// notices, up to `MaxAsks`. A break (`brk`) is a turn end with only the
/// agent's own work running. There the upgrade takes only a notice, a
/// give-up, or a void of an answer a person held, and never ends the agent.
///
/// Four properties.
/// - `NoEndOnAHeldAnswer`: the agent is never ended on an answer that a look
///   saw a person hold at or past the bound (`stale`). That is the person who
///   answered the box hours later and had the agent ended under them on the
///   strength of the old answer.
/// - `NoSilentWait`, about the agent's own work: it is waited for and never
///   ended, but once the re-ask clock has run out only a PERSON makes the
///   upgrade wait without a word. Otherwise it asks again, naming what runs,
///   or gives up (`silent`). That is the tab of 2026-09-26: an old Claude Code
///   sat four days behind two poll loops that could never end, told once and
///   never again.
/// - `NeverEndsRunningWork`: aterm never ends the agent while its own work
///   runs, at a break, or on a status of Claude's own that is not `idle`
///   (`cut`).
/// - `NoHastySupersede`: a READY answer gets a whole bound of its own before
///   work it outlives supersedes it, by a re-ask or a give-up (`hasty`). The
///   review of 2026-09-26 found that a notice's clock alone gave up on a READY
///   answered seconds before.
///
/// CLAUDE'S OWN STATUS LAGGING AN IDLE SCREEN (`lag`, the review of
/// 2026-09-27): `busy` or `shell` standing over a screen the session's own
/// reader read idle, looks in a row, with nothing under the agent the kernel
/// can see. Work in the agent's own process — a background agent, a workflow
/// — is no process under it, and that status is the one word that says it
/// runs: the Drain step asks Claude's own `idle` of the signal. So a lag is
/// held like the agent's work: the agent is never ended on it (`cut`), and
/// its READY is asked again past the bound, then given up on — never waited
/// on in silence. The notice and the release it may take type one line and
/// end nothing; they are the never-strands model's. Two knobs, one defect
/// each: `LagEnds`, the restart taking the lagging status for idle (caught by
/// `NeverEndsRunningWork`), and `LagHolds`, the tempting wrong fix — the
/// restart kept to `idle` but the READY behind the lag waited on for good
/// (caught by `NoSilentWait`).
///
/// `Buggy=1` is the drain before these bounds. It never voids, so it waits on
/// the person for good and ends the agent the moment they let go. At a break,
/// or under work that a READY answer did not end, it waits for good and says
/// nothing. It also carries the two tempting wrong fixes for that silence:
/// ending the agent once the bound is past, work or no work, and superseding
/// a READY answer on the notice's clock alone.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_drain_bound_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeDrainBound {
            const Buggy = 0;
            const LagEnds = 0;
            const LagHolds = 0;
            const Bound = 2;
            const MaxAsks = 2;
            var waited = 0;
            var aged = 0;
            var asks = 1;
            var ready = 1;
            var person = 0;
            var agent = 0;
            var brk = 0;
            var lag = 0;
            var stale = 0;
            var silent = 0;
            var cut = 0;
            var hasty = 0;
            var ended = 0;
            var gaveup = 0;

            action PersonHolds when (ended == 0 && gaveup == 0 && person == 0) { person = 1; }
            action PersonLets when (ended == 0 && gaveup == 0 && person == 1) { person = 0; }
            action AgentWorks when (ended == 0 && gaveup == 0 && agent == 0) { agent = 1; }
            action AgentRests when (ended == 0 && gaveup == 0 && agent == 1) { agent = 0; }
            action Answers when (ended == 0 && gaveup == 0 && ready == 0) {
                ready = 1;
                aged = 0;
            }
            action AtBreak when (ended == 0 && gaveup == 0 && brk == 0) { brk = 1; }
            action AtIdle when (ended == 0 && gaveup == 0 && brk == 1) { brk = 0; }
            // Claude's own status lags an idle screen, or catches up with it.
            action StatusLags when (ended == 0 && gaveup == 0 && lag == 0) { lag = 1; }
            action StatusIdle when (ended == 0 && gaveup == 0 && lag == 1) { lag = 0; }

            // At an idle point a READY waits on a person, on the agent's own
            // work, or on a status that lags (`LagEnds`: never); the re-ask
            // bounds the work and the lag (`LagHolds`: never the lag).
            action Wait when (
                ended == 0 && gaveup == 0 &&
                ((Buggy == 1 &&
                  (brk == 1 || (ready == 1 && (person == 1 || agent == 1 || lag == 1)))) ||
                 (brk == 1 && Buggy == 0 &&
                  ((ready == 0 && waited <= Bound - 1) ||
                   (ready == 0 && person == 1 && asks <= MaxAsks - 1) ||
                   (ready == 1 && aged <= Bound - 1 && (person == 0 || waited <= Bound - 1)))) ||
                 (brk == 0 && ready == 0 &&
                  (waited <= Bound - 1 || (person == 1 && asks <= MaxAsks - 1))) ||
                 (brk == 0 && ready == 1 && Buggy == 0 &&
                  (person == 1 || agent == 1 || (lag == 1 && LagEnds == 0)) &&
                  (person == 0 || waited <= Bound - 1) &&
                  ((agent == 0 && (lag == 0 || LagEnds == 1 || LagHolds == 1)) ||
                   person == 1 || aged <= Bound - 1)))
            ) {
                stale = if ready == 1 && person == 1 && waited > Bound - 1 { 1 } else { stale };
                silent = if person == 0 &&
                    ((ready == 0 && waited > Bound - 1) || (ready == 1 && aged > Bound - 1))
                    { 1 } else { silent };
                waited = if waited <= Bound - 1 { waited + 1 } else { waited };
                aged = if aged <= Bound - 1 { aged + 1 } else { aged };
            }
            action Void when (
                ended == 0 && gaveup == 0 && ready == 1 && person == 1 &&
                waited > Bound - 1 && Buggy == 0
            ) {
                ready = 0;
            }
            action ReAsk when (
                ended == 0 && gaveup == 0 && asks <= MaxAsks - 1 && person == 0 &&
                ((brk == 0 && ready == 0 && waited > Bound - 1) ||
                 (Buggy == 0 && brk == 1 &&
                  ((ready == 0 && waited > Bound - 1) || (ready == 1 && aged > Bound - 1))) ||
                 (Buggy == 0 && brk == 0 && ready == 1 &&
                  (agent == 1 || (lag == 1 && LagEnds == 0 && LagHolds == 0)) &&
                  aged > Bound - 1) ||
                 (Buggy == 1 && ready == 1 && agent == 1 && waited > Bound - 1))
            ) {
                hasty = if ready == 1 && aged <= Bound - 1 { 1 } else { hasty };
                asks = asks + 1;
                waited = 0;
                aged = 0;
                ready = 0;
            }
            action GiveUp when (
                ended == 0 && gaveup == 0 && asks > MaxAsks - 1 &&
                ((brk == 0 && ready == 0 && waited > Bound - 1) ||
                 (Buggy == 0 && brk == 1 &&
                  ((ready == 0 && waited > Bound - 1) ||
                   (ready == 1 && aged > Bound - 1 && person == 0))) ||
                 (Buggy == 0 && brk == 0 && ready == 1 &&
                  (agent == 1 || (lag == 1 && LagEnds == 0 && LagHolds == 0)) &&
                  aged > Bound - 1 && person == 0) ||
                 (Buggy == 1 && ready == 1 && agent == 1 && person == 0 && waited > Bound - 1))
            ) {
                hasty = if ready == 1 && aged <= Bound - 1 { 1 } else { hasty };
                gaveup = 1;
            }
            // Only on Claude's own `idle` (`LagEnds`: on a lagging status too).
            action Terminate when (
                ended == 0 && gaveup == 0 && ready == 1 && person == 0 &&
                ((brk == 0 && agent == 0 && (lag == 0 || LagEnds == 1)) ||
                 (Buggy == 1 && waited > Bound - 1))
            ) {
                cut = if brk == 1 || agent == 1 || lag == 1 { 1 } else { cut };
                ended = 1;
            }

            invariant NoEndOnAHeldAnswer: ended == 0 || stale == 0;
            invariant NoSilentWait: silent == 0;
            invariant NeverEndsRunningWork: cut == 0;
            invariant NoHastySupersede: hasty == 0;
        }
    }
}

/// THE LIVE UPGRADE NEVER STRANDS THE AGENT IT ASKED
/// (`aterm_agent::harness::upgrade`, `next_step`, `gate_announce`,
/// `clock_held`, `gate_release`, `transcript_has_ready`,
/// `directed_since_ready`, and the driver's record transitions). The
/// incident it is written for (2026-09-25/26, tab `s-d3346b29dd236432b852`):
/// Claude Code parked the session at its weekly usage limit (`⚠ Usage limit
/// reached · continuing automatically at 6am`), which reads idle to the
/// gates; the upgrade typed four notices into it half an hour apart, gave up
/// two hours later, and at 06:00 all four were delivered at once — the agent
/// stopped its work and answered READY with the last marker, and nothing
/// acted: the upgrade had FAILED `unanswered` for good, and nothing told the
/// agent to go on. It sat stopped until the owner came back.
///
/// `limited`: the session stands at its limit (the account comes and goes as
/// it pleases). `phase`: 0 pending, 1 announced, 2 gave up asking, 3
/// restarted and carried on, 4 stopped otherwise (the owner's hold, a refused
/// restart). `asks` counts notices to `MaxAsks`, the real `MAX_ASKS` scaled
/// down; `window`: the re-ask window since the last notice has run out.
/// `unread`: a notice typed while limited sits queued, read when the limit
/// resets. `holding`: the agent read a notice and stopped for the restart;
/// `ready`: its last word is READY to a marker the record still keeps
/// (`live`). `owed`: one release line is owed. THE CONVERSATION (the second
/// review of 2026-09-26): `told`, someone directed the agent — a person, a
/// peer, the supervisor — since its latest READY or the latest notice, and
/// it has not answered yet; `directed`, it answered such a direction and took
/// it up. Four ghosts: `blind`, a notice was typed while limited; `stuck`, the
/// upgrade's LAST WORD — a look at a point the agent can read that finds
/// nothing left for it to do — found the agent holding; `overrode`, a restart
/// acted on a READY someone had spoken after; `dropheld`, a release was
/// dropped while the agent held for the restart.
///
/// THE AGENT'S OWN WORK UNDER A READY (composed with the drain bound's
/// re-ask, `HarnessUpgradeDrainBound`, 2026-09-27): past the bound an
/// announced upgrade asks again (`Supersede`) — or, its asks spent, gives up
/// over the answer (`GiveUpOutlived`) — and a gave-up one voids it (`Void`),
/// owing the release. A person's hold voids in either phase. Both are the
/// environment's holds; Tier-1 checks each where the model allows it.
///
/// A BREAK THAT MAY NEVER END (`brk`, 2026-09-27): the agent's turn is over
/// and only work it started runs — two widowed `tail -f` shells under a
/// Claude Code whose status read `shell`, every screen a break and no idle
/// point for as long as they lived. The environment enters a break and may
/// leave it, or never (no fairness back to an idle point). There the upgrade
/// ends nothing (`Restart` is an idle point's), but everything else goes as
/// at an idle point: the notice, the give-up, a void — the drain's void of a
/// gave-up upgrade's late READY stands for the restart a break never takes —
/// and the release, typed or dropped. The last word there is the step that
/// repeats for as long as the break lasts. `IdleOnlyRelease` is the defect
/// the incident ran on: the release, its drop and the gave-up void only at
/// an idle point, so a give-up at a break that never ends left the agent
/// holding.
///
/// `Look` is DERIVED FROM THE REDUCER'S OWN GUARDS (the review of
/// 2026-09-26): enabled exactly where neither `Restart` nor `Release` nor
/// `DropRelease` is — the guards spelled again inside it (the release's
/// point, typed or dropped, is one term: the two split it by the direction),
/// which the Tier-0 test checks state by state. The hand-written clauses it
/// replaces ("a restart it would take", "the release will come") assumed
/// what was to be proved: with F2 reverted, or the release never typed, or
/// both, `NeverStranded` still held.
///
/// NO STOP IS FOR GOOD (the owner, 2026-09-27: "you should NEVER have
/// upgrades stalled" — tab `s-d3346b29dd236432b852` sat `failed:unanswered`
/// for 1d22h, its late READY voided under the background gate it always
/// runs, and `Phase::Failed(_) => Wait("failed")` was terminal for the
/// target). `rest`: the stopped round (gave up, or stopped otherwise) has
/// rested the real `RETRY_S` — the environment's clock (`Rests`), begun again
/// by every stop and by the void of a gave-up round's late READY, and not
/// held at a limit (the round it starts types nothing there). `Rearm`: the
/// reducer's new round — pending again, its asks, window and markers reset —
/// taken wherever the rest has run out, except over a gave-up round's late
/// READY, which is still acted on (`Restart`, or voided). The release still
/// owed is carried: typed, or dropped, until the round's rest runs out — at
/// a break too — and past it the new round's first notice, also a break's,
/// supersedes it. The last
/// word (`Look`) is now the upgrade's QUIET word — nothing it would do for
/// the agent at this look: a stopped round's `wait:failed` while it rests,
/// which the host looks at again, is one. A fifth ghost, `stalled`: the quiet
/// word said at a stopped round whose rest has run out — a permanent wait.
///
/// `DropRelease` is the driver's own rule (`release_void`, `directed`),
/// never the property it must keep: its guard is the direction the real code
/// reads, and that it drops only an agent that took other work up — never
/// one holding for the restart — is the invariant `NoDropOverAHold`, proved
/// here, not a guard conjunct assumed (a guard that encodes the obligation
/// makes the invariant naming it vacuous).
///
/// Properties: `NoNoticeWhileLimited` — nothing is typed into a limited
/// session (and the re-ask clock neither runs there nor carries across it:
/// `Elapse`, `LimitResets`); `NeverStranded` — an agent the upgrade asked is
/// restarted and carried on, or released, never left holding under the
/// upgrade's last word; `NoDropOverAHold` — a release is dropped only for an
/// agent that took up direction given after its last answer;
/// `NoRestartOverDirection` — a restart never acts on a READY someone spoke
/// after; and `NeverStalls` — no reachable state is a permanent wait: a
/// stopped round whose rest has run out always has a new round (or its late
/// READY's restart) enabled, so the quiet word is never said past it. ONE
/// KNOB PER DEFECT, each caught on its own: `Terminal` (a stopped round is
/// for good: no `Rearm`, today's terminal `failed`), `NoF1` (notices and
/// the clock at a limit), `NoF2` (a gave-up upgrade deaf to a late READY),
/// `NoOwe` (nothing abandoned owes a release), `NoType` (a release owed,
/// never typed), `KeepReady` (a stop keeps the round's markers, and the
/// release waits on a READY no phase acts on — the review's first blocker),
/// `LastWhileOwed` (a stop's own word is the host's last, with a release
/// still owed — its second), `StaleDirection` (a direction the agent
/// answered with READY still counts, and drops the release its void owes —
/// the second review's blocker), `UnansweredDirection` (a direction counts
/// before the agent answers it, and a look mid-turn drops the release the
/// READY it then gives needed), `ReadyOverDirection` (a READY stays the last
/// word past a direction the agent never answered — an Esc, a message met by
/// a `<synthetic>` row), `IdleOnlyRelease` (the release and a gave-up
/// upgrade's void only at an idle point, never at a break — the
/// 2026-09-27 incident). `Buggy = 1` is the reducer before the fix, the
/// incident's three at once (`NoF1`, `NoF2`, `NoOwe`), and the terminal
/// `failed` of 2026-09-27 (`Terminal`). Tier-1 in
/// aterm-agent's `harness::upgrade_drive` tests drives the real reducer,
/// gates, record transitions, transcript readers and the host's reading of
/// each step's word over every reachable state — a gave-up round's late
/// READY at a BREAK of the agent's own work (`brk`) among them: the real code
/// must `Void` there exactly where the model voids, so the break's old
/// `wait:background` is caught — and replays the incident, the pre-fix trace
/// as the caught negative control.
///
/// ONE STATED EXCEPTION, outside the model: an agent found to be no job of a
/// job-control shell is refused and owed no release
/// (`upgrade_drive::not_a_job`) — the line is typed under the notice's
/// fences, the agent its shell's foreground job among them, and such an
/// agent can never be proven to read it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_never_strands_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeNeverStrands {
            const Buggy = 0;
            const NoF1 = 0;
            const NoF2 = 0;
            const NoOwe = 0;
            const NoType = 0;
            const KeepReady = 0;
            const LastWhileOwed = 0;
            const StaleDirection = 0;
            const UnansweredDirection = 0;
            const ReadyOverDirection = 0;
            const IdleOnlyRelease = 0;
            const Terminal = 0;
            const MaxAsks = 2;
            var limited = 0;
            var brk = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var unread = 0;
            var holding = 0;
            var ready = 0;
            var live = 0;
            var owed = 0;
            var told = 0;
            var directed = 0;
            var blind = 0;
            var stuck = 0;
            var overrode = 0;
            var dropheld = 0;
            var rest = 0;
            var stalled = 0;

            // The account and the agent, as they please.
            action LimitHits when (limited == 0 && (phase <= 2 || phase == 4)) {
                limited = 1;
            }
            // The clock is held through the limit: an announced upgrade's
            // window starts again when the agent can read.
            action LimitResets when (limited == 1) {
                limited = 0;
                holding = if unread == 1 { 1 } else { holding };
                unread = 0;
                window = if (phase == 1 && Buggy == 0 && NoF1 == 0) { 0 } else { window };
            }
            // The agent answers READY — a direction it had not answered yet
            // is answered by it (`StaleDirection`: still counted).
            action AgentReady when (limited == 0 && holding == 1 && ready == 0 && live == 1) {
                ready = 1;
                directed = if ((StaleDirection == 1 || Buggy == 1) && told == 1) { 1 } else if ((StaleDirection == 1 || Buggy == 1)) { directed } else { 0 };
                told = 0;
            }
            // The agent goes on — with the direction it was given, if one
            // stands unanswered.
            action AgentGoesOn when (limited == 0 && holding == 1 && ready == 0) {
                holding = 0;
                directed = if told == 1 { 1 } else { directed };
                told = 0;
            }
            // Someone directs the agent holding for the restart: a person, a
            // peer, the supervisor. A READY before it is its last word no
            // more (`ReadyOverDirection`: it is).
            action Direct when (limited == 0 && holding == 1 && told == 0 && directed == 0) {
                told = 1;
                ready = if (ReadyOverDirection == 1 || Buggy == 1) { ready } else { 0 };
            }
            // The re-ask clock runs only while the session can read a notice.
            action Elapse when (
                phase == 1 && window == 0 && (limited == 0 || Buggy == 1 || NoF1 == 1)
            ) {
                window = 1;
            }
            // A break of the agent's own work: its turn over, work it started
            // still running. It may end, or never.
            action BreakBegins when (brk == 0) {
                brk = 1;
            }
            action BreakEnds when (brk == 1) {
                brk = 0;
            }
            // A stopped round's rest runs out (`RETRY_S` since it stopped),
            // limited or not: the round it lets start types nothing there.
            action Rests when ((phase == 2 || phase == 4) && rest == 0) {
                rest = 1;
            }

            // The upgrade's steps.
            action Announce when (
                ready == 0 && (limited == 0 || Buggy == 1 || NoF1 == 1) &&
                (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
            ) {
                phase = 1;
                asks = asks + 1;
                window = 0;
                live = 1;
                owed = 0;
                unread = limited;
                holding = if limited == 1 { holding } else { 1 };
                blind = if limited == 1 { 1 } else { blind };
                told = 0;
                directed = 0;
            }
            action GiveUp when (
                phase == 1 && window == 1 && asks == MaxAsks && ready == 0 &&
                (limited == 0 || Buggy == 1 || NoF1 == 1)
            ) {
                phase = 2;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // Never at a break: the restart would end the work that runs.
            action Restart when (
                ready == 1 && live == 1 && limited == 0 && brk == 0 &&
                (phase == 1 || (phase == 2 && Buggy == 0 && NoF2 == 0))
            ) {
                phase = 3;
                ready = 0;
                holding = 0;
                live = 0;
                owed = 0;
                overrode = if told == 1 { 1 } else { overrode };
                told = 0;
                directed = 0;
                rest = 0;
            }
            // The agent's own background work outlived the READY answer past
            // the bound (the environment's hold, as `Void`'s is): an
            // announced upgrade asks again, naming what runs, and the new
            // notice supersedes the answer — its READY is no longer the last
            // word after the latest notice (the four-day tab of 2026-09-26).
            action Supersede when (
                ready == 1 && live == 1 && limited == 0 && phase == 1 &&
                window == 1 && asks <= MaxAsks - 1
            ) {
                asks = asks + 1;
                window = 0;
                ready = 0;
                owed = 0;
                holding = 1;
                told = 0;
                directed = 0;
            }
            // Its asks spent, it gives up over the answer the work outlived:
            // the answer stands, and the gave-up arm's `Void` releases it.
            action GiveUpOutlived when (
                ready == 1 && live == 1 && limited == 0 && phase == 1 &&
                window == 1 && asks == MaxAsks
            ) {
                phase = 2;
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // A person held the READY answer past the drain's bound — an
            // announced upgrade's, or the late answer a gave-up one hears —
            // or the agent's own background work held a gave-up one's: at an
            // idle point, and at a break (`IdleOnlyRelease`: a gave-up one's
            // only at an idle point).
            action Void when (
                ready == 1 && live == 1 && limited == 0 &&
                ((phase == 1 && window == 1) ||
                 (phase == 2 && Buggy == 0 && NoF2 == 0 && (brk == 0 || IdleOnlyRelease == 0)))
            ) {
                ready = 0;
                live = 0;
                owed = if NoOwe == 1 { owed } else { 1 };
                window = if (phase == 1 && Buggy == 0 && NoOwe == 0) { 0 } else { window };
                // A gave-up round's rest begins again at the void: the release
                // it owes stands a whole rest before a new round's notice.
                rest = 0;
            }
            // The owner's hold, a refused plan or signal: the round is
            // abandoned — the markers with it, unless `KeepReady`.
            action Abandon when (phase == 1 || (phase == 2 && ready == 1 && live == 1)) {
                phase = 4;
                ready = if KeepReady == 1 { ready } else { 0 };
                live = if KeepReady == 1 { live } else { 0 };
                owed = if NoOwe == 1 { owed } else { 1 };
                rest = 0;
            }
            // NO STOP IS FOR GOOD: a stopped round that has rested starts a
            // new one — pending, its asks, window and markers reset — unless
            // it gave up with a late READY in hand, which is still acted on.
            // A release still owed is carried: the new round's first notice
            // supersedes it (`Terminal`: never — today's terminal `failed`).
            action Rearm when (
                (phase == 2 || phase == 4) && rest == 1 && Buggy == 0 && Terminal == 0 &&
                (phase == 4 || ready == 0 || live == 0)
            ) {
                phase = 0;
                asks = 0;
                window = 0;
                ready = 0;
                live = 0;
                rest = 0;
            }
            // The release's point: it waits behind a READY only where the
            // phase acts on it (`KeepReady` waits behind any), and is typed
            // unless the agent took up direction given after its last answer
            // (`UnansweredDirection`: or was merely given one) — at a break
            // as at an idle point (`IdleOnlyRelease`: never at a break).
            action Release when (
                owed == 1 && limited == 0 && NoType == 0 &&
                (brk == 0 || IdleOnlyRelease == 0) &&
                (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                (phase == 2 || phase == 4 || (phase == 1 && window == 0)) &&
                directed == 0 && ((UnansweredDirection == 0 && Buggy == 0) || told == 0) &&
                (rest == 0 || Buggy == 1 || Terminal == 1 || (phase == 2 && ready == 1 && live == 1))
            ) {
                owed = 0;
                holding = 0;
                live = 0;
                ready = 0;
            }
            // Dropped there instead (`release_void`, `directed`): nothing is
            // owed, and the round's markers are forgotten. Nothing is typed,
            // so a limit holds the drop back only where it holds the step's
            // word (an announced upgrade waits `limited`; a stopped one's
            // word is its stop).
            action DropRelease when (
                owed == 1 && NoType == 0 &&
                (brk == 0 || IdleOnlyRelease == 0) &&
                (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                (phase == 2 || phase == 4 || (phase == 1 && window == 0 && limited == 0)) &&
                (directed == 1 || ((UnansweredDirection == 1 || Buggy == 1) && told == 1)) &&
                (rest == 0 || Buggy == 1 || Terminal == 1 || (phase == 2 && ready == 1 && live == 1))
            ) {
                owed = 0;
                live = 0;
                ready = 0;
                dropheld = if holding == 1 { 1 } else { dropheld };
            }
            // THE QUIET WORD at a point the agent can read: nothing left the
            // upgrade would do now — no restart (at a break, where none is
            // taken, the drain's void of a gave-up upgrade's READY stands for
            // it), no release typed or dropped, no new round (`LastWhileOwed`:
            // a stop's own word said over a release still owed). At a break
            // it is the word the break repeats; said past a stopped round's
            // rest, it is a permanent wait (`stalled`).
            action Look when (
                (phase == 2 || phase == 4) && limited == 0 && stuck == 0 &&
                (if (
                    ready == 1 && live == 1 && limited == 0 &&
                    (phase == 1 || (phase == 2 && Buggy == 0 && NoF2 == 0)) &&
                    (brk == 0 || IdleOnlyRelease == 0)
                ) { 1 } else { 0 }) +
                (if (LastWhileOwed == 1 && phase == 4) { 0 } else if (
                    owed == 1 && limited == 0 && NoType == 0 &&
                    (brk == 0 || IdleOnlyRelease == 0) &&
                    (ready == 0 || (phase == 4 && KeepReady == 0)) &&
                    (phase == 2 || phase == 4 || (phase == 1 && window == 0))
                ) { 1 } else { 0 }) +
                (if (
                    (phase == 2 || phase == 4) && rest == 1 && Buggy == 0 && Terminal == 0 &&
                    (phase == 4 || ready == 0 || live == 0)
                ) { 1 } else { 0 }) == 0
            ) {
                stuck = holding;
                stalled = if rest == 1 { 1 } else { stalled };
            }

            invariant NoNoticeWhileLimited: blind == 0;
            invariant NeverStranded: stuck == 0;
            invariant NoDropOverAHold: dropheld == 0;
            invariant NoRestartOverDirection: overrode == 0;
            invariant NeverStalls: stalled == 0;
        }
    }
}

/// THE END OF THE AGENT'S OWN WORK IS THE UPGRADE'S POINT (aterm-agent
/// `harness/upgrade.rs`, `next_step` over `Facts::work_ended`;
/// `harness/upgrade_drive.rs`, `St::work_seen`, `St::time_work` and the visit's
/// same-visit re-arm). The incident (2026-09-28, s-692e6 "Free disk space"):
/// the agent answered every notice within seconds — "I can't stop yet, my
/// workflow is still running; ask again later" — so the round asked four
/// times at breaks of that work, gave up, rested two hours, re-armed and asked
/// again, round after round. The one moment the move could finish is the
/// first idle point after the work, with nothing under the agent — and there
/// the round waited `awaiting-ready` (announced within its window) or `failed`
/// (resting): words that own no turn end, so the loop's `keep going` took the
/// point and a new multi-hour workflow began. The rest cycles only while the
/// point the "not yet" waited for keeps going to the loop.
///
/// AND NO WAIT THERE OUTLIVES THE RE-ASK'S WINDOW (the review of 2026-09-28):
/// the ask at the end of the work goes once the RESTART's gate is open but for
/// the READY, and that gate never reads past Claude's status — a `shell` or
/// `busy` left over the idle screen after the agent's background shell ended
/// waits `status-stale` at every look. Held there whatever the window, the
/// round stayed announced for ever: no re-ask, no give-up, no new round. Inside
/// the window the gate's wait holds the point; past it the re-ask decides — a
/// line through the lag (the notice's own gate reads it past), or, its asks
/// spent, the give-up.
///
/// `phase` 1 announced with no READY, 2 resting after a give-up (a round that
/// gave up hears its late READY as ever: out of the model); `asks` the round's
/// notices (`Cap` is `MAX_ASKS`, the Tier-1 shifting the model's count onto
/// the real one); `worked` the agent's own work seen since the latest notice;
/// `screen` 0 an idle point with nothing under the agent, 1 a break of its own
/// work; `lag` Claude's status reads its own work over that idle screen and
/// never catches up (the restart's gate waits, a line's does not); `late` the
/// re-ask's window since the latest notice has passed; `looked` the host has
/// taken its step at this point (it goes first: `IdleHost::at_idle`), `owns`
/// that step owns the point. `HostAsks` is the notice at the end of the work
/// (from a rest, re-armed: its asks start again); `HostHolds` the restart
/// gate's wait there; `HostReasks` and `HostGivesUp` the re-ask past the
/// window; `HostWaits` the quiet word owning nothing. Three ghosts: `handed`,
/// the loop continued the agent at an idle point after its work was seen and
/// ended, the restart's gate open, while the round was open; `nagged`, an ask
/// at the end of work no look saw — the one extra ask per end of its work,
/// never a notice on a clock of its own; `stuck`, an announced round's look
/// past its window waited the restart gate's word. DIALS, each caught alone:
/// `Buggy = 1` every build before (the quiet word there, an ask anywhere, the
/// wait past the window); `Nag = 1` an ask with no work seen since the notice;
/// `Hold = 1` the wait at the end of the work holds past the window (the
/// first build of the ask at the end of the work, before that review).
///
/// Tier-1: aterm-agent's `harness::upgrade` tests
/// (`the_work_end_reducer_is_bound_to_its_model`) drive the real `next_step`
/// over every reachable idle point of this model: it asks (`Announce`, or
/// `Rearm` from a rest) exactly where `HostAsks` or `HostReasks` is enabled,
/// gives up exactly where `HostGivesUp` is, waits the restart gate's word
/// exactly where `HostHolds` is, and its quiet word exactly where `HostWaits`
/// is.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_work_end_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeWorkEnd {
            const Buggy = 0;
            const Nag = 0;        // 1: the ask needs no work seen since the notice
            const Hold = 0;       // 1: the wait at the end of the work outlives the window
            const Cap = 3;        // MAX_ASKS: the re-ask past it gives up
            var phase = 1;
            var asks = 1;
            var worked = 0;
            var screen = 0;
            var lag = 0;
            var late = 0;
            var looked = 0;
            var owns = 0;
            var handed = 0;
            var nagged = 0;
            var stuck = 0;

            // The agent's own work runs: a break of it, seen by the round's
            // look there.
            action WorkRuns when (screen == 0) {
                screen = 1;
                worked = 1;
                looked = 0;
                owns = 0;
            }
            // Its work ends: an idle point with nothing under the agent.
            action WorkEnds when (screen == 1) {
                screen = 0;
                looked = 0;
                owns = 0;
            }
            // Claude's status keeps reading its own work over the idle screen,
            // and never catches up: the next look reads it.
            action StatusLags when (screen == 0 && lag == 0) {
                lag = 1;
                looked = 0;
                owns = 0;
            }
            // The re-ask's window since the latest notice runs out: the next
            // look reads it.
            action WindowPasses when (phase == 1 && late == 0) {
                late = 1;
                looked = 0;
                owns = 0;
            }
            // Asked four times at breaks, the round gives up and rests.
            action GivesUp when (phase == 1 && screen == 1) {
                phase = 2;
            }
            // The host's step at the end of the work, the restart's gate open:
            // the notice (a rest re-armed first), owning the point.
            action HostAsks when (
                screen == 0 && looked == 0 && lag == 0 &&
                (worked == 1 || Nag == 1 || Buggy == 1)
            ) {
                nagged = if worked == 0 { 1 } else { nagged };
                asks = if phase == 2 { 1 } else if asks <= Cap - 1 { asks + 1 } else { asks };
                phase = 1;
                worked = 0;
                late = 0;
                looked = 1;
                owns = 1;
            }
            // The restart's gate waits there (the status lags): a settle's
            // word, owning the point — inside the window, or through a rest.
            action HostHolds when (
                screen == 0 && looked == 0 && worked == 1 && lag == 1 &&
                (phase == 2 || late == 0 || Hold == 1 || Buggy == 1)
            ) {
                stuck = if (phase == 1 && late == 1) { 1 } else { stuck };
                looked = 1;
                owns = 1;
            }
            // Past the window the re-ask: its line goes through the lag.
            action HostReasks when (
                phase == 1 && screen == 0 && looked == 0 && late == 1 && asks <= Cap - 1 &&
                (worked == 0 || (lag == 1 && Hold == 0))
            ) {
                asks = asks + 1;
                worked = 0;
                late = 0;
                looked = 1;
                owns = 1;
            }
            // Or, its asks spent, the give-up: the round rests.
            action HostGivesUp when (
                phase == 1 && screen == 0 && looked == 0 && late == 1 && asks > Cap - 1 &&
                (worked == 0 || (lag == 1 && Hold == 0))
            ) {
                phase = 2;
                late = 0;
                looked = 1;
                owns = 0;
            }
            // Or its quiet word — `awaiting-ready`, `failed` — owning nothing.
            action HostWaits when (
                screen == 0 && looked == 0 && (worked == 0 || Buggy == 1) &&
                (phase == 2 || late == 0)
            ) {
                looked = 1;
                owns = 0;
            }
            // The loop continues the agent at a point its host owns nothing
            // of; the next point comes after that turn.
            action LoopContinues when (screen == 0 && looked == 1 && owns == 0) {
                handed = if (worked == 1 && lag == 0) { 1 } else { handed };
                looked = 0;
            }

            invariant TheEndOfItsWorkIsTheUpgrades: handed == 0;
            invariant AnAskFollowsWorkItSaw: nagged == 0;
            invariant NoWaitOutlivesTheReaskWindow: stuck == 0;
        }
    }
}

/// THE LOGIN WALL (`aterm_agent::harness::upgrade`: `gate_announce`,
/// `next_step`, `announce_asks`, `transcript_login`, `clock_held` and the
/// driver's `login_facts`; `aterm_agent::supervise::policy::turn_end`:
/// `decide_turn_end`'s auth arm and its login hold; both over `aterm_phase`'s
/// wall reader). The incident it is written for (2026-09-27, tab
/// `s-b5cf2faabac5ce5127bd`, Claude Code 2.1.281): the login expired, and
/// every turn after it ended in milliseconds on Claude Code's synthetic
/// `authentication_failed` row, drawn `⏺ Login expired · Please run
/// /login`, which the reader read as idle with no wall. The supervisor typed
/// `continue` and `keep going` into it and told nobody; the live upgrade typed
/// four notices into it half an hour apart — each answered by the wall, none
/// read by the model — counted them, and gave up at 07:03; the READY the
/// agent gave once the owner logged in at 14:33 answered an upgrade that had
/// stopped.
///
/// The session: `login` (signed in), `wall` (its last turn ended on the wall,
/// whose row the screen shows), `stood` (the transcript's last word on the
/// login is the wall: every turn the wall answers sets it, the person's
/// `Login successful` or an answer of the model clears it), `back` (the
/// screen says `Login successful`). The supervisor: `track` (its lost
/// login's track: `/login` typed), `told` (the owner told of it). The
/// upgrade: `phase` 0 pending, 1 announced, 2 gave up, 3 restarted; `asks`
/// counted to `MaxAsks` (the real `MAX_ASKS` scaled down); `window` (the
/// re-ask window has run out); `unread` (the latest notice's own turn was the
/// wall's: it never reached the model); `got` (asks whose notice the model
/// received); `dark` (the wall stood in the running window); `holding` (the
/// agent read a notice and winds down); `ready` (its READY is its last
/// word); `inherited` (the state is a build's before the fix, `Inherit`).
/// Four ghosts: `blind`, the upgrade typed where the wall showed or stood;
/// `spent`, a give-up with fewer than `MaxAsks` notices received, or over a
/// window the wall darkened; `futile`, a continuation typed into a login the
/// loop saw gone and has not seen back; `untold`, the supervisor acted at a
/// wall before the owner was told of it.
///
/// `Inherit` is the state a build before the fix left — it gave up on
/// `MaxAsks` notices the wall answered (the owner's `failed:unanswered`) —
/// which the fixed build reads and takes back (`Announce` from phase 2).
/// Outside the model: the upgrade's ownership of the turn ends after an
/// announcement (the supervisor types nothing there; here it may), and the
/// release line an abandoned notice owes (`harness_upgrade_never_strands_model`).
///
/// Properties: `NoNoticeAtTheWall`, `NoGiveUpUnread`, `NoFutileContinue`,
/// `TheOwnerIsToldFirst`. ONE KNOB PER DEFECT, each caught on its own:
/// `NoGate` (the upgrade's gate misses the wall: the reader's error row, the
/// transcript's word), `NoRefund` (a notice the wall answered is counted and
/// given up on), `ClockAtWall` (the re-ask window runs at the wall and the
/// lift does not start it again), `NoSee` (the supervisor's reader misses the
/// wall), `NoHold` (a lost login is continued once its row leaves the
/// screen). `Buggy = 1` is 0.93.0 and main before the fix, all five at once:
/// the incident — notices typed into the wall and spent into a give-up, the
/// supervisor's continuations into it, nobody told. Tier-0 in aterm-spec's
/// `derived_harness_login_wall`; Tier-1 in aterm-agent's
/// `conformance_login_wall`, over the real readers, reducer and turn-end
/// decider on every reachable state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_login_wall_model() -> Model {
    crate::ty_model! {
        HarnessLoginWall {
            const Buggy = 0;
            const NoGate = 0;
            const NoRefund = 0;
            const ClockAtWall = 0;
            const NoSee = 0;
            const NoHold = 0;
            const MaxAsks = 2;
            var login = 1;
            var wall = 0;
            var stood = 0;
            var back = 0;
            var track = 0;
            var told = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var unread = 0;
            var got = 0;
            var dark = 0;
            var holding = 0;
            var ready = 0;
            var inherited = 0;
            var blind = 0;
            var spent = 0;
            var futile = 0;
            var untold = 0;

            // The account and the session, as they please: the login goes;
            // a turn someone else began (a background completion, a /loop
            // wakeup) is answered by the wall; the person's `/login` is done
            // (`Login successful`), and the upgrade's window starts again
            // from the lift (`ClockAtWall`: it does not); a `/login` dialog
            // is dismissed with no login, its wall's row now history.
            action Expire when (login == 1) {
                login = 0;
            }
            action WallHit when (login == 0 && wall == 0) {
                wall = 1;
                stood = 1;
                back = 0;
                dark = if (phase == 1 && window == 0) { 1 } else { dark };
            }
            action LoginBack when (login == 0 && stood == 1) {
                login = 1;
                wall = 0;
                stood = 0;
                back = 1;
                window = if (phase == 1 && Buggy == 0 && ClockAtWall == 0) { 0 } else { window };
                dark = if (Buggy == 0 && ClockAtWall == 0) { 0 } else { dark };
            }
            action Dismiss when (track == 1 && wall == 1 && login == 0) {
                wall = 0;
            }
            // A build before the fix gave up on every notice, each answered
            // by the wall.
            action Inherit when (phase == 0 && asks == 0 && inherited == 0 && stood == 1) {
                phase = 2;
                asks = MaxAsks;
                unread = 1;
                window = 1;
                inherited = 1;
            }

            // The supervisor. At a point it reads the wall at: `/login`,
            // once, the owner told as it is typed.
            action TypeLogin when (wall == 1 && track == 0 && Buggy == 0 && NoSee == 0) {
                track = 1;
                told = 1;
            }
            // At a point it reads no wall at, a continuation — never while a
            // lost login's track stands and the screen does not say it is
            // back (`NoHold`: it does). Answered by the model: the login is
            // back, the track ends, the notices typed are read. Answered by
            // the wall: its row, and the transcript's word.
            action Continue when (
                (wall == 0 || Buggy == 1 || NoSee == 1) &&
                (track == 0 || back == 1 || Buggy == 1 || NoHold == 1)
            ) {
                futile = if (login == 0 && stood == 1) { 1 } else { futile };
                untold = if (stood == 1 && told == 0) { 1 } else { untold };
                wall = if (login == 1) { 0 } else { 1 };
                stood = if (login == 1) { 0 } else { 1 };
                back = 0;
                track = if (login == 1) { 0 } else { track };
                told = if (login == 1) { 0 } else { told };
                holding = if (login == 1 && (phase == 1 || phase == 2)) { 1 } else { holding };
                dark = if (login == 0 && phase == 1 && window == 0) { 1 } else { dark };
            }

            // The upgrade. A notice: never where the wall shows or stands
            // (`NoGate`: it is); a notice the wall answered is typed again
            // as the same ask, and a give-up spent on such notices is taken
            // back — its next notice the next ask the model has not had
            // (`NoRefund`: neither). Typed with the login gone, its own turn
            // is the wall's.
            action Announce when (
                ready == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1) &&
                (phase == 0 ||
                    (phase == 1 &&
                        ((unread == 1 && Buggy == 0 && NoRefund == 0) ||
                            (window == 1 && asks <= MaxAsks - 1))) ||
                    (phase == 2 && unread == 1 && Buggy == 0 && NoRefund == 0))
            ) {
                blind = if (wall == 1 || stood == 1) { 1 } else { blind };
                asks = if (phase == 1 && unread == 1 && Buggy == 0 && NoRefund == 0) {
                    asks
                } else if (phase == 2) {
                    got + 1
                } else {
                    asks + 1
                };
                got = if (login == 1) { got + 1 } else { got };
                unread = if (login == 1) { 0 } else { 1 };
                holding = if (login == 1) { 1 } else { holding };
                wall = if (login == 1) { wall } else { 1 };
                stood = if (login == 1) { stood } else { 1 };
                back = if (login == 1) { back } else { 0 };
                phase = 1;
                window = 0;
                dark = 0;
            }
            // The re-ask window runs only where the agent can answer.
            action Elapse when (
                phase == 1 && window == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || ClockAtWall == 1)
            ) {
                window = 1;
            }
            action GiveUp when (
                phase == 1 && window == 1 && asks == MaxAsks && ready == 0 &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1) &&
                (unread == 0 || Buggy == 1 || NoRefund == 1)
            ) {
                phase = 2;
                spent = if (got <= MaxAsks - 1 || dark == 1) { 1 } else { spent };
            }
            // The agent answers READY (its own turn: the login must hold),
            // and the restart acts on it — an announced upgrade's, or the
            // late answer one that gave up still hears — never into the
            // wall.
            action AgentReady when (
                holding == 1 && login == 1 && ready == 0 && (phase == 1 || phase == 2)
            ) {
                ready = 1;
            }
            action Restart when (
                ready == 1 && (phase == 1 || phase == 2) &&
                ((wall == 0 && stood == 0) || Buggy == 1 || NoGate == 1)
            ) {
                blind = if (wall == 1 || stood == 1) { 1 } else { blind };
                phase = 3;
                ready = 0;
                holding = 0;
            }

            invariant NoNoticeAtTheWall: blind == 0;
            invariant NoGiveUpUnread: spent == 0;
            invariant NoFutileContinue: futile == 0;
            invariant TheOwnerIsToldFirst: untold == 0;
        }
    }
}

/// THE NOTICE QUEUED BEHIND A USAGE LIMIT (`aterm_agent::harness::upgrade`:
/// `notice_scan`, `queued_until`, `queued_copies`, `Scan::rests_until`,
/// `queue_rest`, `transcript_limit_until`, `next_step`, `requested_step`,
/// `announce_asks`, `clock_held`; the driver's `queue_facts`). The incident it
/// is written for (the owner's report of 2026-09-27, tab
/// `s-c543f4e0edd3439e5791`, Claude Code 2.1.280): the session stood at its
/// weekly limit; the upgrade typed four notices half an hour apart — each
/// answered within two seconds by Claude Code's `You've hit your weekly limit`
/// row, and each read as ASKED, the limit's banner no longer on the screen —
/// gave up on them (`no READY answer after 4 notices`), and when the session
/// went on all four reached the agent at once.
///
/// The account: `limited` (it stands at its usage limit — no reader sees
/// this directly). What the upgrade reads of it: `shown` (the screen shows the
/// limit; a person's Esc or a recogniser that misses it clears the screen
/// while the limit stands), `held` (the transcript's word that the limit
/// holds: the limit row that last answered the latest notice — or, with
/// nothing queued, the session's last row — its named reset ahead or, naming
/// none, written less than `REASK_S` ago). The conversation: `untaken`
/// (notices in it the model has not taken, each answered by the limit; they
/// reach the model together, with the next turn that gets past the limit),
/// counted to `RetypeMax + 1 + Daily`, which stands for that many OR MORE —
/// a queue is FULL past `RetypeMax`, and its rest has grown to a day at the
/// top; `took` (the asks the model took). The upgrade: `phase` 0 pending, 1
/// announced, 2 gave up; `asks` counted to `MaxAsks`; `window` (the latest
/// ask's re-ask window has run out); `rested` (a full queue has rested since
/// the transcript's word on its latest notice ran out: 1, a rest shorter than
/// a day; 2, a day). Five ghosts: `stacked`, a fresh ask typed while a notice
/// waited untaken; `burst`, a notice typed into a full queue before it
/// rested; `hurried`, the notices typed into a full queue, the limit still
/// standing, without a day's rest before them (saturating at `Daily + 1`);
/// `spent`, a give-up before `MaxAsks` asks the model took; `stuck`, the
/// upgrade's quiet word where it still owed the agent an ask it could read.
///
/// `Retype` is the same ask typed again: its latest copy was queued and the
/// limit is over by the transcript's word and the screen's (a named reset
/// passed, a `/login` finished, `REASK_S` since a row naming none) — which,
/// for a row naming no reset, it is every half hour however long the limit
/// really stands. So it is BOUNDED: straight away only while at most
/// `RetypeMax` (the real `REQUEUE_MAX`) notices wait untaken; into a full
/// queue only once it has RESTED (`Rests`, the passing of the real
/// `queue_rest`) — one copy per rest, never the half-hourly pile — and the
/// rest GROWS with every copy unread (the owner, 2026-09-27: a limit that
/// names no reset and lasts for days still added a copy every two and a half
/// hours, about ten a day): shorter than a day for the first `Daily` (the
/// real `QUEUE_REST_DOUBLINGS`: two, four, eight, sixteen hours), a day for
/// every one after. `NowAsks` is the owner's `Upgrade now` on a full queue:
/// one copy more, the person asking — never while the limit shows on the
/// screen or holds by the transcript's word. `GoesOn` is a turn someone else
/// began getting past the limit. `Look` is the upgrade's QUIET WORD at a
/// point where the limit is over by every word, DERIVED FROM ITS OWN GUARDS:
/// enabled exactly where none of `Announce`, `Retype`, `GiveUp`, `Elapse`
/// and `Rests` is, which the Tier-0 test checks state by state. One round is
/// modelled: a stopped round's rest and new round (main's "no stop is for
/// good") are `HarnessUpgradeNeverStrands`', and so are the READY and the
/// restart. A notice an EARLIER round, target or build left queued (its
/// marker no longer the state's) is read by the real code as this queue is —
/// held while its limit holds by the transcript's word, and counted toward
/// the same bound and rest — which a new round's first notice then joins;
/// that crossing of rounds is not modelled.
///
/// Properties: `NeverTwoAsksUntaken` — no ask is typed behind a notice the
/// model has not taken (the copies of ONE ask are the retype's, bounded by
/// the next); `CopiesBounded` — at most `RetypeMax + 1` notices wait untaken
/// before a rest, and past that one more per rest (`burst`);
/// `CopiesPerDayBounded` — however long the limit stands, at most `Daily`
/// notices go into a full queue without a day's rest before them (`hurried`),
/// so a limit that lasts for days adds `1 + RetypeMax + Daily` notices at the
/// most before its rests reach a day, and then one a day, never one every
/// rest of the first; `NoGiveUpUntaken` — a give-up only after `MaxAsks` asks
/// the model took; `NeverStranded` — once the limit is over by every word,
/// the upgrade is never quiet while it owes the agent a readable ask:
/// announced, it always has a next act of its own (a notice, a copy, its
/// window, its give-up, a full queue's rest — no waiting on anyone's turn),
/// and a round never gives up before the model took `MaxAsks` (it would never
/// ask again). ONE KNOB PER DEFECT, each caught on its own: `CountRetype` (a
/// notice typed again spends an ask, and at the bound is given up on:
/// `NoGiveUpUntaken`), `Unbounded` (a queued notice is typed again every time
/// its limit is over by the transcript's word, full queue or not — this
/// branch before the bound: `CopiesBounded`), `NoBound` (a queued notice
/// holds the upgrade until the model takes it, whatever the transcript says
/// — the first fix before its review, which left an idle session waiting for
/// a person: `NeverStranded`), `ForGood` (a full queue waits for the model
/// with no rest — b77f7c28e before its review: an idle session whose limit
/// had long ended waited for good, and the owner was told it "moves once that
/// ends": `NeverStranded`), `Flat` (every rest of a full queue as long as the
/// first, whatever it holds unread — d5cc7f01c, a copy every two and a half
/// hours for as long as the limit stands: `CopiesPerDayBounded`). `Buggy = 1`
/// is a COMPOSITE of two builds, so that the ratchet, which walks `Buggy = 1`
/// alone, sees every invariant caught: the owner's build (0.93.0, and main's
/// gate that 0.94.0 shipped) — the limit read off the screen alone, a notice
/// the limit answered read as asked and its window run from its typing: four
/// notices stacked behind the limit, a give-up on asks the model never had,
/// and nothing left to ask it once the limit ended — PLUS d5cc7f01c's flat
/// rest, a full queue retyped after every rest as short as the first. The
/// owner's build never did the second part (it had no queue to rest, and
/// gave up after four), so `CopiesPerDayBounded` is not its defect: the walk
/// of the owner's report as it ran breaks the other four, and `Flat` is the
/// knob that stands for the flat rest on its own. Tier-0 in aterm-spec's
/// `derived_harness_upgrade_limit_queue`; Tier-1 in aterm-agent's
/// `harness::upgrade_drive` tests (`upgrade_queued_tests.rs`), over the real
/// screen reader, transcript readers, `queue_facts`, clock, reducer (with and
/// without the owner's `--now`), ask count and rest on every reachable state
/// and several transcripts each.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_limit_queue_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeLimitQueue {
            const Buggy = 0;
            const CountRetype = 0;
            const Unbounded = 0;
            const NoBound = 0;
            const ForGood = 0;
            const Flat = 0;
            const MaxAsks = 4;
            const RetypeMax = 2;
            const Daily = 4;
            var limited = 0;
            var shown = 0;
            var held = 0;
            var phase = 0;
            var asks = 0;
            var window = 0;
            var untaken = 0;
            var took = 0;
            var rested = 0;
            var stacked = 0;
            var burst = 0;
            var hurried = 0;
            var spent = 0;
            var stuck = 0;

            // The account and the session, as they please. The agent's own
            // turn meets the limit: the screen shows it, and its row holds the
            // transcript's word — until the reset it names or, naming none,
            // for `REASK_S`; an announced upgrade's clock is held from here.
            action LimitHits when (limited == 0 && untaken == 0 && phase <= 1) {
                limited = 1;
                shown = 1;
                held = 1;
                window = 0;
            }
            action LimitEnds when (limited == 1) {
                limited = 0;
            }
            // The transcript's word runs out: the named reset passes, a
            // `/login` finishes, or `REASK_S` since a row naming none —
            // whether or not the limit really ended.
            action BoundPasses when (held == 1) {
                held = 0;
            }
            // The banner leaves the screen: a person's Esc, a recogniser that
            // misses it — whether or not the limit really ended.
            action Clear when (shown == 1) {
                shown = 0;
            }
            // A turn someone else began gets past the limit (Claude Code's own
            // at the reset, a person's, a peer's): the model writes a row,
            // taking every notice waiting at once — one ask it had — and the
            // latest ask's window opens then (`Buggy`: it ran from the typing).
            action GoesOn when (limited == 0 && untaken > 0) {
                untaken = 0;
                took = took + 1;
                shown = 0;
                held = 0;
                rested = 0;
                hurried = 0;
                window = if Buggy == 1 { window } else { 0 };
            }
            // The owner's `Upgrade now` on a FULL queue: one copy more — the
            // person asking — never where the screen shows the limit or the
            // transcript's word holds it. Its own turn: read, or queued, one
            // more unread (the next rest the longer for it).
            action NowAsks when (
                Buggy == 0 && phase == 1 && untaken > RetypeMax && shown == 0 && held == 0
            ) {
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                hurried = if limited == 1 { hurried } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }

            // The upgrade. It reads a limit where the screen shows one or the
            // transcript's word holds, and a FULL queue — more than
            // `RetypeMax` notices untaken — until it has rested (`Unbounded`:
            // never full; `NoBound`: any queue holds; `Buggy`: the screen
            // alone). A FRESH ASK: the first notice, or the next once the
            // window of one the model took has run out — never behind a notice
            // waiting untaken (`Buggy`: a queued notice read as asked). Its own
            // turn: read by the model, or, the account at its limit, answered
            // by the limit and queued.
            action Announce when (
                shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0))) &&
                (untaken == 0 || Buggy == 1) &&
                (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
            ) {
                stacked = if untaken > 0 { 1 } else { stacked };
                burst = if untaken > RetypeMax && rested == 0 { 1 } else { burst };
                hurried = if (limited == 1 && untaken > RetypeMax && rested <= 1) {
                    if hurried > Daily { hurried } else { hurried + 1 }
                } else if (limited == 1) { hurried } else { 0 };
                phase = 1;
                asks = asks + 1;
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }
            // THE SAME ASK AGAIN: its latest copy was the limit's, and that
            // limit is over by the transcript's word and the screen's —
            // straight away while the queue has room, into a full one only
            // once it has rested (`Unbounded`: no bound; `ForGood`: a full
            // queue never; `CountRetype`: it spends an ask; `Buggy`: only
            // into a full queue that rested, the rest d5cc7f01c gave it).
            action Retype when (
                phase == 1 && untaken > 0 && shown == 0 && held == 0 &&
                (Buggy == 0 || (untaken > RetypeMax && rested > 0)) &&
                (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                NoBound == 0 &&
                (CountRetype == 0 || asks <= MaxAsks - 1)
            ) {
                burst = if untaken > RetypeMax && rested == 0 { 1 } else { burst };
                hurried = if (limited == 1 && untaken > RetypeMax && rested <= 1) {
                    if hurried > Daily { hurried } else { hurried + 1 }
                } else if (limited == 1) { hurried } else { 0 };
                asks = if CountRetype == 1 { asks + 1 } else { asks };
                window = 0;
                took = if limited == 0 { took + 1 } else { took };
                untaken = if limited == 1 {
                    if untaken > RetypeMax + Daily { untaken } else { untaken + 1 }
                } else { 0 };
                shown = limited;
                held = limited;
                rested = 0;
            }
            // A FULL queue rests after the transcript's word on its latest
            // notice ran out: shorter than a day while it holds at most
            // `RetypeMax + Daily` notices unread, a day once it holds more
            // (`Flat`, `Buggy`: every rest the first one's).
            action Rests when (phase == 1 && untaken > RetypeMax && held == 0 && rested == 0) {
                rested = if (Flat == 0 && Buggy == 0 && untaken > RetypeMax + Daily) { 2 } else { 1 };
            }
            // The re-ask window runs only where the upgrade reads no limit
            // and no full queue.
            action Elapse when (
                phase == 1 && window == 0 && shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0)))
            ) {
                window = 1;
            }
            // Its asks spent and the last one's window run out, nothing
            // waiting untaken: it gives up (`CountRetype`: a queued notice's
            // re-type at the bound is the give-up).
            action GiveUp when (
                phase == 1 && asks == MaxAsks && shown == 0 &&
                (Buggy == 1 ||
                    (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                        (untaken == 0 || NoBound == 0))) &&
                ((window == 1 && (untaken == 0 || Buggy == 1)) ||
                    (CountRetype == 1 && Buggy == 0 && untaken > 0))
            ) {
                phase = 2;
                spent = if took <= MaxAsks - 1 { 1 } else { spent };
            }
            // THE QUIET WORD where the limit is over by every word — the
            // account's, the screen's, the transcript's: nothing the upgrade
            // would do by itself now, its own guards negated. A strand where
            // it still owes the agent an ask it could read: an announced
            // round with nothing of its own left to do (only someone else's
            // turn would move it), or a round given up before `MaxAsks` the
            // model took.
            action Look when (
                phase > 0 && limited == 0 && shown == 0 && held == 0 && stuck == 0 &&
                (if (
                    shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0))) &&
                    (untaken == 0 || Buggy == 1) &&
                    (phase == 0 || (phase == 1 && window == 1 && asks <= MaxAsks - 1))
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && untaken > 0 && shown == 0 && held == 0 &&
                    (Buggy == 0 || (untaken > RetypeMax && rested > 0)) &&
                    (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                    NoBound == 0 &&
                    (CountRetype == 0 || asks <= MaxAsks - 1)
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && untaken > RetypeMax && held == 0 && rested == 0
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && window == 0 && shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0)))
                ) { 1 } else { 0 }) +
                (if (
                    phase == 1 && asks == MaxAsks && shown == 0 &&
                    (Buggy == 1 ||
                        (held == 0 && (untaken <= RetypeMax || (rested > 0 && ForGood == 0) || Unbounded == 1) &&
                            (untaken == 0 || NoBound == 0))) &&
                    ((window == 1 && (untaken == 0 || Buggy == 1)) ||
                        (CountRetype == 1 && Buggy == 0 && untaken > 0))
                ) { 1 } else { 0 }) == 0
            ) {
                stuck = if (phase == 1 || (phase == 2 && took <= MaxAsks - 1)) { 1 } else { 0 };
            }

            invariant NeverTwoAsksUntaken: stacked == 0;
            invariant CopiesBounded: burst == 0;
            invariant CopiesPerDayBounded: hurried <= Daily;
            invariant NoGiveUpUntaken: spent == 0;
            invariant NeverStranded: stuck == 0;
        }
    }
}

/// THE MODEL PRIORITY LIST (`harness::upgrade_models`): a WRITER/READER
/// pair, the shape of `NativeUpdateFailedMarkSuppression`. The writers are
/// `Priority::admit` — the automatic insertion of a model Claude Code
/// recommends — and the owner's `models set`; the reader is target selection
/// (`upgrade_models::target`, which the live upgrade's model half reads once per sweep), which relaunches
/// a session onto the FIRST available entry. What a writer puts on the list
/// must mean to the reader exactly what the owner's order meant, and nothing
/// the owner did not ask for.
///
/// The list is a map from id to position, best first, `0` meaning absent:
/// the owner's three are `a1` = `claude-opus-5`, `a2` = `claude-opus-5-5` and
/// `b1` = `claude-fable-5-1` (always listed; `hord` names which of three
/// owner orders was last set, `0` being the seed), a recommendation may bring
/// `a3` = `claude-opus-5-6` and `b2` = `claude-fable-5-2`, and three
/// recommendations must never land: `a0` = `claude-opus-4-8` (older than every
/// listed Opus), `ah` = `claude-opus-5-1` (newer than `claude-opus-5`, older
/// than `claude-opus-5-5`) and `c1` = `claude-sonnet-6` (a family the owner
/// never listed). `k_a3` / `k_b2` are the active build's catalog knowing the
/// two newcomers (builds move forward, so knowledge only grows); `av_*` is
/// each id's availability, the reader's whole input besides the order; `pick`
/// is the position the reader chose, valid while `sel == 1` and dropped by
/// every change to the list or to availability. The Tier-1 bind drives the
/// real `admit`, `render`/`parse` and `target` over EVERY reachable state of
/// this machine and projects their answers back onto these variables.
///
/// `Buggy = 1` arms one mutant per law, each an action whose healthy branch
/// changes nothing (the real `admit` answers `None` there): a downgrade
/// admitted (`OfferDowngrade`), the pre-fix newest check that compared only
/// with the family's best-RANKED member and so extended a hand-ordered list
/// downward (`OfferBelowNewest`), a family the owner never listed
/// (`OfferCrossFamily`), an id the build does not know (`AdmitUnknown`), an
/// insertion at the head instead of above the family's best
/// (`AdmitAtTheTop`), an insertion that also re-sorts the family newest-first
/// and so rewrites the owner's order (`AdmitAndResort`), and a reader that
/// answers the head of the list whether or not it is available
/// (`SelectHead`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_priority_model() -> Model {
    crate::ty_model! {
        HarnessModelPriority {
            const Buggy = 0;

            // The seed, verbatim order: claude-opus-5-5, claude-fable-5-1,
            // claude-opus-5.
            var p_a1 = 3;
            var p_a2 = 1;
            var p_b1 = 2;
            var p_a3 = 0;
            var p_b2 = 0;
            var p_a0 = 0;
            var p_ah = 0;
            var p_c1 = 0;
            var hord = 0;
            var k_a3 = 0;
            var k_b2 = 0;
            var av_a1 = 0;
            var av_a2 = 0;
            var av_a3 = 0;
            var av_b1 = 0;
            var av_b2 = 0;
            var sel = 0;
            var pick = 0;
            var forged = 0;

            // -- the environment ------------------------------------------

            action BuildLearnsNewerOpus when (k_a3 == 0) {
                k_a3 = 1;
            }
            action BuildLearnsNewerFable when (k_b2 == 0) {
                k_b2 = 1;
            }
            action ToggleA1 {
                av_a1 = 1 - av_a1;
                sel = 0;
                pick = 0;
            }
            action ToggleA2 {
                av_a2 = 1 - av_a2;
                sel = 0;
                pick = 0;
            }
            action ToggleA3 {
                av_a3 = 1 - av_a3;
                sel = 0;
                pick = 0;
            }
            action ToggleB1 {
                av_b1 = 1 - av_b1;
                sel = 0;
                pick = 0;
            }
            action ToggleB2 {
                av_b2 = 1 - av_b2;
                sel = 0;
                pick = 0;
            }

            // -- the owner (`models set`): the whole list, verbatim --------

            // The seed order again: claude-opus-5-5, claude-fable-5-1, claude-opus-5.
            action HumanSetsTheSeed {
                p_a1 = 3;
                p_a2 = 1;
                p_b1 = 2;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 0;
                sel = 0;
                pick = 0;
            }
            // The older Opus ranked first — the order the newest-listed check exists for.
            action HumanSetsOlderOpusFirst {
                p_a1 = 1;
                p_a2 = 2;
                p_b1 = 3;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 1;
                sel = 0;
                pick = 0;
            }
            // Another family ranked above every Opus.
            action HumanSetsFableFirst {
                p_a1 = 2;
                p_a2 = 3;
                p_b1 = 1;
                p_a3 = 0;
                p_b2 = 0;
                p_a0 = 0;
                p_ah = 0;
                p_c1 = 0;
                hord = 2;
                sel = 0;
                pick = 0;
            }

            // -- the automatic writer (`Priority::admit`) --------------------

            // Strictly newer than every listed Opus, known to the build:
            // directly ABOVE the family's best-ranked member.
            action AdmitNewerOpus when (p_a3 == 0 && k_a3 == 1) {
                p_a3 = (if p_a1 <= p_a2 { p_a1 } else { p_a2 });
                p_a1 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if p_a0 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = 0;
                pick = 0;
            }
            action AdmitNewerFable when (p_b2 == 0 && k_b2 == 1) {
                p_b2 = p_b1;
                p_a1 = if p_b1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if p_b1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if p_b1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if p_a3 > 0 && p_b1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_a0 = if p_a0 > 0 && p_b1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if p_ah > 0 && p_b1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if p_c1 > 0 && p_b1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = 0;
                pick = 0;
            }

            // -- the reader (`models::target`) ------------------------------

            // The FIRST available entry, in the list's order.
            action Select when (sel == 0) {
                pick = if (p_a1 == 1 && av_a1 == 1) ||
                    (p_a2 == 1 && av_a2 == 1) ||
                    (p_a3 == 1 && av_a3 == 1) ||
                    (p_b1 == 1 && av_b1 == 1) ||
                    (p_b2 == 1 && av_b2 == 1) {
                    1
                } else if (p_a1 == 2 && av_a1 == 1) ||
                    (p_a2 == 2 && av_a2 == 1) ||
                    (p_a3 == 2 && av_a3 == 1) ||
                    (p_b1 == 2 && av_b1 == 1) ||
                    (p_b2 == 2 && av_b2 == 1) {
                    2
                } else if (p_a1 == 3 && av_a1 == 1) ||
                    (p_a2 == 3 && av_a2 == 1) ||
                    (p_a3 == 3 && av_a3 == 1) ||
                    (p_b1 == 3 && av_b1 == 1) ||
                    (p_b2 == 3 && av_b2 == 1) {
                    3
                } else if (p_a1 == 4 && av_a1 == 1) ||
                    (p_a2 == 4 && av_a2 == 1) ||
                    (p_a3 == 4 && av_a3 == 1) ||
                    (p_b1 == 4 && av_b1 == 1) ||
                    (p_b2 == 4 && av_b2 == 1) {
                    4
                } else if (p_a1 == 5 && av_a1 == 1) ||
                    (p_a2 == 5 && av_a2 == 1) ||
                    (p_a3 == 5 && av_a3 == 1) ||
                    (p_b1 == 5 && av_b1 == 1) ||
                    (p_b2 == 5 && av_b2 == 1) {
                    5
                } else {
                    0
                };
                sel = 1;
            }

            // -- the defects; every healthy branch changes nothing ----------

            action OfferDowngrade when (p_a0 == 0 && forged == 0) {
                p_a0 = if Buggy == 1 { (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) } else { p_a0 };
                p_a1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_ah = if Buggy == 1 && p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The pre-fix check: newer than the best-RANKED Opus only.
            action OfferBelowNewest when (p_ah == 0 && p_a1 <= p_a2 && forged == 0) {
                p_ah = if Buggy == 1 { p_a1 } else { p_ah };
                p_a1 = if Buggy == 1 && p_a1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && p_a1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && p_a1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && p_a1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && p_a1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && p_a1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_c1 = if Buggy == 1 && p_c1 > 0 && p_a1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action OfferCrossFamily when (p_c1 == 0 && forged == 0) {
                p_c1 = if Buggy == 1 { 1 } else { p_c1 };
                p_a1 = if Buggy == 1 && 1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && 1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && 1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_a3 = if Buggy == 1 && p_a3 > 0 && 1 <= p_a3 { p_a3 + 1 } else { p_a3 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && 1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && 1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && 1 <= p_ah { p_ah + 1 } else { p_ah };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action AdmitUnknown when (p_a3 == 0 && k_a3 == 0 && forged == 0) {
                p_a3 = if Buggy == 1 { (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) } else { p_a3 };
                p_a1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && (if p_a1 <= p_a2 { p_a1 } else { p_a2 }) <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action AdmitAtTheTop when (p_a3 == 0 && k_a3 == 1 && forged == 0) {
                p_a3 = if Buggy == 1 { 1 } else { p_a3 };
                p_a1 = if Buggy == 1 && 1 <= p_a1 { p_a1 + 1 } else { p_a1 };
                p_a2 = if Buggy == 1 && 1 <= p_a2 { p_a2 + 1 } else { p_a2 };
                p_b1 = if Buggy == 1 && 1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && 1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && 1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && 1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && 1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // Insert above the best AND sort the family newest-first.
            action AdmitAndResort when (
                p_a3 == 0 && k_a3 == 1 && p_a1 <= p_a2 && forged == 0
            ) {
                p_a3 = if Buggy == 1 { p_a1 } else { p_a3 };
                p_a2 = if Buggy == 1 { p_a1 + 1 } else { p_a2 };
                p_a1 = if Buggy == 1 { p_a2 + 1 } else { p_a1 };
                p_b1 = if Buggy == 1 && p_a1 <= p_b1 { p_b1 + 1 } else { p_b1 };
                p_b2 = if Buggy == 1 && p_b2 > 0 && p_a1 <= p_b2 { p_b2 + 1 } else { p_b2 };
                p_a0 = if Buggy == 1 && p_a0 > 0 && p_a1 <= p_a0 { p_a0 + 1 } else { p_a0 };
                p_ah = if Buggy == 1 && p_ah > 0 && p_a1 <= p_ah { p_ah + 1 } else { p_ah };
                p_c1 = if Buggy == 1 && p_c1 > 0 && p_a1 <= p_c1 { p_c1 + 1 } else { p_c1 };
                sel = if Buggy == 1 { 0 } else { sel };
                pick = if Buggy == 1 { 0 } else { pick };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SelectHead when (sel == 0 && forged == 0) {
                pick = if Buggy == 1 { 1 } else { pick };
                sel = if Buggy == 1 { 1 } else { sel };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // Nothing older than the family's best-ranked member is admitted.
            invariant AutoInsertIsNewerThanTheFamilysBest: p_a0 == 0;
            // Nothing older than the family's NEWEST listed member either:
            // a hand-ordered list is never extended downward.
            invariant NeverExtendedDownward: p_ah == 0;
            invariant AutoInsertIsSameFamily: p_c1 == 0;
            invariant AutoInsertIsKnownToTheBuild:
                (p_a3 == 0 || k_a3 == 1) && (p_b2 == 0 || k_b2 == 1);
            invariant AutoInsertSitsDirectlyAboveTheFamilysBest:
                (p_a3 == 0 || p_a3 + 1 == (if p_a1 <= p_a2 { p_a1 } else { p_a2 })) &&
                (p_b2 == 0 || p_b2 + 1 == p_b1);
            // The owner's three keep the owner's order, whatever was inserted.
            invariant HumanOrderIsNeverRewritten:
                if hord == 0 {
                    p_a2 + 1 <= p_b1 && p_b1 + 1 <= p_a1
                } else if hord == 1 {
                    p_a1 + 1 <= p_a2 && p_a2 + 1 <= p_b1
                } else {
                    p_b1 + 1 <= p_a1 && p_a1 + 1 <= p_a2
                };
            // The pick is available and nothing available ranks above it.
            invariant ReaderPicksTheFirstAvailable:
                sel == 0 || (
                    (pick == 0 ||
                        (p_a1 == pick && av_a1 == 1) ||
                        (p_a2 == pick && av_a2 == 1) ||
                        (p_a3 == pick && av_a3 == 1) ||
                        (p_b1 == pick && av_b1 == 1) ||
                        (p_b2 == pick && av_b2 == 1)) &&
                    (p_a1 == 0 || av_a1 == 0 || (pick > 0 && pick <= p_a1)) &&
                    (p_a2 == 0 || av_a2 == 0 || (pick > 0 && pick <= p_a2)) &&
                    (p_a3 == 0 || av_a3 == 0 || (pick > 0 && pick <= p_a3)) &&
                    (p_b1 == 0 || av_b1 == 0 || (pick > 0 && pick <= p_b1)) &&
                    (p_b2 == 0 || av_b2 == 0 || (pick > 0 && pick <= p_b2))
                );
        }
    }
}

/// **THE MODEL LADDER** — WHEN the live upgrade takes a DUE model move
/// (`aterm_agent::harness::upgrade_models::model_moves_now`; WHICH model is
/// [`harness_model_priority_model`]'s).
///
/// The incident (2026-09-25): a warm conversation on `claude-opus-5` was
/// restarted 2.1.282 -> 2.1.283 while the managed 2.1.283 offered
/// `claude-opus-5-5`, and came back on Opus 5. The rule then took a due move
/// only when the prompt cache was COLD — an hour without an answer — which a
/// conversation in active use never reaches, so the move waited for as long as
/// anyone used the session. The owner had to type `/model`.
///
/// The ladder, every due move landing: move at once when `cold`; move at once
/// when a newer BUILD `restart`s the session anyway (the model rides it); else
/// wait, at most `Warm` readable visits of being due, then move. `clock` is
/// those visits. A visit that cannot read the live model (`unknown`: a
/// transcript tail with no answer in it) decides nothing and LEAVES the clock
/// — an unknown read that reset it would restart the bound on every flicker.
///
/// `NeverPastTheBound` is the landing law as a safety property: while the move
/// is due and readable, no visit waits past `Warm`. The environment may keep
/// the session warm forever (`Answer`) and may never let it go quiet — that is
/// exactly the case the law must hold in. `Buggy = 1` is the incident's rule
/// (cold is a CONDITION, so a warm session waits every visit) and walks `clock`
/// past the bound.
///
/// Tier-1: `aterm-agent/tests/conformance_upgrade_models/ladder.rs` drives the
/// REAL `model_moves_now` over every reachable state and requires
/// `VisitMoves` to be enabled exactly where it answers a move, with the
/// pre-fix rule as the caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_ladder_model() -> Model {
    crate::ty_model! {
        HarnessModelLadder {
            const Buggy = 0;
            // MODEL_WARM_MAX_S, in readable visits of being due.
            const Warm = 3;
            var cold = 0;
            var restart = 0;
            var unknown = 0;
            var clock = 0;
            var moved = 0;

            // -- the environment ------------------------------------------
            // An answer keeps (or makes) the cache warm; nothing stops a
            // session in active use from answering forever.
            action Answer when (moved == 0) {
                cold = 0;
            }
            action GoQuiet when (moved == 0 && cold == 0) {
                cold = 1;
            }
            // A newer Claude Code build is installed: the session will be
            // restarted onto it whatever the model does.
            action BuildArrives when (moved == 0 && restart == 0) {
                restart = 1;
            }
            action Flicker when (moved == 0 && unknown == 0) {
                unknown = 1;
            }
            action Readable when (moved == 0 && unknown == 1) {
                unknown = 0;
            }

            // -- the harness's visit (readable only) -----------------------
            action VisitMoves when (
                moved == 0 && unknown == 0 &&
                (cold == 1 || (Buggy == 0 && (restart == 1 || clock > Warm - 1)))
            ) {
                moved = 1;
            }
            action VisitWaits when (
                moved == 0 && unknown == 0 && cold == 0 && clock <= Warm &&
                (Buggy == 1 || (restart == 0 && clock <= Warm - 1))
            ) {
                clock = clock + 1;
            }

            invariant NeverPastTheBound: clock <= Warm;
        }
    }
}

/// THE MODEL SWITCH (`harness::upgrade_models` + `harness::upgrade_drive`):
/// which model a conversation is moved to, when, and what the harness
/// remembers of it — the rule, the ladder, the announcement and the settle
/// step as ONE lifecycle for one conversation.
///
/// THE RULE it transcribes is `model_due` as it stands since the owner's
/// decision of 2026-09-27, *"SAME FAMILY FIRST, THEN THE PRIORITY LIST"*,
/// over the build's offer (`upgrade_models::offered`). On this projection its
/// moves are the ones the list-only rule of 2026-09-24 made — Opus 5 moves to
/// Opus 5.5 by its own family's step, whoever chose it; Fable crosses to Opus
/// 5.5 by the list's step, only when nobody chose it; nothing moves down,
/// sideways, or off a model the list does not name — with ONE exception the
/// model states on its own: a person's launch ALIAS (`PersonFlag = 2`), which
/// the family-first rule keeps verbatim (`model-alias`) before it reads what
/// runs, so nothing is ever due under it and even an unread visit clears the
/// due clock (the list-only rule read it as a person's choice, and still
/// moved within its family). A family the list does not name moving by the
/// build's own word (Sonnet to a newer Sonnet) is outside the projection.
///
/// WHAT IS BOUND, AND WHAT IS ONLY TRANSCRIBED. The Tier-1 file
/// (`aterm-agent`'s `conformance_upgrade_models/switch.rs`) drives these real
/// functions over EVERY reachable state and projects what they produce back
/// onto the model:
///
/// * `Visit` — `upgrade_models::model_read_step`, which
///   `upgrade_drive::model_judge` (the one read the drive's visit, the host's
///   pre-filter and a riding restart all take) wraps in its I/O: its early
///   return when no
///   model can be run and none is pending, then what the transcript says runs
///   (`live_model_at`), THE SETTLE STEP (`ModelRecord::settle`), THE MODEL
///   RULE (`model_due`) over the settled record, the cache's coldness
///   (`last_answer_at` against `CACHE_COLD_S`) and THE DUE CLOCK
///   (`ModelRecord::due_clock`), in that order; then `model_to`, with THE
///   MODEL LADDER (`model_moves_now`) inside it.
/// * `Relaunch`, `RelaunchRunsOther`, `Refused`, `Ride` — the ask recorded
///   before the one act (`ModelRecord::asked`) and what the new process then
///   runs (`live_model_at`: its launch flag until it answers).
///
/// TRANSCRIBED, not driven (a change there passes the bind): `model_judge`'s
/// and `model_read_kept`'s I/O (loading and saving the record, reading the
/// transcript, the ledger rows); the offer itself (`upgrade_models::offered`
/// over the build's catalog, the list and `availableModels`); `visit_models`' derivation of the announced model (the upgrade's
/// `model_list` while `Phase::Announced`) and of `build_restart` (a newer
/// build is due); the cheap pre-filter `model_wants_a_look`; `Announce`
/// (`St::for_target` and the notice carrying `model_to` as `model_list`);
/// `GiveUp` (`St::give_up`); `restart`'s ask of `st.model_list` and the
/// relaunch line (`relaunch::with_model`); `Refused`'s `St::unsent`; and
/// which view (managed or native) `visit_models` and `riding_model_in` judge
/// the session by, and the pre-filter's keep rule over two views — the model
/// has one view.
///
/// The projection. Models are the seed list (`upgrade_models::SEED`) plus one
/// it does not name: `live` is `1` = `claude-opus-5-5` (rank 1, Opus), `2` =
/// `claude-fable-5-1` (rank 2, Fable), `3` = `claude-opus-5` (rank 3, Opus),
/// `4` = `claude-sonnet-5` (off the list), `0` = unread (`model-unknown`);
/// `cmd` = it is a `/model` result newer than the last answer
/// (`LiveModel::by_command`). The offer `tgt` is `1` (every seed model
/// available: `offered` = `[claude-opus-5-5, claude-fable-5-1,
/// claude-opus-5]`), `3` (only `claude-opus-5`: every move would be DOWN) or
/// `0` (nothing offered), so the only model ever due, announced or asked for
/// is `claude-opus-5-5`, and the
/// record's `set`/`ap`/`fl`/`due` are that model's ask, applied, failed and
/// due-clock bits; `sage` = `MODEL_SETTLE_S` has passed since `set_at`,
/// `dover` = `MODEL_WARM_MAX_S` since `due_since`, `cold` = no answer for
/// `CACHE_COLD_S`. `hum` = the remembered `/model` (`ModelRecord::human`) is
/// `claude-fable-5-1` — the only memory that can protect anything here, as
/// the only live model outside the target's family that the rule may move.
/// `flag` = the process was relaunched by this harness with `--model
/// claude-opus-5-5`; `PersonFlag` = before that it was launched with a
/// person's own `--model`: `1` an id (`claude-fable-5-1` in the model), `2` a
/// family ALIAS (`fable`, `opus`: `model-alias`, see above); `DefaultFable` =
/// the settings' default is Fable, `Build` = a newer build is due all along. `phase` = the
/// upgrade is `0` not announced, `1` announced (`Phase::Announced`), `2` gave
/// up (`Phase::Failed(GAVE_UP)`: it stops asking, and a late READY still
/// restarts it — with the `model_list` the notice carried, `ann`); `ann` =
/// the notice carried the model. `fresh`/`mto` = the state is the one a visit
/// just left, and that visit's `model_to`: the laws are about THAT decision,
/// read against the inputs it was made on.
///
/// Laws, one invariant each, and `Buggy = 1` arms at least one mutant per law
/// (two for `SettleRecordsWhatRan` and for `NeverOntoAppliedOrFailed`) — each
/// an action whose healthy branch changes nothing, firing once from a state
/// where the defect shows, after which nothing else moves (so the `Buggy = 1`
/// space stays the healthy one plus the forged states). The first three are
/// stated over BOTH the rule's verdict (`due`) and the visit's decision
/// (`mto`), and the decision is EXEMPT while an announced model rides (`phase
/// == 1 && ann == 1`, `AnnouncedModelRides`):
///
/// * `UpTheListOnly` — the verdict, and a decision outside a ride, go only UP
///   the list: never down, never sideways, never off a model the list does not
///   name. Mutant `JudgeMovesOffList`: an off-list model's missing rank read
///   as below everything (`rank(..).unwrap_or(usize::MAX)`), which moves a
///   person's own pick.
/// * `PersonsChoiceStaysInFamily` — a person's choice (a launch `--model` not
///   the harness's, a `/model`, now or remembered, the settings' default by
///   family) is moved, by the verdict or by a decision outside a ride, only
///   within its family. Mutant `JudgeForgetsTheRememberedChoice`: the rule
///   before `18090b6ae`, which read a person's choice only off the launch flag
///   and a `/model` newer than the last answer — so an answered `/model
///   claude-fable-5-1` was moved to Opus.
/// * `NeverOntoAppliedOrFailed` — neither the verdict nor a decision outside a
///   ride is ever onto a model already applied to the conversation, or after
///   it failed. Mutants `JudgeRetriesAFailedModel` (the verdict: the failed
///   record not read, so a model the relaunch did not take is asked for again
///   every time it is due — a restart loop) and `JudgeReadsAStaleAnnouncement`
///   (the decision: a gave-up upgrade's `model_list` read as if still
///   announced, which carries a model recorded FAILED).
/// * `MovesExactlyWhenTheLadderSays` — a due move is taken exactly when THE
///   MODEL LADDER says (a cold cache, a restart happening anyway, or the warm
///   wait over). Mutant `LadderWaitsForCold`: the rule before `2d4656f32`,
///   which moved only on a cold cache, so an active session never moved (the
///   2026-09-25 incident: a restart onto 2.1.283 kept `claude-opus-5`).
/// * `AnnouncedModelRides` — once announced, the model is the one the
///   relaunch asks for, even after the READY answer re-warms the cache.
///   Mutant `LadderRetakenAfterReady`: the decision re-taken at every visit,
///   as before `4f4c00777`, which drops the model half-way.
/// * `SettleRecordsWhatRan` — an ask that runs is recorded applied, and one
///   never applied and still not what runs after `MODEL_SETTLE_S` is recorded
///   failed and cleared. Mutants `SettleNeverFails` (the ask stays pending, so
///   the same model is asked for again) and `SettleNeverVerifies` (never
///   applied, so a person who moves off it by hand is moved back).
/// * `AppliedAskStands` — an ask once applied is never recorded failed: it
///   ran. Mutant `SettleFailsAnAskThatRan`: the settle step before
///   2026-09-27, whose failed arm did not read `applied`, so a person's later
///   `/model` turned a verified ask into a `model-failed` one and the
///   harness's own `--model` then read as a person's.
///
/// OPEN — the ride's exemption is a gap in the CODE, not a convenience of the
/// model: an announced model rides whatever this visit decides, and a
/// gave-up upgrade's late READY relaunches with the `model_list` its notice
/// carried, so the relaunch asks for `claude-opus-5-5` (a) after it was
/// recorded FAILED, (b) over a person's `/model claude-fable-5-1` typed after
/// the notice (across families), and (c) off a model the list does not name.
/// `aterm-spec`'s `derived_harness_model_switch_open_ride_gaps_are_reachable`
/// pins each as reachable here; a fix re-takes those three laws inside the
/// ride too, and flips that test.
///
/// What this does NOT cover is listed in the Tier-1 file's header; the
/// largest omissions are a second family member above the person's (the
/// target is always Opus), a person's own relaunch, the owner's `--now`
/// re-arming a gave-up upgrade, a restart made for another reason while an
/// upgrade is announced, and clock skew.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_model_switch_model() -> Model {
    crate::ty_model! {
        HarnessModelSwitch {
            const Buggy = 0;
            const DefaultFable = 0;
            const PersonFlag = 0;
            const Build = 0;

            var live = 3;
            var cmd = 0;
            var flag = 0;
            var cold = 0;
            var tgt = 1;
            var set = 0;
            var sage = 0;
            var ap = 0;
            var fl = 0;
            var hum = 0;
            var due = 0;
            var dover = 0;
            var phase = 0;
            var ann = 0;
            var fresh = 0;
            var mto = 0;
            var forged = 0;

            // -- the world: every step ends the visit's freshness -----------

            // The conversation answers: the cache is warm, and a `/model`
            // before it is no longer newer than the last answer.
            action Answer when (forged == 0) {
                cold = 0;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            action GoCold when (forged == 0 && cold == 0) {
                cold = 1;
                fresh = 0;
                mto = 0;
            }
            // Availability moves the target: Opus 5.5, only Opus 5, none.
            action Retarget when (forged == 0) {
                tgt = if tgt == 1 { 3 } else if tgt == 3 { 0 } else { 1 };
                fresh = 0;
                mto = 0;
            }
            // A person's `/model`: Fable, or a model the list does not name.
            action PersonTypesModel when (forged == 0) {
                live = if live == 2 { 4 } else { 2 };
                cmd = 1;
                fresh = 0;
                mto = 0;
            }
            // The tail holds no answer (a long tool result): unread.
            action LoseSight when (forged == 0 && live > 0) {
                live = 0;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }
            action SettleElapses when (forged == 0 && set == 1 && sage == 0) {
                sage = 1;
                fresh = 0;
                mto = 0;
            }
            action WarmWaitElapses when (forged == 0 && due == 1 && dover == 0) {
                dover = 1;
                fresh = 0;
                mto = 0;
            }

            // -- the harness -------------------------------------------------

            // `model_read_step` + `model_to`, as one visit. The settle step's
            // failed arm fires only on an ask never applied.
            action Visit when (forged == 0) {
                ap = if set == 1 && live == 1 { 1 } else { ap };
                fl = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 1 } else { fl };
                set = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 0 } else { set };
                sage = if set == 1 && (live <= 0 || live > 1) && sage == 1 && ap == 0 { 0 } else { sage };
                hum = if (tgt > 0 || set == 1) && cmd == 1 {
                    if live == 2 { 1 } else { 0 }
                } else {
                    hum
                };
                due = if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 {
                    1
                } else if tgt > 0 && live <= 0 && (flag == 1 || PersonFlag <= 1) {
                    due
                } else {
                    0
                };
                dover = if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 {
                    if due == 1 { dover } else { 0 }
                } else if tgt > 0 && live <= 0 && (flag == 1 || PersonFlag <= 1) {
                    dover
                } else {
                    0
                };
                mto = if phase == 1 && ann == 1 {
                    1
                } else if (if tgt == 1 && ap == 0 && fl == 0 && (set == 0 || sage == 0) &&
                    (flag == 1 || PersonFlag <= 1) {
                    if live == 3 {
                        1
                    } else if live == 2 {
                        if (flag == 0 && PersonFlag == 1) || (flag == 1 && set == 0) ||
                            cmd == 1 || hum == 1 || DefaultFable == 1 { 0 } else { 1 }
                    } else {
                        0
                    }
                } else {
                    0
                }) == 1 && (cold == 1 || Build == 1 || (due == 1 && dover == 1)) {
                    1
                } else {
                    0
                };
                fresh = 1;
            }
            // The notice, carrying that visit's `model_to`: a restart with
            // nothing to carry is a build's; a build-only notice is
            // retargeted when a model becomes due. A gave-up upgrade is not
            // announced again (`St::for_target` reuses a FAILED state for the
            // build, whatever model would ride).
            action Announce when (
                forged == 0 && fresh == 1 &&
                ((phase == 0 && (mto == 1 || Build == 1)) || (phase == 1 && ann == 0 && mto == 1))
            ) {
                phase = 1;
                ann = mto;
                fresh = 0;
                mto = 0;
            }
            // READY — to a standing notice, or a late one after the upgrade
            // gave up: the ask recorded, the agent ended, the relaunch runs
            // what its line asks — the notice's model, else the launch flag it
            // kept, else what the transcript last named. There is a restart
            // to make only while a build is due or this visit moves the model.
            action Relaunch when (forged == 0 && fresh == 1 && phase > 0 && (Build == 1 || mto == 1)) {
                set = if ann == 1 { 1 } else { set };
                sage = if ann == 1 { 0 } else { sage };
                flag = if ann == 1 { 1 } else { flag };
                live = if ann == 1 || flag == 1 { 1 } else if PersonFlag == 1 { 2 } else { live };
                cmd = if ann == 1 || flag == 1 || PersonFlag == 1 { 0 } else { cmd };
                phase = 0;
                ann = 0;
                fresh = 0;
                mto = 0;
            }
            // The relaunch asked for the model, and Claude Code runs another.
            action RelaunchRunsOther when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1)
            ) {
                set = 1;
                sage = 0;
                flag = 1;
                live = 3;
                cmd = 0;
                phase = 0;
                ann = 0;
                fresh = 0;
                mto = 0;
            }
            // The ask recorded, the signal refused: nothing relaunched, the
            // phase as it was (`St::unsent`).
            action Refused when (
                forged == 0 && fresh == 1 && phase > 0 && ann == 1 && (Build == 1 || mto == 1)
            ) {
                set = 1;
                sage = 0;
                fresh = 0;
                mto = 0;
            }
            // No READY the restart could act on: the upgrade stops asking. It
            // keeps the notice's `model_list`, and a late READY still restarts.
            action GiveUp when (forged == 0 && phase == 1) {
                phase = 2;
                fresh = 0;
                mto = 0;
            }
            // A restart made for another reason carries the due model.
            action Ride when (forged == 0 && fresh == 1 && phase == 0 && due == 1 && live > 0) {
                set = 1;
                sage = 0;
                flag = 1;
                live = 1;
                cmd = 0;
                fresh = 0;
                mto = 0;
            }

            // -- the defects; every healthy branch changes nothing ----------

            action JudgeMovesOffList when (
                forged == 0 && tgt == 1 && live == 4 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 &&
                ((flag == 0 && PersonFlag == 0) || (flag == 1 && set == 1))
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action JudgeForgetsTheRememberedChoice when (
                forged == 0 && tgt == 1 && live == 2 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 &&
                ((flag == 0 && PersonFlag == 0) || (flag == 1 && set == 1)) &&
                (hum == 1 || DefaultFable == 1)
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action JudgeRetriesAFailedModel when (
                forged == 0 && tgt == 1 && live == 3 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 1 && phase == 0
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 {
                    if cold == 1 || Build == 1 || (due == 1 && dover == 1) { 1 } else { 0 }
                } else {
                    mto
                };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The decision, not the verdict: the verdict is right (nothing is
            // due — the model failed), and a gave-up upgrade's `model_list` is
            // carried as if the notice still stood.
            action JudgeReadsAStaleAnnouncement when (
                forged == 0 && phase == 2 && ann == 1 && fl == 1 && due == 0 && live > 0
            ) {
                mto = if Buggy == 1 { 1 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action LadderWaitsForCold when (
                forged == 0 && tgt == 1 && live == 3 && cmd == 0 && sage == 0 &&
                ap == 0 && fl == 0 && phase == 0 && cold == 0 &&
                (Build == 1 || (due == 1 && dover == 1))
            ) {
                due = if Buggy == 1 { 1 } else { due };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // Re-taken where the fresh decision would not carry the model: the
            // READY answer warmed the cache (and nothing else says move), or
            // the target moved away since the notice.
            action LadderRetakenAfterReady when (
                forged == 0 && live == 3 && cmd == 0 && sage == 0 && phase == 1 && ann == 1 && (
                    (tgt == 1 && ap == 0 && fl == 0 && cold == 0 && Build == 0 &&
                        (due == 0 || dover == 0)) ||
                    (tgt == 3 && due == 0)
                )
            ) {
                due = if Buggy == 1 && tgt == 1 { 1 } else { due };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SettleNeverFails when (
                forged == 0 && set == 1 && sage == 1 && live > 1 && ap == 0 && tgt == 3 &&
                cmd == 0 && due == 0 && phase == 0
            ) {
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            action SettleNeverVerifies when (
                forged == 0 && set == 1 && live == 1 && ap == 0 && tgt == 3 &&
                cmd == 0 && due == 0 && phase == 0
            ) {
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }
            // The failed arm without its `applied` test: an ask that ran, and
            // a person has since moved off, recorded failed and cleared.
            action SettleFailsAnAskThatRan when (
                forged == 0 && set == 1 && sage == 1 && ap == 1 && fl == 0 && live > 1 &&
                due == 0 && phase == 0
            ) {
                set = if Buggy == 1 { 0 } else { set };
                sage = if Buggy == 1 { 0 } else { sage };
                fl = if Buggy == 1 { 1 } else { fl };
                mto = if Buggy == 1 { 0 } else { mto };
                fresh = if Buggy == 1 { 1 } else { fresh };
                forged = if Buggy == 1 { 1 } else { forged };
            }

            // A visit's verdict is DUE exactly when it leaves the clock running
            // on a model it could read (`model-unknown` leaves the clock as it
            // was): `fresh == 1 && due == 1 && live > 0`. Its decision is
            // `mto`, exempt from the first three laws only while an announced
            // model rides (`phase == 1 && ann == 1`: OPEN, see above).

            // Up the list only: the move is onto the target, a listed model
            // ranked above the live one, and the live one is on the list.
            invariant UpTheListOnly:
                fresh == 0 || (tgt == 1 && live > 1 && live <= 3) ||
                ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)));
            // A person's choice moves only within its family (the target's is
            // Opus: `1` and `3`).
            invariant PersonsChoiceStaysInFamily:
                fresh == 0 || live == 1 || live == 3 ||
                (if (flag == 0 && PersonFlag > 0) || (flag == 1 && set == 0) || cmd == 1 ||
                    (live == 2 && (hum == 1 || DefaultFable == 1)) { 0 } else { 1 }) == 1 ||
                ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)));
            invariant NeverOntoAppliedOrFailed:
                fresh == 0 || (ap == 0 && fl == 0) ||
                ((due == 0 || live <= 0) && (mto == 0 || (phase == 1 && ann == 1)));
            // Outside an announcement, the visit's `model_to` is the due move
            // exactly when the ladder takes it.
            invariant MovesExactlyWhenTheLadderSays:
                fresh == 0 || (phase == 1 && ann == 1) ||
                mto == (if due == 1 && live > 0 && (cold == 1 || Build == 1 || dover == 1) { 1 } else { 0 });
            invariant AnnouncedModelRides:
                fresh == 0 || phase <= 0 || phase > 1 || ann == 0 || mto == 1;
            invariant SettleRecordsWhatRan:
                fresh == 0 || set == 0 || (live == 1 && ap == 1) ||
                ((live <= 0 || live > 1) && (ap == 1 || sage == 0));
            invariant AppliedAskStands:
                ap == 0 || set == 1;
        }
    }
}
