// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance tests for write fences and supervisor key claims.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use crate::{child_output, headless_boot};

use headless_boot::{
    Instance, boot_session, client_command, ctl, ctl_ok, field, status, type_line,
};

/// Boot one headless instance ([`headless_boot::boot`]).
fn boot(tag: &str) -> Instance {
    headless_boot::boot(
        &format!("atfn{tag}"),
        &["--lines", "40", "--columns", "120"],
    )
}

/// How long a wait for something that must happen may take before it is read as a hang (AGENTS.md).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// Wait for the process-name resolver before asserting supervisor behavior.
fn await_program(inst: &Instance, sid: &str, wanted: &str) {
    let deadline = Instant::now() + HANG;
    let mut last = status(inst, sid);
    while field(&last, "program") != Some(wanted) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        last = status(inst, sid);
    }
    assert_eq!(
        field(&last, "program"),
        Some(wanted),
        "foreground program did not resolve: {last}"
    );
}

/// `who`'s `watchers=` for `sid`.
fn watchers(inst: &Instance, sid: &str) -> usize {
    let who = ctl_ok(inst, &["who"]);
    let row = who
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(sid))
        .unwrap_or_else(|| panic!("no `who` row for {sid}: {who}"));
    field(row, "watchers")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no watchers= in {row}"))
}

/// The fake worker: raw mode.
const FAKE_WORKER: &str = r#"#!/bin/sh
go="$1"
log="$2"
stty raw -echo
: > "$log"
# An asynchronous list gets /dev/null for stdin unless told otherwise: read
# the terminal through a duplicate.
exec 3<&0
( while :; do dd bs=1 count=1 <&3 2>/dev/null >> "$log"; done ) &
rule=$(printf '%120s' '' | sed 's/ /─/g')
put() { printf '\033[%d;1H\033[2K%s' "$1" "$2"; }
box() {
  printf '\033[2J'
  put 1 "$rule"
  put 2 " Bash command"
  put 4 "   $1"
  put 6 " Do you want to proceed?"
  put 7 " ❯ 1. Yes"
  put 8 "   2. No"
  put 10 " Esc to cancel · Tab to amend"
}
printf '\033[?1049h'
box "ls -la /tmp/work"
while [ ! -e "$go" ]; do sleep 0.05; done
printf '\033[?1049l\033[?1049h'
box "rm -rf /tmp/work"
while :; do sleep 1; done
"#;

/// `await match <re>` on the session.
fn await_match(inst: &Instance, sid: &str, re: &str) {
    let reply = ctl_ok(inst, &[&format!("@{sid}"), "await", "match", re, HANG_MS]);
    assert!(
        reply.starts_with("OK") && !reply.starts_with("OK timeout"),
        "`{re}` never reached the screen: {reply}"
    );
}

#[test]
fn a_fenced_key_skips_a_swapped_box_and_the_worker_receives_nothing() {
    let inst = boot("k");
    let (_local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    let keylog = inst.tmp.join("keys.log");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/sh {} {} {}",
            script.display(),
            go.display(),
            keylog.display()
        ),
    );
    await_match(&inst, &sid, "ls.-la./tmp/work");
    await_match(&inst, &sid, "Esc.to.cancel");
    let a = status(&inst, &sid);
    let (gen_a, seq_a, hash_a) = (
        field(&a, "gen").unwrap().to_string(),
        field(&a, "seq").unwrap().to_string(),
        field(&a, "hash").unwrap().to_string(),
    );
    assert!(
        gen_a.contains('.') && seq_a.parse::<u64>().is_ok() && hash_a.len() == 16,
        "{a}"
    );
    assert_eq!(
        std::fs::metadata(&keylog).map(|m| m.len()).ok(),
        Some(0),
        "PRECONDITION: the worker is up and has received nothing"
    );

    assert_eq!(
        watchers(&inst, &sid),
        0,
        "PRECONDITION: nothing watches the session before the await"
    );
    let waiter = {
        let mut cmd = client_command(
            &inst,
            &[
                &format!("@{sid}"),
                "await",
                "match",
                "rm.-rf./tmp/work",
                "timeout=150000",
            ],
        );
        std::thread::spawn(move || {
            headless_boot::until_served(|| {
                let child = cmd.spawn().expect("spawn the await");
                child_output::wait_and_read(child, Instant::now() + Duration::from_secs(150) + HANG)
                    .expect("the await exits after its own timeout")
            })
        })
    };
    let started = Instant::now();
    let armed_by = started + Duration::from_secs(120);
    let mut samples: Vec<(Duration, Duration, usize)> = Vec::new();
    let timeline = |samples: &[(Duration, Duration, usize)]| {
        let slowest = samples.iter().map(|s| s.1).max().unwrap_or_default();
        let tail: Vec<String> = samples
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|(at, took, n)| format!("{at:.1?}+{took:.1?}={n}"))
            .collect();
        format!(
            "{} `who` samples over {:.1?}, slowest {slowest:.1?}; last: {}",
            samples.len(),
            started.elapsed(),
            tail.join(" ")
        )
    };
    loop {
        let at = started.elapsed();
        let n = watchers(&inst, &sid);
        samples.push((at, started.elapsed().saturating_sub(at), n));
        if n > 0 {
            break;
        }
        if waiter.is_finished() {
            let ended = waiter.join().expect("the waiter");
            panic!(
                "the await ended before it armed: {}{} ({})",
                String::from_utf8_lossy(&ended.stdout).trim_end(),
                String::from_utf8_lossy(&ended.stderr).trim_end(),
                timeline(&samples)
            );
        }
        assert!(
            Instant::now() < armed_by,
            "the await never armed ({})",
            timeline(&samples)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(&go, b"").expect("create the go file");
    let waited = waiter.join().expect("the waiter");
    let waited = String::from_utf8_lossy(&waited.stdout).to_string();
    assert!(
        waited.starts_with("OK") && !waited.starts_with("OK timeout"),
        "an `await match` armed over box A must latch on box B: {waited}"
    );
    await_match(&inst, &sid, "Esc.to.cancel");
    let b = status(&inst, &sid);
    let (gen_b, seq_b) = (
        field(&b, "gen").unwrap().to_string(),
        field(&b, "seq").unwrap().to_string(),
    );
    assert_eq!(
        seq_b, seq_a,
        "NEGATIVE CONTROL: the re-entered alternate screen repeats seq= ({a} / {b})"
    );
    assert_ne!(gen_b, gen_a, "the generation moved: {a} / {b}");

    for fence in [format!("if-gen={gen_a}"), format!("if-fp={hash_a}")] {
        let reply = ctl_ok(&inst, &[&format!("@{sid}"), "key", &fence, "1"]);
        assert!(
            reply.starts_with("OK skipped reason=changed seq="),
            "{fence}: {reply}"
        );
    }
    let retired = ctl(
        &inst,
        &[&format!("@{sid}"), "key", &format!("if-seq={seq_a}"), "1"],
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&retired.stdout),
        String::from_utf8_lossy(&retired.stderr)
    );
    assert!(said.contains("ERR usage"), "{said}");
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", HANG_MS],
    );
    assert_eq!(
        std::fs::read(&keylog).expect("the key log"),
        b"",
        "a fence on the replaced box must deliver nothing"
    );

    let reply = ctl_ok(
        &inst,
        &[&format!("@{sid}"), "key", &format!("if-gen={gen_b}"), "1"],
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    let deadline = Instant::now() + HANG;
    while Instant::now() < deadline && std::fs::read(&keylog).unwrap_or_default().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(&keylog).expect("the key log"),
        b"1",
        "the matched fence delivered exactly the one key"
    );
    let bad = ctl(&inst, &[&format!("@{sid}"), "key", "if-gen=soon", "1"]);
    assert!(
        String::from_utf8_lossy(&bad.stdout).contains("ERR usage: if-gen=")
            || String::from_utf8_lossy(&bad.stderr).contains("ERR usage: if-gen="),
        "{bad:?}"
    );
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", HANG_MS],
    );
    assert_eq!(std::fs::read(&keylog).unwrap(), b"1");
}

#[test]
fn a_refused_act_types_nothing() {
    let inst = boot("a");
    let (_local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    let keylog = inst.tmp.join("keys.log");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/sh {} {} {}",
            script.display(),
            go.display(),
            keylog.display()
        ),
    );
    await_match(&inst, &sid, "Esc.to.cancel");
    assert_eq!(
        std::fs::metadata(&keylog).map(|m| m.len()).ok(),
        Some(0),
        "PRECONDITION: the worker is up and has received nothing"
    );
    for bad in [
        vec!["act=bogus"],
        vec!["act="],
        vec!["act=approve", "act=deny"],
    ] {
        let mut args = vec![format!("@{sid}"), "key".to_string()];
        args.extend(bad.iter().map(|s| (*s).to_string()));
        args.push("1".to_string());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = ctl(&inst, &args);
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(said.contains("ERR usage: act="), "{bad:?}: {said}");
    }
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", HANG_MS],
    );
    assert_eq!(
        std::fs::read(&keylog).expect("the key log"),
        b"",
        "a refused act= must deliver nothing"
    );
    let reply = ctl_ok(&inst, &[&format!("@{sid}"), "key", "act=approve", "1"]);
    assert!(reply.starts_with("OK seq="), "{reply}");
    let deadline = Instant::now() + HANG;
    while Instant::now() < deadline && std::fs::read(&keylog).unwrap_or_default().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", HANG_MS],
    );
    assert_eq!(
        std::fs::read(&keylog).expect("the key log"),
        b"1",
        "key act=approve delivers exactly the key"
    );
    let log = ctl_ok(&inst, &[&format!("@{sid}"), "log", "n=400"]);
    assert!(
        log.lines()
            .any(|l| l.contains(" mark - input verb=key act=approve by=ctl ")),
        "{log}"
    );
}

/// One request on an already-authenticated persistent connection.
fn request(reader: &mut BufReader<std::os::unix::net::UnixStream>, line: &str) -> String {
    use std::io::Write;
    reader
        .get_mut()
        .write_all(format!("{line}\n").as_bytes())
        .expect("write a request");
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read a reply");
    reply
}

/// Poll the session's roster row until `supervisor=` reads `want`.
fn supervisor_until(inst: &Instance, sid: &str, want: &str) -> String {
    let deadline = Instant::now() + HANG;
    let mut row = String::new();
    while Instant::now() < deadline {
        let roster = ctl_ok(inst, &["sessions"]);
        row = roster
            .lines()
            .find(|l| l.contains(sid))
            .unwrap_or_default()
            .to_string();
        if field(&row, "supervisor") == Some(want) {
            return row;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("supervisor={want} not reached; last row: {row}");
}

#[test]
fn a_supervisor_claim_shows_in_the_roster_and_clears_when_its_connection_closes() {
    let inst = boot("v");
    let (_local, sid) = boot_session(&inst);
    let token = aterm_ctl::read_token_beside(&inst.sock)
        .expect("the socket's token")
        .trim()
        .to_string();
    let stream = std::os::unix::net::UnixStream::connect(&inst.sock).expect("connect");
    stream.set_read_timeout(Some(HANG)).expect("read timeout");
    let mut conn = BufReader::new(stream);
    assert_eq!(
        request(
            &mut conn,
            &format!("TOKEN {token} @{sid} meta set supervisor sup-live")
        ),
        "OK\n"
    );
    supervisor_until(&inst, &sid, "sup-live");
    assert_eq!(field(&status(&inst, &sid), "supervisor"), Some("sup-live"));
    let busy = ctl(
        &inst,
        &[
            &format!("@{sid}"),
            "meta",
            "set",
            "supervisor",
            "sup-other",
            "ttl=5000",
        ],
    );
    assert!(
        String::from_utf8_lossy(&busy.stdout).contains("ERR busy supervisor=sup-live")
            || String::from_utf8_lossy(&busy.stderr).contains("ERR busy supervisor=sup-live"),
        "{busy:?}"
    );
    assert!(request(&mut conn, &format!("@{sid} meta")).contains(" supervisor=sup-live"));

    let _ = conn.get_ref().shutdown(std::net::Shutdown::Both);
    drop(conn);
    supervisor_until(&inst, &sid, "-");

    ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "meta",
            "set",
            "supervisor",
            "sup-ttl",
            "ttl=120000",
        ],
    );
    supervisor_until(&inst, &sid, "sup-ttl");
    ctl_ok(&inst, &[&format!("@{sid}"), "meta", "unset", "supervisor"]);
    supervisor_until(&inst, &sid, "-");
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "meta", "set", "supervisor", "sup-once"],
    );
    supervisor_until(&inst, &sid, "-");

    let changes = || -> Vec<String> {
        ctl_ok(&inst, &[&format!("@{sid}"), "timeline"])
            .lines()
            .filter_map(|l| l.split_once(" kind=meta-change field=supervisor "))
            .map(|(_, value)| value.to_string())
            .collect()
    };
    let before = changes().len();
    ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "meta",
            "set",
            "supervisor",
            "sup-dies",
            "ttl=400",
        ],
    );
    let deadline = Instant::now() + HANG;
    let mut since = changes().split_off(before);
    while !since.iter().any(|v| v == "value=-") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        since = changes().split_off(before);
    }
    assert_eq!(
        since,
        ["value=sup-dies", "value=-"],
        "the claim, then its one lapse"
    );
    supervisor_until(&inst, &sid, "-");

    let meta = |args: &[&str]| {
        let mut full = vec![format!("@{sid}"), "meta".to_string()];
        full.extend(args.iter().map(|a| (*a).to_string()));
        let full: Vec<&str> = full.iter().map(String::as_str).collect();
        ctl_ok(&inst, &full)
    };
    meta(&["set", "attention", "owner=sup", "approve", "rm"]);
    meta(&["set", "attention", "owner=human", "look", "here"]);
    let read = meta(&[]);
    assert_eq!(field(&read, "attention"), Some("look%20here"), "{read}");
    assert_eq!(field(&read, "attention_owners"), Some("2"), "{read}");
    meta(&["unset", "attention", "owner=human"]);
    let read = meta(&[]);
    assert_eq!(field(&read, "attention"), Some("approve%20rm"), "{read}");
    assert_eq!(field(&read, "attention_owner"), Some("sup"), "{read}");
    meta(&["unset", "attention", "owner=sup"]);
    assert_eq!(field(&meta(&[]), "attention"), Some("-"));
}

/// A [`aterm_agent::supervise::run::Ctl`] that records every request before the real transport
/// sends it.
struct Recording<C> {
    inner: C,
    requests: Vec<String>,
    /// Once a `key` has gone out.
    end_after_press: bool,
}

impl<C: aterm_agent::supervise::run::Ctl> aterm_agent::supervise::run::Ctl for Recording<C> {
    fn call(&mut self, args: &[&str]) -> Result<aterm_agent::supervise::run::CtlReply, String> {
        let pressed = self
            .requests
            .iter()
            .any(|r| r.split_whitespace().any(|w| w == "key"));
        self.requests.push(args.join(" "));
        if self.end_after_press && pressed && args.contains(&"await") {
            return Ok(aterm_agent::supervise::run::CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: "aterm-ctl: ERR exited\n".to_string(),
            });
        }
        self.inner.call(args)
    }
}

#[test]
fn the_supervisor_engine_presses_a_read_box_under_the_generation_fence() {
    use aterm_agent::supervise::run::{Session, SuperviseOpts};
    use aterm_agent::supervise::transport::{Endpoint, RelayCtl};
    let inst = boot("e");
    let (_local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    let keylog = inst.tmp.join("keys.log");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {}'",
            script.display(),
            go.display(),
            keylog.display()
        ),
    );
    await_match(&inst, &sid, "ls.-la./tmp/work");
    await_match(&inst, &sid, "Esc.to.cancel");
    await_program(&inst, &sid, "claude");
    let json = ctl_ok(&inst, &[&format!("@{sid}"), "text", "--json"]);
    assert!(
        json.contains("\"gen\":\""),
        "text --json carries gen: {json}"
    );

    let mut ctl = Recording {
        inner: RelayCtl::new(Endpoint::Socket(inst.sock.clone()), None),
        requests: Vec::new(),
        end_after_press: true,
    };
    let opts = SuperviseOpts {
        max: HANG,
        ..SuperviseOpts::default()
    };
    let ran = {
        let mut s = Session::new(&mut ctl, Some(format!("@{sid}")));
        s.set_approval_ledger(Some(inst.tmp.join("approvals.jsonl")));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut out: Vec<u8> = Vec::new();
        let ran = s.run_hosted(&opts, stop, &mut out);
        drop(s);
        assert!(
            ran.as_ref()
                .map_or_else(|e| e.ends_with("ERR exited"), |()| true),
            "the loop ran: {ran:?} {:?} {}",
            ctl.requests,
            String::from_utf8_lossy(&out)
        );
        ran
    };
    let presses: Vec<&String> = ctl
        .requests
        .iter()
        .filter(|r| r.split_whitespace().any(|w| w == "key"))
        .collect();
    assert!(
        presses.iter().any(|p| {
            let words: Vec<&str> = p.split_whitespace().collect();
            let mut at = words.iter().position(|w| *w == "key").unwrap() + 1;
            if words.get(at) != Some(&"act=approve") {
                return false;
            }
            at += 1;
            if words.get(at).is_some_and(|w| {
                w.strip_prefix("if-human=")
                    .is_some_and(|n| n.parse::<u64>().is_ok())
            }) {
                at += 1;
            }
            words.get(at).is_some_and(|w| {
                w.strip_prefix("if-gen=")
                    .and_then(|g| g.split_once('.'))
                    .is_some_and(|(e, s)| e.parse::<u64>().is_ok() && s.parse::<u64>().is_ok())
            }) && words.get(at + 1).is_some_and(|w| w.starts_with("if=^"))
                && words.last() == Some(&"1")
        }),
        "a press fenced on the read's generation: {:?}; result={ran:?}; status={}; text={}",
        ctl.requests,
        status(&inst, &sid),
        ctl_ok(&inst, &[&format!("@{sid}"), "text"])
    );
    let deadline = Instant::now() + HANG;
    let mut got = Vec::new();
    while Instant::now() < deadline {
        got = std::fs::read(&keylog).unwrap_or_default();
        if got.contains(&b'1') {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(got, b"1", "exactly the approving 1: {:?}", ctl.requests);
}
