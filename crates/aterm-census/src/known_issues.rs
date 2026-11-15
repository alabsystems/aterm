// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The open-issues file, `docs/KNOWN_ISSUES.md`: its entries ([`entries`]), its parts by key
//! ([`parts`]), the entry-level three-way merge of three of its texts ([`merge`]), the three-way
//! merge of one part's texts line by line ([`merge_part`]), and a text with some parts rewritten
//! ([`rewrite`]).
//!
//! The file merges as a union of both sides' lines (`.gitattributes`), so two branches' new
//! entries never conflict; but a union merges lines, not entries. Where one side deletes an
//! entry (the commit that closes it) and the other changes lines beside it, Git finds a
//! conflict there and the union keeps both sides' lines, the deleted entry's among them; and
//! where Git's diff lines up the lines of one side's new entry with those of the other's
//! deleted one (their blank lines match), the union puts one entry's heading over another's
//! text. An entry-level merge decides each entry by the side that changed it.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use crate::guards::conflict_marker::{fence_closes, fence_open};

/// The open-issues file's repository path.
pub const INDEX: &str = "docs/KNOWN_ISSUES.md";

/// The key of the text above the first entry among a text's [`parts`]: no identifier is empty.
pub const HEAD: &str = "";

/// One entry of the file: a level-two heading (one with at most three spaces before it, outside
/// fenced code and HTML-comment blocks) and the lines after it, up to the next such heading of
/// level one or two or the end of the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'t> {
    /// The identifier its heading names (`ATERM-KI-<date>-<number>`), if it names one.
    pub id: Option<&'t str>,
    /// Its heading's line, counted from 1.
    pub line: usize,
    /// Its bytes in the text, from the start of its heading's line, the blank lines after it
    /// included.
    pub span: Range<usize>,
    /// Its text with no white space at its end: two copies of an entry are the same where these
    /// are, whatever blank lines a merge left after one.
    pub body: &'t str,
}

/// A line with at most three spaces before it, those spaces off: what a Markdown block can be.
fn block_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|&byte| byte == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// The level of the heading `line` is, where it is one of level one or two (`#` or `##`, then
/// a space, a tab or the line's end).
fn heading_level(line: &str) -> Option<usize> {
    let line = block_line(line)?;
    let level = line.bytes().take_while(|&byte| byte == b'#').count();
    let after = &line[level..];
    ((1..=2).contains(&level) && (after.is_empty() || after.starts_with([' ', '\t'])))
        .then_some(level)
}

/// The identifier a level-two heading `line` names: its first word, where that is `ATERM-KI-`,
/// digits, `-` and digits.
fn heading_id(line: &str) -> Option<&str> {
    let after = block_line(line)?.strip_prefix("##")?;
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let id = after.split_whitespace().next()?;
    let (date, number) = id.strip_prefix("ATERM-KI-")?.split_once('-')?;
    [date, number]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .then_some(id)
}

/// The entries of `text`, in order. What comes before the first is no entry, nor is a level-one
/// heading, which ends the entry before it.
pub fn entries(text: &str) -> Vec<Entry<'_>> {
    scan(text).0
}

/// [`entries`], and whether every fence and HTML comment `text` opens is closed at its end.
fn scan<'t>(text: &'t str) -> (Vec<Entry<'t>>, bool) {
    let mut found = Vec::new();
    let mut open: Option<(Option<&'t str>, usize, usize)> = None;
    let close = |found: &mut Vec<Entry<'t>>,
                 open: &mut Option<(Option<&'t str>, usize, usize)>,
                 end: usize| {
        if let Some((id, line, start)) = open.take() {
            found.push(Entry {
                id,
                line,
                span: start..end,
                body: text[start..end].trim_end(),
            });
        }
    };
    let mut fence = None;
    let mut comment = false;
    let mut at = 0;
    for (index, whole) in text.split_inclusive('\n').enumerate() {
        let start = at;
        at += whole.len();
        let line = whole.strip_suffix('\n').unwrap_or(whole);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((ch, length)) = fence {
            if fence_closes(line, ch, length) {
                fence = None;
            }
            continue;
        }
        if comment {
            comment = !line.contains("-->");
            continue;
        }
        if block_line(line).is_some_and(|line| line.starts_with("<!--")) {
            comment = !line.contains("-->");
            continue;
        }
        if let Some(opened) = fence_open(line) {
            fence = Some(opened);
            continue;
        }
        let Some(level) = heading_level(line) else {
            continue;
        };
        close(&mut found, &mut open, start);
        if level == 2 {
            open = Some((heading_id(line), index + 1, start));
        }
    }
    close(&mut found, &mut open, text.len());
    (found, fence.is_none() && !comment)
}

/// One part of a text ([`parts`]): its key, its bytes in the text (the white space after it
/// included) and its text with no white space at its end.
struct Part<'t> {
    key: &'t str,
    span: Range<usize>,
    body: &'t str,
}

/// The parts of `text` in order: the text above its first entry, then each entry; `None` where
/// [`parts`] reads none.
fn split(text: &str) -> Option<Vec<Part<'_>>> {
    let (found, closed) = scan(text);
    if !closed {
        return None;
    }
    let first = found.first().map_or(text.len(), |entry| entry.span.start);
    let mut split = vec![Part {
        key: HEAD,
        span: 0..first,
        body: text[..first].trim_end(),
    }];
    let mut at = first;
    for entry in found {
        let key = entry.id?;
        if entry.span.start != at || split.iter().any(|part| part.key == key) {
            return None;
        }
        at = entry.span.end;
        split.push(Part {
            key,
            span: entry.span,
            body: entry.body,
        });
    }
    (at == text.len()).then_some(split)
}

/// The parts of `text` in order, each its key and its text: the text above its first entry under
/// [`HEAD`], then each entry's [`Entry::body`] under its identifier. `None` where `text` is not
/// that and nothing else: where an entry names no identifier, or one another names; where a
/// level-one heading follows an entry, so its lines are in no part; or where a fence or an HTML
/// comment is still open at its end, so the entry it opened in would run over every heading
/// after it.
pub fn ordered(text: &str) -> Option<Vec<(&str, &str)>> {
    Some(
        split(text)?
            .into_iter()
            .map(|part| (part.key, part.body))
            .collect(),
    )
}

/// Each part of `text` by its key ([`ordered`]).
pub fn parts(text: &str) -> Option<BTreeMap<&str, &str>> {
    Some(ordered(text)?.into_iter().collect())
}

/// The side whose change an entry-level merge took ([`merge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The first side merged.
    Ours,
    /// The second.
    Theirs,
    /// Both: they made the same change, or neither changed it.
    Both,
}

/// What an entry-level merge decides of one part ([`merge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merged<'t> {
    /// It holds this text, the [`Side`] that changed it to this.
    Holds(&'t str, Side),
    /// It holds no such part: the [`Side`] that deleted it, the other holding it as the base
    /// does.
    Gone(Side),
    /// Both sides changed it, unlike each other: no entry-level merge decides it.
    Open,
}

/// The entry-level three-way merge of `ours` and `theirs` over `base` (each a text's [`parts`]):
/// each key any of the three names, as the side that changed it holds it.
pub fn merge<'t>(
    base: &BTreeMap<&'t str, &'t str>,
    ours: &BTreeMap<&'t str, &'t str>,
    theirs: &BTreeMap<&'t str, &'t str>,
) -> BTreeMap<&'t str, Merged<'t>> {
    let keys = base.keys().chain(ours.keys()).chain(theirs.keys());
    let mut merged = BTreeMap::new();
    for &key in keys {
        let held = |side: &BTreeMap<&'t str, &'t str>| side.get(key).copied();
        let (was, mine, other) = (held(base), held(ours), held(theirs));
        let (now, side) = if mine == other {
            (mine, Side::Both)
        } else if mine == was {
            (other, Side::Theirs)
        } else if other == was {
            (mine, Side::Ours)
        } else {
            merged.insert(key, Merged::Open);
            continue;
        };
        merged.insert(
            key,
            now.map_or(Merged::Gone(side), |text| Merged::Holds(text, side)),
        );
    }
    merged
}

/// `text` made to hold the parts `want` names, in its order, each with the text `want` gives it
/// (a key and a text each, as [`ordered`] reads them); a part of `text` that `want` does not name
/// is taken out. A part `text` holds with that text, followed there by the part `want` names
/// after it (or, as the last, by none), keeps its bytes, the white space after it included. Each
/// other part is followed by the white space after it in `text` where that holds a blank line
/// and it is not the last, else by one blank line, or as the last by one line end (CRLF ones
/// where `text` holds a CRLF); an empty one is left out. `None` where [`parts`] reads no
/// `text`.
pub fn rewrite(text: &str, want: &[(&str, &str)]) -> Option<String> {
    let split = split(text)?;
    let crlf = text.contains("\r\n");
    let mut out = String::with_capacity(text.len());
    for (index, &(key, body)) in want.iter().enumerate() {
        let held = split.iter().position(|part| part.key == key);
        let next = want.get(index + 1).map(|&(next, _)| next);
        if let Some(at) = held
            && split[at].body == body
            && next == split.get(at + 1).map(|part| part.key)
        {
            out.push_str(&text[split[at].span.clone()]);
            continue;
        }
        if body.is_empty() {
            continue;
        }
        out.push_str(body);
        let after = held.map_or("", |at| {
            let part = &split[at];
            &text[part.span.start + part.body.len()..part.span.end]
        });
        out.push_str(match next {
            Some(_) if after.contains("\n\n") || after.contains("\n\r\n") => after,
            Some(_) if crlf => "\r\n\r\n",
            Some(_) => "\n\n",
            None if crlf => "\r\n",
            None => "\n",
        });
    }
    Some(out)
}

/// The line end of `texts`: CRLF where one holds one, else LF.
fn ending(texts: &[&str]) -> &'static str {
    if texts.iter().any(|text| text.contains("\r\n")) {
        "\r\n"
    } else {
        "\n"
    }
}

/// The pairs `(i, j)` of lines `a[i] == b[j]` that a longest common subsequence of `a` and `b`
/// matches, in order: of the subsequences that match the most lines holding more than white
/// space, one that matches the most lines, the earliest match taken first. So blank lines,
/// which every entry has, never line up two entries' text against each other.
pub fn matched(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (rows, width) = (a.len() + 1, b.len() + 1);
    let mut score = vec![(0_usize, 0_usize); rows * width];
    let at = |i: usize, j: usize| i * width + j;
    let take = |i: usize, j: usize, score: &[(usize, usize)]| {
        let (text, lines) = score[at(i + 1, j + 1)];
        (text + usize::from(!a[i].trim().is_empty()), lines + 1)
    };
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            let mut best = score[at(i + 1, j)].max(score[at(i, j + 1)]);
            if a[i] == b[j] {
                best = best.max(take(i, j, &score));
            }
            score[at(i, j)] = best;
        }
    }
    let (mut pairs, mut i, mut j) = (Vec::new(), 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] && take(i, j, &score) == score[at(i, j)] {
            pairs.push((i, j));
            (i, j) = (i + 1, j + 1);
        } else if score[at(i + 1, j)] == score[at(i, j)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// The three-way merge of the lines `ours` and `theirs` made of `base`. The base lines both
/// sides kept ([`matched`]) stand, and split the rest into hunks: a hunk one side left as the
/// base holds it, or both changed alike, is the other's; one both changed unlike each other
/// keeps neither side's base lines (each took out what the other kept) and both sides' new
/// lines, at each place in the hunk `ours`' first, a line both added there or elsewhere in the
/// hunk held once. So no line one side wrote is lost and no line one side took out stands.
pub fn merge_lines<'t>(base: &[&'t str], ours: &[&'t str], theirs: &[&'t str]) -> Vec<&'t str> {
    let side = |lines: &[&str]| {
        let mut of = vec![None; base.len()];
        for (i, j) in matched(base, lines) {
            of[i] = Some(j);
        }
        of
    };
    let (in_ours, in_theirs) = (side(ours), side(theirs));
    let mut out = Vec::with_capacity(ours.len().max(theirs.len()));
    let (mut from, mut from_ours, mut from_theirs) = (0, 0, 0);
    let stable = (0..base.len()).filter_map(|i| Some((i, in_ours[i]?, in_theirs[i]?)));
    for (to, to_ours, to_theirs) in stable
        .map(Some)
        .chain([None])
        .map(|stop| stop.unwrap_or((base.len(), ours.len(), theirs.len())))
    {
        let kept = |of: &[Option<usize>], start: usize| {
            of[from..to]
                .iter()
                .map(|held| held.map(|j| j - start))
                .collect::<Vec<_>>()
        };
        hunk(
            &mut out,
            &base[from..to],
            [&ours[from_ours..to_ours], &theirs[from_theirs..to_theirs]],
            [&kept(&in_ours, from_ours), &kept(&in_theirs, from_theirs)],
        );
        if to < base.len() {
            out.push(base[to]);
        }
        (from, from_ours, from_theirs) = (to + 1, to_ours + 1, to_theirs + 1);
    }
    out
}

/// One hunk of [`merge_lines`]: `base`'s lines and each side's (`sides`, ours then theirs),
/// and for each base line the side's line it kept (`kept`).
fn hunk<'t>(
    out: &mut Vec<&'t str>,
    base: &[&'t str],
    sides: [&[&'t str]; 2],
    kept: [&[Option<usize>]; 2],
) {
    let [ours, theirs] = sides;
    if ours == base || ours == theirs {
        out.extend_from_slice(theirs);
        return;
    }
    if theirs == base {
        out.extend_from_slice(ours);
        return;
    }
    // Each side's new lines, by the place in the hunk they follow (0 before its first base line).
    let new = |side: &[&'t str], kept: &[Option<usize>]| {
        let mut at = vec![Vec::new(); base.len() + 1];
        let mut place = 0;
        for (j, &line) in side.iter().enumerate() {
            match kept.iter().position(|&held| held == Some(j)) {
                Some(i) => place = i + 1,
                None => at[place].push(line),
            }
        }
        at
    };
    let (mine, other) = (new(ours, kept[0]), new(theirs, kept[1]));
    let mut both: HashMap<&str, usize> = HashMap::new();
    for &line in mine.iter().flatten() {
        *both.entry(line).or_insert(0) += 1;
    }
    for (place, lines) in mine.iter().enumerate() {
        out.extend_from_slice(lines);
        for &line in &other[place] {
            match both.get_mut(line) {
                Some(held) if *held > 0 => *held -= 1,
                _ => out.push(line),
            }
        }
    }
}

/// The three-way merge of part `key`'s texts `ours` and `theirs` made of `base` (each empty
/// where its text holds no such part): an entry's heading (its first line) as one value, the
/// side's that changed it (`theirs`' where both changed it unlike each other), and the rest by
/// [`merge_lines`]; the text above the first entry ([`HEAD`]) by [`merge_lines`] whole. The
/// lines are joined by CRLF where a text holds one, and the merge has no white space at its
/// end.
pub fn merge_part(key: &str, base: &str, ours: &str, theirs: &str) -> String {
    let [b, o, t] = [base, ours, theirs].map(|text| text.lines().collect::<Vec<_>>());
    let merged = if key == HEAD {
        merge_lines(&b, &o, &t)
    } else {
        fn head<'t>(lines: &[&'t str]) -> Option<&'t str> {
            lines.first().copied()
        }
        fn rest<'t>(lines: &[&'t str]) -> Vec<&'t str> {
            lines.get(1..).unwrap_or_default().to_vec()
        }
        let heading = if head(&o) == head(&b) {
            head(&t)
        } else if head(&t) == head(&b) {
            head(&o)
        } else {
            head(&t)
        };
        let mut merged: Vec<&str> = heading.into_iter().collect();
        merged.extend(merge_lines(&rest(&b), &rest(&o), &rest(&t)));
        merged
    };
    merged
        .join(ending(&[base, ours, theirs]))
        .trim_end()
        .to_owned()
}

#[cfg(test)]
#[path = "known_issues_tests.rs"]
mod tests;
