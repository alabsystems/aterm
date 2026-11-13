// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A wiped field keeps no text, no composition and no history: no undo brings a secret back.
#[test]
fn a_wiped_field_keeps_no_text_or_history() {
    let mut input = TextInputState::new(String::new());
    input.insert("river stone");
    input.insert(" lamp");
    assert!(input.undo());
    input.set_preedit("ex".into(), None);
    input.wipe();
    assert_eq!(input.value(), "");
    assert_eq!(input.preedit(), None);
    assert!(!input.undo(), "no history to undo into");
    assert!(!input.redo(), "no history to redo into");
    assert_eq!(input, TextInputState::new(String::new()));
}

#[test]
fn selection_replace_and_local_undo_redo_are_atomic() {
    let mut input = TextInputState::new("hello world".into());
    input.set_selection(6, 11);
    input.insert("aterm");
    assert_eq!(input.value(), "hello aterm");
    assert_eq!(input.selection(), &TextSelection::caret(11));
    assert!(input.undo());
    assert_eq!(input.value(), "hello world");
    assert_eq!(input.selection().range(), 6..11);
    assert!(input.redo());
    assert_eq!(input.value(), "hello aterm");
}

#[test]
fn grapheme_navigation_and_delete_keep_clusters_whole() {
    let text = "e\u{301}👩‍💻z";
    let mut input = TextInputState::new(text.into());
    input.move_left(false);
    input.delete_backward();
    assert_eq!(input.value(), "e\u{301}z");
    input.delete_backward();
    assert_eq!(input.value(), "z");
}

#[test]
fn preedit_is_visible_but_not_committed_until_ime_commit() {
    let mut input = TextInputState::new("ab".into());
    input.set_selection(1, 1);
    input.set_preedit("に".into(), Some(3..3));
    let (display, range) = input.display_projection();
    assert_eq!(display, "aにb");
    assert_eq!(range, Some(1..4));
    assert_eq!(input.value(), "ab");
    input.commit_preedit("日");
    assert_eq!(input.value(), "a日b");
    assert!(input.undo());
    assert_eq!(input.value(), "ab");
}

#[test]
fn invalid_platform_preedit_selection_fails_closed() {
    let mut input = TextInputState::new(String::new());
    input.set_preedit("é".into(), Some(1..99));
    assert_eq!(input.preedit().unwrap().selection, None);
}

#[test]
fn shift_motion_extends_and_plain_motion_collapses() {
    let mut input = TextInputState::new("abc".into());
    input.set_selection(1, 1);
    input.move_right(true);
    assert_eq!(input.selection().range(), 1..2);
    input.move_right(false);
    assert_eq!(input.selection(), &TextSelection::caret(2));
}

#[test]
fn projection_maps_ime_selection_to_grapheme_safe_display_offsets() {
    let mut input = TextInputState::new("aZb".into());
    input.set_selection(1, 2);
    input.set_preedit("e\u{301}日".into(), Some(1..3));
    let projection = input.projection();
    assert_eq!(projection.text, "ae\u{301}日b");
    assert_eq!(projection.preedit, Some(1..7));
    assert_eq!(projection.selection, TextSelection { anchor: 1, head: 4 });
}

#[test]
fn readline_home_end_and_kills_edit_at_the_caret() {
    let mut input = TextInputState::new("alpha beta".into());
    input.move_to_start(false);
    assert_eq!(input.selection(), &TextSelection::caret(0));
    input.move_to_end(false);
    assert_eq!(input.selection(), &TextSelection::caret(10));
    input.move_to_start(true);
    assert_eq!(input.selected_text(), "alpha beta");

    input.set_selection(5, 5);
    input.kill_to_end();
    assert_eq!(input.value(), "alpha");
    assert_eq!(input.selection(), &TextSelection::caret(5));
    assert!(input.undo());
    assert_eq!(input.value(), "alpha beta");

    input.set_selection(6, 6);
    input.kill_to_start();
    assert_eq!(input.value(), "beta");
    assert_eq!(input.selection(), &TextSelection::caret(0));

    let mut idle = TextInputState::new("x".into());
    idle.move_to_end(false);
    idle.kill_to_end();
    assert_eq!(idle.value(), "x");
    assert!(!idle.undo());
}

#[test]
fn ctrl_w_deletes_the_previous_word_and_keeps_clusters_whole() {
    let mut input = TextInputState::new("one two  three ".into());
    input.delete_word_backward();
    assert_eq!(input.value(), "one two  ");
    input.delete_word_backward();
    assert_eq!(input.value(), "one ");
    input.delete_word_backward();
    assert_eq!(input.value(), "");
    input.delete_word_backward();
    assert_eq!(input.value(), "");

    let mut input = TextInputState::new("hello world".into());
    input.set_selection(8, 8);
    input.delete_word_backward();
    assert_eq!(input.value(), "hello rld");

    let mut input = TextInputState::new("alpha beta".into());
    input.set_selection(2, 4);
    input.delete_word_backward();
    assert_eq!(input.value(), "ala beta");

    let mut input = TextInputState::new("cat 👩‍💻e\u{301}".into());
    input.delete_word_backward();
    assert_eq!(input.value(), "cat ");
}

#[test]
fn option_delete_deletes_the_next_word_and_keeps_clusters_whole() {
    let mut input = TextInputState::new("one two  three".into());
    input.set_selection(3, 3);
    input.delete_word_forward();
    assert_eq!(input.value(), "one  three");
    assert_eq!(input.selection(), &TextSelection::caret(3));
    input.delete_word_forward();
    assert_eq!(input.value(), "one");
    input.delete_word_forward();
    assert_eq!(input.value(), "one");

    let mut input = TextInputState::new("alpha beta".into());
    input.set_selection(2, 4);
    input.delete_word_forward();
    assert_eq!(input.value(), "ala beta");

    let mut input = TextInputState::new("👩‍💻e\u{301} cat".into());
    input.set_selection(0, 0);
    input.delete_word_forward();
    assert_eq!(input.value(), " cat");
    assert!(input.undo());
    assert_eq!(input.value(), "👩‍💻e\u{301} cat");
}

#[test]
fn oversized_insert_is_bounded_without_splitting_the_last_grapheme() {
    let mut input = TextInputState::new(String::new());
    let mut paste = "x".repeat(MAX_TEXT_INPUT_BYTES - 1);
    paste.push_str("e\u{301}");
    input.insert(&paste);
    assert!(input.value().len() <= MAX_TEXT_INPUT_BYTES);
    assert!(input.value().is_char_boundary(input.value().len()));
    assert!(!input.value().ends_with('e'));
}

#[test]
fn alt_word_motion_walks_whole_words_and_round_trips() {
    let mut input = TextInputState::new("hello brave world".into());
    input.set_selection(0, 0);

    input.move_word_right(false);
    assert_eq!(
        input.selection().head,
        5,
        "⌥→ must land at the END of 'hello'"
    );
    input.move_word_right(false);
    assert_eq!(
        input.selection().head,
        11,
        "⌥→ must cross to the end of 'brave'"
    );

    input.move_word_left(false);
    assert_eq!(
        input.selection().head,
        6,
        "⌥← must land at the START of 'brave'"
    );

    input.set_selection(0, 0);
    input.move_word_left(false);
    assert_eq!(input.selection().head, 0, "⌥← at the start is inert");
    let end = input.value().len();
    input.set_selection(end, end);
    input.move_word_right(false);
    assert_eq!(input.selection().head, end, "⌥→ at the end is inert");

    input.set_selection(0, 0);
    input.move_word_right(true);
    assert!(
        !input.selection().is_caret(),
        "shift-⌥→ must select, not just move"
    );
    assert_eq!(input.selection().head, 5);
}
