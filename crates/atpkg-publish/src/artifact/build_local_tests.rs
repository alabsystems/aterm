// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn parse_args_rejects_plus_prefixed_jobs() {
    let args = ["aterm", "/tmp/aterm", "--jobs", "+4"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    assert!(parse_args(&args).is_err());
}
