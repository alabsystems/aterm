// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn usage_errors_are_exit_two() {
    assert_eq!(run(&args(&["--ledger"])), exit::USAGE);
    assert_eq!(run(&args(&["--ledger", "--help"])), exit::USAGE);
    assert_eq!(run(&args(&["extra"])), exit::USAGE);
    assert_eq!(run(&args(&["--help"])), exit::PASS);
}

#[test]
fn no_ledger_could_not_run() {
    let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let missing = dir.path().join("recovery-ledger.log");
    let missing = missing.to_string_lossy().into_owned();
    assert_eq!(
        run(&args(&["--ledger", missing.as_str()])),
        exit::COULD_NOT_RUN
    );
}

/// A ledger of one launch with the keeper on and no kill is read and judged: not met.
#[test]
fn a_young_ledger_is_not_met() {
    let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
    let path = dir.path().join("recovery-ledger.log");
    std::fs::write(
        &path,
        "v=1 at=1790000000 pid=7 started=1789999999 build=1 version=0.1.0 launch=cold \
         prev=none prev_pid=? prev_build=? prev_version=? prev_uptime=? prev_stall=? \
         prev_pressure=? prev_trial=0 keeper=on\n",
    )
    .expect("write the ledger");
    assert_eq!(read_rows(&path).len(), 1, "the row parses");
    let path = path.to_string_lossy().into_owned();
    assert_eq!(run(&args(&["--ledger", path.as_str()])), exit::FAILED);
}
