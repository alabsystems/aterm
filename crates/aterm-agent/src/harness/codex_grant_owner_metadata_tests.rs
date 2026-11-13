// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn grant_reference_codec_is_portable_strict_and_redacted() {
    let value = aterm_json::json!({
        "operation": "a".repeat(32), "capability": "b".repeat(64),
        "issuance_digest": "c".repeat(64), "producer_mac": "d".repeat(64)
    });
    let reference: GrantReference = aterm_json::from_value(value.clone()).unwrap();
    assert!(reference.valid());
    assert_eq!(aterm_json::to_value(&reference).unwrap(), value);
    assert!(!format!("{reference:?}").contains(&"b".repeat(64)));
    let mut unknown = value;
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unknown".into(), true.into());
    assert!(aterm_json::from_value::<GrantReference>(unknown).is_err());
}
