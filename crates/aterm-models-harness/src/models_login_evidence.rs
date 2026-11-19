// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Each law of `LoginEvidence` with the one mutant that breaks it: the mutant at index `i` is
/// `Buggy == i + 1`.
pub const LOGIN_EVIDENCE_LAWS: [(&str, &str); 4] = [
    ("NewestObservationDecides", "StickyWord"),
    ("ArrivalOrderNeverDecides", "ArrivalOrder"),
    ("ForeignPrintSaysNothing", "ForeignPrint"),
    ("SameWitnessNeverRenews", "ReStampedWitness"),
];

/// The login verdict as a fold over dated evidence (aterm-evidence's `Ledger`): readings of the
/// credential print current now, each from its own witness, and the verdict, which is the newest
/// of them by the evidence's own date, then by value (2, struck, over 1, serving, at one date).
/// The ledger breaks a tie of date and value by witness, which `va`/`vk` do not show: either
/// reading of such a tie has the same date and value.
///
/// The print's first two witnesses are `a1`/`k1` and `a2`/`k2` (the date, 0 for none yet, and
/// the value). `va`/`vk` is the verdict read after each reading (0/0 for none), `pa`/`pk` the one
/// read before it, `last` what that reading was (1 a new witness of this print, 2 a reading of
/// another print, 3 the first witness read again, with whatever date and value the reading was
/// composed with) and `nw` whether a new witness was newer than every witness before it. The
/// next reading is composed first (`oa` its date, from 1 to 3, `ok` its value, `os` what it is)
/// by the environment's `Later`, `Struck`, `Foreign` and `Again`; `Observe` records it and reads
/// the verdict, and composes the next from scratch.
///
/// Mutants, as [`LOGIN_EVIDENCE_LAWS`] names them: 1 a struck verdict is not replaced by newer
/// evidence that the login serves (the stored word of 2026-11-13), 2 the latest arrival decides
/// whatever its date, 3 a reading of another print decides when it is newer, 4 a witness read
/// again with a later stamp is renewed to it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn login_evidence_model() -> Model {
    aterm_spec::ty_model! {
        LoginEvidence {
            const Buggy = 0;
            var a1 = 0; var k1 = 0; var a2 = 0; var k2 = 0;
            var va = 0; var vk = 0; var pa = 0; var pk = 0;
            var last = 0; var nw = 0;
            var oa = 1; var ok = 1; var os = 1;
            action Later when (oa <= 2) { oa = oa + 1; }
            action Struck when (ok == 1) { ok = 2; }
            action Foreign when (os == 1) { os = 2; }
            action Again when (os == 1 && a1 > 0) { os = 3; }
            action Observe when ((os == 1 && a2 == 0) || os > 1) {
                a1 = if os == 1 && a1 == 0 { oa }
                    else if os == 3 && Buggy == 4 && oa > a1 { oa }
                    else { a1 };
                k1 = if os == 1 && a1 == 0 { ok } else { k1 };
                a2 = if os == 1 && a1 > 0 { oa } else { a2 };
                k2 = if os == 1 && a1 > 0 { ok } else { k2 };
                va = if os == 1 && Buggy == 2 { oa }
                    else if os == 1 && (oa > a1 || (oa == a1 && ok > k1))
                        && (oa > a2 || (oa == a2 && ok > k2)) && Buggy == 1 && vk > ok { va }
                    else if os == 1 && (oa > a1 || (oa == a1 && ok > k1))
                        && (oa > a2 || (oa == a2 && ok > k2)) { oa }
                    else if os == 2 && Buggy == 3 && (oa > va || (oa == va && ok > vk)) { oa }
                    else if os == 3 && Buggy == 4 && oa > a1
                        && (oa > va || (oa == va && k1 > vk)) { oa }
                    else { va };
                vk = if os == 1 && Buggy == 2 { ok }
                    else if os == 1 && (oa > a1 || (oa == a1 && ok > k1))
                        && (oa > a2 || (oa == a2 && ok > k2)) && Buggy == 1 && vk > ok { vk }
                    else if os == 1 && (oa > a1 || (oa == a1 && ok > k1))
                        && (oa > a2 || (oa == a2 && ok > k2)) { ok }
                    else if os == 2 && Buggy == 3 && (oa > va || (oa == va && ok > vk)) { ok }
                    else if os == 3 && Buggy == 4 && oa > a1
                        && (oa > va || (oa == va && k1 > vk)) { k1 }
                    else { vk };
                pa = va; pk = vk; last = os;
                nw = if os == 1 && (oa > a1 || (oa == a1 && ok > k1))
                    && (oa > a2 || (oa == a2 && ok > k2)) { 1 } else { 0 };
                oa = 1; ok = 1; os = 1;
            }
            invariant NewestObservationDecides: last <= 0 || last > 1 || nw <= 0
                || (a2 <= 0 && va == a1 && vk == k1) || (a2 > 0 && va == a2 && vk == k2);
            invariant ArrivalOrderNeverDecides: last <= 0 || last > 1 || nw > 0
                || (va == pa && vk == pk);
            invariant ForeignPrintSaysNothing: last <= 1 || last > 2 || (va == pa && vk == pk);
            invariant SameWitnessNeverRenews: last <= 2 || (va == pa && vk == pk);
            bound DateRange: a1 <= 3 && a2 <= 3 && va <= 3 && pa <= 3 && oa <= 3;
            bound ValueRange: k1 <= 2 && k2 <= 2 && vk <= 2 && pk <= 2 && ok <= 2;
            bound StepRange: last <= 3 && nw <= 1 && os <= 3;
        }
    }
}
