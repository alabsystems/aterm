// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail (#8013): without the `internal-mint` feature,
//! `NetworkAuthorizationToken::__new_for_capability_only` is not reachable
//! from outside `aterm-provenance`. Parallel regression guard to
//! `forge_host_auth_token_without_feature.rs` for the network-origin lift
//! ceremony.

use aterm_provenance::NetworkAuthorizationToken;

fn main() {
    // ERROR: no associated item `__new_for_capability_only` in scope unless
    // the caller crate enables the `aterm-provenance/internal-mint` feature.
    let _tok: NetworkAuthorizationToken<'_> =
        NetworkAuthorizationToken::__new_for_capability_only();
}
