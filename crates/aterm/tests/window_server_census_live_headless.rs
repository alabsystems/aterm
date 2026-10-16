// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `--headless` instance must never become a WindowServer client — read from
//! tccd's own log, which is the only place the fact is recorded.
//!
//! WHY THIS IS THE CHECK. When a process opens its WindowServer connection,
//! WindowServer asks tccd for a synchronous `kTCCServiceListenEvent` preflight
//! of that binary ON WINDOWSERVER'S MAIN THREAD, and tccd `readdir`s the
//! binary's directory to identify it. From a binary in a huge build directory
//! that outran WindowServer's watchdog and killed every window on the machine,
//! twice (AGENTS.md). Until 2026-09-26 every `aterm --headless` launch on
//! macOS did it: the event loop was AppKit's, and `-[NSApplication init]` opens
//! the connection. An idle `aterm --headless --cpu` drew one preflight within
//! two seconds; the live-headless suites in this directory drew 50 in one run.
//! Headless now builds winit's display-free loop
//! (`crates/aterm-window/src/platform_impl/macos/headless.rs`). Nothing inside the
//! process can say whether it connected without asking WindowServer itself, so
//! this reads the preflights back out of the unified log, by pid:
//!
//! ```text
//! log show --start <T0> --info --predicate 'process == "tccd"
//!   AND eventMessage CONTAINS "requesting={TCCDProcess: identifier=com.apple.WindowServer"'
//! ```
//!
//! and fails when any line's `accessing=` process is the instance's pid. The
//! query names no line kind: tccd writes one preflight as several lines, and
//! which ones varies — macOS 26.6 (25G83) writes two Info `AttributionChain:`
//! lines per request and only sometimes a Default `AUTHREQ_ATTRIBUTION:` line
//! (measured 2026-10-08; a census that required `AUTHREQ_ATTRIBUTION` missed
//! most controls and said `CENSUS NOT RUN`). Every such line carries the same
//! `accessing=` pid, so the pids are de-duplicated.
//!
//! NON-VACUITY: THE POSITIVE CONTROL. An empty census proves nothing unless the
//! census can see a preflight at all here: `log` may be unreadable, there may be
//! no WindowServer session (an SSH login, a CI box), or a macOS release may
//! reword the line. So after the instance is gone, `/usr/bin/osascript` calls
//! `CGMainDisplayID()` — measured (2026-09-26) to draw exactly this preflight for
//! osascript's pid — and the census must find THAT pid before the instance's
//! absence counts. The control runs LAST, so seeing it also means the log had
//! caught up with everything the instance did before it. `/usr/bin` is a small
//! directory, and osascript is the tool the activation test already runs. When
//! the control cannot be seen the test says `CENSUS NOT RUN`, uncaptured, and
//! returns: nothing was decided. libtest still reports the test `ok`, so that
//! notice is the record that it decided nothing (AGENTS.md: a notice, never a
//! silent ok).
//!
//! A REWORDED LINE FAILS. Before it says NOT RUN, the census asks tccd's log
//! once more without its own predicate or parser, for any line naming both
//! WindowServer and the control's pid. If tccd recorded the control and the
//! census still cannot read it, the census is stale for this macOS, and that
//! fails, quoting the lines. A live WindowServer session alone (a frontmost
//! app) is not grounds to fail: tccd's lines sit in the log's memory buffers
//! and were measured gone from `log show` within minutes (2026-10-08), so an
//! unseen control cannot tell a stale reader from a line the log dropped.
//!
//! THE INSTANCE is driven, not merely idled: a `spawn` round trip (a user event
//! through the loop), an `image` (the offscreen CPU render and font path), a
//! held message (the message band's timers), a `chrome` read (the one verb that
//! asked AppKit for the menu bar — see below), then three idle seconds, so a door
//! on a lazy or timer path gets its chance.
//!
//! THE SECOND DOOR, `chrome`. With the display-free loop in place, a sweep of
//! the control verbs against a headless instance (fresh instance per door,
//! `DYLD_INSERT_LIBRARIES` on SkyLight's connection entry points) found exactly
//! one more: `read_native_chrome` read the menu bar through
//! `+[NSApplication sharedApplication]`, which CREATES the application object the
//! loop no longer built — and one `aterm ctl chrome` drew one preflight. It now
//! reads through the injected `App::app_menu_bar` (`menu (none)` headless), and
//! this drive fails the day a verb brings the application object back.
//!
//! ISOLATION is `headless_activation.rs`'s (the shared
//! `support/launch_isolation.rs` fixture, `--sock` on every call, a lifeline),
//! under its own `atws-<pid>` scratch name, and the BOOT is the shared
//! `support/headless_boot.rs`: an instance that cannot start FAILS (and one
//! whose log shows it asked for a WindowServer connection fails naming that);
//! a scratch, log, spawn or bind refusal is `CENSUS NOT RUN`, uncaptured.

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

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// Long enough for a door on a timer or a lazily started worker to open.
const IDLE: Duration = Duration::from_secs(3);
/// How long the census waits for the positive control's line to reach the log:
/// a hang detector for a line that must arrive, never a latency budget
/// (AGENTS.md: a minute), so a lagging unified log on a loaded machine is waited
/// out rather than turned into a `CENSUS NOT RUN`.
const CONTROL_VISIBLE_DEADLINE: Duration = Duration::from_secs(60);
const LOG_SHOW_DEADLINE: Duration = Duration::from_secs(60);

/// The unified-log predicate: every line tccd writes that names WindowServer as
/// a TCC preflight's requester, whatever its kind (`AttributionChain:`,
/// `AUTHREQ_ATTRIBUTION:`). A string constant so the prose above and the query
/// cannot drift.
const PREDICATE: &str = "process == \"tccd\" \
                         AND eventMessage CONTAINS \"requesting={TCCDProcess: identifier=com.apple.WindowServer\"";

/// A notice written straight to this process's stderr handle, not through
/// `eprintln!`: libtest captures the print macros of a PASSING test and throws
/// the text away, so a `CENSUS NOT RUN` (or a SKIP) would vanish from a green
/// run — exactly the silent pass it exists to prevent (the `aterm_spec::verify`
/// provenance line's reason and shape).
fn notice(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Boot the instance under the shared fixture: CPU rendering, 24x80.
fn boot() -> Option<Instance> {
    headless_boot::boot("atws", &["--cpu", "--lines", "24", "--columns", "80"])
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

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

fn client_ok(inst: &Instance, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.args(["ctl", "--sock", &inst.sock])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    let child = cmd.spawn().expect("spawn the aterm client");
    let out = bounded_wait(child, &format!("aterm ctl {args:?}"), CLIENT_EXIT_DEADLINE)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}\ninstance log tail:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        log_tail(&inst.log),
    );
    String::from_utf8(out.stdout).expect("client stdout is UTF-8")
}

/// Local wall-clock `YYYY-MM-DD HH:MM:SS`, the shape `log show --start` reads.
/// `/bin/date` rather than arithmetic here: the log speaks local time.
fn log_timestamp() -> Option<String> {
    let out = Command::new("/bin/date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The unified log since `start` under `predicate`, or `Err` when it could not
/// be read at all.
fn log_show(start: &str, predicate: &str) -> Result<String, String> {
    let child = Command::new("/usr/bin/log")
        .args(["show", "--start", start, "--info", "--predicate", predicate])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn /usr/bin/log: {e}"))?;
    let out = bounded_wait(child, "log show", LOG_SHOW_DEADLINE)?;
    if !out.status.success() {
        return Err(format!(
            "log show exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every pid tccd recorded a WindowServer-requested preflight for since
/// `start`, or `Err` when the log could not be read at all.
fn preflighted_pids(start: &str) -> Result<Vec<u32>, String> {
    log_show(start, PREDICATE).map(|log| accessing_pids(&log))
}

/// Every line tccd wrote since `start` that names both WindowServer and `pid`,
/// read without [`PREDICATE`] or [`accessing_pids`]: what this macOS recorded
/// for the positive control, in whatever shape it now writes it.
fn lines_naming_window_server_and(start: &str, pid: u32) -> Result<Vec<String>, String> {
    let field = format!("pid={pid},");
    let predicate = format!(
        "process == \"tccd\" AND eventMessage CONTAINS \"WindowServer\" \
         AND eventMessage CONTAINS \"{field}\""
    );
    Ok(log_show(start, &predicate)?
        .lines()
        .filter(|l| l.contains(&field))
        .map(str::to_owned)
        .collect())
}

/// The `pid=` of each line's `accessing={TCCDProcess: …}` — the process being
/// preflighted, never the `responsible=` one or WindowServer's own — sorted and
/// de-duplicated, since tccd writes several lines for one preflight.
fn accessing_pids(log: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = log
        .lines()
        .filter(|l| l.contains("requesting={TCCDProcess: identifier=com.apple.WindowServer"))
        .filter_map(|l| {
            let rest = &l[l.find("accessing={TCCDProcess:")?..];
            let rest = &rest[..rest.find('}')?];
            let pid = rest
                .split(", ")
                .find_map(|f| f.trim().strip_prefix("pid="))?;
            pid.parse().ok()
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// The positive control: a CoreGraphics display query from osascript, whose pid
/// the census must then find. `None` when osascript is unavailable or failed.
fn positive_control() -> Option<u32> {
    let child = Command::new("/usr/bin/osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            "ObjC.import('CoreGraphics'); String($.CGMainDisplayID())",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let pid = child.id();
    let out = bounded_wait(child, "the osascript control", Duration::from_secs(60)).ok()?;
    out.status.success().then_some(pid)
}

#[test]
fn a_driven_headless_instance_draws_no_window_server_preflight() {
    if !Path::new("/usr/bin/log").exists() || !Path::new("/usr/bin/osascript").exists() {
        notice(
            "CENSUS NOT RUN: /usr/bin/log or /usr/bin/osascript is missing; nothing was decided",
        );
        return;
    }
    let Some(start) = log_timestamp() else {
        notice("CENSUS NOT RUN: /bin/date could not name the census start; nothing was decided");
        return;
    };
    let Some(inst) = boot() else {
        notice(
            "CENSUS NOT RUN: the instance could not boot for an environment reason (the SKIP \
             above); nothing was decided",
        );
        return;
    };
    let pid = inst.child.id();

    let spawned = client_ok(&inst, &["spawn"]);
    assert!(
        spawned.starts_with("OK s-"),
        "spawn replies `OK <sid>`: {spawned:?}"
    );
    let image = client_ok(&inst, &["image", "census.png"]);
    assert!(image.starts_with("OK "), "image: {image:?}");
    let posted = client_ok(
        &inst,
        &[
            "notice",
            "post",
            "script",
            "sev=warn",
            "key=census",
            "census",
            "--",
            "held",
        ],
    );
    assert!(posted.starts_with("OK "), "notice: {posted:?}");
    // The menu-bar read. What it answers is checked after the census, which is
    // this test's verdict: a door here must fail as a preflight, not as text.
    let chrome = client_ok(&inst, &["chrome"]);
    std::thread::sleep(IDLE);
    let log = inst.log.clone();
    let tail_before_exit = log_tail(&log);
    drop(inst);

    let Some(control) = positive_control() else {
        notice("CENSUS NOT RUN: the osascript positive control did not run; nothing was decided");
        return;
    };
    let until = Instant::now() + CONTROL_VISIBLE_DEADLINE;
    let pids = loop {
        match preflighted_pids(&start) {
            Ok(pids) if pids.contains(&control) => break pids,
            Ok(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(500)),
            Ok(pids) => {
                // Did tccd record the control in a shape the census cannot read?
                // Re-read the census after the raw lines, so a line that landed
                // between the two queries decides instead of failing.
                let raw = lines_naming_window_server_and(&start, control);
                match (raw, preflighted_pids(&start)) {
                    (_, Ok(now)) if now.contains(&control) => break now,
                    (Ok(raw), Ok(_)) if !raw.is_empty() => panic!(
                        "tccd logged the positive control's WindowServer preflight (osascript pid \
                         {control}), but the census's PREDICATE and `accessing_pids` do not read \
                         it: this macOS writes another line shape, and until the census reads it \
                         it can decide nothing. tccd's lines:\n{}",
                        raw.join("\n")
                    ),
                    _ => {}
                }
                notice(&format!(
                    "CENSUS NOT RUN: tccd's log never showed the positive control (osascript pid \
                     {control}) within {}s — no WindowServer session here, or the log dropped or \
                     reworded the line; saw preflights for {pids:?}. Nothing was decided.",
                    CONTROL_VISIBLE_DEADLINE.as_secs()
                ));
                return;
            }
            Err(e) => {
                notice(&format!(
                    "CENSUS NOT RUN: the unified log could not be read ({e}); nothing was decided"
                ));
                return;
            }
        }
    };
    assert!(
        !pids.contains(&pid),
        "aterm --headless (pid {pid}) made WindowServer preflight it: a headless process opened \
         a WindowServer connection (AGENTS.md). Find the door with the instance's own log \
         (`log show --predicate 'processID == {pid}'`: the SkyLight `New connection` line and \
         what precedes it). Instance log tail:\n{tail_before_exit}"
    );
    assert!(
        chrome.lines().any(|l| l == "menu (none)"),
        "a headless instance has no menu bar, and says so without asking AppKit: {chrome:?}"
    );
    // The proof-it-ran marker: a NOT RUN never prints this line.
    notice(&format!(
        "window-server census: pid {pid} drew 0 WindowServer preflights; the osascript control \
         (pid {control}) was seen, as were {} other preflighted pid(s) since {start}",
        pids.len().saturating_sub(1)
    ));
}
