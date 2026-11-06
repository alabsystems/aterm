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

/// [`rollouts_by_write`] with each rollout's size: what says it is as a look last found it.
#[must_use]
pub fn rollouts_by_write_sized(home: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    rollout_walk(home)
        .listed
        .into_iter()
        .map(|l| {
            let size = l.meta.len();
            (l.path, l.at, size)
        })
        .collect()
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
                Ok((e, t)) if t.is_dir() => out.push(e.path()),
                Ok(_) => {}
                Err(_) => *whole = false,
            }
        }
        out
    }
    let mut whole = true;
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
                        // another kind of entry so named is no rollout, and nothing to read.
                        Ok((meta, at))
                            if meta.is_file()
                                || (meta.file_type().is_symlink()
                                    && std::fs::metadata(e.path()).is_ok_and(|m| m.is_file())) =>
                        {
                            listed.push(Listed {
                                path: e.path(),
                                meta,
                                at,
                            });
                        }
                        Ok(_) => {}
                        Err(_) => whole = false,
                    }
                }
            }
        }
    }
    listed.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.path.cmp(&a.path)));
    Walk { listed, whole }
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
            Err(_) => return out,
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
pub(crate) fn first_line_into(mut file: &std::fs::File, line: &mut Vec<u8>) -> bool {
    use std::io::{Read as _, Seek as _};
    line.clear();
    if file.seek(std::io::SeekFrom::Start(0)).is_err() {
        return false;
    }
    let cap = usize::try_from(META_BYTES).unwrap_or(usize::MAX);
    let mut buf = [0u8; 65_536];
    while line.len() < cap {
        let room = (cap - line.len()).min(buf.len());
        let n = match file.read(&mut buf[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        let chunk = &buf[..n];
        if let Some(nl) = chunk.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&chunk[..=nl]);
            return true;
        }
        line.extend_from_slice(chunk);
    }
    false
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
pub(crate) mod fixtures {
    /// Formats unix milliseconds `ms` as an RFC 3339 rollout timestamp.
    pub(crate) fn stamp(ms: i64) -> String {
        let whole = crate::harness::usage::rfc3339_utc(ms.div_euclid(1000));
        format!("{}.{:03}Z", &whole[..19], ms.rem_euclid(1000))
    }

    /// Builds a `session_meta` JSON line.
    pub(crate) fn meta(ms: i64, thread: &str, session: &str, fork: Option<&str>) -> String {
        let fork = fork.map_or_else(String::new, |f| format!(",\"forked_from_id\":\"{f}\""));
        format!(
            "{{\"timestamp\":\"{at}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\
             \"{session}\",\"id\":\"{thread}\"{fork},\"timestamp\":\"{at}\",\"cwd\":\
             \"/stand-in/project\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0\",\
             \"source\":\"cli\",\"model_provider\":\"openai\",\"base_instructions\":{{\"text\":\
             \"You are Codex (stand-in)\"}}}}}}\n",
            at = stamp(ms),
        )
    }

    /// Builds a Codex 0.163.0 `session_meta` JSON line with `history_mode` and login IDs.
    pub(crate) fn meta_as(
        ms: i64,
        thread: &str,
        session: &str,
        fork: Option<&str>,
        history: Option<&str>,
        creator: Option<(&str, &str)>,
    ) -> String {
        let mut extra = history.map_or_else(String::new, |h| format!(",\"history_mode\":\"{h}\""));
        if let Some((account, user)) = creator {
            extra.push_str(&format!(
                ",\"creator_account_id\":\"{account}\",\"creator_user_id\":\"{user}\""
            ));
        }
        meta(ms, thread, session, fork).replacen(
            ",\"timestamp\":",
            &format!("{extra},\"timestamp\":"),
            1,
        )
    }

    /// Builds a `turn_context` JSON line for `model` and optional `tier`.
    pub(crate) fn context(ms: i64, model: &str, tier: Option<&str>) -> String {
        let tier = tier.map_or_else(String::new, |t| format!(",\"service_tier\":\"{t}\""));
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\
             \"cwd\":\"/stand-in/project\",\"approval_policy\":\"never\",\"sandbox_policy\":\
             {{\"type\":\"danger-full-access\"}},\"model\":\"{model}\",\"effort\":\"high\"{tier}}}}}\n",
            stamp(ms)
        )
    }

    /// Tuple `(input_tokens, cached_input_tokens, output_tokens)`.
    pub(crate) type Counts = (u64, u64, u64);

    fn usage((input, cached, output): Counts) -> String {
        format!(
            "{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\
             \"cache_write_input_tokens\":0,\"output_tokens\":{output},\
             \"reasoning_output_tokens\":{},\"total_tokens\":{}}}",
            output / 2,
            input + output
        )
    }

    /// Builds a `token_count` JSON line with running `total` and `last` turn counts.
    pub(crate) fn count(ms: i64, total: Counts, last: Counts) -> String {
        count_with(
            ms,
            total,
            last,
            "{\"limit_id\":\"codex\",\"primary\":{\"used_percent\":12.0,\"window_minutes\":300,\
             \"resets_at\":1790000000},\"secondary\":null,\"plan_type\":\"pro\"}",
        )
    }

    /// Builds a `token_count` JSON line as [`count`] does, with `"rate_limits":null` (a login
    /// Codex is told no limits for).
    pub(crate) fn count_unlimited(ms: i64, total: Counts, last: Counts) -> String {
        count_with(ms, total, last, "null")
    }

    fn count_with(ms: i64, total: Counts, last: Counts, rate_limits: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":{{\"total_token_usage\":{},\"last_token_usage\":{},\
             \"model_context_window\":272000}},\"rate_limits\":{rate_limits}}}}}\n",
            stamp(ms),
            usage(total),
            usage(last)
        )
    }

    /// Builds a rate-limits-only `token_count` JSON line (`info: null`).
    pub(crate) fn limits_only(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":null}}}}}}\n",
            stamp(ms)
        )
    }

    /// Rate-limit slot `(used_percent, window_minutes, resets_at)`.
    pub(crate) type RateSlot = (f64, u64, i64);

    /// Builds a rate-limits-only `token_count` JSON line for `family` with two slots.
    pub(crate) fn rate(
        ms: i64,
        family: &str,
        primary: Option<RateSlot>,
        secondary: Option<RateSlot>,
    ) -> String {
        let slot = |s: Option<RateSlot>| {
            s.map_or_else(
                || "null".to_string(),
                |(used, minutes, resets)| {
                    format!(
                        "{{\"used_percent\":{used:.1},\"window_minutes\":{minutes},\
                         \"resets_at\":{resets}}}"
                    )
                },
            )
        };
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":null,\"rate_limits\":{{\"limit_id\":\"{family}\",\"limit_name\":null,\
             \"primary\":{},\"secondary\":{},\"credits\":{{\"has_credits\":true,\
             \"unlimited\":false,\"balance\":\"12\"}},\"individual_limit\":null,\
             \"spend_control_reached\":null,\"plan_type\":null,\"rate_limit_reached_type\":null}}}}}}\n",
            stamp(ms),
            slot(primary),
            slot(secondary),
        )
    }

    /// `line` (a [`rate`]) with each slot given the reset `0` naming none.
    pub(crate) fn no_reset(line: &str) -> String {
        line.replace("\"resets_at\":0}", "\"resets_at\":null}")
    }

    /// `line` (a [`rate`]) with the server's refusal in it.
    pub(crate) fn refused(line: &str) -> String {
        line.replace(
            "\"rate_limit_reached_type\":null",
            "\"rate_limit_reached_type\":\"workspace_member_usage_limit_reached\"",
        )
    }

    /// Builds a model message rollout JSON line.
    pub(crate) fn message(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done\"}}]}}}}\n",
            stamp(ms)
        )
    }

    /// Builds a `thread_settings_applied` JSON line for `thread`.
    pub(crate) fn settings(ms: i64, thread: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"thread_settings_applied\",\"thread_id\":\"{thread}\",\"thread_settings\":\
             {{\"model\":\"gpt-6.1-sol\",\"model_provider_id\":\"openai\",\
             \"approval_policy\":\"on-request\"}}}}}}\n",
            stamp(ms)
        )
    }

    /// Restamps `lines` in a 1-ms burst starting at `ms` to simulate a fork's copied prefix.
    pub(crate) fn copied(lines: &str, ms: i64) -> String {
        lines
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let at = line
                    .find("\"timestamp\":\"")
                    .map(|s| s + "\"timestamp\":\"".len())
                    .expect("a stamped line");
                let i = i64::try_from(i).expect("few lines");
                format!("{}{}{}\n", &line[..at], stamp(ms + i), &line[at + 24..])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::codex_limits::{CONTEXT_REACH, rollout_readings};

    fn unix(s: &str) -> i64 {
        aterm_types::rfc3339::parse_utc_fractional(s).expect("a stamp")
    }

    use super::fixtures::{context, copied, count, limits_only, message, meta, meta_as, rate};

    const LOGIN_A: (&str, &str) = ("acct-lab-1", "user-lab");

    const ROOT: &str = "01900000-0000-7000-8000-000000000001";

    const SUB: &str = "01900000-0000-7000-8000-000000000002";

    const OTHER: &str = "01900000-0000-7000-8000-000000000003";

    const T0: i64 = 1_790_000_000_000;

    fn rows(reader: &mut TurnReader, text: &str) -> Vec<Row> {
        text.lines().map(|l| reader.line(l)).collect()
    }

    fn turns(rows: &[Row]) -> Vec<&Turn> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Turn(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn each_served_turn_counts_once_on_its_turns_model() {
        let untimed = count(T0 + 20_000, (5000, 4000, 300), (2000, 1400, 150))
            .replace(&fixtures::stamp(T0 + 20_000), "not a stamp");
        let cut = count(T0 + 15_000, (4000, 3000, 200), (1000, 400, 50));
        let text = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 5001, (1000, 800, 50), (1000, 800, 50)),
            limits_only(T0 + 5002),
            message(T0 + 5003),
            context(T0 + 9000, "gpt-6-luna", None),
            count(T0 + 12_000, (3000, 2600, 150), (2000, 1800, 100)),
            format!("{}\n", &cut[..cut.len() / 2]),
            untimed,
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &text);
        assert_eq!(got[0], Row::Other, "a session that is no fork");
        assert_eq!(reader.prefix, Prefix::Own);
        assert_eq!(got[3], Row::Repeat);
        assert_eq!(got[4], Row::Other, "the rate limits alone");
        assert_eq!(got[8], Row::Unparsed);
        assert_eq!(got[9], Row::Untimed);
        let served = turns(&got);
        assert_eq!(served.len(), 2);
        assert_eq!(served[0].at, (T0 + 5000) / 1000);
        assert_eq!(served[0].model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(
            served[0].tokens,
            TurnTokens {
                input: 1000,
                cached: 800,
                cache_write: 0,
                output: 50,
                reasoning: 25,
            }
        );
        assert_eq!(served[1].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(served[1].tokens.input, 2000);
        assert_ne!(served[0].key, served[1].key);
        assert_eq!(reader.top, 5300, "the untimed turn still raised the total");
        let per_line = text
            .lines()
            .filter(|l| l.ends_with('}') && l.contains("\"last_token_usage\""))
            .count();
        assert_eq!(per_line, 4);
    }

    #[test]
    fn a_forks_copied_prefix_counts_nothing() {
        let parent = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
        ]
        .concat();
        let fork_at = T0 + 60_000;
        let fork = [
            meta(fork_at, SUB, ROOT, Some(ROOT)),
            copied(&parent, fork_at + 1),
            message(fork_at + 2500),
            context(fork_at + 2501, "gpt-6-luna", None),
            count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &fork);
        assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
        let served = turns(&got);
        assert_eq!(served.len(), 1, "{got:?}");
        assert_eq!(served[0].model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(served[0].tokens.input, 500);
        let parents = rows(&mut TurnReader::default(), &parent);
        assert_eq!(turns(&parents).len(), 2);
        let fork_keys: Vec<u64> = rows(
            &mut TurnReader::default(),
            &fork.replace(&format!(",\"forked_from_id\":\"{ROOT}\""), ""),
        )
        .iter()
        .filter_map(|r| match r {
            Row::Turn(t) => Some(t.key),
            _ => None,
        })
        .collect();
        assert_eq!(fork_keys.len(), 3, "the parent's two again, and its own");
        let parent_keys: Vec<u64> = turns(&parents).iter().map(|t| t.key).collect();
        assert_eq!(fork_keys[..2], parent_keys[..]);
    }

    #[test]
    fn the_copy_ends_at_the_first_gap_past_the_burst() {
        let fork = |gap: i64| {
            [
                meta(T0, SUB, ROOT, Some(ROOT)),
                count(T0 + 400, (100, 0, 10), (100, 0, 10)),
                count(T0 + 800, (200, 0, 20), (100, 0, 10)),
                count(T0 + 800 + gap, (300, 0, 30), (100, 0, 10)),
            ]
            .concat()
        };
        let edge = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS));
        assert!(turns(&edge).is_empty(), "{edge:?}");
        let past = rows(&mut TurnReader::default(), &fork(FORK_BURST_MS + 1));
        assert_eq!(turns(&past).len(), 1, "{past:?}");
        assert_eq!(turns(&past)[0].tokens.input, 100);
        let own = fork(FORK_BURST_MS).replace(&format!(",\"forked_from_id\":\"{ROOT}\""), "");
        assert_eq!(turns(&rows(&mut TurnReader::default(), &own)).len(), 3);
    }

    #[test]
    fn a_count_repeating_the_copys_total_is_no_turn() {
        let parent = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1000, "gpt-5.6-sol", None),
            count(T0 + 5000, (1000, 800, 50), (1000, 800, 50)),
            count(T0 + 9000, (3000, 2600, 150), (2000, 1800, 100)),
        ]
        .concat();
        let fork_at = T0 + 60_000;
        let own = [
            context(fork_at + 2000, "gpt-6-luna", None),
            count(fork_at + 2500, (3000, 2600, 150), (2000, 1800, 100)),
            count(fork_at + 7000, (3500, 2600, 250), (500, 0, 100)),
        ]
        .concat();
        let fork = [
            meta(fork_at, SUB, ROOT, Some(ROOT)),
            copied(&parent, fork_at + 1),
            own.clone(),
        ]
        .concat();
        let mut reader = TurnReader::default();
        let got = rows(&mut reader, &fork);
        assert_eq!(got.iter().filter(|r| **r == Row::Copied).count(), 2);
        assert_eq!(got.iter().filter(|r| **r == Row::Repeat).count(), 1);
        let served = turns(&got);
        assert_eq!(served.len(), 1, "{got:?}");
        assert_eq!(served[0].tokens.input, 500);
        assert_eq!(reader.top, 3750);
        let mut stopped = TurnReader::default();
        let lines: Vec<&str> = fork.lines().collect();
        for line in &lines[..lines.len() - 3] {
            stopped.line(line);
        }
        assert!(matches!(stopped.prefix, Prefix::Copy(_)));
        assert_eq!(stopped.top, 3150);
        let alone = [meta(fork_at, SUB, SUB, None), own].concat();
        assert_eq!(turns(&rows(&mut TurnReader::default(), &alone)).len(), 2);
    }

    #[test]
    fn a_rollout_whose_first_line_names_no_session_is_read_as_a_fork() {
        let body = [
            count(T0, (100, 0, 10), (100, 0, 10)),
            count(T0 + 30, (200, 0, 20), (100, 0, 10)),
            context(T0 + 3000, "gpt-6-sol", None),
            count(T0 + 9000, (300, 0, 30), (100, 0, 10)),
        ]
        .concat();
        let got = rows(&mut TurnReader::default(), &format!("{{\"cut\n{body}"));
        assert_eq!(got[0], Row::Unparsed);
        assert_eq!(got[1], Row::Copied);
        assert_eq!(got[2], Row::Copied);
        assert_eq!(turns(&got).len(), 1);
        assert_eq!(turns(&got)[0].model.as_deref(), Some("gpt-6-sol"));
        let plain = format!("{}{body}", meta(T0 - 10, ROOT, ROOT, None));
        assert_eq!(turns(&rows(&mut TurnReader::default(), &plain)).len(), 3);
    }

    #[test]
    fn a_service_tier_and_a_bad_model_are_read_as_they_are() {
        let text = |tier: Option<&str>, model: &str| {
            [
                meta(T0, ROOT, ROOT, None),
                context(T0 + 1, model, tier),
                count(T0 + 2, (10, 0, 1), (10, 0, 1)),
            ]
            .concat()
        };
        let turn = |tier, model| {
            let got = rows(&mut TurnReader::default(), &text(tier, model));
            turns(&got)[0].clone()
        };
        assert!(turn(Some("fast"), "gpt-6-sol").tier);
        assert!(turn(Some("flex"), "gpt-6-sol").tier);
        assert!(!turn(Some("standard"), "gpt-6-sol").tier);
        assert!(!turn(None, "gpt-6-sol").tier);
        assert_eq!(turn(None, "gpt 6 sol!").model, None);
        assert_eq!(turn(None, "gpt-6-sol").model.as_deref(), Some("gpt-6-sol"));
    }

    #[test]
    fn a_whole_read_takes_complete_lines_within_its_cap() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-usage-read-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let whole = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, "gpt-6-sol", None),
            count(T0 + 2, (10, 0, 1), (10, 0, 1)),
        ]
        .concat();
        let torn = count(T0 + 3, (20, 0, 2), (10, 0, 1));
        std::fs::write(&path, format!("{whole}{}", &torn[..torn.len() / 2])).expect("write");
        let mut n = 0;
        let read = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |r| {
            n += usize::from(matches!(r, Row::Turn(_)));
        })
        .expect("read");
        assert_eq!(n, 1, "the torn count is not read");
        assert!(read.complete);
        assert_eq!(read.read, std::fs::metadata(&path).expect("meta").len());
        let short = read_rollout(&path, &mut TurnReader::default(), 10, |_| {}).expect("read");
        assert!(!short.complete);
        assert_eq!(short.read, 10);
        let len = std::fs::metadata(&path).expect("meta").len();
        assert!(
            read_rollout(&path, &mut TurnReader::default(), len, |_| {})
                .expect("read")
                .complete
        );
        std::fs::write(&path, &whole).expect("write");
        let full = read_rollout(&path, &mut TurnReader::default(), u64::MAX, |_| {}).expect("read");
        assert!(full.complete);
        assert!(
            read_rollout(
                &dir.path().join("none"),
                &mut TurnReader::default(),
                1,
                |_| {}
            )
            .is_none()
        );
    }

    #[test]
    fn a_conversation_is_its_tuis_thread_and_the_subagents_it_spawned() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-usage-conv-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/10/15");
        std::fs::create_dir_all(&day).expect("mkdir");
        let write = |name: &str, body: String, age_s: u64| {
            let path = day.join(name);
            std::fs::write(&path, body).expect("write");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|f| {
                    f.set_modified(SystemTime::now() - std::time::Duration::from_secs(age_s))
                })
                .expect("mtime");
            path
        };
        let root = write(
            &format!("rollout-2026-10-15T10-00-00-{ROOT}.jsonl"),
            meta(T0, ROOT, ROOT, None),
            30,
        );
        let sub = write(
            &format!("rollout-2026-10-15T10-05-00-{SUB}.jsonl"),
            meta(T0, SUB, ROOT, Some(ROOT)),
            20,
        );
        let other = write(
            &format!("rollout-2026-10-15T11-00-00-{OTHER}.jsonl"),
            meta(T0, OTHER, OTHER, None),
            10,
        );
        write(
            "rollout-2026-10-15T12-00-00-cut.jsonl",
            "{\"cut".to_owned(),
            5,
        );
        for named in [SUB, ROOT] {
            let c = conversation_rollouts(home.path(), named);
            assert_eq!(c.root, ROOT, "named {named}");
            assert_eq!(c.files, [root.clone(), sub.clone()], "named {named}");
            assert_eq!(c.subagents, 1);
            assert_eq!(c.unread, 1, "the cut first line");
        }
        assert_eq!(
            conversation_rollouts(home.path(), SUB).own.as_deref(),
            Some(sub.as_path())
        );
        let c = conversation_rollouts(home.path(), OTHER);
        assert_eq!(c.root, OTHER);
        assert_eq!(c.files, [other]);
        assert_eq!(c.subagents, 0);
        let none = conversation_rollouts(home.path(), "01900000-0000-7000-8000-00000000000f");
        assert!(none.own.is_none() && none.files.is_empty());
    }

    #[test]
    fn a_stamp_reads_to_the_millisecond() {
        let s = unix("2026-08-01T17:36:47Z");
        assert_eq!(stamp_ms("2026-08-01T17:36:47.998Z"), Some(s * 1000 + 998));
        assert_eq!(stamp_ms("2026-08-01T17:36:47.9Z"), Some(s * 1000 + 900));
        assert_eq!(
            stamp_ms("2026-08-01T17:36:47.123456Z"),
            Some(s * 1000 + 123)
        );
        assert_eq!(stamp_ms("2026-08-01T17:36:47Z"), Some(s * 1000));
        assert_eq!(fixtures::stamp(s * 1000 + 7), "2026-08-01T17:36:47.007Z");
        assert_eq!(stamp_ms("2026-08-01T17:36:47.1+00:00"), None);
    }

    #[test]
    fn a_paginated_forks_own_first_turn_counts_on_its_model() {
        let spawn = |history: &str| {
            [
                meta_as(T0, SUB, ROOT, Some(ROOT), Some(history), Some(LOGIN_A)),
                meta_as(T0, ROOT, ROOT, None, Some(history), Some(LOGIN_A)),
                context(T0, "gpt-6-luna", None),
                message(T0),
                context(T0, "gpt-6-luna", None),
                message(T0),
                context(T0 + 9, "gpt-6.1-sol", None),
                message(T0 + 10),
                count(T0 + 24, (15_565, 0, 12), (15_565, 0, 12)),
            ]
            .concat()
        };
        let got = rows(&mut TurnReader::default(), &spawn(PAGINATED));
        let t = turns(&got);
        assert_eq!(t.len(), 1, "{got:?}");
        assert_eq!(t[0].model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(t[0].tokens.input, 15_565);
        let user_fork = [
            meta_as(T0, OTHER, OTHER, Some(ROOT), Some(PAGINATED), Some(LOGIN_A)),
            message(T0),
            context(T0 + 14_000, "gpt-6.1-sol", None),
            count(T0 + 14_010, (47_270, 0, 33), (15_860, 0, 10)),
        ]
        .concat();
        for history in [PAGINATED, "legacy"] {
            let text = user_fork.replace(PAGINATED, history);
            assert_eq!(turns(&rows(&mut TurnReader::default(), &text)).len(), 1);
        }
        let legacy = rows(&mut TurnReader::default(), &spawn("legacy"));
        assert!(turns(&legacy).is_empty(), "{legacy:?}");
        assert_eq!(legacy.last(), Some(&Row::Copied));
    }

    #[test]
    fn a_legacy_forks_own_settings_line_ends_its_copy() {
        use super::fixtures::settings;
        let reset = T0 / 1000 + 3600;
        let prompted = |own: &str| {
            [
                meta_as(T0, SUB, SUB, Some(ROOT), Some("legacy"), Some(LOGIN_A)),
                meta(T0, ROOT, ROOT, None),
                context(T0, "gpt-6-luna", None),
                message(T0),
                count(T0, (6_000, 0, 30), (3_000, 0, 10)),
                rate(T0, "codex", Some((43.0, 300, reset)), None),
                settings(T0, ROOT),
                settings(T0, own),
                settings(T0 + 14, own),
                context(T0 + 24, "gpt-6.1-sol", None),
                message(T0 + 33),
                count(T0 + 37, (19_992, 0, 30), (13_992, 0, 0)),
                rate(T0 + 37, "codex", Some((55.0, 300, reset + 1)), None),
            ]
            .concat()
        };
        let text = prompted(SUB);
        let got = rows(&mut TurnReader::default(), &text);
        let t = turns(&got);
        assert_eq!(t.len(), 1, "{got:?}");
        assert_eq!(t[0].model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(t[0].tokens.input, 13_992);
        assert_eq!(got[4], Row::Copied, "the copy's count");
        let mut reader = TurnReader::default();
        let (head, tail) = text.split_at(text.find(&settings(T0, ROOT)).expect("a copied line"));
        let first = rows(&mut reader, head);
        assert!(turns(&first).is_empty());
        assert_eq!(reader.thread.as_deref(), Some(SUB), "kept inside the copy");
        let second = rows(&mut reader, tail);
        assert_eq!(turns(&second).len(), 1, "{second:?}");
        assert_eq!(
            (reader.prefix, reader.thread.as_deref()),
            (Prefix::Own, None)
        );
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-usage-settings-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let shares = |text: &str| {
            std::fs::write(&path, text).expect("write");
            rollout_readings(&path, CONTEXT_REACH)
                .expect("read")
                .iter()
                .flat_map(|r| r.windows.iter().map(|w| w.used_percent))
                .collect::<Vec<_>>()
        };
        assert_eq!(shares(&text), [12.0, 55.0], "the own count's and reading");
        let other = prompted(OTHER);
        let got = rows(&mut TurnReader::default(), &other);
        assert!(turns(&got).is_empty(), "{got:?}");
        assert_eq!(got.last(), Some(&Row::Copied));
        assert_eq!(shares(&other), Vec::<f64>::new());
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .expect("append");
    }

    fn written_at(path: &Path, at: i64) {
        let when = SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(u64::try_from(at).expect("after 1970"));
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(when))
            .expect("set the mtime");
    }

    fn inputs(turns: &[Turn]) -> Vec<u64> {
        turns.iter().map(|t| t.tokens.input).collect()
    }

    fn step(cursor: &mut RolloutCursor, path: &Path, cap: u64) -> (RolloutRead, Vec<Turn>) {
        let mut got = Vec::new();
        let read = cursor
            .advance(path, cap, &mut |t| got.push(t.clone()))
            .expect("the rollout opens");
        (read, got)
    }

    #[test]
    fn a_cursor_hands_each_appended_turn_once_and_a_rewrite_none_again() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-cursor-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let head = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, "gpt-6-sol", None),
        ]
        .concat();
        let one = count(T0 + 2, (10, 0, 1), (10, 0, 1));
        let two = count(T0 + 3, (30, 0, 3), (20, 0, 2));
        let three = count(T0 + 4, (60, 0, 6), (30, 0, 3));
        std::fs::write(&path, format!("{head}{one}")).expect("write");
        let mut cursor = RolloutCursor::default();
        let (read, first) = step(&mut cursor, &path, u64::MAX);
        assert_eq!(inputs(&first), [10]);
        assert_eq!(read.read, (head.len() + one.len()) as u64);
        assert!(read.complete);
        append(&path, &two);
        let (read, second) = step(&mut cursor, &path, u64::MAX);
        assert_eq!(inputs(&second), [20]);
        assert_eq!(
            read.read,
            two.len() as u64 + 1,
            "the appended line and the byte before it"
        );
        let (read, third) = step(&mut cursor, &path, u64::MAX);
        assert!(third.is_empty(), "{third:?}");
        assert!(read.complete && read.read == 1, "{read:?}");
        let (torn, rest) = three.split_at(three.len() / 2);
        append(&path, torn);
        let (read, none) = step(&mut cursor, &path, u64::MAX);
        assert!(
            none.is_empty() && read.complete,
            "a torn line waits: {none:?}"
        );
        append(&path, rest);
        assert_eq!(inputs(&step(&mut cursor, &path, u64::MAX).1), [30]);
        let text = format!("{head}{one}{two}{three}");
        let rewrite = dir.path().join("rewrite.jsonl");
        std::fs::write(&rewrite, &text).expect("write");
        std::fs::rename(&rewrite, &path).expect("rename over");
        let (read, again) = step(&mut cursor, &path, u64::MAX);
        assert_eq!(
            read.read,
            text.len() as u64,
            "another identity: from its start"
        );
        assert!(again.is_empty(), "{again:?}");
        let four = count(T0 + 5, (100, 0, 10), (40, 0, 4));
        let shifted = format!(
            "{}{}{one}{two}{three}{four}",
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, "gpt-6-sol-x", None)
        );
        std::fs::write(&path, &shifted).expect("rewrite in place");
        let (read, moved) = step(&mut cursor, &path, u64::MAX);
        assert_eq!(
            read.read,
            shifted.len() as u64 + 1,
            "its line ends moved: from its start"
        );
        assert_eq!(inputs(&moved), [40], "only the turn it had not handed");
        let fresh = step(&mut RolloutCursor::default(), &path, u64::MAX).1;
        assert_eq!(
            inputs(&fresh),
            [10, 20, 30, 40],
            "NEGATIVE CONTROL: a reader that kept no total hands them all"
        );
    }

    #[test]
    fn a_budgeted_cursor_hands_what_an_unbounded_one_does() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("codex-cursor-cap-")
            .tempdir()
            .expect("temp dir");
        let path = dir.path().join("rollout.jsonl");
        let text = [
            meta(T0, ROOT, ROOT, None),
            context(T0 + 1, "gpt-6-sol", None),
            count(T0 + 2, (10, 0, 1), (10, 0, 1)),
            message(T0 + 3),
            count(T0 + 4, (30, 0, 3), (20, 0, 2)),
            count(T0 + 5, (60, 0, 6), (30, 0, 3)),
        ]
        .concat();
        std::fs::write(&path, &text).expect("write");
        let whole = step(&mut RolloutCursor::default(), &path, u64::MAX).1;
        assert_eq!(inputs(&whole), [10, 20, 30]);
        let mut cursor = RolloutCursor::default();
        let (read, none) = step(&mut cursor, &path, 0);
        assert!(
            none.is_empty() && read.read == 0 && !read.complete,
            "a cap of 0 reads no line"
        );
        let lines = text.lines().count();
        let mut got = Vec::new();
        let mut reads = 0;
        for _ in 0..lines {
            let (read, turns) = step(&mut cursor, &path, 1);
            reads += 1;
            got.extend(turns);
            if read.complete {
                break;
            }
        }
        assert_eq!(reads, lines, "one whole line a read");
        assert_eq!(got, whole);
    }

    #[test]
    fn rollouts_fold_hands_each_turn_once_and_lets_go_of_what_is_gone() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-rollouts-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/11/05");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds");
        let since = now - 3600;
        let base = (now - 600) * 1000;
        let body = |thread: &str| {
            [
                meta(base, thread, thread, None),
                context(base + 1, "gpt-6-sol", None),
                count(base + 2, (10, 0, 1), (10, 0, 1)),
                count(base + 3, (30, 0, 3), (20, 0, 2)),
            ]
            .concat()
        };
        let file = |thread: &str, at: i64| {
            let path = day.join(format!("rollout-2026-11-05T10-00-00-{thread}.jsonl"));
            std::fs::write(&path, body(thread)).expect("write");
            written_at(&path, at);
            path
        };
        let a = file(ROOT, now - 120);
        let b = file(OTHER, now - 60);
        let old = file(SUB, since - 3600);
        let fold = |rollouts: &mut Rollouts, since: i64| {
            let mut got = Vec::new();
            let caught_up = rollouts.fold(home.path(), since, u64::MAX, &mut |path, turn| {
                got.push((path.to_path_buf(), turn.tokens.input));
            });
            (caught_up, got)
        };
        let mut rollouts = Rollouts::default();
        let (caught_up, first) = fold(&mut rollouts, since);
        assert!(caught_up);
        assert_eq!(
            first,
            [
                (b.clone(), 10),
                (b.clone(), 20),
                (a.clone(), 10),
                (a.clone(), 20)
            ],
            "newest first, each turn once"
        );
        assert!(
            !rollouts.files.contains_key(&old),
            "older than `since`: not read"
        );
        let (caught_up, second) = fold(&mut rollouts, since);
        assert!(caught_up && second.is_empty(), "{second:?}");
        append(&a, &count(base + 4, (60, 0, 6), (30, 0, 3)));
        assert_eq!(fold(&mut rollouts, since).1, [(a.clone(), 30)]);
        append(&a, &count((since - 1) * 1000, (100, 0, 10), (40, 0, 4)));
        assert!(
            fold(&mut rollouts, since).1.is_empty(),
            "served before `since`"
        );
        assert_eq!(
            rollouts.files[&a].reader.top, 110,
            "NEGATIVE CONTROL: it was read"
        );
        std::fs::remove_file(&b).expect("remove");
        let (caught_up, none) = fold(&mut rollouts, since);
        assert!(caught_up && none.is_empty(), "{none:?}");
        assert!(
            !rollouts.files.contains_key(&b),
            "a rollout gone keeps no cursor"
        );
        let kept = rollouts.files[&a].clone();
        let at = std::fs::metadata(&a)
            .and_then(|m| m.modified())
            .expect("mtime");
        {
            use std::io::{Seek as _, Write as _};
            let mut f = std::fs::File::options().write(true).open(&a).expect("open");
            f.seek(std::io::SeekFrom::End(-1)).expect("seek");
            f.write_all(b"x").expect("overwrite its last newline");
            f.set_modified(at).expect("put its mtime back");
        }
        let (caught_up, none) = fold(&mut rollouts, since);
        assert!(caught_up && none.is_empty(), "{none:?}");
        assert_eq!(
            rollouts.files[&a], kept,
            "the same identity, size and mtime: not opened"
        );
        written_at(&a, now - 30);
        fold(&mut rollouts, since);
        assert!(
            rollouts.files[&a].pos < kept.pos,
            "NEGATIVE CONTROL: opened, its moved line end reads it again from its start"
        );
        let (_, all) = fold(&mut Rollouts::default(), since - 7200);
        assert_eq!(
            all.iter().filter(|(path, _)| *path == old).count(),
            2,
            "NEGATIVE CONTROL: with `since` before it, the old rollout is read: {all:?}"
        );
    }

    /// A directory or a dangling link named as a rollout is no rollout: the fold reads the real
    /// one beside it and is caught up, where listing them would leave it short every fold.
    #[cfg(unix)]
    #[test]
    fn an_entry_named_as_a_rollout_that_is_no_file_leaves_the_fold_caught_up() {
        let home = aterm_tempfile::Builder::new()
            .prefix("codex-rollouts-")
            .tempdir()
            .expect("temp dir");
        let day = home.path().join("sessions/2026/11/05");
        std::fs::create_dir_all(&day).expect("mkdir");
        let now = i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds");
        let base = (now - 600) * 1000;
        let real = day.join(format!("rollout-2026-11-05T10-00-00-{ROOT}.jsonl"));
        std::fs::write(
            &real,
            [
                meta(base, ROOT, ROOT, None),
                context(base + 1, "gpt-6-sol", None),
                count(base + 2, (10, 0, 1), (10, 0, 1)),
            ]
            .concat(),
        )
        .expect("write");
        std::fs::create_dir(day.join(format!("rollout-2026-11-05T10-00-01-{OTHER}.jsonl")))
            .expect("a directory so named");
        std::os::unix::fs::symlink(
            day.join("gone"),
            day.join(format!("rollout-2026-11-05T10-00-02-{SUB}.jsonl")),
        )
        .expect("a dangling link so named");
        let mut rollouts = Rollouts::default();
        let mut got = Vec::new();
        let caught_up = rollouts.fold(home.path(), now - 3600, u64::MAX, &mut |path, turn| {
            got.push((path.to_path_buf(), turn.tokens.input));
        });
        assert_eq!(got, [(real.clone(), 10)]);
        assert!(caught_up, "an entry that is no rollout held the fold short");
        assert_eq!(rollouts.files.len(), 1, "a cursor for the rollout alone");
    }

    #[test]
    fn codex_home_is_codex_home_else_the_users() {
        use crate::harness::host_path::abs;
        let (home, alt, pj) = (abs("/Users//x"), abs("/alt/home"), abs("/w/pj"));
        let dot_codex = Some(Path::new(&home).join(".codex"));
        let user = Some(home.as_str());
        assert_eq!(codex_home(None, user, None), dot_codex);
        assert_eq!(
            codex_home(Some(&alt), user, None),
            Some(PathBuf::from(&alt))
        );
        assert_eq!(codex_home(Some("rel"), None, None), None);
        assert_eq!(
            codex_home(Some("rel"), user, None),
            None,
            "never another home's records"
        );
        assert_eq!(
            codex_home(Some("rel"), user, Some(Path::new(&pj))),
            Some(Path::new(&pj).join("rel"))
        );
        assert_eq!(codex_home(Some("rel"), user, Some(Path::new("pj"))), None);
        assert_eq!(
            codex_home(Some(""), user, Some(Path::new(&pj))),
            dot_codex,
            "an empty one is unset"
        );
    }
}
