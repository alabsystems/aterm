// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned Windows helper exchange: nonblocking private pipes, one kill-on-close job,
//! and no input-pump thread that can retain a secret after the call returns.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

use crate::secret::Secret;
use crate::spawnfd::{Child, Command, Stdio};

const STEP: Duration = Duration::from_millis(5);
const CLEANUP: Duration = Duration::from_secs(60);
const CREATE_SUSPENDED: u32 = 4;
const PIPE_NOWAIT: u32 = 1;
const PIPE_REJECT_REMOTE_CLIENTS: u32 = 8;
const FIRST_INSTANCE: u32 = 0x0008_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const ERROR_NO_DATA: i32 = 232;
const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
const ERROR_PIPE_CONNECTED: i32 = 535;
const WAIT_TIMEOUT: u32 = 258;

fn timeout() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "helper exchange deadline elapsed")
}

fn handle(raw: RawHandle) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw as isize == -1 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the caller passes a fresh owned handle returned by a Win32 create/open.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

struct Descriptor(*mut c_void);

impl Descriptor {
    fn owner_only() -> io::Result<Self> {
        let sddl: Vec<u16> = "D:P(A;;GA;;;OW)".encode_utf16().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: terminated SDDL and a writable out-pointer; revision 1 is SDDL_REVISION_1.
        // OW grants the created object's owner full access, without Everyone/anonymous ACEs.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the conversion allocated this descriptor with LocalAlloc; one LocalFree.
        unsafe { LocalFree(self.0) };
    }
}

/// Server handles never block; the child's handles have ordinary blocking byte semantics.
/// All handles are initially noninheritable. Std's Stdio duplicates just the three client
/// handles for the child, rather than exposing a process-wide inheritable handle.
fn pipe(parent_writes: bool) -> io::Result<(File, File)> {
    let mut nonce = [0_u8; 16];
    crate::rand::fill(&mut nonce)?;
    let mut name = format!(r"\\.\pipe\aterm-helper-{}-", std::process::id());
    for byte in nonce {
        use std::fmt::Write as _;
        write!(name, "{byte:02x}").expect("writing a String");
    }
    let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let descriptor = Descriptor::owner_only()?;
    let security = Security {
        size: std::mem::size_of::<Security>() as u32,
        descriptor: descriptor.0,
        inherit: 0,
    };
    // SAFETY: a terminated name and initialized security attributes whose descriptor stays
    // alive through this call. First-instance refuses collisions, remote clients are refused,
    // and the explicit owner-only DACL excludes the named-pipe default Everyone read grant.
    let server = handle(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            FIRST_INSTANCE | if parent_writes { 2 } else { 1 },
            PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65_536,
            65_536,
            0,
            &raw const security,
        )
    })?;
    // SAFETY: the first-instance server above owns this unpredictable name. NULL security
    // attributes leave this client noninheritable, and OPEN_EXISTING opens only that pipe.
    let client = handle(unsafe {
        CreateFileW(
            name.as_ptr(),
            if parent_writes {
                GENERIC_READ
            } else {
                GENERIC_WRITE
            },
            0,
            std::ptr::null(),
            3,
            0,
            std::ptr::null_mut(),
        )
    })?;
    // SAFETY: the live server is synchronous and nonblocking; no OVERLAPPED is needed. A
    // client opened just before this call yields ERROR_PIPE_CONNECTED, which is success.
    if unsafe { ConnectNamedPipe(server.as_raw_handle(), std::ptr::null_mut()) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_CONNECTED) {
            return Err(error);
        }
    }
    let mut client_pid = 0;
    // SAFETY: the connected live server and a writable process-id out-pointer.
    if unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &raw mut client_pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if client_pid != std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "helper pipe client changed",
        ));
    }
    Ok((File::from(server), File::from(client)))
}

struct Job(OwnedHandle);

impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: NULL name/security creates a fresh noninheritable anonymous job.
        let job = Self(handle(unsafe {
            CreateJobObjectW(std::ptr::null(), std::ptr::null())
        })?);
        // SAFETY: this Win32 information struct is POD; all-zero values disable limits.
        let mut limits: ExtendedLimits = unsafe { std::mem::zeroed() };
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        // SAFETY: job is live; limits is initialized and has exactly the stated ABI size.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                9,
                (&raw const limits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles are live. The process's primary thread is still suspended;
        // no application instruction or application descendant precedes this assignment.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn terminate(&self) -> io::Result<()> {
        // SAFETY: the owned job, never a reused numeric pid or group, is the cleanup scope.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn empty(&self) -> io::Result<bool> {
        // SAFETY: Accounting is POD out-memory, initialized to zero before the query.
        let mut accounting: Accounting = unsafe { std::mem::zeroed() };
        // SAFETY: job is live and accounting is writable for exactly its stated size.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                1,
                (&raw mut accounting).cast(),
                std::mem::size_of::<Accounting>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.active == 0)
    }
}

/// Find the still-suspended initial thread using documented Toolhelp APIs. Refuse an
/// ambiguous snapshot rather than resuming a thread selected by a guessed numeric id.
fn resume(child: &Child) -> io::Result<()> {
    // SAFETY: TH32CS_SNAPTHREAD requests a fresh snapshot, with no borrowed inputs.
    let snapshot = handle(unsafe { CreateToolhelp32Snapshot(4, 0) })?;
    // SAFETY: ThreadEntry is the Win32 POD structure; size is filled before Thread32First.
    let mut entry: ThreadEntry = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ThreadEntry>() as u32;
    let mut found = None;
    // SAFETY: snapshot is live and entry is initialized writable out-memory.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    if !more {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(18) {
            return Err(error);
        }
    }
    while more {
        if entry.owner == child.id() {
            // SAFETY: open only the rights needed to verify and resume this candidate.
            let thread = handle(unsafe { OpenThread(0x0802, 0, entry.id) })?;
            // SAFETY: live thread and process handles; both queries only read their state.
            let owned = unsafe {
                GetProcessIdOfThread(thread.as_raw_handle()) == child.id()
                    && crate::win::ffi::WaitForSingleObject(child.as_raw_handle() as isize, 0)
                        == WAIT_TIMEOUT
            };
            if !owned || found.replace(thread).is_some() {
                return Err(io::Error::other("suspended helper thread is ambiguous"));
            }
        }
        entry.size = std::mem::size_of::<ThreadEntry>() as u32;
        // SAFETY: the same live snapshot and initialized writable entry.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        if !more {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(18) {
                return Err(error);
            }
        }
    }
    let thread = found.ok_or_else(|| io::Error::other("suspended helper thread is unavailable"))?;
    // SAFETY: this owned thread was verified above while its process was alive and suspended.
    // Its handle cannot be rebound to a different thread if it exits meanwhile.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Scope {
    child: Child,
    job: Job,
    retired: bool,
    cleanup_started: bool,
}

impl Scope {
    fn retire(&mut self) -> io::Result<()> {
        self.job.terminate()?;
        self.cleanup_started = true;
        let by = super::saturating_after(Instant::now(), CLEANUP);
        loop {
            let reaped = self.child.try_wait()?.is_some();
            if reaped && self.job.empty()? {
                self.retired = true;
                return Ok(());
            }
            if Instant::now() >= by {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "helper job did not retire",
                ));
            }
            std::thread::sleep(STEP);
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if !self.retired {
            let _ = self.job.terminate();
            // A setup failure can precede job assignment. Child::kill uses its process
            // handle, so even that unassigned suspended child is ended without a pid race.
            let _ = self.child.kill();
            if !self.cleanup_started {
                super::reap_within(
                    &mut self.child,
                    None,
                    super::saturating_after(Instant::now(), CLEANUP),
                );
            }
        }
    }
}

fn drain(pipe: &mut Option<File>, bytes: &mut Vec<u8>, max: usize) -> io::Result<bool> {
    let Some(reader) = pipe.as_mut() else {
        return Ok(false);
    };
    let mut progressed = false;
    let mut chunk = [0_u8; 8192];
    // Bound each visit so continuous output cannot starve input or the deadline check.
    for _ in 0..16 {
        match reader.read(&mut chunk) {
            Ok(0) => {
                *pipe = None;
                return Ok(true);
            }
            Ok(n) => {
                if n > max.saturating_sub(bytes.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "helper output limit exceeded",
                    ));
                }
                bytes.extend_from_slice(&chunk[..n]);
                progressed = true;
            }
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if error.kind() == io::ErrorKind::BrokenPipe
                    || error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED) =>
            {
                *pipe = None;
                return Ok(true);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(progressed)
}

pub(super) fn output_with_input_until(
    command: &mut Command,
    input: Secret,
    deadline: Instant,
    max_output: usize,
) -> io::Result<Output> {
    if super::deadline_now() >= deadline {
        return Err(timeout());
    }
    let job = Job::new()?;
    let (stdin, child_stdin) = pipe(true)?;
    let (stdout, child_stdout) = pipe(false)?;
    let (stderr, child_stderr) = pipe(false)?;
    command
        .stdin(Stdio::from(child_stdin))
        .stdout(Stdio::from(child_stdout))
        .stderr(Stdio::from(child_stderr));
    crate::spawn::set_flags(command, CREATE_SUSPENDED);
    let spawned = command.spawn();
    // Release the command's retained client endpoints. Otherwise their parent copies keep
    // output writers alive, or keep stdin readable, after the child has closed its copies.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::spawn::set_flags(command, 0);
    let mut scope = Scope {
        child: spawned?,
        job,
        retired: false,
        cleanup_started: false,
    };
    // These bindings drop before Scope on every setup error, scrubbing unsent input and
    // closing the parent pipe endpoints before the job's cleanup grace.
    let mut input = Some(input);
    let mut stdin = Some(stdin);
    let (mut stdout, mut stderr) = (Some(stdout), Some(stderr));
    scope.job.assign(&scope.child)?;
    if super::deadline_now() >= deadline {
        return Err(timeout());
    }
    resume(&scope.child)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut sent = 0;
    let mut status: Option<ExitStatus> = None;
    let result = (|| {
        loop {
            if super::deadline_now() >= deadline {
                return Err(timeout());
            }
            let mut progressed = drain(&mut stdout, &mut out, max_output)?;
            progressed |= drain(&mut stderr, &mut err, max_output)?;
            if let (Some(secret), Some(writer)) = (input.as_ref(), stdin.as_mut()) {
                let left = &secret.expose()[sent..];
                match writer.write(&left[..left.len().min(8192)]) {
                    Ok(n) => {
                        sent += n;
                        progressed |= n > 0;
                        if sent == secret.len() {
                            stdin = None;
                            input = None;
                        }
                    }
                    Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA) => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error)
                        if error.kind() == io::ErrorKind::BrokenPipe
                            || error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED) =>
                    {
                        stdin = None;
                        input = None;
                    }
                    Err(error) => return Err(error),
                }
            }
            if status.is_none()
                && let Some(exited) = scope.child.try_wait()?
            {
                status = Some(exited);
                stdin = None;
                input = None;
                scope.job.terminate()?;
            }
            if let Some(status) = status
                && stdout.is_none()
                && stderr.is_none()
            {
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            if !progressed {
                std::thread::sleep(STEP.min(deadline.saturating_duration_since(Instant::now())));
            }
        }
    })();
    // Scrub any unconsumed input before cleanup; no helper can keep a second copy alive.
    drop(input);
    drop(stdin);
    drop(stdout);
    drop(stderr);
    let retired = scope.retire();
    match (result, retired) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

#[repr(C)]
struct Security {
    size: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[repr(C)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    id: u32,
    owner: u32,
    base_priority: i32,
    delta_priority: i32,
    flags: u32,
}

#[repr(C)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_limit: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}

#[repr(C)]
struct ExtendedLimits {
    basic: BasicLimits,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

#[repr(C)]
struct Accounting {
    user_time: i64,
    kernel_time: i64,
    period_user: i64,
    period_kernel: i64,
    faults: u32,
    total: u32,
    active: u32,
    terminated: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open: u32,
        mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *const Security,
    ) -> RawHandle;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: RawHandle,
    ) -> RawHandle;
    fn ConnectNamedPipe(pipe: RawHandle, overlapped: *mut c_void) -> i32;
    fn GetNamedPipeClientProcessId(pipe: RawHandle, process: *mut u32) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn CreateJobObjectW(security: *const c_void, name: *const u16) -> RawHandle;
    fn SetInformationJobObject(job: RawHandle, class: i32, info: *const c_void, bytes: u32) -> i32;
    fn QueryInformationJobObject(
        job: RawHandle,
        class: i32,
        info: *mut c_void,
        bytes: u32,
        returned: *mut u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: RawHandle, process: RawHandle) -> i32;
    fn TerminateJobObject(job: RawHandle, code: u32) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> RawHandle;
    fn Thread32First(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, id: u32) -> RawHandle;
    fn GetProcessIdOfThread(thread: RawHandle) -> u32;
    fn ResumeThread(thread: RawHandle) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLE: &str = "ATERM_OWNED_EXCHANGE_FIXTURE_ROLE";
    const PROOF: &str = "ATERM_OWNED_EXCHANGE_FIXTURE_PROOF";

    fn fixture(role: &str, proof: &std::path::Path) -> Command {
        let mut command = crate::spawnfd::command(std::env::current_exe().expect("test binary"));
        command
            .args([
                "--exact",
                "deadline::input_windows::tests::owned_exchange_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, role)
            .env(PROOF, proof);
        command
    }

    fn witness(proof: &std::path::Path) -> Option<(i32, crate::process::Birth)> {
        let text = std::fs::read_to_string(proof).ok()?;
        let (pid, birth) = text.trim().split_once(' ')?;
        Some((
            pid.parse().ok()?,
            crate::process::Birth(birth.parse().ok()?),
        ))
    }

    #[test]
    fn owned_exchange_fixture() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        let proof = std::path::PathBuf::from(std::env::var_os(PROOF).expect("proof path"));
        if role == "parent" {
            let mut command = fixture("retainer", &proof);
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            let retainer = command.spawn().expect("owned descendant");
            let by = Instant::now() + CLEANUP;
            loop {
                if let Some((pid, birth)) = witness(&proof) {
                    assert_eq!(u32::try_from(pid).unwrap(), retainer.id());
                    assert_eq!(crate::process::read_birth(pid), Some(birth));
                    std::process::exit(67);
                }
                assert!(Instant::now() < by, "descendant execution proof");
                std::thread::sleep(STEP);
            }
        }
        assert!(matches!(role.as_str(), "retainer" | "nonreader"));
        let pid = i32::try_from(std::process::id()).unwrap();
        let birth = crate::process::read_birth(pid).expect("this live fixture's birth");
        println!("owned-fixture-output:{pid}");
        eprintln!("owned-fixture-diagnostic:{pid}");
        std::io::stdout().flush().expect("stdout proof");
        std::io::stderr().flush().expect("stderr proof");
        std::fs::write(proof, format!("{pid} {}\n", birth.0)).expect("execution proof");
        loop {
            std::thread::park_timeout(CLEANUP);
        }
    }

    #[test]
    fn direct_exit_retires_the_witnessed_descendant_holding_all_three_pipes() {
        let dir = aterm_tempfile::tempdir().expect("owned proof directory");
        let proof = dir.path().join("descendant");
        let output = output_with_input_until(
            &mut fixture("parent", &proof),
            Secret::new(&[b'x'; 256 * 1024]),
            Instant::now() + CLEANUP,
            16 * 1024,
        )
        .expect("the direct exit cannot wait for its descendant's pipe handles");
        let (pid, birth) = witness(&proof).expect("descendant proved execution");
        assert_eq!(output.status.code(), Some(67));
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned-fixture-output:"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("owned-fixture-diagnostic:"));
        assert_ne!(crate::process::read_birth(pid), Some(birth));
    }

    #[test]
    fn a_witnessed_nonreading_child_is_retired_when_the_deadline_crosses() {
        let dir = aterm_tempfile::tempdir().expect("owned proof directory");
        let proof = dir.path().join("child");
        let path = proof.clone();
        let began = Instant::now();
        let deadline = began + CLEANUP;
        super::super::set_deadline_clock_for_test(Some(Box::new(move || {
            if witness(&path).is_some() || began.elapsed() >= CLEANUP {
                deadline + Duration::from_secs(1)
            } else {
                began
            }
        })));
        struct Clock;
        impl Drop for Clock {
            fn drop(&mut self) {
                super::super::set_deadline_clock_for_test(None);
            }
        }
        let clock = Clock;
        let error = output_with_input_until(
            &mut fixture("nonreader", &proof),
            Secret::new(&[b'x'; 256 * 1024]),
            deadline,
            16 * 1024,
        )
        .expect_err("a full stdin cannot block the deadline");
        drop(clock);
        let (pid, birth) = witness(&proof).expect("child ran before its deadline crossed");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_ne!(crate::process::read_birth(pid), Some(birth));
    }

    fn cmd(script: &str) -> Command {
        let root = std::env::var_os("SystemRoot").expect("Windows SystemRoot");
        let mut command =
            crate::spawnfd::command(std::path::Path::new(&root).join("System32").join("cmd.exe"));
        command.args(["/D", "/S", "/C", script]);
        command
    }

    #[test]
    fn the_parent_pipe_does_not_wait_for_input_or_capacity() {
        let (mut reader, client) = pipe(false).expect("read pipe");
        let mut byte = [0_u8; 1];
        let error = reader
            .read(&mut byte)
            .expect_err("an empty nonblocking pipe");
        assert_eq!(error.raw_os_error(), Some(ERROR_NO_DATA));
        drop(client);
        match reader.read(&mut byte) {
            Ok(0) => {}
            Err(error)
                if error.kind() == io::ErrorKind::BrokenPipe
                    || error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED) => {}
            other => panic!("a closed writer must give EOF: {other:?}"),
        }

        let (mut writer, _client) = pipe(true).expect("write pipe");
        let block = [1_u8; 8192];
        let mut full = false;
        for _ in 0..128 {
            match writer.write(&block) {
                Ok(0) => {
                    full = true;
                    break;
                }
                Ok(_) => {}
                Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA) => {
                    full = true;
                    break;
                }
                Err(error) => panic!("pipe write: {error}"),
            }
        }
        assert!(
            full,
            "the unread pipe must report backpressure without a blocking write"
        );
    }

    #[test]
    fn an_expired_exchange_does_not_spawn_the_missing_program() {
        let mut command = crate::spawnfd::command("aterm-deliberately-missing-helper.exe");
        let error = output_with_input_until(
            &mut command,
            Secret::new(b"inert input"),
            Instant::now(),
            1024,
        )
        .expect_err("deadline before spawn");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_direct_exit_keeps_its_status_and_both_complete_streams() {
        let output = output_with_input_until(
            &mut cmd("echo output& echo diagnostic 1>&2& exit /B 67"),
            Secret::new(&[b'x'; 256 * 1024]),
            Instant::now() + CLEANUP,
            1024,
        )
        .expect("the child exits without consuming all input");
        assert_eq!(output.status.code(), Some(67));
        assert!(String::from_utf8_lossy(&output.stdout).contains("output"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic"));
    }

    #[test]
    fn either_complete_stream_refuses_overflow() {
        for script in ["echo too-long", "echo too-long 1>&2"] {
            let error = output_with_input_until(
                &mut cmd(script),
                Secret::new(b""),
                Instant::now() + CLEANUP,
                1,
            )
            .expect_err("complete output cannot be truncated into success");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        bytes: *mut u32,
    ) -> i32;
}
