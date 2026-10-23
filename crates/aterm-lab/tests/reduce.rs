// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that `aterm-lab reduce` resolves relative input and `--out` paths against the caller's working directory.

#![cfg(target_os = "macos")]

use std::path::Path;

fn lab_in(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-lab"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run aterm-lab");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn reduce_reads_and_writes_the_callers_relative_paths() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    std::fs::write(
        dir.path().join("t.jsonl"),
        "{\"type\":\"user\",\"message\":{\"content\":\"hello there friend\"}}\n",
    )
    .expect("a transcript");
    let out = format!("reduced-{}.jsonl", std::process::id());
    let (code, err) = lab_in(
        dir.path(),
        &["reduce", "t.jsonl", "--program", "claude", "--out", &out],
    );
    assert_eq!(code, Some(0), "{err}");
    let text = std::fs::read_to_string(dir.path().join(&out)).expect("written beside the caller");
    assert_eq!(
        text.lines().nth(1),
        Some("{\"type\":\"user\",\"message\":{\"content\":\"\"}}"),
        "{text}"
    );
    assert!(!Path::new("/").join(&out).exists(), "nothing made at /");
}
