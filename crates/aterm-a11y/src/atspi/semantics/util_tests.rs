// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/util.rs, and changed by the aterm project in 2026.

use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::model::{Node as NodeData, NodeId as LocalId, Role, Tree as TreeData, TreeUpdate};
use crate::tree::Tree;

/// A text viewport like aterm-gui's Settings editor.
fn viewport(lines: &[&str]) -> TreeUpdate {
    let mut root = NodeData::new(Role::MultilineTextInput);
    let runs: Vec<LocalId> = (1..=lines.len() as u64).map(LocalId).collect();
    root.set_children(runs.clone());
    let mut nodes = vec![(LocalId(0), root)];
    for (id, line) in runs.into_iter().zip(lines) {
        let mut run = NodeData::new(Role::TextRun);
        run.set_value(*line);
        run.set_character_lengths(line.chars().map(|c| c.len_utf8() as u8).collect::<Vec<_>>());
        nodes.push((id, run));
    }
    TreeUpdate {
        nodes,
        tree: Some(TreeData::new(LocalId(0))),
        tree_id: crate::model::TreeId::ROOT,
        focus: LocalId(0),
    }
}

type Answers = Vec<(i32, Granularity, Option<(String, usize, usize)>)>;

/// Every granularity at every offset, as `(text, start, end)`.
fn answers(lines: &'static [&'static str]) -> Answers {
    let tree = Tree::new(viewport(lines), true);
    let node = tree.state().root();
    let count = lines.iter().map(|l| l.chars().count()).sum::<usize>() as i32;
    let mut out = Vec::new();
    for offset in 0..=count {
        for g in [
            Granularity::Char,
            Granularity::Word,
            Granularity::Line,
            Granularity::Paragraph,
        ] {
            let answer = text_range_from_offset(&node, offset, g).ok().map(|r| {
                (
                    r.text(),
                    r.start().to_global_usv_index(),
                    r.end().to_global_usv_index(),
                )
            });
            out.push((offset, g, answer));
        }
    }
    out
}

/// `answers`, on a thread.
fn answered(lines: &'static [&'static str]) -> Answers {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(answers(lines));
    });
    rx.recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("every search over {lines:?} returns"))
}

fn at(got: &Answers, offset: i32, g: Granularity) -> Option<(&str, usize, usize)> {
    got.iter()
        .find(|(o, gg, _)| *o == offset && *gg == g)
        .and_then(|(_, _, a)| a.as_ref())
        .map(|(text, start, end)| (text.as_str(), *start, *end))
}

/// accesskit_consumer 0.37.0's paragraph search never returned here:
/// walking back from "cd" to the blank line's start biased forward onto
/// "cd" again (the semantics oracle's recorder hung on this shape).
#[test]
fn a_blank_line_ends_no_paragraph_search() {
    let got = answered(&["ab", "", "cd"]);
    assert_eq!(at(&got, 3, Granularity::Paragraph), Some(("abcd", 0, 4)));
    assert_eq!(at(&got, 0, Granularity::Paragraph), Some(("abcd", 0, 4)));
    assert_eq!(at(&got, 3, Granularity::Line), Some(("cd", 2, 4)));
    assert_eq!(at(&got, 1, Granularity::Char), Some(("b", 1, 2)));
    let control = answered(&["ab", "cd"]);
    assert_eq!(
        at(&control, 3, Granularity::Paragraph),
        Some(("abcd", 0, 4))
    );
}

/// A viewport whose top line is blank (Settings or the Manual scrolled
/// to an empty line): accesskit_consumer's walk back from the first
/// text found nothing before the blank line and answered where it
/// began, at every offset, so its paragraph search never returned.
#[test]
fn a_blank_first_line_ends_no_paragraph_search() {
    let got = answered(&["", "cd"]);
    for offset in 0..=2 {
        assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("cd", 0, 2)));
    }
    assert_eq!(at(&got, 1, Granularity::Line), Some(("cd", 0, 2)));
    let got = answered(&["", "", "ab"]);
    for offset in 0..=2 {
        assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("ab", 0, 2)));
    }
    let got = answered(&["", ""]);
    assert_eq!(at(&got, 0, Granularity::Paragraph), Some(("", 0, 0)));
}

/// A line after a blank one that follows a line break starts a
/// paragraph: the break, not the blank line, is the text before it.
#[test]
fn a_line_after_a_blank_one_starts_the_paragraph_a_break_began() {
    let got = answered(&["x\n", "", "ab"]);
    for offset in 2..=4 {
        assert_eq!(at(&got, offset, Granularity::Paragraph), Some(("ab", 2, 4)));
    }
    for offset in 0..=1 {
        assert_eq!(
            at(&got, offset, Granularity::Paragraph),
            Some(("x\n", 0, 2))
        );
    }
}
