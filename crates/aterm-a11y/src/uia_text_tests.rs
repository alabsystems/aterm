// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{Node as Data, NodeId, Role, Tree, TreeId, TreeUpdate};
use crate::platform::Publication;

fn document() -> Publication {
    let mut root = Data::new(Role::Document);
    root.set_children(vec![NodeId(2), NodeId(3), NodeId(4)]);
    let mut first = Data::new(Role::TextRun);
    first.set_value("A😀e\u{301}\n");
    first.set_character_lengths(vec![1, 4, 3, 1]);
    first.set_word_starts(vec![0, 2]);
    let mut blank = Data::new(Role::TextRun);
    blank.set_value("\n");
    blank.set_character_lengths(vec![1]);
    let mut last = Data::new(Role::TextRun);
    last.set_value("tail");
    last.set_character_lengths(vec![1; 4]);
    last.set_word_starts(vec![0]);
    let mut state = Publication::default();
    state.update(TreeUpdate {
        nodes: vec![
            (NodeId(1), root),
            (NodeId(2), first),
            (NodeId(3), blank),
            (NodeId(4), last),
        ],
        tree: Some(Tree::new(NodeId(1))),
        tree_id: TreeId::ROOT,
        focus: NodeId(1),
    });
    state
}

#[test]
fn model_characters_and_blank_lines_define_utf16_navigation() {
    let state = document();
    let doc = Document::new(state.tree().unwrap().root()).unwrap();
    assert_eq!(doc.characters, [0, 1, 3, 5, 6, 7, 8, 9, 10, 11]);
    assert_eq!(
        doc.enclosing(2, Unit::Character),
        (1, 3),
        "astral scalar is indivisible"
    );
    assert_eq!(
        doc.enclosing(4, Unit::Character),
        (3, 5),
        "combining model character is indivisible"
    );
    assert_eq!(
        doc.enclosing(6, Unit::Line),
        (6, 7),
        "blank line remains navigable"
    );
    assert_eq!(doc.enclosing(7, Unit::Paragraph), (7, 11));
    assert_eq!(doc.step(4, Unit::Character, -1), (3, -1));
    assert_eq!(doc.step(4, Unit::Character, 1), (5, 1));
    assert_eq!(doc.step(3, Unit::Word, -99), (0, -1));
    assert_eq!(doc.step(0, Unit::Document, i32::MAX), (11, 1));
    assert_eq!(doc.step(11, Unit::Character, 1), (11, 0));
    assert_eq!(doc.slice(1, 5), "😀e\u{301}");
}

/// A document of one run whose model characters are `value`'s scalars.
fn line(value: &str) -> Publication {
    let mut root = Data::new(Role::Document);
    root.set_children(vec![NodeId(2)]);
    let mut run = Data::new(Role::TextRun);
    run.set_value(value);
    run.set_character_lengths(
        value
            .chars()
            .map(|ch| u8::try_from(ch.len_utf8()).unwrap())
            .collect::<Vec<_>>(),
    );
    let mut state = Publication::default();
    state.update(TreeUpdate {
        nodes: vec![(NodeId(1), root), (NodeId(2), run)],
        tree: Some(Tree::new(NodeId(1))),
        tree_id: TreeId::ROOT,
        focus: NodeId(1),
    });
    state
}

#[test]
fn a_case_blind_search_keys_the_needle_as_it_keys_the_text() {
    let state = line("ΟΔΥΣΣΕΥΣ σοφός \u{212A}elvin GROẞ");
    let doc = Document::new(state.tree().unwrap().root()).unwrap();
    let find =
        |needle, backward, ignore_case| doc.find(0, doc.len(), needle, backward, ignore_case);
    for needle in ["ΟΔΥΣΣΕΥΣ", "οδυσσευς", "Οδυσσευσ"] {
        assert_eq!(
            find(needle, false, true),
            Some((0, 8)),
            "{needle}: a word-final sigma is one letter in either case"
        );
    }
    assert_eq!(find("ΣΟΦΌΣ", false, true), Some((9, 14)));
    assert_eq!(find("Σ", true, true), Some((13, 14)));
    assert_eq!(
        find("kelvin", false, true),
        Some((15, 21)),
        "the Kelvin sign is three bytes, its key one"
    );
    assert_eq!(find("groß", false, true), Some((22, 26)));
    assert_eq!(find("οδυσσευς", false, false), None);
    assert_eq!(find("σοφός", false, false), Some((9, 14)));
}
