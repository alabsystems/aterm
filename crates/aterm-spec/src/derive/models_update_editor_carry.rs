// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unsaved native editor tab rides a self-update (2026-09-26, gap #32):
//! its draft is CARRIED through the handoff instead of holding the update
//! until the person saves or closes it. Before this date a dirty document was
//! an unconditional preflight blocker ("Save or close the open editor to
//! finish updating"), so one forgotten tab stopped automatic updates for as
//! long as it stayed open.
//!
//! The carrier is the crash journal every editor already keeps
//! (`aterm-gui/src/native_document_journal.rs`). The successor restores an
//! editor leaf by reopening its file, which replays the journal's draft over
//! the unchanged file; it then republishes the journal under its own sequence
//! numbers. So the outgoing process may let the update Commit only when the
//! image on disk — read back at Commit, which is the successor's own
//! republication of what it restored — replays to exactly its editor's text,
//! and when no journal write of its own landed after the successor may have
//! read the image.

use super::Model;

/// One document through one update attempt, in the outgoing process's journal
/// store, the journal image on disk, and the successor's restore
/// (`aterm-gui/src/app_documents.rs` `App::draft_carry`, the preflight
/// `App::native_update_close_preflight`, `App::rollback_overlap`).
///
/// State: `head` — the outgoing editor's version (0 is the file on disk, so
/// `head == 0` is a clean document). `inflight`/`target` — a journal append
/// on the document worker and the version it writes (0 when none is). `mem` — the version the
/// outgoing journal store proved durable. `journal` — the version the image on
/// disk replays to. `owner` — 1 when that image is a successor's
/// republication, not the one the outgoing store published (its appends then
/// fail their expected-image preflight). `reseat` — the outgoing store owes a
/// re-seat of its image. `phase` — 0 no attempt, 1 a successor launched and
/// held before the park, 2 parked (the successor restores and proves), 3
/// committed (the outgoing process is gone). `restored`/`succ` — the
/// successor ran its restore, and the version its editor holds
/// (`Placeholder`: its restore failed and the leaf is a Recovery tab, whose
/// Retry reopens the file and replays the journal). `late` — a journal write
/// landed while parked.
///
/// The environment's moves are the person's (`Type`), the worker's (`Land`,
/// `Refuse` — an append whose image changed under it), and the successor's
/// (`Restore`, `RestoreIdentical` — its republication is byte-for-byte the
/// image already there, so the outgoing store's next append still lands on it:
/// a clean document the outgoing store never journaled an edit of does this,
/// and the model lets any unowned image do it — and `RestoreFails`). The App's are `Plan`,
/// `Start` (the preflight), `StandDown`, `Park`, `Commit` (the Commit-time
/// revalidation), `Rollback` and `Reseat`.
///
/// Invariants: `NoDraftLost` — once the update commits, the successor's editor
/// holds exactly the outgoing buffer, or its Recovery tab's journal does.
/// `JournalOwnedOutsideHandoff` — with no attempt in flight, the image on disk
/// is the outgoing store's own unless a re-seat is owed (else its appends fail
/// for the rest of the session and the draft is no longer crash-safe).
///
/// `Buggy = 1` is the two ways this goes wrong, each caught alone: a Commit
/// that trusts a dirty document without the fresh check (carrying the buffer
/// blindly, or on the start-time certificate alone — the person typed during
/// the overlap), and a rollback that forgets the successor's image.
/// `Buggy = 2` re-seats while parked (publishing over the image the successor
/// restored from, which the Commit check then misreads as the successor's
/// copy); `Buggy = 3` drops the `late` check (an append already on the worker
/// at the park lands on a byte-identical republication after the successor
/// read the older image). Both lose the draft.
///
/// Tier-1 (`aterm-gui/src/editor_carry_conformance.rs`) drives a real
/// outgoing `App` and a real successor `App` sharing one journal directory
/// through this machine's schedules — the editor's input, the real preflight
/// and Commit revalidation, the successor's real restore from the captured
/// layout, the real rollback — projects every variable at every step, requires
/// the real verdicts to be the model's guards, and replays the `Buggy`
/// machines as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_editor_carry_model() -> Model {
    crate::ty_model! {
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
