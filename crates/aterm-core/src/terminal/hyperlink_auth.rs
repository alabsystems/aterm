// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The host's OSC 8 hyperlink decision: which URI schemes the grid may carry.
//!
//! `OSC 8 ; params ; URI ST` sets the `current_hyperlink` that subsequent
//! printable characters are tagged with. Acceptance depends on the URI alone,
//! in [`super::handler_osc`]: the byte cap, the control-character scan, the
//! BiDi-override refusal (CVE-2021-42574), and the scheme allowlist (the
//! built-in safe set plus extra schemes the host minted through
//! [`HyperlinkAuth::authorize_scheme`]). There is no on/off switch for OSC 8.
//!
//! Tagging a cell is not following a link. The click-time boundary is in the
//! host: `aterm-gui`'s `is_safe_url` re-checks the scheme before
//! `open_url_external`, and `aterm_gui::link_target`'s caption discloses the
//! destination, since OSC 8 text and target can be unrelated. This module
//! decides which URIs the grid may carry; the host decides what a click opens.

// ---------------------------------------------------------------------------
// Host-minted extra schemes.
// ---------------------------------------------------------------------------

/// Hard cap on host-minted extra schemes; a host mints one or two app schemes
/// (`orca`). Abstract twin: `Cap` in `aterm_spec::derive::hyperlink_scheme_cap_model`.
pub(crate) const MAX_EXTRA_SCHEMES: usize = 4;

/// Hard cap on one extra scheme's length. Real schemes are short; this bounds
/// the per-URI `eq_ignore_ascii_case` comparisons in the OSC-8 hot path.
pub(crate) const MAX_EXTRA_SCHEME_LEN: usize = 32;

/// Schemes refused even when the host asks — execution/exfiltration primitives
/// no terminal hyperlink should ever carry. Matching is case-insensitive and
/// also refuses `+`/`-`/`.`-suffixed spellings (`javascript.` etc.), so the
/// never-allow decision cannot be evaded by RFC 3986 scheme-charset suffixes
/// that some URL parsers normalize away.
const NEVER_ALLOW_SCHEMES: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

/// Whether `scheme` (without the trailing `:`) has the RFC 3986 §3.1 shape
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. Shared by the OSC 8 URI parse
/// and [`HyperlinkAuth::authorize_scheme`] so a minted scheme is always
/// reachable by the gate and vice versa.
#[must_use]
pub(crate) fn is_rfc3986_scheme_shape(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether `lowered` (already ASCII-lowercased) hits the never-allow set,
/// including trailing scheme-charset padding (`javascript.`, `data+`).
fn is_never_allowed(lowered: &str) -> bool {
    let trimmed = lowered.trim_end_matches(['+', '-', '.']);
    NEVER_ALLOW_SCHEMES.contains(&trimmed)
}

// ---------------------------------------------------------------------------
// Authorization state.
// ---------------------------------------------------------------------------

/// The host-minted extra-scheme set for OSC 8 URIs.
///
/// Forwarded to [`super::TerminalHandler`] so the OSC 8 handler can read it;
/// [`authorize_scheme`][Self::authorize_scheme] /
/// [`revoke_scheme`][Self::revoke_scheme] are reachable only through the
/// host-facing `Terminal` API.
#[derive(Debug)]
pub(crate) struct HyperlinkAuth {
    /// Extra schemes accepted beyond the built-in safe allowlist, stored
    /// ASCII-lowercased and bounded by [`MAX_EXTRA_SCHEMES`]. Abstract twin:
    /// `hyperlink_scheme_cap_model` (aterm-spec).
    extra_schemes: Vec<Box<str>>,
}

impl Default for HyperlinkAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperlinkAuth {
    /// Construct the empty scheme set: OSC 8 URIs are admitted on the
    /// built-in safe allowlist alone until a host mints an extra scheme.
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            extra_schemes: Vec::new(),
        }
    }

    /// Mint `scheme` into the extra allowlist. Returns `false`, state
    /// unchanged, when the scheme is over-long, not RFC 3986 scheme-shaped, in
    /// the never-allow set, or the set is full; `true` when stored (re-minting
    /// a live scheme is `true` and takes no slot). Stored lowercased; matching
    /// is case-insensitive.
    pub(crate) fn authorize_scheme(&mut self, scheme: &str) -> bool {
        if scheme.len() > MAX_EXTRA_SCHEME_LEN || !is_rfc3986_scheme_shape(scheme) {
            return false;
        }
        let lowered = scheme.to_ascii_lowercase();
        if is_never_allowed(&lowered) {
            return false;
        }
        if self.extra_schemes.iter().any(|s| **s == *lowered) {
            return true;
        }
        if self.extra_schemes.len() >= MAX_EXTRA_SCHEMES {
            return false;
        }
        self.extra_schemes.push(lowered.into_boxed_str());
        true
    }

    /// Remove `scheme` from the extra allowlist (case-insensitive), restoring
    /// the default posture for it. Removing an absent scheme is a no-op.
    pub(crate) fn revoke_scheme(&mut self, scheme: &str) {
        self.extra_schemes
            .retain(|s| !s.eq_ignore_ascii_case(scheme));
    }

    /// The live host-minted extra schemes (lowercased), for the OSC-8 gate and
    /// host introspection.
    #[inline]
    #[must_use]
    pub(crate) fn extra_schemes(&self) -> &[Box<str>] {
        &self.extra_schemes
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_scheme_set_is_empty_so_only_the_builtin_safe_list_admits() {
        let auth = HyperlinkAuth::new();
        assert!(auth.extra_schemes().is_empty());
        assert!(HyperlinkAuth::default().extra_schemes().is_empty());
    }

    // ---- host-minted extra schemes ----------------------

    #[test]
    fn authorize_scheme_stores_lowercased_and_is_idempotent() {
        let mut auth = HyperlinkAuth::new();
        assert!(auth.authorize_scheme("Orca"));
        assert_eq!(auth.extra_schemes().len(), 1);
        assert_eq!(&*auth.extra_schemes()[0], "orca");
        // Re-minting (any case) succeeds without consuming a slot.
        assert!(auth.authorize_scheme("ORCA"));
        assert_eq!(auth.extra_schemes().len(), 1);
    }

    #[test]
    fn authorize_scheme_bounded_at_max_extra_schemes() {
        let mut auth = HyperlinkAuth::new();
        for s in ["a1", "b2", "c3", "d4"] {
            assert!(auth.authorize_scheme(s));
        }
        assert!(
            !auth.authorize_scheme("e5"),
            "slot {MAX_EXTRA_SCHEMES}+1 must be refused"
        );
        assert_eq!(auth.extra_schemes().len(), MAX_EXTRA_SCHEMES);
        // A refused mint changes nothing; revoking frees the slot again.
        auth.revoke_scheme("a1");
        assert!(auth.authorize_scheme("e5"));
    }

    #[test]
    fn authorize_scheme_refuses_never_allow_incl_case_and_suffix_evasion() {
        let mut auth = HyperlinkAuth::new();
        for s in [
            "javascript",
            "JavaScript",
            "data",
            "file",
            "vbscript",
            "about",
            "blob",
            "javascript.",
            "javascript+",
            "DATA-",
        ] {
            assert!(!auth.authorize_scheme(s), "never-allow must refuse {s:?}");
        }
        assert!(auth.extra_schemes().is_empty());
        // A genuinely different scheme sharing the prefix is not blocked.
        assert!(auth.authorize_scheme("javascriptx"));
    }

    #[test]
    fn authorize_scheme_refuses_malformed_shapes_and_overlength() {
        let mut auth = HyperlinkAuth::new();
        for s in ["", "1orca", "+orca", "or ca", "orca\t", "or%3aca", "orc:a"] {
            assert!(
                !auth.authorize_scheme(s),
                "malformed shape must refuse {s:?}"
            );
        }
        assert!(!auth.authorize_scheme(&"a".repeat(MAX_EXTRA_SCHEME_LEN + 1)));
        assert!(auth.extra_schemes().is_empty());
        // `+`/`-`/`.` are legal NON-LEADING scheme chars (RFC 3986).
        assert!(auth.authorize_scheme("web+orca"));
    }

    #[test]
    fn revoke_scheme_is_case_insensitive_and_noop_when_absent() {
        let mut auth = HyperlinkAuth::new();
        assert!(auth.authorize_scheme("orca"));
        auth.revoke_scheme("nonexistent");
        assert_eq!(auth.extra_schemes().len(), 1);
        auth.revoke_scheme("ORCA");
        assert!(auth.extra_schemes().is_empty());
    }
}
