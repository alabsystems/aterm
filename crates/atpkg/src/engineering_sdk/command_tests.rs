// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
fn args() -> Vec<OsString> {
    [
        "--base-prefix",
        "/private/base",
        "--base-build",
        "1",
        "--targo",
        "/private/image/targo",
        "--targo-sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--expected-targo-source",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "--destination",
        "/private/fresh",
        "--build",
        "2",
        "--timeout-seconds",
        "60",
    ]
    .map(OsString::from)
    .into()
}
#[test]
fn every_input_is_explicit_and_unknown_duplicate_or_wide_deadline_refuses() {
    let original = args();
    assert!(parse(&original).is_ok());
    let mut bad = original.clone();
    bad.extend([OsString::from("--build"), OsString::from("3")]);
    assert!(parse(&bad).is_err());
    let mut bad = original.clone();
    bad[0] = "--prefix".into();
    assert!(parse(&bad).is_err());
    let mut bad = original.clone();
    bad.pop();
    assert!(parse(&bad).is_err());
    let mut bad = original.clone();
    *bad.last_mut().unwrap() = "3601".into();
    assert!(parse(&bad).is_err());
    let mut bad = original;
    *bad.last_mut().unwrap() = "0".into();
    assert!(parse(&bad).is_err());
}
