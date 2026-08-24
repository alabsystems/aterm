// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless end-to-end test for `aterm conn` — the §9 [v5.1] obligation
//! (docs/design/SESSION_CONNECTIONS.md): the no-arg form shows both directions
//! against a LIVE headless instance, and add/set/rm round-trip equals the wire
//! verbs BYTE-FOR-BYTE in `edges` output.
//!
//! The whole exercise runs through the ONE `aterm` binary (the same
//! `CARGO_BIN_EXE_aterm` seam `protected_spawn.rs` drives): `aterm --headless`
//! boots the real engine + control socket, `aterm ctl …` speaks the raw wire,
//! and `aterm conn …` is the presentation layer under test — three faces of
//! the shipped front door, no mocks anywhere.
//!
//! ISOLATION (the smoke-stage `Sandbox` discipline, `aterm-verify`
//! `smoke_stages.rs`): the instance runs under a per-run scratch
//! `XDG_RUNTIME_DIR` (its socket, token, and discovery-graph entries all land
//! inside it — never in the user's live socket dir) and a scratch, EMPTY
//! `XDG_CONFIG_HOME` (a test instance must never read or WRITE the developer's
//! real `aterm.toml` — the 2026-08-10 probe font incident), with
//! `SHELL=/bin/sh` so no rc file leaks in. Every client call is pinned to the
//! instance with the explicit `--sock` flag and the same scratch environment.
//!
//! SHELL-LESS CALLER NOTE: the test process is not an aterm session, so the
//! `@self` forms are exercised BOTH ways — once refusing without
//! `$ATERM_PARENT_SESSION_ID` (the documented shell-less error), and then with
//! the harness hosting the session context by setting that variable to the
//! boot session's real sid, exactly what an in-session shell would carry.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The socket-bind budget for the headless boot (matches the verify smoke's
/// 100 x 100 ms), and the per-client-call exit bound. Both GENEROUS: a healthy
/// run binds in well under a second and each client call finishes in tens of
/// milliseconds; the bounds only turn a wedged run into a diagnosable failure
/// instead of a hung `cargo test`.
const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);

/// `sockaddr_un.sun_path` is ~104 bytes on macOS/BSD; refuse bases that would
/// overflow it (with margin) instead of failing deep inside bind/connect.
const MAX_SOCK_PATH: usize = 100;

/// One booted headless instance plus its scratch world, torn down (kill, reap,
/// remove) on every exit path — Drop runs on panic too, so a failing assert
/// never leaks a live aterm or a scratch dir.
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

/// The tail of the instance log, for skip/failure diagnostics.
fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

/// Whether `path` exists as a unix socket or a symlink (the `latest` alias) —
/// the same readiness probe the verify smoke uses.
fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

fn chmod_700(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

/// Pick a scratch base whose socket path fits `sun_path`: `$TMPDIR` (via
/// `temp_dir`), else `/tmp`. `None` when neither fits — an environment refusal,
/// reported as a clean SKIP by the caller.
fn scratch_root() -> Option<PathBuf> {
    let name = format!("atconn-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let tmp = base.join(&name);
        let sock = tmp.join("run/aterm/aterm.sock");
        if sock.as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if std::fs::create_dir_all(tmp.join("run/aterm")).is_err() {
            continue;
        }
        if std::fs::create_dir_all(tmp.join("cfg/aterm")).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }
        chmod_700(&tmp);
        chmod_700(&tmp.join("run"));
        chmod_700(&tmp.join("run/aterm"));
        chmod_700(&tmp.join("cfg"));
        return Some(tmp);
    }
    None
}

/// Apply the hermetic environment shared by the server and every client call:
/// scratch runtime + config dirs, a quiet known shell, and none of the
/// caller's own aterm session/socket context (the test may itself be running
/// inside an aterm terminal — its env must never leak into the harness).
fn hermetic_env(cmd: &mut Command, tmp: &Path) {
    cmd.env("XDG_RUNTIME_DIR", tmp.join("run"))
        .env("XDG_CONFIG_HOME", tmp.join("cfg"))
        .env("SHELL", "/bin/sh")
        .env_remove("ATERM_HEADLESS")
        .env_remove("ATERM_CONTROL_SOCK")
        .env_remove("ATERM_NO_CONTROL_SOCK")
        .env_remove("ATERM_PARENT_SESSION_ID")
        .env_remove("ATERM_CONTAINMENT_MODE");
}

/// Boot one real headless instance under the scratch world. `None` means the
/// binary cannot boot headless in this sandbox (announced as a SKIP with the
/// log tail) — the GPU-test skip idiom, reserved for environmental refusals.
fn boot() -> Option<Instance> {
    let Some(tmp) = scratch_root() else {
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
    cmd.arg("--headless")
        .env("ATERM_LINES", "40")
        .env("ATERM_COLUMNS", "120")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    hermetic_env(&mut cmd, &tmp);
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
        if is_socket_or_symlink(&sock_path) {
            return Some(inst);
        }
        std::thread::sleep(POLL_GAP);
    }
    eprintln!(
        "SKIP: control socket never appeared; log tail:\n{}",
        log_tail(&inst.log)
    );
    None
}

/// Run one `aterm <args…>` client call against the instance with the hermetic
/// environment plus `extra_env`, bounded by [`CLIENT_EXIT_DEADLINE`]: drain
/// threads keep the child's pipes flowing while the main thread polls, so a
/// hung client becomes a panic with the captured output, never a hung harness
/// (the `protected_spawn.rs` bounded-wait discipline, compacted).
fn run_client(inst: &Instance, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hermetic_env(&mut cmd, &inst.tmp);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn the aterm client");
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
        match child.try_wait().expect("poll the aterm client") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout.join().expect("join the stdout drain"),
                    stderr: stderr.join().expect("join the stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "aterm {args:?} did not exit within {}s",
                    CLIENT_EXIT_DEADLINE.as_secs()
                );
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// [`run_client`] asserting success, returning stdout as UTF-8.
fn client_ok(inst: &Instance, args: &[&str], extra_env: &[(&str, &str)]) -> String {
    let out = run_client(inst, args, extra_env);
    assert!(
        out.status.success(),
        "aterm {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("client stdout is UTF-8")
}

/// Decode the wire's percent-encoding (the tolerant `aterm conn` rule: a `%`
/// not followed by two hex digits passes through verbatim).
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse the raw `sessions` rows (`<local> <sid> <parent|-> <state> <title>
/// [meta=…]`, title pct-encoded — vanishing entirely when empty) into
/// `(local, sid, title)` triples.
fn parse_sessions(body: &str) -> Vec<(u64, String, String)> {
    let mut rows = Vec::new();
    for line in body.lines() {
        let mut toks = line.split_whitespace();
        let (Some(local), Some(sid), Some(_parent), Some(_state)) =
            (toks.next(), toks.next(), toks.next(), toks.next())
        else {
            continue;
        };
        let Ok(local) = local.parse::<u64>() else {
            continue;
        };
        let title = match toks.next() {
            None => String::new(),
            Some(t) if t.starts_with("meta=") => String::new(),
            Some(t) => pct_decode(t),
        };
        rows.push((local, sid.to_string(), title));
    }
    rows
}

/// The quoted-title fetch for one sid, fresh from the wire (titles are live
/// state; each byte-pin reads them immediately before rendering its
/// expectation, exactly as `aterm conn` itself does).
fn titles(inst: &Instance, sock: &str) -> Vec<(u64, String, String)> {
    parse_sessions(&client_ok(inst, &["ctl", "--sock", sock, "sessions"], &[]))
}

fn title_of(rows: &[(u64, String, String)], sid: &str) -> String {
    let t = rows
        .iter()
        .find(|(_, s, _)| s == sid)
        .map(|(_, _, t)| t.as_str())
        .unwrap_or_default();
    format!("\"{t}\"")
}

/// The whole §9 [v5.1] loop against ONE live headless instance: spawn a second
/// session over the wire, wire both directions with `conn add`, pin the no-arg
/// and `ls` renders byte-stably, narrow to pull with `conn set` and pin the
/// raw `edges --json` wire bytes to exactly the read-screen row, then `conn rm`
/// both pairs back to empty and pin the empty renders too. One test function:
/// the steps are one causal story, and a single instance keeps the run modest.
#[test]
fn conn_round_trips_against_a_live_headless_instance() {
    let Some(inst) = boot() else { return };
    let sock = inst.sock.clone();

    // --- the two sessions -------------------------------------------------
    // The boot session is the only row of a fresh instance; the second is
    // minted over the REAL wire (`spawn`, reply `OK <sid>` — immediately
    // addressable, no shell settle needed for connection acts).
    let rows = titles(&inst, &sock);
    assert_eq!(
        rows.len(),
        1,
        "a fresh headless instance hosts exactly the boot session, got {rows:?}"
    );
    let sid1 = rows[0].1.clone();
    let spawn = client_ok(&inst, &["ctl", "--sock", &sock, "spawn"], &[]);
    let sid2 = spawn
        .trim()
        .strip_prefix("OK ")
        .expect("spawn replies OK <sid>")
        .to_string();
    assert!(sid2.starts_with("s-"), "minted sid shape: {sid2}");
    assert_ne!(sid1, sid2);

    // --- (a) the shell-less refusal, then the hosted add ------------------
    // Without $ATERM_PARENT_SESSION_ID the default direction's `@self` half
    // must refuse with the documented error naming the variable (§6.1) — the
    // honest shell-less-caller behavior, live.
    let refused = run_client(
        &inst,
        &["conn", "--sock", &sock, "add", &format!("@{sid2}")],
        &[],
    );
    assert!(
        !refused.status.success(),
        "@self outside a session must refuse"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("ATERM_PARENT_SESSION_ID"),
        "the refusal names the env var: {:?}",
        String::from_utf8_lossy(&refused.stderr)
    );
    // The harness hosts the session context the way a real in-session shell
    // would: $ATERM_PARENT_SESSION_ID = the boot session's actual sid.
    let in_session: &[(&str, &str)] = &[("ATERM_PARENT_SESSION_ID", sid1.as_str())];
    let got = client_ok(
        &inst,
        &["conn", "--sock", &sock, "add", &format!("@{sid2}")],
        in_session,
    );
    assert_eq!(got, format!("connected {sid1} -> {sid2} (both)\n"));
    // The incoming half (§9: the no-arg form shows BOTH directions): invite
    // the peer as a pull-only controller of this session.
    let got = client_ok(
        &inst,
        &[
            "conn",
            "--sock",
            &sock,
            "add",
            &format!("@{sid2}"),
            "--to-me",
            "--kind",
            "pull",
        ],
        in_session,
    );
    assert_eq!(got, format!("connected {sid2} -> {sid1} (pull)\n"));

    // --- (b) the no-arg and ls renders, byte-stable -----------------------
    // Titles are live wire state, so each pin fetches them immediately before
    // rendering its expectation — the same `sessions` index `conn` reads.
    let rows = titles(&inst, &sock);
    let (t1, t2) = (title_of(&rows, &sid1), title_of(&rows, &sid2));
    let got = client_ok(&inst, &["conn", "--sock", &sock], in_session);
    assert_eq!(
        got,
        format!(
            "\u{21e5} both  {sid2} {t2}\n\
             \u{21e4} pull  {sid2} {t2}\n\
             \n\
             drive it: aterm ctl @{sid2} turn 'your message'\n"
        ),
        "the no-arg render: outgoing \u{21e5}, incoming \u{21e4}, the drive hint"
    );
    // `conn ls`: one line per directed pair, in the wire's sorted (src, dst)
    // order — computed here the same way so the pin is order-exact.
    let mut pairs = [
        format!("{sid1} -> {sid2}  both  {t1} -> {t2}"),
        format!("{sid2} -> {sid1}  pull  {t2} -> {t1}"),
    ];
    pairs.sort();
    let got = client_ok(&inst, &["conn", "--sock", &sock, "ls"], &[]);
    assert_eq!(got, format!("{}\n{}\n", pairs[0], pairs[1]));

    // --- (c) set --kind pull, then the raw-wire byte pin -------------------
    // Drop the incoming half first so exactly one pair remains under test.
    let got = client_ok(
        &inst,
        &[
            "conn",
            "--sock",
            &sock,
            "rm",
            &format!("@{sid2}"),
            "--to-me",
        ],
        in_session,
    );
    assert_eq!(got, format!("disconnected {sid2} -> {sid1} (1 revoked)\n"));
    let got = client_ok(
        &inst,
        &[
            "conn",
            "--sock",
            &sock,
            "set",
            &format!("@{sid2}"),
            "--kind",
            "pull",
        ],
        in_session,
    );
    assert_eq!(got, format!("set {sid1} -> {sid2} (pull)\n"));
    // The §9 wire-equivalence pin, BYTE-FOR-BYTE: after the both→pull set,
    // the raw `edges --json` body (the server's own emitter, no conn in the
    // path) holds exactly the read-screen row — the push half is gone and no
    // intermediate authority appeared in its place.
    let got = client_ok(
        &inst,
        &[
            "ctl",
            "--sock",
            &sock,
            &format!("@{sid2}"),
            "edges",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        got,
        format!(
            "{{\"edges\":[{{\"src\":\"{sid1}\",\"dst\":\"{sid2}\",\"op\":\"read-screen\"}}],\
             \"dst\":\"{sid2}\"}}\n"
        )
    );

    // --- (d) rm, then verify empty ----------------------------------------
    let got = client_ok(
        &inst,
        &["conn", "--sock", &sock, "rm", &format!("@{sid2}")],
        in_session,
    );
    assert_eq!(got, format!("disconnected {sid1} -> {sid2} (1 revoked)\n"));
    let got = client_ok(
        &inst,
        &[
            "ctl",
            "--sock",
            &sock,
            &format!("@{sid2}"),
            "edges",
            "--json",
        ],
        &[],
    );
    assert_eq!(got, format!("{{\"edges\":[],\"dst\":\"{sid2}\"}}\n"));
    let got = client_ok(&inst, &["conn", "--sock", &sock], in_session);
    assert_eq!(
        got,
        format!(
            "no session connections for {sid1}\n\
             create one:\n  \
             aterm conn add @<sid>        take control of a session (pull+push)\n  \
             aterm conn spawn controlled  spawn a new session this one controls\n"
        ),
        "the empty-state render after rm"
    );

    // The proof-it-ran marker: a skipped run never prints this line.
    eprintln!(
        "conn e2e: ran live against headless pid {} ({sid1} -> {sid2}) at {sock}",
        inst.child.id()
    );
}
