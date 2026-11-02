// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the Chat Completions dialect's parsing and its loopback answers.

use std::time::Duration;

use super::super::{Args, ToolTurn, control, start};
use super::*;

fn body(v: &str) -> Vec<u8> {
    v.as_bytes().to_vec()
}

const TOOLS: &str = r#"[{"type":"function","function":{"name":"Bash","parameters":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}}]"#;

fn turn(msgs: &[&str]) -> Vec<u8> {
    body(&format!(
        r#"{{"model":"lab-model","stream":true,"tools":{TOOLS},"messages":[{{"role":"system","content":"be brief"}},{}]}}"#,
        msgs.join(",")
    ))
}

const PERSON: &str = r#"{"role":"user","content":[{"type":"text","text":"lab toolstart"}]}"#;
const CALLED: &str = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"Bash","arguments":"{}"}}]}"#;
const RESULT: &str = r#"{"role":"tool","tool_call_id":"c1","content":"ran ok"}"#;
/// The note Kimi Code 2.4.0 sends as a user message of its own after the person's.
const REMINDER: &str = r#"{"role":"user","content":"<system-reminder>\nToday's date is 2026-10-29.\n</system-reminder>"}"#;

#[test]
fn a_request_is_a_turn_when_it_offers_tools_and_its_user_text_is_read_from_parts() {
    let t = analyse_chat(&turn(&[PERSON]));
    assert_eq!(
        (t.kind, t.msgs, t.tools, t.stream, t.last_user.as_str()),
        ("chat", 2, 1, true, "lab toolstart")
    );
    let aux = analyse_chat(&body(
        r#"{"model":"m","messages":[{"role":"user","content":"title this"}]}"#,
    ));
    assert_eq!((aux.kind, aux.stream), ("chat-aux", false));
    assert_eq!(analyse_chat(b"not json").kind, "opaque");
    let c = catalog();
    let first = c.get("data").and_then(J::as_arr).and_then(<[J]>::first);
    assert_eq!(
        first.and_then(|m| m.get("id")).and_then(J::as_str),
        Some(MODEL)
    );
}

#[test]
fn the_tool_script_steps_once_per_result_since_the_person_s_toolstart() {
    let script = J::parse(
        r#"[{"name":"Bash","input":{"command":"echo one"}},{"when":"x","name":"skipped"},{"name":"Bash","input":{"command":"echo two"}}]"#,
    )
    .unwrap();
    let hi = r#"{"role":"user","content":"hi"}"#;
    assert_eq!(tool_turn(&turn(&[hi]), &script), None);
    let first = tool_turn(&turn(&[PERSON]), &script);
    assert!(
        matches!(&first, Some(ToolTurn::Call(n, i, 0)) if n == "Bash"
            && i.get("command").and_then(J::as_str) == Some("echo one")),
        "{first:?}"
    );
    let second = tool_turn(&turn(&[PERSON, CALLED, RESULT]), &script);
    assert!(
        matches!(&second, Some(ToolTurn::Call(_, i, 1))
            if i.get("command").and_then(J::as_str) == Some("echo two")),
        "{second:?}"
    );
    let done = turn(&[PERSON, CALLED, RESULT, CALLED, RESULT]);
    assert_eq!(tool_turn(&done, &script), Some(ToolTurn::Done));
    assert_eq!(last_tool_output(&done).as_deref(), Some("ran ok"));
    // Kimi Code's reminder after the person's message is no person's message: the script
    // still starts, steps and reads the person's text.
    let noted = tool_turn(&turn(&[PERSON, REMINDER]), &script);
    assert!(matches!(&noted, Some(ToolTurn::Call(_, _, 0))), "{noted:?}");
    let noted = tool_turn(&turn(&[PERSON, REMINDER, CALLED, RESULT]), &script);
    assert!(matches!(&noted, Some(ToolTurn::Call(_, _, 1))), "{noted:?}");
    assert_eq!(chat_users(&turn(&[PERSON, REMINDER])), ["lab toolstart"]);
    assert_eq!(
        analyse_chat(&turn(&[PERSON, REMINDER])).last_user,
        "lab toolstart"
    );
    // A newer person's message that does not say `toolstart` ends the script.
    assert_eq!(
        tool_turn(&turn(&[PERSON, CALLED, RESULT, hi]), &script),
        None
    );
    assert!(results_answered(&turn(&[PERSON, CALLED, RESULT])));
    assert!(!results_answered(&turn(&[PERSON, CALLED, RESULT, hi])));
}

fn data(sse: &str) -> Vec<String> {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(str::to_string)
        .collect()
}

fn chunks(sse: &str) -> Vec<J> {
    data(sse).iter().filter_map(|d| J::parse(d).ok()).collect()
}

fn delta_text(ch: &[J]) -> String {
    ch.iter()
        .filter_map(|c| c.get("choices").and_then(J::as_arr)?.first().cloned())
        .filter_map(|c| c.get("delta")?.get("content")?.as_str().map(str::to_string))
        .collect()
}

fn finish(ch: &[J]) -> Option<String> {
    ch.iter()
        .filter_map(|c| c.get("choices").and_then(J::as_arr)?.first().cloned())
        .find_map(|c| c.get("finish_reason")?.as_str().map(str::to_string))
}

/// A background title request spends none of a finite scripted turn's budget.
/// The actual next turn gets the one tool call, and the turn after it does not.
#[test]
fn auxiliary_chat_preserves_a_finite_tool_reply_for_the_next_turn() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir(&modes).unwrap();
    std::fs::write(
        modes.join("once.json"),
        r#"{"times":1,"replies":[{"last":"lab toolstart","text":"after the tool","tool":{"name":"Bash","input":{"command":"echo only-once"}}}]}"#,
    )
    .unwrap();
    let (port, _) = start(Args {
        modes_dir: Some(modes),
        dump_dir: Some(dir.path().join("dumps")),
        ..Args::default()
    })
    .unwrap();
    control(&port.to_string(), "/__mode?m=once").unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |body: &[u8]| {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("Authorization", "Bearer lab-not-a-key")
            .send(body)
            .unwrap()
    };
    let aux = post(br#"{"messages":[{"role":"user","content":"title this"}]}"#);
    assert_eq!(aux.status(), 200);
    let next = post(&turn(&[PERSON, REMINDER]));
    assert_eq!(next.status(), 200);
    assert_eq!(
        finish(&chunks(&String::from_utf8_lossy(next.body()))).as_deref(),
        Some("tool_calls"),
        "the auxiliary request must leave the scripted reply available"
    );
    let spent = post(&turn(&[PERSON]));
    assert_eq!(spent.status(), 200);
    assert_eq!(
        finish(&chunks(&String::from_utf8_lossy(spent.body()))).as_deref(),
        Some("stop"),
        "the real turn consumes exactly the one available reply"
    );
    let dumps: Vec<_> = std::fs::read_dir(dir.path().join("dumps"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        dumps
            .iter()
            .filter(|name| name.starts_with("side-"))
            .count(),
        1
    );
    assert_eq!(
        dumps.iter().filter(|name| name.starts_with("req-")).count(),
        2
    );
}

/// Identical model names and message bodies use the dialect of their lab key,
/// including catalogs and error shapes. Neither branch can hide the other.
#[test]
fn the_lab_key_selects_each_shared_chat_route_independently_of_the_body() {
    let (port, _) = start(Args::default()).unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let url = |path: &str| format!("http://127.0.0.1:{port}{path}");
    let body = body(&format!(
        r#"{{"model":"the-same-model","stream":false,"tools":{TOOLS},"messages":[{{"role":"user","content":"the same request"}}]}}"#
    ));
    for (key, model, xai) in [
        (super::super::LAB_CODEX_KEY, "lab-model", false),
        (super::super::xai::LAB_XAI_KEY, "lab-stub", true),
    ] {
        let authorization = format!("Bearer {key}");
        control(&port.to_string(), "/__mode?m=ok").unwrap();
        let models = client
            .get(&url("/v1/models"))
            .header("Authorization", &authorization)
            .send(b"")
            .unwrap();
        assert_eq!(models.status(), 200);
        let models = J::parse_bytes(models.body()).unwrap();
        let first = models
            .get("data")
            .and_then(J::as_arr)
            .and_then(<[J]>::first);
        assert_eq!(
            first.and_then(|item| item.get("id")).and_then(J::as_str),
            Some(model)
        );
        control(&port.to_string(), "/__mode?m=rate-limit").unwrap();
        let reply = client
            .post(&url("/v1/chat/completions"))
            .header("Authorization", &authorization)
            .send(&body)
            .unwrap();
        assert_eq!(reply.status(), 429);
        let error = J::parse_bytes(reply.body()).unwrap();
        assert_eq!(error.get("error").and_then(J::as_str).is_some(), xai);
        assert_eq!(error.get("code").is_some(), xai);
        for (mode, status) in [
            (
                format!(
                    "limit:five_hour:{}",
                    crate::util::now_secs().saturating_add(3600)
                ),
                429,
            ),
            ("limit:five_hour:1".to_string(), 200),
            ("limit:five_hour:bad".to_string(), 400),
        ] {
            control(&port.to_string(), &format!("/__mode?m={mode}")).unwrap();
            let reply = client
                .post(&url("/v1/chat/completions"))
                .header("Authorization", &authorization)
                .send(&body)
                .unwrap();
            assert_eq!(reply.status(), status, "{mode}, xai={xai}");
            if status == 429 {
                let error = J::parse_bytes(reply.body()).unwrap();
                assert_eq!(error.get("error").and_then(J::as_str).is_some(), xai);
                assert_eq!(error.get("code").is_some(), xai);
            }
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn served_on_loopback_it_answers_chat_completions_as_the_modes_say() {
    let dir = std::env::temp_dir().join(format!("lab-stub-chat-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("modes")).unwrap();
    let tool_file = dir.join("tool.json");
    let args = Args {
        log: Some(dir.join("stub.log")),
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
    let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
    let post = |b: &[u8]| {
        client
            .post(&url("/v1/chat/completions"))
            .header("Authorization", "Bearer lab-not-a-key")
            .send(b)
            .unwrap()
    };
    let hi = r#"{"role":"user","content":"hello"}"#;

    let models = client.get(&url("/v1/models")).send(b"").unwrap();
    assert_eq!(models.status(), 200);
    let m = J::parse(&String::from_utf8_lossy(models.body())).unwrap();
    assert_eq!(m, J::parse(&catalog().py()).unwrap());

    let r = post(&turn(&[hi]));
    assert_eq!(r.status(), 200);
    let sse = String::from_utf8_lossy(r.body()).into_owned();
    assert_eq!(
        data(&sse).last().map(String::as_str),
        Some("[DONE]"),
        "{sse}"
    );
    let ch = chunks(&sse);
    assert_eq!(
        delta_text(&ch),
        "Hi! I am the local lab stub, not a model. (reply 1)"
    );
    assert_eq!(finish(&ch).as_deref(), Some("stop"));
    assert!(
        ch.iter()
            .filter_map(|c| c.get("usage")?.get("total_tokens")?.as_i64())
            .any(|n| n > 0),
        "{sse}"
    );
    assert!(
        ch.iter()
            .all(|c| c.get("object").and_then(J::as_str) == Some("chat.completion.chunk"))
    );

    let plain = post(&body(
        r#"{"model":"lab-model","messages":[{"role":"user","content":"x"}]}"#,
    ));
    let p = J::parse(&String::from_utf8_lossy(plain.body())).unwrap();
    assert_eq!(p.get("object").and_then(J::as_str), Some("chat.completion"));

    control(&port, "/__mode?m=401").unwrap();
    let r = post(&turn(&[hi]));
    assert_eq!(r.status(), 401);
    assert!(String::from_utf8_lossy(r.body()).contains("invalid_api_key"));
    control(&port, "/__mode?m=rate-limit").unwrap();
    assert_eq!(post(&turn(&[hi])).status(), 429);

    control(&port, "/__mode?m=slow").unwrap();
    let ch = chunks(&String::from_utf8_lossy(post(&turn(&[hi])).body()));
    assert_eq!(delta_text(&ch), "tick 1 tick 2 ");

    std::fs::write(
        dir.join("modes/quota.json"),
        r#"{"status":429,"json":{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"exceeded_current_quota_error"}}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=quota").unwrap();
    let r = post(&turn(&[hi]));
    assert_eq!(r.status(), 429);
    assert!(String::from_utf8_lossy(r.body()).contains("exceeded_current_quota_error"));

    std::fs::write(
        dir.join("modes/ask.json"),
        r#"{"tool":{"name":"Bash","input":{"command":"echo lab"}},"text":"after the tool"}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=ask").unwrap();
    let ch = chunks(&String::from_utf8_lossy(post(&turn(&[hi])).body()));
    assert_eq!(finish(&ch).as_deref(), Some("tool_calls"));
    let args: String = ch
        .iter()
        .filter_map(|c| c.get("choices").and_then(J::as_arr)?.first().cloned())
        .filter_map(|c| {
            let call = c
                .get("delta")?
                .get("tool_calls")?
                .as_arr()?
                .first()?
                .clone();
            call.get("function")?
                .get("arguments")?
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        J::parse(&args).unwrap(),
        J::parse(r#"{"command":"echo lab"}"#).unwrap()
    );
    let after = chunks(&String::from_utf8_lossy(
        post(&turn(&[hi, CALLED, RESULT])).body(),
    ));
    assert_eq!(delta_text(&after), "after the tool");

    std::fs::write(
        &tool_file,
        r#"{"name":"Bash","input":{"command":"echo t"}}"#,
    )
    .unwrap();
    control(&port, "/__mode?m=tool").unwrap();
    let ch = chunks(&String::from_utf8_lossy(post(&turn(&[PERSON])).body()));
    assert_eq!(finish(&ch).as_deref(), Some("tool_calls"));
    let ch = chunks(&String::from_utf8_lossy(
        post(&turn(&[PERSON, CALLED, RESULT])).body(),
    ));
    assert_eq!(delta_text(&ch), "Tool call done. Result: ran ok");
    let _ = std::fs::remove_dir_all(&dir);
}
