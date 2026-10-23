// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies `--headless` launches never register with LaunchServices or WindowServer.

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

/// Boot one real 40x120 headless instance ([`headless_boot::boot`]).
fn boot() -> Instance {
    headless_boot::boot("athl", &["--lines", "40", "--columns", "120"])
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// A `swift -e` fallback can spend 5-15 s building its module cache cold.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// `activateIgnoringOtherApps:` lands through the WindowServer asynchronously.
const PROBE_SAMPLES: usize = 3;
const PROBE_GAP: Duration = Duration::from_millis(500);

/// Applies the hermetic scratch environment ([`launch_isolation::apply`]).
fn hermetic_env(cmd: &mut Command, tmp: &Path) {
    launch_isolation::apply(cmd, tmp);
}

/// Drains a child pipe on a dedicated background thread.
fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Wait for `child` with a deadline.
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

/// Runs an isolated `aterm` client command against the instance and returns stdout.
fn client_ok(inst: &Instance, args: &[&str]) -> String {
    let out = headless_boot::until_served(|| {
        let mut cmd = Command::new(launch_isolation::aterm_executable());
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

/// The probe reads four facts through AppKit and prints one line.
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
    /// `NSRunningApplication.activationPolicy`.
    policy: Option<isize>,
    /// `NSWorkspace.frontmostApplication`'s pid, `-1` when there is none.
    frontmost: i64,
    /// The same lookup as `policy`, for `frontmost`'s pid: the positive control.
    front_policy: Option<isize>,
    /// `NSRunningApplication.isActive`.
    active: bool,
}

/// Run the probe, bounded, and parse its one line.
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

/// The one honest reason this test cannot run at all, COULD NOT RUN: no probe tool.
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

    let spawned = client_ok(&inst, &["ctl", "--sock", &sock, "spawn"]);
    assert!(
        spawned.starts_with("OK s-"),
        "spawn replies `OK <sid>`: {spawned:?}"
    );
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

    eprintln!(
        "headless activation: pid {pid} policy={:?} frontmost={} (policy {:?}; before {before}) \
         at {sock}",
        got.policy, got.frontmost, got.front_policy
    );
}
