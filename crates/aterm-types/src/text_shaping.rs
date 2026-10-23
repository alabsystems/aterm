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

    /// Parse a SINGLE feature token into a [`FontFeature`], or `None` if it is malformed.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse_token(token: &str) -> Option<Self> {
        let (tag_str, value) = if let Some(rest) = token.strip_prefix('+') {
            (rest, 1u32)
        } else if let Some(rest) = token.strip_prefix('-') {
            (rest, 0u32)
        } else if let Some((tag, val)) = token.split_once('=') {
            (tag, val.trim().parse::<u32>().ok()?)
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

/// Parse a whitespace-separated feature spec (e.g. `"+ss01 -calt zero"`) into a list of
/// [`FontFeature`]s.
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
    /// Admit MERGED (Cascadia N:1) ligatures.
    pub admit_collapsed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_font_features_short_tag_padded() {
        let f = parse_font_features("+cv1");
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].tag, b"cv1 ");
    }

    #[test]
    fn parse_font_features_skips_malformed() {
        let f = parse_font_features("+toolong + -calt cv01=x zero");
        assert_eq!(f.len(), 2);
        assert_eq!(&f[0].tag, b"calt");
        assert_eq!(f[0].value, 0);
        assert_eq!(&f[1].tag, b"zero");
    }
}

#[cfg(test)]
mod generated_tests {
    use super::*;

    use aterm_spec::rng::XorShift;

    fn string(rng: &mut XorShift, alphabet: &[char], max: usize) -> String {
        (0..rng.index(max + 1))
            .map(|_| *rng.pick(alphabet))
            .collect()
    }

    const ALNUM: &[char] = &['a', 'k', 'z', 'A', 'L', 'Z', '0', '5', '9'];
    const SOUP: &[char] = &['a', 'l', 'z', 'L', '0', '1', '9', ' ', '+', '-', '='];
    const ANY: &[char] = &[
        'a', 'Z', '0', ' ', '\t', '\n', '\u{a0}', '\u{3000}', '+', '-', '=', ',', '\0', '\u{7f}',
        'é', '世', '🎉', '\u{200b}',
    ];

    fn assert_well_formed(f: &FontFeature) {
        assert!(f.tag.iter().all(u8::is_ascii), "tag bytes must be ASCII");
        let first_space = f.tag.iter().position(|&b| b == b' ').unwrap_or(4);
        assert!(
            f.tag[first_space..].iter().all(|&b| b == b' '),
            "tag must be right-padded with spaces (no interior space): {:?}",
            f.tag
        );
        assert!(
            first_space >= 1,
            "tag must have at least one non-space byte"
        );
        assert!(
            f.tag[..first_space]
                .iter()
                .all(|b| !b.is_ascii_whitespace()),
            "the tag prefix must not contain whitespace"
        );
    }

    #[test]
    fn parse_never_panics_and_is_well_formed() {
        let mut rng = XorShift::new(0xF047_FEA7);
        for i in 0..1024 {
            let s = string(&mut rng, if i % 2 == 0 { ANY } else { SOUP }, 48);
            let out = parse_font_features(&s);
            assert!(out.len() <= s.split_whitespace().count(), "{s:?}");
            for f in &out {
                assert_well_formed(f);
            }
        }
    }

    #[test]
    fn affixes_and_explicit_values() {
        let mut rng = XorShift::new(0xAF_F1CE5);
        for _ in 0..256 {
            let tag = {
                let mut t = string(&mut rng, ALNUM, 3);
                t.push(*rng.pick(ALNUM));
                t
            };
            let bare = parse_font_features(&tag);
            assert_eq!(bare.len(), 1, "{tag:?}");
            assert_eq!(bare[0].value, 1);
            assert_eq!(parse_font_features(&format!("+{tag}")), bare);
            let minus = parse_font_features(&format!("-{tag}"));
            assert_eq!(minus.len(), 1);
            assert_eq!(minus[0].value, 0);
            assert_eq!(minus[0].tag, bare[0].tag);

            let alpha: String = tag.chars().filter(char::is_ascii_alphabetic).collect();
            if !alpha.is_empty() {
                let v = rng.below(u64::from(u32::MAX) + 1) as u32;
                let out = parse_font_features(&format!("{alpha}={v}"));
                assert_eq!(out.len(), 1, "{alpha}={v}");
                assert_eq!(out[0].value, v);
            }
        }
        for v in [0, 1, u32::MAX] {
            assert_eq!(parse_font_features(&format!("liga={v}"))[0].value, v);
        }
    }
}
