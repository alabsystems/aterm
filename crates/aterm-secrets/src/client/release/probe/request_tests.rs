// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args() -> Vec<String> {
    [
        "run",
        "--nonce",
        "0123456789abcdef0123456789abcdef",
        "--evidence-dir",
        "/private/tmp/.ubd/aterm-release-probe-0123456789abcdef0123456789abcdef.noindex",
        "--repo",
        "fixture-owner/aterm",
        "--json",
    ]
    .map(str::to_owned)
    .into()
}

#[test]
fn only_explicit_owned_transactions_parse() {
    assert!(Request::parse(&args()).is_ok());
    for (at, value) in [
        (2, "--token"),
        (4, "/tmp/unrelated"),
        (6, "fixture-owner/other"),
        (6, "https://github.com/fixture-owner/aterm"),
    ] {
        let mut args = args();
        args[at] = value.into();
        assert!(Request::parse(&args).is_err());
    }
    let mut args = args();
    args.extend(["--binary".into(), "/bin/echo".into()]);
    assert!(Request::parse(&args).is_err());
}

#[test]
fn resume_cannot_change_repository_set() {
    let mut args = args();
    args[0] = "resume".into();
    assert!(Request::parse(&args).is_err());
    args.drain(5..7);
    assert!(Request::parse(&args).is_ok());
}
