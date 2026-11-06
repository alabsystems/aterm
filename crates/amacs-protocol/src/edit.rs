// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `edit` request and its ticket replies.

/// One `edit` request's name on the server, from admission to its outcome.
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

/// Why a line is not one of this module's forms.
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
    /// A field to be written holds whitespace (or is empty), so the wire would split it.
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
pub const EDIT_USAGE: &str = "edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0] [ro=1] \
     [secret=<header>[,<header>…]]";

/// The `await edit` request's usage line.
pub const AWAIT_EDIT_USAGE: &str = "await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]";

/// Parsed `edit` request (`edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0]
/// [ro=1] [secret=<header>[,<header>…]]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    /// The file, as a `file://` URI ([`super::uri::path_to_file_uri`]).
    pub uri: String,
    /// Where the caret lands: a 1-based line and, optionally, column.
    pub line: Option<(u32, Option<u32>)>,
    /// Admit a missing file as empty; the first save creates it.
    pub create: bool,
    /// Program name shown in the view header (`[A-Za-z0-9._-]{1,32}`).
    pub for_word: Option<String>,
    /// Whether the asker will wait on the ticket (`await edit`).
    pub wait: bool,
    /// `ro=1`: the document opens read-only. Nothing typed, pasted or cut changes it and no save
    /// writes it; the person reads it and closes the tab (a revealed secret's grid, one-mesh
    /// design §4.8).
    pub read_only: bool,
    /// `secret=<header>[,<header>…]`: the grid columns of a private document (a revealed
    /// secret's grid) whose cells no reader of the window but the person's eyes gets, by each
    /// header's whole text; `secret=` names none. `None` when it is not given, and then a private
    /// document conceals every cell (one-mesh design §4.8, "Revealed values stay out of the
    /// accessibility tree"). Only a private document takes it.
    pub secret: Option<Vec<String>>,
}

/// The most headers a `secret=` names.
pub const MAX_SECRET_HEADERS: usize = 64;

/// Whether `header` may be named by `secret=`: 1 to 128 bytes with no whitespace, control
/// character or `,`, so the list stays one `key=value` wire field.
fn is_secret_header(header: &str) -> bool {
    (1..=128).contains(&header.len())
        && !header
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == ',')
}

/// The headers a `secret=` value names; an empty value names none.
fn parse_secret(value: &str) -> Result<Vec<String>, EditError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let headers: Vec<String> = value.split(',').map(str::to_string).collect();
    let mut seen = std::collections::BTreeSet::new();
    if headers.len() > MAX_SECRET_HEADERS
        || headers
            .iter()
            .any(|header| !is_secret_header(header) || !seen.insert(header.as_str()))
    {
        return Err(EditError::BadField("secret", value.to_string()));
    }
    Ok(headers)
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
            read_only: false,
            secret: None,
        };
        let mut seen = 0u8;
        let mut mark = |bit: u8, word: &str| -> Result<(), EditError> {
            if seen & bit != 0 {
                return Err(EditError::UnknownOption(word.to_string()));
            }
            seen |= bit;
            Ok(())
        };
        for word in words {
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| EditError::UnknownOption(word.to_string()))?;
            match key {
                "line" => {
                    mark(1 << 0, word)?;
                    request.line = Some(parse_line(value)?);
                }
                "create" => {
                    mark(1 << 1, word)?;
                    request.create = match value {
                        "1" => true,
                        "0" => false,
                        _ => return Err(EditError::BadField("create", value.to_string())),
                    };
                }
                "for" => {
                    mark(1 << 2, word)?;
                    if !is_for_word(value) {
                        return Err(EditError::BadField("for", value.to_string()));
                    }
                    request.for_word = Some(value.to_string());
                }
                "wait" => {
                    mark(1 << 3, word)?;
                    request.wait = match value {
                        "1" => true,
                        "0" => false,
                        _ => return Err(EditError::BadField("wait", value.to_string())),
                    };
                }
                "ro" => {
                    mark(1 << 4, word)?;
                    request.read_only = match value {
                        "1" => true,
                        "0" => false,
                        _ => return Err(EditError::BadField("ro", value.to_string())),
                    };
                }
                "secret" => {
                    mark(1 << 5, word)?;
                    request.secret = Some(parse_secret(value)?);
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
        let mut args = String::with_capacity(self.uri.len() + 64);
        args.push_str(&self.uri);
        if let Some((line, col)) = self.line {
            if line == 0 || col == Some(0) {
                return Err(EditError::BadLine(format_line(line, col)));
            }
            use std::fmt::Write as _;
            match col {
                Some(col) => {
                    let _ = write!(args, " line={line}:{col}");
                }
                None => {
                    let _ = write!(args, " line={line}");
                }
            }
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
        if self.read_only {
            args.push_str(" ro=1");
        }
        if let Some(headers) = &self.secret {
            let value = headers.join(",");
            if parse_secret(&value).as_deref() != Ok(headers.as_slice()) {
                return Err(EditError::BadField("secret", value));
            }
            args.push_str(" secret=");
            args.push_str(&value);
        }
        Ok(args)
    }
}

/// `OK edit ticket=<t> view=<v> doc=<d> uri=<canonical-uri>` admission reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOk {
    /// The ticket the edit opened (`ticket=<t>`).
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
        if !self.uri.starts_with("file://") {
            return Err(EditError::BadField("uri", self.uri.clone()));
        }
        Ok(format!(
            "OK edit ticket={} view={} doc={} uri={}",
            self.ticket, self.view, self.doc, self.uri
        ))
    }

    /// Read a reply line (a trailing newline is allowed).
    pub fn parse(line: &str) -> Result<Self, EditError> {
        let [ticket, view, doc, uri] = reply_keyed(
            line,
            &["OK", "edit"],
            ["ticket", "view", "doc", "uri"],
            "OK edit ticket= view= doc= uri=",
        )?;
        if !uri.starts_with("file://") {
            return Err(EditError::BadField("uri", uri.to_string()));
        }
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
    /// How the ticket ended (`result=<r>`).
    pub result: EditResult,
    /// Whether a durable save occurred and the file holds the view's last text.
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
        let [result, saved, seq] = reply_keyed(
            line,
            &["OK", "edit", "done"],
            ["result", "saved", "seq"],
            FORM,
        )?;
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
    /// The file's URI, done when no `edit`-opened view of it remains.
    Uri(String),
}

/// `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwaitEdit {
    /// What the wait is for: one ticket, or every view of one file.
    pub key: AwaitKey,
    /// Wait bound in milliseconds (capped at 600 s, defaulting to 30 s).
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
                    timeout_ms = Some(number(value, "timeout")?);
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
        use std::fmt::Write as _;
        let mut args = match &self.key {
            AwaitKey::Ticket(ticket) => {
                let mut s = String::with_capacity(48);
                let _ = write!(s, "edit ticket={ticket}");
                s
            }
            AwaitKey::Uri(uri) => {
                token("uri", uri)?;
                if !uri.starts_with("file://") {
                    return Err(EditError::BadField("uri", uri.clone()));
                }
                let mut s = String::with_capacity(uri.len() + 32);
                s.push_str("edit uri=");
                s.push_str(uri);
                s
            }
        };
        if let Some(ms) = self.timeout_ms {
            let _ = write!(args, " timeout={ms}");
        }
        Ok(args)
    }
}

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

verify_island! {
    clean {
        theorem for_word_byte_is_exactly_the_word_set (byte : UInt8) :
            trust_import_amacs_protocol__edit__for_word_byte byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 46) || (decide (byte = 95) || decide (byte = 45)))))) := by
          simp only [trust_import_amacs_protocol__edit__for_word_byte]
    }
}

fn token(field: &'static str, value: &str) -> Result<(), EditError> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(EditError::Whitespace(field));
    }
    Ok(())
}

fn number(text: &str, field: &'static str) -> Result<u64, EditError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(EditError::BadField(field, text.to_string()));
    }
    text.parse()
        .map_err(|_| EditError::BadField(field, text.to_string()))
}

fn reply_keyed<'a, const N: usize>(
    line: &'a str,
    leading: &[&str],
    keys: [&'static str; N],
    form: &'static str,
) -> Result<[&'a str; N], EditError> {
    let mut words = line.split_whitespace();
    for want in leading {
        if words.next() != Some(*want) {
            return Err(EditError::Usage(form));
        }
    }
    let mut values = [""; N];
    for (slot, key) in values.iter_mut().zip(keys) {
        let field = words.next().ok_or(EditError::Usage(form))?;
        let value = field
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or(EditError::Usage(form))?;
        if value.is_empty() {
            return Err(EditError::BadField(key, String::new()));
        }
        *slot = value;
    }
    if words.next().is_some() {
        return Err(EditError::Usage(form));
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
                read_only: false,
                secret: None,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((3, None)),
                create: false,
                for_word: Some("git".to_string()),
                wait: true,
                read_only: false,
                secret: None,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((12, Some(7))),
                create: true,
                for_word: Some("claude-code_2.1".to_string()),
                wait: true,
                read_only: false,
                secret: None,
            },
            EditRequest {
                uri: URI.to_string(),
                line: None,
                create: false,
                for_word: Some("git".to_string()),
                wait: false,
                read_only: false,
                secret: None,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((1, None)),
                create: false,
                for_word: Some("aterm".to_string()),
                wait: true,
                read_only: true,
                secret: None,
            },
            EditRequest {
                uri: URI.to_string(),
                line: Some((1, None)),
                create: false,
                for_word: Some("aterm-db".to_string()),
                wait: true,
                read_only: true,
                secret: Some(vec!["password".to_string(), "=totp".to_string()]),
            },
            EditRequest {
                uri: URI.to_string(),
                line: None,
                create: false,
                for_word: Some("aterm-db".to_string()),
                wait: true,
                read_only: false,
                secret: Some(Vec::new()),
            },
        ] {
            let args = request.format().unwrap();
            assert_eq!(EditRequest::parse(&args).unwrap(), request, "{args}");
            assert_eq!(
                args.contains("wait="),
                !request.wait,
                "only `wait=0` is written: {args}"
            );
            assert_eq!(
                args.contains("ro="),
                request.read_only,
                "only `ro=1` is written: {args}"
            );
            assert_eq!(
                args.contains("secret="),
                request.secret.is_some(),
                "`secret=` is written only when given: {args}"
            );
        }
        assert_eq!(
            EditRequest::parse(&format!("{URI} secret="))
                .unwrap()
                .secret,
            Some(Vec::new()),
            "`secret=` names no column"
        );
        assert!(
            !EditRequest::parse(&format!("{URI} ro=0"))
                .unwrap()
                .read_only,
            "`ro=0` is the default spelled out"
        );
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
                read_only: false,
                secret: None,
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
            read_only: false,
            secret: None,
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
            "ro=yes",
            "ro=1 ro=1",
            "secret=a,a",
            "secret=a,,b",
            "secret=,",
            "secret=a secret=b",
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
            read_only: false,
            secret: None,
        };
        assert_eq!(spaced.format(), Err(EditError::Whitespace("uri")));
        let word = EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: Some("git commit".to_string()),
            wait: true,
            read_only: false,
            secret: None,
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
        assert!(EditOk::parse("OK edit ticket=1 view=2 doc=3 uri=/tmp/x").is_err());
        assert!(
            EditOk {
                uri: "/tmp/x".to_string(),
                ..ok.clone()
            }
            .format()
            .is_err()
        );
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
            "edit ticket=1 timeout=+1000",
            "edit ticket=1 timeout=-0",
            "edit ticket=1 later",
        ] {
            assert!(AwaitEdit::parse(bad).is_err(), "{bad}");
        }
    }
}
