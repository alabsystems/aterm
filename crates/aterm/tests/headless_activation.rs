// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `--headless` launch must never become an app at all — not one that can
//! take the keyboard, and not one LaunchServices or the WindowServer knows.
//!
//! HISTORY, two rungs. Until 2026-09-21 `aterm --headless` built the real
//! AppKit event loop, and for an UNBUNDLED binary winit's
//! `applicationDidFinishLaunching:` made it a REGULAR app
//! (`setActivationPolicy:`) and sent `activateIgnoringOtherApps:YES`. Every
//! conformance probe and every headless test in this directory launches that
//! binary, dozens of times per `tools/verify.sh` run, and each launch yanked
//! keyboard focus from whatever the developer was typing into while owning no
//! window at all. `aterm_gui::launch_posture` made it `Prohibited`. That still
//! built an `NSApplication`, whose initialisation checks the process in with
//! LaunchServices and opens its WindowServer connection — which makes
//! WindowServer run a synchronous TCC preflight of the binary on its main
//! thread (AGENTS.md rule 5). Since 2026-09-26 headless builds winit's
//! display-free loop (`vendor/winit/src/platform_impl/macos/headless.rs`), which
//! never creates the application object, so LaunchServices holds NO record for
//! the pid: `policy=none`. That is what this pins now; the `Prohibited` value it
//! pinned before is what a regression back to the AppKit loop would read, and it
//! fails here. (`window_server_census_live_headless.rs` reads the WindowServer
//! half from tccd's log.) This is the only kind of test that can see it, because
//! the effect lives in LaunchServices and the WindowServer, not inside the
//! process.
//!
//! NON-VACUITY. LaunchServices check-in happened inside `-[NSApplication init]`
//! and the posture inside `applicationDidFinishLaunching:`, and the control
//! socket binds BEFORE the loop starts. A probe fired the moment the socket
//! appears could read the pre-launch state and pass against the broken build.
//! So this first drives `aterm ctl spawn` — a `Wake::SpawnSession` round trip
//! that only `user_event` can answer, which no loop delivers before it is
//! running — and probes only after that reply, three times over a second, so a
//! registration or activation that lands a beat late is still caught. Between
//! the two it drives `aterm ctl chrome`, the one verb that read AppKit's menu
//! bar: `+[NSApplication sharedApplication]` there would create the application
//! object (and its LaunchServices record) the loop never built.
//!
//! ISOLATION and BOOT discipline are the shared fixtures' (`support/headless_boot.rs`): the
//! shared `support/launch_isolation.rs` fixture (a private HOME and every XDG
//! root, no inherited `ATERM_*`/`ATPKG_*`, the auto-update / priming / packages
//! fences, `SHELL=/bin/sh`), and every client call pinned with `--sock`. That
//! harness keys its scratch dir on the test-binary pid, so this file's boots
//! carry their own `athl` prefix, distinct from every other file in the suite.

#![cfg(target_os = "macos")]

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::{headless_boot, launch_isolation};

use headless_boot::{Instance, log_tail};

/// Boot one real headless instance under the scratch world, 40x120
/// ([`headless_boot::boot`]: `None` is an environment refusal — no scratch base,
/// no log, no spawn, or a bind the OS refuses; an instance that exits or never
/// listens FAILS the test, a startup regression rather than a skip).
fn boot() -> Option<Instance> {
    headless_boot::boot("athl", &["--lines", "40", "--columns", "120"])
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// A `swift -e` fallback can spend 5-15 s building its module cache cold; GENEROUS.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// `activateIgnoringOtherApps:` lands through the WindowServer asynchronously:
/// three samples half a second apart keep a late-landing theft observable.
const PROBE_SAMPLES: usize = 3;
const PROBE_GAP: Duration = Duration::from_millis(500);

/// The hermetic environment shared by the server and every client call: the
/// fixture strips every inherited `ATERM_*` (so the `--headless` FLAG — the
/// spelling every probe script uses — is what arms the mode) and points HOME
/// and every XDG root into the scratch world.
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

/// Run one `aterm <args…>` client call against the instance, hermetically,
/// asserting success and returning stdout as UTF-8. The first call of this file is
/// the anchor that needs the event loop running, and on a machine under a build
/// storm the loop can take longer than the client's 10 s accept deadline to start
/// taking connections ([`headless_boot::until_accepted`]; seen 2026-10-09, 3 times in
/// sixteen gate-like rounds). Any other failure ends the test at once.
fn client_ok(inst: &Instance, args: &[&str]) -> String {
    let out = headless_boot::until_accepted(|| {
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

/// The probe reads four facts through AppKit and prints ONE line:
/// `policy=<0|1|2|none> frontmost=<pid|-1> frontpolicy=<0|1|2|none> active=<0|1>`.
/// Two spellings of the same program: JXA's ObjC bridge needs no compile step
/// (~0.3 s wall, measured on macOS 13.7), and the `/usr/bin/swift -e` twin — the
/// `aterm-verify` `smoke.rs` pattern — is the fallback (seconds per run, more on
/// a cold module cache). `policy=none` means LaunchServices holds no app record
/// for the pid. `frontpolicy` is the same lookup for the FRONTMOST app's pid, the
/// positive control: a lookup that answered `none` for every pid would read every
/// headless instance as clean, so it must find the app that is in front.
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

/// The one honest reason this test cannot run at all, as a SKIP reason: no
/// probe tool. (The gate's `--no-windows` is the windowed stages' opt-out;
/// this test opens no window, so it has none — until 2026-09-24 it read the
/// `ATERM_SKIP_GUI_SMOKE` export a flag replaced.) A missing WindowServer session
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

/// The regression, live: a headless instance, its loop running (proved by the
/// `spawn` round trip), is no app LaunchServices knows, is not active, and is
/// not what the human is looking at.
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
             a CI box?); the frontmost half of the check is vacuous here, and the policy half \
             runs without its positive control"
        );
    }

    let Some(inst) = boot() else { return };
    let sock = inst.sock.clone();
    let pid = inst.child.id();

    // THE ANCHOR. `spawn` is `call_main(proxy, Wake::SpawnSession { .. })`,
    // answered only from `user_event`; no loop delivers a user event before it
    // is running — and the AppKit loop, if headless ever built it again, not
    // before `did_finish_launching` had checked the process in, set the policy
    // and sent (or not sent) the activation. A probe before this reply proves
    // nothing.
    let spawned = client_ok(&inst, &["ctl", "--sock", &sock, "spawn"]);
    assert!(
        spawned.starts_with("OK s-"),
        "spawn replies `OK <sid>`: {spawned:?}"
    );
    // `chrome` reads the menu bar. Through AppKit that read begins with
    // `+[NSApplication sharedApplication]`, which would CREATE the application
    // object — and check the process in with LaunchServices, which the probes
    // below then see — in a process whose display-free loop never built one
    // (2026-09-26: the loop fix alone left this door open). Headless answers
    // from `App::app_menu_bar`'s inert arm instead. Its text is checked after
    // the probes, which are this test's verdict.
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
                 the WindowServer connection a headless process must never open (AGENTS.md rule \
                 5) — the display-free loop was not selected, or a verb (`chrome` above) created \
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
