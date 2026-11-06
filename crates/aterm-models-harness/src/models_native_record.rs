// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded local writer race, ambiguous publication, cancellation and ABA.
/// Filesystem acknowledgements and the supplied terminal observation are assumed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_record_model() -> Model {
    aterm_spec::ty_model! {
        NativeRecord {
            const Buggy = 0;
            var disk = 0; var dg = 0; var expected = 0; var eg = 0; var target = 1;
            var attempted = 3; var ag = 0;
            var advanced = 0; var reloaded = 0; var removed = 0; var aba = 0;
            var failed = 0; var attempts = 0;
            var regressed = 0; var forgotten = 0; var blocked = 0; var premature = 0; var reused = 0;
            action Advance when (advanced == 0) {
                advanced = 1; disk = 2;
                dg = if Buggy == 6 {dg} else {dg + 1};
                expected = if Buggy == 3 {2} else {expected};
                eg = if Buggy == 3 {dg + 1} else {eg};
                reused = if Buggy == 6 {1} else {reused};
            }
            action ABA when (advanced == 1 && aba == 0) {
                aba = 1; disk = 0;
                dg = if Buggy == 6 {dg} else {dg + 1};
                reused = if Buggy == 6 {1} else {reused};
            }
            action FailBarrier when (failed == 0 && attempts <= 1) { failed = 1; }
            action RepairBarrier when (failed == 1 && attempts <= 1) { failed = 0; }
            action Write when (attempts <= 1) {
                attempts = attempts + 1;
                blocked = if failed == 0 && ((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) && Buggy == 4 {1} else {blocked};
                regressed = if ((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) {if Buggy == 3 && disk == 2 && target == 1 {1} else {regressed}} else {if Buggy == 1 && target == disk {1} else {regressed}};
                premature = if failed == 1 && (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) && Buggy == 5 {1} else {premature};
                reused = if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) && Buggy == 6 {1} else {reused};
                disk = if Buggy == 4 {disk} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) {target} else {disk}};
                dg = if Buggy == 4 {dg} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) {if (attempted == disk && ag == dg && target == disk) {dg} else {if Buggy == 6 {dg} else {dg + 1}}} else {dg}};
                expected = if Buggy == 4 {expected} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) && (failed == 0 || Buggy == 5) {target} else {expected}};
                eg = if Buggy == 4 {eg} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) && (failed == 0 || Buggy == 5) {if (attempted == disk && ag == dg && target == disk) {dg} else {if Buggy == 6 {dg} else {dg + 1}}} else {eg}};
                attempted = if Buggy == 4 {attempted} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) {if failed == 0 || Buggy == 5 {3} else {target}} else {attempted}};
                ag = if Buggy == 4 {ag} else {if (((expected == disk && eg == dg) || (attempted == disk && ag == dg && target == disk)) || (Buggy == 1 && target == disk)) {if failed == 0 || Buggy == 5 {0} else {if (attempted == disk && ag == dg && target == disk) {dg} else {if Buggy == 6 {dg} else {dg + 1}}}} else {ag}};
            }
            action Reload when (reloaded == 0) {
                reloaded = 1; expected = disk; eg = dg; target = disk; attempted = 3; ag = 0;
            }
            action Remove when (removed == 0) {
                removed = 1;
                forgotten = if Buggy == 2 {1} else {forgotten};
            }
            invariant NoStaleRegression: regressed == 0;
            invariant CoordinationIsSticky: forgotten == 0;
            invariant ValidWritesProgress: blocked == 0;
            invariant FailedWritesDoNotAck: premature == 0;
            invariant GenerationsNeverReuse: reused == 0;
            bound Finite: 0 <= disk && disk <= 2 && 0 <= expected && expected <= 2
                && 0 <= target && target <= 2 && 0 <= attempted && attempted <= 3
                && 0 <= dg && dg <= 4 && 0 <= eg && eg <= 4 && 0 <= ag && ag <= 4
                && 0 <= advanced && advanced <= 1 && 0 <= aba && aba <= 1
                && 0 <= reloaded && reloaded <= 1 && 0 <= removed && removed <= 1
                && 0 <= failed && failed <= 1 && 0 <= attempts && attempts <= 2
                && 0 <= regressed && regressed <= 1 && 0 <= forgotten && forgotten <= 1
                && 0 <= blocked && blocked <= 1 && 0 <= premature && premature <= 1
                && 0 <= reused && reused <= 1;
        }
    }
}
