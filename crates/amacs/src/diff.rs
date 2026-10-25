// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The smallest edits that turn one text into another.
//!
//! A document that takes a new text from outside (a reload from disk, a
//! recovered draft) must not replace itself whole: every caret, selection,
//! mark and scroll anchor rebased through a `0..len` edit lands at the end of
//! the insert. [`minimal_edits`] keeps every unchanged line and word out of
//! the edits, so a position rebased through them stays on the same text
//! unless that text itself changed.

use std::ops::Range;

use crate::store::TextEdit;

/// The most piece edits one diff pass searches before it settles for one edit
/// over the changed middle. A larger difference is no longer a reload of the
/// same file in any useful sense, and the search's memory grows with its
/// square.
const MAX_EDITS: usize = 512;

/// The most piece comparisons one diff pass spends before it settles for one
/// edit over the changed middle, so a huge rewrite costs bounded time.
const MAX_COMPARISONS: usize = 4_000_000;

/// The edits that turn `old` into `new`, sorted and non-overlapping.
///
/// Unchanged lines stay out of every edit (a line diff); inside a changed run
/// of lines unchanged words stay out too (a word diff), and inside a changed
/// run of words only the bytes that differ are replaced, cut at char
/// boundaries. Applying the edits to `old` gives `new` exactly. Equal texts
/// give none.
#[must_use]
pub fn minimal_edits(old: &str, new: &str) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    if old != new {
        diff_pieces(
            old,
            0..old.len(),
            new,
            0..new.len(),
            Piece::Line,
            &mut edits,
        );
    }
    edits
}

/// What a diff pass compares a text as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Piece {
    /// Lines, each with its `\n`.
    Line,
    /// Words: a run of letters, digits and `_`, a run of spaces, or one other char.
    Word,
}

/// How [`Piece::Word`] sorts a char.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

impl CharClass {
    fn of(c: char) -> Self {
        if c.is_alphanumeric() || c == '_' {
            Self::Word
        } else if c != '\n' && c.is_whitespace() {
            Self::Space
        } else {
            Self::Other
        }
    }
}

/// The byte offset each piece of `text[range]` starts at, then `range.end`.
fn piece_starts(text: &str, range: Range<usize>, piece: Piece) -> Vec<usize> {
    let mut starts = vec![range.start];
    let part = &text[range.clone()];
    match piece {
        Piece::Line => starts.extend(part.match_indices('\n').map(|(at, _)| range.start + at + 1)),
        Piece::Word => {
            let mut previous = None;
            for (at, c) in part.char_indices() {
                let class = CharClass::of(c);
                if at > 0 && (class == CharClass::Other || previous != Some(class)) {
                    starts.push(range.start + at);
                }
                previous = Some(class);
            }
        }
    }
    if *starts.last().unwrap_or(&range.start) != range.end {
        starts.push(range.end);
    }
    starts
}

/// The edits that turn `old[old_range]` into `new[new_range]`, compared as
/// `piece`s, onto `edits`: unchanged pieces stay out, a changed run of lines
/// is compared again as words, and a changed run of words is cut to the bytes
/// that differ.
fn diff_pieces(
    old: &str,
    old_range: Range<usize>,
    new: &str,
    new_range: Range<usize>,
    piece: Piece,
    edits: &mut Vec<TextEdit>,
) {
    let old_starts = piece_starts(old, old_range, piece);
    let new_starts = piece_starts(new, new_range, piece);
    let old_count = old_starts.len() - 1;
    let new_count = new_starts.len() - 1;
    let old_piece = |index: usize| &old[old_starts[index]..old_starts[index + 1]];
    let new_piece = |index: usize| &new[new_starts[index]..new_starts[index + 1]];

    let mut head = 0usize;
    while head < old_count.min(new_count) && old_piece(head) == new_piece(head) {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < (old_count - head).min(new_count - head)
        && old_piece(old_count - 1 - tail) == new_piece(new_count - 1 - tail)
    {
        tail += 1;
    }
    let old_middle = head..old_count - tail;
    let new_middle = head..new_count - tail;
    let hunks = piece_hunks(
        &old_middle.clone().map(old_piece).collect::<Vec<_>>(),
        &new_middle.clone().map(new_piece).collect::<Vec<_>>(),
    )
    .unwrap_or_else(|| vec![(0..old_middle.len(), 0..new_middle.len())]);

    for (old_hunk, new_hunk) in hunks {
        let old_bytes = old_starts[head + old_hunk.start]..old_starts[head + old_hunk.end];
        let new_bytes = new_starts[head + new_hunk.start]..new_starts[head + new_hunk.end];
        if piece == Piece::Line && !old_bytes.is_empty() && !new_bytes.is_empty() {
            diff_pieces(old, old_bytes, new, new_bytes, Piece::Word, edits);
        } else if let Some(edit) = refine(old, old_bytes, new, new_bytes) {
            edits.push(edit);
        }
    }
}

/// One edit for a changed run of pieces: only the bytes between their common
/// prefix and common suffix, or none when the run is unchanged.
fn refine(
    old: &str,
    old_bytes: Range<usize>,
    new: &str,
    new_bytes: Range<usize>,
) -> Option<TextEdit> {
    let from = &old[old_bytes.clone()];
    let to = &new[new_bytes];
    let mut prefix = from
        .bytes()
        .zip(to.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !from.is_char_boundary(prefix) || !to.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let room = from.len().min(to.len()) - prefix;
    let mut suffix = from
        .bytes()
        .rev()
        .zip(to.bytes().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !from.is_char_boundary(from.len() - suffix) || !to.is_char_boundary(to.len() - suffix) {
        suffix -= 1;
    }
    let range = old_bytes.start + prefix..old_bytes.end - suffix;
    let insert = &to[prefix..to.len() - suffix];
    (!range.is_empty() || !insert.is_empty()).then(|| TextEdit {
        range,
        insert: insert.to_string(),
    })
}

/// The changed runs between two piece lists (Myers' shortest edit script), as
/// pairs of old and new piece ranges in order. `None` when the difference is
/// larger than [`MAX_EDITS`] or costs more than [`MAX_COMPARISONS`].
fn piece_hunks<'a>(old: &[&'a str], new: &[&'a str]) -> Option<Vec<(Range<usize>, Range<usize>)>> {
    // Pieces as small ids, so a comparison is one integer compare. A keyed
    // map: the pieces come from a file, and a file can be anyone's.
    let mut ids = aterm_hash::KeyedHashMap::<&'a str, usize>::default();
    let mut id_of = |line: &'a str| {
        let next = ids.len();
        *ids.entry(line).or_insert(next)
    };
    let a = old.iter().map(|line| id_of(line)).collect::<Vec<_>>();
    let b = new.iter().map(|line| id_of(line)).collect::<Vec<_>>();
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Some(vec![(0..n, 0..m)]);
    }

    let limit = (n + m).min(MAX_EDITS);
    let offset = limit + 1;
    // `v[k + offset]`: the furthest old piece reached on diagonal `k = x - y`.
    let mut v = vec![0usize; 2 * offset + 1];
    // `trace[d]`: diagonals `-d..=d` of `v` as edit `d` began.
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut comparisons = 0usize;
    let mut found = None;
    'search: for d in 0..=limit {
        trace.push(v[offset - d..=offset + d].to_vec());
        for k in (-(d as isize)..=d as isize).step_by(2) {
            let at = (k + offset as isize) as usize;
            let mut x = if k == -(d as isize) || (k != d as isize && v[at - 1] < v[at + 1]) {
                v[at + 1]
            } else {
                v[at - 1] + 1
            };
            let mut y = (x as isize - k) as usize;
            let start = x;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            comparisons += 1 + (x - start);
            v[at] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
        }
        if comparisons > MAX_COMPARISONS {
            return None;
        }
    }
    let found = found?;

    // Walk back from the end, keeping each matched piece pair.
    let mut matched = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=found).rev() {
        let row = &trace[d];
        let k = x as isize - y as isize;
        let get = |k: isize| row[(k + d as isize) as usize];
        let previous_k = if d == 0 {
            0
        } else if k == -(d as isize) || (k != d as isize && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = if d == 0 { 0 } else { get(previous_k) };
        let previous_y = (previous_x as isize - previous_k) as usize;
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            matched.push((x, y));
        }
        x = previous_x;
        y = previous_y;
    }
    matched.reverse();

    let mut hunks = Vec::new();
    let (mut old_at, mut new_at) = (0usize, 0usize);
    for (x, y) in matched.into_iter().chain(std::iter::once((n, m))) {
        if x > old_at || y > new_at {
            hunks.push((old_at..x, new_at..y));
        }
        old_at = x + 1;
        new_at = y + 1;
    }
    Some(hunks)
}

#[cfg(test)]
mod tests {
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
}
