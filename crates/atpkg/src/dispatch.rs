// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-member apply dispatch (§16.4 / §10.1) — the decision of *how* a staged, verified
//! artifact is made live, keyed on its manifest `kind`.
//!
//! The design deliberately does **not** apply every member the same way (§16.4): a CLI
//! tool flips immediately (a POSIX `bin/` symlink), `trust`/`trust-mc` need a sysroot
//! relocation (§10.1), and `aterm.app`
//! itself is **not** a tarball at all — it is a notarized DMG staged for the self-swap on
//! next launch (the two-anchor gate, §16.2/§16.4), never the immediate symlink flip.
//! [`strategy_for`] is that pure mapping; an unknown kind is **fail-closed** ([`Unknown`])
//! so a member the client cannot install is never silently treated as a plain binary.
//!
//! [`Unknown`]: ApplyStrategy::Unknown

/// How a staged artifact is made live, dispatched on its `[[artifact]].kind` (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// A plain binary / from-source build: symlink-flip the channel `current` and shim each
    /// exposed binary into `bin/` (§10). The immediate, tool path.
    Shim,
    /// A `trust`/`trust-mc` sysroot bundle: extract, relocate the (dangling) sysroot
    /// toolchain link to its resolved toolchain, gated
    /// on the four-component nightly being installed (§10.1). Its `exposes` still shim.
    SysrootBundle,
    /// The `aterm.app` DMG: NOT extracted as a tarball — staged for the **notarized
    /// self-swap on next launch** (`renamex_np(RENAME_SWAP)` + re-exec), AND-gated by the
    /// two anchors (notarization + the signed-index sha256), never the immediate flip
    /// (§16.2/§16.4). A different apply *topology*, dispatched here so it can never be
    /// symlink-flipped like a tool.
    AppBundle,
    /// An unrecognized `kind` — fail closed: the client refuses to apply an artifact it
    /// does not know how to install (never default to [`Shim`](ApplyStrategy::Shim)).
    Unknown,
}

/// Map a manifest artifact `kind` to its [`ApplyStrategy`]. Fail-closed on anything
/// unrecognized so a future/garbled kind is never mis-applied as a plain binary.
#[must_use]
pub fn strategy_for(kind: &str) -> ApplyStrategy {
    match kind {
        "binary" | "cargo-src" => ApplyStrategy::Shim,
        "sysroot-bundle" => ApplyStrategy::SysrootBundle,
        "app-bundle" => ApplyStrategy::AppBundle,
        _ => ApplyStrategy::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kinds_map_to_their_strategy() {
        assert_eq!(strategy_for("binary"), ApplyStrategy::Shim);
        assert_eq!(strategy_for("cargo-src"), ApplyStrategy::Shim);
        assert_eq!(strategy_for("sysroot-bundle"), ApplyStrategy::SysrootBundle);
        assert_eq!(strategy_for("app-bundle"), ApplyStrategy::AppBundle);
    }

    #[test]
    fn the_app_is_never_flipped_like_a_tool() {
        // The single most important distinction (§16.4): aterm.app does NOT take the
        // immediate symlink-flip path.
        assert_ne!(strategy_for("app-bundle"), ApplyStrategy::Shim);
    }

    #[test]
    fn unknown_kind_fails_closed() {
        assert_eq!(strategy_for(""), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("dmg"), ApplyStrategy::Unknown);
        assert_eq!(strategy_for("Binary"), ApplyStrategy::Unknown); // case-sensitive
        assert_eq!(strategy_for("sysroot"), ApplyStrategy::Unknown);
    }
}
