// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The plumbing the two smokes stand on. Unix only, like the smokes
//! (`crate::smoke_stages`).
//!
//! Its invariants are unit tests below — among them the one a real incident
//! taught: a "friendlier" temp prefix pushed the instance socket path past
//! macOS's `SUN_LEN` ceiling and the smoke started timing out with a child log
//! that said `path must be shorter than SUN_LEN`, a product-shaped failure
//! caused entirely by the harness.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// macOS's `sockaddr_un.sun_path` ceiling. Every smoke socket path must stay
/// under it, which is why the smokes live in `/tmp` and not in `$TMPDIR`.
pub const SUN_LEN: usize = 104;

/// Pull `<field>=<digits>` out of a metrics reply.
///
/// Faithful to the script's `sed -n "s/.*[[:space:]]$2=\([0-9][0-9]*\).*/\1/p"`,
/// including two properties that matter:
///  * the field must be preceded by WHITESPACE, so `max_frames=9` is not a
///    reading of `frames`;
///  * the leading `.*` is greedy, so the LAST occurrence wins.
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

/// Pull the whole-millisecond part of `<field>=<digits>.<frac>`.
///
/// The script's `sed -n 's/.* max_input_present_ms=\([0-9]*\)\..*/\1/p'` requires
/// the decimal point, and yields NOTHING when the digits are absent — which the
/// caller then reports as "could not parse metrics" rather than as a latency
/// finding. Both properties are preserved.
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

/// `/bin/kill` — used instead of a `libc` dependency so this crate keeps zero of
/// them. Returns whether the signal was delivered, and anything the tool said.
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

/// Retire and reap ONE exact child without giving a broken shutdown path the
/// power to hang the gate: TERM, then two seconds, then KILL.
///
/// Only an exit caused by a signal this function actually SENT is normalised to
/// success — a child that died of its own accord (or of someone else's signal)
/// is a failed teardown, because the smoke's conclusions depend on the process it
/// launched being the process it measured.
///
/// Returns `(ok, stderr_from_kill)`.
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
/// "retired by us". Bounded only as a fail-safe (`false`). The smokes' own
/// tests use it to witness a child's exit before a teardown.
#[cfg(test)]
#[must_use]
pub(crate) fn exited_on_its_own(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
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

/// Bring exactly the launched GUI process to the front without Accessibility or
/// Apple-events permission.
///
/// A background CLI launch does not activate its AppKit process; measuring it
/// behind the caller's terminal makes Metal correctly report `Occluded`, the
/// finite product retry policy parks, and the gate falsely reports `frames=0`.
/// `activate` returning is not enough either — this waits until NSWorkspace
/// independently reports THIS pid as frontmost, then fails closed.
///
/// The program is run FROM A FILE, never through `swift -e`: `-e` is a
/// swift-driver flag, and the Command Line Tools' Swift on macOS 13 (5.8.1,
/// no swift-driver) prints `option '-e' is only supported in swift-driver`,
/// runs nothing and exits 1 — measured on an Intel MacBook Pro, 2026-09-15,
/// where every `--fast` run therefore recorded `skip: gui smoke (could not
/// make the test window frontmost)` and the merge-contract sentence was
/// unreachable. `swift <file>` interprets the same source on both toolchains.
///
/// THE SAME SENTENCE WENT UNREACHABLE AGAIN ON A LOADED MACHINE (2026-09-24,
/// m27, macOS 26.6.2). The program asked WindowServer once and gave the answer
/// three seconds; a `--fast` run is ~40 minutes at ~220% CPU on a fanless
/// laptop, and under its own load the freshly launched child did not win focus
/// inside that window. The stage skipped, the receipt recorded
/// `merge-contract no`, and the pre-push hook then refused a correctly shaped
/// gated merge — so the machine the gate exists to serve could not satisfy it
/// while it was busy satisfying it. Measured the same day with the machine
/// idle: the identical program returns 0, so this is a deadline, not a
/// capability. The wait is [`ACTIVATE_DEADLINE_SECS`] now and the activation is
/// RE-ISSUED while it runs — one `activate` that loses a race against a
/// contended WindowServer is not evidence the app cannot come forward.
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
    // The budget travels as ARGUMENTS, so the Swift waits exactly what the Rust
    // states: there is no second copy of either number to drift.
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

/// How long the activation program waits for WindowServer to report THIS pid as
/// frontmost before it fails closed.
///
/// Sized for the machine the gate actually runs on: a `--fast` run saturates a
/// fanless laptop for the better part of an hour, and the three seconds this
/// was cannot be told apart from "the app may not come forward" when
/// WindowServer is that contended. Twelve is long enough that load is no longer
/// the explanation and short enough that a genuinely refused activation still
/// skips the stage inside a few seconds of the old budget — the stage around it
/// already takes ~5 s.
pub(crate) const ACTIVATE_DEADLINE_SECS: u32 = 12;

/// How often the program RE-ISSUES `activate` while it waits. One call that
/// loses a race against a contended WindowServer says nothing about whether the
/// app can come forward, and re-asking is free.
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

/// The tail of a smoke's child log, indented under the ladder exactly as
/// `show_smoke_log` printed it. Empty when there is nothing to show.
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

/// Is this path a bound unix socket (or a symlink to one)? The script's
/// `[ -S "$sock" ] || [ -L "$sock" ]`.
#[must_use]
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink() || m.file_type().is_socket())
        .unwrap_or(false)
}

/// Is the control socket at `path` LISTENING? A connect the kernel takes into
/// the backlog, dropped at once — the server's own `socket_is_live` probe. The
/// socket FILE ([`is_socket_or_symlink`]) is not that signal: `bind(2)` creates
/// it before `listen(2)`, and a client that dials in between is refused
/// (`ECONNREFUSED`), which a loaded machine turns from a microsecond window into
/// a smoke stage's first `aterm-ctl` call failing (2026-09-24).
#[must_use]
pub fn socket_listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
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
        // The script's greedy leading `.*`: the last occurrence wins.
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
        // The incident invariant.
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
        // Its own exit, witnessed — not a 50 ms nap that a loaded machine can
        // outlast, letting the TERM below kill it first and read as ours.
        let exited = exited_on_its_own(child.id());
        let (ok, _) = retire_smoke_child(&mut child);
        assert!(exited, "`exit 7` never exited within 30 s");
        assert!(!ok, "exit 7 is not something this teardown caused");
    }

    /// The budget is TERM, two seconds, then KILL; the bound only has to tell
    /// that from waiting the child out, so it is a minute — half the life of
    /// the `sleep` the child execs, which ignores TERM (the disposition
    /// survives the exec) and so ends only by the KILL or by itself.
    ///
    /// The TERM must land AFTER the trap. Sent at once, it killed the shell
    /// before the shell ignored anything, the KILL was never reached, and the
    /// test passed with the KILL removed (sweep 4, 2026-09-29). So the child
    /// says when it is ignoring TERM, and only then is it retired.
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
            "the gate is never hung by teardown: {:?}",
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
