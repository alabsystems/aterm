// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage view: Claude Code's transcript rows folded into one per-account view, the one-line HUD, and the `harness usage --json` document (schema 1).

use std::collections::{BTreeMap, VecDeque};

use aterm_json::{Map, Value};

/// The longest transcript line that is parsed.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// How many `message.id`s [`TranscriptUsage`] remembers for dedupe, in arrival order.
pub const SEEN_IDS_MAX: usize = 4096;

/// A `message.id` longer than this is treated as absent (summed, not deduped), so a hostile id cannot fill the dedupe set.
pub const MAX_ID_BYTES: usize = 128;

/// Distinct model ids [`TranscriptUsage`] keeps separately; further ones fold into the `"(other)"` row.
pub const MAX_MODELS: usize = 64;

/// The longest HUD line, in bytes, that `harness usage` prints.
pub const HUD_MAX_BYTES: usize = 1024;

/// The model key spend is filed under when an assistant row carries none.
pub const UNKNOWN_MODEL: &str = "unknown";

/// The model key spend is filed under past [`MAX_MODELS`] distinct ids.
pub const OTHER_MODEL: &str = "(other)";

fn num_f64(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|f| f.is_finite())
}

fn num_u64(v: Option<&Value>) -> Option<u64> {
    let f = num_f64(v)?;
    if f < 0.0 {
        return None;
    }
    Some(f as u64)
}

/// One request's token counts — the vendor's `usage` object on every assistant transcript row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    /// `input_tokens`.
    pub input: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `cache_creation_input_tokens`: every cache write, both lifetimes.
    pub cache_write: u64,
    /// Of [`Self::cache_write`], the tokens written to the one-hour cache (`cache_creation.ephemeral_1h_input_tokens`), which list at a higher price than the five-minute ones.
    pub cache_write_1h: u64,
    /// `cache_read_input_tokens`.
    pub cache_read: u64,
}

// The usage keys of a transcript row the counts are read from, as the anchors name them (the
// step fold's token counts are these: `lab claude-measure` refuses GONE when a build drops one).
const INPUT_TOKENS: &str = aterm_phase::anchor!("transcript.input_tokens");
const OUTPUT_TOKENS: &str = aterm_phase::anchor!("transcript.output_tokens");
const CACHE_CREATION: &str = aterm_phase::anchor!("transcript.cache_creation");
const EPHEMERAL_5M: &str = aterm_phase::anchor!("transcript.ephemeral_5m_input_tokens");
const EPHEMERAL_1H: &str = aterm_phase::anchor!("transcript.ephemeral_1h_input_tokens");
const CACHE_CREATION_INPUT: &str = aterm_phase::anchor!("transcript.cache_creation_input_tokens");
const CACHE_READ_INPUT: &str = aterm_phase::anchor!("transcript.cache_read_input_tokens");

impl TokenUsage {
    /// Read the counts; a missing one reads 0.
    pub(crate) fn from_object(obj: &Map) -> Option<Self> {
        let input = num_u64(obj.get(INPUT_TOKENS));
        let output = num_u64(obj.get(OUTPUT_TOKENS));
        let split = obj.get(CACHE_CREATION).and_then(Value::as_object);
        let five_min = split.and_then(|s| num_u64(s.get(EPHEMERAL_5M)));
        let one_hour = split.and_then(|s| num_u64(s.get(EPHEMERAL_1H)));
        let cache_write = num_u64(obj.get(CACHE_CREATION_INPUT)).or_else(|| {
            (five_min.is_some() || one_hour.is_some())
                .then(|| five_min.unwrap_or(0).saturating_add(one_hour.unwrap_or(0)))
        });
        let cache_read = num_u64(obj.get(CACHE_READ_INPUT));
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

    /// The request's prompt: input, cache writes and cache reads together — what a price by prompt length is decided on.
    #[must_use]
    pub const fn prompt(&self) -> u64 {
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
    pub const fn less(&self, other: &Self) -> Self {
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
    pub const fn plus(&self, other: &Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            output: self.output.saturating_add(other.output),
            cache_write: self.cache_write.saturating_add(other.cache_write),
            cache_write_1h: self.cache_write_1h.saturating_add(other.cache_write_1h),
            cache_read: self.cache_read.saturating_add(other.cache_read),
        }
    }
}

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

    const fn add_tokens(&mut self, t: TokenUsage) {
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
    pub const fn tokens(&self) -> TokenUsage {
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
    /// A repeat of a counted id whose usage differed (a streamed message's later row).
    Conflict,
    /// A row that is not an assistant row (user, progress, summary, …).
    NotAssistant,
    /// A main-chain `turn_duration` row: the turn it closes was folded into the session's working time ([`TranscriptUsage::busy_ms`]).
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

/// The bounded set of ids already counted, each with the largest usage seen for it so far, count by count.
#[derive(Debug, Clone, Default)]
pub(crate) struct SeenIds {
    order: VecDeque<String>,
    largest: BTreeMap<String, TokenUsage>,
}

pub(crate) enum Seen {
    New,
    Same,
    /// A differing repeat: how much each count grew past the largest held.
    Differs(TokenUsage),
}

impl SeenIds {
    pub(crate) fn remember(&mut self, id: &str, usage: TokenUsage) -> Seen {
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
    pub(crate) fn shed(&mut self, keep: usize) {
        while self.order.len() > keep {
            if let Some(old) = self.order.pop_front() {
                self.largest.remove(&old);
            }
        }
    }
}

/// Spend per model, folded one transcript line at a time.
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
    /// Assistant rows with usage but no usable `message.id`: summed, not deduped.
    pub rows_without_id: u64,
    /// Repeats folded away (same id, same usage) plus conflicts.
    pub duplicates: u64,
    /// Repeats whose usage differed from the largest held for the id; each raised the counts and summed the difference.
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
    /// The summed rows' tokens over time ([`Self::series`]).
    series: TokenSeries,
    /// Summed rows with no `timestamp`: in the totals, not in the series.
    unstamped: u64,
    /// Moves whenever the series or the working time does
    /// ([`Self::generation`]).
    generation: u64,
    /// What the rows say of the conversation's `/goal` ([`Self::goal`]).
    goal: super::goal::ClaudeGoal,
}

/// The time Claude was working in one transcript: the union of its main
/// chain's turns, each `[timestamp − durationMs, timestamp]` of the
/// `{"type":"system","subtype":"turn_duration"}` row Claude Code writes at
/// every turn's end.
#[derive(Debug, Clone, Default)]
struct BusySpans {
    /// Disjoint `[start, end)` spans in unix milliseconds, in time order.
    spans: VecDeque<(i64, i64)>,
    /// The sum of every span's length, those that left the deque included.
    total_ms: u64,
}

/// The most disjoint spans `BusySpans` keeps.
pub const BUSY_SPANS_MAX: usize = 4096;

impl BusySpans {
    fn note(&mut self, start: i64, end: i64) {
        if end <= start {
            return;
        }
        let len = |(a, b): (i64, i64)| u64::try_from(b - a).unwrap_or(0);
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

/// The most buckets a [`TokenSeries`] holds.
pub const SERIES_BUCKETS: usize = 120;

/// The finest bucket of a [`TokenSeries`], in seconds: a minute.
pub const SERIES_BASE_S: i64 = 60;

/// The session's tokens over time, for the footer's traffic graph: each summed row's tokens in the bucket of its own `timestamp`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenSeries {
    /// The bucket width in seconds; 0 while empty.
    width_s: i64,
    /// The first bucket's index at `width_s`.
    first: i64,
    /// `(in, out)` per bucket from `first`, at most [`SERIES_BUCKETS`].
    buckets: VecDeque<(u64, u64)>,
}

impl TokenSeries {
    /// The bucket width in seconds (0 while empty).
    #[must_use]
    pub const fn width_s(&self) -> i64 {
        self.width_s
    }

    /// Unix seconds of the first bucket's start (0 while empty).
    #[must_use]
    pub const fn start_s(&self) -> i64 {
        self.first.saturating_mul(self.width_s)
    }

    /// `(in, out)` per bucket, oldest first.
    #[must_use]
    pub const fn buckets(&self) -> &VecDeque<(u64, u64)> {
        &self.buckets
    }

    /// Whether no row was noted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Every bucket summed: every row noted.
    #[must_use]
    pub fn sum(&self) -> (u64, u64) {
        self.buckets.iter().fold((0, 0), |(i, o), &(bi, bo)| {
            (i.saturating_add(bi), o.saturating_add(bo))
        })
    }

    fn end(&self) -> i64 {
        self.first
            .saturating_add(i64::try_from(self.buckets.len()).unwrap_or(i64::MAX))
    }

    fn coarsen(&mut self) {
        let Some(width) = self.width_s.checked_mul(2) else {
            return;
        };
        let first = self.first.div_euclid(2);
        let mut merged: VecDeque<(u64, u64)> = VecDeque::with_capacity(self.buckets.len() / 2 + 1);
        for (k, &(i, o)) in self.buckets.iter().enumerate() {
            let at = self.first + i64::try_from(k).unwrap_or(0);
            let slot = usize::try_from(at.div_euclid(2) - first).unwrap_or(0);
            if merged.len() <= slot {
                merged.resize(slot + 1, (0, 0));
            }
            let held = &mut merged[slot];
            *held = (held.0.saturating_add(i), held.1.saturating_add(o));
        }
        self.width_s = width;
        self.first = first;
        self.buckets = merged;
    }

    fn fits(lo: i64, hi: i64) -> bool {
        hi.saturating_sub(lo) <= i64::try_from(SERIES_BUCKETS).unwrap_or(i64::MAX)
    }

    fn cover(&mut self, lo: i64, hi: i64) {
        while self.first > lo {
            self.buckets.push_front((0, 0));
            self.first -= 1;
        }
        while self.end() < hi {
            self.buckets.push_back((0, 0));
        }
    }

    /// Note a row stamped `at_s` (unix seconds) with `input` and `output` tokens.
    pub fn note(&mut self, at_s: i64, input: u64, output: u64) {
        if input == 0 && output == 0 {
            return;
        }
        if self.width_s == 0 {
            self.width_s = SERIES_BASE_S;
            self.first = at_s.div_euclid(SERIES_BASE_S);
            self.buckets.clear();
        }
        let mut at = at_s.div_euclid(self.width_s);
        while !Self::fits(self.first.min(at), self.end().max(at.saturating_add(1))) {
            let width = self.width_s;
            self.coarsen();
            if self.width_s == width {
                at = at.clamp(self.first, self.end().saturating_sub(1));
                break;
            }
            at = at_s.div_euclid(self.width_s);
        }
        self.cover(at, at.saturating_add(1));
        let slot = usize::try_from(at - self.first).unwrap_or(0);
        if let Some(held) = self.buckets.get_mut(slot) {
            *held = (held.0.saturating_add(input), held.1.saturating_add(output));
        }
    }

    /// Merge `other` in: the finer of the two is coarsened to the wider width (and both further while their span passes [`SERIES_BUCKETS`]), then bucket adds to bucket.
    pub fn absorb(&mut self, other: &Self) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            self.clone_from(other);
            return;
        }
        let mut theirs = other.clone();
        while self.width_s < theirs.width_s {
            self.coarsen();
        }
        while theirs.width_s < self.width_s {
            theirs.coarsen();
        }
        while !Self::fits(self.first.min(theirs.first), self.end().max(theirs.end())) {
            let width = self.width_s;
            self.coarsen();
            theirs.coarsen();
            if self.width_s == width {
                break;
            }
        }
        let (lo, hi) = (self.first.min(theirs.first), self.end().max(theirs.end()));
        self.cover(lo, hi);
        for (k, &(i, o)) in theirs.buckets.iter().enumerate() {
            let at = theirs.first + i64::try_from(k).unwrap_or(0);
            let slot = usize::try_from(at - self.first).unwrap_or(0);
            if let Some(held) = self.buckets.get_mut(slot) {
                *held = (held.0.saturating_add(i), held.1.saturating_add(o));
            }
        }
    }

    /// The buckets spread over `n` columns of `from_s..to_s` (unix seconds), for a graph `n` cells wide.
    #[must_use]
    pub fn columns(&self, from_s: i64, to_s: i64, n: usize) -> Vec<(u64, u64)> {
        spread_columns(
            self.first,
            self.width_s,
            self.buckets.iter().copied(),
            (from_s, to_s),
            n,
        )
    }
}

/// [`TokenSeries::columns`] over any run of buckets.
#[must_use]
pub(crate) fn spread_columns(
    first: i64,
    width_s: i64,
    buckets: impl IntoIterator<Item = (u64, u64)>,
    (from_s, to_s): (i64, i64),
    n: usize,
) -> Vec<(u64, u64)> {
    let mut out = vec![(0u64, 0u64); n];
    if n == 0 || to_s <= from_s || width_s == 0 {
        return if n == 0 { Vec::new() } else { out };
    }
    let span = i128::from(to_s) - i128::from(from_s);
    let cols = i128::try_from(n).unwrap_or(i128::MAX);
    let edge = |c: i128| i128::from(from_s) * cols + c * span;
    for (k, (input, output)) in buckets.into_iter().enumerate() {
        if input == 0 && output == 0 {
            continue;
        }
        let at = i128::from(first) + i128::try_from(k).unwrap_or(0);
        let lo = (at * i128::from(width_s)).max(i128::from(from_s)) * cols;
        let hi = ((at + 1) * i128::from(width_s)).min(i128::from(to_s)) * cols;
        if hi <= lo {
            continue;
        }
        let whole = hi - lo;
        let first_col = ((lo - edge(0)) / span).clamp(0, cols - 1);
        let mut c = first_col;
        let mut done = 0i128;
        let mut given = (0u64, 0u64);
        while c < cols {
            let (a, b) = (edge(c).max(lo), edge(c + 1).min(hi));
            if b > a {
                done += b - a;
            }
            let share = |v: u64| u64::try_from(i128::from(v) * done / whole).unwrap_or(u64::MAX);
            let upto = (share(input), share(output));
            let slot = &mut out[usize::try_from(c).unwrap_or(0)];
            slot.0 = slot.0.saturating_add(upto.0 - given.0);
            slot.1 = slot.1.saturating_add(upto.1 - given.1);
            given = upto;
            if edge(c + 1) >= hi {
                break;
            }
            c += 1;
        }
    }
    out
}

/// A limit notice as Claude Code wrote it into the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitNoticeRow {
    /// The reset the notice names, as its text prints it (`aterm_phase::notice_reset`, the screen reader's own grammar).
    pub reset_text: Option<String>,
    /// Unix seconds of the row's `timestamp`.
    pub at: i64,
}

/// Unix seconds of a transcript row's own `timestamp` (the workspace's RFC 3339 reader, which refuses an offset it would misread).
pub(crate) fn row_time(obj: &Map) -> Option<i64> {
    obj.get("timestamp")
        .and_then(Value::as_str)
        .and_then(super::unix_time::parse_utc)
        .and_then(|t| i64::try_from(t).ok())
}

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

/// The window a limit notice's text names: `session limit` / `5-hour limit` is `five_hour`, a weekly one `seven_day`.
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

    /// An empty fold that replaces `older` (its file started over).
    pub(crate) fn following(older: &Self) -> Self {
        Self {
            generation: older.generation.wrapping_add(1),
            ..Self::default()
        }
    }

    /// Spend by model id, in key order.
    pub const fn per_model(&self) -> &BTreeMap<String, ModelSpend> {
        &self.per_model
    }

    /// The model id the newest summed main-chain row named: the session's model as its transcript last recorded it.
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

    /// Fold one line.
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
        if super::goal::ClaudeGoal::may_be(line)
            && let Ok(row) = super::transcript::goal(line)
            && let Some(row) = row.as_object()
            && self.goal.row(row)
        {
            return Fold::NotAssistant;
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
        let served = !api_error && model != UNKNOWN_MODEL && super::upgrade::is_model_id(model);
        let Some(id) = id else {
            self.rows_without_id = self.rows_without_id.saturating_add(1);
            self.add(model, usage, sidechain);
            self.note_series(obj, usage);
            if served {
                self.note_served(obj);
            }
            return Fold::Summed;
        };
        match self.seen.remember(id, usage) {
            Seen::New => {
                self.add(model, usage, sidechain);
                self.note_series(obj, usage);
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
                self.note_series(obj, growth);
                if served && growth != TokenUsage::default() {
                    self.note_served(obj);
                }
                Fold::Conflict
            }
        }
    }

    /// The conversation's active `/goal`, as the rows folded so far leave it
    /// ([`super::goal::ClaudeGoal`]).
    #[must_use]
    pub fn goal(&self) -> Option<&str> {
        self.goal.active()
    }

    /// Unix seconds of the newest response the API served in this file.
    pub const fn served_at(&self) -> Option<i64> {
        self.served_at
    }

    /// Milliseconds Claude was working in this transcript: the union of its main chain's finished turns.
    pub const fn busy_ms(&self) -> u64 {
        self.busy.total_ms
    }

    /// The main chain's finished turns as disjoint `[start, end)` spans in unix milliseconds, in time order ([`BusySpans`]: the newest [`BUSY_SPANS_MAX`]).
    #[must_use]
    pub const fn busy_spans(&self) -> &VecDeque<(i64, i64)> {
        &self.busy.spans
    }

    /// The summed rows' tokens over time ([`TokenSeries`]).
    #[must_use]
    pub const fn series(&self) -> &TokenSeries {
        &self.series
    }

    /// Summed rows the series could not place (no `timestamp`): counted in the totals, not graphed.
    #[must_use]
    pub const fn unstamped(&self) -> u64 {
        self.unstamped
    }

    /// A count that moves whenever the series, the unplaced rows or the working time move.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    fn note_series(&mut self, obj: &Map, usage: TokenUsage) {
        if usage.is_zero() {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        match row_time(obj) {
            Some(at) => self.series.note(at, usage.prompt(), usage.output),
            None => self.unstamped = self.unstamped.saturating_add(1),
        }
    }

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
        let before = self.busy.total_ms;
        self.busy.note(end.saturating_sub(ms), end);
        if self.busy.total_ms != before {
            self.generation = self.generation.wrapping_add(1);
        }
    }

    fn note_served(&mut self, obj: &Map) {
        if let Some(at) = row_time(obj) {
            self.served_at = Some(self.served_at.map_or(at, |held| held.max(at)));
        }
    }

    /// The newest limit notice written into the transcript, per window it names (`five_hour`, `seven_day`).
    pub const fn limit_notices(&self) -> &BTreeMap<String, LimitNoticeRow> {
        &self.limit_notices
    }

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

    /// Fold one line's raw bytes: [`Self::fold_line`] when they are UTF-8, else counted unparsed.
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

    /// Add another fold's spend and counters to this one (the files of one session).
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
        self.series.absorb(&other.series);
        self.unstamped = self.unstamped.saturating_add(other.unstamped);
        self.generation = self.generation.wrapping_add(other.generation);
        if self.last_model.is_none() {
            self.last_model.clone_from(&other.last_model);
        }
    }

    /// Forget all but the newest `keep` message ids, for a finished file.
    pub(crate) fn shed_seen(&mut self, keep: usize) {
        self.seen.shed(keep);
        self.seen.order.shrink_to_fit();
    }

    /// How many message ids the dedupe set holds.
    #[cfg(test)]
    pub(crate) fn seen_ids(&self) -> usize {
        self.seen.order.len()
    }

    /// Count one line longer than [`MAX_LINE_BYTES`] that a streaming reader discarded without holding it.
    pub(crate) const fn note_long_line(&mut self) {
        self.rows = self.rows.saturating_add(1);
        self.rows_skipped_long = self.rows_skipped_long.saturating_add(1);
    }

    fn model_key<'a>(&self, model: &'a str) -> &'a str {
        if self.per_model.contains_key(model) || self.per_model.len() < MAX_MODELS {
            model
        } else {
            OTHER_MODEL
        }
    }

    fn add(&mut self, model: &str, usage: TokenUsage, sidechain: bool) {
        if !sidechain && model != UNKNOWN_MODEL && super::upgrade::is_model_id(model) {
            self.last_model = Some(model.to_owned());
        }
        let key = self.model_key(model).to_owned();
        self.per_model.entry(key).or_default().add(usage);
    }
}

/// Whether this line provably is not an assistant row, decided on the raw bytes so the JSON parser never sees it.
pub(crate) fn cannot_be_assistant(line: &str) -> bool {
    line.starts_with('{')
        && line.ends_with('}')
        && !line.contains("assistant")
        && !line.contains(TURN_DURATION)
        && !line.contains("\\u")
}

const TURN_DURATION: &str = "turn_duration";

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
    /// List dollars, where the caller priced every turn of it (Codex's rollouts, `spend::fold_conversation`).
    pub usd: Option<f64>,
}

/// One rate-limit window of an account as the usage tap read it ([`super::tap::Window`]), or as a Codex rollout wrote it ([`super::accounts::Sample`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowView {
    /// `used_percentage`; a Codex share taken up to the next whole percent.
    pub used_pct: u32,
    /// `resets_at`, Unix seconds.
    pub resets_at: i64,
    /// Seconds since its figures were first reported since its process was last served ([`super::tap::Reading::at`]), or since a Codex rollout's reading wrote its share, at the view's `as_of`.
    pub age_s: u64,
    /// Where it was read: [`super::accounts::SOURCE_TAP`], or a Codex rollout ([`super::source::Source::Rollout`]).
    pub source: &'static str,
}

/// [`WindowView::source`] of a figure the accounts store holds of the account beyond the session's own reading ([`AccountView::add_held`]): another tapped session's on the same login, a sample, a standing wall, Claude Code's warning.
pub const SOURCE_STORE: &str = "store";

/// WHY A WINDOW OF THE SESSION'S ACCOUNT HAS NO FIGURE after every source aterm holds was asked ([`AccountView::note_gaps`]): [`hud_line`] prints `–` for it (never a bare `?`), [`gap_lines`] says why, and `--json` carries [`Self::word`] under the account's `missing`. aterm never polls `/api/oauth/usage` for one (accounts v2 R-12), so none is invented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowGap {
    /// Claude Code reports another window of the login but not this one: it hands a window only while the API reports it (2.1.316's statusLine `rate_limits` keeps a window only while the last reply's headers carried both its utilization and its reset), so the vendor reports none now, and nothing else aterm read has it ([`Consulted`]).
    NotReported,
    /// The session's own reading carries no rate-limit window at all: none comes before the first reply, and none for a login that is no Claude subscription.
    NoFigures,
    /// No usage-tap reading of this process (it runs without aterm's tap, or wrote none since it started), and nothing else aterm read has the window ([`Consulted`]).
    NoReading,
}

impl WindowGap {
    /// The closed word `--json` carries.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            WindowGap::NotReported => "not-reported",
            WindowGap::NoFigures => "no-figures",
            WindowGap::NoReading => "no-reading",
        }
    }

    /// WHY A WINDOW IS EMPTY, from the session's own usage-tap `reading` (`None`: none of its process) and whether the accounts store's newest sample of the account carried the other window and not this one (`unreported`, [`super::account_stats::Missing::NotReported`]).
    #[must_use]
    pub fn of(reading: Option<&super::tap::Reading>, unreported: bool) -> WindowGap {
        match reading {
            Some(r) if r.has_figures() => WindowGap::NotReported,
            _ if unreported => WindowGap::NotReported,
            Some(_) => WindowGap::NoFigures,
            None => WindowGap::NoReading,
        }
    }
}

/// WHAT aterm READ BESIDE THE SESSION'S OWN READING before it called a window empty
/// ([`AccountView::note_gaps`]): [`gap_lines`] says a source holds none only where it was read,
/// and says why one was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Consulted {
    /// The accounts store's record of the session's account was read
    /// ([`super::account_stats::home_windows`] ran on it).
    pub store: bool,
    /// What became of the default home's cached `/usage` body.
    pub usage_cache: CacheRead,
}

/// WHAT BECAME OF THE DEFAULT HOME'S CACHED `/usage` BODY for the session's account ([`Consulted`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheRead {
    /// Not looked at: no account was judged, so nothing beside the session's reading was read.
    #[default]
    Unasked,
    /// Read as the account's: the process runs on the default home's own login, and the home's
    /// profile names no other account than the record's.
    Read,
    /// Not used: the process is not shown to run on the default home's login (several logins, a
    /// login of its own), or the home's profile names another account than the record's.
    OtherLogin,
    /// The process runs on the default home's login, but its profile could not be read.
    Unreadable,
}

impl Consulted {
    /// What [`gap_lines`] adds to a reason: which sources hold none, and why any other was not read.
    fn clause(self) -> &'static str {
        match (self.store, self.usage_cache) {
            (false, _) => {
                "the accounts store and the /usage cache were not read: aterm judged no account \
                 for this process"
            }
            (true, CacheRead::Read) => "the accounts store and the /usage cache hold none either",
            (true, CacheRead::OtherLogin) => {
                "the accounts store holds none either, and the /usage cache was not used: it is \
                 the default home's, and this process is not shown to run on its account"
            }
            (true, CacheRead::Unreadable) => {
                "the accounts store holds none either, and the default home's profile, which \
                 holds the /usage cache, could not be read"
            }
            (true, CacheRead::Unasked) => {
                "the accounts store holds none either, and the /usage cache was not read"
            }
        }
    }

    /// The `--json` words of the sources read (`missing`'s companion `consulted`).
    fn words(self) -> Vec<&'static str> {
        [
            (self.store, "store"),
            (
                self.store && self.usage_cache == CacheRead::Read,
                "usage-cache",
            ),
        ]
        .into_iter()
        .filter_map(|(read, word)| read.then_some(word))
        .collect()
    }
}

/// One account: its spend (per model), the model its live session runs, and the windows its usage tap or its Codex rollouts read.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    /// The owner's label for the account (`accounts.toml`).
    pub label: String,
    /// Whether this is the account the live session runs under: always, for the one account `harness usage` shows.
    pub active: bool,
    /// Spend by model id.
    pub spend: BTreeMap<String, SpendView>,
    /// The session's model id ([`TranscriptUsage::last_model`], taken by [`Self::add_transcript`]).
    pub model: Option<String>,
    /// The rate-limit windows by the vendor's name (`five_hour`, `seven_day`), from the usage tap's reading for the session's own process ([`Self::add_reading`]), what aterm holds of the account beside it ([`Self::add_held`]), or a Codex conversation's newest reading ([`Self::add_sample`]).
    pub windows: BTreeMap<String, WindowView>,
    /// Why a window has no figure, by the vendor's name, where the caller asked ([`Self::note_gaps`]).
    pub gaps: BTreeMap<String, WindowGap>,
    /// What was read beside the session's own reading before a window was called empty ([`Self::note_gaps`]).
    pub consulted: Consulted,
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
            gaps: BTreeMap::new(),
            consulted: Consulted::default(),
        }
    }

    /// TAKE A FIGURE aterm HOLDS OF THE ACCOUNT for window `name` (`five_hour`, `seven_day`) where the view has none yet: the accounts store's ([`SOURCE_STORE`]) or the home's cached `/usage` body's ([`super::account_stats::SOURCE_USAGE_CACHE`]), current and not cut by a limit reset ([`super::account_stats::home_windows`]), as old as `as_of` makes it. The session's own reading always wins.
    pub fn add_held(&mut self, name: &str, fig: &super::accounts::WindowFig, as_of: i64) {
        use super::account_stats::SOURCE_USAGE_CACHE;
        if self.windows.contains_key(name) || !fig.current(as_of) || fig.cut() {
            return;
        }
        let source = if fig.source == SOURCE_USAGE_CACHE {
            SOURCE_USAGE_CACHE
        } else {
            SOURCE_STORE
        };
        self.windows.insert(
            name.to_owned(),
            WindowView {
                used_pct: fig.whole(),
                resets_at: fig.reset,
                age_s: u64::try_from(as_of.saturating_sub(fig.at)).unwrap_or(0),
                source,
            },
        );
    }

    /// SAY WHY THE FIVE-HOUR WINDOW OR THE WEEK STILL HAS NO FIGURE once every source has filled its windows ([`WindowGap::of`]): from the session's own usage-tap `reading`, what else was read (`consulted`), and `unreported`, which answers for a window name whether the store says the vendor reports none.
    pub fn note_gaps(
        &mut self,
        reading: Option<&super::tap::Reading>,
        consulted: Consulted,
        unreported: impl Fn(&str) -> bool,
    ) {
        self.consulted = consulted;
        for name in ["five_hour", "seven_day"] {
            if !self.windows.contains_key(name) {
                self.gaps
                    .insert(name.to_owned(), WindowGap::of(reading, unreported(name)));
            }
        }
    }

    /// Take the windows of a usage tap's `reading` ([`super::tap::Reading`]), as old as `as_of` makes it.
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
                        // `pct_x10` is the authoritative figure when the tap file has it. Early
                        // format-v1 writers rounded 99.9x to the legacy whole `pct = 100`; deriving
                        // the display whole from the tenths keeps those persisted rows below 100.
                        used_pct: super::decimal::whole_used(w.x10()),
                        resets_at: w.resets_at,
                        age_s,
                        source: super::accounts::SOURCE_TAP,
                    },
                );
            }
        }
    }

    /// Take the five-hour and week windows of a usage `sample` ([`super::accounts::Sample`]),
    /// spelled `source`, each as old as `as_of` makes the moment its share was read.
    ///
    /// A WINDOW THE SAMPLE GIVES NO SHARE OR NO RESET IS LEFT OUT, so the HUD shows `?` for it
    /// rather than a figure the sample does not hold.
    pub fn add_sample(
        &mut self,
        sample: &super::accounts::Sample,
        source: &'static str,
        as_of: i64,
    ) {
        for (name, pct, reset, read_at) in [
            (
                "five_hour",
                sample.session_pct,
                sample.session_reset,
                sample.session_read_at,
            ),
            (
                "seven_day",
                sample.week_pct,
                sample.week_reset,
                sample.week_read_at,
            ),
        ] {
            let (Some(used_pct), Some(resets_at)) = (pct, reset) else {
                continue;
            };
            let read = read_at.unwrap_or(sample.at);
            self.windows.insert(
                name.to_owned(),
                WindowView {
                    used_pct,
                    resets_at,
                    age_s: u64::try_from(as_of.saturating_sub(read)).unwrap_or(0),
                    source,
                },
            );
        }
    }

    /// Take the per-model spend from a transcript fold, and the model its newest summed row names.
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
    pub const fn new(as_of: i64) -> Self {
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

/// One rate-limit window in a CLI's `/usage` panel, before its reset text is placed on a clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelWindow {
    /// The window key this row names (the vendor's own: `five_hour`, …).
    pub name: String,
    /// The whole-number percent printed (`7% used` → 7).
    pub used_pct: u32,
    /// The text after `Resets `, exactly as painted and not placed on a clock.
    pub reset_text: Option<String>,
}

const PANEL_TITLES: [(&str, &str); 5] = [
    ("Current session", "five_hour"),
    ("Current week (all models)", "seven_day"),
    ("Current week (Sonnet only)", "seven_day_sonnet"),
    ("Current week (Fable)", FABLE_WEEK_WINDOW),
    ("Spend limit", "spend_limit"),
];

const PANEL_BLOCK_ROWS: usize = 7;
const PANEL_KEY_CAP: usize = 40;

fn is_bar_glyph(c: char) -> bool {
    ('\u{2580}'..='\u{259f}').contains(&c)
}

fn pct_used(s: &str) -> Option<u32> {
    let at = s.find("% used")?;
    let head = &s[..at];
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    head[head.len() - digits..].parse().ok()
}

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

/// The rate-limit windows Claude Code painted on its `/usage` panel, read off the rows of aterm's own parsed grid.
///
/// Rows with no Settings panel around them prove nothing of where they came from, so fewer than two
/// windows read as none: one `Current session: 99% used` in prose is not a panel. The Usage tab's own
/// reader ([`usage_panel_read`]) has the panel's header, tab mark and tail for that proof and takes
/// even one window.
#[must_use]
pub fn usage_panel_windows(rows: &[String]) -> Vec<PanelWindow> {
    let out = panel_windows(rows);
    if out.len() < 2 {
        return Vec::new();
    }
    out
}

/// Every window block on `rows`, each read whether or not any other is there ([`usage_panel_windows`]).
fn panel_windows(rows: &[String]) -> Vec<PanelWindow> {
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
    out
}

/// The windows in Kimi Code's complete inline `/usage` panel, including its optional legacy
/// weekly window and its monthly total separately.
#[must_use]
pub fn kimi_usage_windows(rows: &[String]) -> Vec<PanelWindow> {
    let Some((start, _)) = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, row)| row.trim_start().starts_with("╭ Usage "))
    else {
        return Vec::new();
    };
    let Some(end) = rows
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(i, row)| row.trim_start().starts_with('╰').then_some(i))
    else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for row in &rows[start + 1..end] {
        let text = row.trim().trim_matches(['│', ' ']).trim();
        let name = if text.starts_with("5h limit") {
            SESSION_WINDOW
        } else if text.starts_with("Weekly limit") || text.starts_with("Week limit") {
            WEEK_WINDOW
        } else if text.starts_with("Monthly limit") {
            super::accounts::MONTH_LIMIT
        } else {
            continue;
        };
        let Some(used_pct) = pct_used(text).filter(|pct| *pct <= 100) else {
            continue;
        };
        windows.push(PanelWindow {
            name: name.to_owned(),
            used_pct,
            reset_text: reset_after(text)
                .map(|reset| reset.trim_end_matches(['│', ' ']).trim_end().to_owned()),
        });
    }
    windows
}

/// The command the accounts probe types to open the panel.
pub const USAGE_COMMAND: &str = "/usage";

/// What one read of the rows under the Settings panel's header says of the Usage tab ([`usage_panel_read`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelRead {
    /// No row only the Usage tab draws is under the header: another tab, or something that is not this panel.
    NotUsage,
    /// The tab says its figures are not settled, or the whole tab is on the grid with neither the
    /// session's nor the week's window on it (a load that has drawn the Session block alone).
    Unsettled,
    /// The panel's tail is not on the grid.
    Fold,
    /// The whole tab is on the grid with nothing unsettled on it: its windows, the session's
    /// (`five_hour`) or the week's (`seven_day`) among them. Claude Code draws a window only where
    /// the account has its figure (2.1.318 draws no `Current session` for an account with no
    /// five-hour figure), so either one is a whole read.
    Settled(Vec<PanelWindow>),
}

/// What `rows` say of the Usage tab, read from the row under `header` (the Settings panel's header row, `aterm_phase::settings_panel`) to the end.
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
    let windows = panel_windows(under);
    let has = |name: &str| windows.iter().any(|w| w.name == name);
    if has(SESSION_WINDOW) || has(WEEK_WINDOW) {
        PanelRead::Settled(windows)
    } else {
        PanelRead::Unsettled
    }
}

const SESSION_WINDOW: &str = "five_hour";
const WEEK_WINDOW: &str = "seven_day";

/// The window key of the panel's `Current week (Fable)` row ([`PANEL_TITLES`]): Fable's own week,
/// the bucket its limit notice names ([`super::profile::Bucket::Fable`]).
pub const FABLE_WEEK_WINDOW: &str = super::profile::Bucket::Fable.id();

/// A settled panel's windows as the roster's sample ([`super::accounts::Sample`], what `accounts::note_sample` records).
#[must_use]
pub fn panel_sample(
    windows: &[PanelWindow],
    now: i64,
    local_offset_s: i64,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> Option<super::accounts::Sample> {
    panel_sample_local(windows, now, &|_| Some(local_offset_s), zone_offset_at)
}

/// [`panel_sample`], reading the local zone at each candidate reset instant (`local_at`).
#[must_use]
pub fn panel_sample_local(
    windows: &[PanelWindow],
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> Option<super::accounts::Sample> {
    use super::accounts::{RESET_BELIEVED_S, SESSION_WINDOW_S, STAMP_SKEW_S, Sample};
    let read = |name: &str, ahead_max: i64| {
        panel_window_local(windows, name, ahead_max, now, local_at, zone_offset_at)
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
        session_read_at: session.map(|_| now),
        week_read_at: week.map(|_| now),
        at: now,
        by: String::new(),
    })
}

/// The settled panel's window `name` as its percent used and its reset placed on the clock at
/// `now` (`local_at`, `zone_offset_at`: [`panel_sample_local`]'s), a reset more than `ahead_max`
/// seconds ahead read as none; `None` where the panel drew no such row.
#[must_use]
pub fn panel_window_local(
    windows: &[PanelWindow],
    name: &str,
    ahead_max: i64,
    now: i64,
    local_at: &dyn Fn(i64) -> Option<i64>,
    zone_offset_at: &dyn Fn(&str, i64) -> Option<i64>,
) -> Option<(u32, Option<i64>)> {
    use crate::supervise::limit;
    let window = windows.iter().find(|w| w.name == name)?;
    let reset = window
        .reset_text
        .as_deref()
        .and_then(limit::parse_reset)
        .map(|spec| limit::reset_at_local(&spec, now, local_at, zone_offset_at))
        .filter(|at| at.saturating_sub(now) <= ahead_max);
    Some((window.used_pct, reset))
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

/// The family word of a model id: `claude-fable-5-1` → `fable`, `claude-3-5-haiku-20241022` → `haiku`.
pub fn model_short(id: &str) -> String {
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    rest.split('-')
        .find(|seg| seg.chars().next().is_some_and(char::is_alphabetic))
        .unwrap_or(rest)
        .to_owned()
}

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

fn truncate_to(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let cut = super::truncate_bytes(&s, max).len();
    s.truncate(cut);
    s
}

/// The one-line HUD: `fable ?%/5h · ?%/7d · acct work · resets ? · none`; a window whose emptiness the caller explained ([`AccountView::gaps`]) reads `–` (`opus –/5h · 1%/7d · …`), as does `resets` where both are, and [`gap_lines`] says why. The source and age are each window's where they differ (`5h tap 5s · 7d store 5d`).
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
    let pct = |name: &str| match (acct.windows.get(name), acct.gaps.contains_key(name)) {
        (Some(w), _) => format!("{}%", w.used_pct),
        (None, true) => "\u{2013}".to_owned(),
        (None, false) => "?%".to_owned(),
    };
    let explained = ["five_hour", "seven_day"]
        .iter()
        .all(|name| acct.gaps.contains_key(*name));
    let resets = five.or(seven).map_or_else(
        || if explained { "\u{2013}" } else { "?" }.to_owned(),
        |w| {
            let left = u64::try_from(w.resets_at.saturating_sub(view.as_of)).unwrap_or(0);
            format!("in {}", super::upgrade::span(left))
        },
    );
    // Each window's own source and age where they differ, so a week the store held for days
    // never reads as fresh as the tap's five-hour figure beside it.
    let each = |w: &WindowView| format!("{} {}", w.source, super::upgrade::span(w.age_s));
    let source = match (five, seven) {
        (Some(f), Some(s)) if each(f) != each(s) => format!("5h {} · 7d {}", each(f), each(s)),
        (Some(w), _) | (None, Some(w)) => each(w),
        (None, None) => "none".to_owned(),
    };
    truncate_to(
        format!(
            "{model} {}/5h · {}/7d · acct {} · resets {resets} · {source}",
            pct("five_hour"),
            pct("seven_day"),
            sanitize(&acct.label)
        ),
        HUD_MAX_BYTES,
    )
}

/// WHY EACH EMPTY WINDOW OF THE HUD'S ACCOUNT IS EMPTY ([`AccountView::gaps`]), one line per reason, the windows it covers first: `5h –: not reported: …`.
pub fn gap_lines(view: &UsageView) -> Vec<String> {
    let Some(acct) = view.shown() else {
        return Vec::new();
    };
    let label = |name: &str| if name == "five_hour" { "5h" } else { "7d" };
    let mut lines: Vec<(Vec<&str>, WindowGap, &str)> = Vec::new();
    for name in ["five_hour", "seven_day"] {
        let Some(&gap) = acct.gaps.get(name) else {
            continue;
        };
        match lines.iter_mut().find(|(_, g, _)| *g == gap) {
            Some((names, ..)) => names.push(label(name)),
            None => lines.push((vec![label(name)], gap, name)),
        }
    }
    lines
        .into_iter()
        .map(|(names, gap, first)| {
            let why = match gap {
                WindowGap::NotReported => {
                    let (this, other) = if first == "five_hour" {
                        ("5-hour window", "week")
                    } else {
                        ("week", "5-hour window")
                    };
                    format!(
                        "not reported: Claude Code reports this login's {other} but no {this} (it \
                         hands a window only while the API reports one)"
                    )
                }
                WindowGap::NoFigures => "Claude Code's status line for this process carries no \
                                         rate-limit window: none comes before the first reply, \
                                         and none for a login that is no Claude subscription"
                    .to_owned(),
                WindowGap::NoReading => "no usage-tap reading of this Claude Code process (it \
                                         runs without aterm's tap, or wrote none since it \
                                         started)"
                    .to_owned(),
            };
            format!(
                "{} \u{2013}: {why}; {}",
                names.join(", "),
                acct.consulted.clause()
            )
        })
        .collect()
}

/// `YYYY-MM-DDTHH:MM:SSZ` of an epoch instant, before the epoch too (the calendar is `aterm_types::rfc3339`'s, the workspace's one copy).
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
    o.insert(
        "windows".to_owned(),
        Value::Object(
            a.windows
                .iter()
                .map(|(name, w)| {
                    let mut m = Map::new();
                    m.insert("used_pct".to_owned(), Value::from(u64::from(w.used_pct)));
                    m.insert("resets_at".to_owned(), Value::from(w.resets_at));
                    m.insert("source".to_owned(), Value::from(w.source));
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
    if !a.gaps.is_empty() {
        o.insert(
            "missing".to_owned(),
            Value::Object(
                a.gaps
                    .iter()
                    .map(|(name, gap)| (name.clone(), Value::from(gap.word())))
                    .collect(),
            ),
        );
        o.insert(
            "consulted".to_owned(),
            Value::Array(a.consulted.words().into_iter().map(Value::from).collect()),
        );
    }
    Value::Object(o)
}

/// The `harness usage --json` document, schema 1, keys in sorted order.
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
    aterm_json::to_string(&Value::Object(root))
        .unwrap_or_else(|_| r#"{"schema":1,"accounts":[],"error":"serialize"}"#.to_owned())
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
