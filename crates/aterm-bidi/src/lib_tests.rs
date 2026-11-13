// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const ALEF: char = '\u{05D0}';

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

#[test]
fn pure_rtl_reverses() {
    let t = chars("\u{05D0}\u{05D1}\u{05D2}");
    assert!(has_bidi(&t));
    let (classes, _) = classes_and_brackets(&t);
    let mut matching = Vec::new();
    match_isolates(&classes, &mut matching);
    assert_eq!(
        first_strong_level(&classes, &matching, 0, classes.len()),
        Some(1)
    );
    assert_eq!(
        paragraph_level_of(&classes, &matching, BaseDirection::Auto),
        1
    );
    assert_eq!(
        reorder_visual_to_logical(&t, BaseDirection::Auto),
        vec![2, 1, 0]
    );
}

#[test]
fn has_bidi_false_is_identity_for_auto_and_ltr_only() {
    let t = chars("a!");
    assert!(!has_bidi(&t));
    assert_eq!(
        reorder_visual_to_logical(&t, BaseDirection::Auto),
        vec![0, 1]
    );
    assert_eq!(
        reorder_visual_to_logical(&t, BaseDirection::Ltr),
        vec![0, 1]
    );
    assert_eq!(
        reorder_visual_to_logical(&t, BaseDirection::Rtl),
        vec![1, 0]
    );
}

#[test]
fn multi_paragraph_reorder_keeps_paragraphs_disjoint() {
    let t = chars("\u{05D0}\u{05D1}\n\u{05D2}\u{05D3}");
    let levels = resolve_levels(&t, BaseDirection::Auto);
    assert_eq!(levels, vec![1, 1, 1, 1, 1]);
    assert_eq!(
        reorder_visual_to_logical(&t, BaseDirection::Auto),
        vec![2, 1, 0, 4, 3]
    );
    assert_eq!(reorder_from_levels(&levels), vec![4, 3, 2, 1, 0]);
}

const CJK: char = '\u{4E2D}';

#[test]
fn reorder_cells_wide_glyph_stays_paired_ltr() {
    let cc = vec![CJK, ' '];
    let wide = vec![false, true];
    let (classes, brackets) = classes_and_brackets(&cc);
    assert_eq!(
        reorder_cells_with_classes(&classes, &brackets, &wide, BaseDirection::Auto),
        vec![0, 1]
    );
}

#[test]
fn reorder_cells_rtl_with_wide_glyph_keeps_pair_unmirrored() {
    let cc = vec![ALEF, CJK, ' '];
    let wide = vec![false, false, true];
    let (classes, brackets) = classes_and_brackets(&cc);
    assert_eq!(
        reorder_cells_with_classes(&classes, &brackets, &wide, BaseDirection::Auto),
        vec![1, 2, 0]
    );
}

/// `cell_levels` folds the row as the reorder does: a Hebrew run is odd, an
/// LTR word and the blank between them even, and a wide glyph's
/// continuation cell carries its lead's level (here an `L` glyph inside an
/// RTL paragraph, level 2, and the paragraph's RTL letter, level 1).
#[test]
fn cell_levels_marks_rtl_cells_and_pairs_wide_glyphs() {
    let cc = chars("ab \u{05D0}\u{05D1}");
    let classes: Vec<BidiClass> = cc.iter().map(|&c| bidi_class(c)).collect();
    let wide = vec![false; cc.len()];
    assert_eq!(
        cell_levels(&classes, &[], &wide, BaseDirection::Ltr),
        vec![0, 0, 0, 1, 1]
    );
    let cc = [ALEF, CJK, ' '];
    let classes: Vec<BidiClass> = cc.iter().map(|&c| bidi_class(c)).collect();
    let wide = [false, false, true];
    assert_eq!(
        cell_levels(&classes, &[], &wide, BaseDirection::Rtl),
        vec![1, 2, 2]
    );
}

#[test]
fn reorder_cells_is_always_a_permutation() {
    let cases: &[(Vec<char>, Vec<bool>)] = &[
        (vec![CJK, ' ', ALEF, '1'], vec![false, true, false, false]),
        (
            vec![ALEF, CJK, ' ', '!', 'a'],
            vec![false, false, true, false, false],
        ),
        (
            vec!['a', 'b', CJK, ' ', ' ', 'c', 'd'],
            vec![false, false, false, true, false, false, false],
        ),
    ];
    for (cc, wide) in cases {
        let (classes, brackets) = classes_and_brackets(cc);
        let order = reorder_cells_with_classes(&classes, &brackets, wide, BaseDirection::Auto);
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..cc.len()).collect::<Vec<_>>(),
            "not a permutation: {cc:?}"
        );
    }
    let overlong_wide = [false, false, true, true];
    let short_cc = [ALEF, 'a'];
    let (short_classes, short_brackets) = classes_and_brackets(&short_cc);
    let order = reorder_cells_with_classes(
        &short_classes,
        &short_brackets,
        &overlong_wide,
        BaseDirection::Auto,
    );
    let mut seen = order;
    seen.sort_unstable();
    assert_eq!(
        seen,
        vec![0, 1],
        "trailing wide flags past cell_classes.len() must not emit out-of-range indices"
    );
}

#[test]
fn reorder_cells_malformed_leading_continuation_is_safe() {
    let cc = vec![' ', 'a'];
    let wide = vec![true, false];
    let (classes, brackets) = classes_and_brackets(&cc);
    let order = reorder_cells_with_classes(&classes, &brackets, &wide, BaseDirection::Auto);
    let mut seen = order.clone();
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1]);
}

#[test]
fn into_variants_match_vec_returning() {
    let cell_cases: &[(Vec<char>, Vec<bool>)] = &[
        (chars("hello"), vec![false; 5]),
        (chars("\u{05D0}\u{05D1}\u{05D2}"), vec![false; 3]),
        (chars("hi \u{05D0}\u{05D1} bye"), vec![false; 9]),
        (chars("\u{05D0}12"), vec![false; 3]),
        (chars("a \u{2067}(\u{05D0}) b\u{2069} c"), vec![false; 10]),
        (vec![ALEF, CJK, ' '], vec![false, false, true]),
        (vec![CJK, ' ', ALEF, '1'], vec![false, true, false, false]),
        (vec![' ', 'a'], vec![true, false]),
        (chars(""), vec![]),
    ];
    let mut scratch = Scratch::default();
    let mut cell_order = Vec::new();
    for (cc, wide) in cell_cases {
        let classes: Vec<BidiClass> = cc.iter().map(|&c| bidi_class(c)).collect();
        let brackets: Vec<Bracket> = cc.iter().map(|&c| bracket_of(c)).collect();
        for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
            reorder_cells_with_classes_into(
                &classes,
                &brackets,
                wide,
                base,
                &mut scratch,
                &mut cell_order,
            );
            assert_eq!(
                cell_order,
                reorder_cells_with_classes(&classes, &brackets, wide, base),
                "reorder_cells_with_classes_into {cc:?} {base:?}"
            );
            let levels = resolve_levels_from_classes(&classes, &brackets, base);
            let mut order_out = Vec::new();
            reorder_from_levels_into(&levels, &mut order_out);
            assert_eq!(order_out, reorder_from_levels(&levels), "{cc:?} {base:?}");
        }
    }
}

#[test]
fn mirror_glyph_pairs() {
    assert_eq!(mirror_glyph('('), Some(')'));
    assert_eq!(mirror_glyph(')'), Some('('));
    assert_eq!(mirror_glyph('<'), Some('>'));
    assert_eq!(mirror_glyph('\u{2264}'), Some('\u{2265}'));
    assert_eq!(mirror_glyph('\u{FF08}'), Some('\u{FF09}'));
    assert_eq!(mirror_glyph('a'), None);
    assert_eq!(mirror_glyph('\u{221B}'), None);
    for pair in mirroring::MIRROR_PAIRS.chunks(2) {
        let (a, b) = (
            char::from_u32(pair[0]).unwrap(),
            char::from_u32(pair[1]).unwrap(),
        );
        assert_eq!(mirror_glyph(a), Some(b));
    }
}

/// L4 on a reordered row: brackets enclosing right-to-left text at a
/// right-to-left level are RTL cells, the same brackets around an RTL word
/// in a left-to-right context are not, and a wide continuation answers as
/// its lead.
#[test]
fn cell_is_rtl_follows_resolved_levels() {
    let rtl_brackets = chars("\u{627}(\u{628})\u{62C}");
    let ltr_brackets = chars("(\u{645}\u{631})");
    for (text, want) in [
        (rtl_brackets, vec![true, true, true, true, true]),
        (ltr_brackets, vec![false, true, true, false]),
    ] {
        let classes: Vec<BidiClass> = text.iter().map(|&c| bidi_class(c)).collect();
        let brackets: Vec<Bracket> = text.iter().map(|&c| bracket_of(c)).collect();
        let wide = vec![false; text.len()];
        let mut scratch = Scratch::default();
        let mut order = Vec::new();
        reorder_cells_with_classes_into(
            &classes,
            &brackets,
            &wide,
            BaseDirection::Ltr,
            &mut scratch,
            &mut order,
        );
        let got: Vec<bool> = (0..text.len()).map(|c| scratch.cell_is_rtl(c)).collect();
        assert_eq!(got, want, "{text:?}");
        assert!(!scratch.cell_is_rtl(text.len()));
    }
    let wide_row = chars("\u{5D0}\u{FF08}\0\u{5D1}");
    let classes: Vec<BidiClass> = wide_row.iter().map(|&c| bidi_class(c)).collect();
    let brackets = vec![Bracket::None; wide_row.len()];
    let wide = [false, false, true, false];
    let mut scratch = Scratch::default();
    let mut order = Vec::new();
    reorder_cells_with_classes_into(
        &classes,
        &brackets,
        &wide,
        BaseDirection::Ltr,
        &mut scratch,
        &mut order,
    );
    assert!(scratch.cell_is_rtl(1) && scratch.cell_is_rtl(2));
}

#[test]
fn reused_scratch_matches_folded_maps_and_forward_expanded_levels() {
    let mut scratch = Scratch::default();
    let mut out = Vec::new();
    for text in [
        "",
        "a(אב)12",
        "א中 12ב",
        "a\u{2067}אב\u{2069}(1)",
        "א\nb(ג)1",
    ] {
        let chars: Vec<_> = text.chars().collect();
        let (classes, mut brackets) = classes_and_brackets(&chars);
        let n = classes.len();
        brackets.extend([bracket_of('('), bracket_of(')')]);
        for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
            for mask in 0..(1usize << (n + 1)) {
                // Includes malformed leading markers and a marker past the last cell.
                let wide: Vec<_> = (0..=n).map(|i| mask & (1 << i) != 0).collect();
                for bracket_len in [0, n / 2, n, n + 2] {
                    let brackets = &brackets[..bracket_len];
                    let mut folded = Scratch::default();
                    fold_cells_and_resolve(&classes, brackets, &wide, base, &mut folded);
                    let mut char_order = Vec::new();
                    reorder_paragraphs_into(&folded.logical, &folded.levels, &mut char_order);
                    let expected: Vec<_> = char_order
                        .iter()
                        .flat_map(|&c| {
                            let lead = folded.lead_cell[c];
                            lead..lead + 1 + usize::from(folded.has_cont[c])
                        })
                        .collect();
                    let levels = cell_levels(&classes, brackets, &wide, base);
                    reorder_cells_with_classes_into(
                        &classes,
                        brackets,
                        &wide,
                        base,
                        &mut scratch,
                        &mut out,
                    );
                    assert_eq!(
                        out, expected,
                        "{text:?} {base:?} mask={mask} brackets={bracket_len}"
                    );
                    for cell in 0..n + 2 {
                        assert_eq!(
                            scratch.cell_is_rtl(cell),
                            levels.get(cell).is_some_and(|l| l % 2 == 1),
                            "{text:?} {base:?} mask={mask} brackets={bracket_len} cell={cell}"
                        );
                    }
                    // Reuse a previously folded scratch on a direct row immediately.
                    reorder_cells_with_classes_into(
                        &classes,
                        brackets,
                        &[],
                        base,
                        &mut scratch,
                        &mut out,
                    );
                    let direct = cell_levels(&classes, brackets, &[], base);
                    for (cell, level) in direct.iter().enumerate() {
                        assert_eq!(scratch.cell_is_rtl(cell), level % 2 == 1);
                    }
                }
            }
        }
    }
    reorder_cells_with_classes_into(&[], &[], &[], BaseDirection::Auto, &mut scratch, &mut out);
    assert!(out.is_empty());
    assert!(!scratch.cell_is_rtl(0));
}
