// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl` answers `<verb> --help` client-side, without a socket: a
//! protocol-table verb and a client verb, asked with `--help` against a socket
//! path that cannot exist, exit 0, print the verb's entry, and report no
//! connect error. (Every table row's entry is pinned in-process by the lib's
//! `help_is_answered_client_side_only_for_what_the_client_knows`.)
//!
//! No live aterm session is discovered or addressed: the socket named does not
//! exist, and the child runs with every inherited `ATERM_*` variable removed.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::process::{Command, Output, Stdio};

fn ask_help(verb: &str) -> Output {
    ctl(&[verb, "--help"])
}

/// `aterm-ctl --sock <a path that cannot exist> <args…>`, every inherited
/// `ATERM_*` and multiplexer variable removed.
fn ctl(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aterm-ctl"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        // The verify run's mark is no authority and stays (`VERIFY_RUN_VAR`).
        let aterm = name.starts_with("ATERM_") && name != aterm_uds::process::VERIFY_RUN_VAR;
        if aterm || name == "TMUX" || name == "STY" || name == "ZELLIJ" {
            command.env_remove(key);
        }
    }
    command
        .args(["--sock", "/nonexistent/nope.sock"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run aterm-ctl")
}

#[test]
fn a_verbs_help_is_answered_without_a_socket() {
    for verb in ["text", "turn", "ls", "mux"] {
        let out = ask_help(verb);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "`{verb} --help` must exit 0 with no socket: {:?}\nstdout: {stdout}\nstderr: {stderr}",
            out.status
        );
        // A dial names the socket it tried (`aterm-ctl: connect <path>: …`); the
        // entry text itself may say "connect" (`window`'s targets do).
        assert!(
            !stderr.contains("nope.sock") && !stdout.contains("nope.sock"),
            "`{verb} --help` must not dial: stdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            stdout.lines().next().is_some_and(|l| l.starts_with(verb)),
            "`{verb} --help` prints the verb's entry: {stdout}"
        );
    }
}

/// Negative control: a name no table knows goes to the server, so with a dead
/// socket it fails.
#[test]
fn an_unknown_verbs_help_still_needs_the_server() {
    let out = ask_help("nonesuch-verb");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("nope.sock"),
        "an unknown verb is not client-answered, so it dials: {:?} {stderr}",
        out.status,
    );
}

/// An explicit `help <protocol verb>` is the server's question (it answers for its own
/// build), so it dials; only `<verb> --help` is answered from the client's table.
#[test]
fn an_explicit_help_verb_still_asks_the_server() {
    let out = ctl(&["help", "text"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("nope.sock"),
        "`help text` goes to the server, so with a dead socket it dials and fails: {:?} {stderr}",
        out.status,
    );
}
