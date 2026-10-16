// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use atpkg_publish::cli::{self, VERBS};
use atpkg_publish::error::Error;
use atpkg_publish::host::fake::FakeHost;
use atpkg_publish::testkit::Keys;

use super::scratch;

#[test]
fn every_listed_verb_is_routed_and_an_unknown_one_is_a_usage_error() {
    let tmp = scratch("cli");
    let ctx = Keys::mint("m3", &[]).ctx(tmp.path(), Arc::new(FakeHost::new()));
    // An empty fake host serves no channel, so a verb that needs none of its arguments
    // (`index`, `status`) answers could-not-run; none is an unknown verb.
    for v in VERBS {
        let err = cli::dispatch(&ctx, v.name, &[]).expect_err("a usage error or no channel");
        assert!(
            matches!(err, Error::Usage(_) | Error::CannotRun(_))
                && !err.to_string().contains("unknown verb"),
            "{}: {err}",
            v.name
        );
    }
    let err = cli::dispatch(&ctx, "publish", &[]).expect_err("unknown");
    assert_eq!(err.exit_code(), 2);
    assert_eq!(Error::cannot_run("x").exit_code(), 3);
}
