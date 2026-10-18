// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Re-running ONE test of this binary in a child process, for the tests whose
//! subject is a process death (an abort with a named message, a signal) or a
//! state the parent must not take on (a closed stderr, no autorelease pool).
//!
//! The child is this same binary, so libtest dispatches it by the test's full
//! path, `<module>::<test>`: the caller passes its `module_path!()` and the
//! test's name, and the child runs that test alone with `env` set, which is
//! how the test knows to run its fatal body instead of re-spawning.

#![allow(
    clippy::disallowed_types,
    reason = "test code: the child is this test binary re-run on one test"
)]

use std::process::{Command, Output};

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
