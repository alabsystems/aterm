// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for worker result extraction and transcript fallback.

use super::*;
use crate::supervise::ScratchDir;
use crate::supervise::policy::turn_end::{
    DONE_CHECK, DONE_CHECK_MARKER_BEFORE, LAND, LAND_ANSWER, LAND_NUDGE, RETIRED_LAND_NUDGES,
    RETIRED_LANDS, done_check_text, done_check_words,
};

const SESSION: &str = "0b2299fa-2e8d-447e-a9b8-082171298ccd";

fn js(t: &str) -> String {
    aterm_json::to_string(&Value::String(t.to_string())).expect("json")
}

fn user(uuid: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":null,"isSidechain":false,"promptId":"7a223c05","type":"user","message":{{"role":"user","content":{}}},"uuid":"{uuid}","timestamp":"2026-10-15T17:28:25.422Z","permissionMode":"auto","origin":{{"kind":"human"}},"promptSource":"typed","userType":"external","entrypoint":"cli","cwd":"/private/tmp/at.0000/proj","sessionId":"{SESSION}","version":"2.1.299","gitBranch":"HEAD"}}"#,
        js(text)
    )
}

fn tool_result(uuid: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_1","type":"tool_result","content":"ATERM-TASK-DONE\n12 passed","is_error":false}}]}},"uuid":"{uuid}","timestamp":"2026-10-15T17:28:26.000Z","toolUseResult":{{"stdout":"12 passed"}},"sessionId":"{SESSION}","version":"2.1.299"}}"#
    )
}

fn said(uuid: &str, id: &str, blocks: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"message":{{"id":"{id}","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":{blocks},"stop_reason":"end_turn","usage":{{"input_tokens":22682,"output_tokens":12}}}},"requestId":"req_lab","type":"assistant","uuid":"{uuid}","timestamp":"2026-10-15T17:28:27.463Z","effort":"medium","sessionId":"{SESSION}","version":"2.1.299","gitBranch":"HEAD"}}"#
    )
}

fn text(t: &str) -> String {
    format!(r#"{{"type":"text","text":{}}}"#, js(t))
}

fn tool_use() -> String {
    r#"{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo ATERM-TASK-DONE"}}"#
        .to_string()
}

fn says(uuid: &str, id: &str, t: &str) -> String {
    said(uuid, id, &format!("[{}]", text(t)))
}

fn turn_duration() -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":54,"messageCount":12,"timestamp":"2026-10-15T17:28:28.000Z","uuid":"td","isMeta":false,"sessionId":"{SESSION}","version":"2.1.299"}}"#
    )
}

fn jsonl(rows: &[String]) -> String {
    let mut out = rows.join("\n");
    out.push('\n');
    out
}

fn worked(report_rows: &[String]) -> Vec<String> {
    let mut rows = vec![
        user("u-task", "Fix the parser and run the suite."),
        said(
            "a-call",
            "msg_1",
            &format!("[{},{}]", text("Running the suite."), tool_use()),
        ),
        tool_result("u-result"),
    ];
    rows.extend_from_slice(report_rows);
    rows.push(turn_duration());
    rows
}

#[test]
fn a_plain_turn_is_its_final_message_no_token_required() {
    for marker in [
        "",
        "ATERM-TASK-DONE",
        "**ATERM-TASK-DONE**",
        "`ATERM-TASK-DONE`",
    ] {
        let rows = worked(&[says(
            "a-report",
            "msg_2",
            &format!("Fixed the parser.\n\nAll 12 tests pass.\n\n{marker}\n"),
        )]);
        assert_eq!(
            report(&jsonl(&rows), DoneBy::Turn),
            Ok(Report {
                text: "Fixed the parser.\n\nAll 12 tests pass.".to_string(),
                session: SESSION.to_string(),
                uuid: "a-report".to_string(),
            }),
            "{marker}"
        );
    }
    let rows = [
        user("u-task", "Fix the parser and run the suite."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("no-reply"));
}

#[test]
fn a_token_alone_after_the_report_gives_the_report() {
    let rows = worked(&[
        said(
            "a-report",
            "msg_2",
            &format!(
                "[{},{}]",
                text("Fixed the parser; 12 tests pass."),
                tool_use()
            ),
        ),
        tool_result("u-verify"),
        says("a-marker", "msg_3", "ATERM-TASK-DONE"),
    ]);
    let got = report(&jsonl(&rows), DoneBy::Turn).expect("a report");
    assert_eq!(got.text, "Fixed the parser; 12 tests pass.");
    assert_eq!(got.uuid, "a-report");
    let rows = [
        user("u-task", "Fix it."),
        says("a-marker", "msg_1", "ATERM-TASK-DONE"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("empty"));
}

fn checked(reply: &[String]) -> Vec<String> {
    let mut rows = vec![
        user("u-task", "Fix the parser and run the suite."),
        says("a-progress", "msg_1", "Half of it is in."),
        turn_duration(),
        user("u-keep", "keep going"),
        says("a-call", "msg_2", "Running the suite."),
        tool_result("u-result"),
        says("a-report", "msg_3", "The parser is fixed; all done."),
        turn_duration(),
        user("u-check", DONE_CHECK),
    ];
    rows.extend_from_slice(reply);
    rows.push(turn_duration());
    rows
}

#[test]
fn a_done_check_is_the_report_then_what_its_reply_adds() {
    let verified = "Verified: the suite is green and the parser handles the empty case.";
    let fixed = "Found 3 regressions and fixed them; the suite is green.";
    let committed = "Committed 4f2a9c1: the parser fix and its 12 tests, all green.";
    for (reply, words, row) in [
        (
            vec![says(
                "a-reply",
                "msg_4",
                &format!("{verified}\n\nATERM-TASK-DONE"),
            )],
            Some(verified),
            "a-reply",
        ),
        (
            vec![
                said(
                    "a-again",
                    "msg_4",
                    &format!("[{},{}]", text("Running the suite again."), tool_use()),
                ),
                tool_result("u-again"),
                says(
                    "a-fixed",
                    "msg_5",
                    &format!("{fixed}\n\n**ATERM-TASK-DONE**"),
                ),
            ],
            Some(fixed),
            "a-fixed",
        ),
        (
            vec![says("a-reply", "msg_4", "ATERM-TASK-DONE")],
            None,
            "a-report",
        ),
        (vec![says("a-reply", "msg_4", "DONE")], None, "a-report"),
        (
            vec![
                said(
                    "a-commit",
                    "msg_4",
                    &format!("[{},{}]", text(committed), tool_use()),
                ),
                tool_result("u-commit"),
                says("a-reply", "msg_5", "ATERM-TASK-DONE"),
            ],
            Some(committed),
            "a-commit",
        ),
    ] {
        let got = report(&jsonl(&checked(&reply)), DoneBy::Check).expect("a report");
        let want = words.map_or_else(
            || "The parser is fixed; all done.".to_string(),
            |w| format!("The parser is fixed; all done.\n\n{w}"),
        );
        assert_eq!(
            (got.text.as_str(), got.uuid.as_str()),
            (want.as_str(), row),
            "{words:?}"
        );
    }
    let rows = [
        user("u-task", "Fix it."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
        user("u-check", DONE_CHECK),
        says("a-reply", "msg_2", "ATERM-TASK-DONE"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-report"));
    let rows = [
        user("u-task", "Fix it."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
        user("u-check", DONE_CHECK),
        says("a-reply", "msg_2", "Fixed it.\nATERM-TASK-DONE"),
    ];
    assert_eq!(
        report(&jsonl(&rows), DoneBy::Check).map(|r| (r.text, r.uuid)),
        Ok(("Fixed it.".to_string(), "a-reply".to_string()))
    );
}

#[test]
fn a_check_reply_written_late_is_read_again_by_the_checks_rule() {
    let preamble = || {
        vec![
            said(
                "a-again",
                "msg_4",
                &format!("[{},{}]", text("Running the suite again."), tool_use()),
            ),
            tool_result("u-again"),
        ]
    };
    for (i, written) in [Vec::new(), preamble()].into_iter().enumerate() {
        let tasks = ScratchDir::new("result", &format!("check-lag-{i}"));
        let transcript = tasks.join("t.jsonl");
        let mut first = checked(&written);
        first.pop();
        let first = jsonl(&first);
        let snap =
            snapshot(&tasks, "s-1-1", Some((&transcript, &first)), DoneBy::Check).expect("written");
        assert_eq!(
            (snap.unavailable, snap.by, snap.bytes),
            (Some("no-reply"), DoneBy::Check, 0),
            "{i}"
        );
        std::fs::write(&transcript, &first).expect("transcript");
        assert_eq!(reread(&snap.path), None, "{i}");
        let mut late = written;
        late.push(says(
            "a-reply",
            "msg_5",
            "Verified: green.\nATERM-TASK-DONE",
        ));
        std::fs::write(&transcript, jsonl(&checked(&late))).expect("transcript");
        let again = reread(&snap.path).expect("read again");
        assert_eq!(
            (again.report.text.as_str(), again.report.uuid.as_str()),
            (
                "The parser is fixed; all done.\n\nVerified: green.",
                "a-reply"
            ),
            "{i}"
        );
    }
}

#[test]
fn a_done_check_with_the_standing_rules_is_the_check() {
    let completion = "All work is complete. All parser tests passed.";
    for rules in [None, Some("leave unrelated files alone")] {
        for irreversible in [false, true] {
            let current = done_check_text(rules, irreversible);
            for check in [
                current.clone(),
                current.replacen(&done_check_words(0), DONE_CHECK_MARKER_BEFORE, 1),
            ] {
                let mut rows = [
                    user("u-task", "Fix the parser."),
                    says("a-report", "msg_1", "The parser is fixed."),
                    user("u-check", &check),
                    says("a-reply", "msg_2", completion),
                ];
                for by in [DoneBy::Check, DoneBy::Turn] {
                    let got = report(&jsonl(&rows), by).expect("the emitted check's report");
                    assert_eq!(got.text, format!("The parser is fixed.\n\n{completion}"));
                    assert_eq!(got.uuid, "a-reply");
                }
                if rules.is_none() {
                    let near = if check == current {
                        format!("Please read this: {check}")
                    } else {
                        format!("{check} This is another request.")
                    };
                    rows[2] = user("u-check", &near);
                    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-check"));
                }
            }
        }
    }

    let check = done_check_text(Some("never push to main"), false);
    assert_ne!(check, DONE_CHECK);
    for (reply, want) in [
        ("ATERM-TASK-DONE", ("The parser is fixed.", "a-report")),
        (
            "Verified against the rules.\nATERM-TASK-DONE",
            (
                "The parser is fixed.\n\nVerified against the rules.",
                "a-reply",
            ),
        ),
    ] {
        let rows = [
            user("u-task", "Fix the parser."),
            says("a-report", "msg_1", "The parser is fixed."),
            user("u-check", &check),
            says("a-reply", "msg_2", reply),
        ];
        let got = report(&jsonl(&rows), DoneBy::Check).expect("a report");
        assert_eq!((got.text.as_str(), got.uuid.as_str()), want, "{reply:?}");
    }
    let rows = [
        user("u-task", "Fix the parser."),
        says("a-report", "msg_1", "The parser is fixed."),
        user("u-check1", DONE_CHECK),
        says("a-reply1", "msg_2", "DONE"),
        user("u-check2", &check),
        says("a-reply2", "msg_3", "ATERM-TASK-DONE"),
    ];
    let got = report(&jsonl(&rows), DoneBy::Check).expect("a report");
    assert_eq!(got.uuid, "a-report");
    let rows = [
        user("u-task", "Fix the parser."),
        says("a-early", "msg_1", "Half of it."),
        user("u-check1", DONE_CHECK),
        says("a-more", "msg_2", "Not yet; finished the rest: all fixed."),
        user("u-check2", DONE_CHECK),
        says("a-reply2", "msg_3", "ATERM-TASK-DONE"),
    ];
    let got = report(&jsonl(&rows), DoneBy::Check).expect("a report");
    assert_eq!(got.uuid, "a-more");
    let rows = worked(&[says("a-report", "msg_2", "Fixed.\nATERM-TASK-DONE")]);
    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-check"));
}

#[test]
fn a_legacy_done_check_still_recovers_the_workers_result() {
    let ruled = done_check_text(Some("leave unrelated files alone"), false).replacen(
        &done_check_words(0),
        DONE_CHECK_MARKER_BEFORE,
        1,
    );
    let completion = "All work is complete. All parser tests passed.";
    for check in [DONE_CHECK_MARKER_BEFORE, ruled.as_str()] {
        let rows = [
            user("u-task", "Fix the parser."),
            says("a-report", "msg_1", "The parser is fixed."),
            user("u-check1", DONE_CHECK_MARKER_BEFORE),
            says("a-reply1", "msg_2", "DONE"),
            user("u-check2", check),
            says("a-reply2", "msg_3", completion),
        ];
        for by in [DoneBy::Check, DoneBy::Turn] {
            let got = report(&jsonl(&rows), by).expect("legacy check's result");
            assert_eq!(got.text, format!("The parser is fixed.\n\n{completion}"));
            assert_eq!(got.uuid, "a-reply2");
        }
    }
    let quoted = format!("Read this advice: {DONE_CHECK_MARKER_BEFORE}");
    let rows = [
        user("u-task", &quoted),
        says("a-report", "msg_1", completion),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-check"));
}

#[test]
fn a_check_newest_is_read_by_the_checks_rule_whatever_rule_is_asked() {
    let tasks = ScratchDir::new("result", "check-newest");
    let file = Path::new("/tmp/t.jsonl");
    for (i, (reply, want)) in [
        (
            "ATERM-TASK-DONE",
            ("The parser is fixed; 12 tests pass.", "a-report"),
        ),
        (
            "Verified: all 12 green.\nATERM-TASK-DONE",
            (
                "The parser is fixed; 12 tests pass.\n\nVerified: all 12 green.",
                "a-reply",
            ),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let rows = [
            user("u-task", "Fix the parser."),
            says("a-report", "msg_1", "The parser is fixed; 12 tests pass."),
            turn_duration(),
            user("u-check", DONE_CHECK),
            says("a-reply", "msg_2", reply),
            turn_duration(),
        ];
        let text = jsonl(&rows);
        let got = report(&text, DoneBy::Turn).expect("a report");
        assert_eq!((got.text.as_str(), got.uuid.as_str()), want, "{reply:?}");
        let snap = snapshot(
            &tasks,
            &format!("s-1-{i}"),
            Some((file, &text)),
            DoneBy::Turn,
        )
        .expect("written");
        assert_eq!(snap.by, DoneBy::Check, "{reply:?}");
        assert_eq!(snap.bytes, got.text.len() as u64);
    }
    let rows = [
        user("u-task", "Fix the parser."),
        says("a-report", "msg_1", "The parser is fixed."),
        user("u-check1", DONE_CHECK),
        says("a-reply1", "msg_2", "Yes, it's all done."),
        user("u-check2", DONE_CHECK),
        says("a-reply2", "msg_3", "ATERM-TASK-DONE"),
    ];
    for by in [DoneBy::Check, DoneBy::Turn] {
        assert_eq!(
            report(&jsonl(&rows), by).map(|r| (r.text, r.uuid)),
            Ok((
                "The parser is fixed.\n\nYes, it's all done.".to_string(),
                "a-reply1".to_string()
            )),
            "{by:?}"
        );
    }
    let text = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed.\nATERM-TASK-DONE",
    )]));
    let snap = snapshot(&tasks, "s-1-9", Some((file, &text)), DoneBy::Turn).expect("written");
    assert_eq!((snap.by, snap.bytes), (DoneBy::Turn, "Fixed.".len() as u64));
}

fn landing(act: &str, reply: &[String]) -> Vec<String> {
    let mut rows = vec![
        user("u-task", "Fix the parser."),
        says("a-report", "msg_1", "The parser is fixed; 12 tests pass."),
        turn_duration(),
        user("u-land", LAND),
        says(
            "a-progress",
            "msg_2",
            "Committed it on the branch and pushed; merging next.",
        ),
        turn_duration(),
        user("u-act", act),
    ];
    rows.extend_from_slice(reply);
    rows.push(turn_duration());
    rows
}

#[test]
fn the_result_is_the_report_before_the_land_acts_then_the_defense() {
    let defense = "Nothing left: merged to origin/main at 4f2a9c1, worktree removed.";
    let acts = [LAND, LAND_NUDGE, LAND_ANSWER]
        .into_iter()
        .chain(RETIRED_LANDS)
        .chain(RETIRED_LAND_NUDGES);
    for act in acts {
        let rows = landing(act, &[says("a-defense", "msg_3", defense)]);
        for by in [DoneBy::Turn, DoneBy::Check] {
            let got = report(&jsonl(&rows), by).expect("the landing's result");
            assert_eq!(
                (got.text.as_str(), got.uuid.as_str()),
                (
                    format!("The parser is fixed; 12 tests pass.\n\n{defense}").as_str(),
                    "a-defense"
                ),
                "{act:.40}"
            );
        }
        let tasks = ScratchDir::new("result", "landing");
        let text = jsonl(&rows);
        let snap = snapshot(
            &tasks,
            "s-1-1",
            Some((Path::new("/t.jsonl"), &text)),
            DoneBy::Turn,
        )
        .expect("written");
        assert_eq!(snap.by, DoneBy::Check, "{act:.40}");
    }
    let rows = landing(
        LAND_NUDGE,
        &[says(
            "a-more",
            "msg_3",
            "Merging now; the suite runs on main.",
        )],
    );
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("no-reply"));
    let rows = landing(
        LAND_NUDGE,
        &[
            said(
                "a-pre",
                "msg_3",
                &format!("[{},{}]", text(defense), tool_use()),
            ),
            tool_result("u-pre"),
        ],
    );
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("no-reply"));
    let mut rows = landing(LAND_NUDGE, &[says("a-defense", "msg_3", defense)]);
    rows.extend([
        user("u-person", "status?"),
        says("a-person", "msg_4", defense),
    ]);
    assert_eq!(
        report(&jsonl(&rows), DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok((defense.to_string(), "a-person".to_string()))
    );
}

#[test]
fn a_landing_prunes_the_previous_result_pair() {
    let tasks = ScratchDir::new("result", "prune");
    for name in [
        "s-ab12-1.result.md",
        "s-ab12-1.result.src",
        "s-ab12-2.result.md",
        "s-ab12-2.result.src",
        "s-ab12-3.result.md",
        "s-ab12-3.result.src",
        "s-ab123-1.result.md",
        "s-ab12-x.result.md",
        "s-ab12-1.task",
    ] {
        std::fs::write(tasks.join(name), "").expect("file");
    }
    assert_eq!(prune_results(&tasks, "s-ab12-3"), 4);
    let mut left: Vec<String> = std::fs::read_dir(&*tasks)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "s-ab12-1.task",
            "s-ab12-3.result.md",
            "s-ab12-3.result.src",
            "s-ab12-x.result.md",
            "s-ab123-1.result.md",
        ]
    );
    assert_eq!(prune_results(&tasks, "s-ab12-3"), 0);
}

#[test]
fn a_delayed_landing_keeps_results_from_newer_revisions() {
    let tasks = ScratchDir::new("result", "prune-order");
    for revision in ["2", "9", "10", "010", "11", "100", "18446744073709551616"] {
        for extension in ["md", "src"] {
            std::fs::write(
                tasks.join(format!("s-ab12-{revision}.result.{extension}")),
                format!("result {revision}, {extension}"),
            )
            .expect("a landing's result pair");
        }
    }
    let removed = prune_results(&tasks, "s-ab12-10");
    for revision in ["11", "100", "10", "010", "18446744073709551616"] {
        for extension in ["md", "src"] {
            assert_eq!(
                std::fs::read_to_string(
                    tasks.join(format!("s-ab12-{revision}.result.{extension}"))
                )
                .ok(),
                Some(format!("result {revision}, {extension}")),
                "an older capture must preserve revision {revision}'s {extension}"
            );
        }
    }
    for revision in ["2", "9"] {
        for extension in ["md", "src"] {
            assert!(
                !tasks
                    .join(format!("s-ab12-{revision}.result.{extension}"))
                    .exists(),
                "revision {revision} precedes 10 numerically"
            );
        }
    }
    assert_eq!(removed, 4, "only the two earlier result pairs");
}

#[test]
fn an_unordered_landing_never_prunes_result_files() {
    let tasks = ScratchDir::new("result", "prune-unordered");
    for extension in ["md", "src"] {
        std::fs::write(
            tasks.join(format!("s-ab12-2.result.{extension}")),
            "a result whose ordering is known",
        )
        .expect("a landing's result pair");
    }
    let observed: Vec<usize> = ["unknown", "", "+10", "18446744073709551616"]
        .iter()
        .map(|revision| prune_results(&tasks, &format!("s-ab12-{revision}")))
        .collect();
    assert_eq!(
        observed,
        vec![0; 4],
        "an invalid or overflowing captured revision proves no earlier result"
    );
    for extension in ["md", "src"] {
        assert_eq!(
            std::fs::read_to_string(tasks.join(format!("s-ab12-2.result.{extension}"))).ok(),
            Some("a result whose ordering is known".to_string())
        );
    }
}

fn dead_pid() -> u32 {
    #[cfg(windows)]
    let mut command = {
        let mut cmd = aterm_uds::spawnfd::command("cmd");
        cmd.args(["/c", "exit 0"]);
        cmd
    };
    #[cfg(not(windows))]
    let mut command = aterm_uds::spawnfd::Command::new("true");
    let mut child = command.spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    dead
}

#[test]
fn a_check_reply_past_the_upgrades_tail_still_gives_the_report() {
    use super::super::upgrade_drive::{TAIL_BYTES, process_transcript, process_transcript_tail};
    let root = ScratchDir::new("result", "long-check");
    let home = root.join("home");
    let claude = home.join(".claude");
    let start = "Thu Oct 8 18:04:23 2026";
    let cwd = "/private/tmp/at.0000/proj";
    let pid = dead_pid();
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
    std::fs::write(
        claude.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"{cwd}","procStart":"{start}","version":"2.1.299","kind":"interactive","entrypoint":"cli","status":"idle"}}"#
        ),
    )
    .expect("record");
    let output = "test parser::case_ok ... ok\n".repeat(4_000);
    let suite = |uuid: &str| {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_1","type":"tool_result","content":{},"is_error":false}}]}},"uuid":"{uuid}","sessionId":"{SESSION}","version":"2.1.299"}}"#,
            js(&output)
        )
    };
    let mut rows = vec![
        user("u-task", "Fix the parser and run the suite."),
        says(
            "a-report",
            "msg_1",
            "The parser is fixed; the suite is green.",
        ),
        turn_duration(),
        user("u-check", DONE_CHECK),
        said("a-call", "msg_2", &format!("[{}]", tool_use())),
    ];
    rows.extend((0..3).map(|i| suite(&format!("u-suite-{i}"))));
    rows.push(says("a-reply", "msg_3", "Verified again.\nATERM-TASK-DONE"));
    let text = jsonl(&rows);
    let check_at = text.find("u-check").expect("the check") as u64;
    assert!(text.len() as u64 - check_at > TAIL_BYTES, "past the tail");
    let project = claude.join("projects/-private-tmp-at-0000-proj");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(project.join(format!("{SESSION}.jsonl")), &text).expect("transcript");
    let home_s = home.to_str();
    let (path, read) = process_transcript(None, home_s, None, pid, start).expect("found");
    assert_eq!(
        path.file_name(),
        Some(std::ffi::OsStr::new(&format!("{SESSION}.jsonl")))
    );
    let got = report(&read, DoneBy::Check).expect("a report");
    assert_eq!(
        (got.text.as_str(), got.uuid.as_str()),
        (
            "The parser is fixed; the suite is green.\n\nVerified again.",
            "a-reply"
        )
    );
    let tail = process_transcript_tail(None, home_s, None, pid, start).expect("found");
    assert_eq!(report(&tail, DoneBy::Check), Err("no-check"));
}

#[test]
fn the_upgrades_announcement_turn_is_read_past() {
    let notice = format!(
        "{ANNOUNCE_HEAD}2.1.300 (managed) is installed; this session runs 2.1.299. Reply READY."
    );
    let mut rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser.\nATERM-TASK-DONE",
    )]);
    rows.extend([user("u-ready", &notice), says("a-ready", "msg_r", "READY")]);
    let got = report(&jsonl(&rows), DoneBy::Turn).expect("a report");
    assert_eq!(got.uuid, "a-report");
    let mut rows = worked(&[says("a-report", "msg_2", "Fixed the parser.")]);
    rows.extend([
        user("u-ready", &notice),
        says("a-ready", "msg_r", "READY"),
        user("u-check", DONE_CHECK),
        says("a-reply", "msg_3", "ATERM-TASK-DONE"),
    ]);
    let got = report(&jsonl(&rows), DoneBy::Check).expect("a report");
    assert_eq!(got.uuid, "a-report");
    let mut rows = worked(&[says("a-report", "msg_2", "Half of it.")]);
    rows.extend([
        user("u-carry", "[aterm harness] Upgraded: carry on"),
        says("a-rest", "msg_3", "The rest is in.\nATERM-TASK-DONE"),
    ]);
    let got = report(&jsonl(&rows), DoneBy::Turn).expect("a report");
    assert_eq!(got.uuid, "a-rest");
}

#[test]
fn a_message_written_over_several_rows_is_one_message() {
    let rows = |second: &str| {
        worked(&[
            said(
                "a-think",
                "msg_2",
                r#"[{"type":"thinking","thinking":"done?","signature":"x"}]"#,
            ),
            says("a-one", "msg_2", "Part one: the parser."),
            says("a-two", second, "Part two: the suite.\nATERM-TASK-DONE"),
        ])
    };
    let got = report(&jsonl(&rows("msg_2")), DoneBy::Turn).expect("a report");
    assert_eq!(got.text, "Part one: the parser.\n\nPart two: the suite.");
    assert_eq!(got.uuid, "a-two");
    let got = report(&jsonl(&rows("msg_3")), DoneBy::Turn).expect("a report");
    assert_eq!(got.text, "Part two: the suite.");
}

#[test]
fn a_subagents_rows_and_claude_codes_own_are_no_ones_words() {
    let sidechain = says(
        "a-side",
        "msg_9",
        "Subagent: I fixed everything.\nATERM-TASK-DONE",
    )
    .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    let synthetic = says("a-syn", "msg_s", "No response requested.")
        .replace("claude-sonnet-5-5", "<synthetic>");
    let api_error = says("a-err", "msg_e", "API Error: 529 overloaded")
        .replace(r#""requestId""#, r#""isApiErrorMessage":true,"requestId""#);
    let meta = user("u-meta", "Base directory for this skill: /x").replace(
        r#""isSidechain":false"#,
        r#""isSidechain":false,"isMeta":true"#,
    );
    let summary = user(
        "u-sum",
        "This session is being continued from a previous conversation.",
    )
    .replace(
        r#""isSidechain":false"#,
        r#""isSidechain":false,"isCompactSummary":true"#,
    );
    let command = user("u-cmd", "<command-name>/usage</command-name>");
    let stdout = user(
        "u-out",
        "<local-command-stdout>Current session 3%</local-command-stdout>",
    );
    let report_row = says("a-report", "msg_2", "Fixed the parser.\nATERM-TASK-DONE");
    let mut rows = worked(&[report_row]);
    for row in [
        &sidechain, &synthetic, &api_error, &meta, &summary, &command, &stdout,
    ] {
        rows.push(row.clone());
    }
    let got = report(&jsonl(&rows), DoneBy::Turn).expect("a report");
    assert_eq!(got.text, "Fixed the parser.");
    assert_eq!(got.uuid, "a-report");
    let main = sidechain.replace(r#""isSidechain":true"#, r#""isSidechain":false"#);
    let mut rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser.\nATERM-TASK-DONE",
    )]);
    rows.push(main);
    assert_eq!(
        report(&jsonl(&rows), DoneBy::Turn).map(|r| r.text),
        Ok("Subagent: I fixed everything.".to_string())
    );
}

#[test]
fn an_unknown_shape_is_said_never_guessed() {
    let tasks = ScratchDir::new("result", "unknown");
    let marked = |t: &str| vec![user("u-task", "Fix it."), says("a", "m1", t)];
    let cases: [(&str, DoneBy, Option<Vec<String>>); 4] = [
        (
            "no-reply",
            DoneBy::Turn,
            Some(vec![user("u-task", "Fix it.")]),
        ),
        ("empty", DoneBy::Turn, Some(marked("ATERM-TASK-DONE"))),
        (
            "no-check",
            DoneBy::Check,
            Some(marked("Fixed.\nATERM-TASK-DONE")),
        ),
        ("no-transcript", DoneBy::Turn, None),
    ];
    for (i, (why, by, rows)) in cases.iter().enumerate() {
        let text = rows.as_ref().map(|rows| jsonl(rows));
        let transcript = Path::new("/tmp/t.jsonl");
        let snap = snapshot(
            &tasks,
            &format!("s-1-{i}"),
            text.as_deref().map(|t| (transcript, t)),
            *by,
        )
        .expect("written");
        assert_eq!((snap.bytes, snap.unavailable), (0, Some(*why)), "{why}");
        for full in [false, true] {
            assert_eq!(
                answer(&snap.path, full),
                format!("ERR result unavailable why={why}\n")
            );
        }
    }
    assert_eq!(
        answer(&tasks.join("s-9-1.result.md"), false),
        "ERR result unavailable why=no-snapshot\n"
    );
}

#[test]
fn result_answers_the_snapshot_and_full_reads_the_thread_now() {
    let root = ScratchDir::new("result", "answer");
    let tasks = root.join("tasks");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    let rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser.\nAll green.\nATERM-TASK-DONE",
    )]);
    std::fs::write(&transcript, jsonl(&rows)).expect("transcript");
    let text = std::fs::read_to_string(&transcript).expect("read");
    let snap =
        snapshot(&tasks, "s-ab12-1", Some((&transcript, &text)), DoneBy::Turn).expect("written");
    assert_eq!(snap.path, tasks.join("s-ab12-1.result.md"));
    assert_eq!(snap.unavailable, None);
    assert_eq!(snap.bytes, "Fixed the parser.\nAll green.".len() as u64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&tasks), 0o700);
        assert_eq!(mode(&snap.path), 0o600);
        assert_eq!(mode(&sidecar_of(&snap.path)), 0o600);
    }
    let reply = format!(
        "OK 2 source=transcript session={SESSION} uuid=a-report bytes={}\nFixed the parser.\nAll green.\n",
        snap.bytes
    );
    assert_eq!(answer(&snap.path, false), reply);
    let mut later = rows.clone();
    later.extend([
        user(
            "u-ready",
            "[aterm harness] A newer Claude Code is installed. Reply READY.",
        ),
        says("a-ready", "msg_r", "READY"),
        user("u-person", "what did you change?"),
        says("a-person", "msg_p", "Only the parser."),
    ]);
    std::fs::write(&transcript, jsonl(&later)).expect("transcript");
    assert_eq!(answer(&snap.path, false), reply, "the snapshot stands");
    let full = answer(&snap.path, true);
    let (head, body) = full.split_once('\n').expect("a header");
    assert!(
        head.starts_with(&format!(
            "OK {} source=transcript session={SESSION} uuid=a-report bytes=",
            body.lines().count()
        )) && head.ends_with(" full=1"),
        "{head}"
    );
    assert_eq!(
        body,
        "> Fix the parser and run the suite.\n\nRunning the suite.\n\nFixed the parser.\nAll green.\nATERM-TASK-DONE\n\n\
         > [aterm harness] A newer Claude Code is installed. Reply READY.\n\nREADY\n\n\
         > what did you change?\n\nOnly the parser.\n"
    );
}

#[test]
fn the_reply_is_capped_at_24000_characters_and_ends_with_the_path() {
    let root = ScratchDir::new("result", "cap");
    let tasks = root.join("tasks");
    let line = "é".repeat(99);
    let long: Vec<String> = (0..300).map(|i| format!("{i:03}{line}")).collect();
    let long = long.join("\n");
    assert!(long.chars().count() > RESULT_MAX_CHARS);
    let transcript = root.join("t.jsonl");
    let rows = worked(&[says(
        "a-report",
        "msg_2",
        &format!("{long}\nATERM-TASK-DONE"),
    )]);
    let text = jsonl(&rows);
    std::fs::write(&transcript, &text).expect("transcript");
    let snap =
        snapshot(&tasks, "s-1-1", Some((&transcript, &text)), DoneBy::Turn).expect("written");
    let reply = answer(&snap.path, false);
    let mut lines: Vec<&str> = reply.lines().collect();
    let head = lines.remove(0);
    assert!(head.ends_with(&format!(" bytes={}", long.len())), "{head}");
    assert_eq!(
        head.split_whitespace().nth(1),
        Some(lines.len().to_string()).as_deref()
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("… truncated; full: {}", snap.path.display()).as_str())
    );
    let shown: usize = lines[..lines.len() - 1]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum();
    assert_eq!(
        shown - 1,
        RESULT_MAX_CHARS,
        "exactly the bound, newlines counted"
    );
    let full = answer(&snap.path, true);
    let first = full.lines().nth(1).expect("a line");
    assert_eq!(
        first,
        format!(
            "… earlier turns cut; the transcript: {}",
            transcript.display()
        )
    );
    assert!(
        full.trim_end().ends_with("ATERM-TASK-DONE"),
        "the end is kept"
    );
    let exact: String = "x".repeat(RESULT_MAX_CHARS);
    let rows = worked(&[says(
        "a-report",
        "msg_2",
        &format!("{exact}\nATERM-TASK-DONE"),
    )]);
    let text = jsonl(&rows);
    let snap =
        snapshot(&tasks, "s-1-2", Some((&transcript, &text)), DoneBy::Turn).expect("written");
    let reply = answer(&snap.path, false);
    assert_eq!(reply.lines().count(), 2, "the header and the one line");
    assert!(!reply.contains("truncated"));
}

#[test]
fn a_thread_with_no_prompt_is_shown_cut_or_said() {
    let rows = [says("a-1", "msg_1", "Still on the parser.")];
    assert_eq!(
        thread(&jsonl(&rows)),
        ("Still on the parser.".to_string(), false)
    );
    let mut with_prompt = vec![user("u-task", "Fix it.")];
    with_prompt.extend(rows.iter().cloned());
    assert_eq!(
        thread(&jsonl(&with_prompt)),
        ("> Fix it.\n\nStill on the parser.".to_string(), true)
    );
    let root = ScratchDir::new("result", "noprompt");
    let transcript = root.join("t.jsonl");
    let report_rows = worked(&[says("a-report", "msg_2", "Fixed.\nATERM-TASK-DONE")]);
    let text = jsonl(&report_rows);
    let snap = snapshot(
        &root.join("tasks"),
        "s-1-1",
        Some((&transcript, &text)),
        DoneBy::Turn,
    )
    .expect("written");
    std::fs::write(&transcript, jsonl(&rows)).expect("transcript");
    assert_eq!(
        answer(&snap.path, true),
        "ERR result unavailable why=no-prompt\n"
    );
}

#[test]
fn the_sidecar_line_reads_back_and_a_stem_is_one_name() {
    for source in [
        Source::Transcript {
            session: SESSION.to_string(),
            uuid: "a-1".to_string(),
            transcript: Some(PathBuf::from("/Users//a b/.claude/projects/x%y/s.jsonl")),
        },
        Source::Unavailable {
            why: "no-marker".to_string(),
            transcript: None,
            turn: None,
        },
        Source::Unavailable {
            why: "no-reply".to_string(),
            transcript: Some(PathBuf::from("/t/a b.jsonl")),
            turn: Some("u-task 1".to_string()),
        },
        Source::Screen {
            program: "agy".to_string(),
        },
    ] {
        assert_eq!(Source::parse(&source.line()), Some(source.clone()));
    }
    for bad in [
        "",
        "source=else",
        "source=none why=Not%20a%20word",
        "source=screen",
        "source=screen program=Not%20a%20word",
        "source=transcript uuid=a",
    ] {
        assert_eq!(Source::parse(bad), None, "{bad:?}");
    }
    let tasks = ScratchDir::new("result", "stem");
    for stem in ["", "../x", "a/b", "s 1"] {
        assert!(
            snapshot(&tasks, stem, None, DoneBy::Turn).is_err(),
            "{stem:?}"
        );
    }
    assert!(std::fs::read_dir(&*tasks).expect("dir").next().is_none());
}

#[test]
fn a_screen_landings_result_opens_with_its_report() {
    let defense = "Nothing left: merged into the remote main branch.";
    assert_eq!(
        with_report(Some("Fixed the parser.\n"), defense),
        format!("Fixed the parser.\n\n{defense}")
    );
    for none in [None, Some(""), Some("  \n"), Some(defense)] {
        assert_eq!(with_report(none, defense), defense, "{none:?}");
    }
}

#[test]
fn a_screens_result_is_the_landings_words() {
    let tasks = ScratchDir::new("result", "screen");
    let said = screen_said(
        Some("Merged the parser.\n"),
        "Nothing left: all of it is merged into the remote main branch.\n",
    );
    assert_eq!(
        said,
        "Merged the parser.\n\nNothing left: all of it is merged into the remote main branch."
    );
    assert_eq!(screen_said(None, " last "), "last");
    for program in ["agy", "muse"] {
        let stem = format!("s-{program}-2");
        let snap = snapshot_screen(&tasks, &stem, program, &said, DoneBy::Turn).expect("written");
        assert_eq!(snap.bytes, said.len() as u64);
        assert_eq!(snap.unavailable, None);
        assert_eq!(
            std::fs::read_to_string(&snap.path).expect("the result"),
            said
        );
        assert_eq!(
            source_of(&snap.path),
            Some(Source::Screen {
                program: program.to_string()
            })
        );
        assert_eq!(
            answer(&snap.path, false),
            format!(
                "OK 3 source=screen program={program} bytes={}\n{said}\n",
                said.len()
            )
        );
        assert_eq!(
            answer(&snap.path, true),
            "ERR result unavailable why=no-transcript\n"
        );
    }
    let long = "x".repeat(RESULT_MAX_CHARS + 10);
    let snap = snapshot_screen(&tasks, "s-long-1", "agy", &long, DoneBy::Turn).expect("written");
    assert!(
        answer(&snap.path, false).contains("… truncated; full: "),
        "cut as a transcript's is"
    );
    let blank =
        snapshot_screen(&tasks, "s-blank-1", "muse", " \n ", DoneBy::Turn).expect("written");
    assert_eq!(blank.unavailable, Some("no-words"));
    assert_eq!(
        answer(&blank.path, false),
        "ERR result unavailable why=no-words\n"
    );
    let none = snapshot(&tasks, "s-none-1", None, DoneBy::Turn).expect("written");
    assert_eq!(
        answer(&none.path, false),
        "ERR result unavailable why=no-transcript\n"
    );
    assert!(snapshot_screen(&tasks, "../x", "agy", &said, DoneBy::Turn).is_err());
}

#[test]
fn each_screen_harnesss_landing_words_are_read_off_its_screen() {
    use crate::supervise::policy::turn_end::{TurnEndReading, screen_alone};
    use aterm_phase::Program;
    let shell = aterm_phase::prompt::fixtures::rows(&["% "]);
    for (program, text) in [
        (Program::Agy, aterm_phase::agy::fixtures::lab::END_OF_TURN),
        (Program::Muse, aterm_phase::muse::fixtures::END_OF_TURN),
    ] {
        assert!(screen_alone(program), "{program:?}: no transcript is read");
        let rows = aterm_phase::prompt::fixtures::screen(text);
        let reading = aterm_phase::read(Some(program.name()), &rows, None);
        let point = TurnEndReading::of(&reading, &rows, false, None, None, None);
        let tail = point
            .said_tail
            .as_deref()
            .unwrap_or_else(|| panic!("{program:?}: the reply's words"));
        assert!(!point.said_cut, "{program:?}: whole on its screen");
        let said = screen_said(point.turn_said.as_deref(), tail);
        assert!(!said.trim().is_empty(), "{program:?}");
        assert!(
            said.ends_with(tail.trim()),
            "{program:?}: {said:?} ends with {tail:?}"
        );
        let none = aterm_phase::read(Some(program.name()), &shell, None);
        let none = TurnEndReading::of(&none, &shell, false, None, None, None);
        assert_eq!(
            none.said_tail, None,
            "CONTROL {program:?}: a shell's prompt"
        );
    }
}

fn lagging() -> Vec<String> {
    vec![
        user("u-task", "Fix the parser and run the suite."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
    ]
}

fn turn_ended(jsonl: &str) -> bool {
    end_of(jsonl, &Opener::of(jsonl, None)).ended
}

#[test]
fn the_newest_turn_has_ended_at_its_end_row_after_its_prompt() {
    let reply = says("a-report", "msg_2", "Fixed.\nATERM-TASK-DONE");
    assert!(turn_ended(&jsonl(&worked(std::slice::from_ref(&reply)))));
    assert!(turn_ended(&jsonl(&[reply.clone(), turn_duration()])));
    let mut no_end = lagging();
    no_end.push(reply.clone());
    assert!(!turn_ended(&jsonl(&no_end)), "the reply, no end row yet");
    assert!(!turn_ended(&jsonl(&lagging())), "the prompt alone");
    let ended = worked(std::slice::from_ref(&reply));
    for (newer, why) in [
        (
            user("u-next", "and the docs?"),
            "a new prompt after the end",
        ),
        (
            user("u-ask", "what is turn_duration?"),
            "a prompt that names the row",
        ),
    ] {
        let mut rows = ended.clone();
        rows.push(newer);
        assert!(!turn_ended(&jsonl(&rows)), "{why}");
    }
    let side = turn_duration().replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    let mut rows = no_end.clone();
    rows.push(side);
    assert!(!turn_ended(&jsonl(&rows)), "a subagent's end row");
    let end = turn_duration();
    let cut = format!("{}{}", jsonl(&no_end), &end[..end.len() / 2]);
    assert!(!turn_ended(&cut), "an end row half written");
}

#[test]
fn a_reply_written_after_the_done_edges_read_is_waited_for() {
    let first = jsonl(&lagging());
    let whole = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser.\nATERM-TASK-DONE",
    )]));
    let mut writes = std::collections::VecDeque::from([first.clone(), whole.clone()]);
    let mut asked = Vec::new();
    let mut pauses = 0;
    let got = settle_with(
        first.clone(),
        None,
        |bytes| {
            asked.push(bytes);
            if writes.len() > 1 {
                writes.pop_front()
            } else {
                writes.front().cloned()
            }
        },
        || {
            pauses += 1;
            true
        },
    );
    assert_eq!(
        got,
        Settled {
            tail: whole.clone(),
            waits: 1,
            ended: true,
        }
    );
    assert_eq!(pauses, 1);
    assert_eq!(
        got.note(),
        "the transcript's turn end was read after 1 wait"
    );
    assert_eq!(
        asked,
        [END_READ_BYTES, END_READ_BYTES, TRANSCRIPT_READ_BYTES]
    );
    assert_eq!(
        report(&got.tail, DoneBy::Turn).map(|r| r.text),
        Ok("Fixed the parser.".to_string())
    );
    assert_eq!(report(&first, DoneBy::Turn), Err("no-reply"));
    let at_once = settle_with(
        whole.clone(),
        None,
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!(at_once.tail, whole);
    assert_eq!((at_once.waits, at_once.ended), (0, true));
    let gone = settle_with(first.clone(), None, |_| None, || panic!("waited"));
    assert_eq!(gone.tail, first);
    assert_eq!((gone.waits, gone.ended), (0, false));
}

#[test]
fn past_the_bound_the_snapshot_is_read_as_it_stands() {
    let first = jsonl(&lagging());
    let mut pauses = 0u32;
    let got = settle_with(
        first.clone(),
        None,
        |_| Some(first.clone()),
        || {
            pauses += 1;
            pauses <= 3
        },
    );
    assert_eq!((got.waits, got.ended), (3, false));
    assert_eq!(got.tail, first);
    assert_eq!(
        got.note(),
        "no turn end in the transcript after 3 waits: read as it stands"
    );
    let root = ScratchDir::new("result", "bound");
    let tasks = root.join("tasks");
    let file = root.join("t.jsonl");
    let snap = snapshot(&tasks, "s-1-1", Some((&file, &got.tail)), DoneBy::Turn).expect("written");
    assert_eq!((snap.bytes, snap.unavailable), (0, Some("no-reply")));
    assert_eq!(
        answer(&snap.path, false),
        "ERR result unavailable why=no-reply\n"
    );
    assert_eq!(
        source_of(&snap.path),
        Some(Source::Unavailable {
            why: "no-reply".to_string(),
            transcript: Some(file.clone()),
            turn: Some("u-task".to_string()),
        })
    );
    std::fs::write(&file, &first).expect("transcript");
    let stopped = settled(&file, first.clone(), None, || true);
    assert_eq!((stopped.waits, stopped.ended), (0, false));
    let whole = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed.\nATERM-TASK-DONE",
    )]));
    std::fs::write(&file, &whole).expect("transcript");
    let read = settled(&file, first, None, || true);
    assert_eq!((read.tail, read.waits, read.ended), (whole, 0, true));
}

#[test]
fn result_reads_a_lagged_turn_again_and_never_a_later_one() {
    let root = ScratchDir::new("result", "reread");
    let tasks = root.join("tasks");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    let first = jsonl(&lagging());
    std::fs::write(&transcript, &first).expect("transcript");
    let snap =
        snapshot(&tasks, "s-1-1", Some((&transcript, &first)), DoneBy::Turn).expect("written");
    assert_eq!(snap.unavailable, Some("no-reply"));
    assert_eq!(reread(&snap.path), None, "nothing written since");
    let mut later = lagging();
    later.extend([
        says("a-report", "msg_2", "Fixed the parser.\nATERM-TASK-DONE"),
        turn_duration(),
        user("u-person", "and the docs?"),
        says("a-person", "msg_3", "Docs too.\nATERM-TASK-DONE"),
        turn_duration(),
    ]);
    std::fs::write(&transcript, jsonl(&later)).expect("transcript");
    let again = reread(&snap.path).expect("the turn's reply");
    assert_eq!(
        (again.report.text.as_str(), again.report.uuid.as_str()),
        ("Fixed the parser.", "a-report")
    );
    assert_eq!(again.bytes(), "Fixed the parser.".len() as u64);
    let reply = format!(
        "OK 1 source=transcript session={SESSION} uuid=a-report bytes=17\nFixed the parser.\n"
    );
    assert_eq!(again.answer(&snap.path, false), reply, "answered unwritten");
    again.write(&snap.path).expect("written over");
    assert_eq!(answer(&snap.path, false), reply);
    assert_eq!(reread(&snap.path), None, "a result is never read again");

    let snap =
        snapshot(&tasks, "s-1-2", Some((&transcript, &first)), DoneBy::Turn).expect("written");
    let mut later = lagging();
    later.extend([
        turn_duration(),
        user("u-person", "status?"),
        says("a-person", "msg_3", "All done.\nATERM-TASK-DONE"),
        turn_duration(),
    ]);
    std::fs::write(&transcript, jsonl(&later)).expect("transcript");
    assert_eq!(reread(&snap.path), None, "never a later turn's reply");
    let whole = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed.\nATERM-TASK-DONE",
    )]));
    let snap =
        snapshot(&tasks, "s-1-3", Some((&transcript, &whole)), DoneBy::Check).expect("written");
    assert_eq!(snap.unavailable, Some("no-check"));
    std::fs::write(&transcript, &whole).expect("transcript");
    assert_eq!(reread(&snap.path), None);
    let old = tasks.join("s-1-4.result.md");
    std::fs::write(&old, "").expect("result");
    std::fs::write(
        sidecar_of(&old),
        format!(
            "source=none why=no-reply transcript={}\n",
            aterm_codec::pct::encode(&transcript.to_string_lossy())
        ),
    )
    .expect("sidecar");
    assert_eq!(reread(&old), None);
}

fn notification(uuid: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"d3ac0a3e","type":"user","message":{{"role":"user","content":{}}},"uuid":"{uuid}","timestamp":"2026-10-16T05:21:55.916Z","permissionMode":"auto","origin":{{"kind":"task-notification","producer":"session-task"}},"promptSource":"system","turnOrigin":"task_notification","userType":"external","entrypoint":"cli","sessionId":"{SESSION}","version":"2.1.301","gitBranch":"HEAD"}}"#,
        js(
            "<task-notification>\n<task-id>bbm1zphah</task-id>\n<status>completed</status>\n\
            <summary>Background command \"Await both workers\" completed (exit code 0)</summary>\n\
            </task-notification>"
        )
    )
}

fn continuation(uuid: &str) -> String {
    user(
        uuid,
        "Your claude.ai usage limit has reset. Continue the task you were working on.",
    )
    .replace(
        r#""isSidechain":false,"#,
        r#""isSidechain":false,"isMeta":true,"#,
    )
}

fn woken() -> Vec<String> {
    woken_by(notification("u-woke"))
}

fn woken_by(opener: String) -> Vec<String> {
    vec![
        user("u-task", "Delegate the count to two workers and report."),
        said("a-spawn", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-spawned"),
        says("a-wait", "msg_2", "Started both workers; waiting for them."),
        turn_duration(),
        opener,
        said("a-read", "msg_3", &format!("[{}]", tool_use())),
        tool_result("u-read"),
        said(
            "a-again",
            "msg_4",
            &format!(
                "[{},{}]",
                text("Task is done; try plain-text screen and result again"),
                tool_use()
            ),
        ),
        tool_result("u-again"),
    ]
}

fn woken_reply() -> [String; 2] {
    [
        says(
            "a-report",
            "msg_5",
            "Worker 1 counted 932; worker 2 counted 41.\nATERM-TASK-DONE",
        ),
        turn_duration(),
    ]
}

#[test]
fn a_turn_a_task_notification_started_is_waited_for() {
    let first = jsonl(&woken());
    assert!(
        !turn_ended(&first),
        "the notification's turn has no end yet"
    );
    assert!(turn_ended(&jsonl(&woken()[..5])), "the turn ended to wait");
    let mut resumed = woken_by(continuation("u-resumed"));
    assert!(!turn_ended(&jsonl(&resumed)), "the continuation's turn");
    resumed.extend(woken_reply());
    assert!(turn_ended(&jsonl(&resumed)));
    assert_eq!(report(&first, DoneBy::Turn), Err("no-reply"));
    let mut rows = woken();
    rows.extend(woken_reply());
    let whole = jsonl(&rows);
    assert!(turn_ended(&whole));
    let mut writes = std::collections::VecDeque::from([first.clone(), whole.clone()]);
    let got = settle_with(
        first,
        None,
        |_| {
            if writes.len() > 1 {
                writes.pop_front()
            } else {
                writes.front().cloned()
            }
        },
        || true,
    );
    assert_eq!((got.waits, got.ended), (1, true));
    assert_eq!(got.tail, whole);
    assert_eq!(
        report(&got.tail, DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok((
            "Worker 1 counted 932; worker 2 counted 41.".to_string(),
            "a-report".to_string()
        ))
    );
}

#[test]
fn the_wait_is_for_the_done_edges_turn_never_one_begun_since() {
    let first = jsonl(&lagging());
    let mut task = lagging();
    task.extend([
        says("a-report", "msg_2", "Fixed the parser.\nATERM-TASK-DONE"),
        turn_duration(),
    ]);
    let person = [
        user("u-person", "and the docs?"),
        says("a-person", "msg_3", "Docs too.\nATERM-TASK-DONE"),
        turn_duration(),
    ];
    let mut rows = task.clone();
    rows.extend(person.iter().cloned());
    let later = jsonl(&rows);
    let got = settle_with(first.clone(), None, |_| Some(later.clone()), || true);
    assert_eq!((got.waits, got.ended), (0, true));
    assert_eq!(got.tail, jsonl(&task));
    assert_eq!(
        report(&got.tail, DoneBy::Turn).map(|r| r.text),
        Ok("Fixed the parser.".to_string())
    );
    assert_eq!(
        report(&later, DoneBy::Turn).map(|r| r.text),
        Ok("Docs too.".to_string())
    );

    let mut rows = lagging();
    rows.extend(person.iter().cloned());
    let later = jsonl(&rows);
    let got = settle_with(first.clone(), None, |_| Some(later.clone()), || true);
    assert_eq!(
        (got.waits, got.ended, got.tail.as_str()),
        (0, true, first.as_str())
    );
    let root = ScratchDir::new("result", "since");
    let tasks = root.join("tasks");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    std::fs::write(&transcript, &later).expect("transcript");
    let snap = snapshot(
        &tasks,
        "s-1-1",
        Some((&transcript, &got.tail)),
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(snap.unavailable, Some("no-reply"));
    assert_eq!(
        source_of(&snap.path),
        Some(Source::Unavailable {
            why: "no-reply".to_string(),
            transcript: Some(transcript.clone()),
            turn: Some("u-task".to_string()),
        })
    );
    assert_eq!(reread(&snap.path), None, "never the person's reply");
}

#[test]
fn result_reads_a_notifications_turn_again_up_to_its_end() {
    let first = jsonl(&woken());
    let got = settle_with(first.clone(), None, |_| Some(first.clone()), || false);
    assert_eq!((got.waits, got.ended), (0, false));
    let root = ScratchDir::new("result", "woken");
    let tasks = root.join("tasks");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    let snap = snapshot(
        &tasks,
        "s-1-1",
        Some((&transcript, &got.tail)),
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(snap.unavailable, Some("no-reply"));
    assert_eq!(
        source_of(&snap.path),
        Some(Source::Unavailable {
            why: "no-reply".to_string(),
            transcript: Some(transcript.clone()),
            turn: Some("u-woke".to_string()),
        })
    );
    let again = [
        notification("u-woke-2"),
        says("a-later", "msg_9", "Nothing more to do.\nATERM-TASK-DONE"),
        turn_duration(),
    ];
    let mut rows = woken();
    rows.extend(woken_reply());
    rows.extend(again.iter().cloned());
    std::fs::write(&transcript, jsonl(&rows)).expect("transcript");
    let found = reread(&snap.path).expect("the woken turn's reply");
    assert_eq!(
        (found.report.text.as_str(), found.report.uuid.as_str()),
        ("Worker 1 counted 932; worker 2 counted 41.", "a-report")
    );
    let mut rows = woken();
    rows.push(turn_duration());
    rows.extend(again.iter().cloned());
    std::fs::write(&transcript, jsonl(&rows)).expect("transcript");
    assert_eq!(
        reread(&snap.path),
        None,
        "never a later notification's reply"
    );
}

fn stamped(row: &str, at: &str) -> String {
    let (head, rest) = row.split_once(r#""timestamp":""#).expect("a stamped row");
    let (_, tail) = rest.split_once('"').expect("the stamp's end");
    format!(r#"{head}"timestamp":"{at}"{tail}"#)
}

fn shown(at: &str) -> Option<i64> {
    Some(stamp_ms(at).expect("a stamp"))
}

fn done_then_woken() -> Vec<String> {
    let mut rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser; the build runs in the background.\nATERM-TASK-DONE",
    )]);
    rows.push(stamped(&notification("u-bg"), "2026-10-15T17:28:28.040Z"));
    rows
}

const DONE_AT_THE_MARKER: &str = "2026-10-15T17:28:27.990Z";

#[test]
fn a_notification_after_the_done_showed_is_no_turn_waited_for() {
    let first = jsonl(&done_then_woken());
    let task = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser; the build runs in the background.\nATERM-TASK-DONE",
    )]));
    let got = settle_with(
        first.clone(),
        shown(DONE_AT_THE_MARKER),
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!((got.waits, got.ended), (0, true));
    assert_eq!(got.tail, task);
    assert_eq!(
        report(&got.tail, DoneBy::Turn).map(|r| r.text),
        Ok("Fixed the parser; the build runs in the background.".to_string())
    );
    let mut rows = done_then_woken();
    rows.extend([
        says("a-wake", "msg_3", "The background build passed."),
        turn_duration(),
    ]);
    let whole = jsonl(&rows);
    let again = settle_with(
        whole.clone(),
        shown(DONE_AT_THE_MARKER),
        |_| None,
        || panic!("waited"),
    );
    assert_eq!(again.tail, task);
    assert_eq!(
        report(&whole, DoneBy::Turn).map(|r| r.text),
        Ok("The background build passed.".to_string())
    );
    let mut pauses = 0;
    let unplaced = settle_with(
        first.clone(),
        None,
        |_| Some(first.clone()),
        || {
            pauses += 1;
            pauses <= 2
        },
    );
    assert_eq!((unplaced.waits, unplaced.ended), (2, false));

    let mut rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser; the build runs in the background.\nATERM-TASK-DONE",
    )]);
    rows.push(stamped(
        &user("u-person", "and the docs?"),
        "2026-10-15T17:28:31.000Z",
    ));
    let person = settle_with(
        jsonl(&rows),
        shown(DONE_AT_THE_MARKER),
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!((person.tail.as_str(), person.ended), (task.as_str(), true));
    assert!(!turn_ended(&jsonl(&rows)));

    let mut rows = worked(&[says("a-report", "msg_2", "Fixed the parser.")]);
    rows.extend([
        user("u-check", DONE_CHECK),
        says("a-yes", "msg_3", "Yes, all done."),
        turn_duration(),
        stamped(&notification("u-bg"), "2026-10-15T17:28:28.040Z"),
    ]);
    let checked = settle_with(
        jsonl(&rows),
        shown(DONE_AT_THE_MARKER),
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!((checked.waits, checked.ended), (0, true));
    assert_eq!(
        report(&checked.tail, DoneBy::Check).map(|r| r.text),
        Ok("Fixed the parser.\n\nYes, all done.".to_string())
    );
}

fn held_then_woken() -> Vec<String> {
    let mut rows = worked(&[says(
        "a-held",
        "msg_2",
        "Started the review agent in the background.\nATERM-TASK-DONE",
    )]);
    rows.extend([
        stamped(&notification("u-woke"), "2026-10-15T17:29:10.000Z"),
        stamped(
            &said("a-read", "msg_3", &format!("[{}]", tool_use())),
            "2026-10-15T17:29:13.000Z",
        ),
        stamped(&tool_result("u-read"), "2026-10-15T17:29:14.000Z"),
    ]);
    rows
}

fn held_woken_reply() -> [String; 2] {
    [
        stamped(
            &says(
                "a-wake",
                "msg_4",
                "The review agent found nothing more to fix.\nATERM-TASK-DONE",
            ),
            "2026-10-15T17:29:20.020Z",
        ),
        stamped(&turn_duration(), "2026-10-15T17:29:20.040Z"),
    ]
}

#[test]
fn the_wake_whose_end_is_the_done_is_waited_for() {
    let first = jsonl(&held_then_woken());
    let mut rows = held_then_woken();
    rows.extend(held_woken_reply());
    let whole = jsonl(&rows);
    let mut writes = std::collections::VecDeque::from([first.clone(), whole.clone()]);
    let got = settle_with(
        first.clone(),
        shown("2026-10-15T17:29:20.000Z"),
        |_| {
            if writes.len() > 1 {
                writes.pop_front()
            } else {
                writes.front().cloned()
            }
        },
        || true,
    );
    assert_eq!((got.waits, got.ended), (1, true));
    assert_eq!(got.tail, whole);
    assert_eq!(
        report(&got.tail, DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok((
            "The review agent found nothing more to fix.".to_string(),
            "a-wake".to_string()
        ))
    );
    let earlier = settle_with(
        first,
        shown(DONE_AT_THE_MARKER),
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!((earlier.waits, earlier.ended), (0, true));
    assert_eq!(
        report(&earlier.tail, DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok((
            "Started the review agent in the background.".to_string(),
            "a-held".to_string()
        ))
    );
}

#[test]
fn a_rows_stamp_is_read_to_the_millisecond() {
    let base = 1_792_085_305_000;
    assert_eq!(stamp_ms("2026-10-15T17:28:25.422Z"), Some(base + 422));
    assert_eq!(stamp_ms("2026-10-15T17:28:25.4Z"), Some(base + 400));
    assert_eq!(stamp_ms("2026-10-15T17:28:25.422999Z"), Some(base + 422));
    assert_eq!(stamp_ms("2026-10-15T17:28:25Z"), Some(base));
    for bad in [
        "2026-10-15T17:28:25.422+00:00",
        "2026-10-15T17:28:25.422",
        "2026-10-15T17:28:25.Z",
    ] {
        assert_eq!(stamp_ms(bad), None, "{bad}");
    }
}

#[test]
fn a_turn_that_wrote_no_end_row_stops_at_the_next_turns_start() {
    let first = jsonl(&lagging());
    let mut rows = lagging();
    rows.extend([
        says("a-half", "msg_2", "Fixed the parser, now running the"),
        notification("u-bg"),
        says("a-wake", "msg_3", "The build passed.\nATERM-TASK-DONE"),
        turn_duration(),
    ]);
    let later = jsonl(&rows);
    let got = settle_with(first.clone(), None, |_| Some(later.clone()), || true);
    assert_eq!((got.waits, got.ended), (0, true));
    assert_eq!(got.tail, jsonl(&rows[..4]));
    let half = Ok("Fixed the parser, now running the".to_string());
    assert_eq!(report(&got.tail, DoneBy::Turn).map(|r| r.text), half);
    let root = ScratchDir::new("result", "no-end");
    let tasks = root.join("tasks");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    let snap =
        snapshot(&tasks, "s-1-1", Some((&transcript, &first)), DoneBy::Turn).expect("written");
    assert_eq!(snap.unavailable, Some("no-reply"));
    std::fs::write(&transcript, &later).expect("transcript");
    assert_eq!(
        reread(&snap.path).map(|r| r.report.text),
        half.ok(),
        "never the wake's reply"
    );
    let mut ended = rows[..4].to_vec();
    ended.push(turn_duration());
    ended.extend(rows[4..].iter().cloned());
    let got = settle_with(first.clone(), None, |_| Some(jsonl(&ended)), || true);
    assert_eq!(got.tail, jsonl(&ended[..5]));
    let mut pair = woken()[..5].to_vec();
    pair.extend([notification("u-bg-1"), notification("u-bg-2")]);
    pair.extend(woken_reply());
    let whole = jsonl(&pair);
    assert!(turn_ended(&whole));
    assert_eq!(
        report(
            &settle_with(whole.clone(), None, |_| None, || true).tail,
            DoneBy::Turn
        )
        .map(|r| r.uuid),
        Ok("a-report".to_string())
    );
}

const THREAD: &str = "01a15a2c-3732-7b22-9cf1-19657e6444e7";

/// When the fixtures' codex landing was seen (unix ms): five minutes after their rows'
/// `timestamp` ([`cx`]).
const LANDED: i64 = 1_792_749_900_000;

fn cx(kind: &str, payload: &str) -> String {
    format!(r#"{{"timestamp":"2026-10-23T10:00:00.000Z","type":"{kind}","payload":{payload}}}"#)
}

fn cx_started(turn: &str, trigger: &str) -> String {
    cx(
        "event_msg",
        &format!(
            r#"{{"type":"task_started","turn_id":"{turn}","root_turn_id":"{turn}","turn_attribution":{{"turn_id":"{turn}","turn_trigger":"{trigger}"}}}}"#
        ),
    )
}

fn cx_user(t: &str) -> String {
    cx(
        "response_item",
        &format!(
            r#"{{"type":"message","role":"user","content":[{{"type":"input_text","text":{}}}]}}"#,
            js(t)
        ),
    )
}

fn cx_said(t: &str, phase: &str) -> String {
    cx(
        "response_item",
        &format!(
            r#"{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":{}}}],"phase":"{phase}"}}"#,
            js(t)
        ),
    )
}

fn cx_complete(turn: &str, last: &str) -> String {
    cx(
        "event_msg",
        &format!(
            r#"{{"type":"task_complete","turn_id":"{turn}","last_agent_message":{}}}"#,
            js(last)
        ),
    )
}

fn cx_reasoning() -> String {
    cx(
        "response_item",
        r#"{"type":"reasoning","summary":[],"encrypted_content":"gAAAAB-message-"}"#,
    )
}

/// A codex-alab worker's conversation: its task (after the context Codex opens a
/// conversation with), the report, Land's first act, then `act` and its `reply`.
fn codex_landing(act: &str, reply: &str) -> Vec<String> {
    vec![
        cx_started("t-1", "user"),
        cx_user("<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"),
        cx_user("# AGENTS.md instructions for /w\n\n<INSTRUCTIONS>\nrules\n</INSTRUCTIONS>"),
        cx_user("Fix the parser."),
        cx_said("Reading the parser first.", "commentary"),
        cx_reasoning(),
        cx_user("Also run the suite."),
        cx_said("The parser is fixed; 12 tests pass.", "final_answer"),
        cx_complete("t-1", "The parser is fixed; 12 tests pass."),
        cx_started("t-2", "alab_autopilot"),
        cx_user(LAND),
        cx_said("Committed it and pushed; merging next.", "final_answer"),
        cx_complete("t-2", "Committed it and pushed; merging next."),
        cx_started("t-3", "alab_autopilot"),
        cx_user(act),
        cx_said(reply, "final_answer"),
        cx_complete("t-3", reply),
    ]
}

/// A codex rollout is read as a transcript is: a landing's result is the report before the land
/// acts, then the defense; a turn whose last words are commentary has no reply yet; and a person's
/// own turn after the landing is its final answer alone.
#[test]
fn a_codex_rollout_is_read_as_a_transcript_is() {
    let defense = "Nothing left: merged to origin/main at 4f2a9c1, worktree removed.";
    for act in [LAND, LAND_NUDGE, LAND_ANSWER] {
        let rows = codex_landing(act, defense);
        for by in [DoneBy::Turn, DoneBy::Check] {
            let got = codex_report(&jsonl(&rows), THREAD, by).expect("the landing's result");
            assert_eq!(
                (got.text.as_str(), got.session.as_str(), got.uuid.as_str()),
                (
                    format!("The parser is fixed; 12 tests pass.\n\n{defense}").as_str(),
                    THREAD,
                    "t-3"
                ),
                "{act:.40}"
            );
        }
    }
    let mut rows = codex_landing(LAND_NUDGE, defense);
    rows.extend([
        cx_started("t-4", "user"),
        cx_user("status?"),
        cx_said("All merged.", "final_answer"),
        cx_complete("t-4", "All merged."),
    ]);
    assert_eq!(
        codex_report(&jsonl(&rows), THREAD, DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok(("All merged.".to_string(), "t-4".to_string())),
        "the control: a person's turn is no check"
    );
    let mut rows = codex_landing(LAND_NUDGE, defense);
    rows.truncate(rows.len() - 2);
    rows.push(cx_said("Merging now.", "commentary"));
    assert_eq!(
        codex_report(&jsonl(&rows), THREAD, DoneBy::Turn),
        Err("no-reply")
    );
    let task = jsonl(&codex_landing(LAND, defense)[..9]);
    assert_eq!(
        codex_report(&task, THREAD, DoneBy::Turn).map(|r| r.text),
        Ok("The parser is fixed; 12 tests pass.".to_string()),
        "the task's own turn, before any land act"
    );
    assert_eq!(codex_report("", THREAD, DoneBy::Turn), Err("no-reply"));
}

/// A codex turn's prompt is its first words past what Codex adds itself: a message steered into
/// the land turn opens no turn, an `<environment_context>` update or a directory-less `AGENTS.md`
/// fragment is no prompt, and a prompt with an image attached (`<image …>`) is the person's own.
#[test]
fn a_codex_turns_prompt_is_its_first_words_past_codexs_own() {
    let defense = "Nothing left: merged to origin/main at 4f2a9c1.";
    let landed = format!("The parser is fixed; 12 tests pass.\n\n{defense}");
    let land_turn_at = 14;
    for (label, extra, at) in [
        ("a steered message", cx_user("hurry"), land_turn_at + 1),
        (
            "an environment update",
            cx_user("<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"),
            land_turn_at,
        ),
        (
            "a directory-less AGENTS.md",
            cx_user("# AGENTS.md instructions\n\n<INSTRUCTIONS>\nrules\n</INSTRUCTIONS>"),
            land_turn_at,
        ),
    ] {
        let mut rows = codex_landing(LAND_NUDGE, defense);
        assert_eq!(
            rows[land_turn_at],
            cx_user(LAND_NUDGE),
            "the land act's row"
        );
        rows.insert(at, extra);
        assert_eq!(
            codex_report(&jsonl(&rows), THREAD, DoneBy::Turn).map(|r| r.text),
            Ok(landed.clone()),
            "{label}"
        );
    }
    let mut rows = codex_landing(LAND_NUDGE, defense);
    rows.extend([
        cx_started("t-4", "user"),
        cx(
            "response_item",
            r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"<image name=[Image #1] path=/w/s.png>"},{"type":"input_image","image_url":"data:image/png;base64,AA"},{"type":"input_text","text":"</image>"},{"type":"input_text","text":"still failing, see this"}]}"#,
        ),
        cx_said("Fixed what the screenshot shows.", "final_answer"),
        cx_complete("t-4", "Fixed what the screenshot shows."),
    ]);
    assert_eq!(
        codex_report(&jsonl(&rows), THREAD, DoneBy::Turn).map(|r| (r.text, r.uuid)),
        Ok((
            "Fixed what the screenshot shows.".to_string(),
            "t-4".to_string()
        )),
        "a prompt with an image attached"
    );
}

/// A codex turn has ended once a matching `task_complete` or `turn_aborted` follows its newest
/// `task_started`; a tail with no start has nothing to wait for.
#[test]
fn a_codex_turn_ends_at_its_completion_or_abort() {
    let rows = codex_landing(LAND, "Nothing left: merged.");
    assert!(codex_turn_ended(&jsonl(&rows)));
    let running = &rows[..rows.len() - 1];
    assert!(!codex_turn_ended(&jsonl(&rows[..rows.len() - 3])));
    assert!(
        !codex_turn_ended(&jsonl(running)),
        "the control: no completion yet"
    );
    let mut aborted = rows[..rows.len() - 1].to_vec();
    aborted.push(cx(
        "event_msg",
        r#"{"type":"turn_aborted","turn_id":"t-3"}"#,
    ));
    assert!(codex_turn_ended(&jsonl(&aborted)));
    assert!(codex_turn_ended(""));
}

#[test]
fn a_codex_completion_can_name_a_started_event_in_its_report() {
    let rows = [
        cx_started("t-1", "user"),
        cx_user("Name the start event."),
        cx_said("task_started", "final_answer"),
        cx_complete("t-1", "task_started"),
    ];
    assert!(
        codex_turn_ended(&jsonl(&rows)),
        "the completion's report does not reopen the turn"
    );
}

#[test]
fn codex_message_words_cannot_end_a_running_turn() {
    let mut observed = Vec::new();
    for word in ["task_complete", "turn_aborted"] {
        for message in [
            cx_user(word),
            cx_said(word, "commentary"),
            cx(
                "event_msg",
                &format!(r#"{{"type":"agent_message","message":{}}}"#, js(word)),
            ),
        ] {
            let rows = [cx_started("t-2", "user"), message];
            observed.push(codex_turn_ended(&jsonl(&rows)));
        }
    }
    assert_eq!(
        observed,
        vec![false; 6],
        "user and assistant words leave the actual started turn running"
    );
}

#[test]
fn a_late_codex_terminal_does_not_end_the_active_turn() {
    let mut observed = Vec::new();
    for late in [
        cx_complete("older", "The earlier turn finished."),
        cx("event_msg", r#"{"type":"turn_aborted","turn_id":"older"}"#),
        cx(
            "event_msg",
            r#"{"type":"task_complete","turn_id":"older","root_turn_id":"current"}"#,
        ),
        cx(
            "event_msg",
            r#"{"type":"turn_aborted","turn_id":"older","root_turn_id":"current"}"#,
        ),
    ] {
        let rows = [
            cx_started("older", "user"),
            cx_complete("older", "The earlier turn finished."),
            cx_started("current", "user"),
            cx_user("Keep working on the current task."),
            late,
        ];
        observed.push(codex_turn_ended(&jsonl(&rows)));
    }
    assert_eq!(
        observed,
        vec![false; 4],
        "a previous turn's delayed terminal cannot close the current turn"
    );
}

#[test]
fn a_codex_turn_ends_at_its_own_terminal_despite_later_older_terminals() {
    for terminal in [
        cx_complete("current", "The current task finished."),
        cx(
            "event_msg",
            r#"{"type":"turn_aborted","turn_id":"current"}"#,
        ),
    ] {
        let mut rows = vec![
            cx_started("older", "user"),
            cx_started("current", "user"),
            cx_complete("older", "The earlier turn finished."),
            terminal,
        ];
        assert!(codex_turn_ended(&jsonl(&rows)), "its matching terminal");
        rows.extend([
            cx_complete("older", "The earlier turn finished."),
            cx("event_msg", r#"{"type":"turn_aborted","turn_id":"older"}"#),
        ]);
        assert!(
            codex_turn_ended(&jsonl(&rows)),
            "older terminals cannot reopen a completed current turn"
        );
        rows.push(cx_started("resumed", "alab_autopilot"));
        assert!(
            !codex_turn_ended(&jsonl(&rows)),
            "a subsequent turn still opens normally"
        );
    }
}

#[test]
fn codex_turn_end_preserves_legacy_events_without_turn_ids() {
    let legacy_start = cx("event_msg", r#"{"type":"task_started"}"#);
    let legacy_abort = cx("event_msg", r#"{"type":"turn_aborted"}"#);
    for (start, terminal) in [
        (cx_started("current", "user"), legacy_abort.clone()),
        (
            cx_started("current", "user"),
            cx("event_msg", r#"{"type":"turn_aborted","turn_id":null}"#),
        ),
        (legacy_start.clone(), legacy_abort.clone()),
        (legacy_start.clone(), cx_complete("current", "Finished.")),
    ] {
        assert!(!codex_turn_ended(&start), "an unmatched start is active");
        assert!(
            codex_turn_ended(&jsonl(&[start, terminal])),
            "an absent turn id retains the legacy terminal boundary"
        );
    }
    assert!(codex_turn_ended(&legacy_abort), "a tail without a start");
    assert!(
        !codex_turn_ended(&jsonl(&[legacy_abort, legacy_start])),
        "an older terminal cannot close a later legacy start"
    );
}

#[test]
fn a_codex_xml_task_keeps_its_report_separate_from_an_earlier_landing() {
    let tasks = ScratchDir::new("result", "codex-xml-task");
    let defense = "Nothing left: the lexer is committed and pushed.";
    let report = "The lexer is fixed; 9 tests pass.";
    let mut rows = codex_landing(LAND_NUDGE, "Nothing left: the parser landed.");
    rows.extend([
        cx_started("t-4", "user"),
        cx_user("<task>Fix the lexer.</task>"),
        cx_said(report, "final_answer"),
        cx_complete("t-4", report),
        cx_started("t-5", "alab_autopilot"),
        cx_user(LAND),
        cx_said(defense, "final_answer"),
        cx_complete("t-5", defense),
    ]);
    let text = jsonl(&rows);
    let file = Path::new("/c/sessions/rollout-xml-task.jsonl");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-4",
        Some((file, &text)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(
        std::fs::read_to_string(&snap.path).expect("the captured result"),
        format!("{report}\n\n{defense}"),
        "a new task's landing captures its own report"
    );
}

#[test]
fn an_aborted_codex_task_does_not_reuse_an_earlier_report() {
    let tasks = ScratchDir::new("result", "codex-aborted-task");
    let defense = "Nothing left: the aborted task has no work to land.";
    let mut rows = codex_landing(LAND_NUDGE, "Nothing left: the parser landed.");
    rows.extend([
        cx_started("t-4", "user"),
        cx_user("<task>Fix the lexer.</task>"),
        cx("event_msg", r#"{"type":"turn_aborted","turn_id":"t-4"}"#),
        cx_started("t-5", "alab_autopilot"),
        cx_user(LAND),
        cx_said(defense, "final_answer"),
        cx_complete("t-5", defense),
    ]);
    let text = jsonl(&rows);
    let file = Path::new("/c/sessions/rollout-aborted-task.jsonl");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-5",
        Some((file, &text)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(
        std::fs::read_to_string(&snap.path).expect("the captured result"),
        defense,
        "an empty task remains distinct from the earlier report"
    );
}

/// A codex landing's snapshot names its rollout: `result` answers its text under
/// `source=rollout`, `result full` answers `no-transcript` while that rollout does not read, a
/// missed result whose rollout does not read is not read again, and no rollout at all is said.
#[test]
fn a_codex_snapshot_names_its_rollout_and_answers_it() {
    let tasks = ScratchDir::new("result", "codex");
    let defense = "Nothing left: merged.";
    let text = jsonl(&codex_landing(LAND, defense));
    let file = Path::new("/c/sessions/rollout-x.jsonl");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-2",
        Some((file, &text)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    let want = format!("The parser is fixed; 12 tests pass.\n\n{defense}");
    assert_eq!(
        (snap.by, snap.bytes, snap.unavailable),
        (DoneBy::Check, want.len() as u64, None)
    );
    assert_eq!(
        source_of(&snap.path),
        Some(Source::Rollout {
            thread: THREAD.to_string(),
            turn: "t-3".to_string(),
            rollout: Some(file.to_path_buf()),
        })
    );
    let shown = answer(&snap.path, false);
    assert!(
        shown.starts_with(&format!(
            "OK 3 source=rollout thread={THREAD} turn=t-3 bytes={}\n",
            want.len()
        )),
        "{shown}"
    );
    assert!(shown.ends_with(&format!("{defense}\n")), "{shown}");
    assert_eq!(
        answer(&snap.path, true),
        "ERR result unavailable why=no-transcript\n"
    );
    assert!(reread(&snap.path).is_none());
    let mut missed = codex_landing(LAND, defense);
    missed.truncate(missed.len() - 2);
    missed.push(cx_said("Merging now.", "commentary"));
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-4",
        Some((file, &jsonl(&missed))),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(
        (snap.by, snap.unavailable),
        (DoneBy::Check, Some("no-reply")),
        "a missed landing names the check's rule"
    );
    assert!(
        reread(&snap.path).is_none(),
        "never read again as a transcript"
    );
    assert_eq!(
        answer(&snap.path, false),
        "ERR result unavailable why=no-reply\n"
    );
    let none = snapshot_rollout(&tasks, "s-c1-3", None, THREAD, None, LANDED, DoneBy::Turn)
        .expect("written");
    assert_eq!((none.bytes, none.unavailable), (0, Some("no-transcript")));
    assert_eq!(
        answer(&none.path, false),
        "ERR result unavailable why=no-transcript\n"
    );
    let line = Source::Rollout {
        thread: THREAD.to_string(),
        turn: "t 3".to_string(),
        rollout: None,
    }
    .line();
    assert_eq!(line, format!("source=rollout thread={THREAD} turn=t%203"));
    assert_eq!(
        Source::parse(&line),
        Some(Source::Rollout {
            thread: THREAD.to_string(),
            turn: "t 3".to_string(),
            rollout: None,
        })
    );
}

/// `row` ([`cx`]) as written at `stamp` in place of the fixtures' time.
fn cx_at(row: &str, stamp: &str) -> String {
    row.replacen("2026-10-23T10:00:00.000Z", stamp, 1)
}

/// The fixtures' landing's result ([`codex_landing`]): its task's report, then the defense.
fn codex_landed(defense: &str) -> String {
    format!("The parser is fixed; 12 tests pass.\n\n{defense}")
}

/// A person's turn after a codex landing, begun at `stamp`.
fn codex_person_turn(stamp: &str) -> Vec<String> {
    vec![
        cx_at(&cx_started("t-4", "user"), stamp),
        cx_user("status?"),
        cx_said("All merged.", "final_answer"),
        cx_complete("t-4", "All merged."),
    ]
}

/// A CODEX LANDING WHOSE REPLY WAS NOT YET WRITTEN IS READ AGAIN AT THE NEXT `result`, FOR ITS
/// TURN ALONE: the snapshot keeps the rollout and the turn its tail named last, the reply written
/// after the settle is the result once it is there (written over the snapshot as a rollout's
/// result, never read again), and the full exchange is framed from it. NEGATIVE CONTROL: with
/// nothing written since, the result stays unavailable, and a turn that ends with still no report
/// never takes the person's later turn's reply.
#[test]
fn a_codex_landing_missed_at_the_settle_is_read_again_for_its_turn_alone() {
    let root = ScratchDir::new("result", "codex-reread");
    let tasks = root.join("tasks");
    let file = root.join("rollout.jsonl");
    let defense = "Nothing left: merged.";
    let mut missed = codex_landing(LAND, defense);
    missed.truncate(missed.len() - 2);
    missed.push(cx_said("Merging now.", "commentary"));
    let first = jsonl(&missed);
    std::fs::write(&file, &first).expect("rollout");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-6",
        Some((&file, &first)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!((snap.bytes, snap.unavailable), (0, Some("no-reply")));
    assert_eq!(
        source_of(&snap.path),
        Some(Source::RolloutUnavailable {
            why: "no-reply".to_string(),
            thread: THREAD.to_string(),
            turn: Some("t-3".to_string()),
            rollout: Some(file.clone()),
            home: None,
            landed: Some(LANDED),
        })
    );
    assert_eq!(reread(&snap.path), None, "nothing written since");
    assert_eq!(
        answer(&snap.path, false),
        "ERR result unavailable why=no-reply\n"
    );
    let mut later = missed.clone();
    later.extend([
        cx_said(defense, "final_answer"),
        cx_complete("t-3", defense),
    ]);
    later.extend(codex_person_turn("2026-10-23T10:10:00.000Z"));
    std::fs::write(&file, jsonl(&later)).expect("rollout");
    let again = reread(&snap.path).expect("the landing's reply");
    let want = codex_landed(defense);
    assert_eq!(
        (
            again.report.text.as_str(),
            again.report.uuid.as_str(),
            again.report.session.as_str(),
            again.transcript.as_path(),
            again.from,
        ),
        (
            want.as_str(),
            "t-3",
            THREAD,
            file.as_path(),
            Conversation::Rollout
        )
    );
    assert_eq!(again.bytes(), want.len() as u64);
    let reply = format!(
        "OK 3 source=rollout thread={THREAD} turn=t-3 bytes={}\n{want}\n",
        want.len()
    );
    assert_eq!(again.answer(&snap.path, false), reply, "answered unwritten");
    again.write(&snap.path).expect("written over");
    assert_eq!(answer(&snap.path, false), reply);
    assert_eq!(
        source_of(&snap.path),
        Some(Source::Rollout {
            thread: THREAD.to_string(),
            turn: "t-3".to_string(),
            rollout: Some(file.clone()),
        })
    );
    assert_eq!(reread(&snap.path), None, "a result is never read again");
    let full = answer(&snap.path, true);
    let (head, body) = full.split_once('\n').expect("framed");
    assert_eq!(
        head,
        format!(
            "OK {} source=rollout thread={THREAD} turn=t-3 bytes={} full=1",
            body.split('\n').count() - 1,
            body.len() - 1
        )
    );
    assert!(
        body.starts_with("> Fix the parser.\n") && body.ends_with("> status?\n\nAll merged.\n"),
        "{full}"
    );
    assert_eq!(again.answer(&snap.path, true), full);

    let snap = snapshot_rollout(
        &tasks,
        "s-c1-7",
        Some((&file, &first)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    let mut aborted = missed;
    aborted.push(cx(
        "event_msg",
        r#"{"type":"turn_aborted","turn_id":"t-3"}"#,
    ));
    aborted.extend(codex_person_turn("2026-10-23T10:10:00.000Z"));
    std::fs::write(&file, jsonl(&aborted)).expect("rollout");
    assert_eq!(reread(&snap.path), None, "never a later turn's reply");
    assert_eq!(
        answer(&snap.path, false),
        "ERR result unavailable why=no-reply\n"
    );
}

/// A CODEX LANDING THAT READ NO ROLLOUT FINDS IT LATER BY ITS THREAD AND HOME: the snapshot keeps
/// the thread, the `$CODEX_HOME` the claim named and the landing's time, and the next `result`
/// finds the rollout as the landing's resolver does and reads the newest turn begun by the
/// landing. NEGATIVE CONTROL: no rollout yet, no home, no thread id, or a landing before every
/// turn began finds nothing.
#[test]
fn a_codex_landing_with_no_rollout_finds_it_later_by_its_thread_and_home() {
    let root = ScratchDir::new("result", "codex-found");
    let tasks = root.join("tasks");
    let home = root.join("codex");
    let snapshot = |stem: &str, thread: &str, home: Option<&Path>, landed: i64| {
        snapshot_rollout(&tasks, stem, None, thread, home, landed, DoneBy::Turn).expect("written")
    };
    let snap = snapshot("s-c1-8", THREAD, Some(&home), LANDED);
    assert_eq!((snap.bytes, snap.unavailable), (0, Some("no-transcript")));
    assert_eq!(
        source_of(&snap.path),
        Some(Source::RolloutUnavailable {
            why: "no-transcript".to_string(),
            thread: THREAD.to_string(),
            turn: None,
            rollout: None,
            home: Some(home.clone()),
            landed: Some(LANDED),
        })
    );
    assert_eq!(reread(&snap.path), None, "no rollout yet");
    let day = home.join("sessions").join("2026").join("10").join("20");
    std::fs::create_dir_all(&day).expect("sessions");
    let file = day.join(format!("rollout-2026-10-20T16-36-49-{THREAD}.jsonl"));
    let defense = "Nothing left: merged.";
    let mut rows = codex_landing(LAND, defense);
    rows.extend(codex_person_turn("2026-10-23T10:10:00.000Z"));
    std::fs::write(&file, jsonl(&rows)).expect("rollout");
    // The rollout a conversation named `-` would be found by, so only the thread id rule keeps
    // the `-` landing below from reading it.
    std::fs::write(day.join("rollout-x--.jsonl"), jsonl(&rows)).expect("rollout");
    let found = reread(&snap.path).expect("the rollout its home holds now");
    assert_eq!(
        (
            found.report.text,
            found.report.uuid.as_str(),
            found.transcript,
            found.from
        ),
        (codex_landed(defense), "t-3", file, Conversation::Rollout)
    );
    for (stem, thread, home, landed, label) in [
        ("s-c1-9", THREAD, None, LANDED, "no home"),
        ("s-c1-10", "-", Some(home.as_path()), LANDED, "no thread id"),
        (
            "s-c1-11",
            THREAD,
            Some(home.as_path()),
            LANDED - 3_600_000,
            "a landing before every turn began",
        ),
    ] {
        let snap = snapshot(stem, thread, home, landed);
        assert_eq!(reread(&snap.path), None, "{label}");
        assert_eq!(
            answer(&snap.path, false),
            "ERR result unavailable why=no-transcript\n",
            "{label}"
        );
    }
}

/// A CODEX LANDING THAT KNEW NO CONVERSATION IS FOUND BY THE WORKER'S CLAIM AT THE NEXT `result`
/// ([`reread_with`]): the conversation the claim names now, under the home it names, is read for
/// the newest turn begun by the landing, and the claim is asked only for such a landing (never
/// for one that knew its conversation, nor for a reply not yet written). NEGATIVE CONTROL: no
/// claim, a claim that names no thread id, a conversation begun after the landing, or a
/// conversation other than the one the landing named finds nothing.
#[test]
fn a_codex_landing_that_knew_no_conversation_is_found_by_the_workers_claim() {
    const OTHER: &str = "11111111-2222-4333-8444-555555555555";
    const LATER: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";
    let root = ScratchDir::new("result", "codex-claim");
    let tasks = root.join("tasks");
    let home = root.join("codex");
    let day = home.join("sessions").join("2026").join("10").join("20");
    std::fs::create_dir_all(&day).expect("sessions");
    let defense = "Nothing left: merged.";
    let mut rows = codex_landing(LAND, defense);
    rows.extend(codex_person_turn("2026-10-23T10:10:00.000Z"));
    let file = day.join(format!("rollout-2026-10-20T16-36-49-{THREAD}.jsonl"));
    let begun_later: Vec<String> = rows
        .iter()
        .map(|r| cx_at(r, "2026-10-23T10:10:00.000Z"))
        .collect();
    for (name, rows) in [
        (file.clone(), &rows),
        (
            day.join(format!("rollout-2026-10-20T09-00-00-{OTHER}.jsonl")),
            &rows,
        ),
        (
            day.join(format!("rollout-2026-10-23T10-10-00-{LATER}.jsonl")),
            &begun_later,
        ),
        (day.join("rollout-x--.jsonl"), &rows),
    ] {
        std::fs::write(name, jsonl(rows)).expect("rollout");
    }
    let claim = |thread: &str| {
        let named = (home.clone(), thread.to_string());
        move || Some(named)
    };
    let unasked = || -> Option<(PathBuf, String)> { panic!("the claim was asked") };
    let snapshot = |stem: &str, read: Option<(&Path, &str)>, thread: &str, home: Option<&Path>| {
        snapshot_rollout(&tasks, stem, read, thread, home, LANDED, DoneBy::Turn).expect("written")
    };

    let snap = snapshot("s-c1-20", None, "-", None);
    assert_eq!(
        source_of(&snap.path),
        Some(Source::RolloutUnavailable {
            why: "no-transcript".to_string(),
            thread: "-".to_string(),
            turn: None,
            rollout: None,
            home: None,
            landed: Some(LANDED),
        })
    );
    assert_eq!(reread(&snap.path), None, "no claim");
    for (named, label) in [
        ("-", "no thread id"),
        (LATER, "a conversation begun after the landing"),
    ] {
        assert_eq!(reread_with(&snap.path, claim(named)), None, "{label}");
    }
    let found = reread_with(&snap.path, claim(THREAD)).expect("the claim's conversation");
    let want = codex_landed(defense);
    assert_eq!(
        (
            found.report.text.as_str(),
            found.report.session.as_str(),
            found.report.uuid.as_str(),
            found.transcript.as_path(),
            found.from,
        ),
        (
            want.as_str(),
            THREAD,
            "t-3",
            file.as_path(),
            Conversation::Rollout
        )
    );
    assert_eq!(
        found.answer(&snap.path, false),
        format!(
            "OK 3 source=rollout thread={THREAD} turn=t-3 bytes={}\n{want}\n",
            want.len()
        )
    );

    let named = snapshot("s-c1-21", None, THREAD, None);
    assert_eq!(
        reread_with(&named.path, claim(OTHER)),
        None,
        "a conversation other than the landing's"
    );
    assert_eq!(
        reread_with(&named.path, claim(THREAD)).map(|r| r.report.uuid),
        Some("t-3".to_string()),
        "the landing's own, under the home the claim names"
    );

    let known = snapshot("s-c1-22", None, THREAD, Some(&home));
    assert_eq!(
        reread_with(&known.path, unasked).map(|r| r.report.uuid),
        Some("t-3".to_string()),
        "a landing that knew its conversation"
    );
    let mut missed = codex_landing(LAND, defense);
    missed.truncate(missed.len() - 2);
    missed.push(cx_said("Merging now.", "commentary"));
    let first = jsonl(&missed);
    let lag = snapshot("s-c1-23", Some((&file, &first)), THREAD, None);
    assert_eq!(lag.unavailable, Some("no-reply"));
    assert_eq!(
        reread_with(&lag.path, unasked).map(|r| r.report.uuid),
        Some("t-3".to_string()),
        "a reply not yet written at the landing"
    );
}

/// `RESULT FULL` ON A CODEX WORKER FRAMES THE WHOLE EXCHANGE FROM ITS TASK'S PROMPT, as a
/// transcript's is framed: each prompt and each message steered into a turn quoted, past the
/// context Codex adds itself, and every message's words, to the rollout's end. NEGATIVE CONTROL:
/// a rollout result with no rollout named, or a screen's, reads no transcript, and a rollout read
/// whole with no prompt in it has none.
#[test]
fn result_full_on_a_codex_worker_frames_the_whole_exchange_from_its_task() {
    let root = ScratchDir::new("result", "codex-full");
    let tasks = root.join("tasks");
    let file = root.join("rollout.jsonl");
    let defense = "Nothing left: merged.";
    let text = jsonl(&codex_landing(LAND, defense));
    std::fs::write(&file, &text).expect("rollout");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-12",
        Some((&file, &text)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    let quoted = |t: &str| {
        t.lines()
            .map(|l| {
                if l.is_empty() {
                    ">".to_string()
                } else {
                    format!("> {l}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let exchange = [
        quoted("Fix the parser."),
        "Reading the parser first.".to_string(),
        quoted("Also run the suite."),
        "The parser is fixed; 12 tests pass.".to_string(),
        quoted(LAND.trim()),
        "Committed it and pushed; merging next.".to_string(),
        quoted(LAND.trim()),
        defense.to_string(),
    ]
    .join("\n\n");
    assert_eq!(
        answer(&snap.path, true),
        format!(
            "OK {} source=rollout thread={THREAD} turn=t-3 bytes={} full=1\n{exchange}\n",
            exchange.split('\n').count(),
            exchange.len()
        )
    );
    assert!(
        !answer(&snap.path, true).contains("environment_context"),
        "no context Codex adds"
    );

    let unnamed = tasks.join("s-c1-13.result.md");
    std::fs::write(&unnamed, codex_landed(defense)).expect("result");
    let line = Source::Rollout {
        thread: THREAD.to_string(),
        turn: "t-3".to_string(),
        rollout: None,
    }
    .line();
    std::fs::write(sidecar_of(&unnamed), format!("{line}\n")).expect("sidecar");
    assert!(answer(&unnamed, false).starts_with("OK 3 source=rollout "));
    assert_eq!(
        answer(&unnamed, true),
        "ERR result unavailable why=no-transcript\n"
    );
    let screen =
        snapshot_screen(&tasks, "s-c1-14", "agy", "Fixed it.", DoneBy::Turn).expect("written");
    assert_eq!(
        answer(&screen.path, true),
        "ERR result unavailable why=no-transcript\n"
    );
    let promptless = jsonl(&[
        cx_started("t-1", "user"),
        cx_user("<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"),
        cx_said("Nothing to do.", "final_answer"),
        cx_complete("t-1", "Nothing to do."),
    ]);
    let bare = root.join("bare.jsonl");
    std::fs::write(&bare, &promptless).expect("rollout");
    let snap = snapshot_rollout(
        &tasks,
        "s-c1-15",
        Some((&bare, &promptless)),
        THREAD,
        None,
        LANDED,
        DoneBy::Turn,
    )
    .expect("written");
    assert_eq!(snap.unavailable, None);
    assert_eq!(
        answer(&snap.path, true),
        "ERR result unavailable why=no-prompt\n"
    );
}

/// `RESULT FULL` ON A CODEX WORKER IS BOUNDED AS A TRANSCRIPT'S IS: an exchange past
/// [`RESULT_MAX_CHARS`] keeps its end under a first line that names the rollout, and a rollout
/// read from past its start says so under that line though its exchange fits. NEGATIVE CONTROL:
/// an exchange of exactly the bound, read whole, is shown whole with no cut line.
#[test]
fn result_full_on_a_codex_worker_is_capped_and_names_the_rollout_it_cut() {
    let root = ScratchDir::new("result", "codex-cap");
    let tasks = root.join("tasks");
    let file = root.join("rollout.jsonl");
    let full_of = |stem: &str, rows: &[String]| {
        let text = jsonl(rows);
        std::fs::write(&file, &text).expect("rollout");
        let snap = snapshot_rollout(
            &tasks,
            stem,
            Some((&file, &text)),
            THREAD,
            None,
            LANDED,
            DoneBy::Turn,
        )
        .expect("written");
        assert_eq!(snap.unavailable, None, "{stem}");
        answer(&snap.path, true)
    };
    let task = |said: &str| {
        vec![
            cx_started("t-1", "user"),
            cx_user("Fix it."),
            cx_said(said, "final_answer"),
            cx_complete("t-1", said),
        ]
    };
    let head = |lines: usize, bytes: usize| {
        format!("OK {lines} source=rollout thread={THREAD} turn=t-1 bytes={bytes} full=1")
    };
    let cut = format!("… earlier turns cut; the rollout: {}", file.display());
    let prompt = "> Fix it.\n\n";

    let line = "é".repeat(99);
    let long: Vec<String> = (0..300).map(|i| format!("{i:03}{line}")).collect();
    let long = long.join("\n");
    assert!(long.chars().count() > RESULT_MAX_CHARS);
    let full = full_of("s-c1-24", &task(&long));
    let mut lines: Vec<&str> = full.lines().collect();
    assert_eq!(
        lines.remove(0),
        head(lines.len(), prompt.len() + long.len())
    );
    assert_eq!(lines.remove(0), cut);
    let shown: usize = lines.iter().map(|l| l.chars().count() + 1).sum();
    assert_eq!(
        shown - 1,
        RESULT_MAX_CHARS,
        "exactly the bound, newlines counted"
    );
    assert!(full.ends_with(&format!("\n299{line}\n")), "the end is kept");

    let exact = "x".repeat(RESULT_MAX_CHARS - prompt.len());
    assert_eq!(
        full_of("s-c1-25", &task(&exact)),
        format!("{}\n{prompt}{exact}\n", head(3, RESULT_MAX_CHARS))
    );

    let pad = "x".repeat(usize::try_from(TRANSCRIPT_READ_BYTES).expect("a size"));
    let mut past = vec![cx(
        "response_item",
        &format!(r#"{{"type":"reasoning","summary":[],"encrypted_content":"{pad}"}}"#),
    )];
    past.extend(task("Fixed it."));
    let said = format!("{prompt}Fixed it.");
    assert_eq!(
        full_of("s-c1-26", &past),
        format!("{}\n{cut}\n{said}\n", head(4, said.len()))
    );
}

/// A SNAPSHOT AN OLDER BUILD WROTE READS AND ANSWERS AS IT DID, AND AN OLDER READER READS A NEW
/// ONE AS IT READ ITS OWN: a codex miss an older build kept (`source=none why=<word>`, nothing
/// more) is a plain unavailable result, answered by its word and never read again; a rollout's
/// missing result reads back whole; and, past the `thread=` an older reader takes no field from,
/// it is that same plain result (an older reader's `source=none` fields are `why=`, `turn=` and
/// `transcript=`), with no transcript to read again. NEGATIVE CONTROL: a why that is no word
/// reads as no line.
#[test]
fn an_older_codex_snapshot_reads_as_it_did_and_a_new_one_reads_in_an_older_reader() {
    let tasks = ScratchDir::new("result", "codex-old");
    for (stem, why) in [("s-c1-16", "no-reply"), ("s-c1-17", "no-transcript")] {
        let old = tasks.join(format!("{stem}.result.md"));
        std::fs::write(&old, "").expect("result");
        std::fs::write(sidecar_of(&old), format!("source=none why={why}\n")).expect("sidecar");
        assert_eq!(
            source_of(&old),
            Some(Source::Unavailable {
                why: why.to_string(),
                transcript: None,
                turn: None,
            })
        );
        assert_eq!(reread(&old), None, "{why}");
        assert_eq!(
            answer(&old, false),
            format!("ERR result unavailable why={why}\n")
        );
        assert_eq!(
            answer(&old, true),
            format!("ERR result unavailable why={why}\n")
        );
    }
    let missed = Source::RolloutUnavailable {
        why: "no-reply".to_string(),
        thread: THREAD.to_string(),
        turn: Some("t 3".to_string()),
        rollout: Some(PathBuf::from("/Users//a b/.codex/sessions/rollout-x.jsonl")),
        home: Some(PathBuf::from("/Users//a b/.codex")),
        landed: Some(LANDED),
    };
    let bare = Source::RolloutUnavailable {
        why: "no-transcript".to_string(),
        thread: "-".to_string(),
        turn: None,
        rollout: None,
        home: None,
        landed: None,
    };
    assert_eq!(
        bare.line(),
        "source=none why=no-transcript thread=-",
        "the fields it has"
    );
    for source in [missed.clone(), bare] {
        assert_eq!(Source::parse(&source.line()), Some(source.clone()));
    }
    let line = missed.line();
    let older: Vec<&str> = line
        .split(' ')
        .filter(|t| !t.starts_with("thread="))
        .collect();
    assert_eq!(
        Source::parse(&older.join(" ")),
        Some(Source::Unavailable {
            why: "no-reply".to_string(),
            transcript: None,
            turn: Some("t 3".to_string()),
        }),
        "{line}"
    );
    assert_eq!(
        Source::parse(&format!("source=none why=Not%20a%20word thread={THREAD}")),
        None
    );
}
