// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A CLAUDE CODE SESSION WAS ASKED, SAID AND IS DOING, for the survey (`aterm ctl survey`, `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §3): three excerpts of its transcript's end, read by the footer's own tail read ([`super::footer::TailCache`]) — the fresh read looks back with it, an append scans only what was appended — so the survey asks no file of its own:

use aterm_json::Value;

use crate::supervise::policy::turn_end::without_finished_lines;

/// The most bytes one excerpt keeps (`…` included when it was cut).
pub const EXCERPT_BYTES: usize = 400;

/// How many distinct prompts are kept, newest first ([`Excerpts::prompts`]).
pub const PROMPTS_KEPT: usize = 8;

/// How much of a folded prompt its key reads ([`prompt_key`]): well under the turn ledger's 512-byte text, so a ledger record's truncated text and the transcript's whole row agree on it.
pub const PROMPT_KEY_BYTES: usize = 256;

/// The most tool results one read keeps while no assistant row has decided
/// what is in flight: the newest rows only ever hold a few.
const MAX_RESULTS: usize = 64;

/// What the end of one transcript says, for the survey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Excerpts {
    /// The newest typed prompts, newest first, distinct by key.
    pub prompts: Vec<Prompt>,
    /// The newest text the agent wrote, folded and capped keeping its head.
    pub said: Option<String>,
    /// The same text capped keeping its END ([`cap_end`], led by `…` when cut): what the survey sends for a finished turn, whose words are a report that ends with its result.
    pub said_end: Option<String>,
    /// The tool the newest assistant message called that no result has answered yet (the newest such call), by name.
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

/// `text` cut to at most `cap` bytes on a character boundary, ending in `…` when it was cut — and never inside a word: a cut that falls inside a run of ASCII letters, digits and punctuation, the characters every credential shape is made of, backs up to the run's start, so the survey's redaction (which runs on what the cap kept) judges every token whole, and a secret cut short of the length its shape needs is never quoted in part.
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

/// [`cap`] from the other end: `text`'s last `cap` bytes at most, on a character boundary, led by `…` when it was cut — and never inside a word, by the same rule: a cut that falls inside a run of ASCII letters, digits and punctuation moves on to the run's end, and one inside a fleet join code to the code's end — one on a code's start with punctuation before it as well — so the redaction judges every token and every code whole.
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

/// The key a typed text is matched by, wherever it was recorded: FNV-1a/64 of its words ([`fold`]) — a paste read as what was pasted (`super::upgrade::typed_words`) — up to [`PROMPT_KEY_BYTES`].
#[must_use]
pub fn prompt_key(text: &str) -> u64 {
    let folded = fold(super::upgrade::typed_words(text));
    let mut end = PROMPT_KEY_BYTES.min(folded.len());
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    aterm_hash::fnv1a_64(&folded.as_bytes()[..end])
}

/// What one run of rows says, read newest first: the excerpts it decided, and what an older run must still answer ([`Scan::over`]).
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
    /// `rows`, newest first, as the footer's tail read splits them: whole, UTF-8, none longer than the footer's longest row.
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

    /// Nothing older can change what these rows say: every prompt kept, the newest text met, and what is in flight decided.
    #[must_use]
    pub const fn settled(&self) -> bool {
        self.prompts.len() >= PROMPTS_KEPT
            && self.said.is_some()
            && matches!(
                self.flight,
                Flight::Idle | Flight::Message { open: false, .. }
            )
    }

    /// One row, older than every row met so far.
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

    /// Keep one prompt's words, unless a newer one has its key or enough are
    /// kept.
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

    /// `self` (newer rows) over `older`: what the newer rows decided stands; their tool results answer the older rows' calls; a message whose rows the newer run ended inside goes on into the older run's rows of the same message.
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

/// The call ids a user row's tool results answer; empty for any other row.
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

/// A user row's typed words, when it is a prompt: its text (the whole string,
/// or its text parts joined), a paste read as what was pasted, and none of
/// the rows Claude Code writes itself or the harness's own lines.
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
    let words = super::upgrade::typed_words(&text);
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

/// The newest text part of an assistant row that has words once its done
/// token's lines are dropped, folded: what the result reads a worker's report
/// by, the token alone said by no part (`turn_end::without_finished_lines`).
fn newest_text(parts: &[Value]) -> Option<String> {
    parts
        .iter()
        .rev()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .map(|text| fold(&without_finished_lines(text)))
        .find(|t| !t.is_empty())
}

/// An assistant row's tool calls, newest first: id and name.
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

/// A tool's name as the survey shows it: at most 64 bytes of letters, digits
/// and `_.:-` (an MCP tool's `mcp__server__tool` included); anything else is
/// cut where it stops being one.
fn tool_name(name: &str) -> String {
    name.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
        .take(64)
        .collect()
}

/// The `calls` no result in `results` answers.
fn unanswered(calls: Vec<(String, String)>, results: &[String]) -> Vec<(String, String)> {
    calls
        .into_iter()
        .filter(|(id, _)| id.is_empty() || !results.contains(id))
        .collect()
}

#[cfg(test)]
#[path = "excerpt_tests.rs"]
mod tests;
