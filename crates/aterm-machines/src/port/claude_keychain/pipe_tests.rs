// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

fn command(script: &str) -> aterm_uds::spawnfd::Command {
    let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let mut command = aterm_uds::spawnfd::command(&shell);
    command.args(["-c", script]);
    command
}

#[test]
fn capture_distinguishes_present_absent_denied_and_partial_errors() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let present = capture(&mut command("printf fixture"), || deadline, || {})
        .unwrap()
        .unwrap();
    assert_eq!(present.as_slice(), b"fixture");
    assert!(
        capture(&mut command("exit 10"), || deadline, || {})
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        capture(&mut command("printf partial; exit 10"), || deadline, || {}),
        Err(ReadError::Unavailable)
    ));
    assert!(matches!(
        capture(&mut command("printf private; exit 12"), || deadline, || {}),
        Err(ReadError::Interaction)
    ));
}

#[test]
fn capture_caps_output_and_kills_a_started_child_at_the_deadline() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let script = "i=0; while [ \"$i\" -lt 1025 ]; do printf '%064d' 0; i=$((i+1)); done";
    assert!(matches!(
        capture(&mut command(script), || deadline, || {}),
        Err(ReadError::TooLarge)
    ));

    let temp = aterm_tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("child.pid");
    let mut child = command("printf '%s' \"$$\" > \"$1\"; printf started; while :; do :; done");
    child.arg("fixture").arg(&pid_file);
    let until = Cell::new(Instant::now() + Duration::from_secs(60));
    let result = capture(&mut child, || until.get(), || until.set(Instant::now()));
    assert!(matches!(result, Err(ReadError::TimedOut)));
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if matches!(
            aterm_uds::process::process_presence(pid),
            aterm_uds::process::ProcessPresence::Exited
                | aterm_uds::process::ProcessPresence::Absent
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned timed-out child is still alive"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
