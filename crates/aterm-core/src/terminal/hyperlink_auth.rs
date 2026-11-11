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
#[derive(Debug, Clone)]
pub(crate) struct HyperlinkAuth {
    extra_schemes: Vec<Box<str>>,
}

impl Default for HyperlinkAuth {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl super::checkpoint_heap::Heap for HyperlinkAuth {
    fn heap(&self) -> usize {
        let Self { extra_schemes } = self;
        extra_schemes.heap()
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
#[path = "hyperlink_auth_tests.rs"]
mod tests;
