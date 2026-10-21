// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified diffs, patch mails and the diff `git commit -v` appends to a
//! commit message.
//!
//! A hunk header's line counts go into the [`LexState`], so inside a hunk a
//! line is read by its first byte alone: a removed line that reads
//! `--- a/x` is a removal, not a file header. Past a hunk's last line, or in
//! a hand-edited hunk whose counts are off, the headers are recognised by
//! their words and `+`/`-` lines still read as added and removed.

use super::{Cursor, LexState, Mode, Spans, SyntaxClass, decimal_push};

/// The words a diff's file and mail header lines start with.
const HEADERS: &[&str] = &[
    "diff ",
    "index ",
    "--- ",
    "+++ ",
    "new file mode",
    "deleted file mode",
    "old mode",
    "new mode",
    "similarity index",
    "dissimilarity index",
    "rename from",
    "rename to",
    "copy from",
    "copy to",
    "Binary files",
    "Only in ",
    "From ",
    "Date: ",
    "Subject: ",
    "commit ",
    "Author: ",
    "Merge: ",
];

pub(super) fn lex(state: LexState, line: &str, out: &mut Spans) -> LexState {
    if let Mode::Hunk(old, new) = state.mode
        && let Some(mode) = hunk_line(old, new, line, out)
    {
        return LexState { mode, ..state };
    }
    // Outside a hunk, or a line no hunk has: a header.
    if line.starts_with("@@") {
        return hunk_header(state, line, out);
    }
    let class = if line == "---" || HEADERS.iter().any(|header| line.starts_with(header)) {
        Some(SyntaxClass::Meta)
    } else {
        match line.as_bytes().first() {
            Some(b'+') => Some(SyntaxClass::Added),
            Some(b'-') => Some(SyntaxClass::Removed),
            Some(b'\\' | b'#') => Some(SyntaxClass::Comment),
            _ => None,
        }
    };
    if let Some(class) = class {
        out.push(0, line.len(), class);
    }
    LexState {
        mode: Mode::Code,
        ..state
    }
}

/// A line inside a hunk with `old` and `new` lines left, read by its first
/// byte; returns the mode after it. `None` for a line that cannot be in a
/// hunk, which ends it early.
fn hunk_line(old: u32, new: u32, line: &str, out: &mut Spans) -> Option<Mode> {
    let (class, old, new) = match line.as_bytes().first() {
        Some(b'+') => (Some(SyntaxClass::Added), old, new.saturating_sub(1)),
        Some(b'-') => (Some(SyntaxClass::Removed), old.saturating_sub(1), new),
        // A context line; some tools write an empty one for a blank.
        Some(b' ') | None => (None, old.saturating_sub(1), new.saturating_sub(1)),
        // `\ No newline at end of file` counts on neither side.
        Some(b'\\') => (Some(SyntaxClass::Comment), old, new),
        Some(_) => return None,
    };
    if let Some(class) = class {
        out.push(0, line.len(), class);
    }
    Some(if hunk_open(old, new) {
        Mode::Hunk(old, new)
    } else {
        Mode::Code
    })
}

/// `@@ -a,b +c,d @@ context`: the `@@ … @@` part is a heading, and the
/// counts `b` and `d` (1 when left out) open a hunk. A combined diff's
/// `@@@` header is a heading with no counts.
fn hunk_header(state: LexState, line: &str, out: &mut Spans) -> LexState {
    let mut cur = Cursor::new(line);
    cur.bump_n(2);
    let counts = if cur.looking_at(b" -") {
        cur.bump_n(2);
        let old = range_count(&mut cur);
        if cur.looking_at(b" +") {
            cur.bump_n(2);
            let new = range_count(&mut cur);
            if cur.looking_at(b" @@") {
                cur.bump_n(3);
                old.zip(new)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let heading_end = if counts.is_some() {
        cur.pos()
    } else {
        line.len()
    };
    out.push(0, heading_end, SyntaxClass::Heading);
    let mode = match counts {
        Some((old, new)) if hunk_open(old, new) => Mode::Hunk(old, new),
        _ => Mode::Code,
    };
    LexState { mode, ..state }
}

/// A hunk range, `start[,count]`: its count, 1 when it is left out. `None`
/// when there is no start number.
fn range_count(cur: &mut Cursor<'_>) -> Option<u32> {
    digits(cur)?;
    if cur.eat(b',') { digits(cur) } else { Some(1) }
}

/// A run of decimal digits as a number (saturating), or `None` for no digit.
fn digits(cur: &mut Cursor<'_>) -> Option<u32> {
    let mut value: Option<u32> = None;
    while let Some(byte) = cur.peek().filter(u8::is_ascii_digit) {
        cur.bump();
        value = Some(decimal_push(value.unwrap_or(0), byte));
    }
    value
}

trust_contract_fn! {
    /// Whether a hunk with `old` old-side and `new` new-side lines left still
    /// has a line to come: it ends only when both sides are used up.
    pub(super) fn hunk_open(old: u32, new: u32) -> bool
    clauses [ensures result == (old > 0 || new > 0) by hunk_open_until_both_sides_end]
    {
        old > 0 || new > 0
    }
}

// Spec `diff-hunk-open` (SPECS.md), kernel-checked against the compiled body.
verify_island! {
    clean {
        theorem hunk_open_until_both_sides_end (old new : UInt32) :
            trust_import_amacs__language__diff__hunk_open old new =
              (decide (0 < old) || decide (0 < new)) := by
          simp only [trust_import_amacs__language__diff__hunk_open]
    }
}
