// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A theme's typography (DTP §3.1).

use crate::scheme::{THEME_KEYS, ThemeKey, ThemeKeyGroup};

/// Longest font family name a theme may carry, in chars.
pub const MAX_THEME_FONT_NAME_CHARS: usize = 128;
/// Most entries a theme's `fallback_fonts` list may carry.
pub const MAX_THEME_FONT_LIST: usize = 8;
/// Most feature specs a theme's `font_features` may carry.
pub const MAX_THEME_FEATURES: usize = 16;
/// The theme format's schema version this aterm writes and reads in full.
pub const THEME_SCHEMA_VERSION: u16 = 1;
/// Most unknown key names a newer-schema file's admission records (the count is kept in full).
pub const MAX_SKIPPED_THEME_KEYS: usize = 16;
/// Longest recorded unknown key name, in bytes (cut at a char boundary).
pub const MAX_SKIPPED_THEME_KEY_BYTES: usize = 64;

/// A theme's typography: every field optional; `None` means "don't touch it".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeTypography {
    /// The regular face's family.
    pub font_family: Option<String>,
    /// The bold face's family.
    pub font_family_bold: Option<String>,
    /// The italic face's family.
    pub font_family_italic: Option<String>,
    /// The bold-italic face's family.
    pub font_family_bold_italic: Option<String>,
    /// Families tried, in order, for a character the primary face lacks.
    pub fallback_fonts: Option<Vec<String>>,
    /// The family for symbols and Nerd Font icons.
    pub symbol_font: Option<String>,
    /// The family for colour emoji.
    pub emoji_font: Option<String>,
    /// Line height in thousandths of the face's own (`800..=2000`).
    pub line_height_milli: Option<u16>,
    /// Whether programming ligatures are drawn.
    pub ligatures: Option<bool>,
    /// OpenType feature specs, one per entry: `1..=`[`MAX_THEME_FEATURES`] entries.
    pub font_features: Option<Vec<String>>,
    /// Whether a missing bold or italic face is synthesized.
    pub font_synthetic_style: Option<bool>,
    /// The regular face's weight (`1..=1000`, 400 regular).
    pub font_weight: Option<u16>,
    /// The text size scale in thousandths (`800..=1250`).
    pub font_size_scale_milli: Option<u16>,
}

impl ThemeTypography {
    /// No typography: a theme that leaves every type setting alone.
    pub const NONE: Self = Self {
        font_family: None,
        font_family_bold: None,
        font_family_italic: None,
        font_family_bold_italic: None,
        fallback_fonts: None,
        symbol_font: None,
        emoji_font: None,
        line_height_milli: None,
        ligatures: None,
        font_features: None,
        font_synthetic_style: None,
        font_weight: None,
        font_size_scale_milli: None,
    };

    /// Whether this sets nothing.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::NONE
    }

    /// Whether this sets `key` (always `false` for a key outside the typography group).
    #[inline]
    #[must_use]
    pub const fn is_set(&self, key: ThemeKey) -> bool {
        match key {
            ThemeKey::FontFamily => self.font_family.is_some(),
            ThemeKey::FontFamilyBold => self.font_family_bold.is_some(),
            ThemeKey::FontFamilyItalic => self.font_family_italic.is_some(),
            ThemeKey::FontFamilyBoldItalic => self.font_family_bold_italic.is_some(),
            ThemeKey::FallbackFonts => self.fallback_fonts.is_some(),
            ThemeKey::SymbolFont => self.symbol_font.is_some(),
            ThemeKey::EmojiFont => self.emoji_font.is_some(),
            ThemeKey::LineHeight => self.line_height_milli.is_some(),
            ThemeKey::Ligatures => self.ligatures.is_some(),
            ThemeKey::FontFeatures => self.font_features.is_some(),
            ThemeKey::FontSyntheticStyle => self.font_synthetic_style.is_some(),
            ThemeKey::FontWeight => self.font_weight.is_some(),
            ThemeKey::FontSizeScale => self.font_size_scale_milli.is_some(),
            _ => false,
        }
    }

    /// Unset `key` (a no-op for a key outside the typography group).
    #[inline]
    pub fn clear(&mut self, key: ThemeKey) {
        match key {
            ThemeKey::FontFamily => self.font_family = None,
            ThemeKey::FontFamilyBold => self.font_family_bold = None,
            ThemeKey::FontFamilyItalic => self.font_family_italic = None,
            ThemeKey::FontFamilyBoldItalic => self.font_family_bold_italic = None,
            ThemeKey::FallbackFonts => self.fallback_fonts = None,
            ThemeKey::SymbolFont => self.symbol_font = None,
            ThemeKey::EmojiFont => self.emoji_font = None,
            ThemeKey::LineHeight => self.line_height_milli = None,
            ThemeKey::Ligatures => self.ligatures = None,
            ThemeKey::FontFeatures => self.font_features = None,
            ThemeKey::FontSyntheticStyle => self.font_synthetic_style = None,
            ThemeKey::FontWeight => self.font_weight = None,
            ThemeKey::FontSizeScale => self.font_size_scale_milli = None,
            _ => {}
        }
    }

    /// Fill every field this leaves unset from `other` (a field set here stays).
    pub fn fill_from(&mut self, other: &Self) {
        fn fill<T: Clone>(slot: &mut Option<T>, other: &Option<T>) {
            if slot.is_none() {
                slot.clone_from(other);
            }
        }
        fill(&mut self.font_family, &other.font_family);
        fill(&mut self.font_family_bold, &other.font_family_bold);
        fill(&mut self.font_family_italic, &other.font_family_italic);
        fill(
            &mut self.font_family_bold_italic,
            &other.font_family_bold_italic,
        );
        fill(&mut self.fallback_fonts, &other.fallback_fonts);
        fill(&mut self.symbol_font, &other.symbol_font);
        fill(&mut self.emoji_font, &other.emoji_font);
        fill(&mut self.line_height_milli, &other.line_height_milli);
        fill(&mut self.ligatures, &other.ligatures);
        fill(&mut self.font_features, &other.font_features);
        fill(&mut self.font_synthetic_style, &other.font_synthetic_style);
        fill(&mut self.font_weight, &other.font_weight);
        fill(
            &mut self.font_size_scale_milli,
            &other.font_size_scale_milli,
        );
    }

    /// `key`'s value as a theme file writes it, or `None` when unset or not a typography key.
    #[must_use]
    pub fn value_text(&self, key: ThemeKey) -> Option<String> {
        let list = |items: &Vec<String>| items.join(", ");
        let flag = |b: bool| if b { "true" } else { "false" }.to_string();
        match key {
            ThemeKey::FontFamily => self.font_family.clone(),
            ThemeKey::FontFamilyBold => self.font_family_bold.clone(),
            ThemeKey::FontFamilyItalic => self.font_family_italic.clone(),
            ThemeKey::FontFamilyBoldItalic => self.font_family_bold_italic.clone(),
            ThemeKey::FallbackFonts => self.fallback_fonts.as_ref().map(list),
            ThemeKey::SymbolFont => self.symbol_font.clone(),
            ThemeKey::EmojiFont => self.emoji_font.clone(),
            ThemeKey::LineHeight => self.line_height_milli.map(milli_text),
            ThemeKey::Ligatures => self.ligatures.map(flag),
            ThemeKey::FontFeatures => self.font_features.as_ref().map(list),
            ThemeKey::FontSyntheticStyle => self.font_synthetic_style.map(flag),
            ThemeKey::FontWeight => self.font_weight.map(|w| w.to_string()),
            ThemeKey::FontSizeScale => self.font_size_scale_milli.map(milli_text),
            _ => None,
        }
    }

    /// The canonical keys this sets, in [`THEME_KEYS`] order.
    #[must_use]
    pub fn keys(&self) -> Vec<&'static str> {
        THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography && self.is_set(spec.key))
            .map(|spec| spec.name)
            .collect()
    }

    /// One line for listings and cards.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        self.phrases(false)
    }

    /// [`Self::summary`] in words for a screen reader.
    #[must_use]
    pub fn spoken(&self) -> Option<String> {
        self.phrases(true)
    }

    fn phrases(&self, spoken: bool) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut named = 0usize;
        if let Some(family) = &self.font_family {
            named += 1;
            parts.push(if spoken {
                let mut text = String::from("sets font ");
                text.push_str(family);
                text
            } else {
                family.clone()
            });
        }
        if let Some(milli) = self.line_height_milli {
            named += 1;
            let value = milli_text(milli);
            parts.push(if spoken {
                let mut text = String::from("line height ");
                text.push_str(&value);
                text
            } else {
                let mut text = value;
                text.push('×');
                text
            });
        }
        if let Some(milli) = self.font_size_scale_milli {
            named += 1;
            let mut text = String::from(if spoken { "text size " } else { "size " });
            text.push_str(&milli_text(milli));
            if !spoken {
                text.push('×');
            }
            parts.push(text);
        }
        if let Some(on) = self.ligatures {
            named += 1;
            parts.push(if on { "ligatures" } else { "no ligatures" }.to_string());
        }
        if let Some(weight) = self.font_weight {
            named += 1;
            let mut text = String::from("weight ");
            text.push_str(&weight.to_string());
            parts.push(text);
        }
        let rest = self.keys().len().saturating_sub(named);
        if rest > 0 {
            let mut text = String::from("+");
            text.push_str(&rest.to_string());
            text.push_str(if spoken {
                " more type settings"
            } else {
                " more"
            });
            parts.push(text);
        }
        Some(parts.join(if spoken { ", " } else { " · " }))
    }
}

/// A thousandths value as the shortest decimal that reads back to it.
#[must_use]
pub fn milli_text(milli: u16) -> String {
    let mut out = (milli / 1000).to_string();
    out.push('.');
    let frac = milli % 1000;
    if frac == 0 {
        out.push('0');
        return out;
    }
    let mut digits = [b'0'; 3];
    digits[0] = b'0' + u8::try_from(frac / 100).unwrap_or(0);
    digits[1] = b'0' + u8::try_from(frac / 10 % 10).unwrap_or(0);
    digits[2] = b'0' + u8::try_from(frac % 10).unwrap_or(0);
    let mut len = 3;
    while len > 1 && digits[len - 1] == b'0' {
        len -= 1;
    }
    for digit in &digits[..len] {
        out.push(char::from(*digit));
    }
    out
}

/// Why a font name was refused: a theme names a family, never a file.
pub const FONT_NAME_IS_PATH: &str =
    "font_family names a family, not a file: a theme cannot point at a path";

/// Read one family name.
/// # Errors
/// The reason, for the parser to place at the key's line.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_font_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() {
        return Err("a font name is empty".to_string());
    }
    if name.chars().count() > MAX_THEME_FONT_NAME_CHARS {
        let mut reason = String::from("a font name is longer than ");
        reason.push_str(&MAX_THEME_FONT_NAME_CHARS.to_string());
        reason.push_str(" characters");
        return Err(reason);
    }
    if name.chars().any(char::is_control) {
        return Err("a font name contains a control character".to_string());
    }
    let lower = name.to_ascii_lowercase();
    let drive = {
        let mut chars = name.chars();
        matches!((chars.next(), chars.next()), (Some(c), Some(':')) if c.is_ascii_alphabetic())
    };
    let file_ext = [".ttf", ".otf", ".ttc", ".otc", ".dfont", ".woff", ".woff2"]
        .iter()
        .any(|ext| lower.ends_with(ext));
    if name.contains('/')
        || name.contains('\\')
        || name.starts_with('~')
        || name.starts_with('.')
        || drive
        || file_ext
    {
        return Err(FONT_NAME_IS_PATH.to_string());
    }
    Ok(name.to_string())
}

/// Read a comma-separated list of family names.
/// # Errors
/// The reason, for the parser to place at the key's line.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_font_list(value: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in value.split(',') {
        if entry.trim().is_empty() {
            return Err("a font list has an empty entry".to_string());
        }
        names.push(parse_font_name(entry)?);
        if names.len() > MAX_THEME_FONT_LIST {
            let mut reason = String::from("a font list holds at most ");
            reason.push_str(&MAX_THEME_FONT_LIST.to_string());
            reason.push_str(" names");
            return Err(reason);
        }
    }
    Ok(names)
}

/// Read `true` or `false`, case-insensitive.
/// # Errors
/// The reason, for the parser to place at the key's line.
pub fn parse_flag(value: &str) -> Result<bool, String> {
    let word = value.trim();
    if word.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if word.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        let mut reason = String::from("expected true or false, got ");
        reason.push_str(&crate::trust_fmt::debug_string(word));
        Err(reason)
    }
}

/// `", from <min> to <max>"`-style range text for an error.
fn range_text(min: &str, max: &str) -> String {
    let mut out = String::from(" (allowed: ");
    out.push_str(min);
    out.push_str(" to ");
    out.push_str(max);
    out.push(')');
    out
}

/// Read a decimal with at most three fraction digits as thousandths, by integer arithmetic only.
/// # Errors
/// The reason, for the parser to place at the key's line.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_milli(value: &str, min: u16, max: u16) -> Result<u16, String> {
    let text = value.trim();
    let malformed = || {
        let mut reason = String::from("expected a decimal number such as 1.15, got ");
        reason.push_str(&crate::trust_fmt::debug_string(text));
        reason.push_str(&range_text(&milli_text(min), &milli_text(max)));
        reason
    };
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (text, None),
    };
    if whole.is_empty() || whole.len() > 4 || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed());
    }
    let mut milli: u32 = 0;
    for b in whole.bytes() {
        milli = milli * 10 + u32::from(b - b'0');
    }
    milli *= 1000;
    if let Some(frac) = frac {
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(malformed());
        }
        let mut scale = 100;
        for b in frac.bytes() {
            milli += u32::from(b - b'0') * scale;
            scale /= 10;
        }
    }
    match u16::try_from(milli) {
        Ok(milli) if (min..=max).contains(&milli) => Ok(milli),
        _ => {
            let mut reason = String::from("value ");
            reason.push_str(text);
            reason.push_str(" is out of range");
            reason.push_str(&range_text(&milli_text(min), &milli_text(max)));
            Err(reason)
        }
    }
}

/// Read a whole number in `[min, max]`: digits only, no sign.
/// # Errors
/// The reason, for the parser to place at the key's line.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_integer(value: &str, min: u16, max: u16) -> Result<u16, String> {
    let text = value.trim();
    let range = range_text(&min.to_string(), &max.to_string());
    if text.is_empty() || text.len() > 5 || !text.bytes().all(|b| b.is_ascii_digit()) {
        let mut reason = String::from("expected a whole number, got ");
        reason.push_str(&crate::trust_fmt::debug_string(text));
        reason.push_str(&range);
        return Err(reason);
    }
    let mut n: u32 = 0;
    for b in text.bytes() {
        n = n * 10 + u32::from(b - b'0');
    }
    match u16::try_from(n) {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        _ => {
            let mut reason = String::from("value ");
            reason.push_str(text);
            reason.push_str(" is out of range");
            reason.push_str(&range);
            Err(reason)
        }
    }
}

/// Whether `spec` is one OpenType feature spec a theme may carry.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn is_feature_spec(spec: &str) -> bool {
    let tag_ok =
        |tag: &str| (1..=4).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_alphanumeric());
    if let Some(tag) = spec.strip_prefix('+').or_else(|| spec.strip_prefix('-')) {
        return tag_ok(tag);
    }
    match spec.split_once('=') {
        Some((tag, n)) => {
            tag_ok(tag)
                && (1..=5).contains(&n.len())
                && n.bytes().all(|b| b.is_ascii_digit())
                && n.parse::<u32>().is_ok_and(|n| n <= 65_535)
        }
        None => tag_ok(spec),
    }
}

/// Read a feature list.
/// # Errors
/// The reason, for the parser to place at the key's line.
#[cfg_attr(trust_verify, trust::skip)]
pub fn parse_feature_list(value: &str) -> Result<Vec<String>, String> {
    let mut specs = Vec::new();
    for entry in value.split(',') {
        if entry.trim().is_empty() {
            return Err("a feature list has an empty entry".to_string());
        }
        for spec in entry.split_whitespace() {
            if !is_feature_spec(spec) {
                let mut reason = String::from("not a font feature (tag, +tag, -tag or tag=N): ");
                reason.push_str(&crate::trust_fmt::debug_string(spec));
                return Err(reason);
            }
            specs.push(spec.to_string());
            if specs.len() > MAX_THEME_FEATURES {
                let mut reason = String::from("a feature list holds at most ");
                reason.push_str(&MAX_THEME_FEATURES.to_string());
                reason.push_str(" features");
                return Err(reason);
            }
        }
    }
    Ok(specs)
}

/// Read a `schema` version: a whole number `1..=65535`.
/// # Errors
/// The reason, for the parser to place at the key's line.
pub fn parse_schema(value: &str) -> Result<u16, String> {
    parse_integer(value, 1, u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{
        CatalogTheme, ParsedTheme, ThemeError, ThemeMeta, ThemeValueType, ThemeWarning,
        builtin_names, parse_theme_str,
    };
    use crate::scheme_import::{to_native_conf_package, to_native_conf_package_commented};

    const BASE: &str = "foreground = #c0caf5\nbackground = #1a1b26\n";

    fn with(line: &str) -> Result<ParsedTheme, ThemeError> {
        parse_theme_str(&format!("{BASE}{line}\n"))
    }

    fn error_key_reason(result: Result<ParsedTheme, ThemeError>) -> (Option<String>, String) {
        match result {
            Err(ThemeError::Parse { key, reason, .. }) => (key, reason),
            Ok(parsed) => panic!("expected a parse error, got {parsed:?}"),
        }
    }

    /// Every typography row of the table parses its example into exactly its
    /// own field, and the package writer reads back equal.
    #[test]
    fn typography_rows_round_trip() {
        let rows: Vec<_> = THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography)
            .collect();
        assert!(!rows.is_empty());
        for spec in rows {
            let parsed = with(&format!("{} = {}", spec.name, spec.example))
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            assert_eq!(parsed.typography.keys(), vec![spec.name], "{}", spec.name);
            assert!(parsed.typography.is_set(spec.key));
            for written in [
                to_native_conf_package(&parsed, &["a test"]),
                to_native_conf_package_commented(&parsed, &[]),
            ] {
                let back = parse_theme_str(&written).unwrap_or_else(|e| panic!("{e}\n{written}"));
                assert_eq!(back, parsed, "{written}");
            }
            let mut cleared = parsed.typography.clone();
            cleared.clear(spec.key);
            assert!(cleared.is_empty(), "{}", spec.name);
        }
    }

    /// A theme carrying every key (colours, schema, bold_is_bright and all
    /// the typography) round-trips through both package writers.
    #[test]
    fn full_package_round_trips() {
        let mut text = String::from(BASE);
        text.push_str("schema = 1\nbold_is_bright = false\n");
        for spec in THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography)
        {
            text.push_str(&format!("{} = {}\n", spec.name, spec.example));
        }
        let parsed = parse_theme_str(&text).expect("parses");
        assert_eq!(parsed.schema, Some(1));
        assert_eq!(parsed.bold_is_bright, Some(false));
        for written in [
            to_native_conf_package(&parsed, &[]),
            to_native_conf_package_commented(&parsed, &[]),
        ] {
            assert_eq!(
                parse_theme_str(&written).expect("parses"),
                parsed,
                "{written}"
            );
        }
        let colours = crate::scheme_import::to_native_conf(
            &parsed.scheme,
            parsed.appearance_declared,
            None,
            None,
            &[],
        );
        assert!(
            parse_theme_str(&colours)
                .expect("parses")
                .typography
                .is_empty()
        );
    }

    /// A theme never names a file: each path-like or malformed name is a
    /// parse error about its key, for every font-name key.
    #[test]
    fn font_names_never_name_files() {
        let long = "x".repeat(MAX_THEME_FONT_NAME_CHARS + 1);
        let bad = [
            "/Library/Fonts/X.ttf",
            "~/f",
            "C:\\f",
            "..\\f",
            "Mono.otf",
            "Mono.WOFF2",
            ".hidden",
            "a\u{7}b",
            long.as_str(),
        ];
        for key in ["font_family", "symbol_font", "fallback_fonts"] {
            for value in bad {
                let (got, reason) = error_key_reason(with(&format!("{key} = {value}")));
                assert_eq!(got.as_deref(), Some(key), "{key} = {value:?}: {reason}");
            }
            assert!(with(&format!("{key} =")).is_err());
            for good in ["Berkeley Mono", "display:pixel+chunky"] {
                assert!(with(&format!("{key} = {good}")).is_ok(), "{key} = {good}");
            }
        }
        let (_, reason) = error_key_reason(with("font_family = /Library/Fonts/X.ttf"));
        assert!(reason.contains(FONT_NAME_IS_PATH), "{reason}");
        assert!(with("fallback_fonts = Menlo, /tmp/x.ttf").is_err());
        assert!(with("fallback_fonts = Menlo,,Monaco").is_err());
        let nine = ["Menlo"; MAX_THEME_FONT_LIST + 1].join(", ");
        assert!(with(&format!("fallback_fonts = {nine}")).is_err());
        let eight = ["Menlo"; MAX_THEME_FONT_LIST].join(", ");
        assert!(with(&format!("fallback_fonts = {eight}")).is_ok());
    }

    /// The decimal and integer readers keep exactly their ranges, and each
    /// refusal says the range.
    #[test]
    fn numeric_ranges_are_exact() {
        let ok = |line: &str| with(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        let refused = |line: &str| {
            let (_, reason) = error_key_reason(with(line));
            assert!(reason.contains("allowed:"), "{line}: {reason}");
        };
        refused("line_height = 0.799");
        assert_eq!(
            ok("line_height = 0.8").typography.line_height_milli,
            Some(800)
        );
        assert_eq!(
            ok("line_height = 2.0").typography.line_height_milli,
            Some(2000)
        );
        assert_eq!(
            ok("line_height = 2").typography.line_height_milli,
            Some(2000)
        );
        refused("line_height = 2.001");
        for malformed in [
            "1e0", "+1.1", "nan", "inf", "1.1234", "1.", ".5", "-1", "1,2",
        ] {
            refused(&format!("line_height = {malformed}"));
        }
        assert_eq!(
            ok("font_size_scale = 1.25")
                .typography
                .font_size_scale_milli,
            Some(1250)
        );
        refused("font_size_scale = 1.251");
        refused("font_weight = 0");
        assert_eq!(ok("font_weight = 1").typography.font_weight, Some(1));
        assert_eq!(ok("font_weight = 1000").typography.font_weight, Some(1000));
        refused("font_weight = 1001");
        refused("font_weight = 400.5");
        for milli in 0..=9999u16 {
            assert_eq!(
                parse_milli(&milli_text(milli), 0, 9999),
                Ok(milli),
                "{milli}"
            );
        }
    }

    /// The feature reader accepts exactly specs the renderer's reader takes
    /// (`FontFeature::parse_token`), and refuses malformed lists.
    #[test]
    fn feature_lists_match_text_shaping() {
        for (value, count) in [("calt, -liga, ss01=2", 3), ("+zero ss02", 2)] {
            let parsed = with(&format!("font_features = {value}")).expect(value);
            let specs = parsed.typography.font_features.expect("set");
            assert_eq!(specs.len(), count, "{value}");
            for spec in &specs {
                assert!(
                    crate::text_shaping::FontFeature::parse_token(spec).is_some(),
                    "{spec}"
                );
            }
        }
        let seventeen = ["calt"; MAX_THEME_FEATURES + 1].join(" ");
        for bad in [
            "toolong5",
            "calt,,ss02",
            "+ss01=2",
            "ca-t",
            "x=70000",
            &seventeen,
        ] {
            assert!(with(&format!("font_features = {bad}")).is_err(), "{bad}");
        }
        assert!(!is_feature_spec("+ss01=2"));
    }

    /// A newer schema skips the keys this aterm does not know and lists
    /// them; every known key stays strict, and without a newer schema an
    /// unknown key is still refused.
    #[test]
    fn schema_gate() {
        let parsed = with("schema = 2\nfuture_key = x").expect("a newer file loads");
        assert_eq!(parsed.schema, Some(2));
        assert_eq!(parsed.skipped_keys, vec!["future_key".to_string()]);
        assert_eq!(parsed.skipped_count, 1);
        let mut admitted = parsed.clone();
        let meta = ThemeMeta::admit(&mut admitted, "t", crate::Appearance::Dark);
        assert!(meta.warnings.contains(&ThemeWarning::SkippedNewerKeys {
            schema: 2,
            count: 1
        }));
        assert_eq!(meta.skipped_keys, vec!["future_key".to_string()]);
        for body in ["schema = 1\nfuture_key = x", "future_key = x"] {
            let (key, reason) = error_key_reason(with(body));
            assert_eq!(key.as_deref(), Some("future_key"));
            assert!(reason.contains("unknown key"), "{reason}");
        }
        assert!(with("schema = 2\nline_height = big").is_err());
        for bad in ["schema = 0", "schema = x", "schema = 1\nschema = 1"] {
            let (key, _) = error_key_reason(with(bad));
            assert_eq!(key.as_deref(), Some("schema"), "{bad}");
        }
        let mut body = String::from("schema = 9\n");
        for n in 0..40 {
            body.push_str(&format!("future_{n} = x\n"));
        }
        let many = with(&body).expect("loads");
        assert_eq!(many.skipped_keys.len(), MAX_SKIPPED_THEME_KEYS);
        assert_eq!(many.skipped_count, 40);
        let long = with(&format!("schema = 2\n{} = x", "k".repeat(200))).expect("loads");
        assert_eq!(long.skipped_keys[0].len(), MAX_SKIPPED_THEME_KEY_BYTES);
        let evil = with("schema = 999\n\x1b]0;pwned\x07\u{9b}2J = 1").expect("loads");
        assert_eq!(
            evil.skipped_keys,
            vec!["\\u{1b}]0;pwned\\u{7}\\u{9b}2j".to_string()]
        );
        assert!(!evil.skipped_keys[0].chars().any(char::is_control));
    }

    /// A typography key written twice is a duplicate naming the first line.
    #[test]
    fn duplicate_typography_keys() {
        let (key, reason) =
            error_key_reason(with("fallback_fonts = Menlo\nfallback_fonts = Monaco"));
        assert_eq!(key.as_deref(), Some("fallback_fonts"));
        assert!(reason.contains("line 3"), "{reason}");
    }

    /// DTP §7: no built-in carries typography or `bold_is_bright`, so a
    /// built-in theme changes no type setting.
    #[test]
    fn builtins_have_no_typography() {
        for name in builtin_names() {
            let theme = CatalogTheme::builtin(name).expect("a built-in");
            assert!(theme.theme.typography.is_empty(), "{name}");
            assert_eq!(theme.theme.bold_is_bright, None, "{name}");
            let package = crate::scheme::ThemePackage::of_builtin(
                crate::scheme::builtin_theme(name).expect("identity"),
            )
            .expect("package");
            assert!(package.typography.is_empty());
            assert_eq!(package.colors, theme.theme.scheme);
        }
    }

    /// The summary names what is set and is `None` for nothing; the spoken
    /// form carries no glyphs.
    #[test]
    fn summary_follows_the_fields() {
        assert_eq!(ThemeTypography::NONE.summary(), None);
        let parsed =
            with("font_family = Berkeley Mono\nline_height = 1.15\nemoji_font = Noto").expect("ok");
        let summary = parsed.typography.summary().expect("set");
        assert!(
            summary.contains("Berkeley Mono") && summary.contains("1.15"),
            "{summary}"
        );
        let spoken = parsed.typography.spoken().expect("set");
        assert!(!spoken.contains('×') && !spoken.contains('·'), "{spoken}");
        for spec in &THEME_KEYS {
            assert!(!spec.value.label().is_empty());
            if let ThemeValueType::Milli { min, max } = spec.value {
                assert!(min < max);
            }
        }
    }
}
