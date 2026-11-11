// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The edit script, v1.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::ops::{Range, RangeInclusive};
use std::str::FromStr;

use super::anchor::{Anchor, Hash6, Lines};
use crate::store::TextEdit;

/// What an apply's body is written in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// The edit script.
    #[default]
    Script,
    /// A unified diff of one file.
    Patch,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Script => "script",
            Self::Patch => "patch",
        })
    }
}

impl FromStr for Format {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "script" => Ok(Self::Script),
            "patch" => Ok(Self::Patch),
            _ => Err(format!("bad format={text} (want script or patch)")),
        }
    }
}

/// Which matches a `substitute` replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// The one match.
    Unique,
    /// Every match (`all`).
    All,
    /// The k-th match, from 1 (`nth=<k>`).
    Nth(usize),
}

/// One op of a script, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// `replace A B <<W`.
    Replace {
        first: Anchor,
        last: Anchor,
        body: Vec<String>,
    },
    /// `insert after N <<W`.
    InsertAfter { anchor: Anchor, body: Vec<String> },
    /// `insert before N <<W`.
    InsertBefore { anchor: Anchor, body: Vec<String> },
    /// `insert at-start <<W`.
    InsertAtStart { body: Vec<String> },
    /// `insert at-end <<W`.
    InsertAtEnd { body: Vec<String> },
    /// `delete A B`.
    Delete { first: Anchor, last: Anchor },
    /// `substitute <<W1 ... with <<W2 ... [all|nth=k]`, literal.
    Substitute {
        old: Vec<String>,
        new: Vec<String>,
        which: Which,
    },
    /// One hunk of a `format=patch` diff.
    Hunk(PatchHunk),
}

/// One `@@ -a,b +c,d @@` hunk of a unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchHunk {
    /// The old side's first line (`a`).
    pub old_start: usize,
    /// The context and removed lines, which must match the text.
    pub old: Vec<String>,
    /// The context and added lines, which replace them.
    pub new: Vec<String>,
    /// The old side ends without an end-of-line (`\ No newline at end of file`).
    pub old_open: bool,
    /// The new side ends without an end-of-line.
    pub new_open: bool,
}

/// A parsed script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    ops: Vec<Op>,
}

/// Why a script did not apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    /// An anchor's line no longer has the h6 it was read with.
    Conflict {
        op: usize,
        line: usize,
        want: Hash6,
        have: Option<Hash6>,
    },
    /// A substitute matched more than once without `all` or `nth=`, or a `near=` search found the anchors both above and below.
    Ambiguous { op: usize, matches: usize },
    /// The op's change overlaps an earlier op's, or changes a line an insert hangs on.
    Overlap { op: usize },
    /// A substitute found no match (or fewer than `nth=`), or a patch hunk's line or end-of-file marker does not match the text.
    Missing { op: usize },
    /// `base=` names a sequence the document's history no longer reaches.
    Stale { base: u64 },
    /// The script does not read.
    Parse { line: usize, why: String },
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict {
                op,
                line,
                want,
                have,
            } => {
                write!(f, "conflict op={op} line={line} want={want} have=")?;
                match have {
                    Some(have) => write!(f, "{have}"),
                    None => f.write_str("none"),
                }
            }
            Self::Ambiguous { op, matches } => write!(f, "ambiguous op={op} matches={matches}"),
            Self::Overlap { op } => write!(f, "overlap op={op}"),
            Self::Missing { op } => write!(f, "missing op={op}"),
            Self::Stale { base } => write!(f, "stale base={base}"),
            Self::Parse { line, why } => write!(f, "parse line={line}: {why}"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// How a plan may move anchors.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanOptions<'m> {
    /// `near=N`: an op whose anchors no longer match may find them up to N lines away, all by the same shift.
    pub near: Option<usize>,
    /// `base=<seq>`.
    pub base_map: Option<&'m LineMap>,
}

/// One change of a text's lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub at: usize,
    pub removed: usize,
    pub inserted: usize,
}

/// What changed in a document since a sequence, as a map of its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMap {
    base: u64,
    reaches: bool,
    changes: Vec<LineChange>,
}

impl LineMap {
    /// The map at `base` with nothing changed since.
    #[must_use]
    pub const fn new(base: u64) -> Self {
        Self {
            base,
            reaches: true,
            changes: Vec::new(),
        }
    }

    /// The map of a `base` the history no longer reaches.
    #[must_use]
    pub const fn unreachable(base: u64) -> Self {
        Self {
            base,
            reaches: false,
            changes: Vec::new(),
        }
    }

    /// Record the next changes ([`line_changes`]).
    pub fn record(&mut self, changes: impl IntoIterator<Item = LineChange>) {
        self.changes.extend(changes);
    }

    /// The sequence the script's anchors were read at.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// Whether the history still reaches `base`.
    #[must_use]
    pub const fn reaches_base(&self) -> bool {
        self.reaches
    }

    /// Lines `first..=last` (1-based) at `base`, as lines of the current text.
    #[must_use]
    pub fn map_lines(&self, first: usize, last: usize) -> Option<(usize, usize)> {
        let (mut first, mut last) = (first, last);
        for change in &self.changes {
            let end = change.at.saturating_add(change.removed);
            let touched = if change.removed == 0 {
                first < change.at && change.at <= last
            } else {
                change.at <= last && first < end
            };
            if touched {
                return None;
            }
            first = shift_line(first, change);
            last = shift_line(last, change);
        }
        Some((first, last))
    }
}

const fn shift_line(line: usize, change: &LineChange) -> usize {
    if line < change.at {
        line
    } else {
        line.saturating_sub(change.removed)
            .saturating_add(change.inserted)
    }
}

/// Line changes produced by a transaction of non-overlapping `edits` on `pre_text`.
#[must_use]
pub fn line_changes(pre_text: &str, edits: &[TextEdit]) -> Vec<LineChange> {
    let lines = Lines::of(pre_text);
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| (edit.range.start, edit.range.end));
    let mut windows: Vec<Window<'_>> = Vec::new();
    for edit in sorted {
        let window = Window::of(&lines, edit);
        match windows.last_mut() {
            Some(open) if window.bytes.start < open.bytes.end => open.absorb(window),
            _ => windows.push(window),
        }
    }
    windows
        .iter()
        .rev()
        .map(|window| window.change(pre_text))
        .collect()
}

struct Window<'e> {
    /// The first line touched (1-based; one past the last for an append).
    first: usize,
    /// How many lines the window holds before the edits.
    lines: usize,
    bytes: Range<usize>,
    edits: Vec<&'e TextEdit>,
}

impl<'e> Window<'e> {
    fn of(lines: &Lines<'_>, edit: &'e TextEdit) -> Self {
        let text_len = lines.text().len();
        let start = edit.range.start.max(lines.origin());
        let end = edit.range.end.max(start);
        let count = lines.count();
        let first = lines.line_at(start).unwrap_or(if lines.open_ended() {
            count
        } else {
            count.saturating_add(1)
        });
        let first_start = lines.span(first).map_or(text_len, |span| span.start);
        let whole_line_insert = start == end
            && start == first_start
            && (edit.insert.is_empty() || edit.insert.ends_with('\n'));
        if whole_line_insert {
            return Self {
                first,
                lines: 0,
                bytes: start..start,
                edits: vec![edit],
            };
        }
        let mut last = if end > start {
            lines.line_at(end.saturating_sub(1)).unwrap_or(count)
        } else {
            first
        };
        let mut window_end = lines.span(last).map_or(text_len, |span| span.end);
        let text = lines.text();
        let new_tail = [
            text.get(end..window_end).unwrap_or(""),
            edit.insert.as_str(),
            text.get(first_start..start).unwrap_or(""),
        ]
        .into_iter()
        .find(|piece| !piece.is_empty());
        if new_tail.is_some_and(|tail| !tail.ends_with('\n'))
            && window_end < text_len
            && let Some(next) = lines.span(last.saturating_add(1))
        {
            last = last.saturating_add(1);
            window_end = next.end;
        }
        let held = if first > count {
            0
        } else {
            last.saturating_sub(first).saturating_add(1)
        };
        Self {
            first,
            lines: held,
            bytes: first_start.min(start)..window_end.max(end),
            edits: vec![edit],
        }
    }

    fn absorb(&mut self, other: Self) {
        self.bytes.end = self.bytes.end.max(other.bytes.end);
        let other_last = other
            .first
            .saturating_add(other.lines)
            .saturating_sub(1)
            .max(other.first);
        let held_last = self
            .first
            .saturating_add(self.lines)
            .saturating_sub(1)
            .max(other_last);
        self.lines = held_last.saturating_sub(self.first).saturating_add(1);
        self.edits.extend(other.edits);
    }

    fn change(&self, pre_text: &str) -> LineChange {
        let mut new_text = String::new();
        let mut cursor = self.bytes.start;
        for edit in &self.edits {
            let start = edit.range.start.max(cursor);
            new_text.push_str(pre_text.get(cursor..start).unwrap_or(""));
            new_text.push_str(&edit.insert);
            cursor = edit.range.end.max(start);
        }
        new_text.push_str(pre_text.get(cursor..self.bytes.end).unwrap_or(""));
        let breaks = new_text.bytes().filter(|byte| *byte == b'\n').count();
        let open = usize::from(!new_text.is_empty() && !new_text.ends_with('\n'));
        LineChange {
            at: self.first,
            removed: self.lines,
            inserted: breaks.saturating_add(open),
        }
    }
}

/// A script's plan against one text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The edits, sorted by position.
    pub edits: Vec<TextEdit>,
    /// One per edit, in script order.
    pub hunks: Vec<Hunk>,
}

/// Where one edit's new text sits in the post-text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The op it came from, from 1.
    pub op: usize,
    /// Its bytes in the post-text (empty for a deletion).
    pub post: Range<usize>,
}

impl Hunk {
    /// The fresh anchors of the hunk's first and last line in `post_text`.
    #[must_use]
    pub fn anchors(&self, post_text: &str) -> Option<(Anchor, Anchor)> {
        self.anchors_in(&Lines::of(post_text))
    }

    /// [`Self::anchors`] over the post-text's line table.
    fn anchors_in(&self, lines: &Lines<'_>) -> Option<(Anchor, Anchor)> {
        let anchor = |line: usize| lines.h6(line).map(|h6| Anchor { line, h6 });
        if self.post.is_empty() {
            let before = self
                .post
                .start
                .checked_sub(1)
                .filter(|byte| *byte >= lines.origin())
                .and_then(|byte| lines.line_at(byte));
            let after = lines.line_at(self.post.start);
            let first = before.or(after)?;
            let last = after.or(before)?;
            return Some((anchor(first)?, anchor(last)?));
        }
        let mut first = lines.line_at(self.post.start)?;
        if let Some(span) = lines.span(first)
            && self.post.start >= span.content_end
            && self.post.end > span.end
        {
            first = first.saturating_add(1);
        }
        let last = lines.line_at(self.post.end.saturating_sub(1))?.max(first);
        Some((anchor(first)?, anchor(last)?))
    }
}

impl Plan {
    /// The post-text.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for edit in &self.edits {
            out.push_str(text.get(cursor..edit.range.start).unwrap_or(""));
            out.push_str(&edit.insert);
            cursor = edit.range.end.max(cursor);
        }
        out.push_str(text.get(cursor..).unwrap_or(""));
        out
    }

    /// The success reply for the post-text committed at `seq`.
    #[must_use]
    pub fn reply(&self, post_text: &str, seq: u64) -> String {
        let lines = Lines::of(post_text);
        let mut out = format!("OK applied hunks={} seq={seq}\n", self.hunks.len());
        for (index, hunk) in self.hunks.iter().enumerate() {
            let number = index.saturating_add(1);
            let _ = match hunk.anchors_in(&lines) {
                Some((first, last)) => writeln!(out, "hunk {number} {first} {last}"),
                None => writeln!(out, "hunk {number} none"),
            };
        }
        out
    }

    /// The line changes this plan makes to `pre_text`, for a [`LineMap`].
    #[must_use]
    pub fn line_changes(&self, pre_text: &str) -> Vec<LineChange> {
        line_changes(pre_text, &self.edits)
    }
}

impl Script {
    /// The ops, in order.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Read a body in `format`.
    pub fn parse_as(format: Format, input: &str) -> Result<Self, ScriptError> {
        match format {
            Format::Script => Self::parse(input),
            Format::Patch => Self::parse_patch(input),
        }
    }

    /// Read an edit script (each [`Op`] variant names its op's form).
    pub fn parse(input: &str) -> Result<Self, ScriptError> {
        let lines = script_lines(input);
        let mut ops = Vec::new();
        let mut at = 0;
        while let Some(line) = lines.get(at) {
            let number = at.saturating_add(1);
            at = number;
            let words = words_of(line);
            let Some((keyword, rest)) = words.split_first() else {
                continue;
            };
            let parse = |why: String| ScriptError::Parse { line: number, why };
            let op = match *keyword {
                "replace" => {
                    let (before, word, after) = heredoc(rest).map_err(parse)?;
                    no_more(&after).map_err(parse)?;
                    let (first, last) = anchor_span(&before).map_err(parse)?;
                    let body = take_body(&lines, &mut at, word, number)?;
                    Op::Replace { first, last, body }
                }
                "insert" => {
                    let (before, word, after) = heredoc(rest).map_err(parse)?;
                    no_more(&after).map_err(parse)?;
                    let place = parse_place(&before).map_err(parse)?;
                    let body = take_body(&lines, &mut at, word, number)?;
                    if body.is_empty() {
                        return Err(parse("an insert needs at least one line".into()));
                    }
                    match place {
                        Place::After(anchor) => Op::InsertAfter { anchor, body },
                        Place::Before(anchor) => Op::InsertBefore { anchor, body },
                        Place::Start => Op::InsertAtStart { body },
                        Place::End => Op::InsertAtEnd { body },
                    }
                }
                "delete" => {
                    if rest.iter().any(|word| word.starts_with("<<")) {
                        return Err(parse("delete takes no body".into()));
                    }
                    let (first, last) = anchor_span(rest).map_err(parse)?;
                    Op::Delete { first, last }
                }
                "substitute" => parse_substitute(&lines, &mut at, number, rest)?,
                other => {
                    return Err(parse(format!(
                        "unknown op `{other}` (want replace, insert, delete or substitute)"
                    )));
                }
            };
            ops.push(op);
        }
        if ops.is_empty() {
            return Err(ScriptError::Parse {
                line: lines.len().max(1),
                why: "the script holds no op".into(),
            });
        }
        Ok(Self { ops })
    }

    /// Read a unified diff of one file (`format=patch`).
    pub fn parse_patch(input: &str) -> Result<Self, ScriptError> {
        let lines = script_lines(input);
        let mut ops = Vec::new();
        let mut at = 0;
        let mut headers = 0usize;
        while let Some(line) = lines.get(at) {
            let number = at.saturating_add(1);
            at = number;
            if line.starts_with("--- ") {
                headers = headers.saturating_add(1);
                if headers > 1 || !ops.is_empty() {
                    return Err(ScriptError::Parse {
                        line: number,
                        why: "a patch for one file only".into(),
                    });
                }
                continue;
            }
            if line.starts_with('\\') {
                return Err(ScriptError::Parse {
                    line: number,
                    why: "a newline marker needs a preceding hunk line".into(),
                });
            }
            if !line.starts_with("@@") {
                // Headers before the first hunk, and blank lines after the
                // last, are not the patch.
                if ops.is_empty() || line.trim().is_empty() {
                    continue;
                }
                return Err(ScriptError::Parse {
                    line: number,
                    why: format!("`{line}` is not a hunk line"),
                });
            }
            let parse = |why: String| ScriptError::Parse { line: number, why };
            let (old_start, old_count, new_count) = hunk_header(line).map_err(parse)?;
            let mut hunk = PatchHunk {
                old_start,
                old: Vec::new(),
                new: Vec::new(),
                old_open: false,
                new_open: false,
            };
            let mut last_side = None;
            while hunk.old.len() < old_count || hunk.new.len() < new_count {
                let Some(body) = lines.get(at) else {
                    return Err(parse(format!(
                        "the hunk ends early: want {old_count} old and {new_count} new lines"
                    )));
                };
                at = at.saturating_add(1);
                let (tag, text) = match body.as_bytes().first() {
                    None => (b' ', ""),
                    Some(&tag) => (tag, body.get(1..).unwrap_or("")),
                };
                match tag {
                    b' ' => {
                        hunk.old.push(text.to_string());
                        hunk.new.push(text.to_string());
                        last_side = Some(b' ');
                    }
                    b'-' => {
                        hunk.old.push(text.to_string());
                        last_side = Some(b'-');
                    }
                    b'+' => {
                        hunk.new.push(text.to_string());
                        last_side = Some(b'+');
                    }
                    b'\\' => {
                        mark_no_newline(&mut hunk, old_count, new_count, last_side.take(), body)
                            .map_err(|why| ScriptError::Parse { line: at, why })?
                    }
                    _ => return Err(parse(format!("`{body}` is not a hunk line"))),
                }
            }
            if hunk.old.len() != old_count || hunk.new.len() != new_count {
                return Err(parse(format!(
                    "the hunk holds {} old and {} new lines, its header says {old_count} and {new_count}",
                    hunk.old.len(),
                    hunk.new.len()
                )));
            }
            if let Some(marker) = lines.get(at).filter(|next| next.starts_with('\\')) {
                mark_no_newline(&mut hunk, old_count, new_count, last_side.take(), marker)
                    .map_err(|why| ScriptError::Parse {
                        line: at.saturating_add(1),
                        why,
                    })?;
                at = at.saturating_add(1);
            }
            ops.push(Op::Hunk(hunk));
        }
        if ops.is_empty() {
            return Err(ScriptError::Parse {
                line: lines.len().max(1),
                why: "the patch holds no hunk".into(),
            });
        }
        Ok(Self { ops })
    }

    /// Plan the script against `text`.
    pub fn plan(&self, text: &str, options: &PlanOptions<'_>) -> Result<Plan, ScriptError> {
        let lines = Lines::of(text);
        let mut admitted = Admitted::default();
        let mut regions = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            let number = index.saturating_add(1);
            let resolved = resolve_op(number, op, &lines, options)?;
            admitted.admit(number, &resolved)?;
            regions.extend(resolved);
        }
        Ok(plan_of(regions))
    }
}

/// One change an op makes, in the pre-text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Region {
    op: usize,
    range: Range<usize>,
    insert: String,
    /// The line an insert hangs on (its bytes with its end-of-line), which
    /// no other op may change.
    holds: Option<Range<usize>>,
    /// No nonempty insertion may follow this open final line at EOF.
    opens_tail: bool,
}

fn resolve_op(
    op: usize,
    script_op: &Op,
    lines: &Lines<'_>,
    options: &PlanOptions<'_>,
) -> Result<Vec<Region>, ScriptError> {
    let eol = lines.eol();
    let text_len = lines.text().len();
    let region = |range: Range<usize>, insert: String, holds: Option<Range<usize>>| Region {
        op,
        range,
        insert,
        holds,
        opens_tail: false,
    };
    match script_op {
        Op::Replace { first, last, body } => {
            let (top, bottom) = locate(op, *first, *last, lines, options)?;
            let range = line_bytes(op, lines, top, bottom)?;
            let open = bottom == lines.count() && lines.open_ended();
            let insert = if open {
                body.join(eol)
            } else {
                terminated(body, eol)
            };
            Ok(vec![region(range, insert, None)])
        }
        Op::Delete { first, last } => {
            let (top, bottom) = locate(op, *first, *last, lines, options)?;
            let range = line_bytes(op, lines, top, bottom)?;
            Ok(vec![region(range, String::new(), None)])
        }
        Op::InsertAfter { anchor, body } => {
            let (line, _) = locate(op, *anchor, *anchor, lines, options)?;
            let span = lines.span(line).ok_or(ScriptError::Missing { op })?;
            let insert = if span.content_end == span.end {
                opened(body, eol)
            } else {
                terminated(body, eol)
            };
            Ok(vec![region(
                span.end..span.end,
                insert,
                Some(span.start..span.end),
            )])
        }
        Op::InsertBefore { anchor, body } => {
            let (line, _) = locate(op, *anchor, *anchor, lines, options)?;
            let span = lines.span(line).ok_or(ScriptError::Missing { op })?;
            Ok(vec![region(
                span.start..span.start,
                terminated(body, eol),
                Some(span.start..span.end),
            )])
        }
        Op::InsertAtStart { body } => {
            let start = lines.origin();
            Ok(vec![region(start..start, terminated(body, eol), None)])
        }
        Op::InsertAtEnd { body } => {
            let open_last = if lines.open_ended() {
                lines.span(lines.count())
            } else {
                None
            };
            let (insert, holds) = match open_last {
                Some(span) => (opened(body, eol), Some(span.start..span.end)),
                None => (terminated(body, eol), None),
            };
            Ok(vec![region(text_len..text_len, insert, holds)])
        }
        Op::Substitute { old, new, which } => {
            let replacement = new.join(eol);
            let found = find_old(lines, old);
            let picked: Vec<Range<usize>> = match *which {
                Which::Unique => match found.len() {
                    0 => return Err(ScriptError::Missing { op }),
                    1 => found,
                    matches => return Err(ScriptError::Ambiguous { op, matches }),
                },
                Which::All if found.is_empty() => return Err(ScriptError::Missing { op }),
                Which::All => found,
                Which::Nth(nth) => {
                    let index = nth.checked_sub(1).ok_or(ScriptError::Missing { op })?;
                    vec![
                        found
                            .get(index)
                            .cloned()
                            .ok_or(ScriptError::Missing { op })?,
                    ]
                }
            };
            Ok(picked
                .into_iter()
                .map(|range| region(range, replacement.clone(), None))
                .collect())
        }
        Op::Hunk(hunk) => resolve_hunk(op, hunk, lines, options),
    }
}

fn resolve_hunk(
    op: usize,
    hunk: &PatchHunk,
    lines: &Lines<'_>,
    options: &PlanOptions<'_>,
) -> Result<Vec<Region>, ScriptError> {
    let eol = lines.eol();
    let eof = |end: usize| {
        if ((hunk.old_open || hunk.new_open) && end != lines.text().len())
            || (hunk.old_open && (hunk.old.is_empty() || !lines.open_ended()))
            || (hunk.new_open && hunk.new.is_empty())
        {
            Err(ScriptError::Missing { op })
        } else {
            Ok(())
        }
    };
    let Some(old_last) = hunk.old.len().checked_sub(1) else {
        let after = map_base(op, hunk.old_start, hunk.old_start, None, options)?.0;
        let insert = if hunk.new_open {
            hunk.new.join(eol)
        } else {
            terminated(&hunk.new, eol)
        };
        if after == 0 {
            let start = lines.origin();
            eof(start)?;
            return Ok(vec![Region {
                op,
                range: start..start,
                insert,
                holds: None,
                opens_tail: hunk.new_open,
            }]);
        }
        let span = lines.span(after).ok_or(ScriptError::Missing { op })?;
        eof(span.end)?;
        let insert = if span.content_end == span.end {
            opened(&hunk.new, eol)
        } else {
            insert
        };
        return Ok(vec![Region {
            op,
            range: span.end..span.end,
            insert,
            holds: Some(span.start..span.end),
            opens_tail: hunk.new_open,
        }]);
    };
    let first = hunk.old_start.max(1);
    let last = first.saturating_add(old_last);
    let (first, _) = map_base(op, first, last, Some(&hunk.old), options)?;
    let matches_at =
        |top: usize| {
            hunk.old.iter().enumerate().all(|(index, want)| {
                lines.content(top.saturating_add(index)) == Some(want.as_str())
            }) && lines
                .span(top.saturating_add(old_last))
                .is_some_and(|span| eof(span.end).is_ok())
        };
    let top = if matches_at(first) {
        first
    } else {
        let mut found = None;
        for distance in near_shifts(options, first, lines.count()) {
            let up = first
                .checked_sub(distance)
                .filter(|line| *line >= 1 && matches_at(*line));
            let down = Some(first.saturating_add(distance)).filter(|line| matches_at(*line));
            match (up, down) {
                (Some(_), Some(_)) => return Err(ScriptError::Ambiguous { op, matches: 2 }),
                (Some(line), None) | (None, Some(line)) => {
                    found = Some(line);
                    break;
                }
                (None, None) => {}
            }
        }
        match found {
            Some(line) => line,
            None => return Err(hunk_conflict(op, hunk, lines, first)),
        }
    };
    let bottom = top.saturating_add(old_last);
    let range = line_bytes(op, lines, top, bottom)?;
    eof(range.end)?;
    let insert = if hunk.new_open {
        hunk.new.join(eol)
    } else {
        terminated(&hunk.new, eol)
    };
    Ok(vec![Region {
        op,
        range,
        insert,
        holds: None,
        opens_tail: hunk.new_open,
    }])
}

fn hunk_conflict(op: usize, hunk: &PatchHunk, lines: &Lines<'_>, first: usize) -> ScriptError {
    for (index, want) in hunk.old.iter().enumerate() {
        let line = first.saturating_add(index);
        if lines.content(line) != Some(want.as_str()) {
            return ScriptError::Conflict {
                op,
                line,
                want: Hash6::of(want),
                have: lines.h6(line),
            };
        }
    }
    ScriptError::Missing { op }
}

fn near_shifts(options: &PlanOptions<'_>, from: usize, count: usize) -> RangeInclusive<usize> {
    let nearest = from.saturating_sub(count).max(1);
    let furthest = options.near.unwrap_or(0).min(from.max(count));
    nearest..=furthest
}

fn map_base(
    op: usize,
    first: usize,
    last: usize,
    old: Option<&Vec<String>>,
    options: &PlanOptions<'_>,
) -> Result<(usize, usize), ScriptError> {
    let Some(map) = options.base_map else {
        return Ok((first, last));
    };
    if !map.reaches_base() {
        return Err(ScriptError::Stale { base: map.base() });
    }
    map.map_lines(first, last)
        .ok_or_else(|| ScriptError::Conflict {
            op,
            line: first,
            want: old
                .and_then(|old| old.first())
                .map_or(Hash6::of(""), |line| Hash6::of(line)),
            have: None,
        })
}

fn locate(
    op: usize,
    first: Anchor,
    last: Anchor,
    lines: &Lines<'_>,
    options: &PlanOptions<'_>,
) -> Result<(usize, usize), ScriptError> {
    let (top, bottom) = match options.base_map {
        None => (first.line, last.line),
        Some(map) => {
            if !map.reaches_base() {
                return Err(ScriptError::Stale { base: map.base() });
            }
            map.map_lines(first.line, last.line)
                .ok_or(ScriptError::Conflict {
                    op,
                    line: first.line,
                    want: first.h6,
                    have: None,
                })?
        }
    };
    let holds = |top: usize, bottom: usize| {
        lines.h6(top) == Some(first.h6) && lines.h6(bottom) == Some(last.h6)
    };
    if holds(top, bottom) {
        return Ok((top, bottom));
    }
    for distance in near_shifts(options, top, lines.count()) {
        let up = top
            .checked_sub(distance)
            .zip(bottom.checked_sub(distance))
            .filter(|(up_top, up_bottom)| *up_top >= 1 && holds(*up_top, *up_bottom));
        let down = Some((
            top.saturating_add(distance),
            bottom.saturating_add(distance),
        ))
        .filter(|(down_top, down_bottom)| holds(*down_top, *down_bottom));
        match (up, down) {
            (Some(_), Some(_)) => return Err(ScriptError::Ambiguous { op, matches: 2 }),
            (Some(found), None) | (None, Some(found)) => return Ok(found),
            (None, None) => {}
        }
    }
    let (line, want) = if lines.h6(top) == Some(first.h6) {
        (bottom, last.h6)
    } else {
        (top, first.h6)
    };
    Err(ScriptError::Conflict {
        op,
        line,
        want,
        have: lines.h6(line),
    })
}

fn line_bytes(
    op: usize,
    lines: &Lines<'_>,
    top: usize,
    bottom: usize,
) -> Result<Range<usize>, ScriptError> {
    let start = lines.span(top).ok_or(ScriptError::Missing { op })?.start;
    let end = lines.span(bottom).ok_or(ScriptError::Missing { op })?.end;
    Ok(start..end.max(start))
}

fn terminated(body: &[String], eol: &str) -> String {
    let mut out = String::new();
    for line in body {
        out.push_str(line);
        out.push_str(eol);
    }
    out
}

fn opened(body: &[String], eol: &str) -> String {
    let mut out = eol.to_string();
    out.push_str(&body.join(eol));
    out
}

fn find_old(lines: &Lines<'_>, old: &[String]) -> Vec<Range<usize>> {
    let text = lines.text();
    let Some((head, rest)) = old.split_first() else {
        return Vec::new();
    };
    let Some((tail, middle)) = rest.split_last() else {
        let origin = lines.origin();
        return text
            .get(origin..)
            .unwrap_or("")
            .match_indices(head.as_str())
            .map(|(offset, found)| {
                let start = origin.saturating_add(offset);
                start..start.saturating_add(found.len())
            })
            .collect();
    };
    let ended = |line: usize| lines.span(line).filter(|span| span.end > span.content_end);
    let mut found = Vec::new();
    let mut free = lines.origin();
    'lines: for line in 1..=lines.count() {
        let Some(span) = ended(line) else {
            continue;
        };
        let Some(start) = span
            .content_end
            .checked_sub(head.len())
            .filter(|start| *start >= span.start.max(free))
        else {
            continue;
        };
        if text.get(start..span.content_end) != Some(head.as_str()) {
            continue;
        }
        let mut end = span.end;
        let mut next = line.saturating_add(1);
        for want in middle {
            match ended(next) {
                Some(span) if lines.content(next) == Some(want.as_str()) => end = span.end,
                _ => continue 'lines,
            }
            next = next.saturating_add(1);
        }
        if !tail.is_empty() {
            match lines.span(next) {
                Some(span)
                    if lines
                        .content(next)
                        .is_some_and(|content| content.starts_with(tail.as_str())) =>
                {
                    end = span.start.saturating_add(tail.len());
                }
                _ => continue,
            }
        }
        found.push(start..end);
        free = end;
    }
    found
}

/// The changes of the ops admitted so far, which never overlap one another,
/// so each check is one probe of an ordered map.
#[derive(Default)]
struct Admitted {
    /// Changes that remove or replace bytes: start -> end.
    changes: BTreeMap<usize, usize>,
    /// Where inserts go.
    points: BTreeSet<usize>,
    /// The lines inserts hang on: start -> end. Whole lines of one text, so
    /// two are equal or disjoint.
    held: BTreeMap<usize, usize>,
    /// A prior open final line. A later insertion here would follow it.
    open_tail: Option<usize>,
}

impl Admitted {
    fn admit(&mut self, op: usize, regions: &[Region]) -> Result<(), ScriptError> {
        if regions.iter().any(|region| self.meets(region)) {
            return Err(ScriptError::Overlap { op });
        }
        for region in regions {
            if region.opens_tail {
                self.open_tail = Some(region.range.end);
            }
            if region.range.is_empty() {
                self.points.insert(region.range.start);
            } else {
                self.changes.insert(region.range.start, region.range.end);
            }
            if let Some(held) = &region.holds {
                self.held.insert(held.start, held.end);
            }
        }
        Ok(())
    }

    fn meets(&self, region: &Region) -> bool {
        let Range { start, end } = region.range;
        if self.open_tail == Some(start) && !region.insert.is_empty() {
            return true;
        }
        let held_changed = region
            .holds
            .as_ref()
            .is_some_and(|held| reaches_past(&self.changes, held.end, held.start));
        if start == end {
            return reaches_past(&self.changes, start, start) || held_changed;
        }
        let inside = start.saturating_add(1);
        reaches_past(&self.changes, end, start)
            || reaches_past(&self.held, end, start)
            || self.points.range(inside..end.max(inside)).next().is_some()
            || held_changed
    }
}

fn reaches_past(ranges: &BTreeMap<usize, usize>, before: usize, after: usize) -> bool {
    ranges
        .range(..before)
        .next_back()
        .is_some_and(|(_, end)| *end > after)
}

fn plan_of(mut regions: Vec<Region>) -> Plan {
    regions.sort_by_key(|region| (region.range.start, region.range.end, region.op));
    let mut hunks = Vec::with_capacity(regions.len());
    let mut edits = Vec::with_capacity(regions.len());
    let (mut removed, mut inserted) = (0usize, 0usize);
    for region in regions {
        let start = region
            .range
            .start
            .saturating_add(inserted)
            .saturating_sub(removed);
        hunks.push(Hunk {
            op: region.op,
            post: start..start.saturating_add(region.insert.len()),
        });
        removed = removed.saturating_add(region.range.len());
        inserted = inserted.saturating_add(region.insert.len());
        edits.push(TextEdit {
            range: region.range,
            insert: region.insert,
        });
    }
    hunks.sort_by_key(|hunk| (hunk.op, hunk.post.start));
    Plan { edits, hunks }
}

fn script_lines(input: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = input
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    if input.is_empty() || input.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn words_of(line: &str) -> Vec<&str> {
    line.split_whitespace()
        .take_while(|word| !word.starts_with('#'))
        .collect()
}

fn heredoc<'w>(words: &[&'w str]) -> Result<(Vec<&'w str>, &'w str, Vec<&'w str>), String> {
    let at = words
        .iter()
        .position(|word| word.starts_with("<<"))
        .ok_or_else(|| "no body: end the line with <<WORD".to_string())?;
    let opener = words.get(at).copied().unwrap_or("");
    let glued = opener.get(2..).unwrap_or("");
    let (word, after) = if glued.is_empty() {
        let word = words
            .get(at.saturating_add(1))
            .copied()
            .ok_or_else(|| "`<<` names no delimiter word".to_string())?;
        (word, at.saturating_add(2))
    } else {
        (glued, at.saturating_add(1))
    };
    let word = unquote(word);
    if word.is_empty() {
        return Err("`<<` names no delimiter word".into());
    }
    let before = words.get(..at).unwrap_or(&[]).to_vec();
    let after = words.get(after..).unwrap_or(&[]).to_vec();
    Ok((before, word, after))
}

fn unquote(word: &str) -> &str {
    for quote in ['\'', '"'] {
        if let Some(inner) = word
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    word
}

fn take_body(
    lines: &[&str],
    at: &mut usize,
    word: &str,
    opened: usize,
) -> Result<Vec<String>, ScriptError> {
    let mut body = Vec::new();
    while let Some(line) = lines.get(*at) {
        *at = at.saturating_add(1);
        if *line == word {
            return Ok(body);
        }
        body.push((*line).to_string());
    }
    Err(ScriptError::Parse {
        line: opened,
        why: format!("the body opened here never ends: no line `{word}`"),
    })
}

fn no_more(words: &[&str]) -> Result<(), String> {
    match words.first() {
        None => Ok(()),
        Some(word) => Err(format!("unexpected `{word}` after the body's delimiter")),
    }
}

fn anchor_span(words: &[&str]) -> Result<(Anchor, Anchor), String> {
    let anchors = words
        .iter()
        .map(|word| word.parse::<Anchor>().map_err(|why| why.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    match anchors.as_slice() {
        [one] => Ok((*one, *one)),
        [first, last] if first.line <= last.line => Ok((*first, *last)),
        [_, _] => Err("the span ends before it starts".into()),
        _ => Err(format!("want one or two anchors, got {}", anchors.len())),
    }
}

enum Place {
    After(Anchor),
    Before(Anchor),
    Start,
    End,
}

fn parse_place(words: &[&str]) -> Result<Place, String> {
    let anchor = |word: &str| word.parse::<Anchor>().map_err(|why| why.to_string());
    match words {
        ["after", word] => Ok(Place::After(anchor(word)?)),
        ["before", word] => Ok(Place::Before(anchor(word)?)),
        ["at-start"] => Ok(Place::Start),
        ["at-end"] => Ok(Place::End),
        _ => Err("want insert after <anchor>, before <anchor>, at-start or at-end".into()),
    }
}

fn parse_substitute(
    lines: &[&str],
    at: &mut usize,
    number: usize,
    rest: &[&str],
) -> Result<Op, ScriptError> {
    let parse = |line: usize| move |why: String| ScriptError::Parse { line, why };
    let mut which = None;
    let (before, word, after) = heredoc(rest).map_err(parse(number))?;
    for word in before.iter().chain(after.iter()) {
        set_which(&mut which, word).map_err(parse(number))?;
    }
    let old = take_body(lines, at, word, number)?;
    if old.concat().is_empty() && old.len() <= 1 {
        return Err(parse(number)("substitute needs old text to find".into()));
    }
    let with_line = loop {
        let Some(line) = lines.get(*at) else {
            return Err(parse(number)("substitute has no `with <<WORD`".into()));
        };
        *at = at.saturating_add(1);
        if !words_of(line).is_empty() {
            break *at;
        }
    };
    let words = words_of(
        lines
            .get(with_line.saturating_sub(1))
            .copied()
            .unwrap_or(""),
    );
    let Some((&"with", rest)) = words.split_first() else {
        return Err(parse(with_line)(
            "want `with <<WORD` after substitute's old text".into(),
        ));
    };
    let (before, word, after) = heredoc(rest).map_err(parse(with_line))?;
    for word in before.iter().chain(after.iter()) {
        set_which(&mut which, word).map_err(parse(with_line))?;
    }
    let new = take_body(lines, at, word, with_line)?;
    if let Some(line) = lines.get(*at) {
        let words = words_of(line);
        if let [word] = words.as_slice()
            && (*word == "all" || word.starts_with("nth="))
        {
            let number = at.saturating_add(1);
            set_which(&mut which, word).map_err(parse(number))?;
            *at = number;
        }
    }
    Ok(Op::Substitute {
        old,
        new,
        which: which.unwrap_or(Which::Unique),
    })
}

fn set_which(which: &mut Option<Which>, word: &str) -> Result<(), String> {
    let picked = if word == "all" {
        Which::All
    } else if let Some(nth) = word.strip_prefix("nth=") {
        Which::Nth(
            super::anchor::parse_line_number(nth)
                .ok_or_else(|| format!("bad {word} (want nth=<k>, from 1)"))?,
        )
    } else {
        return Err(format!(
            "unexpected `{word}` (substitute takes all or nth=<k>)"
        ));
    };
    if which.replace(picked).is_some() {
        return Err("substitute takes one of all and nth=<k>".into());
    }
    Ok(())
}

fn hunk_header(line: &str) -> Result<(usize, usize, usize), String> {
    let bad = || format!("`{line}` is not a hunk header (want @@ -a,b +c,d @@)");
    let mut words = line.split_whitespace();
    if words.next() != Some("@@") {
        return Err(bad());
    }
    let old = words
        .next()
        .and_then(|word| word.strip_prefix('-'))
        .ok_or_else(bad)?;
    let new = words
        .next()
        .and_then(|word| word.strip_prefix('+'))
        .ok_or_else(bad)?;
    if words.next() != Some("@@") {
        return Err(bad());
    }
    let side = |field: &str| -> Option<(usize, usize)> {
        let (start, count) = field.split_once(',').unwrap_or((field, "1"));
        let digits = |text: &str| {
            (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| text.parse::<usize>().ok())
                .flatten()
        };
        Some((digits(start)?, digits(count)?))
    };
    let (old_start, old_count) = side(old).ok_or_else(bad)?;
    let (_, new_count) = side(new).ok_or_else(bad)?;
    Ok((old_start, old_count, new_count))
}

fn mark_no_newline(
    hunk: &mut PatchHunk,
    old_count: usize,
    new_count: usize,
    side: Option<u8>,
    marker: &str,
) -> Result<(), String> {
    if marker != "\\ No newline at end of file" {
        return Err("want the exact no-newline marker after a hunk line".into());
    }
    match side {
        Some(b'-') if hunk.old.len() == old_count => hunk.old_open = true,
        Some(b'+') if hunk.new.len() == new_count => hunk.new_open = true,
        Some(b' ') if hunk.old.len() == old_count && hunk.new.len() == new_count => {
            hunk.old_open = true;
            hunk.new_open = true;
        }
        _ => return Err("a newline marker must follow the last line on its side".into()),
    }
    Ok(())
}

#[cfg(test)]
#[path = "script_tests.rs"]
mod tests;
