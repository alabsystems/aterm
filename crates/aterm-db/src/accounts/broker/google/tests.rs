// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[test]
fn google_text_constructs_one_closed_route_and_body() {
    let prepared = prepare(
        ADAPTER,
        br#"{"model":"gemini-3.8-flash","prompt":"a \"quote\"\nline","max_output_tokens":64}"#,
        until(),
    )
    .unwrap();
    assert_eq!(
        prepared.path,
        "/v1beta/models/gemini-3.8-flash:generateContent"
    );
    let body: aterm_json::Value = aterm_json::from_slice(prepared.body.expose()).unwrap();
    assert_eq!(
        body,
        aterm_json::json!({
            "contents":[{"role":"user","parts":[{"text":"a \"quote\"\nline"}]}],
            "generationConfig":{"candidateCount":1,"maxOutputTokens":64,"responseModalities":["TEXT"]}
        })
    );
    assert!(prepare("another-adapter", b"{}", until()).is_err());
    assert!(prepare(ADAPTER, b"{}", Instant::now()).is_err());
}

#[test]
fn google_text_refuses_unknown_duplicate_and_unbounded_requests() {
    for bytes in [
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":1,"tools":[]}"#.as_slice(),
        br#"{"model":"gemini-x","model":"gemini-y","prompt":"p","max_output_tokens":1}"#,
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":1,"url":"https://elsewhere"}"#,
        br#"{"model":"gemini-x/../../other","prompt":"p","max_output_tokens":1}"#,
        br#"{"model":"gemini-x%2fother","prompt":"p","max_output_tokens":1}"#,
        br#"{"model":"gemini-x","prompt":"","max_output_tokens":1}"#,
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":0}"#,
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":8193}"#,
        br#"{"model":"gemini-x","prompt":"p","max_output_tokens":1.0}"#,
        br#"{"model":"gemini-x","prompt":["p"],"max_output_tokens":1}"#,
    ] {
        assert!(prepare(ADAPTER, bytes, until()).is_err());
    }
    let huge = aterm_json::to_vec(&aterm_json::json!({"model":"gemini-x","prompt":"x".repeat(PROMPT_MAX+1),"max_output_tokens":1})).unwrap();
    assert!(prepare(ADAPTER, &huge, until()).is_err());
    assert!(prepare(ADAPTER, &vec![b' '; INPUT_MAX + 1], until()).is_err());
}

#[test]
fn google_response_is_a_bounded_private_http_observation() {
    assert_eq!(
        response(&[0, 200, b'{', b'}']).unwrap(),
        (200, b"{}".as_slice())
    );
    assert!(response(&[]).is_err());
    assert!(response(&[0, 0]).is_err());
}
