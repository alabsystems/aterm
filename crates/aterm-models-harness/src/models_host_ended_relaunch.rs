// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Whether a coordinated worker's relaunch after its host ended is a typed plan no native
/// authority gates (RS1), and what proves the exit every relaunch waits on (RS2).
///
/// `Judge` reads the agent's exit by identity. The record names its agent by pid and the kernel
/// start it was born at (`rec`, written when the record bound it), or, written by an earlier
/// build, by pid alone, and then takes the start a snapshot of that pid carries (`sn`, the
/// layout's after a restore, as RS1 carries an earlier host's restart). The kernel's answer for
/// that pid now is `cur`: `0` no process, `1` one born at that start, `2` one born at another
/// time (a reused pid, as after a reboot), `3` a live one whose start cannot be read. The exit
/// is proven (`pr`) only by a free pid or another start under a known start
/// (`ProvenByIdentity`); the relaunch waits (`wt`) only on the process born at that start
/// (`WaitsOnlyOnItsAgent`), never on a bare pid; and an exit that identity can prove is proven
/// or waited on, never left to the stale clock (`NeverHeldPastItsProof`).
///
/// `Decide` is RS1's: a record that once took a native cold restart keeps its coordination mark
/// for good, but every native act of an aterm process that ended died with its epoch. An agent
/// that ended under an earlier host (`ea`, whatever restart stamp it carried) whose record is
/// coordinated (`co`), whose relaunch defers no cold intent (`de`), whose exit `Judge` proved
/// (`pr`), that leaves no native operation or rollback unresolved (`un`) and whose record holds
/// no native act of this host's epoch (`own`) comes back (`ComesBack`). Never for this host's
/// own end (`OnlyAnEarlierHost`), an unproven exit (`ExitProven`), over an unresolved native
/// operation (`NothingUnresolvedReplaced`), or past a native act of this process
/// (`NoActOfThisHost`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn host_ended_relaunch_model() -> Model {
    aterm_spec::ty_model! {
        HostEndedRelaunch {
            const Buggy = 0;
            var own = 0; var ea = 0; var co = 0; var de = 0; var un = 0;
            var rec = 0; var sn = 0; var cur = 0;
            var phase = 0; var pr = 0; var wt = 0; var fresh = 0;
            action Own when (phase == 0 && own == 0) { own = 1; }
            action Earlier when (phase == 0 && ea == 0) { ea = 1; }
            action Coordinated when (phase == 0 && co == 0) { co = 1; }
            action Deferred when (phase == 0 && de == 0) { de = 1; }
            action Unresolved when (phase == 0 && un == 0) { un = 1; }
            action Recorded when (phase == 0 && rec == 0) { rec = 1; }
            action Snapshot when (phase == 0 && sn == 0) { sn = 1; }
            action Same when (phase == 0 && cur == 0) { cur = 1; }
            action Reborn when (phase == 0 && cur == 0) { cur = 2; }
            action Unread when (phase == 0 && cur == 0) { cur = 3; }
            action Judge when (phase == 0) {
                phase = 1;
                pr = if Buggy == 9 {0}
                    else {if cur == 0
                        || ((rec == 1 || (if Buggy == 8 {0} else {sn}) == 1) && cur == 2)
                        || (cur == 1 && Buggy == 6)
                        || (rec == 0 && sn == 0 && Buggy == 7) {1} else {0}};
                wt = if Buggy == 10 {if cur > 0 {1} else {0}}
                    else {if (rec == 1 || (if Buggy == 8 {0} else {sn}) == 1) && cur == 1 {1} else {0}};
            }
            action Decide when (phase == 1) {
                phase = 2;
                fresh = if Buggy == 2 {0}
                    else {if (own == 0 || Buggy == 5) && (ea == 1 || Buggy == 1) && co == 1 && de == 0 && (pr == 1 || Buggy == 4) && (un == 0 || Buggy == 3) {1} else {0}};
            }
            invariant OnlyAnEarlierHost: phase <= 1 || fresh == 0 || ea == 1;
            invariant NothingUnresolvedReplaced: phase <= 1 || fresh == 0 || un == 0;
            invariant ExitProven: phase <= 1 || fresh == 0 || pr == 1;
            invariant NoActOfThisHost: phase <= 1 || fresh == 0 || own == 0;
            invariant ComesBack: phase <= 1 || own == 1 || ea == 0 || co == 0 || de == 1 || pr == 0 || un == 1 || fresh == 1;
            invariant ProvenByIdentity: phase == 0 || pr == 0 || cur == 0 || ((rec == 1 || sn == 1) && cur == 2);
            invariant WaitsOnlyOnItsAgent: phase == 0 || wt == 0 || ((rec == 1 || sn == 1) && cur == 1);
            invariant NeverHeldPastItsProof: phase == 0 || pr == 1 || wt == 1 || cur == 3 || (rec == 0 && sn == 0);
        }
    }
}
