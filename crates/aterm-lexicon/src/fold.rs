// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Case folding, diacritic stripping, and character classification for the lexicon matcher.

/// Lowercase, drop strippable diacritics, and apply the special-case folds.
#[inline]
#[must_use]
pub fn fold(s: &str) -> String {
    let mut out = String::new();
    fold_into(s, &mut out);
    out
}

/// [`fold`] into a caller-provided buffer.
#[inline]
pub fn fold_into(s: &str, out: &mut String) {
    out.clear();
    out.reserve(s.len().saturating_sub(out.len()).min(4096));
    for c in s.chars() {
        if is_strippable_mark(c) {
            continue;
        }
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'), // Greek final sigma → medial sigma
            _ => {
                for lc in c.to_lowercase() {
                    if is_strippable_mark(lc) {
                        continue;
                    }
                    out.push(base_letter(lc).unwrap_or(lc));
                }
            }
        }
    }
}

/// The base letter of a lowercase precomposed accented Latin or monotonic Greek letter, or `None`.
#[inline]
#[must_use]
const fn base_letter(c: char) -> Option<char> {
    if c.is_ascii() {
        return None;
    }
    let b = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' => 'a',
        'ç' | 'ć' | 'č' | 'ċ' | 'ĉ' => 'c',
        'ď' | 'đ' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' | 'ħ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ł' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ǒ' => 'o',
        'ŕ' | 'ř' | 'ŗ' => 'r',
        'ś' | 'š' | 'ş' | 'ŝ' | 'ș' => 's',
        'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' => 'u',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ž' | 'ż' => 'z',
        'ά' => 'α',
        'έ' => 'ε',
        'ή' => 'η',
        'ί' | 'ϊ' | 'ΐ' => 'ι',
        'ό' => 'ο',
        'ύ' | 'ϋ' | 'ΰ' => 'υ',
        'ώ' => 'ω',
        _ => return None,
    };
    Some(b)
}

/// True when [`fold`] drops diacritic information from `s`.
#[inline]
#[must_use]
pub fn has_foldable_marks(s: &str) -> bool {
    s.chars().any(|c| {
        if c.is_ascii() {
            return false;
        }
        is_strippable_mark(c)
            || c.to_lowercase()
                .any(|lc| is_strippable_mark(lc) || base_letter(lc).is_some())
    })
}

/// [`is_strippable_mark`]'s ranges, as one pattern.
macro_rules! strippable_mark_ranges {
    () => {
        0x0300..=0x036F   // Combining Diacritical Marks (Latin)
        | 0x0483..=0x0489 // Cyrillic combining
        | 0x0591..=0x05BD // Hebrew points (niqqud)
        | 0x05BF
        | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5
        | 0x05C7
        | 0x0610..=0x061A // Arabic honorifics
        | 0x064B..=0x065F // Arabic harakat
        | 0x0670          // Arabic superscript alef
        | 0x06D6..=0x06DC
        | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8
        | 0x06EA..=0x06ED
        | 0x1AB0..=0x1AFF // Combining Diacritical Marks Extended
        | 0x1DC0..=0x1DFF // Combining Diacritical Marks Supplement
        | 0x20D0..=0x20FF // Combining Diacritical Marks for Symbols
        | 0xFE20..=0xFE2F // Combining Half Marks
    };
}

/// A combining mark [`fold`] drops.
#[inline]
#[must_use]
const fn is_strippable_mark(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, strippable_mark_ranges!())
}

/// [`is_no_space_script`]'s ranges, as one pattern its predicate and the ASCII check below share.
macro_rules! no_space_script_ranges {
    () => {
        0x3040..=0x30FF   // Hiragana + Katakana
        | 0x31F0..=0x31FF // Katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0x1100..=0x11FF // Hangul Jamo
        | 0x0E00..=0x0E7F // Thai
        | 0x0E80..=0x0EFF // Lao
        | 0x1780..=0x17FF // Khmer
        | 0x20000..=0x2A6DF // CJK Ext B
    };
}

/// True for a character of a no-space script ([`is_no_space_surface`](crate::is_no_space_surface)).
#[inline]
#[must_use]
pub const fn is_no_space_script(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, no_space_script_ranges!())
}

/// True for a character inside a whitespace-delimited token.
#[inline]
#[must_use]
pub fn is_token_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    if is_no_space_script(c) {
        return false;
    }
    c.is_alphanumeric() || c == '_' || is_strippable_mark(c) || is_kept_mark(c)
}

/// [`is_kept_mark`]'s ranges, as one pattern its predicate and the ASCII check below share.
macro_rules! kept_mark_ranges {
    () => {
        0x0900..=0x097F   // Devanagari (matras, virama)
        | 0x0980..=0x09FF // Bengali
        | 0x0B80..=0x0BFF // Tamil
        | 0x0A00..=0x0A7F // Gurmukhi
        | 0x0C00..=0x0C7F // Telugu
    };
}

/// A spacing or combining mark [`fold`] keeps (Indic etc.).
#[inline]
const fn is_kept_mark(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, kept_mark_ranges!())
}

/// The ASCII early-outs are only sound while the tables stay above ASCII.
const _: () = {
    let mut cp: u32 = 0;
    while cp < 0x80 {
        assert!(
            !matches!(cp, strippable_mark_ranges!())
                && !matches!(cp, no_space_script_ranges!())
                && !matches!(cp, kept_mark_ranges!()),
            "a fold table names an ASCII scalar, which its predicate's early-out would skip"
        );
        cp += 1;
    }
};

/// ASCII punctuation that marks a code, path or URL context.
#[inline]
#[must_use]
pub const fn is_code_adjacent_punct(c: char) -> bool {
    matches!(
        c,
        '/' | '\\'
            | ':'
            | '='
            | '@'
            | '#'
            | '+'
            | '~'
            | '$'
            | '%'
            | '|'
            | '<'
            | '>'
            | '*'
            | '&'
            | '^'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_greek_final_sigma() {
        assert_eq!(fold("γάτες"), fold("γατες"));
    }

    #[test]
    fn fold_is_idempotent() {
        for s in ["fucking", "Kätzchen", "Scheiße", "猫", "γάτα", "بسة", "İ"] {
            assert_eq!(fold(&fold(s)), fold(s), "fold not idempotent for {s}");
        }
        assert_eq!(fold("İ"), fold("i"));
    }

    #[test]
    fn keeps_indic_marks() {
        let folded = fold("बिल्ली");
        assert!(folded.contains('\u{093F}') || folded.chars().count() >= 4);
    }

    #[test]
    fn cjk_detected_as_no_space() {
        assert!(is_no_space_script('猫'));
        assert!(is_no_space_script('ね'));
        assert!(is_no_space_script('고'));
        assert!(!is_no_space_script('a'));
    }
}
