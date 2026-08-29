// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The host's OSC 8 hyperlink decision: WHICH URI SCHEMES the grid may
//! carry (orca deep-links §7, #4384).
//!
//! # What decides whether an OSC 8 hyperlink is accepted
//!
//! `OSC 8 ; params ; URI ST` sets the `current_hyperlink` field that
//! subsequent printable characters are tagged with, and the host's UI
//! renders those cells as clickable. Acceptance is decided by the URI
//! ALONE, in [`super::handler_osc`]: the byte cap, the control-character
//! scan, the BiDi-override refusal (CVE-2021-42574), and the scheme
//! allowlist — the built-in safe set plus whatever EXTRA schemes the
//! embedding host minted through [`HyperlinkAuth::authorize_scheme`].
//!
//! There is deliberately NO "does this terminal accept OSC 8 at all"
//! switch. OSC 8 has been a universally supported terminal feature
//! since xterm's 2017 patch and every host that embeds this engine
//! wants it on; a gate whose only setting is "on" is not a decision,
//! it is scaffolding that invites a reader to believe someone chose.
//! The scheme set below is the hyperlink decision a host actually
//! makes, and it has callers (`aterm-wasm`, `aterm-gpu-web`).
//!
//! # Tagging a cell is not following a link
//!
//! The boundary that matters to a person is at CLICK time, in the host,
//! and it is not in this crate: `aterm-gui`'s `is_safe_url` re-checks
//! the scheme at the point of action, and every URL that ARRIVES FROM THE
//! GRID or from any other untrusted place is admitted by it before
//! `open_url_external` sees it. The launcher has two further callers, and
//! both hand it a URL this program wrote — a compile-time help address,
//! and a compile-time form address carrying a percent-encoded comment —
//! so neither can put a scheme on the wire that a person did not
//! already trust. Meanwhile `aterm_gui::link_target`'s caption
//! band discloses the destination — percent-encoded, host-anchored —
//! because OSC 8 lets the visible text and the target be unrelated and
//! no engine-side admission rule can close that gap. Keep the two ends
//! distinct: this module decides which URIs the GRID may carry; the
//! host decides what a click may OPEN.

// ---------------------------------------------------------------------------
// Host-minted extra schemes (orca deep-links §7).
// ---------------------------------------------------------------------------

/// Hard cap on host-minted extra schemes. A host mints one or two app schemes
/// (`orca`); a large set would recreate the pre-#7919 open-ended surface.
/// Abstract twin: `Cap` in `aterm_spec::derive::hyperlink_scheme_cap_model`.
pub(crate) const MAX_EXTRA_SCHEMES: usize = 4;

/// Hard cap on one extra scheme's length. Real schemes are short; this bounds
/// the per-URI `eq_ignore_ascii_case` comparisons in the OSC-8 hot path.
pub(crate) const MAX_EXTRA_SCHEME_LEN: usize = 32;

/// Schemes refused EVEN WHEN THE HOST ASKS — execution/exfiltration primitives
/// no terminal hyperlink should ever carry. Matching is case-insensitive and
/// also refuses `+`/`-`/`.`-suffixed spellings (`javascript.` etc.), so the
/// never-allow decision cannot be evaded by RFC 3986 scheme-charset suffixes
/// that some URL parsers normalize away.
const NEVER_ALLOW_SCHEMES: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

/// Whether `scheme` (WITHOUT the trailing `:`) has the RFC 3986 §3.1 shape:
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. Shared by the OSC-8 gate's
/// URI parse and [`HyperlinkAuth::authorize_scheme`]'s mint validation so the
/// two can never diverge (a minted scheme unreachable by the gate, or vice
/// versa — the smuggling seam).
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

/// The host-minted extra-scheme set for OSC 8 URIs (deep-links §7).
///
/// Lives on [`super::Terminal`] and is forwarded through
/// [`super::TerminalHandler`] so the OSC 8 handler can READ it, while
/// [`authorize_scheme`][Self::authorize_scheme] /
/// [`revoke_scheme`][Self::revoke_scheme] are reachable only through the
/// host-facing `Terminal` API — a parser handler cannot mint itself a
/// scheme.
#[derive(Debug)]
pub(crate) struct HyperlinkAuth {
    /// Host-minted extra schemes accepted IN ADDITION to the hardcoded safe
    /// allowlist. Stored ASCII-lowercased; bounded by [`MAX_EXTRA_SCHEMES`].
    /// Mutations only via [`authorize_scheme`][Self::authorize_scheme] /
    /// [`revoke_scheme`][Self::revoke_scheme] (host API — handlers cannot
    /// mint schemes for themselves). Abstract twin:
    /// `hyperlink_scheme_cap_model` (registered in aterm-spec).
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

    /// Mint `scheme` into the extra allowlist (deep-links §7). Returns `false`
    /// — refusing, state unchanged — when the scheme is over-long, not RFC 3986
    /// scheme-shaped, in the never-allow set, or the set is full; `true` when
    /// stored (idempotently: re-minting a live scheme is `true` and does not
    /// consume a slot). Stored ASCII-lowercased; the OSC-8 gate compares
    /// case-insensitively, so acceptance is case-blind like the safe list.
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

    // ---- host-minted extra schemes (deep-links §7) ----------------------

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
