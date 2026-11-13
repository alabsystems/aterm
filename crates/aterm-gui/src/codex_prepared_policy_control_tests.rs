// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn policy_request_is_closed_and_has_no_caller_supplied_policy() {
    let operation_id = "a".repeat(32);
    let valid = format!("assignment-target schema=1 operation={operation_id}");
    assert_eq!(operation(&valid), Some(operation_id.as_str()));
    for extra in [
        "schema=1",
        "member=none",
        "target_provider=unknown",
        "capability=other",
        "source_fedramp=false",
    ] {
        assert!(operation(&format!("{valid} {extra}")).is_none());
    }
    assert!(operation(&valid.replace("schema=1", "schema=2")).is_none());
    assert!(operation(&valid.replace(&operation_id, &"A".repeat(32))).is_none());
    assert!(operation(&format!("{valid}{}", " ".repeat(MAX_FRAME))).is_none());
}
