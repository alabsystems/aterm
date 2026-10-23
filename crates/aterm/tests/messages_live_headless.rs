// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the `messages` and `notice` control-socket verbs.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::headless_boot;

use headless_boot::Instance;

fn boot(tag: &str) -> Instance {
    headless_boot::boot_with(
        &format!("atms{tag}"),
        |_, cmd| {
            cmd.args(["--lines", "40", "--columns", "120"]);
        },
        |inst| token_path(inst).is_file(),
    )
}

const REPLY_DEADLINE: Duration = Duration::from_secs(60);

fn token_path(inst: &Instance) -> PathBuf {
    let mut p = std::ffi::OsString::from(&inst.sock);
    p.push(".token");
    PathBuf::from(p)
}

fn token(inst: &Instance) -> String {
    std::fs::read_to_string(token_path(inst))
        .expect("the owner token file")
        .trim()
        .to_string()
}

fn is_lines_verb(line: &str) -> bool {
    matches!(
        line.split_whitespace().next(),
        Some("messages" | "appstatus")
    )
}

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

fn status(inst: &Instance, line: &str) -> String {
    ctl(inst, line).0
}

fn field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

fn row_id(row: &str) -> u64 {
    row.strip_prefix("message ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("not a message row: {row}"))
}

fn minted(reply: &str) -> u64 {
    reply
        .rsplit_once("message=")
        .and_then(|(_, id)| id.parse().ok())
        .unwrap_or_else(|| panic!("no minted id in {reply:?}"))
}

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

fn live_row(inst: &Instance, id: u64) -> String {
    let (st, rows) = ctl(inst, "messages live");
    assert!(st.starts_with("OK "), "messages live: {st}");
    rows.into_iter()
        .find(|r| row_id(r) == id)
        .unwrap_or_else(|| panic!("message {id} is not live"))
}

#[test]
fn the_message_verbs_round_trip_on_a_live_instance() {
    let inst = boot("m");

    let (st, rows) = ctl(&inst, "messages");
    assert_eq!(st, format!("OK {}", rows.len()), "{st}");
    let b = rows.last().map_or(0, |r| row_id(r));

    let a = status(&inst, "notice post script hello from a script");
    assert!(a.starts_with("OK recorded message="), "{a}");
    let a = minted(&a);
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

    let w = status(
        &inst,
        "notice post script sev=warn key=deploy Deploy failed -- connection refused",
    );
    assert!(w.starts_with("OK message="), "{w}");
    let w = minted(&w);
    let row = live_row(&inst, w);
    assert_eq!(field(&row, "state"), Some("held"), "{row}");
    assert_eq!(field(&row, "key"), Some("wire.deploy"), "{row}");
    assert_eq!(field(&row, "detail"), Some("connection%20refused"), "{row}");

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

    assert_eq!(
        status(&inst, &format!("notice dismiss {w}")),
        format!("OK dismissed={w}")
    );
    assert_eq!(
        status(&inst, &format!("notice act {w} 0")),
        format!("ERR notice: no live message {w}")
    );

    assert!(
        status(&inst, "notice progress").starts_with("ERR usage: notice progress "),
        "a bare progress"
    );
    assert!(
        status(
            &inst,
            "notice post script sev=warn one two three four five six seven"
        )
        .starts_with("ERR notice: "),
        "a title over 6 words"
    );
    assert!(
        status(&inst, "notice post update sev=warn Nope").starts_with("ERR notice: "),
        "a tag that is aterm's own"
    );
    assert!(
        status(&inst, "messages 0").starts_with("ERR usage: messages "),
        "a zero count"
    );

    let (st, rows) = ctl(&inst, "appstatus");
    assert!(st.starts_with("OK "), "appstatus: {st}");
    for row in &rows {
        assert!(
            !row.contains("wire.") && !row.contains("Building") && !row.contains("hello"),
            "a script's row in appstatus: {row}"
        );
    }
}
