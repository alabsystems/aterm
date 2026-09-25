// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the unified message system's two verbs,
//! `messages` (the log's text face) and `notice` (the outside world's voice
//! on the band), driven over a real headless instance's control socket
//! (design §5, rulings 163-198; the Phase 5 spec's live scenario).
//!
//! The socket round trip is what the engine and host unit tests cannot see:
//! the control thread's refusals (never a wake), the owner token, the main
//! thread's `wire::apply` replies, `OK recorded` for a note, the same id on
//! every progress restate, `done` then `how=gone`, `dismiss` then `act`
//! finding nothing, `appstatus` never listing a script's rows, and `help`
//! carrying both rows.
//!
//! The client speaks the wire directly (`TOKEN <owner token> <verb …>`, the
//! status line, then `<n>` rows for a Lines verb), so every assertion is on
//! the exact bytes a script reads.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer. SKIP (not fail) when the instance cannot boot.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const REPLY_DEADLINE: Duration = Duration::from_secs(30);
const MAX_SOCK_PATH: usize = 100;

/// One booted headless instance plus its scratch world, torn down on every
/// exit path (Drop runs on panic too).
struct Instance {
    child: Child,
    tmp: PathBuf,
    log: PathBuf,
    sock: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

fn scratch_root(tag: &str) -> Option<PathBuf> {
    let name = format!("atms{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let tmp = base.join(&name);
        let sock = tmp.join("run/aterm/aterm.sock");
        if sock.as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&tmp).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }
        return Some(tmp);
    }
    None
}

/// Boot one headless instance, 40x120. `None` = an environmental refusal,
/// announced as a SKIP with the log tail.
fn boot(tag: &str) -> Option<Instance> {
    let Some(tmp) = scratch_root(tag) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .env("ATERM_LINES", "40")
        .env("ATERM_COLUMNS", "120")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let sock = tmp.join("run/aterm/aterm.sock");
    let mut inst = Instance {
        child,
        sock,
        tmp,
        log,
    };
    for _ in 0..SOCKET_POLLS {
        if matches!(inst.child.try_wait(), Ok(Some(_)) | Err(_)) {
            eprintln!(
                "SKIP: aterm --headless exited before binding its socket; log tail:\n{}",
                log_tail(&inst.log)
            );
            return None;
        }
        if is_socket_or_symlink(&inst.sock)
            && token_path(&inst).is_file()
            && launch_isolation::control_listening(&inst.sock)
        {
            return Some(inst);
        }
        std::thread::sleep(POLL_GAP);
    }
    eprintln!(
        "SKIP: control socket never started listening; log tail:\n{}",
        log_tail(&inst.log)
    );
    None
}

fn token_path(inst: &Instance) -> PathBuf {
    let mut p = inst.sock.clone().into_os_string();
    p.push(".token");
    PathBuf::from(p)
}

/// The instance's owner token: `notice` is `OwnerOnly`.
fn token(inst: &Instance) -> String {
    std::fs::read_to_string(token_path(inst))
        .expect("the owner token file")
        .trim()
        .to_string()
}

/// A Lines verb: its status line is `OK <n> …` and `<n>` rows follow.
fn is_lines_verb(line: &str) -> bool {
    matches!(
        line.split_whitespace().next(),
        Some("messages" | "appstatus" | "help")
    )
}

/// One request over the raw socket with the owner token: the status line and,
/// for a Lines verb answering `OK <n>`, its `<n>` rows.
fn ctl(inst: &Instance, line: &str) -> (String, Vec<String>) {
    let mut stream = UnixStream::connect(&inst.sock).expect("connect the control socket");
    stream.set_read_timeout(Some(REPLY_DEADLINE)).unwrap();
    stream.set_write_timeout(Some(REPLY_DEADLINE)).unwrap();
    writeln!(stream, "TOKEN {} {line}", token(inst)).expect("write the request");
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader
        .read_line(&mut status)
        .unwrap_or_else(|e| panic!("{line}: no status line ({e})"));
    let status = status.trim_end().to_string();
    let mut rows = Vec::new();
    if is_lines_verb(line)
        && let Some(n) = status
            .strip_prefix("OK ")
            .and_then(|t| t.split_whitespace().next())
            .and_then(|n| n.parse::<usize>().ok())
    {
        for _ in 0..n {
            let mut row = String::new();
            reader
                .read_line(&mut row)
                .unwrap_or_else(|e| panic!("{line}: a row short ({e})"));
            rows.push(row.trim_end().to_string());
        }
    }
    (status, rows)
}

/// The status line alone.
fn status(inst: &Instance, line: &str) -> String {
    ctl(inst, line).0
}

/// `key=value` out of a row.
fn field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

/// A `messages` row's id.
fn row_id(row: &str) -> u64 {
    row.strip_prefix("message ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("not a message row: {row}"))
}

/// The id at the end of `OK message=<id>` / `OK recorded message=<id>`.
fn minted(reply: &str) -> u64 {
    reply
        .rsplit_once("message=")
        .and_then(|(_, id)| id.parse().ok())
        .unwrap_or_else(|| panic!("no minted id in {reply:?}"))
}

/// The rows above `after` (ids start at 1 and `since=0` is a usage error,
/// ruling 187: a reader that has seen nothing reads bare).
fn rows_after(inst: &Instance, after: u64) -> Vec<String> {
    let line = if after == 0 {
        "messages 512".to_string()
    } else {
        format!("messages since={after}")
    };
    let (st, rows) = ctl(inst, &line);
    assert!(st.starts_with("OK "), "{line}: {st}");
    rows.into_iter().filter(|r| row_id(r) > after).collect()
}

/// The live row with `id`, from `messages live`.
fn live_row(inst: &Instance, id: u64) -> String {
    let (st, rows) = ctl(inst, "messages live");
    assert!(st.starts_with("OK "), "messages live: {st}");
    rows.into_iter()
        .find(|r| row_id(r) == id)
        .unwrap_or_else(|| panic!("message {id} is not live"))
}

#[test]
fn the_message_verbs_round_trip_on_a_live_instance() {
    let Some(inst) = boot("m") else { return };

    // 1. The log as it stands; the last id seen.
    let (st, rows) = ctl(&inst, "messages");
    assert_eq!(st, format!("OK {}", rows.len()), "{st}");
    let b = rows.last().map_or(0, |r| row_id(r));

    // 2. A note from a script is a RECORD, never a row.
    let a = status(&inst, "notice post system hello from a script");
    assert!(a.starts_with("OK recorded message="), "{a}");
    let a = minted(&a);
    // aterm's own boot reports (a config note) may land beside it: the
    // script's rows are the wire's.
    let after: Vec<String> = rows_after(&inst, b)
        .into_iter()
        .filter(|r| field(r, "origin") != Some("host"))
        .collect();
    assert_eq!(after.len(), 1, "{after:?}");
    let rec = &after[0];
    assert_eq!(row_id(rec), a);
    assert_eq!(field(rec, "origin"), Some("wire"), "{rec}");
    assert_eq!(field(rec, "state"), Some("recorded"), "{rec}");
    assert_eq!(field(rec, "glass"), Some("-"), "{rec}");

    // 3. A failure is a held row, its detail in the log.
    let w = status(
        &inst,
        "notice post system sev=warn key=deploy Deploy failed -- connection refused",
    );
    assert!(w.starts_with("OK message="), "{w}");
    let w = minted(&w);
    let row = live_row(&inst, w);
    assert_eq!(field(&row, "state"), Some("held"), "{row}");
    assert_eq!(field(&row, "key"), Some("wire.deploy"), "{row}");
    assert_eq!(field(&row, "detail"), Some("connection%20refused"), "{row}");

    // 4. Progress: one row per key, the same id on every restate.
    let p = status(&inst, "notice progress build pct=40 Building aterm");
    assert!(p.starts_with("OK message="), "{p}");
    let p = minted(&p);
    assert_eq!(
        status(&inst, "notice progress build pct=41 Building aterm"),
        format!("OK message={p}")
    );
    assert_eq!(
        field(&live_row(&inst, p), "progress"),
        Some("41/100"),
        "a restate moves the fill"
    );
    assert_eq!(
        status(&inst, "notice progress build Building aterm -- 3 of 10"),
        format!("OK message={p}")
    );
    let row = live_row(&inst, p);
    assert_eq!(field(&row, "busy"), Some("1"), "{row}");
    assert_eq!(field(&row, "progress"), None, "{row}");

    // 5. `done` ends it with its finish; a second `done` finds nothing.
    assert_eq!(
        status(&inst, "notice done build ok Built aterm"),
        format!("OK done={p} how=resolved-ok")
    );
    let ended = rows_after(&inst, p - 1)
        .into_iter()
        .find(|r| row_id(r) == p)
        .expect("the progress row's record");
    assert_eq!(field(&ended, "state"), Some("resolved-ok"), "{ended}");
    assert_eq!(field(&ended, "title"), Some("Built%20aterm"), "{ended}");
    assert_eq!(status(&inst, "notice done build"), "OK done=- how=gone");

    // 6. Dismiss takes the failure down; a press then finds no row.
    assert_eq!(
        status(&inst, &format!("notice dismiss {w}")),
        format!("OK dismissed={w}")
    );
    assert_eq!(
        status(&inst, &format!("notice act {w} 0")),
        format!("ERR notice: no live message {w}")
    );

    // 7. Refusals, every one answered on the control thread.
    assert!(
        status(&inst, "notice progress").starts_with("ERR usage: notice progress "),
        "a bare progress"
    );
    assert_eq!(
        status(
            &inst,
            "notice post system sev=warn one two three four five six seven"
        ),
        "ERR notice: a glass title over six words"
    );
    assert_eq!(
        status(&inst, "notice post update sev=warn Nope"),
        "ERR notice: the toolchain, update and harness tags are aterm's own"
    );
    assert!(
        status(&inst, "messages 0").starts_with("ERR usage: messages "),
        "a zero count"
    );

    // 8. `appstatus` is aterm's own initiative: no script row, ever.
    let (st, rows) = ctl(&inst, "appstatus");
    assert!(st.starts_with("OK "), "appstatus: {st}");
    for row in &rows {
        assert!(
            !row.contains("wire.") && !row.contains("Building") && !row.contains("hello"),
            "a script's row in appstatus: {row}"
        );
    }

    // 9. `help` carries both rows.
    let (st, rows) = ctl(&inst, "help");
    let count: usize = st
        .strip_prefix("OK ")
        .and_then(|t| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("help: {st}"));
    assert!(count >= 112 && count == rows.len(), "help: {st}");
    for (verb, summary) in [
        ("messages", "the message log, one row each"),
        (
            "notice",
            "record, show work in flight, end or press a message",
        ),
    ] {
        assert!(
            rows.iter()
                .any(|r| r.split_whitespace().next() == Some(verb) && r.contains(summary)),
            "no `{verb}` row in help"
        );
    }
}
