// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn messages_split_on_nul_and_a_partial_one_waits() {
    let mut pending = b"{\"id\":1}\0{\"method\":\"x\"}\0{\"id\"".to_vec();
    let got = split_messages(&mut pending);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].get("id").and_then(Value::as_u64), Some(1));
    assert_eq!(got[1].get("method").and_then(Value::as_str), Some("x"));
    assert_eq!(pending, b"{\"id\"");
    pending.extend_from_slice(b":2}\0");
    assert_eq!(
        split_messages(&mut pending)[0]
            .get("id")
            .and_then(Value::as_u64),
        Some(2)
    );
    assert!(pending.is_empty());
    let bad = split_messages(&mut b"nope\0".to_vec());
    assert!(bad[0].get("unparsed").is_some());
}

#[test]
fn an_evaluation_answers_its_value_or_its_exception() {
    let ok = aterm_json::json!({"result": {"type": "object", "value": {"a": 1}}});
    assert_eq!(evaluated(&ok), Ok(aterm_json::json!({"a": 1})));
    let threw = aterm_json::json!({
        "result": {"type": "object"},
        "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: init(canvas): no WebGL2"}}
    });
    assert_eq!(
        evaluated(&threw),
        Err("Error: init(canvas): no WebGL2".to_string())
    );
    assert!(evaluated(&aterm_json::json!({"result": {"type": "undefined"}})).is_err());
}

#[test]
fn console_lines_name_their_kind_and_join_their_arguments() {
    let m = aterm_json::json!({"params": {"type": "error", "args": [
        {"type": "string", "value": "boom"}, {"type": "number", "value": 3},
        {"type": "object", "description": "Error: x"}
    ]}});
    assert_eq!(console_line(&m), "console.error: boom 3 Error: x");
}

#[test]
fn a_call_waits_for_its_own_answer_and_keeps_the_events_between() {
    let (to_read, to_write) = std::io::pipe().expect("pipe");
    let (from_read, mut from_write) = std::io::pipe().expect("pipe");
    let browser = std::thread::spawn(move || {
        let mut r = to_read;
        let mut pending = Vec::new();
        let mut buf = [0u8; 4096];
        let mut answered = 0;
        while answered < 2 {
            let n = r.read(&mut buf).expect("read");
            pending.extend_from_slice(&buf[..n]);
            for m in split_messages(&mut pending) {
                let id = m.get("id").and_then(Value::as_u64).expect("an id");
                let reply = if answered == 0 {
                    format!(
                        "{{\"method\":\"Page.loadEventFired\",\"sessionId\":\"s\",\"params\":{{}}}}\0\
                         {{\"id\":{id},\"result\":{{\"ok\":true}}}}\0"
                    )
                } else {
                    format!("{{\"id\":{id},\"error\":{{\"message\":\"no such method\"}}}}\0")
                };
                from_write.write_all(reply.as_bytes()).expect("write");
                answered += 1;
            }
        }
    });
    let mut cdp = Cdp::new(to_write, from_read);
    assert_eq!(
        cdp.call(Some("s"), "Page.navigate", aterm_json::json!({}))
            .expect("answered"),
        aterm_json::json!({"ok": true})
    );
    assert_eq!(
        cdp.call(None, "Nope.nope", aterm_json::json!({})),
        Err("Nope.nope: no such method".to_string())
    );
    assert!(cdp.wait_event("s", "Page.loadEventFired").is_ok());
    browser.join().expect("the stand-in");
}

#[test]
fn a_console_marker_ends_the_wait_and_the_lines_before_it_stay() {
    let (_to_read, to_write) = std::io::pipe().expect("pipe");
    let (from_read, mut from_write) = std::io::pipe().expect("pipe");
    let line = |session: &str, text: &str| {
        format!(
            "{{\"method\":\"Runtime.consoleAPICalled\",\"sessionId\":\"{session}\",\"params\":\
             {{\"type\":\"error\",\"args\":[{{\"type\":\"string\",\"value\":\"{text}\"}}]}}}}\0"
        )
    };
    let mut sent = line("s", "first");
    sent.push_str(&line("other", "done"));
    sent.push_str(&line("s", "second"));
    sent.push_str(&line("s", "done"));
    sent.push_str("{\"method\":\"Inspector.targetCrashed\",\"sessionId\":\"s\",\"params\":{}}\0");
    from_write.write_all(sent.as_bytes()).expect("write");
    let mut cdp = Cdp::new(to_write, from_read);
    assert_eq!(cdp.wait_console("s", "done"), Ok(()));
    assert_eq!(
        cdp.take_console("s"),
        ["console.error: first", "console.error: second"]
    );
    assert_eq!(
        cdp.wait_console("s", "done"),
        Err("the page crashed before \"done\"".to_string())
    );
    assert_eq!(cdp.wait_console("other", "done"), Ok(()));
}

#[test]
fn a_crash_read_during_an_earlier_call_ends_the_console_wait_at_once() {
    let (to_read, to_write) = std::io::pipe().expect("pipe");
    let (from_read, mut from_write) = std::io::pipe().expect("pipe");
    let crashed = |session: &str| {
        format!(
            "{{\"method\":\"Inspector.targetCrashed\",\"sessionId\":\"{session}\",\"params\":{{}}}}\0"
        )
    };
    let marker = |session: &str| {
        format!(
            "{{\"method\":\"Runtime.consoleAPICalled\",\"sessionId\":\"{session}\",\"params\":\
             {{\"type\":\"debug\",\"args\":[{{\"type\":\"string\",\"value\":\"done\"}}]}}}}\0"
        )
    };
    let mut sent = crashed("other");
    sent.push_str(&marker("early"));
    sent.push_str(&crashed("early"));
    sent.push_str(&crashed("s"));
    sent.push_str("{\"id\":1,\"result\":{}}\0");
    from_write.write_all(sent.as_bytes()).expect("write");
    let mut cdp = Cdp::new(to_write, from_read);
    cdp.call(Some("s"), "Runtime.evaluate", aterm_json::json!({}))
        .expect("answered");
    drop(to_read);
    assert_eq!(
        cdp.wait_console("s", "done"),
        Err("the page crashed before \"done\"".to_string())
    );
    assert_eq!(cdp.wait_console("early", "done"), Ok(()));
    drop(from_write);
}
