// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::records_shell_start;

#[test]
fn only_a_fresh_interactive_shell_records_a_start() {
    let false_cmd = vec![std::ffi::OsString::from("false")];
    assert!(records_shell_start(false, None), "a fresh shell");
    assert!(
        records_shell_start(false, Some(&[])),
        "an empty `-e` runs the shell, as at the spawn"
    );
    assert!(!records_shell_start(false, Some(&false_cmd)), "`-e false`");
    assert!(!records_shell_start(true, None), "an adopted shell");
    assert!(!records_shell_start(true, Some(&false_cmd)));
}
