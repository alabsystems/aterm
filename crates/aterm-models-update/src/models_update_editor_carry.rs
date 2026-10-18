// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unsaved native editor tab rides a self-update: its draft is carried through the
//! handoff rather than holding the update until the person saves or closes it.
//!
//! The carrier is the editor's crash journal (`aterm-gui/src/native_document_journal.rs`).
//! The successor reopens the file, replays the journal's draft over it, and republishes
//! the journal under its own sequence numbers. The outgoing process lets the update Commit
//! only when the image on disk, read back at Commit, replays to exactly its editor's text
//! and no journal write of its own landed after the successor may have read the image.

use aterm_spec::derive::Model;

/// One document through one update attempt: the outgoing journal store, the journal image
/// on disk, and the successor's restore (`App::draft_carry`, the preflight
/// `App::native_update_close_preflight`, `App::rollback_overlap`).
///
/// State: `head` — the outgoing editor's version (0 is the file on disk: a clean
/// document). `inflight`/`target` — a journal append on the document worker and the
/// version it writes. `mem` — the version the outgoing store proved durable. `journal` —
/// the version the image on disk replays to. `owner` — 1 when that image is a successor's
/// republication (outgoing appends then fail their expected-image preflight). `reseat` —
/// the outgoing store owes a re-seat of its image. `phase` — 0 no attempt, 1 successor
/// launched and held, 2 parked, 3 committed. `restored`/`succ` — the successor ran its
/// restore, and the version its editor holds (`Placeholder`: a Recovery tab, whose Retry
/// replays the journal). `late` — a journal write landed while parked.
///
/// `RestoreIdentical` is a republication byte-for-byte equal to the image already there (a
/// clean document's restore), so the outgoing store's next append still lands on it.
///
/// `NoDraftLost`: after Commit the successor's editor, or its Recovery tab's journal, holds
/// exactly the outgoing buffer. `JournalOwnedOutsideHandoff`: with no attempt in flight the
/// image is the outgoing store's own unless a re-seat is owed (else its appends fail and
/// the draft is no longer crash-safe).
///
/// `Buggy = 1` commits a dirty document without the fresh check, and rolls back forgetting
/// the successor's image. `Buggy = 2` re-seats while parked (over the image the successor
/// restored from, which Commit then misreads as the successor's copy); `Buggy = 3` drops
/// the `late` check (an append on the worker at the park lands on an identical
/// republication after the successor read the older image). Both lose the draft.
///
/// Tier-1: `aterm-gui/src/editor_carry_conformance.rs` drives a real outgoing and a real
/// successor `App` over one journal directory through every schedule, with the `Buggy`
/// machines as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_editor_carry_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateEditorCarry {
            const Buggy = 0;
            const MaxEdits = 3;
            const Placeholder = 9;
            var head = 0;
            var inflight = 0;
            var target = 0;
            var mem = 0;
            var journal = 0;
            var owner = 0;
            var reseat = 0;
            var phase = 0;
            var restored = 0;
            var succ = 0;
            var late = 0;

            action Type when (phase <= 2 && head <= MaxEdits - 1) {
                head = head + 1;
            }
            action Plan when (phase <= 2 && inflight == 0 && reseat == 0 && head > mem) {
                inflight = 1;
                target = head;
            }
            action Land when (inflight == 1 && owner == 0) {
                inflight = 0;
                mem = target;
                journal = target;
                late = if phase == 2 { 1 } else { late };
                target = 0;
            }
            action Refuse when (inflight == 1 && owner == 1) {
                inflight = 0;
                target = 0;
            }
            action Start when (
                phase == 0
                    && (head == 0
                        || (reseat == 0 && inflight == 0 && mem == head && journal == head))
            ) {
                phase = 1;
            }
            action StandDown when (phase == 1) {
                phase = 0;
            }
            action Park when (phase == 1) {
                phase = 2;
            }
            action Restore when (phase == 2 && restored == 0) {
                restored = 1;
                succ = journal;
                owner = 1;
            }
            action RestoreIdentical when (phase == 2 && restored == 0 && owner == 0) {
                restored = 1;
                succ = journal;
            }
            action RestoreFails when (phase == 2 && restored == 0) {
                restored = 1;
                succ = Placeholder;
            }
            action Commit when (
                phase == 2
                    && restored == 1
                    && (Buggy == 1
                        || ((late == 0 || Buggy == 3)
                            && (head == 0
                                || (reseat == 0
                                    && inflight == 0
                                    && mem == head
                                    && journal == head))))
            ) {
                phase = 3;
            }
            action Rollback when (phase == 2) {
                phase = 0;
                restored = 0;
                succ = 0;
                late = 0;
                reseat = if Buggy == 1 { reseat } else { 1 };
            }
            action Reseat when (
                reseat == 1 && inflight == 0 && (phase == 0 || (Buggy == 2 && phase <= 2))
            ) {
                reseat = 0;
                owner = 0;
                mem = head;
                journal = head;
            }

            invariant NoDraftLost:
                phase <= 2 || succ == head || (succ == Placeholder && journal == head);
            invariant JournalOwnedOutsideHandoff: phase > 0 || reseat == 1 || owner == 0;
        }
    }
}
