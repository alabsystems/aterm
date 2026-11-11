// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::projection::{byte_of_line, line_number};
use crate::editor::{EditorCommand, editor};
use aterm_models_native::native_editor_viewport_model;
use aterm_spec::interp::admits;

#[test]
fn visible_line_capacity_is_the_clamp_it_replaced() {
    for lines in [0, 1, 2, 255, 256, 257, usize::MAX] {
        assert_eq!(visible_line_capacity(lines), lines.clamp(1, 256));
    }
}

/// `selections` is public, so a host can empty it; the primary selection
/// is then a caret at 0 (it used to panic), and an out-of-range `primary`
/// still names the last selection.
#[test]
fn primary_selection_of_an_emptied_view_is_a_caret_at_zero() {
    let (_, _, mut view, _) = editor("alpha\nbeta");
    view.selections = vec![Selection::caret(2), Selection::caret(7)];
    view.primary = 9;
    assert_eq!(view.primary_selection(), &Selection::caret(7));
    view.selections.clear();
    assert_eq!(view.primary_selection(), &Selection::caret(0));
    view.primary = 0;
    assert_eq!(view.primary_selection(), &Selection::caret(0));
}

/// THE PAINTED VIEW IS THE STORED VIEW: reconciling an unchanged capacity
/// (what every frame does on a copy) keeps a view scrolled away from the
/// caret where it was scrolled to, so the wheel scrolls what is painted
/// and nothing builds up unseen; a changed capacity (a resize) reveals the
/// caret.
#[test]
fn an_unchanged_capacity_never_pulls_a_scrolled_view_back_to_the_caret() {
    let text = (0..300)
        .map(|line| format!("line {line:03}\n"))
        .collect::<String>();
    let (_, _, mut view, _) = editor(&text);
    let lines = EditorLines::scanning(&text);
    view.reconcile_viewport(&text, 35);
    view.scroll_lines(&text, 3);
    let mut painted = view.clone();
    painted.reconcile_viewport_with(&lines, 35);
    assert_eq!(lines.number_at(painted.viewport_anchor), 3);
    assert_eq!(painted.viewport_anchor, view.viewport_anchor);
    view.scroll_lines(&text, -1);
    let mut painted = view.clone();
    painted.reconcile_viewport_with(&lines, 35);
    assert_eq!(
        lines.number_at(painted.viewport_anchor),
        2,
        "one line back paints"
    );
    let mut resized = view.clone();
    view.scroll_lines(&text, 100);
    resized.viewport_anchor = view.viewport_anchor;
    resized.reconcile_viewport_with(&lines, 20);
    assert_eq!(lines.number_at(resized.viewport_anchor), 0);
}

/// A PROMPT HOLDS THE VIEWPORT: while the minibuffer holds the keyboard, a changed capacity
/// (its footer wrapping onto another row) keeps the view scrolled away from the caret, the
/// anchor only clamped to the last full viewport; the same change with no prompt reveals the
/// caret (the NEGATIVE CONTROL), and closing the prompt reveals it again.
#[test]
fn a_prompt_holds_the_viewport_a_changed_capacity_would_move() {
    let text = (0..300)
        .map(|line| format!("line {line:03}\n"))
        .collect::<String>();
    let lines = EditorLines::scanning(&text);
    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.reconcile_viewport(&text, 35);
    view.scroll_lines(&text, 200);
    let scrolled = view.viewport_anchor;
    assert_eq!(
        lines.number_at(scrolled),
        200,
        "premise: the caret is far above"
    );

    let mut plain = view.clone();
    plain.reconcile_viewport(&text, 34);
    assert_eq!(
        lines.number_at(plain.viewport_anchor),
        0,
        "no prompt: revealed"
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
        .unwrap();
    assert!(view.minibuffer_active(), "premise: the prompt is open");
    assert!(view.reconcile_viewport(&text, 34));
    assert_eq!(
        view.viewport_anchor, scrolled,
        "the prompt keeps the scroll"
    );
    view.reconcile_viewport(&text, 250);
    assert_eq!(
        lines.number_at(view.viewport_anchor),
        lines.total() - 250,
        "clamped to the last full viewport"
    );
    workspace
        .execute(&mut store, &mut view, EditorCommand::Abort)
        .unwrap();
    assert!(!view.minibuffer_active());
    view.reconcile_viewport(&text, 35);
    assert_eq!(lines.number_at(view.viewport_anchor), 0, "closed: revealed");
}

#[test]
fn vertical_motion_preserves_a_bounded_text_column() {
    let (mut store, mut workspace, mut view, _) = editor("alpha\nβ\ngamma");
    view.selections = vec![Selection::caret(4)];
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
        .unwrap();
    assert_eq!(view.primary_selection().head, "alpha\nβ".len());
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
        .unwrap();
    assert_eq!(view.primary_selection().head, "alpha\nβ\ngamm".len());
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
        .unwrap();
    assert_eq!(view.primary_selection().head, "alpha\nβ".len());
}

#[test]
fn vertical_motion_preserves_display_cells_across_zwj_and_cjk_lines() {
    let text = "abcd\n👩‍💻xyz\n中xyz";
    let (mut store, mut workspace, mut view, _) = editor(text);
    view.selections = vec![Selection::caret("abcd".len())];

    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
        .unwrap();
    let emoji_target = "abcd\n".len() + "👩‍💻xy".len();
    assert_eq!(view.primary_selection().head, emoji_target);
    assert!(
        text.grapheme_indices()
            .any(|(byte, _)| byte == emoji_target)
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
        .unwrap();
    let cjk_target = "abcd\n👩‍💻xyz\n".len() + "中xy".len();
    assert_eq!(view.primary_selection().head, cjk_target);
    assert!(text.grapheme_indices().any(|(byte, _)| byte == cjk_target));

    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
        .unwrap();
    assert_eq!(view.primary_selection().head, emoji_target);
}

#[test]
fn viewport_scroll_and_caret_reveal_use_stable_line_anchors() {
    let text = (0..100)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (_, _, mut view, _) = editor(&text);
    view.scroll_lines(&text, 40);
    assert_eq!(&text[view.viewport_anchor..][..8], "line 40\n");
    view.selections = vec![Selection::caret(byte_of_line(&text, 90))];
    view.ensure_primary_visible(&text, 20);
    assert_eq!(line_number(&text, view.viewport_anchor), 73);
    view.selections = vec![Selection::caret(byte_of_line(&text, 5))];
    view.ensure_primary_visible(&text, 20);
    assert_eq!(line_number(&text, view.viewport_anchor), 5);
}

#[test]
fn viewport_scroll_stores_only_presentable_anchors_at_every_capacity() {
    let text = (0..12)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (_, _, mut view, _) = editor(&text);
    view.reconcile_viewport(&text, 4);

    view.scroll_lines(&text, i32::MAX);
    assert_eq!(line_number(&text, view.viewport_anchor), 9);
    let bottom = view.viewport_anchor;
    view.scroll_lines(&text, -1);
    assert_eq!(line_number(&text, view.viewport_anchor), 8);
    assert!(view.viewport_anchor < bottom, "one reverse step moves now");

    for short in ["", "one", "one\ntwo"] {
        let (_, _, mut short_view, _) = editor(short);
        short_view.reconcile_viewport(short, 40);
        short_view.scroll_lines(short, i32::MAX);
        assert_eq!(short_view.viewport_anchor, 0);
        short_view.scroll_lines(short, i32::MIN);
        assert_eq!(short_view.viewport_anchor, 0);
    }

    let (_, _, mut zero_view, _) = editor(&text);
    zero_view.viewport_lines = 0;
    zero_view.scroll_lines(&text, i32::MAX);
    assert_eq!(line_number(&text, zero_view.viewport_anchor), 12);
    zero_view.scroll_lines(&text, -1);
    assert_eq!(line_number(&text, zero_view.viewport_anchor), 11);
}

#[test]
fn real_viewport_reconciliation_conforms_to_renderer_capacity_model() {
    let text = (0..30)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (_, _, mut view, _) = editor(&text);
    view.selections = vec![Selection::caret(byte_of_line(&text, 20))];
    let short_text = "# Manual\nfont_px = 14\n";
    let (_, _, mut short_view, _) = editor(short_text);
    short_view.viewport_anchor = short_text.len();
    short_view.selections = vec![Selection::caret(short_text.len())];

    let model = native_editor_viewport_model();
    let before = model.init_state();
    assert!(view.reconcile_viewport(&text, 8));
    assert!(short_view.reconcile_viewport(short_text, 40));
    let mut after = before.clone();
    after.insert(
        "anchor_line",
        line_number(&text, view.viewport_anchor) as i64,
    );
    after.insert("visible_lines", view.viewport_lines() as i64);
    after.insert(
        "short_anchor_line",
        line_number(short_text, short_view.viewport_anchor) as i64,
    );
    after.insert("short_visible_lines", short_view.viewport_lines() as i64);
    after.insert("resized", 1);
    assert_eq!(admits(&model, &before, &after), Some("Resize"));
    assert!(model.check_invariant("CaretVisibleAfterResize", &after));
    assert!(model.check_invariant("ShortDocumentFullyVisible", &after));

    let scroll_text = (0..12)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let (_, _, mut scroll_view, _) = editor(&scroll_text);
    scroll_view.reconcile_viewport(&scroll_text, 4);
    scroll_view.scroll_lines(&scroll_text, i32::MAX);
    let scroll_before = model.init_state();
    let mut at_bottom = scroll_before.clone();
    at_bottom.insert(
        "scroll_anchor_line",
        line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
    );
    at_bottom.insert("scroll_phase", 1);
    assert_eq!(
        admits(&model, &scroll_before, &at_bottom),
        Some("Overscroll")
    );
    assert!(model.check_invariant("StoredScrollAnchorPresentable", &at_bottom));
    scroll_view.scroll_lines(&scroll_text, -1);
    let mut one_line_up = at_bottom.clone();
    one_line_up.insert(
        "scroll_anchor_line",
        line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
    );
    one_line_up.insert("scroll_phase", 2);
    assert_eq!(
        admits(&model, &at_bottom, &one_line_up),
        Some("ReverseScroll")
    );
    assert!(model.check_invariant("FirstReverseStepMoves", &one_line_up));

    let mut fixed_desktop = before.clone();
    fixed_desktop.insert("visible_lines", 8);
    fixed_desktop.insert("resized", 1);
    assert_eq!(admits(&model, &before, &fixed_desktop), None);
    assert!(!model.check_invariant("CaretVisibleAfterResize", &fixed_desktop));

    let mut stranded_short = after;
    stranded_short.insert("short_anchor_line", 2);
    assert_eq!(admits(&model, &before, &stranded_short), None);
    assert!(!model.check_invariant("ShortDocumentFullyVisible", &stranded_short));
}

#[test]
fn pointer_selection_collapses_extends_and_clamps_to_utf8() {
    let text = "aé🦀z\nsecond";
    let (_, _, mut view, _) = editor(text);

    assert!(view.pointer_select(text, 2, false, 4));
    assert_eq!(view.primary_selection(), &Selection::caret(1));

    assert!(view.pointer_select(text, "aé🦀".len(), true, 4));
    assert_eq!(view.primary_selection().range(), 1.."aé🦀".len());

    let first_line_end = text.find('\n').unwrap();
    assert!(view.pointer_select(text, first_line_end, false, 4));
    assert_eq!(view.primary_selection(), &Selection::caret(first_line_end));

    assert!(view.pointer_select(text, usize::MAX, false, 4));
    assert_eq!(view.primary_selection(), &Selection::caret(text.len()));
    assert!(!view.pointer_select(text, text.len(), false, 4));
}

#[test]
fn pointer_selection_clamps_to_complete_user_visible_graphemes() {
    let text = "ae\u{301}👩‍💻z";
    let (_, _, mut view, _) = editor(text);

    assert!(view.pointer_select(text, "ae".len(), false, 4));
    assert_eq!(view.primary_selection(), &Selection::caret("a".len()));

    let emoji_start = "ae\u{301}".len();
    assert!(view.pointer_select(text, emoji_start + "👩".len(), false, 4));
    assert_eq!(view.primary_selection(), &Selection::caret(emoji_start));

    let crlf = "ab\r\ncd";
    let (_, _, mut view, _) = editor(crlf);
    assert!(view.pointer_select(crlf, 3, false, 4));
    assert_eq!(
        view.primary_selection(),
        &Selection::caret(2),
        "the interior of CRLF retreats to the start of the pair"
    );
    assert!(!view.pointer_select(crlf, 2, false, 4));
}

#[test]
fn set_mark_persists_across_motion_and_kill_region() {
    let (mut store, mut workspace, mut view, document) = editor("alpha beta");
    workspace
        .execute(&mut store, &mut view, EditorCommand::SetMark)
        .unwrap();
    for _ in 0..5 {
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveForward)
            .unwrap();
    }
    assert!(view.mark_active);
    assert_eq!(view.primary_selection(), &Selection { anchor: 0, head: 5 });

    workspace
        .execute(&mut store, &mut view, EditorCommand::KillRegion)
        .unwrap();
    assert_eq!(store.snapshot(document).unwrap().text.as_ref(), " beta");
    assert_eq!(
        workspace.kill_ring.front().map(String::as_str),
        Some("alpha")
    );
    assert!(!view.mark_active);
    assert!(view.primary_selection().is_caret());
}

/// The words M-f then M-b find in `text`, as byte ranges.
fn motion_words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut at = 0;
    loop {
        let end = next_word_boundary(text, at, 1);
        let start = previous_word_boundary(text, end, 1);
        if end == at || start < at {
            return words;
        }
        words.push((start, end));
        at = end;
    }
}

/// The words a double-click selects in `text`, widened to the grapheme
/// clusters they touch: the editor's caret never lands inside a cluster.
fn clicked_words(text: &str) -> Vec<(usize, usize)> {
    let smart = aterm_core::selection::SmartSelection::new();
    let mut bounds: Vec<usize> = text.grapheme_indices().map(|(at, _)| at).collect();
    bounds.push(text.len());
    let mut words: Vec<(usize, usize)> = text
        .char_indices()
        .filter_map(|(at, _)| smart.word_boundaries_at(text, at))
        .map(|(start, end)| {
            let floor = bounds.iter().rev().find(|&&b| b <= start).copied();
            let ceil = bounds.iter().find(|&&b| b >= end).copied();
            (floor.unwrap_or(0), ceil.unwrap_or(text.len()))
        })
        .collect();
    words.dedup();
    words
}

/// M-f / M-b find the words a double-click selects. They used to call a word
/// any run of grapheme clusters that START with a letter, so a cluster led by
/// a prepended number sign (`۝١`) was no word, and a ZWJ or ZWNJ, which joins
/// the cluster before it, welded `foo` and `bar` into one word where a
/// double-click (and the find field) see two: an invisible character must not
/// weld two words together.
#[test]
fn word_motion_finds_the_words_a_double_click_selects() {
    for word in [
        "\u{06DD}\u{0661}\u{0662}\u{0663}",
        "\u{0645}\u{06CC}\u{200C}\u{062E}\u{0648}\u{0627}\u{0647}\u{0645}",
        "\u{0DC1}\u{0DCA}\u{200D}\u{0DBB}\u{0DD3}",
        "\u{09B0}\u{200D}\u{09CD}\u{09AF}\u{09BE}\u{09AC}",
        "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
        "\u{182C}\u{1820}\u{1837}\u{180E}\u{1820}",
    ] {
        let text = format!("x {word} y");
        let end = 2 + word.len();
        assert_eq!(
            motion_words(&text),
            [(0, 1), (2, end), (end + 1, end + 2)],
            "{word:?}"
        );
        assert_eq!(motion_words(&text), clicked_words(&text), "{word:?}");
    }
    for (text, words) in [
        ("foo\u{200D}bar", [(0, 6), (6, 9)]),
        ("foo\u{200C}bar", [(0, 6), (6, 9)]),
        ("abc\u{202E}def", [(0, 3), (6, 9)]),
        ("a\u{2060}b", [(0, 1), (4, 5)]),
        ("x\u{200E}y", [(0, 1), (4, 5)]),
        ("ab\u{180E}cd", [(0, 2), (5, 7)]),
    ] {
        assert_eq!(motion_words(text), words, "{text:?}");
        assert_eq!(motion_words(text), clicked_words(text), "{text:?}");
    }
}

/// M-f with a count reads the line once, from the last word break before the
/// caret: its cost is the distance it moves, pinned here by the words it reads.
#[test]
fn forward_word_applies_a_count_in_one_pass_from_the_last_break() {
    let text = "ab.".repeat(64 * 1024);
    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.selections = vec![Selection::caret(0)];
    view.prefix_argument = Some(PrefixArgument::Numeric {
        value: 1000,
        open: true,
    });
    EDITOR_WORDS_READ.with(|n| n.set(0));
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
        .unwrap();
    assert_eq!(view.primary_selection().head, 1000 * 3 - 1);
    let read = EDITOR_WORDS_READ.with(std::cell::Cell::get);
    assert!(read <= 1001, "C-u 1000 M-f read {read} words");

    view.selections = vec![Selection::caret(30_000 * 3)];
    EDITOR_WORDS_READ.with(|n| n.set(0));
    for _ in 0..1000 {
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
            .unwrap();
    }
    assert_eq!(view.primary_selection().head, 31_000 * 3 - 1);
    let read = EDITOR_WORDS_READ.with(std::cell::Cell::get);
    assert!(read <= 2 * 1000, "1000 M-f read {read} words");
}

#[test]
fn backward_word_scans_a_long_token_once_and_applies_counts_in_one_pass() {
    let token = "a".repeat(256 * 1024);
    let text = format!("{token} one two three");
    let one = token.len() + 1;
    let two = one + "one ".len();
    let three = two + "two ".len();

    EDITOR_WORDS_READ.with(|n| n.set(0));
    assert_eq!(previous_word_boundary(&text, text.len(), 3), one);
    assert_eq!(
        EDITOR_WORDS_READ.with(std::cell::Cell::get),
        4,
        "previous_word_boundary(count=3) scans the four words once"
    );
    assert_eq!(previous_word_boundary(&text, text.len(), 1), three);
    assert_eq!(previous_word_boundary(&text, text.len(), 2), two);
    assert_eq!(previous_word_boundary(&text, three, 1), two);
    assert_eq!(previous_word_boundary(&text, text.len(), 4), 0);

    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.selections = vec![Selection::caret(text.len())];
    view.prefix_argument = Some(PrefixArgument::Numeric {
        value: 3,
        open: true,
    });
    EDITOR_WORDS_READ.with(|n| n.set(0));
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveWordBackward)
        .unwrap();
    assert_eq!(view.primary_selection().head, one);
    assert_eq!(
        EDITOR_WORDS_READ.with(std::cell::Cell::get),
        4,
        "C-u 3 M-b applies count=3 in one pass over the four words"
    );
}

#[test]
fn motion_treats_combining_and_zwj_sequences_as_one_character() {
    let text = "e\u{301}👩‍💻z";
    let (mut store, mut workspace, mut view, _) = editor(text);
    view.selections = vec![Selection::caret(text.len())];
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveBackward)
        .unwrap();
    assert_eq!(view.primary_selection().head, "e\u{301}👩‍💻".len());
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveBackward)
        .unwrap();
    assert_eq!(view.primary_selection().head, "e\u{301}".len());
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveBackward)
        .unwrap();
    assert_eq!(view.primary_selection().head, 0);
}

#[test]
fn line_and_grapheme_boundaries_clamp_mid_char_offsets_and_keep_crlf_whole() {
    let text = "α\r\nβ\ne\u{301}";
    assert_eq!(line_start(text, 1), 0);
    assert_eq!(line_end(text, 1), 3);
    assert_eq!(previous_boundary(text, 1), 0);
    assert_eq!(next_boundary(text, 1), "α".len());
    assert_eq!(previous_boundary(text, "α\r\n".len()), "α".len());
    assert_eq!(previous_boundary(text, "α\r\nβ\n".len()), "α\r\nβ".len());
    assert_eq!(previous_boundary(text, text.len()), "α\r\nβ\n".len());
}
