// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seamless handoff's rendezvous grant across a version boundary.

use aterm_spec::derive::Model;

/// One launched-lane handoff, from the candidate the parent verified to the descriptors its
/// successor adopts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_grant_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateRendezvousGrant {
            const Buggy = 0;
            var phase = 0;
            var capable = 0;
            var declared = 0;
            var large = 0;
            var offered = 0;
            var claimed = 0;
            var grant = 0;
            var need = 0;
            var sent = 0;
            var held = 0;
            var adopted = 0;
            var closed = 0;

            action Upgrade when (phase == 0 && capable == 0) {
                capable = 1;
            }
            action Declare when (phase == 0 && capable == 1 && declared == 0) {
                declared = 1;
            }
            action Grow when (phase == 0 && large == 0) {
                large = 1;
            }
            action Offer when (phase == 0 && offered == 0 && (declared == 1 || Buggy == 1)) {
                offered = 1;
            }
            action Launch when (phase == 0) {
                phase = 1;
            }
            action ClaimOne when (phase == 1 && (offered == 0 || capable == 0)) {
                claimed = 1;
                phase = 2;
            }
            action ClaimChunks when (phase == 1 && capable == 1 && (offered == 1 || Buggy == 1)) {
                claimed = 2;
                phase = 2;
            }
            action GrantOne when (phase == 2 && large == 0) {
                grant = 1;
                need = 1;
                phase = 3;
            }
            action GrantChunks when (
                phase == 2 && (large == 1 || Buggy == 1) && (claimed == 2 || Buggy == 1)
            ) {
                grant = 2;
                need = 2;
                phase = 3;
            }
            action RefuseTooMany when (phase == 2 && large == 1 && claimed == 1) {
                grant = 3;
                phase = 4;
            }
            action Send when (phase == 3 && sent <= need - 1) {
                sent = sent + 1;
            }
            action Stop when (phase == 3 && sent > 0 && sent <= need - 1) {
                phase = 4;
            }
            action Take when (
                phase > 2 && held <= sent - 1 && adopted == 0 && closed == 0
            ) {
                held = held + 1;
            }
            action Adopt when (
                adopted == 0 && closed == 0 && (grant == 1 || claimed == 2) &&
                held > 0 && (held == need || Buggy == 1)
            ) {
                adopted = 1;
            }
            action Refuse when (
                adopted == 0 && closed == 0 && held == sent && held > 0 &&
                ((phase == 4 && held <= need - 1) || (grant == 2 && claimed == 1))
            ) {
                closed = 1;
                held = if Buggy == 1 { held } else { 0 };
            }

            invariant OfferedOnlyWhenDeclared: offered <= declared;
            invariant ChunkedClaimOnlyWhenOffered: claimed <= 1 || offered == 1;
            invariant ChunkedGrantOnlyToChunkedClaim: grant <= 1 || grant == 3 || claimed == 2;
            invariant OneMessageWhenItFits: grant <= 1 || grant == 3 || large == 1;
            invariant AdoptionIsWhole: adopted == 0 || held == need;
            invariant RefusalClosesEverything: closed == 0 || held == 0;
        }
    }
}
