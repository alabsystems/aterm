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
//! HOME and XDG roots, an explicit private control socket, and a private config
//! disabling unrelated package, primer and machine maintenance. Reroute and
//! native-update work are explicitly disabled too: private runtime/config roots
//! alone still let a real window rewrite the user's default package prefix.
//! `SHELL=/bin/sh` avoids shell rc files. Every client call is pinned to the
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
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use headless_boot::{Instance, log_tail};

/// Boot one real headless instance under the scratch world, 40x120
/// ([`headless_boot::boot`]: `None` is an environment refusal; a product that
/// cannot start fails the test).
fn boot() -> Option<Instance> {
    headless_boot::boot("atconn", &["--lines", "40", "--columns", "120"])
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);

/// Apply the hermetic environment shared by the server and every client call:
/// scratch runtime + config dirs, a quiet known shell, and none of the
/// caller's own aterm session/socket context (the test may itself be running
/// inside an aterm terminal — its env must never leak into the harness).
fn hermetic_env(cmd: &mut Command, tmp: &Path) {
    launch_isolation::apply(cmd, tmp);
}

/// THE SCRATCH WORLD KEEPS EVERY LAUNCH OFF THE MACHINE, through CONFIG (2026-09-23):
/// the update system's environment vetoes are gone, so the observer reads the shipping
/// config readers in the child's real launch environment and records what the
/// automatic lanes would do — the app updater (`[update] enabled`), the package loop
/// (`[packages] enabled`) — while the reroute's escape is a FLAG no environment can
/// carry (`aterm --no-reroute`; the internal marker is cleared at the front door). The
/// negative control is the same scratch world with the switches left at their
/// batteries-included defaults, which must read as "would run", so the isolated
/// "blocked" is the config's doing.
#[test]
fn private_launch_environment_gates_host_maintenance() {
    const OBSERVER: &str = "ATERM_FIXTURE_OBSERVER_ROOT";
    if let Some(root) = std::env::var_os(OBSERVER) {
        // Executed in a child process with the actual launch environment. Read
        // the shipping config/store/policy APIs, but never perform maintenance.
        let root = PathBuf::from(root);
        for (name, relative) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "cfg"),
            ("XDG_RUNTIME_DIR", "run"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
        ] {
            assert_eq!(std::env::var_os(name), Some(root.join(relative).into()));
        }
        assert!(std::env::var_os("ATERM_PARENT_SESSION_ID").is_none());
        // No environment veto is set, and none is needed: nothing reads one.
        for name in [
            "ATERM_NO_AUTO_UPDATE",
            "ATERM_NO_AUTO_APPLY",
            "ATPKG_DISABLE",
            "ATERM_NO_REROUTE",
            "ATERM_UPDATE_ROOT",
            atpkg::reroute::PASSTHROUGH_ENV,
        ] {
            assert_eq!(std::env::var_os(name), None, "{name}");
        }
        let layout = atpkg::store::resolve_configured().expect("private store resolves");
        assert!(layout.prefix.starts_with(root.join("home")));
        let updates = aterm_update_core::seal_guard::updates_root().expect("updates root");
        assert!(
            updates.starts_with(root.join("home")),
            "{}",
            updates.display()
        );
        let machine = atpkg::config::load_machine();
        assert!(!machine.spotlight_noindex());
        assert_eq!(
            machine.universal_control(),
            atpkg::config::UniversalControlPolicy::Leave
        );
        let config = root.join("cfg/aterm/aterm.toml");
        let updater = aterm_update_core::settings::update_enabled_at(&config);
        let packages = atpkg::config::load().enabled();
        // A fake writer records the real admission decision in private state.
        // It does not run an updater, a pass, launchd, defaults, or any maintenance.
        std::fs::write(
            root.join("observed"),
            if updater || packages {
                "would-run"
            } else {
                "blocked"
            },
        )
        .unwrap();
        return;
    }

    let base = std::env::temp_dir().join(format!("atconn-isolation-{}", std::process::id()));
    for (case, defaults, expected) in [
        ("isolated", false, "blocked"),
        ("negative-control", true, "would-run"),
    ] {
        let root = base.join(case);
        launch_isolation::prepare(&root).unwrap();
        if defaults {
            // The update switches at their defaults; the host settings still left
            // alone, so the observer's `[machine]` assertions hold either way.
            std::fs::write(
                root.join("cfg/aterm/aterm.toml"),
                "agents_auto_prime = false\n[machine]\nspotlight_noindex = false\n\
                 universal_control = \"leave\"\n",
            )
            .unwrap();
        }
        let log = root.join("observer.log");
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "private_launch_environment_gates_host_maintenance",
            "--nocapture",
        ])
        // Explicit foreign overrides must be removed as well as ambient ones.
        .env("ATERM_PARENT_SESSION_ID", "foreign-session")
        .env("ATERM_NO_AUTO_UPDATE", "1")
        .env(atpkg::reroute::PASSTHROUGH_ENV, "1");
        hermetic_env(&mut cmd, &root);
        cmd.env(OBSERVER, &root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap());
        let mut child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{case}: {}", log_tail(&log));
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{case}: isolation observer did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("observed")).unwrap(),
            expected
        );
    }
    std::fs::remove_dir_all(base).unwrap();
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

/// NEGATIVE CONTROL for the shared boot (`support/headless_boot.rs`): an
/// instance that cannot start FAILS the test. Every copy of the boot this file
/// once carried answered this case with `SKIP` and a green test — so a startup
/// crash read as a pass. A launch flag the binary refuses is the startup
/// failure here.
#[test]
#[should_panic(expected = "before its control socket listened")]
fn a_headless_instance_that_cannot_start_fails_the_boot() {
    let _ = headless_boot::boot("atconn-bad", &["--no-such-launch-flag"]);
}
