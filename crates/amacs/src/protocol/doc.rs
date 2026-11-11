// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `doc` verb's grammar: its requests and the rows `doc ls` answers.

use std::fmt;

use super::anchor::{Lines, parse_line_number};
use super::edit::Ticket;
use super::read::LineRange;

/// The `doc` request's usage line.
pub const DOC_USAGE: &str = "doc ls [uri=<file-uri>] | doc read <doc-id|file-uri> [lines=<A>-<B>]";

/// Why a line is not one of this module's forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocError {
    /// The line is not a form at all: no sub-form, or no document named.
    Usage,
    /// A field's value is not what the field takes.
    BadField(&'static str, String),
    /// `lines=` is not a range; the text says why.
    BadLines(String),
    /// An option the form does not have, or one given twice.
    UnknownOption(String),
    /// A field to be written holds whitespace (or is empty), so the wire would split it.
    Whitespace(&'static str),
}

impl fmt::Display for DocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(f, "usage: {DOC_USAGE}"),
            Self::BadField(field, value) => write!(f, "bad {field}={value}"),
            Self::BadLines(why) => f.write_str(why),
            Self::UnknownOption(option) => write!(f, "unknown option {option}"),
            Self::Whitespace(field) => write!(f, "{field} is empty or holds whitespace"),
        }
    }
}

impl std::error::Error for DocError {}

/// Which document a request names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocRef {
    Id(u64),
    Uri(String),
}

impl DocRef {
    /// `<d>`, `doc=<d>`, `<file-uri>` or `uri=<file-uri>`.
    pub fn parse(word: &str) -> Result<Self, DocError> {
        let value = word
            .strip_prefix("doc=")
            .or_else(|| word.strip_prefix("uri="))
            .unwrap_or(word);
        if value.starts_with("file://") && !word.starts_with("doc=") {
            return Ok(Self::Uri(value.to_string()));
        }
        if word.starts_with("uri=") {
            return Err(DocError::BadField("uri", value.to_string()));
        }
        match number(value, "doc")? {
            0 => Err(DocError::BadField("doc", value.to_string())),
            id => Ok(Self::Id(id)),
        }
    }
}

impl fmt::Display for DocRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Uri(uri) => f.write_str(uri),
        }
    }
}

/// A sub-form (`apply`, `save`, or `log`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaterForm {
    Apply,
    Save,
    Log,
}

impl LaterForm {
    /// The sub-form's word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Save => "save",
            Self::Log => "log",
        }
    }
}

/// One `doc` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocRequest {
    /// `ls [uri=<file-uri>]`: every document, or the one that file is.
    Ls { uri: Option<String> },
    /// `read <doc-id|file-uri> [lines=<A>-<B>]`: a document's anchored lines.
    Read { doc: DocRef, lines: LineRange },
    /// A sub-form not served yet; the rest of its line is not read.
    Later(LaterForm),
}

impl DocRequest {
    /// Read the request's arguments (everything after the `doc` verb).
    pub fn parse(args: &str) -> Result<Self, DocError> {
        let mut words = args.split_whitespace();
        match words.next() {
            Some("ls") => {
                let mut uri = None;
                for word in words {
                    match word.strip_prefix("uri=") {
                        Some(value) if uri.is_none() => {
                            if !value.starts_with("file://") {
                                return Err(DocError::BadField("uri", value.to_string()));
                            }
                            uri = Some(value.to_string());
                        }
                        _ => return Err(DocError::UnknownOption(word.to_string())),
                    }
                }
                Ok(Self::Ls { uri })
            }
            Some("read") => {
                let doc = DocRef::parse(words.next().ok_or(DocError::Usage)?)?;
                let mut lines = None;
                for word in words {
                    match word.strip_prefix("lines=") {
                        Some(value) if lines.is_none() => {
                            lines = Some(value.parse::<LineRange>().map_err(DocError::BadLines)?);
                        }
                        _ => return Err(DocError::UnknownOption(word.to_string())),
                    }
                }
                Ok(Self::Read {
                    doc,
                    lines: lines.unwrap_or(LineRange::ALL),
                })
            }
            Some("apply") => Ok(Self::Later(LaterForm::Apply)),
            Some("save") => Ok(Self::Later(LaterForm::Save)),
            Some("log") => Ok(Self::Later(LaterForm::Log)),
            _ => Err(DocError::Usage),
        }
    }

    /// The request's arguments, as [`Self::parse`] reads them.
    pub fn format(&self) -> Result<String, DocError> {
        match self {
            Self::Ls { uri: None } => Ok("ls".to_string()),
            Self::Ls { uri: Some(uri) } => {
                file_uri_token(uri)?;
                Ok(format!("ls uri={uri}"))
            }
            Self::Read { doc, lines } => {
                match doc {
                    DocRef::Id(0) => return Err(DocError::BadField("doc", "0".to_string())),
                    DocRef::Id(_) => {}
                    DocRef::Uri(uri) => {
                        file_uri_token(uri)?;
                    }
                }
                if lines.first == 0 || lines.last.is_some_and(|last| last < lines.first) {
                    return Err(DocError::BadLines(lines.to_string()));
                }
                let mut args = format!("read {doc}");
                if *lines != LineRange::ALL {
                    args.push_str(&format!(" lines={lines}"));
                }
                Ok(args)
            }
            Self::Later(form) => Ok(form.as_str().to_string()),
        }
    }
}

/// How a document's file stands against the version its buffer was read or last saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskState {
    /// The file holds that version's bytes.
    Same,
    /// The file holds other bytes: something else wrote it.
    Changed,
    /// No file is there.
    Missing,
    /// The file could not be read, or no version is known to compare with.
    Unknown,
}

impl DiskState {
    /// Compare file contents ([`Self::Missing`], [`Self::Same`], [`Self::Changed`]).
    #[must_use]
    pub fn by_content(
        now_exists: bool,
        base_exists: bool,
        same_len: bool,
        same_content: bool,
    ) -> Self {
        if !now_exists {
            Self::Missing
        } else if disk_same(now_exists, base_exists, same_len, same_content) {
            Self::Same
        } else {
            Self::Changed
        }
    }

    /// The wire word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Changed => "changed",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "same" => Some(Self::Same),
            "changed" => Some(Self::Changed),
            "missing" => Some(Self::Missing),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

trust_contract_fn! {
    /// Whether a file on disk matches the version a buffer was read or last saved as.
    fn disk_same(now_exists: bool, base_exists: bool, same_len: bool, same_content: bool) -> bool
    clauses [ensures result == (now_exists && base_exists && same_len && same_content)
        by disk_same_is_all_four]
    {
        now_exists && base_exists && same_len && same_content
    }
}

verify_island! {
    clean {
        theorem disk_same_is_all_four (now base len content : Bool) :
            trust_import_amacs__protocol__doc__disk_same now base len content =
              (((now && base) && len) && content) := by
          simp only [trust_import_amacs__protocol__doc__disk_same]
          cases now <;> cases base <;> cases len <;> cases content <;> rfl
    }
}

/// A position in a text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub line: usize,
    pub col: usize,
}

impl Point {
    /// Line 1, column 1.
    pub const START: Self = Self { line: 1, col: 1 };

    /// Where byte `offset` of `text` sits.
    #[must_use]
    pub fn at(text: &str, offset: usize) -> Self {
        let lines = Lines::of(text);
        let offset = offset.min(text.len());
        if let Some((line, span)) = lines
            .line_at(offset)
            .and_then(|line| Some((line, lines.span(line)?)))
        {
            return Self {
                line,
                col: column(text, span.start, offset.min(span.content_end)),
            };
        }
        if offset < lines.origin() {
            return Self::START;
        }
        let count = lines.count();
        match lines.span(count) {
            Some(last) if lines.open_ended() => Self {
                line: count,
                col: column(text, last.start, last.content_end),
            },
            _ => Self {
                line: count.saturating_add(1),
                col: 1,
            },
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let (line, col) = text.split_once(':')?;
        Some(Self {
            line: parse_line_number(line)?,
            col: parse_line_number(col)?,
        })
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

fn column(text: &str, start: usize, end: usize) -> usize {
    let mut end = end.max(start);
    while end > start && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(start..end)
        .map_or(0, |between| between.chars().count())
        .saturating_add(1)
}

/// A caret in one view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewPoint {
    /// The view (`inspect app/v1 view <view>`).
    pub view: u64,
    pub at: Point,
}

impl ViewPoint {
    fn parse(text: &str) -> Option<Self> {
        let (view, at) = text.split_once(':')?;
        Some(Self {
            view: digits(view)?,
            at: Point::parse(at)?,
        })
    }
}

impl fmt::Display for ViewPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.view, self.at)
    }
}

/// A selection in one view, start to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewSpan {
    pub view: u64,
    pub from: Point,
    pub to: Point,
}

impl ViewSpan {
    fn parse(text: &str) -> Option<Self> {
        let (start, to) = text.split_once('-')?;
        let start = ViewPoint::parse(start)?;
        Some(Self {
            view: start.view,
            from: start.at,
            to: Point::parse(to)?,
        })
    }
}

impl fmt::Display for ViewSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}-{}", self.view, self.from, self.to)
    }
}

/// One `doc ls` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRow {
    /// The document (`doc read <d>`).
    pub doc: u64,
    /// The file's canonical URI (symlinks resolved).
    pub uri: String,
    /// The buffer's sequence: what a read sees and a script's `base=`.
    pub seq: u64,
    /// The buffer holds changes its file does not.
    pub dirty: bool,
    pub disk: DiskState,
    /// The Editor views showing it, ascending.
    pub views: Vec<u64>,
    /// The `edit` tickets a program waits on through one of those views.
    pub tickets: Vec<Ticket>,
    /// The person's caret: in the view in front, else the first view.
    pub cursor: Option<ViewPoint>,
    /// That view's selection, when it selects anything.
    pub sel: Option<ViewSpan>,
}

impl DocRow {
    /// The row, without its newline.
    pub fn format(&self) -> Result<String, DocError> {
        file_uri_token(&self.uri)?;
        if let Some(at) = self.cursor
            && (at.at.line == 0 || at.at.col == 0)
        {
            return Err(DocError::BadField("cursor", at.to_string()));
        }
        if let Some(sel) = self.sel
            && (sel.from.line == 0 || sel.from.col == 0 || sel.to.line == 0 || sel.to.col == 0)
        {
            return Err(DocError::BadField("sel", sel.to_string()));
        }
        Ok(format!(
            "doc={} uri={} seq={} dirty={} disk={} views={} ticket={} cursor={} sel={}",
            self.doc,
            self.uri,
            self.seq,
            u8::from(self.dirty),
            self.disk.as_str(),
            list(&self.views),
            list(&self.tickets),
            self.cursor
                .map_or_else(|| "-".to_string(), |at| at.to_string()),
            self.sel
                .map_or_else(|| "-".to_string(), |sel| sel.to_string()),
        ))
    }

    /// Read a row (a trailing newline is allowed).
    pub fn parse(line: &str) -> Result<Self, DocError> {
        const KEYS: [&str; 9] = [
            "doc", "uri", "seq", "dirty", "disk", "views", "ticket", "cursor", "sel",
        ];
        let mut words = line.split_whitespace();
        let mut values = [""; 9];
        for (key, value) in KEYS.iter().zip(values.iter_mut()) {
            let word = words.next().ok_or(DocError::Usage)?;
            *value = word
                .strip_prefix(*key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or_else(|| DocError::UnknownOption(word.to_string()))?;
        }
        if let Some(extra) = words.next() {
            return Err(DocError::UnknownOption(extra.to_string()));
        }
        let [doc, uri, seq, dirty, disk, views, tickets, cursor, sel] = values;
        if !uri.starts_with("file://") {
            return Err(DocError::BadField("uri", uri.to_string()));
        }
        Ok(Self {
            doc: number(doc, "doc")?,
            uri: uri.to_string(),
            seq: number(seq, "seq")?,
            dirty: match dirty {
                "1" => true,
                "0" => false,
                _ => return Err(DocError::BadField("dirty", dirty.to_string())),
            },
            disk: DiskState::from_word(disk)
                .ok_or_else(|| DocError::BadField("disk", disk.to_string()))?,
            views: parse_list(views, "views")?,
            tickets: parse_list(tickets, "ticket")?
                .into_iter()
                .map(Ticket)
                .collect(),
            cursor: if cursor == "-" {
                None
            } else {
                Some(ViewPoint::parse(cursor).ok_or_else(|| bad("cursor", cursor))?)
            },
            sel: if sel == "-" {
                None
            } else {
                Some(ViewSpan::parse(sel).ok_or_else(|| bad("sel", sel))?)
            },
        })
    }
}

fn list<T: fmt::Display>(items: &[T]) -> String {
    if items.is_empty() {
        return "-".to_string();
    }
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_list(text: &str, field: &'static str) -> Result<Vec<u64>, DocError> {
    if text == "-" {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|word| digits(word).ok_or_else(|| bad(field, text)))
        .collect()
}

fn bad(field: &'static str, text: &str) -> DocError {
    DocError::BadField(field, text.to_string())
}

fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn number(text: &str, field: &'static str) -> Result<u64, DocError> {
    digits(text).ok_or_else(|| DocError::BadField(field, text.to_string()))
}

fn file_uri_token(uri: &str) -> Result<(), DocError> {
    if uri.is_empty() || uri.chars().any(char::is_whitespace) {
        return Err(DocError::Whitespace("uri"));
    }
    if !uri.starts_with("file://") {
        return Err(DocError::BadField("uri", uri.to_string()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "doc_tests.rs"]
mod tests;
