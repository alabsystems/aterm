// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The usage view (design §5.2): the two JSON shapes Claude Code writes to
//! disk, read into one per-account view; the one-line HUD; the
//! `harness usage --json` document (schema 1).
//!
//! Everything here is a pure function of its arguments. No clock is read
//! (`as_of` and every `age_s` are injected), no environment, no network, no
//! control socket. The one I/O surface is [`TranscriptUsage::fold_reader`],
//! which takes a `BufRead` the caller opened.
//!
//! # The two inputs
//!
//! **The statusLine JSON** ([`parse_statusline`]) is what the vendor pipes into
//! its `statusLine` command on stdin. The field names below are those the
//! vendor's own help text documents (MEASURED 2026-09-17 against claude
//! 2.1.274). Every field is optional and unknown fields are ignored, because
//! the vendor ships roughly one build a day and a renamed key must degrade the
//! HUD, not break it. Numbers are accepted as integer or float. The parser
//! underneath is `aterm_json`, which bounds nesting at 128 and refuses a lone
//! surrogate, so hostile input is an `Err`, never a panic.
//!
//! **Transcript rows** ([`TranscriptUsage`]) are the lines of
//! `~/.claude/projects/<slug>/<uuid>.jsonl`: one JSON object per line. An
//! assistant row carries `message.{id,model,usage}`, and the SAME `message.id`
//! repeats once per content block with the usage object copied onto each, so a
//! sum must count each id once. MEASURED on this box, 2026-09-19, over 30
//! transcripts and 15,901 assistant rows: the longest line was 1,125,606
//! bytes; an id repeated at most 11 times and never more than 5 rows apart;
//! all 8,202 repeated usage objects were byte-for-byte the same as the first;
//! every usage carried all four token counts as integers; `isSidechain` was a
//! key on every row and `true` on none. The bounds below are set from those
//! numbers, with room:
//!
//! | bound | value | why |
//! |---|---|---|
//! | [`MAX_LINE_BYTES`] | 16 MiB | 16× the longest measured row; a longer one is skipped and counted, never buffered |
//! | [`SEEN_IDS_MAX`] | 4096 | ids are remembered in arrival order and the oldest forgotten past this; ~700× the measured 5-row repeat distance |
//! | [`MAX_ID_BYTES`] | 128 | a longer `message.id` is treated as absent (summed, not deduped) so a hostile id cannot fill the set |
//! | [`MAX_MODELS`] | 64 | past this many distinct model ids, further ones fold into `"(other)"` |
//! | [`MAX_WINDOWS`] | 32 | rate-limit windows kept from one statusLine, in key order |
//!
//! # What is assumed
//!
//! A repeat whose usage DIFFERS from the first (never measured) keeps the first
//! and counts the conflict; it is not summed and not overwritten. A row with
//! usage but no id cannot be deduped: it is summed and counted, because for a
//! spend figure an over-count is the direction that does not hide cost. A
//! sidechain row (`isSidechain: true`) is real API spend and is summed, and
//! counted so the view can say so. A token field that is missing reads as 0;
//! a usage object with none of the four is "no usage". A price the caller has
//! not supplied yields `usd: null`, never `0`: zero would claim the tokens were
//! free.
//!
//! # Honesty of "per model" (design §5.2)
//!
//! The windows (`five_hour`, `seven_day`, `spend_limit`, …) are per ACCOUNT:
//! the vendor reports them account-wide. The per-model figures are SPEND —
//! tokens and, with a caller-supplied [`PriceTable`], dollars — folded from the
//! transcript. Nothing here carries a built-in price; the table is injected.
//! The JSON says which is which by shape (`windows` beside `spend`), and the
//! HUD labels the windows with the account, never with the model.
//!
//! STATUS: unit-tested against inline fixtures and the measured row shapes
//! above. REACHED from the front door as of 2026-09-22 — `aterm harness
//! usage [--json]` prints the view, and `aterm harness statusline` records
//! the vendor's sample and prints [`hud_line`] for the vendor's own footer —
//! which corrects the line that stood here saying it was wired to no verb.
//! Still TARGET: no status BAR renders the HUD (design §5.2's `Lane::Harness`
//! does not exist), and the Sheets sync of §5.2 is unwritten.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{self, BufRead};

use aterm_json::{Map, Value};

use super::source::Source;

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// The longest transcript line that is parsed; a longer one is counted in
/// [`TranscriptUsage::rows_skipped_long`] and its bytes are discarded as they
/// stream, so memory stays bounded whatever the file holds. 16× the longest
/// row MEASURED on this box (1,125,606 bytes).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// The longest statusLine document that is parsed. A real one is a few
/// kilobytes (the vendor's shape holds a handful of strings and numbers);
/// this cap is assumed, not measured, and exists only so a runaway writer
/// cannot make the reader allocate without limit.
pub const MAX_STATUSLINE_BYTES: usize = 1024 * 1024;

/// How many `message.id`s [`TranscriptUsage`] remembers for dedupe. Ids are
/// kept in arrival order; once more than this many are held, the oldest is
/// forgotten and a later repeat of it would be summed again. MEASURED: repeats
/// arrive within 5 rows of each other, so the bound is ~700× what is needed.
pub const SEEN_IDS_MAX: usize = 4096;

/// A `message.id` longer than this is treated as absent. Real ids are
/// `msg_…` strings of a few dozen bytes (MEASURED).
pub const MAX_ID_BYTES: usize = 128;

/// Distinct model ids [`TranscriptUsage`] keeps separately; further ones fold
/// into the `"(other)"` row.
pub const MAX_MODELS: usize = 64;

/// The most rate-limit windows kept from one statusLine, in key order.
pub const MAX_WINDOWS: usize = 32;

/// The HUD line's byte cap — the `meta set description` cap.
pub const HUD_MAX_BYTES: usize = 1024;

/// The model key spend is filed under when an assistant row carries none.
pub const UNKNOWN_MODEL: &str = "unknown";

/// The model key spend is filed under past [`MAX_MODELS`] distinct ids.
pub const OTHER_MODEL: &str = "(other)";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a document could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageError {
    /// Not JSON, or JSON `aterm_json` refuses (nesting past 128, a lone
    /// surrogate, trailing data, a control character in a string).
    Json(String),
    /// Well-formed JSON whose top level is not an object.
    NotObject,
    /// Longer than the cap named.
    TooLong {
        /// The document's length.
        bytes: usize,
        /// The cap it exceeded.
        max: usize,
    },
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(why) => write!(f, "not JSON: {why}"),
            Self::NotObject => f.write_str("top level is not an object"),
            Self::TooLong { bytes, max } => write!(f, "{bytes} bytes exceeds the {max}-byte cap"),
        }
    }
}

impl std::error::Error for UsageError {}

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

/// An epoch-seconds instant; a float is floored.
fn num_i64(v: Option<&Value>) -> Option<i64> {
    let f = num_f64(v)?;
    Some(f as i64)
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

// ---------------------------------------------------------------------------
// The statusLine JSON
// ---------------------------------------------------------------------------

/// One request's token counts — the vendor's `usage` object, in the statusLine
/// (`context_window.current_usage`) and on every assistant transcript row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    /// `input_tokens`.
    pub input: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `cache_creation_input_tokens`.
    pub cache_write: u64,
    /// `cache_read_input_tokens`.
    pub cache_read: u64,
}

impl TokenUsage {
    /// Read the four counts; a missing one reads 0. `None` when none of the
    /// four is present as a number — that object is not a usage.
    fn from_object(obj: &Map) -> Option<Self> {
        let input = num_u64(obj.get("input_tokens"));
        let output = num_u64(obj.get("output_tokens"));
        let cache_write = num_u64(obj.get("cache_creation_input_tokens"));
        let cache_read = num_u64(obj.get("cache_read_input_tokens"));
        if input.is_none() && output.is_none() && cache_write.is_none() && cache_read.is_none() {
            return None;
        }
        Some(Self {
            input: input.unwrap_or(0),
            output: output.unwrap_or(0),
            cache_write: cache_write.unwrap_or(0),
            cache_read: cache_read.unwrap_or(0),
        })
    }
}

/// `model: {id, display_name}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelRef {
    /// `model.id`, e.g. `claude-fable-5-1`.
    pub id: Option<String>,
    /// `model.display_name`.
    pub display_name: Option<String>,
}

/// `workspace: {current_dir, project_dir, …}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Workspace {
    /// `workspace.current_dir`.
    pub current_dir: Option<String>,
    /// `workspace.project_dir`.
    pub project_dir: Option<String>,
}

/// `context_window: {…}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextWindow {
    /// `total_input_tokens`.
    pub total_input_tokens: Option<u64>,
    /// `total_output_tokens`.
    pub total_output_tokens: Option<u64>,
    /// `context_window_size`.
    pub context_window_size: Option<u64>,
    /// `current_usage`, `null` before the first request.
    pub current_usage: Option<TokenUsage>,
    /// `used_percentage`, `null` before the first request.
    pub used_percentage: Option<f64>,
    /// `remaining_percentage`, `null` before the first request.
    pub remaining_percentage: Option<f64>,
}

/// One rate-limit window as the statusLine reports it:
/// `{used_percentage, resets_at}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RateWindow {
    /// `used_percentage`, 0–100 for the time windows; `spend_limit` reads
    /// above 100 once exceeded. Kept as reported, never clamped.
    pub used_pct: Option<f64>,
    /// `resets_at`, epoch seconds.
    pub resets_at: Option<i64>,
}

/// The statusLine's `rate_limits` object. A window is present only while the
/// API reports it, so this is a map keyed by the vendor's window name
/// (`five_hour`, `seven_day`, `spend_limit`, and whatever else arrives).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RateLimits {
    /// Windows by name, at most [`MAX_WINDOWS`] of them in key order.
    pub windows: BTreeMap<String, RateWindow>,
}

impl RateLimits {
    /// The `five_hour` window.
    pub fn five_hour(&self) -> Option<&RateWindow> {
        self.windows.get("five_hour")
    }

    /// The `seven_day` window.
    pub fn seven_day(&self) -> Option<&RateWindow> {
        self.windows.get("seven_day")
    }

    /// The `spend_limit` window.
    pub fn spend_limit(&self) -> Option<&RateWindow> {
        self.windows.get("spend_limit")
    }

    fn from_object(obj: &Map) -> Self {
        let windows = obj
            .iter()
            .filter_map(|(name, v)| {
                let w = v.as_object()?;
                Some((
                    name.clone(),
                    RateWindow {
                        used_pct: num_f64(w.get("used_percentage")),
                        resets_at: num_i64(w.get("resets_at")),
                    },
                ))
            })
            .take(MAX_WINDOWS)
            .collect();
        Self { windows }
    }
}

/// The statusLine stdin JSON, every field optional.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatusLine {
    /// `session_id`.
    pub session_id: Option<String>,
    /// `session_name`.
    pub session_name: Option<String>,
    /// `transcript_path`.
    pub transcript_path: Option<String>,
    /// `cwd`.
    pub cwd: Option<String>,
    /// `version` — the vendor build.
    pub version: Option<String>,
    /// `model`.
    pub model: Option<ModelRef>,
    /// `workspace`.
    pub workspace: Option<Workspace>,
    /// `context_window`.
    pub context_window: Option<ContextWindow>,
    /// `effort.level`.
    pub effort_level: Option<String>,
    /// `thinking.enabled`.
    pub thinking_enabled: Option<bool>,
    /// `rate_limits`; `None` when the key is absent or not an object.
    pub rate_limits: Option<RateLimits>,
}

impl StatusLine {
    /// `model.id`, the one string the HUD names the model by.
    pub fn model_id(&self) -> Option<&str> {
        self.model.as_ref().and_then(|m| m.id.as_deref())
    }

    fn from_object(obj: &Map) -> Self {
        let model = obj
            .get("model")
            .and_then(Value::as_object)
            .map(|m| ModelRef {
                id: str_of(m.get("id")),
                display_name: str_of(m.get("display_name")),
            });
        let workspace = obj
            .get("workspace")
            .and_then(Value::as_object)
            .map(|w| Workspace {
                current_dir: str_of(w.get("current_dir")),
                project_dir: str_of(w.get("project_dir")),
            });
        let context_window = obj
            .get("context_window")
            .and_then(Value::as_object)
            .map(|c| ContextWindow {
                total_input_tokens: num_u64(c.get("total_input_tokens")),
                total_output_tokens: num_u64(c.get("total_output_tokens")),
                context_window_size: num_u64(c.get("context_window_size")),
                current_usage: c
                    .get("current_usage")
                    .and_then(Value::as_object)
                    .and_then(TokenUsage::from_object),
                used_percentage: num_f64(c.get("used_percentage")),
                remaining_percentage: num_f64(c.get("remaining_percentage")),
            });
        Self {
            session_id: str_of(obj.get("session_id")),
            session_name: str_of(obj.get("session_name")),
            transcript_path: str_of(obj.get("transcript_path")),
            cwd: str_of(obj.get("cwd")),
            version: str_of(obj.get("version")),
            model,
            workspace,
            context_window,
            effort_level: obj
                .get("effort")
                .and_then(Value::as_object)
                .and_then(|e| str_of(e.get("level"))),
            thinking_enabled: obj
                .get("thinking")
                .and_then(Value::as_object)
                .and_then(|t| t.get("enabled"))
                .and_then(Value::as_bool),
            rate_limits: obj
                .get("rate_limits")
                .and_then(Value::as_object)
                .map(RateLimits::from_object),
        }
    }
}

/// Parse one statusLine document. Every field is optional, unknown fields are
/// ignored, numbers may be integer or float. Hostile input is an `Err`.
pub fn parse_statusline(json: &str) -> Result<StatusLine, UsageError> {
    if json.len() > MAX_STATUSLINE_BYTES {
        return Err(UsageError::TooLong {
            bytes: json.len(),
            max: MAX_STATUSLINE_BYTES,
        });
    }
    let value: Value = aterm_json::from_str(json).map_err(|e| UsageError::Json(e.to_string()))?;
    let obj = value.as_object().ok_or(UsageError::NotObject)?;
    Ok(StatusLine::from_object(obj))
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
    /// Cache-creation input tokens.
    pub cache_write: u64,
    /// Cache-read input tokens.
    pub cache_read: u64,
    /// Distinct messages counted (one per id; each id-less row counts one).
    pub messages: u64,
}

impl ModelSpend {
    fn add(&mut self, t: TokenUsage) {
        self.input = self.input.saturating_add(t.input);
        self.output = self.output.saturating_add(t.output);
        self.cache_write = self.cache_write.saturating_add(t.cache_write);
        self.cache_read = self.cache_read.saturating_add(t.cache_read);
        self.messages = self.messages.saturating_add(1);
    }

    fn add_spend(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.messages = self.messages.saturating_add(other.messages);
    }
}

/// What [`TranscriptUsage::fold_line`] did with a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// An assistant row with usage, counted into its model's spend.
    Summed,
    /// A repeat of an id already counted, with the same usage: folded away.
    Duplicate,
    /// A repeat of an id already counted whose usage DIFFERED: the first was
    /// kept, this one was not summed, and the conflict was counted.
    Conflict,
    /// A row that is not an assistant row (user, progress, summary, …).
    NotAssistant,
    /// An assistant row with no usage object.
    NoUsage,
    /// Not JSON, not an object, or not UTF-8.
    Unparsed,
    /// Longer than [`MAX_LINE_BYTES`].
    TooLong,
    /// Whitespace only.
    Blank,
}

/// The bounded set of ids already counted, each with the usage first seen.
#[derive(Debug, Clone, Default)]
struct SeenIds {
    order: VecDeque<String>,
    first: BTreeMap<String, TokenUsage>,
}

enum Seen {
    New,
    Same,
    Differs,
}

impl SeenIds {
    fn remember(&mut self, id: &str, usage: TokenUsage) -> Seen {
        if let Some(first) = self.first.get(id) {
            return if *first == usage {
                Seen::Same
            } else {
                Seen::Differs
            };
        }
        self.first.insert(id.to_owned(), usage);
        self.order.push_back(id.to_owned());
        while self.order.len() > SEEN_IDS_MAX {
            if let Some(old) = self.order.pop_front() {
                self.first.remove(&old);
            }
        }
        Seen::New
    }
}

/// Spend per model, folded one transcript line at a time. Every count below
/// is exposed so a view can say what it did NOT count.
#[derive(Debug, Clone, Default)]
pub struct TranscriptUsage {
    per_model: BTreeMap<String, ModelSpend>,
    seen: SeenIds,
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
    /// Repeats whose usage differed from the first; the first was kept.
    pub conflicting_repeats: u64,
    /// Assistant rows with `isSidechain: true`; summed like any other.
    pub sidechain_rows: u64,
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

    /// The sum over every model.
    pub fn total(&self) -> ModelSpend {
        let mut t = ModelSpend::default();
        for s in self.per_model.values() {
            t.add_spend(s);
        }
        t
    }

    /// Fold one line. Never panics; a hostile line is [`Fold::Unparsed`] or
    /// [`Fold::TooLong`].
    ///
    /// A line [`cannot_be_assistant`] rejects is never handed to the JSON
    /// parser — see that function for what the cut costs and what it saves.
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
        let Ok(value) = aterm_json::from_str::<Value>(line) else {
            self.rows_unparsed = self.rows_unparsed.saturating_add(1);
            return Fold::Unparsed;
        };
        let Some(obj) = value.as_object() else {
            self.rows_unparsed = self.rows_unparsed.saturating_add(1);
            return Fold::Unparsed;
        };
        if obj.get("type").and_then(Value::as_str) != Some("assistant") {
            return Fold::NotAssistant;
        }
        self.assistant_rows = self.assistant_rows.saturating_add(1);
        if obj.get("isSidechain").and_then(Value::as_bool) == Some(true) {
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
        let Some(id) = id else {
            self.rows_without_id = self.rows_without_id.saturating_add(1);
            self.add(model, usage);
            return Fold::Summed;
        };
        match self.seen.remember(id, usage) {
            Seen::New => {
                self.add(model, usage);
                Fold::Summed
            }
            Seen::Same => {
                self.duplicates = self.duplicates.saturating_add(1);
                Fold::Duplicate
            }
            Seen::Differs => {
                self.duplicates = self.duplicates.saturating_add(1);
                self.conflicting_repeats = self.conflicting_repeats.saturating_add(1);
                Fold::Conflict
            }
        }
    }

    /// Fold every line of a reader, streaming: a line past [`MAX_LINE_BYTES`]
    /// is discarded as it arrives and counted, never held. Stops at the first
    /// read error, keeping what was folded before it.
    pub fn fold_reader<R: BufRead>(&mut self, mut reader: R) -> io::Result<()> {
        let mut line: Vec<u8> = Vec::new();
        let mut overflow = false;
        loop {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                if overflow {
                    self.rows = self.rows.saturating_add(1);
                    self.rows_skipped_long = self.rows_skipped_long.saturating_add(1);
                } else if !line.is_empty() {
                    self.fold_bytes(&line);
                }
                return Ok(());
            }
            let (chunk, newline) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (&buf[..i], true),
                None => (buf, false),
            };
            let used = chunk.len() + usize::from(newline);
            if !overflow {
                if line.len().saturating_add(chunk.len()) > MAX_LINE_BYTES {
                    overflow = true;
                    line.clear();
                } else {
                    line.extend_from_slice(chunk);
                }
            }
            reader.consume(used);
            if newline {
                if overflow {
                    self.rows = self.rows.saturating_add(1);
                    self.rows_skipped_long = self.rows_skipped_long.saturating_add(1);
                    overflow = false;
                } else {
                    self.fold_bytes(&line);
                }
                line.clear();
            }
        }
    }

    fn fold_bytes(&mut self, line: &[u8]) {
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

    fn add(&mut self, model: &str, usage: TokenUsage) {
        let key = if self.per_model.contains_key(model) || self.per_model.len() < MAX_MODELS {
            model
        } else {
            OTHER_MODEL
        };
        self.per_model.entry(key.to_owned()).or_default().add(usage);
    }
}

/// Whether this line PROVABLY is not an assistant row, decided on the raw
/// bytes so the JSON parser never sees it.
///
/// A transcript is mostly rows this fold does not want. MEASURED 2026-09-22
/// over a real 16,228,604-byte transcript (9,772 rows): 6,294 rows — 8,071,052
/// bytes, half the file — are non-assistant, and building a `Value` tree for
/// each one was half the fold's whole cost. MEASURED with and without the cut,
/// same binary, best of 7 in release: that file folds in 34.06 ms without it
/// and 21.33 ms with it (-37%), and a 222,717-byte one in 0.31 ms against
/// 0.24 ms.
///
/// The cut is SOUND for the question it answers. A JSON string whose value is
/// `assistant` either carries those nine bytes literally or spells at least one
/// of them with a `\u` escape — no other JSON escape produces a letter — so a
/// line holding neither `assistant` nor `\u` cannot say `"type":"assistant"`
/// however it is written. MEASURED on the same file: 8 of those 6,294 rows
/// carry a `\u` and are parsed as before.
///
/// What it COSTS, stated rather than hidden: a line that is object-shaped and
/// malformed now answers [`Fold::NotAssistant`] instead of [`Fold::Unparsed`],
/// because nothing parsed it. That is why the object shape is part of the test
/// — junk that is not `{…}` still reaches the parser and is still counted
/// unparsed — and why the one corruption that loses spend, a TORN ASSISTANT
/// ROW, is unaffected: it carries the word `assistant` and is parsed, and
/// counted, exactly as before.
fn cannot_be_assistant(line: &str) -> bool {
    line.starts_with('{')
        && line.ends_with('}')
        && !line.contains("assistant")
        && !line.contains("\\u")
}

// ---------------------------------------------------------------------------
// Prices — injected, never built in
// ---------------------------------------------------------------------------

/// Dollars per million tokens for one model, in the four token classes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelPrice {
    /// Per million input tokens.
    pub input: f64,
    /// Per million output tokens.
    pub output: f64,
    /// Per million cache-creation tokens.
    pub cache_write: f64,
    /// Per million cache-read tokens.
    pub cache_read: f64,
}

impl ModelPrice {
    fn is_valid(&self) -> bool {
        [self.input, self.output, self.cache_write, self.cache_read]
            .iter()
            .all(|p| p.is_finite() && *p >= 0.0)
    }
}

/// Prices by exact model id. Empty unless the caller fills it: nothing here
/// knows what any model costs, and a model with no row prices as `None`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PriceTable {
    rows: BTreeMap<String, ModelPrice>,
}

impl PriceTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the price of one model id (exact match, no prefix or family rule).
    pub fn insert(&mut self, model: impl Into<String>, price: ModelPrice) {
        self.rows.insert(model.into(), price);
    }

    /// Builder form of [`insert`](Self::insert).
    pub fn with(mut self, model: impl Into<String>, price: ModelPrice) -> Self {
        self.insert(model, price);
        self
    }

    /// The price row for a model id, if the caller supplied one.
    pub fn get(&self, model: &str) -> Option<&ModelPrice> {
        self.rows.get(model)
    }

    /// Dollars for a spend, or `None` when the model has no row or the row
    /// holds a negative or non-finite price.
    pub fn usd(&self, model: &str, spend: &ModelSpend) -> Option<f64> {
        let p = self.get(model)?;
        if !p.is_valid() {
            return None;
        }
        let per = |tokens: u64, price: f64| tokens as f64 * price / 1_000_000.0;
        Some(
            per(spend.input, p.input)
                + per(spend.output, p.output)
                + per(spend.cache_write, p.cache_write)
                + per(spend.cache_read, p.cache_read),
        )
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// One window of one account, as the view shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowView {
    /// Percent used, as reported; `None` when unknown. Not clamped: the
    /// `spend_limit` window reads above 100 once exceeded.
    pub used_pct: Option<f64>,
    /// Epoch seconds at which the window resets, when reported.
    pub resets_at: Option<i64>,
    /// Which input said so.
    pub source: Source,
    /// Seconds between that input's sample and `as_of`, when known.
    pub age_s: Option<u64>,
}

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
    /// Dollars, when the price table had the model; `None` otherwise.
    pub usd: Option<f64>,
}

/// One account: its windows (account-wide), its spend (per model), the model
/// its live session runs.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    /// The owner's label for the account (`accounts.toml`).
    pub label: String,
    /// The config directory, when the caller wants it shown.
    pub dir: Option<String>,
    /// Whether this is the account the live session runs under.
    pub active: bool,
    /// Windows by the vendor's name.
    pub windows: BTreeMap<String, WindowView>,
    /// Spend by model id.
    pub spend: BTreeMap<String, SpendView>,
    /// The live session's model id, from the statusLine.
    pub model: Option<String>,
}

impl AccountView {
    /// An account with nothing known about it yet.
    pub fn new(label: impl Into<String>, active: bool) -> Self {
        Self {
            label: label.into(),
            dir: None,
            active,
            windows: BTreeMap::new(),
            spend: BTreeMap::new(),
            model: None,
        }
    }

    /// Add a window, keeping the higher-authority source when the name is
    /// already present; at equal authority the later one wins.
    pub fn insert_window(&mut self, name: impl Into<String>, window: WindowView) {
        let name = name.into();
        let keep_existing = self
            .windows
            .get(&name)
            .is_some_and(|have| have.source.figure_authority() > window.source.figure_authority());
        if !keep_existing {
            self.windows.insert(name, window);
        }
    }

    /// Take the windows and the model from a statusLine sampled `age_s`
    /// seconds before `as_of`.
    pub fn add_statusline(&mut self, line: &StatusLine, age_s: u64) {
        if let Some(id) = line.model_id() {
            self.model = Some(id.to_owned());
        }
        for (name, w) in windows_from_statusline(line, age_s) {
            self.insert_window(name, w);
        }
    }

    /// Add one window read from the vendor's on-disk cache.
    pub fn add_cache_window(
        &mut self,
        name: impl Into<String>,
        used_pct: Option<f64>,
        resets_at: Option<i64>,
        age_s: Option<u64>,
    ) {
        self.insert_window(
            name,
            WindowView {
                used_pct,
                resets_at,
                source: Source::Cache,
                age_s,
            },
        );
    }

    /// Take the windows Claude Code PAINTED on its `/usage` panel, read off
    /// aterm's own grid ([`usage_panel_windows`]).
    ///
    /// `place` turns the painted reset text into epoch seconds; it is
    /// injected because no clock is read here. `age_s` is the age of the
    /// grid read, which is the honest age of a painted figure.
    ///
    /// **TARGET — no production caller.** The shipped fold of the same
    /// panel is [`super::limits::Evidence::windows_from_screen`], read by
    /// `watch::Watcher::fold_sample` straight off the gated grid read; this
    /// one is the `harness usage` view's half and is reached only by this
    /// module's tests. Two folds of one fact, and the documented one is not
    /// the shipped one — said here rather than left to be discovered.
    ///
    /// These land at [`Source::Grid`], the FLOOR for authority: a
    /// statusLine, cache or transcript figure for the same window keeps its
    /// place ([`AccountView::insert_window`]). What the grid adds is the
    /// window no other source carries — `seven_day_overage_included`, the
    /// Fable bucket design §5.8.2 records as header-only.
    pub fn add_panel_windows(
        &mut self,
        panel: &[PanelWindow],
        place: impl Fn(&str) -> Option<i64>,
        age_s: Option<u64>,
    ) {
        for w in panel {
            self.insert_window(
                w.name.clone(),
                WindowView {
                    used_pct: Some(f64::from(w.used_pct)),
                    resets_at: w.reset_text.as_deref().and_then(&place),
                    source: Source::Grid,
                    age_s,
                },
            );
        }
    }

    /// Take the per-model spend from a transcript fold, priced by `prices`.
    pub fn add_transcript(&mut self, usage: &TranscriptUsage, prices: &PriceTable) {
        for (model, s) in spend_from_transcript(usage, prices) {
            self.spend.insert(model, s);
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

/// The statusLine's windows as view windows, every one `source=statusline`.
pub fn windows_from_statusline(line: &StatusLine, age_s: u64) -> BTreeMap<String, WindowView> {
    line.rate_limits
        .as_ref()
        .map(|rl| {
            rl.windows
                .iter()
                .map(|(name, w)| {
                    (
                        name.clone(),
                        WindowView {
                            used_pct: w.used_pct,
                            resets_at: w.resets_at,
                            source: Source::StatusLine,
                            age_s: Some(age_s),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The `/usage` panel, read off aterm's own grid (design §5.2 "source 0")
// ---------------------------------------------------------------------------

/// One rate-limit window the vendor PAINTED on its `/usage` panel, before its
/// reset text is placed on a clock.
///
/// This is the producer design §5.8.1 rank 1 always described and nothing
/// built: [`Source::Grid`] was admissible and rankable, and no reader
/// could construct one. What the vendor prints was MEASURED twice against
/// Claude Code 2.1.278 on 2026-09-22 — once by extracting the renderer from
/// the installed binary, and once by driving a real session and reading the
/// screen back over aterm's own control socket.
///
/// **The footer carries no quota figure, and saying so is the finding.** The
/// footer's only percentage is the CONTEXT indicator
/// (`aterm_phase::context_left`: `7% until auto-compact`), which is not a
/// quota; a captured idle footer read `⏵⏵ auto mode on (shift+tab to cycle) ·
/// ← for agents` and nothing else. A limit BANNER
/// (`aterm_phase::limit_notice`) carries a reset and an exhausted-yes and
/// never a percentage. The one surface that paints a quota figure is the
/// `/usage` panel, and this is its reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelWindow {
    /// The statusLine / cache key this row names.
    pub name: String,
    /// The whole-number percent the vendor printed (`7% used` → 7). The
    /// vendor floors its own figure (`Math.floor(utilization)`, MEASURED), so
    /// this is the painted integer and never a re-derivation of it.
    pub used_pct: u32,
    /// The text after `Resets `, exactly as painted, NOT placed on a clock:
    /// this module reads no clock. `None` when the row carried no reset.
    pub reset_text: Option<String>,
}

/// The panel titles whose window key is MEASURED, and the key each names.
///
/// `Current week (Fable)` is the row that matters most: design §5.8.2 records
/// the Fable window as `seven_day_overage_included`, delivered only through
/// the `anthropic-ratelimit-unified-*` headers — "not in the statusLine
/// `rate_limits` schema and not a `cachedUsageUtilization` key … so the HUD
/// cannot show it". The vendor PAINTS it (captured 2026-09-22:
/// `Current week (Fable)` at `100% used`), and the vendor's own label map
/// spells that bucket `seven_day_overage_included:"Fable limit"`. So the grid
/// is not merely a fallback for figures the statusLine also carries — it is
/// the only readable source for that one.
const PANEL_TITLES: [(&str, &str); 5] = [
    ("Current session", "five_hour"),
    ("Current week (all models)", "seven_day"),
    ("Current week (Sonnet only)", "seven_day_sonnet"),
    ("Current week (Fable)", "seven_day_overage_included"),
    ("Spend limit", "spend_limit"),
];

/// How many rows past a title row the reader looks for that window's figure.
/// MEASURED: the wide layout (panel width ≥ 62) needs 2 — title, then
/// `<bar> N% used`, then `Resets …`; the narrow layout needs 5 — title with
/// the reset wrapped onto the next row, the bar (which itself wraps at
/// 100 %), a blank, then `N% used`.
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
/// an UNMEASURED `Current week (<display name>)` title.
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

/// The window key a panel title names, and what the title row carries AFTER
/// it (the narrow layout's ` · Resets …`, or the copied text form's
/// `: 62% used · resets …`).
///
/// A `Current week (<x>)` whose display name is not one of the measured ones
/// mints `seven_day_<slug>` — UNVERIFIED as a vendor key, and deliberately
/// not a guess that could land on a measured one, because every measured key
/// is matched first.
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

/// The rate-limit windows Claude Code PAINTED on its `/usage` panel, read off
/// the rows of aterm's own parsed grid.
///
/// Both layouts are MEASURED (Claude Code 2.1.278, 2026-09-22, captured over
/// the control socket). Wide (panel ≥ 62 columns):
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
/// The copied text form the panel also renders
/// (`Current session: 7% used · resets 1:20pm`) is read by the same path,
/// because the title row's remainder is parsed exactly as the narrow layout's
/// is.
///
/// **FENCE: fewer than two windows reads as none.** The panel renders
/// `Current session` and `Current week (all models)` together whenever the
/// vendor has figures at all (MEASURED from the renderer: both rows are built
/// unconditionally and dropped only on a null utilization), so a lone title —
/// a worker quoting one line, a peer's screen pasted into a tool result — is
/// refused rather than believed. The BOUND, stated rather than hidden: a
/// quoted copy of two or more whole rows would still be read, and this reader
/// has no way to tell that copy from the panel. It is a screen read, ranked
/// where design §5.8.1 ranks one, and it is never OBEYED — the text is
/// evidence about what was drawn and never an instruction.
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

/// The fold's per-model totals as spend rows, priced where the table allows.
pub fn spend_from_transcript(
    usage: &TranscriptUsage,
    prices: &PriceTable,
) -> BTreeMap<String, SpendView> {
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
                    usd: prices.usd(model, s),
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
/// field, and the question of what breaks a line is
/// [`crate::supervise::limit::breaks_a_line`] — not `char::is_control()`,
/// which is Cc-only and let `U+2028` through this sweep into that field.
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

/// `HH:MM` of an epoch instant in the zone `utc_offset_s` east of UTC.
pub fn hhmm(epoch: i64, utc_offset_s: i64) -> String {
    let s = epoch.saturating_add(utc_offset_s).rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3_600, (s % 3_600) / 60)
}

/// A short age: `12s`, `5m`, `3h`, `2d`.
pub fn age_short(age_s: u64) -> String {
    match age_s {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
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

fn pct_text(w: Option<&WindowView>) -> String {
    match w.and_then(|w| w.used_pct) {
        Some(p) => format!("{}%", p.round() as i64),
        None => "?%".to_owned(),
    }
}

/// The one-line HUD: `fable 62%/5h · 18%/7d · acct work · resets 15:45`.
///
/// The model is the shown account's live model family; the two percentages are
/// that ACCOUNT's `five_hour` and `seven_day` windows (`?%` when unknown, never
/// clamped); `resets` is the five-hour reset, else the seven-day one, in the
/// zone `utc_offset_s` east of UTC (`?` when neither is known). When the
/// windows shown did not come from the live statusLine the line ends with the
/// source and its age (`· cache 1h`, `· none`), so a stale figure never reads
/// as live. Cut to [`HUD_MAX_BYTES`] on a char boundary.
pub fn hud_line(view: &UsageView, utc_offset_s: i64) -> String {
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
    let resets = five
        .and_then(|w| w.resets_at)
        .or_else(|| seven.and_then(|w| w.resets_at))
        .map_or_else(|| "?".to_owned(), |t| hhmm(t, utc_offset_s));
    let mut line = format!(
        "{model} {}/5h · {}/7d · acct {} · resets {resets}",
        pct_text(five),
        pct_text(seven),
        sanitize(&acct.label)
    );
    match five.or(seven) {
        Some(w) if w.source != Source::StatusLine => {
            line.push_str(" · ");
            line.push_str(w.source.as_str());
            if let Some(age) = w.age_s {
                line.push(' ');
                line.push_str(&age_short(age));
            }
        }
        Some(_) => {}
        None => line.push_str(" · none"),
    }
    truncate_to(line, HUD_MAX_BYTES)
}

// ---------------------------------------------------------------------------
// `harness usage --json`, schema 1
// ---------------------------------------------------------------------------

/// Proleptic-Gregorian civil date of a day count from 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDTHH:MM:SSZ` of an epoch instant.
pub fn rfc3339_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

fn opt_f64(v: Option<f64>) -> Value {
    v.filter(|f| f.is_finite()).map_or(Value::Null, Value::from)
}

fn opt_u64(v: Option<u64>) -> Value {
    v.map_or(Value::Null, Value::from)
}

fn window_json(w: &WindowView) -> Value {
    let mut o = Map::new();
    o.insert("used_pct".to_owned(), opt_f64(w.used_pct));
    if let Some(t) = w.resets_at {
        o.insert("resets_at".to_owned(), Value::from(t));
    }
    o.insert("source".to_owned(), Value::from(w.source.as_str()));
    o.insert("age_s".to_owned(), opt_u64(w.age_s));
    Value::Object(o)
}

fn spend_json(s: &SpendView) -> Value {
    let mut o = Map::new();
    o.insert("in".to_owned(), Value::from(s.input));
    o.insert("out".to_owned(), Value::from(s.output));
    o.insert("cache_write".to_owned(), Value::from(s.cache_write));
    o.insert("cache_read".to_owned(), Value::from(s.cache_read));
    o.insert(
        "usd".to_owned(),
        opt_f64(s.usd.map(|u| (u * 10_000.0).round() / 10_000.0)),
    );
    Value::Object(o)
}

fn account_json(a: &AccountView) -> Value {
    let mut o = Map::new();
    o.insert("label".to_owned(), Value::from(a.label.as_str()));
    if let Some(dir) = &a.dir {
        o.insert("dir".to_owned(), Value::from(dir.as_str()));
    }
    o.insert("active".to_owned(), Value::from(a.active));
    o.insert(
        "windows".to_owned(),
        Value::Object(
            a.windows
                .iter()
                .map(|(k, w)| (k.clone(), window_json(w)))
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

/// The `harness usage --json` document, schema 1 (design §5.2). Keys are
/// emitted in sorted order (the writer's rule). `identity` and `failover` are
/// not this slice's to know and are absent; `sheet` is the fixed
/// `{enabled:false,last_sync:null,last_error:null}` until Sheets sync exists.
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
    use std::fs;
    use std::io::{BufReader, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // -- fixtures -----------------------------------------------------------

    /// The vendor's statusLine shape (field names as its help text documents
    /// them, 2.1.274), with integers and floats mixed on purpose.
    const STATUSLINE: &str = r#"{
      "session_id":"s-7f3","session_name":"wrapper","transcript_path":"/Users//x/.claude/projects/p/s-7f3.jsonl",
      "cwd":"/work/proj","model":{"id":"claude-fable-5-1","display_name":"Fable 5.1"},
      "workspace":{"current_dir":"/work/proj","project_dir":"/work","added_dirs":[]},
      "version":"2.1.274",
      "context_window":{"total_input_tokens":1204000,"total_output_tokens":88000,"context_window_size":200000,
        "current_usage":{"input_tokens":1500,"output_tokens":20,"cache_creation_input_tokens":300,"cache_read_input_tokens":90000},
        "used_percentage":46.5,"remaining_percentage":53.5},
      "effort":{"level":"high"},"thinking":{"enabled":true},
      "rate_limits":{"five_hour":{"used_percentage":62,"resets_at":1789669500},
                     "seven_day":{"used_percentage":18.4,"resets_at":1790000000.0},
                     "spend_limit":{"used_percentage":140,"resets_at":1790500000}},
      "prompt_cache":{"hits":3},"a_field_from_next_week":{"x":1}
    }"#;

    /// The `/usage` panel exactly as Claude Code 2.1.278 painted it in a
    /// 120-column session, captured over aterm's control socket 2026-09-22.
    /// The leading three-space indent is the vendor's, kept verbatim.
    const PANEL_WIDE: &str = "\
   Settings  Status   Config   Usage   Stats

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
    const PANEL_NARROW: &str = "\
   Current session · Resets 1:20pm
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

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-harness-usage-{label}-{}-{nonce}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create the scratch root");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn view_with(acct: AccountView) -> UsageView {
        let mut v = UsageView::new(1_789_650_000);
        v.accounts.push(acct);
        v
    }

    fn live_account() -> AccountView {
        let line = parse_statusline(STATUSLINE).expect("fixture parses");
        let mut a = AccountView::new("work", true);
        a.add_statusline(&line, 12);
        a
    }

    // -- parse_statusline ---------------------------------------------------

    #[test]
    fn statusline_full_fixture_reads_every_documented_field() {
        let s = parse_statusline(STATUSLINE).expect("parses");
        assert_eq!(s.session_id.as_deref(), Some("s-7f3"));
        assert_eq!(s.session_name.as_deref(), Some("wrapper"));
        assert_eq!(s.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(s.version.as_deref(), Some("2.1.274"));
        assert_eq!(s.model_id(), Some("claude-fable-5-1"));
        assert_eq!(
            s.model.as_ref().and_then(|m| m.display_name.as_deref()),
            Some("Fable 5.1")
        );
        assert_eq!(
            s.workspace.as_ref().and_then(|w| w.project_dir.as_deref()),
            Some("/work")
        );
        let cw = s.context_window.as_ref().expect("context window");
        assert_eq!(cw.total_input_tokens, Some(1_204_000));
        assert_eq!(cw.context_window_size, Some(200_000));
        assert_eq!(cw.used_percentage, Some(46.5));
        assert_eq!(
            cw.current_usage,
            Some(TokenUsage {
                input: 1500,
                output: 20,
                cache_write: 300,
                cache_read: 90_000
            })
        );
        assert_eq!(s.effort_level.as_deref(), Some("high"));
        assert_eq!(s.thinking_enabled, Some(true));
        let rl = s.rate_limits.as_ref().expect("rate limits");
        assert_eq!(rl.five_hour().and_then(|w| w.used_pct), Some(62.0));
        assert_eq!(
            rl.five_hour().and_then(|w| w.resets_at),
            Some(1_789_669_500)
        );
        // Float and integer are both numbers here.
        assert_eq!(rl.seven_day().and_then(|w| w.used_pct), Some(18.4));
        assert_eq!(
            rl.seven_day().and_then(|w| w.resets_at),
            Some(1_790_000_000)
        );
    }

    #[test]
    fn statusline_malformed_json_is_an_error_not_a_panic() {
        for bad in [
            "",
            "{",
            "{\"model\":}",
            "not json at all",
            "{\"a\":1} trailing",
            "{\"s\":\"\\ud800\"}",
        ] {
            assert!(
                matches!(parse_statusline(bad), Err(UsageError::Json(_))),
                "{bad:?} should be a Json error"
            );
        }
    }

    #[test]
    fn statusline_top_level_must_be_an_object() {
        assert_eq!(parse_statusline("[1,2,3]"), Err(UsageError::NotObject));
        assert_eq!(parse_statusline("42"), Err(UsageError::NotObject));
        assert_eq!(parse_statusline("null"), Err(UsageError::NotObject));
    }

    #[test]
    fn statusline_missing_rate_limits_reads_none_and_yields_no_windows() {
        let s = parse_statusline(r#"{"model":{"id":"claude-opus-5"},"version":"2.1.274"}"#)
            .expect("parses");
        assert!(s.rate_limits.is_none());
        assert!(windows_from_statusline(&s, 0).is_empty());
        // `rate_limits` that is not an object reads the same as absent.
        let s = parse_statusline(r#"{"rate_limits":"soon"}"#).expect("parses");
        assert!(s.rate_limits.is_none());
        // Present but empty: Some, with no windows — the API reported nothing.
        let s = parse_statusline(r#"{"rate_limits":{}}"#).expect("parses");
        assert!(s.rate_limits.as_ref().is_some_and(|r| r.windows.is_empty()));
    }

    #[test]
    fn statusline_window_above_100_percent_is_kept_not_clamped() {
        let s = parse_statusline(STATUSLINE).expect("parses");
        let rl = s.rate_limits.as_ref().expect("rate limits");
        assert_eq!(rl.spend_limit().and_then(|w| w.used_pct), Some(140.0));
        let w = windows_from_statusline(&s, 5);
        assert_eq!(w["spend_limit"].used_pct, Some(140.0));
        assert_eq!(w["spend_limit"].source, Source::StatusLine);
        assert_eq!(w["spend_limit"].age_s, Some(5));
    }

    #[test]
    fn statusline_every_field_is_optional_and_wrong_types_read_as_absent() {
        let s = parse_statusline("{}").expect("parses");
        assert_eq!(s, StatusLine::default());
        let s = parse_statusline(
            r#"{"model":"claude-fable-5-1","context_window":[],"effort":{"level":7},
                "thinking":{"enabled":"yes"},
                "rate_limits":{"five_hour":{"used_percentage":"62","resets_at":null},
                               "seven_day":17,"spend_limit":{"used_percentage":-3}}}"#,
        )
        .expect("parses");
        assert!(s.model.is_none());
        assert!(s.context_window.is_none());
        assert!(s.effort_level.is_none());
        assert!(s.thinking_enabled.is_none());
        let rl = s.rate_limits.as_ref().expect("rate limits");
        // A string percentage is not a number; the window itself still exists.
        assert_eq!(rl.five_hour().map(|w| w.used_pct), Some(None));
        assert!(rl.seven_day().is_none(), "a non-object window is dropped");
        // A negative percentage is kept: it is what was reported.
        assert_eq!(rl.spend_limit().and_then(|w| w.used_pct), Some(-3.0));
    }

    #[test]
    fn statusline_unknown_windows_are_kept_and_the_count_is_bounded() {
        let s = parse_statusline(
            r#"{"rate_limits":{"seven_day_opus":{"used_percentage":null},"five_hour":{"used_percentage":1}}}"#,
        )
        .expect("parses");
        let rl = s.rate_limits.as_ref().expect("rate limits");
        assert_eq!(rl.windows.len(), 2);
        assert_eq!(rl.windows["seven_day_opus"].used_pct, None);

        let many: Vec<String> = (0..(MAX_WINDOWS + 10))
            .map(|i| format!(r#""w{i:03}":{{"used_percentage":{i}}}"#))
            .collect();
        let doc = format!(r#"{{"rate_limits":{{{}}}}}"#, many.join(","));
        let s = parse_statusline(&doc).expect("parses");
        assert_eq!(
            s.rate_limits.as_ref().map(|r| r.windows.len()),
            Some(MAX_WINDOWS)
        );
    }

    #[test]
    fn statusline_hostile_nesting_and_size_are_errors() {
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(parse_statusline(&deep), Err(UsageError::Json(_))));
        let deep_obj = format!("{}1{}", r#"{"a":"#.repeat(5_000), "}".repeat(5_000));
        assert!(matches!(
            parse_statusline(&deep_obj),
            Err(UsageError::Json(_))
        ));
        let huge = format!(r#"{{"pad":"{}"}}"#, "x".repeat(MAX_STATUSLINE_BYTES));
        assert!(matches!(
            parse_statusline(&huge),
            Err(UsageError::TooLong {
                max: MAX_STATUSLINE_BYTES,
                ..
            })
        ));
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
        assert_eq!(num_i64(v.get("neg")), Some(-1));
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
        // The prefilter's whole risk is an assistant row it does not recognise.
        // NEGATIVE CONTROL: the type value spelled with a `\u` escape is legal
        // JSON, carries the word nowhere, and must still be summed.
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

        // And the rows it may NOT skip, each for its own reason.
        for parsed in [
            r#"{"type":"assistant","message":{}}"#, // says the word
            r#"{"type":"user","text":"\u00e9"}"#,   // carries an escape
            r#"["type","user"]"#,                   // not object-shaped
            r#"{"type":"user""#,                    // torn: no closing brace
        ] {
            assert!(!cannot_be_assistant(parsed), "{parsed}");
        }

        // WHAT THE CUT COSTS, pinned rather than left to be discovered: a torn
        // ASSISTANT row still counts unparsed (it says the word), while a torn
        // object-shaped row that cannot be one now counts NotAssistant, because
        // nothing parsed it.
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
        // The longest real row measured (1,125,606 bytes) is well inside.
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

    #[test]
    fn transcript_conflicting_repeat_keeps_the_first_and_counts_it() {
        let mut t = TranscriptUsage::new();
        let first = assistant_row("msg_c", "m", 100, 10);
        let differs = assistant_row("msg_c", "m", 100, 999);
        assert_eq!(t.fold_line(&first), Fold::Summed);
        assert_eq!(t.fold_line(&differs), Fold::Conflict);
        assert_eq!(t.per_model()["m"].output, 10);
        assert_eq!(t.conflicting_repeats, 1);
        assert_eq!(t.duplicates, 1);
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
    fn transcript_fold_reader_streams_a_file_past_an_oversize_line() {
        let dir = TestDir::new("reader");
        let path = dir.0.join("session.jsonl");
        {
            let mut f = fs::File::create(&path).expect("create");
            writeln!(f, "{}", assistant_row("msg_1", "m", 10, 1)).expect("write");
            writeln!(f, "{}", assistant_row("msg_1", "m", 10, 1)).expect("write");
            write!(f, "{{\"type\":\"assistant\",\"pad\":\"").expect("write");
            // Written in chunks so the reader sees it across many fill_buf calls.
            let chunk = "y".repeat(64 * 1024);
            for _ in 0..(MAX_LINE_BYTES / chunk.len() + 1) {
                f.write_all(chunk.as_bytes()).expect("write");
            }
            writeln!(f, "\"}}").expect("write");
            f.write_all(b"\xff\xfe not utf8\n").expect("write");
            writeln!(f, "{}", assistant_row("msg_2", "m", 5, 2)).expect("write");
            // Last line without a trailing newline still folds.
            write!(f, "{}", assistant_row("msg_3", "m", 1, 1)).expect("write");
        }
        let mut t = TranscriptUsage::new();
        let file = fs::File::open(&path).expect("open");
        t.fold_reader(BufReader::with_capacity(8 * 1024, file))
            .expect("fold");
        assert_eq!(t.rows, 6);
        assert_eq!(t.rows_skipped_long, 1);
        assert_eq!(t.rows_unparsed, 1);
        assert_eq!(t.duplicates, 1);
        let s = t.per_model()["m"];
        assert_eq!((s.input, s.output, s.messages), (16, 4, 3));
    }

    #[test]
    fn transcript_fold_reader_empty_and_newline_only_inputs() {
        let mut t = TranscriptUsage::new();
        t.fold_reader(BufReader::new(&b""[..])).expect("fold");
        assert_eq!(t.rows, 0);
        t.fold_reader(BufReader::new(&b"\n\n"[..])).expect("fold");
        assert_eq!(t.rows, 2);
        assert_eq!(t.rows_unparsed, 0, "blank lines are not errors");
    }

    // -- PriceTable ---------------------------------------------------------

    #[test]
    fn price_table_prices_only_what_the_caller_supplied() {
        let spend = ModelSpend {
            input: 1_000_000,
            output: 500_000,
            cache_write: 200_000,
            cache_read: 4_000_000,
            messages: 9,
        };
        let empty = PriceTable::new();
        assert_eq!(
            empty.usd("claude-fable-5-1", &spend),
            None,
            "no built-in prices"
        );
        let table = PriceTable::new().with(
            "claude-fable-5-1",
            ModelPrice {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.3,
            },
        );
        let usd = table.usd("claude-fable-5-1", &spend).expect("priced");
        // 3 + 7.5 + 0.75 + 1.2
        assert!((usd - 12.45).abs() < 1e-9, "{usd}");
        assert_eq!(table.usd("claude-fable-5", &spend), None, "exact id only");
        let bad = PriceTable::new().with(
            "m",
            ModelPrice {
                input: -1.0,
                ..ModelPrice::default()
            },
        );
        assert_eq!(
            bad.usd("m", &spend),
            None,
            "a negative price is not a price"
        );
        let nan = PriceTable::new().with(
            "m",
            ModelPrice {
                output: f64::NAN,
                ..ModelPrice::default()
            },
        );
        assert_eq!(nan.usd("m", &spend), None);
    }

    #[test]
    fn spend_from_transcript_carries_tokens_and_prices_where_known() {
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 1_000_000, 0),
                &assistant_row("msg_b", "claude-opus-5", 10, 0),
            ],
        );
        let prices = PriceTable::new().with(
            "claude-fable-5-1",
            ModelPrice {
                input: 2.0,
                ..ModelPrice::default()
            },
        );
        let spend = spend_from_transcript(&t, &prices);
        assert_eq!(spend["claude-fable-5-1"].input, 1_000_000);
        assert_eq!(spend["claude-fable-5-1"].cache_read, 1000);
        // Only the input price was supplied; the cache tokens price at 0.
        assert!((spend["claude-fable-5-1"].usd.expect("priced") - 2.0).abs() < 1e-9);
        assert_eq!(
            spend["claude-opus-5"].usd, None,
            "unpriced is unknown, not zero"
        );
    }

    // -- the view -----------------------------------------------------------

    #[test]
    fn account_window_authority_statusline_over_cache_over_transcript() {
        let mut a = AccountView::new("work", true);
        a.add_cache_window("five_hour", Some(5.0), None, Some(3600));
        assert_eq!(a.windows["five_hour"].source, Source::Cache);
        let line = parse_statusline(STATUSLINE).expect("parses");
        a.add_statusline(&line, 12);
        assert_eq!(a.windows["five_hour"].source, Source::StatusLine);
        assert_eq!(a.windows["five_hour"].used_pct, Some(62.0));
        assert_eq!(a.model.as_deref(), Some("claude-fable-5-1"));
        // A later cache sample does not displace the live figure.
        a.add_cache_window("five_hour", Some(99.0), None, Some(1));
        assert_eq!(a.windows["five_hour"].used_pct, Some(62.0));
        // But it does fill a window the statusLine did not carry.
        a.add_cache_window("seven_day_opus", None, None, Some(3600));
        assert_eq!(a.windows["seven_day_opus"].source, Source::Cache);
        // A transcript-sourced window never displaces cache.
        a.insert_window(
            "seven_day_opus",
            WindowView {
                used_pct: Some(1.0),
                resets_at: None,
                source: Source::Transcript,
                age_s: None,
            },
        );
        assert_eq!(a.windows["seven_day_opus"].source, Source::Cache);
        // Equal authority: the later sample wins.
        a.add_cache_window("seven_day_opus", Some(2.0), None, Some(10));
        assert_eq!(a.windows["seven_day_opus"].used_pct, Some(2.0));
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

    #[test]
    fn hud_line_happy_path_matches_the_design_shape() {
        // 1789669500 is 2026-09-17 18:25:00 UTC; at +0 the line reads 18:25.
        // (The design's example pairs that instant with "15:45"; the two do
        // not agree in any whole-hour zone, so the instant is the fixture.)
        assert_eq!(
            hud_line(&view_with(live_account()), 0),
            "fable 62%/5h · 18%/7d · acct work · resets 18:25"
        );
    }

    #[test]
    fn hud_line_applies_the_utc_offset_and_wraps_midnight() {
        let v = view_with(live_account());
        assert_eq!(hhmm(1_789_669_500, 0), "18:25");
        assert!(hud_line(&v, -7 * 3600).ends_with("resets 11:25"));
        assert!(hud_line(&v, 6 * 3600).ends_with("resets 00:25"));
        assert!(hud_line(&v, -19 * 3600).ends_with("resets 23:25"));
        assert_eq!(hhmm(0, 0), "00:00");
        assert_eq!(hhmm(-1, 0), "23:59", "before the epoch still wraps");
        assert_eq!(hhmm(i64::MAX, i64::MAX), hhmm(i64::MAX, 0), "saturates");
    }

    #[test]
    fn hud_line_says_unknown_rather_than_inventing_a_number() {
        assert_eq!(hud_line(&UsageView::new(0), 0), "no account");
        let a = AccountView::new("work", true);
        assert_eq!(
            hud_line(&view_with(a), 0),
            "model? ?%/5h · ?%/7d · acct work · resets ? · none"
        );
        // Only the seven-day window known, from cache: its reset is used and
        // the source and age are named.
        let mut a = AccountView::new("alt-1", false);
        a.model = Some("claude-opus-5".to_owned());
        a.add_cache_window("seven_day", Some(18.4), Some(1_790_000_000), Some(86_400));
        assert_eq!(
            hud_line(&view_with(a), 0),
            "opus ?%/5h · 18%/7d · acct alt-1 · resets 14:13 · cache 1d"
        );
    }

    #[test]
    fn hud_line_keeps_a_window_above_100_and_rounds_halves_away() {
        let mut a = AccountView::new("work", true);
        a.model = Some("claude-fable-5-1".to_owned());
        a.add_cache_window("five_hour", Some(250.0), Some(60), None);
        a.add_cache_window("seven_day", Some(0.5), None, None);
        assert_eq!(
            hud_line(&view_with(a), 0),
            "fable 250%/5h · 1%/7d · acct work · resets 00:01 · cache"
        );
    }

    #[test]
    fn hud_line_is_capped_at_1024_bytes_on_a_char_boundary() {
        let mut a = AccountView::new("é".repeat(700), true);
        a.model = Some("claude-fable-5-1".to_owned());
        let line = hud_line(&view_with(a), 0);
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
            let line = hud_line(&view_with(a), 0);
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
        let line = hud_line(&view_with(a), 0);
        assert!(!line.contains('\n') && !line.contains('\x1b') && !line.contains('\t'));
        assert!(line.starts_with("fa ble ?%/5h"));

        // NEGATIVE CONTROL for the shared predicate: `U+2028` LINE SEPARATOR
        // is Zl, so `char::is_control()` is false for it and this sweep passed
        // it into a one-line status field until 2026-09-22.
        assert!(!'\u{2028}'.is_control());
        let split = AccountView::new("wo\u{2028}rk", true);
        let line = hud_line(&view_with(split), 0);
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
        assert_eq!(age_short(12), "12s");
        assert_eq!(age_short(300), "5m");
        assert_eq!(age_short(3_600 * 3 + 59), "3h");
        assert_eq!(age_short(86_400 * 2), "2d");
    }

    // -- usage_json ---------------------------------------------------------

    #[test]
    fn usage_json_is_schema_1_and_round_trips_through_the_parser() {
        let mut work = live_account();
        work.dir = Some("/Users//x/.claude".to_owned());
        work.add_cache_window("seven_day_opus", None, None, Some(3600));
        let mut t = TranscriptUsage::new();
        fold_all(
            &mut t,
            &[
                &assistant_row("msg_a", "claude-fable-5-1", 1_000_000, 0),
                &assistant_row("msg_b", "claude-opus-5", 10, 0),
            ],
        );
        let prices = PriceTable::new().with(
            "claude-fable-5-1",
            ModelPrice {
                input: 3.0,
                output: 15.0,
                cache_write: 3.75,
                cache_read: 0.3,
            },
        );
        work.add_transcript(&t, &prices);
        let mut alt = AccountView::new("alt-1", false);
        alt.add_cache_window("five_hour", Some(5.0), None, Some(86_400));
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
        assert_eq!(w["dir"].as_str(), Some("/Users//x/.claude"));
        assert_eq!(w["active"].as_bool(), Some(true));
        assert_eq!(w["model"].as_str(), Some("claude-fable-5-1"));
        assert_eq!(w["windows"]["five_hour"]["used_pct"].as_f64(), Some(62.0));
        assert_eq!(
            w["windows"]["five_hour"]["resets_at"].as_i64(),
            Some(1_789_669_500)
        );
        assert_eq!(
            w["windows"]["five_hour"]["source"].as_str(),
            Some("statusline")
        );
        assert_eq!(w["windows"]["five_hour"]["age_s"].as_u64(), Some(12));
        assert_eq!(
            w["windows"]["spend_limit"]["used_pct"].as_f64(),
            Some(140.0)
        );
        assert!(w["windows"]["seven_day_opus"]["used_pct"].is_null());
        assert!(w["windows"]["seven_day_opus"].get("resets_at").is_none());
        assert_eq!(
            w["windows"]["seven_day_opus"]["source"].as_str(),
            Some("cache")
        );
        let fable = &w["spend"]["claude-fable-5-1"];
        assert_eq!(fable["in"].as_u64(), Some(1_000_000));
        assert_eq!(fable["out"].as_u64(), Some(0));
        assert_eq!(fable["cache_write"].as_u64(), Some(10));
        assert_eq!(fable["cache_read"].as_u64(), Some(1000));
        // 3.0 + 0 + 0.0000375 + 0.0003 → 3.0003375 → 3.0003 at four places.
        assert_eq!(fable["usd"].as_f64(), Some(3.0003));
        assert!(
            w["spend"]["claude-opus-5"]["usd"].is_null(),
            "unpriced is null"
        );
        let a = &accounts[1];
        assert_eq!(a["active"].as_bool(), Some(false));
        assert!(a.get("model").is_none());
        assert!(a.get("dir").is_none());
        assert_eq!(a["windows"]["five_hour"]["source"].as_str(), Some("cache"));
        assert_eq!(a["windows"]["five_hour"]["age_s"].as_u64(), Some(86_400));
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
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-719_468), (0, 3, 1));
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
                    // The window the statusLine and the cache cannot carry.
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
        // NEGATIVE CONTROL 1: the fence. One row is never a panel, however
        // exactly it is spelled.
        assert!(
            usage_panel_windows(&rows(
                "⏺ my five-hour window says Current session: 99% used\n"
            ))
            .is_empty()
        );
        // NEGATIVE CONTROL 2: titles with no figure painted anywhere.
        assert!(
            usage_panel_windows(&rows(
                "   Current session\n   Resets 1:20pm\n\n   Current week (all models)\n   Resets Sep 23\n"
            ))
            .is_empty()
        );
        // NEGATIVE CONTROL 3: the CONTEXT indicator is not a quota, and the
        // footer this session actually painted carries no figure at all.
        assert!(
            usage_panel_windows(&rows(
                "                                              7% until auto-compact\n\
                 ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents\n"
            ))
            .is_empty()
        );
        // NEGATIVE CONTROL 4: the panel's own trailing lines are percentages
        // of USAGE, not `% used`, and must not be read as windows.
        assert!(
            usage_panel_windows(&rows(
                "   100% of your usage came from subagent-heavy sessions\n\
                    81% of your usage was at >150k context\n"
            ))
            .is_empty()
        );
        // NEGATIVE CONTROL 5: empty input.
        assert!(usage_panel_windows(&[]).is_empty());
    }

    #[test]
    fn panel_reader_never_guesses_a_measured_key_for_an_unmeasured_title() {
        let got = usage_panel_windows(&rows(
            "   Current session\n   ███ 5% used\n   Resets 1pm\n\n\
             \x20  Current week (Haiku 9 Super)\n   ███ 11% used\n   Resets Sep 23\n",
        ));
        assert_eq!(got.len(), 2);
        // The slug is derived and labelled UNVERIFIED; what matters is that
        // it can never collide with a key the reader measured.
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

    #[test]
    fn panel_windows_are_the_floor_for_authority_and_add_what_no_one_else_has() {
        let mut acct = AccountView::new("work", true);
        let line = parse_statusline(STATUSLINE).expect("fixture parses");
        acct.add_statusline(&line, 0);
        acct.add_panel_windows(
            &usage_panel_windows(&rows(PANEL_WIDE)),
            |_| Some(99),
            Some(0),
        );
        // The statusLine's own figure survives a painted one for the same
        // window: grid is rank 1 for ADMISSIBILITY and the floor for
        // AUTHORITY, and both orderings hold here at once.
        assert_eq!(acct.windows["five_hour"].source, Source::StatusLine);
        assert_eq!(acct.windows["five_hour"].used_pct, Some(62.0));
        // And the window no other source carries arrives from the grid.
        let fable = &acct.windows["seven_day_overage_included"];
        assert_eq!(fable.source, Source::Grid);
        assert_eq!(fable.used_pct, Some(100.0));
        assert_eq!(fable.resets_at, Some(99));
    }
}
