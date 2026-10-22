// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worker's result read from its transcript: the two rules, the rows that
//! are no one's words, the snapshot and the `result [full]` reply. The rows are
//! the shapes Claude Code 2.1.299 writes (the lab fixture
//! `supervise/fixtures/claude-2.1.299-transcript-session-limit-cancelled-then-person-turn.jsonl`),
//! cut to the keys anyone reads plus a few that nobody does.

use super::*;
use crate::supervise::ScratchDir;
use crate::supervise::policy::turn_end::{
    DONE_CHECK, DONE_CHECK_MARKER_BEFORE, LAND, LAND_ANSWER, LAND_NUDGE, RETIRED_LAND_NUDGES,
    RETIRED_LANDS, done_check_text,
};

const SESSION: &str = "0b2299fa-2e8d-447e-a9b8-082171298ccd";

/// A JSON string literal.
fn js(t: &str) -> String {
    aterm_json::to_string(&Value::String(t.to_string())).expect("json")
}

/// A typed user row: a person's prompt, a manager's, or the harness's own.
fn user(uuid: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":null,"isSidechain":false,"promptId":"7a223c05","type":"user","message":{{"role":"user","content":{}}},"uuid":"{uuid}","timestamp":"2026-10-15T17:28:25.422Z","permissionMode":"auto","origin":{{"kind":"human"}},"promptSource":"typed","userType":"external","entrypoint":"cli","cwd":"/private/tmp/at.0000/proj","sessionId":"{SESSION}","version":"2.1.299","gitBranch":"HEAD"}}"#,
        js(text)
    )
}

/// A tool's result, which opens no turn.
fn tool_result(uuid: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_1","type":"tool_result","content":"ATERM-TASK-DONE\n12 passed","is_error":false}}]}},"uuid":"{uuid}","timestamp":"2026-10-15T17:28:26.000Z","toolUseResult":{{"stdout":"12 passed"}},"sessionId":"{SESSION}","version":"2.1.299"}}"#
    )
}

/// One assistant row of message `id`: its `blocks` (a JSON array).
fn said(uuid: &str, id: &str, blocks: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"message":{{"id":"{id}","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":{blocks},"stop_reason":"end_turn","usage":{{"input_tokens":22682,"output_tokens":12}}}},"requestId":"req_lab","type":"assistant","uuid":"{uuid}","timestamp":"2026-10-15T17:28:27.463Z","effort":"medium","sessionId":"{SESSION}","version":"2.1.299","gitBranch":"HEAD"}}"#
    )
}

/// A `text` block.
fn text(t: &str) -> String {
    format!(r#"{{"type":"text","text":{}}}"#, js(t))
}

/// A `tool_use` block.
fn tool_use() -> String {
    r#"{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo ATERM-TASK-DONE"}}"#
        .to_string()
}

/// One assistant row of message `id` holding the one text block `t`.
fn says(uuid: &str, id: &str, t: &str) -> String {
    said(uuid, id, &format!("[{}]", text(t)))
}

/// The system row that ends a turn (no one's words).
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

/// The task, a tool's call and result, then a report: the turn's work.
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

/// A PLAIN TURN: its final assistant message, NO TOKEN REQUIRED — the
/// retired token's lines dropped (Markdown punctuation around it too) where
/// a worker still writes it, the message the same without one — stamped
/// with the row it came from. The token inside the tool's output and the
/// tool call is no report. NEGATIVE CONTROL: a turn whose only rows are a
/// tool's call and its result has no reply yet (`no-reply`).
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
    // NEGATIVE CONTROL: no words yet.
    let rows = [
        user("u-task", "Fix the parser and run the suite."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("no-reply"));
}

/// THE RETIRED TOKEN ALONE AT THE END: the report, a check of the work,
/// then a bare `ATERM-TASK-DONE` (a worker taught by an older build) — the
/// turn's last earlier message with words is the result, stamped with its
/// own row. NEGATIVE CONTROL: a turn whose only words are the token is
/// `empty`.
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
    // NEGATIVE CONTROL: the token is all the turn said.
    let rows = [
        user("u-task", "Fix it."),
        says("a-marker", "msg_1", "ATERM-TASK-DONE"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("empty"));
}

/// The task, a `keep going`, the report, then the harness's done check and
/// `reply`'s rows: the conversation a worker the check ended writes.
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

/// DONE THROUGH THE DONE CHECK: the worker's REPORT — the final message of
/// the turn before the check — then what the check's turn says beyond its
/// done, as a plain turn is read: its final message, token lines dropped,
/// or where that is the retired token alone the turn's last earlier
/// message with words (a report, a commit, then a bare token). A worker
/// that verified or fixed inside the check's turn has its final word
/// returned (the fact line's promise), named by its row. A `keep going`
/// before the report opens the report's own turn. NEGATIVE CONTROLS: a
/// reply that is the token alone, or `DONE`, with nothing before it in the
/// turn, adds nothing — the report alone, by its row; a check whose turn
/// before it said nothing, answered bare, is no report.
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
    // NEGATIVE CONTROL: nothing before the check, and a bare marker.
    let rows = [
        user("u-task", "Fix it."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
        user("u-check", DONE_CHECK),
        says("a-reply", "msg_2", "ATERM-TASK-DONE"),
    ];
    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-report"));
    // The same with words in the reply: they are the result.
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

/// A CHECK'S REPLY NOT WRITTEN YET IS THE LAG THE RESULT READS AGAIN: the
/// check turn holds no reply with text, or only a preamble before its tool
/// calls (Claude Code writes its final row a moment after its screen shows
/// it, and the policy ends a task on a check only on a reply that says
/// done), so the snapshot is `no-reply` naming the check's row, and once
/// the reply is in the transcript `result` reads that turn by the check's
/// rule — the report, then the reply's words. NEGATIVE CONTROL: a reply
/// that never comes stays the miss, the preamble never the result.
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
        // NEGATIVE CONTROL: still no reply.
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

/// THE CHECK AS THE HARNESS TYPES IT: with `[harness] rules_file` set it is
/// the check and the standing rules after it, and is the check all the
/// same — the report before it is the result, the reply's words after it
/// where it said more than the token. An earlier check answered with a
/// bare done (a loop started again types its own) is passed over for the
/// report before it. NEGATIVE CONTROLS: a newest turn that is no check is
/// `no-check`, never the plain turn's read of it; a check answered with
/// work is the report the next check follows.
#[test]
fn a_done_check_with_the_standing_rules_is_the_check() {
    // Read every form the builder actually emits, including the fixed
    // irreversible-action safeguard before optional standing rules. Older
    // conversations use the preceding check text with the same suffixes.
    let completion = "All work is complete. All parser tests passed.";
    for rules in [None, Some("leave unrelated files alone")] {
        for irreversible in [false, true] {
            let current = done_check_text(rules, irreversible);
            for check in [
                current.clone(),
                current.replacen(DONE_CHECK, DONE_CHECK_MARKER_BEFORE, 1),
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
                    // NEGATIVE CONTROL: an older check with someone's words
                    // after it, and today's (whose suffixes vary, so any
                    // words after it are its own) after someone's words.
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
    // Two checks, the first answered bare.
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
    // NEGATIVE CONTROL: a check answered with work is that work's report.
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
    // NEGATIVE CONTROL: no check newest.
    let rows = worked(&[says("a-report", "msg_2", "Fixed.\nATERM-TASK-DONE")]);
    assert_eq!(report(&jsonl(&rows), DoneBy::Check), Err("no-check"));
}

/// A worker already checked before an update keeps its result. Earlier
/// checks answered with a bare done are passed over, just as current checks
/// are; ordinary completion prose supplies the final verification details.
#[test]
fn a_legacy_done_check_still_recovers_the_workers_result() {
    let ruled = done_check_text(Some("leave unrelated files alone"), false).replacen(
        DONE_CHECK,
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

/// A CHECK NEWEST IS READ AS ONE, WHATEVER RULE IS ASKED: a loop that
/// starts at a point another loop's check answered (another supervisor's
/// hand-back, the harness's fault restart, a seamless update's successor)
/// follows no act of its own there, and its policy hands the
/// plain turn's rule — the reader still gives the report before the check,
/// then the reply's words where it said more than the token, and says it
/// read by the check's ([`Snapshot::by`]). An earlier
/// check answered with a done the policy ends the task on (`Yes, it's all
/// done.`), asked again by a loop started after it, is passed over, its
/// words kept after the report. NEGATIVE CONTROL: a newest turn that is no
/// check is read by the plain turn's rule, and says so.
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
    // An earlier check answered with a done phrase, then a loop's own.
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
    // NEGATIVE CONTROL: no check newest.
    let text = jsonl(&worked(&[says(
        "a-report",
        "msg_2",
        "Fixed.\nATERM-TASK-DONE",
    )]));
    let snap = snapshot(&tasks, "s-1-9", Some((file, &text)), DoneBy::Turn).expect("written");
    assert_eq!((snap.by, snap.bytes), (DoneBy::Turn, "Fixed.".len() as u64));
}

/// The task, its report, then Land's instruction answered with progress
/// and `act` (a nudge, Land's answer to a question) answered with `reply`'s
/// rows: the conversation a landing writes.
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

/// THE RESULT IS THE LANDING: the newest turn a land act opened (a nudge,
/// Land's answer to a question, the instruction itself — today's words and
/// every older build's), its reply the defense that landed the task: the
/// worker's REPORT, the final message of the turn before the episode's land
/// acts (each passed over: their replies are the landing's progress), then
/// the defense's words — read by the check's rule whatever rule is asked,
/// no token anywhere. NEGATIVE CONTROLS: a land act's reply that is no done
/// (more progress) is `no-reply`, the lag read again; so is a defense
/// written as a preamble before a tool's call (its turn goes on); and the
/// same defense after a person's prompt is a plain turn, its words alone.
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
    // NEGATIVE CONTROL: the nudge answered with more progress.
    let rows = landing(
        LAND_NUDGE,
        &[says(
            "a-more",
            "msg_3",
            "Merging now; the suite runs on main.",
        )],
    );
    assert_eq!(report(&jsonl(&rows), DoneBy::Turn), Err("no-reply"));
    // NEGATIVE CONTROL: a defense before a tool's call, its turn going on.
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
    // NEGATIVE CONTROL: the same words after a person's prompt.
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

/// A LANDING REPLACES THE ONE BEFORE IT ([`prune_results`]): beside the
/// result just written, every other `<gsid>-<n>.result.md` and its sidecar
/// of the same session goes. NEGATIVE CONTROLS: the new pair stays, and
/// another session's pair — one whose sid only begins with this one's, and
/// a file of this sid's whose suffix is no revision — is never touched.
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
    // Nothing else of the session: nothing goes.
    assert_eq!(prune_results(&tasks, "s-ab12-3"), 0);
}

/// A pid that WAS a process and is not one now: the transcript's resolver
/// reads the live process's directory, so a pid some process on this
/// machine holds would read its directory instead.
fn dead_pid() -> u32 {
    // Windows has no `true`: cmd.exe exits as soon as it starts.
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

/// THE CHECK'S REPLY THAT VERIFIED AGAIN: the check asks for "finished and
/// verified", and a reply that runs the suites again carries their output
/// in its tool rows — more than the live upgrade's 256 KiB tail
/// ([`super::super::upgrade_drive::TAIL_BYTES`]). The transcript the done
/// edge reads ([`super::super::upgrade_drive::process_transcript`],
/// [`TRANSCRIPT_READ_BYTES`]) still holds the check's prompt and the report
/// before it. NEGATIVE CONTROL: the same transcript read as that tail is
/// `no-check`.
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
    // NEGATIVE CONTROL: the upgrade's tail.
    let tail = process_transcript_tail(None, home_s, None, pid, start).expect("found");
    assert_eq!(report(&tail, DoneBy::Check), Err("no-check"));
}

/// THE LIVE UPGRADE'S ANNOUNCEMENT IS THE HARNESS'S TURN: its READY, typed
/// into the worker after its done (or between its report and the done
/// check), is read past by both rules. NEGATIVE CONTROL: the relaunch's
/// continuation (`[aterm harness] Upgraded: …`) is the worker's work
/// resumed, and its answer is the report.
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
    // Between the report and the check.
    let mut rows = worked(&[says("a-report", "msg_2", "Fixed the parser.")]);
    rows.extend([
        user("u-ready", &notice),
        says("a-ready", "msg_r", "READY"),
        user("u-check", DONE_CHECK),
        says("a-reply", "msg_3", "ATERM-TASK-DONE"),
    ]);
    let got = report(&jsonl(&rows), DoneBy::Check).expect("a report");
    assert_eq!(got.uuid, "a-report");
    // NEGATIVE CONTROL: the continuation.
    let mut rows = worked(&[says("a-report", "msg_2", "Half of it.")]);
    rows.extend([
        user("u-carry", "[aterm harness] Upgraded: carry on"),
        says("a-rest", "msg_3", "The rest is in.\nATERM-TASK-DONE"),
    ]);
    let got = report(&jsonl(&rows), DoneBy::Turn).expect("a report");
    assert_eq!(got.uuid, "a-rest");
}

/// ONE MESSAGE, MANY ROWS: Claude Code writes one row per content block, all
/// under the message's id (a thinking block, then text, then more text), and
/// the reader takes them as the one message they are — its text blocks in
/// order, a blank line between two, its uuid the row with the last text.
/// NEGATIVE CONTROL: the same rows under two ids are two messages, and the
/// final one alone is the report.
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

/// THE ROWS THAT ARE NO ONE'S WORDS change nothing: a subagent's
/// (`isSidechain`) report, Claude Code's own `<synthetic>`
/// row (`No response requested.`), an API error's row
/// (`isApiErrorMessage`), an `isMeta` prompt (a skill's expansion), a
/// compaction summary and a `/usage` probe's tagged rows — each written
/// after the report. NEGATIVE CONTROL: the subagent's row read as the main
/// chain's would be the report.
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
    // NEGATIVE CONTROL: the subagent's row on the main chain.
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

/// A SHAPE THE READER DOES NOT KNOW IS SAID, NEVER GUESSED: a newest turn
/// with no answer yet (`no-reply`), the retired token alone (`empty`), a
/// read by the check's rule whose newest turn is no check (`no-check`), and
/// no transcript at all (`no-transcript`) — each snapshotted as an empty
/// result with its word, which `result` answers as `ERR result unavailable
/// why=<word>`.
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
    // No sidecar at all: no snapshot was ever made.
    assert_eq!(
        answer(&tasks.join("s-9-1.result.md"), false),
        "ERR result unavailable why=no-snapshot\n"
    );
}

/// THE SNAPSHOT IS THE RESULT FROM THE DONE EDGE ON: its file and sidecar are
/// written once, 0600 under a 0700 directory, and `result` answers from them
/// with the stamp in its header and the text alone in its lines — and goes
/// on answering the same once the transcript has grown past the done edge
/// (the live upgrade's READY notice and its answer, a person's question and
/// its answer), which only `result full`, read now, shows.
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
    // The transcript grows past the done edge.
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

/// THE REPLY IS BOUNDED: a result longer than [`RESULT_MAX_CHARS`] prints
/// that many characters, then one line naming the file that holds it all;
/// the header's `bytes=` is the whole result's. `result full` keeps the END
/// of a long thread and says where the rest is. NEGATIVE CONTROL: a result
/// of exactly the bound prints whole, with no such line.
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
    // NEGATIVE CONTROL: exactly the bound.
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

/// `result full` OF A TRANSCRIPT WITH NO PROMPT: a tail that began inside
/// the task's turns is all it holds, shown behind the line that says earlier
/// turns are cut ([`thread`] says it began at no prompt); a whole transcript
/// with no prompt at all is `why=no-prompt`, never `ERR no task`.
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

/// The sidecar line reads back as written, a path with a space and a `%`
/// included; a line it never writes, or a why that is no word, reads as
/// nothing. A stem that is no file name's is refused before anything is
/// written.
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
            program: "gemini".to_string(),
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

/// A SCREEN-READ LANDING'S RESULT OPENS WITH ITS REPORT ([`with_report`]):
/// the words read where the Land instruction was typed, a blank line, then
/// the landing's own. NEGATIVE CONTROLS: no report, a blank one, and one
/// that is the landing's own words leave the landing's words alone.
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

/// A SCREEN'S RESULT (agy, Muse Code, Gemini CLI: no transcript aterm
/// reads, [`snapshot_screen`]): the landing's words the screen's reader read
/// are the snapshot, its sidecar `source=screen program=<agent>`, and
/// `result` answers them under that stamp, cut at [`RESULT_MAX_CHARS`] as a
/// transcript's result is; `result full` has no transcript to read.
/// NEGATIVE CONTROLS: words that are only blanks are no result
/// (`no-words`); the same done with neither words nor a transcript is
/// `no-transcript`, as before.
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
    for program in ["gemini", "agy", "muse"] {
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
    // The controls.
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

/// EACH SCREEN-READ HARNESS'S LANDING IS READ OFF ITS OWN SCREEN: Gemini
/// CLI's, agy's and Muse Code's measured end of a turn, read by its reader
/// as the turn-end loop reads a point (`TurnEndReading::of`), carries its
/// reply whole, and the words the done edge snapshots ([`screen_said`], the
/// loop's `fold_point`) end with that reply — what a manager's `result`
/// returns for a worker of theirs. NEGATIVE CONTROL: a shell's prompt, read
/// by the same readers, carries no words, so no result is made of it.
#[test]
fn each_screen_harnesss_landing_words_are_read_off_its_screen() {
    use crate::supervise::policy::turn_end::{TurnEndReading, screen_alone};
    use aterm_phase::Program;
    let shell = aterm_phase::prompt::fixtures::rows(&["% "]);
    for (program, text) in [
        (Program::Gemini, aterm_phase::gemini::fixtures::END_OF_TURN),
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

/// The task's turn as the done edge can read it before Claude Code wrote its
/// end: the prompt, a call with no words and its result, no reply yet (the
/// `no-reply` three of five real done edges read, 2.1.301).
fn lagging() -> Vec<String> {
    vec![
        user("u-task", "Fix the parser and run the suite."),
        said("a-call", "msg_1", &format!("[{}]", tool_use())),
        tool_result("u-result"),
    ]
}

/// Whether the newest turn of the transcript tail `jsonl` has ended: its
/// `turn_duration` row, or the next turn's start, after the row that began
/// it ([`end_of`] of [`Opener::of`] with no done point to place it).
fn turn_ended(jsonl: &str) -> bool {
    end_of(jsonl, &Opener::of(jsonl, None)).ended
}

/// THE NEWEST TURN HAS ENDED at Claude Code's `turn_duration` row after its
/// newest prompt — a tool's result inside the turn opens none. NEGATIVE
/// CONTROLS: the reply with no end row yet, the prompt alone, a new prompt
/// after the end, a subagent's end row, a prompt that names the row, and an
/// end row still half written are no end.
#[test]
fn the_newest_turn_has_ended_at_its_end_row_after_its_prompt() {
    let reply = says("a-report", "msg_2", "Fixed.\nATERM-TASK-DONE");
    assert!(turn_ended(&jsonl(&worked(std::slice::from_ref(&reply)))));
    // A tail inside a long turn: no prompt in it, its end row there.
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

/// THE REPLY WRITTEN AFTER THE DONE EDGE'S READ IS WAITED FOR (F1, measured
/// on 2.1.301): the transcript as the edge read it holds no reply; read
/// again, it is still behind, then written to its end row — the tail is
/// then read whole and its report is the result. The reads are the end's
/// until the end row is there, then the whole tail's; one pause between.
/// NEGATIVE CONTROL: the edge's own read is `no-reply`. A first read that
/// holds the end row reads nothing again and waits for nothing; a
/// transcript that can no longer be read leaves the first read, unwaited.
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
    // NEGATIVE CONTROL: the done edge's own read.
    assert_eq!(report(&first, DoneBy::Turn), Err("no-reply"));
    // The end row in the first read: nothing read again.
    let at_once = settle_with(
        whole.clone(),
        None,
        |_| -> Option<String> { panic!("read again") },
        || panic!("waited"),
    );
    assert_eq!(at_once.tail, whole);
    assert_eq!((at_once.waits, at_once.ended), (0, true));
    // A transcript no longer read: the first read stands.
    let gone = settle_with(first.clone(), None, |_| None, || panic!("waited"));
    assert_eq!(gone.tail, first);
    assert_eq!((gone.waits, gone.ended), (0, false));
}

/// PAST THE BOUND THE SNAPSHOT IS READ AS IT STANDS — fail closed as before
/// the wait: a reply never written is `no-reply`, `result` says so, and the
/// sidecar keeps the turn's user row for a later `result` to read again.
/// The file-backed wait ([`settled`]) stops at the loop's stop without a
/// pause, and reads a reply already on disk at once.
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
    // The file-backed wait: stopped, it reads once and waits for nothing.
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

/// `result` READS A LAGGED TURN AGAIN — THAT TURN, NEVER A LATER ONE: a
/// snapshot that missed its turn's reply (`no-reply`) names the turn's user
/// row; once the reply is written, [`reread`] finds it in that turn and
/// answers it, and written over the snapshot it is the result from then on
/// (read again no more). NEGATIVE CONTROLS: nothing written since reads
/// nothing; a turn that ended with no reply, followed by a person's turn
/// answered with a done, gives nothing — the later turn's reply is never
/// the task's; a miss no lag explains (`no-check`) and an old sidecar that
/// names no turn are never read again.
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
    // Claude Code writes the reply, and a person's turn follows it.
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

    // NEGATIVE CONTROL: the task's turn ended with no reply; a later one's.
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
    // NEGATIVE CONTROL: a miss no lag explains.
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
    // NEGATIVE CONTROL: a sidecar from before the turn was kept.
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

/// A background command's `<task-notification>` as Claude Code 2.1.301
/// writes it (the real smoke's manager, 2026-10-16): a user row with words
/// that starts a turn of Claude Code's own, though no prompt this reader
/// reads.
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

/// Claude Code's own continuation once a usage limit resets: an `isMeta`
/// user row that starts a turn (seen in this machine's transcripts).
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

/// A manager's task as the real 2.1.301 smoke wrote it, cut where its done
/// edge can read it: the prompt, the workers started and their wait put in
/// the background, a turn ended to wait (`turn_duration`), the background
/// command's notification, the results read — and the final reply, which
/// Claude Code writes about 20 ms before its end row, not yet there.
fn woken() -> Vec<String> {
    woken_by(notification("u-woke"))
}

/// [`woken`], the turn after the wait started by `opener`.
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

/// The manager's final reply, then its turn's end row.
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

/// A TURN A `<task-notification>` STARTED IS WAITED FOR TOO: the manager's
/// flow (a background `await task`, the turn ended, woken by the command's
/// exit) and any worker that used a background Bash. The done edge's read
/// holds an end row — the turn ended to wait — but none after the
/// notification, so it is read again until the final reply and its end row
/// are there, and the reply is the result. NEGATIVE CONTROLS: the read cut
/// before the notification HAS ended (the end row the wait must not stop
/// at), and the edge's own read gives the miss F1 measured (`no-reply`:
/// the text before the last tool call is no report). Claude Code's own
/// continuation after a usage limit reset (an `isMeta` row) starts a turn
/// the same way.
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

/// THE WAIT IS FOR THE DONE EDGE'S TURN, NEVER ONE BEGUN SINCE: read
/// again, the transcript holds the task's reply and end row and then a
/// person's turn answered with a done. The tail stops at the task's end
/// row, so the snapshot is the task's report. NEGATIVE CONTROL: the newest
/// turn of that same read is the person's (`Docs too.`). A task's turn
/// that never ended, a prompt typed after it, is read up to that prompt:
/// `no-reply`, its sidecar naming the task's row, and `result` never takes
/// the person's reply for it.
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
    // NEGATIVE CONTROL: the read's own newest turn.
    assert_eq!(
        report(&later, DoneBy::Turn).map(|r| r.text),
        Ok("Docs too.".to_string())
    );

    // The task's turn never ended; a person typed after it.
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

/// `result` READS A NOTIFICATION'S TURN AGAIN UP TO ITS END: past the bound
/// the snapshot of the manager's woken turn is `no-reply` (its last message
/// a preamble before a tool's call), its sidecar naming the notification's
/// row. Once the reply is written, `result` finds it, though another
/// notification woke the manager since and its reply says done too.
/// NEGATIVE CONTROL: the woken turn ended with no report, and the later
/// notification's reply is never taken.
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
    // NEGATIVE CONTROL: the woken turn ended with no report.
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

/// `row` as Claude Code wrote it at `at` (`2026-10-15T17:28:28.040Z`): its
/// one `timestamp` set.
fn stamped(row: &str, at: &str) -> String {
    let (head, rest) = row.split_once(r#""timestamp":""#).expect("a stamped row");
    let (_, tail) = rest.split_once('"').expect("the stamp's end");
    format!(r#"{head}"timestamp":"{at}"{tail}"#)
}

/// `at` in unix milliseconds: when a test's screen showed its done point.
fn shown(at: &str) -> Option<i64> {
    Some(stamp_ms(at).expect("a stamp"))
}

/// The task's turn ended with a done, its end row written at
/// 17:28:28.000; then a background shell the worker left ended, and its
/// `<task-notification>` (40 ms later, the quickest this machine's
/// transcripts hold is 10 ms) woke Claude Code on a turn of its own — all
/// before the done edge read the transcript (P5 of the rl review): the
/// shape the retired `finished_over_background` published on, with the
/// shell quick to end (a result is still read from it). Its screen showed
/// the done at 17:28:27.990, before the end row (the screen leads the
/// transcript).
fn done_then_woken() -> Vec<String> {
    let mut rows = worked(&[says(
        "a-report",
        "msg_2",
        "Fixed the parser; the build runs in the background.\nATERM-TASK-DONE",
    )]);
    rows.push(stamped(&notification("u-bg"), "2026-10-15T17:28:28.040Z"));
    rows
}

/// When [`done_then_woken`]'s screen showed its done.
const DONE_AT_THE_MARKER: &str = "2026-10-15T17:28:27.990Z";

/// A NOTIFICATION WRITTEN AFTER THE SCREEN SHOWED THE DONE STARTS NO TURN
/// THE SNAPSHOT WAITS FOR OR TAKES: the done edge's turn is the one that
/// ended with the done, already ended in the first read — nothing is read
/// again — and the tail stops at its end row, so the wake's own reply
/// (written by the next read) is never the result. A person's prompt
/// written after the done showed is no turn of it either, and the check's
/// rule is placed the same. NEGATIVE CONTROLS: with no done point to place
/// it, the newest turn is the wake's, waited for, and the wake's reply would
/// be read for the task's — what the snapshot read when the opener was the
/// newest row.
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
    // The wake's reply and end row written since: the first read still
    // places the task's turn, and so does a read of the whole.
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
    // NEGATIVE CONTROL: no done point to place it — the wake's reply.
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

    // A person's prompt written after the done showed, before the read.
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
    // NEGATIVE CONTROL: unplaced, the person's turn is the newest.
    assert!(!turn_ended(&jsonl(&rows)));

    // The check's rule: the check answered, then the notification.
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

/// The task's turn ended with a done while a background agent it
/// started still ran (its done row counts `1 background agent still
/// running`, a done the policy holds back for that work's end); the agent
/// ended at 17:29:10, its `<task-notification>` woke Claude Code, and the
/// woken turn ran a tool — the read its done edge took, the woken turn's
/// final reply (written about 20 ms after the screen showed the done, at
/// 17:29:20) not there yet. Its screen showed the done after the wake.
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

/// [`held_then_woken`]'s final reply and end row, as Claude Code writes
/// them after the screen showed the done.
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

/// THE WAKE WHOSE END THE DONE IS, IS WAITED FOR: a marker the policy held
/// back over a background agent, then that agent's notification woke the
/// worker, and the woken turn's end — a done too — is the done
/// point. The done edge's read holds the earlier turn's marker and the
/// woken turn's tool rows, but not its final reply: the woken turn began
/// before the screen showed the done, so it is the turn waited for, and its
/// reply is the result. NEGATIVE CONTROL — the same rows, the screen having
/// shown the done at the earlier turn's end (P5's showing): that turn's
/// report, never waited for. Each the other's control: the done point's
/// time alone decides which turn the same transcript gives.
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
    // NEGATIVE CONTROL: the done shown at the earlier turn's end.
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

/// A ROW'S STAMP is Claude Code's UTC to the millisecond, a fraction of
/// fewer digits read as one, none as `.000`. NEGATIVE CONTROLS: an offset,
/// a missing `Z` and a fraction with no digits are no stamp.
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

/// A TURN THAT WROTE NO END ROW DOES NOT RUN ON INTO THE NEXT TURN'S REPLY:
/// the done edge read the task's turn with no reply yet; the turn then
/// stopped without its `turn_duration` row (an interrupted one), and a
/// background command's notification woke Claude Code on a turn whose reply
/// says done. The settled tail stops at the notification — the next turn's
/// start — so neither the snapshot nor `result`'s read again takes the
/// later reply: the task's turn gives its own last words, cut as they are.
/// NEGATIVE CONTROLS: the same rows with the end
/// row written are cut at that row alike; and a tagged row before the
/// turn said anything (two notifications delivered together) is the same
/// turn's.
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
    // `result`'s read again: the task's turn, never the wake's.
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
    // NEGATIVE CONTROL: the end row written, the cut is there.
    let mut ended = rows[..4].to_vec();
    ended.push(turn_duration());
    ended.extend(rows[4..].iter().cloned());
    let got = settle_with(first.clone(), None, |_| Some(jsonl(&ended)), || true);
    assert_eq!(got.tail, jsonl(&ended[..5]));
    // NEGATIVE CONTROL: two notifications together open one turn.
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
