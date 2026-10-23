// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process facts from the kernel.

/// What a process-existence query established, independently of readable identity data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessPresence {
    /// The pid exists, including when access to it is refused.
    Present,
    /// The kernel positively reported that no process has this pid.
    Absent,
    /// The query failed or this platform has no reader.
    Unknown,
}

/// Query one positive pid without delivering a signal. Only `ESRCH` proves absence.
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
        // SAFETY: a positive pid addresses one process; signal zero only queries existence.
        if unsafe { kill(pid, 0) } == 0 {
            return ProcessPresence::Present;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => ProcessPresence::Present,
            Some(libc::ESRCH) => ProcessPresence::Absent,
            _ => ProcessPresence::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        ProcessPresence::Unknown
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
        // Can't even query it: only "exists but is not ours" counts as alive.
        return unsafe { ffi::GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code: u32 = 0;
    let alive = unsafe { ffi::WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetExitCodeProcess(handle, &mut code) } != 0
        && code == STILL_ACTIVE;
    let _ = unsafe { ffi::CloseHandle(handle) };
    alive
}

/// A process's kernel start time: the identity that tells a pid from the next process given the same number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessBirth {
    /// Start time, unix seconds.
    pub seconds: u64,
    /// The microseconds within `seconds`.
    pub microseconds: u64,
}

/// Who a process is, re-read before every pid-addressed signal
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.4): a pid wraps in about ten minutes
/// (M14), so a token names the pid, its birth and its pid version; an observed mismatch
/// refuses the next signal. The token's wire form is
/// `<pid>@<birth_us>:<idversion>` (design §11 Q5); its reader is [`read_identity`], its
/// enforcer [`end_proven`]. The final identity read and pid-addressed signal are separate
/// kernel calls: an exec or pid reuse after that read cannot be excluded by this token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The pid.
    pub pid: i32,
    /// Its birth, which tells it from the next process given the same number.
    pub birth: ProcessBirth,
    /// Its pid version, which moves at each exec (M22), detecting changes observed before a
    /// signal. `0` on a platform with
    /// no version (Linux, where the token is `<pid>@<start_ticks>`: the leftover design's
    /// slice 7).
    pub idversion: i32,
}

impl Identity {
    /// The token's wire form, `<pid>@<birth_us>:<idversion>`: `birth_us` is the birth as whole
    /// microseconds, so it survives a round trip through text with no loss.
    #[must_use]
    pub fn token(&self) -> String {
        let micros = self
            .birth
            .seconds
            .saturating_mul(1_000_000)
            .saturating_add(self.birth.microseconds);
        format!("{}@{micros}:{}", self.pid, self.idversion)
    }
}

/// Why [`end_proven`] sent no signal, or sent one and the process stayed: the words
/// `sys left`'s `refused` line and verify's receipt print (design §6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndRefusal {
    /// The pid names another process now (gone, reused, or exec'd into another program): its
    /// birth or pid version has moved since the token was proven. No signal was sent.
    Stale,
    /// The fresh read no longer meets the tier's conditions (membership lost, a witness gone):
    /// no signal was sent.
    NotProven,
    /// A signal could not be delivered (the kernel refused it): the reason, for the words.
    SignalFailed,
}

/// What [`end_proven`] did: the signals it sent and whether the process was gone by the end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ended {
    /// A `SIGTERM` was sent.
    pub termed: bool,
    /// A `SIGKILL` was sent (the process outlived the grace).
    pub killed: bool,
    /// Identity reads no longer named the process: no identity, or another birth. A failed
    /// identity read also yields no identity; callers requiring confirmed absence must query
    /// [`process_presence`]. An exec under the same birth is not gone.
    pub gone: bool,
}

/// The kernel seam [`end_proven`] acts through: re-read a pid's identity, and signal it. The
/// shipped reader reads the kernel ([`EndControlLive`]); a test injects one whose identity
/// changes between the judgment and the signal, the negative control the `EndByProof` model
/// names, so no signal goes out.
pub trait EndControl {
    /// `pid`'s identity now, or `None` when gone, unreadable, or not of this uid.
    fn identity(&mut self, pid: i32) -> Option<Identity>;
    /// Send `sig` to `pid` (never a group; [`end_proven`] signals by pid alone).
    ///
    /// # Errors
    /// The kernel's own, when it refuses the signal.
    fn signal(&mut self, pid: i32, sig: i32) -> std::io::Result<()>;
    /// Wait `pause` before the next poll for the process to be gone. The shipped arm sleeps;
    /// a test's injects a no-op or a clock it controls, so a wait for gone is a hang detector,
    /// never a budget.
    fn nap(&mut self, pause: std::time::Duration);
}

/// The grace [`end_proven`] gives a process to exit after `SIGTERM` before it `SIGKILL`s it,
/// as verify's per-child `term_grace` is a bound rather than a wait (design §4.4): 10 s.
pub const END_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// How long [`end_proven`] naps between polls for a signalled process to be gone.
const END_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// End the one process `token` names, re-proving it before each signal (design §4.4). The
/// judgment is the tier's reads (`still_proven`, on the fresh state) bracketed by two identity
/// reads through `control` that must both equal `token`, so the tier judged the token's own
/// program and an exec or an exit while it read shows in the second read: a read that moved
/// refuses [`EndRefusal::Stale`], a tier lost [`EndRefusal::NotProven`]. Then `SIGTERM` by
/// pid, a poll within `grace` ([`END_GRACE`] for every shipped caller), the same judgment again
/// and `SIGKILL` by pid, and a poll within `grace` again. A read that moved ends the sequence
/// with nothing more sent, and a lost tier leaves the process unkilled for the caller to name.
///
/// `gone` describes identity reads, not confirmed absence: a missing read may have failed,
/// while a known different birth establishes that the original process left. A read of the
/// same birth under another pid version is the same process running another program. Never
/// `killpg`, never a bare pid
/// acted on without this re-read. The actor that calls it owns the whole sequence; nothing
/// waits for a later reading. `grace` is a bound, not a wait, so a caller may pass a shorter
/// one; a test passes [`std::time::Duration::ZERO`] to reach the `SIGKILL` with no real wait.
///
/// It is the `EndByProof` machine's one implementation (`crates/aterm-models-misc`, bound by
/// `crates/aterm-verify/tests/conformance_end_by_proof.rs`): every signal follows its own
/// judgment that held, and none follows a read whose identity moved or a tier that was lost.
/// This is an ordering of observed reads, not an atomic identity-conditioned kernel signal.
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
    if control.signal(pid, SIGTERM).is_err() {
        // `ESRCH` is a process that exited after the judgment: the read says which.
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
    // Still the token's program at the grace's end: judge again, then SIGKILL by pid.
    match judge(&mut *control) {
        Judged::Holds => {}
        Judged::Moved(now) => {
            ended.gone = gone(now);
            return Ok(ended);
        }
        Judged::Lost => return Ok(ended),
    }
    if control.signal(pid, SIGKILL).is_err() {
        ended.gone = gone(control.identity(pid));
        return Ok(ended);
    }
    ended.killed = true;
    // XNU tears a killed process down after the signal; a wedged one may stay.
    ended.gone = moved_within(control, token, grace).is_some_and(gone);
    Ok(ended)
}

/// One judgment of [`end_proven`]'s.
enum Judged {
    /// Both identity reads equal the token and the tier holds.
    Holds,
    /// An identity read no longer equals the token: what it read.
    Moved(Option<Identity>),
    /// The tier's conditions no longer hold.
    Lost,
}

/// Poll `token`'s pid within `grace` (a bound, not a wait) until a read no longer equals the
/// token: that read, or `None` when the pid still reads the token at the grace's end.
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

/// The shipped [`EndControl`]: it reads the kernel ([`read_identity`]), signals by pid
/// (`kill(2)`), and sleeps to nap. Every signal is one `kill` of one pid.
#[derive(Clone, Copy, Debug, Default)]
pub struct EndControlLive;

impl EndControl for EndControlLive {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        read_identity(pid)
    }
    // Skip: bottoms out at the `kill` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    fn signal(&mut self, pid: i32, sig: i32) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            // SAFETY: two integer arguments; `end_proven` re-read `pid`'s identity just before
            // this and signals one pid, never a group.
            if unsafe { kill(pid, sig) } == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (pid, sig);
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

/// Who a process descends from and which run started it (flavors 17/18, launchd's version, a
/// group's members, the three marker variables, this boot's id and the program-check
/// signature), moved here from aterm-gui's warm-spare escape scan. See [`kin`].
#[path = "process_kin.rs"]
mod kin;
pub use kin::{
    GroupMembers, Lineage, MARKER_VARS, MAX_GROUP_PIDS, Markers, VERIFY_RUN_VAR, at_program_check,
    boot_id, group_members, launchd_idversion, launched_before, read_identity, read_lineage,
    read_process_markers,
};
// The flavor-17/18 record readers exist only where XNU keeps those records (Apple).
#[cfg(target_vendor = "apple")]
pub use kin::{BsdInfoWithUniqId, UniqIdentifierInfo, bsdinfo_uniq, uniq_identifier};

/// Read the kernel's start-time record for `pid`, or `None` when there is no process of this uid to name there.
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
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let parent = procfs::parent(&status)?;
    (read_process_start(pid)? == start).then_some(parent)
}

/// See the Apple twin: no parent-pid primitive is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_parent(_pid: i32) -> Option<u32> {
    None
}

/// The executable the kernel records for `pid` (`proc_pidpath`), on the terms of [`read_process_birth`], and `None` when the kernel cannot name the file any more.
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
    // SAFETY: `buf` is MAX writable bytes; libproc writes at most that many and returns the path's length, or <= 0 on failure.
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
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_short_info(pid: i32) -> Option<BsdShortInfo> {
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const SIZE: i32 = std::mem::size_of::<BsdShortInfo>() as i32;
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<BsdShortInfo>::zeroed();
    // SAFETY: `info` is SIZE writable bytes of exactly the structure this flavor fills (size asserted above).
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
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

/// `pid`'s live children, at most [`CHILD_PIDS_MAX`], from libproc, or `None` when the table cannot answer.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn child_pids(pid: i32) -> Option<Vec<i32>> {
    let mut pids = [0i32; CHILD_PIDS_MAX];
    let bytes = i32::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is `bytes` writable bytes of pid slots; libproc writes at most that many and answers how many pids it wrote, or -1.
    let found = unsafe { proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
    let found = usize::try_from(found).ok()?.min(CHILD_PIDS_MAX);
    Some(pids[..found].iter().copied().filter(|p| *p > 0).collect())
}

#[cfg(all(test, target_vendor = "apple"))]
mod short_info_tests {
    use super::{BsdShortInfo, child_pids, read_short_info};

    /// The layout read back: this process's record names it, its parent and its group, and
    /// flags the `exec` that started this test binary; a child it spawns is listed among its
    /// children.
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

/// See the Apple twin: no executable-path primitive is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_path(_pid: i32) -> Option<std::path::PathBuf> {
    None
}

/// The `proc_bsdinfo` fields this module reads, for a live process of this uid.
#[cfg(target_vendor = "apple")]
struct BsdInfo {
    birth: ProcessBirth,
    parent: u32,
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)` for `pid`, on the terms
/// [`read_process_birth`] states.
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
    // SAFETY: `info` is SIZE writable bytes, exactly the structure this flavor fills; libproc returns the number of bytes it wrote.
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

/// The RAW creation `FILETIME` of `pid` — 100 ns ticks since 1601-01-01 UTC, as `GetProcessTimes` reports it, with no conversion.
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
    // SAFETY: `handle` is the live process handle opened above, and the four out-params are distinct writable `FILETIME`s that outlive the call.
    let read = unsafe { ffi::WaitForSingleObject(handle, 0) } == WAIT_TIMEOUT
        && unsafe { ffi::GetProcessTimes(handle, created, exited, kernel, user) } != 0;
    // SAFETY: closes the one handle this function opened.
    let _ = unsafe { ffi::CloseHandle(handle) };
    read.then(|| times[0].ticks())
}

/// A Windows `FILETIME` (100 ns ticks since 1601-01-01 UTC) as a
/// [`ProcessBirth`]; `None` before the Unix epoch.
#[cfg(any(windows, test))]
fn birth_of_filetime(ticks: u64) -> Option<ProcessBirth> {
    /// The ticks from 1601-01-01 to 1970-01-01.
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let since = ticks.checked_sub(UNIX_EPOCH_TICKS)?;
    Some(ProcessBirth {
        seconds: since / 10_000_000,
        microseconds: since % 10_000_000 / 10,
    })
}

/// See the Apple twin: no birth-record primitive is wired here.
#[cfg(not(any(target_vendor = "apple", windows)))]
#[must_use]
pub fn read_process_birth(_pid: i32) -> Option<ProcessBirth> {
    None
}

/// A reading of the kernel's boot clock, the monotonic clock this platform records a process's start on.
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
    // SAFETY: a NUL-terminated name, one writable `int` and its length, all live for the call.
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

/// See the Apple twin: nothing translates a process here.
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

/// See the Linux twin: no test depends on a translator here.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub const fn translator() -> Option<String> {
    None
}

/// The Apple boot clock for a process whose [`Translation`] is `translation`, read by `ticks` (`mach_absolute_time`).
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

/// The boot clock now ([`BootClock`]), or `None` where this process cannot read it in the kernel's unit.
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

/// See the Apple twin: no boot clock wired here.
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

/// Milliseconds on a clock that COUNTS THROUGH SLEEP and never steps with the wall clock, from an origin fixed for this boot.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn continuous_ms() -> Option<u64> {
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
    u64::try_from(ns / 1_000_000).ok()
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn continuous_ms() -> Option<u64> {
    boot_clock_now().map(|BootClock(ns)| ns / 1_000_000)
}

/// HOW LONG THIS MACHINE HAS SLEPT SINCE IT BOOTED.
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

/// One `clock_gettime` reading of `clock`, in nanoseconds.
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

/// See the Apple twin: no sleep is counted here.
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

/// See the Apple twin: not wired here.
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

/// THE MACHINE'S LAST SLEEP on the wall clock, as the kernel recorded it: when it went to sleep and when it woke, unix milliseconds.
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn last_sleep_ms() -> Option<(i64, i64)> {
    let slept = sysctl_timeval_ms(c"kern.sleeptime")?;
    let woke = sysctl_timeval_ms(c"kern.waketime")?;
    (slept > 0 && slept < woke).then_some((slept, woke))
}

/// One `struct timeval` sysctl by name, unix milliseconds; `None` if the
/// kernel refuses it or answers with another width.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn sysctl_timeval_ms(name: &core::ffi::CStr) -> Option<i64> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = core::mem::size_of::<libc::timeval>();
    // SAFETY: `name` is NUL-terminated and outlives the call.
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

/// See the Apple twin: no sleep is recorded here.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn last_sleep_ms() -> Option<(i64, i64)> {
    None
}

/// When a process started, as the kernel recorded it ([`read_process_start`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStart {
    /// On the boot clock in the kernel's unit, and never later than the real start: exact on Apple.
    pub boot: BootClock,
    /// The wall-clock record where the kernel keeps one ([`ProcessBirth`], Apple).
    pub wall: Option<ProcessBirth>,
}

/// Read the kernel's start record for `pid` on the boot clock, or `None` for the same cases as [`read_process_birth`] (no such process, a zombie, another uid, a record naming another pid, `pid <= 1`).
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

/// `ri_proc_start_abstime` of `pid` (`struct rusage_info_v0`, sys/resource.h:
/// 96 bytes, the field at offset 80).
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn start_abstime(pid: i32) -> Option<BootClock> {
    const RUSAGE_INFO_V0: i32 = 0;
    const START_WORD: usize = 80 / 8;
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut core::ffi::c_void) -> i32;
    }
    let mut info = [0u64; 12];
    // SAFETY: `info` is 96 writable, aligned bytes — the size of the structure this flavor fills — and outlives the call.
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
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let again = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (euid, per_second) = linux_reader()?;
    let boot = procfs::start(&stat, &status, &again, pid, euid, per_second)?;
    Some(ProcessStart { boot, wall: None })
}

/// This process's effective uid and the kernel's clock ticks per second
/// (`USER_HZ`, the unit of `starttime`), for [`procfs::start`].
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

/// See the Apple twin: no start record wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_process_start(_pid: i32) -> Option<ProcessStart> {
    None
}

/// Linux's record of a process's start, judged from the text of its `/proc` files.
pub mod procfs {
    use super::BootClock;

    /// `(state, starttime)` from a `/proc/<pid>/stat` line (fields 3 and 22, the latter in clock ticks after boot), or `None` when the line does not parse or names another pid.
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

    /// The parent pid in a `/proc/<pid>/status` text: its `PPid:` line.
    #[must_use]
    pub fn parent(status: &str) -> Option<u32> {
        status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))?
            .trim()
            .parse()
            .ok()
    }

    /// The executable a `/proc/<pid>/exe` link names, or `None` when the kernel marks it ` (deleted)`.
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

    /// The start of `pid` from its `stat`, its `status` and `stat` read again after `status`, for a reader whose effective uid is `euid`.
    #[must_use]
    pub fn start(
        stat: &str,
        status: &str,
        again: &str,
        pid: i32,
        euid: u32,
        per_second: u64,
    ) -> Option<BootClock> {
        let (state, ticks) = stat_start(stat, pid)?;
        if stat_start(again, pid)?.1 != ticks || matches!(state, 'Z' | 'X' | 'x') {
            return None;
        }
        if effective_uid(status)? != euid {
            return None;
        }
        tick_start(ticks, per_second)
    }
}

/// Read the argument words of one live process of this uid, between two matching [`ProcessStart`] records.
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

/// [`read_cmdline`], and whether the word it dropped named the translator.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn cmdline_as_run(pid: u32) -> Option<(Vec<u8>, bool)> {
    use std::io::Read as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    let dir = std::fs::File::open(format!("/proc/{pid}")).ok()?;
    // SAFETY: `dir` is open for the call and the name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c"cmdline".as_ptr(),
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
    cmdline_from_read(bytes, PROCESS_ARGV_MAX, || shared_interpreter(pid))
}

/// Admit one whole bounded read before changing its words.
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

/// The executable the kernel holds for this process when a translator runs it: the
/// translator, which `binfmt_misc` names as every translated process's executable
/// (`docs/measured/linux-rosetta-2026-10-17.md`).
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

/// [`own_interpreter`], when the kernel holds it as `pid`'s executable too: the translator
/// runs `pid`.
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

/// The argument words of one `KERN_PROCARGS2` answer ([`crate::procargs::parse`]), all of
/// them or none: refused when cut, when argc is 0 or past [`PROCESS_ARGV_WORDS`], when the
/// exec path is empty, or when the buffer is past [`PROCESS_ARGV_MAX`].
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

/// End this process by `SIGKILL`, here: no destructor, exit handler or buffered flush runs.
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

/// End this process here, running nothing (see the Unix twin); Windows has no `SIGKILL`.
#[cfg(windows)]
pub fn kill_self_now() -> ! {
    #[expect(
        clippy::disallowed_methods,
        reason = "Windows has no SIGKILL: abort ends the process at once, and no macOS crash report exists here"
    )]
    std::process::abort()
}

/// This machine's host name as configured (`m7.local`): the ONE answer every reader that names the machine gives.
#[must_use]
pub fn host_name() -> Option<String> {
    host_name_over(std::env::var_os("HOSTNAME"))
}

/// [`host_name`] with `$HOSTNAME` passed in: no test may set it.
fn host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    env_host_name(env).or_else(os_host_name)
}

/// The OS's own name for this machine now, NEVER `$HOSTNAME`.
#[must_use]
pub fn kernel_host_name() -> Option<String> {
    os_host_name()
}

/// A `$HOSTNAME` a person set, trimmed; `None` for none or a blank one.
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

/// [`lan_host_name`] with `$HOSTNAME` passed in: no test may set it.
fn lan_host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    env_host_name(env)
        .or_else(configured_host_name)
        .or_else(|| bonjour_host_name().map(|name| format!("{name}.local")))
        .or_else(os_host_name)
}

/// The CoreFoundation and SystemConfiguration calls the macOS names take.
#[cfg(target_os = "macos")]
mod sc {
    use core::ffi::{c_char, c_ulong, c_void};

    pub(super) type CFTypeRef = *const c_void;

    /// `kCFStringEncodingUTF8` (CFString.h).
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

    /// A host name `string` holds, trimmed; `None` for a blank one or one that is no UTF-8.
    ///
    /// # Safety
    /// `string` is a live CFString.
    #[cfg_attr(trust_verify, trust::skip)]
    pub(super) unsafe fn text(string: CFTypeRef) -> Option<String> {
        let mut buf = [0u8; 256];
        // SAFETY: `string` is a live CFString (the caller's promise).
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

/// The HostName an administrator set for this Mac (`scutil --get
/// HostName`): `HostName` in the configuration daemon's `Setup:/System`,
/// which `gethostname(2)` then gives too.
#[cfg(target_os = "macos")]
fn configured_host_name() -> Option<String> {
    // SAFETY: a CFString constant SystemConfiguration exports.
    setup_system(unsafe { sc::kSCPropSystemHostName })
}

/// The string `key` names in the configuration daemon's `Setup:/System`
/// (the machine's names): `None` when it holds none there, or the daemon
/// does not answer.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn setup_system(key_name: sc::CFTypeRef) -> Option<String> {
    // SAFETY: the default allocator; the result is a new CFString this frame owns, or NULL.
    let key = unsafe { sc::SCDynamicStoreKeyCreateComputerName(std::ptr::null()) };
    if key.is_null() {
        return None;
    }
    // SAFETY: a NULL store asks with a temporary session; `key` is live.
    let setup = unsafe { sc::SCDynamicStoreCopyValue(std::ptr::null(), key) };
    // SAFETY: this frame owns exactly the one reference the create returned.
    unsafe { sc::CFRelease(key) };
    if setup.is_null() {
        return None;
    }
    // SAFETY: `setup` is live until it is released below.
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

/// No configured name apart from the kernel's off macOS: [`os_host_name`]
/// answers.
#[cfg(not(target_os = "macos"))]
fn configured_host_name() -> Option<String> {
    None
}

/// macOS's Bonjour name, `LocalHostName` without its `.local`
/// (`SCDynamicStoreCopyLocalHostName`): `None` when none is set or the
/// configuration daemon does not answer (a sandbox that refuses it).
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn bonjour_host_name() -> Option<String> {
    // SAFETY: a NULL store asks with a temporary session; the result is a new CFString reference this frame owns, or NULL.
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

/// No Bonjour name off macOS: [`lan_host_name`] is [`host_name`]'s answer.
#[cfg(not(target_os = "macos"))]
fn bonjour_host_name() -> Option<String> {
    None
}

/// `gethostname(2)`: the kernel's node name, NUL-terminated in a buffer that
/// holds any host name (POSIX caps it at `HOST_NAME_MAX`, 255 on Linux, 255 on
/// Darwin's `MAXHOSTNAMELEN`).
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn os_host_name() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut core::ffi::c_char, len: usize) -> i32;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is `buf.len()` writable bytes that outlive the call.
    let rc = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(buf.get(..len)?).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Windows: the DNS host name, as `hostname.exe` prints it
/// (`GetComputerNameExW(ComputerNameDnsHostname)`), else `%COMPUTERNAME%` (the
/// NetBIOS name: upper-cased, at most 15 characters, so a longer name would not
/// match).
#[cfg(windows)]
fn os_host_name() -> Option<String> {
    /// `COMPUTER_NAME_FORMAT::ComputerNameDnsHostname` (sysinfoapi.h).
    const COMPUTER_NAME_DNS_HOSTNAME: u32 = 1;
    computer_name(COMPUTER_NAME_DNS_HOSTNAME)
        .or_else(|| env_host_name(std::env::var_os("COMPUTERNAME")))
}

/// Windows: this machine's NetBIOS name, read from the OS (`GetComputerNameExW(ComputerNameNetBIOS)`: the name `[Environment]::MachineName` answers, so the one aterm's PowerShell integration writes into its OSC 7 URI).
#[cfg(windows)]
#[must_use]
pub fn netbios_host_name() -> Option<String> {
    netbios_host_name_over(std::env::var_os("COMPUTERNAME"))
}

/// [`netbios_host_name`] with `%COMPUTERNAME%` passed in: no test may set it.
#[cfg(windows)]
fn netbios_host_name_over(env: Option<std::ffi::OsString>) -> Option<String> {
    /// `COMPUTER_NAME_FORMAT::ComputerNameNetBIOS` (sysinfoapi.h).
    const COMPUTER_NAME_NETBIOS: u32 = 0;
    computer_name(COMPUTER_NAME_NETBIOS).or_else(|| env_host_name(env))
}

/// Windows: the machine's name in the `COMPUTER_NAME_FORMAT` `format`
/// (`GetComputerNameExW`), trimmed; `None` when the call fails or the name is
/// blank.
#[cfg(windows)]
fn computer_name(format: u32) -> Option<String> {
    /// A DNS host label is at most 63 characters and a NetBIOS name 15; this
    /// holds any name.
    const CAPACITY: usize = 256;
    let mut buf = [0u16; CAPACITY];
    let mut len = CAPACITY as u32;
    // SAFETY: `buf` is `len` writable UTF-16 units and outlives the call.
    let ok = unsafe { crate::win::ffi::GetComputerNameExW(format, buf.as_mut_ptr(), &mut len) };
    (ok != 0)
        .then(|| buf.get(..len as usize).map(String::from_utf16_lossy))
        .flatten()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// No OS answer wired here (the wasm32 hosts): `$HOSTNAME` alone.
#[cfg(not(any(unix, windows)))]
fn os_host_name() -> Option<String> {
    None
}

#[cfg(all(test, windows))]
mod netbios_name_tests {
    /// THE NETBIOS NAME IS THE OS'S, NOT THE ENVIRONMENT'S: with no
    /// `%COMPUTERNAME%` at all (a process started with an environment of its
    /// own) it is still read, at most fifteen characters, and it is the name
    /// Windows set `%COMPUTERNAME%` to where this process's environment has
    /// it.
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
    /// A `$HOSTNAME` a person set names the machine, trimmed; a blank one
    /// names nothing and the OS answers.
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

    /// On macOS the LAN's name for this machine is the HostName an
    /// administrator set, the one `scutil --get HostName` prints; with none,
    /// its Bonjour name, the one `scutil --get LocalHostName` prints, with
    /// `.local`: never the name DHCP or reverse DNS gave the kernel.
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

    /// The kernel's answer is the one `uname -n` prints: the same node name,
    /// read without a spawn.
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

/// `sigset_t`: a 32-bit mask on Darwin, 1024 bits on Linux.
#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(all(unix, not(target_vendor = "apple")))]
type SigSet = [u64; 16];

/// SIGTERM, SIGINT and SIGHUP, the signals a supervisor or a terminal stops a process with
/// (launchd at logout, `launchctl bootout`, `systemctl stop`, ^C, a terminal gone away).
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn stop_set() -> SigSet {
    unsafe extern "C" {
        fn sigemptyset(set: *mut SigSet) -> i32;
        fn sigaddset(set: *mut SigSet, sig: i32) -> i32;
    }
    let mut set = SigSet::default();
    // SAFETY: `sigemptyset` initialises the set it is handed; `sigaddset` then adds valid signal numbers to that initialised set.
    unsafe {
        sigemptyset(&mut set);
        for sig in [15, 2, 1] {
            sigaddset(&mut set, sig);
        }
    }
    set
}

/// TAKE THE STOP SIGNALS (SIGTERM, SIGINT, SIGHUP) so a process can stop cleanly instead of dying of their default action.
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

    /// A Windows `FILETIME` counts 100 ns ticks from 1601: the Unix epoch is its
    /// 116444736000000000th, a part of a microsecond is dropped, and a tick
    /// before the epoch is no birth.
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

    /// Real `/proc` text (`USER_HZ` 100): a command name holding a space and both parentheses,
    /// running and as a zombie, and a `cat` read at uptime 518318.71 s.
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

    /// Field 22 counted from the command name's last `)`, the effective uid as the second
    /// `Uid:` id, a tick as its first instant; another pid's line, a zombie, another euid, two
    /// disagreeing `stat` reads, a short line and a zero tick rate answer `None`.
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
        let tricky_status = STATUS_501.replace("Pid:\t13", "Pid:\t7");
        assert_eq!(
            procfs::start(TRICKY, &tricky_status, TRICKY, 7, 502, 100),
            at(51_832_760)
        );
        for (case, stat, again, euid, per_second) in [
            ("a zombie", ZOMBIE, ZOMBIE, 502, 100),
            ("dead", &*TRICKY.replace(") S ", ") X "), TRICKY, 502, 100),
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

    /// A child spawned after a boot-clock reading starts later, and no later than a reading
    /// taken once it runs; this process started earlier; an ended process has no record.
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

    /// The translated half of [`a_child_started_after_a_boot_clock_reading_reads_later`]: the
    /// wall record orders a child spawned after a wall-clock reading as later.
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

    /// With the translation injected, `Translated` answers `None` without reading the clock,
    /// and `Native` and `Unknown` answer the reading as is.
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

    /// `kill_self_now` ends the process by SIGKILL at the call, which leaves no
    /// macOS crash report.
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

#[cfg(test)]
mod end_proven_tests {
    use super::{EndControl, EndRefusal, Ended, Identity, ProcessBirth, end_proven};
    use std::time::Duration;

    fn ident(pid: i32, idversion: i32) -> Identity {
        Identity {
            pid,
            birth: ProcessBirth {
                seconds: 1_792_000_000,
                microseconds: 42,
            },
            idversion,
        }
    }

    /// A control whose identity reads follow a script (one per `identity` call) and whose
    /// signals and naps are recorded. No process, no clock: the sequence is all that is under
    /// test.
    struct Fake {
        reads: std::cell::RefCell<std::vec::IntoIter<Option<Identity>>>,
        signals: Vec<(i32, i32)>,
        naps: usize,
        /// A signal the kernel refuses (`ESRCH`: the process had just exited).
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
        fn signal(&mut self, pid: i32, sig: i32) -> std::io::Result<()> {
            if self.refuses == Some(sig) {
                return Err(std::io::Error::from_raw_os_error(3));
            }
            self.signals.push((pid, sig));
            Ok(())
        }
        fn nap(&mut self, _pause: Duration) {
            self.naps += 1;
        }
    }

    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;

    /// The token's program exits within the grace: one SIGTERM, then gone, no SIGKILL.
    #[test]
    fn a_process_that_exits_within_the_grace_takes_one_sigterm() {
        let t = ident(4242, 7);
        // judge -> T, T; poll -> None (gone).
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

    /// A process that outlives the grace is SIGKILLed by pid; the token still reads at the
    /// grace's end, and gone after. A zero grace reaches the kill with no wait.
    #[test]
    fn a_process_that_outlives_the_grace_is_sigkilled() {
        let t = ident(4242, 7);
        // judge -> T, T; poll -> T (not gone); post-grace judge -> T, T; post-kill poll -> None.
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

    /// NEGATIVE CONTROL (the `EndByProof` law): the identity moved between the token and the
    /// re-read, so NO signal goes out — the first judgment reads another pid version and refuses
    /// `stale`. Break `end_proven`'s `names_token` (accept any `Some`) and the refusal assertion
    /// fails: the run returns `Ok`, its SIGTERM sent to a pid that is now another program.
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

    /// The identity moves after the SIGTERM but before the SIGKILL (it exec'd another
    /// program): the post-grace re-read refuses, so no SIGKILL reaches the program the token no
    /// longer names. One SIGTERM, and the process is still there, so not gone.
    #[test]
    fn a_moved_identity_before_the_kill_sends_no_second_signal() {
        let t = ident(4242, 7);
        // judge -> T, T; poll -> T; post-grace judge -> moved (idversion 8).
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

    /// The tier's conditions hold at the SIGTERM and are lost by the grace's end (the process
    /// left its group, or reached the program check): the judgment before the SIGKILL runs the
    /// tier's check too, so no SIGKILL goes out and the process is left, neither killed nor
    /// gone. NEGATIVE CONTROL: drop the `still_proven` call from the judgment and the signals
    /// read `[SIGTERM, SIGKILL]`.
    #[test]
    fn a_lost_witness_before_the_kill_sends_no_second_signal() {
        let t = ident(4242, 7);
        // judge -> T, T; poll -> T (not gone); post-grace judge -> T, but the tier is lost.
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

    /// The tier's conditions no longer hold on the fresh read: refused `not-proven`, no signal.
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

    /// The tier's reads are bracketed by two identity reads: a process that exec'd another
    /// program WHILE `still_proven` read it (so the tier judged the new program) is refused
    /// `stale` by the read after the judgment, and no signal goes out. NEGATIVE CONTROL: drop
    /// the identity read after `still_proven`, and the SIGTERM reaches the new program.
    #[test]
    fn an_exec_during_the_judgment_sends_no_signal() {
        let t = ident(4242, 7);
        // judge: identity -> T, the tier holds (on what is now another program), identity ->
        // the exec's version.
        let mut c = Fake::new(vec![Some(t), Some(ident(4242, 8))]);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| true),
            Err(EndRefusal::Stale)
        );
        assert!(c.signals.is_empty(), "no signal after an exec mid-judgment");
    }

    /// A process that execs after its SIGTERM is still there, running another program: it is
    /// neither SIGKILLed nor gone. Gone is only a pid that reads nothing, or another birth (the
    /// number taken by a new process). NEGATIVE CONTROL: call any moved read gone, and `gone`
    /// reads true for a process still alive in its group.
    #[test]
    fn a_process_that_execs_after_its_sigterm_is_not_gone() {
        let t = ident(4242, 7);
        // judge -> T, T; poll -> the same pid and birth, another version (it exec'd).
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
        // A pid read under another birth is a new process: the token's one has exited.
        let mut reborn = ident(4242, 8);
        reborn.birth.microseconds += 1;
        let mut c = Fake::new(vec![Some(t), Some(t), Some(reborn)]);
        let out = end_proven(t, Duration::from_secs(60), &mut c, |_| true).expect("termed");
        assert!(out.gone, "{out:?}");
    }

    /// A SIGKILLed process still in the table at the last read is not gone (XNU tears a
    /// process down after the signal, and a wedged one may stay): `killed` without `gone`.
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

    /// A signal the kernel refuses because the process had just exited (`ESRCH`) is judged by
    /// a read, never by the error: the process is gone, and a SIGTERM already sent stays
    /// counted. NEGATIVE CONTROL: map every failed signal to `SignalFailed`, and both runs
    /// read as refusals for a process that is gone.
    #[test]
    fn a_signal_refused_for_a_process_just_gone_reads_gone() {
        let t = ident(4242, 7);
        // judge -> T, T; the SIGTERM is refused; the read after it -> nothing.
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
        // judge -> T, T; poll -> T; judge -> T, T; the SIGKILL is refused; then nothing.
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
        // Refused while it still reads: a refusal, nothing sent.
        let mut c = Fake::new(vec![Some(t), Some(t), Some(t)]);
        c.refuses = Some(SIGTERM);
        assert_eq!(
            end_proven(t, Duration::from_secs(60), &mut c, |_| true),
            Err(EndRefusal::SignalFailed)
        );
    }

    /// The token's wire form round-trips its three parts.
    #[test]
    fn a_token_is_pid_birth_and_version() {
        assert_eq!(
            ident(63452, 331207).token(),
            "63452@1792000000000042:331207"
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    /// A process that exits with code 259 (== STILL_ACTIVE) still reads dead.
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

    /// The birth record (`GetProcessTimes`): a child started after a wall-clock
    /// reading was born after it and no later than a reading taken once it runs,
    /// and this process before it.
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

    /// The raw creation `FILETIME` (`read_creation_filetime`) is the untruncated
    /// integer Claude Code records as `procStart`: it reads for this live
    /// process, is past the Unix epoch, and rounds to the microsecond-rounded
    /// [`super::read_process_birth`] — the exact decimal the harness compares
    /// against a session record, not a value reconstructed from the rounded
    /// birth.
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

    /// This process: its parent is the one `std` reports, and its executable
    /// is the test binary running now.
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

    /// Linux's `/proc` text: the `PPid:` line, and an exe link the kernel marks
    /// ` (deleted)` names no live binary.
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

    /// A live process whose binary was deleted under it reads no path — the macOS
    /// contract — while its start record still answers.
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

    /// A child's program and argv read as it was given them, and reading them leaves no
    /// descriptor behind ([`read_cmdline`], [`read_exec_path`]).
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
    use super::{asleep_since_boot, continuous_ms, last_sleep_ms};
    use std::time::Duration;

    /// The machine's sleep since boot reads where it is wired, never runs
    /// back between two reads (the reads' own gap aside, a nanosecond's
    /// order), and is no more than the clock that counts it has run since
    /// boot.
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

    /// The last sleep, where the kernel keeps one, went to sleep before it
    /// woke, and woke no later than now.
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
