// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The plumbing the two smokes stand on. Unix only, like the smokes
//! (`crate::smoke_stages`).

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// macOS's `sockaddr_un.sun_path` ceiling. Every smoke socket path must stay
/// under it, which is why the smokes live in `/tmp` and not in `$TMPDIR`.
pub const SUN_LEN: usize = 104;

/// Pull `<field>=<digits>` out of a metrics reply. The field must follow
/// whitespace, so `max_frames=9` is not a reading of `frames`, and the last
/// occurrence wins.
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
    // Saturating rather than failing: an absurd counter is still a counter, and
    // the comparisons that use this are one-sided floors and ceilings.
    found.map(|d| d.parse::<u64>().unwrap_or(u64::MAX))
}

/// Pull the whole-millisecond part of `<field>=<digits>.<frac>`, the last
/// occurrence. Without the decimal point or the digits it yields nothing, which
/// the caller reports as unparseable metrics rather than a latency finding.
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

/// Send `-<sig>` to `pid` with `/bin/kill`, so this crate needs no `libc`.
/// Returns whether the signal was delivered, and anything the tool said.
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

/// Retire and reap one exact child without giving a broken shutdown path the
/// power to hang the run: TERM, then two seconds, then KILL. Returns
/// `(ok, stderr_from_kill)`.
///
/// Success is a clean exit or death by a signal this function sent: a child
/// that failed on its own (or died of someone else's signal) is a failed
/// teardown, because the smoke's conclusions depend on the process it launched
/// being the process it measured.
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

/// Block until `pid`, a child of this process that ends by itself, has exited:
/// a zombie in `ps`, not yet reaped, so the pid cannot be recycled under a
/// signal sent to it next. Polls the state, never a fixed sleep a loaded
/// machine can outlast. `false` after a minute's hang detector.
#[cfg(test)]
#[must_use]
pub(crate) fn exited_on_its_own(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let stat = Command::new("ps")
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

/// Whether the console session's screen is locked: `CGSSessionScreenIsLocked`
/// in `ioreg -n Root -d1`'s console users. A locked screen is why no window can
/// come forward, so a smoke that could not do so says that instead of a guess.
/// `false` when it cannot be read.
#[must_use]
pub fn screen_locked() -> bool {
    Command::new("/usr/sbin/ioreg")
        .args(["-n", "Root", "-d1"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| reads_locked(&String::from_utf8_lossy(&out.stdout)))
}

/// [`screen_locked`]'s reading of `ioreg`'s text.
#[must_use]
pub fn reads_locked(ioreg: &str) -> bool {
    ioreg.contains("\"CGSSessionScreenIsLocked\"=Yes")
}

/// Bring exactly the launched GUI process to the front without Accessibility or
/// Apple-events permission. A background launch does not activate its AppKit
/// process, and measured behind the caller's terminal Metal reports `Occluded`
/// and the run reads `frames=0`. Waits until NSWorkspace reports this pid as
/// frontmost, re-issuing `activate` ([`ACTIVATE_REISSUE_SECS`]) up to
/// [`ACTIVATE_DEADLINE_SECS`], then fails closed.
///
/// The program runs from a file, never `swift -e`: `-e` is a swift-driver flag,
/// and the Command Line Tools' Swift on macOS 13 (no swift-driver) runs nothing
/// and exits 1.
#[must_use]
pub fn activate_macos_gui_pid(pid: u32) -> bool {
    const SWIFT: &str = "/usr/bin/swift";
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
    // The budget travels as arguments, so the Swift holds no copy of it to drift.
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

/// The activation program: `<pid> <deadline secs> <reissue secs>`, the last two
/// handed over from the two constants above.
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

/// The last 80 lines of a smoke's child log, indented under the ladder. Empty
/// when there is nothing to show.
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

/// Is this path a bound unix socket (or a symlink to one)?
#[must_use]
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink() || m.file_type().is_socket())
        .unwrap_or(false)
}

/// Is the control socket at `path` listening? A connect the kernel takes into
/// the backlog, dropped at once, as the server's own `socket_is_live` probe.
/// The socket file ([`is_socket_or_symlink`]) is not that signal: `bind(2)`
/// creates it before `listen(2)`, and a client that dials in between is
/// refused (`ECONNREFUSED`), a window a loaded machine widens.
#[must_use]
pub fn socket_listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_locked_console_session_reads_locked_and_an_unlocked_one_does_not() {
        let locked = r#""IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"CGSSessionScreenIsLocked"=Yes,"kCGSSessionUserIDKey"=501})"#;
        let open =
            r#""IOConsoleUsers" = ({"kCGSSessionOnConsoleKey"=Yes,"kCGSSessionUserIDKey"=501})"#;
        assert!(super::reads_locked(locked));
        assert!(!super::reads_locked(open));
        assert!(!super::reads_locked(""), "an unread ioreg is not a lock");
    }

    use super::*;

    #[test]
    fn a_metric_field_must_be_the_whole_field() {
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
        // The last occurrence wins.
        assert_eq!(metric_u64("OK frames=3 frames=9", "frames"), Some(9));
    }

    #[test]
    fn the_latency_reading_needs_its_decimal_point() {
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
    fn the_longest_instance_socket_path_stays_under_sun_len() {
        let tmp = crate::mktemp_dir("atx").expect("mktemp");
        let sock = tmp.join("run/aterm/aterm-4294967295.sock");
        assert!(
            sock.as_os_str().len() < SUN_LEN,
            "{} is {} bytes, over the {SUN_LEN}-byte ceiling",
            sock.display(),
            sock.as_os_str().len()
        );
        std::fs::remove_dir_all(&tmp).ok();
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
    fn a_child_that_died_on_its_own_is_not_a_clean_retirement() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        // Its exit witnessed, so the TERM below cannot kill it first and read as ours.
        let exited = exited_on_its_own(child.id());
        let (ok, _) = retire_smoke_child(&mut child);
        assert!(exited, "`exit 7` never exited within a minute");
        assert!(!ok, "exit 7 is not something this teardown caused");
    }

    /// The bound is a minute, half the life of the `sleep` the child execs,
    /// which ignores TERM (the disposition survives the exec) and so ends only
    /// by the KILL. The child reports its trap before it is retired, so the TERM
    /// cannot kill the shell first and leave the KILL path untested.
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

    #[test]
    fn the_smoke_log_tail_is_indented_and_bounded() {
        let tmp = crate::mktemp_dir("atv-log").expect("mktemp");
        let log = tmp.join("gui.log");
        assert_eq!(
            smoke_log_tail("GUI smoke", &log),
            "",
            "no log, nothing to show"
        );
        std::fs::write(&log, b"").expect("write");
        assert_eq!(
            smoke_log_tail("GUI smoke", &log),
            "",
            "an empty log is not printed"
        );

        let body: String = (0..100).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&log, body).expect("write");
        let tail = smoke_log_tail("GUI smoke", &log);
        assert!(tail.starts_with("        GUI smoke child log (last 80 lines):\n"));
        assert_eq!(tail.lines().count(), 81);
        assert!(tail.contains("\n          line20\n"));
        assert!(!tail.contains("line19\n"), "older lines are dropped");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn only_a_socket_or_a_symlink_counts_as_a_bound_socket() {
        let tmp = crate::mktemp_dir("atv-sock").expect("mktemp");
        let plain = tmp.join("aterm.sock");
        assert!(!is_socket_or_symlink(&plain), "absent is not bound");
        std::fs::write(&plain, b"not a socket").expect("write");
        assert!(
            !is_socket_or_symlink(&plain),
            "a regular file is not a bound socket"
        );
        let link = tmp.join("link.sock");
        std::os::unix::fs::symlink(&plain, &link).expect("symlink");
        assert!(is_socket_or_symlink(&link), "the script accepted a symlink");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
