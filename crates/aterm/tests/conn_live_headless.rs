// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless end-to-end test for `aterm conn` (docs/design/SESSION_CONNECTIONS.md §9
//! [v5.1]): against a live headless instance, an add/set/rm round trip equals the wire verbs
//! byte for byte in `edges` output. Everything runs through the one `aterm` binary:
//! `aterm --headless` boots the engine and control socket, `aterm ctl …` speaks the raw wire,
//! `aterm conn …` is under test.
//!
//! Isolation is `support/launch_isolation.rs`'s, and every client call is pinned with `--sock`.
//! The test process is not an aterm session, so the `@self` forms run both without
//! `$ATERM_PARENT_SESSION_ID` (the documented refusal) and with it set to the boot session.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use aterm_control::wire::pct_decode;

use crate::{ctl_accept, headless_boot, launch_isolation};

use headless_boot::Instance;

/// Boot one real headless instance under the scratch world, 40x120
/// ([`headless_boot::boot`]: an environment refusal is COULD NOT RUN; a product
/// that cannot start fails the test).
fn boot() -> Instance {
    headless_boot::boot("atconn", &["--lines", "40", "--columns", "120"])
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);

/// Run one `aterm <args…>` client call with the hermetic environment plus `extra_env`, bounded
/// by [`CLIENT_EXIT_DEADLINE`]: drain threads keep the pipes flowing, so a hung client is a
/// panic, never a hung harness. A call the instance has not yet accepted, or answered busy, is
/// asked again, a fresh client each time ([`ctl_accept::until_served`]): the client's own 10 s
/// accept budget is a person's, and a loaded machine can miss it. An `aterm conn` call goes
/// through here too: it talks with no accept probe (`converse`, its own 60 s deadline), so it
/// never says "so none was sent", and only a busy answer is asked again.
fn run_client(inst: &Instance, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    ctl_accept::until_served(|| run_client_once(inst, args, extra_env))
}

/// One attempt of [`run_client`].
fn run_client_once(inst: &Instance, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
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

/// Parse the raw `sessions` rows, `<local> <sid> <parent|-> <state> <title> [meta=…]` (the
/// title pct-encoded and absent when empty), into `(local, sid, title)` triples.
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

/// The whole §9 [v5.1] loop against one live instance: spawn a second session, wire both
/// directions with `conn add`, narrow to pull with `conn set` and pin the raw `edges --json`
/// bytes, then `conn rm` both pairs. A fresh read grant stays usable on its established
/// connection after the destination closes, until the owner revokes it through the real
/// dispatch path. One test: the steps are one causal story.
#[test]
fn conn_round_trips_against_a_live_headless_instance() {
    let inst = boot();
    let sock = inst.sock.clone();

    // --- the two sessions -------------------------------------------------
    // The boot session is a fresh instance's only row; the second is minted over the wire
    // (`spawn`, reply `OK <sid>`, addressable at once).
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
    // Without $ATERM_PARENT_SESSION_ID the default direction's `@self` half refuses with the
    // documented error naming the variable (§6.1).
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
    client_ok(
        &inst,
        &["conn", "--sock", &sock, "add", &format!("@{sid2}")],
        in_session,
    );
    // The incoming half (§9: the no-arg form shows BOTH directions): invite
    // the peer as a pull-only controller of this session.
    client_ok(
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
    let got = client_ok(&inst, &["conn", "--sock", &sock, "ls"], &[]);
    assert_eq!(got.lines().count(), 2, "one line per directed pair: {got}");

    // --- (c) set --kind pull, then the raw-wire byte pin -------------------
    // Drop the incoming half first so exactly one pair remains under test.
    client_ok(
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
    client_ok(
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
    // The §9 wire-equivalence pin: after the both→pull set, the raw `edges --json` body (the
    // server's own emitter) holds exactly the read-screen row.
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
    client_ok(
        &inst,
        &["conn", "--sock", &sock, "rm", &format!("@{sid2}")],
        in_session,
    );
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

    let target = format!("@{sid2}");
    client_ok(&inst, &["ctl", "--sock", &sock, "tab", "1"], &[]);
    let grant = client_ok(
        &inst,
        &["ctl", "--sock", &sock, "grant", &sid1, "read-screen"],
        &[],
    );
    let token = grant.trim().strip_prefix("OK ").expect("read grant token");
    // Edge authentication uses the active live session. Keep the authenticated
    // connection across its close; the closed index does not mint new authority.
    let mut stream = UnixStream::connect(&sock).expect("connect read edge");
    let hang = Some(Duration::from_secs(60));
    stream.set_read_timeout(hang).unwrap();
    stream.set_write_timeout(hang).unwrap();
    writeln!(stream, "AUTH {token}").unwrap();
    let mut edge = BufReader::new(stream);
    let mut read_log = || {
        writeln!(edge.get_mut(), "{target} log").unwrap();
        let mut header = String::new();
        assert_ne!(edge.read_line(&mut header).unwrap(), 0, "edge disconnected");
        let mut rows = String::new();
        if let Some(rest) = header.strip_prefix("OK ") {
            let count: usize = rest.split_whitespace().next().unwrap().parse().unwrap();
            assert!(count <= 2050, "bounded log reply: {header}");
            for _ in 0..count {
                assert_ne!(edge.read_line(&mut rows).unwrap(), 0, "truncated log reply");
            }
        }
        (header, rows)
    };
    let (live, _) = read_log();
    assert!(live.starts_with("OK "), "live read control: {live}");
    assert_eq!(
        client_ok(&inst, &["ctl", "--sock", &sock, &target, "close"], &[]),
        format!("OK closed {sid2}\n")
    );
    let (closed, rows) = read_log();
    assert!(closed.starts_with("OK "), "closed read: {closed}");
    assert!(rows.contains(" close - "), "session close record: {rows}");
    let owner_log = client_ok(&inst, &["ctl", "--sock", &sock, &target, "log"], &[]);
    assert!(
        owner_log.contains(" close - "),
        "owner closed read: {owner_log}"
    );
    assert_eq!(
        client_ok(&inst, &["ctl", "--sock", &sock, "revoke", token], &[]),
        "OK\n"
    );
    assert_eq!(read_log().0, "ERR no such session\n");
}
