// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Codex home, rollout file discovery, and turn token reader ([`TurnReader`], [`read_rollout`], [`conversation_rollouts`]).

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
            .is_some_and(|stem| stem.ends_with(&format!("-{thread}")))
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
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for year in dirs(&home.join("sessions")) {
        for month in dirs(&year) {
            for day in dirs(&month) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    if let Ok(at) = e.metadata().and_then(|m| m.modified()) {
                        out.push((e.path(), at));
                    }
                }
            }
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.1));
    out
}

/// Reads the last `bytes` of `path`, dropping a partial leading line.
#[must_use]
pub fn tail_of(path: &Path, bytes: u64) -> Option<String> {
    tail_after(path, 0, bytes)
}

/// Reads up to the last `bytes` of `path` starting no earlier than line-aligned `floor`.
pub(crate) fn tail_after(path: &Path, floor: u64, bytes: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = crate::harness::footer::open_regular(path)?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(bytes).max(floor);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if from > floor {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
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
    /// The counts of a usage object; a missing class reads 0. `None` for
    /// anything but an object holding `input_tokens` or `output_tokens` as a
    /// whole number.
    fn of(v: Option<&Value>) -> Option<Self> {
        let o = v?.as_object()?;
        let n = |key: &str| o.get(key).and_then(Value::as_u64);
        if n("input_tokens").is_none() && n("output_tokens").is_none() {
            return None;
        }
        Some(Self {
            input: n("input_tokens").unwrap_or(0),
            cached: n("cached_input_tokens").unwrap_or(0),
            cache_write: n("cache_write_input_tokens").unwrap_or(0),
            output: n("output_tokens").unwrap_or(0),
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

/// The fingerprint of a `token_count` ([`Turn::key`]).
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

/// Extracts `(last_token_usage, total_tokens)` from a `token_count` payload.
pub(crate) fn counts_of(payload: Option<&Value>) -> Option<(TurnTokens, u64, TurnTokens)> {
    let info = payload?.get("info").filter(|i| i.is_object())?;
    let total_usage = info.get("total_token_usage");
    let total = TurnTokens::of(total_usage)?;
    let last = TurnTokens::of(info.get("last_token_usage"))?;
    let total_tokens = total_usage
        .and_then(|t| t.get("total_tokens"))
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
            // A count of the rate limits alone.
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
                line = Vec::new();
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

/// The `session_meta` of a rollout's first line: its `session_id` and `id`.
fn session_ids(first_line: &str) -> Option<(Option<String>, Option<String>)> {
    let v: Value = aterm_json::from_str(first_line.trim()).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    let id = |key: &str| p.get(key).and_then(Value::as_str).map(str::to_owned);
    Some((id("session_id"), id("id")))
}

/// Reads the first line of `path` up to [`META_BYTES`].
pub(crate) fn first_line(path: &Path) -> Option<String> {
    first_line_of(&crate::harness::footer::open_regular(path)?)
}

/// Reads the first line of open `file` from offset 0 up to [`META_BYTES`].
pub(crate) fn first_line_of(file: &std::fs::File) -> Option<String> {
    use std::io::{BufRead as _, Seek as _};
    let mut file = file;
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    let mut line = Vec::new();
    std::io::BufReader::new(std::io::Read::take(file, META_BYTES))
        .read_until(b'\n', &mut line)
        .ok()?;
    line.ends_with(b"\n")
        .then(|| String::from_utf8_lossy(&line).into_owned())
}

/// Finds all rollout files belonging to `thread`'s conversation under `home`.
#[must_use]
pub fn conversation_rollouts(home: &Path, thread: &str) -> Conversation {
    let all = rollouts_by_write(home);
    let name_of = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let own = all
        .iter()
        .find(|(p, _)| is_rollout_of(&name_of(p), thread))
        .map(|(p, _)| p.clone());
    let root = own
        .as_deref()
        .and_then(first_line)
        .and_then(|l| session_ids(&l))
        .and_then(|(session, _)| session)
        .filter(|s| is_thread_id(s))
        .unwrap_or_else(|| thread.to_owned());
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
        if is_rollout_of(&name_of(path), &root) {
            roots.push(path.clone());
            continue;
        }
        opened += 1;
        if opened > MAX_CONVERSATION_SCAN {
            unread += 1;
            continue;
        }
        match first_line(path).and_then(|l| session_ids(&l)) {
            Some((Some(session), id))
                if session == root && id.as_deref() != Some(root.as_str()) =>
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
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":{{\"total_token_usage\":{},\"last_token_usage\":{},\
             \"model_context_window\":272000}},\"rate_limits\":{{\"limit_id\":\"codex\",\
             \"primary\":{{\"used_percent\":12.0,\"window_minutes\":300,\"resets_at\":1790000000}},\
             \"secondary\":null,\"plan_type\":\"pro\"}}}}}}\n",
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

    /// The lab's composed login (Codex 0.163.0 wrote it into its
    /// `session_meta`s, 2026-10-16): stand-in ids, no real account's.
    const LOGIN_A: (&str, &str) = ("acct-lab-1", "user-lab");

    /// Thread ids in UUIDv7 form, made in 2024: every file a test writes
    /// now is written since they began.
    const ROOT: &str = "01900000-0000-7000-8000-000000000001";

    const SUB: &str = "01900000-0000-7000-8000-000000000002";

    const OTHER: &str = "01900000-0000-7000-8000-000000000003";

    /// A stand-in instant, unix ms.
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

    /// EACH SERVED TURN ONCE: a `token_count` whose running total grew is
    /// one turn, its `last_token_usage` on the latest `turn_context`'s
    /// model, stamped with its own second.
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

    /// THE FORK RULE (the design's §3.5, §8.4): a fork opens with its
    /// parent's lines rewritten in one burst; that copied prefix counts
    /// nothing, and the fork's own turn — its total continuing from the
    /// copy's — counts once.
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

    /// The burst's edge: a line stamped [`FORK_BURST_MS`] after the newest
    /// before it is still the copy; one a millisecond later is the fork's
    /// own.
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

    /// A FORK'S OWN TOTALS GO ON FROM ITS COPY'S: a
    /// `token_count` its own lines write again with the copy's last total —
    /// as Codex writes one at a turn's start once the thread has a total —
    /// is a repeat, never its parent's last turn counted again; its own turn
    /// after it counts once.
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

    /// A rollout whose first line names no session (cut, or of a shape this
    /// reader does not know) may be a fork: its first burst counts nothing,
    /// and turns after it count.
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

    /// A turn whose `turn_context` named a service tier other than the
    /// standard one says so; one naming none or `standard` does not, and a
    /// model that is no model id is none.
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

    /// A whole read streams the rollout's complete lines through the reader:
    /// a torn last line (one still being written) is not read, and a cap
    /// stops the read short and says so.
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

    /// A CONVERSATION's rollouts: the TUI's own thread — the `session_id`
    /// the named thread's rollout gives — first, then every subagent whose
    /// `session_meta` names it, oldest write first, named by a subagent or
    /// by the root alike; a rollout whose first line cannot be read is
    /// counted unread.
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

    /// A rollout stamp to the millisecond: the fraction's first three
    /// digits, padded.
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

    /// A PAGINATED THREAD'S FORK COPIES NO COUNT (measured on
    /// 0.163.0 in the lab): a subagent spawned with its parent's turns opens
    /// with its own `session_meta`, its parent's, and the parent's turns —
    /// messages and `turn_context`s, no `token_count` — in one millisecond;
    /// its own `turn_context` follows within 10 ms and, on the stub's instant
    /// answer, its first count within 25 ms, its total from zero.
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

    /// A LEGACY FORK'S OWN SETTINGS LINE ENDS ITS COPY (measured on
    /// 0.163.0): `codex fork <legacy thread> <prompt>` copies its
    /// parent's lines under one stamp, writes its own
    /// `thread_settings_applied` in the same millisecond, and runs the
    /// prompt's turn at once: its next line 14 ms on, its count 37 ms on,
    /// inside any burst.
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

    /// The home Codex's records are read under: `CODEX_HOME`, else
    /// `~/.codex`; a relative one is the TUI's directory's, as Codex resolves
    /// it.
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
