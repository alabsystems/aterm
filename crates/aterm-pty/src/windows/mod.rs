// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows ConPTY backend of the PTY seam.
//!
//! Same public surface as the Unix seam, same `i32`-based signatures. The
//! `master` an aterm caller holds is an OPAQUE KEY (always `>= 0`, never
//! reused) into a process-global [`SESSIONS`] registry; the registry entry owns
//! the real HANDLEs (ConPTY, pipes, process, job, close event). Every op clones
//! the entry's `Arc` under a BRIEF map lock, releases the lock, then does the
//! blocking syscall — so a handle can never be closed/recycled while any thread
//! is mid-`ReadFile`/`WriteFile`, reproducing the Unix `OwnedFd` discipline.
//!
//! EOF is MANUFACTURED (the load-bearing ConPTY gotcha): a blocked
//! `ReadFile(output)` does NOT return when the child exits — conhost holds the
//! pipe's write side until `ClosePseudoConsole`. Each session therefore runs a
//! small WAITER THREAD that waits on the child process (and the hangup event),
//! records the exit code, and closes the pseudoconsole — breaking the pipe so
//! the session reader's `read()` returns `0` and the existing `r <= 0` teardown
//! paths fire untouched. `ClosePseudoConsole` may block until the out pipe is
//! drained; that is safe by construction because the session's reader keeps
//! reading until broken-pipe — any future change that stops the reader before
//! close reintroduces the macOS-style quit-hang this crate already fought.
//!
//! Parser implications: (1) ConPTY re-renders — output arrives as full-region
//! repaints + cursor addressing rather than the child's raw byte stream; the
//! engine is a VT state machine, unaffected. (2) conhost requests
//! win32-input-mode (`CSI ? 9001 h`) at every session start, and the engine
//! HONOURS it (DEC mode 9001 → `KeyboardMode::WIN32_INPUT`): an Enter chord
//! (Shift/Ctrl, with or without Alt, main block or keypad) is written to the
//! input pipe as the win32 key-record pair `CSI 13;28;Uc;Kd;Cs;1 _` conhost
//! asked for — UnicodeChar CR for Shift+Enter and LF whenever Ctrl is held,
//! the chars a physical chord carries under Windows Terminal — because legacy
//! VT has no byte for those chords and conhost read the legacy Shift+Enter LF
//! as Ctrl+Enter (measured 2026-09-22). The record outranks a kitty push the
//! application made through conhost, which drops a CSI u written back. The
//! keys whose legacy bytes conhost holds once its parser has switched (a lone or
//! dangling ESC: Escape, Ctrl+[, Alt+[, Alt+O, Alt+Backspace ...) and Ctrl+H
//! (whose bare BS conhost reads as Ctrl+Backspace, measured 2026-10-16) go out as
//! records too; `KeyboardMode::WIN32_INPUT` lists them. Every other key still
//! flows as plain xterm VT input; conhost accepts the mixed stream. (3) conhost
//! issues DSR/CPR queries; the existing reader-thread `take_response()` reply path
//! answers them with zero changes. (4) A resize makes the MSYS2 runtime (Git
//! Bash) drop the next input record — a bracketed paste then loses its ESC
//! and bash runs `[200~…` — so the first writes after a size change lead with
//! a Shift key-up record no reader acts on, for it to drop instead. That
//! record, like (2)'s, switches conhost's parser to holding a trailing lone
//! ESC, so once one has gone out a raw lone ESC is sent as the Escape key's
//! record pair; see [`resize_guard`]. OpenConsole hands such a reader no key
//! record but text, so there a second console client writes the record
//! ([`console_guard`]). The terminal's answers to (3)'s queries pass the guard
//! ([`as_reply`]): OpenConsole waits for its own cursor query's answer after a
//! resize, and the client cannot connect until it has it, so an input writer
//! runs the client before it takes any lock an answer needs
//! ([`settle_input`]). The reports the terminal sends unasked (a resize's DEC
//! 2048 report) are input and wait for the guard. With no input after a size
//! change, the guard sends its record by itself 50 ms later: a program of the
//! runtime waits for one more record before it can act on the resize.
//! (5) conhost hands that runtime a character beyond U+FFFF typed as text
//! without its high surrogate, so it is sent as records.
//! (6) A full-screen app that dies on the alternate screen leaves OpenConsole on
//! it; a second console client leaves it ([`resync_alt_screen`]).

mod cmdline;
mod console_guard;
mod ffi;
mod openconsole;
mod outside_job;
mod resize_guard;
mod shell;
mod winpath;

// The front door's first call: `aterm.exe` started as one of a tab's console
// clients (the resize guard's record, the alternate-screen leave) does its one
// write and exits (`console_guard`).
pub use console_guard::serve_console_client;
// The image those clients run from, named by this crate's Windows tests, whose
// binaries have no `aterm.exe` beside them.
#[cfg(any(test, debug_assertions))]
pub use console_guard::set_console_client;

// The console host a pseudoconsole runs in (conhost or OpenConsole): the
// window states aterm.toml's `conpty` choice, `aterm doctor` and
// `show-config` report the host a new tab gets, and the frontend keys the
// workarounds that differ between the two hosts on a session's kind.
pub use openconsole::{
    console_host_report, preview_console_host, probe_console_host, set_console_host,
    take_console_host_fallback,
};
// Published so config validation can ask the REAL resolver how a `shell` value
// would resolve, rather than modelling it (see `ShellResolution`), and so
// `aterm doctor` / `show-config` can name the shell a new tab spawns and the
// arm that chose it (see `ShellOrigin`) instead of the CLI's own `$SHELL`.
pub use shell::{ShellOrigin, ShellResolution, classify_shell_name, select_shell_with_origin};

use std::collections::HashMap;
use std::ffi::{OsStr, c_void};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use aterm_types::MutexExt;

use crate::console_host::policy::OpenConsoleExe;
use crate::input_release;
use crate::{ConsoleHostKind, SpawnedShell};
use cmdline::{build_command_line, build_env_block, wide_nul};
use resize_guard::{ResizeGuard, Writer};

/// One live ConPTY session: the real HANDLEs behind an opaque `i32` master key.
/// HANDLEs are stored as `isize` (plain integers, not pointers) so the map is
/// `Send + Sync` without unsafe impls.
struct WinSession {
    /// HPCON; swapped to 0 once `ClosePseudoConsole` ran (idempotence guard).
    /// The mutex also makes UI-thread/control-thread resizes safe against the
    /// waiter's close (resize holds it across `ResizePseudoConsole`).
    ///
    /// **0 for an ADOPTED session** ([`adopt_handoff`]): in the DefTerm inbound
    /// handoff conhost already created the pseudoconsole and kept it, so there
    /// is no HPCON for us to hold, resize or close. The same mutex then guards
    /// [`signal`](Self::signal) instead — see [`resize`] and
    /// [`close_pseudoconsole`], which both branch on which of the two is live.
    /// **0 for an OpenConsole session** too: there is no kernel32 HPCON, the
    /// three handles winconpty keeps are [`signal`](Self::signal),
    /// [`reference`](Self::reference) and [`conhost`](Self::conhost), and it
    /// resizes and closes through the signal pipe exactly like the adopted lane.
    hpc: Mutex<isize>,
    /// The ConPTY SIGNAL PIPE's write end, for ADOPTED and OpenConsole sessions
    /// (0 for an inbox session we spawned, which resizes through its HPCON).
    ///
    /// This is the same handle `ResizePseudoConsole`/`ClosePseudoConsole` use
    /// internally: verified by disassembling `kernelbase!ResizePseudoConsole`
    /// on Windows 11 26200, which loads `hPC->[0]` and `WriteFile`s the 6-byte
    /// packet [`encode_resize_signal`] builds; `ClosePseudoConsole` closes that
    /// same `[0]` handle (plus `[8]`/`[0x10]`) to tear the session down. So an
    /// adopted session reproduces both operations by hand on this one handle.
    ///
    /// **This mutex — not the `hpc` one — is what makes the adopted lane safe.**
    /// [`resize`] holds THIS guard across its `WriteFile`;
    /// [`close_pseudoconsole`] swaps the handle to 0 under THIS guard and calls
    /// `CloseHandle` outside it. So a resize either wins the lock and writes to
    /// a handle that is still open, or loses it and reads the 0 that says the
    /// session is already gone — never a use-after-close.
    ///
    /// The `hpc` guard does NOT cover it: `close_pseudoconsole` takes the hpc
    /// lock in a `let` initializer, so that guard is dropped at the semicolon
    /// before this lock is acquired at all. What rules out a lock-order
    /// inversion is instead that the order is consistently hpc → signal →
    /// reference everywhere it matters ([`resize`] holds `hpc` while taking
    /// `signal`; [`resync_alt_screen`] holds `signal` while taking `reference`;
    /// [`close_pseudoconsole`] releases each before the next; `Drop` takes them
    /// one at a time), and nothing ever takes an earlier one while holding a
    /// later one.
    signal: Mutex<isize>,
    /// An OpenConsole session's `\Reference` handle on its ConDrv server — the
    /// handle a client connects through, and what keeps the host alive until
    /// the signal pipe has closed (winconpty closes it second). 0 for every
    /// other session. Swapped to 0 by [`close_pseudoconsole`] after the signal.
    reference: Mutex<isize>,
    /// Which console host serves this session.
    host: ConsoleHostKind,
    /// Write end of the child-stdin pipe (we `WriteFile` keystrokes here).
    input: isize,
    /// Read end of the child-stdout pipe (we `ReadFile` VT output here).
    output: isize,
    /// The child's process handle (held ⇒ the pid cannot be recycled).
    process: isize,
    /// The session's ConPTY console host (conhost.exe / OpenConsole.exe — the
    /// VT↔INPUT_RECORD middleman). Inbox: opened `PROCESS_SET_INFORMATION` at
    /// spawn via a before/after child-diff around `CreatePseudoConsole`; 0 when
    /// discovery was ambiguous or failed (boost then covers the shell only).
    /// OpenConsole: the process handle `CreateProcessW` returned, which is also
    /// the third slot of its pseudoconsole. Used for the focus-boost QoS lane
    /// ([`set_focus_boost`]) and, for OpenConsole, as that slot when a second
    /// client attaches ([`resync_alt_screen`]) — never waited on, read, or
    /// terminated through this handle.
    conhost: isize,
    /// Per-session Job Object (KILL_ON_JOB_CLOSE): last-handle close sweeps any
    /// orphaned descendant tree — slightly stronger than SIGHUP-to-pgid (a
    /// double-forked daemon survives SIGHUP but not the job); accepted and
    /// documented. Children spawned with CREATE_BREAKAWAY escape it, and so do
    /// the programs a packaged shell (pwsh from the Store) starts; those still
    /// below the shell are ended at the close ([`outside_job`]).
    job: isize,
    /// Whether [`outside_job::end_once`] has run for this session: the first
    /// of a close, the shell's own exit and a quit ends the programs outside
    /// the job, and the rest find it done, or wait the few milliseconds until it
    /// is (held across the sweep, so a quit never exits in the middle of one).
    outside_job_ended: Mutex<bool>,
    /// Manual-reset event; [`hangup`] signals it and the waiter thread closes
    /// the pseudoconsole (the console-close analog of SIGHUP).
    close_evt: isize,
    /// The child's Windows process id (what [`SpawnedShell::pid`] holds).
    pid: u32,
    /// Exit code recorded by the waiter thread (or by [`reap`]).
    exit_code: Mutex<Option<u32>>,
    /// The inert record the first writes after a resize lead with, and the
    /// state that decides when it may be written ([`resize_guard`]).
    resize_guard: ResizeGuard,
    /// Set by [`release_input_writes`]: the session's input is severed, every
    /// input write in flight was cancelled, and every later one writes nothing.
    input_severed: AtomicBool,
}

impl Drop for WinSession {
    fn drop(&mut self) {
        // Close every owned handle. The job close is last-handle by construction
        // (only this struct holds it), so KILL_ON_JOB_CLOSE sweeps any orphans.
        // `signal` is normally already 0 (the waiter's teardown swapped it out),
        // but a session dropped before its waiter ran still owns it — take it
        // through the mutex so the close happens exactly once either way.
        let signal = std::mem::replace(&mut *self.signal.lock_or_recover(), 0);
        let reference = std::mem::replace(&mut *self.reference.lock_or_recover(), 0);
        for h in [
            self.input,
            self.output,
            self.process,
            self.conhost,
            self.job,
            self.close_evt,
            signal,
            reference,
        ] {
            if h != 0 {
                // SAFETY: each handle is owned solely by this session entry and
                // no thread can hold it past the Arc this Drop runs under.
                unsafe { ffi::CloseHandle(h) };
            }
        }
    }
}

/// The process-global session registry: opaque `i32` key → live session.
static SESSIONS: LazyLock<Mutex<HashMap<i32, Arc<WinSession>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Keys start positive (the gui asserts `master >= 0`) and are never reused.
static NEXT_KEY: AtomicI32 = AtomicI32::new(0x4000_0000);

/// Clone the session for `master` out under a brief map lock (`None` for a
/// fabricated/closed key — every caller maps that to the documented
/// error/no-op registry-miss semantics).
fn session(master: i32) -> Option<Arc<WinSession>> {
    SESSIONS.lock_or_recover().get(&master).cloned()
}

/// Find THE session whose child has this pid (linear scan — session counts are
/// tiny). `None` when no session matches **or when more than one does**.
///
/// AMBIGUITY IS REFUSED, deliberately. The pid-keyed public ops ([`hangup`],
/// [`reap`], [`exit_code`]) carry the Unix `killpg`/`waitpid` signatures, so a
/// pid is all they get — but a pid is NOT a guaranteed unique key over this
/// registry:
///
/// * A session we SPAWNED pins its pid: the entry holds an open process handle,
///   so the number cannot be recycled while the entry lives. Those never
///   collide with each other.
/// * An ADOPTED session ([`adopt_handoff`]) takes whatever client conhost hands
///   it, and the SAME client can be handed off more than once. A console
///   program that calls `FreeConsole` and then `AllocConsole` (or is
///   re-`AttachConsole`d) gets a fresh pseudoconsole and a fresh handoff under
///   its ORIGINAL pid, while the previous entry is still registered — that one
///   is removed only when the frontend's EOF path reaches [`close_master`],
///   which is asynchronous. Two live entries, one pid.
///
/// A first-match scan resolved that window to an ARBITRARY entry (`HashMap`
/// iteration order, which Rust randomizes per process), so closing one tab
/// could hang up or reap the OTHER console. For an adopted session that console
/// is somebody else's program — an installer mid-write is the case this whole
/// lane is built to protect — so acting on the wrong one is the worst outcome
/// available. Refusing lands instead on the miss semantics every caller already
/// documents (`hangup`/`reap` no-op, `exit_code` → `None`): doing nothing to a
/// console we cannot uniquely identify beats doing something to the wrong one,
/// and the tab still tears down through its reader EOF / `close_master` path,
/// which is master-keyed and never ambiguous.
///
/// `pid <= 1` is refused for the same reason: an adopted session whose process
/// handle carried no query rights records pid 0 (see [`adopt_handoff`]), and
/// several of those can coexist.
fn session_by_pid(pid: i32) -> Option<Arc<WinSession>> {
    if pid <= 1 {
        return None;
    }
    let map = SESSIONS.lock_or_recover();
    let mut hit: Option<Arc<WinSession>> = None;
    for s in map.values() {
        if s.pid == pid as u32 {
            if hit.is_some() {
                // Two live sessions claim this pid — refuse rather than guess.
                return None;
            }
            hit = Some(Arc::clone(s));
        }
    }
    hit
}

/// Pids of every console-host child (`conhost.exe` / `OpenConsole.exe`) of
/// THIS process, via a Toolhelp walk. ConPTY parents the console host to the
/// CALLING process (aterm itself, not the shell), so a before/after diff
/// around `CreatePseudoConsole` isolates the host a spawn just created.
/// Best-effort: any failure returns what was collected (possibly empty) —
/// the caller degrades to a shell-only boost, never an error.
fn conhost_children() -> Vec<u32> {
    // SAFETY: no-argument pid query.
    let me = unsafe { ffi::GetCurrentProcessId() };
    // SAFETY: documented snapshot form; the pid argument is unused for
    // TH32CS_SNAPPROCESS (always a whole-system process list).
    let snap = unsafe { ffi::CreateToolhelp32Snapshot(ffi::TH32CS_SNAPPROCESS, 0) };
    if snap == 0 || snap == ffi::INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: POD out-struct; setting dwSize pre-call is the documented contract.
    let mut e: ffi::PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<ffi::PROCESSENTRY32W>() as u32;
    // SAFETY: live snapshot handle + sized entry struct (here and in the loop).
    let mut ok = unsafe { ffi::Process32FirstW(snap, &mut e) };
    while ok != 0 {
        if e.th32ParentProcessID == me {
            let n = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..n]);
            // OpenConsole.exe: the sideloaded modern host (adoption lane #2)
            // registers under its own name — match both now so the discovery
            // survives that switch untouched.
            if name.eq_ignore_ascii_case("conhost.exe")
                || name.eq_ignore_ascii_case("openconsole.exe")
            {
                out.push(e.th32ProcessID);
            }
        }
        // SAFETY: same live snapshot + entry struct as above.
        ok = unsafe { ffi::Process32NextW(snap, &mut e) };
    }
    // SAFETY: the snapshot handle is owned solely by this walk.
    unsafe { ffi::CloseHandle(snap) };
    out
}

/// Swap the session's HPCON to 0 under its lock and close it OUTSIDE the lock
/// (`ClosePseudoConsole` can block until the out pipe drains; a resize on the
/// UI thread must never wait behind that). Idempotent.
///
/// ADOPTED sessions ([`adopt_handoff`]) have no HPCON — conhost owns the
/// pseudoconsole — so the equivalent teardown is to close the SIGNAL PIPE:
/// conhost's `PtySignalInputThread` sees the broken pipe and tears its session
/// down, which is precisely what `ClosePseudoConsole` does under the hood
/// (verified: it closes `hPC->[0]`, the signal pipe, first). Closing the pipe
/// is also the only teardown lever we have — we must NOT `TerminateProcess`
/// the client, because the DefTerm client is somebody else's program (an
/// installer mid-write, a `.bat` mid-copy) and killing it on window-close would
/// be strictly worse than the console going away under it.
///
/// OpenConsole sessions take the same branch — winconpty's own
/// `ClosePseudoConsole` is exactly these handle closes — plus the `\Reference`
/// handle after the signal pipe. The 2026-09-29 snapshot recorded (measured
/// 2026-09-28) that the close does not block, the shell exits `0xC000013A`
/// within 2 ms, and the host and the shell are gone within 10 ms, so the
/// reader's EOF follows as it does for conhost.
fn close_pseudoconsole(s: &WinSession) {
    let hpc = std::mem::replace(&mut *s.hpc.lock_or_recover(), 0);
    if hpc != 0 {
        // SAFETY: the swap-to-0 makes this thread the sole closer; conhost
        // flushes pending output and breaks the out pipe (the reader's EOF).
        unsafe { ffi::ClosePseudoConsole(hpc) };
        return;
    }
    let signal = std::mem::replace(&mut *s.signal.lock_or_recover(), 0);
    if signal != 0 {
        // SAFETY: same swap-to-0 sole-closer discipline as the HPCON above.
        unsafe { ffi::CloseHandle(signal) };
    }
    // OpenConsole: the reference goes SECOND, winconpty's order — the broken
    // signal pipe lets the host tear its session down cleanly, and the
    // reference is what kept it running until then. Never waits.
    let reference = std::mem::replace(&mut *s.reference.lock_or_recover(), 0);
    if reference != 0 {
        // SAFETY: same swap-to-0 sole-closer discipline.
        unsafe { ffi::CloseHandle(reference) };
    }
}

/// Grace the waiter gives a child to honor the console-close signal before it
/// force-terminates the job (the SIGHUP→SIGKILL escalation, on the close path).
/// The second wait is NEVER `INFINITE`: a child that SURVIVES the console-close
/// signal (called `FreeConsole`, or otherwise ignores `CTRL_CLOSE_EVENT`) would
/// otherwise park the waiter forever — keeping the session `Arc` alive so its
/// `KILL_ON_JOB_CLOSE` job never closes — and the child plus its whole descendant
/// tree would outlive the master. Bounding the wait + terminating the job makes
/// the job the thing that actually kills a survivor, independent of the frontend
/// reaper (which can lose its race with `close_master`'s registry removal).
const CLOSE_GRACE_MS: u32 = 2000;
/// Bounded wait after `TerminateJobObject` for the kill to take (never `INFINITE`);
/// a terminated job's process dies near-instantly, so this only backstops a hang.
const CLOSE_KILL_REST_MS: u32 = 3000;

/// Per-session waiter thread body (see module docs: EOF is manufactured here).
fn waiter(s: &WinSession) {
    let handles = [s.process, s.close_evt];
    // SAFETY: both handles are owned by the session entry, which the calling
    // thread keeps alive via its Arc for the whole wait.
    let r = unsafe { ffi::WaitForMultipleObjects(2, handles.as_ptr(), 0, ffi::INFINITE) };
    // The programs the tab runs outside its job end with it, looked for while
    // the pseudoconsole is still open: a program sharing the tab's console
    // still links the shell to what it started (`outside_job`). A close
    // request found them already (`hangup`, `close_master`); this is the
    // shell's own exit.
    if r == ffi::WAIT_OBJECT_0 || r == ffi::WAIT_OBJECT_0 + 1 {
        outside_job::end_once(s);
    }
    if r == ffi::WAIT_OBJECT_0 + 1 {
        // Hangup/close requested: close the pseudoconsole. The child console
        // session receives the console-close signal (CTRL_CLOSE_EVENT) — the moral
        // equivalent of SIGHUP-on-controlling-tty — and should exit.
        close_pseudoconsole(s);
        // Bounded wait, NEVER INFINITE (see CLOSE_GRACE_MS): a child that survives
        // the console-close signal is force-killed via its job here, so it can
        // never outlive its master and the waiter always returns (dropping its
        // session Arc so the handles/thread don't leak per closed tab).
        // SAFETY: process/job handles owned by the live session entry.
        let g = unsafe { ffi::WaitForSingleObject(s.process, CLOSE_GRACE_MS) };
        // `job == 0` is an ADOPTED (DefTerm handoff) session: we did not create
        // the client, it is not in a job of ours, and there is deliberately no
        // escalation — see `close_pseudoconsole`. The waiter still returns here
        // (the wait above is bounded), so nothing leaks; the client simply keeps
        // whatever lifetime the console-close signal gave it, exactly as it
        // would have under conhost.
        if g == ffi::WAIT_TIMEOUT && s.job != 0 {
            unsafe { ffi::TerminateJobObject(s.job, 1) };
            unsafe { ffi::WaitForSingleObject(s.process, CLOSE_KILL_REST_MS) };
        }
    }
    // Child exited (on its own or after the close above; a failed wait is
    // treated as exited — never spin). Record the exit code, then close the
    // pseudoconsole if still open: that flushes pending output and BREAKS the
    // out pipe, so the reader's ReadFile returns ERROR_BROKEN_PIPE, read()
    // returns 0, and the existing `r <= 0 => exit` reader paths fire untouched.
    if r == ffi::WAIT_OBJECT_0 || r == ffi::WAIT_OBJECT_0 + 1 {
        let mut code: u32 = 0;
        // SAFETY: valid process handle + out-param.
        if unsafe { ffi::GetExitCodeProcess(s.process, &mut code) } != 0 {
            s.exit_code.lock_or_recover().get_or_insert(code);
        }
    }
    close_pseudoconsole(s);
}

/// RAII for the HPCON during spawn (closed on early error, released into the
/// session on success).
struct PconGuard(isize);

impl PconGuard {
    fn release(mut self) -> isize {
        std::mem::replace(&mut self.0, 0)
    }
}

impl Drop for PconGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: owned, unreleased pseudoconsole (spawn failed before the
            // registry took ownership); no reader exists yet, so no drain hang.
            unsafe { ffi::ClosePseudoConsole(self.0) };
        }
    }
}

/// RAII for the proc-thread attribute list (8-aligned heap block + the
/// mandatory `DeleteProcThreadAttributeList`).
struct AttrList {
    buf: Vec<u64>,
    initialized: bool,
}

impl AttrList {
    /// An initialized list with room for `count` attributes.
    fn new(count: u32) -> io::Result<Self> {
        let mut size: usize = 0;
        // SAFETY: documented size-probe form (NULL list): fails with
        // ERROR_INSUFFICIENT_BUFFER and writes the required size.
        unsafe {
            ffi::InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size)
        };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = AttrList {
            buf: vec![0u64; size.div_ceil(8)], // u64 elements keep it 8-aligned
            initialized: false,
        };
        // SAFETY: `list.buf` is a writable block of >= size bytes.
        if unsafe { ffi::InitializeProcThreadAttributeList(list.ptr(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        list.initialized = true;
        Ok(list)
    }

    fn ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }
}

/// The four ends of a pseudoconsole's two pipes. The HOST reads `in_read` and
/// writes `out_write` (it gets duplicates of both, and ours are closed once it
/// has them); the session keeps `in_write` (keystrokes in) and `out_read` (VT
/// out). Blocking anonymous pipes map 1:1 onto the blocking read()/write
/// contract (no overlapped I/O: the reader thread is dedicated and blocking),
/// with advisory 64 KiB buffers — the system default is 4 KiB, which caps
/// every ReadFile at ~4 KiB, 16x the wakeups/copies of the frontend's 64 KiB
/// read buffer, and 64 KiB matches the Unix kernel PTY buffer.
struct Pipes {
    in_read: ffi::Handle,
    in_write: ffi::Handle,
    out_read: ffi::Handle,
    out_write: ffi::Handle,
}

impl Pipes {
    fn new() -> io::Result<Self> {
        const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
        let pipe = || -> io::Result<(ffi::Handle, ffi::Handle)> {
            let (mut read, mut write): (isize, isize) = (0, 0);
            // SAFETY: valid out-params, NULL security attributes.
            if unsafe {
                ffi::CreatePipe(
                    &mut read,
                    &mut write,
                    std::ptr::null_mut(),
                    PIPE_BUFFER_BYTES,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok((ffi::Handle::new(read), ffi::Handle::new(write)))
        };
        let (in_read, in_write) = pipe()?;
        let (out_read, out_write) = pipe()?;
        Ok(Self {
            in_read,
            in_write,
            out_read,
            out_write,
        })
    }
}

/// A pseudoconsole a spawn created, in one of the two hosts.
enum Pseudoconsole {
    /// kernel32 `CreatePseudoConsole`: the HPCON, and the conhost the
    /// before/after child diff found (0 when it was ambiguous).
    Inbox {
        hpc: PconGuard,
        conhost: ffi::Handle,
    },
    /// An OpenConsole this crate launched, and the file it was launched from.
    OpenConsole {
        host: openconsole::OpenConsoleHost,
        exe: OpenConsoleExe,
    },
}

/// Create the pseudoconsole a new session's shell attaches to — in the host the
/// frontend chose ([`set_console_host`]) when `allow_openconsole`, else in the
/// inbox host — with its pipes. An OpenConsole that cannot be launched is
/// noted for the log and the inbox host serves this tab on FRESH pipes, so a
/// tab always opens (one that launches but never speaks is caught after the
/// shell starts, by the spawn's step (9b)). A GUI-subsystem `program`
/// (`aterm -e explorer .`, a `pythonw`) gets the inbox host, as every tab did
/// before OpenConsole, and nothing is noted: it never connects to its console,
/// so an OpenConsole could never show it works, and the program is no tab's
/// shell to keep.
fn create_pseudoconsole(
    size: ffi::COORD,
    program: &OsStr,
    allow_openconsole: bool,
) -> io::Result<(Pseudoconsole, Pipes)> {
    let wanted = |_: &OpenConsoleExe| {
        allow_openconsole && !openconsole::is_gui_image(std::path::Path::new(program))
    };
    if let Some(exe) = openconsole::host_for_spawn(program).filter(wanted) {
        let pipes = Pipes::new()?;
        match openconsole::launch(&exe, size, &pipes) {
            Ok(host) => return Ok((Pseudoconsole::OpenConsole { host, exe }, pipes)),
            Err(why) => openconsole::note_launch_failure(&exe, &why),
        }
    }
    let pipes = Pipes::new()?;
    // Focus-boost lane: `CreatePseudoConsole` synchronously starts the
    // session's console host as a child of THIS process. Snapshot our
    // console-host children before/after so the ONE new pid is this session's
    // conhost — the second starvable middleman the boost must cover.
    let conhost_before = conhost_children();
    let mut hpc: isize = 0;
    // SAFETY: live pipe handles for the child side; valid out-param for the HPCON.
    let hr = unsafe {
        ffi::CreatePseudoConsole(
            size,
            pipes.in_read.raw(),
            pipes.out_write.raw(),
            0,
            &mut hpc,
        )
    };
    if hr < 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let hpc = PconGuard(hpc);
    let new: Vec<u32> = conhost_children()
        .into_iter()
        .filter(|p| !conhost_before.contains(p))
        .collect();
    let conhost = ffi::Handle::new(match new.as_slice() {
        // SAFETY: opening OUR OWN just-spawned child with the least right
        // the boost needs; NULL (0) on failure is the "no conhost lane"
        // sentinel the guard tolerates.
        [pid] => unsafe { ffi::OpenProcess(ffi::PROCESS_SET_INFORMATION, 0, *pid) },
        // 0 or >1 new hosts (a concurrent spawn elsewhere in the process):
        // ambiguous — cover the shell only, never guess at a pid.
        _ => 0,
    });
    Ok((Pseudoconsole::Inbox { hpc, conhost }, pipes))
}

impl Drop for AttrList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialized exactly once and not yet deleted.
            unsafe { ffi::DeleteProcThreadAttributeList(self.buf.as_mut_ptr().cast()) };
        }
    }
}

/// Put THIS process back into normal CTRL+C processing — the state every
/// ConPTY child inherits at `CreateProcessW` (see the spawn's comment for the
/// measured defect). `SetConsoleCtrlHandler(NULL, FALSE)` is the documented
/// switch: with a NULL routine, `FALSE` "restores normal processing of CTRL+C
/// input" and that attribute "is inherited by child processes". It needs no
/// attached console to succeed — aterm-gui is a GUI-subsystem image with none —
/// and it is idempotent, so it runs unconditionally per spawn rather than once
/// under a flag whose truth could be undone by a later `CreateProcess` caller
/// in this process. The return value is deliberately ignored: a failure here
/// leaves the pre-existing state, which is exactly what the spawn did before,
/// and nothing about the spawn depends on it.
///
/// THE HOST'S OWN DISPOSITION CHANGES TOO — this is not a child-only switch.
/// For aterm-gui (GUI subsystem, no console) that is moot. For the console
/// image that hosts `--headless` sessions (aterm-cli's `spawn_shell_with_pid`
/// lands in this same spawn), a host that a harness launched with
/// CREATE_NEW_PROCESS_GROUP precisely so it would survive the shared console's
/// Ctrl+C becomes CTRL_C-terminable from its first spawn on. Accepted on
/// purpose: Windows gives a child no way to hold a Ctrl+C state its parent does
/// not hold at CreateProcess, and a host that shrugs off Ctrl+C while every
/// shell under it is deaf to it is the defect. A harness that needs the host
/// to outlive its console's Ctrl+C gives it its own console (DETACHED_PROCESS
/// or CREATE_NEW_CONSOLE) instead: neither flag touches the Ctrl+C
/// disposition, and a host with no shared console has no Ctrl+C to survive.
fn restore_ctrl_c_processing() {
    // SAFETY: NULL routine + FALSE is the documented "restore normal CTRL+C"
    // form; no pointer is dereferenced and no handler is registered.
    unsafe { ffi::SetConsoleCtrlHandler(std::ptr::null(), 0) };
}

/// Clamp a u16 cell count into ConPTY's positive `i16` COORD range.
fn coord(cols: u16, rows: u16) -> ffi::COORD {
    ffi::COORD {
        x: cols.clamp(1, 32767) as i16,
        y: rows.clamp(1, 32767) as i16,
    }
}

/// Spawn the selected shell in a fresh ConPTY of `rows`×`cols`, returning the
/// master key. Same contract as the Unix `spawn_shell_with_pid` (see the crate docs and
/// [`spawn_shell_with_pid`] for the full sequence); this thin wrapper drops the
/// pid and preserves the historical hardened default limits.
///
/// # Errors
/// See [`spawn_shell_with_pid`].
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub fn spawn_shell(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
) -> io::Result<i32> {
    spawn_shell_with_pid(
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        aterm_sandbox::Limits::shell_default(),
    )
    .map(|s| s.master)
}

/// Like [`spawn_shell`] but also returns the child pid (see
/// [`SpawnedShell`]) — the Windows ConPTY spawn sequence.
///
/// There is no fork window here, so the Unix status-pipe protocol is replaced
/// by SYNCHRONOUS failures — fail-closed is structural. The confinement window
/// is `CREATE_SUSPENDED`: the child is created with ZERO instructions run,
/// confined (Job Object with KILL_ON_JOB_CLOSE), and only then resumed — the
/// exit-before-exec guarantee (ATERM_DESIGN §5.6) in suspended-resume form. A
/// weak `Cap<Sandbox>` is denied BEFORE any process exists (SEC-2 parity with
/// the Unix child's `_exit(126)`).
///
/// `sandbox_wrap = Some(_)` (the macOS Seatbelt SBPL wrap) is REFUSED with
/// `Unsupported`: a caller that demanded an OS sandbox must never get an
/// unsandboxed shell. (In practice unreachable: `decide_spawn` only emits a
/// profile on macOS, and refuses a Containment spawn everywhere else.)
///
/// `limits` are actuated on the Job Object while the child is still SUSPENDED
/// (`aterm_sandbox::Limits::apply_to_job`): the address-space, CPU,
/// active-process and UI-restriction lanes fold into the job before resume. A
/// limit that cannot be installed terminates the never-resumed child and fails
/// closed with the actuator's error.
///
/// # Errors
/// `PermissionDenied` if either capability tier is too low or the Job-Object
/// confinement could not be installed (the suspended child is terminated —
/// never resumed unconfined); `Unsupported` for `sandbox_wrap`; otherwise the
/// OS error from pipe/ConPTY/process creation (a bogus `-e` program surfaces
/// here as `CreateProcessW`'s not-found error — the `_exit(127)` analog, with
/// no session leaked).
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    limits: aterm_sandbox::Limits,
) -> io::Result<SpawnedShell> {
    spawn_shell_with_pid_cell_px(
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        limits,
        None,
    )
}

/// Unix-seam parity for
/// [`spawn_shell_with_pid_cell_px`](crate::spawn_shell_with_pid_cell_px): ConPTY
/// sizes in CHARACTER cells only, so the metrics have nowhere to land and are
/// ignored. Same errors as [`spawn_shell_with_pid`].
///
/// # Errors
/// See [`spawn_shell_with_pid`].
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid_cell_px(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[std::ffi::OsString]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    limits: aterm_sandbox::Limits,
    // Unix-seam parity (the crate's public signatures are identical on both
    // platforms). ConPTY's resize wire is a `COORD` of CHARACTER cells with no
    // pixel fields at all, so the host's cell metrics have nowhere to go here
    // and the argument is accepted and ignored rather than absent — a Windows
    // build of the frontend keeps compiling against one call shape.
    _cell_px: Option<(u16, u16)>,
) -> io::Result<SpawnedShell> {
    spawn_on_host(
        SpawnRequest {
            rows,
            cols,
            cap,
            sandbox_cap,
            env_add,
            shell_override,
            shell_args,
            argv_override,
            exec_command,
            cwd,
            sandbox_wrap,
            limits,
        },
        true,
    )
}

/// One spawn's inputs, so a spawn whose OpenConsole failed its health check can
/// be made again on the inbox host ([`spawn_on_host`]).
#[derive(Clone, Copy)]
struct SpawnRequest<'a> {
    rows: u16,
    cols: u16,
    cap: &'a aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &'a aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &'a [(String, String)],
    shell_override: Option<&'a str>,
    shell_args: Option<&'a [String]>,
    argv_override: Option<&'a [String]>,
    exec_command: Option<&'a [std::ffi::OsString]>,
    cwd: Option<&'a str>,
    sandbox_wrap: Option<&'a str>,
    limits: aterm_sandbox::Limits,
}

/// The spawn sequence of [`spawn_shell_with_pid`], with the console host the
/// frontend chose when `allow_openconsole` (the inbox host otherwise).
fn spawn_on_host(req: SpawnRequest<'_>, allow_openconsole: bool) -> io::Result<SpawnedShell> {
    let SpawnRequest {
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        limits,
    } = req;
    // (1) Spawn cap gate — identical PermissionDenied mapping to the Unix seam.
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
    // (2) Sandbox cap gate — SEC-2: a weak `Cap<Sandbox>` must never yield a
    // shell. Unix enforces this inside the child's `Limits::apply` (exit-
    // before-exec); with no fork window we enforce it HERE, before any process
    // exists. Step (8) re-gates via `limits.apply_to_job(sandbox_cap, job)`
    // (harmless: the actuator re-checks the same Trusted tier).
    aterm_cap::require(sandbox_cap, aterm_cap::Tier::Trusted).map_err(|e| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("sandbox capability tier too low — fail-closed: shell not spawned ({e})"),
        )
    })?;
    // (3) OS-sandbox wrap: not available on Windows — NEVER ignore a demanded
    // sandbox (fail-closed, ATERM_DESIGN §5.6).
    if sandbox_wrap.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OS sandbox wrap demanded but not available on Windows — refusing to spawn an \
             unsandboxed shell (fail-closed)",
        ));
    }

    // (4) Resolve program+argv (shell.rs), env block (shared `build_child_env`
    // → case-insensitive block builder), command line, cwd.
    //
    // A NUL cannot reach a Windows program whole: every wide string this seam
    // hands Windows ends at its first NUL, so running what is left is running a
    // different command, and the spawn is refused instead (the Unix seam's
    // rule). The configured shell is checked as configured, BEFORE it is
    // resolved, because the resolution reads only up to the NUL too:
    // `SearchPathW` finds `cmd.exe` for `cmd.exe\0x`, and the program would hold
    // no NUL by then. Only when the shell is what runs: an `-e` command does not
    // run it. `$COMSPEC` and a PATH hit cannot hold a NUL.
    let runs_program = exec_command.is_some_and(|c| !c.is_empty());
    if !runs_program && shell_override.is_some_and(|s| s.contains('\0')) {
        return Err(crate::program_nul_refusal(
            "the shell (set by --shell or `shell` in aterm.toml)",
        ));
    }
    let (program, argv) =
        shell::resolve_spawn_target(shell_override, shell_args, argv_override, exec_command);
    // An argument holding a NUL would end the command line `CreateProcessW`
    // reads right there, dropping it and every argument after it. An `-e`
    // program is argv[0], so this covers it as well.
    if let Some(index) = argv.iter().position(|a| {
        use std::os::windows::ffi::OsStrExt as _;
        a.encode_wide().any(|u| u == 0)
    }) {
        return Err(crate::argv_nul_refusal("the command line", index));
    }
    let program_w = wide_nul(&program);
    let mut cmdline_w = build_command_line(&argv);
    let mut env_pairs = crate::build_child_env(std::env::vars_os(), env_add);
    // Refresh PATH from the LIVE registry so a tool installed after Explorer started
    // (whose dir is on the registry PATH but not on Explorer's stale process PATH) is
    // found in a freshly-opened terminal without a session restart. See `winpath`.
    winpath::refresh_child_path(&mut env_pairs);
    let mut env_block = build_env_block(&env_pairs);
    let cwd_w = resolve_cwd_wide(cwd);

    // (5) Pipes + the pseudoconsole, in the console host the frontend chose
    // (`create_pseudoconsole`: OpenConsole when one resolves and starts, else
    // the inbox `CreatePseudoConsole`).
    let (pcon, pipes) = create_pseudoconsole(coord(cols, rows), &program, allow_openconsole)?;
    // The host duplicated its two pipe ends; drop ours now. We retain in_write
    // (keystrokes in) and out_read (VT out).
    let Pipes {
        in_read,
        in_write,
        out_read,
        out_write,
    } = pipes;
    drop(in_read);
    drop(out_write);

    // (6) Attribute list carrying the pseudoconsole. The attribute VALUE is a
    // pointer passed by value: the inbox HPCON itself (the documented ConPTY
    // sample), or, for OpenConsole, the address of its three slots (winconpty's
    // `PseudoConsole` struct, see `openconsole`) — kernelbase reads them during
    // `CreateProcessW`, so the array lives on this frame across that call.
    let mut attr = AttrList::new(1)?;
    let mut slots: [isize; 3] = match &pcon {
        Pseudoconsole::Inbox { .. } => [0; 3],
        Pseudoconsole::OpenConsole { host, .. } => host.slots(),
    };
    let value: *mut c_void = match &pcon {
        Pseudoconsole::Inbox { hpc, .. } => hpc.0 as *mut c_void,
        Pseudoconsole::OpenConsole { .. } => slots.as_mut_ptr().cast(),
    };
    // SAFETY: initialized list; `value` is the live HPCON or the address of
    // `slots`, both valid until `attr` is dropped after CreateProcessW.
    let updated = unsafe {
        ffi::UpdateProcThreadAttribute(
            attr.ptr(),
            0,
            ffi::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            value,
            std::mem::size_of::<isize>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if updated == 0 {
        return Err(io::Error::last_os_error());
    }

    // (7) CreateProcessW, SUSPENDED (the confinement window: zero instructions
    // run until step (9) resumes). No handle inheritance — ConPTY carries the
    // pipes. Failure here IS the `_exit(127)` analog: a bogus `-e` program
    // surfaces as the OS not-found error, and no session is leaked.
    // SAFETY: zeroed STARTUPINFOEXW is valid (cb + lpAttributeList set below);
    // all pointers passed live across the call.
    let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr.ptr();
    // STARTF_USESTDHANDLES with NULL hStd* — LOAD-BEARING (verified on this
    // box): since Win8, CreateProcess silently DUPLICATES a parent's
    // non-console std handles (pipes/files — e.g. aterm spawned from a CI
    // runner, a redirected launcher, or the test harness) into a console-
    // subsystem child even with bInheritHandles = FALSE and no
    // STARTF_USESTDHANDLES. The child then keeps stdout = the parent's pipe
    // while its CONSOLE is the pseudoconsole — every byte bypasses ConPTY.
    // Explicitly claiming the std-handle fields (as NULL) suppresses that
    // duplication, so the child's console init binds its stdio to the fresh
    // pseudoconsole in every launch environment.
    si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
    // CTRL+C INHERITANCE — LOAD-BEARING (audit 2026-09-22, reproduced): an
    // aterm launched by a harness that used CREATE_NEW_PROCESS_GROUP had Ctrl+C
    // dead in EVERY tab (0x03 reached the shell as text; a running loop never
    // stopped), while Explorer/shell-launched instances were fine. That flag
    // sets the launched process's "ignore CTRL+C" state, Windows copies that
    // state into each child at CreateProcess, and the ConPTY child inherits it
    // from US — so the shell in the new tab starts deaf to Ctrl+C no matter
    // what the terminal writes. Restore normal processing before every spawn so
    // the child's state is the same regardless of who launched aterm.
    restore_ctrl_c_processing();
    // SAFETY: PROCESS_INFORMATION is POD out-memory.
    let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: program_w/cmdline_w/env_block are NUL-terminated (double-NUL for
    // the block) buffers alive across the call; cwd_w likewise when Some.
    let created = unsafe {
        ffi::CreateProcessW(
            program_w.as_ptr(),
            cmdline_w.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0, // bInheritHandles = FALSE
            ffi::EXTENDED_STARTUPINFO_PRESENT
                | ffi::CREATE_UNICODE_ENVIRONMENT
                | ffi::CREATE_SUSPENDED,
            env_block.as_mut_ptr().cast::<c_void>(),
            cwd_w.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    let h_process = ffi::Handle::new(pi.hProcess);
    let h_thread = ffi::Handle::new(pi.hThread);

    // (8) Confinement window (the exit-before-exec analog — the child is
    // SUSPENDED, zero instructions run): Job Object with KILL_ON_JOB_CLOSE,
    // then assignment, then `limits.apply_to_job` folds the requested resource
    // limits into the job. ANY failure ⇒ terminate the never-resumed child and
    // fail closed.
    let fail_closed = |what: &str| -> io::Error {
        let os = io::Error::last_os_error();
        // SAFETY: the suspended child never executed an instruction; terminate
        // + guard-drop close everything (the §5.6 exit-before-exec guarantee).
        unsafe { ffi::TerminateProcess(pi.hProcess, 126) };
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} ({os}) — fail-closed: shell not resumed"),
        )
    };
    // SAFETY: NULL attributes/name — a fresh anonymous job.
    let job = unsafe { ffi::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if job == 0 {
        return Err(fail_closed("CreateJobObjectW failed"));
    }
    let job = ffi::Handle::new(job);
    // SAFETY: zeroed extended-limit info is valid; only LimitFlags is set.
    let mut info: ffi::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = ffi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // `outside_job`'s tests: a tab whose programs leave its job, as a packaged
    // shell's do.
    #[cfg(test)]
    if outside_job::JOB_LETS_GO.with(std::cell::Cell::get) {
        info.BasicLimitInformation.LimitFlags |= outside_job::JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
    }
    // SAFETY: valid job handle + fully-initialized info struct of the stated size.
    let set = unsafe {
        ffi::SetInformationJobObject(
            job.raw(),
            ffi::JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            std::ptr::addr_of_mut!(info).cast::<c_void>(),
            std::mem::size_of::<ffi::JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if set == 0 {
        return Err(fail_closed("SetInformationJobObject failed"));
    }
    // SAFETY: valid job + suspended-process handles.
    if unsafe { ffi::AssignProcessToJobObject(job.raw(), pi.hProcess) } == 0 {
        return Err(fail_closed("AssignProcessToJobObject failed"));
    }
    // Actuate the requested resource limits on the job while the child is still
    // SUSPENDED. Query-modify-write preserves the KILL_ON_JOB_CLOSE flag just
    // set. Err ⇒ terminate the never-resumed child and fail closed (§5.6),
    // propagating the actuator's OWN error (PermissionDenied for a weak cap,
    // else the Job Object OS error) rather than a stale last-error.
    if let Err(e) = limits.apply_to_job(sandbox_cap, job.raw() as std::os::windows::io::RawHandle) {
        // SAFETY: the suspended child never executed an instruction.
        unsafe { ffi::TerminateProcess(pi.hProcess, 126) };
        return Err(e);
    }
    // The hangup event, created BEFORE resume so a failure still takes the
    // terminate-the-suspended-child path (never a live child without hangup).
    // SAFETY: NULL attrs/name; manual-reset (TRUE), initially unsignaled.
    let close_evt = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    if close_evt == 0 {
        return Err(fail_closed("CreateEventW failed"));
    }
    let close_evt = ffi::Handle::new(close_evt);

    // (9) Resume, register, start the waiter.
    // SAFETY: the child's (suspended) primary thread handle.
    if unsafe { ffi::ResumeThread(h_thread.raw()) } == u32::MAX {
        return Err(fail_closed("ResumeThread failed"));
    }
    drop(h_thread); // done with the thread handle
    // (9b) An OpenConsole proves itself only once its first client connects:
    // it must answer the shell's connect with its first output. Until that
    // output a console-subsystem shell is held inside its console connect and
    // has run nothing of its own (`openconsole::await_started` has the
    // snapshot's record), so a host that fails here is dropped, the shell's job
    // with it, and the spawn is made again on the inbox host — the tab always
    // opens. A host that exited or wrote no VT is kept from later tabs; one that
    // only stayed silent costs this tab, until it has done so three times in a
    // row. A program that never connects (a GUI image that slipped past
    // `create_pseudoconsole`'s check, e.g. through an app execution alias) or
    // exited before it did is no failure of the host's: it has run its own
    // code, so it is neither ended nor run again, and the session goes on in
    // this host for the waiter to report its exit.
    let failed = match &pcon {
        Pseudoconsole::Inbox { .. } => None,
        Pseudoconsole::OpenConsole { host, exe } => {
            match openconsole::await_started(host, h_process.raw(), out_read.raw()) {
                openconsole::Health::Started => {
                    openconsole::note_started(exe);
                    None
                }
                openconsole::Health::NoClient => None,
                openconsole::Health::Failed { why, sticky } => Some((exe.clone(), why, sticky)),
            }
        }
    };
    if let Some((exe, why, sticky)) = failed {
        // SAFETY: the job we just created; it kills the held shell.
        unsafe { ffi::TerminateJobObject(job.raw(), 126) };
        // Every handle of the failed attempt goes before the retry makes its own.
        drop((pcon, attr, job, h_process, close_evt, in_write, out_read));
        if sticky {
            openconsole::note_launch_failure(&exe, &why);
        } else {
            openconsole::note_silent_start(&exe, &why);
        }
        return spawn_on_host(req, false);
    }
    // Windows pids are small multiples of 4 in practice; a DWORD above i32::MAX
    // would truncate here (theoretical, documented).
    let pid = pi.dwProcessId as i32;
    // Inbox: we own the HPCON and resize/close through it; its signal pipe is
    // conhost's private business. OpenConsole: no HPCON — the three slots are
    // ours, and resize/close go through the signal pipe.
    let (hpc, [signal, reference, conhost], host) = match pcon {
        Pseudoconsole::Inbox { hpc, conhost } => (
            hpc.release(),
            [0, 0, conhost.into_raw()],
            ConsoleHostKind::Inbox,
        ),
        Pseudoconsole::OpenConsole { host, .. } => {
            (0, host.into_raw(), ConsoleHostKind::OpenConsole)
        }
    };
    let session = Arc::new(WinSession {
        hpc: Mutex::new(hpc),
        signal: Mutex::new(signal),
        reference: Mutex::new(reference),
        host,
        input: in_write.into_raw(),
        output: out_read.into_raw(),
        process: h_process.into_raw(),
        conhost,
        job: job.into_raw(),
        outside_job_ended: Mutex::new(false),
        close_evt: close_evt.into_raw(),
        pid: pi.dwProcessId,
        exit_code: Mutex::new(None),
        resize_guard: {
            let size = coord(cols, rows);
            ResizeGuard::new(Some((size.y, size.x)), host)
        },
        input_severed: AtomicBool::new(false),
    });
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    SESSIONS.lock_or_recover().insert(key, Arc::clone(&session));
    let waiter_session = Arc::clone(&session);
    let spawned = std::thread::Builder::new()
        .name("aterm-pty-waiter".into())
        .spawn(move || waiter(&waiter_session));
    if spawned.is_err() {
        // Without the waiter, EOF never fires and the tab never closes — tear
        // the just-registered session down instead of leaking it.
        SESSIONS.lock_or_recover().remove(&key);
        // SAFETY: job handle owned by `session` (still alive via our Arc).
        unsafe { ffi::TerminateJobObject(session.job, 126) };
        return Err(io::Error::other("failed to spawn the ConPTY waiter thread"));
    }
    Ok(SpawnedShell { master: key, pid })
}

/// Close the handles an inbound handoff carried, skipping the 0 slots (a
/// handoff may legitimately arrive without a signal pipe). Used only on the
/// [`adopt_handoff`] failure paths that CONSUME the handoff — see its
/// `# Ownership` section, whose whole value to the broker is that it never has
/// to guess which side owns what.
fn release_handoff_handles(handles: [isize; 4]) {
    for h in handles {
        if h != 0 {
            // SAFETY: each handle was handed to us and is not owned by any live
            // session — this runs only before a session was built, or instead
            // of building one.
            unsafe { ffi::CloseHandle(h) };
        }
    }
}

/// ADOPT an inbound Windows 11 default-terminal (DefTerm) handoff: build a
/// session around handles conhost ALREADY created, instead of creating our own.
///
/// This is the second constructor the DefTerm lane needs, and it is the exact
/// inverse of [`spawn_shell_with_pid`] in who owns what. On the normal path we
/// create the pipes, call `CreatePseudoConsole`, and `CreateProcessW` the child.
/// On this path a console program was launched by somebody else — a
/// double-clicked `.bat`, `Win+R cmd`, an installer shelling out — conhost
/// created the pseudoconsole for it, then handed the far ends to the registered
/// default terminal over COM. **We do NOT call `CreatePseudoConsole`; conhost
/// owns it**, so `hpc` stays 0 and every operation that would have gone through
/// the HPCON goes through the SIGNAL PIPE instead ([`resize`],
/// [`close_pseudoconsole`]).
///
/// Handle roles, named from OUR side of the pipes so there is nothing to
/// misread (the COM interface's own parameter names are written from conhost's
/// perspective, and mapping them is the BROKER's job, not this function's):
/// * `in_pipe` — we `WriteFile` keystrokes into it; conhost reads.
/// * `out_pipe` — we `ReadFile` VT output from it; conhost writes.
/// * `signal_pipe` — we write ConPTY signal packets into it (resize; closing it
///   tears the session down). Pass 0 if the handoff carried none: the session
///   still works, it just cannot be resized, which is strictly better than
///   refusing to show the user their console at all.
/// * `client_process` — the console program's process handle. LOAD-BEARING: the
///   waiter keys EOF on exactly this handle, so it must be the client, never
///   conhost. If it is 0 there is nothing to wait on and we fail rather than
///   register a session that can never exit (a tab that never closes).
///
/// # Ownership
/// On success the session OWNS all four handles and closes them on drop — the
/// caller must not close them.
///
/// On failure the rule is by ERROR KIND, so the broker never has to guess or
/// parse a message:
/// * `InvalidInput` — the refusal happens before anything is taken, so the
///   caller still owns every handle and can answer the COM call with an error
///   and let conhost fall back. This is the only failure that leaves something
///   for a fallback to use.
/// * `Other` — the handles are CONSUMED and already closed; the caller must not
///   close them again. That is true of the waiter-spawn failure (the session
///   exists, and dropping it closes them) and it is made true of the
///   `CreateEventW` failure by [`release_handoff_handles`], which closes all
///   four before returning. Uniform closure is deliberate: the alternative
///   would be a broker that leaks four handles precisely in the
///   handle-exhaustion scenario that produced the failure.
///
/// # Errors
/// `InvalidInput` when `client_process` is 0 (see above). `Other` when the
/// hangup event cannot be created, or when the waiter thread cannot be spawned
/// — in the latter case the just-registered session is removed again, because
/// without a waiter EOF never fires and the tab could never close.
///
/// The waiter, the reader/writer paths, the registry and the exit-code plumbing
/// are REUSED VERBATIM: the waiter already keys on the client process handle,
/// which is exactly what the handoff hands us. That reuse is the whole reason
/// this lane is small.
pub fn adopt_handoff(
    in_pipe: isize,
    out_pipe: isize,
    signal_pipe: isize,
    client_process: isize,
) -> io::Result<SpawnedShell> {
    if client_process == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "adopt_handoff: a handoff without a client process handle has no EOF source \
             (the waiter keys on it); refusing to register an unexitable session",
        ));
    }
    // The hangup event. LOAD-BEARING, and not merely for `hangup`: the waiter
    // blocks on the ARRAY [process, close_evt], and `WaitForMultipleObjects`
    // fails IMMEDIATELY (WAIT_FAILED) if any handle in the array is invalid. A
    // 0 here would therefore drop the waiter straight through to its teardown
    // and close the signal pipe the instant the session was adopted — the
    // console would flash and vanish. Created BEFORE the session is registered,
    // so a failure yields no session at all rather than a live one that can
    // never be hung up (the same rule the spawn path follows).
    // SAFETY: NULL attrs/name; manual-reset (TRUE), initially unsignaled.
    let close_evt = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    if close_evt == 0 {
        // CONSUME, so the `# Ownership` rule stays "InvalidInput ⇒ yours, Other
        // ⇒ ours" with no third case for the broker to special-case. Closing the
        // pipes lets conhost tear its own session down; leaking them under the
        // handle exhaustion that caused this would strand the client's console.
        release_handoff_handles([in_pipe, out_pipe, signal_pipe, client_process]);
        return Err(io::Error::other(
            "adopt_handoff: CreateEventW failed; refusing to register a session \
             the waiter cannot wait on and nothing can hang up",
        ));
    }

    // The child's pid, for the pid-keyed public ops (hangup/reap/exit_code).
    // A failure here is NOT fatal: 0 simply means those pid-keyed lookups miss
    // for this session, and every one of them is documented as a no-op on a
    // miss. The master-keyed ops (read/write/resize/close) are unaffected.
    // SAFETY: `client_process` is a live process handle owned by the caller
    // until we take ownership below.
    let pid = unsafe { ffi::GetProcessId(client_process) };

    let session = Arc::new(WinSession {
        // No HPCON: conhost kept the pseudoconsole. This 0 is what makes every
        // HPCON-shaped operation take its signal-pipe branch.
        hpc: Mutex::new(0),
        signal: Mutex::new(signal_pipe),
        reference: Mutex::new(0),
        // The DefTerm handoff comes from Windows' own conhost.
        host: ConsoleHostKind::Inbox,
        input: in_pipe,
        output: out_pipe,
        process: client_process,
        // No conhost handle: the focus-boost lane wants PROCESS_SET_INFORMATION
        // on the host, and the handoff's server-process handle is not plumbed
        // through this signature. The boost then covers nothing for adopted
        // sessions rather than the wrong process — `set_focus_boost` already
        // treats 0 as "not discovered" on the spawn path, so this is the
        // existing, tested degradation, not a new one.
        conhost: 0,
        // No job: we did not create this process and must never sweep it. The
        // client belongs to whoever launched it; see `waiter`'s `s.job != 0`
        // guard for why there is deliberately no kill escalation here.
        job: 0,
        // Nor are the programs it runs ours to end (`outside_job::end` takes
        // nothing without a job).
        outside_job_ended: Mutex::new(false),
        close_evt,
        pid,
        exit_code: Mutex::new(None),
        // conhost sized this pseudoconsole before the handoff and did not say
        // to what, so the first resize counts as a change.
        resize_guard: ResizeGuard::new(None, ConsoleHostKind::Inbox),
        input_severed: AtomicBool::new(false),
    });
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    SESSIONS.lock_or_recover().insert(key, Arc::clone(&session));
    let waiter_session = Arc::clone(&session);
    let spawned = std::thread::Builder::new()
        .name("aterm-pty-waiter".into())
        .spawn(move || waiter(&waiter_session));
    if spawned.is_err() {
        // Same rule as the spawn path: without the waiter, EOF never fires and
        // the tab never closes. Unlike the spawn path there is no job to
        // terminate — dropping the last Arc closes the handles, which breaks
        // the pipes and lets conhost tear its own session down.
        SESSIONS.lock_or_recover().remove(&key);
        return Err(io::Error::other(
            "failed to spawn the ConPTY waiter thread for an adopted handoff",
        ));
    }
    Ok(SpawnedShell {
        master: key,
        pid: pid as i32,
    })
}

/// The `lpCurrentDirectory` for the spawn, as a wide NUL-terminated buffer.
/// An explicit-but-invalid `cwd` returns `None` (inherit) — mirroring the Unix
/// best-effort `chdir`, because an invalid `lpCurrentDirectory` makes
/// `CreateProcessW` hard-fail, which Unix did NOT. With no explicit `cwd`, a
/// process started in `%SystemRoot%\System32` (the Start-menu/shortcut analog
/// of the Finder `/` launch) begins in `%USERPROFILE%` instead.
fn resolve_cwd_wide(cwd: Option<&str>) -> Option<Vec<u16>> {
    if let Some(dir) = cwd {
        if std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
            return Some(wide_nul(OsStr::new(dir)));
        }
        return None;
    }
    let cur = std::env::current_dir().ok()?;
    let sysroot = std::env::var_os("SystemRoot")?;
    let sys32 = std::path::Path::new(&sysroot).join("System32");
    if paths_eq_ignore_ascii_case(&cur, &sys32) {
        let home = std::env::var_os("USERPROFILE").filter(|h| !h.is_empty())?;
        return Some(wide_nul(&home));
    }
    None
}

/// Case-insensitive (ASCII) path equality, ignoring trailing separators — the
/// `%SystemRoot%\System32` launch check only ever compares ASCII system paths.
fn paths_eq_ignore_ascii_case(a: &std::path::Path, b: &std::path::Path) -> bool {
    let a = a.as_os_str().to_string_lossy();
    let b = b.as_os_str().to_string_lossy();
    a.trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

/// HANG UP a spawned shell: signal the session's close event so the WAITER
/// thread runs `ClosePseudoConsole` — the child console session receives the
/// console-close signal (CTRL_CLOSE_EVENT), the moral equivalent of
/// SIGHUP-on-controlling-tty, and exits; the broken out pipe then EOFs the
/// reader. First it ends the programs below the shell that the session's job
/// does not hold ([`outside_job`]), which no close of the job reaches: one
/// process snapshot, a few milliseconds, once per session. Otherwise
/// non-blocking (the close itself runs on the waiter thread), so it is
/// UI-thread-safe like the Unix `killpg(SIGHUP)`. Best-effort
/// no-op for a non-positive/unknown pid (the ESRCH contract) — and for an
/// AMBIGUOUS one: see [`session_by_pid`], which refuses a pid two live sessions
/// claim rather than hang up an arbitrary one of them.
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    if let Some(s) = session_by_pid(pid) {
        // On the caller's thread, while the shell still runs: a quit can follow
        // a tab's close within milliseconds and exit before the waiter wakes,
        // and the kernel's close of the job reaches only what the job holds.
        outside_job::end_once(&s);
        // SAFETY: event handle owned by the session Arc we hold.
        unsafe { ffi::SetEvent(s.close_evt) };
    }
}

/// End, in every open session, the programs below its shell that its job does
/// not hold ([`outside_job`]: what a Store pwsh starts): the quit's half of a
/// tab's close. A quit closes no session one by one: the process exits, and
/// the kernel's close of each job ends only what that job holds. Synchronous;
/// a few milliseconds per session (one process snapshot). Nothing for an
/// adopted session, and nothing twice for a session a close already took.
pub fn end_programs_outside_jobs() {
    let open: Vec<Arc<WinSession>> = SESSIONS.lock_or_recover().values().cloned().collect();
    for s in &open {
        outside_job::end_once(s);
    }
}

/// Reap an exited child WITHOUT ever blocking unboundedly (the Unix `reap`'s
/// poll/escalate/deadline discipline, on Windows primitives): wait the ~250 ms
/// grace; on timeout `TerminateJobObject` — the SIGKILL-the-process-GROUP
/// escalation, killing the whole descendant tree like `killpg` — then wait out
/// the rest of the ~2 s budget and record the exit code if the child signaled.
/// Runs on the frontend's detached reaper thread exactly as on Unix.
/// Best-effort; a no-op for a non-positive pid, an already-closed session (the
/// ECHILD analog), or a pid two live sessions claim (see [`session_by_pid`] —
/// escalating against a session we cannot identify could kill the wrong tree).
/// Losing the race with [`close_master`]'s registry removal
/// (the session is gone before this runs) is now BENIGN: the per-session waiter
/// thread force-kills a close-surviving child via its job on a bounded wait (see
/// [`waiter`] / `CLOSE_GRACE_MS`), so the SIGKILL escalation no longer depends on
/// this reaper winning that race.
pub fn reap(pid: i32) {
    const KILL_GRACE_MS: u32 = 250;
    const DEADLINE_REST_MS: u32 = 1750;
    if pid <= 1 {
        return;
    }
    let Some(s) = session_by_pid(pid) else {
        return;
    };
    // SAFETY: process handle owned by the session Arc we hold.
    let mut r = unsafe { ffi::WaitForSingleObject(s.process, KILL_GRACE_MS) };
    // `job == 0` is an ADOPTED (DefTerm handoff) session — there is no job, and
    // deliberately no escalation: the client is somebody else's program (an
    // installer mid-write, a `.bat` mid-copy) that conhost handed us a handle
    // to, not a shell we spawned. Killing it because a tab closed would be
    // strictly worse than letting the console go away under it. The bare
    // `TerminateJobObject(0, ..)` this replaces was already a harmless no-op;
    // the guard makes the POLICY explicit rather than incidental.
    if r == ffi::WAIT_TIMEOUT && s.job != 0 {
        // Still alive past the grace ⇒ escalate: kill the whole job tree.
        // SAFETY: job handle owned by the session Arc we hold.
        unsafe { ffi::TerminateJobObject(s.job, 1) };
        // SAFETY: as above.
        r = unsafe { ffi::WaitForSingleObject(s.process, DEADLINE_REST_MS) };
    }
    if r == ffi::WAIT_OBJECT_0 {
        let mut code: u32 = 0;
        // SAFETY: valid process handle + out-param.
        if unsafe { ffi::GetExitCodeProcess(s.process, &mut code) } != 0 {
            s.exit_code.lock_or_recover().get_or_insert(code);
        }
    }
}

/// The recorded exit code of a spawned shell, once it has exited — the
/// `waitpid(-1, &status)` replacement the aterm-cli Windows main loop needs to
/// propagate the shell's exit status. `None` while the child runs, when the
/// pid is unknown, after [`close_master`] removed the session, or when two live
/// sessions claim the pid — [`session_by_pid`] answers nothing rather than some
/// other session's status.
#[must_use]
pub fn exit_code(pid: i32) -> Option<i32> {
    let s = session_by_pid(pid)?;
    let code = *s.exit_code.lock_or_recover();
    code.map(|c| c as i32)
}

/// The Unix start-time reader's twin (2026-09-26, backlog item 5). Windows
/// needs none: every pid-addressed teardown here resolves the pid through
/// THIS process's own session registry ([`session_by_pid`]), which holds the
/// process handle — and a held handle keeps the kernel from reissuing the pid.
#[must_use]
pub fn process_birth(_pid: i32) -> Option<u64> {
    None
}

/// The Unix recorder's twin: nothing to record (see [`process_birth`]); the
/// registry is the identity.
#[must_use]
pub fn record_spawned_shell(pid: i32) -> crate::ShellIdentity {
    crate::ShellIdentity::from_parts(pid, None, true)
}

/// The Unix recorder's twin. There is no seamless handoff off Unix, so nothing
/// is adopted this way; the registry is the identity all the same.
#[must_use]
pub fn record_adopted_shell(pid: i32, _master: i32) -> crate::ShellIdentity {
    crate::ShellIdentity::from_parts(pid, None, false)
}

impl crate::ShellIdentity {
    /// On Windows a pid "still names the shell" exactly when this process's
    /// session registry resolves it unambiguously — the same lookup
    /// [`hangup`] and [`reap`] make before they act.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.pid > 1 && session_by_pid(self.pid).is_some()
    }
}

/// The Unix [`hangup_shell`] twin: [`hangup`] already resolves the pid through
/// the registry, so the identity adds nothing it does not already check. There
/// is no foreground-group fallback (no process groups for a ConPTY).
pub fn hangup_shell(identity: &crate::ShellIdentity, _master: i32) -> crate::ShellHangup {
    if identity.verify() {
        hangup(identity.pid);
        crate::ShellHangup::Shell(identity.pid)
    } else {
        crate::ShellHangup::Nothing
    }
}

/// The Unix [`reap_shell`] twin: [`reap`] resolves the pid through the registry.
pub fn reap_shell(identity: crate::ShellIdentity) {
    reap(identity.pid);
}

/// The Unix `reap_shell_in_background` twin: [`reap_shell`] on a thread of its
/// own, so the caller never waits.
pub fn reap_shell_in_background(identity: crate::ShellIdentity) {
    if identity.pid > 1 {
        std::thread::spawn(move || reap_shell(identity));
    }
}

/// Platform-neutral twin of the Unix collector: the recorded exit code, if the
/// waiter thread has already observed the child end.
///
/// Windows has no signal delivery, so every termination — including a kernel
/// kill — is reported as a CODE. `None` means the same thing it does on Unix:
/// the status is not knowable right now, never that the child succeeded.
#[must_use]
pub fn collect_exit_status(pid: i32) -> Option<crate::ChildExit> {
    exit_code(pid).map(crate::ChildExit::Code)
}

/// Close a session's master key: remove the registry entry (subsequent ops get
/// the documented registry-miss semantics), request hangup so an orphaned
/// child cannot outlive its master (the Unix close-the-master EOF analog), and
/// cancel any in-flight blocking read so a parked reader thread returns. The
/// underlying handles close when the LAST `Arc` reference drops (possibly
/// deferred until the reader returns from `ReadFile` and the waiter exits) —
/// the same close-on-last-drop discipline as the Unix `OwnedFd`.
pub fn close_master(master: i32) {
    let removed = SESSIONS.lock_or_recover().remove(&master);
    if let Some(s) = removed {
        // The programs outside the job, as [`hangup`] ends them (nothing when
        // it already did, or when the shell's own exit did).
        outside_job::end_once(&s);
        // SAFETY: event/pipe handles owned by the Arc we still hold.
        unsafe {
            ffi::SetEvent(s.close_evt);
            ffi::CancelIoEx(s.output, std::ptr::null_mut());
        }
    }
}

/// RAII around a Windows master key: dropping the LAST owner closes the
/// session via [`close_master`]. The Windows twin of wrapping the Unix master
/// fd in an `OwnedFd` — `aterm-session`'s `SinkWriter::new_owned` holds one so
/// the session closes exactly when the last `Arc<SinkWriter>` clone drops.
#[derive(Debug)]
pub struct OwnedMaster(i32);

impl OwnedMaster {
    /// Adopt sole ownership of `master`. Safe by CONTRACT, not by construction:
    /// the caller asserts it is the sole owner of the key — exactly the
    /// contract of the Unix `OwnedFd::from_raw_fd` call this replaces (there
    /// the assertion is `unsafe`; here a double-adopt cannot corrupt memory,
    /// only close a session out from under its other holder).
    #[must_use]
    pub fn adopt(master: i32) -> Self {
        Self(master)
    }

    /// The raw registry key (for callers that read/resize via the `i32` seam;
    /// valid for as long as this owner — or the Arc holding it — is alive).
    #[must_use]
    pub fn as_raw(&self) -> i32 {
        self.0
    }
}

impl Drop for OwnedMaster {
    fn drop(&mut self) {
        close_master(self.0);
    }
}

/// Read up to `buf.len()` bytes of ConPTY output. Returns the byte count
/// (`0` = EOF, `< 0` = error), matching the Unix `read(2)` contract the
/// frontend reader threads key on (`r <= 0` ⇒ exit). Blocking; called ONLY on
/// a session's dedicated reader thread, exactly as on Unix. EOF (`0`) is
/// reported when the out pipe is BROKEN — which happens precisely when the
/// waiter thread ran `ClosePseudoConsole` after child exit/hangup — or when
/// the pseudoconsole is already torn down. A fabricated/closed key returns
/// `-1` (the error half of the contract).
pub fn read(master: i32, buf: &mut [u8]) -> isize {
    let Some(s) = session(master) else {
        return -1;
    };
    read_session(&s, buf)
}

/// [`read`] on a session already looked up.
fn read_session(s: &WinSession, buf: &mut [u8]) -> isize {
    let mut n: u32 = 0;
    let want = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` is a valid writable slice of >= `want` bytes; the output
    // handle stays open for the call because the caller holds the session Arc.
    let ok = unsafe {
        ffi::ReadFile(
            s.output,
            buf.as_mut_ptr(),
            want,
            &mut n,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        s.resize_guard.note_output(&buf[..n as usize]);
        return n as isize;
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ffi::ERROR_BROKEN_PIPE as i32) || *s.hpc.lock_or_recover() == 0 {
        0 // EOF — the exact `r <= 0` trigger the reader threads key on
    } else {
        -1
    }
}

/// Outcome of [`read_or_wake`] — mirror of the Unix enum so the shared reader loop is
/// one code path. On Windows only `Data`/`Eof` occur (see [`read_or_wake`]): there is
/// no wake pipe and no bounded poll, so `Wake`/`Idle` are never produced here — they
/// exist only so the shared `spawn_pty_reader` match compiles on both targets.
pub enum ReadOutcome {
    Data(usize),
    Eof,
    /// Never produced on Windows (ConPTY teardown EOFs the reader — see
    /// [`make_wake_pipe`]); present to mirror the Unix enum for the shared loop.
    Wake,
    /// Never produced on Windows (a plain blocking read has no poll-timeout / stop-flag
    /// path); present to mirror the Unix enum for the shared loop.
    Idle,
    /// Never produced on Windows (the waiter thread closes the pseudoconsole when the
    /// child exits, which EOFs the reader); present to mirror the Unix enum.
    Exited,
}

/// Windows has no orphaned-different-pgroup reader-pin problem (ConPTY teardown flows
/// through `ClosePseudoConsole`, which breaks the output pipe and EOFs the reader), so
/// there is no wake pipe: return `None` and let the reader use a plain blocking read —
/// exactly the pre-MEM-L2 behavior, no regression.
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    None
}

/// With no wake pipe on Windows (`wake_rd` is always the `-1` sentinel), this is a
/// plain blocking read reported as [`ReadOutcome`]. No exit watch either: the
/// child's exit already ends the read.
pub fn read_or_wake(master: i32, buf: &mut [u8], _wake_rd: i32, _exit_rd: i32) -> ReadOutcome {
    let n = read(master, buf);
    if n > 0 {
        ReadOutcome::Data(n as usize)
    } else {
        ReadOutcome::Eof
    }
}

/// Windows counterpart of the unix batched-gather top-up: a passthrough. ConPTY
/// reads aren't capped at the macOS ~1 KiB tty outq, so the per-chunk lockstep
/// the unix drain bridges does not exist here.
pub fn drain_more(_master: i32, _buf: &mut [u8], filled: usize) -> usize {
    filled
}

/// Top `buf[filled..]` up with ConPTY output that is already waiting in the
/// pipe, never blocking: `PeekNamedPipe` says how much is there, and each
/// `ReadFile` asks for no more than that. Returns the new fill. A closed
/// session, a broken pipe or an empty one ends the top-up; the next blocking
/// [`read`] reports an end.
///
/// The reader's gather while its session's output is held (aterm-gui's
/// `spawn::gather_held_output`): a program that writes a line per call hands
/// the blocking read one line at a time, and what piled up behind it is
/// collected here into one batch.
pub fn drain_available(master: i32, buf: &mut [u8], mut filled: usize) -> usize {
    let Some(s) = session(master) else {
        return filled;
    };
    while filled < buf.len() {
        let mut avail: u32 = 0;
        // SAFETY: a peek of the session's own live output pipe end (the Arc
        // keeps it open) that reads no bytes: a null buffer of size 0 and a
        // valid out-param for the count waiting.
        let peeked = unsafe {
            ffi::PeekNamedPipe(
                s.output,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 || avail == 0 {
            break;
        }
        let want = (avail as usize).min(buf.len() - filled);
        let n = read_session(&s, &mut buf[filled..filled + want]);
        if n <= 0 {
            break;
        }
        filled += n as usize;
    }
    filled
}

/// No wake pipe on Windows — nothing to signal.
pub fn wake(_wake_wr: i32) {}

/// No wake-pipe fds on Windows — nothing to close.
pub fn close_fd(_fd: i32) {}

/// No `FD_CLOEXEC` on Windows — inheritance is a per-HANDLE property set at creation
/// (`bInheritHandle` / `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), not a flag toggled on an
/// integer fd. Reports success so the shared caller's `?`/`ok()?` chain proceeds; the
/// only consumer is the POSIX exec-handoff path, which is inert on this platform.
///
/// Mirrors [`crate::set_cloexec`]. Both mirrors must grow together — a shared consumer
/// reads whichever one its target provides.
#[allow(clippy::unnecessary_wraps)]
pub fn set_cloexec(_fd: i32, _on: bool) -> io::Result<()> {
    Ok(())
}

/// Always `false`: Windows has no inherited PTY-master fd to interrogate. The unix
/// original is the last-line "and it's still a tty" assertion on a descriptor adopted
/// across `execve`; there is no such descriptor here, so the honest answer is "no",
/// which makes every adoption attempt reject rather than proceed on a guess.
#[must_use]
pub fn fd_is_tty(_fd: i32) -> bool {
    false
}

/// Windows twin of the Unix [`tty_echo`]: ConPTY exposes no termios, so the
/// line-discipline question has no answer here and the caller keeps its
/// default behaviour (bank the press) — the same fail-open a non-tty fd gets
/// on Unix.
#[must_use]
pub fn tty_echo(_master: i32) -> Option<crate::TtyEcho> {
    None
}

/// Windows twin of the Unix [`input_queue_len`]: ConPTY's input pipe is read
/// by conhost, not by the program, so its depth says nothing about whether the
/// program is reading — `None`, the same "not measured" a non-tty fd gets on
/// Unix, and a caller that gates on the count then never refuses here.
#[must_use]
pub fn input_queue_len(_master: i32) -> Option<usize> {
    None
}

/// Windows twin of the Unix [`output_queue_len`]: `None`, for the reason
/// [`input_queue_len`] gives.
#[must_use]
pub fn output_queue_len(_master: i32) -> Option<usize> {
    None
}

/// Windows twin of the Unix [`tty_passes_every_byte`]: `None` — ConPTY has no
/// line discipline to ask ([`tty_echo`]), so no queue count is read as a read.
#[must_use]
pub fn tty_passes_every_byte(_master: i32) -> Option<bool> {
    None
}

/// Windows twin of the Unix [`flush_input_queue`]: `None`, nothing discarded —
/// there is no measured queue to flush ([`input_queue_len`]).
#[must_use]
pub fn flush_input_queue(_master: i32) -> Option<usize> {
    None
}

/// Write all of `bytes` to the child's input, retrying short writes. Stops
/// silently on any error (matches the Unix Stop-on-error/peer-closed contract;
/// there is no EINTR on Windows). After the child exits, writes still succeed
/// until `ClosePseudoConsole` (conhost holds the pipe) and then break —
/// matching Unix master semantics closely enough for the keyboard path. A
/// fabricated/closed key is a silent no-op (the Unix EBADF Stop analog).
pub fn write_all(master: i32, bytes: &[u8]) {
    let Some(s) = session(master) else {
        return;
    };
    let mut data = bytes;
    while !data.is_empty() {
        match guarded_write(&s, data, None) {
            Ok(written) if written > 0 => data = &data[written..],
            _ => break,
        }
    }
}

thread_local! {
    /// Set while this thread writes inside [`as_reply`].
    static REPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `write` as the terminal's own ANSWER to a query its program or console
/// host sent (DA, DSR/CPR, a colour or mode query): every write it makes on
/// this thread to a session's input passes that session's [`resize_guard`],
/// which stays owed to the next input. OpenConsole holds its API thread for
/// the answer to its own `ESC[6n` after a resize, and the guard's client
/// cannot connect while it does, so a guarded answer stalled the tab 500 ms
/// and, during a window drag, reached Git Bash as typed text (`resize_guard`,
/// REPLIES). Returns what `write` returns; the mark is lifted however
/// `write` ends, and a nested call keeps it until the outer one returns.
pub fn as_reply<T>(write: impl FnOnce() -> T) -> T {
    /// Puts the mark back as it was, on return or unwind.
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REPLYING.with(|r| r.set(self.0));
        }
    }
    let _restore = Restore(REPLYING.with(|r| r.replace(true)));
    write()
}

/// Settle `master`'s [`resize_guard`] for the INPUT write its caller is about
/// to make: when the session owes the guard's OpenConsole client, run it now
/// and return once its record is in ([`console_guard::guard`]). A writer that
/// serializes its frames under a lock of its own (aterm-session's sink, whose
/// frame lock the terminal's answers take too) calls this before it takes that
/// lock: the client cannot connect while OpenConsole waits for the answer to
/// its own cursor query after a resize, so a client run under that lock held
/// the answer back until the host gave up on it (`resize_guard`, REPLIES).
/// An input write that comes while the client runs waits for it here. Inside
/// [`as_reply`], and for a key no live session has, it does nothing; a write
/// made without it settles in [`write_some`] all the same.
pub fn settle_input(master: i32) {
    if REPLYING.with(std::cell::Cell::get) {
        return;
    }
    let Some(s) = session(master) else {
        return;
    };
    s.resize_guard.settle(Writer::Input, |rows, cols| {
        console_guard::guard(&s, rows, cols);
    });
}

/// One write of `bytes` through the session's [`resize_guard`], with the
/// guard's OpenConsole client ([`console_guard::guard`]) when one is owed and
/// the probe of the tab for the MSYS2 runtime its astral rewrite asks; inside
/// [`as_reply`], as a reply that passes the guard. `stop` is the stop flag of
/// the frame the write belongs to ([`write_some_stoppable`]).
fn guarded_write(s: &WinSession, bytes: &[u8], stop: Option<&AtomicBool>) -> io::Result<usize> {
    let writer = if REPLYING.with(std::cell::Cell::get) {
        Writer::Reply
    } else {
        Writer::Input
    };
    s.resize_guard.write_with_client(
        bytes,
        writer,
        |b| write_input(s, b, stop),
        |rows, cols| console_guard::guard(s, rows, cols),
        || console_guard::runtime_in_tab(s),
    )
}

/// Write `bytes` with a single `WriteFile`, returning the count the kernel
/// ACCEPTED (the true count the routing-fabric `SinkWriter` is built on).
/// Blocking — identical to the sink's documented Phase-0 posture. The first
/// writes after a resize also carry the [`resize_guard`] record, which the
/// count leaves out: it reports the CALLER's bytes.
///
/// # Errors
/// `NotFound` for a fabricated/closed key; otherwise the OS error when the
/// pipe is gone.
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    write_some_stoppable(master, bytes, None)
}

/// [`write_some`] for one write of a frame whose stop flag is `stop` (a
/// metered paste's `BulkMeter`, in aterm-session): a stop set before the write
/// begins bars it, and one set while it is blocked in `WriteFile`
/// ([`release_stopped_writes`]) cancels it. Either way it answers what it
/// wrote, `Ok(0)` when nothing (a cancelled `WriteFile` reports nothing
/// written). A sever of the session ([`release_input_writes`]) does the same
/// to every write, with or without a flag.
///
/// # Errors
/// Exactly those of [`write_some`].
pub fn write_some_stoppable(
    master: i32,
    bytes: &[u8],
    stop: Option<&AtomicBool>,
) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let Some(s) = session(master) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "unknown PTY master key (closed or never spawned)",
        ));
    };
    guarded_write(&s, bytes, stop)
}

/// The input writes in a `WriteFile` now ([`input_release`]): a sever
/// ([`release_input_writes`]) or a paste's stop ([`release_stopped_writes`])
/// cancels theirs. ConPTY's input is an anonymous pipe, which has no
/// overlapped mode to wait on, so a write to a program that has stopped
/// reading blocks inside the call, and `CancelSynchronousIo` on the writer's
/// thread is what reaches it there.
static WRITERS: input_release::Registry<WriterThread> = input_release::Registry::new();

/// How often, and for how long, a release tries again for a writer that was
/// registered but not yet blocked when its cancel came ([`input_release`]).
const RELEASE_STEP: std::time::Duration = std::time::Duration::from_millis(1);
const RELEASE_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

/// A registered writer's own thread handle (`CancelSynchronousIo` needs a
/// real handle with `THREAD_TERMINATE`, never the `GetCurrentThread`
/// pseudo-handle). Open while the thread is registered: the thread owns it
/// ([`ThisThread`]) and is inside [`input_release::Registry::write`].
#[derive(Clone, Copy)]
struct WriterThread(isize);

impl input_release::Blocked for WriterThread {
    fn cancel(&self) -> bool {
        // SAFETY: the registry calls this under its lock, for a thread still
        // registered, so the handle (closed only when that thread exits) is open.
        unsafe { ffi::CancelSynchronousIo(self.0) != 0 }
    }
}

/// The calling thread's own handle, duplicated once per thread from its
/// pseudo-handle and closed when the thread exits; 0 when the duplicate failed.
struct ThisThread(isize);

impl ThisThread {
    fn open() -> Self {
        let mut handle: isize = 0;
        // SAFETY: both process handles are the current-process pseudo-handle,
        // the source is the current-thread pseudo-handle, and `handle` is a
        // valid out-pointer.
        let ok = unsafe {
            ffi::DuplicateHandle(
                ffi::GetCurrentProcess(),
                ffi::GetCurrentThread(),
                ffi::GetCurrentProcess(),
                &mut handle,
                0,
                0,
                ffi::DUPLICATE_SAME_ACCESS,
            )
        };
        Self(if ok == 0 { 0 } else { handle })
    }
}

impl Drop for ThisThread {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the duplicate this thread owns; nothing else closes it.
            unsafe { ffi::CloseHandle(self.0) };
        }
    }
}

thread_local! {
    static THIS_THREAD: ThisThread = ThisThread::open();
}

/// This thread as a writer a cancel can reach; `None` without a handle (a
/// failed duplicate, or the thread's locals already torn down).
fn this_writer() -> Option<WriterThread> {
    THIS_THREAD
        .try_with(|t| t.0)
        .ok()
        .filter(|h| *h != 0)
        .map(WriterThread)
}

/// A session's identity in [`WRITERS`]: its address, live while any write
/// tagged with it runs (the writer holds the session's `Arc`).
fn session_tag(s: &WinSession) -> usize {
    std::ptr::from_ref(s) as usize
}

/// Cancel the writes `ids` names now, and keep trying on a thread of its own
/// for any that was registered but not yet blocked, so the caller (the UI
/// thread's `Stop paste`, a tab's close) never waits on a writer.
fn release_soon(mut ids: Vec<u64>) {
    WRITERS.cancel_round(&mut ids);
    if ids.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-input-release".into())
        .spawn(move || WRITERS.release(ids, RELEASE_STEP, RELEASE_BOUND));
    // Without the thread, the writer still sees its flag at its next write.
    drop(spawned);
}

/// Sever `master`'s input for good (aterm-session's `SinkWriter::sever_input`,
/// at a tab's close): every input write blocked in `WriteFile` is cancelled and
/// answers what it wrote, and every later input write on the session writes
/// nothing. Without it, a writer blocked writing to a program that stopped
/// reading stayed blocked until the program read again, holding the session.
/// Idempotent; an unknown key does nothing.
pub fn release_input_writes(master: i32) {
    let Some(s) = session(master) else {
        return;
    };
    release_soon(WRITERS.sever(session_tag(&s), &s.input_severed));
}

/// Release the input writes of the frame whose stop flag is `stop`, which the
/// caller has just set (aterm-session's `BulkMeter::request_stop`): a write of
/// the frame blocked in `WriteFile` is cancelled and answers what it wrote, and
/// one that begins after it writes nothing ([`write_some_stoppable`]).
pub fn release_stopped_writes(stop: &AtomicBool) {
    release_soon(WRITERS.stopped(stop));
}

/// One `WriteFile` of `bytes` to the session's input pipe: the count the
/// kernel accepted, or the OS error. Made through [`WRITERS`], so a sever or
/// `stop` releases it ([`write_some_stoppable`]); a released write answers the
/// count `WriteFile` gave back.
fn write_input(s: &WinSession, bytes: &[u8], stop: Option<&AtomicBool>) -> io::Result<usize> {
    WRITERS.write(
        session_tag(s),
        &s.input_severed,
        stop,
        this_writer(),
        || {
            let mut written: u32 = 0;
            let want = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            // SAFETY: `bytes` is a valid slice of >= `want` bytes; the input handle
            // stays open for the call because the caller holds the session Arc.
            let ok = unsafe {
                ffi::WriteFile(
                    s.input,
                    bytes.as_ptr(),
                    want,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ffi::ERROR_OPERATION_ABORTED) {
                    return Ok(written as usize);
                }
                return Err(error);
            }
            Ok(written as usize)
        },
    )
}

/// [`write_some`] twin of the Unix blocking-emulation wrapper. A ConPTY input
/// pipe is NEVER non-blocking here ([`set_nonblocking`]`(true)` is
/// `Unsupported`), so there is no `WouldBlock` to park on — a plain pass-through
/// keeps the shared `SinkWriter` frame writers one code path.
///
/// # Errors
/// Exactly those of [`write_some`].
pub fn write_some_blocking(master: i32, bytes: &[u8]) -> io::Result<usize> {
    write_some(master, bytes)
}

/// Toggle non-blocking mode — NOT actuated on Windows (honesty rule: never
/// pretend it succeeded). `false` is `Ok` (the pipes are already blocking);
/// `true` is `Err(Unsupported)`. There are zero callers today; the future
/// backpressure milestone will be built on overlapped I/O + a drained ring on
/// Windows rather than a pipe mode bit (`PIPE_NOWAIT` is deprecated).
///
/// # Errors
/// `Unsupported` when `nonblocking` is `true`.
pub fn set_nonblocking(_master: i32, nonblocking: bool) -> io::Result<()> {
    if nonblocking {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-blocking PTY writes are not supported on Windows (overlapped-I/O milestone)",
        ));
    }
    Ok(())
}

/// ConPTY signal-pipe opcode for a window resize.
///
/// MACHINE-VERIFIED, not recalled: `kernelbase!ResizePseudoConsole` on Windows
/// 11 26200 materializes the literal `8` into the first `u16` of a 6-byte stack
/// buffer (`lea r8d,[rbx+8]` / `mov [rsp+0x30],r8w`) and `WriteFile`s exactly 6
/// bytes of it to `hPC->[0]`. See [`encode_resize_signal`].
const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;

/// Build ConPTY's 6-byte resize signal packet: `{opcode, columns, rows}`, three
/// little-endian `u16`s IN THAT ORDER.
///
/// The column/row ORDER is the trap and the reason this is a separate, tested
/// function: everything else in this module passes `rows, cols` (the Unix
/// `winsize` habit), while the wire packet is `cols, rows` — the COORD `.X`
/// before `.Y`. Verified against the shipping `kernelbase!ResizePseudoConsole`,
/// which writes `dx` (COORD.X, columns) at buffer offset 2 and `ax`
/// (COORD.Y, rows) at offset 4.
///
/// Counts are clamped through [`coord`] into ConPTY's positive `i16` range
/// before the `u16` cast, so a caller's `0` or `u16::MAX` cannot become a
/// negative COORD that `ResizePseudoConsole` would reject (the HPCON path
/// rejects those explicitly — `test dx,dx / js error`) or that conhost would
/// read as a garbage viewport on the adopted path.
fn encode_resize_signal(rows: u16, cols: u16) -> [u8; 6] {
    let c = coord(cols, rows);
    let mut buf = [0u8; 6];
    buf[0..2].copy_from_slice(&PTY_SIGNAL_RESIZE_WINDOW.to_le_bytes());
    buf[2..4].copy_from_slice(&(c.x as u16).to_le_bytes());
    buf[4..6].copy_from_slice(&(c.y as u16).to_le_bytes());
    buf
}

/// Resize the pseudoconsole to `rows`×`cols`. Thread-safe: the session's HPCON
/// mutex makes UI-thread + control-thread resizes safe against the waiter's
/// `ClosePseudoConsole` (a closed session is a no-op). The result is ignored,
/// like the Unix `TIOCSWINSZ` ioctl's. A fabricated/closed key is a no-op.
///
/// Two lanes, one lock EACH. A session WE spawned owns its HPCON and resizes
/// through `ResizePseudoConsole` under the `hpc` lock. An ADOPTED session
/// ([`adopt_handoff`]) has no HPCON — conhost kept the pseudoconsole — so it
/// writes the same packet `ResizePseudoConsole` would have written, straight to
/// the signal pipe conhost handed us, under the `signal` lock. Each branch is
/// serialized against the waiter's teardown by the lock that owns ITS handle;
/// see [`WinSession::signal`] for why the adopted branch cannot borrow the
/// `hpc` guard's protection.
pub fn resize(master: i32, rows: u16, cols: u16) {
    resize_with_cell_px(master, rows, cols, None);
}

/// Whether `master` is a LIVE ConPTY session — one this process spawned
/// through `CreatePseudoConsole` or adopted through the DefTerm handoff. Both
/// are conhost-backed, and conhost repaints the whole viewport after every
/// `ResizePseudoConsole` (row-0-anchored, from its own buffer — measured
/// 2026-09-22, see `aterm_grid::grid::reflow::ResizePolicy`), so the frontend
/// picks its resize seam policy from THIS answer rather than from
/// `cfg(windows)`: a stub or replay session on Windows (a fabricated key, a
/// closed one) has no conhost behind it and keeps the native accounting.
/// A closed session answers `false` — nothing resizes it afterwards.
#[must_use]
pub fn backend_is_conpty(master: i32) -> bool {
    session(master).is_some()
}

/// Unix-seam parity for `aterm_pty::size`: ConPTY has no winsize to read back,
/// so there is nothing to verify and the answer is `None`.
#[must_use]
pub fn size(_master: i32) -> Option<(u16, u16)> {
    None
}

/// Make `master`'s console host LEAVE its alternate screen buffer: start a
/// one-shot console client in the same pseudoconsole that writes `CSI ? 1049 l`
/// and exits. The recovery for a full-screen app that died on the alternate
/// screen: conhost keeps that buffer up with nobody behind it (nothing a
/// terminal writes into the input pipe switches it; a console client's
/// `?1049l` does), and an OpenConsole host then forwards the switch to the
/// terminal verbatim ([`ConsoleHostKind::passes_screen_switches_through`]), so
/// the terminal leaves its own alternate screen from the same bytes, in the
/// same place in the stream, and the two stay in step. Returns once the client
/// has been started; the switch arrives on the session's output.
///
/// The client is `aterm.exe --conpty-leave-alt-screen` ([`console_guard`]),
/// which writes to the console's screen and nothing else: the console's input
/// mode is shared by every client of the tab, and the program the user still
/// runs there (an ssh session, wsl.exe, PSReadLine) set it. `cmd.exe /c set
/// /p`, the first client, turned processed, line and echo input back
/// on as it started (a raw 0x288 read 0x28f after it), so Ctrl+C at an ssh
/// prompt became a CTRL_C_EVENT that ended ssh.
///
/// # Errors
/// `NotFound` for a key no live session has; `Unsupported` for an inbox
/// session (its host repaints over the terminal instead — the frontend keeps
/// its own recovery there) and for a process with no `aterm.exe` beside it to
/// run the client (the screen then stays up on both sides, in step); the OS
/// error when the client cannot be started.
pub fn resync_alt_screen(master: i32) -> io::Result<()> {
    resync_alt_screen_with(master, None)
}

/// [`resync_alt_screen`], and after the switch the client writes `carry` (the
/// prompt the shell drew on the dead buffer, aterm-core's
/// `Terminal::dead_alt_screen_carry`) when the host's cursor is still `cursor`
/// (0-based row and column) and the shell reads raw; otherwise the plain leave.
/// One write, so the shell's line editor, anchored where it drew its prompt,
/// finds that prompt where it left it on the primary screen.
///
/// # Errors
/// As [`resync_alt_screen`].
pub fn resync_alt_screen_carrying(master: i32, cursor: (u16, u16), carry: &str) -> io::Result<()> {
    resync_alt_screen_with(master, Some((cursor, carry)))
}

fn resync_alt_screen_with(master: i32, carry: Option<((u16, u16), &str)>) -> io::Result<()> {
    let Some(s) = session(master) else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such session"));
    };
    if !s.host.passes_screen_switches_through() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the session's console host repaints over the terminal; nothing to resync",
        ));
    }
    let Some(mut command) = console_guard::leave_alt_screen_command(carry) else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no aterm.exe beside this program to run the console host's leave",
        ));
    };
    // Held across the client's CreateProcessW: the close path swaps the signal
    // (then the reference) to 0 under these locks, so the slots stay open for
    // as long as kernelbase reads them.
    let signal = s.signal.lock_or_recover();
    let reference = s.reference.lock_or_recover();
    if *signal == 0 || *reference == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the session's pseudoconsole is closing",
        ));
    }
    let mut slots = [*signal, *reference, s.conhost];
    openconsole::start_client(&mut slots, &mut command).map(drop)
}

/// Which console host serves `master` — `None` for a key no live session has
/// (the same answer [`backend_is_conpty`] gives `false` for). The frontend
/// keys the workarounds the two hosts need differently on this
/// ([`ConsoleHostKind`] lists which do and which do not).
#[must_use]
pub fn conpty_host(master: i32) -> Option<ConsoleHostKind> {
    session(master).map(|s| s.host)
}

/// Unix-seam parity for [`resize_with_cell_px`](crate::resize_with_cell_px):
/// ConPTY resizes in CHARACTER cells (`COORD`) and has no winsize pixel fields,
/// so `cell_px` has nowhere to land and is ignored. Kept so the frontend's one
/// call shape compiles on both platforms.
pub fn resize_with_cell_px(master: i32, rows: u16, cols: u16, _cell_px: Option<(u16, u16)>) {
    let Some(s) = session(master) else {
        return;
    };
    let size = coord(cols, rows);
    let g = s.hpc.lock_or_recover();
    if *g != 0 {
        // SAFETY: the HPCON is live while the lock is held (the waiter swaps it
        // to 0 under this same lock before closing).
        if unsafe { ffi::ResizePseudoConsole(*g, size) } >= 0
            && let Some(arming) = s.resize_guard.note_resize(size.y, size.x)
        {
            schedule_nudge(master, arming);
        }
        return;
    }
    // Adopted and OpenConsole lane. The `signal` MUTEX — not the `hpc` guard
    // `g` — is what keeps this write off a closed handle: `close_pseudoconsole`
    // swaps the handle to 0 under this same lock and closes it outside, so we
    // either hold a live handle for the whole WriteFile or read the 0 that says
    // it is gone. `g` is still in scope only to keep the lock order hpc →
    // signal uniform (nothing anywhere takes `hpc` while holding `signal`, so
    // there is no inversion).
    let sig = s.signal.lock_or_recover();
    if *sig != 0 {
        let buf = encode_resize_signal(rows, cols);
        // A 6-byte write to a signal pipe never blocks meaningfully (conhost's
        // PtySignalInputThread is a dedicated reader on a 64 KiB pipe), so this
        // is safe on the UI thread. Failure is ignored exactly like the HPCON
        // branch's return code and the Unix `TIOCSWINSZ`'s: a dead conhost just
        // means the session is already going away.
        // SAFETY: `*sig` is a live pipe handle for as long as this guard is held.
        let sent = unsafe {
            ffi::WriteFile(
                *sig,
                buf.as_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if sent != 0
            && let Some(arming) = s.resize_guard.note_resize(size.y, size.x)
        {
            schedule_nudge(master, arming);
        }
    }
}

/// How long after a size change the [`resize_guard`]'s record waits for an
/// input write to carry it before it goes by itself (`resize_guard`, THE
/// STALL): long enough for the console host to have queued the size event
/// behind which the record must land, and for an input that follows at once
/// (a key, the in-band size report a full-screen program asked for) to carry
/// it the usual way.
const NUDGE_AFTER: std::time::Duration = std::time::Duration::from_millis(50);

/// Have the guard nudge for `arming` ([`resize_guard::ResizeGuard::nudge`])
/// [`NUDGE_AFTER`] from now, on a thread of its own: the call runs on the
/// resizing thread (the UI's), and a nudge may write to the input pipe or wait
/// for the OpenConsole client. A newer size change makes it a no-op. A thread
/// that cannot be started leaves the record to the next input, as before.
fn schedule_nudge(master: i32, arming: u64) {
    let _ = std::thread::Builder::new()
        .name("aterm-conpty-nudge".into())
        .spawn(move || {
            std::thread::sleep(NUDGE_AFTER);
            let Some(s) = session(master) else {
                return;
            };
            // A failed write leaves the record owed to the next input.
            let _ = s.resize_guard.nudge(
                arming,
                || console_guard::runtime_in_tab(&s),
                |rows, cols| console_guard::guard(&s, rows, cols),
                |b| write_input(&s, b, None),
            );
        });
}

/// Focus-linked QoS boost — the Windows Terminal lesson, public-API edition.
/// While a window is focused its shell must never lose the CPU to background
/// NORMAL-priority load: that is exactly the "laggy but smooth" ConPTY
/// starvation (conhost + the shell's line editor are two starvable middlemen
/// between every keystroke and its echo). `on = true` raises the shell ROOT
/// process and its conhost to ABOVE_NORMAL and claims power throttling
/// (EcoQoS) OFF; `on = false` restores NORMAL + system-managed throttling.
/// Root-only by design: children the shell spawns (builds) start at NORMAL
/// either way (a child inherits only IDLE/BELOW_NORMAL classes), so a boosted
/// shell can never starve the terminal itself. Failures are ignored like the
/// Unix `TIOCSWINSZ`'s (a dead child's handle simply no-ops); a
/// fabricated/closed key is a no-op. Cheap non-blocking syscalls — safe on
/// the UI thread. A development build's `ATERM_TRACE_BOOST` seam
/// ([`aterm_types::dev_seam!`]) traces each application to stderr.
pub fn set_focus_boost(master: i32, on: bool) {
    let Some(s) = session(master) else {
        return;
    };
    for h in [s.process, s.conhost] {
        if h == 0 {
            continue;
        }
        let class = if on {
            ffi::ABOVE_NORMAL_PRIORITY_CLASS
        } else {
            ffi::NORMAL_PRIORITY_CLASS
        };
        // SAFETY: the registry Arc keeps both handles open across the calls.
        let prio_ok = unsafe { ffi::SetPriorityClass(h, class) };
        let mut st = ffi::PROCESS_POWER_THROTTLING_STATE {
            Version: ffi::PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            // Focused: claim the EXECUTION_SPEED bit with a clear state bit —
            // "never EcoQoS-throttle this process". Blurred: release the
            // claim entirely (ControlMask 0) — back to system-managed.
            ControlMask: if on {
                ffi::PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            } else {
                0
            },
            StateMask: 0,
        };
        // SAFETY: valid handle + fully-initialized POD payload of stated size.
        let qos_ok = unsafe {
            ffi::SetProcessInformation(
                h,
                ffi::PROCESS_POWER_THROTTLING_CLASS,
                std::ptr::addr_of_mut!(st).cast::<c_void>(),
                std::mem::size_of::<ffi::PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if aterm_types::dev_seam!("ATERM_TRACE_BOOST").is_some() {
            let which = if h == s.process { "shell" } else { "conhost" };
            // Best-effort: `eprintln!` PANICS when stderr is closed or its reader is
            // gone, and this trace runs on the spawn path inside the GUI process.
            {
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "BOOST master={master} on={on} target={which} prio_ok={prio_ok} qos_ok={qos_ok}"
                );
            }
        }
    }
}

/// Locale overrides for spawned children — Windows stub returning NO overrides
/// (identical signature, so `aterm-gui`'s call site compiles unchanged).
/// POSIX locale categories mean nothing to pwsh/cmd/native children, ConPTY's
/// VT stream to us is UTF-8 regardless, and injecting `LC_CTYPE=C.UTF-8` could
/// only confuse MSYS children. The Unix implementation (and its 64-shape
/// conformance suite) lives in `src/unix.rs`, untouched.
#[must_use]
pub fn resolve_spawn_locale(
    _lc_all: Option<&str>,
    _lc_ctype: Option<&str>,
    _lang: Option<&str>,
) -> Vec<(String, String)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fail-closed spawn gates (SEC-2 parity, ported from the Unix suite) ----

    // An under-tier `Cap<Spawn>` must be rejected BEFORE any process exists —
    // the Windows twin of `under_tier_spawn_cap_is_denied_before_forking`.
    #[test]
    fn under_tier_spawn_cap_is_denied_before_any_process() {
        // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let weak_spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Untrusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let err = spawn_shell(
            24,
            80,
            &weak_spawn,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect_err("an under-tier spawn cap must be denied, not spawn a shell");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    // SEC-2: a weak `Cap<Sandbox>` must never yield a shell. Unix fails in the
    // child (exit-before-exec, _exit(126)); Windows must fail BEFORE
    // CreateProcessW — no process, no registry entry, "fail-closed" named.
    #[test]
    fn under_tier_sandbox_cap_is_denied_fail_closed_no_session() {
        // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);
        let sessions_before = SESSIONS.lock_or_recover().len();
        let err = spawn_shell(
            24,
            80,
            &spawn_cap,
            &weak_sandbox,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect_err("a weak sandbox cap must fail closed, not spawn a shell");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "weak sandbox cap must be PermissionDenied, got: {err}"
        );
        assert!(
            err.to_string().contains("fail-closed"),
            "error must describe the fail-closed refusal: {err}"
        );
        assert_eq!(
            SESSIONS.lock_or_recover().len(),
            sessions_before,
            "no session may be registered for a denied spawn"
        );
    }

    // A demanded OS-sandbox wrap has no Windows actuator: refuse (Unsupported),
    // never silently spawn unsandboxed.
    #[test]
    fn sandbox_wrap_demand_fails_closed_unsupported() {
        // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let err = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            Some("(deny network*)"),
        )
        .expect_err("a demanded sandbox wrap must refuse on Windows, not spawn unsandboxed");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(
            err.to_string().contains("fail-closed"),
            "error must describe the fail-closed refusal: {err}"
        );
    }

    // A configured shell holding a NUL refuses the spawn, before any process
    // exists, instead of running the program the part before the NUL names.
    // `cmd.exe\0x` is the sharp case: `SearchPathW` reads only `cmd.exe` and
    // resolves it, so neither the program nor argv holds a NUL by the time a
    // check after resolution could look. The Windows twin of
    // `unix_tests.rs`'s `a_nul_in_an_argument_or_the_shell_refuses_the_spawn`.
    #[test]
    fn a_shell_holding_a_nul_refuses_the_spawn() {
        // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let args = ["/c".to_string(), "exit".to_string()];
        let over = ["cmd.exe".to_string(), "/c".to_string(), "exit".to_string()];
        for (what, shell_args, argv_override) in [
            ("argv_override", None, Some(&over[..])),
            ("shell_args", Some(&args[..]), None),
            ("bare shell", None, None),
        ] {
            let err = spawn_shell(
                24,
                80,
                &spawn_cap,
                &sandbox_cap,
                &[],
                Some("cmd.exe\0x"),
                shell_args,
                argv_override,
                None,
                None,
                None,
            )
            .expect_err("a shell holding a NUL must refuse, not run another program");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{what}: {err}");
            assert!(
                err.to_string().contains("the shell (set by --shell"),
                "{what}: the refusal names the shell: {err}"
            );
        }
    }

    // ---- DefTerm inbound handoff: adopt_handoff + the signal-pipe resize ----

    /// Access rights the adoption path actually depends on. `SYNCHRONIZE` is
    /// the waiter's (it blocks on the client); `PROCESS_QUERY_LIMITED_INFORMATION`
    /// is `GetProcessId`'s. We do NOT get to choose these in production — the
    /// handoff hands us whatever conhost duplicated — which is exactly why the
    /// pid is treated as optional.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;

    /// `CREATE_NO_WINDOW` — the helper below must not flash a console.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// A REAL, DISTINCT client process for the adopted-session tests.
    ///
    /// Why not our own process (which is what these tests used to open): every
    /// such session records the TEST PROCESS's pid, so two of them collide on
    /// the one key `hangup`/`reap`/`exit_code` have — and the suite really did
    /// go red about half the time because one test's `hangup` reached the other
    /// test's session. A test whose subject is a pid-keyed operation needs a pid
    /// of its own; anything else is testing the scheduler.
    ///
    /// `sort.exe` (System32, present on every Windows) blocks in `ReadFile` on
    /// stdin until EOF. We hand it a pipe and never write to it, so its lifetime
    /// is EXACTLY ours: it cannot exit early and make a teardown assertion pass
    /// for the wrong reason, and it dies on `Drop`.
    struct TestClient(std::process::Child);

    impl TestClient {
        fn spawn() -> Self {
            use std::os::windows::process::CommandExt;
            let exe = std::env::var_os("SystemRoot")
                .map(|r| std::path::Path::new(&r).join("System32").join("sort.exe"))
                .unwrap_or_else(|| std::path::PathBuf::from("sort.exe"));
            #[expect(
                clippy::disallowed_methods,
                reason = "aterm-pty links no aterm-uds on Windows (it calls kernel32 itself); System32's sort.exe as a windowless test client"
            )]
            let child = std::process::Command::new(exe)
                .stdin(std::process::Stdio::piped()) // held open ⇒ it blocks
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .expect("spawning the System32 test client must succeed");
            Self(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }

        /// A handle with exactly the rights the adoption path needs, fresh each
        /// call so the session can own and close it.
        fn open(&self) -> isize {
            // SAFETY: OpenProcess against a live child we are holding.
            let h = unsafe {
                ffi::OpenProcess(
                    SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    self.0.id(),
                )
            };
            assert_ne!(h, 0, "OpenProcess on the test client must succeed");
            h
        }

        /// Still running? Guards teardown assertions against the vacuous pass
        /// where the waiter woke because the CLIENT exited, not because of us.
        fn is_alive(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }
    }

    impl Drop for TestClient {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // The 6-byte ConPTY resize packet, pinned against the shipping
    // kernelbase!ResizePseudoConsole (Windows 11 26200), which builds exactly
    // this buffer and WriteFile()s 6 bytes of it to the signal pipe:
    //   lea r8d,[rbx+8] ; mov [rsp+0x30],r8w   -> u16 opcode 8   at offset 0
    //   mov [rsp+0x32],dx                      -> u16 COORD.X    at offset 2  (COLUMNS)
    //   mov [rsp+0x34],ax                      -> u16 COORD.Y    at offset 4  (ROWS)
    //   lea r8d,[rbx+6]                        -> 6 bytes written
    // The column-before-row order is the whole point: every other function in
    // this module takes (rows, cols).
    #[test]
    fn resize_signal_packet_is_opcode_then_columns_then_rows() {
        let buf = encode_resize_signal(24, 80);
        assert_eq!(buf.len(), 6, "ConPTY's resize signal is exactly 6 bytes");
        assert_eq!(
            &buf[0..2],
            &8u16.to_le_bytes(),
            "opcode must be PTY_SIGNAL_RESIZE_WINDOW (8), little-endian"
        );
        assert_eq!(
            &buf[2..4],
            &80u16.to_le_bytes(),
            "offset 2 is COLUMNS (COORD.X) — not rows"
        );
        assert_eq!(
            &buf[4..6],
            &24u16.to_le_bytes(),
            "offset 4 is ROWS (COORD.Y)"
        );
        // Whole-buffer form, so a reordering that happens to keep both halves
        // individually plausible still fails.
        assert_eq!(buf, [0x08, 0x00, 0x50, 0x00, 0x18, 0x00]);
    }

    // A non-square size cannot be symmetric, so a transposed encoder is caught
    // even if the constants above were themselves wrong.
    #[test]
    fn resize_signal_is_not_transposed() {
        let buf = encode_resize_signal(1, 999);
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 999, "columns");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), 1, "rows");
    }

    // Degenerate sizes must clamp into ConPTY's POSITIVE i16 COORD range: a raw
    // 0 or 40000 reaching conhost is a zero/negative viewport. `coord` already
    // clamps the HPCON path; the signal path must not bypass it.
    #[test]
    fn resize_signal_clamps_into_positive_coord_range() {
        let zero = encode_resize_signal(0, 0);
        assert_eq!(
            u16::from_le_bytes([zero[2], zero[3]]),
            1,
            "0 cols clamps up to 1"
        );
        assert_eq!(
            u16::from_le_bytes([zero[4], zero[5]]),
            1,
            "0 rows clamps up to 1"
        );

        let huge = encode_resize_signal(u16::MAX, u16::MAX);
        let cols = u16::from_le_bytes([huge[2], huge[3]]);
        let rows = u16::from_le_bytes([huge[4], huge[5]]);
        assert_eq!(cols, 32767, "columns clamp to i16::MAX");
        assert_eq!(rows, 32767, "rows clamp to i16::MAX");
        assert!(
            cols as i16 > 0 && rows as i16 > 0,
            "a clamped COORD must stay positive when reinterpreted as i16"
        );
    }

    /// Run Windows PowerShell 5.1 in a real ConPTY as a reader that reports
    /// key UPs too (`RawUI.ReadKey` with `IncludeKeyUp`), hand the master to
    /// `drive` once it is reading, and return its report: every key it read,
    /// as `Vk/KeyDown` joined by `;`, up to and including a `q` key-down.
    /// Windows PowerShell because it ships with every Windows this seam runs on.
    fn keys_read_through_conpty(drive: impl FnOnce(i32)) -> String {
        keys_read_through_conpty_after("", drive)
    }

    /// [`keys_read_through_conpty`], the reader first running `prelude` (a
    /// PowerShell statement list) in the tab.
    fn keys_read_through_conpty_after(prelude: &str, drive: impl FnOnce(i32)) -> String {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        // SAFETY: test-process mint; the trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let script = format!(
            "{prelude}$ui = $Host.UI.RawUI; [Console]::Out.Write('READY'); $keys = @(); \
             do {{ $k = $ui.ReadKey('NoEcho,IncludeKeyDown,IncludeKeyUp'); \
             $keys += ('{{0}}/{{1}}' -f $k.VirtualKeyCode, $k.KeyDown) }} \
             until ($k.VirtualKeyCode -eq 81 -and $k.KeyDown); \
             [Console]::Out.Write('KEYS ' + ($keys -join ';') + ' END')"
        );
        let shell = spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&[
                "powershell.exe".into(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-Command".into(),
                script.into(),
            ]),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("ConPTY spawn of powershell.exe must succeed");
        let master = shell.master;
        // The reader goes through `read`, which is where the seam watches for
        // conhost's win32-input-mode request.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = read(master, &mut buf);
                if n <= 0 || tx.send(buf[..n as usize].to_vec()).is_err() {
                    return;
                }
            }
        });
        // conhost RENDERS the child's text, so strip its CSI/OSC framing before
        // looking for a marker it may have painted in pieces.
        fn text_of(stream: &[u8]) -> String {
            let mut text = Vec::new();
            let mut i = 0;
            while i < stream.len() {
                match (stream[i], stream.get(i + 1)) {
                    (0x1b, Some(b'[')) => {
                        i += 2;
                        while i < stream.len() && !(0x40..=0x7e).contains(&stream[i]) {
                            i += 1;
                        }
                    }
                    (0x1b, Some(b']')) => {
                        i += 2;
                        while i < stream.len() && !matches!(stream[i], 0x07 | 0x1b) {
                            i += 1;
                        }
                        // ST is two bytes; the loop's step skips the second.
                        if stream.get(i) == Some(&0x1b) {
                            i += 1;
                        }
                    }
                    (b, _) => text.push(b),
                }
                i += 1;
            }
            String::from_utf8_lossy(&text).into_owned()
        }
        let mut seen = Vec::new();
        let mut wait_for = |marker: &str| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !text_of(&seen).contains(marker) {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(chunk) => seen.extend_from_slice(&chunk),
                    Err(_) => {
                        hangup(shell.pid);
                        panic!("no {marker:?} from powershell: {:?}", text_of(&seen));
                    }
                }
            }
            text_of(&seen)
        };
        wait_for("READY");
        drive(master);
        let out = wait_for(" END");
        hangup(shell.pid);
        out.split("KEYS ")
            .nth(1)
            .and_then(|tail| tail.split(" END").next())
            .expect("the KEYS report")
            .to_string()
    }

    // THE RESIZE GUARD, end to end on a real ConPTY: an input before any
    // resize arrives untouched, and the first write after a size change
    // arrives behind exactly one Shift key-up — the record the MSYS2 runtime
    // may drop in place of the user's first key (see `resize_guard`).
    #[test]
    fn the_first_write_after_a_resize_reaches_the_reader_behind_one_shift_key_up() {
        let keys = keys_read_through_conpty(|master| {
            write_all(master, b"a");
            resize(master, 25, 80);
            write_all(master, b"xq");
        });
        assert_eq!(
            keys, "65/True;65/False;16/False;88/True;88/False;81/True",
            "`a` before the resize arrives as typed; `xq` after it arrives behind \
             one Shift key-up and nothing else"
        );
    }

    // REPLIES, end to end: a write inside `as_reply` after a size change (here
    // a plain `x` standing in for an answer, which conhost would otherwise
    // take for itself) reaches the reader with no record before it, and the
    // guard is still owed: the next input write, `q`, carries it.
    #[test]
    fn a_reply_after_a_resize_passes_the_guard_and_the_next_input_carries_it() {
        let keys = keys_read_through_conpty(|master| {
            write_all(master, b"a");
            resize(master, 25, 80);
            as_reply(|| write_all(master, b"x"));
            write_all(master, b"q");
        });
        assert_eq!(
            keys, "65/True;65/False;88/True;88/False;16/False;81/True",
            "the reply `x` arrives as written; the Shift key-up rides the next input"
        );
    }

    // The reply mark covers exactly the closure: set inside it, kept by a
    // nested call until the outer one returns, and lifted however the closure
    // ends, a panic included, so a reply writer that unwinds cannot leave
    // its thread's later input unguarded.
    #[test]
    fn the_reply_mark_covers_the_closure_and_no_more() {
        let marked = || REPLYING.with(std::cell::Cell::get);
        assert!(!marked());
        assert_eq!(
            as_reply(|| {
                let inner = as_reply(marked);
                (inner, marked())
            }),
            (true, true)
        );
        assert!(!marked());
        let unwound = std::panic::catch_unwind(|| as_reply(|| panic!("a reply writer unwinds")));
        assert!(unwound.is_err());
        assert!(!marked(), "lifted on unwind");
        let other = as_reply(|| std::thread::spawn(marked).join().expect("thread"));
        assert!(!other, "another thread's writes stay input");
    }

    // THE SWITCH, end to end: once the guard's record has gone to conhost, its
    // parser holds a read that ends in a lone ESC (measured: the ESC then
    // joined the next key as Alt+Q), so a raw lone ESC written after it must
    // reach the reader as the Escape key — here behind the second guard
    // record, and with a pause before `q` so conhost reads the ESC alone.
    #[test]
    fn a_lone_esc_after_the_guard_record_reaches_the_reader_as_escape() {
        let keys = keys_read_through_conpty(|master| {
            write_all(master, b"a");
            resize(master, 25, 80);
            write_all(master, b"x");
            write_all(master, b"\x1b");
            std::thread::sleep(std::time::Duration::from_millis(500));
            write_all(master, b"q");
        });
        assert_eq!(
            keys, "65/True;65/False;16/False;88/True;88/False;16/False;27/True;27/False;81/True",
            "after the switch a lone ESC is Escape (27), not held into Alt+Q"
        );
    }

    /// A stand-in for a program of the MSYS2 runtime, which the tab probe
    /// recognises by its DLL: a copy of cmd.exe beside an `msys-2.0.dll`,
    /// under a folder of its own named for `what`. Returns the folder (for the
    /// test to remove) and a PowerShell prelude that leaves it idling in the
    /// tab for a minute.
    fn runtime_standin(what: &str) -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!("aterm-{what}-{}", std::process::id()));
        let bin = root.join("usr").join("bin");
        std::fs::create_dir_all(&bin).expect("stand-in dir");
        let system = std::env::var_os("SystemRoot").expect("SystemRoot");
        let cmd = std::path::Path::new(&system)
            .join("System32")
            .join("cmd.exe");
        std::fs::copy(&cmd, bin.join("cmd.exe")).expect("copy cmd.exe");
        std::fs::write(bin.join("msys-2.0.dll"), b"").expect("stand-in runtime");
        let prelude = format!(
            "Start-Process -NoNewWindow -FilePath '{}' -ArgumentList '/d /c ping -n 60 127.0.0.1 >nul'; ",
            bin.join("cmd.exe").display()
        );
        (root, prelude)
    }

    // THE STALL, end to end: in a tab with a program of the MSYS2 runtime, a
    // size change that no input follows still gets the guard's record, by
    // itself, once conhost has had time to queue the size event: the reader
    // gets a Shift key-up that nobody typed, and the next input carries the
    // second record the resize owes. Without it the runtime that dropped the
    // record after the size event waited for the next key, and vim redrew only
    // when one came.
    #[test]
    fn a_resize_no_input_follows_sends_the_record_by_itself() {
        use std::time::{Duration, Instant};
        let (root, prelude) = runtime_standin("nudge");
        let keys = keys_read_through_conpty_after(&prelude, |master| {
            write_all(master, b"a");
            resize(master, 25, 80);
            let s = session(master).expect("the live session");
            let deadline = Instant::now() + Duration::from_secs(60);
            while s.resize_guard.records_owed() == 2 {
                assert!(Instant::now() < deadline, "no record went by itself");
                std::thread::sleep(Duration::from_millis(10));
            }
            write_all(master, b"q");
        });
        assert_eq!(
            keys, "65/True;65/False;16/False;16/False;81/True",
            "one Shift key-up by itself, then `q` behind the second"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ASTRAL CHARACTERS, end to end: in a tab with a program of the MSYS2
    // runtime in its job, an emoji written as UTF-8 reaches the reader as its
    // surrogate pair's records, both key-downs and then both key-ups with no
    // virtual key, the layout that runtime pairs (see `resize_guard`); what
    // conhost makes of the text by itself loses the high surrogate on the way
    // to it. The runtime is recognised by its DLL, so a copy of cmd.exe beside
    // an `msys-2.0.dll`, idling in the tab, stands in for one; with no such
    // program the emoji goes as the text it is.
    #[test]
    fn an_astral_character_reaches_the_reader_as_its_surrogate_records() {
        const RECORDS: &str = "0/True;0/True;0/False;0/False;81/True";
        let (root, prelude) = runtime_standin("astral");
        let typed = |master| {
            write_all(master, "😀".as_bytes());
            write_all(master, b"q");
        };
        let keys = keys_read_through_conpty_after(&prelude, typed);
        assert_eq!(keys, RECORDS, "a runtime in the tab: the records");
        let plain = keys_read_through_conpty(typed);
        assert_ne!(plain, RECORDS, "no runtime in the tab: the text, as typed");
        assert!(plain.ends_with("81/True"), "{plain}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // adopt_handoff must REFUSE a handoff with no client process handle rather
    // than registering a session whose EOF can never fire (the waiter keys on
    // exactly that handle) — a tab that could never close.
    #[test]
    fn adopt_handoff_without_a_client_process_is_refused_and_registers_nothing() {
        let before = SESSIONS.lock_or_recover().len();
        let err = adopt_handoff(0, 0, 0, 0)
            .expect_err("a handoff with no client process handle must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            SESSIONS.lock_or_recover().len(),
            before,
            "a refused handoff must not register a session"
        );
    }

    // ---- backend_is_conpty: the frontend's resize-policy gate ----

    // The gate answers from the REGISTRY, never from the platform: a key no
    // session was registered under (a GUI stub, a fabricated key) is not
    // ConPTY even on Windows, a registered session is — the adopted lane
    // included, since conhost repaints an adopted viewport exactly like a
    // spawned one — and a closed session stops being one the moment
    // `close_master` unregisters it. Registered the adopted way (a real,
    // waitable handle to our own process + a real pipe) so no shell has to be
    // spawned: what is under test is the registry answer, not conhost.
    #[test]
    fn backend_is_conpty_follows_the_registry() {
        assert!(!backend_is_conpty(-1), "a negative key is nobody's session");
        assert!(
            !backend_is_conpty(0x7fff_ffff),
            "a fabricated key is not a session"
        );

        // SAFETY: OpenProcess on our own pid yields a real, closable handle.
        let me = unsafe {
            ffi::OpenProcess(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                ffi::GetCurrentProcessId(),
            )
        };
        assert_ne!(me, 0, "OpenProcess on self must succeed");
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default attrs, default buffer size.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
            0,
            "CreatePipe must succeed"
        );
        let sh =
            adopt_handoff(0, 0, wr, me).expect("adopt_handoff must accept a real client handle");
        assert!(
            backend_is_conpty(sh.master),
            "a registered (adopted) session is a ConPTY backend"
        );
        close_master(sh.master);
        assert!(
            !backend_is_conpty(sh.master),
            "a closed session is unregistered and no longer answers ConPTY"
        );
        // SAFETY: the read end is still ours (the session never owned it).
        unsafe { ffi::CloseHandle(rd) };
    }

    // Construction shape: an adopted session must carry hpc == 0 (conhost owns
    // the pseudoconsole — we must never call ClosePseudoConsole on it), no job
    // (we did not create the client and must never sweep it), and the signal
    // pipe we were handed. Uses a real, waitable process handle (our own,
    // duplicated) so the waiter has something legitimate to block on.
    #[test]
    fn adopted_session_has_no_hpcon_no_job_and_keeps_the_signal_pipe() {
        // A real kernel handle to stand in for the "client process": our own
        // process, opened fresh so the session's Drop can own and close it.
        // SYNCHRONIZE is what the waiter needs; QUERY_LIMITED_INFORMATION is
        // what `GetProcessId` needs (see the pid-tolerance test below).
        // SAFETY: OpenProcess on our own pid yields a real, closable handle.
        let me = unsafe {
            ffi::OpenProcess(
                SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                ffi::GetCurrentProcessId(),
            )
        };
        assert_ne!(me, 0, "OpenProcess on self must succeed");
        // A real pipe to stand in for the signal pipe, so the handle the session
        // takes ownership of is genuinely closable.
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default attrs, default buffer size.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
            0,
            "CreatePipe must succeed"
        );

        let sh =
            adopt_handoff(0, 0, wr, me).expect("adopt_handoff must accept a real client handle");
        let s = session(sh.master).expect("the adopted session must be registered");
        assert_eq!(
            *s.hpc.lock_or_recover(),
            0,
            "an adopted session must hold NO HPCON — conhost owns the pseudoconsole"
        );
        assert_eq!(
            *s.signal.lock_or_recover(),
            wr,
            "an adopted session must keep the signal pipe it was handed"
        );
        assert_eq!(
            s.job, 0,
            "an adopted session must own no job (never sweep someone else's process)"
        );
        assert_eq!(
            s.conhost, 0,
            "no host handle is plumbed through this signature"
        );
        assert_eq!(
            s.pid,
            unsafe { ffi::GetCurrentProcessId() },
            "the pid must be derived from the client process HANDLE"
        );

        // resize() on the adopted lane must take the signal-pipe branch and not
        // panic / not touch a null HPCON. Read it back off the pipe to prove the
        // exact bytes reached the wire.
        resize(sh.master, 24, 80);
        let mut buf = [0u8; 6];
        let mut got: u32 = 0;
        // SAFETY: `rd` is the live read end; buffer is sized for the 6-byte packet.
        let ok = unsafe { ffi::ReadFile(rd, buf.as_mut_ptr(), 6, &mut got, std::ptr::null_mut()) };
        assert_ne!(
            ok, 0,
            "the resize packet must have been written to the signal pipe"
        );
        assert_eq!(got, 6, "exactly 6 bytes");
        assert_eq!(
            buf,
            [0x08, 0x00, 0x50, 0x00, 0x18, 0x00],
            "resize() must put opcode/cols/rows on the signal pipe verbatim"
        );

        // The waiter must still be BLOCKED, not already torn down. This is the
        // deterministic half of the test: `adopt_handoff` must hand the waiter a
        // real close event, because `WaitForMultipleObjects` over an array
        // containing a 0 handle returns WAIT_FAILED IMMEDIATELY — which would
        // drop the waiter straight through to its teardown and close the signal
        // pipe the instant the session was created. Without this loop the test
        // above merely races the waiter's thread start and usually wins.
        for _ in 0..50 {
            assert_ne!(
                *s.signal.lock_or_recover(),
                0,
                "the waiter tore the adopted session down while its client was still alive"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // Teardown: close_master drops the session, closing the signal pipe (and
        // the duplicated process handle) — no ClosePseudoConsole is reachable.
        close_master(sh.master);
        assert!(
            session(sh.master).is_none(),
            "close_master must unregister the adopted session"
        );
        // SAFETY: the read end is still ours (the session never owned it).
        unsafe { ffi::CloseHandle(rd) };
    }

    // `hangup` must actually tear an adopted session down. It signals the close
    // event, which the waiter is blocked on; the waiter then runs the adopted
    // teardown, which closes the SIGNAL PIPE (there is no HPCON). Before this
    // lane carried a real close event, `hangup` set an invalid handle and did
    // nothing at all — so this test guards the lever, not just its plumbing.
    #[test]
    fn hangup_tears_down_an_adopted_session_through_the_signal_pipe() {
        // A client process of OUR OWN, so `sh.pid` names this session and only
        // this session no matter what else the suite is running concurrently.
        let mut client = TestClient::spawn();
        let (mut rd, mut wr): (isize, isize) = (0, 0);
        // SAFETY: two out-params, default attrs/size.
        assert_ne!(
            unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
            0
        );
        let sh = adopt_handoff(0, 0, wr, client.open()).expect("adopt");
        let s = session(sh.master).expect("registered");
        assert_eq!(
            sh.pid as u32,
            client.pid(),
            "the adopted session must key on the CLIENT's pid"
        );
        assert_ne!(
            sh.pid as u32,
            // SAFETY: no-argument pid query.
            unsafe { ffi::GetCurrentProcessId() },
            "the fixture must not borrow the test process's pid (see TestClient)"
        );

        hangup(sh.pid);

        // The waiter should wake on the close event and close the signal pipe.
        let mut closed = false;
        for _ in 0..12_000 {
            if *s.signal.lock_or_recover() == 0 {
                closed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            closed,
            "hangup must reach the waiter and close the adopted session's signal pipe"
        );
        // NON-VACUITY: the waiter has exactly two ways to wake — the client
        // exiting, or the close event. The client is still running, so it was
        // the close event, i.e. `hangup` really is the lever under test.
        assert!(
            client.is_alive(),
            "the client must still be running, or the teardown proves nothing"
        );
        close_master(sh.master);
        // SAFETY: the read end was never owned by the session.
        unsafe { ffi::CloseHandle(rd) };
    }

    /// The held gather's top-up: what a line-per-call writer left in the output
    /// pipe comes back as one batch, a full buffer leaves the rest queued, and
    /// an empty pipe answers at once instead of blocking like [`read`].
    #[test]
    fn drain_available_collects_what_the_pipe_holds_and_never_blocks() {
        let client = TestClient::spawn();
        let pipe = || {
            let (mut rd, mut wr): (isize, isize) = (0, 0);
            // SAFETY: two out-params, default attrs, a 64 KiB buffer as a
            // session's output pipe has.
            assert_ne!(
                unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 64 * 1024) },
                0,
                "CreatePipe must succeed"
            );
            (rd, wr)
        };
        let (out_rd, out_wr) = pipe();
        let (sig_rd, sig_wr) = pipe();
        // The session owns `out_rd` (its output) and `sig_wr` from here.
        let sh = adopt_handoff(0, out_rd, sig_wr, client.open())
            .expect("adopt_handoff must accept a real client handle");
        let write = |bytes: &[u8]| {
            let mut put: u32 = 0;
            // SAFETY: our own live write end; `bytes` outlives the call.
            let ok = unsafe {
                ffi::WriteFile(
                    out_wr,
                    bytes.as_ptr(),
                    u32::try_from(bytes.len()).expect("small"),
                    &mut put,
                    std::ptr::null_mut(),
                )
            };
            assert!(
                ok != 0 && put as usize == bytes.len(),
                "the write lands whole"
            );
        };
        // Empty: answered at once. On a thread, so a blocking read would be a
        // one-minute hang, not a hung suite.
        let master = sh.master;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let _ = tx.send(drain_available(master, &mut buf, 5));
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(60)),
            Ok(5),
            "an empty pipe leaves the fill as it was, without blocking"
        );
        // Three writes, as a line-per-call writer makes them: one batch.
        for line in [&b"one\r\n"[..], b"two\r\n", b"six\r\n"] {
            write(line);
        }
        let mut buf = [0u8; 64];
        buf[..2].copy_from_slice(b">>");
        let filled = drain_available(sh.master, &mut buf, 2);
        assert_eq!(&buf[..filled], b">>one\r\ntwo\r\nsix\r\n");
        // A full buffer stops the top-up; the rest stays queued for the next.
        write(&[b'x'; 100]);
        let mut small = [0u8; 40];
        assert_eq!(drain_available(sh.master, &mut small, 0), 40);
        assert_eq!(drain_available(sh.master, &mut small, 0), 40);
        assert_eq!(drain_available(sh.master, &mut small, 10), 30);
        assert_eq!(drain_available(sh.master, &mut small, 0), 0, "drained");
        // A closed key reads nothing.
        close_master(sh.master);
        assert_eq!(drain_available(sh.master, &mut small, 7), 7);
        // SAFETY: the ends the session never owned.
        unsafe {
            ffi::CloseHandle(out_wr);
            ffi::CloseHandle(sig_rd);
        }
    }

    // THE DUPLICATE-PID GUARD. A pid is not a unique key over the session
    // registry: the same console client can be handed off twice (FreeConsole →
    // AllocConsole) while the first entry is still registered, so two live
    // sessions can carry one pid. `session_by_pid`'s old first-match scan
    // resolved that to an ARBITRARY entry — HashMap order, randomized per
    // process — so closing one tab could hang up somebody ELSE's console. The
    // contract is now: an ambiguous pid resolves to NOTHING, and every pid-keyed
    // op falls back to its documented miss behavior.
    #[test]
    fn a_pid_claimed_by_two_live_sessions_resolves_to_neither() {
        // One client, adopted TWICE — exactly the production shape.
        let client = TestClient::spawn();
        let mut pipes = Vec::new();
        let mut sessions = Vec::new();
        for _ in 0..2 {
            let (mut rd, mut wr): (isize, isize) = (0, 0);
            // SAFETY: two out-params, default attrs/size.
            assert_ne!(
                unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
                0
            );
            pipes.push(rd);
            let sh = adopt_handoff(0, 0, wr, client.open()).expect("adopt");
            let s = session(sh.master).expect("registered");
            assert_eq!(sh.pid as u32, client.pid());
            sessions.push((sh, s));
        }
        let pid = sessions[0].0.pid;

        // Ambiguous ⇒ hangup must touch NEITHER session. Both signal pipes stay
        // open for the whole window; with a first-match scan one of them closes.
        hangup(pid);
        for _ in 0..50 {
            for (i, (_, s)) in sessions.iter().enumerate() {
                assert_ne!(
                    *s.signal.lock_or_recover(),
                    0,
                    "hangup on a pid two live sessions claim tore down session {i}"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Same rule for the read-only lookup: no wrong answer, no answer.
        assert_eq!(
            exit_code(pid),
            None,
            "an ambiguous pid must not report some other session's exit code"
        );
        // ...and reap must not escalate against a session it cannot identify.
        // (Both are adopted, so `job == 0` and there is nothing to terminate —
        // this asserts the lookup, and that it stays a bounded no-op.)
        reap(pid);

        // Drop one: the pid is unambiguous again, so the lever works again —
        // the refusal is scoped to the collision, not a blanket disable.
        let (sh0, _s0) = sessions.remove(0);
        close_master(sh0.master);
        let (sh1, s1) = &sessions[0];
        hangup(sh1.pid);
        let mut closed = false;
        for _ in 0..12_000 {
            if *s1.signal.lock_or_recover() == 0 {
                closed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            closed,
            "once only one session claims the pid, hangup must reach it again"
        );

        let masters: Vec<i32> = sessions.iter().map(|(sh, _)| sh.master).collect();
        drop(sessions);
        for m in masters {
            close_master(m);
        }
        for rd in pipes {
            // SAFETY: the read ends were never owned by a session.
            unsafe { ffi::CloseHandle(rd) };
        }
    }

    // The handoff failure paths that CONSUME must really close what they were
    // given, and must skip the 0 slots (a handoff can arrive with no signal
    // pipe). Proved by observation, not by inspection: the read end of a pipe
    // whose only write handle was closed reports broken-pipe IMMEDIATELY,
    // whereas a still-open write end makes the same ReadFile block forever — so
    // the read runs on a thread with a deadline and a neutered
    // `release_handoff_handles` fails the test instead of hanging it.
    #[test]
    fn release_handoff_handles_closes_the_live_slots_and_skips_the_zeros() {
        /// A hang detector, never a latency budget: a closed write end answers
        /// the read at once.
        const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        for _ in 0..2 {
            let (mut rd, mut wr): (isize, isize) = (0, 0);
            // SAFETY: two out-params, default attrs/size.
            assert_ne!(
                unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 0) },
                0
            );
            reads.push(rd);
            writes.push(wr);
        }
        // Zeros in the outer slots: position must not matter, and a 0 must not
        // be handed to CloseHandle.
        release_handoff_handles([0, writes[0], writes[1], 0]);

        for (i, rd) in reads.iter().copied().enumerate() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1];
                let mut got: u32 = 0;
                // SAFETY: `rd` is the live read end; 1-byte buffer.
                let ok = unsafe {
                    ffi::ReadFile(rd, buf.as_mut_ptr(), 1, &mut got, std::ptr::null_mut())
                };
                let _ = tx.send(ok);
            });
            let ok = rx.recv_timeout(HANG_DETECTOR).unwrap_or_else(|_| {
                panic!("slot {i}: ReadFile blocked, so its write end was never closed")
            });
            assert_eq!(ok, 0, "slot {i}: the read end must report broken pipe");
            // SAFETY: the read ends are ours alone.
            unsafe { ffi::CloseHandle(rd) };
        }
    }

    // A process handle WITHOUT query rights must still yield a usable session.
    // Found the hard way: `GetProcessId` needs PROCESS_QUERY_LIMITED_INFORMATION
    // and returns 0 without it — and we do not control the access mask conhost
    // duplicated into us. Refusing such a handoff (or, worse, panicking) would
    // drop the user's console on the floor for a purely cosmetic missing pid, so
    // the pid is optional and only the PID-KEYED lookups degrade to their
    // documented no-op-on-miss behavior. The master-keyed ops must all still work.
    #[test]
    fn adopted_session_tolerates_a_process_handle_with_no_query_rights() {
        // SAFETY: SYNCHRONIZE-only handle to our own process — enough for the
        // waiter, deliberately NOT enough for GetProcessId.
        let me = unsafe { ffi::OpenProcess(SYNCHRONIZE, 0, ffi::GetCurrentProcessId()) };
        assert_ne!(me, 0, "OpenProcess(SYNCHRONIZE) on self must succeed");

        let sh = adopt_handoff(0, 0, 0, me)
            .expect("a query-less process handle must still be adoptable");
        let s = session(sh.master).expect("the adopted session must be registered");
        assert_eq!(
            s.pid, 0,
            "GetProcessId cannot see through a SYNCHRONIZE-only handle"
        );
        assert_eq!(
            sh.pid, 0,
            "the unknown pid is reported as 0, not fabricated"
        );

        // Master-keyed ops stay well-defined; a 0 signal pipe makes resize a no-op
        // rather than a null-handle write.
        resize(sh.master, 24, 80);
        close_master(sh.master);
        assert!(
            session(sh.master).is_none(),
            "close_master must unregister it"
        );
    }

    // Registry-miss semantics for fabricated masters (gui/cli tests fabricate
    // `22`): every op is a defined error/no-op, never UB or a wrong session.
    #[test]
    fn fabricated_master_keys_get_defined_miss_semantics() {
        let mut buf = [0u8; 8];
        assert_eq!(read(22, &mut buf), -1, "read: -1 (error half of contract)");
        assert_eq!(
            write_some(22, b"x").expect_err("must err").kind(),
            io::ErrorKind::NotFound
        );
        write_all(22, b"x"); // silent no-op
        resize(22, 24, 80); // no-op
        set_focus_boost(22, true); // no-op
        set_focus_boost(22, false); // no-op
        hangup(22); // no-op (unknown pid)
        reap(22); // no-op
        assert_eq!(exit_code(22), None);
        close_master(22); // no-op
    }
}
