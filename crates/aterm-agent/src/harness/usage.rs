// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage view: Claude Code's transcript rows folded into one per-account
//! view, the one-line HUD, and the `harness usage --json` document (schema 1).
//!
//! Everything here is a pure function of its arguments (no clock, environment,
//! network or file); [`super::session_usage::TranscriptCursor`] reads the bytes.
//!
//! Rows are the lines of `~/.claude/projects/<slug>/<uuid>.jsonl`. An assistant
//! row carries `message.{id,model,usage}`, repeated once per content block, so
//! each id counts once; a repeat with larger counts sums only the difference.
//! A row without an id is summed (over-count never hides cost), sidechain rows
//! are real spend, and `usd` is `null`: nothing here knows what a model
//! costs. A caller that priced a row says so in it ([`SpendView::usd`]: the
//! Codex path of `harness usage`, priced by `spend`). Bounds:
//!
//! | bound | value | why |
//! |---|---|---|
//! | [`MAX_LINE_BYTES`] | 16 MiB | a longer row is skipped and counted, never buffered |
//! | [`SEEN_IDS_MAX`] | 4096 | ids are remembered in arrival order, oldest forgotten past this; repeats arrive within a few rows |
//! | [`MAX_ID_BYTES`] | 128 | a longer `message.id` is treated as absent (summed, not deduped) so a hostile id cannot fill the set |
//! | [`MAX_MODELS`] | 64 | further distinct model ids fold into `"(other)"` |
//!
//! The per-account rate-limit windows are filled only from the usage tap's
//! reading for the session's own process ([`AccountView::windows`],
//! [`super::tap`]), its caller's to read; without one they stay unknown
//! (`"windows":{}`, `?%` in the HUD), and `harness limits` prints a painted
//! panel's ([`usage_panel_windows`]), or a Codex `/status` card's
//! ([`codex_status_windows`]). The fold also feeds the window
//! footer's `Σ` tokens and limit wall ([`LimitNoticeRow`]) through
//! [`super::session_usage`].

use std::collections::{BTreeMap, VecDeque};

use aterm_json::{Map, Value};

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// The longest transcript line that is parsed; a longer one is counted in
/// [`TranscriptUsage::rows_skipped_long`] and its bytes are discarded as they
/// stream, so memory stays bounded. Real rows reach about 1.1 MB.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// How many `message.id`s [`TranscriptUsage`] remembers for dedupe, in arrival
/// order; past this the oldest is forgotten and a later repeat of it would be
/// summed again. Repeats arrive within a few rows of each other.
pub const SEEN_IDS_MAX: usize = 4096;

/// A `message.id` longer than this is treated as absent. Real ids are `msg_…`
/// strings of a few dozen bytes.
pub const MAX_ID_BYTES: usize = 128;

/// Distinct model ids [`TranscriptUsage`] keeps separately; further ones fold
/// into the `"(other)"` row.
pub const MAX_MODELS: usize = 64;

/// The longest HUD line, in bytes, that `harness usage` prints.
pub const HUD_MAX_BYTES: usize = 1024;

/// The model key spend is filed under when an assistant row carries none.
pub const UNKNOWN_MODEL: &str = "unknown";

/// The model key spend is filed under past [`MAX_MODELS`] distinct ids.
pub const OTHER_MODEL: &str = "(other)";

// ---------------------------------------------------------------------------
// Numbers: integer or float, never a panic
// ---------------------------------------------------------------------------

fn num_f64(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|f| f.is_finite())
}

/// A non-negative count; a float is floored. Negative or non-finite is absent.
fn num_u64(v: Option<&Value>) -> Option<u64> {
    let f = num_f64(v)?;
    if f < 0.0 {
        return None;
    }
    // `as` saturates at u64::MAX for anything past it.
    Some(f as u64)
}

// ---------------------------------------------------------------------------
// One request's tokens
// ---------------------------------------------------------------------------

/// One request's token counts — the vendor's `usage` object on every
/// assistant transcript row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    /// `input_tokens`.
    pub input: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `cache_creation_input_tokens`: every cache write, both lifetimes.
    pub cache_write: u64,
    /// Of [`Self::cache_write`], the tokens written to the one-hour cache
    /// (`cache_creation.ephemeral_1h_input_tokens`), which list at a higher
    /// price than the five-minute ones; never more than `cache_write`. 0 where
    /// the row carries no split (every write is then a five-minute one).
    pub cache_write_1h: u64,
    /// `cache_read_input_tokens`.
    pub cache_read: u64,
}

impl TokenUsage {
    /// Read the counts; a missing one reads 0. `None` when none of the four
    /// totals is present as a number — that object is not a usage. A row with
    /// a `cache_creation` split and no total sums the split.
    pub(crate) fn from_object(obj: &Map) -> Option<Self> {
        let input = num_u64(obj.get("input_tokens"));
        let output = num_u64(obj.get("output_tokens"));
        let split = obj.get("cache_creation").and_then(Value::as_object);
        let five_min = split.and_then(|s| num_u64(s.get("ephemeral_5m_input_tokens")));
        let one_hour = split.and_then(|s| num_u64(s.get("ephemeral_1h_input_tokens")));
        let cache_write = num_u64(obj.get("cache_creation_input_tokens")).or_else(|| {
            (five_min.is_some() || one_hour.is_some())
                .then(|| five_min.unwrap_or(0).saturating_add(one_hour.unwrap_or(0)))
        });
        let cache_read = num_u64(obj.get("cache_read_input_tokens"));
        if input.is_none() && output.is_none() && cache_write.is_none() && cache_read.is_none() {
            return None;
        }
        let cache_write = cache_write.unwrap_or(0);
        Some(Self {
            input: input.unwrap_or(0),
            output: output.unwrap_or(0),
            cache_write,
            cache_write_1h: one_hour.unwrap_or(0).min(cache_write),
            cache_read: cache_read.unwrap_or(0),
        })
    }

    /// Whether every count is 0: a row that spent nothing.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// The request's prompt: input, cache writes and cache reads together —
    /// what a price by prompt length is decided on.
    #[must_use]
    pub fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    /// Each count the larger of the two.
    #[must_use]
    pub fn max(&self, other: &Self) -> Self {
        Self {
            input: self.input.max(other.input),
            output: self.output.max(other.output),
            cache_write: self.cache_write.max(other.cache_write),
            cache_write_1h: self.cache_write_1h.max(other.cache_write_1h),
            cache_read: self.cache_read.max(other.cache_read),
        }
    }

    /// Each count less the other's, never below 0.
    #[must_use]
    pub fn less(&self, other: &Self) -> Self {
        Self {
            input: self.input.saturating_sub(other.input),
            output: self.output.saturating_sub(other.output),
            cache_write: self.cache_write.saturating_sub(other.cache_write),
            cache_write_1h: self.cache_write_1h.saturating_sub(other.cache_write_1h),
            cache_read: self.cache_read.saturating_sub(other.cache_read),
        }
    }

    /// Each count plus the other's, saturating.
    #[must_use]
    pub fn plus(&self, other: &Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            output: self.output.saturating_add(other.output),
            cache_write: self.cache_write.saturating_add(other.cache_write),
            cache_write_1h: self.cache_write_1h.saturating_add(other.cache_write_1h),
            cache_read: self.cache_read.saturating_add(other.cache_read),
        }
    }
}

// ---------------------------------------------------------------------------
// The transcript: spend per model, deduped by message.id
// ---------------------------------------------------------------------------

/// Token totals for one model over the rows folded so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelSpend {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-creation input tokens, both lifetimes.
    pub cache_write: u64,
    /// Of `cache_write`, the one-hour cache writes ([`TokenUsage::cache_write_1h`]).
    pub cache_write_1h: u64,
    /// Cache-read input tokens.
    pub cache_read: u64,
    /// Distinct messages counted (one per id; each id-less row counts one).
    pub messages: u64,
}

impl ModelSpend {
    fn add(&mut self, t: TokenUsage) {
        self.add_tokens(t);
        self.messages = self.messages.saturating_add(1);
    }

    /// Add a message's token counts without counting another message — the
    /// growth a streamed repeat of one id brings.
    fn add_tokens(&mut self, t: TokenUsage) {
        let sum = self.tokens().plus(&t);
        self.input = sum.input;
        self.output = sum.output;
        self.cache_write = sum.cache_write;
        self.cache_write_1h = sum.cache_write_1h;
        self.cache_read = sum.cache_read;
    }

    /// Add another spend's totals — two files' folds of one session, summed.
    pub(crate) fn add_spend(&mut self, other: &Self) {
        self.add_tokens(other.tokens());
        self.messages = self.messages.saturating_add(other.messages);
    }

    /// The token counts, as one request's are held.
    #[must_use]
    pub fn tokens(&self) -> TokenUsage {
        TokenUsage {
            input: self.input,
            output: self.output,
            cache_write: self.cache_write,
            cache_write_1h: self.cache_write_1h,
            cache_read: self.cache_read,
        }
    }
}

/// What [`TranscriptUsage::fold_line`] did with a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// An assistant row with usage, counted into its model's spend.
    Summed,
    /// A repeat of an id already counted, with the same usage: folded away.
    Duplicate,
    /// A repeat of a counted id whose usage differed (a streamed message's
    /// later row): each count was raised to the largest seen and the
    /// difference summed.
    Conflict,
    /// A row that is not an assistant row (user, progress, summary, …).
    NotAssistant,
    /// A main-chain `turn_duration` row: the turn it closes was folded into
    /// the session's working time ([`TranscriptUsage::busy_ms`]).
    Turn,
    /// An assistant row with no usage object.
    NoUsage,
    /// Not JSON, not an object, or not UTF-8.
    Unparsed,
    /// Longer than [`MAX_LINE_BYTES`].
    TooLong,
    /// Whitespace only.
    Blank,
}

/// The bounded set of ids already counted, each with the largest usage seen
/// for it so far, count by count.
#[derive(Debug, Clone, Default)]
struct SeenIds {
    order: VecDeque<String>,
    largest: BTreeMap<String, TokenUsage>,
}

enum Seen {
    New,
    Same,
    /// A differing repeat: how much each count grew past the largest held.
    Differs(TokenUsage),
}

impl SeenIds {
    fn remember(&mut self, id: &str, usage: TokenUsage) -> Seen {
        if let Some(held) = self.largest.get_mut(id) {
            if *held == usage {
                return Seen::Same;
            }
            let grown = held.max(&usage);
            let delta = grown.less(held);
            *held = grown;
            return Seen::Differs(delta);
        }
        self.largest.insert(id.to_owned(), usage);
        self.order.push_back(id.to_owned());
        self.shed(SEEN_IDS_MAX);
        Seen::New
    }

    /// Forget all but the newest `keep` ids.
    fn shed(&mut self, keep: usize) {
        while self.order.len() > keep {
            if let Some(old) = self.order.pop_front() {
                self.largest.remove(&old);
            }
        }
    }
}

/// Spend per model, folded one transcript line at a time. Every count below
/// is exposed so a view can say what it did not count.
#[derive(Debug, Clone, Default)]
pub struct TranscriptUsage {
    per_model: BTreeMap<String, ModelSpend>,
    seen: SeenIds,
    /// The model the newest summed row named ([`Self::last_model`]).
    last_model: Option<String>,
    /// Lines offered, of every kind.
    pub rows: u64,
    /// Lines that were not JSON, not an object, or not UTF-8.
    pub rows_unparsed: u64,
    /// Lines longer than [`MAX_LINE_BYTES`], skipped whole.
    pub rows_skipped_long: u64,
    /// Rows with `type: "assistant"`.
    pub assistant_rows: u64,
    /// Assistant rows with no `message.usage` object.
    pub rows_without_usage: u64,
    /// Assistant rows with usage but no usable `message.id`: summed, not
    /// deduped.
    pub rows_without_id: u64,
    /// Repeats folded away (same id, same usage) plus conflicts.
    pub duplicates: u64,
    /// Repeats whose usage differed from the largest held for the id; each
    /// raised the counts and summed the difference.
    pub conflicting_repeats: u64,
    /// Assistant rows with `isSidechain: true`; summed like any other.
    pub sidechain_rows: u64,
    /// The newest limit notice Claude Code wrote into the transcript, per
    /// window it names ([`LimitNoticeRow`]).
    limit_notices: BTreeMap<String, LimitNoticeRow>,
    /// Unix seconds of the newest row the API served ([`Self::served_at`]).
    served_at: Option<i64>,
    /// The time Claude was working ([`Self::busy_ms`]).
    busy: BusySpans,
}

/// The time Claude was WORKING in one transcript: the union of its main
/// chain's turns, each `[timestamp − durationMs, timestamp]` of a
/// `{"type":"system","subtype":"turn_duration"}` row (MEASURED: Claude Code
/// writes one at every turn's end — 2.1.282, .283, .284 and .286 on this
/// box). The UNION, not the sum: a turn that waited on a workflow's
/// subagents is written when they finish, so it can overlap the turns after
/// it (the sum read 8.04h against a union of 7.88h in one measured session,
/// 19.27h against 18.77h in another); the first-to-last span is mostly idle
/// (32.3h and 31.8h for the same two).
///
/// Held as disjoint spans in time order with their running sum: a new turn
/// merges with the spans it overlaps, which are at the back in a transcript
/// written in time order, so a row costs O(1) amortised. At most
/// [`BUSY_SPANS_MAX`] spans are kept; past it the oldest leaves the deque
/// (its time stays in the sum), and a later turn reaching back over a span
/// that left counts that overlap twice — the stated bound, far past any
/// measured session (a span per turn; 528 turns in the 150 newest
/// transcripts here).
///
/// What it does NOT count, MEASURED in the 2.1.286 bundle: a turn the person
/// interrupts (Esc) writes no row (`if (… && !signal.aborted)` guards the
/// append), and neither does a turn in flight when the process ends (a
/// relaunch, a crash) — that time is lost, an undercount. A turn still
/// running is not counted until it ends: the figure never goes down, and
/// Claude's own spinner times the turn in flight.
#[derive(Debug, Clone, Default)]
struct BusySpans {
    /// Disjoint `[start, end)` spans in unix milliseconds, in time order.
    spans: VecDeque<(i64, i64)>,
    /// The sum of every span's length, those that left the deque included.
    total_ms: u64,
}

/// The most disjoint spans [`BusySpans`] keeps.
pub const BUSY_SPANS_MAX: usize = 4096;

impl BusySpans {
    /// Fold one turn `[start, end)`.
    fn note(&mut self, start: i64, end: i64) {
        if end <= start {
            return;
        }
        let len = |(a, b): (i64, i64)| u64::try_from(b - a).unwrap_or(0);
        // The spans the turn touches: from the first ending at or after its
        // start to the last starting at or before its end.
        let from = self.spans.partition_point(|&(_, b)| b < start);
        let to = self.spans.partition_point(|&(a, _)| a <= end);
        let mut merged = (start, end);
        for span in self.spans.drain(from..to.max(from)) {
            merged = (merged.0.min(span.0), merged.1.max(span.1));
            self.total_ms = self.total_ms.saturating_sub(len(span));
        }
        self.spans.insert(from, merged);
        self.total_ms = self.total_ms.saturating_add(len(merged));
        if self.spans.len() > BUSY_SPANS_MAX {
            self.spans.pop_front();
        }
    }
}

/// A limit notice as Claude Code wrote it into the transcript: an assistant
/// row with `isApiErrorMessage: true` and `error: rate_limit` whose text is
/// the notice (`You've hit your session limit · resets 3pm (…)`), stamped with
/// the row's own `timestamp` (the login wall's row shape,
/// `upgrade::transcript_login_wall`). A rejected rate limit becomes such a row;
/// the usage warning is UI-only. The row's time survives re-rendering, so the
/// reset is placed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitNoticeRow {
    /// The reset the notice names, as its text prints it
    /// (`aterm_phase::notice_reset`, the screen reader's own grammar).
    pub reset_text: Option<String>,
    /// Unix seconds of the row's `timestamp`.
    pub at: i64,
}

/// Unix seconds of a transcript row's own `timestamp` (the workspace's
/// RFC 3339 reader, which refuses an offset it would misread).
pub(crate) fn row_time(obj: &Map) -> Option<i64> {
    obj.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::upgrade_models::parse_utc)
        .and_then(|t| i64::try_from(t).ok())
}

/// Unix MILLISECONDS of a transcript row's own `timestamp`: [`row_time`]
/// plus its fraction's first three digits (Claude Code stamps
/// `2026-09-29T15:11:53.390Z`).
fn row_time_ms(obj: &Map) -> Option<i64> {
    let stamp = obj.get("timestamp").and_then(Value::as_str)?;
    let secs = row_time(obj)?;
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

/// The window a limit notice's text NAMES, reached: `session limit` /
/// `5-hour limit` is `five_hour`, a weekly one `seven_day`; `None` for a
/// notice that names none (`You've hit your limit` — the vendor prints it
/// for a weekly wall too, MEASURED in the 2.1.278 bundle), and for any other
/// wall. Which wall it is at all is `aterm_phase::classify_wall`'s answer,
/// the screen reader's.
#[must_use]
pub fn notice_window(message: &str) -> Option<&'static str> {
    let head = message.replace('\u{2019}', "'").to_ascii_lowercase();
    let names = |phrases: &[&str]| phrases.iter().any(|p| head.contains(p));
    match aterm_phase::classify_wall(message) {
        Some(aterm_phase::WallKind::UsageSession)
            if names(&["session limit", "5-hour limit", "five-hour limit"]) =>
        {
            Some("five_hour")
        }
        Some(aterm_phase::WallKind::UsageWeekly)
            if names(&["weekly", "7-day limit", "seven-day limit"]) =>
        {
            Some("seven_day")
        }
        _ => None,
    }
}

impl TranscriptUsage {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spend by model id, in key order.
    pub fn per_model(&self) -> &BTreeMap<String, ModelSpend> {
        &self.per_model
    }

    /// The model id the newest summed main-chain row named: the session's
    /// model as its transcript last recorded it. Sidechain rows and rows naming
    /// none ([`UNKNOWN_MODEL`], Claude Code's `<synthetic>`) do not move it.
    /// `None` before such a row.
    pub fn last_model(&self) -> Option<&str> {
        self.last_model.as_deref()
    }

    /// The sum over every model.
    #[cfg(test)]
    pub(crate) fn total(&self) -> ModelSpend {
        let mut t = ModelSpend::default();
        for s in self.per_model.values() {
            t.add_spend(s);
        }
        t
    }

    /// Fold one line. Never panics; a hostile line is [`Fold::Unparsed`] or
    /// [`Fold::TooLong`]. A line [`cannot_be_assistant`] rejects never reaches
    /// the JSON parser.
    pub fn fold_line(&mut self, line: &str) -> Fold {
        self.rows = self.rows.saturating_add(1);
        if line.len() > MAX_LINE_BYTES {
            self.rows_skipped_long = self.rows_skipped_long.saturating_add(1);
            return Fold::TooLong;
        }
        let line = line.trim();
        if line.is_empty() {
            return Fold::Blank;
        }
        if cannot_be_assistant(line) {
            return Fold::NotAssistant;
        }
        let Ok(value) = super::transcript::usage(line) else {
            self.rows_unparsed = self.rows_unparsed.saturating_add(1);
            return Fold::Unparsed;
        };
        let Some(obj) = value.as_object() else {
            self.rows_unparsed = self.rows_unparsed.saturating_add(1);
            return Fold::Unparsed;
        };
        match obj.get("type").and_then(Value::as_str) {
            Some("assistant") => {}
            Some("system")
                if obj.get("subtype").and_then(Value::as_str) == Some("turn_duration") =>
            {
                self.note_turn(obj);
                return Fold::Turn;
            }
            _ => return Fold::NotAssistant,
        }
        let api_error = obj.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true);
        // Only the vendor's API-error rows need content for a limit notice.
        // Ordinary usage rows keep tool payloads out of the materialized
        // projection; the rare notice gets the text-block projection too.
        if api_error
            && let Ok(notice) = super::transcript::conversation(line)
            && let Some(notice) = notice.as_object()
        {
            self.note_limit_notice(notice);
        }
        self.assistant_rows = self.assistant_rows.saturating_add(1);
        let sidechain = obj.get("isSidechain").and_then(Value::as_bool) == Some(true);
        if sidechain {
            self.sidechain_rows = self.sidechain_rows.saturating_add(1);
        }
        let usage = obj
            .get("message")
            .and_then(Value::as_object)
            .and_then(|m| m.get("usage"))
            .and_then(Value::as_object)
            .and_then(TokenUsage::from_object);
        let Some(usage) = usage else {
            self.rows_without_usage = self.rows_without_usage.saturating_add(1);
            return Fold::NoUsage;
        };
        // `message` is an object here: the usage came out of it.
        let message = obj.get("message").and_then(Value::as_object);
        let model = message
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(UNKNOWN_MODEL);
        let id = message
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= MAX_ID_BYTES);
        // A response the API served: not an API-error row, from a real model
        // id (Claude Code's own rows are `<synthetic>`), and below, a message
        // not already counted at these figures. The login wall lifts by the
        // same rule (`upgrade::LoginRow::Lifted`).
        let served = !api_error && model != UNKNOWN_MODEL && super::upgrade::is_model_id(model);
        let Some(id) = id else {
            self.rows_without_id = self.rows_without_id.saturating_add(1);
            self.add(model, usage, sidechain);
            if served {
                self.note_served(obj);
            }
            return Fold::Summed;
        };
        match self.seen.remember(id, usage) {
            Seen::New => {
                self.add(model, usage, sidechain);
                if served {
                    self.note_served(obj);
                }
                Fold::Summed
            }
            Seen::Same => {
                self.duplicates = self.duplicates.saturating_add(1);
                Fold::Duplicate
            }
            Seen::Differs(growth) => {
                self.duplicates = self.duplicates.saturating_add(1);
                self.conflicting_repeats = self.conflicting_repeats.saturating_add(1);
                let key = self.model_key(model).to_owned();
                self.per_model.entry(key).or_default().add_tokens(growth);
                // Only a message that GREW is a response served again: a
                // repeat at or below the figures held is an old one.
                if served && growth != TokenUsage::default() {
                    self.note_served(obj);
                }
                Fold::Conflict
            }
        }
    }

    /// Unix seconds of the newest response the API served in this file: an
    /// assistant row with usage from a real model (not `<synthetic>`, not
    /// `isApiErrorMessage`) whose message is new or grew. A limit wall with a
    /// response served after its notice no longer blocks (`/limit-reset`,
    /// extra usage, another account).
    pub fn served_at(&self) -> Option<i64> {
        self.served_at
    }

    /// Milliseconds Claude was working in this transcript: the union of its
    /// main chain's finished turns ([`BusySpans`]). `0` before a turn ends.
    pub fn busy_ms(&self) -> u64 {
        self.busy.total_ms
    }

    /// Fold a `turn_duration` row: the turn `[timestamp − durationMs,
    /// timestamp]`. A sidechain's row, or one without both fields, is not
    /// a main turn and is left out.
    fn note_turn(&mut self, obj: &Map) {
        if obj.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let (Some(end), Some(ms)) = (
            row_time_ms(obj),
            num_u64(obj.get("durationMs")).and_then(|ms| i64::try_from(ms).ok()),
        ) else {
            return;
        };
        self.busy.note(end.saturating_sub(ms), end);
    }

    fn note_served(&mut self, obj: &Map) {
        if let Some(at) = row_time(obj) {
            self.served_at = Some(self.served_at.map_or(at, |held| held.max(at)));
        }
    }

    /// The newest limit notice written into the transcript, per window it
    /// names (`five_hour`, `seven_day`).
    pub fn limit_notices(&self) -> &BTreeMap<String, LimitNoticeRow> {
        &self.limit_notices
    }

    /// Keep an API-error row that is a limit notice naming its window.
    fn note_limit_notice(&mut self, obj: &Map) {
        let message = obj.get("message").and_then(Value::as_object);
        let text = match message.and_then(|m| m.get("content")) {
            Some(Value::String(t)) => Some(t.as_str()),
            Some(Value::Array(blocks)) => blocks.iter().find_map(|b| {
                let b = b.as_object()?;
                (b.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| b.get("text").and_then(Value::as_str))
                    .flatten()
            }),
            _ => None,
        };
        let Some(text) = text else {
            return;
        };
        let (Some(window), Some(at)) = (notice_window(text), row_time(obj)) else {
            return;
        };
        let row = LimitNoticeRow {
            reset_text: aterm_phase::notice_reset(text),
            at,
        };
        if self
            .limit_notices
            .get(window)
            .is_none_or(|held| held.at <= row.at)
        {
            self.limit_notices.insert(window.to_owned(), row);
        }
    }

    /// Fold one line's raw bytes: [`Self::fold_line`] when they are UTF-8,
    /// else counted unparsed.
    pub(crate) fn fold_bytes(&mut self, line: &[u8]) {
        match std::str::from_utf8(line) {
            Ok(s) => {
                self.fold_line(s);
            }
            Err(_) => {
                self.rows = self.rows.saturating_add(1);
                self.rows_unparsed = self.rows_unparsed.saturating_add(1);
            }
        }
    }

    /// Add another fold's spend and counters to this one (the files of one
    /// session). The [`MAX_MODELS`] bound holds. Dedupe sets are not merged
    /// (each file deduped within itself). Limit notices and the served time
    /// are not summed: the limit wall is the main transcript's
    /// (`session_usage::SessionUsage::main`).
    pub(crate) fn absorb(&mut self, other: &Self) {
        for (model, spend) in &other.per_model {
            let key = self.model_key(model).to_owned();
            self.per_model.entry(key).or_default().add_spend(spend);
        }
        for (mine, theirs) in [
            (&mut self.rows, other.rows),
            (&mut self.rows_unparsed, other.rows_unparsed),
            (&mut self.rows_skipped_long, other.rows_skipped_long),
            (&mut self.assistant_rows, other.assistant_rows),
            (&mut self.rows_without_usage, other.rows_without_usage),
            (&mut self.rows_without_id, other.rows_without_id),
            (&mut self.duplicates, other.duplicates),
            (&mut self.conflicting_repeats, other.conflicting_repeats),
            (&mut self.sidechain_rows, other.sidechain_rows),
        ] {
            *mine = mine.saturating_add(theirs);
        }
        // The session's model is the first absorbed fold's that names one:
        // `SessionUsage::total_fold` absorbs the main transcript first, and
        // subagent rows are sidechain rows, which name none.
        if self.last_model.is_none() {
            self.last_model.clone_from(&other.last_model);
        }
    }

    /// Forget all but the newest `keep` message ids, for a finished file: a
    /// message still streaming when the file paused is then raised, not
    /// counted again, if the file grows.
    pub(crate) fn shed_seen(&mut self, keep: usize) {
        self.seen.shed(keep);
        self.seen.order.shrink_to_fit();
    }

    /// How many message ids the dedupe set holds.
    #[cfg(test)]
    pub(crate) fn seen_ids(&self) -> usize {
        self.seen.order.len()
    }

    /// Count one line longer than [`MAX_LINE_BYTES`] that a streaming reader
    /// discarded without holding it.
    pub(crate) fn note_long_line(&mut self) {
        self.rows = self.rows.saturating_add(1);
        self.rows_skipped_long = self.rows_skipped_long.saturating_add(1);
    }

    /// The key `model`'s spend is filed under: its own id while fewer than
    /// [`MAX_MODELS`] are held (or it is one of them), else [`OTHER_MODEL`].
    /// The one rule for every writer of `per_model`.
    fn model_key<'a>(&self, model: &'a str) -> &'a str {
        if self.per_model.contains_key(model) || self.per_model.len() < MAX_MODELS {
            model
        } else {
            OTHER_MODEL
        }
    }

    /// Sum one row's usage under `model`. A main-chain row naming a model id
    /// also sets [`Self::last_model`]; Claude Code's `<synthetic>` rows (an API
    /// error, the usage-limit notice, `No response requested.`) never move it.
    fn add(&mut self, model: &str, usage: TokenUsage, sidechain: bool) {
        if !sidechain && model != UNKNOWN_MODEL && super::upgrade::is_model_id(model) {
            self.last_model = Some(model.to_owned());
        }
        let key = self.model_key(model).to_owned();
        self.per_model.entry(key).or_default().add(usage);
    }
}

/// Whether this line provably is not an assistant row, decided on the raw
/// bytes so the JSON parser never sees it. Sound: `"assistant"` in JSON is
/// either literal or uses a `\u` escape, so a line with neither cannot say
/// `"type":"assistant"`. A malformed object-shaped line answers
/// [`Fold::NotAssistant`] instead of [`Fold::Unparsed`].
pub(crate) fn cannot_be_assistant(line: &str) -> bool {
    line.starts_with('{')
        && line.ends_with('}')
        && !line.contains("assistant")
        && !line.contains(TURN_DURATION)
        && !line.contains("\\u")
}

/// The subtype of the row Claude Code writes at every turn's end: it is let
/// past [`cannot_be_assistant`]'s cut the same way, by its literal bytes (a
/// `\u`-escaped spelling is parsed anyway), and folded into
/// [`TranscriptUsage::busy_ms`].
const TURN_DURATION: &str = "turn_duration";

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Spend on one model, as the view shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendView {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-creation tokens.
    pub cache_write: u64,
    /// Cache-read tokens.
    pub cache_read: u64,
    /// List dollars, where the caller priced every turn of it (Codex's
    /// rollouts, `spend::fold_conversation`); `None` otherwise: nothing in
    /// this module knows a price.
    pub usd: Option<f64>,
}

/// One rate-limit window of an account as the usage tap read it
/// ([`super::tap::Window`]): whole percent used, the reset, and how old the
/// reading is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowView {
    /// `used_percentage`.
    pub used_pct: u32,
    /// `resets_at`, Unix seconds.
    pub resets_at: i64,
    /// Seconds since the tap wrote it, at the view's `as_of`.
    pub age_s: u64,
}

/// One account: its spend (per model), the model its live session runs, and
/// the windows its usage tap read.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    /// The owner's label for the account (`accounts.toml`). `harness usage`
    /// names the roster row its Claude Code's login names, where two or more
    /// are listed, and says `account` where it cannot tell.
    pub label: String,
    /// Whether this is the account the live session runs under: always, for
    /// the one account `harness usage` shows. Not the machine's active
    /// account, which is `harness accounts --json`'s.
    pub active: bool,
    /// Spend by model id.
    pub spend: BTreeMap<String, SpendView>,
    /// The session's model id ([`TranscriptUsage::last_model`], taken by
    /// [`Self::add_transcript`]).
    pub model: Option<String>,
    /// The rate-limit windows by the vendor's name (`five_hour`,
    /// `seven_day`), from the usage tap's reading for the session's own
    /// process ([`Self::add_reading`]); empty with none.
    pub windows: BTreeMap<String, WindowView>,
}

impl AccountView {
    /// An account with nothing known about it yet.
    pub fn new(label: impl Into<String>, active: bool) -> Self {
        Self {
            label: label.into(),
            active,
            spend: BTreeMap::new(),
            model: None,
            windows: BTreeMap::new(),
        }
    }

    /// Take the windows of a usage tap's `reading` ([`super::tap::Reading`]),
    /// as old as `as_of` makes it.
    pub fn add_reading(&mut self, reading: &super::tap::Reading, as_of: i64) {
        let age_s = u64::try_from(as_of.saturating_sub(reading.at)).unwrap_or(0);
        for (name, window) in [
            ("five_hour", reading.five_hour),
            ("seven_day", reading.seven_day),
        ] {
            if let Some(w) = window {
                self.windows.insert(
                    name.to_owned(),
                    WindowView {
                        used_pct: w.pct,
                        resets_at: w.resets_at,
                        age_s,
                    },
                );
            }
        }
    }

    /// Take the per-model spend from a transcript fold, and the model its
    /// newest summed row names.
    pub fn add_transcript(&mut self, usage: &TranscriptUsage) {
        for (model, s) in spend_from_transcript(usage) {
            self.spend.insert(model, s);
        }
        if let Some(model) = usage.last_model() {
            self.model = Some(model.to_owned());
        }
    }
}

/// The whole view: every account, and the instant it describes.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageView {
    /// Epoch seconds the view is as of (injected by the caller).
    pub as_of: i64,
    /// Accounts in the caller's order; the HUD shows the first active one.
    pub accounts: Vec<AccountView>,
}

impl UsageView {
    /// A view of no accounts.
    pub fn new(as_of: i64) -> Self {
        Self {
            as_of,
            accounts: Vec::new(),
        }
    }

    /// The account the HUD describes: the first active one, else the first.
    pub fn shown(&self) -> Option<&AccountView> {
        self.accounts
            .iter()
            .find(|a| a.active)
            .or_else(|| self.accounts.first())
    }
}

// ---------------------------------------------------------------------------
// The `/usage` panel, read off aterm's own grid
// ---------------------------------------------------------------------------

/// One rate-limit window Claude Code painted on its `/usage` panel, or Codex
/// on its `/status` card ([`codex_status_windows`]), before its reset text is
/// placed on a clock; the producer of [`super::source::Source::Grid`]. The
/// panel and the card are the only surfaces that paint a quota figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelWindow {
    /// The window key this row names (the vendor's own: `five_hour`, …; on
    /// Codex's card, the same keys for the windows its labels name by length,
    /// `5h` and `Weekly`, another length's label folded: [`codex_window_key`]).
    pub name: String,
    /// The whole-number percent printed (`7% used` → 7). The vendor floors its
    /// own figure, so this is the painted integer, never re-derived. Codex's
    /// card paints what is LEFT (`99% left`), and this is its complement
    /// (`1`): the painted integer, turned round.
    pub used_pct: u32,
    /// The text after `Resets `, exactly as painted and not placed on a clock
    /// (on Codex's card, inside `(resets …)`). `None` when the row carried no
    /// reset.
    pub reset_text: Option<String>,
}

/// The panel titles with a known window key, and the key each names.
/// `Current week (Fable)` is the vendor's `seven_day_overage_included` bucket,
/// which the statusLine schema does not carry, so the painted panel is the
/// only readable source for it.
const PANEL_TITLES: [(&str, &str); 5] = [
    ("Current session", "five_hour"),
    ("Current week (all models)", "seven_day"),
    ("Current week (Sonnet only)", "seven_day_sonnet"),
    ("Current week (Fable)", "seven_day_overage_included"),
    ("Spend limit", "spend_limit"),
];

/// How many rows past a title row the reader looks for that window's figure.
/// The wide layout (panel width ≥ 62) needs 2 (title, `<bar> N% used`,
/// `Resets …`); the narrow layout needs 5 (title with the reset wrapped below,
/// the bar, which itself wraps at 100 %, a blank, then `N% used`).
const PANEL_BLOCK_ROWS: usize = 7;

/// The longest window key this reader will mint for an unmeasured display
/// name, so a hostile or absurd title cannot grow the view's key space.
const PANEL_KEY_CAP: usize = 40;

/// A block-element glyph, the run the vendor draws its meter with.
fn is_bar_glyph(c: char) -> bool {
    ('\u{2580}'..='\u{259f}').contains(&c)
}

/// The whole-number percent in `… 62% used`, or `None`.
fn pct_used(s: &str) -> Option<u32> {
    let at = s.find("% used")?;
    let head = &s[..at];
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    head[head.len() - digits..].parse().ok()
}

/// The reset text after `Resets `, cut before the meter or the figure that
/// can follow it on a wrapped row. `None` when there is no reset.
fn reset_after(s: &str) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    let at = lower.find("resets ")? + "resets ".len();
    let rest = s.get(at..)?;
    let cut = rest
        .char_indices()
        .find(|(_, c)| is_bar_glyph(*c))
        .map(|(i, _)| i)
        .or_else(|| {
            rest.find("% used").map(|i| {
                rest[..i]
                    .trim_end_matches(|c: char| c.is_ascii_digit())
                    .trim_end()
                    .len()
            })
        })
        .unwrap_or(rest.len());
    let text = rest.get(..cut)?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Lowercase, non-alphanumeric runs folded to `_`, bounded: the key half of
/// an UNMEASURED `Current week (<display name>)` title, and of a Codex card's
/// label no window key names ([`codex_window_key`]).
fn panel_key_slug(display: &str) -> Option<String> {
    let mut out = String::new();
    for c in display.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= PANEL_KEY_CAP {
            break;
        }
    }
    let out = out.trim_matches('_').to_owned();
    (!out.is_empty()).then_some(out)
}

/// The window key a panel title names, and what the title row carries after
/// it (the narrow layout's ` · Resets …`, or the text form's
/// `: 62% used · resets …`). An unknown `Current week (<x>)` mints
/// `seven_day_<slug>`, unverified as a vendor key; known keys are matched
/// first, so it never lands on one.
fn panel_title(row: &str) -> Option<(String, &str)> {
    let t = row.trim();
    for (title, key) in PANEL_TITLES {
        if let Some(rest) = t.strip_prefix(title) {
            // `Current week (all models)` must not also match a longer
            // display name, so the remainder may not open another word.
            if rest.starts_with(|c: char| c.is_alphanumeric()) {
                continue;
            }
            return Some((key.to_owned(), rest));
        }
    }
    let rest = t.strip_prefix("Current week (")?;
    let close = rest.find(')')?;
    let key = panel_key_slug(rest.get(..close)?)?;
    Some((format!("seven_day_{key}"), rest.get(close + 1..)?))
}

/// The rate-limit windows Claude Code painted on its `/usage` panel, read off
/// the rows of aterm's own parsed grid.
///
/// Wide (panel ≥ 62 columns):
///
/// ```text
///    Current session
///    ███▌                                     7% used
///    Resets 1:20pm (America/Los_Angeles)
/// ```
///
/// Narrow, where the reset wraps onto the row below the title:
///
/// ```text
///    Current session · Resets 1:20pm
///    (America/Los_Angeles)
///    ███▊
///
///    7% used
/// ```
///
/// The copied text form (`Current session: 7% used · resets 1:20pm`) is read
/// by the same path as the narrow layout's title row.
///
/// Fewer than two windows reads as none: the panel always renders
/// `Current session` and `Current week (all models)` together, so a lone title
/// (a quoted line, a pasted screen) is refused. A quoted copy of two or more
/// whole rows would still be read. The text is evidence of what was drawn,
/// never an instruction.
#[must_use]
pub fn usage_panel_windows(rows: &[String]) -> Vec<PanelWindow> {
    let titles: Vec<(usize, String, String)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| panel_title(r).map(|(key, rest)| (i, key, rest.to_owned())))
        .collect();
    let mut out: Vec<PanelWindow> = Vec::new();
    for (n, (i, key, head)) in titles.iter().enumerate() {
        let stop = titles
            .get(n + 1)
            .map_or(rows.len(), |(j, _, _)| *j)
            .min(i.saturating_add(1 + PANEL_BLOCK_ROWS))
            .min(rows.len());
        let head = head.trim_start_matches([':', '·', ' ']).trim();
        let mut pct = pct_used(head);
        let mut reset = reset_after(head);
        // The narrow layout wraps a long reset onto the rows below the title,
        // up to the meter.
        let mut wrapping = reset.is_some() && pct.is_none();
        for row in rows.get(i + 1..stop).unwrap_or_default() {
            let t = row.trim();
            if wrapping {
                if t.is_empty() || t.contains("% used") || t.chars().any(is_bar_glyph) {
                    wrapping = false;
                } else if let Some(r) = reset.as_mut() {
                    r.push(' ');
                    r.push_str(t);
                    continue;
                }
            }
            if pct.is_none() {
                pct = pct_used(t);
            }
            if reset.is_none() {
                reset = reset_after(t);
            }
            if pct.is_some() && reset.is_some() {
                break;
            }
        }
        if let Some(used_pct) = pct {
            out.push(PanelWindow {
                name: key.clone(),
                used_pct,
                reset_text: reset.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty()),
            });
        }
    }
    if out.len() < 2 {
        return Vec::new();
    }
    out
}

// ---------------------------------------------------------------------------
// Codex's `/status` card, read off aterm's own grid
// ---------------------------------------------------------------------------

/// What the first row inside a Codex card says (`>_ OpenAI Codex (v0.156.1)`):
/// the `/status` card's, and the session card's it opens on, which carries no
/// limit row.
const CODEX_CARD_HEADER: &str = ">_ OpenAI Codex";

/// What a card row holds between its side rules (`│ … │`); `None` for a row
/// that is not one.
fn card_row(row: &str) -> Option<&str> {
    row.trim()
        .strip_prefix('\u{2502}')?
        .strip_suffix('\u{2502}')
}

/// The words inside `(resets …)` on a card row, or `None`.
fn card_reset(s: &str) -> Option<String> {
    let at = s.find("(resets ")? + "(resets ".len();
    let rest = s.get(at..)?;
    let text = rest.get(..rest.find(')')?)?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The windows the card names by their length (codex-rs labels a window
/// `5h`, `weekly`, …), keyed as Claude Code's panel keys the same windows
/// ([`PANEL_TITLES`]): Codex's own limit's are aterm's `five_hour` and
/// `seven_day` (`docs/DESIGN-account-stats-2026-10-15.md` §7.2). Any other
/// length is its label folded ([`panel_key_slug`]: `monthly`).
const CODEX_CARD_WINDOWS: [(&str, &str); 2] = [("5h", "five_hour"), ("weekly", "seven_day")];

/// A `<label> limit:` row of the card, its side rules gone: the label and
/// what follows the colon.
fn codex_limit_label(inner: &str) -> Option<(&str, &str)> {
    let (label, value) = inner.trim().split_once(':')?;
    Some((label.trim().strip_suffix(" limit")?, value))
}

/// What a limit row says after its label: the meter, `N% left`, and
/// `(resets …)` where it fits on the row; read as what is used, the
/// complement of what is left, and the reset's words.
fn codex_limit_figure(value: &str) -> Option<(u32, Option<String>)> {
    let at = value.find("% left")?;
    let head = &value[..at];
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let left: u32 = head[head.len() - digits..].parse().ok()?;
    Some((100u32.checked_sub(left)?, card_reset(&value[at..])))
}

/// The key of the window a limit row labels ([`CODEX_CARD_WINDOWS`]).
/// Another model's limit (`limit_name`, the accounts design's F11) labels
/// its windows by length too, so its key carries its name, folded, in front
/// (`<name>_five_hour`): under a `<name> limit:` row that heads it
/// (`family`), or in a label of its own (`<name> 5h`, a limit with one
/// window). `None` for a label that folds to nothing.
fn codex_window_key(label: &str, family: Option<&str>) -> Option<String> {
    let known = |label: &str| {
        CODEX_CARD_WINDOWS
            .iter()
            .find(|(length, _)| label.eq_ignore_ascii_case(length))
            .map(|(_, key)| (*key).to_owned())
    };
    let named = |name: &str, window: String| match panel_key_slug(name) {
        Some(name) => format!("{name}_{window}"),
        None => window,
    };
    if known(label).is_none()
        && let Some((name, window)) = label
            .rsplit_once(' ')
            .and_then(|(name, length)| Some((name, known(length)?)))
    {
        return Some(named(name, window));
    }
    let window = known(label).or_else(|| panel_key_slug(label))?;
    Some(match family {
        Some(name) => named(name, window),
        None => window,
    })
}

/// The windows one card's rows name, read row by row ([`Self::feed`]): each
/// `<label> limit:` row with a figure is a window, a `<name> limit:` row with
/// none heads another model's limit, and a `(resets …)` row right under a
/// window is that window's reset where its own row had none.
#[derive(Default)]
struct CardWindows {
    windows: Vec<PanelWindow>,
    /// The last row read was a window (its reset may be painted on the next).
    after_window: bool,
    /// The other model's limit a row above headed (its `<name> limit:`).
    family: Option<String>,
}

impl CardWindows {
    /// One card row, `inner` its text (a boxed row's between its side rules).
    fn feed(&mut self, inner: &str) {
        let after_window = std::mem::replace(&mut self.after_window, false);
        if let Some((label, value)) = codex_limit_label(inner) {
            if value.trim().is_empty() {
                self.family = Some(label.to_owned());
            } else if let Some((used_pct, reset_text)) = codex_limit_figure(value)
                && let Some(name) = codex_window_key(label, self.family.as_deref())
            {
                self.windows.push(PanelWindow {
                    name,
                    used_pct,
                    reset_text,
                });
                self.after_window = true;
            }
        } else if after_window
            && inner.trim().starts_with("(resets ")
            && let Some(window) = self.windows.last_mut()
            && window.reset_text.is_none()
        {
            window.reset_text = card_reset(inner);
        }
    }
}

/// The rate-limit windows Codex painted on its `/status` card, read off the
/// rows of aterm's own parsed grid. Two layouts are read, measured:
///
/// * codex 0.156.1 (2026-09-24) draws the card in a box:
///
/// ```text
/// ╭────────────────────────────────────────────────────────────────────────╮
/// │  >_ OpenAI Codex (v0.156.1)                                            │
/// │  …                                                                     │
/// │  Context window:  99% left (14.8K used / 258K)                         │
/// │  Weekly limit:    [████████████████████] 99% left (resets 13:24 on 1 Oct) │
/// ╰────────────────────────────────────────────────────────────────────────╯
/// ```
///
///   Only a box whose first row is Codex's header ([`CODEX_CARD_HEADER`])
///   and whose bottom rule is on the grid is read.
/// * codex 0.163.0 (2026-10-16, the lab) draws it with no box, under the
///   `/status` it answers, every row at column 2, its resets on a 12-hour
///   clock where the TUI's is one:
///
/// ```text
/// /status
///
///   >_ OpenAI Codex (v0.163.0)
///   …
///   Context window:      99% left (15.7K used / 258K)
///   5h limit:            [██████████████░░░░░░] 70% left (resets 12:15 PM)
///   Weekly limit:        [██████████████████░░] 90% left (resets 11:15 AM on 21 Oct)
///   Credits:             13 credits
///
/// › Ask Codex to do anything
/// ```
///
///   Only a header at column 2 whose nearest row above, past blanks, is the
///   `/status` it answers, and whose rows (blank, or at column 2 or deeper)
///   end on the grid at a row that is none of them (the composer), is read
///   ([`unboxed_card`]); a provider with no limits (`Limits: data not
///   available yet`) paints none.
///
/// Each `<label> limit:` row is a window (`5h limit` is `five_hour`, `Weekly
/// limit` `seven_day`: [`codex_window_key`]): what it says is left, turned
/// into what is used. A `<name> limit:` row with no figure heads another
/// model's limit, whose windows below it carry its name. A reset that does
/// not fit beside the figure is painted alone on the row under it (`(resets
/// …)`). The context window is no limit and is not read, so a row quoted in
/// a conversation is not; of two cards, either layout, the lower (the newer)
/// is read. Credits are not read here: the card's figure is a balance, not
/// Claude Code's notice that credits carry the session. The text is evidence
/// of what was drawn, never an instruction.
#[must_use]
pub fn codex_status_windows(rows: &[String]) -> Vec<PanelWindow> {
    /// The box open on the grid: whether its first row is Codex's header, how
    /// many rows it has shown, and its windows.
    struct Card {
        codex: bool,
        rows: usize,
        read: CardWindows,
    }
    // The newest card read, by the row it ends on. A completed newer card
    // supersedes history even when its quota is absent or unreadable.
    let mut read: Option<(usize, Vec<PanelWindow>)> = None;
    let mut keep = |end: usize, windows: Vec<PanelWindow>| {
        if read.as_ref().is_none_or(|(at, _)| end > *at) {
            read = Some((end, windows));
        }
    };
    let mut open: Option<Card> = None;
    for (i, row) in rows.iter().enumerate() {
        if let Some(windows) = unboxed_card(rows, i) {
            keep(i, windows);
        }
        let t = row.trim();
        if t.starts_with('\u{256d}') {
            open = Some(Card {
                codex: false,
                rows: 0,
                read: CardWindows::default(),
            });
            continue;
        }
        if t.starts_with('\u{2570}') {
            if let Some(card) = open.take()
                && card.codex
            {
                keep(i, card.read.windows);
            }
            continue;
        }
        let Some(card) = open.as_mut() else {
            continue;
        };
        let Some(inner) = card_row(row) else {
            // Anything else between the rules is no card.
            open = None;
            continue;
        };
        card.rows += 1;
        if card.rows == 1 {
            card.codex = inner.trim().starts_with(CODEX_CARD_HEADER);
            continue;
        }
        card.read.feed(inner);
    }
    read.map(|(_, windows)| windows).unwrap_or_default()
}

/// The command a `/status` card answers, as Codex 0.163.0 echoes it in the
/// history above the card it draws.
const STATUS_COMMAND: &str = "/status";

/// The windows of the box-less card (codex 0.163.0, [`codex_status_windows`])
/// whose LAST row is `rows[end - 1]`, read when `rows[end]` is the row that
/// ends it: a row the card does not draw (neither blank nor at column 2 or
/// deeper: the composer), under rows that run back to its header at column 2
/// (`  >_ OpenAI Codex (v…)`), the nearest row above which, past blanks, is
/// the `/status` it answers. `None` for any other `end`.
fn unboxed_card(rows: &[String], end: usize) -> Option<Vec<PanelWindow>> {
    let card_row = |r: &str| r.trim().is_empty() || r.starts_with("  ");
    if card_row(rows.get(end)?) {
        return None;
    }
    let first = rows[..end]
        .iter()
        .rposition(|r| !card_row(r))
        .map_or(0, |at| at + 1);
    let header = (first..end).find(|&i| {
        rows[i]
            .strip_prefix("  ")
            .is_some_and(|r| r.starts_with(CODEX_CARD_HEADER))
    })?;
    let above = rows[..header].iter().rev().find(|r| !r.trim().is_empty())?;
    if above.trim() != STATUS_COMMAND {
        return None;
    }
    let mut read = CardWindows::default();
    for row in &rows[header + 1..end] {
        read.feed(row);
    }
    Some(read.windows)
}

// ---------------------------------------------------------------------------
// The accounts probe's read of the panel (`docs/SPEC-claude-accounts-probe-
// 2026-10-01.md`, P10, P22)
// ---------------------------------------------------------------------------

/// The command the accounts probe types to open the panel. READ in the
/// 2.1.287 bundle: `usage` is a `local-jsx` command and `immediate`, and it
/// shares its prefix with a second command, `usage-credits`. MEASURED on
/// 2.1.287, 2026-10-01 (the probe's live check, a private headless aterm):
/// with these six bytes typed the typeahead lists `/usage` first and
/// `/usage-credits` under it, and Enter opened the Settings panel on its
/// Usage tab in every run captured (five grids and the inline renderer).
/// One build, one login: a later build's typeahead order is its own.
pub const USAGE_COMMAND: &str = "/usage";

/// What one read of the rows under the Settings panel's header says of the
/// Usage tab ([`usage_panel_read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelRead {
    /// No row only the Usage tab draws is under the header: another tab, or
    /// something that is not this panel. Nothing is sampled.
    NotUsage,
    /// The tab says its figures are not settled — a load or a refresh in
    /// flight, last-known or partial figures, a failed load — or the whole
    /// tab is on the grid and one of the two windows a sample needs is not.
    Unsettled,
    /// The panel's tail is not on the grid: what stands right above it (the
    /// refresh line, an enrich error) cannot be read, so nothing can be said
    /// of the figures that ARE visible.
    Fold,
    /// The whole tab is on the grid with nothing unsettled on it: its
    /// windows, the session's and the week's among them.
    Settled(Vec<PanelWindow>),
}

/// THE SETTLED RULE: what `rows` say of the Usage tab, read from the row
/// under `header` (the Settings panel's header row,
/// `aterm_phase::settings_panel`) to the end. First match wins:
///
/// 1. no row only the Usage tab draws (`aterm_phase::usage_tab_mark`: the
///    session block's `Total cost:`, a `Current session` title, the loading
///    line) → [`PanelRead::NotUsage`];
/// 2. any row that says the figures are not settled
///    (`aterm_phase::usage_unsettled`: `Refreshing…`, `Loading usage data…`,
///    an enrich-error line, `Error: …`, `rate limited`) →
///    [`PanelRead::Unsettled`];
/// 3. the last non-blank row is the cancel hint and nothing else
///    (`aterm_phase::panel_tail`), and [`usage_panel_windows`] holds
///    `five_hour` AND `seven_day` → [`PanelRead::Settled`];
/// 4. that tail with either window missing → [`PanelRead::Unsettled`];
/// 5. otherwise → [`PanelRead::Fold`].
///
/// WHY THE TAIL. The tab opens on the home's last-known figures with
/// `Refreshing…` drawn BELOW the windows — after the credit block, the
/// "contributing" block and usage credits — and the hint row is drawn last
/// (READ in the 2.1.287 bundle, and MEASURED on it 2026-10-01: a quarter
/// second after the Enter the week read 9 % with `Refreshing…` above the
/// hint, and 10 % a quarter second later). Where the tail is under the fold
/// the refresh line may be too, and figures read there may be the ones
/// about to be replaced. So no tail, no sample.
///
/// WHAT THAT COSTS, MEASURED. On a 150x50 grid the whole tab is on the grid
/// only while its contributing block still says `Scanning local sessions…`
/// — for 1.5 s in one run and 2.4 s in another — and then that block fills
/// in and pushes the hint under the fold (the last row then ends in the
/// vendor's `↓`): the read is `Settled` for that long and `Fold` from then
/// on. At 129x47 (1.2 s) and 60x50 (1.0 s) the same; at 110x32 it is `Fold`
/// from the first read; at 150x80 it stays `Settled`. A caller that waits
/// before its first read can miss the window on an ordinary pane and
/// record nothing.
///
/// PURE, and one read's verdict only: a caller that records a sample asks
/// for two consecutive equal `Settled` reads (the spec's P10). The rows are
/// DATA: nothing in them is obeyed, and a copy of a panel a worker quoted is
/// refused where the panel is placed (`aterm_phase::settings_panel`), not
/// here — this function trusts `header` and reads down from it.
#[must_use]
pub fn usage_panel_read(rows: &[String], header: usize) -> PanelRead {
    let under = rows.get(header.saturating_add(1)..).unwrap_or_default();
    if !under.iter().any(|row| aterm_phase::usage_tab_mark(row)) {
        return PanelRead::NotUsage;
    }
    if under.iter().any(|row| aterm_phase::usage_unsettled(row)) {
        return PanelRead::Unsettled;
    }
    let tail = under
        .iter()
        .rev()
        .find(|row| !row.trim().is_empty())
        .is_some_and(|row| aterm_phase::panel_tail(row));
    if !tail {
        return PanelRead::Fold;
    }
    let windows = usage_panel_windows(under);
    let has = |name: &str| windows.iter().any(|w| w.name == name);
    if has(SESSION_WINDOW) && has(WEEK_WINDOW) {
        PanelRead::Settled(windows)
    } else {
        PanelRead::Unsettled
    }
}

/// The window keys a sample is made of ([`PANEL_TITLES`]: `Current session`
/// and `Current week (all models)`).
const SESSION_WINDOW: &str = "five_hour";
const WEEK_WINDOW: &str = "seven_day";

/// A settled panel's windows as the roster's sample
/// ([`super::accounts::Sample`], what `accounts::note_sample` records):
/// `session_*` from `five_hour`, `week_*` from `seven_day`, read at `now`
/// (`by` is `note_sample`'s to stamp). `None` when neither figure was read —
/// a figure nobody read is never `0`.
///
/// EACH RESET IS PLACED ON THE CLOCK by the supervisor's own grammar, as
/// `harness limits` places them ([`super::cli::limits_view`]):
/// `limit::parse_reset` on the text the vendor painted, then
/// `limit::reset_at_with` with the caller's clock (`now`, unix seconds), its
/// local offset and its zone table (`zone_offset_at(zone, unix)`, the
/// offset the zone has AT the instant placed for) — all three injected, so
/// this reads no clock and no zone file. A reset the grammar cannot read is
/// `None`, and so is one placed further ahead than its window can be: more
/// than [`super::accounts::SESSION_WINDOW_S`] plus
/// [`super::accounts::STAMP_SKEW_S`] for the session's, more than
/// [`super::accounts::RESET_BELIEVED_S`] for the week's. Its figure is kept:
/// the roster then counts it for one window length from the sample
/// (`Sample::session`, `Sample::week`). A reset placed in the PAST is kept
/// as placed, and voids its figure there: the window the panel showed has
/// ended.
#[must_use]
pub fn panel_sample(
    windows: &[PanelWindow],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> Option<super::accounts::Sample> {
    use super::accounts::{RESET_BELIEVED_S, SESSION_WINDOW_S, STAMP_SKEW_S, Sample};
    use crate::supervise::limit;
    let read = |name: &str, ahead_max: i64| {
        let window = windows.iter().find(|w| w.name == name)?;
        let reset = window
            .reset_text
            .as_deref()
            .and_then(limit::parse_reset)
            .map(|spec| limit::reset_at_with(&spec, now, local_offset_s, zone_offset_at))
            .filter(|at| at.saturating_sub(now) <= ahead_max);
        Some((window.used_pct, reset))
    };
    let session = read(
        SESSION_WINDOW,
        SESSION_WINDOW_S.saturating_add(STAMP_SKEW_S),
    );
    let week = read(WEEK_WINDOW, RESET_BELIEVED_S);
    if session.is_none() && week.is_none() {
        return None;
    }
    Some(Sample {
        session_pct: session.map(|(pct, _)| pct),
        session_reset: session.and_then(|(_, reset)| reset),
        week_pct: week.map(|(pct, _)| pct),
        week_reset: week.and_then(|(_, reset)| reset),
        at: now,
        by: String::new(),
    })
}

/// The fold's per-model totals as spend rows.
pub fn spend_from_transcript(usage: &TranscriptUsage) -> BTreeMap<String, SpendView> {
    usage
        .per_model()
        .iter()
        .map(|(model, s)| {
            (
                model.clone(),
                SpendView {
                    input: s.input,
                    output: s.output,
                    cache_write: s.cache_write,
                    cache_read: s.cache_read,
                    usd: None,
                },
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The HUD line
// ---------------------------------------------------------------------------

/// The family word of a model id: `claude-fable-5-1` → `fable`,
/// `claude-3-5-haiku-20241022` → `haiku`. The first `-`-separated segment
/// after a `claude-` prefix that starts with a letter; the whole id when none
/// does.
pub fn model_short(id: &str) -> String {
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    rest.split('-')
        .find(|seg| seg.chars().next().is_some_and(char::is_alphabetic))
        .unwrap_or(rest)
        .to_owned()
}

/// Line-breaking characters become spaces: the HUD is one line of a status
/// field. What breaks a line is [`crate::supervise::limit::breaks_a_line`],
/// not `char::is_control()` (Cc-only, which misses `U+2028`).
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if crate::supervise::limit::breaks_a_line(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Cut a string to at most `max` bytes on a char boundary (the module-wide
/// cut, not a copy of it).
fn truncate_to(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let cut = super::truncate_bytes(&s, max).len();
    s.truncate(cut);
    s
}

/// The one-line HUD: `fable ?%/5h · ?%/7d · acct work · resets ? · none`.
///
/// The model is the account's live model family, and `acct` its label
/// ([`AccountView::label`]). The windows' figures and reset are `?` with
/// source `none`, since nothing here knows a window (see `harness limits`).
/// Cut to [`HUD_MAX_BYTES`] on a char boundary.
pub fn hud_line(view: &UsageView) -> String {
    let Some(acct) = view.shown() else {
        return "no account".to_owned();
    };
    let model = acct
        .model
        .as_deref()
        .map(model_short)
        .map(|m| sanitize(&m))
        .unwrap_or_else(|| "model?".to_owned());
    let five = acct.windows.get("five_hour");
    let seven = acct.windows.get("seven_day");
    let pct =
        |w: Option<&WindowView>| w.map_or_else(|| "?%".to_owned(), |w| format!("{}%", w.used_pct));
    // The reset the HUD names: the five-hour window's, else the week's, as
    // a span from the view's instant (no zone is read here).
    let resets = five.or(seven).map_or_else(
        || "?".to_owned(),
        |w| {
            let left = u64::try_from(w.resets_at.saturating_sub(view.as_of)).unwrap_or(0);
            format!("in {}", super::upgrade::span(left))
        },
    );
    let source = match five.or(seven) {
        Some(w) => format!("tap {}", super::upgrade::span(w.age_s)),
        None => "none".to_owned(),
    };
    truncate_to(
        format!(
            "{model} {}/5h · {}/7d · acct {} · resets {resets} · {source}",
            pct(five),
            pct(seven),
            sanitize(&acct.label)
        ),
        HUD_MAX_BYTES,
    )
}

// ---------------------------------------------------------------------------
// `harness usage --json`, schema 1
// ---------------------------------------------------------------------------

/// `YYYY-MM-DDTHH:MM:SSZ` of an epoch instant, before the epoch too (the
/// calendar is `aterm_types::rfc3339`'s, the workspace's one copy).
pub fn rfc3339_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

fn spend_json(s: &SpendView) -> Value {
    let mut o = Map::new();
    o.insert("in".to_owned(), Value::from(s.input));
    o.insert("out".to_owned(), Value::from(s.output));
    o.insert("cache_write".to_owned(), Value::from(s.cache_write));
    o.insert("cache_read".to_owned(), Value::from(s.cache_read));
    // Schema 1 keeps the key: null where the caller priced nothing (module
    // doc); Codex's list dollars where every turn of the row was priced.
    o.insert(
        "usd".to_owned(),
        s.usd
            .filter(|u| u.is_finite())
            .map_or(Value::Null, Value::from),
    );
    Value::Object(o)
}

fn account_json(a: &AccountView) -> Value {
    let mut o = Map::new();
    o.insert("label".to_owned(), Value::from(a.label.as_str()));
    o.insert("active".to_owned(), Value::from(a.active));
    // The usage tap's windows, by the vendor's name; empty where the session
    // has no reading (module doc), as every run before the tap printed it.
    o.insert(
        "windows".to_owned(),
        Value::Object(
            a.windows
                .iter()
                .map(|(name, w)| {
                    let mut m = Map::new();
                    m.insert("used_pct".to_owned(), Value::from(u64::from(w.used_pct)));
                    m.insert("resets_at".to_owned(), Value::from(w.resets_at));
                    m.insert("source".to_owned(), Value::from("tap"));
                    m.insert("age_s".to_owned(), Value::from(w.age_s));
                    (name.clone(), Value::Object(m))
                })
                .collect(),
        ),
    );
    o.insert(
        "spend".to_owned(),
        Value::Object(
            a.spend
                .iter()
                .map(|(k, s)| (k.clone(), spend_json(s)))
                .collect(),
        ),
    );
    if let Some(model) = &a.model {
        o.insert("model".to_owned(), Value::from(model.as_str()));
    }
    Value::Object(o)
}

/// The `harness usage --json` document, schema 1, keys in sorted order.
/// `identity` and `failover` are absent; `sheet` is the fixed
/// `{enabled:false,last_sync:null,last_error:null}`.
pub fn usage_json(view: &UsageView) -> String {
    let mut root = Map::new();
    root.insert("schema".to_owned(), Value::from(1u64));
    root.insert("as_of".to_owned(), Value::from(rfc3339_utc(view.as_of)));
    root.insert(
        "accounts".to_owned(),
        Value::Array(view.accounts.iter().map(account_json).collect()),
    );
    let mut sheet = Map::new();
    sheet.insert("enabled".to_owned(), Value::from(false));
    sheet.insert("last_sync".to_owned(), Value::Null);
    sheet.insert("last_error".to_owned(), Value::Null);
    root.insert("sheet".to_owned(), Value::Object(sheet));
    // A `Value` tree always serializes (non-finite floats become null); the
    // fallback is a schema-1 document that says so rather than a panic.
    aterm_json::to_string(&Value::Object(root))
        .unwrap_or_else(|_| r#"{"schema":1,"accounts":[],"error":"serialize"}"#.to_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- fixtures -----------------------------------------------------------

    /// The `/usage` panel exactly as Claude Code 2.1.278 painted it in a
    /// 120-column session, captured over aterm's control socket 2026-09-22.
    /// The leading three-space indent is the vendor's, kept verbatim (the
    /// literal opens on its first row: a `\` continuation there ate that
    /// row's indent, which the Settings-panel reader places the header by).
    const PANEL_WIDE: &str = "   Settings  Status   Config   Usage   Stats

   Session

   Total cost:            $0.0000

   Current session
   ███▌                                               7% used
   Resets 1:20pm (America/Los_Angeles)

   Current week (all models)
   ███████████████████████████████                    62% used
   Resets Sep 23 at 12pm (America/Los_Angeles)

   Current week (Fable)
   ██████████████████████████████████████████████████ 100% used
   Resets Sep 23 at 11:59am (America/Los_Angeles)

   What's contributing to your limits usage?
   Approximate, based on local sessions on this machine

   100% of your usage came from subagent-heavy sessions
";

    /// The same panel at 56 columns, where the reset WRAPS onto the row under
    /// the title and the meter, the blank and the figure follow it. Same
    /// capture, same session, resized.
    const PANEL_NARROW: &str = "   Current session · Resets 1:20pm
   (America/Los_Angeles)
   ███▊

   7% used

   Current week (all models) · Resets Sep 23 at 12pm
   (America/Los_Angeles)
   █████████████████████████████████▍

   62% used

   Current week (Fable) · Resets Sep 23 at 11:59am
   (America/Los_Angeles)
   ██████████████████████████████████████████████████
   ████
   100% used

   What's contributing to your limits usage?
";

    fn rows(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    fn assistant_row(id: &str, model: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"uuid":"u-{id}","message":{{"id":"{id}","model":"{model}","role":"assistant","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":10,"cache_read_input_tokens":1000,"service_tier":"standard"}}}}}}"#
        )
    }

    fn fold_all(t: &mut TranscriptUsage, lines: &[&str]) -> Vec<Fold> {
        lines.iter().map(|l| t.fold_line(l)).collect()
    }

    fn view_with(acct: AccountView) -> UsageView {
        let mut v = UsageView::new(1_789_650_000);
        v.accounts.push(acct);
        v
    }

    /// An account with its model known.
    fn live_account() -> AccountView {
        let mut a = AccountView::new("work", true);
        a.model = Some("claude-fable-5-1".to_owned());
        a
    }

    #[test]
    fn numbers_are_read_as_integer_or_float_and_junk_is_absent() {
        let v: Value = aterm_json::from_str(
            r#"{"i":62,"f":62.7,"neg":-1,"big":1e30,"s":"62","n":null,"nan_like":"NaN"}"#,
        )
        .expect("parses");
        assert_eq!(num_f64(v.get("i")), Some(62.0));
        assert_eq!(num_f64(v.get("f")), Some(62.7));
        assert_eq!(num_u64(v.get("f")), Some(62));
        assert_eq!(num_u64(v.get("neg")), None);
        assert_eq!(num_u64(v.get("big")), Some(u64::MAX), "saturates");
        assert_eq!(num_f64(v.get("s")), None);
        assert_eq!(num_f64(v.get("n")), None);
        assert_eq!(num_f64(v.get("missing")), None);
    }

    // -- TranscriptUsage ----------------------------------------------------

    #[test]
    fn transcript_duplicate_message_ids_are_summed_once() {
        let mut t = TranscriptUsage::new();
        let a = assistant_row("msg_a", "claude-fable-5-1", 100, 10);
        let folds = fold_all(&mut t, &[&a, &a, &a]);
        assert_eq!(folds, [Fold::Summed, Fold::Duplicate, Fold::Duplicate]);
        let s = t.per_model()["claude-fable-5-1"];
        assert_eq!(
            (s.input, s.output, s.cache_write, s.cache_read),
            (100, 10, 10, 1000)
        );
        assert_eq!(s.messages, 1);
        assert_eq!(t.duplicates, 2);
        assert_eq!(t.conflicting_repeats, 0);
        assert_eq!(t.assistant_rows, 3);
    }

    #[test]
    fn transcript_distinct_ids_sum_per_model() {
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 100, 10),
                &assistant_row("msg_b", "claude-fable-5-1", 50, 5),
                &assistant_row("msg_c", "claude-opus-5", 7, 3),
            ],
        );
        assert_eq!(t.per_model()["claude-fable-5-1"].input, 150);
        assert_eq!(t.per_model()["claude-fable-5-1"].messages, 2);
        assert_eq!(t.per_model()["claude-opus-5"].output, 3);
        let total = t.total();
        assert_eq!((total.input, total.output, total.messages), (157, 18, 3));
    }

    #[test]
    fn transcript_rows_without_usage_are_counted_not_summed() {
        let mut t = TranscriptUsage::new();
        let folds = fold_all(
            &mut t,
            &[
                r#"{"type":"assistant","message":{"id":"msg_x","model":"m","content":[]}}"#,
                r#"{"type":"assistant","message":{"id":"msg_y","model":"m","usage":"none"}}"#,
                r#"{"type":"assistant","message":{"id":"msg_z","model":"m","usage":{"service_tier":"standard"}}}"#,
                r#"{"type":"assistant"}"#,
            ],
        );
        assert!(folds.iter().all(|f| *f == Fold::NoUsage), "{folds:?}");
        assert_eq!(t.rows_without_usage, 4);
        assert!(t.per_model().is_empty());
    }

    #[test]
    fn transcript_non_assistant_rows_are_ignored() {
        let mut t = TranscriptUsage::new();
        let folds = fold_all(
            &mut t,
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi","usage":{"input_tokens":999}}}"#,
                r#"{"type":"progress","data":{}}"#,
                r#"{"type":"summary","summary":"x"}"#,
                r#"{"message":{"usage":{"input_tokens":5}}}"#,
                "   ",
            ],
        );
        assert_eq!(
            folds,
            [
                Fold::NotAssistant,
                Fold::NotAssistant,
                Fold::NotAssistant,
                Fold::NotAssistant,
                Fold::Blank
            ]
        );
        assert!(t.per_model().is_empty());
        assert_eq!(t.rows, 5);
        assert_eq!(t.assistant_rows, 0);
    }

    #[test]
    fn the_byte_prefilter_never_drops_a_row_the_parser_would_have_summed() {
        // Negative control for the prefilter: the type value spelled with a
        // `\u` escape is legal JSON without the literal word, and is summed.
        let mut t = TranscriptUsage::new();
        let escaped = r#"{"type":"\u0061ssistant","message":{"id":"msg_esc","model":"m","usage":{"input_tokens":7}}}"#;
        assert!(!cannot_be_assistant(escaped), "a `\\u` line must be parsed");
        assert_eq!(t.fold_line(escaped), Fold::Summed);
        assert_eq!(t.total().input, 7);
        assert_eq!(t.assistant_rows, 1);

        // The rows it MAY skip: object-shaped, no `assistant`, no `\u`.
        for skipped in [
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"attachment","data":{"path":"/tmp/x"}}"#,
            r#"{"type":"user","message":{"content":"a \"quoted\" \\ tab\there"}}"#,
        ] {
            assert!(cannot_be_assistant(skipped), "{skipped}");
            assert_eq!(t.fold_line(skipped), Fold::NotAssistant, "{skipped}");
        }

        // And the rows it may not skip, each for its own reason.
        for parsed in [
            r#"{"type":"assistant","message":{}}"#, // says the word
            r#"{"type":"user","text":"\u00e9"}"#,   // carries an escape
            r#"["type","user"]"#,                   // not object-shaped
            r#"{"type":"user""#,                    // torn: no closing brace
        ] {
            assert!(!cannot_be_assistant(parsed), "{parsed}");
        }

        // The cut's cost: a torn assistant row still counts unparsed (it says
        // the word), while a torn object-shaped row that cannot be one counts
        // NotAssistant.
        let mut u = TranscriptUsage::new();
        assert_eq!(
            u.fold_line(r#"{"type":"assistant","message":{"usage":}"#),
            Fold::Unparsed,
            "the corruption that loses spend is still seen"
        );
        assert_eq!(u.rows_unparsed, 1);
        assert_eq!(
            u.fold_line(r#"{"type":"user","message":{"content":}"#),
            Fold::NotAssistant
        );
        assert_eq!(u.rows_unparsed, 1, "unchanged: it was never parsed");
    }

    #[test]
    fn transcript_sidechain_row_is_summed_and_counted() {
        let mut t = TranscriptUsage::new();
        let side = assistant_row("msg_side", "claude-haiku-4-5", 20, 2)
            .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
        assert!(side.contains(r#""isSidechain":true"#));
        assert_eq!(t.fold_line(&side), Fold::Summed);
        assert_eq!(t.sidechain_rows, 1);
        assert_eq!(t.per_model()["claude-haiku-4-5"].input, 20);
        // A repeat of the sidechain message dedupes like any other.
        assert_eq!(t.fold_line(&side), Fold::Duplicate);
        assert_eq!(t.sidechain_rows, 2, "every sidechain row is counted");
        assert_eq!(t.per_model()["claude-haiku-4-5"].messages, 1);
    }

    #[test]
    fn transcript_very_long_line_is_skipped_and_counted() {
        let mut t = TranscriptUsage::new();
        let long = format!(
            r#"{{"type":"assistant","message":{{"id":"msg_l","model":"m","usage":{{"input_tokens":1}},"pad":"{}"}}}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        assert!(long.len() > MAX_LINE_BYTES);
        assert_eq!(t.fold_line(&long), Fold::TooLong);
        assert_eq!(t.rows_skipped_long, 1);
        assert!(t.per_model().is_empty());
        // A real-sized row (about 1.1 MB) is well inside.
        let real_sized = format!(
            r#"{{"type":"assistant","message":{{"id":"msg_r","model":"m","usage":{{"input_tokens":1}},"pad":"{}"}}}}"#,
            "x".repeat(1_125_606)
        );
        assert_eq!(t.fold_line(&real_sized), Fold::Summed);
    }

    #[test]
    fn transcript_malformed_lines_are_counted_unparsed() {
        let mut t = TranscriptUsage::new();
        let folds = fold_all(
            &mut t,
            &[
                "{\"type\":\"assistant\",",
                "[1,2]",
                "\"assistant\"",
                "{\"type\":\"assistant\"} x",
            ],
        );
        assert!(folds.iter().all(|f| *f == Fold::Unparsed), "{folds:?}");
        assert_eq!(t.rows_unparsed, 4);
        assert_eq!(t.rows, 4);
    }

    /// A differing repeat raises each count to the largest seen for its id
    /// and sums only the growth; the message counts once.
    #[test]
    fn transcript_differing_repeat_raises_to_the_largest_and_counts_once() {
        let mut t = TranscriptUsage::new();
        let first = assistant_row("msg_c", "m", 100, 10);
        let grown = assistant_row("msg_c", "m", 100, 999);
        let lower = assistant_row("msg_c", "m", 50, 20);
        assert_eq!(t.fold_line(&first), Fold::Summed);
        assert_eq!(t.fold_line(&grown), Fold::Conflict);
        assert_eq!(t.fold_line(&lower), Fold::Conflict);
        let m = t.per_model()["m"];
        assert_eq!(m.output, 999, "the largest output, not the first");
        assert_eq!(m.input, 100, "a lower count never lowers what is held");
        assert_eq!(m.messages, 1);
        assert_eq!(t.conflicting_repeats, 2);
        assert_eq!(t.duplicates, 2);
    }

    /// WORKING TIME is the UNION of the main chain's turns, each the
    /// `[timestamp − durationMs, timestamp]` of the `turn_duration` row
    /// Claude Code writes at its end (2.1.284's shape): a turn written late
    /// over the turns after it (a workflow's) counts once, a sidechain's row
    /// not at all, and a row past the raw-byte cut is still parsed.
    #[test]
    fn working_time_is_the_union_of_the_main_chains_turns() {
        let turn = |at: &str, ms: u64, side: bool| {
            format!(
                r#"{{"parentUuid":"p","isSidechain":{side},"type":"system","subtype":"turn_duration","durationMs":{ms},"messageCount":28,"timestamp":"2026-09-29T15:{at}Z","uuid":"u","isMeta":false,"version":"2.1.284"}}"#
            )
        };
        let mut t = TranscriptUsage::new();
        assert_eq!(t.fold_line(&turn("00:10.500", 10_500, false)), Fold::Turn);
        assert_eq!(t.busy_ms(), 10_500);
        // 15:00:20–15:00:30, then a workflow turn written at 15:01:00 that
        // began at 15:00:05: it covers both before it, and 15:00:10.5–20.
        fold_all(
            &mut t,
            &[
                &turn("00:30.000", 10_000, false),
                &turn("01:00.000", 55_000, false),
                &turn("02:00.000", 30_000, true),
            ],
        );
        assert_eq!(t.busy_ms(), 60_000, "the union, not the 75.5 s sum");
        assert_eq!(t.assistant_rows, 0, "no turn row is an answer");
    }

    /// THE MEASURED SUBAGENT SHAPE (2026-09-25, `agent-*.jsonl`): one message
    /// streamed over three rows — `stop_reason` null with the message_start
    /// output twice, then the final row. Its output is the final 349, once;
    /// keeping the first row counted 8.
    #[test]
    fn a_streamed_subagent_message_counts_its_final_output_once() {
        let row = |stop: &str, output: u64, block: &str| {
            format!(
                r#"{{"type":"assistant","isSidechain":true,"message":{{"id":"msg_011Cej","model":"claude-opus-5","role":"assistant","stop_reason":{stop},"content":[{{"type":"{block}"}}],"usage":{{"input_tokens":3,"output_tokens":{output},"cache_creation_input_tokens":1200,"cache_read_input_tokens":20000}}}}}}"#
            )
        };
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &row("null", 8, "thinking"),
                &row("null", 8, "tool_use"),
                &row("\"tool_use\"", 349, "tool_use"),
            ],
        );
        let m = t.per_model()["claude-opus-5"];
        assert_eq!(m.output, 349);
        assert_eq!(m.messages, 1);
        assert_eq!((m.input, m.cache_write, m.cache_read), (3, 1200, 20_000));
    }

    /// A limit notice written into the transcript (`model: <synthetic>`,
    /// `isApiErrorMessage: true`, the notice as text, its own `timestamp`) is
    /// kept per window it names, the newest one, with the reset its text names.
    /// A notice naming no window, an ordinary row quoting the words, and an
    /// older row are not.
    #[test]
    fn a_limit_notice_row_is_kept_with_its_time() {
        let notice = |text: &str, ts: &str, api_error: bool| {
            format!(
                r#"{{"type":"assistant","timestamp":"{ts}","isApiErrorMessage":{api_error},"error":"rate_limit","message":{{"id":"x-{ts}","model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":0,"output_tokens":0}}}}}}"#
            )
        };
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &notice(
                    "You've hit your session limit · resets 3pm (America/Los_Angeles)",
                    "2026-09-24T21:00:00.000Z",
                    true,
                ),
                &notice(
                    "You've hit your session limit · resets 1pm (America/Los_Angeles)",
                    "2026-09-23T19:00:00Z",
                    true,
                ),
                &notice(
                    "You've hit your weekly limit · resets Oct 1 at 9am (UTC)",
                    "2026-09-24T20:30:00Z",
                    true,
                ),
                &notice(
                    "You've hit your limit · resets 4pm",
                    "2026-09-25T00:00:00Z",
                    true,
                ),
                &notice(
                    "You've hit your session limit · resets 5pm",
                    "2026-09-25T00:00:00Z",
                    false,
                ),
            ],
        );
        let rows = t.limit_notices();
        assert_eq!(
            rows.get("five_hour"),
            Some(&LimitNoticeRow {
                reset_text: Some("3pm (America/Los_Angeles)".into()),
                at: 1_790_283_600,
            }),
            "the newest, not the later-written older one; not the quoted words"
        );
        assert_eq!(
            rows.get("seven_day")
                .map(|r| (r.reset_text.as_deref(), r.at)),
            Some((Some("Oct 1 at 9am (UTC)"), 1_790_281_800)),
        );
        assert_eq!(rows.len(), 2, "a notice naming no window is not kept");
        assert_eq!(t.served_at(), None, "a notice is no served response");
        let mut summed = TranscriptUsage::new();
        summed.absorb(&t);
        assert!(
            summed.limit_notices().is_empty(),
            "a sum carries no wall: the footer reads the main file's own fold"
        );
    }

    /// The selective parser still feeds the live footer wall, including when
    /// flags and timestamps follow content or use escaped, duplicate keys.
    #[test]
    fn projected_usage_preserves_the_live_limit_wall_and_its_lift() {
        let at = |minute: u32| format!("2026-09-28T12:{minute:02}:00Z");
        let unix = |minute: u32| {
            i64::try_from(super::super::upgrade_models::parse_utc(&at(minute)).unwrap()).unwrap()
        };
        let answer = |output: u64, minute: u32, api_error: bool| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"m1","model":"claude-opus-5-5","usage":{{"input_tokens":1,"output_tokens":{output}}}}},"timestamp":"{}","isApiErrorMessage":{api_error}}}"#,
                at(minute)
            )
        };
        for content in [
            r#""You've hit your session limit · resets 3pm (UTC)""#,
            r#"[{"type":"tool_result","text":"not the notice","content":{"output":"ignored"}},null,["nested"],{"type":"text","text":"You've hit your session limit · resets 3pm (UTC)"}]"#,
        ] {
            let mut t = TranscriptUsage::new();
            assert_eq!(t.fold_line(&answer(5, 0, false)), Fold::Summed);
            // The last spelling of each key wins, even when escaped and
            // placed after the message whose text the notice reader needs.
            let notice = format!(
                r#"{{"type":"assistant","isApiErrorMessage":false,"timestamp":"2026-09-27T12:00:00Z","message":{{"id":"notice","model":"<synthetic>","content":{content}}},"isApiErrorMess\u0061ge":true,"tim\u0065stamp":"{}"}}"#,
                at(1)
            );
            assert_eq!(t.fold_line(&notice), Fold::NoUsage);
            assert_eq!(
                t.limit_notices().get("five_hour"),
                Some(&LimitNoticeRow {
                    reset_text: Some("3pm (UTC)".into()),
                    at: unix(1),
                })
            );
            let wall = |t: &TranscriptUsage| {
                super::super::session_usage::wall_of(
                    t.limit_notices(),
                    t.served_at(),
                    unix(4),
                    &|_, _| Some(0),
                )
            };
            assert!(wall(&t).is_some(), "the new notice reaches the live wall");
            assert_eq!(t.fold_line(&answer(5, 2, false)), Fold::Duplicate);
            assert_eq!(t.fold_line(&answer(9, 3, true)), Fold::Conflict);
            assert_eq!(t.served_at(), Some(unix(0)));
            assert!(
                wall(&t).is_some(),
                "a repeated response or API-error row cannot lift the wall"
            );
            assert_eq!(t.fold_line(&answer(12, 4, false)), Fold::Conflict);
            assert_eq!(t.served_at(), Some(unix(4)));
            assert!(wall(&t).is_none(), "new served output lifts the wall");
            let counts = t.per_model()["claude-opus-5-5"];
            assert_eq!((counts.output, counts.messages), (12, 1));
        }
    }

    /// The served time is the newest response the API served: a row with usage
    /// whose message is new or grew, or has no id. Not Claude Code's own rows
    /// (`isApiErrorMessage`, `<synthetic>`), not a same-figure repeat, not a
    /// row without usage or model.
    #[test]
    fn the_served_time_is_the_newest_served_response() {
        let at = |minute: u32| format!("2026-09-24T21:{minute:02}:00.000Z");
        let row = |id: Option<&str>, model: &str, out: u64, minute: u32, api_error: bool| {
            let id = id.map_or(String::new(), |id| format!("\"id\":\"{id}\","));
            format!(
                r#"{{"type":"assistant","timestamp":"{}","isApiErrorMessage":{api_error},"message":{{{id}"model":"{model}","usage":{{"input_tokens":1,"output_tokens":{out}}}}}}}"#,
                at(minute)
            )
        };
        let minute = |m: i64| 1_790_283_600 + m * 60;
        let mut t = TranscriptUsage::new();
        t.fold_line(&row(Some("n"), "<synthetic>", 0, 1, true));
        assert_eq!(t.served_at(), None, "a limit notice");
        t.fold_line(&row(Some("m1"), "claude-opus-5-5", 5, 2, false));
        assert_eq!(t.served_at(), Some(minute(2)));
        t.fold_line(&row(Some("m1"), "claude-opus-5-5", 5, 3, false));
        assert_eq!(t.served_at(), Some(minute(2)), "the same figures again");
        t.fold_line(&row(Some("m1"), "claude-opus-5-5", 9, 4, false));
        assert_eq!(t.served_at(), Some(minute(4)), "the message grew");
        t.fold_line(&row(Some("m1"), "claude-opus-5-5", 2, 30, false));
        assert_eq!(
            t.served_at(),
            Some(minute(4)),
            "a repeat BELOW the figures held grew nothing: not served"
        );
        t.fold_line(&row(None, "claude-opus-5-5", 1, 5, false));
        assert_eq!(t.served_at(), Some(minute(5)), "a row with no id");
        t.fold_line(&row(Some("s"), "<synthetic>", 3, 6, false));
        t.fold_line(&row(Some("e"), "claude-opus-5-5", 3, 7, true));
        t.fold_line(&format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"u","model":"claude-opus-5-5"}}}}"#,
            at(8)
        ));
        t.fold_line(&format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"id":"v","usage":{{"input_tokens":1}}}}}}"#,
            at(9)
        ));
        assert_eq!(
            t.served_at(),
            Some(minute(5)),
            "synthetic, an API error, no usage, no model: none served"
        );
        t.fold_line(&row(Some("m2"), "claude-opus-5-5", 1, 1, false));
        assert_eq!(t.served_at(), Some(minute(5)), "the NEWEST time is kept");
    }

    /// Which window a notice NAMES: the screen's wall classes, narrowed to a
    /// notice that says its window.
    #[test]
    fn a_notice_names_its_window_or_none() {
        for (text, want) in [
            (
                "You've hit your session limit · resets 3pm",
                Some("five_hour"),
            ),
            (
                "You\u{2019}ve hit your weekly limit · resets Oct 1 at 9am",
                Some("seven_day"),
            ),
            ("You've hit your limit · resets 4pm", None),
            ("Approaching usage limit · resets 3pm", None),
            ("Login expired · Please run /login", None),
            ("the session limit is a thing", None),
        ] {
            assert_eq!(notice_window(text), want, "{text}");
        }
    }

    #[test]
    fn transcript_rows_without_id_or_with_a_huge_id_are_summed_and_counted() {
        let mut t = TranscriptUsage::new();
        let no_id = r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":1}}}"#;
        let empty_id =
            r#"{"type":"assistant","message":{"id":"","model":"m","usage":{"input_tokens":1}}}"#;
        let huge_id = assistant_row(&"i".repeat(MAX_ID_BYTES + 1), "m", 1, 0);
        let folds = fold_all(&mut t, &[no_id, no_id, empty_id, &huge_id, &huge_id]);
        assert!(folds.iter().all(|f| *f == Fold::Summed), "{folds:?}");
        assert_eq!(t.rows_without_id, 5);
        assert_eq!(
            t.per_model()["m"].input,
            5,
            "cannot dedupe, so every row counts"
        );
        assert_eq!(t.duplicates, 0);
    }

    #[test]
    fn transcript_missing_model_files_under_unknown_and_missing_fields_read_zero() {
        let mut t = TranscriptUsage::new();
        let row = r#"{"type":"assistant","message":{"id":"msg_u","usage":{"output_tokens":4}}}"#;
        assert_eq!(t.fold_line(row), Fold::Summed);
        let s = t.per_model()[UNKNOWN_MODEL];
        assert_eq!(
            (s.input, s.output, s.cache_write, s.cache_read),
            (0, 4, 0, 0)
        );
        // A float count is floored; a negative one reads as absent (0).
        let row = r#"{"type":"assistant","message":{"id":"msg_f","model":"m","usage":{"input_tokens":12.9,"output_tokens":-5}}}"#;
        assert_eq!(t.fold_line(row), Fold::Summed);
        assert_eq!(
            (t.per_model()["m"].input, t.per_model()["m"].output),
            (12, 0)
        );
    }

    #[test]
    fn transcript_seen_set_is_bounded_and_the_bound_is_what_it_says() {
        let mut t = TranscriptUsage::new();
        let first = assistant_row("msg_first", "m", 1, 0);
        assert_eq!(t.fold_line(&first), Fold::Summed);
        // Still remembered after exactly SEEN_IDS_MAX - 1 more distinct ids.
        for i in 0..(SEEN_IDS_MAX - 1) {
            assert_eq!(
                t.fold_line(&assistant_row(&format!("msg_{i}"), "m", 1, 0)),
                Fold::Summed
            );
        }
        assert_eq!(t.fold_line(&first), Fold::Duplicate);
        // One more distinct id evicts it: a repeat now sums again — the
        // documented bound, not a silent miscount.
        assert_eq!(
            t.fold_line(&assistant_row("msg_evictor", "m", 1, 0)),
            Fold::Summed
        );
        assert_eq!(t.fold_line(&first), Fold::Summed);
        assert_eq!(t.per_model()["m"].messages as usize, SEEN_IDS_MAX + 2);
    }

    #[test]
    fn transcript_model_count_is_bounded_into_other() {
        let mut t = TranscriptUsage::new();
        for i in 0..(MAX_MODELS + 3) {
            t.fold_line(&assistant_row(
                &format!("msg_{i}"),
                &format!("model-{i:03}"),
                1,
                0,
            ));
        }
        assert_eq!(t.per_model().len(), MAX_MODELS + 1);
        assert_eq!(t.per_model()[OTHER_MODEL].messages, 3);
        // A model already present keeps its own row past the bound.
        t.fold_line(&assistant_row("msg_again", "model-000", 1, 0));
        assert_eq!(t.per_model()["model-000"].messages, 2);
    }

    #[test]
    fn spend_from_transcript_carries_tokens() {
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 1_000_000, 0),
                &assistant_row("msg_b", "claude-opus-5", 10, 0),
            ],
        );
        let spend = spend_from_transcript(&t);
        assert_eq!(spend["claude-fable-5-1"].input, 1_000_000);
        assert_eq!(spend["claude-fable-5-1"].cache_read, 1000);
        assert_eq!(spend["claude-opus-5"].input, 10);
    }

    /// THE ONE-HOUR WRITES ARE KEPT APART: 800 of the row's 1000 cache writes
    /// are one-hour ones (`cache_creation.ephemeral_1h_input_tokens`, the key
    /// MEASURED by name on Claude Code 2.1.30x transcripts), which list higher
    /// than five-minute ones. A split larger than the total is held to it, and
    /// a split with no total sums to one. Negative control: a row with no
    /// split reads every write as a five-minute one.
    #[test]
    fn one_hour_cache_writes_are_kept_apart() {
        let split = r#"{"type":"assistant","message":{"id":"msg_s","model":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":1000,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":800},"output_tokens":0}}}"#;
        let over = r#"{"type":"assistant","message":{"id":"msg_o","model":"o","usage":{"cache_creation_input_tokens":5,"cache_creation":{"ephemeral_1h_input_tokens":9}}}}"#;
        let only_split = r#"{"type":"assistant","message":{"id":"msg_p","model":"p","usage":{"cache_creation":{"ephemeral_5m_input_tokens":2,"ephemeral_1h_input_tokens":3}}}}"#;
        let bare = r#"{"type":"assistant","message":{"id":"msg_b","model":"b","usage":{"cache_creation_input_tokens":1000}}}"#;
        let mut t = TranscriptUsage::new();
        for line in [split, over, only_split, bare] {
            assert_eq!(t.fold_line(line), Fold::Summed);
        }
        let held = |t: &TranscriptUsage, m: &str| {
            let s = t.per_model()[m];
            (s.cache_write, s.cache_write_1h)
        };
        assert_eq!(held(&t, "m"), (1000, 800));
        assert_eq!(held(&t, "o"), (5, 5));
        assert_eq!(held(&t, "p"), (5, 3));
        // NEGATIVE CONTROL.
        assert_eq!(held(&t, "b"), (1000, 0));
        // A streamed repeat raises the split with the rest.
        t.fold_line(&split.replace("800}", "900}").replace(":1000,", ":1100,"));
        assert_eq!(held(&t, "m"), (1100, 900));
    }

    // -- the view -----------------------------------------------------------

    /// The account's model is the one the newest main-chain row names: a
    /// `/model` switch shows from the next answer, and a sidechain row on
    /// another model does not move it. Negative control: no naming row names
    /// none.
    #[test]
    fn the_account_model_is_the_newest_main_chain_row_of_its_transcript() {
        let mut t = TranscriptUsage::new();
        assert_eq!(t.last_model(), None);
        let side = r#"{"type":"assistant","isSidechain":true,"message":{"id":"msg_s","model":"claude-haiku-4-5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-opus-5", 10, 1),
                &assistant_row("msg_b", "claude-fable-5-1", 10, 1),
                side,
            ],
        );
        assert_eq!(t.last_model(), Some("claude-fable-5-1"));
        let mut a = AccountView::new("work", true);
        a.add_transcript(&t);
        assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
        // A row that names no model: nothing named.
        let mut none = TranscriptUsage::new();
        fold_all(
            &mut none,
            &[r#"{"type":"assistant","message":{"id":"msg_n","usage":{"input_tokens":1}}}"#],
        );
        assert_eq!(none.last_model(), None);
        let mut a = AccountView::new("work", true);
        a.add_transcript(&none);
        assert_eq!(a.model, None);
    }

    /// The session sum (`SessionUsage::total_fold`) names the main transcript's
    /// model; a subagent's sidechain rows on another model do not move it.
    /// Negative control: a sum of folds that name no model names none.
    #[test]
    fn the_session_sum_names_the_main_transcripts_model() {
        let mut main = TranscriptUsage::new();
        fold_all(
            &mut main,
            &[&assistant_row("msg_a", "claude-fable-5-1", 10, 1)],
        );
        let side = assistant_row("msg_s", "claude-haiku-4-5", 1, 1)
            .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
        let mut sub = TranscriptUsage::new();
        fold_all(&mut sub, &[&side]);
        assert_eq!(sub.last_model(), None, "a sidechain row names no model");
        let mut all = TranscriptUsage::new();
        all.absorb(&main);
        all.absorb(&sub);
        assert_eq!(all.last_model(), Some("claude-fable-5-1"));
        assert_eq!(all.per_model().len(), 2, "both files' spend is summed");
        let mut a = AccountView::new("account", true);
        a.add_transcript(&all);
        assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
        // Negative control: nothing named anywhere, nothing named in the sum.
        let mut none = TranscriptUsage::new();
        none.absorb(&sub);
        none.absorb(&TranscriptUsage::new());
        assert_eq!(none.last_model(), None);
    }

    /// Claude Code's `<synthetic>` rows (an API error, the usage-limit notice,
    /// `No response requested.`) carry usage and an id on the main chain but
    /// leave the session's model as it was. Negative control: a real model
    /// after it moves it.
    #[test]
    fn a_synthetic_row_never_names_the_sessions_model() {
        let synthetic = r#"{"type":"assistant","isSidechain":false,"message":{"id":"msg_x","model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your limit"}],"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 10, 1),
                synthetic,
            ],
        );
        assert_eq!(t.last_model(), Some("claude-fable-5-1"));
        let mut a = AccountView::new("account", true);
        a.add_transcript(&t);
        assert_eq!(
            hud_line(&view_with(a)),
            "fable ?%/5h · ?%/7d · acct account · resets ? · none"
        );
        // Alone, it names nothing.
        let mut only = TranscriptUsage::new();
        fold_all(&mut only, &[synthetic]);
        assert_eq!(only.last_model(), None);
        // Negative control: a real model after it.
        fold_all(&mut t, &[&assistant_row("msg_b", "claude-opus-5", 10, 1)]);
        assert_eq!(t.last_model(), Some("claude-opus-5"));
    }

    #[test]
    fn view_shown_is_the_first_active_account_else_the_first() {
        let mut v = UsageView::new(0);
        assert!(v.shown().is_none());
        v.accounts.push(AccountView::new("alt-1", false));
        v.accounts.push(AccountView::new("work", true));
        v.accounts.push(AccountView::new("work-2", true));
        assert_eq!(v.shown().map(|a| a.label.as_str()), Some("work"));
        v.accounts.iter_mut().for_each(|a| a.active = false);
        assert_eq!(v.shown().map(|a| a.label.as_str()), Some("alt-1"));
    }

    // -- hud_line -----------------------------------------------------------

    /// The HUD names the model family and the account, and says the
    /// windows are unknown here (`?%`, `resets ?`, source `none`) rather
    /// than inventing a number.
    #[test]
    fn hud_line_names_the_model_and_says_the_windows_are_unknown() {
        assert_eq!(hud_line(&UsageView::new(0)), "no account");
        assert_eq!(
            hud_line(&view_with(live_account())),
            "fable ?%/5h · ?%/7d · acct work · resets ? · none"
        );
        assert_eq!(
            hud_line(&view_with(AccountView::new("work", true))),
            "model? ?%/5h · ?%/7d · acct work · resets ? · none"
        );
    }

    /// A USAGE TAP'S READING fills the windows: the HUD shows both figures,
    /// the five-hour reset as a span from the view's instant and the
    /// reading's age, and the JSON's `windows` (schema 1's own key, empty
    /// before) carries each by the vendor's name. NEGATIVE CONTROL: a window
    /// the reading has not is still `?%`, and no reading leaves `windows`
    /// empty and the HUD's `none`.
    #[test]
    fn a_tap_reading_fills_the_windows_of_the_hud_and_the_json() {
        let reading = crate::harness::tap::Reading {
            at: 1_000,
            sid: "s-1".to_string(),
            pid: 7,
            session: None,
            login: None,
            home: None,
            api_ms: 5,
            five_hour: Some(crate::harness::tap::Window {
                pct: 42,
                resets_at: 1_000 + 2 * 3_600 + 30 * 60,
            }),
            seven_day: None,
            warm: None,
            cache_expires_at: None,
            version: None,
            dur_us: 0,
        };
        let mut a = live_account();
        a.add_reading(&reading, 1_012);
        let mut view = UsageView::new(1_012);
        view.accounts.push(a);
        let line = hud_line(&view);
        assert!(
            line.starts_with("fable 42%/5h · ?%/7d · acct work · resets in "),
            "{line}"
        );
        assert!(line.ends_with(" · tap 12s"), "{line}");
        let json: Value = aterm_json::from_str(&usage_json(&view)).expect("json");
        let five = &json["accounts"][0]["windows"]["five_hour"];
        assert_eq!(five["used_pct"], Value::from(42u64));
        assert_eq!(five["source"], Value::from("tap"));
        assert_eq!(five["age_s"], Value::from(12u64));
        assert!(json["accounts"][0]["windows"].get("seven_day").is_none());
        let bare: Value =
            aterm_json::from_str(&usage_json(&view_with(live_account()))).expect("json");
        assert_eq!(bare["accounts"][0]["windows"], Value::Object(Map::new()));
    }

    #[test]
    fn hud_line_is_capped_at_1024_bytes_on_a_char_boundary() {
        let mut a = AccountView::new("é".repeat(700), true);
        a.model = Some("claude-fable-5-1".to_owned());
        let line = hud_line(&view_with(a));
        assert!(line.len() <= HUD_MAX_BYTES, "{}", line.len());
        assert!(
            line.len() > HUD_MAX_BYTES - 4,
            "cut near the cap, not far below"
        );
        assert!(line.is_char_boundary(line.len()));
        // Every char is one the input carried: the label's `é`, the `·`
        // separators, or ASCII — no replacement char, no torn sequence.
        assert!(line.chars().all(|c| c == 'é' || c == '·' || c.is_ascii()));
        // Three-byte and four-byte scalars, too.
        for label in ["日".repeat(400), "🙂".repeat(300)] {
            let mut a = AccountView::new(label, true);
            a.model = Some("m".to_owned());
            let line = hud_line(&view_with(a));
            assert!(line.len() <= HUD_MAX_BYTES);
            assert!(line.len() > HUD_MAX_BYTES - 5);
            assert!(std::str::from_utf8(line.as_bytes()).is_ok());
        }
        // An under-cap line is untouched.
        assert_eq!(truncate_to("abc".to_owned(), 3), "abc");
        assert_eq!(truncate_to("aé".to_owned(), 2), "a");
    }

    #[test]
    fn hud_line_flattens_control_characters_in_labels_and_model_ids() {
        let mut a = AccountView::new("wo\nrk\x1b[31m", true);
        a.model = Some("claude-fa\tble-5-1".to_owned());
        let line = hud_line(&view_with(a));
        assert!(!line.contains('\n') && !line.contains('\x1b') && !line.contains('\t'));
        assert!(line.starts_with("fa ble ?%/5h"));

        // Negative control for the shared predicate: `U+2028` is Zl, so
        // `char::is_control()` is false for it.
        assert!(!'\u{2028}'.is_control());
        let split = AccountView::new("wo\u{2028}rk", true);
        let line = hud_line(&view_with(split));
        assert!(!line.contains('\u{2028}'), "{line:?}");
        assert!(line.contains("wo rk"), "{line:?}");
    }

    #[test]
    fn model_short_names_the_family() {
        assert_eq!(model_short("claude-fable-5-1"), "fable");
        assert_eq!(model_short("claude-opus-4-1"), "opus");
        assert_eq!(model_short("claude-sonnet-4-5-20250929"), "sonnet");
        assert_eq!(model_short("claude-3-5-haiku-20241022"), "haiku");
        assert_eq!(model_short("gpt-5"), "gpt");
        assert_eq!(
            model_short("claude-3-5"),
            "3-5",
            "no family word: the id itself"
        );
        assert_eq!(model_short(""), "");
    }

    // -- usage_json ---------------------------------------------------------

    #[test]
    fn usage_json_is_schema_1_and_round_trips_through_the_parser() {
        let mut work = live_account();
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 1_000_000, 0),
                &assistant_row("msg_b", "claude-opus-5", 10, 0),
            ],
        );
        work.add_transcript(&t);
        let alt = AccountView::new("alt-1", false);
        let mut v = UsageView::new(1_789_650_000);
        v.accounts.push(work);
        v.accounts.push(alt);

        let doc = usage_json(&v);
        let j: Value = aterm_json::from_str(&doc).expect("the document is JSON");
        assert_eq!(j["schema"].as_u64(), Some(1));
        assert_eq!(j["as_of"].as_str(), Some("2026-09-17T13:00:00Z"));
        let accounts = j["accounts"].as_array().expect("accounts");
        assert_eq!(accounts.len(), 2);
        let w = &accounts[0];
        assert_eq!(w["label"].as_str(), Some("work"));
        assert_eq!(w["active"].as_bool(), Some(true));
        // The newest summed row of the transcript names the model.
        assert_eq!(w["model"].as_str(), Some("claude-opus-5"));
        // No window is known here: the key stays, empty.
        assert!(w["windows"].as_object().is_some_and(Map::is_empty));
        let fable = &w["spend"]["claude-fable-5-1"];
        assert_eq!(fable["in"].as_u64(), Some(1_000_000));
        assert_eq!(fable["out"].as_u64(), Some(0));
        assert_eq!(fable["cache_write"].as_u64(), Some(10));
        assert_eq!(fable["cache_read"].as_u64(), Some(1000));
        // No price is known: the key stays, null.
        assert!(fable["usd"].is_null());
        let a = &accounts[1];
        assert_eq!(a["active"].as_bool(), Some(false));
        assert!(a.get("model").is_none());
        assert!(a["windows"].as_object().is_some_and(Map::is_empty));
        assert!(a["spend"].as_object().is_some_and(Map::is_empty));
        assert_eq!(j["sheet"]["enabled"].as_bool(), Some(false));
        assert!(j["sheet"]["last_sync"].is_null());
        assert!(j["sheet"]["last_error"].is_null());
        assert!(!doc.contains('\n'), "one line");
    }

    #[test]
    fn usage_json_of_an_empty_view_is_still_schema_1() {
        let doc = usage_json(&UsageView::new(0));
        assert_eq!(
            doc,
            r#"{"accounts":[],"as_of":"1970-01-01T00:00:00Z","schema":1,"sheet":{"enabled":false,"last_error":null,"last_sync":null}}"#
        );
    }

    #[test]
    fn rfc3339_utc_matches_known_instants() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_789_669_500), "2026-09-17T18:25:00Z");
        assert_eq!(rfc3339_utc(4_102_444_799), "2099-12-31T23:59:59Z");
        assert_eq!(rfc3339_utc(-62_162_035_200), "0000-03-01T00:00:00Z");
        // The extremes do not panic.
        let _ = rfc3339_utc(i64::MAX);
        let _ = rfc3339_utc(i64::MIN);
    }

    // -- the `/usage` panel -------------------------------------------------

    #[test]
    fn panel_reader_takes_both_measured_layouts_and_the_fable_window() {
        for (what, text) in [("wide", PANEL_WIDE), ("narrow", PANEL_NARROW)] {
            let got = usage_panel_windows(&rows(text));
            let want = vec![
                PanelWindow {
                    name: String::from("five_hour"),
                    used_pct: 7,
                    reset_text: Some(String::from("1:20pm (America/Los_Angeles)")),
                },
                PanelWindow {
                    name: String::from("seven_day"),
                    used_pct: 62,
                    reset_text: Some(String::from("Sep 23 at 12pm (America/Los_Angeles)")),
                },
                PanelWindow {
                    // A window only the panel carries.
                    name: String::from("seven_day_overage_included"),
                    used_pct: 100,
                    reset_text: Some(String::from("Sep 23 at 11:59am (America/Los_Angeles)")),
                },
            ];
            assert_eq!(got, want, "{what}");
        }
    }

    #[test]
    fn panel_reader_takes_the_copied_text_form() {
        // The vendor's own one-line rendering of the same rows.
        let got = usage_panel_windows(&rows(
            "Current session: 7% used · resets 1:20pm (America/Los_Angeles)\n\
             Current week (all models): 62% used · resets Sep 23 at 12pm\n",
        ));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "five_hour");
        assert_eq!(got[0].used_pct, 7);
        assert_eq!(got[1].used_pct, 62);
        assert_eq!(got[1].reset_text.as_deref(), Some("Sep 23 at 12pm"));
    }

    #[test]
    fn panel_reader_refuses_what_is_not_a_panel() {
        // Negative control 1: one row is never a panel, however exactly it is
        // spelled.
        assert!(
            usage_panel_windows(&rows(
                "⏺ my five-hour window says Current session: 99% used\n"
            ))
            .is_empty()
        );
        // Negative control 2: titles with no figure painted anywhere.
        assert!(
            usage_panel_windows(&rows(
                "   Current session\n   Resets 1:20pm\n\n   Current week (all models)\n   Resets Sep 23\n"
            ))
            .is_empty()
        );
        // Negative control 3: the context indicator is not a quota, and a real
        // idle footer carries no figure.
        assert!(
            usage_panel_windows(&rows(
                "                                              7% until auto-compact\n\
                 ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n"
            ))
            .is_empty()
        );
        // Negative control 4: the panel's trailing lines are percentages of
        // usage, not `% used`, and are not windows.
        assert!(
            usage_panel_windows(&rows(
                "   100% of your usage came from subagent-heavy sessions\n\
                    81% of your usage was at >150k context\n"
            ))
            .is_empty()
        );
        // Negative control 5: empty input.
        assert!(usage_panel_windows(&[]).is_empty());
    }

    // -- Codex's `/status` card --------------------------------------------

    /// A card as codex-rs lays it out: its header row first, `body` between,
    /// its rules at `width` columns.
    fn codex_card(body: &[&str]) -> Vec<String> {
        let width = 90;
        let row = |text: &str| {
            let pad = width - text.chars().count();
            format!("\u{2502}{text}{}\u{2502}", " ".repeat(pad))
        };
        let rule = "\u{2500}".repeat(width);
        std::iter::once(format!("\u{256d}{rule}\u{256e}"))
            .chain(std::iter::once(row("  >_ OpenAI Codex (v0.156.1)")))
            .chain(body.iter().map(|text| row(text)))
            .chain(std::iter::once(format!("\u{2570}{rule}\u{256f}")))
            .collect()
    }

    /// MEASURED (codex 0.156.1, 2026-09-24): the `/status` card's one limit
    /// row, `Weekly limit: […] 99% left (resets 13:24 on 1 Oct)`, is a window,
    /// `seven_day` (the key Claude Code's weekly window has), 1% used, its
    /// reset as painted; the `Context window: 99% left` row above it is no
    /// limit and is not read.
    #[test]
    fn the_codex_status_card_paints_its_weekly_window() {
        use aterm_phase::codex::fixtures::STATUS;
        let got = codex_status_windows(&fx::screen(STATUS));
        assert_eq!(
            got,
            vec![PanelWindow {
                name: String::from("seven_day"),
                used_pct: 1,
                reset_text: Some(String::from("13:24 on 1 Oct")),
            }]
        );
        // NEGATIVE CONTROL: Claude Code's reader finds no panel on it.
        assert!(usage_panel_windows(&fx::screen(STATUS)).is_empty());
    }

    /// MEASURED (codex 0.163.0, 2026-10-16, the lab, a composed ChatGPT
    /// login at 5h 30% and weekly 10% used): the card is drawn with no box
    /// under the `/status` it answers, and read the same: `five_hour` 30%
    /// used, reset `12:15 PM`, and `seven_day` 10%, reset `11:15 AM on 21 Oct`
    /// (a 12-hour clock); the `Context window` and `Credits` rows are no
    /// limit. With the 5h window spent (the same login later) the newer card
    /// reads it 100% used, under an older card scrolled past its header. The
    /// lab provider's card says `Limits: data not available yet`: no window.
    #[test]
    fn the_codex_0_163_status_card_is_read_without_its_box() {
        let window = |name: &str, used_pct, reset: &str| PanelWindow {
            name: name.to_owned(),
            used_pct,
            reset_text: Some(reset.to_owned()),
        };
        let credits = fx::screen(include_str!(
            "../../../aterm-phase/src/fixtures/codex-0.163.0-status-card-chatgpt-credits.txt"
        ));
        assert_eq!(
            codex_status_windows(&credits),
            vec![
                window("five_hour", 30, "12:15 PM"),
                window("seven_day", 10, "11:15 AM on 21 Oct"),
            ]
        );
        let spent = fx::screen(include_str!(
            "../../../aterm-phase/src/fixtures/codex-0.163.0-status-card-chatgpt-credits-in-use.txt"
        ));
        assert_eq!(
            codex_status_windows(&spent),
            vec![
                window("five_hour", 100, "12:15 PM"),
                window("seven_day", 10, "11:15 AM on 21 Oct"),
            ]
        );
        let lab = fx::screen(include_str!(
            "../../../aterm-phase/src/fixtures/codex-0.163.0-status-card-lab-provider.txt"
        ));
        assert!(codex_status_windows(&lab).is_empty(), "no limits painted");
        // Each reset is the card's clock.
        for (w, at) in [("12:15 PM", (12, 15)), ("11:15 AM on 21 Oct", (11, 15))] {
            assert!(
                matches!(
                    crate::supervise::limit::parse_card_reset(w),
                    Some(crate::supervise::limit::ResetSpec::At { hour, minute, .. })
                        if (hour, minute) == at
                ),
                "{w}"
            );
        }
    }

    /// The box-less card's reader ([`unboxed_card`]) refuses what is not the
    /// card. NEGATIVE CONTROLS, each the measured card with one thing
    /// changed: no `/status` above its header (a card quoted in a message);
    /// no row under it that ends it (the card runs off the grid); its header
    /// not at column 2; the session card Codex opens on (no limit row).
    #[test]
    fn the_codex_0_163_card_reader_refuses_what_is_not_the_card() {
        let credits = fx::screen(include_str!(
            "../../../aterm-phase/src/fixtures/codex-0.163.0-status-card-chatgpt-credits.txt"
        ));
        assert_eq!(codex_status_windows(&credits).len(), 2, "the control");
        let status = credits
            .iter()
            .position(|r| r == "/status")
            .expect("the command");
        let mut quoted = credits.clone();
        quoted[status] = "• Here is my card:".to_string();
        assert!(codex_status_windows(&quoted).is_empty(), "no /status above");
        let composer = credits
            .iter()
            .rposition(|r| r.starts_with('›'))
            .expect("the composer");
        assert!(
            codex_status_windows(&credits[..composer]).is_empty(),
            "nothing ends it"
        );
        let shifted: Vec<String> = credits
            .iter()
            .map(|r| {
                if r.contains(">_ OpenAI Codex") {
                    format!("  {r}")
                } else {
                    r.clone()
                }
            })
            .collect();
        assert!(
            codex_status_windows(&shifted).is_empty(),
            "the header off column 2"
        );
        let idle = fx::screen(include_str!(
            "../../../aterm-phase/src/fixtures/codex-0.163.0-idle.txt"
        ));
        assert!(codex_status_windows(&idle).is_empty(), "the session card");
    }

    /// Every `<label> limit:` row of the card is a window, keyed by the
    /// window its label names (`5h` is `five_hour`, `Weekly` `seven_day`,
    /// another length its label folded); a reset that does not fit beside the
    /// figure is read off the row under it, alone; a row with no reset has
    /// none; a full window is 100% used. Of two cards, the lower (the newer)
    /// is read.
    #[test]
    fn every_limit_row_of_the_codex_card_is_a_window() {
        let card = codex_card(&[
            "  Context window:   99% left (14.8K used / 258K)",
            "  5h limit:         [████████████████░░░░] 80% left",
            "                    (resets 18:05)",
            "  Weekly limit:     [░░░░░░░░░░░░░░░░░░░░] 0% left (resets 13:24 on 1 Oct)",
            "  Monthly limit:    [████████████████████] 100% left",
        ]);
        let window = |name: &str, used_pct, reset: Option<&str>| PanelWindow {
            name: name.to_owned(),
            used_pct,
            reset_text: reset.map(str::to_owned),
        };
        assert_eq!(
            codex_status_windows(&card),
            vec![
                window("five_hour", 20, Some("18:05")),
                window("seven_day", 100, Some("13:24 on 1 Oct")),
                window("monthly", 0, None),
            ]
        );
        let older = codex_card(&["  Weekly limit:     [██████████] 50% left (resets 09:00)"]);
        let both: Vec<String> = older.into_iter().chain(card.clone()).collect();
        assert_eq!(codex_status_windows(&both).len(), 3, "the newer card");
    }

    /// The newest completed Codex card owns the reading even when it is the
    /// session card, which paints no quota. An older `/status` card left in
    /// the transcript must not supply this card's windows.
    #[test]
    fn a_newer_codex_card_without_limits_retires_older_windows() {
        let older = codex_card(&["  Weekly limit:     [██████████] 50% left"]);
        assert_eq!(codex_status_windows(&older)[0].used_pct, 50);
        let newer = codex_card(&["  Model:            GPT-5.6-Luna"]);
        assert!(codex_status_windows(&newer).is_empty());
        let both: Vec<String> = older.into_iter().chain(newer).collect();
        assert!(
            codex_status_windows(&both).is_empty(),
            "the newest card paints no quota; the older card is history"
        );
    }

    /// An unreadable figure on the newest completed card is unknown, not
    /// permission to reuse an older card's readable figure. Positive control:
    /// a newer valid figure replaces the old one as before.
    #[test]
    fn an_unreadable_newer_codex_limit_retires_older_windows() {
        let older = codex_card(&["  Weekly limit:     [██████████] 50% left"]);
        let valid = codex_card(&["  Weekly limit:     [███████████████] 75% left"]);
        let valid_pair: Vec<String> = older.iter().cloned().chain(valid).collect();
        assert_eq!(
            codex_status_windows(&valid_pair)
                .into_iter()
                .map(|window| window.used_pct)
                .collect::<Vec<_>>(),
            vec![25],
            "the newer valid card owns the reading"
        );
        let invalid = codex_card(&["  Weekly limit:     [████] 101% left"]);
        assert!(codex_status_windows(&invalid).is_empty());
        let both: Vec<String> = older.into_iter().chain(invalid).collect();
        assert!(
            codex_status_windows(&both).is_empty(),
            "the unreadable current quota must not become the older quota"
        );
    }

    /// ANOTHER MODEL'S LIMIT on the card (`limit_name`, the accounts design's
    /// F11; UNMEASURED, laid out as codex-rs's status card lays it: a
    /// `<name> limit:` row with no figure over its windows, or `<name> <length>
    /// limit` for a limit with one window) labels its windows by length as
    /// Codex's own do, and is keyed with its name in front: its `5h` is never
    /// read as Codex's `five_hour`. NEGATIVE CONTROL: Codex's own windows
    /// above it keep the bare keys.
    #[test]
    fn another_models_limit_on_the_codex_card_keeps_its_own_keys() {
        let card = codex_card(&[
            "  5h limit:                    [████████████████░░░░] 80% left (resets 18:05)",
            "  Weekly limit:                [████████████████████] 99% left",
            "  GPT-5.3-Codex-Spark limit:",
            "  5h limit:                    [░░░░░░░░░░░░░░░░░░░░] 0% left (resets 19:00)",
            "  Weekly limit:                [██████████████████░░] 90% left",
            "  GPT-5.6-Mini weekly limit:   [███████████████░░░░░] 75% left",
        ]);
        let keys: Vec<(String, u32)> = codex_status_windows(&card)
            .into_iter()
            .map(|w| (w.name, w.used_pct))
            .collect();
        let want = |rows: &[(&str, u32)]| -> Vec<(String, u32)> {
            rows.iter().map(|(k, u)| ((*k).to_owned(), *u)).collect()
        };
        assert_eq!(
            keys,
            want(&[
                ("five_hour", 20),
                ("seven_day", 1),
                ("gpt_5_3_codex_spark_five_hour", 100),
                ("gpt_5_3_codex_spark_seven_day", 10),
                ("gpt_5_6_mini_seven_day", 25),
            ])
        );
    }

    /// The card's reader refuses what is not the card. NEGATIVE CONTROLS: the
    /// session card Codex opens on (its header, no limit row); a limit row
    /// quoted outside a card; a box that is not Codex's; a card whose bottom
    /// rule is not on the grid; a `(resets …)` row that follows no window;
    /// and a figure past 100% left.
    #[test]
    fn the_codex_card_reader_refuses_what_is_not_the_card() {
        use aterm_phase::codex::fixtures::IDLE;
        assert!(
            codex_status_windows(&fx::screen(IDLE)).is_empty(),
            "the session card"
        );
        assert!(
            codex_status_windows(&rows(
                "• Weekly limit:     [████] 99% left (resets 13:24 on 1 Oct)\n"
            ))
            .is_empty(),
            "a quoted row"
        );
        let mut other = codex_card(&["  Weekly limit:     [████] 99% left (resets 13:24)"]);
        other[1] = other[1].replace(">_ OpenAI Codex", ">_ Another Tool ");
        assert!(codex_status_windows(&other).is_empty(), "not Codex's box");
        let mut cut = codex_card(&["  Weekly limit:     [████] 99% left (resets 13:24)"]);
        cut.pop();
        assert!(codex_status_windows(&cut).is_empty(), "no bottom rule");
        let stray = codex_card(&[
            "  Model:            GPT-5.6-Luna",
            "                    (resets 13:24)",
            "  Weekly limit:     [████] 99% left",
        ]);
        assert_eq!(
            codex_status_windows(&stray)
                .into_iter()
                .map(|w| w.reset_text)
                .collect::<Vec<_>>(),
            vec![None],
            "a reset under no window is no window's"
        );
        let past = codex_card(&["  Weekly limit:     [████] 101% left"]);
        assert!(codex_status_windows(&past).is_empty(), "past 100% left");
        assert!(codex_status_windows(&[]).is_empty());
    }

    // -- the accounts probe's read: the Settings panel, the settled rule ------

    use aterm_phase::prompt::fixtures as fx;

    /// MEASURED: the one whole-screen capture of the panel on Claude Code
    /// 2.1.278, 2026-09-22 (the vendor corpus, whose manifest records its
    /// digest): 46 rows at 56 columns, the `▔` rule
    /// on row 3, the header on row 4, three windows in the narrow layout,
    /// and the vendor's `↓` ending row 46 — the tail is under the fold.
    const SCREEN_2_1_278: &str =
        include_str!("../../tests/testdata/vendor/2.1.278/screen/usage.txt");

    /// The cancel hint as the panel draws it (three columns in), from the
    /// anchor table.
    fn hint() -> String {
        format!("   {}", aterm_phase::anchor("prompt.cancel"))
    }

    /// The retry hint beside the cancel hint, as the 2.1.287 bundle composes
    /// the row beside an enrich error or a failed load (READ, not measured:
    /// the `settings:retry` binding's key and word, the byline's ` · `).
    fn retry_hints() -> String {
        format!("   r to retry · {}", aterm_phase::anchor("prompt.cancel"))
    }

    /// MEASURED on 2.1.287 (`aterm_phase::prompt::fixtures::USAGE_SETTLED`,
    /// 150x50, half a second after the Enter): the Usage tab settled and
    /// whole on the grid — the session at 1 %, the week at 10 %, the Fable
    /// window at 0 %, the cancel hint last.
    fn settled() -> Vec<String> {
        fx::screen(fx::USAGE_SETTLED)
    }

    /// The index of the last non-blank row.
    fn last_row(rows: &[String]) -> usize {
        rows.iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("a drawn row")
    }

    /// COMPOSED, NOT MEASURED, and only for the states no capture holds: the
    /// MEASURED settled screen ([`settled`]) with `between` drawn right
    /// above its hint row (each with the blank row the tab's `gap` puts
    /// under it) and the hint row replaced by `tail`. The place is the READ
    /// one: the 2.1.287 bundle draws the enrich-error line and `Refreshing…`
    /// after the usage-credits block and before the hint row, and the
    /// measured refreshing screen agrees.
    fn composed(between: &[&str], tail: &str) -> Vec<String> {
        let mut out = settled();
        let at = last_row(&out);
        assert!(aterm_phase::panel_tail(&out[at]), "{:?}", out[at]);
        out[at] = tail.to_string();
        for row in between.iter().rev() {
            out.insert(at, String::new());
            out.insert(at, (*row).to_string());
        }
        out
    }

    /// The panel's header on `rows`, as the phase reader places it.
    fn header_of(rows: &[String]) -> usize {
        aterm_phase::settings_panel(rows).expect("the panel").header
    }

    fn read(rows: &[String]) -> PanelRead {
        usage_panel_read(rows, header_of(rows))
    }

    /// The windows of a read, as `(key, percent)`.
    fn figures(read: &PanelRead) -> Vec<(String, u32)> {
        let PanelRead::Settled(windows) = read else {
            panic!("not settled: {read:?}")
        };
        windows
            .iter()
            .map(|w| (w.name.clone(), w.used_pct))
            .collect()
    }

    fn three(session: u32, week: u32, fable: u32) -> Vec<(String, u32)> {
        vec![
            ("five_hour".to_string(), session),
            ("seven_day".to_string(), week),
            ("seven_day_overage_included".to_string(), fable),
        ]
    }

    /// THE MEASURED SCREENS WHOSE TAIL IS UNDER THE FOLD — 2.1.278's one
    /// capture (46 rows), 2.1.287 at 150x50 once the contributing block
    /// filled in, and 2.1.287 at 110x32 from its first read — are each the
    /// Settings panel under its `▔` rule, drawn by the fullscreen renderer,
    /// the header the screen's fourth row; and the settled rule reads `Fold`
    /// although the windows are on the grid (which `harness limits` prints
    /// as they are). NEGATIVE CONTROLS for the panel's placement: the two
    /// older fixtures (a header with no rule over it; no header at all), the
    /// 2.1.278 capture with a composer's frame under it, and with its header
    /// one column out or in, are no panel.
    #[test]
    fn the_measured_screens_with_the_tail_under_the_fold_are_no_sample() {
        let screen = rows(SCREEN_2_1_278);
        assert_eq!(screen.len(), 46);
        let fullscreen = Some(aterm_phase::SettingsPanel {
            rule: 2,
            header: 3,
            renderer: aterm_phase::PanelRenderer::Fullscreen,
        });
        for (what, screen, seen) in [
            ("2.1.278", screen.clone(), three(18, 64, 100)),
            (
                "2.1.287 150x50",
                fx::screen(fx::USAGE_FOLD),
                three(1, 10, 0),
            ),
            (
                "2.1.287 110x32",
                fx::screen(fx::USAGE_SHORT),
                three(2, 10, 0),
            ),
        ] {
            assert_eq!(aterm_phase::settings_panel(&screen), fullscreen, "{what}");
            assert!(
                screen[last_row(&screen)].ends_with('↓'),
                "{what}: {:?}",
                screen[last_row(&screen)]
            );
            assert_eq!(usage_panel_read(&screen, 3), PanelRead::Fold, "{what}");
            let on_the_grid: Vec<(String, u32)> = usage_panel_windows(&screen[4..])
                .into_iter()
                .map(|w| (w.name, w.used_pct))
                .collect();
            assert_eq!(on_the_grid, seen, "{what}");
        }

        assert_eq!(aterm_phase::settings_panel(&rows(PANEL_WIDE)), None);
        assert_eq!(aterm_phase::settings_panel(&rows(PANEL_NARROW)), None);
        let rule = "─".repeat(56);
        let mut framed = screen.clone();
        framed.extend([rule.clone(), "❯ ".to_string(), rule]);
        assert_eq!(aterm_phase::settings_panel(&framed), None);
        let mut shifted = screen.clone();
        shifted[3] = shifted[3][1..].to_string();
        assert_eq!(aterm_phase::settings_panel(&shifted), None);
        let mut shifted = screen.clone();
        shifted[3] = format!(" {}", shifted[3]);
        assert_eq!(aterm_phase::settings_panel(&shifted), None);
    }

    /// THE HEADER'S GUARD, compiled by the server's own row matcher
    /// (`aterm_observe::row_matcher`, what `key if=` and `await gone` test
    /// every row of the screen with), matches the measured screen's header
    /// row and no other row of it — on 2.1.278's capture and on every
    /// 2.1.287 one, both renderers. NEGATIVE CONTROL: a copy of the header
    /// quoted further in — a tool's output, the worker's words — is not
    /// matched.
    #[test]
    fn the_header_guard_matches_the_measured_header_and_no_copy_of_it() {
        for (what, screen) in [
            ("2.1.278", rows(SCREEN_2_1_278)),
            ("refreshing", fx::screen(fx::USAGE_REFRESHING)),
            ("settled", fx::screen(fx::USAGE_SETTLED)),
            ("fold", fx::screen(fx::USAGE_FOLD)),
            ("whole", fx::screen(fx::USAGE_WHOLE)),
            ("narrow", fx::screen(fx::USAGE_NARROW)),
            ("short", fx::screen(fx::USAGE_SHORT)),
            ("inline", fx::screen(fx::USAGE_INLINE)),
        ] {
            let panel = aterm_phase::settings_panel(&screen).expect("the panel");
            let guard = panel.header_guard(&screen);
            let matcher = aterm_observe::row_matcher(&guard).expect("the guard compiles");
            let hits: Vec<usize> = (0..screen.len())
                .filter(|&i| matcher.matches(&screen[i]))
                .collect();
            assert_eq!(hits, [panel.header], "{what}: {guard}");
        }
        let screen = fx::screen(fx::USAGE_SETTLED);
        let guard = aterm_phase::settings_panel(&screen)
            .expect("the panel")
            .header_guard(&screen);
        let matcher = aterm_observe::row_matcher(&guard).expect("the guard compiles");
        let help = format!("aterm ctl key 'if={guard}' esc");
        for copy in [
            "  ⎿     Settings  Status   Config   Usage   Stats",
            "    Settings  Status   Config   Usage   Stats",
            "⏺ Settings  Status   Config   Usage   Stats",
            "Settings  Status   Config   Usage   Stats",
            help.as_str(),
        ] {
            assert!(!matcher.matches(copy), "{copy}");
        }
    }

    /// MEASURED on 2.1.287: the whole tab on the grid, nothing unsettled,
    /// the hint row last — `Settled`, with the figures the screen shows: at
    /// 150x50 while the contributing block is still scanning, at 150x80 with
    /// that block filled in, and at 60x50 in the narrow layout. NEGATIVE
    /// CONTROL, also MEASURED: the screen the same run showed a quarter
    /// second EARLIER has `Refreshing…` above the hint and is `Unsettled` —
    /// and the week it shows (9 %) is not the week the settled one shows
    /// (10 %): the figures of an unsettled panel are the ones being
    /// replaced, which is the whole reason for the rule.
    #[test]
    fn a_whole_panel_with_nothing_unsettled_on_it_is_settled() {
        assert_eq!(figures(&read(&settled())), three(1, 10, 0));
        assert_eq!(
            figures(&read(&fx::screen(fx::USAGE_WHOLE))),
            three(2, 10, 0)
        );
        assert_eq!(
            figures(&read(&fx::screen(fx::USAGE_NARROW))),
            three(2, 10, 0)
        );
        let PanelRead::Settled(windows) = read(&settled()) else {
            panic!("not settled")
        };
        assert_eq!(
            (
                windows[0].reset_text.as_deref(),
                windows[1].reset_text.as_deref()
            ),
            (
                Some("2am (America/Los_Angeles)"),
                Some("Oct 8 at 9am (America/Los_Angeles)")
            )
        );

        let refreshing = fx::screen(fx::USAGE_REFRESHING);
        assert_eq!(read(&refreshing), PanelRead::Unsettled);
        // What it would have recorded: the last-known week, and no Fable
        // window at all.
        let stale: Vec<(String, u32)> = usage_panel_windows(&refreshing[4..])
            .into_iter()
            .map(|w| (w.name, w.used_pct))
            .collect();
        assert_eq!(
            stale,
            [("five_hour".to_string(), 1), ("seven_day".to_string(), 9)]
        );
        // COMPOSED: the refresh line put on the settled screen is unsettled
        // too, so the verdict is the row's and not that capture's.
        assert_eq!(
            read(&composed(&["   Refreshing…"], &hint())),
            PanelRead::Unsettled
        );
        assert_eq!(read(&composed(&[], &hint())), read(&settled()));
    }

    /// COMPOSED (see [`composed`]; no capture holds one): each enrich-error
    /// line the 2.1.287 bundle draws under the usage-credits block, with the
    /// retry hint it brings beside the cancel hint — `Unsettled`, whichever
    /// it is: the figures are last-known, partial or unrefreshed by the
    /// vendor's own word. NEGATIVE CONTROL: the retry hint alone (no error
    /// line read) is no tail, so that panel is `Fold`, never `Settled`.
    #[test]
    fn an_enrich_error_under_the_windows_is_unsettled() {
        let hints = retry_hints();
        for line in [
            "   Showing last-known usage as of 2m ago (rate limited — try again in a moment)",
            "   Showing last-known usage (could not refresh)",
            "   Could not refresh usage data",
            "   Partial usage data (rate limited — try again in a moment)",
            "   Per-model breakdown unavailable (rate limited — try again in a moment)",
        ] {
            assert_eq!(
                read(&composed(&[line], &hints)),
                PanelRead::Unsettled,
                "{line}"
            );
            // Under the cancel hint alone it is unsettled all the same: the
            // line decides, not the hint beside it.
            assert_eq!(
                read(&composed(&[line], &hint())),
                PanelRead::Unsettled,
                "{line}"
            );
        }
        assert_eq!(read(&composed(&[], &hints)), PanelRead::Fold);
    }

    /// COMPOSED (no capture holds either state): the MEASURED rule, header
    /// and `Session` block, and — in the plan-limits component's place —
    /// what the 2.1.287 bundle draws with no figures yet (`Loading usage
    /// data…` over the cancel hint) and after a failed load (`Error: <why>`
    /// over a retry hint and the cancel hint). Both are the Usage tab (the
    /// session block's `Total cost:` is drawn above the component, always —
    /// READ, and MEASURED on every capture) and both are `Unsettled`.
    /// NEGATIVE CONTROL: the same rows with the loading line or the error
    /// taken out are the Usage tab with no window on it — `Unsettled` under
    /// the hint row by the missing windows, never `Settled`.
    #[test]
    fn a_loading_or_failed_panel_is_unsettled() {
        let measured = settled();
        let first_window = measured
            .iter()
            .position(|r| r.trim() == "Current session")
            .expect("the first window");
        let head = |rest: &[&str]| {
            let mut out = measured[..first_window].to_vec();
            for row in rest {
                out.push((*row).to_string());
                out.push(String::new());
            }
            out
        };
        let cancel = hint();
        let retry = retry_hints();
        let loading = head(&["   Loading usage data…", &cancel]);
        assert!(loading.iter().any(|r| r.trim().starts_with("Total cost:")));
        assert_eq!(read(&loading), PanelRead::Unsettled);
        for why in [
            "Failed to load usage data",
            "Failed to load usage data: upstream timed out",
            "Usage endpoint is rate limited. Please try again in a moment.",
        ] {
            let failed = head(&[&format!("   Error: {why}"), &retry]);
            assert_eq!(read(&failed), PanelRead::Unsettled, "{why}");
        }
        // The loading line alone marks the tab (a thin client's session
        // block draws no `Total cost:`).
        let mut bare = measured[..=header_of(&measured)].to_vec();
        bare.extend([
            String::new(),
            "   Loading usage data…".to_string(),
            String::new(),
            cancel.clone(),
        ]);
        assert_eq!(read(&bare), PanelRead::Unsettled);

        assert_eq!(read(&head(&[&cancel])), PanelRead::Unsettled);
        assert!(usage_panel_windows(&head(&[&cancel])).is_empty());
    }

    /// The fold, beyond the measured screens: the settled screen with the
    /// vendor's `↓` ending its hint row (COMPOSED), and the settled screen
    /// cut anywhere above its hint row, are `Fold` — more is below, and what
    /// is below is where the refresh line is drawn. NEGATIVE CONTROL: the
    /// same rows whole are `Settled`.
    #[test]
    fn a_panel_whose_tail_is_under_the_fold_is_no_sample() {
        let whole = settled();
        let tail = last_row(&whole);
        let mut arrow = whole.clone();
        arrow[tail] = format!("{:<149}↓", hint());
        assert_eq!(read(&arrow), PanelRead::Fold);
        // …from the first row that marks the tab (`Total cost:`) down.
        let mark = whole
            .iter()
            .position(|r| r.trim().starts_with("Total cost:"))
            .expect("the session block");
        for cut in mark + 1..tail {
            assert_eq!(read(&whole[..cut]), PanelRead::Fold, "cut at {cut}");
        }
        assert!(matches!(read(&whole), PanelRead::Settled(_)));
        assert!(matches!(read(&whole[..=tail]), PanelRead::Settled(_)));
        // A row after the hint row (the vendor draws none): the hint is not
        // the tail, so nothing is sampled.
        let mut after = whole[..=tail].to_vec();
        after.push("   What's contributing to your limits usage?".to_string());
        assert_eq!(read(&after), PanelRead::Fold);
    }

    /// COMPOSED from the measured settled screen, a window's rows taken out
    /// (no capture holds a tab with one missing): only `Current session`
    /// (the reader's own fence then yields no window at all), the session
    /// with the Fable window but no `Current week (all models)`, and the
    /// week with no session — each `Unsettled` under its hint row: the whole
    /// tab is on the grid and the figures a sample needs are not. NEGATIVE
    /// CONTROL: with both windows and no third, it is `Settled`.
    #[test]
    fn a_whole_panel_missing_the_session_or_the_week_is_unsettled() {
        let whole = settled();
        let title = |name: &str| {
            whole
                .iter()
                .position(|r| r.trim() == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        let (session, week, fable, rest) = (
            title("Current session"),
            title("Current week (all models)"),
            title("Current week (Fable)"),
            title("What's contributing to your limits usage?"),
        );
        let without = |from: usize, to: usize| {
            let mut out = whole[..from].to_vec();
            out.extend_from_slice(&whole[to..]);
            out
        };
        let only_session = without(week, rest);
        assert!(only_session.iter().any(|r| r.contains("1% used")));
        assert_eq!(read(&only_session), PanelRead::Unsettled);
        let no_week = without(week, fable);
        assert_eq!(usage_panel_windows(&no_week).len(), 2);
        assert_eq!(read(&no_week), PanelRead::Unsettled);
        let no_session = without(session, week);
        assert_eq!(usage_panel_windows(&no_session).len(), 2);
        assert_eq!(read(&no_session), PanelRead::Unsettled);

        assert_eq!(
            figures(&read(&without(fable, rest))),
            [("five_hour".to_string(), 1), ("seven_day".to_string(), 10)]
        );
    }

    /// COMPOSED: the MEASURED rule and header with none of the Usage tab's
    /// rows under them — another tab's (invented rows in the Status and
    /// Config tabs' shape: this build's other tabs were never captured, and
    /// never will be committed: the Status tab names the login) — is
    /// `NotUsage`, hint row or none. NEGATIVE CONTROL: one `Total cost:` row
    /// under the same header and it is the Usage tab (`Unsettled`: no
    /// window).
    #[test]
    fn a_settings_panel_on_another_tab_is_not_the_usage_tab() {
        let measured = settled();
        let other = |body: &[&str]| {
            let mut out = measured[..=header_of(&measured)].to_vec();
            out.push(String::new());
            out.extend(body.iter().map(|r| (*r).to_string()));
            out.extend([String::new(), hint()]);
            out
        };
        for body in [
            &[
                "   Version: 0.0.0",
                "   Session name: (none)",
                "   cwd: /work",
            ][..],
            &[
                "   Auto-compact                          true",
                "   Theme                                 dark",
            ][..],
            &[][..],
        ] {
            let panel = other(body);
            assert!(aterm_phase::settings_panel(&panel).is_some());
            assert_eq!(read(&panel), PanelRead::NotUsage, "{body:?}");
        }
        assert_eq!(
            read(&other(&["   Total cost:            $0.0000"])),
            PanelRead::Unsettled
        );
        // A header index past the rows reads nothing, and says so.
        assert_eq!(usage_panel_read(&settled(), 10_000), PanelRead::NotUsage);
    }

    /// MEASURED on 2.1.287 under the INLINE renderer: the panel under its
    /// `─` rule, two columns in — the phase reader says `Inline`, which is
    /// what the probe refuses to type under; the settled rule itself reads
    /// the rows the same. And once that panel grew and the main screen
    /// scrolled its rule and header away, there is no panel to read at all,
    /// though its hint row is on the grid. NEGATIVE CONTROL: the fullscreen
    /// capture of the same tab is `Fullscreen`.
    #[test]
    fn the_inline_renderers_panel_is_told_from_the_fullscreen_one() {
        let inline = fx::screen(fx::USAGE_INLINE);
        let panel = aterm_phase::settings_panel(&inline).expect("the inline panel");
        assert_eq!(panel.renderer, aterm_phase::PanelRenderer::Inline);
        assert_eq!(
            figures(&usage_panel_read(&inline, panel.header)),
            three(2, 10, 0)
        );
        assert_eq!(
            aterm_phase::settings_panel(&fx::screen(fx::USAGE_INLINE_SCROLLED)),
            None
        );
        assert_eq!(
            aterm_phase::settings_panel(&settled()).map(|p| p.renderer),
            Some(aterm_phase::PanelRenderer::Fullscreen)
        );
    }

    // -- the sample ------------------------------------------------------------

    /// 2026-10-02 04:20:00 UTC, 9:20 PM on Oct 1 in the panel's zone: the
    /// minute the measured 150x50 screens were read (their done row says
    /// `done 9:20 PM`).
    const READ_AT: i64 = 1_790_914_800;

    /// The zone table of the tests: the measured panels' zone, on daylight
    /// time that week; nothing else is known.
    fn zones(zone: &str, _at: i64) -> Option<i64> {
        (zone == "America/Los_Angeles").then_some(-7 * 3600)
    }

    fn window(name: &str, pct: u32, reset: Option<&str>) -> PanelWindow {
        PanelWindow {
            name: name.to_string(),
            used_pct: pct,
            reset_text: reset.map(str::to_string),
        }
    }

    /// THE SAMPLE OF THE MEASURED SETTLED PANEL: the session's figure and
    /// the week's, each reset placed on the clock in the zone the panel
    /// names — a bare clock time (`2am`, four hours forty ahead) and a dated
    /// one (`Oct 8 at 9am`) — read at the injected `now`. The machine's own
    /// offset (here nine hours east) is not used where the zone is known.
    /// NEGATIVE CONTROL: with a zone table that knows nothing, the same text
    /// is placed at the machine's offset — another instant.
    #[test]
    fn a_settled_panels_sample_places_each_reset_in_the_panels_zone() {
        let PanelRead::Settled(windows) = read(&settled()) else {
            panic!("not settled")
        };
        let east = 9 * 3600;
        let sample = panel_sample(&windows, READ_AT, east, &zones).expect("a sample");
        assert_eq!(
            sample,
            crate::harness::accounts::Sample {
                session_pct: Some(1),
                session_reset: Some(1_790_931_600),
                week_pct: Some(10),
                week_reset: Some(1_791_475_200),
                at: READ_AT,
                by: String::new(),
            }
        );
        assert_eq!(
            (
                rfc3339_utc(1_790_931_600).as_str(),
                rfc3339_utc(1_791_475_200).as_str()
            ),
            ("2026-10-02T09:00:00Z", "2026-10-08T16:00:00Z")
        );
        // The roster reads it as it reads any sample: both windows running.
        assert_eq!(sample.session(READ_AT), Some((1, 1_790_931_600)));
        assert_eq!(sample.week(READ_AT), Some((10, 1_791_475_200)));

        let nowhere = panel_sample(&windows, READ_AT, east, &|_, _| None).expect("a sample");
        assert_eq!((nowhere.session_pct, nowhere.week_pct), (Some(1), Some(10)));
        assert_ne!(nowhere.session_reset, sample.session_reset);
        assert_ne!(nowhere.week_reset, sample.week_reset);
    }

    /// A RESET FURTHER AHEAD THAN ITS WINDOW CAN BE IS NO RESET: the figure
    /// is kept and the reset is `None` (the roster then counts the figure
    /// for one window length). The session's bound is its five hours and the
    /// stamp's skew; the week's is eight days. So is a reset the grammar
    /// cannot read, and a window that carried none. NEGATIVE CONTROLS: the
    /// same windows with a reset inside the bound keep it — the week's
    /// seven days ahead, the session's four hours ahead.
    #[test]
    fn a_reset_beyond_its_windows_bound_is_dropped_and_its_figure_kept() {
        use crate::harness::accounts::{RESET_BELIEVED_S, SESSION_WINDOW_S, STAMP_SKEW_S};
        let sample = |session: Option<&str>, week: Option<&str>| {
            panel_sample(
                &[
                    window("five_hour", 40, session),
                    window("seven_day", 50, week),
                ],
                READ_AT,
                0,
                &zones,
            )
            .expect("a sample")
        };
        let ahead = |at: Option<i64>| at.map(|at| at - READ_AT);
        // The two bounds, by value: five hours and five minutes; eight days.
        assert_eq!(
            (SESSION_WINDOW_S + STAMP_SKEW_S, RESET_BELIEVED_S),
            (5 * 3600 + 300, 8 * 24 * 3600)
        );
        // Read at 9:20 PM in the panel's zone, Oct 1.
        let zone = "(America/Los_Angeles)";
        // The session: 1:20am is four hours ahead; a dated reset tomorrow
        // evening is past five hours and five minutes.
        let kept = sample(Some(&format!("1:20am {zone}")), None);
        assert_eq!(ahead(kept.session_reset), Some(4 * 3600));
        let dropped = sample(Some(&format!("Oct 2 at 9:20pm {zone}")), None);
        assert_eq!(
            (dropped.session_pct, dropped.session_reset),
            (Some(40), None)
        );
        // The week: Oct 8 is seven days ahead; Oct 10 is nine.
        let kept = sample(None, Some(&format!("Oct 8 at 9:20pm {zone}")));
        assert_eq!(ahead(kept.week_reset), Some(7 * 24 * 3600));
        let dropped = sample(None, Some(&format!("Oct 10 at 9:20pm {zone}")));
        assert_eq!((dropped.week_pct, dropped.week_reset), (Some(50), None));
        // A dated reset tomorrow is the WEEK's to keep and the session's to
        // drop: the bound is each window's own.
        let both = sample(
            Some(&format!("Oct 2 at 9:20pm {zone}")),
            Some(&format!("Oct 2 at 9:20pm {zone}")),
        );
        assert_eq!(
            (both.session_reset, ahead(both.week_reset)),
            (None, Some(24 * 3600))
        );
        // Words the grammar does not read, and no reset at all.
        let unread = sample(Some("when the moon is full"), None);
        assert_eq!(
            (unread.session_pct, unread.session_reset, unread.week_reset),
            (Some(40), None, None)
        );
    }

    /// A FIGURE NOBODY READ IS `None`, NEVER `0`: a lone week window gives
    /// no session figure, a lone session window no week's, and windows that
    /// are neither (the Fable bucket, the spend limit) — or none at all —
    /// give no sample. NEGATIVE CONTROL: the two windows together give both.
    #[test]
    fn a_window_that_was_not_read_gives_no_figure_and_none_gives_no_sample() {
        let week = window("seven_day", 10, Some("Oct 8 at 9am (America/Los_Angeles)"));
        let session = window("five_hour", 1, Some("2am (America/Los_Angeles)"));
        let fable = window(
            "seven_day_overage_included",
            100,
            Some("Oct 8 at 9am (America/Los_Angeles)"),
        );
        let of = |windows: &[PanelWindow]| panel_sample(windows, READ_AT, 0, &zones);

        let lone = of(std::slice::from_ref(&week)).expect("the week alone");
        assert_eq!(
            (lone.session_pct, lone.session_reset, lone.week_pct),
            (None, None, Some(10))
        );
        assert_eq!(lone.session(READ_AT), None);
        let lone = of(std::slice::from_ref(&session)).expect("the session alone");
        assert_eq!(
            (lone.session_pct, lone.week_pct, lone.week_reset),
            (Some(1), None, None)
        );
        assert_eq!(of(&[]), None);
        assert_eq!(of(std::slice::from_ref(&fable)), None);
        assert_eq!(of(&[fable.clone(), window("spend_limit", 12, None)]), None);
        let both = of(&[fable, week, session]).expect("both");
        assert_eq!((both.session_pct, both.week_pct), (Some(1), Some(10)));
        // The command the probe types is the six bytes, no more.
        assert_eq!(USAGE_COMMAND, "/usage");
    }

    #[test]
    fn panel_reader_never_guesses_a_measured_key_for_an_unmeasured_title() {
        let got = usage_panel_windows(&rows(
            "   Current session\n   ███ 5% used\n   Resets 1pm\n\n\
             \x20  Current week (Haiku 9 Super)\n   ███ 11% used\n   Resets Sep 23\n",
        ));
        assert_eq!(got.len(), 2);
        // The slug is derived and unverified; it can never collide with a
        // known key.
        assert_eq!(got[1].name, "seven_day_haiku_9_super");
        for (_, key) in PANEL_TITLES {
            assert_ne!(got[1].name, key);
        }
        // And a display name that is only punctuation mints no key at all —
        // the row is dropped while its neighbours are still read.
        let junk = usage_panel_windows(&rows(
            "   Current session\n   ███ 5% used\n\n\
             \x20  Current week (···)\n   ███ 11% used\n\n\
             \x20  Current week (all models)\n   ███ 22% used\n",
        ));
        assert_eq!(
            junk.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["five_hour", "seven_day"],
            "a keyless title is dropped, not guessed"
        );
    }
}
