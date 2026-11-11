// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;

#[test]
fn original_git_digest_and_api_identity_are_exact_not_normalized_equivalents() {
    assert_eq!(
        aterm_codec::hex::encode_lower(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        aterm_codec::hex::encode_lower(&sha1(b"")),
        "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    let raw = format!(
        "tree {}\nauthor A <a@b> 1700000000 -0800\ncommitter C <c@d> 1700000000 +0000\n\noriginal\n",
        "1".repeat(40)
    );
    let id = oid(raw.as_bytes());
    let mut value = aterm_json::json!({"sha":id,"tree":{"sha":"1".repeat(40)},"parents":[],
        "author":{"name":"A","email":"a@b","date":"2023-11-14T22:13:20Z"},
        "committer":{"name":"C","email":"c@d","date":"2023-11-14T22:13:20Z"},"message":"original"});
    assert_eq!(from_api(&id, &value).unwrap(), raw.as_bytes());
    value
        .as_object_mut()
        .unwrap()
        .insert("message".into(), Value::String("mutated".into()));
    assert!(from_api(&id, &value).is_err());
    value
        .as_object_mut()
        .unwrap()
        .get_mut("author")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("date".into(), Value::String("ééééééééééééx".into()));
    assert!(from_api(&id, &value).is_err());
}
