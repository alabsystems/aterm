// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `capture-privacy`: checks that decoded test captures contain no forbidden export patterns.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aterm_regex::{Regex, RegexBuilder};

use super::{Finding, Rule, Tree};

pub(super) const CAPTURE_PRIVACY: Rule = Rule {
    id: "capture-privacy",
    why: "a test capture ships in the public snapshot as hex or escapes, which the export's \
          content guard and its home rewrites never decode: an account, a home path, a \
          banner's plan or a session id recorded in one is published as recorded",
    scope: &[],
    check: capture_privacy,
};

/// The export's own rows, relative to the checkout.
pub(super) const EXTRA: &str = "publish/forbidden-extra.txt";

const BASELINE: &str = "baseline/forbidden-content.txt";

/// Capture-specific forbidden `(description, pattern)` pairs beyond the export's rows.
pub(super) const CAPTURE_ROWS: &[(&str, &str)] = &[
    (
        "a home path that names an account",
        r"/(Users|home)/[A-Za-z0-9._-]",
    ),
    (
        "the plan on Claude Code's banner",
        r"Claude (Max|Pro|Team|Enterprise)\b|API Usage Billing",
    ),
    (
        "a session id (a UUID whose second group is not the redaction's 0000)",
        r"(?i)[0-9a-f]{8}-([1-9a-f][0-9a-f]{3}|0[1-9a-f][0-9a-f]{2}|00[1-9a-f][0-9a-f]|000[1-9a-f])-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    ),
    (
        "a toolchain checkout's cwd, or a sibling of one",
        r"~/(clean|trust)",
    ),
];

struct Format {
    ext: &'static str,
    decode: fn(&str) -> Result<Vec<Stream>, Undecodable>,
}

const FORMATS: &[Format] = &[
    Format {
        ext: "ptylog",
        decode: ptylog,
    },
    Format {
        ext: "reads",
        decode: reads,
    },
];

/// One decoded direction of a capture.
#[derive(Debug, Default)]
pub(super) struct Stream {
    /// `output`, `input` or `reads`.
    name: &'static str,
    bytes: Vec<u8>,
    /// `(offset into bytes, 1-based line of the record starting there)`, in
    /// offset order.
    records: Vec<(usize, usize)>,
}

impl Stream {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    fn push(&mut self, line: usize, bytes: &[u8]) {
        self.records.push((self.bytes.len(), line));
        self.bytes.extend_from_slice(bytes);
    }

    fn record_of(&self, at: usize) -> usize {
        let i = self.records.partition_point(|&(start, _)| start <= at);
        self.records
            .get(i.wrapping_sub(1))
            .map_or(0, |&(_, line)| line)
    }
}

/// A record the decoder could not read.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Undecodable(usize, String);

fn ptylog(text: &str) -> Result<Vec<Stream>, Undecodable> {
    let (mut out, mut inp) = (Stream::new("output"), Stream::new("input"));
    for (i, line) in text.lines().enumerate() {
        let refuse = |why: &str| Undecodable(i + 1, why.to_string());
        let mut fields = line.split_ascii_whitespace();
        let Some(_stamp) = fields.next() else {
            continue;
        };
        let (kind, payload) = (fields.next(), fields.next());
        if fields.next().is_some() {
            return Err(refuse("a record with more than one payload"));
        }
        let stream = match (kind, payload) {
            (Some("O"), _) => &mut out,
            (Some("I"), _) => &mut inp,
            (Some(_), None) => continue,
            (Some(_), Some(_)) => return Err(refuse("a record of a kind it does not know")),
            (None, _) => return Err(refuse("a line that is not a record")),
        };
        let bytes =
            unhex(payload.unwrap_or("")).ok_or_else(|| refuse("a payload that is not hex"))?;
        stream.push(i + 1, &bytes);
    }
    Ok(vec![out, inp])
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[hi, lo]| Some(digit(hi)? << 4 | digit(lo)?))
        .collect()
}

fn reads(text: &str) -> Result<Vec<Stream>, Undecodable> {
    let mut s = Stream::new("reads");
    for (i, line) in text.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let mut bytes = Vec::with_capacity(line.len());
        let mut it = line.bytes();
        while let Some(b) = it.next() {
            if b != b'\\' {
                bytes.push(b);
                continue;
            }
            bytes.push(match it.next() {
                Some(b'e') => 0x1b,
                Some(b'a') => 0x07,
                Some(b'b') => 0x08,
                Some(b'r') => b'\r',
                Some(b'n') => b'\n',
                Some(b'\\') => b'\\',
                _ => {
                    return Err(Undecodable(
                        i + 1,
                        "an escape the format does not define".into(),
                    ));
                }
            });
        }
        s.push(i + 1, &bytes);
    }
    Ok(vec![s])
}

/// How [`View::printable`] reads the controls it drops.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Escape sequences gone, every other control a line break.
    Joined,
    /// As [`Read::Joined`], but a CSI breaks the line too.
    Spaced,
    /// As [`Read::Joined`], but a Backspace or a DEL takes back the byte before it on its line.
    Edited,
}

struct View {
    bytes: Vec<u8>,
    from: Vec<usize>,
}

impl View {
    fn raw(stream: &[u8]) -> Self {
        Self {
            bytes: stream.to_vec(),
            from: (0..stream.len()).collect(),
        }
    }

    fn put(&mut self, b: u8, at: usize) {
        self.bytes.push(b);
        self.from.push(at);
    }

    fn split(&mut self, at: usize) {
        if self.bytes.last().is_some_and(|&b| b != b'\n') {
            self.put(b'\n', at);
        }
    }

    fn take_back(&mut self) {
        if self.bytes.last().is_some_and(|&b| b != b'\n') {
            self.bytes.pop();
            self.from.pop();
        }
    }

    /// What the stream prints ([`Read::Spaced`], [`Read::Edited`]).
    fn printable(stream: &[u8], read: Read) -> Self {
        const ESC: u8 = 0x1b;
        let mut v = Self {
            bytes: Vec::with_capacity(stream.len()),
            from: Vec::with_capacity(stream.len()),
        };
        let n = stream.len();
        let mut i = 0;
        while i < n {
            let b = stream[i];
            if b != ESC {
                if read == Read::Edited && (b == 0x08 || b == 0x7f) {
                    v.take_back();
                } else if b < 0x20 || b == 0x7f {
                    v.split(i);
                } else {
                    v.put(b, i);
                }
                i += 1;
                continue;
            }
            let mut j = i + 1;
            match stream.get(j) {
                Some(b'[') => {
                    j += 1;
                    while j < n && (0x20..=0x3f).contains(&stream[j]) {
                        j += 1;
                    }
                    if j < n && (0x40..=0x7e).contains(&stream[j]) {
                        j += 1;
                    }
                    if read == Read::Spaced {
                        v.split(i);
                    }
                }
                Some(b']' | b'P' | b'_' | b'^' | b'X') => {
                    v.split(i);
                    j += 1;
                    while j < n {
                        match stream[j] {
                            0x07 => {
                                j += 1;
                                break;
                            }
                            ESC if stream.get(j + 1) == Some(&b'\\') => {
                                j += 2;
                                break;
                            }
                            ESC => break,
                            c if c < 0x20 || c == 0x7f => v.split(j),
                            c => v.put(c, j),
                        }
                        j += 1;
                    }
                    v.split(j.min(n.saturating_sub(1)));
                }
                Some(_) => {
                    while j < n && (0x20..=0x2f).contains(&stream[j]) {
                        j += 1;
                    }
                    j = (j + 1).min(n);
                }
                None => {}
            }
            i = j.max(i + 1);
        }
        v
    }

    fn lines(&self) -> impl Iterator<Item = (usize, &[u8])> {
        let mut start = 0;
        self.bytes.split(|&b| b == b'\n').map(move |line| {
            let at = start;
            start += line.len() + 1;
            (at, line)
        })
    }
}

/// One pattern a capture is held to.
pub(super) struct Row {
    /// Where it comes from, as a finding names it.
    pub(super) source: String,
    /// The pattern as grep runs it, on one line.
    pub(super) line: Regex,
    /// The same with `(?m)`: a whole view without a match of it holds no line
    /// with one.
    any: Regex,
}

const VIEW_STEPS: u64 = 1 << 32;

impl Row {
    pub(super) fn new(source: String, pattern: &str) -> Result<Self, String> {
        let line = Regex::new(pattern).map_err(|e| e.to_string())?;
        let any = RegexBuilder::new(&format!("(?m){pattern}"))
            .step_limit(VIEW_STEPS)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { source, line, any })
    }
}

/// Compiles the non-comment pattern rows of `text`, recording compile errors into `out`.
pub(super) fn file_rows(
    text: &str,
    label: &str,
    finding_path: &str,
    rule: &'static str,
    out: &mut Vec<Finding>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let pattern = raw.trim();
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }
        match Row::new(format!("{label}:{}", i + 1), pattern) {
            Ok(row) => rows.push(row),
            Err(why) => out.push(Finding {
                rule,
                path: finding_path.to_string(),
                line: i + 1,
                detail: format!(
                    "this row does not compile in aterm-regex ({why}), so nothing is held to it"
                ),
            }),
        }
    }
    rows
}

/// [`CAPTURE_ROWS`], compiled.
pub(super) fn capture_rows() -> Vec<Row> {
    CAPTURE_ROWS
        .iter()
        .map(|(what, pattern)| {
            Row::new(format!("the capture row for {what}"), pattern)
                .unwrap_or_else(|e| panic!("capture row {pattern:?} compiles: {e}"))
        })
        .collect()
}

fn baseline_path() -> Option<PathBuf> {
    std::env::var_os("PUBLICATION_ENGINE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("publication")))
        .map(|engine| engine.join(BASELINE))
}

/// The engine's baseline `(where, text)`, or why the rules judge without it.
pub(super) fn baseline() -> Result<(String, String), String> {
    let path = baseline_path().ok_or("neither PUBLICATION_ENGINE nor HOME names the engine")?;
    let shown = path.display().to_string();
    std::fs::read_to_string(&path)
        .map(|text| (shown.clone(), text))
        .map_err(|e| format!("{shown}: {e}"))
}

/// Returns census notes listing any absent baseline or extras files.
pub(super) fn notes(root: &Path) -> Vec<String> {
    let mut without = Vec::new();
    if let Err(why) = baseline() {
        without.push(format!("the publication engine's baseline ({why})"));
    }
    if !root.join(EXTRA).exists() {
        without.push(format!("{EXTRA} (absent)"));
    }
    if without.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "capture-privacy and export-content judged without {}; the capture rows still held",
        without.join(" or ")
    )]
}

fn capture_privacy(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    held_to(tree, baseline(), out);
}

fn held_to(tree: &Tree<'_>, baseline: Result<(String, String), String>, out: &mut Vec<Finding>) {
    let captures: Vec<&String> = tree
        .corpus
        .paths
        .iter()
        .filter(|rel| format_of(rel).is_some())
        .collect();
    if captures.is_empty() {
        return;
    }
    let extra = match std::fs::read_to_string(tree.root.join(EXTRA)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            out.push(Finding {
                rule: CAPTURE_PRIVACY.id,
                path: EXTRA.to_string(),
                line: 0,
                detail: format!("unreadable ({e}), so no capture is held to the export's rows"),
            });
            String::new()
        }
    };
    let mut rows = file_rows(&extra, EXTRA, EXTRA, CAPTURE_PRIVACY.id, out);
    if let Ok((path, text)) = baseline {
        rows.extend(file_rows(
            &text,
            "the engine's baseline",
            &path,
            CAPTURE_PRIVACY.id,
            out,
        ));
    }
    rows.extend(capture_rows());
    for rel in captures {
        match std::fs::read(tree.root.join(rel)) {
            Ok(bytes) => judge(rel, &String::from_utf8_lossy(&bytes), &rows, out),
            Err(e) => out.push(Finding {
                rule: CAPTURE_PRIVACY.id,
                path: rel.clone(),
                line: 0,
                detail: format!("unreadable: {e}"),
            }),
        }
    }
}

fn format_of(rel: &str) -> Option<&'static Format> {
    let ext = rel.rsplit('/').next()?.rsplit_once('.')?.1;
    FORMATS.iter().find(|f| f.ext == ext)
}

/// Checks one capture (`rel`, `text`) against `rows` and appends any findings to `out`.
pub(super) fn judge(rel: &str, text: &str, rows: &[Row], out: &mut Vec<Finding>) {
    let Some(format) = format_of(rel) else {
        return;
    };
    let streams = match (format.decode)(text) {
        Ok(streams) => streams,
        Err(Undecodable(line, why)) => {
            out.push(Finding {
                rule: CAPTURE_PRIVACY.id,
                path: rel.to_string(),
                line,
                detail: format!("{why}: the guard cannot decode it, so nothing judges its bytes"),
            });
            return;
        }
    };
    for stream in &streams {
        let views = [
            View::raw(&stream.bytes),
            View::printable(&stream.bytes, Read::Joined),
            View::printable(&stream.bytes, Read::Spaced),
            View::printable(&stream.bytes, Read::Edited),
        ];
        let mut hits: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
        let mut exhausted: BTreeSet<usize> = BTreeSet::new();
        for view in &views {
            let whole = String::from_utf8_lossy(&view.bytes);
            for (k, row) in rows.iter().enumerate() {
                if matches!(row.any.try_is_match(&whole), Ok(false)) {
                    continue;
                }
                for (start, line) in view.lines() {
                    let text = String::from_utf8_lossy(line);
                    match row.line.try_find(&text) {
                        Ok(Some(m)) => {
                            let at = match text {
                                Cow::Borrowed(_) => start + m.start(),
                                Cow::Owned(_) => start,
                            };
                            let from = view.from.get(at).copied().unwrap_or(0);
                            hits.entry(k).or_default().insert(stream.record_of(from));
                        }
                        Ok(None) => {}
                        Err(_) => {
                            exhausted.insert(k);
                        }
                    }
                }
            }
        }
        for (k, records) in hits {
            let first = records.first().copied().unwrap_or(0);
            let n = records.len();
            out.push(Finding {
                rule: CAPTURE_PRIVACY.id,
                path: rel.to_string(),
                line: first,
                detail: format!(
                    "its decoded {} matches {} ({n} record{}); scrub the capture with neutral \
                     text of the same width",
                    stream.name,
                    rows[k].source,
                    if n == 1 { "" } else { "s" }
                ),
            });
        }
        for k in exhausted {
            out.push(Finding {
                rule: CAPTURE_PRIVACY.id,
                path: rel.to_string(),
                line: 0,
                detail: format!(
                    "a line of its decoded {} exhausted the search for {}, so it is not judged",
                    stream.name, rows[k].source
                ),
            });
        }
    }
}

/// Encodes `(kind, bytes)` records into `ptylog` text for tests.
#[cfg(test)]
pub(super) fn ptylog_text(records: &[(&str, &[u8])]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for (i, (kind, bytes)) in records.iter().enumerate() {
        let _ = write!(s, "{} {kind} ", 1000 * (i + 1));
        for b in *bytes {
            let _ = write!(s, "{b:02x}");
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
#[path = "captures_tests.rs"]
mod tests;
