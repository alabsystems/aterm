// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub's xAI API (`GET /v1/models`, `POST /v1/chat/completions`) for Grok Build lab runs:
//! the OpenAI Chat Completions wire Grok Build speaks to `GROK_XAI_API_BASE_URL` when a model
//! names no `api_backend` (its guide: "When you omit `api_backend`, Grok uses
//! `chat_completions`"). An error is xAI's REST shape, `{"code": <status text>, "error": <why>}`.

use std::io;
use std::time::Duration;

use super::{Out, Stub, ToolTurn, Turn, analyse, bump, call_input, call_name, closed};
use super::{est_tokens, limit_params, rand_hex, script_calls, spec_headers, split_deltas};
use crate::pyjson::{COMPACT, J};
use crate::util::one_line;

/// The key the lab hands Grok Build as `XAI_API_KEY`: no account's, and the stub knows it.
pub const LAB_XAI_KEY: &str = "xai-lab-not-a-key";

/// The model the stub's catalog offers.
pub const MODEL: &str = "lab-stub";

/// The `GET /v1/models` catalog: one model, OpenAI's list shape.
#[must_use]
pub fn catalog() -> J {
    J::obj([
        ("object", J::s("list")),
        (
            "data",
            J::Arr(vec![J::obj([
                ("id", J::s(MODEL)),
                ("object", J::s("model")),
                ("created", J::Int(0)),
                ("owned_by", J::s("xai")),
            ])]),
        ),
    ])
}

/// How Grok Build 1.0.70's prompt predictor opens its system prompt: its answer is drawn in the
/// idle composer as a suggestion (`Tab/→:accept suggestion`), and `NONE` draws none.
pub const PREDICTOR: &str = "You predict the next line the USER will type";

/// What the stub answers the prompt predictor in `mode`: the custom mode's `suggest` text, else
/// `NONE`, so an idle composer shows no suggestion unless a mode asks for one.
#[must_use]
pub fn suggestion(spec: Option<&J>) -> String {
    spec.and_then(|s| s.get("suggest"))
        .and_then(J::as_str)
        .unwrap_or("NONE")
        .to_string()
}

/// The text of a Chat Completions message's `content`: the string, or its text parts joined.
#[must_use]
pub fn content_text(m: &J) -> String {
    match m.get("content") {
        Some(J::Str(s)) => s.clone(),
        Some(J::Arr(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(J::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(J::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn role(m: &J) -> &str {
    m.get("role").and_then(J::as_str).unwrap_or("")
}

/// The current person's message and everything returned after it. Older
/// conversation history cannot consume a new turn's scripted tool call.
fn current_messages(messages: &[J]) -> &[J] {
    messages
        .iter()
        .rposition(|message| role(message) == "user")
        .map_or(&[], |at| &messages[at..])
}

/// What one `/v1/chat/completions` request is, read off its body.
#[derive(Clone, Debug)]
pub struct CInfo {
    /// Its `model` (as sent).
    pub model: String,
    /// Whether it asks for SSE.
    pub stream: bool,
    /// How many messages it holds.
    pub msgs: usize,
    /// How many tools it offers.
    pub tools: usize,
    /// Tool results after the latest user message (`role: tool`).
    pub results: usize,
    /// The system prompt's length in characters.
    pub sys_chars: usize,
    /// The last user message's text.
    pub last_user: String,
    /// `grok` (a turn that offers tools), `grok-json` (a structured side request), `grok-suggest`
    /// (the prompt predictor's side request, [`PREDICTOR`]) or `grok-aux`.
    pub kind: &'static str,
}

/// [`CInfo`] of a body (best effort, never fails).
#[must_use]
pub fn analyse_chat(body: &[u8]) -> CInfo {
    let mut info = CInfo {
        model: "?".into(),
        stream: false,
        msgs: 0,
        tools: 0,
        results: 0,
        sys_chars: 0,
        last_user: String::new(),
        kind: "grok-aux",
    };
    let Ok(j @ J::Obj(_)) = J::parse_bytes(body) else {
        info.kind = "opaque";
        return info;
    };
    info.model = j
        .get("model")
        .and_then(J::as_str)
        .unwrap_or("?")
        .to_string();
    info.stream = j.get("stream").is_some_and(J::truthy);
    let msgs = j.get("messages").and_then(J::as_arr).unwrap_or_default();
    info.msgs = msgs.len();
    info.tools = j.get("tools").and_then(J::as_arr).map_or(0, <[J]>::len);
    info.results = current_messages(msgs)
        .iter()
        .filter(|m| role(m) == "tool")
        .count();
    info.sys_chars = msgs
        .iter()
        .filter(|m| matches!(role(m), "system" | "developer"))
        .map(|m| content_text(m).chars().count())
        .sum();
    info.last_user = msgs
        .iter()
        .rev()
        .find(|m| role(m) == "user")
        .map(content_text)
        .unwrap_or_default();
    let json = j
        .get("response_format")
        .and_then(|f| f.get("type"))
        .and_then(J::as_str)
        .is_some_and(|t| t.starts_with("json"));
    let predicts = msgs
        .iter()
        .filter(|m| matches!(role(m), "system" | "developer"))
        .any(|m| content_text(m).starts_with(PREDICTOR));
    info.kind = if json {
        "grok-json"
    } else if predicts && info.tools == 0 {
        "grok-suggest"
    } else if info.tools > 0 {
        "grok"
    } else {
        "grok-aux"
    };
    info
}

/// The user texts of a request, oldest first.
#[must_use]
pub fn chat_users(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    j.get("messages")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|m| role(m) == "user")
        .map(content_text)
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// What the `tool` mode answers a turn: the script's next call once a user message says
/// `toolstart`, each tool result after it moving one step on, then [`ToolTurn::Done`].
/// A later user message starts a new turn, even when it requests no tool.
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let calls: Vec<&J> = script_calls(script)
        .into_iter()
        .filter(|c| !c.get("when").is_some_and(J::truthy))
        .collect();
    let mut step: Option<usize> = None;
    for m in current_messages(j.get("messages").and_then(J::as_arr).unwrap_or_default()) {
        match role(m) {
            "tool" => step = step.map(|s| s + 1),
            "user" if content_text(m).contains("toolstart") => step = Some(0),
            _ => {}
        }
    }
    let step = step?;
    Some(match calls.get(step) {
        Some(c) => ToolTurn::Call(call_name(c), call_input(c), step),
        None => ToolTurn::Done,
    })
}

/// The last tool result after the current person's message, if one exists.
#[must_use]
pub fn last_result(body: &[u8]) -> Option<String> {
    let j = J::parse_bytes(body).ok()?;
    current_messages(j.get("messages").and_then(J::as_arr).unwrap_or_default())
        .iter()
        .rev()
        .find(|m| role(m) == "tool")
        .map(content_text)
}

/// An xAI REST error body: `{"code": <status text>, "error": <why>}`.
#[must_use]
pub fn xai_error(code: &str, error: &str) -> J {
    J::obj([("code", J::s(code)), ("error", J::s(error))])
}

/// The error a wall mode answers a turn with: its status, body and headers.
#[must_use]
pub fn mode_error(mode: &str, retry_after: &str) -> Option<super::WallAnswer> {
    Some(match mode {
        "rate-limit" => (
            429,
            xai_error(
                "Some resource has been exhausted",
                "lab stub: too many requests, try again later",
            ),
            vec![("retry-after".to_string(), retry_after.to_string())],
        ),
        "401" | "bad-key" => (
            401,
            xai_error(
                "The request does not have valid authentication credentials for the operation",
                "Incorrect API key provided: xa***ey. You can obtain an API key from https://console.x.ai.",
            ),
            Vec::new(),
        ),
        "overloaded" => (
            503,
            xai_error(
                "The service is currently unavailable",
                "lab stub: overloaded",
            ),
            Vec::new(),
        ),
        "500" => (
            500,
            xai_error("Internal error", "lab stub: the server had an error"),
            Vec::new(),
        ),
        _ => return None,
    })
}

/// One streamed piece of an answer: text, or a tool call.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// Text after a pause of this many seconds.
    Text(f64, String),
    /// A function call: its name and its arguments.
    Call(String, J),
}

/// `text` as pieces of [`split_deltas`], each after `pace` seconds.
#[must_use]
pub fn text_pieces(text: &str, pace: f64) -> Vec<Piece> {
    split_deltas(text)
        .into_iter()
        .map(|d| Piece::Text(pace, d))
        .collect()
}

/// The `slow` turn: a `tick N` of text a second for `secs` seconds.
#[must_use]
pub fn slow_pieces(secs: i64) -> Vec<Piece> {
    (1..=secs.max(1))
        .map(|i| Piece::Text(1.0, format!("tick {i} ")))
        .collect()
}

/// A `chat.completion.chunk`.
#[must_use]
pub fn chunk(
    id: &str,
    model: &str,
    delta: J,
    finish: Option<&str>,
    usage: Option<(i64, i64)>,
) -> J {
    let mut c = J::obj([
        ("id", J::s(id)),
        ("object", J::s("chat.completion.chunk")),
        ("created", J::Int(crate::util::now_secs())),
        ("model", J::s(model)),
        (
            "choices",
            J::Arr(vec![J::obj([
                ("index", J::Int(0)),
                ("delta", delta),
                ("finish_reason", finish.map_or(J::Null, J::s)),
            ])]),
        ),
    ]);
    if let Some((i, o)) = usage {
        c.set(
            "usage",
            J::obj([
                ("prompt_tokens", J::Int(i)),
                ("completion_tokens", J::Int(o)),
                ("total_tokens", J::Int(i + o)),
            ]),
        );
    }
    c
}

/// The whole answer as one `chat.completion` (a request with `stream` off).
#[must_use]
pub fn completion(id: &str, model: &str, pieces: &[Piece], usage: (i64, i64)) -> J {
    let text: String = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Text(_, t) => Some(t.as_str()),
            Piece::Call(..) => None,
        })
        .collect();
    let calls: Vec<J> = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Call(name, args) => Some(call_json(name, args, None)),
            Piece::Text(..) => None,
        })
        .collect();
    let mut message = J::obj([("role", J::s("assistant")), ("content", J::s(text))]);
    let finish = if calls.is_empty() {
        "stop"
    } else {
        message.set("tool_calls", J::Arr(calls));
        "tool_calls"
    };
    J::obj([
        ("id", J::s(id)),
        ("object", J::s("chat.completion")),
        ("created", J::Int(crate::util::now_secs())),
        ("model", J::s(model)),
        (
            "choices",
            J::Arr(vec![J::obj([
                ("index", J::Int(0)),
                ("message", message),
                ("finish_reason", J::s(finish)),
            ])]),
        ),
        (
            "usage",
            J::obj([
                ("prompt_tokens", J::Int(usage.0)),
                ("completion_tokens", J::Int(usage.1)),
                ("total_tokens", J::Int(usage.0 + usage.1)),
            ]),
        ),
    ])
}

fn call_json(name: &str, args: &J, index: Option<i64>) -> J {
    let mut c = J::obj([
        ("id", J::s(format!("call_lab{}", rand_hex()))),
        ("type", J::s("function")),
        (
            "function",
            J::obj([
                ("name", J::s(name)),
                ("arguments", J::s(args.dump(COMPACT))),
            ]),
        ),
    ]);
    if let Some(i) = index {
        c.set("index", J::Int(i));
    }
    c
}

/// The facts of one request every answer logs: method, path, body length, mode, model.
type Facts<'a> = (&'a str, &'a str, usize, &'a str, &'a str);

impl Stub {
    /// `GET /v1/models`.
    pub(super) fn xai_models(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        auth: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let st = out.json(200, &catalog(), &[])?;
        self.record(
            method,
            path,
            st,
            0,
            mode,
            &format!("kind=grok-models auth={auth}"),
        );
        Ok(())
    }

    /// `POST /v1/chat/completions`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn xai_chat(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mode: &str,
        auth: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let info = analyse_chat(body);
        let kind = info.kind;
        let selected_mode = match self.mode_for_request(mode, kind == "grok") {
            Ok(mode) => mode,
            Err(refusal) => return refusal.answer(self, method, path, body.len(), out),
        };
        let mode = selected_mode.as_str();
        let n = bump(&mut self.st().by_kind, kind);
        let mut summary = format!(
            "kind={kind} auth={auth} model={} stream={} msgs={} tools={} sys={}c results={} last_user=\"{}\"",
            info.model,
            info.stream,
            info.msgs,
            info.tools,
            info.sys_chars,
            info.results,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let blen = body.len();
        let in_tok = est_tokens(blen);
        let model = info.model.as_str();
        if kind != "grok" {
            let f = (method, path, blen, mode, model);
            let text = match kind {
                "grok-json" => "{}".to_string(),
                "grok-suggest" => suggestion(self.custom_spec(mode).as_ref()),
                _ => "Lab stub session".to_string(),
            };
            let p = text_pieces(&text, 0.0);
            return self.xai_answer(f, info.stream, &summary, &p, in_tok, &[], out);
        }
        if mode == "tool" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| J::parse(&t).ok());
            let f = (method, path, blen, mode, model);
            match script.and_then(|s| tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let note = super::misfit_note(body, &[(&name, &input)]);
                    let s = format!("{summary} tool:{name} step {step}{note}");
                    let p = [Piece::Call(name, input)];
                    return self.xai_answer(f, info.stream, &s, &p, in_tok, &[], out);
                }
                Some(ToolTurn::Done) => {
                    let result = last_result(body).unwrap_or_default();
                    let s = format!(
                        "{summary} tool:done result={}",
                        crate::util::py_repr(&one_line(&result, 160))
                    );
                    let p = text_pieces("Tool call done.", 0.0);
                    return self.xai_answer(f, info.stream, &s, &p, in_tok, &[], out);
                }
                Some(ToolTurn::Say(..) | ToolTurn::Calls(..)) | None => {}
            }
        }
        let mut mode = mode.to_string();
        if let Some(spec) = self.take_custom(&mode, Some(auth)) {
            let spec = self.gated_reply(spec, &chat_users(body));
            let f = (method, path, blen, mode.as_str(), model);
            if let Some((name, input)) = super::scripted_call(&spec, info.results > 0) {
                let note = super::misfit_note(body, &[(&name, &input)]);
                let s = format!("{summary} custom tool:{name}{note}");
                let p = [Piece::Call(name, input)];
                return self.xai_answer(f, info.stream, &s, &p, in_tok, &[], out);
            }
            let status = spec
                .get("status")
                .and_then(J::as_i64)
                .and_then(|s| u16::try_from(s).ok())
                .unwrap_or(200);
            if status == 200 && spec.get("stream").is_none_or(J::truthy) {
                if let Some(ms) = spec
                    .get("delay_ms")
                    .and_then(J::as_f64)
                    .filter(|v| *v > 0.0)
                {
                    std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
                }
                let text = spec
                    .get("text")
                    .and_then(J::as_str)
                    .map_or_else(|| format!("Hi! (custom mode {mode})"), str::to_string);
                let pace = spec.get("pace_s").and_then(J::as_f64).unwrap_or(0.0);
                let s = format!("{summary} custom");
                let p = text_pieces(&text, pace);
                return self.xai_answer(f, info.stream, &s, &p, in_tok, &spec_headers(&spec), out);
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
            let Some((claim, reset, _plan)) = limit_params(&mode) else {
                let msg = format!("lab stub: mode {mode} is not limit:CLAIM:RESET");
                let st = out.json(
                    400,
                    &xai_error("Client specified an invalid argument", &msg),
                    &[],
                )?;
                self.record(method, path, st, blen, &mode, &summary);
                return Ok(());
            };
            let now_s = crate::util::now_secs();
            if now_s < reset {
                let wait = reset - now_s;
                let body = xai_error(
                    "Some resource has been exhausted",
                    &format!("lab stub: {claim} limit reached, resets in {wait}s"),
                );
                let h = [("retry-after".to_string(), wait.to_string())];
                let st = out.json(429, &body, &h)?;
                let s = format!("{summary} limit:{claim} resets in {wait}s");
                self.record(method, path, st, blen, &mode, &s);
                return Ok(());
            }
            summary.push_str(" limit lifted (reset passed)");
            mode = "ok".into();
        }
        if let Some((status, err, h)) = mode_error(&mode, &self.args.retry_after) {
            let st = out.json(status, &err, &h)?;
            self.record(method, path, st, blen, &mode, &summary);
            return Ok(());
        }
        let pieces = if mode == "slow" {
            slow_pieces(self.args.slow_seconds)
        } else {
            text_pieces(&self.reply_text("main", &mode, n), 0.0)
        };
        let f = (method, path, blen, mode.as_str(), model);
        self.xai_answer(f, info.stream, &summary, &pieces, in_tok, &[], out)
    }

    /// An answer of `pieces`: SSE `data:` chunks ending in `data: [DONE]`, or one
    /// `chat.completion` when the request did not ask for a stream.
    #[allow(clippy::too_many_arguments)]
    fn xai_answer(
        &self,
        (method, path, blen, mode, model): Facts<'_>,
        stream: bool,
        summary: &str,
        pieces: &[Piece],
        in_tok: i64,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let id = format!("chatcmpl-lab{}", rand_hex());
        let chars: usize = pieces
            .iter()
            .map(|p| match p {
                Piece::Text(_, t) => t.chars().count(),
                Piece::Call(_, a) => a.dump(COMPACT).chars().count(),
            })
            .sum();
        let out_tok = i64::try_from(chars / 4).unwrap_or(0).max(1);
        if !stream {
            let st = out.json(
                200,
                &completion(&id, model, pieces, (in_tok, out_tok)),
                extra,
            )?;
            self.record(method, path, st, blen, mode, &format!("{summary} json"));
            return Ok(());
        }
        out.stream_head(extra)?;
        let send = |out: &mut Out<'_>, c: &J| {
            out.chunk(format!("data: {}\n\n", c.dump(COMPACT)).as_bytes())
        };
        let sent = (|| -> io::Result<()> {
            send(
                out,
                &chunk(
                    &id,
                    model,
                    J::obj([("role", J::s("assistant")), ("content", J::s(""))]),
                    None,
                    None,
                ),
            )?;
            let mut calls = 0;
            for p in pieces {
                match p {
                    Piece::Text(pause, t) => {
                        if *pause > 0.0 {
                            std::thread::sleep(Duration::from_secs_f64(*pause));
                        }
                        send(
                            out,
                            &chunk(
                                &id,
                                model,
                                J::obj([("content", J::s(t.as_str()))]),
                                None,
                                None,
                            ),
                        )?;
                    }
                    Piece::Call(name, args) => {
                        let call = call_json(name, args, Some(calls));
                        calls += 1;
                        send(
                            out,
                            &chunk(
                                &id,
                                model,
                                J::obj([("tool_calls", J::Arr(vec![call]))]),
                                None,
                                None,
                            ),
                        )?;
                    }
                }
            }
            let finish = if calls > 0 { "tool_calls" } else { "stop" };
            send(
                out,
                &chunk(
                    &id,
                    model,
                    J::Obj(Vec::new()),
                    Some(finish),
                    Some((in_tok, out_tok)),
                ),
            )?;
            out.chunk(b"data: [DONE]\n\n")?;
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
            &format!("{summary} sse={} pieces{note}", pieces.len()),
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "xai_tests.rs"]
mod tests;
