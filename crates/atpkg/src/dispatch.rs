// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-member apply dispatch by `(kind, protocol)` pair.

/// How a staged artifact is made live, dispatched on the row's `(kind, protocol)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// Plain binary or from-source build activated by shimming into `bin/`.
    Shim,
    /// Sysroot bundle extracted, resolve-checked, and shimmed.
    SysrootBundle,
    /// The `aterm.app` bundle, refused by `atpkg` and applied in-session by `aterm-update`.
    AppBundle,
    /// Vendor `.app` copied into the store and exposed through the row's `links`.
    VendorApp,
    /// Unrecognized `(kind, protocol)` pair, refused fail-closed.
    Unknown,
}

/// Map a row's `kind` and `protocol` to its [`ApplyStrategy`], failing closed on unknown pairs.
#[must_use]
pub fn strategy_for(kind: &str, protocol: &str) -> ApplyStrategy {
    match (protocol, kind) {
        ("github-release", "binary" | "cargo-src" | "elf-binary") => ApplyStrategy::Shim,
        ("github-release", "sysroot-bundle") => ApplyStrategy::SysrootBundle,
        ("github-release", "app-bundle") => ApplyStrategy::AppBundle,
        ("https", "binary") => ApplyStrategy::Shim,
        ("https", "app-bundle") => ApplyStrategy::VendorApp,
        _ => ApplyStrategy::Unknown,
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
