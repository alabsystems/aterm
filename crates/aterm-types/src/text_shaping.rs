// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text shaping configuration types for terminal rendering.

/// Ambiguous-width character handling.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(u8)]
pub enum AmbiguousWidth {
    /// Single-width (default).
    #[default]
    Single = 0,
    /// Double-width (CJK mode).
    Double = 1,
}

/// Ligature rendering mode.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(u8)]
pub enum LigatureMode {
    /// Always render ligatures.
    #[default]
    Enabled = 0,
    /// Disable ligatures at cursor position.
    CursorDisabled = 1,
    /// Never render ligatures.
    Disabled = 2,
}

/// OpenType font feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(C)]
pub struct FontFeature {
    /// 4-byte OpenType tag (e.g., b"calt", b"ss01").
    pub tag: [u8; 4],
    /// Feature value: 0 = disabled, 1 = enabled, >1 for stylistic alternates.
    pub value: u32,
}

impl FontFeature {
    /// Create a new font feature.
    #[inline]
    #[must_use]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    /// Parse a single feature token into a [`FontFeature`], or `None` if malformed.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse_token(token: &str) -> Option<Self> {
        let (tag_str, value) = if let Some(rest) = token.strip_prefix('+') {
            (rest, 1u32)
        } else if let Some(rest) = token.strip_prefix('-') {
            (rest, 0u32)
        } else if let Some((tag, val)) = token.split_once('=') {
            let val = val.trim();
            if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (tag, val.parse::<u32>().ok()?)
        } else {
            (token, 1u32)
        };
        let tag_str = tag_str.trim();
        if tag_str.is_empty()
            || tag_str.len() > 4
            || !tag_str.is_ascii()
            || tag_str.bytes().any(|b| b.is_ascii_whitespace())
        {
            return None;
        }
        let mut tag = [b' '; 4];
        tag[..tag_str.len()].copy_from_slice(tag_str.as_bytes());
        Some(Self::new(tag, value))
    }
}

/// Parse a whitespace-separated feature spec (e.g. `"+ss01 -calt zero"`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_font_features(spec: &str) -> Vec<FontFeature> {
    spec.split_whitespace()
        .filter_map(FontFeature::parse_token)
        .take(256)
        .collect()
}

/// Per-font feature set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontFeatureSet {
    /// Font identifier.
    pub font_id: u32,
    /// Feature overrides.
    pub features: Vec<FontFeature>,
}

/// Text shaping configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextShapingConfig {
    /// Ligature rendering mode.
    pub ligature_mode: LigatureMode,
    /// Ambiguous-width character handling.
    pub ambiguous_width: AmbiguousWidth,
    /// Per-font OpenType features.
    pub font_features: Vec<FontFeatureSet>,
    /// Admit merged (Cascadia N:1) ligatures.
    pub admit_collapsed: bool,
}

#[cfg(test)]
#[path = "text_shaping_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "text_shaping_generated_tests.rs"]
mod generated_tests;
