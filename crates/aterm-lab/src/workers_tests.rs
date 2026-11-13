// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_workers_line_is_read_from_its_foreground_program() {
    let procs = "procs  tab 1 · 2 live\n\
        proc pid=7 exe=trustc label=job pgid=7 cpu_mc=1\n\
        proc pid=88 exe=claude label=fg pgid=88 cpu_mc=2\n";
    assert_eq!(foreground_pid(procs), Some(88));
    assert_eq!(foreground_pid("proc pid=7 exe=zsh label=job\n"), None);
    let line = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert!(rides_parent_mode(&line(&[
        "claude",
        "--permission-mode",
        "acceptEdits",
        "--model",
        "x",
    ])));
    assert!(!rides_parent_mode(&line(&[
        "claude",
        "--permission-mode",
        "default",
    ])));
    assert!(!rides_parent_mode(&line(&["claude"])));
}

#[test]
fn an_iso_stamp_is_read_as_unix_milliseconds() {
    assert_eq!(iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(iso_ms("2026-10-16T10:24:42.738Z"), Some(1_792_146_282_738));
    assert_eq!(iso_ms("2000-03-01T00:00:01Z"), Some(951_868_801_000));
    assert_eq!(iso_ms("2026-10-16 10:24:42"), None);
    assert_eq!(iso_ms("2026-10-26 09:11:48Z"), None);
    for bad in [
        "-9223372036854775808-01-01T00:00:00Z",
        "999999999999-01-01T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-10-16T24:00:00Z",
        "2026-10-16T10:60:00Z",
        "2026-10-16T10:24:42.Z",
        "2026-10-16T10:24:42.738xyzZ",
        "2026-10-16-01T10:24:42Z",
    ] {
        assert_eq!(iso_ms(bad), None, "{bad}");
    }
}

#[test]
fn the_script_keys_each_conversation_and_the_stub_plays_it() {
    use crate::stub::{ToolTurn, tool_turn};
    use aterm_agent::supervise::policy::turn_end::clean_defense;
    let plan = Plan::new("0badc0de");
    let script = plan.script();
    let req =
        |msgs: &[J]| J::obj([("messages", J::Arr(msgs.to_vec()))]).dump(crate::pyjson::COMPACT);
    let user = |t: &str| J::obj([("role", J::s("user")), ("content", J::s(t))]);
    let used_call = || {
        J::obj([
            ("role", J::s("assistant")),
            (
                "content",
                J::Arr(vec![J::obj([("type", J::s("tool_use"))])]),
            ),
        ])
    };
    let used = used_call();
    let result = |t: &str| {
        J::obj([
            ("role", J::s("user")),
            (
                "content",
                J::Arr(vec![J::obj([
                    ("type", J::s("tool_result")),
                    ("content", J::s(t)),
                ])]),
            ),
        ])
    };
    let said = |t: &str| J::obj([("role", J::s("assistant")), ("content", J::s(t))]);
    let mut m = vec![user(&plan.prompt)];
    for (k, w) in plan.started().enumerate() {
        match tool_turn(req(&m).as_bytes(), &script) {
            Some(ToolTurn::Call(name, input, step)) => {
                assert_eq!((name.as_str(), step), ("Bash", k));
                assert_eq!(
                    input.get("command").and_then(J::as_str),
                    Some(Plan::spawn_command(w).as_str())
                );
            }
            other => panic!("spawn {k}: {other:?}"),
        }
        m.push(used.clone());
        m.push(result(&format!("OK s-{k}{k}{k}a")));
    }
    match tool_turn(req(&m).as_bytes(), &script) {
        Some(ToolTurn::Call(_, input, 4)) => {
            assert_eq!(
                input.get("command").and_then(J::as_str),
                Some("aterm ctl await task @s-000a,@s-111a,@s-222a,@s-333a all since=0")
            );
            assert!(input.get("run_in_background").is_some_and(J::truthy));
        }
        other => panic!("await: {other:?}"),
    }
    m.push(used.clone());
    m.push(result("Command running in background with ID: b1"));
    assert_eq!(
        tool_turn(req(&m).as_bytes(), &script),
        Some(ToolTurn::Say(plan.waiting.clone(), 5))
    );
    m.push(said(&plan.waiting));
    m.push(user(
        "<task-notification><status>completed</status></task-notification>",
    ));
    match tool_turn(req(&m).as_bytes(), &script) {
        Some(ToolTurn::Call(_, input, 6)) => assert_eq!(
            input.get("command").and_then(J::as_str),
            Some("aterm ctl @s-000a result")
        ),
        other => panic!("result: {other:?}"),
    }
    for w in plan.started() {
        m.push(used.clone());
        m.push(result(&w.report));
    }
    assert_eq!(
        tool_turn(req(&m).as_bytes(), &script),
        Some(ToolTurn::Say(plan.summary.clone(), 10))
    );
    let w = &plan.workers[1];
    let task_row = user(&format!("{}\n\n(Started by `s-m` …)", w.task));
    assert!(matches!(
        tool_turn(req(std::slice::from_ref(&task_row)).as_bytes(), &script),
        Some(ToolTurn::Call(n, _, 0)) if n == "Bash"
    ));
    assert_eq!(
        tool_turn(
            req(&[task_row.clone(), used.clone(), result("2 w2.out")]).as_bytes(),
            &script
        ),
        Some(ToolTurn::Say(w.report.clone(), 1))
    );
    let land = user("[aterm harness] Autopilot is Land: land ALL of this work …");
    assert_eq!(
        tool_turn(
            req(&[task_row, used, result("2 w2.out"), said(&w.report), land]).as_bytes(),
            &script
        ),
        Some(ToolTurn::Say(w.defense.clone(), 2))
    );
    let failing = user(&format!("{}\n\n(Started by `s-m` …)", plan.failing.task));
    assert_eq!(
        tool_turn(
            req(&[failing, used_call(), result("2 w4.out")]).as_bytes(),
            &script
        ),
        Some(ToolTurn::Say(plan.failing.report.clone(), 1))
    );
    assert_eq!(
        aterm_agent::supervise::policy::needs::could_not_finish(&plan.failing.report),
        plan.failing.report.strip_prefix("Could not finish: ")
    );
    for w in &plan.workers {
        assert!(clean_defense(Some(&w.defense)), "{}", w.defense);
        assert!(!clean_defense(Some(&w.report)), "{}", w.report);
        assert!(!w.result().contains("ATERM-TASK-DONE"));
        assert_eq!(w.result(), format!("{}\n\n{}\n", w.report, w.defense));
    }
    for w in plan.started() {
        assert!(!w.task.contains(&plan.mgr));
        assert!(plan.started().filter(|o| w.task.contains(&o.token)).count() == 1);
        assert!(!w.task.lines().any(|l| l == "EOF"));
        assert_eq!(w.report.trim(), w.report, "the result reader trims");
    }
}

#[test]
fn transcript_rows_give_the_prompt_the_tool_calls_and_their_results() {
    let text = [
        r#"{"type":"mode"}"#,
        r#"{"type":"user","timestamp":"2026-10-16T10:24:42.738Z","message":{"role":"user","content":"LABW1-x: go\n\n(Started by `s-m`)"}}"#,
        r#"{"type":"assistant","timestamp":"2026-10-16T10:24:42.836Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-10-16T10:24:42.916Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
        r#"{"type":"api-request-blob","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
        "not json",
        r#"{"type":"assistant","timestamp":"2026-10-16T10:24:42.952Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
    ]
    .join("\n");
    let r = rows(&text);
    assert_eq!(r.len(), 7);
    assert!(r[5].result.is_none() && r[5].kind == "api-request-blob");
    let p = first_prompt(&r).unwrap();
    assert_eq!(p.text, "LABW1-x: go\n\n(Started by `s-m`)");
    assert_eq!(p.ts, Some(1_792_146_282_738));
    assert_eq!(
        r[2].tool_use.as_ref().map(|(n, _)| n.as_str()),
        Some("Bash")
    );
    assert_eq!(spawned(&r), vec!["s-0f0a".to_string()]);
    assert_eq!(r[6].text, "done");
    assert!(first_prompt(&r[3..4]).is_none());
}

#[test]
fn a_spawn_result_naming_its_request_still_names_its_sid() {
    let result = |id: &str, content: &str| {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"{content}"}}]}}}}"#
        )
    };
    let text = [
        result("t1", "OK s-0f0a req=r-20261109-1411-local-01"),
        result("t2", r"OK s-0f0b\n"),
        // NEGATIVE CONTROLS: a refusal, a sid that is not one, and nothing after `OK`.
        result(
            "t3",
            "ERR spawn req=r-20261109-1411-local-01: the request has concluded",
        ),
        result("t4", "OK s-0f0z req=r-20261109-1411-local-02"),
        result("t5", "OK "),
    ]
    .join("\n");
    assert_eq!(
        spawned(&rows(&text)),
        vec!["s-0f0a".to_string(), "s-0f0b".to_string()]
    );
}

#[test]
fn the_pushes_name_each_session_by_its_ack() {
    let lines: Vec<(f64, String)> = [
        "sub 0 s-m",
        "EVENT 0 agent idle rev=3",
        "sub 1 s-w",
        "EVENT * session-created s-w",
        "EVENT 1 task rev=1 state=working attention=0",
        "EVENT 1 task rev=2 state=done attention=0 why=landed",
        "EVENT 7 task rev=1 state=stalled attention=1 why=shell",
        "GAP 1 events-dropped=2",
    ]
    .iter()
    .enumerate()
    .map(|(i, l)| (f64::from(u32::try_from(i).unwrap()), (*l).to_string()))
    .collect();
    let p = pushed(&lines);
    assert_eq!(p.created, vec![("s-w".to_string(), 3.0)]);
    assert_eq!(p.tasks.len(), 3);
    assert_eq!(
        p.tasks[1],
        (
            "s-w".into(),
            5.0,
            "rev=2 state=done attention=0 why=landed".into()
        )
    );
    assert_eq!(kv(&p.tasks[1].2, "why"), Some("landed"));
    assert_eq!(p.tasks[2].0, "local:7");
    assert_eq!(p.gaps.len(), 1);
    assert_eq!(
        kv("OK task rev=2 state=done result_bytes=15", "result_bytes"),
        Some("15")
    );
    assert_eq!(kv("OK task rev=2", "state"), None);
}

#[test]
fn the_upgrade_column_is_read_off_the_sessions_own_ls_row() {
    let ls = "0 0 s-m - alive t program=claude upgrade=done/2.1.301/-/40s task=-\n\
              0 1 s-w s-m alive t program=claude upgrade=pending/2.1.301/settling/5s task=done";
    assert_eq!(upgrade_of(ls, "s-m"), "done/2.1.301/-/40s");
    assert_eq!(upgrade_of(ls, "s-w"), "pending/2.1.301/settling/5s");
    assert_eq!(upgrade_of("0 1 s-w s-m alive upgrade=-", "s-m"), "");
}

#[test]
fn a_notification_names_its_task_and_output_file() {
    let t = "<task-notification>\n<task-id>b1</task-id>\n<output-file>/private/tmp/x/b1.output</output-file>\n<status>completed</status>";
    assert_eq!(notification_field(t, "task-id"), Some("b1"));
    assert_eq!(
        notification_field(t, "output-file"),
        Some("/private/tmp/x/b1.output")
    );
    assert_eq!(
        notification_field("<status>completed</status>", "output-file"),
        None
    );
}

/// The await's output is read only where Claude Code writes the task's
/// output for the manager's own conversation under the temp root the lab
/// gave it, a path the lab builds: never wherever the notification's text
/// points.
#[test]
fn the_await_output_is_read_where_the_managers_conversation_writes_it() {
    let root = Path::new("/s/runs/t/scratch/tmp.Ab12Cd");
    let transcript = Path::new("/s/home/.claude/projects/-private-tmp-at-Ab12-proj/5f0c.jsonl");
    let uid = crate::sys::getuid();
    assert_eq!(
        task_output_path(root, transcript, "b1x2"),
        Some(PathBuf::from(format!(
            "/s/runs/t/scratch/tmp.Ab12Cd/claude-{uid}/-private-tmp-at-Ab12-proj/5f0c/tasks/b1x2.output"
        )))
    );
    for id in ["", "../x", "a/b", "..", "b1 x"] {
        assert_eq!(task_output_path(root, transcript, id), None, "{id:?}");
    }
    for t in [
        "/s/home/.claude/projects/p/...jsonl",
        "/s/home/.claude/projects/p/..jsonl",
        "/s/home/.claude/projects/../5f0c.jsonl",
        "5f0c.jsonl",
    ] {
        assert_eq!(task_output_path(root, Path::new(t), "b1x2"), None, "{t:?}");
    }
}

#[test]
fn a_bounded_read_takes_a_short_file_and_refuses_a_long_one() {
    let d = crate::TestScratch::new("workers");
    let f = d.join("b1.output");
    std::fs::write(&f, "@s-1 rev=2 state=done\n").unwrap();
    assert_eq!(
        read_bounded(&f, 64).as_deref(),
        Ok("@s-1 rev=2 state=done\n")
    );
    assert!(read_bounded(&f, 21).is_err());
    let link = d.join("link.output");
    std::os::unix::fs::symlink(&f, &link).unwrap();
    assert!(read_bounded(&link, 64).is_err(), "a link is not followed");
}

#[test]
fn the_survey_shows_a_session_only_with_what_it_was_asked_and_said() {
    let text = "== host pid 1 0.105.0 · 2 sessions trust=screen\n\
        s-m claude idle 0s | ✳ Claude Code\n\
        \x20   asked: LABMGR-x: start three workers\n\
        \x20   said: Summary: all three\n\
        s-w claude idle 0s task=done/2 | ✳ Claude Code\n\
        \x20   asked: LABW1-x: write the file\n\
        s-z bash running sleep\n";
    let want = |sid: &str, asked: &str, said: &str| {
        (
            sid.to_string(),
            asked.to_string(),
            said.to_string(),
            "the end of it".to_string(),
        )
    };
    assert_eq!(
        survey_shown(text, &[want("s-m", "LABMGR-x", "Summary: all")]),
        1
    );
    let cut = text.replace(
        "    said: Summary: all three",
        "    said: …and the end of it",
    );
    assert_eq!(
        survey_shown(&cut, &[want("s-m", "LABMGR-x", "Summary: all")]),
        1
    );
    assert_eq!(
        survey_shown(text, &[want("s-w", "LABW1-x", "Worker one")]),
        0
    );
    assert_eq!(
        survey_shown(text, &[want("s-w", "LABW1-x", "Summary: all")]),
        0
    );
    assert_eq!(survey_shown(text, &[want("s-z", "", "")]), 0);
    assert_eq!(survey_block(text, "s"), "");
    assert_eq!(survey_block(text, ""), "");
}

#[test]
fn a_record_never_stalled_moved_through_every_rev_working_and_done() {
    let timeline = "OK 4\n\
        event 1 t=5 kind=spawned pid=7\n\
        event 2 t=6 kind=task-change rev=1 state=working attention=0\n\
        event 3 t=9 kind=agent-change agent=busy\n\
        event 4 t=12 kind=task-change rev=2 state=done attention=0 why=landed\n";
    let moves = task_moves(timeline);
    assert_eq!(
        moves,
        vec![(1, "working".to_string()), (2, "done".to_string())]
    );
    assert!(never_stalled(&moves, Some(2)));
    let stalled = [(1, "working".to_string()), (2, "stalled".to_string())];
    assert!(!never_stalled(&stalled, Some(2)));
    assert!(!never_stalled(&moves[1..], Some(2)), "rev 1 missing");
    assert!(!never_stalled(&moves, Some(3)), "rev 3 never recorded");
    assert!(!never_stalled(&moves, None));
}
