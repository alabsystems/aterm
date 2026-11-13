// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Codex home, rollout file discovery, and turn token reader.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aterm_json::Value;

/// Whether `s` is a valid hyphenated UUID thread ID (`8-4-4-4-12` hex digits).
#[must_use]
pub fn is_thread_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| aterm_codec::hex::is_hex(p, n))
}

/// Extracts the creation timestamp in unix milliseconds from a UUIDv7 thread ID.
#[must_use]
pub fn thread_created_ms(thread: &str) -> Option<u64> {
    if !is_thread_id(thread) || thread.as_bytes().get(14) != Some(&b'7') {
        return None;
    }
    let hex: String = thread.split('-').take(2).collect();
    u64::from_str_radix(&hex, 16).ok()
}

/// Whether `name` is a rollout filename for `thread` (`rollout-<stamp>-<thread>.jsonl`).
#[must_use]
pub fn is_rollout_of(name: &str, thread: &str) -> bool {
    name.starts_with("rollout-")
        && name
            .strip_suffix(".jsonl")
            .and_then(|stem| stem.strip_suffix(thread))
            .is_some_and(|prefix| prefix.ends_with('-'))
}

/// Resolves `$CODEX_HOME` for a Codex process started with `env` in `cwd`.
#[must_use]
pub fn codex_home(
    codex_home: Option<&str>,
    home: Option<&str>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    match codex_home.filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => Some(PathBuf::from(set)),
        Some(relative) => cwd.filter(|c| c.is_absolute()).map(|c| c.join(relative)),
        None => home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|h| h.join(".codex")),
    }
}

/// Lists all rollout files under `home/sessions/<y>/<m>/<d>/`, newest mtime first.
#[must_use]
pub fn rollouts_by_write(home: &Path) -> Vec<(PathBuf, SystemTime)> {
    rollout_walk(home)
        .listed
        .into_iter()
        .map(|l| (l.path, l.at))
        .collect()
}

/// [`rollouts_by_write`] with each rollout's size, what says it is as a look last found it, and
/// whether every directory and entry on the way could be read. A link's size and time are its
/// target's, since a read of it reads the target: newest first by those.
#[must_use]
pub fn rollouts_by_write_sized(home: &Path) -> (Vec<(PathBuf, SystemTime, u64)>, bool) {
    let walk = rollout_walk(home);
    let mut whole = walk.whole && !walk.unexamined;
    let mut listed: Vec<_> = walk
        .listed
        .into_iter()
        .filter_map(|l| {
            if !l.meta.file_type().is_symlink() {
                let size = l.meta.len();
                return Some((l.path, l.at, size));
            }
            match std::fs::metadata(&l.path).and_then(|m| m.modified().map(|at| (m.len(), at))) {
                Ok((size, at)) => Some((l.path, at, size)),
                Err(_) => {
                    whole = false;
                    None
                }
            }
        })
        .collect();
    listed.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    (listed, whole)
}

/// One rollout a [`rollout_walk`] found.
struct Listed {
    path: PathBuf,
    /// Its directory entry's metadata (a link is not followed).
    meta: std::fs::Metadata,
    /// Its mtime.
    at: SystemTime,
}

/// What a [`rollout_walk`] found.
struct Walk {
    /// Newest mtime first, ties by path, last first.
    listed: Vec<Listed>,
    /// Every directory and entry under `sessions/` could be read (an absent `sessions/` reads whole).
    whole: bool,
    /// A link so named has a target that could not be examined. It may be a rollout, which matters to
    /// a look for a wall. A reader of turns cannot open it either, so it holds none that reader can count.
    unexamined: bool,
}

/// Every `rollout-*.jsonl` under `home/sessions/<y>/<m>/<d>/` with its metadata, and whether nothing on the way could not be read.
fn rollout_walk(home: &Path) -> Walk {
    fn subdirs(dir: &Path, top: bool, whole: &mut bool) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                *whole &= top && e.kind() == std::io::ErrorKind::NotFound;
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for e in entries {
            match e.and_then(|e| e.file_type().map(|t| (e, t))) {
                Ok((e, t)) if t.is_dir() || (t.is_symlink() && e.path().is_dir()) => {
                    out.push(e.path());
                }
                Ok(_) => {}
                Err(_) => *whole = false,
            }
        }
        out
    }
    let mut whole = true;
    let mut unexamined = false;
    let mut listed = Vec::new();
    for year in subdirs(&home.join("sessions"), true, &mut whole) {
        for month in subdirs(&year, false, &mut whole) {
            for day in subdirs(&month, false, &mut whole) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    whole = false;
                    continue;
                };
                for e in entries {
                    let Ok(e) = e else {
                        whole = false;
                        continue;
                    };
                    let file_name = e.file_name();
                    let name = file_name.to_string_lossy();
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    match e.metadata().and_then(|m| m.modified().map(|at| (m, at))) {
                        // A regular file, or a link to one: a directory, a dangling link or
                        // another kind of entry so named is no rollout, and nothing to read. A
                        // target that could not be examined may be one.
                        Ok((meta, at)) => {
                            let rollout = if meta.file_type().is_symlink() {
                                match std::fs::metadata(e.path()) {
                                    Ok(target) => target.is_file(),
                                    Err(why) => {
                                        unexamined |= why.kind() != std::io::ErrorKind::NotFound;
                                        false
                                    }
                                }
                            } else {
                                meta.is_file()
                            };
                            if rollout {
                                listed.push(Listed {
                                    path: e.path(),
                                    meta,
                                    at,
                                });
                            }
                        }
                        Err(_) => whole = false,
                    }
                }
            }
        }
    }
    listed.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.path.cmp(&a.path)));
    Walk {
        listed,
        whole,
        unexamined,
    }
}

/// The identity `meta` carries, where the platform keeps one in a file's metadata (Unix); `None` elsewhere, where only an open file tells it.
#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity_of(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Reads the last `bytes` of `path`, dropping a partial leading line.
#[must_use]
pub fn tail_of(path: &Path, bytes: u64) -> Option<String> {
    tail_after(path, 0, bytes)
}

/// Reads up to the last `bytes` of `path` starting no earlier than line-aligned `floor`.
pub(crate) fn tail_after(path: &Path, floor: u64, bytes: u64) -> Option<String> {
    let (mut f, meta) = crate::harness::footer::open_regular_with_meta(path)?;
    tail_after_file_with_len(&mut f, meta.len(), floor, bytes)
}

/// Reads up to the last `bytes` of open `f` starting no earlier than line-aligned `floor`.
pub(crate) fn tail_after_file(f: &mut std::fs::File, floor: u64, bytes: u64) -> Option<String> {
    let len = f.metadata().ok()?.len();
    tail_after_file_with_len(f, len, floor, bytes)
}

fn tail_after_file_with_len(
    f: &mut std::fs::File,
    len: u64,
    floor: u64,
    bytes: u64,
) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let from = len.saturating_sub(bytes).max(floor);
    f.seek(SeekFrom::Start(from)).ok()?;
    let want = usize::try_from(len.saturating_sub(from).min(bytes)).unwrap_or(0);
    let mut buf = Vec::with_capacity(want.min(512 * 1024).saturating_add(1));
    f.take(bytes).read_to_end(&mut buf).ok()?;
    if from > floor {
        let Some(nl) = buf.iter().position(|&b| b == b'\n') else {
            return Some(String::new());
        };
        buf.drain(..=nl);
    }
    Some(match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Maximum timestamp gap (ms) between consecutive lines in a legacy fork's copied prefix.
pub const FORK_BURST_MS: i64 = 500;

/// `session_meta` `history_mode` value for threads whose forks copy no token counts.
pub const PAGINATED: &str = "paginated";

/// Per-turn token usage counts (`last_token_usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnTokens {
    /// `input_tokens`.
    pub input: u64,
    /// `cached_input_tokens`.
    pub cached: u64,
    /// `cache_write_input_tokens`.
    pub cache_write: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `reasoning_output_tokens`.
    pub reasoning: u64,
}

impl TurnTokens {
    fn of(v: Option<&Value>) -> Option<Self> {
        let o = v?.as_object()?;
        let n = |key: &str| o.get(key).and_then(Value::as_u64);
        if n(KEY_INPUT_TOKENS).is_none() && n(KEY_OUTPUT_TOKENS).is_none() {
            return None;
        }
        Some(Self {
            input: n(KEY_INPUT_TOKENS).unwrap_or(0),
            cached: n("cached_input_tokens").unwrap_or(0),
            cache_write: n("cache_write_input_tokens").unwrap_or(0),
            output: n(KEY_OUTPUT_TOKENS).unwrap_or(0),
            reasoning: n("reasoning_output_tokens").unwrap_or(0),
        })
    }
}

/// Single served turn extracted from a rollout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// `token_count` timestamp in unix seconds.
    pub at: i64,
    /// Model from the latest `turn_context` preceding this turn.
    pub model: Option<String>,
    /// Turn token usage (`last_token_usage`).
    pub tokens: TurnTokens,
    /// Whether the preceding `turn_context` named a non-standard service tier (`fast` or `flex`).
    pub tier: bool,
    /// FNV-1a/64 hash of the running total and turn counts within this rollout.
    pub key: u64,
}

/// Position of a [`TurnReader`] relative to a fork's copied prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefix {
    /// No line read yet.
    #[default]
    Start,
    /// Inside a fork's copied prefix, holding the last copied line's timestamp (ms).
    Copy(i64),
    /// Past any copied prefix; all turns belong to this rollout.
    Own,
}

/// Classification of a single rollout line by [`TurnReader::line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// Served turn belonging to this rollout.
    Turn(Turn),
    /// `token_count` whose running total did not increase.
    Repeat,
    /// `token_count` inside a fork's copied prefix.
    Copied,
    /// Turn without a valid timestamp.
    Untimed,
    /// Malformed JSON line or unreadable `token_count` payload.
    Unparsed,
    /// Any other rollout record.
    Other,
}

/// Incremental line-by-line spend reader for a single Codex rollout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnReader {
    /// Current prefix state.
    pub prefix: Prefix,
    /// Highest `total_token_usage.total_tokens` seen so far (including any copied prefix).
    pub top: u64,
    /// Latest `turn_context` model seen past the prefix.
    pub model: Option<String>,
    /// Whether the latest `turn_context` named a non-standard service tier.
    pub tier: bool,
    /// Fork's own thread ID while scanning a legacy fork's copied prefix.
    pub thread: Option<String>,
}

/// Parses an RFC 3339 rollout timestamp into unix milliseconds.
pub(crate) fn stamp_ms(stamp: &str) -> Option<i64> {
    let secs = aterm_types::rfc3339::parse_utc_fractional(stamp)?;
    let millis = stamp
        .strip_suffix('Z')
        .and_then(|head| head.get(19..))
        .and_then(|fraction| fraction.strip_prefix('.'))
        .map_or(0, |digits| {
            digits
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(3)
                .fold(0_i64, |ms, d| ms * 10 + i64::from(d.wrapping_sub(b'0')))
        });
    secs.checked_mul(1000)?.checked_add(millis)
}

fn turn_key(total: &TurnTokens, total_tokens: u64, last: &TurnTokens) -> u64 {
    let text = format!(
        "{},{},{},{},{},{total_tokens};{},{},{},{},{}",
        total.input,
        total.cached,
        total.cache_write,
        total.output,
        total.reasoning,
        last.input,
        last.cached,
        last.cache_write,
        last.output,
        last.reasoning,
    );
    aterm_hash::fnv1a_64(text.as_bytes())
}

// The keys of a `token_count` payload the counts are read from, as the anchors name them (the
// Codex step fold's token counts are these: `lab codex-measure` refuses GONE when a build drops
// one).
const KEY_INFO: &str = aterm_phase::anchor!("rollout.info");
const KEY_TOTAL_USAGE: &str = aterm_phase::anchor!("rollout.total_token_usage");
const KEY_LAST_USAGE: &str = aterm_phase::anchor!("rollout.last_token_usage");
const KEY_TOTAL_TOKENS: &str = aterm_phase::anchor!("rollout.total_tokens");
const KEY_INPUT_TOKENS: &str = aterm_phase::anchor!("rollout.input_tokens");
const KEY_OUTPUT_TOKENS: &str = aterm_phase::anchor!("rollout.output_tokens");

/// Extracts `(last_token_usage, total_tokens)` from a `token_count` payload.
pub(crate) fn counts_of(payload: Option<&Value>) -> Option<(TurnTokens, u64, TurnTokens)> {
    let info = payload?.get(KEY_INFO).filter(|i| i.is_object())?;
    let total_usage = info.get(KEY_TOTAL_USAGE);
    let total = TurnTokens::of(total_usage)?;
    let last = TurnTokens::of(info.get(KEY_LAST_USAGE))?;
    let total_tokens = total_usage
        .and_then(|t| t.get(KEY_TOTAL_TOKENS))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| total.input.saturating_add(total.output));
    Some((total, total_tokens, last))
}

impl TurnReader {
    /// Processes the next line of the rollout and returns its [`Row`] classification.
    pub fn line(&mut self, line: &str) -> Row {
        let line = line.trim();
        if line.is_empty() {
            return Row::Other;
        }
        let counts = line.contains("\"token_count\"");
        if self.prefix == Prefix::Own && !counts && !line.contains("\"turn_context\"") {
            return Row::Other;
        }
        let Ok(v) = crate::harness::transcript::codex_spend(line) else {
            return Row::Unparsed;
        };
        let word = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
        let kind = word(v.get("type"));
        let payload = v.get("payload");
        let is_count = kind.as_deref() == Some("event_msg")
            && word(payload.and_then(|p| p.get("type"))).as_deref() == Some("token_count");
        let stamp = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(stamp_ms);
        let copied = |reader: &mut Self| {
            if !is_count {
                return Row::Other;
            }
            if let Some((_, total_tokens, _)) = counts_of(payload) {
                reader.top = reader.top.max(total_tokens);
            }
            Row::Copied
        };
        match self.prefix {
            Prefix::Start => {
                let fork = word(payload.and_then(|p| p.get("forked_from_id")))
                    .is_some_and(|parent| !parent.is_empty());
                let paginated =
                    word(payload.and_then(|p| p.get("history_mode"))).as_deref() == Some(PAGINATED);
                if kind.as_deref() == Some("session_meta") && (!fork || paginated) {
                    self.prefix = Prefix::Own;
                    return Row::Other;
                }
                if kind.as_deref() == Some("session_meta") {
                    self.thread =
                        word(payload.and_then(|p| p.get("id"))).filter(|id| is_thread_id(id));
                }
                if let Some(at) = stamp {
                    self.prefix = Prefix::Copy(at);
                }
                return copied(self);
            }
            Prefix::Copy(newest) => {
                let own_settings = kind.as_deref() == Some("event_msg")
                    && word(payload.and_then(|p| p.get("type"))).as_deref()
                        == Some("thread_settings_applied")
                    && self.thread.is_some()
                    && word(payload.and_then(|p| p.get("thread_id"))) == self.thread;
                match stamp {
                    _ if own_settings => self.prefix = Prefix::Own,
                    Some(at) if at.saturating_sub(newest) > FORK_BURST_MS => {
                        self.prefix = Prefix::Own;
                    }
                    Some(at) => {
                        self.prefix = Prefix::Copy(newest.max(at));
                        return copied(self);
                    }
                    None => return copied(self),
                }
                self.thread = None;
            }
            Prefix::Own => {}
        }
        if kind.as_deref() == Some("turn_context") {
            self.model = word(payload.and_then(|p| p.get("model")))
                .filter(|m| crate::harness::upgrade::is_model_id(m));
            self.tier = word(payload.and_then(|p| p.get("service_tier")))
                .is_some_and(|t| !matches!(t.as_str(), "" | "standard" | "default" | "auto"));
            return Row::Other;
        }
        if !is_count {
            return Row::Other;
        }
        if !payload
            .and_then(|p| p.get("info"))
            .is_some_and(Value::is_object)
        {
            return Row::Other;
        }
        let Some((total, total_tokens, last)) = counts_of(payload) else {
            return Row::Unparsed;
        };
        if total_tokens <= self.top {
            return Row::Repeat;
        }
        self.top = total_tokens;
        let Some(at) = stamp else {
            return Row::Untimed;
        };
        Row::Turn(Turn {
            at: at.div_euclid(1000),
            model: self.model.clone(),
            tokens: last,
            tier: self.tier,
            key: turn_key(&total, total_tokens, &last),
        })
    }
}

/// Summary of a [`read_rollout`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RolloutRead {
    /// Total bytes read.
    pub read: u64,
    /// Whether the rollout was read to EOF without hitting the byte cap or an I/O error.
    pub complete: bool,
    /// Whether an I/O error ended the read, which reaching the byte cap does not.
    pub failed: bool,
    /// Number of lines exceeding [`crate::harness::usage::MAX_LINE_BYTES`] skipped unread.
    pub long: u64,
}

/// Reads up to `cap` bytes of `path` from the start through `reader` (`TurnReader`).
pub fn read_rollout(
    path: &Path,
    reader: &mut TurnReader,
    cap: u64,
    mut each: impl FnMut(Row),
) -> Option<RolloutRead> {
    stream_lines(path, cap, |_, text| {
        each(text.map_or(Row::Unparsed, |text| reader.line(text)));
        std::ops::ControlFlow::Continue(())
    })
}

/// Streams complete lines of `path` up to `cap` bytes into `line`.
pub(crate) fn stream_lines(
    path: &Path,
    cap: u64,
    mut each: impl FnMut(u64, Option<&str>) -> std::ops::ControlFlow<()>,
) -> Option<RolloutRead> {
    stream_lines_ends(path, cap, |start, _, line| match line {
        Streamed::Line(text) => each(start, text),
        Streamed::Long => std::ops::ControlFlow::Continue(()),
    })
}

/// Single line item yielded by [`stream_lines_ends`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Streamed<'a> {
    /// Complete line (`Some` if valid UTF-8, `None` otherwise).
    Line(Option<&'a str>),
    /// Complete line exceeding [`crate::harness::usage::MAX_LINE_BYTES`], skipped unread.
    Long,
}

/// Streams complete lines of `path` with their `(start, end)` byte offsets.
pub(crate) fn stream_lines_ends(
    path: &Path,
    cap: u64,
    each: impl FnMut(u64, u64, Streamed<'_>) -> std::ops::ControlFlow<()>,
) -> Option<RolloutRead> {
    let file = crate::harness::footer::open_regular(path)?;
    Some(stream_open_ends(file, 0, None, cap, each))
}

/// Streams complete lines from open `file` starting at byte offset `from`.
pub(crate) fn stream_open_ends(
    file: std::fs::File,
    from: u64,
    skip: Option<u64>,
    cap: u64,
    each: impl FnMut(u64, u64, Streamed<'_>) -> std::ops::ControlFlow<()>,
) -> RolloutRead {
    stream_open_ends_ref(&file, from, skip, cap, each)
}

/// Streams complete lines from borrowed `file` starting at byte offset `from`.
pub(crate) fn stream_open_ends_ref(
    file: &std::fs::File,
    from: u64,
    skip: Option<u64>,
    cap: u64,
    mut each: impl FnMut(u64, u64, Streamed<'_>) -> std::ops::ControlFlow<()>,
) -> RolloutRead {
    use std::io::{BufRead as _, Seek as _};
    let max_line = crate::harness::usage::MAX_LINE_BYTES;
    let mut file = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut out = RolloutRead::default();
    if file.seek(std::io::SeekFrom::Start(from)).is_err() {
        out.failed = true;
        return out;
    }
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = skip.is_some();
    let mut counted = false;
    let mut start = skip.unwrap_or(from);
    loop {
        let buf = match file.fill_buf() {
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                out.failed = true;
                return out;
            }
        };
        if buf.is_empty() {
            break;
        }
        if out.read >= cap {
            return out;
        }
        let room = usize::try_from(cap - out.read).unwrap_or(usize::MAX);
        let chunk = &buf[..buf.len().min(room)];
        let newline = chunk.iter().position(|&b| b == b'\n');
        let used = newline.map_or(chunk.len(), |i| i + 1);
        if !skipping {
            line.extend_from_slice(&chunk[..newline.unwrap_or(used)]);
            if line.len() > max_line {
                out.long += 1;
                counted = true;
                // Drop the oversized allocation before skipping the rest of the row.
                drop(std::mem::take(&mut line));
                skipping = true;
            }
        }
        file.consume(used);
        out.read += used as u64;
        if newline.is_some() {
            let end = from.saturating_add(out.read);
            let flow = if skipping {
                if !counted {
                    out.long += 1;
                }
                each(start, end, Streamed::Long)
            } else {
                each(start, end, Streamed::Line(std::str::from_utf8(&line).ok()))
            };
            line.clear();
            skipping = false;
            counted = false;
            start = end;
            if flow.is_break() {
                return out;
            }
        }
    }
    out.complete = true;
    out
}

/// How far one rollout has been read (design §4.14 "Tokens"): the file's identity, where the next unread line starts, and the reader standing where those bytes left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RolloutCursor {
    /// The file read (device, file number); `None` before its first read, or where the platform tells none.
    identity: Option<(u64, u64)>,
    /// Where the next unread line starts: 0, or just past a newline.
    pos: u64,
    /// Its size and mtime (ns) at the last read.
    size: u64,
    mtime_ns: i64,
    /// The last read reached the end the file had (a torn last line aside).
    end: bool,
    /// The reader, standing at `pos`.
    reader: TurnReader,
}

impl RolloutCursor {
    /// Read what `path` appended since the last call, handing `each` every turn of this rollout's own (`Row::Turn`). It reads whole lines until it has read `cap` bytes, and the line it is in then to that line's end (a line past [`crate::harness::usage::MAX_LINE_BYTES`] is skipped unparsed), so a read can pass `cap` by one line; a `cap` of 0 reads no line. A file replaced (another identity), shorter than the cursor, or whose byte before the cursor is no longer a newline is read again from its start with a fresh reader that keeps `top`, so turns it already handed read `Repeat`. A torn last line is read again next time. `read` counts the byte before the cursor it checks; `complete` says the read reached the end the file had when opened. `None` when it cannot be opened.
    pub fn advance(
        &mut self,
        path: &Path,
        cap: u64,
        each: &mut dyn FnMut(&Turn),
    ) -> Option<RolloutRead> {
        use std::io::{Read as _, Seek as _};
        let (mut file, meta) = crate::harness::footer::open_regular_with_meta(path)?;
        let size = meta.len();
        let id = crate::harness::session_usage::open_identity(&file, &meta);
        let mut out = RolloutRead::default();
        let mut replaced = matches!((self.identity, id), (Some(held), Some(now)) if held != now)
            || size < self.pos;
        if let (false, Some(at)) = (replaced, self.pos.checked_sub(1)) {
            let mut byte = [0u8; 1];
            let ok = file.seek(std::io::SeekFrom::Start(at)).is_ok()
                && matches!(file.read(&mut byte), Ok(1));
            out.read = 1;
            replaced = !ok || byte[0] != b'\n';
        }
        if replaced {
            self.restart();
        }
        self.identity = id;
        let from = self.pos;
        let mut reached = from >= size;
        if cap > 0 {
            let reader = &mut self.reader;
            let mut last = None;
            let streamed = stream_open_ends_ref(&file, from, None, u64::MAX, |_, end, line| {
                if let Streamed::Line(Some(text)) = line
                    && let Row::Turn(turn) = reader.line(text)
                {
                    each(&turn);
                }
                last = Some(end);
                if end.saturating_sub(from) >= cap {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            });
            self.pos = last.unwrap_or(from);
            out.read = out.read.saturating_add(streamed.read);
            out.long = streamed.long;
            reached = from.saturating_add(streamed.read) >= size;
        }
        self.size = size;
        self.mtime_ns = mtime_ns(&meta);
        self.end = reached;
        out.complete = reached;
        Some(out)
    }

    /// Start over at the file's start with a fresh reader that keeps the running total it reached.
    fn restart(&mut self) {
        *self = Self {
            reader: TurnReader {
                top: self.reader.top,
                ..TurnReader::default()
            },
            ..Self::default()
        };
    }

    /// The last read reached the end of a file that still has this identity, size and mtime.
    fn unchanged(&self, meta: &std::fs::Metadata) -> bool {
        self.end
            && identity_of(meta).is_none_or(|id| self.identity == Some(id))
            && self.size == meta.len()
            && self.mtime_ns == mtime_ns(meta)
    }
}

/// The most rollouts one [`Rollouts`] follows; past it a new one is not read, and the fold is not caught up.
pub const MAX_ROLLOUTS: usize = 16_384;

/// The rollouts of one Codex home a long-lived reader follows, each with its own cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rollouts {
    /// Each rollout's cursor, by its path.
    files: std::collections::BTreeMap<PathBuf, RolloutCursor>,
}

impl Rollouts {
    /// Fold what each rollout under `home`'s `sessions/` modified at or after `since` (Unix seconds) appended since the last fold, newest first, handing `each` the rollout's path and each turn served at or after `since`. It reads whole lines until `cap` bytes in all are read, and the line it is in then to that line's end ([`RolloutCursor::advance`]); a rollout with the identity, size and mtime its last read left, read to its end, is not opened. Cursors of rollouts gone, or older than `since`, are dropped once every directory could be listed. Answers whether every rollout it could open was read to the end it had (a torn last line aside), with every directory listed and none left unfollowed at [`MAX_ROLLOUTS`].
    pub fn fold(
        &mut self,
        home: &Path,
        since: i64,
        cap: u64,
        each: &mut dyn FnMut(&Path, &Turn),
    ) -> bool {
        let walk = rollout_walk(home);
        let mut caught_up = walk.whole;
        let recent: Vec<Listed> = walk
            .listed
            .into_iter()
            .filter(|l| mtime_ns(&l.meta) / 1_000_000_000 >= since)
            .collect();
        if walk.whole {
            let there: std::collections::BTreeSet<&Path> =
                recent.iter().map(|l| l.path.as_path()).collect();
            self.files.retain(|path, _| there.contains(path.as_path()));
        }
        let mut left = cap;
        for listed in &recent {
            if !self.files.contains_key(&listed.path) && self.files.len() >= MAX_ROLLOUTS {
                caught_up = false;
                continue;
            }
            let cursor = self.files.entry(listed.path.clone()).or_default();
            if cursor.unchanged(&listed.meta) {
                continue;
            }
            if left == 0 {
                caught_up = false;
                continue;
            }
            let path = listed.path.as_path();
            let read = cursor.advance(path, left, &mut |turn| {
                if turn.at >= since {
                    each(path, turn);
                }
            });
            // A rollout that cannot be opened (`None`) holds no turn this Mac can count, as a
            // Claude transcript that cannot be read does not (`FoldReport::unread`); its cursor
            // stays, so a later fold that opens it hands what it appended.
            if let Some(read) = read {
                left = left.saturating_sub(read.read);
                caught_up &= read.complete;
            }
        }
        caught_up
    }
}

/// Maximum bytes read from a rollout's first line when inspecting `session_meta`.
pub const META_BYTES: u64 = 1 << 20;

/// Maximum number of rollout files scanned by [`conversation_rollouts`].
pub const MAX_CONVERSATION_SCAN: usize = 16_384;

/// Rollout files belonging to a single Codex conversation under a home directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conversation {
    /// Conversation root thread ID (`session_id` from `session_meta`).
    pub root: String,
    /// Rollout file of the queried thread itself, if found.
    pub own: Option<PathBuf>,
    /// Root rollout (if found) followed by all subagent rollouts in the conversation.
    pub files: Vec<PathBuf>,
    /// Number of entries in `files` belonging to subagents.
    pub subagents: usize,
    /// Count of rollouts written since the root began whose first line could not be read.
    pub unread: usize,
}

struct MaybeBorrowedStr<'a>(Option<std::borrow::Cow<'a, str>>);

impl<'de> serde::Deserialize<'de> for MaybeBorrowedStr<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Option<std::borrow::Cow<'de, str>>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                v: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(Some(std::borrow::Cow::Borrowed(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(Some(std::borrow::Cow::Owned(v.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(Some(std::borrow::Cow::Owned(v)))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_some<D2: serde::Deserializer<'de>>(
                self,
                d: D2,
            ) -> Result<Self::Value, D2::Error> {
                d.deserialize_any(self)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(None)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(None)
            }
        }
        de.deserialize_any(V).map(Self)
    }
}

struct BorrowedKey<'a>(std::borrow::Cow<'a, str>);

impl<'de> serde::Deserialize<'de> for BorrowedKey<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = std::borrow::Cow<'de, str>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string key")
            }
            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                v: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(std::borrow::Cow::Borrowed(v))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(std::borrow::Cow::Owned(v.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(std::borrow::Cow::Owned(v))
            }
        }
        de.deserialize_str(V).map(Self)
    }
}

#[derive(Default)]
struct SessionMetaPayload<'a> {
    present: bool,
    session_id: Option<std::borrow::Cow<'a, str>>,
    id: Option<std::borrow::Cow<'a, str>>,
}

impl<'de> serde::Deserialize<'de> for SessionMetaPayload<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = SessionMetaPayload<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(SessionMetaPayload {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = SessionMetaPayload {
                    present: true,
                    ..Default::default()
                };
                while let Some(key) = map.next_key::<BorrowedKey<'de>>()? {
                    match key.0.as_ref() {
                        "session_id" => {
                            out.session_id = map.next_value::<MaybeBorrowedStr<'de>>()?.0
                        }
                        "id" => out.id = map.next_value::<MaybeBorrowedStr<'de>>()?.0,
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(out)
            }
        }
        de.deserialize_any(V)
    }
}

#[derive(Default)]
struct SessionMetaLine<'a> {
    kind: Option<std::borrow::Cow<'a, str>>,
    payload: SessionMetaPayload<'a>,
}

impl<'de> serde::Deserialize<'de> for SessionMetaLine<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = SessionMetaLine<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = SessionMetaLine::default();
                while let Some(key) = map.next_key::<BorrowedKey<'de>>()? {
                    match key.0.as_ref() {
                        "type" => out.kind = map.next_value::<MaybeBorrowedStr<'de>>()?.0,
                        "payload" => out.payload = map.next_value::<SessionMetaPayload<'de>>()?,
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(out)
            }
        }
        de.deserialize_map(V)
    }
}

type SessionMetaIds<'a> = (
    Option<std::borrow::Cow<'a, str>>,
    Option<std::borrow::Cow<'a, str>>,
);

fn parse_session_meta_ids(first_line: &str) -> Option<SessionMetaIds<'_>> {
    let parsed: SessionMetaLine<'_> = aterm_json::from_str(first_line.trim()).ok()?;
    if parsed.kind.as_deref() != Some("session_meta") || !parsed.payload.present {
        return None;
    }
    Some((parsed.payload.session_id, parsed.payload.id))
}

/// Reads the first newline-terminated line of open `file` from offset 0 into `line` up to
/// [`META_BYTES`], returning `true` when a terminating `\n` was found within the cap.
pub(crate) fn first_line_into(file: &std::fs::File, line: &mut Vec<u8>) -> bool {
    first_line_read(file, line).unwrap_or(false)
}

/// [`first_line_into`], which also tells a read that failed from a file with no such line.
///
/// # Errors
///
/// A seek or read failed, for any reason but an interruption.
pub(crate) fn first_line_read(
    mut file: &std::fs::File,
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    use std::io::{Read as _, Seek as _};
    line.clear();
    file.seek(std::io::SeekFrom::Start(0))?;
    let cap = usize::try_from(META_BYTES).unwrap_or(usize::MAX);
    let mut buf = [0u8; 65_536];
    while line.len() < cap {
        let room = (cap - line.len()).min(buf.len());
        let n = match file.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let chunk = &buf[..n];
        if let Some(nl) = chunk.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&chunk[..=nl]);
            return Ok(true);
        }
        line.extend_from_slice(chunk);
    }
    Ok(false)
}

/// Reads the first line of open `file` from offset 0 up to [`META_BYTES`].
pub(crate) fn first_line_of(file: &std::fs::File) -> Option<String> {
    let mut line = Vec::new();
    first_line_into(file, &mut line).then(|| match String::from_utf8(line) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Finds all rollout files belonging to `thread`'s conversation under `home`.
#[must_use]
pub fn conversation_rollouts(home: &Path, thread: &str) -> Conversation {
    conversation_rollouts_inner(home, thread, true)
}

/// Like [`conversation_rollouts`], skipping full JSON lexing on rollouts whose first line cannot
/// encode `root` (when `unread` is unused by the caller).
#[must_use]
pub(crate) fn conversation_rollouts_for_sample(home: &Path, thread: &str) -> Conversation {
    conversation_rollouts_inner(home, thread, false)
}

fn conversation_rollouts_inner(home: &Path, thread: &str, count_unread: bool) -> Conversation {
    let all = rollouts_by_write(home);
    fn name_of(p: &Path) -> &str {
        p.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    }
    let own = all
        .iter()
        .find(|(p, _)| is_rollout_of(name_of(p), thread))
        .map(|(p, _)| p.clone());
    let mut scratch = Vec::with_capacity(64 * 1024);
    let root = own
        .as_deref()
        .and_then(crate::harness::footer::open_regular)
        .filter(|file| first_line_into(file, &mut scratch))
        .and_then(|_| {
            let line = String::from_utf8_lossy(&scratch);
            parse_session_meta_ids(&line)?
                .0
                .filter(|s| is_thread_id(s))
                .map(std::borrow::Cow::into_owned)
        })
        .unwrap_or_else(|| thread.to_owned());
    let root_is_uuid = is_thread_id(&root);
    let since = thread_created_ms(&root)
        .map(|ms| SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(ms));
    let mut roots = Vec::new();
    let mut subagents = Vec::new();
    let mut unread = 0;
    let mut opened = 0;
    for (path, at) in all.iter().rev() {
        if since.is_some_and(|s| *at < s) {
            continue;
        }
        if is_rollout_of(name_of(path), &root) {
            roots.push(path.clone());
            continue;
        }
        opened += 1;
        if opened > MAX_CONVERSATION_SCAN {
            unread += 1;
            continue;
        }
        let Some(file) = crate::harness::footer::open_regular(path) else {
            unread += 1;
            continue;
        };
        if !first_line_into(&file, &mut scratch) {
            unread += 1;
            continue;
        }
        let line = String::from_utf8_lossy(&scratch);
        if !count_unread && root_is_uuid && !line.contains(root.as_str()) && !line.contains("\\u") {
            continue;
        }
        match parse_session_meta_ids(&line) {
            Some((Some(session), id))
                if session.as_ref() == root.as_str() && id.as_deref() != Some(root.as_str()) =>
            {
                subagents.push(path.clone());
            }
            Some(_) => {}
            None => unread += 1,
        }
    }
    let spawned = subagents.len();
    roots.extend(subagents);
    Conversation {
        root,
        own,
        files: roots,
        subagents: spawned,
        unread,
    }
}

/// Synthetic Codex rollout line builders for tests.
#[cfg(test)]
#[path = "codex_rollout_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "codex_rollout_tests.rs"]
mod tests;
