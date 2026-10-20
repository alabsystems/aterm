// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-lab reduce` reads the caller's paths as the caller gave them: the
//! lab moves itself to `/` before it dispatches a verb (`sys::confine_env`),
//! so a relative `<file>` or `--out FILE` is read against the directory the
//! lab was run from (`paths::from_caller`), never against `/`.

#![cfg(target_os = "macos")]

use std::path::Path;

/// The real binary, run from `dir` with `args`: its exit code and stderr.
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

/// A relative transcript and a relative `--out` are the caller's: the
/// reduction is read from and written beside the caller's directory, and
/// nothing is made at `/`.
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
