// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exclusivity of the app's TCC identity, and the census that reports on it.

use super::Model;

/// A second bundle claiming this app's `CFBundleIdentifier` must never be
/// SILENT, and nothing may be retired that was not first named.
///
/// # The hazard this machine states
///
/// macOS keeps ONE code requirement per bundle identifier. A copy that does not
/// satisfy the stored requirement is not merely denied — `tccd` answers
/// `DB Action:Update, UpdateVerifierData` and REPLACES the stored requirement
/// with the asker's own, resetting the grant for every copy sharing the id.
/// So the grant is not lost by the copy that misbehaves; it is lost by whoever
/// asks next. Measured on the owner's Mac 2026-09-21: three Full Disk Access
/// grants in 55 seconds, two of them destroyed, one by an ad-hoc fossil and one
/// by the legitimate `/Applications` install asking afterwards.
///
/// `stored_foreign` is which identity the row currently holds (0 = the live
/// install's, 1 = a foreign one). `LiveAsks` and `ForeignAsks` are the two
/// destructive resolutions, and they are symmetric on purpose: the second one
/// is the step everyone omits when they reason about this, and it is the step
/// that ate the owner's second grant.
///
/// # What it obliges the code to do
///
/// * **`AConflictIsNeverSilent`** — once the census has looked at a disk that
///   holds a foreign claimant, that claimant is REPORTED. Staging a claimant
///   invalidates a previous look, so "the census already ran" is not a defence
///   against a bundle that appeared afterwards. This is the property
///   `aterm_containment::consent::classify_claimants` owes, and the Tier-1
///   conformance drives the real function to discharge it.
/// * **`NothingIsRetiredUnnamed`** — a claimant is only ever removed after it
///   was named. Automatically deleting a bundle nobody reported is the act this
///   area forbids, and the fence is stated here rather than left to review.
/// * **`GrantLossImpliesMismatch`** — a held grant cannot simply evaporate;
///   every loss is preceded by a mismatching ask. This is what makes the
///   owner's report ("it didn't persist") a statement about a MECHANISM rather
///   than about storage that failed.
///
/// `Buggy = 1` restores the three historical behaviours, one per invariant: a
/// census that looks without reporting, a retirement with no report behind it,
/// and a grant that vanishes on its own.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn tcc_identity_claim_exclusivity_model() -> Model {
    crate::ty_model! {
        TccIdentityClaimExclusivity {
            const Buggy = 0;
            const Foreign = 2;
            // Foreign bundles on disk claiming this app's identifier.
            var foreign = 0;
            // 1 when the stored requirement is a foreign copy's, 0 when it is
            // the live install's.
            var stored_foreign = 0;
            var granted = 0;
            var ever_granted = 0;
            // A mismatching ask has happened since the last grant.
            var mismatched = 0;
            // The census has looked at the disk in its CURRENT shape.
            var censused = 0;
            // The census named a foreign claimant.
            var reported = 0;
            // Set when a retirement happens with NOTHING having named the
            // claimant. A sticky "we retired something" flag cannot express the
            // contract, because `reported` is legitimately cleared when the disk
            // changes; a flag on the UNNAMED case states the act itself. Kept a
            // FLAG rather than a count on purpose — a counter is unbounded here
            // (stage, retire, stage again) and the interpreter refuses the
            // state space it opens.
            var retired_unnamed = 0;

            // A copy appears beside the install — an apply renaming the
            // running bundle aside, a cutter placing a second one, a backup.
            // It invalidates any previous look: a census is about a disk.
            action StageForeign when (foreign <= Foreign - 1) {
                foreign = foreign + 1;
                censused = 0;
                reported = 0;
            }

            // The owner grants in System Settings.
            action Grant when (granted == 0) {
                granted = 1;
                ever_granted = 1;
                mismatched = 0;
            }

            // The live install asks while the row holds a foreign identity.
            // macOS rewrites the row to the asker and resets the grant — this
            // is the step that destroyed the owner's SECOND grant.
            action LiveAsks when (stored_foreign == 1) {
                stored_foreign = 0;
                granted = 0;
                mismatched = 1;
            }

            // A foreign copy asks while the row holds the live identity.
            action ForeignAsks when (foreign > 0 && stored_foreign == 0) {
                stored_foreign = 1;
                granted = 0;
                mismatched = 1;
            }

            // The census looks at a disk that HOLDS a foreign claimant, and
            // names it. Looking and reporting are one step because a census
            // that has looked but not yet reported is exactly the silent
            // state this model forbids.
            action Census when (censused == 0 && foreign > 0 && Buggy == 0) {
                censused = 1;
                reported = 1;
            }

            // The census looks at a clean disk. Nothing to name.
            action CensusClean when (censused == 0 && foreign == 0) {
                censused = 1;
            }

            // THE HISTORICAL DEFECT: look, and say nothing. Every identity
            // surface in the tree was first-person over `current_exe()`, so
            // this is what the product did on every machine until 2026-09-22.
            action BuggyCensusSilent when (censused == 0 && Buggy == 1) {
                censused = 1;
            }

            // The owner retires a claimant that was named.
            action Retire when (reported == 1 && foreign > 0) {
                foreign = foreign - 1;
                censused = 0;
                reported = 0;
            }

            // THE FENCE, INVERTED: remove a bundle nobody reported.
            action BuggyRetireUnnamed when (Buggy == 1 && foreign > 0) {
                foreign = foreign - 1;
                retired_unnamed = 1;
            }

            // A grant that evaporates with no mismatching ask behind it —
            // the reading the owner's "it didn't persist" would have if the
            // mechanism above were not the explanation.
            action BuggyGrantVanishes when (Buggy == 1 && granted == 1) {
                granted = 0;
            }

            invariant AConflictIsNeverSilent:
                foreign == 0 || censused == 0 || reported == 1;

            invariant NothingIsRetiredUnnamed:
                retired_unnamed == 0;

            invariant GrantLossImpliesMismatch:
                granted == 1 || ever_granted == 0 || mismatched == 1;
        }
    }
}
