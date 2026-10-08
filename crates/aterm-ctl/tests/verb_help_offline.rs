// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl` answers `<verb> --help` client-side, without a socket. Every
//! verb the client knows (every protocol-table row and every client verb),
//! asked with `--help` against a socket path that cannot exist, exits 0,
//! prints the verb's entry, and reports no connect error.
//!
//! No live aterm session is discovered or addressed: the socket named does not
//! exist, and the child runs with every inherited `ATERM_*` variable removed.

use std::process::{Command, Output, Stdio};

/// The client verbs `--help` documents in its client-verbs block (not in the
/// protocol table; the lib's `CLIENT_HELP_VERBS`).
const CLIENT_VERBS: [&str; 4] = ["ls", "instances", "windows", "mux"];

fn ask_help(verb: &str) -> Output {
    ctl(&[verb, "--help"])
}

/// `aterm-ctl --sock <a path that cannot exist> <args…>`, every inherited
/// `ATERM_*` and multiplexer variable removed.
fn ctl(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aterm-ctl"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("ATERM_") || name == "TMUX" || name == "STY" || name == "ZELLIJ" {
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
fn every_verb_help_is_answered_without_a_socket() {
    let names = aterm_types::control_verbs::VERBS
        .iter()
        .map(|v| v.name)
        .chain(CLIENT_VERBS);
    let mut checked = 0usize;
    for verb in names {
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
        checked += 1;
    }
    // Vacuity guard: the table is the whole protocol, not an empty slice.
    assert!(checked > 80, "only {checked} verbs were asked");
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

/// An explicit `help <protocol verb>` is the server's question (its `cmd_help`
/// answers for its own build), so it dials and fails on a dead socket; only
/// `<verb> --help` is answered from the client's table.
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
