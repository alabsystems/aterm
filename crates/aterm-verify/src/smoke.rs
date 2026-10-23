// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plumbing for the Unix GUI smoke (`crate::smoke_stages`).

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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

/// Sends `-<sig>` to `pid` via `/bin/kill` and returns `(delivered, stderr)`.
#[must_use]
pub fn signal(pid: u32, sig: &str) -> (bool, String) {
    let killer = if Path::new("/bin/kill").exists() {
        "/bin/kill"
    } else {
        "kill"
    };
    match Command::new(killer)
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

/// Retires and reaps `child` (TERM, 2s wait, then KILL), returning `(ok, stderr_from_kill)`.
/// Succeeds only on exit 0 or death by a signal sent here.
pub fn retire_smoke_child(child: &mut Child) -> (bool, String) {
    use std::os::unix::process::ExitStatusExt;

    let pid = child.id();
    let mut noise = String::new();
    let (term_sent, err) = signal(pid, "TERM");
    noise.push_str(&err);

    let mut exited = false;
    for _ in 0..40 {
        match child.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let mut kill_sent = false;
    if !exited && matches!(child.try_wait(), Ok(None)) {
        let (sent, err) = signal(pid, "KILL");
        kill_sent = sent;
        noise.push_str(&err);
    }
    let ok = match child.wait() {
        Ok(st) => match (st.code(), st.signal()) {
            (Some(0), _) => true,
            (_, Some(15)) => term_sent,
            (_, Some(9)) => kill_sent,
            _ => false,
        },
        Err(_) => false,
    };
    (ok, noise)
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
    let script = std::env::temp_dir().join(format!(
        "aterm-verify-activate-{}-{pid}.swift",
        std::process::id()
    ));
    if std::fs::write(&script, ACTIVATE_SWIFT).is_err() {
        return false;
    }
    let ok = Command::new(SWIFT)
        .arg(&script)
        .arg(pid.to_string())
        .arg(ACTIVATE_DEADLINE_SECS.to_string())
        .arg(ACTIVATE_REISSUE_SECS.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&script);
    ok
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
    let Ok(text) = std::fs::read_to_string(path) else {
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
mod tests {
    use super::*;

    #[test]
    fn a_metric_is_its_whole_field_and_a_latency_needs_its_decimal_point() {
        assert_eq!(
            metric_u64("OK backend=gpu frames=17 present_drops=0", "frames"),
            Some(17)
        );
        assert_eq!(
            metric_u64("OK max_frames=9", "frames"),
            None,
            "max_frames is not frames"
        );
        assert_eq!(metric_u64("OK frames=0", "frames"), Some(0));
        assert_eq!(metric_u64("OK frames=", "frames"), None);
        assert_eq!(metric_u64("OK", "frames"), None);
        assert_eq!(metric_u64("OK frames=3 frames=9", "frames"), Some(9));
        let m = "OK frames=31 max_input_present_ms=12.480 sync_rel_timeout=0 ";
        assert_eq!(metric_ms_whole(m, "max_input_present_ms"), Some(12));
        assert_eq!(
            metric_ms_whole("OK max_input_present_ms=530 ", "max_input_present_ms"),
            None,
            "no decimal point is an unparseable reply, not a latency finding"
        );
        assert_eq!(
            metric_ms_whole("OK max_input_present_ms=.5 ", "max_input_present_ms"),
            None
        );
        assert_eq!(
            metric_ms_whole("OK other=1.0", "max_input_present_ms"),
            None
        );
    }

    #[test]
    fn retiring_a_live_child_terms_it_and_reaps_exactly_once() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let (ok, noise) = retire_smoke_child(&mut child);
        assert!(ok, "a TERM we sent is a clean retirement");
        assert!(noise.is_empty(), "teardown must be quiet: {noise}");
    }

    #[test]
    fn a_child_that_ignores_term_is_killed_within_the_budget() {
        use std::io::BufRead as _;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; echo ignoring; exec sleep 120"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
            .read_line(&mut ready)
            .expect("read the child's word");
        assert_eq!(ready, "ignoring\n", "the child set its trap");
        let start = std::time::Instant::now();
        let (ok, _) = retire_smoke_child(&mut child);
        assert!(ok, "KILL after the budget is still a retirement we caused");
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the run is never hung by teardown: {:?}",
            start.elapsed()
        );
    }
}
