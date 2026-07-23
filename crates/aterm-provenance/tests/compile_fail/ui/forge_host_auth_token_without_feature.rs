// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Compile-fail (#8013): without the `internal-mint` feature,
//! `HostAuthorizationToken::__new_for_capability_only` is not reachable from
//! outside `aterm-provenance`. This fixture attempts to mint the capability
//! token from a hypothetical downstream consumer that has NOT enabled the
//! feature, and must fail to compile with an E0599 "no associated item"
//! error.
//!
//! This is the core regression guard for the provenance capability seal:
//! prior to #8013 the constructor was unconditionally `pub`, and any
//! workspace crate could mint a `HostAuthorizationToken`, wrap
//! attacker-controlled bytes as `Provenanced<T, Host>`, and short-circuit
//! the entire lattice.
//!
//! Note that this fixture intentionally does NOT use
//! `cfg(feature = "internal-mint")` — we want the constructor call to be
//! unconditional so that the compile error reproduces under every build
//! configuration, including the default one.

use aterm_provenance::HostAuthorizationToken;

fn main() {
    // ERROR: no associated item `__new_for_capability_only` in scope unless
    // the caller crate enables the `aterm-provenance/internal-mint` feature.
    let _tok: HostAuthorizationToken<'_> = HostAuthorizationToken::__new_for_capability_only();
}
