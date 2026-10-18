// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A test that must own its process, inside the shared suite binary: it re-runs
//! the binary on itself alone (`--exact <module>::<test>`), so its process-wide
//! readings (the peak resident set, the font-coverage counters) see no sibling,
//! and answers as the child did. The binary is shared; the process is not.

#![allow(
    clippy::disallowed_types,
    reason = "test code: the child is this test binary re-run on one test"
)]

use std::process::Command;

/// Set in the child: run the body here.
const ALONE: &str = "ATERM_RENDER_TEST_ALONE";

/// `true` in the child, which runs the test's body. In the parent, run
/// `test` (of the module whose `module_path!()` is `module`) alone in a child
/// and answer as it did — its failure is this test's failure, its COULD NOT
/// RUN this test's — then `false`: the parent's part is done.
pub fn in_own_process(module: &str, test: &str) -> bool {
    if std::env::var_os(ALONE).is_some() {
        return true;
    }
    // `module_path!()` leads with the crate; libtest names the test from the
    // module below it.
    let path = match module.split_once("::") {
        Some((_, inner)) => format!("{inner}::{test}"),
        None => test.to_owned(),
    };
    let exe = std::env::current_exe().expect("the test binary's own path");
    let out = Command::new(exe)
        .args(["--exact", "--nocapture", "--test-threads=1", &path])
        .env(ALONE, "1")
        .output()
        .expect("re-running this test binary on one test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        assert!(
            stdout.contains("1 passed"),
            "the child ran no test named {path}\nstdout: {stdout}\nstderr: {stderr}"
        );
        return false;
    }
    if stderr.contains(aterm_spec::host::COULD_NOT_RUN)
        || stdout.contains(aterm_spec::host::COULD_NOT_RUN)
    {
        aterm_spec::host::could_not_run(format!("{path}, alone in its own process:\n{stderr}"));
    }
    panic!("{path} failed alone in its own process\nstdout: {stdout}\nstderr: {stderr}");
}
