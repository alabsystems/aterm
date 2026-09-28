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

/// A READY answer is usable only by the process and tab that received the
/// notice, with a unique live owner of that conversation and a complete
/// session-file scan. The process-start token represents the kernel PID-reuse
/// guard. `Buggy=1` replays the old conversation-keyed reducer, which accepts
/// READY in another tab or after a partial scan.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_notice_owner_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeNoticeOwner {
            const Buggy = 0;
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
            var signaled = 0;

            action Announce when (phase == 0 && owners == 1 && scan_complete == 1) {
                phase = 1;
                owner_tab = tab;
                owner_pid = pid;
                owner_start = start;
            }
            action Ready when (phase == 1 && ready == 0) {
                ready = 1;
            }
            action OtherTab when (phase == 1 && tab == 1) {
                tab = 2;
                pid = 2;
                start = 2;
            }
            action DuplicateOwner when (phase == 1 && owners == 1) {
                owners = 2;
            }
            action IncompleteScan when (scan_complete == 1 && phase <= 1) {
                scan_complete = 0;
            }
            action Terminate when (
                phase == 1 && ready == 1 &&
                (Buggy == 1 ||
                    (owners == 1 && scan_complete == 1 && owner_tab == tab &&
                     owner_pid == pid && owner_start == start))
            ) {
                phase = 2;
                signaled = 1;
            }

            invariant OnlyIssuerSignaled:
                signaled == 0 ||
                (owner_tab == tab && owner_pid == pid && owner_start == start);
            invariant NoDuplicateOwnerSignal: signaled == 0 || owners == 1;
            invariant NoPartialScanSignal: signaled == 0 || scan_complete == 1;
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
///   runs, or at a break (`cut`).
/// - `NoHastySupersede`: a READY answer gets a whole bound of its own before
///   work it outlives supersedes it, by a re-ask or a give-up (`hasty`). The
///   review of 2026-09-26 found that a notice's clock alone gave up on a READY
///   answered seconds before.
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
            const Bound = 2;
            const MaxAsks = 2;
            var waited = 0;
            var aged = 0;
            var asks = 1;
            var ready = 1;
            var person = 0;
            var agent = 0;
            var brk = 0;
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

            action Wait when (
                ended == 0 && gaveup == 0 &&
                ((Buggy == 1 && (brk == 1 || (ready == 1 && (person == 1 || agent == 1)))) ||
                 (brk == 1 && Buggy == 0 &&
                  ((ready == 0 && waited <= Bound - 1) ||
                   (ready == 0 && person == 1 && asks <= MaxAsks - 1) ||
                   (ready == 1 && aged <= Bound - 1 && (person == 0 || waited <= Bound - 1)))) ||
                 (brk == 0 && ready == 0 &&
                  (waited <= Bound - 1 || (person == 1 && asks <= MaxAsks - 1))) ||
                 (brk == 0 && ready == 1 && Buggy == 0 && (person == 1 || agent == 1) &&
                  (person == 0 || waited <= Bound - 1) &&
                  (agent == 0 || person == 1 || aged <= Bound - 1)))
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
                 (Buggy == 0 && brk == 0 && ready == 1 && agent == 1 && aged > Bound - 1) ||
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
                 (Buggy == 0 && brk == 0 && ready == 1 && agent == 1 && aged > Bound - 1 &&
                  person == 0) ||
                 (Buggy == 1 && ready == 1 && agent == 1 && person == 0 && waited > Bound - 1))
            ) {
                hasty = if ready == 1 && aged <= Bound - 1 { 1 } else { hasty };
                gaveup = 1;
            }
            action Terminate when (
                ended == 0 && gaveup == 0 && ready == 1 && person == 0 &&
                ((brk == 0 && agent == 0) || (Buggy == 1 && waited > Bound - 1))
            ) {
                cut = if brk == 1 || agent == 1 { 1 } else { cut };
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
/// owed is carried: the new round's first notice supersedes it. The last
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
/// a `<synthetic>` row). `Buggy = 1` is the reducer before the fix, the
/// incident's three at once (`NoF1`, `NoF2`, `NoOwe`), and the terminal
/// `failed` of 2026-09-27 (`Terminal`). Tier-1 in
/// aterm-agent's `harness::upgrade_drive` tests drives the real reducer,
/// gates, record transitions, transcript readers and the host's reading of
/// each step's word over every reachable state, and replays the incident,
/// the pre-fix trace as the caught negative control.
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
            const Terminal = 0;
            const MaxAsks = 2;
            var limited = 0;
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
            action Restart when (
                ready == 1 && live == 1 && limited == 0 &&
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
            // or the agent's own background work held a gave-up one's.
            action Void when (
                ready == 1 && live == 1 && limited == 0 &&
                ((phase == 1 && window == 1) || (phase == 2 && Buggy == 0 && NoF2 == 0))
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
            // (`UnansweredDirection`: or was merely given one).
            action Release when (
                owed == 1 && limited == 0 && NoType == 0 &&
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
            // upgrade would do now — no restart, no release typed or dropped,
            // no new round (`LastWhileOwed`: a stop's own word said over a
            // release still owed). Said past a stopped round's rest, it is a
            // permanent wait (`stalled`).
            action Look when (
                (phase == 2 || phase == 4) && limited == 0 && stuck == 0 &&
                (if (
                    ready == 1 && live == 1 && limited == 0 &&
                    (phase == 1 || (phase == 2 && Buggy == 0 && NoF2 == 0))
                ) { 1 } else { 0 }) +
                (if (LastWhileOwed == 1 && phase == 4) { 0 } else if (
                    owed == 1 && limited == 0 && NoType == 0 &&
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
