// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-member apply dispatch (§16.4): how a staged, verified artifact goes live, keyed on
//! both halves of its row. `kind` says what lands and how it goes live (a `binary` shims, a
//! `sysroot-bundle` is resolve-checked, an `app-bundle` is a `.app`); `protocol` says where
//! the bytes come from (a `github-release` asset under the account slug, or an `https`
//! download from a vendor host the signed row pins). The retired `kind = "vendor-fetch"`
//! is refused at parse ([`crate::sig::Reject::RetiredKind`]) and never reaches here.
//!
//! The aterm `.app` is not atpkg's to apply: the app's own updater hands every PTY to a
//! successor in-session, so atpkg refuses it. A vendor's `.app` lands in the store and is
//! shimmed through its `links`; it has its own variant so it is never confused with the
//! self-update. An unknown pair fails closed ([`Unknown`]) rather than being installed as
//! a plain binary.
//!
//! [`Unknown`]: ApplyStrategy::Unknown

/// How a staged artifact is made live, dispatched on the row's `(kind, protocol)` (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// A plain binary or from-source build: flip the channel `current` and shim each exposed
    /// binary into `bin/` (§10). `binary` activates the same over `github-release` and
    /// `https`; [`crate::flow`] picks the download lane from `protocol`, not this variant.
    Shim,
    /// A `trust`/`trust-mc` sysroot bundle: extract, then the fail-loud resolve check
    /// (`sysroot::resolve_check`: each exposed native binary must reach an exit code on
    /// `--version`) before the install counts. Bundles are `self-contained` (relocated at
    /// pack time), so applying one relocates nothing. Its `exposes` still shim.
    SysrootBundle,
    /// The `aterm.app` DMG (`app-bundle` over `github-release`): applied in-session by the
    /// app's own updater (aterm-update), never by atpkg, which refuses it
    /// ([`crate::FlowError::AppBundleRefused`]) and never stages or swaps it.
    AppBundle,
    /// A vendor's `.app` (`app-bundle` over `https`, `payload = "dmg"`): the single `.app`
    /// at the image root is copied into the store at the modes every lane lays
    /// (`extract::safe_mode`) and exposed through the row's `links`, activating like
    /// [`Shim`]. Its own variant so the [`AppBundle`](ApplyStrategy::AppBundle) refusal
    /// never catches a vendor app and a vendor app is never taken for the aterm self-update.
    ///
    /// [`Shim`]: ApplyStrategy::Shim
    VendorApp,
    /// An unrecognized `(kind, protocol)` pair: refused, never defaulted to
    /// [`Shim`](ApplyStrategy::Shim).
    Unknown,
}

/// Map a row's `kind` and `protocol` to its [`ApplyStrategy`]. Fails closed on anything
/// unrecognized, including a known kind over a protocol that cannot carry it (a
/// `sysroot-bundle` over `https`) and the unsupported OS-installer protocols (`pkg`,
/// `system-pm`, `softwareupdate`).
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

    /// Every known pair maps to its strategy; anything else — a known kind over a
    /// protocol that cannot carry it, a deleted OS-installer protocol, a misspelling —
    /// fails closed to `Unknown`, never `Shim`.
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
