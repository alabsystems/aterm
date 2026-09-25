// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-member apply dispatch (§16.4 / §10.1) — the decision of *how* a staged, verified
//! artifact is made live, keyed on BOTH halves of the row: its `kind` (the payload /
//! apply shape) and its `protocol` (how the bytes are obtained).
//!
//! The two axes are deliberately separate. `kind` says what lands and how it goes live —
//! a `binary` shims, a `sysroot-bundle` relocates, an `app-bundle` is a `.app`.
//! `protocol` says where the bytes come from — a `github-release` asset under the account
//! slug, or an `https` download from a vendor host pinned by the signed row. The retired
//! `kind = "vendor-fetch"` conflated the two; it is refused at parse
//! ([`crate::sig::Reject::RetiredKind`]) and never reaches here. The OS-installer
//! protocols (`pkg`, `system-pm`, `softwareupdate`) were deleted 2026-09-24 (design
//! 2026-09-22 §5.3(b)); a row naming one is an unknown pair like any other, refused.
//!
//! The design deliberately does **not** apply every member the same way (§16.4): a CLI
//! tool flips immediately (a `bin/` shim), `trust`/`trust-mc` need a sysroot relocation
//! (§10.1), and `aterm.app` itself is **not** a tarball at all — it is a notarized DMG the
//! app's own updater applies IN-SESSION by the overlap handoff (a successor is spawned and
//! every PTY is handed across; the cold-launch swap is only the fallback), never the
//! immediate shim flip — atpkg refuses it, always (§16.4). A
//! VENDOR's `.app` (a DMG from the vendor's host) is a different thing again:
//! it lands in the store and is shimmed through its `links`, and it must never be confused
//! with the self-update topology — so it gets its own variant. [`strategy_for`] is that
//! pure mapping; an unknown pair is **fail-closed** ([`Unknown`]) so a member the client
//! cannot install is never silently treated as a plain binary.
//!
//! [`Unknown`]: ApplyStrategy::Unknown

/// How a staged artifact is made live, dispatched on the row's `(kind, protocol)` (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// A plain binary / from-source build: symlink-flip the channel `current` and shim each
    /// exposed binary into `bin/` (§10). The immediate, tool path — reached by a
    /// `github-release` asset AND by an `https` vendor download (`binary` over either
    /// protocol activates identically; only the download lane differs, and
    /// [`crate::flow`] picks that lane from `protocol`, never from this variant).
    Shim,
    /// A `trust`/`trust-mc` sysroot bundle: extract, relocate the (dangling) sysroot
    /// toolchain link to its resolved toolchain, gated
    /// on the four-component nightly being installed (§10.1). Its `exposes` still shim.
    SysrootBundle,
    /// The `aterm.app` DMG: NOT extracted as a tarball — applied **in-session by the app's
    /// own updater** (the seamless overlap handoff in aterm-gui; `renamex_np(RENAME_SWAP)`
    /// lives in aterm-update, never here), never the immediate flip (§16.4). A different
    /// apply *topology*, dispatched here so it can never be symlink-flipped like a tool —
    /// atpkg REFUSES it, always ([`crate::FlowError::AppBundleRefused`]), and never stages
    /// or swaps it. `app-bundle` over `github-release` ONLY.
    AppBundle,
    /// A VENDOR's `.app` (`app-bundle` over `https`, `payload = "dmg"`): the single
    /// `.app` at the image root is copied into the store with its mode bits preserved and
    /// exposed through the row's `links` — i.e. it activates like [`Shim`], through the
    /// ordinary `bin/` flip. A DISTINCT variant so the self-update
    /// [`AppBundle`](ApplyStrategy::AppBundle) gate is never taken for a vendor app, and
    /// a vendor app is never mistaken for the aterm self-update.
    ///
    /// [`Shim`]: ApplyStrategy::Shim
    VendorApp,
    /// An unrecognized `(kind, protocol)` pair — fail closed: the client refuses to apply an
    /// artifact it does not know how to install (never default to
    /// [`Shim`](ApplyStrategy::Shim)).
    Unknown,
}

/// The protocols a row may declare, in the spelling the schema signs.
pub const PROTOCOLS: &[&str] = &["github-release", "https"];

/// Map a row's `kind` and `protocol` to its [`ApplyStrategy`]. Fail-closed on anything
/// unrecognized — including a known kind over a protocol that cannot carry it (a
/// `sysroot-bundle` over `https`) and every row of a deleted protocol (`pkg`,
/// `system-pm`, `softwareupdate`) — so a future/garbled row is never mis-applied as a
/// plain binary.
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
    fn known_pairs_map_to_their_strategy() {
        assert_eq!(
            strategy_for("binary", "github-release"),
            ApplyStrategy::Shim
        );
        assert_eq!(
            strategy_for("cargo-src", "github-release"),
            ApplyStrategy::Shim
        );
        assert_eq!(
            strategy_for("sysroot-bundle", "github-release"),
            ApplyStrategy::SysrootBundle
        );
        assert_eq!(
            strategy_for("app-bundle", "github-release"),
            ApplyStrategy::AppBundle
        );
        assert_eq!(strategy_for("binary", "https"), ApplyStrategy::Shim);
        assert_eq!(
            strategy_for("app-bundle", "https"),
            ApplyStrategy::VendorApp
        );
    }

    /// The OS-installer protocols were deleted (design 2026-09-22 §5.3(b)): the pairs
    /// they carried are Unknown, so an index naming one is refused, never applied.
    #[test]
    fn the_deleted_os_installer_pairs_fail_closed() {
        for (kind, protocol) in [
            ("installer-pkg", "pkg"),
            ("system-package", "system-pm"),
            ("system-package", "softwareupdate"),
        ] {
            assert_eq!(
                strategy_for(kind, protocol),
                ApplyStrategy::Unknown,
                "{kind}/{protocol}"
            );
        }
    }

    /// The vendor `.app` and the aterm self-update are DIFFERENT strategies: the former
    /// lands in the store and shims, the latter is refused so the app's own in-session
    /// updater stays the only thing that applies it. Neither may ever be taken for the other, and neither
    /// is a plain Shim.
    #[test]
    fn a_vendor_app_is_not_the_self_update_app_bundle() {
        assert_ne!(
            strategy_for("app-bundle", "https"),
            strategy_for("app-bundle", "github-release")
        );
        assert_ne!(strategy_for("app-bundle", "https"), ApplyStrategy::Shim);
        assert_ne!(strategy_for("app-bundle", "https"), ApplyStrategy::Unknown);
    }

    /// The retired `vendor-fetch` spelling is refused at PARSE; if one ever reached
    /// dispatch it would still fail closed, over every protocol.
    #[test]
    fn the_retired_vendor_fetch_kind_is_unknown_everywhere() {
        for protocol in PROTOCOLS {
            assert_eq!(
                strategy_for("vendor-fetch", protocol),
                ApplyStrategy::Unknown,
                "{protocol}"
            );
        }
    }

    /// A kind over a protocol that cannot carry it is Unknown — not the kind's usual
    /// strategy with a wrong download lane.
    #[test]
    fn a_kind_over_the_wrong_protocol_fails_closed() {
        assert_eq!(
            strategy_for("sysroot-bundle", "https"),
            ApplyStrategy::Unknown
        );
        assert_eq!(strategy_for("cargo-src", "https"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("binary", "pkg"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("binary", "system-pm"), ApplyStrategy::Unknown);
        assert_eq!(
            strategy_for("installer-pkg", "github-release"),
            ApplyStrategy::Unknown
        );
        assert_eq!(
            strategy_for("installer-pkg", "https"),
            ApplyStrategy::Unknown
        );
        assert_eq!(
            strategy_for("system-package", "github-release"),
            ApplyStrategy::Unknown
        );
        assert_eq!(strategy_for("app-bundle", "pkg"), ApplyStrategy::Unknown);
        assert_eq!(
            strategy_for("installer-pkg", "softwareupdate"),
            ApplyStrategy::Unknown
        );
        assert_eq!(
            strategy_for("binary", "softwareupdate"),
            ApplyStrategy::Unknown
        );
        // Spelling is exact and case-sensitive on both axes.
        assert_eq!(strategy_for("binary", "HTTPS"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("binary", "http"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("Binary", "https"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("binary", ""), ApplyStrategy::Unknown);
    }

    #[test]
    fn the_app_is_never_flipped_like_a_tool() {
        // The single most important distinction (§16.4): aterm.app does NOT take the
        // immediate symlink-flip path.
        assert_ne!(
            strategy_for("app-bundle", "github-release"),
            ApplyStrategy::Shim
        );
    }

    #[test]
    fn unknown_kind_fails_closed() {
        assert_eq!(strategy_for("", "github-release"), ApplyStrategy::Unknown);
        assert_eq!(
            strategy_for("dmg", "github-release"),
            ApplyStrategy::Unknown
        );
        assert_eq!(
            strategy_for("Binary", "github-release"),
            ApplyStrategy::Unknown
        ); // case-sensitive
        assert_eq!(
            strategy_for("sysroot", "github-release"),
            ApplyStrategy::Unknown
        );
    }
}
