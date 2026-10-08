// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme width calculation, emoji detection, and classification.

use crate::emoji_presentation::is_vs16_emoji_capable;
use crate::grapheme_iter::GraphemeClusters;
use crate::tables::width as unicode_tables;
use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

#[cfg(any(test, kani))]
use crate::types::GraphemeType;
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
pub fn grapheme_width(s: &str) -> GraphemeInfo {
    grapheme_width_inner(s, false)
}

fn grapheme_width_inner(s: &str, cjk: bool) -> GraphemeInfo {
    let mut info = GraphemeInfo {
        byte_count: s.len(),
        ..Default::default()
    };
    for g in s.graphemes() {
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
///
/// A lone C0/C1 control is 1 (terminals advance past a rendered control); a
/// cluster takes its first character's width, except that an emoji base with
/// VS16, a skin tone or a ZWJ is 2.
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

    let second = match chars.next() {
        Some(c) => c,
        None => {
            if first_cp <= 0x1F || (0x7F..=0x9F).contains(&first_cp) {
                return 1;
            }
            return char_width_fn(first);
        }
    };

    // Multi-codepoint grapheme cluster. VS16 (U+FE0F) widens a text-default
    // emoji, and only that: the grid's rule (`is_vs16_emoji_capable`), so `a️`
    // and `✓️` stay one cell, as the grid draws them.
    if is_vs16_emoji_capable(first) && grapheme.contains('\u{FE0F}') {
        return 2;
    }
    let first_is_emoji = is_emoji_char(first) || is_vs16_emoji_capable(first);

    // An emoji base with a skin tone, or an emoji ZWJ sequence, is wide.
    if first_is_emoji
        && (is_skin_tone_modifier(second) || second == '\u{200D}' || chars.any(|c| c == '\u{200D}'))
    {
        return 2;
    }

    // Base + combining marks: the base's width.
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
pub fn grapheme_width_with_config(s: &str, config: &TextShapingConfig) -> GraphemeInfo {
    grapheme_width_inner(s, is_cjk(config))
}

/// [`split_graphemes`] with ambiguous-width characters sized by
/// `config.ambiguous_width`.
pub fn split_graphemes_with_config<'a>(
    s: &'a str,
    config: &TextShapingConfig,
) -> impl Iterator<Item = Grapheme<'a>> + use<'a> {
    split_graphemes_inner(s, is_cjk(config))
}

/// [`grapheme_display_width`] with East Asian Ambiguous characters (degree
/// sign, em dash, some box drawing) 2 cells wide when
/// `config.ambiguous_width` is `Double`.
pub fn grapheme_display_width_with_config(grapheme: &str, config: &TextShapingConfig) -> usize {
    grapheme_display_width_inner(grapheme, is_cjk(config))
}

fn is_cjk(config: &TextShapingConfig) -> bool {
    matches!(config.ambiguous_width, AmbiguousWidth::Double)
}

/// Iterator over grapheme clusters with metadata.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: iterator yields graphemes in byte order with non-overlapping byte ranges that cover `s`.
pub fn split_graphemes(s: &str) -> impl Iterator<Item = Grapheme<'_>> {
    split_graphemes_inner(s, false)
}

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

/// Whether the cluster's first character [`is_emoji_char`].
fn is_emoji_grapheme(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(is_emoji_char)
}

/// Whether `c` is in an emoji-heavy block, a regional indicator or VS16: a
/// coarse block test for width clustering, not the `Emoji` property (see
/// [`crate::is_emoji_presentation`]).
pub fn is_emoji_char(c: char) -> bool {
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

/// Zero Width Joiner (ZWJ) character.
#[cfg(any(test, kani))]
pub const ZWJ: char = '\u{200D}';

/// Check if a grapheme contains a ZWJ sequence.
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns `true` iff `grapheme` contains U+200D.
#[cfg(any(test, kani))]
#[inline]
pub fn has_zwj(grapheme: &str) -> bool {
    grapheme.contains(ZWJ)
}

/// Check if a character is a skin tone modifier.
///
/// Skin tone modifiers (Fitzpatrick scale) are U+1F3FB through U+1F3FF.
#[inline]
pub fn is_skin_tone_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Whether `c` takes a Fitzpatrick skin-tone modifier (U+1F3FB..=U+1F3FF).
///
/// Exactly `Emoji_Modifier_Base` from Unicode 16.0.0 emoji-data.txt (UTS #51;
/// 134 code points), the UCD version of [`crate::tables`]. The live write path
/// (`try_combine_skin_tone_modifier`) and the scrollback materializer both
/// fold skin tones on this, so a skin-toned emoji is one cell either way.
#[must_use]
#[inline]
pub fn is_emoji_modifier_base(c: char) -> bool {
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

/// Check if a grapheme has a skin tone modifier.
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns `true` iff at least one codepoint is in `U+1F3FB..=U+1F3FF`.
#[cfg(any(test, kani))]
pub fn has_skin_tone(grapheme: &str) -> bool {
    grapheme.chars().any(is_skin_tone_modifier)
}

/// Check if a character is a regional indicator.
///
/// Regional indicators (A-Z) are used in pairs to create flag emoji.
#[inline]
pub fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Check if a grapheme is a flag emoji (two regional indicators).
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns `true` iff `grapheme` has exactly two regional-indicator codepoints.
#[cfg(any(test, kani))]
pub fn is_flag_emoji(grapheme: &str) -> bool {
    let chars: Vec<char> = grapheme.chars().collect();
    chars.len() == 2 && chars.iter().all(|&c| is_regional_indicator(c))
}

/// Classify a grapheme into its type.
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns `GraphemeType::Control` for empty input; otherwise returns one deterministic classification.
#[cfg(any(test, kani))]
pub fn classify_grapheme(grapheme: &str) -> GraphemeType {
    if grapheme.is_empty() {
        return GraphemeType::Control;
    }

    let first = grapheme
        .chars()
        .next()
        .expect("invariant: checked !is_empty()");
    let codepoint_count = grapheme.chars().count();

    if grapheme.len() == 1 && first.is_ascii() {
        return if first.is_control() {
            GraphemeType::Control
        } else {
            GraphemeType::Ascii
        };
    }

    if has_zwj(grapheme) {
        return GraphemeType::ZwjSequence;
    }

    if is_flag_emoji(grapheme) {
        return GraphemeType::Flag;
    }

    if codepoint_count > 1 && grapheme_display_width(grapheme) <= 1 {
        return GraphemeType::Combining;
    }

    if is_emoji_char(first) {
        return GraphemeType::Emoji;
    }

    if grapheme_display_width(grapheme) == 2 {
        return GraphemeType::Wide;
    }

    GraphemeType::Other
}
