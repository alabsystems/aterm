// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub's Meta model API, as Muse Code (Meta's `muse`, measured on
//! 1.4.10-R6427.1) speaks it once `--base-url http://127.0.0.1:<port>` names
//! the stub and `META_API_KEY` holds the lab's fixed fake key (the Codex
//! provider's, [`super::LAB_CODEX_KEY`], sent as a bearer token and logged as
//! `auth=key`).
//!
//! Routes: `GET /muse-code/models`, the catalog ([`catalog`]: one model,
//! [`MODEL`], whose `metadata` is keyed `muse-code`; a model without that
//! key is dropped, and an empty catalog stops the TUI with `model catalog
//! has no visible models`), answered in every mode; and `POST /responses`,
//! the OpenAI Responses API with `stream: true`. Its tools come as ONE
//! `{"type": "namespace", "name": "muse", "tools": […]}` entry (`bash`,
//! `write_file`, `edit_file`, `request_user_input`, …), and a call answers
//! with the bare name and `"namespace": "muse"`; the tool's result comes
//! back as a `function_call_output` input item of the next request.
//!
//! The stream (`Sse`): every event carries its `sequence_number`; it opens
//! with `response.created` and `response.in_progress`, announces, fills and
//! closes each item (`output_item.added`, `content_part.added`,
//! `output_text.delta` × N, `output_text.done`, `content_part.done`,
//! `output_item.done`; a call's `function_call_arguments.delta` and `.done`),
//! and ends with `response.completed`, which lists the items and the usage.
//!
//! Who asks (the `x-tbh-agent-role` header): `main` is the turn, answered by
//! the lab's modes as a Codex request is (`Stub::responses`, `kind=muse`);
//! `tool` there calls the script's tools ([`tool_turn`]) and answers the last
//! result with `Tool call done. Result: <its output>` ([`last_tool_output`]).
//! Every other role is a SIDE call (`kind=muse-side`), answered in every mode
//! so a wall and a custom mode's `times` are spent on the turn alone
//! ([`side_answer`]): a `reminder:*` agent gets its one strict tool,
//! `submit_reminder_decision` with `decision: "none"`; `prompt-hint` the text
//! `NONE` (it asks for a tip's id or `NONE`); any other role the text `ok`.

use std::io;
use std::time::Duration;

use super::{Dialect, Out, RInfo, Stub, ToolTurn, analyse_responses, auth_label, bump};
use super::{call_input, call_name, closed, est_tokens, item_text, rand_hex, script_calls};
use crate::pyjson::{COMPACT, J};
use crate::util::one_line;

/// The one model the stub's catalog names: the TUI's footer shows it.
pub const MODEL: &str = "lab-stub";

/// The catalog `GET /muse-code/models` answers: an OpenAI-style list of one
/// model whose `metadata` is keyed `muse-code` (Muse Code's own wire shape;
/// its `ModelCatalogResponse` is the cache it keeps, not what it reads).
#[must_use]
pub fn catalog() -> J {
    let strs = |v: &[&str]| J::Arr(v.iter().map(|s| J::s(*s)).collect());
    let meta = J::obj([
        ("family", J::s("lab")),
        ("release_date", J::s("2026-01-01")),
        ("is_hidden", J::Bool(false)),
        ("attachment", J::Bool(false)),
        ("reasoning", J::Bool(true)),
        ("temperature", J::Bool(false)),
        ("tool_call", J::Bool(true)),
        (
            "modalities",
            J::obj([("input", strs(&["text"])), ("output", strs(&["text"]))]),
        ),
        (
            "limit",
            J::obj([("context", J::Int(200_000)), ("output", J::Int(32_000))]),
        ),
        ("roles", J::Arr(Vec::new())),
        ("description", J::s("the aterm lab's stub, not a model")),
    ]);
    J::obj([
        ("object", J::s("list")),
        (
            "data",
            J::Arr(vec![J::obj([
                ("id", J::s(MODEL)),
                ("object", J::s("model")),
                ("created", J::Int(0)),
                ("owned_by", J::s("meta")),
                ("metadata", J::obj([("muse-code", meta)])),
            ])]),
        ),
    ])
}

/// The log's `kind=` of a Muse Code turn, from what [`analyse_responses`]
/// read: `muse`, `muse-json` (a structured side request), or `opaque`.
#[must_use]
pub fn kind_of(info: &RInfo) -> &'static str {
    match info.kind {
        "codex" => "muse",
        "codex-json" => "muse-json",
        other => other,
    }
}

/// The names of the tools a request offers, a namespace's inner tools by
/// their bare names (`bash`, `submit_reminder_decision`).
#[must_use]
pub fn tool_names(body: &[u8]) -> Vec<String> {
    let Ok(j) = J::parse_bytes(body) else {
        return Vec::new();
    };
    let name = |t: &J| t.get("name").and_then(J::as_str).map(str::to_string);
    j.get("tools")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .flat_map(|t| {
            if t.get("type").and_then(J::as_str) == Some("namespace") {
                t.get("tools")
                    .and_then(J::as_arr)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(name)
                    .collect()
            } else {
                name(t).into_iter().collect::<Vec<_>>()
            }
        })
        .collect()
}

/// A person's message: a user item that is no runtime `<system-reminder …>` note.
fn is_person(it: &J) -> bool {
    matches!(it.get("type").and_then(J::as_str), Some("message") | None)
        && it.get("role").and_then(J::as_str) == Some("user")
        && !item_text(it).trim_start().starts_with("<system-reminder")
}

/// A tool's result coming back.
fn is_result(it: &J) -> bool {
    matches!(
        it.get("type").and_then(J::as_str),
        Some("function_call_output" | "custom_tool_call_output")
    )
}

/// What the `tool` mode answers a Muse Code turn: the script's next call,
/// done once the last call's result is in, or `None` when no person's
/// message holding `toolstart` was sent. The step is the number of results
/// after the last such message, read off the request (Muse Code sends the
/// whole conversation each time), so the stub keeps no state. A call with a
/// `when` (a Claude Code subagent's) has no Muse Code counterpart and is skipped.
#[must_use]
pub fn tool_turn(body: &[u8], script: &J) -> Option<ToolTurn> {
    let j = J::parse_bytes(body).ok()?;
    let items = j.get("input").and_then(J::as_arr).unwrap_or_default();
    let calls: Vec<&J> = script_calls(script)
        .into_iter()
        .filter(|c| !c.get("when").is_some_and(J::truthy))
        .collect();
    let mut step: Option<usize> = None;
    for it in items {
        if is_result(it) {
            step = step.map(|s| s + 1);
        } else if is_person(it) && item_text(it).contains("toolstart") {
            step = Some(0);
        }
    }
    let step = step?;
    Some(match calls.get(step) {
        Some(c) => ToolTurn::Call(call_name(c), call_input(c), step),
        None => ToolTurn::Done,
    })
}

/// Whether a Responses request's conversation carries a tool's result back:
/// a result item anywhere in its input — after its last person's message, or
/// before one (the next prompt, a retry's error turn after an empty reply) —
/// so a scripted call is made once a conversation
/// ([`super::scripted_call`]), never again at each retry.
#[must_use]
pub fn results_answered(body: &[u8]) -> bool {
    let Ok(j) = J::parse_bytes(body) else {
        return false;
    };
    j.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .any(is_result)
}

/// The last tool result a request carries, on one line (at most 200
/// characters): what the person's answer to a box came back as.
#[must_use]
pub fn last_tool_output(body: &[u8]) -> Option<String> {
    let j = J::parse_bytes(body).ok()?;
    let items = j.get("input").and_then(J::as_arr)?;
    let it = items.iter().rev().find(|it| is_result(it))?;
    let text = match it.get("output") {
        Some(J::Str(s)) => s.clone(),
        Some(other) => other.dump(COMPACT),
        None => String::new(),
    };
    Some(one_line(&text, 200))
}

/// How a side call is answered ([module docs](self)).
#[derive(Clone, Debug, PartialEq)]
pub enum Side {
    /// A text reply.
    Text(&'static str),
    /// One call of this tool with these arguments.
    Call(&'static str, J),
}

/// The answer to a side call of `role` that offers `tools`.
#[must_use]
pub fn side_answer(role: &str, tools: &[String]) -> Side {
    if role == "prompt-hint" {
        return Side::Text("NONE");
    }
    if tools.iter().any(|t| t == "submit_reminder_decision") {
        return Side::Call(
            "submit_reminder_decision",
            J::obj([
                ("decision", J::s("none")),
                ("advisory_text", J::Null),
                ("confidence", J::Null),
                ("priority", J::Null),
                ("reason", J::s("lab stub: no reminder")),
                ("skill_id", J::Null),
                ("visible_for_steps", J::Null),
            ]),
        );
    }
    Side::Text("ok")
}

/// The `x-tbh-agent-role` of a request, as the log may carry it (letters,
/// digits, `:`, `_`, `-`; at most 40), `main` when it names none.
#[must_use]
pub fn role_of(header: Option<&str>) -> String {
    let r: String = header
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ":_-".contains(*c))
        .take(40)
        .collect();
    if r.is_empty() { "main".into() } else { r }
}

/// One answer in Muse Code's stream ([module docs](self)): the events go out
/// numbered, and the items it sent are listed again by `response.completed`.
struct Sse {
    seq: i64,
    rid: String,
    model: J,
    created: i64,
    output: Vec<J>,
}

impl Sse {
    /// An answer about to be sent for `model`.
    fn new(model: J) -> Sse {
        Sse {
            seq: 0,
            rid: format!("resp_lab{}", rand_hex()),
            model,
            created: crate::util::now_secs(),
            output: Vec::new(),
        }
    }

    fn ev(&mut self, out: &mut Out<'_>, typ: &str, fields: Vec<(&str, J)>) -> io::Result<()> {
        let mut pairs = vec![
            ("type".to_string(), J::s(typ)),
            ("sequence_number".to_string(), J::Int(self.seq)),
        ];
        pairs.extend(fields.into_iter().map(|(k, v)| (k.to_string(), v)));
        self.seq += 1;
        out.sse(typ, &J::Obj(pairs))
    }

    /// The response object as an event carries it.
    fn response(&self, status: &str, usage: Option<(i64, i64)>, error: J) -> J {
        let mut r = J::obj([
            ("id", J::s(self.rid.as_str())),
            ("object", J::s("response")),
            ("created_at", J::Int(self.created)),
            ("status", J::s(status)),
            ("model", self.model.clone()),
            ("output", J::Arr(self.output.clone())),
            ("error", error),
            ("incomplete_details", J::Null),
            ("previous_response_id", J::Null),
        ]);
        if let Some((i, o)) = usage {
            r.set("completed_at", J::Int(crate::util::now_secs()));
            r.set(
                "usage",
                J::obj([
                    ("input_tokens", J::Int(i)),
                    (
                        "input_tokens_details",
                        J::obj([("cached_tokens", J::Int(0))]),
                    ),
                    ("output_tokens", J::Int(o)),
                    (
                        "output_tokens_details",
                        J::obj([("reasoning_tokens", J::Int(0))]),
                    ),
                    ("total_tokens", J::Int(i + o)),
                ]),
            );
        }
        r
    }

    /// `response.created` and `response.in_progress`.
    ///
    /// # Errors
    /// The client went away.
    fn start(&mut self, out: &mut Out<'_>) -> io::Result<()> {
        for t in ["response.created", "response.in_progress"] {
            let r = self.response("in_progress", None, J::Null);
            self.ev(out, t, vec![("response", r)])?;
        }
        Ok(())
    }

    /// One `final_answer` message item streamed as `deltas`, `pace` seconds apart.
    ///
    /// # Errors
    /// The client went away.
    fn text(&mut self, out: &mut Out<'_>, deltas: &[String], pace: f64) -> io::Result<()> {
        let idx = J::Int(i64::try_from(self.output.len()).unwrap_or(0));
        let mid = format!("msg_lab{}", rand_hex());
        let item = |status: &str, content: Vec<J>| {
            J::obj([
                ("type", J::s("message")),
                ("id", J::s(mid.as_str())),
                ("role", J::s("assistant")),
                ("status", J::s(status)),
                ("content", J::Arr(content)),
                ("phase", J::s("final_answer")),
            ])
        };
        let part = |text: &str| {
            J::obj([
                ("type", J::s("output_text")),
                ("text", J::s(text)),
                ("annotations", J::Arr(Vec::new())),
            ])
        };
        let at = |extra: Vec<(&'static str, J)>| {
            let mut v = vec![
                ("item_id", J::s(mid.as_str())),
                ("output_index", idx.clone()),
                ("content_index", J::Int(0)),
            ];
            v.extend(extra);
            v
        };
        self.ev(
            out,
            "response.output_item.added",
            vec![
                ("output_index", idx.clone()),
                ("item", item("in_progress", Vec::new())),
            ],
        )?;
        self.ev(
            out,
            "response.content_part.added",
            at(vec![("part", part(""))]),
        )?;
        for d in deltas {
            if pace > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(pace));
            }
            self.ev(
                out,
                "response.output_text.delta",
                at(vec![("delta", J::s(d.as_str()))]),
            )?;
        }
        let full: String = deltas.concat();
        self.ev(
            out,
            "response.output_text.done",
            at(vec![("text", J::s(full.as_str()))]),
        )?;
        self.ev(
            out,
            "response.content_part.done",
            at(vec![("part", part(&full))]),
        )?;
        let done = item("completed", vec![part(&full)]);
        self.ev(
            out,
            "response.output_item.done",
            vec![("output_index", idx), ("item", done.clone())],
        )?;
        self.output.push(done);
        Ok(())
    }

    /// One `function_call` item: the tool's bare `name` in the `muse` namespace, its arguments `args`.
    ///
    /// # Errors
    /// The client went away.
    fn call(&mut self, out: &mut Out<'_>, name: &str, args: &J) -> io::Result<()> {
        let idx = J::Int(i64::try_from(self.output.len()).unwrap_or(0));
        let fid = format!("fc_lab{}", rand_hex());
        let arguments = args.dump(COMPACT);
        let item = |status: &str, arguments: &str| {
            J::obj([
                ("type", J::s("function_call")),
                ("id", J::s(fid.as_str())),
                ("call_id", J::s(format!("call_lab{}", &fid[6..]))),
                ("name", J::s(name)),
                ("namespace", J::s("muse")),
                ("arguments", J::s(arguments)),
                ("status", J::s(status)),
            ])
        };
        self.ev(
            out,
            "response.output_item.added",
            vec![
                ("output_index", idx.clone()),
                ("item", item("in_progress", "")),
            ],
        )?;
        self.ev(
            out,
            "response.function_call_arguments.delta",
            vec![
                ("item_id", J::s(fid.as_str())),
                ("output_index", idx.clone()),
                ("delta", J::s(arguments.as_str())),
            ],
        )?;
        self.ev(
            out,
            "response.function_call_arguments.done",
            vec![
                ("item_id", J::s(fid.as_str())),
                ("output_index", idx.clone()),
                ("name", J::s(name)),
                ("arguments", J::s(arguments.as_str())),
            ],
        )?;
        let done = item("completed", &arguments);
        self.ev(
            out,
            "response.output_item.done",
            vec![("output_index", idx), ("item", done.clone())],
        )?;
        self.output.push(done);
        Ok(())
    }

    /// `response.completed` with the usage, and the end of the stream.
    ///
    /// # Errors
    /// The client went away.
    fn finish(&mut self, out: &mut Out<'_>, in_tok: i64, out_tok: i64) -> io::Result<()> {
        let r = self.response("completed", Some((in_tok, out_tok)), J::Null);
        self.ev(out, "response.completed", vec![("response", r)])?;
        out.chunk(b"")
    }

    /// `response.failed` with `code`, and the end of the stream.
    ///
    /// # Errors
    /// The client went away.
    fn failed(&mut self, out: &mut Out<'_>, code: &str, message: &str) -> io::Result<()> {
        let err = J::obj([("code", J::s(code)), ("message", J::s(message))]);
        let r = self.response("failed", None, err);
        self.ev(out, "response.failed", vec![("response", r)])?;
        out.chunk(b"")
    }
}

impl Stub {
    /// `POST /responses`: a turn (`x-tbh-agent-role: main`, or none) is
    /// answered by the lab's modes ([`Stub::responses`] in Muse Code's
    /// dialect), any other role as a side call in every mode ([`side_answer`]).
    pub(super) fn muse(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mode: &str,
        req: &super::Request,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let role = role_of(req.header("x-tbh-agent-role"));
        let auth = req.header("authorization");
        if role == "main" {
            return self.responses(method, path, body, mode, auth, Dialect::Muse, out);
        }
        let info = analyse_responses(body);
        bump(&mut self.st().by_kind, "muse-side");
        let tools = tool_names(body);
        let model = match &info.model {
            J::Str(s) => s.clone(),
            other => other.py(),
        };
        self.dump(body);
        let answer = side_answer(&role, &tools);
        let summary = format!(
            "kind=muse-side role={role} auth={} model={model} items={} tools={} last_user=\"{}\" side:{}",
            auth_label(auth),
            info.items,
            tools.len(),
            one_line(&info.last_user, 50),
            match &answer {
                Side::Text(t) => format!("text {t}"),
                Side::Call(n, _) => format!("call {n}"),
            }
        );
        out.stream_head(&[])?;
        let mut s = Sse::new(info.model.clone());
        let sent = (|| -> io::Result<()> {
            s.start(out)?;
            match &answer {
                Side::Text(t) => s.text(out, &[(*t).to_string()], 0.0)?,
                Side::Call(n, args) => s.call(out, n, args)?,
            }
            s.finish(out, est_tokens(body.len()), 1)
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
            body.len(),
            mode,
            &format!("{summary}{note}"),
        );
        Ok(())
    }

    /// `GET /muse-code/models`: the [`catalog`], in every mode.
    pub(super) fn muse_models(
        &self,
        method: &str,
        path: &str,
        mode: &str,
        auth: Option<&str>,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        let st = out.json(200, &catalog(), &[])?;
        let s = format!("kind=muse-models auth={} models=1", auth_label(auth));
        self.record(method, path, st, 0, mode, &s);
        Ok(())
    }

    /// A text reply in Muse Code's stream (the turn's answer, `ok`, `slow`, a custom mode's text).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn muse_text(
        &self,
        (method, path, blen, mode): (&str, &str, usize, &str),
        info: &RInfo,
        summary: &str,
        deltas: &[String],
        pace: f64,
        in_tok: i64,
        extra: &[(String, String)],
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(extra)?;
        let out_tok = i64::try_from(deltas.concat().chars().count() / 4)
            .unwrap_or(0)
            .max(1);
        let mut s = Sse::new(info.model.clone());
        let sent = (|| -> io::Result<()> {
            s.start(out)?;
            s.text(out, deltas, pace)?;
            s.finish(out, in_tok, out_tok)
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

    /// The `tool` mode's call in Muse Code's stream.
    pub(super) fn muse_call(
        &self,
        (method, path, blen, mode): (&str, &str, usize, &str),
        info: &RInfo,
        summary: &str,
        name: &str,
        args: &J,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(&[])?;
        let mut s = Sse::new(info.model.clone());
        let sent = (|| -> io::Result<()> {
            s.start(out)?;
            s.call(out, name, args)?;
            s.finish(out, est_tokens(blen), 30)
        })();
        if let Err(e) = sent
            && !closed(&e)
        {
            return Err(e);
        }
        self.record(method, path, 200, blen, mode, summary);
        Ok(())
    }

    /// A `response.failed` with `code` in Muse Code's stream.
    pub(super) fn muse_failed(
        &self,
        (method, path, blen, mode): (&str, &str, usize, &str),
        summary: &str,
        code: &str,
        message: &str,
        out: &mut Out<'_>,
    ) -> io::Result<()> {
        out.stream_head(&[])?;
        let mut s = Sse::new(J::s(MODEL));
        let sent = (|| -> io::Result<()> {
            s.start(out)?;
            s.failed(out, code, message)
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
}

#[cfg(test)]
#[path = "muse_tests.rs"]
mod tests;
