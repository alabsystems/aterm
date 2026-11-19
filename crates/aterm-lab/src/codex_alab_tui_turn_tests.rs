// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};
use std::fs;

const THREAD: &str = "019a9320-60db-7d90-bb25-59bdb5285b86";
const PROMPT: &str = "prove-one-private-TUI-turn";

fn name() -> String {
    format!("rollout-2026-11-18T11-00-00-{THREAD}.jsonl")
}

fn records(user: Value, end: Value) -> String {
    [
        json!({"type":"session_meta","payload":{"id":THREAD,"session_id":THREAD,"source":"cli"}}),
        frame(json!({"type":"task_started","turn_id":"T"})),
        frame(user),
        frame(end),
    ]
    .map(|row| row.to_string())
    .join("\n")
}

fn frame(event: Value) -> Value {
    json!({"type":"event_msg","payload":event})
}

fn legacy() -> Value {
    json!({"type":"user_message","message":PROMPT})
}

fn answer() -> Value {
    json!({"type":"task_complete","turn_id":"T","last_agent_message":"A private answer"})
}

#[test]
fn withheld_rollout_correlates_both_signed_history_forms_and_terminal_outcomes() {
    let paginated = json!({
        "type":"item_completed", "thread_id":THREAD, "turn_id":"T",
        "item":{"type":"UserMessage","id":"I","content":[{"type":"text","text":PROMPT}]}
    });
    let denied = json!({"type":"task_complete","turn_id":"T","last_agent_message":null,
        "error":{"message":"private provider denied","codex_error_info":{"http_connection_failed":{"http_status_code":401}}}});
    for user in [legacy(), paginated] {
        for (end, is_denied) in [(answer(), false), (denied.clone(), true)] {
            assert_eq!(
                read_turn(&name(), &records(user.clone(), end), PROMPT).unwrap(),
                Some(Turn {
                    thread: THREAD.to_owned(),
                    denied: is_denied,
                    completed: !is_denied,
                })
            );
        }
    }
    let retry = json!({"type":"stream_error","message":"401 Unauthorized"});
    let ongoing = records(legacy(), retry);
    let turn = read_turn(&name(), &ongoing, PROMPT).unwrap().unwrap();
    assert!(!turn.completed && !turn.denied);
    let recovered = format!("{ongoing}\n{}", frame(answer()));
    assert!(
        read_turn(&name(), &recovered, PROMPT)
            .unwrap()
            .unwrap()
            .completed
    );
}

#[test]
fn withheld_rollout_rejects_foreign_prompts_threads_turns_and_ambiguous_completions() {
    let valid = records(legacy(), answer());
    for bad in [
        valid.replace(PROMPT, "not the challenge"),
        valid.replace("\"source\":\"cli\"", "\"source\":\"exec\""),
        format!(
            "{valid}\n{}",
            frame(json!({"type":"task_started","turn_id":"next"}))
        ),
        records(legacy(), legacy()),
        records(legacy(), json!({"type":"task_complete","turn_id":"other"})),
        records(
            legacy(),
            json!({"type":"task_complete","turn_id":"T","error":{"message":"401 in unrelated text","codex_error_info":"other"}}),
        ),
        records(
            legacy(),
            json!({"type":"task_complete","turn_id":"T","last_agent_message":null}),
        ),
        records(
            json!({"type":"item_completed","thread_id":"other","turn_id":"T","item":{"type":"UserMessage","content":[{"type":"text","text":PROMPT}]}}),
            answer(),
        ),
    ] {
        assert!(read_turn(&name(), &bad, PROMPT).is_err());
    }
    assert!(read_turn("rollout-other.jsonl", &valid, PROMPT).is_err());
}

#[test]
fn withheld_rollout_waits_for_complete_records_and_refuses_a_second_file() {
    let home = aterm_tempfile::tempdir().unwrap();
    assert_eq!(rollout(home.path(), PROMPT).unwrap(), None);
    let date = home.path().join("sessions/2026/11/18");
    fs::create_dir_all(&date).unwrap();
    let content = records(legacy(), answer());
    fs::write(date.join(name()), &content).unwrap();
    assert!(!rollout(home.path(), PROMPT).unwrap().unwrap().completed);
    let mut growing = content.into_bytes();
    growing.extend_from_slice(b"\n{\xC3");
    fs::write(date.join(name()), growing).unwrap();
    assert!(rollout(home.path(), PROMPT).unwrap().unwrap().completed);
    fs::write(date.join("rollout-extra.jsonl"), "").unwrap();
    assert!(rollout(home.path(), PROMPT).is_err());
}

#[test]
fn withheld_login_requires_the_signed_option_list_and_continue_action() {
    let menu = "> 1. Sign in with ChatGPT\n  2. Sign in with Device Code\nPress enter to continue";
    assert!(login_screen(menu));
    for item in ["1. ", "Device Code", "Press enter to continue"] {
        assert!(!login_screen(&menu.replace(item, "")));
    }
}

#[test]
fn withheld_composer_waits_for_a_resolved_session_after_startup_and_onboarding() {
    let prompt = "› Ask Codex to do anything\n\n  ? for shortcuts";
    let loading = format!("  >_ OpenAI Codex (v0.11)\n     loading\n\n{prompt}");
    assert!(!composer_ready(&loading));
    assert!(!composer_ready(&format!(
        "{loading}\n  Waiting for startup  · esc cancel"
    )));
    assert!(!composer_ready(prompt));
    assert!(composer_ready(&format!(
        "  >_ OpenAI Codex (v0.11)\n     /tmp/project\n\n{prompt}"
    )));
    let fork = format!(
        "  codex-alab v0.11 · based on OpenAI Codex 0.163\n     gpt-5.5 (high)\n     ~/project\n\n{prompt}"
    );
    assert!(composer_ready(&fork));
    assert!(composer_ready(&format!(
        "{prompt}\n  GPT-5.5 default · /tmp/project"
    )));
    assert!(!composer_ready(&format!(
        "{fork}\n  1. Sign in with ChatGPT\n  2. Sign in with Device Code\n  Press enter to continue"
    )));
    let picker = ">_ OpenAI Codex (v0.11)\n    /tmp/project\nSelect Model and Effort\n\n› 1. gpt-5.5\n\nenter select · esc back";
    assert!(!composer_ready(picker));
}
