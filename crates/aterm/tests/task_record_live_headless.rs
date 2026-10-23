// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TASK RECORD THROUGH THE ONE `aterm` BINARY against a real `aterm --headless`.

#![cfg(unix)]

use std::time::Instant;

use crate::{child_output, headless_boot};

use headless_boot::{Instance, boot_session, client_command, ctl, ctl_ok, field, status};

const HANG_MS: &str = "timeout=60000";

fn boot(prefix: &str) -> Instance {
    headless_boot::boot_with(
        prefix,
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    )
}

fn spawn(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["spawn"]);
    body.split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {body}"))
        .to_string()
}

#[test]
fn a_workers_record_is_marked_read_published_and_awaited() {
    let inst = boot("attr");
    let (_, a) = boot_session(&inst);
    let b = spawn(&inst);

    assert_eq!(
        ctl_ok(&inst, &[&format!("@{a}"), "task"]).trim(),
        "OK task -"
    );
    let marked = ctl_ok(
        &inst,
        &[&format!("@{a}"), "task", "mark", "working", "from=ctl"],
    );
    assert!(
        marked.starts_with("OK task rev=1 state=working from=ctl "),
        "{marked}"
    );
    assert!(marked.trim_end().ends_with(" changed=1"), "{marked}");
    ctl_ok(&inst, &[&format!("@{b}"), "task", "mark", "working"]);
    let again = ctl_ok(&inst, &[&format!("@{a}"), "task", "mark", "working"]);
    assert!(
        again.contains(" rev=1 ") && again.trim_end().ends_with(" changed=0"),
        "{again}"
    );
    let bad = ctl(&inst, &[&format!("@{a}"), "task", "mark", "finished"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("ERR usage: task"));

    let list = format!("@{a}:1,@{b}:1");
    let mut waiting = client_command(&inst, &["await", "task", &list, "all", HANG_MS]);
    let waiter = waiting.spawn().expect("spawn the waiting client");
    ctl_ok(
        &inst,
        &[
            &format!("@{a}"),
            "task",
            "mark",
            "done",
            "why=landed",
            "result_bytes=11",
        ],
    );
    ctl_ok(
        &inst,
        &[&format!("@{b}"), "task", "mark", "done", "why=done-check"],
    );
    let mut first = Some(waiter);
    let out = headless_boot::until_served(|| {
        let child = first
            .take()
            .unwrap_or_else(|| waiting.spawn().expect("spawn the waiting client"));
        child_output::wait_and_read(child, Instant::now() + headless_boot::CLIENT_EXIT_DEADLINE)
            .expect("the waiting client exits")
    });
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "await task: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<&str> = stdout.lines().collect();
    assert_eq!(rows.len(), 2, "{stdout}");
    for (row, sid, bytes) in [(rows[0], &a, "11"), (rows[1], &b, "0")] {
        assert!(
            row.starts_with(&format!("@{sid} rev=2 state=done ")),
            "{stdout}"
        );
        assert_eq!(field(row, "result_bytes"), Some(bytes), "{row}");
        assert!(
            field(row, "agent").is_some() && field(row, "attention").is_some(),
            "{row}"
        );
    }

    let timed = ctl(&inst, &["await", "task", &format!("@{a}"), "timeout=300"]);
    assert_eq!(timed.status.code(), Some(124), "{timed:?}");
    assert!(String::from_utf8_lossy(&timed.stderr).contains("OK timeout"));
    assert!(
        String::from_utf8_lossy(&timed.stdout).starts_with(&format!("@{a} rev=2 state=done ")),
        "{timed:?}"
    );

    let record = status(&inst, &a);
    assert_eq!(field(&record, "task_state"), Some("done"), "{record}");
    assert_eq!(field(&record, "task_rev"), Some("2"), "{record}");
    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(a.as_str()))
        .unwrap_or_else(|| panic!("a's row: {roster}"));
    assert!(row.ends_with(" task_state=done task_rev=2"), "{row}");
}

fn state_root(inst: &Instance) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        inst.tmp.join("home/Library/Application Support/aterm")
    } else {
        inst.tmp.join("state/aterm")
    }
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_graphic() && b != b'%' {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[test]
fn a_workers_result_is_read_and_a_turn_opens_it_again() {
    let inst = boot("atres");
    let (_, a) = boot_session(&inst);
    let sel = format!("@{a}");
    let refused = |args: &[&str]| {
        let out = ctl(&inst, args);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(refused(&[&sel, "result"]).contains("ERR no task"));
    ctl_ok(&inst, &[&sel, "task", "mark", "working"]);
    assert!(refused(&[&sel, "result"]).contains("ERR result unavailable why=working"));

    let tasks = state_root(&inst).join("tasks");
    std::fs::create_dir_all(&tasks).expect("tasks");
    let md = tasks.join(format!("{a}-1.result.md"));
    std::fs::write(&md, "Fixed the parser.\nAll green.").expect("result");
    std::fs::write(
        md.with_extension("src"),
        "source=transcript session=0b2299fa uuid=a-1\n",
    )
    .expect("sidecar");
    let path = format!("result_path={}", pct(&md.to_string_lossy()));
    ctl_ok(
        &inst,
        &[
            &sel,
            "task",
            "mark",
            "done",
            "why=landed",
            "result_bytes=28",
            &path,
        ],
    );
    let out = ctl(&inst, &[&sel, "result"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Fixed the parser.\nAll green.\n"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("OK 2 source=transcript session=0b2299fa uuid=a-1 bytes=28"),
        "{out:?}"
    );

    ctl_ok(&inst, &[&sel, "turn", "timeout=60000", "true"]);
    let record = ctl_ok(&inst, &[&sel, "task"]);
    assert_eq!(field(&record, "state"), Some("working"), "{record}");
    assert_eq!(field(&record, "why"), Some("turn"), "{record}");
    assert!(refused(&[&sel, "result"]).contains("ERR result unavailable why=working"));
}
