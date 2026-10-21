// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsEditTicket`: one `edit` request's ticket, from admission to its one
//! outcome (docs/DESIGN-amacs-2026-10-19.md §4.9; slice N1, extended by N3).
//!
//! Invariants: OneOutcome; SavedOnlyWhenAcked (`saved=1` implies the
//! checkpoint reached the requested seq); OrphanKeepsView;
//! WaiterAnsweredOnlyAfterViewGone; AbortNeverAdvancesCheckpoint (an abort
//! writes nothing to the file and answers `saved=0`); FinishedOnlyOnItsSave
//! (`finished` only once a save covers the text the person had when they
//! pressed done); AbortKeepsOthersText (an abort never throws away unsaved
//! text the edit did not type). Buggy mutants: AnswerBeforeAck (`Buggy = 1`,
//! the `AnswerAtRequest` action), SecondOutcome (2, `ResolveAgain`),
//! OrphanCloses (3, in `Orphan`), AnswerWhileViewOpen (4, in `PersonCloses`),
//! AbortSaves (5, in `Abort`), FinishBeforeItsSave (6, in `Retire`) and
//! AbortDiscardsOthersText (7, in `Abort`). Tier-1: aterm-gui's
//! `edit_ticket_conformance.rs`, whose controls answer before
//! `checkpoint_ack`, save on abort and discard the person's text on abort,
//! and must fail.
//!
//! Slice N1 wrote the close half (closing the view is done); N3 added finish
//! (C-c C-c: the Finishing state, then the close once its save is durable)
//! and abort (C-c C-k: discard, then close).

use aterm_spec::derive::Model;

/// One ticket's life (aterm-gui's `app_edit`).
///
/// `state`: 0 not yet asked, 1 admitting, 2 open, 3 orphaned (nobody waits,
/// the view stays), 4 resolved, 5 dropped (refused, or cancelled before its
/// view opened). `view` is the ticket's own Editor view; `other` is a view of
/// the same file the person opened, which shares the document and must never
/// answer the ticket. `head`/`durable` are the shared document's sequence and
/// durable checkpoint, `start` the checkpoint when the view opened;
/// `closing`/`close_seq` a close of the ticket's view waiting for the
/// checkpoint to cover the sequence it froze. `waiter` is a parked
/// `await edit`; `outcomes` counts recorded outcomes; `answered` says a waiter
/// received the outcome, whose `seq=` is `answer_seq` (the durable checkpoint
/// when the view retired), whose `saved` bit claims the file holds the text
/// the view last showed (`close_seq`), and whose `result` is 1 closed, 2
/// finished or 3 aborted (0 before it resolves).
///
/// `finishing` is the Finishing state: done was pressed (`Finish`, C-c C-c)
/// with the document at `finish_seq`, a save of it was started, and the view
/// closes once the checkpoint covers it. A view that retires before then
/// (another view keeps the document, so its close forces no save) answers
/// `closed`, never `finished`; a save that did not land stops it finishing
/// (`FinishHalts`), and the view stays open. `aborting` says abort was pressed (`Abort`,
/// C-c C-k): its discard put the file's text back as a new sequence the
/// checkpoint covers at once, writing nothing, and its close comes next
/// (`AbortHalts` when that close did not happen). `writes` counts saves that
/// reached the file while the ticket's view was open, `abort_writes` its
/// value when abort was pressed.
///
/// `foreign` says the shared document holds unsaved text the edit did not
/// type: it was dirty when the ticket's view opened, or the person typed in
/// their own view since (`PersonEdits`; `Edit` is typing in the ticket's
/// view). A save covers it again. Abort discards only when nothing is
/// foreign; with foreign text it closes the ticket's view and keeps the text
/// in the person's view (a view that is not the document's last closes with
/// no save), and with no other view to keep it in, abort is refused. `lost`
/// says an abort threw such text away, which no abort may.
///
/// The view retires (`Retire`) once its close is covered, or at once when
/// another view keeps the document (a non-final close forces no checkpoint),
/// and the outcome says `saved=1` only when a save landed during the edit and
/// covers the frozen sequence. Each mutant is its own dial, dead at
/// `Buggy = 0`: a waiter answered when the close is requested, before its
/// checkpoint (`AnswerAtRequest`, Buggy 1); a second outcome for a resolved
/// ticket (`ResolveAgain`, Buggy 2); an orphan that closes its view
/// (`Orphan`, Buggy 3); the person's own view closing answering the ticket
/// while its view is still open (`PersonCloses`, Buggy 4); an abort that saves
/// the buffer instead of discarding it (`Abort`, Buggy 5); a finish that
/// answers `finished` whatever its save did (`Retire`, Buggy 6); and an
/// abort that discards the person's unsaved text with its own (`Abort`,
/// Buggy 7).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn amacs_edit_ticket_model() -> Model {
    aterm_spec::ty_model! {
        AmacsEditTicket {
            const Buggy = 0;
            const MaxSeq = 2;
            var state = 0;
            var view = 0;
            var other = 0;
            var head = 0;
            var durable = 0;
            var start = 0;
            var closing = 0;
            var close_seq = 0;
            var waiter = 0;
            var outcomes = 0;
            var answered = 0;
            var saved = 0;
            var answer_seq = 0;
            var result = 0;
            var finishing = 0;
            var finish_seq = 0;
            var aborting = 0;
            var writes = 0;
            var abort_writes = 0;
            var foreign = 0;
            var lost = 0;
            action Request when (state == 0) {
                state = 1;
            }
            action Refuse when (state == 1) {
                state = 5;
            }
            action Admit when (state == 1) {
                state = 2;
                view = 1;
                start = durable;
                foreign = if durable <= head - 1 { 1 } else { 0 };
            }
            action PersonOpens when (other == 0) {
                other = 1;
            }
            action PersonCloses when (other == 1) {
                other = 0;
                answered = if Buggy == 4 && waiter == 1 && view == 1 { 1 } else { answered };
                outcomes = if Buggy == 4 && waiter == 1 && view == 1 && answered == 0 {
                    outcomes + 1
                } else {
                    outcomes
                };
            }
            action Edit when (view == 1 && closing == 0 && aborting == 0 && head <= MaxSeq - 1) {
                head = head + 1;
            }
            action PersonEdits when (
                other == 1 && closing == 0 && aborting == 0 && head <= MaxSeq - 1
            ) {
                head = head + 1;
                foreign = if view == 1 { 1 } else { foreign };
            }
            action SaveAck when (
                (view == 1 || other == 1) && aborting == 0 && durable <= head - 1
            ) {
                durable = head;
                writes = if view == 1 { writes + 1 } else { writes };
                foreign = 0;
            }
            action Await when ((state == 2 || state == 3) && waiter == 0) {
                state = 2;
                waiter = 1;
            }
            action AwaitResolved when (state == 4 && answered == 0) {
                answered = 1;
            }
            action Withdraw when ((state == 2 || state == 3) && waiter == 1) {
                waiter = 0;
            }
            action Orphan when (state == 2 && waiter == 0) {
                state = 3;
                view = if Buggy == 3 { 0 } else { view };
            }
            action Finish when ((state == 2 || state == 3) && view == 1 && aborting == 0) {
                finishing = 1;
                finish_seq = head;
            }
            action FinishHalts when (finishing == 1) {
                finishing = 0;
            }
            action Abort when (
                (state == 2 || state == 3) && view == 1 && aborting == 0
                    && (foreign == 0 || other == 1 || Buggy == 7)
            ) {
                aborting = 1;
                finishing = 0;
                abort_writes = writes;
                head = if Buggy == 5 {
                    head
                } else {
                    if durable <= head - 1 && (foreign == 0 || Buggy == 7) { head + 1 } else { head }
                };
                durable = if Buggy == 5 {
                    head
                } else {
                    if durable <= head - 1 && (foreign == 0 || Buggy == 7) {
                        head + 1
                    } else {
                        durable
                    }
                };
                writes = if Buggy == 5 && durable <= head - 1 { writes + 1 } else { writes };
                lost = if Buggy == 7 && foreign == 1 { 1 } else { lost };
                foreign = if Buggy == 5 || Buggy == 7 { 0 } else { foreign };
            }
            action AbortHalts when (aborting == 1 && closing == 0) {
                aborting = 0;
            }
            action RequestClose when (view == 1 && closing == 0) {
                closing = 1;
                close_seq = head;
            }
            action Retire when (
                view == 1 && closing == 1 && (close_seq <= durable || other == 1)
            ) {
                view = 0;
                closing = 0;
                state = 4;
                outcomes = outcomes + 1;
                result = if aborting == 1 {
                    3
                } else {
                    if finishing == 1 && (finish_seq <= durable || Buggy == 6) { 2 } else { 1 }
                };
                saved = if aborting == 0 && close_seq <= durable && start <= durable - 1 {
                    1
                } else {
                    0
                };
                answer_seq = durable;
                answered = if waiter == 1 { 1 } else { answered };
                waiter = 0;
                finishing = 0;
                aborting = 0;
                foreign = 0;
            }
            action AnswerAtRequest when (
                Buggy == 1 && view == 1 && closing == 1 && waiter == 1 && answered == 0
            ) {
                answered = 1;
                outcomes = outcomes + 1;
                saved = 1;
                answer_seq = close_seq;
            }
            action ResolveAgain when (Buggy == 2 && state == 4 && outcomes <= 1) {
                outcomes = outcomes + 1;
            }
            invariant OneOutcome: outcomes <= 1;
            invariant SavedOnlyWhenAcked:
                if saved == 1 { close_seq <= durable && answer_seq <= durable } else { saved == 0 };
            invariant OrphanKeepsView:
                if state == 3 { view == 1 } else { view <= 1 };
            invariant WaiterAnsweredOnlyAfterViewGone:
                if answered == 1 { view == 0 } else { answered == 0 };
            invariant AbortNeverAdvancesCheckpoint:
                if aborting == 1 || result == 3 {
                    writes == abort_writes && saved == 0
                } else {
                    abort_writes <= writes
                };
            invariant FinishedOnlyOnItsSave:
                if result == 2 { finish_seq <= answer_seq } else { result <= 3 };
            invariant AbortKeepsOthersText: lost == 0;
            bound SequenceBounded: head <= MaxSeq + 1;
        }
    }
}
