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
    // After `clear()` this reserves `s.len()`; the saturating form keeps
    // `len + additional` provably in range for the verifier, which cannot see
    // `clear`'s effect on `len`. The reserve is only a hint, and the clamp
    // bounds the up-front allocation the verifier must budget for.
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
/// ASCII typing would otherwise complete (`dm` mid-`dmg`). Such a surface
/// only matches a token that itself folds marks away — `đm`, `ĐM`, or a
/// decomposed `d` + combining stroke — never the bare skeleton.
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

/// [`is_strippable_mark`]'s ranges, as one pattern: the predicate and the
/// build's ASCII check below match the same tokens.
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

/// A combining mark we DROP during folding (a non-spacing diacritic in a script
/// whose base letters are written separately). Restricted to the ranges where
/// stripping is safe: Latin, Greek, Cyrillic, Arabic harakat, Hebrew niqqud.
/// Deliberately excludes Indic / Thai / SE-Asian spacing marks (category Mc),
/// which are part of the syllable.
#[must_use]
fn is_strippable_mark(c: char) -> bool {
    // ASCII cannot be a combining mark, and the table says so: its lowest range
    // starts at U+0300 (the build checks it, below the tables). Terminal text
    // is overwhelmingly ASCII and this runs per character of every folded
    // token.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, strippable_mark_ranges!())
}

/// [`is_no_space_script`]'s ranges, as one pattern (see
/// `strippable_mark_ranges`).
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
    // No ASCII scalar is in a no-space script — the table's lowest range starts
    // at U+0E00 (Thai; the build checks it). This predicate opens
    // `is_token_char`, so it is the FIRST thing every scanned character meets.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, no_space_script_ranges!())
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

/// [`is_kept_mark`]'s ranges, as one pattern (see `strippable_mark_ranges`).
macro_rules! kept_mark_ranges {
    () => {
        0x0900..=0x097F   // Devanagari (matras, virama)
        | 0x0980..=0x09FF // Bengali
        | 0x0B80..=0x0BFF // Tamil
        | 0x0A00..=0x0A7F // Gurmukhi
        | 0x0C00..=0x0C7F // Telugu
    };
}

/// A spacing/combining mark we KEEP (Indic / Thai etc.) — these are part of the
/// token but are not stripped by `fold`.
fn is_kept_mark(c: char) -> bool {
    // Same argument, one range further up: the lowest here is U+0900.
    if c.is_ascii() {
        return false;
    }
    matches!(c as u32, kept_mark_ranges!())
}

/// THE ASCII EARLY-OUTS ARE ONLY SOUND WHILE THE TABLES STAY ABOVE ASCII, and
/// the hazard is the day someone adds an ASCII codepoint to one of them and the
/// early-out silently shadows it. Asserting "every ASCII scalar answers false"
/// of the predicates would be circular (the early-out is what makes it true),
/// so the build asks the TABLES, early-outs aside, about all 128 ASCII scalars:
/// a table that grows an ASCII entry stops the compile. The predicates match
/// the same pattern macros inline, so they call nothing (no obligation the
/// Trust lane would have to discharge through a callee).
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

/// ASCII punctuation that signals a *code / path / URL* context rather than
/// prose. A whole-word match immediately adjacent to one of these is suppressed
/// (`cat.txt`, `/api/cat`, `--cat`, `cat=1`, `$cat`).
///
/// Public for the same reason as [`is_token_char`]: the typed-line kitty-command
/// listener judges the context of a TYPED token (`./sit`, `sit=3`, `$play`
/// must never move the pet), and it has to draw the code/prose line exactly
/// where the screen scanner draws it. A re-implemented look-alike set would
/// drift, and the two would disagree about the same word on the same row.
#[must_use]
pub fn is_code_adjacent_punct(c: char) -> bool {
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
        // ΓΑΤΕΣ vs γάτες should both collapse the final sigma
        assert_eq!(fold("γάτες"), fold("γατες"));
    }

    #[test]
    fn fold_is_idempotent() {
        for s in ["fucking", "Kätzchen", "Scheiße", "猫", "γάτα", "بسة", "İ"] {
            assert_eq!(fold(&fold(s)), fold(s), "fold not idempotent for {s}");
        }
        // 'İ'.to_lowercase() emits 'i' + a combining dot; fold drops the dot.
        assert_eq!(fold("İ"), fold("i"));
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
}
