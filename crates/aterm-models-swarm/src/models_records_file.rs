// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of a vault server's membership records file: complete records only, appends in
//! causal order, and what a restart reads back (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1
//! F23).

use aterm_spec::derive::*;

/// `SwarmRecordsFile`: one vault server's records store and its file (`<name>.records`), past
/// a genesis it holds on file from the start, with three records: `P`, a child of the genesis;
/// `C`, a child of `P`; and `J`, whose parent exists nowhere, so it can only wait.
///
/// Memory: `hp` and `hc` are 1 while the store holds `P` and `C` complete, `wc` and `wj` while it
/// holds `C` and `J` waiting on a parent. `post` is the POST in flight (`PostP`, `PostC`,
/// `PostJ`: 1, 2, 3; 0 none), which `Write` lands, or `Tear` and `Fail` refuse with a 500.
///
/// The file: `fp`, `fc` and `fj` are the 1-based places of `P`, `C` and `J` among its frames past
/// the genesis (0: not on file), `len` how many frames it holds, a torn one counted, and `tear`
/// the place of the torn frame (0: none). A frame before `tear` (every frame, with none) is in
/// the file's untorn prefix: what a read gets back. `onf` is 1 while the slot knows how many held
/// records the file holds (`on_file`), `same` while the file's stamp is the one the slot kept.
/// `ap` and `ac` are 1 once a POST was answered 204 with `P` or `C` complete (a 204 says every
/// complete record the store holds is on disk). `restarted` counts restarts, at most two.
///
/// `Write` first looks at the file: a stamp other than the slot's is read again, which merges
/// nothing new into a store the file came from and leaves `on_file` unknown. A POST that makes
/// records complete then appends them alone, in causal order, when the slot knows the file, and
/// otherwise writes the held records whole (a temp file and a rename, which a failure leaves
/// untouched). A POST that adds only waiting records writes nothing. `Tear` is an append cut
/// short: a torn frame on the end, a 500, and a stamp the slot does not know. `Fail` is a write
/// that touched nothing, a 500 after the look. `Foreign` is another writer replacing the file
/// with its untorn prefix (a copy renamed over it). `Restart` forgets the store: memory becomes
/// what the file's untorn prefix reads to, with nothing waiting, and the slot knows the file
/// only when its bytes are exactly the held records.
///
/// Mutants: 1 appends a tail in id order (`C` sorts before `P`); 2 writes a waiting record (the
/// whole store, `J` with it); 3 answers 204 before a write that then fails; 4 has a restart trust
/// a file whose bytes are not exactly the held records, so the next growth appends onto a torn
/// tail. The box is the mutants' too: under mutant 4, `P` torn on the first append and then
/// appended after each of the two restarts, with `C` behind it, makes five frames.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn records_file_model() -> Model {
    aterm_spec::ty_model! {
        SwarmRecordsFile {
            const Buggy = 0;

            var post = 0;
            var hp = 0;
            var hc = 0;
            var wc = 0;
            var wj = 0;
            var fp = 0;
            var fc = 0;
            var fj = 0;
            var len = 0;
            var tear = 0;
            var onf = 1;
            var same = 1;
            var ap = 0;
            var ac = 0;
            var restarted = 0;

            action PostP when (post == 0 && hp == 0) {
                post = 1;
            }
            action PostC when (post == 0 && hc == 0 && wc == 0) {
                post = 2;
            }
            action PostJ when (post == 0 && wj == 0) {
                post = 3;
            }
            action Write when (post > 0) {
                post = 0;
                hp = if post == 1 { 1 } else { hp };
                hc = if (post == 1 && wc == 1) || (post == 2 && hp == 1) { 1 } else { hc };
                wc = if post == 1 { 0 } else { if post == 2 && hp == 0 { 1 } else { wc } };
                wj = if post == 3 { 1 } else { wj };
                // A growth appends when the slot knows the file after the look, else writes
                // whole: `P`, then `C` when it is complete. Mutant 2 writes the whole store,
                // `J` last, on `J`'s own POST.
                fp = if post == 1 || (post == 2 && hp == 1) {
                    if same == 1 && onf == 1 {
                        if post == 1 {
                            if Buggy == 1 && wc == 1 { len + 2 } else { len + 1 }
                        } else {
                            fp
                        }
                    } else {
                        1
                    }
                } else {
                    if Buggy == 2 && post == 3 { hp } else { fp }
                };
                fc = if (post == 1 && wc == 1) || (post == 2 && hp == 1) {
                    if same == 1 && onf == 1 {
                        if post == 1 {
                            if Buggy == 1 { len + 1 } else { len + 2 }
                        } else {
                            len + 1
                        }
                    } else {
                        2
                    }
                } else {
                    if post == 1 && 2 > same + onf {
                        0
                    } else {
                        if Buggy == 2 && post == 3 { hc + hc } else { fc }
                    }
                };
                fj = if post == 1 || (post == 2 && hp == 1) {
                    if same == 1 && onf == 1 { fj } else { 0 }
                } else {
                    if Buggy == 2 && post == 3 { hp + hc + 1 } else { fj }
                };
                len = if post == 1 || (post == 2 && hp == 1) {
                    if same == 1 && onf == 1 {
                        if post == 1 && wc == 1 { len + 2 } else { len + 1 }
                    } else {
                        if (post == 1 && wc == 1) || post == 2 { 2 } else { 1 }
                    }
                } else {
                    if Buggy == 2 && post == 3 { hp + hc + 1 } else { len }
                };
                tear = if post == 1 || (post == 2 && hp == 1) {
                    if same == 1 && onf == 1 { tear } else { 0 }
                } else {
                    if Buggy == 2 && post == 3 { 0 } else { tear }
                };
                onf = if post == 1 || (post == 2 && hp == 1) || (Buggy == 2 && post == 3) {
                    1
                } else {
                    if same == 1 { onf } else { 0 }
                };
                same = 1;
                ap = if post == 1 { 1 } else { hp };
                ac = if (post == 1 && wc == 1) || (post == 2 && hp == 1) { 1 } else { hc };
            }
            action Tear when (post > 0 && tear == 0 && same == 1 && onf == 1 &&
                (post == 1 || (post == 2 && hp == 1))) {
                post = 0;
                len = len + 1;
                tear = len + 1;
                same = 0;
            }
            action Fail when (post == 1 || (post == 2 && hp == 1)) {
                post = 0;
                onf = if same == 1 { onf } else { 0 };
                same = 1;
                hp = if Buggy == 3 { 1 } else { hp };
                hc = if Buggy == 3 && ((post == 1 && wc == 1) || post == 2) { 1 } else { hc };
                wc = if Buggy == 3 && post == 1 { 0 } else { wc };
                ap = if Buggy == 3 { 1 } else { ap };
                ac = if Buggy == 3 && ((post == 1 && wc == 1) || post == 2) { 1 } else { ac };
            }
            action Foreign when (same == 1) {
                same = 0;
                fp = if tear > 0 && tear <= fp { 0 } else { fp };
                fc = if tear > 0 && tear <= fc { 0 } else { fc };
                fj = if tear > 0 && tear <= fj { 0 } else { fj };
                len = if tear > 0 { tear - 1 } else { len };
                tear = 0;
            }
            action Restart when (2 > restarted) {
                post = 0;
                restarted = restarted + 1;
                hp = if fp > 0 && (tear == 0 || tear > fp) { 1 } else { 0 };
                hc = if fp > 0 && (tear == 0 || tear > fp) && fc > 0 && (tear == 0 || tear > fc) {
                    1
                } else {
                    0
                };
                wc = 0;
                wj = 0;
                onf = if Buggy == 4 || (tear == 0 && fj == 0 && len == (if fp > 0 { 1 } else { 0 }) +
                    (if fp > 0 && fc > 0 { 1 } else { 0 })) {
                    1
                } else {
                    0
                };
                same = 1;
            }

            invariant FileHoldsCompleteOnly:
                (fc == 0 || (tear > 0 && tear <= fc) || (fp > 0 && fc > fp)) &&
                (fj == 0 || (tear > 0 && tear <= fj));
            invariant AckedIsDurable:
                (ap == 0 || (fp > 0 && (tear == 0 || tear > fp))) &&
                (ac == 0 || (fc > 0 && (tear == 0 || tear > fc)));
            invariant RestartKeepsAcked:
                restarted == 0 || ((ap == 0 || hp == 1) && (ac == 0 || hc == 1));
            bound RecordsFileBox:
                post <= 3 && hp <= 1 && hc <= 1 && wc <= 1 && wj <= 1 && fp <= 5 && fc <= 5 &&
                fj <= 5 && len <= 5 && tear <= 5 && onf <= 1 && same <= 1 && ap <= 1 &&
                ac <= 1 && restarted <= 2;
        }
    }
}
