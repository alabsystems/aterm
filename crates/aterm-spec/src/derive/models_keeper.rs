// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pty keeper (`docs/DESIGN-pty-keeper-2026-09-26.md` §6.1, §6.2): custody of every
//! terminal's master outside the window, and the brake on bringing a window back.
//!
//! [`pty_keeper_custody_model`] is who holds and who reads each master across a window, its
//! update successor, a relaunched window and the keeper; [`keeper_relaunch_brake_model`] is the
//! writer/reader pair that decides whether a death is followed by a relaunch;
//! [`keeper_death_judgement_model`] is how the keeper judges a window's end. The real code is
//! `aterm-keeper`'s `KeeperCore` and `RelaunchBrake`; the Tier-1 binds are
//! `crates/aterm-keeper/src/conformance_custody.rs`.

use super::Model;

/// Custody of two masters, `a` and `b`, across four processes: `w` (the registered window), `s`
/// (an update successor `w` named), `r` (a window the keeper relaunched) and `k` (the keeper).
/// Two is the smallest pool in which a partial offer differs from a whole one.
///
/// Per master: `*_holds_*` (the process has a descriptor for it) and `*_reads_*` (it is that
/// master's reader). The keeper holds a custody copy (`k_holds_*`) and never reads; `orphan_*`
/// is its verdict that the master's last claimant died without quitting. `closed_*`: the tab was
/// closed (its shell hung up, F3). `ever_registered_*`: the keeper was told of it at least once.
/// `lost_*`: a live, unreleased shell lost its last holder, so the kernel hung it up. Globals:
/// `pending` (an update in flight: PENDING was sent before the grant), `committed`, `bye` (the
/// window quit), the relaunch count and the keeper's faults.
///
/// The keeper's moves are `KeeperCore`'s: register (a claim from any live holder, also the
/// re-registration after a keeper restart), honour a release (in the closing step: the keeper
/// reads a RELEASE before anything later on that stream), honour a BYE, orphan on a crash (never
/// on an update's hold), relaunch under the brake, and offer: only an orphan, only to the
/// relaunched window, only when the holder scan finds no live window or successor holding it
/// and the shell is alive, keeping its own copy.
///
/// Invariants: `NeverTwoReadersA`/`B` (never two readers on one master, the silent unrecoverable
/// interleaving); `OfferOnlyWithNoLiveHolderA`/`B` (the relaunched window holds a master only
/// once the window and the successor are both gone); `ClosedNeverReachesAWindowA`/`B`;
/// `NoRelaunchAfterQuit`; `LossNeedsKeeperFaultA`/`B` (a live, unreleased shell is hung up only
/// if the keeper also failed or the window died before registering it). `RelaunchSpace` is a
/// `bound` on the relaunch count; the brake's policy is [`keeper_relaunch_brake_model`]'s.
///
/// `Buggy = 1` is eight defects, each falsifying an invariant by its own schedule: an EOF on a
/// live window's link read as its death (`LinkDrops`); the holder scan skipped, so a committed
/// successor's masters are orphaned and offered (`Commit`, `OfferA`/`B`); the keeper closing its
/// copy when it offers (`OfferA`/`B`); a BYE read as a crash (`KeeperHonoursBye`, `Relaunch`); a
/// RELEASE forgotten and the shell's liveness unread at the offer (`CloseTabA`/`B`); a RELEASE
/// from one of two claimants closing the copy the other depends on (`OutgoingReleasesA`; real:
/// `KeeperCore::release`); the committed successor's quit read as a crash (`SuccessorQuits`;
/// real: `KeeperCore::resolve_holds`); and the outgoing window's Commit read as a quit when its
/// successor died first (`SuccessorCrashesFirst`; real: `KeeperCore::peer_died`). Two more are
/// caught only by the conformance's negative control (no invariant here states liveness): an
/// offer left with the relaunched window's dropped link instead of returned to Orphaned
/// (`RelaunchedLinkDrops`), and a BYE on the reconnected link that leaves the dropped one's end
/// reading as a crash (`RelaunchedWindowQuits`).
///
/// After Commit the outgoing window's stream is not yet read to its end (`o_draining`), so it
/// still claims every master it registered (`o_claims_a`). Its late RELEASE
/// (`OutgoingReleasesA`), the successor's tab close (`SuccessorClosesTabA`) and the keeper
/// reading its EOF and exit (`OutgoingDrained`) interleave. The successor's crash or quit
/// otherwise follows the drain (the outgoing window `_exit`s at Commit, so it drains in
/// milliseconds), except a crash within those milliseconds, which the keeper reads with the drain
/// in one step (`SuccessorCrashesFirst`). These are stated for master `a` only: one master
/// suffices, and the `b` twins would push the `Buggy = 1` space past the interpreter's bound.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pty_keeper_custody_model() -> Model {
    crate::ty_model! {
        PtyKeeperCustody {
            const Buggy = 0;
            const MaxRelaunch = 3;
            var w_live = 1;
            var w_holds_a = 1;
            var w_reads_a = 1;
            var w_holds_b = 1;
            var w_reads_b = 1;
            var s_live = 0;
            var s_holds_a = 0;
            var s_reads_a = 0;
            var s_holds_b = 0;
            var s_reads_b = 0;
            var r_live = 0;
            var r_holds_a = 0;
            var r_reads_a = 0;
            var r_holds_b = 0;
            var r_reads_b = 0;
            var k_live = 1;
            var k_holds_a = 0;
            var k_holds_b = 0;
            var orphan_a = 0;
            var orphan_b = 0;
            var pending = 0;
            var committed = 0;
            var bye = 0;
            var closed_a = 0;
            var closed_b = 0;
            var ever_registered_a = 0;
            var ever_registered_b = 0;
            var lost_a = 0;
            var lost_b = 0;
            var relaunches = 0;
            var k_faults = 0;
            // The outgoing window's stream after Commit: its late frames (a RELEASE from a
            // `Session::drop`), EOF and exit status are unread, so it still claims every master
            // the keeper held at Commit (`o_claims_*`).
            var o_draining = 0;
            var o_claims_a = 0;
            // The relaunched window's link dropped, its unregistered offers went back to
            // Orphaned (`RelaunchedLinkDrops`), and it reconnected: its later BYE arrives on
            // another link. The mutant returns nothing, so never sets this.
            var r_relinked = 0;

            // A claim: the window's first REGISTER, a committed successor's, the relaunched
            // window's after adopting, or anyone's after a keeper restart.
            action RegisterA when (
                k_live == 1 && k_holds_a == 0 && closed_a == 0 &&
                ((w_live == 1 && w_holds_a == 1) ||
                 (s_live == 1 && committed == 1 && s_holds_a == 1) ||
                 (r_live == 1 && r_reads_a == 1))
            ) {
                k_holds_a = 1;
                ever_registered_a = 1;
                orphan_a = 0;
            }
            action RegisterB when (
                k_live == 1 && k_holds_b == 0 && closed_b == 0 &&
                ((w_live == 1 && w_holds_b == 1) ||
                 (s_live == 1 && committed == 1 && s_holds_b == 1) ||
                 (r_live == 1 && r_reads_b == 1))
            ) {
                k_holds_b = 1;
                ever_registered_b = 1;
                orphan_b = 0;
            }
            // Closing a tab or pane: its reader closes its copy, hangs the shell
            // up (F3) and sends RELEASE, which the keeper reads before anything
            // later on that stream — so the release and the keeper closing its
            // copy are one step. Never while an update is in flight.
            action CloseTabA when (
                closed_a == 0 &&
                ((w_live == 1 && w_reads_a == 1 && pending == 0) || (r_live == 1 && r_reads_a == 1))
            ) {
                closed_a = 1;
                w_holds_a = 0;
                w_reads_a = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_a = if Buggy == 1 { orphan_a } else { 0 };
            }
            action CloseTabB when (
                closed_b == 0 &&
                ((w_live == 1 && w_reads_b == 1 && pending == 0) || (r_live == 1 && r_reads_b == 1))
            ) {
                closed_b = 1;
                w_holds_b = 0;
                w_reads_b = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_b = if Buggy == 1 { orphan_b } else { 0 };
            }
            // Cmd-Q: BYE precedes EOF in the stream, so intent and exit are one step.
            action Quit when (w_live == 1 && pending == 0) {
                w_live = 0;
                bye = 1;
                w_holds_a = 0;
                w_reads_a = 0;
                w_holds_b = 0;
                w_reads_b = 0;
            }
            // Honoured, the keeper closes its copies and the kernel hangs the shells up. Read
            // as a crash, they become orphans.
            action KeeperHonoursBye when (
                bye == 1 && k_live == 1 && w_live == 0 && s_live == 0 && r_live == 0 &&
                (k_holds_a == 1 || k_holds_b == 1) && orphan_a == 0 && orphan_b == 0
            ) {
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_b = if Buggy == 1 { k_holds_b } else { 0 };
            }
            // A fatal signal, SIGKILL, Force Quit. A shell only the window held is hung up by
            // the kernel. With an update in flight the keeper holds (the successor may still
            // register); otherwise it orphans what it holds.
            action WindowCrashes when (w_live == 1) {
                w_live = 0;
                w_holds_a = 0;
                w_reads_a = 0;
                w_holds_b = 0;
                w_reads_b = 0;
                orphan_a = if pending == 0 { k_holds_a } else { orphan_a };
                orphan_b = if pending == 0 { k_holds_b } else { orphan_b };
                lost_a = if w_holds_a == 1 && k_holds_a == 0 && s_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if w_holds_b == 1 && k_holds_b == 0 && s_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            // The window's link drops (EOF) while the window lives, and
            // reconnects. Not a death: the keeper changes nothing. The mutant
            // reads the EOF as a death and orphans what the window still reads.
            action LinkDrops when (w_live == 1 && orphan_a == 0 && orphan_b == 0) {
                orphan_a = if Buggy == 1 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 1 { k_holds_b } else { orphan_b };
            }
            // The seamless update: PENDING first, then duplicates to the
            // successor, the window's readers parked.
            action StartSuccessor when (
                w_live == 1 && s_live == 0 && pending == 0 && committed == 0 && bye == 0
            ) {
                s_live = 1;
                pending = 1;
                s_holds_a = w_holds_a;
                s_holds_b = w_holds_b;
                w_reads_a = 0;
                w_reads_b = 0;
            }
            // Commit and the window's `_exit`, one step (`CommitAtomically`): a hold, never an
            // orphan, since the successor holds every master and registers them. One that takes
            // no part in the keeper (keeper off, or an older build) registers nothing, and the
            // keeper holds them for it until it ends.
            action Commit when (w_live == 1 && s_live == 1 && pending == 1) {
                w_live = 0;
                w_holds_a = 0;
                w_holds_b = 0;
                s_reads_a = s_holds_a;
                s_reads_b = s_holds_b;
                committed = 1;
                pending = 0;
                o_draining = 1;
                o_claims_a = k_holds_a;
                orphan_a = if Buggy == 1 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 1 { k_holds_b } else { orphan_b };
            }
            // Two claimants: the outgoing window's late RELEASE of a master its successor now
            // reads, written before its `_exit` and read after the successor registered. It
            // ends the outgoing window's claim only; the keeper keeps its copy for the
            // successor. The mutant closes it, and a successor crash next loses the shell with
            // no keeper fault. (If the successor already closed the tab, this release is the
            // last claim and closes.)
            action OutgoingReleasesA when (o_draining == 1 && o_claims_a == 1 && k_holds_a == 1) {
                o_claims_a = 0;
                k_holds_a = if Buggy == 1 || closed_a == 1 { 0 } else { k_holds_a };
            }
            // The successor closes a tab: it hangs the shell up (F3) and releases. While the
            // outgoing window still claims the master the keeper keeps the record for that
            // claimant's end; otherwise the release closes it.
            action SuccessorClosesTabA when (
                o_draining == 1 && s_live == 1 && s_reads_a == 1 && closed_a == 0
            ) {
                closed_a = 1;
                s_holds_a = 0;
                s_reads_a = 0;
                k_holds_a = if o_draining == 1 && o_claims_a == 1 { k_holds_a } else { 0 };
            }
            // The keeper reads the outgoing window's EOF and exit status: a master only it
            // still claimed had its shell hung up by the successor's close, so it is closed,
            // never orphaned.
            action OutgoingDrained when (o_draining == 1) {
                o_draining = 0;
                o_claims_a = 0;
                k_holds_a = if closed_a == 1 { 0 } else { k_holds_a };
            }
            // The window proves the successor dead and resumes its readers.
            action Rollback when (w_live == 1 && s_live == 1 && pending == 1) {
                s_live = 0;
                s_holds_a = 0;
                s_holds_b = 0;
                pending = 0;
                w_reads_a = w_holds_a;
                w_reads_b = w_holds_b;
            }
            // The window died before Commit: the successor fail-stops (exit 74, F11), and the
            // keeper's hold becomes an orphan.
            action SuccessorFailStops when (w_live == 0 && s_live == 1 && committed == 0) {
                s_live = 0;
                pending = 0;
                s_holds_a = 0;
                s_holds_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if s_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action SuccessorCrashes when (s_live == 1 && committed == 1 && o_draining == 0) {
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if s_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            // The successor dies first: it crashes within the Commit's milliseconds, before the
            // keeper judged the outgoing window's drain, and the keeper judges both ends in one
            // step (to the state the drain and then the crash reach). The outgoing window's
            // `exit(0)` with its crash marker removed is the Commit (handing off is a clean
            // end), not a quit whose BYE was lost: what it handed the dead successor is an
            // orphan. The mutant reads it as that quit (§5.4 row 3's cross-check, the successor
            // unread), so every shell the update carried is hung up.
            action SuccessorCrashesFirst when (o_draining == 1 && s_live == 1 && committed == 1) {
                o_draining = 0;
                o_claims_a = 0;
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                k_holds_a = if closed_a == 1 || Buggy == 1 { 0 } else { k_holds_a };
                k_holds_b = if Buggy == 1 { 0 } else { k_holds_b };
                orphan_a = if closed_a == 1 || Buggy == 1 { 0 } else { k_holds_a };
                orphan_b = if Buggy == 1 { 0 } else { k_holds_b };
                lost_a = if s_holds_a == 1 && closed_a == 0 && (k_holds_a == 0 || Buggy == 1) { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && closed_b == 0 && (k_holds_b == 0 || Buggy == 1) { 1 } else { lost_b };
            }
            // The person quits the committed successor. One that registered says BYE; one that
            // takes no part says nothing, and the keeper reads its `exit(0)` after the outgoing
            // window's clean Commit (§5.4 row 3). Either way a quit: the copies close and the
            // shells are hung up. The mutant judges the hold by its holders alone, reads the
            // quit as a crash, and orphans every shell the update carried.
            action SuccessorQuits when (s_live == 1 && committed == 1 && o_draining == 0) {
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                bye = 1;
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_b = if Buggy == 1 { k_holds_b } else { 0 };
            }
            // The keeper brings a window back: it holds an orphan, no window or
            // successor is connected, no BYE was read, and the brake permits.
            action Relaunch when (
                k_live == 1 && (w_live == 0 || Buggy == 1) && s_live == 0 && r_live == 0 &&
                (bye == 0 || Buggy == 1) && (orphan_a == 1 || orphan_b == 1) &&
                relaunches <= MaxRelaunch - 1
            ) {
                r_live = 1;
                relaunches = relaunches + 1;
            }
            // The offer: an orphan, to the relaunched window, after the holder scan and the
            // shell's liveness. The keeper keeps its copy.
            action OfferA when (
                k_live == 1 && r_live == 1 && orphan_a == 1 && k_holds_a == 1 && r_holds_a == 0 &&
                (w_holds_a + s_holds_a == 0 || Buggy == 1) && (closed_a == 0 || Buggy == 1)
            ) {
                r_holds_a = 1;
                orphan_a = 0;
                k_holds_a = if Buggy == 1 { 0 } else { 1 };
            }
            action OfferB when (
                k_live == 1 && r_live == 1 && orphan_b == 1 && k_holds_b == 1 && r_holds_b == 0 &&
                (w_holds_b + s_holds_b == 0 || Buggy == 1) && (closed_b == 0 || Buggy == 1)
            ) {
                r_holds_b = 1;
                orphan_b = 0;
                k_holds_b = if Buggy == 1 { 0 } else { 1 };
            }
            // The relaunched window reads what it was offered and REGISTERs it, claiming the
            // record whether still Offered or back to Orphaned after its link dropped
            // (`RelaunchedLinkDrops`): REGISTER proves possession.
            action AdoptA when (r_live == 1 && r_holds_a == 1 && r_reads_a == 0) {
                r_reads_a = 1;
                orphan_a = 0;
            }
            action AdoptB when (r_live == 1 && r_holds_b == 1 && r_reads_b == 0) {
                r_reads_b = 1;
                orphan_b = 0;
            }
            // The relaunched window's link drops before it REGISTERed what it was offered (its
            // boot HELLO timed out, or the OFFER's send failed) and reconnects. An OFFER is made
            // to a connection: every master offered on the dropped link is an orphan again,
            // the window still holding its descriptor, so the holder scan keeps a second offer
            // away and the new link may still REGISTER it (`AdoptA`). The mutant leaves the
            // offer with the dead link. (After the HELLO's answer no orphan is unoffered: the
            // keeper makes every offer in that one answer.)
            action RelaunchedLinkDrops when (
                k_live == 1 && r_live == 1 &&
                (orphan_a == 0 || r_holds_a == 1) && (orphan_b == 0 || r_holds_b == 1) &&
                ((r_holds_a == 1 && r_reads_a == 0 && k_holds_a == 1 && orphan_a == 0) ||
                 (r_holds_b == 1 && r_reads_b == 0 && k_holds_b == 1 && orphan_b == 0))
            ) {
                orphan_a = if Buggy == 0 && r_holds_a == 1 && r_reads_a == 0 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 0 && r_holds_b == 1 && r_reads_b == 0 { k_holds_b } else { orphan_b };
                r_relinked = if Buggy == 0 { 1 } else { r_relinked };
            }
            // The person quits the relaunched window: BYE on its current link, then its end.
            // The keeper closes the copies it read, returns an unregistered offer to Orphaned,
            // and relaunches nothing: a BYE is the process's, so a link that dropped earlier
            // (`r_relinked`) quit too. The mutant reads the BYE on its own link only, so the
            // dead link's returned offers read as a crash. Stated only once a link dropped (a
            // quit on the one link is `KeeperHonoursBye`'s, and the extra schedules would push
            // the `Buggy = 1` space past the interpreter's bound).
            action RelaunchedWindowQuits when (r_live == 1 && r_relinked == 1) {
                r_live = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                k_holds_a = if r_reads_a == 1 { 0 } else { k_holds_a };
                k_holds_b = if r_reads_b == 1 { 0 } else { k_holds_b };
                orphan_a = if r_holds_a == 1 && r_reads_a == 0 { k_holds_a } else { orphan_a };
                orphan_b = if r_holds_b == 1 && r_reads_b == 0 { k_holds_b } else { orphan_b };
                bye = if Buggy == 1 && r_relinked == 1 { 0 } else { 1 };
                r_relinked = 0;
            }
            action RelaunchedWindowCrashes when (r_live == 1) {
                r_live = 0;
                r_relinked = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if r_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if r_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            // The keeper dies: every custody copy closes, and a shell no window holds is hung up.
            action KeeperCrashes when (k_live == 1 && k_faults == 0) {
                k_live = 0;
                k_faults = 1;
                k_holds_a = 0;
                k_holds_b = 0;
                orphan_a = 0;
                orphan_b = 0;
                o_draining = 0;
                o_claims_a = 0;
                r_relinked = 0;
                lost_a = if k_holds_a == 1 && w_holds_a + s_holds_a + r_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if k_holds_b == 1 && w_holds_b + s_holds_b + r_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            // launchd respawns it: an empty table and a fresh brake.
            action KeeperRestarts when (k_live == 0) {
                k_live = 1;
                relaunches = 0;
            }

            invariant NeverTwoReadersA: w_reads_a + s_reads_a + r_reads_a <= 1;
            invariant NeverTwoReadersB: w_reads_b + s_reads_b + r_reads_b <= 1;
            invariant OfferOnlyWithNoLiveHolderA: r_holds_a == 0 || (w_live == 0 && s_live == 0);
            invariant OfferOnlyWithNoLiveHolderB: r_holds_b == 0 || (w_live == 0 && s_live == 0);
            invariant ClosedNeverReachesAWindowA: closed_a == 0 || s_holds_a + r_holds_a == 0;
            invariant ClosedNeverReachesAWindowB: closed_b == 0 || s_holds_b + r_holds_b == 0;
            invariant NoRelaunchAfterQuit: bye == 0 || r_live == 0;
            invariant LossNeedsKeeperFaultA: lost_a == 0 || k_faults > 0 || ever_registered_a == 0;
            invariant LossNeedsKeeperFaultB: lost_b == 0 || k_faults > 0 || ever_registered_b == 0;
            bound RelaunchSpace: relaunches <= MaxRelaunch;
        }
    }
}

/// The relaunch brake as a writer/reader pair (the shape of
/// `NativeUpdateFailedMarkSuppression`): the keeper writes `streak` and `held` each time it
/// relaunches or gives up, and reads them at the next death to decide relaunch or hold.
///
/// `kind` is the environment's pick, made once: 1 the relaunched windows keep dying; 2 a boot
/// trial, in which the updater needs `MAX_BOOT_ATTEMPTS = 3` relaunches to revert a bad build
/// unattended (F17). `phase`: 0 running, 1 a death to decide, 2 a relaunch in flight, 3 held
/// (only a manual launch, a HELLO the keeper did not cause, resets it). `clean`: the death was a
/// quit. `grants`: relaunches decided since the last reset; `after_quit`: one was decided for a
/// quit.
///
/// Invariants: `CleanQuitNeverRelaunches`; `StreakBounded` (never more than `MaxStreak` failed
/// relaunches in a row: a design claim the relaunch storm falsifies, so not a `bound`);
/// `HoldOnlyWhenSpent` (a hold only after `MaxStreak` relaunches since the last reset, so a boot
/// trial gets its relaunches).
///
/// `Buggy = 1` has three defects, each scoped to one `kind` so none masks another: the spent
/// brake written as `not_before = 0` meaning never and read as already elapsed (kind 1: a
/// relaunch storm); the death that opens a boot trial counted as a failure of its own (kind 2: a
/// hold after two relaunches); and a quit counted as a death.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keeper_relaunch_brake_model() -> Model {
    crate::ty_model! {
        KeeperRelaunchBrake {
            const Buggy = 0;
            const MaxStreak = 3;
            var kind = 0;
            var phase = 0;
            var clean = 0;
            var streak = 0;
            var held = 0;
            var grants = 0;
            var after_quit = 0;

            // The environment's one pick: windows that keep dying, or a boot trial.
            action PickRepeatedDeaths when (kind == 0) {
                kind = 1;
            }
            action PickBootTrial when (kind == 0) {
                kind = 2;
            }
            // The registered window dies with orphans (not a quit).
            action Death when (kind > 0 && phase == 0) {
                phase = 1;
                clean = 0;
            }
            // The registered window quits (BYE before EOF).
            action CleanQuit when (kind > 0 && phase == 0) {
                phase = 1;
                clean = 1;
            }
            // The reader decides: relaunch while the streak has room, else hold; a quit is
            // neither. The writer records it: a relaunch counts toward the streak, and a spent
            // streak is written as a hold (its own field, never a `not_before` sentinel).
            action Decide when (phase == 1) {
                phase = if clean == 1 && Buggy == 0 { 0 } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { 2 } else { 3 }
                };
                grants = if clean == 1 && Buggy == 0 { grants } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { grants + 1 } else { grants }
                };
                streak = if clean == 1 && Buggy == 0 { streak } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { streak + 1 } else { streak }
                };
                held = if clean == 1 && Buggy == 0 { held } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { 0 } else { 1 }
                };
                after_quit = if clean == 1 && Buggy == 1 { 1 } else { after_quit };
            }
            // The relaunched window dies early (§6.2's five minutes) or never says HELLO: a
            // failed relaunch, and another death to decide. The mutant (kind 2) counts the
            // death that opened the trial again, spending the streak one relaunch early.
            action RelaunchFails when (phase == 2) {
                phase = 1;
                clean = 0;
                streak = if Buggy == 1 && kind == 2 && streak == 1 && grants == 1 { streak + 1 } else { streak };
            }
            // It lived `HEALTHY_MS` (30 minutes): the streak resets.
            action Healthy when (phase == 2) {
                phase = 0;
                streak = 0;
                grants = 0;
            }
            // A launch the keeper did not cause: the brake resets.
            action ManualLaunch when (phase == 3) {
                phase = 0;
                streak = 0;
                grants = 0;
                held = 0;
            }

            invariant CleanQuitNeverRelaunches: after_quit == 0;
            invariant StreakBounded: streak <= MaxStreak;
            invariant HoldOnlyWhenSpent: held == 0 || grants > MaxStreak - 1;
        }
    }
}

/// How the keeper judges one window's end (§5.4's classifier, with row 3's crash-marker
/// cross-check), and the prune of an orphan whose shell died. The real code is `aterm-keeper`'s
/// `classify_death`, `KeeperCore::peer_died` and `KeeperCore::prune_dead_orphans`; the Tier-1
/// bind is `crates/aterm-keeper/src/conformance_custody.rs`.
///
/// The window: `sends_bye` (its HELLO said it sends BYE) and `names_marker` (it named its crash
/// marker, and the keeper found it held), both 1 for this build; the environment may pick an
/// older window (`PickP3Window`: BYE, no marker; `PickPreByeWindow`: neither). `ending`, picked
/// once: 1 a quit whose BYE arrived, 2 a quit whose BYE was lost, 3 a death with no exit path
/// (SIGKILL, jetsam, a fatal signal), 4 an unwinding panic (`exit(101)` through the same
/// `atexit` that unlinks the marker). `quit` is the ghost truth: the exit path ran to `exit(0)`.
///
/// The evidence: `bye`; `status`, the kernel's wait status (0 none, the watch never armed; 1
/// `exit(0)`; 2 another exit; 3 a signal); `marker` (0 no evidence, 1 released, 2 dead: there,
/// lock free, 3 held: there, locked by a forked child). It degrades before the keeper judges:
/// the status can be missing (`StatusLost`), another start's sweep can remove a dead marker
/// (`MarkerSwept`), a child can hold its lock (`MarkerInherited`).
///
/// `Judge` is the classifier: a quit is a BYE, or `exit(0)` with the marker released (for a
/// window that names no marker and sends no BYE, `exit(0)` alone). `judged_quit` is its verdict,
/// `orphaned` the record it leaves on offer (only while the shell lives), `ever_orphaned` whether
/// it ever did. The shell may die after the window (`ShellDies`); the keeper watches each
/// orphan's leader (`watch_pending`: its exit is on the way) and `KeeperSeesShellExit` prunes it.
///
/// Invariants: `CleanQuitNeverResurrected` (a window that quit is never orphaned, given the
/// kernel's status and a named marker; a keeper-aware window with no marker evidence that ended
/// without BYE is judged a crash, by design); `CrashNeverClosed` (an end that was not a quit is
/// never closed as one); `NeverListsTheDead` (an orphan is listed only while its shell lives or
/// its exit is still on the way).
///
/// `Buggy = 1`, each mutant scoped to the endings it breaks: for a quit, the classifier without
/// the marker (a lost BYE resurrects a clean quit); for any other end, the marker trusted alone
/// (a panic's released marker, or a swept one, read as a quit); and a keeper that keeps an orphan
/// whose shell died (listed until the next HELLO).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keeper_death_judgement_model() -> Model {
    crate::ty_model! {
        KeeperDeathJudgement {
            const Buggy = 0;
            var sends_bye = 1;
            var names_marker = 1;
            var ending = 0;
            var quit = 0;
            var bye = 0;
            var status = 0;
            var marker = 0;
            var judged = 0;
            var judged_quit = 0;
            var orphaned = 0;
            var ever_orphaned = 0;
            var shell_live = 1;
            var watch_pending = 0;

            // The environment's pick of an older window: BYE but no marker, or neither.
            action PickP3Window when (ending == 0 && names_marker == 1) {
                names_marker = 0;
            }
            action PickPreByeWindow when (ending == 0 && sends_bye == 1) {
                sends_bye = 0;
                names_marker = 0;
            }
            // The window's end.
            action QuitWithBye when (ending == 0) {
                ending = 1;
                quit = 1;
                bye = sends_bye;
                status = 1;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            action QuitByeLost when (ending == 0 && sends_bye == 1) {
                ending = 2;
                quit = 1;
                bye = 0;
                status = 1;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            action Killed when (ending == 0) {
                ending = 3;
                status = 3;
                marker = if names_marker == 1 { 2 } else { 0 };
            }
            action PanicExit when (ending == 0) {
                ending = 4;
                status = 2;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            // The evidence degrades before the keeper judges.
            action StatusLost when (ending > 0 && judged == 0 && status > 0) {
                status = 0;
            }
            action MarkerSwept when (judged == 0 && marker == 2) {
                marker = 1;
            }
            action MarkerInherited when (judged == 0 && marker == 2) {
                marker = 3;
            }
            // The classifier. Buggy: for a quit, without the marker; for any other end, the
            // marker trusted alone.
            action Judge when (ending > 0 && judged == 0) {
                judged = 1;
                judged_quit = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 1 } else { 0 };
                orphaned = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 0 } else { shell_live };
                ever_orphaned = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 0 } else { shell_live };
            }
            // The shell ends after its window; an orphan's leader is watched.
            action ShellDies when (ending > 0 && shell_live == 1) {
                shell_live = 0;
                watch_pending = orphaned;
            }
            // The keeper reads the leader's exit and prunes the orphan. Buggy: it keeps it
            // until the next HELLO.
            action KeeperSeesShellExit when (watch_pending == 1) {
                watch_pending = 0;
                orphaned = if Buggy == 1 { orphaned } else { 0 };
            }

            invariant CleanQuitNeverResurrected: ever_orphaned == 0 || quit == 0 || status == 0 || names_marker == 0;
            invariant CrashNeverClosed: judged_quit == 0 || quit == 1;
            invariant NeverListsTheDead: orphaned == 0 || shell_live == 1 || watch_pending == 1;
        }
    }
}
