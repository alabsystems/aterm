// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The plumbing the two smokes stand on, and the self-check that keeps it honest.
//!
//! `tools/verify.sh` grew a `smoke_helpers_selftest` after a real incident: a
//! "friendlier" temp prefix pushed the instance socket path past macOS's
//! `SUN_LEN` ceiling and the smoke started timing out with a child log that said
//! `path must be shorter than SUN_LEN` — a product-shaped failure caused entirely
//! by the harness. Those invariants are unit tests here AND still run under
//! `--selftest`, because the point was always to check the harness on a machine,
//! not only in a test binary.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// macOS's `sockaddr_un.sun_path` ceiling. Every smoke socket path must stay
/// under it, which is why the smokes live in `/tmp` and not in `$TMPDIR`.
pub const SUN_LEN: usize = 104;

/// The ordinary debug binaries ARE the smoke artifacts. Launching them directly
/// (rather than through `targo run`) keeps the recorded PID attached to the real
/// process, so teardown and activation are deterministic — and keeps the driver's
/// lane banner out of a captured control-socket reply.
///
/// A relative `CARGO_TARGET_DIR` is interpreted from the repo root, matching cargo.
#[must_use]
pub fn debug_bin(root: &Path, cargo_target_dir: Option<&OsStr>, name: &str) -> PathBuf {
    let dir = cargo_target_dir.map_or_else(
        || root.join("target"),
        |d| {
            let p = Path::new(d);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                root.join(p)
            }
        },
    );
    dir.join("debug").join(name)
}

/// The same, for a `[[example]]` target — cargo puts those under
/// `<target>/debug/examples/`, not beside the binaries.
///
/// A separate function rather than a `name` a caller spells with a slash: the
/// layout is cargo's, and a caller encoding it in a string is a caller that
/// will be wrong on the day it changes.
#[must_use]
pub fn debug_example(root: &Path, cargo_target_dir: Option<&OsStr>, name: &str) -> PathBuf {
    debug_bin(root, cargo_target_dir, "examples").join(name)
}

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
#[cfg(unix)]
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

/// Windows analogue of the above. There is no signal to ask politely with and no
/// `signal()` on `ExitStatus` to classify the death by, so the sequence is: give the
/// child two seconds to leave on its own, then `TerminateProcess` via [`Child::kill`].
///
/// The Unix contract — only a death WE caused counts as a clean retirement — is kept:
/// a self-exit is judged by its code, and a terminated child is a success only when
/// this function is the one that terminated it.
#[cfg(not(unix))]
pub fn retire_smoke_child(child: &mut Child) -> (bool, String) {
    let mut noise = String::new();

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
        match child.kill() {
            Ok(()) => kill_sent = true,
            Err(e) => noise.push_str(&e.to_string()),
        }
    }
    let ok = match child.wait() {
        // Left on its own terms, cleanly.
        Ok(st) if st.code() == Some(0) => true,
        // Any other exit is clean only if this function is what ended it.
        Ok(_) => kill_sent,
        Err(_) => false,
    };
    (ok, noise)
}

/// Bring exactly the launched GUI process to the front without Accessibility or
/// Apple-events permission.
///
/// A background CLI launch does not activate its AppKit process; measuring it
/// behind the caller's terminal makes Metal correctly report `Occluded`, the
/// finite product retry policy parks, and the gate falsely reports `frames=0`.
/// `activate` returning is not enough either — this waits until NSWorkspace
/// independently reports THIS pid as frontmost, then fails closed.
#[must_use]
pub fn activate_macos_gui_pid(pid: u32) -> bool {
    const SWIFT: &str = "/usr/bin/swift";
    if !Path::new(SWIFT).exists() {
        return false;
    }
    Command::new(SWIFT)
        .arg("-e")
        .arg(ACTIVATE_SWIFT)
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The activation program, byte-identical to the one the script inlined.
const ACTIVATE_SWIFT: &str = r#"
import AppKit
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
guard let app = NSRunningApplication(processIdentifier: pid) else { exit(2) }
_ = app.activate(options: [.activateAllWindows])
let deadline = Date().addingTimeInterval(3)
while Date() < deadline {
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
#[cfg(unix)]
#[must_use]
pub fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink() || m.file_type().is_socket())
        .unwrap_or(false)
}

/// Windows has no `S_IFSOCK`: a bound `AF_UNIX` socket lands on disk as a REPARSE
/// POINT, which `FileType::is_socket` (absent there) could not report anyway. So the
/// honest analogue tests the reparse attribute alongside the symlink case — still
/// specific, and still false for the plain file the selftest asserts against.
#[cfg(not(unix))]
#[must_use]
pub fn is_socket_or_symlink(path: &Path) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .map(|m| {
            #[cfg(windows)]
            {
                m.file_type().is_symlink()
                    || (m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            }
            #[cfg(not(windows))]
            {
                let _ = FILE_ATTRIBUTE_REPARSE_POINT;
                m.file_type().is_symlink()
            }
        })
        .unwrap_or(false)
}

/// The `--selftest` harness check: pure and temporary coverage of the plumbing
/// the real smokes depend on. Same assertions the script made.
#[must_use]
pub fn smoke_helpers_selftest(root: &Path) -> bool {
    let Ok(tmp) = crate::mktemp_dir("atx") else {
        return false;
    };
    let got = debug_bin(root, Some(OsStr::new("relative-target")), "aterm-gui");
    let expected = root.join("relative-target/debug/aterm-gui");
    // Pin the maximum macOS instance-socket spelling below SUN_LEN: this catches
    // a future "friendlier" temp prefix silently reintroducing the timeout whose
    // child log said `path must be shorter than SUN_LEN`.
    let sock = tmp.join("run/aterm/aterm-4294967295.sock");

    let mut noise = String::new();
    let mut retired = false;
    if let Ok(mut long) = Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let (ok, err) = retire_smoke_child(&mut long);
        retired = ok;
        noise.push_str(&err);
    }
    // Negative control: a child that exited on its own must NOT be reported as
    // successfully retired.
    let mut unexpected_accepted = true;
    if let Ok(mut quick) = Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::sleep(Duration::from_millis(50));
        let (ok, err) = retire_smoke_child(&mut quick);
        unexpected_accepted = ok;
        noise.push_str(&err);
    }

    let ok = got == expected
        && metric_u64("OK backend=gpu frames=17 present_drops=0", "frames") == Some(17)
        && metric_u64("OK max_frames=9", "frames").is_none()
        && retired
        && !unexpected_accepted
        && noise.is_empty()
        && sock.as_os_str().len() < SUN_LEN;
    std::fs::remove_dir_all(&tmp).ok();
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_debug_binary_path_follows_cargo_target_dir() {
        let root = Path::new("/repo");
        assert_eq!(
            debug_bin(root, None, "aterm-gui"),
            Path::new("/repo/target/debug/aterm-gui")
        );
        assert_eq!(
            debug_bin(root, Some(OsStr::new("relative-target")), "aterm-gui"),
            Path::new("/repo/relative-target/debug/aterm-gui"),
            "a relative CARGO_TARGET_DIR is interpreted from the repo root"
        );
        assert_eq!(
            debug_bin(root, Some(OsStr::new("/elsewhere")), "aterm-ctl"),
            Path::new("/elsewhere/debug/aterm-ctl")
        );
    }

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
        // The incident invariant, checked here as well as under --selftest.
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
        std::thread::sleep(Duration::from_millis(50));
        let (ok, _) = retire_smoke_child(&mut child);
        assert!(!ok, "exit 7 is not something this teardown caused");
    }

    #[test]
    fn a_child_that_ignores_term_is_killed_within_the_budget() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let start = std::time::Instant::now();
        let (ok, _) = retire_smoke_child(&mut child);
        assert!(ok, "KILL after the budget is still a retirement we caused");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the gate is never hung by teardown"
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

    #[test]
    fn the_harness_selftest_passes_on_this_machine() {
        assert!(smoke_helpers_selftest(Path::new("/repo")));
    }
}
