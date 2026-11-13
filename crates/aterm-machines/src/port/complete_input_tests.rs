// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, System};
use std::time::Duration;

#[test]
fn complete_input_streams_beyond_pipe_capacity_without_a_file_or_extra_newline() {
    let input = vec![b'q'; 256 * 1024];
    let out = Live::new()
        .run_complete_stdout_input(&Cmd::new("/bin/cat"), &input)
        .unwrap();
    assert!(out.success());
    assert_eq!(out.stdout, input);
    let max_budget = Live::new()
        .run_complete_stdout_input(&Cmd::new("/bin/cat").budget(Duration::MAX), b"ok")
        .unwrap();
    assert_eq!(max_budget.stdout, b"ok");
}

#[test]
fn complete_input_refuses_output_overflow_and_an_unread_request_at_its_deadline() {
    let live = Live::new();
    let input = vec![b'x'; aterm_uds::deadline::OUTPUT_KEEP + 1];
    let error = live
        .run_complete_stdout_input(&Cmd::new("/bin/cat"), &input)
        .unwrap_err();
    assert!(matches!(error, RunFailed::AfterStart(_)), "{error:?}");
    assert!(error.to_string().contains("complete stdout exceeds"));
    let error = live
        .run_complete_stdout_input(
            &Cmd::new("/bin/sleep").arg("60").budget(Duration::ZERO),
            &input,
        )
        .unwrap_err();
    assert!(matches!(error, RunFailed::TimedOut(_)));
}

#[test]
fn complete_input_is_absent_from_fake_calls_fed_records_and_failure_diagnostics() {
    let sys = Fake::root();
    let input = b"private-input-must-never-be-recorded";
    let directory = aterm_tempfile::tempdir().unwrap();
    let cmd = Cmd::new(directory.path().join("absent-input"));
    sys.on(&cmd.display(), Ran::ok("done"));
    assert_eq!(
        sys.run_complete_stdout_input(&cmd, input).unwrap().stdout,
        b"done"
    );
    assert_eq!(sys.calls(), [cmd.display()]);
    assert!(sys.fed().is_empty());
    let error = Live::new()
        .run_complete_stdout_input(&cmd, input)
        .unwrap_err();
    assert!(
        !error
            .to_string()
            .contains(std::str::from_utf8(input).unwrap())
    );
    let file_stdin = cmd.stdin_file(directory.path().join("unopened-input-file"));
    let before = sys.calls();
    assert!(sys.run_complete_stdout_input(&file_stdin, input).is_err());
    assert_eq!(sys.calls(), before);
}
