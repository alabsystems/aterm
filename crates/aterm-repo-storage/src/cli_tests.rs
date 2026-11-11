// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(text: &[&str]) -> Vec<OsString> {
    text.iter().map(OsString::from).collect()
}

#[test]
fn exact_cli_has_no_implicit_root_or_unbounded_verification() {
    let (_, limits, json) =
        parse(&args(&["plan", "--git-common-dir", "/repo/.git", "--json"])).unwrap();
    assert_eq!(limits.verify_io_bytes, 0);
    assert!(json);
    for bad in [
        vec!["plan"],
        vec!["plan", "--git-common-dir"],
        vec!["plan", "--git-common-dir", "/x", "--verify-io-bytes", "-1"],
        vec!["plan", "--git-common-dir", "/x", "--json", "--json"],
        vec!["apply", "--git-common-dir", "/x"],
    ] {
        assert!(parse(&args(&bad)).is_err(), "{bad:?}");
    }
}
