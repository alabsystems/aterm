// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REPLY GRAMMAR OVER THE REAL SOCKET for `controls`, `open … close`,
//! `await`, `temporal` and `history`, read back as a client reads it — the
//! status line, then exactly the
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
//! product that cannot start fails; a scratch/spawn refusal is COULD NOT RUN,
//! `boot_with`, never a pass).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use aterm_types::control_verbs::{Framing, framing_of};

use crate::headless_boot;

use headless_boot::Instance;

/// How long a reply may take before it is read as a hang (AGENTS.md: a hang
/// detector is a minute, never a latency budget).
const REPLY_DEADLINE: Duration = Duration::from_secs(60);

fn token(inst: &Instance) -> String {
    let mut p = std::ffi::OsString::from(&inst.sock);
    p.push(".token");
    std::fs::read_to_string(PathBuf::from(p))
        .expect("the owner token file")
        .trim()
        .to_string()
}

/// One boot with the temporal recorder on (so `temporal` has a spine to read)
/// and the token file written. `prefix` names its scratch root, which is
/// per process: each test boots under its own. A refused boot is COULD NOT
/// RUN, never a pass.
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
    let inst = boot("atrg");

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
                "by",
                "arch",
            ] {
                assert!(field(row, key).is_some(), "history row lacks {key}=: {row}");
            }
            // The test's own connection presents the owner token: `ctl`.
            assert_eq!(field(row, "by"), Some("ctl"), "{row}");
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

/// One persistent connection, as a supervisor holds one: `AUTH <token>`
/// first (no reply), then requests whose status lines are read in turn. A
/// lease belongs to the connection that took it, so a hand's acquire, the
/// writes under it and its release go out on one.
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

    /// The one status line `line` is answered with.
    fn ask(&mut self, line: &str) -> String {
        writeln!(self.writer, "{line}").expect("write the request");
        let mut status = String::new();
        self.reader
            .read_line(&mut status)
            .unwrap_or_else(|e| panic!("{line}: no status line ({e})"));
        status.trim_end_matches('\n').to_string()
    }
}

/// A program that draws `READY` and reads the terminal in raw mode with no
/// echo, keeping what it reads in `$1`: typing into it moves nothing on the
/// screen, so one read's generation fences every write after it.
const QUIET_READER: &str = "#!/bin/sh\nstty raw -echo\nprintf 'READY\\r\\n'\nexec cat > \"$1\"\n";

/// THE WINDOW HARNESS'S `send` AND `key enter` ARE ITS TURN, over the real
/// socket and on the turn-end loop's own wire: on one connection, the hand
/// taken as the loop takes it (holder `aterm-harness@<this instance's pid>`,
/// the window harness's own name, `hard here`), the fenced text (`send
/// if-gen=<the read's generation> if=<a row of it> -- …`), its fenced Enter
/// (`key if-gen=… if=… enter`), the hand given back (`here`) — is one
/// `history` row, `by=harness status=pressed submitted=1`, its text the
/// tail. NEGATIVE CONTROLS: the same fenced `send` and `key enter` with no
/// hand, and the unfenced lines that start the reader, add no row (a raw
/// driver's line is no turn); an owner-token `turn` is `by=ctl` (the test
/// above).
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

/// A HAND TAKEN `here` ENDS WITH ITS CONNECTION, over the real socket: the
/// turn-end loop's hand, when a stop hangs the loop's connection up before
/// the give-back (the cut lets no `lease` out after it). One connection
/// takes the harness's hand as the loop takes it (`hard here`) and hangs up
/// holding it; the tab is then free (`lease status` reads `none`, waited for
/// with a one-minute hang detector, since the server reads the end on its
/// own lane; the hand's TTL is ten, so a lapse cannot pass for it), and
/// another connection's write is not refused `ERR busy`.
/// NEGATIVE CONTROL: the same hand taken without `here` outlives its
/// connection — read once a supervisor claim that connection bound to
/// itself has been released by the same sweep, so the end was read.
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

    // NEGATIVE CONTROL first: a plain hand outlives its connection.
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
