// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process facts from the kernel.

/// What a process-existence query established, independently of readable identity data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessPresence {
    /// The pid exists, including when access to it is refused.
    Present,
    /// The pid is held by a process that has exited and is not yet reaped (a zombie, or on XNU
    /// one whose exit has begun and not finished): it runs nothing again, and once reapable it
    /// holds nothing but its number.
    Exited,
    /// The kernel positively reported that no process has this pid.
    Absent,
    /// The query failed or this platform has no reader.
    Unknown,
}

/// Query one positive pid without delivering a signal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn process_presence(pid: i32) -> ProcessPresence {
    if pid <= 0 {
        return ProcessPresence::Unknown;
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // Read before signal zero: an exit only moves forward, so one read here that signal
        // zero then no longer finds was reaped in between, and reads absent, not present.
        let exited = exited_unreaped(pid);
        // SAFETY: a positive pid addresses one process; signal zero only queries existence.
        let answers = unsafe { kill(pid, 0) } == 0;
        if !answers {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EPERM) => {}
                Some(libc::ESRCH) => return ProcessPresence::Absent,
                _ => return ProcessPresence::Unknown,
            }
        }
        if exited {
            ProcessPresence::Exited
        } else {
            ProcessPresence::Present
        }
    }
    #[cfg(not(unix))]
    {
        ProcessPresence::Unknown
    }
}

/// Query a dedicated positive process group atomically, without delivering a signal.
/// `Absent` means this namespace's kernel returned `ESRCH` for the entire group. An empty
/// process census or a dead leader cannot establish this fact. Permission failures remain
/// `Unknown`; `Exited` is never returned for a group. A caller interpreting a durable group
/// must independently bind this read to its captured boot and pid namespace.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn group_presence(pgid: u32) -> ProcessPresence {
    let Ok(group) = i32::try_from(pgid) else {
        return ProcessPresence::Unknown;
    };
    if group <= 1 {
        return ProcessPresence::Unknown;
    }
    #[cfg(unix)]
    {
        // SAFETY: group is greater than one, so its negative addresses exactly one group,
        // never this process's group by pid zero or every process by -1. Signal zero only
        // checks existence and permission; no signal is delivered.
        if unsafe { libc::kill(-group, 0) } == 0 {
            ProcessPresence::Present
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            ProcessPresence::Absent
        } else {
            ProcessPresence::Unknown
        }
    }
    #[cfg(not(unix))]
    {
        ProcessPresence::Unknown
    }
}

/// Whether `pid` reads as a process that has exited and is not yet reaped. On XNU: `SZOMB`, or
/// a record only the zombie lookup finds. XNU takes an exiting process off its live list before
/// it ends its threads and address space, so from then every live read ([`read_birth`],
/// [`read_lineage`], and [`end_proven`]'s identity) refuses it while its status reads running
/// until the exit finishes. On Linux a `Z` or `X` row with no other thread
/// ([`procfs::exited_row`]). A read that fails says no.
#[cfg(unix)]
fn exited_unreaped(pid: i32) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        short_info(pid, true).is_some_and(|info| {
            info.pbsi_status == BsdShortInfo::SZOMB || short_info(pid, false).is_none()
        })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .is_ok_and(|stat| procfs::exited_row(&stat, pid))
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        false
    }
}

/// Whether `pid` names a live process.
#[must_use]
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    if unsafe { kill(pid as i32, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(1)
}

/// Whether `pid` names a live process (see the Unix twin's docs).
#[must_use]
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use crate::win::ffi;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const WAIT_TIMEOUT: u32 = 258;
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    let handle =
        unsafe { ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        return unsafe { ffi::GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code: u32 = 0;
    let alive = unsafe { ffi::WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetExitCodeProcess(handle, &mut code) } != 0
        && code == STILL_ACTIVE;
    let _ = unsafe { ffi::CloseHandle(handle) };
    alive
}

/// A process's kernel start time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessBirth {
    /// Start time, unix seconds.
    pub seconds: u64,
    /// The microseconds within `seconds`.
    pub microseconds: u64,
}

/// A process's start, compared for equality and order only (system awareness §4.4's `Birth`):
/// whole microseconds of the kernel's wall start record on Apple and Windows, and clock ticks
/// after boot (`/proc/<pid>/stat` field 22) on Linux. Every record that holds one is kept for
/// one boot on one machine, so one unit per platform is enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Birth(pub u64);

impl From<ProcessBirth> for Birth {
    /// The wall record as whole microseconds, saturating.
    fn from(birth: ProcessBirth) -> Self {
        Self(
            birth
                .seconds
                .saturating_mul(1_000_000)
                .saturating_add(birth.microseconds),
        )
    }
}

/// Process identity token (`<pid>@<birth>:<idversion>`, or `<pid>@<birth>` where the kernel
/// keeps no pid version) re-read before each pid signal. Two reads that agree saw no reuse of
/// the pid between them and, where the identity has a pid version (Apple), no exec; on Linux an
/// exec keeps the pid and the start, so only a caller's own reads (the executable it judged)
/// can see it. On Linux the signal goes through a pidfd opened before the last read
/// ([`signal_by_handle`]), so a pid reused after that read is never signalled unless the kernel
/// refuses the pidfd; elsewhere such a reuse is not excluded. No kernel excludes an exec after
/// the last read: a pidfd names the process, not its program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    /// The pid.
    pub pid: i32,
    /// Its birth, which tells it from the next process given the same number.
    pub birth: Birth,
    /// Its pid version, incremented on exec (`None` where the kernel keeps none: Linux).
    pub idversion: Option<i32>,
}

/// Whether this kernel keeps a pid version, so a token names one ([`Identity::idversion`]).
pub const KEEPS_PID_VERSIONS: bool = cfg!(target_vendor = "apple");

impl Identity {
    /// Formats the wire token `<pid>@<birth>:<idversion>`, or `<pid>@<birth>` with no version.
    #[must_use]
    pub fn token(&self) -> String {
        match self.idversion {
            Some(version) => format!("{}@{}:{version}", self.pid, self.birth.0),
            None => format!("{}@{}", self.pid, self.birth.0),
        }
    }

    /// Reads a token [`Self::token`] wrote, in this kernel's shape ([`KEEPS_PID_VERSIONS`]).
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Self::parse_shaped(token, KEEPS_PID_VERSIONS)
    }

    /// Reads `token` in one shape: `<pid>@<birth>:<idversion>` where `versioned`, else
    /// `<pid>@<birth>`. Only the exact text [`Self::token`] writes reads back: digits only (no
    /// sign, space or leading zero), a pid above 1, every number within its type, and no field
    /// more or fewer. A bare pid, a pattern or a word is no token.
    #[must_use]
    pub fn parse_shaped(token: &str, versioned: bool) -> Option<Self> {
        fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse().ok())
                .flatten()
        }
        let (pid, rest) = token.split_once('@')?;
        let (birth, idversion) = if versioned {
            let (birth, version) = rest.split_once(':')?;
            (birth, Some(number::<i32>(version)?))
        } else {
            (rest, None)
        };
        let parsed = Self {
            pid: number::<i32>(pid).filter(|&pid| pid > 1)?,
            birth: Birth(number(birth)?),
            idversion,
        };
        (parsed.token() == token).then_some(parsed)
    }
}

/// Why [`end_proven`] refused to signal or failed to deliver a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndRefusal {
    /// The pid's birth or pid version moved since the token was proven.
    Stale,
    /// The fresh read no longer meets the tier's conditions.
    NotProven,
    /// A signal could not be delivered (the kernel refused it).
    SignalFailed,
}

/// What [`end_proven`] did: the signals it sent and whether the process was gone by the end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ended {
    /// A `SIGTERM` was sent.
    pub termed: bool,
    /// A `SIGKILL` was sent (the process outlived the grace).
    pub killed: bool,
    /// Identity reads no longer named the process (no identity or a different birth).
    pub gone: bool,
}

/// Kernel seam for [`end_proven`] to re-read a pid's identity.
pub trait EndControl {
    /// `pid`'s identity now, or `None` when gone, unreadable, or not of this uid.
    fn identity(&mut self, pid: i32) -> Option<Identity>;
    /// Sends `sig` to the one process `token` names, the identity just re-read (an arm with a
    /// process handle holds the signal to it: [`signal_by_handle`]).
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()>;
    /// Sleeps `pause` before the next poll for the process to exit.
    fn nap(&mut self, pause: std::time::Duration);
}

/// Grace period [`end_proven`] gives a process to exit after `SIGTERM` before `SIGKILL`.
pub const END_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

const END_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Terminates `token`'s process, re-proving identity and `still_proven` before each signal.
///
/// # Errors
/// [`EndRefusal`]: stale or not proven before the `SIGTERM`, or a `SIGTERM` the kernel refused
/// while the process still reads. Once a `SIGTERM` went out the answer is `Ok`.
pub fn end_proven(
    token: Identity,
    grace: std::time::Duration,
    control: &mut impl EndControl,
    mut still_proven: impl FnMut(&Identity) -> bool,
) -> Result<Ended, EndRefusal> {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    let pid = token.pid;
    let gone = |now: Option<Identity>| now.is_none_or(|n| n.birth != token.birth);
    let names_token = |now: &Option<Identity>| *now == Some(token);
    let mut judge = |control: &mut dyn EndControl| -> Judged {
        let before = control.identity(pid);
        if !names_token(&before) {
            return Judged::Moved(before);
        }
        if !still_proven(&token) {
            return Judged::Lost;
        }
        let after = control.identity(pid);
        if names_token(&after) {
            Judged::Holds
        } else {
            Judged::Moved(after)
        }
    };
    match judge(&mut *control) {
        Judged::Holds => {}
        Judged::Moved(_) => return Err(EndRefusal::Stale),
        Judged::Lost => return Err(EndRefusal::NotProven),
    }
    let mut ended = Ended::default();
    if control.signal(&token, SIGTERM).is_err() {
        ended.gone = gone(control.identity(pid));
        return if ended.gone {
            Ok(ended)
        } else {
            Err(EndRefusal::SignalFailed)
        };
    }
    ended.termed = true;
    if let Some(now) = moved_within(control, token, grace) {
        ended.gone = gone(now);
        return Ok(ended);
    }
    match judge(&mut *control) {
        Judged::Holds => {}
        Judged::Moved(now) => {
            ended.gone = gone(now);
            return Ok(ended);
        }
        Judged::Lost => return Ok(ended),
    }
    if control.signal(&token, SIGKILL).is_err() {
        ended.gone = gone(control.identity(pid));
        return Ok(ended);
    }
    ended.killed = true;
    ended.gone = moved_within(control, token, grace).is_some_and(gone);
    Ok(ended)
}

enum Judged {
    Holds,
    Moved(Option<Identity>),
    Lost,
}

fn moved_within(
    control: &mut impl EndControl,
    token: Identity,
    grace: std::time::Duration,
) -> Option<Option<Identity>> {
    let deadline = crate::deadline::saturating_after(std::time::Instant::now(), grace);
    loop {
        let now = control.identity(token.pid);
        if now != Some(token) {
            return Some(now);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        control.nap(END_POLL);
    }
}

/// Live [`EndControl`] backed by [`read_identity`], a pidfd on Linux ([`signal_by_handle`]) or
/// `kill(2)` elsewhere, and [`std::thread::sleep`].
#[derive(Clone, Copy, Debug, Default)]
pub struct EndControlLive;

impl EndControl for EndControlLive {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        read_identity(pid)
    }
    #[cfg_attr(trust_verify, trust::skip)]
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            signal_by_handle(
                token,
                sig,
                pidfd::open,
                read_identity,
                pidfd::send,
                kill_pid,
            )
        }
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        {
            kill_pid(token.pid, sig)
        }
        #[cfg(not(unix))]
        {
            let _ = (token, sig);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "end_proven signals are unix-only",
            ))
        }
    }
    fn nap(&mut self, pause: std::time::Duration) {
        std::thread::sleep(pause);
    }
}

/// Signal the process `token` names through a handle opened on its pid before a last identity
/// read: `open(pid)`, then `identity(pid)` must still equal `token`, then `send(&handle, sig)`.
/// A handle names the process it was opened on, so a pid reused after the open cannot be
/// reached, and a reuse before it shows in the read. An open the kernel will not make takes the
/// same read and then `kill(pid, sig)`: `ENOSYS` ([`std::io::ErrorKind::Unsupported`], a kernel
/// before 5.3 or a seccomp filter) or `EPERM` ([`std::io::ErrorKind::PermissionDenied`], a
/// seccomp filter's usual answer; `pidfd_open` itself never refuses a permission). Every other
/// open error is returned unread (`ESRCH`: the process is gone). Never a group.
///
/// # Errors
/// The open's or the signal's, or [`std::io::ErrorKind::NotFound`] when the read no longer
/// names `token` (nothing was sent).
pub fn signal_by_handle<H>(
    token: &Identity,
    sig: i32,
    open: impl FnOnce(i32) -> std::io::Result<H>,
    identity: impl FnOnce(i32) -> Option<Identity>,
    send: impl FnOnce(&H, i32) -> std::io::Result<()>,
    kill: impl FnOnce(i32, i32) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let handle = match open(token.pid) {
        Ok(handle) => Some(handle),
        Err(why)
            if matches!(
                why.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            None
        }
        Err(why) => return Err(why),
    };
    if identity(token.pid) != Some(*token) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the pid no longer names the token's process",
        ));
    }
    match handle {
        Some(handle) => send(&handle, sig),
        None => kill(token.pid, sig),
    }
}

/// `kill(pid, sig)`: one pid, never a group.
#[cfg(unix)]
// Skip: bottoms out at the `kill` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn kill_pid(pid: i32, sig: i32) -> std::io::Result<()> {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: two integer arguments; `end_proven` re-read the identity just before this and
    // signals one pid, never a group.
    if unsafe { kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Linux's process handles (`pidfd_open(2)`, `pidfd_send_signal(2)`), for [`signal_by_handle`].
#[cfg(any(target_os = "linux", target_os = "android"))]
mod pidfd {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    /// A pidfd for `pid` (close-on-exec, as every pidfd is).
    // Skip: bottoms out at the `syscall` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn open(pid: i32) -> std::io::Result<OwnedFd> {
        let no_flags: libc::c_long = 0;
        // SAFETY: `pidfd_open` reads a pid and a flags word, each passed as the `long` the
        // kernel reads, and returns a new descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::c_long::from(pid), no_flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = i32::try_from(fd).map_err(|_| std::io::Error::other("a pidfd past i32"))?;
        // SAFETY: the kernel just returned `fd`, which nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Send `sig` to the process `fd` was opened on.
    // Skip: bottoms out at the `syscall` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) fn send(fd: &OwnedFd, sig: i32) -> std::io::Result<()> {
        let none: libc::c_long = 0;
        // SAFETY: `pidfd_send_signal` reads a live descriptor, a signal number, a null
        // `siginfo_t` pointer (the kernel fills it as `kill` would) and a zero flags word, each
        // passed as the `long` the kernel reads.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                libc::c_long::from(fd.as_raw_fd()),
                libc::c_long::from(sig),
                none,
                none,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[path = "process_ancestry.rs"]
mod ancestry;
pub use ancestry::{Ancestry, stable_descendant};

#[path = "process_kin.rs"]
mod kin;
pub use kin::{
    GroupMembers, HeldNow, Lineage, MARKER_VARS, MAX_GROUP_PIDS, Markers, TRUST_CHECK_VAR,
    VERIFY_RUN_VAR, at_program_check, boot_id, group_listing_can_be_whole, group_members, held_age,
    held_programs, held_since, host_id, launchd_idversion, launched_before, pid_namespace,
    process_serial, read_identity, read_lineage, read_process_markers, stripped_from_child,
    thread_ran,
};
#[cfg(any(unix, windows))]
pub use kin::{held_among, held_since_among};

#[path = "process_image.rs"]
mod image;
pub use image::{LoadedImage, image_matches_file};

#[cfg(target_vendor = "apple")]
pub use kin::{BsdInfoWithUniqId, UniqIdentifierInfo, bsdinfo_uniq, uniq_identifier};

/// `pid`'s [`Birth`]: its wall start record as whole microseconds where the kernel keeps one
/// ([`read_process_birth`], Apple and Windows), its start in clock ticks on Linux
/// ([`read_identity`]), on those readers' terms; `None` elsewhere.
#[must_use]
pub fn read_birth(pid: i32) -> Option<Birth> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        read_identity(pid).map(|identity| identity.birth)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        read_process_birth(pid).map(Birth::from)
    }
}

/// The kernel's CodeDirectory hash for a running Apple image, bracketed by its process
/// identity. Unlike a hash read from the pathname, this identifies the code loaded by exec.
/// Returns `None` off Apple platforms or when the kernel refuses the read.
#[must_use]
pub fn read_code_directory_hash(pid: i32) -> Option<[u8; 20]> {
    #[cfg(target_vendor = "apple")]
    {
        unsafe extern "C" {
            fn csops(pid: i32, ops: u32, useraddr: *mut core::ffi::c_void, usersize: usize) -> i32;
        }
        let before = read_identity(pid)?;
        let mut hash = [0_u8; 20];
        // SAFETY: hash holds 20 writable bytes, exactly the length passed to CS_OPS_CDHASH.
        let result = unsafe { csops(pid, 5, hash.as_mut_ptr().cast(), hash.len()) };
        (result == 0 && read_identity(pid) == Some(before)).then_some(hash)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

/// Reads the kernel's start-time record for `pid`, or `None` if unavailable.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_birth(pid: i32) -> Option<ProcessBirth> {
    read_bsdinfo(pid).map(|info| info.birth)
}

/// The pid of `pid`'s parent, or `None` on the terms of [`read_process_birth`].
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_parent(pid: i32) -> Option<u32> {
    read_bsdinfo(pid).map(|info| info.parent)
}

/// See the Apple twin.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_parent(pid: i32) -> Option<u32> {
    let start = read_process_start(pid)?;
    let status = read_proc_text(std::path::Path::new(&format!("/proc/{pid}/status"))).ok()?;
    let parent = procfs::parent(&status)?;
    (read_process_start(pid)? == start).then_some(parent)
}

/// See the Apple twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_parent(_pid: i32) -> Option<u32> {
    None
}

/// Whether `pid` has run an exec since it was forked (`None` on the terms of
/// [`read_process_birth`], or a record that does not read). A forked child maps its parent's
/// image until it execs, so a reader that takes a process's image as a program it ran waits
/// for `Some(true)` first: a shell's child between its fork and its exec is the shell's copy,
/// and one whose command was not found never execs. Apple: the BSD info's `PROC_FLAG_EXEC`,
/// which a fork clears and an exec or a `posix_spawn` sets.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_execd(pid: i32) -> Option<bool> {
    const PROC_FLAG_EXEC: u32 = 0x4000;
    read_bsdinfo(pid).map(|info| info.flags & PROC_FLAG_EXEC != 0)
}

/// See the Apple twin. Linux: `/proc/<pid>/stat`'s flags without `PF_FORKNOEXEC`
/// ([`procfs::execd_row`]), for a process of any user.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_execd(pid: i32) -> Option<bool> {
    if pid <= 1 {
        return None;
    }
    let stat = read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat"))).ok()?;
    procfs::execd_row(&stat, pid)
}

/// See the Apple twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_execd(_pid: i32) -> Option<bool> {
    None
}

/// Reads the executable path the kernel records for `pid` (`proc_pidpath`).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_process_path(pid: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    const MAX: usize = 4096;
    unsafe extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut core::ffi::c_void, buffersize: u32) -> i32;
    }
    read_bsdinfo(pid)?;
    let mut buf = [0u8; MAX];
    // SAFETY: `buf` is MAX writable bytes; libproc writes at most that many and returns the path's
    // length, or <= 0 on failure.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), MAX as u32) };
    let len = usize::try_from(len).ok().filter(|&n| n > 0 && n <= MAX)?;
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
        buf.get(..len)?,
    )))
}

/// `struct proc_bsdshortinfo` (`<sys/proc_info.h>`).
#[cfg(target_vendor = "apple")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BsdShortInfo {
    /// The process.
    pub pbsi_pid: u32,
    /// Its parent.
    pub pbsi_ppid: u32,
    /// Its process group.
    pub pbsi_pgid: u32,
    /// `p_stat`: `SIDL` 1, `SRUN` 2, `SSLEEP` 3, `SSTOP` 4, `SZOMB` 5.
    pub pbsi_status: u32,
    /// Its name, NUL-padded.
    pub pbsi_comm: [u8; 16],
    /// `PROC_FLAG_*`: [`BsdShortInfo::PROC_FLAG_EXEC`] among them.
    pub pbsi_flags: u32,
    /// Effective user.
    pub pbsi_uid: u32,
    /// Effective group.
    pub pbsi_gid: u32,
    /// Real user.
    pub pbsi_ruid: u32,
    /// Real group.
    pub pbsi_rgid: u32,
    /// Saved user.
    pub pbsi_svuid: u32,
    /// Saved group.
    pub pbsi_svgid: u32,
    /// Reserved.
    pub pbsi_rfu: u32,
}

#[cfg(target_vendor = "apple")]
const _: () = assert!(std::mem::size_of::<BsdShortInfo>() == 64);

#[cfg(target_vendor = "apple")]
impl BsdShortInfo {
    /// `pbsi_status` of a zombie.
    pub const SZOMB: u32 = 5;
    /// `pbsi_status` of a stopped process.
    pub const SSTOP: u32 = 4;
    /// `pbsi_flags`: the process has replaced the image it was forked with (`P_EXEC`).
    pub const PROC_FLAG_EXEC: u32 = 0x4000;
}

/// Whether `pid` is stopped (a `SIGSTOP`, a tracer's stop): it answers nothing until resumed.
#[must_use]
pub fn is_stopped(pid: u32) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        i32::try_from(pid)
            .ok()
            .and_then(read_short_info)
            .is_some_and(|info| info.pbsi_status == BsdShortInfo::SSTOP)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // `T` is a job-control stop, `t` a tracer's.
        i32::try_from(pid).ok().is_some_and(|id| {
            read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
                .ok()
                .and_then(|stat| procfs::stat_start(&stat, id))
                .is_some_and(|(state, _)| matches!(state, 'T' | 't'))
        })
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        false
    }
}

/// The signals `pid` ignores now (bit `n - 1` for signal `n`), as the kernel reports them: macOS's
/// `kinfo_proc` ([`aterm_sysprobe::kproc::ignored_signals`]), Linux's `/proc/<pid>/status`. `None`
/// when the process is gone, its record cannot be read, or this platform has no reader.
#[must_use]
pub fn ignored_signals(pid: i32) -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        u32::try_from(pid)
            .ok()
            .and_then(aterm_sysprobe::kproc::ignored_signals)
            .map(u64::from)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        read_proc_text(std::path::Path::new(&format!("/proc/{pid}/status")))
            .ok()
            .and_then(|status| procfs::ignored_signals(&status))
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        None
    }
}

/// `pid`'s short BSD record ([`BsdShortInfo`]).
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_short_info(pid: i32) -> Option<BsdShortInfo> {
    short_info(pid, false)
}

/// [`read_short_info`], and of a zombie too when `zombies` (XNU's `proc_pidinfo` finds one
/// only when its argument is 1).
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn short_info(pid: i32, zombies: bool) -> Option<BsdShortInfo> {
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const SIZE: i32 = std::mem::size_of::<BsdShortInfo>() as i32;
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<BsdShortInfo>::zeroed();
    // SAFETY: `info` is SIZE writable bytes of exactly the structure this flavor fills (size
    // asserted above); libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            u64::from(zombies),
            info.as_mut_ptr().cast(),
            SIZE,
        )
    };
    if read != SIZE {
        return None;
    }
    // SAFETY: zero-initialised plain integers, and the exact-size read wrote the whole record.
    let info = unsafe { info.assume_init() };
    (info.pbsi_pid == pid.unsigned_abs()).then_some(info)
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    fn proc_listchildpids(ppid: i32, buffer: *mut core::ffi::c_void, buffersize: i32) -> i32;
}

/// How many children [`child_pids`] lists at most.
#[cfg(target_vendor = "apple")]
pub const CHILD_PIDS_MAX: usize = 64;

/// Lists `pid`'s live children, up to [`CHILD_PIDS_MAX`].
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn child_pids(pid: i32) -> Option<Vec<i32>> {
    let mut pids = [0i32; CHILD_PIDS_MAX];
    let bytes = i32::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is `bytes` writable bytes of pid slots; libproc writes at most that many and
    // answers how many pids it wrote, or -1.
    let found = unsafe { proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
    let found = usize::try_from(found).ok()?.min(CHILD_PIDS_MAX);
    Some(pids[..found].iter().copied().filter(|p| *p > 0).collect())
}

#[cfg(all(test, target_vendor = "apple"))]
#[path = "process_short_info_tests.rs"]
mod short_info_tests;

/// See the Apple twin, whose contract this keeps: the program `pid` runs ([`program_in`] over
/// `/proc`), read between two reads of its start.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_path(pid: i32) -> Option<std::path::PathBuf> {
    let start = read_process_start(pid)?;
    let exe = program_in(std::path::Path::new("/proc"), pid)?;
    (read_process_start(pid)? == start).then_some(exe)
}

/// The program `pid` runs, read off the `/proc` at `proc` through `pid`'s own directory there
/// ([`ProcDir`]): its `exe`, or `None` once the kernel marks that ` (deleted)`, unless its
/// `auxv` names a descriptor ([`procfs::exec_fd`]). Only `binfmt_misc` puts one there, under
/// its `O` flag, on which it hands the interpreter it starts the program to run; the kernel
/// then names that interpreter (Rosetta's translator, in an aarch64 VM) as the executable,
/// where macOS names a Rosetta 2 process's own program, so this reads the program on that
/// descriptor ([`translated_program`]), or `None`. An `auxv` that names none, or does not read,
/// leaves `exe`: a registered interpreter run natively reads as itself, and so does a
/// translator registered with no `O` flag, which opens its program by path.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn program_in(proc: &std::path::Path, pid: i32) -> Option<std::path::PathBuf> {
    let dir = ProcDir::of(proc, pid)?;
    match dir.read(c"auxv").as_deref().and_then(procfs::exec_fd) {
        Some(fd) => translated_program(&dir, fd),
        None => procfs::live_exe(dir.link(c"exe")?),
    }
}

/// The program `binfmt_misc` handed the interpreter `dir`'s process runs, on its descriptor
/// `fd`: the live absolute path that descriptor names, while its `fdinfo` says it holds the
/// kernel's exec open ([`procfs::exec_opened`]) of the file at that path (its `ino:`,
/// [`procfs::open_inode`], is that file's) and the process maps that path executable
/// ([`procfs::maps_executable`]). A descriptor the program closed and reused names nothing: a
/// library `dlopen` holds there for a moment carries no exec mark, an exec open put there in
/// place of the file the link named is another inode, and one the process inherited names a
/// file it does not map. A process exec'd a moment ago reads `None` until its translator has
/// mapped its program, as does any on a kernel whose `fdinfo` prints no `ino:` (before 5.14).
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn translated_program(dir: &ProcDir, fd: i32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let program = dir.link(&std::ffi::CString::new(format!("fd/{fd}")).ok()?)?;
    let program = procfs::live_exe(program).filter(|path| path.is_absolute())?;
    let fdinfo = dir.read(&std::ffi::CString::new(format!("fdinfo/{fd}")).ok()?)?;
    let fdinfo = String::from_utf8_lossy(&fdinfo);
    let inode = std::fs::metadata(&program).ok()?.ino();
    let held = procfs::exec_opened(&fdinfo) && procfs::open_inode(&fdinfo) == Some(inode);
    let maps = std::io::BufReader::new(dir.open(c"maps")?);
    (held && procfs::maps_executable(maps, program.as_os_str().as_bytes())).then_some(program)
}

/// A process's `/proc/<pid>` directory, held open: each read goes through its descriptor, never
/// a path, so all are of the process the directory named when it opened (one that has exited
/// reads nothing), and a translated process's read of itself is a peer's, where Rosetta answers
/// a path read of its own `/proc/<pid>/exe` itself (`docs/measured/linux-rosetta-2026-10-17.md`).
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
struct ProcDir(std::fs::File);

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
impl ProcDir {
    /// The most [`ProcDir::read`] takes: far past any `auxv`, and a regular file's `fdinfo`.
    const READ_MAX: usize = 64 << 10;

    /// `pid`'s directory under `proc`.
    fn of(proc: &std::path::Path, pid: i32) -> Option<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(proc.join(pid.to_string()))
            .ok()
            .map(Self)
    }

    /// The file `name` names under the directory, opened to read.
    fn open(&self, name: &std::ffi::CStr) -> Option<std::fs::File> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        // SAFETY: the directory is open for the call and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: `openat` just returned `fd`, which nothing else owns.
        Some(std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// The bytes of the file `name` names, `None` past [`ProcDir::READ_MAX`].
    fn read(&self, name: &std::ffi::CStr) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let mut bytes = Vec::new();
        self.open(name)?
            .take(Self::READ_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() <= Self::READ_MAX).then_some(bytes)
    }

    /// The link `name` names under the directory, `None` past 4095 bytes.
    fn link(&self, name: &std::ffi::CStr) -> Option<std::path::PathBuf> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStringExt as _;
        let mut buf = [0u8; 4096];
        // SAFETY: the directory is open, `name` is NUL-terminated, and `readlinkat` writes at
        // most `buf.len()` bytes.
        let n = unsafe {
            libc::readlinkat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        let len = usize::try_from(n).ok().filter(|&len| len < buf.len())?;
        Some(std::ffi::OsString::from_vec(buf[..len].to_vec()).into())
    }
}

/// Whether the kernel handed this process's translator the program it runs on a descriptor and
/// this process maps the file on it executable, as under Rosetta's `binfmt_misc` registration
/// (its `O` flag): read off this process's `auxv`, that descriptor's link and `maps` with the
/// pure readers ([`procfs::exec_fd`], [`procfs::maps_executable`]), never through
/// [`read_process_path`], so a test that is not run without it is never skipped by a broken
/// read. Under a translator that opens its program by path, or maps it only readable, the
/// kernel records no translated program, and a translated process reads as its translator. A
/// test seam.
#[cfg(all(
    any(test, debug_assertions),
    any(target_os = "linux", target_os = "android")
))]
#[must_use]
pub fn translator_holds_its_program() -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let me = i32::try_from(std::process::id()).unwrap_or(0);
    let Some(dir) = ProcDir::of(std::path::Path::new("/proc"), me) else {
        return false;
    };
    let handed = dir.read(c"auxv").as_deref().and_then(procfs::exec_fd);
    let program = handed
        .and_then(|fd| std::ffi::CString::new(format!("fd/{fd}")).ok())
        .and_then(|link| dir.link(&link));
    program.is_some_and(|program| {
        dir.open(c"maps").is_some_and(|maps| {
            procfs::maps_executable(
                std::io::BufReader::new(maps),
                program.as_os_str().as_bytes(),
            )
        })
    })
}

/// See the Linux twin: `binfmt_misc` is Linux's, so no translator here is handed a program.
#[cfg(all(
    any(test, debug_assertions),
    not(any(target_os = "linux", target_os = "android"))
))]
#[must_use]
pub const fn translator_holds_its_program() -> bool {
    false
}

/// See the Apple twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_path(_pid: i32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(target_vendor = "apple")]
struct BsdInfo {
    birth: ProcessBirth,
    parent: u32,
    /// `pbi_flags` (`PROC_FLAG_*`).
    flags: u32,
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_bsdinfo(pid: i32) -> Option<BsdInfo> {
    const PROC_PIDTBSDINFO: i32 = 3;
    const SIZE: usize = 136;
    const FLAGS: usize = 0;
    const STATUS: usize = 4;
    const PID: usize = 12;
    const PPID: usize = 16;
    const UID: usize = 20;
    const START_SEC: usize = 120;
    const START_USEC: usize = 128;
    const SZOMB: u32 = 5;
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut core::ffi::c_void,
            buffersize: i32,
        ) -> i32;
        fn geteuid() -> u32;
    }
    if pid <= 1 {
        return None;
    }
    let mut info = [0u8; SIZE];
    // SAFETY: `info` is SIZE writable bytes, exactly the structure this flavor fills; libproc
    // returns the number of bytes it wrote.
    let read = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            SIZE as i32,
        )
    };
    if read != SIZE as i32 {
        return None;
    }
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(info.get(at..at + 4)?.try_into().ok()?))
    };
    let u64_at = |at: usize| -> Option<u64> {
        Some(u64::from_ne_bytes(info.get(at..at + 8)?.try_into().ok()?))
    };
    // SAFETY: `geteuid` is a side-effect-free getter.
    let ours = unsafe { geteuid() };
    if u32_at(PID)? != u32::try_from(pid).ok()? || u32_at(UID)? != ours {
        return None;
    }
    if u32_at(STATUS)? == SZOMB {
        return None;
    }
    Some(BsdInfo {
        birth: ProcessBirth {
            seconds: u64_at(START_SEC)?,
            microseconds: u64_at(START_USEC)?,
        },
        parent: u32_at(PPID)?,
        flags: u32_at(FLAGS)?,
    })
}

/// See the Apple twin.
#[cfg(windows)]
#[must_use]
pub fn read_process_birth(pid: i32) -> Option<ProcessBirth> {
    birth_of_filetime(read_creation_filetime(pid)?)
}

/// Raw creation `FILETIME` of `pid` in 100 ns ticks since 1601-01-01 UTC.
#[cfg(windows)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_creation_filetime(pid: i32) -> Option<u64> {
    use crate::win::ffi;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 258;
    if pid <= 1 {
        return None;
    }
    let pid = u32::try_from(pid).ok()?;
    // SAFETY: a plain open by pid; a handle it returns is closed below.
    let handle =
        unsafe { ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle == 0 {
        return None;
    }
    let mut times = [ffi::FileTime::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: `handle` is the live process handle opened above, and the four out-params are
    // distinct writable `FILETIME`s that outlive the call.
    let read = unsafe { ffi::WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetProcessTimes(handle, created, exited, kernel, user) } != 0;
    // SAFETY: closes the one handle this function opened.
    let _ = unsafe { ffi::CloseHandle(handle) };
    read.then(|| times[0].ticks())
}

#[cfg(any(windows, test))]
fn birth_of_filetime(ticks: u64) -> Option<ProcessBirth> {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let since = ticks.checked_sub(UNIX_EPOCH_TICKS)?;
    Some(ProcessBirth {
        seconds: since / 10_000_000,
        microseconds: since % 10_000_000 / 10,
    })
}

/// See the Apple twin.
#[cfg(not(any(target_vendor = "apple", windows)))]
#[must_use]
pub fn read_process_birth(_pid: i32) -> Option<ProcessBirth> {
    None
}

/// Reading of the kernel's monotonic boot clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootClock(pub u64);

/// Whether this process runs translated (Rosetta), as the kernel says (`sysctl.proc_translated`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Translation {
    /// Native: `0`, or no such sysctl (an Intel Mac before macOS 11).
    Native,
    /// Translated: `1`.
    Translated,
    /// The sysctl answered something else, or failed another way.
    Unknown,
}

/// This process's [`Translation`] (Apple; `Native` elsewhere).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn translation() -> Translation {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const core::ffi::c_char,
            oldp: *mut core::ffi::c_void,
            oldlenp: *mut usize,
            newp: *mut core::ffi::c_void,
            newlen: usize,
        ) -> i32;
    }
    const ENOENT: i32 = 2;
    let mut value: i32 = 0;
    let mut len = core::mem::size_of::<i32>();
    // SAFETY: a NUL-terminated name, one writable `int` and its length, all
    // live for the call; nothing is written to the kernel (`newp` null).
    let rc = unsafe {
        sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            core::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return if std::io::Error::last_os_error().raw_os_error() == Some(ENOENT) {
            Translation::Native
        } else {
            Translation::Unknown
        };
    }
    match (len == core::mem::size_of::<i32>(), value) {
        (true, 0) => Translation::Native,
        (true, 1) => Translation::Translated,
        _ => Translation::Unknown,
    }
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn translation() -> Translation {
    Translation::Native
}

/// The binary translator this process runs under, named, when one does on Linux.
#[cfg(target_os = "linux")]
#[must_use]
pub fn translator() -> Option<String> {
    let kernel =
        crate::atomic_file::read_regular_text(std::path::Path::new("/proc/sys/kernel/arch"), 1024)
            .ok()?;
    let (kernel, code) = (kernel.trim(), std::env::consts::ARCH);
    if kernel.is_empty() || kernel == code {
        return None;
    }
    let rosetta =
        crate::atomic_file::read_regular_text(std::path::Path::new("/proc/cpuinfo"), 1024 * 1024)
            .is_ok_and(|info| info.contains("VirtualApple"));
    Some(if rosetta {
        "Rosetta".to_string()
    } else {
        format!("a translator ({code} code on an {kernel} kernel)")
    })
}

/// See the Linux twin.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub const fn translator() -> Option<String> {
    None
}

/// Reads the Apple boot clock for a process with the given [`Translation`].
#[must_use]
pub fn apple_boot_clock(
    translation: Translation,
    ticks: impl FnOnce() -> u64,
) -> Option<BootClock> {
    match translation {
        Translation::Translated => None,
        Translation::Native | Translation::Unknown => Some(BootClock(ticks())),
    }
}

/// Reads the current [`BootClock`], or `None` where unavailable.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn boot_clock_now() -> Option<BootClock> {
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }
    // SAFETY: a side-effect-free getter with no arguments.
    apple_boot_clock(translation(), || unsafe { mach_absolute_time() })
}

/// See the Apple twin.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64"
))]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn boot_clock_now() -> Option<BootClock> {
    #[repr(C)]
    struct Timespec {
        tv_sec: core::ffi::c_long,
        tv_nsec: core::ffi::c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, tp: *mut Timespec) -> i32;
    }
    const CLOCK_BOOTTIME: i32 = 7;
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is one writable timespec that outlives the call.
    if unsafe { clock_gettime(CLOCK_BOOTTIME, &mut now) } != 0 {
        return None;
    }
    u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)
        .map(BootClock)
}

/// See the Apple twin.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        any(target_os = "linux", target_os = "android"),
        target_pointer_width = "64"
    )
)))]
#[must_use]
pub fn boot_clock_now() -> Option<BootClock> {
    None
}

/// Monotonic milliseconds since boot, counting through sleep.
#[must_use]
pub fn continuous_ms() -> Option<u64> {
    continuous_ns().map(|ns| ns / 1_000_000)
}

/// Monotonic nanoseconds since boot, counting through sleep.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn continuous_ns() -> Option<u64> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut base = Timebase { numer: 0, denom: 0 };
    // SAFETY: one writable `mach_timebase_info_data_t` that outlives the call.
    if unsafe { mach_timebase_info(&raw mut base) } != 0 || base.denom == 0 {
        return None;
    }
    // SAFETY: a side-effect-free getter with no arguments.
    let ticks = unsafe { mach_continuous_time() };
    let ns = u128::from(ticks) * u128::from(base.numer) / u128::from(base.denom);
    u64::try_from(ns).ok()
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn continuous_ns() -> Option<u64> {
    boot_clock_now().map(|BootClock(ns)| ns)
}

/// How long this machine has slept since it booted.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn asleep_since_boot() -> Option<std::time::Duration> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut base = Timebase { numer: 0, denom: 0 };
    // SAFETY: one writable `mach_timebase_info_data_t` that outlives the call.
    if unsafe { mach_timebase_info(&raw mut base) } != 0 || base.denom == 0 {
        return None;
    }
    // SAFETY: side-effect-free getters with no arguments.
    let awake = unsafe { mach_absolute_time() };
    // SAFETY: as above.
    let boot = unsafe { mach_continuous_time() };
    let ns =
        u128::from(boot.saturating_sub(awake)) * u128::from(base.numer) / u128::from(base.denom);
    u64::try_from(ns).ok().map(std::time::Duration::from_nanos)
}

/// See the Apple twin.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64"
))]
#[must_use]
pub fn asleep_since_boot() -> Option<std::time::Duration> {
    const CLOCK_MONOTONIC: i32 = 1;
    const CLOCK_BOOTTIME: i32 = 7;
    let awake = linux_clock_ns(CLOCK_MONOTONIC)?;
    let boot = linux_clock_ns(CLOCK_BOOTTIME)?;
    Some(std::time::Duration::from_nanos(boot.saturating_sub(awake)))
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64"
))]
#[cfg_attr(trust_verify, trust::skip)]
fn linux_clock_ns(clock: i32) -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        tv_sec: core::ffi::c_long,
        tv_nsec: core::ffi::c_long,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, tp: *mut Timespec) -> i32;
    }
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is one writable timespec that outlives the call.
    if unsafe { clock_gettime(clock, &mut now) } != 0 {
        return None;
    }
    u64::try_from(now.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(now.tv_nsec).ok()?)
}

/// See the Apple twin.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        any(target_os = "linux", target_os = "android"),
        target_pointer_width = "64"
    )
)))]
#[must_use]
pub fn asleep_since_boot() -> Option<std::time::Duration> {
    None
}

/// Monotonic awake time since boot in milliseconds (paused during sleep).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn awake_ms() -> Option<u64> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut base = Timebase { numer: 0, denom: 0 };
    // SAFETY: one writable `mach_timebase_info_data_t` that outlives the call.
    if unsafe { mach_timebase_info(&raw mut base) } != 0 || base.denom == 0 {
        return None;
    }
    // SAFETY: a side-effect-free getter with no arguments.
    let ticks = unsafe { mach_absolute_time() };
    let ns = u128::from(ticks) * u128::from(base.numer) / u128::from(base.denom);
    u64::try_from(ns / 1_000_000).ok()
}

/// See the Apple twin.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    target_pointer_width = "64"
))]
#[must_use]
pub fn awake_ms() -> Option<u64> {
    const CLOCK_MONOTONIC: i32 = 1;
    linux_clock_ns(CLOCK_MONOTONIC).map(|ns| ns / 1_000_000)
}

/// See the Apple twin.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        any(target_os = "linux", target_os = "android"),
        target_pointer_width = "64"
    )
)))]
#[must_use]
pub fn awake_ms() -> Option<u64> {
    None
}

/// Wall-clock `(sleep_ms, wake_ms)` of the machine's last sleep.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn last_sleep_ms() -> Option<(i64, i64)> {
    let slept = sysctl_timeval_ms(c"kern.sleeptime")?;
    let woke = sysctl_timeval_ms(c"kern.waketime")?;
    (slept > 0 && slept < woke).then_some((slept, woke))
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn sysctl_timeval_ms(name: &core::ffi::CStr) -> Option<i64> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = core::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is NUL-terminated and outlives the call; `tv` and `len`
    // are live locals, `len` the buffer's real size; nothing is written to the
    // kernel (`newp` null).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut tv).cast(),
            &raw mut len,
            core::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != core::mem::size_of::<libc::timeval>() {
        return None;
    }
    tv.tv_sec
        .checked_mul(1000)?
        .checked_add(i64::from(tv.tv_usec) / 1000)
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn last_sleep_ms() -> Option<(i64, i64)> {
    None
}

/// When a process started, as the kernel recorded it ([`read_process_start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStart {
    /// On the boot clock in the kernel's unit, and never later than the real start.
    pub boot: BootClock,
    /// The wall-clock record where the kernel keeps one ([`ProcessBirth`], Apple).
    pub wall: Option<ProcessBirth>,
}

/// Reads the kernel's boot-clock and wall-clock start record for `pid`.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn read_process_start(pid: i32) -> Option<ProcessStart> {
    let wall = read_process_birth(pid)?;
    let boot = start_abstime(pid)?;
    (read_process_birth(pid)? == wall).then_some(ProcessStart {
        boot,
        wall: Some(wall),
    })
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn start_abstime(pid: i32) -> Option<BootClock> {
    const RUSAGE_INFO_V0: i32 = 0;
    const START_WORD: usize = 80 / 8;
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }
    let mut info = [0u64; 12];
    // SAFETY: `info` is 96 writable, aligned bytes, the size of the structure this flavor fills,
    // and outlives the call.
    if unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V0, info.as_mut_ptr().cast()) } != 0 {
        return None;
    }
    Some(BootClock(*info.get(START_WORD)?))
}

/// See the Apple twin.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_start(pid: i32) -> Option<ProcessStart> {
    if pid <= 1 {
        return None;
    }
    let [stat, status, again] = proc_stat_status_stat(pid)?;
    let (euid, per_second) = linux_reader()?;
    let boot = procfs::start(&stat, &status, &again, pid, euid, per_second)?;
    Some(ProcessStart { boot, wall: None })
}

/// When `pid` started, in whole Unix seconds as `ps -o lstart` reckons it: the boot ([`boot_s`]),
/// read at each call as `ps` reads it, plus the start in clock ticks its `/proc/<pid>/stat` holds
/// ([`procfs::start_s`]). A zombie's is read as a live
/// process's. `None` where either cannot be read: no such process, or one gone mid-read.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_start_s(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let stat = read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat"))).ok()?;
    let (_, ticks) = procfs::stat_start(&stat, pid)?;
    let boot = boot_s()?;
    let (_, per_second) = linux_reader()?;
    Some(procfs::start_s(boot, ticks, per_second))
}

/// The boot in whole Unix seconds, `/proc/stat`'s `btime` ([`procfs::btime`]), read now: a step
/// of the wall clock moves it. `None` where it cannot be read.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn boot_s() -> Option<u64> {
    // A line for each CPU and a count for each interrupt: a large host's outgrows
    // `read_proc_text`'s per-pid cap.
    const MAX_PROC_STAT_BYTES: u64 = 16 * 1024 * 1024;
    let (stat, _) =
        crate::atomic_file::read_regular(std::path::Path::new("/proc/stat"), MAX_PROC_STAT_BYTES)
            .ok()?;
    procfs::btime(&String::from_utf8_lossy(&stat))
}

/// `pid`'s parent pid, process group and controlling terminal's foreground group (`-1` with no
/// terminal), from its `/proc/<pid>/stat` ([`procfs::stat_ids`]); `None` where it cannot be read.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_ids(pid: i32) -> Option<(u32, i64, i64)> {
    if pid <= 0 {
        return None;
    }
    let stat = read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat"))).ok()?;
    procfs::stat_ids(&stat, pid)
}

/// How far [`read_process_start`]'s `boot` can precede a process's real start on Linux, in
/// nanoseconds: one tick of `/proc/<pid>/stat`'s start time, `AT_CLKTCK` a second (10 ms at
/// 100), rounded up ([`procfs::tick_span`]).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn proc_start_tick_ns() -> Option<u64> {
    linux_reader().and_then(|(_, per_second)| procfs::tick_span(per_second))
}

/// See the Linux twin: no `/proc` start record is read here.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
#[must_use]
pub const fn proc_start_tick_ns() -> Option<u64> {
    None
}

/// How many ticks a second `/proc/<pid>/stat`'s start time counts (`AT_CLKTCK`), on Linux: what
/// [`procfs::tick_start`] reads a start at.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn stat_ticks_per_second() -> Option<u64> {
    linux_reader().map(|(_, per_second)| per_second)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[cfg_attr(trust_verify, trust::skip)]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::useless_conversion,
        reason = "c_ulong is u64 on LP64 and u32 on 32-bit"
    )
)]
fn linux_reader() -> Option<(u32, u64)> {
    unsafe extern "C" {
        fn geteuid() -> u32;
        fn getauxval(kind: core::ffi::c_ulong) -> core::ffi::c_ulong;
    }
    const AT_CLKTCK: core::ffi::c_ulong = 17;
    // SAFETY: two side-effect-free getters.
    let (euid, ticks) = unsafe { (geteuid(), getauxval(AT_CLKTCK)) };
    (ticks > 0).then_some((euid, u64::from(ticks)))
}

/// A `/proc` text file, its bytes decoded lossily. The kernel writes a command name as raw
/// bytes cut at 15 (a UTF-8 letter may be cut, and `prctl(PR_SET_NAME)` sets any bytes), so
/// `stat`, `status` and `comm` need hold no UTF-8; a command name is words only, and every
/// field a reader here parses is ASCII. Every OS has it, for a reader that finds `/proc` there.
///
/// # Errors
/// The file's read error.
pub fn read_proc_text(path: &std::path::Path) -> std::io::Result<String> {
    crate::atomic_file::read_regular(path, 256 * 1024)
        .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
}

/// `pid`'s `stat`, `status` and `stat` again, the reads [`procfs::start`] and
/// [`procfs::lineage`] judge.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn proc_stat_status_stat(pid: i32) -> Option<[String; 3]> {
    let read = |name: &str| read_proc_text(std::path::Path::new(&format!("/proc/{pid}/{name}")));
    Some([read("stat").ok()?, read("status").ok()?, read("stat").ok()?])
}

/// Whether this host's `/proc` lists every process to every reader
/// ([`procfs::shows_every_process`] over `/proc/self/mountinfo`; a mount table that does not
/// read shows nothing).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn proc_shows_every_process() -> bool {
    // A busy host's table (thousands of overlay mounts) outgrows `read_proc_text`'s per-pid cap.
    const MAX_MOUNT_TABLE_BYTES: u64 = 16 * 1024 * 1024;
    crate::atomic_file::read_regular(
        std::path::Path::new("/proc/self/mountinfo"),
        MAX_MOUNT_TABLE_BYTES,
    )
    .is_ok_and(|(mounts, _)| procfs::shows_every_process(&String::from_utf8_lossy(&mounts)))
}

/// One `/proc/<pid>/stat` read of a group listing ([`stat_read`]).
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn stat_row(path: &std::path::Path) -> procfs::StatRead {
    stat_read(read_proc_text(path))
}

/// This pid namespace's last allocated id (`/proc/sys/kernel/ns_last_pid`), which every fork
/// and thread here or in a namespace below moves; `None` where it does not read (a kernel built
/// without checkpoint and restore, a `/proc/sys` a sandbox withholds).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn last_pid() -> Option<u32> {
    read_proc_text(std::path::Path::new("/proc/sys/kernel/ns_last_pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The namespace's highest id plus one (`/proc/sys/kernel/pid_max`), where its ids wrap.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn pid_max() -> Option<u32> {
    read_proc_text(std::path::Path::new("/proc/sys/kernel/pid_max"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Whether `/proc` is this process's own pid namespace's: `/proc/self` names this process's
/// pid. A `/proc` mounted for another namespace (`unshare --pid` with no `--mount-proc`) names
/// it by another number or none, and every pid it lists is that namespace's.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn proc_is_this_namespaces() -> bool {
    std::fs::read_link("/proc/self")
        .ok()
        .and_then(|link| link.to_str()?.parse::<u32>().ok())
        == Some(std::process::id())
}

/// The row of id `id` that a group listing reads after its walk ([`procfs::chased_group_of`]):
/// its `stat` ([`stat_read`]) when its `status` names it its own thread group's, and gone for
/// another process's thread, whose process is listed on a row of its own. `/proc/<tid>` reads
/// though no walk lists it.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_row(id: i32) -> procfs::StatRead {
    let read = |name: &str| read_proc_text(std::path::Path::new(&format!("/proc/{id}/{name}")));
    match read("status") {
        Ok(status) => match procfs::thread_group(&status) {
            Some(group) if group == id => stat_read(read("stat")),
            Some(_) => procfs::StatRead::Gone,
            None => procfs::StatRead::Unreadable,
        },
        Err(why) => stat_read(Err(why)),
    }
}

/// A `/proc/<pid>/stat` read as a group listing and a descriptor table take it: its text, gone
/// (`ENOENT`, `ESRCH`), or unreadable (any other error). Under a `hidepid=1` mount another
/// user's `/proc/<pid>` is listed and its files answer `EPERM`, so such a row is unreadable and
/// no listing there is whole; under `hidepid=2` the kernel lists no such directory at all.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
pub(crate) fn stat_read(read: std::io::Result<String>) -> procfs::StatRead {
    match read {
        Ok(text) => procfs::StatRead::Text(text),
        Err(why)
            if why.kind() == std::io::ErrorKind::NotFound
                || why.raw_os_error() == Some(libc::ESRCH) =>
        {
            procfs::StatRead::Gone
        }
        Err(_) => procfs::StatRead::Unreadable,
    }
}

/// See the Apple twin.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_start(_pid: i32) -> Option<ProcessStart> {
    None
}

/// Linux's records of a process, judged from the text of its `/proc` files: each reader here is
/// pure, so every platform's tests hold it to fixture text, and the Linux reads that feed it
/// are thin.
pub mod procfs {
    use super::BootClock;

    /// Parses `(state, starttime)` from a `/proc/<pid>/stat` line for `pid`.
    #[must_use]
    pub fn stat_start(stat: &str, pid: i32) -> Option<(char, u64)> {
        let open = stat.find(" (")?;
        let p = stat[..open].trim();
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<i32>().ok()? != pid {
            return None;
        }
        let close = stat.rfind(')')?;
        if close < open {
            return None;
        }
        let mut fields = stat[close + 1..].split_ascii_whitespace();
        let state = fields.next()?.chars().next()?;
        let start = fields
            .nth(18)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        Some((state, start))
    }

    /// The parent pid, process group and controlling terminal's foreground group (`-1` with no
    /// terminal) from a `/proc/<pid>/stat` line for `pid`: what `ps -o ppid=,pgid=,tpgid=` prints.
    #[must_use]
    pub fn stat_ids(stat: &str, pid: i32) -> Option<(u32, i64, i64)> {
        let open = stat.find(" (")?;
        if stat[..open].trim().parse::<i32>().ok()? != pid {
            return None;
        }
        let close = stat.rfind(')')?;
        if close < open {
            return None;
        }
        let mut fields = stat[close + 1..].split_ascii_whitespace().skip(1);
        let parent = fields.next()?.parse().ok()?;
        let group = fields.next()?.parse().ok()?;
        let foreground = fields.nth(2)?.parse().ok()?;
        Some((parent, group, foreground))
    }

    /// The boot time in whole Unix seconds from a `/proc/stat` text (its `btime` line).
    #[must_use]
    pub fn btime(stat: &str) -> Option<u64> {
        let b = stat.lines().find_map(|l| l.strip_prefix("btime "))?.trim();
        (!b.is_empty() && b.bytes().all(|c| c.is_ascii_digit()))
            .then(|| b.parse().ok())
            .flatten()
    }

    /// A start `ticks` clock ticks (`per_second` a second) after a boot at Unix second `boot`,
    /// in whole Unix seconds, cut short: `ps -o lstart`'s reckoning.
    #[must_use]
    pub const fn start_s(boot: u64, ticks: u64, per_second: u64) -> u64 {
        boot.saturating_add(ticks / if per_second == 0 { 1 } else { per_second })
    }

    /// The parent pid in a `/proc/<pid>/status` text.
    #[must_use]
    pub fn parent(status: &str) -> Option<u32> {
        let p = status.lines().find_map(|l| l.strip_prefix("PPid:"))?.trim();
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    }

    /// Returns `link` unless the kernel marks it ` (deleted)`.
    #[must_use]
    pub fn live_exe(link: std::path::PathBuf) -> Option<std::path::PathBuf> {
        (!link.as_os_str().as_encoded_bytes().ends_with(b" (deleted)")).then_some(link)
    }

    /// The descriptor `AT_EXECFD` names in a `/proc/<pid>/auxv`, the kernel's native-endian
    /// 64-bit `(type, value)` pairs up to `AT_NULL`: the program `binfmt_misc` opened and
    /// handed the interpreter it started (its `O` flag), at its start.
    #[must_use]
    pub fn exec_fd(auxv: &[u8]) -> Option<i32> {
        const AT_NULL: u64 = 0;
        const AT_EXECFD: u64 = 2;
        for pair in auxv.as_chunks::<16>().0 {
            let (kind, value) = pair.split_at(8);
            match u64::from_ne_bytes(kind.try_into().ok()?) {
                AT_NULL => return None,
                AT_EXECFD => return i32::try_from(u64::from_ne_bytes(value.try_into().ok()?)).ok(),
                _ => {}
            }
        }
        None
    }

    /// Whether a `/proc/<pid>/fdinfo/<fd>` text says the kernel opened the file to exec it: its
    /// `flags:`, octal digits only, carry `__FMODE_EXEC` (`0o40`), which no program's `open`
    /// can set.
    #[must_use]
    pub fn exec_opened(fdinfo: &str) -> bool {
        const FMODE_EXEC: u32 = 0o40;
        fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("flags:"))
            .map(str::trim)
            .filter(|flags| !flags.is_empty() && flags.bytes().all(|b| matches!(b, b'0'..=b'7')))
            .and_then(|flags| u32::from_str_radix(flags, 8).ok())
            .is_some_and(|flags| flags & FMODE_EXEC != 0)
    }

    /// The inode a `/proc/<pid>/fdinfo/<fd>` text says the descriptor's file is: its `ino:`,
    /// decimal digits only, which the kernel prints from Linux 5.14 on.
    #[must_use]
    pub fn open_inode(fdinfo: &str) -> Option<u64> {
        fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("ino:"))
            .map(str::trim)
            .filter(|ino| !ino.is_empty() && ino.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()
    }

    /// Whether the `/proc/<pid>/maps` rows `maps` reads map `path`, which is not empty,
    /// executable: a row whose permissions (its second field) grant `x` and whose path (all
    /// after its fifth) is `path` as `maps` prints it, which writes a newline as `\012`. It
    /// reads a row at a time up to the first such row, and a read that fails finds none.
    #[must_use]
    pub fn maps_executable(mut maps: impl std::io::BufRead, path: &[u8]) -> bool {
        use std::io::{BufRead as _, Read as _};
        if path.is_empty() {
            return false;
        }
        let mut shown = Vec::with_capacity(path.len());
        for &byte in path {
            if byte == b'\n' {
                shown.extend_from_slice(b"\\012");
            } else {
                shown.push(byte);
            }
        }
        let mut row = Vec::new();
        loop {
            row.clear();
            if (&mut maps)
                .take(64 * 1024)
                .read_until(b'\n', &mut row)
                .map_or(true, |read| read == 0)
            {
                return false;
            }
            let mut rest = row.strip_suffix(b"\n").unwrap_or(&row);
            let mut perms: &[u8] = &[];
            for field in 0..5 {
                rest = rest.trim_ascii_start();
                let end = rest
                    .iter()
                    .position(u8::is_ascii_whitespace)
                    .unwrap_or(rest.len());
                let (word, after) = rest.split_at(end);
                if field == 1 {
                    perms = word;
                }
                rest = after;
            }
            if perms.get(2) == Some(&b'x') && rest.trim_ascii_start() == shown {
                return true;
            }
        }
    }

    /// The ignored-signal mask (`SigIgn:`, hex) in a `/proc/<pid>/status` text.
    #[must_use]
    pub fn ignored_signals(status: &str) -> Option<u64> {
        let mask = status.lines().find_map(|l| l.strip_prefix("SigIgn:"))?;
        u64::from_str_radix(mask.trim(), 16).ok()
    }

    /// The effective uid in a `/proc/<pid>/status` text.
    #[must_use]
    pub fn effective_uid(status: &str) -> Option<u32> {
        let line = status.lines().find(|l| l.starts_with("Uid:"))?;
        line["Uid:".len()..]
            .split_ascii_whitespace()
            .nth(1)
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()
    }

    /// The first instant of start tick `ticks` on the boot clock, at `per_second` ticks a second.
    #[must_use]
    pub fn tick_start(ticks: u64, per_second: u64) -> Option<BootClock> {
        if per_second == 0 {
            return None;
        }
        let nanos = u128::from(ticks) * 1_000_000_000 / u128::from(per_second);
        u64::try_from(nanos).ok().map(BootClock)
    }

    /// How far a start [`tick_start`] reads at `per_second` ticks a second can precede the real
    /// one, in nanoseconds: Linux counts a start in whole ticks, so the real start lies within
    /// one tick after the first instant of its tick; a tick, rounded up (`None` at 0 a second).
    #[must_use]
    pub fn tick_span(per_second: u64) -> Option<u64> {
        (per_second > 0).then(|| 1_000_000_000_u64.div_ceil(per_second))
    }

    /// `pid`'s `/proc/<pid>/stat` line, whole, when it names `pid` itself
    /// ([`aterm_sysprobe::jobs::linux::parse_stat`], whose command name runs to the last `)`).
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn stat_line(stat: &str, pid: i32) -> Option<aterm_sysprobe::jobs::linux::StatLine> {
        let open = stat.find(" (")?;
        let p = stat[..open].trim();
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<i32>().ok()? != pid {
            return None;
        }
        aterm_sysprobe::jobs::linux::parse_stat(stat)
    }

    /// A process that has exited and holds nothing: its row's task has exited (a zombie, `Z`,
    /// or dead, `X` and the older `x`) and no other thread runs. A main thread that exited
    /// while its other threads run reads `Z` too (`ps` shows `Zl`) with more than one thread,
    /// and that process still runs. Every reader here judges a row's life by this one rule.
    #[cfg(any(unix, windows))]
    fn exited(line: &aterm_sysprobe::jobs::linux::StatLine) -> bool {
        matches!(line.state, b'Z' | b'X' | b'x') && line.threads <= 1
    }

    /// Whether `stat`, `pid`'s `/proc/<pid>/stat`, names `pid` as a process that has exited
    /// and holds nothing ([`exited`]): a zombie its parent has not reaped.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn exited_row(stat: &str, pid: i32) -> bool {
        stat_line(stat, pid).is_some_and(|line| exited(&line))
    }

    /// `PF_FORKNOEXEC`, the task flag the kernel sets on a forked task and clears at its exec.
    #[cfg(any(unix, windows))]
    const PF_FORKNOEXEC: u64 = 0x40;

    /// Whether `stat`, `pid`'s `/proc/<pid>/stat`, shows a process that has run an exec since
    /// its fork: its flags (field 9) without [`PF_FORKNOEXEC`]. `None`: another pid's line, or
    /// one cut short.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn execd_row(stat: &str, pid: i32) -> Option<bool> {
        stat_line(stat, pid).map(|line| line.flags & PF_FORKNOEXEC == 0)
    }

    /// The thread-group id in a `/proc/<pid>/status` text: its `Tgid:` line.
    #[must_use]
    pub fn thread_group(status: &str) -> Option<i32> {
        let t = status.lines().find_map(|l| l.strip_prefix("Tgid:"))?.trim();
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse().ok())
            .flatten()
    }

    /// `pid`'s [`super::Lineage`] from its `stat`, its `status` and `stat` read again after
    /// `status`, for a reader whose effective uid is `euid`: both `stat` reads name `pid` with
    /// one start (a pid reused between them reads another), neither has exited ([`exited`]: a
    /// process whose main thread alone exited runs), the `status` is this uid's and names `pid`
    /// as its thread group (a thread's id is no process), and the fields are the second read's.
    /// Linux keeps no pid version, so the identity has none ([`super::Identity::idversion`]),
    /// and its birth and unique id are both its start in clock ticks after boot. A terminal is
    /// kept as the kernel numbers it (`tty_nr`), `None` for none.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn lineage(
        stat: &str,
        status: &str,
        again: &str,
        pid: i32,
        euid: u32,
    ) -> Option<super::Lineage> {
        let first = stat_line(stat, pid)?;
        let now = stat_line(again, pid)?;
        if now.start != first.start || exited(&first) || exited(&now) {
            return None;
        }
        if effective_uid(status)? != euid || thread_group(status)? != pid {
            return None;
        }
        Some(super::Lineage {
            identity: super::Identity {
                pid,
                birth: super::Birth(now.start),
                idversion: None,
            },
            uniqueid: now.start,
            parent: now.ppid,
            pgid: now.pgrp,
            launcher_version: None,
            tty: (now.tty_nr != 0).then_some(now.tty_nr),
        })
    }

    /// One `/proc/<pid>/stat` read of a group listing ([`group_of`]).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum StatRead {
        /// The file's text.
        Text(String),
        /// The process was gone by the read (`ENOENT`, `ESRCH`).
        Gone,
        /// The read failed another way, or the directory entry did not read.
        Unreadable,
    }

    /// The members of process group `pgid` among `rows` (each a pid and the read of its
    /// `/proc/<pid>/stat`): each row whose text names its own pid in `pgid`, unless it has exited
    /// ([`exited`]: an exited stage leader is a zombie in its group until it is reaped, and holds
    /// nothing). A gone row is skipped. The listing is complete only when `shows_every` is set
    /// ([`shows_every_process`]), every other row read and parsed, and at most
    /// [`super::MAX_GROUP_PIDS`] members were found (the first that many are kept).
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn group_of(
        rows: impl IntoIterator<Item = (i32, StatRead)>,
        pgid: u32,
        shows_every: bool,
    ) -> super::GroupMembers {
        let mut members = super::GroupMembers {
            pids: Vec::new(),
            complete: shows_every,
        };
        for (pid, read) in rows {
            let text = match read {
                StatRead::Text(text) => text,
                StatRead::Gone => continue,
                StatRead::Unreadable => {
                    members.complete = false;
                    continue;
                }
            };
            let Some(line) = stat_line(&text, pid) else {
                members.complete = false;
                continue;
            };
            if line.pgrp != pgid || exited(&line) {
                continue;
            }
            if members.pids.len() < super::MAX_GROUP_PIDS {
                members.pids.push(pid);
            } else {
                members.complete = false;
            }
        }
        members
    }

    /// How many rounds [`chased_group_of`] follows the ids allocated behind its walk.
    pub const CHASE_ROUNDS: usize = 64;

    /// How many allocated ids [`chased_group_of`] reads, over all its rounds.
    pub const CHASE_LIMIT: usize = 4096;

    /// The ids a pid namespace allocated between two reads of its last id (`ns_last_pid`),
    /// `from` and then `to`, in the order it allocated them. The kernel hands out the next free
    /// id at or past its cursor, wrapping below `pid_max` to its low ids, so every id allocated
    /// between the reads lies after `from` and at or before `to`, cyclically. `None` for a wrap
    /// with no `pid_max`, or more than `limit` ids.
    #[must_use]
    pub fn allocated_after(
        from: u32,
        to: u32,
        pid_max: Option<u32>,
        limit: usize,
    ) -> Option<Vec<i32>> {
        let ids: Vec<u32> = if to >= from {
            if usize::try_from(to - from).ok()? > limit {
                return None;
            }
            (from + 1..=to).collect()
        } else {
            let top = pid_max?;
            let above = top.saturating_sub(from.saturating_add(1));
            if usize::try_from(above.saturating_add(to)).ok()? > limit {
                return None;
            }
            (from.saturating_add(1)..top).chain(1..=to).collect()
        };
        ids.into_iter().map(|id| i32::try_from(id).ok()).collect()
    }

    /// [`group_of`] over a walk of `/proc` and then every id allocated behind it. A walk is no
    /// snapshot: a member it read live can fork and exit before the listing is judged, and one
    /// whose row is read after it forked can be a zombie by then, its child forked after the
    /// directory was read and so never listed. `from` is the namespace's last id read before the
    /// walk (`ns_last_pid`); each round then reads it again (`last_pid`) and reads the row of each
    /// id allocated since ([`allocated_after`], `pid_max` read on a wrap), in place of the walk's
    /// row for an id the walk listed (a bystander's then, freed and handed to a member's fork
    /// since), until a round allocated none. It is complete only when that round came, within
    /// [`CHASE_ROUNDS`] and [`CHASE_LIMIT`], `shows_every` holds, and [`group_of`] finds it whole.
    /// Even so it is no snapshot: the kernel numbers a fork (`ns_last_pid` moves) before `/proc`
    /// shows it, so a fork in flight as `from` is read, or an id read between its numbering and
    /// its showing, can be missed by a listing whose forker exits before its own row is read; an
    /// empty group is shown only by two whole listings in a row (`tether`'s census), which such a
    /// relay would have to outrun twice. Allocations anywhere in the namespace move
    /// `ns_last_pid`, so sustained churn (a hundred thousand ids a second, measured) can run out
    /// of rounds and leave a listing incomplete: a caller reads that as not shown empty, and lists
    /// again.
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn chased_group_of(
        walked: impl IntoIterator<Item = (i32, StatRead)>,
        from: Option<u32>,
        mut last_pid: impl FnMut() -> Option<u32>,
        mut pid_max: impl FnMut() -> Option<u32>,
        mut row: impl FnMut(i32) -> StatRead,
        pgid: u32,
        shows_every: bool,
    ) -> super::GroupMembers {
        let mut rows: std::collections::BTreeMap<i32, StatRead> = walked.into_iter().collect();
        let mut settled = false;
        if let Some(mut at) = from {
            let mut left = CHASE_LIMIT;
            for _ in 0..CHASE_ROUNDS {
                let Some(now) = last_pid() else {
                    break;
                };
                if now == at {
                    settled = true;
                    break;
                }
                let top = if now < at { pid_max() } else { None };
                let Some(ids) = allocated_after(at, now, top, left) else {
                    break;
                };
                left -= ids.len();
                for id in ids {
                    rows.insert(id, row(id));
                }
                at = now;
            }
        }
        group_of(rows, pgid, shows_every && settled)
    }

    /// Whether the `/proc` a `/proc/self/mountinfo` text mounts at `/proc` lists every process
    /// to every reader: each `proc` mount there sets no `hidepid`, or sets it `0` or `off`.
    /// Any other `hidepid` hides another user's processes, and this uid's own that it may not
    /// trace (a non-dumpable one), from the listing (`invisible`, `ptraceable`; `2`, `4`), or
    /// refuses their files (`noaccess`, `1`); and a text with no `proc` mount at `/proc` shows
    /// nothing. Both answer `false`.
    #[must_use]
    pub fn shows_every_process(mountinfo: &str) -> bool {
        let mut mounted = false;
        for line in mountinfo.lines() {
            let Some((mount, fs)) = line.split_once(" - ") else {
                continue;
            };
            if mount.split(' ').nth(4) != Some("/proc") || fs.split(' ').next() != Some("proc") {
                continue;
            }
            let hides = line
                .split([' ', ','])
                .filter_map(|word| word.strip_prefix("hidepid="))
                .any(|mode| !matches!(mode, "0" | "off"));
            if hides {
                return false;
            }
            mounted = true;
        }
        mounted
    }

    /// The start of `pid` from its `stat`, its `status` and `stat` read again after `status`,
    /// for a reader whose effective uid is `euid`: the start of its [`lineage`], on the boot
    /// clock at `per_second` ticks a second ([`tick_start`]).
    #[cfg(any(unix, windows))]
    #[must_use]
    pub fn start(
        stat: &str,
        status: &str,
        again: &str,
        pid: i32,
        euid: u32,
        per_second: u64,
    ) -> Option<BootClock> {
        tick_start(
            lineage(stat, status, again, pid, euid)?.uniqueid,
            per_second,
        )
    }
}

/// Reads the argv of a live process of this uid between two matching [`ProcessStart`] reads.
#[must_use]
pub fn read_process_argv(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
    {
        argv_of_same_process(pid, read_process_start, read_process_argv_inner)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
fn argv_of_same_process(
    pid: i32,
    mut start: impl FnMut(i32) -> Option<ProcessStart>,
    argv: impl FnOnce(i32) -> Option<Vec<std::ffi::OsString>>,
) -> Option<Vec<std::ffi::OsString>> {
    if pid <= 1 {
        return None;
    }
    let before = start(pid)?;
    let words = argv(pid)?;
    (start(pid)? == before).then_some(words)
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
const PROCESS_ARGV_MAX: usize = 8 * 1024 * 1024;
#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
const PROCESS_ARGV_WORDS: usize = 65_536;

#[cfg(target_os = "macos")]
fn read_process_argv_inner(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    argv_from_procargs2(&crate::procargs::read(u32::try_from(pid).ok()?)?)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_process_argv_inner(pid: i32) -> Option<Vec<std::ffi::OsString>> {
    argv_from_cmdline(&read_cmdline(u32::try_from(pid).ok()?)?)
}

/// `pid`'s `/proc/<pid>/cmdline`, at most 8 MiB: a longer read is unknown, never a prefix.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_cmdline(pid: u32) -> Option<Vec<u8>> {
    cmdline_as_run(pid).map(|(bytes, _)| bytes)
}

/// `pid`'s `/proc/<pid>/environ`, at most 8 MiB: a longer read is unknown, never a prefix. The
/// read is bounded, never sized: procfs `stat`s these files as empty.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_environ(pid: u32) -> Option<Vec<u8>> {
    read_proc_bounded(pid, c"environ").filter(|bytes| bytes.len() <= PROCESS_ARGV_MAX)
}

/// The path `pid`'s program was exec'd from.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_exec_path(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    if own_interpreter().is_some()
        && let Some((cmdline, true)) = cmdline_as_run(pid)
        && let Some(program) = cmdline.split(|&b| b == 0).next().filter(|w| !w.is_empty())
    {
        return Some(std::ffi::OsString::from_vec(program.to_vec()).into());
    }
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn cmdline_as_run(pid: u32) -> Option<(Vec<u8>, bool)> {
    let bytes = read_proc_bounded(pid, c"cmdline")?;
    cmdline_from_read(bytes, PROCESS_ARGV_MAX, || shared_interpreter(pid))
}

/// `/proc/<pid>/<name>`, opened under the pid's own directory, read to at most one byte past
/// [`PROCESS_ARGV_MAX`], so a reader can tell a whole file from one past its bound.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_proc_bounded(pid: u32, name: &std::ffi::CStr) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let dir = ProcDir::of(std::path::Path::new("/proc"), i32::try_from(pid).ok()?)?;
    let file = dir.open(name)?;
    let mut bytes = Vec::new();
    file.take((PROCESS_ARGV_MAX + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn cmdline_from_read(
    mut bytes: Vec<u8>,
    limit: usize,
    interpreter: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<(Vec<u8>, bool)> {
    if bytes.len() > limit {
        return None;
    }
    if let Some(interpreter) = interpreter()
        && bytes.get(interpreter.len()) == Some(&0)
        && bytes.starts_with(&interpreter)
    {
        bytes.drain(..=interpreter.len());
        return Some((bytes, true));
    }
    Some((bytes, false))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn own_interpreter() -> Option<&'static std::path::Path> {
    use std::sync::OnceLock;
    static OURS: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    OURS.get_or_init(|| {
        translator()?;
        match crate::holders::exe_of(std::process::id()) {
            crate::holders::Exe::At(path) => Some(path),
            crate::holders::Exe::Unnamed | crate::holders::Exe::Exited => None,
        }
    })
    .as_deref()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn shared_interpreter(pid: u32) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt as _;
    let ours = own_interpreter()?;
    match crate::holders::exe_of(pid) {
        crate::holders::Exe::At(theirs) if theirs == ours => {
            Some(theirs.into_os_string().into_vec())
        }
        _ => None,
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn argv_from_procargs2(bytes: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;
    if bytes.len() > PROCESS_ARGV_MAX {
        return None;
    }
    let p = crate::procargs::parse(bytes)?;
    if !p.whole() || p.argc == 0 || p.argc > PROCESS_ARGV_WORDS || p.exec_path.is_empty() {
        return None;
    }
    Some(
        p.argv
            .iter()
            .map(|w| std::ffi::OsString::from_vec(w.to_vec()))
            .collect(),
    )
}

#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn argv_from_cmdline(bytes: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;
    if bytes.is_empty() || bytes.len() > PROCESS_ARGV_MAX {
        return None;
    }
    let body = bytes.strip_suffix(b"\0")?;
    let count = body.split(|&b| b == 0).count();
    if count > PROCESS_ARGV_WORDS {
        return None;
    }
    Some(
        body.split(|&b| b == 0)
            .map(|word| std::ffi::OsString::from_vec(word.to_vec()))
            .collect(),
    )
}

/// End this process by `SIGKILL`, here.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kill_self_now() -> ! {
    unsafe extern "C" {
        fn getpid() -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    // SAFETY: neither call takes a pointer, and `getpid` cannot fail.
    let sent = unsafe { kill(getpid(), SIGKILL) };
    if sent == 0 {
        loop {
            std::thread::park();
        }
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "unreachable: the kernel never refuses SIGKILL to this process's own pid"
    )]
    std::process::abort()
}

/// End this process here, running nothing (see the Unix twin).
#[cfg(windows)]
pub fn kill_self_now() -> ! {
    #[expect(
        clippy::disallowed_methods,
        reason = "Windows has no SIGKILL: abort ends the process at once, and no macOS crash report exists here"
    )]
    std::process::abort()
}

/// How long [`kill_group_until_empty`] signals a group that has not emptied (Apple's rounds; one
/// signal elsewhere needs no bound): a member `SIGKILL` cannot end (one blocked in the kernel, on
/// a stale mount) is left to the kernel after this rather than holding the caller.
#[cfg(all(unix, not(test)))]
const GROUP_KILL_CAP: std::time::Duration = std::time::Duration::from_secs(5);
/// A unit test's is the one-minute hang detector (`aterm_spec::wait::MUST_HAPPEN`, which this
/// crate sits below): its groups' members all die, but a loaded machine can keep a shell killed
/// mid-fork from exiting within 5 s, and a round that stops first leaves the fork's child running.
#[cfg(all(unix, test))]
const GROUP_KILL_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// The longest pause between two of [`kill_group_until_empty`]'s signals.
#[cfg(target_vendor = "apple")]
const GROUP_KILL_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// `SIGKILL` every member of process group `pgid`, whose leader the caller has spawned and not
/// yet reaped: the unreaped leader keeps its pid, and with it the group id, from being reused.
///
/// One signal reaches every member on Linux, whose fork restarts when a group signal lands
/// during it (`copy_process`). XNU's fork does not: the child of a fork a member is making as
/// the signal lands joins the group unsignalled and runs on once its parent dies, so a shell
/// forking its next command at that moment leaves the command running. On Apple targets this
/// therefore signals again, before the leader is reaped, until the kernel finds no member it
/// can signal, for at most five seconds: a member puts the child it forks in the group before it
/// can exit, so no fork outlives the last round, and XNU answers `EPERM` once the members left
/// are ones no signal reaches ([`settle_group_signal`]) and `ESRCH` once there are none. Elsewhere
/// it signals once.
///
/// # Errors
/// `InvalidInput` for a `pgid` of 1 or less, which names no one group (`kill(0)` and `kill(-1)`
/// reach far more), with nothing sent; otherwise the first signal's error, as
/// [`settle_group_signal`] leaves it: `ESRCH` (no such group); on Apple, `PermissionDenied` naming
/// a member that has not ended, or the kernel's `EPERM` for a group that could not be listed
/// whole; elsewhere, the kernel's `EPERM` (only members this user may not signal). A group whose
/// members have all ended is no error.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kill_group_until_empty(pgid: i32) -> std::io::Result<()> {
    kill_group_capped(pgid, GROUP_KILL_CAP)
}

/// [`kill_group_until_empty`] with the caller's bound on repeated signal rounds.
/// The caller must still own the unreaped group leader. This does not prove that the group
/// drained: a member blocked in the kernel may remain after the bound. Off Apple one signal
/// is sent. A test requiring eventual drain uses its one-minute hang detector as the cap.
///
/// # Errors
/// As [`kill_group_until_empty`]'s.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kill_group_until_empty_within(pgid: i32, cap: std::time::Duration) -> std::io::Result<()> {
    kill_group_capped(pgid, cap)
}

/// [`kill_group_until_empty`], its Apple rounds ending after at most `cap`.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn kill_group_capped(pgid: i32, cap: std::time::Duration) -> std::io::Result<()> {
    if pgid <= 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{pgid} names no one process group"),
        ));
    }
    if !signal_group(pgid) {
        settle_group_signal(pgid, std::io::Error::last_os_error())?;
    }
    #[cfg(target_vendor = "apple")]
    {
        let start = std::time::Instant::now();
        let mut pause = std::time::Duration::from_millis(1);
        while !cap.saturating_sub(start.elapsed()).is_zero() && signal_group(pgid) {
            let left = cap.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(pause.min(left));
            pause = (pause * 2).min(GROUP_KILL_PAUSE);
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = cap;
    Ok(())
}

/// Settles `error`, the failure of a signal to process group `pgid`: `Ok` only where the group
/// has nothing left to signal, else the failure, naming the member that keeps it.
///
/// On XNU a group signal's `EPERM` says only that the group exists and the signal reached no
/// member, which is three different things: every member has ended, a member is another user's,
/// or the group holds nothing but what a member's exec left. A member a `SIGKILL` just reached
/// counts as ended before it is a zombie: for an instant it is off the live list with its status
/// still running, and an orphan's parent is launchd, so neither `SZOMB` nor "our child" names it
/// (macOS 27, 144 of 30,000 group signals just after a `SIGKILL` of the group's last member, on a
/// loaded machine: `docs/measured/killpg-eperm-2026-11-09.md`). Only the group's listing tells
/// them apart. Linux signals a zombie, so its `EPERM` is always a real refusal and stands.
///
/// # Errors
/// `error` itself unless it is an Apple `EPERM`. An `EPERM` becomes `PermissionDenied` naming a
/// member that has not ended (or joined after the first listing), and stays the kernel's when the
/// group could not be listed whole.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
pub fn settle_group_signal(pgid: i32, error: std::io::Error) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    if error.raw_os_error() == Some(libc::EPERM)
        && let Ok(group) = u32::try_from(pgid)
    {
        match judge_group_refusal(group_members(group), process_presence, || {
            group_members(group)
        }) {
            GroupRefusal::Ended => return Ok(()),
            GroupRefusal::Held(pid) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "process group {pgid}: member {pid} has not ended and this user may not \
                         signal it"
                    ),
                ));
            }
            GroupRefusal::Unlisted => {}
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = pgid;
    Err(error)
}

/// What [`judge_group_refusal`] found behind a group signal's `EPERM`.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupRefusal {
    /// A whole listing found no process that has not ended: nothing is left to signal.
    Ended,
    /// This member has not ended, or appeared after the first listing.
    Held(i32),
    /// The listing was unreadable or cut short, which proves nothing.
    Unlisted,
}

/// Judges a group that refused a signal from `members`, its listing; `presence`, each member's
/// state; and `members_again`, a second listing. A member that could not be signalled may fork
/// after the first listing and exit before its own read, so a second whole listing must find no
/// one new.
#[cfg(target_vendor = "apple")]
fn judge_group_refusal(
    members: Option<GroupMembers>,
    mut presence: impl FnMut(i32) -> ProcessPresence,
    members_again: impl FnOnce() -> Option<GroupMembers>,
) -> GroupRefusal {
    let Some(first) = members.filter(|listing| listing.complete) else {
        return GroupRefusal::Unlisted;
    };
    if let Some(&held) = first.pids.iter().find(|pid| {
        !matches!(
            presence(**pid),
            ProcessPresence::Exited | ProcessPresence::Absent
        )
    }) {
        return GroupRefusal::Held(held);
    }
    let Some(again) = members_again().filter(|listing| listing.complete) else {
        return GroupRefusal::Unlisted;
    };
    match again.pids.iter().find(|pid| !first.pids.contains(pid)) {
        Some(&joined) => GroupRefusal::Held(joined),
        None => GroupRefusal::Ended,
    }
}

/// `kill(-pgid, SIGKILL)`: whether the kernel found a member of group `pgid` it could signal.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn signal_group(pgid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    // SAFETY: a plain syscall taking no pointer. `pgid` > 1, so it names one group, never this
    // process's own (0) or every process (-1).
    unsafe { kill(-pgid, SIGKILL) == 0 }
}

/// Returns this machine's configured host name.
#[must_use]
pub fn host_name() -> Option<String> {
    host_name_over(std::env::var_os("HOSTNAME"))
}

fn host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    env_host_name(env).or_else(os_host_name)
}

/// The OS's own name for this machine now, never `$HOSTNAME`.
#[must_use]
pub fn kernel_host_name() -> Option<String> {
    os_host_name()
}

fn env_host_name(env: Option<std::ffi::OsString>) -> Option<String> {
    let name = env?;
    let name = name.to_str()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The name the other machines of this one's LAN reach it by: a dotted HostName, else
/// `<LocalHostName>.local`, else the kernel's name.
///
/// Never `$HOSTNAME`, which a shell exported before a rename keeps stale in everything it starts;
/// and a bare HostName (`m8`, as aterm's name rule sets it) yields to the Bonjour name, which the
/// LAN resolves where a bare word does not.
#[must_use]
pub fn lan_host_name() -> Option<String> {
    lan_name(configured_host_name, bonjour_host_name, os_host_name)
}

/// [`lan_host_name`]'s order over its three readers, each asked only when the one before it
/// gives nothing usable.
fn lan_name(
    configured: impl FnOnce() -> Option<String>,
    bonjour: impl FnOnce() -> Option<String>,
    kernel: impl FnOnce() -> Option<String>,
) -> Option<String> {
    configured()
        .filter(|name| name.contains('.'))
        .or_else(|| bonjour().map(|name| format!("{name}.local")))
        .or_else(kernel)
}

#[cfg(target_os = "macos")]
mod sc {
    use core::ffi::{c_char, c_ulong, c_void};

    pub(super) type CFTypeRef = *const c_void;

    const UTF8: u32 = 0x0800_0100;

    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        pub(super) static kSCPropSystemHostName: CFTypeRef;
        #[cfg(test)]
        pub(super) static kSCPropSystemComputerName: CFTypeRef;
        pub(super) fn SCDynamicStoreCopyLocalHostName(store: CFTypeRef) -> CFTypeRef;
        pub(super) fn SCDynamicStoreKeyCreateComputerName(allocator: CFTypeRef) -> CFTypeRef;
        pub(super) fn SCDynamicStoreCopyValue(store: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetCString(string: CFTypeRef, buf: *mut c_char, len: isize, enc: u32) -> u8;
        pub(super) fn CFRelease(cf: CFTypeRef);
        pub(super) fn CFGetTypeID(cf: CFTypeRef) -> c_ulong;
        pub(super) fn CFDictionaryGetTypeID() -> c_ulong;
        pub(super) fn CFStringGetTypeID() -> c_ulong;
        pub(super) fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    }

    /// A host name `string` holds, trimmed.
    /// # Safety
    /// `string` is a live CFString.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) unsafe fn text(string: CFTypeRef) -> Option<String> {
        let mut buf = [0u8; 256];
        // SAFETY: `string` is a live CFString (the caller's promise); `buf`
        // is `buf.len()` writable bytes, which the call fills with the name
        // and its NUL, or leaves unread.
        let ok = unsafe {
            CFStringGetCString(string, buf.as_mut_ptr().cast(), buf.len() as isize, UTF8)
        };
        if ok == 0 {
            return None;
        }
        let len = buf.iter().position(|b| *b == 0)?;
        let name = std::str::from_utf8(buf.get(..len)?).ok()?.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

#[cfg(target_os = "macos")]
fn configured_host_name() -> Option<String> {
    // SAFETY: a CFString constant SystemConfiguration exports.
    setup_system(unsafe { sc::kSCPropSystemHostName })
}

#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn setup_system(key_name: sc::CFTypeRef) -> Option<String> {
    // SAFETY: the default allocator; the result is a new CFString this frame owns, or NULL.
    let key = unsafe { sc::SCDynamicStoreKeyCreateComputerName(std::ptr::null()) };
    if key.is_null() {
        return None;
    }
    // SAFETY: a NULL store asks with a temporary session; `key` is live. The
    // result is a new reference this frame owns, or NULL.
    let setup = unsafe { sc::SCDynamicStoreCopyValue(std::ptr::null(), key) };
    // SAFETY: this frame owns exactly the one reference the create returned.
    unsafe { sc::CFRelease(key) };
    if setup.is_null() {
        return None;
    }
    // SAFETY: `setup` is live until it is released below; a type id is a
    // plain query, and the value is read only once it is known to be a
    // CFString, borrowed from the dictionary (the Get rule) while it lives.
    let name = unsafe {
        if sc::CFGetTypeID(setup) == sc::CFDictionaryGetTypeID() {
            let value = sc::CFDictionaryGetValue(setup, key_name);
            if !value.is_null() && sc::CFGetTypeID(value) == sc::CFStringGetTypeID() {
                sc::text(value)
            } else {
                None
            }
        } else {
            None
        }
    };
    // SAFETY: this frame owns exactly the one reference the copy returned.
    unsafe { sc::CFRelease(setup) };
    name
}

#[cfg(not(target_os = "macos"))]
fn configured_host_name() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn bonjour_host_name() -> Option<String> {
    // SAFETY: a NULL store asks with a temporary session; the result is a new CFString reference
    // this frame owns, or NULL.
    let name = unsafe { sc::SCDynamicStoreCopyLocalHostName(std::ptr::null()) };
    if name.is_null() {
        return None;
    }
    // SAFETY: `name` is a live CFString.
    let text = unsafe { sc::text(name) };
    // SAFETY: this frame owns exactly the one reference the copy returned.
    unsafe { sc::CFRelease(name) };
    text
}

#[cfg(not(target_os = "macos"))]
fn bonjour_host_name() -> Option<String> {
    None
}

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn os_host_name() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut core::ffi::c_char, len: usize) -> i32;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is `buf.len()` writable bytes that outlive the call; the
    // call writes at most that many and a NUL within them on success.
    let rc = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(buf.get(..len)?).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(windows)]
fn os_host_name() -> Option<String> {
    const COMPUTER_NAME_DNS_HOSTNAME: u32 = 1;
    computer_name(COMPUTER_NAME_DNS_HOSTNAME)
        .or_else(|| env_host_name(std::env::var_os("COMPUTERNAME")))
}

/// Reads this machine's NetBIOS name from the OS (`GetComputerNameExW(ComputerNameNetBIOS)`).
#[cfg(windows)]
#[must_use]
pub fn netbios_host_name() -> Option<String> {
    netbios_host_name_over(std::env::var_os("COMPUTERNAME"))
}

#[cfg(windows)]
fn netbios_host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    const COMPUTER_NAME_NETBIOS: u32 = 0;
    computer_name(COMPUTER_NAME_NETBIOS).or_else(|| env_host_name(env))
}

#[cfg(windows)]
fn computer_name(format: u32) -> Option<String> {
    const CAPACITY: usize = 256;
    let mut buf = [0u16; CAPACITY];
    let mut len = CAPACITY as u32;
    // SAFETY: `buf` is `len` writable UTF-16 units and outlives the call. On
    // success the API writes the name and its NUL inside them and sets `len` to
    // the name's length without the NUL; on failure nothing written is read.
    let ok = unsafe { crate::win::ffi::GetComputerNameExW(format, buf.as_mut_ptr(), &mut len) };
    (ok != 0)
        .then(|| buf.get(..len as usize).map(String::from_utf16_lossy))
        .flatten()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

#[cfg(not(any(unix, windows)))]
fn os_host_name() -> Option<String> {
    None
}

#[cfg(all(test, windows))]
#[path = "process_netbios_name_tests.rs"]
mod netbios_name_tests;

#[cfg(all(test, unix))]
#[path = "process_host_name_tests.rs"]
mod host_name_tests;

#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(all(unix, not(target_vendor = "apple")))]
type SigSet = [u64; 16];

#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn stop_set() -> SigSet {
    unsafe extern "C" {
        fn sigemptyset(set: *mut SigSet) -> i32;
        fn sigaddset(set: *mut SigSet, sig: i32) -> i32;
    }
    let mut set = SigSet::default();
    // SAFETY: `sigemptyset` initialises `set`, and `sigaddset` adds valid signal numbers.
    unsafe {
        sigemptyset(&mut set);
        for sig in [15, 2, 1] {
            sigaddset(&mut set, sig);
        }
    }
    set
}

/// Installs a signal-waiter thread for `SIGTERM`, `SIGINT`, and `SIGHUP` that invokes `on_stop`.
#[cfg(unix)]
#[cfg(not(target_arch = "wasm32"))]
#[cfg_attr(trust_verify, trust::skip)]
pub fn take_stop_signals(name: &str, on_stop: impl FnOnce(i32) + Send + 'static) -> bool {
    unsafe extern "C" {
        fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
        fn sigwait(set: *const SigSet, sig: *mut i32) -> i32;
    }
    let (block, unblock) = if cfg!(target_os = "linux") || cfg!(target_os = "android") {
        (0, 1)
    } else {
        (1, 2)
    };
    let set = stop_set();
    // SAFETY: changes only the calling thread's mask, from a live, initialised set.
    unsafe { pthread_sigmask(block, &set, std::ptr::null_mut()) };
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let set = stop_set();
            let mut sig = 0;
            // SAFETY: waits on a live, initialised set and writes one int.
            while unsafe { sigwait(&set, &mut sig) } != 0 {}
            on_stop(sig);
        });
    if spawned.is_err() {
        // SAFETY: as above.
        unsafe { pthread_sigmask(unblock, &set, std::ptr::null_mut()) };
        return false;
    }
    true
}

static ALLOCATOR_RELIEF_HOOK: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register a process-allocator hook that [`release_free_memory`] calls alongside the OS trim.
pub fn register_allocator_relief(hook: fn()) {
    let _ = ALLOCATOR_RELIEF_HOOK.set(hook);
}

/// Ask the process allocator to return unused free pages to the operating system.
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_free_memory() {
    if let Some(&hook) = ALLOCATOR_RELIEF_HOOK.get() {
        hook();
    }
    #[cfg(target_vendor = "apple")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: a null zone and a zero goal ask libsystem_malloc to relieve every
        // registered zone of its cached reusable pages; no caller pointer is dereferenced.
        let _ = unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: `malloc_trim(0)` asks glibc's allocator to release free pages in its
        // arenas without touching live allocations.
        let _ = unsafe { malloc_trim(0) };
    }
}

#[cfg(test)]
#[path = "process_start_tests.rs"]
mod start_tests;

#[cfg(all(test, unix))]
#[path = "process_kill_self_tests.rs"]
mod kill_self_tests;

#[cfg(all(test, unix))]
#[path = "process_presence_tests.rs"]
mod presence_tests;

#[cfg(test)]
#[path = "process_end_proven_tests.rs"]
mod end_proven_tests;

#[cfg(all(test, windows))]
#[path = "process_tests.rs"]
mod tests;

/// What a process's own `/proc` directory says it runs, judged on a made `/proc`, so the
/// Linux read's composition holds on every platform.
#[cfg(all(test, unix))]
#[path = "process_program_tests.rs"]
mod program_tests;

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "process_parent_and_path_tests.rs"]
mod parent_and_path_tests;

#[cfg(all(test, unix))]
#[path = "process_argv_tests.rs"]
mod argv_tests;

#[cfg(all(
    test,
    any(
        target_vendor = "apple",
        all(target_os = "linux", target_pointer_width = "64")
    )
))]
#[path = "process_sleep_probe_tests.rs"]
mod sleep_probe_tests;

/// [`settle_group_signal`]: XNU's `EPERM` over groups with nothing left to signal, and over one
/// with a live member it may not touch.
#[cfg(all(test, target_vendor = "apple"))]
#[path = "process_group_refusal_tests.rs"]
mod group_refusal_tests;
