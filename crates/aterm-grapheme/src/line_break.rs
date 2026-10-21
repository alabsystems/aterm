// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! WHERE A LINE OF TEXT MAY BREAK: the one answer every wrap and every
//! word-boundary cut of aterm's chrome reads (Settings' rows and copy, the
//! Messages band's titles and details, the Messages page, notices).
//!
//! The essentials of UAX #14 (Unicode line breaking) these surfaces need:
//!
//! - After a run of white space ([`BreakKind::Space`]). A no-break space
//!   (U+00A0, U+2007, U+202F) is glue, not a place to break.
//! - Between two characters of a script written without spaces between its
//!   words, and between one of them and its neighbour
//!   ([`BreakKind::Ideographic`]): CJK ideographs, kana, Hangul syllables,
//!   Bopomofo, Yi, the fullwidth forms and the CJK symbols and punctuation.
//!   A title that is all CJK has no space to break at, and broken only at
//!   spaces it was left out at a narrow width (or broken anywhere, the
//!   closing `。` starting a line).
//! - Kinsoku, the start and end of line rules: closing punctuation and the
//!   characters that may not start a line (small kana, the prolonged sound
//!   mark, iteration marks, `、。！？`) stay with the character before them,
//!   and opening punctuation (`「（【`) stays with the character after it.
//! - Inside a word, after a hyphen or a slash another character follows
//!   ([`BreakKind::Joint`]): `aarch64-` / `apple-darwin`, and a path at its
//!   folders, but never inside a run of them (`https://`, `--columns`).
//!
//! Text with neither CJK nor a hyphen or slash breaks only at its spaces,
//! exactly as before. Each surface takes the kinds it breaks at: a wrap
//! takes them all, a cut that keeps whole words (the band's title) takes
//! [`BreakKind::Space`] and [`BreakKind::Ideographic`], where a character
//! is a word.
//!
//! Breaks fall only between grapheme clusters, so a mark, a variation
//! selector or a kana's voicing mark stays with its base.

use crate::GraphemeClusters as _;

/// What kind of place a line may break at ([`line_breaks`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// After a run of white space, which ends the line before it and is not
    /// drawn at the start of the next.
    Space,
    /// Between two characters of a script written without spaces between
    /// words (CJK ideographs, kana, Hangul) or between one and its
    /// neighbour, kinsoku kept: nothing goes between the two halves.
    Ideographic,
    /// Inside a word, after a hyphen or a slash: nothing goes between the
    /// two halves.
    Joint,
}

impl BreakKind {
    /// Whether this break keeps whole words: a space, or a character of a
    /// script whose every character is a word's worth (a cut there reads
    /// as words, never as a word cut short).
    #[must_use]
    pub const fn is_word_boundary(self) -> bool {
        matches!(self, Self::Space | Self::Ideographic)
    }
}

/// A place a line may break: the next line would start at byte `at` of the
/// text (never 0 and never the text's end), after a break of `kind`. For a
/// [`BreakKind::Space`] break, the white space before `at` ends the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineBreak {
    pub at: usize,
    pub kind: BreakKind,
}

/// How a grapheme takes part in line breaking, by its first character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// White space a line breaks after.
    Space,
    /// A no-break space: glue.
    Glue,
    /// A character of a script written without spaces (UAX #14 `ID`).
    Ideographic,
    /// CJK closing punctuation and the characters that may not start a line
    /// (UAX #14 `CL`, `CP`, `NS`, `EX` and their fullwidth `IS`): no break
    /// before it.
    Closing,
    /// CJK opening punctuation (UAX #14 `OP`): no break after it.
    Opening,
    /// A hyphen or a slash: a word may break after it.
    Joint,
    /// Anything else: a letter, a digit, Latin punctuation.
    Other,
}

/// The kinsoku table's closing side: may not start a line.
fn is_closing(c: char) -> bool {
    matches!(
        c,
        // CJK punctuation and closing brackets.
        '\u{3001}' // 、
            | '\u{3002}' // 。
            | '\u{3009}' // 〉
            | '\u{300B}' // 》
            | '\u{300D}' // 」
            | '\u{300F}' // 』
            | '\u{3011}' // 】
            | '\u{3015}' // 〕
            | '\u{3017}' // 〗
            | '\u{3019}' // 〙
            | '\u{301B}' // 〛
            | '\u{301C}' // 〜
            | '\u{301E}' // 〞
            | '\u{301F}' // 〟
            // Iteration marks, the prolonged sound mark, the middle dot,
            // the voicing marks.
            | '\u{3005}' // 々
            | '\u{303B}' // 〻
            | '\u{309B}'..='\u{309E}' // ゛゜ゝゞ
            | '\u{30A0}' // ゠
            | '\u{30FB}' // ・
            | '\u{30FC}' // ー
            | '\u{30FD}'..='\u{30FE}' // ヽヾ
            // Small kana.
            | '\u{3041}' | '\u{3043}' | '\u{3045}' | '\u{3047}' | '\u{3049}'
            | '\u{3063}' | '\u{3083}' | '\u{3085}' | '\u{3087}' | '\u{308E}'
            | '\u{3095}' | '\u{3096}'
            | '\u{30A1}' | '\u{30A3}' | '\u{30A5}' | '\u{30A7}' | '\u{30A9}'
            | '\u{30C3}' | '\u{30E3}' | '\u{30E5}' | '\u{30E7}' | '\u{30EE}'
            | '\u{30F5}' | '\u{30F6}'
            | '\u{31F0}'..='\u{31FF}'
            // Vertical and small forms' closers.
            | '\u{FE10}'..='\u{FE16}'
            | '\u{FE18}'
            | '\u{FE36}' | '\u{FE38}' | '\u{FE3A}' | '\u{FE3C}' | '\u{FE3E}'
            | '\u{FE40}' | '\u{FE42}' | '\u{FE44}' | '\u{FE48}'
            | '\u{FE50}'..='\u{FE57}'
            | '\u{FE5A}' | '\u{FE5C}' | '\u{FE5E}'
            // Fullwidth and halfwidth forms.
            | '\u{FF01}' // ！
            | '\u{FF09}' // ）
            | '\u{FF0C}' // ，
            | '\u{FF0E}' // ．
            | '\u{FF1A}' // ：
            | '\u{FF1B}' // ；
            | '\u{FF1F}' // ？
            | '\u{FF3D}' // ］
            | '\u{FF5D}' // ｝
            | '\u{FF60}' // ｠
            | '\u{FF61}' // ｡
            | '\u{FF63}' // ｣
            | '\u{FF64}' // ､
            | '\u{FF65}' // ･
            | '\u{FF67}'..='\u{FF70}' // small halfwidth kana, ｰ
            | '\u{FF9E}'..='\u{FF9F}' // ﾞﾟ
            // Latin punctuation that closes or ends a clause, beside CJK.
            | ')' | ']' | '}' | ',' | '.' | ':' | ';' | '!' | '?' | '%'
            | '\u{2019}' // ’
            | '\u{201D}' // ”
            | '\u{00BB}' // »
            | '\u{2025}' // ‥
            | '\u{2026}' // …
            | '\u{203C}' // ‼
            | '\u{2047}'..='\u{2049}' // ⁇⁈⁉
    )
}

/// The kinsoku table's opening side: may not end a line.
fn is_opening(c: char) -> bool {
    matches!(
        c,
        '\u{3008}' // 〈
            | '\u{300A}' // 《
            | '\u{300C}' // 「
            | '\u{300E}' // 『
            | '\u{3010}' // 【
            | '\u{3014}' // 〔
            | '\u{3016}' // 〖
            | '\u{3018}' // 〘
            | '\u{301A}' // 〚
            | '\u{301D}' // 〝
            | '\u{FE17}'
            | '\u{FE35}' | '\u{FE37}' | '\u{FE39}' | '\u{FE3B}' | '\u{FE3D}'
            | '\u{FE3F}' | '\u{FE41}' | '\u{FE43}' | '\u{FE47}'
            | '\u{FE59}' | '\u{FE5B}' | '\u{FE5D}'
            | '\u{FF08}' // （
            | '\u{FF3B}' // ［
            | '\u{FF5B}' // ｛
            | '\u{FF5F}' // ｟
            | '\u{FF62}' // ｢
            | '(' | '[' | '{'
            | '\u{2018}' // ‘
            | '\u{201C}' // “
            | '\u{00AB}' // «
    )
}

/// A character of a script written without spaces between its words, or
/// CJK punctuation and symbols (UAX #14 `ID`, and the fullwidth forms).
fn is_ideographic(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{11FF}' // Hangul Jamo
            | '\u{2E80}'..='\u{2FFF}' // CJK radicals, Kangxi, description
            | '\u{3001}'..='\u{303F}' // CJK symbols and punctuation
            | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
            | '\u{3100}'..='\u{31FF}' // Bopomofo, compatibility Jamo, Kanbun, strokes
            | '\u{3200}'..='\u{33FF}' // enclosed CJK, compatibility
            | '\u{3400}'..='\u{4DBF}' // CJK extension A
            | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
            | '\u{A000}'..='\u{A4CF}' // Yi
            | '\u{A960}'..='\u{A97F}' // Hangul Jamo extended A
            | '\u{AC00}'..='\u{D7FF}' // Hangul syllables, Jamo extended B
            | '\u{F900}'..='\u{FAFF}' // CJK compatibility ideographs
            | '\u{FE10}'..='\u{FE1F}' // vertical forms
            | '\u{FE30}'..='\u{FE4F}' // CJK compatibility forms
            | '\u{FF01}'..='\u{FF60}' // fullwidth forms
            | '\u{FF61}'..='\u{FFDC}' // halfwidth CJK forms
            | '\u{FFE0}'..='\u{FFE6}' // fullwidth signs
            | '\u{1B000}'..='\u{1B16F}' // kana supplement and extended
            | '\u{20000}'..='\u{3FFFD}' // CJK extensions B on
    )
}

/// White space that is glue: a no-break space keeps its neighbours on one
/// line.
fn is_glue(c: char) -> bool {
    matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}')
}

fn class_of(c: char) -> Class {
    if is_glue(c) {
        Class::Glue
    } else if c.is_whitespace() {
        Class::Space
    } else if is_closing(c) {
        Class::Closing
    } else if is_opening(c) {
        Class::Opening
    } else if is_ideographic(c) {
        Class::Ideographic
    } else if matches!(c, '/' | '-') {
        Class::Joint
    } else {
        Class::Other
    }
}

/// Whether `c` is CJK: an ideograph, kana, Hangul or the CJK and fullwidth
/// punctuation that goes with them. Latin punctuation is never CJK, so it
/// keeps its own rules beside Latin words.
fn is_cjk(c: char) -> bool {
    is_ideographic(c)
}

/// EVERY PLACE `text` MAY BREAK, in order ([`LineBreak`]): after each run
/// of white space, between CJK characters and beside them (kinsoku kept),
/// and inside a word after a hyphen or a slash. Linear in `text`; text with
/// no CJK, hyphen or slash in it has only its space breaks.
#[must_use]
pub fn line_breaks(text: &str) -> Vec<LineBreak> {
    let mut breaks = Vec::new();
    // The previous grapheme: its class and its first character.
    let mut prev: Option<(Class, char)> = None;
    // Whether anything but white space came before: no break at the start.
    let mut seen_text = false;
    for (at, grapheme) in text.grapheme_indices() {
        let Some(first) = grapheme.chars().next() else {
            continue;
        };
        let class = class_of(first);
        let Some((prev_class, prev_char)) = prev else {
            prev = Some((class, first));
            seen_text = class != Class::Space;
            continue;
        };
        if class == Class::Space {
            prev = Some((class, first));
            continue;
        }
        if prev_class == Class::Space {
            // The end of a run of white space. A CJK closer may not start
            // the line even after a space; Latin punctuation keeps the
            // break it always had.
            let closer_after_space = class == Class::Closing && is_cjk(first);
            if seen_text && !closer_after_space {
                breaks.push(LineBreak {
                    at,
                    kind: BreakKind::Space,
                });
            }
            seen_text = true;
            prev = Some((class, first));
            continue;
        }
        seen_text = true;
        let kind = if prev_class == Class::Glue || class == Class::Glue {
            None
        } else if (is_cjk(prev_char) || is_cjk(first))
            && prev_class != Class::Opening
            && class != Class::Closing
        {
            Some(BreakKind::Ideographic)
        } else if prev_class == Class::Joint
            && prev_char != first
            && class != Class::Joint
            && class != Class::Ideographic
        {
            Some(BreakKind::Joint)
        } else {
            None
        };
        if let Some(kind) = kind {
            breaks.push(LineBreak { at, kind });
        }
        prev = Some((class, first));
    }
    breaks
}

/// The places `text` may break that keep whole words
/// ([`BreakKind::is_word_boundary`]): its spaces, and between CJK
/// characters.
#[must_use]
pub fn word_breaks(text: &str) -> Vec<LineBreak> {
    let mut breaks = line_breaks(text);
    breaks.retain(|brk| brk.kind.is_word_boundary());
    breaks
}

/// A piece of a line of text between two places it may break
/// ([`pieces`]): what a greedy wrap lays on a line whole, or carries to the
/// next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece<'a> {
    /// The piece, white space trimmed from both ends: a slice of the text.
    pub text: &'a str,
    /// The byte of the text it starts at.
    pub at: usize,
    /// White space came between it and the piece before: a line that goes
    /// on with it puts a space between them; otherwise nothing goes between
    /// (a CJK character, the rest of a word after its hyphen). Never set on
    /// the first piece.
    pub spaced: bool,
}

/// `text` CUT AT EVERY PLACE A LINE OF IT MAY BREAK OF A KIND `keep` TAKES
/// ([`line_breaks`]), in order, white space trimmed and empty pieces
/// dropped: the one cut every greedy word wrap of aterm's chrome lays its
/// lines from. A wrap that keeps hyphenated words whole takes
/// [`BreakKind::is_word_boundary`]; one that breaks them takes every kind.
/// Text of words and breaking spaces alone cuts exactly as
/// `split_whitespace` does, each piece but the first spaced.
///
/// `keep` is a function, not a generic closure, so the cut is this crate's
/// code wherever it is called: a debug build compiles it optimized (the root
/// `Cargo.toml`'s `[profile.dev.package.aterm-grapheme]`), where a generic
/// one was compiled into each caller at the caller's opt-level.
#[must_use]
pub fn pieces<'a>(text: &'a str, keep: fn(BreakKind) -> bool) -> Vec<Piece<'a>> {
    let mut out: Vec<Piece<'a>> = Vec::new();
    let push = |out: &mut Vec<Piece<'a>>, from: usize, to: usize, spaced: bool| {
        let piece = text[from..to].trim();
        if !piece.is_empty() {
            let at = from + (text[from..to].len() - text[from..to].trim_start().len());
            let spaced = spaced && !out.is_empty();
            out.push(Piece {
                text: piece,
                at,
                spaced,
            });
        }
    };
    let mut from = 0;
    let mut spaced = false;
    for brk in line_breaks(text).into_iter().filter(|brk| keep(brk.kind)) {
        push(&mut out, from, brk.at, spaced);
        from = brk.at;
        spaced = brk.kind == BreakKind::Space;
    }
    push(&mut out, from, text.len(), spaced);
    out
}

/// Whether a line may break after `c` as after white space: white space
/// that is not a no-break space ([`line_breaks`]' glue, U+00A0, U+2007,
/// U+202F). For a wrap that walks the text itself and keeps its white space
/// on the line it ends.
#[must_use]
pub fn is_break_space(c: char) -> bool {
    c.is_whitespace() && !is_glue(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` cut at each break of the kinds `keep` takes: its pieces, white
    /// space trimmed.
    fn pieces(text: &str, keep: fn(BreakKind) -> bool) -> Vec<&str> {
        let mut out = Vec::new();
        let mut from = 0;
        for brk in line_breaks(text).into_iter().filter(|brk| keep(brk.kind)) {
            out.push(text[from..brk.at].trim());
            from = brk.at;
        }
        out.push(text[from..].trim());
        out
    }

    fn all(_: BreakKind) -> bool {
        true
    }

    /// LATIN TEXT BREAKS WHERE IT ALWAYS DID: at its spaces, and inside a
    /// word after a hyphen or a slash another character follows, never
    /// inside a run of them and never at a no-break space.
    #[test]
    fn latin_breaks_at_spaces_hyphens_and_slashes() {
        assert_eq!(
            pieces("Cursor blink was not saved", all),
            ["Cursor", "blink", "was", "not", "saved"]
        );
        assert_eq!(
            pieces("aarch64-apple-darwin ~/a/b", all),
            ["aarch64-", "apple-", "darwin", "~/", "a/", "b"]
        );
        assert_eq!(
            pieces("https://x --columns", all),
            ["https://", "x", "--", "columns"]
        );
        assert_eq!(pieces("50\u{a0}% done", all), ["50\u{a0}%", "done"]);
        assert_eq!(pieces("  lead  and trail  ", all), ["lead", "and", "trail"]);
        assert!(line_breaks("").is_empty());
        assert!(line_breaks("word").is_empty());
        // No break before a Latin closer or after a Latin opener only where
        // a CJK neighbour asks for one; between Latin words nothing moves.
        assert_eq!(pieces("(a) b, c!", all), ["(a)", "b,", "c!"]);
    }

    /// CJK BREAKS BETWEEN ITS CHARACTERS, KINSOKU KEPT: closing punctuation
    /// and the characters that may not start a line stay with the
    /// character before them, opening punctuation with the one after it,
    /// and a kana's voicing mark with its kana.
    #[test]
    fn cjk_breaks_between_characters_with_kinsoku() {
        assert_eq!(
            pieces("設定を保存できません。", all),
            ["設", "定", "を", "保", "存", "で", "き", "ま", "せ", "ん。"]
        );
        assert_eq!(
            pieces("「設定」を開く", all),
            ["「設", "定」", "を", "開", "く"]
        );
        assert_eq!(pieces("ちょっと", all), ["ちょっ", "と"]);
        assert_eq!(pieces("コーヒー", all), ["コー", "ヒー"]);
        assert_eq!(pieces("한국어 설정", all), ["한", "국", "어", "설", "정"]);
        // A decomposed kana and its voicing mark are one grapheme.
        assert_eq!(pieces("か\u{3099}き", all), ["か\u{3099}", "き"]);
        // Beside Latin: either side may break, but not before a closer.
        assert_eq!(pieces("aterm設定。", all), ["aterm", "設", "定。"]);
        // A CJK closer after a space stays on the line before.
        assert_eq!(pieces("設定 。次", all), ["設", "定 。", "次"]);
        // Between a closer and an opener.
        assert_eq!(pieces("終」「始", all), ["終」", "「始"]);
    }

    /// THE PIECES A WRAP LAYS ITS LINES FROM: Latin text of words and spaces
    /// cuts as `split_whitespace` does, each piece after the first spaced; a
    /// CJK run cuts between its characters, kinsoku kept, nothing between
    /// them; a hyphenated word is whole unless the wrap takes its joints; and
    /// each piece is the slice of the text at its own byte.
    #[test]
    fn pieces_are_what_a_wrap_lays_down() {
        fn words(text: &str, keep: fn(BreakKind) -> bool) -> Vec<(bool, &str)> {
            super::pieces(text, keep)
                .into_iter()
                .map(|piece| {
                    assert_eq!(&text[piece.at..piece.at + piece.text.len()], piece.text);
                    (piece.spaced, piece.text)
                })
                .collect()
        }
        for text in [
            "Show the tab bar when only one tab is open",
            "  lead  and\ttrail \n",
            "aarch64-apple-darwin ~/a/b",
            "",
            "word",
        ] {
            let split: Vec<(bool, &str)> = text
                .split_whitespace()
                .enumerate()
                .map(|(n, word)| (n > 0, word))
                .collect();
            assert_eq!(words(text, BreakKind::is_word_boundary), split, "{text:?}");
        }
        assert_eq!(
            words("aarch64-apple x", all),
            [(false, "aarch64-"), (false, "apple"), (true, "x")]
        );
        assert_eq!(
            words("設定「テーマ」を開く。 next", BreakKind::is_word_boundary),
            [
                (false, "設"),
                (false, "定"),
                (false, "「テー"),
                (false, "マ」"),
                (false, "を"),
                (false, "開"),
                (false, "く。"),
                (true, "next"),
            ]
        );
        assert_eq!(
            words("50\u{a0}% done", all),
            [(false, "50\u{a0}%"), (true, "done")]
        );
        assert!(is_break_space(' ') && is_break_space('\u{3000}'));
        assert!(!is_break_space('\u{a0}') && !is_break_space('a'));
    }

    /// A CUT THAT KEEPS WHOLE WORDS takes spaces and CJK characters, never a
    /// hyphen or a slash inside a word.
    #[test]
    fn word_breaks_are_spaces_and_cjk() {
        let words = |text: &str| -> Vec<usize> {
            word_breaks(text).into_iter().map(|brk| brk.at).collect()
        };
        assert_eq!(words("use your-theme now"), [4, 15]);
        assert_eq!(words("設定です"), [3, 6, 9]);
        assert!(word_breaks("a/b-c").is_empty());
    }
}
