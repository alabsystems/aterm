// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live headless `aterm ctl survey` and `status needs=` tests: survey output across plain text,
//! `--sid`, and `--json` (including bidi-override stripping), plus wire-published needs for keyed
//! attention, stalled tasks, and stopped agents with queued input.

#![cfg(unix)]

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use crate::headless_boot;

use headless_boot::{Instance, boot_session, ctl, ctl_ok, type_line};

fn boot() -> Instance {
    headless_boot::boot_with(
        "asrv",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "100"]);
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

fn survey(inst: &Instance, args: &[&str]) -> String {
    let mut all = vec!["survey"];
    all.extend_from_slice(args);
    ctl_ok(inst, &all)
}

fn sid_field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("  {key}: ");
    record.lines().find_map(|l| l.strip_prefix(prefix.as_str()))
}

#[test]
fn the_survey_reads_every_session_of_an_instance() {
    let inst = boot();
    let (_, a) = boot_session(&inst);
    let b = spawn(&inst);
    type_line(&inst, &b, "echo survey-marker; sleep 600");
    type_line(&inst, &a, r"printf '\033]2;build \342\200\256gnp.exe\007'");

    let record = until_within(MUST_HAPPEN, "b running `sleep`, its rows read", || {
        let record = survey(&inst, &["--sid", &b]);
        (sid_field(&record, "program") == Some("sleep")
            && record.lines().any(|l| l == "  rows: survey-marker"))
        .then_some(record)
    });
    assert_eq!(sid_field(&record, "agent"), Some("-"), "{record}");
    assert_eq!(sid_field(&record, "state"), Some("alive"), "{record}");
    assert!(
        record
            .lines()
            .nth(2)
            .unwrap()
            .contains(&format!("{b} running sleep")),
        "{record}"
    );

    let text = until_within(MUST_HAPPEN, "a's title in the survey", || {
        let text = survey(&inst, &[]);
        text.contains("build ").then_some(text)
    });
    let (data, rest) = text.split_once('\n').expect("a first line");
    assert!(
        data.starts_with("# ") && data.contains("trust=screen"),
        "{text}"
    );
    let lines: Vec<&str> = rest.lines().collect();
    assert_eq!(lines.len(), 3, "a header and two sessions: {text}");
    assert!(lines[0].starts_with("== "), "{text}");
    assert!(lines[0].contains(" · 2 sessions"), "{text}");
    assert!(lines[0].ends_with(" trust=screen"), "{text}");
    assert!(lines[0].chars().count() <= 100, "{text}");
    for line in &lines[1..] {
        assert!(line.chars().count() <= 110, "{line}");
    }
    assert!(
        lines[1].starts_with(&format!("{b} running sleep")),
        "{text}"
    );
    assert!(lines[2].starts_with(&format!("{a} ")), "{text}");
    assert!(lines[2].contains(" idle typed by "), "{text}");
    assert!(lines[2].contains(" ago | build "), "{text}");
    assert!(
        !text.contains('\u{202e}'),
        "the override reached the reader: {text:?}"
    );

    let json = survey(&inst, &["--json"]);
    assert!(json.starts_with("[{\"label\":\"pid "), "{json}");
    for sid in [&a, &b] {
        assert!(json.contains(&format!("\"sid\":\"{sid}\"")), "{json}");
    }
    assert!(json.contains("\"level\":\"content\""), "{json}");

    let bad = ctl(&inst, &["survey", "--everything"]);
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("ERR usage: survey"),
        "{bad:?}"
    );
}

fn wire_field(line: &str, key: &str) -> Option<String> {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

fn json_needs(json: &str, sid: &str) -> Option<String> {
    let at = json.find(&format!("\"sid\":\"{sid}\""))?;
    let rest = &json[at..];
    let start = rest.find("\"needs\":\"")? + "\"needs\":\"".len();
    let end = rest[start..].find('"')?;
    Some(rest[start..start + end].to_string())
}

#[test]
fn a_kind_ask_attention_is_a_need_every_reader_names_and_a_wait_is_none() {
    let inst = headless_boot::boot_with(
        "asnd",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "100"]);
        },
        |_| true,
    );
    let (_, waiter) = boot_session(&inst);
    let asker = spawn(&inst);
    let at = |sid: &str| format!("@{sid}");
    let meta = |sid: &str, rest: &[&str]| {
        let mut all = vec![at(sid), "meta".to_string()];
        all.extend(rest.iter().map(|s| (*s).to_string()));
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        ctl_ok(&inst, &all)
    };
    let needs = |sid: &str| wire_field(&ctl_ok(&inst, &[&at(sid), "status"]), "needs");
    let until_needs = |sid: &str, word: &str| {
        until_within(MUST_HAPPEN, &format!("{sid} needs={word}"), || {
            (needs(sid).as_deref() == Some(word)).then_some(())
        });
    };
    meta(
        &asker,
        &[
            "set",
            "attention",
            "owner=ci",
            "kind=ask",
            "look at the build",
        ],
    );
    meta(&waiter, &["set", "attention", "lunch"]);
    until_needs(&asker, "ask");
    until_needs(&waiter, "ask");
    meta(
        &waiter,
        &[
            "set",
            "attention",
            "owner=supervisor",
            "kind=wait",
            "limited: resets 7pm",
        ],
    );
    assert_eq!(
        needs(&waiter).as_deref(),
        Some("ask"),
        "the older ask stands"
    );
    let record = survey(&inst, &["--sid", &waiter]);
    assert_eq!(sid_field(&record, "attention"), Some("lunch"), "{record}");
    meta(&waiter, &["unset", "attention"]);
    until_needs(&waiter, "-");
    let read = meta(&waiter, &[]);
    assert_eq!(
        wire_field(&read, "attention_kinds").as_deref(),
        Some("wait:supervisor"),
        "{read}"
    );
    let json = survey(&inst, &["--json"]);
    assert_eq!(json_needs(&json, &asker).as_deref(), Some("ask"), "{json}");
    assert_eq!(json_needs(&json, &waiter).as_deref(), Some("-"), "{json}");
    let chrome = ctl_ok(&inst, &["chrome"]);
    let line = chrome
        .lines()
        .find(|l| l.starts_with("statusitem "))
        .unwrap_or_else(|| panic!("a statusitem line: {chrome}"));
    assert_eq!(wire_field(line, "needs").as_deref(), Some("1"), "{line}");
    assert_eq!(wire_field(line, "stuck").as_deref(), Some("0"), "{line}");
}

const FAKE_LEADER: &str = r#"#!/bin/sh
echo $$ > "$1"
printf 'fake agent up\n'
while [ ! -e "$2" ]; do sleep 0.1; done
while IFS= read -r line; do printf 'read %s\n' "$line"; done
"#;

struct KillOnDrop(i32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // SAFETY: `kill(2)` on the fake's own pid, read from the file it wrote.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

#[test]
fn a_stalled_task_and_a_stopped_agent_are_needs_on_the_wire() {
    let inst = headless_boot::boot_with(
        "asns",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "100"]);
        },
        |_| true,
    );
    let (_, agent) = boot_session(&inst);
    let worker = spawn(&inst);
    let at = |sid: &str| format!("@{sid}");
    let status = |sid: &str| ctl_ok(&inst, &[&at(sid), "status"]);
    let until_needs = |sid: &str, word: &str| {
        until_within(MUST_HAPPEN, &format!("{sid} needs={word}"), || {
            let record = status(sid);
            (wire_field(&record, "needs").as_deref() == Some(word)).then_some(record)
        })
    };

    assert_eq!(wire_field(&status(&worker), "needs").as_deref(), Some("-"));
    let marked = ctl_ok(
        &inst,
        &[&at(&worker), "task", "mark", "stalled", "why=held"],
    );
    assert!(marked.contains("changed=1"), "{marked}");
    until_needs(&worker, "stalled");
    ctl_ok(
        &inst,
        &[&at(&worker), "task", "mark", "working", "why=moved"],
    );
    until_needs(&worker, "-");

    if !cfg!(target_os = "macos") {
        return;
    }

    let script = inst.tmp.join("fake-leader.sh");
    let pid_file = inst.tmp.join("fake-leader.pid");
    let go = inst.tmp.join("fake-leader.go");
    std::fs::write(&script, FAKE_LEADER).expect("write the fake");
    ctl_ok(
        &inst,
        &[&at(&agent), "await", "match", r"[$#]\s*$", "timeout=60000"],
    );
    type_line(
        &inst,
        &agent,
        &format!(
            "exec /bin/bash -c 'exec -a claude /bin/sh {} {} {}'",
            script.display(),
            pid_file.display(),
            go.display()
        ),
    );
    let pid: i32 = until_within(MUST_HAPPEN, "the fake's pid", || {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
    });
    let _reaper = KillOnDrop(pid);
    until_within(MUST_HAPPEN, "program=claude, read as an agent", || {
        let record = status(&agent);
        (wire_field(&record, "program").as_deref() == Some("claude")
            && wire_field(&record, "agent").is_some_and(|word| word != "-"))
        .then_some(())
    });
    // SAFETY: `kill(2)` on the fake's own pid, read from the file it wrote.
    assert_eq!(
        unsafe { libc::kill(pid, libc::SIGSTOP) },
        0,
        "stop the fake"
    );
    assert_eq!(
        wire_field(&status(&agent), "needs").as_deref(),
        Some("-"),
        "stopped with nothing queued is no stall"
    );
    ctl_ok(&inst, &[&at(&agent), "feed", "780a"]);
    let stopped = until_needs(&agent, "stopped");
    assert_eq!(
        wire_field(&stopped, "agent").as_deref(),
        Some("wall:unresponsive"),
        "{stopped}"
    );
    assert_eq!(
        wire_field(&stopped, "agent_detail").as_deref(),
        Some("stopped"),
        "{stopped}"
    );
    std::fs::write(&go, b"").expect("create the go file");
    ctl_ok(&inst, &[&at(&agent), "signal", "cont"]);
    until_needs(&agent, "-");
}
