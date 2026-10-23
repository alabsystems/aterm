// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Host-authorized extra URI schemes for OSC 8 hyperlinks.

/// Maximum number of host-authorized extra URI schemes.
pub(crate) const MAX_EXTRA_SCHEMES: usize = 4;

/// Maximum byte length of a single extra URI scheme.
pub(crate) const MAX_EXTRA_SCHEME_LEN: usize = 32;

const NEVER_ALLOW_SCHEMES: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

/// Returns whether `scheme` matches RFC 3986 §3.1 `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
#[must_use]
#[inline]
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
#[inline]
fn is_never_allowed(lowered: &str) -> bool {
    let trimmed = lowered.trim_end_matches(['+', '-', '.']);
    NEVER_ALLOW_SCHEMES.contains(&trimmed)
}

/// Host-authorized extra URI scheme set for OSC 8 hyperlinks.
#[derive(Debug)]
pub(crate) struct HyperlinkAuth {
    extra_schemes: Vec<Box<str>>,
}

impl Default for HyperlinkAuth {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl HyperlinkAuth {
    /// Creates an empty extra-scheme set.
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            extra_schemes: Vec::new(),
        }
    }

    /// Authorizes `scheme` (lowercased), returning `false` if invalid, forbidden, or full.
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

    /// Removes `scheme` from the extra allowlist (case-insensitive).
    #[inline]
    pub(crate) fn revoke_scheme(&mut self, scheme: &str) {
        self.extra_schemes
            .retain(|s| !s.eq_ignore_ascii_case(scheme));
    }

    /// Returns the active host-authorized extra schemes (lowercased).
    #[inline]
    #[must_use]
    pub(crate) fn extra_schemes(&self) -> &[Box<str>] {
        &self.extra_schemes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_scheme_stores_lowercased_and_is_idempotent() {
        let mut auth = HyperlinkAuth::new();
        assert!(auth.authorize_scheme("Orca"));
        assert_eq!(auth.extra_schemes().len(), 1);
        assert_eq!(&*auth.extra_schemes()[0], "orca");
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
