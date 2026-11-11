// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The real scanner's byte boundary, normalized to the model's character
/// count, refines AsciiGraphemeFastPath with fresh and cached classes.
#[test]
fn ascii_boundaries_refine_the_derived_fast_path_model() {
    let model = aterm_models_core::ascii_grapheme_fast_path_model();
    for (text, actions) in [
        ("ab", &[][..]),
        ("a\u{0301}", &["SecondIsCombiningMark"][..]),
        ("ab", &["SecondIsCombiningMark", "SecondIsAscii"][..]),
    ] {
        let mut before = model.init_state();
        for action in actions {
            assert!(model.fire(action, &mut before));
        }
        let mut chars = text.chars();
        let first = chars.next().unwrap();
        let second = chars.next().unwrap();
        assert_eq!(before["c0_ascii"], i64::from(first.is_ascii()));
        assert_eq!(before["c1_ascii"], i64::from(second.is_ascii()));
        assert_eq!(
            before["c1_extend"],
            i64::from(gcb_class(second) == GCB::Extend)
        );
        for cached in [None, Some((GCB::Other, InCB::None))] {
            let (end, _) = find_first_cluster_end(text, cached);
            let mut observed = before.clone();
            observed.insert(
                "cluster_end",
                i64::try_from(text[..end].chars().count()).unwrap(),
            );
            observed.insert("decided", 1);
            assert!(
                model
                    .successors("FindClusterEnd", &before)
                    .contains(&observed),
                "{text:?}: {before:?} -> {observed:?}"
            );
            assert!(model.check_invariant("CombiningMarkNeverSplit", &observed));
            if before["c1_extend"] == 1 {
                let mut base_only = observed;
                base_only.insert("cluster_end", 1);
                assert!(
                    !model
                        .successors("FindClusterEnd", &before)
                        .contains(&base_only)
                );
                assert!(!model.check_invariant("CombiningMarkNeverSplit", &base_only));
                assert!(
                    aterm_spec::interp::with_buggy(&model, 1)
                        .successors("FindClusterEnd", &before)
                        .contains(&base_only)
                );
            }
        }
    }
}

#[test]
fn test_ethiopic_combining() {
    let g: Vec<&str> = "\u{1200}\u{135D}".graphemes().collect();
    assert_eq!(g, &["\u{1200}\u{135D}"]);
}

#[test]
fn test_extpict_zwj_extpict() {
    let s = "\u{1F91D}\u{200D}\u{1F91D}";
    let g: Vec<&str> = s.graphemes().collect();
    assert_eq!(g, &[s]);
}

#[test]
fn test_gb11_needs_extend_only_between_pictograph_and_zwj() {
    let (p, zwj, ext) = ("\u{1F44D}", "\u{200D}", "\u{0308}");
    for (head, why) in [
        (format!("{p}{zwj}{zwj}"), "ZWJ ZWJ"),
        (format!("{p}{zwj}{ext}{zwj}"), "ZWJ Extend ZWJ"),
        (format!("{p}{ext}{zwj}{ext}{zwj}"), "Extend ZWJ Extend ZWJ"),
    ] {
        let s = format!("{head}{p}");
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g, [head.as_str(), p], "{why}");
    }
    for joined in [
        format!("{p}{ext}{zwj}{p}"),
        format!("{p}{ext}{ext}{zwj}{p}{zwj}{p}"),
    ] {
        let g: Vec<&str> = joined.graphemes().collect();
        assert_eq!(g, [joined.as_str()]);
    }
}

#[test]
fn test_boundary_class_reuse_across_clusters() {
    let conjuncts = "\u{0915}\u{094D}\u{092F}\u{0915}\u{094D}\u{092F}";
    let g: Vec<&str> = conjuncts.graphemes().collect();
    assert_eq!(
        g,
        &["\u{0915}\u{094D}\u{092F}", "\u{0915}\u{094D}\u{092F}"],
        "GB9c: two conjuncts split exactly at the consonant boundary"
    );

    let ris = "\u{1F1FA}\u{1F1F8}\u{1F1EC}a";
    let g: Vec<&str> = ris.graphemes().collect();
    assert_eq!(g, &["\u{1F1FA}\u{1F1F8}", "\u{1F1EC}", "a"]);

    let emoji = "\u{1F600}\u{1F601}";
    let g: Vec<&str> = emoji.graphemes().collect();
    assert_eq!(g, &["\u{1F600}", "\u{1F601}"]);

    let idx: Vec<(usize, &str)> = conjuncts.grapheme_indices().collect();
    assert_eq!(idx[0].0, 0);
    assert_eq!(idx[1].0, "\u{0915}\u{094D}\u{092F}".len());
}

#[test]
fn test_plane14_tag_classification() {
    use crate::tables::gcb::{GCB, gcb_class};
    assert_eq!(gcb_class('\u{E0020}'), GCB::Extend);
    assert_eq!(gcb_class('\u{E0067}'), GCB::Extend);
    assert_eq!(gcb_class('\u{E007F}'), GCB::Extend);
    assert_eq!(gcb_class('\u{E0100}'), GCB::Extend);
    assert_eq!(gcb_class('\u{E01EF}'), GCB::Extend);
}
