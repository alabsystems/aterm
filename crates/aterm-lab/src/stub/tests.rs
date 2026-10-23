// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stub on typed request bodies, and served for real on loopback to the
//! workspace's HTTP client (an independent parser of its framing).
use super::*;

fn body(v: &str) -> Vec<u8> {
    v.as_bytes().to_vec()
}

#[test]
fn a_request_is_classed_by_what_it_asks() {
    let main = body(
        r#"{"model":"m","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#,
    );
    let i = analyse(&main);
    assert_eq!(
        (i.kind, i.stream, i.msgs, i.tools, i.last_user.as_str()),
        ("main", true, 1, 1, "hi")
    );
    assert_eq!(analyse(&body(r#"{"messages":[]}"#)).kind, "aux");
    assert_eq!(
        analyse(&body(r#"{"max_tokens":1,"tools":[{}],"messages":[]}"#)).kind,
        "quota"
    );
    let compact = body(
        r#"{"system":[{"type":"text","text":"You are tasked with summarizing conversations"}],"tools":[{}],"messages":[]}"#,
    );
    assert_eq!(analyse(&compact).kind, "compact");
    assert_eq!(analyse(b"not json").kind, "opaque");
    assert_eq!(analyse(b"[1]").kind, "opaque");
}

#[test]
fn the_tool_script_is_answered_one_call_per_request_from_toolstart() {
    let script =
        J::parse(r#"[{"name":"Read","input":{"p":1}},{"name":"Edit","input":{"p":2}}]"#).unwrap();
    let none = body(r#"{"messages":[{"role":"user","content":"hello"}]}"#);
    assert_eq!(tool_turn(&none, &script), None);
    let start = body(r#"{"messages":[{"role":"user","content":"toolstart"}]}"#);
    assert!(matches!(tool_turn(&start, &script), Some(ToolTurn::Call(n, _, 0)) if n == "Read"));
    let result = r#"{"role":"user","content":[{"type":"tool_result","content":"x"}]}"#;
    let one = body(&format!(
        r#"{{"messages":[{{"role":"user","content":"toolstart"}},{{"role":"assistant","content":[{{"type":"tool_use","name":"Read"}}]}},{result}]}}"#
    ));
    assert!(matches!(tool_turn(&one, &script), Some(ToolTurn::Call(n, _, 1)) if n == "Edit"));
    let two = body(&format!(
        r#"{{"messages":[{{"role":"user","content":"toolstart"}},{result},{result}]}}"#
    ));
    assert_eq!(tool_turn(&two, &script), Some(ToolTurn::Done));
}

/// A `calls` step is several tool uses in ONE assistant message (parallel
/// calls), answered by one request holding all their results: one step.
#[test]
fn a_calls_step_makes_its_calls_in_one_message_and_counts_as_one_step() {
    let script = J::parse(
        r#"[{"calls":[{"name":"Bash","input":{"command":"echo a"}},{"name":"Bash","input":{"command":"echo b"}}]},
            {"name":"Read","input":{}}]"#,
    )
    .unwrap();
    let start = body(r#"{"messages":[{"role":"user","content":"toolstart"}]}"#);
    match tool_turn(&start, &script) {
        Some(ToolTurn::Calls(calls, 0)) => {
            let commands: Vec<Option<&str>> = calls
                .iter()
                .map(|(n, i)| {
                    assert_eq!(n, "Bash");
                    i.get("command").and_then(J::as_str)
                })
                .collect();
            assert_eq!(commands, [Some("echo a"), Some("echo b")]);
        }
        other => panic!("{other:?}"),
    }
    let both = body(
        r#"{"messages":[{"role":"user","content":"toolstart"},{"role":"assistant","content":[{"type":"tool_use","name":"Bash"},{"type":"tool_use","name":"Bash"}]},{"role":"user","content":[{"type":"tool_result","content":"a"},{"type":"tool_result","content":"b"}]}]}"#,
    );
    assert!(matches!(tool_turn(&both, &script), Some(ToolTurn::Call(n, _, 1)) if n == "Read"));
    let empty = J::parse(r#"[{"calls":[{"input":{}}]},{"name":"Read","input":{}}]"#).unwrap();
    assert!(matches!(tool_turn(&start, &empty), Some(ToolTurn::Call(n, _, 0)) if n == "Read"));
}

/// Each input of a `calls` step has its `@SID:<k>@` filled from the `k`th
/// tool result, as a single call's is ([`fill_sids`]): every call in the
/// message, not the first alone.
#[test]
fn a_calls_steps_inputs_have_their_sids_filled() {
    let script = J::parse(
        r#"[{"name":"Bash","input":{"command":"spawn"}},
            {"calls":[{"name":"Bash","input":{"command":"result @SID:1@"}},{"name":"Bash","input":{"command":"await @SID:1@"}}]}]"#,
    )
    .unwrap();
    let commands = |b: &[u8]| match tool_turn(b, &script) {
        Some(ToolTurn::Calls(calls, _)) => calls
            .iter()
            .map(|(_, i)| i.get("command").and_then(J::as_str).unwrap().to_string())
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let after = body(
        r#"{"messages":[{"role":"user","content":"toolstart"},{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]},{"role":"user","content":[{"type":"tool_result","content":"spawned s-1a2b"}]}]}"#,
    );
    assert_eq!(commands(&after), ["result s-1a2b", "await s-1a2b"]);
    let first =
        J::parse(r#"[{"calls":[{"name":"Bash","input":{"command":"result @SID:1@"}}]}]"#).unwrap();
    let start = body(r#"{"messages":[{"role":"user","content":"toolstart"}]}"#);
    match tool_turn(&start, &first) {
        Some(ToolTurn::Calls(calls, 0)) => assert_eq!(
            calls[0].1.get("command").and_then(J::as_str),
            Some("result @SID:1@")
        ),
        other => panic!("{other:?}"),
    }
}

/// Served for real: a `calls` step streams one assistant message with one
/// `tool_use` block per call, at indexes 0, 1, …, each block's input
/// reassembled from its own `input_json_delta`s, and stops for `tool_use`
/// (what Claude Code answered with two boxes in the breaker-4 lab), each
/// block with an id of its own (Claude Code pairs each result with its call
/// by that id).
#[test]
fn served_a_calls_step_streams_every_call_in_one_message() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("lab-stub-calls-")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
    let tool_file = dir.join("tool.json");
    let mode_file = dir.join("mode");
    std::fs::write(&mode_file, "tool\n").unwrap();
    let args = Args {
        mode_file: Some(mode_file),
        tool_file: Some(tool_file.clone()),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"toolstart"}]}"#;
    let streamed = || -> (Vec<(String, J)>, Vec<String>, Option<String>) {
        let r = client
            .post(&format!("http://127.0.0.1:{port}/v1/messages"))
            .send(turn.as_bytes())
            .unwrap();
        assert_eq!(r.status(), 200);
        let events: Vec<J> = String::from_utf8_lossy(r.body())
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| J::parse(d).ok())
            .collect();
        let mut blocks: Vec<(String, String)> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut stop = None;
        for e in &events {
            let index = e
                .get("index")
                .and_then(J::as_i64)
                .and_then(|i| usize::try_from(i).ok());
            match e.get("type").and_then(J::as_str) {
                Some("content_block_start") => {
                    let b = e.get("content_block").unwrap();
                    assert_eq!(b.get("type").and_then(J::as_str), Some("tool_use"));
                    assert_eq!(index, Some(blocks.len()));
                    let name = b.get("name").and_then(J::as_str).unwrap().to_string();
                    blocks.push((name, String::new()));
                    ids.push(b.get("id").and_then(J::as_str).unwrap().to_string());
                }
                Some("content_block_delta") => {
                    let part = e
                        .get("delta")
                        .and_then(|d| d.get("partial_json"))
                        .and_then(J::as_str)
                        .unwrap();
                    blocks[index.unwrap()].1.push_str(part);
                }
                Some("message_delta") => {
                    stop = e
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(J::as_str)
                        .map(str::to_string);
                }
                _ => {}
            }
        }
        let blocks = blocks
            .into_iter()
            .map(|(n, js)| (n, J::parse(&js).unwrap()))
            .collect();
        (blocks, ids, stop)
    };
    std::fs::write(
        &tool_file,
        r#"[{"calls":[{"name":"Bash","input":{"command":"echo a","description":"A"}},{"name":"Bash","input":{"command":"echo b","description":"B"}}]}]"#,
    )
    .unwrap();
    let (blocks, ids, stop) = streamed();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.iter().all(|id| id.starts_with("toolu_lab")), "{ids:?}");
    assert_ne!(ids[0], ids[1], "each call its own id");
    let commands: Vec<(&str, Option<&str>)> = blocks
        .iter()
        .map(|(n, i)| (n.as_str(), i.get("command").and_then(J::as_str)))
        .collect();
    assert_eq!(
        commands,
        [("Bash", Some("echo a")), ("Bash", Some("echo b"))]
    );
    assert_eq!(stop.as_deref(), Some("tool_use"));
    std::fs::write(
        &tool_file,
        r#"[{"name":"Bash","input":{"command":"echo c"}}]"#,
    )
    .unwrap();
    let (blocks, ids, stop) = streamed();
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(ids.len(), 1, "{ids:?}");
    assert_eq!(
        blocks[0].1.get("command").and_then(J::as_str),
        Some("echo c")
    );
    assert_eq!(stop.as_deref(), Some("tool_use"));
}

#[test]
fn a_call_with_when_belongs_to_the_subagent_whose_prompt_holds_it() {
    let script = J::parse(r#"[{"name":"Agent","input":{}},{"when":"SUBLAB","name":"Bash","input":{"command":"touch x"}}]"#).unwrap();
    let sub = body(r#"{"messages":[{"role":"user","content":"SUBLAB touch it"}]}"#);
    assert!(matches!(tool_turn(&sub, &script), Some(ToolTurn::Call(n, _, 0)) if n == "Bash"));
    let sub_done = body(
        r#"{"messages":[{"role":"user","content":"SUBLAB"},{"role":"user","content":[{"type":"tool_result"}]}]}"#,
    );
    assert_eq!(tool_turn(&sub_done, &script), Some(ToolTurn::Done));
    let main = body(r#"{"messages":[{"role":"user","content":"toolstart"}]}"#);
    assert!(matches!(tool_turn(&main, &script), Some(ToolTurn::Call(n, _, 0)) if n == "Agent"));
}

#[test]
fn a_text_step_ends_the_turn_and_the_script_goes_on_at_the_next_user_row() {
    let script = J::parse(
        r#"[{"when":"MGR","name":"Bash","input":{"command":"spawn"}},
            {"when":"MGR","text":"Waiting."},
            {"when":"MGR","name":"Bash","input":{"command":"aterm ctl @@SID:1@ result","n":["@SID:2@"]}},
            {"when":"MGR","text":"Summary."}]"#,
    )
    .unwrap();
    let start = r#"{"role":"user","content":"MGR go"}"#;
    let used = r#"{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}"#;
    let result =
        r#"{"role":"user","content":[{"type":"tool_result","content":"OK s-0f0accf352b3"}]}"#;
    let said = r#"{"role":"assistant","content":[{"type":"text","text":"Waiting."}]}"#;
    let wake = r#"{"role":"user","content":"<task-notification>done</task-notification>"}"#;
    let req = |ms: &[&str]| body(&format!(r#"{{"messages":[{}]}}"#, ms.join(",")));
    assert!(
        matches!(tool_turn(&req(&[start]), &script), Some(ToolTurn::Call(n, _, 0)) if n == "Bash")
    );
    assert_eq!(
        tool_turn(&req(&[start, used, result]), &script),
        Some(ToolTurn::Say("Waiting.".into(), 1))
    );
    match tool_turn(&req(&[start, used, result, said, wake]), &script) {
        Some(ToolTurn::Call(_, input, 2)) => {
            assert_eq!(
                input.get("command").and_then(J::as_str),
                Some("aterm ctl @s-0f0accf352b3 result")
            );
            assert_eq!(input.get("n"), Some(&J::Arr(vec![J::s("@SID:2@")])));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        tool_turn(
            &req(&[start, used, result, said, wake, used, result]),
            &script
        ),
        Some(ToolTurn::Say("Summary.".into(), 3))
    );
    let summary = r#"{"role":"assistant","content":"Summary."}"#;
    let more = r#"{"role":"user","content":"again"}"#;
    assert_eq!(
        tool_turn(
            &req(&[start, used, result, said, wake, used, result, summary, more]),
            &script
        ),
        Some(ToolTurn::Done)
    );
    let calls_only = J::parse(
        r#"[{"when":"MGR","name":"Bash","input":{}},{"when":"MGR","name":"Read","input":{}}]"#,
    )
    .unwrap();
    assert!(
        matches!(tool_turn(&req(&[start, used, result]), &calls_only), Some(ToolTurn::Call(n, _, 1)) if n == "Read")
    );
    let main =
        J::parse(r#"[{"name":"Read","input":{}},{"text":"mid"},{"name":"Edit","input":{}}]"#)
            .unwrap();
    let ts = r#"{"role":"user","content":"toolstart"}"#;
    let mid = r#"{"role":"assistant","content":"mid"}"#;
    assert_eq!(
        tool_turn(&req(&[ts, used, result]), &main),
        Some(ToolTurn::Say("mid".into(), 1))
    );
    assert!(
        matches!(tool_turn(&req(&[ts, used, result, mid, more]), &main), Some(ToolTurn::Call(n, _, 2)) if n == "Edit")
    );
}

#[test]
fn a_session_id_is_s_dash_and_hex_digits() {
    assert_eq!(first_sid("OK s-0f0a\n"), Some("s-0f0a"));
    assert_eq!(first_sid("aterm-ctl: OK s-1 x s-2"), Some("s-1"));
    assert_eq!(first_sid("s- s-xyz this-s-12 s-"), None);
}

#[test]
fn deltas_rebuild_the_text_and_each_but_the_last_holds_twelve_characters() {
    let text = "Hi! I am the local lab stub, not a model. (reply 1)";
    let d = split_deltas(text);
    assert_eq!(d.concat(), text);
    assert!(d[..d.len() - 1].iter().all(|p| p.chars().count() >= 12));
    assert_eq!(split_deltas(""), vec![String::new()]);
}

#[test]
fn mode_parameters_parse_as_the_modes_document_them() {
    assert_eq!(
        limit_params("limit:five_hour:1792000000"),
        Some(("five_hour".into(), 1_792_000_000, None))
    );
    assert_eq!(
        limit_params("limit:seven_day:5:team"),
        Some(("seven_day".into(), 5, Some("team".into())))
    );
    assert_eq!(
        limit_params("limit:seven_day:5:Bad"),
        Some(("seven_day".into(), 5, None))
    );
    assert_eq!(limit_params("limit:Five:1"), None);
    assert_eq!(limit_params("limit:five_hour"), None);
    assert_eq!(bg_params("bg", 30), (20, 30));
    assert_eq!(bg_params("bg:14:40", 30), (14, 40));
    assert_eq!(bg_params("bg:0:x", 30), (1, 30));
    assert_eq!(clean_mode("custom:my-mode"), "my-mode");
    assert_eq!(clean_mode("a b;c"), "abc");
    assert_eq!(clean_mode(""), "ok");
    assert_eq!(
        parse_query("m=limit%3Afive_hour%3A5&n="),
        vec![("m".to_string(), "limit:five_hour:5".to_string())]
    );
}

#[test]
fn rate_headers_rise_per_answer_served_and_clamp_at_one() {
    let spec = J::parse(
        r#"{"on200":{"anthropic-ratelimit-unified-5h-utilization":"@SEQ:0.40:0.02@","anthropic-ratelimit-unified-7d-reset":1792000000,"x-plain":"@SEQ:1..2:3@"},"on429":{"anthropic-ratelimit-unified-status":"rejected"}}"#,
    )
    .unwrap();
    let at = |served| rate_headers(&spec, served);
    let five = |r: &RateHeaders| r.on200[0].1.clone();
    assert_eq!(five(&at(0)), "0.4000");
    assert_eq!(five(&at(3)), "0.4600");
    assert_eq!(five(&at(1000)), "1.0000", "clamped to 0..1");
    assert_eq!(at(0).on200[1].1, "1792000000", "a number is its text");
    assert_eq!(
        at(0).on200[2].1,
        "@SEQ:1..2:3@",
        "a value that is no figure is kept"
    );
    assert_eq!(
        at(5).summary(),
        " RL200[5h-utilization=0.5000,7d-reset=1792000000,x-plain=@SEQ:1..2:3@] RL429[status=rejected]"
    );
    assert_eq!(
        rate_headers(&J::parse("[1]").unwrap(), 0),
        RateHeaders::default()
    );
}

/// One `--headers-file` serves both of the stub's readers: `headers` changes
/// the `on200`/`on429` blocks alone, `ratelimits` the flat entries alone, and
/// each keeps what the other wrote.
#[test]
fn each_headers_verb_keeps_the_others_entries() {
    let old = r#"{"x-codex-primary-used-percent":"76.0","on200":{"a":"1"},"on429":{"b":"2"}}"#;
    let blocks = headers_file_with(old, J::parse(r#"{"on200":{"c":"3"}}"#).unwrap(), true);
    assert_eq!(
        blocks.py(),
        r#"{"x-codex-primary-used-percent": "76.0", "on200": {"c": "3"}}"#,
        "headers: the flat entry stays, the blocks are the new ones"
    );
    let flat = headers_file_with(
        &blocks.py(),
        J::parse(r#"{"x-codex-secondary-used-percent":"5.0"}"#).unwrap(),
        false,
    );
    assert_eq!(
        flat.py(),
        r#"{"on200": {"c": "3"}, "x-codex-secondary-used-percent": "5.0"}"#,
        "ratelimits: the blocks stay, the flat entries are the new ones"
    );
    assert_eq!(
        headers_file_with(&flat.py(), J::parse("{}").unwrap(), true).py(),
        r#"{"x-codex-secondary-used-percent": "5.0"}"#,
        "headers '{{}}' clears the blocks alone"
    );
    assert_eq!(
        headers_file_with("not json", J::parse(r#"{"on429":{}}"#).unwrap(), true).py(),
        r#"{"on429": {}}"#,
        "a file that reads as no object keeps nothing"
    );
    assert_eq!(
        headers_file_with("{}", J::parse(r#"{"on200":{},"x":"1"}"#).unwrap(), false).py(),
        r#"{"x": "1"}"#
    );
}

/// The server, for real: the modes a lab switches between, the SSE framing an
/// independent client decodes, the control paths, the black hole and the log.
#[test]
fn served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request() {
    let dir = std::env::temp_dir().join(format!("lab-stub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("stub.log");
    let mode_file = dir.join("mode");
    std::fs::write(&mode_file, "ok\n").unwrap();
    let headers_file = dir.join("headers.json");
    let args = Args {
        proxy_port: Some(0),
        log: Some(log.clone()),
        mode_file: Some(mode_file.clone()),
        headers_file: Some(headers_file.clone()),
        modes_dir: Some(dir.join("modes")),
        dump_dir: Some(dir.join("dump")),
        ..Args::default()
    };
    let (port, proxy_port) = start(args).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |path: &str, b: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .send(b.as_bytes())
            .unwrap()
    };
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;

    let r = post("/v1/messages", turn);
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.header("content-type"),
        Some("text/event-stream; charset=utf-8")
    );
    let sse = String::from_utf8_lossy(r.body()).into_owned();
    let text: String = sse
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .filter_map(|e| {
            e.get("delta")
                .and_then(|d| d.get("text"))
                .and_then(J::as_str)
                .map(str::to_string)
        })
        .collect();
    assert_eq!(text, "Hi! I am the local lab stub, not a model. (reply 1)");
    assert!(sse.starts_with("event: message_start\n") && sse.contains("event: message_stop\n"));

    std::fs::write(
        &headers_file,
        r#"{"on200":{"anthropic-ratelimit-unified-5h-utilization":"@SEQ:0.40:0.02@"},"on429":{"anthropic-ratelimit-unified-7d-utilization":"0.99"}}"#,
    )
    .unwrap();
    let five = |r: &aterm_http::Response| {
        r.header("anthropic-ratelimit-unified-5h-utilization")
            .map(str::to_string)
    };
    assert_eq!(five(&post("/v1/messages", turn)).as_deref(), Some("0.4000"));
    assert_eq!(five(&post("/v1/messages", turn)).as_deref(), Some("0.4200"));

    let r = post("/v1/messages/count_tokens", turn);
    assert_eq!(
        J::parse(&String::from_utf8_lossy(r.body()))
            .unwrap()
            .get("input_tokens"),
        Some(&J::Int(27))
    );

    assert_eq!(
        control(&port, "/__mode?m=overloaded").unwrap(),
        r#"{"mode": "overloaded"}"#
    );
    assert_eq!(std::fs::read_to_string(&mode_file).unwrap(), "overloaded\n");
    assert_eq!(post("/v1/messages", turn).status(), 529);

    let future = crate::util::now_secs() + 600;
    control(&port, &format!("/__mode?m=limit:five_hour:{future}")).unwrap();
    let r = post("/v1/messages", turn);
    assert_eq!(r.status(), 429);
    assert_eq!(
        r.header("anthropic-ratelimit-unified-status"),
        Some("rejected")
    );
    assert_eq!(
        r.header("anthropic-ratelimit-unified-7d-utilization"),
        Some("0.99")
    );
    std::fs::remove_file(&headers_file).unwrap();
    assert_eq!(
        r.header("anthropic-ratelimit-unified-representative-claim"),
        Some("five_hour")
    );
    control(&port, "/__mode?m=limit:five_hour:1").unwrap();
    assert_eq!(
        post("/v1/messages", turn).status(),
        200,
        "a limit whose reset passed answers ok"
    );

    std::fs::create_dir_all(dir.join("modes")).unwrap();
    std::fs::write(
        dir.join("modes/streamed.json"),
        r#"{"stream":true,"text":"custom hi","headers":{"x-lab-custom":"on"}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=streamed").unwrap();
    let r = post("/v1/messages", turn);
    assert_eq!(
        (
            r.status(),
            r.header("content-type"),
            r.header("x-lab-custom")
        ),
        (200, Some("text/event-stream; charset=utf-8"), Some("on"))
    );

    std::fs::write(&mode_file, "too-long\n").unwrap();
    assert_eq!(post("/v1/messages", turn).status(), 400);

    let r = post("/nope", "");
    assert_eq!(r.status(), 404);
    let status = J::parse(&control(&port, "/__status").unwrap()).unwrap();
    assert!(status.get("n_requests").and_then(J::as_i64).unwrap() >= 6);

    let mut s = TcpStream::connect(("127.0.0.1", proxy_port.unwrap())).unwrap();
    s.write_all(b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    s.read_to_string(&mut answer).unwrap();
    assert!(
        answer.starts_with("HTTP/1.1 502 Bad Gateway\r\n"),
        "{answer}"
    );

    let want = [
        "stub listening on 127.0.0.1:",
        "MODE -> ok (from file)",
        "POST /v1/messages -> 200 body=",
        " kind=main model=claude-x stream=true max_tokens=32000 msgs=1 tools=1 sys=0c last_user=\"hi\" sse=",
        "POST /v1/messages -> 529",
        " RL200[5h-utilization=0.4000]",
        " RL200[5h-utilization=0.4200]",
        "limit:five_hour resets in ",
        "limit lifted (reset passed)",
        "POST /nope -> 404 body=0B mode=",
        "PROXY refused: CONNECT api.anthropic.com:443 HTTP/1.1",
    ];
    let logged = logged_with(&log, &want, Duration::from_secs(60));
    for w in want {
        assert!(logged.contains(w), "{w:?} is not in the log:\n{logged}");
    }
    assert!(dir.join("dump/req-00001.json").is_file());
    std::fs::write(&mode_file, "ok\n").unwrap();
    let n = std::fs::read_dir(dir.join("dump")).unwrap().count() as u64;
    let aux =
        r#"{"model":"claude-x","max_tokens":32,"messages":[{"role":"user","content":"a title"}]}"#;
    assert_eq!(post("/v1/messages", aux).status(), 200);
    assert!(
        dir.join(format!("dump/side-{:05}.json", n + 1)).is_file(),
        "{:?}",
        std::fs::read_dir(dir.join("dump"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );
    assert!(
        !crate::stub::turn_dumps(&dir.join("dump"))
            .iter()
            .any(|(i, _)| *i == n + 1),
        "the side call is no turn"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SIDE CALL IS DUMPED APART FROM THE TURNS (`side-<n>.json`, `req-<n>.json`
/// for a turn, one count over both), and the turns' reader reads `req-`
/// alone, newest first: every kind the stub logs is a turn or a side call by
/// [`is_side`].
#[test]
fn a_side_call_is_dumped_apart_from_the_turns() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let stub = Stub::new(Args {
        dump_dir: Some(dir.path().to_path_buf()),
        ..Args::default()
    });
    let kinds = [
        ("main", false),
        ("aux", true),
        ("quota", true),
        ("compact", true),
        ("codex", false),
        ("codex-json", true),
        ("muse", false),
        ("muse-json", true),
        ("muse-side", true),
        ("gemini", false),
        ("gemini-json", true),
        ("gemini-aux", true),
        ("opaque", false),
    ];
    for (i, (kind, side)) in kinds.iter().enumerate() {
        assert_eq!(is_side(kind), *side, "{kind}");
        stub.dump(format!("body {i} {kind}").as_bytes(), kind);
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<String> = kinds
        .iter()
        .enumerate()
        .map(|(i, (_, side))| format!("{}{:05}.json", if *side { "side-" } else { "req-" }, i + 1))
        .collect();
    want.sort();
    assert_eq!(names, want);
    let turns: Vec<(u64, String)> = turn_dumps(dir.path())
        .into_iter()
        .map(|(n, p)| (n, std::fs::read_to_string(p).unwrap()))
        .collect();
    assert_eq!(
        turns,
        [
            (13, "body 12 opaque".to_string()),
            (10, "body 9 gemini".to_string()),
            (7, "body 6 muse".to_string()),
            (5, "body 4 codex".to_string()),
            (1, "body 0 main".to_string()),
        ]
    );
}

/// ONLY THE NEWEST [`DUMPS_KEPT`] DUMPS STAY, turns and side calls alike:
/// once the one count passes an old dump's index by [`DUMPS_KEPT`], that
/// dump goes, whichever it was.
#[test]
fn the_oldest_dumps_go_turns_and_side_calls_alike() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let stub = Stub::new(Args {
        dump_dir: Some(dir.path().to_path_buf()),
        ..Args::default()
    });
    stub.dump(b"a side call", "muse-side");
    stub.dump(b"a turn", "main");
    stub.dump(b"a side call", "aux");
    let names = || {
        let mut n: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        n.sort();
        n
    };
    assert_eq!(
        names(),
        ["req-00002.json", "side-00001.json", "side-00003.json"]
    );
    stub.dumped.store(DUMPS_KEPT, Ordering::Relaxed);
    stub.dump(b"a turn", "main");
    let newest = format!("req-{:05}.json", DUMPS_KEPT + 1);
    assert_eq!(
        names(),
        ["req-00002.json", newest.as_str(), "side-00003.json"],
        "the side call #1 went"
    );
    stub.dump(b"a side call", "gemini-json");
    let side = format!("side-{:05}.json", DUMPS_KEPT + 2);
    assert_eq!(
        names(),
        [newest.as_str(), "side-00003.json", side.as_str()],
        "the turn #2 went"
    );
}

#[test]
fn a_codex_request_is_read_for_its_items_and_its_structured_side_requests() {
    let main = body(
        r#"{"model":"gpt-x","stream":true,"instructions":"be brief","tools":[{},{}],"input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#,
    );
    let i = analyse_responses(&main);
    assert_eq!(
        (
            i.kind,
            i.stream,
            i.items,
            i.tools,
            i.instr_chars,
            i.last_user.as_str()
        ),
        ("codex", true, 1, 2, 8, "hi")
    );
    let side = body(
        r#"{"input":[],"text":{"format":{"type":"json_schema","schema":{"properties":{"title":{"type":"string","maxLength":5},"n":{"type":"integer"}}}}}}"#,
    );
    let i = analyse_responses(&side);
    assert_eq!(i.kind, "codex-json");
    assert_eq!(schema_answer(i.schema.as_ref()), r#"{"title": "lab s"}"#);
    assert_eq!(analyse_responses(b"nope").kind, "opaque");
    assert_eq!(
        usage_limit_body("five_hour", 9, Some("plus")).py(),
        r#"{"error": {"type": "usage_limit_reached", "message": "The usage limit has been reached", "resets_at": 9, "limit_window_minutes": 300, "plan_type": "plus"}}"#
    );
    assert!(usage_limit_headers("other", 9).is_empty());
    let flat = flat_headers(
        &J::parse(r#"{"X-Codex-Primary-Reset-At":"@NOW+60@","bad name":"x","on200":{"a":"b"},"n":1.5,"z":null}"#).unwrap(),
        100,
    );
    assert_eq!(
        flat,
        vec![
            ("x-codex-primary-reset-at".to_string(), "160".to_string()),
            ("n".to_string(), "1.5".to_string())
        ]
    );
}

/// A raw GET (the workspace client only posts): status and body.
fn get(port: &str, path: &str, auth: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    let auth = auth
        .map(|a| format!("Authorization: {a}\r\n"))
        .unwrap_or_default();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut answer = String::new();
    s.read_to_string(&mut answer).unwrap();
    let status = answer
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// The Responses API and the ChatGPT backend, served for real: the SSE Codex
/// reads, the walls in its shapes, the flat `x-codex-*` headers on every
/// answer, the account usage, the refresh, the note, and the credential labels.
#[test]
fn served_on_loopback_it_answers_codex_as_the_modes_say() {
    let dir = std::env::temp_dir().join(format!("lab-stub-codex-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("modes")).unwrap();
    let log = dir.join("stub.log");
    let headers_file = dir.join("headers.json");
    let args = Args {
        log: Some(log.clone()),
        headers_file: Some(headers_file.clone()),
        modes_dir: Some(dir.join("modes")),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |path: &str, b: &str, auth: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .header("Authorization", auth)
            .send(b.as_bytes())
            .unwrap()
    };
    let turn = r#"{"model":"gpt-x","stream":true,"input":[{"role":"user","content":"hi"}]}"#;
    std::fs::write(&headers_file, r#"{"x-codex-primary-used-percent":"76.0","x-codex-primary-window-minutes":"300","x-codex-primary-reset-at":"@NOW+3600@","x-codex-credits-has-credits":"true","x-codex-credits-balance":"12.5"}"#).unwrap();
    let r = post("/v1/responses", turn, "Bearer lab-not-a-key");
    assert_eq!(r.status(), 200);
    assert_eq!(r.header("x-codex-primary-used-percent"), Some("76.0"));
    let sse = String::from_utf8_lossy(r.body()).into_owned();
    let events: Vec<J> = sse
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| J::parse(d).ok())
        .collect();
    let types: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("type").and_then(J::as_str))
        .collect();
    assert_eq!(types.first(), Some(&"response.created"));
    assert_eq!(types.last(), Some(&"response.completed"));
    let text: String = events
        .iter()
        .filter(|e| e.get("type").and_then(J::as_str) == Some("response.output_text.delta"))
        .filter_map(|e| e.get("delta").and_then(J::as_str).map(str::to_string))
        .collect();
    assert_eq!(text, "Hi! I am the local lab stub, not a model. (reply 1)");
    let usage = events
        .last()
        .and_then(|e| e.get("response"))
        .and_then(|r| r.get("usage"))
        .unwrap();
    assert!(usage.get("total_tokens").and_then(J::as_i64).unwrap() > 0);

    let jwt = {
        let j = J::parse(&crate::codex::auth_json("A", "acct-9", "pro", 0)).unwrap();
        j.get("tokens")
            .and_then(|t| t.get("access_token"))
            .and_then(J::as_str)
            .unwrap()
            .to_string()
    };
    let (st, b) = get(
        &port,
        "/backend-api/wham/usage",
        Some(&format!("Bearer {jwt}")),
    );
    assert_eq!(st, 200);
    let u = J::parse(&b).unwrap();
    assert_eq!(u.get("plan_type").and_then(J::as_str), Some("pro"));
    let rl = u.get("rate_limit").unwrap();
    assert_eq!(rl.get("limit_reached"), Some(&J::Bool(false)));
    assert_eq!(
        rl.get("primary_window").and_then(|w| w.get("used_percent")),
        Some(&J::Int(76))
    );
    assert_eq!(
        rl.get("primary_window")
            .and_then(|w| w.get("limit_window_seconds")),
        Some(&J::Int(18_000))
    );
    assert_eq!(rl.get("secondary_window"), Some(&J::Null));
    assert_eq!(
        u.get("credits")
            .and_then(|c| c.get("balance"))
            .and_then(J::as_str),
        Some("12.5")
    );
    let (st, b) = get(
        &port,
        "/backend-api/wham/accounts/check",
        Some(&format!("Bearer {jwt}")),
    );
    assert_eq!(st, 200);
    let a = J::parse(&b).unwrap();
    assert_eq!(
        a.get("default_account_id").and_then(J::as_str),
        Some("acct-9")
    );

    let future = crate::util::now_secs() + 600;
    control(&port, &format!("/__mode?m=limit:seven_day:{future}:team")).unwrap();
    let r = post("/v1/responses", turn, &format!("Bearer {jwt}"));
    assert_eq!(r.status(), 429);
    assert_eq!(r.header("x-codex-secondary-used-percent"), Some("100"));
    let e = J::parse(&String::from_utf8_lossy(r.body())).unwrap();
    assert_eq!(
        e.get("error")
            .and_then(|x| x.get("plan_type"))
            .and_then(J::as_str),
        Some("team")
    );
    assert_eq!(
        e.get("error").and_then(|x| x.get("limit_window_minutes")),
        Some(&J::Int(10080))
    );
    control(&port, "/__mode?m=overloaded").unwrap();
    assert_eq!(post("/v1/responses", turn, "").status(), 503);
    control(&port, "/__mode?m=too-long").unwrap();
    let r = post("/v1/responses", turn, "");
    assert!(String::from_utf8_lossy(r.body()).contains("context_length_exceeded"));

    std::fs::write(
        dir.join("modes/a401.json"),
        r#"{"status": 401, "auth": "jwt:A", "json": {"error": {"code": "token_expired"}}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=a401").unwrap();
    assert_eq!(
        post("/v1/responses", turn, "Bearer lab-not-a-key").status(),
        200
    );
    assert_eq!(
        post("/v1/responses", turn, &format!("Bearer {jwt}")).status(),
        401
    );

    let r = post("/oauth/token", r#"{"refresh_token":"lab-refresh-A"}"#, "");
    assert_eq!(r.status(), 401);
    assert!(String::from_utf8_lossy(r.body()).contains("refresh_token_expired"));
    std::fs::write(
        dir.join("modes/refresh.json"),
        r#"{"status": 401, "json": {"error": {"code": "refresh_token_reused"}}}"#,
    )
    .unwrap();
    assert!(
        String::from_utf8_lossy(post("/oauth/token", "{}", "").body())
            .contains("refresh_token_reused")
    );
    control(&port, "/__mode?m=refresh").unwrap();
    assert_eq!(post("/v1/responses", turn, "").status(), 200);
    std::fs::write(dir.join("modes/ok.json"), r#"{"status": 429}"#).unwrap();
    control(&port, "/__mode?m=ok").unwrap();
    assert_eq!(post("/v1/responses", turn, "").status(), 200);
    control(&port, "/__note?t=auth.json%20now%20holds%20B").unwrap();

    let want = [
        "POST /v1/responses -> 200 body=",
        " kind=codex auth=key model=gpt-x stream=true items=1 tools=0 instructions=0c last_user=\"hi\" sse=",
        "limit:seven_day resets in ",
        " plan=team",
        "kind=usage auth=jwt:A primary=76 secondary=None credits=True",
        "kind=refresh auth=none (refresh.json)",
        "NOTE auth.json now holds B",
    ];
    let logged = logged_with(&log, &want, Duration::from_secs(60));
    for w in want {
        assert!(logged.contains(w), "{w:?} is not in the log:\n{logged}");
    }
    assert!(
        !logged.contains("lab-refresh-A") && !logged.contains(&jwt),
        "no token is ever logged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The claude.ai account routes, served for real: each answers its key of
/// the account file with the instants expanded, a missing key is a 404, and
/// the log names the route, the read and the credential's lab label, never
/// the credential.
#[test]
fn served_on_loopback_the_account_routes_answer_from_the_account_file() {
    let dir = std::env::temp_dir().join(format!("lab-stub-account-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("stub.log");
    let account_file = dir.join("account.json");
    let args = Args {
        log: Some(log.clone()),
        account_file: Some(account_file.clone()),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let oat = crate::lab::oauth_token();
    let bearer = format!("Bearer {oat}");

    let (st, _) = get(&port, "/api/oauth/usage", Some(&bearer));
    assert_eq!(st, 404, "no account file yet");
    std::fs::write(&account_file, account::composed().py()).unwrap();
    let (st, b) = get(
        &port,
        "/api/oauth/usage?at_wall=1&skip_spend=1",
        Some(&bearer),
    );
    assert_eq!(st, 200);
    let usage = J::parse(&b).unwrap();
    let reset = usage
        .get("five_hour")
        .and_then(|w| w.get("resets_at"))
        .and_then(J::as_str)
        .unwrap();
    assert!(
        reset.starts_with("20") && reset.ends_with(".000Z"),
        "{reset}"
    );
    assert!(usage.get("cedar_ember").is_some() && usage.get("juniper_tide").is_some());
    let (st, b) = get(&port, "/api/oauth/profile", Some(&bearer));
    assert_eq!(st, 200);
    assert!(b.contains("ana@example.com"));
    std::fs::write(&account_file, r#"{"usage":{"limits":[]},"profile":null}"#).unwrap();
    assert_eq!(get(&port, "/api/claude_cli_profile", None).0, 404);
    assert_eq!(get(&port, "/api/claude_cli/bootstrap", None).0, 404);

    let want = [
        "GET /api/oauth/usage -> 200 body=0B mode=ok kind=account route=usage read=at_wall auth=oat served=yes",
        "route=cli-profile auth=none served=none",
    ];
    let text = logged_with(&log, &want, Duration::from_secs(60));
    for w in want {
        assert!(text.contains(w), "{w:?} is not in the log:\n{text}");
    }
    assert!(!text.contains(&oat), "the token is never logged");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A mode name is judged without case, as the case-insensitive scratch volume
/// names its file: `Refresh` is Codex's `refresh.json`, `SLOW` the built-in.
#[test]
fn a_mode_name_is_judged_as_the_volume_names_its_file() {
    for name in [
        "refresh",
        "Refresh",
        "REFRESH",
        "slow",
        "SLOW",
        "Limit:five_hour",
    ] {
        assert!(super::not_custom(name).is_some(), "{name}");
    }
    for name in ["mine", "Mine", "refreshed", "slowly"] {
        assert!(super::not_custom(name).is_none(), "{name}");
    }
}

/// A Gemini API request's credentials are labelled, never logged: the lab's
/// key alone is `gemini-key`, nothing is `none`, a bearer token alone is its
/// own label, and a bearer token beside the key is both labels joined.
#[test]
fn a_gemini_requests_credentials_are_labelled_both_when_two_ride() {
    let key = Some(gemini::LAB_GEMINI_KEY);
    let login = Some("Bearer ya29.a-google-login");
    assert_eq!(gemini::credential_label(key, None), "gemini-key");
    assert_eq!(gemini::credential_label(None, None), "none");
    assert_eq!(gemini::credential_label(None, login), "other(19 chars)");
    let both = gemini::credential_label(key, login);
    assert_eq!(both, "gemini-key+other(19 chars)");
    assert_ne!(both, "gemini-key", "the control");
    assert!(!both.contains("ya29"));
}

/// The Gemini API's side requests and turns are classed as both CLIs send
/// them, the `tool` script is stepped by the results after the last
/// `toolstart` (agy returns them in a `model` content), and a schema is
/// filled as the router needs it.
#[test]
fn a_gemini_request_is_classed_and_its_tool_script_stepped() {
    let turn = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"<session_context>x</session_context>"}]},{"role":"user","parts":[{"text":"hi"}]}],"systemInstruction":{"parts":[{"text":"be brief"}]},"tools":[{"functionDeclarations":[{"name":"a"},{"name":"b"}]}]}"#,
    );
    let i = gemini::analyse_gemini(&turn);
    assert_eq!(
        (
            i.kind,
            i.contents,
            i.tools,
            i.sys_chars,
            i.last_user.as_str()
        ),
        ("gemini", 2, 2, 8, "hi")
    );
    let router = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseJsonSchema":{"type":"object","properties":{"complexity_reasoning":{"type":"string"},"complexity_score":{"type":"integer"}},"required":["complexity_reasoning","complexity_score"]}}}"#,
    );
    let r = gemini::analyse_gemini(&router);
    assert_eq!(r.kind, "gemini-json");
    let filled = gemini::schema_fill(r.schema.as_ref().unwrap(), None);
    assert_eq!(filled.get("complexity_score"), Some(&J::Int(10)));
    let title = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"<USER_REQUEST>\nhi there\n</USER_REQUEST>"}]}]}"#,
    );
    let t = gemini::analyse_gemini(&title);
    assert_eq!((t.kind, t.last_user.as_str()), ("gemini-aux", "hi there"));
    assert_eq!(gemini::analyse_gemini(b"nope").kind, "opaque");
    assert_eq!(
        gemini::schema_fill(
            &J::parse(r#"{"type":"OBJECT","properties":{"t":{"type":"STRING","maxLength":3},"e":{"enum":["x","y"]},"b":{"type":"BOOLEAN"}}}"#).unwrap(),
            None
        )
        .py(),
        r#"{"b": false, "e": "x", "t": "lab"}"#,
        "each property filled, in the parser's key order"
    );

    let script = J::parse(
        r#"[{"name":"run_shell_command","input":{"command":"touch x"}},{"name":"write_file","input":{"file_path":"/p/y"}},{"when":"SUB","name":"Bash"}]"#,
    )
    .unwrap();
    let start = body(r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]}]}"#);
    assert!(matches!(gemini::tool_turn(&start, &script),
        Some(ToolTurn::Call(n, _, 0)) if n == "run_shell_command"));
    for role in ["model", "user"] {
        let one = body(&format!(
            r#"{{"contents":[{{"role":"user","parts":[{{"text":"<USER_REQUEST>toolstart</USER_REQUEST>"}}]}},{{"role":"model","parts":[{{"functionCall":{{"name":"run_shell_command"}}}}]}},{{"role":"{role}","parts":[{{"functionResponse":{{"name":"run_shell_command","response":{{"output":""}}}}}}]}}]}}"#
        ));
        assert!(
            matches!(gemini::tool_turn(&one, &script),
            Some(ToolTurn::Call(n, _, 1)) if n == "write_file"),
            "{role}"
        );
    }
    let two = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]},{"role":"user","parts":[{"functionResponse":{}}]},{"role":"user","parts":[{"functionResponse":{}}]}]}"#,
    );
    assert_eq!(gemini::tool_turn(&two, &script), Some(ToolTurn::Done));
    assert_eq!(
        gemini::tool_turn(
            &body(r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#),
            &script
        ),
        None
    );
    let held = J::parse(
        r#"[{"name":"invoke_subagent","input":{"Subagents":"[{\"Prompt\":\"LABSUB-HOLD reply DONE\"}]"}},{"when":"LABSUB-HOLD","text":"DONE","delay_ms":150000}]"#,
    )
    .unwrap();
    let sub = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"<USER_REQUEST>LABSUB-HOLD reply DONE</USER_REQUEST>"}]}]}"#,
    );
    assert_eq!(
        gemini::held_reply(&sub, &held),
        Some(("DONE".to_string(), 150_000.0))
    );
    let main = body(
        r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]},{"role":"model","parts":[{"functionCall":{"name":"invoke_subagent","args":{"Subagents":"LABSUB-HOLD"}}}]}]}"#,
    );
    assert_eq!(gemini::held_reply(&main, &held), None);
    assert_eq!(
        gemini::held_reply(&sub, &script),
        None,
        "a named `when` step is a call"
    );
    assert_eq!(
        gemini::route(
            "POST",
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent"
        ),
        Some(("gemini-3.5-flash".into(), gemini::Verb::Stream))
    );
    assert_eq!(
        gemini::route("GET", "/v1beta/models/m:generateContent"),
        None
    );
    assert_eq!(gemini::route("POST", "/v1beta/models/m:embedContent"), None);
}

/// Each wall a mode puts on a Gemini turn is Google's error body with the
/// status and the details both CLIs read: the per-day quota's QuotaFailure
/// and its hour-long RetryInfo (agy retried a per-day 429 with a 5 s delay),
/// the API_KEY_INVALID reason; a mode that is no wall gets none.
#[test]
fn a_gemini_wall_is_googles_error_body() {
    let (st, e) = gemini::mode_error("rate-limit", "gemini-3.5-flash").unwrap();
    assert_eq!(st, 429);
    let err = e.get("error").unwrap();
    assert_eq!(
        (err.get("code"), err.get("status").and_then(J::as_str)),
        (Some(&J::Int(429)), Some("RESOURCE_EXHAUSTED"))
    );
    let details = err.get("details").and_then(J::as_arr).unwrap();
    let quota_id = details[0]
        .get("violations")
        .and_then(J::as_arr)
        .and_then(|v| v[0].get("quotaId"))
        .and_then(J::as_str)
        .unwrap();
    assert!(quota_id.contains("PerDay"), "{quota_id}");
    assert_eq!(
        details[2].get("retryDelay").and_then(J::as_str),
        Some("3600s")
    );
    for (mode, status, word) in [
        ("overloaded", 503, "UNAVAILABLE"),
        ("500", 500, "INTERNAL"),
        ("401", 401, "UNAUTHENTICATED"),
        ("bad-key", 400, "INVALID_ARGUMENT"),
        ("too-long", 400, "INVALID_ARGUMENT"),
    ] {
        let (st, e) = gemini::mode_error(mode, "m").unwrap();
        assert_eq!(st, status, "{mode}");
        assert_eq!(
            e.get("error")
                .and_then(|x| x.get("status"))
                .and_then(J::as_str),
            Some(word),
            "{mode}"
        );
    }
    assert!(
        gemini::mode_error("bad-key", "m")
            .unwrap()
            .1
            .py()
            .contains("\"reason\": \"API_KEY_INVALID\"")
    );
    for none in ["ok", "slow", "tool", "limit:five_hour:1"] {
        assert!(gemini::mode_error(none, "m").is_none(), "{none}");
    }
    let q = gemini::quota_error("m", 42);
    assert!(q.py().contains("\"retryDelay\": \"42s\""));
}

/// A Google ACCOUNT's spent usage limit (`exhausted_error`, the claim
/// `code_assist`) is Code Assist's 429 in the shape Gemini CLI 0.46.0's
/// `classifyGoogleError` takes as its terminal quota error WITH a delay —
/// an `ErrorInfo` whose reason is `QUOTA_EXHAUSTED` in a Code Assist domain,
/// and a `RetryInfo` — so its usage-limit dialog names the reset; the reset
/// is in the message and the metadata too.
#[test]
fn a_google_accounts_spent_limit_is_code_assists_quota_exhausted() {
    let reset = 1_792_584_000;
    let e = gemini::exhausted_error("gemini-3.5-flash", 3725, reset);
    let err = e.get("error").unwrap();
    assert_eq!(
        (err.get("code"), err.get("status").and_then(J::as_str)),
        (Some(&J::Int(429)), Some("RESOURCE_EXHAUSTED"))
    );
    assert_eq!(
        err.get("message").and_then(J::as_str),
        Some("You have exhausted your capacity on this model. Your quota will reset after 1h2m5s.")
    );
    let details = err.get("details").and_then(J::as_arr).unwrap();
    let info = details
        .iter()
        .find(|d| {
            d.get("@type").and_then(J::as_str) == Some("type.googleapis.com/google.rpc.ErrorInfo")
        })
        .unwrap();
    assert_eq!(
        info.get("reason").and_then(J::as_str),
        Some("QUOTA_EXHAUSTED")
    );
    assert_eq!(
        info.get("domain").and_then(J::as_str),
        Some("cloudcode-pa.googleapis.com")
    );
    let meta = info.get("metadata").unwrap();
    assert_eq!(
        meta.get("quotaResetTimeStamp").and_then(J::as_str),
        Some(format!("{}Z", crate::util::utc_civil(reset)).as_str())
    );
    assert_eq!(
        meta.get("model").and_then(J::as_str),
        Some("gemini-3.5-flash")
    );
    let retry = details
        .iter()
        .find(|d| {
            d.get("@type").and_then(J::as_str) == Some("type.googleapis.com/google.rpc.RetryInfo")
        })
        .unwrap();
    assert_eq!(retry.get("retryDelay").and_then(J::as_str), Some("3725s"));
    let short = gemini::exhausted_error("m", 240, reset).py();
    assert!(short.contains("reset after 4m0s."), "{short}");
    let day = gemini::quota_error("gemini-3.5-flash", 3725).py();
    assert!(
        day.contains("PerDay") && !day.contains("QUOTA_EXHAUSTED"),
        "{day}"
    );
    assert_eq!(gemini::ACCOUNT_CLAIM, "code_assist");
}

/// The Gemini API's other refusals the lab measured (`measured_refusals`):
/// each a custom mode's spec (`status` and `json`) whose body is Google's
/// error with that code, under a name the stub serves as a custom mode and
/// no built-in mode has; the per-minute and per-day 429s name their quota and
/// carry their RetryInfo, the 403 its SERVICE_DISABLED reason.
#[test]
fn the_measured_refusals_are_custom_modes_in_googles_shape() {
    let refusals = gemini::measured_refusals();
    assert_eq!(refusals.len(), 8);
    for (name, what, spec) in &refusals {
        assert!(
            not_custom(name).is_none(),
            "{name}: served as a custom mode"
        );
        assert_eq!(clean_mode(name), *name, "{name}");
        let status = spec.get("status").and_then(J::as_i64).unwrap();
        let err = spec.get("json").and_then(|j| j.get("error")).unwrap();
        assert_eq!(err.get("code").and_then(J::as_i64), Some(status), "{name}");
        assert!(
            what.starts_with(&status.to_string()),
            "{name}: its words name its status ({what})"
        );
    }
    let body = |name: &str| {
        refusals
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, _, s)| s.py())
            .unwrap()
    };
    assert!(body("minute429").contains("PerMinutePerProject"));
    assert!(body("minute429").contains("\"retryDelay\": \"47.218363629s\""));
    assert!(body("day429long").contains("PerDayPerProject"));
    assert!(body("day429long").contains("\"retryDelay\": \"41397.204181s\""));
    assert!(body("perm403").contains("\"reason\": \"SERVICE_DISABLED\""));
    assert!(!body("exhausted429").contains("retryDelay"));
    assert!(not_custom("rate-limit").is_some(), "the control");
}

/// The Gemini API, served for real: the SSE framing an independent client
/// decodes (one `data:` line a chunk, the last one finishing), a side
/// request answered in a wall's mode, the turn walled, a scripted call with
/// its signature, the slow turn's thoughts before its text, the custom
/// modes, and the key logged by its lab label only.
#[test]
fn served_on_loopback_it_answers_gemini_as_the_modes_say() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("lab-stub-gemini-")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
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
    let post = |path: &str, b: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .header("x-goog-api-key", gemini::LAB_GEMINI_KEY)
            .send(b.as_bytes())
            .unwrap()
    };
    let chunks = |r: &aterm_http::Response| -> Vec<J> {
        let text = String::from_utf8_lossy(r.body()).into_owned();
        assert!(text.contains("\r\n\r\n"), "Google's SSE separator: {text}");
        text.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| J::parse(d.trim_end()).ok())
            .collect()
    };
    let parts = |c: &J| -> Vec<J> {
        c.get("candidates")
            .and_then(J::as_arr)
            .and_then(|a| a.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(J::as_arr)
            .map(<[J]>::to_vec)
            .unwrap_or_default()
    };
    let stream = "/v1beta/models/gemini-3.5-flash:streamGenerateContent?alt=sse";
    let turn = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"a"}]}]}"#;

    let r = post(stream, turn);
    assert_eq!(r.status(), 200);
    let cs = chunks(&r);
    let text: String = cs
        .iter()
        .flat_map(&parts)
        .filter_map(|p| p.get("text").and_then(J::as_str).map(str::to_string))
        .collect();
    assert_eq!(text, "Hi! I am the local lab stub, not a model. (reply 1)");
    let last = cs.last().unwrap();
    assert_eq!(
        last.get("candidates")
            .and_then(J::as_arr)
            .and_then(|a| a[0].get("finishReason"))
            .and_then(J::as_str),
        Some("STOP")
    );
    assert!(last.get("usageMetadata").is_some());
    assert!(cs[0].get("usageMetadata").is_none() || cs.len() == 1);

    control(&port, "/__mode?m=rate-limit").unwrap();
    let router = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseJsonSchema":{"type":"object","properties":{"complexity_score":{"type":"integer"}}}}}"#;
    let side = post(
        "/v1beta/models/gemini-3.1-flash-lite:generateContent",
        router,
    );
    assert_eq!(side.status(), 200);
    let answer = J::parse(&String::from_utf8_lossy(side.body())).unwrap();
    let routed = parts(&answer)[0]
        .get("text")
        .and_then(J::as_str)
        .unwrap()
        .to_string();
    assert_eq!(routed, r#"{"complexity_score":10}"#);
    let walled = post(stream, turn);
    assert_eq!(walled.status(), 429);
    assert!(String::from_utf8_lossy(walled.body()).contains("PerDay"));
    let future = crate::util::now_secs() + 600;
    control(&port, &format!("/__mode?m=limit:code_assist:{future}")).unwrap();
    let account = post(stream, turn);
    assert_eq!(account.status(), 429);
    let body = String::from_utf8_lossy(account.body()).into_owned();
    assert!(body.contains("\"QUOTA_EXHAUSTED\""), "{body}");
    assert!(body.contains("cloudcode-pa.googleapis.com"), "{body}");
    assert!(!body.contains("PerDay"), "{body}");
    control(&port, &format!("/__mode?m=limit:five_hour:{future}")).unwrap();
    let key = String::from_utf8_lossy(post(stream, turn).body()).into_owned();
    assert!(
        key.contains("PerDay") && !key.contains("QUOTA_EXHAUSTED"),
        "the control: {key}"
    );

    std::fs::write(
        &tool_file,
        r#"{"name":"run_shell_command","input":{"command":"touch x"}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=tool").unwrap();
    let start_turn = r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]}],"tools":[{"functionDeclarations":[{"name":"a"}]}]}"#;
    let call = parts(&chunks(&post(stream, start_turn))[0])[0].clone();
    assert_eq!(
        call.get("functionCall")
            .and_then(|f| f.get("args"))
            .and_then(|a| a.get("command"))
            .and_then(J::as_str),
        Some("touch x")
    );
    assert!(call.get("thoughtSignature").is_some());
    let done = r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]},{"role":"model","parts":[{"functionCall":{"name":"run_shell_command"}}]},{"role":"user","parts":[{"functionResponse":{"name":"run_shell_command","response":{}}}]}],"tools":[{"functionDeclarations":[{"name":"a"}]}]}"#;
    let reply = chunks(&post(stream, done));
    assert!(parts(&reply[0])[0].py().contains("Tool call done."));

    control(&port, "/__mode?m=slow").unwrap();
    let slow = chunks(&post(stream, turn));
    assert_eq!(slow.len(), 2);
    assert_eq!(parts(&slow[0])[0].get("thought"), Some(&J::Bool(true)));
    assert_eq!(
        parts(&slow[1])[0].get("text").and_then(J::as_str),
        Some("tick 2 ")
    );

    std::fs::write(
        dir.join("modes/g401.json"),
        r#"{"status": 401, "auth": "gemini-key", "json": {"error": {"code": 401, "status": "UNAUTHENTICATED"}}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=g401").unwrap();
    assert_eq!(post(stream, turn).status(), 401);
    let counted = post("/v1beta/models/m:countTokens", turn);
    assert!(String::from_utf8_lossy(counted.body()).contains("totalTokens"));

    control(&port, "/__mode?m=ok").unwrap();
    let catalog = format!("/v1beta/models?key={}", gemini::LAB_GEMINI_KEY);
    assert_eq!(get(&port, &catalog, None).0, 200);
    assert_eq!(
        get(
            &port,
            "/v1beta/models/m",
            Some("Bearer ya29.a-google-login")
        )
        .0,
        200
    );
    let both = client
        .post(&format!("http://127.0.0.1:{port}{stream}"))
        .header("x-goog-api-key", gemini::LAB_GEMINI_KEY)
        .header("authorization", "Bearer ya29.a-google-login")
        .send(turn.as_bytes())
        .unwrap();
    assert_eq!(both.status(), 200);

    let want = [
        "GET /v1beta/models -> 200 body=0B mode=ok kind=gemini-models auth=gemini-key\n",
        "GET /v1beta/models/m -> 200 body=0B mode=ok kind=gemini-models auth=other(19 chars)\n",
        " kind=gemini auth=gemini-key+other(19 chars) model=gemini-3.5-flash ",
        ":streamGenerateContent -> 200 body=",
        " kind=gemini auth=gemini-key model=gemini-3.5-flash stream=true contents=1 tools=1 sys=0c results=0 last_user=\"hi\" sse=",
        "kind=gemini-json auth=gemini-key",
        ":streamGenerateContent -> 429 ",
        "tool:run_shell_command step 0",
        "tool:done",
    ];
    let logged = logged_with(&log, &want, Duration::from_secs(60));
    for w in want {
        assert!(logged.contains(w), "{w:?} is not in the log:\n{logged}");
    }
    assert!(!logged.contains("ya29"), "a token is never logged");
    assert!(
        !logged.contains(gemini::LAB_GEMINI_KEY),
        "the key is never logged"
    );
}

/// The modes every API answers alike (`401`, `500`, `bad-key`), in each
/// one's own shape: Claude Code's and Codex's 401s, their 500s.
#[test]
fn the_shared_wall_modes_answer_each_api_in_its_shape() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("lab-stub-walls-")
        .tempdir()
        .unwrap();
    let dir = scratch.path();
    let args = Args {
        log: Some(dir.join("stub.log")),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |path: &str, b: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .send(b.as_bytes())
            .unwrap()
    };
    let claude =
        r#"{"model":"m","stream":true,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;
    let codex = r#"{"model":"m","stream":true,"input":[{"role":"user","content":"hi"}]}"#;
    for (mode, status, claude_word, codex_word) in [
        ("401", 401, "authentication_error", "invalid_api_key"),
        ("bad-key", 401, "authentication_error", "invalid_api_key"),
        ("500", 500, "api_error", "server_error"),
    ] {
        control(&port, &format!("/__mode?m={mode}")).unwrap();
        let r = post("/v1/messages", claude);
        assert_eq!(r.status(), status, "{mode}");
        assert!(
            String::from_utf8_lossy(r.body()).contains(claude_word),
            "{mode}"
        );
        let r = post("/v1/responses", codex);
        assert_eq!(r.status(), status, "{mode}");
        assert!(
            String::from_utf8_lossy(r.body()).contains(codex_word),
            "{mode}"
        );
    }
}

/// REQUESTS IN FLIGHT TOGETHER ARE EACH DUMPED: a dump's index is taken as
/// it is written, never from the request count `record` bumps once the
/// reply's delay and pace are over — sixteen bodies dumped at once from
/// sixteen threads are sixteen files, each its own body (measured
/// 2026-10-20: a lab whose replies overlapped kept 27 dumps of 32
/// requests).
#[test]
fn requests_in_flight_together_are_each_dumped() {
    let dir = std::env::temp_dir().join(format!("lab-stub-dumps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let stub = Stub::new(Args {
        dump_dir: Some(dir.clone()),
        ..Args::default()
    });
    let gate = Arc::new(std::sync::Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|i| {
            let (stub, gate) = (Arc::clone(&stub), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                stub.dump(format!("body {i}").as_bytes(), "main");
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let mut bodies: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    bodies.sort();
    let mut want: Vec<String> = (0..16).map(|i| format!("body {i}")).collect();
    want.sort();
    assert_eq!(bodies, want);
    assert_eq!(stub.st().n_requests, 0, "the control");
    let _ = std::fs::remove_dir_all(&dir);
}
