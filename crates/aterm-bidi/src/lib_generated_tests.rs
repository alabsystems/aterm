// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_spec::rng::XorShift;

/// A representative bidi alphabet exercising every resolved class: LTR letters,
/// European numbers/separators/terminators, Hebrew (R), Arabic (AL) +
/// Arabic-Indic digits (AN), common separators, neutrals, whitespace, and the
/// directional marks.
const ALPHABET: &[char] = &[
    'a', 'Z', '5', '0', '+', '-', '$', '%', ',', '.', ':', '/', ' ', '\t', '\n', '!', '(', ')',
    '\u{05D0}', '\u{05D1}', '\u{05EA}', '\u{0627}', '\u{0628}', '\u{0660}', '\u{0669}', '\u{200F}',
    '\u{200E}', '[', ']', '\u{0301}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];
const LTR_ONLY: &[char] = &[
    'a', 'q', 'Z', 'M', '0', '7', ' ', '+', '-', '.', ',', ':', '(', ')', '!',
];
const BASES: [BaseDirection; 3] = [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl];

/// A line of up to 47 characters drawn from `alphabet`.
fn draw_line(rng: &mut XorShift, alphabet: &[char]) -> Vec<char> {
    (0..rng.index(48)).map(|_| *rng.pick(alphabet)).collect()
}

/// Lines over [`ALPHABET`], plus a two-paragraph RTL/LTR line.
fn lines() -> impl Iterator<Item = Vec<char>> {
    let mut rng = XorShift::new(0x0B1D_1A57_C0FF_EE11);
    std::iter::once(vec!['\u{05D0}', '\n', 'a'])
        .chain((0..512).map(move |_| draw_line(&mut rng, ALPHABET)))
}

/// The visual order is always a permutation of 0..n, for any input and base
/// — the load-bearing safety property (no cell dropped or duplicated).
#[test]
fn reorder_is_always_a_permutation() {
    for line in lines() {
        for base in BASES {
            let mut order = reorder_visual_to_logical(&line, base);
            assert_eq!(order.len(), line.len());
            order.sort_unstable();
            assert_eq!(order, (0..line.len()).collect::<Vec<_>>(), "{line:?}");
        }
    }
}

/// Levels match the input length and never fall below their PARAGRAPH's
/// base level (explicit embeddings and I1/I2 only raise; L1 resets to
/// base).
#[test]
fn levels_well_formed() {
    let mut matching = Vec::new();
    for line in lines() {
        let (line_classes, _) = classes_and_brackets(&line);
        for base in BASES {
            let levels = resolve_levels(&line, base);
            assert_eq!(levels.len(), line.len());
            let mut start = 0;
            while start < line.len() {
                let end = line_classes[start..]
                    .iter()
                    .position(|&c| c == B)
                    .map_or(line.len(), |off| start + off + 1);
                let para_classes = &line_classes[start..end];
                match_isolates(para_classes, &mut matching);
                let para = paragraph_level_of(para_classes, &matching, base);
                for &l in &levels[start..end] {
                    assert!(l >= para, "{line:?}");
                }
                start = end;
            }
        }
    }
}

/// A line with no strong-RTL and no Arabic number is pure LTR → identity.
#[test]
fn ltr_only_is_identity() {
    let mut rng = XorShift::new(0x17E0_17E0_17E0_17E0);
    for _ in 0..512 {
        let line = draw_line(&mut rng, LTR_ONLY);
        assert!(!has_bidi(&line));
        let order = reorder_visual_to_logical(&line, BaseDirection::Auto);
        assert_eq!(order, (0..line.len()).collect::<Vec<_>>());
    }
}

/// The class-taking entry points, and the `_into` companion with one
/// Scratch reused across bases, are byte-identical to the char-taking
/// entry points (the dedup the bridge depends on).
#[test]
fn class_taking_and_into_variants_match_char_taking() {
    let mut scratch = Scratch::default();
    let mut cell_order = Vec::new();
    for line in lines() {
        let classes: Vec<BidiClass> = line.iter().map(|&c| bidi_class(c)).collect();
        let brackets: Vec<Bracket> = line.iter().map(|&c| bracket_of(c)).collect();
        assert_eq!(
            classes_and_brackets(&line),
            (classes.clone(), brackets.clone())
        );
        let wide = vec![false; line.len()];
        assert_eq!(has_bidi(&line), has_bidi_classes(&classes));
        for base in BASES {
            let levels = resolve_levels(&line, base);
            assert_eq!(levels, resolve_levels(&line, base), "deterministic");
            assert_eq!(
                levels,
                resolve_levels_from_classes(&classes, &brackets, base)
            );
            let cells = reorder_cells_with_classes(&classes, &brackets, &wide, base);
            assert_eq!(cells, reorder_visual_to_logical(&line, base));
            reorder_cells_with_classes_into(
                &classes,
                &brackets,
                &wide,
                base,
                &mut scratch,
                &mut cell_order,
            );
            assert_eq!(cell_order, cells, "{line:?}");
        }
    }
    // Partial `brackets` slices shorter than a paragraph still supply the
    // entries they have and default only the missing trailing indices to
    // `Bracket::None` (N0 paired brackets inside an RTL run).
    let partial_para: Vec<char> = "\u{05D0}(a)\u{05D1}x".chars().collect();
    let (p_cls, p_br) = classes_and_brackets(&partial_para);
    assert_eq!(
        resolve_levels_from_classes(&p_cls, &p_br[..p_cls.len() - 1], BaseDirection::Rtl),
        resolve_levels_from_classes(&p_cls, &p_br, BaseDirection::Rtl),
        "truncating a trailing non-bracket entry must preserve earlier bracket pairs"
    );
}

#[test]
fn ascii_table_and_hebrew_threshold_match_ucd_tables() {
    for u in 0..0x80u32 {
        let c = char::from_u32(u).expect("valid ASCII char");
        assert_eq!(
            bidi_class(c),
            bidi_class_table(u),
            "ASCII_BIDI_CLASS mismatch at U+{u:04X}"
        );
        assert_eq!(
            bracket_of(c),
            bracket_of_table(u),
            "ASCII bracket_of mismatch at U+{u:04X}"
        );
    }
    for u in 0..0x0590u32 {
        assert!(
            !is_bidi_class(bidi_class_table(u)),
            "codepoint U+{u:04X} below U+0590 must not be RTL-triggering"
        );
    }
    assert!(
        is_bidi_class(bidi_class_table(0x0590)),
        "U+0590 is the first RTL-triggering codepoint"
    );
    for u in 0..=0x10000u32 {
        let Some(c) = char::from_u32(u) else {
            continue;
        };
        assert_eq!(
            mirror_glyph(c),
            mirror_glyph_table(u),
            "mirror_glyph fast-path mismatch at U+{u:04X}"
        );
        assert_eq!(
            bracket_of(c),
            bracket_of_table(u),
            "bracket_of fast-path mismatch at U+{u:04X}"
        );
    }
}
