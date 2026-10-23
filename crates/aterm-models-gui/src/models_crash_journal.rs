// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CRASH JOURNAL'S CLAIM (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.5, §6.3): who may take a dead window's layout, and when.

use aterm_spec::derive::Model;

/// One journal and every launch that comes after its owner (`aterm-gui/src/crash_journal.rs`: `JournalOwner::create`/`publish`/`retire` on the owner's side, `claim_at_boot` on the reader's).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn crash_journal_claim_model() -> Model {
    aterm_spec::ty_model! {
        CrashJournalClaim {
            const Buggy = 0;
            const MaxClaims = 2;
            var booted = 0;
            var live = 1;
            var clean = 0;
            var probation = 0;
            var second = 0;
            var crashed = 0;
            var image = 0;
            var image_probation = 0;
            var image_second = 0;
            var torn = 0;
            var written = 0;
            var claims = 0;
            var live_claim = 0;
            var applied = 0;
            var lost = 0;
            var quit_layout = 0;
            var successor = 0;

            action BootFresh when (booted == 0) {
                booted = 1;
            }
            action BootFromJournal when (booted == 0) {
                booted = 1;
                probation = 1;
            }
            action BootSecondChance when (booted == 0) {
                booted = 1;
                probation = 1;
                second = 1;
            }
            action Write when (booted == 1 && live == 1 && torn == 0) {
                image = if Buggy == 1 { 0 } else { 1 };
                torn = if Buggy == 1 { 1 } else { 0 };
                image_probation = probation;
                image_second = second;
                written = if Buggy == 1 { written } else { 1 };
            }
            action FinishWrite when (live == 1 && torn == 1) {
                image = 1;
                torn = 0;
                written = 1;
            }
            action Settle when (live == 1 && probation == 1 && torn == 0) {
                probation = 0;
                second = 0;
                image_probation = 0;
                image_second = 0;
            }
            action Quit when (booted == 1 && live == 1) {
                live = 0;
                clean = 1;
                image = 0;
                torn = 0;
            }
            action WriteQuitLayout when (booted == 1 && live == 1 && quit_layout == 0) {
                quit_layout = 1;
            }
            action HandOff when (booted == 1 && live == 1 && quit_layout == 0) {
                live = 0;
                clean = 1;
                successor = 1;
            }
            action Adopt when (booted == 1 && live == 0 && clean == 1 && image == 1) {
                live = 1;
                clean = 0;
                probation = 0;
                second = 0;
                successor = 0;
            }
            action SuccessorDies when (
                successor == 1 && live == 0 && (image == 0 || Buggy == 1)
            ) {
                successor = 0;
                lost = if image == 1 { 1 } else { lost };
            }
            action Die when (booted == 1 && live == 1) {
                live = 0;
                lost = if written == 1 && image == 0 { 1 } else { 0 };
            }
            action Crash when (booted == 1 && live == 1) {
                live = 0;
                crashed = 1;
                lost = if written == 1 && image == 0 { 1 } else { 0 };
            }
            action Claim when (
                image == 1 && (live == 0 || Buggy == 1) && claims <= MaxClaims - 1
            ) {
                claims = claims + 1;
                live_claim = if live == 1 { 1 } else { live_claim };
                image = if Buggy == 1 { 1 } else { 0 };
                applied = if (clean == 0 || Buggy == 1)
                    && (image_probation == 0 || (crashed == 0 && image_second == 0) || Buggy == 1)
                    && (quit_layout == 0 || Buggy == 1)
                {
                    1
                } else {
                    applied
                };
            }

            invariant SingleUse: claims <= 1;
            invariant LiveOwnerNeverClaimed: live_claim == 0;
            invariant CleanEndNeverRestored: applied == 0 || clean == 0;
            invariant NoCrashLoop: applied == 0 || image_probation == 0 || crashed == 0;
            invariant NoLoop: applied == 0 || image_second == 0;
            invariant NoLoss: lost == 0;
            invariant OneDesk: applied == 0 || quit_layout == 0;
        }
    }
}
