// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `--headless` instance must never become a WindowServer client.

#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use aterm_spec::host::could_not_run;

use crate::{headless_boot, launch_isolation};

use headless_boot::{Instance, log_tail};

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(90);
/// Long enough for a door on a timer or a lazily started worker to open.
const IDLE: Duration = Duration::from_secs(3);
/// How long the census waits for the positive control's line to reach the log:
/// a hang detector for a line that must arrive, never a latency budget
/// (AGENTS.md: a minute), so a lagging unified log on a loaded machine is waited
/// out rather than turned into a COULD NOT RUN.
const CONTROL_VISIBLE_DEADLINE: Duration = Duration::from_secs(60);
const LOG_SHOW_DEADLINE: Duration = Duration::from_secs(60);

/// The unified-log predicate: every line tccd writes that names WindowServer as
/// a TCC preflight's requester, whatever its kind (`AttributionChain:`,
/// `AUTHREQ_ATTRIBUTION:`).
const PREDICATE: &str = "process == \"tccd\" \
                         AND eventMessage CONTAINS \"requesting={TCCDProcess: identifier=com.apple.WindowServer\"";

/// A notice written straight to this process's stderr handle, not through
/// `eprintln!`: libtest captures the print macros of a PASSING test and throws
/// the text away, so a proof-it-ran line would vanish from a green run — the
/// silent pass it exists to prevent (the `aterm_spec::verify`
/// provenance line's reason and shape).
fn notice(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Boot the instance under the shared fixture: CPU rendering, 24x80.
fn boot() -> Instance {
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
    let out = headless_boot::until_served(|| {
        let mut cmd = Command::new(launch_isolation::aterm_executable());
        cmd.args(["ctl", "--sock", &inst.sock])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        launch_isolation::apply(&mut cmd, &inst.tmp);
        let child = cmd.spawn().expect("spawn the aterm client");
        bounded_wait(child, &format!("aterm ctl {args:?}"), CLIENT_EXIT_DEADLINE)
            .unwrap_or_else(|e| panic!("{e}"))
    });
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
/// the census must then find.
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
        could_not_run("/usr/bin/log or /usr/bin/osascript is missing; nothing was decided");
    }
    let Some(start) = log_timestamp() else {
        could_not_run("/bin/date could not name the census start; nothing was decided");
    };
    let inst = boot();
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
    let chrome = client_ok(&inst, &["chrome"]);
    std::thread::sleep(IDLE);
    let log = inst.log.clone();
    let tail_before_exit = log_tail(&log);
    drop(inst);

    let Some(control) = positive_control() else {
        could_not_run("the osascript positive control did not run; nothing was decided");
    };
    let until = Instant::now() + CONTROL_VISIBLE_DEADLINE;
    let pids = loop {
        match preflighted_pids(&start) {
            Ok(pids) if pids.contains(&control) => break pids,
            Ok(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(500)),
            Ok(pids) => {
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
                could_not_run(format!(
                    "tccd's log never showed the positive control (osascript pid {control}) \
                     within {}s — no WindowServer session here, or the log dropped or reworded \
                     the line; saw preflights for {pids:?}. Nothing was decided.",
                    CONTROL_VISIBLE_DEADLINE.as_secs()
                ));
            }
            Err(e) => {
                could_not_run(format!(
                    "the unified log could not be read ({e}); nothing was decided"
                ));
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
    notice(&format!(
        "window-server census: pid {pid} drew 0 WindowServer preflights; the osascript control \
         (pid {control}) was seen, as were {} other preflighted pid(s) since {start}",
        pids.len().saturating_sub(1)
    ));
}
