// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Whether a coordinated worker's relaunch after its host ended is a typed plan no native
/// authority gates (RS1). A record that once took a native cold restart keeps its coordination
/// mark for good, but every native act of an aterm process that ended died with its epoch:
/// an agent that ended under an earlier host (`ea`, whatever restart stamp it carried) whose
/// record is coordinated (`co`), whose relaunch defers no cold intent (`de`), whose exit is
/// proven by identity (`pr`), that leaves no native operation or rollback unresolved (`un`) and
/// whose record holds no native act of this host's epoch (`own`) comes back (`ComesBack`).
/// Never for this host's own end (`OnlyAnEarlierHost`), an unproven exit (`ExitProven`), over
/// an unresolved native operation (`NothingUnresolvedReplaced`), or past a native act of this
/// process (`NoActOfThisHost`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn host_ended_relaunch_model() -> Model {
    aterm_spec::ty_model! {
        HostEndedRelaunch {
            const Buggy = 0;
            var own = 0; var ea = 0; var co = 0; var de = 0; var pr = 0; var un = 0;
            var phase = 0; var fresh = 0;
            action Own when (phase == 0 && own == 0) { own = 1; }
            action Earlier when (phase == 0 && ea == 0) { ea = 1; }
            action Coordinated when (phase == 0 && co == 0) { co = 1; }
            action Deferred when (phase == 0 && de == 0) { de = 1; }
            action Proven when (phase == 0 && pr == 0) { pr = 1; }
            action Unresolved when (phase == 0 && un == 0) { un = 1; }
            action Decide when (phase == 0) {
                phase = 1;
                fresh = if Buggy == 2 {0}
                    else {if (own == 0 || Buggy == 5) && (ea == 1 || Buggy == 1) && co == 1 && de == 0 && (pr == 1 || Buggy == 4) && (un == 0 || Buggy == 3) {1} else {0}};
            }
            invariant OnlyAnEarlierHost: phase == 0 || fresh == 0 || ea == 1;
            invariant NothingUnresolvedReplaced: phase == 0 || fresh == 0 || un == 0;
            invariant ExitProven: phase == 0 || fresh == 0 || pr == 1;
            invariant NoActOfThisHost: phase == 0 || fresh == 0 || own == 0;
            invariant ComesBack: phase == 0 || own == 1 || ea == 0 || co == 0 || de == 1 || pr == 0 || un == 1 || fresh == 1;
        }
    }
}
