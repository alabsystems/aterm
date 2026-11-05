// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Three consecutive NoExec replacements, two competing IDs at each link,
/// and three physical capacities. Reopen is observation, never a live grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn noexec_successor_model() -> Model {
    aterm_spec::ty_model! {
        NoExecSuccessor {
            const Buggy = 0;
            var slots = 5;
            var byte_limit = 2595;
            var bytes = 484;
            var depth = 0;
            // 1 prepared, 2 may execute, 3 NoExec, 4/5 opaque without/with May.
            var tail = 1;
            var one = 0; var two = 0; var three = 0;
            var records = 1;
            var ack = 0;
            var changed = 0; var unsafe_parent = 0; var authority = 0;
            action Room7 when (depth == 0 && tail == 1 && records == 1) { slots = 7; byte_limit = 3633; }
            action Room9 when (depth == 0 && tail == 1 && records == 1) { slots = 9; byte_limit = 4671; }
            action TightBytes when (depth == 0 && tail == 1 && records == 1 && slots == 5) { byte_limit = 2420; }
            action May when (tail == 1) { tail = 2; records = records + 1; bytes = bytes + 484; }
            action NoExec when (tail == 1) { tail = 3; records = records + 1; bytes = bytes + 484; }
            action Seal when (tail == 1 || tail == 2) {
                tail = if tail == 2 {5} else {4}; records = records + 1; bytes = bytes + 484;
            }
            action ClaimA when (3 > depth && (tail == 3 || Buggy == 2)
                && (records + (if tail == 1 {2} else {if tail == 2 {1} else {0}}) + 3 <= slots || Buggy == 5)
                && (bytes + 1487 <= byte_limit || Buggy == 5 || Buggy == 10)) {
                unsafe_parent = if tail == 3 {unsafe_parent} else {1};
                one = if depth == 0 {if Buggy == 8 {one} else {1}} else {one};
                two = if depth == 1 {1} else {two};
                three = if depth == 2 {1} else {three};
                depth = depth + 1; tail = 1; records = records + 1;
                bytes = if Buggy == 11 {bytes} else {bytes + 519};
                ack = depth + 1;
            }
            action ClaimB when (3 > depth && tail == 3 && records + 3 <= slots && bytes + 1487 <= byte_limit) {
                one = if depth == 0 {2} else {one};
                two = if depth == 1 {2} else {two};
                three = if depth == 2 {2} else {three};
                depth = depth + 1; tail = 1; records = records + 1;
                bytes = bytes + 519;
                ack = depth + 1;
            }
            action ClaimLostAck when (3 > depth && tail == 3 && records + 3 <= slots && bytes + 1487 <= byte_limit) {
                one = if depth == 0 {1} else {one};
                two = if depth == 1 {1} else {two};
                three = if depth == 2 {1} else {three};
                depth = depth + 1; tail = 1; records = records + 1;
                bytes = bytes + 519;
            }
            action Retry when (depth > 0 && tail == 1) {
                ack = depth;
                records = if Buggy == 6 && 9 > records {records + 1} else {records};
            }
            action RetryClosed when (depth > 0 && tail > 1) {
                changed = if Buggy == 9 {1} else {changed};
            }
            action Rival when (depth > 0) {
                changed = if Buggy == 1 {1} else {changed};
            }
            action Conflict when (depth > 0) {
                changed = if Buggy == 4 {1} else {changed};
            }
            action FailedClaim when (3 > depth && tail == 3) {
                ack = if Buggy == 3 {depth + 1} else {ack};
            }
            action Reopen { authority = if Buggy == 7 {1} else {0}; }
            invariant NoUnsafeParent: unsafe_parent == 0;
            invariant ImmutableClaims: changed == 0;
            invariant Acknowledged: ack <= depth;
            invariant ObservationOnly: authority == 0;
            invariant Capacity: records + (if tail == 1 {2} else {if tail == 2 {1} else {0}}) <= slots;
            invariant ByteCapacity: bytes + (if tail == 1 {968} else {if tail == 2 {484} else {0}}) <= byte_limit;
            invariant CountedBytes: bytes == (if depth == 0 {484} else {if depth == 1 {1487} else {if depth == 2 {2490} else {3493}}})
                + (if tail == 1 {0} else {if tail == 5 {968} else {484}});
            invariant Counted: records == depth + depth + (if tail == 1 {1} else {if tail == 5 {3} else {2}});
            invariant Chain: (one > 0 || depth == 0) && (one == 0 || depth > 0)
                && (two > 0 || 2 > depth) && (two == 0 || depth > 1)
                && (three > 0 || 3 > depth) && (three == 0 || depth == 3);
            bound Finite: 0 <= depth && depth <= 3 && 1 <= tail && tail <= 5
                && 0 <= one && one <= 2 && 0 <= two && two <= 2 && 0 <= three && three <= 2
                && 0 <= ack && ack <= 4 && 1 <= records && records <= 11
                && 5 <= slots && slots <= 9
                && 0 <= bytes && bytes <= 6000
                && 2420 <= byte_limit && byte_limit <= 4671
                && 0 <= changed && changed <= 1 && 0 <= unsafe_parent && unsafe_parent <= 1
                && 0 <= authority && authority <= 1;
        }
    }
}
