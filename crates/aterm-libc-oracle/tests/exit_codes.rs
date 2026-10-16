// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The binary's exit-code contract on the cases that need no toolchain: a root
//! with no oracle could not run (3, with its fix), a stray flag is usage (2).
//! What a real run finds is decided by the unit tests and, on every `verify
//! libc` run, by the planted-defect controls.

use std::ffi::OsStr;
use std::time::Instant;

use aterm_json::Value;
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
    let (code, stdout) = gate(&[
        "--root".as_ref(),
        empty.path().as_os_str(),
        "--json".as_ref(),
    ]);
    assert_eq!(code, Some(3));
    let report: Value = aterm_json::from_str(&stdout).expect("one JSON report");
    assert_eq!(report["verdict"].as_str(), Some("could_not_run"));
    assert!(report["could_not_run"]["fix"].as_str().is_some());
    assert_eq!(report["phases"].as_array().map(Vec::len), Some(0));
    let (code, _) = gate(&["--fast".as_ref()]);
    assert_eq!(code, Some(2));
}
