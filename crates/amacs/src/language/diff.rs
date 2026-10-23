// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified diff, patch mail, and commit-message diff lexer.

use super::{Cursor, LexState, Mode, Spans, SyntaxClass, decimal_push};

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

fn hunk_line(old: u32, new: u32, line: &str, out: &mut Spans) -> Option<Mode> {
    let (class, old, new) = match line.as_bytes().first() {
        Some(b'+') => (Some(SyntaxClass::Added), old, new.saturating_sub(1)),
        Some(b'-') => (Some(SyntaxClass::Removed), old.saturating_sub(1), new),
        Some(b' ') | None => (None, old.saturating_sub(1), new.saturating_sub(1)),
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

fn range_count(cur: &mut Cursor<'_>) -> Option<u32> {
    digits(cur)?;
    if cur.eat(b',') { digits(cur) } else { Some(1) }
}

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

verify_island! {
    clean {
        theorem hunk_open_until_both_sides_end (old new : UInt32) :
            trust_import_amacs__language__diff__hunk_open old new =
              (decide (0 < old) || decide (0 < new)) := by
          simp only [trust_import_amacs__language__diff__hunk_open]
    }
}
