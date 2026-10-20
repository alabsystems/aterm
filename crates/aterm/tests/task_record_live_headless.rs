// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TASK RECORD THROUGH THE ONE `aterm` BINARY against a real `aterm
//! --headless` (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.2): a
//! worker's record is marked and read over the socket, rides `status` and
//! `sessions`, and `aterm ctl await task` — the manager's one wait — answers
//! over two workers with `all` once both are done, and times out with exit
//! 124 when the news it would answer was already read.
//!
//! Every wait that must end is a one-minute hang detector; the one short
//! `timeout=` is the timeout under test, never a latency budget. The wait over
//! both workers names each worker's rev (`@<sid>:1`), so the marks may land
//! before or after the client arms and the answer is the same.
//!
//! ISOLATION: `support/launch_isolation.rs` and `support/headless_boot.rs` (a
//! product that cannot start fails; a scratch/spawn refusal is COULD NOT RUN,
//! `boot_with`, never a pass).

#![cfg(unix)]

use std::time::Instant;

use crate::{child_output, headless_boot};

use headless_boot::{Instance, boot_session, client_command, ctl, ctl_ok, field, status};

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG_MS: &str = "timeout=60000";

/// The headless instance at 24x80 under `prefix`'s scratch world (each test
/// boots under its own), or COULD NOT RUN.
fn boot(prefix: &str) -> Instance {
    headless_boot::boot_with(
        prefix,
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    )
}

/// The sid a `spawn` minted.
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
    // A mark that changes nothing is no change.
    let again = ctl_ok(&inst, &[&format!("@{a}"), "task", "mark", "working"]);
    assert!(
        again.contains(" rev=1 ") && again.trim_end().ends_with(" changed=0"),
        "{again}"
    );
    // An unknown state is the usage line, and exit 1.
    let bad = ctl(&inst, &[&format!("@{a}"), "task", "mark", "finished"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("ERR usage: task"));

    // The manager's wait over both workers, armed past each one's rev 1.
    let list = format!("@{a}:1,@{b}:1");
    let waiter = client_command(&inst, &["await", "task", &list, "all", HANG_MS])
        .spawn()
        .expect("spawn the waiting client");
    ctl_ok(
        &inst,
        &[
            &format!("@{a}"),
            "task",
            "mark",
            "done",
            "why=marker",
            "result_bytes=11",
        ],
    );
    ctl_ok(
        &inst,
        &[&format!("@{b}"), "task", "mark", "done", "why=done-check"],
    );
    let out =
        child_output::wait_and_read(waiter, Instant::now() + headless_boot::CLIENT_EXIT_DEADLINE)
            .expect("the waiting client exits");
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

    // Re-armed with no since, the news is already read: the deadline answers,
    // the row says where the worker stands, and the client exits 124.
    let timed = ctl(&inst, &["await", "task", &format!("@{a}"), "timeout=300"]);
    assert_eq!(timed.status.code(), Some(124), "{timed:?}");
    assert!(String::from_utf8_lossy(&timed.stderr).contains("OK timeout"));
    assert!(
        String::from_utf8_lossy(&timed.stdout).starts_with(&format!("@{a} rev=2 state=done ")),
        "{timed:?}"
    );

    // Published: `status` and the `sessions` row carry the record.
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

/// The instance's state root, as `aterm_types::dirs::state_dir` resolves it
/// under the scratch world's `HOME` and `XDG_STATE_HOME`.
fn state_root(inst: &Instance) -> std::path::PathBuf {
    if cfg!(target_os = "macos") {
        inst.tmp.join("home/Library/Application Support/aterm")
    } else {
        inst.tmp.join("state/aterm")
    }
}

/// The control wire's percent escape (`aterm_codec::pct::encode`): every byte
/// that is not ASCII-graphic, and `%`, as `%XX`.
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

/// THE WORKER'S RESULT, AND A TURN THAT OPENS IT AGAIN, through the one
/// binary (design §2.3, §2.2): `result` answers `ERR no task`, then its
/// task's state's word, until the record names a snapshot under the
/// instance's `<state>/tasks/` — the files the window harness writes at the
/// done edge — and then `aterm ctl` prints the stamp on stderr and the
/// worker's text alone on stdout. A `turn` typed into the done worker from
/// `aterm ctl` moves its record to `working why=turn` as it lands.
#[test]
fn a_workers_result_is_read_and_a_turn_opens_it_again() {
    // Its own scratch world (`<prefix>-<pid>`): never the other test's.
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
            "why=marker",
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

    // A follow-up typed into the done worker.
    ctl_ok(&inst, &[&sel, "turn", "timeout=60000", "true"]);
    let record = ctl_ok(&inst, &[&sel, "task"]);
    assert_eq!(field(&record, "state"), Some("working"), "{record}");
    assert_eq!(field(&record, "why"), Some("turn"), "{record}");
    assert!(refused(&[&sel, "result"]).contains("ERR result unavailable why=working"));
}
