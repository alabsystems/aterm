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

    /// Record the next changes, in the order they were applied ([`line_changes`] gives a transaction's).
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

/// Where line `line` lands after `change`, when the change left it alone.
const fn shift_line(line: usize, change: &LineChange) -> usize {
    if line < change.at {
        line
    } else {
        line.saturating_sub(change.removed)
            .saturating_add(change.inserted)
    }
}

/// Line changes produced by a transaction of non-overlapping `edits` on `pre_text` (last line first).
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

/// The whole lines one or more edits touch.
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

    /// Read an edit script (the module docs give the grammar).
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

/// Resolve op number `op` (from 1) against the pre-text: its regions, or why
/// it does not apply.
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

/// A patch hunk: its old lines must be the text's, at the header's line
/// (mapped through `base=`, or moved by up to `near=`).
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

/// The conflict a hunk reports: its first old line that is not the text's.
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

/// The shifts a `near=` search tries from line `from` of a text of `count`
/// lines, nearest first: up to `near=`, and only those that can still land
/// in the text.
fn near_shifts(options: &PlanOptions<'_>, from: usize, count: usize) -> RangeInclusive<usize> {
    let nearest = from.saturating_sub(count).max(1);
    let furthest = options.near.unwrap_or(0).min(from.max(count));
    nearest..=furthest
}

/// Map lines `first..=last` through `base=`, when given.
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

/// Find the lines an op's anchors name: their lines (mapped through `base=`)
/// when both h6s match there, else the one shift within `near=` where both
/// do.
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

/// The bytes of lines `top..=bottom` with their end-of-lines.
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

/// Body lines, each with an end-of-line.
fn terminated(body: &[String], eol: &str) -> String {
    let mut out = String::new();
    for line in body {
        out.push_str(line);
        out.push_str(eol);
    }
    out
}

/// Body lines after a last line without an end-of-line: the end-of-line
/// first, which that line gets, and none after the last body line, so the
/// text still ends without one.
fn opened(body: &[String], eol: &str) -> String {
    let mut out = eol.to_string();
    out.push_str(&body.join(eol));
    out
}

/// Where a substitute's old lines occur in the text, leftmost first and
/// never overlapping.
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
    /// The lines inserts hang on: start -> end.
    held: BTreeMap<usize, usize>,
    /// A prior open final line. A later insertion here would follow it.
    open_tail: Option<usize>,
}

impl Admitted {
    /// Admit op `op`'s regions, or refuse it as an overlap when one of them
    /// meets an earlier op's: a shared byte, an insert strictly inside a
    /// change, or a change to a line an insert hangs on.
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

    /// Whether `region` meets a change admitted before it.
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
            // An insert: strictly inside a change, or on a changed line.
            return reaches_past(&self.changes, start, start) || held_changed;
        }
        let inside = start.saturating_add(1);
        reaches_past(&self.changes, end, start)
            || reaches_past(&self.held, end, start)
            || self.points.range(inside..end.max(inside)).next().is_some()
            || held_changed
    }
}

/// Whether a range of `ranges` (disjoint, start -> end) that starts before
/// `before` ends after `after`.
fn reaches_past(ranges: &BTreeMap<usize, usize>, before: usize, after: usize) -> bool {
    ranges
        .range(..before)
        .next_back()
        .is_some_and(|(_, end)| *end > after)
}

/// The plan of resolved, non-overlapping regions: the edits sorted by
/// position, equal positions in script order.
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

/// The script's lines, numbered from 1 by index + 1: split at `\n`, one `\r`
/// before it dropped, no empty line after a final `\n`.
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

/// A line's words up to a `#` comment.
fn words_of(line: &str) -> Vec<&str> {
    line.split_whitespace()
        .take_while(|word| !word.starts_with('#'))
        .collect()
}

/// Split `words` at the body opener `<<WORD` (or `<<` then `WORD`): the
/// words before, the delimiter, the words after.
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

/// A delimiter written `'END'` or `"END"`, as a shell heredoc quotes it.
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

/// The body after the opener on line `opened`: the lines up to the one that
/// is exactly `word`, which ends it.
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

/// One anchor or two: the lines an op spans, the first not after the last.
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

/// Where an insert goes.
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

/// `substitute <<W1`, its body, `with <<W2`, its body, and an optional
/// `all` or `nth=<k>` (on either opener line or on the line after).
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

/// `@@ -a[,b] +c[,d] @@ ...`: the old side's start and both sides' counts.
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

/// An exact marker belongs to the immediately preceding hunk line, which
/// must already be the last line on every side that marker names.
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
mod tests {
    use super::*;
    use crate::store::{DocumentStore, DocumentTxnOutcome};

    /// The anchor of `line` in `text`, as a read prints it.
    fn at(text: &str, line: usize) -> String {
        Anchor::of_line(text, line).unwrap().to_string()
    }

    fn apply(text: &str, script: &str) -> Result<String, ScriptError> {
        apply_with(text, script, &PlanOptions::default())
    }

    fn apply_with(
        text: &str,
        script: &str,
        options: &PlanOptions<'_>,
    ) -> Result<String, ScriptError> {
        let plan = Script::parse(script)?.plan(text, options)?;
        Ok(plan.apply(text))
    }

    const TEXT: &str = "one\ntwo\nthree\nfour\nfive\n";

    #[test]
    fn each_op_applies_exactly() {
        let t = TEXT;
        let cases = [
            (
                format!("replace {} {} <<END\n2a\n2b\nEND\n", at(t, 2), at(t, 3)),
                "one\n2a\n2b\nfour\nfive\n",
            ),
            (
                format!("replace {} <<END\nTWO\nEND\n", at(t, 2)),
                "one\nTWO\nthree\nfour\nfive\n",
            ),
            (
                format!("replace {} <<END\nEND\n", at(t, 2)),
                "one\nthree\nfour\nfive\n",
            ),
            (
                format!("insert after {} <<X\nnew\nX\n", at(t, 5)),
                "one\ntwo\nthree\nfour\nfive\nnew\n",
            ),
            (
                format!("insert before {} <<X\nnew\nX\n", at(t, 1)),
                "new\none\ntwo\nthree\nfour\nfive\n",
            ),
            (
                "insert at-start <<X\nhead\nX\n".to_string(),
                "head\none\ntwo\nthree\nfour\nfive\n",
            ),
            (
                "insert at-end <<X\ntail\nX\n".to_string(),
                "one\ntwo\nthree\nfour\nfive\ntail\n",
            ),
            (
                format!("delete {} {}\n", at(t, 4), at(t, 5)),
                "one\ntwo\nthree\n",
            ),
            (format!("delete {}\n", at(t, 1)), "two\nthree\nfour\nfive\n"),
            (
                "substitute <<OLD\nthree\nOLD\nwith <<NEW\n3\nNEW\n".to_string(),
                "one\ntwo\n3\nfour\nfive\n",
            ),
            (
                "substitute <<OLD\nwo\nthr\nOLD\nwith <<NEW\nW\nNEW\n".to_string(),
                "one\ntWee\nfour\nfive\n",
            ),
        ];
        for (script, expected) in cases {
            assert_eq!(apply(t, &script).as_deref(), Ok(expected), "{script}");
        }

        let script = format!(
            "# rename and grow\ninsert at-start <<A\nzero\nA\n\nreplace {} <<B # one line\nTWO\nB\ndelete {}\ninsert after {} <<C\nfive and a half\nC\n",
            at(t, 2),
            at(t, 4),
            at(t, 5)
        );
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("zero\none\nTWO\nthree\nfive\nfive and a half\n")
        );

        let open = "a\nb";
        assert_eq!(
            apply(open, &format!("insert after {} <<X\nc\nX\n", at(open, 2))).as_deref(),
            Ok("a\nb\nc")
        );
        assert_eq!(
            apply(open, "insert at-end <<X\nc\nX\n").as_deref(),
            Ok("a\nb\nc")
        );
        assert_eq!(
            apply(open, &format!("replace {} <<X\nB\nX\n", at(open, 2))).as_deref(),
            Ok("a\nB")
        );
        let crlf = "a\r\nb\r\n";
        assert_eq!(
            apply(crlf, &format!("replace {} <<X\nx\ny\nX\n", at(crlf, 1))).as_deref(),
            Ok("x\r\ny\r\nb\r\n")
        );
        let bom = "\u{feff}a\nb\n";
        assert_eq!(
            apply(bom, &format!("delete {}\n", at(bom, 1))).as_deref(),
            Ok("\u{feff}b\n")
        );
        assert_eq!(
            apply(bom, "insert at-start <<X\nz\nX\n").as_deref(),
            Ok("\u{feff}z\na\nb\n")
        );
        assert_eq!(apply("", "insert at-end <<X\nz\nX\n").as_deref(), Ok("z\n"));
    }

    #[test]
    fn delimiter_word_inside_a_body_line_is_text() {
        let t = TEXT;
        let script = format!(
            "replace {} <<END\nEND of story\n  END\nEND.\n#END\n\nENDEND\nEND\n",
            at(t, 1)
        );
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("END of story\n  END\nEND.\n#END\n\nENDEND\ntwo\nthree\nfour\nfive\n")
        );
        let script = format!(
            "insert after {} << 'EOF'\n# not a comment\na <<b\nEOF\n",
            at(t, 5)
        );
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("one\ntwo\nthree\nfour\nfive\n# not a comment\na <<b\n")
        );
        let script = format!("replace {} <<END\r\nTWO\r\nEND\r\n", at(t, 2));
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("one\nTWO\nthree\nfour\nfive\n")
        );
        let script = format!(
            "delete {}\nreplace {} <<END\nnever closed\n",
            at(t, 1),
            at(t, 2)
        );
        assert!(matches!(
            Script::parse(&script),
            Err(ScriptError::Parse { line: 2, .. })
        ));
    }

    #[test]
    fn substitute_unique_all_and_nth() {
        let text = "let a = 1;\nlet b = a + a;\n";
        let script = |which: &str| format!("substitute <<OLD\na\nOLD\nwith <<NEW\nx\nNEW\n{which}");
        assert_eq!(
            apply(text, &script("")),
            Err(ScriptError::Ambiguous { op: 1, matches: 3 })
        );
        assert_eq!(
            apply(text, &script("all\n")).as_deref(),
            Ok("let x = 1;\nlet b = x + x;\n")
        );
        assert_eq!(
            apply(text, &script("nth=2\n")).as_deref(),
            Ok("let a = 1;\nlet b = x + a;\n")
        );
        assert_eq!(
            apply(text, &script("nth=4\n")),
            Err(ScriptError::Missing { op: 1 })
        );
        let unique = "substitute <<OLD\nb =\nOLD\nwith <<NEW\nc =\nNEW\n";
        assert_eq!(
            apply(text, unique).as_deref(),
            Ok("let a = 1;\nlet c = a + a;\n")
        );
        let none = "substitute <<OLD\nzzz\nOLD\nwith <<NEW\nc\nNEW\nall\n";
        assert_eq!(apply(text, none), Err(ScriptError::Missing { op: 1 }));
        assert_eq!(
            apply(text, "substitute <<OLD\na\nOLD\nwith <<NEW all\nx\nNEW\n").as_deref(),
            Ok("let x = 1;\nlet b = x + x;\n")
        );
        assert!(matches!(
            Script::parse("substitute all <<OLD\na\nOLD\nwith <<NEW\nx\nNEW\nnth=2\n"),
            Err(ScriptError::Parse { line: 7, .. })
        ));
        assert!(matches!(
            Script::parse("substitute <<OLD\nOLD\nwith <<NEW\nx\nNEW\n"),
            Err(ScriptError::Parse { line: 1, .. })
        ));
        assert_eq!(
            apply("a.b\naxb\n", "substitute <<O\n.\nO\nwith <<N\n!\nN\n").as_deref(),
            Ok("a!b\naxb\n")
        );
    }

    #[test]
    fn multi_line_substitute_matches_any_end_of_line() {
        let join = "substitute <<O\nb\nc\nO\nwith <<N\nB\nN\n";
        for (text, expected) in [
            ("a\nb\nc\n", "a\nB\n"),
            ("a\r\nb\r\nc\r\n", "a\r\nB\r\n"),
            ("a\nb\r\nc\r\n", "a\nB\r\n"),
            ("a\r\nb\nc\n", "a\r\nB\n"),
        ] {
            assert_eq!(apply(text, join).as_deref(), Ok(expected), "{text:?}");
        }
        let mixed = "x = 1\r\nif x {\nrun()\r\n} else\n";
        assert_eq!(
            apply(
                mixed,
                "substitute <<O\nx {\nrun()\n}\nO\nwith <<N\nx {\n  run()\n}\nN\n"
            )
            .as_deref(),
            Ok("x = 1\r\nif x {\r\n  run()\r\n} else\n")
        );
        assert_eq!(
            apply(mixed, "substitute <<O\nif\nrun()\n}\nO\nwith <<N\nz\nN\n"),
            Err(ScriptError::Missing { op: 1 })
        );
        assert_eq!(
            apply("a\nb\r\nc\n", "substitute <<O\n\nc\nO\nwith <<N\n!\nN\n").as_deref(),
            Ok("a\nb!\n")
        );
        assert_eq!(
            apply("a\r\nb\n", "substitute <<O\nb\n\nO\nwith <<N\nB\nN\n").as_deref(),
            Ok("a\r\nB")
        );
        assert_eq!(
            apply(
                "a\na\r\na\na\n",
                "substitute <<O\na\na\nO\nwith <<N\nb\nN\nall\n"
            )
            .as_deref(),
            Ok("b\r\nb\n")
        );
        assert_eq!(
            apply("a\nb", "substitute <<O\nb\n\nO\nwith <<N\nB\nN\n"),
            Err(ScriptError::Missing { op: 1 })
        );
    }

    #[test]
    fn overlapping_ops_refused() {
        let t = TEXT;
        let refused = [
            format!(
                "replace {} {} <<A\nx\nA\nreplace {} {} <<B\ny\nB\n",
                at(t, 2),
                at(t, 3),
                at(t, 3),
                at(t, 4)
            ),
            format!(
                "insert after {} <<A\nx\nA\ndelete {} {}\n",
                at(t, 3),
                at(t, 2),
                at(t, 4)
            ),
            format!(
                "replace {} <<A\nx\nA\nsubstitute <<O\nhre\nO\nwith <<N\nHRE\nN\n",
                at(t, 3)
            ),
            format!(
                "insert before {} <<A\nx\nA\nsubstitute <<O\nhre\nO\nwith <<N\nHRE\nN\n",
                at(t, 3)
            ),
            format!(
                "replace {} {} <<A\nx\nA\ninsert after {} <<B\ny\nB\n",
                at(t, 1),
                at(t, 2),
                at(t, 2)
            ),
        ];
        for script in refused {
            assert_eq!(
                apply(t, &script),
                Err(ScriptError::Overlap { op: 2 }),
                "{script}"
            );
        }
        let script = format!(
            "insert after {} <<A\nA1\nA\nreplace {} <<B\nTWO\nB\nreplace {} <<C\nTHREE\nC\ninsert before {} <<D\nB4\nD\ninsert after {} <<E\nA4\nE\ninsert before {} <<F\nB5\nF\n",
            at(t, 1),
            at(t, 2),
            at(t, 3),
            at(t, 4),
            at(t, 4),
            at(t, 5)
        );
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("one\nA1\nTWO\nTHREE\nB4\nfour\nA4\nB5\nfive\n")
        );

        for open in ["a\nb\nc", "a\r\nb\r\nc"] {
            let at_end = "insert at-end <<X\nX1\nX\n";
            for (changes_c, also) in [
                (format!("delete {}\n", at(open, 3)), true),
                (format!("replace {} <<R\nR\n", at(open, 3)), true),
                ("substitute <<O\nc\nO\nwith <<N\n\nN\n".to_string(), true),
                (
                    "substitute <<O\nb\nc\nO\nwith <<N\nb\nN\n".to_string(),
                    true,
                ),
                (format!("replace {} <<R\nC\nR\n", at(open, 3)), true),
                (format!("insert after {} <<Y\nnew\nY\n", at(open, 3)), false),
            ] {
                for script in [
                    format!("{changes_c}{at_end}"),
                    format!("{at_end}{changes_c}"),
                ] {
                    let planned = apply(open, &script);
                    if also {
                        assert_eq!(planned, Err(ScriptError::Overlap { op: 2 }), "{script}");
                    } else {
                        assert!(planned.is_ok(), "{script}: {planned:?}");
                    }
                }
            }
        }
        let closed = "a\nb\nc\n";
        assert_eq!(
            apply(
                closed,
                &format!("delete {}\ninsert at-end <<X\nX1\nX\n", at(closed, 3))
            )
            .as_deref(),
            Ok("a\nb\nX1\n")
        );
    }

    #[test]
    fn first_failing_op_in_script_order_names_the_error() {
        let t = TEXT;
        let stale = format!("4:{}", Hash6::of("not four"));
        let script = format!(
            "replace {} <<X\nA\nX\nreplace {} <<X\nB\nX\ndelete {stale}\n",
            at(t, 3),
            at(t, 3)
        );
        assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 2 }));
        let script = format!(
            "replace {} <<X\nA\nX\ndelete {stale}\nreplace {} <<X\nB\nX\n",
            at(t, 3),
            at(t, 3)
        );
        assert!(matches!(
            apply(t, &script),
            Err(ScriptError::Conflict { op: 2, line: 4, .. })
        ));
        let script = format!(
            "replace {} {} <<X\nA\nX\nreplace {} <<X\nB\nX\nreplace {} <<X\nC\nX\n",
            at(t, 3),
            at(t, 5),
            at(t, 1),
            at(t, 4)
        );
        assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 3 }));
        let script = format!(
            "insert after {} <<X\nA\nX\nsubstitute <<O\ne\nO\nwith <<N\nE\nN\nall\n",
            at(t, 2)
        );
        assert_eq!(
            apply(t, &script).as_deref(),
            Ok("onE\ntwo\nA\nthrEE\nfour\nfivE\n")
        );
        let script = format!(
            "insert after {} <<X\nA\nX\nsubstitute <<O\no\nO\nwith <<N\nO\nN\nall\n",
            at(t, 2)
        );
        assert_eq!(apply(t, &script), Err(ScriptError::Overlap { op: 2 }));
    }

    #[test]
    fn failing_third_op_leaves_text_untouched() {
        let t = TEXT;
        let mut store = DocumentStore::for_test();
        let document = store.open("mem://script".into(), t.into());
        let before = store.snapshot(document).unwrap();
        let stale_anchor = Anchor {
            line: 4,
            h6: Hash6::of("not four"),
        };
        let script = format!(
            "replace {} <<A\nONE\nA\ninsert at-end <<B\nsix\nB\ndelete {stale_anchor}\n",
            at(t, 1)
        );
        let script = Script::parse(&script).unwrap();
        let error = script
            .plan(&before.text, &PlanOptions::default())
            .unwrap_err();
        assert_eq!(
            error,
            ScriptError::Conflict {
                op: 3,
                line: 4,
                want: Hash6::of("not four"),
                have: Some(Hash6::of("four")),
            }
        );
        assert_eq!(
            error.to_string(),
            format!(
                "conflict op=3 line=4 want={} have={}",
                Hash6::of("not four"),
                Hash6::of("four")
            )
        );
        let after = store.snapshot(document).unwrap();
        assert_eq!(after.seq, before.seq);
        assert_eq!(after.text.as_bytes(), t.as_bytes());

        let script = format!(
            "replace {} <<A\nONE\nA\ninsert at-end <<B\nsix\nB\ndelete {}\n",
            at(t, 1),
            at(t, 4)
        );
        let plan = Script::parse(&script)
            .unwrap()
            .plan(&before.text, &PlanOptions::default())
            .unwrap();
        let outcome = store.transact(document, before.seq, plan.edits.clone());
        assert!(
            matches!(outcome, DocumentTxnOutcome::Committed { .. }),
            "{outcome:?}"
        );
        let committed = store.snapshot(document).unwrap();
        assert_eq!(&*committed.text, plan.apply(t));
        assert_eq!(&*committed.text, "ONE\ntwo\nthree\nfive\nsix\n");
    }

    #[test]
    fn near_relocates_only_when_given() {
        let read = TEXT;
        let script = format!("replace {} <<X\nTHREE\nX\n", at(read, 3));
        let moved = "zero\nhalf\none\ntwo\nthree\nfour\nfive\n";
        assert_eq!(
            apply(moved, &script),
            Err(ScriptError::Conflict {
                op: 1,
                line: 3,
                want: Hash6::of("three"),
                have: Some(Hash6::of("one")),
            })
        );
        let near = |n| PlanOptions {
            near: Some(n),
            base_map: None,
        };
        assert_eq!(
            apply_with(moved, &script, &near(1)),
            Err(ScriptError::Conflict {
                op: 1,
                line: 3,
                want: Hash6::of("three"),
                have: Some(Hash6::of("one")),
            })
        );
        assert_eq!(
            apply_with(moved, &script, &near(2)).as_deref(),
            Ok("zero\nhalf\none\ntwo\nTHREE\nfour\nfive\n")
        );
        let span = format!("delete {} {}\n", at(read, 2), at(read, 3));
        assert_eq!(
            apply_with(moved, &span, &near(2)).as_deref(),
            Ok("zero\nhalf\none\nfour\nfive\n")
        );
        let twins = "x\nthree\ny\nthree\n";
        let script = format!("replace 3:{} <<X\nT\nX\n", Hash6::of("three"));
        assert_eq!(
            apply_with(twins, &script, &near(1)),
            Err(ScriptError::Ambiguous { op: 1, matches: 2 })
        );
        let script = format!("replace {} <<X\nT\nX\n", at(twins, 2));
        assert_eq!(
            apply_with(twins, &script, &near(3)).as_deref(),
            Ok("x\nT\ny\nthree\n")
        );

        let read: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        let shrunk: String = (7..=10).map(|n| format!("line {n}\n")).collect();
        let script = format!("replace {} <<X\nTEN\nX\n", at(&read, 10));
        assert_eq!(
            apply_with(&shrunk, &script, &near(6)).as_deref(),
            Ok("line 7\nline 8\nline 9\nTEN\n")
        );
        assert_eq!(
            apply_with(&shrunk, &script, &near(100)).as_deref(),
            Ok("line 7\nline 8\nline 9\nTEN\n")
        );
        assert_eq!(
            apply_with(&shrunk, &script, &near(5)),
            Err(ScriptError::Conflict {
                op: 1,
                line: 10,
                want: Hash6::of("line 10"),
                have: None,
            })
        );
        let hunk = Script::parse_patch("@@ -10 +10 @@\n-line 10\n+TEN\n").unwrap();
        assert_eq!(
            hunk.plan(&shrunk, &near(6)).map(|plan| plan.apply(&shrunk)),
            Ok("line 7\nline 8\nline 9\nTEN\n".to_string())
        );
        assert!(matches!(
            hunk.plan(&shrunk, &near(5)),
            Err(ScriptError::Conflict {
                op: 1,
                line: 10,
                ..
            })
        ));
        let far =
            |content: &str| format!("replace {}:{} <<X\nT\nX\n", usize::MAX, Hash6::of(content));
        assert_eq!(
            apply_with(&shrunk, &far("gone"), &near(usize::MAX)),
            Err(ScriptError::Conflict {
                op: 1,
                line: usize::MAX,
                want: Hash6::of("gone"),
                have: None,
            })
        );
        assert_eq!(
            apply_with(&shrunk, &far("line 8"), &near(usize::MAX)).as_deref(),
            Ok("line 7\nT\nline 9\nline 10\n")
        );
        let script = format!("replace {} <<X\nT\nX\n", at(&read, 9));
        assert_eq!(
            apply_with(&shrunk, &script, &near(usize::MAX)).as_deref(),
            Ok("line 7\nline 8\nT\nline 10\n")
        );
    }

    #[test]
    fn base_maps_anchors_through_what_changed_since_the_read() {
        let read = TEXT;
        let other = format!(
            "insert at-start <<A\nzero\nA\nreplace {} <<B\nFIVE\nB\n",
            at(read, 5)
        );
        let other = Script::parse(&other)
            .unwrap()
            .plan(read, &PlanOptions::default())
            .unwrap();
        let now = other.apply(read);
        let mut map = LineMap::new(7);
        map.record(other.line_changes(read));
        let options = PlanOptions {
            near: None,
            base_map: Some(&map),
        };
        let script = format!("replace {} <<X\nTHREE\nX\n", at(read, 3));
        assert_eq!(
            apply_with(&now, &script, &options).as_deref(),
            Ok("zero\none\ntwo\nTHREE\nfour\nFIVE\n")
        );
        let script = format!("delete {}\n", at(read, 5));
        assert_eq!(
            apply_with(&now, &script, &options),
            Err(ScriptError::Conflict {
                op: 1,
                line: 5,
                want: Hash6::of("five"),
                have: None,
            })
        );
        let mut inside = LineMap::new(7);
        inside.record([LineChange {
            at: 3,
            removed: 0,
            inserted: 1,
        }]);
        let span = format!("delete {} {}\n", at(read, 2), at(read, 3));
        let options = PlanOptions {
            near: None,
            base_map: Some(&inside),
        };
        assert!(matches!(
            apply_with("one\ntwo\nnew\nthree\nfour\nfive\n", &span, &options),
            Err(ScriptError::Conflict { op: 1, line: 2, .. })
        ));
        let gone = LineMap::unreachable(3);
        let options = PlanOptions {
            near: None,
            base_map: Some(&gone),
        };
        assert_eq!(
            apply_with(read, &script, &options),
            Err(ScriptError::Stale { base: 3 })
        );
        assert_eq!(ScriptError::Stale { base: 3 }.to_string(), "stale base=3");
    }

    #[test]
    fn line_changes_count_whole_lines() {
        let edit = |range: Range<usize>, insert: &str| TextEdit {
            range,
            insert: insert.into(),
        };
        let text = "a\nb\nc\n";
        assert_eq!(
            line_changes(text, &[edit(2..2, "x\ny\n")]),
            [LineChange {
                at: 2,
                removed: 0,
                inserted: 2
            }]
        );
        assert_eq!(
            line_changes(text, &[edit(2..3, "B")]),
            [LineChange {
                at: 2,
                removed: 1,
                inserted: 1
            }]
        );
        assert_eq!(
            line_changes(text, &[edit(1..2, "")]),
            [LineChange {
                at: 1,
                removed: 2,
                inserted: 1
            }]
        );
        assert_eq!(
            line_changes(
                text,
                &[
                    edit(6..6, "d\n"),
                    edit(0..1, "A"),
                    edit(4..5, "C"),
                    edit(5..5, "!")
                ]
            ),
            [
                LineChange {
                    at: 4,
                    removed: 0,
                    inserted: 1
                },
                LineChange {
                    at: 3,
                    removed: 1,
                    inserted: 1
                },
                LineChange {
                    at: 1,
                    removed: 1,
                    inserted: 1
                },
            ]
        );
    }

    #[test]
    fn reply_echoes_fresh_anchors_in_script_order() {
        let t = TEXT;
        let script = format!(
            "replace {} <<A\nFOUR\n4b\nA\ninsert at-start <<B\nzero\nB\ndelete {}\n",
            at(t, 4),
            at(t, 2)
        );
        let plan = Script::parse(&script)
            .unwrap()
            .plan(t, &PlanOptions::default())
            .unwrap();
        let post = plan.apply(t);
        assert_eq!(post, "zero\none\nthree\nFOUR\n4b\nfive\n");
        let reply = plan.reply(&post, 9);
        let expected = format!(
            "OK applied hunks=3 seq=9\nhunk 1 {} {}\nhunk 2 {} {}\nhunk 3 {} {}\n",
            at(&post, 4),
            at(&post, 5),
            at(&post, 1),
            at(&post, 1),
            at(&post, 2),
            at(&post, 3)
        );
        assert_eq!(reply, expected);
        let next = format!("delete {}\n", at(&post, 5));
        assert_eq!(
            apply(&post, &next).as_deref(),
            Ok("zero\none\nthree\nFOUR\nfive\n")
        );
        let open = "a\nb";
        let plan = Script::parse(&format!("insert after {} <<X\nc\nX\n", at(open, 2)))
            .unwrap()
            .plan(open, &PlanOptions::default())
            .unwrap();
        let post = plan.apply(open);
        assert_eq!(
            plan.reply(&post, 1),
            format!(
                "OK applied hunks=1 seq=1\nhunk 1 {} {}\n",
                at(&post, 3),
                at(&post, 3)
            )
        );
        let plan = Script::parse(&format!("delete {}\n", at("x\n", 1)))
            .unwrap()
            .plan("x\n", &PlanOptions::default())
            .unwrap();
        assert_eq!(plan.reply("", 2), "OK applied hunks=1 seq=2\nhunk 1 none\n");
    }

    #[test]
    fn patch_format_applies_and_refuses_bad_context() {
        let t = TEXT;
        let patch = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -2,3 +2,3 @@ fn\n two\n-three\n+THREE\n four\n@@ -5 +5,2 @@\n five\n+six\n\n";
        let script = Script::parse_as(Format::Patch, patch).unwrap();
        assert_eq!(script.ops().len(), 2);
        let plan = script.plan(t, &PlanOptions::default()).unwrap();
        assert_eq!(plan.apply(t), "one\ntwo\nTHREE\nfour\nfive\nsix\n");

        let bad = "@@ -2,3 +2,3 @@\n two\n-three\n+THREE\n FOUR\n";
        assert_eq!(
            Script::parse_patch(bad)
                .unwrap()
                .plan(t, &PlanOptions::default()),
            Err(ScriptError::Conflict {
                op: 1,
                line: 4,
                want: Hash6::of("FOUR"),
                have: Some(Hash6::of("four")),
            })
        );
        let moved = "@@ -1,2 +1,2 @@\n two\n-three\n+3\n";
        let script = Script::parse_patch(moved).unwrap();
        assert!(matches!(
            script.plan(t, &PlanOptions::default()),
            Err(ScriptError::Conflict { op: 1, line: 1, .. })
        ));
        let near = PlanOptions {
            near: Some(1),
            base_map: None,
        };
        assert_eq!(
            script.plan(t, &near).unwrap().apply(t),
            "one\ntwo\n3\nfour\nfive\n"
        );
        let open =
            "@@ -5 +5 @@\n-five\n+FIVE\n\\ No newline at end of file\n@@ -0,0 +1 @@\n+zero\n";
        let plan = Script::parse_patch(open)
            .unwrap()
            .plan(t, &PlanOptions::default())
            .unwrap();
        assert_eq!(plan.apply(t), "zero\none\ntwo\nthree\nfour\nFIVE");
        for patch in [
            "@@ -1 +1 @@\n-a\n+X\n\\ No newline at end of file\n",
            "@@ -1,0 +2 @@\n+X\n\\ No newline at end of file\n",
        ] {
            assert_eq!(
                Script::parse_patch(patch)
                    .unwrap()
                    .plan("a\nb\n", &PlanOptions::default()),
                Err(ScriptError::Missing { op: 1 })
            );
        }
        let direct = PatchHunk {
            old_start: 1,
            old: vec!["a".into()],
            new: vec!["X".into()],
            old_open: false,
            new_open: true,
        };
        assert_eq!(
            resolve_hunk(1, &direct, &Lines::of("a\nb\n"), &PlanOptions::default()),
            Err(ScriptError::Missing { op: 1 })
        );
        let old_open = "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+X\n";
        let script = Script::parse_patch(old_open).unwrap();
        assert_eq!(
            script
                .plan("a", &PlanOptions::default())
                .unwrap()
                .apply("a"),
            "X\n"
        );
        for text in ["a\n", "a\nb\n"] {
            assert_eq!(
                script.plan(text, &PlanOptions::default()),
                Err(ScriptError::Missing { op: 1 })
            );
        }
        let near = PlanOptions {
            near: Some(1),
            base_map: None,
        };
        assert_eq!(script.plan("a\na", &near).unwrap().apply("a\na"), "a\nX\n");
        for patch in [
            "@@ -1 +1 @@\n a\n\\ No newline at end of file\n",
            "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+a\n\\ No newline at end of file\n",
        ] {
            assert_eq!(
                Script::parse_patch(patch)
                    .unwrap()
                    .plan("a", &PlanOptions::default())
                    .unwrap()
                    .apply("a"),
                "a"
            );
        }
        let open_then_more =
            "@@ -1,0 +2 @@\n+last\n\\ No newline at end of file\n@@ -1,0 +3 @@\n+more\n";
        assert_eq!(
            Script::parse_patch(open_then_more)
                .unwrap()
                .plan("a\n", &PlanOptions::default()),
            Err(ScriptError::Overlap { op: 2 })
        );
        let more_then_open =
            "@@ -1,0 +2 @@\n+more\n@@ -1,0 +3 @@\n+last\n\\ No newline at end of file\n";
        assert_eq!(
            Script::parse_patch(more_then_open)
                .unwrap()
                .plan("a\n", &PlanOptions::default())
                .unwrap()
                .apply("a\n"),
            "a\nmore\nlast"
        );
        for bad in [
            "\\ No newline at end of file\n@@ -1 +1 @@\n-a\n+X\n",
            "@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+X\n",
            "@@ -1 +1 @@\n-a\n+X\n\\ not a newline marker\n",
            "@@ -1,2 +1,2 @@\n a\n\\ No newline at end of file\n-b\n+B\n",
            "@@ -1,2 +1 @@\n-a\n\\ No newline at end of file\n-b\n+X\n",
            "@@ -1 +1,2 @@\n-a\n+X\n\\ No newline at end of file\n+Y\n",
            "@@ -1 +1 @@\n-a\n+X\n\\ No newline at end of file\n\\ No newline at end of file\n",
        ] {
            assert!(
                matches!(Script::parse_patch(bad), Err(ScriptError::Parse { .. })),
                "{bad}"
            );
        }
        for (bad, line) in [
            ("@@ -1,2 +1,2 @@\n one\n", 1),
            ("@@ -1 +1 @@\n*one\n", 1),
            ("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-one\n+1\n--- a/g\n", 6),
            ("no hunk here\n", 1),
            ("@@ -x +1 @@\n", 1),
        ] {
            match Script::parse_patch(bad) {
                Err(ScriptError::Parse { line: got, .. }) => assert_eq!(got, line, "{bad}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
    }

    #[test]
    fn parse_errors_name_the_script_line() {
        for (script, line) in [
            ("", 1),
            ("# only a comment\n", 1),
            ("frobnicate 1:abcdef\n", 1),
            ("\nreplace 1:abcdef\n", 2),
            ("replace 1:abcdef <<END extra\nx\nEND\n", 1),
            ("replace 2:abcdef 1:abcdef <<END\nEND\n", 1),
            ("replace 1:abcdef 2:abcdef 3:abcdef <<END\nEND\n", 1),
            ("replace 1:abcdefg <<END\nEND\n", 1),
            ("insert after <<END\nx\nEND\n", 1),
            ("insert at-end <<END\nEND\n", 1),
            ("insert sideways 1:abcdef <<END\nx\nEND\n", 1),
            ("delete 1:abcdef <<END\nEND\n", 1),
            ("delete\n", 1),
            ("substitute <<O\nx\nO\n", 1),
            ("substitute <<O\nx\nO\nwhat <<N\ny\nN\n", 4),
            ("substitute <<O\nx\nO\nwith <<N\ny\nN\nnth=0\n", 7),
        ] {
            match Script::parse(script) {
                Err(ScriptError::Parse { line: got, why }) => {
                    assert_eq!(got, line, "{script:?}: {why}");
                }
                other => panic!("{script:?}: {other:?}"),
            }
        }
        assert_eq!("patch".parse::<Format>(), Ok(Format::Patch));
        assert!("diff".parse::<Format>().is_err());
    }

    /// The acceptance loop an agent runs: read, copy tokens out of the
    /// printed rows as text, write a script, apply it against the read's
    /// `seq=`.
    #[test]
    fn anchors_copied_from_a_read_apply_as_is() {
        use crate::protocol::read::{DOC_READ_CAP, LineRange, format_lines};

        let source = "fn main() {\r\n    let x = 1;\r\n    run(x);\r\n}\r\n";
        let mut store = DocumentStore::for_test();
        let document = store.open("mem://read".into(), source.into());
        let snapshot = store.snapshot(document).unwrap();
        let read = format_lines(&snapshot.text, snapshot.seq.0, LineRange::ALL, DOC_READ_CAP);
        let token = |line: &str| -> String {
            read.lines()
                .find_map(|row| {
                    let (anchor, text) = row.split_once('|')?;
                    (text == line).then(|| anchor.to_string())
                })
                .unwrap()
        };
        let seq: u64 = read
            .lines()
            .last()
            .and_then(|trailer| {
                trailer
                    .split(' ')
                    .find_map(|word| word.strip_prefix("seq="))
            })
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(seq, snapshot.seq.0);
        let script = format!(
            "replace {} <<END\n    let x = 2;\nEND\ninsert before {} <<END\n    log(x);\nEND\n",
            token("    let x = 1;"),
            token("}")
        );
        let plan = Script::parse(&script)
            .unwrap()
            .plan(&snapshot.text, &PlanOptions::default())
            .unwrap();
        let outcome = store.transact(document, aterm_buffer::Seq(seq), plan.edits.clone());
        assert!(
            matches!(outcome, DocumentTxnOutcome::Committed { .. }),
            "{outcome:?}"
        );
        let committed = store.snapshot(document).unwrap();
        assert_eq!(
            &*committed.text,
            "fn main() {\r\n    let x = 2;\r\n    run(x);\r\n    log(x);\r\n}\r\n"
        );
        let reply = plan.reply(&committed.text, committed.seq.0);
        let reread = format_lines(
            &committed.text,
            committed.seq.0,
            LineRange::ALL,
            DOC_READ_CAP,
        );
        for hunk in &plan.hunks {
            let (first, last) = hunk.anchors(&committed.text).unwrap();
            for anchor in [first, last] {
                assert!(
                    reread.contains(&format!("{anchor}|")),
                    "{anchor} not in\n{reread}"
                );
                assert!(reply.contains(&anchor.to_string()), "{reply}");
            }
        }
    }

    mod conformance {
        //! Tier 1: the planner refines `AmacsAnchor` (`aterm_models_native::models_amacs_anchor`).

        use super::super::*;
        use aterm_models_native::models_amacs_anchor::amacs_anchor_model;
        use aterm_spec::derive::Model;
        use aterm_spec::interp::admits;
        use std::collections::BTreeMap;

        type State = BTreeMap<&'static str, i64>;

        const LINES: usize = 3;
        const TARGET: &str = "target";

        #[derive(Clone, Copy, Debug)]
        struct Scenario {
            want: usize,
            at: usize,
            near: bool,
            matches: usize,
            mode: usize,
        }

        impl Scenario {
            fn all() -> Vec<Self> {
                let mut all = Vec::new();
                for want in 1..=LINES {
                    for at in 0..=LINES {
                        for near in [false, true] {
                            for matches in 0..=3 {
                                for mode in 0..=2 {
                                    all.push(Self {
                                        want,
                                        at,
                                        near,
                                        matches,
                                        mode,
                                    });
                                }
                            }
                        }
                    }
                }
                all
            }

            /// Lines 1-3: the target at `at`, fillers elsewhere; line 4
            /// holds the substitute's matches, out of op 1's reach.
            fn text(&self) -> String {
                let mut text = String::new();
                for line in 1..=LINES {
                    if line == self.at {
                        text.push_str(TARGET);
                    } else {
                        text.push_str(&format!("filler {line}"));
                    }
                    text.push('\n');
                }
                text.push_str(&"x".repeat(self.matches));
                text.push_str("\ntail\n");
                text
            }

            fn script(&self) -> Script {
                let anchor = Anchor {
                    line: self.want,
                    h6: Hash6::of(TARGET),
                };
                let which = ["", "all\n", "nth=2\n"][self.mode];
                Script::parse(&format!(
                    "replace {anchor} {anchor} <<END\nONE\nEND\nsubstitute <<OLD\nx\nOLD\nwith <<NEW\nY\nNEW\n{which}"
                ))
                .unwrap()
            }

            fn options(&self) -> PlanOptions<'static> {
                PlanOptions {
                    near: self.near.then_some(1),
                    base_map: None,
                }
            }

            /// The model's state once its setup actions chose this scenario,
            /// reached through the model's own transitions.
            fn start(&self, model: &Model) -> State {
                let mut state = model.init_state();
                let mut fire = |action: &str, times: usize| {
                    for _ in 0..times {
                        assert!(model.fire(action, &mut state), "{action} {self:?}");
                    }
                };
                fire("MoveAnchor", self.want - 1);
                fire("PlaceContent", self.at);
                fire("GiveNear", usize::from(self.near));
                fire("AddMatch", self.matches);
                fire("ChooseMode", self.mode);
                fire("Start", 1);
                state
            }
        }

        fn int(value: usize) -> i64 {
            i64::try_from(value).unwrap()
        }

        /// How many of the script's ops show in `text`: op 1's `ONE` on
        /// line `landed`, op 2's `Y`s (as many as it replaced).
        fn applied_in(text: &str, landed: usize, replaced: usize) -> i64 {
            let lines = Lines::of(text);
            let one = landed > 0 && lines.content(landed) == Some("ONE");
            let two = replaced > 0 && text.matches('Y').count() == replaced;
            i64::from(one) + i64::from(two)
        }

        /// What a resolver did with op 1: the line it landed on, or a
        /// refusal.
        type Resolve =
            fn(&Script, usize, &str, &PlanOptions<'_>) -> Result<Vec<Region>, ScriptError>;

        fn real_resolve(
            script: &Script,
            op: usize,
            text: &str,
            options: &PlanOptions<'_>,
        ) -> Result<Vec<Region>, ScriptError> {
            resolve_op(op, &script.ops()[op - 1], &Lines::of(text), options)
        }

        /// One run of a planner (the real one, or a rig) over a scenario,
        /// as the model's states (the start, then one state per step) and
        /// the text the caller holds at the end.
        fn run(
            model: &Model,
            scenario: &Scenario,
            resolve: Resolve,
            apply_as_resolved: bool,
        ) -> (Vec<State>, String) {
            let script = scenario.script();
            let options = scenario.options();
            let pre = scenario.text();
            let mut held = pre.clone();
            let mut states = vec![scenario.start(model)];
            let mut state = states[0].clone();
            let mut landed = 0;
            let mut replaced = 0;
            let mut staged = Vec::new();
            for op in 1..=2 {
                match resolve(&script, op, &held, &options) {
                    Ok(regions) => {
                        if op == 1 {
                            let lines = Lines::of(&held);
                            landed = lines.line_at(regions[0].range.start).unwrap();
                        } else {
                            replaced = regions.len();
                        }
                        if apply_as_resolved {
                            // the text the moment it resolves.
                            held = plan_of(regions.clone()).apply(&held);
                        }
                        staged.extend(regions);
                        state.insert("landed", int(landed));
                        state.insert("replaced", int(replaced));
                        state.insert("staged", int(op));
                        state.insert("phase", int(op) + 1);
                        state.insert("applied", applied_in(&held, landed, replaced));
                        states.push(state.clone());
                    }
                    Err(_) => {
                        state.insert("applied", applied_in(&held, landed, replaced));
                        state.insert("outcome", 2);
                        state.insert("phase", 4);
                        states.push(state.clone());
                        return (states, held);
                    }
                }
            }
            if !apply_as_resolved {
                let mut admitted = Admitted::default();
                for op in 1..=2 {
                    let regions: Vec<Region> = staged
                        .iter()
                        .filter(|region| region.op == op)
                        .cloned()
                        .collect();
                    admitted
                        .admit(op, &regions)
                        .expect("op 1 and op 2 touch different lines");
                }
                held = plan_of(staged).apply(&pre);
            }
            state.insert("applied", applied_in(&held, landed, replaced));
            state.insert("outcome", 1);
            state.insert("phase", 4);
            states.push(state);
            (states, held)
        }

        /// The first step of `states` the model does not admit, if any.
        fn refused_step(model: &Model, states: &[State]) -> Option<usize> {
            states
                .windows(2)
                .position(|pair| admits(model, &pair[0], &pair[1]).is_none())
        }

        fn broken_law(model: &Model, states: &[State]) -> Option<&'static str> {
            ["AllOrNothing", "AnchorExact", "SubstituteUnique"]
                .into_iter()
                .find(|law| {
                    states
                        .iter()
                        .any(|state| !model.check_invariant(law, state))
                })
        }

        #[test]
        fn conformance_refines_amacs_anchor() {
            let model = amacs_anchor_model();
            let fillers: Vec<Hash6> = (1..=LINES)
                .map(|line| Hash6::of(&format!("filler {line}")))
                .collect();
            assert!(!fillers.contains(&Hash6::of(TARGET)));
            let mut actions = std::collections::BTreeSet::new();
            for scenario in Scenario::all() {
                let (states, held) = run(&model, &scenario, real_resolve, false);
                for pair in states.windows(2) {
                    let action = admits(&model, &pair[0], &pair[1]);
                    assert!(
                        action.is_some(),
                        "{scenario:?}: the model admits no step {:?} -> {:?}",
                        pair[0],
                        pair[1]
                    );
                    actions.extend(action);
                }
                assert_eq!(broken_law(&model, &states), None, "{scenario:?}");
                let pre = scenario.text();
                match scenario.script().plan(&pre, &scenario.options()) {
                    Ok(plan) => {
                        assert_eq!(states.last().unwrap()["outcome"], 1, "{scenario:?}");
                        assert_eq!(plan.apply(&pre), held, "{scenario:?}");
                    }
                    Err(_) => {
                        assert_eq!(states.last().unwrap()["outcome"], 2, "{scenario:?}");
                        assert_eq!(held, pre, "{scenario:?}");
                    }
                }
            }
            for action in [
                "LandExact",
                "Relocate",
                "Conflict",
                "Substitute",
                "SubstituteRefused",
                "Commit",
            ] {
                assert!(actions.contains(action), "no scenario reached {action}");
            }

            let rigs: [(&str, Resolve, bool, &str); 3] = [
                ("PartialApply", real_resolve, true, "AllOrNothing"),
                ("OffByOne", off_by_one_resolve, false, "AnchorExact"),
                (
                    "FirstMatchWins",
                    first_match_resolve,
                    false,
                    "SubstituteUnique",
                ),
            ];
            for (mutant, resolve, apply_as_resolved, law) in rigs {
                let mut caught = 0;
                for scenario in Scenario::all() {
                    let (states, _) = run(&model, &scenario, resolve, apply_as_resolved);
                    if refused_step(&model, &states).is_some() {
                        caught += 1;
                        assert!(
                            states
                                .iter()
                                .any(|state| !model.check_invariant(law, state)),
                            "{mutant} {scenario:?}: refused without breaking {law}"
                        );
                    }
                }
                assert!(caught > 0, "the {mutant} rig passed as a refinement");
            }
        }

        /// OffByOne: checks the h6 of the line after the anchor's, and
        /// lands on the anchor's line.
        fn off_by_one_resolve(
            script: &Script,
            op: usize,
            text: &str,
            options: &PlanOptions<'_>,
        ) -> Result<Vec<Region>, ScriptError> {
            let lines = Lines::of(text);
            if let (1, Op::Replace { first, body, .. }) = (op, &script.ops()[0])
                && lines.h6(first.line + 1) == Some(first.h6)
            {
                let span = lines.span(first.line).unwrap();
                return Ok(vec![Region {
                    op,
                    range: span.start..span.end,
                    insert: terminated(body, "\n"),
                    holds: None,
                    opens_tail: false,
                }]);
            }
            real_resolve(script, op, text, options)
        }

        /// FirstMatchWins: a unique substitute takes the first of several
        /// matches.
        fn first_match_resolve(
            script: &Script,
            op: usize,
            text: &str,
            options: &PlanOptions<'_>,
        ) -> Result<Vec<Region>, ScriptError> {
            match real_resolve(script, op, text, options) {
                Err(ScriptError::Ambiguous { .. }) => {
                    let start = text.find('x').unwrap();
                    Ok(vec![Region {
                        op,
                        range: start..start + 1,
                        insert: "Y".into(),
                        holds: None,
                        opens_tail: false,
                    }])
                }
                other => other,
            }
        }
    }
}
