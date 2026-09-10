// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise the actual CLI's stdout/stderr split against a private protocol peer.
//! No live aterm session is discovered or addressed by these tests.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_inbox(args: &[&str], reply: &[u8]) -> Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // Keep the socket below macOS's short sun_path limit.
    let directory = PathBuf::from("/tmp").join(format!(
        "at-inbox-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let scratch = Scratch(directory);
    let socket = scratch.0.join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let reply = reply.to_vec();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connection = loop {
            match listener.accept() {
                Ok((connection, _)) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI never connected");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        connection
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = String::new();
        BufReader::new(&connection).read_line(&mut request).unwrap();
        connection.write_all(&reply).unwrap();
        connection.flush().unwrap();
        request
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_aterm-ctl"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ATERM_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("--sock")
        .arg(&socket)
        .args(["--timeout", "2", "@s-0123456789abcdef0123", "inbox"])
        .args(args)
        .output()
        .unwrap();
    let expected = std::iter::once("@s-0123456789abcdef0123")
        .chain(std::iter::once("inbox"))
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(server.join().unwrap(), expected + "\n");
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
fn inbox_seen_remains_an_ordinary_status_reply() {
    let output = run_inbox(&["seen", "8", "handled"], b"OK seen=8\n");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"OK seen=8\n");
    assert!(output.stderr.is_empty());
}
