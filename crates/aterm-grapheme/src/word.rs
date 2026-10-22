// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Word boundaries shared by double-click selection, find-field word motions,
//! and native editor word motions.
//!
//! The caller supplies the base word-character predicate ([`is_word_char`] by
//! default, or non-separator under a configured separator set). Zero-width
//! characters are placed as follows:
//!
//! - Marks (`Grapheme_Cluster_Break` Extend/SpacingMark or other non-control
//!   zero-width chars) attach to the preceding character's word, and to no word
//!   after a separator.
//! - Prepended marks (`Grapheme_Cluster_Break=Prepend`) attach to the following
//!   word character.
//! - Format controls that spell a word belong to it only in context: ZWNJ/ZWJ
//!   next to a virama, between two cursive-joining letters, or (for ZWJ) between
//!   `Extended_Pictographic` characters; and script-specific format controls
//!   ([`SCRIPT_CONTROLS`]) between two letters of their script.
//! - Every other zero-width control ends a word under any classifier; visible
//!   controls (`char_width > 0`) are classified as base characters.

use std::ops::Range;

use crate::emoji_presentation::in_ranges;
use crate::tables::char_width;
use crate::tables::gcb::{GCB, gcb_class};

/// Zero width non-joiner.
const ZWNJ: char = '\u{200C}';
/// Zero width joiner.
const ZWJ: char = '\u{200D}';

/// The default word character: a letter or digit of any script, or `_` (the
/// readline and xterm convention).
#[must_use]
#[inline]
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The words of `text`, in order, as byte ranges.
///
/// `is_word` says which characters are word characters on their own; a word
/// is a maximal run of those plus the characters that belong to them: the
/// marks after a word character, the prepended marks before one, and a format
/// control where it spells the word (a ZWNJ or ZWJ next to a virama, between
/// two letters of a cursive-joining script or inside an emoji ZWJ sequence; a
/// script's own format controls between two of its letters). Every other
/// zero-width control, and a joiner anywhere else, is in no word, whatever
/// `is_word` says of it.
///
/// One pass, linear in `text`'s length: a run of prepended marks or of a
/// script's format controls is read once, and a joiner looks ahead only over
/// the marks right after it, so no character is classified more than a few
/// times. A range starts and ends on character boundaries, and its first
/// character is never a mark or a format control.
#[inline]
pub const fn words<F: Fn(char) -> bool>(text: &str, is_word: F) -> Words<'_, F> {
    Words {
        text,
        is_word,
        at: 0,
        prev: None,
        prev_in: false,
        letter: None,
    }
}

/// The byte offset of the last character before `position` that ends every
/// word around it (a zero-width control, or a character `is_word` rejects
/// that is no mark, prepended mark or format control that can spell a word),
/// or 0 if there is none. `position` is floored onto a character boundary.
///
/// No word spans that character and the walk carries nothing past it, so the
/// words of `&text[at..]`, shifted by `at`, are exactly the words of `text`
/// that start at or after `at`, every word that ends after `position` among
/// them: a caret's next words are read from there, not from the start of the
/// line. Reads back only as far as that character.
#[must_use]
pub fn word_break_before<F: Fn(char) -> bool>(text: &str, position: usize, is_word: F) -> usize {
    text.get(..floor_char_boundary(text, position))
        .unwrap_or_default()
        .char_indices()
        .rev()
        .find(|&(_, c)| match role(c) {
            Role::Break => true,
            Role::Base => !is_word(c),
            Role::Mark | Role::Prepend | Role::Joiner => false,
        })
        .map_or(0, |(at, _)| at)
}

/// The start of the last word of `text` that starts before `position`, or
/// `None` if no word does: where a caret's word-left motion lands.
///
/// Reads back only to the word break before that word, and forward only to
/// `position`, so a caret's motion costs the distance it moves.
#[must_use]
pub fn word_start_before<F: Fn(char) -> bool>(
    text: &str,
    position: usize,
    is_word: F,
) -> Option<usize> {
    let mut end = floor_char_boundary(text, position);
    loop {
        let from = word_break_before(text, end, &is_word);
        let mut walk = words(text.get(from..)?, &is_word);
        let mut start = None;
        while from.saturating_add(walk.at) < end {
            let at = walk.at;
            let was_in = walk.prev_in;
            match walk.step() {
                Some(true) if !was_in => start = Some(from.saturating_add(at)),
                Some(_) => {}
                None => break,
            }
        }
        if start.is_some() || from == 0 {
            return start;
        }
        end = from;
    }
}

/// The largest character boundary of `text` at or before `position`.
#[inline]
const fn floor_char_boundary(text: &str, position: usize) -> usize {
    let mut at = if position < text.len() {
        position
    } else {
        text.len()
    };
    while !text.is_char_boundary(at) {
        at = at.saturating_sub(1);
    }
    at
}

/// The iterator [`words`] returns.
#[derive(Clone)]
pub struct Words<'t, F> {
    text: &'t str,
    is_word: F,
    /// The byte the next step classifies.
    at: usize,
    /// The character before `at`.
    prev: Option<char>,
    /// Whether the character before `at` is in a word.
    prev_in: bool,
    /// The last letter before `at`, marks and the controls that spell its word
    /// passed over, while it is in a word.
    letter: Option<char>,
}

impl<F: Fn(char) -> bool> Words<'_, F> {
    /// Whether `c`, standing on its own, is a word character.
    #[inline]
    fn is_word_base(&self, c: char) -> bool {
        role(c) == Role::Base && (self.is_word)(c)
    }

    /// Classify the next unit at `self.at` (one character, or a run of
    /// prepended marks or of one script's format controls, which share one
    /// fate): whether it is in a word. `None` at the end of the text.
    fn step(&mut self) -> Option<bool> {
        let mut rest = self.text.get(self.at..)?.chars();
        let c = rest.next()?;
        let mut end = self.at.saturating_add(c.len_utf8());
        let mut last = c;
        let role_of_c = role(c);
        let in_word = match role_of_c {
            Role::Base => {
                let word = (self.is_word)(c);
                self.letter = word.then_some(c);
                word
            }
            Role::Mark => self.prev_in,
            Role::Break => false,
            Role::Prepend => {
                let mut after = None;
                for n in rest {
                    if role(n) != Role::Prepend {
                        after = Some(n);
                        break;
                    }
                    end = end.saturating_add(n.len_utf8());
                    last = n;
                }
                after.is_some_and(|n| self.is_word_base(n))
            }
            Role::Joiner => match script_controls(c) {
                Some(script) => {
                    let mut after = None;
                    for n in rest {
                        if !script.has_control(n) {
                            after = Some(n);
                            break;
                        }
                        end = end.saturating_add(n.len_utf8());
                        last = n;
                    }
                    self.prev_in
                        && self.letter.is_some_and(|l| script.has_letter(l))
                        && after.is_some_and(|n| script.has_letter(n) && self.is_word_base(n))
                }
                None => self.prev_in && self.spells_word(c, rest),
            },
        };
        if !in_word || matches!(role_of_c, Role::Break | Role::Prepend) {
            self.letter = None;
        }
        self.prev = Some(last);
        self.prev_in = in_word;
        self.at = end;
        Some(in_word)
    }

    /// Whether `joiner` (ZWNJ or ZWJ), after a character in a word and before
    /// `after`, spells that word rather than standing between two words.
    fn spells_word(&self, joiner: char, mut after: std::str::Chars<'_>) -> bool {
        let Some(next) = after.clone().next() else {
            return false;
        };
        let next_role = role(next);
        let continues =
            next_role == Role::Mark || (next_role == Role::Base && (self.is_word)(next));
        if !continues {
            return false;
        }
        if self.prev.is_some_and(is_virama) || is_virama(next) {
            return true;
        }
        // The letter after the joiner, the marks after it passed over.
        let letter = if next_role == Role::Base {
            Some(next)
        } else {
            after
                .find(|&n| role(n) != Role::Mark)
                .filter(|&n| self.is_word_base(n))
        };
        let (Some(before), Some(after)) = (self.letter, letter) else {
            return false;
        };
        (joins_cursively(before) && joins_cursively(after))
            || (joiner == ZWJ && is_pictographic(before) && is_pictographic(after))
    }
}

impl<F: Fn(char) -> bool> Iterator for Words<'_, F> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let mut start = None;
        loop {
            let from = self.at;
            let Some(in_word) = self.step() else {
                return start.map(|start| start..self.at);
            };
            match (start, in_word) {
                (None, true) => start = Some(from),
                (Some(start), false) => return Some(start..from),
                _ => {}
            }
        }
    }
}

/// How a character takes part in a word before its neighbours are known.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Stands on its own: in a word exactly when the caller's classifier says.
    Base,
    /// Belongs to the character before it.
    Mark,
    /// Belongs to the character after it (UAX #29 GB9b).
    Prepend,
    /// A format control that can spell a word (ZWNJ, ZWJ, a script's own
    /// controls): in a word only where it spells one.
    Joiner,
    /// A zero-width control that ends any word.
    Break,
}

fn role(c: char) -> Role {
    #[cfg(test)]
    ROLE_LOOKUPS.with(|n| n.set(n.get().saturating_add(1)));
    if matches!(c, ZWNJ | ZWJ) || script_controls(c).is_some() {
        return Role::Joiner;
    }
    match gcb_class(c) {
        GCB::Prepend => Role::Prepend,
        GCB::Extend | GCB::SpacingMark => Role::Mark,
        GCB::Control | GCB::CR | GCB::LF if char_width(c) == 0 => Role::Break,
        GCB::Control | GCB::CR | GCB::LF => Role::Base,
        _ if char_width(c) == 0 => Role::Mark,
        _ => Role::Base,
    }
}

/// A script that spells words with format controls of its own: the controls,
/// and the code points its letters are drawn from (inclusive ranges).
struct ScriptControls {
    controls: (u32, u32),
    letters: (u32, u32),
}

impl ScriptControls {
    #[inline]
    const fn has_control(&self, c: char) -> bool {
        let cp = c as u32;
        self.controls.0 <= cp && cp <= self.controls.1
    }

    #[inline]
    const fn has_letter(&self, c: char) -> bool {
        let cp = c as u32;
        self.letters.0 <= cp && cp <= self.letters.1
    }
}

/// The scripts whose own format controls belong to a word between two of
/// their letters, with the blocks those letters are in (Unicode 16.0.0
/// `Blocks.txt`; block ranges never move).
static SCRIPT_CONTROLS: [ScriptControls; 3] = [
    // MONGOLIAN VOWEL SEPARATOR, in the Mongolian block. It was a space (Zs)
    // before Unicode 6.3, so between letters of another script it still
    // separates them.
    ScriptControls {
        controls: (0x180E, 0x180E),
        letters: (0x1800, 0x18AF),
    },
    // The Egyptian hieroglyph format controls (joiners, insertions, segments,
    // enclosures), between the signs of the Egyptian Hieroglyphs, Egyptian
    // Hieroglyph Format Controls and Egyptian Hieroglyphs Extended-A blocks.
    ScriptControls {
        controls: (0x13430, 0x1343F),
        letters: (0x13000, 0x143FF),
    },
    // The shorthand format controls (letter overlap, continuing overlap, down
    // step, up step), between Duployan letters.
    ScriptControls {
        controls: (0x1BCA0, 0x1BCA3),
        letters: (0x1BC00, 0x1BC9F),
    },
];

/// The script `c` is one of the own format controls of, if any.
#[inline]
fn script_controls(c: char) -> Option<&'static ScriptControls> {
    SCRIPT_CONTROLS.iter().find(|script| script.has_control(c))
}

/// Whether `c` is `Extended_Pictographic`, an element an emoji ZWJ sequence
/// joins.
#[inline]
fn is_pictographic(c: char) -> bool {
    gcb_class(c) == GCB::Extended_Pictographic
}

/// Whether `c` is a virama (`Canonical_Combining_Class=9`, Virama), the sign
/// that kills a consonant's vowel and that a joiner after or before it turns
/// into a half form, an eyelash ra or a ya-phala.
#[inline]
fn is_virama(c: char) -> bool {
    in_ranges(VIRAMA, c as u32)
}

/// Whether `c` joins its neighbours cursively (`Joining_Type` Dual, Left,
/// Right or Join_Causing): the letters of Arabic, Syriac, N'Ko, Mongolian and
/// the other cursive scripts, between which ZWNJ and ZWJ decide whether two
/// letters join.
#[inline]
fn joins_cursively(c: char) -> bool {
    in_ranges(JOINING, c as u32)
}

/// `Canonical_Combining_Class=9` (Virama) ranges, inclusive `(lo, hi)`, sorted
/// and disjoint.
///
/// Source: Unicode 16.0.0 `DerivedCombiningClass.txt`, the UCD version of the
/// crate's other tables, range-merged. The tests below check it against that
/// file, every code point; re-extract from the matching UCD release to update.
static VIRAMA: &[(u32, u32)] = &[
    (0x094D, 0x094D),
    (0x09CD, 0x09CD),
    (0x0A4D, 0x0A4D),
    (0x0ACD, 0x0ACD),
    (0x0B4D, 0x0B4D),
    (0x0BCD, 0x0BCD),
    (0x0C4D, 0x0C4D),
    (0x0CCD, 0x0CCD),
    (0x0D3B, 0x0D3C),
    (0x0D4D, 0x0D4D),
    (0x0DCA, 0x0DCA),
    (0x0E3A, 0x0E3A),
    (0x0EBA, 0x0EBA),
    (0x0F84, 0x0F84),
    (0x1039, 0x103A),
    (0x1714, 0x1715),
    (0x1734, 0x1734),
    (0x17D2, 0x17D2),
    (0x1A60, 0x1A60),
    (0x1B44, 0x1B44),
    (0x1BAA, 0x1BAB),
    (0x1BF2, 0x1BF3),
    (0x2D7F, 0x2D7F),
    (0xA806, 0xA806),
    (0xA82C, 0xA82C),
    (0xA8C4, 0xA8C4),
    (0xA953, 0xA953),
    (0xA9C0, 0xA9C0),
    (0xAAF6, 0xAAF6),
    (0xABED, 0xABED),
    (0x10A3F, 0x10A3F),
    (0x11046, 0x11046),
    (0x11070, 0x11070),
    (0x1107F, 0x1107F),
    (0x110B9, 0x110B9),
    (0x11133, 0x11134),
    (0x111C0, 0x111C0),
    (0x11235, 0x11235),
    (0x112EA, 0x112EA),
    (0x1134D, 0x1134D),
    (0x113CE, 0x113D0),
    (0x11442, 0x11442),
    (0x114C2, 0x114C2),
    (0x115BF, 0x115BF),
    (0x1163F, 0x1163F),
    (0x116B6, 0x116B6),
    (0x1172B, 0x1172B),
    (0x11839, 0x11839),
    (0x1193D, 0x1193E),
    (0x119E0, 0x119E0),
    (0x11A34, 0x11A34),
    (0x11A47, 0x11A47),
    (0x11A99, 0x11A99),
    (0x11C3F, 0x11C3F),
    (0x11D44, 0x11D45),
    (0x11D97, 0x11D97),
    (0x11F41, 0x11F42),
    (0x1612F, 0x1612F),
];

/// `Joining_Type` Dual_Joining, Left_Joining, Right_Joining and Join_Causing
/// ranges, inclusive `(lo, hi)`, sorted and disjoint.
///
/// Source: Unicode 16.0.0 `DerivedJoiningType.txt`, range-merged and checked
/// against that file the same way.
static JOINING: &[(u32, u32)] = &[
    (0x0620, 0x0620),
    (0x0622, 0x064A),
    (0x066E, 0x066F),
    (0x0671, 0x0673),
    (0x0675, 0x06D3),
    (0x06D5, 0x06D5),
    (0x06EE, 0x06EF),
    (0x06FA, 0x06FC),
    (0x06FF, 0x06FF),
    (0x0710, 0x0710),
    (0x0712, 0x072F),
    (0x074D, 0x077F),
    (0x07CA, 0x07EA),
    (0x07FA, 0x07FA),
    (0x0840, 0x0858),
    (0x0860, 0x0860),
    (0x0862, 0x0865),
    (0x0867, 0x086A),
    (0x0870, 0x0886),
    (0x0889, 0x088E),
    (0x08A0, 0x08AC),
    (0x08AE, 0x08C8),
    (0x1807, 0x1807),
    (0x180A, 0x180A),
    (0x1820, 0x1878),
    (0x1887, 0x18A8),
    (0x18AA, 0x18AA),
    (0x200D, 0x200D),
    (0xA840, 0xA872),
    (0x10AC0, 0x10AC5),
    (0x10AC7, 0x10AC7),
    (0x10AC9, 0x10ACA),
    (0x10ACD, 0x10AE1),
    (0x10AE4, 0x10AE4),
    (0x10AEB, 0x10AEF),
    (0x10B80, 0x10B91),
    (0x10BA9, 0x10BAE),
    (0x10D00, 0x10D23),
    (0x10EC2, 0x10EC4),
    (0x10F30, 0x10F44),
    (0x10F51, 0x10F54),
    (0x10F70, 0x10F81),
    (0x10FB0, 0x10FB0),
    (0x10FB2, 0x10FB6),
    (0x10FB8, 0x10FBF),
    (0x10FC1, 0x10FC4),
    (0x10FC9, 0x10FCB),
    (0x1E900, 0x1E943),
];

#[cfg(test)]
thread_local! {
    /// Test seam: how many characters [`role`] has classified on this thread.
    static ROLE_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The code points a UCD `Derived*.txt` file gives one of `values`.
    fn ucd(data: &str, values: &[&str]) -> Vec<bool> {
        let mut set = vec![false; 0x11_0000];
        for line in data.lines() {
            let content = line.split('#').next().unwrap_or("");
            let Some((range, value)) = content.split_once(';') else {
                continue;
            };
            if !values.contains(&value.trim()) {
                continue;
            }
            let range = range.trim();
            let (lo, hi) = range.split_once("..").unwrap_or((range, range));
            let lo = u32::from_str_radix(lo, 16).expect("a hex code point");
            let hi = u32::from_str_radix(hi, 16).expect("a hex code point");
            for cp in lo..=hi {
                set[cp as usize] = true;
            }
        }
        assert!(set.iter().any(|&b| b), "no {values:?} code points parsed");
        set
    }

    /// `ours` answers exactly `ucd` for every scalar value.
    fn agrees(name: &str, ours: fn(char) -> bool, ucd: &[bool]) {
        let (missing, extra): (Vec<char>, Vec<char>) = (0..=0x10_FFFF)
            .filter_map(char::from_u32)
            .filter(|&c| ours(c) != ucd[c as usize])
            .partition(|&c| ucd[c as usize]);
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{name}: {} missing, e.g. {:?}; {} extra, e.g. {:?}",
            missing.len(),
            &missing[..missing.len().min(8)],
            extra.len(),
            &extra[..extra.len().min(8)],
        );
    }

    /// The virama table is the UCD's `Canonical_Combining_Class=9`, every code
    /// point, in the Unicode version of the crate's other tables.
    #[test]
    fn the_virama_table_is_the_ucds_combining_class_9() {
        let data = include_str!("../tests/data/DerivedCombiningClass-16.0.0.txt");
        agrees("virama", is_virama, &ucd(data, &["9"]));
    }

    /// The cursive-joining table is the UCD's `Joining_Type` D, L, R and C,
    /// every code point.
    #[test]
    fn the_joining_table_is_the_ucds_joining_letters() {
        let data = include_str!("../tests/data/DerivedJoiningType-16.0.0.txt");
        agrees(
            "joins cursively",
            joins_cursively,
            &ucd(data, &["D", "L", "R", "C"]),
        );
    }

    /// The words of `text` under the default classifier.
    fn split(text: &str) -> Vec<&str> {
        words(text, is_word_char).map(|word| &text[word]).collect()
    }

    /// The words of `text` when only a space separates words.
    fn split_at_spaces(text: &str) -> Vec<&str> {
        words(text, |c| c != ' ').map(|word| &text[word]).collect()
    }

    /// A joiner belongs to a word exactly where it spells it: next to a virama,
    /// between two letters of a cursive-joining script, inside an emoji ZWJ
    /// sequence. Anywhere else, and at either end of a run, it is in no word.
    #[test]
    fn a_joiner_belongs_to_a_word_only_where_it_spells_it() {
        for word in [
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}", // می‌خواهم
            "\u{062F}\u{06CC}\u{0648}\u{200C}\u{0647}\u{0627}", // دیو‌ها: after a right-joining letter
            "\u{0628}\u{064E}\u{200C}\u{0647}", // a harakat between the letter and the ZWNJ
            "\u{0926}\u{0930}\u{094D}\u{200D}\u{092F}\u{093E}", // दर्‍या: eyelash ra
            "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}", // ශ්‍රී
            "\u{0915}\u{094D}\u{200D}\u{0937}", // क्‍ष: half form
            "\u{0915}\u{094D}\u{200C}\u{0937}", // क्‌ष: explicit virama
            "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}", // র‍্যাব: ya-phala
            "\u{0CB0}\u{200D}\u{0CCD}\u{0C95}", // ರ‍್ಕ: Kannada non-repha ra
            "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}", // ᠬᠠᠷ᠎ᠠ: Mongolian vowel separator
            "\u{1828}\u{1822}\u{180B}\u{1826}\u{180E}\u{1821}", // a free variation selector inside
            "\u{13000}\u{13430}\u{13001}",      // an Egyptian vertical joiner
            "\u{13000}\u{13431}\u{13437}\u{13001}\u{13430}\u{13002}\u{13438}\u{13003}", // a nested quadrat
            "\u{1BC00}\u{1BCA0}\u{1BC01}\u{1BCA2}\u{1BC02}", // Duployan overlap and step
        ] {
            assert_eq!(split(&format!("x {word} y")), ["x", word, "y"], "{word:?}");
            assert_eq!(
                split_at_spaces(&format!("x {word} y")),
                ["x", word, "y"],
                "{word:?}"
            );
        }
        for (text, parts) in [
            ("foo\u{200D}bar", &["foo", "bar"][..]),
            ("foo\u{200C}bar", &["foo", "bar"]),
            ("e\u{301}\u{200D}f", &["e\u{301}", "f"]),
            ("\u{0628}\u{200D}x", &["\u{0628}", "x"]),
            ("x\u{200C}\u{0628}", &["x", "\u{0628}"]),
            ("\u{0915}\u{094D}\u{200D} x", &["\u{0915}\u{094D}", "x"]),
            ("x \u{200C}\u{0915}", &["x", "\u{0915}"]),
            ("\u{200D}\u{0915}", &["\u{0915}"]),
            (
                "\u{0915}\u{094D}\u{200D}\u{200C}\u{0937}",
                &["\u{0915}\u{094D}", "\u{0937}"],
            ),
            (
                "\u{0628}\u{200C}\u{200C}\u{0628}",
                &["\u{0628}", "\u{0628}"],
            ),
            // A script's own control beside letters of another script (the
            // Mongolian vowel separator was a space before Unicode 6.3), or at
            // either end of a run of its own.
            ("ab\u{180E}cd", &["ab", "cd"]),
            ("\u{0628}\u{180E}\u{0628}", &["\u{0628}", "\u{0628}"]),
            ("\u{1820}\u{180E}x", &["\u{1820}", "x"]),
            (
                "\u{1820}\u{180E} \u{180E}\u{1820}",
                &["\u{1820}", "\u{1820}"],
            ),
            ("ab\u{13430}cd", &["ab", "cd"]),
            ("\u{1343C}\u{13000}\u{1343D} x", &["\u{13000}", "x"]),
            ("\u{13000}\u{180E}\u{13001}", &["\u{13000}", "\u{13001}"]),
            ("ab\u{1BCA0}cd", &["ab", "cd"]),
        ] {
            assert_eq!(split(text), parts, "{text:?}");
        }
        // An emoji ZWJ sequence is one glyph: where emoji are word characters,
        // its ZWJ keeps it whole. Between a letter and an emoji it does nothing.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(
            split_at_spaces(&format!("a{family}b c")),
            [&format!("a{family}b"), "c"]
        );
        assert_eq!(split_at_spaces("a\u{200D}\u{1F469}"), ["a", "\u{1F469}"]);
        assert_eq!(split(family), Vec::<&str>::new(), "emoji are no letters");
    }

    /// Marks belong to the character before them and prepended marks to the one
    /// after; every other format control separates words, under any classifier.
    #[test]
    fn marks_attach_and_format_controls_separate() {
        assert_eq!(split("cafe\u{301} x"), ["cafe\u{301}", "x"]);
        assert_eq!(
            split("x \u{301}\u{301}y"),
            ["x", "y"],
            "a mark after a space"
        );
        assert_eq!(split("\u{301}"), Vec::<&str>::new());
        assert_eq!(
            split("\u{1B13}\u{1B44}\u{1B31}"),
            ["\u{1B13}\u{1B44}\u{1B31}"],
            "a spacing virama"
        );
        assert_eq!(
            split("x \u{06DD}\u{0661}\u{0662}"),
            ["x", "\u{06DD}\u{0661}\u{0662}"]
        );
        assert_eq!(
            split("\u{0600}\u{0600}1"),
            ["\u{0600}\u{0600}1"],
            "a run of prepended marks"
        );
        assert_eq!(
            split("a\u{06DD} \u{06DD}"),
            ["a"],
            "a prepended mark before nothing"
        );
        for control in [
            '\u{061C}',
            '\u{200B}',
            '\u{200E}',
            '\u{200F}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{2064}',
            '\u{2066}',
            '\u{2069}',
            '\u{FEFF}',
            '\u{FFF9}',
            '\u{FFFA}',
            '\u{FFFB}',
            '\u{1D173}',
            '\u{1D17A}',
            '\u{E0001}',
            '\t',
            '\n',
            '\u{85}',
        ] {
            let text = format!("ab{control}cd");
            assert_eq!(split(&text), ["ab", "cd"], "U+{:04X}", control as u32);
            assert_eq!(
                split_at_spaces(&text),
                ["ab", "cd"],
                "U+{:04X}",
                control as u32
            );
        }
        assert_eq!(split("foo\nbar_1"), ["foo", "bar_1"]);
        assert_eq!(split_at_spaces("a-b c"), ["a-b", "c"]);
    }

    /// The walk is one pass: however marks, joiners and prepended marks pile up,
    /// it classifies each character a bounded number of times.
    #[test]
    fn the_walk_classifies_each_character_a_bounded_number_of_times() {
        let marks = "\u{064E}".repeat(512);
        for text in [
            format!("\u{0628}\u{200C}{marks}\u{0628}").repeat(256),
            "\u{0628}\u{200C}".repeat(64 * 1024),
            format!("{}1", "\u{0600}".repeat(128 * 1024)),
            format!("\u{0915}{}", "\u{094D}\u{200D}".repeat(64 * 1024)),
            format!("\u{13000}{}\u{13001}", "\u{13430}".repeat(128 * 1024)),
            "\u{1820}\u{180E}".repeat(64 * 1024),
        ] {
            ROLE_LOOKUPS.with(|n| n.set(0));
            let count = words(&text, is_word_char).count();
            let lookups = ROLE_LOOKUPS.with(std::cell::Cell::get);
            let chars = text.chars().count();
            assert!(count >= 1);
            assert!(
                lookups <= 3 * chars,
                "{lookups} classifications for {chars} characters"
            );
        }
        // The break before a caret is found by reading back to it, not by
        // reading the line.
        let text = "ab.".repeat(64 * 1024);
        ROLE_LOOKUPS.with(|n| n.set(0));
        assert_eq!(
            word_break_before(&text, text.len() - 1, is_word_char),
            text.len() - 4
        );
        assert!(ROLE_LOOKUPS.with(std::cell::Cell::get) <= 3);
    }

    /// A caret's word-left motion lands where the whole walk says the last word
    /// before it starts, reading back no further than the break before that
    /// word.
    #[test]
    fn the_word_before_a_caret_is_the_last_word_that_starts_before_it() {
        let classifiers: [fn(char) -> bool; 2] = [is_word_char, |c| c != ' '];
        for text in [
            "x \u{06DD}\u{0661}  .\u{301}a \u{0600}.\u{0600}1",
            "foo\u{200D}bar \u{0628}\u{200C}\u{0628}-\u{0628}\u{200C}x",
            "\u{0915}\u{094D}\u{200D}\u{0937}-\u{0915}\u{094D}\u{200D} y\u{2066}z",
            "\u{13000}\u{13431}\u{13437}\u{13001}\u{13438} \u{1820}\u{180E}\u{1820}",
            "  ",
            "",
        ] {
            for is_word in classifiers {
                let all: Vec<Range<usize>> = words(text, is_word).collect();
                for position in 0..=text.len() {
                    let floor = (0..=position)
                        .rev()
                        .find(|&at| text.is_char_boundary(at))
                        .unwrap_or(0);
                    let expected = all
                        .iter()
                        .rev()
                        .map(|word| word.start)
                        .find(|&start| start < floor);
                    assert_eq!(
                        word_start_before(text, position, is_word),
                        expected,
                        "{text:?} at {position}"
                    );
                }
            }
        }
        // From the end of a long line of short words, a step left reads only
        // the last word and the break before it.
        let text = "ab.".repeat(64 * 1024);
        ROLE_LOOKUPS.with(|n| n.set(0));
        assert_eq!(
            word_start_before(&text, text.len(), is_word_char),
            Some(text.len() - 3)
        );
        assert!(ROLE_LOOKUPS.with(std::cell::Cell::get) <= 8);
        // Past a run of separators, the step reads back over the run once.
        let text = format!("ab{}", " ".repeat(64 * 1024));
        ROLE_LOOKUPS.with(|n| n.set(0));
        assert_eq!(word_start_before(&text, text.len(), is_word_char), Some(0));
        let lookups = ROLE_LOOKUPS.with(std::cell::Cell::get);
        assert!(lookups <= 3 * text.len(), "{lookups} classifications");
    }

    /// From the last word break before any position, the walk finds exactly the
    /// words of the whole text that start there or later, every word that ends
    /// past the position among them, under either classifier: a caret's words
    /// can be read from the break instead of from the start of the line.
    #[test]
    fn the_words_after_a_break_are_the_words_of_the_whole_text() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let corpus = [
            "x \u{06DD}\u{0661} .\u{301}a \u{0600}.\u{0600}1".to_string(),
            "foo\u{200D}bar \u{0628}\u{200C}\u{0628}-\u{0628}\u{200C}x".to_string(),
            "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}.ab\u{200B}cd"
                .to_string(),
            "\u{0915}\u{094D}\u{200D}\u{0937}-\u{0915}\u{094D}\u{200D} y\u{2066}z".to_string(),
            "\u{13000}\u{13431}\u{13437}\u{13001}\u{13438} \u{1820}\u{180E}\u{1820}\u{180E}"
                .to_string(),
            format!("a{family}b {family}.c\td\ne"),
            String::new(),
        ];
        let classifiers: [fn(char) -> bool; 2] = [is_word_char, |c| c != ' '];
        for text in &corpus {
            for is_word in classifiers {
                let all: Vec<Range<usize>> = words(text, is_word).collect();
                for position in 0..=text.len() {
                    let at = word_break_before(text, position, is_word);
                    assert!(at <= position, "{text:?} at {position}");
                    let from_break: Vec<Range<usize>> = words(&text[at..], is_word)
                        .map(|word| word.start + at..word.end + at)
                        .collect();
                    let after: Vec<Range<usize>> = all
                        .iter()
                        .filter(|word| word.start >= at)
                        .cloned()
                        .collect();
                    assert_eq!(from_break, after, "{text:?} from {at} ({position})");
                    assert!(
                        all.iter()
                            .filter(|word| word.end > position)
                            .all(|word| word.start >= at),
                        "{text:?}: a word before {at} ends past {position}"
                    );
                }
            }
        }
    }
}
