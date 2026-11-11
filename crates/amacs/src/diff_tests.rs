// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn apply(old: &str, edits: &[TextEdit]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for edit in edits {
        assert!(
            edit.range.start >= at,
            "sorted and non-overlapping: {edits:?}"
        );
        out.push_str(&old[at..edit.range.start]);
        out.push_str(&edit.insert);
        at = edit.range.end;
    }
    out.push_str(&old[at..]);
    out
}

fn check(old: &str, new: &str) -> Vec<TextEdit> {
    let edits = minimal_edits(old, new);
    assert_eq!(apply(old, &edits), new, "{old:?} -> {new:?}: {edits:?}");
    edits
}

#[test]
fn equal_texts_need_no_edit() {
    assert!(check("a\nb\n", "a\nb\n").is_empty());
    assert!(check("", "").is_empty());
}

#[test]
fn a_deleted_line_is_the_only_edit() {
    let edits = check("a = 1\nb = 2\nc = 3\n", "a = 1\nc = 3\n");
    assert_eq!(
        edits,
        vec![TextEdit {
            range: 6..12,
            insert: String::new()
        }]
    );
}

#[test]
fn a_changed_value_replaces_only_its_bytes() {
    let edits = check(
        "x = 1\nblink = true\ny = 2\n",
        "x = 1\nblink = false\ny = 2\n",
    );
    assert_eq!(
        edits,
        vec![TextEdit {
            range: 14..17,
            insert: "fals".into()
        }],
        "only `tru` of `true` differs from `false`"
    );
}

#[test]
fn separate_changes_stay_separate_edits() {
    let old = "root = 1\nkeep\nkeep\nkeep\n[table]\nkey = 1\n";
    let new = "root = 2\nkeep\nkeep\nkeep\n[table]\nkey = 1\nadded = 3\n";
    let edits = check(old, new);
    assert_eq!(edits.len(), 2, "{edits:?}");
    assert!(edits[0].range.end <= 9 && edits[1].range.start >= old.len() - 1);
}

#[test]
fn cuts_land_on_char_boundaries() {
    check("é\n", "è\n");
    check("日本語\n", "日本人\n");
    check("x🙂y", "x🙃y");
    check("e\u{301}", "e\u{300}");
}

#[test]
fn whole_rewrites_and_empty_sides_hold() {
    check("", "a\nb");
    check("a\nb", "");
    check("one\ntwo\n", "three\nfour\nfive");
    check("no newline", "no newline\nnow one");
    check("a\r\nb\r\n", "a\r\nc\r\n");
}

#[test]
fn a_difference_past_the_search_bound_is_one_middle_edit() {
    let old = (0..3000).map(|i| format!("old {i}\n")).collect::<String>();
    let new = (0..3000).map(|i| format!("new {i}\n")).collect::<String>();
    let old = format!("head\n{old}tail\n");
    let new = format!("head\n{new}tail\n");
    let edits = check(&old, &new);
    assert_eq!(edits.len(), 1);
    assert!(edits[0].range.start >= "head\n".len());
    assert!(edits[0].range.end <= old.len() - "tail\n".len());
}

#[test]
fn inside_a_changed_line_unchanged_words_stay_out() {
    let edits = check("local disk two\n", "disk three\n");
    assert_eq!(
        edits[0],
        TextEdit {
            range: 0..6,
            insert: String::new()
        },
        "the word typed before the kept text goes alone: {edits:?}"
    );
    assert!(
        edits[1..].iter().all(|edit| edit.range.start >= 11),
        "{edits:?}"
    );
    check("a = [1, 2, 3]\n", "a = [1, 4, 3, 5]\n");
    check("  indented\tline \n", "indented line\n");
}

#[test]
fn every_edit_list_rebuilds_the_new_text_exactly() {
    // A fixed pseudo-random walk over texts of words, spaces, newlines and
    // multi-byte chars: whatever the change, the edits apply to the new text.
    let alphabet = [
        "a", "b", "key", " ", "  ", "\n", "=", "é", "日本", "🙂", "\r\n", "_x",
    ];
    let mut seed = 0x2545_f491_u64;
    let mut next = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for _ in 0..400 {
        let old = (0..next(40))
            .map(|_| alphabet[next(alphabet.len())])
            .collect::<String>();
        let mut new = old.clone();
        for _ in 0..next(4) {
            let mut at = next(new.len() + 1);
            while !new.is_char_boundary(at) {
                at -= 1;
            }
            if next(2) == 0 {
                new.insert_str(at, alphabet[next(alphabet.len())]);
            } else {
                let mut end = (at + next(6)).min(new.len());
                while !new.is_char_boundary(end) {
                    end += 1;
                }
                new.replace_range(at..end, "");
            }
        }
        check(&old, &new);
    }
}

#[test]
fn interleaved_changes_match_the_shared_lines() {
    let old = "a\nb\nc\nd\ne\nf\n";
    let new = "a\nB\nc\nd\nE\nf\ng\n";
    let edits = check(old, new);
    assert_eq!(edits.len(), 3, "{edits:?}");
}
