// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the xAI dialect: its request reading, its tool script, and its answers over loopback.

use std::time::Duration;

use super::super::{Args, ToolTurn, auth_label, declared_tools, is_side, misfit_note, start};
use super::*;

fn body(v: &str) -> Vec<u8> {
    v.as_bytes().to_vec()
}

const TOOLS: &str = r#"[{"type":"function","function":{"name":"run_command","parameters":{"type":"object","required":["command"],"properties":{"command":{"type":"string"}}}}}]"#;

fn turn(messages: &str) -> Vec<u8> {
    body(&format!(
        r#"{{"model":"grok-x","stream":true,"tools":{TOOLS},"messages":[{{"role":"system","content":"be brief"}},{messages}]}}"#
    ))
}

#[test]
fn a_chat_request_is_classed_and_its_tool_script_stepped() {
    let t = turn(r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#);
    let i = analyse_chat(&t);
    assert_eq!(
        (
            i.kind,
            i.model.as_str(),
            i.stream,
            i.msgs,
            i.tools,
            i.sys_chars,
            i.last_user.as_str()
        ),
        ("grok", "grok-x", true, 2, 1, 8, "hi")
    );
    assert_eq!(analyse_chat(br#"{"messages":[]}"#).kind, "grok-aux");
    assert_eq!(
        analyse_chat(br#"{"messages":[],"response_format":{"type":"json_schema"}}"#).kind,
        "grok-json"
    );
    assert_eq!(analyse_chat(b"nope").kind, "opaque");
    let predictor = format!(
        r#"{{"messages":[{{"role":"system","content":"{PREDICTOR} into their coding agent."}},{{"role":"user","content":"Transcript: ..."}}]}}"#
    );
    assert_eq!(analyse_chat(predictor.as_bytes()).kind, "grok-suggest");
    let quoted = r#"{"messages":[{"role":"user","content":"You predict the next line the USER will type"}]}"#;
    assert_eq!(
        analyse_chat(quoted.as_bytes()).kind,
        "grok-aux",
        "a user's words are no predictor"
    );
    assert!(
        is_side("grok-aux") && is_side("grok-json") && is_side("grok-suggest") && !is_side("grok")
    );
    assert_eq!(suggestion(None), "NONE");
    assert_eq!(
        suggestion(Some(&J::parse(r#"{"suggest":"run the tests"}"#).unwrap())),
        "run the tests"
    );

    let script = J::parse(r#"[{"name":"run_command","input":{"command":"ls"}},{"name":"run_command","input":{"command":"pwd"}}]"#).unwrap();
    assert_eq!(tool_turn(&t, &script), None, "no toolstart, no call");
    let start = turn(r#"{"role":"user","content":"toolstart"}"#);
    assert_eq!(
        tool_turn(&start, &script),
        Some(ToolTurn::Call(
            "run_command".into(),
            J::parse(r#"{"command":"ls"}"#).unwrap(),
            0
        ))
    );
    let one = turn(
        r#"{"role":"user","content":"toolstart"},{"role":"assistant","content":"","tool_calls":[{"id":"c1","type":"function","function":{"name":"run_command","arguments":"{}"}}]},{"role":"tool","tool_call_id":"c1","content":"a.txt"}"#,
    );
    assert!(matches!(
        tool_turn(&one, &script),
        Some(ToolTurn::Call(_, _, 1))
    ));
    let two = turn(
        r#"{"role":"user","content":"toolstart"},{"role":"tool","content":"a"},{"role":"tool","content":"b"}"#,
    );
    assert_eq!(tool_turn(&two, &script), Some(ToolTurn::Done));
    assert_eq!(last_result(&two).as_deref(), Some("b"));
    assert_eq!(chat_users(&two), ["toolstart"]);
}

#[test]
fn a_scripted_call_is_read_against_chat_completions_tools() {
    let t = turn(r#"{"role":"user","content":"x"}"#);
    let declared = declared_tools(&t).unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].0, "run_command");
    let fits = J::parse(r#"{"command":"ls"}"#).unwrap();
    assert_eq!(misfit_note(&t, &[("run_command", &fits)]), "");
    let lacks = J::parse("{}").unwrap();
    assert!(misfit_note(&t, &[("run_command", &lacks)]).contains("lacks command"));
    assert!(misfit_note(&t, &[("bash", &fits)]).contains("no such tool"));
}

#[test]
fn scripted_tools_only_observe_results_after_the_current_persons_message() {
    let spec =
        J::parse(r#"{"tool":{"name":"run_command","input":{"command":"echo current"}}}"#).unwrap();
    let script = J::parse(r#"{"name":"run_command","input":{"command":"echo current"}}"#).unwrap();
    let person = r#"{"role":"user","content":"new toolstart"}"#;
    let result = r#"{"role":"tool","content":"current result"}"#;
    let call = |body: &[u8]| {
        let info = analyse_chat(body);
        assert_eq!(info.kind, "grok", "the main tool-offering route");
        super::super::scripted_call(&spec, info.results > 0)
    };
    let fresh = turn(person);
    assert_eq!(analyse_chat(&fresh).results, 0);
    assert!(call(&fresh).is_some(), "a new turn gets its scripted call");
    let answered = turn(&format!("{person},{result}"));
    assert_eq!(analyse_chat(&answered).results, 1);
    assert_eq!(call(&answered), None, "the current result ends the call");
    assert_eq!(last_result(&answered).as_deref(), Some("current result"));
    assert_eq!(tool_turn(&answered, &script), Some(ToolTurn::Done));

    let history =
        r#"{"role":"user","content":"old toolstart"},{"role":"tool","content":"old result"}"#;
    let next = turn(&format!("{history},{person}"));
    assert_eq!(
        analyse_chat(&next).results,
        0,
        "old results are not this turn's"
    );
    assert_eq!(
        call(&next),
        call(&fresh),
        "history must not consume the new call"
    );
    assert_eq!(tool_turn(&next, &script), tool_turn(&fresh, &script));
    assert_eq!(last_result(&next), None, "no result for the new turn yet");
    let next_answered = turn(&format!("{history},{person},{result}"));
    assert_eq!(analyse_chat(&next_answered).results, 1);
    assert_eq!(call(&next_answered), None);
    assert_eq!(
        last_result(&next_answered).as_deref(),
        Some("current result")
    );

    let ordinary = turn(&format!(
        "{history},{}",
        r#"{"role":"user","content":"an ordinary new turn"}"#
    ));
    assert_eq!(tool_turn(&ordinary, &script), None, "old toolstart is over");
    assert_eq!(last_result(&ordinary), None);
}

#[test]
fn missing_or_empty_current_user_text_never_reuses_historic_tool_results() {
    let script = J::parse(r#"{"name":"run_command","input":{"command":"echo current"}}"#).unwrap();
    for messages in [
        r#"{"role":"tool","content":"unattributed result"}"#,
        r#"{"role":"user","content":"old toolstart"},{"role":"tool","content":"old result"},{"role":"user","content":[]}"#,
    ] {
        let body = turn(messages);
        assert_eq!(analyse_chat(&body).results, 0);
        assert_eq!(last_result(&body), None);
        assert_eq!(tool_turn(&body, &script), None);
    }
    let body = turn(
        r#"{"role":"user","content":"old toolstart"},{"role":"tool","content":"old result"},{"role":"user","content":"new toolstart"},{"role":"tool","content":"first"},{"role":"assistant","content":"continuing"},{"role":"tool","content":"second"}"#,
    );
    assert_eq!(analyse_chat(&body).results, 2);
    assert_eq!(last_result(&body).as_deref(), Some("second"));
    assert_eq!(tool_turn(&body, &script), Some(ToolTurn::Done));
}

#[test]
fn the_lab_key_is_labelled_and_never_logged() {
    assert_eq!(
        auth_label(Some(format!("Bearer {LAB_XAI_KEY}").as_str())),
        "xai-key"
    );
    assert_ne!(
        auth_label(Some("Bearer xai-somebody-else")),
        "xai-key",
        "the control"
    );
}

#[test]
fn a_wall_mode_answers_in_xai_s_error_shape() {
    let (st, b, h) = mode_error("rate-limit", "7").unwrap();
    assert_eq!(st, 429);
    assert_eq!(
        b.get("code").and_then(J::as_str),
        Some("Some resource has been exhausted")
    );
    assert!(b.get("error").and_then(J::as_str).is_some());
    assert_eq!(h, [("retry-after".to_string(), "7".to_string())]);
    assert_eq!(mode_error("401", "7").unwrap().0, 401);
    assert_eq!(mode_error("ok", "7"), None);
}

fn chunks(sse: &str) -> Vec<J> {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| J::parse(d).ok())
        .collect()
}

fn delta_text(cs: &[J]) -> String {
    cs.iter()
        .filter_map(|c| {
            c.get("choices")?
                .as_arr()?
                .first()?
                .get("delta")?
                .get("content")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

fn finish(cs: &[J]) -> Option<String> {
    cs.last()?
        .get("choices")?
        .as_arr()?
        .first()?
        .get("finish_reason")?
        .as_str()
        .map(str::to_string)
}

#[test]
fn served_on_loopback_it_answers_grok_build_as_the_modes_say() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let tool_file = dir.path().join("tool.json");
    let log = dir.path().join("stub.log");
    std::fs::create_dir_all(dir.path().join("modes")).unwrap();
    std::fs::write(
        dir.path().join("modes/sug.json"),
        r#"{"suggest":"run the tests"}"#,
    )
    .unwrap();
    let (port, _) = start(Args {
        log: Some(log.clone()),
        tool_file: Some(tool_file.clone()),
        modes_dir: Some(dir.path().join("modes")),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    let key = format!("Bearer {LAB_XAI_KEY}");
    let post = |b: &[u8]| {
        client
            .post(&url("/v1/chat/completions"))
            .header("Authorization", &key)
            .send(b)
            .unwrap()
    };
    let mode =
        |m: &str| super::super::control(&port.to_string(), &format!("/__mode?m={m}")).unwrap();

    let r = client
        .get(&url("/v1/models"))
        .header("Authorization", &key)
        .send(b"")
        .unwrap();
    assert_eq!(r.status(), 200);
    // Parsing orders an object's keys, so the catalog is compared parsed too.
    assert_eq!(
        J::parse_bytes(r.body()).unwrap(),
        J::parse(&catalog().py()).unwrap()
    );

    let hi = turn(r#"{"role":"user","content":"hi"}"#);
    let r = post(&hi);
    assert_eq!(r.status(), 200);
    let sse = String::from_utf8_lossy(r.body()).into_owned();
    assert!(sse.trim_end().ends_with("data: [DONE]"), "{sse}");
    let cs = chunks(&sse);
    assert_eq!(
        delta_text(&cs),
        "Hi! I am the local lab stub, not a model. (reply 1)"
    );
    assert_eq!(finish(&cs).as_deref(), Some("stop"));
    assert!(cs.last().unwrap().get("usage").is_some());

    let quiet = body(
        r#"{"model":"grok-x","stream":false,"messages":[{"role":"user","content":"title?"}]}"#,
    );
    let r = post(&quiet);
    assert_eq!(r.status(), 200);
    let c = J::parse_bytes(r.body()).unwrap();
    assert_eq!(c.get("object").and_then(J::as_str), Some("chat.completion"));

    let predict = body(&format!(
        r#"{{"model":"grok-x","stream":true,"messages":[{{"role":"system","content":"{PREDICTOR}."}},{{"role":"user","content":"Transcript: ..."}}]}}"#
    ));
    let said = |b: &[u8]| delta_text(&chunks(&String::from_utf8_lossy(post(b).body())));
    assert_eq!(
        said(&predict),
        "NONE",
        "no suggestion unless a mode asks for one"
    );
    mode("sug");
    assert_eq!(said(&predict), "run the tests");
    mode("ok");

    std::fs::write(
        &tool_file,
        r#"{"name":"run_command","input":{"command":"ls"}}"#,
    )
    .unwrap();
    mode("tool");
    let r = post(&turn(r#"{"role":"user","content":"toolstart"}"#));
    let cs = chunks(&String::from_utf8_lossy(r.body()));
    assert_eq!(finish(&cs).as_deref(), Some("tool_calls"));
    let call = cs
        .iter()
        .find_map(|c| {
            c.get("choices")?
                .as_arr()?
                .first()?
                .get("delta")?
                .get("tool_calls")?
                .as_arr()?
                .first()
                .cloned()
        })
        .expect("a tool call");
    let f = call.get("function").unwrap();
    assert_eq!(f.get("name").and_then(J::as_str), Some("run_command"));
    assert_eq!(
        J::parse(f.get("arguments").and_then(J::as_str).unwrap()).unwrap(),
        J::parse(r#"{"command":"ls"}"#).unwrap()
    );
    let r = post(&turn(
        r#"{"role":"user","content":"toolstart"},{"role":"tool","content":"a.txt"}"#,
    ));
    assert_eq!(
        delta_text(&chunks(&String::from_utf8_lossy(r.body()))),
        "Tool call done."
    );

    for (m, want) in [("rate-limit", 429), ("401", 401), ("500", 500)] {
        mode(m);
        let r = post(&hi);
        assert_eq!(r.status(), want, "{m}");
        let e = J::parse_bytes(r.body()).unwrap();
        assert!(
            e.get("code").is_some() && e.get("error").is_some(),
            "{m}: {e:?}"
        );
    }
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("POST /v1/chat/completions -> 200"),
        "{logged}"
    );
    assert!(logged.contains("auth=xai-key"), "{logged}");
    assert!(
        !logged.contains(LAB_XAI_KEY),
        "the key's value is never logged"
    );
}
