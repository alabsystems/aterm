// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::{EditorCommand, EditorWorkspace, KeyChord, Selection, editor};
use crate::store::DocumentViewId;
use std::collections::VecDeque;

/// A kill the document refuses takes nothing into the kill ring: with
/// carets on neighbouring lines, C-u 2 C-k asks for overlapping ranges.
#[test]
fn a_refused_kill_leaves_the_kill_ring_alone() {
    let text = "aa\nbb\ncc\ndd\n";
    let (mut store, mut workspace, mut view, document) = editor(text);
    view.selections = vec![Selection::caret(0), Selection::caret(3)];
    workspace
        .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
        .unwrap();
    workspace.insert_text(&mut store, &mut view, "2").unwrap();
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::KillLine),
        Err(EditorError::InvalidSelections)
    );
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);
    assert!(workspace.kill_ring.is_empty());
    assert_eq!(view.prefix_hud, None);
}

/// C-y takes the argument as Emacs does: C-u 2 C-y yanks the second most
/// recent kill and M-y goes on from there; a bare C-u yanks the latest and
/// leaves point before it.
#[test]
fn universal_argument_picks_the_kill_to_yank() {
    let (mut store, mut workspace, mut view, document) = editor("ab");
    workspace.kill_ring = VecDeque::from(["newest".to_string(), "older".to_string()]);
    view.selections = vec![Selection::caret(1)];
    workspace
        .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
        .unwrap();
    workspace.insert_text(&mut store, &mut view, "2").unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::Yank)
        .unwrap();
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "aolderb");
    workspace
        .execute(&mut store, &mut view, EditorCommand::YankPop)
        .unwrap();
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "anewestb");

    let (mut store, mut workspace, mut view, document) = editor("ab");
    workspace.kill_ring = VecDeque::from(["newest".to_string(), "older".to_string()]);
    view.selections = vec![Selection::caret(1)];
    workspace
        .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
        .unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::Yank)
        .unwrap();
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "anewestb");
    assert_eq!(view.primary_selection(), &Selection::caret(1));
}

fn press(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    keys: &str,
) {
    for key in keys.split_whitespace() {
        workspace
            .handle_chord(
                store,
                view,
                KeyChord::parse(key).expect("test chord parses"),
            )
            .unwrap();
    }
}

/// A kill straight after a kill joins its kill-ring entry, as Emacs's
/// kill-append does: `C-k C-k` yanks back the line and its newline as one,
/// a backward kill goes in front, and `C-u` between kills keeps the run.
#[test]
fn kill_append_joins_consecutive_kills() {
    let (mut store, mut workspace, mut view, document) = editor("one\ntwo\nthree\n");
    press(&mut store, &mut workspace, &mut view, "C-k C-k C-u 1 C-k");
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "three\n");
    assert_eq!(workspace.kill_ring, ["one\ntwo\n"]);
    press(&mut store, &mut workspace, &mut view, "C-y");
    assert_eq!(
        store.snapshot(document).unwrap().text.as_ref(),
        "one\ntwo\nthree\n"
    );

    let (mut store, mut workspace, mut view, _) = editor("alpha beta gamma");
    view.selections = vec![Selection::caret(16)];
    press(
        &mut store,
        &mut workspace,
        &mut view,
        "M-backspace M-backspace",
    );
    assert_eq!(workspace.kill_ring, ["beta gamma"]);
    press(&mut store, &mut workspace, &mut view, "C-a M-d");
    assert_eq!(
        workspace.kill_ring,
        ["alpha", "beta gamma"],
        "a motion ends the run"
    );

    for between in ["C-g", "C-f C-b"] {
        let (mut store, mut workspace, mut view, _) = editor("ab\ncd\n");
        press(&mut store, &mut workspace, &mut view, "C-k");
        press(&mut store, &mut workspace, &mut view, between);
        press(&mut store, &mut workspace, &mut view, "C-k");
        assert_eq!(workspace.kill_ring.len(), 2, "{between}");
    }
    let (mut store, mut workspace, mut view, _) = editor("ab\ncd\n");
    press(&mut store, &mut workspace, &mut view, "C-k");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    press(&mut store, &mut workspace, &mut view, "C-k");
    assert_eq!(workspace.kill_ring, ["\n", "ab"], "typing ends the run");

    let (mut store, mut workspace, mut view, _) = editor("one two three");
    view.selections = vec![Selection { anchor: 0, head: 3 }];
    press(&mut store, &mut workspace, &mut view, "M-w C-k");
    assert_eq!(workspace.kill_ring, [" two three", "one"]);

    let (mut store, mut workspace, mut view, _) = editor("one\ntwo\n");
    press(&mut store, &mut workspace, &mut view, "C-k");
    workspace.select_whole_document(&store, &mut view).unwrap();
    press(&mut store, &mut workspace, &mut view, "C-w");
    assert_eq!(workspace.kill_ring, ["\ntwo\n", "one"]);

    let mut store = DocumentStore::for_test();
    let mut workspace = EditorWorkspace::new();
    let one = store.open("mem://one".into(), "one\n".into());
    let two = store.open("mem://two".into(), "two\n".into());
    let mut first = workspace
        .attach(&mut store, one, DocumentViewId(1))
        .unwrap();
    let mut second = workspace
        .attach(&mut store, two, DocumentViewId(2))
        .unwrap();
    press(&mut store, &mut workspace, &mut first, "C-k");
    press(&mut store, &mut workspace, &mut second, "C-k");
    press(&mut store, &mut workspace, &mut first, "C-k");
    assert_eq!(workspace.kill_ring, ["\n", "two", "one"]);
}

/// The kill ring keeps the newest sixty kills, newest first.
#[test]
fn the_kill_ring_stays_bounded_at_sixty() {
    let text = (0..70)
        .map(|line| format!("line{line}\n"))
        .collect::<String>();
    let (mut store, mut workspace, mut view, _) = editor(&text);
    for _ in 0..70 {
        press(&mut store, &mut workspace, &mut view, "C-k C-g C-d");
    }
    assert_eq!(workspace.kill_ring.len(), KILL_RING_LIMIT);
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("line69")
    );
    assert_eq!(
        workspace.kill_ring.back().map(String::as_str),
        Some("line10")
    );
}

#[test]
fn kill_yank_pop_and_macro_are_workspace_state() {
    let (mut store, mut workspace, mut view, document) = editor("first\nsecond");
    view.selections = vec![Selection { anchor: 0, head: 5 }];
    workspace
        .execute(&mut store, &mut view, EditorCommand::KillRegion)
        .unwrap();
    workspace.kill_ring.push_back("alternate".into());
    workspace
        .execute(&mut store, &mut view, EditorCommand::Yank)
        .unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::YankPop)
        .unwrap();
    assert_eq!(
        store.snapshot(document).unwrap().text.as_ref(),
        "alternate\nsecond"
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::StartMacro)
        .unwrap();
    workspace.insert_text(&mut store, &mut view, "!").unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::EndMacro)
        .unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::PlayMacro)
        .unwrap();
    assert!(
        store
            .snapshot(document)
            .unwrap()
            .text
            .ends_with("!!\nsecond")
    );
}
