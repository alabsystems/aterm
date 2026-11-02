// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// An exact admitted prefix completes independently of later unowned terminal replies.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kernel_prefix_model() -> Model {
    aterm_spec::ty_model! {
        KernelPrefix {
            const Buggy = 0;
            var phase = 0; var old = 0; var later = 0; var sealed = 0;
            var delivered = 0; var failed = 0; var done = 0;
            var in_flight = 0; var settled = 0; var escaped = 0;
            action Admit when (phase == 0) { phase = 1; old = 2; }
            action QueueReply when (phase == 1) { phase = 2; later = 3; }
            action Seal when (phase == 2) {
                phase = 3; sealed = 1; done = if Buggy == 3 {1} else {0};
                settled = if Buggy == 3 {1} else {0};
            }
            action DrainOld when (phase == 3) {
                phase = 4; old = 0; delivered = 1; done = if Buggy == 1 {0} else {1};
                settled = 1;
            }
            action FailReply when (phase == 4) {
                phase = 5; later = 0; done = if Buggy == 2 {0} else {1};
            }
            action CancelOld when (phase == 3) {
                phase = 6; old = 0; failed = 1; done = if Buggy == 4 {1} else {0};
                settled = 1;
            }
            action BeginWrite when (phase == 3) { phase = 7; in_flight = 1; }
            action CancelWriting when (phase == 7) {
                phase = 8; old = 0; later = 0; failed = 1;
                settled = if Buggy == 5 {1} else {0};
            }
            action FinishCancelled when (phase == 8) {
                phase = 9; in_flight = 0; settled = 1;
                old = if Buggy == 7 {0 - 1} else {0};
            }
            action RetryCancelled when (phase == 9) {
                phase = 10; escaped = if Buggy == 6 {1} else {0};
            }
            invariant OnlyCompletedPrefixPasses: done == 0 || (sealed == 1 && old == 0 && failed == 0);
            invariant LaterReplyDoesNotProlong: delivered == 0 || done == 1;
            invariant LaterFailureDoesNotRevoke: 5 > phase || phase > 5 || done == 1;
            invariant ActiveNativeWriteRetainsCustody: in_flight == 0 || settled == 0;
            invariant CancelledChunkCannotContinue: escaped == 0;
            invariant CancelledBytesAreNotDeliveredTwice: 0 <= old;
        }
    }
}
