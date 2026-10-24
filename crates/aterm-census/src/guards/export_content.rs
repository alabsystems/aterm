// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! export-content: a file the export publishes carries nothing its content guard forbids.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_regex::{Regex, RegexBuilder};

use super::captures::{self, EXTRA, Row, file_rows};
use super::{Finding, Rule, Tree};
use crate::export::plaintext::{self, Plaintext};
use crate::export::{MANIFEST, Selection, rewrite};

pub(super) const EXPORT_CONTENT: Rule = Rule {
    id: "export-content",
    why: "the publication engine refuses an export whose text carries a home, the account, \
          the scratch uid or a token, and judges only a pushed main: a fixture that names one \
          lands unnoticed and blocks the next release",
    scope: &[],
    check: export_content,
};

/// The engine's one inline allow tag.
const ALLOW_TAG: &str = "gitleaks:allow";

/// The first line of the header of the baseline's credential section.
const CREDENTIALS: &str = "# Credentials";

/// The work one whole-file search for the rows without anchors may spend;
/// exhausting it only sends the file to the per-line search.
const FILE_STEPS: u64 = 1 << 32;

/// The baseline's credential patterns, as the engine reads them: a section is a
/// header (a run of comment lines after a blank line, a pattern or the top of the
/// file) plus the patterns after it, up to a blank line; the credential section's
/// header starts `# Credentials` (alone, or with a `.`); a pattern that is also
/// in another section is not one.
fn credentials(baseline: &str) -> BTreeSet<&str> {
    let (mut credentials, mut others) = (BTreeSet::new(), Vec::new());
    let (mut inside, mut after_comment) = (false, false);
    for raw in baseline.lines() {
        let line = raw.trim();
        if line.is_empty() {
            (inside, after_comment) = (false, false);
        } else if line.starts_with('#') {
            if !after_comment {
                inside = line == CREDENTIALS || line.starts_with(&format!("{CREDENTIALS}."));
            }
            after_comment = true;
        } else {
            after_comment = false;
            if inside {
                credentials.insert(line);
            } else {
                others.push(line);
            }
        }
    }
    for line in others {
        credentials.remove(line);
    }
    credentials
}

/// One piece of a pattern, as [`anchors`] reads it.
#[derive(Clone, Copy)]
enum Piece {
    /// A character a match holds here (`true`: in either case, from `[Xx]`).
    Char(char, bool),
    /// Zero-width (`^`, `$`, `\b`): what is either side of it stays adjacent.
    Assert,
    /// A quantifier: the piece before it may be absent or repeated.
    Repeat,
    /// Anything else (a group, a class, `.`, `\d`): it ends a literal run.
    Other,
}

/// The index of the `]` closing the class opened at `chars[open]`.
fn class_end(chars: &[char], open: usize) -> Option<usize> {
    let mut i = open + 1;
    if chars.get(i) == Some(&'^') {
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        i += 1;
    }
    while let Some(&c) = chars.get(i) {
        match c {
            ']' => return Some(i),
            '\\' => i += 2,
            '[' if chars.get(i + 1) == Some(&':') => {
                i += 2;
                while chars.get(i..i + 2)? != [':', ']'] {
                    i += 1;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    None
}

/// The literals every match of `pattern` holds: per top-level alternative, its
/// longest run of two or more characters a match holds side by side (`[Xx]` a
/// letter of either case, which folds the whole run). `None` when an alternative
/// has none, or the pattern sets a flag or spells a code point in hex, so the row
/// is read whole.
fn anchors(pattern: &str) -> Option<Vec<(String, bool)>> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut branches: Vec<Vec<Piece>> = vec![Vec::new()];
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let piece = match c {
            '|' => {
                branches.push(Vec::new());
                i += 1;
                continue;
            }
            '\\' => {
                let e = *chars.get(i + 1)?;
                i += 1;
                match e {
                    'b' | 'B' | 'A' | 'z' | '<' | '>' => Piece::Assert,
                    'x' | 'u' | 'U' => return None,
                    e if e.is_ascii_punctuation() => Piece::Char(e, false),
                    _ => Piece::Other,
                }
            }
            '[' => {
                let end = class_end(&chars, i)?;
                let set = &chars[i + 1..end];
                i = end;
                match set {
                    [a, b] if a != b && a.is_ascii_alphabetic() && a.eq_ignore_ascii_case(b) => {
                        Piece::Char(a.to_ascii_lowercase(), true)
                    }
                    _ => Piece::Other,
                }
            }
            '(' => {
                if chars.get(i + 1) == Some(&'?') {
                    return None;
                }
                let mut depth = 0;
                loop {
                    match chars.get(i)? {
                        '\\' => i += 1,
                        '[' => i = class_end(&chars, i)?,
                        '(' => depth += 1,
                        ')' if depth == 1 => break,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                Piece::Other
            }
            ')' => return None,
            '?' | '*' | '+' => Piece::Repeat,
            '{' => {
                while chars.get(i).is_some_and(|&c| c != '}') {
                    i += 1;
                }
                Piece::Repeat
            }
            '^' | '$' => Piece::Assert,
            '.' => Piece::Other,
            c => Piece::Char(c, false),
        };
        branches.last_mut()?.push(piece);
        i += 1;
    }
    branches
        .iter()
        .map(|pieces| {
            let (mut best, mut run) = (Vec::<(char, bool)>::new(), Vec::new());
            for piece in pieces.iter().copied().chain([Piece::Other]) {
                match piece {
                    Piece::Char(c, fold) => run.push((c, fold)),
                    Piece::Assert => {}
                    Piece::Repeat | Piece::Other => {
                        if matches!(piece, Piece::Repeat) {
                            run.pop();
                        }
                        if run.len() > best.len() {
                            best = std::mem::take(&mut run);
                        }
                        run.clear();
                    }
                }
            }
            let fold = best.iter().any(|&(_, f)| f);
            (best.len() >= 2).then(|| {
                let text: String = best.iter().map(|&(c, _)| c).collect();
                (
                    if fold {
                        text.to_ascii_lowercase()
                    } else {
                        text
                    },
                    fold,
                )
            })
        })
        .collect()
}

/// An anchor: its bytes, whether it matches in either case, and its row.
type Anchor = (Vec<u8>, bool, usize);

/// Every row's anchors, found together: a map from an anchor's first two bytes
/// (ASCII-folded) to the anchors that start so.
struct Anchors {
    by_lead: HashMap<[u8; 2], Vec<Anchor>>,
    lead: Vec<bool>,
}

impl Anchors {
    const fn key(a: u8, b: u8) -> [u8; 2] {
        [a.to_ascii_lowercase(), b.to_ascii_lowercase()]
    }

    fn new(rows: &[(usize, Vec<(String, bool)>)]) -> Self {
        let mut by_lead: HashMap<[u8; 2], Vec<Anchor>> = HashMap::new();
        let mut lead = vec![false; 1 << 16];
        for (row, anchors) in rows {
            for (text, fold) in anchors {
                let bytes = text.as_bytes();
                let key = Self::key(bytes[0], bytes[1]);
                lead[usize::from(u16::from_be_bytes(key))] = true;
                by_lead
                    .entry(key)
                    .or_default()
                    .push((bytes.to_vec(), *fold, *row));
            }
        }
        Self { by_lead, lead }
    }

    /// Each `(row, 0-based line)` of `text` holding one of the row's anchors.
    fn lines(&self, text: &[u8]) -> BTreeSet<(usize, usize)> {
        let mut hits = BTreeSet::new();
        let mut line = 0;
        for (i, pair) in text.windows(2).enumerate() {
            let key = Self::key(pair[0], pair[1]);
            if self.lead[usize::from(u16::from_be_bytes(key))] {
                for (anchor, fold, row) in &self.by_lead[&key] {
                    let here = text.get(i..i + anchor.len());
                    if here.is_some_and(|h| {
                        if *fold {
                            h.eq_ignore_ascii_case(anchor)
                        } else {
                            h == anchor
                        }
                    }) {
                        hits.insert((*row, line));
                    }
                }
            }
            if pair[0] == b'\n' {
                line += 1;
            }
        }
        hits
    }
}

/// The rows the export's text is held to, and how each is searched.
struct Held<'a> {
    rows: Vec<&'a Row>,
    /// Per row: a baseline credential row, which a tagged line is exempt from.
    credential: Vec<bool>,
    anchors: Anchors,
    /// The rows without anchors, and their union for a whole-file search.
    whole: Vec<usize>,
    any: Option<Regex>,
}

impl<'a> Held<'a> {
    fn new(rows: Vec<&'a Row>, credential: Vec<bool>) -> Self {
        let (mut anchored, mut whole) = (Vec::new(), Vec::new());
        for (k, row) in rows.iter().enumerate() {
            match anchors(row.line.as_str()) {
                Some(found) => anchored.push((k, found)),
                None => whole.push(k),
            }
        }
        let union: Vec<String> = whole
            .iter()
            .map(|&k| format!("(?:{})", rows[k].line.as_str()))
            .collect();
        let any = RegexBuilder::new(&format!("(?m){}", union.join("|")))
            .step_limit(FILE_STEPS)
            .build()
            .ok();
        Self {
            rows,
            credential,
            anchors: Anchors::new(&anchored),
            whole,
            any,
        }
    }

    /// Each `(row, 0-based line)` of `text` some row may match there.
    fn candidates(&self, text: &[u8]) -> BTreeSet<(usize, usize)> {
        let mut found = self.anchors.lines(text);
        if self.whole.is_empty() {
            return found;
        }
        let lossy = String::from_utf8_lossy(text);
        if self
            .any
            .as_ref()
            .is_some_and(|any| matches!(any.try_is_match(&lossy), Ok(false)))
        {
            return found;
        }
        for line in 0..=text.iter().filter(|&&b| b == b'\n').count() {
            found.extend(self.whole.iter().map(|&k| (k, line)));
        }
        found
    }
}

fn export_content(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    held_to(tree, captures::baseline(), out);
}

fn finding(path: &str, line: usize, detail: String) -> Finding {
    Finding {
        rule: EXPORT_CONTENT.id,
        path: path.to_string(),
        line,
        detail,
    }
}

/// The rule over `tree`, with the engine's baseline as this host reads it (`Err`
/// when it has none, which holds the text to the export's own rows).
fn held_to(tree: &Tree<'_>, baseline: Result<(String, String), String>, out: &mut Vec<Finding>) {
    let read = |rel: &str| match std::fs::read_to_string(tree.root.join(rel)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(finding(
            rel,
            0,
            format!("unreadable ({e}), so the rule cannot run"),
        )),
    };
    let (manifest, extra) = match (read(MANIFEST), read(EXTRA)) {
        (Ok(manifest), Ok(extra)) => (manifest, extra.unwrap_or_default()),
        (manifest, extra) => {
            out.extend(manifest.err().into_iter().chain(extra.err()));
            return;
        }
    };
    let selection = match manifest
        .as_deref()
        .map_or_else(|| Ok(Selection::everything()), Selection::parse)
    {
        Ok(selection) => selection,
        Err((line, why)) => {
            out.push(finding(MANIFEST, line, why));
            return;
        }
    };
    let files: Vec<&str> = tree
        .corpus
        .paths
        .iter()
        .map(String::as_str)
        .filter(|rel| selection.selects(rel))
        .collect();
    if manifest.is_some() && files.is_empty() {
        out.push(finding(
            MANIFEST,
            0,
            "it selects no file of the checkout, so the engine refuses the export".into(),
        ));
        return;
    }
    let extras = file_rows(&extra, EXTRA, EXTRA, EXPORT_CONTENT.id, out);
    let (base, exempt) = match &baseline {
        Ok((path, text)) => (
            file_rows(text, "the engine's baseline", path, EXPORT_CONTENT.id, out),
            credentials(text),
        ),
        Err(_) => (Vec::new(), BTreeSet::new()),
    };
    let rows: Vec<&Row> = extras.iter().chain(&base).collect();
    if rows.is_empty() {
        return;
    }
    let credential = extras
        .iter()
        .map(|_| false)
        .chain(base.iter().map(|row| exempt.contains(row.line.as_str())))
        .collect();
    let held = Held::new(rows, credential);
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    let found: Vec<Vec<Finding>> = std::thread::scope(|s| {
        let running: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut found = Vec::new();
                    while let Some(rel) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        judge(tree, rel, &held, &mut found);
                    }
                    found
                })
            })
            .collect();
        running
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    out.extend(found.into_iter().flatten());
}

/// How much of a file the engine's grep reads for a NUL before it calls the file binary:
/// macOS's grep (BSD grep 2.6.0) looks only in its first 32 KiB read (a first NUL at byte 32767
/// is binary to `grep -IlE`, at 32768 text).
const GREP_HEAD: usize = 32 << 10;

/// One exported file as the engine reads it: its path, its content as the engine's grep reads
/// it, and what it decodes to ([`Plaintext`]). The transforms leave a file holding a NUL as it
/// is; one with a NUL where the engine's grep looks ([`GREP_HEAD`]) is binary to it, so a match
/// in its bytes, as in what it decodes to, is one finding per row at line 0, cleared by no tag.
/// Otherwise its lines are held as [`by_line`] says.
fn judge(tree: &Tree<'_>, rel: &str, held: &Held<'_>, out: &mut Vec<Finding>) {
    for (k, _) in held.candidates(rel.as_bytes()) {
        if held.rows[k].line.is_match(rel) {
            out.push(finding(
                rel,
                0,
                format!("its path matches {}", held.rows[k].source),
            ));
        }
    }
    let bytes = match std::fs::read(tree.root.join(rel)) {
        Ok(bytes) => bytes,
        Err(e) => {
            out.push(finding(rel, 0, format!("unreadable: {e}")));
            return;
        }
    };
    let exported = if bytes.contains(&0) {
        bytes
    } else {
        rewrite(&bytes)
    };
    let plain = plaintext::read(&exported);
    let binary = exported[..exported.len().min(GREP_HEAD)].contains(&0);
    // A binary stream that decodes whole is judged by its plaintext alone: the
    // engine sets its bytes' verdict aside (a zstd copy holds them again).
    if !binary || !matches!(plain, Plaintext::Stream { stopped: None, .. }) {
        let how = if binary { Lines::Bytes } else { Lines::Text };
        by_line(rel, &exported, how, held, out);
    }
    match plain {
        Plaintext::None => {}
        Plaintext::Bom { encoding, text } => whole(rel, encoding, &text, held, out),
        Plaintext::Stream {
            kind,
            copy,
            stopped,
        } => {
            let stopped = stopped.as_deref();
            by_line(rel, &copy, Lines::Copy { kind, stopped }, held, out);
        }
        Plaintext::Unjudged { kind, why } => out.push(finding(
            rel,
            0,
            format!(
                "its {kind} stream {why}, so it is not judged here (`verify export` runs the \
                 engine itself on a pushed HEAD)"
            ),
        )),
    }
}

/// What [`by_line`] holds.
#[derive(Clone, Copy)]
enum Lines<'a> {
    /// A text file's lines, as exported: each at its number, a tag honoured.
    Text,
    /// A binary file's bytes.
    Bytes,
    /// The copy the engine greps of a compressed file, and why it stopped
    /// early, if it did.
    Copy {
        kind: &'static str,
        stopped: Option<&'a str>,
    },
}

/// Each line of `text`, as `grep` reads it, held to the rows. The engine reads
/// the lines its text grep prints as UTF-8 and aborts its whole scan on one
/// that is not (its BUG-3), so such a line that matches is a finding no tag
/// clears.
fn by_line(rel: &str, text: &[u8], how: Lines<'_>, held: &Held<'_>, out: &mut Vec<Finding>) {
    let binary = !matches!(how, Lines::Text);
    let lines: Vec<(&[u8], std::borrow::Cow<'_, str>)> = text
        .split(|&b| b == b'\n')
        .map(|raw| (raw, String::from_utf8_lossy(raw)))
        .collect();
    let mut reported = BTreeSet::new();
    for (k, i) in held.candidates(text) {
        if binary && reported.contains(&k) {
            continue;
        }
        let (raw, line) = &lines[i];
        let row = held.rows[k];
        let source = &row.source;
        let detail = match (row.line.try_is_match(line), how) {
            (Ok(false), _) => continue,
            (Ok(true), Lines::Text) if std::str::from_utf8(raw).is_err() => format!(
                "as exported, this line matches {source} and its bytes are not UTF-8: the \
                 engine's grep prints it and the engine aborts its scan on output that is not \
                 UTF-8 (its BUG-3), whatever tag the line carries; write the line as UTF-8"
            ),
            (Ok(true), Lines::Text) if held.credential[k] && line.contains(ALLOW_TAG) => continue,
            (Ok(true), Lines::Text) => format!(
                "as exported, this line matches {source}; replace the text, or spell what a \
                 test needs in halves"
            ),
            (Ok(true), Lines::Bytes) => format!(
                "its bytes match {source} (a binary file, which the transforms leave as it is \
                 and no tag clears); make it without that text"
            ),
            (Ok(true), Lines::Copy { kind, stopped }) => format!(
                "what it decodes to{} matches {source} (the engine greps a {kind} file's \
                 decoded stream, where no tag clears a match); make it without that text",
                stopped.map_or_else(String::new, |why| format!(
                    " before its stream stops where the engine's does ({why})"
                ))
            ),
            (Err(_), _) => format!(
                "{} exhausted the search for {source}, so it is not judged",
                match how {
                    Lines::Text => "this line",
                    Lines::Bytes => "a line of its bytes",
                    Lines::Copy { .. } => "a line of what it decodes to",
                }
            ),
        };
        reported.insert(k);
        out.push(finding(rel, if binary { 0 } else { i + 1 }, detail));
    }
}

/// A BOM'd file's decoded `text`, searched whole as the engine searches it
/// with Python's `re`: a row both read alike ([`python`]) matches when it
/// matches the text or the text less a final newline, where Python's `$` also
/// matches. A row read apart is unjudged, unless it reads apart only past
/// ASCII and the text holds none of its anchors, which every match in either
/// reading holds.
fn whole(rel: &str, encoding: &str, text: &str, held: &Held<'_>, out: &mut Vec<Finding>) {
    let ascii = text
        .bytes()
        .all(|b| b.is_ascii() && !(0x1c..=0x1f).contains(&b));
    let anchored: BTreeSet<usize> = held
        .anchors
        .lines(text.as_bytes())
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let less = text.strip_suffix('\n');
    for (k, row) in held.rows.iter().enumerate() {
        let source = &row.source;
        let reading = python(row.line.as_str());
        if reading == Python::Apart || (reading == Python::AsciiAlike && !ascii) {
            if reading == Python::AsciiAlike && !held.whole.contains(&k) && !anchored.contains(&k) {
                continue;
            }
            out.push(finding(
                rel,
                0,
                format!(
                    "the engine searches its {encoding} text whole with Python's re, which may \
                     read {source} apart from this guard's regex there, so it is not judged"
                ),
            ));
            continue;
        }
        let found = row.line.try_is_match(text).and_then(|hit| match less {
            Some(less) if !hit => row.line.try_is_match(less),
            _ => Ok(hit),
        });
        let detail = match found {
            Ok(false) => continue,
            Ok(true) => format!(
                "decoded as the engine reads it ({encoding}), its text matches {source} (no tag \
                 clears it); make it without that text"
            ),
            Err(_) => format!(
                "its {encoding} text exhausted the search for {source}, so it is not judged"
            ),
        };
        out.push(finding(rel, 0, detail));
    }
}

/// How Python's `re`, the engine's reader of a BOM'd file's text, reads a row
/// beside this crate's regex, both searching one whole text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Python {
    /// Alike over any text, once the text less a final newline is searched
    /// too: every `$` ends its branch.
    Alike,
    /// Alike over ASCII text without U+001C..U+001F; past it, its `\w`, `\d`,
    /// `\s`, `\b` or `(?i)` may read a character apart.
    AsciiAlike,
    /// Apart: a character past ASCII, a POSIX class, a flag or extension but
    /// `(?:` and `(?i)`, `{,n}`, an escape the two read apart (`\<` and `\>` are
    /// literal in Python), or a `$` something may follow.
    Apart,
}

fn python(pattern: &str) -> Python {
    /// An escape's reading: `None` apart, `Some(true)` alike over ASCII only.
    fn escape(e: Option<&char>) -> Option<bool> {
        match *e? {
            'w' | 'W' | 'd' | 'D' | 's' | 'S' | 'b' | 'B' => Some(true),
            'n' | 't' | 'r' | 'f' | 'v' | 'a' | 'x' | 'u' | 'U' | 'A' => Some(false),
            e => (e.is_ascii_punctuation() && !matches!(e, '<' | '>')).then_some(false),
        }
    }
    let chars: Vec<char> = pattern.chars().collect();
    let (mut ascii_only, mut depth, mut i) = (false, 0usize, 0);
    while let Some(&c) = chars.get(i) {
        match c {
            _ if !c.is_ascii() => return Python::Apart,
            '\\' => {
                let Some(only) = escape(chars.get(i + 1)) else {
                    return Python::Apart;
                };
                ascii_only |= only;
                i += 1;
            }
            '[' => {
                let Some(end) = class_end(&chars, i) else {
                    return Python::Apart;
                };
                let mut j = i + 1;
                while j < end {
                    match chars[j] {
                        '\\' => {
                            j += 1;
                            let Some(only) = escape(chars.get(j)) else {
                                return Python::Apart;
                            };
                            ascii_only |= only;
                        }
                        '[' => return Python::Apart,
                        c if !c.is_ascii() => return Python::Apart,
                        _ => {}
                    }
                    j += 1;
                }
                i = end;
            }
            '(' => {
                depth += 1;
                match chars.get(i + 1..i + 4) {
                    Some(['?', ':', ..]) => {}
                    Some(['?', 'i', ')' | ':']) => ascii_only = true,
                    _ if chars.get(i + 1) == Some(&'?') => return Python::Apart,
                    _ => {}
                }
            }
            ')' => depth = depth.saturating_sub(1),
            '{' if chars.get(i + 1) == Some(&',') => return Python::Apart,
            '$' => {
                let (mut j, mut d) = (i + 1, depth);
                while chars.get(j) == Some(&')') {
                    (j, d) = (j + 1, d.saturating_sub(1));
                }
                if !(j == chars.len() || (chars[j] == '|' && d == 0)) {
                    return Python::Apart;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if ascii_only {
        Python::AsciiAlike
    } else {
        Python::Alike
    }
}

#[cfg(test)]
#[path = "export_content_tests.rs"]
mod tests;
