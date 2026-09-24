// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `--headless` launch must never become an app that can take the keyboard.
//!
//! On macOS there is no display-free winit backend, so `aterm --headless`
//! builds the real AppKit event loop, and for an UNBUNDLED binary winit's
//! `applicationDidFinishLaunching:` made it a REGULAR app
//! (`setActivationPolicy:`) and sent `activateIgnoringOtherApps:YES`. Every
//! conformance probe and every headless test in this directory launches that
//! binary, dozens of times per `tools/verify.sh` run, and each launch yanked
//! keyboard focus from whatever the developer was typing into while owning no
//! window at all. `aterm_gui::launch_posture` is the fix; this is the only test
//! that can see it take effect, because the effect lives in LaunchServices and
//! the WindowServer, not inside the process.
//!
//! NON-VACUITY. The policy and the activation are applied INSIDE
//! `applicationDidFinishLaunching:`, before winit marks the loop running, and
//! the control socket binds BEFORE the loop starts. A probe fired the moment
//! the socket appears could read the pre-launch state and pass against the
//! broken build. So this first drives `aterm ctl spawn` — a `Wake::SpawnSession`
//! round trip that only `user_event` can answer, which winit does not deliver
//! until `did_finish_launching` has completed — and probes only after that
//! reply, three times over a second, so an activation that lands through the
//! WindowServer a beat late is still caught.
//!
//! ISOLATION and SKIP discipline are `conn_live_headless.rs`'s, compacted: the
//! shared `support/launch_isolation.rs` fixture (a private HOME and every XDG
//! root, no inherited `ATERM_*`/`ATPKG_*`, the auto-update / priming / packages
//! fences, `SHELL=/bin/sh`), and every client call pinned with `--sock`. That
//! harness keys its scratch dir on the test-binary pid, so two booting tests in
//! one binary would collide — which is why this is its own file, under its own
//! `athl-<pid>` name.

#![cfg(target_os = "macos")]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// The socket-bind budget for the headless boot (the verify smoke's 100 ms
/// grid, generous) and the per-client-call exit bound.
const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// A `swift -e` fallback can spend 5-15 s building its module cache cold; GENEROUS.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// `activateIgnoringOtherApps:` lands through the WindowServer asynchronously:
/// three samples half a second apart keep a late-landing theft observable.
const PROBE_SAMPLES: usize = 3;
const PROBE_GAP: Duration = Duration::from_millis(500);

/// `sockaddr_un.sun_path` is ~104 bytes on macOS; refuse bases that would
/// overflow it (with margin) instead of failing deep inside bind/connect.
const MAX_SOCK_PATH: usize = 100;

/// One booted headless instance plus its scratch world, torn down (kill, reap,
/// remove) on every exit path — Drop runs on panic too.
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

/// Whether `path` exists as a unix socket or a symlink (the `latest` alias).
fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Pick a scratch base whose socket path fits `sun_path`: `$TMPDIR` (via
/// `temp_dir`), else `/tmp`. `None` when neither fits — an environment refusal,
/// reported as a clean SKIP by the caller.
fn scratch_root() -> Option<PathBuf> {
    let name = format!("athl-{}", std::process::id());
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

/// The hermetic environment shared by the server and every client call: the
/// fixture strips every inherited `ATERM_*` (so the `--headless` FLAG — the
/// spelling every probe script uses — is what arms the mode) and points HOME
/// and every XDG root into the scratch world.
fn hermetic_env(cmd: &mut Command, tmp: &Path) {
    launch_isolation::apply(cmd, tmp);
}

/// Boot one real headless instance under the scratch world. `None` means the
/// binary cannot boot headless in this sandbox (announced as a SKIP with the
/// log tail) — reserved for environmental refusals.
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
    hermetic_env(&mut cmd, &tmp);
    // AFTER the fixture: `apply` strips every `ATERM_*`, explicit ones included.
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

/// Drain a child's pipe on its own thread so a chatty child can never wedge
/// the poll loop that bounds it.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Wait for `child` with a deadline; a hung child becomes an `Err` carrying
/// what it printed, never a hung harness.
fn bounded_wait(mut child: Child, what: &str, deadline: Duration) -> Result<Output, String> {
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let until = Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what} did not exit within {}s",
                    deadline.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("poll {what}: {e}")),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().expect("join the stdout drain"),
        stderr: stderr.join().expect("join the stderr drain"),
    })
}

/// Run one `aterm <args…>` client call against the instance, hermetically,
/// asserting success and returning stdout as UTF-8.
fn client_ok(inst: &Instance, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hermetic_env(&mut cmd, &inst.tmp);
    let child = cmd.spawn().expect("spawn the aterm client");
    let out = bounded_wait(child, &format!("aterm {args:?}"), CLIENT_EXIT_DEADLINE)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.status.success(),
        "aterm {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("client stdout is UTF-8")
}

/// The probe reads three facts through AppKit and prints ONE line:
/// `policy=<0|1|2|none> frontmost=<pid|-1> active=<0|1>`. Two spellings of the
/// same program: JXA's ObjC bridge needs no compile step (~0.3 s wall, measured
/// on macOS 13.7), and the `/usr/bin/swift -e` twin — the `aterm-verify`
/// `smoke.rs` pattern — is the fallback (seconds per run, more on a cold module
/// cache). `policy=none` means LaunchServices holds no app record for the pid.
enum ProbeTool {
    Jxa,
    Swift,
}

fn probe_tool() -> Option<ProbeTool> {
    if Path::new("/usr/bin/osascript").exists() {
        return Some(ProbeTool::Jxa);
    }
    if Path::new("/usr/bin/swift").exists() {
        return Some(ProbeTool::Swift);
    }
    None
}

const PROBE_JXA: &str = r#"
ObjC.import("AppKit");
function run(argv) {
    var pid = parseInt(argv[0], 10);
    var front = $.NSWorkspace.sharedWorkspace.frontmostApplication;
    var frontPid = front.isNil() ? -1 : front.processIdentifier;
    var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
    var policy = app.isNil() ? "none" : String(app.activationPolicy);
    var active = (!app.isNil() && app.active) ? "1" : "0";
    return "policy=" + policy + " frontmost=" + frontPid + " active=" + active;
}
"#;

const PROBE_SWIFT: &str = r#"
import AppKit
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
let frontPid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
if let app = NSRunningApplication(processIdentifier: pid) {
    print("policy=\(app.activationPolicy.rawValue) frontmost=\(frontPid) active=\(app.isActive ? 1 : 0)")
} else {
    print("policy=none frontmost=\(frontPid) active=0")
}
exit(0)
"#;

/// What LaunchServices and the WindowServer say about one pid.
#[derive(Debug)]
struct Posture {
    /// `NSRunningApplication.activationPolicy`; `None` when LaunchServices holds
    /// no app record for the pid at all.
    policy: Option<isize>,
    /// `NSWorkspace.frontmostApplication`'s pid, `-1` when there is none.
    frontmost: i64,
    /// `NSRunningApplication.isActive`.
    active: bool,
}

/// Run the probe, bounded, and parse its one line. `Err` is a harness failure
/// carrying the captured output — never a verdict about aterm.
fn probe(tool: &ProbeTool, pid: u32) -> Result<Posture, String> {
    let mut cmd = match tool {
        ProbeTool::Jxa => {
            let mut c = Command::new("/usr/bin/osascript");
            c.args(["-l", "JavaScript", "-e", PROBE_JXA])
                .arg(pid.to_string());
            c
        }
        ProbeTool::Swift => {
            let mut c = Command::new("/usr/bin/swift");
            c.arg("-e").arg(PROBE_SWIFT).arg(pid.to_string());
            c
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| format!("spawn the probe: {e}"))?;
    let out = bounded_wait(child, "the activation probe", PROBE_DEADLINE)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(format!(
            "the probe failed ({}): stdout={stdout:?} stderr={stderr:?}",
            out.status
        ));
    }
    let line = stdout
        .lines()
        .find(|l| l.starts_with("policy="))
        .ok_or_else(|| {
            format!("the probe printed no posture line: stdout={stdout:?} stderr={stderr:?}")
        })?;
    let field = |key: &str| -> Result<&str, String> {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(key))
            .ok_or_else(|| format!("no {key} in {line:?}"))
    };
    let policy = match field("policy=")? {
        "none" => None,
        n => Some(
            n.parse::<isize>()
                .map_err(|e| format!("policy {n:?}: {e}"))?,
        ),
    };
    Ok(Posture {
        policy,
        frontmost: field("frontmost=")?
            .parse()
            .map_err(|e| format!("frontmost: {e}"))?,
        active: field("active=")? == "1",
    })
}

/// The two honest reasons this test cannot run at all, as SKIP reasons: the
/// gate's explicit opt-out, and no probe tool. A missing WindowServer session
/// (an SSH login, a CI box) is deliberately NOT a skip — `ioreg -c IOHIDSystem`
/// exits 0 whether or not the class matches, so the `gui_smoke_unavailable`
/// rung it would copy never fires, and skipping on `SSH_CONNECTION` would
/// silently un-guard the regression in a developer's tmux or ssh shell on a
/// Mac that does have one. Without a WindowServer the probe reads
/// `frontmost=-1`, the policy half still runs, and `boot()` SKIPs by itself if
/// AppKit refuses to start; the test says which case it saw.
fn cannot_present() -> Option<String> {
    if std::env::var_os("ATERM_SKIP_GUI_SMOKE").is_some_and(|v| v == "1") {
        return Some("ATERM_SKIP_GUI_SMOKE=1".into());
    }
    if probe_tool().is_none() {
        return Some("neither /usr/bin/osascript nor /usr/bin/swift is available".into());
    }
    None
}

/// The regression, live: a headless instance, past
/// `applicationDidFinishLaunching:` (proved by the `spawn` round trip), is not
/// a Regular app, is not active, and is not what the human is looking at.
#[test]
fn a_headless_launch_is_not_an_activatable_app_and_never_takes_the_front() {
    if let Some(why) = cannot_present() {
        eprintln!("SKIP: {why}");
        return;
    }
    let tool = probe_tool().expect("cannot_present checked the tool");
    // Diagnostics only: the human may legitimately switch apps mid-test, so
    // the assertion below is `frontmost != child`, never `frontmost == before`.
    let before = probe(&tool, std::process::id())
        .map(|p| p.frontmost)
        .unwrap_or(-1);
    if before == -1 {
        eprintln!(
            "note: no front app is visible from this session (no WindowServer — an SSH login or \
             a CI box?); the frontmost half of the check is vacuous here, the policy half still runs"
        );
    }

    let Some(inst) = boot() else { return };
    let sock = inst.sock.clone();
    let pid = inst.child.id();

    // THE ANCHOR. `spawn` is `call_main(proxy, Wake::SpawnSession { .. })`,
    // answered only from `user_event`; winit delivers no user event before
    // `did_finish_launching` has set the policy, sent (or not sent) the
    // activation, and marked the loop running. A probe before this reply
    // proves nothing.
    let spawned = client_ok(&inst, &["ctl", "--sock", &sock, "spawn"]);
    assert!(
        spawned.starts_with("OK s-"),
        "spawn replies `OK <sid>`: {spawned:?}"
    );

    // The crate's own answer, so the pin follows the decision and cannot drift
    // from it — and the independent `!= Regular` line below is the incident
    // itself, kept even if the decision ever moves to Accessory.
    let expected = aterm_gui::launch_posture(true)
        .policy
        .expect("a headless launch names a policy")
        .ns_raw();

    let mut last = None;
    for sample in 0..PROBE_SAMPLES {
        if sample > 0 {
            std::thread::sleep(PROBE_GAP);
        }
        let got = match probe(&tool, pid) {
            Ok(p) => p,
            Err(e) => panic!("{e}\ninstance log tail:\n{}", log_tail(&inst.log)),
        };
        match got.policy {
            Some(0) => panic!(
                "sample {sample}: aterm --headless (pid {pid}) registered as a REGULAR app — the \
                 Dock-visible, self-activating posture that steals focus; posture={got:?}"
            ),
            Some(p) => assert_eq!(
                p, expected,
                "sample {sample}: the policy winit applied is the one aterm asked for; {got:?}"
            ),
            None => eprintln!(
                "note: sample {sample}: LaunchServices holds no app record for pid {pid} \
                 (stronger than not-Regular)"
            ),
        }
        assert!(
            !got.active,
            "sample {sample}: a headless instance must never be the active app: {got:?}"
        );
        assert_ne!(
            got.frontmost,
            i64::from(pid),
            "sample {sample}: a headless instance must never be frontmost (frontmost before \
             launch: {before}): {got:?}"
        );
        last = Some(got);
    }
    let got = last.expect("at least one sample");

    // The proof-it-ran marker: a skipped run never prints this line.
    eprintln!(
        "headless activation: pid {pid} policy={:?} frontmost={} (before {before}) at {sock}",
        got.policy, got.frontmost
    );
}
