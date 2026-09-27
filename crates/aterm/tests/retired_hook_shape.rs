// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every ONE-BINARY route to the retired `hook` verb answers as `aterm-link`'s
//! `retired_hook` does (b58cde423): exit 0, nothing on stdout, exactly its one
//! stderr line (`aterm_link::cli::RETIRED_HOOK_LINE`). Round 25 cut the vendor
//! hooks, but a hook entry can survive the removal — a project's
//! `.claude/settings.local.json` (the agents pass never opens one), a host the
//! pass has not reached, a Claude Code already running — and refused as an
//! unknown subcommand such an entry exited 2, Claude Code's BLOCKING error, so
//! every prompt was blocked after an upgrade (round-25 review, 2026-09-23). A
//! `UserPromptSubmit` or `SessionStart` hook's stdout is added to the model's
//! context, which is why stdout must stay empty. The shipped product is this
//! binary (the dev `aterm-link` bin is not shipped), reached three ways a
//! settings file may still hold:
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

/// The Stop hook's JSON, as Claude Code sends it on stdin.
const STOP_INPUT: &[u8] = br#"{"session_id":"x","hook_event_name":"Stop"}"#;

fn run(exe: &Path, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aterm");
    // Ignored on purpose: the negative control's refusal exits without reading.
    let _ = child.stdin.take().expect("stdin").write_all(STOP_INPUT);
    child.wait_with_output().expect("wait")
}

/// `args` with `stdin` written in full (an EPIPE fails the test: the verb must
/// drain what the vendor writes), or with no stdin at all.
fn run_with(exe: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut child = Command::new(exe)
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

/// EVERY SHAPE THE REMOVED INSTALLER WROTE, and a few it did not, through the
/// `aterm-link` argv0 alias (where every `hook …` reaches `retired_hook`, not only
/// `hook run`): each with the vendor's JSON on stdin, with a prompt larger than any
/// pipe buffer (and than the 1 MiB cap a first cut had — the write must never meet a
/// closed pipe), and with no stdin at all. The no-op writes no state; and every OTHER
/// unknown word is still refused by name, exit 2 — a neighbour carrying the retired
/// verb's own arguments included.
#[test]
fn every_retired_hook_shape_through_the_alias_is_a_silent_success() {
    let dir =
        std::env::temp_dir().join(format!("aterm-retired-hook-shapes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let alias = dir.join("aterm-link");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_aterm"), &alias).expect("argv0 alias");
    let state = dir.join("state");
    let state = state.to_string_lossy().into_owned();
    let input = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    let big = vec![b' '; 3 * 1024 * 1024];
    let shapes: [&[&str]; 11] = [
        &["hook", "run", "user-prompt-submit", "--state", &state],
        &["hook", "run", "stop", "--state", &state],
        &["hook", "run", "pre-tool-use", "--state", &state],
        &["hook", "run", "session-start", "--state", &state],
        &["hook", "run", "permission-request", "--state", &state],
        &["hook", "install", "claude"],
        &["hook"],
        // The argv an installed 0.91.0 wrote into the owner's settings.json
        // (2026-09-23), flags and all, and the opt-in rows beside it.
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
        &["hook", "run", "notification"],
        &["hook", "run", "permission-request", "--gate-tools"],
        &["hook", "install", "--dry-run"],
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
