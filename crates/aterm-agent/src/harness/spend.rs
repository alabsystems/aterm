// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! List-price estimates for `harness usage` inside a Codex conversation.
//!
//! The caller supplies a dated price table; an unknown model or a token class
//! with no rate stays unpriced. Each request selects its own long-prompt tier.
//! The rollout reader excludes copied prefixes and repeated counts before the
//! conversation fold sums its own thread and subagents. These are estimates
//! from recorded turns, never a bill or an account's remaining capacity.
//!
//! `aterm lab prices` cross-checks the shipped table against managed vendor
//! catalogs without changing it. Account-wide transcript scanning, spend-file
//! persistence and capacity estimation are not implemented here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::harness::codex_rollout::{Row, Turn, TurnReader};

use super::usage::{TokenUsage, UNKNOWN_MODEL};

/// The shipped price table, for the caller to load and hand to the fold.
pub const PRICES_TOML: &str = include_str!("prices.toml");

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
/// ([`Self::from_toml`]) and hands to the conversation fold.
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

/// One served turn mapped into the price table's token classes, with its
/// model and whether the table cannot price it exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message<'a> {
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
    /// ([`ModelTurns::approx`]).
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
    /// Turns with no readable stamp: the rollout reader does not count them.
    pub untimed: u64,
    /// Lines that were not JSON, or a `token_count` that could not be read.
    pub unparsed: u64,
    /// Lines longer than [`super::usage::MAX_LINE_BYTES`], skipped.
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

#[cfg(test)]
#[path = "spend_tests.rs"]
mod tests;
