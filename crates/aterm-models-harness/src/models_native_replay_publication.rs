// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Every replay needs its own successful publication, and keeps the exact
/// operation through failed writes and lost replies. This models two attempts;
/// it assumes the writer's successful durability acknowledgement is truthful.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_replay_publication_model() -> Model {
    aterm_spec::ty_model! {
        NativeReplayPublication {
            const Buggy = 0;
            var barrier_failed = 0; var dry = 0; var held = 0;
            var attempts = 0; var sent = 0;
            var unpublished = 0; var dry_sent = 0; var held_sent = 0; var changed = 0;
            action FailBarrier when (barrier_failed == 0 && attempts <= 1) { barrier_failed = 1; }
            action RepairBarrier when (barrier_failed == 1 && attempts <= 1) { barrier_failed = 0; }
            action DryRun when (dry == 0 && attempts <= 1) { dry = 1; }
            action LiveRun when (dry == 1 && attempts <= 1) { dry = 0; }
            action Hold when (held == 0 && attempts <= 1) { held = 1; }
            action Attempt when (attempts <= 1) {
                attempts = attempts + 1;
                sent = sent + if (barrier_failed == 0 || Buggy == 1)
                    && (dry == 0 || Buggy == 2) && (held == 0 || Buggy == 3) {1} else {0};
                unpublished = if barrier_failed == 1 && Buggy == 1 && dry == 0 && held == 0
                    {1} else {unpublished};
                dry_sent = if dry == 1 && Buggy == 2 && barrier_failed == 0 && held == 0
                    {1} else {dry_sent};
                held_sent = if held == 1 && Buggy == 3 && barrier_failed == 0 && dry == 0
                    {1} else {held_sent};
                changed = if Buggy == 4 && barrier_failed == 1 && dry == 0 && held == 0
                    {1} else {changed};
            }
            invariant PublicationBeforeDispatch: unpublished == 0;
            invariant DryRunIsInert: dry_sent == 0;
            invariant RetirementHoldIsInert: held_sent == 0;
            invariant ExactOperationRetained: changed == 0;
            bound Finite: 0 <= barrier_failed && barrier_failed <= 1
                && 0 <= dry && dry <= 1 && 0 <= held && held <= 1
                && 0 <= attempts && attempts <= 2 && 0 <= sent && sent <= 2
                && 0 <= unpublished && unpublished <= 1 && 0 <= dry_sent && dry_sent <= 1
                && 0 <= held_sent && held_sent <= 1 && 0 <= changed && changed <= 1;
        }
    }
}
