// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

const CHILD_ENV: &str = "ATERM_UDS_TEST_KILL_SELF_CHILD";

#[test]
fn kill_self_now_dies_by_sigkill() {
    if std::env::var_os(CHILD_ENV).is_some() {
        super::kill_self_now();
    }
    let status = crate::deadline::std_ends_within(
        Command::new(std::env::current_exe().expect("the test binary"))
            .arg("process::kill_self_tests::kill_self_now_dies_by_sigkill")
            .arg("--exact")
            .arg("--test-threads=1")
            .env(CHILD_ENV, "1"),
        crate::deadline::ECHO_HANG,
    )
    .status;
    assert_eq!(status.signal(), Some(9), "the child's end: {status}");
}
