// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every ONE-BINARY route to the retired `hook` verb answers as `aterm-link`'s
//! `retired_hook` does (b58cde423): exit 0, nothing on stdout, exactly its one
//! stderr line (`aterm_link::cli::RETIRED_HOOK_LINE`). The crate's own
//! `tests/r25_retired_hook.rs` drives the dev `aterm-link` bin; the shipped
//! product is this binary, reached three ways a settings file may still hold:
//!
//! * `<aterm> hook run <event> …` — what `aterm link hook install` wrote on
//!   2026-09-14, before 7bcb0503a spelled it `link hook run`. Without the route
//!   in `src/main.rs` it fell to the window parser's `unknown option`, exit 2,
//!   which Claude Code reads as a BLOCK.
//! * `<aterm> link hook run <event> …` — the 0.91.0 shape, through the verb.
//! * `…/aterm-link hook run <event> …` — the argv0 alias symlink.

#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn run(exe: &Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(exe)
        .args(args)
        .env_remove("ATERM_HEADLESS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aterm");
    // Ignored on purpose: the negative control's refusal exits without reading.
    let _ = child
        .stdin
        .take()
        .expect("stdin")
        .write_all(br#"{"session_id":"x","hook_event_name":"Stop"}"#);
    child.wait_with_output().expect("wait")
}

fn assert_retired(exe: &Path, form: &[&str]) {
    let out = run(exe, form);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{form:?}: {stderr}");
    assert!(out.stdout.is_empty(), "{form:?}: {:?}", out.stdout);
    assert_eq!(
        stderr,
        format!("{}\n", aterm_link::cli::RETIRED_HOOK_LINE),
        "{form:?}: the one line naming the removal"
    );
}

#[test]
fn every_one_binary_route_to_the_retired_hook_is_a_silent_success() {
    let aterm = Path::new(env!("CARGO_BIN_EXE_aterm"));
    for form in [
        &["hook", "run", "stop", "--state", "/nonexistent/aterm-link"][..],
        &["hook", "run", "user-prompt-submit"][..],
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
        &["link", "hook", "run", "session-start"][..],
    ] {
        assert_retired(aterm, form);
    }

    // The argv0 alias: a symlink NAMED `aterm-link` onto this binary IS that tool.
    let dir = std::env::temp_dir().join(format!("aterm-retired-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let alias = dir.join("aterm-link");
    std::os::unix::fs::symlink(aterm, &alias).expect("argv0 alias");
    assert_retired(
        &alias,
        &["hook", "run", "stop", "--state", "/nonexistent/x"],
    );
    assert_retired(
        &alias,
        &["hook", "run", "permission-request", "--gate-tools"],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Negative control: only `hook run` is routed at the front door. `hook`
/// followed by anything else is still the mode fork's refusal (exit 2), so the
/// pass above is the route and not a front door that went quiet.
#[test]
fn a_bare_hook_word_is_still_refused() {
    let out = run(
        Path::new(env!("CARGO_BIN_EXE_aterm")),
        &["hook", "walk", "stop"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
}
