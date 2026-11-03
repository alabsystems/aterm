// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One Mac's storage of one retired catalog table, and the release pass that may free it
/// (docs/DESIGN-alab-lab-model-2026-11-01.md §3.8, §3.10, step 13; `aterm_db::catalog::release`).
///
/// The table is one of two kinds the build compiles (`carry`): one whose rows count only for a
/// grace after its seal (1, the pre-ring claims tables), or one a roll restates into its next
/// generation (2, a registry generation). `sealed` is the catalog showing the twin declaration;
/// `seen` that this Mac's pass recorded when it first saw the seal, and `age` the graces this
/// Mac's clock has counted since; `claim` the graces the sealer's own clock claims since. For a
/// restated table `held` is the last record this Mac holds of it and `covered` the last its next
/// generation's cover rows carry. A roll restates before it seals (`Restate`, `Seal`); a record
/// can arrive after a restatement (`Late`), and the roll run again carries it (`RestateLate`).
///
/// The laws: storage goes only once the table is sealed (`ReleaseOnlySealed`), and never while
/// a peer may still read a lapsing table or this Mac holds a record no restatement carried
/// (`NothingNeededReleased`).
///
/// Mutants: 1 a pass that ignores the seal, 2 any cover row taken as carrying everything this
/// Mac holds, 3 the grace counted by the sealer's clock alone.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn storage_release_model() -> Model {
    aterm_spec::ty_model! {
        DbStorageRelease {
            const Buggy = 0;
            var carry = 0; var sealed = 0; var seen = 0; var age = 0; var claim = 0;
            var held = 1; var covered = 0; var stored = 1; var lost = 0; var early = 0;

            action Lapses when (carry == 0) { carry = 1; }
            action Restated when (carry == 0) { carry = 2; }
            action Restate when (carry == 2 && sealed == 0 && held > covered) { covered = held; }
            action Seal when (carry > 0 && sealed == 0 && (carry == 1 || covered > 0)) {
                sealed = 1; claim = 0;
            }
            action SealSkewed when (carry == 1 && sealed == 0) { sealed = 1; claim = 2; }
            action Late when (carry == 2 && 2 > held && stored == 1) { held = held + 1; }
            action RestateLate when (carry == 2 && sealed == 1 && held > covered) { covered = held; }
            action Elapse when (sealed == 1 && (2 > claim || (seen == 1 && 2 > age))) {
                age = if seen == 1 && 2 > age {age + 1} else {age};
                claim = if 2 > claim {claim + 1} else {claim};
            }
            action Pass when (carry > 0 && stored == 1) {
                lost = if (if Buggy == 1 {1} else {sealed}) == 1
                    && ((carry == 1 && claim > 0
                            && (if Buggy == 3 {claim} else {if seen == 1 {age} else {0}}) > 0)
                        || (carry == 2
                            && held <= (if Buggy == 2 && covered > 0 {held} else {covered})))
                    && ((carry == 1 && 1 > age) || (carry == 2 && held > covered)) {1} else {lost};
                early = if (if Buggy == 1 {1} else {sealed}) == 1
                    && ((carry == 1 && claim > 0
                            && (if Buggy == 3 {claim} else {if seen == 1 {age} else {0}}) > 0)
                        || (carry == 2
                            && held <= (if Buggy == 2 && covered > 0 {held} else {covered})))
                    && sealed == 0 {1} else {early};
                stored = if (if Buggy == 1 {1} else {sealed}) == 1
                    && ((carry == 1 && claim > 0
                            && (if Buggy == 3 {claim} else {if seen == 1 {age} else {0}}) > 0)
                        || (carry == 2
                            && held <= (if Buggy == 2 && covered > 0 {held} else {covered}))) {0} else {1};
                seen = if sealed == 1 {1} else {seen};
            }

            invariant ReleaseOnlySealed: early == 0;
            invariant NothingNeededReleased: lost == 0;
        }
    }
}
