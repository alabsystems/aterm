// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plumbing for the Unix GUI smoke (`crate::smoke_stages`).

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::exec::group;

/// macOS `sockaddr_un.sun_path` byte ceiling.
pub const SUN_LEN: usize = 104;

/// Pulls the last whitespace-preceded `<field>=<digits>` out of a metrics reply.
#[must_use]
pub fn metric_u64(line: &str, field: &str) -> Option<u64> {
    let needle = format!("{field}=");
    let bytes = line.as_bytes();
    let mut found = None;
    let mut from = 0;
    while let Some(rel) = line[from..].find(&needle) {
        let at = from + rel;
        from = at + needle.len();
        if at == 0 || !bytes[at - 1].is_ascii_whitespace() {
            continue;
        }
        let digits: String = line[from..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            found = Some(digits);
        }
    }
    found.map(|d| d.parse::<u64>().unwrap_or(u64::MAX))
}

/// Pulls the whole-millisecond part of the last `<field>=<digits>.<frac>` reading.
#[must_use]
pub fn metric_ms_whole(line: &str, field: &str) -> Option<u64> {
    let needle = format!(" {field}=");
    let mut found = None;
    let mut from = 0;
    while let Some(rel) = line[from..].find(&needle) {
        let at = from + rel + needle.len();
        from = at;
        let digits: String = line[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if line[at + digits.len()..].starts_with('.') && !digits.is_empty() {
            found = Some(digits);
        }
    }
    found.map(|d| d.parse::<u64>().unwrap_or(u64::MAX))
}

/// How long the smoke's instance has to leave after its group's `SIGTERM` before the group is
/// `SIGKILL`ed.
const TERM_GRACE: Duration = Duration::from_secs(2);

/// The wait for a `SIGKILL`ed group to hold nothing but its leader: a hang detector, never a
/// budget.
const GROUP_GONE: Duration = Duration::from_secs(60);

/// The smoke's instance, leading a process group of its own. What it starts as it goes (a lane's
/// `atpkg machine` read, its consent sentinel's `log stream`) stays in that group when the instance
/// dies and launchd adopts it, so [`SmokeChild::retire`] ends it with the instance, by the group
/// and not by a pid: nothing the instance started outlives the smoke into verify's run-end check
/// or the next timed run.
pub struct SmokeChild {
    child: Child,
    /// The group's custody while its leader is unreaped, and its place on the interrupt list.
    live: group::Live,
}

impl SmokeChild {
    /// Spawn `cmd` as the leader of a new process group, its stdin `/dev/null`: the group is in
    /// the background, so a member that read a terminal would stop on `SIGTTIN`. `made` gets the
    /// group's id while the spawn still holds off an interrupt's sweep, so the run records the
    /// group before anything can end it.
    /// # Errors
    /// The run was interrupted before the spawn, or the spawn failed.
    pub fn spawn(cmd: &mut Command, made: impl FnOnce(u32)) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        let spawning = group::Spawning::begin();
        if group::stopping() {
            return Err(std::io::Error::other(
                crate::exec::halted()
                    .unwrap_or("the run was interrupted before this child started"),
            ));
        }
        let child = cmd.stdin(Stdio::null()).process_group(0).spawn()?;
        let live = group::Live::enter(child.id());
        made(child.id());
        drop(spawning);
        Ok(Self { child, live })
    }

    /// The leader's pid, which is the group's id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Whether the leader has exited, seen without reaping it, so its group id names no other
    /// group; a wait that fails reads as exited.
    pub fn exited(&mut self) -> bool {
        self.live.exited(&mut self.child, false).unwrap_or(true)
    }

    /// End the instance and everything in its group, then reap it: `SIGTERM` the group,
    /// `SIGKILL` it once [`TERM_GRACE`] passes with the leader still there, and once the leader
    /// has exited `SIGKILL` the group until it holds nothing but the unreaped leader (a child
    /// forked as one signal went out takes the next), until [`GROUP_GONE`] passes, or until the
    /// group's custody is lost and no signal may go to its id. `sweep`
    /// reads the group before the leader is reaped, and names what is still there; `reaped` is
    /// told the group once the leader is reaped. True only when the leader exited 0 or died of a
    /// signal sent here.
    pub fn retire(mut self, sweep: impl FnOnce(u32), reaped: impl FnOnce(u32)) -> bool {
        use std::os::unix::process::ExitStatusExt as _;
        let pgid = self.child.id();
        let term_sent = self.live.term().is_ok();
        let mut kill_sent = false;
        let grace = std::time::Instant::now() + TERM_GRACE;
        while !self.exited() {
            if std::time::Instant::now() >= grace {
                kill_sent = self.live.kill().is_ok();
                let _ = self.live.exited(&mut self.child, true);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let gone = std::time::Instant::now() + GROUP_GONE;
        while self.live.holds(pgid) && others_in(pgid) && std::time::Instant::now() < gone {
            let _ = self.live.kill();
            std::thread::sleep(Duration::from_millis(10));
        }
        match self
            .live
            .reap_stage_sweeping(&mut self.child, true, sweep, reaped)
        {
            Ok(Some(st)) => match (st.code(), st.signal()) {
                (Some(0), _) => true,
                (_, Some(15)) => term_sent,
                (_, Some(9)) => kill_sent,
                _ => false,
            },
            _ => false,
        }
    }
}

/// Whether group `pgid` lists a member besides its leader (Apple lists the unreaped leader, and a
/// killed member until launchd reaps it); a group no listing reads counts as empty.
fn others_in(pgid: u32) -> bool {
    let leader = i32::try_from(pgid).unwrap_or(0);
    aterm_uds::process::group_members(pgid)
        .is_some_and(|members| members.pids.iter().any(|&pid| pid != leader))
}

/// Block until `pid` — a child of this process that ends by itself — has EXITED:
/// a zombie in `ps`, its status fixed and not yet reaped, so the pid is still
/// ours and nothing can be recycled under a signal sent to it next. The state,
/// never a nap: a loaded machine can keep `sh -c 'exit 7'` alive past any fixed
/// sleep, and a TERM that beats it to its `exit` turns "died on its own" into
/// "retired by us". Bounded only as a fail-safe (`false`): a minute's hang
/// detector, never a latency budget. The smoke's own tests use it to witness a
/// child's exit before a teardown.
#[cfg(test)]
#[must_use]
pub(crate) fn exited_on_its_own(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let stat = Command::new(aterm_uds::os_path!("ps"))
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if stat.starts_with('Z') {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether the console session's screen is locked (`CGSSessionScreenIsLocked` in `ioreg`).
#[must_use]
pub fn screen_locked() -> bool {
    Command::new("/usr/sbin/ioreg")
        .args(["-n", "Root", "-d1"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| reads_locked(&String::from_utf8_lossy(&out.stdout)))
}

/// Checks whether `ioreg` output reports a locked screen.
#[must_use]
pub fn reads_locked(ioreg: &str) -> bool {
    ioreg.contains("\"CGSSessionScreenIsLocked\"=Yes")
}

/// Brings `pid` to the front via AppKit, re-issuing `activate` up to
/// [`ACTIVATE_DEADLINE_SECS`] until `NSWorkspace` reports it frontmost.
#[must_use]
pub fn activate_macos_gui_pid(pid: u32) -> bool {
    const SWIFT: &str = aterm_uds::os_path!("swift");
    if !Path::new(SWIFT).exists() {
        return false;
    }
    let Ok(dir) = aterm_tempfile::Builder::new()
        .prefix("aterm-verify-activate-")
        .suffix(".noindex")
        .tempdir()
    else {
        return false;
    };
    let script = dir.path().join("activate.swift");
    if crate::write_fresh(&script, ACTIVATE_SWIFT).is_err() {
        return false;
    }
    Command::new(SWIFT)
        .arg(&script)
        .arg(pid.to_string())
        .arg(ACTIVATE_DEADLINE_SECS.to_string())
        .arg(ACTIVATE_REISSUE_SECS.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// How long the activation program waits for WindowServer to report this pid as
/// frontmost before it fails closed. Sized for a loaded machine, where three
/// seconds cannot be told from a refused activation.
pub(crate) const ACTIVATE_DEADLINE_SECS: u32 = 12;

/// How often the program re-issues `activate` while it waits: one call that
/// loses a race against a contended WindowServer is not a refusal.
pub(crate) const ACTIVATE_REISSUE_SECS: u32 = 2;

const ACTIVATE_SWIFT: &str = r#"
import AppKit
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
let deadlineSecs = TimeInterval(CommandLine.arguments[2])!
let reissueSecs = TimeInterval(CommandLine.arguments[3])!
guard let app = NSRunningApplication(processIdentifier: pid) else { exit(2) }
let deadline = Date().addingTimeInterval(deadlineSecs)
var nextActivate = Date()
while Date() < deadline {
    // RE-ISSUED, not asked once: under a loaded WindowServer the first call can
    // simply lose the race, which is not the same as being refused.
    if Date() >= nextActivate {
        _ = app.activate(options: [.activateAllWindows])
        nextActivate = Date().addingTimeInterval(reissueSecs)
    }
    if NSWorkspace.shared.frontmostApplication?.processIdentifier == pid { exit(0) }
    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
}
exit(3)
"#;

/// Returns the last 80 lines of a smoke child log, indented under the ladder.
#[must_use]
pub fn smoke_log_tail(label: &str, path: &Path) -> String {
    let Ok(text) = crate::read_regular_text(path) else {
        return String::new();
    };
    if text.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(80);
    let mut out = format!("        {label} child log (last 80 lines):\n");
    for l in &lines[start..] {
        out.push_str("          ");
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Whether `path` is a bound Unix socket or symlink.
#[must_use]
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink() || m.file_type().is_socket())
        .unwrap_or(false)
}

/// Whether the control socket at `path` is listening (accepts a connection).
#[must_use]
pub fn socket_listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
#[path = "smoke_tests.rs"]
mod tests;
