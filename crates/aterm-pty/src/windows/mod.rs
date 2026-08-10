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
//! Parser implications (documented, no engine change): (1) ConPTY re-renders —
//! output arrives as full-region repaints + cursor addressing rather than the
//! child's raw byte stream; the engine is a VT state machine, unaffected.
//! (2) conhost may emit private-mode requests such as win32-input-mode
//! (`CSI ? 9001 h`); the parser ignores unknown private modes and keystrokes
//! flow as plain xterm VT input written to the input pipe. (3) conhost issues
//! DSR/CPR queries; the existing reader-thread `take_response()` reply path
//! answers them with zero changes.

mod cmdline;
mod ffi;
mod shell;
mod winpath;

use std::collections::HashMap;
use std::ffi::{OsStr, c_void};
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use aterm_types::MutexExt;

use crate::SpawnedShell;
use cmdline::{build_command_line, build_env_block, wide_nul};

/// One live ConPTY session: the real HANDLEs behind an opaque `i32` master key.
/// HANDLEs are stored as `isize` (plain integers, not pointers) so the map is
/// `Send + Sync` without unsafe impls.
struct WinSession {
    /// HPCON; swapped to 0 once `ClosePseudoConsole` ran (idempotence guard).
    /// The mutex also makes UI-thread/control-thread resizes safe against the
    /// waiter's close (resize holds it across `ResizePseudoConsole`).
    hpc: Mutex<isize>,
    /// Write end of the child-stdin pipe (we `WriteFile` keystrokes here).
    input: isize,
    /// Read end of the child-stdout pipe (we `ReadFile` VT output here).
    output: isize,
    /// The child's process handle (held ⇒ the pid cannot be recycled).
    process: isize,
    /// The session's ConPTY console host (conhost.exe / OpenConsole.exe — the
    /// VT↔INPUT_RECORD middleman), opened `PROCESS_SET_INFORMATION` at spawn
    /// via a before/after child-diff around `CreatePseudoConsole`; 0 when
    /// discovery was ambiguous or failed (boost then covers the shell only).
    /// Held ONLY for the focus-boost QoS lane ([`set_focus_boost`]) — never
    /// waited on, read, or terminated through this handle.
    conhost: isize,
    /// Per-session Job Object (KILL_ON_JOB_CLOSE): last-handle close sweeps any
    /// orphaned descendant tree — slightly stronger than SIGHUP-to-pgid (a
    /// double-forked daemon survives SIGHUP but not the job); accepted and
    /// documented. Children spawned with CREATE_BREAKAWAY escape it.
    job: isize,
    /// Manual-reset event; [`hangup`] signals it and the waiter thread closes
    /// the pseudoconsole (the console-close analog of SIGHUP).
    close_evt: isize,
    /// The child's Windows process id (what [`SpawnedShell::pid`] holds).
    pid: u32,
    /// Exit code recorded by the waiter thread (or by [`reap`]).
    exit_code: Mutex<Option<u32>>,
}

impl Drop for WinSession {
    fn drop(&mut self) {
        // Close every owned handle. The job close is last-handle by construction
        // (only this struct holds it), so KILL_ON_JOB_CLOSE sweeps any orphans.
        for h in [
            self.input,
            self.output,
            self.process,
            self.conhost,
            self.job,
            self.close_evt,
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

/// Find a session by child pid (linear scan — session counts are tiny).
fn session_by_pid(pid: i32) -> Option<Arc<WinSession>> {
    if pid <= 1 {
        return None;
    }
    SESSIONS
        .lock_or_recover()
        .values()
        .find(|s| s.pid == pid as u32)
        .cloned()
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
fn close_pseudoconsole(s: &WinSession) {
    let hpc = std::mem::replace(&mut *s.hpc.lock_or_recover(), 0);
    if hpc != 0 {
        // SAFETY: the swap-to-0 makes this thread the sole closer; conhost
        // flushes pending output and breaks the out pipe (the reader's EOF).
        unsafe { ffi::ClosePseudoConsole(hpc) };
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
        if g == ffi::WAIT_TIMEOUT {
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
    fn ptr(&mut self) -> *mut c_void {
        self.buf.as_mut_ptr().cast()
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: initialized exactly once and not yet deleted.
            unsafe { ffi::DeleteProcThreadAttributeList(self.buf.as_mut_ptr().cast()) };
        }
    }
}

/// Clamp a u16 cell count into ConPTY's positive `i16` COORD range.
fn coord(cols: u16, rows: u16) -> ffi::COORD {
    ffi::COORD {
        x: cols.clamp(1, 32767) as i16,
        y: rows.clamp(1, 32767) as i16,
    }
}

/// Spawn the selected shell in a fresh ConPTY of `rows`×`cols`, returning the
/// master key. Same contract as the Unix `spawn_shell` (see the crate docs and
/// [`spawn_shell_with_pid`] for the full sequence); this thin wrapper drops the
/// pid and preserves the historical hardened default limits.
///
/// # Errors
/// See [`spawn_shell_with_pid`].
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[String]>,
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
/// profile on macOS.)
///
/// `limits` are actuated on the Job Object while the child is still SUSPENDED
/// (`aterm_sandbox::job_limits_actuated()` is `true`): the address-space, CPU,
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
    exec_command: Option<&[String]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    limits: aterm_sandbox::Limits,
) -> io::Result<SpawnedShell> {
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
    let (program, argv) =
        shell::resolve_spawn_target(shell_override, shell_args, argv_override, exec_command);
    let program_w = wide_nul(&program);
    let mut cmdline_w = build_command_line(&argv);
    let mut env_pairs = crate::build_child_env(std::env::vars_os(), env_add);
    // Refresh PATH from the LIVE registry so a tool installed after Explorer started
    // (whose dir is on the registry PATH but not on Explorer's stale process PATH) is
    // found in a freshly-opened terminal without a session restart. See `winpath`.
    winpath::refresh_child_path(&mut env_pairs);
    let mut env_block = build_env_block(&env_pairs);
    let cwd_w = resolve_cwd_wide(cwd);

    // (5) Pipes + ConPTY. Blocking anonymous pipes map 1:1 onto the existing
    // blocking read()/write contract; NO overlapped I/O in phase 1 (the reader
    // thread is dedicated and blocking today — overlapped is only needed for
    // the future set_nonblocking backpressure milestone). Advisory 64 KiB
    // buffers (the system default is 4 KiB, which caps every ReadFile at ~4 KiB
    // — 16x the wakeups/copies of the frontend's 64 KiB read buffer, and
    // matches the ~64 KiB Unix kernel PTY buffer).
    const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
    let mut in_read: isize = 0;
    let mut in_write: isize = 0;
    // SAFETY: valid out-params, NULL security attributes.
    if unsafe {
        ffi::CreatePipe(
            &mut in_read,
            &mut in_write,
            std::ptr::null_mut(),
            PIPE_BUFFER_BYTES,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let in_read = ffi::Handle::new(in_read);
    let in_write = ffi::Handle::new(in_write);
    let mut out_read: isize = 0;
    let mut out_write: isize = 0;
    // SAFETY: as above.
    if unsafe {
        ffi::CreatePipe(
            &mut out_read,
            &mut out_write,
            std::ptr::null_mut(),
            PIPE_BUFFER_BYTES,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let out_read = ffi::Handle::new(out_read);
    let out_write = ffi::Handle::new(out_write);
    // Focus-boost lane: `CreatePseudoConsole` synchronously starts the
    // session's console host as a child of THIS process. Snapshot our
    // console-host children before/after so the ONE new pid is this session's
    // conhost — the second starvable middleman the boost must cover.
    let conhost_before = conhost_children();
    let mut hpc: isize = 0;
    // SAFETY: live pipe handles for the child side; valid out-param for the HPCON.
    let hr = unsafe {
        ffi::CreatePseudoConsole(
            coord(cols, rows),
            in_read.raw(),
            out_write.raw(),
            0,
            &mut hpc,
        )
    };
    if hr < 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let pcon = PconGuard(hpc);
    let conhost = {
        let new: Vec<u32> = conhost_children()
            .into_iter()
            .filter(|p| !conhost_before.contains(p))
            .collect();
        ffi::Handle::new(match new.as_slice() {
            // SAFETY: opening OUR OWN just-spawned child with the least right
            // the boost needs; NULL (0) on failure is the "no conhost lane"
            // sentinel the guard tolerates.
            [pid] => unsafe { ffi::OpenProcess(ffi::PROCESS_SET_INFORMATION, 0, *pid) },
            // 0 or >1 new hosts (a concurrent spawn elsewhere in the process):
            // ambiguous — cover the shell only, never guess at a pid.
            _ => 0,
        })
    };
    // ConPTY duplicated its two pipe ends; drop ours now. We retain in_write
    // (keystrokes in) and out_read (VT out).
    drop(in_read);
    drop(out_write);

    // (6) Attribute list carrying the HPCON.
    let mut attr_size: usize = 0;
    // SAFETY: documented size-probe form (NULL list): fails with
    // ERROR_INSUFFICIENT_BUFFER and writes the required size.
    unsafe { ffi::InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size) };
    if attr_size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut attr = AttrList {
        buf: vec![0u64; attr_size.div_ceil(8)], // u64 elements keep it 8-aligned
        initialized: false,
    };
    // SAFETY: `attr.buf` is a writable block of >= attr_size bytes.
    if unsafe { ffi::InitializeProcThreadAttributeList(attr.ptr(), 1, 0, &mut attr_size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    attr.initialized = true;
    // SAFETY: initialized list; the attribute VALUE is the HPCON itself passed
    // by value (cast to PVOID), per the documented ConPTY sample.
    let updated = unsafe {
        ffi::UpdateProcThreadAttribute(
            attr.ptr(),
            0,
            ffi::PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
            pcon.0 as *mut c_void,
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
    // Windows pids are small multiples of 4 in practice; a DWORD above i32::MAX
    // would truncate here (theoretical, documented).
    let pid = pi.dwProcessId as i32;
    let session = Arc::new(WinSession {
        hpc: Mutex::new(pcon.release()),
        input: in_write.into_raw(),
        output: out_read.into_raw(),
        process: h_process.into_raw(),
        conhost: conhost.into_raw(),
        job: job.into_raw(),
        close_evt: close_evt.into_raw(),
        pid: pi.dwProcessId,
        exit_code: Mutex::new(None),
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
/// reader. O(1) and non-blocking (the actual close runs on the waiter thread),
/// so it is UI-thread-safe exactly like the Unix `killpg(SIGHUP)`. Best-effort
/// no-op for a non-positive/unknown pid (the ESRCH contract).
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    if let Some(s) = session_by_pid(pid) {
        // SAFETY: event handle owned by the session Arc we hold.
        unsafe { ffi::SetEvent(s.close_evt) };
    }
}

/// Reap an exited child WITHOUT ever blocking unboundedly (the Unix `reap`'s
/// poll/escalate/deadline discipline, on Windows primitives): wait the ~250 ms
/// grace; on timeout `TerminateJobObject` — the SIGKILL-the-process-GROUP
/// escalation, killing the whole descendant tree like `killpg` — then wait out
/// the rest of the ~2 s budget and record the exit code if the child signaled.
/// Runs on the frontend's detached reaper thread exactly as on Unix.
/// Best-effort; a no-op for a non-positive pid or an already-closed session
/// (the ECHILD analog). Losing the race with [`close_master`]'s registry removal
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
    if r == ffi::WAIT_TIMEOUT {
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
/// pid is unknown, or after [`close_master`] removed the session.
#[must_use]
pub fn exit_code(pid: i32) -> Option<i32> {
    let s = session_by_pid(pid)?;
    let code = *s.exit_code.lock_or_recover();
    code.map(|c| c as i32)
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
    let mut n: u32 = 0;
    let want = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` is a valid writable slice of >= `want` bytes; the output
    // handle stays open for the call because we hold the session Arc.
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
    #[allow(dead_code)]
    Wake,
    /// Never produced on Windows (a plain blocking read has no poll-timeout / stop-flag
    /// path); present to mirror the Unix enum for the shared loop.
    #[allow(dead_code)]
    Idle,
}

/// Windows has no orphaned-different-pgroup reader-pin problem (ConPTY teardown flows
/// through `ClosePseudoConsole`, which breaks the output pipe and EOFs the reader), so
/// there is no wake pipe: return `None` and let the reader use a plain blocking read —
/// exactly the pre-MEM-L2 behavior, no regression.
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    None
}

/// With no wake pipe on Windows (`wake_rd` is always the `-1` sentinel), this is a
/// plain blocking read reported as [`ReadOutcome`].
pub fn read_or_wake(master: i32, buf: &mut [u8], _wake_rd: i32) -> ReadOutcome {
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

/// No wake pipe on Windows — nothing to signal.
pub fn wake(_wake_wr: i32) {}

/// No wake-pipe fds on Windows — nothing to close.
pub fn close_fd(_fd: i32) {}

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
        let mut written: u32 = 0;
        let want = u32::try_from(data.len()).unwrap_or(u32::MAX);
        // SAFETY: `data` is a valid slice of >= `want` bytes; the input handle
        // stays open for the call because we hold the session Arc.
        let ok = unsafe {
            ffi::WriteFile(
                s.input,
                data.as_ptr(),
                want,
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

/// Write `bytes` with a single `WriteFile`, returning the count the kernel
/// ACCEPTED (the true count the routing-fabric `SinkWriter` is built on).
/// Blocking — identical to the sink's documented Phase-0 posture.
///
/// # Errors
/// `NotFound` for a fabricated/closed key; otherwise the OS error when the
/// pipe is gone.
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let Some(s) = session(master) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "unknown PTY master key (closed or never spawned)",
        ));
    };
    let mut written: u32 = 0;
    let want = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    // SAFETY: `bytes` is a valid slice of >= `want` bytes; the input handle
    // stays open for the call because we hold the session Arc.
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
        return Err(io::Error::last_os_error());
    }
    Ok(written as usize)
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

/// Resize the pseudoconsole to `rows`×`cols`. Thread-safe: the session's HPCON
/// mutex makes UI-thread + control-thread resizes safe against the waiter's
/// `ClosePseudoConsole` (a closed session is a no-op). The result is ignored,
/// like the Unix `TIOCSWINSZ` ioctl's. A fabricated/closed key is a no-op.
pub fn resize(master: i32, rows: u16, cols: u16) {
    let Some(s) = session(master) else {
        return;
    };
    let g = s.hpc.lock_or_recover();
    if *g != 0 {
        // SAFETY: the HPCON is live while the lock is held (the waiter swaps it
        // to 0 under this same lock before closing).
        let _ = unsafe { ffi::ResizePseudoConsole(*g, coord(cols, rows)) };
    }
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
/// the UI thread. `ATERM_TRACE_BOOST=1` traces each application to stderr.
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
        if std::env::var_os("ATERM_TRACE_BOOST").is_some() {
            let which = if h == s.process { "shell" } else { "conhost" };
            eprintln!(
                "BOOST master={master} on={on} target={which} prio_ok={prio_ok} qos_ok={qos_ok}"
            );
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
