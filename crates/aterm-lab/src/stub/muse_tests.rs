// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse Code's dialect on typed request bodies, and served for real on
//! loopback to the workspace's HTTP client.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;

use super::super::{Args, ToolTurn, control, logged_with, start};
use super::*;

fn body(v: &str) -> Vec<u8> {
    v.as_bytes().to_vec()
}

/// A person's message, a runtime note, a call and its result, as Muse Code sends them.
const PERSON: &str = r#"{"type":"message","role":"user","content":"toolstart"}"#;
const NOTE: &str = r#"{"type":"message","role":"user","content":"<system-reminder source=\"x\">toolstart</system-reminder>"}"#;
const CALL: &str =
    r#"{"type":"function_call","call_id":"c1","name":"bash","namespace":"muse","arguments":"{}"}"#;
const RESULT: &str =
    r#"{"type":"function_call_output","call_id":"c1","output":"{\"status\":\"answered\"}"}"#;

fn input(items: &[&str]) -> Vec<u8> {
    body(&format!(r#"{{"input":[{}]}}"#, items.join(",")))
}

#[test]
fn the_catalog_keys_its_model_s_metadata_muse_code() {
    let c = catalog();
    let m = c
        .get("data")
        .and_then(J::as_arr)
        .and_then(<[J]>::first)
        .unwrap();
    assert_eq!(m.get("id").and_then(J::as_str), Some(MODEL));
    let meta = m.get("metadata").unwrap();
    assert!(meta.get("muse-code").is_some(), "{}", c.py());
    assert_eq!(
        meta.get("muse-code")
            .and_then(|x| x.get("is_hidden"))
            .cloned(),
        Some(J::Bool(false))
    );
    // NEGATIVE CONTROL: the metadata is no TbhMetadata at its top (a model so shaped is dropped)
    assert!(meta.get("family").is_none());
}

#[test]
fn the_tool_script_is_answered_from_the_person_s_toolstart_one_result_at_a_time() {
    let script = J::parse(
        r#"[{"name":"bash","input":{"command":"touch x","sandbox_permissions":"require_escalated"}},{"when":"SUB","name":"skipped"},{"name":"write_file","input":{"path":".agents/a.md"}}]"#,
    )
    .unwrap();
    assert_eq!(
        tool_turn(&input(&[r#"{"role":"user","content":"hi"}"#]), &script),
        None
    );
    let first = tool_turn(&input(&[PERSON]), &script);
    assert!(
        matches!(&first, Some(ToolTurn::Call(n, i, 0)) if n == "bash"
            && i.get("sandbox_permissions").and_then(J::as_str) == Some("require_escalated")),
        "{first:?}"
    );
    // the subagent's call is skipped: the second step is the write
    let second = tool_turn(&input(&[PERSON, CALL, RESULT]), &script);
    assert!(
        matches!(&second, Some(ToolTurn::Call(n, _, 1)) if n == "write_file"),
        "{second:?}"
    );
    assert_eq!(
        tool_turn(&input(&[PERSON, CALL, RESULT, CALL, RESULT]), &script),
        Some(ToolTurn::Done)
    );
    // NEGATIVE CONTROL: a runtime note holding the word is not the person's message
    assert_eq!(tool_turn(&input(&[NOTE]), &script), None);
    // a later toolstart starts over
    let again = tool_turn(&input(&[PERSON, CALL, RESULT, PERSON]), &script);
    assert!(matches!(again, Some(ToolTurn::Call(_, _, 0))), "{again:?}");
}

#[test]
fn the_last_tool_result_is_read_back_on_one_line() {
    assert_eq!(
        last_tool_output(&input(&[PERSON, CALL, RESULT])).as_deref(),
        Some(r#"{"status":"answered"}"#)
    );
    let json = r#"{"type":"function_call_output","call_id":"c2","output":{"a":[1,2]}}"#;
    assert_eq!(
        last_tool_output(&input(&[RESULT, json])).as_deref(),
        Some(r#"{"a":[1,2]}"#)
    );
    assert_eq!(last_tool_output(&input(&[PERSON])), None);
}

#[test]
fn a_side_call_gets_its_own_answer_by_its_role_and_tools() {
    let tools = vec!["submit_reminder_decision".to_string()];
    assert_eq!(side_answer("prompt-hint", &[]), Side::Text("NONE"));
    let Side::Call(name, args) = side_answer("reminder:skill-reminder", &tools) else {
        panic!("a reminder offered its decision tool calls it")
    };
    assert_eq!(name, "submit_reminder_decision");
    assert_eq!(args.get("decision").and_then(J::as_str), Some("none"));
    // NEGATIVE CONTROL: a role that offers no decision tool gets text
    assert_eq!(side_answer("reminder:goal-reminder", &[]), Side::Text("ok"));
    assert_eq!(role_of(None), "main");
    assert_eq!(role_of(Some("")), "main");
    assert_eq!(
        role_of(Some("reminder:verify-reminder")),
        "reminder:verify-reminder"
    );
    assert_eq!(role_of(Some("a b\n\"c")), "abc");
    let names = tool_names(&body(
        r#"{"tools":[{"type":"namespace","name":"muse","tools":[{"name":"bash"},{"name":"request_user_input"}]},{"type":"function","name":"flat"}]}"#,
    ));
    assert_eq!(names, ["bash", "request_user_input", "flat"]);
}

/// The events of an SSE body, in order.
fn events(sse: &str) -> Vec<J> {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .collect()
}

fn types(ev: &[J]) -> Vec<String> {
    ev.iter()
        .filter_map(|e| e.get("type").and_then(J::as_str).map(str::to_string))
        .collect()
}

/// Every event numbered from 0, one apart.
fn numbered(ev: &[J]) -> bool {
    ev.iter()
        .enumerate()
        .all(|(i, e)| e.get("sequence_number").and_then(J::as_i64) == i64::try_from(i).ok())
}

/// A raw GET with the lab's bearer key: status and body.
fn get(port: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    s.write_all(
        format!(
            "GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
            super::super::LAB_CODEX_KEY
        )
        .as_bytes(),
    )
    .unwrap();
    let mut answer = String::new();
    s.read_to_string(&mut answer).unwrap();
    let status = answer
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let b = answer
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, b)
}

/// Muse Code's routes, served for real: the catalog, a turn's numbered
/// stream, the side calls answered in every mode, the scripted calls and the
/// answer that quotes their result, the walls, and the log. NEGATIVE
/// CONTROLS: Codex's `/v1/responses` keeps its own stream (no
/// `sequence_number`), and a side call never spends a custom mode's `times`.
#[test]
#[allow(clippy::too_many_lines)]
fn served_on_loopback_it_answers_muse_code_as_the_modes_say() {
    let dir = std::env::temp_dir().join(format!("lab-stub-muse-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("modes")).unwrap();
    let log = dir.join("stub.log");
    let tool_file = dir.join("tool.json");
    let args = Args {
        log: Some(log.clone()),
        modes_dir: Some(dir.join("modes")),
        tool_file: Some(tool_file.clone()),
        slow_seconds: 2,
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |path: &str, b: &str, role: &str| {
        let mut r = client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .header(
                "Authorization",
                &format!("Bearer {}", super::super::LAB_CODEX_KEY),
            );
        if !role.is_empty() {
            r = r.header("x-tbh-agent-role", role);
        }
        r.send(b.as_bytes()).unwrap()
    };
    let turn = |text: &str| {
        format!(
            r#"{{"model":"lab-stub","stream":true,"tools":[{{"type":"namespace","name":"muse","tools":[{{"name":"bash"}}]}}],"input":[{{"type":"message","role":"developer","content":"be brief"}},{{"type":"message","role":"user","content":"{text}"}}]}}"#
        )
    };
    let reminder = r#"{"model":"lab-stub","stream":true,"tools":[{"type":"namespace","name":"muse","tools":[{"name":"submit_reminder_decision"}]}],"input":[{"type":"message","role":"user","content":"x"}]}"#;

    let (st, b) = get(&port, "/muse-code/models");
    assert_eq!(st, 200);
    // the stub answers JSON as Python's json.dumps(sort_keys=True) spells it
    assert_eq!(J::parse(&b).unwrap(), J::parse(&catalog().py()).unwrap());

    // a turn: the numbered stream, its one message, the usage
    let r = post("/responses", &turn("carry on"), "main");
    assert_eq!(r.status(), 200);
    let ev = events(&String::from_utf8_lossy(r.body()));
    assert!(numbered(&ev), "{ev:?}");
    assert_eq!(
        types(&ev),
        [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let done = ev.last().and_then(|e| e.get("response")).unwrap();
    let out_items = done.get("output").and_then(J::as_arr).unwrap();
    assert_eq!(
        out_items[0]
            .get("content")
            .and_then(J::as_arr)
            .and_then(<[J]>::first)
            .and_then(|p| p.get("text"))
            .and_then(J::as_str),
        Some("Hi! I am the local lab stub, not a model. (reply 1)")
    );
    assert!(
        done.get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(J::as_i64)
            .unwrap()
            > 0
    );
    // NEGATIVE CONTROL: Codex's path keeps Codex's stream
    let codex = post(
        "/v1/responses",
        r#"{"model":"gpt-x","stream":true,"input":[{"role":"user","content":"hi"}]}"#,
        "",
    );
    let cev = events(&String::from_utf8_lossy(codex.body()));
    assert!(
        !cev.is_empty() && cev.iter().all(|e| e.get("sequence_number").is_none()),
        "{cev:?}"
    );

    // the side calls, answered even under a wall
    control(&port, "/__mode?m=401").unwrap();
    let r = post("/responses", reminder, "reminder:skill-reminder");
    assert_eq!(r.status(), 200);
    let ev = events(&String::from_utf8_lossy(r.body()));
    let call = ev
        .iter()
        .find(|e| e.get("type").and_then(J::as_str) == Some("response.output_item.done"))
        .and_then(|e| e.get("item"))
        .unwrap();
    assert_eq!(
        (
            call.get("type").and_then(J::as_str),
            call.get("name").and_then(J::as_str),
            call.get("namespace").and_then(J::as_str)
        ),
        (
            Some("function_call"),
            Some("submit_reminder_decision"),
            Some("muse")
        )
    );
    let hint = post("/responses", &turn("x"), "prompt-hint");
    assert!(String::from_utf8_lossy(hint.body()).contains(r#""delta":"NONE""#));
    // ... and the turn gets the wall
    let r = post("/responses", &turn("carry on"), "main");
    assert_eq!(r.status(), 401);
    assert!(String::from_utf8_lossy(r.body()).contains("invalid_api_key"));
    control(&port, "/__mode?m=500").unwrap();
    assert_eq!(post("/responses", &turn("x"), "").status(), 500);
    control(&port, "/__mode?m=rate-limit").unwrap();
    let r = post("/responses", &turn("x"), "main");
    assert_eq!(
        (r.status(), r.header("retry-after")),
        (429, Some("5")),
        "{}",
        String::from_utf8_lossy(r.body())
    );
    control(&port, "/__mode?m=too-long").unwrap();
    let ev = events(&String::from_utf8_lossy(
        post("/responses", &turn("x"), "main").body(),
    ));
    assert!(numbered(&ev));
    assert_eq!(
        types(&ev).last().map(String::as_str),
        Some("response.failed")
    );
    control(&port, "/__mode?m=slow").unwrap();
    let ev = events(&String::from_utf8_lossy(
        post("/responses", &turn("x"), "main").body(),
    ));
    let text: String = ev
        .iter()
        .filter(|e| e.get("type").and_then(J::as_str) == Some("response.output_text.delta"))
        .filter_map(|e| e.get("delta").and_then(J::as_str).map(str::to_string))
        .collect();
    assert_eq!(text, "tick 1 tick 2 ");

    // a side call never spends a custom mode's budget
    std::fs::write(
        dir.join("modes/once.json"),
        r#"{"times": 1, "text": "the custom turn"}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=once").unwrap();
    post("/responses", reminder, "reminder:goal-reminder");
    let first = String::from_utf8_lossy(post("/responses", &turn("x"), "main").body()).into_owned();
    assert!(first.contains("the custom turn"), "{first}");
    let second =
        String::from_utf8_lossy(post("/responses", &turn("x"), "main").body()).into_owned();
    assert!(
        !second.contains("the custom turn"),
        "used up after one turn"
    );

    // the tool mode: the script's call in the muse namespace, then the answer quoting its result
    std::fs::write(
        &tool_file,
        r#"{"name":"bash","input":{"command":"touch probe.txt","sandbox_permissions":"require_escalated"}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=tool").unwrap();
    let ev = events(&String::from_utf8_lossy(
        post("/responses", &turn("toolstart"), "main").body(),
    ));
    assert!(numbered(&ev));
    let args_done = ev
        .iter()
        .find(|e| {
            e.get("type").and_then(J::as_str) == Some("response.function_call_arguments.done")
        })
        .unwrap();
    assert_eq!(args_done.get("name").and_then(J::as_str), Some("bash"));
    let a = J::parse(args_done.get("arguments").and_then(J::as_str).unwrap()).unwrap();
    assert_eq!(
        a.get("sandbox_permissions").and_then(J::as_str),
        Some("require_escalated")
    );
    let after = format!(
        r#"{{"model":"lab-stub","stream":true,"input":[{PERSON},{CALL},{RESULT}],"tools":[{{"type":"namespace","name":"muse","tools":[{{"name":"bash"}}]}}]}}"#
    );
    let answer = String::from_utf8_lossy(post("/responses", &after, "main").body()).into_owned();
    assert!(
        answer.contains(r#"Tool call done. Result: {\"status\":\"answered\"}"#),
        "{answer}"
    );

    let wants = [
        "GET /muse-code/models -> 200 body=0B mode=ok kind=muse-models auth=key models=1",
        " kind=muse auth=key model=lab-stub stream=true items=2 tools=1 instructions=0c last_user=\"carry on\" sse=",
        " kind=muse-side role=reminder:skill-reminder auth=key model=lab-stub items=1 tools=1 last_user=\"x\" side:call submit_reminder_decision",
        " kind=muse-side role=prompt-hint ",
        "side:text NONE",
        "POST /responses -> 401 ",
        " tool:bash step 0",
        " tool:done result='{\"status\":\"answered\"}'",
    ];
    // The stub writes a request's line once its answer is sent, so the last
    // answer reaches this client before its line reaches the log.
    let logged = logged_with(&log, &wants, Duration::from_secs(10));
    for want in wants {
        assert!(logged.contains(want), "{want} in\n{logged}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An admitted log retains the lines already present. A deliberately
/// expired observation cannot invent a missing line; there is no wall-clock
/// performance assertion or scheduler-dependent late writer.
#[test]
fn the_log_observation_keeps_present_lines_and_refuses_missing_ones() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("stub.log");
    std::fs::write(&log, "early\n tool:done result='x'\n").unwrap();
    let got = logged_with(&log, &["early", " tool:done "], Duration::from_secs(60));
    assert!(
        got.contains("early") && got.contains(" tool:done "),
        "{got}"
    );
    let got = logged_with(&log, &["never written"], Duration::ZERO);
    assert!(
        !got.contains("never written") && got.contains("early"),
        "{got}"
    );
}

/// MUSE CODE'S SIDE CALLS ARE DUMPED APART FROM ITS TURNS, by the role it
/// names (`x-tbh-agent-role`): a reminder observer and the prompt hint as
/// `side-<n>.json`, a turn (`main`, or no role) as `req-<n>.json`, one count
/// over both; the turns' reader ([`super::super::turn_dumps`]) reads the
/// turns alone. The observer's body here carries the conversation rendered
/// into one text, as Muse Code 1.4.10-R6427.1 sends it (the lab's `r10-cv`,
/// 2026-10-22: read as a turn, the owner's words reached the agent with a
/// line break). NEGATIVE CONTROL: the turns stay `req-`.
#[test]
fn muse_codes_side_calls_are_dumped_apart_from_its_turns() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let dump = dir.path().join("dump");
    let (port, _) = start(Args {
        dump_dir: Some(dump.clone()),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |b: &str, role: &str| {
        let mut r = client
            .post(&format!("http://127.0.0.1:{port}/responses"))
            .header(
                "Authorization",
                &format!("Bearer {}", super::super::LAB_CODEX_KEY),
            );
        if !role.is_empty() {
            r = r.header("x-tbh-agent-role", role);
        }
        assert_eq!(r.send(b.as_bytes()).unwrap().status(), 200);
    };
    let turn = r#"{"model":"lab-stub","stream":true,"tools":[{"type":"namespace","name":"muse","tools":[{"name":"bash"}]}],"input":[{"type":"message","role":"user","content":"the turn"}]}"#;
    let observer = r#"{"model":"lab-stub","stream":true,"tools":[{"type":"namespace","name":"muse","tools":[{"name":"submit_reminder_decision"}]}],"input":[{"type":"message","role":"user","content":"You are a reminder observer for the main agent.\n<main-agent-conversation format=\"model-conversation-xml/v1\">\n  <message role=\"user\">\n    <text>the turn</text>\n  </message>\n</main-agent-conversation>"}]}"#;
    post(turn, "main");
    post(observer, "reminder:verify-reminder");
    post(turn, "prompt-hint");
    post(turn, "");
    let mut names: Vec<String> = std::fs::read_dir(&dump)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "req-00001.json",
            "req-00004.json",
            "side-00002.json",
            "side-00003.json"
        ]
    );
    let turns: Vec<u64> = super::super::turn_dumps(&dump)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(turns, [4, 1]);
}
