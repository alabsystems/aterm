// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text shaping configuration types for terminal rendering.
//!
//! Provides configuration types for:
//! - Ligature rendering mode
//! - OpenType font features
//! - Ambiguous-width character handling
//!
//! These settings flow from UI → FFI → rendering pipeline, affecting both
//! text shaping and grapheme width calculation.
//!
//! Extracted from `aterm-core::text_shaping_config` to break cross-crate
//! dependency chains (Part of #2584).

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
    #[must_use]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    /// Parse a SINGLE feature token into a [`FontFeature`], or `None` if it is
    /// malformed. Accepted forms (matching the common terminal/ghostty syntax):
    /// - `ss01` or `+ss01` — ENABLE (value `1`)
    /// - `-calt` — DISABLE (value `0`)
    /// - `cv01=2` — explicit VALUE (stylistic alternates / WezTerm syntax)
    ///
    /// The tag must be 1–4 ASCII characters (padded with spaces to 4, as
    /// OpenType requires). Anything else (empty tag, >4 chars, non-ASCII, or an
    /// unparseable `=value`) yields `None` so a typo is harmlessly skipped rather
    /// than poisoning the whole feature list.
    #[must_use]
    // Skip: `str::parse`/split absent std bodies; fail-closed.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse_token(token: &str) -> Option<Self> {
        let (tag_str, value) = if let Some(rest) = token.strip_prefix('+') {
            (rest, 1u32)
        } else if let Some(rest) = token.strip_prefix('-') {
            (rest, 0u32)
        } else if let Some((tag, val)) = token.split_once('=') {
            (tag, val.trim().parse::<u32>().ok()?)
        } else {
            // A bare tag enables the feature (`ss01` == `+ss01`).
            (token, 1u32)
        };
        let tag_str = tag_str.trim();
        // A valid tag is 1–4 ASCII bytes with NO interior whitespace (so the padded
        // tag is always `<chars><spaces>` — see the well-formedness property test).
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

/// Parse a whitespace-separated feature spec (e.g. `"+ss01 -calt zero"`) into a
/// list of [`FontFeature`]s. Each token is parsed by [`FontFeature::parse_token`];
/// malformed tokens are skipped. An empty/blank spec yields an empty list, so an
/// unset config is byte-identical to the pre-feature renderer.
#[must_use]
// Skip: split/collect absent std bodies; fail-closed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_font_features(spec: &str) -> Vec<FontFeature> {
    // `.take(256)` bounds the `collect` allocation for the Trust gate with a
    // literal element count. Identity on every real input: the spec is one
    // hand-written config line of 4-char OpenType tags, and no font exposes
    // anywhere near 256 features (a few dozen is the practical ceiling), so
    // the clamp never fires and the parsed list is unchanged.
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
    /// Admit MERGED (Cascadia N:1) ligatures: a run whose OpenType shaping
    /// collapses several cells into ONE wide glyph (Cascadia Code's convention),
    /// which the renderer slices into per-cell tiles (M4). DEFAULT `false`, so the
    /// gate stays on the proven 1:1 "spacer convention" every Fira/JetBrains-style
    /// font uses and the output is byte-identical to the pre-M4 renderer. Only the
    /// Cascadia-style collapse is affected; a `false`→`true` flip is a no-op on a
    /// 1:1 font. Wired from the GUI config key `merged_ligatures`.
    pub admit_collapsed: bool,
}

impl TextShapingConfig {
    /// Get display width for ambiguous characters (1 or 2).
    #[inline]
    #[must_use]
    pub const fn ambiguous_char_width(&self) -> usize {
        match self.ambiguous_width {
            AmbiguousWidth::Single => 1,
            AmbiguousWidth::Double => 2,
        }
    }

    /// Check if ligatures should be disabled for a glyph run given cursor position.
    ///
    /// Parameters:
    /// - `cursor`: Optional (row, col) tuple. None if cursor not visible.
    /// - `shaping_row`: The row being shaped (0-indexed from viewport top).
    /// - `glyph_start_col`: Start column of the ligature glyph run.
    /// - `glyph_end_col`: End column (exclusive) of the ligature glyph run.
    ///
    /// Returns true if:
    /// - `ligature_mode == Disabled`, OR
    /// - `ligature_mode == CursorDisabled` AND cursor is ON this row AND within glyph range
    #[inline]
    #[must_use]
    pub fn should_disable_ligatures(
        &self,
        cursor: Option<(usize, usize)>,
        shaping_row: usize,
        glyph_start_col: usize,
        glyph_end_col: usize,
    ) -> bool {
        match self.ligature_mode {
            LigatureMode::Enabled => false,
            LigatureMode::Disabled => true,
            LigatureMode::CursorDisabled => {
                if let Some((cursor_row, cursor_col)) = cursor {
                    cursor_row == shaping_row
                        && cursor_col >= glyph_start_col
                        && cursor_col < glyph_end_col
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_width_default_is_single() {
        assert_eq!(AmbiguousWidth::default(), AmbiguousWidth::Single);
    }

    #[test]
    fn ligature_mode_default_is_enabled() {
        assert_eq!(LigatureMode::default(), LigatureMode::Enabled);
    }

    #[test]
    fn text_shaping_config_default() {
        let cfg = TextShapingConfig::default();
        assert_eq!(cfg.ligature_mode, LigatureMode::Enabled);
        assert_eq!(cfg.ambiguous_width, AmbiguousWidth::Single);
        assert!(cfg.font_features.is_empty());
    }

    #[test]
    fn ambiguous_char_width_single() {
        let cfg = TextShapingConfig::default();
        assert_eq!(cfg.ambiguous_char_width(), 1);
    }

    #[test]
    fn ambiguous_char_width_double() {
        let cfg = TextShapingConfig {
            ambiguous_width: AmbiguousWidth::Double,
            ..Default::default()
        };
        assert_eq!(cfg.ambiguous_char_width(), 2);
    }

    #[test]
    fn should_disable_ligatures_enabled_mode() {
        let cfg = TextShapingConfig::default();
        assert!(!cfg.should_disable_ligatures(Some((0, 5)), 0, 3, 8));
    }

    #[test]
    fn should_disable_ligatures_disabled_mode() {
        let cfg = TextShapingConfig {
            ligature_mode: LigatureMode::Disabled,
            ..Default::default()
        };
        assert!(cfg.should_disable_ligatures(None, 0, 0, 10));
    }

    #[test]
    fn should_disable_ligatures_cursor_disabled_mode() {
        let cfg = TextShapingConfig {
            ligature_mode: LigatureMode::CursorDisabled,
            ..Default::default()
        };
        // Cursor on same row, within glyph range
        assert!(cfg.should_disable_ligatures(Some((0, 5)), 0, 3, 8));
        // Cursor on different row
        assert!(!cfg.should_disable_ligatures(Some((1, 5)), 0, 3, 8));
        // Cursor before glyph range
        assert!(!cfg.should_disable_ligatures(Some((0, 2)), 0, 3, 8));
        // Cursor after glyph range
        assert!(!cfg.should_disable_ligatures(Some((0, 8)), 0, 3, 8));
        // No cursor
        assert!(!cfg.should_disable_ligatures(None, 0, 3, 8));
    }

    #[test]
    fn font_feature_new() {
        let f = FontFeature::new(*b"calt", 1);
        assert_eq!(f.tag, *b"calt");
        assert_eq!(f.value, 1);
    }

    #[test]
    fn parse_font_features_plus_minus() {
        let f = parse_font_features("+ss01 -calt");
        assert_eq!(f.len(), 2);
        assert_eq!(&f[0].tag, b"ss01");
        assert_eq!(f[0].value, 1);
        assert_eq!(&f[1].tag, b"calt");
        assert_eq!(f[1].value, 0);
    }

    #[test]
    fn parse_font_features_bare_tag_enables() {
        // ghostty-style: a bare tag turns the feature ON (value 1).
        let f = parse_font_features("ss01 zero");
        assert_eq!(f.len(), 2);
        assert_eq!(&f[0].tag, b"ss01");
        assert_eq!(f[0].value, 1);
        assert_eq!(&f[1].tag, b"zero");
        assert_eq!(f[1].value, 1);
    }

    #[test]
    fn parse_font_features_explicit_value() {
        let f = parse_font_features("cv01=2");
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].tag, b"cv01");
        assert_eq!(f[0].value, 2);
    }

    #[test]
    fn parse_font_features_short_tag_padded() {
        let f = parse_font_features("+cv1");
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].tag, b"cv1 ");
    }

    #[test]
    fn parse_font_features_skips_malformed() {
        // Too long, empty tag, and an unparseable explicit value are all dropped;
        // a valid neighbour still parses.
        let f = parse_font_features("+toolong + -calt cv01=x zero");
        assert_eq!(f.len(), 2);
        assert_eq!(&f[0].tag, b"calt");
        assert_eq!(f[0].value, 0);
        assert_eq!(&f[1].tag, b"zero");
    }

    #[test]
    fn parse_font_features_empty() {
        assert!(parse_font_features("").is_empty());
        assert!(parse_font_features("   ").is_empty());
    }
}

/// Property-based verification of the feature parser: the structural invariants
/// the rest of the shaping pipeline relies on must hold for ARBITRARY input, not
/// just the hand-picked cases above. (proptest is the right tool here rather than
/// Kani — the input is an unbounded UTF-8 string; the bounded numeric/width
/// invariants that ARE Kani-tractable live in `aterm-grapheme`'s `config_proofs`.)
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Every `FontFeature` the parser emits is WELL-FORMED: the 4-byte tag is all
    /// ASCII, right-padded with spaces (`<1..=4 chars><spaces>`), and never empty —
    /// exactly the shape `rustybuzz` requires.
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

    proptest! {
        /// PANIC-FREEDOM + well-formedness over ANY input string, and never more
        /// features than whitespace-separated tokens (each token yields ≤1 feature).
        #[test]
        fn parse_never_panics_and_is_well_formed(s in ".*") {
            let out = parse_font_features(&s);
            prop_assert!(out.len() <= s.split_whitespace().count());
            for f in &out {
                assert_well_formed(f);
            }
        }

        /// The realistic config alphabet (tags, affixes, `=`, spaces) — adversarial
        /// soup must still only ever produce well-formed features.
        #[test]
        fn token_soup_stays_well_formed(s in "[a-zA-Z0-9 +\\-=]{0,48}") {
            for f in &parse_font_features(&s) {
                assert_well_formed(f);
            }
        }

        /// Affix semantics: a bare tag enables (value 1), `+tag` is identical, and
        /// `-tag` flips to 0 with the same tag bytes — for any 1–4 char alnum tag.
        #[test]
        fn affixes_set_value(tag in "[a-zA-Z0-9]{1,4}") {
            let bare = parse_font_features(&tag);
            prop_assert_eq!(bare.len(), 1);
            prop_assert_eq!(bare[0].value, 1);
            prop_assert_eq!(parse_font_features(&format!("+{tag}")), bare.clone());
            let minus = parse_font_features(&format!("-{tag}"));
            prop_assert_eq!(minus.len(), 1);
            prop_assert_eq!(minus[0].value, 0);
            prop_assert_eq!(minus[0].tag, bare[0].tag);
        }

        /// `tag=value` carries the exact parsed value for ANY u32.
        #[test]
        fn explicit_value_roundtrips(tag in "[a-zA-Z]{1,4}", v in any::<u32>()) {
            let out = parse_font_features(&format!("{tag}={v}"));
            prop_assert_eq!(out.len(), 1);
            prop_assert_eq!(out[0].value, v);
        }
    }
}
