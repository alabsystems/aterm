// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the HOST's reading of an agent session: the
//! program an atpkg shim launches and the wall its turn ended on, driven
//! through the one `aterm` binary against a real headless instance.
//!
//! * THE SHIM WINDOW. `claude` typed in an aterm shell runs atpkg's twin, a
//!   `#!/bin/sh` script `…/agents/claude` that `exec`s the store binary; until
//!   the exec the foreground leader's argv[0] is `/bin/sh` (measured). A twin
//!   that draws a Claude end-of-turn screen and never execs must read
//!   `program=claude` — and so be read by the Claude reader — not `sh`.
//!   NEGATIVE CONTROLS: the same script under another name reads `program=sh
//!   agent=-`, and so does one named `claude` outside the managed prefix (a
//!   user's own wrapper).
//! * THE WALL. The screen it draws is aterm-phase's 529 end of turn
//!   (`hand-built-529-end-of-turn.txt`): `agent=wall:overloaded`, `level=
//!   limited`, and `await agent wall:overloaded` / `await agent wall` latch.
//!   It read `agent=idle` before, and a supervisor that trusted idle waited
//!   for a worker that would never move.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer.

#![cfg(unix)]

use std::io::Read;
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
    let name = format!("athp{tag}-{}", std::process::id());
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

/// Boot one headless instance, 40x200. `None` = an environmental refusal,
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
        // Wide enough that the fixture's longest row (the 529 notice) is
        // not wrapped by the terminal: Claude Code lays out its own rows, so
        // a terminal wrap to column 0 is not a shape it draws.
        .env("ATERM_COLUMNS", "200")
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

/// Type one line into the session and submit it.
fn type_line(inst: &Instance, sid: &str, line: &str) {
    ctl_ok(inst, &[&format!("@{sid}"), "send", line]);
    ctl_ok(inst, &[&format!("@{sid}"), "key", "enter"]);
}

/// Poll `status` until `pred` holds, returning the matching record, or panic
/// with the last record after `within`.
fn status_until(
    inst: &Instance,
    sid: &str,
    within: Duration,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = status(inst, sid);
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what}: not within {within:?}; last status: {last}");
}

/// aterm-phase's 529 end of turn, its provenance line dropped.
fn screen_529() -> String {
    let text = include_str!("../../aterm-phase/src/fixtures/hand-built-529-end-of-turn.txt");
    text.split_once('\n')
        .map_or(text, |(_, rest)| rest)
        .to_string()
}

/// A shim-shaped script: `#!/bin/sh`, draw the 529 screen on the alternate
/// screen, then wait without ever `exec`-ing.
fn write_shim(path: &Path, screen: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let body = format!(
        "#!/bin/sh\nprintf '\\033[?1049h\\033[2J\\033[H'\ncat '{}'\nsleep 60\nexit 0\n",
        screen.display()
    );
    std::fs::write(path, body).expect("write the shim");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Where a shim under test lives: the instance's managed atpkg prefix (its
/// scratch HOME's default, `atpkg::store::default_prefix`), so
/// `agents/claude` is the prefix's own twin path — the only agent-named
/// script the server reads as the agent.
fn shim_root(inst: &Instance) -> PathBuf {
    atpkg::store::default_prefix(&inst.tmp.join("home"))
}

/// Start `name` — under the managed prefix ([`shim_root`]) when `managed`,
/// else under a scratch directory of the user's own — in the boot session
/// and wait for the screen to show the 529.
fn run_shim(name: &str, managed: bool, tag: &str) -> Option<(Instance, String)> {
    let inst = boot(tag)?;
    let (_local, sid) = boot_session(&inst);
    let screen = inst.tmp.join("screen.txt");
    std::fs::write(&screen, screen_529()).expect("write the screen");
    let root = if managed {
        shim_root(&inst)
    } else {
        inst.tmp.join("home/bin")
    };
    let shim = root.join(name);
    std::fs::create_dir_all(shim.parent().expect("a parent")).expect("mkdir");
    write_shim(&shim, &screen);
    type_line(&inst, &sid, &format!("'{}'", shim.display()));
    let reply = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "API.Error:.529",
            "timeout=10000",
        ],
    );
    assert!(
        reply.starts_with("OK") && !reply.starts_with("OK timeout"),
        "the 529 screen never drew: {reply}"
    );
    Some((inst, sid))
}

#[test]
fn a_shim_before_its_exec_is_the_agent_and_its_529_is_a_wall() {
    let Some((inst, sid)) = run_shim("agents/claude", true, "w") else {
        return;
    };
    let rec = status_until(
        &inst,
        &sid,
        Duration::from_secs(10),
        "program=claude agent=wall:overloaded",
        |s| field(s, "program") == Some("claude") && field(s, "agent") == Some("wall:overloaded"),
    );
    assert_eq!(field(&rec, "agent_detail"), Some("-"), "{rec}");
    assert_eq!(field(&rec, "level"), Some("limited"), "{rec}");
    // Latched: the verdict is already true, so both answer at once.
    for word in ["wall:overloaded", "wall", "busy,wall:overloaded"] {
        let reply = ctl_ok(
            &inst,
            &[&format!("@{sid}"), "await", "agent", word, "timeout=3000"],
        );
        assert!(
            reply.starts_with("OK agent wall:overloaded rev="),
            "{word}: {reply}"
        );
    }
    // NEGATIVE CONTROL for the await: a wall kind the screen is not.
    let reply = ctl(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "agent",
            "wall:usage-session",
            "timeout=300",
        ],
    );
    let said = String::from_utf8_lossy(&reply.stdout);
    assert!(said.starts_with("OK timeout"), "{said}");
    // The roster says the same.
    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster.lines().find(|l| l.contains(&sid)).expect("row");
    assert_eq!(field(row, "program"), Some("claude"), "{row}");
    assert_eq!(field(row, "agent"), Some("wall:overloaded"), "{row}");
}

/// NEGATIVE CONTROL: the very same script, named `build.sh`, is a shell
/// running a script — `program=sh`, never an agent, whatever it draws.
#[test]
fn the_same_script_under_another_name_is_a_shell() {
    let Some((inst, sid)) = run_shim("build.sh", true, "n") else {
        return;
    };
    let rec = status_until(&inst, &sid, Duration::from_secs(10), "program=sh", |s| {
        field(s, "program") == Some("sh")
    });
    // Sample for a second: never an agent.
    let until = Instant::now() + Duration::from_secs(1);
    let mut last = rec;
    while Instant::now() < until {
        assert_eq!(field(&last, "agent"), Some("-"), "{last}");
        std::thread::sleep(Duration::from_millis(200));
        last = status(&inst, &sid);
    }
}

/// NEGATIVE CONTROL: a script NAMED `claude` that is not atpkg's shim — a
/// user's own `~/bin/claude` wrapper — is a shell running a script too, so the
/// in-GUI supervisor never attaches a pressing loop to whatever it runs.
#[test]
fn a_wrapper_named_claude_outside_the_prefix_is_a_shell() {
    let Some((inst, sid)) = run_shim("claude", false, "u") else {
        return;
    };
    let rec = status_until(&inst, &sid, Duration::from_secs(10), "program=sh", |s| {
        field(s, "program") == Some("sh")
    });
    assert_eq!(field(&rec, "agent"), Some("-"), "{rec}");
}
