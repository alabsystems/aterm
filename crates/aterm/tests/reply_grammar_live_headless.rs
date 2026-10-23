// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies reply framing (`Status`, `Lines`, `Bytes`) over a live `aterm --headless` socket
//! against `aterm_types::control_verbs::framing_of`, plus harness turn ledger recording and
//! connection-bound `here` lease release.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use aterm_types::control_verbs::{Framing, framing_of};

use crate::headless_boot;

use headless_boot::Instance;

const REPLY_DEADLINE: Duration = Duration::from_secs(60);

fn token(inst: &Instance) -> String {
    let mut p = std::ffi::OsString::from(&inst.sock);
    p.push(".token");
    std::fs::read_to_string(PathBuf::from(p))
        .expect("the owner token file")
        .trim()
        .to_string()
}

/// Boots an isolated headless instance with `temporal_recording = true` and waits for its token file.
fn boot(prefix: &str) -> Instance {
    headless_boot::boot_with(
        prefix,
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

/// Sends `line` and reads one reply framed according to `framing_of`.
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
    let inst = boot("atrg");

    match wire(&inst, "await seq 0 timeout=0") {
        Reply::Status(s) => assert!(s.starts_with("OK") || s.starts_with("ERR "), "{s}"),
        other => panic!("await is Status-framed: {other:?}"),
    }

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
                "by",
                "arch",
            ] {
                assert!(field(row, key).is_some(), "history row lacks {key}=: {row}");
            }
            assert_eq!(field(row, "by"), Some("ctl"), "{row}");
            let (_, text) = row.split_once(" text=").expect("text= is the tail");
            assert!(text.contains("grammar-lane"), "{row}");
        }
        other => panic!("history is Lines-framed: {other:?}"),
    }

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
    match wire(&inst, "open no-such-target") {
        Reply::Status(s) => assert!(s.starts_with("ERR "), "{s}"),
        other => panic!("{other:?}"),
    }

    match wire(&inst, "history 1") {
        Reply::Lines(s, rows) => assert_eq!(s, format!("OK {}", rows.len())),
        other => panic!("{other:?}"),
    }
}

/// Persistent `AUTH <token>` control connection for connection-bound lease requests.
struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn open(inst: &Instance) -> Self {
        let writer = UnixStream::connect(&inst.sock).expect("connect the control socket");
        writer.set_read_timeout(Some(REPLY_DEADLINE)).unwrap();
        writer.set_write_timeout(Some(REPLY_DEADLINE)).unwrap();
        let mut writer = writer;
        writeln!(writer, "AUTH {}", token(inst)).expect("authenticate");
        let reader = BufReader::new(writer.try_clone().expect("a second handle"));
        Conn { reader, writer }
    }

    fn ask(&mut self, line: &str) -> String {
        writeln!(self.writer, "{line}").expect("write the request");
        let mut status = String::new();
        self.reader
            .read_line(&mut status)
            .unwrap_or_else(|e| panic!("{line}: no status line ({e})"));
        status.trim_end_matches('\n').to_string()
    }
}

/// Prints `READY` and reads raw stdin with no echo so subsequent writes do not advance the screen generation.
const QUIET_READER: &str = "#!/bin/sh\nstty raw -echo\nprintf 'READY\\r\\n'\nexec cat > \"$1\"\n";

/// Verifies that fenced `send` and `key enter` under an `aterm-harness@<pid>` `hard here` lease
/// record one `by=harness status=pressed submitted=1` ledger row, while unfenced or unleased writes do not.
#[test]
fn the_harness_send_and_enter_land_in_the_ledger_as_its_turn() {
    let inst = boot("atrh");
    let history = |inst: &Instance| match wire(inst, "history") {
        Reply::Lines(_, rows) => rows,
        other => panic!("history is Lines-framed: {other:?}"),
    };
    let before = history(&inst);
    let script = inst.tmp.join("quiet.sh");
    let keys = inst.tmp.join("keys.log");
    std::fs::write(&script, QUIET_READER).expect("write the reader");
    let mut c = Conn::open(&inst);
    let ok = |c: &mut Conn, line: &str| {
        let reply = c.ask(line);
        assert!(
            reply.starts_with("OK") && !reply.contains("skipped"),
            "{line}: {reply}"
        );
    };
    ok(
        &mut c,
        &format!(
            "send -- exec /bin/sh {} {}",
            script.display(),
            keys.display()
        ),
    );
    ok(&mut c, "key enter");
    match wire(&inst, "await match READY timeout=60000") {
        Reply::Status(s) => assert!(s.starts_with("OK") && !s.contains("timeout"), "{s}"),
        other => panic!("await is one status line: {other:?}"),
    }
    let fenced = |c: &mut Conn, text: &str| {
        let status = c.ask("status");
        let generation = field(&status, "gen")
            .unwrap_or_else(|| panic!("status names a generation: {status}"))
            .to_string();
        ok(c, &format!("send if-gen={generation} if=READY -- {text}"));
        ok(c, &format!("key if-gen={generation} if=READY enter"));
    };
    fenced(&mut c, "no-hand");
    assert_eq!(history(&inst), before, "no hand, no turn");

    let holder = format!("aterm-harness@{}", inst.child.id());
    let took = c.ask(&format!(
        "lease acquire ttl=60000 holder={holder} hard here"
    ));
    assert!(took.starts_with("OK lease acquired"), "{took}");
    fenced(&mut c, "harness-lane");
    assert_eq!(
        c.ask(&format!("lease release holder={holder} here")),
        "OK lease released"
    );
    let after = history(&inst);
    assert_eq!(after.len(), before.len() + 1, "{after:#?}");
    let row = after.last().expect("the harness's row");
    for (key, want) in [("by", "harness"), ("status", "pressed"), ("submitted", "1")] {
        assert_eq!(field(row, key), Some(want), "{key}: {row}");
    }
    let (_, text) = row.split_once(" text=").expect("text= is the tail");
    assert_eq!(text, "harness-lane", "{row}");
}

/// Verifies that a lease acquired with `here` is released when its connection closes, whereas a
/// lease without `here` outlives its connection.
#[test]
fn a_hand_taken_here_ends_with_its_connection() {
    let inst = boot("atrl");
    let holder = format!("aterm-harness@{}", inst.child.id());
    let status_line = |line: &str| match wire(&inst, line) {
        Reply::Status(s) => s,
        other => panic!("{line} is one status line: {other:?}"),
    };
    let ok = |c: &mut Conn, line: &str| {
        let reply = c.ask(line);
        assert!(reply.starts_with("OK"), "{line}: {reply}");
    };

    let mut c = Conn::open(&inst);
    ok(&mut c, "meta set supervisor grammar-sweep");
    ok(
        &mut c,
        &format!("lease acquire ttl=600000 holder={holder} hard"),
    );
    drop(c);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the ended connection's bound claim released",
        || {
            let meta = status_line("meta");
            (field(&meta, "supervisor") == Some("-")).then_some(())
        },
    );
    let plain = status_line("lease status");
    assert!(
        plain.starts_with(&format!("OK lease holder={holder} ")),
        "a plain hand outlives its connection: {plain}"
    );
    assert_eq!(
        status_line(&format!("lease release holder={holder}")),
        "OK lease released"
    );

    let mut c = Conn::open(&inst);
    ok(
        &mut c,
        &format!("lease acquire ttl=600000 holder={holder} hard here"),
    );
    assert!(
        status_line("send -- x").starts_with("ERR busy lease="),
        "held while its connection lives"
    );
    drop(c);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the hand taken `here` freed with its connection",
        || (status_line("lease status") == "OK lease none").then_some(()),
    );
    let write = status_line("send -- x");
    assert!(write.starts_with("OK"), "free to write: {write}");
}
