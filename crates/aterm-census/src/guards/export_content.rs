// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! export-content: a file the export publishes carries nothing its content guard forbids.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_regex::{Regex, RegexBuilder};

use super::captures::{self, EXTRA, Row, file_rows};
use super::{Finding, Rule, Tree};
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

/// One exported file: its path, then each line of its rewritten text, held to
/// the rows. A file holding a NUL the transforms leave as it is, and the
/// engine's grep reads its bytes (`-a`): a match there is a binary hit, at no
/// line and cleared by no tag, so it is one finding per row at line 0.
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
    let binary = bytes.contains(&0);
    let text = if binary { bytes } else { rewrite(&bytes) };
    let lossy = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = lossy.split('\n').collect();
    let mut reported = BTreeSet::new();
    for (k, i) in held.candidates(&text) {
        if binary && reported.contains(&k) {
            continue;
        }
        let line = lines[i];
        let row = held.rows[k];
        let detail = match row.line.try_is_match(line) {
            Ok(false) => continue,
            Ok(true) if !binary && held.credential[k] && line.contains(ALLOW_TAG) => continue,
            Ok(true) if binary => format!(
                "its bytes match {} (a binary file, which the transforms leave as it is and no \
                 tag clears); make it without that text",
                row.source
            ),
            Ok(true) => format!(
                "as exported, this line matches {}; replace the text, or spell what a test \
                 needs in halves",
                row.source
            ),
            Err(_) => format!(
                "{} exhausted the search for {}, so it is not judged",
                if binary {
                    "a line of its bytes"
                } else {
                    "this line"
                },
                row.source
            ),
        };
        reported.insert(k);
        out.push(finding(rel, if binary { 0 } else { i + 1 }, detail));
    }
}

#[cfg(test)]
#[path = "export_content_tests.rs"]
mod tests;
