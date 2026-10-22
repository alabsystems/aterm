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
//! Codex path of `harness usage`, priced by `spend`).
//!
//! The rate-limit windows come only from the usage tap's reading for the
//! session's own process ([`AccountView::windows`], [`super::tap`]); without
//! one they stay unknown (`"windows":{}`, `?%` in the HUD). `harness limits`
//! prints a painted panel's instead ([`usage_panel_windows`]). The fold also
//! feeds the window footer's `Σ` tokens and limit wall ([`LimitNoticeRow`])
//! through [`super::session_usage`].

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

/// A `message.id` longer than this is treated as absent (summed, not
/// deduped), so a hostile id cannot fill the dedupe set. Real ids are `msg_…`
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
    /// The summed rows' tokens over time ([`Self::series`]).
    series: TokenSeries,
    /// Summed rows with no `timestamp`: in the totals, not in the series.
    unstamped: u64,
    /// Moves whenever the series or the working time does
    /// ([`Self::generation`]).
    generation: u64,
}

/// The time Claude was working in one transcript: the union of its main
/// chain's turns, each `[timestamp − durationMs, timestamp]` of the
/// `{"type":"system","subtype":"turn_duration"}` row Claude Code writes at
/// every turn's end. The union, not the sum: a turn that waited on a
/// workflow's subagents is written when they finish, so it can overlap the
/// turns after it; the first-to-last span is mostly idle.
///
/// Held as disjoint spans in time order with their running sum: a new turn
/// merges with the spans it overlaps, which are at the back in a transcript
/// written in time order, so a row costs O(1) amortised. Past
/// [`BUSY_SPANS_MAX`] spans the oldest leaves the deque (its time stays in the
/// sum), and a later turn reaching back over it counts that overlap twice —
/// the stated bound, far past a real session's turn count.
///
/// Not counted: a turn the person interrupts (Esc) and one in flight when the
/// process ends write no row, so that time is lost (an undercount). A running
/// turn counts once it ends, so the figure never goes down.
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

/// The most buckets a [`TokenSeries`] holds.
pub const SERIES_BUCKETS: usize = 120;

/// The finest bucket of a [`TokenSeries`], in seconds: a minute.
pub const SERIES_BASE_S: i64 = 60;

/// The session's tokens over time, for the footer's traffic graph: each
/// summed row's tokens in the bucket of its own `timestamp` — in, the prompt
/// ([`TokenUsage::prompt`]: input, cache writes and reads, the `Σ ↑`), and
/// out, the output (the `Σ ↓`).
///
/// Buckets are ALIGNED TO THE EPOCH: bucket `i` at width `w` holds the rows
/// stamped in `[i·w, (i+1)·w)`, `w` [`SERIES_BASE_S`] times a power of two. A
/// row that would put the span past [`SERIES_BUCKETS`] COARSENS the series:
/// the width doubles and aligned pairs merge exactly, until the row fits. No
/// history is ever dropped and the sum of the buckets is the sum of every row
/// noted; two series merge exactly ([`Self::absorb`]). A row costs O(1)
/// amortised (a coarsening halves the buckets).
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
    pub fn width_s(&self) -> i64 {
        self.width_s
    }

    /// Unix seconds of the first bucket's start (0 while empty).
    #[must_use]
    pub fn start_s(&self) -> i64 {
        self.first.saturating_mul(self.width_s)
    }

    /// `(in, out)` per bucket, oldest first.
    #[must_use]
    pub fn buckets(&self) -> &VecDeque<(u64, u64)> {
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

    /// One bucket index past the last.
    fn end(&self) -> i64 {
        self.first
            .saturating_add(i64::try_from(self.buckets.len()).unwrap_or(i64::MAX))
    }

    /// Double the width, merging each aligned pair: bucket `i` goes to
    /// `⌊i/2⌋`. The sum is unchanged.
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

    /// Whether buckets `lo..hi` fit the series at its width.
    fn fits(lo: i64, hi: i64) -> bool {
        hi.saturating_sub(lo) <= i64::try_from(SERIES_BUCKETS).unwrap_or(i64::MAX)
    }

    /// Widen the buckets to cover `lo..hi` (indices at the width), zeros in
    /// the new ones.
    fn cover(&mut self, lo: i64, hi: i64) {
        while self.first > lo {
            self.buckets.push_front((0, 0));
            self.first -= 1;
        }
        while self.end() < hi {
            self.buckets.push_back((0, 0));
        }
    }

    /// Note a row stamped `at_s` (unix seconds) with `input` and `output`
    /// tokens. A row of no tokens notes nothing.
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
                // The width can double no further: the row joins the
                // nearer end's bucket.
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

    /// Merge `other` in: the finer of the two is coarsened to the wider width
    /// (and both further while their span passes [`SERIES_BUCKETS`]), then
    /// bucket adds to bucket. Exact: the widths are both
    /// [`SERIES_BASE_S`]·2ᵏ, so their buckets align.
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

    /// The buckets spread over `n` columns of `from_s..to_s` (unix seconds),
    /// for a graph `n` cells wide: each bucket's tokens go to the columns its
    /// span overlaps, in proportion, rounded so every token lands in exactly
    /// one column. A bucket the window clips puts ALL its tokens in the part
    /// inside: a window that ends at now, inside the newest bucket, holds
    /// every token stamped so far, so over the series' whole span the
    /// columns sum to [`Self::sum`]. Pure. Empty for no columns; zeros for
    /// an empty window.
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

/// [`TokenSeries::columns`] over any run of buckets: `buckets` from bucket
/// index `first`, `width_s` seconds each, spread over `n` columns of
/// `from_s..to_s` — what a series and a flat view of one
/// (`session_usage::TrafficView::columns`) both draw with. Pure.
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
    // Column `c` covers `[from + c·span/n, from + (c+1)·span/n)`, in
    // seconds scaled by `n` to stay whole.
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
        // The columns the clipped span touches, and the share of each
        // by cumulative rounding: every token lands in exactly one.
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

/// A limit notice as Claude Code wrote it into the transcript: an assistant
/// row with `isApiErrorMessage: true` and `error: rate_limit` whose text is
/// the notice (`You've hit your session limit · resets 3pm (…)`), stamped with
/// the row's own `timestamp` (the login wall's row shape,
/// `upgrade::transcript_login_wall`). A rejected rate limit writes such a row;
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

/// Unix milliseconds of a transcript row's own `timestamp`: [`row_time`]
/// plus its fraction's first three digits (`YYYY-MM-DDTHH:MM:SS.sssZ`).
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

/// The window a limit notice's text names: `session limit` / `5-hour limit`
/// is `five_hour`, a weekly one `seven_day`. `None` for a notice that names
/// none (`You've hit your limit`, which the vendor also prints for a weekly
/// wall) and for any other wall. Which wall it is at all is
/// `aterm_phase::classify_wall`'s answer.
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

    /// An empty fold that replaces `older` (its file started over): its
    /// [`Self::generation`] starts past `older`'s, so a reader keyed on it
    /// never takes the new fold for the old one.
    pub(crate) fn following(older: &Self) -> Self {
        Self {
            generation: older.generation.wrapping_add(1),
            ..Self::default()
        }
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

    /// The main chain's finished turns as disjoint `[start, end)` spans in
    /// unix milliseconds, in time order ([`BusySpans`]: the newest
    /// [`BUSY_SPANS_MAX`]).
    #[must_use]
    pub fn busy_spans(&self) -> &VecDeque<(i64, i64)> {
        &self.busy.spans
    }

    /// The summed rows' tokens over time ([`TokenSeries`]): a new message's
    /// whole usage, a grown repeat's growth alone, each at its row's own
    /// `timestamp`.
    #[must_use]
    pub fn series(&self) -> &TokenSeries {
        &self.series
    }

    /// Summed rows the series could not place (no `timestamp`): counted in
    /// the totals, not graphed.
    #[must_use]
    pub fn unstamped(&self) -> u64 {
        self.unstamped
    }

    /// A count that moves whenever the series, the unplaced rows or the
    /// working time move: a reader rebuilds what it drew from them only when
    /// it did.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Put a summed row's `usage` (a new message's, or a repeat's growth) in
    /// the series at its `timestamp`, else count it unplaced.
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
    /// (each file deduped within itself). The series merge exactly
    /// ([`TokenSeries::absorb`]). Limit notices, the served time and the
    /// working time are not summed: the limit wall is the main transcript's
    /// (`session_usage::SessionUsage::main`), and so is the working time.
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
    /// Seconds since its figures were first reported since its process was
    /// last served ([`super::tap::Reading::at`]), at the view's `as_of`: the
    /// figure's age, not the file's.
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

/// One rate-limit window Claude Code painted on its `/usage` panel, before
/// its reset text is placed on a clock; the producer of
/// [`super::source::Source::Grid`]. The panel is the only surface read that
/// paints a quota figure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelWindow {
    /// The window key this row names (the vendor's own: `five_hour`, …).
    pub name: String,
    /// The whole-number percent printed (`7% used` → 7). The vendor floors its
    /// own figure, so this is the painted integer, never re-derived.
    pub used_pct: u32,
    /// The text after `Resets `, exactly as painted and not placed on a clock.
    /// `None` when the row carried no
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
/// an unknown `Current week (<display name>)` title.
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
// The accounts probe's read of the panel (P10, P22)
// ---------------------------------------------------------------------------

/// The command the accounts probe types to open the panel. `usage` is an
/// `immediate` `local-jsx` command sharing its prefix with `usage-credits`; on
/// 2.1.287 the typeahead lists `/usage` first and Enter opens the Settings
/// panel on its Usage tab. A later build's typeahead order is its own.
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

/// What `rows` say of the Usage tab, read from the row under `header` (the
/// Settings panel's header row, `aterm_phase::settings_panel`) to the end.
/// First match wins:
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
/// Why the tail: the tab opens on last-known figures with `Refreshing…` drawn
/// below the windows, and the hint row is drawn last. Where the tail is under
/// the fold the refresh line may be too, and the visible figures may be about
/// to be replaced, so no tail, no sample. The cost: the whole tab is on the
/// grid only until its contributing block (`Scanning local sessions…`) fills
/// in and pushes the hint under the fold — 1.0–2.4 s at 60x50 to 150x50, never
/// at 110x32, always at 150x80 — so a caller that waits before its first read
/// can record nothing.
///
/// Pure, and one read's verdict only: a caller that records a sample asks for
/// two consecutive equal `Settled` reads (the spec's P10). The rows are data;
/// a quoted copy of a panel is refused where the panel is placed
/// (`aterm_phase::settings_panel`), and this trusts `header`.
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
/// Each reset is placed as `harness limits` places it
/// ([`super::cli::limits_view`]): `limit::parse_reset`, then
/// `limit::reset_at_with` with the injected clock (`now`), local offset and
/// zone table (`zone_offset_at(zone, unix)`: the zone's offset at that
/// instant), so this reads no clock or zone file. A reset the grammar cannot
/// read is `None`, and so is one further ahead than its window can be (past
/// [`super::accounts::SESSION_WINDOW_S`] + [`super::accounts::STAMP_SKEW_S`]
/// for the session, [`super::accounts::RESET_BELIEVED_S`] for the week); its
/// figure is kept, and the roster counts it for one window length from the
/// sample (`Sample::session`, `Sample::week`). A reset in the past is kept and
/// voids its figure: that window has ended.
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

/// Cut a string to at most `max` bytes on a char boundary
/// (`super::truncate_bytes`).
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
/// ([`AccountView::label`]). The figures and reset come from the account's
/// tap reading (source `tap <age>`); without one they are `?` and the source
/// `none`. Cut to [`HUD_MAX_BYTES`] on a char boundary.
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
    // has no reading (module doc).
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

    /// The `/usage` panel as Claude Code 2.1.278 painted it in a 120-column
    /// session. The literal opens on its first row, so the vendor's
    /// three-space indent survives (the Settings-panel reader places the
    /// header by it).
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

    /// The same panel at 56 columns (same session, resized): the reset wraps
    /// onto the row under the title, and the meter, a blank and the figure
    /// follow it.
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

    // -- TokenSeries --------------------------------------------------------

    /// An assistant row stamped `at` (RFC 3339) with `input` prompt tokens
    /// (no cache) and `output`.
    fn stamped_row(id: &str, input: u64, output: u64, at: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{at}","isSidechain":false,"message":{{"id":"{id}","model":"m","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
    }

    /// Unix seconds of `at`.
    fn unix(at: &str) -> i64 {
        i64::try_from(super::super::upgrade_models::parse_utc(at).expect("a stamp")).unwrap()
    }

    /// The fold's totals, `(prompt, output)` over every model.
    fn totals(t: &TranscriptUsage) -> (u64, u64) {
        t.per_model().values().fold((0, 0), |(i, o), spend| {
            let tokens = spend.tokens();
            (i + tokens.prompt(), o + tokens.output)
        })
    }

    /// EACH ROW IN THE MINUTE OF ITS OWN STAMP: buckets are a minute wide and
    /// aligned to the epoch (the first starts on the minute, not at the
    /// first row), rows of one minute share a bucket, a quiet minute is an
    /// empty bucket, and the prompt counts its cache writes and reads.
    #[test]
    fn the_series_buckets_tokens_by_row_minute() {
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &stamped_row("a", 100, 1, "2026-10-19T12:00:05Z"),
                &stamped_row("b", 200, 2, "2026-10-19T12:00:59.900Z"),
                &stamped_row("c", 400, 4, "2026-10-19T12:01:00Z"),
                &stamped_row("d", 800, 8, "2026-10-19T12:03:10Z"),
                // Cache tokens are prompt tokens: the Σ ↑.
                &assistant_row("e", "m", 1, 16).replacen(
                    '{',
                    r#"{"timestamp":"2026-10-19T12:03:59Z","#,
                    1,
                ),
            ],
        );
        let series = t.series();
        assert_eq!(series.width_s(), SERIES_BASE_S);
        assert_eq!(series.start_s(), unix("2026-10-19T12:00:00Z"));
        assert_eq!(
            series.buckets().iter().copied().collect::<Vec<_>>(),
            [(300, 3), (400, 4), (0, 0), (800 + 1 + 10 + 1000, 8 + 16)]
        );
        assert_eq!(series.sum(), totals(&t), "every row graphed");
        assert_eq!(t.unstamped(), 0);
    }

    /// A ROW PAST THE SPAN COARSENS, NEVER DROPS: the width doubles until
    /// the span fits [`SERIES_BUCKETS`] (the bucket edges still on the
    /// epoch's grid), whichever end the row lands past, and the buckets sum
    /// to every row noted, out to the widest stamps an `i64` holds.
    #[test]
    fn the_series_coarsens_and_keeps_the_sum() {
        let base = unix("2026-10-19T00:00:00Z");
        let mut series = TokenSeries::default();
        let mut sum = (0u64, 0u64);
        // Five hours, one row a minute, then two hours BEFORE the first.
        for minute in (0..300).chain((-120..0).rev()) {
            let (i, o) = (u64::try_from(minute + 1000).unwrap(), 1);
            series.note(base + minute * 60 + 7, i, o);
            sum = (sum.0 + i, sum.1 + o);
            assert!(series.buckets().len() <= SERIES_BUCKETS);
            assert_eq!(series.sum(), sum, "nothing dropped at minute {minute}");
        }
        // Seven hours fit 120 buckets of four minutes.
        assert_eq!(series.width_s(), 4 * SERIES_BASE_S);
        assert_eq!(series.start_s().rem_euclid(series.width_s()), 0);
        assert!(series.start_s() <= base - 120 * 60);
        assert!(series.start_s() + series.width_s() * 120 > base + 299 * 60);
        // A row of no tokens notes nothing and widens nothing.
        let held = series.clone();
        series.note(base + 86_400 * 365, 0, 0);
        assert_eq!(series, held);
        // The extremes still keep every token.
        for at in [i64::MIN, i64::MAX, 0] {
            series.note(at, 5, 5);
            sum = (sum.0 + 5, sum.1 + 5);
        }
        assert!(series.buckets().len() <= SERIES_BUCKETS);
        assert_eq!(series.sum(), sum);
    }

    /// TWO WIDTHS MERGE EXACTLY: the finer series is coarsened to the wider
    /// one's width (and both further while the union passes
    /// [`SERIES_BUCKETS`]), so absorbing either into the other is the series
    /// one fold of every row would hold. Control: the same buckets added
    /// index for index without aligning them differ.
    #[test]
    fn absorb_aligns_two_widths() {
        let base = unix("2026-10-19T09:00:00Z");
        let fine: Vec<(i64, u64, u64)> = (0..10).map(|m| (base + m * 60, 10, 1)).collect();
        // Three hours: two-minute buckets.
        let coarse: Vec<(i64, u64, u64)> = (0..180)
            .step_by(7)
            .map(|m| (base - 3600 + m * 60 + 30, 100, 3))
            .collect();
        let of = |rows: &[(i64, u64, u64)]| {
            let mut series = TokenSeries::default();
            for &(at, i, o) in rows {
                series.note(at, i, o);
            }
            series
        };
        let (a, b) = (of(&fine), of(&coarse));
        assert_eq!(
            (a.width_s(), b.width_s()),
            (SERIES_BASE_S, 2 * SERIES_BASE_S)
        );
        let mut ab = a.clone();
        ab.absorb(&b);
        let mut ba = b.clone();
        ba.absorb(&a);
        let mut all = fine.clone();
        all.extend(&coarse);
        assert_eq!(ab, of(&all), "one fold of every row");
        assert_eq!(ba, ab);
        assert_eq!(ab.sum(), (a.sum().0 + b.sum().0, a.sum().1 + b.sum().1));
        // Merging nothing changes nothing; merging into nothing copies.
        let mut held = ab.clone();
        held.absorb(&TokenSeries::default());
        assert_eq!(held, ab);
        let mut empty = TokenSeries::default();
        empty.absorb(&b);
        assert_eq!(empty, b);
        // CONTROL: unaligned, the fine minutes land in the wrong buckets.
        let mut naive = b.clone();
        for (k, &(i, o)) in a.buckets().iter().enumerate() {
            let slot = &mut naive.buckets[k];
            *slot = (slot.0 + i, slot.1 + o);
        }
        assert_ne!(naive, ab);
    }

    /// A REPEAT THAT GREW ADDS ITS GROWTH ALONE, in the bucket of the row
    /// that carried it: the series sums as the totals do.
    #[test]
    fn a_grown_repeat_adds_only_its_growth() {
        let mut t = TranscriptUsage::new();
        let first = stamped_row("msg_c", 100, 10, "2026-10-19T12:00:10Z");
        let grown = stamped_row("msg_c", 100, 999, "2026-10-19T12:01:10Z");
        let lower = stamped_row("msg_c", 50, 20, "2026-10-19T12:02:10Z");
        assert_eq!(
            fold_all(&mut t, &[&first, &grown, &lower]),
            [Fold::Summed, Fold::Conflict, Fold::Conflict]
        );
        assert_eq!(
            t.series().buckets().iter().copied().collect::<Vec<_>>(),
            [(100, 10), (0, 989)],
            "the growth alone, in its own row's minute; a lower repeat adds nothing"
        );
        assert_eq!(t.series().sum(), totals(&t));
        assert_eq!(totals(&t), (100, 999));
    }

    /// A ROW WITH NO STAMP is summed in the totals and counted, never placed
    /// in the series; every summed row moves the generation, and a row that
    /// sums nothing moves nothing. Control: the same row stamped is graphed.
    #[test]
    fn a_row_without_a_timestamp_counts_in_the_totals_not_the_series() {
        let mut t = TranscriptUsage::new();
        let bare = assistant_row("n1", "m", 100, 10);
        assert_eq!(t.fold_line(&bare), Fold::Summed);
        assert_eq!(totals(&t), (100 + 10 + 1000, 10));
        assert!(t.series().is_empty());
        assert_eq!(t.unstamped(), 1);
        let generation = t.generation();
        assert_ne!(generation, 0);
        assert_eq!(t.fold_line(&bare), Fold::Duplicate);
        assert_eq!(t.generation(), generation, "a duplicate sums nothing");
        // CONTROL: stamped, the same tokens are graphed.
        let stamped = assistant_row("n2", "m", 100, 10).replacen(
            '{',
            r#"{"timestamp":"2026-10-19T12:00:00Z","#,
            1,
        );
        assert_eq!(t.fold_line(&stamped), Fold::Summed);
        assert_eq!(t.series().sum(), (100 + 10 + 1000, 10));
        assert_eq!(t.unstamped(), 1);
        assert_ne!(t.generation(), generation);
        // Merged folds keep both counts.
        let mut whole = TranscriptUsage::new();
        whole.absorb(&t);
        whole.absorb(&t);
        assert_eq!((whole.unstamped(), whole.series().sum()), (2, (2220, 20)));
    }

    /// COLUMNS SPREAD A BUCKET OVER THE CELLS ITS SPAN COVERS, in proportion
    /// and rounded so every token lands in one column: a minute over three
    /// columns is a third each, a window that ends inside a bucket (now,
    /// mid-minute) holds that bucket's every token, and every width sums to
    /// the series.
    #[test]
    fn columns_spread_and_keep_the_sum() {
        let start = unix("2026-10-19T12:00:00Z");
        let mut series = TokenSeries::default();
        series.note(start + 5, 60, 6);
        series.note(start + 65, 120, 0);
        series.note(start + 185, 1, 7);
        assert_eq!(series.columns(start, start + 60, 3), [(20, 2); 3]);
        assert_eq!(series.columns(start, start + 30, 1), [(60, 6)]);
        assert_eq!(
            series.columns(start, start + 90, 3),
            [(30, 3), (30, 3), (120, 0)],
            "a window ending mid-minute holds the minute"
        );
        let whole = series.sum();
        for n in 1..=97 {
            let columns = series.columns(start - 17, start + 240 + 13, n);
            let sum = columns
                .iter()
                .fold((0, 0), |(i, o), &(ci, co)| (i + ci, o + co));
            assert_eq!(sum, whole, "{n} columns");
        }
        // A token is never split into nothing: one over three columns is
        // in exactly one.
        let lone = series.columns(start + 180, start + 240, 3);
        assert_eq!(lone.iter().map(|c| c.0).sum::<u64>(), 1);
        assert_eq!(lone.iter().map(|c| c.1).sum::<u64>(), 7);
        assert!(series.columns(start, start + 60, 0).is_empty());
        assert_eq!(series.columns(start + 60, start, 2), [(0, 0); 2]);
        assert_eq!(
            TokenSeries::default().columns(start, start + 60, 2),
            [(0, 0); 2]
        );
        // A flat view of the series spreads as the series does.
        let view = crate::harness::session_usage::TrafficView {
            start_s: series.start_s(),
            width_s: series.width_s(),
            buckets: series.buckets().iter().copied().collect(),
            ..Default::default()
        };
        for n in [1, 3, 7, 60] {
            assert_eq!(
                view.columns(start - 17, start + 253, n),
                series.columns(start - 17, start + 253, n),
                "{n} columns"
            );
        }
        assert_eq!(
            crate::harness::session_usage::TrafficView::default().columns(start, start + 60, 2),
            [(0, 0); 2]
        );
    }

    /// Working time is the union of the main chain's `turn_duration` turns: a
    /// turn written late over the turns after it (a workflow's) counts once, a
    /// sidechain's row not at all, and a row past the raw-byte cut is still
    /// parsed.
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

    /// A subagent message streamed over three rows (`stop_reason` null with
    /// the message_start output twice, then the final row) counts its final
    /// output, 349, once.
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

    /// Which window a notice names: the screen's wall classes, narrowed to a
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

    /// The one-hour writes are kept apart: 800 of the row's 1000 cache writes
    /// are one-hour ones (`cache_creation.ephemeral_1h_input_tokens`, on Claude
    /// Code 2.1.30x transcripts), which list higher than five-minute ones. A
    /// split larger than the total is held to it, and a split with no total sums
    /// to one. Negative control: a row with no split reads every write as a
    /// five-minute one.
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
        // Negative control.
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
        assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
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

    /// A usage tap reading fills the JSON's `windows` (schema 1) by the
    /// vendor's name, with the reading's age. Negative control: a window the
    /// reading lacks is absent, and no reading leaves `windows` empty.
    #[test]
    fn a_tap_reading_fills_the_windows_of_the_json() {
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
                pct_x10: None,
            }),
            seven_day: None,
            warm: None,
            cache_expires_at: None,
            version: None,
            model: None,
            dur_us: 0,
        };
        let mut a = live_account();
        a.add_reading(&reading, 1_012);
        let mut view = UsageView::new(1_012);
        view.accounts.push(a);
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

    // -- hud_line -----------------------------------------------------------

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

        // `U+2028` is no `char::is_control()` (it is Zl), and is flattened too.
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

    // -- the accounts probe's read: the Settings panel, the settled rule ------

    use aterm_phase::prompt::fixtures as fx;

    /// The one whole-screen capture of the panel on Claude Code 2.1.278 (the
    /// vendor corpus's manifest records its digest): 46 rows at 56 columns,
    /// the `▔` rule on row 3, the header on row 4, three windows in the narrow
    /// layout, and the vendor's `↓` ending row 46 — the tail is under the fold.
    const SCREEN_2_1_278: &str =
        include_str!("../../tests/testdata/vendor/2.1.278/screen/usage.txt");

    /// The cancel hint as the panel draws it (three columns in), from the
    /// anchor table.
    fn hint() -> String {
        format!("   {}", aterm_phase::anchor("prompt.cancel"))
    }

    /// The retry hint beside the cancel hint, as the 2.1.287 bundle composes
    /// the row beside an enrich error or a failed load (read from the bundle,
    /// not measured: the `settings:retry` key and word, the byline's ` · `).
    fn retry_hints() -> String {
        format!("   r to retry · {}", aterm_phase::anchor("prompt.cancel"))
    }

    /// Measured on 2.1.287 (`aterm_phase::prompt::fixtures::USAGE_SETTLED`,
    /// 150x50, half a second after the Enter): the Usage tab settled and whole
    /// on the grid — the session at 1 %, the week at 10 %, the Fable window at
    /// 0 %, the cancel hint last.
    fn settled() -> Vec<String> {
        fx::screen(fx::USAGE_SETTLED)
    }

    /// The index of the last non-blank row.
    fn last_row(rows: &[String]) -> usize {
        rows.iter()
            .rposition(|r| !r.trim().is_empty())
            .expect("a drawn row")
    }

    /// Composed, for the states no capture holds: the measured settled screen
    /// ([`settled`]) with `between` drawn right above its hint row (each with
    /// the blank row the tab's `gap` puts under it) and the hint row replaced
    /// by `tail`. The place is the 2.1.287 bundle's: the enrich-error line and
    /// `Refreshing…` come after the usage-credits block and before the hint
    /// row, and the measured refreshing screen agrees.
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

    /// The measured screens whose tail is under the fold (2.1.278's capture;
    /// 2.1.287 at 150x50 once the contributing block filled in, and at 110x32)
    /// are the fullscreen Settings panel with the header on row 4, and read
    /// `Fold` although the windows are on the grid. Negative controls: the
    /// older fixtures, and the 2.1.278 capture with a composer frame under it
    /// or its header a column out or in, are no panel.
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

    /// The header guard, compiled by the server's own row matcher
    /// (`aterm_observe::row_matcher`, as `key if=` and `await gone` use it),
    /// matches each measured screen's header row and no other. Negative
    /// control: a quoted copy of the header is not matched.
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

    /// Measured on 2.1.287: the whole tab on the grid, nothing unsettled, the
    /// hint row last is `Settled` (150x50 while scanning, 150x80, and 60x50
    /// narrow). Negative control: the same run a quarter second earlier shows
    /// `Refreshing…` and a stale week (9 % against 10 %) and is `Unsettled`.
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
        // Composed: the refresh line on the settled screen is unsettled too,
        // so the verdict is the row's, not that capture's.
        assert_eq!(
            read(&composed(&["   Refreshing…"], &hint())),
            PanelRead::Unsettled
        );
        assert_eq!(read(&composed(&[], &hint())), read(&settled()));
    }

    /// Composed ([`composed`]): each enrich-error line the 2.1.287 bundle
    /// draws is `Unsettled`, with the retry hint beside the cancel hint or
    /// without. Negative control: the retry hint alone is no tail, so that
    /// panel is `Fold`.
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

    /// Composed: the measured head of the tab with the bundle's no-figures
    /// state (`Loading usage data…`) or a failed load (`Error: <why>` over a
    /// retry hint) in the plan-limits component's place is the Usage tab and
    /// `Unsettled`. Negative control: with that line taken out it is still
    /// `Unsettled` (no window), never `Settled`.
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

    /// The settled screen with a `↓` ending its hint row (composed), or cut
    /// anywhere above its hint row, is `Fold`. Negative control: the same rows
    /// whole are `Settled`.
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

    /// Composed from the settled screen: the session alone, the session and
    /// Fable without the week, and the week without the session are each
    /// `Unsettled` under the hint row. Negative control: both windows and no
    /// third is `Settled`.
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

    /// Composed: the measured rule and header over another tab's rows
    /// (invented: the Status tab names the login, so no capture is committed)
    /// is `NotUsage`, hint row or none. Negative control: one `Total cost:`
    /// row under it makes it the Usage tab (`Unsettled`: no window).
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

    /// Measured on 2.1.287 under the inline renderer: the panel under its `─`
    /// rule reads `Inline` (which the probe refuses to type under) with the
    /// same figures, and once the main screen scrolled its rule and header
    /// away there is no panel. Negative control: the fullscreen capture reads
    /// `Fullscreen`.
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

    /// 9:20 PM on Oct 1 in the panel's zone (04:20 UTC on Oct 2): the
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

    /// The settled panel's sample places each reset in the zone the panel
    /// names, a bare clock time (`2am`) and a dated one (`Oct 8 at 9am`), not
    /// at the machine's offset (here nine hours east). Negative control: a
    /// zone table that knows nothing places the same text at the machine's
    /// offset, another instant.
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

    /// A reset further ahead than its window can be (the session's five hours
    /// plus the stamp's skew, the week's eight days), one the grammar cannot
    /// read, and none at all are `None`, the figure kept. Negative controls:
    /// resets inside each bound are kept.
    #[test]
    fn a_reset_beyond_its_windows_bound_is_dropped_and_its_figure_kept() {
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

    /// A figure nobody read is `None`, never `0`: a lone window gives only its
    /// own figure, and windows that are neither (the Fable bucket, the spend
    /// limit), or none, give no sample. Negative control: both windows give
    /// both.
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
