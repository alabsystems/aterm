// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_uds::spawn`'s console rule, and `aterm_uds::spawnfd::command` — the
//! constructor every shipped child is built with, which applies that rule off
//! Unix — against real Windows processes: what console a child actually gets,
//! read from inside the child.
//!
//! The window (`aterm-gui.exe`) has no console, and a console child it starts
//! with no flag gets a new VISIBLE console window — the flash the module exists
//! to prevent (its docs carry the 2026-09-30 measurement). This file builds that
//! parent without ever being it on screen: the test binary re-runs ITSELF as a
//! PARENT started `Console::Detached` (no console at all, exactly the window's
//! state), and that parent starts <this binary> as a CHILD that reports what it
//! got — built by the rule's setter on a plain `Command` (`spawn::set_flags`,
//! `Console::Quiet`: what an allow-listed raw site does) in one test, and by
//! `spawnfd::command` itself in another, whose parent is also started the way
//! the windowed sibling is (`spawnfd::command`, then `set_flags` replacing its
//! choice with `Console::Detached`). Four distinct states are observed, so a
//! pass is never vacuous:
//!
//! * a quiet child of a parent with no console, either way it was built: a
//!   console (`GetConsoleProcessList` ≥ 1) with NO window (`GetConsoleWindow`
//!   NULL) — the fix;
//! * a detached child: no console at all (list 0) — so "has a console" above is
//!   a measurement, not a constant;
//! * a quiet child of a parent that HAS a console: that console, shared (list
//!   ≥ 2) — the branch that keeps a terminal's children on its screen and in
//!   reach of Ctrl+C;
//! * a probe on a ConPTY pseudoconsole (an aterm tab's console): a console
//!   whose `GetConsoleWindow` is NOT NULL — an invisible stand-in, nothing on
//!   screen — so the NULL the fix reads is a measurement the observable can
//!   fail, not a constant (on a machine whose own console is windowless, every
//!   other process here reads NULL).
//!
//! NO SPAWN HERE OPENS A CONSOLE WINDOW WHILE THE RULE IS WRONG. The
//! pseudoconsole control does not use the rule: its child's console is the
//! pseudoconsole, which shows nothing (every aterm tab is one). Every other
//! spawn is preceded by [`spawn_is_safe_here`], which refuses, before anything is
//! started, when `spawn::have_console` disagrees with an independent oracle
//! (`GetConsoleCP`, measured to answer the same in every console state) — a
//! wrong "I have a console" turns a quiet child into a flash — or when the
//! word `spawn::creation_flags` gives that child in this console state would
//! show a window ([`shows_a_console_window`]: a regression of the rule itself,
//! such as a quiet arm that drops `CREATE_NO_WINDOW`). What the guard cannot
//! see is a builder that stops APPLYING the rule (a `set_flags` that sets
//! nothing, a `spawnfd::command` that skips it): std has no getter for the word
//! a `Command` will be started with, so that stays a matter of reading
//! `set_flags` and the off-Unix `spawnfd::command`, a few lines each.
//!
//! WARNING: such a regression puts a console window on the screen of whoever
//! runs this file, and then the test fails. That is the negative control,
//! measured 2026-10-01 without anyone's screen: with the off-Unix
//! `spawnfd::command` reverted to a bare `Command::new`, the
//! `spawnfd::command` test below, run on a scratch desktop (`CreateDesktopW`,
//! under a `cmd.exe` started `CREATE_NO_WINDOW` there), failed with its child
//! at `window: true`, and a `ConsoleWindowClass` window titled with this binary
//! appeared on that desktop; the same test of the real constructor passed
//! there with no console window on either desktop.

#![cfg(windows)]

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::path::Path;

use aterm_uds::spawn::{self, Console};

/// Which role a re-run of this binary plays; unset in the suite itself.
const ROLE_ENV: &str = "ATERM_UDS_SPAWN_CONSOLE_ROLE";
/// Where a probe also writes its report, when set (the pseudoconsole control).
const REPORT_FILE_ENV: &str = "ATERM_UDS_SPAWN_CONSOLE_REPORT_FILE";
/// The prefix of every report line a role prints.
const REPORT: &str = "aterm-uds-spawn-report";
/// A hang detector, never a latency budget: a role answers in well under a
/// second when the machine is idle.
const HANG: Duration = Duration::from_secs(60);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleWindow() -> isize;
    fn GetConsoleProcessList(lpdwProcessList: *mut u32, dwProcessCount: u32) -> u32;
    fn GetConsoleCP() -> u32;
}

/// What a process can see of its own console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Facts {
    /// `GetConsoleWindow` is non-NULL: the console has a window (visible, or a
    /// ConPTY's invisible one). NULL for no console AND for a windowless one.
    window: bool,
    /// How many processes `GetConsoleProcessList` counts on the console; 0 for
    /// none.
    attached: u32,
    /// `aterm_uds::spawn::have_console`, the answer under test.
    have_console: bool,
}

fn own_facts() -> Facts {
    let mut first = 0u32;
    Facts {
        // SAFETY: no arguments; returns a window handle or 0.
        window: unsafe { GetConsoleWindow() } != 0,
        // SAFETY: a one-element out-buffer that outlives the call, its length
        // passed as 1; the count returned may exceed it, which is what is read.
        attached: unsafe { GetConsoleProcessList(&mut first, 1) },
        have_console: spawn::have_console(),
    }
}

/// Refuse, before anything is started, a child built with `console` (and no
/// extra flags, as every spawn here is) unless both hold:
///
/// * `have_console` agrees with the code-page oracle: a quiet child of a process
///   that wrongly believes it has a console gets NO flag, and from a process
///   without one that is a visible window;
/// * the word the rule gives that child in this console state, as the ORACLE
///   reads it, shows no window ([`shows_a_console_window`]). Without this, a
///   rule regressed to the bug it exists for (a quiet arm with no
///   `CREATE_NO_WINDOW`) would put a window on screen before the test failed.
fn spawn_is_safe_here(console: Console) -> Result<(), String> {
    // SAFETY: no arguments; returns the console's input code page, 0 without one.
    let oracle = unsafe { GetConsoleCP() } != 0;
    let answer = spawn::have_console();
    if answer != oracle {
        return Err(format!(
            "have_console() says {answer} but GetConsoleCP says {oracle}: refusing to start \
             a child that could open a visible console"
        ));
    }
    let word = spawn::creation_flags(console, 0, oracle);
    if shows_a_console_window(word, oracle) {
        return Err(format!(
            "creation_flags({console:?}) is {word:#x} in a process with{} a console: \
             refusing to start a child that would open a visible console",
            if oracle { "" } else { "out" }
        ));
    }
    Ok(())
}

/// Whether a CONSOLE-subsystem child started with `word` by a process that
/// `has_console` gets a console WINDOW: a new console always has one, and from a
/// process with no console so does any word without `CREATE_NO_WINDOW` or
/// `DETACHED_PROCESS` (Windows gives the child a new, visible console — the
/// flash; `spawn`'s module docs carry the measurements).
fn shows_a_console_window(word: u32, has_console: bool) -> bool {
    word & spawn::CREATE_NEW_CONSOLE != 0
        || (!has_console && word & (spawn::CREATE_NO_WINDOW | spawn::DETACHED_PROCESS) == 0)
}

/// `exe` with `console` decided by the rule's setter alone, on a plain
/// `Command`: what an allow-listed raw site (a transparent pass-through) does.
fn with_console(exe: &Path, console: Console) -> Command {
    let mut command = Command::new(exe);
    spawn::set_flags(&mut command, console, 0);
    command
}

/// `exe` built as the windowed sibling is: the shipped constructor, then
/// `set_flags` REPLACING its quiet choice with `Console::Detached`.
fn like_the_windowed_sibling(exe: &Path) -> Command {
    let mut command = aterm_uds::spawnfd::command(exe);
    spawn::set_flags(&mut command, Console::Detached, 0);
    command
}

fn report_line(who: &str, facts: Facts) -> String {
    format!(
        "{REPORT} {who} window={} attached={} have_console={}",
        u8::from(facts.window),
        facts.attached,
        u8::from(facts.have_console)
    )
}

/// Run THIS binary as `role`, limited to the one test `test` (whose body hands
/// the run to [`play_role`]), with `command`'s console choice; answer its stdout.
/// Panics when it does not end within [`HANG`] (it is killed first, by its own
/// handle) or exits nonzero.
fn run_as(mut command: Command, role: &str, test: &str) -> String {
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(ROLE_ENV, role)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("start this test binary in a role");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = out_tx.send(text);
    });
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        let _ = err_tx.send(text);
    });
    let deadline = Instant::now() + HANG;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the {role} role did not finish within {HANG:?}: {other:?}");
            }
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let stdout = out_rx.recv_timeout(left).unwrap_or_default();
    let stderr = err_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    assert!(
        status.success(),
        "the {role} role exited {status}\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    stdout
}

/// The report `who` printed, out of a role's stdout. Searched for, not matched
/// at a line's start: libtest has already printed `test <name> ... ` on the line
/// the role's report lands on.
fn facts_of(stdout: &str, who: &str) -> Facts {
    let mark = format!("{REPORT} {who} ");
    let line = stdout
        .lines()
        .find_map(|line| line.find(&mark).map(|at| &line[at..]))
        .unwrap_or_else(|| panic!("no report from {who} in:\n{stdout}"));
    let field = |key: &str| -> u32 {
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("no {key} in {line:?}"))
    };
    Facts {
        window: field("window") != 0,
        attached: field("attached"),
        have_console: field("have_console") != 0,
    }
}

/// When this run of the binary was started in a role, play it and answer
/// `true`; the calling test then returns at once. `test` is the caller's own
/// name, which a parent hands on to its child.
fn play_role(test: &str) -> bool {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return false;
    };
    match role.as_str() {
        "probe" => {
            let line = report_line("probe", own_facts());
            println!("{line}");
            // On a pseudoconsole stdout is the console itself, rendered as VT
            // text that nothing parses reliably; the report also goes to a file.
            if let Some(path) = std::env::var_os(REPORT_FILE_ENV) {
                std::fs::write(path, format!("{line}\n")).expect("write the report file");
            }
        }
        // "parent" builds its quiet child with the rule's setter on a plain
        // `Command`; "parent-spawnfd" with the shipped constructor.
        "parent" | "parent-spawnfd" => {
            println!("{}", report_line("parent", own_facts()));
            if let Err(why) = spawn_is_safe_here(Console::Quiet) {
                println!("{REPORT} refused {why}");
                std::process::exit(3);
            }
            let exe = std::env::current_exe().expect("this binary");
            let child = if role == "parent" {
                with_console(&exe, Console::Quiet)
            } else {
                aterm_uds::spawnfd::command(&exe)
            };
            let stdout = run_as(child, "probe", test);
            let facts = facts_of(&stdout, "probe");
            println!("{}", report_line("child", facts));
        }
        other => panic!("unknown role {other:?}"),
    }
    true
}

/// THE FIX: a parent with no console — the window's state — starts a quiet
/// child, and the child has a console with no window. Before the module the
/// same child had a console WITH a window: the flash.
#[test]
fn a_quiet_child_of_a_parent_with_no_console_gets_a_console_with_no_window() {
    const TEST: &str = "a_quiet_child_of_a_parent_with_no_console_gets_a_console_with_no_window";
    if play_role(TEST) {
        return;
    }
    spawn_is_safe_here(Console::Detached).unwrap();
    let exe = std::env::current_exe().expect("this test binary");
    let stdout = run_as(with_console(&exe, Console::Detached), "parent", TEST);
    let parent = facts_of(&stdout, "parent");
    assert_eq!(
        (parent.attached, parent.have_console),
        (0, false),
        "a detached parent has no console, and says so: {parent:?}"
    );
    let child = facts_of(&stdout, "child");
    assert!(
        child.attached >= 1 && child.have_console,
        "a quiet child of a parent with no console still gets a console: {child:?}"
    );
    assert!(
        !child.window,
        "…and that console has NO window — a window here is the flash: {child:?}"
    );
}

/// THE FIX, through the shipped constructor: a parent with no console, started
/// as the windowed sibling is (`spawnfd::command`, then `set_flags` replacing
/// its choice with `Console::Detached`), starts `spawnfd::command(<this
/// binary>)` — how every shipped child is built — and the child has a console
/// with no window. A `spawnfd::command` off Unix that dropped the console rule
/// (a plain `Command::new`) gave this child a console window and failed here
/// (measured on a scratch desktop: the module docs).
#[test]
fn spawnfd_command_from_a_parent_with_no_console_gives_a_console_with_no_window() {
    const TEST: &str =
        "spawnfd_command_from_a_parent_with_no_console_gives_a_console_with_no_window";
    if play_role(TEST) {
        return;
    }
    spawn_is_safe_here(Console::Detached).unwrap();
    let exe = std::env::current_exe().expect("this test binary");
    let stdout = run_as(like_the_windowed_sibling(&exe), "parent-spawnfd", TEST);
    let parent = facts_of(&stdout, "parent");
    assert_eq!(
        (parent.attached, parent.have_console),
        (0, false),
        "a parent started as the windowed sibling has no console, and says so: {parent:?}"
    );
    let child = facts_of(&stdout, "child");
    assert!(
        child.attached >= 1 && child.have_console,
        "spawnfd::command's child of a parent with no console still gets a console: {child:?}"
    );
    assert!(
        !child.window,
        "…and that console has NO window — a window here is the flash: {child:?}"
    );
}

/// NON-VACUITY, the first other state: a detached child reports NO console, so
/// the console the test above reads is one the quiet child was given, not one
/// every child of this suite would report.
#[test]
fn a_detached_child_has_no_console_at_all() {
    const TEST: &str = "a_detached_child_has_no_console_at_all";
    if play_role(TEST) {
        return;
    }
    spawn_is_safe_here(Console::Detached).unwrap();
    let exe = std::env::current_exe().expect("this test binary");
    let stdout = run_as(with_console(&exe, Console::Detached), "probe", TEST);
    let probe = facts_of(&stdout, "probe");
    assert_eq!(
        probe,
        Facts {
            window: false,
            attached: 0,
            have_console: false,
        },
        "DETACHED_PROCESS leaves the child no console"
    );
}

/// NON-VACUITY, the second other state: a parent that HAS a console — a quiet
/// child of this suite, which shares the suite's console or, when the suite has
/// none, gets a windowless one — starts a quiet child, and that child SHARES
/// the parent's console (two or more attached) instead of getting its own. This
/// is the branch a terminal's `aterm pkg …` takes: its child's output reaches
/// the screen and Ctrl+C reaches the child. Both levels are built by
/// `spawnfd::command`, the shipped constructor.
#[test]
fn a_quiet_child_of_a_parent_with_a_console_shares_it() {
    const TEST: &str = "a_quiet_child_of_a_parent_with_a_console_shares_it";
    if play_role(TEST) {
        return;
    }
    spawn_is_safe_here(Console::Quiet).unwrap();
    let exe = std::env::current_exe().expect("this test binary");
    let stdout = run_as(aterm_uds::spawnfd::command(&exe), "parent-spawnfd", TEST);
    let parent = facts_of(&stdout, "parent");
    assert!(
        parent.attached >= 1 && parent.have_console,
        "a quiet child of this suite has a console: {parent:?}"
    );
    let child = facts_of(&stdout, "child");
    assert!(
        child.attached >= 2 && child.have_console,
        "a quiet child of a console holder shares that console: {child:?}"
    );
}

/// `have_console` answers what the code-page oracle answers in this process,
/// whichever console state the suite was started in.
#[test]
fn have_console_agrees_with_the_code_page_oracle_here() {
    if play_role("have_console_agrees_with_the_code_page_oracle_here") {
        return;
    }
    spawn_is_safe_here(Console::Quiet).unwrap();
    let facts = own_facts();
    assert_eq!(facts.have_console, facts.attached > 0, "{facts:?}");
}

/// The guard is not vacuous: it refuses each word that shows a window — above
/// all the bug itself, a quiet child of a process with no console started with
/// no console flag — and passes the words the rule gives. Pure: nothing is
/// started, so the refused words are judged without ever being run.
#[test]
fn the_guard_refuses_every_word_that_would_show_a_console_window() {
    if play_role("the_guard_refuses_every_word_that_would_show_a_console_window") {
        return;
    }
    let group = spawn::CREATE_NEW_PROCESS_GROUP;
    for (word, has_console, shows) in [
        // From the window's state (no console).
        (0, false, true),
        (group, false, true),
        (spawn::CREATE_NEW_CONSOLE, false, true),
        (spawn::CREATE_NO_WINDOW, false, false),
        (spawn::CREATE_NO_WINDOW | group, false, false),
        (spawn::DETACHED_PROCESS, false, false),
        // From a console.
        (0, true, false),
        (group, true, false),
        (spawn::CREATE_NEW_CONSOLE, true, true),
        (spawn::DETACHED_PROCESS, true, false),
    ] {
        assert_eq!(
            shows_a_console_window(word, has_console),
            shows,
            "{word:#x} has_console {has_console}"
        );
    }
    // …and the rule as it stands passes it in both states, for both choices
    // these tests start children with.
    for has_console in [false, true] {
        for console in [Console::Quiet, Console::Detached] {
            assert!(
                !shows_a_console_window(
                    spawn::creation_flags(console, 0, has_console),
                    has_console
                ),
                "{console:?} has_console {has_console}"
            );
        }
    }
}

/// NON-VACUITY of `window`, with nothing on screen: a probe started on a ConPTY
/// pseudoconsole — the console of every aterm tab — reports a console whose
/// `GetConsoleWindow` is NOT NULL (a pseudoconsole hands it an invisible
/// stand-in window: a `PseudoConsoleWindow` created hidden and never shown,
/// measured with a WinEvent hook on Windows 11 build 26200, 2026-09-30).
/// Without this, `window` was never seen to read anything but NULL on a machine
/// whose own console is windowless (a review measured every process here at
/// `window=0`), so the fix's `window=0` could have been a constant. A hidden `CREATE_NEW_CONSOLE` child would read 1 too, but it makes
/// a real console window, which nothing in this file does.
#[test]
fn a_probe_on_a_pseudoconsole_sees_its_console_window() {
    const TEST: &str = "a_probe_on_a_pseudoconsole_sees_its_console_window";
    if play_role(TEST) {
        return;
    }
    let exe = std::env::current_exe().expect("this test binary");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let report = std::env::temp_dir().join(format!(
        "aterm-uds-spawn-console-{}-{nanos}.report",
        std::process::id()
    ));
    let code = pseudoconsole::run(
        &exe,
        &["--exact", TEST, "--nocapture", "--test-threads=1"],
        &[
            (ROLE_ENV, "probe".as_ref()),
            (REPORT_FILE_ENV, report.as_os_str()),
        ],
        HANG,
    );
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    let _ = std::fs::remove_file(&report);
    assert_eq!(
        code, 0,
        "the probe on a pseudoconsole exited {code}: {text:?}"
    );
    let probe = facts_of(&text, "probe");
    assert!(
        probe.window && probe.attached >= 1 && probe.have_console,
        "a probe on a pseudoconsole has a console, and `GetConsoleWindow` names a window \
         for it: {probe:?}"
    );
}

/// The one raw spawn here: a child on a fresh ConPTY pseudoconsole, the way
/// `aterm-pty` starts a tab (its `spawn` carries the reasons for each step).
mod pseudoconsole {
    use std::ffi::{OsStr, OsString, c_void};
    use std::io::{self, Read as _};
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use std::time::Duration;

    const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x0002_0016;
    const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
    const CREATE_UNICODE_ENVIRONMENT: u32 = 0x400;
    const STARTF_USESTDHANDLES: u32 = 0x100;
    const WAIT_OBJECT_0: u32 = 0;

    /// `COORD`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord {
        x: i16,
        y: i16,
    }

    /// `STARTUPINFOW` (64-bit layout, field for field).
    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_count_chars: u32,
        y_count_chars: u32,
        fill_attribute: u32,
        flags: u32,
        show_window: u16,
        reserved2_len: u16,
        reserved2: *mut u8,
        std_input: RawHandle,
        std_output: RawHandle,
        std_error: RawHandle,
    }

    /// `STARTUPINFOEXW`.
    #[repr(C)]
    struct StartupInfoExW {
        startup_info: StartupInfoW,
        attribute_list: *mut c_void,
    }

    /// `PROCESS_INFORMATION`.
    #[repr(C)]
    struct ProcessInformation {
        process: RawHandle,
        thread: RawHandle,
        process_id: u32,
        thread_id: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreatePipe(
            read: *mut RawHandle,
            write: *mut RawHandle,
            attributes: *mut c_void,
            size: u32,
        ) -> i32;
        fn CreatePseudoConsole(
            size: Coord,
            input: RawHandle,
            output: RawHandle,
            flags: u32,
            console: *mut RawHandle,
        ) -> i32;
        fn ClosePseudoConsole(console: RawHandle);
        fn InitializeProcThreadAttributeList(
            list: *mut c_void,
            count: u32,
            flags: u32,
            size: *mut usize,
        ) -> i32;
        fn UpdateProcThreadAttribute(
            list: *mut c_void,
            flags: u32,
            attribute: usize,
            value: *mut c_void,
            size: usize,
            previous: *mut c_void,
            returned: *mut usize,
        ) -> i32;
        fn DeleteProcThreadAttributeList(list: *mut c_void);
        fn CreateProcessW(
            application: *const u16,
            command_line: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit_handles: i32,
            flags: u32,
            environment: *mut c_void,
            directory: *const u16,
            startup: *const StartupInfoW,
            information: *mut ProcessInformation,
        ) -> i32;
        fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
        fn GetExitCodeProcess(process: RawHandle, code: *mut u32) -> i32;
        fn TerminateProcess(process: RawHandle, code: u32) -> i32;
    }

    /// An anonymous pipe, neither end inheritable: `(read, write)`.
    fn pipe() -> (OwnedHandle, OwnedHandle) {
        let (mut read, mut write) = (null_mut(), null_mut());
        // SAFETY: two out-pointers that outlive the call; NULL attributes (so
        // neither end is inheritable) and the default buffer size.
        let made = unsafe { CreatePipe(&mut read, &mut write, null_mut(), 0) };
        assert_ne!(made, 0, "CreatePipe: {}", io::Error::last_os_error());
        // SAFETY: both handles were just created, and nothing else owns them.
        unsafe {
            (
                OwnedHandle::from_raw_handle(read),
                OwnedHandle::from_raw_handle(write),
            )
        }
    }

    /// `s`, UTF-16 and NUL-terminated.
    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// Run `exe args…` on a fresh pseudoconsole, with `env` over this process's
    /// environment, and answer its exit code. The console's output is drained
    /// and dropped (a pseudoconsole whose output nobody reads stalls its
    /// client). Panics when it does not end within `hang`, killing it first by
    /// its own handle.
    pub(super) fn run(exe: &Path, args: &[&str], env: &[(&str, &OsStr)], hang: Duration) -> u32 {
        let (console_in, our_in) = pipe();
        let (our_out, console_out) = pipe();
        let mut console: RawHandle = null_mut();
        // SAFETY: two live pipe ends, which the pseudoconsole duplicates, and an
        // out-pointer that outlives the call.
        let hr = unsafe {
            CreatePseudoConsole(
                Coord { x: 120, y: 30 },
                console_in.as_raw_handle(),
                console_out.as_raw_handle(),
                0,
                &mut console,
            )
        };
        assert!(hr >= 0, "CreatePseudoConsole: HRESULT {hr:#x}");
        drop((console_in, console_out));
        let mut output = std::fs::File::from(our_out);
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = output.read_to_end(&mut sink);
        });

        let mut size = 0usize;
        // SAFETY: the size query (a NULL list): it fails with
        // ERROR_INSUFFICIENT_BUFFER and writes the size one attribute needs.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        let mut block = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = block.as_mut_ptr().cast::<c_void>();
        // SAFETY: a pointer-aligned buffer of at least `size` bytes, alive (and
        // never moved) until `DeleteProcThreadAttributeList` below.
        let made = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        assert_ne!(
            made,
            0,
            "InitializeProcThreadAttributeList: {}",
            io::Error::last_os_error()
        );
        // SAFETY: the initialized list; the attribute's VALUE is the
        // pseudoconsole handle itself, passed by value (the documented idiom,
        // as `aterm-pty` passes it).
        let set = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                console,
                size_of::<RawHandle>(),
                null_mut(),
                null_mut(),
            )
        };
        assert_ne!(
            set,
            0,
            "UpdateProcThreadAttribute: {}",
            io::Error::last_os_error()
        );

        let application = wide(exe.as_os_str());
        let mut line = OsString::from("\"");
        line.push(exe.as_os_str());
        line.push("\"");
        for arg in args {
            line.push(" ");
            line.push(arg);
        }
        let mut command_line = wide(&line);
        let mut environment: Vec<u16> = Vec::new();
        for (key, value) in std::env::vars_os()
            .filter(|(key, _)| !env.iter().any(|(name, _)| key.eq_ignore_ascii_case(name)))
            .chain(
                env.iter()
                    .map(|(name, value)| (OsString::from(name), OsString::from(value))),
            )
        {
            environment.extend(key.encode_wide());
            environment.push(u16::from(b'='));
            environment.extend(value.encode_wide());
            environment.push(0);
        }
        environment.push(0);

        // SAFETY: an all-zero STARTUPINFOEXW is a valid empty one; `cb`, the
        // flags and the attribute list are set below.
        let mut startup: StartupInfoExW = unsafe { std::mem::zeroed() };
        startup.startup_info.cb = u32::try_from(size_of::<StartupInfoExW>()).expect("small");
        // NULL std handles under STARTF_USESTDHANDLES bind the child's stdio to
        // the pseudoconsole instead of this suite's pipes (`aterm-pty`'s spawn
        // measured why).
        startup.startup_info.flags = STARTF_USESTDHANDLES;
        startup.attribute_list = list;
        // SAFETY: an all-zero PROCESS_INFORMATION is plain out-memory.
        let mut information: ProcessInformation = unsafe { std::mem::zeroed() };
        // SAFETY: every buffer is NUL-terminated (the environment block
        // double-NUL) and alive across the call, the startup info's list is
        // initialized, and no handle is inherited.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null_mut(),
                null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_mut_ptr().cast::<c_void>(),
                null(),
                &raw const startup.startup_info,
                &mut information,
            )
        };
        let why = io::Error::last_os_error();
        // SAFETY: the list initialized above, deleted once; the child holds its
        // own reference to the pseudoconsole.
        unsafe { DeleteProcThreadAttributeList(list) };
        assert_ne!(created, 0, "CreateProcessW on a pseudoconsole: {why}");
        // SAFETY: both handles were just returned to us, and nothing else owns them.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(information.process),
                OwnedHandle::from_raw_handle(information.thread),
            )
        };
        drop(thread);

        let ms = u32::try_from(hang.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: a live process handle we own.
        let waited = unsafe { WaitForSingleObject(process.as_raw_handle(), ms) };
        let mut code = u32::MAX;
        if waited == WAIT_OBJECT_0 {
            // SAFETY: a live process handle we own, and an out-pointer that
            // outlives the call.
            unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
        } else {
            // SAFETY: a live process handle we own: this child, nothing else.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        }
        // SAFETY: the pseudoconsole created above, closed once, with its output
        // still drained so the close cannot stall on a last frame; its input end
        // stays open until after (a broken input pipe ends the console early).
        unsafe { ClosePseudoConsole(console) };
        drop(our_in);
        assert_eq!(
            waited, WAIT_OBJECT_0,
            "the probe on a pseudoconsole did not finish within {hang:?}"
        );
        code
    }
}
