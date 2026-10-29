// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Write as _;
use std::path::PathBuf;

use super::*;

fn row(kind: &str, payload: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-10-24T12:00:00.000Z\",\"type\":\"{kind}\",\"payload\":{payload}}}\n"
    )
}

fn started(trigger: Option<&str>) -> String {
    let attribution = match trigger {
        Some(t) => format!(",\"turn_attribution\":{{\"turn_id\":\"t\",\"turn_trigger\":\"{t}\"}}"),
        None => String::new(),
    };
    row(
        "event_msg",
        &format!("{{\"type\":\"task_started\",\"turn_id\":\"t\"{attribution}}}"),
    )
}

fn ended() -> String {
    row(
        "event_msg",
        "{\"type\":\"task_complete\",\"turn_id\":\"t\",\"last_agent_message\":null}",
    )
}

fn user(texts: &[&str]) -> String {
    let parts: Vec<String> = texts
        .iter()
        .map(|t| {
            format!(
                "{{\"type\":\"input_text\",\"text\":{}}}",
                aterm_json::to_string(t).unwrap()
            )
        })
        .collect();
    row(
        "response_item",
        &format!(
            "{{\"type\":\"message\",\"role\":\"user\",\"content\":[{}]}}",
            parts.join(",")
        ),
    )
}

fn assistant(text: &str) -> String {
    row(
        "response_item",
        &format!(
            "{{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"output_text\",\"text\":{}}}]}}",
            aterm_json::to_string(text).unwrap()
        ),
    )
}

fn call(kind: &str, name: &str, id: &str) -> String {
    row(
        "response_item",
        &format!(
            "{{\"type\":\"{kind}\",\"name\":\"{name}\",\"arguments\":\"{{}}\",\"call_id\":\"{id}\"}}"
        ),
    )
}

fn output(kind: &str, id: &str) -> String {
    row(
        "response_item",
        &format!("{{\"type\":\"{kind}\",\"call_id\":\"{id}\",\"output\":\"done\"}}"),
    )
}

fn turn_context(model: &str, effort: Option<&str>, cwd: &str) -> String {
    let effort = effort.map_or(String::new(), |e| format!(",\"effort\":\"{e}\""));
    row(
        "turn_context",
        &format!(
            "{{\"turn_id\":\"t\",\"cwd\":\"{cwd}\",\"model\":\"{model}\"{effort},\"collaboration_mode\":{{\"mode\":\"default\",\"settings\":{{\"model\":\"{model}\",\"reasoning_effort\":null}}}}}}"
        ),
    )
}

fn tokens(last_total: u64, window: u64) -> String {
    row(
        "event_msg",
        &format!(
            "{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"total_tokens\":{}}},\"last_token_usage\":{{\"input_tokens\":{last_total},\"total_tokens\":{last_total}}},\"model_context_window\":{window}}},\"rate_limits\":null}}",
            last_total * 3
        ),
    )
}

fn facts_of(rows: &[String]) -> CodexFacts {
    let mut fold = CodexFactsFold::default();
    fold.feed(rows.concat().as_bytes());
    fold.facts()
}

fn prompts(facts: &CodexFacts) -> Vec<&str> {
    facts
        .excerpts
        .prompts
        .iter()
        .map(|p| p.text.as_str())
        .collect()
}

#[test]
fn the_model_effort_and_folder_are_the_newest_turns_or_the_settings_named_since() {
    let mut rows = vec![turn_context("gpt-5.5", Some("high"), "/work/a")];
    let facts = facts_of(&rows);
    assert_eq!(facts.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(facts.effort.as_deref(), Some("high"));
    assert_eq!(facts.cwd, Some(PathBuf::from("/work/a")));

    rows.push(row(
        "event_msg",
        "{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"gpt-6.1-sol\",\"reasoning_effort\":\"low\",\"cwd\":\"/work/b\"}}",
    ));
    let facts = facts_of(&rows);
    assert_eq!(
        facts.model.as_deref(),
        Some("gpt-6.1-sol"),
        "a /model switch"
    );
    assert_eq!(facts.effort.as_deref(), Some("low"));
    assert_eq!(facts.cwd, Some(PathBuf::from("/work/b")));

    rows.push(turn_context("gpt-6.1-sol", None, "/work/b"));
    assert_eq!(
        facts_of(&rows).effort,
        None,
        "a turn that names no effort runs at the model's default"
    );

    rows.push(row(
        "turn_context",
        "{\"turn_id\":\"u\",\"cwd\":\"/work/b\",\"model\":\"gpt-6.1-sol\",\"collaboration_mode\":{\"mode\":\"plan\",\"settings\":{\"model\":\"gpt-6.1-sol\",\"reasoning_effort\":\"medium\"}}}",
    ));
    assert_eq!(
        facts_of(&rows).effort.as_deref(),
        Some("medium"),
        "the collaboration mode's effort"
    );
}

/// Codex's footer: `percent_of_context_window_remaining` over `last_token_usage`, 12000 tokens
/// out of the window and the use, rounded.
#[test]
fn the_context_left_is_counted_as_codexs_footer_counts_it() {
    let pct = |total, window| facts_of(&[tokens(total, window)]).ctx_left_pct;
    assert_eq!(pct(0, 258_400), Some(100));
    assert_eq!(pct(12_000, 258_400), Some(100), "the baseline is no use");
    // (246400 - 123200) / 246400 = 50%.
    assert_eq!(pct(135_200, 258_400), Some(50));
    // (246400 - 246399 + 12000 - 12000) ... 1 of 246400 left rounds to 0.
    assert_eq!(pct(258_399, 258_400), Some(0));
    assert_eq!(pct(400_000, 258_400), Some(0), "past the window");
    assert_eq!(
        pct(5, 12_000),
        Some(0),
        "a window no larger than the baseline"
    );
    // 2/3 left: 66.67 rounds up.
    assert_eq!(pct(12_000 + 82_133, 12_000 + 246_400), Some(67));

    let rate_limits_only = row(
        "event_msg",
        "{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{\"primary\":{\"used_percent\":4.0}}}",
    );
    assert_eq!(
        facts_of(&[tokens(135_200, 258_400), rate_limits_only]).ctx_left_pct,
        Some(50),
        "a count with no usage keeps the last"
    );
}

/// The plan's windows (codex 0.164's `token_count`): each of `primary` and `secondary` names
/// its window by `window_minutes`, 300 the five-hour one and 10080 the week; a plan with only a
/// weekly window names it `primary`; a count whose `rate_limits` is `null` keeps the last.
#[test]
fn the_plans_windows_are_read_by_their_minutes() {
    let limits = |json: &str| {
        row(
            "event_msg",
            &format!("{{\"type\":\"token_count\",\"info\":null,\"rate_limits\":{json}}}"),
        )
    };
    let both = limits(
        "{\"primary\":{\"used_percent\":6.6,\"window_minutes\":300,\"resets_at\":1},\
         \"secondary\":{\"used_percent\":1.0,\"window_minutes\":10080,\"resets_at\":2}}",
    );
    let facts = facts_of(std::slice::from_ref(&both));
    assert_eq!((facts.usage_5h_pct, facts.usage_7d_pct), (Some(7), Some(1)));
    let weekly = limits(
        "{\"limit_id\":\"codex\",\"primary\":{\"used_percent\":1.0,\"window_minutes\":10080,\
         \"resets_at\":1793718245},\"secondary\":null}",
    );
    let facts = facts_of(&[both.clone(), weekly]);
    assert_eq!((facts.usage_5h_pct, facts.usage_7d_pct), (None, Some(1)));
    let facts = facts_of(&[both.clone(), tokens(135_200, 258_400)]);
    assert_eq!(
        (facts.usage_5h_pct, facts.usage_7d_pct),
        (Some(7), Some(1)),
        "a count with no limits keeps the last"
    );
    let bengalfox = limits(
        "{\"limit_id\":\"codex_bengalfox\",\"primary\":{\"used_percent\":3.0,\"window_minutes\":300,\
         \"resets_at\":3},\"secondary\":null}",
    );
    let premium = limits("{\"limit_id\":\"premium\",\"primary\":null,\"secondary\":null}");
    let null_family =
        limits("{\"limit_id\":null,\"primary\":{\"used_percent\":99.0,\"window_minutes\":300}}");
    let facts = facts_of(&[both, bengalfox, premium, null_family]);
    assert_eq!(
        (facts.usage_5h_pct, facts.usage_7d_pct),
        (Some(7), Some(1)),
        "auxiliary and untagged limit_id snapshots never clobber the codex plan windows"
    );
    let near_bounds = limits(
        "{\"limit_id\":\"codex\",\"primary\":{\"used_percent\":12.4,\"window_minutes\":299},\
         \"secondary\":{\"used_percent\":44.6,\"window_minutes\":10079}}",
    );
    let facts = facts_of(&[near_bounds]);
    assert_eq!(
        (facts.usage_5h_pct, facts.usage_7d_pct),
        (Some(12), Some(45)),
        "5% window_minutes tolerance matches codex_limits::Bucket"
    );
    let odd = limits("{\"primary\":{\"used_percent\":140,\"window_minutes\":300}}");
    assert_eq!(
        facts_of(&[odd]).usage_5h_pct,
        None,
        "past 100 is no percent"
    );
}

#[test]
fn a_persons_prompts_are_kept_newest_first_and_codexs_context_is_none_of_them() {
    let facts = facts_of(&[
        started(Some("user")),
        user(&["<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"]),
        user(&["# AGENTS.md instructions for /w\n\nBe brief."]),
        user(&["Fix the flaky test"]),
        ended(),
        started(None),
        user(&[
            "<image name=[Image #1]>",
            "what is in this picture",
            "</image>",
        ]),
        ended(),
        started(Some("queue")),
        user(&["  Fix   the flaky\ntest  "]),
        ended(),
    ]);
    assert_eq!(
        prompts(&facts),
        ["Fix the flaky test", "what is in this picture"],
        "one prompt per words, the newest first; a media label is no word"
    );
}

#[test]
fn the_message_a_starter_began_its_turn_with_is_no_prompt_but_a_steer_in_it_is() {
    let facts = facts_of(&[
        started(Some("user")),
        user(&["Write the parser"]),
        ended(),
        started(Some("alab_autopilot")),
        user(&["<environment_context>\n</environment_context>"]),
        user(&["Autopilot is Cruise: keep going. Finish everything in hand."]),
        user(&["also cover the empty input"]),
        ended(),
        started(Some("goal")),
        user(&["<environment_context>\n</environment_context>"]),
        user(&[
            "<codex_internal_context source=\"goal\">\nContinue working toward the active thread \
             goal.\n</codex_internal_context>",
        ]),
        user(&["stop, fix the failing test first"]),
        ended(),
        started(Some("daemon_recovery")),
        user(&[
            "<codex_internal_context source=\"daemon_recovery\">\nResume.\n</codex_internal_context>",
        ]),
        user(&["and rerun the suite"]),
        ended(),
        started(Some("exec")),
        user(&["port the lexer"]),
        ended(),
    ]);
    assert_eq!(
        prompts(&facts),
        [
            "port the lexer",
            "and rerun the suite",
            "stop, fix the failing test first",
            "also cover the empty input",
            "Write the parser"
        ],
        "codex-alab's opening message is its own, a goal's or a recovery's is codex's context; a \
         person's steer in either and `codex exec`'s prompt are prompts"
    );

    let owed = facts_of(&[
        started(Some("alab_autopilot")),
        ended(),
        user(&["typed between turns"]),
    ]);
    assert_eq!(
        prompts(&owed),
        ["typed between turns"],
        "a turn that ended owes no message"
    );
}

#[test]
fn harness_words_and_eight_prompts_are_the_most_kept() {
    let mut rows = vec![user(&["[aterm harness] keep going"])];
    for n in 0..10 {
        rows.push(user(&[&format!("prompt {n}")]));
    }
    let facts = facts_of(&rows);
    assert_eq!(facts.excerpts.prompts.len(), PROMPTS_KEPT);
    assert_eq!(prompts(&facts)[0], "prompt 9");
    assert!(!prompts(&facts).iter().any(|p| p.contains("harness")));
}

#[test]
fn said_is_the_newest_assistant_text_cut_both_ways() {
    let long = "word ".repeat(200);
    let facts = facts_of(&[assistant("First thoughts."), assistant(&long)]);
    let said = facts.excerpts.said.as_deref().unwrap();
    let said_end = facts.excerpts.said_end.as_deref().unwrap();
    assert!(said.len() <= EXCERPT_BYTES && said.ends_with('…'), "{said}");
    assert!(
        said_end.len() <= EXCERPT_BYTES && said_end.starts_with('…'),
        "{said_end}"
    );
    assert_eq!(
        facts_of(&[assistant("Done.\n\nIt   works.")])
            .excerpts
            .said
            .as_deref(),
        Some("Done. It works.")
    );
}

#[test]
fn doing_is_the_newest_call_no_output_answered_in_the_turn() {
    let mut rows = vec![
        started(Some("user")),
        call("function_call", "exec_command", "c1"),
    ];
    assert_eq!(
        facts_of(&rows).excerpts.doing.as_deref(),
        Some("exec_command")
    );
    rows.push(call("custom_tool_call", "apply_patch", "c2"));
    assert_eq!(
        facts_of(&rows).excerpts.doing.as_deref(),
        Some("apply_patch")
    );
    rows.push(output("custom_tool_call_output", "c2"));
    assert_eq!(
        facts_of(&rows).excerpts.doing.as_deref(),
        Some("exec_command"),
        "the older call still runs"
    );
    rows.push(output("function_call_output", "c1"));
    assert_eq!(facts_of(&rows).excerpts.doing, None);
    rows.push(call("function_call", "exec_command", "c3"));
    rows.push(ended());
    assert_eq!(
        facts_of(&rows).excerpts.doing,
        None,
        "a turn's end leaves nothing in flight"
    );
    rows.push(started(Some("user")));
    rows.push(row(
        "response_item",
        "{\"type\":\"local_shell_call\",\"status\":\"in_progress\",\"action\":{\"type\":\"exec\",\"command\":[\"ls\"]}}",
    ));
    assert_eq!(
        facts_of(&rows).excerpts.doing.as_deref(),
        Some("local_shell")
    );
}

#[test]
fn rows_fed_a_line_at_a_time_fold_as_they_do_whole() {
    let rows = [
        turn_context("gpt-5.5", Some("high"), "/work/a"),
        started(Some("user")),
        user(&["Fix the flaky test"]),
        call("function_call", "exec_command", "c1"),
        "not json at all\n".to_owned(),
        tokens(135_200, 258_400),
        output("function_call_output", "c1"),
        assistant("Fixed it."),
        ended(),
        started(Some("alab_autopilot")),
        user(&["keep going"]),
        call("function_call", "exec_command", "c2"),
    ];
    let whole = facts_of(&rows);
    let mut fold = CodexFactsFold::default();
    for line in &rows {
        fold.feed(line.as_bytes());
    }
    assert_eq!(fold.facts(), whole);
    assert_eq!(whole.excerpts.doing.as_deref(), Some("exec_command"));
    assert_eq!(prompts(&whole), ["Fix the flaky test"]);
    assert_eq!(whole.ctx_left_pct, Some(50));
}

/// A codex-alab rollout the lab recorded (reduced: its strings replaced, its numbers and row
/// order kept).
#[test]
fn a_recorded_codex_alab_rollout_names_its_model_and_context() {
    let recorded = include_str!("testdata/steps/codex-alab-0.164.0-dev.13172-usage.jsonl");
    let facts = facts_of(&[recorded.to_owned()]);
    assert_eq!(facts.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(facts.ctx_left_pct, Some(100));
    assert_eq!(facts.excerpts.doing, None, "every turn ended");
}

fn scratch(name: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("codex-facts-{name}-"))
        .tempdir()
        .expect("temp dir")
}

fn append(path: &std::path::Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .expect("append");
}

#[test]
fn the_tail_folds_whole_lines_as_the_rollout_gains_them() {
    let dir = scratch("gain");
    let path = dir.path().join("rollout.jsonl");
    let mut tail = CodexFactsTail::new(path.clone());
    assert!(!tail.read(), "no rollout yet");

    append(&path, &turn_context("gpt-5.5", None, "/w"));
    assert!(tail.read());
    assert_eq!(tail.facts().model.as_deref(), Some("gpt-5.5"));
    assert!(!tail.read(), "nothing gained");

    let line = assistant("Half written.");
    let (head, rest) = line.split_at(line.len() / 2);
    append(&path, head);
    assert!(!tail.read(), "a line still being written waits");
    assert_eq!(tail.facts().excerpts.said, None);
    append(&path, rest);
    assert!(tail.read());
    assert_eq!(tail.facts().excerpts.said.as_deref(), Some("Half written."));
}

#[test]
fn a_replaced_or_shrunk_rollout_is_read_again_from_its_end() {
    let dir = scratch("replace");
    let path = dir.path().join("rollout.jsonl");
    append(&path, &turn_context("gpt-5.5", None, "/w"));
    append(&path, &assistant("Old words."));
    let mut tail = CodexFactsTail::new(path.clone());
    assert!(tail.read());

    let fresh = dir.path().join("fresh.jsonl");
    append(&fresh, &assistant("New words."));
    std::fs::rename(&fresh, &path).expect("replace");
    assert!(tail.read());
    let facts = tail.facts();
    assert_eq!(facts.excerpts.said.as_deref(), Some("New words."));
    assert_eq!(facts.model, None, "nothing of the old file stays");

    std::fs::write(&path, assistant("Short.")).expect("shrink");
    assert!(tail.read());
    assert_eq!(tail.facts().excerpts.said.as_deref(), Some("Short."));
}

#[test]
fn a_rollout_past_the_window_is_read_from_the_first_whole_line_of_its_end() {
    let dir = scratch("window");
    let path = dir.path().join("rollout.jsonl");
    append(&path, &turn_context("gpt-old", None, "/w"));
    let filler = assistant(&"x".repeat(1000));
    let mut body = String::new();
    while body.len() < usize::try_from(TAIL_BYTES).unwrap() {
        body.push_str(&filler);
    }
    append(&path, &body);
    append(&path, &turn_context("gpt-5.5", None, "/w"));
    let mut tail = CodexFactsTail::new(path.clone());
    assert!(tail.read());
    assert_eq!(
        tail.facts().model.as_deref(),
        Some("gpt-5.5"),
        "the window's newest turn, the oldest out of reach"
    );

    append(&path, &body);
    append(&path, &assistant("After the jump."));
    assert!(
        tail.read(),
        "more than the window gained: from the end again"
    );
    let facts = tail.facts();
    assert_eq!(facts.excerpts.said.as_deref(), Some("After the jump."));
    assert_eq!(facts.model, None, "its turn context is out of reach now");
}
