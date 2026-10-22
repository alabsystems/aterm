// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub: a local stand-in for the Anthropic Messages API, the OpenAI
//! Responses API and the Gemini API, so the REAL Claude Code, Codex, Gemini
//! CLI, agy and Muse Code TUIs can be driven without spending anyone's tokens
//! or touching anyone's login. Claude Code is pointed at it with
//! `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>`, Codex with a model provider
//! whose `base_url = "http://127.0.0.1:<port>/v1"` and `wire_api =
//! "responses"` (`codex` writes one), Gemini CLI and agy with
//! `GOOGLE_GEMINI_BASE_URL` ([`gemini`]), Muse Code with `--base-url
//! http://127.0.0.1:<port>` ([`muse`]); it runs jailed, as `aterm-lab stub
//! …`, started by `up`.
//!
//! Routes: `POST /v1/messages` (SSE when the request says `"stream": true`,
//! else one JSON message); `POST /v1/messages/count_tokens`
//! (`{"input_tokens": N}`); `POST /v1/responses` (Codex: the Responses API's
//! SSE — `response.created`, `output_item.added`, `output_text.delta` × N,
//! `output_text.done`, `output_item.done`, `response.completed` with usage —
//! or the error a mode names in the shape Codex parses); `GET
//! /backend-api/wham/accounts/check` (Codex's workspace discovery under a
//! ChatGPT login: one personal account, the lab token's own; the TUI does not
//! start without it); `GET /backend-api/wham/usage` (the account usage a
//! ChatGPT login's TUI polls for its `/status` card and its limit status-line
//! items: the `--headers-file` windows and credits in the backend's payload);
//! `POST /oauth/token` (Codex's token refresh, which
//! `CODEX_REFRESH_TOKEN_URL_OVERRIDE` points here: `<modes-dir>/refresh.json`
//! when there is one, `{"status", "json"}`, else 401 `refresh_token_expired`);
//! the claude.ai account routes `GET /api/oauth/usage`, `/api/oauth/profile`,
//! `/api/claude_cli_profile` and `/api/claude_cli/bootstrap`, answered from
//! `--account-file` ([`account`]); the Gemini API's `POST
//! /v1beta/models/<model>:streamGenerateContent`, `:generateContent` and
//! `:countTokens`, and `GET /v1beta/models` ([`gemini`]); Muse Code's `GET
//! /muse-code/models` and `POST /responses` ([`muse`]); anything else a 404
//! in Anthropic's error shape. Control (any method):
//! `/__mode[?m=NAME]` reads or switches the mode, `/__status` the counters,
//! `/__log[?n=N]` the last log lines, `/__reset` zeroes the counters and the
//! one-shot budget, `/__note?t=TEXT` writes `NOTE TEXT` into the log (a
//! scenario's marker). The mode can also be written into `--mode-file`
//! (re-read when it changes).
//!
//! Modes: `ok` a short streamed reply; `long` three paragraphs;
//! `numbered` a reply holding `1. 2. 3.` (Claude hides surveys under one);
//! `overloaded` 529; `too-long` 400 `prompt is too long`; `compact-fails` a
//! compaction request fails as too-long while ordinary turns succeed; `slow`
//! one `tick N` a second for `--slow-seconds`; `rate-limit` 429 with
//! `retry-after`; `limit:CLAIM:RESET` a SUBSCRIPTION usage limit (429 with the
//! unified-limit headers until the unix second RESET, then `ok`; Claude Code
//! reads them only on its subscriber path, `claude TAG --subscriber`); `401`,
//! `500` and `bad-key` the walls every API answers in its own shape (Claude
//! Code's `authentication_error` 401, `bad-key` the same, and `api_error` 500;
//! Codex's and Muse Code's `invalid_api_key` 401, `bad-key` the same, and
//! `server_error` 500 ([`responses_error`]); Google's
//! `UNAUTHENTICATED` 401, `INTERNAL` 500 and the 400 `API_KEY_INVALID` a key
//! that is no key gets);
//! `bg[:SLEEP[:SLOW]]` the prompt `bgstart` runs `sleep SLEEP` as a BACKGROUND
//! Bash task, its result gets a one-line end_turn and any other request (the
//! wake) is answered slowly; `tool` the prompt `toolstart` calls what
//! `--tool-file` holds (one call, or a list answered one per request; a call
//! with `"when": "<text>"` belongs to the conversation — a subagent's, or a
//! worker's whole session — whose FIRST prompt holds the text; a step
//! `{"text": "…"}` with no `name` is a text reply that ends the turn, and the
//! script goes on at the next request, a later user row's: a manager's
//! background wake; `@SID:<k>@` in a call's input is the first session id,
//! `s-<hex>`, of the conversation's k-th tool result, so a call can name the
//! worker an earlier one started); any other name with
//! `<modes-dir>/<name>.json` but `refresh` (the refresh's answer,
//! [`not_custom`]) is a CUSTOM mode (`{"status", "headers", "json"|"body",
//! "stream", "text", "delay_ms", "times", "auth", "replies"}`; `times`: the
//! first N requests, then `ok`; `auth`: only a request that carried that lab
//! credential gets it, the others are answered as `ok`; its `headers` go out
//! on a streamed answer too; `replies`: `[{"when", "last", "after", "text"}, …]`, a
//! turn's text picked by the words its conversation's user texts, its
//! newest one and one before it hold, in every API, a side request answered
//! as `ok` ([`scripted_reply`])).
//!
//! A Codex request (`/v1/responses`) is answered by the same modes: `ok`,
//! `long`, `numbered` and `slow` as text; `overloaded` as a 503
//! `server_is_overloaded`; `rate-limit` as a 429 `rate_limit_exceeded`; `401`
//! as a 401 `invalid_api_key`; `500` as a 500 `server_error`
//! ([`responses_error`]); `too-long` as a `response.failed` event with
//! `context_length_exceeded`;
//! `limit:CLAIM:RESET[:PLAN]` as Codex's usage limit, a 429 whose body is
//! `{"error":{"type":"usage_limit_reached","resets_at":RESET,
//! "limit_window_minutes":300|10080,"plan_type":PLAN}}` with the claim's
//! window at 100% in the `x-codex-*` headers (`five_hour` the primary slot,
//! `seven_day` the secondary; Muse Code draws its own usage wall from the same
//! body); `tool` and `bg` as `ok`. A custom mode streams its `text` unless it
//! says `"stream": false` or another status. A Muse Code turn (`/responses`)
//! is answered the same way in Muse Code's own stream, and `tool` there calls
//! the script's tools in its `muse` namespace ([`muse`]). A Gemini API turn is
//! answered by them in Google's shapes, its side requests in every mode
//! ([`gemini`]).
//!
//! `--headers-file` (re-read at every request): its flat entries (`ratelimits
//! TAG …` writes them) are added to EVERY answer of both APIs, success and
//! error — Codex's `x-codex-primary-used-percent` family and its credits
//! headers; a value `"@NOW+N@"` is the unix second N seconds from when the
//! answer is sent. Its `{"on200": {header: value}, "on429": {header: value}}`
//! blocks (`headers TAG '<json>'` writes them) are extra headers on the 200
//! and 429 answers to `/v1/messages` — the
//! `anthropic-ratelimit-unified-*` figures a subscriber's Claude Code reads its
//! usage from on every answer (no mode sends them on a 200). A value
//! `"@SEQ:BASE:STEP@"` is `BASE + STEP*k`, `k` the number of 200 answers that
//! carried on200 headers before this one (clamped to 0..1, four decimals), so
//! the figures rise as the session works, from the first answer that carries
//! them ([`rate_headers`]). Each verb changes only its own entries and keeps
//! the other's ([`headers_file_with`]).
//!
//! Every request is logged to `--log` (method, path, status, body length, a
//! one-line summary with
//! `kind=main|aux|compact|quota|codex|codex-json|muse|muse-json|muse-side|muse-models`,
//! and `gemini|gemini-json|gemini-aux` for the Gemini API); no header value,
//! key or token is ever logged: a Codex or Gemini API request's credential is
//! logged as the lab's own label for it (`auth=key`: the provider's fixed fake
//! key; `auth=jwt:<label>`: a token the lab composed, whose payload names its
//! label; `auth=gemini-key`: the fixed fake key of Gemini CLI and agy;
//! `auth=none`; `auth=other(<n> chars)`; a Gemini API request with an
//! `Authorization` header beside its key, both joined by `+`:
//! [`gemini::credential_label`]). `--dump-dir` keeps the
//! request bodies of every API (the newest [`DUMPS_KEPT`]): a turn of the
//! conversation as `req-<n>.json`, a side call ([`is_side`]) as
//! `side-<n>.json` ([`Stub::dump`]), so a reader of the turns
//! ([`turn_dumps`]) never takes a side call's copy of the conversation for
//! a turn.
//! `--proxy-port` also runs a BLACK-HOLE proxy: it reads the request line a
//! client sends its `HTTP(S)_PROXY`, logs it (`PROXY refused: CONNECT
//! api.anthropic.com:443 …`), answers 502 at once and closes — a dead proxy
//! that says what Claude Code tried to reach.

use std::io::{self, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::pyjson::{COMPACT, J};
use crate::util::{now, one_line, py_repr, stamp};

pub mod account;
pub mod gemini;
pub mod muse;

/// What the stub was started with.
#[derive(Clone, Debug)]
pub struct Args {
    /// Where it listens (loopback only).
    pub host: String,
    /// The API port (0: the kernel picks).
    pub port: u16,
    /// Where the chosen API port is written.
    pub port_file: Option<PathBuf>,
    /// The black-hole proxy port (0: the kernel picks; `None`: off).
    pub proxy_port: Option<u16>,
    /// Where the chosen proxy port is written.
    pub proxy_port_file: Option<PathBuf>,
    /// The starting mode.
    pub mode: String,
    /// A file holding the mode name, re-read when it changes.
    pub mode_file: Option<PathBuf>,
    /// The directory of custom modes (`<name>.json`).
    pub modes_dir: Option<PathBuf>,
    /// The request log (stdout when none).
    pub log: Option<PathBuf>,
    /// Where request bodies are kept.
    pub dump_dir: Option<PathBuf>,
    /// When this file has text, the `ok` reply is exactly that text.
    pub reply_file: Option<PathBuf>,
    /// The `tool` mode's script.
    pub tool_file: Option<PathBuf>,
    /// Extra headers on `/v1/messages` answers ([`rate_headers`]), re-read per request.
    pub headers_file: Option<PathBuf>,
    /// The bodies of the claude.ai account routes ([`account`]), re-read per request.
    pub account_file: Option<PathBuf>,
    /// Ticks of a `slow` reply.
    pub slow_seconds: i64,
    /// The `retry-after` of `rate-limit`.
    pub retry_after: String,
    /// A descriptor whose end of file ends the stub (`--lifeline-fd`): the lab's driver went away.
    pub lifeline_fd: Option<i32>,
    /// A pipe's write end the stub writes one byte to, then closes, once it
    /// listens and its port files are written (`--ready-fd`): the lab waits on
    /// it instead of polling the files (`aterm_agent::ready`).
    pub ready_fd: Option<i32>,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            host: "127.0.0.1".into(),
            port: 0,
            port_file: None,
            proxy_port: None,
            proxy_port_file: None,
            mode: "ok".into(),
            mode_file: None,
            modes_dir: None,
            log: None,
            dump_dir: None,
            reply_file: None,
            tool_file: None,
            headers_file: None,
            account_file: None,
            slow_seconds: 30,
            retry_after: "5".into(),
            lifeline_fd: None,
            ready_fd: None,
        }
    }
}

/// Parse `aterm-lab stub` arguments.
///
/// # Errors
/// An unknown flag, a missing value, or a number that does not parse.
pub fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("stub: {flag} needs a value"))
        };
        let num = |v: String| {
            v.parse::<i64>()
                .map_err(|_| format!("stub: {flag}: not a number: {v}"))
        };
        match flag.as_str() {
            "--host" => a.host = val()?,
            "--port" => {
                a.port = u16::try_from(num(val()?)?).map_err(|_| "stub: --port out of range")?
            }
            "--port-file" => a.port_file = Some(val()?.into()),
            "--proxy-port" => {
                let n = num(val()?)?;
                a.proxy_port = if n < 0 {
                    None
                } else {
                    Some(u16::try_from(n).map_err(|_| "stub: --proxy-port out of range")?)
                };
            }
            "--proxy-port-file" => a.proxy_port_file = Some(val()?.into()),
            "--mode" => a.mode = val()?,
            "--mode-file" => a.mode_file = Some(val()?.into()),
            "--modes-dir" => a.modes_dir = Some(val()?.into()),
            "--log" => a.log = Some(val()?.into()),
            "--dump-dir" => a.dump_dir = Some(val()?.into()),
            "--reply-file" => a.reply_file = Some(val()?.into()),
            "--tool-file" => a.tool_file = Some(val()?.into()),
            "--headers-file" => a.headers_file = Some(val()?.into()),
            "--account-file" => a.account_file = Some(val()?.into()),
            "--slow-seconds" => a.slow_seconds = num(val()?)?,
            "--retry-after" => a.retry_after = val()?,
            "--ready-fd" => {
                a.ready_fd = Some(
                    i32::try_from(num(val()?)?)
                        .ok()
                        .filter(|fd| *fd > 2)
                        .ok_or("stub: --ready-fd names a descriptor above stdio")?,
                );
            }
            "--lifeline-fd" => {
                a.lifeline_fd = Some(
                    i32::try_from(num(val()?)?).map_err(|_| "stub: --lifeline-fd out of range")?,
                );
            }
            other => return Err(format!("stub: unknown argument {other}")),
        }
    }
    Ok(a)
}

/// How many request dumps the stub keeps, the newest: a lab of a dozen
/// agents' conversations (`lab forever`) and their side calls stays whole.
pub const DUMPS_KEPT: u64 = 2000;

/// The name a TURN's dump starts with: `req-<n>.json` ([`Stub::dump`]).
pub const TURN_DUMP: &str = "req-";
/// The name a SIDE CALL's dump starts with: `side-<n>.json` ([`is_side`]).
pub const SIDE_DUMP: &str = "side-";

/// Whether a request of `kind` (the log's `kind=`) is a SIDE CALL: a request
/// the program makes beside the conversation's turns, never one of them —
/// Claude Code's `aux` (no tools: a title, a topic), `quota` (`max_tokens <=
/// 1`) and `compact` (the conversation's summary); Codex's and Muse Code's
/// structured requests (`codex-json`: the thread title; `muse-json`); every
/// role of Muse Code's but `main` (`muse-side`: its reminder observers, the
/// prompt hint, the away recap); Gemini CLI's `gemini-json` (its Auto
/// model's router, its next-speaker and loop checks) and `gemini-aux`. A
/// side call can carry the whole conversation — Muse Code's reminder
/// observer renders it into ONE user text, `<main-agent-conversation
/// format="model-conversation-xml/v1">` with each message on rows of its own
/// (measured on 1.4.10-R6427.1, the lab's `r10-cv`, 2026-10-22: C4 read that
/// text as the owner's words reaching the agent with a line break) — so a
/// judge of the turns reads `req-<n>.json` alone ([`turn_dumps`]). A body
/// the stub could not read (`opaque`) is kept as a turn: nothing says it is
/// not one.
#[must_use]
pub fn is_side(kind: &str) -> bool {
    matches!(
        kind,
        "aux"
            | "quota"
            | "compact"
            | "codex-json"
            | "muse-json"
            | "muse-side"
            | "gemini-json"
            | "gemini-aux"
    )
}

/// The stub's dumps of the conversations' TURNS in `dir` (`req-<n>.json`;
/// a side call's `side-<n>.json` is none of them, [`is_side`]), `(n, path)`
/// each, the newest first. Empty where `dir` cannot be read.
#[must_use]
pub fn turn_dumps(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut out: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix(TURN_DUMP)?
                .strip_suffix(".json")?
                .parse()
                .ok()?;
            Some((n, e.path()))
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    out
}

/// The counters and the mode, as `/__status` shows them.
#[derive(Debug, Default)]
struct State {
    mode: String,
    mode_file_mtime: Option<std::time::SystemTime>,
    custom_budget: Vec<(String, Option<i64>)>,
    n_requests: i64,
    by_status: Vec<(String, i64)>,
    by_kind: Vec<(String, i64)>,
    proxy_attempts: i64,
}

fn bump(counts: &mut Vec<(String, i64)>, key: &str) -> i64 {
    if let Some(slot) = counts.iter_mut().find(|(k, _)| k == key) {
        slot.1 += 1;
        slot.1
    } else {
        counts.push((key.to_string(), 1));
        1
    }
}

/// A running stub.
pub struct Stub {
    args: Args,
    started: f64,
    state: Mutex<State>,
    log_lock: Mutex<()>,
    /// The 200 answers to `/v1/messages` served so far that carried on200
    /// headers: the `k` of a `@SEQ@` header value.
    served_200: AtomicU32,
    /// The request bodies dumped so far ([`Stub::dump`]): each dump takes
    /// its index from here as it is written, so requests in flight together
    /// (a reply's delay and pace hold `record` back) never share one.
    dumped: AtomicU64,
}

/// The extra headers one `/v1/messages` request's answer carries (`--headers-file`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateHeaders {
    /// On a 200 answer, JSON or SSE.
    pub on200: Vec<(String, String)>,
    /// On a 429 answer.
    pub on429: Vec<(String, String)>,
}

impl RateHeaders {
    /// The log's ` RL200[…] RL429[…]` tail: each header without the unified-limit
    /// prefix, in header-name order.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for (tag, side) in [("RL200", &self.on200), ("RL429", &self.on429)] {
            if side.is_empty() {
                continue;
            }
            let mut rows = side.clone();
            rows.sort();
            let rows: Vec<String> = rows
                .iter()
                .map(|(h, v)| format!("{}={v}", h.replace("anthropic-ratelimit-unified-", "")))
                .collect();
            out.push_str(&format!(" {tag}[{}]", rows.join(",")));
        }
        out
    }
}

/// The headers `spec` (`{"on200": {…}, "on429": {…}}`) puts on the answer after
/// `served` 200s that carried on200 headers. A value is its text, except
/// `"@SEQ:BASE:STEP@"`: `BASE + STEP*served`, clamped to 0..1, with four
/// decimals.
#[must_use]
pub fn rate_headers(spec: &J, served: u32) -> RateHeaders {
    let side = |which: &str| -> Vec<(String, String)> {
        let Some(J::Obj(m)) = spec.get(which) else {
            return Vec::new();
        };
        m.iter()
            .map(|(h, v)| (h.clone(), seq_value(&value_text(v), served)))
            .collect()
    };
    RateHeaders {
        on200: side("on200"),
        on429: side("on429"),
    }
}

/// A header value's text, as Python's `str()` spelled the JSON value.
fn value_text(v: &J) -> String {
    match v {
        J::Str(s) => s.clone(),
        J::Int(i) => i.to_string(),
        J::Float(f) => crate::pyjson::py_float(*f),
        J::Bool(true) => "True".into(),
        J::Bool(false) => "False".into(),
        J::Null => "None".into(),
        other => other.py(),
    }
}

/// `v`, or the figure a `@SEQ:BASE:STEP@` value reaches after `served` answers.
fn seq_value(v: &str, served: u32) -> String {
    let num = |t: &str| {
        let digits = !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '.');
        digits.then(|| t.parse::<f64>().ok()).flatten()
    };
    let parsed = v
        .strip_prefix("@SEQ:")
        .and_then(|r| r.strip_suffix('@'))
        .and_then(|r| r.split_once(':'))
        .and_then(|(base, step)| Some((num(base)?, num(step)?)));
    match parsed {
        Some((base, step)) => format!("{:.4}", (base + step * f64::from(served)).clamp(0.0, 1.0)),
        None => v.to_string(),
    }
}

/// The built-in modes' names: never a custom mode.
const BUILTIN: &[&str] = &[
    "ok",
    "long",
    "numbered",
    "overloaded",
    "too-long",
    "compact-fails",
    "slow",
    "rate-limit",
    "bg",
    "tool",
    "limit",
    "401",
    "500",
    "bad-key",
];

/// The file in `<modes-dir>` that answers Codex's token refresh (`POST
/// /oauth/token`), never a mode of the APIs.
const REFRESH_FILE: &str = "refresh.json";

/// Why `<modes-dir>/<name>.json` would never be served as the custom mode
/// `name`, or `None` when it would: a built-in mode's name (its `:` arguments
/// aside) is answered by that mode whatever a file of the name holds, and
/// `refresh` is the file Codex's token refresh is answered from. Compared
/// without case: the scratch sits on a case-insensitive volume (APFS), where
/// `Refresh.json` IS `refresh.json`.
#[must_use]
pub fn not_custom(name: &str) -> Option<&'static str> {
    let head = name.split(':').next().unwrap_or("");
    if BUILTIN.iter().any(|b| b.eq_ignore_ascii_case(head)) {
        Some("a built-in mode")
    } else if REFRESH_FILE
        .strip_suffix(".json")
        .is_some_and(|r| r.eq_ignore_ascii_case(name))
    {
        Some("the file Codex's token refresh (POST /oauth/token) is answered from")
    } else {
        None
    }
}

/// A random 64-bit id in the API's `…_lab%016x` shape.
fn rand_hex() -> String {
    let mut b = [0u8; 8];
    let _ = aterm_uds::rand::fill(&mut b);
    format!("{:016x}", u64::from_ne_bytes(b))
}

fn err_body(etype: &str, message: &str) -> J {
    J::obj([
        ("type", J::s("error")),
        (
            "error",
            J::obj([("type", J::s(etype)), ("message", J::s(message))]),
        ),
        ("request_id", J::s(format!("req_lab{}", rand_hex()))),
    ])
}

/// `max(12, len / 4)`: the stub's token estimate of a body.
#[must_use]
pub fn est_tokens(raw_len: usize) -> i64 {
    i64::try_from(raw_len / 4).unwrap_or(i64::MAX).max(12)
}

/// A reply as a handful of text deltas: words, gathered until a delta holds 12
/// characters, so the stream looks alive.
#[must_use]
pub fn split_deltas(text: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        if chars[i].is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
        parts.push(chars[start..i].iter().collect());
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for p in parts {
        cur.push_str(&p);
        if cur.chars().count() >= 12 {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// The text of a message's `content`: the string, or its text blocks joined by newlines.
#[must_use]
pub fn text_of(content: Option<&J>) -> String {
    match content {
        Some(J::Str(s)) => s.clone(),
        Some(J::Arr(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(J::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(J::as_str).unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// THE SCRIPTED REPLY OF A CUSTOM MODE (`"replies": [{"when", "last",
/// "after", "text", "delay_ms", "pace_s"}, …]`, the per-turn script of
/// `forever`): the first rule with a `text` whose `when` stands in one of the
/// conversation's user texts `users` (in order, oldest first), whose `last`
/// stands in the newest of them and whose `after` stands in one before the
/// newest (a turn told apart from another of the same words by an act the
/// conversation met earlier), each left out or empty matching any — its
/// `text` the reply, its own `delay_ms` and `pace_s` (where it
/// carries them) the spec's for that reply ([`with_reply`]: a conversation
/// answered at once beside others paced). The stub keeps no state: a
/// conversation's turn is told by what it holds (the scenario's word in its
/// prompt, the harness's act it answers), so several tabs of one lab, each
/// its own conversation, are scripted at once. `None` where the spec has no
/// rules or none matches.
#[must_use]
pub fn scripted_reply<'a>(spec: &'a J, users: &[String]) -> Option<&'a J> {
    let rules = spec.get("replies").and_then(J::as_arr)?;
    fn word<'a>(r: &'a J, k: &str) -> Option<&'a str> {
        r.get(k).and_then(J::as_str).filter(|w| !w.is_empty())
    }
    let last = users.last().map_or("", String::as_str);
    let before = &users[..users.len().saturating_sub(1)];
    rules.iter().find(|r| {
        let when = word(r, "when").is_none_or(|w| users.iter().any(|u| u.contains(w)));
        let at_last = word(r, "last").is_none_or(|w| last.contains(w));
        let after = word(r, "after").is_none_or(|w| before.iter().any(|u| u.contains(w)));
        r.get("text").and_then(J::as_str).is_some() && when && at_last && after
    })
}

/// Whether a custom spec scripts its replies ([`scripted_reply`]): such a
/// spec answers a conversation's turns alone, and a side request (Claude
/// Code's title, a structured check) is answered as `ok`.
#[must_use]
pub fn is_scripted(spec: &J) -> bool {
    spec.get("replies").is_some()
}

/// `spec` with its `text` the reply `users` pick ([`scripted_reply`]) — its
/// `delay_ms` and `pace_s` the rule's own, where the rule carries them —
/// else as it is.
#[must_use]
pub fn with_reply(spec: J, users: &[String]) -> J {
    let Some(rule) = scripted_reply(&spec, users).cloned() else {
        return spec;
    };
    let mut spec = spec;
    for key in ["text", "delay_ms", "pace_s", "tool"] {
        if let Some(v) = rule.get(key) {
            spec.set(key, v.clone());
        }
    }
    spec
}

/// A SCRIPTED RULE'S TOOL CALL (`"tool": {"name", "input"}` on a rule of
/// [`scripted_reply`], kept on the spec by [`with_reply`]): the call a turn's
/// first request is answered with, before the request that carries the
/// call's result back (`answered`, each API's reader: [`messages_answered`],
/// [`gemini::results_answered`], [`muse::results_answered`], each true once
/// any request of the conversation carries it) is answered with the rule's
/// `text` — an empty one, and the turn ends on the tool's row. `None` where
/// the spec holds no call, or its result is in: the call is made once a
/// conversation.
#[must_use]
pub fn scripted_call(spec: &J, answered: bool) -> Option<(String, J)> {
    let call = spec.get("tool").filter(|c| matches!(c, J::Obj(_)))?;
    let name = call_name(call);
    (!answered && !name.is_empty()).then(|| (name, call_input(call)))
}

/// Whether a Messages API request's conversation carries a tool's result
/// back: a user message of it holds a `tool_result` block (Claude Code's
/// answer to a call) — the newest, or one before a later message (the next
/// prompt, a retry's user-role error turn after an empty reply), so a
/// scripted call is made once a conversation ([`scripted_call`]), never again
/// at each retry (agy's 2501 calls on Gemini's API before its reader read the
/// whole conversation).
#[must_use]
pub fn messages_answered(body: &[u8]) -> bool {
    let Ok(j) = J::parse_bytes(body) else {
        return false;
    };
    j.get("messages")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .any(is_tool_result)
}

/// The user texts of a Messages API request, oldest first: each user
/// message's text blocks ([`text_of`]); one that holds none (a tool result's)
/// is left out.
#[must_use]
pub fn message_users(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    j.get("messages")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|m| m.get("role").and_then(J::as_str) == Some("user"))
        .map(|m| text_of(m.get("content")))
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// The facts of one `/v1/messages` request every answer logs: its request
/// line, its body's length, the mode it is answered in, and what it asked.
#[derive(Clone, Copy)]
struct Turn<'a> {
    method: &'a str,
    path: &'a str,
    blen: usize,
    mode: &'a str,
    info: &'a Info,
}

impl<'a> Turn<'a> {
    /// The same request, answered in `mode` (the mode a used-up custom mode or a lifted limit falls back to).
    fn in_mode<'b>(self, mode: &'b str) -> Turn<'b>
    where
        'a: 'b,
    {
        Turn { mode, ..self }
    }
}

/// What one `/v1/messages` request is, read off its body.
#[derive(Clone, Debug)]
pub struct Info {
    /// Its `model` (as sent).
    pub model: J,
    /// Whether it asks for SSE.
    pub stream: bool,
    /// Its `max_tokens`.
    pub max_tokens: Option<J>,
    /// How many messages it holds.
    pub msgs: usize,
    /// How many tools it offers.
    pub tools: usize,
    /// The system prompt's length in characters.
    pub sys_chars: usize,
    /// The last user message's text.
    pub last_user: String,
    /// `main`, `aux` (no tools), `compact` (a summary request), `quota` (`max_tokens <= 1`) or `opaque`.
    pub kind: &'static str,
    /// A compaction request.
    pub compact: bool,
}

fn is_tool_result(m: &J) -> bool {
    m.get("role").and_then(J::as_str) == Some("user")
        && m.get("content").and_then(J::as_arr).is_some_and(|c| {
            c.iter()
                .any(|b| b.get("type").and_then(J::as_str) == Some("tool_result"))
        })
}

/// [`Info`] of a request body (best effort, never fails).
#[must_use]
pub fn analyse(body: &[u8]) -> Info {
    let mut info = Info {
        model: J::s("?"),
        stream: false,
        max_tokens: None,
        msgs: 0,
        tools: 0,
        sys_chars: 0,
        last_user: String::new(),
        kind: "main",
        compact: false,
    };
    let Ok(j @ J::Obj(_)) = J::parse_bytes(body) else {
        info.kind = "opaque";
        return info;
    };
    info.model = j.get("model").cloned().unwrap_or(J::s("?"));
    info.stream = j.get("stream").is_some_and(J::truthy);
    info.max_tokens = j
        .get("max_tokens")
        .filter(|v| !matches!(v, J::Null))
        .cloned();
    let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
    info.msgs = msgs.len();
    info.tools = match j.get("tools") {
        Some(J::Arr(a)) => a.len(),
        Some(J::Obj(m)) => m.len(),
        _ => 0,
    };
    let sys_text = text_of(j.get("system"));
    info.sys_chars = sys_text.chars().count();
    info.last_user = msgs
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(J::as_str) == Some("user"))
        .map(|m| text_of(m.get("content")))
        .unwrap_or_default();
    info.compact = sys_text.contains("tasked with summarizing conversations")
        || info
            .last_user
            .contains("Your task is to create a detailed summary");
    info.kind = if info.compact {
        "compact"
    } else if info
        .max_tokens
        .as_ref()
        .and_then(J::as_i64)
        .is_some_and(|n| n <= 1)
    {
        "quota"
    } else if info.tools == 0 {
        "aux"
    } else {
        "main"
    };
    info
}

/// What the `tool` mode answers a request: the next scripted call or text,
/// done once every step is answered, or `None` when no `toolstart` prompt was
/// sent and no `when` step is this conversation's. The step is read off the
/// request (each tool result, and each assistant reply with no tool use, is
/// one step answered), so the stub keeps no state and a second prompt starts
/// over.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolTurn {
    /// Call this tool with this input (its `@SID:<k>@` filled); the step it is.
    Call(String, J, usize),
    /// Reply with this text and end the turn; the step it is.
    Say(String, usize),
    /// Call these tools, in this order, in ONE assistant message (a step
    /// `{"calls": [{"name", "input"}, …]}`: parallel tool calls, each input's
    /// `@SID:<k>@` filled); the step it is.
    Calls(Vec<(String, J)>, usize),
    /// Every step is answered.
    Done,
}

/// Whether `m` is an assistant reply with no tool use: a scripted text step answered.
fn is_said(m: &J) -> bool {
    m.get("role").and_then(J::as_str) == Some("assistant")
        && match m.get("content") {
            Some(J::Str(_)) => true,
            Some(J::Arr(blocks)) => !blocks
                .iter()
                .any(|b| b.get("type").and_then(J::as_str) == Some("tool_use")),
            _ => false,
        }
}

/// The text of every tool result in `msgs`, in order: a result's string, or
/// its text blocks joined by newlines.
fn tool_results(msgs: &[J]) -> Vec<String> {
    msgs.iter()
        .filter(|m| m.get("role").and_then(J::as_str) == Some("user"))
        .filter_map(|m| m.get("content").and_then(J::as_arr))
        .flatten()
        .filter(|b| b.get("type").and_then(J::as_str) == Some("tool_result"))
        .map(|b| match b.get("content") {
            Some(J::Str(s)) => s.clone(),
            other => text_of(other),
        })
        .collect()
}

/// The first session id (`s-` and hex digits) in `text`.
#[must_use]
pub fn first_sid(text: &str) -> Option<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .find(|w| {
            w.strip_prefix("s-")
                .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
        })
}

/// `input` with every `@SID:<k>@` in its strings replaced by the first session
/// id of `results[k-1]`; one with no such result, or none in it, is left as
/// written, so the call that names it fails where it can be seen.
#[must_use]
pub fn fill_sids(input: &J, results: &[String]) -> J {
    match input {
        J::Str(s) => {
            let mut out = s.clone();
            for (k, r) in results.iter().enumerate() {
                if let Some(sid) = first_sid(r) {
                    out = out.replace(&format!("@SID:{}@", k + 1), sid);
                }
            }
            J::Str(out)
        }
        J::Arr(a) => J::Arr(a.iter().map(|v| fill_sids(v, results)).collect()),
        J::Obj(m) => J::Obj(
            m.iter()
                .map(|(k, v)| (k.clone(), fill_sids(v, results)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The calls of a `tool` script (one call or a list): each an object with a name.
fn script_calls(script: &J) -> Vec<&J> {
    match script {
        J::Arr(a) => a.iter().collect(),
        one => vec![one],
    }
    .into_iter()
    .filter(|c| matches!(c, J::Obj(_)) && c.get("name").is_some_and(J::truthy))
    .collect()
}

/// A scripted call's tool name.
fn call_name(c: &J) -> String {
    match c.get("name") {
        Some(J::Str(s)) => s.clone(),
        Some(other) => other.py(),
        None => String::new(),
    }
}

/// A scripted call's input (`{}` when it has none).
fn call_input(c: &J) -> J {
    c.get("input")
        .filter(|v| v.truthy())
        .cloned()
        .unwrap_or(J::Obj(Vec::new()))
}

/// [`ToolTurn`] of a request body against a Claude Code script (one step or a
/// list): a step is a call (an object with a name), several calls made in one
/// message (an object with `calls`, a list of named calls; Claude Code answers
/// them all in one request, so they are one step) or a reply that ends the
/// turn (an object with a `text`).
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
    let steps: Vec<&J> = match script {
        J::Arr(a) => a.iter().collect(),
        one => vec![one],
    }
    .into_iter()
    .filter(|c| {
        matches!(c, J::Obj(_))
            && (c.get("name").is_some_and(J::truthy)
                || c.get("text").and_then(J::as_str).is_some()
                || c.get("calls")
                    .is_some_and(|calls| !script_calls(calls).is_empty()))
    })
    .collect();
    let results = tool_results(msgs);
    let answer = |c: &J, step: usize| {
        if c.get("name").is_some_and(J::truthy) {
            ToolTurn::Call(call_name(c), fill_sids(&call_input(c), &results), step)
        } else if let Some(calls) = c.get("calls").filter(|_| c.get("text").is_none()) {
            ToolTurn::Calls(
                script_calls(calls)
                    .into_iter()
                    .map(|k| (call_name(k), fill_sids(&call_input(k), &results)))
                    .collect(),
                step,
            )
        } else {
            ToolTurn::Say(
                c.get("text").and_then(J::as_str).unwrap_or("").to_string(),
                step,
            )
        }
    };
    // A conversation of its own (a subagent's, a worker's) opens with its prompt: its
    // steps are the ones whose `when` that prompt carries, counted from its first request.
    let first = msgs
        .iter()
        .find(|m| m.get("role").and_then(J::as_str) == Some("user"))
        .map(|m| text_of(m.get("content")))
        .unwrap_or_default();
    let when_of = |c: &J| {
        c.get("when")
            .filter(|w| w.truthy())
            .map(|w| w.as_str().map_or_else(|| w.py(), str::to_string))
    };
    let mine: Vec<&&J> = steps
        .iter()
        .filter(|c| when_of(c).is_some_and(|w| first.contains(&w)))
        .collect();
    if !mine.is_empty() {
        let done = msgs
            .iter()
            .filter(|m| is_tool_result(m) || is_said(m))
            .count();
        return Some(match mine.get(done) {
            Some(c) => answer(c, done),
            None => ToolTurn::Done,
        });
    }
    let main: Vec<&&J> = steps.iter().filter(|c| when_of(c).is_none()).collect();
    let mut step: Option<usize> = None;
    for m in msgs {
        if is_said(m) {
            step = step.map(|s| s + 1);
            continue;
        }
        if m.get("role").and_then(J::as_str) != Some("user") {
            continue;
        }
        if is_tool_result(m) {
            step = step.map(|s| s + 1);
        }
        // also when it rides in the message of an interrupted call's result
        if text_of(m.get("content")).contains("toolstart") {
            step = Some(0);
        }
    }
    let step = step?;
    Some(match main.get(step) {
        Some(c) => answer(c, step),
        None => ToolTurn::Done,
    })
}

/// `limit:CLAIM:RESET[:PLAN]` → `(claim, reset unix seconds, plan)`; PLAN is
/// Codex's `plan_type` (plus, pro, free, team, …), and Claude Code's answer has none.
#[must_use]
pub fn limit_params(mode: &str) -> Option<(String, i64, Option<String>)> {
    let parts: Vec<&str> = mode.split(':').collect();
    let (claim, reset, plan) = match parts.as_slice() {
        [_, claim, reset] => (*claim, *reset, None),
        [_, claim, reset, plan] => (*claim, *reset, Some(*plan)),
        _ => return None,
    };
    let claim_ok = !claim.is_empty()
        && claim
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !claim_ok || reset.is_empty() || !reset.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let plan = plan.filter(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    });
    Some((
        claim.to_string(),
        reset.parse().ok()?,
        plan.map(str::to_string),
    ))
}

/// The fixed fake key the lab gives Codex's lab provider (never a credential anywhere).
pub const LAB_CODEX_KEY: &str = "lab-not-a-key";

/// The payload of a token the lab composed (a JWT with a `"lab"` label), from
/// an `Authorization` value: the lab's own fake login, never a credential.
#[must_use]
pub fn lab_claims(authorization: &str) -> Option<J> {
    let tok = authorization
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("bearer "))
        .map_or("", |_| authorization[7..].trim());
    let parts: Vec<&str> = tok.split('.').collect();
    let [_, payload, _] = parts.as_slice() else {
        return None;
    };
    let j = J::parse_bytes(&crate::util::b64url_decode(payload)?).ok()?;
    j.get("lab").and_then(J::as_str)?;
    Some(j)
}

/// Which lab credential a request carried, NEVER its value: `key`
/// ([`LAB_CODEX_KEY`]), `gemini-key` ([`gemini::LAB_GEMINI_KEY`]),
/// `jwt:<label>` (a token the lab composed), `oat` and
/// `api-key` (Claude Code's fake claude.ai token and fake API key, both
/// `…-LABLABLAB…`), `none`, or `other(<n> chars)`.
#[must_use]
pub fn auth_label(authorization: Option<&str>) -> String {
    let v = authorization.unwrap_or("");
    if v.is_empty() {
        return "none".into();
    }
    let tok = if v
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
    {
        v[7..].trim()
    } else {
        v.trim()
    };
    if tok == LAB_CODEX_KEY {
        return "key".into();
    }
    if tok == gemini::LAB_GEMINI_KEY {
        return "gemini-key".into();
    }
    if tok.starts_with("sk-ant-oat01-LABLABLAB") {
        return "oat".into();
    }
    if tok.starts_with("sk-ant-api03-LABLABLAB") {
        return "api-key".into();
    }
    if let Some(c) = lab_claims(v) {
        let label: String = c
            .get("lab")
            .and_then(J::as_str)
            .unwrap_or("")
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
            .take(24)
            .collect();
        return format!("jwt:{label}");
    }
    format!("other({} chars)", tok.chars().count())
}

/// Which client's Responses API a request speaks: the path it came on says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// Codex: `POST /v1/responses`, the events codex-api reads.
    Codex,
    /// Muse Code: `POST /responses`, its own numbered stream and its `muse` tools ([`muse`]).
    Muse,
}

/// What one `/v1/responses` request is, read off its body.
#[derive(Clone, Debug)]
pub struct RInfo {
    /// Its `model` (as sent).
    pub model: J,
    /// Whether it asks for SSE.
    pub stream: bool,
    /// How many input items it holds.
    pub items: usize,
    /// How many tools it offers.
    pub tools: usize,
    /// The instructions' length in characters.
    pub instr_chars: usize,
    /// The last user item's text.
    pub last_user: String,
    /// `codex`, `codex-json` (a structured side request: the thread title, …) or `opaque`.
    pub kind: &'static str,
    /// The side request's JSON schema.
    pub schema: Option<J>,
}

/// The text of a Responses input item: its `content` string, or its
/// `input_text`/`text` parts joined by newlines.
#[must_use]
pub fn item_text(it: &J) -> String {
    match it.get("content") {
        Some(J::Str(t)) => t.clone(),
        Some(J::Arr(blocks)) => blocks
            .iter()
            .filter(|b| {
                matches!(
                    b.get("type").and_then(J::as_str),
                    Some("input_text" | "text")
                )
            })
            .map(|b| b.get("text").and_then(J::as_str).unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// [`RInfo`] of a `/v1/responses` body (best effort, never fails).
#[must_use]
pub fn analyse_responses(body: &[u8]) -> RInfo {
    let mut info = RInfo {
        model: J::s("?"),
        stream: false,
        items: 0,
        tools: 0,
        instr_chars: 0,
        last_user: String::new(),
        kind: "codex",
        schema: None,
    };
    let Ok(j @ J::Obj(_)) = J::parse_bytes(body) else {
        info.kind = "opaque";
        return info;
    };
    info.model = j.get("model").cloned().unwrap_or(J::s("?"));
    info.stream = j.get("stream").is_some_and(J::truthy);
    let items = j.get("input").and_then(J::as_arr).unwrap_or_default();
    info.items = items.len();
    info.tools = j.get("tools").and_then(J::as_arr).map_or(0, <[J]>::len);
    info.instr_chars = match j.get("instructions") {
        Some(J::Str(t)) => t.chars().count(),
        Some(J::Null) | None => 0,
        Some(other) => other.py().chars().count(),
    };
    // Muse Code puts the runtime's `<system-reminder …>` notes in user items of their own: not what a person typed
    if let Some(t) = items
        .iter()
        .rev()
        .filter(|it| it.get("role").and_then(J::as_str) == Some("user"))
        .map(item_text)
        .find(|t| !t.trim_start().starts_with("<system-reminder"))
    {
        info.last_user = t;
    }
    let fmt = j.get("text").and_then(|t| t.get("format"));
    if fmt.and_then(|f| f.get("type")).and_then(J::as_str) == Some("json_schema") {
        info.kind = "codex-json";
        info.schema = fmt.and_then(|f| f.get("schema")).cloned();
    }
    info
}

/// The user texts of a Responses API request (Codex's, Muse Code's), oldest
/// first: each user item's text ([`item_text`]), the runtime's
/// `<system-reminder …>` notes and empty items left out.
#[must_use]
pub fn responses_users(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    j.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|it| it.get("role").and_then(J::as_str) == Some("user"))
        .map(item_text)
        .filter(|t| !t.trim().is_empty() && !t.trim_start().starts_with("<system-reminder"))
        .collect()
}

/// A wall's answer: its status, body and headers.
pub type WallAnswer = (u16, J, Vec<(String, String)>);

/// The error a wall mode answers a Responses API request with, in the shape
/// Codex and Muse Code both read (`{"error": {"type", "code", "message"}}`):
/// `overloaded` a 503 `server_is_overloaded`, `rate-limit` a 429
/// `rate_limit_exceeded` with `retry-after`, `401` and `bad-key` an
/// `invalid_api_key`, `500` a `server_error`; `None` for any other mode. Its
/// status, body and headers.
#[must_use]
pub fn responses_error(mode: &str, retry_after: &str) -> Option<WallAnswer> {
    let err = |pairs: Vec<(&str, J)>| {
        let e = J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        J::obj([("error", e)])
    };
    Some(match mode {
        "overloaded" => (
            503,
            err(vec![
                ("code", J::s("server_is_overloaded")),
                ("message", J::s("lab stub: overloaded")),
            ]),
            Vec::new(),
        ),
        "rate-limit" => (
            429,
            err(vec![
                ("type", J::s("rate_limit_exceeded")),
                ("code", J::s("rate_limit_exceeded")),
                (
                    "message",
                    J::s(format!(
                        "Rate limit reached for requests per min. Please try again in {retry_after}s."
                    )),
                ),
            ]),
            vec![("retry-after".to_string(), retry_after.to_string())],
        ),
        "401" | "bad-key" => (
            401,
            err(vec![
                (
                    "message",
                    J::s(format!("Incorrect API key provided: {LAB_CODEX_KEY}.")),
                ),
                ("type", J::s("invalid_request_error")),
                ("code", J::s("invalid_api_key")),
                ("param", J::Null),
            ]),
            Vec::new(),
        ),
        "500" => (
            500,
            err(vec![
                ("type", J::s("server_error")),
                ("code", J::s("server_error")),
                ("message", J::s("lab stub: the server had an error")),
            ]),
            Vec::new(),
        ),
        _ => return None,
    })
}

/// A JSON text that fits a side request's schema: each string property `lab
/// stub` (cut to its `maxLength`), else `{}`.
#[must_use]
pub fn schema_answer(schema: Option<&J>) -> String {
    let mut out: Vec<(String, J)> = Vec::new();
    if let Some(J::Obj(props)) = schema.and_then(|s| s.get("properties")) {
        for (k, v) in props {
            if v.get("type").and_then(J::as_str) == Some("string") {
                let max = v
                    .get("maxLength")
                    .and_then(J::as_i64)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(8);
                out.push((
                    k.clone(),
                    J::s("lab stub".chars().take(max).collect::<String>()),
                ));
            }
        }
    }
    J::Obj(out).py()
}

/// Codex's usage-limit 429 body (`type`, `resets_at`, `limit_window_minutes`, `plan_type`).
#[must_use]
pub fn usage_limit_body(claim: &str, reset: i64, plan: Option<&str>) -> J {
    let mut err = J::obj([
        ("type", J::s("usage_limit_reached")),
        ("message", J::s("The usage limit has been reached")),
        ("resets_at", J::Int(reset)),
    ]);
    match claim {
        "five_hour" => err.set("limit_window_minutes", J::Int(300)),
        "seven_day" => err.set("limit_window_minutes", J::Int(10080)),
        _ => {}
    }
    if let Some(p) = plan {
        err.set("plan_type", J::s(p));
    }
    J::obj([("error", err)])
}

/// The claim's window at 100% in Codex's header family: `five_hour` the
/// primary slot (300 min), `seven_day` the secondary (10080 min); other claims add nothing.
#[must_use]
pub fn usage_limit_headers(claim: &str, reset: i64) -> Vec<(String, String)> {
    let (slot, minutes) = match claim {
        "five_hour" => ("primary", 300),
        "seven_day" => ("secondary", 10080),
        _ => return Vec::new(),
    };
    vec![
        (format!("x-codex-{slot}-used-percent"), "100".into()),
        (
            format!("x-codex-{slot}-window-minutes"),
            minutes.to_string(),
        ),
        (format!("x-codex-{slot}-reset-at"), reset.to_string()),
    ]
}

/// The keys of a `--headers-file` that are [`rate_headers`]'s blocks (`headers`
/// writes them); every other key is a flat entry ([`flat_headers`],
/// `ratelimits` writes them).
pub const RATE_BLOCKS: [&str; 2] = ["on200", "on429"];

/// The `--headers-file` a verb leaves: `old` (the file's text as it reads now;
/// anything but a JSON object reads as no entry) with the verb's own entries
/// replaced by `new`'s — the [`RATE_BLOCKS`] when `blocks` (`headers`), else
/// the flat entries (`ratelimits`) — and the other verb's kept. One file
/// serves both of the stub's readers, and each verb's figures stand while the
/// other's change.
#[must_use]
pub fn headers_file_with(old: &str, new: J, blocks: bool) -> J {
    let own = |k: &str| RATE_BLOCKS.contains(&k) == blocks;
    let mut out: Vec<(String, J)> = match J::parse(old) {
        Ok(J::Obj(m)) => m.into_iter().filter(|(k, _)| !own(k)).collect(),
        _ => Vec::new(),
    };
    if let J::Obj(m) = new {
        out.extend(m.into_iter().filter(|(k, _)| own(k)));
    }
    J::Obj(out)
}

/// The flat entries of a `--headers-file` (`ratelimits` writes them):
/// `{name: value}` added to every API answer, `"@NOW+N@"` in a value the unix
/// second N seconds from `now`. The `on200`/`on429` blocks are
/// [`rate_headers`]'s, never a header.
#[must_use]
pub fn flat_headers(spec: &J, now: i64) -> Vec<(String, String)> {
    let J::Obj(m) = spec else {
        return Vec::new();
    };
    let name_ok = |k: &str| {
        !k.is_empty() && k.len() <= 80 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in m {
        if matches!(v, J::Null | J::Obj(_) | J::Arr(_)) || !name_ok(k) {
            continue;
        }
        let mut text = value_text(v);
        while let Some(i) = text.find("@NOW+") {
            let rest = &text[i + 5..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if digits.is_empty() || !rest[digits.len()..].starts_with('@') {
                break;
            }
            let n: i64 = digits.parse().unwrap_or(0);
            text = format!("{}{}{}", &text[..i], now + n, &rest[digits.len() + 1..]);
        }
        let text: String = text
            .chars()
            .map(|c| {
                if (c as u32) < 0x20 || c == '\u{7f}' {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let k = k.to_ascii_lowercase();
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1 = text,
            None => out.push((k, text)),
        }
    }
    out
}

/// `extra` over `base`, names lowercased: a name already there takes the new value where it stands.
fn merged(base: &[(String, String)], extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = base.to_vec();
    for (k, v) in extra {
        let k = k.to_ascii_lowercase();
        match out.iter_mut().find(|(n, _)| *n == k) {
            Some(slot) => slot.1.clone_from(v),
            None => out.push((k, v.clone())),
        }
    }
    out
}

/// A custom mode's `headers` (`{name: value}`, a value that is no string as its
/// JSON text), on its answer of either API, streamed or not.
fn spec_headers(spec: &J) -> Vec<(String, String)> {
    match spec.get("headers") {
        Some(J::Obj(m)) => m
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.py(), str::to_string)))
            .collect(),
        _ => Vec::new(),
    }
}

/// `bg[:SLEEP[:SLOW]]` → (seconds the background task sleeps, ticks of the wake's answer).
#[must_use]
pub fn bg_params(mode: &str, slow_seconds: i64) -> (i64, i64) {
    let parts: Vec<&str> = mode.split(':').collect();
    let num = |i: usize, d: i64| {
        parts
            .get(i)
            .and_then(|p| p.trim().parse::<i64>().ok())
            .map_or(d, |n| n.max(1))
    };
    (num(1, 20), num(2, slow_seconds))
}

/// A mode name as `/__mode?m=` takes it: only `[A-Za-z0-9_.:-]`, at most 64,
/// `ok` when nothing is left, and a `custom:` prefix dropped.
#[must_use]
pub fn clean_mode(m: &str) -> String {
    let kept: String = m
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "_.:-".contains(*c))
        .take(64)
        .collect();
    let m = if kept.is_empty() {
        "ok".to_string()
    } else {
        kept
    };
    m.strip_prefix("custom:").map_or(m.clone(), str::to_string)
}

const LONG: &str = "This is a long canned reply from the lab stub, not a model. It exists so the transcript \
grows and the TUI has several paragraphs to lay out, scroll and wrap at different widths.\n\n\
Nothing here is advice. The words are fixed and repeated on every turn, which keeps the \
captures comparable from one run to the next and keeps the stub free of any dependency.\n\n\
The stub streams this text as small deltas, the way the real API does, so the turn is \
busy for a moment and then idle, and the idle edge is what the survey and the harness wait for.";

const NUMBERED: &str = "Here are the options, pick one:\n\
1. The first option from the lab stub.\n\
2. The second option from the lab stub.\n\
3. The third option from the lab stub.";

const COMPACT_SUMMARY: &str = "<analysis>lab stub: nothing to analyse.</analysis>\n<summary>\n1. Primary Request and Intent: lab stub summary.\n</summary>";

/// One HTTP request as the server read it.
#[derive(Debug)]
pub struct Request {
    /// `GET`, `POST`, …
    pub method: String,
    /// The path and query.
    pub target: String,
    /// `HTTP/1.1` or `HTTP/1.0`.
    pub version: String,
    /// The headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// The body (read for POST and PUT).
    pub body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the connection stays open after the reply.
    fn keep_alive(&self) -> bool {
        let conn = self.header("connection").unwrap_or("").to_ascii_lowercase();
        if conn.contains("close") {
            return false;
        }
        self.version == "HTTP/1.1" || conn.contains("keep-alive")
    }
}

/// Read one request; `None` at a clean end of the connection.
///
/// # Errors
/// A malformed request line or chunk, or the connection failing mid-request.
pub fn read_request(r: &mut impl io::BufRead) -> io::Result<Option<Request>> {
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut words = line.split_whitespace();
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad request line",
        ));
    };
    let version = words.next().unwrap_or("HTTP/1.0").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let mut req = Request {
        method: method.to_string(),
        target: target.to_string(),
        version,
        headers,
        body: Vec::new(),
    };
    if req.method == "POST" || req.method == "PUT" {
        let n: usize = req
            .header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if n > 0 {
            req.body.resize(n, 0);
            r.read_exact(&mut req.body)?;
        } else if req
            .header("transfer-encoding")
            .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
        {
            loop {
                let mut size = String::new();
                r.read_line(&mut size)?;
                let size = size.trim();
                if size.is_empty() {
                    break;
                }
                let n = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                if n == 0 {
                    let mut end = String::new();
                    r.read_line(&mut end)?;
                    break;
                }
                let mut chunk = vec![0u8; n];
                r.read_exact(&mut chunk)?;
                req.body.extend_from_slice(&chunk);
                let mut crlf = String::new();
                r.read_line(&mut crlf)?;
            }
        }
    }
    Ok(Some(req))
}

/// `?a=b&c=d` decoded, blank values dropped (Python's `parse_qs`).
#[must_use]
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let v = percent_decode(v);
            (!v.is_empty()).then(|| (percent_decode(k), v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| {
                    char::from(c)
                        .to_digit(16)
                        .and_then(|d| u8::try_from(d).ok())
                };
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi * 16 + lo);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The reason phrase Python's server writes for a status (empty for one it does not know).
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Request Entity Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// The reply side of one connection.
struct Out<'a> {
    w: &'a mut TcpStream,
    /// The `--headers-file`'s flat headers, on every answer ([`flat_headers`]).
    flat: Vec<(String, String)>,
    /// A `/v1/messages` answer's extra headers, set once the request is read as one.
    rate: Option<RateHeaders>,
    /// The stub's count of 200 answers that carried them.
    served_200: &'a AtomicU32,
}

impl Out<'_> {
    fn head(&mut self, status: u16, headers: &[(String, String)]) -> io::Result<()> {
        let mut s = format!(
            "HTTP/1.1 {status} {}\r\nServer: lab-stub/1\r\n",
            reason(status)
        );
        for (k, v) in headers {
            s.push_str(&format!("{k}: {v}\r\n"));
        }
        s.push_str("\r\n");
        self.w.write_all(s.as_bytes())
    }

    fn json(&mut self, status: u16, obj: &J, extra: &[(String, String)]) -> io::Result<u16> {
        let raw = obj.py();
        let mut h = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("content-length".to_string(), raw.len().to_string()),
            ("request-id".to_string(), format!("req_lab{}", rand_hex())),
            ("x-lab-stub".to_string(), "1".to_string()),
        ];
        h.extend(merged(&self.flat, extra));
        h.extend(self.rate_for(status));
        self.head(status, &h)?;
        self.w.write_all(raw.as_bytes())?;
        self.w.flush()?;
        Ok(status)
    }

    fn chunk(&mut self, b: &[u8]) -> io::Result<()> {
        let mut v = format!("{:x}\r\n", b.len()).into_bytes();
        v.extend_from_slice(b);
        v.extend_from_slice(b"\r\n");
        self.w.write_all(&v)?;
        self.w.flush()
    }

    fn sse(&mut self, event: &str, data: &J) -> io::Result<()> {
        self.chunk(format!("event: {event}\ndata: {}\n\n", data.dump(COMPACT)).as_bytes())
    }

    fn stream_head(&mut self, extra: &[(String, String)]) -> io::Result<()> {
        let h = [
            (
                "content-type",
                "text/event-stream; charset=utf-8".to_string(),
            ),
            ("cache-control", "no-cache".to_string()),
            ("transfer-encoding", "chunked".to_string()),
            ("request-id", format!("req_lab{}", rand_hex())),
            ("x-lab-stub", "1".to_string()),
        ]
        .map(|(k, v)| (k.to_string(), v));
        let mut h = h.to_vec();
        h.extend(merged(&self.flat, extra));
        h.extend(self.rate_for(200));
        self.head(200, &h)
    }

    /// The extra headers a `status` answer carries; a 200 that carries on200
    /// headers counts toward `@SEQ@`, and no other answer does: a ramp starts
    /// at its BASE on the first answer that carries it, however many were
    /// served before the file named it.
    fn rate_for(&self, status: u16) -> Vec<(String, String)> {
        let Some(rate) = &self.rate else {
            return Vec::new();
        };
        match status {
            200 => {
                if !rate.on200.is_empty() {
                    self.served_200.fetch_add(1, Ordering::Relaxed);
                }
                rate.on200.clone()
            }
            429 => rate.on429.clone(),
            _ => Vec::new(),
        }
    }
}

fn closed(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
}

impl Stub {
    /// A stub over `args` (nothing bound yet).
    #[must_use]
    pub fn new(args: Args) -> Arc<Stub> {
        let mode = args.mode.clone();
        Arc::new(Stub {
            args,
            started: now(),
            state: Mutex::new(State {
                mode,
                ..State::default()
            }),
            log_lock: Mutex::new(()),
            served_200: AtomicU32::new(0),
            dumped: AtomicU64::new(0),
        })
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append one stamped line to the log.
    pub fn log(&self, line: &str) {
        let out = format!("{} {line}\n", stamp(now()));
        let _g = self.log_lock.lock().unwrap_or_else(|p| p.into_inner());
        match &self.args.log {
            Some(p) => {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                {
                    let _ = f.write_all(out.as_bytes());
                }
            }
            None => {
                let mut o = io::stdout().lock();
                let _ = o.write_all(out.as_bytes());
                let _ = o.flush();
            }
        }
    }

    /// The current mode; a `--mode-file` that changed since the last look wins.
    pub fn mode(&self) -> String {
        if let Some(path) = &self.args.mode_file
            && let Ok(meta) = std::fs::metadata(path)
        {
            let mtime = meta.modified().ok();
            let changed = self.st().mode_file_mtime != mtime;
            if changed {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                self.st().mode_file_mtime = mtime;
                let m = text.trim();
                if !m.is_empty() {
                    self.set_mode(m, true);
                }
            }
        }
        self.st().mode.clone()
    }

    /// Switch the mode (from the control path, or from the file).
    pub fn set_mode(&self, m: &str, from_file: bool) {
        let m = clean_mode(m);
        {
            let mut st = self.st();
            st.mode.clone_from(&m);
            st.custom_budget.retain(|(k, _)| *k != m);
        }
        if let (Some(path), false) = (&self.args.mode_file, from_file)
            && std::fs::write(path, format!("{m}\n")).is_ok()
        {
            self.st().mode_file_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        }
        self.log(&format!(
            "MODE -> {m}{}",
            if from_file { " (from file)" } else { "" }
        ));
    }

    fn record(
        &self,
        method: &str,
        path: &str,
        status: u16,
        blen: usize,
        mode: &str,
        summary: &str,
    ) {
        {
            let mut st = self.st();
            st.n_requests += 1;
            bump(&mut st.by_status, &status.to_string());
        }
        self.log(&format!(
            "{method} {path} -> {status} body={blen}B mode={mode} {summary}"
        ));
    }

    /// The `--headers-file` as it reads now.
    fn headers_spec(&self) -> Option<J> {
        self.args
            .headers_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| J::parse(&t).ok())
    }

    /// This request's extra answer headers: `--headers-file` as it reads now.
    fn answer_headers(&self) -> RateHeaders {
        self.headers_spec()
            .map_or_else(RateHeaders::default, |spec| {
                rate_headers(&spec, self.served_200.load(Ordering::Relaxed))
            })
    }

    /// The custom spec this request gets, or `None` (then it is answered as
    /// `ok`): a spec with `auth` applies only to a request that carried that
    /// lab credential, and a spec with `times` only to its first N requests.
    fn take_custom(&self, mode: &str, label: Option<&str>) -> Option<J> {
        let spec = self.custom_spec(mode)?;
        if let Some(want) = spec.get("auth").filter(|a| a.truthy())
            && want.as_str() != label
        {
            return None;
        }
        let times = spec.get("times").and_then(J::as_i64);
        let mut st = self.st();
        let left = match st.custom_budget.iter().find(|(k, _)| k == mode) {
            Some((_, l)) => *l,
            None => {
                st.custom_budget.push((mode.to_string(), times));
                times
            }
        };
        let out = match left {
            Some(n) if n <= 0 => None,
            Some(n) => {
                if let Some(slot) = st.custom_budget.iter_mut().find(|(k, _)| k == mode) {
                    slot.1 = Some(n - 1);
                }
                Some(spec)
            }
            None => Some(spec),
        };
        drop(st);
        if out.is_none() {
            self.log(&format!("custom mode {mode} used up -> ok"));
        }
        out
    }

    fn custom_spec(&self, name: &str) -> Option<J> {
        if not_custom(name).is_some() {
            return None;
        }
        let dir = self.args.modes_dir.as_ref()?;
        let text = std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()?;
        J::parse(&text).ok().filter(|j| matches!(j, J::Obj(_)))
    }

    fn reply_text(&self, kind: &str, mode: &str, n: i64) -> String {
        if let Some(p) = &self.args.reply_file
            && let Ok(t) = std::fs::read_to_string(p)
            && !t.trim().is_empty()
        {
            return t.trim_end_matches('\n').to_string();
        }
        if kind == "quota" {
            return "a".into();
        }
        match mode {
            "long" => LONG.into(),
            "numbered" => NUMBERED.into(),
            _ => format!("Hi! I am the local lab stub, not a model. (reply {n})"),
        }
    }

    /// The request `body` of `kind` (the log's `kind=`) kept in the dump
    /// directory: a turn as `req-<index>.json`, a side call ([`is_side`]) as
    /// `side-<index>.json`, its index the next of [`Stub::dumped`] (one count
    /// over both, so the two read in the order they came); only the newest
    /// [`DUMPS_KEPT`] stay.
    fn dump(&self, body: &[u8], kind: &str) {
        let Some(dir) = &self.args.dump_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        let idx = self.dumped.fetch_add(1, Ordering::Relaxed) + 1;
        let prefix = if is_side(kind) { SIDE_DUMP } else { TURN_DUMP };
        let _ = std::fs::write(
            dir.join(format!("{prefix}{idx:05}.json")),
            &body[..body.len().min(4 << 20)],
        );
        if let Some(old) = idx.checked_sub(DUMPS_KEPT) {
            for prefix in [TURN_DUMP, SIDE_DUMP] {
                let _ = std::fs::remove_file(dir.join(format!("{prefix}{old:05}.json")));
            }
        }
    }

    /// Answer one request on `w`. `Err` only when the client went away.
    fn handle(&self, req: &Request, w: &mut TcpStream) -> io::Result<()> {
        let (path, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
        let q = parse_query(query);
        let flat = self
            .headers_spec()
            .map(|spec| flat_headers(&spec, crate::util::now_secs()))
            .unwrap_or_default();
        let mut out = Out {
            w,
            flat,
            rate: None,
            served_200: &self.served_200,
        };
        if path.starts_with("/__") {
            if !self.control(path, &q, &mut out)? {
                let st = out.json(
                    404,
                    &err_body("not_found_error", "unknown control path"),
                    &[],
                )?;
                self.record(&req.method, path, st, 0, &self.mode(), "control");
            }
            return Ok(());
        }
        let mode = self.mode();
        let blen = req.body.len();
        let auth = req.header("authorization");
        // a Gemini API request's credentials: the API key in x-goog-api-key (or a `key` query, which neither CLI
        // sends), and any Authorization header beside it
        let gemini_label = || {
            let key = req
                .header("x-goog-api-key")
                .or_else(|| q.iter().find(|(k, _)| k == "key").map(|(_, v)| v.as_str()));
            gemini::credential_label(key, auth)
        };
        let result = if req.method == "POST" && path == "/v1/messages" {
            self.messages(&req.method, path, &req.body, mode, &mut out)
        } else if req.method == "POST" && path == "/v1/responses" {
            let d = Dialect::Codex;
            self.responses(&req.method, path, &req.body, &mode, auth, d, &mut out)
        } else if req.method == "POST" && path == "/responses" {
            self.muse(&req.method, path, &req.body, &mode, req, &mut out)
        } else if req.method == "GET" && path == "/muse-code/models" {
            self.muse_models(&req.method, path, &mode, auth, &mut out)
        } else if req.method == "POST" && path == "/oauth/token" {
            self.oauth_token(&req.method, path, &req.body, auth, &mut out)
        } else if req.method == "GET" && path == "/backend-api/wham/accounts/check" {
            self.accounts_check(&req.method, path, &mode, req, &mut out)
        } else if req.method == "GET" && path == "/backend-api/wham/usage" {
            self.wham_usage(&req.method, path, &mode, auth, &mut out)
        } else if let Some(route) = account::Route::of(&req.method, path) {
            self.account(&req.method, path, &q, route, &mode, req, &mut out)
        } else if let Some((model, verb)) = gemini::route(&req.method, path) {
            let r = (model.as_str(), verb);
            let label = gemini_label();
            self.gemini(&req.method, path, r, &req.body, &mode, &label, &mut out)
        } else if req.method == "GET" && path.starts_with("/v1beta/models") {
            self.gemini_models(&req.method, path, &mode, &gemini_label(), &mut out)
        } else if req.method == "POST" && path == "/v1/messages/count_tokens" {
            let n = est_tokens(blen);
            out.json(200, &J::obj([("input_tokens", J::Int(n))]), &[])
                .map(|st| self.record(&req.method, path, st, blen, &mode, &format!("tokens={n}")))
        } else {
            let msg = format!("Not Found: {} {path}", req.method);
            out.json(404, &err_body("not_found_error", &msg), &[])
                .map(|st| {
                    let s = format!("unrouted auth={}", auth_label(auth));
                    self.record(&req.method, path, st, blen, &mode, &s);
                })
        };
        if let Err(e) = &result
            && closed(e)
        {
            self.log(&format!(
                "{} {path} client closed the connection",
                req.method
            ));
        }
        result
    }

    /// The control endpoints; `false` for a path that is none of them.
    fn control(&self, path: &str, q: &[(String, String)], out: &mut Out<'_>) -> io::Result<bool> {
        let arg = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        match path {
            "/__mode" => {
                if let Some(m) = arg("m") {
                    self.set_mode(&m, false);
                }
                out.json(200, &J::obj([("mode", J::s(self.mode()))]), &[])?;
            }
            "/__status" => {
                let mode = self.mode();
                let st = self.st();
                let counts = |v: &[(String, i64)]| {
                    J::Obj(v.iter().map(|(k, n)| (k.clone(), J::Int(*n))).collect())
                };
                let snap = J::obj([
                    ("mode", J::s(mode)),
                    (
                        "custom_budget",
                        J::Obj(
                            st.custom_budget
                                .iter()
                                .map(|(k, n)| (k.clone(), J::from(*n)))
                                .collect(),
                        ),
                    ),
                    ("n_requests", J::Int(st.n_requests)),
                    ("by_status", counts(&st.by_status)),
                    ("by_kind", counts(&st.by_kind)),
                    ("proxy_attempts", J::Int(st.proxy_attempts)),
                    (
                        "uptime_s",
                        J::Float(((now() - self.started) * 10.0).round() / 10.0),
                    ),
                ]);
                drop(st);
                out.json(200, &snap, &[])?;
            }
            "/__log" => {
                let n: usize = arg("n").and_then(|v| v.parse().ok()).unwrap_or(50);
                let text = self
                    .args
                    .log
                    .as_ref()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                let tail: Vec<J> = lines[lines.len().saturating_sub(n)..]
                    .iter()
                    .map(|l| J::s(*l))
                    .collect();
                out.json(200, &J::obj([("lines", J::Arr(tail))]), &[])?;
            }
            "/__note" => {
                // a marker in the request log (a scenario's step: "auth.json now holds B")
                self.log(&format!(
                    "NOTE {}",
                    one_line(&arg("t").unwrap_or_default(), 160)
                ));
                out.json(200, &J::obj([("ok", J::Bool(true))]), &[])?;
            }
            "/__reset" => {
                {
                    let mut st = self.st();
                    st.n_requests = 0;
                    st.by_status.clear();
                    st.by_kind.clear();
                    st.custom_budget.clear();
                }
                out.json(200, &J::obj([("ok", J::Bool(true))]), &[])?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn record_turn(&self, t: &Turn<'_>, status: u16, summary: &str) {
        self.record(t.method, t.path, status, t.blen, t.mode, summary);
    }

    #[allow(clippy::too_many_lines)]
    fn messages(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mode: String,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let info = analyse(body);
        let kind = info.kind;
        let n = bump(&mut self.st().by_kind, kind);
        let model = match &info.model {
            J::Str(s) => s.clone(),
            other => other.py(),
        };
        let max_tokens = info
            .max_tokens
            .as_ref()
            .map_or_else(|| "None".to_string(), |v| v.py());
        let mut summary = format!(
            "kind={kind} model={model} stream={} max_tokens={max_tokens} msgs={} tools={} sys={}c last_user=\"{}\"",
            info.stream,
            info.msgs,
            info.tools,
            info.sys_chars,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let rate = self.answer_headers();
        summary.push_str(&rate.summary());
        out.rate = Some(rate);
        let blen = body.len();
        let base = Turn {
            method,
            path,
            blen,
            mode: "",
            info: &info,
        };
        let head = mode.split(':').next().unwrap_or("").to_string();

        if head == "bg" && kind == "main" {
            return self.bg(&base.in_mode(&mode), body, &summary, out);
        }
        if mode == "tool" && kind == "main" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| J::parse(&t).ok());
            match script.and_then(|s| tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let s = format!("{summary} tool:{name} step {step}");
                    return self.stream_tool(&base.in_mode(&mode), &s, &name, &input, out);
                }
                Some(ToolTurn::Calls(calls, step)) => {
                    let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
                    let s = format!("{summary} tool:{} step {step}", names.join("+"));
                    return self.stream_tools(&base.in_mode(&mode), &s, &calls, out);
                }
                Some(ToolTurn::Say(text, step)) => {
                    let s = format!("{summary} tool:text step {step}");
                    let deltas = split_deltas(&text);
                    return self.stream(&base.in_mode(&mode), &s, &deltas, 0.0, &[], out);
                }
                Some(ToolTurn::Done) => {
                    let s = format!("{summary} tool:done");
                    let done = ["Tool call done.".to_string()];
                    return self.stream(&base.in_mode(&mode), &s, &done, 0.0, &[], out);
                }
                None => {}
            }
        }
        let mut mode = mode;
        if let Some(sp) = self
            .take_custom(&mode, None)
            .filter(|sp| kind == "main" || !is_scripted(sp))
        {
            let sp = with_reply(sp, &message_users(body));
            if let Some((name, input)) = scripted_call(&sp, messages_answered(body)) {
                let s = format!("{summary} custom tool:{name}");
                return self.stream_tool(&base.in_mode(&mode), &s, &name, &input, out);
            }
            return self.custom(&base.in_mode(&mode), &sp, &summary, out);
        }
        if self.custom_spec(&mode).is_some() {
            mode = "ok".into();
        }
        if mode.split(':').next() == Some("limit") {
            let Some((claim, reset, _plan)) = limit_params(&mode) else {
                let msg = format!("lab stub: mode {mode} is not limit:CLAIM:RESET");
                let st = out.json(400, &err_body("invalid_request_error", &msg), &[])?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            };
            let now_s = crate::util::now_secs();
            if now_s < reset {
                let headers = [
                    ("anthropic-ratelimit-unified-status", "rejected".to_string()),
                    ("anthropic-ratelimit-unified-reset", reset.to_string()),
                    (
                        "anthropic-ratelimit-unified-representative-claim",
                        claim.clone(),
                    ),
                ]
                .map(|(k, v)| (k.to_string(), v));
                let msg =
                    "This request would exceed your account's rate limit. Please try again later.";
                let st = out.json(429, &err_body("rate_limit_error", msg), &headers)?;
                let s = format!("{summary} limit:{claim} resets in {}s", reset - now_s);
                self.record(method, path, st, blen, &mode, &s);
                return Ok(());
            }
            summary.push_str(" limit lifted (reset passed)");
            mode = "ok".into();
        }
        match mode.as_str() {
            "overloaded" => {
                let st = out.json(529, &err_body("overloaded_error", "Overloaded"), &[])?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            }
            "rate-limit" => {
                let msg = "This request would exceed your organization's rate limit of 50 requests per minute.";
                let h = [("retry-after".to_string(), self.args.retry_after.clone())];
                let st = out.json(429, &err_body("rate_limit_error", msg), &h)?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            }
            "401" | "bad-key" => {
                let st = out.json(
                    401,
                    &err_body("authentication_error", "invalid x-api-key"),
                    &[],
                )?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            }
            "500" => {
                let st = out.json(500, &err_body("api_error", "Internal server error"), &[])?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            }
            _ => {}
        }
        if mode == "too-long" || (mode == "compact-fails" && info.compact) {
            let msg = "prompt is too long: 250000 tokens > 200000 maximum";
            let st = out.json(400, &err_body("invalid_request_error", msg), &[])?;
            let s = format!(
                "{summary}{}",
                if info.compact {
                    " (compaction request)"
                } else {
                    ""
                }
            );
            self.record(method, path, st, blen, &mode, &s);
            return Ok(());
        }
        // ok / long / numbered / slow / compact-fails (an ordinary turn)
        let text = self.reply_text(kind, &mode, n);
        let (mut deltas, pace) = if mode == "slow" && kind != "quota" && kind != "compact" {
            (
                (1..=self.args.slow_seconds.max(1))
                    .map(|i| format!("tick {i} "))
                    .collect(),
                1.0,
            )
        } else {
            (split_deltas(&text), 0.0)
        };
        if kind == "compact" {
            deltas = split_deltas(COMPACT_SUMMARY);
        }
        if info.stream {
            return self.stream(&base.in_mode(&mode), &summary, &deltas, pace, &[], out);
        }
        let all: String = deltas.concat();
        let out_tok = i64::try_from(all.len() / 4).unwrap_or(0).max(1);
        let msg = J::obj([
            ("id", J::s(format!("msg_lab{}", rand_hex()))),
            ("type", J::s("message")),
            ("role", J::s("assistant")),
            ("model", info.model.clone()),
            (
                "content",
                J::Arr(vec![J::obj([("type", J::s("text")), ("text", J::s(all))])]),
            ),
            ("stop_reason", J::s("end_turn")),
            ("stop_sequence", J::Null),
            (
                "usage",
                J::obj([
                    ("input_tokens", J::Int(est_tokens(blen))),
                    ("output_tokens", J::Int(out_tok)),
                ]),
            ),
        ]);
        let st = out.json(200, &msg, &[])?;
        self.record(method, path, st, blen, &mode, &summary);
        Ok(())
    }

    /// A Responses API turn: Codex's `/v1/responses`, or Muse Code's
    /// `/responses` main turn (`dialect` says which stream answers it, and
    /// only Muse Code's calls the `tool` mode's script).
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn responses(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mode: &str,
        auth: Option<&str>,
        dialect: Dialect,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let info = analyse_responses(body);
        let kind = match dialect {
            Dialect::Codex => info.kind,
            Dialect::Muse => muse::kind_of(&info),
        };
        let label = auth_label(auth);
        let n = bump(&mut self.st().by_kind, kind);
        let model = match &info.model {
            J::Str(s) => s.clone(),
            other => other.py(),
        };
        let mut summary = format!(
            "kind={kind} auth={label} model={model} stream={} items={} tools={} instructions={}c last_user=\"{}\"",
            info.stream,
            info.items,
            info.tools,
            info.instr_chars,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let blen = body.len();
        let in_tok = est_tokens(blen);
        let mut mode = mode.to_string();
        if dialect == Dialect::Muse && mode == "tool" && kind == "muse" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| J::parse(&t).ok());
            let facts = (method, path, blen, mode.as_str());
            match script.and_then(|s| muse::tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let s = format!("{summary} tool:{name} step {step}");
                    return self.muse_call(facts, &info, &s, &name, &input, out);
                }
                Some(ToolTurn::Done) => {
                    let result = muse::last_tool_output(body).unwrap_or_default();
                    let s = format!(
                        "{summary} tool:done result={}",
                        py_repr(&one_line(&result, 160))
                    );
                    let done = split_deltas(&format!("Tool call done. Result: {result}"));
                    return self.muse_text(facts, &info, &s, &done, 0.0, in_tok, &[], out);
                }
                // Its script holds named calls only (`script_calls`): a text
                // step, or calls made in one message, is a Claude Code
                // script's (`tool_turn`).
                Some(ToolTurn::Say(..) | ToolTurn::Calls(..)) | None => {}
            }
        }
        if let Some(spec) = self
            .take_custom(&mode, Some(&label))
            .filter(|sp| !matches!(kind, "codex-json" | "muse-json") || !is_scripted(sp))
        {
            let spec = with_reply(spec, &responses_users(body));
            if dialect == Dialect::Muse
                && let Some((name, input)) = scripted_call(&spec, muse::results_answered(body))
            {
                let s = format!("{summary} custom tool:{name}");
                return self.muse_call(
                    (method, path, blen, mode.as_str()),
                    &info,
                    &s,
                    &name,
                    &input,
                    out,
                );
            }
            if let Some(ms) = spec
                .get("delay_ms")
                .and_then(J::as_f64)
                .filter(|v| *v > 0.0)
            {
                std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
            }
            let status = spec
                .get("status")
                .and_then(J::as_i64)
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200);
            let stream = spec.get("stream").is_none_or(J::truthy);
            if status == 200 && stream {
                let text = spec
                    .get("text")
                    .and_then(J::as_str)
                    .map_or_else(|| format!("Hi! (custom mode {mode})"), str::to_string);
                let pace = spec.get("pace_s").and_then(J::as_f64).unwrap_or(0.0);
                let headers = spec_headers(&spec);
                let s = format!("{summary} custom");
                return self.responses_stream(
                    dialect,
                    (method, path, blen, &mode),
                    &info,
                    &s,
                    &split_deltas(&text),
                    pace,
                    in_tok,
                    &headers,
                    out,
                );
            }
            // the shared custom answer reads only the request facts every API logs
            let msg_info = analyse(b"");
            let t = Turn {
                method,
                path,
                blen,
                mode: &mode,
                info: &msg_info,
            };
            return self.custom(&t, &spec, &summary, out);
        }
        if self.custom_spec(&mode).is_some() {
            mode = "ok".into();
        }
        if mode.split(':').next() == Some("limit") {
            let Some((claim, reset, plan)) = limit_params(&mode) else {
                let msg = format!("lab stub: mode {mode} is not limit:CLAIM:RESET[:PLAN]");
                let body = J::obj([(
                    "error",
                    J::obj([
                        ("type", J::s("invalid_request_error")),
                        ("message", J::s(msg)),
                    ]),
                )]);
                let st = out.json(400, &body, &[])?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            };
            let now_s = crate::util::now_secs();
            if now_s < reset {
                let st = out.json(
                    429,
                    &usage_limit_body(&claim, reset, plan.as_deref()),
                    &usage_limit_headers(&claim, reset),
                )?;
                let s = format!(
                    "{summary} limit:{claim} resets in {}s plan={}",
                    reset - now_s,
                    plan.as_deref().unwrap_or("-")
                );
                self.record(method, path, st, blen, &mode, &s);
                return Ok(());
            }
            summary.push_str(" limit lifted (reset passed)");
            mode = "ok".into();
        }
        if let Some((status, body, h)) = responses_error(&mode, &self.args.retry_after) {
            let st = out.json(status, &body, &h)?;
            self.record(method, path, st, blen, &mode, &summary);
            return Ok(());
        }
        if mode == "too-long" {
            let (code, msg) = (
                "context_length_exceeded",
                "Your input exceeds the context window of this model.",
            );
            let facts = (method, path, blen, mode.as_str());
            return match dialect {
                Dialect::Codex => self.responses_failed(facts, &summary, code, msg, out),
                Dialect::Muse => self.muse_failed(facts, &summary, code, msg, out),
            };
        }
        let text = if matches!(kind, "codex-json" | "muse-json") {
            schema_answer(info.schema.as_ref())
        } else {
            self.reply_text("main", &mode, n)
        };
        let (deltas, pace) = if mode == "slow" && matches!(kind, "codex" | "muse") {
            (
                (1..=self.args.slow_seconds.max(1))
                    .map(|i| format!("tick {i} "))
                    .collect(),
                1.0,
            )
        } else {
            (split_deltas(&text), 0.0)
        };
        self.responses_stream(
            dialect,
            (method, path, blen, &mode),
            &info,
            &summary,
            &deltas,
            pace,
            in_tok,
            &[],
            out,
        )
    }

    /// A plain text reply as the Responses API's SSE: the event types
    /// codex-api reads, or Muse Code's own stream ([`Stub::muse_text`]).
    #[allow(clippy::too_many_arguments)]
    fn responses_stream(
        &self,
        dialect: Dialect,
        (method, path, blen, mode): (&str, &str, usize, &str),
        info: &RInfo,
        summary: &str,
        deltas: &[String],
        pace: f64,
        in_tok: i64,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        if dialect == Dialect::Muse {
            let facts = (method, path, blen, mode);
            return self.muse_text(facts, info, summary, deltas, pace, in_tok, extra, out);
        }
        out.stream_head(extra)?;
        let rid = format!("resp_lab{}", rand_hex());
        let mid = format!("msg_lab{}", rand_hex());
        let text: String = deltas.concat();
        let out_tok = i64::try_from(text.chars().count() / 4).unwrap_or(0).max(1);
        let ev = |pairs: Vec<(&str, J)>| {
            J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
        };
        let sent = (|| -> io::Result<()> {
            let send = |out: &mut Out<'_>, j: J| {
                let t = j.get("type").and_then(J::as_str).unwrap_or("").to_string();
                out.sse(&t, &j)
            };
            send(
                out,
                ev(vec![
                    ("type", J::s("response.created")),
                    (
                        "response",
                        ev(vec![
                            ("id", J::s(rid.as_str())),
                            ("object", J::s("response")),
                            ("status", J::s("in_progress")),
                            ("model", info.model.clone()),
                        ]),
                    ),
                ]),
            )?;
            send(
                out,
                ev(vec![
                    ("type", J::s("response.output_item.added")),
                    ("output_index", J::Int(0)),
                    (
                        "item",
                        ev(vec![
                            ("id", J::s(mid.as_str())),
                            ("type", J::s("message")),
                            ("role", J::s("assistant")),
                            ("status", J::s("in_progress")),
                            ("content", J::Arr(Vec::new())),
                        ]),
                    ),
                ]),
            )?;
            send(
                out,
                ev(vec![
                    ("type", J::s("response.content_part.added")),
                    ("item_id", J::s(mid.as_str())),
                    ("output_index", J::Int(0)),
                    ("content_index", J::Int(0)),
                    (
                        "part",
                        ev(vec![
                            ("type", J::s("output_text")),
                            ("text", J::s("")),
                            ("annotations", J::Arr(Vec::new())),
                        ]),
                    ),
                ]),
            )?;
            for d in deltas {
                if pace > 0.0 {
                    std::thread::sleep(Duration::from_secs_f64(pace));
                }
                send(
                    out,
                    ev(vec![
                        ("type", J::s("response.output_text.delta")),
                        ("item_id", J::s(mid.as_str())),
                        ("output_index", J::Int(0)),
                        ("content_index", J::Int(0)),
                        ("delta", J::s(d.as_str())),
                    ]),
                )?;
            }
            send(
                out,
                ev(vec![
                    ("type", J::s("response.output_text.done")),
                    ("item_id", J::s(mid.as_str())),
                    ("output_index", J::Int(0)),
                    ("content_index", J::Int(0)),
                    ("text", J::s(text.as_str())),
                ]),
            )?;
            send(
                out,
                ev(vec![
                    ("type", J::s("response.output_item.done")),
                    ("output_index", J::Int(0)),
                    (
                        "item",
                        ev(vec![
                            ("id", J::s(mid.as_str())),
                            ("type", J::s("message")),
                            ("role", J::s("assistant")),
                            ("status", J::s("completed")),
                            (
                                "content",
                                J::Arr(vec![ev(vec![
                                    ("type", J::s("output_text")),
                                    ("text", J::s(text.as_str())),
                                    ("annotations", J::Arr(Vec::new())),
                                ])]),
                            ),
                        ]),
                    ),
                ]),
            )?;
            send(
                out,
                ev(vec![
                    ("type", J::s("response.completed")),
                    (
                        "response",
                        ev(vec![
                            ("id", J::s(rid.as_str())),
                            ("object", J::s("response")),
                            ("status", J::s("completed")),
                            ("model", info.model.clone()),
                            (
                                "usage",
                                ev(vec![
                                    ("input_tokens", J::Int(in_tok)),
                                    (
                                        "input_tokens_details",
                                        ev(vec![("cached_tokens", J::Int(0))]),
                                    ),
                                    ("output_tokens", J::Int(out_tok)),
                                    (
                                        "output_tokens_details",
                                        ev(vec![("reasoning_tokens", J::Int(0))]),
                                    ),
                                    ("total_tokens", J::Int(in_tok + out_tok)),
                                ]),
                            ),
                        ]),
                    ),
                ]),
            )?;
            out.chunk(b"")
        })();
        let note = match sent {
            Err(e) if closed(&e) => " (client closed mid-stream)",
            Err(e) => return Err(e),
            Ok(()) => "",
        };
        self.record(
            method,
            path,
            200,
            blen,
            mode,
            &format!("{summary} sse={} deltas{note}", deltas.len()),
        );
        Ok(())
    }

    /// A `response.failed` event with `code` (the turn fails as a stream, as the real API fails it).
    fn responses_failed(
        &self,
        (method, path, blen, mode): (&str, &str, usize, &str),
        summary: &str,
        code: &str,
        message: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(&[])?;
        let rid = format!("resp_lab{}", rand_hex());
        let sent = (|| -> io::Result<()> {
            out.sse(
                "response.created",
                &J::obj([
                    ("type", J::s("response.created")),
                    (
                        "response",
                        J::obj([("id", J::s(rid.as_str())), ("status", J::s("in_progress"))]),
                    ),
                ]),
            )?;
            out.sse(
                "response.failed",
                &J::obj([
                    ("type", J::s("response.failed")),
                    (
                        "response",
                        J::obj([
                            ("id", J::s(rid.as_str())),
                            ("status", J::s("failed")),
                            (
                                "error",
                                J::obj([("code", J::s(code)), ("message", J::s(message))]),
                            ),
                        ]),
                    ),
                ]),
            )?;
            out.chunk(b"")
        })();
        if let Err(e) = sent
            && !closed(&e)
        {
            return Err(e);
        }
        self.record(
            method,
            path,
            200,
            blen,
            mode,
            &format!("{summary} response.failed {code}"),
        );
        Ok(())
    }

    /// Codex's workspace discovery under a ChatGPT login (`chatgpt_base_url` is
    /// the stub under `codex --chatgpt`): the TUI will not start without an
    /// answer. One personal account, the lab token's own.
    fn accounts_check(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        req: &Request,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let auth = req.header("authorization");
        let claims = auth
            .and_then(lab_claims)
            .and_then(|c| c.get("https://api.openai.com/auth").cloned())
            .unwrap_or(J::Obj(Vec::new()));
        let acct = req
            .header("chatgpt-account-id")
            .map(str::to_string)
            .or_else(|| {
                claims
                    .get("chatgpt_account_id")
                    .and_then(J::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "acct-lab-1".into());
        let acct: String = acct
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(40)
            .collect();
        // Codex wants an HTTPS origin here ("workspace backend must use an HTTPS origin without credentials"), so
        // the workspace's own backend is a name at example.com: its calls go to the black-hole proxy, never anywhere
        let entry = J::obj([
            ("id", J::s(acct.as_str())),
            (
                "plan_type",
                claims.get("chatgpt_plan_type").cloned().unwrap_or(J::Null),
            ),
            (
                "workspace_backend_origin",
                J::s("https://workspace.example.com"),
            ),
            ("account_routing_override", J::s("NO_CONSTRAINT")),
            ("name", J::Null),
            ("structure", J::s("personal")),
        ]);
        let body = J::obj([
            ("accounts", J::Arr(vec![entry])),
            ("default_account_id", J::s(acct.as_str())),
        ]);
        let st = out.json(200, &body, &[])?;
        let s = format!("kind=accounts auth={}", auth_label(auth));
        self.record(method, path, st, 0, mode, &s);
        Ok(())
    }

    /// The account usage a ChatGPT login's TUI polls (its `/status` card's
    /// limits and credits): the windows and credits the `--headers-file` sends
    /// on answers, in the backend's payload, and the plan the lab token names.
    fn wham_usage(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        auth: Option<&str>,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let now = crate::util::now_secs();
        let h = out.flat.clone();
        let get = |k: &str| h.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let claims = auth
            .and_then(lab_claims)
            .and_then(|c| c.get("https://api.openai.com/auth").cloned())
            .unwrap_or(J::Obj(Vec::new()));
        let window = |slot: &str| -> Option<(i64, J)> {
            let used = get(&format!("x-codex-{slot}-used-percent"))?;
            // whole percent, as the backend reports it
            #[allow(clippy::cast_possible_truncation)]
            let pct = used.trim().parse::<f64>().unwrap_or(0.0) as i64;
            let mins: i64 = get(&format!("x-codex-{slot}-window-minutes"))
                .and_then(|m| m.trim().parse().ok())
                .unwrap_or(0);
            let reset: i64 = get(&format!("x-codex-{slot}-reset-at"))
                .and_then(|r| r.trim().parse().ok())
                .unwrap_or(now);
            Some((
                pct,
                J::obj([
                    ("used_percent", J::Int(pct)),
                    ("limit_window_seconds", J::Int(mins * 60)),
                    ("reset_after_seconds", J::Int((reset - now).max(0))),
                    ("reset_at", J::Int(reset)),
                ]),
            ))
        };
        let (prim, sec) = (window("primary"), window("secondary"));
        let reached = [&prim, &sec]
            .iter()
            .any(|w| w.as_ref().is_some_and(|(p, _)| *p >= 100));
        let credits = get("x-codex-credits-has-credits").map(|has| {
            J::obj([
                ("has_credits", J::Bool(has == "true")),
                (
                    "unlimited",
                    J::Bool(get("x-codex-credits-unlimited").as_deref() == Some("true")),
                ),
                (
                    "balance",
                    get("x-codex-credits-balance").map_or(J::Null, J::s),
                ),
            ])
        });
        let reached_type = get("x-codex-rate-limit-reached-type")
            .filter(|t| !t.is_empty())
            .map_or(J::Null, |t| J::obj([("type", J::s(t))]));
        let has_credits = credits.is_some();
        let body = J::obj([
            (
                "plan_type",
                claims
                    .get("chatgpt_plan_type")
                    .filter(|p| p.truthy())
                    .cloned()
                    .unwrap_or_else(|| J::s("plus")),
            ),
            (
                "rate_limit",
                J::obj([
                    ("allowed", J::Bool(!reached)),
                    ("limit_reached", J::Bool(reached)),
                    (
                        "primary_window",
                        prim.as_ref().map_or(J::Null, |(_, w)| w.clone()),
                    ),
                    (
                        "secondary_window",
                        sec.as_ref().map_or(J::Null, |(_, w)| w.clone()),
                    ),
                ]),
            ),
            ("credits", credits.unwrap_or(J::Null)),
            ("rate_limit_reached_type", reached_type),
        ]);
        let st = out.json(200, &body, &[])?;
        let pct = |w: &Option<(i64, J)>| {
            w.as_ref()
                .map_or_else(|| "None".into(), |(p, _)| p.to_string())
        };
        let s = format!(
            "kind=usage auth={} primary={} secondary={} credits={}",
            auth_label(auth),
            pct(&prim),
            pct(&sec),
            if has_credits { "True" } else { "False" }
        );
        self.record(method, path, st, 0, mode, &s);
        Ok(())
    }

    /// A claude.ai account route: the `--account-file`'s body for it, its
    /// instants expanded ([`account::expand`]), or a 404 when the file has none.
    /// The log names the route, the usage read and the credential's lab label.
    #[allow(clippy::too_many_arguments)]
    fn account(
        &self,
        method: &str,
        path: &str,
        q: &[(String, String)],
        route: account::Route,
        mode: &str,
        req: &Request,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let auth = req.header("authorization").or(req.header("x-api-key"));
        let body = self
            .args
            .account_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| J::parse(&t).ok())
            .and_then(|spec| spec.get(route.key()).cloned())
            .filter(|b| !matches!(b, J::Null));
        let read = if route == account::Route::Usage {
            format!(" read={}", account::usage_read(q))
        } else {
            String::new()
        };
        let st = match &body {
            Some(b) => out.json(200, &account::expand(b, crate::util::now_secs()), &[])?,
            None => out.json(
                404,
                &err_body(
                    "not_found_error",
                    &format!("the lab's account file has no {}", route.key()),
                ),
                &[],
            )?,
        };
        let s = format!(
            "kind=account route={}{read} auth={} served={}",
            route.name(),
            auth_label(auth),
            if body.is_some() { "yes" } else { "none" }
        );
        self.record(method, path, st, 0, mode, &s);
        Ok(())
    }

    /// Codex's token refresh, pointed here by `CODEX_REFRESH_TOKEN_URL_OVERRIDE`.
    /// The body holds the lab's fake refresh token: never logged or dumped.
    /// `<modes-dir>/refresh.json` (`{"status", "json"}`) answers it, else a 401
    /// `refresh_token_expired` (what the token authority says of a login that has lapsed).
    fn oauth_token(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        auth: Option<&str>,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let file = self.args.modes_dir.as_ref().map(|d| d.join(REFRESH_FILE));
        let spec = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| J::parse(&t).ok())
            .filter(|j| matches!(j, J::Obj(_)));
        let from_file = spec.is_some();
        let spec = spec.unwrap_or_else(|| {
            J::obj([
                ("status", J::Int(401)),
                (
                    "json",
                    J::obj([(
                        "error",
                        J::obj([
                            ("code", J::s("refresh_token_expired")),
                            ("message", J::s("lab stub: the refresh token has expired")),
                        ]),
                    )]),
                ),
            ])
        });
        let status = spec
            .get("status")
            .and_then(J::as_i64)
            .and_then(|s| u16::try_from(s).ok())
            .unwrap_or(401);
        let answer = spec
            .get("json")
            .filter(|j| j.truthy())
            .cloned()
            .unwrap_or(J::Obj(Vec::new()));
        let st = out.json(status, &answer, &[])?;
        let s = format!(
            "kind=refresh auth={}{}",
            auth_label(auth),
            if from_file { " (refresh.json)" } else { "" }
        );
        let mode = self.mode();
        self.record(method, path, st, body.len(), &mode, &s);
        Ok(())
    }

    fn message_start(info: &Info, in_tok: i64) -> J {
        J::obj([
            ("type", J::s("message_start")),
            (
                "message",
                J::obj([
                    ("id", J::s(format!("msg_lab{}", rand_hex()))),
                    ("type", J::s("message")),
                    ("role", J::s("assistant")),
                    ("model", info.model.clone()),
                    ("content", J::Arr(Vec::new())),
                    ("stop_reason", J::Null),
                    ("stop_sequence", J::Null),
                    (
                        "usage",
                        J::obj([
                            ("input_tokens", J::Int(in_tok)),
                            ("cache_creation_input_tokens", J::Int(0)),
                            ("cache_read_input_tokens", J::Int(0)),
                            ("output_tokens", J::Int(1)),
                        ]),
                    ),
                ]),
            ),
        ])
    }

    /// A text reply as SSE: `deltas`, `pace` seconds apart.
    /// A streamed Messages answer, `extra` (a custom mode's `headers`) on its head.
    fn stream(
        &self,
        t: &Turn<'_>,
        summary: &str,
        deltas: &[String],
        pace: f64,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(extra)?;
        let in_tok = est_tokens(t.blen);
        let all_len = deltas.iter().map(String::len).sum::<usize>();
        let out_tok = i64::try_from(all_len / 4).unwrap_or(0).max(1);
        let sent = (|| -> io::Result<()> {
            out.sse("message_start", &Self::message_start(t.info, in_tok))?;
            out.sse(
                "content_block_start",
                &J::obj([
                    ("type", J::s("content_block_start")),
                    ("index", J::Int(0)),
                    (
                        "content_block",
                        J::obj([("type", J::s("text")), ("text", J::s(""))]),
                    ),
                ]),
            )?;
            out.sse("ping", &J::obj([("type", J::s("ping"))]))?;
            for d in deltas {
                if pace > 0.0 {
                    std::thread::sleep(Duration::from_secs_f64(pace));
                }
                out.sse(
                    "content_block_delta",
                    &J::obj([
                        ("type", J::s("content_block_delta")),
                        ("index", J::Int(0)),
                        (
                            "delta",
                            J::obj([("type", J::s("text_delta")), ("text", J::s(d.as_str()))]),
                        ),
                    ]),
                )?;
            }
            out.sse(
                "content_block_stop",
                &J::obj([("type", J::s("content_block_stop")), ("index", J::Int(0))]),
            )?;
            out.sse(
                "message_delta",
                &J::obj([
                    ("type", J::s("message_delta")),
                    (
                        "delta",
                        J::obj([
                            ("stop_reason", J::s("end_turn")),
                            ("stop_sequence", J::Null),
                        ]),
                    ),
                    ("usage", J::obj([("output_tokens", J::Int(out_tok))])),
                ]),
            )?;
            out.sse("message_stop", &J::obj([("type", J::s("message_stop"))]))?;
            out.chunk(b"")
        })();
        let note = match sent {
            Err(e) if closed(&e) => " (client closed mid-stream)",
            Err(e) => return Err(e),
            Ok(()) => "",
        };
        self.record_turn(
            t,
            200,
            &format!("{summary} sse={} deltas{note}", deltas.len()),
        );
        Ok(())
    }

    /// One tool use as SSE, its input split across two `input_json_delta`s.
    fn stream_tool(
        &self,
        t: &Turn<'_>,
        summary: &str,
        name: &str,
        input: &J,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        self.stream_tools(t, summary, &[(name.to_string(), input.clone())], out)
    }

    /// Tool uses in ONE assistant message as SSE, one content block each in
    /// order, each input split across two `input_json_delta`s.
    fn stream_tools(
        &self,
        t: &Turn<'_>,
        summary: &str,
        calls: &[(String, J)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(&[])?;
        let sent = (|| -> io::Result<()> {
            out.sse(
                "message_start",
                &Self::message_start(t.info, est_tokens(t.blen)),
            )?;
            for (index, (name, input)) in (0_i64..).zip(calls) {
                out.sse(
                    "content_block_start",
                    &J::obj([
                        ("type", J::s("content_block_start")),
                        ("index", J::Int(index)),
                        (
                            "content_block",
                            J::obj([
                                ("type", J::s("tool_use")),
                                ("id", J::s(format!("toolu_lab{}", rand_hex()))),
                                ("name", J::s(name.as_str())),
                                ("input", J::Obj(Vec::new())),
                            ]),
                        ),
                    ]),
                )?;
                out.sse("ping", &J::obj([("type", J::s("ping"))]))?;
                let js = input.dump(COMPACT);
                let chars: Vec<char> = js.chars().collect();
                let half = chars.len() / 2;
                for part in [&chars[..half], &chars[half..]] {
                    out.sse(
                        "content_block_delta",
                        &J::obj([
                            ("type", J::s("content_block_delta")),
                            ("index", J::Int(index)),
                            (
                                "delta",
                                J::obj([
                                    ("type", J::s("input_json_delta")),
                                    ("partial_json", J::s(part.iter().collect::<String>())),
                                ]),
                            ),
                        ]),
                    )?;
                }
                out.sse(
                    "content_block_stop",
                    &J::obj([
                        ("type", J::s("content_block_stop")),
                        ("index", J::Int(index)),
                    ]),
                )?;
            }
            out.sse(
                "message_delta",
                &J::obj([
                    ("type", J::s("message_delta")),
                    (
                        "delta",
                        J::obj([
                            ("stop_reason", J::s("tool_use")),
                            ("stop_sequence", J::Null),
                        ]),
                    ),
                    ("usage", J::obj([("output_tokens", J::Int(30))])),
                ]),
            )?;
            out.sse("message_stop", &J::obj([("type", J::s("message_stop"))]))?;
            out.chunk(b"")
        })();
        if let Err(e) = sent
            && !closed(&e)
        {
            return Err(e);
        }
        self.record_turn(t, 200, summary);
        Ok(())
    }

    /// The `bg` mode: `bgstart` → a background `sleep`; a tool result → a
    /// one-line end_turn; anything else (the wake) → a slow reply.
    fn bg(&self, t: &Turn<'_>, body: &[u8], summary: &str, out: &mut Out<'_>) -> io::Result<()> {
        let j = J::parse_bytes(body).unwrap_or(J::Obj(Vec::new()));
        let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
        // Claude Code ends the list with a role "system" message: the turn is decided by the last USER message.
        let last = msgs
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(J::as_str) == Some("user"));
        let has_result = last.is_some_and(is_tool_result);
        let last_text = text_of(last.and_then(|m| m.get("content")));
        if has_result {
            let s = format!("{summary} bg:tool_result -> short end_turn");
            let started = ["Background task started.".to_string()];
            return self.stream(t, &s, &started, 0.0, &[], out);
        }
        let (sleep_s, slow_s) = bg_params(t.mode, self.args.slow_seconds);
        if last_text.contains("bgstart") {
            let input = J::obj([
                ("command", J::s(format!("sleep {sleep_s}"))),
                ("description", J::s("Wait in the background")),
                ("run_in_background", J::Bool(true)),
            ]);
            let s = format!("{summary} bg:tool_use sleep {sleep_s}");
            return self.stream_tool(t, &s, "Bash", &input, out);
        }
        let deltas: Vec<String> = (1..=slow_s).map(|i| format!("tick {i} ")).collect();
        let s = format!(
            "{summary} bg:wake slow {slow_s}s last_user={}",
            py_repr(&one_line(&last_text, 60))
        );
        self.stream(t, &s, &deltas, 1.0, &[], out)
    }

    /// A `custom` mode's answer: its status, headers and body or stream, as its spec says.
    fn custom(&self, t: &Turn<'_>, spec: &J, summary: &str, out: &mut Out<'_>) -> io::Result<()> {
        let mode = t.mode;
        if let Some(ms) = spec
            .get("delay_ms")
            .and_then(J::as_f64)
            .filter(|v| *v > 0.0)
        {
            std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
        }
        let status = spec
            .get("status")
            .and_then(J::as_i64)
            .and_then(|s| u16::try_from(s).ok())
            .unwrap_or(200);
        let s = format!("{summary} custom");
        let headers = spec_headers(spec);
        if spec.get("stream").is_some_and(J::truthy) && status == 200 {
            let text = spec
                .get("text")
                .and_then(J::as_str)
                .map_or_else(|| format!("Hi! (custom mode {mode})"), str::to_string);
            let pace = spec.get("pace_s").and_then(J::as_f64).unwrap_or(0.0);
            return self.stream(t, &s, &split_deltas(&text), pace, &headers, out);
        }
        let st = if spec.get("json").is_some() || (status >= 400 && spec.get("body").is_none()) {
            let obj = match spec.get("json") {
                Some(J::Null) | None => err_body("api_error", &format!("custom mode {mode}")),
                Some(o) => o.clone(),
            };
            out.json(status, &obj, &headers)?
        } else {
            let raw = spec.get("body").map_or_else(String::new, |b| {
                b.as_str().map_or_else(|| b.py(), str::to_string)
            });
            let mut h = vec![
                ("content-length".to_string(), raw.len().to_string()),
                ("x-lab-stub".to_string(), "1".to_string()),
            ];
            h.extend(headers);
            out.head(status, &h)?;
            out.w.write_all(raw.as_bytes())?;
            status
        };
        self.record_turn(t, st, &s);
        Ok(())
    }

    /// Serve one connection until it closes.
    fn connection(&self, stream: TcpStream) {
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(read_half);
        let mut w = stream;
        while let Ok(Some(req)) = read_request(&mut reader) {
            if self.handle(&req, &mut w).is_err() || !req.keep_alive() {
                return;
            }
        }
    }

    /// The black-hole proxy: read a request line, log it, answer 502, close.
    fn black_hole(&self, mut s: TcpStream) {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        while !data.windows(2).any(|w| w == b"\r\n") && data.len() < 4096 {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
            }
        }
        let end = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap_or(data.len());
        // latin-1: each byte one character, as the request line was read before
        let first: String = data[..end].iter().map(|&b| char::from(b)).collect();
        self.st().proxy_attempts += 1;
        self.log(&format!("PROXY refused: {}", one_line(&first, 120)));
        let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\nx-lab-blackhole: 1\r\n\r\n");
    }
}

fn write_port_file(path: &Path, port: u16) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, format!("{port}\n"))?;
    std::fs::rename(&tmp, path)
}

/// Bind and start serving (threads); the ports are written to their files.
/// The API port and the black-hole proxy's, when one runs.
///
/// # Errors
/// A non-loopback host, or a bind that failed.
pub fn start(args: Args) -> Result<(u16, Option<u16>), String> {
    if !["127.0.0.1", "localhost", "::1"].contains(&args.host.as_str()) {
        return Err(format!(
            "stub: refuses to listen on anything but loopback (got {})",
            py_repr(&args.host)
        ));
    }
    let listener = TcpListener::bind((args.host.as_str(), args.port))
        .map_err(|e| format!("stub: bind: {e}"))?;
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())?;
    let stub = Stub::new(args.clone());
    if let Some(p) = &args.port_file {
        write_port_file(p, port).map_err(|e| format!("stub: {}: {e}", p.display()))?;
    }
    let mut proxy_port = None;
    if let Some(pp) = args.proxy_port {
        let pl = TcpListener::bind((args.host.as_str(), pp))
            .map_err(|e| format!("stub: proxy bind: {e}"))?;
        let ppn = pl
            .local_addr()
            .map(|a| a.port())
            .map_err(|e| e.to_string())?;
        proxy_port = Some(ppn);
        let s = Arc::clone(&stub);
        std::thread::spawn(move || {
            for conn in pl.incoming().flatten() {
                let s = Arc::clone(&s);
                std::thread::spawn(move || s.black_hole(conn));
            }
        });
        if let Some(p) = &args.proxy_port_file {
            write_port_file(p, ppn).map_err(|e| format!("stub: {}: {e}", p.display()))?;
        }
    }
    stub.log(&format!(
        "stub listening on {}:{port} (proxy black hole: {}) mode={} pid={}",
        args.host,
        proxy_port.map_or_else(|| "off".to_string(), |p| p.to_string()),
        stub.st().mode,
        std::process::id()
    ));
    let s = Arc::clone(&stub);
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let s = Arc::clone(&s);
            std::thread::spawn(move || s.connection(conn));
        }
    });
    Ok((port, proxy_port))
}

/// One control call to a stub on loopback (`/__mode?m=…`, `/__note?t=…`): the
/// body it answered. Nothing of the caller's steers it: no proxy, loopback only.
///
/// # Errors
/// The stub could not be reached, or did not answer 2xx.
pub fn control(port: &str, path_and_query: &str) -> Result<String, String> {
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let r = client
        .post(&format!("http://127.0.0.1:{port}{path_and_query}"))
        .send(b"")
        .map_err(|e| format!("the stub on port {port} did not answer: {e}"))?;
    if !r.is_success() {
        return Err(format!("the stub on port {port} answered {}", r.status()));
    }
    Ok(String::from_utf8_lossy(r.body()).into_owned())
}

/// A mode name for a query string (`%XX` for everything outside `[A-Za-z0-9_.:-]`).
#[must_use]
pub fn query_escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"_.:-".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `aterm-lab stub …`: serve until killed.
#[must_use]
pub fn main(argv: &[String]) -> i32 {
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    // the lab's driver holds the other end: its going (however it goes) ends the stub
    if let Some(fd) = args.lifeline_fd {
        match aterm_uds::lifeline::adopt(fd) {
            Ok(owned) => {
                std::thread::spawn(move || {
                    let _ = aterm_uds::lifeline::wait_for_cut(owned);
                    std::process::exit(0);
                });
            }
            Err(e) => {
                eprintln!("stub: --lifeline-fd {fd}: {e}");
                return 2;
            }
        }
    }
    let ready_fd = args.ready_fd;
    match start(args) {
        Ok(_ports) => {
            // Listening, its port files written: one byte, then the close.
            if let Some(fd) = ready_fd {
                let _ = crate::sys::write_all(fd, b"R");
                crate::sys::close(fd);
            }
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// The stub's log once it holds every one of `wants`, or as it stands when
/// `limit` is spent: the stub records a request after its answer is sent
/// (`Stub::record`), so a reader that got the answer may be ahead of the line.
#[cfg(test)]
fn logged_with(log: &Path, wants: &[&str], limit: Duration) -> String {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if wants.iter().all(|w| text.contains(w)) || std::time::Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests;
