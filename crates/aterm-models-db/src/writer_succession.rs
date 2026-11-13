// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A pending Commons writer succession ends with no person, and a person's removal of the Mac
/// ends it for good. The fenced original, still a current member, signs a statement naming its
/// successor's device key; the vault's members seat the successor on that statement, and the
/// witnessed-prefix import and the seal then run as before. The successor is the same Mac's
/// writer under a key of its own, so a person's removal of the Mac takes it out too, whichever
/// came first.
///
/// What a current member's membership log and the fenced original's home hold, wherever the
/// newest records are held: `member` is whether the original is a member, `passed` whether its
/// recovery pass ran on its own statement, `seated` how many seats stand on its admission of a
/// successor, `completed` whether its succession is sealed and selects its successor, `rekeyed`
/// whether its successor home came back under another device after the log seated the first,
/// `authored` whether the fenced original authored a Commons table record or could, `presented`
/// which statement that is not the original's own was presented (2 one naming another key, 3
/// one another member signed, 4 one a Mac with no seat signed), and `foreign` whether a seat
/// stands on it.
///
/// Mutants: 1 the statement admits nothing, 2 the fenced original authors, 3 to 5 a statement
/// that is not the original's own seats a key, 6 a removed original's later admission stands,
/// 7 a home restored with nothing of its fence gets a second successor, 8 a succession with no
/// admission completes, 9 a person's removal leaves the Mac's successor seated.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn writer_succession_model() -> Model {
    aterm_spec::ty_model! {
        DbWriterSuccession {
            const Buggy = 0;
            var member = 1; var passed = 0; var seated = 0; var completed = 0;
            var rekeyed = 0; var authored = 0; var presented = 0; var foreign = 0;
            // The fenced original's pass signs for the device its reserved successor home
            // holds. One that another member removed, and that has not heard, signs too: the
            // removal voids its admission wherever both are held.
            action Statement when (passed == 0 && presented == 0) {
                passed = 1;
                seated = if member == 1 {if Buggy == 1 {0} else {1}} else {if Buggy == 6 {1} else {0}};
                completed = if member == 1 {if Buggy == 1 {0} else {1}} else {if Buggy == 8 {1} else {0}};
                authored = if Buggy == 2 {1} else {0};
            }
            // A person removes the original's Mac at another member, before its pass or after.
            action Remove when (member == 1 && presented == 0) {
                member = 0;
                seated = if Buggy == 9 {seated} else {0};
                completed = 0;
            }
            // The log seats the key first named; the successor home, the kept statement and
            // the fence are then lost together (a home restored from a backup) and made again
            // under another device, and the pass runs again: it fences anew and its successor
            // waits for a person.
            action Rekey when (passed == 1 && member == 1 && rekeyed == 0) {
                rekeyed = 1; completed = 0;
                seated = if Buggy == 7 {seated + 1} else {seated};
            }
            // The statement presented names another key than the one that asks.
            action OtherKey when (passed == 0 && member == 1 && presented == 0) {
                presented = 2; foreign = if Buggy == 3 {1} else {0};
            }
            // A current member that is not the lineage's fenced original signed it.
            action OtherSigner when (passed == 0 && member == 1 && presented == 0) {
                presented = 3; foreign = if Buggy == 4 {1} else {0};
            }
            // Its signer holds no seat in the vault.
            action NonMember when (passed == 0 && member == 1 && presented == 0) {
                presented = 4; foreign = if Buggy == 5 {1} else {0};
            }
            invariant StatementCompletes: passed == 0 || member == 0 || rekeyed == 1 || (seated > 0 && completed == 1);
            invariant KeyNamedExactly: 2 > presented || presented > 2 || foreign == 0;
            invariant LineageOriginalSigns: 3 > presented || presented > 3 || foreign == 0;
            invariant MemberSigns: 4 > presented || foreign == 0;
            invariant RemovedMacKeepsNoSuccessor: member == 1 || seated == 0;
            invariant OneSuccessor: 2 > seated;
            invariant OriginalAuthorsNothing: authored == 0;
            invariant CompletionNeedsAdmission: completed == 0 || seated > 0;
        }
    }
}
