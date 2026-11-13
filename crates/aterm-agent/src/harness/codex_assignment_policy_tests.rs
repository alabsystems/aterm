// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_json::json;

fn document(claims: aterm_json::Value) -> aterm_json::Value {
    let payload = aterm_codec::base64::encode(&aterm_json::to_vec(&claims).unwrap())
        .unwrap()
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    json!({"tokens":{
        "id_token":format!("header.{payload}.signature"),
        "oauth_provenance":{"issuer":"https://auth.example/","client_id":"client"}
    }})
}
fn observe(document: &aterm_json::Value) -> Result<ProviderPolicy, String> {
    ProviderPolicy::from_document(&aterm_json::to_vec(document).unwrap())
}

#[test]
fn policy_binding_uses_native_endpoint_client_and_exact_length_encoding() {
    let conventional = observe(&document(json!({"iss":"https://unrelated.example"}))).unwrap();
    assert_eq!(
        conventional.binding(),
        "3af4df6e85b607c4d04337b7339f46469f0f81a72baf53241892d1d5637d21b4"
    );
    assert_eq!(conventional.endpoint(), "https://auth.example/oauth/token");
    assert!(!conventional.fedramp());
    let federal = observe(&document(json!({"https://api.openai.com/auth":{
        "chatgpt_account_is_fedramp":true
    }})))
    .unwrap();
    assert_eq!(conventional.binding(), federal.binding());
    assert!(federal.fedramp());
    assert_eq!(format!("{federal:?}"), "ProviderPolicy(redacted)");
}

#[test]
fn unknown_provenance_or_malformed_realm_never_gets_a_default_policy() {
    let original = document(json!({}));
    for key in ["oauth_provenance", "id_token"] {
        let mut changed = original.clone();
        changed
            .as_object_mut()
            .unwrap()
            .get_mut("tokens")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), aterm_json::Value::Null);
        assert!(observe(&changed).is_err());
    }
    for claims in [
        json!([]),
        json!({"https://api.openai.com/auth":null}),
        json!({"https://api.openai.com/auth":{"chatgpt_account_is_fedramp":"false"}}),
    ] {
        assert!(observe(&document(claims)).is_err());
    }
    let mut changed = original;
    changed
        .as_object_mut()
        .unwrap()
        .get_mut("tokens")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("oauth_provenance")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("issuer".into(), json!("http://auth.example"));
    assert!(observe(&changed).is_err());
}
