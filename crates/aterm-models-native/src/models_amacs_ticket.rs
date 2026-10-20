// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsEditTicket`: one `edit` request's ticket, from admission to its one
//! outcome (docs/DESIGN-amacs-2026-10-19.md §4.9; slice N1, extended by N3).
//!
//! Invariants: OneOutcome; SavedOnlyWhenAcked (`saved=1` implies the
//! checkpoint reached the requested seq); OrphanKeepsView;
//! WaiterAnsweredOnlyAfterViewGone. Buggy mutants: AnswerBeforeAck (`Buggy = 1`,
//! the `AnswerAtRequest` action), SecondOutcome (2, `ResolveAgain`),
//! OrphanCloses (3, in `Orphan`) and AnswerWhileViewOpen (4, in
//! `PersonCloses`). Tier-1: aterm-gui's `edit_ticket_conformance.rs`, whose
//! control answers before `checkpoint_ack` and must fail.
//!
//! Slice N1 writes the close half (closing the view is done); N3 adds finish
//! and abort, with AbortNeverAdvancesCheckpoint and its AbortSaves mutant.

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
/// when the view retired) and whose `saved` bit claims the file holds the text
/// the view last showed (`close_seq`).
///
/// The view retires (`Retire`) once its close is covered, or at once when
/// another view keeps the document (a non-final close forces no checkpoint),
/// and the outcome says `saved=1` only when a save landed during the edit and
/// covers the frozen sequence. Each mutant is its own dial, dead at
/// `Buggy = 0`: a waiter answered when the close is requested, before its
/// checkpoint (`AnswerAtRequest`, Buggy 1); a second outcome for a resolved
/// ticket (`ResolveAgain`, Buggy 2); an orphan that closes its view
/// (`Orphan`, Buggy 3); and the person's own view closing answering the
/// ticket while its view is still open (`PersonCloses`, Buggy 4).
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
            action Edit when (
                (view == 1 || other == 1) && closing == 0 && head <= MaxSeq - 1
            ) {
                head = head + 1;
            }
            action SaveAck when ((view == 1 || other == 1) && durable <= head - 1) {
                durable = head;
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
                saved = if close_seq <= durable && start <= durable - 1 { 1 } else { 0 };
                answer_seq = durable;
                answered = if waiter == 1 { 1 } else { answered };
                waiter = 0;
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
            bound SequenceBounded: head <= MaxSeq;
        }
    }
}
