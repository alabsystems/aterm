// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `NativeUpdateRendezvousGrant` (item 13 of the fifth
//! update-robustness round): every claim proves at `Buggy = 0`, and the mutant
//! family breaks it at `Buggy = 1`.

#[test]
fn rendezvous_grant_proves_and_catches_a_chunked_grant_an_old_successor_cannot_take() {
    aterm_spec::verify::prove_and_catch_scalar(
        &aterm_spec::derive::native_update_rendezvous_grant_model(),
        "rendezvous grant",
    );
}
