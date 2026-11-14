// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A native restart's hold on the app update's handoff ends only with an end the old host's
/// terminal record answers, or once the restart starts nothing more, and it does end: no later
/// than the operation's close (its deadline plus the completion grace). One operation of one
/// host epoch, its one grant and its grantee, and one update.
/// The issuer: `writing` is the grant's journal append in flight and `granted` its possible
/// execution recorded; a grant is delivered (`child` 1, its exit 2) only while the deadline is
/// ahead; an append that failed records no grant and delivers none, no step. `late`
/// is the deadline passed on the boot's monotonic clock and `closed` the close passed on it.
/// The host: `held` is the operation's hold on the handoff, and `outcome` its sealed end: none
/// (0), committed (1), closed (2) or retired (3); its terminal record answers what it sealed, a
/// close as a seal after whatever the journal recorded. `early` books a close sealed before the
/// close and `late_commit` a commit sealed past it. A signalled restart that never granted, past
/// its deadline, starts nothing more and stops holding before it ends. The update: `latch` is
/// its handoff latched, which the host's epoch survives (`epoch` 1) until the handoff goes on
/// (`epoch` 2); `cut` books a handoff that left an open grant behind. `heard` is the old host's
/// `closed` reply, and `answer` a successor's, which it may give from the journal for a
/// recorded grant past the close (a cold or asked restart's: a private one's successor keeps
/// refusing). What the harness does on either is `NativeClosedWatch`'s.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_handoff_hold_model() -> Model {
    aterm_spec::ty_model! {
        NativeHandoffHold {
            const Buggy = 0;
            var held = 1; var writing = 0; var granted = 0; var child = 0; var late = 0;
            var closed = 0; var outcome = 0; var early = 0; var late_commit = 0;
            var latch = 0; var epoch = 1; var cut = 0;
            var heard = 0; var answer = 0;
            action Reserve when (epoch == 1 && late == 0 && outcome == 0 && granted == 0
                && writing == 0) {
                writing = 1;
            }
            action GrantAck when (writing == 1) { writing = 0; granted = 1; }
            action Deliver when (epoch == 1 && late == 0 && outcome == 0 && granted == 1
                && child == 0) {
                child = 1;
            }
            action ChildExits when (child == 1) { child = 2; }
            action Expire when (late == 0) { late = 1; }
            action PassClose when (late == 1 && closed == 0) { closed = 1; }
            // A slow commit inside the grace commits; none commits past the close.
            action Commit when (epoch == 1 && outcome == 0 && child == 1
                && (closed == 0 || Buggy == 1)) {
                outcome = 1; held = 0; late_commit = closed;
            }
            // The close seals an operation nothing else ended, once its append is a fact: one
            // whose grant may have run, or one whose append failed and left a byte or a grant
            // undecided.
            action Sweep when (epoch == 1 && outcome == 0 && writing == 0
                && (closed == 1 || Buggy == 2)) {
                outcome = 2; early = 1 - closed; held = if Buggy == 6 {held} else {0};
            }
            action ReleaseUngranted when (epoch == 1 && outcome == 0 && late == 1 && held == 1
                && writing == 0 && (granted == 0 || Buggy == 3)) {
                held = 0;
            }
            action Retire when (epoch == 1 && outcome == 0 && (child == 0 || child == 2) && writing == 0) {
                outcome = 3; held = 0;
            }
            action Latch when (epoch == 1 && latch == 0) { latch = 1; }
            action Handoff when (epoch == 1 && latch == 1 && (held == 0 || Buggy == 4)) {
                cut = if outcome == 0 && (granted == 1 || writing == 1) {1} else {0};
                epoch = 2;
            }
            action Hear when (epoch == 1 && outcome == 2 && heard == 0) { heard = 1; }
            action Answer when (epoch == 2 && granted == 1 && answer == 0
                && (closed == 1 || Buggy == 5)) {
                answer = 1;
            }
            invariant NoCommitPastClose: late_commit == 0;
            invariant NoCloseBeforeClose: early == 0;
            // Released with a sealed end the old host's terminal record answers, or an ungranted
            // restart past its deadline, which starts nothing more.
            invariant ReleaseOnlyAnswerable: held == 1 || outcome == 1 || outcome == 3
                || (outcome == 2 && closed == 1) || (outcome == 0 && granted == 0 && late == 1);
            invariant HandoffNeverCutsAnOpenGrant: cut == 0;
            invariant ClosedAnswerOnlyPastClose: answer == 0 || closed == 1;
            invariant SealedEndReleases: outcome == 0 || held == 0;
            bound Finite: 0 <= held && held <= 1 && 0 <= writing && writing <= 1
                && 0 <= granted && granted <= 1 && 0 <= child && child <= 2
                && 0 <= late && late <= 1 && 0 <= closed && closed <= 1
                && 0 <= outcome && outcome <= 3 && 0 <= early && early <= 1
                && 0 <= late_commit && late_commit <= 1 && 0 <= latch && latch <= 1
                && 1 <= epoch && epoch <= 2 && 0 <= cut && cut <= 1
                && 0 <= heard && heard <= 1 && 0 <= answer && answer <= 1;
        }
    }
}
