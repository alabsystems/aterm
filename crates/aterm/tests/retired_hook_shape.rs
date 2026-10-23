// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that every binary route to the retired `hook` verb exits 0 with empty stdout and `RETIRED_HOOK_LINE` on stderr.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

const STOP_INPUT: &[u8] = br#"{"session_id":"x","hook_event_name":"Stop"}"#;

fn run(exe: &Path, args: &[&str]) -> std::process::Output {
    let mut child = isolated(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aterm");
    let _ = child.stdin.take().expect("stdin").write_all(STOP_INPUT);
    child.wait_with_output().expect("wait")
}

fn isolated(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    let root = aterm_uds::test_binary::scratch_root().expect("this test binary's own root");
    crate::launch_isolation::apply(&mut cmd, &root.join("hook-shape"));
    cmd
}

fn run_with(exe: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut child = isolated(exe)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aterm");
    if let Some(bytes) = stdin {
        let mut pipe = child.stdin.take().expect("stdin");
        pipe.write_all(bytes).expect("write the hook input");
    }
    child.wait_with_output().expect("wait")
}

fn assert_retired_output(out: &std::process::Output, form: &[&str]) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{form:?}: exit 2 is Claude Code's blocking error — {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "{form:?}: stdout reaches the model's context: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        stderr,
        format!("{}\n", aterm_link::cli::RETIRED_HOOK_LINE),
        "{form:?}: the one line naming the removal"
    );
}

fn assert_retired(exe: &Path, form: &[&str]) {
    assert_retired_output(&run(exe, form), form);
}

#[test]
fn every_one_binary_route_to_the_retired_hook_is_a_silent_success() {
    let aterm = crate::launch_isolation::aterm_executable();
    let aterm = aterm.as_path();
    for form in [
        &["hook", "run", "stop", "--state", "/nonexistent/aterm-link"][..],
        &["hook", "run"][..],
        &[
            "link",
            "hook",
            "run",
            "stop",
            "--state",
            "/nonexistent/aterm-link",
            "--wake-budget",
            "6/1",
            "--timeout",
            "15",
        ][..],
    ] {
        assert_retired(aterm, form);
    }
}

#[test]
fn every_retired_hook_shape_through_the_alias_is_a_silent_success() {
    let dir =
        std::env::temp_dir().join(format!("aterm-retired-hook-shapes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let alias = dir.join("aterm-link");
    std::os::unix::fs::symlink(crate::launch_isolation::aterm_executable(), &alias)
        .expect("argv0 alias");
    let state = dir.join("state");
    let state = state.to_string_lossy().into_owned();
    let input = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    let big = vec![b' '; 3 * 1024 * 1024];
    let shapes: [&[&str]; 4] = [
        &["hook", "run", "user-prompt-submit", "--state", &state],
        &["hook", "install", "claude"],
        &["hook"],
        &[
            "hook",
            "run",
            "stop",
            "--state",
            &state,
            "--wake-budget",
            "6/1",
            "--timeout",
            "15",
        ],
    ];
    for form in shapes {
        for stdin in [Some(&input[..]), Some(&big[..]), None] {
            assert_retired_output(&run_with(&alias, form, stdin), form);
        }
    }
    assert!(!Path::new(&state).exists(), "the no-op writes no state");
    for word in [&["hoook"][..], &["hooks", "run", "stop"][..]] {
        let out = run_with(&alias, word, None);
        assert_eq!(out.status.code(), Some(2), "{word:?}");
        let refused = format!("unknown subcommand `{}`", word[0]);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&refused),
            "{word:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_hook_word_is_still_refused() {
    let out = run(
        &crate::launch_isolation::aterm_executable(),
        &["hook", "walk", "stop"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}
