// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console-host lane end to end: a tab spawned under `conpty = "auto"`
//! runs in OpenConsole when this machine has one (a bundled copy, or the
//! installed Windows Terminal's) and passes a child's VT through verbatim; a
//! host that cannot start falls back to the Windows console host and the tab
//! still opens; the default (`conpty` unset) is the OpenConsole beside the
//! binary and never Windows Terminal's; `conpty = "inbox"` is the Windows
//! console host.
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

/// Whether an `aterm.exe` sits beside this test binary, as in an app folder
/// laid out to run it: its front door then serves the tab's console clients.
fn front_door_beside() -> bool {
    std::env::current_exe()
        .expect("this test binary")
        .with_file_name("aterm.exe")
        .is_file()
}

/// The console clients a shipped tab runs from the `aterm.exe` beside it, run
/// for one test from this crate's `aterm-pty-console-client`, which serves the
/// same `serve_console_client`, then from nothing again, so the other tests
/// keep the client-less process they expect. Never from an `aterm.exe` in
/// `deps`: that is whichever build of `aterm` last ran in the target, and an
/// older one does not know a newer client's flag.
struct ConsoleClients;

impl ConsoleClients {
    fn standin() -> Self {
        let standin = env!("CARGO_BIN_EXE_aterm-pty-console-client");
        aterm_pty::set_console_client(Some(standin.into()));
        ConsoleClients
    }
}

impl Drop for ConsoleClients {
    fn drop(&mut self) {
        aterm_pty::set_console_client(None);
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

// `conpty = "inbox"` is the Windows console host, with no note: the inbox
// host was asked for, so nothing failed.
#[test]
fn inbox_is_the_windows_console_host_and_logs_nothing() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::from_config(Some("inbox")));
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
    let probe = aterm_pty::probe_console_host(&ConsoleHostChoice::Inbox);
    assert_eq!(probe, report, "the doctor's probe starts nothing for inbox");
    tab.enter();
    tab.wait_eof();
}

// The default (aterm.toml with no `conpty`) is the OpenConsole beside this
// binary: with one there (a bundle's layout) the tab runs in it; with none (a
// bare test binary) the tab runs in the Windows console host, the log hears
// why once, and Windows Terminal's host is NOT taken, as `auto` would. The
// branch taken is ASSERTED, never skipped, and the doctor's probe, which
// starts the host, names the same one.
#[test]
fn the_default_is_the_bundled_host_and_never_windows_terminals() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let bundled = std::env::current_exe()
        .expect("this test binary")
        .with_file_name("OpenConsole.exe");
    aterm_pty::set_console_host(ConsoleHostChoice::from_config(None));
    let _ = aterm_pty::take_console_host_fallback();
    let tab = spawn_powershell("[Console]::Out.Write('DEFAULT')");
    tab.wait_for(b"DEFAULT");
    let kind = aterm_pty::conpty_host(tab.master);
    let note = aterm_pty::take_console_host_fallback();
    let report = aterm_pty::console_host_report();
    if bundled.is_file() {
        assert_eq!(
            kind,
            Some(ConsoleHostKind::OpenConsole),
            "{report}; {note:?}"
        );
        assert_eq!(report.kind, ConsoleHostKind::OpenConsole);
        assert!(
            report.line.contains(&bundled.display().to_string()),
            "{report}"
        );
        assert_eq!(note, None);
    } else {
        assert_eq!(kind, Some(ConsoleHostKind::Inbox), "{report}");
        assert_eq!(report.kind, ConsoleHostKind::Inbox);
        assert!(report.fell_back, "{report}");
        assert!(
            report
                .line
                .starts_with("Windows console host; no OpenConsole.exe beside aterm"),
            "{report}"
        );
        assert_eq!(note.as_deref(), Some(report.line.as_str()), "noted once");
        assert!(!report.line.contains("Windows Terminal "), "{report}");
    }
    let probe = aterm_pty::probe_console_host(&ConsoleHostChoice::from_config(None));
    assert_eq!(probe, report, "the doctor's probe names the same host");
    tab.enter();
    tab.wait_eof();
}

// The resync client, end to end: with an app's alternate screen up in the
// host, `resync_alt_screen` starts a second client in the same pseudoconsole
// and the host FORWARDS its `?1049l`, the bytes the terminal then leaves its
// own alternate screen on, ahead of the app's next output. With no client
// image beside the process it starts nothing and says so, and the screen
// stays up. An inbox session refuses: its host repaints instead of forwarding.
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
            if !front_door_beside() {
                let refused = aterm_pty::resync_alt_screen(tab.master)
                    .expect_err("no client image beside this test binary");
                assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported, "{refused}");
            }
            let _clients = ConsoleClients::standin();
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

/// A console program that puts its console's input in raw mode (0x288:
/// window and extended flags and VT input, no processed, line or echo input,
/// as Win32-OpenSSH and wsl.exe set it once and then read keys), says `SET=`
/// what the console then reports, goes onto the alternate screen and waits for
/// a key; then says `MODE=` what the console reports, and puts back the mode
/// it started with for the helper's closing line read (a raw `ReadLine` would
/// wait past the Enter for the line feed after it).
const RAW_MODE_SCRIPT: &str = "$k = Add-Type -Namespace AtermPtyTest -Name Con -PassThru -MemberDefinition \
     '[DllImport(\"kernel32.dll\")] public static extern IntPtr GetStdHandle(int n); \
     [DllImport(\"kernel32.dll\")] public static extern bool GetConsoleMode(IntPtr h, out uint m); \
     [DllImport(\"kernel32.dll\")] public static extern bool SetConsoleMode(IntPtr h, uint m);'; \
     $h = $k::GetStdHandle(-10); $was = [uint32]0; [void]$k::GetConsoleMode($h, [ref]$was); \
     [void]$k::SetConsoleMode($h, 0x288); \
     $m = [uint32]0; [void]$k::GetConsoleMode($h, [ref]$m); $e = [char]27; \
     [Console]::Out.Write((\"SET=0x{0:x};$e[?1049hRAW-UP\" -f $m)); [Console]::Out.Flush(); \
     [void][Console]::In.Read(); [void]$k::GetConsoleMode($h, [ref]$m); \
     [Console]::Out.Write(('MODE=0x{0:x};' -f $m)); [void]$k::SetConsoleMode($h, $was)";

/// The hex value after `key` in `bytes`, up to its `;`.
fn mode_after(bytes: &[u8], key: &str) -> Option<u32> {
    let text = String::from_utf8_lossy(bytes);
    let at = text.find(key)? + key.len();
    let hex = text[at..].split(';').next()?.strip_prefix("0x")?;
    u32::from_str_radix(hex, 16).ok()
}

// The resync client touches the screen and nothing else: the input mode the
// tab's program set reads the same after the host has left its alternate
// screen. The console's input mode is shared by every client of the tab, and
// the first client, `cmd.exe /c set /p`, turned processed, line and
// echo input back on as it started (0x288 read 0x28f), so a Ctrl+C at an ssh
// prompt became a CTRL_C_EVENT that ended ssh. The negative control is that
// client: this test fails with it.
#[test]
fn the_resync_client_leaves_the_console_input_mode_alone() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let _clients = ConsoleClients::standin();
    let tab = spawn_powershell(RAW_MODE_SCRIPT);
    let seen = tab.wait_for(b"RAW-UP");
    let set = mode_after(&seen, "SET=").expect("the program read its mode back");
    assert_eq!(
        set & 0x7,
        0,
        "raw: no processed, line or echo input ({set:#x})"
    );
    let mark = seen.len();
    match aterm_pty::conpty_host(tab.master) {
        Some(ConsoleHostKind::OpenConsole) => {
            aterm_pty::resync_alt_screen(tab.master).expect("the client starts");
            tab.wait_for_from(mark, b"\x1b[?1049l");
            tab.enter();
            let seen = tab.wait_for_from(mark, b"MODE=");
            let at = mark
                + seen[mark..]
                    .windows(5)
                    .position(|w| w == b"MODE=")
                    .expect("just seen");
            let after = tab.wait_for_from(at, b";");
            let mode = mode_after(&after[at..], "MODE=").expect("the mode after");
            assert_eq!(
                mode, set,
                "the input mode after the resync: {mode:#x}, set {set:#x}"
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

/// A console program whose input is in `mode` (0x288: raw, as PSReadLine reads
/// it; 0x287: line mode, as cmd.exe's prompt reads it) that goes onto the
/// alternate screen, draws a prompt at its top left (the cursor then at row 0,
/// column 4) and waits for a key, then puts its mode back and says `AFTER`.
fn prompt_on_the_alternate_screen(mode: u32) -> String {
    format!(
        "$k = Add-Type -Namespace AtermPtyCarry -Name Con -PassThru -MemberDefinition \
         '[DllImport(\"kernel32.dll\")] public static extern IntPtr GetStdHandle(int n); \
         [DllImport(\"kernel32.dll\")] public static extern bool GetConsoleMode(IntPtr h, out uint m); \
         [DllImport(\"kernel32.dll\")] public static extern bool SetConsoleMode(IntPtr h, uint m);'; \
         $h = $k::GetStdHandle(-10); $was = [uint32]0; [void]$k::GetConsoleMode($h, [ref]$was); \
         [void]$k::SetConsoleMode($h, {mode}); $e = [char]27; \
         [Console]::Out.Write(\"$e[?1049h$e[2J$e[3;1HCARRY-UP$e[1;1HPS> \"); [Console]::Out.Flush(); \
         [void][Console]::ReadKey($true); [void]$k::SetConsoleMode($h, $was); [Console]::Out.Write('AFTER')"
    )
}

// The leave's carry under a real OpenConsole: the client writes it right after
// the switch, in the same write, for a raw reader whose cursor is still the one
// the carry was made for, and leaves it out for a cursor that moved since and for
// a reader in line mode (whose read the switch ends: cmd.exe prompts again).
#[test]
fn the_resync_client_carries_the_prompt_only_for_a_raw_reader_at_its_cursor() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let _clients = ConsoleClients::standin();
    for (mode, cursor, carried) in [
        (0x288, (0, 4), true),
        (0x288, (0, 5), false),
        (0x287, (0, 4), false),
    ] {
        let tab = spawn_powershell(&prompt_on_the_alternate_screen(mode));
        tab.wait_for(b"CARRY-UP");
        let mark = tab.wait_for(b"\x1b[1;1HPS> ").len();
        match aterm_pty::conpty_host(tab.master) {
            Some(ConsoleHostKind::OpenConsole) => {
                aterm_pty::resync_alt_screen_carrying(tab.master, cursor, "<CARRIED>")
                    .expect("the client starts");
                tab.wait_for_from(mark, b"\x1b[?1049l");
                tab.enter();
                let all = tab.wait_for_from(mark, b"AFTER");
                let tail = &all[mark..];
                let at = |what: &[u8]| tail.windows(what.len()).position(|w| w == what);
                let what = String::from_utf8_lossy(tail);
                assert_eq!(
                    at(b"<CARRIED>").is_some(),
                    carried,
                    "{mode:#x} {cursor:?}: {what:?}"
                );
                if carried {
                    assert!(at(b"\x1b[?1049l") < at(b"<CARRIED>"), "{what:?}");
                    assert!(at(b"<CARRIED>") < at(b"AFTER"), "{what:?}");
                }
            }
            other => {
                assert_eq!(other, Some(ConsoleHostKind::Inbox));
                let err = aterm_pty::resync_alt_screen_carrying(tab.master, cursor, "<CARRIED>")
                    .expect_err("inbox refuses");
                assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
                tab.enter();
            }
        }
        // The helper's own closing line read.
        tab.enter();
        tab.wait_eof();
    }
}

// A GUI-subsystem program never connects to its console, so a console host
// never answers it: the tab neither waits for an answer nor ends the program
// and starts it again on the Windows console host, and holds nothing against
// the host. The program runs once, nothing is noted, and the next console tab
// gets the host `auto` resolved. The first health check ran it
// twice (the first copy killed at 250 ms) and turned OpenConsole off for every
// later tab.
#[test]
fn a_gui_program_runs_once_and_keeps_openconsole_for_later_tabs() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let _ = aterm_pty::take_console_host_fallback();
    let report = aterm_pty::console_host_report();
    let marker = std::env::temp_dir().join(format!("aterm-gui-standin-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let marker_arg = marker.display().to_string();
    let gui = Tab::spawn(&[env!("CARGO_BIN_EXE_aterm-pty-gui-standin"), &marker_arg]);
    gui.wait_eof();
    let runs = std::fs::read_to_string(&marker).unwrap_or_default();
    let _ = std::fs::remove_file(&marker);
    assert_eq!(runs.lines().count(), 1, "the program ran once: {runs:?}");
    assert_eq!(
        aterm_pty::take_console_host_fallback(),
        None,
        "nothing failed"
    );
    drop(gui);

    let tab = spawn_powershell("[Console]::Out.Write('LATER')");
    tab.wait_for(b"LATER");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(report.kind),
        "{report}"
    );
    assert_eq!(aterm_pty::console_host_report(), report);
    tab.enter();
    tab.wait_eof();
}

// `--show-config`'s preview names the host a tab under that choice gets, as the
// process's own resolution does, and leaves the process's choice alone: a tab
// spawned after previewing `auto` under the inbox choice is still inbox.
#[test]
fn the_preview_names_the_host_and_leaves_the_choice_alone() {
    let _serial = HOST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    aterm_pty::set_console_host(ConsoleHostChoice::Inbox);
    let inbox = aterm_pty::preview_console_host(&ConsoleHostChoice::Inbox);
    assert_eq!(inbox, aterm_pty::console_host_report());
    let auto = aterm_pty::preview_console_host(&ConsoleHostChoice::Auto);
    let tab = spawn_powershell("[Console]::Out.Write('STILL-INBOX')");
    tab.wait_for(b"STILL-INBOX");
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(ConsoleHostKind::Inbox),
        "the preview of {auto} changed no choice"
    );
    tab.enter();
    tab.wait_eof();
    aterm_pty::set_console_host(ConsoleHostChoice::Auto);
    let _ = aterm_pty::take_console_host_fallback();
    let resolved = aterm_pty::console_host_report();
    assert_eq!(auto.kind, resolved.kind, "{auto} vs {resolved}");
    if resolved.kind == ConsoleHostKind::OpenConsole {
        assert_eq!(auto, resolved, "the preview names the same file");
    }
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
// host when no guard client can run (no `aterm.exe` beside this test binary):
// under OpenConsole the runtime's post-resize drop would take a real key
// (`console_host::policy::runs_a_posix_runtime` has the numbers). With one
// beside it (a build of `aterm` in the same target puts one in `deps`, and an
// app folder has one) the guard runs and the program takes the host `auto`
// resolved; the branch taken is asserted. The runtime is recognised by its
// DLL, so a copy of cmd.exe beside an `msys-2.0.dll` stands in for Git Bash
// here; the same program beside no runtime takes the host `auto` resolved.
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
    let guarded = front_door_beside();
    assert_eq!(
        aterm_pty::conpty_host(tab.master),
        Some(if guarded {
            report.kind
        } else {
            ConsoleHostKind::Inbox
        }),
        "a runtime program under auto, guarded={guarded} ({report})"
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
