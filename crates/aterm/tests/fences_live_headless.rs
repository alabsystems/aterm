// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the WRITE FENCES and the SUPERVISOR KEY,
//! driven through the one `aterm` binary against a real headless instance.
//!
//! * THE SWAP SCENARIO. A FAKE WORKER (a POSIX `sh` script in raw mode that
//!   appends every byte it reads to a key log) draws approval box A on a
//!   fresh alternate screen; the test reads `status gen= seq= hash=`; the
//!   worker LEAVES AND RE-ENTERS the alternate screen and draws box B with the
//!   same number of writes — the review's live reproduction, where box B read
//!   the same `seq=` as A. `key if-gen=<A's gen> 1` and `key if-fp=<A's hash>
//!   1` must answer `OK skipped reason=changed` and the key log must stay
//!   EMPTY; the retired `if-seq=` is refused. NEGATIVE CONTROLS: `seq=`
//!   really does repeat (the fence it used to be would have pressed), and the
//!   fence built from B's own generation presses and the `1` reaches the log —
//!   so the log can see a press, and the skip is the fence deciding.
//! * THE SUPERVISOR KEY. `meta set supervisor <holder>` on a persistent
//!   connection shows in `sessions` and `status` while that connection lives
//!   and clears when it closes; a `ttl=` claim outlives its one-shot client.
//!   Two attention owners set and clear independently over the wire.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance opens no window.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use headless_boot::Instance;

/// Boot one headless instance, 40x120 ([`headless_boot::boot`]: `None` is an
/// environment refusal; a product that cannot start fails the test).
fn boot(tag: &str) -> Option<Instance> {
    headless_boot::boot(
        &format!("atfn{tag}"),
        &["--lines", "40", "--columns", "120"],
    )
}

/// A client that has not exited by now is hung: past every [`HANG`] wait a
/// call makes, so a wait that times out answers before it is killed.
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait for something that MUST happen may take before it is read
/// as a hang (AGENTS.md: a hang detector is a minute, never a latency budget).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// A connection-bound supervisor claim goes WITH its connection: the fastest
/// of [`CLAIMS_RELEASED`] closes is read off the roster within this, a roster
/// poll's `aterm ctl` spawn included. A loaded gate's one slow poll is one
/// close among several; a claim that outlives its connection by a lease is
/// late on every one (each was held to 5 s, then only to the minute's hang).
const RELEASED_PROMPTLY: Duration = Duration::from_secs(5);
const CLAIMS_RELEASED: usize = 4;

fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    cmd
}

/// One bounded `aterm ctl --sock <sock> <args…>` call.
fn ctl(inst: &Instance, args: &[&str]) -> Output {
    let mut child = client_command(inst, args).spawn().expect("spawn aterm ctl");
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + CLIENT_EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll aterm ctl") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout.join().expect("stdout drain"),
                    stderr: stderr.join().expect("stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("aterm ctl {args:?} did not exit in time");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn ctl_ok(inst: &Instance, args: &[&str]) -> String {
    let out = ctl(inst, args);
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// `key=value` out of a status/sessions line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

/// The boot session's `(local, sid)` from `sessions`.
fn boot_session(inst: &Instance) -> (u64, String) {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    let mut toks = row.split_whitespace();
    let local = toks.next().unwrap().parse().unwrap();
    let sid = toks.next().unwrap().to_string();
    (local, sid)
}

fn status(inst: &Instance, sid: &str) -> String {
    ctl_ok(inst, &[&format!("@{sid}"), "status"])
}

/// Wait for the process-name resolver before asserting supervisor behavior.
/// The fake worker's screen can be ready before its asynchronous name lookup.
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

/// `who`'s `watchers=` for `sid`: the live subscribers on the session, one of
/// which each parked `await` registers right after it arms its watcher.
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

/// Type one line into the session and submit it.
fn type_line(inst: &Instance, sid: &str, line: &str) {
    ctl_ok(inst, &[&format!("@{sid}"), "send", line]);
    ctl_ok(inst, &[&format!("@{sid}"), "key", "enter"]);
}

/// The fake worker: raw mode, a background reader appending every byte it
/// receives to `$2`, box A, then — once `$1` exists — box B, then idle. POSIX
/// sh + printf + dd only.
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

/// `await match <re>` on the session: the server's latched watcher, not a
/// client poll. `<re>` is one wire token (`.` for a space).
fn await_match(inst: &Instance, sid: &str, re: &str) {
    let reply = ctl_ok(inst, &[&format!("@{sid}"), "await", "match", re, HANG_MS]);
    assert!(
        reply.starts_with("OK") && !reply.starts_with("OK timeout"),
        "`{re}` never reached the screen: {reply}"
    );
}

/// THE SWAP SCENARIO, live: a fence built from box A's stamp never presses
/// into box B, and the worker receives nothing.
#[test]
fn a_fenced_key_skips_a_swapped_box_and_the_worker_receives_nothing() {
    let Some(inst) = boot("k") else { return };
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
    // Box A is whole once its last row is up (the worker draws top down).
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

    // Box B's `await match` is ARMED OVER BOX A, before the worker is let go:
    // box B arrives on a re-entered alternate screen at box A's own `seq=`,
    // and the observation kernel used to key change on `(seq, alt screen)`,
    // neither of which moves — this wait answered `OK timeout` while `text`
    // showed box B. It keys on the screen generation now. The worker draws
    // box B only once the go file exists, and the go file is created only
    // after the server reports the wait ARMED — `who`'s `watchers=` counts
    // the subscriber the await registers right after it arms its watcher —
    // so the watcher is armed over box A, never evaluated at arm over box B.
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
                // This wait must outlive the arm backstop below: a loaded full
                // workspace run can take longer than ten seconds to observe
                // the watcher even though the same test passes alone.
                "timeout=150000",
            ],
        );
        std::thread::spawn(move || cmd.output().expect("await match"))
    };
    // Armed, or the waiter ended without arming: the two events that decide
    // it. A wall-clock bound alone read a client starved of its spawn under a
    // loaded lane as "never armed" (red 1 in 4 under `verify-lane.sh -p aterm
    // --tests` on 2026-09-26, green 3/3 alone); the backstop is only for a
    // server that neither arms nor answers.
    //
    // Every `who` sample is kept — when it started, how long the call took,
    // what it read — and a failure prints them. This red was seen once in a
    // full gate (2026-09-27: the await answered its own 150 s `OK timeout`
    // while no sample had seen it armed, and the 120 s backstop never fired)
    // and never alone or under a 24-way CPU stress, so the next one has to
    // explain itself: slow calls say the client was starved, fast zeros say
    // the server never counted the watcher.
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
    // The retired spelling is refused, not pressed and not typed.
    let retired = ctl(
        &inst,
        &[&format!("@{sid}"), "key", &format!("if-seq={seq_a}"), "1"],
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&retired.stdout),
        String::from_utf8_lossy(&retired.stderr)
    );
    assert!(said.contains("ERR usage: if-seq= is not a fence"), "{said}");
    // The presses above were refused in the server, so nothing is in flight:
    // one `await idle` bounds the wait for a byte that must not come. The
    // window is its 200 ms of quiet; the timeout is only how long the quiet
    // may take to come (exit 124 fails `ctl_ok`), so it is a hang detector.
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", HANG_MS],
    );
    assert_eq!(
        std::fs::read(&keylog).expect("the key log"),
        b"",
        "a fence on the replaced box must deliver nothing"
    );

    // NEGATIVE CONTROL: B's own generation presses, and the log sees it.
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
    // A malformed fence is a usage error, and writes nothing.
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

/// THE SUPERVISOR KEY, live: shown while its persistent connection lives,
/// cleared when it closes; a `ttl=` claim outlives its one-shot client, and a
/// connection-bound one-shot claim is gone with its client. Plus two attention
/// owners over the wire.
#[test]
fn a_supervisor_claim_shows_in_the_roster_and_clears_when_its_connection_closes() {
    let Some(inst) = boot("v") else { return };
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
    let row = supervisor_until(&inst, &sid, "sup-live");
    // `supervisor=` is followed only by the OWNER'S COLUMNS (2026-09-24,
    // `help sessions`: `path_evidence= copy= upgrade=`, last): it is the column
    // the agent verdict's block ends with, and nothing else moved in between.
    let tail = ["path_evidence", "copy", "upgrade"];
    let words: Vec<&str> = row.split_whitespace().collect();
    let at = words
        .iter()
        .position(|w| *w == "supervisor=sup-live")
        .unwrap_or_else(|| panic!("no supervisor column: {row}"));
    let after: Vec<&str> = words[at + 1..]
        .iter()
        .map(|w| w.split_once('=').map_or(*w, |(k, _)| k))
        .collect();
    assert_eq!(after, tail, "{row}");
    assert_eq!(field(&status(&inst, &sid), "supervisor"), Some("sup-live"));
    // Still the same connection: other clients' connections closing (every
    // `aterm ctl` above) did not release it, and a second holder is refused.
    std::thread::sleep(Duration::from_millis(300));
    supervisor_until(&inst, &sid, "sup-live");
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

    // The connection closes: the claim goes with it. Shut down, then dropped:
    // the server sees the close by reading EOF, which a close alone delivers only
    // once every copy of this descriptor is gone — and std makes a connected
    // socket close-on-exec non-atomically on macOS, so a sibling test's spawn can
    // carry a copy into an `aterm` that outlives the wait below (the fd-copy
    // sweep of 2026-09-27). `shutdown` acts on the socket, copies and all.
    let closed = Instant::now();
    let _ = conn.get_ref().shutdown(std::net::Shutdown::Both);
    drop(conn);
    supervisor_until(&inst, &sid, "-");
    // How long each connection-bound claim outlived its connection, for
    // [`RELEASED_PROMPTLY`].
    let mut outlived = vec![closed.elapsed()];

    // A `ttl=` claim from a one-shot client outlives the client. Its ttl
    // outlives the wait for the roster to show it (it was 30 s, under that
    // wait), and it is unset below, not left to lapse.
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
    std::thread::sleep(Duration::from_millis(300));
    supervisor_until(&inst, &sid, "sup-ttl");
    ctl_ok(&inst, &[&format!("@{sid}"), "meta", "unset", "supervisor"]);
    supervisor_until(&inst, &sid, "-");
    // …and a one-shot claim WITHOUT ttl= is bound to a connection that has
    // already closed by the time anyone can read it.
    for _ in 1..CLAIMS_RELEASED {
        ctl_ok(
            &inst,
            &[&format!("@{sid}"), "meta", "set", "supervisor", "sup-once"],
        );
        let exited = Instant::now();
        supervisor_until(&inst, &sid, "-");
        outlived.push(exited.elapsed());
    }
    // Released WITH the connection, not by a lease of its own: each roster
    // wait above is only a hang detector, and a claim that held on for a
    // lease of seconds past its connection is late every time.
    let fastest = outlived.iter().min().copied().unwrap_or_default();
    assert!(
        fastest < RELEASED_PROMPTLY,
        "the fastest of {CLAIMS_RELEASED} connection-bound claims outlived its connection by \
         {fastest:?}: {outlived:?}"
    );

    // A `ttl=` claim nobody renews LAPSES, and the lapse is recorded — the
    // `meta-change field=supervisor value=-` a dead supervisor never writes
    // itself — by the instance's own timer. Both ends are read off the
    // TIMELINE, which keeps them after the lease is gone. A roster poll cannot
    // witness this claim: it is shown for 400 ms from the instant the server
    // takes it (`live_supervisor` filters on the expiry), and one poll has to
    // fit this client's exit, the next `aterm ctl`'s spawn and `sessions`' own
    // main-thread hop (allowed 500 ms) inside that window. On a loaded gate it
    // misses, and every later poll reads `-`. The `sup-ttl` claim above is the
    // roster's witness for a `ttl=` claim, and that it outlives its client.
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
    // Claimed (recorded only when a claim moves the SHOWN holder), then
    // lapsed, the lapse recorded exactly once — and the roster agrees.
    assert_eq!(
        since,
        ["value=sup-dies", "value=-"],
        "the claim, then its one lapse"
    );
    supervisor_until(&inst, &sid, "-");

    // Two attention owners, set and cleared independently.
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

/// A [`aterm_agent::supervise::run::Ctl`] that records every request before
/// the real transport sends it: what the engine asked the server, verbatim.
struct Recording<C> {
    inner: C,
    requests: Vec<String>,
    /// Once a `key` has gone out, every later `await` answers `ERR exited`
    /// — the session gone, as the engine's own mock ends a watch — so the
    /// loop ends after its press instead of at its budget, and the budget can
    /// be one only a hang reaches.
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

/// THE MERGED ENGINE'S FENCED PRESS, live (harness/integrate, 2026-09-24):
/// the engine's `supervise` at its default (every box its answer), over its
/// real persistent transport, reads the fake worker's read-only Bash box,
/// decides it, and presses
/// `key if-gen=<the read's generation> if=<the judged row> 1` — the
/// generation taken from its own `text --json` read (`"gen"`), the fence
/// found in the server's real `help key` answer — and the server takes it:
/// the `1` reaches the worker's key log. The fence form was not refused and
/// dropped (that would press under `if=` alone, and the recorded `key` line
/// would carry no `if-gen=`), and the retired `if-seq=` is never sent.
#[test]
fn the_supervisor_engine_presses_a_read_box_under_the_generation_fence() {
    use aterm_agent::supervise::run::{Session, SuperviseOpts};
    use aterm_agent::supervise::transport::{Endpoint, RelayCtl};
    let Some(inst) = boot("e") else { return };
    let (_local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    let keylog = inst.tmp.join("keys.log");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");
    // The engine judges a box only in a session whose FOREGROUND program is
    // an agent (`status program=`): the fake worker runs under the name
    // `claude`, in a job of its own, as a real one does (the form
    // agent_verdict_live_headless uses).
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
    // PRECONDITION: the server's own read carries the generation.
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
    // The budget is a hang discriminator: the loop ends once it has pressed
    // (`end_after_press`). It was 4 s, spent on read, settle, read, status,
    // `help key` and the press — each a round trip to the live instance's main
    // thread — so a contended gate returned before the press, and the fence
    // assertion failed on a correct engine (the load-sensitive test audit of
    // 2026-09-27).
    let opts = SuperviseOpts {
        max: Duration::from_secs(30),
        ..SuperviseOpts::default()
    };
    let ran = {
        let mut s = Session::new(&mut ctl, Some(format!("@{sid}")));
        // The approval ledger goes to the scratch world, never the real HOME.
        s.set_approval_ledger(Some(inst.tmp.join("approvals.jsonl")));
        let ran = s.supervise(&opts);
        drop(s);
        // Ended by `end_after_press` (a wait answered `ERR exited` after the
        // press), or by the budget; anything else is the loop failing.
        assert!(
            ran.as_ref()
                .map_or_else(|e| e.ends_with("ERR exited"), |_| true),
            "supervise ran: {ran:?} {:?}",
            ctl.requests
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
            let at = words.iter().position(|w| *w == "key").unwrap();
            words.get(at + 1).is_some_and(|w| {
                w.strip_prefix("if-gen=")
                    .and_then(|g| g.split_once('.'))
                    .is_some_and(|(e, s)| e.parse::<u64>().is_ok() && s.parse::<u64>().is_ok())
            }) && words.get(at + 2).is_some_and(|w| w.starts_with("if=^"))
                && words.last() == Some(&"1")
        }),
        "a press fenced on the read's generation: {:?}; result={ran:?}; status={}; text={}",
        ctl.requests,
        status(&inst, &sid),
        ctl_ok(&inst, &[&format!("@{sid}"), "text"])
    );
    assert!(
        !ctl.requests.iter().any(|r| r.contains("if-seq=")),
        "the retired seq fence is never sent: {:?}",
        ctl.requests
    );
    // The server took the fenced press: the `1` reached the worker.
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
