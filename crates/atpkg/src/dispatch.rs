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
        ("github-release", "binary" | "cargo-src") => ApplyStrategy::Shim,
        ("github-release", "sysroot-bundle") => ApplyStrategy::SysrootBundle,
        ("github-release", "app-bundle") => ApplyStrategy::AppBundle,
        ("https", "binary") => ApplyStrategy::Shim,
        ("https", "app-bundle") => ApplyStrategy::VendorApp,
        _ => ApplyStrategy::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_map_to_their_strategy_and_the_rest_fail_closed() {
        use ApplyStrategy::{AppBundle, Shim, SysrootBundle, Unknown, VendorApp};
        for (kind, protocol, want) in [
            ("binary", "github-release", Shim),
            ("cargo-src", "github-release", Shim),
            ("sysroot-bundle", "github-release", SysrootBundle),
            ("app-bundle", "github-release", AppBundle),
            ("binary", "https", Shim),
            ("app-bundle", "https", VendorApp),
            ("sysroot-bundle", "https", Unknown),
            ("cargo-src", "https", Unknown),
            ("binary", "pkg", Unknown),
            ("system-package", "softwareupdate", Unknown),
            ("installer-pkg", "github-release", Unknown),
            ("binary", "HTTPS", Unknown),
            ("Binary", "github-release", Unknown),
            ("", "github-release", Unknown),
            ("binary", "", Unknown),
        ] {
            assert_eq!(strategy_for(kind, protocol), want, "{kind}/{protocol}");
        }
    }
}
