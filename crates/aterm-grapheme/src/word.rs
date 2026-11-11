// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Word boundary classification ([`is_word_char`], [`SCRIPT_CONTROLS`]).

use std::ops::Range;

use crate::emoji_presentation::in_ranges;
use crate::tables::char_width;
use crate::tables::gcb::{GCB, gcb_class};

/// Zero width non-joiner.
const ZWNJ: char = '\u{200C}';
/// Zero width joiner.
const ZWJ: char = '\u{200D}';

/// The default word character: a letter or digit of any script, or `_`.
#[must_use]
#[inline]
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The words of `text`, in order, as byte ranges.
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

/// Byte offset of the last word-ending character before `position`, or 0 if none.
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

/// The start of the last word of `text` that starts before `position`, or `None` if no word does.
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
pub(crate) const fn floor_char_boundary(text: &str, position: usize) -> usize {
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
    /// The last letter before `at`.
    letter: Option<char>,
}

impl<F: Fn(char) -> bool> Words<'_, F> {
    /// Whether `c`, standing on its own, is a word character.
    #[inline]
    fn is_word_base(&self, c: char) -> bool {
        role(c) == Role::Base && (self.is_word)(c)
    }

    /// Classify the next unit at `self.at`: whether it is in a word.
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

    /// Whether `joiner` (ZWNJ or ZWJ) joins into the surrounding word.
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
    /// A format control that can spell a word: in a word only where it spells one.
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

/// A script that spells words with format controls of its own.
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

/// The scripts whose own format controls belong to a word between two of their letters.
static SCRIPT_CONTROLS: [ScriptControls; 3] = [
    ScriptControls {
        controls: (0x180E, 0x180E),
        letters: (0x1800, 0x18AF),
    },
    ScriptControls {
        controls: (0x13430, 0x1343F),
        letters: (0x13000, 0x143FF),
    },
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

/// Whether `c` is `Extended_Pictographic`, an element an emoji ZWJ sequence joins.
#[inline]
fn is_pictographic(c: char) -> bool {
    gcb_class(c) == GCB::Extended_Pictographic
}

/// Whether `c` is a virama (`Canonical_Combining_Class=9`).
#[inline]
fn is_virama(c: char) -> bool {
    in_ranges(VIRAMA, c as u32)
}

/// Whether `c` joins its neighbours cursively (`Joining_Type` Dual, Left, Right or Join_Causing).
#[inline]
fn joins_cursively(c: char) -> bool {
    in_ranges(JOINING, c as u32)
}

/// `Canonical_Combining_Class=9` (Virama) ranges, inclusive `(lo, hi)`, sorted and disjoint.
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

/// `Joining_Type` Dual_Joining.
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
#[path = "word_tests.rs"]
mod tests;
