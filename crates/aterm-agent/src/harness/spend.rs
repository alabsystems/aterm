// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOLLAR FIGURES of the account stats (`docs/DESIGN-account-stats-2026-10-15.md`
//! §3.6): every message an account's agent was served, its tokens class by
//! class, priced at the vendor's public LIST price — `≈$ (list)`, a
//! normalization, never a bill — and, beside a window's reading, what a full
//! window holds (`C ≥ $ ÷ used`) and what is left (`≈ C × (1 − used)`).
//!
//! * **The table** is [`PRICES_TOML`], a dated file of its own whose rows a
//!   person read off the vendors' public pricing pages, each naming its page.
//!   The caller loads it ([`PriceTable::from_toml`]) and hands it to every
//!   function here: nothing in this module knows a price. A model id with no
//!   row is UNPRICED — its tokens are counted and shown, never priced at a
//!   guess (Claude Code prices an unknown model at a default rate; this does
//!   not).
//! * **The fold** ([`Spend::fold_claude_home`]) reads EVERY transcript under a
//!   Claude Code home's `projects/` modified in the last [`HORIZON_S`],
//!   subagent and workflow transcripts included (they carry nearly all the
//!   dollars: the design's F15). Each message counts once in the HOME, by its
//!   `message.id`: at the largest counts its rows carry within its file (a
//!   streamed message repeats its id over rows whose output grows), and not
//!   again when another transcript repeats it (a copy a fork or a resume
//!   could write; none was seen — 0 of 15,717 ids over 8 days on the machine
//!   this was built on, F15 — but a copy would double a home-wide sum). The
//!   ids counted are kept, by the hour they were served, for the
//!   [`HORIZON_S`]. Each message is priced on its own
//!   ([`PriceTable::message_usd`]: a model priced by prompt length is decided
//!   request by request) and placed in the [`BUCKET_S`]-wide bucket of its
//!   time, back to [`HORIZON_S`]; older ones are dropped ([`Spend::expire`]),
//!   never merged into wider ones. Fast mode, US-only inference, web search,
//!   a non-standard tier and an advisor's iterations are priced at the base
//!   rates (an advisor's own tokens not at all) and counted as `approx`
//!   ([`Tally::approx`]), a message once, when ANY row of it says so: Claude
//!   Code writes `server_tool_use` on most streamed messages only with the
//!   final usage, and on 2.1.300 often `speed` too. Claude Code's own empty
//!   usage, which it writes where the server named nothing, is standard.
//! * **The spend** ([`Spend`]) holds the buckets and, per transcript, a
//!   byte cursor — its identity, the offset folded to, its size and mtime
//!   when read, the byte before the offset, and the newest message ids of a
//!   file still being written — and the home's counted ids, so a second fold
//!   of the same spend reads only the bytes appended since and an unchanged
//!   file is not opened. It lives in the caller's memory: `accounts stats`
//!   folds the home at each run and saves nothing (the spend file the design
//!   names is the recorder's, A2). A spend priced by another table, or for
//!   another home, is folded again from the transcripts.
//! * **A transcript that changed other than by an append** (shrunk, replaced,
//!   the byte before the offset changed) is folded again from its start: the
//!   messages it already counted are known by their ids and not counted
//!   again ([`FoldReport::refolded`]).
//! * **Capacity** ([`figure`]): `C = $(window start, t_r] ÷ u(t_r)`, summed
//!   from the window's own five-minute buckets: the numerator stops at the
//!   reading (whole buckets only, so later spend never inflates it) and
//!   starts at a reset used inside the window. `C` is a lower bound (`≥`):
//!   spend no machine here sees only makes it low. `left = C × (1 − u_now)`
//!   is an estimate (`≈`), `stale` when `u_now` is older than a tenth of its
//!   window or a machine that used the account did not answer. Both are
//!   `on-credits` when usage credits served any message in the window: no
//!   share of the window's spend is split off by time. The error bar is the
//!   reading's resolution — whole until a tenth is seen, a painted panel
//!   floored; the spread of closed windows (the calibration) waits for the
//!   series' `close` rows (A2), so every capacity is uncalibrated. Every
//!   empty figure carries one closed word ([`Why`]).
//!
//! Nothing here is set against the vendor's own cost. Claude Code's process
//! cost (the status line's `cost.total_cost_usd`) also counts model calls its
//! transcripts leave out: the prompt-suggestion and compaction forks, which
//! write no transcript, side queries such as the session title, and an
//! advisor's iterations. The table is checked against the managed build's own
//! catalog in the lab (`targo --unverified lab prices`), never at run time.
//!
//! **A Codex conversation** ([`fold_conversation`]): what `harness usage`
//! shows from a Codex session's own tools, its turns summed by model from
//! the Codex reader (`harness::codex_rollout::TurnReader`), each mapped into
//! Anthropic's classes as [`codex_message`] says. Codex has no capacity
//! figure (§3.6).
//!
//! This is a LEAF (§8.2): it depends on `usage`, `transcript` and the Codex
//! reader (`harness::codex_rollout`, which imports nothing of it), and
//! nothing that decides anything imports it. Its API is report functions; there is no "best" or
//! "next" accessor.
//!
//! STATUS: unit-tested against composed transcripts in temp homes (never a
//! real one). The usage keys read (`cache_creation.ephemeral_1h_input_tokens`,
//! `speed`, `inference_geo`, `service_tier`, `server_tool_use`, `iterations`)
//! were MEASURED by name on Claude Code 2.1.30x transcripts. Over 8 days on
//! the machine this was built on, 26,531 of 41,794 assistant rows carried
//! `iterations` and every entry was `type: "message"`: no advisor ran. Fast
//! mode's and US-only inference's own words were not seen either, so any word
//! but the standard ones counts as approximate. Claude Code's empty usage was
//! READ in 2.1.301 (`speed:"standard"`, `service_tier:"standard"`,
//! `inference_geo:""`, `iterations:[]`, no search) and MEASURED in the lab,
//! on a stub's reply that named none (the design's §9, A5). The advisor's
//! price was READ there too: each `iterations[]` entry of type
//! `advisor_message`, priced at that entry's model and added on top of the
//! message's own usage. A `fallback_message` entry (a server-side fallback
//! served the message; seen in this machine's transcripts before the 8-day
//! horizon) counts as approximate too, as any entry but a plain `message`
//! does.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use aterm_json::{Map, Value};

use super::usage::{
    MAX_ID_BYTES, MAX_LINE_BYTES, MAX_MODELS, OTHER_MODEL, SEEN_IDS_MAX, TokenUsage, UNKNOWN_MODEL,
};
use crate::harness::codex_rollout::{Row, Turn, TurnReader};

/// The shipped price table, `prices.toml` beside this file, for the caller
/// to load ([`PriceTable::from_toml`]) and hand in.
pub const PRICES_TOML: &str = include_str!("prices.toml");

/// How far back spend is kept: a week's window and a day more.
pub const HORIZON_S: i64 = 8 * 24 * 3600;

/// The width of every bucket, back to the horizon: a window's capacity is
/// summed from these, never from a wider sum that would hold spend from
/// before its start or after its reading.
pub const BUCKET_S: i64 = 5 * 60;

/// The counted ids are kept by the hour their message was served.
const HOUR_S: i64 = 3600;

/// The most transcripts one spend follows; past it a new one is counted
/// unread.
pub const MAX_FILES: usize = 16_384;

/// The most directory entries one walk of `projects/` looks at; a walk cut
/// short makes the fold incomplete.
pub const MAX_WALK_ENTRIES: usize = 262_144;

/// How deep under `projects/` the walk goes (`<slug>/<session>/subagents/
/// workflows/<run>/agent-*.jsonl` is five directories down).
pub const MAX_DEPTH: usize = 8;

/// The message ids kept for a transcript read to its end that is still being
/// written: a message still streaming when a fold stops is raised, not
/// counted again, by the next. A transcript not yet read to its end keeps up
/// to [`SEEN_IDS_MAX`].
pub const IDS_KEPT: usize = 16;

/// A transcript unchanged this long keeps no message ids of its own (the
/// home's counted ids still know them).
pub const QUIET_S: i64 = 10 * 60;

/// The most counted message ids the home keeps (16 bytes each); past it the oldest hours are forgotten first, and a later copy of
/// one of their messages would count again. A busy home serves a few
/// thousand messages a day.
pub const MAX_SEEN: usize = 400_000;

/// `unpriced` and `unattributed` suppress a capacity past this share.
pub const SHARE_MAX: f64 = 0.01;

/// At most every retained bucket of one message. A model change or aggregate
/// overflow makes the whole message unpriced. Reaching this bound also retires
/// its tracked price; it never drops provenance while keeping USD.
const MAX_CONTRIBUTIONS: usize = (HORIZON_S / BUCKET_S) as usize + 1;

/// How much one read call asks the kernel for.
const READ_CHUNK: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// The price table — read from its file, handed in, never built in
// ---------------------------------------------------------------------------

/// Dollars per million tokens in each token class. `input` and `output`
/// always have a price; a cache class the vendor's page lists none for is
/// `None`, and a request with tokens in it cannot be priced.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rates {
    /// Per million input tokens.
    pub input: f64,
    /// Per million output tokens.
    pub output: f64,
    /// Per million cache writes (Anthropic's five-minute ones).
    pub cache_write: Option<f64>,
    /// Per million one-hour cache writes.
    pub cache_write_1h: Option<f64>,
    /// Per million cache reads.
    pub cache_read: Option<f64>,
}

impl Rates {
    fn is_valid(&self) -> bool {
        [
            Some(self.input),
            Some(self.output),
            self.cache_write,
            self.cache_write_1h,
            self.cache_read,
        ]
        .into_iter()
        .flatten()
        .all(|p| p.is_finite() && p >= 0.0)
    }

    /// Dollars for `t` at these rates, or `None` when a class with tokens has
    /// no rate. Five-minute writes are `cache_write − cache_write_1h`.
    fn usd(&self, t: &TokenUsage) -> Option<f64> {
        let per = |tokens: u64, price: Option<f64>| match (tokens, price) {
            (0, _) => Some(0.0),
            (n, Some(p)) => Some(n as f64 * p / 1_000_000.0),
            (_, None) => None,
        };
        let one_hour = t.cache_write_1h.min(t.cache_write);
        Some(
            per(t.input, Some(self.input))?
                + per(t.output, Some(self.output))?
                + per(t.cache_write - one_hour, self.cache_write)?
                + per(one_hour, self.cache_write_1h)?
                + per(t.cache_read, self.cache_read)?,
        )
    }
}

/// The price of one model: its rates, and the higher rates a request whose
/// prompt ([`TokenUsage::prompt`]) passes a length pays, where the vendor
/// prices by prompt length.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelPrice {
    /// The rates of every request, or of those at or under [`Self::long`]'s
    /// length.
    pub rates: Rates,
    /// A request whose prompt is over this many tokens pays these rates.
    pub long: Option<(u64, Rates)>,
}

impl ModelPrice {
    /// One set of rates for every request.
    #[must_use]
    pub fn flat(rates: Rates) -> Self {
        Self { rates, long: None }
    }

    fn is_valid(&self) -> bool {
        self.rates.is_valid() && self.long.is_none_or(|(_, r)| r.is_valid())
    }
}

/// The most rows a price table may hold.
pub const MAX_PRICE_ROWS: usize = 512;

/// Prices by exact model id. Empty unless the caller fills it: no code knows
/// what any model costs, and a model with no row prices as `None`. The
/// shipped table is a file of its own ([`PRICES_TOML`]) the caller reads in
/// ([`Self::from_toml`]) and hands to every fold and figure here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PriceTable {
    rows: BTreeMap<String, ModelPrice>,
    /// The page each row was read from, by model id.
    sources: BTreeMap<String, String>,
    /// The day the table was read (`YYYY-MM-DD`), when it says.
    as_of: Option<String>,
}

impl PriceTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the price of one model id (exact match, no prefix or family rule).
    #[cfg(test)]
    pub(crate) fn insert(&mut self, model: impl Into<String>, price: ModelPrice) {
        self.rows.insert(model.into(), price);
    }

    /// Builder form of [`insert`](Self::insert).
    #[cfg(test)]
    pub(crate) fn with(mut self, model: impl Into<String>, price: ModelPrice) -> Self {
        self.insert(model, price);
        self
    }

    /// A table from its file (`prices.toml`'s grammar, format 1): `as_of`, a
    /// `YYYY-MM-DD` day; and under `[models."<id>"]` a row of dollars per
    /// million tokens — `input` and `output` required, `cache_write`,
    /// `cache_write_1h` and `cache_read` where the page prices them, `source`
    /// and `id_from` naming the pages — with an optional `long` table
    /// (`over`, then the same classes). STRICT: an unknown key, a missing
    /// price or source, a negative or non-finite price, or more than
    /// [`MAX_PRICE_ROWS`] rows refuses the whole table, so a typo cannot drop
    /// a class and price it at nothing.
    ///
    /// # Errors
    ///
    /// What is wrong, and where.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        use aterm_toml::Value as T;
        let doc = aterm_toml::from_str::<T>(text).map_err(|e| format!("not TOML: {e}"))?;
        let top = doc.as_table().ok_or("not a table")?;
        for key in top.keys() {
            if !["format", "as_of", "models"].contains(&key.as_str()) {
                return Err(format!("unknown key `{key}`"));
            }
        }
        if top.get("format").and_then(T::as_integer) != Some(1) {
            return Err("`format` is not 1".to_owned());
        }
        let as_of = top
            .get("as_of")
            .and_then(T::as_str)
            .filter(|d| is_day(d))
            .ok_or("`as_of` is not a YYYY-MM-DD day")?;
        let models = top
            .get("models")
            .and_then(T::as_table)
            .ok_or("no `models` table")?;
        if models.len() > MAX_PRICE_ROWS {
            return Err(format!("more than {MAX_PRICE_ROWS} rows"));
        }
        let mut table = Self {
            as_of: Some(as_of.to_owned()),
            ..Self::default()
        };
        for (id, row) in models.iter() {
            if !super::upgrade::is_model_id(id) {
                return Err(format!("`{id}` is not a model id"));
            }
            let row = row.as_table().ok_or_else(|| format!("{id}: not a table"))?;
            let at = |what: &str| format!("{id}: {what}");
            for key in row.keys() {
                if !RATE_KEYS.contains(&key.as_str())
                    && !["source", "id_from", "long"].contains(&key.as_str())
                {
                    return Err(at(&format!("unknown key `{key}`")));
                }
            }
            let source = row
                .get("source")
                .and_then(T::as_str)
                .filter(|s| s.starts_with("https://"))
                .ok_or_else(|| at("no `source` naming an https page"))?;
            if !row
                .get("id_from")
                .and_then(T::as_str)
                .is_some_and(|s| s.starts_with("https://"))
            {
                return Err(at("no `id_from` naming an https page"));
            }
            let rates = rates_of(row).map_err(|e| at(&e))?;
            let long = match row.get("long") {
                None => None,
                Some(long) => {
                    let long = long.as_table().ok_or_else(|| at("`long` is not a table"))?;
                    for key in long.keys() {
                        if !RATE_KEYS.contains(&key.as_str()) && key != "over" {
                            return Err(at(&format!("unknown key `long.{key}`")));
                        }
                    }
                    let over = long
                        .get("over")
                        .and_then(T::as_integer)
                        .and_then(|n| u64::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or_else(|| at("`long.over` is not a positive count"))?;
                    Some((over, rates_of(long).map_err(|e| at(&format!("long: {e}")))?))
                }
            };
            let price = ModelPrice { rates, long };
            if !price.is_valid() {
                return Err(at("a price is negative or not finite"));
            }
            table.rows.insert(id.clone(), price);
            table.sources.insert(id.clone(), source.to_owned());
        }
        Ok(table)
    }

    /// The day the table was read, when it says.
    #[must_use]
    pub fn as_of(&self) -> Option<&str> {
        self.as_of.as_deref()
    }

    /// The page a model's row was read from.
    #[must_use]
    pub fn source(&self, model: &str) -> Option<&str> {
        self.sources.get(model).map(String::as_str)
    }

    /// The model ids the table prices, in order.
    pub fn models(&self) -> impl Iterator<Item = &str> {
        self.rows.keys().map(String::as_str)
    }

    /// The price row for a model id, if the caller supplied one.
    pub fn get(&self, model: &str) -> Option<&ModelPrice> {
        self.rows.get(model)
    }

    /// Dollars for ONE request of `model` with the counts `t`, at the rates
    /// its prompt length picks; `None` when the model has no row, a class
    /// with tokens has no rate, or the row holds a negative or non-finite
    /// price. Never a guess.
    #[must_use]
    pub fn message_usd(&self, model: &str, t: &TokenUsage) -> Option<f64> {
        let p = self.get(model).filter(|p| p.is_valid())?;
        let rates = match p.long {
            Some((over, long)) if t.prompt() > over => long,
            _ => p.rates,
        };
        rates.usd(t)
    }
}

/// The token classes a price row names.
const RATE_KEYS: [&str; 5] = [
    "input",
    "output",
    "cache_write",
    "cache_write_1h",
    "cache_read",
];

/// One set of rates from a row's table: `input` and `output` required.
fn rates_of(row: &aterm_toml::Table) -> Result<Rates, String> {
    let price = |key: &str| -> Result<Option<f64>, String> {
        match row.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_float()
                .or_else(|| v.as_integer().map(|n| n as f64))
                .map(Some)
                .ok_or_else(|| format!("`{key}` is not a number")),
        }
    };
    Ok(Rates {
        input: price("input")?.ok_or("no `input` price")?,
        output: price("output")?.ok_or("no `output` price")?,
        cache_write: price("cache_write")?,
        cache_write_1h: price("cache_write_1h")?,
        cache_read: price("cache_read")?,
    })
}

/// `YYYY-MM-DD`, digits in place (the calendar is not checked).
fn is_day(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

// ---------------------------------------------------------------------------
// What is counted
// ---------------------------------------------------------------------------

/// One served message as a fold yields it: when it was served, its model, its
/// tokens, and whether the table cannot price it exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message<'a> {
    /// Unix seconds it was served.
    pub at: i64,
    /// Its model id, exactly as the vendor wrote it.
    pub model: &'a str,
    /// Its token counts in Anthropic's classes, where `input` is the
    /// UNCACHED input only. A Codex turn's `input_tokens` include its
    /// `cached_input_tokens`, so it maps as `input = input_tokens −
    /// cached_input_tokens`, `cache_read = cached_input_tokens` and `output =
    /// output_tokens` (reasoning tokens are already in it) — and a cache
    /// write as a part too ([`codex_message`]). The prompt a `long` row is
    /// decided on ([`TokenUsage::prompt`]) is then OpenAI's `input_tokens`;
    /// `input = input_tokens` would price every cached token twice.
    pub tokens: TokenUsage,
    /// Priced at the base rates though the vendor's were higher or lower
    /// ([`Tally::approx`]).
    pub approx: bool,
}

/// A Codex turn as one [`Message`]: `input = input_tokens −
/// cached_input_tokens − cache_write_input_tokens`, `cache_read =
/// cached_input_tokens`, `cache_write = cache_write_input_tokens` (one
/// lifetime: OpenAI lists one cache-write price) and `output =
/// output_tokens`; filed under [`UNKNOWN_MODEL`] where no `turn_context`
/// named a model. APPROXIMATE when its turn named a service tier (`fast`,
/// `flex`), when it wrote to the cache (whether `input_tokens` holds the
/// writes is UNKNOWN: none was ever seen), or when its parts exceed its
/// input.
#[must_use]
pub fn codex_message(turn: &Turn) -> Message<'_> {
    let t = &turn.tokens;
    let parts = t.cached.saturating_add(t.cache_write);
    Message {
        at: turn.at,
        model: turn.model.as_deref().unwrap_or(UNKNOWN_MODEL),
        tokens: TokenUsage {
            input: t.input.saturating_sub(parts),
            output: t.output,
            cache_write: t.cache_write,
            cache_write_1h: 0,
            cache_read: t.cached,
        },
        approx: turn.tier || t.cache_write > 0 || parts > t.input,
    }
}

/// One model's turns in a [`ConversationSpend`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelTurns {
    /// Their tokens, in Anthropic's classes ([`codex_message`]).
    pub tokens: TokenUsage,
    /// Turns counted.
    pub turns: u64,
    /// List dollars of the turns the table priced.
    pub usd: f64,
    /// Some turn of it could not be priced (no row for the model, or tokens
    /// in a class its row has no rate for): `usd` is then not its whole.
    pub unpriced: bool,
    /// Turns priced at the base rates ([`codex_message`]'s approximate).
    pub approx: u64,
}

/// Every served turn of one Codex conversation, by model, at list price:
/// what `harness usage` shows from a Codex session's own tools. No horizon:
/// a conversation's turns are summed whenever they were served.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConversationSpend {
    /// By model id ([`UNKNOWN_MODEL`] where no `turn_context` named one).
    pub models: BTreeMap<String, ModelTurns>,
    /// Rollouts read.
    pub files: usize,
    /// Rollouts that could not be opened.
    pub unread: usize,
    /// Bytes read.
    pub read: u64,
    /// Every rollout was read to its end, within the cap.
    pub caught_up: bool,
    /// Turns counted.
    pub turns: u64,
    /// `token_count`s whose running total did not grow: written again.
    pub repeats: u64,
    /// `token_count`s inside a fork's copied prefix.
    pub copied: u64,
    /// Turns with no readable stamp: not counted (as the home's fold places
    /// them in no window).
    pub untimed: u64,
    /// Lines that were not JSON, or a `token_count` that could not be read.
    pub unparsed: u64,
    /// Lines longer than [`MAX_LINE_BYTES`], skipped.
    pub long: u64,
    /// The model of `own`'s latest `turn_context` past any copied prefix.
    pub model: Option<String>,
}

/// Sum every served turn of the rollouts `files` (a conversation's, the
/// named thread's own among them as `own`), each read once from its start,
/// at most `cap` bytes in all, priced by `prices`. Two threads served the
/// same counts are two turns: a fingerprint names a turn only within its
/// rollout.
#[must_use]
pub fn fold_conversation(
    files: &[PathBuf],
    own: Option<&Path>,
    prices: &PriceTable,
    cap: u64,
) -> ConversationSpend {
    let mut out = ConversationSpend {
        caught_up: true,
        ..ConversationSpend::default()
    };
    for path in files {
        let mut reader = TurnReader::default();
        let left = cap.saturating_sub(out.read);
        let mut each = |row: Row| match row {
            Row::Turn(turn) => {
                let message = codex_message(&turn);
                let usd = prices.message_usd(message.model, &message.tokens);
                let m = out.models.entry(message.model.to_owned()).or_default();
                m.tokens = m.tokens.plus(&message.tokens);
                m.turns = m.turns.saturating_add(1);
                m.usd += usd.unwrap_or(0.0);
                m.unpriced |= usd.is_none();
                m.approx = m.approx.saturating_add(u64::from(message.approx));
                out.turns = out.turns.saturating_add(1);
            }
            Row::Repeat => out.repeats = out.repeats.saturating_add(1),
            Row::Copied => out.copied = out.copied.saturating_add(1),
            Row::Untimed => out.untimed = out.untimed.saturating_add(1),
            Row::Unparsed => out.unparsed = out.unparsed.saturating_add(1),
            Row::Other => {}
        };
        let read = crate::harness::codex_rollout::read_rollout(path, &mut reader, left, &mut each);
        let Some(read) = read else {
            out.unread += 1;
            continue;
        };
        out.files += 1;
        out.read = out.read.saturating_add(read.read);
        out.long = out.long.saturating_add(read.long);
        out.caught_up &= read.complete;
        if own == Some(path.as_path()) {
            out.model = reader.model;
        }
    }
    out
}

/// What a bucket holds for one model.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tally {
    /// Messages counted.
    pub messages: u64,
    /// Every counted message's tokens, priced or not.
    pub tokens: TokenUsage,
    /// List dollars of the messages the table priced.
    pub usd: f64,
    /// The tokens of the messages it could not price: no row for the model,
    /// or tokens in a class the row has no rate for.
    pub unpriced: TokenUsage,
    /// Messages priced at the base rates though fast mode, US-only inference,
    /// web search, a tier other than the standard one or an advisor's
    /// iterations changed what the vendor lists, each in the bucket of the
    /// first row of it that said so.
    pub approx: u64,
}

impl Tally {
    fn absorb(&mut self, other: &Tally) {
        self.messages = self.messages.saturating_add(other.messages);
        self.tokens = self.tokens.plus(&other.tokens);
        self.usd += other.usd;
        self.unpriced = self.unpriced.plus(&other.unpriced);
        self.approx = self.approx.saturating_add(other.approx);
    }
}

/// Whether a row's usage asks for something the table does not price: any
/// `speed` but `standard` (fast mode), any `inference_geo` but `global` or
/// `not_available` (US-only inference lists at 1.1×), any `service_tier` but
/// `standard`, a web search, or any `iterations` entry but a plain
/// `message`: an advisor's (`advisor_message`, which Claude Code prices at
/// the advisor's model on top of the message's own usage; its tokens are not
/// in that usage, so the fold does not count them) or a server-side
/// fallback's (`fallback_message`: the declined primary attempt and the
/// fallback model's, two models in one message). Claude Code's own empty
/// usage, written where the server named none (as the lab's stub and a
/// gateway leave it), is standard: `speed:"standard"`,
/// `service_tier:"standard"`, `inference_geo:""`, `iterations:[]`, no search.
fn approx(usage: &Map) -> bool {
    let word = |key: &str| usage.get(key).and_then(Value::as_str);
    word("speed").is_some_and(|w| w != "standard")
        || word("inference_geo")
            .is_some_and(|w| !w.is_empty() && w != "global" && w != "not_available")
        || word("service_tier").is_some_and(|w| w != "standard")
        || usage
            .get("server_tool_use")
            .and_then(|s| s.get("web_search_requests"))
            .and_then(Value::as_f64)
            .is_some_and(|n| n > 0.0)
        || usage
            .get("iterations")
            .and_then(Value::as_array)
            .is_some_and(|all| {
                all.iter()
                    .any(|i| i.get("type").and_then(Value::as_str) != Some("message"))
            })
}

/// The catalog family of a model id (`claude-fable-5-1` → `fable`), the one
/// rule `upgrade_models` reads ids by.
fn family_of(model: &str) -> Option<String> {
    super::upgrade_models::family_version(model).map(|(family, _)| family)
}

// ---------------------------------------------------------------------------
// The cursor of one transcript
// ---------------------------------------------------------------------------

/// How far one transcript has been folded.
#[derive(Debug, Clone, PartialEq, Default)]
struct Cursor {
    /// The file folded (device, inode); `None` before its first read.
    identity: Option<(u64, u64)>,
    /// Bytes folded: every complete line, and a long line's bytes skipped. A
    /// torn last line is read again next time.
    offset: u64,
    /// The byte at `offset - 1` when it was read.
    boundary: Option<u8>,
    /// Its size and mtime (ns) at the last read.
    size: u64,
    mtime_ns: i64,
    /// The last read reached the end the file had.
    end: bool,
    /// Inside a line longer than [`MAX_LINE_BYTES`]: skipped to its newline.
    skipping: bool,
    /// The newest messages it counted, oldest first ([`Cursor::let_go`]).
    ids: VecDeque<Held>,
}

/// A message a transcript's cursor holds: what its later rows are folded
/// against.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    /// FNV-1a/64 of its `message.id`.
    key: u64,
    /// The largest counts its rows carried.
    tokens: TokenUsage,
    /// A row of it was [`approx`], and the message was counted so.
    approx: bool,
    /// Sorted (bucket, actual aggregate model key) contributions. None means
    /// tracking overflowed or the message changed model: it stays unpriced.
    contributions: Option<Vec<Contribution>>,
}

#[derive(Debug, Clone, PartialEq)]
struct Contribution {
    bucket: i64,
    model: String,
    tokens: TokenUsage,
    usd: f64,
    unpriced: TokenUsage,
}

impl Held {
    /// Invalidate only this message's actual earlier contributions. Its tokens
    /// and message count stay in their original buckets, as does unrelated spend.
    fn unprice(&mut self, buckets: &mut Buckets) {
        if let Some(parts) = self.contributions.as_mut() {
            for part in parts {
                if let Some(t) = buckets
                    .get_mut(&part.bucket)
                    .and_then(|m| m.get_mut(&part.model))
                {
                    t.usd = (t.usd - part.usd).max(0.0);
                    t.unpriced = t.unpriced.plus(&part.tokens.less(&part.unpriced));
                }
                part.usd = 0.0;
                part.unpriced = part.tokens;
            }
        }
    }

    /// A later, fully priced tier can resolve previously unpriced classes.
    /// The newly proved dollars are added at that later row, never backdated.
    fn price(&mut self, buckets: &mut Buckets) {
        if let Some(parts) = self.contributions.as_mut() {
            for part in parts {
                if let Some(t) = buckets
                    .get_mut(&part.bucket)
                    .and_then(|m| m.get_mut(&part.model))
                {
                    t.unpriced = t.unpriced.less(&part.unpriced);
                }
                part.unpriced = TokenUsage::default();
            }
        }
    }

    /// A table may also price a longer request lower. Remove the difference
    /// from its own latest credited buckets, never write a negative bucket.
    fn reduce_price(&mut self, buckets: &mut Buckets, mut amount: f64) {
        if let Some(parts) = self.contributions.as_mut() {
            for part in parts.iter_mut().rev() {
                let take = amount.min(part.usd);
                if let Some(t) = buckets
                    .get_mut(&part.bucket)
                    .and_then(|m| m.get_mut(&part.model))
                {
                    t.usd = (t.usd - take).max(0.0);
                }
                part.usd -= take;
                amount -= take;
                if amount <= 0.0 {
                    break;
                }
            }
        }
    }

    fn add(&mut self, buckets: &mut Buckets, at: i64, model: &str, mut t: Tally) {
        let bucket = at.div_euclid(BUCKET_S) * BUCKET_S;
        let actual = buckets.get(&bucket).map_or(model, |m| {
            if m.contains_key(model) || m.len() < MAX_MODELS {
                model
            } else {
                OTHER_MODEL
            }
        });
        if self.contributions.is_none() || actual == OTHER_MODEL {
            self.unprice(buckets);
            self.contributions = None;
            t.usd = 0.0;
            t.unpriced = t.tokens;
        }
        if let Some(parts) = self.contributions.as_mut() {
            let at =
                parts.binary_search_by(|p| (p.bucket, p.model.as_str()).cmp(&(bucket, actual)));
            match at {
                Ok(at) => {
                    let p = &mut parts[at];
                    p.tokens = p.tokens.plus(&t.tokens);
                    p.usd += t.usd;
                    p.unpriced = p.unpriced.plus(&t.unpriced);
                }
                Err(at) if parts.len() < MAX_CONTRIBUTIONS => parts.insert(
                    at,
                    Contribution {
                        bucket,
                        model: actual.to_owned(),
                        tokens: t.tokens,
                        usd: t.usd,
                        unpriced: t.unpriced,
                    },
                ),
                Err(_) => {
                    self.unprice(buckets);
                    self.contributions = None;
                    t.usd = 0.0;
                    t.unpriced = t.tokens;
                }
            }
        }
        add(buckets, bucket, model, t);
    }
}

impl Cursor {
    /// Start the file over from its first byte: it changed other than by an
    /// append. The ids it holds stand: a message still streaming is raised by
    /// its later rows.
    fn restart(&mut self) {
        *self = Self {
            ids: std::mem::take(&mut self.ids),
            ..Self::default()
        };
    }

    /// Let go of the oldest ids past `keep`.
    fn let_go(&mut self, keep: usize) {
        let past = self.ids.len().saturating_sub(keep);
        self.ids.drain(..past);
    }

    /// Whether the file `meta` describes is the one the last read left, read
    /// to its end: nothing to read.
    fn unchanged(&self, meta: &std::fs::Metadata) -> bool {
        self.end
            && identity_of(meta).is_none_or(|id| self.identity == Some(id))
            && self.size == meta.len()
            && self.mtime_ns == mtime_ns(meta)
    }
}

/// The identity `meta` carries, where the platform keeps one in a file's
/// metadata (Unix); `None` elsewhere, where only an open file tells it
/// ([`super::session_usage::open_identity`], read by [`advance`]).
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
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------------------
// The spend of one home
// ---------------------------------------------------------------------------

/// How much one fold may do: bytes read, and a deadline.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Bytes read, over every file.
    pub bytes: u64,
    /// Stop reading at this instant.
    pub until: Option<Instant>,
}

impl Limits {
    /// No bound.
    #[must_use]
    pub fn none() -> Self {
        Self {
            bytes: u64::MAX,
            until: None,
        }
    }

    fn over(&self) -> bool {
        self.until.is_some_and(|t| Instant::now() >= t)
    }
}

/// What one fold did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldReport {
    /// Transcripts modified inside the horizon.
    pub files: usize,
    /// Bytes read from disk by this fold.
    pub read: u64,
    /// Every followed transcript was read to the end it had (a torn last line
    /// aside): the sums are not a mid-read prefix.
    pub caught_up: bool,
    /// Files or directories which could not be inspected, and transcripts
    /// past [`MAX_FILES`] or named by a path that is not UTF-8.
    pub unread: usize,
    /// Transcripts that changed other than by an append, folded again from
    /// their start (what they had counted is not counted again).
    pub refolded: usize,
    /// The walk stopped at [`MAX_WALK_ENTRIES`], [`MAX_DEPTH`] or the
    /// fold's deadline.
    pub walk_cut: bool,
    /// Messages counted by this fold.
    pub messages: u64,
    /// Rows of a message already counted, folded away (or raised): a
    /// streamed message's later rows, and a message another transcript
    /// already counted. Codex: a `token_count` whose total did not grow, and
    /// a turn the home already counted.
    pub duplicates: u64,
    /// Lines longer than [`MAX_LINE_BYTES`], skipped.
    pub rows_long: u64,
    /// Lines that were not JSON or not UTF-8.
    pub rows_unparsed: u64,
    /// Assistant rows with usage but no readable `timestamp`: placed in no
    /// window, so not counted.
    pub rows_untimed: u64,
    /// Codex `token_count`s inside a fork's copied prefix: its parent's
    /// turns, counted for nothing. 0 for Claude Code.
    pub copied: u64,
}

impl FoldReport {
    /// Whether the sums are the home's whole spend in the horizon.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.caught_up && self.unread == 0 && !self.walk_cut
    }
}

/// A bucket's start (a multiple of [`BUCKET_S`]) → model → tally.
type Buckets = BTreeMap<i64, BTreeMap<String, Tally>>;

/// One Claude Code home's spend: its buckets, the cursors that say how far
/// each of its transcripts was folded, and the ids of the messages it
/// counted.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    /// The home folded (a path, written lossily).
    home: String,
    /// The price table's date: a spend priced by another is folded again.
    prices: String,
    /// Each transcript's cursor, by its path under `<home>/projects/`.
    files: BTreeMap<String, Cursor>,
    buckets: Buckets,
    /// Every message id counted in the horizon (FNV-1a/64 of `message.id`),
    /// with the hour its message was served: a message one transcript
    /// repeats from another counts once in the home.
    seen: HashMap<u64, i64>,
}

impl Spend {
    /// Nothing folded yet, for `home` under `prices`.
    #[must_use]
    pub fn new(home: &Path, prices: &PriceTable) -> Self {
        Self {
            home: home.to_string_lossy().into_owned(),
            prices: prices.as_of().unwrap_or_default().to_owned(),
            files: BTreeMap::new(),
            buckets: BTreeMap::new(),
            seen: HashMap::new(),
        }
    }

    /// The price table's date the buckets were priced by.
    #[must_use]
    pub fn prices_as_of(&self) -> &str {
        &self.prices
    }

    /// The buckets, oldest first: each one's start ([`BUCKET_S`] wide) and
    /// each model's tally.
    pub fn buckets(&self) -> impl Iterator<Item = (i64, &BTreeMap<String, Tally>)> {
        self.buckets.iter().map(|(k, v)| (*k, v))
    }

    /// Whether this spend was folded for `home` under `prices`.
    fn is_for(&self, home: &Path, prices: &PriceTable) -> bool {
        self.home == home.to_string_lossy() && self.prices == prices.as_of().unwrap_or_default()
    }

    /// Fold what every transcript under the home's `projects/` appended since
    /// the last fold, within `limits`, newest file first. `now` is the
    /// caller's clock. A spend priced by another table is emptied first and
    /// folded again from the transcripts.
    pub fn fold_claude_home(
        &mut self,
        prices: &PriceTable,
        now: i64,
        limits: Limits,
    ) -> FoldReport {
        let home = PathBuf::from(&self.home);
        if !self.is_for(&home, prices) {
            *self = Self::new(&home, prices);
        }
        let projects = home.join("projects");
        let walk = walk(&projects, now.saturating_sub(HORIZON_S), &limits);
        let mut report = FoldReport {
            files: walk.files.len(),
            caught_up: true,
            unread: walk.unread,
            walk_cut: walk.cut,
            ..FoldReport::default()
        };
        // A transcript gone, or past the horizon, keeps no cursor: what it
        // counted stays in the buckets, and its ids in the home's, until they
        // age out, and every row a file past the horizon holds is older than
        // the horizon too. An incomplete walk cannot prove a file is gone:
        // keep its streaming state so later growth is not mistaken for a copy.
        if !walk.cut && walk.unread == 0 {
            let there: BTreeSet<&str> = walk.files.iter().map(|f| f.rel.as_str()).collect();
            self.files.retain(|rel, _| there.contains(rel.as_str()));
        }
        let mut left = limits.bytes;
        for found in &walk.files {
            if !self.files.contains_key(&found.rel) && self.files.len() >= MAX_FILES {
                report.unread += 1;
                continue;
            }
            let cursor = self.files.entry(found.rel.clone()).or_default();
            if cursor.unchanged(&found.meta) {
                quiet(cursor, &found.meta, now);
                continue;
            }
            if left == 0 || limits.over() {
                report.caught_up = false;
                continue;
            }
            let mut sink = Sink {
                prices,
                now,
                buckets: &mut self.buckets,
                seen: &mut self.seen,
                report: &mut report,
            };
            match advance(cursor, &found.path, &mut left, &limits, &mut sink) {
                Ok(true) => quiet(cursor, &found.meta, now),
                Ok(false) => report.caught_up = false,
                Err(()) => report.unread += 1,
            }
        }
        self.expire(now);
        report
    }

    /// Count one served message, priced by `prices`, in the bucket of its
    /// time: the tests' seam, a spend composed with no transcript. A message
    /// older than [`HORIZON_S`] is not counted; one stamped ahead of
    /// `now` is placed now; a model that is no model id is filed (unpriced)
    /// under [`UNKNOWN_MODEL`], as the transcript fold files it.
    #[cfg(test)]
    pub fn add_message(&mut self, message: &Message<'_>, prices: &PriceTable, now: i64) {
        if message.at < now.saturating_sub(HORIZON_S) || message.tokens.is_zero() {
            return;
        }
        let model = if super::upgrade::is_model_id(message.model) {
            message.model
        } else {
            UNKNOWN_MODEL
        };
        let usd = prices.message_usd(model, &message.tokens);
        add(
            &mut self.buckets,
            message.at.min(now),
            model,
            tally(1, message.tokens, usd, message.approx),
        );
    }

    /// Drop the buckets and counted ids past the horizon, and the oldest ids
    /// past [`MAX_SEEN`]. No bucket is merged into a wider one.
    pub fn expire(&mut self, now: i64) {
        let horizon = now.saturating_sub(HORIZON_S);
        for cursor in self.files.values_mut() {
            for held in &mut cursor.ids {
                if let Some(parts) = held.contributions.as_mut() {
                    parts.retain(|p| p.bucket.saturating_add(BUCKET_S) > horizon);
                }
            }
        }
        self.seen
            .retain(|_, hour| hour.saturating_add(HOUR_S) > horizon);
        if self.seen.len() > MAX_SEEN {
            // The hour of the oldest id that would be kept goes too, so what
            // stays is at most MAX_SEEN.
            let mut hours: Vec<i64> = self.seen.values().copied().collect();
            hours.sort_unstable();
            if let Some(&oldest_kept) = hours.get(hours.len() - MAX_SEEN) {
                self.seen.retain(|_, hour| *hour > oldest_kept);
            }
        }
        self.buckets
            .retain(|start, _| start.saturating_add(BUCKET_S) > horizon);
    }

    /// The spend in `span`, of the models of `family` where one is named
    /// (`fable` for the Fable bucket).
    #[must_use]
    pub fn sum(&self, span: Span, family: Option<&str>) -> Sum {
        let mut sum = Sum::default();
        for (&start, models) in &self.buckets {
            let inside = start >= span.from
                && if span.whole {
                    start.saturating_add(BUCKET_S) <= span.to
                } else {
                    start < span.to
                };
            if !inside {
                continue;
            }
            for (model, t) in models {
                if family.is_some_and(|f| family_of(model).as_deref() != Some(f)) {
                    continue;
                }
                sum.messages = sum.messages.saturating_add(t.messages);
                sum.tokens = sum.tokens.plus(&t.tokens);
                sum.approx = sum.approx.saturating_add(t.approx);
                sum.usd += t.usd;
                if !t.unpriced.is_zero() {
                    sum.unpriced_tokens = sum.unpriced_tokens.plus(&t.unpriced);
                    let held = sum.unpriced.entry(model.clone()).or_default();
                    *held = held.plus(&t.unpriced);
                }
            }
        }
        sum
    }
}

/// Where one fold's rows go: the table and clock they are priced and placed
/// by, the buckets, the home's counted ids and the report.
struct Sink<'a> {
    prices: &'a PriceTable,
    now: i64,
    buckets: &'a mut Buckets,
    seen: &'a mut HashMap<u64, i64>,
    report: &'a mut FoldReport,
}

impl Sink<'_> {
    /// The fold's report.
    fn report(&mut self) -> &mut FoldReport {
        self.report
    }

    /// Fold one complete line (its newline cut) of the file `cursor` follows.
    fn row(&mut self, line: &[u8], cursor: &mut Cursor) {
        fold_row(line, cursor, self);
    }
}

/// One message's tally (`messages` 1), or what a later row of it added (0),
/// `approx` when it made the message approximate.
fn tally(messages: u64, tokens: TokenUsage, usd: Option<f64>, approx: bool) -> Tally {
    Tally {
        messages,
        tokens,
        usd: usd.unwrap_or(0.0),
        unpriced: if usd.is_some() {
            TokenUsage::default()
        } else {
            tokens
        },
        approx: u64::from(approx),
    }
}

/// Add `t` under `model` to the bucket of `at`.
fn add(buckets: &mut Buckets, at: i64, model: &str, t: Tally) {
    let into = buckets
        .entry(at.div_euclid(BUCKET_S) * BUCKET_S)
        .or_default();
    merge_model(into, model, &t);
}

/// Add `t` under `model`, or under [`OTHER_MODEL`] once [`MAX_MODELS`] are
/// held — where it is unpriced: the bucket no longer says which model it was.
fn merge_model(into: &mut BTreeMap<String, Tally>, model: &str, t: &Tally) {
    if into.contains_key(model) || into.len() < MAX_MODELS {
        into.entry(model.to_owned()).or_default().absorb(t);
    } else {
        let mut other = *t;
        other.unpriced = other.tokens;
        other.usd = 0.0;
        into.entry(OTHER_MODEL.to_owned())
            .or_default()
            .absorb(&other);
    }
}

/// Trim a file the fold read to its end, the only files it is called on: a
/// quiet one keeps no message ids, one still being written the newest
/// [`IDS_KEPT`]. A file not read to its end is not trimmed: a message it
/// holds is raised by its rows not read yet, which with its id let go would
/// fall to the home's counted ids as a copy.
fn quiet(cursor: &mut Cursor, meta: &std::fs::Metadata, now: i64) {
    let quiet = cursor.end && mtime_ns(meta) / 1_000_000_000 <= now.saturating_sub(QUIET_S);
    cursor.let_go(if quiet { 0 } else { IDS_KEPT });
}

/// Read what one transcript appended since its cursor, within `left` bytes,
/// and fold every line it completes. `Ok(true)` when it reached the end the
/// file had; `Err` when it could not be read. A file that changed other than
/// by an append (another file at the path, shorter than the offset, another
/// byte before it) is read again from its start.
fn advance(
    cursor: &mut Cursor,
    path: &Path,
    left: &mut u64,
    limits: &Limits,
    sink: &mut Sink<'_>,
) -> Result<bool, ()> {
    let mut file = super::footer::open_regular(path).ok_or(())?;
    let meta = file.metadata().map_err(|_| ())?;
    let size = meta.len();
    let id = super::session_usage::open_identity(&file, &meta);
    let mut rewritten = matches!((cursor.identity, id), (Some(held), Some(now)) if held != now)
        || size < cursor.offset;
    if let (false, Some(expect), Some(at)) =
        (rewritten, cursor.boundary, cursor.offset.checked_sub(1))
    {
        let mut byte = [0u8; 1];
        let ok = file.seek(SeekFrom::Start(at)).is_ok() && matches!(file.read(&mut byte), Ok(1));
        let report = sink.report();
        report.read = report.read.saturating_add(1);
        rewritten = !ok || byte[0] != expect;
    }
    if rewritten {
        cursor.restart();
        sink.report().refolded += 1;
    }
    cursor.identity = id;
    // The budget is spent at line boundaries: a line begun is read to its
    // newline (or past MAX_LINE_BYTES, skipped), so a budget smaller than a
    // line still moves the fold on.
    let mut line: Vec<u8> = Vec::new();
    let mut pos = cursor.offset;
    let mut chunk = vec![0u8; READ_CHUNK];
    file.seek(SeekFrom::Start(pos)).map_err(|_| ())?;
    let mut read_to = pos;
    'read: while read_to < size && !limits.over() {
        let mid_line = !line.is_empty() || cursor.skipping;
        if *left == 0 && !mid_line {
            break;
        }
        let mut ask = usize::try_from(size - read_to)
            .unwrap_or(READ_CHUNK)
            .min(READ_CHUNK);
        if *left > 0 {
            ask = ask.min(usize::try_from(*left).unwrap_or(READ_CHUNK));
        }
        let n = match file.read(&mut chunk[..ask]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        read_to += n as u64;
        *left = left.saturating_sub(n as u64);
        let report = sink.report();
        report.read = report.read.saturating_add(n as u64);
        let mut bytes = &chunk[..n];
        while !bytes.is_empty() {
            let newline = bytes.iter().position(|&b| b == b'\n');
            let take = newline.map_or(bytes.len(), |i| i + 1);
            let (head, rest) = bytes.split_at(take);
            pos += take as u64;
            if cursor.skipping {
                cursor.offset = pos;
                cursor.boundary = head.last().copied();
                if newline.is_some() {
                    cursor.skipping = false;
                }
            } else if newline.is_some() {
                line.extend_from_slice(&head[..head.len() - 1]);
                sink.row(&line, cursor);
                line.clear();
                cursor.offset = pos;
                cursor.boundary = Some(b'\n');
            } else {
                line.extend_from_slice(head);
                if line.len() > MAX_LINE_BYTES {
                    // Discarded as it streams, never held: the bytes are
                    // folded past, and the rest up to its newline skipped.
                    let report = sink.report();
                    report.rows_long = report.rows_long.saturating_add(1);
                    line = Vec::new();
                    cursor.skipping = true;
                    cursor.offset = pos;
                    cursor.boundary = head.last().copied();
                }
            }
            bytes = rest;
            if *left == 0 && line.is_empty() && !cursor.skipping {
                // Out of budget at a line's end: what follows is read next
                // time.
                pos = cursor.offset;
                break 'read;
            }
        }
    }
    cursor.size = size;
    cursor.mtime_ns = mtime_ns(&meta);
    cursor.end = pos >= size;
    Ok(cursor.end)
}

/// Fold one complete line: an assistant row with usage, served (not an API
/// error), timed, inside the horizon, counted once in the home per
/// `message.id`, at its largest counts within its file.
fn fold_row(bytes: &[u8], cursor: &mut Cursor, sink: &mut Sink<'_>) {
    let report = &mut *sink.report;
    let Ok(line) = std::str::from_utf8(bytes) else {
        report.rows_unparsed = report.rows_unparsed.saturating_add(1);
        return;
    };
    let line = line.trim();
    if line.is_empty() || super::usage::cannot_be_assistant(line) {
        return;
    }
    let Some(value) = super::transcript::usage(line).ok().filter(Value::is_object) else {
        report.rows_unparsed = report.rows_unparsed.saturating_add(1);
        return;
    };
    let Some(obj) = value.as_object() else {
        return;
    };
    if obj.get("type").and_then(Value::as_str) != Some("assistant")
        || obj.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
    {
        return;
    }
    let Some(message) = obj.get("message").and_then(Value::as_object) else {
        return;
    };
    let Some(usage) = message.get("usage").and_then(Value::as_object) else {
        return;
    };
    let Some(tokens) = TokenUsage::from_object(usage).filter(|t| !t.is_zero()) else {
        return;
    };
    let Some(at) = super::usage::row_time(obj) else {
        report.rows_untimed = report.rows_untimed.saturating_add(1);
        return;
    };
    let now = sink.now;
    if at < now.saturating_sub(HORIZON_S) {
        return;
    }
    let at = at.min(now);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| super::upgrade::is_model_id(m))
        .unwrap_or(UNKNOWN_MODEL);
    let approx = approx(usage);
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_ID_BYTES)
        .map(|s| aterm_hash::fnv1a_64(s.as_bytes()));
    let held = id.and_then(|key| cursor.ids.iter_mut().rev().find(|held| held.key == key));
    if let Some(held) = held {
        report.duplicates = report.duplicates.saturating_add(1);
        // A message is approximate when any row of it is, and counted so
        // once: Claude Code writes `server_tool_use` on most streamed
        // messages, and on 2.1.300 often `speed`, only with the final usage.
        let newly = approx && !held.approx;
        held.approx |= approx;
        let changed_model = held
            .contributions
            .as_ref()
            .is_some_and(|parts| parts.iter().any(|p| p.model != model));
        if changed_model {
            held.unprice(sink.buckets);
            held.contributions = None;
        }
        let grown = held.tokens.max(&tokens);
        if grown == held.tokens {
            if newly {
                let marked = Tally {
                    approx: 1,
                    ..Tally::default()
                };
                add(sink.buckets, at, model, marked);
            }
            return;
        }
        // A streamed message's later row: count only what grew, priced as
        // the whole message's price moved.
        let growth = grown.less(&held.tokens);
        let usd = match (
            sink.prices.message_usd(model, &grown),
            sink.prices.message_usd(model, &held.tokens),
        ) {
            _ if held.contributions.is_none() => None,
            (Some(now_usd), Some(was)) => {
                if now_usd < was {
                    held.reduce_price(sink.buckets, was - now_usd);
                }
                Some((now_usd - was).max(0.0))
            }
            (Some(now_usd), None) => {
                held.price(sink.buckets);
                Some(now_usd)
            }
            (None, Some(_)) => {
                held.unprice(sink.buckets);
                None
            }
            (None, None) => None,
        };
        held.tokens = grown;
        held.add(sink.buckets, at, model, tally(0, growth, usd, newly));
        return;
    }
    if let Some(key) = id {
        // Another transcript counted it: a copy, never a second message.
        if sink.seen.contains_key(&key) {
            report.duplicates = report.duplicates.saturating_add(1);
            return;
        }
        sink.seen.insert(key, at.div_euclid(HOUR_S) * HOUR_S);
        let mut held = Held {
            key,
            tokens,
            approx,
            contributions: Some(Vec::new()),
        };
        let usd = sink.prices.message_usd(model, &tokens);
        held.add(sink.buckets, at, model, tally(1, tokens, usd, approx));
        cursor.ids.push_back(held);
        cursor.let_go(SEEN_IDS_MAX);
    } else {
        let usd = sink.prices.message_usd(model, &tokens);
        add(sink.buckets, at, model, tally(1, tokens, usd, approx));
    }
    report.messages = report.messages.saturating_add(1);
}

/// One transcript the walk found.
struct Found {
    rel: String,
    path: PathBuf,
    meta: std::fs::Metadata,
}

/// The walk of `projects/`.
struct Walk {
    /// Newest first.
    files: Vec<Found>,
    cut: bool,
    /// Entries which could not be inspected or named by a UTF-8 path.
    unread: usize,
}

/// Every regular `*.jsonl` under `projects` (links not followed), at most
/// [`MAX_DEPTH`] directories down and [`MAX_WALK_ENTRIES`] entries in all,
/// modified at or after `since`, listed before the fold's deadline
/// ([`Limits::until`]): a walk stopped by either is cut.
fn walk(projects: &Path, since: i64, limits: &Limits) -> Walk {
    let mut out = Walk {
        files: Vec::new(),
        cut: false,
        unread: 0,
    };
    let root = match std::fs::symlink_metadata(projects) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return out,
        Err(_) => {
            out.unread = 1;
            return out;
        }
    };
    let linked = root.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt as _;
        linked || root.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if linked || !root.is_dir() {
        out.unread = 1;
        return out;
    }
    let mut stack = vec![(projects.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    'dirs: while let Some((dir, depth)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => {
                // Only a genuinely absent projects root means an empty home.
                // A vanished child, dangling link or failed read is incomplete.
                let absent = depth == 0
                    && error.kind() == io::ErrorKind::NotFound
                    && matches!(std::fs::symlink_metadata(&dir), Err(missing)
                        if missing.kind() == io::ErrorKind::NotFound);
                if !absent {
                    out.unread += 1;
                }
                continue;
            }
        };
        for entry in entries {
            seen += 1;
            if seen > MAX_WALK_ENTRIES || limits.over() {
                out.cut = true;
                break 'dirs;
            }
            let Ok(entry) = entry else {
                out.unread += 1;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                out.unread += 1;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    out.cut = true;
                }
                continue;
            }
            if !kind.is_file() || path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                out.unread += 1;
                continue;
            };
            if mtime_ns(&meta) / 1_000_000_000 < since {
                continue;
            }
            match path.strip_prefix(projects).ok().and_then(Path::to_str) {
                Some(rel) => out.files.push(Found {
                    rel: rel.to_owned(),
                    path: path.clone(),
                    meta,
                }),
                None => out.unread += 1,
            }
        }
    }
    out.files
        .sort_by_key(|f| std::cmp::Reverse((mtime_ns(&f.meta), f.rel.clone())));
    out
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// A span of time to sum: buckets that START in `[from, to)`, or, `whole`,
/// that lie wholly inside it (so nothing after `to` is counted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Unix seconds, inclusive.
    pub from: i64,
    /// Unix seconds, exclusive.
    pub to: i64,
    /// Count only buckets that END by `to`.
    pub whole: bool,
}

/// What a span holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sum {
    /// List dollars of the priced messages.
    pub usd: f64,
    /// Every message's tokens.
    pub tokens: TokenUsage,
    /// The tokens no row priced, all models.
    pub unpriced_tokens: TokenUsage,
    /// The same, by model.
    pub unpriced: BTreeMap<String, TokenUsage>,
    /// Messages counted.
    pub messages: u64,
    /// Messages priced at the base rates ([`Tally::approx`]).
    pub approx: u64,
}

impl Sum {
    /// Whether more than [`SHARE_MAX`] of any token class is unpriced.
    #[must_use]
    pub fn unpriced_over(&self) -> bool {
        let t = &self.tokens;
        let u = &self.unpriced_tokens;
        [
            (u.input, t.input),
            (u.output, t.output),
            (u.cache_write, t.cache_write),
            // A final streamed row can reveal an unpriced lifetime without
            // increasing the total writes, leaving only this class unpriced.
            (u.cache_write_1h, t.cache_write_1h),
            (u.cache_read, t.cache_read),
        ]
        .into_iter()
        .any(|(unpriced, all)| all > 0 && unpriced as f64 > SHARE_MAX * all as f64)
    }
}

/// How a reading was taken, which decides its resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingKind {
    /// Rounded to the digit it carries (the tap, the usage cache).
    Rounded,
    /// Floored to a whole percent (a painted `/usage` panel): the truth lies
    /// in `[p, p + 1)`.
    Floored,
    /// A wall: exactly 100%, the best reading there is.
    Wall,
}

/// One reading of a window's use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// Percent used, in tenths (`425` is 42.5%).
    pub pct_x10: u32,
    /// Unix seconds it was served.
    pub at: i64,
    /// How it was taken.
    pub kind: ReadingKind,
}

impl Reading {
    /// The fraction used and the half-width of what it may be, at the
    /// resolution actually seen: whole percents until a tenth was
    /// (`tenths_seen`).
    fn point(&self, tenths_seen: bool) -> (f64, f64) {
        let pct = f64::from(self.pct_x10) / 10.0;
        match self.kind {
            ReadingKind::Wall => (1.0, 0.0),
            ReadingKind::Rounded if tenths_seen => (pct / 100.0, 0.0005),
            ReadingKind::Rounded => (pct.round() / 100.0, 0.005),
            ReadingKind::Floored => ((pct.floor() + 0.5) / 100.0, 0.005),
        }
    }

    /// Too small a figure for a ratio: under 5 whole percents, or under 1%
    /// once tenths are seen.
    fn low(&self, tenths_seen: bool) -> bool {
        match self.kind {
            ReadingKind::Wall => false,
            ReadingKind::Rounded if tenths_seen => self.pct_x10 < 10,
            ReadingKind::Rounded => (self.pct_x10 + 5) / 10 < 5,
            ReadingKind::Floored => self.pct_x10 / 10 < 5,
        }
    }
}

/// Why a capacity or a `left` figure is empty: one closed word each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// No reading in this window (or it has passed its reset).
    NoReading,
    /// The reading is too small for a ratio, or no whole bucket of priced
    /// spend comes before it.
    Low,
    /// The window is spent: nothing is left to estimate.
    AtLimit,
    /// The newest reading is older than a tenth of its window, or a machine
    /// that used the account in it did not answer.
    Stale,
    /// More than [`SHARE_MAX`] of a token class has no price.
    Unpriced,
    /// More than [`SHARE_MAX`] of the spend cannot be put on one account.
    Unattributed,
    /// A process on an API key, Bedrock or Vertex used the home in it.
    ApiKeySeen,
    /// Usage credits served a message in the window ([`Ask::on_credits`]):
    /// part of its spend is not the plan's, and no bucket says which part.
    OnCredits,
}

impl Why {
    /// The closed word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Why::NoReading => "no-reading",
            Why::Low => "low",
            Why::AtLimit => "at-limit",
            Why::Stale => "stale",
            Why::Unpriced => "unpriced",
            Why::Unattributed => "unattributed",
            Why::ApiKeySeen => "api-key-seen",
            Why::OnCredits => "on-credits",
        }
    }
}

/// Whose the home's messages were, as the account record can tell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Attribution {
    /// Epoch rows say so.
    Seen,
    /// No other account was seen on the home since the oldest open window
    /// began, so they are taken as the current account's.
    Assumed,
    /// A second account was seen in the window: this share of the spend
    /// cannot be put on either.
    Unattributed {
        /// A fraction of the window's spend.
        share: f64,
    },
}

impl Attribution {
    /// The closed word: `seen`, `assumed` or `unattributed`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Attribution::Seen => "seen",
            Attribution::Assumed => "assumed",
            Attribution::Unattributed { .. } => "unattributed",
        }
    }
}

/// The machines behind a figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Machines {
    /// Machines whose spend is summed.
    pub reported: u32,
    /// Machines that used the account in the window.
    pub used: u32,
    /// Of those, the ones that did not answer.
    pub silent: u32,
}

/// What one window's figure is asked from.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    /// The window's length (5 h, a week).
    pub length_s: i64,
    /// Its reset, from a current reading; `None` sums a rolling span ending
    /// now, with no capacity.
    pub resets_at: Option<i64>,
    /// A reset used inside the window: the vendor refilled it there.
    pub epoch_at: Option<i64>,
    /// Only the models of this family (`fable`, the Fable bucket).
    pub family: Option<&'a str>,
    /// The reading `C` is taken at: the best one in the window.
    pub reading: Option<Reading>,
    /// The newest reading (`u_now`); `reading` where `None` or from before
    /// the window.
    pub newest: Option<Reading>,
    /// A non-zero tenth was seen in this account's readings.
    pub tenths_seen: bool,
    /// Usage credits served a message in the window: S5 read them `in use`,
    /// a wall row said `isUsingOverage`, or a message was served after a
    /// reading of 100%. Then `C` and `left` mean nothing ([`Why::OnCredits`]):
    /// no share of the window's spend is split off by time.
    pub on_credits: bool,
    /// Whose the messages were.
    pub attribution: Attribution,
    /// A process on an API key, Bedrock or Vertex used the home.
    pub api_key_seen: bool,
    /// The machines behind it.
    pub machines: Machines,
    /// The caller's clock.
    pub now: i64,
}

/// What a full window holds, at list price: a LOWER BOUND.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capacity {
    /// List dollars, `≥`.
    pub usd: f64,
    /// The reading it was taken at.
    pub at: i64,
    /// The error bar, ± percent: the reading's resolution. Uncalibrated:
    /// the spread of closed windows waits for the series (A2).
    pub pm_pct: f64,
}

/// What is left of a window, at list price: an ESTIMATE, never a bound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Left {
    /// List dollars, `≈`.
    pub usd: f64,
    /// How old `u_now` is.
    pub age_s: i64,
}

/// One window's dollar figure.
#[derive(Debug, Clone, PartialEq)]
pub struct Figure {
    /// No reset known: a rolling span ending now.
    pub rolling: bool,
    /// The span summed, unix seconds.
    pub from: i64,
    /// Its end (the reset; now, rolling).
    pub to: i64,
    /// What was spent in it so far.
    pub used: Sum,
    /// What a full window holds, or why that means nothing here.
    pub capacity: Result<Capacity, Why>,
    /// What is left, or why that means nothing here.
    pub left: Result<Left, Why>,
}

/// One window's dollars: spent so far (from the window's start, or from a
/// reset used inside it), and with a reading in it, its capacity, with its
/// error bar, and what is left, with `u_now`'s age, or the word that says
/// why each is empty.
#[must_use]
pub fn figure(spend: &Spend, ask: &Ask<'_>) -> Figure {
    let (from, to, rolling) = match ask.resets_at {
        Some(reset) => (
            reset
                .saturating_sub(ask.length_s)
                .max(ask.epoch_at.unwrap_or(i64::MIN)),
            reset,
            false,
        ),
        None => (
            ask.now.saturating_sub(ask.length_s),
            ask.now.saturating_add(1),
            true,
        ),
    };
    let used = spend.sum(
        Span {
            from,
            to,
            whole: false,
        },
        ask.family,
    );
    let capacity = if rolling {
        Err(Why::NoReading)
    } else {
        capacity(spend, ask, from)
    };
    let left = capacity.and_then(|c| left(ask, c, from));
    Figure {
        rolling,
        from,
        to,
        used,
        capacity,
        left,
    }
}

fn capacity(spend: &Spend, ask: &Ask<'_>, from: i64) -> Result<Capacity, Why> {
    if ask.api_key_seen {
        return Err(Why::ApiKeySeen);
    }
    if ask.on_credits {
        return Err(Why::OnCredits);
    }
    if matches!(ask.attribution, Attribution::Unattributed { share } if share.is_nan() || share > SHARE_MAX)
    {
        return Err(Why::Unattributed);
    }
    // A reading from before the window (or before a reset used in it) is of
    // the spend the vendor has refilled.
    let reading = ask
        .reading
        .filter(|r| r.at >= from && ask.resets_at.is_some_and(|reset| r.at < reset))
        .ok_or(Why::NoReading)?;
    if reading.low(ask.tenths_seen) {
        return Err(Why::Low);
    }
    let spent = spend.sum(
        Span {
            from,
            to: reading.at,
            whole: true,
        },
        ask.family,
    );
    if spent.unpriced_over() {
        return Err(Why::Unpriced);
    }
    // No whole bucket of priced spend before the reading (a window's first
    // minutes): `$0 ÷ u` is no capacity.
    if spent.usd <= 0.0 {
        return Err(Why::Low);
    }
    let (point, half) = reading.point(ask.tenths_seen);
    Ok(Capacity {
        usd: spent.usd / point,
        at: reading.at,
        pm_pct: half / point * 100.0,
    })
}

fn left(ask: &Ask<'_>, capacity: Capacity, from: i64) -> Result<Left, Why> {
    // A newest reading from before the window (or before a reset used in it)
    // is no `u_now` of it; the reading `C` was taken at stands in.
    let newest = ask
        .newest
        .filter(|n| n.at >= from && ask.resets_at.is_some_and(|reset| n.at < reset))
        .or(ask.reading)
        .ok_or(Why::NoReading)?;
    if newest.pct_x10 >= 1000 {
        return Err(Why::AtLimit);
    }
    let age_s = ask.now.saturating_sub(newest.at).max(0);
    if age_s > ask.length_s / 10 || ask.machines.silent > 0 {
        return Err(Why::Stale);
    }
    let (used, _) = newest.point(ask.tenths_seen);
    Ok(Left {
        usd: capacity.usd * (1.0 - used).max(0.0),
        age_s,
    })
}

#[cfg(test)]
#[path = "spend_tests.rs"]
mod tests;
