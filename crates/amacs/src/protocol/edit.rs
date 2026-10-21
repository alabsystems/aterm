// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `edit` request and its ticket replies (design §6.2-§6.5; slice N1,
//! with split placements in N6).
//!
//! A client in an aterm tab asks the window hosting the tab for a fresh
//! Editor view of a file, then waits on the edit ticket until the person
//! finishes, aborts or closes it. This module is the wire form both ends
//! share. aterm-gui serves it (`@<sid> edit`, `await edit`), and any control
//! client can send it, `aterm ctl` among them; the `amacs FILE` client that
//! will send it is planned (design §6).
//!
//! The control wire splits on whitespace and never quotes, so every field is
//! one whitespace-free token: a formatter refuses a field that holds
//! whitespace rather than send a line the other end would read differently.
//! Each `parse` takes what its `format` writes, and `parse(format(x)) == x`.
//!
//! - `edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0]` →
//!   `OK edit ticket=<t> view=<v> doc=<d> uri=<canonical-uri>`
//! - `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]` →
//!   `OK edit done result=finished|closed|aborted saved=0|1 seq=<n>`

/// One `edit` request's name on the server, from admission to its outcome.
/// Opaque to the client: it is quoted back on `await edit ticket=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ticket(pub u64);

impl std::fmt::Display for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for Ticket {
    type Err = EditError;

    fn from_str(text: &str) -> Result<Self, EditError> {
        number(text, "ticket").map(Ticket)
    }
}

/// Why a line is not one of this module's forms. Its `Display` is the text a
/// server answers after `ERR `.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// The line is not the form at all: no URI, or a missing key.
    Usage(&'static str),
    /// `line=` is not `<L>` or `<L>:<C>` with both 1-based.
    BadLine(String),
    /// A field's value is not what the field takes.
    BadField(&'static str, String),
    /// An option the form does not have, or one given twice.
    UnknownOption(String),
    /// A field to be written holds whitespace (or is empty), so the wire
    /// would split it.
    Whitespace(&'static str),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(usage) => write!(f, "usage: {usage}"),
            Self::BadLine(value) => write!(f, "bad line={value} (want <line>[:<col>], 1-based)"),
            Self::BadField(field, value) => write!(f, "bad {field}={value}"),
            Self::UnknownOption(option) => write!(f, "unknown option {option}"),
            Self::Whitespace(field) => write!(f, "{field} is empty or holds whitespace"),
        }
    }
}

impl std::error::Error for EditError {}

/// The `edit` request's usage line.
pub const EDIT_USAGE: &str = "edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0]";

/// The `await edit` request's usage line.
pub const AWAIT_EDIT_USAGE: &str = "await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]";

/// `edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0]`: open
/// a fresh Editor view of the file beside the asking session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    /// The file, as a `file://` URI ([`super::uri::path_to_file_uri`]).
    pub uri: String,
    /// Where the caret lands: a 1-based line and, optionally, column.
    pub line: Option<(u32, Option<u32>)>,
    /// Admit a missing file as empty; the first save creates it.
    pub create: bool,
    /// The program the edit is for (`git`), shown in the view's header row:
    /// `[A-Za-z0-9._-]`, at most 32 characters.
    pub for_word: Option<String>,
    /// Whether the asker will wait on the ticket (`await edit`). `wait=0`
    /// (`amacs --no-wait`) says nobody will, so the asking session is not
    /// held while the view is open (design §6.7). On the wire only when
    /// false: a waiting request reads as it always has.
    pub wait: bool,
}

impl EditRequest {
    /// Read the request's arguments (everything after the `edit` verb).
    pub fn parse(args: &str) -> Result<Self, EditError> {
        let mut words = args.split_whitespace();
        let uri = words.next().ok_or(EditError::Usage(EDIT_USAGE))?;
        if !uri.starts_with("file://") {
            return Err(EditError::BadField("uri", uri.to_string()));
        }
        let mut request = Self {
            uri: uri.to_string(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
        };
        let mut seen = Vec::new();
        for word in words {
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| EditError::UnknownOption(word.to_string()))?;
            if seen.contains(&key) {
                return Err(EditError::UnknownOption(word.to_string()));
            }
            seen.push(key);
            match key {
                "line" => request.line = Some(parse_line(value)?),
                "create" => {
                    request.create = match value {
                        "1" => true,
                        "0" => false,
                        _ => return Err(EditError::BadField("create", value.to_string())),
                    };
                }
                "for" => {
                    if !is_for_word(value) {
                        return Err(EditError::BadField("for", value.to_string()));
                    }
                    request.for_word = Some(value.to_string());
                }
                "wait" => {
                    request.wait = match value {
                        "1" => true,
                        "0" => false,
                        _ => return Err(EditError::BadField("wait", value.to_string())),
                    };
                }
                _ => return Err(EditError::UnknownOption(word.to_string())),
            }
        }
        Ok(request)
    }

    /// The request's arguments, as [`Self::parse`] reads them.
    pub fn format(&self) -> Result<String, EditError> {
        token("uri", &self.uri)?;
        if !self.uri.starts_with("file://") {
            return Err(EditError::BadField("uri", self.uri.clone()));
        }
        let mut args = self.uri.clone();
        if let Some((line, col)) = self.line {
            if line == 0 || col == Some(0) {
                return Err(EditError::BadLine(format_line(line, col)));
            }
            args.push_str(" line=");
            args.push_str(&format_line(line, col));
        }
        if self.create {
            args.push_str(" create=1");
        }
        if let Some(word) = &self.for_word {
            token("for", word)?;
            if !is_for_word(word) {
                return Err(EditError::BadField("for", word.clone()));
            }
            args.push_str(" for=");
            args.push_str(word);
        }
        if !self.wait {
            args.push_str(" wait=0");
        }
        Ok(args)
    }
}

/// `OK edit ticket=<t> view=<v> doc=<d> uri=<canonical-uri>`: the fresh view
/// is installed and the ticket is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOk {
    pub ticket: Ticket,
    /// The installed view (`inspect app/v1 view <v>`).
    pub view: u64,
    /// The document it shows; other views of the file share it.
    pub doc: u64,
    /// The file's canonical URI (symlinks resolved).
    pub uri: String,
}

impl EditOk {
    /// The reply line, without its newline.
    pub fn format(&self) -> Result<String, EditError> {
        token("uri", &self.uri)?;
        Ok(format!(
            "OK edit ticket={} view={} doc={} uri={}",
            self.ticket, self.view, self.doc, self.uri
        ))
    }

    /// Read a reply line (a trailing newline is allowed).
    pub fn parse(line: &str) -> Result<Self, EditError> {
        let fields = reply_fields(line, &["OK", "edit"], "OK edit ticket= view= doc= uri=")?;
        let [ticket, view, doc, uri] = keyed(
            &fields,
            ["ticket", "view", "doc", "uri"],
            "OK edit ticket= view= doc= uri=",
        )?;
        Ok(Self {
            ticket: ticket.parse()?,
            view: number(view, "view")?,
            doc: number(doc, "doc")?,
            uri: uri.to_string(),
        })
    }
}

/// How an edit ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditResult {
    /// The person pressed done (C-c C-c): saved, then closed.
    Finished,
    /// The view closed; the file on disk is the answer.
    Closed,
    /// The person pressed abort (C-c C-k): changes discarded, then closed.
    Aborted,
}

impl EditResult {
    /// The wire word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Closed => "closed",
            Self::Aborted => "aborted",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "finished" => Some(Self::Finished),
            "closed" => Some(Self::Closed),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

/// `OK edit done result=<r> saved=0|1 seq=<n>`: the ticket's one outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOutcome {
    pub result: EditResult,
    /// A save of the document was acknowledged durable during the edit, and
    /// the file holds the text the view last showed.
    pub saved: bool,
    /// The document's durable checkpoint sequence when the ticket resolved.
    pub seq: u64,
}

impl EditOutcome {
    /// The reply line, without its newline.
    pub fn format(&self) -> String {
        format!(
            "OK edit done result={} saved={} seq={}",
            self.result.as_str(),
            u8::from(self.saved),
            self.seq
        )
    }

    /// Read a reply line (a trailing newline is allowed).
    pub fn parse(line: &str) -> Result<Self, EditError> {
        const FORM: &str = "OK edit done result= saved= seq=";
        let fields = reply_fields(line, &["OK", "edit", "done"], FORM)?;
        let [result, saved, seq] = keyed(&fields, ["result", "saved", "seq"], FORM)?;
        Ok(Self {
            result: EditResult::from_word(result)
                .ok_or_else(|| EditError::BadField("result", result.to_string()))?,
            saved: match saved {
                "1" => true,
                "0" => false,
                _ => return Err(EditError::BadField("saved", saved.to_string())),
            },
            seq: number(seq, "seq")?,
        })
    }
}

/// What an `await edit` waits on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AwaitKey {
    /// One ticket's outcome.
    Ticket(Ticket),
    /// The file: done when no view an `edit` opened of it remains (the form a
    /// client re-arms with after a seamless update replaced its server).
    Uri(String),
}

/// `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwaitEdit {
    pub key: AwaitKey,
    /// The wait's bound; the server holds it to the `await` family's (600 s
    /// at most, 30 s when absent).
    pub timeout_ms: Option<u64>,
}

impl AwaitEdit {
    /// Read the `await` verb's arguments (`edit ticket=…`).
    pub fn parse(args: &str) -> Result<Self, EditError> {
        let mut words = args.split_whitespace();
        if words.next() != Some("edit") {
            return Err(EditError::Usage(AWAIT_EDIT_USAGE));
        }
        let (mut key, mut timeout_ms) = (None, None);
        for word in words {
            let (name, value) = word
                .split_once('=')
                .ok_or_else(|| EditError::UnknownOption(word.to_string()))?;
            match name {
                "ticket" if key.is_none() => key = Some(AwaitKey::Ticket(value.parse()?)),
                "uri" if key.is_none() => {
                    if !value.starts_with("file://") {
                        return Err(EditError::BadField("uri", value.to_string()));
                    }
                    key = Some(AwaitKey::Uri(value.to_string()));
                }
                "timeout" if timeout_ms.is_none() => {
                    timeout_ms = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| EditError::BadField("timeout", value.to_string()))?,
                    );
                }
                _ => return Err(EditError::UnknownOption(word.to_string())),
            }
        }
        Ok(Self {
            key: key.ok_or(EditError::Usage(AWAIT_EDIT_USAGE))?,
            timeout_ms,
        })
    }

    /// The `await` verb's arguments, as [`Self::parse`] reads them.
    pub fn format(&self) -> Result<String, EditError> {
        let mut args = match &self.key {
            AwaitKey::Ticket(ticket) => format!("edit ticket={ticket}"),
            AwaitKey::Uri(uri) => {
                token("uri", uri)?;
                if !uri.starts_with("file://") {
                    return Err(EditError::BadField("uri", uri.clone()));
                }
                format!("edit uri={uri}")
            }
        };
        if let Some(ms) = self.timeout_ms {
            args.push_str(&format!(" timeout={ms}"));
        }
        Ok(args)
    }
}

/// `<L>` or `<L>:<C>`, both 1-based.
fn parse_line(value: &str) -> Result<(u32, Option<u32>), EditError> {
    let bad = || EditError::BadLine(value.to_string());
    let (line, col) = match value.split_once(':') {
        Some((line, col)) => (line, Some(col)),
        None => (value, None),
    };
    let positive = |text: &str| {
        text.parse::<u32>()
            .ok()
            .filter(|n| *n > 0 && text.bytes().all(|b| b.is_ascii_digit()))
    };
    let line = positive(line).ok_or_else(bad)?;
    let col = match col {
        Some(col) => Some(positive(col).ok_or_else(bad)?),
        None => None,
    };
    Ok((line, col))
}

fn format_line(line: u32, col: Option<u32>) -> String {
    match col {
        Some(col) => format!("{line}:{col}"),
        None => line.to_string(),
    }
}

/// `[A-Za-z0-9._-]{1,32}`: what a client may name the program it edits for.
fn is_for_word(word: &str) -> bool {
    (1..=32).contains(&word.len()) && word.bytes().all(for_word_byte)
}

trust_contract_fn! {
    /// Whether `byte` may appear in a `for=` word: an ASCII letter or digit,
    /// `.`, `_` or `-`. No space and no `=` is among them, so the word stays
    /// one `key=value` wire field.
    #[expect(
        clippy::manual_range_contains,
        reason = "Trust kernel-imports a bool tree of comparisons, not a call to `RangeInclusive::contains` (E6 refuses calls)"
    )]
    fn for_word_byte(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 46 || (byte == 95 || byte == 45)))))
        by for_word_byte_is_exactly_the_word_set]
    {
        (byte >= b'0' && byte <= b'9')
            || (byte >= b'A' && byte <= b'Z')
            || (byte >= b'a' && byte <= b'z')
            || byte == b'.'
            || byte == b'_'
            || byte == b'-'
    }
}

// Spec `for-word-byte-set` (SPECS.md), kernel-checked against the compiled
// body: the set is the clause, byte for byte.
verify_island! {
    clean {
        theorem for_word_byte_is_exactly_the_word_set (byte : UInt8) :
            trust_import_amacs__protocol__edit__for_word_byte byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 46) || (decide (byte = 95) || decide (byte = 45)))))) := by
          simp only [trust_import_amacs__protocol__edit__for_word_byte]
    }
}

/// `value` is one non-empty, whitespace-free wire token.
fn token(field: &'static str, value: &str) -> Result<(), EditError> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(EditError::Whitespace(field));
    }
    Ok(())
}

/// A decimal `u64` field, digits only.
fn number(text: &str, field: &'static str) -> Result<u64, EditError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EditError::BadField(field, text.to_string()));
    }
    text.parse()
        .map_err(|_| EditError::BadField(field, text.to_string()))
}

/// The `key=value` words of a reply line after its fixed leading words.
fn reply_fields<'a>(
    line: &'a str,
    leading: &[&str],
    form: &'static str,
) -> Result<Vec<&'a str>, EditError> {
    let mut words = line.split_whitespace();
    for want in leading {
        if words.next() != Some(*want) {
            return Err(EditError::Usage(form));
        }
    }
    Ok(words.collect())
}

/// Exactly the keys `keys`, in order, each once: their values.
fn keyed<'a, const N: usize>(
    fields: &[&'a str],
    keys: [&'static str; N],
    form: &'static str,
) -> Result<[&'a str; N], EditError> {
    if fields.len() != N {
        return Err(EditError::Usage(form));
    }
    let mut values = [""; N];
    for (slot, (field, key)) in values.iter_mut().zip(fields.iter().zip(keys)) {
        let value = field
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or(EditError::Usage(form))?;
        if value.is_empty() {
            return Err(EditError::BadField(key, String::new()));
        }
        *slot = value;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_word_byte_is_the_set_it_replaced() {
        for byte in u8::MIN..=u8::MAX {
            let before = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
            assert_eq!(for_word_byte(byte), before, "byte {byte:#04x}");
        }
    }

    const URI: &str = "file:///private/tmp/x%20y/COMMIT_EDITMSG";

    #[test]
    fn edit_requests_round_trip() {
        for request in [
            EditRequest {
                uri: URI.to_string(),
                line: None,
                create: false,
                for_word: None,
                wait: true,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((3, None)),
                create: false,
                for_word: Some("git".to_string()),
                wait: true,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((12, Some(7))),
                create: true,
                for_word: Some("claude-code_2.1".to_string()),
                wait: true,
            },
            EditRequest {
                uri: URI.to_string(),
                line: None,
                create: false,
                for_word: Some("git".to_string()),
                wait: false,
            },
        ] {
            let args = request.format().unwrap();
            assert_eq!(EditRequest::parse(&args).unwrap(), request, "{args}");
            assert_eq!(
                args.contains("wait="),
                !request.wait,
                "only `wait=0` is written: {args}"
            );
        }
        assert!(
            EditRequest::parse(&format!("{URI} wait=1")).unwrap().wait,
            "`wait=1` is the default spelled out"
        );
        assert_eq!(
            EditRequest::parse(&format!("{URI}   for=git line=2:1 ")).unwrap(),
            EditRequest {
                uri: URI.to_string(),
                line: Some((2, Some(1))),
                create: false,
                for_word: Some("git".to_string()),
                wait: true,
            },
            "options in any order, any spacing"
        );
    }

    #[test]
    fn edit_requests_refuse_a_bad_line() {
        for line in [
            "0",
            "3:0",
            "abc",
            "3:",
            ":4",
            "-1",
            "+3",
            "3:4:5",
            "99999999999",
            "",
        ] {
            assert_eq!(
                EditRequest::parse(&format!("{URI} line={line}")),
                Err(EditError::BadLine(line.to_string())),
                "line={line}"
            );
        }
        let zero = EditRequest {
            uri: URI.to_string(),
            line: Some((0, None)),
            create: false,
            for_word: None,
            wait: true,
        };
        assert!(matches!(zero.format(), Err(EditError::BadLine(_))));
    }

    #[test]
    fn edit_requests_refuse_what_the_form_does_not_have() {
        assert_eq!(EditRequest::parse("  "), Err(EditError::Usage(EDIT_USAGE)));
        assert!(matches!(
            EditRequest::parse("/tmp/x"),
            Err(EditError::BadField("uri", _))
        ));
        for tail in [
            "place=split",
            "line=1 line=2",
            "create=yes",
            "wait=no",
            "wait=0 wait=0",
            "for=a/b",
            "bare",
        ] {
            assert!(
                EditRequest::parse(&format!("{URI} {tail}")).is_err(),
                "{tail}"
            );
        }
        let long = "x".repeat(33);
        assert!(EditRequest::parse(&format!("{URI} for={long}")).is_err());
    }

    #[test]
    fn no_field_is_written_with_whitespace() {
        let spaced = EditRequest {
            uri: "file:///tmp/a b".to_string(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
        };
        assert_eq!(spaced.format(), Err(EditError::Whitespace("uri")));
        let word = EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: Some("git commit".to_string()),
            wait: true,
        };
        assert_eq!(word.format(), Err(EditError::Whitespace("for")));
        let ok = EditOk {
            ticket: Ticket(1),
            view: 2,
            doc: 3,
            uri: "file:///tmp/a\tb".to_string(),
        };
        assert_eq!(ok.format(), Err(EditError::Whitespace("uri")));
        let wait = AwaitEdit {
            key: AwaitKey::Uri("file:///tmp/a b".to_string()),
            timeout_ms: None,
        };
        assert_eq!(wait.format(), Err(EditError::Whitespace("uri")));
        let empty = EditOk {
            uri: String::new(),
            ..ok
        };
        assert_eq!(empty.format(), Err(EditError::Whitespace("uri")));
    }

    #[test]
    fn edit_replies_round_trip() {
        let ok = EditOk {
            ticket: Ticket(18_446_744_073_709_551_615),
            view: 41,
            doc: 7,
            uri: URI.to_string(),
        };
        let line = ok.format().unwrap();
        assert_eq!(
            line,
            format!("OK edit ticket=18446744073709551615 view=41 doc=7 uri={URI}")
        );
        assert_eq!(EditOk::parse(&format!("{line}\n")).unwrap(), ok);
        assert!(EditOk::parse("OK edit ticket=1 view=2 doc=3").is_err());
        assert!(EditOk::parse("OK edit view=2 ticket=1 doc=3 uri=file:///x").is_err());
        assert!(EditOk::parse("ERR refused missing").is_err());

        for (result, saved, seq) in [
            (EditResult::Closed, true, 9),
            (EditResult::Finished, true, 0),
            (EditResult::Aborted, false, 3),
        ] {
            let outcome = EditOutcome { result, saved, seq };
            let line = outcome.format();
            assert_eq!(EditOutcome::parse(&line).unwrap(), outcome, "{line}");
        }
        assert_eq!(
            EditOutcome {
                result: EditResult::Closed,
                saved: true,
                seq: 5
            }
            .format(),
            "OK edit done result=closed saved=1 seq=5"
        );
        assert!(EditOutcome::parse("OK edit done result=closed saved=2 seq=5").is_err());
        assert!(EditOutcome::parse("OK edit done result=kept saved=1 seq=5").is_err());
        assert!(EditOutcome::parse("OK timeout").is_err());
    }

    #[test]
    fn await_edit_round_trips() {
        for wait in [
            AwaitEdit {
                key: AwaitKey::Ticket(Ticket(77)),
                timeout_ms: None,
            },
            AwaitEdit {
                key: AwaitKey::Uri(URI.to_string()),
                timeout_ms: Some(600_000),
            },
        ] {
            let args = wait.format().unwrap();
            assert_eq!(AwaitEdit::parse(&args).unwrap(), wait, "{args}");
        }
        for bad in [
            "edit",
            "inbox since=1",
            "edit ticket=x",
            "edit ticket=1 uri=file:///x",
            "edit uri=/tmp/x",
            "edit ticket=1 timeout=2s",
            "edit ticket=1 later",
        ] {
            assert!(AwaitEdit::parse(bad).is_err(), "{bad}");
        }
    }
}
