// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_uds::spawn`'s console rule, and `aterm_uds::spawnfd::command` — the constructor every
//! shipped child is built with, which applies that rule off Unix — against real Windows processes:
//! what console a child actually gets, read from inside the child.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

#![cfg(windows)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: the plain std Command a reviewed raw site builds is what is measured"
)]

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::path::Path;

use aterm_uds::spawn::{self, Console};

const ROLE_ENV: &str = "ATERM_UDS_SPAWN_CONSOLE_ROLE";
const REPORT_FILE_ENV: &str = "ATERM_UDS_SPAWN_CONSOLE_REPORT_FILE";
const REPORT: &str = "aterm-uds-spawn-report";
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
    /// ConPTY's invisible one).
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
        // SAFETY: a one-element out-buffer that outlives the call, its length passed as 1; the
        // count returned may exceed it, which is what is read.
        attached: unsafe { GetConsoleProcessList(&mut first, 1) },
        have_console: spawn::have_console(),
    }
}

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
    if spawn::shows_a_console_window(word, oracle) {
        return Err(format!(
            "creation_flags({console:?}) is {word:#x} in a process with{} a console: \
             refusing to start a child that would open a visible console",
            if oracle { "" } else { "out" }
        ));
    }
    Ok(())
}

fn with_console(exe: &Path, console: Console) -> Command {
    let mut command = Command::new(exe);
    match console {
        Console::Quiet => spawn::set_flags(&mut command, 0),
        Console::Detached => spawn::set_gui_image_flags(&mut command),
    };
    command
}

fn like_the_windowed_sibling(exe: &Path) -> Command {
    let mut command = aterm_uds::spawnfd::command(exe);
    spawn::set_gui_image_flags(&mut command);
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

fn play_role(test: &str) -> bool {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return false;
    };
    match role.as_str() {
        "probe" => {
            let line = report_line("probe", own_facts());
            println!("{line}");
            if let Some(path) = std::env::var_os(REPORT_FILE_ENV) {
                std::fs::write(path, format!("{line}\n")).expect("write the report file");
            }
        }
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

#[test]
fn have_console_agrees_with_the_code_page_oracle_here() {
    if play_role("have_console_agrees_with_the_code_page_oracle_here") {
        return;
    }
    spawn_is_safe_here(Console::Quiet).unwrap();
    let facts = own_facts();
    assert_eq!(facts.have_console, facts.attached > 0, "{facts:?}");
}

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

    fn pipe() -> (OwnedHandle, OwnedHandle) {
        let (mut read, mut write) = (null_mut(), null_mut());
        // SAFETY: two out-pointers that outlive the call; NULL attributes (so neither end is
        // inheritable) and the default buffer size.
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

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// Run `exe args…` on a fresh pseudoconsole, with `env` over this process's environment, and answer its exit code.
    pub(super) fn run(exe: &Path, args: &[&str], env: &[(&str, &OsStr)], hang: Duration) -> u32 {
        let (console_in, our_in) = pipe();
        let (our_out, console_out) = pipe();
        let mut console: RawHandle = null_mut();
        // SAFETY: two live pipe ends, which the pseudoconsole duplicates, and an out-pointer that
        // outlives the call.
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
        // SAFETY: the size query (a NULL list): it fails with ERROR_INSUFFICIENT_BUFFER and writes
        // the size one attribute needs.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        let mut block = vec![0usize; size.div_ceil(size_of::<usize>())];
        let list = block.as_mut_ptr().cast::<c_void>();
        // SAFETY: a pointer-aligned buffer of at least `size` bytes, alive (and never moved) until
        // `DeleteProcThreadAttributeList` below.
        let made = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        assert_ne!(
            made,
            0,
            "InitializeProcThreadAttributeList: {}",
            io::Error::last_os_error()
        );
        // SAFETY: the initialized list; the attribute's VALUE is the pseudoconsole handle itself,
        // passed by value (the documented idiom, as `aterm-pty` passes it).
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

        // SAFETY: an all-zero STARTUPINFOEXW is a valid empty one; `cb`, the flags and the
        // attribute list are set below.
        let mut startup: StartupInfoExW = unsafe { std::mem::zeroed() };
        startup.startup_info.cb = u32::try_from(size_of::<StartupInfoExW>()).expect("small");
        startup.startup_info.flags = STARTF_USESTDHANDLES;
        startup.attribute_list = list;
        // SAFETY: an all-zero PROCESS_INFORMATION is plain out-memory.
        let mut information: ProcessInformation = unsafe { std::mem::zeroed() };
        // SAFETY: every buffer is NUL-terminated (the environment block double-NUL) and alive
        // across the call, the startup info's list is initialized, and no handle is inherited.
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
        // SAFETY: the list initialized above, deleted once; the child holds its own reference to
        // the pseudoconsole.
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
            // SAFETY: a live process handle we own, and an out-pointer that outlives the call.
            unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
        } else {
            // SAFETY: a live process handle we own: this child, nothing else.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        }
        // SAFETY: the pseudoconsole created above, closed once, with its output still drained so
        // the close cannot stall on a last frame; its input end stays open until after (a broken
        // input pipe ends the console early).
        unsafe { ClosePseudoConsole(console) };
        drop(our_in);
        assert_eq!(
            waited, WAIT_OBJECT_0,
            "the probe on a pseudoconsole did not finish within {hang:?}"
        );
        code
    }
}
