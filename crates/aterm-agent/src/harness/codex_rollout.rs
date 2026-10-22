// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What Codex's own files say: where a Codex keeps its home ([`codex_home`]),
//! its rollouts (`sessions/<y>/<m>/<d>/rollout-<stamp>-<thread>.jsonl`,
//! [`rollouts_by_write`], [`is_rollout_of`]), its thread ids ([`is_thread_id`],
//! [`thread_created_ms`]), and the turns a rollout served, once each, for the
//! spend fold ([`TurnReader`], [`read_rollout`], [`conversation_rollouts`]).
//! Never the screen, and no supervision: the spend fold (`harness::spend`),
//! `aterm harness usage` from a Codex session's own shell
//! (`harness::cli::run_codex_usage`), `aterm ai`'s limit reader
//! (`ai::codex_limits`), the session log's Codex step fold
//! (`harness::codex_steps`) and the codex-alab path read Codex through it. Codex's
//! files are a third party's, so every reader refuses a shape it was not
//! written for.
//!
//! Spend ([`TurnReader`], the account stats' C1,
//! `docs/DESIGN-account-stats-2026-10-15.md` §3.3, §3.5, §4.3): a rollout
//! read from its start yields each served turn once — the `last_token_usage`
//! of every `token_count` whose running total (`total_token_usage`) grew, on
//! the model of the latest `turn_context` before it. Codex writes a
//! `token_count` again with the same total (most often after a reasoning
//! item, and at a turn's start once the thread has a total), and those count
//! nothing. A LEGACY FORK (a `session_meta` naming `forked_from_id`, its
//! `history_mode` `legacy` or none) begins with a copy of its parent's
//! lines, its `token_count`s among them, rewritten under new stamps in one
//! burst; summing them counts the parent's turns again. So the copied prefix
//! — the run of lines from the fork's `session_meta` up to the first
//! `thread_settings_applied` naming the fork's own thread (Codex 0.163.0
//! writes one right after the copy; a copied one names the thread it was
//! copied from), or, in a fork that writes none, each stamped within
//! [`FORK_BURST_MS`] of the newest before it — counts for nothing: no
//! tokens, no model, no usage reading. Its totals stand: the fork's own go
//! on from the copy's last, so a count the fork writes again with that total
//! is a repeat, not its parent's last turn. A rollout whose first line names
//! no session is read the same way, since nothing rules a fork out, and a
//! turn needs a model's answer, which no burst holds. A PAGINATED thread's
//! fork ([`PAGINATED`]) copies no count, and is read from its first line:
//! a subagent's own first turn follows its copy within milliseconds, inside
//! any burst a gap between stamps could tell. Each turn
//! carries a fingerprint of its total and its counts ([`Turn::key`]), one
//! per turn within its rollout (the total grows at every one), so a rollout
//! read again from its start is known by them. Across rollouts it names no
//! turn: two threads served the same counts (one prompt run twice, the lab's
//! stub) share it, so no fold dedupes across rollouts by it.
//!
//! MEASURED on the 133 rollouts of the machine this was built on (Codex
//! 0.145.0 and 0.146.0, July and August 2026; 121 of them forks, each with
//! lines after its copy): the copy's consecutive stamps were at most 26 ms
//! apart and the fork's own first line came at least 1.47 s after the
//! copy's last; the fork's own running total continued from the copy's last;
//! outside the copies no total fell, each growth equalled the line's
//! `last_token_usage.total_tokens`, and every `token_count` had a model.
//! There, 1,063 `token_count`s repeated their thread's total; the first
//! count of a turn after a thread's first did so in 151 of 218 turns, and
//! the first of a thread's first turn, a fork's included, never did.
//! Summing every growth per file, copies included, gave 20.4 times the
//! fork-aware sum. `cache_write_input_tokens` was 0 on all 463,935
//! `token_count` lines, and no `turn_context` named a `service_tier`.
//! MEASURED again on the lab's 34 rollouts of Codex 0.163.0 (stub replies,
//! no forks): the same `token_count` and `turn_context` keys, and 79 turns
//! with only 46 distinct fingerprints among them.
//!
//! MEASURED in the lab on Codex 0.163.0 (2026-10-16; the stub's provider,
//! the lab's composed ChatGPT logins, stand-in replies; a tool call scripted
//! at the stub): every new thread is `history_mode: "paginated"`, and every
//! line carries an `ordinal`.
//! * `/fork`, and `codex fork --last` twice (once under another login), of a
//!   paginated thread: the fork's rollout copies nothing. It holds its
//!   `session_meta` (its ordinals going on from the parent's last) and a
//!   `thread_settings_applied` in the same millisecond, then its own turn
//!   when a person typed one, 6 to 14 s on. Its running total went on from
//!   the parent's (47,303 after 31,433).
//! * A subagent (`spawn_agent` in the `collaboration` namespace, `fork_turns:
//!   "all"`): its rollout opens with its own `session_meta`, its parent's,
//!   and the parent's turns compacted (`task_started`, messages,
//!   `world_state`, `turn_context`, `task_complete`): 21 lines in one
//!   millisecond, no `token_count` among them. Its own lines began 2 ms
//!   later, its own `turn_context` 9 ms later, and on the stub's instant
//!   answer its first count came 24 ms after the copy, its total from zero
//!   (15,577, that turn alone). The burst rule took that turn for its
//!   parent's: over the lab's rollouts the fold counted 13 of 14 turns,
//!   173,891 of 189,468 tokens. Read from its first line, it counts 14 and
//!   189,468, as an independent count does.
//! * `/fork` of a legacy thread (a composed 0.146.0-shaped rollout): the fork
//!   is `history_mode: "legacy"`, with no ordinals. It copied the parent's
//!   `session_meta`, `turn_context`s, messages and `token_count`s (their
//!   rate limits among them) under one stamp (1 ms spread), then a
//!   `turn_aborted` and its own `thread_settings_applied` in the same
//!   millisecond. A person's turn came 21 s later, its total going on
//!   (20,022 after 6,030). A fork of that fork copied the parent's
//!   `thread_settings_applied` lines too, each naming the parent's thread,
//!   then wrote its own naming itself. The parent's file was not rewritten.
//! * `codex fork <legacy thread> <prompt>` (three times): the same copy, and
//!   the prompt's turn at once. Its first own line came 247 ms after the copy
//!   on a cold start, 17 and 14 ms after it warm, its count 37 ms after: well
//!   inside [`FORK_BURST_MS`], and inside the 26 ms gaps 0.145's copies
//!   showed, so no gap between stamps tells this copy's end. In all five
//!   legacy forks the first `thread_settings_applied` naming the fork's own
//!   thread came in the copy's millisecond, after the copy (and after a
//!   `turn_aborted` the fork writes when its parent's last turn did not
//!   complete), before any line of a turn of its own.
//! * A legacy thread is offered no `collaboration` tools: a `spawn_agent`
//!   call there was answered `unsupported call: collaborationspawn_agent`.
//!   So 0.163.0 spawns only from paginated threads, and a copy holding
//!   counts comes only from a person's fork of a legacy thread.
//! * The login: a ChatGPT login's `session_meta` names `creator_account_id`
//!   and `creator_user_id`; an API-key login's names neither
//!   (`ai::codex_limits::opening_account`).
//! * The rate limits on the stub's provider: `plan_type`,
//!   `individual_limit` and `spend_control_reached` were null on every line,
//!   and the credits were carried over. A turn whose answer carried no rate
//!   headers wrote its windows null and kept the credits.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aterm_json::Value;

// Codex's names: thread ids and rollout files

/// Whether `s` is a thread id Codex prints and takes back: a hyphenated UUID
/// (8-4-4-4-12 hex digits). Nothing else is ever put on a relaunch line.
#[must_use]
pub fn is_thread_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| aterm_codec::hex::is_hex(p, n))
}

/// When a thread was created, in unix milliseconds, from its UUIDv7 id's first
/// 48 bits. `None` for any id that is not version-7.
#[must_use]
pub fn thread_created_ms(thread: &str) -> Option<u64> {
    if !is_thread_id(thread) || thread.as_bytes().get(14) != Some(&b'7') {
        return None;
    }
    let hex: String = thread.split('-').take(2).collect();
    u64::from_str_radix(&hex, 16).ok()
}

/// Whether `name` is a rollout file of `thread`:
/// `rollout-<stamp>-<thread>.jsonl`. A compressed `.jsonl.zst` is not read.
#[must_use]
pub fn is_rollout_of(name: &str, thread: &str) -> bool {
    name.starts_with("rollout-")
        && name
            .strip_suffix(".jsonl")
            .is_some_and(|stem| stem.ends_with(&format!("-{thread}")))
}

/// The `$CODEX_HOME` a Codex started with `env` in the working directory
/// `cwd` uses: `CODEX_HOME`, a relative one joined to `cwd` (Codex
/// canonicalizes it against its own working directory as it starts), else
/// `$HOME/.codex` (the Codex upgrade's own rule); an empty `CODEX_HOME` is
/// unset. A relative `CODEX_HOME` with no absolute `cwd` names no home aterm
/// can place — never `$HOME/.codex`, which may be another Codex's records —
/// so nothing is read (`None`).
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

/// Every rollout under `home`'s `sessions/<y>/<m>/<d>/`, newest write first.
/// A thread's rollout sits in the folder of the day it started, so the one
/// written last may be in an old day's folder.
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

/// The last `bytes` of `path`, its cut first line dropped.
#[must_use]
pub fn tail_of(path: &Path, bytes: u64) -> Option<String> {
    tail_after(path, 0, bytes)
}

/// The last `bytes` of `path`, read from no earlier than `floor` (the start
/// of a line): a first line cut by the read's start is dropped, one starting
/// at `floor` is kept whole.
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

// Spend: each served turn of a rollout, once (module header)

/// How far apart, in milliseconds, two lines of a legacy fork's copied
/// prefix may be stamped, in a fork that writes no `thread_settings_applied`
/// of its own: the prefix runs from the fork's `session_meta` while each
/// line is stamped within this of the newest stamp before it (module
/// header: 0.145 and 0.146's copies measured at most 26 ms apart and the
/// fork's own first line at least 1.47 s after them). Codex 0.163.0's legacy
/// fork writes that line right after its copy, and its own turn can follow
/// within 14 ms, so there the line ends the copy. A paginated thread's fork
/// copies no count and is read without either ([`PAGINATED`]).
pub const FORK_BURST_MS: i64 = 500;

/// The `session_meta` `history_mode` of a thread whose forks copy no count
/// (module header: Codex 0.163.0 makes every new thread so; a fork of a
/// `legacy` one is `legacy` too).
pub const PAGINATED: &str = "paginated";

/// One turn's token counts as Codex records them (`last_token_usage`), in
/// OpenAI's classes: `input` holds `cached`, and `output` holds `reasoning`.
/// Whether `input` holds `cache_write` too is UNKNOWN (none was seen, module
/// header); the spend fold reads it as a part, as OpenAI's usage details are
/// parts of the input, and calls such a turn approximate.
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

/// One served turn, counted once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The `token_count`'s stamp, unix seconds.
    pub at: i64,
    /// The model of the latest `turn_context` before it, outside a fork's
    /// copied prefix; `None` where none named one.
    pub model: Option<String>,
    /// Its counts (`last_token_usage`).
    pub tokens: TurnTokens,
    /// That `turn_context` named a service tier (`fast`, `flex`): the list
    /// price is another than the standard one.
    pub tier: bool,
    /// FNV-1a/64 of its running total and its counts: one per turn within
    /// its rollout, never an identity across rollouts (module header).
    pub key: u64,
}

/// Where a [`TurnReader`] stands in its rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prefix {
    /// No line read yet.
    #[default]
    Start,
    /// Inside a fork's copied prefix, or the first burst of a rollout whose
    /// first line names no session: the newest stamp read, unix ms.
    Copy(i64),
    /// Past it, or a rollout that is no fork: every line counts.
    Own,
}

/// What one rollout line was to the fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A served turn.
    Turn(Turn),
    /// A `token_count` whose running total did not grow: written again (after
    /// a rate-limit update), no turn.
    Repeat,
    /// A `token_count` in a fork's copied prefix: its parent's turn.
    Copied,
    /// A turn with no readable stamp: placed in no window, so not counted.
    Untimed,
    /// A line that is not JSON, or a `token_count` whose counts cannot be
    /// read.
    Unparsed,
    /// Any other line.
    Other,
}

/// The spend fold of one rollout, a line at a time from its start (module
/// header), its state kept between reads: where it stands against a fork's
/// copied prefix (inside a legacy fork's, the fork's own thread too), the
/// largest running total read, and the model and service tier of the latest
/// `turn_context`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnReader {
    /// Where the reader stands.
    pub prefix: Prefix,
    /// The largest `total_token_usage.total_tokens` read, a fork's copied
    /// prefix included: the fork's own totals go on from its copy's (module
    /// header), so a count its own lines write again with the copy's total
    /// is a repeat, not its parent's last turn again.
    pub top: u64,
    /// The latest `turn_context`'s `model` past the prefix.
    pub model: Option<String>,
    /// That `turn_context` named a service tier other than the standard one.
    pub tier: bool,
    /// While a legacy fork's copy is read, the fork's own thread: the `id`
    /// its `session_meta` names, where that is a thread id. The copy ends at
    /// the first `thread_settings_applied` naming it (module header);
    /// `None` past the copy, and for a rollout whose first line names none.
    pub thread: Option<String>,
}

/// Unix milliseconds of a rollout stamp (`2026-08-01T17:36:47.998Z`): the
/// whole second, plus the fraction's first three digits.
pub(crate) fn stamp_ms(stamp: &str) -> Option<i64> {
    let secs = aterm_types::rfc3339::parse_utc_fractional(stamp)?;
    // The parser accepted the fraction: ASCII digits only.
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

/// A `token_count` payload's running total, its `total_tokens` (else input
/// plus output), and its last turn; `None` where `info` holds no readable
/// pair of counts (the rate limits alone, or counts that cannot be read).
/// The Codex step fold reads a turn's counts by it (`super::codex_steps`).
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
    /// Read the rollout's next line (module header). Past the prefix only a
    /// `token_count` or `turn_context` line is opened; inside it every line
    /// is, for its stamp.
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
        // A copied count counts nothing, but its total stands: the fork's
        // own totals go on from it (module header).
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
                // A paginated thread's fork copies no count (module header):
                // its every line is its own.
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
                // A fork's `session_meta`, or a first line naming no
                // session: the burst starts at its stamp. One with none is
                // no start; the next line's stamp is.
                if let Some(at) = stamp {
                    self.prefix = Prefix::Copy(at);
                }
                return copied(self);
            }
            Prefix::Copy(newest) => {
                // The fork's own settings line ends the copy, however soon
                // after it (module header); a copied one names the thread it
                // was copied from.
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

/// What one whole read of a rollout ([`read_rollout`]) did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RolloutRead {
    /// Bytes read.
    pub read: u64,
    /// It was read to the end it had (a torn last line aside), not stopped
    /// at the cap or by an error.
    pub complete: bool,
    /// Lines longer than [`crate::harness::usage::MAX_LINE_BYTES`], skipped
    /// unread.
    pub long: u64,
}

/// Read the rollout at `path` from its start, at most `cap` bytes, each
/// complete line through `reader` into `each`. A line longer than
/// [`crate::harness::usage::MAX_LINE_BYTES`] is skipped as it streams, never
/// held, and a torn last line (one still being written) is not read. `None`
/// when the file cannot be opened.
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

/// The lines of the rollout at `path` from its start, at most `cap` bytes,
/// each complete one handed to `each` with the offset it starts at — its
/// text, `None` for one that is not UTF-8 — until `each` breaks. A line
/// longer than [`crate::harness::usage::MAX_LINE_BYTES`] is skipped as it
/// streams, never held (counted in [`RolloutRead::long`]), and a torn last
/// line is not handed. `complete`: it was read to its end, neither stopped
/// at the cap, by an error, nor by `each`. `None` when the file cannot be
/// opened.
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

/// One line [`stream_lines_ends`] hands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Streamed<'a> {
    /// A complete line: its text, `None` for one that is not UTF-8.
    Line(Option<&'a str>),
    /// A complete line longer than
    /// [`crate::harness::usage::MAX_LINE_BYTES`], skipped unread.
    Long,
}

/// [`stream_lines`], each complete line handed on with the offsets it
/// starts and ends at (past its newline), a long one too, unread
/// ([`Streamed::Long`]): for a reader that places every line it could not
/// read (the Codex step fold, `super::codex_steps::fold_rollout`).
pub(crate) fn stream_lines_ends(
    path: &Path,
    cap: u64,
    each: impl FnMut(u64, u64, Streamed<'_>) -> std::ops::ControlFlow<()>,
) -> Option<RolloutRead> {
    let file = crate::harness::footer::open_regular(path)?;
    Some(stream_open_ends(file, 0, None, cap, each))
}

/// [`stream_lines_ends`] over the open `file` from offset `from`, the start
/// of a line (where a reader that reads on stopped: the Codex rollout tap,
/// `super::codex_tap`), or, with `skip` (where that line began), a point
/// inside a line the reader skips, handed on as [`Streamed::Long`] at its
/// newline and counted in [`RolloutRead::long`] there: the offsets handed on
/// are the file's own, `read` counts the bytes read from `from`, and at most
/// `cap` of them are. A file that cannot be sought there reads nothing.
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
    // Whether the line being skipped is counted long yet: one carried in
    // from an earlier read is counted at its newline.
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
        // Bytes are left past the cap: the read stops short.
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

/// How much of a rollout's first line is read for its `session_meta`
/// ([`conversation_rollouts`]); the line carries base instructions (about
/// 20 KB).
pub const META_BYTES: u64 = 1 << 20;

/// The most rollouts [`conversation_rollouts`] opens the first line of.
pub const MAX_CONVERSATION_SCAN: usize = 16_384;

/// The rollouts of one Codex conversation under a home, for `harness usage`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conversation {
    /// The thread whose conversation it is: the `session_id` the named
    /// thread's `session_meta` gives (its conversation's first thread, the
    /// TUI's own), else the named thread.
    pub root: String,
    /// The named thread's own rollout, where it has one.
    pub own: Option<PathBuf>,
    /// The root's rollout first, where found, then every rollout whose
    /// `session_meta` names the root as its `session_id` (the subagents),
    /// oldest write first.
    pub files: Vec<PathBuf>,
    /// How many of `files` are subagents'.
    pub subagents: usize,
    /// Rollouts written since the root began whose first line could not be
    /// read: any of them may be one of its subagents. Past
    /// [`MAX_CONVERSATION_SCAN`], each one not opened.
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

/// The first line of `path`, at most [`META_BYTES`] of it; `None` when it
/// cannot be read or runs past the bound.
pub(crate) fn first_line(path: &Path) -> Option<String> {
    first_line_of(&crate::harness::footer::open_regular(path)?)
}

/// [`first_line`] of the open `file`, read from its start.
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

/// The rollouts of the conversation `thread` belongs to under `home`
/// ([`Conversation`]): the conversation's own thread, by the `session_id`
/// the named thread's rollout gives, and every subagent whose
/// `session_meta` names it. Only rollouts written since the root began are
/// opened (a UUIDv7 id says when; another id opens every one).
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
    // Oldest write first, so the root's first turns are read first.
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

/// Composed rollout lines in the shapes Codex writes them (key names as on
/// 0.145–0.163; the `token_count` as the survey in the module header read
/// it), each with its newline, stamped in unix milliseconds. Stand-in values
/// only: no real rollout is copied.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A rollout stamp: `2026-08-01T17:36:47.998Z`.
    pub(crate) fn stamp(ms: i64) -> String {
        let whole = crate::harness::usage::rfc3339_utc(ms.div_euclid(1000));
        format!("{}.{:03}Z", &whole[..19], ms.rem_euclid(1000))
    }

    /// A thread's `session_meta`: its id, its conversation (`session_id`),
    /// and the thread it was forked from, if any.
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

    /// [`meta`] as Codex 0.163.0 writes it: with its `history_mode`, and the
    /// ids of the login it was made under, where given.
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

    /// A `turn_context` naming `model`, and `tier` as its service tier.
    pub(crate) fn context(ms: i64, model: &str, tier: Option<&str>) -> String {
        let tier = tier.map_or_else(String::new, |t| format!(",\"service_tier\":\"{t}\""));
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\
             \"cwd\":\"/stand-in/project\",\"approval_policy\":\"never\",\"sandbox_policy\":\
             {{\"type\":\"danger-full-access\"}},\"model\":\"{model}\",\"effort\":\"high\"{tier}}}}}\n",
            stamp(ms)
        )
    }

    /// One count's classes: input (cached ones in it), cached input, output.
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

    /// A `token_count`: the running total and the last turn's counts, with
    /// the rate limits Codex writes beside them.
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

    /// A `token_count` of the rate limits alone (`info` null).
    pub(crate) fn limits_only(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":null,\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":null}}}}}}\n",
            stamp(ms)
        )
    }

    /// One slot of [`rate`]: `(used_percent, window_minutes, resets_at)`.
    pub(crate) type RateSlot = (f64, u64, i64);

    /// A `token_count` of the rate limits alone (`info` null) for `family`,
    /// its two slots each a [`RateSlot`] or null, the rest as Codex 0.163.0
    /// writes it on the lab's provider (credits carried, the plan null).
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

    /// Any other line: the model's message.
    pub(crate) fn message(ms: i64) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\
             \"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"done\"}}]}}}}\n",
            stamp(ms)
        )
    }

    /// A `thread_settings_applied` naming `thread`, as Codex 0.163.0 writes
    /// one when a thread's settings take effect (the settings stand-ins).
    pub(crate) fn settings(ms: i64, thread: &str) -> String {
        format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\
             \"thread_settings_applied\",\"thread_id\":\"{thread}\",\"thread_settings\":\
             {{\"model\":\"gpt-6.1-sol\",\"model_provider_id\":\"openai\",\
             \"approval_policy\":\"on-request\"}}}}}}\n",
            stamp(ms)
        )
    }

    /// `lines` re-stamped the way a fork copies them: in one burst from
    /// `ms`, a millisecond apart.
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
    /// model, stamped with its own second. A count written again with the
    /// same total, one of the rate limits alone, a line that is not JSON and
    /// a turn with no stamp count nothing. Negative control: counting every
    /// `token_count` that carries counts, as a per-line sum would, finds
    /// four where two turns were served.
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
        // NEGATIVE CONTROL: every count that carries counts.
        let per_line = text
            .lines()
            .filter(|l| l.ends_with('}') && l.contains("\"last_token_usage\""))
            .count();
        assert_eq!(per_line, 4);
    }

    /// THE FORK RULE (the design's §3.5, §8.4): a fork opens with its
    /// parent's lines rewritten in one burst; that copied prefix counts
    /// nothing, and the fork's own turn — its total continuing from the
    /// copy's — counts once. Negative control: the same fork read without
    /// its `forked_from_id`, as the per-file rule reads it, counts the
    /// parent's two turns again.
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
        // The parent read alone: its own two turns.
        let parents = rows(&mut TurnReader::default(), &parent);
        assert_eq!(turns(&parents).len(), 2);
        // The copies carry the parent's fingerprints exactly.
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
        // NEGATIVE CONTROL: no fork named, the copies count again.
        assert_eq!(fork_keys.len(), 3, "the parent's two again, and its own");
        let parent_keys: Vec<u64> = turns(&parents).iter().map(|t| t.key).collect();
        assert_eq!(fork_keys[..2], parent_keys[..]);
    }

    /// The burst's edge: a line stamped [`FORK_BURST_MS`] after the newest
    /// before it is still the copy; one a millisecond later is the fork's
    /// own. A slow copy whose every step is within the burst stays a copy
    /// however long it runs. Negative control: the same counts in a rollout
    /// that is no fork are turns.
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
        // NEGATIVE CONTROL.
        let own = fork(FORK_BURST_MS).replace(&format!(",\"forked_from_id\":\"{ROOT}\""), "");
        assert_eq!(turns(&rows(&mut TurnReader::default(), &own)).len(), 3);
    }

    /// A FORK'S OWN TOTALS GO ON FROM ITS COPY'S (module header): a
    /// `token_count` its own lines write again with the copy's last total —
    /// as Codex writes one at a turn's start once the thread has a total —
    /// is a repeat, never its parent's last turn counted again; its own turn
    /// after it counts once. Negative control: the same own lines with no
    /// copy before them count that total as a turn.
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
        // A reader stopped inside the copy holds the copy's total so far.
        let mut stopped = TurnReader::default();
        let lines: Vec<&str> = fork.lines().collect();
        for line in &lines[..lines.len() - 3] {
            stopped.line(line);
        }
        assert!(matches!(stopped.prefix, Prefix::Copy(_)));
        assert_eq!(stopped.top, 3150);
        // NEGATIVE CONTROL.
        let alone = [meta(fork_at, SUB, SUB, None), own].concat();
        assert_eq!(turns(&rows(&mut TurnReader::default(), &alone)).len(), 2);
    }

    /// A rollout whose first line names no session (cut, or of a shape this
    /// reader does not know) may be a fork: its first burst counts nothing,
    /// and turns after it count. Negative control: the same lines under a
    /// plain `session_meta` count from the first.
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
        // NEGATIVE CONTROL.
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
        // NEGATIVE CONTROLS.
        assert!(!turn(Some("standard"), "gpt-6-sol").tier);
        assert!(!turn(None, "gpt-6-sol").tier);
        assert_eq!(turn(None, "gpt 6 sol!").model, None);
        assert_eq!(turn(None, "gpt-6-sol").model.as_deref(), Some("gpt-6-sol"));
    }

    /// A whole read streams the rollout's complete lines through the reader:
    /// a torn last line (one still being written) is not read, and a cap
    /// stops the read short and says so. Negative control: the whole file
    /// within its cap is complete.
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
        // A cap the file ends at is a whole read.
        let len = std::fs::metadata(&path).expect("meta").len();
        assert!(
            read_rollout(&path, &mut TurnReader::default(), len, |_| {})
                .expect("read")
                .complete
        );
        // NEGATIVE CONTROL.
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
    /// counted unread. Negative control: another conversation's rollout is
    /// never among them, and it alone is its own.
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
        // NEGATIVE CONTROL.
        let c = conversation_rollouts(home.path(), OTHER);
        assert_eq!(c.root, OTHER);
        assert_eq!(c.files, [other]);
        assert_eq!(c.subagents, 0);
        // A thread with no rollout yet: nothing of anyone's.
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

    /// A PAGINATED THREAD'S FORK COPIES NO COUNT (module header, measured on
    /// 0.163.0 in the lab): a subagent spawned with its parent's turns opens
    /// with its own `session_meta`, its parent's, and the parent's turns —
    /// messages and `turn_context`s, no `token_count` — in one millisecond;
    /// its own `turn_context` follows within 10 ms and, on the stub's instant
    /// answer, its first count within 25 ms, its total from zero. Read from
    /// its first line, that turn counts once on its model. A `/fork` (and
    /// `codex fork`) of a paginated thread copies nothing: its own turn
    /// counts either way. Negative control: the same subagent marked
    /// `legacy` is read with the burst rule, which takes its first turn for
    /// its parent's and loses it — the undercount this rule ends.
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
        // NEGATIVE CONTROL.
        let legacy = rows(&mut TurnReader::default(), &spawn("legacy"));
        assert!(turns(&legacy).is_empty(), "{legacy:?}");
        assert_eq!(legacy.last(), Some(&Row::Copied));
    }

    /// A LEGACY FORK'S OWN SETTINGS LINE ENDS ITS COPY (module header,
    /// measured on 0.163.0): `codex fork <legacy thread> <prompt>` copies its
    /// parent's lines under one stamp, writes its own
    /// `thread_settings_applied` in the same millisecond, and runs the
    /// prompt's turn at once: its next line 14 ms on, its count 37 ms on,
    /// inside any burst. That turn counts once, on its own model, and its
    /// reading is the file's own. A copied settings line (a fork of a fork
    /// copies its parent's), naming the thread it came from, ends nothing.
    /// A reader that stops inside the copy and goes on with its state kept
    /// ends it at the same line. Negative control: the same fork whose own
    /// settings lines name another thread is read by the stamps alone, which
    /// take the prompt's turn and its reading for the copy's.
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
        // Read in two parts, the reader kept between them.
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
        // The readings: the file's own past its copy.
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
        // NEGATIVE CONTROL.
        let other = prompted(OTHER);
        let got = rows(&mut TurnReader::default(), &other);
        assert!(turns(&got).is_empty(), "{got:?}");
        assert_eq!(got.last(), Some(&Row::Copied));
        assert_eq!(shares(&other), Vec::<f64>::new());
    }

    /// The home Codex's records are read under: `CODEX_HOME`, else
    /// `~/.codex`; a relative one is the TUI's directory's, as Codex resolves
    /// it. Negative controls: a relative home with no absolute directory to
    /// resolve it against is none, never another home's records; an empty one
    /// is unset.
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
