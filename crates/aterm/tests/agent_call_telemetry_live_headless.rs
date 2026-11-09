// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A headless instance's `kind=agent-call` rows say how each call ended, read off its leader's
//! exit by a kernel watch though aterm is not the leader's parent, and name the program the call
//! ran, never the agent's shell wrapper. A stand-in agent in the boot tab starts calls as Claude
//! Code does: `zsh -c <command>`, each in a new session with no terminal, reaped by the stand-in.
//! Each program waits on a pipe until the instance has observed its call and installed its
//! exit watch. A sampler has no guarantee of observing a program that lives only four seconds.

#![cfg(target_os = "macos")]

use std::io::Write as _;
use std::time::{Duration, Instant};

use crate::headless_boot::{self, Instance};
use crate::standin;

/// This file's stand-in entry: run as the agent, with the calls [`standin::spec`] names.
const ENTRY: &str = "agent_call_telemetry_live_headless::agent_standin";
const ROLE: &str = "agent-calls";
const CALLS_FILE: &str = "calls.json";

/// The four ends of the package's measurement, each a call of its own, and what its row says:
/// `(command, label, outcome, code, sig)`. Each end of its own runs a program of its own, so a
/// row that took another call's status fails by its label.
const ENDS: [(&str, &str, &str, &str, &str); 4] = [
    ("sleep 4; true", "sleep", "ok", "0", "-"),
    ("caffeinate -t 4; exit 3", "caffeinate", "fail", "3", "-"),
    // The wrapper's own child is the label: `time`, which runs `sleep`.
    (
        "/usr/bin/time sleep 4; kill -TERM $$",
        "time",
        "fail",
        "-",
        "15",
    ),
    // zsh execs a lone command: the leader is `sleep` itself.
    ("sleep 4", "sleep", "ok", "0", "-"),
];

/// Each call's end as the stand-in reaped it, from its `status <i> <code|-> <sig|->` lines.
fn reaped(screen: &str) -> std::collections::BTreeMap<usize, (String, String)> {
    screen
        .lines()
        .filter_map(|l| {
            let mut w = l.trim().strip_prefix("status ")?.split(' ');
            let i = w.next()?.parse().ok()?;
            Some((i, (w.next()?.to_owned(), w.next()?.to_owned())))
        })
        .collect()
}

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// The stand-in agent: read the calls and their start offsets from its gate's JSON file,
/// each `zsh -c` in a session of its own with no terminal; write a dot
/// every quarter second while any runs (the output a windowless instance's passes follow), then
/// one `status <i> <code|-> <sig|->` line per call as the stand-in reaped it, and `standin-done`.
/// Closing their input pipes releases the programs only after the observer's `release` marker.
#[test]
fn agent_standin() {
    if standin::spec().as_deref() != Some(ROLE) {
        return;
    }
    let gate = std::path::PathBuf::from(std::env::var_os("CALL_GATE").expect("call gate"));
    let calls: Vec<(u64, String)> = aterm_json::from_str(
        &std::fs::read_to_string(gate.join(CALLS_FILE)).expect("call specification"),
    )
    .expect("call offsets and commands");
    standin::put(b"standin-ready\n");
    let deadline = Instant::now() + HANG;
    while !gate.join("start").exists() {
        assert!(Instant::now() < deadline, "calls never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let t0 = Instant::now();
    let mut queue: Vec<(Duration, String)> = calls
        .into_iter()
        .map(|(ms, cmd)| (Duration::from_millis(ms), cmd))
        .collect();
    queue.reverse();
    let mut running = Vec::new();
    let mut done: Vec<Option<String>> = Vec::new();
    let mut spawned = false;
    loop {
        assert!(t0.elapsed() < HANG, "calls were never released and reaped");
        while queue.last().is_some_and(|(at, _)| t0.elapsed() >= *at) {
            let (_, cmd) = queue.pop().expect("a call");
            // Signed cat stand-ins keep each original program's kernel name, but wait for
            // input instead of expiring on a wall clock before a loaded sampler can see them.
            let cmd = cmd
                .replace("sleep 4", &format!("'{}'", gate.join("sleep").display()))
                .replace(
                    "caffeinate -t 4",
                    &format!("'{}'", gate.join("caffeinate").display()),
                );
            let mut child = aterm_uds::spawnfd::command("/bin/zsh")
                .args(["-c", &cmd])
                .setsid(true)
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .spawn()
                .unwrap_or_else(|e| {
                    standin::put(format!("spawn failed: {e}\n").as_bytes());
                    standin::exit(1)
                });
            child
                .stdin
                .as_mut()
                .expect("call input")
                .write_all(b"call-proof\n")
                .expect("prove copied program ran");
            aterm_uds::deadline::prints(&mut child, "call-proof", HANG);
            running.push((done.len(), child));
            done.push(None);
        }
        if queue.is_empty() && !spawned {
            std::fs::write(gate.join("spawned"), b"").expect("spawned marker");
            spawned = true;
        }
        if gate.join("release").exists() {
            for (_, child) in &mut running {
                drop(child.stdin.take());
            }
        }
        running.retain_mut(|(i, child)| match child.try_wait() {
            Ok(Some(st)) => {
                use std::os::unix::process::ExitStatusExt as _;
                let word = |n: Option<i32>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
                done[*i] = Some(format!(
                    "status {i} {} {}",
                    word(st.code()),
                    word(st.signal())
                ));
                false
            }
            Ok(None) => true,
            Err(e) => {
                standin::put(format!("wait failed: {e}\n").as_bytes());
                standin::exit(1)
            }
        });
        if queue.is_empty() && running.is_empty() {
            break;
        }
        standin::put(b".");
        std::thread::sleep(Duration::from_millis(250));
    }
    standin::put(b"\n");
    for line in done.into_iter().flatten() {
        standin::put(format!("{line}\n").as_bytes());
    }
    std::fs::write(gate.join("done"), b"").expect("done marker");
    standin::put(b"\nstandin-done\n");
    let deadline = Instant::now() + HANG;
    while !gate.join("finish").exists() {
        assert!(Instant::now() < deadline, "stand-in never finished");
        standin::put(b".");
        std::thread::sleep(Duration::from_millis(250));
    }
    standin::exit(0)
}

/// The instance's `kind=agent-call` rows, polled until `n` are written.
fn call_rows(inst: &Instance, n: usize) -> Vec<String> {
    let deadline = Instant::now() + HANG;
    loop {
        let body = headless_boot::ctl_ok(
            inst,
            &[
                "telemetry",
                "rows",
                "--kind",
                "agent-call",
                "--limit",
                "500",
            ],
        );
        let rows: Vec<String> = body
            .lines()
            .filter(|l| l.contains("kind=agent-call"))
            .map(str::to_owned)
            .collect();
        if rows.len() >= n {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "{} of {n} agent-call rows within {HANG:?}:\n{body}\n{}",
            rows.len(),
            headless_boot::log_tail(&inst.log)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn word<'a>(row: &'a str, key: &str) -> &'a str {
    headless_boot::field(row, key).unwrap_or_else(|| panic!("no {key}= in {row}"))
}

/// Run `calls` under the stand-in agent in a fresh instance's boot tab: the rows the instance
/// wrote for them, and the stand-in's own reaped statuses.
fn run(prefix: &str, calls: &[(u64, &str)]) -> (Vec<String>, String) {
    let inst = headless_boot::boot(prefix, &[]);
    let (_, sid) = headless_boot::boot_session(&inst);
    start(&inst, &sid, calls);
    let deadline = Instant::now() + HANG;
    loop {
        let jobs = headless_boot::ctl_ok(&inst, &["sys", "jobs", "top=64"]);
        let live: Vec<_> = jobs
            .lines()
            .filter(|r| r.contains("kind=agent-call"))
            .collect();
        let telemetry = headless_boot::ctl_ok(&inst, &["telemetry"]);
        if live.len() == calls.len()
            && live
                .iter()
                .all(|r| matches!(headless_boot::field(r, "exe"), Some("sleep" | "caffeinate")))
            && telemetry.lines().any(|r| {
                headless_boot::field(r, "watched") == Some(calls.len().to_string().as_str())
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "calls were not observed:\n{jobs}\n{telemetry}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(inst.tmp.join("release"), b"").expect("release calls after observation");
    std::fs::write(inst.tmp.join("finish"), b"").expect("finish stand-in");
    let done = headless_boot::ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "standin-done",
            "timeout=120000",
        ],
    );
    let screen = headless_boot::ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    let rows = call_rows(&inst, calls.len());
    let summary = headless_boot::ctl_ok(&inst, &["telemetry"]);
    eprintln!("{done}\n{summary}\n{}", rows.join("\n"));
    (rows, screen)
}

fn wait_file(path: &std::path::Path) {
    let deadline = Instant::now() + HANG;
    while !path.exists() {
        assert!(Instant::now() < deadline, "no {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Resume the owned instance even if a marker wait panics.
struct Resume(u32);

impl Drop for Resume {
    fn drop(&mut self) {
        // SAFETY: this guard lives inside start, while the owned instance still lives.
        let _ = unsafe { libc::kill(self.0 as i32, libc::SIGCONT) };
    }
}

/// Start behind a gate so even a reader delayed until every call was spawned must observe them.
fn start(inst: &Instance, sid: &str, calls: &[(u64, &str)]) {
    // Session publication precedes the shell's raw mode. Keep the submitted command
    // below the canonical input limit, regardless of how many calls its data describes.
    std::fs::write(
        inst.tmp.join(CALLS_FILE),
        aterm_json::to_string(calls).expect("serialize calls"),
    )
    .expect("write call specification");
    for name in ["sleep", "caffeinate"] {
        aterm_uds::deadline::ad_hoc_copy(std::path::Path::new("/bin/cat"), &inst.tmp.join(name));
    }
    headless_boot::type_line(
        inst,
        sid,
        &format!(
            "env CALL_GATE='{}' {}",
            inst.tmp.display(),
            standin::shell_line(ENTRY, ROLE)
        ),
    );
    headless_boot::ctl_ok(
        inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "standin-ready",
            "timeout=60000",
        ],
    );
    // Pause the reader, not its descendants. No call exists at this point.
    // SAFETY: the pid is this test's live, owned instance.
    assert_eq!(
        unsafe { libc::kill(inst.child.id() as i32, libc::SIGSTOP) },
        0
    );
    let resume = Resume(inst.child.id());
    let deadline = Instant::now() + HANG;
    loop {
        let mut status = 0;
        // SAFETY: the owned child is live and status points to one writable int. Reap only
        // its stop notification; its exit remains for Instance::drop.
        let got = unsafe {
            libc::waitpid(
                inst.child.id() as i32,
                &raw mut status,
                libc::WUNTRACED | libc::WNOHANG,
            )
        };
        if got == inst.child.id() as i32 && libc::WIFSTOPPED(status) {
            break;
        }
        assert!(
            got >= 0 && Instant::now() < deadline,
            "instance did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(inst.tmp.join("start"), b"").expect("start calls");
    wait_file(&inst.tmp.join("spawned"));
    if inst.tmp.join("release").exists() {
        wait_file(&inst.tmp.join("done"));
    }
    drop(resume);
}

/// Negative control: calls reaped before the first process reading leave zero rows and files.
/// A completed stand-in alone never proved that a sampling telemetry source saw its calls.
#[test]
fn calls_ended_before_the_first_reading_leave_no_rows() {
    let inst = headless_boot::boot("atctlate", &[]);
    let (_, sid) = headless_boot::boot_session(&inst);
    std::fs::write(inst.tmp.join("release"), b"").expect("release before observation");
    let calls: Vec<_> = (0..50).map(|i| (0, ENDS[i % ENDS.len()].0)).collect();
    start(&inst, &sid, &calls);
    let jobs = headless_boot::ctl_ok(&inst, &["sys", "jobs"]);
    let telemetry = headless_boot::ctl_ok(&inst, &["telemetry"]);
    assert!(!jobs.contains("kind=agent-call"), "{jobs}");
    assert_eq!(
        headless_boot::field(&telemetry, "rows"),
        Some("0"),
        "{telemetry}"
    );
    assert_eq!(
        headless_boot::field(&telemetry, "files"),
        Some("0"),
        "{telemetry}"
    );
    eprintln!("delayed first reading: {telemetry}");
    std::fs::write(inst.tmp.join("finish"), b"").expect("finish stand-in");
}

#[test]
fn each_calls_row_says_how_its_leader_ended_and_what_it_ran() {
    let calls: Vec<(u64, &str)> = ENDS.iter().map(|(cmd, ..)| (0, *cmd)).collect();
    let (rows, screen) = run("atct4", &calls);
    assert_eq!(rows.len(), ENDS.len(), "one row per call: {rows:#?}");
    // The ground truth: the stand-in, the calls' parent, reaped each as the table says.
    let reaped = reaped(&screen);
    assert_eq!(
        reaped.len(),
        ENDS.len(),
        "the stand-in's statuses: {screen}"
    );
    for (i, (_, label, _, code, sig)) in ENDS.iter().enumerate() {
        assert_eq!(
            reaped.get(&i),
            Some(&((*code).to_owned(), (*sig).to_owned())),
            "call {i} ({label}) as its parent reaped it: {screen}"
        );
    }
    let mut seen: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            assert_ne!(word(r, "stop"), "unknown", "{r}");
            ["label", "outcome", "code", "sig"].map(|k| word(r, k).to_owned())
        })
        .collect();
    seen.sort();
    let mut want: Vec<[String; 4]> = ENDS
        .iter()
        .map(|(_, l, o, c, s)| [*l, *o, *c, *s].map(str::to_owned))
        .collect();
    want.sort();
    assert_eq!(
        seen, want,
        "each call's row says its own program and end, never the wrapper's: rows:\n{rows:#?}\nscreen:\n{screen}"
    );
}

#[test]
fn fifty_mixed_calls_end_known_and_none_wrongly() {
    let mut calls = Vec::new();
    for i in 0..50u64 {
        let (cmd, ..) = ENDS[usize::try_from(i).expect("small") % ENDS.len()];
        calls.push((i * 100, cmd));
    }
    let (rows, screen) = run("atct50", &calls);
    let mut want: std::collections::BTreeMap<(&str, &str, &str, &str), usize> = Default::default();
    for (i, _) in calls.iter().enumerate() {
        let (_, l, o, c, s) = ENDS[i % ENDS.len()];
        *want.entry((l, o, c, s)).or_default() += 1;
    }
    let mut got: std::collections::BTreeMap<(&str, &str, &str, &str), usize> = Default::default();
    let mut known = 0;
    for r in &rows {
        let label = word(r, "label");
        assert!(
            ENDS.iter().any(|(_, l, ..)| *l == label),
            "the program, never the wrapper: {r}"
        );
        let end = (label, word(r, "outcome"), word(r, "code"), word(r, "sig"));
        if end.1 != "unknown" {
            known += 1;
            *got.entry(end).or_default() += 1;
        }
    }
    for (end, n) in &got {
        assert!(
            want.get(end).is_some_and(|w| n <= w),
            "{n} rows end {end:?}, more than were started so: {rows:#?}\n{screen}"
        );
    }
    eprintln!(
        "known {known} of {} calls ({} rows)",
        calls.len(),
        rows.len()
    );
    assert!(
        known * 100 >= calls.len() * 95,
        "known {known} of {}: {rows:#?}\n{screen}",
        calls.len()
    );
}
