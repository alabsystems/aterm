// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn owned_input_timeout_input_completion_is_independent_of_pipe_closure() {
    let mut observed = OwnedInputTimeout::spawned(Some(6));
    observed.observe_stdin(Some(6), 3, true);
    assert_eq!(observed.stdin, OwnedInputState::Pending);
    assert_eq!(observed.stdin_bytes_sent, 3);
    observed.observe_stdin(Some(6), 3, false);
    assert_eq!(observed.stdin, OwnedInputState::ClosedEarly);
    observed.observe_stdin(Some(6), 6, true);
    assert_eq!(observed.stdin, OwnedInputState::Complete);
    observed.observe_stdin(Some(0), 0, false);
    assert_eq!(observed.stdin, OwnedInputState::Complete);
    observed.observe_stdin(None, 0, false);
    assert_eq!(observed.stdin, OwnedInputState::Unmanaged);
    assert!(!observed.natural_exit_observed);
    assert!(!observed.stdout_eof && !observed.stderr_eof);
}
