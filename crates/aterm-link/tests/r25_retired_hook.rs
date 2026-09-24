// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Round 25's retired `hook` verb never blocks the agent.**
//!
//! Round 25 cut the vendor hooks, but a hook entry can survive the removal: a project's
//! `.claude/settings.local.json` (the agents pass never opens one), a host the pass has
//! not reached, a Claude Code already running. Refused as an unknown subcommand, such an
//! entry exited 2 — Claude Code's BLOCKING error — so every prompt was blocked after an
//! upgrade (round-25 review, 2026-09-23). The real binary, every shape the removed
//! installer wrote and a few it did not: exit 0, nothing on stdout (a `UserPromptSubmit` or
//! `SessionStart` hook's stdout is added to the model's context), one stderr line naming
//! the removal — with the vendor's JSON on stdin as Claude Code sends it, and with none.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn hook(args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aterm-link");
    if let Some(bytes) = stdin {
        let mut pipe = child.stdin.take().expect("stdin");
        pipe.write_all(bytes).expect("write the hook input");
    }
    child.wait_with_output().expect("aterm-link output")
}

#[test]
fn every_retired_hook_invocation_exits_zero_silently_with_one_stderr_line() {
    let state = std::env::temp_dir().join(format!("aterm-link-r25-hook-{}", std::process::id()));
    let state = state.to_string_lossy().into_owned();
    let input = br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    // A prompt larger than any pipe buffer (and than the 1 MiB cap a first cut had): the
    // write must never meet a closed pipe, which `write_all` below would report as EPIPE.
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
    for args in shapes {
        for stdin in [Some(&input[..]), Some(&big[..]), None] {
            let out = hook(args, stdin);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{args:?}: exit 2 is Claude Code's blocking error — {stderr}"
            );
            assert!(
                out.stdout.is_empty(),
                "{args:?}: stdout reaches the model's context: {:?}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert_eq!(
                stderr,
                format!("{}\n", aterm_link::cli::RETIRED_HOOK_LINE),
                "{args:?}: one line naming the removal"
            );
        }
    }
    assert!(
        !std::path::Path::new(&state).exists(),
        "the no-op writes no state"
    );
    // Every OTHER unknown word is still refused by name, exit 2 — a neighbour
    // with the retired verb's own arguments included.
    for word in [&["hoook"][..], &["hooks", "run", "stop"][..]] {
        let out = hook(word, None);
        assert_eq!(out.status.code(), Some(2), "{word:?}");
        let refused = format!("unknown subcommand `{}`", word[0]);
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&refused),
            "{word:?}"
        );
    }
}
