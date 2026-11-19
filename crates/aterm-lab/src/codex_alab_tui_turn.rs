// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Correlate the private TUI's original thread, submitted prompt and terminal turn event.

use std::path::Path;

use aterm_agent::harness::codex_rollout::{is_rollout_of, is_thread_id, rollouts_by_write_sized};

use crate::pyjson::J;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Turn {
    pub(super) thread: String,
    pub(super) denied: bool,
    pub(super) completed: bool,
}

/// Read the complete records of the one fresh rollout in this run's private CODEX_HOME.
pub(super) fn rollout(home: &Path, prompt: &str) -> Result<Option<Turn>, String> {
    let (files, complete) = rollouts_by_write_sized(home);
    if !complete {
        return Err("incomplete private rollout listing".into());
    }
    let [(path, _, _)] = files.as_slice() else {
        return if files.is_empty() {
            Ok(None)
        } else {
            Err("multiple rollouts in the fresh TUI home".into())
        };
    };
    let bytes = crate::util::read_artifact(path).map_err(|e| e.to_string())?;
    let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Ok(None);
    };
    // The writer may still be appending a UTF-8 codepoint or JSON record.
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| "invalid private rollout UTF-8")?;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    read_turn(name, text, prompt)
}

fn string<'a>(row: &'a J, key: &str) -> Option<&'a str> {
    row.get(key).and_then(J::as_str)
}

fn unauthorized(error: &J) -> bool {
    let Some(info) = error.get("codex_error_info") else {
        return false;
    };
    info.as_str() == Some("unauthorized")
        || [
            "http_connection_failed",
            "response_stream_connection_failed",
            "response_stream_disconnected",
            "response_too_many_failed_attempts",
        ]
        .iter()
        .any(|kind| {
            info.get(kind)
                .and_then(|reason| reason.get("http_status_code"))
                .and_then(J::as_i64)
                == Some(401)
        })
}

fn read_turn(name: &str, text: &str, prompt: &str) -> Result<Option<Turn>, String> {
    let mut rows = text.lines();
    let row = J::parse(rows.next().ok_or("empty rollout record")?)
        .map_err(|_| "invalid session metadata JSON")?;
    let meta = row.get("payload").ok_or("missing session metadata")?;
    let thread = string(meta, "id").ok_or("missing original thread")?;
    if string(&row, "type") != Some("session_meta")
        || string(meta, "source") != Some("cli")
        || string(meta, "session_id") != Some(thread)
        || string(meta, "parent_thread_id").is_some()
        || string(meta, "forked_from_id").is_some()
        || !is_thread_id(thread)
        || !is_rollout_of(name, thread)
    {
        return Err("rollout does not identify the fresh interactive root".into());
    }
    let mut turn_id = None;
    let mut matched = false;
    let mut closed = false;
    let mut denied = false;
    for line in rows {
        let row = J::parse(line).map_err(|_| "invalid completed rollout record")?;
        match string(&row, "type") {
            Some("session_meta") => return Err("duplicate session metadata".into()),
            Some("event_msg") => {}
            _ => continue,
        }
        let event = row.get("payload").ok_or("missing rollout event")?;
        let message = match string(event, "type") {
            Some("task_started" | "turn_started") => {
                let id = string(event, "turn_id")
                    .filter(|id| !id.is_empty())
                    .ok_or("missing turn identity")?;
                if turn_id.replace(id.to_owned()).is_some() {
                    return Err("multiple turns in the fresh TUI thread".into());
                }
                continue;
            }
            Some("user_message") => string(event, "message"),
            Some("item_completed") => {
                let Some(item) = event.get("item") else {
                    return Err("missing completed item".into());
                };
                if string(item, "type") != Some("UserMessage") {
                    continue;
                }
                if string(event, "thread_id") != Some(thread)
                    || string(event, "turn_id") != turn_id.as_deref()
                {
                    return Err("user item belongs to a different thread or turn".into());
                }
                let Some([input]) = item.get("content").and_then(J::as_arr) else {
                    return Err("expected one text input on the challenge turn".into());
                };
                if string(input, "type") != Some("text") {
                    return Err("challenge input was not text".into());
                }
                string(input, "text")
            }
            Some("task_complete" | "turn_complete") => {
                if !matched || closed || string(event, "turn_id") != turn_id.as_deref() {
                    return Err("uncorrelated TUI turn closure".into());
                }
                closed = true;
                if let Some(error) = event.get("error").filter(|e| **e != J::Null) {
                    if !unauthorized(error) {
                        return Err("TUI turn ended with a different or unclassified error".into());
                    }
                    denied = true;
                } else if string(event, "last_agent_message").is_none_or(|s| s.trim().is_empty()) {
                    return Err(
                        "TUI turn closed without an assistant answer or classified denial".into(),
                    );
                }
                continue;
            }
            Some("turn_aborted") => return Err("TUI turn aborted".into()),
            // Warnings, retries and live error notices are not a terminal provider verdict.
            _ => continue,
        };
        if turn_id.is_none() || closed || matched || message != Some(prompt) {
            return Err("unexpected user message in the private TUI turn".into());
        }
        matched = true;
    }
    Ok(matched.then(|| Turn {
        thread: thread.to_owned(),
        denied,
        completed: closed && !denied,
    }))
}

/// Use the actual options and the action prompt shared by 0.11's full and compact menus.
pub(super) fn login_screen(text: &str) -> bool {
    text.contains("1. Sign in with ChatGPT")
        && text.contains("2. Sign in with Device Code")
        && text.contains("Press enter to continue")
}

/// The startup draft already draws the real placeholder and shortcut hint. The initialized
/// session header replaces its loading label with the resolved directory, optionally preceded
/// by the configured model. The footer can remain visible when the banner scrolls offscreen.
pub(super) fn composer_ready(text: &str) -> bool {
    if crate::screen::composer_row("codex", text).as_deref() != Some("Ask Codex to do anything")
        || login_screen(text)
        || text.lines().any(|row| {
            let row = row.trim();
            row == "loading" || row.starts_with("Waiting for startup")
        })
    {
        return false;
    }
    let directory = |s: &str| s == "~" || s.starts_with("~/") || s.starts_with('/');
    let mut rows = text.lines().map(str::trim);
    while let Some(row) = rows.next() {
        if (row.starts_with(">_ OpenAI Codex (v") || row.starts_with("codex-alab v"))
            && rows.clone().take(2).any(directory)
        {
            return true;
        }
        // A fresh session's model and working directory share this status row.
        if (row.starts_with("GPT-") || row.starts_with("gpt-"))
            && row.split(" · ").skip(1).any(directory)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "codex_alab_tui_turn_tests.rs"]
mod tests;
