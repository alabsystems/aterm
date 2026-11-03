// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh` effect tapes (`atape-text 1` subset) written through a redacting sink.

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use astream_effects::EffectRecord;

use crate::error::{Error, Replay, Result};
use crate::redact::Redactor;
use crate::wire::Endpoints;

/// First line of every tape.
pub const MAGIC: &str = "atape-text 1";
/// Maximum tape size `Fx::replay` reads (64 MiB).
pub const TAPE_MAX: u64 = 64 << 20;

const HEADER: &str = "header";
const CONTROL_DOORS: [&str; 2] = ["ctl", "teller"];
const BODY_PREFIXES: [char; 5] = ['|', '>', '<', '=', '!'];
const ARGV_MAX: usize = 1 << 20;
const HEADER_LINES: usize = 5;

/// Header metadata recorded at the start of a tape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapeHeader {
    /// `agh {version}`: the build that recorded it.
    pub agh_version: String,
    /// `api {version}`: must be `crate::API_VERSION` for a replay.
    pub api_version: String,
    /// `argv {tokens}`: the command line, less its tape flags.
    pub argv: Vec<String>,
    /// `mode anonymous`, or `mode credentialed`.
    pub mode: TapeMode,
    /// `endpoints github.com`, or `endpoints loopback` for a test's agh-fake.
    pub endpoints: TapeEndpoints,
}

/// Credential mode of a taped run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TapeMode {
    /// No credential.
    Anonymous,
    /// `mode credentialed`: a run that spoke as a profile, its decisions taped at the route
    /// door and its credential never, so it replays with no agh.toml and no token.
    Credentialed,
}

/// Endpoint target of a taped run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapeEndpoints {
    /// `Endpoints::github_com()`.
    GithubCom,
    /// A test's agh-fake.
    Loopback,
}

/// The next of a tape a replay cannot read: every replay of it would be refused the same, so the remedy is a new tape, never `Error::replay`'s CouldNotRun default (`retry later`).
pub(crate) const RECORD_AGAIN: &str =
    "record a new tape: agh --record NEW followed by the command line, then replay that one";

fn format_at(at: usize, what: impl std::fmt::Display) -> Error {
    Error::replay(Replay::Format, format!("line {at}: {what}")).with_next(RECORD_AGAIN)
}

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

fn token(name: &str) -> String {
    escape(name.as_bytes())
        .replace(' ', "\\x20")
        .replace('=', "\\x3d")
}

fn name(word: &str) -> std::result::Result<String, String> {
    String::from_utf8(unescape(word)?).map_err(|_| "a name that is not UTF-8".to_string())
}

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
    /// Build a header for `argv` and `endpoints`.
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

    /// Render the five header lines joined by `\n`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut argv = String::from("argv");
        for arg in &self.argv {
            argv.push(' ');
            argv.push_str(&token(arg));
        }
        let mode = match self.mode {
            TapeMode::Anonymous => "anonymous",
            TapeMode::Credentialed => "credentialed",
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

    /// Parse a header written by `render`.
    ///
    /// # Errors
    /// `Replay::Format`: a line missing, out of order or unknown; an `api` other than `API_VERSION`; a `mode` other than `anonymous` or `credentialed`; an endpoints value that is neither.
    pub fn parse(text: &str) -> Result<TapeHeader> {
        TapeHeader::parse_lines(&header_lines(text), 1)
    }

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
            "credentialed" => TapeMode::Credentialed,
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

fn header_lines(text: &str) -> Vec<&str> {
    text.split('\n').take(HEADER_LINES + 1).collect()
}

/// Render a full tape from `header` and `records`.
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

fn head_text(header: &TapeHeader) -> String {
    let mut out = format!("{MAGIC}\n{HEADER}\n");
    pieces(&mut out, '|', header.render().as_bytes());
    out
}

/// Render one record's lines, or `None` for record kinds outside the `agh` subset.
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

struct Lines<'a> {
    lines: std::iter::Peekable<std::str::Split<'a, char>>,
    /// How many lines have been read: the 1-based number of the last one.
    at: usize,
}

impl<'a> Lines<'a> {
    fn of(text: &'a str) -> Lines<'a> {
        let mut lines = text.strip_suffix('\n').unwrap_or(text).split('\n');
        if text.is_empty() {
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

    fn peek(&mut self) -> Option<&'a str> {
        self.lines.peek().copied()
    }

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

fn number(word: &str, at: usize) -> Result<u64> {
    crate::path::digits(word).ok_or_else(|| format_at(at, format!("{word:?} is no number")))
}

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
            let kind = words.first().copied().unwrap_or_default();
            return Err(format_at(
                at,
                format!("a {kind} item, which agh's tapes do not hold"),
            ));
        }
    };
    Ok(record)
}

/// Parse a tape back into its header and effect records.
///
/// # Errors
/// `Replay::Format`, naming the line number: no `MAGIC` first line; no header item, or one `TapeHeader::parse` refuses; anything outside the subset (a stamp, `note`, `mark`, `end`, a `=` line, `rand`, `file`, `file-err`, `raised`, `exchange … raw`, the doors `ctl` and `teller`); a body line with the wrong prefix; an escape that does not decode.
pub fn parse(text: &str) -> Result<(TapeHeader, Vec<EffectRecord>)> {
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

struct SinkState {
    file: std::fs::File,
    path: PathBuf,
    failed: Option<String>,
    /// The remedy `finish` names for `failed`. A refusal that every run of this command line
    /// would meet again (a record outside the subset, a rendering the shapes would change, a
    /// tape over `TAPE_MAX`) is `Refused`: its file can never replay, so `finish` removes it.
    remedy: Remedy,
    /// The bytes written so far, header included: never more than `max`.
    written: u64,
    /// The most the file may hold: `TAPE_MAX`, the most a replay reads (`create`), or a
    /// test's own smaller bound (`create_within`).
    max: u64,
    /// `discard` removed the file.
    discarded: bool,
    /// `finish` succeeded: the exit record is the tape's last, and a record after it is an
    /// error (a `Ctx` reused for another command line), never a line on the tape.
    finished: bool,
    /// A credentialed tape's redactor (`with_redactor`): every record passes its `apply`, which
    /// holds the credentials learned so far, where an anonymous tape's pass `Redactor::shapes`.
    redactor: Option<Redactor>,
    /// The header item `create` wrote (`MAGIC`, then the shaped header): `finish` reads it
    /// again through the redactor, which may have learned a credential the command line names
    /// after the header was written.
    head: String,
    /// The streamed bodies this sink made in `{tape}.d` (`store_body`): a tape that is removed
    /// (`discard`, a refused `finish`) or left cut short (`finish` after a write error) removes
    /// them, then `{tape}.d` once it is empty.
    #[cfg(unix)]
    sidecars: Vec<PathBuf>,
}

/// Remove the streamed bodies `state`'s sink made, then `{tape}.d` when nothing else is in it:
/// its tape is gone or cut short, and a body beside no tape that replays serves nobody.
#[cfg(unix)]
fn drop_sidecars(state: &mut SinkState) {
    for path in state.sidecars.drain(..) {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir(sidecar_dir(&state.path));
}

/// What a sink's latched failure leaves, and the `next` its `finish` names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remedy {
    /// An I/O error: the partial file is kept, as far as the door the run reached, its streamed
    /// bodies removed (it never replays), and the next is `remove {path}, then record again`.
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
    Unflushed,
}

/// Shared handle to an open recording tape file.
#[derive(Clone)]
pub struct TapeSink {
    state: Arc<Mutex<SinkState>>,
}

fn not_written(path: &Path, e: &str) -> Error {
    Error::local(format!(
        "the tape {} could not be written: {e}",
        path.display()
    ))
}

fn path_word(path: &Path) -> String {
    super::shell_word(&path.display().to_string())
}

fn in_the_way(path: &Path) -> String {
    let magic = format!("{MAGIC}\n");
    let mut head = Vec::new();
    let tape = std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && crate::download::open_nonblocking(path)
            .and_then(|file| {
                if !file.metadata()?.is_file() {
                    return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
                }
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
        Remedy::Unflushed => Error::local(format!(
            "the tape {} is whole, but its directory could not be flushed: {why}",
            path.display()
        ))
        .with_next(replay_it()),
    }
}

impl TapeSink {
    /// The same sink, passing every record through `redactor.apply`, which holds the learned
    /// credentials, where M1's passes `Redactor::shapes`: a credentialed tape's sink.
    #[must_use]
    pub fn with_redactor(self, redactor: crate::redact::Redactor) -> TapeSink {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .redactor = Some(redactor);
        self
    }

    /// Create `path` and write `MAGIC` and the shaped header item.
    ///
    /// # Errors
    /// As `Fx::record`. A header whose rendered lines the shapes would still change (two
    /// arguments that read as one shape once joined, `Bearer` then a long word) is CouldNotRun,
    /// `Sub::Local`, before any file is made. A header that cannot be written removes the file
    /// it was written to. A header over the tape's size bound fails before any file is made.
    pub fn create(path: &Path, header: &TapeHeader) -> Result<TapeSink> {
        Self::open_within(path, header, TAPE_MAX)
    }

    /// `create`, then `with_redactor(redactor)`: a credentialed tape's sink. Its header item
    /// passes only `Redactor::shapes` when it is written, so it is first read through
    /// `redactor.apply`, which holds the credentials learned so far (a vault token, once
    /// adoption has settled), and `finish` reads it again for any learned later.
    ///
    /// # Errors
    /// As `create`. A header item `redactor.apply` would change is CouldNotRun, `Sub::Local`,
    /// why `the tape {path} was not made: its command line holds a credential this run used`,
    /// next `record a command line that names no token`, before any file is made.
    pub(crate) fn create_redacted(
        path: &Path,
        header: &TapeHeader,
        redactor: Redactor,
    ) -> Result<TapeSink> {
        if head_holds_a_credential(&head_text(&shaped_header(header)), &redactor) {
            return Err(Error::local(format!(
                "the tape {} was not made: {HEAD_CREDENTIAL}",
                path.display()
            ))
            .with_next("record a command line that names no token"));
        }
        Ok(Self::create(path, header)?.with_redactor(redactor))
    }

    /// Create `path` with the file held to `max` bytes in place of `TAPE_MAX`.
    ///
    /// # Errors
    /// As `create`.
    #[cfg(any(test, debug_assertions))]
    pub fn create_within(path: &Path, header: &TapeHeader, max: u64) -> Result<TapeSink> {
        Self::open_within(path, header, max)
    }

    fn open_within(path: &Path, header: &TapeHeader, max: u64) -> Result<TapeSink> {
        if head_redacts(header) {
            return Err(Error::local(format!(
                "the tape {} was not made: its command line holds a token shape across its \
                 arguments, which a tape redacts",
                path.display()
            ))
            .with_next("record a command line that names no token"));
        }
        let head = head_text(&shaped_header(header));
        let written = u64::try_from(head.len()).unwrap_or(u64::MAX);
        if written > max {
            return Err(latched_error(
                path,
                &format!("the header would be over {max} bytes, the tape's size bound"),
                Remedy::Refused,
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::local(format!(
                    "the tape {} exists: --record never overwrites",
                    path.display()
                ))
                .with_next(in_the_way(path))
            } else {
                Error::local(format!(
                    "the tape {} could not be created: {e}",
                    path.display()
                ))
            }
        })?;
        if let Err(e) = file.write_all(head.as_bytes()) {
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
                written,
                max,
                discarded: false,
                finished: false,
                redactor: None,
                head,
                #[cfg(unix)]
                sidecars: Vec::new(),
            })),
        })
    }

    /// First latched error message, if any.
    #[must_use]
    pub fn failed(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failed
            .clone()
    }

    /// Flush the file, then make it durable (`aterm_durable::durable`).
    ///
    /// # Errors
    /// CouldNotRun, `Sub::Local`, why `the tape {path} could not be written: {e}`: the latched
    /// error, or the flush's own. A refusal every run would meet again (a record outside the
    /// subset, a rendering the shapes would change, a tape over `TAPE_MAX`, or a header that a
    /// credentialed sink's redactor would now change: a credential the command line names,
    /// learned after the header was written) first removes the file, and its next is `run it
    /// without --record: a tape cannot hold this run` (a file it cannot remove is kept, with a
    /// write error's next); a write error keeps the partial file, next `remove {path}, then
    /// record again`. A tape left that way or refused never replays, so the streamed bodies
    /// this sink stored go with it (`drop_sidecars`). A directory that cannot be flushed after
    /// the file was made durable is why `the tape {path} is whole, but its directory could not
    /// be flushed: {e}`, next `replay it: agh --replay {path}`: the file and its bodies are
    /// kept. In each next, `{path}` is one POSIX shell word.
    pub fn finish(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let head_holds_one = state
            .redactor
            .as_ref()
            .is_some_and(|redactor| head_holds_a_credential(&state.head, redactor));
        if state.failed.is_none() && head_holds_one {
            state.failed = Some(HEAD_CREDENTIAL.to_string());
            state.remedy = Remedy::Refused;
        }
        if let Some(e) = state.failed.clone() {
            let remedy = match state.remedy {
                Remedy::Refused => match std::fs::remove_file(&state.path) {
                    Ok(()) => {
                        state.remedy = Remedy::Removed;
                        Remedy::Removed
                    }
                    Err(_) => Remedy::Kept,
                },
                remedy => remedy,
            };
            #[cfg(unix)]
            if matches!(remedy, Remedy::Kept | Remedy::Removed) {
                drop_sidecars(&mut state);
            }
            return Err(latched_error(&state.path, &e, remedy));
        }
        let done = state
            .file
            .flush()
            .and_then(|()| aterm_durable::durable(&state.file));
        if let Err(e) = done {
            let e = e.to_string();
            state.failed = Some(e.clone());
            #[cfg(unix)]
            drop_sidecars(&mut state);
            return Err(latched_error(&state.path, &e, Remedy::Kept));
        }
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
        state.finished = true;
        Ok(())
    }

    /// Remove the file this sink's `create` made, for a run whose tape can never replay (a rerecording that diverged): nobody's work is lost, because the old tape still holds it. The streamed bodies this sink stored go with it (`drop_sidecars`).
    pub(crate) fn discard(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.failed = Some("the tape was discarded".to_string());
        state.remedy = Remedy::Discarded;
        state.discarded = true;
        let _ = std::fs::remove_file(&state.path);
        #[cfg(unix)]
        drop_sidecars(&mut state);
    }

    /// Whether `discard` removed the file.
    pub(crate) fn discarded(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .discarded
    }

    /// Append one redacted record to the tape.
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
        let Some(text) = render_record(&redacted_record(record, state.redactor.as_ref())) else {
            state.failed = Some(format!(
                "the seam made a {} record, which agh's tapes do not hold",
                record.kind()
            ));
            state.remedy = Remedy::Refused;
            return;
        };
        let size = u64::try_from(text.len()).unwrap_or(u64::MAX);
        if state.written.saturating_add(size) > state.max {
            state.failed = Some(format!(
                "the tape would be over {} bytes, the most a replay reads",
                state.max
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

/// `{tape}.d`: the directory beside a credentialed tape that holds its streamed bodies, each
/// named by its lowercase hex SHA-256.
#[cfg(unix)]
pub(crate) fn sidecar_dir(tape: &Path) -> PathBuf {
    let mut name = tape.as_os_str().to_os_string();
    name.push(".d");
    PathBuf::from(name)
}

/// A streamed body stored beside its tape (`TapeSink::store_body`).
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Stored {
    /// Its lowercase hex SHA-256: the file's name in `{tape}.d`.
    pub(crate) hex: String,
    /// Its length.
    pub(crate) len: u64,
    /// The very file the bytes were written to, open at its start: the caller streams what
    /// was hashed, whatever is at `{tape}.d/{hex}` later.
    pub(crate) file: std::fs::File,
}

/// Why `TapeSink::store_body` stored nothing.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) enum StoreFault {
    /// The body could not be read: the answer's own failure.
    Read(std::io::Error),
    /// The sidecar could not be written: could_not_run, `Sub::Local`, its next naming
    /// `{tape}.d`.
    Write(Error),
    /// The body holds a credential the sink learned, or a token shape (`Redactor::apply`): a
    /// tape never holds one, in a sidecar either.
    Shaped,
}

/// The bytes of a stored body each scan looks at again, before the next chunk: more than any
/// credential or shape `Redactor::apply` finds, so one split between two reads is still seen
/// whole.
#[cfg(unix)]
const SCAN_OVERLAP: usize = 4096;

/// The fewest word bytes (`[A-Za-z0-9_]`) after a token prefix that a streamed body's scan
/// counts as a token GitHub could have issued: classic tokens carry 36, fine-grained ones 82.
/// `Redactor::shapes` (rule 1) takes one, which binary data holds by chance: five bytes such as
/// `ghs_7` turn up about once in 3.4 GB of random bytes, so one in four 1 GiB assets would
/// never record, where a prefix and 20 such bytes turn up about once in 10^21 bytes.
#[cfg(unix)]
const TOKEN_TAIL_MIN: usize = 20;

/// Whether a valid UTF-8 run of `bytes`, a streamed body, holds what its tape must not keep: a
/// credential this run learned (its exact bytes or git's Basic form, `Redactor::learn`), a
/// value under `Authorization:`, `bearer` or `basic` (`Redactor::shapes`' rules 2 to 4), or a
/// token prefix followed by at least `TOKEN_TAIL_MIN` word bytes (rule 1, held to a length
/// GitHub issues).
#[cfg(unix)]
fn holds_a_secret(bytes: &[u8], redactor: &Redactor) -> bool {
    bytes
        .utf8_chunks()
        .any(|chunk| text_holds_a_secret(chunk.valid(), redactor))
}

/// `holds_a_secret` for one valid UTF-8 run.
#[cfg(unix)]
fn text_holds_a_secret(text: &str, redactor: &Redactor) -> bool {
    // Nothing `apply` would change holds nothing: the common case, one pass.
    if text.is_empty() || redactor.apply(text) == text {
        return false;
    }
    let learned =
        |text: &str| redactor.learned_and_tokens(text) != Redactor::new().learned_and_tokens(text);
    // A learned credential anywhere. A redacted credential that is exactly one of rule 1's
    // runs reads the same either way, and is caught word by word below.
    if learned(text) {
        return true;
    }
    // Rules 2 to 4 alone: every token prefix holds `_`, and `-` is no byte rules 2 to 4 read
    // otherwise than `_` (neither is blank, a quote or base64), so no rule 1 run is left.
    let masked = text.replace('_', "-");
    if Redactor::shapes(&masked) != masked {
        return true;
    }
    // Rule 1, word by word: a run starts at a word's first prefix and takes the rest of the
    // word, so it has `TOKEN_TAIL_MIN` bytes after its prefix exactly when the word less its
    // last `TOKEN_TAIL_MIN - 1` bytes still holds a prefix and one byte after it.
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| Redactor::shapes(word) != *word)
        .any(|word| {
            let head = word.get(..word.len().saturating_sub(TOKEN_TAIL_MIN - 1));
            let long = head.is_some_and(|head| Redactor::shapes(head) != head);
            // A short run that is a learned credential: one more word byte grows rule 1's
            // run, never the credential, so the two redactions part.
            long || ["Z", "0"]
                .iter()
                .any(|more| learned(&format!("{word}{more}")))
        })
}

#[cfg(unix)]
impl TapeSink {
    /// Store at most `cap + 1` bytes of `body` beside this sink's tape: `{tape}.d/.partial-{n}`
    /// (`{tape}.d` made 0700 by `create_private_dir`, the file 0600 and `create_new`, a partial
    /// a killed recording left there removed first), hashed and scanned as it is written, made
    /// durable, then renamed over `{tape}.d/{hex}`. `n` is the tape's count of streamed answers
    /// so far, unique within the run. The scan is `holds_a_secret` with the sink's redactor
    /// (one that learned nothing on a sink with none) over each valid UTF-8 run,
    /// `SCAN_OVERLAP` bytes carried across reads. A body file this store made is the sink's:
    /// `discard` removes it. The partial file is removed on every failure.
    pub(crate) fn store_body(
        &self,
        n: u64,
        body: &mut dyn std::io::Read,
        cap: u64,
    ) -> std::result::Result<Stored, StoreFault> {
        use std::io::{Seek as _, SeekFrom};
        use std::os::unix::fs::OpenOptionsExt;

        let (dir, redactor) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (
                sidecar_dir(&state.path),
                state.redactor.clone().unwrap_or_default(),
            )
        };
        let fault = |what: &str, e: &dyn std::fmt::Display| {
            StoreFault::Write(
                Error::local(format!("{what} {}: {e}", dir.display())).with_next(format!(
                    "make room in {}, or record to another path",
                    path_word(&dir)
                )),
            )
        };
        aterm_uds::private_file::create_private_dir(&dir)
            .map_err(|e| fault("the tape's body directory could not be made:", &e))?;
        let partial = dir.join(format!(".partial-{n}"));
        // A recording killed mid-store leaves its partial; `{tape}.d` is private, and the tape
        // it served is gone or this one, which `create_new` made.
        let _ = std::fs::remove_file(&partial);
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|e| fault("a body file could not be made in", &e))?;
        let gone = |fault: StoreFault| {
            let _ = std::fs::remove_file(&partial);
            fault
        };
        let mut hasher = aterm_digest::Sha256::new();
        let mut len: u64 = 0;
        let mut buf = vec![0u8; 64 * 1024];
        let mut window: Vec<u8> = Vec::with_capacity(SCAN_OVERLAP + buf.len());
        let mut body = body.take(cap.saturating_add(1));
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(gone(StoreFault::Read(e))),
            };
            let chunk = buf.get(..n).unwrap_or_default();
            window.extend_from_slice(chunk);
            if holds_a_secret(&window, &redactor) {
                return Err(gone(StoreFault::Shaped));
            }
            window.drain(..window.len().saturating_sub(SCAN_OVERLAP));
            hasher.update(chunk);
            file.write_all(chunk)
                .map_err(|e| gone(fault("a body file could not be written in", &e)))?;
            len = len.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        }
        file.flush()
            .and_then(|()| aterm_durable::durable(&file))
            .map_err(|e| gone(fault("a body file could not be flushed in", &e)))?;
        let hex = aterm_codec::hex::encode_lower(&hasher.finalize());
        let path = dir.join(&hex);
        let fresh = std::fs::symlink_metadata(&path).is_err();
        std::fs::rename(&partial, &path)
            .map_err(|e| gone(fault("a body file could not be named in", &e)))?;
        if fresh {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sidecars
                .push(path);
        }
        aterm_durable::durable_dir(&dir)
            .map_err(|e| fault("the tape's body directory could not be flushed:", &e))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| fault("a body file could not be read back in", &e))?;
        Ok(Stored { hex, len, file })
    }
}

/// The next of a replay whose streamed body is missing or not the one taped.
#[cfg(unix)]
const STREAM_AGAIN: &str =
    "record the tape again: agh --record NEW followed by the command line, then replay that one";

/// `Replay::StreamDigest`: the sidecar `{dir}/{hex}` is missing, or holds other bytes.
#[cfg(unix)]
pub(crate) fn stream_digest(dir: &Path, hex: &str, what: &str) -> Error {
    Error::replay(
        Replay::StreamDigest,
        format!("the tape's body file {} {what}", dir.join(hex).display()),
    )
    .with_next(STREAM_AGAIN)
}

/// A taped body as it replays: the sidecar's bytes, hashed as they are read, never more than
/// the taped length. At their end (the file's, or the taped length when the file goes on), a
/// digest or length that is not the taped one latches `Replay::StreamDigest` into `latch` and
/// fails the read, so the run stops there and its exit is the replay's.
#[cfg(unix)]
pub(crate) struct DigestReader {
    file: std::fs::File,
    hasher: Option<aterm_digest::Sha256>,
    dir: PathBuf,
    hex: String,
    len: u64,
    seen: u64,
    latch: Arc<Mutex<Option<Error>>>,
}

/// `path` opened to read when its entry is a regular file and the handle is that entry: the
/// entry is judged unfollowed (`symlink_metadata`), the open does not wait (`O_NONBLOCK`), and
/// the handle's device and inode must be the entry's, so an entry swapped between the two is
/// refused too. `None` for anything else.
#[cfg(unix)]
fn open_regular(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::MetadataExt as _;

    let entry = std::fs::symlink_metadata(path).ok()?;
    if !entry.is_file() {
        return None;
    }
    let file = crate::download::open_nonblocking(path).ok()?;
    let held = file.metadata().ok()?;
    (held.is_file() && held.dev() == entry.dev() && held.ino() == entry.ino()).then_some(file)
}

#[cfg(unix)]
impl DigestReader {
    /// `{dir}/{hex}` opened to replay a body of `len` bytes: the entry itself must be a regular
    /// file (a link is not followed, and a FIFO is opened without waiting, then refused), and
    /// the open handle must be that very file.
    ///
    /// # Errors
    ///
    /// `Replay::StreamDigest` when the file is missing or is not a regular file.
    pub(crate) fn open(
        dir: &Path,
        hex: &str,
        len: u64,
        latch: Arc<Mutex<Option<Error>>>,
    ) -> Result<DigestReader> {
        let file =
            open_regular(&dir.join(hex)).ok_or_else(|| stream_digest(dir, hex, "is missing"))?;
        Ok(DigestReader {
            file,
            hasher: Some(aterm_digest::Sha256::new()),
            dir: dir.to_path_buf(),
            hex: hex.to_string(),
            len,
            seen: 0,
            latch,
        })
    }
}

#[cfg(unix)]
impl std::io::Read for DigestReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(hasher) = self.hasher.as_mut() else {
            return Ok(0);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        // At most the taped length is given; past it, one byte more is read to see whether the
        // file ends there. So a longer file is judged at the taped length, before a reader that
        // stops one byte past it (`client.rs`'s `Bounded`) refuses it as too large.
        let left = usize::try_from(self.len.saturating_sub(self.seen)).unwrap_or(usize::MAX);
        let mut more = [0u8; 1];
        let into = if left == 0 {
            more.as_mut_slice()
        } else {
            let end = left.min(buf.len());
            buf.get_mut(..end).unwrap_or_default()
        };
        let n = self.file.read(into)?;
        if n > 0 && left > 0 {
            hasher.update(into.get(..n).unwrap_or_default());
            self.seen = self
                .seen
                .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
            return Ok(n);
        }
        let Some(hasher) = self.hasher.take() else {
            return Ok(0);
        };
        let hex = aterm_codec::hex::encode_lower(&hasher.finalize());
        if n == 0 && hex == self.hex && self.seen == self.len {
            return Ok(0);
        }
        let e = stream_digest(
            &self.dir,
            &self.hex,
            "holds other bytes than the tape names",
        );
        let why = e.why.clone();
        self.latch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(e);
        Err(std::io::Error::other(why))
    }
}

/// `bytes` through `Redactor::shapes`: each valid UTF-8 run shaped, each invalid byte kept as it is.
pub(crate) fn shaped(bytes: &[u8]) -> Vec<u8> {
    per_run(bytes, Redactor::shapes)
}

/// `bytes` with each valid UTF-8 run through `f` and each invalid byte kept as it is.
fn per_run(bytes: &[u8], f: impl Fn(&str) -> String) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.extend_from_slice(f(chunk.valid()).as_bytes());
        out.extend_from_slice(chunk.invalid());
    }
    out
}

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

/// Whether the header item a sink would write for `header` holds a line the shapes would change: two arguments that read as one shape once joined (`Bearer` then a long word).
pub(crate) fn head_redacts(header: &TapeHeader) -> bool {
    let head = head_text(&shaped_header(header));
    Redactor::shapes(&head) != head
}

/// Why a credentialed sink refuses its header item (`TapeSink::create_redacted`, `finish`).
const HEAD_CREDENTIAL: &str = "its command line holds a credential this run used";

/// Whether `head`, a header item as written, holds a credential `redactor` learned: one with no
/// token shape (a legacy classic token is any long printable run) passes the shapes the header
/// was written through.
fn head_holds_a_credential(head: &str, redactor: &Redactor) -> bool {
    redactor.apply(head) != head
}

/// `bytes` through `redactor.apply` (the learned credentials, then the shapes), each valid UTF-8
/// run redacted and each invalid byte kept; with no redactor, `shaped`.
fn redacted(bytes: &[u8], redactor: Option<&Redactor>) -> Vec<u8> {
    match redactor {
        Some(redactor) => per_run(bytes, |text| redactor.apply(text)),
        None => shaped(bytes),
    }
}

fn redacted_record(record: &EffectRecord, redactor: Option<&Redactor>) -> EffectRecord {
    match record {
        EffectRecord::Exchange {
            door,
            request,
            reply,
        } => EffectRecord::Exchange {
            door: door.clone(),
            request: redacted(request, redactor),
            reply: redacted(reply, redactor),
        },
        EffectRecord::Emit { door, bytes } => EffectRecord::Emit {
            door: door.clone(),
            bytes: redacted(bytes, redactor),
        },
        other => other.clone(),
    }
}

/// Whether the shapes would change `bytes` on the tape (`shaped`): a canonical request a tape of which could not replay, because the replay asks the unredacted question.
pub(crate) fn redacts(bytes: &[u8]) -> bool {
    shaped(bytes) != bytes
}

/// Whether the shapes would change the canonical reply `reply` (`canon::reply`'s bytes), or any string of the JSON text its body holds, read decoded.
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::error::Sub;

    #[test]
    fn a_tape_cut_short_by_a_write_error_takes_its_bodies_with_it() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("agh-tape-")
            .tempdir()
            .expect("a scratch directory");
        let path = dir.path().join("t.atape");
        let argv = ["release".to_string(), "download".to_string()];
        let header = TapeHeader::for_run(&argv, &crate::wire::Endpoints::github_com());
        let sink = TapeSink::create(&path, &header).expect("a sink");
        let stored = sink
            .store_body(0, &mut &b"the bytes of a body"[..], 1 << 20)
            .expect("stored");
        let body = sidecar_dir(&path).join(&stored.hex);
        assert!(body.exists());
        // The tape's handle made read-only: the next append fails to write, as on a full disk.
        sink.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file = std::fs::File::open(&path).expect("the tape, read-only");
        sink.append(&EffectRecord::Clock(1));
        let refused = sink.finish().expect_err("a write error");
        assert_eq!(refused.sub, Sub::Local);
        assert!(path.exists(), "the partial tape is kept");
        assert!(!body.exists(), "its body is removed");
        assert!(!sidecar_dir(&path).exists());
    }
}
