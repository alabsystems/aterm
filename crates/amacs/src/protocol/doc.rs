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

/// Which document a request names: the `doc=` id a row or `OK edit` gave, or a `file://` URI in any
/// spelling of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocRef {
    Id(u64),
    Uri(String),
}

impl DocRef {
    /// `<d>`, `doc=<d>`, `<file-uri>` or `uri=<file-uri>`: a field copied from a row reads as the
    /// bare value.
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

/// One `doc` request: the arguments after the verb.
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
                if let DocRef::Uri(uri) = doc {
                    file_uri_token(uri)?;
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
    /// How a file read now stands against the version a buffer was read or last saved as, by
    /// content: [`Self::Missing`] when no file is there, [`Self::Same`] when that version was a
    /// file too and the length and the content agree (`disk_same`, contracted), and
    /// [`Self::Changed`] otherwise.
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
    /// Whether a file read now holds the version a buffer was read or last
    /// saved as: the file is there, the version was a file too, and both its
    /// length and its content agree. `disk=same` is exactly this; a length
    /// that agrees over other bytes is still a change.
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

/// A position in a text: a 1-based line by the anchor rules and a 1-based column in characters,
/// written `<L>:<C>`.
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

/// The column of byte `end` on a line starting at byte `start`: one past the
/// characters between them.
fn column(text: &str, start: usize, end: usize) -> usize {
    let mut end = end.max(start);
    while end > start && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(start..end)
        .map_or(0, |between| between.chars().count())
        .saturating_add(1)
}

/// A caret in one view: `<view>:<L>:<C>`.
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

/// A selection in one view, start to end: `<view>:<L>:<C>-<L>:<C>`.
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

/// One `doc ls` row: `doc=<d> uri=<canonical-uri> seq=<s> dirty=0|1 disk=<state> views=<v,…>
/// ticket=<t,…|-> cursor=<view>:<L>:<C>|- sel=<view>:<L>:<C>-<L>:<C>|-`.
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

/// `a,b,c`, or `-` for none.
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

/// `a,b,c` of decimal ids, or `-` for none. Not generic over the item's
/// parser: a call through an `impl Fn` is an instance the Trust lane cannot
/// discharge (SPECS.md §4).
fn parse_list(text: &str, field: &'static str) -> Result<Vec<u64>, DocError> {
    if text == "-" {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|word| digits(word).ok_or_else(|| bad(field, text)))
        .collect()
}

/// `field=<text>` is not what the field takes.
fn bad(field: &'static str, text: &str) -> DocError {
    DocError::BadField(field, text.to_string())
}

/// A decimal `u64`, digits only.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A decimal `u64` field, digits only.
fn number(text: &str, field: &'static str) -> Result<u64, DocError> {
    digits(text).ok_or_else(|| DocError::BadField(field, text.to_string()))
}

/// `uri` is one `file://` wire token.
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
mod tests {
    use super::*;

    #[test]
    fn requests_parse_what_they_format() {
        let forms = [
            ("ls", DocRequest::Ls { uri: None }),
            (
                "ls uri=file:///tmp/a%20b.txt",
                DocRequest::Ls {
                    uri: Some("file:///tmp/a%20b.txt".to_string()),
                },
            ),
            (
                "read 3",
                DocRequest::Read {
                    doc: DocRef::Id(3),
                    lines: LineRange::ALL,
                },
            ),
            (
                "read file:///x lines=2-9",
                DocRequest::Read {
                    doc: DocRef::Uri("file:///x".to_string()),
                    lines: LineRange {
                        first: 2,
                        last: Some(9),
                    },
                },
            ),
            (
                "read 4 lines=7-",
                DocRequest::Read {
                    doc: DocRef::Id(4),
                    lines: LineRange {
                        first: 7,
                        last: None,
                    },
                },
            ),
        ];
        for (line, request) in forms {
            assert_eq!(DocRequest::parse(line).as_ref(), Ok(&request), "{line}");
            assert_eq!(request.format().as_deref(), Ok(line), "{line}");
        }
        assert_eq!(
            DocRequest::parse("read doc=3 lines=5"),
            Ok(DocRequest::Read {
                doc: DocRef::Id(3),
                lines: LineRange {
                    first: 5,
                    last: Some(5)
                },
            })
        );
        assert_eq!(
            DocRequest::parse("read uri=file:///x"),
            DocRequest::parse("read file:///x")
        );
        assert_eq!(
            DocRequest::parse("apply 3 base=7 len=12"),
            Ok(DocRequest::Later(LaterForm::Apply))
        );
        assert_eq!(
            DocRequest::parse("save 3"),
            Ok(DocRequest::Later(LaterForm::Save))
        );
        assert_eq!(
            DocRequest::parse("log 3 since=2"),
            Ok(DocRequest::Later(LaterForm::Log))
        );
    }

    #[test]
    fn malformed_requests_say_why() {
        let refused = [
            ("", "usage: doc ls"),
            ("frob", "usage: doc ls"),
            ("read", "usage: doc ls"),
            ("read 0", "bad doc=0"),
            ("read doc=file:///x", "bad doc=file:///x"),
            ("read uri=7", "bad uri=7"),
            ("read /tmp/x", "bad doc=/tmp/x"),
            ("read 3 lines=0-2", "bad lines=0-2"),
            ("read 3 lines=5-2", "bad lines=5-2"),
            ("read 3 lines=1 lines=2", "unknown option lines=2"),
            ("read 3 near=2", "unknown option near=2"),
            ("ls uri=/tmp/x", "bad uri=/tmp/x"),
            ("ls 3", "unknown option 3"),
            (
                "ls uri=file:///a uri=file:///b",
                "unknown option uri=file:///b",
            ),
        ];
        for (line, why) in refused {
            let error = DocRequest::parse(line).expect_err(line).to_string();
            assert!(error.starts_with(why), "{line}: {error}");
        }
    }

    #[test]
    fn rows_parse_what_they_format() {
        let row = DocRow {
            doc: 3,
            uri: "file:///private/tmp/COMMIT_EDITMSG".to_string(),
            seq: 12,
            dirty: true,
            disk: DiskState::Changed,
            views: vec![7, 9],
            tickets: vec![Ticket(4_294_967_297)],
            cursor: Some(ViewPoint {
                view: 7,
                at: Point { line: 2, col: 11 },
            }),
            sel: Some(ViewSpan {
                view: 7,
                from: Point { line: 2, col: 6 },
                to: Point { line: 2, col: 11 },
            }),
        };
        let line = row.format().unwrap();
        assert_eq!(
            line,
            "doc=3 uri=file:///private/tmp/COMMIT_EDITMSG seq=12 dirty=1 disk=changed \
             views=7,9 ticket=4294967297 cursor=7:2:11 sel=7:2:6-2:11"
        );
        assert_eq!(DocRow::parse(&format!("{line}\n")), Ok(row.clone()));
        let bare = DocRow {
            dirty: false,
            disk: DiskState::Same,
            views: vec![7],
            tickets: Vec::new(),
            cursor: None,
            sel: None,
            ..row
        };
        let line = bare.format().unwrap();
        assert!(line.ends_with("views=7 ticket=- cursor=- sel=-"), "{line}");
        assert_eq!(DocRow::parse(&line), Ok(bare));
        for broken in [
            "doc=3",
            "uri=file:///x doc=3 seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=-",
            "doc=3 uri=file:///x seq=1 dirty=2 disk=same views=1 ticket=- cursor=- sel=-",
            "doc=3 uri=file:///x seq=1 dirty=0 disk=lost views=1 ticket=- cursor=- sel=-",
            "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1, ticket=- cursor=- sel=-",
            "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=1:0:1 sel=-",
            "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=1:1:1",
            "doc=3 uri=file:///x seq=1 dirty=0 disk=same views=1 ticket=- cursor=- sel=- x=1",
        ] {
            assert!(DocRow::parse(broken).is_err(), "{broken}");
        }
        let spaced = DocRow {
            uri: "file:///a b".to_string(),
            ..DocRow::parse(&line).unwrap()
        };
        assert_eq!(spaced.format(), Err(DocError::Whitespace("uri")));
    }

    /// `disk=`'s table over every input: a file that is gone is missing
    /// whatever else holds, and a file is the same only when its version
    /// was a file and both length and content agree. `disk_same`'s contract
    /// proves the second; this checks the arm order `by_content` reads them in.
    #[test]
    fn disk_state_by_content_is_its_table() {
        for bits in 0..16_u8 {
            let [now, base, len, content] = [8, 4, 2, 1].map(|bit| bits & bit != 0);
            let expected = match (now, base && len && content) {
                (false, _) => DiskState::Missing,
                (true, true) => DiskState::Same,
                (true, false) => DiskState::Changed,
            };
            assert_eq!(
                DiskState::by_content(now, base, len, content),
                expected,
                "now={now} base={base} len={len} content={content}"
            );
        }
    }

    #[test]
    fn points_follow_the_anchor_rules() {
        let text = "ab\r\nc\u{e9}d\n\ne";
        let at = |offset| Point::at(text, offset).to_string();
        assert_eq!([at(0), at(1), at(2), at(3)], ["1:1", "1:2", "1:3", "1:3"]);
        assert_eq!(
            [at(4), at(5), at(6), at(7), at(8)],
            ["2:1", "2:2", "2:2", "2:3", "2:4"]
        );
        assert_eq!([at(9), at(10), at(11)], ["3:1", "4:1", "4:2"]);
        assert_eq!(at(99), "4:2", "past the end is the end");
        assert_eq!(Point::at("a\nb\n", 4).to_string(), "3:1");
        assert_eq!(Point::at("", 0), Point::START);
        let marked = "\u{feff}x\n";
        assert_eq!(
            [0, 1, 3, 4, 5].map(|offset| Point::at(marked, offset).to_string()),
            ["1:1", "1:1", "1:1", "1:2", "2:1"]
        );
        let lines = Lines::of(text);
        for offset in 0..text.len() {
            let point = Point::at(text, offset);
            if point.line <= lines.count() {
                let span = lines.span(point.line).unwrap();
                assert!(span.start <= offset && offset < span.end.max(span.start + 1));
            }
        }
    }
}
