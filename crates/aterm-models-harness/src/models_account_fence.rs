// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One abandoned candidate and one later candidate share a retained parent.
/// Disk fate is independent of the temporary in-memory reservation lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_fence_model() -> Model {
    aterm_spec::ty_model! {
        AccountFence {
            const Buggy = 0;
            // disk: absent, claimed, fenced. pending: none, A, fenced A, B.
            var disk = 0; var next_disk = 0; var pending = 0; var reserved = 0;
            var operation = 0; var writer = 0; var drain = 0;
            var ack = 0; var checked = 0; var bad_confirm = 0; var lost_next = 0; var claimed = 0;
            var gc = 0; var reclaimed = 0;
            var next_claimed = 0; var next_before = 0;
            action Reserve when (disk == 0 && pending == 0 && reserved == 0 && operation == 0 && next_disk == 0) {
                reserved = 1;
            }
            action Insert when (reserved == 1 && pending == 0) {
                reserved = 0; operation = 1; writer = 1; pending = 1;
            }
            action CancelReservation when (reserved == 1) { reserved = 0; checked = 0; }
            action FinishWriter when (writer == 1) { writer = 0; checked = 0; }
            action HoldDrain when (operation == 1 && drain == 0 && ack == 0) { drain = 1; checked = 0; }
            action Drain when (drain == 1) { drain = 0; checked = 0; }
            action ClaimWrite when (disk == 0 && operation == 1 && writer == 1) { disk = 1; claimed = 1; }
            action FenceWrite when (disk == 0 || (disk == 1 && Buggy == 6)) { disk = 2; }
            action Settle when (disk == 2) {
                checked = 1;
                next_disk = if Buggy == 9 {0} else {next_disk};
                ack = if (reserved == 0 || Buggy == 1) && (writer == 0 || Buggy == 2) && (drain == 0 || Buggy == 3) {if Buggy == 7 {0} else {1}} else {0};
                operation = if (reserved == 0 || Buggy == 1) && (writer == 0 || Buggy == 2) && (drain == 0 || Buggy == 3) {0} else {operation};
                lost_next = if pending == 3 && Buggy == 5 && reserved == 0 && writer == 0 && drain == 0 {1} else {lost_next};
                pending = if (reserved == 0 || Buggy == 1) && (writer == 0 || Buggy == 2) && (drain == 0 || Buggy == 3) {
                    if Buggy == 4 {pending} else {
                        if pending == 1 || pending == 2 || Buggy == 5 {0} else {pending}
                    }
                } else {
                    if Buggy == 4 {pending} else {
                        if pending == 1 || (pending == 0 && (reserved == 1 || operation == 1)) {2} else {pending}
                    }
                };
            }
            action LateConfirm when (disk == 2 && pending == 1 && checked == 1) { bad_confirm = 1; }
            action ReserveNext when (((disk == 2 && ack == 1) || (disk == 0 && reserved == 0 && operation == 0)) && pending == 0 && next_disk == 0) { pending = 3; }
            action NextClaim when (pending == 3 && next_disk == 0) {
                pending = 0; next_disk = 1; next_claimed = 1;
                next_before = if disk == 0 {1} else {next_before};
            }
            action Reclaim when ((pending == 1 || pending == 2) && gc == 0) {
                gc = 1;
                reclaimed = if Buggy == 8 {1} else {0};
            }
            invariant ReservationDrained: ack == 0 || reserved == 0;
            invariant WriterDrained: ack == 0 || writer == 0;
            invariant CustodyDrained: ack == 0 || drain == 0;
            invariant NoLateClaim: bad_confirm == 0;
            invariant NextPreserved: lost_next == 0;
            invariant ClaimedPermanent: claimed == 0 || disk == 1;
            invariant BusyPinned: checked == 0 || (reserved == 0 && writer == 0 && drain == 0) || pending == 2;
            invariant QuiescentAvailable: checked == 0 || reserved == 1 || writer == 1 || drain == 1 || ack == 1;
            invariant PendingRetained: reclaimed == 0;
            invariant NextPermanent: next_claimed == 0 || next_disk == 1;
            bound Finite: 0 <= disk && disk <= 2 && 0 <= pending && pending <= 3
                && 0 <= next_disk && next_disk <= 1
                && 0 <= reserved && reserved <= 1 && 0 <= operation && operation <= 1
                && 0 <= writer && writer <= 1 && 0 <= drain && drain <= 1
                && 0 <= ack && ack <= 1 && 0 <= checked && checked <= 1
                && 0 <= bad_confirm && bad_confirm <= 1 && 0 <= lost_next && lost_next <= 1
                && 0 <= claimed && claimed <= 1 && 0 <= gc && gc <= 1 && 0 <= reclaimed && reclaimed <= 1
                && 0 <= next_claimed && next_claimed <= 1 && 0 <= next_before && next_before <= 1;
        }
    }
}
