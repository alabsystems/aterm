// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn explicit_stage_and_exact_installed_identity_are_required() {
    let args = [
        "--assets",
        "/private/cut",
        "--host-pid",
        "123",
        "--expect-build",
        "42",
        "--expect-commit",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ]
    .map(str::to_string)
    .to_vec();
    assert!(!parse(&args).unwrap().stage);
    let mut stage = args.clone();
    stage.push("--stage".into());
    assert!(parse(&stage).unwrap().stage);
    stage.push("--stage".into());
    assert!(parse(&stage).is_err());
    for remove in [0, 2, 4, 6] {
        let mut missing = args.clone();
        missing.drain(remove..remove + 2);
        assert!(parse(&missing).is_err());
    }
    let mut installed_stamp = args.clone();
    installed_stamp[7] = "d63937ff7a2c".into();
    assert_eq!(
        parse(&installed_stamp).unwrap().expected_commit,
        "d63937ff7a2c"
    );
    let mut short = args;
    short[7] = "abc".into();
    assert!(parse(&short).is_err());
}
