// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two attempts to expose one saved candidate. Persistence acknowledgements are
/// assumed truthful; ordinary rewrites must request the same durability. Issuer
/// claims, host crashes and promotion are not modeled.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_account_client_slot_model() -> Model {
    aterm_spec::ty_model! {
        NativeAccountClientSlot {
            const Buggy = 0;
            var barrier_failed = 0; var dry = 0; var staged = 0; var readable = 1;
            var attempts = 0; var exposed = 0; var rival = 0; var reloaded = 0; var stale = 0;
            var debt = 1; var exact = 1; var unpublished = 0; var dry_exposed = 0; var lost = 0;
            var rewritten = 0; var weakened = 0;
            action FailBarrier when (barrier_failed == 0 && attempts <= 1) { barrier_failed = 1; }
            action RepairBarrier when (barrier_failed == 1 && attempts <= 1) { barrier_failed = 0; }
            action DryRun when (dry == 0 && attempts <= 1) { dry = 1; }
            action LiveRun when (dry == 1 && attempts <= 1) { dry = 0; }
            action Attempt when (attempts <= 1) {
                attempts = attempts + 1;
                staged = if dry == 0 || Buggy == 3 {1} else {staged};
                exposed = exposed + if readable == 1 && (dry == 0 || Buggy == 3)
                    && (barrier_failed == 0 || Buggy == 2) {1} else {0};
                debt = if Buggy == 1 && dry == 0 {0} else {debt};
                unpublished = if Buggy == 2 && barrier_failed == 1 && dry == 0 && readable == 1
                    {1} else {unpublished};
                dry_exposed = if Buggy == 3 && dry == 1 && barrier_failed == 0 && readable == 1
                    {1} else {dry_exposed};
            }
            action Rival when (staged == 1 && readable == 1 && rival == 0) {
                rival = 1;
                exact = if Buggy == 4 && dry == 0 {0} else {exact};
            }
            action Opaque when (staged == 1 && readable == 1) { readable = 0; }
            action Reload when (staged == 1 && reloaded == 0) {
                reloaded = 1;
                lost = if Buggy == 5 && readable == 0 {1} else {lost};
            }
            action StaleSave when (staged == 1 && stale == 0) {
                stale = 1;
                lost = if Buggy == 6 && dry == 0 {1} else {lost};
            }
            action Rewrite when (rewritten == 0) {
                rewritten = 1;
                weakened = if Buggy == 7 && dry == 0 {1} else {weakened};
            }
            invariant PredecessorRetained: debt == 1;
            invariant ExactCandidate: exact == 1;
            invariant PublicationBeforeExposure: unpublished == 0;
            invariant DryRunIsInert: dry_exposed == 0;
            invariant SlotRetained: lost == 0;
            invariant RewriteKeepsDurability: weakened == 0;
            bound Finite: 0 <= barrier_failed && barrier_failed <= 1
                && 0 <= dry && dry <= 1 && 0 <= staged && staged <= 1 && 0 <= readable && readable <= 1
                && 0 <= attempts && attempts <= 2 && 0 <= exposed && exposed <= 2
                && 0 <= rival && rival <= 1 && 0 <= reloaded && reloaded <= 1 && 0 <= stale && stale <= 1
                && 0 <= debt && debt <= 1 && 0 <= exact && exact <= 1
                && 0 <= unpublished && unpublished <= 1 && 0 <= dry_exposed && dry_exposed <= 1
                && 0 <= lost && lost <= 1 && 0 <= rewritten && rewritten <= 1
                && 0 <= weakened && weakened <= 1;
        }
    }
}
