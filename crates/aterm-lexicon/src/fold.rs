// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Case folding, diacritic stripping, and character classification used by the
//! matcher and the lexicon builder.
//!
//! There is intentionally NO dependency on a full Unicode normalization crate
//! (that would violate aterm's vendoring norm). Instead we define a small,
//! auditable contract:
//!
//! * **Case** — `char::to_lowercase` (locale-independent simple lowering) plus a
//!   handful of special cases (`ß`/`ẞ` → `ss`, Greek final sigma `ς` → `σ`).
//! * **Diacritics** — combining marks in the Latin / Greek / Cyrillic / Arabic /
//!   Hebrew ranges are *dropped*, so terminal text emitted as base+combining
//!   (e.g. `e` + U+0301) folds identically to a precomposed `é`, and Arabic
//!   harakat / Hebrew niqqud (usually omitted when typing) never block a match.
//!   Indic / Thai / other *spacing* combining marks are NOT stripped — they are
//!   integral to the syllable and removing them would corrupt the word.
//!
//! Both the scanned token and every lexicon key are folded through the SAME
//! function, so the "compare in one normal form" invariant is enforced by
//! construction, not assumed. See the `fold_is_idempotent` test.

/// Lowercase, drop strippable diacritics, and apply the special-case folds.
/// The result is the canonical surface used for every lexicon lookup.
#[must_use]
pub fn fold(s: &str) -> String {
    let mut out = String::new();
    fold_into(s, &mut out);
    out
}

/// [`fold`] writing into a caller-provided buffer (cleared first) so a hot
/// per-token scanner reuses a single allocation instead of allocating one
/// folded `String` per probed token. [`fold`] is the thin owning wrapper.
///
/// Public because the host's context tokenizer (the sparkle-words genome's ±K
/// neighbor-token walk, which must be allocation-free on resident scratch)
/// folds candidate tokens through the SAME function as the scanner — a
/// re-implemented look-alike fold would drift.
pub fn fold_into(s: &str, out: &mut String) {
    out.clear();
    // `clear()` left `out.len() == 0`, so this reserves exactly `s.len()` —
    // identical behavior; the saturating form keeps `len + additional` provably
    // in range for the verifier, which cannot see `clear`'s effect on `len`.
    // The reserve is only a pre-allocation hint (push grows amortized), so the
    // clamp bounds the up-front allocation the verifier must budget for; probed
    // tokens are far shorter than the clamp, making this a no-op in practice.
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
                    // Lowercasing can itself emit a combining mark (e.g. Turkish
                    // 'İ' → 'i' + U+0307); drop those too so `fold` is idempotent.
                    if is_strippable_mark(lc) {
                        continue;
                    }
                    // Map a precomposed Latin/Greek accented letter to its base
                    // so it folds identically to the decomposed (base+combining)
                    // form the terminal emits.
                    out.push(base_letter(lc).unwrap_or(lc));
                }
            }
        }
    }
}

/// Map a (lowercase) precomposed accented Latin or Greek letter to its base
/// letter. Returns `None` for characters that need no accent folding. Covers the
/// Latin-1 / Latin Extended-A letters and Greek monotonic-accented vowels that
/// appear in the lexicon's languages. Cyrillic is intentionally left untouched
/// (its accented letters are distinct phonemes, not decorations).
#[must_use]
fn base_letter(c: char) -> Option<char> {
    // Every arm below matches an ACCENTED letter, all of them above ASCII (the
    // ASCII letters in this table are the results, not the patterns). So an
    // ASCII input has no accent to fold and needs none of the walk.
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
        // Greek monotonic accented vowels → base vowel.
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

/// True when `s` carries diacritic information that [`fold`] DROPS: a
/// strippable combining mark, or a precomposed accented letter that folds to
/// its bare base (`đ` → `d`, `ç` → `c`, `ä` → `a`, Turkish `ı` → `i`).
///
/// The `ß`→`ss` and final-sigma folds are NOT mark loss — they are canonical
/// respellings whose result spells the same word — so they do not count.
///
/// This is the witness the scanner's marks-required gate keys on: a lexicon
/// surface that loses marks in folding (Vietnamese `đm` → `dm`, whose entry
/// says the tone marks are essential) compiles to a bare-ASCII key that plain
/// ASCII typing would otherwise complete (`dm` mid-`dmg` fired the curse
/// bonk, 2026-08-29). Such a surface only matches a token that itself folds
/// marks away — `đm`, `ĐM`, or a decomposed `d` + combining stroke — never
/// the bare skeleton.
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

/// A combining mark we DROP during folding (a non-spacing diacritic in a script
/// whose base letters are written separately). Restricted to the ranges where
/// stripping is safe: Latin, Greek, Cyrillic, Arabic harakat, Hebrew niqqud.
/// Deliberately excludes Indic / Thai / SE-Asian spacing marks (category Mc),
/// which are part of the syllable.
#[must_use]
fn is_strippable_mark(c: char) -> bool {
    // ASCII cannot be a combining mark, and the ranges below say so: the lowest
    // starts at U+0300. Terminal text is overwhelmingly ASCII and this runs per
    // character of every folded token, so the whole list was being walked to
    // return `false`.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32,
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
    )
}

/// True for a character that belongs to a *no-space* script (CJK ideographs,
/// kana, Hangul, Thai, Lao, Khmer, …). Such scripts are matched by the maximal
/// run scanner rather than the whitespace tokenizer, because they write words
/// without separators.
///
/// Public (v3 §6): the sparkle-words custom-spec resolver must classify a
/// user surface as spaced vs no-space with the SAME predicate the scanner
/// uses (a spaced entry containing a CJK surface silently never matches, so
/// the resolver emits it as a `cjk = true` entry instead).
#[must_use]
pub fn is_no_space_script(c: char) -> bool {
    // No ASCII scalar is in a no-space script — the lowest range here starts at
    // U+0E00 (Thai). This predicate opens `is_token_char`, so it is the FIRST
    // thing every scanned character meets.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32,
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
    )
}

/// True for a character that can sit *inside* a whitespace-delimited token: a
/// letter, a decimal digit, a connector (`_`), or a non-spacing mark we keep.
/// Digits are token-internal so that identifiers like `cat5` do not tokenize as
/// the word `cat`.
///
/// Public because the host's context tokenizer (the ±K neighbor-token walk that
/// feeds the sparkle-words genome) MUST use the same predicate as the scanner —
/// a re-implemented look-alike would drift.
#[must_use]
pub fn is_token_char(c: char) -> bool {
    // The ASCII answer in one branch. For an ASCII scalar `is_no_space_script`,
    // `is_strippable_mark` and `is_kept_mark` are all provably false (their
    // lowest ranges are U+0E00, U+0300 and U+0900), and `char::is_alphanumeric`
    // — a Unicode table lookup — agrees exactly with `is_ascii_alphanumeric`
    // over ASCII. So the whole predicate collapses to two comparisons.
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    if is_no_space_script(c) {
        return false;
    }
    c.is_alphanumeric() || c == '_' || is_strippable_mark(c) || is_kept_mark(c)
}

/// A spacing/combining mark we KEEP (Indic / Thai etc.) — these are part of the
/// token but are not stripped by `fold`.
fn is_kept_mark(c: char) -> bool {
    // Same argument, one range further up: the lowest here is U+0900.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32,
        0x0900..=0x097F   // Devanagari (matras, virama)
        | 0x0980..=0x09FF // Bengali
        | 0x0B80..=0x0BFF // Tamil
        | 0x0A00..=0x0A7F // Gurmukhi
        | 0x0C00..=0x0C7F // Telugu
    )
}

/// ASCII punctuation that signals a *code / path / URL* context rather than
/// prose. A whole-word match immediately adjacent to one of these is suppressed
/// (`cat.txt`, `/api/cat`, `--cat`, `cat=1`, `$cat`).
#[must_use]
pub(crate) fn is_code_adjacent_punct(c: char) -> bool {
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
    fn fold_lowercases_ascii() {
        assert_eq!(fold("FuCKing"), "fucking");
    }

    #[test]
    fn fold_eszett_to_ss() {
        assert_eq!(fold("Scheiße"), "scheisse");
        assert_eq!(fold("SCHEIẞE"), "scheisse");
    }

    #[test]
    fn fold_strips_latin_combining() {
        // "Kätzchen" written decomposed: K a +U+0308 t z c h e n
        let decomposed = "Ka\u{0308}tzchen";
        let precomposed = "Kätzchen";
        assert_eq!(fold(decomposed), fold(precomposed));
        assert_eq!(fold(decomposed), "katzchen");
    }

    #[test]
    fn fold_greek_final_sigma() {
        // ΓΑΤΕΣ vs γάτες should both collapse the final sigma
        assert_eq!(fold("γάτες"), fold("γατες"));
    }

    #[test]
    fn fold_is_idempotent() {
        for s in ["fucking", "Kätzchen", "Scheiße", "猫", "γάτα", "بسة"] {
            assert_eq!(fold(&fold(s)), fold(s), "fold not idempotent for {s}");
        }
    }

    #[test]
    fn keeps_indic_marks() {
        // Devanagari बिल्ली must retain its matras (not be stripped to बलल)
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

    /// THE EARLY-OUTS ARE ONLY SOUND WHILE THE TABLES STAY ABOVE ASCII, and the
    /// hazard is not today — it is the day someone adds an ASCII codepoint to
    /// one of these tables and the early-out silently shadows it.
    ///
    /// Asserting "every ASCII scalar answers false" would be CIRCULAR: the
    /// early-out is what makes it true. So this reads the source of the three
    /// range tables instead and pins the structural fact the fast paths rest
    /// on — no scalar below U+0080 is named in any of them. A table that grows
    /// an ASCII entry fails here rather than going quietly unreachable.
    #[test]
    fn every_range_the_ascii_early_outs_skip_lies_above_ascii() {
        let src = include_str!("fold.rs");
        for name in [
            "fn is_strippable_mark",
            "fn is_kept_mark",
            "pub fn is_no_space_script",
        ] {
            let start = src
                .find(name)
                .unwrap_or_else(|| panic!("{name} moved or was renamed"));
            let body = &src[start..];
            let end = body.find("\n}").expect("function has a closing brace");
            let body = &body[..end];
            let mut seen = 0usize;
            let mut rest = body;
            while let Some(at) = rest.find("0x") {
                rest = &rest[at + 2..];
                let hex: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
                if hex.is_empty() {
                    continue;
                }
                let cp = u32::from_str_radix(&hex, 16).expect("hex literal parses");
                assert!(
                    cp >= 0x80,
                    "{name} names U+{cp:04X}, which is ASCII — the `c.is_ascii()` \
                     early-out would skip it and this predicate would silently \
                     stop matching it",
                );
                seen += 1;
            }
            assert!(
                seen >= 5,
                "{name}: found only {seen} codepoints — did the scan break?"
            );
        }
    }
}
