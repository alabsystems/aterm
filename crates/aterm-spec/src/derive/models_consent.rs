// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MACOS ACCESS GESTURES (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
//! §5): the owner's one-time change of macOS's debugger rule, and — in P2 —
//! the band's one `privacy.*` ask and the authd lane.

use super::*;

/// THE OWNER'S DEBUGGER-ACCESS CHANGE (design §5.1; owner rulings O7 and O8,
/// 2026-09-29). The owner presses *Allow debugger access…* (or *Turn off…*) on
/// Settings ▸ Security; aterm's own `confirm_owner` alert states the whole-Mac
/// scope and defaults to Cancel; a detached worker takes the Mac-wide change
/// lock, re-reads `DevToolsSecurity -status` and, only if it still reads the
/// change's precondition, runs `DevToolsSecurity -enable` (or `-disable`)
/// UNPRIVILEGED; then aterm re-probes and reports only what it reads. What
/// macOS shows for that child — an administrator dialog is expected — is
/// spike D2's to measure; the model only lets the child take as long as it
/// likes.
///
/// DIRECTION-NORMALISED. *Turn off* is the same reducer with the roles of the
/// two modes swapped, so `world` and `obs` count in the change's own terms:
/// `0` is the change's PRECONDITION (`disabled` for On, `enabled` for Off), `1`
/// its TARGET, `2` a rule Apple's tool does not recognise (either direction),
/// and `obs = 3` not read. `claim` is the Security row stating the target.
///
/// Variables. `phase`: 0 idle, 1 aterm's alert up, 2 the worker taking the
/// lock and re-reading `-status`, 3 the re-read showed the precondition, 4 the
/// child in flight (for hours, if nobody answers), 5 the child EXITED and its
/// result waits UNREAD in the shared slot, 6 the result read and a re-probe
/// owed. `world`: the right itself (the ENVIRONMENT moves it: `SetOutside`,
/// `Drift`, `Customize`, `Normalize` — an administrator, a macOS upgrade, an
/// MDM, another aterm). `obs`: the last `-status` read. `polling`: the park
/// re-reads the change lane every `RESULT_RECHECK` (`needs_poll`). `lock`: the
/// Mac-wide change lock beside `aterm.toml` — 0 free, 1 this process's worker,
/// 2 a peer aterm's (`PeerLaunch`/`PeerExit`: another windowed aterm sharing
/// the config, which under O8 may be a development build beside the installed
/// app). `fresh`: an owner's "Allow…" not yet spent by a launch or an abort.
/// `launches`: change children started. `unconsented`: launches made with no
/// fresh "Allow…". `inflight`: change children alive ON THIS MAC, ours and
/// peers'. `armed`: the worker's re-read showed the precondition and nothing
/// was launched since. `unarmed`: launches made without that re-read.
/// `clobbered`: a child rewrote a rule that was customised after the re-read —
/// the named RESIDUAL RACE. `stale`: the change marker beside `aterm.toml`
/// names a child that is gone and that this lane does not follow — a peer
/// aterm's, whose change that peer has yet to record (`PeerSettle`).
/// `misadopted`: a records read re-read a change whose child was still
/// running as one that had ended.
///
/// THE RECORDS RE-READ. Mid-session the lane reads the shared records again
/// (an unsettled read, or a mode the drift watch must judge against the
/// record as it is on disk now), and what it finds moves the lane: idle over
/// a dead marker it ADOPTS that change as aterm's and owes it a re-probe
/// (`Adopt`, 0 → 6, polling on); idle over a peer's LIVE child it FOLLOWS
/// that child as in flight (`Follow`, 0 → 4, polling on; `PeerExit` then
/// leaves its result to be read, 4 → 5); with a change of its own under way
/// it removes the dead marker (`Discard`). Nothing obliges a re-read to come,
/// so none of the three is fair.
///
/// Invariants, each with a `Buggy = 1` mutant that breaks it alone:
/// `NoActWithoutConsent` (`RetryAfterCancel`: retrying after the child read
/// unchanged; `SpendBanked`: a launch no alert was answered for),
/// `OneChild` (`IgnorePeerLock`: the worker spawning while a peer's child
/// holds the lock), `ClaimIsObserved` (`ClaimFromExitCode`: "exit 0 means
/// enabled"), `EveryLaunchFollowsAFreshRead` (`SkipRecheck`: the worker
/// spawning on the render's stale reading), `ReReadsOnlyAnEndedChange`
/// (`AdoptLive`: a records reader that skips the pid-and-birth check takes a
/// peer's running child for a dead one, and the lane re-probes — and ends —
/// a change whose dialog is still up) and `UnderWayIsPolled`
/// (`ResultWakeLost`: the park stops re-reading while a result waits in the
/// slot — the 2026-09-10 swallowed-verdict shape; `PressWhileInFlight`: a
/// second press while our child runs, whose worker the lock refuses, leaves
/// the lane idle over a running child). `UnderWayIsPolled` is the safety half
/// of [`debugger_access_change_liveness`], and Tier-1 projects `polling` from
/// the lane's own `needs_poll`, so it says something about the code: a lane
/// that stops re-reading while a change is under way breaks it.
///
/// THE RESIDUAL RACE IS NAMED, NOT HIDDEN. `ChildClobbers` is honest
/// environment behaviour: the rule can change between the worker's re-read and
/// `DevToolsSecurity`'s own write, and that window spans the whole time the
/// child waits (§1.3). No invariant claims to exclude it; Tier-0 checks a copy
/// with `NoClobber: clobbered == 0` and REQUIRES a counterexample at
/// `Buggy = 0`.
///
/// O8 (owner, 2026-09-29): an unhardened aterm offers the gesture too, so no
/// variable stands for aterm's own hardening and no guard refuses `Press` on
/// it — the alert body states the exposure instead. A Tier-1 bind drives the
/// real reducer with `self_hardened = NotHardened` and requires the press to
/// be ACCEPTED like any other.
///
/// Tier-1 (P1, `aterm-gui/src/debugger_access.rs`) drives the real
/// `DebuggerChangeLane` (`begin` with the owner's answer, `poll`, `observe`,
/// `needs_poll`), the real worker body `run_devtools_job` on a thread with the
/// real change lock, and the real order `devtools_change_with` (through its
/// debug-build wrapper `devtools_change_with_for_test`) with a
/// scripted re-read and a stand-in child, in both directions, and the records
/// re-read mid-session (`ask_records`, the real reader `read_records`, then
/// `poll`; a peer's marker written as its worker writes it and let go by the
/// real `settle_markers`). It projects `phase`/`obs`/`polling` from the lane,
/// `claim` from what the Security row states, `stale` from the marker on disk
/// and the lane's own `follows`, and `lock`/`fresh`/`launches`/`unconsented`/
/// `inflight`/`armed`/`unarmed`/`misadopted` from what the stand-in
/// environment saw happen; it checks
/// `Model::action_enabled` against the lane's acceptance at every step, and
/// negative controls run the same harness over deliberately faulty lanes and
/// orders and require it to fail.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn debugger_access_change_model() -> Model {
    crate::ty_model! {
        DebuggerAccessChange {
            const Buggy = 0;
            const Cap = 2;
            var phase = 0;
            var world = 0;
            var obs = 3;
            var claim = 0;
            var polling = 0;
            var lock = 0;
            var fresh = 0;
            var launches = 0;
            var unconsented = 0;
            var inflight = 0;
            var armed = 0;
            var unarmed = 0;
            var clobbered = 0;
            var stale = 0;
            var misadopted = 0;

            // A completed `-status` read, idle or once the result was read:
            // the only step that sets what a surface says. It ends the change.
            action Probe when (phase == 0 || phase == 6) {
                obs = world;
                claim = if (world == 1) { 1 } else { 0 };
                phase = 0;
                polling = 0;
            }
            // A probe refused, failed or timed out: nothing is claimed.
            action ProbeFails when (phase == 0 || phase == 6) {
                obs = 3;
                claim = 0;
                phase = 0;
                polling = 0;
            }
            // The owner presses the button the render offered: only over an
            // observed precondition, and never while a change is under way.
            action Press when (phase == 0 && obs == 0) {
                phase = 1;
            }
            // MUTANT: the button stays live while our child is in flight.
            action PressWhileInFlight when (Buggy == 1 && phase == 4) {
                phase = 1;
            }
            // Cancel in aterm's alert, or nobody asked: nothing is spawned.
            action Decline when (phase == 1) {
                phase = if (polling == 1) { 4 } else { 0 };
            }
            // "Allow…" in aterm's alert: the lane arms its job and the park
            // starts re-reading (the environment bounds how often).
            action Consent when (phase == 1 && launches <= Cap - 1) {
                fresh = 1;
                polling = 1;
                phase = 2;
            }
            // MUTANT: a launch no alert was answered for.
            action SpendBanked when (Buggy == 1 && phase == 0 && obs == 0 && launches <= Cap - 1) {
                polling = 1;
                phase = 2;
            }
            // The worker takes the change lock and re-reads `-status`; it aborts
            // (and holds nothing) unless the lock was free and the read still
            // shows the precondition. A lock held elsewhere is not read past.
            action Recheck when (phase == 2) {
                obs = if (lock == 0) { world } else { obs };
                armed = if (lock == 0 && world == 0) { 1 } else { 0 };
                fresh = if (lock == 0 && world == 0) { fresh } else { 0 };
                lock = if (lock == 0 && world == 0) { 1 } else { lock };
                polling = if (lock == 0 && world == 0) { 1 } else { 0 };
                phase = if (lock == 0 && world == 0) { 3 } else { 0 };
            }
            // MUTANT: the worker takes the lock and spawns on the render's
            // reading.
            action SkipRecheck when (Buggy == 1 && phase == 2 && lock == 0) {
                lock = 1;
                phase = 3;
            }
            // MUTANT: the worker reads past a peer's lock.
            action IgnorePeerLock when (Buggy == 1 && phase == 2 && lock == 2 && world == 0) {
                obs = world;
                armed = 1;
                phase = 3;
            }
            // The change child starts, and its marker overwrites any dead one.
            action Launch when (phase == 3) {
                stale = 0;
                launches = launches + 1;
                inflight = inflight + 1;
                unarmed = if (armed == 1) { unarmed } else { unarmed + 1 };
                unconsented = if (fresh == 1) { unconsented } else { unconsented + 1 };
                armed = 0;
                fresh = 0;
                phase = 4;
            }
            // The child applied the change: the rule is the target now. The
            // worker publishes the result, releases the lock and ends. (A
            // followed peer's child — `lock == 2` — ends by `PeerExit`.)
            action ChildApplied when (phase == 4 && lock <= 1 && inflight > 0 && world == 0) {
                world = 1;
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            // THE RESIDUAL RACE: the rule was customised after the re-read,
            // and the child's write lands over it.
            action ChildClobbers when (phase == 4 && lock <= 1 && inflight > 0 && world == 2) {
                world = 1;
                clobbered = 1;
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            // Cancel in macOS's dialog, a wrong password, a failed write.
            action ChildUnchanged when (phase == 4 && lock <= 1 && inflight > 0) {
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            // The park's re-read takes the result from the slot, wake or no
            // wake, and asks for a fresh probe.
            action Notice when (phase == 5 && polling == 1) {
                phase = 6;
            }
            // MUTANT: "exit 0 means enabled".
            action ClaimFromExitCode when (Buggy == 1 && phase == 5) {
                claim = 1;
            }
            // MUTANT: an unchanged result retried without asking again.
            action RetryAfterCancel when (Buggy == 1 && phase == 6 && lock == 0 && world == 0 && launches <= Cap - 1) {
                lock = 1;
                armed = 1;
                phase = 3;
            }
            // MUTANT: the result's wake is swallowed (an uncommitted incoming
            // handoff drops every wake but Commit) and the park stops
            // re-reading the slot.
            action ResultWakeLost when (Buggy == 1 && phase == 5 && polling == 1) {
                polling = 0;
            }
            // An idle lane's records re-read finds a marker naming a child
            // that is gone: it re-reads that change as aterm's, and only the
            // re-probe it now owes ends it.
            action Adopt when (phase == 0 && stale == 1) {
                stale = 0;
                polling = 1;
                phase = 6;
            }
            // MUTANT: the re-read takes a running child for a dead one.
            action AdoptLive when (Buggy == 1 && phase == 0 && lock == 2) {
                misadopted = 1;
                polling = 1;
                phase = 6;
            }
            // An idle lane's records re-read finds a peer's child running: the
            // change is in flight here too, and no press may start another.
            action Follow when (phase == 0 && lock == 2) {
                polling = 1;
                phase = 4;
            }
            // A records re-read with a change of this lane's under way finds a
            // dead marker that is not that change's: it removes it.
            action Discard when (phase > 0 && stale == 1) {
                stale = 0;
            }
            // A peer aterm takes the free lock and starts its own child; its
            // marker overwrites any dead one.
            action PeerLaunch when (lock == 0 && inflight <= Cap - 1) {
                lock = 2;
                inflight = inflight + 1;
                stale = 0;
            }
            // The peer's child exits and its worker releases the lock. Its
            // marker stays until the peer records its re-probe: dead, and
            // this lane's to read unless it follows that child — then its
            // result waits to be read, as an inherited child's does.
            action PeerExit when (lock == 2 && inflight > 0) {
                lock = 0;
                inflight = inflight - 1;
                stale = if (phase == 4) { 0 } else { 1 };
                phase = if (phase == 4) { 5 } else { phase };
            }
            // The peer records its re-probe and lets its marker go.
            action PeerSettle when (stale == 1) {
                stale = 0;
            }
            // The environment moves the rule to the change's target (for On:
            // enabled elsewhere; for Off: turned off elsewhere).
            action SetOutside when (world == 0) {
                world = 1;
            }
            // The environment moves it back to the precondition.
            action Drift when (world == 1) {
                world = 0;
            }
            // The environment writes a rule Apple's tool does not recognise.
            action Customize when (world <= 1) {
                world = 2;
            }
            // …and puts back one it does.
            action Normalize when (world == 2) {
                world = 0;
            }

            invariant NoActWithoutConsent: unconsented == 0;
            invariant OneChild: inflight <= 1;
            invariant ClaimIsObserved: claim == 0 || obs == 1;
            invariant EveryLaunchFollowsAFreshRead: unarmed == 0;
            invariant UnderWayIsPolled: (phase <= 1 && (lock == 0 || lock == 2)) || polling == 1;
            invariant ReReadsOnlyAnEndedChange: misadopted == 0;
        }
    }
}

/// AN EXITED CHANGE IS READ: `[]<>(phase <= 4)` — every exited child's result
/// is taken from the slot and re-probed, so the Security row never sits on an
/// exit code or on "waiting" over a child that is gone.
///
/// THE HUMAN IS NEVER FAIR: the owner may leave aterm's alert or macOS's
/// dialog up for as long as they like (a 12-hour dialog is legal), so no
/// progress is claimed for phases 1–4 and none of their actions is assumed
/// fair. What the verdict assumes, and nothing more: `Notice` weakly fair —
/// bound to the park's `RESULT_RECHECK` re-read of the shared result slot
/// (`DebuggerChangeLane::poll` from `App::tick_debugger_access`, design §3.4
/// step 4), which runs whether or not the result's wake arrived — and `Probe`
/// weakly fair — the bounded `-status` worker the re-read dispatches. Both are
/// load-bearing (`verify::audit_liveness`): without `Notice` the change
/// stutters at `phase = 5`, without `Probe` at `phase = 6`. A records re-read
/// (`Adopt`, `Follow`) opens more ways into phases 4–6 and none out of them,
/// so the same two assumptions carry a change it adopted. The mutant that
/// breaks it alone: `ResultWakeLost` — the 2026-09-10 swallowed-verdict shape
/// (TCC doc Status, fixed in 122add794), where the result waits in a slot
/// nothing re-reads.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::debugger_access_change_model)]
pub fn debugger_access_change_liveness() -> Liveness {
    Liveness {
        name: "AnExitedChangeIsRead",
        goal: le(var("phase"), int(4)),
        weak: vec!["Notice", "Probe"],
        strong: vec![],
        mutants: vec!["ResultWakeLost"],
    }
}
