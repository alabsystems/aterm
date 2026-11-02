// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stub OpenAI Chat Completions API (`GET /v1/models`, `POST /v1/chat/completions`) for Kimi
//! Code lab runs: the wire an `openai`- or `kimi`-type provider in its `config.toml` speaks.

use std::io;
use std::time::Duration;

use super::{Out, Stub, ToolTurn, Turn, analyse, auth_label, bump, call_input, call_name, closed};
use super::{
    est_tokens, is_scripted, limit_params, misfit_note, rand_hex, rate_limit_answer,
    responses_error, script_calls,
};
use super::{scripted_call, spec_headers, split_deltas, text_of};
use crate::pyjson::{COMPACT, J};
use crate::util::{one_line, py_repr};

/// The model the stub's catalog lists, and the one the lab's provider names.
pub const MODEL: &str = "lab-model";

/// The `GET /v1/models` catalog.
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
                ("owned_by", J::s("lab")),
            ])]),
        ),
    ])
}

/// What one `/v1/chat/completions` request is, read off its body.
#[derive(Clone, Debug)]
pub struct CInfo {
    /// Its `model` (as sent).
    pub model: J,
    /// Whether it asks for SSE.
    pub stream: bool,
    /// How many messages it holds.
    pub msgs: usize,
    /// How many tools it offers.
    pub tools: usize,
    /// The last user message's text.
    pub last_user: String,
    /// `chat` (a turn: it offers tools), `chat-aux` (a side call: none) or `opaque`.
    pub kind: &'static str,
}

fn messages(j: &J) -> &[J] {
    j.get("messages").and_then(J::as_arr).unwrap_or_default()
}

fn role(m: &J) -> &str {
    m.get("role").and_then(J::as_str).unwrap_or("")
}

/// A person's message: a user message that is no runtime `<system-reminder>` note (Kimi Code
/// sends one as a user message of its own after the person's).
fn is_person(m: &J) -> bool {
    role(m) == "user"
        && !text_of(m.get("content"))
            .trim_start()
            .starts_with("<system-reminder")
}

/// [`CInfo`] of a body (best effort, never fails).
#[must_use]
pub fn analyse_chat(body: &[u8]) -> CInfo {
    let mut info = CInfo {
        model: J::s("?"),
        stream: false,
        msgs: 0,
        tools: 0,
        last_user: String::new(),
        kind: "chat",
    };
    let Ok(j @ J::Obj(_)) = J::parse_bytes(body) else {
        info.kind = "opaque";
        return info;
    };
    info.model = j.get("model").cloned().unwrap_or(J::s("?"));
    info.stream = j.get("stream").is_some_and(J::truthy);
    let msgs = messages(&j);
    info.msgs = msgs.len();
    info.tools = j.get("tools").and_then(J::as_arr).map_or(0, <[J]>::len);
    info.last_user = chat_users(body).pop().unwrap_or_default();
    if info.tools == 0 {
        info.kind = "chat-aux";
    }
    info
}

/// The person's texts of a request ([`is_person`]), oldest first.
#[must_use]
pub fn chat_users(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    messages(&j)
        .iter()
        .filter(|m| is_person(m))
        .map(|m| text_of(m.get("content")))
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// The messages after the person's newest message (the tool results of the turn in hand).
fn since_person(j: &J) -> &[J] {
    let msgs = messages(j);
    let at = msgs.iter().rposition(is_person).map_or(0, |i| i + 1);
    &msgs[at..]
}

/// Whether a tool's result came back since the person's newest message.
#[must_use]
pub fn results_answered(body: &[u8]) -> bool {
    J::parse_bytes(body).is_ok_and(|j| since_person(&j).iter().any(|m| role(m) == "tool"))
}

/// The next scripted [`ToolTurn`] of a `tool`-mode request: the script's first step once the
/// person's newest text says `toolstart`, one step further per tool result after it.
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let person = messages(&j).iter().rev().find(|m| is_person(m))?;
    if !text_of(person.get("content")).contains("toolstart") {
        return None;
    }
    let step = since_person(&j)
        .iter()
        .filter(|m| role(m) == "tool")
        .count();
    let calls: Vec<&J> = script_calls(script)
        .into_iter()
        .filter(|c| !c.get("when").is_some_and(J::truthy))
        .collect();
    Some(match calls.get(step) {
        Some(c) => ToolTurn::Call(call_name(c), call_input(c), step),
        None => ToolTurn::Done,
    })
}

/// The newest tool result's text (one line, up to 200 chars).
#[must_use]
pub fn last_tool_output(body: &[u8]) -> Option<String> {
    let j = J::parse_bytes(body).ok()?;
    let m = messages(&j).iter().rev().find(|m| role(m) == "tool")?;
    Some(one_line(&text_of(m.get("content")), 200))
}

/// One streamed answer: `chat.completion.chunk` lines, a usage chunk, then `[DONE]`.
struct Sse {
    id: String,
    model: J,
    created: i64,
}

impl Sse {
    fn new(model: J) -> Sse {
        Sse {
            id: format!("chatcmpl-lab{}", rand_hex()),
            model,
            created: crate::util::now_secs(),
        }
    }

    fn line(out: &mut Out<'_>, j: &J) -> io::Result<()> {
        out.chunk(format!("data: {}\n\n", j.dump(COMPACT)).as_bytes())
    }

    fn chunk(&self, out: &mut Out<'_>, delta: J, finish: J) -> io::Result<()> {
        let choice = J::obj([
            ("index", J::Int(0)),
            ("delta", delta),
            ("finish_reason", finish),
        ]);
        Self::line(out, &self.wrap(vec![choice]))
    }

    fn wrap(&self, choices: Vec<J>) -> J {
        J::obj([
            ("id", J::s(self.id.as_str())),
            ("object", J::s("chat.completion.chunk")),
            ("created", J::Int(self.created)),
            ("model", self.model.clone()),
            ("choices", J::Arr(choices)),
        ])
    }

    /// The text as `deltas`, `pace` seconds apart.
    fn text(&self, out: &mut Out<'_>, deltas: &[String], pace: f64) -> io::Result<()> {
        let first = J::obj([("role", J::s("assistant")), ("content", J::s(""))]);
        self.chunk(out, first, J::Null)?;
        for d in deltas {
            if pace > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(pace));
            }
            self.chunk(out, J::obj([("content", J::s(d.as_str()))]), J::Null)?;
        }
        self.chunk(out, J::Obj(Vec::new()), J::s("stop"))
    }

    /// One call of the function `name` with `args`.
    fn call(&self, out: &mut Out<'_>, name: &str, args: &J) -> io::Result<()> {
        let call = |fields: Vec<(&str, J)>| {
            let mut c = vec![("index".to_string(), J::Int(0))];
            c.extend(fields.into_iter().map(|(k, v)| (k.to_string(), v)));
            J::obj([("tool_calls", J::Arr(vec![J::Obj(c)]))])
        };
        let mut head = call(vec![
            ("id", J::s(format!("call_lab{}", rand_hex()))),
            ("type", J::s("function")),
            (
                "function",
                J::obj([("name", J::s(name)), ("arguments", J::s(""))]),
            ),
        ]);
        head.set("role", J::s("assistant"));
        head.set("content", J::Null);
        self.chunk(out, head, J::Null)?;
        let body = call(vec![(
            "function",
            J::obj([("arguments", J::s(args.dump(COMPACT)))]),
        )]);
        self.chunk(out, body, J::Null)?;
        self.chunk(out, J::Obj(Vec::new()), J::s("tool_calls"))
    }

    /// The usage chunk, `[DONE]` and the end of the stream.
    fn finish(&self, out: &mut Out<'_>, in_tok: i64, out_tok: i64) -> io::Result<()> {
        let mut u = self.wrap(Vec::new());
        u.set("usage", usage(in_tok, out_tok));
        Self::line(out, &u)?;
        out.chunk(b"data: [DONE]\n\n")?;
        out.chunk(b"")
    }
}

fn usage(in_tok: i64, out_tok: i64) -> J {
    J::obj([
        ("prompt_tokens", J::Int(in_tok)),
        ("completion_tokens", J::Int(out_tok)),
        ("total_tokens", J::Int(in_tok + out_tok)),
    ])
}

/// What one answer is: a text, or a call.
enum Answer<'a> {
    Text(&'a [String], f64),
    Call(&'a str, &'a J),
}

impl Stub {
    /// Handles `GET /v1/models`.
    pub(super) fn chat_models(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        auth: Option<&str>,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let st = out.json(200, &catalog(), &[])?;
        let s = format!("kind=chat-models auth={} models=1", auth_label(auth));
        self.record(method, path, st, 0, mode, &s);
        Ok(())
    }

    /// Handles `POST /v1/chat/completions`, a turn or a side call, as the mode says.
    #[allow(clippy::too_many_lines)]
    pub(super) fn chat(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mode: &str,
        auth: Option<&str>,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let info = analyse_chat(body);
        let kind = info.kind;
        let label = auth_label(auth);
        let n = bump(&mut self.st().by_kind, kind);
        let model = match &info.model {
            J::Str(s) => s.clone(),
            other => other.py(),
        };
        let mut summary = format!(
            "kind={kind} auth={label} model={model} stream={} msgs={} tools={} last_user=\"{}\"",
            info.stream,
            info.msgs,
            info.tools,
            one_line(&info.last_user, 70)
        );
        self.dump(body, kind);
        let blen = body.len();
        let in_tok = est_tokens(blen);
        let facts = (method, path, blen);
        let mut mode = match self.mode_for_request(mode, kind == "chat") {
            Ok(mode) => mode,
            Err(refusal) => return refusal.answer(self, method, path, blen, out),
        };
        if mode == "tool" && kind == "chat" {
            let script = self
                .args
                .tool_file
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| J::parse(&t).ok());
            match script.and_then(|s| tool_turn(body, &s)) {
                Some(ToolTurn::Call(name, input, step)) => {
                    let note = misfit_chat(body, &name, &input);
                    let s = format!("{summary} tool:{name} step {step}{note}");
                    let a = Answer::Call(&name, &input);
                    return self.chat_answer(facts, &mode, &info, &s, &a, in_tok, &[], out);
                }
                Some(ToolTurn::Done) => {
                    let result = last_tool_output(body).unwrap_or_default();
                    let s = format!("{summary} tool:done result={}", py_repr(&result));
                    let done = split_deltas(&format!("Tool call done. Result: {result}"));
                    let a = Answer::Text(&done, 0.0);
                    return self.chat_answer(facts, &mode, &info, &s, &a, in_tok, &[], out);
                }
                Some(ToolTurn::Say(..) | ToolTurn::Calls(..)) | None => {}
            }
        }
        if let Some(spec) =
            self.take_custom_if(&mode, Some(&label), |sp| kind == "chat" || !is_scripted(sp))
        {
            let spec = self.gated_reply(spec, &chat_users(body));
            if let Some((name, input)) = scripted_call(&spec, results_answered(body)) {
                let note = misfit_chat(body, &name, &input);
                let s = format!("{summary} custom tool:{name}{note}");
                let a = Answer::Call(&name, &input);
                return self.chat_answer(facts, &mode, &info, &s, &a, in_tok, &[], out);
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
                let deltas = split_deltas(&text);
                let s = format!("{summary} custom");
                let a = Answer::Text(&deltas, pace);
                let h = spec_headers(&spec);
                return self.chat_answer(facts, &mode, &info, &s, &a, in_tok, &h, out);
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
            summary.push_str(" custom not used");
            mode = "ok".into();
        }
        if mode.split(':').next() == Some("limit") {
            let Some((claim, reset, _plan)) = limit_params(&mode) else {
                let msg = format!("lab stub: mode {mode} is not limit:CLAIM:RESET[:PLAN]");
                let error = J::obj([(
                    "error",
                    J::obj([
                        ("type", J::s("invalid_request_error")),
                        ("message", J::s(msg)),
                    ]),
                )]);
                let status = out.json(400, &error, &[])?;
                self.record(method, path, status, blen, &mode, &summary);
                return Ok(());
            };
            let now_s = crate::util::now_secs();
            if now_s < reset {
                let wait = reset.saturating_sub(now_s);
                let (status, body, headers) = rate_limit_answer(&wait.to_string());
                let status = out.json(status, &body, &headers)?;
                let summary = format!("{summary} limit:{claim} resets in {wait}s");
                self.record(method, path, status, blen, &mode, &summary);
                return Ok(());
            }
            summary.push_str(" limit lifted (reset passed)");
            mode = "ok".into();
        }
        if let Some((status, err, h)) = responses_error(&mode, &self.args.retry_after) {
            let st = out.json(status, &err, &h)?;
            self.record(method, path, st, blen, &mode, &summary);
            return Ok(());
        }
        let deltas: Vec<String> = if mode == "slow" && kind == "chat" {
            (1..=self.args.slow_seconds.max(1))
                .map(|i| format!("tick {i} "))
                .collect()
        } else {
            split_deltas(&self.reply_text("main", &mode, n))
        };
        let pace = if mode == "slow" && kind == "chat" {
            1.0
        } else {
            0.0
        };
        let a = Answer::Text(&deltas, pace);
        self.chat_answer(facts, &mode, &info, &summary, &a, in_tok, &[], out)
    }

    /// Sends `answer`: streamed when the request asked for SSE, else one `chat.completion`.
    #[allow(clippy::too_many_arguments)]
    fn chat_answer(
        &self,
        (method, path, blen): (&str, &str, usize),
        mode: &str,
        info: &CInfo,
        summary: &str,
        answer: &Answer<'_>,
        in_tok: i64,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let out_tok = match answer {
            Answer::Text(d, _) => i64::try_from(d.concat().chars().count() / 4)
                .unwrap_or(0)
                .max(1),
            Answer::Call(..) => 30,
        };
        if !info.stream {
            let message = match answer {
                Answer::Text(d, _) => {
                    J::obj([("role", J::s("assistant")), ("content", J::s(d.concat()))])
                }
                Answer::Call(name, args) => J::obj([
                    ("role", J::s("assistant")),
                    ("content", J::Null),
                    (
                        "tool_calls",
                        J::Arr(vec![J::obj([
                            ("id", J::s(format!("call_lab{}", rand_hex()))),
                            ("type", J::s("function")),
                            (
                                "function",
                                J::obj([
                                    ("name", J::s(*name)),
                                    ("arguments", J::s(args.dump(COMPACT))),
                                ]),
                            ),
                        ])]),
                    ),
                ]),
            };
            let finish = if matches!(answer, Answer::Call(..)) {
                "tool_calls"
            } else {
                "stop"
            };
            let reply = J::obj([
                ("id", J::s(format!("chatcmpl-lab{}", rand_hex()))),
                ("object", J::s("chat.completion")),
                ("created", J::Int(crate::util::now_secs())),
                ("model", info.model.clone()),
                (
                    "choices",
                    J::Arr(vec![J::obj([
                        ("index", J::Int(0)),
                        ("message", message),
                        ("finish_reason", J::s(finish)),
                    ])]),
                ),
                ("usage", usage(in_tok, out_tok)),
            ]);
            let st = out.json(200, &reply, extra)?;
            self.record(method, path, st, blen, mode, &format!("{summary} json"));
            return Ok(());
        }
        out.stream_head(extra)?;
        let s = Sse::new(info.model.clone());
        let sent = (|| -> io::Result<()> {
            match answer {
                Answer::Text(d, pace) => s.text(out, d, *pace)?,
                Answer::Call(name, args) => s.call(out, name, args)?,
            }
            s.finish(out, in_tok, out_tok)
        })();
        let note = match sent {
            Err(e) if closed(&e) => " (client closed mid-stream)",
            Err(e) => return Err(e),
            Ok(()) => "",
        };
        let what = match answer {
            Answer::Text(d, _) => format!("sse={} deltas", d.len()),
            Answer::Call(..) => "sse=call".to_string(),
        };
        self.record(
            method,
            path,
            200,
            blen,
            mode,
            &format!("{summary} {what}{note}"),
        );
        Ok(())
    }
}

/// [`misfit_note`] for a chat request, whose tools nest their schema under `function`.
fn misfit_chat(body: &[u8], name: &str, input: &J) -> String {
    let Ok(j) = J::parse_bytes(body) else {
        return String::new();
    };
    let tools: Vec<J> = j
        .get("tools")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t.get("function").cloned())
        .collect();
    let flat = J::obj([("tools", J::Arr(tools))]).dump(COMPACT);
    misfit_note(flat.as_bytes(), &[(name, input)])
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
