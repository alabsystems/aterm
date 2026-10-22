// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Re-running ONE test of this binary in a child process, for the tests whose
//! subject is a process death (an abort with a named message, a signal) or a
//! state the parent must not take on (a closed stderr, no autorelease pool).
//!
//! The child is this same binary, so libtest dispatches it by the test's full
//! path, `<module>::<test>`: the caller passes its `module_path!()` and the
//! test's name, and the child runs that test alone with `env` set, which is
//! how the test knows to run its fatal body instead of re-spawning
//! ([`is_child`]).
//!
//! A child's abort ends it with exit status [`ABORTED`], never a death by
//! `SIGABRT`: the abort is the subject, and dying of the signal would have
//! macOS write a crash report for every run of the suite.

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

/// Whether this process is the child [`rerun`] started with `var` set; if so,
/// an `abort(3)` from here on ends it with [`ABORTED`]. Only `SIGABRT` reaches
/// the handler, and `_exit` runs nothing else of the process, so the parent
/// reads exactly what the abort left: the stderr written before it.
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

/// Run `test`, a test of the module whose `module_path!()` is `module`, alone
/// in a child (`--exact --nocapture`) with `env` set, and wait for it.
pub fn rerun(module: &str, test: &str, env: &[(&str, &str)]) -> Output {
    // `module_path!()` leads with the crate (`objc_process_suite::pools`);
    // libtest names the test from the module below it.
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
