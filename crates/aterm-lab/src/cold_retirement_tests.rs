// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_a_durable_lapse_with_no_retained_authority_is_success() {
    let good = r#"{"phase":"failed","why":"native-lapsed","native_prepare":""}"#;
    assert!(lapsed(&J::parse(good).unwrap()));
    for bad in [
        r#"{"phase":"failed","why":"native-lapsed","native_prepare":"prepare"}"#,
        r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_id":"old"}"#,
        r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_retired_unissued":true}"#,
        r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_retired_unissued":null}"#,
        r#"{"phase":"failed","why":"native-lapsed","native_prepare":"","native_account_candidate":{}}"#,
        r#"{"phase":"failed","why":"something-else","native_prepare":""}"#,
        r#"{"phase":"relaunched","why":"native-lapsed","native_prepare":""}"#,
    ] {
        assert!(!lapsed(&J::parse(bad).unwrap()), "{bad}");
    }
}
