// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Only Google's own token endpoint is admitted for a key. NEGATIVE CONTROL: the exact
/// endpoint is.
#[test]
fn only_googles_token_endpoint_is_admitted() {
    let google = Origins::google();
    assert_eq!(google.token, "https://oauth2.googleapis.com/token");
    assert!(google.admit("https://oauth2.googleapis.com/token").is_ok());
    for off in [
        "https://oauth2.googleapis.com.attacker.example/token",
        "https://oauth2.googleapis.com/token/",
        "https://oauth2.googleapis.com/token?x=1",
        "http://oauth2.googleapis.com/token",
        "https://accounts.google.com/o/oauth2/token",
        "https://attacker.example/token",
        "",
    ] {
        let why = google.admit(off).unwrap_err();
        assert!(why.contains("token_uri"), "{why}");
    }
}

/// A URL is on an origin only when the origin's own path follows. NEGATIVE CONTROL:
/// one that is passes.
#[test]
fn a_url_is_on_an_origin_only_by_its_path() {
    let api = "https://firebasehosting.googleapis.com";
    assert!(Origins::on(
        api,
        "https://firebasehosting.googleapis.com/v1beta1/x"
    ));
    for off in [
        "https://firebasehosting.googleapis.com.attacker.example/v1beta1/x",
        "https://firebasehosting.googleapis.com@attacker.example/x",
        "https://firebasehosting.googleapis.com",
        "https://firebasehosting.googleapis.com/a b",
        "https://firebasehosting.googleapis.com/a\\b",
    ] {
        assert!(!Origins::on(api, off), "{off}");
    }
}

/// An error body is said in the API's own words. NEGATIVE CONTROL: a 2xx is JSON.
#[test]
fn an_answer_says_the_apis_own_message() {
    let hosting = Answer {
        status: 403,
        body: br#"{"error":{"code":403,"message":"The caller does not have permission","status":"PERMISSION_DENIED"}}"#.to_vec(),
    };
    assert_eq!(
        hosting.said(),
        "HTTP 403: The caller does not have permission"
    );
    assert!(hosting.json().is_err());
    let token = Answer {
        status: 400,
        body: br#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#.to_vec(),
    };
    assert_eq!(
        token.said(),
        "HTTP 400: invalid_grant: Invalid JWT Signature."
    );
    let ok = Answer {
        status: 200,
        body: b"{\"a\":1}".to_vec(),
    };
    assert_eq!(ok.json().unwrap(), aterm_json::json!({"a": 1}));
}
