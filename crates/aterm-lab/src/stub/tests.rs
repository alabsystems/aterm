// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the stub on typed request bodies and over loopback.
use super::*;

fn body(v: &str) -> Vec<u8> {
    v.as_bytes().to_vec()
}

#[test]
fn a_prearmed_wall_rejects_the_next_main_request_before_any_observer_read() {
    let messages = r#"{"model":"claude-x","stream":false,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"initial task"}]}"#;
    let responses =
        r#"{"model":"gpt-x","stream":true,"input":[{"role":"user","content":"initial task"}]}"#;
    let structured =
        r#"{"input":[],"text":{"format":{"type":"json_schema","schema":{"type":"object"}}}}"#;
    let chat = r#"{"model":"chat-x","stream":false,"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}],"messages":[{"role":"user","content":"initial task"}]}"#;
    let gemini = r#"{"contents":[{"role":"user","parts":[{"text":"initial task"}]}],"tools":[{"functionDeclarations":[{"name":"read"}]}]}"#;
    let title =
        r#"{"contents":[{"role":"user","parts":[{"text":"<USER_REQUEST>title</USER_REQUEST>"}]}]}"#;
    assert_eq!(analyse(messages.as_bytes()).kind, "main");
    assert_eq!(analyse_responses(responses.as_bytes()).kind, "codex");
    assert_eq!(chat::analyse_chat(chat.as_bytes()).kind, "chat");
    assert_eq!(xai::analyse_chat(chat.as_bytes()).kind, "grok");
    assert_eq!(gemini::analyse_gemini(gemini.as_bytes()).kind, "gemini");
    let (port, _) = start(Args::default()).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |path: &str, body: &str, token: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}{path}"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("x-tbh-agent-role", "main")
            .send(body.as_bytes())
            .unwrap()
    };
    let mut wrong = Vec::new();
    for (name, path, side, main, token) in [
        (
            "Messages",
            "/v1/messages",
            r#"{"messages":[]}"#,
            messages,
            LAB_CODEX_KEY,
        ),
        (
            "Codex",
            "/v1/responses",
            structured,
            responses,
            LAB_CODEX_KEY,
        ),
        ("Muse", "/responses", structured, responses, LAB_CODEX_KEY),
        (
            "Gemini",
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent?alt=sse",
            title,
            gemini,
            gemini::LAB_GEMINI_KEY,
        ),
        (
            "Chat",
            "/v1/chat/completions",
            r#"{"messages":[]}"#,
            chat,
            LAB_CODEX_KEY,
        ),
        (
            "Kimi",
            "/v1/chat/completions",
            r#"{"messages":[]}"#,
            chat,
            "lab-kimi-not-a-key",
        ),
        (
            "Grok",
            "/v1/chat/completions",
            r#"{"messages":[]}"#,
            chat,
            xai::LAB_XAI_KEY,
        ),
    ] {
        control(&port, "/__mode?m=ok").unwrap();
        assert_eq!(
            post(path, main, token).status(),
            200,
            "{name}: ordinary main"
        );
        assert_eq!(
            post(path, main, token).status(),
            200,
            "{name}: ordinary successor"
        );
        control(&port, "/__mode?m=limit-after:1:five_hour:180000").unwrap();
        assert_eq!(post("/v1/messages/count_tokens", "{}", token).status(), 200);
        assert_eq!(
            client
                .get(&format!("http://127.0.0.1:{port}/v1/models"))
                .send(&[])
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            post(path, side, token).status(),
            200,
            "{name}: side spends no main allowance"
        );
        assert_eq!(
            post(path, main, token).status(),
            200,
            "{name}: initial task is admitted"
        );
        // No observer/status read separates these requests. The last allowed reply must
        // already have installed the wall before its continuation can be sent.
        let next = post(path, main, token).status();
        if next != 429 {
            wrong.push(format!(
                "{name}: immediate next main answered {next}, expected 429"
            ));
            continue;
        }
        let status = J::parse(&control(&port, "/__status").unwrap()).unwrap();
        let arm = status.get("limit_arm").expect("actual admission evidence");
        let raised = arm.get("raised_ms").and_then(J::as_i64).unwrap();
        let reset = arm.get("reset_s").and_then(J::as_i64).unwrap();
        assert!(raised > 0 && reset % 60 == 0);
        assert!(
            (180_000..240_000).contains(&(reset * 1000 - raised)),
            "reset is rounded from admission, not a latency measure"
        );
        control(&port, "/__mode?m=ok").unwrap();
        assert_eq!(
            post(path, main, token).status(),
            200,
            "{name}: explicit control wins"
        );
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn finite_allowance_parameters_refuse_signed_overflow_and_incomplete_fields() {
    assert_eq!(
        limit_after_params("limit-after:0:five_hour:1"),
        Some((0, "five_hour", 1, "ok"))
    );
    assert_eq!(
        limit_after_params("limit-after:2:five_hour:180000:forever"),
        Some((2, "five_hour", 180000, "forever"))
    );
    for invalid in [
        "limit-after:+1:x:1",
        "limit-after:1:x:+1",
        "limit-after:-1:x:1",
        "limit-after:18446744073709551616:x:1",
        "limit-after:1:x:9223372036854775808",
        "limit-after:1::1",
        "limit-after:1:x:0",
        "limit-after:1:x:1:ok:extra",
    ] {
        assert!(limit_after_params(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn a_prearmed_allowance_is_atomic_across_concurrent_main_admissions() {
    let mode = "limit-after:2:five_hour:180000";
    let stub = Stub::new(Args {
        mode: mode.to_string(),
        ..Args::default()
    });
    assert_eq!(stub.mode_for_request(mode, false).unwrap(), "ok");
    let ready = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let stub = Arc::clone(&stub);
            let ready = Arc::clone(&ready);
            std::thread::spawn(move || {
                ready.wait();
                stub.mode_for_request(mode, true).unwrap()
            })
        })
        .collect();
    let modes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(modes.iter().filter(|m| m.as_str() == "ok").count(), 2);
    assert_eq!(
        modes
            .iter()
            .filter(|m| m.starts_with("limit:five_hour:"))
            .count(),
        2
    );
    let state = stub.st();
    let arm = state.limit_arm.as_ref().unwrap();
    assert_eq!(arm.reset_s % 60, 0);
    assert!((180_000..240_000).contains(&(arm.reset_s * 1000 - arm.raised_ms)));
}

#[test]
fn an_unchanged_mode_file_cannot_resurrect_an_exhausted_prearm() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("mode");
    let mode = "limit-after:0:five_hour:180000";
    std::fs::write(&file, mode).unwrap();
    let stub = Stub::new(Args {
        mode_file: Some(file.clone()),
        ..Args::default()
    });
    assert_eq!(stub.mode(), mode);
    assert_eq!(stub.mode_for_request(mode, false).unwrap(), "ok");
    assert!(stub.st().limit_arm.is_none());
    assert!(
        stub.mode_for_request(mode, true)
            .unwrap()
            .starts_with("limit:five_hour:")
    );
    assert!(stub.mode().starts_with("limit:five_hour:"));
    stub.set_mode(mode).unwrap();
    assert_eq!(stub.mode(), mode);
    assert!(stub.st().limit_arm.is_none());
    assert!(
        stub.mode_for_request(mode, true)
            .unwrap()
            .starts_with("limit:five_hour:")
    );
    stub.set_mode("ok").unwrap();
    assert_eq!(
        stub.mode_for_request(mode, true).unwrap(),
        "ok",
        "an older finite snapshot cannot undo explicit control"
    );
}

fn message_delta_text(wire: &str) -> String {
    wire.lines()
        .filter_map(|line| J::parse(line.strip_prefix("data: ")?).ok())
        .filter_map(|data| data.get("delta")?.get("text")?.as_str().map(str::to_string))
        .collect()
}

#[test]
fn a_prearmed_wall_preserves_the_admitted_scripted_reply() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("once.json"),
        r#"{"times":1,"stream":true,"pace_s":0,"replies":[{"last":"initial task","text":"the scripted initial answer"}]}"#).unwrap();
    let (port, _) = start(Args {
        mode: "limit-after:1:five_hour:180000:once".to_string(),
        modes_dir: Some(dir.path().to_path_buf()),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = |body: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/messages"))
            .send(body.as_bytes())
            .unwrap()
    };
    assert_eq!(
        post(r#"{"messages":[]}"#).status(),
        200,
        "an auxiliary call spends neither finite allowance nor the scripted main reply"
    );
    let main = r#"{"model":"claude-x","stream":false,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"initial task"}]}"#;
    let first = post(main);
    assert_eq!(first.status(), 200);
    assert_eq!(
        message_delta_text(&String::from_utf8_lossy(first.body())),
        "the scripted initial answer"
    );
    assert_eq!(post(main).status(), 429);
    control(&port.to_string(), "/__reset").unwrap();
    let status = J::parse(&control(&port.to_string(), "/__status").unwrap()).unwrap();
    assert_eq!(status.get("limit_arm"), Some(&J::Null));
}

/// Capture a tiny actual protocol answer over an owned socket pair without a server or poll race.
fn wire_answer(stub: &Stub, answer: impl FnOnce(&mut Out<'_>) -> io::Result<()>) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut reader = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut writer, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    writer
        .set_write_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    {
        let mut out = Out {
            w: &mut writer,
            flat: Vec::new(),
            rate: None,
            served_200: &stub.served_200,
        };
        answer(&mut out).unwrap();
    }
    drop(writer);
    let mut answer = String::new();
    reader.read_to_string(&mut answer).unwrap();
    answer
}

fn messages_at_snapshot(stub: &Stub, snapshot: &str, body: &[u8]) -> String {
    wire_answer(stub, |out| {
        stub.messages(
            "POST",
            "/v1/messages",
            body,
            (snapshot.to_string(), "key"),
            out,
        )
    })
}

fn replaced_finite_mode_admits_its_current_allowance(from_file: bool) {
    let main = br#"{"model":"claude-x","stream":false,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"initial task"}]}"#;
    let side = br#"{"messages":[]}"#;
    assert_eq!(analyse(main).kind, "main");
    assert_ne!(analyse(side).kind, "main");
    for (allowed, reply_mode) in [(0, "ok"), (2, "ok"), (1, "once")] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("mode");
        std::fs::write(dir.path().join("once.json"), r#"{"times":1,"stream":true,"pace_s":0,"replies":[{"last":"initial task","text":"the replacement's scripted answer"}]}"#).unwrap();
        let original = "limit-after:99:five_hour:180000";
        let replacement = format!("limit-after:{allowed}:weekly:240000:{reply_mode}");
        let stub = Stub::new(Args {
            mode: original.to_string(),
            mode_file: from_file.then_some(file.clone()),
            modes_dir: Some(dir.path().to_path_buf()),
            log: Some(dir.path().join("requests")),
            ..Args::default()
        });
        // Ordinary nonfinite selection remains authoritative; it spends no global allowance.
        assert!(messages_at_snapshot(&stub, "ok", main).starts_with("HTTP/1.1 200 "));
        assert!(messages_at_snapshot(&stub, "500", main).starts_with("HTTP/1.1 500 "));
        let snapshot = stub.mode();
        assert_eq!(snapshot, original);
        if from_file {
            // The file was absent at the snapshot. Its first actual adoption is deterministic,
            // independent of filesystem timestamp resolution and without a timed rewrite.
            assert!(!file.exists());
            std::fs::write(&file, &replacement).unwrap();
            assert_eq!(stub.mode(), replacement);
        } else {
            stub.set_mode(&replacement).unwrap();
        }
        assert!(stub.st().limit_arm.is_none());
        assert!(messages_at_snapshot(&stub, &snapshot, side).starts_with("HTTP/1.1 200 "));
        {
            let state = stub.st();
            assert_eq!(state.mode, replacement);
            assert_eq!(state.limit_remaining, None);
            assert!(state.limit_arm.is_none());
            assert!(
                state.custom_budget.is_empty(),
                "the side request spends no scripted reply"
            );
        }
        for n in 0..allowed {
            let answer = messages_at_snapshot(&stub, &snapshot, main);
            assert!(
                answer.starts_with("HTTP/1.1 200 "),
                "{replacement}: {answer}"
            );
            if reply_mode == "once" {
                assert_eq!(
                    message_delta_text(&answer),
                    "the replacement's scripted answer",
                    "{answer}"
                );
            }
            let state = stub.st();
            assert_eq!(state.limit_remaining, Some(allowed - n - 1));
            assert_eq!(state.limit_arm.is_some(), n + 1 == allowed);
        }
        let refused = messages_at_snapshot(&stub, &snapshot, main);
        assert!(
            refused.starts_with("HTTP/1.1 429 "),
            "{replacement}: {refused}"
        );
        assert!(
            stub.mode().starts_with("limit:weekly:"),
            "unchanged file cannot resurrect the allowance"
        );
        {
            let state = stub.st();
            assert_eq!(state.limit_remaining, Some(0));
            let arm = state
                .limit_arm
                .as_ref()
                .expect("the current allowance installed its wall");
            assert!(arm.raised_ms > 0);
            assert!((240_000..300_000).contains(&(arm.reset_s * 1000 - arm.raised_ms)));
        }
        stub.set_mode("ok").unwrap();
        assert!(messages_at_snapshot(&stub, &snapshot, main).starts_with("HTTP/1.1 200 "));
        assert!(
            stub.st().limit_arm.is_none(),
            "explicit ordinary control still wins"
        );
    }

    for malformed in ["limit-after:bad", "limit-after:1:x:0", "LIMIT-AFTER:1:x:2"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("mode");
        let stub = Stub::new(Args {
            mode: "limit-after:1:five_hour:180000".to_string(),
            mode_file: from_file.then_some(file.clone()),
            log: Some(dir.path().join("requests")),
            ..Args::default()
        });
        let snapshot = stub.mode();
        assert_eq!(
            limit_after_params(&snapshot),
            Some((1, "five_hour", 180000, "ok"))
        );
        if from_file {
            assert!(!file.exists());
            std::fs::write(&file, malformed).unwrap();
            assert_eq!(stub.mode(), malformed);
        } else {
            stub.set_mode(malformed).unwrap();
        }
        for body in [main.as_slice(), side.as_slice()] {
            let refused = messages_at_snapshot(&stub, &snapshot, body);
            assert!(
                refused.starts_with("HTTP/1.1 400 "),
                "{malformed}: {refused}"
            );
            let error = J::parse(refused.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(
                error.get("error").unwrap().get("type").and_then(J::as_str),
                Some("invalid_finite_global_mode")
            );
            assert_eq!(stub.st().limit_remaining, None);
            assert!(stub.st().limit_arm.is_none());
        }
        let log = std::fs::read_to_string(dir.path().join("requests")).unwrap();
        assert!(
            log.contains(&format!(
                "mode={malformed} invalid finite global mode refused"
            )),
            "{log}"
        );
    }
}

#[test]
fn a_control_replacing_a_finite_snapshot_admits_only_the_new_allowance() {
    replaced_finite_mode_admits_its_current_allowance(false);
}

#[test]
fn a_mode_file_replacing_a_finite_snapshot_admits_only_the_new_allowance() {
    replaced_finite_mode_admits_its_current_allowance(true);
}

#[test]
fn every_admitting_protocol_refuses_a_reserved_mode_replaced_after_its_snapshot() {
    let original = "limit-after:1:five_hour:180000";
    let stub = Stub::new(Args {
        mode: original.to_string(),
        ..Args::default()
    });
    let snapshot = stub.mode();
    let body = b"{}";
    let req = Request {
        method: "POST".into(),
        target: "/responses".into(),
        version: "HTTP/1.1".into(),
        headers: Vec::new(),
        body: body.to_vec(),
    };
    let side = Request {
        headers: vec![("x-tbh-agent-role".into(), "subagent".into())],
        method: "POST".into(),
        target: "/responses".into(),
        version: "HTTP/1.1".into(),
        body: body.to_vec(),
    };
    for protocol in [
        "Messages",
        "Responses",
        "Muse main",
        "Muse side",
        "Chat",
        "Grok",
        "Gemini",
    ] {
        let answer_for = |mode: &str| {
            wire_answer(&stub, |out| match protocol {
                "Messages" => {
                    stub.messages("POST", "/v1/messages", body, (mode.to_string(), "key"), out)
                }
                "Responses" => stub.responses(
                    "POST",
                    "/v1/responses",
                    body,
                    mode,
                    None,
                    Dialect::Codex,
                    out,
                ),
                "Muse main" => stub.muse("POST", "/responses", body, mode, &req, out),
                "Muse side" => stub.muse("POST", "/responses", body, mode, &side, out),
                "Chat" => stub.chat("POST", "/v1/chat/completions", body, mode, None, out),
                "Grok" => stub.xai_chat("POST", "/v1/chat/completions", body, mode, "key", out),
                "Gemini" => stub.gemini(
                    "POST",
                    "/v1beta/models/lab:generateContent",
                    ("lab", gemini::Verb::Generate),
                    body,
                    mode,
                    "key",
                    out,
                ),
                _ => unreachable!(),
            })
        };
        stub.set_mode(original).unwrap();
        assert!(
            answer_for("ok").starts_with("HTTP/1.1 200 "),
            "{protocol}: ordinary control"
        );
        stub.set_mode("limit-after:bad").unwrap();
        let answer = answer_for(&snapshot);
        assert!(answer.starts_with("HTTP/1.1 400 "), "{protocol}: {answer}");
        let error = J::parse(answer.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            error.get("error").unwrap().get("type").and_then(J::as_str),
            Some("invalid_finite_global_mode"),
            "{protocol}: {error:?}"
        );
    }
    assert_eq!(stub.st().limit_remaining, None);
    assert!(stub.st().limit_arm.is_none());
}

/// A GATED RULE HOLDS ITS REPLY ON EVERY PROTOCOL until the lab opens its gate
/// ([`Stub::gated_reply`], the forever lab's need gate): a gate the lab opens later holds the
/// reply until then, and each protocol's custom reply logs its gate opened. Negative control:
/// one protocol taking its reply with `with_reply` alone (agy's Gemini did, so land-a's need came
/// and went before the lab read it).
#[test]
fn a_gated_rule_holds_its_reply_on_every_protocol() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("stub.log");
    std::fs::write(
        dir.path().join("gated.json"),
        r#"{"text":"LANDED","gate":"g"}"#,
    )
    .unwrap();
    let stub = Stub::new(Args {
        mode: "gated".to_string(),
        modes_dir: Some(dir.path().to_path_buf()),
        log: Some(log.clone()),
        ..Args::default()
    });
    let opened = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .matches("gate g opened")
            .count()
    };
    let body = b"{}";
    // Grok and Gemini serve a custom mode to a main turn alone, one that offers tools.
    let grok = br#"{"model":"grok-x","stream":true,"tools":[{"type":"function","function":{"name":"run_command"}}],"messages":[{"role":"user","content":"hi"}]}"#;
    let gemini_turn = br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"tools":[{"functionDeclarations":[{"name":"read"}]}]}"#;
    let req = Request {
        method: "POST".into(),
        target: "/responses".into(),
        version: "HTTP/1.1".into(),
        headers: Vec::new(),
        body: body.to_vec(),
    };
    let answer_for = |protocol: &str| {
        wire_answer(&stub, |out| match protocol {
            "Messages" => stub.messages(
                "POST",
                "/v1/messages",
                body,
                ("gated".to_string(), "key"),
                out,
            ),
            "Responses" => stub.responses(
                "POST",
                "/v1/responses",
                body,
                "gated",
                None,
                Dialect::Codex,
                out,
            ),
            "Muse" => stub.muse("POST", "/responses", body, "gated", &req, out),
            "Chat" => stub.chat("POST", "/v1/chat/completions", body, "gated", None, out),
            "Grok" => stub.xai_chat("POST", "/v1/chat/completions", grok, "gated", "key", out),
            "Gemini" => stub.gemini(
                "POST",
                "/v1beta/models/lab:generateContent",
                ("lab", gemini::Verb::Generate),
                gemini_turn,
                "gated",
                "key",
                out,
            ),
            _ => unreachable!(),
        })
    };
    let gate = gate_path(dir.path(), "g");
    let late = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(&gate, b"").unwrap();
        })
    };
    let began = std::time::Instant::now();
    let first = answer_for("Messages");
    late.join().unwrap();
    assert!(
        began.elapsed() >= Duration::from_millis(300),
        "the reply waited for its gate"
    );
    assert!(first.starts_with("HTTP/1.1 200 "), "{first}");
    assert_eq!(opened(), 1, "Messages");
    for (n, protocol) in ["Responses", "Muse", "Chat", "Grok", "Gemini"]
        .into_iter()
        .enumerate()
    {
        let answer = answer_for(protocol);
        assert!(answer.starts_with("HTTP/1.1 200 "), "{protocol}: {answer}");
        assert_eq!(opened(), n + 2, "{protocol}: its reply held for the gate");
    }
}

#[test]
fn finite_modes_are_global_without_swallowing_credential_override_precedence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("auth.json");
    std::fs::write(&file, r#"{"key":"500"}"#).unwrap();
    let mode = "limit-after:1:five_hour:180000";
    let (port, _) = start(Args {
        mode: mode.to_string(),
        auth_modes_file: Some(file.clone()),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let main = r#"{"model":"claude-x","stream":false,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"initial task"}]}"#;
    let post = || {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/messages"))
            .header("Authorization", &format!("Bearer {LAB_CODEX_KEY}"))
            .send(main.as_bytes())
            .unwrap()
    };
    assert_eq!(
        post().status(),
        500,
        "existing nonfinite credential precedence remains"
    );
    std::fs::write(&file, format!(r#"{{"key":"{mode}"}}"#)).unwrap();
    let refused = post();
    assert_eq!(
        refused.status(),
        400,
        "even an equal string has explicit credential provenance"
    );
    assert!(String::from_utf8_lossy(refused.body()).contains("finite_mode_requires_global"));
    std::fs::write(&file, r#"{"key":"off"}"#).unwrap();
    assert_eq!(
        post().status(),
        200,
        "neither override spent global allowance"
    );
    assert_eq!(post().status(), 429);
    for invalid in [
        "limit-after:bad",
        "limit-after:1:x:0",
        "limit-after:1:x:2:ok:extra",
    ] {
        control(&port.to_string(), &format!("/__mode?m={invalid}")).unwrap();
        let answer = post();
        assert_eq!(answer.status(), 400);
        assert!(String::from_utf8_lossy(answer.body()).contains("invalid_finite_global_mode"));
    }
}

#[test]
fn prearmed_side_roles_and_grok_side_forms_spend_no_main_allowance() {
    let (port, _) = start(Args {
        mode: "limit-after:1:five_hour:180000".to_string(),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let responses = r#"{"input":[{"role":"user","content":"hi"}]}"#;
    assert_eq!(
        client
            .post(&format!("http://127.0.0.1:{port}/responses"))
            .header("x-tbh-agent-role", "subagent")
            .send(responses.as_bytes())
            .unwrap()
            .status(),
        200
    );
    let auth = format!("Bearer {}", xai::LAB_XAI_KEY);
    let post = |body: &str| {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("Authorization", &auth)
            .send(body.as_bytes())
            .unwrap()
    };
    let predict = format!(
        r#"{{"messages":[{{"role":"system","content":"{} into the coding agent."}}]}}"#,
        xai::PREDICTOR
    );
    for side in [
        r#"{"messages":[]}"#,
        r#"{"messages":[],"response_format":{"type":"json_schema"}}"#,
        predict.as_str(),
    ] {
        assert_ne!(xai::analyse_chat(side.as_bytes()).kind, "grok");
        assert_eq!(post(side).status(), 200);
    }
    let main = r#"{"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}],"messages":[{"role":"user","content":"initial task"}]}"#;
    assert_eq!(post(main).status(), 200);
    assert_eq!(post(main).status(), 429);
    assert_eq!(
        post(&predict).status(),
        200,
        "Grok's existing side-before-error semantics remain after exhaustion"
    );
}

#[test]
fn a_scripted_call_is_read_against_the_tools_its_request_declares() {
    let call = |v: &str| J::parse(v).unwrap();
    // agy 1.4.10's shell tool: closed, with arguments it requires.
    let gemini = body(
        r#"{"contents":[],"tools":[{"functionDeclarations":[{"name":"run_command","parametersJsonSchema":{"type":"object","properties":{"CommandLine":{"type":"string"},"toolSummary":{"type":"string"},"WaitMsBeforeAsync":{"type":"integer"}},"required":["CommandLine","toolSummary","WaitMsBeforeAsync"],"additionalProperties":false}}]}]}"#,
    );
    let fits = call(r#"{"CommandLine":"ls","toolSummary":"list","WaitMsBeforeAsync":500}"#);
    assert_eq!(misfit_note(&gemini, &[("run_command", &fits)]), "");
    let old =
        call(r#"{"CommandLine":"ls","Cwd":"/tmp","toolAction":"list","WaitMsBeforeAsync":500}"#);
    assert_eq!(
        misfit_note(&gemini, &[("run_command", &old)]),
        " misfit='run_command: lacks toolSummary, undeclared Cwd, undeclared toolAction'"
    );
    assert_eq!(
        misfit_note(&gemini, &[("view_file", &fits)]),
        " misfit='view_file: no such tool declared'"
    );

    // An open schema takes arguments it does not name; one a Messages API tool requires is still
    // missed, and a tool with no schema (a server tool) takes any.
    let messages = body(
        r#"{"messages":[],"tools":[{"name":"Bash","input_schema":{"type":"object","properties":{"command":{}},"required":["command"]}},{"type":"web_search_20250305","name":"web_search"}]}"#,
    );
    let extra = call(r#"{"command":"ls","timeout":5}"#);
    let none = call("{}");
    assert_eq!(misfit_note(&messages, &[("Bash", &extra)]), "");
    assert_eq!(misfit_note(&messages, &[("web_search", &extra)]), "");
    assert_eq!(
        misfit_note(
            &messages,
            &[("Bash", &extra), ("Bash", &none), ("Read", &none)]
        ),
        " misfit='Bash: lacks command; Read: no such tool declared'"
    );

    // Muse Code's tools sit in a namespace.
    let muse = body(
        r#"{"input":[],"tools":[{"type":"namespace","name":"muse","tools":[{"type":"function","name":"bash","parameters":{"type":"object","properties":{"command":{}},"required":["command"],"additionalProperties":false}}]}]}"#,
    );
    assert_eq!(
        misfit_note(&muse, &[("bash", &call(r#"{"command":"ls"}"#))]),
        ""
    );
    assert_eq!(
        misfit_note(&muse, &[("bash", &call(r#"{"cmd":"ls"}"#))]),
        " misfit='bash: lacks command, undeclared cmd'"
    );

    // A request that declares no named tool has nothing to read a call against.
    for empty in [r#"{"tools":[{}]}"#, r#"{"messages":[]}"#, "not json"] {
        assert_eq!(declared_tools(&body(empty)), None, "{empty}");
        assert_eq!(misfit_note(&body(empty), &[("Bash", &none)]), "", "{empty}");
    }
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
fn received_user_digests_keep_complete_text_and_literal_block_newlines() {
    let string = analyse(&body(r#"{"messages":[{"role":"user","content":"α\nβ "}]}"#));
    let blocks = analyse(&body(
        r#"{"messages":[{"role":"user","content":[{"type":"text","text":"α"},{"type":"tool_result","text":"ignored"},{"type":"text","text":"β "}]}]}"#,
    ));
    assert_eq!(string.last_user, "α\nβ ");
    assert_eq!(blocks.last_user, string.last_user);
    let whole = message_digest(&string.last_user);
    assert_eq!(message_digest(&blocks.last_user), whole);
    for changed in ["α\nβ", "α β ", "α\nb "] {
        assert_ne!(message_digest(changed), whole, "{changed:?}");
    }
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

/// A custom mode's `script` answers only a conversation whose first prompt holds a step's `when`,
/// step by step; every other conversation, `toolstart` in it or not, is the mode's replies', so a
/// lane can script one session (aterm's disk worker) beside tabs its replies answer.
#[test]
fn a_custom_scripts_keyed_steps_answer_only_their_own_conversation() {
    let script = J::parse(
        r#"[{"when":"(rule x worker)","name":"Bash","input":{"command":"rm -rf c"}},
            {"when":"(rule x worker)","text":"Nothing left: kept it."},
            {"name":"Read","input":{}}]"#,
    )
    .unwrap();
    let start = r#"{"role":"user","content":[{"type":"text","text":"<system-reminder>r</system-reminder>"},{"type":"text","text":"Free it. (rule x worker)"}]}"#;
    let used = r#"{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}"#;
    let declined = r#"{"role":"user","content":[{"type":"tool_result","is_error":true,"content":"declined: keep it"}]}"#;
    let said =
        r#"{"role":"assistant","content":[{"type":"text","text":"Nothing left: kept it."}]}"#;
    let req = |ms: &[&str]| body(&format!(r#"{{"messages":[{}]}}"#, ms.join(",")));
    match keyed_tool_turn(&req(&[start]), &script) {
        Some(ToolTurn::Call(name, input, 0)) => {
            assert_eq!(name, "Bash");
            assert_eq!(input.get("command").and_then(J::as_str), Some("rm -rf c"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        keyed_tool_turn(&req(&[start, used, declined]), &script),
        Some(ToolTurn::Say("Nothing left: kept it.".into(), 1))
    );
    assert_eq!(
        keyed_tool_turn(&req(&[start, used, declined, said]), &script),
        Some(ToolTurn::Done)
    );
    let tab = r#"{"role":"user","content":"LABDISK-alpha: keep building"}"#;
    assert_eq!(keyed_tool_turn(&req(&[tab]), &script), None);
    let toolstart = r#"{"role":"user","content":"toolstart"}"#;
    assert_eq!(
        keyed_tool_turn(&req(&[toolstart]), &script),
        None,
        "a main step is the tool mode's, never a custom script's"
    );
    assert!(
        matches!(tool_turn(&req(&[toolstart]), &script), Some(ToolTurn::Call(n, _, 0)) if n == "Read"),
        "the control: the tool mode answers it"
    );
}

/// Served on loopback, a custom mode with replies and a `script` streams the keyed conversation's
/// tool call and every other conversation its reply.
#[test]
fn served_a_custom_modes_script_calls_for_its_conversation_and_replies_to_the_rest() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mode_file = dir.path().join("mode");
    std::fs::write(&mode_file, "scripted\n").unwrap();
    std::fs::create_dir_all(dir.path().join("modes")).unwrap();
    std::fs::write(
        dir.path().join("modes/scripted.json"),
        r#"{"stream":true,"text":"Working on it.","replies":[{"last":"(rule x)","text":"Asked."}],
            "script":[{"when":"(rule x worker)","name":"Bash","input":{"command":"aterm harness disk free --dry-run"}}]}"#,
    )
    .unwrap();
    let args = Args {
        mode_file: Some(mode_file),
        modes_dir: Some(dir.path().join("modes")),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let events = |first: &str| -> Vec<J> {
        let turn = format!(
            r#"{{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{{}}],"messages":[{{"role":"user","content":"{first}"}}]}}"#
        );
        let r = client
            .post(&format!("http://127.0.0.1:{port}/v1/messages"))
            .send(turn.as_bytes())
            .unwrap();
        assert_eq!(r.status(), 200);
        String::from_utf8_lossy(r.body())
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter_map(|d| J::parse(d).ok())
            .collect()
    };
    let text = |ev: &[J]| -> String {
        ev.iter()
            .filter_map(|e| e.get("delta")?.get("text")?.as_str().map(str::to_string))
            .collect()
    };
    let tool = |ev: &[J]| -> Option<String> {
        ev.iter().find_map(|e| {
            let b = e.get("content_block")?;
            if b.get("type")?.as_str()? != "tool_use" {
                return None;
            }
            b.get("name")?.as_str().map(str::to_string)
        })
    };
    let worker = events("Free it. (rule x worker)");
    assert_eq!(tool(&worker).as_deref(), Some("Bash"), "{worker:?}");
    let tab = events("keep building");
    assert_eq!(tool(&tab), None, "{tab:?}");
    assert_eq!(text(&tab), "Working on it.");
    let asked = events("free space (rule x)");
    assert_eq!(text(&asked), "Asked.", "{asked:?}");
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

#[test]
fn served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(scratch.path()).unwrap();
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
        " kind=main model=claude-x stream=true max_tokens=32000 msgs=1 tools=1 sys=0c last_user=\"hi\" auth=none sse=",
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
}

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
    let saturated = flat_headers(
        &J::parse(r#"{"X-Reset":"@NOW+100@","X-Huge":"@NOW+99999999999999999999999999@"}"#)
            .unwrap(),
        i64::MAX - 10,
    );
    assert_eq!(
        saturated,
        vec![
            ("x-huge".to_string(), i64::MAX.to_string()),
            ("x-reset".to_string(), i64::MAX.to_string()),
        ]
    );
}

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

#[test]
fn a_codex_websocket_probe_leaves_the_finite_post_reply_available() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("policy.json"),
        r#"{"times":1,"status":400,"json":{"error":{"type":"invalid_request_error","code":"cyber_policy","message":"lab policy refusal"}}}"#,
    )
    .unwrap();
    let (port, _) = start(Args {
        mode: "policy".into(),
        modes_dir: Some(dir.path().to_path_buf()),
        ..Args::default()
    })
    .unwrap();
    let get = |path: &str, headers: &str| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n{headers}\r\n").as_bytes())
            .unwrap();
        let mut answer = String::new();
        s.read_to_string(&mut answer).unwrap();
        answer
    };
    let websocket = "cOnNeCtIoN: cLoSe, UpGrAdE\r\nuPgRaDe: WebSocket\r\n";
    for (path, headers, status) in [
        ("/v1/responses", "Connection: close\r\n", 404),
        (
            "/v1/responses",
            "Connection: close\r\nUpgrade: websocket\r\n",
            404,
        ),
        (
            "/v1/responses",
            "Connection: close, upgrade\r\nUpgrade: h2c\r\n",
            404,
        ),
        ("/responses", websocket, 404),
        ("/v1/responses/other", websocket, 404),
        ("/v1/responses?model=gpt-5.5", websocket, 426),
        (
            "/v1/responses",
            "Connection: close\r\nConnection: upgrade\r\nUpgrade: websocket\r\n",
            426,
        ),
    ] {
        let answer = get(path, headers);
        let reason = reason(status);
        assert!(
            answer.starts_with(&format!("HTTP/1.1 {status} {reason}\r\n")),
            "{path}: {answer}"
        );
    }
    let status = J::parse(&control(&port.to_string(), "/__status").unwrap()).unwrap();
    assert_eq!(status.get("custom_budget").unwrap().get("policy"), None);
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let post = || {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/responses"))
            .send(br#"{"model":"gpt-5.5","stream":true,"input":[{"role":"user","content":"hi"}]}"#)
            .unwrap()
    };
    let refused = post();
    assert_eq!(refused.status(), 400);
    let error = J::parse(&String::from_utf8_lossy(refused.body())).unwrap();
    assert_eq!(
        error.get("error").unwrap().get("code").and_then(J::as_str),
        Some("cyber_policy")
    );
    let recovered = post();
    assert_eq!(recovered.status(), 200);
    assert_eq!(
        recovered.header("content-type"),
        Some("text/event-stream; charset=utf-8")
    );
    assert!(String::from_utf8_lossy(recovered.body()).contains("event: response.completed\n"));
}

#[test]
fn a_wham_usage_read_echoes_only_the_selected_account_header() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let headers_file = dir.path().join("headers.json");
    // Fixed past deadlines keep payload equality independent of request timing.
    std::fs::write(
        &headers_file,
        r#"{"x-codex-primary-used-percent":"18","x-codex-primary-window-minutes":"300","x-codex-primary-reset-at":"0","x-codex-secondary-used-percent":"30","x-codex-secondary-window-minutes":"10080","x-codex-secondary-reset-at":"0"}"#,
    )
    .unwrap();
    let (port, _) = start(Args {
        headers_file: Some(headers_file),
        ..Args::default()
    })
    .unwrap();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let auth = J::parse(&crate::codex::auth_json("A", "acct-lab-1", "pro", 0)).unwrap();
    let token = auth
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(J::as_str)
        .unwrap();
    let read = |account: Option<&str>| {
        let mut request = client
            .get(&format!("http://127.0.0.1:{port}/backend-api/wham/usage"))
            .header("Authorization", &format!("Bearer {token}"));
        if let Some(account) = account {
            request = request.header("ChatGPT-Account-Id", account);
        }
        let response = request.send(&[]).unwrap();
        assert_eq!(response.status(), 200);
        J::parse(&String::from_utf8_lossy(response.body())).unwrap()
    };
    let missing = read(None);
    assert_eq!(missing.get("account_id"), None);
    assert_eq!(missing.get("plan_type").and_then(J::as_str), Some("pro"));
    let limits = J::parse(
        r#"{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":18,"limit_window_seconds":18000,"reset_after_seconds":0,"reset_at":0},"secondary_window":{"used_percent":30,"limit_window_seconds":604800,"reset_after_seconds":0,"reset_at":0}}"#,
    )
    .unwrap();
    assert_eq!(missing.get("rate_limit"), Some(&limits));
    assert_eq!(
        read(Some("")),
        missing,
        "an empty header invents no identity"
    );

    let selected = read(Some("acct-lab-1"));
    let J::Obj(mut without_identity) = selected.clone() else {
        panic!("the usage reply must remain an object");
    };
    without_identity.retain(|(key, _)| key != "account_id");
    assert_eq!(
        J::Obj(without_identity),
        missing,
        "selecting the fake account changes no existing quota fields"
    );
    assert_eq!(
        selected.get("account_id").and_then(J::as_str),
        Some("acct-lab-1"),
        "the usage read must identify the selected account for durable quota history"
    );
}

#[test]
fn served_on_loopback_it_answers_codex_as_the_modes_say() {
    let dir = crate::TestScratch::new("codex");
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
}

#[test]
fn served_on_loopback_the_account_routes_answer_from_the_account_file() {
    let dir = crate::TestScratch::new("account");
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
}

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
    let top = aterm_tempfile::tempdir().unwrap();
    let modes = top.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(top.path().join("escape.json"), r#"{"status":418}"#).unwrap();
    std::fs::write(modes.join("v1..v2.json"), r#"{"status":418}"#).unwrap();
    let s = Stub::new(Args {
        modes_dir: Some(modes),
        ..Args::default()
    });
    assert!(s.custom_spec(".").is_none());
    assert!(s.custom_spec("..").is_none());
    assert!(s.custom_spec("../escape").is_none());
    assert!(s.custom_spec("c:escape").is_none());
    assert!(s.custom_spec("v1..v2").is_some());
    super::sleep_secs(f64::NAN);
    super::sleep_secs(f64::INFINITY);
    super::sleep_secs(1e100);
    super::sleep_secs(-1.0);
    let hdr = J::parse(r#"{"headers":{"x-ok":"a\r\nb","bad\r\n":"1"}}"#).unwrap();
    assert_eq!(
        super::spec_headers(&hdr),
        vec![("x-ok".to_string(), "a  b".to_string())]
    );
    let mut huge =
        std::io::Cursor::new(b"POST /v1/messages HTTP/1.1\r\nContent-Length: 999999999\r\n\r\n");
    assert!(read_request(&mut huge).is_err());
}

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
        r#"[{"name":"invoke_subagent","input":{"Prompt":"LABSUB-HOLD reply DONE","Role":"Lab Waiter","TypeName":"research"}},{"when":"LABSUB-HOLD","text":"DONE","delay_ms":150000}]"#,
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
        r#"{"contents":[{"role":"user","parts":[{"text":"toolstart"}]},{"role":"model","parts":[{"functionCall":{"name":"invoke_subagent","args":{"Prompt":"LABSUB-HOLD reply DONE"}}}]}]}"#,
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
    control(&port, &format!("/__mode?m=limit:five_hour:{future}")).unwrap();
    let key = String::from_utf8_lossy(post(stream, turn).body()).into_owned();
    assert!(
        key.contains("PerDay") && !key.contains("QUOTA_EXHAUSTED"),
        "the control: {key}"
    );
    let side_answer = |p: &str| -> String {
        let side = post("/v1beta/models/gemini-3.1-flash-lite:generateContent", p);
        assert_eq!(side.status(), 200);
        let answer = J::parse(&String::from_utf8_lossy(side.body())).unwrap();
        parts(&answer)[0]
            .get("text")
            .and_then(J::as_str)
            .unwrap()
            .to_string()
    };
    let classifier = r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"responseJsonSchema":{"type":"object","properties":{"reasoning":{"type":"string"},"model_choice":{"type":"string","enum":["flash","pro"]}},"required":["reasoning","model_choice"]}}}"#;
    // The retired Gemini CLI's Google-account claims are no wall of their own:
    // each is an API key's spent per-day quota, and no side answer is routed.
    for claim in ["code_assist", "code_assist_pro"] {
        control(&port, &format!("/__mode?m=limit:{claim}:{future}")).unwrap();
        let wall = String::from_utf8_lossy(post(stream, turn).body()).into_owned();
        assert!(
            wall.contains("PerDay") && !wall.contains("QUOTA_EXHAUSTED"),
            "{claim}: {wall}"
        );
        assert_eq!(side_answer(router), r#"{"complexity_score":10}"#, "{claim}");
        assert!(
            side_answer(classifier).contains(r#""model_choice":"flash""#),
            "{claim}"
        );
    }

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
        "tool:run_shell_command step 0 misfit='run_shell_command: no such tool declared'",
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

#[test]
fn requests_in_flight_together_are_each_dumped() {
    let scratch = crate::TestScratch::new("dumps");
    let dir = scratch.join("dumps");
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
}

#[test]
fn a_labelled_claude_token_reads_as_its_label_and_any_other_as_before() {
    let a = labelled_oauth_token("lab-a");
    assert!(
        a.starts_with(concat!("sk-ant-oat01-LABLABLAB", "-lab-a-LAB")),
        "{a}"
    );
    assert_eq!(auth_label(Some(&format!("Bearer {a}"))), "oat:lab-a");
    assert_eq!(auth_label(Some(&a)), "oat:lab-a", "no bearer word");
    assert_eq!(
        auth_label(Some(&format!("Bearer {}", labelled_oauth_token("b_2")))),
        "oat:b_2"
    );
    assert_eq!(
        auth_label(Some(&format!("Bearer {}", crate::lab::oauth_token()))),
        "oat",
        "the unlabelled lab token"
    );
    for bad in ["", "Lab-A", "a.b", &"x".repeat(25)] {
        assert_eq!(
            auth_label(Some(&format!("Bearer {}", labelled_oauth_token(bad)))),
            "oat",
            "{bad:?}: no label it could carry"
        );
    }
    assert_eq!(
        auth_label(Some("Bearer sk-ant-oat01-LABLABLAB-lab-a")),
        "oat",
        "no end mark"
    );
    for label in ["lab-a", "b_2"] {
        let label_of = auth_label(Some(&labelled_oauth_token(label)));
        assert!(!label_of.contains("AAAA"), "no token is echoed: {label_of}");
    }
    assert_eq!(auth_label(None), "none");
    assert_eq!(auth_label(Some(LAB_CODEX_KEY)), "key");
}

#[test]
fn each_credential_reads_its_own_mode_and_its_own_headers_over_the_shared_ones() {
    let modes =
        J::parse(r#"{"oat:lab-a":"limit:five_hour:1800000000","oat:lab-b":"off","key":" ","x":7}"#)
            .unwrap();
    assert_eq!(
        auth_mode(&modes, "oat:lab-a").as_deref(),
        Some("limit:five_hour:1800000000")
    );
    for label in ["oat:lab-b", "key", "x", "oat:lab-c"] {
        assert_eq!(auth_mode(&modes, label), None, "{label}");
    }
    assert_eq!(auth_mode(&J::parse("[1]").unwrap(), "oat:lab-a"), None);
    assert_eq!(
        auth_mode(&J::parse(r#"{"k":"ok; rm"}"#).unwrap(), "k").as_deref(),
        Some("okrm"),
        "cleaned as /__mode cleans one"
    );

    let spec = J::parse(
        r#"{"on200":{"anthropic-ratelimit-unified-5h-utilization":"0.10","anthropic-ratelimit-unified-7d-utilization":"0.20"},
            "by_auth":{"oat:lab-a":{"on200":{"anthropic-ratelimit-unified-5h-utilization":"0.95"},"on429":{"x-a":"1"}}}}"#,
    )
    .unwrap();
    let a = rate_headers_for(&spec, "oat:lab-a", 0);
    let b = rate_headers_for(&spec, "oat:lab-b", 0);
    let five = |r: &RateHeaders| {
        r.on200
            .iter()
            .find(|(k, _)| k == "anthropic-ratelimit-unified-5h-utilization")
            .map(|(_, v)| v.clone())
    };
    assert_eq!(five(&a).as_deref(), Some("0.95"));
    assert_eq!(five(&b).as_deref(), Some("0.10"));
    assert_eq!(a.on200.len(), 2, "the shared 7d figure kept: {a:?}");
    assert_eq!(a.on429, vec![("x-a".to_string(), "1".to_string())]);
    assert_eq!(
        b,
        rate_headers(&spec, 0),
        "a credential with no blocks reads the shared ones"
    );

    let file = headers_file_for(
        &spec.py(),
        "oat:lab-b",
        J::parse(r#"{"on200":{"y":"2"}}"#).unwrap(),
    );
    assert!(
        file.get(BY_AUTH)
            .and_then(|by| by.get("oat:lab-a"))
            .is_some()
    );
    assert_eq!(
        rate_headers_for(&file, "oat:lab-b", 0).on200.last(),
        Some(&("y".to_string(), "2".to_string()))
    );
    let headers = headers_file_with(
        &file.py(),
        J::parse(r#"{"on200":{"z":"3"}}"#).unwrap(),
        true,
    );
    let flat = headers_file_with(&headers.py(), J::parse(r#"{"x-f":"4"}"#).unwrap(), false);
    assert_eq!(
        flat.get(BY_AUTH),
        file.get(BY_AUTH),
        "neither headers verb touches a credential's own blocks"
    );
    assert_eq!(
        headers_file_with("{}", J::parse(r#"{"by_auth":{"k":{}}}"#).unwrap(), true).get(BY_AUTH),
        None,
        "nor writes them"
    );
    let cleared = headers_file_for(&file.py(), "oat:lab-a", J::parse("{}").unwrap());
    let cleared = headers_file_for(&cleared.py(), "oat:lab-b", J::parse("{}").unwrap());
    assert_eq!(
        cleared.get(BY_AUTH),
        None,
        "the last credential's blocks gone"
    );
    assert!(cleared.get("on200").is_some());
}

/// One raw POST of `body` to `path` carrying `auth`: its status, its header lines and its body.
fn post_as(port: &str, path: &str, auth: Option<&str>, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    let auth = auth
        .map(|a| format!("Authorization: {a}\r\n"))
        .unwrap_or_default();
    s.write_all(
        format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
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
    let (head, body) = answer.split_once("\r\n\r\n").unwrap_or((&answer, ""));
    (status, head.to_lowercase(), body.to_string())
}

#[test]
fn served_on_loopback_each_credential_meets_its_own_mode_and_its_own_headers() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let dir = dir.path();
    let log = dir.join("stub.log");
    let auth_modes = dir.join("auth-modes.json");
    let headers = dir.join("headers.json");
    let args = Args {
        log: Some(log.clone()),
        auth_modes_file: Some(auth_modes.clone()),
        headers_file: Some(headers.clone()),
        modes_dir: Some(dir.join("modes")),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let (a, b) = (labelled_oauth_token("lab-a"), labelled_oauth_token("lab-b"));
    let (bearer_a, bearer_b) = (format!("Bearer {a}"), format!("Bearer {b}"));
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;

    let reset = crate::util::now_secs() + 600;
    std::fs::write(
        &auth_modes,
        format!(r#"{{"oat:lab-a":"limit:five_hour:{reset}"}}"#),
    )
    .unwrap();
    std::fs::write(
        &headers,
        r#"{"on200":{"anthropic-ratelimit-unified-5h-utilization":"0.10"},"by_auth":{"oat:lab-b":{"on200":{"anthropic-ratelimit-unified-5h-utilization":"0.95"}}}}"#,
    )
    .unwrap();
    let (st, head, _) = post_as(&port, "/v1/messages", Some(&bearer_a), turn);
    assert_eq!(st, 429, "lab-a at its limit");
    assert!(
        head.contains("anthropic-ratelimit-unified-status: rejected"),
        "{head}"
    );
    assert!(
        head.contains(&format!("anthropic-ratelimit-unified-reset: {reset}")),
        "{head}"
    );
    let (st, head, _) = post_as(&port, "/v1/messages", Some(&bearer_b), turn);
    assert_eq!(
        st, 200,
        "lab-b, which no mode names, in the stub's own mode"
    );
    assert!(
        head.contains("anthropic-ratelimit-unified-5h-utilization: 0.95"),
        "{head}"
    );
    let (st, head, _) = post_as(&port, "/v1/messages", None, turn);
    assert_eq!(st, 200);
    assert!(
        head.contains("anthropic-ratelimit-unified-5h-utilization: 0.10"),
        "{head}"
    );

    std::fs::write(&auth_modes, r#"{"oat:lab-a":"limit:five_hour:1"}"#).unwrap();
    assert_eq!(
        post_as(&port, "/v1/messages", Some(&bearer_a), turn).0,
        200,
        "lab-a's limit lifts at its reset"
    );

    std::fs::create_dir_all(dir.join("modes")).unwrap();
    std::fs::write(
        dir.join("modes/gated.json"),
        r#"{"status":418,"auth":"oat:lab-b","json":{"error":{"type":"x"}}}"#,
    )
    .unwrap();
    std::fs::write(&auth_modes, r#"{"oat:lab-a":"gated","oat:lab-b":"gated"}"#).unwrap();
    assert_eq!(
        post_as(&port, "/v1/messages", Some(&bearer_b), turn).0,
        418,
        "a custom mode with auth answers the credential it names"
    );
    assert_eq!(
        post_as(&port, "/v1/messages", Some(&bearer_a), turn).0,
        200,
        "and no other"
    );

    let want = [
        "POST /v1/messages -> 429 body=",
        " last_user=\"hi\" auth=oat:lab-a",
        "mode=limit:five_hour:",
        " last_user=\"hi\" auth=oat:lab-b RL200[5h-utilization=0.95]",
        " last_user=\"hi\" auth=none RL200[5h-utilization=0.10]",
        "POST /v1/messages -> 418 body=",
    ];
    let logged = logged_with(&log, &want, Duration::from_secs(60));
    for w in want {
        assert!(logged.contains(w), "{w:?} is not in the log:\n{logged}");
    }
    assert!(
        !logged.contains(&a) && !logged.contains(&b),
        "no token is logged"
    );
}

#[test]
fn a_limits_429_names_a_windows_reset_moved_from_its_own() {
    assert_eq!(reset_value("@RESET@", 1_800_000_000), "1800000000");
    assert_eq!(reset_value("@RESET+120@", 1_800_000_000), "1800000120");
    assert_eq!(reset_value("@RESET-60@", 1_800_000_000), "1799999940");
    for kept in [
        "@RESET+@",
        "@RESET*2@",
        "@RESET+1x@",
        "1800000000",
        "@SEQ:0.1:0.1@",
    ] {
        assert_eq!(reset_value(kept, 1_800_000_000), kept, "{kept}");
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let dir = dir.path();
    let headers = dir.join("headers.json");
    let reset = crate::util::now_secs() + 600;
    let args = Args {
        log: Some(dir.join("stub.log")),
        headers_file: Some(headers.clone()),
        mode: format!("limit:five_hour:{reset}"),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    std::fs::write(
        &headers,
        r#"{"on429":{"anthropic-ratelimit-unified-5h-utilization":"1.0","anthropic-ratelimit-unified-5h-reset":"@RESET+120@"}}"#,
    )
    .unwrap();
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;
    let (st, head, _) = post_as(&port, "/v1/messages", None, turn);
    assert_eq!(st, 429, "at the limit");
    assert!(
        head.contains(&format!("anthropic-ratelimit-unified-reset: {reset}")),
        "the notice's reset: {head}"
    );
    assert!(
        head.contains(&format!(
            "anthropic-ratelimit-unified-5h-reset: {}",
            reset + 120
        )),
        "the window's reset, two minutes past the notice's: {head}"
    );
}

/// THE HOST UNREACHABLE (`drop`, v6 §14.17.5's stand-in): the stub closes each API request's
/// connection with no answer, and still answers its control paths; `ok` (the control) answers.
#[test]
fn a_dropped_mode_closes_each_api_request_with_no_answer() {
    let args = Args::default();
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    let client = aterm_http::Client::new(
        aterm_http::Trust::PlatformVerifier,
        aterm_http::ProxyMode::Direct,
        Duration::from_secs(60),
    );
    let claude = r#"{"model":"m","stream":false,"messages":[{"role":"user","content":"hi"}]}"#;
    let post = || {
        client
            .post(&format!("http://127.0.0.1:{port}/v1/messages"))
            .send(claude.as_bytes())
    };
    control(&port, "/__mode?m=drop").unwrap();
    assert!(post().is_err(), "no answer while the mode is drop");
    assert!(
        control(&port, "/__status").unwrap().contains("\"drop\""),
        "the control paths still answer"
    );
    control(&port, "/__mode?m=ok").unwrap();
    assert_eq!(post().unwrap().status(), 200, "the control: ok answers");
    assert_eq!(not_custom("drop"), Some("a built-in mode"));
}

/// A 429 that names no limit reset (the `rate-limit` mode's) carries no header made from a
/// `@RESET…@` template, never the template's words; the headers file's other on429 headers
/// stand.
#[test]
fn a_429_naming_no_reset_carries_no_reset_template() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let dir = dir.path();
    let headers = dir.join("headers.json");
    let args = Args {
        log: Some(dir.join("stub.log")),
        headers_file: Some(headers.clone()),
        mode: "rate-limit".to_string(),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    std::fs::write(
        &headers,
        r#"{"on429":{"anthropic-ratelimit-unified-5h-utilization":"1.0","anthropic-ratelimit-unified-5h-reset":"@RESET+120@"}}"#,
    )
    .unwrap();
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;
    let (st, head, _) = post_as(&port, "/v1/messages", None, turn);
    assert_eq!(st, 429, "rate-limited");
    assert!(
        !head.to_ascii_lowercase().contains("@reset"),
        "no template on the wire: {head}"
    );
    assert!(
        !head.contains("anthropic-ratelimit-unified-5h-reset:"),
        "no reset to set it against: {head}"
    );
    assert!(
        head.contains("anthropic-ratelimit-unified-5h-utilization: 1.0"),
        "the other headers stand: {head}"
    );
}

/// THE ANTHROPIC 429 NAMES ITS WAIT IN A HEADER ALONE (v6 §14.17.5.4): `rate-limit:<secs>` on the
/// Messages API answers the real API's body, which names no number, with the wait in its
/// `retry-after` header; the Responses API's body names it in its words (the control).
#[test]
fn an_anthropic_rate_limit_names_its_wait_in_its_header_alone() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let args = Args {
        log: Some(dir.path().join("stub.log")),
        mode: "rate-limit:150".to_string(),
        ..Args::default()
    };
    let (port, _) = start(args).unwrap();
    let turn = r#"{"model":"claude-x","stream":true,"max_tokens":32000,"tools":[{}],"messages":[{"role":"user","content":"hi"}]}"#;
    let (st, head, body) = post_as(&port.to_string(), "/v1/messages", None, turn);
    assert_eq!(st, 429, "rate-limited");
    assert!(head.contains("retry-after: 150"), "{head}");
    // The error's own words, not the whole body: its random request id can hold "150".
    let error = J::parse(&body).unwrap();
    let error = error.get("error").unwrap();
    let message = error.get("message").and_then(J::as_str).unwrap();
    assert_eq!(
        error.get("type").and_then(J::as_str),
        Some("rate_limit_error"),
        "{body}"
    );
    assert!(
        !message.contains("150") && !message.contains("try again"),
        "{body}"
    );
    let (st, body, _) = responses_error("rate-limit:150", "5").unwrap();
    assert_eq!(st, 429);
    assert!(
        body.py().contains("Please try again in 150s."),
        "{}",
        body.py()
    );
}

#[test]
fn a_failed_mode_write_keeps_the_active_mode_and_refuses_the_control_request() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mode_file = dir.path().join("mode");
    std::fs::create_dir(&mode_file).unwrap();
    let args = Args {
        mode_file: Some(mode_file.clone()),
        ..Args::default()
    };
    let stub = Stub::new(args.clone());
    assert!(stub.set_mode("overloaded").is_err());
    assert_eq!(stub.mode(), "ok");
    assert!(mode_file.is_dir());

    let (port, _) = start(args).unwrap();
    let port = port.to_string();
    assert!(
        control(&port, "/__mode?m=overloaded")
            .unwrap_err()
            .ends_with("answered 500")
    );
    assert_eq!(control(&port, "/__mode").unwrap(), r#"{"mode": "ok"}"#);
}

/// A token session's windows can be told from another credential's: the resets
/// [`chosen_resets`] names reach only the credential whose blocks they are, and another credential
/// answers with the shared headers.
/// NEGATIVE CONTROL: the same resets written as the shared `on200` reach every credential, so the
/// two answers no longer differ.
#[test]
fn chosen_resets_reach_only_their_credential() {
    let blocks = chosen_resets(("0.25", 1_800_007_200), ("0.40", 1_800_432_000));
    let spec = headers_file_for(
        r#"{"on200":{"anthropic-ratelimit-unified-5h-reset":"1"}}"#,
        "oat:lab-t",
        blocks.clone(),
    );
    let reset = |r: &RateHeaders, window: &str| {
        r.on200
            .iter()
            .find(|(k, _)| k == &format!("anthropic-ratelimit-unified-{window}-reset"))
            .map(|(_, v)| v.clone())
    };
    let token = rate_headers_for(&spec, "oat:lab-t", 0);
    assert_eq!(reset(&token, "5h").as_deref(), Some("1800007200"));
    assert_eq!(reset(&token, "7d").as_deref(), Some("1800432000"));
    let other = rate_headers_for(&spec, "oat:lab-a", 0);
    assert_eq!(reset(&other, "5h").as_deref(), Some("1"));
    assert_eq!(reset(&other, "7d"), None);

    let shared = blocks.get("on200").cloned().expect("on200");
    let spec = J::obj([("on200", shared)]);
    assert_eq!(
        rate_headers_for(&spec, "oat:lab-t", 0),
        rate_headers_for(&spec, "oat:lab-a", 0),
        "the control: shared resets reach both"
    );
}
