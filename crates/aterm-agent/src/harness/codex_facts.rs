// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `codex-alab` rollout says about its session, for `aterm ctl survey`: the prompts,
//! said and doing a Claude Code transcript's [`Excerpts`] give, and the model, reasoning effort,
//! context left and working directory Claude Code's footer shows. [`CodexFactsTail`] reads the
//! rollout's end once and then what it gains, and [`CodexFactsFold`] folds the rows forward.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::excerpt::{
    EXCERPT_BYTES, Excerpts, PROMPTS_KEPT, Prompt, cap, cap_end, clean_prompt_words, fold,
    prompt_key, tool_name,
};
use super::result::{codex_context, codex_media_label};
use crate::supervise::policy::turn_end::without_finished_lines;

/// Tokens codex counts as always in the context (its instructions, tools and room to compact):
/// its own context-left figure takes them out of the window and the use alike
/// (`TokenUsage::percent_of_context_window_remaining`).
const BASELINE_TOKENS: u64 = 12_000;

/// The most one read takes: a first read takes at most this much of the rollout's end, and a
/// rollout that gained more since the last read is read again from its end.
pub const TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The most calls in flight kept ([`CodexFactsFold`]).
const PENDING_KEPT: usize = 64;

/// The `turn_trigger`s of a person's turn: the TUI's own submission, its queue of what the
/// person typed while a turn ran, `codex exec`'s prompt and a voice session's handoff of what the
/// person said. A turn that names no trigger is a person's too; any other (codex-alab's
/// Autopilot, a goal's continuation, a daemon's recovery) is its starter's.
const PERSON_TRIGGERS: [&str; 4] = ["user", "queue", "exec", "realtime"];

/// How codex wraps the context an extension steers a turn with (a goal's continuation, a daemon's
/// recovery): a starter's whole opening message when it is one.
const INTERNAL_CONTEXT: &str = "<codex_internal_context";

/// The rows [`CodexFactsFold`] reads, as their lines spell a type; a line naming none is never
/// parsed.
const MARKS: [&str; 9] = [
    "\"turn_context\"",
    "\"thread_settings_applied\"",
    "\"token_count\"",
    "\"task_started\"",
    "\"task_complete\"",
    "\"turn_aborted\"",
    "\"message\"",
    "_call\"",
    "_call_output\"",
];

/// What a rollout says, for the survey ([`CodexFactsFold::facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexFacts {
    /// A person's prompts (newest first), what the agent last said, and the tool call in
    /// flight, as [`Excerpts`] gives a transcript's.
    pub excerpts: Excerpts,
    /// The model the newest turn ran on, or the one the thread's settings named since.
    pub model: Option<String>,
    /// Its reasoning effort; `None` where neither named one (the model's default).
    pub effort: Option<String>,
    /// How much of the model's context window is left, in percent, as codex's own footer counts
    /// it.
    pub ctx_left_pct: Option<u8>,
    /// The newest turn's working directory.
    pub cwd: Option<PathBuf>,
}

/// A forward fold of a rollout's rows into [`CodexFacts`].
#[derive(Debug, Clone, Default)]
pub struct CodexFactsFold {
    prompts: Vec<Prompt>,
    /// The newest text, cut keeping its head and cut keeping its end.
    said: Option<(String, String)>,
    /// Calls no output answered yet, oldest first: their id and tool.
    pending: Vec<(String, String)>,
    /// The turn in hand is its starter's (no person's), and the message it began with is still
    /// to come.
    starter_owed: bool,
    model: Option<String>,
    effort: Option<String>,
    ctx_left_pct: Option<u8>,
    cwd: Option<PathBuf>,
}

impl CodexFactsFold {
    /// Folds `rows`, whole JSONL lines in the rollout's order; a line that is no row is skipped.
    pub fn feed(&mut self, rows: &[u8]) {
        for line in rows.split(|&b| b == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            if !MARKS.iter().any(|mark| line.contains(mark)) {
                continue;
            }
            if let Ok(row) = aterm_json::from_str::<Value>(line) {
                self.row(&row);
            }
        }
    }

    /// What the rows folded so far say.
    #[must_use]
    pub fn facts(&self) -> CodexFacts {
        let (said, said_end) = self.said.clone().unzip();
        CodexFacts {
            excerpts: Excerpts {
                prompts: self.prompts.clone(),
                said,
                said_end,
                doing: self.pending.last().map(|(_, tool)| tool.clone()),
            },
            model: self.model.clone(),
            effort: self.effort.clone(),
            ctx_left_pct: self.ctx_left_pct,
            cwd: self.cwd.clone(),
        }
    }

    fn row(&mut self, row: &Value) {
        let Some(payload) = row.get("payload") else {
            return;
        };
        match (
            row.get("type").and_then(Value::as_str),
            payload.get("type").and_then(Value::as_str),
        ) {
            (Some("turn_context"), _) => {
                if let Some(model) = word(payload, "model") {
                    self.model = Some(model);
                }
                self.effort = word(payload, "effort").or_else(|| mode_effort(payload));
                if let Some(cwd) = word(payload, "cwd") {
                    self.cwd = Some(PathBuf::from(cwd));
                }
            }
            (Some("event_msg"), Some("thread_settings_applied")) => {
                let Some(settings) = payload.get("thread_settings") else {
                    return;
                };
                if let Some(model) = word(settings, "model") {
                    self.model = Some(model);
                    self.effort =
                        word(settings, "reasoning_effort").or_else(|| mode_effort(settings));
                }
                if let Some(cwd) = word(settings, "cwd") {
                    self.cwd = Some(PathBuf::from(cwd));
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                if let Some(pct) = payload.get("info").and_then(ctx_left_pct) {
                    self.ctx_left_pct = Some(pct);
                }
            }
            (Some("event_msg"), Some("task_started")) => {
                self.pending.clear();
                self.starter_owed = !begun_by_a_person(payload);
            }
            (Some("event_msg"), Some("task_complete" | "turn_aborted")) => {
                self.pending.clear();
                self.starter_owed = false;
            }
            (Some("response_item"), Some("message")) => {
                match payload.get("role").and_then(Value::as_str) {
                    Some("user") => self.user(payload),
                    Some("assistant") => self.assistant(payload),
                    _ => {}
                }
            }
            (
                Some("response_item"),
                Some(kind @ ("function_call" | "custom_tool_call" | "local_shell_call")),
            ) => {
                let tool = if kind == "local_shell_call" {
                    "local_shell".to_owned()
                } else {
                    word(payload, "name")
                        .map(|name| tool_name(&name))
                        .unwrap_or_default()
                };
                if tool.is_empty() {
                    return;
                }
                let id = word(payload, "call_id").unwrap_or_default();
                if !id.is_empty() {
                    self.pending.retain(|(call, _)| *call != id);
                }
                if self.pending.len() >= PENDING_KEPT {
                    self.pending.remove(0);
                }
                self.pending.push((id, tool));
            }
            (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
                if let Some(id) = word(payload, "call_id") {
                    self.pending.retain(|(call, _)| *call != id);
                }
            }
            _ => {}
        }
    }

    /// A user message: context codex adds to a turn is none of the person's words, and the
    /// message a starter (no person) began its turn with is its own; every other is a prompt.
    fn user(&mut self, payload: &Value) {
        let mut parts = texts(payload, "input_text");
        parts.retain(|part| !codex_media_label(part));
        if parts.iter().any(|part| codex_context(part)) {
            if parts
                .iter()
                .any(|part| part.trim_start().starts_with(INTERNAL_CONTEXT))
            {
                self.starter_owed = false;
            }
            return;
        }
        let words = parts.join("\n");
        let words = words.trim();
        if words.is_empty() || std::mem::take(&mut self.starter_owed) {
            return;
        }
        let Some(words) = clean_prompt_words(words) else {
            return;
        };
        let key = prompt_key(&words);
        self.prompts.retain(|prompt| prompt.key != key);
        self.prompts.insert(
            0,
            Prompt {
                text: cap(&fold(&words), EXCERPT_BYTES),
                key,
            },
        );
        self.prompts.truncate(PROMPTS_KEPT);
    }

    fn assistant(&mut self, payload: &Value) {
        let text = fold(&without_finished_lines(
            &texts(payload, "output_text").join("\n"),
        ));
        if !text.is_empty() {
            self.said = Some((cap(&text, EXCERPT_BYTES), cap_end(&text, EXCERPT_BYTES)));
        }
    }
}

/// `key`'s string in `v`, when it is one and not empty.
fn word(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The reasoning effort a collaboration mode's settings name.
fn mode_effort(v: &Value) -> Option<String> {
    v.get("collaboration_mode")
        .and_then(|mode| mode.get("settings"))
        .and_then(|settings| word(settings, "reasoning_effort"))
}

/// The text of a message's `part` items.
fn texts(payload: &Value, part: &str) -> Vec<String> {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some(part))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a `task_started` names a person's turn ([`PERSON_TRIGGERS`]); one with no
/// attribution at all is a person's too.
fn begun_by_a_person(payload: &Value) -> bool {
    match payload
        .get("turn_attribution")
        .and_then(|a| a.get("turn_trigger"))
    {
        None | Some(Value::Null) => true,
        Some(trigger) => trigger
            .as_str()
            .is_some_and(|t| PERSON_TRIGGERS.contains(&t)),
    }
}

/// A `token_count`'s context left, as codex's footer counts it: the newest request's whole
/// count against the window, the [`BASELINE_TOKENS`] out of both, rounded.
fn ctx_left_pct(info: &Value) -> Option<u8> {
    let window = info.get("model_context_window")?.as_u64()?;
    let used = info
        .get("last_token_usage")?
        .get("total_tokens")?
        .as_u64()?;
    if window <= BASELINE_TOKENS {
        return Some(0);
    }
    let effective = u128::from(window - BASELINE_TOKENS);
    let remaining = effective.saturating_sub(u128::from(used.saturating_sub(BASELINE_TOKENS)));
    u8::try_from((remaining * 100 + effective / 2) / effective).ok()
}

/// A rollout read for [`CodexFacts`]: its end first (at most [`TAIL_BYTES`]), then what it
/// gains, whole lines only.
#[derive(Debug)]
pub struct CodexFactsTail {
    path: PathBuf,
    /// The file the last read read (its identity) and where the next starts.
    at: Option<(Option<(u64, u64)>, u64)>,
    fold: CodexFactsFold,
}

impl CodexFactsTail {
    /// A reader of the rollout at `path`, which has read nothing.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            at: None,
            fold: CodexFactsFold::default(),
        }
    }

    /// The rollout it reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Folds the lines the rollout gained since the last read; it starts again from the end
    /// (at most [`TAIL_BYTES`], from the line after the first cut) when the reader is new, or
    /// the file was replaced, shrank or gained more than [`TAIL_BYTES`]. Whether any line was
    /// folded; a rollout that cannot be opened as a regular file reads nothing.
    pub fn read(&mut self) -> bool {
        let Some(mut file) = super::footer::open_regular(&self.path) else {
            return false;
        };
        let Ok(meta) = file.metadata() else {
            return false;
        };
        let id = super::session_usage::open_identity(&file, &meta);
        let len = meta.len();
        let resume = self
            .at
            .filter(|&(was, pos)| was == id && pos <= len && len - pos <= TAIL_BYTES)
            .map(|(_, pos)| pos);
        let from = match resume {
            Some(pos) if pos == len => return false,
            Some(pos) => pos,
            None => {
                self.fold = CodexFactsFold::default();
                len.saturating_sub(TAIL_BYTES)
            }
        };
        let mut buf = Vec::new();
        if file.seek(SeekFrom::Start(from)).is_err()
            || (&mut file).take(len - from).read_to_end(&mut buf).is_err()
        {
            self.at = None;
            return false;
        }
        let start = if resume.is_none() && from > 0 {
            buf.iter()
                .position(|&b| b == b'\n')
                .map_or(buf.len(), |at| at + 1)
        } else {
            0
        };
        let end = buf
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(start, |at| (at + 1).max(start));
        self.at = Some((id, from + end as u64));
        if end == start {
            return false;
        }
        self.fold.feed(&buf[start..end]);
        true
    }

    /// What the lines read so far say.
    #[must_use]
    pub fn facts(&self) -> CodexFacts {
        self.fold.facts()
    }
}

#[cfg(test)]
#[path = "codex_facts_tests.rs"]
mod tests;
