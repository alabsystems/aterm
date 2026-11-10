// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One recovery reader beside an issuer whose append may complete after the
/// deadline. Recipient states are absent, recorded, delivered, dead with a live
/// group, and retired. A legacy grant has no complete recipient record.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_closure_model() -> Model {
    aterm_spec::ty_model! {
        RecipientClosure {
            const Buggy = 0;
            var mode = 0; var journal = 0; var live = 1; var late = 0; var scope = 0;
            var f = 0; var r = 0; var fd = 0; var rd = 0; var unknown = 0;
            var clock = 0; var seen = 0; var sc = 0; var sf = 0; var sr = 0;
            var probe = 0; var fp = 0; var rp = 0; var answer = 0; var fresh = 0;
            action Complete when (mode == 0 && journal == 0 && seen == 0) { mode = 1; }
            action Prepare when (journal == 0 && live == 1) { journal = 1; }
            action RecordF when (mode == 1 && live == 1 && journal == 1 && f == 0 && r == 0) {
                f = 1; journal = 2;
            }
            action RecordR when (mode == 1 && live == 1 && (journal == 1 || journal == 2)
                && r == 0 && (f == 0 || f == 4)) { r = 1; journal = 2; }
            action DeliverF when (f == 1 && live == 1 && late == 0) { f = 2; fd = 1; }
            action DeliverR when (r == 1 && live == 1 && late == 0) { r = 2; rd = 1; }
            action LegacyDeliver when (mode == 0 && live == 1 && journal == 1 && late == 0) {
                f = 2; fd = 1; journal = 2;
            }
            action ExitF when (f == 1 || f == 2) { f = 3; }
            action ExitR when (r == 1 || r == 2) { r = 3; }
            action ClearF when (f == 3) { f = 4; }
            action ClearR when (r == 3) { r = 4; }
            action SealOpen when (journal == 1 && live == 1) { journal = 3; }
            action SealMay when (journal == 2 && live == 1) { journal = 4; }
            action NoExec when (journal == 1 && live == 1) { journal = 5; }
            action Crash when (live == 1) { live = 0; }
            action Expire when (late == 0) { late = 1; }
            action Reboot when (scope == 0) {
                scope = 1; live = 0;
                f = if f > 0 {4} else {0}; r = if r > 0 {4} else {0};
            }
            action Foreign when (scope == 0 && clock == 0) { scope = 2; }
            action UnknownBoot when (scope == 0 && clock == 0) { scope = 3; }
            action UnknownBirth when (unknown == 0 && probe == 0) { unknown = 1; }
            action UnknownGroup when (unknown == 0 && probe == 0) { unknown = 2; }
            action ReadClock when (clock == 0) { clock = if late == 1 || 2 <= scope {2} else {1}; }
            action ReadJournal when (seen == 0 && (clock > 0 || Buggy == 1)) {
                seen = journal + 1; sc = mode;
                sf = if mode == 1 && f > 0 {1} else {0};
                sr = if mode == 1 && r > 0 {1} else {0};
            }
            action Probe when (probe == 0 && (seen == 3 || seen == 5)
                && (sc == 1 || Buggy == 6)
                && (clock == 2 || Buggy == 5) && (scope == 0 || Buggy == 7)) {
                probe = 1;
                fp = if sf == 0 || (Buggy == 4 && unknown > 0)
                    || (unknown == 0 && (f == 4 || (Buggy == 3 && f == 3))) {1} else {0};
                rp = if sr == 0 || Buggy == 2 || (Buggy == 4 && unknown > 0)
                    || (unknown == 0 && (r == 4 || (Buggy == 3 && r == 3))) {1} else {0};
            }
            action Answer when (answer == 0 && (sc == 1 || Buggy == 6)
                && (scope == 0 || (Buggy == 7 && 2 <= scope))
                && (seen == 4 || (Buggy == 8 && seen == 6)
                    || ((clock == 2 || Buggy == 5) && (seen == 2
                        || ((seen == 3 || seen == 5) && probe == 1 && fp == 1 && rp == 1))))) {
                answer = 1;
            }
            action AnswerBoot when (answer == 0 && sc == 1 && scope == 1 && 2 <= seen && seen <= 5) { answer = 1; }
            action AnswerNoExec when (answer == 0 && sc == 1 && seen == 6) { answer = 2; }
            action Fresh when (fresh == 0 && answer == 1) { fresh = 1; }
            invariant NoDoubleExec: fresh == 0 || ((fd == 0 || f == 4) && (rd == 0 || r == 4));
            invariant CompleteProof: answer == 0 || answer == 2 || sc == 1;
            invariant LocalProof: answer == 0 || answer == 2 || scope <= 1;
            invariant NoExecHeld: seen <= 5 || answer == 0 || answer == 2;
            invariant DeadlineOrFinal: answer == 0 || answer == 2 || scope == 1 || seen == 4 || clock == 2;
            bound Finite: 0 <= mode && mode <= 1 && 0 <= journal && journal <= 5
                && 0 <= live && live <= 1 && 0 <= late && late <= 1 && 0 <= scope && scope <= 3
                && 0 <= f && f <= 4 && 0 <= r && r <= 4 && 0 <= fd && fd <= 1 && 0 <= rd && rd <= 1
                && 0 <= unknown && unknown <= 2 && 0 <= clock && clock <= 2 && 0 <= seen && seen <= 6
                && 0 <= sc && sc <= 1 && 0 <= sf && sf <= 1 && 0 <= sr && sr <= 1
                && 0 <= probe && probe <= 1 && 0 <= fp && fp <= 1 && 0 <= rp && rp <= 1
                && 0 <= answer && answer <= 2 && 0 <= fresh && fresh <= 1;
        }
    }
}
