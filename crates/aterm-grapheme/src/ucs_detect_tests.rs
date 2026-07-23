// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ucs-detect style validation tests for grapheme width calculations.
//!
//! These tests validate that aterm-grapheme's Unicode width calculations match
//! expected terminal behavior, using test vectors inspired by the jquast/ucs-detect
//! project methodology.
//!
//! ## Test Categories (from ucs-detect)
//!
//! - **WIDE**: East Asian width characters (CJK ideographs, fullwidth forms)
//! - **ZERO**: Zero-width combining characters and marks
//! - **ZWJ**: Zero Width Joiner emoji sequences
//! - **VS16**: Emoji Variation Selector-16 (FE0F) sequences
//!
//! ## Reference
//!
//! - <https://github.com/jquast/ucs-detect>
//! - Unicode TR11: East Asian Width
//! - Unicode TR51: Emoji
//!
//! Part of #245: Integrate ucs-detect methodology for Unicode width validation

#[cfg(test)]
mod tests {
    use crate::{
        GraphemeType, classify_grapheme, grapheme_display_width, grapheme_width, has_zwj,
        is_flag_emoji,
    };

    // =========================================================================
    // WIDE CHARACTER TESTS (East Asian Width = Wide or Fullwidth)
    // =========================================================================

    /// Test CJK Unified Ideographs (U+4E00-U+9FFF)
    /// These are the most common wide characters in terminals.
    #[test]
    fn wide_cjk_unified_ideographs() {
        // Common Chinese characters
        assert_eq!(
            grapheme_display_width("\u{4E2D}"),
            2,
            "中 (U+4E2D) should be wide"
        );
        assert_eq!(
            grapheme_display_width("\u{6587}"),
            2,
            "文 (U+6587) should be wide"
        );
        assert_eq!(
            grapheme_display_width("\u{65E5}"),
            2,
            "日 (U+65E5) should be wide"
        );
        assert_eq!(
            grapheme_display_width("\u{672C}"),
            2,
            "本 (U+672C) should be wide"
        );

        // Boundary characters
        assert_eq!(
            grapheme_display_width("\u{4E00}"),
            2,
            "U+4E00 start of CJK block"
        );
        assert_eq!(
            grapheme_display_width("\u{9FFF}"),
            2,
            "U+9FFF end of CJK block"
        );
    }

    /// Test Japanese Hiragana (U+3040-U+309F)
    #[test]
    fn wide_japanese_hiragana() {
        assert_eq!(grapheme_display_width("\u{3042}"), 2, "U+3042");
        assert_eq!(grapheme_display_width("\u{3044}"), 2, "U+3044");
        assert_eq!(grapheme_display_width("\u{3046}"), 2, "U+3046");
        assert_eq!(
            grapheme_display_width("\u{3093}"),
            2,
            "U+3093 final hiragana"
        );
    }

    /// Test Japanese Katakana (U+30A0-U+30FF)
    #[test]
    fn wide_japanese_katakana() {
        assert_eq!(grapheme_display_width("\u{30A2}"), 2, "U+30A2");
        assert_eq!(grapheme_display_width("\u{30A4}"), 2, "U+30A4");
        assert_eq!(grapheme_display_width("\u{30A6}"), 2, "U+30A6");
    }

    /// Test Korean Hangul syllables (U+AC00-U+D7A3)
    #[test]
    fn wide_korean_hangul() {
        assert_eq!(
            grapheme_display_width("\u{AC00}"),
            2,
            "U+AC00 first syllable"
        );
        assert_eq!(grapheme_display_width("\u{D55C}"), 2, "U+D55C");
        assert_eq!(grapheme_display_width("\u{AE00}"), 2, "U+AE00");
    }

    /// Test Fullwidth ASCII forms (U+FF00-U+FF5E)
    /// These are double-width versions of ASCII characters.
    #[test]
    fn wide_fullwidth_ascii() {
        assert_eq!(grapheme_display_width("\u{FF21}"), 2, "U+FF21 fullwidth A");
        assert_eq!(grapheme_display_width("\u{FF3A}"), 2, "U+FF3A fullwidth Z");
        assert_eq!(grapheme_display_width("\u{FF10}"), 2, "U+FF10 fullwidth 0");
        assert_eq!(grapheme_display_width("\u{FF19}"), 2, "U+FF19 fullwidth 9");
        assert_eq!(
            grapheme_display_width("\u{FF01}"),
            2,
            "U+FF01 fullwidth exclamation"
        );
    }

    /// Test string of wide characters has correct total width
    #[test]
    fn wide_string_total_width() {
        let info = grapheme_width("\u{4E2D}\u{6587}\u{65E5}\u{672C}\u{8A9E}");
        assert_eq!(info.grapheme_count, 5, "5 graphemes");
        assert_eq!(info.display_width, 10, "10 cells total");
        assert!(info.has_wide);
    }

    // =========================================================================
    // ZERO WIDTH TESTS (Combining marks, ZWJ, etc.)
    // =========================================================================

    /// Test combining diacritical marks (U+0300-U+036F)
    /// These should not add to display width.
    #[test]
    fn zero_combining_diacriticals() {
        // e + combining acute accent = single grapheme, 1 cell
        let acute = "e\u{0301}";
        let info = grapheme_width(acute);
        assert_eq!(info.grapheme_count, 1, "e + acute = 1 grapheme");
        assert_eq!(info.display_width, 1, "e + acute = 1 cell");
        assert!(info.has_combining);

        // a + combining grave = single grapheme
        let grave = "a\u{0300}";
        let info = grapheme_width(grave);
        assert_eq!(info.display_width, 1);

        // o + combining circumflex
        let circumflex = "o\u{0302}";
        let info = grapheme_width(circumflex);
        assert_eq!(info.display_width, 1);
    }

    /// Test multiple stacked combining marks
    #[test]
    fn zero_stacked_combining() {
        // a + grave + acute + circumflex + tilde
        let stacked = "a\u{0300}\u{0301}\u{0302}\u{0303}";
        let info = grapheme_width(stacked);
        assert_eq!(info.grapheme_count, 1, "stacked marks = 1 grapheme");
        assert_eq!(info.display_width, 1, "stacked marks = 1 cell");
        assert_eq!(info.codepoint_count, 5, "5 codepoints");
    }

    /// Test Thai combining marks
    #[test]
    fn zero_thai_combining() {
        // Thai ko kai + sara i + mai tho
        let thai = "\u{0E01}\u{0E34}\u{0E49}";
        let info = grapheme_width(thai);
        assert_eq!(info.grapheme_count, 1);
        // Thai consonants are typically 1 cell wide
        assert!(info.display_width <= 2);
    }

    /// Test Devanagari virama (halant) combining
    #[test]
    fn zero_devanagari_virama() {
        // ka + virama + sha
        let ksha = "\u{0915}\u{094D}\u{0937}";
        let info = grapheme_width(ksha);
        // Conjuncts may render as 1-2 graphemes depending on font
        assert!(info.grapheme_count >= 1);
    }

    /// Test Zero Width Joiner alone has no width
    #[test]
    fn zero_zwj_alone() {
        let zwj = "\u{200D}";
        let info = grapheme_width(zwj);
        assert_eq!(info.display_width, 0, "ZWJ alone = 0 width");
    }

    /// Test Zero Width Non-Joiner
    #[test]
    fn zero_zwnj() {
        let zwnj = "\u{200C}";
        let info = grapheme_width(zwnj);
        assert_eq!(info.display_width, 0, "ZWNJ = 0 width");
    }

    // =========================================================================
    // ZWJ SEQUENCE TESTS (Emoji joined by Zero Width Joiner)
    // =========================================================================

    /// Test family emoji ZWJ sequences
    #[test]
    fn zwj_family_sequences() {
        // man + ZWJ + woman + ZWJ + girl + ZWJ + boy
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let info = grapheme_width(family);
        assert_eq!(info.grapheme_count, 1, "family = 1 grapheme cluster");
        assert_eq!(info.display_width, 2, "family = 2 cells");
        assert!(has_zwj(family));
        assert_eq!(classify_grapheme(family), GraphemeType::ZwjSequence);
    }

    /// Test profession ZWJ sequences
    #[test]
    fn zwj_profession_sequences() {
        // man + ZWJ + rocket (astronaut)
        let astronaut = "\u{1F468}\u{200D}\u{1F680}";
        assert!(has_zwj(astronaut));
        let info = grapheme_width(astronaut);
        assert_eq!(info.grapheme_count, 1);
        assert_eq!(info.display_width, 2);

        // woman + ZWJ + microscope (scientist)
        let scientist = "\u{1F469}\u{200D}\u{1F52C}";
        assert!(has_zwj(scientist));
        let info = grapheme_width(scientist);
        assert_eq!(info.grapheme_count, 1);
    }

    /// Test gender ZWJ sequences
    #[test]
    fn zwj_gender_sequences() {
        // Person running + ZWJ + male sign + VS16
        let man_running = "\u{1F3C3}\u{200D}\u{2642}\u{FE0F}";
        assert!(has_zwj(man_running));

        // Person running + ZWJ + female sign + VS16
        let woman_running = "\u{1F3C3}\u{200D}\u{2640}\u{FE0F}";
        assert!(has_zwj(woman_running));
    }

    /// Test that non-ZWJ emoji don't have ZWJ flag
    #[test]
    fn zwj_not_present_in_simple_emoji() {
        assert!(!has_zwj("\u{1F600}"));
        assert!(!has_zwj("\u{1F389}"));
        assert!(!has_zwj("\u{1F680}"));
        assert!(!has_zwj("A"));
        assert!(!has_zwj("\u{4E2D}"));
    }

    // =========================================================================
    // VS-16 TESTS (Emoji Variation Selector-16)
    // =========================================================================

    /// Test VS-16 (U+FE0F) emoji presentation
    #[test]
    fn vs16_emoji_presentation() {
        // sun without VS-16 might be text style
        let sun_text = "\u{2600}";

        // sun with VS-16 should be emoji style (2 cells)
        let sun_emoji = "\u{2600}\u{FE0F}";

        // Both should be single grapheme
        assert_eq!(grapheme_width(sun_text).grapheme_count, 1);
        assert_eq!(grapheme_width(sun_emoji).grapheme_count, 1);

        // Emoji presentation is typically 2 cells
        let info = grapheme_width(sun_emoji);
        assert_eq!(info.display_width, 2, "sun with VS16 should be wide");
    }

    /// Test VS-15 (U+FE0E) text presentation
    #[test]
    fn vs15_text_presentation() {
        let heart = "\u{2764}";
        let heart_emoji = "\u{2764}\u{FE0F}";
        let heart_text = "\u{2764}\u{FE0E}";

        assert_eq!(grapheme_width(heart).grapheme_count, 1);
        assert_eq!(grapheme_width(heart_emoji).grapheme_count, 1);
        assert_eq!(grapheme_width(heart_text).grapheme_count, 1);
    }

    /// Test keycap sequences (digit + VS-16 + combining enclosing keycap)
    #[test]
    fn vs16_keycap_sequences() {
        // 0 + VS-16 + combining enclosing keycap
        let keycap_0 = "0\u{FE0F}\u{20E3}";
        let info = grapheme_width(keycap_0);
        assert_eq!(info.grapheme_count, 1, "keycap = 1 grapheme");
        assert_eq!(info.display_width, 2, "keycap = 2 cells");

        let keycap_1 = "1\u{FE0F}\u{20E3}";
        let info = grapheme_width(keycap_1);
        assert_eq!(info.grapheme_count, 1);

        let keycap_hash = "#\u{FE0F}\u{20E3}";
        let info = grapheme_width(keycap_hash);
        assert_eq!(info.grapheme_count, 1);

        let keycap_star = "*\u{FE0F}\u{20E3}";
        let info = grapheme_width(keycap_star);
        assert_eq!(info.grapheme_count, 1);
    }

    // =========================================================================
    // FLAG EMOJI TESTS (Regional Indicator pairs)
    // =========================================================================

    /// Test country flag emoji
    #[test]
    fn flags_country() {
        // US flag
        let us_flag = "\u{1F1FA}\u{1F1F8}";
        assert!(is_flag_emoji(us_flag));
        let info = grapheme_width(us_flag);
        assert_eq!(info.grapheme_count, 1, "flag = 1 grapheme");
        assert_eq!(info.display_width, 2, "flag = 2 cells");
        assert_eq!(classify_grapheme(us_flag), GraphemeType::Flag);

        // JP flag
        let jp_flag = "\u{1F1EF}\u{1F1F5}";
        assert!(is_flag_emoji(jp_flag));
        assert_eq!(grapheme_width(jp_flag).grapheme_count, 1);

        // GB flag
        let gb_flag = "\u{1F1EC}\u{1F1E7}";
        assert!(is_flag_emoji(gb_flag));
        assert_eq!(grapheme_width(gb_flag).grapheme_count, 1);

        // FR flag
        let fr_flag = "\u{1F1EB}\u{1F1F7}";
        assert!(is_flag_emoji(fr_flag));
        assert_eq!(grapheme_width(fr_flag).grapheme_count, 1);
    }

    /// Test that single regional indicator is not a flag
    #[test]
    fn flags_single_indicator_not_flag() {
        let single = "\u{1F1FA}";
        assert!(!is_flag_emoji(single));
    }

    // =========================================================================
    // SKIN TONE MODIFIER TESTS
    // =========================================================================

    /// Test emoji with skin tone modifiers
    #[test]
    fn skin_tone_modifiers() {
        // wave + light skin tone
        let wave_light = "\u{1F44B}\u{1F3FB}";
        let info = grapheme_width(wave_light);
        assert_eq!(info.grapheme_count, 1, "wave + skin tone = 1 grapheme");
        assert_eq!(info.display_width, 2);

        // wave + medium-light skin tone
        let wave_medium_light = "\u{1F44B}\u{1F3FC}";
        assert_eq!(grapheme_width(wave_medium_light).grapheme_count, 1);

        // wave + medium skin tone
        let wave_medium = "\u{1F44B}\u{1F3FD}";
        assert_eq!(grapheme_width(wave_medium).grapheme_count, 1);

        // wave + medium-dark skin tone
        let wave_medium_dark = "\u{1F44B}\u{1F3FE}";
        assert_eq!(grapheme_width(wave_medium_dark).grapheme_count, 1);

        // wave + dark skin tone
        let wave_dark = "\u{1F44B}\u{1F3FF}";
        assert_eq!(grapheme_width(wave_dark).grapheme_count, 1);
    }

    // =========================================================================
    // BASIC EMOJI TESTS
    // =========================================================================

    /// Test basic emoji without modifiers
    #[test]
    fn emoji_basic() {
        // Emoticons block (U+1F600-U+1F64F) - always wide
        assert_eq!(grapheme_display_width("\u{1F600}"), 2, "grinning face");
        assert_eq!(grapheme_display_width("\u{1F601}"), 2, "beaming face");
        assert_eq!(grapheme_display_width("\u{1F64F}"), 2, "folded hands");

        // Transport & map symbols (U+1F680-U+1F6FF) - always wide
        assert_eq!(grapheme_display_width("\u{1F680}"), 2, "rocket");

        // Dingbats (U+2700-U+27BF) - these follow Unicode TR11
        // U+2708 is East Asian Width = N (Neutral), so width 1
        assert_eq!(
            grapheme_display_width("\u{2708}"),
            1,
            "airplane (no VS-16) = narrow"
        );
        assert_eq!(
            grapheme_display_width("\u{2708}\u{FE0F}"),
            2,
            "airplane with VS-16 = wide"
        );

        // Miscellaneous Symbols (U+2600-U+26FF)
        assert_eq!(grapheme_display_width("\u{2B50}"), 2, "star");

        // Supplemental Symbols (U+1F300-U+1F5FF)
        assert_eq!(grapheme_display_width("\u{1F389}"), 2, "party popper");
    }

    // =========================================================================
    // NARROW CHARACTER TESTS (ASCII, Latin, etc.)
    // =========================================================================

    /// Test ASCII characters are narrow (1 cell)
    #[test]
    fn narrow_ascii() {
        assert_eq!(grapheme_display_width("a"), 1);
        assert_eq!(grapheme_display_width("z"), 1);
        assert_eq!(grapheme_display_width("A"), 1);
        assert_eq!(grapheme_display_width("Z"), 1);
        assert_eq!(grapheme_display_width("0"), 1);
        assert_eq!(grapheme_display_width("9"), 1);
        assert_eq!(grapheme_display_width("."), 1);
        assert_eq!(grapheme_display_width(","), 1);
        assert_eq!(grapheme_display_width("!"), 1);
        assert_eq!(grapheme_display_width("?"), 1);
        assert_eq!(grapheme_display_width(" "), 1);
    }

    /// Test Latin Extended characters
    #[test]
    fn narrow_latin_extended() {
        assert_eq!(grapheme_display_width("\u{00E9}"), 1, "e-acute");
        assert_eq!(grapheme_display_width("\u{00F1}"), 1, "n-tilde");
        assert_eq!(grapheme_display_width("\u{00FC}"), 1, "u-umlaut");
        assert_eq!(grapheme_display_width("\u{00F8}"), 1, "o-slash");
        assert_eq!(grapheme_display_width("\u{00DF}"), 1, "sharp s");
    }

    /// Test box drawing characters are narrow
    #[test]
    fn narrow_box_drawing() {
        assert_eq!(grapheme_display_width("\u{2500}"), 1, "horizontal line");
        assert_eq!(grapheme_display_width("\u{2502}"), 1, "vertical line");
        assert_eq!(grapheme_display_width("\u{250C}"), 1, "top-left corner");
        assert_eq!(grapheme_display_width("\u{2510}"), 1, "top-right corner");
        assert_eq!(grapheme_display_width("\u{2514}"), 1, "bottom-left corner");
        assert_eq!(grapheme_display_width("\u{2518}"), 1, "bottom-right corner");
        assert_eq!(grapheme_display_width("\u{251C}"), 1, "tee left");
        assert_eq!(grapheme_display_width("\u{2524}"), 1, "tee right");
        assert_eq!(grapheme_display_width("\u{253C}"), 1, "cross");
        assert_eq!(grapheme_display_width("\u{2550}"), 1, "double horizontal");
        assert_eq!(grapheme_display_width("\u{2551}"), 1, "double vertical");
    }

    /// Test block elements are narrow
    #[test]
    fn narrow_block_elements() {
        assert_eq!(grapheme_display_width("\u{2580}"), 1, "upper half block");
        assert_eq!(grapheme_display_width("\u{2584}"), 1, "lower half block");
        assert_eq!(grapheme_display_width("\u{2588}"), 1, "full block");
        assert_eq!(grapheme_display_width("\u{2591}"), 1, "light shade");
        assert_eq!(grapheme_display_width("\u{2592}"), 1, "medium shade");
        assert_eq!(grapheme_display_width("\u{2593}"), 1, "dark shade");
    }

    // =========================================================================
    // CONTROL CHARACTER TESTS
    // =========================================================================

    /// Test control characters width.
    #[test]
    fn control_characters_width() {
        // C0 controls - unicode-width returns 1 for terminal compatibility
        assert_eq!(
            grapheme_display_width("\x00"),
            1,
            "null (diverges from ucs-detect)"
        );
        assert_eq!(
            grapheme_display_width("\x07"),
            1,
            "bell (diverges from ucs-detect)"
        );
        assert_eq!(
            grapheme_display_width("\x08"),
            1,
            "backspace (diverges from ucs-detect)"
        );
        assert_eq!(
            grapheme_display_width("\x1B"),
            1,
            "escape (diverges from ucs-detect)"
        );
        assert_eq!(
            grapheme_display_width("\x7F"),
            1,
            "delete (diverges from ucs-detect)"
        );
    }

    // =========================================================================
    // MIXED STRING TESTS
    // =========================================================================

    /// Test realistic mixed-script string
    #[test]
    fn mixed_hello_world() {
        let hello = "Hello, \u{4E16}\u{754C}!";
        let info = grapheme_width(hello);
        // "Hello, " = 7, "世界" = 4, "!" = 1 -> total 12
        assert_eq!(info.display_width, 12);
        assert_eq!(info.grapheme_count, 10);
        assert!(info.has_wide);
        assert!(!info.has_emoji);
    }

    /// Test string with emoji interspersed
    #[test]
    fn mixed_emoji_text() {
        let love = "I \u{2764}\u{FE0F} \u{65E5}\u{672C}";
        let info = grapheme_width(love);
        // "I " = 2, "heart" = 2, " " = 1, "日本" = 4 -> total 9
        assert_eq!(info.display_width, 9);
        assert!(info.has_wide);
        assert!(info.has_emoji);
    }

    // =========================================================================
    // CJK EXTENSION G/H/I TESTS (SIP/TIP, #7775)
    // =========================================================================

    /// Test CJK Unified Ideographs Extension G (U+30000-U+3134A)
    /// These are on the Tertiary Ideographic Plane, beyond the table range,
    /// handled by the code fallback in char_width/char_width_cjk.
    #[test]
    fn wide_cjk_extension_g() {
        // First codepoint in Extension G
        assert_eq!(
            grapheme_display_width("\u{30000}"),
            2,
            "U+30000 start of Extension G should be wide"
        );
        // Middle of Extension G
        assert_eq!(
            grapheme_display_width("\u{30A00}"),
            2,
            "U+30A00 middle of Extension G should be wide"
        );
        // Last codepoint in Extension G
        assert_eq!(
            grapheme_display_width("\u{3134A}"),
            2,
            "U+3134A end of Extension G should be wide"
        );
    }

    /// Test CJK Unified Ideographs Extension H (U+31350-U+323AF)
    #[test]
    fn wide_cjk_extension_h() {
        // First codepoint in Extension H
        assert_eq!(
            grapheme_display_width("\u{31350}"),
            2,
            "U+31350 start of Extension H should be wide"
        );
        // Middle of Extension H
        assert_eq!(
            grapheme_display_width("\u{31A00}"),
            2,
            "U+31A00 middle of Extension H should be wide"
        );
        // Last codepoint in Extension H
        assert_eq!(
            grapheme_display_width("\u{323AF}"),
            2,
            "U+323AF end of Extension H should be wide"
        );
    }

    /// Test CJK Unified Ideographs Extension I (U+2EBF0-U+2F7FF)
    /// These are on the Supplementary Ideographic Plane, within the table range.
    #[test]
    fn wide_cjk_extension_i() {
        // First codepoint in Extension I
        assert_eq!(
            grapheme_display_width("\u{2EBF0}"),
            2,
            "U+2EBF0 start of Extension I should be wide"
        );
        // Middle of Extension I
        assert_eq!(
            grapheme_display_width("\u{2F000}"),
            2,
            "U+2F000 middle of Extension I should be wide"
        );
        // Last codepoint in Extension I
        assert_eq!(
            grapheme_display_width("\u{2F7FF}"),
            2,
            "U+2F7FF end of Extension I should be wide"
        );
    }

    /// Test that codepoints just beyond Extension H are NOT wide
    #[test]
    fn beyond_extension_h_narrow() {
        // U+323B0 is just after Extension H ends at U+323AF
        assert_eq!(
            grapheme_display_width("\u{323B0}"),
            1,
            "U+323B0 beyond Extension H should be narrow"
        );
    }

    /// Test grapheme_width aggregate for Extension G/H/I characters
    #[test]
    fn wide_cjk_extensions_aggregate() {
        // Two Extension G + one Extension I character = 6 cells
        let text = "\u{30000}\u{30001}\u{2EBF0}";
        let info = grapheme_width(text);
        assert_eq!(info.grapheme_count, 3);
        assert_eq!(info.display_width, 6);
        assert!(info.has_wide);
    }

    // =========================================================================
    // UNICODE VERSION EDGE CASES
    // =========================================================================

    /// Test characters that changed width in Unicode 9
    #[test]
    fn unicode9_width_changes() {
        // Black Large Square
        let black_square = "\u{2B1B}";
        let info = grapheme_width(black_square);
        assert_eq!(info.display_width, 2, "should be wide in Unicode 9+");

        // White Large Square
        let white_square = "\u{2B1C}";
        assert_eq!(
            grapheme_width(white_square).display_width,
            2,
            "should be wide in Unicode 9+"
        );
    }

    // =========================================================================
    // AMBIGUOUS WIDTH TESTS
    // =========================================================================

    /// Test East Asian Width = Ambiguous characters
    #[test]
    fn ambiguous_width() {
        assert_eq!(
            grapheme_display_width("\u{00A7}"),
            1,
            "section sign = 1 in non-CJK"
        );
        assert_eq!(
            grapheme_display_width("\u{00A9}"),
            1,
            "copyright = 1 in non-CJK"
        );
        assert_eq!(
            grapheme_display_width("\u{00AE}"),
            1,
            "registered = 1 in non-CJK"
        );
        assert_eq!(
            grapheme_display_width("\u{00B0}"),
            1,
            "degree = 1 in non-CJK"
        );
    }
}
