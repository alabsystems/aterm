// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One normal future-Wait receipt across one Off/On cycle, with a producer and GUI read
/// independently in flight. Both payload values are waits with a future next event; their
/// distinct values make stale replacement observable, and a current producer can republish
/// the original value without a new wall timestamp. The last delivered view remains until
/// another delivery: read-publication freshness is checked, not continuous instant UI agreement.
/// Exact execution receipts, current work, process binding and action permissions are inputs
/// outside this machine. The GUI conformance fixture exercises their existing boundaries.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_progress_policy_model() -> Model {
    aterm_spec::ty_model! {
        AccountProgressPolicy {
            const Buggy = 0;
            // The tiered helper checks Buggy=1. Fault selects its individual defect;
            // direct Buggy=2..8 also keep every law visible to registry non-vacuity.
            const Fault = 0;
            // turn is the observed policy history; epoch is the implementation's fence.
            // 0: initially on, 1: Off, 2: resumed. 3 means no pending producer/read.
            var turn = 0; var epoch = 0;
            var record_epoch = 0; var record_value = 0;
            var wanted_epoch = 0; var wanted_value = 0;
            var producer_epoch = 3; var producer_turn = 0; var producer_value = 0;
            var read_turn = 3; var read_epoch = 0; var read_record_epoch = 0;
            var read_wanted_epoch = 0;
            // view: not delivered, current normal wait, paused, unavailable/busy.
            var view = 0; var wanted_view = 0; var next = 0; var stale_read = 0;

            action CaptureSame when (producer_epoch == 3) {
                producer_epoch = epoch; producer_turn = turn; producer_value = 0;
            }
            action CaptureChanged when (producer_epoch == 3) {
                producer_epoch = epoch; producer_turn = turn; producer_value = 1;
            }
            action Off when (turn == 0) {
                turn = 1;
                epoch = if Buggy == 1 && (Fault == 0 || Fault == 1) {epoch} else {epoch + 1};
            }
            action On when (turn == 1) {
                turn = 2;
                epoch = if Buggy == 2 || (Buggy == 1 && Fault == 2) {epoch} else {epoch + 1};
            }
            action Publish when (producer_epoch <= 2) {
                // The oracle follows the policy event, never the possibly defective fence.
                wanted_epoch = if producer_turn == turn {turn} else {wanted_epoch};
                wanted_value = if producer_turn == turn {producer_value} else {wanted_value};
                record_epoch = if Buggy == 6 || (Buggy == 1 && Fault == 6) {record_epoch}
                    else if producer_epoch == epoch || Buggy == 3 || (Buggy == 1 && Fault == 3) {
                    producer_epoch
                } else {record_epoch};
                record_value = if Buggy == 6 || (Buggy == 1 && Fault == 6) {record_value}
                    else if producer_epoch == epoch || Buggy == 3 || (Buggy == 1 && Fault == 3) {
                    producer_value
                } else {record_value};
                producer_epoch = 3;
            }
            action Read when (read_turn == 3) {
                read_turn = turn; read_epoch = epoch;
                read_record_epoch = record_epoch; read_wanted_epoch = wanted_epoch;
                // A display observation cannot rewrite the retained producer's history.
                record_value = if (Buggy == 8 || (Buggy == 1 && Fault == 8)) && turn == 1 {
                    1 - record_value
                } else {record_value};
            }
            action Deliver when (read_turn <= 2) {
                stale_read = if read_turn == turn {0} else {1};
                wanted_view = if read_turn == turn {
                    if read_turn == 1 {2}
                    else if read_wanted_epoch == read_turn {1} else {2}
                } else {3};
                view = if read_epoch == epoch || Buggy == 7 || (Buggy == 1 && Fault == 7) {
                    if read_turn == 1 {2}
                    else if read_record_epoch == read_epoch || Buggy == 4 || (Buggy == 1 && Fault == 4) {1}
                    else {2}
                } else {3};
                next = if (read_epoch == epoch || Buggy == 7 || (Buggy == 1 && Fault == 7))
                    && (read_turn > 1 || 1 > read_turn)
                    && (read_record_epoch == read_epoch || Buggy == 4 || (Buggy == 1 && Fault == 4)) {1}
                    else if Buggy == 5 || (Buggy == 1 && Fault == 5) {1} else {0};
                read_turn = 3;
            }

            invariant PublicationUsesCurrentPolicy:
                record_epoch == wanted_epoch && record_value == wanted_value;
            invariant ProjectionUsesCapturedAuthority: view == wanted_view;
            invariant NoNextWithoutCurrentDecision:
                (view == 1 && next == 1) || ((view > 1 || 1 > view) && next == 0);
            invariant StaleReadWithdrawsClaims: stale_read == 0 || view == 3;
            bound PolicyRange: turn <= 2 && epoch <= 2;
            bound RecordRange: record_epoch <= 2 && wanted_epoch <= 2
                && record_value <= 1 && wanted_value <= 1;
            bound ProducerRange: producer_epoch <= 3 && producer_turn <= 2 && producer_value <= 1;
            bound ReadRange: read_turn <= 3 && read_epoch <= 2
                && read_record_epoch <= 2 && read_wanted_epoch <= 2;
            bound ProjectionRange: view <= 3 && wanted_view <= 3 && next <= 1 && stale_read <= 1;
        }
    }
}
