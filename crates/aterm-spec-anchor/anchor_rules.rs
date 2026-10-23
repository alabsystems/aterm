// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)`: the anchor attributes are aterm-spec-anchor's `macro_rules!` ones, decided
//! by the fact that decides whether aterm-spec-anchor depends on the `aterm-spec-macros` proc
//! macros at all.

/// Declare `cfg(anchor_rules)` for the calling crate, and set it when Cargo resolved this build
/// against a Trust compiler's verify cfg.
pub fn emit() {
    println!("cargo::rustc-check-cfg=cfg(anchor_rules)");
    if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
        println!("cargo::rustc-cfg=anchor_rules");
    }
}
