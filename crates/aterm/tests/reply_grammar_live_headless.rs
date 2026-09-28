// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REPLY GRAMMAR OVER THE REAL SOCKET for the verbs no lane drove end to
//! end: `controls`, `open … close`, `await`, `temporal` and `history` (the
//! INTROSPECTION audit's F13/F15 remainder). Each had handler-level tests; none
//! was read back as a client reads it — the status line, then exactly the
//! frame its `Framing` promises (`OK <n>` + n rows for Lines, `OK <nbytes>` +
//! exactly those bytes for Bytes, one line for Status) — through a real
//! `aterm --headless`.
//!
//! The client speaks the wire directly (`TOKEN <owner token> <verb …>`), so a
//! frame the server over- or under-writes is a failure here, not something a
//! forgiving client papers over. Every reply is checked against the verb
//! table's framing (`aterm_types::control_verbs::framing_of`), so the lane
//! cannot drift from the table either.
//!
//! ISOLATION: `support/launch_isolation.rs` and `support/headless_boot.rs` (a
//! product that cannot start fails; only a scratch/spawn refusal skips).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use aterm_types::control_verbs::{Framing, framing_of};

#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use headless_boot::Instance;

const REPLY_DEADLINE: Duration = Duration::from_secs(30);

fn token(inst: &Instance) -> String {
    let mut p = std::ffi::OsString::from(&inst.sock);
    p.push(".token");
    std::fs::read_to_string(PathBuf::from(p))
        .expect("the owner token file")
        .trim()
        .to_string()
}

/// One boot with the temporal recorder on (so `temporal` has a spine to read)
/// and the token file written.
fn boot() -> Option<Instance> {
    headless_boot::boot_with(
        "atrg",
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            std::fs::write(&cfg, format!("temporal_recording = true\n{base}"))
                .expect("write the config");
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |inst| {
            let mut p = std::ffi::OsString::from(&inst.sock);
            p.push(".token");
            PathBuf::from(p).is_file()
        },
    )
}

/// A reply as the wire framed it.
#[derive(Debug)]
enum Reply {
    /// One status line (`OK …` / `ERR …`).
    Status(String),
    /// `OK <n> …` and the n rows that followed.
    Lines(String, Vec<String>),
    /// `OK <nbytes> …` and exactly those bytes.
    Bytes(String, Vec<u8>),
}

/// Send `line` and read ONE reply as `verb`'s table framing says: the status
/// line, then — only on `OK` — the rows or bytes its count names. Anything the
/// status line does not account for (a missing row, a short body) fails.
fn wire(inst: &Instance, line: &str) -> Reply {
    let verb = line.split_whitespace().next().expect("a verb");
    assert!(
        aterm_types::control_verbs::spec(verb).is_some(),
        "{verb}: not in the verb table"
    );
    let framing = framing_of(verb, line);
    let mut stream = UnixStream::connect(&inst.sock).expect("connect the control socket");
    stream.set_read_timeout(Some(REPLY_DEADLINE)).unwrap();
    stream.set_write_timeout(Some(REPLY_DEADLINE)).unwrap();
    writeln!(stream, "TOKEN {} {line}", token(inst)).expect("write the request");
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("{line}: no status line ({e})"));
    assert!(
        status.ends_with('\n'),
        "{line}: an unterminated status line {status:?}"
    );
    let status = status.trim_end_matches('\n').to_string();
    assert!(
        status.starts_with("OK") || status.starts_with("ERR "),
        "{line}: a status line is OK or ERR: {status:?}"
    );
    let count = || -> usize {
        status
            .strip_prefix("OK ")
            .and_then(|t| t.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("{line}: an OK without a count: {status:?}"))
    };
    if !status.starts_with("OK") {
        return Reply::Status(status);
    }
    match framing {
        Framing::Status | Framing::Push => Reply::Status(status),
        Framing::Lines => {
            let n = count();
            let mut rows = Vec::with_capacity(n);
            for i in 0..n {
                let mut row = String::new();
                reader
                    .read_line(&mut row)
                    .unwrap_or_else(|e| panic!("{line}: row {i} of {n} missing ({e})"));
                assert!(
                    row.ends_with('\n'),
                    "{line}: row {i} of {n} cut short: {row:?}"
                );
                rows.push(row.trim_end_matches('\n').to_string());
            }
            Reply::Lines(status, rows)
        }
        Framing::Bytes => {
            let n = count();
            let mut body = vec![0u8; n];
            reader
                .read_exact(&mut body)
                .unwrap_or_else(|e| panic!("{line}: the {n}-byte body came short ({e})"));
            Reply::Bytes(status, body)
        }
    }
}

fn field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

#[test]
fn the_audit_verbs_frame_their_replies_as_the_table_says() {
    let Some(inst) = boot() else { return };

    // `await seq <n> timeout=0`: the cheap dirty check — a Status line.
    match wire(&inst, "await seq 0 timeout=0") {
        Reply::Status(s) => assert!(s.starts_with("OK") || s.starts_with("ERR "), "{s}"),
        other => panic!("await is Status-framed: {other:?}"),
    }

    // A turn, so the ledger has a row: `history` is Lines, one row per turn,
    // each carrying its fields with `text=` the free-text tail.
    match wire(&inst, "turn idle=200 timeout=10000 -- echo grammar-lane") {
        Reply::Lines(s, _) | Reply::Status(s) if s.starts_with("OK") => {}
        other => panic!("the turn ran: {other:?}"),
    }
    match wire(&inst, "history") {
        Reply::Lines(s, rows) => {
            assert_eq!(s, format!("OK {}", rows.len()), "{s}");
            let row = rows.last().expect("the turn's ledger row");
            let mut head = row.split_whitespace();
            assert_eq!(head.next(), Some("turn"), "{row}");
            assert!(
                head.next().is_some_and(|id| id.parse::<u64>().is_ok()),
                "a numeric turn id: {row}"
            );
            for key in [
                "submitted",
                "status",
                "started_ms",
                "dur_ms",
                "seq",
                "hash",
                "arch",
            ] {
                assert!(field(row, key).is_some(), "history row lacks {key}=: {row}");
            }
            let (_, text) = row.split_once(" text=").expect("text= is the tail");
            assert!(text.contains("grammar-lane"), "{row}");
        }
        other => panic!("history is Lines-framed: {other:?}"),
    }

    // `temporal status` is one status line naming the reachable ticks; a tick
    // read is the Bytes frame, exactly the counted body — the screen then.
    let latest = match wire(&inst, "temporal status") {
        Reply::Status(s) => {
            assert!(s.starts_with("OK enabled=true "), "the recorder is on: {s}");
            field(&s, "latest_tick")
                .and_then(|t| t.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("a latest_tick: {s}"))
        }
        other => panic!("`temporal status` is a status line: {other:?}"),
    };
    match wire(&inst, &format!("temporal {latest}")) {
        Reply::Bytes(s, body) => {
            assert!(s.starts_with(&format!("OK {}", body.len())), "{s}");
            let screen = String::from_utf8_lossy(&body);
            assert!(
                screen.contains("grammar-lane"),
                "the turn's screen: {screen:?}"
            );
        }
        other => panic!("a tick read is Bytes-framed: {other:?}"),
    }
    match wire(&inst, "temporal status trim") {
        Reply::Status(s) => assert!(s.starts_with("ERR usage"), "{s}"),
        other => panic!("`status trim` is a usage error: {other:?}"),
    }

    // `controls` (Lines) and `open <target>` / `open <target> close` (Status).
    // A headless instance has no front window, so each either answers in its
    // frame or refuses on one ERR line — never a half frame.
    for line in ["controls", "controls prefs"] {
        match wire(&inst, line) {
            Reply::Lines(s, rows) => assert_eq!(s, format!("OK {}", rows.len()), "{line}"),
            Reply::Status(s) => assert!(s.starts_with("ERR "), "{line}: {s}"),
            other => panic!("{line} is Lines-framed: {other:?}"),
        }
    }
    for line in [
        "open prefs",
        "open prefs close",
        "open about",
        "open about close",
    ] {
        match wire(&inst, line) {
            Reply::Status(s) => assert!(s.starts_with("OK") || s.starts_with("ERR "), "{s}"),
            other => panic!("{line} is Status-framed: {other:?}"),
        }
    }
    // An unknown target is a usage error naming the roster, on one line.
    match wire(&inst, "open no-such-target") {
        Reply::Status(s) => assert!(s.starts_with("ERR "), "{s}"),
        other => panic!("{other:?}"),
    }

    // The connection is still in sync after every frame above: a fresh read
    // answers its own status line, not a leftover row.
    match wire(&inst, "history 1") {
        Reply::Lines(s, rows) => assert_eq!(s, format!("OK {}", rows.len())),
        other => panic!("{other:?}"),
    }
}
