// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console-host lane end to end: a tab spawned under `conpty = "auto"`
//! runs in OpenConsole when this machine has one (a bundled copy, or the
//! installed Windows Terminal's) and passes a child's VT through verbatim; a
//! host that cannot start falls back to the Windows console host and the tab
//! still opens; the default (`conpty` unset) is the Windows console host.
//!
//! Its own test binary on purpose: the host choice is PROCESS-global
//! (`aterm_pty::set_console_host`), and the other Windows suites expect the
//! inbox host every embedder starts with. Within this binary the tests take
//! [`HOST_LOCK`], so no two of them change the choice under each other.
//!
//! No test here waits on a clock for the child: each child holds its console
//! open on a line read until the test has seen what it came for and types
//! Enter, so its output is never raced by its own exit. Every wait is a
//! one-minute hang detector.

#![cfg(windows)]

use std::ffi::OsString;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_pty::{ConsoleHostChoice, ConsoleHostKind};

/// Serializes the tests: each one sets the process-global host choice.
static HOST_LOCK: Mutex<()> = Mutex::new(());

/// A wait for something that must happen: a hang detector, never a budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn caps() -> (
    aterm_cap::Cap<aterm_cap::effects::Spawn>,
    aterm_cap::Cap<aterm_sandbox::Sandbox>,
) {
    // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    (
        authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted),
        authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted),
    )
}

/// A spawned session with a reader thread that collects its output and — as
/// the frontend's engine does — answers the console host's DA1 query at once
/// (the snapshot recorded OpenConsole holding its client 3 s for an unanswered
/// one).
struct Tab {
    master: i32,
    pid: i32,
    out: Arc<Mutex<Vec<u8>>>,
    eof: mpsc::Receiver<()>,
}

impl Tab {
    fn spawn(argv: &[&str]) -> Self {
        let (spawn_cap, sandbox_cap) = caps();
        let argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            100,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&argv),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("the spawn must succeed under either host");
        let out = Arc::new(Mutex::new(Vec::new()));
        let (tx, eof) = mpsc::channel();
        let sink = Arc::clone(&out);
        let master = shell.master;
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut answered = false;
            loop {
                let n = aterm_pty::read(master, &mut buf);
                if n <= 0 {
                    let _ = tx.send(());
                    return;
                }
                let mut all = sink.lock().unwrap_or_else(|p| p.into_inner());
                all.extend_from_slice(&buf[..n as usize]);
                if !answered && all.windows(3).any(|w| w == b"\x1b[c") {
                    answered = true;
                    aterm_pty::write_all(master, b"\x1b[?64;6;22;28c");
                }
            }
        });
        Tab {
            master,
            pid: shell.pid,
            out,
            eof,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.out.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Everything read so far once `marker` is in it at or after byte `from`.
    fn wait_for_from(&self, from: usize, marker: &[u8]) -> Vec<u8> {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            let seen = self.bytes();
            if seen.len() > from && seen[from..].windows(marker.len()).any(|w| w == marker) {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "no {:?} within {HANG_DETECTOR:?}; saw {:?}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&seen)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for(&self, marker: &[u8]) -> Vec<u8> {
        self.wait_for_from(0, marker)
    }

    /// Type Enter: the child's line read returns and it goes on.
    fn enter(&self) {
        aterm_pty::write_all(self.master, b"\r");
    }

    fn wait_eof(&self) {
        self.eof
            .recv_timeout(HANG_DETECTOR)
            .expect("the session reached EOF after its child ended");
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        aterm_pty::hangup(self.pid);
        aterm_pty::close_master(self.master);
    }
}

/// Windows PowerShell 5.1, present on every Windows this crate runs on, running
/// `script` and then holding the console on one line read.
fn powershell(script: &str) -> Vec<String> {
    [
        "powershell.exe",
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain(std::iter::once(format!(
        "{script}; [Console]::Out.Flush(); [void][Console]::In.ReadLine()"
    )))
    .collect()
}

fn spawn_powershell(script: &str) -> Tab {
    let argv = powershell(script);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    Tab::spawn(&argv)
}

/// The SGR 58 turn: an underline colour the snapshot recorded the inbox host
/// rewriting to `4m … 24m` and OpenConsole passing as sent.
const SGR58: &[u8] = b"\x1b[58;2;255;0;0m\x1b[4mSEMI-RGB\x1b[59m\x1b[24m-END";

const SGR58_SCRIPT: &str =
    "$e=[char]27; [Console]::Out.Write(\"$e[58;2;255;0;0m$e[4mSEMI-RGB$e[59m$e[24m-END\")";

// `auto`: the host this machine resolves decides what the tab must show. With
// an OpenConsole to launch, the child's SGR 58 arrives byte for byte and the
// session says OpenConsole; with none, the tab opens under the inbox host, the
// session says Inbox, and the log is handed the reason once. Either way the
// branch taken is ASSERTED, never skipped.
#[test]
fn auto_runs_the_tab_in_openconsole_or_says_why_not() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let _ = aterm_pty::take_console_host_fallback();
    let report = aterm_pty::console_host_report();
    let tab = spawn_powershell(SGR58_SCRIPT);
    let seen = tab.wait_for(b"-END");
    let kind = aterm_pty::conpty_host(tab.master);
    match report.kind {
        ConsoleHostKind::OpenConsole => {
            assert_eq!(
                kind,
                Some(ConsoleHostKind::OpenConsole),
                "{report}; fallback: {:?}",
                aterm_pty::take_console_host_fallback()
            );
            assert!(
                seen.windows(SGR58.len()).any(|w| w == SGR58),
                "OpenConsole must pass SGR 58 through verbatim; saw {:?}",
                String::from_utf8_lossy(&seen)
            );
        }
        ConsoleHostKind::Inbox => {
            assert_eq!(kind, Some(ConsoleHostKind::Inbox), "{report}");
            assert!(report.fell_back, "{report}");
            assert!(
                report.line.starts_with("Windows console host; "),
                "{report}"
            );
        }
    }
    tab.enter();
    tab.wait_eof();
}

// A configured host that is not a console host starts, writes no VT and is
// refused by the health check: the tab opens under the inbox host all the
// same, the log hears why once, and the choice stays refused for later tabs.
#[test]
fn a_configured_host_that_does_not_start_falls_back_to_the_inbox_host() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let not_a_host = std::path::Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("sort.exe");
    aterm_pty::set_console_host(ConsoleHostChoice::Path(not_a_host.clone()));
    let _ = aterm_pty::take_console_host_fallback();
    let tab = spawn_powershell("[Console]::Out.Write('FELL-BACK')");
    tab.wait_for(b"FELL-BACK");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(ConsoleHostKind::Inbox)
    );
    let note = aterm_pty::take_console_host_fallback().expect("the failure is handed to the log");
    assert!(
        note.contains("OpenConsole did not start")
            && note.contains(&format!("aterm.toml conpty = \"{}\"", not_a_host.display())),
        "{note}"
    );
    let report = aterm_pty::console_host_report();
    assert_eq!(report.kind, ConsoleHostKind::Inbox, "the refusal sticks");
    assert!(report.fell_back, "{report}");
    assert!(
        report.line.contains("OpenConsole did not start"),
        "{report}"
    );
    tab.enter();
    tab.wait_eof();

    // A second tab takes the inbox host straight away and logs nothing new.
    let again = spawn_powershell("[Console]::Out.Write('AGAIN')");
    again.wait_for(b"AGAIN");
    assert_eq!(
        aterm_pty::conpty_host(again.master),
        Some(ConsoleHostKind::Inbox)
    );
    assert_eq!(aterm_pty::take_console_host_fallback(), None);
    again.enter();
    again.wait_eof();
}

// The default (aterm.toml with no `conpty`) is the Windows console host, with
// no note: nothing was asked for, so nothing failed.
#[test]
fn the_default_is_the_windows_console_host_and_logs_nothing() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::from_config(None));
    let _ = aterm_pty::take_console_host_fallback();
    let tab = spawn_powershell("[Console]::Out.Write('INBOX')");
    tab.wait_for(b"INBOX");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(ConsoleHostKind::Inbox)
    );
    assert_eq!(aterm_pty::take_console_host_fallback(), None);
    let report = aterm_pty::console_host_report();
    assert_eq!(report.kind, ConsoleHostKind::Inbox);
    assert!(!report.fell_back, "{report}");
    let probe = aterm_pty::probe_console_host(&ConsoleHostChoice::from_config(None));
    assert_eq!(
        probe, report,
        "the doctor's probe starts nothing for the default"
    );
    tab.enter();
    tab.wait_eof();
}

// The resync client, end to end: with an app's alternate screen up in the
// host, `resync_alt_screen` starts a second client in the same pseudoconsole
// and the host FORWARDS its `?1049l`, the bytes the terminal then leaves its
// own alternate screen on, ahead of the app's next output. An inbox session
// refuses: its host repaints instead of forwarding.
#[test]
fn the_resync_client_makes_openconsole_forward_the_leave() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let tab = spawn_powershell(
        "$e=[char]27; [Console]::Out.Write(\"$e[?1049hALT-UP\"); [Console]::Out.Flush(); \
         [void][Console]::In.ReadLine(); [Console]::Out.Write('AFTER')",
    );
    let mark = tab.wait_for(b"ALT-UP").len();
    match aterm_pty::conpty_host(tab.master) {
        Some(ConsoleHostKind::OpenConsole) => {
            aterm_pty::resync_alt_screen(tab.master).expect("the client starts");
            tab.wait_for_from(mark, b"\x1b[?1049l");
            tab.enter();
            let all = tab.wait_for_from(mark, b"AFTER");
            let tail = &all[mark..];
            let leave = tail.windows(8).position(|w| w == b"\x1b[?1049l");
            let after = tail.windows(5).position(|w| w == b"AFTER");
            assert!(leave.is_some(), "the host forwarded the leave");
            assert!(
                leave < after,
                "the leave precedes the app's next output: {:?}",
                String::from_utf8_lossy(tail)
            );
        }
        other => {
            assert_eq!(other, Some(ConsoleHostKind::Inbox));
            let err = aterm_pty::resync_alt_screen(tab.master).expect_err("inbox refuses");
            assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
            tab.enter();
        }
    }
    // The helper's own closing line read.
    tab.enter();
    tab.wait_eof();
}

// An inbox session never runs the client, whatever the machine has, and a key
// no session has is not found.
#[test]
fn an_inbox_session_refuses_the_resync() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Inbox);
    let tab = spawn_powershell("[Console]::Out.Write('READY')");
    tab.wait_for(b"READY");
    let err = aterm_pty::resync_alt_screen(tab.master).expect_err("inbox refuses");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(
        aterm_pty::resync_alt_screen(0x7fff_fff0)
            .expect_err("no such session")
            .kind(),
        std::io::ErrorKind::NotFound
    );
    tab.enter();
    tab.wait_eof();
}

// `auto` keeps a program of the MSYS2/Cygwin runtime on the Windows console
// host: under OpenConsole the runtime's post-resize drop takes a real key (the
// snapshot's measurement is on `console_host::policy::runs_a_posix_runtime`).
// The runtime is recognised by its DLL, so a copy of cmd.exe beside an
// `msys-2.0.dll` stands in for Git Bash here; the same program beside no
// runtime takes the host `auto` resolved.
#[test]
fn auto_keeps_msys2_runtime_programs_on_the_inbox_host() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let report = aterm_pty::console_host_report();
    let root = std::env::temp_dir().join(format!("aterm-msys-standin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (msys, native) = (
        root.join("msys").join("usr").join("bin"),
        root.join("plain").join("bin"),
    );
    std::fs::create_dir_all(&msys).unwrap();
    std::fs::create_dir_all(&native).unwrap();
    let cmd = std::path::Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(&cmd, msys.join("cmd.exe")).unwrap();
    std::fs::copy(&cmd, native.join("cmd.exe")).unwrap();
    std::fs::write(msys.join("msys-2.0.dll"), b"").unwrap();

    // `set /p` holds the console on a line read until the test types Enter.
    let under_msys = msys.join("cmd.exe").display().to_string();
    let tab = Tab::spawn(&[&under_msys, "/d", "/c", "echo MSYS-STANDIN& set /p _="]);
    tab.wait_for(b"MSYS-STANDIN");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(ConsoleHostKind::Inbox),
        "a runtime program keeps conhost under auto ({report})"
    );
    tab.enter();
    tab.wait_eof();
    drop(tab);

    let plain = native.join("cmd.exe").display().to_string();
    let tab = Tab::spawn(&[&plain, "/d", "/c", "echo NATIVE& set /p _="]);
    tab.wait_for(b"NATIVE");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(report.kind),
        "any other program takes the host auto resolved ({report})"
    );
    tab.enter();
    tab.wait_eof();
    drop(tab);
    let _ = std::fs::remove_dir_all(&root);
}
