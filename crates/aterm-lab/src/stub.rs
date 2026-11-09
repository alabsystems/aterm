// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub: a local stand-in for the Anthropic Messages API (and, beside it, the OpenAI
//! Responses, Gemini, Meta and xAI routes its dialects serve).

use std::io::{self, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::pyjson::{COMPACT, J};
use crate::util::{now, one_line, py_repr, stamp};

pub mod account;
pub mod chat;
pub mod gemini;
pub mod muse;
pub mod xai;

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
    /// The mode each credential's `/v1/messages` turns are answered in, by its label
    /// ([`auth_label`]): a JSON object `{label: mode}`, re-read per request ([`auth_mode`]); a
    /// credential it does not name is answered in the stub's own mode.
    pub auth_modes_file: Option<PathBuf>,
    /// Ticks of a `slow` reply ([`SLOW_SECONDS`] unless `--slow-seconds` says).
    pub slow_seconds: i64,
    /// The `retry-after` of `rate-limit`.
    pub retry_after: String,
    /// A descriptor whose end of file ends the stub (`--lifeline-fd`): the lab's driver went away.
    pub lifeline_fd: Option<i32>,
    /// A pipe's write end the stub writes one byte to.
    pub ready_fd: Option<i32>,
}

/// The ticks of a `slow` reply when `--slow-seconds` names none: one a second.
pub const SLOW_SECONDS: i64 = 30;

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
            auth_modes_file: None,
            slow_seconds: SLOW_SECONDS,
            retry_after: "5".into(),
            lifeline_fd: None,
            ready_fd: None,
        }
    }
}

/// Parse `aterm-lab stub` arguments.
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
            "--auth-modes-file" => a.auth_modes_file = Some(val()?.into()),
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

/// How many request dumps the stub keeps, the newest.
pub const DUMPS_KEPT: u64 = 2000;

/// The name a TURN's dump starts with: `req-<n>.json` ([`Stub::dump`]).
pub const TURN_DUMP: &str = "req-";
/// The name a SIDE CALL's dump starts with: `side-<n>.json` ([`is_side`]).
pub const SIDE_DUMP: &str = "side-";

/// Whether a request of `kind` (the log's `kind=`) is a SIDE CALL.
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
            | "grok-json"
            | "grok-suggest"
            | "grok-aux"
            | "chat-aux"
    )
}

/// The stub's dumps of the conversations' TURNS in `dir`, `(n, path)` each, the newest first.
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
    limit_remaining: Option<u64>,
    limit_arm: Option<LimitArm>,
    custom_budget: Vec<(String, Option<i64>)>,
    n_requests: i64,
    by_status: Vec<(String, i64)>,
    by_kind: Vec<(String, i64)>,
    proxy_attempts: i64,
}

/// The admission that installed a prearmed wall, before returning its last allowed answer.
#[derive(Debug)]
struct LimitArm {
    raised_ms: i64,
    reset_s: i64,
}

impl State {
    fn switch_mode(&mut self, mode: &str) {
        self.mode = mode.to_string();
        self.limit_remaining = None;
        self.limit_arm = None;
        self.custom_budget.retain(|(key, _)| key != mode);
    }
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
    /// The 200 answers to `/v1/messages` served so far that carried on200 headers.
    served_200: AtomicU32,
    /// The request bodies dumped so far ([`Stub::dump`]).
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
    /// The log's ` RL200[…] RL429[…]` tail.
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

/// The headers `spec` puts on the answer after `served` 200s that carried on200 headers.
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

/// The key of a `--headers-file` that holds each credential's own blocks, by its label
/// ([`rate_headers_for`]).
pub const BY_AUTH: &str = "by_auth";

/// The headers `spec` puts on the answer to the credential `label` after `served` 200s that
/// carried on200 headers: [`rate_headers`]' blocks with `spec.by_auth[label]`'s own blocks
/// merged over them, header by header ([`BY_AUTH`]), so each credential can read its own window.
#[must_use]
pub fn rate_headers_for(spec: &J, label: &str, served: u32) -> RateHeaders {
    let mut out = rate_headers(spec, served);
    if let Some(own) = spec.get(BY_AUTH).and_then(|by| by.get(label)) {
        let own = rate_headers(own, served);
        out.on200 = merged(&out.on200, &own.on200);
        out.on429 = merged(&out.on429, &own.on429);
    }
    out
}

/// The mode an `--auth-modes-file`'s JSON `modes` names for the credential `label`, cleaned as
/// `/__mode?m=` cleans one ([`clean_mode`]): `None` where it names none, names `off` or `""`, or
/// is no object, so the stub's own mode answers.
#[must_use]
pub fn auth_mode(modes: &J, label: &str) -> Option<String> {
    let J::Obj(_) = modes else {
        return None;
    };
    let m = modes.get(label)?.as_str()?.trim();
    (!m.is_empty() && m != "off").then(|| clean_mode(m))
}

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

/// An on429 header value `v` as the standing limit's 429 carries it: `@RESET@`, `@RESET+N@` or
/// `@RESET-N@` is the limit's reset (`reset`, unix seconds) moved by `N` seconds, so a window's
/// own reset header can name a reset apart from the notice's (`anthropic-ratelimit-unified-reset`)
/// however late the wall was raised; any other value is itself.
#[must_use]
pub fn reset_value(v: &str, reset: i64) -> String {
    reset_moved(v)
        .and_then(|by| reset.checked_add(by))
        .map_or_else(|| v.to_string(), |at| at.to_string())
}

/// How far `@RESET@`, `@RESET+N@` or `@RESET-N@` moves the limit's reset (seconds); `None` for
/// any other value ([`reset_value`]).
fn reset_moved(v: &str) -> Option<i64> {
    let rest = v.strip_prefix("@RESET")?.strip_suffix('@')?;
    let secs = |n: &str| {
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse::<i64>().ok())
            .flatten()
    };
    if rest.is_empty() {
        Some(0)
    } else if let Some(n) = rest.strip_prefix('+') {
        secs(n)
    } else if let Some(n) = rest.strip_prefix('-') {
        secs(n).map(|n| -n)
    } else {
        None
    }
}

/// The on429 headers a 429 carries: each `@RESET…@` value ([`reset_value`]) set against the
/// reset the 429 itself names (its `anthropic-ratelimit-unified-reset` among `named`), and left
/// off a 429 that names none (the stub's `rate-limit` mode, a custom answer naming no reset), so
/// no answer carries the template's words as a reset.
fn on429_resolved(on429: &[(String, String)], named: &[(String, String)]) -> Vec<(String, String)> {
    let reset = named
        .iter()
        .find(|(h, _)| h.eq_ignore_ascii_case("anthropic-ratelimit-unified-reset"))
        .and_then(|(_, v)| v.trim().parse::<i64>().ok());
    on429
        .iter()
        .filter_map(|(h, v)| match reset_moved(v) {
            None => Some((h.clone(), v.clone())),
            Some(_) => reset.map(|r| (h.clone(), reset_value(v, r))),
        })
        .collect()
}

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
    "limit-after",
    "401",
    "500",
    "bad-key",
    "drop",
];

const REFRESH_FILE: &str = "refresh.json";

/// Why `<modes-dir>/<name>.json` would never be served as the custom mode `name`.
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

/// A reply as a handful of text deltas.
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

/// The digest of received message text, after [`text_of`]: strings unchanged, text blocks
/// joined by literal newlines. Hash the complete UTF-8 text without trimming, folding spaces
/// or using the log's shortened single-line excerpt.
pub(crate) fn message_digest(text: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(text.as_bytes()))
}

/// THE SCRIPTED REPLY OF A CUSTOM MODE.
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

/// The index among a custom spec's `replies` of the rule [`scripted_reply`] picks for `users`,
/// which the stub's log names (`reply=<index>`), so a scene can tell which rule answered a request.
#[must_use]
pub fn scripted_rule(spec: &J, users: &[String]) -> Option<usize> {
    let picked = scripted_reply(spec, users)?;
    spec.get("replies")
        .and_then(J::as_arr)?
        .iter()
        .position(|r| std::ptr::eq(r, picked))
}

/// Whether a custom spec scripts its replies ([`scripted_reply`]).
#[must_use]
pub fn is_scripted(spec: &J) -> bool {
    spec.get("replies").is_some()
}

/// What a gate's file name ends in ([`gate_path`]).
pub const GATE_SUFFIX: &str = ".gate";

/// The longest a reply waits on its gate ([`gate_path`]): a one-minute hang detector, never a
/// pace. A gate still shut then is answered all the same, and the lab's want that needed it fails
/// on what it read.
pub const GATE_HANG: Duration = Duration::from_secs(60);

/// THE FILE A SCRIPTED RULE'S `gate` WAITS ON, in the custom modes' folder `modes`: the rule's
/// reply goes once the lab writes it (an event the lab read, never a timer). The name is kept to
/// a mode name's letters ([`clean_mode`]), so it never leaves the folder.
#[must_use]
pub fn gate_path(modes: &Path, gate: &str) -> PathBuf {
    modes.join(format!("{}{GATE_SUFFIX}", clean_mode(gate)))
}

/// `spec` with its `text` the reply `users` pick ([`scripted_reply`]).
#[must_use]
pub fn with_reply(spec: J, users: &[String]) -> J {
    let Some(rule) = scripted_reply(&spec, users).cloned() else {
        return spec;
    };
    let mut spec = spec;
    for key in ["text", "delay_ms", "pace_s", "tool", "gate"] {
        if let Some(v) = rule.get(key) {
            spec.set(key, v.clone());
        }
    }
    spec
}

/// A SCRIPTED RULE'S TOOL CALL.
#[must_use]
pub fn scripted_call(spec: &J, answered: bool) -> Option<(String, J)> {
    let call = spec.get("tool").filter(|c| matches!(c, J::Obj(_)))?;
    let name = call_name(call);
    (!answered && !name.is_empty()).then(|| (name, call_input(call)))
}

/// The tools a request `body` declares, by name, each with its arguments' schema (`None` for
/// one that declares none, such as a freeform or server tool): Gemini's
/// `tools[].functionDeclarations[]` (`parametersJsonSchema` or `parameters`), the Messages API's
/// `tools[]` (`input_schema`), the Responses API's `tools[]` (`parameters`), the Chat Completions
/// API's `tools[].function` (`parameters`), a Muse Code `namespace` tool's own `tools[]` among
/// them. `None` when the request declares no named tool, so a call has nothing to be read against.
#[must_use]
pub fn declared_tools(body: &[u8]) -> Option<Vec<(String, Option<J>)>> {
    fn named(t: &J, keys: &[&str]) -> Option<(String, Option<J>)> {
        let name = t.get("name").and_then(J::as_str)?.to_string();
        Some((name, keys.iter().find_map(|k| t.get(k)).cloned()))
    }
    fn of(tools: &[J]) -> Vec<(String, Option<J>)> {
        tools
            .iter()
            .flat_map(|t| {
                if let Some(decls) = t.get("functionDeclarations").and_then(J::as_arr) {
                    decls
                        .iter()
                        .filter_map(|d| named(d, &["parametersJsonSchema", "parameters"]))
                        .collect()
                } else if t.get("type").and_then(J::as_str) == Some("namespace") {
                    of(t.get("tools").and_then(J::as_arr).unwrap_or_default())
                } else if let Some(f) = t.get("function").filter(|f| matches!(f, J::Obj(_))) {
                    named(f, &["parameters"]).into_iter().collect()
                } else {
                    named(t, &["input_schema", "parameters"])
                        .into_iter()
                        .collect()
                }
            })
            .collect()
    }
    let j = J::parse_bytes(body).ok()?;
    let out = of(j.get("tools").and_then(J::as_arr)?);
    (!out.is_empty()).then_some(out)
}

/// What a scripted call of `name` with `input` gets wrong against the tools `declared`: a tool
/// none declares, an argument its schema requires and the call lacks, or one a closed schema
/// (`additionalProperties: false`) does not name. `None` when it fits. Only the schema's top
/// level is read: a nested shape, and each argument's type, are the agent's to refuse.
#[must_use]
pub fn misfit(declared: &[(String, Option<J>)], name: &str, input: &J) -> Option<String> {
    let Some((_, schema)) = declared.iter().find(|(n, _)| n == name) else {
        return Some(format!("{name}: no such tool declared"));
    };
    let schema = schema.as_ref()?;
    let given: Vec<&str> = match input {
        J::Obj(m) => m.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    };
    let mut wrong: Vec<String> = schema
        .get("required")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter_map(J::as_str)
        .filter(|r| !given.contains(r))
        .map(|r| format!("lacks {r}"))
        .collect();
    if schema.get("additionalProperties") == Some(&J::Bool(false)) {
        let props = schema.get("properties");
        wrong.extend(
            given
                .iter()
                .filter(|k| props.and_then(|p| p.get(k)).is_none())
                .map(|k| format!("undeclared {k}")),
        );
    }
    (!wrong.is_empty()).then(|| format!("{name}: {}", wrong.join(", ")))
}

/// What a stub log line adds for scripted `calls` that do not fit the tools `body` declares
/// ([`misfit`]): ` misfit='…'`, or nothing when each fits or the request declares no tool. An
/// agent refuses such a call itself, often with nothing on its screen to say why (agy 1.4.10
/// answered every undeclared argument with a tool result), so the stub's log says it.
#[must_use]
pub fn misfit_note(body: &[u8], calls: &[(&str, &J)]) -> String {
    let Some(declared) = declared_tools(body) else {
        return String::new();
    };
    let wrong: Vec<String> = calls
        .iter()
        .filter_map(|(name, input)| misfit(&declared, name, input))
        .collect();
    if wrong.is_empty() {
        String::new()
    } else {
        format!(" misfit={}", py_repr(&wrong.join("; ")))
    }
}

/// Whether a Messages API request's conversation carries a tool's result back.
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

/// The tool results a Messages API request carries back, oldest first, each as its text.
#[must_use]
pub fn message_results(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    tool_results(j.get("messages").and_then(J::as_arr).unwrap_or_default())
}

/// The user texts of a Messages API request, oldest first.
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

/// The facts of one `/v1/messages` request every answer logs.
#[derive(Clone, Copy)]
struct Turn<'a> {
    method: &'a str,
    path: &'a str,
    blen: usize,
    mode: &'a str,
    info: &'a Info,
}

impl<'a> Turn<'a> {
    const fn in_mode<'b>(self, mode: &'b str) -> Turn<'b>
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
    /// `main`, `aux`, `compact`, `quota` or `opaque`.
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

/// What the `tool` mode answers a request.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolTurn {
    /// Call this tool with this input (its `@SID:<k>@` filled).
    Call(String, J, usize),
    /// Reply with this text and end the turn.
    Say(String, usize),
    /// Call these tools.
    Calls(Vec<(String, J)>, usize),
    /// Every step is answered.
    Done,
}

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

/// `input` with every `@SID:<k>@` in its strings replaced by the first session id of
/// `results[k-1]`.
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

fn script_calls(script: &J) -> Vec<&J> {
    match script {
        J::Arr(a) => a.iter().collect(),
        one => vec![one],
    }
    .into_iter()
    .filter(|c| matches!(c, J::Obj(_)) && c.get("name").is_some_and(J::truthy))
    .collect()
}

fn call_name(c: &J) -> String {
    match c.get("name") {
        Some(J::Str(s)) => s.clone(),
        Some(other) => other.py(),
        None => String::new(),
    }
}

fn call_input(c: &J) -> J {
    c.get("input")
        .filter(|v| v.truthy())
        .cloned()
        .unwrap_or(J::Obj(Vec::new()))
}

/// The steps of a Claude Code script (one step or a list): each an object that calls a tool
/// (`name`), several (`calls`), or says `text`.
fn script_steps(script: &J) -> Vec<&J> {
    match script {
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
    .collect()
}

/// What step `c`, a conversation's `step`-th, answers, its `@SID:<k>@` filled from the tool
/// results so far (`results`, [`fill_sids`]).
fn step_turn(c: &J, step: usize, results: &[String]) -> ToolTurn {
    if c.get("name").is_some_and(J::truthy) {
        ToolTurn::Call(call_name(c), fill_sids(&call_input(c), results), step)
    } else if let Some(calls) = c.get("calls").filter(|_| c.get("text").is_none()) {
        ToolTurn::Calls(
            script_calls(calls)
                .into_iter()
                .map(|k| (call_name(k), fill_sids(&call_input(k), results)))
                .collect(),
            step,
        )
    } else {
        ToolTurn::Say(
            c.get("text").and_then(J::as_str).unwrap_or("").to_string(),
            step,
        )
    }
}

/// A step's `when`, the word its conversation's first prompt holds, or `None` (a main step).
fn step_when(c: &J) -> Option<String> {
    c.get("when")
        .filter(|w| w.truthy())
        .map(|w| w.as_str().map_or_else(|| w.py(), str::to_string))
}

/// The turn of the conversation `msgs` among `steps` keyed to it: those whose `when` its first
/// user message holds, in order, the conversation standing at the step its tool results and
/// replies count. `None` when no step is keyed to it.
fn keyed_turn(msgs: &[J], steps: &[&J], results: &[String]) -> Option<ToolTurn> {
    let first = msgs
        .iter()
        .find(|m| m.get("role").and_then(J::as_str) == Some("user"))
        .map(|m| text_of(m.get("content")))
        .unwrap_or_default();
    let mine: Vec<&&J> = steps
        .iter()
        .filter(|c| step_when(c).is_some_and(|w| first.contains(&w)))
        .collect();
    if mine.is_empty() {
        return None;
    }
    let done = msgs
        .iter()
        .filter(|m| is_tool_result(m) || is_said(m))
        .count();
    Some(match mine.get(done) {
        Some(c) => step_turn(c, done, results),
        None => ToolTurn::Done,
    })
}

/// [`ToolTurn`] of a request body against a Claude Code script (one step or a list).
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
    let steps = script_steps(script);
    let results = tool_results(msgs);
    if let Some(turn) = keyed_turn(msgs, &steps, &results) {
        return Some(turn);
    }
    let main: Vec<&&J> = steps.iter().filter(|c| step_when(c).is_none()).collect();
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
        if text_of(m.get("content")).contains("toolstart") {
            step = Some(0);
        }
    }
    let step = step?;
    Some(match main.get(step) {
        Some(c) => step_turn(c, step, &results),
        None => ToolTurn::Done,
    })
}

/// [`ToolTurn`] of a request body against the keyed steps of `script` alone (each with a `when`):
/// a custom mode's `script`, which answers only a conversation whose first prompt holds a step's
/// `when` and leaves every other to the mode's own replies, whatever it says (`toolstart`
/// included). `None` for a conversation no step is keyed to.
#[must_use]
pub fn keyed_tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
    keyed_turn(msgs, &script_steps(script), &tool_results(msgs))
}

/// `limit:CLAIM:RESET[:PLAN]` → `(claim, reset unix seconds, plan)`.
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

/// A nonnegative wall timestamp and delay, in milliseconds, rounded up to a whole Unix minute.
/// Timestamp arithmetic saturates at the supported range before rounding.
#[must_use]
pub(crate) const fn reset_s_ahead(now: i64, ahead: i64) -> i64 {
    now.saturating_add(ahead).saturating_add(59_999) / 60_000 * 60
}

/// A finite allowance of main requests, followed by a normal usage wall. Side requests do not
/// spend it. The reset is measured from the final admission, not from stub startup.
fn limit_after_params(mode: &str) -> Option<(u64, &str, i64, &str)> {
    let mut parts = mode.split(':');
    if parts.next()? != "limit-after" {
        return None;
    }
    let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let allowed = parts.next()?;
    if !uint(allowed) {
        return None;
    }
    let allowed = allowed.parse().ok()?;
    let claim = parts.next()?;
    let ahead = parts.next()?;
    if !uint(ahead) {
        return None;
    }
    let ahead = ahead.parse::<i64>().ok()?;
    let admitted_mode = parts.next().unwrap_or("ok");
    if parts.next().is_some()
        || claim.is_empty()
        || !claim
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        || ahead <= 0
        || admitted_mode.is_empty()
        || !admitted_mode
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return None;
    }
    Some((allowed, claim, ahead, admitted_mode))
}

/// A reserved finite configuration that cannot be admitted as an ordinary reply mode.
#[derive(Debug)]
struct InvalidFiniteGlobalMode {
    mode: String,
}

impl InvalidFiniteGlobalMode {
    fn answer(
        self,
        stub: &Stub,
        method: &str,
        path: &str,
        blen: usize,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.json(
            400,
            &err_body(
                "invalid_finite_global_mode",
                "limit-after needs N:CLAIM:AHEAD_MS[:ADMITTED_MODE]",
            ),
            &[],
        )
        .map(|status| {
            stub.record(
                method,
                path,
                status,
                blen,
                &self.mode,
                "invalid finite global mode refused",
            )
        })
    }
}

fn finite_allowance(mode: &str) -> Result<Option<(u64, &str, i64, &str)>, InvalidFiniteGlobalMode> {
    let parsed = limit_after_params(mode);
    if parsed.is_none()
        && mode
            .split(':')
            .next()
            .is_some_and(|h| h.eq_ignore_ascii_case("limit-after"))
    {
        Err(InvalidFiniteGlobalMode {
            mode: mode.to_string(),
        })
    } else {
        Ok(parsed)
    }
}

/// The fixed fake key the lab gives Codex's lab provider (never a credential anywhere).
pub const LAB_CODEX_KEY: &str = "lab-not-a-key";

/// The payload of a token the lab composed (a JWT with a `"lab"` label).
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

/// The head of a lab Claude OAuth token that names its label ([`labelled_oauth_token`]).
pub const LABELLED_OAUTH: &str = "sk-ant-oat01-LABLABLAB-";

/// Where a labelled token's label ends ([`labelled_oauth_token`]): no label holds it, a label
/// being lowercase.
const LABEL_END: &str = "-LAB";

/// A fake Claude OAuth token the stub reads as `oat:<label>` ([`auth_label`]): the realistic
/// `sk-ant-oat01-` head, the lab's `LABLABLAB` mark, the label, then `-LAB` and padding. No
/// credential anywhere. `label` is 1–24 characters of `[a-z0-9_-]`, else the token reads as the
/// plain `oat`.
#[must_use]
pub fn labelled_oauth_token(label: &str) -> String {
    format!("{LABELLED_OAUTH}{label}{LABEL_END}{}AA", "A".repeat(72))
}

/// The label of a lab token [`labelled_oauth_token`] made, where `tok` is one and its label is
/// 1–24 characters of `[a-z0-9_-]`.
fn oauth_label(tok: &str) -> Option<&str> {
    let rest = tok.strip_prefix(LABELLED_OAUTH)?;
    let label = &rest[..rest.find(LABEL_END)?];
    (!label.is_empty()
        && label.len() <= 24
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'))
    .then_some(label)
}

/// Which lab credential a request carried, NEVER its value.
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
    if tok == xai::LAB_XAI_KEY {
        return "xai-key".into();
    }
    if let Some(label) = oauth_label(tok) {
        return format!("oat:{label}");
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

/// The Chat Completions dialect named by the lab credential. Both use the same
/// paths; request models and message text do not select their wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatDialect {
    OpenAi,
    Xai,
}

impl ChatDialect {
    fn of(auth: Option<&str>) -> Self {
        if auth_label(auth) == "xai-key" {
            Self::Xai
        } else {
            Self::OpenAi
        }
    }
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

/// The text of a Responses input item.
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

/// The user texts of a Responses API request (Codex's, Muse Code's), oldest first.
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

/// The `retry-after` a `rate-limit:<secs>` mode names, in whole seconds (v6 §14.17.5.4): in the
/// words of the Responses API's body (`Please try again in <secs>s.`) and of Gemini's per-minute
/// quota (`RetryInfo`), and in the Anthropic API's `retry-after` header alone, its body naming no
/// number as the real API's does not. `None` for any other mode, and for the bare `rate-limit`,
/// whose Gemini answer is a spent day.
#[must_use]
pub fn rate_limit_after(mode: &str) -> Option<&str> {
    let secs = mode.strip_prefix("rate-limit:")?;
    (!secs.is_empty() && secs.bytes().all(|b| b.is_ascii_digit())).then_some(secs)
}

/// The OpenAI-compatible rate-limit error and retry header.
fn rate_limit_answer(retry_after: &str) -> WallAnswer {
    (
        429,
        J::obj([(
            "error",
            J::obj([
                ("type", J::s("rate_limit_exceeded")),
                ("code", J::s("rate_limit_exceeded")),
                (
                    "message",
                    J::s(format!(
                        "Rate limit reached for requests per min. Please try again in {retry_after}s."
                    )),
                ),
            ]),
        )]),
        vec![("retry-after".to_string(), retry_after.to_string())],
    )
}

/// The error a wall mode answers a Responses API request with.
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
        "rate-limit" => rate_limit_answer(retry_after),
        m if rate_limit_after(m).is_some() => rate_limit_answer(rate_limit_after(m)?),
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

/// A JSON text that fits a side request's schema: each string property `lab stub`, else `{}`.
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

/// The claim's window at 100% in Codex's header family.
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

/// The keys of a `--headers-file` that are [`rate_headers`]'s blocks (`headers` writes them).
pub const RATE_BLOCKS: [&str; 2] = ["on200", "on429"];

/// The `--headers-file` a verb leaves: `headers` (`blocks`) rewrites the [`RATE_BLOCKS`],
/// `ratelimits` the flat entries, and each keeps the other's and every credential's own blocks
/// ([`BY_AUTH`], which [`headers_file_for`] writes).
#[must_use]
pub fn headers_file_with(old: &str, new: J, blocks: bool) -> J {
    let own = |k: &str| k != BY_AUTH && RATE_BLOCKS.contains(&k) == blocks;
    let mut out: Vec<(String, J)> = match J::parse(old) {
        Ok(J::Obj(m)) => m.into_iter().filter(|(k, _)| !own(k)).collect(),
        _ => Vec::new(),
    };
    if let J::Obj(m) = new {
        out.extend(m.into_iter().filter(|(k, _)| own(k)));
    }
    J::Obj(out)
}

/// One credential's `on200` blocks ([`headers_file_for`]) with resets the caller chooses: both
/// windows allowed at `five`'s and `seven`'s utilization, the five-hour window resetting at
/// `five_reset` and the week's at `seven_reset` (Unix seconds), so a session's status-line windows
/// can be told from another credential's (DESIGN-claude-setup-token S9: a token session's windows
/// against its account's).
#[must_use]
pub fn chosen_resets((five, five_reset): (&str, i64), (seven, seven_reset): (&str, i64)) -> J {
    J::obj([(
        "on200",
        J::obj([
            ("anthropic-ratelimit-unified-status", J::s("allowed")),
            ("anthropic-ratelimit-unified-5h-status", J::s("allowed")),
            ("anthropic-ratelimit-unified-5h-utilization", J::s(five)),
            ("anthropic-ratelimit-unified-5h-reset", J::Int(five_reset)),
            ("anthropic-ratelimit-unified-7d-status", J::s("allowed")),
            ("anthropic-ratelimit-unified-7d-utilization", J::s(seven)),
            ("anthropic-ratelimit-unified-7d-reset", J::Int(seven_reset)),
            (
                "anthropic-ratelimit-unified-representative-claim",
                J::s("five_hour"),
            ),
        ]),
    )])
}

/// The `--headers-file` `headers-for` leaves: `old` with the credential `label`'s own blocks
/// ([`BY_AUTH`]) set to `blocks` (`{"on200": {…}, "on429": {…}}`), or removed where `blocks` holds
/// none; every other entry and credential kept.
#[must_use]
pub fn headers_file_for(old: &str, label: &str, blocks: J) -> J {
    let mut out = match J::parse(old) {
        Ok(j @ J::Obj(_)) => j,
        _ => J::Obj(Vec::new()),
    };
    let mut by: Vec<(String, J)> = match out.get(BY_AUTH) {
        Some(J::Obj(m)) => m.iter().filter(|(k, _)| k != label).cloned().collect(),
        _ => Vec::new(),
    };
    if matches!(&blocks, J::Obj(m) if !m.is_empty()) {
        by.push((label.to_string(), blocks));
    }
    if by.is_empty() {
        out.remove(BY_AUTH);
    } else {
        out.set(BY_AUTH, J::Obj(by));
    }
    out
}

/// The flat entries of a `--headers-file` (`ratelimits` writes them).
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
            let n: i64 = digits.parse().unwrap_or(i64::MAX);
            text = format!(
                "{}{}{}",
                &text[..i],
                now.saturating_add(n),
                &rest[digits.len() + 1..]
            );
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

fn spec_headers(spec: &J) -> Vec<(String, String)> {
    match spec.get("headers") {
        Some(J::Obj(m)) => m
            .iter()
            .filter(|(k, _)| {
                !k.is_empty()
                    && !k
                        .chars()
                        .any(|c| c == ':' || (c as u32) <= 0x20 || c == '\u{7f}')
            })
            .map(|(k, v)| {
                let raw = v.as_str().map_or_else(|| v.py(), str::to_string);
                let safe: String = raw
                    .chars()
                    .map(|c| {
                        if (c as u32) < 0x20 || c == '\u{7f}' {
                            ' '
                        } else {
                            c
                        }
                    })
                    .collect();
                (k.clone(), safe)
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn sleep_secs(secs: f64) {
    if secs.is_finite()
        && secs > 0.0
        && let Ok(d) = Duration::try_from_secs_f64(secs)
    {
        std::thread::sleep(d);
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

/// A mode name as `/__mode?m=` takes it.
#[must_use]
pub fn clean_mode(m: &str) -> String {
    let kept: String = m
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "_.:-".contains(*c))
        .take(64)
        .collect();
    let mut m = if kept.is_empty() {
        "ok".to_string()
    } else {
        kept
    };
    if m.starts_with("custom:") {
        m.drain(..7);
    }
    m
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
    /// `GET`, `POST`, ….
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

    fn websocket_upgrade(&self) -> bool {
        let has_token = |name: &str, token: &str| {
            self.headers.iter().any(|(k, v)| {
                k == name && v.split(',').any(|v| v.trim().eq_ignore_ascii_case(token))
            })
        };
        has_token("connection", "upgrade") && has_token("upgrade", "websocket")
    }

    fn keep_alive(&self) -> bool {
        let conn = self.header("connection").unwrap_or("");
        let has = |needle: &[u8]| {
            conn.as_bytes()
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle))
        };
        if has(b"close") {
            return false;
        }
        self.version == "HTTP/1.1" || has(b"keep-alive")
    }
}

const MAX_HEADER_LINES: usize = 256;
const MAX_BODY_BYTES: usize = 64 << 20;

/// Read one request; `None` at a clean end of the connection.
/// # Errors
/// A malformed request line or chunk, or the connection failing mid-request.
pub fn read_request(r: &mut impl io::BufRead) -> io::Result<Option<Request>> {
    use std::io::BufRead as _;
    const MAX_LINE_BYTES: u64 = 64 * 1024;
    let mut line = String::new();
    loop {
        line.clear();
        if (&mut *r).take(MAX_LINE_BYTES).read_line(&mut line)? == 0 {
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
        if (&mut *r).take(MAX_LINE_BYTES).read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many headers",
            ));
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
        if let Some(raw_len) = req.header("content-length") {
            let n: usize = raw_len
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad content-length"))?;
            if n > MAX_BODY_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request body too large",
                ));
            }
            if n > 0 {
                req.body.resize(n, 0);
                r.read_exact(&mut req.body)?;
            }
        } else if req
            .header("transfer-encoding")
            .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
        {
            loop {
                let mut size = String::new();
                (&mut *r).take(MAX_LINE_BYTES).read_line(&mut size)?;
                let size = size.trim();
                if size.is_empty() {
                    break;
                }
                let hex = size.split(';').next().unwrap_or("0").trim();
                let n = hex
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                    .then(|| usize::from_str_radix(hex, 16).ok())
                    .flatten()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                if n == 0 {
                    let mut end = String::new();
                    (&mut *r).take(MAX_LINE_BYTES).read_line(&mut end)?;
                    break;
                }
                if n > MAX_BODY_BYTES || req.body.len().saturating_add(n) > MAX_BODY_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "chunked request body too large",
                    ));
                }
                let mut chunk = vec![0u8; n];
                r.read_exact(&mut chunk)?;
                req.body.extend_from_slice(&chunk);
                let mut crlf = String::new();
                (&mut *r).take(MAX_LINE_BYTES).read_line(&mut crlf)?;
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

const fn reason(status: u16) -> &'static str {
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
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

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
        let rate = self.rate_for(status, &h);
        h.extend(rate);
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
        h.extend(self.rate_for(200, &[]));
        self.head(200, &h)
    }

    /// The `--headers-file`'s headers on a `status` answer whose own headers are `named`
    /// ([`on429_resolved`] for a 429).
    fn rate_for(&self, status: u16, named: &[(String, String)]) -> Vec<(String, String)> {
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
            429 => on429_resolved(&rate.on429, named),
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

    /// `spec` WITH THE REPLY `users` PICK ([`with_reply`]), once its rule's gate is open: every
    /// protocol takes a custom mode's reply here, so a gated rule holds its text or call alike.
    fn gated_reply(&self, spec: J, users: &[String]) -> J {
        let spec = with_reply(spec, users);
        self.hold_for_gate(&spec);
        spec
    }

    /// A RULE'S GATE ([`gate_path`]): its reply waits until the lab writes the gate's file, at
    /// most [`GATE_HANG`], and the stub's log says which.
    fn hold_for_gate(&self, spec: &J) {
        let (Some(gate), Some(dir)) = (
            spec.get("gate")
                .and_then(J::as_str)
                .filter(|g| !g.is_empty()),
            self.args.modes_dir.as_ref(),
        ) else {
            return;
        };
        let path = gate_path(dir, gate);
        let began = std::time::Instant::now();
        while !path.exists() && began.elapsed() < GATE_HANG {
            sleep_secs(0.2);
        }
        let opened = path.exists();
        self.log(&format!(
            "gate {gate} {} after {:.1} s",
            if opened { "opened" } else { "STILL SHUT" },
            began.elapsed().as_secs_f64()
        ));
    }

    /// Append one stamped line to the log.
    pub fn log(&self, line: &str) {
        let out = format!("{} {line}\n", stamp(now()));
        let _g = self.log_lock.lock().unwrap_or_else(|p| p.into_inner());
        match &self.args.log {
            Some(p) => {
                if let Ok(mut f) = crate::util::append_regular_file(p) {
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
        let mut st = self.st();
        let mut from_file = None;
        if let Some(path) = &self.args.mode_file
            && let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.is_file()
        {
            let mtime = meta.modified().ok();
            if st.mode_file_mtime != mtime {
                let text = crate::util::read_regular_text(path).unwrap_or_default();
                st.mode_file_mtime = mtime;
                let m = text.trim();
                if !m.is_empty() {
                    let m = clean_mode(m);
                    st.switch_mode(&m);
                    from_file = Some(m);
                }
            }
        }
        let mode = st.mode.clone();
        drop(st);
        if let Some(m) = from_file {
            self.log(&format!("MODE -> {m} (from file)"));
        }
        mode
    }

    /// Persist and switch the mode from the control path.
    ///
    /// # Errors
    /// Returns a configured mode file's write error without changing the active mode.
    pub fn set_mode(&self, m: &str) -> io::Result<()> {
        let m = clean_mode(m);
        {
            let mut st = self.st();
            let mtime = if let Some(path) = &self.args.mode_file {
                crate::util::write_out(path, format!("{m}\n").as_bytes())?;
                std::fs::symlink_metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
            } else {
                None
            };
            st.switch_mode(&m);
            st.mode_file_mtime = mtime;
        }
        self.log(&format!("MODE -> {m}"));
        Ok(())
    }

    /// Resolve a request once its protocol has classified it. The last allowed main request
    /// installs the wall under the same lock that admits it, before any answer can provoke the
    /// next main request. A request holding an older finite-mode snapshot resolves the current
    /// mode under that lock too; a newer finite allowance must be admitted, not returned raw.
    fn mode_for_request(&self, mode: &str, main: bool) -> Result<String, InvalidFiniteGlobalMode> {
        if finite_allowance(mode)?.is_none() {
            return Ok(mode.to_string());
        }
        let mut st = self.st();
        let current = st.mode.clone();
        let Some((allowed, claim, ahead, admitted_mode)) = finite_allowance(&current)? else {
            return Ok(current);
        };
        if !main {
            return Ok(admitted_mode.to_string());
        }
        let remaining = st.limit_remaining.get_or_insert(allowed);
        let admitted = *remaining > 0;
        *remaining = remaining.saturating_sub(1);
        if *remaining > 0 {
            return Ok(admitted_mode.to_string());
        }
        let raised_ms = crate::util::now_ms();
        let reset_s = reset_s_ahead(raised_ms, ahead);
        st.mode = format!("limit:{claim}:{reset_s}");
        st.limit_arm = Some(LimitArm { raised_ms, reset_s });
        if admitted {
            Ok(admitted_mode.to_string())
        } else {
            Ok(st.mode.clone())
        }
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

    fn headers_spec(&self) -> Option<J> {
        self.args
            .headers_file
            .as_ref()
            .and_then(|p| crate::util::read_regular_text(p).ok())
            .and_then(|t| J::parse(&t).ok())
    }

    fn answer_headers(&self, label: &str) -> RateHeaders {
        self.headers_spec()
            .map_or_else(RateHeaders::default, |spec| {
                rate_headers_for(&spec, label, self.served_200.load(Ordering::Relaxed))
            })
    }

    /// The mode the `--auth-modes-file` names for the credential `label` ([`auth_mode`]).
    fn credential_mode(&self, label: &str) -> Option<String> {
        let path = self.args.auth_modes_file.as_ref()?;
        let modes = J::parse(&crate::util::read_regular_text(path).ok()?).ok()?;
        auth_mode(&modes, label)
    }

    fn take_custom(&self, mode: &str, label: Option<&str>) -> Option<J> {
        self.take_custom_if(mode, label, |_| true)
    }

    /// Check this request's eligibility before spending a finite custom reply.
    fn take_custom_if(
        &self,
        mode: &str,
        label: Option<&str>,
        accepts: impl FnOnce(&J) -> bool,
    ) -> Option<J> {
        let spec = self.custom_spec(mode)?;
        if !accepts(&spec) {
            return None;
        }
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
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains(':')
            || not_custom(name).is_some()
        {
            return None;
        }
        let dir = self.args.modes_dir.as_ref()?;
        let text = crate::util::read_regular_text(&dir.join(format!("{name}.json"))).ok()?;
        J::parse(&text).ok().filter(|j| matches!(j, J::Obj(_)))
    }

    fn reply_text(&self, kind: &str, mode: &str, n: i64) -> String {
        if let Some(p) = &self.args.reply_file
            && let Ok(t) = crate::util::read_regular_text(p)
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

    fn dump(&self, body: &[u8], kind: &str) {
        let Some(dir) = &self.args.dump_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        let idx = self.dumped.fetch_add(1, Ordering::Relaxed) + 1;
        let prefix = if is_side(kind) { SIDE_DUMP } else { TURN_DUMP };
        let _ = crate::util::write_fresh(
            &dir.join(format!("{prefix}{idx:05}.json")),
            &body[..body.len().min(4 << 20)],
        );
        if let Some(old) = idx.checked_sub(DUMPS_KEPT) {
            for prefix in [TURN_DUMP, SIDE_DUMP] {
                let _ = std::fs::remove_file(dir.join(format!("{prefix}{old:05}.json")));
            }
        }
    }

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
        // THE HOST UNREACHABLE (`drop`, v6 §14.17.5's stand-in): the connection closed with no
        // answer, as a dropped route or a refused connection reaches the program, at once (a
        // connection left open would hold the program to its own request timeout).
        if mode == "drop" {
            self.record(&req.method, path, 0, blen, &mode, "dropped: no answer");
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "dropped: no answer",
            ));
        }
        if let Err(refusal) = finite_allowance(&mode) {
            return refusal.answer(self, &req.method, path, blen, &mut out);
        }
        let auth = req.header("authorization");
        let gemini_label = || {
            let key = req
                .header("x-goog-api-key")
                .or_else(|| q.iter().find(|(k, _)| k == "key").map(|(_, v)| v.as_str()));
            gemini::credential_label(key, auth)
        };
        let result = if req.method == "POST" && path == "/v1/messages" {
            let label = auth_label(auth);
            let credential_mode = self.credential_mode(&label);
            // A credential override has explicit provenance here, before any mode snapshot
            // comparison. Finite admission owns global State; it is not a per-key mode.
            if credential_mode.as_deref().is_some_and(|m| {
                m.split(':')
                    .next()
                    .is_some_and(|h| h.eq_ignore_ascii_case("limit-after"))
            }) {
                return out.json(400, &err_body(
                    "finite_mode_requires_global",
                    "limit-after must be configured as the global stub mode, not a credential override",
                ), &[]).map(|status| self.record(&req.method, path, status, blen, &mode, "finite credential override refused"));
            }
            let mode = credential_mode.unwrap_or(mode);
            self.messages(&req.method, path, &req.body, (mode, &label), &mut out)
        } else if req.method == "GET" && path == "/v1/responses" && req.websocket_upgrade() {
            // Codex falls back to HTTP on 426; a handshake must not take a scripted POST reply.
            out.json(
                426,
                &err_body(
                    "websocket_not_supported",
                    "lab stub supports HTTP POST /v1/responses only",
                ),
                &[],
            )
            .map(|st| self.record(&req.method, path, st, blen, &mode, "websocket refused"))
        } else if req.method == "POST" && path == "/v1/responses" {
            let d = Dialect::Codex;
            self.responses(&req.method, path, &req.body, &mode, auth, d, &mut out)
        } else if req.method == "POST" && path == "/responses" {
            self.muse(&req.method, path, &req.body, &mode, req, &mut out)
        } else if req.method == "GET" && path == "/v1/models" {
            match ChatDialect::of(auth) {
                ChatDialect::OpenAi => self.chat_models(&req.method, path, &mode, auth, &mut out),
                ChatDialect::Xai => {
                    self.xai_models(&req.method, path, &mode, &auth_label(auth), &mut out)
                }
            }
        } else if req.method == "POST" && path == "/v1/chat/completions" {
            match ChatDialect::of(auth) {
                ChatDialect::OpenAi => {
                    self.chat(&req.method, path, &req.body, &mode, auth, &mut out)
                }
                ChatDialect::Xai => self.xai_chat(
                    &req.method,
                    path,
                    &req.body,
                    &mode,
                    &auth_label(auth),
                    &mut out,
                ),
            }
        } else if req.method == "GET" && path == "/muse-code/models" {
            self.muse_models(&req.method, path, &mode, auth, &mut out)
        } else if req.method == "POST" && path == "/oauth/token" {
            self.oauth_token(&req.method, path, &req.body, auth, &mut out)
        } else if req.method == "GET" && path == "/backend-api/wham/accounts/check" {
            self.accounts_check(&req.method, path, &mode, req, &mut out)
        } else if req.method == "GET" && path == "/backend-api/wham/usage" {
            self.wham_usage(
                &req.method,
                path,
                &mode,
                auth,
                req.header("chatgpt-account-id"),
                &mut out,
            )
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

    fn control(&self, path: &str, q: &[(String, String)], out: &mut Out<'_>) -> io::Result<bool> {
        let arg = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        match path {
            "/__mode" => {
                if let Some(m) = arg("m")
                    && let Err(error) = self.set_mode(&m)
                {
                    out.json(
                        500,
                        &err_body("api_error", &format!("mode was not changed: {error}")),
                        &[],
                    )?;
                    return Ok(true);
                }
                out.json(200, &J::obj([("mode", J::s(self.mode()))]), &[])?;
            }
            "/__status" => {
                self.mode();
                let st = self.st();
                let counts = |v: &[(String, i64)]| {
                    J::Obj(v.iter().map(|(k, n)| (k.clone(), J::Int(*n))).collect())
                };
                let snap = J::obj([
                    ("mode", J::s(st.mode.as_str())),
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
                        "limit_arm",
                        st.limit_arm.as_ref().map_or(J::Null, |arm| {
                            J::obj([
                                ("raised_ms", J::Int(arm.raised_ms)),
                                ("reset_s", J::Int(arm.reset_s)),
                            ])
                        }),
                    ),
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
                    .and_then(|p| crate::util::read_regular_text(p).ok())
                    .unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                let tail: Vec<J> = lines[lines.len().saturating_sub(n)..]
                    .iter()
                    .map(|l| J::s(*l))
                    .collect();
                out.json(200, &J::obj([("lines", J::Arr(tail))]), &[])?;
            }
            "/__note" => {
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
                    st.limit_remaining = None;
                    st.limit_arm = None;
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
        (mode, label): (String, &str),
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let info = analyse(body);
        let kind = info.kind;
        let mode = match self.mode_for_request(&mode, kind == "main") {
            Ok(mode) => mode,
            Err(refusal) => return refusal.answer(self, method, path, body.len(), out),
        };
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
            "kind={kind} model={model} stream={} max_tokens={max_tokens} msgs={} tools={} sys={}c last_user=\"{}\" auth={label}",
            info.stream,
            info.msgs,
            info.tools,
            info.sys_chars,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let rate = self.answer_headers(label);
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
                .and_then(|p| crate::util::read_regular_text(p).ok())
                .and_then(|t| J::parse(&t).ok());
            if let Some(turn) = script.and_then(|s| tool_turn(body, &s)) {
                return self.tool_turn_answer(&base.in_mode(&mode), body, &summary, turn, out);
            }
        }
        let mut mode = mode;
        if let Some(sp) =
            self.take_custom_if(&mode, Some(label), |sp| kind == "main" || !is_scripted(sp))
        {
            // A custom mode's `script` answers the conversations keyed to it, step by step, as the
            // `tool` mode does; its replies answer every other.
            let keyed = sp
                .get("script")
                .filter(|_| kind == "main")
                .and_then(|s| keyed_tool_turn(body, s));
            if let Some(turn) = keyed {
                return self.tool_turn_answer(&base.in_mode(&mode), body, &summary, turn, out);
            }
            let users = message_users(body);
            let summary = match scripted_rule(&sp, &users) {
                Some(rule) => format!(
                    "{summary} reply={rule} last_user_sha256={}",
                    message_digest(&info.last_user)
                ),
                None => summary,
            };
            let sp = self.gated_reply(sp, &users);
            if let Some((name, input)) = scripted_call(&sp, messages_answered(body)) {
                let note = misfit_note(body, &[(&name, &input)]);
                let s = format!("{summary} custom tool:{name}{note}");
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
                if let Some(rate) = out.rate.as_ref() {
                    for (_, v) in &rate.on429 {
                        let at = reset_value(v, reset);
                        if at != *v {
                            summary = summary.replace(v.as_str(), &at);
                        }
                    }
                }
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
                let s = format!(
                    "{summary} limit:{claim} resets in {}s",
                    reset.saturating_sub(now_s)
                );
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
            m if m == "rate-limit" || rate_limit_after(m).is_some() => {
                // The real API's body names no number: its wait is the `retry-after` header
                // alone, which Claude Code never draws, so its harness tries on the ladder.
                let msg = "This request would exceed your organization's rate limit of 50 requests per minute.";
                let after = rate_limit_after(m)
                    .map_or_else(|| self.args.retry_after.clone(), str::to_string);
                let h = [("retry-after".to_string(), after)];
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

    /// A Responses API turn.
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
        let mut mode = match self.mode_for_request(mode, matches!(kind, "codex" | "muse")) {
            Ok(mode) => mode,
            Err(refusal) => return refusal.answer(self, method, path, blen, out),
        };
        if dialect == Dialect::Muse && mode == "tool" && kind == "muse" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| crate::util::read_regular_text(p).ok())
                .and_then(|t| J::parse(&t).ok());
            let facts = (method, path, blen, mode.as_str());
            match script.and_then(|s| muse::tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let note = misfit_note(body, &[(&name, &input)]);
                    let s = format!("{summary} tool:{name} step {step}{note}");
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
                Some(ToolTurn::Say(..) | ToolTurn::Calls(..)) | None => {}
            }
        }
        if let Some(spec) = self.take_custom_if(&mode, Some(&label), |sp| {
            !matches!(kind, "codex-json" | "muse-json") || !is_scripted(sp)
        }) {
            let spec = self.gated_reply(spec, &responses_users(body));
            if dialect == Dialect::Muse
                && let Some((name, input)) = scripted_call(&spec, muse::results_answered(body))
            {
                let note = misfit_note(body, &[(&name, &input)]);
                let s = format!("{summary} custom tool:{name}{note}");
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
                sleep_secs(ms / 1000.0);
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
                    reset.saturating_sub(now_s),
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

    /// A plain text reply as the Responses API's SSE.
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
                sleep_secs(pace);
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

    fn wham_usage(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        auth: Option<&str>,
        account_id: Option<&str>,
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
                    ("limit_window_seconds", J::Int(mins.saturating_mul(60))),
                    (
                        "reset_after_seconds",
                        J::Int(reset.saturating_sub(now).max(0)),
                    ),
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
        let mut body = J::obj([
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
        if let Some(account_id) = account_id.filter(|id| !id.is_empty()) {
            body.set("account_id", J::s(account_id));
        }
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
            .and_then(|p| crate::util::read_regular_text(p).ok())
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
            .and_then(|f| crate::util::read_regular_text(f).ok())
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
                sleep_secs(pace);
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

    /// Answer a request (`body`) at `turn` of its script ([`tool_turn`], [`keyed_tool_turn`]): the
    /// call or calls it makes, the words it says, or, every step answered, `Tool call done.`.
    fn tool_turn_answer(
        &self,
        t: &Turn<'_>,
        body: &[u8],
        summary: &str,
        turn: ToolTurn,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        match turn {
            ToolTurn::Call(name, input, step) => {
                let note = misfit_note(body, &[(&name, &input)]);
                let s = format!("{summary} tool:{name} step {step}{note}");
                self.stream_tool(t, &s, &name, &input, out)
            }
            ToolTurn::Calls(calls, step) => {
                let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
                let each: Vec<(&str, &J)> = calls.iter().map(|(n, i)| (n.as_str(), i)).collect();
                let note = misfit_note(body, &each);
                let s = format!("{summary} tool:{} step {step}{note}", names.join("+"));
                self.stream_tools(t, &s, &calls, out)
            }
            ToolTurn::Say(text, step) => {
                let s = format!("{summary} tool:text step {step}");
                self.stream(t, &s, &split_deltas(&text), 0.0, &[], out)
            }
            ToolTurn::Done => {
                let s = format!("{summary} tool:done");
                self.stream(t, &s, &["Tool call done.".to_string()], 0.0, &[], out)
            }
        }
    }

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

    fn bg(&self, t: &Turn<'_>, body: &[u8], summary: &str, out: &mut Out<'_>) -> io::Result<()> {
        let j = J::parse_bytes(body).unwrap_or(J::Obj(Vec::new()));
        let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
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

    fn custom(&self, t: &Turn<'_>, spec: &J, summary: &str, out: &mut Out<'_>) -> io::Result<()> {
        let mode = t.mode;
        if let Some(ms) = spec
            .get("delay_ms")
            .and_then(J::as_f64)
            .filter(|v| *v > 0.0)
        {
            sleep_secs(ms / 1000.0);
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
        let first: String = data[..end].iter().map(|&b| char::from(b)).collect();
        self.st().proxy_attempts += 1;
        self.log(&format!("PROXY refused: {}", one_line(&first, 120)));
        let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\nx-lab-blackhole: 1\r\n\r\n");
    }
}

fn write_port_file(path: &Path, port: u16) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let written = opts
        .open(&tmp)
        .and_then(|mut f| f.write_all(format!("{port}\n").as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Bind and start serving (threads); the ports are written to their files.
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
            while let Ok((conn, _)) = aterm_uds::accept(
                &pl,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) {
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
        while let Ok((conn, _)) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            let s = Arc::clone(&s);
            std::thread::spawn(move || s.connection(conn));
        }
    });
    Ok((port, proxy_port))
}

/// One control call to a stub on loopback (`/__mode?m=…`, `/__note?t=…`): the body it answered.
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

/// The stub's log once it holds every one of `wants`.
#[cfg(test)]
fn logged_with(log: &Path, wants: &[&str], limit: Duration) -> String {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let text = crate::util::read_regular_text(log).unwrap_or_default();
        if wants.iter().all(|w| text.contains(w)) || std::time::Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests;
