// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resize guard's OpenConsole half: a second console client, started into a tab's pseudoconsole after a resize, that writes ONE `MENU_EVENT` record into the console's input and exits, for the MSYS2 runtime to drop in place of the user's first key ([`super::resize_guard`] has the account of the drop).

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

/// The argument that makes `aterm.exe` the guard's client: `aterm.exe --conpty-input-guard <cols> <rows>`.
pub(crate) const FLAG: &str = "--conpty-input-guard";

/// The argument that makes `aterm.exe` the alternate-screen leave's client: `aterm.exe --conpty-leave-alt-screen`, or with a carry, `aterm.exe --conpty-leave-alt-screen <row> <col> <hex>` ([`Carry`]).
pub(crate) const LEAVE_FLAG: &str = "--conpty-leave-alt-screen";

/// The most hex digits a carry's command line takes (half as many bytes of VT): well
/// inside `CreateProcessW`'s 32767 characters.
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

/// What the leave client writes: the end of a full-screen app that died before it could write its own.
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
const CLIENT_WAIT_MS: u32 = 1000;

/// How long the client waits for the console to take the size its session last
/// sent before it writes anyway (the resize went out over the signal pipe before
/// the client was started, so it is normally already in).
const SIZE_WAIT: Duration = Duration::from_millis(250);

/// The front door's hook: when this process was started as one of a tab's console clients ([`FLAG`], [`LEAVE_FLAG`]), do its one write and return the exit code; otherwise `None`, and the caller carries on.
#[must_use = "return a handled console command's exit status to the caller"]
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
    let num = |a: &OsString| {
        let s = a.to_str()?;
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<i16>().ok())
            .flatten()
            .filter(|n| *n > 0)
    };
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
/// bytes, two lowercase or uppercase digits each).
fn carry_args(row: &OsString, col: &OsString, hex: &OsString) -> Option<Carry> {
    let num = |a: &OsString| {
        let s = a.to_str()?;
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<u16>().ok())
            .flatten()
    };
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
        .map(|pair| {
            pair.iter()
                .all(u8::is_ascii_hexdigit)
                .then(|| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
                .flatten()
        })
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
/// writes lands between the switch and the prompt it brings.
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
/// (`line_mode`, `None` when unread).
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
/// its prompt reads with a cooked `ReadConsoleW`.
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
/// [`SIZE_WAIT`] has passed), write one `MENU_EVENT`.
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
            window.Right.saturating_sub(window.Left).saturating_add(1),
            window.Bottom.saturating_sub(window.Top).saturating_add(1),
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

/// Run this process's console clients from `exe` instead of the `aterm.exe` beside it, or from that again for `None`.
#[cfg(any(test, debug_assertions))]
pub fn set_console_client(exe: Option<PathBuf>) {
    *CLIENT_OVERRIDE.lock_or_recover() = exe;
}

/// `aterm.exe` beside the running executable, looked for once: the console
/// image the front door ships next to the windowed one, and the running image
/// itself for a console-image process.
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

/// Whether this process can start the guard's client, so an MSYS2 program's input survives a resize under OpenConsole (`HostState::host_for_spawn`).
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

/// The leave client's command line (`"<aterm.exe>" --conpty-leave-alt-screen`, and with a `carry` (the cursor it was composed for, its VT) ` <row> <col> <hex>`), `None` when this process has no client image ([`client_exe`]).
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

/// The guard for `s` after a resize to `rows`x`cols`: when a program of the MSYS2/Cygwin runtime is in the session's job, start the client into its pseudoconsole and wait for it ([`CLIENT_WAIT_MS`] at most).
pub(super) fn guard(s: &WinSession, rows: i16, cols: i16) {
    if !runtime_in_tab(s) {
        return;
    }
    let Some(exe) = client_exe() else {
        return;
    };
    let mut line = command_line(&exe, cols, rows);
    let client = {
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

/// Whether a process of the MSYS2 or Cygwin runtime runs in session `s`'s tab now: in its job, or below its first process in the process tree (an adopted session has no job: its client process and the processes below it are asked).
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

/// What [`below`] does with a pair whose start, or whose parent's, it cannot read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// Takes it as recorded: the runtime probe, which only reads, would rather find a process it cannot vouch for than miss one.
    AsRecorded,
    /// Leaves it out with everything below it: a set something is done to (`outside_job`, which ends it) takes only what it vouched for.
    LeftOut,
}

/// The processes below `root` in `tree` ((pid, parent) pairs from one snapshot), each once.
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

/// When the process behind `process` (a handle with at least `PROCESS_QUERY_LIMITED_INFORMATION`) started, in 100 ns ticks since 1601; `None` when it cannot be read.
pub(super) fn started(process: HANDLE) -> Option<u64> {
    let mut times = [ffi::FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: a process handle the caller holds and four FILETIME out-params.
    let ok = unsafe { ffi::GetProcessTimes(process, created, exited, kernel, user) };
    (ok != 0)
        .then(|| (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime))
}

/// Every process now, as (pid, parent) pairs from one Toolhelp snapshot (empty on any failure).
pub(super) fn process_tree() -> Vec<(u32, u32)> {
    // SAFETY: documented snapshot form; the pid argument is unused for TH32CS_SNAPPROCESS (always a whole-system process list).
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
pub(super) fn pids_in(job: HANDLE) -> Vec<u32> {
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
pub(super) fn image_of(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain OpenProcess; the handle is owned below.
    let process =
        Handle::new(unsafe { ffi::OpenProcess(ffi::PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.raw() == 0 {
        return None;
    }
    image_of_handle(process.raw())
}

/// The image path of the process behind `process` (a handle with at least `PROCESS_QUERY_LIMITED_INFORMATION`).
pub(super) fn image_of_handle(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a process handle the caller holds, a buffer of `len` characters, an in/out length.
    let ok = unsafe { ffi::QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) };
    (ok != 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

#[cfg(test)]
#[path = "console_guard_tests.rs"]
mod tests;
