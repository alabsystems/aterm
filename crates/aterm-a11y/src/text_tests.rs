// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/text.rs, and cut by the aterm project in 2026 to what the
// AT-SPI Text interface reads (aterm's model has no text attributes, so no
// format runs, and no lines spanning runs), with fixes: a blank line no
// longer stalls or misplaces a paragraph search (see
// `Position::backward_to_line_start` and `Position::is_paragraph_start`),
// and positions stay in bounds: a word search past 255 characters does not
// wrap, a step past the document's end stops there, a run whose geometry is
// shorter than its text gives no boxes and a hit test reads none past its
// text, and a selection in another document is none.

use super::*;
use crate::model::{
    Node as Data, NodeId as LocalNodeId, Point, Rect, Role, TextDirection,
    TextPosition as WeakPosition, TextSelection, Tree, TreeId, TreeUpdate,
};
use crate::platform::Publication;

#[test]
fn word_navigation_past_255_characters_does_not_wrap_u8() {
    let mut root = Data::new(Role::Document);
    root.set_children(vec![LocalNodeId(2), LocalNodeId(3)]);

    let mut long_run = Data::new(Role::TextRun);
    let text = format!("word {}tail", "a".repeat(260));
    long_run.set_value(text.as_str());
    long_run.set_character_lengths(vec![1; text.len()]);
    long_run.set_word_starts(vec![0, 5]);

    let mut next_run = Data::new(Role::TextRun);
    next_run.set_value("next");
    next_run.set_character_lengths(vec![1; 4]);
    next_run.set_word_starts(vec![0]);

    let mut pub_state = Publication::default();
    pub_state.update(TreeUpdate {
        nodes: vec![
            (LocalNodeId(1), root),
            (LocalNodeId(2), long_run),
            (LocalNodeId(3), next_run),
        ],
        tree: Some(Tree::new(LocalNodeId(1))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(1),
    });
    let tree = pub_state.tree().unwrap();
    let doc = tree.root();

    let pos_256 = doc.text_position_from_global_usv_index(256).unwrap();
    assert_eq!(pos_256.inner.character_index, 256);
    assert!(
        !pos_256.is_word_start(),
        "index 256 must not wrap to u8 index 0"
    );

    let forward = pos_256.forward_to_word_start();
    assert_eq!(forward.inner.node.id(), NodeId(LocalNodeId(3)));
    assert_eq!(forward.inner.character_index, 0);

    let backward = pos_256.backward_to_word_start();
    assert_eq!(backward.inner.node.id(), NodeId(LocalNodeId(2)));
    assert_eq!(backward.inner.character_index, 5);
}

#[test]
fn forward_to_character_end_clamps_at_document_end_and_degenerate_cross_run_range_is_empty() {
    let mut root = Data::new(Role::Document);
    root.set_children(vec![LocalNodeId(2), LocalNodeId(3)]);

    let mut run1 = Data::new(Role::TextRun);
    run1.set_value("abc");
    run1.set_character_lengths(vec![1, 1, 1]);

    let mut run2 = Data::new(Role::TextRun);
    run2.set_value("def");
    run2.set_character_lengths(vec![1, 1, 1]);

    let mut pub_state = Publication::default();
    pub_state.update(TreeUpdate {
        nodes: vec![
            (LocalNodeId(1), root),
            (LocalNodeId(2), run1),
            (LocalNodeId(3), run2),
        ],
        tree: Some(Tree::new(LocalNodeId(1))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(1),
    });
    let tree = pub_state.tree().unwrap();
    let doc = tree.root();

    let end = doc.document_end();
    let stepped = end.forward_to_character_end();
    assert_eq!(stepped, end);
    assert_eq!(stepped.to_global_usv_index(), 6);

    let r1 = tree.node_by_id(NodeId(LocalNodeId(2))).unwrap();
    let r2 = tree.node_by_id(NodeId(LocalNodeId(3))).unwrap();
    let cross_degenerate = Range::new(
        doc,
        InnerPosition {
            node: r1,
            character_index: 3,
        },
        InnerPosition {
            node: r2,
            character_index: 0,
        },
    );
    assert!(cross_degenerate.is_degenerate());
    assert_eq!(cross_degenerate.text(), "");
}

#[test]
fn mismatched_geometry_lengths_and_foreign_text_selection_stay_in_bounds() {
    let mut window = Data::new(Role::Window);
    window.set_children(vec![LocalNodeId(2), LocalNodeId(4)]);

    let mut doc1 = Data::new(Role::Document);
    doc1.set_children(vec![LocalNodeId(3)]);
    doc1.set_text_direction(TextDirection::LeftToRight);
    doc1.set_text_selection(TextSelection {
        anchor: WeakPosition {
            node: LocalNodeId(5),
            character_index: 0,
        },
        focus: WeakPosition {
            node: LocalNodeId(5),
            character_index: 2,
        },
    });

    let mut short_geom_run = Data::new(Role::TextRun);
    short_geom_run.set_value("abcd");
    short_geom_run.set_character_lengths(vec![1, 1, 1, 1]);
    short_geom_run.set_bounds(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 40.0,
        y1: 10.0,
    });
    short_geom_run.set_character_positions(vec![0.0]);
    short_geom_run.set_character_widths(vec![10.0]);

    let mut doc2 = Data::new(Role::Document);
    doc2.set_children(vec![LocalNodeId(5)]);
    doc2.set_text_direction(TextDirection::LeftToRight);

    let mut long_geom_run = Data::new(Role::TextRun);
    long_geom_run.set_value("xy");
    long_geom_run.set_character_lengths(vec![1, 1]);
    long_geom_run.set_bounds(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 40.0,
        y1: 10.0,
    });
    long_geom_run.set_character_positions(vec![0.0, 10.0, 20.0, 30.0]);
    long_geom_run.set_character_widths(vec![10.0, 10.0, 10.0, 10.0]);

    let mut pub_state = Publication::default();
    pub_state.update(TreeUpdate {
        nodes: vec![
            (LocalNodeId(1), window),
            (LocalNodeId(2), doc1),
            (LocalNodeId(3), short_geom_run),
            (LocalNodeId(4), doc2),
            (LocalNodeId(5), long_geom_run),
        ],
        tree: Some(Tree::new(LocalNodeId(1))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(2),
    });
    let tree = pub_state.tree().unwrap();
    let d1 = tree.node_by_id(NodeId(LocalNodeId(2))).unwrap();
    assert!(d1.text_selection().is_none());
    assert!(d1.text_selection_focus().is_none());
    assert!(d1.document_range().bounding_boxes().is_empty());

    let d2 = tree.node_by_id(NodeId(LocalNodeId(4))).unwrap();
    let hit = d2.text_position_at_point(Point { x: 35.0, y: 5.0 });
    assert!(hit.inner.character_index <= 2);
    assert_eq!(hit.to_global_usv_index(), 2);
}

/// A run whose character lengths disagree with its text (one ending inside `é`, one past the
/// text, word starts past the run, a run with no lengths at all) answers every position,
/// word move and range on a char boundary instead of panicking or slicing inside a char.
#[test]
fn character_lengths_that_disagree_with_the_text_stay_on_char_boundaries() {
    let mut pub_state = Publication::default();
    let mut bad_doc = Data::new(Role::Document);
    bad_doc.set_children(vec![LocalNodeId(11), LocalNodeId(12)]);
    let mut mid_utf8_run = Data::new(Role::TextRun);
    mid_utf8_run.set_value("éx");
    mid_utf8_run.set_character_lengths(vec![1, 50]);
    mid_utf8_run.set_word_starts(vec![99]);
    let mut short_len_run = Data::new(Role::TextRun);
    short_len_run.set_value("tail");
    short_len_run.set_character_lengths(Vec::new());
    pub_state.update(TreeUpdate {
        nodes: vec![
            (LocalNodeId(10), bad_doc),
            (LocalNodeId(11), mid_utf8_run),
            (LocalNodeId(12), short_len_run),
        ],
        tree: Some(Tree::new(LocalNodeId(10))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(10),
    });
    let bad_tree = pub_state.tree().unwrap();
    let bad_root = bad_tree.node_by_id(NodeId(LocalNodeId(10))).unwrap();
    for idx in 0..=6 {
        let pos = bad_root.text_position_from_global_usv_index(idx).unwrap();
        assert!(pos.to_global_usv_index() <= 6);
        let _ = pos.forward_to_word_start().to_global_usv_index();
        let _ = pos.backward_to_word_start().to_global_usv_index();
    }
    let sub_range = Range::new(
        bad_root,
        InnerPosition {
            node: bad_tree.node_by_id(NodeId(LocalNodeId(11))).unwrap(),
            character_index: 1,
        },
        InnerPosition {
            node: bad_tree.node_by_id(NodeId(LocalNodeId(11))).unwrap(),
            character_index: 2,
        },
    );
    assert_eq!(sub_range.text(), "éx");
}
