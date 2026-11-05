// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Child process spawning with explicit descriptor inheritance ([`command`](crate::spawnfd::command)).

#![cfg(unix)]

#[cfg(all(
    target_os = "linux",
    not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    ))
))]
compile_error!("spawnfd's Linux arm is written for x86_64, aarch64 and riscv64 only");

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

/// The two descriptor numbers the fabric bridge inherits.
pub const BRIDGE_VERB_FD: RawFd = 3;
pub const BRIDGE_PUSH_FD: RawFd = 4;
/// A bounded guard set, including a source worker's capacity and source/cache lock custody.
pub const MAX_GUARD_FDS: usize = 8;

const _: () = assert!(BRIDGE_VERB_FD == 3 && BRIDGE_PUSH_FD == 4);

/// The composing executable's startup admission of the bridge's two inherited lanes, [`BRIDGE_VERB_FD`] then [`BRIDGE_PUSH_FD`], each by [`adopt_inherited_socket`].
///
/// # Safety
/// Each eligible lane at [`BRIDGE_VERB_FD`] and [`BRIDGE_PUSH_FD`] must have been transferred exclusively to this call: no Rust or foreign owner may retain it. No thread or callback may close or replace either lane during adoption. The call consumes each eligible lane, including on a later failure.
pub unsafe fn adopt_startup_bridge_lanes() -> io::Result<(OwnedFd, OwnedFd)> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the bridge's inherited lanes were already taken by this process",
        ));
    }
    // SAFETY: on the GUI route these are the launcher's unclaimed lanes; a hand-started process may have neither, which is a checked refusal. This startup route takes each eligible descriptor once (`TAKEN`) before bridge threads; neither has another Rust owner or concurrent close/replacement.
    let verb = unsafe { adopt_inherited_socket(BRIDGE_VERB_FD) }?;
    // SAFETY: the same startup ownership admission for the distinct push lane.
    let push = unsafe { adopt_inherited_socket(BRIDGE_PUSH_FD) }?;
    Ok((verb, push))
}

/// Adopt an inherited socket as a close-on-exec copy, closing the original number after duplication.
///
/// # Safety
/// The descriptor number must not be closed or replaced by another thread or callback during this call. If it names a socket eligible for adoption, no other owner may hold it: its ownership must have been given up to this call. Once taken, it is closed even if duplication fails.
pub unsafe fn adopt_inherited_socket(fd: RawFd) -> io::Result<OwnedFd> {
    use std::os::unix::fs::FileTypeExt as _;
    let meta = descriptor_metadata(fd).map_err(|e| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            format!("descriptor {fd} is not open ({e})"),
        )
    })?;
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("descriptor {fd} is not a socket"),
        ));
    }
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags < 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!(
                "descriptor {fd} is not open ({})",
                io::Error::last_os_error()
            ),
        ));
    }
    if flags & FD_CLOEXEC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "descriptor {fd} is close-on-exec: this process opened it, so it is not an \
                 inherited one to adopt"
            ),
        ));
    }
    // SAFETY: the caller admitted exclusive, stable ownership; the checks above only diagnose type/flags. This is the number's one owner, dropped immediately after its close-on-exec copy is made.
    let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
    crate::kept_out(|| inherited.try_clone())
}

/// What descriptor `fd` refers to, read from the descriptor itself.
pub fn descriptor_metadata(fd: RawFd) -> io::Result<std::fs::Metadata> {
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
    if unsafe { fcntl(fd, F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is open (just answered), and the `File` is never dropped (`ManuallyDrop`): it names the number for one `fstat`, which changes nothing. Closed by another thread in between, the `fstat` answers `EBADF`.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    file.metadata()
}

const FIRST_FREE: RawFd = BRIDGE_PUSH_FD + 1;

#[cfg(target_vendor = "apple")]
const ACTION_FD_LIMIT: RawFd = 10_240;

fn placeable(fd: RawFd) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        (FIRST_FREE..ACTION_FD_LIMIT).contains(&fd)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        fd >= FIRST_FREE
    }
}

/// One of a child's three stdio slots.
#[derive(Debug)]
pub enum ChildStdio {
    /// `/dev/null`: read-only as stdin, write-only as stdout or stderr.
    Null,
    /// This process's own descriptor at that number.
    Inherit,
    /// This descriptor, placed at that number.
    Fd(OwnedFd),
}

/// The program [`spawn`] and [`spawn_with_two_fds`] start: its argv, its environment and its stdio.
#[derive(Debug)]
pub struct LaneCommand {
    program: OsString,
    args: Vec<OsString>,
    env: Option<Vec<(OsString, OsString)>>,
    stdin: ChildStdio,
    stdout: ChildStdio,
    stderr: ChildStdio,
}

impl LaneCommand {
    /// `program` with no arguments, this process's environment, and stdio inherited.
    #[must_use]
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: None,
            stdin: ChildStdio::Inherit,
            stdout: ChildStdio::Inherit,
            stderr: ChildStdio::Inherit,
        }
    }

    /// Append one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Append arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// The child's whole environment (`env_clear` and `envs` in one call).
    pub fn env_exactly<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        for (k, v) in vars {
            let (k, v) = (k.as_ref(), v.as_ref().to_os_string());
            match env.iter_mut().find(|(key, _)| key.as_os_str() == k) {
                Some(slot) => slot.1 = v,
                None => env.push((k.to_os_string(), v)),
            }
        }
        self.env = Some(env);
        self
    }

    /// The child's stdin.
    pub fn stdin(&mut self, slot: ChildStdio) -> &mut Self {
        self.stdin = slot;
        self
    }

    /// The child's stdout.
    pub fn stdout(&mut self, slot: ChildStdio) -> &mut Self {
        self.stdout = slot;
        self
    }

    /// The child's stderr.
    pub fn stderr(&mut self, slot: ChildStdio) -> &mut Self {
        self.stderr = slot;
        self
    }
}

/// A child [`spawn_with_two_fds`] started: `std::process::Child`'s process half, by pid.
#[derive(Debug)]
pub struct LaneChild {
    pid: i32,
    status: Option<ExitStatus>,
}

impl LaneChild {
    /// The child's pid.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// Wait for the child to exit and reap it (once; see the type).
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.reap(0)? {
                return Ok(status);
            }
        }
    }

    /// The exit status if the child has exited (reaping it), else `None`.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.reap(WNOHANG)
    }

    /// `SIGKILL` the child.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: `kill` takes two integers; the pid is our own unreaped child, which nothing else can have reused.
        if unsafe { kill(self.pid, SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// `SIGTERM` the child, so it can pass the signal on; once it is reaped, signal nothing.
    pub fn terminate(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: as in `kill`: two integers, and our own unreaped child.
        if unsafe { kill(self.pid, SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// `SIGCONT` the child: a child started stopped ([`Command::start_suspended`]) runs.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: as in `kill`: two integers, and our own unreaped child.
        if unsafe { kill(self.pid, SIGCONT) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn reap(&mut self, options: i32) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        loop {
            let mut raw = 0;
            // SAFETY: `raw` is a live local `waitpid` writes; the pid is ours.
            let got = unsafe { waitpid(self.pid, &mut raw, options) };
            if got == self.pid {
                let status = ExitStatus::from_raw(raw);
                self.status = Some(status);
                return Ok(Some(status));
            }
            if got == 0 {
                return Ok(None);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

impl Drop for LaneChild {
    fn drop(&mut self) {
        if matches!(self.try_wait(), Ok(None)) {
            crate::reaper::hand_off_pid(self.pid);
        }
    }
}

/// Spawn `cmd` with `first` and `second` inherited at [`BRIDGE_VERB_FD`] and [`BRIDGE_PUSH_FD`], and nothing else above stdio.
///
/// A lane (or a stdio descriptor) made through [`crate::kept_out`] reaches this child all the
/// same: the spawn unmarks it for its own child alone, with no other spawn in flight.
pub fn spawn_with_two_fds(
    cmd: LaneCommand,
    first: OwnedFd,
    second: OwnedFd,
) -> io::Result<LaneChild> {
    let lanes = [off_the_targets(first)?, off_the_targets(second)?];
    launch(cmd, &lanes)
}

/// Spawn `cmd` DETACHED, holding `first` and `second` at [`BRIDGE_VERB_FD`] and [`BRIDGE_PUSH_FD`]
/// and nothing else above stdio, as [`spawn_with_two_fds`] does, in a session of its own
/// (`setsid`), so it leads a process group no terminal's job holds and no job's signal reaches:
/// a process meant to outlive the job that started it (a build's claim holder,
/// [`crate::build_front::HOLD_VERB`]). Its parent is still this process until it forks away.
///
/// # Errors
/// The spawn's; on Linux, `Unsupported` (the Linux lane spawn places stdio and lanes only).
pub fn detached_with_two_fds(
    cmd: LaneCommand,
    first: OwnedFd,
    second: OwnedFd,
) -> io::Result<LaneChild> {
    let lanes = [off_the_targets(first)?, off_the_targets(second)?];
    launch_placed(cmd, &lanes, true)
}

/// Spawn `cmd` holding its stdio and nothing else.
pub fn spawn(cmd: LaneCommand) -> io::Result<LaneChild> {
    launch(cmd, &[])
}

fn launch(cmd: LaneCommand, lanes: &[OwnedFd]) -> io::Result<LaneChild> {
    launch_placed(cmd, lanes, false)
}

fn launch_placed(cmd: LaneCommand, lanes: &[OwnedFd], setsid: bool) -> io::Result<LaneChild> {
    let LaneCommand {
        program,
        args,
        env,
        stdin,
        stdout,
        stderr,
    } = cmd;
    let env = env.unwrap_or_else(|| std::env::vars_os().collect());
    let image = Image::build(&program, &args, &env)?;
    let stdio = [stage(stdin)?, stage(stdout)?, stage(stderr)?];
    let pid = platform::spawn(
        &image,
        &Placement {
            stdio: [stdio[0].source(), stdio[1].source(), stdio[2].source()],
            lanes,
            setsid,
            ..Placement::default()
        },
    )?;
    Ok(LaneChild { pid, status: None })
}

fn stage(slot: ChildStdio) -> io::Result<ChildStdio> {
    match slot {
        ChildStdio::Fd(fd) => off_the_targets(fd).map(ChildStdio::Fd),
        other => Ok(other),
    }
}

impl ChildStdio {
    fn source(&self) -> Source {
        match self {
            Self::Null => Source::Null,
            Self::Inherit => Source::Inherit,
            Self::Fd(fd) => Source::Fd(fd.as_raw_fd()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Source {
    Null,
    Inherit,
    Fd(RawFd),
}

#[derive(Debug)]
struct Placement<'a> {
    stdio: [Source; 3],
    lanes: &'a [OwnedFd],
    /// Parent source and advertised child target, distinct for a guarded in-place exec.
    guards: &'a [(RawFd, RawFd)],
    cwd: Option<&'a std::ffi::CStr>,
    base: Option<&'a std::path::Path>,
    pgroup: Option<i32>,
    setsid: bool,
    replace: bool,
    long_entry_ends: bool,
    inherit_all: bool,
    #[cfg(target_vendor = "apple")]
    native_image: bool,
    disclaim: Option<&'a std::ffi::CStr>,
    suspended: bool,
    /// The task type the child starts as, in place of the one it inherits ([`TaskType`]).
    #[cfg(target_vendor = "apple")]
    task: Option<TaskType>,
}

/// The task type a child starts as (`posix_spawnattr_setprocesstype_np`) in place of the one
/// it inherits from this process: on macOS a task's type decides its band, so a child of a
/// launchd job of `ProcessType` `Background` (darwin-bg: priority 4 at every class) started as
/// another type leaves that band. Elsewhere a spawn has no task type and none is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskType {
    /// A standard daemon (`POSIX_SPAWN_PROC_TYPE_DAEMON_STANDARD`): no band, each thread at
    /// the class it asks for.
    Standard,
    /// A background daemon (`POSIX_SPAWN_PROC_TYPE_DAEMON_BACKGROUND`): the band launchd starts
    /// a `Background` job in, every thread at priority 4 ([`start_children_in_background_band`]).
    Background,
    /// An interactive daemon (`POSIX_SPAWN_PROC_TYPE_DAEMON_INTERACTIVE`): what launchd starts a
    /// job of `ProcessType` `Interactive` as, no band, each thread at the class it asks for, as a
    /// standard daemon's ([`Command::start_as_interactive_daemon`]).
    Interactive,
}

/// Whether every child this process starts from now on, but one started out of the band
/// ([`Command::leave_background_band`]), starts in the background band
/// ([`start_children_in_background_band`]).
#[cfg(target_vendor = "apple")]
static CHILDREN_IN_BAND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From now on start every child of this process in macOS's background band, as a child of a
/// launchd job of `ProcessType` `Background` starts (a background daemon task: every thread at
/// priority 4 whatever class it asks for), but one started with
/// [`Command::leave_background_band`]; an in-place exec keeps this process's own type. For a
/// process that left such a job's band to lift one thread, whose children stay where the job
/// ran them (the vault subscriber's converge runner). Elsewhere nothing changes.
pub fn start_children_in_background_band() {
    #[cfg(target_vendor = "apple")]
    CHILDREN_IN_BAND.store(true, std::sync::atomic::Ordering::Release);
}

/// The task type a spawn sets: `asked` ([`Command::leave_background_band`]), else the background
/// band for a child (not an in-place exec, `replace`) of a process that starts its children there
/// ([`start_children_in_background_band`]).
#[cfg(target_vendor = "apple")]
fn task_for(asked: Option<TaskType>, replace: bool) -> Option<TaskType> {
    asked.or_else(|| {
        (!replace && CHILDREN_IN_BAND.load(std::sync::atomic::Ordering::Acquire))
            .then_some(TaskType::Background)
    })
}

impl Default for Placement<'_> {
    fn default() -> Self {
        Self {
            stdio: [Source::Inherit; 3],
            lanes: &[],
            guards: &[],
            cwd: None,
            base: None,
            pgroup: None,
            setsid: false,
            replace: false,
            long_entry_ends: false,
            inherit_all: false,
            #[cfg(target_vendor = "apple")]
            native_image: false,
            disclaim: None,
            suspended: false,
            #[cfg(target_vendor = "apple")]
            task: None,
        }
    }
}

const DISCLAIM_SPI: &std::ffi::CStr = c"responsibility_spawnattrs_setdisclaim";

fn off_the_targets(fd: OwnedFd) -> io::Result<OwnedFd> {
    if placeable(fd.as_raw_fd()) {
        return Ok(fd);
    }
    placeable_copy(fd.as_raw_fd())
}

#[cfg(target_vendor = "apple")]
fn off_stdio(fd: OwnedFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    placeable_copy(fd.as_raw_fd())
}

/// A copy of `fd` above every number a spawn places, kept out of children from its creation
/// (a copy never carries its source's mark): the spawn it is made for hands it to its own child
/// alone (`spawn_lock::handing`).
fn placeable_copy(fd: RawFd) -> io::Result<OwnedFd> {
    let copy = crate::kept_out(|| {
        // SAFETY: `F_DUPFD_CLOEXEC` reads one descriptor number and returns a fresh descriptor or -1; it touches no memory.
        let copy = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, FIRST_FREE) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `copy` was just created here and nothing else owns it.
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    })?;
    if !placeable(copy.as_raw_fd()) {
        return Err(io::Error::from_raw_os_error(EMFILE));
    }
    Ok(copy)
}

struct Image {
    program: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    search: Option<OsString>,
}

/// Strings and pointer arrays whose addresses remain stable when the owner is moved.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
struct ExecImage {
    _image: Image,
    path: CString,
    argv: Vec<*const core::ffi::c_char>,
    envp: Vec<*const core::ffi::c_char>,
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
impl ExecImage {
    fn new(image: Image, path: CString) -> Self {
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        Self {
            _image: image,
            path,
            argv,
            envp,
        }
    }
}

/// Read the same absolute microsecond clock the native-operation contract transmits.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn exec_now_us() -> io::Result<u64> {
    let mut now = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: the successful call initializes one writable timespec.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, now.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: clock_gettime succeeded and initialized the complete value.
    let now = unsafe { now.assume_init() };
    let valid = u64::try_from(now.tv_sec)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .zip(
            u64::try_from(now.tv_nsec)
                .ok()
                .filter(|&ns| ns < 1_000_000_000),
        )
        .and_then(|(seconds, nanos)| seconds.checked_add(nanos / 1000));
    valid.ok_or_else(|| io::Error::other("invalid native exec clock"))
}

/// Enter the actual dispatch gate before checking the original deadline. Nothing between a
/// successful check and `dispatch` allocates, resolves paths, or waits for another lock.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn exec_dispatch<G>(
    enter: impl FnOnce() -> G,
    deadline_us: u64,
    now: impl FnOnce() -> io::Result<u64>,
    dispatch: impl FnOnce() -> io::Error,
) -> io::Error {
    let _gate = enter();
    match now() {
        Ok(now) if now < deadline_us => dispatch(),
        Ok(_) => io::Error::new(io::ErrorKind::TimedOut, "native exec deadline elapsed"),
        Err(error) => error,
    }
}

const BSHELL: &core::ffi::CStr = c"/bin/sh";
const ENOEXEC: i32 = 8;

impl Image {
    fn build(program: &OsStr, args: &[OsString], env: &[(OsString, OsString)]) -> io::Result<Self> {
        let search = env
            .iter()
            .find(|(k, _)| k.as_bytes() == b"PATH")
            .map(|(_, v)| v.clone());
        let program = c_string(program.as_bytes())?;
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(program.clone());
        for arg in args {
            argv.push(c_string(arg.as_bytes())?);
        }
        let mut envp = Vec::with_capacity(env.len());
        for (k, v) in env {
            let mut kv = Vec::with_capacity(k.len() + v.len() + 1);
            kv.extend_from_slice(k.as_bytes());
            kv.push(b'=');
            kv.extend_from_slice(v.as_bytes());
            envp.push(c_string(&kv)?);
        }
        Ok(Self {
            program,
            argv,
            envp,
            search,
        })
    }

    fn script(&self, path: &CString) -> Vec<CString> {
        let mut script = Vec::with_capacity(self.argv.len() + 1);
        script.push(c"sh".to_owned());
        script.push(path.clone());
        script.extend(self.argv.iter().skip(1).cloned());
        script
    }

    fn pointers(strings: &[CString]) -> Vec<*const core::ffi::c_char> {
        strings
            .iter()
            .map(|s| s.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    }
}

fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "nul byte found in provided data",
        )
    })
}

#[cfg(not(target_vendor = "apple"))]
fn resolve(program: &OsStr, path_var: Option<&OsStr>) -> io::Result<CString> {
    if program.as_bytes().contains(&b'/') {
        return c_string(program.as_bytes());
    }
    if program.is_empty() {
        return Err(io::Error::from_raw_os_error(ENOENT));
    }
    let search = path_var.unwrap_or_else(|| OsStr::new("/usr/bin:/bin"));
    let mut denied = false;
    for dir in std::env::split_paths(search) {
        let dir = if dir.as_os_str().is_empty() {
            std::path::PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(program);
        if !std::fs::metadata(&candidate).is_ok_and(|md| md.is_file()) {
            continue;
        }
        let candidate = c_string(candidate.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path; `access` reads it and retains nothing.
        if unsafe { access(candidate.as_ptr(), X_OK) } == 0 {
            return Ok(candidate);
        }
        denied = true;
    }
    Err(io::Error::from_raw_os_error(if denied {
        EACCES
    } else {
        ENOENT
    }))
}

#[cfg(target_vendor = "apple")]
mod platform {
    use super::{
        BSHELL, ENOEXEC, ExecImage, F_GETFD, Image, Placement, Source, X_OK, access, c_string,
        exec_dispatch, exec_now_us, fcntl,
    };
    use core::ffi::{CStr, c_char, c_void};
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    type Attr = *mut c_void;
    type Actions = *mut c_void;

    const POSIX_SPAWN_SETPGROUP: i16 = 0x0002;
    const POSIX_SPAWN_SETSIGDEF: i16 = 0x0004;
    const POSIX_SPAWN_SETSIGMASK: i16 = 0x0008;
    const POSIX_SPAWN_SETEXEC: i16 = 0x0040;
    const POSIX_SPAWN_START_SUSPENDED: i16 = 0x0080;
    const POSIX_SPAWN_SETSID: i16 = 0x0400;
    const POSIX_SPAWN_CLOEXEC_DEFAULT: i16 = 0x4000;
    /// `<spawn_private.h>`'s `POSIX_SPAWN_PROC_TYPE_DAEMON_STANDARD`.
    const PROC_TYPE_DAEMON_STANDARD: i32 = 0x300;
    /// `<spawn_private.h>`'s `POSIX_SPAWN_PROC_TYPE_DAEMON_INTERACTIVE`.
    const PROC_TYPE_DAEMON_INTERACTIVE: i32 = 0x400;
    /// `<spawn_private.h>`'s `POSIX_SPAWN_PROC_TYPE_DAEMON_BACKGROUND`.
    const PROC_TYPE_DAEMON_BACKGROUND: i32 = 0x500;
    const O_RDONLY: i32 = 0;
    const O_WRONLY: i32 = 1;
    const SIGPIPE_BIT: u32 = 1 << (13 - 1);
    const SIGHUP_BIT: u32 = 1;
    const PATH_MAX: usize = 1024;
    const ENOENT: i32 = 2;
    const E2BIG: i32 = 7;
    const ENOMEM: i32 = 12;
    const EACCES: i32 = 13;
    const ENOTDIR: i32 = 20;
    const ETXTBSY: i32 = 26;
    const ELOOP: i32 = 62;
    const ENAMETOOLONG: i32 = 63;
    const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;

    type SetDisclaim = unsafe extern "C" fn(*mut Attr, i32) -> i32;

    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn posix_spawnattr_init(attr: *mut Attr) -> i32;
        fn posix_spawnattr_destroy(attr: *mut Attr) -> i32;
        fn posix_spawnattr_setflags(attr: *mut Attr, flags: i16) -> i32;
        fn posix_spawnattr_setsigmask(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawnattr_setsigdefault(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawnattr_setpgroup(attr: *mut Attr, pgroup: i32) -> i32;
        fn posix_spawnattr_setprocesstype_np(attr: *mut Attr, kind: i32) -> i32;
        fn posix_spawn_file_actions_init(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_destroy(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_addopen(
            actions: *mut Actions,
            fd: i32,
            path: *const c_char,
            oflag: i32,
            mode: u16,
        ) -> i32;
        fn posix_spawn_file_actions_adddup2(actions: *mut Actions, fd: i32, new: i32) -> i32;
        fn posix_spawn_file_actions_addinherit_np(actions: *mut Actions, fd: i32) -> i32;
        fn posix_spawn_file_actions_addchdir_np(actions: *mut Actions, path: *const c_char) -> i32;
        fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            actions: *const Actions,
            attr: *const Attr,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> i32;
    }

    struct Objects {
        attr: Attr,
        actions: Actions,
        /// Each descriptor a `dup2` action hands the child, which the spawn unmarks for it alone
        /// (`crate::spawn_lock::handing`).
        handed: Vec<i32>,
    }

    impl Objects {
        fn new() -> io::Result<Self> {
            let mut attr: Attr = std::ptr::null_mut();
            // SAFETY: init writes a fresh attribute object into `attr`.
            check(unsafe { posix_spawnattr_init(&mut attr) })?;
            let mut actions: Actions = std::ptr::null_mut();
            // SAFETY: init writes a fresh file-actions object into `actions`.
            if let Err(e) = check(unsafe { posix_spawn_file_actions_init(&mut actions) }) {
                // SAFETY: `attr` was initialised above and is destroyed once.
                unsafe { posix_spawnattr_destroy(&mut attr) };
                return Err(e);
            }
            Ok(Self {
                attr,
                actions,
                handed: Vec::new(),
            })
        }

        fn spawn(
            &self,
            path: &CStr,
            argv: &[*const c_char],
            envp: &[*const c_char],
        ) -> Result<i32, i32> {
            let _spawning = crate::spawn_lock::handing(&self.handed);
            self.spawn_unguarded(path, argv, envp)
        }

        /// The caller holds the spawn lock through the raw native dispatch.
        fn spawn_unguarded(
            &self,
            path: &CStr,
            argv: &[*const c_char],
            envp: &[*const c_char],
        ) -> Result<i32, i32> {
            let mut pid = 0;
            // SAFETY: both objects were initialised by `new`; `path` is a C string and `argv`/`envp` NULL-terminated arrays of C strings, all of which outlive the call and none of which it retains.
            let rc = unsafe {
                posix_spawn(
                    &mut pid,
                    path.as_ptr(),
                    &self.actions,
                    &self.attr,
                    argv.as_ptr(),
                    envp.as_ptr(),
                )
            };
            if rc == 0 { Ok(pid) } else { Err(rc) }
        }
    }

    impl Drop for Objects {
        fn drop(&mut self) {
            // SAFETY: both were initialised by `new` and are destroyed once.
            unsafe {
                posix_spawn_file_actions_destroy(&mut self.actions);
                posix_spawnattr_destroy(&mut self.attr);
            }
        }
    }

    fn check(rc: i32) -> io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    }

    pub(super) struct PreparedExec {
        image: ExecImage,
        objects: Objects,
    }

    impl PreparedExec {
        pub(super) fn new(image: Image) -> io::Result<Self> {
            if image.program.as_bytes().is_empty() {
                return Err(io::Error::from_raw_os_error(ENOENT));
            }
            let path = if image.program.as_bytes().contains(&b'/') {
                image.program.clone()
            } else {
                let mut denied = false;
                let mut found = None;
                for candidate in candidates(
                    image.program.as_bytes(),
                    image.search.as_deref(),
                    None,
                    true,
                ) {
                    let candidate = candidate?;
                    if !exists(&candidate) {
                        continue;
                    }
                    match runnable(&candidate) {
                        Ok(()) => {
                            found = Some(candidate);
                            break;
                        }
                        Err(error) => denied |= error.raw_os_error() == Some(EACCES),
                    }
                }
                found.ok_or_else(|| {
                    io::Error::from_raw_os_error(if denied { EACCES } else { ENOENT })
                })?
            };
            let mut objects = Objects::new()?;
            let defaults = SIGPIPE_BIT;
            // SAFETY: initialized, exclusively owned attributes; the setters copy their
            // scalar arguments. There are no descriptor actions or signal-mask changes.
            unsafe {
                check(posix_spawnattr_setflags(
                    &mut objects.attr,
                    POSIX_SPAWN_SETEXEC | POSIX_SPAWN_SETSIGDEF,
                ))?;
                check(posix_spawnattr_setsigdefault(&mut objects.attr, &defaults))?;
            }
            Ok(Self {
                image: ExecImage::new(image, path),
                objects,
            })
        }

        pub(super) fn exec_until(self, deadline_us: u64) -> io::Error {
            self.exec_with(deadline_us, exec_now_us, |prepared| {
                let image = &prepared.image;
                match prepared
                    .objects
                    .spawn_unguarded(&image.path, &image.argv, &image.envp)
                {
                    Ok(_) => io::Error::other("the in-place exec returned a child"),
                    Err(error) => io::Error::from_raw_os_error(error),
                }
            })
        }

        pub(super) fn exec_with(
            self,
            deadline_us: u64,
            now: impl FnOnce() -> io::Result<u64>,
            dispatch: impl FnOnce(&Self) -> io::Error,
        ) -> io::Error {
            exec_dispatch(crate::spawn_lock::spawning, deadline_us, now, || {
                dispatch(&self)
            })
        }

        #[cfg(test)]
        pub(super) fn captured(&self) -> (&Image, &CStr) {
            (&self.image._image, &self.image.path)
        }
    }

    fn disclaimer(symbol: &CStr) -> io::Result<SetDisclaim> {
        // SAFETY: `symbol` is a NUL-terminated C string that outlives the call; `RTLD_DEFAULT` is dlfcn.h's pseudo-handle for a global search.
        let found = unsafe { dlsym(RTLD_DEFAULT, symbol.as_ptr()) };
        if found.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "this macOS exports no {}: a child asked to be its own responsible \
                     process is not started without it",
                    symbol.to_string_lossy()
                ),
            ));
        }
        // SAFETY: dyld resolved the name to a function, and the one this module names, `responsibility_spawnattrs_setdisclaim`, has the C prototype `int f(posix_spawnattr_t *, int)` — exactly `SetDisclaim` (a test names a symbol that resolves to nothing, never another function).
        Ok(unsafe { std::mem::transmute::<*mut c_void, SetDisclaim>(found) })
    }

    /// `posix_spawn`, with `POSIX_SPAWN_CLOEXEC_DEFAULT` unless `place.inherit_all`.
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        let disclaim = place.disclaim.map(disclaimer).transpose()?;
        let mut objects = Objects::new()?;
        let empty: u32 = 0;
        let defaults: u32 = if place.suspended {
            SIGPIPE_BIT | SIGHUP_BIT
        } else {
            SIGPIPE_BIT
        };
        let mut flags = POSIX_SPAWN_SETSIGDEF;
        if !place.inherit_all {
            flags |= POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGMASK;
        }
        if place.pgroup.is_some() {
            flags |= POSIX_SPAWN_SETPGROUP;
        }
        if place.setsid {
            flags |= POSIX_SPAWN_SETSID;
        }
        if place.replace {
            flags |= POSIX_SPAWN_SETEXEC;
        }
        if place.suspended {
            flags |= POSIX_SPAWN_START_SUSPENDED;
        }
        // SAFETY: each call takes an object `Objects::new` initialised and a live local or C string; none retains a pointer past the call. The disclaim setter is the SPI `disclaimer` resolved, called with its own prototype on that same attribute object.
        unsafe {
            check(posix_spawnattr_setflags(&mut objects.attr, flags))?;
            if !place.inherit_all {
                check(posix_spawnattr_setsigmask(&mut objects.attr, &empty))?;
            }
            check(posix_spawnattr_setsigdefault(&mut objects.attr, &defaults))?;
            if let Some(pgroup) = place.pgroup {
                check(posix_spawnattr_setpgroup(&mut objects.attr, pgroup))?;
            }
            if let Some(set_disclaim) = disclaim {
                check(set_disclaim(&mut objects.attr, 1))?;
            }
            if let Some(task) = super::task_for(place.task, place.replace) {
                let kind = match task {
                    super::TaskType::Standard => PROC_TYPE_DAEMON_STANDARD,
                    super::TaskType::Background => PROC_TYPE_DAEMON_BACKGROUND,
                    super::TaskType::Interactive => PROC_TYPE_DAEMON_INTERACTIVE,
                };
                check(posix_spawnattr_setprocesstype_np(&mut objects.attr, kind))?;
            }
            for (target, slot) in (0..).zip(place.stdio) {
                let null_mode = if target == 0 { O_RDONLY } else { O_WRONLY };
                let open_null = |actions: *mut Actions| {
                    posix_spawn_file_actions_addopen(
                        actions,
                        target,
                        c"/dev/null".as_ptr(),
                        null_mode,
                        0,
                    )
                };
                check(match slot {
                    Source::Null => open_null(&mut objects.actions),
                    Source::Inherit if place.inherit_all => 0,
                    Source::Inherit if fcntl(target, F_GETFD) >= 0 => {
                        posix_spawn_file_actions_addinherit_np(&mut objects.actions, target)
                    }
                    Source::Inherit => open_null(&mut objects.actions),
                    Source::Fd(fd) => {
                        objects.handed.push(fd);
                        posix_spawn_file_actions_adddup2(&mut objects.actions, fd, target)
                    }
                })?;
            }
            for (target, lane) in (super::BRIDGE_VERB_FD..).zip(place.lanes) {
                objects.handed.push(lane.as_raw_fd());
                check(posix_spawn_file_actions_adddup2(
                    &mut objects.actions,
                    lane.as_raw_fd(),
                    target,
                ))?;
            }
            for &(source, target) in place.guards {
                objects.handed.push(source);
                check(if source == target {
                    posix_spawn_file_actions_addinherit_np(&mut objects.actions, target)
                } else {
                    posix_spawn_file_actions_adddup2(&mut objects.actions, source, target)
                })?;
            }
            if let Some(cwd) = place.cwd {
                check(posix_spawn_file_actions_addchdir_np(
                    &mut objects.actions,
                    cwd.as_ptr(),
                ))?;
            }
        }
        spawnp(image, place, &objects, &argv, &envp)
    }

    fn spawnp(
        image: &Image,
        place: &Placement<'_>,
        objects: &Objects,
        argv: &[*const c_char],
        envp: &[*const c_char],
    ) -> io::Result<i32> {
        let as_script = |candidate: &CString| {
            let script = image.script(candidate);
            let script = Image::pointers(&script);
            objects
                .spawn(BSHELL, &script, envp)
                .map_err(io::Error::from_raw_os_error)
        };
        let program = image.program.as_bytes();
        if program.contains(&b'/') {
            let path = rebase(program, place.base)?;
            return match objects.spawn(&path, argv, envp) {
                Ok(pid) => Ok(pid),
                Err(ENOEXEC) if !place.native_image => as_script(&path),
                Err(rc) => Err(io::Error::from_raw_os_error(rc)),
            };
        }
        if program.is_empty() {
            return Err(io::Error::from_raw_os_error(ENOENT));
        }
        let mut eacces = false;
        for candidate in candidates(
            program,
            image.search.as_deref(),
            place.base,
            place.long_entry_ends,
        ) {
            let candidate = candidate?;
            if !exists(&candidate) {
                continue;
            }
            let rc = match objects.spawn(&candidate, argv, envp) {
                Ok(pid) => return Ok(pid),
                Err(rc) => rc,
            };
            match rc {
                ELOOP | ENAMETOOLONG | ENOENT | ENOTDIR => {}
                ENOEXEC if !place.native_image => return as_script(&candidate),
                E2BIG | ENOMEM | ETXTBSY => return Err(io::Error::from_raw_os_error(rc)),
                _ if !exists(&candidate) => {}
                EACCES => eacces = true,
                _ => return Err(io::Error::from_raw_os_error(rc)),
            }
        }
        Err(io::Error::from_raw_os_error(if eacces {
            EACCES
        } else {
            ENOENT
        }))
    }

    /// Whether [`spawnp`] would find `program` something to run, judged before the spawn of
    /// another program that runs it later (a tethered child's tether, [`crate::tether`]): the
    /// same search over the same candidates, each one runnable when `access(X_OK)` passes and it
    /// is no directory.
    ///
    /// # Errors
    /// What that spawn would answer when no candidate is runnable: `access`'s error for a
    /// `program` that names a path, else `ENOENT`, `EACCES` when a candidate that exists may not
    /// be run, or `ENAMETOOLONG` for an entry the search ends at. An image the exec refuses
    /// although `access` passes it fails later, in the program that runs it.
    pub(super) fn findable(
        program: &[u8],
        search: Option<&OsStr>,
        base: Option<&Path>,
        long_entry_ends: bool,
    ) -> io::Result<()> {
        if program.contains(&b'/') {
            return runnable(&rebase(program, base)?);
        }
        if program.is_empty() {
            return Err(io::Error::from_raw_os_error(ENOENT));
        }
        let mut eacces = false;
        for candidate in candidates(program, search, base, long_entry_ends) {
            let candidate = candidate?;
            if !exists(&candidate) {
                continue;
            }
            match runnable(&candidate) {
                Ok(()) => return Ok(()),
                Err(e) => eacces |= e.raw_os_error() == Some(EACCES),
            }
        }
        Err(io::Error::from_raw_os_error(if eacces {
            EACCES
        } else {
            ENOENT
        }))
    }

    /// The paths a search for the bare name `program` tries, in order: each entry of `search`
    /// (the child's `PATH`, else `/usr/bin:/bin`; an empty entry is `.`) joined to it and rebased
    /// on `base`. An entry too long to join ends the search with `ENAMETOOLONG` when
    /// `long_entry_ends` (posix_spawnp's rule for this process's own `PATH`), and is passed over
    /// otherwise.
    fn candidates<'a>(
        program: &'a [u8],
        search: Option<&'a OsStr>,
        base: Option<&'a Path>,
        long_entry_ends: bool,
    ) -> impl Iterator<Item = io::Result<CString>> + 'a {
        search
            .map_or(&b"/usr/bin:/bin"[..], OsStr::as_bytes)
            .split(|&b| b == b':')
            .filter_map(move |dir| {
                let dir: &[u8] = if dir.is_empty() { b"." } else { dir };
                if dir.len() + program.len() + 2 > PATH_MAX {
                    return long_entry_ends
                        .then(|| Err(io::Error::from_raw_os_error(ENAMETOOLONG)));
                }
                let mut full = Vec::with_capacity(dir.len() + program.len() + 1);
                full.extend_from_slice(dir);
                full.push(b'/');
                full.extend_from_slice(program);
                Some(rebase(&full, base))
            })
    }

    /// Whether something is at `path`, a link followed.
    fn exists(path: &CStr) -> bool {
        std::fs::metadata(OsStr::from_bytes(path.to_bytes())).is_ok()
    }

    /// Whether the exec could run the file at `path`: `access(X_OK)`, and no directory.
    fn runnable(path: &CStr) -> io::Result<()> {
        // SAFETY: a NUL-terminated path; `access` reads it and retains nothing.
        if unsafe { access(path.as_ptr(), X_OK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if std::fs::metadata(OsStr::from_bytes(path.to_bytes()))?.is_dir() {
            return Err(io::Error::from_raw_os_error(EACCES));
        }
        Ok(())
    }

    fn rebase(path: &[u8], base: Option<&Path>) -> io::Result<CString> {
        match base {
            Some(base) if path.first() != Some(&b'/') => {
                let base = base.as_os_str().as_bytes();
                let mut full = Vec::with_capacity(base.len() + path.len() + 1);
                full.extend_from_slice(base);
                full.push(b'/');
                full.extend_from_slice(path);
                c_string(&full)
            }
            _ => c_string(path),
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{
        ExecImage, F_GETFD, F_SETFD, Image, InheritStrip, Placement, Source, exec_dispatch,
        exec_now_us, fcntl, off_the_targets, resolve, waitpid,
    };
    use core::ffi::{c_char, c_void};
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    const O_RDWR: i32 = 2;
    const O_CLOEXEC: i32 = 0o2_000_000;
    const SIG_SETMASK: i32 = 2;
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    const INHERIT: RawFd = -1;

    unsafe extern "C" {
        fn fork() -> i32;
        fn execve(
            path: *const c_char,
            argv: *const *const c_char,
            envp: *const *const c_char,
        ) -> i32;
        fn pipe2(fds: *mut RawFd, flags: i32) -> i32;
        fn open(path: *const c_char, flags: i32, ...) -> RawFd;
        fn read(fd: RawFd, buf: *mut c_void, count: usize) -> isize;
        fn write(fd: RawFd, buf: *const c_void, count: usize) -> isize;
        fn dup2(old: RawFd, new: RawFd) -> RawFd;
        fn sigprocmask(how: i32, set: *const u64, old: *mut u64) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn _exit(code: i32) -> !;
    }

    pub(super) struct PreparedExec {
        image: ExecImage,
    }

    impl PreparedExec {
        pub(super) fn new(image: Image) -> io::Result<Self> {
            let path = resolve(
                std::ffi::OsStr::from_bytes(image.program.as_bytes()),
                image.search.as_deref(),
            )?;
            Ok(Self {
                image: ExecImage::new(image, path),
            })
        }

        pub(super) fn exec_until(self, deadline_us: u64) -> io::Error {
            self.exec_with(deadline_us, exec_now_us, |prepared| {
                let image = &prepared.image;
                // SAFETY: preparation owns every C string and null-terminated pointer array
                // through this native dispatch; the kernel retains none on a returned error.
                unsafe {
                    execve(
                        image.path.as_ptr(),
                        image.argv.as_ptr(),
                        image.envp.as_ptr(),
                    )
                };
                io::Error::last_os_error()
            })
        }

        pub(super) fn exec_with(
            self,
            deadline_us: u64,
            now: impl FnOnce() -> io::Result<u64>,
            dispatch: impl FnOnce(&Self) -> io::Error,
        ) -> io::Error {
            with_default_sigpipe(|| exec_dispatch(|| (), deadline_us, now, || dispatch(&self)))
        }

        #[cfg(test)]
        pub(super) fn captured(&self) -> (&Image, &std::ffi::CStr) {
            (&self.image._image, &self.image.path)
        }
    }

    /// An in-place transparent exec: no fork, descriptor strip, mask clear or process-group
    /// change. Prepare every allocation before changing SIGPIPE, and restore the complete
    /// action if the exact native exec fails. The calling constructor is single-threaded.
    pub(super) fn exec(image: &Image) -> io::Error {
        let path = match resolve(
            std::ffi::OsStr::from_bytes(image.program.as_bytes()),
            image.search.as_deref(),
        ) {
            Ok(path) => path,
            Err(error) => return error,
        };
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        with_default_sigpipe(|| {
            // SAFETY: all C strings and pointer vectors remain owned by `image`/locals through
            // the call. No FD, mask, cwd, group or environment is modified before exec.
            unsafe { execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
            io::Error::last_os_error()
        })
    }

    /// Set only SIGPIPE's action for dispatch, restoring the full prior action on every
    /// returned outcome, including deadline/clock refusals before the native call.
    fn with_default_sigpipe(dispatch: impl FnOnce() -> io::Error) -> io::Error {
        // SAFETY: zero initializes plain signal-action storage and an empty signal set.
        let mut defaults: libc::sigaction = unsafe { std::mem::zeroed() };
        defaults.sa_sigaction = libc::SIG_DFL;
        let mut old = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: one valid action and one writable old-action slot; neither pointer escapes.
        if unsafe { libc::sigaction(libc::SIGPIPE, &defaults, old.as_mut_ptr()) } != 0 {
            return io::Error::last_os_error();
        }
        // SAFETY: sigaction succeeded and initialized the entire old action.
        let old = unsafe { old.assume_init() };
        let refused = dispatch();
        // SAFETY: the exact successfully captured old action is still live. A constructor
        // treats a failed restore as terminal rather than resuming work with altered state.
        if unsafe { libc::sigaction(libc::SIGPIPE, &old, std::ptr::null_mut()) } != 0 {
            return io::Error::other(format!(
                "exec failed ({refused}); restoring SIGPIPE failed ({})",
                io::Error::last_os_error()
            ));
        }
        refused
    }

    /// Fork, strip, place, exec, the child calling only async-signal-safe functions on memory prepared here.
    pub(super) fn spawn(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        super::past_text_busy(
            super::TEXT_BUSY_FOR,
            || spawn_once(image, place),
            super::text_busy_pause,
        )
    }

    fn spawn_once(image: &Image, place: &Placement<'_>) -> io::Result<i32> {
        if place.cwd.is_some()
            || place.base.is_some()
            || place.pgroup.is_some()
            || place.setsid
            || place.replace
            || place.long_entry_ends
            || place.disclaim.is_some()
            || place.suspended
            || !place.guards.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the Linux lane spawn places stdio and lanes only",
            ));
        }
        let (stdio, lanes) = (&place.stdio, place.lanes);
        let path = resolve(
            std::ffi::OsStr::from_bytes(image.program.as_bytes()),
            image.search.as_deref(),
        )?;
        let script = image.script(&path);
        let argv = Image::pointers(&image.argv);
        let envp = Image::pointers(&image.envp);
        let script = Image::pointers(&script);
        let strip = (!place.inherit_all).then(InheritStrip::prepare);
        // SAFETY: a C string literal; the result is a fresh descriptor or -1.
        let null = unsafe { open(c"/dev/null".as_ptr(), O_RDWR | O_CLOEXEC) };
        if null < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened here, owned by nothing else.
        let null = off_the_targets(unsafe { OwnedFd::from_raw_fd(null) })?;
        let mut ends: [RawFd; 2] = [-1, -1];
        // SAFETY: `pipe2` fills the two-element array it is handed.
        if unsafe { pipe2(ends.as_mut_ptr(), O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both ends were just created here and nothing else owns them.
        let (status_rd, status_wr) =
            unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        let status_wr = off_the_targets(status_wr)?;
        let stdio_fds = stdio.iter().map(|slot| match *slot {
            Source::Null => null.as_raw_fd(),
            Source::Inherit => INHERIT,
            Source::Fd(fd) => fd,
        });
        let mut sources = [INHERIT; 5];
        let mut placed = 0;
        for (source, from) in sources
            .iter_mut()
            .zip(stdio_fds.chain(lanes.iter().map(AsRawFd::as_raw_fd)))
        {
            *source = from;
            placed += 1;
        }
        let (null_fd, report) = (null.as_raw_fd(), status_wr.as_raw_fd());
        let exec = Exec {
            path: &path,
            argv: &argv,
            envp: &envp,
            script: &script,
        };
        // SAFETY: `fork` takes no arguments. The child branch never returns.
        let pid = unsafe { fork() };
        if pid == 0 {
            // SAFETY: the forked child; see `child`.
            unsafe { child(strip, &sources, placed, null_fd, report, &exec) }
        }
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        drop(status_wr);
        let mut word = [0u8; 4];
        let mut got = 0;
        while got < word.len() {
            let rest = &mut word[got..];
            // SAFETY: `rest` is a live, writable slice of `word`.
            let n = unsafe { read(status_rd.as_raw_fd(), rest.as_mut_ptr().cast(), rest.len()) };
            if n == 0 {
                break;
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            got += n.unsigned_abs();
        }
        if got == 0 {
            return Ok(pid);
        }
        let mut raw = 0;
        // SAFETY: our own child; `raw` is a live local.
        while unsafe { waitpid(pid, &mut raw, 0) } < 0
            && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
        {}
        Err(if got == word.len() {
            io::Error::from_raw_os_error(i32::from_be_bytes(word))
        } else {
            io::Error::other("the child's exec-status report was cut short")
        })
    }

    struct Exec<'a> {
        path: &'a std::ffi::CStr,
        argv: &'a [*const c_char],
        envp: &'a [*const c_char],
        script: &'a [*const c_char],
    }

    /// # Safety
    /// Only between `fork` and `exec`, with `sources[..placed]` at 5 or above (or `INHERIT`), `null` and `report` open, and `exec`'s pointers live.
    unsafe fn child(
        strip: Option<InheritStrip>,
        sources: &[RawFd; 5],
        placed: usize,
        null: RawFd,
        report: RawFd,
        exec: &Exec<'_>,
    ) -> ! {
        // SAFETY (whole body): the function's contract.
        unsafe {
            let transparent = strip.is_none();
            if let Some(strip) = strip {
                strip.apply();
            }
            for (target, &source) in (0..).zip(sources.iter().take(placed)) {
                let placed = if source != INHERIT {
                    dup2(source, target)
                } else if transparent {
                    0
                } else if fcntl(target, F_GETFD) < 0 {
                    dup2(null, target)
                } else {
                    fcntl(target, F_SETFD, 0)
                };
                if placed < 0 {
                    fail(report);
                }
            }
            if !transparent {
                let empty = [0u64; 16];
                sigprocmask(SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut());
            }
            signal(SIGPIPE, SIG_DFL);
            execve(exec.path.as_ptr(), exec.argv.as_ptr(), exec.envp.as_ptr());
            if io::Error::last_os_error().raw_os_error() == Some(super::ENOEXEC) {
                execve(
                    super::BSHELL.as_ptr(),
                    exec.script.as_ptr(),
                    exec.envp.as_ptr(),
                );
            }
            fail(report)
        }
    }

    /// # Safety
    /// Only in the forked child.
    unsafe fn fail(report: RawFd) -> ! {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let word = errno.to_be_bytes();
        // SAFETY: `write` and `_exit` are async-signal-safe; `word` is live.
        unsafe {
            write(report, word.as_ptr().cast(), word.len());
            _exit(127)
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
mod platform {
    use super::{Image, Placement};
    use std::io;

    pub(super) fn spawn(_image: &Image, _place: &Placement<'_>) -> io::Result<i32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "spawnfd's spawn is built for Darwin and Linux",
        ))
    }

    pub(super) struct PreparedExec;

    impl PreparedExec {
        pub(super) fn new(_image: Image) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "in-place exec is unavailable",
            ))
        }

        pub(super) fn exec_until(self, _deadline_us: u64) -> io::Error {
            io::Error::new(io::ErrorKind::Unsupported, "in-place exec is unavailable")
        }
    }
}

/// The strip, for a spawn that must fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritStrip {
    bound: RawFd,
}

impl InheritStrip {
    /// Read [`descriptor_bound`] in the parent, before the fork, and pass libnotify's once-gate.
    #[must_use]
    pub fn prepare() -> Self {
        pass_libnotify_gate();
        Self {
            bound: descriptor_bound(),
        }
    }

    /// How far a marking loop reaches (exclusive).
    #[must_use]
    pub const fn bound(self) -> RawFd {
        self.bound
    }

    /// Mark every descriptor from 3 up close-on-exec.
    pub fn apply(self) {
        #[cfg(target_os = "linux")]
        if linux_strip::close_range_cloexec() || linux_strip::proc_self_fd() {
            return;
        }
        #[cfg(target_vendor = "apple")]
        let rest = darwin_strip::mark_listed(self.bound);
        #[cfg(not(target_vendor = "apple"))]
        let rest = 3..self.bound;
        for fd in rest {
            // SAFETY: `F_SETFD` takes an int and touches no memory; an unused number is a harmless `EBADF`.
            unsafe {
                fcntl(fd, F_SETFD, FD_CLOEXEC);
            }
        }
    }
}

/// One descriptor a forked child gets at a fixed number: the same open file as the parent's
/// `from`, at `at`, open across the exec. Made in the parent; [`PassAt::apply`] runs in the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassAt {
    from: RawFd,
    at: RawFd,
}

impl PassAt {
    /// `from` at number `at` in the child.
    ///
    /// # Errors
    /// `InvalidInput` when either number is 0, 1 or 2: by the time the pass runs those are the
    /// child's own stdio, so neither can be passed from or onto.
    pub fn new(from: std::os::fd::BorrowedFd<'_>, at: RawFd) -> io::Result<Self> {
        let from = from.as_raw_fd();
        if from <= 2 || at <= 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a descriptor is passed from and onto a number above 2 (from {from}, at {at}): \
                     0, 1 and 2 are the child's stdio"
                ),
            ));
        }
        Ok(Self { from, at })
    }

    /// The number the child holds the descriptor at.
    #[must_use]
    pub const fn at(self) -> RawFd {
        self.at
    }

    /// Put the descriptor at its number, open across the exec: `dup2` from `from` onto `at`
    /// (the copy is not close-on-exec), or, when `from` already is `at`, clear its close-on-exec
    /// flag (a `dup2` onto itself changes nothing, and the exec would close it). `from` itself
    /// stays as it is. Async-signal-safe: `fcntl` and `dup2` over two integers, no allocation,
    /// no lock. Answers whether `at` now holds the descriptor.
    ///
    /// # Safety
    /// Only in a forked child before its exec (or a process about to exec), with `from` open, and
    /// after any [`InheritStrip::apply`], which marks `at` close-on-exec again. The `dup2` closes
    /// whatever `at` held, so nothing there may be needed past this call.
    #[must_use]
    pub unsafe fn apply(self) -> bool {
        if self.from == self.at {
            // SAFETY: `F_GETFD` and `F_SETFD` take integers and touch no memory.
            unsafe {
                let flags = fcntl(self.at, F_GETFD);
                return flags >= 0 && fcntl(self.at, F_SETFD, flags & !FD_CLOEXEC) == 0;
            }
        }
        loop {
            // SAFETY: `dup2` takes two integers; the caller vouches nothing at `at` is needed.
            if unsafe { dup2(self.from, self.at) } == self.at {
                return true;
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
        }
    }
}

/// A `std::process::Command` whose child inherits nothing of this process above stdio.
pub trait InheritNothing {
    /// Install the strip.
    fn inherit_nothing(&mut self) -> &mut Self;
}

/// `std`'s own `Command`, under the one name a reviewed raw site TAKES it by.
#[expect(
    clippy::disallowed_types,
    reason = "the one name a reviewed raw std Command is taken by; constructing one stays linted at its own statement"
)]
pub type StdCommand = std::process::Command;

impl InheritNothing for StdCommand {
    fn inherit_nothing(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt as _;
        let strip = InheritStrip::prepare();
        // SAFETY: the hook runs between fork and exec (or right before an exec of this process) and calls only `InheritStrip::apply`: raw syscalls over integers, no allocation, no lock. `strip` is a plain integer copied into the closure.
        unsafe {
            self.pre_exec(move || {
                strip.apply();
                Ok(())
            })
        }
    }
}

/// A `std::process::Command` whose child gets one descriptor at a fixed number ([`PassAt`]).
pub trait PassDescriptor {
    /// Give the child `fd` at number `at`, the same open file, open across the exec. The command
    /// holds it at `at` in this process from this call until it is dropped (a close-on-exec copy
    /// made there when `fd` sits elsewhere, and `fd` closed), so nothing this process opens
    /// meanwhile takes that number: not `std`'s exec-error pipe either, whose write end a `dup2`
    /// onto it in the child would replace, telling a failed exec as a started child. The child
    /// only clears the number's close-on-exec flag; a hook that fails fails the spawn.
    ///
    /// Call it after [`InheritNothing::inherit_nothing`]: hooks run in the order they were added,
    /// so a strip added after the pass marks `at` close-on-exec again and the exec closes it.
    /// Added after the strip, the child holds `at` and nothing else above stdio.
    /// On Apple, neither the input nor the retained descriptor may be close-on-fork: the fork
    /// removes a marked descriptor before any hook runs. This adapter refuses a marked input
    /// before copying it, so no unmarked copy can expose it to other forks, and never clears
    /// that flag.
    ///
    /// # Errors
    /// [`PassAt::new`]'s, when `fd` or `at` is 0, 1 or 2; `ResourceBusy` when something else of
    /// this process holds `at`; `Unsupported` on Apple when the input or retained descriptor is
    /// close-on-fork; `fcntl`'s, when no copy can be made there (`at` at or past the descriptor
    /// limit) or its flags cannot be read. A refusal installs no pass hook.
    fn pass_at(&mut self, fd: OwnedFd, at: RawFd) -> io::Result<&mut Self>;
}

impl PassDescriptor for StdCommand {
    fn pass_at(&mut self, fd: OwnedFd, at: RawFd) -> io::Result<&mut Self> {
        use std::os::fd::AsFd as _;
        use std::os::unix::process::CommandExt as _;
        let at = PassAt::new(fd.as_fd(), at)?.at();
        #[cfg(target_vendor = "apple")]
        std_pass_forkable(&fd)?;
        let held = if fd.as_raw_fd() == at {
            fd
        } else {
            held_at(&fd, at)?
        };
        #[cfg(target_vendor = "apple")]
        std_pass_forkable(&held)?;
        // SAFETY: the hook runs between fork and exec, after any strip added before it, and calls only `PassAt::apply` (`fcntl` over integers) and reads `errno`; no allocation, no lock. The closure owns `held`, open at `at` whenever the command spawns, so the pass is in place and only its close-on-exec flag is cleared.
        Ok(unsafe {
            self.pre_exec(move || {
                let pass = PassAt {
                    from: held.as_raw_fd(),
                    at,
                };
                if pass.apply() {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            })
        })
    }
}

#[cfg(target_vendor = "apple")]
fn std_pass_forkable(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: `fd` owns this live descriptor; `F_GETFD` only reads its flags and touches no
    // memory. No close-on-fork flag is cleared while Command lives.
    let flags = unsafe { fcntl(fd.as_raw_fd(), F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & FD_CLOFORK != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "std Command cannot pass close-on-fork descriptor {}: fork removes it before \
                 pre_exec",
                fd.as_raw_fd()
            ),
        ));
    }
    Ok(())
}

/// A close-on-exec copy of `fd` at exactly `at`, or `ResourceBusy` when something else of this
/// process holds that number.
fn held_at(fd: &OwnedFd, at: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `F_DUPFD_CLOEXEC` reads one descriptor number and returns a fresh descriptor (the lowest free at or above `at`) or -1; it touches no memory.
    let copy = unsafe { fcntl(fd.as_raw_fd(), F_DUPFD_CLOEXEC, at) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` was just created here and nothing else owns it.
    let copy = unsafe { OwnedFd::from_raw_fd(copy) };
    if copy.as_raw_fd() != at {
        return Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!("a pass at {at} needs that number free, and this process holds it elsewhere"),
        ));
    }
    Ok(copy)
}

#[cfg(not(target_vendor = "apple"))]
const TEXT_BUSY_FOR: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(not(target_vendor = "apple"))]
const TEXT_BUSY_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);

#[cfg(not(target_vendor = "apple"))]
fn past_text_busy<T>(
    within: std::time::Duration,
    mut spawn: impl FnMut() -> io::Result<T>,
    mut pause: impl FnMut(),
) -> io::Result<T> {
    let deadline = std::time::Instant::now() + within;
    loop {
        match spawn() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && std::time::Instant::now() < deadline =>
            {
                pause();
            }
            done => return done,
        }
    }
}

/// Run `spawn`, a spawn this module does not make (an [`InheritNothing`] `std::process::Command`),
/// retrying an `ETXTBSY` exec for up to a second off Darwin; on Darwin it runs once.
pub fn spawn_past_text_busy<T>(spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(not(target_vendor = "apple"))]
    {
        past_text_busy(TEXT_BUSY_FOR, spawn, text_busy_pause)
    }
    #[cfg(target_vendor = "apple")]
    {
        let mut spawn = spawn;
        spawn()
    }
}

#[cfg(not(target_vendor = "apple"))]
fn text_busy_pause() {
    #[cfg(any(test, debug_assertions))]
    if let Some(hook) = TEXT_BUSY_HOOK.with(|hook| hook.borrow_mut().take()) {
        hook();
        return;
    }
    std::thread::sleep(TEXT_BUSY_PAUSE);
}

#[cfg(all(any(test, debug_assertions), not(target_vendor = "apple")))]
thread_local! {
    static TEXT_BUSY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: `hook` runs in place of this thread's next pause between `ETXTBSY` tries (of
/// [`spawn_past_text_busy`] and of the spawns this module makes), so a test can let a writer go
/// exactly once a spawn has been refused.
#[cfg(all(any(test, debug_assertions), not(target_vendor = "apple")))]
pub fn on_next_text_busy_pause(hook: impl FnOnce() + 'static) {
    TEXT_BUSY_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[path = "spawnfd_command.rs"]
mod no_fork;
pub use no_fork::{
    Child, ChildEvent, ChildNotice, ChildObservation, ChildStop, Command, Rusage, Stdio,
};

#[path = "spawnfd_birth.rs"]
mod birth;
pub use birth::{BirthAttempt, BirthKey, NotSpawned, ObservedSpawned, SpawnFailure, Spawned};

/// The process's hook naming an inherited variable that has gone stale since its start
/// ([`register_stale_inherited_var`]).
static STALE_INHERITED_VAR: std::sync::OnceLock<fn() -> Option<&'static str>> =
    std::sync::OnceLock::new();

/// Register `hook`, asked at each [`command`]: a variable it names is removed from that child's
/// environment (a later `env` on the command still sets it). The one executable registers the
/// Command Line Tools shim's `DEVELOPER_DIR` once the tools arrive while it runs, which it cannot
/// take out of its own environment once it has threads. Only the first registration holds.
pub fn register_stale_inherited_var(hook: fn() -> Option<&'static str>) {
    let _ = STALE_INHERITED_VAR.set(hook);
}

/// A [`Command`] for `program`, without the variable a registered hook calls stale
/// ([`register_stale_inherited_var`]).
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    if let Some(var) = STALE_INHERITED_VAR.get().and_then(|hook| hook()) {
        command.env_remove(var);
    }
    command
}

/// Request a parent-death tie ([`crate::tether`]), killed outright or crashed included, for a
/// helper this process stops or bounds by its own clock (a read or a stream that never ends by
/// itself), which would otherwise run on, adopted, for as long as it can. The first signal is
/// `SIGTERM`, subject to the platform's conditions. On Linux the tie is the parent-death signal
/// (`PR_SET_PDEATHSIG`), which the kernel sends when the forking thread exits, so the spawn forks
/// on one of the tether's threads, each of which lives as long as this process: the tie is the
/// process's, whichever thread asked, and a child that ignores `SIGTERM` runs on. An exec in this
/// process's place ends its other threads, so on Linux it sends the child the signal too, where
/// Darwin's watcher waits for an exit. A spawn whose fork cannot reach such a thread (none was
/// free and a new one could not start) fails, with nothing started. On Darwin the spawn runs
/// through this executable ([`crate::tether::executable`]), whose watcher sends `SIGKILL`
/// [`END_GRACE`](crate::process::END_GRACE) after the `SIGTERM` to the same process only
/// ([`crate::tether::end_watched`]); an unreadable identity or a thread that has not run gets no
/// signal and the watcher exits, so the child can run on. In a process whose `main` never called
/// [`crate::tether::entry`] the child starts untethered. Neither an exec nor a suspended start
/// can be tethered.
pub trait Tether {
    /// Tie the child to this process.
    fn tether(&mut self) -> &mut Self;
}

#[path = "spawnfd_transparent.rs"]
mod pass_through;
pub use pass_through::{PreparedExec, TransparentChild, TransparentCommand, transparent};

#[cfg(target_vendor = "apple")]
mod darwin_strip {
    use super::{F_SETFD, FD_CLOEXEC, fcntl};
    use std::os::fd::RawFd;

    const PROC_PIDLISTFDS: i32 = 1;
    /// How many descriptors one listing holds.
    pub(super) const SLOTS: usize = 2048;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FdInfo {
        fd: i32,
        _kind: u32,
    }

    const LISTING_BYTES: i32 = {
        let bytes = SLOTS * core::mem::size_of::<FdInfo>();
        assert!(bytes <= i32::MAX as usize);
        bytes as i32
    };

    unsafe extern "C" {
        fn getpid() -> i32;
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            size: i32,
        ) -> i32;
    }

    /// Mark every descriptor from 3 up that this process's table lists, and return the numbers the listing does not vouch for: none when it listed the whole table.
    pub(super) fn mark_listed(bound: RawFd) -> core::ops::Range<RawFd> {
        let mut listed = [FdInfo { fd: -1, _kind: 0 }; SLOTS];
        // SAFETY: `listed` is `LISTING_BYTES` writable bytes of `proc_fdinfo` slots, which the kernel fills and does not retain; `getpid` takes nothing.
        let got = unsafe {
            proc_pidinfo(
                getpid(),
                PROC_PIDLISTFDS,
                0,
                listed.as_mut_ptr().cast(),
                LISTING_BYTES,
            )
        };
        let Ok(bytes) = usize::try_from(got) else {
            return 3..bound;
        };
        let count = (bytes / core::mem::size_of::<FdInfo>()).min(SLOTS);
        if count == 0 {
            return 3..bound;
        }
        let mut last = -1;
        for entry in listed.iter().take(count) {
            if entry.fd >= 3 {
                // SAFETY: `F_SETFD` takes an int and touches no memory.
                unsafe {
                    fcntl(entry.fd, F_SETFD, FD_CLOEXEC);
                }
            }
            last = entry.fd;
        }
        if count < SLOTS {
            bound..bound
        } else {
            last.saturating_add(1).max(3)..table_end(bound)
        }
    }

    fn table_end(bound: RawFd) -> RawFd {
        // SAFETY: a null buffer of size 0 asks for the size alone; nothing is written. `getpid` takes nothing.
        let got = unsafe { proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, core::ptr::null_mut(), 0) };
        match usize::try_from(got) {
            Ok(bytes) if bytes > 0 => RawFd::try_from(bytes / core::mem::size_of::<FdInfo>())
                .map_or(bound, |end| end.min(bound)),
            _ => bound,
        }
    }
}

/// One more than the highest descriptor number this process can ever hold, by a limit no thread of an unprivileged process can raise.
#[must_use]
pub fn descriptor_bound() -> RawFd {
    const FALLBACK: RawFd = 1 << 20;
    #[cfg(target_vendor = "apple")]
    {
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>();
        // SAFETY: a NUL-terminated name and a live `i32` with its size; the call writes at most `len` bytes and sets nothing.
        let rc = unsafe {
            sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&raw mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && len == std::mem::size_of::<i32>() && value > 0 {
            value
        } else {
            FALLBACK
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut limit = Rlimit { cur: 0, max: 0 };
        // SAFETY: `limit` is a live `struct rlimit` of the platform's layout, which `getrlimit` fills and does not retain.
        if unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) } == 0 {
            RawFd::try_from(limit.max).unwrap_or(RawFd::MAX)
        } else {
            FALLBACK
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        FALLBACK
    }
}

pub use crate::{SOFT_NOFILE_FLOOR, raised_soft_nofile_target};

/// Raise this process's soft `RLIMIT_NOFILE` to [`raised_soft_nofile_target`]'s
/// answer, best effort: children inherit the raised limit (their descriptors
/// are still closed past the standard three by the spawn posture), and a
/// refusal leaves the limit exactly as it was.
pub fn raise_soft_nofile() {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        let mut limit = Rlimit { cur: 0, max: 0 };
        // SAFETY: `limit` is a live `struct rlimit` of the platform's layout, which `getrlimit` fills and does not retain.
        if unsafe { getrlimit(RLIMIT_NOFILE, &mut limit) } != 0 {
            return;
        }
        let Some(target) = raised_soft_nofile_target(limit.cur, limit.max) else {
            return;
        };
        let raised = Rlimit {
            cur: target,
            max: limit.max,
        };
        // SAFETY: `raised` keeps the hard limit the kernel just reported, so the call can only raise the soft limit toward it.
        let _ = unsafe { setrlimit(RLIMIT_NOFILE, &raised) };
    }
}

/// Pass libnotify's once-gate, so that no fork after this returns can catch it half taken.
pub fn pass_libnotify_gate() {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: a libSystem query that takes and returns plain values; on its first call it builds libnotify's process globals, which is the point.
        let _ = unsafe { notify_is_valid_token(0) };
    }
}

#[macro_export]
macro_rules! pass_libnotify_gate_at_load {
    () => {
        #[used]
        #[unsafe(link_section = "__DATA,__mod_init_func")]
        static PASS_THE_LIBNOTIFY_GATE_AT_LOAD: extern "C" fn() = {
            extern "C" fn pass() {
                $crate::spawnfd::pass_libnotify_gate();
            }
            pass
        };
    };
}

#[cfg(target_os = "linux")]
mod linux_strip {
    use super::{F_SETFD, FD_CLOEXEC, fcntl};
    use core::ffi::{c_char, c_long};
    use std::os::fd::RawFd;

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
        fn open(path: *const c_char, flags: i32, ...) -> RawFd;
        fn close(fd: RawFd) -> i32;
    }

    /// `close_range(3, ~0U, CLOSE_RANGE_CLOEXEC)`.
    pub(super) fn close_range_cloexec() -> bool {
        const SYS_CLOSE_RANGE: c_long = 436;
        const CLOSE_RANGE_CLOEXEC: c_long = 1 << 2;
        // SAFETY: three integer arguments; the kernel touches no memory of ours.
        unsafe {
            syscall(
                SYS_CLOSE_RANGE,
                3 as c_long,
                c_long::from(u32::MAX),
                CLOSE_RANGE_CLOEXEC,
            ) == 0
        }
    }

    /// Mark every number `/proc/self/fd` lists (but its own directory's), read with raw `getdents64` into a stack buffer.
    pub(super) fn proc_self_fd() -> bool {
        #[cfg(target_arch = "x86_64")]
        const SYS_GETDENTS64: c_long = 217;
        #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
        const SYS_GETDENTS64: c_long = 61;
        const O_RDONLY_CLOEXEC: i32 = 0o2_000_000;
        const RECLEN_AT: usize = 16;
        const NAME_AT: usize = 19;
        // SAFETY: a C string literal; the result is a fresh descriptor or -1.
        let dir = unsafe { open(c"/proc/self/fd".as_ptr(), O_RDONLY_CLOEXEC) };
        if dir < 0 {
            return false;
        }
        let mut buf = [0u8; 4096];
        let complete = loop {
            // SAFETY: `buf` is a live, writable buffer of the length passed.
            let n = unsafe {
                syscall(
                    SYS_GETDENTS64,
                    c_long::from(dir),
                    buf.as_mut_ptr(),
                    buf.len() as c_long,
                )
            };
            if n <= 0 {
                break n == 0;
            }
            let end = usize::try_from(n).unwrap_or(0).min(buf.len());
            let mut at = 0;
            while at + NAME_AT < end {
                let (Some(&lo), Some(&hi)) = (buf.get(at + RECLEN_AT), buf.get(at + RECLEN_AT + 1))
                else {
                    break;
                };
                let reclen = usize::from(u16::from_ne_bytes([lo, hi]));
                if reclen == 0 {
                    break;
                }
                let mut fd: RawFd = 0;
                let mut digits = 0;
                let mut i = at + NAME_AT;
                while let Some(&b) = buf.get(i) {
                    if b == 0 || i >= at + reclen {
                        break;
                    }
                    if !b.is_ascii_digit() {
                        digits = 0;
                        break;
                    }
                    fd = fd.saturating_mul(10).saturating_add(RawFd::from(b - b'0'));
                    digits += 1;
                    i += 1;
                }
                if digits > 0 && fd >= 3 && fd != dir {
                    // SAFETY: integer arguments only; an unused number is EBADF.
                    unsafe {
                        fcntl(fd, F_SETFD, FD_CLOEXEC);
                    }
                }
                at += reclen;
            }
        };
        // SAFETY: the directory descriptor opened above.
        unsafe {
            close(dir);
        }
        complete
    }
}

unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    fn dup2(old: RawFd, new: RawFd) -> RawFd;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn access(path: *const core::ffi::c_char, mode: i32) -> i32;
    #[cfg(target_vendor = "apple")]
    fn sysctlbyname(
        name: *const core::ffi::c_char,
        old: *mut core::ffi::c_void,
        old_len: *mut usize,
        new: *mut core::ffi::c_void,
        new_len: usize,
    ) -> i32;
    #[cfg(target_vendor = "apple")]
    fn notify_is_valid_token(token: i32) -> bool;
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn setrlimit(resource: i32, limit: *const Rlimit) -> i32;
}

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}
#[cfg(target_vendor = "apple")]
const RLIMIT_NOFILE: i32 = 8;
#[cfg(target_os = "linux")]
const RLIMIT_NOFILE: i32 = 7;

const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const FD_CLOEXEC: i32 = 1;
/// Apple's close-on-fork descriptor flag, which its libc bindings do not name.
#[cfg(target_vendor = "apple")]
const FD_CLOFORK: i32 = 2;
#[cfg(target_vendor = "apple")]
const F_DUPFD_CLOEXEC: i32 = 67;
#[cfg(not(target_vendor = "apple"))]
const F_DUPFD_CLOEXEC: i32 = 1030;
const WNOHANG: i32 = 1;
const SIGKILL: i32 = 9;
#[cfg(target_vendor = "apple")]
const SIGCONT: i32 = 19;
const SIGTERM: i32 = 15;
#[cfg(not(target_vendor = "apple"))]
const SIGCONT: i32 = 18;
#[cfg(not(target_vendor = "apple"))]
const ENOENT: i32 = 2;
#[cfg(any(test, not(target_vendor = "apple")))]
const EACCES: i32 = 13;
const EMFILE: i32 = 24;
const X_OK: i32 = 1;

/// `spawnfd`'s tests; [`tests::translator`] is every test's in the crate.
#[cfg(test)]
#[path = "spawnfd_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "spawnfd_command_tests.rs"]
mod command_tests;

#[cfg(test)]
#[path = "spawnfd_transparent_tests.rs"]
mod transparent_tests;

/// Test-only, deliberately leaky spawns that owe nothing to std's.
#[cfg(test)]
#[path = "spawnfd_test_support.rs"]
pub(crate) mod test_support;
