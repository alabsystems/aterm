// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The contract `targo --unverified verify libc` reads, on the cases that need
//! no toolchain: a root with no oracle could not run (exit 3, and a
//! `COULD NOT RUN: <reason>` line the ladder quotes), a stray flag is usage (2).
//! What a real run finds is decided by the unit tests and, on every `verify
//! libc` run, by the planted-defect controls.

use std::ffi::OsStr;
use std::time::Instant;

use aterm_spec::wait::MUST_HAPPEN;

/// Run the gate with `args`; its exit code and stdout. The wait is a hang
/// detector: a wedged gate is killed (`SIGKILL`) and named.
fn gate(args: &[&OsStr]) -> (Option<i32>, String) {
    let mut command = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-libc-oracle"));
    command.args(args);
    let out = aterm_uds::deadline::output_until(&mut command, Instant::now() + MUST_HAPPEN)
        .unwrap_or_else(|e| panic!("the gate {e}"));
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    (out.status.code(), stdout)
}

#[test]
fn no_oracle_could_not_run_and_a_stray_flag_is_usage() {
    let empty = aterm_tempfile::tempdir().expect("tempdir");
    let (code, stdout) = gate(&["--root".as_ref(), empty.path().as_os_str()]);
    assert_eq!(code, Some(3), "{stdout}");
    assert!(
        stdout.lines().any(|l| l
            .trim()
            .strip_prefix("COULD NOT RUN: ")
            .is_some_and(|why| !why.trim().is_empty())),
        "verify quotes the `COULD NOT RUN: ` line:\n{stdout}"
    );
    let (code, _) = gate(&["--fast".as_ref()]);
    assert_eq!(code, Some(2));
}
