// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! traced-reads: a file the accounts keeper replays reads the file system only through the
//! trace (`aterm_uds::trace::fs`).
//!
//! A file says it is read under the keeper's trace with [`MARKER`] in its module doc. The keeper
//! skips a pass whose trace still stands, and the trace knows only the reads that recorded
//! themselves: a read made past it is one the replay never looks at again, so a change to that
//! file leaves a stale answer standing. The rule reads each marked file's shipped code (its
//! comments, literals and test-only items left out) and fails each read std makes for it past
//! `trace::fs`: a free `fs::read`, `read_to_string`, `read_dir`, `metadata`,
//! `symlink_metadata`, `canonicalize`, `read_link` or `exists`, and the same on `Path` or
//! `PathBuf` named as a path; `File::open`; a path's `.exists()`, `.try_exists()`,
//! `.read_dir()`, `.symlink_metadata()`, `.canonicalize()` or `.read_link()`; `.metadata()` on
//! anything but an opened file ([`FILE_HANDLES`]); `.is_file()` or `.is_dir()` on anything but
//! what a look answered ([`LOOKED`]); a hand-built open for reading (`.read(true)`); and
//! astream's `open_nonblocking`, which records nothing.
//!
//! A function that records its own look, by calling `trace::fs::opening` on what it opens, may
//! open and look past the trace within its body: its stamp is recorded before its open, so a
//! change after it moves the stamp a replay looks at (the reader that refuses a link, judges an
//! owner or bounds a length).
//!
//! The wall clock has the same rule: a run that read it raw is one whose answer moves with time
//! the trace was never told of. `aterm_time`'s readers (`unix_secs`, `unix_millis`,
//! `since_epoch`) count each read (`aterm_time::raw_reads`), and a trace scope that saw the count
//! move is poisoned, so its run is made again; a `SystemTime::now` std makes for it counts
//! nothing. The rule fails each `SystemTime::now` in a marked file's shipped code, whatever path
//! names it, inside a function that records its own look or not: read the wall through the
//! trace's `Now`, or through `aterm_time`'s counted readers. It judges no read of the monotonic
//! clock (`Instant::now`): a marked file's are the deadlines of its lock and read waits, which no
//! answer compares, and a monotonic read an answer compares is not told apart from them by its
//! spelling.
//!
//! The rule is lexical: a receiver is judged by its name, so a path bound to a name in
//! [`LOOKED`] or [`FILE_HANDLES`] passes unread. The names are those this repo gives a look's
//! answer and an opened file.

use super::{Bound, Finding, Rule, Tree, brace_close, hits, is_word};

/// What a file's module doc says to be judged by this rule.
pub(crate) const MARKER: &str = "Traced by the accounts keeper";

pub(super) const TRACED_READS: Rule = Rule {
    id: "traced-reads",
    why: "a file the accounts keeper replays (its module doc says `Traced by the accounts \
          keeper`) reads the file system through `aterm_uds::trace::fs`, or in a function that \
          records its own look with `trace::fs::opening`, and the wall clock through the \
          trace's `Now` or `aterm_time`'s counted readers: a read past the trace is one the \
          replay never looks at again, so a change there leaves the keeper's answer standing \
          stale",
    scope: &[],
    check: traced_reads,
};

/// The reads std names as free functions of `fs` (and of `Path` and `PathBuf` named as a path),
/// each a read the trace's `fs` module makes for a traced file.
const FREE_READS: &[&str] = &[
    "read",
    "read_to_string",
    "read_dir",
    "metadata",
    "symlink_metadata",
    "canonicalize",
    "read_link",
    "exists",
    "try_exists",
    "is_file",
    "is_dir",
];

/// The methods only a path has, each a read of the file system.
const PATH_READS: &[&str] = &[
    "exists",
    "try_exists",
    "read_dir",
    "symlink_metadata",
    "canonicalize",
    "read_link",
];

/// The names an opened file goes by, whose `.metadata()` asks the open file (`fstat`), not the
/// path: its open was the read.
pub(crate) const FILE_HANDLES: &[&str] = &["file", "f", "opened"];

/// The names a look's answer goes by (a `Metadata` or a `FileType`), whose `.is_file()` and
/// `.is_dir()` ask that answer, and the calls that answer one.
pub(crate) const LOOKED: &[&str] = &[
    "meta",
    "metadata",
    "m",
    "ft",
    "kind",
    "file_type",
    "symlink_metadata",
];

fn traced_reads(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(&["crates/", "apps/"]) {
        if !file.mentions(&[MARKER]) || !marked(file.raw()) {
            continue;
        }
        if tree.corpus.test_code().holds(&file.rel) {
            continue;
        }
        let code = file.shipped_code();
        let recording = recording_bodies(&code);
        let starts: Vec<usize> = std::iter::once(0)
            .chain(code.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let mut found: Vec<(usize, String)> = raw_reads(&code)
            .into_iter()
            .filter(|(at, _)| {
                !recording
                    .iter()
                    .any(|&(open, close)| open < *at && *at < close)
            })
            .map(|(at, what)| {
                (
                    at,
                    format!(
                        "{what} past the trace: read it through `aterm_uds::trace::fs`, or \
                         record the look with `trace::fs::opening` in the function that opens it"
                    ),
                )
            })
            .chain(clock_reads(&code))
            .map(|(at, what)| (starts.partition_point(|&start| start <= at), what))
            .collect();
        found.sort_by_key(|(line, _)| *line);
        found.dedup_by(|a, b| a.0 == b.0);
        for (line, what) in found {
            out.push(file.finding(TRACED_READS.id, line, what));
        }
    }
}

/// Whether the module doc that heads `raw` (its `//!` lines before its first item) carries
/// [`MARKER`].
fn marked(raw: &str) -> bool {
    for line in raw.lines() {
        let line = line.trim_start();
        if let Some(doc) = line.strip_prefix("//!") {
            if doc.contains(MARKER) {
                return true;
            }
        } else if !(line.is_empty() || line.starts_with("//") || line.starts_with("#!")) {
            return false;
        }
    }
    false
}

/// Each read in the CODE view `code` the trace does not see, by its offset, with what it is.
fn raw_reads(code: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for owner in ["fs::", "Path::", "PathBuf::"] {
        for at in hits(code, owner, Bound::Sub) {
            if at > 0 && is_word(code.as_bytes()[at - 1]) {
                continue;
            }
            if owner == "fs::" && code[..at].ends_with("trace::") {
                continue;
            }
            let name_at = at + owner.len();
            let len = code[name_at..].bytes().take_while(|&b| is_word(b)).count();
            let name = &code[name_at..name_at + len];
            let free = FREE_READS.contains(&name)
                && (owner != "fs::" || !matches!(name, "is_file" | "is_dir" | "try_exists"));
            if free {
                out.push((at, format!("`{owner}{name}` reads the file system")));
            }
        }
    }
    for at in hits(code, "File::open", Bound::Word) {
        out.push((at, "`File::open` opens a file".to_string()));
    }
    for at in hits(code, "astream_sys::open_nonblocking", Bound::Word) {
        out.push((at, "astream's `open_nonblocking` opens a file".to_string()));
    }
    for at in hits(code, ".read(true)", Bound::Sub) {
        out.push((
            at,
            "a hand-built `.read(true)` open reads a file".to_string(),
        ));
    }
    for (at, name) in methods(code) {
        let read = if PATH_READS.contains(&name) {
            true
        } else if name == "metadata" {
            receiver(code, at).is_none_or(|name| !FILE_HANDLES.contains(&name))
        } else if matches!(name, "is_file" | "is_dir") {
            receiver(code, at).is_none_or(|name| !LOOKED.contains(&name))
        } else {
            false
        };
        if read {
            out.push((at, format!("`.{name}()` reads the file system")));
        }
    }
    out.sort_by_key(|(at, _)| *at);
    out
}

/// Each read of the wall clock in the CODE view `code` that `aterm_time` does not count, by its
/// offset, with what to read in its place.
fn clock_reads(code: &str) -> Vec<(usize, String)> {
    hits(code, "SystemTime::now", Bound::Word)
        .map(|at| {
            (
                at,
                "`SystemTime::now` reads the wall clock past the trace: read it through the \
                 trace's `Now`, or `aterm_time::unix_secs`, `unix_millis` or `since_epoch`, \
                 which count the read"
                    .to_string(),
            )
        })
        .collect()
}

/// Each method call with no arguments in `code`, `.name()`, by the offset of its `.`.
fn methods(code: &str) -> Vec<(usize, &str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for (at, _) in code.match_indices('.') {
        let start = at + 1;
        let len = b[start..].iter().take_while(|&&c| is_word(c)).count();
        if len > 0 && b[start + len..].starts_with(b"()") {
            out.push((at, &code[start..start + len]));
        }
    }
    out
}

/// The receiver of the method whose `.` is at `dot` in `code`, by its last name: a binding's
/// (`meta` in `meta.is_file()`) or a call's (`path` in `entry.path().is_file()`, `metadata` in
/// `fs::metadata(p)?.is_dir()`), past blanks, newlines and a `?`. `None` where it ends in neither
/// (an index, a literal).
fn receiver(code: &str, dot: usize) -> Option<&str> {
    let b = code.as_bytes();
    let mut end = dot;
    while end > 0 && b[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end > 0 && b[end - 1] == b'?' {
        end -= 1;
    }
    if end > 0 && b[end - 1] == b')' {
        let mut depth = 0usize;
        let mut open = None;
        for i in (0..end).rev() {
            match b[i] {
                b')' => depth += 1,
                b'(' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        open = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        end = open?;
        while end > 0 && b[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
    }
    let mut start = end;
    while start > 0 && is_word(b[start - 1]) {
        start -= 1;
    }
    (start < end).then(|| &code[start..end])
}

/// The bodies, as `(open, close)` brace offsets in `code`, of each function that records its own
/// look (`fs::opening(`).
fn recording_bodies(code: &str) -> Vec<(usize, usize)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for at in hits(code, "fn", Bound::Word) {
        let rest = &code[at + 2..];
        let name_at = at + 2 + (rest.len() - rest.trim_start().len());
        if name_at == at + 2 || !b.get(name_at).is_some_and(|&c| is_word(c)) {
            continue;
        }
        let mut depth = 0usize;
        let mut open = None;
        for (i, &c) in b.iter().enumerate().skip(name_at) {
            match c {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth = depth.saturating_sub(1),
                b'{' if depth == 0 => {
                    open = Some(i);
                    break;
                }
                b';' if depth == 0 => break,
                _ => {}
            }
        }
        let Some(open) = open else {
            continue;
        };
        if let Some(close) = brace_close(code, open)
            && code[open..close].contains("fs::opening(")
        {
            out.push((open, close));
        }
    }
    out
}

#[cfg(test)]
#[path = "traced_reads_tests.rs"]
mod tests;
