// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Emoji property tests against UCD `emoji-data.txt` (Unicode 16.0.0) across all code points.

use aterm_grapheme::{
    grapheme_display_width, is_emoji_modifier_base, is_emoji_presentation, is_vs16_emoji_capable,
    str_grid_columns,
};

/// The code points `emoji-data.txt` gives `property`.
fn property(property: &str) -> Vec<bool> {
    let data = include_str!("data/emoji-data-16.0.0.txt");
    let mut set = vec![false; 0x11_0000];
    for line in data.lines() {
        let content = line.split('#').next().unwrap_or("");
        let Some((range, name)) = content.split_once(';') else {
            continue;
        };
        if name.trim() != property {
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
    assert!(set.iter().any(|&b| b), "no {property} code points parsed");
    set
}

fn scalars() -> impl Iterator<Item = char> {
    (0..=0x10_FFFF).filter_map(char::from_u32)
}

/// `ours` answers exactly `ucd` for every scalar value.
fn agrees(name: &str, ours: fn(char) -> bool, ucd: impl Fn(usize) -> bool) {
    let (missing, extra): (Vec<char>, Vec<char>) = scalars()
        .filter(|&c| ours(c) != ucd(c as usize))
        .partition(|&c| ucd(c as usize));
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{name}: {} missing, e.g. {:?}; {} extra, e.g. {:?}",
        missing.len(),
        &missing[..missing.len().min(8)],
        extra.len(),
        &extra[..extra.len().min(8)],
    );
}

#[test]
fn the_emoji_tables_are_the_ucds() {
    let emoji = property("Emoji");
    let presentation = property("Emoji_Presentation");
    let modifier_base = property("Emoji_Modifier_Base");
    agrees(
        "is_vs16_emoji_capable (Emoji, not Emoji_Presentation)",
        is_vs16_emoji_capable,
        |cp| emoji[cp] && !presentation[cp],
    );
    agrees("is_emoji_presentation", is_emoji_presentation, |cp| {
        presentation[cp]
    });
    agrees("is_emoji_modifier_base", is_emoji_modifier_base, |cp| {
        modifier_base[cp]
    });
}

/// `grapheme_display_width` and the grid agree on what VS16 does to any
/// printing character: it widens a narrow text-default emoji (`⬆️` is two
/// cells) and nothing else (`a️`, `✓️` stay one).
#[test]
fn a_base_and_vs16_are_as_wide_as_the_grid_draws_them() {
    let disagree: Vec<(char, usize, usize)> = scalars()
        .filter(|c| !c.is_control())
        .filter_map(|c| {
            let s: String = [c, '\u{FE0F}'].into_iter().collect();
            let (width, grid) = (grapheme_display_width(&s), str_grid_columns(&s));
            (width != grid).then_some((c, width, grid))
        })
        .collect();
    assert!(
        disagree.is_empty(),
        "{} bases + VS16 measure other than the grid draws them, e.g. (base, width, grid) {:?}",
        disagree.len(),
        &disagree[..disagree.len().min(8)],
    );
}
