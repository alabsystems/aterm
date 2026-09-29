// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seamless handoff's rendezvous GRANT across a version boundary (item 13 of
//! the fifth update-robustness round): which grant a parent sends to which claim,
//! and what a successor may adopt of it.

use super::Model;

/// One launched-lane handoff, from the candidate the parent verified to the
/// descriptors its successor adopts.
///
/// The environment chooses the successor's code (`capable`: it reads the chunked
/// `ATRZ2G` grant), whether its verified `Info.plist` declares that
/// (`declared`, only ever of code that can), and whether the pool outgrew one
/// descriptor message (`large`: more than 62 sessions). The parent may offer the
/// chunked grant (`ATERM_HANDOFF_GRANT_CAPS`); the successor claims `ATRZ2C`
/// when offered and capable, else `ATRZ1C`. The parent then grants one message
/// (`ATRZ1G`), two (`ATRZ2G`, the model's stand-in for "more than one"), or
/// refuses before any descriptor leaves. Messages are sent and taken one at a
/// time; the parent may stop after the first (it died, or its write failed), and
/// the successor either adopts the WHOLE grant or refuses and closes everything
/// it received.
///
/// The claims, each with a mutant that breaks it alone:
/// * `OfferedOnlyWhenDeclared` — the capability is offered only to a candidate
///   whose sealed bundle declares it (mutant: offer to any candidate).
/// * `ChunkedClaimOnlyWhenOffered` — a successor claims `ATRZ2C` only when its
///   parent offered it, so a parent that never heard of it is claimed as always
///   (mutant: claim it unoffered).
/// * `ChunkedGrantOnlyToChunkedClaim` — no `ATRZ1C` claimant, i.e. no build
///   before the chunked grant, is ever sent `ATRZ2G` (mutant: grant it anyway).
/// * `OneMessageWhenItFits` — up to 62 sessions the grant is the one-message
///   `ATRZ1G` whatever was claimed (mutant: chunk a small pool).
/// * `AdoptionIsWhole` — a successor adopts only a grant it holds every message
///   of (mutant: adopt after the first chunk).
/// * `RefusalClosesEverything` — a refusal leaves nothing held (mutant: a
///   refusal that keeps what arrived).
///
/// Tier-1: `handoff_rendezvous`'s `rendezvous_grant_conformance` tests drive the
/// shipping decisions (`grant_shape`, `offers_chunked_grant`, the claim frames)
/// and a real chunked grant over a real rendezvous, cut short and whole.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_rendezvous_grant_model() -> Model {
    crate::ty_model! {
        NativeUpdateRendezvousGrant {
            const Buggy = 0;
            // 0 choosing, 1 launched, 2 claimed, 3 granting, 4 the parent stopped.
            var phase = 0;
            var capable = 0;
            var declared = 0;
            var large = 0;
            var offered = 0;
            // 1 ATRZ1C, 2 ATRZ2C.
            var claimed = 0;
            // 1 ATRZ1G, 2 ATRZ2G, 3 refused before any descriptor left.
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
