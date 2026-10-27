// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lifecycle model of the Linux lane's own VM (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1,
//! §12): the disk layers, the lab's bring-up, the detached helper with its run files, its
//! sessions, `keep` and the release rule, its stops, the agent's hello, the commands in flight,
//! the escalation by pid, unique id and start time, and the end of a helper whose hard stop
//! never finishes.

use aterm_spec::derive::*;

/// `VzLifecycle`: one VM's state directory (`vz.noindex`) over the lives of its helpers.
///
/// | Var | Values |
/// | --- | --- |
/// | `bake` | 0 no baked disk, 1 a bake's `*.partial.img`, 2 sealed under its final name |
/// | `live` | 0 no live disk, 1 cloned from a sealed baked disk, 2 cloned from a partial one |
/// | `disk` | what the planned or running VM attaches writable: 0 nothing, 1 the partial bake, 2 live, 3 the sealed baked disk |
/// | `up`, `prepared` | an `up` holds the bring-up lock; it prepared disks for a boot it has not made |
/// | `helper` | 0 none, 1 serving, 2 hung (alive, answers nothing), 3 gone (its record and socket left), 4 gone with its pid now another process's (another unique id, or another start time: a later boot's) |
/// | `phase` | the helper's VM: 0 Absent, 1 Booting, 2 Ready, 3 Stopping, 4 Stopped, 5 Failed |
/// | `sessions`, `keep` | the helper's attached sessions, and its `keep` flag |
/// | `hello` | 0 none, 1 the plan's agent, 2 another agent |
/// | `asked` | why a stop runs: 0 nothing asked, 1 a detector (or the wrong agent), 2 a `Stop`, 3 the release rule |
/// | `hard` | a hard stop (`stopWithCompletionHandler:`) was called |
/// | `ran` | a bake's VM (on the partial file): 0 never `Ready`, 1 `Ready` (the lab provisions only then), 2 then stopped by its guest's own power-off, with no stop asked (`POWERED_OFF`, §4's step 4 done), 3 sealed although it never finished |
/// | `exec`, `outcome` | a command in flight; how the last one ended: 0 not yet, 1 its `Exit`, 2 `Lost`, 3 `Lost` although its agent wrote its `Exit` |
/// | `second`, `pulled`, `wrong_kill` | ghosts: a second VM on a held disk; a disk prepared under a VM or a prepared boot; a pid signalled whose unique id or start time was not the record's |
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vz_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        VzLifecycle {
            const Sessions = 2; // lab sessions attached at once
            const Buggy = 0;

            var bake = 0;
            var live = 0;
            var disk = 0;
            var up = 0;
            var prepared = 0;
            var helper = 0;
            var phase = 0;
            var sessions = 0;
            var keep = 0;
            var hello = 0;
            var asked = 0;
            var hard = 0;
            var ran = 0;
            var exec = 0;
            var outcome = 0;
            var second = 0;
            var pulled = 0;
            var wrong_kill = 0;

            // The lab's bring-up lock (`run/bring-up.lock`): one `up` prepares and boots at a
            // time; another is NOT RUN, naming it.
            action UpLocks when (up == 0) {
                up = 1;
            }
            action UpRefused when (up == 1) {
                up = 1;
            }
            action UpUnlocks when (up == 1) {
                up = 0;
                prepared = 0;
                disk = if phase == 0 { 0 } else { disk };
            }

            // `Session::bring_up`'s `Layers::prepare`, for a session that found no VM: every
            // partial file removed, then the live disk kept, or cloned from the sealed baked
            // disk, or a bake begun.
            action Prepare when (up == 1 && helper == 1 && sessions > 0 && phase == 0 &&
                prepared == 0) {
                live = if live > 0 { live } else if bake == 2 { 1 } else { 0 };
                bake = if live > 0 { if bake == 1 { 0 } else { bake } }
                    else if bake == 2 { 2 } else { 1 };
                disk = if live > 0 || bake == 2 { 2 } else { 1 };
                prepared = 1;
            }
            // §4's step 5, once step 4 is done: the bake's VM was `Ready` (so the lab
            // provisioned it) and then its guest powered itself off, with no stop asked (the
            // helper's `POWERED_OFF`). The partial file is flushed, made 0444 and renamed. A
            // bake a detector, a `Stop` or the release rule stopped, or one that never reached
            // `Ready`, stays a partial file for the next `Prepare`.
            action Seal when (bake == 1 && disk == 1 && phase == 4 && ran == 2) {
                bake = 2;
                disk = 0;
                ran = 0;
            }

            // A lab finds no live helper and starts one: the disk's `flock`, the socket, the
            // record.
            action Start when (helper == 0) {
                helper = 1;
                phase = 0;
                sessions = 0;
                keep = 0;
                hello = 0;
                asked = 0;
                hard = 0;
                ran = 0;
                exec = 0;
                outcome = 0;
                disk = 0;
                prepared = 0;
            }
            // A second helper for the disk fails its `flock`, naming the holder.
            action SecondRefused when (helper == 1 || helper == 2) {
                helper = helper;
            }

            // The helper's sessions and `keep` (§5.1).
            action Attach when (helper == 1 && sessions <= Sessions - 1 && phase <= 2) {
                sessions = sessions + 1;
            }
            action AttachKeep when (helper == 1 && sessions <= Sessions - 1 && phase <= 2) {
                sessions = sessions + 1;
                keep = 1;
            }
            action AttachRefused when (helper == 1 && phase > 2) {
                helper = 1;
            }
            action KeepSet when (helper == 1 && sessions > 0) {
                keep = 1;
            }
            action KeepClear when (helper == 1 && sessions > 0) {
                keep = 0;
            }
            // A session's `Detach` or EOF, then the release rule: the last one with `keep`
            // unset powers the VM off; with no VM, the helper exits.
            action Detach when (helper == 1 && sessions > 0) {
                sessions = sessions - 1;
                phase = if sessions == 1 && keep == 0 && (phase == 1 || phase == 2) { 3 }
                    else { phase };
                asked = if sessions == 1 && keep == 0 && (phase == 1 || phase == 2) { 3 }
                    else { asked };
                helper = if sessions == 1 && (phase == 0 || phase == 5) { 0 } else { 1 };
            }
            // No session attached within a minute of the helper's start.
            action Quiet when (helper == 1 && phase == 0 && sessions == 0) {
                helper = 0;
            }

            // The VM: its boot, the agent's hello and `Ready`, and its stops (§10).
            action Boot when (helper == 1 && sessions > 0 && phase == 0 && prepared == 1) {
                phase = 1;
                prepared = 0;
            }
            // The framework refuses the configuration (`validate`: the entitlement): nothing
            // starts.
            action BootRefused when (helper == 1 && sessions > 0 && phase == 0 &&
                prepared == 1) {
                phase = 5;
                prepared = 0;
            }
            action StartFails when (helper == 1 && phase == 1 && hello == 0) {
                phase = 5;
                helper = if sessions == 0 { 0 } else { 1 };
            }
            action Hello when (helper == 1 && phase == 1 && hello == 0) {
                hello = 1;
            }
            // A hello naming another agent, first or after the plan's (a guest whose agent
            // changed), while the VM runs: every session hears `Lost`, and the VM is stopped
            // without the agent (§10).
            action HelloWrong when (helper == 1 && (phase == 1 || phase == 2)) {
                hello = 2;
                phase = 3;
                asked = 1;
            }
            action Ready when (helper == 1 && phase == 1 && hello == 1) {
                phase = 2;
                ran = if disk == 1 { 1 } else { 0 };
            }
            // A detector fires (the boot's, the heartbeat's, the free space's, a panic): every
            // session hears `Lost`, and the VM stops hard at once.
            action Detect when (helper == 1 && (phase == 1 || phase == 2 ||
                (phase == 3 && hard == 0))) {
                phase = 3;
                hard = 1;
                asked = 1;
            }
            // `down`'s `Stop`, on a connection of its own: `keep` goes, the guest powers off;
            // with no VM to power off, the helper exits.
            action Stop when (helper == 1 && (phase <= 3 || phase == 5)) {
                keep = 0;
                phase = if phase == 1 || phase == 2 { 3 } else { phase };
                asked = if phase == 1 || phase == 2 { 2 } else { asked };
                sessions = if phase == 0 || phase == 5 { 0 } else { sessions };
                helper = if phase == 0 || phase == 5 { 0 } else { 1 };
            }
            // A soft stop's grace passed with no new console line: the hard stop.
            action GraceOut when (helper == 1 && phase == 3 && hard == 0) {
                hard = 1;
            }
            // The hard stop did not finish within a span: the helper exits, and its VM
            // process with it (fact 44); every command in flight ends `Lost`.
            action HardTimeout when (helper == 1 && phase == 3 && hard == 1) {
                helper = 0;
                sessions = 0;
                phase = 4;
                outcome = if exec == 1 { 2 } else { outcome };
                exec = 0;
            }
            // The VM stopped (the guest powered off, or the hard stop finished), or stopped
            // with an error: every command in flight ends `Lost`, and the helper exits.
            action Stopped when (helper == 1 && (phase == 1 || phase == 2 || phase == 3)) {
                phase = 4;
                helper = 0;
                sessions = 0;
                ran = if ran == 1 && asked == 0 { 2 } else { ran };
                outcome = if exec == 1 { 2 } else { outcome };
                exec = 0;
            }
            action StoppedWithError when (helper == 1 && (phase == 1 || phase == 2 ||
                phase == 3)) {
                phase = 5;
                helper = 0;
                sessions = 0;
                outcome = if exec == 1 { 2 } else { outcome };
                exec = 0;
            }

            // A command: its connection, then exactly one end.
            action ExecBegin when (helper == 1 && sessions > 0 && phase == 2 && exec == 0) {
                exec = 1;
                outcome = 0;
            }
            action ExecExit when (exec == 1 && (phase == 2 || phase == 3)) {
                exec = 0;
                outcome = 1;
            }

            // The world: a helper that stops answering, or dies (its VM process exits by
            // itself), and a dead helper's pid taken by another process.
            action Hang when (helper == 1) {
                helper = 2;
            }
            action Die when (helper == 1 || helper == 2) {
                helper = 3;
                sessions = 0;
                phase = if phase == 1 || phase == 2 || phase == 3 { 4 } else { phase };
                outcome = if exec == 1 { 2 } else { outcome };
                exec = 0;
            }
            action PidReused when (helper == 3) {
                helper = 4;
            }
            // The lab's leftovers and escalation: a dead helper's record and socket go
            // unsignalled; a hung one is SIGKILLed by its pid, its unique id and start time read
            // again and matched, and its VM process exits by itself. An earlier build's record
            // names no start time, so its helper is never escalated: `down` asks it to `Stop`,
            // and one that answers nothing stays hung (NOT RUN).
            action ClearDead when (helper == 3 || helper == 4) {
                helper = 0;
            }
            action Escalate when (helper == 2) {
                helper = 0;
                sessions = 0;
                phase = if phase == 1 || phase == 2 || phase == 3 { 4 } else { phase };
                outcome = if exec == 1 { 2 } else { outcome };
                exec = 0;
            }

            // The mutants (`Buggy == 1`), each a planted defect of the conformance.
            action SecondBoots when (Buggy == 1 && (helper == 1 || helper == 2) &&
                (phase == 1 || phase == 2 || phase == 3) && disk == 2) {
                second = 1;
            }
            action ClonePartial when (Buggy == 1 && up == 1 && helper == 1 && sessions > 0 &&
                phase == 0 && prepared == 0 && live == 0 && bake == 1) {
                live = 2;
                disk = 2;
                prepared = 1;
            }
            action AttachBaked when (Buggy == 1 && up == 1 && helper == 1 && sessions > 0 &&
                phase == 0 && prepared == 0 && live == 0 && bake == 2) {
                disk = 3;
                prepared = 1;
            }
            action PrepareUnderVm when (Buggy == 1 && up == 1 && helper == 1 && sessions > 0 &&
                (phase == 1 || phase == 2)) {
                pulled = 1;
            }
            action UpWithoutLock when (Buggy == 1 && prepared == 1 && phase == 0) {
                pulled = 1;
            }
            action DropExit when (Buggy == 1 && exec == 1 && (phase == 2 || phase == 3)) {
                exec = 0;
                outcome = 3;
            }
            action ExecBooting when (Buggy == 1 && helper == 1 && sessions > 0 && phase == 1 &&
                exec == 0) {
                exec = 1;
                outcome = 0;
            }
            action SealHalfMade when (Buggy == 1 && bake == 1 && disk == 1 && phase > 3 &&
                ran <= 1) {
                bake = 2;
                disk = 0;
                ran = 3;
            }
            action KeepRunning when (Buggy == 1 && helper == 1 && sessions == 1 && keep == 0 &&
                (phase == 1 || phase == 2)) {
                sessions = 0;
            }
            action HardUnasked when (Buggy == 1 && helper == 1 && sessions == 2 &&
                (phase == 1 || phase == 2)) {
                sessions = 1;
                phase = 3;
                hard = 1;
            }
            action SignalReused when (Buggy == 1 && helper == 4) {
                helper = 0;
                wrong_kill = 1;
            }
            action ReadyUnchecked when (Buggy == 1 && helper == 1 && phase == 1 && hello == 0) {
                phase = 2;
                ran = if disk == 1 { 1 } else { 0 };
            }

            // The live disk is attached to at most one VM.
            invariant LiveAttachedOnce: second == 0;
            // Base and baked are never attached writable after their bake.
            invariant SealedNeverAttached: disk <= 2;
            // A baked disk is cloned only once sealed.
            invariant ClonedOnlyWhenSealed: live <= 1;
            // A bake is sealed only once it finished (§4): a half-provisioned disk is never
            // sealed, so never cloned.
            invariant SealedOnlyWhenFinished: ran <= 2;
            // No disk is removed or replaced under a VM, or under another `up`'s prepared
            // boot.
            invariant NothingPreparedUnderAVm: pulled == 0;
            // Every `Exec` runs on a `Ready` VM (or one stopping since) and ends in exactly one of
            // Exited, Signaled, NotStarted (its `Exit`) or Lost: `Lost` only once its VM is
            // going, and never for a command whose agent wrote its `Exit` (3, which a stopping
            // VM's `Lost` would otherwise hide).
            invariant EveryExecEndsOnce:
                (exec == 0 || phase == 2 || phase == 3) && (outcome <= 1 || phase > 2) &&
                outcome <= 2;
            // A VM with no session and `keep` unset is Stopping or Stopped.
            invariant ReleaseRule: sessions > 0 || keep == 1 || phase == 0 || phase > 2;
            // A hard stop follows only a detector, a `Stop`, or the release rule.
            invariant HardStopOnlyWhenAsked: hard == 0 || asked > 0;
            // A pid is signalled only when its unique id and start time are the record's.
            invariant SignalOnlyTheRecordedHelper: wrong_kill == 0;
            // Ready implies the agent hash matched.
            invariant ReadyOnlyForThePlansAgent: phase <= 1 || phase > 2 || hello == 1;
            bound Bounds:
                bake <= 2 && live <= 2 && disk <= 3 && up <= 1 && prepared <= 1 &&
                helper <= 4 && phase <= 5 && sessions <= Sessions && keep <= 1 &&
                hello <= 2 && asked <= 3 && hard <= 1 && ran <= 3 && exec <= 1 && outcome <= 3 &&
                second <= 1 && pulled <= 1 && wrong_kill <= 1;
        }
    }
}
