// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Transcript tail excerpts (`prompts`, `said`, `doing`) for `aterm ctl survey`.

use aterm_json::Value;

use crate::supervise::policy::turn_end::without_finished_lines;

/// The most bytes one excerpt keeps (`…` included when it was cut).
pub const EXCERPT_BYTES: usize = 400;

/// How many distinct prompts are kept, newest first ([`Excerpts::prompts`]).
pub const PROMPTS_KEPT: usize = 8;

/// Maximum folded prompt prefix hashed by [`prompt_key`].
pub const PROMPT_KEY_BYTES: usize = 256;

const MAX_RESULTS: usize = 64;

/// What the end of one transcript says, for the survey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Excerpts {
    /// The newest typed prompts, newest first, distinct by key.
    pub prompts: Vec<Prompt>,
    /// The newest text the agent wrote, folded and capped keeping its head.
    pub said: Option<String>,
    /// The newest assistant text capped from the end ([`cap_end`]).
    pub said_end: Option<String>,
    /// Name of the newest unanswered tool call in the latest assistant message.
    pub doing: Option<String>,
}

/// One typed prompt ([`Excerpts::prompts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The prompt's words, folded and capped at [`EXCERPT_BYTES`].
    pub text: String,
    /// [`prompt_key`] of the prompt's whole words.
    pub key: u64,
}

/// `text` as one line: every run of whitespace (newlines included) one space, the ends trimmed.
#[must_use]
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(EXCERPT_BYTES * 2));
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Truncates `text` to at most `cap` bytes with a trailing `…`, backing up out of tokens/codes.
#[must_use]
pub fn cap(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let room = cap.saturating_sub('…'.len_utf8());
    let mut end = room.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let codes = code_spans(text);
    let in_word = |c: char| c.is_ascii_graphic();
    loop {
        let was = end;
        if let Some(code) = codes.iter().find(|code| code.start < end && end < code.end) {
            end = code.start;
        }
        if text[end..].starts_with(in_word) {
            end = text[..end].trim_end_matches(in_word).len();
        }
        if end == was {
            break;
        }
    }
    let mut out = text[..end].trim_end().to_owned();
    out.push('…');
    out
}

/// Truncates `text` to at most `cap` bytes from the end with a leading `…`, skipping whole tokens.
#[must_use]
pub fn cap_end(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let room = cap.saturating_sub('…'.len_utf8());
    let mut start = text.len() - room.min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let codes = code_spans(text);
    let in_word = |c: char| c.is_ascii_graphic();
    loop {
        let was = start;
        if let Some(code) = codes
            .iter()
            .find(|code| code.start < start && start < code.end)
        {
            start = code.end;
        }
        if text[..start].ends_with(in_word) {
            start = text.len() - text[start..].trim_start_matches(in_word).len();
        }
        if start == was {
            break;
        }
    }
    let mut out = String::from('…');
    out.push_str(text[start..].trim_start());
    out
}

/// Where a fleet join code sits in `text`
/// ([`aterm_net::fleet_member::code_spans`]), for [`cap`] and [`cap_end`];
/// none off unix, where no machine joins a fleet.
#[cfg(unix)]
fn code_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    aterm_net::fleet_member::code_spans(text)
}

/// Where a fleet join code sits in `text`, for [`cap`] and [`cap_end`]; none
/// off unix, where no machine joins a fleet.
#[cfg(not(unix))]
fn code_spans(_text: &str) -> Vec<std::ops::Range<usize>> {
    Vec::new()
}

/// Computes the FNV-1a/64 key of `text`'s folded words up to [`PROMPT_KEY_BYTES`].
#[must_use]
pub fn prompt_key(text: &str) -> u64 {
    let folded = fold(super::upgrade::typed_words(text));
    let mut end = PROMPT_KEY_BYTES.min(folded.len());
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    aterm_hash::fnv1a_64(&folded.as_bytes()[..end])
}

/// Incremental reverse scan state over transcript tail rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    prompts: Vec<Prompt>,
    /// The newest text, cut keeping its head and cut keeping its end.
    said: Option<(String, String)>,
    flight: Flight,
}

/// What the rows say is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flight {
    /// No assistant row and no prompt met yet: the calls these tool results
    /// answered (their ids), which an older run's message may have made.
    Open { results: Vec<String> },
    /// The newest assistant message met: its id, its calls no result met
    /// answers (id and name, newest first), the results met before it, and
    /// whether the run ENDED inside its rows (`open`), so an older run's rows
    /// of the same message are its calls too.
    Message {
        id: Option<String>,
        pending: Vec<(String, String)>,
        results: Vec<String>,
        open: bool,
    },
    /// A prompt is newer than any assistant row: nothing is in flight.
    Idle,
}

impl Default for Flight {
    fn default() -> Self {
        Self::Open {
            results: Vec::new(),
        }
    }
}

impl Scan {
    /// Scans `rows` (newest first) until [`Self::settled`] or exhausted.
    #[must_use]
    pub fn of(rows: &[&str]) -> Self {
        let mut scan = Self::default();
        for &line in rows {
            if scan.settled() {
                break;
            }
            if !line.contains("\"user\"") && !line.contains("\"assistant\"") {
                continue;
            }
            let Ok(row) = super::transcript::excerpt(line) else {
                continue;
            };
            scan.meet(&row);
        }
        scan
    }

    /// Returns whether all prompts, latest assistant text, and in-flight tool state are decided.
    #[must_use]
    pub const fn settled(&self) -> bool {
        self.prompts.len() >= PROMPTS_KEPT
            && self.said.is_some()
            && matches!(
                self.flight,
                Flight::Idle | Flight::Message { open: false, .. }
            )
    }

    fn meet(&mut self, row: &Value) {
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let message = row.get("message");
        match row.get("type").and_then(Value::as_str) {
            Some("user") => {
                if row.get("isMeta").and_then(Value::as_bool) == Some(true)
                    || row.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
                    || row.get("queueTranscriptOnly").and_then(Value::as_bool) == Some(true)
                {
                    return;
                }
                let content = message.and_then(|m| m.get("content"));
                let results = tool_results(content);
                if results.is_empty() {
                    if let Some(words) = prompt_words(content) {
                        self.note_prompt(&words);
                        self.flight = match std::mem::take(&mut self.flight) {
                            Flight::Open { .. } => Flight::Idle,
                            Flight::Message {
                                id,
                                pending,
                                results,
                                ..
                            } => Flight::Message {
                                id,
                                pending,
                                results,
                                open: false,
                            },
                            Flight::Idle => Flight::Idle,
                        };
                    }
                    return;
                }
                match &mut self.flight {
                    Flight::Open { results: met } => {
                        for id in results {
                            if met.len() < MAX_RESULTS {
                                met.push(id);
                            }
                        }
                    }
                    Flight::Message { open, .. } => *open = false,
                    Flight::Idle => {}
                }
            }
            Some("assistant") => {
                if row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                    return;
                }
                let Some(message) = message else {
                    return;
                };
                if message
                    .get("model")
                    .and_then(Value::as_str)
                    .is_some_and(|m| !super::upgrade::is_model_id(m))
                {
                    return;
                }
                let parts = message.get("content").and_then(Value::as_array);
                if self.said.is_none()
                    && let Some(text) = parts.map(Vec::as_slice).and_then(newest_text)
                {
                    self.said = Some((cap(&text, EXCERPT_BYTES), cap_end(&text, EXCERPT_BYTES)));
                }
                let id = message.get("id").and_then(Value::as_str).map(str::to_owned);
                let calls = parts.map(Vec::as_slice).map(tool_calls).unwrap_or_default();
                self.flight = match std::mem::take(&mut self.flight) {
                    Flight::Open { results } => Flight::Message {
                        pending: unanswered(calls, &results),
                        id,
                        results,
                        open: true,
                    },
                    Flight::Message {
                        id: newest,
                        mut pending,
                        results,
                        open: true,
                    } if newest.is_some() && newest == id => {
                        pending.extend(unanswered(calls, &results));
                        Flight::Message {
                            id: newest,
                            pending,
                            results,
                            open: true,
                        }
                    }
                    Flight::Message {
                        id,
                        pending,
                        results,
                        ..
                    } => Flight::Message {
                        id,
                        pending,
                        results,
                        open: false,
                    },
                    Flight::Idle => Flight::Idle,
                };
            }
            _ => {}
        }
    }

    fn note_prompt(&mut self, words: &str) {
        if self.prompts.len() >= PROMPTS_KEPT {
            return;
        }
        let key = prompt_key(words);
        if self.prompts.iter().any(|p| p.key == key) {
            return;
        }
        self.prompts.push(Prompt {
            text: cap(&fold(words), EXCERPT_BYTES),
            key,
        });
    }

    /// Merges `self` (newer rows) over `older` scan results.
    #[must_use]
    pub fn over(self, older: Self) -> Self {
        let mut prompts = self.prompts;
        for prompt in older.prompts {
            if prompts.len() >= PROMPTS_KEPT {
                break;
            }
            if !prompts.iter().any(|p| p.key == prompt.key) {
                prompts.push(prompt);
            }
        }
        let flight = match (self.flight, older.flight) {
            (Flight::Open { mut results }, Flight::Open { results: more }) => {
                results.extend(more);
                results.truncate(MAX_RESULTS);
                Flight::Open { results }
            }
            (
                Flight::Open { mut results },
                Flight::Message {
                    id,
                    mut pending,
                    results: more,
                    open,
                },
            ) => {
                pending.retain(|(call, _)| !results.contains(call));
                results.extend(more);
                results.truncate(MAX_RESULTS);
                Flight::Message {
                    id,
                    pending,
                    results,
                    open,
                }
            }
            (Flight::Open { .. }, Flight::Idle) => Flight::Idle,
            (
                Flight::Message {
                    id,
                    mut pending,
                    results,
                    open: true,
                },
                Flight::Message {
                    id: older_id,
                    pending: older_pending,
                    results: more,
                    open: older_open,
                },
            ) if id.is_some() && id == older_id => {
                pending.extend(
                    older_pending
                        .into_iter()
                        .filter(|(call, _)| !results.contains(call)),
                );
                let mut results = results;
                results.extend(more);
                results.truncate(MAX_RESULTS);
                Flight::Message {
                    id,
                    pending,
                    results,
                    open: older_open,
                }
            }
            (newer @ Flight::Message { open: true, .. }, Flight::Open { results })
                if results.is_empty() =>
            {
                newer
            }
            (
                Flight::Message {
                    id,
                    pending,
                    results,
                    ..
                },
                _,
            ) => Flight::Message {
                id,
                pending,
                results,
                open: false,
            },
            (Flight::Idle, _) => Flight::Idle,
        };
        Self {
            prompts,
            said: self.said.or(older.said),
            flight,
        }
    }

    /// The excerpts these rows decided; what they did not is `None`.
    #[must_use]
    pub fn excerpts(&self) -> Excerpts {
        let doing = match &self.flight {
            Flight::Message { pending, .. } => pending.first().map(|(_, name)| name.clone()),
            Flight::Open { .. } | Flight::Idle => None,
        };
        let (said, said_end) = self.said.clone().unzip();
        Excerpts {
            prompts: self.prompts.clone(),
            said,
            said_end,
            doing,
        }
    }
}

fn tool_results(content: Option<&Value>) -> Vec<String> {
    content
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|p| {
                    p.get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn prompt_words(content: Option<&Value>) -> Option<String> {
    let text = match content? {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    clean_prompt_words(&text)
}

pub(crate) fn clean_prompt_words(text: &str) -> Option<String> {
    let words = super::upgrade::typed_words(text);
    if words.is_empty()
        || words.starts_with("[aterm harness]")
        || words.starts_with("[Request interrupted")
    {
        return None;
    }
    if let Some(command) = words.strip_prefix("<bash-input>") {
        let command = command.split("</bash-input>").next().unwrap_or(command);
        return Some(format!("! {}", command.trim()));
    }
    (!words.starts_with('<')).then(|| words.to_owned())
}

fn agy_user_input_words(content: &str) -> Option<String> {
    let body = content
        .split_once("<USER_REQUEST>")
        .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>").map(|(inner, _)| inner))
        .unwrap_or(content);
    clean_prompt_words(body)
}

fn push_prompt(prompts: &mut Vec<Prompt>, words: &str) {
    if prompts.len() >= PROMPTS_KEPT {
        return;
    }
    let key = prompt_key(words);
    if prompts.iter().any(|p| p.key == key) {
        return;
    }
    prompts.push(Prompt {
        text: cap(&fold(words), EXCERPT_BYTES),
        key,
    });
}

/// Extracts [`Excerpts`] (`prompts`, `said`, `said_end`, `doing`) from the tail
/// of an `agy` conversation's `transcript.jsonl` and `<config>/history.jsonl`.
#[must_use]
pub fn agy_excerpts_from(
    transcript_tail: &[u8],
    history_tail: Option<&[u8]>,
    conversation: &str,
) -> Excerpts {
    let mut prompts: Vec<Prompt> = Vec::new();
    let mut said: Option<(String, String)> = None;
    let mut doing: Option<String> = None;
    let mut saw_planner = false;
    let mut saw_user = false;
    let mut generic_since_planner = 0usize;

    let transcript_text = String::from_utf8_lossy(transcript_tail);
    for line in transcript_text.lines().rev() {
        let Ok(row) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        match row.get("type").and_then(Value::as_str) {
            Some("PLANNER_RESPONSE") => {
                if said.is_none()
                    && let Some(content) = row.get("content").and_then(Value::as_str)
                {
                    let text = fold(&without_finished_lines(content));
                    if !text.is_empty() {
                        said = Some((cap(&text, EXCERPT_BYTES), cap_end(&text, EXCERPT_BYTES)));
                    }
                }
                if !saw_planner && !saw_user {
                    if let Some(calls) = row.get("tool_calls").and_then(Value::as_array)
                        && let Some(call) = calls.get(generic_since_planner)
                        && let Some(name) = call.get("name").and_then(Value::as_str)
                    {
                        let tn = tool_name(name.strip_prefix("default_api:").unwrap_or(name));
                        if !tn.is_empty() {
                            doing = Some(tn);
                        }
                    }
                    saw_planner = true;
                }
            }
            Some("GENERIC") => {
                if !saw_planner
                    && !saw_user
                    && row.get("status").and_then(Value::as_str) != Some("RUNNING")
                {
                    generic_since_planner = generic_since_planner.saturating_add(1);
                }
            }
            Some("USER_INPUT") => {
                saw_user = true;
                if prompts.len() < PROMPTS_KEPT
                    && let Some(content) = row.get("content").and_then(Value::as_str)
                    && let Some(words) = agy_user_input_words(content)
                {
                    push_prompt(&mut prompts, &words);
                }
            }
            _ => {}
        }
    }

    if prompts.len() < PROMPTS_KEPT
        && let Some(history_bytes) = history_tail
    {
        let history_text = String::from_utf8_lossy(history_bytes);
        for line in history_text.lines().rev() {
            if prompts.len() >= PROMPTS_KEPT {
                break;
            }
            let Ok(row) = aterm_json::from_str::<Value>(line) else {
                continue;
            };
            if row.get("conversationId").and_then(Value::as_str) != Some(conversation) {
                continue;
            }
            if let Some(display) = row.get("display").and_then(Value::as_str)
                && let Some(words) = clean_prompt_words(display)
            {
                push_prompt(&mut prompts, &words);
            }
        }
    }

    let (said, said_end) = said.unzip();
    Excerpts {
        prompts,
        said,
        said_end,
        doing,
    }
}

fn muse_records_in_line(line: &str) -> Vec<Value> {
    let Ok(v) = aterm_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    match v.get("children").and_then(Value::as_array) {
        Some(children) if v.get("retained_frame").is_some() => children
            .iter()
            .filter_map(|c| c.get("record_json")?.as_str())
            .filter_map(|r| aterm_json::from_str(r).ok())
            .collect(),
        _ => vec![v],
    }
}

/// Extracts [`Excerpts`] (`prompts`, `said`, `said_end`, `doing`) from the tail
/// of a Muse Code session's `session.jsonl`.
#[must_use]
pub fn muse_excerpts_from(session_tail: &[u8]) -> Excerpts {
    let mut prompts: Vec<Prompt> = Vec::new();
    let mut said: Option<(String, String)> = None;
    let mut open_tasks: Vec<(String, String)> = Vec::new();

    let text = String::from_utf8_lossy(session_tail);
    for line in text.lines() {
        for r in muse_records_in_line(line) {
            let Some(payload_type) = r.get("payload_type").and_then(Value::as_str) else {
                continue;
            };
            let Some(payload) = r.get("payload") else {
                continue;
            };
            let is_task = match payload_type {
                "runtime.session" => payload.get("kind").and_then(Value::as_str) == Some("task"),
                "runtime.session.task" => true,
                _ => false,
            };
            if is_task {
                if let Some(event) = payload.get("event") {
                    let task_id = payload
                        .get("task_id")
                        .and_then(Value::as_str)
                        .or_else(|| event.get("task_id").and_then(Value::as_str))
                        .unwrap_or_default();
                    match event.get("kind").and_then(Value::as_str) {
                        Some("proposed") => {
                            if let Some(tk) = event.get("task_kind").and_then(Value::as_str)
                                && tk != "session_name.allocate"
                            {
                                let tn = tool_name(tk.strip_prefix("tool.").unwrap_or(tk));
                                if !tn.is_empty() {
                                    if !task_id.is_empty() {
                                        open_tasks.retain(|(id, _)| id != task_id);
                                    }
                                    open_tasks.push((task_id.to_owned(), tn));
                                }
                            }
                        }
                        Some("completed" | "cancelled" | "failed" | "rejected") => {
                            if task_id.is_empty() {
                                open_tasks.clear();
                            } else {
                                open_tasks.retain(|(id, _)| id != task_id);
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if payload_type == "runtime.session"
                && payload.get("kind").and_then(Value::as_str) == Some("run")
                && let Some(event) = payload.get("event")
            {
                match event.get("kind").and_then(Value::as_str) {
                    Some("started") => {
                        open_tasks.clear();
                        if let Some(prompt) = event.get("prompt").and_then(Value::as_str)
                            && let Some(words) = clean_prompt_words(prompt)
                        {
                            let key = prompt_key(&words);
                            prompts.retain(|p| p.key != key);
                            prompts.insert(
                                0,
                                Prompt {
                                    text: cap(&fold(&words), EXCERPT_BYTES),
                                    key,
                                },
                            );
                            prompts.truncate(PROMPTS_KEPT);
                        }
                    }
                    Some("assistant_message_committed") => {
                        if let Some(msg) = event.get("text").and_then(Value::as_str) {
                            let clean = fold(&without_finished_lines(msg));
                            if !clean.is_empty() {
                                said = Some((
                                    cap(&clean, EXCERPT_BYTES),
                                    cap_end(&clean, EXCERPT_BYTES),
                                ));
                            }
                        }
                    }
                    Some("terminal") => {
                        open_tasks.clear();
                    }
                    _ => {}
                }
            }
        }
    }

    let (said, said_end) = said.unzip();
    let doing = open_tasks.pop().map(|(_, tn)| tn);
    Excerpts {
        prompts,
        said,
        said_end,
        doing,
    }
}

fn newest_text(parts: &[Value]) -> Option<String> {
    parts
        .iter()
        .rev()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .map(|text| fold(&without_finished_lines(text)))
        .find(|t| !t.is_empty())
}

fn tool_calls(parts: &[Value]) -> Vec<(String, String)> {
    parts
        .iter()
        .rev()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|p| {
            let name = p.get("name").and_then(Value::as_str)?;
            let id = p.get("id").and_then(Value::as_str).unwrap_or_default();
            Some((id.to_owned(), tool_name(name)))
        })
        .collect()
}

pub(crate) fn tool_name(name: &str) -> String {
    name.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
        .take(64)
        .collect()
}

fn unanswered(calls: Vec<(String, String)>, results: &[String]) -> Vec<(String, String)> {
    calls
        .into_iter()
        .filter(|(id, _)| id.is_empty() || !results.contains(id))
        .collect()
}

#[cfg(test)]
#[path = "excerpt_tests.rs"]
mod tests;
