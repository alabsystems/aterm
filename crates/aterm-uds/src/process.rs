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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The pid.
    pub pid: i32,
    /// Its birth, which tells it from the next process given the same number.
    pub birth: Birth,
    /// Its pid version, incremented on exec (`None` where the kernel keeps none: Linux).
    pub idversion: Option<i32>,
}

impl Identity {
    /// Formats the wire token `<pid>@<birth>:<idversion>`, or `<pid>@<birth>` with no version.
    #[must_use]
    pub fn token(&self) -> String {
        match self.idversion {
            Some(version) => format!("{}@{}:{version}", self.pid, self.birth.0),
            None => format!("{}@{}", self.pid, self.birth.0),
        }
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
    let deadline = std::time::Instant::now() + grace;
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

#[path = "process_kin.rs"]
mod kin;
#[cfg(any(unix, windows))]
pub use kin::held_among;
pub use kin::{
    GroupMembers, HeldNow, Lineage, MARKER_VARS, MAX_GROUP_PIDS, Markers, TRUST_CHECK_VAR,
    VERIFY_RUN_VAR, at_program_check, boot_id, group_listing_can_be_whole, group_members, held_age,
    held_programs, launchd_idversion, launched_before, pid_namespace, process_serial,
    read_identity, read_lineage, read_process_markers, thread_ran,
};

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
mod short_info_tests {
    use super::{BsdShortInfo, child_pids, read_short_info};

    #[test]
    fn this_process_reads_its_own_short_record_back() {
        let me = i32::try_from(std::process::id()).expect("pid");
        let info = read_short_info(me).expect("this process reads");
        assert_eq!(info.pbsi_pid, me.unsigned_abs());
        // SAFETY: plain getters with no arguments.
        let (ppid, pgid) = unsafe { (libc::getppid(), libc::getpgrp()) };
        assert_eq!(info.pbsi_ppid, ppid.unsigned_abs());
        assert_eq!(info.pbsi_pgid, pgid.unsigned_abs());
        assert_ne!(info.pbsi_status, BsdShortInfo::SZOMB);
        assert_ne!(
            info.pbsi_flags & BsdShortInfo::PROC_FLAG_EXEC,
            0,
            "a test binary was exec'd"
        );
        assert!(read_short_info(0).is_none() && read_short_info(-1).is_none());
        struct StandIn(crate::spawnfd::Child);
        impl Drop for StandIn {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = StandIn(
            crate::spawnfd::command("/bin/sleep")
                .arg("30")
                .spawn()
                .expect("spawn"),
        );
        let pid = i32::try_from(child.0.id()).expect("pid");
        let listed = child_pids(me).expect("the table answers").contains(&pid);
        let _ = child.0.kill();
        let _ = child.0.wait();
        assert!(listed, "a spawned child is listed");
    }
}

/// See the Apple twin, whose contract this keeps.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_process_path(pid: i32) -> Option<std::path::PathBuf> {
    let start = read_process_start(pid)?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let exe = procfs::live_exe(exe)?;
    (read_process_start(pid)? == start).then_some(exe)
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
}

#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn read_bsdinfo(pid: i32) -> Option<BsdInfo> {
    const PROC_PIDTBSDINFO: i32 = 3;
    const SIZE: usize = 136;
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
    let kernel = std::fs::read_to_string("/proc/sys/kernel/arch").ok()?;
    let (kernel, code) = (kernel.trim(), std::env::consts::ARCH);
    if kernel.is_empty() || kernel == code {
        return None;
    }
    let rosetta =
        std::fs::read_to_string("/proc/cpuinfo").is_ok_and(|info| info.contains("VirtualApple"));
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
/// `stat` and `status` need hold no UTF-8; every field a reader here parses is ASCII.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
pub(crate) fn read_proc_text(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
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
    read_proc_text(std::path::Path::new("/proc/self/mountinfo"))
        .is_ok_and(|mounts| procfs::shows_every_process(&mounts))
}

/// One `/proc/<pid>/stat` read of a group listing ([`stat_read`]).
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn stat_row(path: &std::path::Path) -> procfs::StatRead {
    stat_read(read_proc_text(path))
}

/// A `/proc/<pid>/stat` read as a group listing takes it: its text, gone (`ENOENT`, `ESRCH`),
/// or unreadable (any other error). Under a `hidepid=1` mount another user's `/proc/<pid>` is
/// listed and its files answer `EPERM`, so such a row is unreadable and no listing there is
/// whole; under `hidepid=2` the kernel lists no such directory at all.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn stat_read(read: std::io::Result<String>) -> procfs::StatRead {
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
        if stat[..open].trim().parse::<i32>().ok()? != pid {
            return None;
        }
        let close = stat.rfind(')')?;
        if close < open {
            return None;
        }
        let mut fields = stat[close + 1..].split_ascii_whitespace();
        let state = fields.next()?.chars().next()?;
        let start = fields.nth(18)?.parse().ok()?;
        Some((state, start))
    }

    /// The parent pid in a `/proc/<pid>/status` text.
    #[must_use]
    pub fn parent(status: &str) -> Option<u32> {
        status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))?
            .trim()
            .parse()
            .ok()
    }

    /// Returns `link` unless the kernel marks it ` (deleted)`.
    #[must_use]
    pub fn live_exe(link: std::path::PathBuf) -> Option<std::path::PathBuf> {
        (!link.as_os_str().as_encoded_bytes().ends_with(b" (deleted)")).then_some(link)
    }

    /// The effective uid in a `/proc/<pid>/status` text.
    #[must_use]
    pub fn effective_uid(status: &str) -> Option<u32> {
        let line = status.lines().find(|l| l.starts_with("Uid:"))?;
        line["Uid:".len()..]
            .split_ascii_whitespace()
            .nth(1)?
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

    /// `pid`'s `/proc/<pid>/stat` line, whole, when it names `pid` itself
    /// ([`aterm_sysprobe::jobs::linux::parse_stat`], whose command name runs to the last `)`).
    #[cfg(any(unix, windows))]
    fn stat_line(stat: &str, pid: i32) -> Option<aterm_sysprobe::jobs::linux::StatLine> {
        let open = stat.find(" (")?;
        if stat[..open].trim().parse::<i32>().ok()? != pid {
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

    /// The thread-group id in a `/proc/<pid>/status` text: its `Tgid:` line.
    #[must_use]
    pub fn thread_group(status: &str) -> Option<i32> {
        status
            .lines()
            .find_map(|l| l.strip_prefix("Tgid:"))?
            .trim()
            .parse()
            .ok()
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
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let dir = std::fs::File::open(format!("/proc/{pid}")).ok()?;
    // SAFETY: `dir` is open for the call and the name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `openat` just returned `fd`, which nothing else owns.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
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

/// The name the other machines of this one's LAN reach it by.
#[must_use]
pub fn lan_host_name() -> Option<String> {
    lan_host_name_over(std::env::var_os("HOSTNAME"))
}

fn lan_host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    env_host_name(env)
        .or_else(configured_host_name)
        .or_else(|| bonjour_host_name().map(|name| format!("{name}.local")))
        .or_else(os_host_name)
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
mod netbios_name_tests {
    #[test]
    fn the_netbios_name_is_read_from_the_os() {
        let name = super::netbios_host_name_over(None).expect("the OS names this machine");
        assert!(name.chars().count() <= 15, "{name}");
        assert_eq!(
            super::netbios_host_name_over(Some("elsewhere.example".into())).as_deref(),
            Some(name.as_str())
        );
        if let Ok(env) = std::env::var("COMPUTERNAME") {
            assert!(env.trim().eq_ignore_ascii_case(&name), "{env} / {name}");
        }
    }
}

#[cfg(all(test, unix))]
mod host_name_tests {
    #[test]
    fn the_hostname_override_wins_and_a_blank_one_falls_to_the_os() {
        assert_eq!(
            super::host_name_over(Some(" my-override.local ".into())).as_deref(),
            Some("my-override.local")
        );
        assert_eq!(
            super::host_name_over(Some("  ".into())),
            super::os_host_name()
        );
        assert_eq!(super::host_name_over(None), super::os_host_name());
        assert_eq!(
            super::lan_host_name_over(Some(" my-override.local ".into())).as_deref(),
            Some("my-override.local"),
            "the override names the machine to its LAN too"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_lan_name_is_the_configured_name_else_the_bonjour_name() {
        let scutil = |key: &str| {
            let out = crate::spawnfd::command("/usr/sbin/scutil")
                .args(["--get", key])
                .output()
                .expect("run scutil");
            let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            (out.status.success() && !name.is_empty()).then_some(name)
        };
        let configured = scutil("HostName");
        assert_eq!(super::configured_host_name(), configured);
        // SAFETY: a CFString constant SystemConfiguration exports.
        let computer = super::setup_system(unsafe { super::sc::kSCPropSystemComputerName });
        assert!(computer.is_some(), "the read gives the ComputerName");
        assert_eq!(computer, scutil("ComputerName"));
        let want = configured
            .or_else(|| scutil("LocalHostName").map(|local| format!("{local}.local")))
            .or_else(super::os_host_name);
        assert_eq!(super::lan_host_name_over(None), want);
    }

    #[test]
    fn the_host_name_is_the_kernels_node_name() {
        let ours = super::os_host_name().expect("gethostname names this machine");
        let uname = crate::spawnfd::command("/usr/bin/uname")
            .arg("-n")
            .output()
            .or_else(|_| crate::spawnfd::command("/bin/uname").arg("-n").output())
            .expect("run uname -n");
        assert_eq!(ours, String::from_utf8_lossy(&uname.stdout).trim());
    }
}

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

#[cfg(test)]
mod start_tests {
    use super::{BootClock, procfs};

    #[test]
    fn a_filetime_is_a_birth_counted_from_the_unix_epoch() {
        use super::{ProcessBirth, birth_of_filetime};
        const EPOCH: u64 = 116_444_736_000_000_000;
        let at = |seconds, microseconds| {
            Some(ProcessBirth {
                seconds,
                microseconds,
            })
        };
        assert_eq!(birth_of_filetime(EPOCH), at(0, 0));
        assert_eq!(birth_of_filetime(EPOCH + 9), at(0, 0));
        assert_eq!(
            birth_of_filetime(EPOCH + 17_900_000_000_000_000 + 1_234_567),
            at(1_790_000_000, 123_456)
        );
        assert_eq!(birth_of_filetime(EPOCH - 1), None);
    }

    const TRICKY: &str = "7 (a) b (c) S 1 1 1 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 51832760 \
         7032832 463 18446744073709551615 205458244435968 205458248030472 281474793899808 0 0 0 \
         0 128 0 1 0 0 17 0 0 0 0 0 0 205458248044264 205458248115556 205458772447232 \
         281474793901609 281474793901860 281474793901860 281474793902058 0\n";
    const ZOMBIE: &str = "7 (a) b (c) Z 1 1 1 0 -1 4227148 140 0 0 0 0 0 0 0 20 0 1 0 51832760 0 \
         0 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
    const CAT: &str = "13 (cat) R 1 1 1 0 -1 4194304 43 0 0 0 0 0 0 0 20 0 1 0 51831871 2424832 \
         192 18446744073709551615 218051104473088 218051104506384 281474237451360 0 0 0 0 0 0 0 \
         0 0 17 1 0 0 0 0 0 218051104603072 218051104604896 218051165171712 281474237452043 \
         281474237452063 281474237452063 281474237452267 0\n";
    const STATUS_501: &str = "Name:\tcat\nState:\tR (running)\nTgid:\t13\nPid:\t13\n\
         Uid:\t501\t502\t501\t502\nGid:\t20\t20\t20\t20\n";

    /// `stat` field 22 counted from the command name's last `)`, the effective uid as the second
    /// `Uid:` id, a tick as its first instant; a process whose main thread alone exited has its
    /// start, and every failure case answers `None`. Negative control: judge the first read's
    /// state alone, and the process that died between the reads has a start.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_linux_start_is_read_off_proc_as_linux_writes_it() {
        assert_eq!(procfs::stat_start(TRICKY, 7), Some(('S', 51_832_760)));
        assert_eq!(procfs::stat_start(ZOMBIE, 7), Some(('Z', 51_832_760)));
        assert_eq!(procfs::stat_start(CAT, 13), Some(('R', 51_831_871)));
        assert_eq!(procfs::stat_start(CAT, 14), None, "another pid's line");
        assert_eq!(
            procfs::stat_start("13 (cat) R 1 1 1 0 -1 4194304 43 0 0", 13),
            None,
            "cut short"
        );
        assert_eq!(procfs::effective_uid(STATUS_501), Some(502));

        let at = |ticks: u64| Some(BootClock(ticks * 10_000_000));
        assert_eq!(
            procfs::start(CAT, STATUS_501, CAT, 13, 502, 100),
            at(51_831_871),
            "518318.71 s after boot"
        );
        let tricky_status = STATUS_501.replace("Tgid:\t13\nPid:\t13", "Tgid:\t7\nPid:\t7");
        assert_eq!(
            procfs::start(TRICKY, &tricky_status, TRICKY, 7, 502, 100),
            at(51_832_760)
        );
        let thread_status = STATUS_501.replace("Tgid:\t13\nPid:\t13", "Tgid:\t6\nPid:\t7");
        assert_eq!(
            procfs::start(TRICKY, &thread_status, TRICKY, 7, 502, 100),
            None,
            "a thread's id"
        );
        let main_thread_gone = ZOMBIE.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1);
        assert_eq!(
            procfs::start(
                &main_thread_gone,
                &tricky_status,
                &main_thread_gone,
                7,
                502,
                100
            ),
            at(51_832_760),
            "a process whose main thread alone exited (`Zl`) runs"
        );
        for (case, stat, again, euid, per_second) in [
            ("a zombie", ZOMBIE, ZOMBIE, 502, 100),
            ("dead", &*TRICKY.replace(") S ", ") X "), TRICKY, 502, 100),
            ("died between the reads", TRICKY, ZOMBIE, 502, 100),
            ("another effective uid", TRICKY, TRICKY, 501, 100),
            (
                "reused between the reads",
                TRICKY,
                &*CAT.replacen("13 ", "7 ", 1),
                502,
                100,
            ),
            ("no tick rate", TRICKY, TRICKY, 502, 0),
        ] {
            assert_eq!(
                procfs::start(stat, &tricky_status, again, 7, euid, per_second),
                None,
                "{case}"
            );
        }
        assert_eq!(
            procfs::tick_start(51_831_871, 100),
            Some(BootClock(518_318_710_000_000))
        );
        assert_eq!(procfs::tick_start(7, 3), Some(BootClock(2_333_333_333)));
    }

    /// A status whose thread group is `pid`, with effective uid 502.
    #[cfg(any(unix, windows))]
    fn status_of(pid: i32) -> String {
        STATUS_501.replace("Tgid:\t13\nPid:\t13", &format!("Tgid:\t{pid}\nPid:\t{pid}"))
    }

    /// A lineage off `/proc` as Linux writes it: the start tick as both birth and unique id, no
    /// pid version, and the group and terminal of the second read (a terminal the kernel prints
    /// negative is its `int` with the sign bit set); a process whose main thread alone exited
    /// reads, and every case in the table reads nothing. Negative controls: drop the second
    /// read's start or state check, and the reused pid or the process that died between the
    /// reads reads live; map the terminal to `None`, or read one that does not convert as none
    /// (`parse_stat`), and a member with one would be proven free of it; judge a row's state
    /// alone, whatever its threads, and the running process reads nothing.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_linux_lineage_is_read_off_proc_as_linux_writes_it() {
        use super::{Birth, Identity, Lineage};
        let status = status_of(7);
        assert_eq!(
            procfs::lineage(TRICKY, &status, TRICKY, 7, 502),
            Some(Lineage {
                identity: Identity {
                    pid: 7,
                    birth: Birth(51_832_760),
                    idversion: None,
                },
                uniqueid: 51_832_760,
                parent: 1,
                pgid: 1,
                launcher_version: None,
                tty: None,
            })
        );
        let grouped = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 500 500 34817 -1 ", 1);
        let read = procfs::lineage(&grouped, &status, &grouped, 7, 502).expect("a member");
        assert_eq!((read.pgid, read.tty), (500, Some(34817)), "{read:?}");
        let far = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 500 500 -2147448832 -1 ", 1);
        let read = procfs::lineage(&far, &status, &far, 7, 502).expect("a member");
        assert_eq!(
            read.tty,
            Some(0x8000_8800),
            "pts 524288, past 2^19: {read:?}"
        );
        let main_thread_gone = ZOMBIE.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1);
        assert_eq!(
            procfs::lineage(&main_thread_gone, &status, &main_thread_gone, 7, 502)
                .map(|l| l.identity),
            Some(Identity {
                pid: 7,
                birth: Birth(51_832_760),
                idversion: None,
            }),
            "a process whose main thread alone exited (`Zl`) runs"
        );
        let unread_tty = TRICKY.replacen(") S 1 1 1 0 -1 ", ") S 1 1 1 x -1 ", 1);
        let cut = &TRICKY[..40];
        for (case, stat, again, status, euid) in [
            ("a zombie", ZOMBIE, ZOMBIE, status.clone(), 502),
            (
                "dead",
                &*TRICKY.replace(") S ", ") X "),
                TRICKY,
                status.clone(),
                502,
            ),
            (
                "died between the reads",
                TRICKY,
                ZOMBIE,
                status.clone(),
                502,
            ),
            ("another effective uid", TRICKY, TRICKY, status.clone(), 501),
            (
                "reused between the reads",
                TRICKY,
                &*CAT.replacen("13 ", "7 ", 1),
                status.clone(),
                502,
            ),
            ("a thread's id", TRICKY, TRICKY, status_of(6), 502),
            (
                "a terminal that does not read",
                &*unread_tty,
                &*unread_tty,
                status.clone(),
                502,
            ),
            ("cut short", cut, cut, status.clone(), 502),
            ("another pid's line", CAT, CAT, status.clone(), 502),
        ] {
            assert_eq!(
                procfs::lineage(stat, &status, again, 7, euid),
                None,
                "{case}"
            );
        }
    }

    /// A group is listed from every `/proc/<pid>/stat`: its live members by `pgrp` (a command
    /// name of `x) (y` and a `Z` row with three threads among them), never its zombie leader or a
    /// dead row; a gone row is skipped, and an unreadable or garbled row, more members than the
    /// bound, or a `/proc` that does not list every process leave it incomplete. Negative
    /// controls: stop skipping zombies, and the exited leader is listed; skip every `Z` row, and
    /// the running process is left out; read an unreadable row as gone, or start the listing
    /// complete whatever `/proc` shows, and a listing with a hole reads complete.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_linux_group_is_listed_from_proc_without_its_zombies() {
        use procfs::StatRead;
        let row = |pid: i32, state: char, pgrp: u32| {
            let stat = format!(
                "{pid} (sh) {state} 1 {pgrp} {pgrp} 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0                  51832760 7032832 463 18446744073709551615 0 0 0 0 0 0 0 128 0 1 0 0 17 0 0 0
"
            );
            (pid, StatRead::Text(stat))
        };
        let odd = TRICKY.replacen("7 (a) b (c) S 1 1 1 ", "502 (x) (y) S 1 500 500 ", 1);
        let main_thread_gone = match row(506, 'Z', 500) {
            (pid, StatRead::Text(stat)) => (
                pid,
                StatRead::Text(stat.replacen(" 20 0 1 0 ", " 20 0 3 0 ", 1)),
            ),
            other => other,
        };
        let rows = || {
            vec![
                row(500, 'Z', 500),
                row(501, 'S', 500),
                (502, StatRead::Text(odd.clone())),
                row(503, 'S', 600),
                (504, StatRead::Gone),
                row(505, 'X', 500),
                main_thread_gone.clone(),
            ]
        };
        let listed = procfs::group_of(rows(), 500, true);
        assert_eq!(listed.pids, [501, 502, 506], "{listed:?}");
        assert!(listed.complete, "{listed:?}");
        let hidden = procfs::group_of(rows(), 500, false);
        assert_eq!(hidden.pids, [501, 502, 506], "{hidden:?}");
        assert!(
            !hidden.complete,
            "a /proc that does not list every process: {hidden:?}"
        );
        let mut holed = rows();
        holed.push((506, StatRead::Unreadable));
        assert!(
            !procfs::group_of(holed, 500, true).complete,
            "a row that did not read"
        );
        let mut garbled = rows();
        garbled.push((507, StatRead::Text("507 (sh) S 1 500".to_owned())));
        assert!(
            !procfs::group_of(garbled, 500, true).complete,
            "a row that did not parse"
        );
        let many = (1000..1257).map(|pid| row(pid, 'S', 500));
        let full = procfs::group_of(many, 500, true);
        assert_eq!(full.pids.len(), super::MAX_GROUP_PIDS);
        assert!(!full.complete, "past the bound");
    }

    /// `/proc` lists every process only under a `proc` mount at `/proc` with no `hidepid`, or
    /// `hidepid=0` or `off`; a `proc` mount elsewhere and other file systems under `/proc` do
    /// not count. Negative control: read `invisible` as showing every process, and a `/proc` that
    /// hides this uid's non-dumpable processes reads as listing them.
    #[test]
    fn a_proc_mounted_to_hide_processes_shows_no_listing_whole() {
        let line = |at: &str, fs: &str, options: &str| {
            format!(
                "22 1 0:21 / {at} rw,nosuid,nodev,noexec,relatime shared:12 - {fs} {fs} {options}\n"
            )
        };
        let table = |options: &str| {
            [
                line("/", "ext4", "rw"),
                line("/proc", "proc", options),
                line("/proc/sys/fs/binfmt_misc", "binfmt_misc", "rw,relatime"),
                line("/proc/kcore", "tmpfs", "rw,size=0k"),
                line("/mnt/proc", "proc", "rw,hidepid=invisible"),
            ]
            .concat()
        };
        for options in ["rw", "rw,hidepid=off", "rw,hidepid=0"] {
            assert!(procfs::shows_every_process(&table(options)), "{options}");
        }
        for options in [
            "rw,hidepid=invisible",
            "rw,hidepid=2",
            "rw,hidepid=ptraceable",
            "rw,hidepid=4",
            "rw,hidepid=noaccess",
            "rw,hidepid=1",
            "rw,gid=27,hidepid=invisible,subset=pid",
        ] {
            assert!(!procfs::shows_every_process(&table(options)), "{options}");
        }
        let stacked = [table("rw"), line("/proc", "proc", "rw,hidepid=invisible")].concat();
        assert!(
            !procfs::shows_every_process(&stacked),
            "a hiding mount over it"
        );
        assert!(
            !procfs::shows_every_process(&line("/", "ext4", "rw")),
            "no /proc mount"
        );
        assert!(!procfs::shows_every_process(""), "no mount table");
    }

    /// A listing's row is gone only when the kernel says its process is (`ENOENT`, `ESRCH`):
    /// `EPERM`, a `hidepid=1` mount's answer for another user's process, and every other error
    /// leave a hole. Negative control: read a refused permission as gone, and the listing with a
    /// hidden row reads whole.
    #[cfg(unix)]
    #[test]
    fn a_row_the_kernel_will_not_show_leaves_the_listing_partial() {
        use std::io::{Error, ErrorKind};
        let member = "501 (sh) S 1 500 500 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 51832760 0 0\n";
        assert_eq!(
            super::stat_read(Ok(member.to_owned())),
            procfs::StatRead::Text(member.to_owned())
        );
        for (case, error, whole) in [
            ("gone", Error::from(ErrorKind::NotFound), true),
            (
                "exited by its read",
                Error::from_raw_os_error(libc::ESRCH),
                true,
            ),
            (
                "hidepid=1's refusal",
                Error::from_raw_os_error(libc::EPERM),
                false,
            ),
            (
                "a refused open",
                Error::from_raw_os_error(libc::EACCES),
                false,
            ),
        ] {
            let listed = procfs::group_of(
                [
                    (501, super::stat_read(Ok(member.to_owned()))),
                    (9, super::stat_read(Err(error))),
                ],
                500,
                true,
            );
            assert_eq!(listed.pids, [501], "{case}");
            assert_eq!(listed.complete, whole, "{case}");
        }
    }

    /// A `/proc` file whose command name holds no UTF-8 still reads
    /// ([`super::read_proc_text`]): its lineage, start, group row and exited row are judged.
    /// Negative control: read the files with `read_to_string`, and the lineage reads nothing and
    /// the row is unreadable.
    #[cfg(unix)]
    #[test]
    fn a_proc_file_whose_command_name_is_not_utf8_still_reads() {
        use super::{read_proc_text, stat_row};
        let dir = aterm_tempfile::TempDir::new().expect("tempdir");
        let cut_name = |text: &str| {
            let mut bytes = text.as_bytes().to_vec();
            let at = bytes
                .windows(3)
                .position(|w| w == b"(a)")
                .expect("the name")
                + 1;
            bytes.splice(at..=at, [0xe3, 0x83]);
            bytes
        };
        let stat = dir.path().join("stat");
        let status = dir.path().join("status");
        std::fs::write(&stat, cut_name(TRICKY)).expect("stat");
        let mut raw_status = status_of(7).into_bytes();
        raw_status.splice(6..9, [0xe3, 0x83]);
        std::fs::write(&status, raw_status).expect("status");
        assert!(
            std::fs::read_to_string(&stat).is_err() && std::fs::read_to_string(&status).is_err(),
            "the fixture's names hold no UTF-8"
        );
        let read = |path: &std::path::Path| read_proc_text(path).expect("a file that reads");
        let lineage = procfs::lineage(&read(&stat), &read(&status), &read(&stat), 7, 502);
        assert_eq!(lineage.map(|l| l.uniqueid), Some(51_832_760), "{lineage:?}");
        assert!(
            procfs::start(&read(&stat), &read(&status), &read(&stat), 7, 502, 100).is_some(),
            "its start"
        );
        assert!(
            matches!(stat_row(&stat), procfs::StatRead::Text(_)),
            "a group row that reads"
        );
        let zombie = dir.path().join("zombie");
        std::fs::write(&zombie, cut_name(ZOMBIE)).expect("zombie");
        assert!(procfs::exited_row(&read(&zombie), 7), "an exited row");
        assert!(!procfs::exited_row(&read(&stat), 7), "a live row");
        assert_eq!(
            stat_row(&dir.path().join("gone")),
            procfs::StatRead::Gone,
            "a file gone by its read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_child_started_after_a_boot_clock_reading_reads_later() {
        use std::time::Duration;
        let own = i32::try_from(std::process::id()).unwrap();
        let Some(moment) = super::boot_clock_now() else {
            if super::translation() == super::Translation::Translated {
                a_child_started_after_a_wall_reading_reads_later();
                return;
            }
            let wired = cfg!(any(target_vendor = "apple", target_os = "linux"));
            assert!(!wired, "macOS and Linux have a boot clock");
            assert_eq!(super::read_process_start(own), None);
            return;
        };
        std::thread::sleep(Duration::from_millis(20));
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let pid = i32::try_from(child.id()).unwrap();
        let started = super::read_process_start(pid);
        let after = super::boot_clock_now().expect("the boot clock again");
        let wall = super::read_process_birth(pid);
        let _ = child.kill();
        let _ = child.wait();
        let started = started.expect("a live child's start");
        assert!(started.boot > moment, "{started:?} after {moment:?}");
        assert!(started.boot <= after, "{started:?} by {after:?}");
        assert_eq!(started.wall, wall, "the wall record beside it");
        assert_eq!(started.wall.is_some(), cfg!(target_vendor = "apple"));
        let mine = super::read_process_start(own).expect("this process's start");
        assert!(mine.boot < moment, "{mine:?} before {moment:?}");
        assert_eq!(super::read_process_start(pid), None, "an ended process");
        assert_eq!(super::read_process_start(1), None, "never pid 1");
    }

    #[cfg(unix)]
    fn a_child_started_after_a_wall_reading_reads_later() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let moment = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let started = super::read_process_start(i32::try_from(child.id()).unwrap());
        let _ = child.kill();
        let _ = child.wait();
        let birth = started
            .and_then(|s| s.wall)
            .expect("a translated reader still reads the start record");
        let wall = UNIX_EPOCH
            + Duration::from_secs(birth.seconds)
            + Duration::from_micros(birth.microseconds);
        assert!(wall > moment, "{wall:?} after {moment:?}");
    }

    #[test]
    fn a_translated_process_reads_no_boot_clock() {
        let native_ticks = 16_941_646_465_128;
        assert_eq!(
            super::apple_boot_clock(super::Translation::Native, || native_ticks),
            Some(BootClock(native_ticks))
        );
        assert_eq!(
            super::apple_boot_clock(super::Translation::Unknown, || native_ticks),
            Some(BootClock(native_ticks))
        );
        assert_eq!(
            super::apple_boot_clock(super::Translation::Translated, || {
                panic!("a translated process's reading is never taken")
            }),
            None
        );
        if cfg!(target_vendor = "apple") {
            assert_eq!(
                super::boot_clock_now().is_none(),
                super::translation() == super::Translation::Translated
            );
        } else {
            assert_eq!(super::translation(), super::Translation::Native);
        }
    }
}

#[cfg(all(test, unix))]
mod kill_self_tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "ATERM_UDS_TEST_KILL_SELF_CHILD";

    #[test]
    fn kill_self_now_dies_by_sigkill() {
        if std::env::var_os(CHILD_ENV).is_some() {
            super::kill_self_now();
        }
        let status = Command::new(std::env::current_exe().expect("the test binary"))
            .arg("process::kill_self_tests::kill_self_now_dies_by_sigkill")
            .arg("--exact")
            .arg("--test-threads=1")
            .env(CHILD_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("re-exec this test binary as the child");
        assert_eq!(status.signal(), Some(9), "the child's end: {status}");
    }
}

#[cfg(all(test, unix))]
mod presence_tests {
    use super::{ProcessPresence, process_presence, read_birth};
    use std::time::{Duration, Instant};

    /// A child that has exited and is not yet reaped (a zombie) reads `Exited`, a live one
    /// `Present`, and the zombie reads no birth. Negative control: answer `Present` whenever the
    /// pid answers signal zero, and the zombie reads `Present`.
    #[test]
    fn an_exited_unreaped_child_reads_exited() {
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        let pid = i32::try_from(child.id()).expect("pid");
        let live = process_presence(pid);
        let _ = child.kill();
        // The exit is waited for without reaping it, under the one-minute hang detector.
        let deadline = Instant::now() + Duration::from_secs(60);
        let exited = loop {
            // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is valid.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: a non-blocking `WNOWAIT` `waitid` of this test's own child into `info`.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    libc::id_t::try_from(pid).expect("pid"),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            // SAFETY: `info` was zeroed and then filled by `waitid`.
            if rc == 0 && unsafe { info.si_pid() } == pid {
                break true;
            }
            if rc != 0 || Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let zombie = process_presence(pid);
        let birth = read_birth(pid);
        let _ = child.wait();
        assert_eq!(live, ProcessPresence::Present, "a live child");
        assert!(exited, "the killed child exited within its hang detector");
        assert_eq!(zombie, ProcessPresence::Exited, "an unreaped zombie");
        assert_eq!(birth, None, "a zombie reads no birth");
    }
}

#[cfg(test)]
mod end_proven_tests {
    use super::{Birth, EndControl, EndRefusal, Ended, Identity, end_proven, signal_by_handle};
    use std::time::Duration;

    fn ident(pid: i32, idversion: i32) -> Identity {
        Identity {
            pid,
            birth: Birth(1_792_000_000_000_042),
            idversion: Some(idversion),
        }
    }

    struct Fake {
        reads: std::cell::RefCell<std::vec::IntoIter<Option<Identity>>>,
        signals: Vec<(i32, i32)>,
        naps: usize,
        refuses: Option<i32>,
    }
    impl Fake {
        fn new(reads: Vec<Option<Identity>>) -> Self {
            Self {
                reads: std::cell::RefCell::new(reads.into_iter()),
                signals: Vec::new(),
                naps: 0,
                refuses: None,
            }
        }
    }
    impl EndControl for Fake {
        fn identity(&mut self, _pid: i32) -> Option<Identity> {
            self.reads.borrow_mut().next().flatten()
        }
        fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
            if self.refuses == Some(sig) {
                return Err(std::io::Error::from_raw_os_error(3));
            }
            self.signals.push((token.pid, sig));
            Ok(())
        }
        fn nap(&mut self, _pause: Duration) {
            self.naps += 1;
        }
    }

    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;

    #[test]
    fn a_process_that_exits_within_the_grace_takes_one_sigterm() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), None]);
        let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("ended");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: false,
                gone: true
            }
        );
        assert_eq!(c.signals, [(4242, SIGTERM)]);
    }

    #[test]
    fn a_process_that_outlives_the_grace_is_sigkilled() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), None]);
        let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: true,
                gone: true
            }
        );
        assert_eq!(c.signals, [(4242, SIGTERM), (4242, SIGKILL)]);
    }

    #[test]
    fn a_moved_identity_before_the_term_sends_no_signal() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(ident(4242, 8)), Some(ident(4242, 8))]);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| true),
            Err(EndRefusal::Stale)
        );
        assert!(c.signals.is_empty(), "no signal after a moved identity");
    }

    #[test]
    fn a_moved_identity_before_the_kill_sends_no_second_signal() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(ident(4242, 8))]);
        let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: false,
                gone: false
            }
        );
        assert_eq!(c.signals, [(4242, SIGTERM)]);
    }

    #[test]
    fn a_lost_witness_before_the_kill_sends_no_second_signal() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t)]);
        let mut judged = 0;
        let out = end_proven(t, Duration::ZERO, &mut c, |_| {
            judged += 1;
            judged == 1
        })
        .expect("termed");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: false,
                gone: false
            }
        );
        assert_eq!(c.signals, [(4242, SIGTERM)]);
        assert_eq!(judged, 2, "the tier is judged before each signal");
    }

    #[test]
    fn a_lost_witness_sends_no_signal() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t)]);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| false),
            Err(EndRefusal::NotProven)
        );
        assert!(c.signals.is_empty());
    }

    #[test]
    fn an_exec_during_the_judgment_sends_no_signal() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(ident(4242, 8))]);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| true),
            Err(EndRefusal::Stale)
        );
        assert!(c.signals.is_empty(), "no signal after an exec mid-judgment");
    }

    #[test]
    fn a_process_that_execs_after_its_sigterm_is_not_gone() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), Some(ident(4242, 8))]);
        let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("termed");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: false,
                gone: false
            }
        );
        assert_eq!(c.signals, [(4242, SIGTERM)]);
        let mut reborn = ident(4242, 8);
        reborn.birth.0 += 1;
        let mut c = Fake::new(vec![Some(t), Some(t), Some(reborn)]);
        let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("termed");
        assert!(out.gone, "{out:?}");
    }

    #[test]
    fn a_sigkilled_process_that_still_reads_is_not_gone() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), Some(t)]);
        let out = end_proven(t, Duration::ZERO, &mut c, |_| true).expect("ended");
        assert_eq!(
            out,
            Ended {
                termed: true,
                killed: true,
                gone: false
            }
        );
    }

    #[test]
    fn a_signal_refused_for_a_process_just_gone_reads_gone() {
        let t = ident(4242, 7);
        let mut c = Fake::new(vec![Some(t), Some(t), None]);
        c.refuses = Some(SIGTERM);
        let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true);
        assert_eq!(
            out,
            Ok(Ended {
                termed: false,
                killed: false,
                gone: true
            })
        );
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t), Some(t), Some(t), None]);
        c.refuses = Some(SIGKILL);
        let out = end_proven(t, Duration::ZERO, &mut c, |_| true);
        assert_eq!(
            out,
            Ok(Ended {
                termed: true,
                killed: false,
                gone: true
            })
        );
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t)]);
        c.refuses = Some(SIGTERM);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| true),
            Err(EndRefusal::SignalFailed)
        );
    }

    #[test]
    fn a_token_is_pid_birth_and_version() {
        assert_eq!(
            ident(63452, 331207).token(),
            "63452@1792000000000042:331207"
        );
    }

    /// A token's two forms: Apple's `<pid>@<birth_us>:<idversion>`, Linux's `<pid>@<start_ticks>`
    /// with no version. Negative control: print `idversion.unwrap_or(0)` always, and the
    /// versionless token reads `…:0`, a version Linux never had.
    #[test]
    fn a_token_prints_its_version_only_where_the_kernel_keeps_one() {
        let apple = Identity {
            pid: 4242,
            birth: Birth(1_792_000_000_000_042),
            idversion: Some(7),
        };
        assert_eq!(apple.token(), "4242@1792000000000042:7");
        let linux = Identity {
            pid: 4242,
            birth: Birth(51_832_760),
            idversion: None,
        };
        assert_eq!(linux.token(), "4242@51832760");
    }

    /// What [`signal_by_handle`] did, in order, with what it passed: the pid opened and read,
    /// the handle and signal sent, the pid and signal killed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Step {
        Open(i32),
        Read(i32),
        Send(u8, i32),
        Kill(i32, i32),
    }

    /// The pid opened, read and signalled, over a handle that is a plain number: `open` answers
    /// `opened`, and the read answers `reads`.
    fn by_handle(
        token: Identity,
        opened: std::io::Result<u8>,
        reads: Option<Identity>,
    ) -> (std::io::Result<()>, Vec<Step>) {
        let log = std::cell::RefCell::new(Vec::new());
        let result = signal_by_handle(
            &token,
            15,
            |pid| {
                log.borrow_mut().push(Step::Open(pid));
                opened
            },
            |pid| {
                log.borrow_mut().push(Step::Read(pid));
                reads
            },
            |handle: &u8, sig| {
                log.borrow_mut().push(Step::Send(*handle, sig));
                Ok(())
            },
            |pid, sig| {
                log.borrow_mut().push(Step::Kill(pid, sig));
                Ok(())
            },
        );
        (result, log.into_inner())
    }

    /// The handle is opened on the token's pid first, that pid's identity re-read after it, and
    /// the signal goes through the handle only while the read still names the token. An open the
    /// kernel will not make (`ENOSYS`, or a seccomp filter's `EPERM`) takes the same read before
    /// a `kill` of that pid; an open that finds the process gone sends and reads nothing.
    /// Negative controls: ignore the re-read's answer, and the pid reused after the open is
    /// signalled; fall back on `ENOSYS` alone, and the filtered open sends nothing.
    #[test]
    fn a_handle_signal_reaches_only_the_process_the_handle_was_opened_on() {
        let token = Identity {
            pid: 4242,
            birth: Birth(51_832_760),
            idversion: None,
        };
        let reused = Identity {
            birth: Birth(51_900_000),
            ..token
        };
        let (opened, read) = (Step::Open(4242), Step::Read(4242));
        let (sent, log) = by_handle(token, Ok(3), Some(token));
        assert!(sent.is_ok(), "{sent:?}");
        assert_eq!(log, [opened, read, Step::Send(3, 15)]);
        let (sent, log) = by_handle(token, Ok(3), Some(reused));
        assert!(sent.is_err(), "a reused pid is refused");
        assert_eq!(log, [opened, read], "nothing sent to a reused pid");
        let (sent, log) = by_handle(token, Ok(3), None);
        assert!(sent.is_err());
        assert_eq!(log, [opened, read], "nothing sent to a gone pid");
        for kind in [
            std::io::ErrorKind::Unsupported,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let (sent, log) = by_handle(token, Err(kind.into()), Some(token));
            assert!(sent.is_ok(), "{kind:?}: {sent:?}");
            assert_eq!(log, [opened, read, Step::Kill(4242, 15)], "{kind:?}");
            let (sent, log) = by_handle(token, Err(kind.into()), Some(reused));
            assert!(sent.is_err(), "{kind:?}");
            assert_eq!(log, [opened, read], "{kind:?}: no kill of a reused pid");
        }
        let (sent, log) = by_handle(
            token,
            Err(std::io::Error::from_raw_os_error(3)),
            Some(token),
        );
        assert_eq!(sent.map_err(|e| e.raw_os_error()), Err(Some(3)));
        assert_eq!(log, [opened], "a gone process is neither read nor sent");
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn exit_code_259_is_not_alive() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit 259"])
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let status = child.wait().expect("wait");
        assert_eq!(status.code(), Some(259), "child exited with the sentinel");
        assert!(
            !super::pid_alive(pid),
            "exit code 259 must not be misread as STILL_ACTIVE"
        );
        drop(child);
    }

    #[test]
    fn a_child_born_after_a_wall_reading_reads_later_and_none_once_exited() {
        use crate::spawnfd::Stdio;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let on_wall = |birth: super::ProcessBirth| {
            UNIX_EPOCH
                + Duration::from_secs(birth.seconds)
                + Duration::from_micros(birth.microseconds)
        };
        let moment = SystemTime::now();
        std::thread::sleep(Duration::from_millis(50));
        let mut child = crate::spawnfd::command("cmd")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");
        let pid = i32::try_from(child.id()).expect("a pid");
        let born = super::read_process_birth(pid);
        let after = SystemTime::now();
        let _ = child.kill();
        let _ = child.wait();
        let born = on_wall(born.expect("a live child's birth"));
        assert!(born > moment, "{born:?} after {moment:?}");
        assert!(born <= after, "{born:?} by {after:?}");
        let own = i32::try_from(std::process::id()).expect("a pid");
        let mine = on_wall(super::read_process_birth(own).expect("this process's birth"));
        assert!(mine < moment, "{mine:?} before {moment:?}");
        assert_eq!(super::read_process_birth(pid), None, "an exited process");
        assert_eq!(super::read_process_birth(1), None, "never pid 1");
        assert_eq!(super::read_process_birth(0), None, "never pid 0");
        drop(child);
    }

    #[test]
    fn the_raw_creation_filetime_is_the_untruncated_procstart() {
        const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
        let own = i32::try_from(std::process::id()).expect("a pid");
        let ticks = super::read_creation_filetime(own).expect("this process's creation time");
        assert!(ticks > UNIX_EPOCH_TICKS, "after 1970: {ticks}");
        let decimal = ticks.to_string();
        assert_eq!(decimal.parse::<u64>(), Ok(ticks));
        let birth = super::read_process_birth(own).expect("birth");
        let since = ticks - UNIX_EPOCH_TICKS;
        assert_eq!(since / 10_000_000, birth.seconds);
        assert_eq!(since % 10_000_000 / 10, birth.microseconds);
        assert_eq!(super::read_creation_filetime(1), None, "never pid 1");
        assert_eq!(super::read_creation_filetime(0), None, "never pid 0");
    }
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod parent_and_path_tests {
    use super::{read_process_parent, read_process_path};

    #[test]
    fn this_process_reads_its_parent_and_its_executable() {
        let me = i32::try_from(std::process::id()).expect("pid");
        assert_eq!(
            read_process_parent(me),
            Some(std::os::unix::process::parent_id())
        );
        let path = read_process_path(me).expect("own path");
        let exe = std::env::current_exe().expect("current_exe");
        assert_eq!(
            path.canonicalize().expect("path exists"),
            exe.canonicalize().expect("exe exists")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reads_the_parent_line_and_refuses_a_deleted_exe_link() {
        use super::procfs;
        let status = "Name:\tcat\nUmask:\t0002\nState:\tR (running)\nTgid:\t13\nNgid:\t0\n\
                      Pid:\t13\nPPid:\t4417\nTracerPid:\t0\n";
        assert_eq!(procfs::parent(status), Some(4417));
        assert_eq!(procfs::parent("Name:\tcat\n"), None);
        let live = std::path::PathBuf::from("/opt/aterm/aterm");
        assert_eq!(procfs::live_exe(live.clone()), Some(live));
        assert_eq!(
            procfs::live_exe(std::path::PathBuf::from("/opt/aterm/aterm (deleted)")),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_on_a_deleted_binary_reads_no_path() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-uds-deleted-exe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let copy = dir.join("sleep-copy");
        let copied = crate::spawnfd::command("/bin/cp")
            .arg("/bin/sleep")
            .arg(&copy)
            .status()
            .expect("run cp");
        assert!(copied.success(), "cp /bin/sleep: {copied}");
        let mut child = crate::spawnfd::command(&copy)
            .arg("120")
            .spawn()
            .expect("spawn the copy");
        let pid = i32::try_from(child.id()).expect("pid");
        assert_eq!(read_process_path(pid), Some(copy.clone()), "the control");
        std::fs::remove_file(&copy).expect("delete the binary");
        let path = read_process_path(pid);
        let start = super::read_process_start(pid);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(path, None, "a deleted binary names no path");
        assert!(start.is_some(), "the process itself still reads");
    }
}

#[cfg(all(test, unix))]
mod argv_tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    fn mac_frame(argc: i32, tail: &[u8]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/aterm\0\0");
        bytes.extend_from_slice(tail);
        bytes
    }

    #[test]
    fn a_kernel_argv_buffer_keeps_words_and_stops_before_the_environment() {
        let expected = vec![
            OsString::from("/bin/aterm"),
            OsString::from("link"),
            OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
            OsString::new(),
        ];
        let bytes = mac_frame(4, b"/bin/aterm\0link\0name\xff with --durability relaxed\0\0--durability=relaxed\0SECRET=not-argv\0\0");
        assert_eq!(argv_from_procargs2(&bytes), Some(expected));
        let ordinary = mac_frame(1, b"/bin/aterm\0SECRET=not-argv\0");
        assert_eq!(
            argv_from_procargs2(&ordinary),
            Some(vec![OsString::from("/bin/aterm")])
        );
        let mut broken = Vec::new();
        for argc in [-1, 0, PROCESS_ARGV_WORDS as i32 + 1] {
            broken.push(mac_frame(argc, b"/bin/aterm\0"));
        }
        broken.push(mac_frame(2, b"/bin/aterm\0"));
        broken.push(mac_frame(1, b"/bin/aterm"));
        broken.push(vec![1, 0, 0]);
        let got: Vec<_> = broken
            .iter()
            .map(|bytes| argv_from_procargs2(bytes))
            .collect();
        assert!(got.iter().all(Option::is_none));
    }

    #[test]
    fn a_proc_cmdline_keeps_empty_and_non_utf8_words_but_refuses_a_prefix() {
        assert_eq!(
            argv_from_cmdline(b"aterm\0\0name\xff with --durability relaxed\0\0"),
            Some(vec![
                OsString::from("aterm"),
                OsString::new(),
                OsString::from_vec(b"name\xff with --durability relaxed".to_vec()),
                OsString::new()
            ])
        );
        assert_eq!(
            argv_from_cmdline(b"aterm\0"),
            Some(vec![OsString::from("aterm")])
        );
        assert_eq!(argv_from_cmdline(b""), None);
        assert_eq!(argv_from_cmdline(b"aterm\0truncated"), None);
        let at_limit = vec![0u8; PROCESS_ARGV_WORDS];
        assert_eq!(
            argv_from_cmdline(&at_limit).unwrap().len(),
            PROCESS_ARGV_WORDS
        );
        let over_limit = vec![0u8; PROCESS_ARGV_WORDS + 1];
        assert_eq!(argv_from_cmdline(&over_limit), None);

        let raw = b"/i\0t\xff\0\0".to_vec();
        let limit = raw.len();
        let (words, translated) =
            cmdline_from_read(raw.clone(), limit, || Some(b"/i".to_vec())).unwrap();
        assert!(translated, "the exact translator word was removed");
        assert_eq!(
            argv_from_cmdline(&words),
            Some(vec![OsString::from_vec(b"t\xff".to_vec()), OsString::new()])
        );
        assert!(words.len() < limit, "trimming could hide the sentinel");
        assert!(words.ends_with(b"\0"), "the prefix would look complete");
        assert_eq!(
            cmdline_from_read(raw.clone(), limit - 1, || {
                panic!("an oversized read must refuse before observing the interpreter")
            }),
            None
        );
        assert_eq!(
            cmdline_from_read(raw.clone(), limit, || None),
            Some((raw, false)),
            "a native read at the bound keeps every word"
        );
        let other = b"/ix\0t\0".to_vec();
        assert_eq!(
            cmdline_from_read(other.clone(), other.len(), || Some(b"/i".to_vec())),
            Some((other, false)),
            "a byte prefix without the NUL boundary names no translator word"
        );
    }

    #[test]
    fn an_argv_observation_needs_the_same_known_process_start_on_both_sides() {
        let old = ProcessStart {
            boot: BootClock(17),
            wall: None,
        };
        let new = ProcessStart {
            boot: BootClock(18),
            wall: None,
        };
        let words = || Some(vec![OsString::from("aterm"), OsString::new()]);
        let mut count = 0;
        assert_eq!(
            argv_of_same_process(
                71,
                |_| {
                    count += 1;
                    Some(old)
                },
                |_| words()
            ),
            words()
        );
        assert_eq!(count, 2);
        for after in [None, Some(new)] {
            let mut n = 0;
            assert_eq!(
                argv_of_same_process(
                    71,
                    |_| {
                        n += 1;
                        if n == 1 { Some(old) } else { after }
                    },
                    |_| words()
                ),
                None
            );
            assert_eq!(n, 2);
        }
        assert_eq!(
            argv_of_same_process(
                71,
                |_| None,
                |_| panic!("unknown identity must not read argv")
            ),
            None
        );
        for pid in [-1, 0, 1] {
            assert_eq!(
                argv_of_same_process(pid, |_| panic!("invalid pid"), |_| panic!("invalid pid")),
                None
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_childs_program_and_argv_read_as_given_and_leave_no_descriptor_behind() {
        const NAME: &str = "process::argv_tests::\
            a_childs_program_and_argv_read_as_given_and_leave_no_descriptor_behind";
        const PARK_ENV: &str = "ATERM_UDS_TEST_ARGV_PARK";
        if std::env::var_os(PARK_ENV).is_some() {
            let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
            return;
        }
        let exe = std::env::current_exe().expect("the test binary");
        let args = ["--exact", NAME, "--nocapture", "--test-threads=1"];
        let mut child = crate::spawnfd::command(&exe)
            .args(args)
            .env(PARK_ENV, "1")
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("the parked copy");
        let pid = i32::try_from(child.id()).expect("a pid");
        let given: Vec<OsString> = std::iter::once(exe.clone().into_os_string())
            .chain(args.iter().map(OsString::from))
            .collect();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut read = read_process_argv(pid);
        while read.as_ref() != Some(&given) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
            read = read_process_argv(pid);
        }
        let again = (0..64).all(|_| read_process_argv(pid) == read);
        let program = read_exec_path(child.id());
        let auxv = std::path::PathBuf::from(format!("/proc/{pid}/auxv"));
        let kept = std::fs::read_dir("/proc/self/fd")
            .expect("this process's descriptors")
            .flatten()
            .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|to| to == auxv))
            .count();
        drop(child.stdin.take());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(read, Some(given), "the parked copy's argv");
        assert_eq!(program, Some(exe), "the parked copy's program");
        assert!(again, "64 more reads answered the same");
        assert_eq!(kept, 0, "descriptors left open on {}", auxv.display());
    }
}

#[cfg(all(
    test,
    any(
        target_vendor = "apple",
        all(target_os = "linux", target_pointer_width = "64")
    )
))]
mod sleep_probe_tests {
    use super::{asleep_since_boot, continuous_ms, continuous_ns, last_sleep_ms};
    use std::time::{Duration, Instant};

    #[test]
    fn the_continuous_clock_counts_at_least_the_awake_time_between_two_reads() {
        let first = continuous_ns().expect("wired on this platform");
        let awake = Instant::now();
        while awake.elapsed() < Duration::from_millis(2) {}
        let between = awake.elapsed();
        let next = continuous_ns().expect("wired on this platform");
        let counted = Duration::from_nanos(next.checked_sub(first).expect("it never runs back"));
        assert!(counted >= between, "{counted:?} {between:?}");
        let ms = continuous_ms().expect("wired on this platform");
        assert!(ms >= next / 1_000_000, "{ms} {next}");
    }

    #[test]
    fn the_sleep_since_boot_reads_and_never_runs_back() {
        let first = asleep_since_boot().expect("wired on this platform");
        let next = asleep_since_boot().expect("wired on this platform");
        assert!(
            next + Duration::from_millis(1) >= first,
            "{first:?} {next:?}"
        );
        let since_boot = Duration::from_millis(continuous_ms().expect("the boot clock"));
        assert!(
            next <= since_boot + Duration::from_millis(1),
            "{next:?} {since_boot:?}"
        );
    }

    #[test]
    fn the_last_sleep_is_in_order() {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let last = last_sleep_ms();
        if let Some((slept, woke)) = last {
            assert!(
                0 < slept && slept < woke && woke <= now_ms + 1000,
                "{slept} {woke} {now_ms}"
            );
        }
        #[cfg(target_vendor = "apple")]
        if last.is_none() {
            let slept = super::sysctl_timeval_ms(c"kern.sleeptime")
                .expect("the kernel answers kern.sleeptime");
            let woke = super::sysctl_timeval_ms(c"kern.waketime")
                .expect("the kernel answers kern.waketime");
            assert!(slept == 0 || slept >= woke, "{slept} {woke}");
        }
    }
}
