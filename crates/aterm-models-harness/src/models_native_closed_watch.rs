// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The harness's watch on a native restart that reached its close uncommitted (`closed`): its
/// exec grant may have started a grantee, which only observation can adopt or rule out, and no
/// fresh plan of the lane starts or signals an agent beside it. One restart, its one grantee and
/// the lane's next plans.
/// The world: `closed` is the restart past its close, after which no grant is delivered;
/// `grantee` is its grantee, never started (0), running (1) or exited (2), and `holds` the
/// running grantee registered on the conversation the watch reads (a fresh start's watch reads
/// none). `shell` is the restart's shell, still the process it typed at (1) or gone by its
/// kernel start (0), and `tab` that shell holding its tab, which it gives up while a foreground
/// job of it runs. The grantee is the shell's child, started by the line the restart typed, and
/// runs the restart's launcher or a command its grant execs. `other` is another child the shell
/// keeps in the background: none (0), one that runs no image the restart's grant may run (1),
/// such as a prompt helper (powerlevel10k's gitstatusd) or a job of the person's, or one that
/// runs such an image (2), such as an agent the person started there, which the watch cannot
/// tell from the grantee. It holds no conversation the watch reads, and is no child of a shell
/// that is gone. The shell leads its terminal's session, so its exit hangs the terminal up: a
/// grantee that holds no conversation the watch reads ends with it, and one that holds it may
/// run on. `boot` is a later boot of the machine, which ends every process of the restart's.
/// The harness: `watch` is the watch the record keeps from the `closed` answer on. It adopts
/// (`adopted`) a running grantee that holds the conversation once its shell gave up the tab or
/// is gone, as the relaunch the commit would have confirmed, and it clears where no child of the
/// shell runs an image the grant may run: the grantee never started or exited, and no other
/// child runs one; a child that runs no such image never holds it. Anything else, a fact that
/// did not read included, holds. `start` is a fresh plan that starts the conversation's agent,
/// and `signal` one that signals an agent: the adopted grantee (1) or another process (2).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_closed_watch_model() -> Model {
    aterm_spec::ty_model! {
        NativeClosedWatch {
            const Buggy = 0;
            var closed = 0; var grantee = 0; var holds = 0; var shell = 1; var tab = 1;
            var other = 0; var boot = 0; var watch = 0; var adopted = 0; var start = 0;
            var signal = 0;
            action Launch when (closed == 0 && grantee == 0 && shell == 1 && boot == 0) {
                grantee = 1; tab = 0;
            }
            action Register when (grantee == 1 && holds == 0 && shell == 1) { holds = 1; }
            action Exit when (grantee == 1) { grantee = 2; holds = 0; tab = shell; }
            action Spawn when (shell == 1 && other == 0) { other = 1; }
            action SpawnAlike when (shell == 1 && other == 0) { other = 2; }
            action OtherExits when (other > 0) { other = 0; }
            action ShellExits when (shell == 1) {
                shell = 0; tab = 0; other = 0;
                grantee = if grantee == 1 && holds == 0 {2} else {grantee};
            }
            action Reboot when (boot == 0) {
                boot = 1; shell = 0; tab = 0; holds = 0; other = 0;
                grantee = if grantee == 0 {0} else {2};
            }
            action Close when (closed == 0) { closed = 1; watch = 1; }
            action Adopt when (watch == 1 && grantee == 1 && holds == 1 && tab == 0) {
                watch = 0; adopted = 1;
            }
            action Clear when (watch == 1 && (grantee == 0 || grantee == 2 || Buggy == 1)
                && other <= 1) {
                watch = 0;
            }
            // A start over a watch kept, or beside the grantee it adopted.
            action Start when (closed == 1 && start == 0 && (watch == 0 || Buggy == 2)
                && (adopted == 0 || grantee == 2 || Buggy == 3)) {
                start = 1;
            }
            action SignalAdopted when (signal == 0 && watch == 0 && adopted == 1 && grantee == 1) {
                signal = 1;
            }
            // A move whose subject is not the grantee, over a watch kept: a settle that clears
            // on a hold.
            action SignalOther when (closed == 1 && signal == 0 && (watch == 0 || Buggy == 4)
                && (adopted == 0 || grantee == 2)) {
                signal = 2;
            }
            invariant NoStartBesideTheGrantee: start == 0 || grantee == 0 || grantee == 2;
            invariant SignalsOnlyItsSubject: signal <= 1 || grantee == 0 || grantee == 2;
            bound Finite: 0 <= closed && closed <= 1 && 0 <= grantee && grantee <= 2
                && 0 <= holds && holds <= 1 && 0 <= shell && shell <= 1 && 0 <= tab && tab <= 1
                && 0 <= other && other <= 2
                && 0 <= boot && boot <= 1 && 0 <= watch && watch <= 1
                && 0 <= adopted && adopted <= 1 && 0 <= start && start <= 1
                && 0 <= signal && signal <= 2;
        }
    }
}
