// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows ConPTY session management, job objects, and pipe I/O.
mod cmdline;
mod console_guard;
mod ffi;
mod openconsole;
mod outside_job;
mod resize_guard;
mod shell;
mod winpath;

pub use console_guard::serve_console_client;
#[cfg(any(test, debug_assertions))]
pub use console_guard::set_console_client;

pub use openconsole::{
    console_host_report, preview_console_host, probe_console_host, set_console_host,
    take_console_host_fallback,
};
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

struct WinSession {
    hpc: Mutex<isize>,
    signal: Mutex<isize>,
    reference: Mutex<isize>,
    host: ConsoleHostKind,
    input: isize,
    output: isize,
    process: isize,
    conhost: isize,
    job: isize,
    outside_job_ended: Mutex<bool>,
    close_evt: isize,
    pid: u32,
    exit_code: Mutex<Option<u32>>,
    resize_guard: ResizeGuard,
    input_severed: AtomicBool,
    #[cfg(any(test, debug_assertions))]
    input_writes: std::sync::atomic::AtomicU64,
}

impl Drop for WinSession {
    fn drop(&mut self) {
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
                // SAFETY: handles are exclusively owned by this session entry and closed once on `Drop`.
                unsafe { ffi::CloseHandle(h) };
            }
        }
    }
}

static SESSIONS: LazyLock<Mutex<HashMap<i32, Arc<WinSession>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_KEY: AtomicI32 = AtomicI32::new(0x4000_0000);

#[cfg(test)]
thread_local! {
    static REGISTERED_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn register(session: &Arc<WinSession>) -> i32 {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    SESSIONS.lock_or_recover().insert(key, Arc::clone(session));
    #[cfg(test)]
    REGISTERED_HERE.with(|n| n.set(n.get() + 1));
    key
}

fn session(master: i32) -> Option<Arc<WinSession>> {
    SESSIONS.lock_or_recover().get(&master).cloned()
}

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

fn conhost_children() -> Vec<u32> {
    // SAFETY: no-argument pid query.
    let me = unsafe { ffi::GetCurrentProcessId() };
    // SAFETY: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)` takes no pointers.
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

fn close_pseudoconsole(s: &WinSession) {
    let hpc = std::mem::replace(&mut *s.hpc.lock_or_recover(), 0);
    if hpc != 0 {
        // SAFETY: atomic swap to 0 makes this thread the sole closer of the `HPCON`.
        unsafe { ffi::ClosePseudoConsole(hpc) };
        return;
    }
    let signal = std::mem::replace(&mut *s.signal.lock_or_recover(), 0);
    if signal != 0 {
        // SAFETY: same swap-to-0 sole-closer discipline as the HPCON above.
        unsafe { ffi::CloseHandle(signal) };
    }
    let reference = std::mem::replace(&mut *s.reference.lock_or_recover(), 0);
    if reference != 0 {
        // SAFETY: same swap-to-0 sole-closer discipline.
        unsafe { ffi::CloseHandle(reference) };
    }
}

const CLOSE_GRACE_MS: u32 = 2000;
const CLOSE_KILL_REST_MS: u32 = 3000;

fn waiter(s: &WinSession) {
    let handles = [s.process, s.close_evt];
    // SAFETY: handles are kept alive by the caller's session `Arc` for the duration of the wait.
    let r = unsafe { ffi::WaitForMultipleObjects(2, handles.as_ptr(), 0, ffi::INFINITE) };
    if r == ffi::WAIT_OBJECT_0 || r == ffi::WAIT_OBJECT_0 + 1 {
        outside_job::end_once(s);
    }
    if r == ffi::WAIT_OBJECT_0 + 1 {
        close_pseudoconsole(s);
        // SAFETY: process/job handles owned by the live session entry.
        let g = unsafe { ffi::WaitForSingleObject(s.process, CLOSE_GRACE_MS) };
        if g == ffi::WAIT_TIMEOUT && s.job != 0 {
            unsafe { ffi::TerminateJobObject(s.job, 1) };
            unsafe { ffi::WaitForSingleObject(s.process, CLOSE_KILL_REST_MS) };
        }
    }
    if r == ffi::WAIT_OBJECT_0 || r == ffi::WAIT_OBJECT_0 + 1 {
        let mut code: u32 = 0;
        // SAFETY: valid process handle + out-param.
        if unsafe { ffi::GetExitCodeProcess(s.process, &mut code) } != 0 {
            s.exit_code.lock_or_recover().get_or_insert(code);
        }
    }
    close_pseudoconsole(s);
}

struct PconGuard(isize);

impl PconGuard {
    fn release(mut self) -> isize {
        std::mem::replace(&mut self.0, 0)
    }
}

impl Drop for PconGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: owned unreleased pseudoconsole closed on spawn failure before any reader starts.
            unsafe { ffi::ClosePseudoConsole(self.0) };
        }
    }
}

struct AttrList {
    buf: Vec<u64>,
    initialized: bool,
}

impl AttrList {
    fn new(count: u32) -> io::Result<Self> {
        let mut size: usize = 0;
        // SAFETY: documented `InitializeProcThreadAttributeList` size probe with a NULL list.
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

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

struct Pipes {
    in_read: ffi::Handle,
    in_write: ffi::Handle,
    out_read: ffi::Handle,
    out_write: ffi::Handle,
}

impl Pipes {
    fn new() -> io::Result<Self> {
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

enum Pseudoconsole {
    Inbox {
        hpc: PconGuard,
        conhost: ffi::Handle,
    },
    OpenConsole {
        host: openconsole::OpenConsoleHost,
        exe: OpenConsoleExe,
    },
}

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
        // SAFETY: `OpenProcess` on our just-spawned child with minimal QoS query rights.
        [pid] => unsafe { ffi::OpenProcess(ffi::PROCESS_SET_INFORMATION, 0, *pid) },
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

fn restore_ctrl_c_processing() {
    // SAFETY: `SetConsoleCtrlHandler(NULL, FALSE)` restores normal `Ctrl+C` handling.
    unsafe { ffi::SetConsoleCtrlHandler(std::ptr::null(), 0) };
}

fn coord(cols: u16, rows: u16) -> ffi::COORD {
    ffi::COORD {
        x: cols.clamp(1, 32767) as i16,
        y: rows.clamp(1, 32767) as i16,
    }
}

/// Spawn the selected shell in a fresh ConPTY of `rows`×`cols`, returning the master key.
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

/// Like [`spawn_shell`] but also returns the child pid (see [`SpawnedShell`]).
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

/// Unix-seam parity for [`spawn_shell_with_pid_cell_px`](crate::spawn_shell_with_pid_cell_px).
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
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
    aterm_cap::require(sandbox_cap, aterm_cap::Tier::Trusted).map_err(|e| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("sandbox capability tier too low — fail-closed: shell not spawned ({e})"),
        )
    })?;
    if sandbox_wrap.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OS sandbox wrap demanded but not available on Windows — refusing to spawn an \
             unsandboxed shell (fail-closed)",
        ));
    }

    let runs_program = exec_command.is_some_and(|c| !c.is_empty());
    if !runs_program && shell_override.is_some_and(|s| s.contains('\0')) {
        return Err(crate::program_nul_refusal(
            "the shell (set by --shell or `shell` in aterm.toml)",
        ));
    }
    let (program, argv) =
        shell::resolve_spawn_target(shell_override, shell_args, argv_override, exec_command);
    if let Some(index) = argv.iter().position(|a| {
        use std::os::windows::ffi::OsStrExt as _;
        a.encode_wide().any(|u| u == 0)
    }) {
        return Err(crate::argv_nul_refusal("the command line", index));
    }
    let program_w = wide_nul(&program);
    let mut cmdline_w = build_command_line(&argv);
    let mut env_pairs = crate::build_child_env(std::env::vars_os(), env_add);
    winpath::refresh_child_path(&mut env_pairs);
    let mut env_block = build_env_block(&env_pairs);
    let cwd_w = resolve_cwd_wide(cwd);

    let (pcon, pipes) = create_pseudoconsole(coord(cols, rows), &program, allow_openconsole)?;
    let Pipes {
        in_read,
        in_write,
        out_read,
        out_write,
    } = pipes;
    drop(in_read);
    drop(out_write);

    let mut attr = AttrList::new(1)?;
    let mut slots: [isize; 3] = match &pcon {
        Pseudoconsole::Inbox { .. } => [0; 3],
        Pseudoconsole::OpenConsole { host, .. } => host.slots(),
    };
    let value: *mut c_void = match &pcon {
        Pseudoconsole::Inbox { hpc, .. } => hpc.0 as *mut c_void,
        Pseudoconsole::OpenConsole { .. } => slots.as_mut_ptr().cast(),
    };
    // SAFETY: `attr` list is initialized and `value` outlives `CreateProcessW`.
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

    // SAFETY: zeroed `STARTUPINFOEXW` with `cb` and `lpAttributeList` set; buffers outlive the call.
    let mut si: ffi::STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<ffi::STARTUPINFOEXW>() as u32;
    si.lpAttributeList = attr.ptr();
    si.StartupInfo.dwFlags = ffi::STARTF_USESTDHANDLES;
    restore_ctrl_c_processing();
    // SAFETY: PROCESS_INFORMATION is POD out-memory.
    let mut pi: ffi::PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: UTF-16 command, environment, and working-directory buffers are NUL-terminated and outlive the call.
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

    let fail_closed = |what: &str| -> io::Error {
        let os = io::Error::last_os_error();
        // SAFETY: suspended child never ran; `TerminateProcess` and handle drops clean up before exec.
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
    if let Err(e) = limits.apply_to_job(sandbox_cap, job.raw() as std::os::windows::io::RawHandle) {
        // SAFETY: the suspended child never executed an instruction.
        unsafe { ffi::TerminateProcess(pi.hProcess, 126) };
        return Err(e);
    }
    // SAFETY: NULL attrs/name; manual-reset (TRUE), initially unsignaled.
    let close_evt = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    if close_evt == 0 {
        return Err(fail_closed("CreateEventW failed"));
    }
    let close_evt = ffi::Handle::new(close_evt);

    // SAFETY: the child's (suspended) primary thread handle.
    if unsafe { ffi::ResumeThread(h_thread.raw()) } == u32::MAX {
        return Err(fail_closed("ResumeThread failed"));
    }
    drop(h_thread); // done with the thread handle
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
        drop((pcon, attr, job, h_process, close_evt, in_write, out_read));
        if sticky {
            openconsole::note_launch_failure(&exe, &why);
        } else {
            openconsole::note_silent_start(&exe, &why);
        }
        return spawn_on_host(req, false);
    }
    let pid = pi.dwProcessId as i32;
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
        #[cfg(any(test, debug_assertions))]
        input_writes: std::sync::atomic::AtomicU64::new(0),
    });
    let key = register(&session);
    let waiter_session = Arc::clone(&session);
    let spawned = std::thread::Builder::new()
        .name("aterm-pty-waiter".into())
        .spawn(move || waiter(&waiter_session));
    if spawned.is_err() {
        SESSIONS.lock_or_recover().remove(&key);
        // SAFETY: job handle owned by `session` (still alive via our Arc).
        unsafe { ffi::TerminateJobObject(session.job, 126) };
        return Err(io::Error::other("failed to spawn the ConPTY waiter thread"));
    }
    Ok(SpawnedShell { master: key, pid })
}

fn release_handoff_handles(handles: [isize; 4]) {
    for h in handles {
        if h != 0 {
            // SAFETY: caller-supplied handles are unowned by any live session on this error path.
            unsafe { ffi::CloseHandle(h) };
        }
    }
}

/// ADOPT an inbound Windows 11 default-terminal (DefTerm) handoff.
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
    // SAFETY: NULL attrs/name; manual-reset (TRUE), initially unsignaled.
    let close_evt = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
    if close_evt == 0 {
        release_handoff_handles([in_pipe, out_pipe, signal_pipe, client_process]);
        return Err(io::Error::other(
            "adopt_handoff: CreateEventW failed; refusing to register a session \
             the waiter cannot wait on and nothing can hang up",
        ));
    }

    // SAFETY: `client_process` is a live process handle transferred to this session.
    let pid = unsafe { ffi::GetProcessId(client_process) };

    let session = Arc::new(WinSession {
        hpc: Mutex::new(0),
        signal: Mutex::new(signal_pipe),
        reference: Mutex::new(0),
        host: ConsoleHostKind::Inbox,
        input: in_pipe,
        output: out_pipe,
        process: client_process,
        conhost: 0,
        job: 0,
        outside_job_ended: Mutex::new(false),
        close_evt,
        pid,
        exit_code: Mutex::new(None),
        resize_guard: ResizeGuard::new(None, ConsoleHostKind::Inbox),
        input_severed: AtomicBool::new(false),
        #[cfg(any(test, debug_assertions))]
        input_writes: std::sync::atomic::AtomicU64::new(0),
    });
    let key = register(&session);
    let waiter_session = Arc::clone(&session);
    let spawned = std::thread::Builder::new()
        .name("aterm-pty-waiter".into())
        .spawn(move || waiter(&waiter_session));
    if spawned.is_err() {
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

fn paths_eq_ignore_ascii_case(a: &std::path::Path, b: &std::path::Path) -> bool {
    let a = a.as_os_str().to_string_lossy();
    let b = b.as_os_str().to_string_lossy();
    a.trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

/// HANG UP a spawned shell.
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    if let Some(s) = session_by_pid(pid) {
        outside_job::end_once(&s);
        // SAFETY: event handle owned by the session Arc we hold.
        unsafe { ffi::SetEvent(s.close_evt) };
    }
}

/// End, in every open session, the programs below its shell that its job does not hold ([`outside_job`]: what a Store pwsh starts).
pub fn end_programs_outside_jobs() {
    let open: Vec<Arc<WinSession>> = SESSIONS.lock_or_recover().values().cloned().collect();
    for s in &open {
        outside_job::end_once(s);
    }
}

/// Reap an exited child WITHOUT ever blocking unboundedly (the Unix `reap`'s poll/escalate/deadline discipline, on Windows primitives).
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
    if r == ffi::WAIT_TIMEOUT && s.job != 0 {
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

/// The recorded exit code of a spawned shell, once it has exited.
#[must_use]
pub fn exit_code(pid: i32) -> Option<i32> {
    let s = session_by_pid(pid)?;
    let code = *s.exit_code.lock_or_recover();
    code.map(|c| c as i32)
}

/// The Unix start-time reader's twin.
#[must_use]
pub fn process_birth(_pid: i32) -> Option<u64> {
    None
}

/// The Unix recorder's twin.
#[must_use]
pub fn record_spawned_shell(pid: i32) -> crate::ShellIdentity {
    crate::ShellIdentity::from_parts(pid, None, true)
}

/// The Unix recorder's twin.
#[must_use]
pub fn record_adopted_shell(pid: i32, _master: i32) -> crate::ShellIdentity {
    crate::ShellIdentity::from_parts(pid, None, false)
}

impl crate::ShellIdentity {
    /// On Windows a pid "still names the shell" exactly when this process's session registry resolves it unambiguously.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.pid > 1 && session_by_pid(self.pid).is_some()
    }
}

/// The Unix [`hangup_shell`] twin.
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

/// The Unix `reap_shell_in_background` twin.
pub fn reap_shell_in_background(identity: crate::ShellIdentity) {
    if identity.pid > 1 {
        std::thread::spawn(move || reap_shell(identity));
    }
}

/// Platform-neutral twin of the Unix collector.
#[must_use]
pub fn collect_exit_status(pid: i32) -> Option<crate::ChildExit> {
    exit_code(pid).map(crate::ChildExit::Code)
}

/// Close a session's master key.
pub fn close_master(master: i32) {
    let removed = SESSIONS.lock_or_recover().remove(&master);
    if let Some(s) = removed {
        outside_job::end_once(&s);
        // SAFETY: event/pipe handles owned by the Arc we still hold.
        unsafe {
            ffi::SetEvent(s.close_evt);
            ffi::CancelIoEx(s.output, std::ptr::null_mut());
        }
    }
}

/// RAII around a Windows master key.
#[derive(Debug)]
pub struct OwnedMaster(i32);

impl OwnedMaster {
    /// Adopt sole ownership of `master`.
    #[must_use]
    pub fn adopt(master: i32) -> Self {
        Self(master)
    }

    /// The raw registry key.
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

/// Read up to `buf.len()` bytes of ConPTY output.
pub fn read(master: i32, buf: &mut [u8]) -> isize {
    let Some(s) = session(master) else {
        return -1;
    };
    read_session(&s, buf)
}

fn read_session(s: &WinSession, buf: &mut [u8]) -> isize {
    let mut n: u32 = 0;
    let want = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` is a writable slice of at least `want` bytes and the session `Arc` keeps the pipe open.
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
        // Distinct branch.
        0 // EOF — the exact `r <= 0` trigger the reader threads key on
    } else {
        -1
    }
}

/// Outcome of [`read_or_wake`].
pub enum ReadOutcome {
    Data(usize),
    Eof,
    Wake,
    Idle,
    Exited,
    Proc,
}

/// Return `None` on Windows where ConPTY teardown breaks the output pipe directly.
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    None
}

/// With no wake pipe on Windows (`wake_rd` is always the `-1` sentinel), this is a plain blocking read reported as [`ReadOutcome`].
pub fn read_or_wake(
    master: i32,
    buf: &mut [u8],
    _wake_rd: i32,
    _exit_rd: i32,
    _proc_rd: i32,
) -> ReadOutcome {
    let n = read(master, buf);
    if n > 0 {
        ReadOutcome::Data(n as usize)
    } else {
        ReadOutcome::Eof
    }
}

/// Windows counterpart of the unix batched-gather top-up.
pub fn drain_more(_master: i32, _buf: &mut [u8], filled: usize) -> usize {
    filled
}

/// Top `buf[filled..]` up with ConPTY output that is already waiting in the pipe, never blocking.
pub fn drain_available(master: i32, buf: &mut [u8], mut filled: usize) -> usize {
    let Some(s) = session(master) else {
        return filled;
    };
    while filled < buf.len() {
        let mut avail: u32 = 0;
        // SAFETY: zero-length `PeekNamedPipe` on the live output pipe reads only the available byte count.
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

/// No `FD_CLOEXEC` on Windows.
#[allow(clippy::unnecessary_wraps)]
pub fn set_cloexec(_fd: i32, _on: bool) -> io::Result<()> {
    Ok(())
}

/// Always `false`.
#[must_use]
pub fn fd_is_tty(_fd: i32) -> bool {
    false
}

/// Windows twin of the Unix [`tty_echo`].
#[must_use]
pub fn tty_echo(_master: i32) -> Option<crate::TtyEcho> {
    None
}

/// Windows twin of the Unix [`input_queue_len`].
#[must_use]
pub fn input_queue_len(_master: i32) -> Option<usize> {
    None
}

/// Windows twin of the Unix [`output_queue_len`].
#[must_use]
pub fn output_queue_len(_master: i32) -> Option<usize> {
    None
}

/// Windows twin of the Unix [`tty_passes_every_byte`].
#[must_use]
pub fn tty_passes_every_byte(_master: i32) -> Option<bool> {
    None
}

/// Windows twin of the Unix [`flush_input_queue`].
#[must_use]
pub fn flush_input_queue(_master: i32) -> Option<usize> {
    None
}

/// Write all of `bytes` to the child's input, retrying short writes.
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
    static REPLYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `write` as the terminal's own ANSWER to a query its program or console host sent (DA, DSR/CPR, a colour or mode query).
pub fn as_reply<T>(write: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REPLYING.with(|r| r.set(self.0));
        }
    }
    let _restore = Restore(REPLYING.with(|r| r.replace(true)));
    write()
}

/// Settle `master`'s [`resize_guard`] for the INPUT write its caller is about to make.
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

/// Write `bytes` with a single `WriteFile` of at most the input pipe's 64 KiB ([`INPUT_WRITE_MAX`]).
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    write_some_stoppable(master, bytes, None)
}

/// [`write_some`] for one write of a frame whose stop flag is `stop` (a metered paste's `BulkMeter`, in aterm-session).
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

static WRITERS: input_release::Registry<WriterThread> = input_release::Registry::new();

const RELEASE_STEP: std::time::Duration = std::time::Duration::from_millis(1);
const RELEASE_BOUND: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy)]
struct WriterThread(isize);

impl input_release::Blocked for WriterThread {
    fn cancel(&self) -> bool {
        // SAFETY: called under the registry lock while the registered thread handle remains open.
        unsafe { ffi::CancelSynchronousIo(self.0) != 0 }
    }
}

struct ThisThread(isize);

impl ThisThread {
    fn open() -> Self {
        let mut handle: isize = 0;
        // SAFETY: `DuplicateHandle` with current-process/thread pseudo-handles and a valid out-pointer.
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

fn this_writer() -> Option<WriterThread> {
    THIS_THREAD
        .try_with(|t| t.0)
        .ok()
        .filter(|h| *h != 0)
        .map(WriterThread)
}

fn session_tag(s: &WinSession) -> usize {
    std::ptr::from_ref(s) as usize
}

fn release_soon(mut ids: Vec<u64>) {
    WRITERS.cancel_round(&mut ids);
    if ids.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-input-release".into())
        .spawn(move || WRITERS.release(ids, RELEASE_STEP, RELEASE_BOUND));
    drop(spawned);
}

/// Sever `master`'s input for good (aterm-session's `SinkWriter::sever_input`, at a tab's close).
pub fn release_input_writes(master: i32) {
    let Some(s) = session(master) else {
        return;
    };
    release_soon(WRITERS.sever(session_tag(&s), &s.input_severed));
}

/// Release the input writes of the frame whose stop flag is `stop`, which the caller has just set (aterm-session's `BulkMeter::request_stop`).
pub fn release_stopped_writes(stop: &AtomicBool) {
    release_soon(WRITERS.stopped(stop));
}

const INPUT_WRITE_MAX: usize = PIPE_BUFFER_BYTES as usize;

fn write_input(s: &WinSession, bytes: &[u8], stop: Option<&AtomicBool>) -> io::Result<usize> {
    WRITERS.write(
        session_tag(s),
        &s.input_severed,
        stop,
        this_writer(),
        || {
            #[cfg(any(test, debug_assertions))]
            s.input_writes.fetch_add(1, Ordering::AcqRel);
            let mut written: u32 = 0;
            let want = u32::try_from(bytes.len().min(INPUT_WRITE_MAX)).unwrap_or(PIPE_BUFFER_BYTES);
            // SAFETY: `bytes` is valid for `want` bytes and the session `Arc` keeps the input pipe open.
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

/// Test-only.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn output_waiting_for_test(master: i32) -> Option<u32> {
    let s = session(master)?;
    let mut avail: u32 = 0;
    // SAFETY: zero-length `PeekNamedPipe` on the session's live output pipe.
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
    (peeked != 0).then_some(avail)
}

/// Test-only.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn input_writes_for_test(master: i32) -> Option<u64> {
    session(master).map(|s| s.input_writes.load(Ordering::Acquire))
}

/// [`write_some`] twin of the Unix blocking-emulation wrapper.
pub fn write_some_blocking(master: i32, bytes: &[u8]) -> io::Result<usize> {
    write_some(master, bytes)
}

/// Toggle non-blocking mode.
pub fn set_nonblocking(_master: i32, nonblocking: bool) -> io::Result<()> {
    if nonblocking {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "non-blocking PTY writes are not supported on Windows (overlapped-I/O milestone)",
        ));
    }
    Ok(())
}

const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;

fn encode_resize_signal(rows: u16, cols: u16) -> [u8; 6] {
    let c = coord(cols, rows);
    let mut buf = [0u8; 6];
    buf[0..2].copy_from_slice(&PTY_SIGNAL_RESIZE_WINDOW.to_le_bytes());
    buf[2..4].copy_from_slice(&(c.x as u16).to_le_bytes());
    buf[4..6].copy_from_slice(&(c.y as u16).to_le_bytes());
    buf
}

/// Resize the pseudoconsole to `rows`×`cols`.
pub fn resize(master: i32, rows: u16, cols: u16) {
    resize_with_cell_px(master, rows, cols, None);
}

/// Whether `master` is a live ConPTY session.
#[must_use]
pub fn backend_is_conpty(master: i32) -> bool {
    session(master).is_some()
}

/// Unix-seam parity for `aterm_pty::size`.
#[must_use]
pub fn size(_master: i32) -> Option<(u16, u16)> {
    None
}

/// Make `master`'s console host LEAVE its alternate screen buffer.
pub fn resync_alt_screen(master: i32) -> io::Result<()> {
    resync_alt_screen_with(master, None)
}

/// [`resync_alt_screen`].
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

/// Which console host serves `master`.
#[must_use]
pub fn conpty_host(master: i32) -> Option<ConsoleHostKind> {
    session(master).map(|s| s.host)
}

/// Unix-seam parity for [`resize_with_cell_px`](crate::resize_with_cell_px).
pub fn resize_with_cell_px(master: i32, rows: u16, cols: u16, _cell_px: Option<(u16, u16)>) {
    let Some(s) = session(master) else {
        return;
    };
    let size = coord(cols, rows);
    let g = s.hpc.lock_or_recover();
    if *g != 0 {
        // SAFETY: `HPCON` is live while the lock is held.
        if unsafe { ffi::ResizePseudoConsole(*g, size) } >= 0
            && let Some(arming) = s.resize_guard.note_resize(size.y, size.x)
        {
            schedule_nudge(master, arming);
        }
        return;
    }
    let sig = s.signal.lock_or_recover();
    if *sig != 0 {
        let buf = encode_resize_signal(rows, cols);
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

const NUDGE_AFTER: std::time::Duration = std::time::Duration::from_millis(50);

fn schedule_nudge(master: i32, arming: u64) {
    let _ = std::thread::Builder::new()
        .name("aterm-conpty-nudge".into())
        .spawn(move || {
            std::thread::sleep(NUDGE_AFTER);
            let Some(s) = session(master) else {
                return;
            };
            let _ = s.resize_guard.nudge(
                arming,
                || console_guard::runtime_in_tab(&s),
                |rows, cols| console_guard::guard(&s, rows, cols),
                |b| write_input(&s, b, None),
            );
        });
}

/// Focus-linked QoS boost.
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

/// Locale overrides for spawned children.
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

    #[test]
    fn under_tier_sandbox_cap_is_denied_fail_closed_no_session() {
        // SAFETY: single-threaded mint; trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);
        let sessions_before = registered_here();
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
            registered_here(),
            sessions_before,
            "no session may be registered for a denied spawn"
        );
    }

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

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        assert_eq!(buf, [0x08, 0x00, 0x50, 0x00, 0x18, 0x00]);
    }

    #[test]
    fn resize_signal_is_not_transposed() {
        let buf = encode_resize_signal(1, 999);
        assert_eq!(u16::from_le_bytes([buf[2], buf[3]]), 999, "columns");
        assert_eq!(u16::from_le_bytes([buf[4], buf[5]]), 1, "rows");
    }

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

    fn keys_read_through_conpty(drive: impl FnOnce(i32)) -> String {
        keys_read_through_conpty_after("", drive)
    }

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

    fn registered_here() -> u64 {
        REGISTERED_HERE.with(std::cell::Cell::get)
    }

    #[test]
    fn adopt_handoff_without_a_client_process_is_refused_and_registers_nothing() {
        let before = registered_here();
        let err = adopt_handoff(0, 0, 0, 0)
            .expect_err("a handoff with no client process handle must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            registered_here(),
            before,
            "a refused handoff must not register a session"
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
        let sh = adopt_handoff(0, 0, wr, me).expect("a handoff with a client process is adopted");
        assert_eq!(
            registered_here(),
            before + 1,
            "the count sees an accepted handoff's registration"
        );
        close_master(sh.master);
        // SAFETY: the read end is still ours (the session never owned it).
        unsafe { ffi::CloseHandle(rd) };
    }

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

    #[test]
    fn adopted_session_has_no_hpcon_no_job_and_keeps_the_signal_pipe() {
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

        for _ in 0..50 {
            assert_ne!(
                *s.signal.lock_or_recover(),
                0,
                "the waiter tore the adopted session down while its client was still alive"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        close_master(sh.master);
        assert!(
            session(sh.master).is_none(),
            "close_master must unregister the adopted session"
        );
        // SAFETY: the read end is still ours (the session never owned it).
        unsafe { ffi::CloseHandle(rd) };
    }

    #[test]
    fn hangup_tears_down_an_adopted_session_through_the_signal_pipe() {
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
        assert!(
            client.is_alive(),
            "the client must still be running, or the teardown proves nothing"
        );
        close_master(sh.master);
        // SAFETY: the read end was never owned by the session.
        unsafe { ffi::CloseHandle(rd) };
    }

    #[test]
    fn drain_available_collects_what_the_pipe_holds_and_never_blocks() {
        let client = TestClient::spawn();
        let pipe = || {
            let (mut rd, mut wr): (isize, isize) = (0, 0);
            // SAFETY: valid out-pointers and default security attributes for `CreatePipe`.
            assert_ne!(
                unsafe { ffi::CreatePipe(&mut rd, &mut wr, std::ptr::null_mut(), 64 * 1024) },
                0,
                "CreatePipe must succeed"
            );
            (rd, wr)
        };
        let (out_rd, out_wr) = pipe();
        let (sig_rd, sig_wr) = pipe();
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
        for line in [&b"one\r\n"[..], b"two\r\n", b"six\r\n"] {
            write(line);
        }
        let mut buf = [0u8; 64];
        buf[..2].copy_from_slice(b">>");
        let filled = drain_available(sh.master, &mut buf, 2);
        assert_eq!(&buf[..filled], b">>one\r\ntwo\r\nsix\r\n");
        write(&[b'x'; 100]);
        let mut small = [0u8; 40];
        assert_eq!(drain_available(sh.master, &mut small, 0), 40);
        assert_eq!(drain_available(sh.master, &mut small, 0), 40);
        assert_eq!(drain_available(sh.master, &mut small, 10), 30);
        assert_eq!(drain_available(sh.master, &mut small, 0), 0, "drained");
        close_master(sh.master);
        assert_eq!(drain_available(sh.master, &mut small, 7), 7);
        // SAFETY: the ends the session never owned.
        unsafe {
            ffi::CloseHandle(out_wr);
            ffi::CloseHandle(sig_rd);
        }
    }

    #[test]
    fn a_pid_claimed_by_two_live_sessions_resolves_to_neither() {
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
        assert_eq!(
            exit_code(pid),
            None,
            "an ambiguous pid must not report some other session's exit code"
        );
        reap(pid);

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

    #[test]
    fn release_handoff_handles_closes_the_live_slots_and_skips_the_zeros() {
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

    #[test]
    fn adopted_session_tolerates_a_process_handle_with_no_query_rights() {
        // SAFETY: `OpenProcess(SYNCHRONIZE, ...)` on our own PID yields a valid closable handle.
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

        resize(sh.master, 24, 80);
        close_master(sh.master);
        assert!(
            session(sh.master).is_none(),
            "close_master must unregister it"
        );
    }

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
