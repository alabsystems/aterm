// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn release_responses_above_one_mib_are_decoded_by_both_paths() {
    let value = aterm_json::json!({"body":"x".repeat(1024 * 1024 + 1)});
    let bytes = aterm_json::to_vec(&value).unwrap();
    for empty_is_null in [true, false] {
        let answer = decode_response(200, &bytes, empty_is_null).unwrap();
        assert_eq!(answer.status, 200);
        assert_eq!(answer.body, value);
    }
}

#[test]
fn response_cap_is_inclusive_and_empty_body_semantics_are_preserved() {
    let mut bytes = vec![b' '; JSON_CAP];
    bytes[..2].copy_from_slice(b"[]");
    for empty_is_null in [true, false] {
        assert_eq!(
            decode_response(200, &bytes, empty_is_null).unwrap().body,
            aterm_json::json!([])
        );
        bytes.push(b' ');
        assert!(matches!(
            decode_response(200, &bytes, empty_is_null),
            Err(ApiError::Uncertain)
        ));
        bytes.pop();
    }
    assert_eq!(decode_response(204, b"", true).unwrap().body, Value::Null);
    assert!(matches!(
        decode_response(201, b"", false),
        Err(ApiError::Uncertain)
    ));
    assert!(matches!(
        decode_response(200, b"not-json", true),
        Err(ApiError::Uncertain)
    ));
}

#[test]
fn revoked_before_spawn_stays_typed_not_started() {
    let token = crate::SecretBytes::from(&b"synthetic-test-token"[..]);
    let client = Http::new(
        &token,
        Instant::now() + std::time::Duration::from_secs(60),
        Arc::new(|| Err(Error::new("fixture revoked"))),
    )
    .unwrap();
    assert!(matches!(
        client.request(
            Method::Post,
            "/repos/fixture/aterm/releases",
            Some(&aterm_json::json!({"draft":true}))
        ),
        Err(ApiError::NotStarted)
    ));
}
