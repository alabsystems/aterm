// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows console driver for the `aterm` passthrough binary — the ConPTY
//! twin of `driver_unix.rs` behind the same four-function surface.
//!
//! Raw mode is the VT console-mode pair: `ENABLE_VIRTUAL_TERMINAL_INPUT` on
//! stdin (the console then synthesizes full VT escape sequences as character
//! streams, so arrows/F-keys arrive as bytes — the same transparency as the
//! unix byte pipe) and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout.
//! Keystrokes arrive as `ReadConsoleInputW` records on an input thread, and
//! `WINDOW_BUFFER_SIZE_EVENT` is the SIGWINCH analogue — it rides the same
//! single input source the way SIGWINCH rides the unix poll loop: the ConPTY
//! is resized promptly (it must repaint), and the engine applies the new size
//! on the output loop's next wake (the unix drain-flag-then-apply shape).
//! Direct `unsafe extern "system"` kernel32 FFI only — the approved std-only
//! pattern; no ConPTY calls live here (those are aterm-pty's seam).

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use aterm_core::terminal::Terminal;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> isize;
    fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    fn GetConsoleScreenBufferInfo(handle: isize, info: *mut ConsoleScreenBufferInfo) -> i32;
    fn ReadConsoleInputW(handle: isize, buf: *mut InputRecord, len: u32, read: *mut u32) -> i32;
    fn GetFileType(handle: isize) -> u32;
    fn WriteFile(
        handle: isize,
        data: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
    fn GetConsoleOutputCP() -> u32;
    fn SetConsoleOutputCP(cp: u32) -> i32;
}

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const INVALID_HANDLE_VALUE: isize = -1;
const FILE_TYPE_CHAR: u32 = 0x0002;

const KEY_EVENT: u16 = 0x0001;
const WINDOW_BUFFER_SIZE_EVENT: u16 = 0x0004;

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_WINDOW_INPUT: u32 = 0x0008;
const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;

/// UTF-8 (`chcp 65001`): ConPTY output is UTF-8 and `WriteFile` to a console
/// decodes bytes in the OUTPUT codepage, so the session pins it (restored on
/// drop by [`RawGuard`]).
const UTF8_CODEPAGE: u32 = 65001;

// Layout-only fields (present so the C struct sizes/offsets are right, never
// read on our side) carry a leading underscore.
#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    _x: i16,
    _y: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
struct ConsoleScreenBufferInfo {
    _size: Coord,
    _cursor_position: Coord,
    _attributes: u16,
    window: SmallRect,
    _maximum_window_size: Coord,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEventRecord {
    key_down: i32,
    repeat_count: u16,
    _virtual_key_code: u16,
    _virtual_scan_code: u16,
    unicode_char: u16,
    _control_key_state: u32,
}

/// The `INPUT_RECORD.Event` C union. Only the KEY_EVENT arm is read; the
/// `pad` arm pins the union to the C size (16 bytes — MOUSE_EVENT_RECORD,
/// the largest member, ties KEY_EVENT_RECORD at 16).
#[repr(C)]
#[derive(Clone, Copy)]
union EventUnion {
    key: KeyEventRecord,
    _pad: [u32; 4],
}

/// `INPUT_RECORD`: a 2-byte tag + (after alignment padding) the event union.
#[repr(C)]
struct InputRecord {
    event_type: u16,
    event: EventUnion,
}

/// Set by the input thread on `WINDOW_BUFFER_SIZE_EVENT`; drained in the
/// output loop before each blocking read (the unix GOT_WINCH shape). The
/// dimensions ride alongside so the loop never re-queries the console.
static GOT_WINCH: AtomicBool = AtomicBool::new(false);
static WINCH_ROWS: AtomicU16 = AtomicU16::new(0);
static WINCH_COLS: AtomicU16 = AtomicU16::new(0);

/// Ask the console for its window size; fall back to 24x80 (same as unix).
pub(crate) fn host_winsize() -> (u16, u16) {
    // SAFETY: out-param query on the process stdout handle; the zeroed struct
    // is a plain POD out-buffer.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: ConsoleScreenBufferInfo = std::mem::zeroed();
        if h != INVALID_HANDLE_VALUE && GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let rows = i32::from(info.window.bottom) - i32::from(info.window.top) + 1;
            let cols = i32::from(info.window.right) - i32::from(info.window.left) + 1;
            if rows > 0 && cols > 0 {
                return (rows as u16, cols as u16);
            }
        }
    }
    (24, 80)
}

/// Whether stdout is a console (`doctor`'s tty check): `GetConsoleMode`
/// succeeds only on a real console handle — the classic isatty analogue
/// (redirected pipes/files fail it).
pub(crate) fn stdout_is_tty() -> bool {
    // SAFETY: mode query on the process stdout handle with a valid out-param.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        h != INVALID_HANDLE_VALUE && GetConsoleMode(h, &mut mode) != 0
    }
}

/// Whether `path` names a runnable shell. Windows has no `access(X_OK)`;
/// execute permission is an ACL/PATHEXT question, so this is the same honest
/// downgrade aterm-dev uses: the file must exist.
pub(crate) fn shell_is_executable(path: &str) -> bool {
    std::path::Path::new(path).is_file()
}

/// RAII console raw-mode guard: swaps stdin/stdout into the VT passthrough
/// modes (and the output codepage to UTF-8) and restores the originals on
/// drop, so a panic or early return never leaves the host console raw —
/// the analogue of the unix `set_raw`/`restore` termios pair.
struct RawGuard {
    stdin: isize,
    stdout: isize,
    stdin_orig: Option<u32>,
    stdout_orig: Option<u32>,
    output_cp_orig: u32,
}

impl RawGuard {
    fn install() -> Self {
        // SAFETY: mode queries/sets on the process std handles; every call is
        // a plain in/out-param kernel32 console API.
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE);
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0u32;
            let stdin_orig = (stdin != INVALID_HANDLE_VALUE
                && GetConsoleMode(stdin, &mut mode) != 0)
                .then_some(mode);
            if let Some(orig) = stdin_orig {
                // Raw keys + VT input synthesis + resize events; no line
                // buffering, no echo, no ^C cooking (0x03 flows to the shell,
                // like cfmakeraw).
                let raw = (orig
                    | ENABLE_VIRTUAL_TERMINAL_INPUT
                    | ENABLE_WINDOW_INPUT
                    | ENABLE_EXTENDED_FLAGS)
                    & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
                SetConsoleMode(stdin, raw);
            }
            let mut mode = 0u32;
            let stdout_orig = (stdout != INVALID_HANDLE_VALUE
                && GetConsoleMode(stdout, &mut mode) != 0)
                .then_some(mode);
            if let Some(orig) = stdout_orig {
                SetConsoleMode(
                    stdout,
                    orig | ENABLE_PROCESSED_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                        | DISABLE_NEWLINE_AUTO_RETURN,
                );
            }
            let output_cp_orig = GetConsoleOutputCP();
            SetConsoleOutputCP(UTF8_CODEPAGE);
            Self {
                stdin,
                stdout,
                stdin_orig,
                stdout_orig,
                output_cp_orig,
            }
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: restores the modes captured at install time on the same
        // handles; best-effort (a vanished console just fails the calls).
        unsafe {
            if let Some(m) = self.stdin_orig {
                SetConsoleMode(self.stdin, m);
            }
            if let Some(m) = self.stdout_orig {
                SetConsoleMode(self.stdout, m);
            }
            SetConsoleOutputCP(self.output_cp_orig);
        }
    }
}

/// Write `data` to the stdout HANDLE via `WriteFile`, looping like the unix
/// `write_all`. Raw bytes on purpose: std's console writer requires each call
/// to be whole valid UTF-8, which ConPTY read-chunk boundaries cannot
/// guarantee; with the output codepage pinned to UTF-8 the console decodes
/// split sequences correctly across calls.
fn stdout_write_all(handle: isize, mut data: &[u8]) {
    while !data.is_empty() {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut written = 0u32;
        // SAFETY: `data` is live for the call and `written` is a valid
        // out-param; a zero return is an error (loop exits).
        let ok = unsafe {
            WriteFile(
                handle,
                data.as_ptr(),
                len,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            break;
        }
        data = &data[written as usize..];
    }
}

/// Append one UTF-16 code unit to `out` as UTF-8, pairing surrogates across
/// calls: `pending` holds a high surrogate whose low half has not arrived yet
/// (a pair CAN straddle two `ReadConsoleInputW` batches). Unpaired surrogates
/// become U+FFFD rather than corrupting the byte stream.
fn push_utf16_unit(unit: u16, pending: &mut Option<u16>, out: &mut Vec<u8>) {
    fn push_char(c: char, out: &mut Vec<u8>) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    if let Some(high) = pending.take() {
        if (0xDC00..=0xDFFF).contains(&unit) {
            let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
            push_char(
                char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER),
                out,
            );
            return;
        }
        push_char(char::REPLACEMENT_CHARACTER, out);
        // fall through: `unit` itself still needs handling below.
    }
    match unit {
        0xD800..=0xDBFF => *pending = Some(unit),
        0xDC00..=0xDFFF => push_char(char::REPLACEMENT_CHARACTER, out),
        u => push_char(
            char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
            out,
        ),
    }
}

/// Console-input pump (input thread): KEY_EVENT characters → UTF-8 → the PTY
/// input; WINDOW_BUFFER_SIZE_EVENT → prompt ConPTY resize + the drained
/// resize flag for the output loop's `engine.resize`. Returns when the
/// console goes away (the shell then runs to its own exit).
fn pump_console_input(stdin: isize, master: i32) {
    // SAFETY: zeroed PODs — every field of every record arm is plain data.
    let mut records: [InputRecord; 64] = unsafe { std::mem::zeroed() };
    let mut pending: Option<u16> = None;
    loop {
        let mut n = 0u32;
        // SAFETY: `records` is a live out-buffer of the declared length;
        // blocking call, `n` is a valid out-param.
        let ok =
            unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), records.len() as u32, &mut n) };
        if ok == 0 || n == 0 {
            return;
        }
        let mut bytes: Vec<u8> = Vec::new();
        for rec in &records[..n as usize] {
            match rec.event_type {
                KEY_EVENT => {
                    // SAFETY: the record tag says this arm is the key event.
                    let key = unsafe { rec.event.key };
                    // Key-down records with a character (with VT input enabled
                    // the console synthesizes escape sequences as char runs;
                    // pure-modifier presses carry NUL and are skipped).
                    if key.key_down != 0 && key.unicode_char != 0 {
                        for _ in 0..key.repeat_count.max(1) {
                            push_utf16_unit(key.unicode_char, &mut pending, &mut bytes);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => {
                    // The SIGWINCH analogue. Resize the ConPTY NOW — it must be
                    // resized promptly so it repaints — reading the live window
                    // rect (the event payload is the BUFFER size, not the
                    // window); the engine applies it on the output loop's next
                    // wake. `aterm_pty::resize` is thread-safe on Windows per
                    // the seam contract.
                    let (rows, cols) = host_winsize();
                    aterm_pty::resize(master, rows, cols);
                    WINCH_ROWS.store(rows, Ordering::Relaxed);
                    WINCH_COLS.store(cols, Ordering::Relaxed);
                    GOT_WINCH.store(true, Ordering::Release);
                }
                _ => {}
            }
        }
        if !bytes.is_empty() {
            aterm_pty::write_all(master, &bytes);
        }
    }
}

/// Piped-stdin pump (input thread): plain blocking byte passthrough — the
/// non-tty/protected_spawn case, the direct analogue of the unix
/// `read(STDIN_FILENO)` arm. EOF just ends the pump; the shell runs to its
/// own exit (the unix `fds[0].fd = -1` behavior).
fn pump_piped_input(master: i32) {
    use std::io::Read as _;
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 8192];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => aterm_pty::write_all(master, &buf[..n]),
        }
    }
}

/// Raw mode, the passthrough loop, resize forwarding, restore, reap: returns
/// the shell's exit code (non-exit → 1, mirroring the unix `!WIFEXITED`).
///
/// Input runs on a detached thread (it parks in `ReadConsoleInputW`/`read`
/// with no portable cancellation; process exit reclaims it — the same way the
/// unix driver's blocked reader ends). Output runs here: blocking ConPTY
/// reads → host stdout passthrough + the engine model.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, engine: &mut Terminal, verbose: bool) -> i32 {
    let master = shell.master;
    let guard = RawGuard::install();
    let stdout = guard.stdout;
    let stdin = guard.stdin;

    // Piped/protected_spawn case: a non-CHAR stdin (pipe/file) cannot be
    // ReadConsoleInputW'd — use plain blocking reads. A CHAR handle that is
    // not a real console (GetConsoleMode failed, e.g. NUL) takes the same
    // fallback.
    // SAFETY: type query on the process stdin handle.
    let stdin_is_console =
        unsafe { GetFileType(stdin) } == FILE_TYPE_CHAR && guard.stdin_orig.is_some();
    std::thread::spawn(move || {
        if stdin_is_console {
            pump_console_input(stdin, master);
        } else {
            pump_piped_input(master);
        }
    });

    let mut bytes_in: u64 = 0;
    let mut buf = [0u8; 8192];
    loop {
        // Apply a pending resize before blocking (the unix drain shape); the
        // ConPTY itself was already resized promptly on the input thread.
        if GOT_WINCH.swap(false, Ordering::Acquire) {
            let (rows, cols) = (
                WINCH_ROWS.load(Ordering::Relaxed),
                WINCH_COLS.load(Ordering::Relaxed),
            );
            if rows > 0 && cols > 0 {
                engine.resize(rows, cols);
            }
        }

        // shell output -> host console (passthrough) AND the engine (model).
        let r = aterm_pty::read(master, &mut buf);
        if r <= 0 {
            break; // shell exited / ConPTY closed
        }
        let out = &buf[..r as usize];
        stdout_write_all(stdout, out);
        engine.process(out);
        bytes_in += out.len() as u64;
    }

    // Restore the console before anything else prints.
    drop(guard);

    // Reap and read the exit code BEFORE close_master: close_master drops the
    // session registry entry, after which exit_code/reap can no longer resolve
    // the pid (same ordering as tests/windows_smoke.rs).
    aterm_pty::reap(shell.pid);
    let code = aterm_pty::exit_code(shell.pid).unwrap_or(1);
    aterm_pty::close_master(master);
    if verbose {
        eprintln!("\r\n[aterm] session ended — engine processed {bytes_in} bytes via the VT core.");
    }
    code
}
