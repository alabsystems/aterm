// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start a child that inherits exactly what it is handed (two descriptors at fixed numbers plus its stdio, or its stdio alone, [`command`]), on Darwin without a fork, and, for the spawns that must still fork, the strip that keeps everything else out.

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

/// The two descriptor numbers the fabric bridge inherits: a verb connection and a push (`subscribe`) connection.
pub const BRIDGE_VERB_FD: RawFd = 3;
pub const BRIDGE_PUSH_FD: RawFd = 4;

const _: () = assert!(BRIDGE_VERB_FD == 3 && BRIDGE_PUSH_FD == 4);

/// The composing executable's startup admission of the bridge's two inherited lanes, [`BRIDGE_VERB_FD`] then [`BRIDGE_PUSH_FD`], each by [`adopt_inherited_socket`].
/// # Safety
/// Each eligible lane at [`BRIDGE_VERB_FD`] and [`BRIDGE_PUSH_FD`] must have been transferred exclusively to this call: no Rust or foreign owner may retain it.
pub unsafe fn adopt_startup_bridge_lanes() -> io::Result<(OwnedFd, OwnedFd)> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the bridge's inherited lanes were already taken by this process",
        ));
    }
    // SAFETY: on the GUI route these are the launcher's unclaimed lanes; a hand-started process may have neither, which is a checked refusal.
    let verb = unsafe { adopt_inherited_socket(BRIDGE_VERB_FD) }?;
    // SAFETY: the same startup ownership admission for the distinct push lane.
    let push = unsafe { adopt_inherited_socket(BRIDGE_PUSH_FD) }?;
    Ok((verb, push))
}

/// Adopt an inherited socket as a close-on-exec copy, closing the original number after duplication.
/// # Safety
/// The descriptor number must not be closed or replaced by another thread or callback during this call.
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
    // SAFETY: the caller admitted exclusive, stable ownership; the checks above only diagnose type/flags.
    let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
    inherited.try_clone()
}

/// What descriptor `fd` refers to, read from the descriptor itself: an `fstat` of the number as it stands, never copied and never closed.
pub fn descriptor_metadata(fd: RawFd) -> io::Result<std::fs::Metadata> {
    // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
    if unsafe { fcntl(fd, F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is open (just answered), and the `File` is never dropped (`ManuallyDrop`): it names the number for one `fstat`, which changes nothing.
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
    /// `program` with no arguments, this process's environment, and stdio inherited — `Command::new`'s defaults.
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

    /// `SIGCONT` the child: a child started stopped ([`Command::start_suspended`]) runs; a running one carries on.
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
pub fn spawn_with_two_fds(
    cmd: LaneCommand,
    first: OwnedFd,
    second: OwnedFd,
) -> io::Result<LaneChild> {
    let lanes = [off_the_targets(first)?, off_the_targets(second)?];
    launch(cmd, &lanes)
}

/// Spawn `cmd` holding its stdio and nothing else: no descriptor of this process above stdio reaches it, whatever this process holds without close-on-exec (Metal's shader-cache files; another thread's `socketpair` or `pipe` caught before its `F_SETFD`).
pub fn spawn(cmd: LaneCommand) -> io::Result<LaneChild> {
    launch(cmd, &[])
}

fn launch(cmd: LaneCommand, lanes: &[OwnedFd]) -> io::Result<LaneChild> {
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
    cwd: Option<&'a std::ffi::CStr>,
    base: Option<&'a std::path::Path>,
    pgroup: Option<i32>,
    setsid: bool,
    replace: bool,
    long_entry_ends: bool,
    inherit_all: bool,
    disclaim: Option<&'a std::ffi::CStr>,
    suspended: bool,
}

impl Default for Placement<'_> {
    fn default() -> Self {
        Self {
            stdio: [Source::Inherit; 3],
            lanes: &[],
            cwd: None,
            base: None,
            pgroup: None,
            setsid: false,
            replace: false,
            long_entry_ends: false,
            inherit_all: false,
            disclaim: None,
            suspended: false,
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

fn placeable_copy(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `F_DUPFD_CLOEXEC` reads one descriptor number and returns a fresh descriptor or -1; it touches no memory.
    let copy = unsafe { fcntl(fd, F_DUPFD_CLOEXEC, FIRST_FREE) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` was just created here and nothing else owns it.
    let copy = unsafe { OwnedFd::from_raw_fd(copy) };
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
    use super::{BSHELL, ENOEXEC, F_GETFD, Image, Placement, Source, c_string, fcntl};
    use core::ffi::{CStr, c_char, c_void};
    use std::ffi::{CString, OsStr};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    type Attr = *mut c_void;
    type Actions = *mut c_void;

    const POSIX_SPAWN_SETPGROUP: i16 = 0x0002;
    const POSIX_SPAWN_SETSIGDEF: i16 = 0x0004;
    const POSIX_SPAWN_SETSIGMASK: i16 = 0x0008;
    const POSIX_SPAWN_SETEXEC: i16 = 0x0040;
    const POSIX_SPAWN_START_SUSPENDED: i16 = 0x0080;
    const POSIX_SPAWN_SETSID: i16 = 0x0400;
    const POSIX_SPAWN_CLOEXEC_DEFAULT: i16 = 0x4000;
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
            Ok(Self { attr, actions })
        }

        fn spawn(
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

    /// `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`: the child holds stdio and the lanes and nothing else.
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
        // SAFETY: each call takes an object `Objects::new` initialised and a live local or C string; none retains a pointer past the call.
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
                        posix_spawn_file_actions_adddup2(&mut objects.actions, fd, target)
                    }
                })?;
            }
            for (target, lane) in (super::BRIDGE_VERB_FD..).zip(place.lanes) {
                check(posix_spawn_file_actions_adddup2(
                    &mut objects.actions,
                    lane.as_raw_fd(),
                    target,
                ))?;
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
            let path = rebase(program, place)?;
            return match objects.spawn(&path, argv, envp) {
                Ok(pid) => Ok(pid),
                Err(ENOEXEC) => as_script(&path),
                Err(rc) => Err(io::Error::from_raw_os_error(rc)),
            };
        }
        if program.is_empty() {
            return Err(io::Error::from_raw_os_error(ENOENT));
        }
        let search = image
            .search
            .as_deref()
            .map_or(&b"/usr/bin:/bin"[..], OsStr::as_bytes);
        let exists = |path: &CString| std::fs::metadata(OsStr::from_bytes(path.as_bytes())).is_ok();
        let mut eacces = false;
        for dir in search.split(|&b| b == b':') {
            let dir: &[u8] = if dir.is_empty() { b"." } else { dir };
            if dir.len() + program.len() + 2 > PATH_MAX {
                if place.long_entry_ends {
                    return Err(io::Error::from_raw_os_error(ENAMETOOLONG));
                }
                continue;
            }
            let mut full = Vec::with_capacity(dir.len() + program.len() + 1);
            full.extend_from_slice(dir);
            full.push(b'/');
            full.extend_from_slice(program);
            let candidate = rebase(&full, place)?;
            if !exists(&candidate) {
                continue;
            }
            let rc = match objects.spawn(&candidate, argv, envp) {
                Ok(pid) => return Ok(pid),
                Err(rc) => rc,
            };
            match rc {
                ELOOP | ENAMETOOLONG | ENOENT | ENOTDIR => {}
                ENOEXEC => return as_script(&candidate),
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

    fn rebase(path: &[u8], place: &Placement<'_>) -> io::Result<CString> {
        match place.base {
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
        F_GETFD, F_SETFD, Image, InheritStrip, Placement, Source, fcntl, off_the_targets, resolve,
        waitpid,
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

    unsafe fn child(
        strip: Option<InheritStrip>,
        sources: &[RawFd; 5],
        placed: usize,
        null: RawFd,
        report: RawFd,
        exec: &Exec<'_>,
    ) -> ! {
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
}

/// The strip, for a spawn that must fork (a PTY child: `setrlimit` and a controlling terminal are not `posix_spawn` file actions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InheritStrip {
    bound: RawFd,
}

impl InheritStrip {
    /// Read [`descriptor_bound`] in the parent, before the fork, and pass libnotify's once-gate ([`pass_libnotify_gate`]), so no fork that strips (aterm-pty's spawn, [`InheritNothing`], the update hand-off's fork lane) loses its child inside `fork()`.
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

/// A `std::process::Command` whose child inherits nothing of this process above stdio: [`InheritStrip`] in a `pre_exec` hook, for a spawn that must fork anyway.
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
        // SAFETY: the hook runs between fork and exec (or right before an exec of this process) and calls only `InheritStrip::apply`: raw syscalls over integers, no allocation, no lock.
        unsafe {
            self.pre_exec(move || {
                strip.apply();
                Ok(())
            })
        }
    }
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

/// A spawn this module does not make — a `std::process::Command` built with [`InheritNothing`] — tried again while the kernel refuses its exec as `ETXTBSY`, as this module's own are ([`past_text_busy`]): for up to a second off Darwin, once on Darwin.
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
    #[cfg(test)]
    if let Some(hook) = TEXT_BUSY_HOOK.with(|hook| hook.borrow_mut().take()) {
        hook();
        return;
    }
    std::thread::sleep(TEXT_BUSY_PAUSE);
}

#[cfg(all(test, not(target_vendor = "apple")))]
thread_local! {
    static TEXT_BUSY_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[path = "spawnfd_command.rs"]
mod no_fork;
pub use no_fork::{Child, Command, Rusage, Stdio};

/// A [`Command`] for `program`: its child holds its stdio and nothing else, and on Darwin it starts without a fork.
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    Command::new(program)
}

#[path = "spawnfd_transparent.rs"]
mod pass_through;
pub use pass_through::{TransparentChild, TransparentCommand, transparent};

#[cfg(target_vendor = "apple")]
mod darwin_strip {
    use super::{F_SETFD, FD_CLOEXEC, fcntl};
    use std::os::fd::RawFd;

    const PROC_PIDLISTFDS: i32 = 1;
    /// How many descriptors one listing holds: `struct proc_fdinfo` is 8 bytes, so 16 KiB of the forking thread's stack.
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

    /// Mark every descriptor from 3 up that this process's table lists, and return the numbers the listing does not vouch for: none when it listed the whole table; from one past its last entry (the kernel lists in ascending order) to [`table_end`] when it filled the buffer; `3..bound` when it failed.
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

/// One more than the highest descriptor number this process can ever hold, by a limit no thread of an unprivileged process can raise, so a number read before a fork still bounds the child's whole table.
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

    /// `close_range(3, ~0U, CLOSE_RANGE_CLOEXEC)`: Linux 5.11+. 436 on every architecture of the unified syscall table.
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

    /// Mark every number `/proc/self/fd` lists (but its own directory's), read with raw `getdents64` into a stack buffer — what CPython's `_posixsubprocess` does in the same place, for the same reason.
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
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    #[cfg(not(target_vendor = "apple"))]
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
    #[cfg(target_os = "linux")]
    fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}
#[cfg(target_os = "linux")]
const RLIMIT_NOFILE: i32 = 7;

const F_GETFD: i32 = 1;
const F_SETFD: i32 = 2;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const FD_CLOEXEC: i32 = 1;
#[cfg(target_vendor = "apple")]
const F_DUPFD_CLOEXEC: i32 = 67;
#[cfg(not(target_vendor = "apple"))]
const F_DUPFD_CLOEXEC: i32 = 1030;
const WNOHANG: i32 = 1;
const SIGKILL: i32 = 9;
#[cfg(target_vendor = "apple")]
const SIGCONT: i32 = 19;
#[cfg(not(target_vendor = "apple"))]
const SIGCONT: i32 = 18;
#[cfg(not(target_vendor = "apple"))]
const ENOENT: i32 = 2;
#[cfg(any(test, not(target_vendor = "apple")))]
const EACCES: i32 = 13;
const EMFILE: i32 = 24;
#[cfg(not(target_vendor = "apple"))]
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

/// Test-only, deliberately leaky spawns that owe nothing to std's: the control arm of every inheritance test here (and in [`crate::pspawn`]).
#[cfg(test)]
#[path = "spawnfd_test_support.rs"]
pub(crate) mod test_support;
