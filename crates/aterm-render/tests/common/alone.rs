// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Subprocess runner for tests that require an isolated process.

#![allow(
    clippy::disallowed_types,
    reason = "test code: the child is this test binary re-run on one test"
)]

use std::process::Command;

/// Set in the child.
const ALONE: &str = "ATERM_RENDER_TEST_ALONE";

/// `true` in the child, which runs the test's body.
pub fn in_own_process(module: &str, test: &str) -> bool {
    if std::env::var_os(ALONE).is_some() {
        return true;
    }
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
    let child_why = |text: &str| {
        text.lines().find_map(|line| {
            let (_, rest) = line.split_once(aterm_spec::host::COULD_NOT_RUN)?;
            Some(
                rest.trim_start_matches(|c: char| c == ':' || c == '—' || c.is_whitespace())
                    .trim_end()
                    .to_owned(),
            )
        })
    };
    if let Some(why) = child_why(&stderr).or_else(|| child_why(&stdout)) {
        aterm_spec::host::could_not_run(format!(
            "{why}\n({path}, alone in its own process)\n{stderr}"
        ));
    }
    panic!("{path} failed alone in its own process\nstdout: {stdout}\nstderr: {stderr}");
}
