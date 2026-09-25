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
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(60);
const MAX_SOCK_PATH: usize = 100;

/// One booted headless instance plus its scratch world, torn down on every
/// exit path (Drop runs on panic too).
struct Instance {
    child: Child,
    tmp: PathBuf,
    log: PathBuf,
    sock: String,
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
    let name = format!("atfn{tag}-{}", std::process::id());
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
    let sock_path = tmp.join("run/aterm/aterm.sock");
    let mut inst = Instance {
        child,
        sock: sock_path.to_string_lossy().into_owned(),
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
        if is_socket_or_symlink(&sock_path) && launch_isolation::control_listening(&sock_path) {
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
put() { printf '\033[%d;1H\033[2K%s' "$1" "$2"; }
box() {
  printf '\033[2J'
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
    let reply = ctl_ok(
        inst,
        &[&format!("@{sid}"), "await", "match", re, "timeout=10000"],
    );
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
                "timeout=10000",
            ],
        );
        std::thread::spawn(move || cmd.output().expect("await match"))
    };
    let armed_by = Instant::now() + Duration::from_secs(5);
    while watchers(&inst, &sid) == 0 {
        assert!(Instant::now() < armed_by, "the await never armed");
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
    // one `await idle` bounds the wait for a byte that must not come.
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "idle", "200", "timeout=2000"],
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
    let deadline = Instant::now() + Duration::from_secs(5);
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
        &[&format!("@{sid}"), "await", "idle", "200", "timeout=2000"],
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
    let deadline = Instant::now() + Duration::from_secs(5);
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
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut conn = BufReader::new(stream);
    assert_eq!(
        request(
            &mut conn,
            &format!("TOKEN {token} @{sid} meta set supervisor sup-live")
        ),
        "OK\n"
    );
    let row = supervisor_until(&inst, &sid, "sup-live");
    assert!(row.trim_end().ends_with("supervisor=sup-live"), "{row}");
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

    // The connection closes: the claim goes with it.
    drop(conn);
    supervisor_until(&inst, &sid, "-");

    // A `ttl=` claim from a one-shot client outlives the client.
    ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "meta",
            "set",
            "supervisor",
            "sup-ttl",
            "ttl=30000",
        ],
    );
    std::thread::sleep(Duration::from_millis(300));
    supervisor_until(&inst, &sid, "sup-ttl");
    ctl_ok(&inst, &[&format!("@{sid}"), "meta", "unset", "supervisor"]);
    supervisor_until(&inst, &sid, "-");
    // …and a one-shot claim WITHOUT ttl= is bound to a connection that has
    // already closed by the time anyone can read it.
    ctl_ok(
        &inst,
        &[&format!("@{sid}"), "meta", "set", "supervisor", "sup-once"],
    );
    supervisor_until(&inst, &sid, "-");

    // A `ttl=` claim nobody renews LAPSES, and the lapse is recorded — the
    // `meta-change field=supervisor value=-` a dead supervisor never writes
    // itself — by the instance's own timer.
    let lapses = || {
        ctl_ok(&inst, &[&format!("@{sid}"), "timeline"])
            .lines()
            .filter(|l| l.contains("field=supervisor value=-"))
            .count()
    };
    let before = lapses();
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
    supervisor_until(&inst, &sid, "sup-dies");
    supervisor_until(&inst, &sid, "-");
    let deadline = Instant::now() + Duration::from_secs(5);
    while lapses() == before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(lapses(), before + 1, "the lapse is recorded once");

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
}

impl<C: aterm_agent::supervise::run::Ctl> aterm_agent::supervise::run::Ctl for Recording<C> {
    fn call(&mut self, args: &[&str]) -> Result<aterm_agent::supervise::run::CtlReply, String> {
        self.requests.push(args.join(" "));
        self.inner.call(args)
    }
}

/// THE MERGED ENGINE'S FENCED PRESS, live (harness/integrate, 2026-09-24):
/// lane B's `supervise --auto-reads`, over its real persistent transport,
/// reads the fake worker's read-only Bash box, decides it, and presses
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
    // PRECONDITION: the server's own read carries the generation.
    let json = ctl_ok(&inst, &[&format!("@{sid}"), "text", "--json"]);
    assert!(
        json.contains("\"gen\":\""),
        "text --json carries gen: {json}"
    );

    let mut ctl = Recording {
        inner: RelayCtl::new(Endpoint::Socket(inst.sock.clone()), None),
        requests: Vec::new(),
    };
    let opts = SuperviseOpts {
        auto_reads: true,
        max: Duration::from_secs(4),
        ..SuperviseOpts::default()
    };
    {
        let mut s = Session::new(&mut ctl, Some(format!("@{sid}")));
        // The approval ledger goes to the scratch world, never the real HOME.
        s.set_approval_ledger(Some(inst.tmp.join("approvals.jsonl")));
        let ran = s.supervise(&opts);
        drop(s);
        assert!(ran.is_ok(), "supervise ran: {ran:?} {:?}", ctl.requests);
    }
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
        "a press fenced on the read's generation: {:?}",
        ctl.requests
    );
    assert!(
        !ctl.requests.iter().any(|r| r.contains("if-seq=")),
        "the retired seq fence is never sent: {:?}",
        ctl.requests
    );
    // The server took the fenced press: the `1` reached the worker.
    let deadline = Instant::now() + Duration::from_secs(5);
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
