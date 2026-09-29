// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateRendezvousClaim`: the successor presents its
//! rendezvous claim only to the attested outgoing process, and the uid-only
//! gate that shipped before 2026-09-27 is the caught mutant.

#[test]
fn rendezvous_claim_proves_and_catches_the_uid_only_gate() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_rendezvous_claim_model(),
        "rendezvous claim presentation",
    );
}
