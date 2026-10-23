// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsEditTicket`: formal model for `edit` request ticket lifecycle, finish, and abort.

use aterm_spec::derive::Model;

/// Returns the `AmacsEditTicket` formal model (`aterm-gui` `app_edit`).
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
