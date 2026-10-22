// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! agh's tapes (design §21.3): a strict subset of atape-text 1, the text form of aterm-agent's
//! harness tape (`crates/aterm-agent/src/supervise/tape/text.rs`), written as the run goes
//! through a redacting sink. aterm-agent's `text::parse` reads every agh tape back to the same
//! records; agh does not link aterm-agent.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use astream_effects::EffectRecord;

use crate::error::{Error, Replay, Result, Sub};
use crate::redact::Redactor;
use crate::wire::Endpoints;

/// The first line of every tape.
pub const MAGIC: &str = "atape-text 1";
/// The largest tape `Fx::replay` reads: 64 MiB.
pub const TAPE_MAX: u64 = 64 << 20;

/// The header item's head line.
const HEADER: &str = "header";
/// The doors aterm-agent writes as words and a reply, which agh's subset never holds.
const CONTROL_DOORS: [&str; 2] = ["ctl", "teller"];
/// Every prefix a body line may carry in atape-text 1: a head never starts with one.
const BODY_PREFIXES: [char; 5] = ['|', '>', '<', '=', '!'];
/// The most arguments a header's `argv` holds: no exec takes more (each argument costs at
/// least a byte of `ARG_MAX`, 1 MiB on macOS, and Linux's is a few MiB of strings and
/// pointers). A parse refuses a longer one before it holds a `String` for each token.
const ARGV_MAX: usize = 1 << 20;
/// The header item's line count: `agh`, `api`, `argv`, `mode`, `endpoints`.
const HEADER_LINES: usize = 5;

/// What a tape says about its run: the header item's five lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapeHeader {
    /// `agh {version}`: the build that recorded it. A replay reads it and ignores it.
    pub agh_version: String,
    /// `api {version}`: must be `crate::API_VERSION` for a replay.
    pub api_version: String,
    /// `argv {tokens}`: the command line, less its tape flags.
    pub argv: Vec<String>,
    /// `mode anonymous`: M1's only mode.
    pub mode: TapeMode,
    /// `endpoints github.com`, or `endpoints loopback` for a test's agh-fake.
    pub endpoints: TapeEndpoints,
}

/// How a taped run spoke to GitHub. `#[non_exhaustive]`: M2 adds credentialed tapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TapeMode {
    /// No credential.
    Anonymous,
}

/// Which GitHub a taped run spoke to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapeEndpoints {
    /// `Endpoints::github_com()`.
    GithubCom,
    /// A test's agh-fake.
    Loopback,
}

/// The next of a tape a replay cannot read: every replay of it would be refused the same, so
/// the remedy is a new tape, never `Error::replay`'s CouldNotRun default (`retry later`).
pub(crate) const RECORD_AGAIN: &str =
    "record a new tape: agh --record NEW followed by the command line, then replay that one";

/// `Replay::Format`, naming the tape's line `at` (1-based).
fn format_at(at: usize, what: impl std::fmt::Display) -> Error {
    Error::replay(Replay::Format, format!("line {at}: {what}")).with_next(RECORD_AGAIN)
}

/// aterm-agent's escape (`tape/header.rs`): `\\`, `\n`, `\r`, `\t`, and `\xHH` for each byte
/// of a character `aterm_codec::tty::is_active` flags and for each byte that is not UTF-8.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if aterm_codec::tty::is_active(c) => {
                    for b in c.encode_utf8(&mut [0; 4]).bytes() {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}

/// `escape`'s inverse. A `\` followed by anything but `\`, `n`, `r`, `t` or `x` and two hex
/// digits is refused, its text the reason.
fn unescape(text: &str) -> std::result::Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'\\') => out.push(b'\\'),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'x') => {
                let hex = [bytes.next(), bytes.next()];
                // Two hex digits exactly: `from_str_radix` alone would take a sign (`\x+a`).
                let byte = match hex {
                    [Some(h), Some(l)] if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() => {
                        std::str::from_utf8(&[h, l])
                            .ok()
                            .and_then(|h| u8::from_str_radix(h, 16).ok())
                    }
                    _ => None,
                };
                out.push(byte.ok_or_else(|| "a broken \\x escape".to_string())?);
            }
            _ => return Err("a broken escape".to_string()),
        }
    }
    Ok(out)
}

/// One name as a token: escaped, with a space written `\x20` and `=` written `\x3d`, so no
/// name reads as a field's end or a stamp.
fn token(name: &str) -> String {
    escape(name.as_bytes())
        .replace(' ', "\\x20")
        .replace('=', "\\x3d")
}

/// A token read back.
fn name(word: &str) -> std::result::Result<String, String> {
    String::from_utf8(unescape(word)?).map_err(|_| "a name that is not UTF-8".to_string())
}

/// `body`'s pieces, each behind `prefix`: the body split at every `\n`, each piece escaped
/// after one space, or the prefix alone for an empty piece.
fn pieces(out: &mut String, prefix: char, body: &[u8]) {
    for piece in body.split(|&b| b == b'\n') {
        out.push(prefix);
        if !piece.is_empty() {
            out.push(' ');
            out.push_str(&escape(piece));
        }
        out.push('\n');
    }
}

impl TapeHeader {
    /// This build's `VERSION`, `API_VERSION`, `argv`, Anonymous, and GithubCom when `endpoints`
    /// equals `Endpoints::github_com()`, else Loopback.
    #[must_use]
    pub fn for_run(argv: &[String], endpoints: &Endpoints) -> TapeHeader {
        TapeHeader {
            agh_version: crate::VERSION.to_string(),
            api_version: crate::API_VERSION.to_string(),
            argv: argv.to_vec(),
            mode: TapeMode::Anonymous,
            endpoints: if *endpoints == Endpoints::github_com() {
                TapeEndpoints::GithubCom
            } else {
                TapeEndpoints::Loopback
            },
        }
    }

    /// The five lines, in field order, joined by `\n`, with no final newline. `argv` is each
    /// argument as a token, joined by one space: escaped as a tape line is (`render_record`),
    /// with a space written `\x20` and `=` written `\x3d`. Each token follows `argv` after one
    /// space, so an empty command line is the word alone and an empty argument still counts.
    #[must_use]
    pub fn render(&self) -> String {
        let mut argv = String::from("argv");
        for arg in &self.argv {
            argv.push(' ');
            argv.push_str(&token(arg));
        }
        let mode = match self.mode {
            TapeMode::Anonymous => "anonymous",
        };
        let endpoints = match self.endpoints {
            TapeEndpoints::GithubCom => "github.com",
            TapeEndpoints::Loopback => "loopback",
        };
        format!(
            "agh {}\napi {}\n{argv}\nmode {mode}\nendpoints {endpoints}",
            self.agh_version, self.api_version
        )
    }

    /// The header `render` wrote.
    ///
    /// # Errors
    ///
    /// `Replay::Format`: a line missing, out of order or unknown; an `api` other than
    /// `API_VERSION`; a `mode` other than `anonymous`; an endpoints value that is neither.
    pub fn parse(text: &str) -> Result<TapeHeader> {
        TapeHeader::parse_lines(&header_lines(text), 1)
    }

    /// The header's lines, the first of them line `first` of the text they came from.
    fn parse_lines(lines: &[&str], first: usize) -> Result<TapeHeader> {
        let line = |i: usize, word: &str| -> Result<&str> {
            let at = first + i;
            let text = lines
                .get(i)
                .ok_or_else(|| format_at(at, format!("the header's {word} line is missing")))?;
            if *text == word {
                return Ok("");
            }
            text.strip_prefix(word)
                .and_then(|rest| rest.strip_prefix(' '))
                .ok_or_else(|| format_at(at, format!("the header's line here is not {word}")))
        };
        let agh_version = line(0, "agh")?.to_string();
        let api_version = line(1, "api")?.to_string();
        if api_version != crate::API_VERSION {
            return Err(format_at(
                first + 1,
                format!(
                    "the tape speaks api {api_version}, and this build speaks {}",
                    crate::API_VERSION
                ),
            ));
        }
        let argv_line = lines.get(2).copied().unwrap_or_default();
        line(2, "argv")?;
        let argv = match argv_line.strip_prefix("argv ") {
            None => Vec::new(),
            Some(tokens) => {
                if tokens.split(' ').nth(ARGV_MAX).is_some() {
                    return Err(format_at(
                        first + 2,
                        format!("an argv of more than {ARGV_MAX} arguments"),
                    ));
                }
                tokens
                    .split(' ')
                    .map(|word| name(word).map_err(|e| format_at(first + 2, e)))
                    .collect::<Result<Vec<String>>>()?
            }
        };
        let mode = match line(3, "mode")? {
            "anonymous" => TapeMode::Anonymous,
            other => {
                return Err(format_at(
                    first + 3,
                    format!("mode {other} is not one this build replays"),
                ));
            }
        };
        let endpoints = match line(4, "endpoints")? {
            "github.com" => TapeEndpoints::GithubCom,
            "loopback" => TapeEndpoints::Loopback,
            other => {
                return Err(format_at(
                    first + 4,
                    format!("endpoints {other} are neither github.com nor loopback"),
                ));
            }
        };
        if lines.len() > HEADER_LINES {
            return Err(format_at(
                first + HEADER_LINES,
                "the header holds a line it does not know",
            ));
        }
        Ok(TapeHeader {
            agh_version,
            api_version,
            argv,
            mode,
            endpoints,
        })
    }
}

/// The header text's lines, at most one past `HEADER_LINES`, so `parse_lines` can refuse the
/// first extra line without holding a slice for every one.
fn header_lines(text: &str) -> Vec<&str> {
    text.split('\n').take(HEADER_LINES + 1).collect()
}

/// A whole tape: `MAGIC`, `header` and the header's lines as `|` pieces, then each record.
/// A record outside the subset (`render_record`'s `None`) is left out.
#[must_use]
pub fn render(header: &TapeHeader, records: &[EffectRecord]) -> String {
    let mut out = head_text(header);
    for record in records {
        if let Some(text) = render_record(record) {
            out.push_str(&text);
        }
    }
    out
}

/// `MAGIC` and the header item, each line ending `\n`.
fn head_text(header: &TapeHeader) -> String {
    let mut out = format!("{MAGIC}\n{HEADER}\n");
    pieces(&mut out, '|', header.render().as_bytes());
    out
}

/// One record's lines, each ending `\n`, or `None` for a kind outside the subset (`Rand`,
/// `File`, `FileErr`, `Raised`, which no agh door makes):
/// - `Clock(ns)` → `clock {ns}`; `Mono(ns)` → `mono {ns}`;
/// - `Exchange { door, request, reply }` → `exchange {token(door)}`, the request's pieces as
///   `>` lines, then the reply's as `<` lines;
/// - `Emit { door, bytes }` → `emit {token(door)}`, then the bytes' pieces as `|` lines.
///
/// A body's pieces are the body split at every `\n`, so n newlines give n+1 lines. A line is
/// the prefix, one space and the escaped piece, or the prefix alone for an empty piece. The
/// escape is aterm-agent's (`tape/header.rs`): `\\`, `\n`, `\r`, `\t`, and `\xHH` (two
/// lowercase hex digits) for each byte of a character `aterm_codec::tty::is_active` flags and
/// for each byte that is not UTF-8.
#[must_use]
pub fn render_record(record: &EffectRecord) -> Option<String> {
    let mut out = String::new();
    match record {
        EffectRecord::Clock(ns) => out.push_str(&format!("clock {ns}\n")),
        EffectRecord::Mono(ns) => out.push_str(&format!("mono {ns}\n")),
        EffectRecord::Exchange {
            door,
            request,
            reply,
        } => {
            out.push_str(&format!("exchange {}\n", token(door)));
            pieces(&mut out, '>', request);
            pieces(&mut out, '<', reply);
        }
        EffectRecord::Emit { door, bytes } => {
            out.push_str(&format!("emit {}\n", token(door)));
            pieces(&mut out, '|', bytes);
        }
        EffectRecord::Rand(_)
        | EffectRecord::File { .. }
        | EffectRecord::FileErr { .. }
        | EffectRecord::Raised { .. } => return None,
    }
    Some(out)
}

/// A tape's lines, read one by one as the text is split, so a parse holds no slice per line:
/// a file of newlines costs no more than its own bytes before its first line is judged.
struct Lines<'a> {
    lines: std::iter::Peekable<std::str::Split<'a, char>>,
    /// How many lines have been read: the 1-based number of the last one.
    at: usize,
}

impl<'a> Lines<'a> {
    /// `text`'s lines. Every line ends with a newline, so the empty text after the last one is
    /// no line.
    fn of(text: &'a str) -> Lines<'a> {
        let mut lines = text.strip_suffix('\n').unwrap_or(text).split('\n');
        if text.is_empty() {
            // `"".split` yields one empty piece: an empty text has no line at all.
            let _ = lines.next();
        }
        Lines {
            lines: lines.peekable(),
            at: 0,
        }
    }

    fn next(&mut self) -> Option<&'a str> {
        let line = self.lines.next()?;
        self.at += 1;
        Some(line)
    }

    /// The next line, not read yet.
    fn peek(&mut self) -> Option<&'a str> {
        self.lines.peek().copied()
    }

    /// The next line's piece, when it is a body line behind `prefix`. A glue (`=`) line is
    /// refused: atape-text 1 writes one only for a control door's screen, outside the subset.
    fn piece(&mut self, prefix: char) -> Result<Option<Vec<u8>>> {
        let Some(line) = self.peek() else {
            return Ok(None);
        };
        if line.starts_with('=') {
            return Err(format_at(
                self.at + 1,
                "a `=` line, which agh's tapes do not hold",
            ));
        }
        let Some(rest) = line.strip_prefix(prefix) else {
            return Ok(None);
        };
        let _ = self.next();
        if rest.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let piece = rest
            .strip_prefix(' ')
            .ok_or_else(|| format_at(self.at, format!("no space after {prefix:?}")))?;
        unescape(piece).map(Some).map_err(|e| format_at(self.at, e))
    }

    /// A body behind `prefix`: its pieces joined by newlines, at least one.
    fn bytes(&mut self, prefix: char) -> Result<Vec<u8>> {
        let Some(mut body) = self.piece(prefix)? else {
            let at = self.at + 1;
            let found = self.peek().and_then(|l| l.chars().next());
            return Err(match found {
                Some(c) if BODY_PREFIXES.contains(&c) => format_at(
                    at,
                    format!("a body line behind {c:?} where one behind {prefix:?} belongs"),
                ),
                _ => format_at(at, format!("a body behind {prefix:?} is missing")),
            });
        };
        while let Some(piece) = self.piece(prefix)? {
            body.push(b'\n');
            body.extend(piece);
        }
        Ok(body)
    }
}

/// A number on a head line: ASCII digits alone (`str::parse` alone would take a `+`).
fn number(word: &str, at: usize) -> Result<u64> {
    crate::path::digits(word).ok_or_else(|| format_at(at, format!("{word:?} is no number")))
}

/// One item from its head, at line `at`.
fn item(head: &str, lines: &mut Lines<'_>, at: usize) -> Result<EffectRecord> {
    if head.is_empty() {
        return Err(format_at(at, "an empty line"));
    }
    if head.starts_with(BODY_PREFIXES) {
        return Err(format_at(
            at,
            "a body line with the wrong prefix, where an item's head belongs",
        ));
    }
    if head.split(' ').skip(1).any(|w| w.starts_with("stamp=")) {
        return Err(format_at(at, "a stamp, which agh's tapes do not hold"));
    }
    // A control door is refused on an exchange and on an emit alike.
    let door = |word: &str| -> Result<String> {
        let door = name(word).map_err(|e| format_at(at, e))?;
        if CONTROL_DOORS.contains(&door.as_str()) {
            return Err(format_at(
                at,
                format!("the control door {door}, which agh's tapes do not hold"),
            ));
        }
        Ok(door)
    };
    // The subset's longest head is three words (`exchange DOOR raw`, refused below), so a
    // fourth piece, whatever it holds, is a head outside it: no slice is held per word.
    let words: Vec<&str> = head.splitn(4, ' ').collect();
    let record = match words.as_slice() {
        ["clock", ns] => EffectRecord::Clock(number(ns, at)?),
        ["mono", ns] => EffectRecord::Mono(number(ns, at)?),
        ["exchange", word] => EffectRecord::Exchange {
            door: door(word)?,
            request: lines.bytes('>')?,
            reply: lines.bytes('<')?,
        },
        ["exchange", _, "raw"] => {
            return Err(format_at(
                at,
                "a raw exchange, which agh's tapes do not hold",
            ));
        }
        ["emit", word] => EffectRecord::Emit {
            door: door(word)?,
            bytes: lines.bytes('|')?,
        },
        _ => {
            // The head is not empty (refused above), so its first word is not either.
            let kind = words.first().copied().unwrap_or_default();
            return Err(format_at(
                at,
                format!("a {kind} item, which agh's tapes do not hold"),
            ));
        }
    };
    Ok(record)
}

/// A tape read back.
///
/// # Errors
///
/// `Replay::Format`, naming the line number: no `MAGIC` first line; no header item, or one
/// `TapeHeader::parse` refuses; anything outside the subset (a stamp, `note`, `mark`, `end`, a
/// `=` line, `rand`, `file`, `file-err`, `raised`, `exchange … raw`, the doors `ctl` and
/// `teller`); a body line with the wrong prefix; an escape that does not decode.
pub fn parse(text: &str) -> Result<(TapeHeader, Vec<EffectRecord>)> {
    // The first line is judged before any other is split off.
    let mut lines = Lines::of(text);
    if lines.next() != Some(MAGIC) {
        return Err(format_at(
            1,
            format!("not a tape: its first line is not {MAGIC}"),
        ));
    }
    if lines.next() != Some(HEADER) {
        return Err(format_at(2, "no header item"));
    }
    let first = lines.at + 1;
    let header_bytes = lines.bytes('|')?;
    let header_text = String::from_utf8(header_bytes)
        .map_err(|_| format_at(first, "a header that is not UTF-8"))?;
    let header = TapeHeader::parse_lines(&header_lines(&header_text), first)?;
    let mut records = Vec::new();
    while let Some(head) = lines.next() {
        let at = lines.at;
        records.push(item(head, &mut lines, at)?);
    }
    Ok((header, records))
}

/// The open tape file a sink shares.
struct SinkState {
    file: std::fs::File,
    path: PathBuf,
    failed: Option<String>,
    /// The remedy `finish` names for `failed`. A refusal that every run of this command line
    /// would meet again (a record outside the subset, a rendering the shapes would change, a
    /// tape over `TAPE_MAX`) is `Refused`: its file can never replay, so `finish` removes it.
    remedy: Remedy,
    /// The bytes written so far, header included: never more than `TAPE_MAX`, the most a
    /// replay reads.
    written: u64,
    /// `discard` removed the file.
    discarded: bool,
    /// `finish` succeeded: the exit record is the tape's last, and a record after it is an
    /// error (a `Ctx` reused for another command line), never a line on the tape.
    finished: bool,
}

/// What a sink's latched failure leaves, and the `next` its `finish` names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remedy {
    /// An I/O error: the partial file is kept, as far as the door the run reached, and the
    /// next is `remove {path}, then record again`.
    Kept,
    /// A refusal every run of this command line would meet again: `finish` removes the file,
    /// which holds no exit and could never replay, and the next is `run it without --record:
    /// a tape cannot hold this run`.
    Refused,
    /// `Refused`, its file removed by a `finish`: a later `finish` answers the same, and
    /// removes nothing.
    Removed,
    /// A record after a successful `finish`: the file is whole (its exit is its last record)
    /// and is kept; the next is `replay it: agh --replay {path}`.
    Whole,
    /// `discard` removed the file; the latch's own answer is the caller's.
    Discarded,
    /// The file is flushed and durable, but its directory entry is not (`durable_dir` failed,
    /// as under a parent that cannot be opened for reading): the file is whole and kept, and
    /// the next is `replay it: agh --replay {path}`.
    #[cfg(unix)]
    Unflushed,
}

/// What a recording Fx writes through: a cheap handle on one open file. The recording seam
/// consumes one clone and gives no access back to it (astream's `RecordingSeam::with_sink`;
/// aterm-agent's `tape/core.rs` shares its sink's state the same way), so the Fx keeps
/// another clone to finish the file. Each record's bytes pass `Redactor::shapes` (raw, before
/// the escape) and its text is appended at once, so a run that dies leaves a tape that reaches
/// the door it died at. The first write error is latched for `Fx::finish`. A record outside
/// the subset latches the why `the seam made a {kind} record, which agh's tapes do not hold`,
/// a record whose rendered lines the shapes would still change (the escape lengthens a run:
/// a CR written `\r`, a control byte `\xHH`) latches `a record whose tape line the shapes
/// would change`, a record that would take the file over `TAPE_MAX` latches `the tape would
/// be over {TAPE_MAX} bytes, the most a replay reads`, and a record after a successful
/// `finish` latches `a record came after the tape's exit`; none of them is written. So every
/// line on disk is one `Redactor::shapes` leaves as it is. The first three would recur on every
/// run of the command line, so `finish` removes their file, which holds no exit and could never
/// replay, and the next `--record` to the path is not refused for a file that exists; a write
/// error keeps the partial file, and a record after the exit, or a directory that `finish`
/// cannot flush, keeps the whole one.
/// `Clone` shares the one state (an `Arc<Mutex<..>>`), and `TapeSink: Send + Sync`, so `Fx`
/// stays `Send + Sync` with a clone inside the seam and one beside it.
#[derive(Clone)]
pub struct TapeSink {
    state: Arc<Mutex<SinkState>>,
}

/// CouldNotRun, `Sub::Local`, why `the tape {path} could not be written: {e}`.
fn not_written(path: &Path, e: &str) -> Error {
    Error::could_not_run(format!(
        "the tape {} could not be written: {e}",
        path.display()
    ))
    .with_sub(Sub::Local)
}

/// `path` as one POSIX shell word, for a next meant to be pasted (`super::shell_word`).
fn path_word(path: &Path) -> String {
    super::shell_word(&path.display().to_string())
}

/// The next for a `--record` path that exists: `--record` never wrote it, so removing it is
/// named only when it is a tape (a regular file whose first bytes are `MAGIC` and a newline):
/// `record to a new path, or remove the tape {path} first`, `{path}` one shell word. Anything
/// else (a directory, a source file, a FIFO, a device node) is `record to a new path: what is
/// there is not a tape`, the path left to the why. Only a regular file is opened: opening a
/// FIFO waits for a writer, and reading a terminal waits for a typed line.
fn in_the_way(path: &Path) -> String {
    let magic = format!("{MAGIC}\n");
    let mut head = Vec::new();
    let tape = std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && std::fs::File::open(path)
            .and_then(|file| {
                file.take(u64::try_from(magic.len()).unwrap_or(u64::MAX))
                    .read_to_end(&mut head)
            })
            .is_ok()
        && head == magic.as_bytes();
    if tape {
        format!(
            "record to a new path, or remove the tape {} first",
            path_word(path)
        )
    } else {
        "record to a new path: what is there is not a tape".to_string()
    }
}

/// `not_written` for a sink's latched `why`, with the next its `remedy` names; each path in a
/// next is one shell word. `Unflushed` has its own why: the file is whole, only its directory
/// entry was not flushed.
fn latched_error(path: &Path, why: &str, remedy: Remedy) -> Error {
    let replay_it = || format!("replay it: agh --replay {}", path_word(path));
    match remedy {
        Remedy::Kept => not_written(path, why)
            .with_next(format!("remove {}, then record again", path_word(path))),
        Remedy::Refused | Remedy::Removed => {
            not_written(path, why).with_next("run it without --record: a tape cannot hold this run")
        }
        Remedy::Whole => not_written(path, why).with_next(replay_it()),
        Remedy::Discarded => not_written(path, why),
        #[cfg(unix)]
        Remedy::Unflushed => Error::could_not_run(format!(
            "the tape {} is whole, but its directory could not be flushed: {why}",
            path.display()
        ))
        .with_sub(Sub::Local)
        .with_next(replay_it()),
    }
}

impl TapeSink {
    /// Create `path` (`create_new`; mode 0600 on unix) and write `MAGIC` and the header item,
    /// each argument of its `argv` through `Redactor::shapes`.
    ///
    /// # Errors
    ///
    /// As `Fx::record`. A header whose rendered lines the shapes would still change (two
    /// arguments that read as one shape once joined, `Bearer` then a long word) is CouldNotRun,
    /// `Sub::Local`, before any file is made. A header that cannot be written removes the file
    /// it was written to.
    pub fn create(path: &Path, header: &TapeHeader) -> Result<TapeSink> {
        if head_redacts(header) {
            return Err(Error::could_not_run(format!(
                "the tape {} was not made: its command line holds a token shape across its \
                 arguments, which a tape redacts",
                path.display()
            ))
            .with_sub(Sub::Local)
            .with_next("record a command line that names no token"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let head = head_text(&shaped_header(header));
        let mut file = options.open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::could_not_run(format!(
                    "the tape {} exists: --record never overwrites",
                    path.display()
                ))
                .with_sub(Sub::Local)
                .with_next(in_the_way(path))
            } else {
                Error::could_not_run(format!(
                    "the tape {} could not be created: {e}",
                    path.display()
                ))
                .with_sub(Sub::Local)
            }
        })?;
        if let Err(e) = file.write_all(head.as_bytes()) {
            // This call made the file (`create_new`), so removing it loses nothing, and the
            // next `--record` to this path is not refused for a file holding half a header.
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(not_written(path, &e.to_string()));
        }
        Ok(TapeSink {
            state: Arc::new(Mutex::new(SinkState {
                file,
                path: path.to_path_buf(),
                failed: None,
                remedy: Remedy::Kept,
                written: u64::try_from(head.len()).unwrap_or(u64::MAX),
                discarded: false,
                finished: false,
            })),
        })
    }

    /// The first error latched, as its text, if any.
    #[must_use]
    pub fn failed(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed
            .clone()
    }

    /// Flush the file, then make it durable (`aterm_durable::durable`). On unix, then also the
    /// directory entry this sink's `create` made (`aterm_durable::durable_dir` of its parent);
    /// Windows opens no directory as a file, so there the file's own flush is the whole step.
    /// A successful `finish` closes the tape to records: a later `append` writes nothing and
    /// latches its why, which the next `finish` answers.
    ///
    /// # Errors
    ///
    /// CouldNotRun, `Sub::Local`, why `the tape {path} could not be written: {e}`: the latched
    /// error, or the flush's own. A refusal every run would meet again (see `TapeSink`) first
    /// removes the file, and its next is `run it without --record: a tape cannot hold this
    /// run`; a write error keeps the partial file, next `remove {path}, then record again`. A
    /// directory that cannot be flushed after the file was made durable is why `the tape {path}
    /// is whole, but its directory could not be flushed: {e}`, next `replay it: agh --replay
    /// {path}`: the file is kept. In each next, `{path}` is one POSIX shell word.
    pub fn finish(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = &state.failed {
            if state.remedy == Remedy::Refused {
                // `create_new` made this file and the refusal recurs on every run, so removing
                // it loses nothing; a removal that fails leaves it, and says so.
                return match std::fs::remove_file(&state.path) {
                    Ok(()) => {
                        let e = e.clone();
                        state.remedy = Remedy::Removed;
                        Err(latched_error(&state.path, &e, Remedy::Removed))
                    }
                    Err(_) => Err(latched_error(&state.path, e, Remedy::Kept)),
                };
            }
            return Err(latched_error(&state.path, e, state.remedy));
        }
        let done = state
            .file
            .flush()
            .and_then(|()| aterm_durable::durable(&state.file));
        if let Err(e) = done {
            let e = e.to_string();
            state.failed = Some(e.clone());
            return Err(latched_error(&state.path, &e, Remedy::Kept));
        }
        // The file is whole and durable from here: a directory that cannot be flushed (a parent
        // that cannot be opened for reading) keeps it, and its next replays it.
        #[cfg(unix)]
        {
            let dir = state
                .path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            if let Err(e) = aterm_durable::durable_dir(dir) {
                let e = e.to_string();
                state.failed = Some(e.clone());
                state.remedy = Remedy::Unflushed;
                return Err(latched_error(&state.path, &e, Remedy::Unflushed));
            }
        }
        state.finished = true;
        Ok(())
    }

    /// Remove the file this sink's `create` made, for a run whose tape can never replay (a
    /// rerecording that diverged): nobody's work is lost, because the old tape still holds
    /// it. Every later `append` writes nothing. A removal that fails leaves the file.
    pub(crate) fn discard(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.failed = Some("the tape was discarded".to_string());
        state.remedy = Remedy::Discarded;
        state.discarded = true;
        let _ = std::fs::remove_file(&state.path);
    }

    /// Whether `discard` removed the file.
    pub(crate) fn discarded(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .discarded
    }

    /// Append one record as `render_record` writes it, its bytes through `Redactor::shapes`
    /// first (`shaped_record`), so the tape decodes to exactly the redacted bytes; a rendering
    /// the shapes would still change is latched, not written. The rerecording Fx calls it
    /// directly; the recording seam calls it through `RecordSink`.
    pub fn append(&self, record: &EffectRecord) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.failed.is_some() {
            return;
        }
        if state.finished {
            state.failed = Some("a record came after the tape's exit".to_string());
            state.remedy = Remedy::Whole;
            return;
        }
        let Some(text) = render_record(&shaped_record(record)) else {
            state.failed = Some(format!(
                "the seam made a {} record, which agh's tapes do not hold",
                record.kind()
            ));
            state.remedy = Remedy::Refused;
            return;
        };
        // The size first: a line no tape can hold is refused without the shapes reading it.
        let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
        if state.written.saturating_add(size) > TAPE_MAX {
            state.failed = Some(format!(
                "the tape would be over {TAPE_MAX} bytes, the most a replay reads"
            ));
            state.remedy = Remedy::Refused;
            return;
        }
        if Redactor::shapes(&text) != text {
            state.failed = Some(format!(
                "a {} record whose tape line the shapes would change",
                record.kind()
            ));
            state.remedy = Remedy::Refused;
            return;
        }
        match state.file.write_all(text.as_bytes()) {
            Ok(()) => state.written = state.written.saturating_add(size),
            Err(e) => state.failed = Some(e.to_string()),
        }
    }
}

/// `bytes` through `Redactor::shapes`: each valid UTF-8 run shaped, each invalid byte kept as
/// it is. The shapes read raw text, never the tape's escaped rendering: there a JSON `\"`
/// reads `\\"` and a tab reads `\t`, which the shapes would miss, or cut one backslash short.
/// A replay compares the run's stdout through it, as the sink wrote the recording's.
pub(crate) fn shaped(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.extend_from_slice(Redactor::shapes(chunk.valid()).as_bytes());
        out.extend_from_slice(chunk.invalid());
    }
    out
}

/// `header` with each argument of its `argv` through the shapes as raw text, before `token`
/// writes its spaces as `\x20`, which the shapes would not read as blanks.
fn shaped_header(header: &TapeHeader) -> TapeHeader {
    TapeHeader {
        argv: header
            .argv
            .iter()
            .map(|arg| Redactor::shapes(arg))
            .collect(),
        ..header.clone()
    }
}

/// Whether the header item a sink would write for `header` holds a line the shapes would
/// change: two arguments that read as one shape once joined (`Bearer` then a long word).
/// `TapeSink::create` refuses such a header; `cli::taping::begin_with` asks first, so the
/// refusal is a usage error before any file is made.
pub(crate) fn head_redacts(header: &TapeHeader) -> bool {
    let head = head_text(&shaped_header(header));
    Redactor::shapes(&head) != head
}

/// `record` with every byte field `shaped`: an exchange's request and reply, an emit's bytes.
/// A door is agh's own constant, and every other kind is left out of the subset anyway.
fn shaped_record(record: &EffectRecord) -> EffectRecord {
    match record {
        EffectRecord::Exchange {
            door,
            request,
            reply,
        } => EffectRecord::Exchange {
            door: door.clone(),
            request: shaped(request),
            reply: shaped(reply),
        },
        EffectRecord::Emit { door, bytes } => EffectRecord::Emit {
            door: door.clone(),
            bytes: shaped(bytes),
        },
        other => other.clone(),
    }
}

/// Whether the shapes would change `bytes` on the tape (`shaped`): a canonical request a tape
/// of which could not replay, because the replay asks the unredacted question.
pub(crate) fn redacts(bytes: &[u8]) -> bool {
    shaped(bytes) != bytes
}

/// Whether the shapes would change the canonical reply `reply` (`canon::reply`'s bytes), or
/// any string of the JSON text its body holds, read decoded. A replay answers the redacted
/// bytes, so a run over the live ones would render other stdout than its replay; and in a
/// JSON string an escape (`\t`, ` `) can stand where the shapes look for a blank, so a
/// credential they would miss in the raw text is caught in the decoded one.
pub(crate) fn redacts_a_reply(reply: &[u8]) -> bool {
    if redacts(reply) {
        return true;
    }
    let body = reply
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .and_then(|at| reply.get(at + 2..));
    body.and_then(|body| aterm_json::from_slice::<aterm_json::Value>(body).ok())
        .is_some_and(|value| json_redacts(&value))
}

/// Whether the shapes would change a string of `value`, a key or a value, at any depth.
fn json_redacts(value: &aterm_json::Value) -> bool {
    let changes = |text: &str| Redactor::shapes(text) != text;
    match value {
        aterm_json::Value::String(text) => changes(text),
        aterm_json::Value::Array(items) => items.iter().any(json_redacts),
        aterm_json::Value::Object(map) => map
            .iter()
            .any(|(key, item)| changes(key) || json_redacts(item)),
        _ => false,
    }
}

impl std::fmt::Debug for TapeSink {
    /// `TapeSink({path})`: the file it writes, never a record.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        write!(f, "TapeSink({})", state.path.display())
    }
}

impl astream_effects::RecordSink for TapeSink {
    fn record(&mut self, record: &EffectRecord) {
        self.append(record);
    }
}
