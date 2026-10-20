// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)`: the anchor attributes are aterm-spec-anchor's `macro_rules!`
//! ones, decided by the fact that decides whether aterm-spec-anchor depends on the
//! `aterm-spec-macros` proc macros at all.
//!
//! Cargo resolves `[target.'cfg(not(trust_verify))'.dependencies]` from its own query
//! of the compiler's cfg, and hands every build script that same query as `CARGO_CFG_*`.
//! A Trust compiler answers it with `trust_verify` in the build, test, check, doc and
//! verified lanes, so there no proc macro sits under the anchors (Trust's PM-1 boundary
//! refuses to verify a library whose graph holds one) and the attributes are the
//! declarative ones, which need the `macro_attr` language feature in every crate that
//! writes them. A stock compiler, and Trust's lint driver (whose cfg query has no
//! `trust_verify`), resolve the proc-macro dependency and keep the proc macros, so the
//! lint gate reads the expansion it always read. Every anchored crate's build script
//! mounts this file: one fact on both sides, so the crate that defines the attributes
//! and the crates that write them cannot disagree.

/// Declare `cfg(anchor_rules)` for the calling crate, and set it when Cargo resolved
/// this build against a Trust compiler's verify cfg.
pub fn emit() {
    println!("cargo::rustc-check-cfg=cfg(anchor_rules)");
    if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
        println!("cargo::rustc-cfg=anchor_rules");
    }
}
