// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resize guard's OpenConsole half: a second console client, started into a
//! tab's pseudoconsole after a resize, that writes ONE `MENU_EVENT` record into
//! the console's input and exits, for the MSYS2 runtime to drop in place of the
//! user's first key ([`super::resize_guard`] has the account of the drop).
//!
//! WHY A CLIENT. OpenConsole (1.24 and 1.25, read in microsoft/terminal's
//! `InputBuffer::_WriteBuffer` and `TerminalInput::HandleKey` and measured)
//! turns every KEY record a terminal sends into text for a reader in VT-input
//! mode: a key-up, a modifier and the conhost guard's Shift key-up become
//! nothing. Only a record that is not a key passes such a reader untouched,
//! and a terminal cannot make one (mouse, focus and window reports reach a
//! VT-input reader as text too); `WriteConsoleInputW` from a client of the
//! console can. A `MENU_EVENT` is the record nothing reads: the runtime skips
//! it (`process_input_message`'s default arm), and so do .NET's `ReadKey`,
//! PSReadLine, cmd's line editor and every `ReadConsoleW` caller.
//!
//! Measured 2026-10-17 on this box (Git for Windows bash 5.3.9, MSYS2 runtime
//! 3.6.7, the audit's msys.ps1: 24 resizes, each followed by a typed or pasted
//! line): with no guard, Git Bash inside a cmd tab kept 13 of 24 lines whole
//! under OpenConsole 1.24.2610.01001 and 17 of 24 under 1.25.2609.30003, and a
//! Git Bash tab kept 20 and 15; with one `MENU_EVENT` written by a client after
//! each resize, 24 of 24 in all four. Waiting does not help: the msys.ps1 input
//! already follows its resize by 400 ms, because the runtime's discard is a
//! blocking read that takes whatever record comes next, whenever.
//!
//! WHO WRITES IT: `aterm.exe`, the console image beside the running one, started
//! with [`FLAG`] by [`guard`] (the front door's `main` calls
//! [`serve_console_client`] before anything else). It is started only for
//! a session in OpenConsole, before the first input write after a size change
//! (the terminal's answers to queries go past it: `super::resize_guard`,
//! REPLIES), or 50 ms after the change when no input came (THE STALL there),
//! when a process of the MSYS2/Cygwin runtime runs in the tab (in its job or
//! below its shell, [`runtime_in_tab`]), and an input write waits for it, once
//! per resize, in those tabs alone: the client run by hand in a
//! pwsh 7 tab took 11 to 38 ms (2026-10-17, this box under build load; about
//! 130 ms the first time), and 24 of 24 lines came through whole in a Git Bash
//! tab, inside cmd and inside pwsh 7, under 1.25 beside aterm and 1.24 by path.
//!
//! THE SECOND CLIENT, [`LEAVE_FLAG`]: the alternate-screen resync
//! (`super::resync_alt_screen`) has the same image write [`LEAVE_SEQUENCE`] to
//! the console's screen, so OpenConsole leaves the alternate buffer a dead app
//! left up and forwards the switch, then the prompt the shell drew on that
//! buffer when it [`Carry`]s one (`super::resync_alt_screen_carrying`). It
//! writes the screen and nothing else; the input mode it only reads. The
//! console's input mode belongs to every client of the tab, and the program
//! the user still runs there set it: Win32-OpenSSH and wsl.exe clear processed,
//! line and echo input once and then read raw keys. `cmd.exe /c set /p`, the
//! first client, set all three again as it started (measured 2026-10-17: a
//! raw 0x288 read 0x28f after it, a mouse reader's 0x1f6 read 0x1e7), so the
//! next Ctrl+C reached an ssh session as a CTRL_C_EVENT and ended it; it also
//! prints a refusal and waits for a key where policy disables cmd, and writes
//! nothing where command extensions are off.

use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_types::MutexExt;

use super::WinSession;
use super::cmdline::wide_nul;
use super::ffi::{self, HANDLE, Handle};
use super::openconsole;
use crate::console_host::policy;

/// The argument that makes `aterm.exe` the guard's client: `aterm.exe
/// --conpty-input-guard <cols> <rows>`.
pub(crate) const FLAG: &str = "--conpty-input-guard";

/// The argument that makes `aterm.exe` the alternate-screen leave's client:
/// `aterm.exe --conpty-leave-alt-screen`, or with a carry,
/// `aterm.exe --conpty-leave-alt-screen <row> <col> <hex>` ([`Carry`]).
pub(crate) const LEAVE_FLAG: &str = "--conpty-leave-alt-screen";

/// The most hex digits a carry's command line takes (half as many bytes of VT): well
/// inside `CreateProcessW`'s 32767 characters. A prompt longer than that is left on the
/// dead screen's archive, and the plain leave runs.
const CARRY_HEX_MAX: usize = 24_000;

/// What the leave client writes after the switch so the prompt the shell drew on the
/// dead screen comes along (aterm-core's `Terminal::dead_alt_screen_carry`), and the
/// cursor (0-based row and column of the screen) the host must still have for it to
/// apply: one the shell moved since was composed for a screen that is gone.
#[derive(Debug, PartialEq, Eq)]
struct Carry {
    row: u16,
    col: u16,
    text: String,
}

/// What the leave client writes: the end of a full-screen app that died before
/// it could write its own. Its keypad and cursor keys back to normal (`rmkx`),
/// its mouse reports off, the reports no shell asks for off (in-band size, 2048,
/// and colour scheme, 2031: vim sets both, and a killed vim left pwsh and cmd
/// typing `CSI 48 ; … t` at every resize, measured 2026-10-18), then its
/// `rmcup`, the switch OpenConsole forwards and the terminal leaves its own
/// alternate screen on. The resets reach the host's own input translation, and
/// OpenConsole forwards them ahead of the switch (1.24.2610.01001, 2026-10-17;
/// 2048 and 2031, which the host does not know, verbatim) to the terminal,
/// which releases the same modes when the switch arrives anyway (`aterm_core`'s
/// `take_dead_alt_screen`). Bracketed paste (2004) is not among them: the
/// host's own flag serves only its window's clipboard, which a pseudoconsole
/// never pastes from, and a forwarded `?2004l` would land with the switch,
/// which can follow bash's or zsh's own `?2004h` for the new prompt. The
/// terminal turns the dead app's paste off at the prompt itself.
pub(crate) const LEAVE_SEQUENCE: &str = "\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\
                                         \x1b[?1006l\x1b[?2048l\x1b[?2031l\x1b[?1049l";

/// One of the clients `aterm.exe` can be started as ([`client_args`]).
#[derive(Debug, PartialEq, Eq)]
enum Client {
    /// [`FLAG`], with the size its session last sent.
    Guard { cols: i16, rows: i16 },
    /// [`LEAVE_FLAG`], with the prompt it carries, if any.
    LeaveAltScreen(Option<Carry>),
    /// One of the flags, with arguments it does not take: it writes nothing.
    Malformed,
}

/// Exit code of a client started with arguments it does not take.
const USAGE: u8 = 64;

/// How long a write waits for its client to start, connect, write and exit.
/// A client that is still running then finishes on its own.
const CLIENT_WAIT_MS: u32 = 1000;

/// How long the client waits for the console to take the size its session last
/// sent before it writes anyway (the resize went out over the signal pipe before
/// the client was started, so it is normally already in).
const SIZE_WAIT: Duration = Duration::from_millis(250);

/// The front door's hook: when this process was started as one of a tab's
/// console clients ([`FLAG`], [`LEAVE_FLAG`]), do its one write and return the
/// exit code; otherwise `None`, and the caller carries on. Called first thing
/// in `main`.
#[must_use]
pub fn serve_console_client() -> Option<std::process::ExitCode> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = match client_args(&args)? {
        Client::Guard { cols, rows } => write_guard(cols, rows),
        Client::LeaveAltScreen(carry) => write_leave(carry.as_ref()),
        Client::Malformed => USAGE,
    };
    Some(std::process::ExitCode::from(code))
}

/// The client `args` (argv after argv[0]) start, `None` when they start none.
fn client_args(args: &[OsString]) -> Option<Client> {
    let (flag, rest) = args.split_first()?;
    if flag == LEAVE_FLAG {
        return Some(match rest {
            [] => Client::LeaveAltScreen(None),
            [row, col, hex] => carry_args(row, col, hex).map_or(Client::Malformed, |carry| {
                Client::LeaveAltScreen(Some(carry))
            }),
            _ => Client::Malformed,
        });
    }
    if flag != FLAG {
        return None;
    }
    let num = |a: &OsString| a.to_str()?.parse::<i16>().ok().filter(|n| *n > 0);
    let size = match rest {
        [cols, rows] => num(cols).zip(num(rows)),
        _ => None,
    };
    Some(
        size.map_or(Client::Malformed, |(cols, rows)| Client::Guard {
            cols,
            rows,
        }),
    )
}

/// A carry's three arguments: the cursor row and column, then the VT as hex (its UTF-8
/// bytes, two lowercase or uppercase digits each). `None` for anything else.
fn carry_args(row: &OsString, col: &OsString, hex: &OsString) -> Option<Carry> {
    let num = |a: &OsString| a.to_str()?.parse::<u16>().ok();
    let hex = hex.to_str()?;
    if hex.is_empty() || hex.len() > CARRY_HEX_MAX {
        return None;
    }
    Some(Carry {
        row: num(row)?,
        col: num(col)?,
        text: String::from_utf8(hex_decode(hex)?).ok()?,
    })
}

/// `bytes` as hex, two lowercase digits each.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The bytes `hex` spells, `None` for an odd length or a non-hex digit.
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// `CONIN$` or `CONOUT$` of this process's console, for reading and writing
/// (`INVALID_HANDLE_VALUE` inside when there is none).
fn open_console(name: &str) -> Handle {
    let name = wide_nul(OsStr::new(name));
    // SAFETY: a NUL-terminated name and no template; the handle is owned below.
    Handle::new(unsafe {
        ffi::CreateFileW(
            name.as_ptr(),
            ffi::GENERIC_READ | ffi::GENERIC_WRITE,
            ffi::FILE_SHARE_ALL,
            std::ptr::null_mut(),
            ffi::OPEN_EXISTING,
            0,
            0,
        )
    })
}

/// The leave client's body: write [`LEAVE_SEQUENCE`] to the console's active
/// screen buffer, the dead app's alternate one, turning VT processing on for
/// that buffer first if the app left it off (the buffer goes with the switch),
/// then the `carry` when [`carry_applies`]: one write, so nothing the shell
/// writes lands between the switch and the prompt it brings. The input handle
/// is opened only to read its mode. 0 when the write went out whole.
fn write_leave(carry: Option<&Carry>) -> u8 {
    let conout = open_console("CONOUT$");
    if conout.raw() == ffi::INVALID_HANDLE_VALUE {
        return 2;
    }
    let mut mode = 0u32;
    // SAFETY: a live console output handle and an out-param.
    if unsafe { ffi::GetConsoleMode(conout.raw(), &mut mode) } == 0 {
        return 2;
    }
    let vt = ffi::ENABLE_PROCESSED_OUTPUT | ffi::ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    // SAFETY: the same handle; only this output buffer's mode changes.
    if mode & vt != vt && unsafe { ffi::SetConsoleMode(conout.raw(), mode | vt) } == 0 {
        return 3;
    }
    let mut leave = LEAVE_SEQUENCE.to_string();
    if let Some(carry) = carry
        && carry_applies(carry, host_cursor(&conout), shell_reads_in_line_mode())
    {
        leave.push_str(&carry.text);
    }
    let text: Vec<u16> = leave.encode_utf16().collect();
    let mut written = 0u32;
    // SAFETY: a live console output handle, a buffer of the stated length, an out-param.
    let ok = unsafe {
        ffi::WriteConsoleW(
            conout.raw(),
            text.as_ptr(),
            text.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    } != 0;
    if ok && written as usize == text.len() {
        0
    } else {
        3
    }
}

/// Whether the leave writes `carry`: only while the host's cursor (`cursor`, `None`
/// when unread) is the one the carry was composed for, since a shell that moved it
/// since drew on a screen the carry does not show, and only when the shell reads raw
/// (`line_mode`, `None` when unread). A console read in line mode is conhost's own
/// line editor, and the switch ends it: cmd.exe printed its prompt again on the
/// primary screen within a millisecond of the switch (measured 2026-10-18), where a
/// carried copy would stand above it, the screen scrolled for nothing.
fn carry_applies(carry: &Carry, cursor: Option<(u16, u16)>, line_mode: Option<bool>) -> bool {
    cursor == Some((carry.row, carry.col)) && line_mode == Some(false)
}

/// The host's cursor on its active screen buffer, as the 0-based row and column of
/// its window; `None` when it cannot be read.
fn host_cursor(conout: &Handle) -> Option<(u16, u16)> {
    let mut info = ffi::CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: a live console output handle and an out-param.
    if unsafe { ffi::GetConsoleScreenBufferInfo(conout.raw(), &mut info) } == 0 {
        return None;
    }
    let [x, y] = info.dwCursorPosition;
    let row = u16::try_from(y.checked_sub(info.srWindow.Top)?).ok()?;
    let col = u16::try_from(x.checked_sub(info.srWindow.Left)?).ok()?;
    Some((row, col))
}

/// Whether the console's input is in line mode (`ENABLE_LINE_INPUT`): the shell at
/// its prompt reads with a cooked `ReadConsoleW`. `None` when the mode cannot be
/// read. The mode is read, never set: it belongs to the program the user runs.
fn shell_reads_in_line_mode() -> Option<bool> {
    let conin = open_console("CONIN$");
    if conin.raw() == ffi::INVALID_HANDLE_VALUE {
        return None;
    }
    let mut mode = 0u32;
    // SAFETY: a live console input handle and an out-param.
    if unsafe { ffi::GetConsoleMode(conin.raw(), &mut mode) } == 0 {
        return None;
    }
    Some(mode & ffi::ENABLE_LINE_INPUT != 0)
}

/// The client's body: once its console's window is `cols`x`rows` (or
/// [`SIZE_WAIT`] has passed), write one `MENU_EVENT`. 0 when it went in.
fn write_guard(cols: i16, rows: i16) -> u8 {
    let (conin, conout) = (open_console("CONIN$"), open_console("CONOUT$"));
    if conin.raw() == ffi::INVALID_HANDLE_VALUE {
        return 2;
    }
    let deadline = Instant::now() + SIZE_WAIT;
    loop {
        let mut info = ffi::CONSOLE_SCREEN_BUFFER_INFO::default();
        // SAFETY: a live console output handle (or an invalid one, which fails) and an out-param.
        let read = unsafe { ffi::GetConsoleScreenBufferInfo(conout.raw(), &mut info) } != 0;
        let window = info.srWindow;
        let size = (
            window.Right - window.Left + 1,
            window.Bottom - window.Top + 1,
        );
        if !read || size == (cols, rows) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let record = ffi::INPUT_RECORD {
        EventType: ffi::MENU_EVENT,
        Event: [0; 4],
    };
    let mut written = 0u32;
    // SAFETY: a live console input handle, one initialized record, an out-param.
    let ok = unsafe { ffi::WriteConsoleInputW(conin.raw(), &record, 1, &mut written) } != 0;
    if ok && written == 1 { 0 } else { 3 }
}

/// The image a test named for the clients ([`set_console_client`]).
#[cfg(any(test, debug_assertions))]
static CLIENT_OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Run this process's console clients from `exe` instead of the `aterm.exe`
/// beside it, or from that again for `None`. This crate's Windows tests name
/// its dev-only `aterm-pty-console-client`, which serves the same clients: a
/// test binary has no `aterm.exe` beside it. Development builds only.
#[cfg(any(test, debug_assertions))]
pub fn set_console_client(exe: Option<PathBuf>) {
    *CLIENT_OVERRIDE.lock_or_recover() = exe;
}

/// `aterm.exe` beside the running executable, looked for once: the console
/// image the front door ships next to the windowed one, and the running image
/// itself for a console-image process. `None` for an embedder without one (this
/// crate's own tests, unless one names a stand-in: [`set_console_client`]),
/// which then gets no client.
fn client_exe() -> Option<PathBuf> {
    #[cfg(any(test, debug_assertions))]
    if let Some(exe) = CLIENT_OVERRIDE.lock_or_recover().clone() {
        return Some(exe);
    }
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| {
        let exe = std::env::current_exe().ok()?.parent()?.join("aterm.exe");
        exe.is_file().then_some(exe)
    })
    .clone()
}

/// Whether this process can start the guard's client, so an MSYS2 program's
/// input survives a resize under OpenConsole (`HostState::host_for_spawn`).
pub(crate) fn available() -> bool {
    client_exe().is_some()
}

/// A client's command line: the quoted image, then ` <args>`.
fn client_command_line(exe: &Path, args: &str) -> Vec<u16> {
    let mut line: Vec<u16> = vec![u16::from(b'"')];
    line.extend(exe.as_os_str().encode_wide());
    line.push(u16::from(b'"'));
    line.push(u16::from(b' '));
    line.extend(args.encode_utf16());
    line.push(0);
    line
}

/// The guard client's command line: the quoted image, [`FLAG`], the size.
fn command_line(exe: &Path, cols: i16, rows: i16) -> Vec<u16> {
    client_command_line(exe, &format!("{FLAG} {cols} {rows}"))
}

/// The leave client's command line (`"<aterm.exe>" --conpty-leave-alt-screen`, and
/// with a `carry` (the cursor it was composed for, its VT) ` <row> <col> <hex>`),
/// `None` when this process has no client image ([`client_exe`]). A carry longer
/// than [`CARRY_HEX_MAX`] digits is left out.
pub(super) fn leave_alt_screen_command(carry: Option<((u16, u16), &str)>) -> Option<Vec<u16>> {
    client_exe().map(|exe| client_command_line(&exe, &leave_args(carry)))
}

/// [`leave_alt_screen_command`]'s arguments.
fn leave_args(carry: Option<((u16, u16), &str)>) -> String {
    let hex = carry
        .map(|(cursor, text)| (cursor, hex_encode(text.as_bytes())))
        .filter(|(_, hex)| !hex.is_empty() && hex.len() <= CARRY_HEX_MAX);
    match hex {
        Some(((row, col), hex)) => format!("{LEAVE_FLAG} {row} {col} {hex}"),
        None => LEAVE_FLAG.to_string(),
    }
}

/// The guard for `s` after a resize to `rows`x`cols`: when a program of the
/// MSYS2/Cygwin runtime is in the session's job, start the client into its
/// pseudoconsole and wait for it ([`CLIENT_WAIT_MS`] at most). Runs in the
/// input writes' turn and outside every lock a reply takes
/// (`ResizeGuard::settle`), so no input write overtakes the record and the
/// terminal's answer the host may be waiting for goes past it. Every failure
/// leaves the input as it would be with no guard.
pub(super) fn guard(s: &WinSession, rows: i16, cols: i16) {
    if !runtime_in_tab(s) {
        return;
    }
    let Some(exe) = client_exe() else {
        return;
    };
    let mut line = command_line(&exe, cols, rows);
    let client = {
        // The order every other path takes (signal, then reference), held across
        // the client's CreateProcessW as `resync_alt_screen` holds it.
        let signal = s.signal.lock_or_recover();
        let reference = s.reference.lock_or_recover();
        if *signal == 0 || *reference == 0 {
            return;
        }
        let mut slots = [*signal, *reference, s.conhost];
        match openconsole::start_client(&mut slots, &mut line) {
            Ok(client) => client,
            Err(_) => return,
        }
    };
    // SAFETY: the client's process handle, owned by `client`.
    unsafe { ffi::WaitForSingleObject(client.raw(), CLIENT_WAIT_MS) };
}

/// Whether a process of the MSYS2 or Cygwin runtime runs in session `s`'s tab
/// now: in its job, or below its first process in the process tree (an adopted
/// session has no job: its client process and the processes below it are
/// asked). The guard's client and the inbox host's astral records are for
/// those programs alone.
///
/// WHY THE TREE TOO: a child can run outside the tab's job. pwsh from the
/// Microsoft Store (the `WindowsApps` alias, which a default pwsh tab runs) is
/// a packaged app, and the programs it starts are not in the job its tab's
/// spawn put it in: measured 2026-10-17, a Git Bash started at that pwsh's
/// prompt saw a 64-process job of the desktop session as its own, where under
/// cmd.exe it saw the tab's 5. Asked of the job alone, the probe never found
/// it, and Git Bash nested in that pwsh kept 16 to 19 of 24 lines whole after a
/// resize under OpenConsole (24 of 24 nested in cmd.exe).
pub(super) fn runtime_in_tab(s: &WinSession) -> bool {
    let job = if s.job != 0 {
        pids_in(s.job)
    } else {
        Vec::new()
    };
    tab_runs_a_posix_runtime(&job, s.pid, process_tree, created_at, runs_a_posix_runtime)
}

/// [`runtime_in_tab`] over injected lookups: a process in `job`, the tab's
/// first process `root` (0 for none), or one below it in `tree()` ((pid,
/// parent) pairs, read only when neither of the others matched; `born` gives
/// a process's start, see [`below`]) for which `is_runtime` holds.
fn tab_runs_a_posix_runtime(
    job: &[u32],
    root: u32,
    tree: impl FnOnce() -> Vec<(u32, u32)>,
    born: impl Fn(u32) -> Option<u64>,
    is_runtime: impl Fn(u32) -> bool,
) -> bool {
    if job.iter().any(|&pid| is_runtime(pid)) {
        return true;
    }
    root != 0
        && (is_runtime(root)
            || below(root, &tree(), born, Unread::AsRecorded)
                .into_iter()
                .any(is_runtime))
}

/// What [`below`] does with a pair whose start, or whose parent's, it cannot
/// read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// Takes it as recorded: the runtime probe, which only reads, would rather
    /// find a process it cannot vouch for than miss one.
    AsRecorded,
    /// Leaves it out with everything below it: a set something is done to
    /// (`outside_job`, which ends it) takes only what it vouched for.
    LeftOut,
}

/// The processes below `root` in `tree` ((pid, parent) pairs from one
/// snapshot), each once. Toolhelp keeps the pid of an orphan's dead parent,
/// and Windows reuses pids, so a pair can name a parent that is not the
/// process now holding that pid: an MSYS daemon (`ssh-agent`, `gpg-agent`)
/// left by an earlier Git Bash would join any tab whose shell got that bash's
/// pid, and send every key after a resize through the guard's client. A
/// process that started before the one it names as its parent (by `born`) is
/// not its child, so it is left out with everything below it; when either
/// start is unreadable (`born` gives `None`) the pair goes as `unread` says.
/// `seen` ends a cycle.
pub(super) fn below(
    root: u32,
    tree: &[(u32, u32)],
    born: impl Fn(u32) -> Option<u64>,
    unread: Unread,
) -> Vec<u32> {
    let mut seen = std::collections::HashSet::from([root]);
    let mut out = Vec::new();
    let mut next = vec![(root, born(root))];
    while let Some((parent, parent_born)) = next.pop() {
        for &(pid, _) in tree
            .iter()
            .filter(|&&(pid, ppid)| ppid == parent && pid != 0)
        {
            if seen.contains(&pid) {
                continue;
            }
            let child_born = born(pid);
            let child_of_parent = match (parent_born, child_born) {
                (Some(parent), Some(child)) => child >= parent,
                _ => unread == Unread::AsRecorded,
            };
            if child_of_parent && seen.insert(pid) {
                out.push(pid);
                next.push((pid, child_born));
            }
        }
    }
    out
}

/// When process `pid` started (its creation time, in 100 ns ticks since
/// 1601), or `None` when it cannot be opened: gone, or another user's.
fn created_at(pid: u32) -> Option<u64> {
    // SAFETY: plain OpenProcess; the handle is owned below.
    let process =
        Handle::new(unsafe { ffi::OpenProcess(ffi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.raw() == 0 {
        return None;
    }
    started(process.raw())
}

/// When the process behind `process` (a handle with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`) started, in 100 ns ticks since 1601;
/// `None` when it cannot be read. The handle names one process for as long as
/// it is open, which a pid does not.
pub(super) fn started(process: HANDLE) -> Option<u64> {
    let mut times = [ffi::FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: a process handle the caller holds and four FILETIME out-params.
    let ok = unsafe { ffi::GetProcessTimes(process, created, exited, kernel, user) };
    (ok != 0)
        .then(|| (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

/// Every process now, as (pid, parent) pairs from one Toolhelp snapshot
/// (empty on any failure). Read only.
pub(super) fn process_tree() -> Vec<(u32, u32)> {
    // SAFETY: documented snapshot form; the pid argument is unused for
    // TH32CS_SNAPPROCESS (always a whole-system process list).
    let snap = Handle::new(unsafe { ffi::CreateToolhelp32Snapshot(ffi::TH32CS_SNAPPROCESS, 0) });
    if snap.raw() == 0 || snap.raw() == ffi::INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: POD out-struct; setting dwSize first is the documented contract.
    let mut e: ffi::PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<ffi::PROCESSENTRY32W>() as u32;
    // SAFETY: a live snapshot handle and a sized entry (here and in the loop).
    let mut ok = unsafe { ffi::Process32FirstW(snap.raw(), &mut e) };
    while ok != 0 {
        out.push((e.th32ProcessID, e.th32ParentProcessID));
        // SAFETY: the same live snapshot and entry as above.
        ok = unsafe { ffi::Process32NextW(snap.raw(), &mut e) };
    }
    out
}

/// Whether process `pid` runs on the MSYS2 or Cygwin runtime
/// (`policy::runs_a_posix_runtime`, by its image path).
fn runs_a_posix_runtime(pid: u32) -> bool {
    image_of(pid).is_some_and(|image| policy::runs_a_posix_runtime(&image, Path::is_file))
}

/// The ids of the processes in `job` (the first [`ffi::JOB_PROCESS_IDS`]).
fn pids_in(job: HANDLE) -> Vec<u32> {
    // SAFETY: a zeroed id list is a valid out-buffer.
    let mut list: ffi::JOBOBJECT_BASIC_PROCESS_ID_LIST = unsafe { std::mem::zeroed() };
    // SAFETY: the session's live job handle and a buffer of the stated size. A job
    // with more processes than fit fails with ERROR_MORE_DATA and still fills it.
    unsafe {
        ffi::QueryInformationJobObject(
            job,
            ffi::JOB_OBJECT_BASIC_PROCESS_ID_LIST_CLASS,
            std::ptr::addr_of_mut!(list).cast::<c_void>(),
            std::mem::size_of::<ffi::JOBOBJECT_BASIC_PROCESS_ID_LIST>() as u32,
            std::ptr::null_mut(),
        )
    };
    let n = (list.NumberOfProcessIdsInList as usize).min(ffi::JOB_PROCESS_IDS);
    list.ProcessIdList[..n]
        .iter()
        .map(|&pid| pid as u32)
        .collect()
}

/// The image path of process `pid`, read and nothing else.
fn image_of(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain OpenProcess; the handle is owned below.
    let process =
        Handle::new(unsafe { ffi::OpenProcess(ffi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.raw() == 0 {
        return None;
    }
    image_of_handle(process.raw())
}

/// The image path of the process behind `process` (a handle with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`).
pub(super) fn image_of_handle(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a process handle the caller holds, a buffer of `len` characters,
    // an in/out length.
    let ok = unsafe { ffi::QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    // Only a flag in first place makes a client, the guard's only with a whole
    // positive size it can wait for and the leave's only alone; every other
    // command line is the front door's own.
    #[test]
    fn only_a_flag_and_its_arguments_make_a_client() {
        assert_eq!(client_args(&os(&[])), None);
        assert_eq!(client_args(&os(&["--window"])), None);
        assert_eq!(client_args(&os(&["ctl", FLAG])), None);
        assert_eq!(client_args(&os(&["ctl", LEAVE_FLAG])), None);
        assert_eq!(
            client_args(&os(&[FLAG, "120", "30"])),
            Some(Client::Guard {
                cols: 120,
                rows: 30
            })
        );
        assert_eq!(
            client_args(&os(&[LEAVE_FLAG])),
            Some(Client::LeaveAltScreen(None))
        );
        assert_eq!(
            client_args(&os(&[LEAVE_FLAG, "4", "54", "50531b5b4b"])),
            Some(Client::LeaveAltScreen(Some(Carry {
                row: 4,
                col: 54,
                text: "PS\x1b[K".to_string()
            })))
        );
        for bad in [
            &[FLAG][..],
            &[FLAG, "120"],
            &[FLAG, "0", "30"],
            &[FLAG, "x", "30"],
            &[FLAG, "1", "2", "3"],
            &[LEAVE_FLAG, "now"],
            &[LEAVE_FLAG, "4", "54"],
            &[LEAVE_FLAG, "4", "54", ""],
            &[LEAVE_FLAG, "4", "54", "5"],
            &[LEAVE_FLAG, "4", "54", "zz"],
            &[LEAVE_FLAG, "4", "54", "ff"],
            &[LEAVE_FLAG, "-1", "54", "50"],
            &[LEAVE_FLAG, "4", "54", "50", "more"],
        ] {
            assert_eq!(client_args(&os(bad)), Some(Client::Malformed), "{bad:?}");
        }
    }

    // A carry's VT survives the command line: its arguments read back to the cursor
    // and text they were made from, and one past the cap (or empty) goes as the
    // plain leave.
    #[test]
    fn a_carry_round_trips_through_the_leave_arguments() {
        let text = "\x1b[0m\x1b[r\x1b[30;1H\n\n\x1b[1;1H\x1b[J\x1b[1;1H\x1b[38;2;1;2;3mPS ✳ C:\\> \"q\"\x1b[0m";
        let args = leave_args(Some(((7, 12), text)));
        let argv: Vec<OsString> = args.split(' ').map(OsString::from).collect();
        assert_eq!(
            client_args(&argv),
            Some(Client::LeaveAltScreen(Some(Carry {
                row: 7,
                col: 12,
                text: text.to_string()
            })))
        );
        assert_eq!(leave_args(None), LEAVE_FLAG);
        assert_eq!(leave_args(Some(((0, 0), ""))), LEAVE_FLAG);
        let long = "x".repeat(CARRY_HEX_MAX / 2 + 1);
        assert_eq!(leave_args(Some(((0, 0), &long))), LEAVE_FLAG);
        let longest = "x".repeat(CARRY_HEX_MAX / 2);
        assert_ne!(leave_args(Some(((0, 0), &longest))), LEAVE_FLAG);
    }

    // The carry goes out only where the host's cursor is the one it was made for and
    // the shell reads raw; a line-mode reader (cmd.exe's prompt), a moved cursor or
    // anything unread gets the plain leave.
    #[test]
    fn a_carry_applies_only_to_a_raw_reader_at_its_own_cursor() {
        let carry = Carry {
            row: 0,
            col: 54,
            text: "PS> ".to_string(),
        };
        assert!(carry_applies(&carry, Some((0, 54)), Some(false)));
        assert!(!carry_applies(&carry, Some((0, 54)), Some(true)), "cmd.exe");
        assert!(!carry_applies(&carry, Some((0, 55)), Some(false)), "moved");
        assert!(!carry_applies(&carry, Some((1, 54)), Some(false)), "moved");
        assert!(!carry_applies(&carry, None, Some(false)));
        assert!(!carry_applies(&carry, Some((0, 54)), None));
    }

    #[test]
    fn the_client_command_lines_quote_the_image() {
        let exe = Path::new(r"C:\Program Files\aterm\aterm.exe");
        let text = |line: Vec<u16>| {
            assert_eq!(line.last(), Some(&0), "NUL-terminated");
            String::from_utf16_lossy(&line[..line.len() - 1])
        };
        assert_eq!(
            text(command_line(exe, 100, 28)),
            format!("\"C:\\Program Files\\aterm\\aterm.exe\" {FLAG} 100 28")
        );
        assert_eq!(
            text(client_command_line(exe, LEAVE_FLAG)),
            format!("\"C:\\Program Files\\aterm\\aterm.exe\" {LEAVE_FLAG}")
        );
        assert_eq!(
            text(client_command_line(exe, &leave_args(Some(((4, 54), "PS"))))),
            format!("\"C:\\Program Files\\aterm\\aterm.exe\" {LEAVE_FLAG} 4 54 5053")
        );
        assert_eq!(
            client_args(&os(&[FLAG, "100", "28"])),
            Some(Client::Guard {
                cols: 100,
                rows: 28
            })
        );
    }

    // The leave ends what a dead app left on and then its screen, with the
    // switch last: the terminal leaves on it, after the resets. The reports no
    // shell asks for go too; the shell's own bracketed paste is left alone.
    #[test]
    fn the_leave_resets_the_app_modes_and_ends_with_the_switch() {
        assert!(LEAVE_SEQUENCE.ends_with("\x1b[?1049l"));
        for reset in [
            "\x1b[?1l",
            "\x1b>",
            "\x1b[?1000l",
            "\x1b[?1002l",
            "\x1b[?1003l",
            "\x1b[?1006l",
            "\x1b[?2048l",
            "\x1b[?2031l",
        ] {
            assert!(LEAVE_SEQUENCE.contains(reset), "{reset:?}");
        }
        assert_eq!(LEAVE_SEQUENCE.matches("\x1b[?1049").count(), 1);
        assert!(!LEAVE_SEQUENCE.contains("\x1b[?2004"), "the shell's paste");
    }

    // The record is laid out as kernel32 reads an `INPUT_RECORD`: 20 bytes,
    // the event union at offset 4 (a `MENU_EVENT_RECORD`'s `dwCommandId`).
    #[test]
    fn the_record_has_the_input_record_layout() {
        let record = ffi::INPUT_RECORD {
            EventType: ffi::MENU_EVENT,
            Event: [0; 4],
        };
        assert_eq!(std::mem::size_of::<ffi::INPUT_RECORD>(), 20);
        let base = std::ptr::addr_of!(record) as usize;
        assert_eq!(std::ptr::addr_of!(record.Event) as usize - base, 4);
        assert_eq!(ffi::MENU_EVENT, 8);
    }

    // The probe reads a real job: a fresh one holds no process at all, so no
    // runtime runs in it.
    #[test]
    fn an_empty_job_runs_no_posix_runtime() {
        // SAFETY: NULL attributes and name, a fresh anonymous job, owned below.
        let job =
            Handle::new(unsafe { ffi::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) });
        assert_ne!(job.raw(), 0);
        assert!(pids_in(job.raw()).is_empty());
        assert!(!tab_runs_a_posix_runtime(
            &pids_in(job.raw()),
            0,
            || panic!("no root, no tree"),
            |_| None,
            runs_a_posix_runtime
        ));
    }

    // A runtime program the tab's job does not hold is still found below the
    // tab's first process: pwsh from the Store is in the job (pid 10), the Git
    // Bash it started (12, under its launcher 11) is not. Asked of the job
    // alone, as before 2026-10-17, the tab ran none. The tree is read only
    // when the job did not answer.
    #[test]
    fn a_runtime_program_outside_the_job_is_found_below_the_shell() {
        let tree = vec![(4, 0), (10, 4), (11, 10), (12, 11), (13, 12), (20, 4)];
        let runtime = |pid: u32| pid == 12;
        assert!(tab_runs_a_posix_runtime(
            &[10],
            10,
            || tree.clone(),
            |_| None,
            runtime
        ));
        assert!(
            !tab_runs_a_posix_runtime(&[10], 10, Vec::new, |_| None, runtime),
            "the job alone holds no runtime program"
        );
        assert!(tab_runs_a_posix_runtime(
            &[10, 12],
            10,
            || panic!("the job answered"),
            |_| None,
            runtime
        ));
        assert!(
            !tab_runs_a_posix_runtime(&[20], 20, || tree.clone(), |_| None, runtime),
            "another tree"
        );
        // An adopted session (no job) asks its client and what runs below it.
        assert!(tab_runs_a_posix_runtime(
            &[],
            11,
            || tree.clone(),
            |_| None,
            runtime
        ));
        assert!(!tab_runs_a_posix_runtime(
            &[],
            0,
            || panic!("no root"),
            |_| None,
            runtime
        ));
    }

    // The walk takes each process below the root once, never the root, and
    // ends on a cycle a reused parent pid can make.
    #[test]
    fn the_walk_below_a_process_takes_each_once_and_ends_on_a_cycle() {
        let tree = [(10, 4), (11, 10), (12, 11), (14, 10), (20, 4), (0, 0)];
        let mut got = below(10, &tree, |_| None, Unread::AsRecorded);
        got.sort_unstable();
        assert_eq!(got, [11, 12, 14]);
        assert_eq!(
            below(12, &tree, |_| None, Unread::AsRecorded),
            Vec::<u32>::new()
        );
        let cycle = [(5, 6), (6, 5)];
        assert_eq!(below(5, &cycle, |_| None, Unread::AsRecorded), [6]);
        let mut got = below(10, &tree, |_| Some(7), Unread::LeftOut);
        got.sort_unstable();
        assert_eq!(got, [11, 12, 14], "equal starts are one tick, not a reuse");
    }

    // A process that started before the one holding its recorded parent's pid
    // is no child of it: an ssh-agent (30, under its agent child 31) whose Git
    // Bash had pid 10 outlives it, and a new tab's pwsh gets pid 10. The walk
    // leaves the agent and what is below it out, keeps the shell's real child
    // (11), and takes a pair as recorded when a start cannot be read.
    #[test]
    fn the_walk_leaves_out_a_process_older_than_its_recorded_parent() {
        let tree = [(10, 4), (30, 10), (31, 30), (11, 10)];
        let born = |pid| match pid {
            10 => Some(100),
            30 => Some(50),
            31 => Some(60),
            11 => Some(120),
            _ => None,
        };
        assert_eq!(below(10, &tree, born, Unread::AsRecorded), [11]);
        assert_eq!(below(10, &tree, born, Unread::LeftOut), [11]);
        let runtime = |pid: u32| pid == 30;
        assert!(!tab_runs_a_posix_runtime(
            &[10],
            10,
            || tree.to_vec(),
            born,
            runtime
        ));
        let unreadable = |pid| (pid != 30).then_some(100);
        let mut got = below(10, &tree, unreadable, Unread::AsRecorded);
        got.sort_unstable();
        assert_eq!(got, [11, 30, 31]);
    }

    // A set something is done to takes only what the walk vouched for: a
    // process whose start cannot be read is left out with everything below
    // it, and an unreadable root takes nothing, where the read-only probe
    // takes them as recorded.
    #[test]
    fn the_strict_walk_leaves_out_what_it_cannot_vouch_for() {
        let tree = [(10, 4), (30, 10), (31, 30), (11, 10), (12, 11)];
        let unreadable = |pid| (pid != 30).then_some(100);
        let mut got = below(10, &tree, unreadable, Unread::LeftOut);
        got.sort_unstable();
        assert_eq!(got, [11, 12]);
        let no_root = |pid| (pid != 10).then_some(100);
        assert_eq!(
            below(10, &tree, no_root, Unread::LeftOut),
            Vec::<u32>::new()
        );
        let mut got = below(10, &tree, no_root, Unread::AsRecorded);
        got.sort_unstable();
        assert_eq!(got, [11, 12, 30, 31]);
    }

    // The real start times order a real parent before this process.
    #[test]
    fn this_process_started_after_its_parent() {
        // SAFETY: plain FFI.
        let me = unsafe { ffi::GetCurrentProcessId() };
        let mine = created_at(me).expect("this process's start reads");
        let parent = process_tree()
            .into_iter()
            .find_map(|(pid, ppid)| (pid == me).then_some(ppid))
            .expect("this process is in the snapshot");
        if let Some(theirs) = created_at(parent) {
            assert!(theirs <= mine, "parent {theirs} after child {mine}");
        }
    }

    // The real snapshot holds this process under its parent.
    #[test]
    fn the_process_tree_holds_this_process() {
        // SAFETY: plain FFI.
        let me = unsafe { ffi::GetCurrentProcessId() };
        assert!(process_tree().iter().any(|&(pid, _)| pid == me));
    }

    // The image path of this very test process reads back.
    #[test]
    fn a_process_image_path_reads_back() {
        // SAFETY: plain FFI.
        let me = unsafe { ffi::GetCurrentProcessId() };
        let image = image_of(me).expect("our own image path");
        assert_eq!(
            image.file_name(),
            std::env::current_exe().unwrap().file_name()
        );
    }
}
