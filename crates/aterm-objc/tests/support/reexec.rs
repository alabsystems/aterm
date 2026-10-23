// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Child-process test runner for tests that verify aborts or process-global state.

#![allow(
    clippy::disallowed_types,
    reason = "test code: the child is this test binary re-run on one test"
)]

use std::process::{Command, Output};

/// What an aborted child exits with: `128 + SIGABRT`, as a shell reports one.
pub const ABORTED: i32 = 128 + SIGABRT;

const SIGABRT: i32 = 6;

unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    fn _exit(status: i32) -> !;
}

/// Return `true` when running inside the child spawned with `var` set.
pub fn is_child(var: &str) -> bool {
    if std::env::var_os(var).is_none() {
        return false;
    }
    // SAFETY: `signal` takes an integer and a handler that is async-signal-safe
    // (it calls `_exit` alone).
    unsafe { signal(SIGABRT, exit_aborted) };
    true
}

extern "C" fn exit_aborted(_: i32) {
    // SAFETY: `_exit` is async-signal-safe and does not return.
    unsafe { _exit(ABORTED) }
}

/// Re-run `test` in `module` inside a child process with `env` set and return its output.
pub fn rerun(module: &str, test: &str, env: &[(&str, &str)]) -> Output {
    let path = match module.split_once("::") {
        Some((_, inner)) => format!("{inner}::{test}"),
        None => test.to_owned(),
    };
    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", "--nocapture", &path]);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .expect("re-running this test binary as a child")
}
