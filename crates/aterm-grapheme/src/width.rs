// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme width calculation, emoji detection, and classification.

use crate::emoji_presentation::is_vs16_emoji_capable;
use crate::grapheme_iter::GraphemeClusters;
use crate::tables::width as unicode_tables;
use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

use crate::types::{Grapheme, GraphemeInfo};

/// Calculate grapheme information for a string.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: `result.byte_count == s.len()` and width/count fields are derived from grapheme iteration of `s`.
///
/// # Example
///
/// ```
/// use aterm_grapheme::grapheme_width;
///
/// let info = grapheme_width("Hello 世界!");
/// assert_eq!(info.grapheme_count, 9);
/// assert_eq!(info.display_width, 11); // 7 ASCII + 2 wide chars
/// ```
#[inline]
pub fn grapheme_width(s: &str) -> GraphemeInfo {
    grapheme_width_inner(s, false)
}

fn grapheme_width_inner(s: &str, cjk: bool) -> GraphemeInfo {
    if s.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        let n = s.len();
        return GraphemeInfo {
            byte_count: n,
            grapheme_count: n,
            display_width: n,
            codepoint_count: n,
            has_emoji: false,
            has_combining: false,
            has_wide: false,
        };
    }
    let mut info = GraphemeInfo {
        byte_count: s.len(),
        ..Default::default()
    };
    for g in s.graphemes() {
        if g.len() == 1 {
            info.grapheme_count += 1;
            info.display_width += 1;
            info.codepoint_count += 1;
            continue;
        }
        let codepoints = g.chars().count();
        let width = grapheme_display_width_inner(g, cjk);
        info.grapheme_count += 1;
        info.display_width += width;
        info.codepoint_count += codepoints;
        info.has_emoji |= is_emoji_grapheme(g);
        info.has_combining |= codepoints > 1 && width <= 1;
        info.has_wide |= width == 2;
    }
    info
}

/// Calculate the display width of a single grapheme cluster.
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a terminal cell width in the inclusive range `0..=2`.
#[inline]
pub fn grapheme_display_width(grapheme: &str) -> usize {
    grapheme_display_width_inner(grapheme, false)
}

/// [`grapheme_display_width`]; `cjk` makes ambiguous-width characters 2.
fn grapheme_display_width_inner(grapheme: &str, cjk: bool) -> usize {
    let char_width_fn = if cjk {
        unicode_tables::char_width_cjk
    } else {
        unicode_tables::char_width
    };

    let mut chars = grapheme.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return 0,
    };

    let first_cp = first as u32;

    if chars.next().is_none() {
        if first_cp <= 0x1F || (0x7F..=0x9F).contains(&first_cp) {
            return 1;
        }
        return char_width_fn(first);
    }

    let mut chars = spacing_chars(grapheme);
    let Some(first) = chars.next() else {
        return 0;
    };
    let second = chars.next();

    if is_vs16_emoji_capable(first) && grapheme.contains('\u{FE0F}') {
        return 2;
    }
    let first_is_emoji = is_emoji_char(first) || is_vs16_emoji_capable(first);

    if first_is_emoji
        && (second.is_some_and(is_skin_tone_modifier)
            || second == Some('\u{200D}')
            || chars.any(|c| c == '\u{200D}'))
    {
        return 2;
    }

    char_width_fn(first).min(2)
}

/// [`grapheme_width`] with ambiguous-width characters sized by
/// `config.ambiguous_width`.
///
/// # Example
///
/// ```
/// use aterm_grapheme::grapheme_width_with_config;
/// use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};
///
/// let cjk = TextShapingConfig { ambiguous_width: AmbiguousWidth::Double, ..Default::default() };
/// // Degree sign is ambiguous-width: 1 in single mode, 2 in double (CJK) mode
/// let info = grapheme_width_with_config("\u{00B0}test", &cjk);
/// assert_eq!(info.display_width, 6); // degree(2) + t(1) + e(1) + s(1) + t(1)
/// ```
#[inline]
pub fn grapheme_width_with_config(s: &str, config: &TextShapingConfig) -> GraphemeInfo {
    grapheme_width_inner(s, is_cjk(config))
}

/// [`split_graphemes`] with ambiguous-width characters sized by
/// `config.ambiguous_width`.
#[inline]
pub fn split_graphemes_with_config<'a>(
    s: &'a str,
    config: &TextShapingConfig,
) -> impl Iterator<Item = Grapheme<'a>> + use<'a> {
    split_graphemes_inner(s, is_cjk(config))
}

/// [`grapheme_display_width`] with East Asian Ambiguous characters (degree
/// sign, em dash, some box drawing) 2 cells wide when
/// `config.ambiguous_width` is `Double`.
#[inline]
pub fn grapheme_display_width_with_config(grapheme: &str, config: &TextShapingConfig) -> usize {
    grapheme_display_width_inner(grapheme, is_cjk(config))
}

#[inline]
const fn is_cjk(config: &TextShapingConfig) -> bool {
    matches!(config.ambiguous_width, AmbiguousWidth::Double)
}

/// Iterator over grapheme clusters with metadata.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: iterator yields graphemes in byte order with non-overlapping byte ranges that cover `s`.
#[inline]
pub fn split_graphemes(s: &str) -> impl Iterator<Item = Grapheme<'_>> {
    split_graphemes_inner(s, false)
}

#[inline]
fn split_graphemes_inner(s: &str, cjk: bool) -> impl Iterator<Item = Grapheme<'_>> {
    s.grapheme_indices().map(move |(offset, g)| {
        let codepoint_count = g.chars().count();
        let width = grapheme_display_width_inner(g, cjk);
        Grapheme {
            text: g,
            byte_offset: offset,
            width,
            codepoint_count,
            is_emoji: is_emoji_grapheme(g),
            has_combining: codepoint_count > 1 && width <= 1,
        }
    })
}

/// The cluster from its first spacing base.
#[inline]
fn spacing_chars(grapheme: &str) -> impl Iterator<Item = char> + '_ {
    grapheme
        .chars()
        .skip_while(|&ch| unicode_tables::char_width(ch) == 0)
}

/// Whether the spacing base [`is_emoji_char`].
#[inline]
fn is_emoji_grapheme(grapheme: &str) -> bool {
    spacing_chars(grapheme)
        .next()
        .or_else(|| grapheme.chars().next())
        .is_some_and(is_emoji_char)
}

/// Whether `c` is in an emoji-heavy block, a regional indicator or VS16: a
/// coarse block test for width clustering, not the `Emoji` property (see
/// [`crate::is_emoji_presentation`]).
#[inline]
pub const fn is_emoji_char(c: char) -> bool {
    matches!(
        c as u32,
        0x2600..=0x27BF // Miscellaneous Symbols, Dingbats
        | 0x1F1E0..=0x1F1FF // Regional indicators
        | 0x1F300..=0x1F64F // Misc Symbols and Pictographs, Emoticons
        | 0x1F680..=0x1F6FF // Transport and Map Symbols
        | 0x1F900..=0x1FAFF // Supplemental Symbols and Pictographs through Extended-A
        | 0xFE0F // VS16
    )
}

/// Check if a character is a skin tone modifier.
#[inline]
pub const fn is_skin_tone_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Whether `c` takes a Fitzpatrick skin-tone modifier (U+1F3FB..=U+1F3FF).
#[must_use]
#[inline]
pub const fn is_emoji_modifier_base(c: char) -> bool {
    let cp = c as u32;
    matches!(
        cp,
        0x261D | 0x26F9 // Index Pointing Up, Person Bouncing Ball
        | 0x270A..=0x270D // Raised Fist through Writing Hand
        | 0x1F385 // Santa Claus
        | 0x1F3C2..=0x1F3C4 // Snowboarder, Runner, Surfer
        | 0x1F3C7 // Horse Racing
        | 0x1F3CA..=0x1F3CC // Swimmer, Lifter, Golfer
        | 0x1F442..=0x1F443 // Ear, Nose
        | 0x1F446..=0x1F450 // Pointing up through open hands
        | 0x1F466..=0x1F478 // Boy through Princess
        | 0x1F47C // Baby Angel
        | 0x1F481..=0x1F483 // Information Desk Person through Dancer
        | 0x1F485..=0x1F487 // Nail Polish through Haircut
        | 0x1F48F // Kiss
        | 0x1F491 // Couple with Heart
        | 0x1F4AA // Flexed Biceps
        | 0x1F574..=0x1F575 // Man in Business Suit Levitating, Sleuth
        | 0x1F57A // Man Dancing
        | 0x1F590 // Raised Hand with Fingers Splayed
        | 0x1F595..=0x1F596 // Reversed Hand with Middle Finger, Vulcan Salute
        | 0x1F645..=0x1F647 // Face No Good through Person Bowing
        | 0x1F64B..=0x1F64F // Happy Person Raising Hand through Person Praying
        | 0x1F6A3 // Rowboat
        | 0x1F6B4..=0x1F6B6 // Bicyclist through Pedestrian
        | 0x1F6C0 // Bath
        | 0x1F6CC // Sleeping Accommodation
        | 0x1F90C // Pinched Fingers
        | 0x1F90F // Pinching Hand
        | 0x1F918..=0x1F91F // Sign of the Horns through I Love You Gesture
        | 0x1F926 // Face Palm
        | 0x1F930..=0x1F939 // Pregnant Woman through Juggling
        | 0x1F93C..=0x1F93E // Wrestlers through Handball
        | 0x1F977 // Ninja
        | 0x1F9B5..=0x1F9B6 // Leg, Foot
        | 0x1F9B8..=0x1F9B9 // Superhero, Supervillain
        | 0x1F9BB // Ear with Hearing Aid
        | 0x1F9CD..=0x1F9CF // Standing Person through Deaf Person
        | 0x1F9D1..=0x1F9DD // Adult through Elf
        | 0x1FAC3..=0x1FAC5 // Pregnant Man, Pregnant Person, Person with Crown
        | 0x1FAF0..=0x1FAF8 // Hand with Index Finger and Thumb Crossed through Rightwards Pushing Hand
    )
}

/// Check if a character is a regional indicator.
#[inline]
pub const fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}
