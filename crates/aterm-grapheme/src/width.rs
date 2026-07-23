// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme width calculation, emoji detection, and classification.

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
/// This is an efficient single-pass analysis that computes all grapheme
/// metrics at once.
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
    let mut info = GraphemeInfo {
        byte_count: s.len(),
        ..Default::default()
    };

    for g in s.graphemes() {
        let codepoints: usize = g.chars().count();
        let width = grapheme_display_width(g);
        let has_combining = codepoints > 1 && width <= 1;
        let is_emoji = is_emoji_grapheme(g);

        info.grapheme_count += 1;
        info.display_width += width;
        info.codepoint_count += codepoints;
        info.has_emoji |= is_emoji;
        info.has_combining |= has_combining;
        info.has_wide |= width == 2;
    }

    info
}

/// Calculate the display width of a single grapheme cluster.
///
/// REQUIRES: `grapheme` is valid UTF-8 (`&str` invariant).
/// ENSURES: returns a terminal cell width in the inclusive range `0..=2`.
///
/// Returns 0, 1, or 2 based on the grapheme's visual width in a terminal.
///
/// # Rules
///
/// - Control characters: 0 width
/// - Most ASCII: 1 width
/// - CJK ideographs: 2 width
/// - Most emoji: 2 width
/// - Zero-width joiners/combining marks: 0 width (but counted in cluster)
///
pub fn grapheme_display_width(grapheme: &str) -> usize {
    grapheme_display_width_inner(grapheme, false)
}

/// Inner implementation of grapheme display width calculation.
///
/// Handles emoji presentation sequences, ZWJ sequences, keycap sequences,
/// and VS16 modifiers that make grapheme clusters width 2.
/// When `cjk` is true, ambiguous-width characters are treated as width 2.
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

    // Single-character grapheme: fast path
    let second = match chars.next() {
        Some(c) => c,
        None => {
            // Control chars (C0/C1): return 1 for terminal display compatibility
            // (terminals typically advance cursor by 1 for rendered control chars)
            if first_cp <= 0x1F || (0x7F..=0x9F).contains(&first_cp) {
                return 1;
            }
            return char_width_fn(first);
        }
    };

    // Multi-codepoint grapheme cluster
    let first_is_emoji = is_emoji_char(first) || is_emoji_presentation_base(first_cp);

    // Check for VS16 (U+FE0F) emoji presentation: base + FE0F = width 2
    if second as u32 == 0xFE0F {
        return 2;
    }

    // Check for emoji modifier (skin tone): emoji + modifier = width 2
    if first_is_emoji && (0x1F3FB..=0x1F3FF).contains(&(second as u32)) {
        return 2;
    }

    // Check for ZWJ (U+200D): only emoji ZWJ sequences are width 2
    if second as u32 == 0x200D && first_is_emoji {
        return 2;
    }

    // Check remaining chars for ZWJ or VS16 (keycap sequences, extended emoji)
    for c in chars {
        let cp = c as u32;
        if cp == 0xFE0F {
            return 2;
        }
        if cp == 0x200D && first_is_emoji {
            return 2;
        }
    }

    // For other multi-codepoint clusters (e.g. base + combining marks),
    // use the first char's width
    let width = char_width_fn(first);

    // Clamp to max 2 for terminal display
    width.min(2)
}

/// Calculate aggregate grapheme information for a string using a text shaping config.
///
/// This is the config-aware equivalent of [`grapheme_width`]. It uses
/// `grapheme_display_width_with_config` for each grapheme, so ambiguous-width
/// characters respect the `AmbiguousWidth` setting.
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
    let mut info = GraphemeInfo {
        byte_count: s.len(),
        ..Default::default()
    };

    for g in s.graphemes() {
        let codepoints: usize = g.chars().count();
        let width = grapheme_display_width_with_config(g, config);
        let has_combining = codepoints > 1 && width <= 1;
        let is_emoji = is_emoji_grapheme(g);

        info.grapheme_count += 1;
        info.display_width += width;
        info.codepoint_count += codepoints;
        info.has_emoji |= is_emoji;
        info.has_combining |= has_combining;
        info.has_wide |= width == 2;
    }

    info
}

/// Iterator over grapheme clusters with metadata, using a text shaping config.
///
/// This is the config-aware equivalent of [`split_graphemes`]. Ambiguous-width
/// characters respect the `AmbiguousWidth` setting when computing per-grapheme
/// display widths.
pub fn split_graphemes_with_config<'a>(
    s: &'a str,
    config: &'a TextShapingConfig,
) -> impl Iterator<Item = Grapheme<'a>> {
    s.grapheme_indices().map(move |(offset, g)| {
        let codepoint_count = g.chars().count();
        let width = grapheme_display_width_with_config(g, config);
        let has_combining = codepoint_count > 1 && width <= 1;
        let is_emoji = is_emoji_grapheme(g);

        Grapheme {
            text: g,
            byte_offset: offset,
            width,
            codepoint_count,
            is_emoji,
            has_combining,
        }
    })
}

/// Calculate the display width of a grapheme with text shaping config.
///
/// Uses `width_cjk()` when `ambiguous_width == Double` (CJK mode).
/// This affects characters with Unicode East Asian Width property "Ambiguous"
/// like degree sign (deg), em-dash, and some box drawing characters.
pub fn grapheme_display_width_with_config(grapheme: &str, config: &TextShapingConfig) -> usize {
    #[allow(unreachable_patterns)] // AmbiguousWidth is #[non_exhaustive]; wildcard required
    let cjk = matches!(config.ambiguous_width, AmbiguousWidth::Double);
    grapheme_display_width_inner(grapheme, cjk)
}

/// Iterator over grapheme clusters with metadata.
///
/// REQUIRES: `s` is valid UTF-8 (`&str` invariant).
/// ENSURES: iterator yields graphemes in byte order with non-overlapping byte ranges that cover `s`.
///
/// This provides detailed information about each grapheme, including
/// byte offset, width, and composition.
pub fn split_graphemes(s: &str) -> impl Iterator<Item = Grapheme<'_>> {
    s.grapheme_indices().map(|(offset, g)| {
        let codepoint_count = g.chars().count();
        let width = grapheme_display_width(g);
        let has_combining = codepoint_count > 1 && width <= 1;
        let is_emoji = is_emoji_grapheme(g);

        Grapheme {
            text: g,
            byte_offset: offset,
            width,
            codepoint_count,
            is_emoji,
            has_combining,
        }
    })
}

/// Check if a grapheme cluster is primarily an emoji.
///
/// This detects emoji including:
/// - Basic emoji
/// - Emoji with modifiers
/// - Emoji ZWJ sequences
/// - Regional indicator pairs
fn is_emoji_grapheme(grapheme: &str) -> bool {
    let first_char = grapheme.chars().next();
    match first_char {
        Some(c) => is_emoji_char(c),
        None => false,
    }
}

/// Check if a codepoint is an emoji presentation base.
///
/// These are characters that can appear as text by default but become emoji
/// with VS16 (U+FE0F). Includes keycap bases (#, *, 0-9), miscellaneous
/// symbols, and dingbats.
fn is_emoji_presentation_base(cp: u32) -> bool {
    // Keycap bases: #, *, 0-9
    if cp == 0x23 || cp == 0x2A || (0x30..=0x39).contains(&cp) {
        return true;
    }
    // Common text-presentation emoji that become emoji with VS16
    // Information source, trade mark, copyright, registered
    if matches!(cp, 0x00A9 | 0x00AE | 0x2122 | 0x2139) {
        return true;
    }
    // Arrows and misc
    if matches!(cp, 0x2194..=0x2199 | 0x21A9..=0x21AA) {
        return true;
    }
    // Misc symbols that have emoji variants
    if (0x2300..=0x23FF).contains(&cp) {
        return true;
    }
    // Misc technical
    if (0x2460..=0x24FF).contains(&cp) {
        return true;
    }
    // Dingbats and misc symbols (including sun U+2600, etc.)
    if (0x2600..=0x27BF).contains(&cp) {
        return true;
    }
    // Supplemental arrows
    if (0x2934..=0x2935).contains(&cp) {
        return true;
    }
    // CJK symbols
    if matches!(cp, 0x3030 | 0x303D | 0x3297 | 0x3299) {
        return true;
    }
    false
}

/// Check if a character is an emoji or emoji component.
pub fn is_emoji_char(c: char) -> bool {
    let cp = c as u32;

    // Common emoji ranges (simplified for performance)
    // See Unicode Emoji specification for complete list

    // Dingbats (some emoji)
    if (0x2600..=0x26FF).contains(&cp) {
        return true;
    }

    // Misc symbols
    if (0x2700..=0x27BF).contains(&cp) {
        return true;
    }

    // Supplemental symbols and pictographs
    if (0x1F300..=0x1F5FF).contains(&cp) {
        return true;
    }

    // Emoticons
    if (0x1F600..=0x1F64F).contains(&cp) {
        return true;
    }

    // Transport and map symbols
    if (0x1F680..=0x1F6FF).contains(&cp) {
        return true;
    }

    // Supplemental symbols
    if (0x1F900..=0x1F9FF).contains(&cp) {
        return true;
    }

    // Symbols and pictographs extended-A
    if (0x1FA00..=0x1FA6F).contains(&cp) {
        return true;
    }

    // Symbols and pictographs extended-B
    if (0x1FA70..=0x1FAFF).contains(&cp) {
        return true;
    }

    // Regional indicator symbols
    if (0x1F1E0..=0x1F1FF).contains(&cp) {
        return true;
    }

    // Variation selectors (emoji presentation)
    if cp == 0xFE0F {
        return true;
    }

    false
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
#[cfg(any(test, kani))]
#[inline]
pub fn is_skin_tone_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Check if a character is an emoji modifier base — i.e., can accept a
/// Fitzpatrick skin-tone modifier (U+1F3FB..=U+1F3FF).
///
/// Covers the common subset of `Emoji_Modifier_Base` from Unicode emoji-data.txt
/// (UTS #51): people, hand gestures, body parts, and person activities. Both the
/// live write path (`try_combine_skin_tone_modifier`) and the scrollback
/// materializer gate skin-tone folding on this, so a skin-toned emoji renders as
/// one cell whether it is live or scrolled back — single source of truth.
#[must_use]
#[inline]
pub fn is_emoji_modifier_base(c: char) -> bool {
    let cp = c as u32;
    matches!(
        cp,
        // People and body parts (U+261D, U+26F9)
        0x261D | 0x26F9
        // Dingbats: raised hand, victory hand, writing hand
        | 0x270A..=0x270D
        // Miscellaneous Symbols and Pictographs
        | 0x1F385 // Santa Claus
        | 0x1F3C2..=0x1F3C4 // Snowboarder, Horse Racing, Surfer
        | 0x1F3C7 // Horse Racing
        | 0x1F3CA..=0x1F3CC // Swimmer, Lifter, Golfer
        // People emoji block (U+1F442-U+1F4AA)
        | 0x1F442..=0x1F443 // Ear, Nose
        | 0x1F446..=0x1F450 // Pointing up through open hands
        | 0x1F466..=0x1F478 // Boy through Princess
        | 0x1F47C // Baby Angel
        | 0x1F481..=0x1F483 // Information Desk Person through Dancer
        | 0x1F485..=0x1F487 // Nail Polish through Haircut
        | 0x1F48F // Kiss
        | 0x1F491 // Couple with Heart
        | 0x1F4AA // Flexed Biceps
        // Additional people and activities
        | 0x1F574..=0x1F575 // Man in Business Suit Levitating, Sleuth
        | 0x1F57A // Man Dancing
        | 0x1F590 // Raised Hand with Fingers Splayed
        | 0x1F595..=0x1F596 // Reversed Hand with Middle Finger, Vulcan Salute
        | 0x1F645..=0x1F647 // Face No Good through Person Bowing
        | 0x1F64B..=0x1F64F // Happy Person Raising Hand through Person Praying
        // Supplemental Symbols and Pictographs
        | 0x1F6A3 // Rowboat
        | 0x1F6B4..=0x1F6B6 // Bicyclist through Pedestrian
        | 0x1F6C0 // Bath
        | 0x1F6CC // Sleeping Accommodation
        // Hand gestures and people (Supplemental Symbols block)
        | 0x1F90C..=0x1F90F // Pinched Fingers through Pinching Hand + extras
        | 0x1F918..=0x1F91F // Sign of the Horns through I Love You Gesture
        | 0x1F926 // Face Palm
        | 0x1F930..=0x1F939 // Pregnant Woman through Juggling
        | 0x1F93C..=0x1F93E // Wrestlers through Handball
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
#[cfg(any(test, kani))]
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

    // Check for ASCII
    if grapheme.len() == 1 && first.is_ascii() {
        return if first.is_control() {
            GraphemeType::Control
        } else {
            GraphemeType::Ascii
        };
    }

    // Check for ZWJ sequence
    if has_zwj(grapheme) {
        return GraphemeType::ZwjSequence;
    }

    // Check for flag emoji
    if is_flag_emoji(grapheme) {
        return GraphemeType::Flag;
    }

    // Check for combining marks
    if codepoint_count > 1 && grapheme_display_width(grapheme) <= 1 {
        return GraphemeType::Combining;
    }

    // Check for emoji
    if is_emoji_char(first) {
        return GraphemeType::Emoji;
    }

    // Check for wide character
    if grapheme_display_width(grapheme) == 2 {
        return GraphemeType::Wide;
    }

    GraphemeType::Other
}
