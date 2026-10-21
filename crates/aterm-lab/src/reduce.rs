// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab reduce`: A KEY-ONLY REDUCTION of a Claude Code transcript or a Codex
//! rollout (session log Phase 5, step 5H-1: the plan's P11 as ruling T7
//! amends it). It is the shape a lab-read build's rows are committed in
//! (`aterm-agent`'s `harness/testdata/steps/<build>.jsonl`, ruling T8; a
//! Codex rollout's `codex-<build>[-<leg>].jsonl`, session log 5G), and it
//! holds nothing anyone said.
//!
//! KEPT:
//! - every key, in its row's order, but RENAMED `k<n>`, one to one across
//!   the file, where it may be what someone said or named: a key that is no
//!   identifier (a path the vendor keys a map by), and every key inside a
//!   FREE subtree ([`opens_free`]: what a tool takes or returns, what a
//!   schema's author named, what Claude Code sends or attaches for the
//!   model, a map keyed by tracked files). So an MCP tool's own argument
//!   names go, in its calls' input (both copies Claude Code 2.1.303 writes:
//!   the `tool_use` part's and the `wireToolInputs` map's) and in every
//!   tool list it writes (a `prompt_snapshot`'s, an `api-request-shape`'s
//!   and a deferred tool's `tool_addition` part in an `api-request-blob`
//!   row), and so do a question's words keying its answer and an MCP
//!   result's own keys. A key that is an id is REMAPPED as that id (below);
//! - row order, `null`, the booleans, and the numbers as written;
//! - THE CLOSED WORDS A READER READS (ruling U7). A candidate is a word
//!   ([`is_word`]: no space, at most [`WORD_MAX`] bytes) under a key of
//!   [`CLAUDE_WORDS`] or [`CODEX_WORDS`] (`type`, `subtype`, `version`,
//!   `model`, `effort`, a notice's `error` and `apiError`, …), outside a free
//!   subtree; those lists only filter candidates. The COMMITTED READERS
//!   decide which are kept ([`Readings`]: the step folds, each notice as
//!   the wall check reads it, the standing limit, the login wall,
//!   [`FinishedScan`], the spend fold and the limits reader, and every row
//!   as the harness's shapes read it, [`claude_reads`] and [`codex_reads`]):
//!   a `(key, word)` pair is kept only when blanking it changes a reading
//!   ([`Candidates::by_readings`]: the pairs needed alone, then each other
//!   pair put back in the order the file first names them until the
//!   readings are the source's, then each kept pair dropped again while
//!   they stay so), and a file nothing restores is refused. So
//!   `"mode":"normal"` on a row no reader opens goes, a `turn_duration`'s
//!   `subtype` and a limit notice's `error` stay. What the verb writes reads
//!   as its source ([`reads_as`]: the folds, each notice, the standing
//!   limit, the login wall and the limits after every row, times one
//!   shift, ids one to one), and each word it keeps, blanked alone, changes
//!   its readings ([`unread_words`]), or nothing is written
//!   ([`check_written`]).
//!
//! REBASED by whole days, so the first row is dated [`BASE_DAY`]: a row's
//! `timestamp` (its time of day and zone as written, and only when the
//! whole string is one RFC 3339 stamp, [`stamp_day`]: anything after it
//! would be kept, so a string that only begins like one is replaced whole),
//! an epoch number under a time key ([`is_time_key`]: `resetsAt`,
//! `resets_at`, and Codex's
//! `started_at_ms` and `create_time`, a fraction kept as written), and a
//! usage limit notice's reset clause (`resets Oct 11 at 9:35am` → `resets
//! Jan 4 at 9:35am`; its zone dropped).
//!
//! REMAPPED one to one across the file: ids (`uuid`, `promptId`, a call's
//! `id` and its result's `tool_use_id`, `message.id`, `requestId`, …, and any
//! UUID-shaped string), and a KEY that is one of them (a map keyed by a
//! call's id, `wireToolInputs`; a first pass learns the file's ids, so a key
//! is known wherever it sits). Each takes an id the step fold reads exactly
//! as it read the source's: a valid `StepId` stays valid, an invalid one
//! stays invalid, and a lowercase prefix such as `toolu_` is kept.
//!
//! WORDS OF A ROW'S OWN, read by name:
//! - a user row's text keeps only the marker it opens with
//!   (`aterm_agent::harness::steps::USER_MARKERS` and `COMPACT`, the shape
//!   test's own list), and so does a `system` `local_command` row's;
//! - a tool's name stays only when the class table names it
//!   (`steps::tool_class`); an `mcp__` name keeps that prefix. A Codex
//!   call's name (`payload.name` of a `response_item` whose payload is a
//!   `function_call` or a `custom_tool_call`) stays while it is a word with
//!   no `__`; a `name` anywhere else (a `parsed_cmd` read's file) is no
//!   tool's, and is replaced;
//! - CLAUDE CODE'S OWN NOTICE (`isApiErrorMessage`), and a local command's
//!   output that reads as a full context, becomes the CANONICAL PHRASE of
//!   the wall its words read as ([`canonical`]). That phrase is the first of
//!   [`phrases`] (the anchor phrases, `aterm_phase::anchor`, before the
//!   others) whose reading equals the source's: `aterm_phase::classify_wall`
//!   and the three tests `wall_confirm`'s `notice_wall` adds
//!   (`not_a_wall`, `named_by_status_alone`, the `/compact` remedy). A
//!   notice no phrase reads as is refused, never kept.
//!
//! REPLACED by `""`: every other string. Every FREE subtree
//! ([`opens_free`]: a call's input and a result's output, wherever a row
//! or a message nested in a `progress` row holds them, and what a map
//! keyed by an id holds; a JSON schema wherever it sits ([`is_schema`]),
//! and a member whose key ends in `schema`; an `api-request-shape`'s
//! `shape` and each field of an `attachment` but its `type` and `origin`,
//! what Claude Code sends or attaches for the model; a file-history
//! snapshot's `trackedFileBackups`; a Codex call's `arguments`, `input` and
//! `output`) is replaced whole, its keys renamed (KEPT, above). The step
//! fold and the wall check read none of them; `upgrade::FinishedScan`
//! reads a `queued_command`'s `type` and `origin`, which are kept.
//!
//! The reduction opens with one `#` line of its own, as the repository's
//! fixtures do; a `#` line of the source (a fixture's header) is dropped.
//! A source that is itself a reduction (its `#` line is one [`header`]
//! wrote) is said so in the new line, whose day count is then the whole
//! move from the first source (ruling U8), so a reduction reduced again is
//! itself. A line that is not one JSON value refuses the whole file.
//!
//! Its proof is its tests (`reduce_tests.rs`): over every `supervise/fixtures`
//! transcript the reduction folds as its source (the same steps, ids one to
//! one, times one shift, the same walls and end), every notice classes as
//! its source's, the harness reads each reduction as its source (the login
//! wall, the standing limit, the model, the last answer, the resumed build,
//! the finish and the task after every row, and each notice as the wall
//! check reads it), the committed 2.1.303 reduction reads its usage limit
//! as the harness does, and no 8-byte run of any source string survives
//! outside the kept vocabulary: no value, no key inside a free subtree, and
//! no stamp's tail. Apart from [`opens_free`], the check finds a tool's
//! arguments by their shape (a JSON schema's `properties`, a `…tool_use`
//! part's `input`, what a map keyed by a call's id holds) wherever they
//! sit: no argument name may be kept as written, and no word there. And it
//! holds ids one to one across keys and values, so no id the reduction
//! remaps as a value is kept as written as a key. A planted transcript (an
//! MCP tool's argument names in its call, in its `wireToolInputs` copy, in
//! an `api-request-shape`'s tool list and in an `api-request-blob`'s
//! `tool_addition`, a question keying its answer, in a row and in a row a
//! `progress` row nests, an MCP result's keys, a server tool's input, a
//! tracked file's name, words after a stamp, a call's id keying a map) and
//! a planted Codex rollout (a `parsed_cmd` read naming a file) keep none of
//! their words. A planted word no reader reads under a closed key
//! (`"mode":"quokkaledger"`) is emptied, and every word a reduction keeps
//! is read ([`unread_words`]). Negative controls: a reducer that keeps
//! content, one that replaces `type`, one that blanks a notice's text, the
//! earlier rules' reductions of the plant, a reducer that keeps any Codex
//! `name` as a tool's, one that keeps every candidate word (the output-side
//! check, which the verb refuses), one that blanks a word a reader reads
//! (the fold check, which the verb refuses) and one that blanks a notice's
//! `error`, `status` or `rateLimitType` (the harness's reading) each fail.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;

use aterm_agent::ai::codex_limits::{CodexLimits, rollout_rate_limits};
use aterm_agent::harness::codex_rollout::{Row as SpendRow, TurnReader};
use aterm_agent::harness::codex_steps::CodexStepFold;
use aterm_agent::harness::steps::{
    COMPACT, End, NoticeRead, StepEvent, StepFold, StepId, StepState, USER_MARKERS, notice_read,
    tool_class,
};
use aterm_agent::harness::upgrade::{
    CreditsWord, FinishedScan, LastTurn, LoginScan, StandingLimit, StandingScan, StandingWall,
};
use aterm_agent::harness::{claude_reads, codex_reads};
use aterm_json::Value;
use aterm_phase::WallKind;

use crate::{Fail, LAB, Res};

/// The program that wrote the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// A Claude Code transcript.
    Claude,
    /// A Codex rollout.
    Codex,
}

impl Program {
    /// Its name on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Program::Claude => "claude",
            Program::Codex => "codex",
        }
    }

    /// The key a row names its build by.
    fn build_key(self) -> &'static str {
        match self {
            Program::Claude => "version",
            Program::Codex => "cli_version",
        }
    }
}

/// The day a reduction's first row is moved to (year, month, day).
pub const BASE_DAY: (i64, u32, u32) = (2000, 1, 1);

/// The longest closed word kept.
pub const WORD_MAX: usize = 64;

/// The keys a Claude Code word MAY be kept under: the row's and a part's
/// `type`, the build, the model and effort, a notice's typed fields
/// (`error`, `apiError`, its quota's words), who asked (`origin.kind`,
/// `turnOrigin`), a system row's `level`, and the usage figures' words. A
/// CANDIDATE FILTER only (ruling U7): a word under one is kept when a
/// reading reads it ([`Candidates::by_readings`]), and goes otherwise.
pub const CLAUDE_WORDS: &[&str] = &[
    "type",
    "subtype",
    "role",
    "version",
    "model",
    "requestedModel",
    "effort",
    "perTurnEffort",
    "error",
    "apiError",
    "kind",
    "turnOrigin",
    "level",
    "status",
    "rateLimitType",
    "overageStatus",
    "overageDisabledReason",
    "stop_reason",
    "service_tier",
    "speed",
    "entrypoint",
    "userType",
    "permissionMode",
    "promptSource",
    "trigger",
    "mode",
];

/// The keys a Codex rollout's word MAY be kept under, a candidate filter as
/// [`CLAUDE_WORDS`] is (ruling U7). The five before the last are as the
/// lab's Codex 0.163.0 rollouts read them (session log 5G, 2026-10-19;
/// ruling T9 names Codex's keys from a committed reduction, never a guess):
/// a call's `namespace` (`functions`, `collaboration`: the step fold's class
/// table reads it beside the `name`), a `turn_aborted`'s `reason`
/// (`interrupted`), who began a thread or a turn (`thread_source`: `user`,
/// `subagent`; `turn_trigger`) and the turn's `collaboration_mode_kind`
/// (`default`). Then a `turn_context`'s `service_tier`, a key that rollout
/// shows and the spend fold reads (`TurnReader`'s tier, one of the
/// [`readings`]), so a tier it names is kept; and last the words of a
/// `token_count`'s `rate_limits` that the limits reader reads
/// (`ai::codex_limits::rollout_rate_limits`: the family `limit_id` it
/// needs to read the row at all, its `limit_name`, the credits' `balance`
/// and an `individual_limit` it reads as a number written as text, and the
/// `rate_limit_reached_type`), so a reduction's limits read as its
/// source's.
pub const CODEX_WORDS: &[&str] = &[
    "type",
    "role",
    "model",
    "effort",
    "reasoning_effort",
    "cli_version",
    "originator",
    "history_mode",
    "approval_policy",
    "mode",
    "status",
    "codex_error_info",
    "plan_type",
    "source",
    "namespace",
    "reason",
    "thread_source",
    "turn_trigger",
    "collaboration_mode_kind",
    "service_tier",
    "limit_id",
    "limit_name",
    "balance",
    "individual_limit",
    "rate_limit_reached_type",
];

/// The keys a Claude Code transcript names an id by (besides any
/// UUID-shaped string).
const CLAUDE_IDS: &[&str] = &[
    "uuid",
    "parentUuid",
    "logicalParentUuid",
    "leafUuid",
    "promptId",
    "sessionId",
    "session_id",
    "id",
    "tool_use_id",
    "requestId",
    "messageId",
    "scheduledTaskId",
    "agentId",
    "anchorUuid",
    "headUuid",
    "tailUuid",
    "requestRef",
    "serverClassifierRequest",
    "sourceToolAssistantUUID",
    "sourceToolUseID",
    "toolUseID",
    "parentToolUseID",
];

/// The keys a Codex rollout names an id by (besides any UUID-shaped string).
const CODEX_IDS: &[&str] = &[
    "id",
    "turn_id",
    "call_id",
    "thread_id",
    "conversation_id",
    "session_id",
    "forked_from_id",
    "parent_thread_id",
    "item_id",
    "response_id",
    "account_id",
    "user_id",
    "creator_account_id",
    "creator_user_id",
];

// ---------------------------------------------------------------------------
// The ordered tree
// ---------------------------------------------------------------------------

/// A JSON value as written: objects in their order, numbers as their text.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number, as written.
    Num(String),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Node>),
    /// An object, in its order.
    Obj(Vec<(String, Node)>),
}

impl Node {
    /// The member `key` of an object (the last of that name, as JSON
    /// readers take it).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Obj(m) => m.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The text of a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Spell the value as one compact JSON line, strings escaped as
    /// `JSON.stringify` escapes them.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Node::Null => out.push_str("null"),
            Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Node::Num(n) => out.push_str(n),
            Node::Str(s) => write_str(out, s),
            Node::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Node::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// How deep a value may nest.
const DEPTH_MAX: usize = 256;

/// Parse one JSON value (whitespace around it allowed).
///
/// # Errors
/// The text is not exactly one JSON value.
pub fn parse(text: &str) -> Result<Node, String> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i == p.b.len() {
        Ok(v)
    } else {
        Err(format!("text after the value at byte {}", p.i))
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn eat(&mut self, word: &[u8]) -> bool {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > DEPTH_MAX {
            return self.err("nested too deep");
        }
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                self.ws();
                if self.eat(b"}") {
                    return Ok(Node::Obj(members));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("a key expected");
                    }
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b":") {
                        return self.err("':' expected");
                    }
                    self.ws();
                    let v = self.value(depth + 1)?;
                    members.push((k, v));
                    self.ws();
                    if self.eat(b",") {
                        continue;
                    }
                    if self.eat(b"}") {
                        return Ok(Node::Obj(members));
                    }
                    return self.err("',' or '}' expected");
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b"]") {
                    return Ok(Node::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b",") {
                        continue;
                    }
                    if self.eat(b"]") {
                        return Ok(Node::Arr(items));
                    }
                    return self.err("',' or ']' expected");
                }
            }
            Some(b'"') => self.string().map(Node::Str),
            Some(b't') if self.eat(b"true") => Ok(Node::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Node::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Node::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("a value expected"),
        }
    }

    fn digits(&mut self) -> usize {
        let at = self.i;
        while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - at
    }

    fn number(&mut self) -> Result<Node, String> {
        let at = self.i;
        self.eat(b"-");
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.err("a digit expected"),
        }
        if self.eat(b".") && self.digits() == 0 {
            return self.err("a fraction's digit expected");
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return self.err("an exponent's digit expected");
            }
        }
        // ASCII only, so a char boundary.
        Ok(Node::Num(
            String::from_utf8_lossy(&self.b[at..self.i]).into_owned(),
        ))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.b.get(self.i..self.i + 4) else {
            return self.err("four hex digits expected");
        };
        let v = std::str::from_utf8(h)
            .ok()
            .and_then(|h| u32::from_str_radix(h, 16).ok());
        match v {
            Some(v) => {
                self.i += 4;
                Ok(v)
            }
            None => self.err("four hex digits expected"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return self.err("an unterminated string");
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return String::from_utf8(out)
                        .or_else(|_| self.err("a string that is no UTF-8"));
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.b.get(self.i) else {
                        return self.err("an unterminated escape");
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.eat(b"\\u") {
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    // Two lone halves: each the replacement character.
                                    out.extend_from_slice("\u{fffd}".as_bytes());
                                    lo
                                }
                            } else {
                                hi
                            };
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("an unknown escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return self.err("a control character in a string"),
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Words, ids and times
// ---------------------------------------------------------------------------

/// A closed word: 1 to [`WORD_MAX`] bytes of `[A-Za-z0-9._:@<>+-]`, so no
/// space, no path and no sentence.
#[must_use]
pub fn is_word(s: &str) -> bool {
    (1..=WORD_MAX).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:@<>+-".contains(&b))
}

/// A key kept as written: an identifier (`[A-Za-z_][A-Za-z0-9_-]*`, at most
/// [`WORD_MAX`] bytes), never a path or a sentence.
#[must_use]
pub fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=WORD_MAX).contains(&b.len())
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

/// A name the reducer gives a key (`k<n>`), which holds nothing of the key.
#[must_use]
pub fn is_key_name(k: &str) -> bool {
    k.strip_prefix('k')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// A UUID's shape (`8-4-4-4-12` hex digits).
#[must_use]
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Whether a key names a time: `timestamp`, a name ending `At`/`_at`, or
/// Codex's millisecond stamps (`started_at_ms`, `completed_at_ms`) and an
/// item's `create_time` (fractional seconds), as its 0.163.0 rollouts
/// write them (session log 5G).
#[must_use]
pub fn is_time_key(key: &str) -> bool {
    key == "timestamp"
        || key.ends_with("At")
        || key.ends_with("_at")
        || key.ends_with("_at_ms")
        || key == "create_time"
}

/// The date of an RFC 3339 stamp (`YYYY-MM-DDTHH:MM:SS…`), read from its
/// first 19 bytes.
#[must_use]
pub fn iso_day(s: &str) -> Option<(i64, u32, u32)> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let t = s.get(r)?;
        (t.bytes().all(|c| c.is_ascii_digit())).then(|| t.parse().ok())?
    };
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hh, mm, ss) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    ((1..=12).contains(&m)
        && (1..=days_in(i64::from(y), m)).contains(&d)
        && hh < 24
        && mm < 60
        && ss < 61)
        .then_some((i64::from(y), m, d))
}

/// The date of a WHOLE RFC 3339 stamp: [`iso_day`]'s first 19 bytes, then
/// at most a `.` fraction and a zone (`Z`, `z`, `±HH:MM` or `±HHMM`), and
/// nothing after. A time is rebased only from one, since everything past
/// its date is kept as written ([`P11`]); a string that only begins like a
/// stamp is no time.
#[must_use]
pub fn stamp_day(s: &str) -> Option<(i64, u32, u32)> {
    let day = iso_day(s)?;
    // `iso_day` read 19 ASCII bytes, so byte 19 is a character boundary.
    let mut rest = s.get(19..)?;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let zone = match rest.as_bytes() {
        [] | [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] | [b'+' | b'-', h1, h2, m1, m2] => {
            [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
        }
        _ => false,
    };
    zone.then_some(day)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in(y: i64, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
#[must_use]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
#[must_use]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A notice's reset clause, rebased: `resets [<Mon> <D> at ]<H>[:<MM>]am|pm`
/// with its date moved `shift` days (in `year`, the first row's), its time
/// of day as written and its zone dropped (` · resets Jan 4 at 9:35am`).
/// `None` when the notice names no reset in that form.
#[must_use]
pub fn reset_clause(text: &str, shift: i64, year: i64) -> Option<String> {
    let rest = &text[text.find("resets ")? + "resets ".len()..];
    let mut words = rest.split_whitespace();
    let first = words.next()?;
    let (date, time) = match MONTHS.iter().position(|m| *m == first) {
        Some(mi) => {
            let day: u32 = words.next()?.trim_end_matches(',').parse().ok()?;
            let month = u32::try_from(mi + 1).ok()?;
            if !(1..=days_in(year, month)).contains(&day) {
                return None;
            }
            let next = words.next()?;
            let time = if next == "at" { words.next()? } else { next };
            let (y, m, d) = civil_from_days(days_from_civil(year, month, day) + shift);
            let _ = y;
            (Some(format!("{} {d} at ", MONTHS[(m - 1) as usize])), time)
        }
        None => (None, first),
    };
    let lower = time.to_ascii_lowercase();
    let (clock, half) = lower
        .strip_suffix("am")
        .map(|c| (c, "am"))
        .or_else(|| lower.strip_suffix("pm").map(|c| (c, "pm")))?;
    let (h, m) = match clock.split_once(':') {
        Some((h, m)) => (h, Some(m)),
        None => (clock, None),
    };
    let ok =
        |t: &str, n: usize| t.len() <= n && !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    if !ok(h, 2) || m.is_some_and(|m| !(m.len() == 2 && ok(m, 2))) {
        return None;
    }
    let clock = m.map_or_else(|| h.to_string(), |m| format!("{h}:{m}"));
    Some(format!(
        " · resets {}{clock}{half}",
        date.unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// The canonical phrase of a notice (T7)
// ---------------------------------------------------------------------------

/// How the readers take a notice's words: the wall `classify_wall` names,
/// and the three readings `wall_confirm`'s `notice_wall` adds to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    /// `aterm_phase::classify_wall`.
    pub wall: Option<WallKind>,
    /// `aterm_phase::wall::not_a_wall`.
    pub not_a_wall: bool,
    /// `aterm_phase::wall::named_by_status_alone`.
    pub by_status: bool,
    /// The vendor's `/compact` remedy (`wall.compact_images`,
    /// `wall.compact_attachments`), which `notice_wall` reads as a full context.
    pub compact: bool,
}

impl Reading {
    /// The reading of `text`.
    #[must_use]
    pub fn of(text: &str) -> Reading {
        let lower = text.to_lowercase();
        Reading {
            wall: aterm_phase::classify_wall(text),
            not_a_wall: aterm_phase::wall::not_a_wall(text),
            by_status: aterm_phase::wall::named_by_status_alone(text),
            compact: ["wall.compact_images", "wall.compact_attachments"]
                .iter()
                .any(|id| lower.contains(&aterm_phase::anchor(id).to_lowercase())),
        }
    }
}

/// The canonical phrases, in the order they are tried: the anchor phrases
/// of the walls (`aterm_phase::anchor`) first, then the table's own words
/// for the walls no anchor names alone, the API error's forms, the notice
/// that is no wall, the `/compact` remedy, and nothing.
#[must_use]
pub fn phrases() -> Vec<&'static str> {
    vec![
        "You've hit your session limit",
        "You've hit your weekly limit",
        aterm_phase::anchor("wall.fable"),
        "You've reached your Opus limit · continuing on Sonnet",
        "Credit balance is too low",
        aterm_phase::anchor("wall.context"),
        aterm_phase::anchor("wall.login"),
        aterm_phase::anchor("wall.overloaded"),
        "The request was rate limited",
        aterm_phase::anchor("wall.api_error"),
        "API Error: can't help with this",
        "API Error: certificate",
        "API Error: stalled",
        "API Error: output token maximum",
        "API Error: connection refused",
        "API Error: Request was aborted.",
        aterm_phase::anchor("wall.compact_images"),
        "",
    ]
}

/// What an `API Error` that prints status `code` may be spelled as.
fn coded(code: u16) -> [String; 2] {
    [
        format!("API Error: {code}"),
        format!("API Error: {code} can't help with this"),
    ]
}

/// THE CANONICAL PHRASE of a notice whose text is `text` (T7): the first
/// phrase ([`phrases`], or an `API Error` printing the status the source
/// printed) whose [`Reading`] is the source's, with its reset clause rebased
/// ([`reset_clause`]) when the source names one and the phrase reads the
/// same with it. `joins` is how many text parts beyond the first the notice
/// joins (`wall_confirm`'s `texts`), each written empty: the reading is
/// held equal as joined.
///
/// # Errors
/// No phrase reads as the source does: the notice is refused, never kept.
pub fn canonical(text: &str, joins: usize, shift: i64, year: i64) -> Result<String, String> {
    let want = Reading::of(text);
    let pad = " ".repeat(joins);
    let mut cands: Vec<String> = phrases().into_iter().map(str::to_string).collect();
    if let Some(WallKind::ApiError { code: Some(c), .. }) = want.wall {
        cands.extend(coded(c));
    }
    let clause = reset_clause(text, shift, year);
    for c in &cands {
        if let Some(r) = &clause {
            let with = format!("{c}{r}");
            if Reading::of(&format!("{with}{pad}")) == want {
                return Ok(with);
            }
        }
        if Reading::of(&format!("{c}{pad}")) == want {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "a notice no canonical phrase reads as ({want:?}): refused"
    ))
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Where a string sits in its row.
pub struct At<'a> {
    /// The program that wrote the row.
    pub program: Program,
    /// Its member's key (an array's elements take the array's).
    pub key: &'a str,
    /// The keys from the row's root down to it (arrays add none).
    pub path: &'a [String],
    /// The `type` of the content part it sits in (`text`, `tool_use`, …).
    pub part: Option<&'a str>,
    /// The row's `type`.
    pub row: Option<&'a str>,
    /// A Codex row's `payload.type` (`function_call`, `exec_command_begin`,
    /// …).
    pub payload: Option<&'a str>,
    /// The row is Claude Code's own notice (`isApiErrorMessage`).
    pub notice: bool,
    /// It sits in a FREE subtree ([`opens_free`]: what a tool takes or
    /// returns, what a schema's author named, what Claude Code sends or
    /// attaches for the model, a map keyed by tracked files).
    pub free: bool,
}

/// What becomes of a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Treat {
    /// As written: the negative controls' (`reduce_tests.rs`); [`P11`] keeps
    /// nothing as written that is no closed word.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only a negative control keeps a string as written"
        )
    )]
    Keep,
    /// A CANDIDATE closed word: as written while it is a word ([`is_word`])
    /// and a reader reads it ([`Rules::words`], ruling U7), else replaced.
    Word,
    /// Remapped, one to one.
    Id,
    /// Rebased by the file's shift.
    Time,
    /// A user row's (or a local command's) text: its opening marker only.
    Lead,
    /// A notice's text: its canonical phrase ([`canonical`]).
    Notice,
    /// A tool's name: kept when the class table names it, else remapped.
    Tool,
    /// `""`.
    Replace,
}

/// The rule a reducer follows. [`P11`] is the one `lab reduce` runs; the
/// tests' negative controls wrap it to break one case.
pub trait Rules {
    /// What becomes of the string `s` at `at`.
    fn string(&self, at: &At<'_>, s: &str) -> Treat;

    /// Which of the file's candidate closed words its reduction keeps: one
    /// flag per pair of [`Candidates::pairs`]. The readers decide
    /// ([`Candidates::by_readings`], ruling U7).
    ///
    /// # Errors
    /// The file is refused: with every candidate kept, its reduction still
    /// does not read as its source.
    fn words(&self, c: &Candidates<'_>) -> Result<Vec<bool>, String> {
        c.by_readings()
    }
}

/// THE REDUCTION RULE (P11 as ruling T7 amends it; module docs).
pub struct P11;

impl Rules for P11 {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.free {
            return Treat::Replace;
        }
        let ids = match at.program {
            Program::Claude => CLAUDE_IDS,
            Program::Codex => CODEX_IDS,
        };
        if is_uuid(s) || ids.contains(&at.key) {
            return Treat::Id;
        }
        if is_time_key(at.key) && stamp_day(s).is_some() {
            return Treat::Time;
        }
        match at.program {
            Program::Claude => {
                // A message's text as its readers read it: its string
                // content, or any content part's `text`, whatever the part's
                // `type` says (`wall_confirm`'s `texts`, the step fold's
                // lead), so a part whose `type` no reader reads, emptied, is
                // still read and reduced as one (ruling U8: a reduction
                // reduced again is itself).
                let message_text = at.path.first().map(String::as_str) == Some("message")
                    && at.path.get(1).map(String::as_str) == Some("content")
                    && (at.path.len() == 2 || (at.path.len() == 3 && at.key == "text"));
                if at.notice && message_text {
                    return Treat::Notice;
                }
                if at.row == Some("user") && message_text {
                    return Treat::Lead;
                }
                if at.row == Some("system") && at.path.len() == 1 && at.key == "content" {
                    return Treat::Lead;
                }
                if at.part == Some("tool_use") && at.key == "name" && at.path.len() == 3 {
                    return Treat::Tool;
                }
                if CLAUDE_WORDS.contains(&at.key) {
                    return Treat::Word;
                }
            }
            Program::Codex => {
                // A call's own tool name: `payload.name` of a call item.
                // Every other `name` (a `parsed_cmd` read's file name, …)
                // is what a command named, never a tool's.
                if at.key == "name"
                    && at.row == Some("response_item")
                    && matches!(at.payload, Some("function_call" | "custom_tool_call"))
                    && at.path == ["payload", "name"]
                {
                    return Treat::Tool;
                }
                if CODEX_WORDS.contains(&at.key) {
                    return Treat::Word;
                }
            }
        }
        Treat::Replace
    }
}

// ---------------------------------------------------------------------------
// The reduction
// ---------------------------------------------------------------------------

/// A file reduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reduction {
    /// The reduction: its own `#` line, then one row per line.
    pub text: String,
    /// Rows reduced.
    pub rows: usize,
    /// The source's `#` lines dropped.
    pub comments: usize,
    /// Distinct ids remapped.
    pub ids: usize,
    /// Notices written as their canonical phrase.
    pub notices: usize,
    /// Days every time was moved by.
    pub shift_days: i64,
    /// Days every time has moved from the first source, through the
    /// reductions the source was itself made by ([`header`]'s count).
    pub moved_days: i64,
    /// The source was itself a reduction (ruling U8).
    pub again: bool,
    /// The newest build a row named (`version`, Codex's `cli_version`).
    pub build: Option<String>,
    /// Distinct `(key, word)` candidate pairs ([`Candidates::pairs`]), and
    /// how many of them the reduction keeps (ruling U7).
    pub words: (usize, usize),
}

/// Reduce `source` by [`P11`].
///
/// # Errors
/// A line is not one JSON value, a notice reads as no canonical phrase, or
/// the reduction cannot read as its source ([`Rules::words`]).
pub fn reduce(source: &str, program: Program) -> Result<Reduction, String> {
    reduce_with(source, program, &P11)
}

/// Reduce `source` by `rules`.
///
/// # Errors
/// As [`reduce`].
pub fn reduce_with(source: &str, program: Program, rules: &dyn Rules) -> Result<Reduction, String> {
    let w = walked(source, program, rules)?;
    let moved = w.before.unwrap_or(0) + w.shift;
    let rows = w.rows.len();
    let c = Candidates::new(program, source, w.shift, w.rows, w.pairs, w.sites)?;
    let keep = rules.words(&c)?;
    if keep.len() != c.pairs.len() {
        return Err(format!(
            "the rule chose {} of {} candidate words",
            keep.len(),
            c.pairs.len()
        ));
    }
    let mut text = header(program, w.build.as_deref(), moved, rows, w.before.is_some());
    text.push('\n');
    for r in c.lines(&keep) {
        text.push_str(&r);
        text.push('\n');
    }
    Ok(Reduction {
        text,
        rows,
        comments: w.comments,
        ids: w.ids,
        notices: w.notices,
        shift_days: w.shift,
        moved_days: moved,
        again: w.before.is_some(),
        build: w.build,
        words: (keep.len(), keep.iter().filter(|k| **k).count()),
    })
}

/// A file walked by its rule: its reduction with EVERY candidate closed word
/// kept ([`Candidates`]), the pairs and where each sits, and what the walk
/// counted.
struct Walked {
    rows: Vec<Node>,
    pairs: Vec<(String, String)>,
    sites: Vec<Site>,
    comments: usize,
    ids: usize,
    notices: usize,
    shift: i64,
    /// The day count the source's own reduction header names, when the
    /// source is a reduction ([`moved_before`]).
    before: Option<i64>,
    build: Option<String>,
}

fn walked(source: &str, program: Program, rules: &dyn Rules) -> Result<Walked, String> {
    let mut rows = Vec::new();
    let (mut comments, mut before, mut named) = (0, None, None);
    for (n, line) in source.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('#') {
            comments += 1;
            if before.is_none()
                && let Some(days) = moved_before(t, program)
            {
                before = Some(days);
                named = build_named(t, program);
            }
            continue;
        }
        rows.push(parse(t).map_err(|e| format!("line {}: not one JSON value ({e})", n + 1))?);
    }
    // The first pass learns the file's ids (every string the rule remaps
    // as one), so a key that names an id is remapped as that id wherever it
    // sits, before its value or after (`wireToolInputs`, keyed by the ids
    // of the calls its row makes); it writes nothing.
    let mut learn = Walk::new(program, rules, true, HashSet::new());
    for (n, row) in rows.iter().enumerate() {
        let mut state = RowState::of(row, program, 0, BASE_DAY.0, true)
            .map_err(|e| format!("row {}: {e}", n + 1))?;
        learn
            .walk(row, "", &mut Vec::new(), None, false, false, &mut state)
            .map_err(|e| format!("row {}: {e}", n + 1))?;
    }
    let mut w = Walk::new(program, rules, false, learn.ids.into_keys().collect());
    w.taken = learn.taken;
    // The first row's own stamp, else the first a row holds anywhere.
    let first = rows
        .iter()
        .find_map(|r| {
            r.get("timestamp")
                .and_then(Node::as_str)
                .and_then(stamp_day)
        })
        .or_else(|| {
            rows.iter()
                .find_map(|r| w.first_day(r, "", 0, None, false, false))
        });
    let (shift, year) = first.map_or((0, BASE_DAY.0), |(y, m, d)| {
        (
            days_from_civil(BASE_DAY.0, BASE_DAY.1, BASE_DAY.2) - days_from_civil(y, m, d),
            y,
        )
    });
    (w.shift, w.year) = (shift, year);
    let mut out_rows = Vec::with_capacity(rows.len());
    let mut notices = 0;
    let mut build = None;
    for (n, row) in rows.iter().enumerate() {
        if let Some(b) = [Some(row), row.get("payload")]
            .into_iter()
            .flatten()
            .find_map(|r| r.get(program.build_key()).and_then(Node::as_str))
            .filter(|b| is_word(b))
        {
            build = Some(b.to_string());
        }
        let mut state = RowState::of(row, program, shift, year, false)
            .map_err(|e| format!("row {}: {e}", n + 1))?;
        notices += usize::from(state.left.is_some());
        let mut path = Vec::new();
        w.row = n;
        let reduced = w
            .walk(row, "", &mut path, None, false, false, &mut state)
            .map_err(|e| format!("row {}: {e}", n + 1))?;
        out_rows.push(reduced);
    }
    Ok(Walked {
        rows: out_rows,
        pairs: w.pairs,
        sites: w.sites,
        comments,
        ids: w.ids.len(),
        notices,
        shift,
        before,
        // A source reduction's rows may keep no build (no reader read it):
        // its own line names it.
        build: build.or(named),
    })
}

/// A reduction's own `#` line: the program, the build its rows named (a
/// closed word), how many days its times have moved from the first source,
/// the row count, and, for a source that was itself a reduction (`again`),
/// that it was reduced again (ruling U8), in fixed words.
#[must_use]
pub fn header(
    program: Program,
    build: Option<&str>,
    moved: i64,
    rows: usize,
    again: bool,
) -> String {
    format!(
        "# {} {} · lab reduce (session log 5H-1; P11 as ruling T7 amends it, a closed word kept only where a reader reads it as ruling U7 says): keys, row order, numbers and the closed words read kept; times moved {moved:+} days; ids remapped; every other string replaced ({rows} rows){}",
        program.name(),
        build.unwrap_or("-"),
        if again {
            "; reduced again from a reduction (ruling U8)"
        } else {
            ""
        },
    )
}

/// The build a reduction's own `#` line for `program` names (`# <program>
/// <build> · lab reduce (…`), a closed word; `None` for `-` and any other
/// line.
fn build_named(line: &str, program: Program) -> Option<String> {
    let rest = line.strip_prefix(&format!("# {} ", program.name()))?;
    let (build, _) = rest.split_once(" · lab reduce (")?;
    (build != "-" && is_word(build)).then(|| build.to_string())
}

/// The day count of a reduction's own `#` line for `program` (this rule's
/// [`header`], or an earlier rule's: `# <program> <build> · lab reduce (…`
/// naming `times moved <n> days`); `None` for any other line.
fn moved_before(line: &str, program: Program) -> Option<i64> {
    let rest = line.strip_prefix(&format!("# {} ", program.name()))?;
    let (_, tail) = rest.split_once(" · lab reduce (")?;
    let n = tail.split("times moved ").nth(1)?.split(' ').next()?;
    let days = tail.split("times moved ").nth(1)?.split(' ').nth(1)?;
    (days.starts_with("day")).then(|| n.parse::<i64>().ok())?
}

/// Whether member `key` of the object that sits under `parent`, `key`
/// being `depth` keys deep in a row and in content part `part`, opens a
/// FREE subtree: one whose keys a tool, a person or a schema's author named
/// and whose strings are what they said, so every key in it is renamed
/// `k<n>` (an id remapped as that id) and every string emptied. `id_key`:
/// the key is an id the file names ([`Walk::is_id`]). Each is named by what
/// it holds:
///
/// - what a tool TAKES: the `input` of any `…tool_use` part (`tool_use`,
///   `server_tool_use`, `mcp_tool_use`), wherever the part sits; and what
///   a map keyed by an id holds under each id, since a map keyed by a
///   call's id holds what that call took (Claude Code 2.1.303 writes each
///   call's input a second time in its row's `wireToolInputs`, and its
///   working directory in `wireIngestContext`, both keyed so);
/// - what a tool RETURNS: the `content` of any `…tool_result` part, and a
///   message's `toolUseResult` and `mcpMeta` (an MCP tool's
///   `structuredContent` and `_meta`) at any depth, since a `progress` row
///   (`agent_progress`) nests a subagent's whole message under its `data`;
/// - what a SCHEMA's author named: a member whose key ends in `schema`, any
///   case (a tool definition's `input_schema`, MCP's `inputSchema` and
///   `outputSchema`), wherever it sits, so a tool list keeps none of its
///   arguments' names wherever Claude Code writes it (a deferred tool's
///   `tool_addition` part in an `api-request-blob` row's `message` among
///   them); and any object that is a JSON schema by its shape
///   ([`is_schema`]) is free whole wherever it sits;
/// - what Claude Code SENDS or ATTACHES for the model: an
///   `api-request-shape` row's `shape` (its system prompt and its tool
///   list), and each field of an `attachment` row's payload but its `type`
///   and `origin`, which `upgrade::FinishedScan` reads (a
///   `prompt_snapshot`'s tool list among them);
/// - a map keyed by the files it tracks (a file-history snapshot's
///   `trackedFileBackups`, keyed by each file's path, a bare file name
///   among them).
///
/// A Codex call's `arguments`, `input` and `output` are free wherever they
/// sit. The step fold, the wall check and the shape test read none of
/// these. A tool's argument names in a place this rule does not name are
/// what the text check's argument check finds by their shape.
fn opens_free(
    program: Program,
    parent: &str,
    key: &str,
    depth: usize,
    part: Option<&str>,
    id_key: bool,
) -> bool {
    id_key
        || key.to_ascii_lowercase().ends_with("schema")
        || match program {
            Program::Claude => {
                matches!(key, "toolUseResult" | "mcpMeta" | "trackedFileBackups")
                    || (key == "input" && part.is_some_and(|p| p.ends_with("tool_use")))
                    || (key == "content" && part.is_some_and(|p| p.ends_with("tool_result")))
                    || (depth == 1 && key == "shape")
                    || (depth == 2 && parent == "attachment" && !matches!(key, "type" | "origin"))
            }
            Program::Codex => matches!(key, "arguments" | "input" | "output"),
        }
}

/// A JSON SCHEMA by its shape: an object whose `properties` is an object
/// and whose `type` is `"object"` or absent. Its property names are what
/// its author named (a tool's arguments), so it is free whole wherever it
/// sits, in a tool list no rule names or nested in another schema.
#[must_use]
pub fn is_schema(v: &Node) -> bool {
    matches!(v.get("properties"), Some(Node::Obj(_)))
        && v.get("type").is_none_or(|t| t.as_str() == Some("object"))
}

/// What a row's walk carries: its type and, for a notice, the phrase its
/// first text is written as.
struct RowState {
    kind: Option<String>,
    /// A Codex row's `payload.type`.
    payload: Option<String>,
    notice: bool,
    left: Option<String>,
}

impl RowState {
    /// A row's state; `learn`: for the first pass, which writes nothing,
    /// so no notice's phrase is made.
    fn of(
        row: &Node,
        program: Program,
        shift: i64,
        year: i64,
        learn: bool,
    ) -> Result<RowState, String> {
        let kind = row.get("type").and_then(Node::as_str).map(str::to_string);
        let payload = (program == Program::Codex)
            .then(|| {
                row.get("payload")?
                    .get("type")?
                    .as_str()
                    .map(str::to_string)
            })
            .flatten();
        let notice = program == Program::Claude
            && kind.as_deref() == Some("assistant")
            && row.get("isApiErrorMessage") == Some(&Node::Bool(true));
        let left = if notice && !learn {
            let (text, parts) = notice_text(row);
            Some(canonical(&text, parts.saturating_sub(1), shift, year)?)
        } else {
            None
        };
        Ok(RowState {
            kind,
            payload,
            notice,
            left,
        })
    }
}

/// A notice row's text as `wall_confirm`'s `texts` joins it, and how many
/// text parts it joined (0 for a string content).
#[must_use]
pub fn notice_text(row: &Node) -> (String, usize) {
    match row.get("message").and_then(|m| m.get("content")) {
        Some(Node::Str(t)) => (t.clone(), 0),
        Some(Node::Arr(parts)) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Node::as_str))
                .collect();
            (texts.join(" "), texts.len())
        }
        _ => (String::new(), 0),
    }
}

struct Walk<'r> {
    program: Program,
    rules: &'r dyn Rules,
    /// The first pass: it learns the file's ids ([`Walk::id`]) and writes
    /// nothing.
    learn: bool,
    /// The ids the first pass learned: every string the rule remaps as an
    /// id where it is a value.
    known: HashSet<String>,
    ids: HashMap<String, String>,
    names: HashMap<String, String>,
    keys: HashMap<String, String>,
    /// The last `k<n>` named, and the names the file already holds as keys
    /// (the first pass collects them).
    next_key: usize,
    taken: HashSet<String>,
    shift: i64,
    year: i64,
    /// The row being walked, and the member and element indices from its
    /// root down to where the walk is ([`Site::trail`]).
    row: usize,
    trail: Vec<usize>,
    /// THE CANDIDATES (ruling U7): each `(key, word)` pair the rule keeps a
    /// word of ([`Treat::Word`] while it is a word), in the order the file
    /// first names it, and every place one sits.
    pairs: Vec<(String, String)>,
    pair_of: HashMap<(String, String), usize>,
    sites: Vec<Site>,
}

impl<'r> Walk<'r> {
    fn new(program: Program, rules: &'r dyn Rules, learn: bool, known: HashSet<String>) -> Self {
        Walk {
            program,
            rules,
            learn,
            known,
            ids: HashMap::new(),
            names: HashMap::new(),
            keys: HashMap::new(),
            next_key: 0,
            taken: HashSet::new(),
            shift: 0,
            year: BASE_DAY.0,
            row: 0,
            trail: Vec::new(),
            pairs: Vec::new(),
            pair_of: HashMap::new(),
            sites: Vec::new(),
        }
    }

    /// A candidate closed word, `word` under `key`, where the walk is.
    fn candidate(&mut self, key: &str, word: &str) {
        let pair = (key.to_string(), word.to_string());
        let n = self.pairs.len();
        let pair = *self.pair_of.entry(pair.clone()).or_insert_with(|| {
            self.pairs.push(pair);
            n
        });
        self.sites.push(Site {
            row: self.row,
            trail: self.trail.clone(),
            pair,
        });
    }
}

impl Walk<'_> {
    /// Whether a key is an ID the file names: a UUID's shape, or a string
    /// the rule remaps as an id where it is a value (the first pass learns
    /// them all, [`Walk::known`]). It is remapped as that id, one to one
    /// with its values, and what it keys is free ([`opens_free`]).
    fn is_id(&self, k: &str) -> bool {
        is_uuid(k) || self.known.contains(k) || self.ids.contains_key(k)
    }

    /// The first rebased date in a row, in file order, outside a FREE
    /// subtree ([`opens_free`], [`is_schema`]). `key` is the member `v`
    /// sits under, `depth` how many keys deep it is, `part` the content
    /// part it is in, and `in_content` whether it is an element of a
    /// `content` array.
    fn first_day(
        &self,
        v: &Node,
        key: &str,
        depth: usize,
        part: Option<&str>,
        in_content: bool,
        free: bool,
    ) -> Option<(i64, u32, u32)> {
        match v {
            Node::Obj(m) => {
                let own = if in_content {
                    v.get("type").and_then(Node::as_str)
                } else {
                    None
                };
                let part = own.or(part);
                let free = free || is_schema(v);
                m.iter().find_map(|(k, x)| {
                    let free =
                        free || opens_free(self.program, key, k, depth + 1, part, self.is_id(k));
                    match x {
                        Node::Str(s) if !free && is_time_key(k) => stamp_day(s),
                        _ => self.first_day(x, k, depth + 1, part, false, free),
                    }
                })
            }
            Node::Arr(a) => a
                .iter()
                .find_map(|x| self.first_day(x, key, depth, part, key == "content", free)),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        v: &Node,
        key: &str,
        path: &mut Vec<String>,
        part: Option<&str>,
        free: bool,
        in_content: bool,
        row: &mut RowState,
    ) -> Result<Node, String> {
        Ok(match v {
            Node::Null => Node::Null,
            Node::Bool(b) => Node::Bool(*b),
            Node::Num(n) => Node::Num(if !free && is_time_key(key) {
                self.epoch(n)
            } else {
                n.clone()
            }),
            Node::Str(s) => Node::Str(self.string(key, path, part, free, row, s)?),
            Node::Arr(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (i, x) in items.iter().enumerate() {
                    self.trail.push(i);
                    out.push(self.walk(x, key, path, part, free, key == "content", row)?);
                    self.trail.pop();
                }
                Node::Arr(out)
            }
            Node::Obj(members) => {
                let own = if in_content {
                    v.get("type").and_then(Node::as_str)
                } else {
                    None
                };
                let part = own.or(part);
                // A JSON schema is free whole, wherever it sits.
                let free = free || is_schema(v);
                let mut out = Vec::with_capacity(members.len());
                for (i, (k, x)) in members.iter().enumerate() {
                    if self.learn && is_key_name(k) {
                        self.taken.insert(k.clone());
                    }
                    path.push(k.clone());
                    self.trail.push(i);
                    let id_key = self.is_id(k);
                    let opens = opens_free(self.program, key, k, path.len(), part, id_key);
                    // Named before its value, so `k<n>` follows reading
                    // order; an id is that id, one to one with its values.
                    let name = if id_key {
                        self.id(k)
                    } else {
                        self.key(k, free)
                    };
                    let y = self.walk(x, k, path, part, free || opens, false, row)?;
                    self.trail.pop();
                    path.pop();
                    out.push((name, y));
                }
                Node::Obj(out)
            }
        })
    }

    fn string(
        &mut self,
        key: &str,
        path: &[String],
        part: Option<&str>,
        free: bool,
        row: &mut RowState,
        s: &str,
    ) -> Result<String, String> {
        let at = At {
            program: self.program,
            key,
            path,
            part,
            row: row.kind.as_deref(),
            payload: row.payload.as_deref(),
            notice: row.notice,
            free,
        };
        let treat = self.rules.string(&at, s);
        if self.learn {
            // The first pass learns the ids, and writes nothing.
            if treat == Treat::Id {
                self.id(s);
            }
            return Ok(String::new());
        }
        Ok(match treat {
            Treat::Keep => s.to_string(),
            Treat::Word if is_word(s) => {
                self.candidate(key, s);
                s.to_string()
            }
            Treat::Word | Treat::Replace => String::new(),
            Treat::Id => self.id(s),
            Treat::Time => self.time(s),
            Treat::Lead => self.lead(s)?,
            Treat::Notice => row.left.take().unwrap_or_default(),
            Treat::Tool => self.tool(s),
        })
    }

    /// An id, remapped one to one, its validity as a `StepId` kept.
    fn id(&mut self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if let Some(v) = self.ids.get(s) {
            return v.clone();
        }
        let n = self.ids.len() + 1;
        let v =
            if is_uuid(s) {
                format!("00000000-0000-4000-8000-{n:012x}")
            } else {
                match s.split_once('_').map(|(p, _)| p).filter(|p| {
                    (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase())
                }) {
                    Some(p) => format!("{p}_r{n:04}"),
                    None => format!("r{n:04}"),
                }
            };
        // An id the step fold cannot read stays one it cannot read.
        let v = if StepId::new(s).is_some() {
            v
        } else {
            format!("{v}/")
        };
        self.ids.insert(s.to_string(), v.clone());
        v
    }

    /// A stamp with its date moved by the file's shift, the rest (its time
    /// of day, fraction and zone, [`stamp_day`]) as written.
    fn time(&self, s: &str) -> String {
        match stamp_day(s) {
            Some((y, m, d)) => {
                let (y, m, d) = civil_from_days(days_from_civil(y, m, d) + self.shift);
                format!("{y:04}-{m:02}-{d:02}{}", &s[10..])
            }
            None => String::new(),
        }
    }

    /// An epoch number under a time key, moved by the file's shift (seconds
    /// or milliseconds by its size; a fraction after the whole part kept as
    /// written, Codex's `create_time`); any other number as written.
    fn epoch(&self, n: &str) -> String {
        if let Some((whole, fraction)) = n.split_once('.')
            && !whole.is_empty()
            && !fraction.is_empty()
            && fraction.bytes().all(|b| b.is_ascii_digit())
        {
            let moved = self.epoch(whole);
            return if moved == whole {
                n.to_string()
            } else {
                format!("{moved}.{fraction}")
            };
        }
        let Ok(v) = n.parse::<i64>() else {
            return n.to_string();
        };
        let day_s = self.shift * 86_400;
        if (1_000_000_000..100_000_000_000).contains(&v) {
            (v + day_s).to_string()
        } else if (1_000_000_000_000..100_000_000_000_000).contains(&v) {
            (v + day_s * 1000).to_string()
        } else {
            n.to_string()
        }
    }

    /// A user row's text (or a local command's): the marker it opens with,
    /// and nothing past it, but the canonical phrase of a local command's
    /// output that reads as a full context (`wall_confirm`'s
    /// `command_notice`) in its tags.
    fn lead(&self, s: &str) -> Result<String, String> {
        let t = s.trim_start();
        if t.trim_end() == COMPACT || t.strip_prefix(COMPACT).is_some_and(|r| r.starts_with(' ')) {
            return Ok(COMPACT.to_string());
        }
        let Some(m) = USER_MARKERS.iter().find(|m| t.starts_with(**m)) else {
            return Ok(String::new());
        };
        if let Some(tag) = m.strip_prefix("<local-command-") {
            let inner = t[m.len()..]
                .split("</local-command-")
                .next()
                .unwrap_or_default()
                .trim();
            if aterm_phase::classify_wall(inner) == Some(WallKind::Context) {
                let phrase = canonical(inner, 0, self.shift, self.year)?;
                return Ok(format!("{m}{phrase}</local-command-{tag}"));
            }
        }
        Ok((*m).to_string())
    }

    /// A tool's name: the class table's names as written (Claude Code), a
    /// vendor's word with no `__` (Codex: every name its 0.163.0 rollouts
    /// called is a word, T9), else `tool<n>`,
    /// an `mcp__` name keeping that prefix.
    fn tool(&mut self, s: &str) -> String {
        let kept = match self.program {
            Program::Claude => !s.starts_with("mcp__") && tool_class(s).0 != "other",
            Program::Codex => is_word(s) && !s.contains("__"),
        };
        if kept {
            return s.to_string();
        }
        if let Some(v) = self.names.get(s) {
            return v.clone();
        }
        let n = self.names.len() + 1;
        let v = if s.starts_with("mcp__") {
            format!("mcp__tool{n}")
        } else {
            format!("tool{n}")
        };
        self.names.insert(s.to_string(), v.clone());
        v
    }

    /// A key: as written when it is an identifier outside a free subtree
    /// (`free`: [`opens_free`], where a key is whatever a tool, a person or
    /// a schema's author named), else `k<n>`, one to one. A key that is
    /// already such a name ([`is_key_name`]: a reduction's own) is that name
    /// wherever it sits, and a new name is never one the file holds
    /// ([`Walk::taken`]), so a reduction reduced again names its keys as it
    /// did (ruling U8).
    fn key(&mut self, k: &str, free: bool) -> String {
        if (!free && is_ident(k)) || is_key_name(k) {
            return k.to_string();
        }
        if let Some(v) = self.keys.get(k) {
            return v.clone();
        }
        let v = loop {
            self.next_key += 1;
            let v = format!("k{}", self.next_key);
            if !self.taken.contains(&v) {
                break v;
            }
        };
        self.keys.insert(k.to_string(), v.clone());
        v
    }
}

// ---------------------------------------------------------------------------
// The readings (ruling U7)
// ---------------------------------------------------------------------------

/// The fingerprint and identity a reading binds its fold to: any, the same
/// for every file read.
const READ_FP: u64 = 0x5eed_0000_0000_0007;
const READ_ID: Option<(u64, u64)> = Some((16_777_220, 7));

/// One screen wall of each class, as the step fold's `confirms` is asked.
const CLASSES: [WallKind; 5] = [
    WallKind::UsageSession,
    WallKind::Context,
    WallKind::Auth,
    WallKind::Overloaded,
    WallKind::Memory,
];

/// Whether a line is a row (no `#` line, not blank).
fn is_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#')
}

/// One step a fold made of a file: the row its offset lies in (0-based,
/// among the file's rows), its time, its state and what it is. Never its
/// offset, which a word blanked moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepRead {
    /// The row.
    pub row: usize,
    /// When (wall ms).
    pub at_ms: u64,
    /// Measured or not.
    pub state: StepState,
    /// What.
    pub ev: StepEvent,
}

/// What [`FinishedScan`] says after a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finish {
    /// [`FinishedScan::finished`].
    pub finished: bool,
    /// [`FinishedScan::settled`].
    pub settled: bool,
    /// [`FinishedScan::said_done`].
    pub said_done: bool,
    /// [`FinishedScan::said_irreversible`].
    pub irreversible: bool,
    /// [`FinishedScan::last_turn`].
    pub last: LastTurn,
}

/// What the harness's standing-limit scan says after a row
/// ([`StandingScan`]: the limit row a transcript stands at).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// `transcript_limit_until`.
    pub until: Option<u64>,
    /// `transcript_limit_reset`.
    pub reset: Option<u64>,
    /// `transcript_limit_spend`.
    pub spend: bool,
    /// `transcript_standing_wall`.
    pub wall: Option<StandingWall>,
    /// `transcript_standing_limit`, its words as their [`Reading`] (the
    /// row's text emptied).
    pub limit: Option<(Reading, StandingLimit)>,
}

impl Standing {
    fn of(scan: &StandingScan) -> Standing {
        Standing {
            until: scan.until(),
            reset: scan.reset(),
            spend: scan.spend(),
            wall: scan.wall(),
            limit: scan.limit().map(|mut l| {
                let r = Reading::of(&l.text);
                l.text.clear();
                (r, l)
            }),
        }
    }
}

/// WHAT THE COMMITTED READERS READ OF A FILE (ruling U7), row by row, which
/// decides the closed words a reduction keeps ([`Candidates::by_readings`]).
/// Claude Code's: the step fold's steps (bound at the file's start, floor
/// 0, every row pushed, then taken twice: a held prompt's release), its
/// losses and the main chain's end after every row and at the last; each
/// notice as the wall check reads it ([`notice_read`]: its words as their
/// [`Reading`], its typed fields, its wall, the walls it confirms and the
/// limit it names); after every row, [`FinishedScan`]'s reading, the
/// standing limit's ([`StandingScan`]: the limit's reset, its spend, the
/// quota wall and the limit row) and the login wall's ([`LoginScan`]). A
/// Codex rollout's: the Codex step fold's steps and losses, the spend
/// fold's row by row ([`TurnReader`], whose copy rule the step fold
/// shares) and the limits reader's ([`rollout_rate_limits`]). And for
/// either, EVERY ROW AS THE HARNESS'S SHAPES READ IT ([`claude_reads`],
/// [`codex_reads`]: the metadata-only reads every one of its readers
/// decides from), so a closed word any committed reader reads, the wall
/// check's `served_since_notice` and the upgrade's `resumed_build_warned`
/// among them, is a reading's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    /// The step fold's steps, oldest first.
    pub steps: Vec<StepRead>,
    /// Rows past the seed the fold could not read.
    pub lost: u64,
    /// Steps dropped from a full queue.
    pub overflow: u64,
    /// The main chain's end at the last take.
    pub end: End,
    /// The main chain's end after each row (Claude Code).
    pub ends: Vec<End>,
    /// Each notice: its row, its words' reading, and the wall check's read
    /// of it with its words emptied (Claude Code).
    pub notices: Vec<(usize, Reading, NoticeRead)>,
    /// [`FinishedScan`] after each row (Claude Code).
    pub finished: Vec<Finish>,
    /// The standing limit after each row (Claude Code).
    pub standing: Vec<Standing>,
    /// The login wall after each row: whether it stands, and when the last
    /// one was lifted (Claude Code).
    pub login: Vec<(bool, Option<u64>)>,
    /// The spend fold's reading of each row (Codex).
    pub spend: Vec<SpendRow>,
    /// The limits reader's readings of each row (Codex).
    pub limits: Vec<Vec<CodexLimits>>,
    /// Each row as the harness's shapes read it.
    pub reads: Vec<Vec<Option<Value>>>,
}

/// The [`Readings`] of a file's rows (its `#` lines and blank lines are
/// none).
#[must_use]
pub fn readings(text: &str, program: Program) -> Readings {
    let rows: Vec<&str> = text.lines().filter(|l| is_row(l)).collect();
    readings_of(&rows, program)
}

fn readings_of(rows: &[&str], program: Program) -> Readings {
    let mut r = Readings::default();
    let mut ends_at = Vec::with_capacity(rows.len());
    let mut end = 0u64;
    let row_of = |off: u64, ends_at: &[u64]| ends_at.partition_point(|&e| e < off);
    match program {
        Program::Claude => {
            let mut f = StepFold::new(0, None);
            f.read_at(READ_FP, READ_ID, 0, false);
            let mut scan = FinishedScan::default();
            let (mut standing, mut login) = (StandingScan::default(), LoginScan::default());
            for (n, line) in rows.iter().enumerate() {
                end += line.len() as u64 + 1;
                ends_at.push(end);
                f.push(line.as_bytes(), end);
                r.ends.push(f.end());
                scan.line(line);
                r.finished.push(Finish {
                    finished: scan.finished(),
                    settled: scan.settled(),
                    said_done: scan.said_done(),
                    irreversible: scan.said_irreversible(),
                    last: scan.last_turn(),
                });
                standing.line(line);
                r.standing.push(Standing::of(&standing));
                login.line(line);
                r.login.push((login.wall(), login.lifted_at()));
                if let Some(mut notice) = notice_read(line) {
                    let reading = Reading::of(&notice.text);
                    notice.text.clear();
                    r.notices.push((n, reading, notice));
                }
                r.reads.push(claude_reads(line));
            }
            let (a, b) = (f.take(), f.take());
            (r.lost, r.overflow, r.end) = (a.lost + b.lost, a.overflow + b.overflow, b.end);
            r.steps = a
                .steps
                .iter()
                .chain(&b.steps)
                .map(|s| StepRead {
                    row: row_of(s.pos.off, &ends_at),
                    at_ms: s.pos.at_ms,
                    state: s.state,
                    ev: s.ev,
                })
                .collect();
        }
        Program::Codex => {
            let mut f = CodexStepFold::new(0);
            f.bind(READ_FP, READ_ID, 0);
            let mut spend = TurnReader::default();
            for line in rows {
                end += line.len() as u64 + 1;
                ends_at.push(end);
                f.push(line.as_bytes(), end);
                r.spend.push(spend.line(line));
                r.limits.push(rollout_rate_limits(line));
                r.reads.push(codex_reads(line));
            }
            let b = f.take();
            (r.lost, r.overflow, r.end) = (b.lost, b.overflow, b.end);
            r.steps = b
                .steps
                .iter()
                .map(|s| StepRead {
                    row: row_of(s.pos.off, &ends_at),
                    at_ms: s.pos.at_ms,
                    state: s.state,
                    ev: s.ev,
                })
                .collect();
        }
    }
    r
}

/// One to one: `a` always maps to `b` and `b` from `a`.
#[derive(Default)]
struct OneToOne {
    fwd: HashMap<String, String>,
    back: HashMap<String, String>,
}

impl OneToOne {
    fn same(&mut self, a: &StepId, b: &StepId) -> Result<(), String> {
        self.pair(a.as_str(), b.as_str())
    }

    fn pair(&mut self, a: &str, b: &str) -> Result<(), String> {
        let (a, b) = (a.to_string(), b.to_string());
        let f = self.fwd.entry(a.clone()).or_insert_with(|| b.clone());
        let g = self.back.entry(b.clone()).or_insert_with(|| a.clone());
        if *f == b && *g == a {
            Ok(())
        } else {
            Err(format!("ids {a} and {b} are not one to one"))
        }
    }
}

/// A step's kind, as a failure names it.
fn kind(e: &StepEvent) -> &'static str {
    match e {
        StepEvent::Bind { .. } => "bind",
        StepEvent::Prompt { .. } => "prompt",
        StepEvent::Interrupt => "interrupt",
        StepEvent::ToolUse { .. } => "tool",
        StepEvent::ToolResult { .. } => "result",
        StepEvent::TurnEnd { .. } => "end",
        StepEvent::EndWall { .. } => "endwall",
        StepEvent::Compact => "compact",
        StepEvent::Model { .. } => "model",
        StepEvent::Off { .. } => "off",
    }
}

/// The step events of two folds agree: the same kinds and states, closed
/// words and numbers, ids one to one.
fn same_event(a: &StepEvent, b: &StepEvent, ids: &mut OneToOne) -> Result<(), String> {
    use StepEvent as E;
    match (a, b) {
        (E::Bind { len: x }, E::Bind { len: y }) if x == y => Ok(()),
        (E::Prompt { vref: x }, E::Prompt { vref: y }) => ids.same(x, y),
        (E::Interrupt, E::Interrupt) | (E::Compact, E::Compact) => Ok(()),
        (
            E::ToolUse {
                id: i,
                tool: t,
                cls: c,
                subagent: s,
            },
            E::ToolUse {
                id: j,
                tool: u,
                cls: d,
                subagent: r,
            },
        ) => {
            ids.same(i, j)?;
            let named = !t.as_str().starts_with("mcp__") && tool_class(t.as_str()).0 != "other";
            if c != d || s != r || (named && t.as_str() != u.as_str()) {
                return Err(format!("tool {t:?} {c} became {u:?} {d}"));
            }
            Ok(())
        }
        (E::ToolResult { id: i, error: x }, E::ToolResult { id: j, error: y }) if x == y => {
            ids.same(i, j)
        }
        (E::TurnEnd { .. }, E::TurnEnd { .. })
        | (E::EndWall { .. }, E::EndWall { .. })
        | (E::Model { .. }, E::Model { .. })
        | (E::Off { .. }, E::Off { .. })
            if a == b =>
        {
            Ok(())
        }
        _ => Err(format!("{a:?} became {b:?}")),
    }
}

/// Two ends agree, the second's time `shift_ms` after the first's: the
/// newest entry's class, the close, the wall and every class it confirms.
fn same_end(a: &End, b: &End, shift_ms: i64) -> bool {
    let ms = |t: u64| i64::try_from(t).unwrap_or(i64::MAX);
    a.last == b.last
        && a.closed == b.closed
        && a.at_ms.map(|t| ms(t).saturating_add(shift_ms)) == b.at_ms.map(ms)
        && a.wall() == b.wall()
        && CLASSES.iter().all(|&k| a.confirms(k) == b.confirms(k))
}

/// Two spend rows agree, the second's turn stamped `shift_s` after the
/// first's.
fn same_spend(a: &SpendRow, b: &SpendRow, shift_s: i64) -> bool {
    match (a, b) {
        (SpendRow::Turn(x), SpendRow::Turn(y)) => {
            x.at.saturating_add(shift_s) == y.at
                && x.model == y.model
                && x.tokens == y.tokens
                && x.tier == y.tier
                && x.key == y.key
        }
        _ => a == b,
    }
}

/// Two standing limits agree, the second's times `shift_s` after the
/// first's and its row's id the first's one to one.
fn same_standing(a: &Standing, b: &Standing, shift_s: i64, ids: &mut OneToOne) -> bool {
    let at = |t: Option<u64>| t.map(|t| i64::try_from(t).unwrap_or(i64::MAX));
    let moved = |x: Option<u64>, y: Option<u64>| at(x).map(|t| t.saturating_add(shift_s)) == at(y);
    let credits = |x: &Option<CreditsWord>, y: &Option<CreditsWord>| match (x, y) {
        (
            Some(CreditsWord::Off {
                reason: r,
                until: u,
            }),
            Some(CreditsWord::Off {
                reason: q,
                until: v,
            }),
        ) => r == q && moved(*u, *v),
        _ => x == y,
    };
    let wall = match (&a.wall, &b.wall) {
        (Some(x), Some(y)) => {
            moved(Some(x.at), Some(y.at))
                && moved(x.until, y.until)
                && x.window == y.window
                && credits(&x.credits, &y.credits)
        }
        (x, y) => x == y,
    };
    let limit = match (&a.limit, &b.limit) {
        (Some((r, x)), Some((q, y))) => {
            r == q
                && moved(x.until, y.until)
                && x.window == y.window
                && moved(x.at, y.at)
                && match (&x.row, &y.row) {
                    (Some(i), Some(j)) => ids.pair(i, j).is_ok(),
                    (i, j) => i == j,
                }
        }
        (x, y) => x == y,
    };
    moved(a.until, b.until) && moved(a.reset, b.reset) && a.spend == b.spend && wall && limit
}

/// Two rows' Codex limits readings agree, the second's times `shift_s`
/// after the first's.
fn same_limits(a: &[CodexLimits], b: &[CodexLimits], shift_s: i64) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.at_unix.saturating_add(shift_s) == y.at_unix
                && x.at_ms.saturating_add(shift_s.saturating_mul(1000)) == y.at_ms
                && x.family == y.family
                && x.name == y.name
                && x.windows.len() == y.windows.len()
                && x.windows.iter().zip(&y.windows).all(|(v, w)| {
                    v.slot == w.slot
                        && v.used_percent.to_bits() == w.used_percent.to_bits()
                        && v.window_minutes == w.window_minutes
                        && v.resets_at.map(|t| t.saturating_add(shift_s)) == w.resets_at
                })
                && x.credits == y.credits
                && x.plan_type == y.plan_type
                && x.individual_limit.map(f64::to_bits) == y.individual_limit.map(f64::to_bits)
                && x.spend_control_reached == y.spend_control_reached
                && x.reached_type == y.reached_type
                && x.account == y.account
        })
}

/// Whether `b`, a reduction's [`Readings`], READS AS `a`, its source's
/// (ruling T7's fold check, which the reducer and its tests share): the same
/// steps at the same rows in the same states, the same closed words and
/// numbers, ids one to one, times `shift_days` apart (a bind's excepted);
/// the same losses; the same end after every row and at the last (its wall
/// and the classes it confirms); every notice read as its source's (its
/// words' reading, its typed fields, its wall, what it confirms and the
/// limit it names); the standing limit and the login wall after every row,
/// their times the same days apart; and the spend fold's and the limits
/// reader's rows, their stamps the same days apart. [`FinishedScan`]'s
/// readings and the rows as the harness's shapes read them are not
/// compared: they read the words a reduction empties or remaps (a prompt's,
/// a reply's, an id), so a candidate is judged against the reduction with
/// every candidate kept ([`Candidates::by_readings`]).
///
/// # Errors
/// The first reading that differs, named.
pub fn reads_as(a: &Readings, b: &Readings, shift_days: i64) -> Result<(), String> {
    let shift = shift_days.saturating_mul(86_400_000);
    let kinds =
        |r: &Readings| -> Vec<&'static str> { r.steps.iter().map(|s| kind(&s.ev)).collect() };
    if a.steps.len() != b.steps.len() {
        return Err(format!(
            "{} steps became {}: {:?} / {:?}",
            a.steps.len(),
            b.steps.len(),
            kinds(a),
            kinds(b)
        ));
    }
    let mut ids = OneToOne::default();
    for (sa, sb) in a.steps.iter().zip(&b.steps) {
        let ms = |s: &StepRead| i64::try_from(s.at_ms).unwrap_or(i64::MAX);
        // A bind is dated at the floor, and so is a step of a row with no
        // readable stamp (a latch): neither moves with the file's times.
        let timed = !matches!(sa.ev, StepEvent::Bind { .. }) && (sa.at_ms, sb.at_ms) != (0, 0);
        if sa.row != sb.row || sa.state != sb.state || (timed && ms(sb) - ms(sa) != shift) {
            return Err(format!(
                "{:?} at row {} ({}) became {:?} at row {} ({})",
                sa.ev, sa.row, sa.at_ms, sb.ev, sb.row, sb.at_ms
            ));
        }
        same_event(&sa.ev, &sb.ev, &mut ids).map_err(|e| format!("row {}: {e}", sa.row))?;
    }
    if (a.lost, a.overflow) != (b.lost, b.overflow) {
        return Err(format!(
            "lost/overflow {:?} became {:?}",
            (a.lost, a.overflow),
            (b.lost, b.overflow)
        ));
    }
    if !same_end(&a.end, &b.end, shift) {
        return Err(format!("the end {:?} became {:?}", a.end, b.end));
    }
    if a.ends.len() != b.ends.len() {
        return Err(format!("{} rows became {}", a.ends.len(), b.ends.len()));
    }
    if let Some(n) = (0..a.ends.len()).find(|&n| !same_end(&a.ends[n], &b.ends[n], shift)) {
        return Err(format!(
            "the end after row {n}, {:?}, became {:?}",
            a.ends[n], b.ends[n]
        ));
    }
    if a.notices.len() != b.notices.len() {
        return Err(format!(
            "{} notices became {}",
            a.notices.len(),
            b.notices.len()
        ));
    }
    for ((ra, ta, na), (rb, tb, nb)) in a.notices.iter().zip(&b.notices) {
        if ra != rb || ta != tb || na != nb {
            return Err(format!(
                "the notice at row {ra}, read {ta:?} {na:?}, became row {rb}'s {tb:?} {nb:?}"
            ));
        }
    }
    let shift_s = shift_days.saturating_mul(86_400);
    if a.standing.len() != b.standing.len() || a.login.len() != b.login.len() {
        return Err(format!(
            "{} rows became {}",
            a.standing.len(),
            b.standing.len()
        ));
    }
    if let Some(n) = (0..a.standing.len())
        .find(|&n| !same_standing(&a.standing[n], &b.standing[n], shift_s, &mut ids))
    {
        return Err(format!(
            "the standing limit after row {n}, {:?}, became {:?}",
            a.standing[n], b.standing[n]
        ));
    }
    let lifted = |x: Option<u64>| x.map(|t| i64::try_from(t).unwrap_or(i64::MAX));
    if let Some(n) = (0..a.login.len()).find(|&n| {
        let ((wa, la), (wb, lb)) = (a.login[n], b.login[n]);
        wa != wb || lifted(la).map(|t| t.saturating_add(shift_s)) != lifted(lb)
    }) {
        return Err(format!(
            "the login wall after row {n}, {:?}, became {:?}",
            a.login[n], b.login[n]
        ));
    }
    if a.spend.len() != b.spend.len() {
        return Err(format!(
            "{} spend rows became {}",
            a.spend.len(),
            b.spend.len()
        ));
    }
    if let Some(n) = (0..a.spend.len()).find(|&n| !same_spend(&a.spend[n], &b.spend[n], shift_s)) {
        return Err(format!(
            "the spend fold's row {n}, {:?}, became {:?}",
            a.spend[n], b.spend[n]
        ));
    }
    if a.limits.len() != b.limits.len() {
        return Err(format!(
            "{} limits rows became {}",
            a.limits.len(),
            b.limits.len()
        ));
    }
    if let Some(n) =
        (0..a.limits.len()).find(|&n| !same_limits(&a.limits[n], &b.limits[n], shift_s))
    {
        return Err(format!(
            "the limits reader's row {n}, {:?}, became {:?}",
            a.limits[n], b.limits[n]
        ));
    }
    Ok(())
}

/// Where a candidate closed word sits: its row (0-based, among the file's
/// rows), the member and element indices from the row's root down to it,
/// and its pair ([`Candidates::pairs`]).
#[derive(Clone, Debug)]
struct Site {
    row: usize,
    trail: Vec<usize>,
    pair: usize,
}

/// The string `trail` leads to in `v`.
fn str_at<'a>(v: &'a Node, trail: &[usize]) -> Option<&'a str> {
    match (v, trail.split_first()) {
        (Node::Str(s), None) => Some(s),
        (Node::Arr(a), Some((i, rest))) => str_at(a.get(*i)?, rest),
        (Node::Obj(m), Some((i, rest))) => str_at(&m.get(*i)?.1, rest),
        _ => None,
    }
}

/// Empty the string `trail` leads to in `v`.
fn blank_at(v: &mut Node, trail: &[usize]) {
    match (v, trail.split_first()) {
        (Node::Str(s), None) => s.clear(),
        (Node::Arr(a), Some((i, rest))) => {
            if let Some(x) = a.get_mut(*i) {
                blank_at(x, rest);
            }
        }
        (Node::Obj(m), Some((i, rest))) => {
            if let Some((_, x)) = m.get_mut(*i) {
                blank_at(x, rest);
            }
        }
        _ => {}
    }
}

/// A file's CANDIDATE CLOSED WORDS (ruling U7): its reduction with every one
/// kept, each `(key, word)` pair the rule keeps a word of, in the order the
/// file first names it, and every place one sits; what [`Rules::words`]
/// chooses from.
pub struct Candidates<'a> {
    program: Program,
    /// The file reduced.
    source: &'a str,
    shift_days: i64,
    rows: Vec<Node>,
    dumped: Vec<String>,
    pairs: Vec<(String, String)>,
    sites: Vec<Site>,
    /// Each row's sites.
    by_row: Vec<Vec<usize>>,
}

impl<'a> Candidates<'a> {
    fn new(
        program: Program,
        source: &'a str,
        shift_days: i64,
        rows: Vec<Node>,
        pairs: Vec<(String, String)>,
        sites: Vec<Site>,
    ) -> Result<Self, String> {
        let mut by_row = vec![Vec::new(); rows.len()];
        for (i, s) in sites.iter().enumerate() {
            let word = pairs.get(s.pair).map(|(_, w)| w.as_str());
            if rows.get(s.row).and_then(|r| str_at(r, &s.trail)) != word {
                return Err(format!(
                    "row {}: a candidate word is not where the walk found it",
                    s.row + 1
                ));
            }
            by_row[s.row].push(i);
        }
        let dumped = rows.iter().map(Node::dump).collect();
        Ok(Candidates {
            program,
            source,
            shift_days,
            rows,
            dumped,
            pairs,
            sites,
            by_row,
        })
    }

    /// The candidate pairs, `(key, word)`, in the order the file first names
    /// them.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only a negative control picks pairs by their words"
        )
    )]
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// Every candidate kept: the reduction as the string rule alone writes
    /// it.
    #[must_use]
    pub fn every(&self) -> Vec<bool> {
        vec![true; self.pairs.len()]
    }

    /// The reduction's rows with the pairs `keep` flags kept, and every
    /// other pair's words emptied wherever they sit.
    fn lines(&self, keep: &[bool]) -> Vec<Cow<'_, str>> {
        let gone = |s: &&Site| !keep.get(s.pair).copied().unwrap_or(true);
        self.rows
            .iter()
            .zip(&self.dumped)
            .zip(&self.by_row)
            .map(|((row, dumped), at)| {
                let blank: Vec<&Site> = at.iter().map(|&i| &self.sites[i]).filter(gone).collect();
                if blank.is_empty() {
                    Cow::Borrowed(dumped.as_str())
                } else {
                    let mut r = row.clone();
                    for s in blank {
                        blank_at(&mut r, &s.trail);
                    }
                    Cow::Owned(r.dump())
                }
            })
            .collect()
    }

    /// The [`Readings`] of the reduction `keep` makes.
    #[must_use]
    pub fn readings(&self, keep: &[bool]) -> Readings {
        let lines = self.lines(keep);
        let rows: Vec<&str> = lines.iter().map(AsRef::as_ref).collect();
        readings_of(&rows, self.program)
    }

    /// THE READERS DECIDE (ruling U7). With every candidate kept the
    /// reduction must read as its source ([`reads_as`]), else the file is
    /// refused: nothing restores it. Then the pairs are chosen by the
    /// reduction's [`Readings`] ([`choose`]): each is kept only when
    /// blanking it alone, across the file, changes them.
    ///
    /// # Errors
    /// With every candidate kept, the reduction does not read as its source.
    pub fn by_readings(&self) -> Result<Vec<bool>, String> {
        let all = self.every();
        reads_as(
            &readings(self.source, self.program),
            &self.readings(&all),
            self.shift_days,
        )
        .map_err(|e| {
            format!("it does not read as its source with every closed word kept ({e}): refused")
        })?;
        Ok(choose(all.len(), |keep| self.readings(keep)))
    }
}

/// THE CHOICE OF `n` CANDIDATE PAIRS BY A READING (ruling U7): `read` of the
/// flags kept, which with every flag set is the reading to keep. Each pair
/// is kept only when blanking it alone changes that reading. Blanked
/// together, the rest may change it (two rows a reader reads either of):
/// they are put back, in the order the file first names them, until it
/// reads as it did. Last, a pair another kept pair made needless is blanked
/// again, until each kept pair, blanked alone, changes the reading. So
/// what is kept reads as every pair kept, and no pair kept is needless.
fn choose<R: PartialEq>(n: usize, read: impl Fn(&[bool]) -> R) -> Vec<bool> {
    let all = vec![true; n];
    let base = read(&all);
    let without = |keep: &[bool], i: usize| {
        let mut k = keep.to_vec();
        k[i] = false;
        k
    };
    let mut keep: Vec<bool> = (0..n).map(|i| read(&without(&all, i)) != base).collect();
    let mut next = 0;
    while next < n && read(&keep) != base {
        while next < n && keep[next] {
            next += 1;
        }
        if let Some(k) = keep.get_mut(next) {
            *k = true;
        }
    }
    loop {
        let mut moved = false;
        for i in 0..n {
            if keep[i] {
                let fewer = without(&keep, i);
                if read(&fewer) == base {
                    keep = fewer;
                    moved = true;
                }
            }
        }
        if !moved {
            return keep;
        }
    }
}

/// THE OUTPUT-SIDE CHECK (ruling U7): the closed words the reduction `text`
/// keeps that no reading reads — each candidate pair ([`Candidates::pairs`])
/// whose words, blanked alone, leave its [`readings`] as they were. A
/// reduction `lab reduce` writes keeps none.
///
/// # Errors
/// `text` is no file `lab reduce` reads (a line that is no JSON value, a
/// notice that reads as no canonical phrase).
pub fn unread_words(text: &str, program: Program) -> Result<Vec<(String, String)>, String> {
    let w = walked(text, program, &P11)?;
    let own = text
        .lines()
        .filter(|l| is_row(l))
        .map(|l| parse(l.trim()))
        .collect::<Result<Vec<Node>, String>>()?;
    let c = Candidates::new(program, text, 0, own, w.pairs, w.sites)?;
    let all = c.every();
    let base = c.readings(&all);
    Ok((0..all.len())
        .filter(|&i| {
            let mut k = all.clone();
            k[i] = false;
            c.readings(&k) == base
        })
        .map(|i| c.pairs[i].clone())
        .collect())
}

/// THE VERB'S CHECK OF WHAT IT WRITES: the reduction `r` of `source` reads
/// as its source ([`reads_as`], ruling T7) and keeps no closed word no
/// reader reads ([`unread_words`], ruling U7). `lab reduce` writes nothing
/// that fails it.
///
/// # Errors
/// The first that fails, named.
pub fn check_written(source: &str, r: &Reduction, program: Program) -> Result<(), String> {
    reads_as(
        &readings(source, program),
        &readings(&r.text, program),
        r.shift_days,
    )
    .map_err(|e| format!("it does not read as its source ({e}): refused"))?;
    let unread = unread_words(&r.text, program)?;
    if unread.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "it keeps {} closed words no reader reads ({:?}): refused",
            unread.len(),
            unread
        ))
    }
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// `reduce <transcript|rollout> --program claude|codex [--out FILE]`: the
/// reduction to FILE (never one that exists) or to stdout, and what it did
/// to stderr. Both paths are the caller's, read against the directory it
/// ran the lab from (`paths::from_caller`), since the lab itself works
/// from `/`.
///
/// What it writes passes the output-side check ([`unread_words`]: every
/// closed word kept is one a reader reads), or nothing is written.
///
/// # Errors
/// Usage; the file cannot be read; a line is not JSON or a notice reads as
/// no canonical phrase; the reduction cannot read as its source, or keeps a
/// word no reader reads; FILE exists or cannot be written.
pub fn cmd_reduce(args: &[String]) -> Res {
    reduce_verb(args, reduce)
}

/// [`cmd_reduce`], reducing `by` ([`reduce`]): the verb's negative controls
/// reduce by a rule that keeps what [`check_written`] refuses.
fn reduce_verb(args: &[String], by: fn(&str, Program) -> Result<Reduction, String>) -> Res {
    let usage = || {
        Fail::usage(format!(
            "usage: {LAB} reduce <transcript|rollout> --program claude|codex [--out FILE]"
        ))
    };
    let (mut file, mut program, mut out) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--program" => {
                program = Some(match it.next().map(String::as_str) {
                    Some("claude") => Program::Claude,
                    Some("codex") => Program::Codex,
                    _ => return Err(usage()),
                });
            }
            "--out" => out = Some(crate::paths::from_caller(it.next().ok_or_else(usage)?)),
            s if s.starts_with('-') => return Err(usage()),
            s if file.is_none() => file = Some(crate::paths::from_caller(s)),
            _ => return Err(usage()),
        }
    }
    let (Some(file), Some(program)) = (file, program) else {
        return Err(usage());
    };
    let source =
        std::fs::read_to_string(&file).map_err(|e| Fail::die(format!("reduce: {file}: {e}")))?;
    let r = by(&source, program).map_err(|e| Fail::die(format!("reduce: {file}: {e}")))?;
    // What is written reads as its source and keeps no word no reader
    // reads, or nothing is written.
    check_written(&source, &r, program)
        .map_err(|e| Fail::die(format!("reduce: {file}: its reduction: {e}")))?;
    match &out {
        Some(path) => {
            // Never over an existing file: a reduction is written once.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| {
                    Fail::die(format!(
                        "reduce: {path}: {e} (a reduction is never written over a file)"
                    ))
                })?;
            f.write_all(r.text.as_bytes())
                .map_err(|e| Fail::die(format!("reduce: {path}: {e}")))?;
        }
        None => print!("{}", r.text),
    }
    eprintln!(
        "reduced {} rows of a {} file (build {}): {} ids remapped, {} notices as their canonical phrase, {} of {} closed words kept where a reader reads them, times moved {:+} days, {} comment lines dropped{}{}",
        r.rows,
        program.name(),
        r.build.as_deref().unwrap_or("-"),
        r.ids,
        r.notices,
        r.words.1,
        r.words.0,
        r.shift_days,
        r.comments,
        if r.again {
            "; the source was a reduction"
        } else {
            ""
        },
        out.map(|p| format!("; written to {p}")).unwrap_or_default()
    );
    Ok(0)
}

#[cfg(test)]
#[path = "reduce_tests.rs"]
mod tests;
