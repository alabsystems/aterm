// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLI stdout/stderr split tests for header-bearing line-framed verbs.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn run_inbox(args: &[&str], reply: &[u8]) -> Output {
    run_verb("inbox", args, reply)
}

fn run_verb(verb: &str, args: &[&str], reply: &[u8]) -> Output {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("at-inbox-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = scratch.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_aterm-ctl"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("ATERM_") && name != aterm_uds::process::VERIFY_RUN_VAR {
            command.env_remove(key);
        }
    }
    let mut cli = command
        .arg("--sock")
        .arg(&socket)
        .args(["@s-0123456789abcdef0123", verb])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut connection = loop {
        match listener.accept() {
            Ok((connection, _)) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(status) = cli.try_wait().unwrap() {
                    let out = cli.wait_with_output().unwrap();
                    panic!(
                        "the CLI exited ({status}) without connecting: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    };
    connection.set_nonblocking(false).unwrap();
    let mut request = String::new();
    BufReader::new(&connection).read_line(&mut request).unwrap();
    connection.write_all(reply).unwrap();
    connection.flush().unwrap();
    drop(connection);
    let output = cli.wait_with_output().unwrap();
    let expected = std::iter::once("@s-0123456789abcdef0123")
        .chain(std::iter::once(verb))
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(request, expected + "\n");
    output
}

#[test]
fn nonempty_inbox_retains_loss_hold_and_ack_metadata_without_changing_rows() {
    let header = "OK 1 hold=1 holder=human seen=7 bus_head=42 dropped=3 pending=2";
    let row = "msg 8 off=42 from=peer kind=task trust=agent len=4 text=work\n";
    let output = run_inbox(&["16", "--peek"], format!("{header}\n{row}").as_bytes());
    assert!(output.status.success());
    assert_eq!(output.stdout, row.as_bytes());
    assert_eq!(output.stderr, format!("aterm-ctl: {header}\n").as_bytes());
}

#[test]
fn empty_inbox_retains_the_same_metadata_shape_once() {
    let header = "OK 0 hold=0 holder=- seen=7 bus_head=42 dropped=3 pending=2";
    let output = run_inbox(&["0", "--peek"], format!("{header}\n").as_bytes());
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, format!("aterm-ctl: {header}\n").as_bytes());
}

#[test]
fn inbox_get_retains_truncation_metadata_and_exact_binary_body() {
    let output = run_inbox(&["get", "8"], b"OK 6 truncated=1 len=100\n\0\xffbody");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\0\xffbody");
    assert_eq!(output.stderr, b"aterm-ctl: OK 6 truncated=1 len=100\n");
}

#[test]
fn empty_inbox_get_reports_its_header_without_fabricating_a_body() {
    let output = run_inbox(&["get", "8"], b"OK 0\n");
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"aterm-ctl: OK 0\n");
}

#[test]
fn offscreen_keeps_its_header_on_stderr_and_its_rows_on_stdout() {
    let header = "OK 3 first=41 last=42 lost=0 breaks=0 back=1 epoch=2 origin=5 alt=1 seq=9 \
                  screen_rows=1";
    let body = "❯ Pick one\n⏺ Looking at both.\n⏺ Looking at both.\n";
    let output = run_verb(
        "offscreen",
        &["since=5:40", "screen=1"],
        format!("{header}\n{body}").as_bytes(),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, body.as_bytes());
    assert_eq!(output.stderr, format!("aterm-ctl: {header}\n").as_bytes());

    let header = "OK 0 first=43 last=42 lost=0 breaks=0 back=0 epoch=2 origin=5 alt=1 seq=9";
    let output = run_verb("offscreen", &["since=42"], format!("{header}\n").as_bytes());
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, format!("aterm-ctl: {header}\n").as_bytes());
}
