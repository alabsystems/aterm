// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `--headless` launch never becomes an app: not one that can take the keyboard, and not one
//! LaunchServices or the WindowServer knows. Every conformance probe and headless test launches
//! the binary, dozens of times per run; on the AppKit loop winit makes an unbundled binary a
//! regular app and activates it, stealing keyboard focus. Headless runs the display-free loop
//! (`crates/aterm-window/src/platform_impl/macos/headless.rs`), which never creates the
//! application object, so LaunchServices holds no record for the pid: `policy=none`. An
//! `NSApplication` (the AppKit loop, even under a `Prohibited` posture) would check the process
//! in with LaunchServices and open a WindowServer connection, whose synchronous TCC preflight
//! runs on WindowServer's main thread (AGENTS.md); `window_server_census_live_headless.rs` reads
//! that half from tccd's log. Only a test outside the process can see either.
//!
//! Non-vacuity: the control socket binds before the loop starts, so a probe fired as soon as it
//! appears could read the pre-launch state. This first drives `aterm ctl spawn`, a
//! `Wake::SpawnSession` round trip only `user_event` answers, and probes only after that reply,
//! three times over a second. Between the two it drives `aterm ctl chrome`, which reads the
//! menu bar: through AppKit, `+[NSApplication sharedApplication]` there would create the
//! application object the loop never built.
//!
//! Isolation and boot are `support/headless_boot.rs`'s and `support/launch_isolation.rs`'s,
//! every client call pinned with `--sock`; this file's boots carry their own `athl` prefix.

#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::{headless_boot, launch_isolation};

use headless_boot::{Instance, log_tail};

/// Boot one real headless instance under the scratch world, 40x120
/// ([`headless_boot::boot`]: an environment refusal — no scratch base, no log, no
/// spawn, or a bind the OS refuses — is COULD NOT RUN; an instance that exits
/// or never listens FAILS the test, a startup regression rather than a skip).
fn boot() -> Instance {
    headless_boot::boot("athl", &["--lines", "40", "--columns", "120"])
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// A `swift -e` fallback can spend 5-15 s building its module cache cold.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// `activateIgnoringOtherApps:` lands through the WindowServer asynchronously:
/// three samples half a second apart keep a late-landing theft observable.
const PROBE_SAMPLES: usize = 3;
const PROBE_GAP: Duration = Duration::from_millis(500);

/// The hermetic environment shared by the server and every client call
/// ([`launch_isolation::apply`]): no inherited `ATERM_*`, so the `--headless` flag is what
/// arms the mode, and HOME and every XDG root in the scratch world.
fn hermetic_env(cmd: &mut Command, tmp: &Path) {
    launch_isolation::apply(cmd, tmp);
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

/// Run one `aterm <args…>` client call against the instance, hermetically, asserting success
/// and returning stdout. The first call is the anchor that needs the event loop running, which
/// under load can start taking connections later than the client's 10 s accept deadline
/// ([`headless_boot::until_served`]); any other failure ends the test.
fn client_ok(inst: &Instance, args: &[&str]) -> String {
    let out = headless_boot::until_served(|| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hermetic_env(&mut cmd, &inst.tmp);
        let child = cmd.spawn().expect("spawn the aterm client");
        bounded_wait(child, &format!("aterm {args:?}"), CLIENT_EXIT_DEADLINE)
            .unwrap_or_else(|e| panic!("{e}"))
    });
    assert!(
        out.status.success(),
        "aterm {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("client stdout is UTF-8")
}

/// The probe reads four facts through AppKit and prints one line:
/// `policy=<0|1|2|none> frontmost=<pid|-1> frontpolicy=<0|1|2|none> active=<0|1>`. Two
/// spellings of one program: JXA's ObjC bridge needs no compile step (~0.3 s), and the
/// `/usr/bin/swift -e` twin is the fallback (seconds, more on a cold module cache).
/// `policy=none`: LaunchServices holds no app record for the pid. `frontpolicy` is the same
/// lookup for the frontmost app's pid, the positive control: a lookup that answered `none` for
/// every pid would read every instance as clean.
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
    var lookup = function (p) {
        var a = $.NSRunningApplication.runningApplicationWithProcessIdentifier(p);
        return a.isNil() ? "none" : String(a.activationPolicy);
    };
    var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
    var policy = lookup(pid);
    var frontPolicy = frontPid === -1 ? "none" : lookup(frontPid);
    var active = (!app.isNil() && app.active) ? "1" : "0";
    return "policy=" + policy + " frontmost=" + frontPid + " frontpolicy=" + frontPolicy +
        " active=" + active;
}
"#;

const PROBE_SWIFT: &str = r#"
import AppKit
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
let frontPid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
func lookup(_ p: pid_t) -> String {
    guard p != -1, let a = NSRunningApplication(processIdentifier: p) else { return "none" }
    return String(a.activationPolicy.rawValue)
}
let active = NSRunningApplication(processIdentifier: pid)?.isActive ?? false
print("policy=\(lookup(pid)) frontmost=\(frontPid) frontpolicy=\(lookup(frontPid)) active=\(active ? 1 : 0)")
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
    /// The same lookup as `policy`, for `frontmost`'s pid: the positive control.
    front_policy: Option<isize>,
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
    let policy_of = |key: &str| -> Result<Option<isize>, String> {
        match field(key)? {
            "none" => Ok(None),
            n => n
                .parse::<isize>()
                .map(Some)
                .map_err(|e| format!("{key}{n:?}: {e}")),
        }
    };
    Ok(Posture {
        policy: policy_of("policy=")?,
        frontmost: field("frontmost=")?
            .parse()
            .map_err(|e| format!("frontmost: {e}"))?,
        front_policy: policy_of("frontpolicy=")?,
        active: field("active=")? == "1",
    })
}

/// The one honest reason this test cannot run at all, COULD NOT RUN: no
/// probe tool. (`verify --no-windows` is the windowed checks' opt-out; this
/// test opens no window, so it has none.) A missing WindowServer session
/// (an SSH login, a CI box) is deliberately NOT a skip — `ioreg -c IOHIDSystem`
/// exits 0 whether or not the class matches, so the `gui_smoke_unavailable`
/// rung it would copy never fires, and skipping on `SSH_CONNECTION` would
/// silently un-guard the regression in a developer's tmux or ssh shell on a
/// Mac that does have one. Without a WindowServer the probe reads
/// `frontmost=-1`, the policy half still runs; an instance that cannot start
/// FAILS in `boot()`, and one whose log carries AppKit's refusal to connect to
/// the WindowServer fails there naming that door, since a headless instance
/// never asks for the connection (`support/headless_boot.rs`); the test says
/// which case it saw.
fn cannot_present() -> Option<String> {
    if probe_tool().is_none() {
        return Some("neither /usr/bin/osascript nor /usr/bin/swift is available".into());
    }
    None
}

/// A headless instance, its loop running (proved by the `spawn` round trip), is no app
/// LaunchServices knows, is not active, and is not what the human is looking at.
#[test]
fn a_headless_launch_is_not_an_activatable_app_and_never_takes_the_front() {
    if let Some(why) = cannot_present() {
        aterm_spec::host::could_not_run(why);
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
             a CI box?); the frontmost half of the check is vacuous here, and the policy half \
             runs without its positive control"
        );
    }

    let inst = boot();
    let sock = inst.sock.clone();
    let pid = inst.child.id();

    // The anchor: `spawn` is `call_main(proxy, Wake::SpawnSession { .. })`, answered only from
    // `user_event`, which no loop delivers before it is running (nor the AppKit loop before
    // `did_finish_launching` checked the process in). A probe before this reply proves nothing.
    let spawned = client_ok(&inst, &["ctl", "--sock", &sock, "spawn"]);
    assert!(
        spawned.starts_with("OK s-"),
        "spawn replies `OK <sid>`: {spawned:?}"
    );
    // `chrome` reads the menu bar. Through AppKit that read begins with
    // `+[NSApplication sharedApplication]`, which would create the application object, and its
    // LaunchServices record, in a process whose display-free loop never built one; headless
    // answers from `App::app_menu_bar`'s inert arm. Its text is checked after the probes.
    let chrome = client_ok(&inst, &["ctl", "--sock", &sock, "chrome"]);

    let mut last = None;
    for sample in 0..PROBE_SAMPLES {
        if sample > 0 {
            std::thread::sleep(PROBE_GAP);
        }
        let got = match probe(&tool, pid) {
            Ok(p) => p,
            Err(e) => panic!("{e}\ninstance log tail:\n{}", log_tail(&inst.log)),
        };
        // The positive control first: the lookup that reads the instance's
        // `none` must find the app that is in front, or `none` proves nothing.
        assert!(
            got.frontmost == -1 || got.front_policy.is_some(),
            "sample {sample}: the probe's LaunchServices lookup found no app record for the \
             frontmost app (pid {}), so its answer for the instance proves nothing — a probe \
             problem, not a verdict about aterm; posture={got:?}",
            got.frontmost
        );
        match got.policy {
            Some(0) => panic!(
                "sample {sample}: aterm --headless (pid {pid}) registered as a REGULAR app — the \
                 Dock-visible, self-activating posture that steals focus; posture={got:?}"
            ),
            Some(p) => panic!(
                "sample {sample}: aterm --headless (pid {pid}) is an app LaunchServices knows \
                 (activationPolicy {p}): it built an NSApplication, whose initialisation opens \
                 the WindowServer connection a headless process must never open (AGENTS.md) \
                 — the display-free loop was not selected, or a verb (`chrome` above) created \
                 the application object; posture={got:?}"
            ),
            None => {}
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
    assert!(
        chrome.lines().any(|l| l == "menu (none)"),
        "a headless instance has no menu bar, and says so without asking AppKit: {chrome:?}"
    );

    // The proof-it-ran marker: a skipped run never prints this line.
    eprintln!(
        "headless activation: pid {pid} policy={:?} frontmost={} (policy {:?}; before {before}) \
         at {sock}",
        got.policy, got.frontmost, got.front_policy
    );
}
