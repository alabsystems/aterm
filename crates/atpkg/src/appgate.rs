// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The two-anchor app apply gate (§16.2) — when a staged `aterm.app` DMG may be applied,
//! once aterm is a named index member.
//!
//! The gate is a strict **AND**, never an OR, and **notarization is always required**:
//! Apple notarization (`codesign --verify` + `PINNED_TEAM_ID` + `spctl`, the existing
//! `aterm-update` path) AND a strictly-newer monotonic `build_number` are *always*
//! enforced. The index conjunct — the signed-index DMG `sha256`, the `min_build` floor,
//! and `yanked` — is **added only when the index is fresh**, never a hard precondition an
//! adversary can switch off by staling the index. That asymmetry is deliberate
//! ([`AppIndexGate`]): a network/mirror adversary must not be able to *block* a notarized
//! emergency app fix that notarization + monotonic alone would apply today.
//!
//! Crucially, because notarization is checked **unconditionally**, the Ed25519/index path
//! can only ever *add* constraints — it can never let a build apply that **skips** the
//! notarization gate. A weaker (OR) wiring is forbidden by construction; [`app_apply_allowed`]
//! encodes the AND so a test pins it.

/// The index conjunct for the app, present **only when the index is fresh** (`valid_until`
/// not lapsed). When the index is stale/absent the caller passes `None` and the gate falls
/// back to notarization + monotonic alone (the existing updater behavior).
#[derive(Debug, Clone)]
pub struct AppIndexGate {
    /// Whether the staged DMG's sha256 equals the signed-index artifact `sha256`.
    pub sha256_match: bool,
    /// The channel `min_build` floor the app build must clear.
    pub min_build: u64,
    /// Whether this exact app build is on the channel's `yanked` list.
    pub yanked: bool,
}

/// Whether a staged app `build` may be applied. `notarized` is the result of the existing
/// notarization gate; `running_build` is the build currently installed (the monotonic
/// downgrade floor); `index` is the fresh-index conjunct or `None` when the index is
/// stale/absent.
///
/// Always required: `notarized` AND `build > running_build`. When `index` is `Some` (fresh):
/// also `sha256_match` AND `build >= min_build` AND not `yanked`. Fail-closed throughout.
#[must_use]
pub fn app_apply_allowed(
    notarized: bool,
    build: u64,
    running_build: u64,
    index: Option<&AppIndexGate>,
) -> bool {
    // Unconditional anchors — the index path can only ADD to these, never bypass them.
    if !notarized {
        return false;
    }
    if build <= running_build {
        return false; // strictly-newer monotonic gate (no downgrade/replay)
    }
    // The index conjunct, only when the index is fresh.
    if let Some(g) = index
        && (!g.sha256_match || build < g.min_build || g.yanked)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(sha256_match: bool, min_build: u64, yanked: bool) -> AppIndexGate {
        AppIndexGate {
            sha256_match,
            min_build,
            yanked,
        }
    }

    // Notarization is ALWAYS required — no index state can substitute for it.
    #[test]
    fn notarization_is_unconditional() {
        // Even with a perfect fresh index, an un-notarized build is refused.
        assert!(!app_apply_allowed(
            false,
            1235,
            1234,
            Some(&fresh(true, 1200, false))
        ));
        // And with no index at all.
        assert!(!app_apply_allowed(false, 1235, 1234, None));
    }

    // The monotonic downgrade gate is always enforced.
    #[test]
    fn never_applies_a_non_newer_build() {
        assert!(!app_apply_allowed(true, 1234, 1234, None)); // equal
        assert!(!app_apply_allowed(true, 1230, 1234, None)); // older
        assert!(app_apply_allowed(true, 1235, 1234, None)); // strictly newer
    }

    // Stale/absent index ⇒ notarization + monotonic suffice (emergency notarized fix path).
    #[test]
    fn stale_index_allows_notarized_monotonic_emergency_fix() {
        assert!(app_apply_allowed(true, 1235, 1234, None));
    }

    // Fresh index ADDS the conjunct: sha256 must match, build must clear min_build, not yanked.
    #[test]
    fn fresh_index_adds_conjunct() {
        // All pass.
        assert!(app_apply_allowed(
            true,
            1235,
            1234,
            Some(&fresh(true, 1200, false))
        ));
        // sha256 mismatch ⇒ refused.
        assert!(!app_apply_allowed(
            true,
            1235,
            1234,
            Some(&fresh(false, 1200, false))
        ));
        // below min_build ⇒ refused.
        assert!(!app_apply_allowed(
            true,
            1235,
            1234,
            Some(&fresh(true, 9999, false))
        ));
        // yanked ⇒ refused.
        assert!(!app_apply_allowed(
            true,
            1235,
            1234,
            Some(&fresh(true, 1200, true))
        ));
    }

    // The documented residual: a build the index would yank still applies when the index is
    // STALE (the yank is only enforceable within the freshness window, §16.2). Notarization
    // + monotonic still bound forge/downgrade.
    #[test]
    fn yank_is_not_enforced_on_a_stale_index() {
        assert!(
            app_apply_allowed(true, 1235, 1234, None),
            "a stale index cannot enforce a yank — the documented §16.2 residual"
        );
    }
}
