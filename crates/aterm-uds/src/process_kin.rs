// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process lineage, group membership, and exec-environment marker queries.

#[cfg(target_vendor = "apple")]
use super::ProcessBirth;

/// `proc_pidinfo(PROC_PIDT_BSDINFOWITHUNIQID)` record (`<sys/proc_info.h>`).
#[cfg(target_vendor = "apple")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BsdInfoWithUniqId {
    /// The BSD info (`PROC_PIDTBSDINFO`'s record).
    pub pbsd: libc::proc_bsdinfo,
    /// The executable's UUID.
    pub p_uuid: [u8; 16],
    /// The process's unique id.
    pub p_uniqueid: u64,
    /// Its parent's at its birth.
    pub p_puniqueid: u64,
    /// Its pid version.
    pub p_idversion: i32,
    /// Launcher's pid version, preserved across adoption and later execs.
    pub p_orig_ppidversion: i32,
    /// Reserved.
    pub p_reserve2: u64,
    /// Reserved.
    pub p_reserve3: u64,
}

#[cfg(target_vendor = "apple")]
const _: () = assert!(
    std::mem::size_of::<BsdInfoWithUniqId>()
        == std::mem::size_of::<libc::proc_bsdinfo>() + std::mem::size_of::<UniqIdentifierInfo>()
);

/// `<sys/proc_info.h>` `proc_uniqidentifierinfo` (flavor 17, 56 bytes).
#[cfg(target_vendor = "apple")]
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UniqIdentifierInfo {
    /// The executable's UUID.
    pub p_uuid: [u8; 16],
    /// As [`BsdInfoWithUniqId::p_uniqueid`].
    pub p_uniqueid: u64,
    /// As [`BsdInfoWithUniqId::p_puniqueid`].
    pub p_puniqueid: u64,
    /// As [`BsdInfoWithUniqId::p_idversion`].
    pub p_idversion: i32,
    /// As [`BsdInfoWithUniqId::p_orig_ppidversion`].
    pub p_orig_ppidversion: i32,
    /// Reserved.
    pub p_reserve2: u64,
    /// Reserved.
    pub p_reserve3: u64,
}

#[cfg(target_vendor = "apple")]
const _: () = assert!(std::mem::size_of::<UniqIdentifierInfo>() == 56);

/// `pid`'s record with its unique ids (flavor 18), whole or `None` ([`read_lineage`]).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn bsdinfo_uniq(pid: i32) -> Option<BsdInfoWithUniqId> {
    const PROC_PIDT_BSDINFOWITHUNIQID: i32 = 18;
    let mut info = std::mem::MaybeUninit::<BsdInfoWithUniqId>::zeroed();
    let size = std::mem::size_of::<BsdInfoWithUniqId>() as i32;
    // SAFETY: `info` is `size` writable bytes matching the structure this flavor fills.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDT_BSDINFOWITHUNIQID,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: Zero-initialised plain data, and a full record was written.
    (n == size).then(|| unsafe { info.assume_init() })
}

/// `pid`'s identifiers (flavor 17, `PROC_PIDUNIQIDENTIFIERINFO`).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn uniq_identifier(pid: i32) -> Option<UniqIdentifierInfo> {
    const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
    let mut info = std::mem::MaybeUninit::<UniqIdentifierInfo>::zeroed();
    let size = std::mem::size_of::<UniqIdentifierInfo>() as i32;
    // SAFETY: `info` is `size` writable bytes; the kernel writes at most that many and returns how
    // many it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDUNIQIDENTIFIERINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: Zero-initialised plain data, and a full record was written.
    (n == size).then(|| unsafe { info.assume_init() })
}

/// launchd's pid version (flavor 17 of pid 1, which any process may read).
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn launchd_idversion() -> Option<i32> {
    uniq_identifier(1).map(|info| info.p_idversion)
}

/// See the Apple twin: no pid versions are read here.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn launchd_idversion() -> Option<i32> {
    None
}

/// Whether pid version `launcher` was handed out before `shell`'s.
#[must_use]
pub const fn launched_before(launcher: i32, shell: i32) -> bool {
    shell.wrapping_sub(launcher) > 0
}

/// Lineage of a live same-uid process (flavor 18; `/proc/<pid>/stat` and `status` on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lineage {
    /// Who it is: what the end protocol re-reads before each signal ([`super::end_proven`]).
    pub identity: super::Identity,
    /// Its unique id, which a later fork never reads below: `p_uniqueid` on Apple (strictly
    /// larger), the start in clock ticks on Linux (equal to `identity.birth`; two forks may
    /// share a tick).
    pub uniqueid: u64,
    /// Its parent now: `1` once launchd or init adopted it.
    pub parent: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its launcher's pid version (`p_orig_ppidversion`, kept through adoption), `None` where
    /// the kernel keeps no versions (Linux).
    pub launcher_version: Option<i32>,
    /// Its session's controlling terminal (`e_tdev`; Linux `tty_nr`).
    pub tty: Option<u32>,
}

impl Lineage {
    /// Whether `launcher` started it, by the launcher's pid version: `None` where either
    /// version is not kept (Linux), so two missing versions never prove a launch.
    #[must_use]
    pub fn launched_by(&self, launcher: &super::Identity) -> Option<bool> {
        Some(self.launcher_version? == launcher.idversion?)
    }

    /// Orphaned. With a launcher version (Apple): launchd adopted it, and launchd did not
    /// start it (`launchd`, [`launchd_idversion`]); `parent_uniqueid` is never called. Without
    /// one (Linux, which keeps no original parent): init adopted it, or its parent is older
    /// than `floor`, the unique id of the run it is judged for. No process a run started is
    /// older than the run, so a parent born before it adopted the process (a subreaper, as
    /// `systemd --user` is on desktop Linux), and the run's own launcher (born at `floor`) never
    /// counts. `parent_uniqueid` reads the parent's unique id; `None` (unreadable, another uid)
    /// is not orphaned.
    #[must_use]
    pub fn orphaned(
        &self,
        launchd: Option<i32>,
        floor: u64,
        parent_uniqueid: impl FnOnce() -> Option<u64>,
    ) -> bool {
        match self.launcher_version {
            Some(version) => self.parent == 1 && launchd.is_some_and(|v| v != version),
            None => {
                self.parent == 1
                    || (self.parent > 1 && parent_uniqueid().is_some_and(|parent| parent < floor))
            }
        }
    }

    /// Started by launchd itself: launchd is its parent, and its launcher's pid version is
    /// launchd's ([`launchd_idversion`]). Such a process is never an orphan.
    #[must_use]
    pub fn launchd_started(&self, launchd: Option<i32>) -> bool {
        self.parent == 1 && launchd.is_some_and(|v| self.launcher_version == Some(v))
    }
}

/// Reads `pid`'s [`Lineage`].
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn read_lineage(pid: i32) -> Option<Lineage> {
    const SZOMB: u32 = 5;
    const NODEV: u32 = u32::MAX;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    if pid <= 1 {
        return None;
    }
    let info = bsdinfo_uniq(pid)?;
    // SAFETY: A side-effect-free getter.
    let ours = unsafe { geteuid() };
    if info.pbsd.pbi_pid != pid.unsigned_abs()
        || info.pbsd.pbi_uid != ours
        || info.pbsd.pbi_status == SZOMB
    {
        return None;
    }
    Some(Lineage {
        identity: super::Identity {
            pid,
            birth: super::Birth::from(ProcessBirth {
                seconds: info.pbsd.pbi_start_tvsec,
                microseconds: info.pbsd.pbi_start_tvusec,
            }),
            idversion: Some(info.p_idversion),
        },
        uniqueid: info.p_uniqueid,
        parent: info.pbsd.pbi_ppid,
        pgid: info.pbsd.pbi_pgid,
        launcher_version: Some(info.p_orig_ppidversion),
        tty: (info.pbsd.e_tdev != NODEV).then_some(info.pbsd.e_tdev),
    })
}

/// See the Apple twin: `/proc/<pid>/stat`, `status` and `stat` again, judged by
/// [`super::procfs::lineage`]. The identity has no version and the token reads
/// `<pid>@<start_ticks>`.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn read_lineage(pid: i32) -> Option<Lineage> {
    if pid <= 1 {
        return None;
    }
    let [stat, status, again] = super::proc_stat_status_stat(pid)?;
    let (euid, _) = super::linux_reader()?;
    super::procfs::lineage(&stat, &status, &again, pid, euid)
}

/// See the Apple twin: no lineage reader is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn read_lineage(_pid: i32) -> Option<Lineage> {
    None
}

/// Reads `pid`'s [`super::Identity`] via [`read_lineage`].
#[must_use]
pub fn read_identity(pid: i32) -> Option<super::Identity> {
    read_lineage(pid).map(|l| l.identity)
}

/// Maximum members listed for one process group ([`group_members`]).
pub const MAX_GROUP_PIDS: usize = 256;

/// Process group members returned by [`group_members`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupMembers {
    /// The members. Apple lists a zombie while it is unreaped (an exited stage leader is);
    /// Linux skips zombies ([`super::procfs::group_of`]).
    pub pids: Vec<i32>,
    /// Whether the listing is whole.
    pub complete: bool,
}

/// Lists up to [`MAX_GROUP_PIDS`] members of process group `pgid` (`proc_listpids(PROC_PGRP_ONLY)`).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn group_members(pgid: u32) -> Option<GroupMembers> {
    const PROC_PGRP_ONLY: u32 = 2;
    if pgid <= 1 {
        return None;
    }
    let mut pids = [0i32; MAX_GROUP_PIDS];
    let bytes = i32::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is `bytes` writable bytes of pid slots; libproc (`holders`' declaration)
    // writes at most that many and answers how many bytes it wrote, or a negative number.
    let got = unsafe {
        crate::holders::ffi::proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr(), bytes)
    };
    let got = usize::try_from(got).ok()?;
    let n = (got / std::mem::size_of::<i32>()).min(MAX_GROUP_PIDS);
    Some(GroupMembers {
        pids: pids[..n].iter().copied().filter(|p| *p > 0).collect(),
        complete: n < MAX_GROUP_PIDS,
    })
}

/// See the Apple twin: every `/proc/<pid>/stat` ([`super::stat_row`]), judged by
/// [`super::procfs::group_of`]. A process gone by its read is skipped; any other failed read
/// leaves the listing incomplete, and so does a `/proc` mounted with `hidepid`
/// ([`group_listing_can_be_whole`]), which hides from its listing, or refuses the files of,
/// another user's processes and this uid's non-dumpable ones.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn group_members(pgid: u32) -> Option<GroupMembers> {
    use super::procfs::StatRead;
    if pgid <= 1 {
        return None;
    }
    let rows = std::fs::read_dir("/proc").ok()?.filter_map(|entry| {
        let Ok(entry) = entry else {
            return Some((0, StatRead::Unreadable));
        };
        let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
        Some((pid, super::stat_row(&entry.path().join("stat"))))
    });
    Some(super::procfs::group_of(
        rows,
        pgid,
        group_listing_can_be_whole(),
    ))
}

/// Whether a [`group_members`] listing here can be complete: on Linux, whether `/proc` lists
/// every process to every reader ([`super::procfs::shows_every_process`] over
/// `/proc/self/mountinfo`); on Apple, whose kernel lists a group itself, it can; elsewhere
/// nothing lists one.
#[must_use]
pub fn group_listing_can_be_whole() -> bool {
    #[cfg(target_vendor = "apple")]
    {
        true
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        super::proc_shows_every_process()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    {
        false
    }
}

/// See the Apple twin: no group lister is wired here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn group_members(_pgid: u32) -> Option<GroupMembers> {
    None
}

/// Environment marker variables read by [`read_process_markers`].
pub const MARKER_VARS: [&str; 4] = [
    "ATERM_PARENT_SESSION_ID",
    "ATERM_CHILD",
    VERIFY_RUN_VAR,
    TRUST_CHECK_VAR,
];

/// `ATERM_VERIFY_RUN`.
pub const VERIFY_RUN_VAR: &str = "ATERM_VERIFY_RUN";

/// `ATERM_TRUST_CHECK`: the mark every process of one trust-lane check carries.
pub const TRUST_CHECK_VAR: &str = "ATERM_TRUST_CHECK";

/// Exec-environment marker variables ([`MARKER_VARS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Markers {
    /// The kernel handed back no environment at all.
    pub withheld: bool,
    /// `ATERM_PARENT_SESSION_ID`.
    pub parent_session: Option<String>,
    /// `ATERM_CHILD`.
    pub child: Option<String>,
    /// `ATERM_VERIFY_RUN`.
    pub verify_run: Option<String>,
    /// `ATERM_TRUST_CHECK`.
    pub trust_check: Option<String>,
}

/// `pid`'s [`Markers`], read between two identity reads that must agree ([`bracketed`]): from
/// `KERN_PROCARGS2` on macOS, `/proc/<pid>/environ` on Linux. A pid reused between the reads
/// shows on both. An exec shows only where the identity has a pid version (Apple): on Linux it
/// keeps the pid and the start, so the words may be either image's, each one the process's own.
#[must_use]
pub fn read_process_markers(pid: i32) -> Option<Markers> {
    #[cfg(target_os = "macos")]
    {
        if pid <= 1 {
            return None;
        }
        bracketed(pid, read_identity, || {
            markers_from_procargs2(&crate::procargs::read(u32::try_from(pid).ok()?)?)
        })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if pid <= 1 {
            return None;
        }
        bracketed(pid, read_identity, || {
            markers_from_environ(&super::read_proc_bounded(
                u32::try_from(pid).ok()?,
                c"environ",
            )?)
        })
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
fn bracketed<I: PartialEq, T>(
    pid: i32,
    mut identity: impl FnMut(i32) -> Option<I>,
    read: impl FnOnce() -> Option<T>,
) -> Option<T> {
    let before = identity(pid)?;
    let got = read()?;
    (identity(pid)? == before).then_some(got)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn markers_from_procargs2(bytes: &[u8]) -> Option<Markers> {
    if bytes.len() > super::PROCESS_ARGV_MAX {
        return None;
    }
    let p = crate::procargs::parse(bytes)?;
    if !p.whole() || p.exec_path.is_empty() {
        return None;
    }
    Some(markers_from_words(p.env()))
}

/// Extract [`Markers`] from a `/proc/<pid>/environ` read: NUL-separated words, the last one
/// NUL-terminated. An empty file (a zombie's, or one whose environment the kernel no longer
/// maps) is withheld, never proof either way; a read past the bound is unknown.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn markers_from_environ(bytes: &[u8]) -> Option<Markers> {
    if bytes.len() > super::PROCESS_ARGV_MAX {
        return None;
    }
    Some(markers_from_words(
        bytes.split(|&b| b == 0).filter(|word| !word.is_empty()),
    ))
}

/// The markers of an environment's `NAME=value` words, as `getenv` reads them: the first
/// word naming each wins, a word with no `=` is skipped, and no word at all is withheld.
#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    all(test, unix)
))]
fn markers_from_words<'a>(words: impl Iterator<Item = &'a [u8]>) -> Markers {
    let mut markers = Markers {
        withheld: true,
        ..Markers::default()
    };
    let mut seen = [false; MARKER_VARS.len()];
    for word in words {
        markers.withheld = false;
        let Some(eq) = word.iter().position(|&b| b == b'=') else {
            continue;
        };
        let Some(slot) = MARKER_VARS.iter().position(|k| k.as_bytes() == &word[..eq]) else {
            continue;
        };
        if std::mem::replace(&mut seen[slot], true) {
            continue;
        }
        let value = std::str::from_utf8(&word[eq + 1..]).ok().map(str::to_owned);
        match slot {
            0 => markers.parent_session = value,
            1 => markers.child = value,
            2 => markers.verify_run = value,
            _ => markers.trust_check = value,
        }
    }
    markers
}

/// A number `pid`'s process holds that no other process given `pid` in this boot holds: its
/// unique id on macOS (`p_uniqueid`, never reused within a boot), its start in clock ticks on
/// Linux (`/proc/<pid>/stat`). Neither reads a clock's zone or a locale. `None` when unread: no
/// such process, a zombie on macOS, or no reader here.
#[must_use]
pub fn process_serial(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    #[cfg(target_vendor = "apple")]
    {
        uniq_identifier(pid).map(|info| info.p_uniqueid)
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        super::procfs::stat_start(&stat, pid).map(|(_, ticks)| ticks)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        None
    }
}

/// This boot's id, which a record kept for one boot names  ([`astream_sys::boot_session`]).
#[cfg(unix)]
#[must_use]
pub fn boot_id() -> Option<String> {
    use std::fmt::Write as _;
    let boot = astream_sys::boot_session()?;
    let mut text = String::with_capacity(2 * boot.len());
    for b in boot {
        let _ = write!(text, "{b:02x}");
    }
    Some(text)
}

/// See the Unix twin: no boot id is wired here.
#[cfg(not(unix))]
#[must_use]
pub fn boot_id() -> Option<String> {
    None
}

/// This process's pid namespace, the one every pid it reads is read in: the inode Linux names
/// in `/proc/self/ns/pid` ([`namespace_inode`]). Namespaces share a boot, so a record of pids
/// is kept per namespace as well as per boot. `None` where there are none, or it does not read.
#[must_use]
pub fn pid_namespace() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        namespace_inode(std::fs::read_link("/proc/self/ns/pid").ok()?.to_str()?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        None
    }
}

/// The inode of a `/proc/<pid>/ns/pid` link's text, `pid:[<inode>]`.
#[cfg(any(target_os = "linux", target_os = "android", all(test, unix)))]
fn namespace_inode(link: &str) -> Option<String> {
    let inode = link.strip_prefix("pid:[")?.strip_suffix(']')?;
    (!inode.is_empty() && inode.bytes().all(|b| b.is_ascii_digit())).then(|| inode.to_owned())
}

/// Whether macOS's program check holds `pid` now: `Some(true)` is the whole held predicate (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.2), one thread that never ran, 20 s or more since the fork, neither stopped nor traced, native, its executable on a local volume, as aterm-sysprobe's one reader judges it ([`aterm_sysprobe::jobs::at_start`]). `None` when unread (gone, another user's, a pid that is no process).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn at_program_check(pid: i32) -> Option<bool> {
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
    aterm_sysprobe::jobs::at_start(pid).map(|at| at == aterm_sysprobe::jobs::AtStart::Held)
}

/// See the Apple twin: Linux has no program check, so a process its identity read names
/// ([`read_identity`]: live, this uid's) is never held there, and one it does not (a gone pid,
/// a zombie, another uid's process, a thread's id, pid 1) is unread, as Apple's `ESRCH` and
/// `EPERM` are.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn at_program_check(pid: i32) -> Option<bool> {
    read_identity(pid).map(|_| false)
}

/// See the Apple twin: nothing is read here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
#[must_use]
pub fn at_program_check(_pid: i32) -> Option<bool> {
    None
}

/// Whether `pid`'s one thread has run, for a reader that would read its arguments or signal it: `Some(false)` for a thread list that is one handle `0` at any age, arch, volume or stop ([`aterm_sysprobe::jobs::StartFacts::lone_zero`]), since a read of such a program's arguments can hang (M29) and whether a signal reaches it is unmeasured (U11); only [`at_program_check`]'s words weigh the rest. `None` when unread.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn thread_ran(pid: i32) -> Option<bool> {
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
    aterm_sysprobe::jobs::read_start(pid, aterm_sysprobe::jobs::now_us()).map(|f| !f.lone_zero)
}

/// See the Apple twin: nothing on Linux holds a program before its first instruction, so every positive pid answers `Some(true)` with nothing read.
#[cfg(target_os = "linux")]
#[must_use]
pub fn thread_ran(pid: i32) -> Option<bool> {
    (pid > 0).then_some(true)
}

/// See the Apple twin: nothing is read here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
#[must_use]
pub fn thread_ran(_pid: i32) -> Option<bool> {
    None
}

/// How many of this user's programs macOS's program check holds now ([`held_programs`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldNow {
    /// The programs judged held: a lower bound, since a row that cannot be read is not counted.
    pub count: usize,
    /// The oldest held program's fork age, ms (an upper bound on how long it has been held); 0 when none is.
    pub oldest_ms: u64,
}

/// How long a program has waited at macOS's program check, in the words every surface that counts the wait shares: `20 s` under a minute, `5 m` under an hour, `9 h 59 m` past one (its minutes two digits: `10 h 05 m`).
#[must_use]
pub fn held_age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} m", s / 60),
        _ => format!("{} h {:02} m", s / 3600, s % 3600 / 60),
    }
}

/// [`held_programs`]' count over `pids`, each read by `read`: a row `read` cannot read is skipped, and only one [`aterm_sysprobe::jobs::judge_start`] calls held is counted.
#[cfg(any(unix, windows))]
#[must_use]
pub fn held_among(
    pids: &[u32],
    read: impl Fn(u32) -> Option<aterm_sysprobe::jobs::StartFacts>,
) -> HeldNow {
    use aterm_sysprobe::jobs::{AtStart, judge_start};
    pids.iter()
        .filter_map(|&pid| read(pid))
        .filter(|f| judge_start(f) == AtStart::Held)
        .fold(HeldNow::default(), |n, f| HeldNow {
            count: n.count + 1,
            oldest_ms: n.oldest_ms.max(f.age_ms.unwrap_or(0)),
        })
}

/// This user's programs macOS's program check holds now: one two-slot thread list per process of this uid, and the start records of a lone `0` only, with one mount table for them all (no argument or environment is read). `None` when the process list cannot be read, and off macOS.
#[cfg(target_os = "macos")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn held_programs() -> Option<HeldNow> {
    let pids = crate::holders::uid_pids().ok()?;
    let now = aterm_sysprobe::jobs::now_us();
    let mounts = std::cell::OnceCell::new();
    Some(held_among(&pids, |pid| {
        aterm_sysprobe::jobs::read_start_in(pid, now, &mounts)
    }))
}

/// See the macOS twin: there is no program check to count here.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn held_programs() -> Option<HeldNow> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn frame(argc: i32, words: &[&[u8]]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/bin/aterm\0\0\0");
        for w in words {
            bytes.extend_from_slice(w);
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn only_the_marker_vars_are_read_and_an_empty_environment_is_withheld() {
        let words: [&[u8]; 7] = [
            b"/bin/aterm",
            b"ATERM_VERIFY_RUN=argv-not-env",
            b"HOME=/h",
            b"ATERM_VERIFY_RUN=71-1792163800331520",
            b"ATERM_PARENT_SESSION_ID=s7",
            b"ATERM_CHILD=1",
            b"ATERM_TRUST_CHECK=0f1e",
        ];
        let read = markers_from_procargs2(&frame(2, &words)).expect("a buffer");
        assert_eq!(
            read,
            Markers {
                withheld: false,
                parent_session: Some("s7".into()),
                child: Some("1".into()),
                verify_run: Some("71-1792163800331520".into()),
                trust_check: Some("0f1e".into()),
            }
        );
        let environ: Vec<u8> = words[1..].join(&0u8);
        assert_eq!(
            markers_from_environ(&environ).expect("within the bound"),
            Markers {
                verify_run: Some("argv-not-env".into()),
                ..read.clone()
            },
            "an environ file reads as the same words"
        );
        let shifted = markers_from_procargs2(&frame(1, &words[..2])).expect("a buffer");
        assert_eq!(shifted.verify_run.as_deref(), Some("argv-not-env"));
        let withheld = markers_from_procargs2(&frame(1, &[b"/bin/sleep", b""])).expect("a buffer");
        assert!(withheld.withheld, "{withheld:?}");
        assert_eq!(withheld.verify_run, None);
        assert_eq!(markers_from_procargs2(&[1, 0]), None, "no argc");
        assert_eq!(
            markers_from_procargs2(&frame(9, &words)),
            None,
            "argc past the words"
        );
    }

    /// A read between two identity reads that disagree is refused: an exit, a pid reused (a
    /// later birth) on every kernel, and an exec where the identity has a pid version. Linux's
    /// identity has none, so an exec there reads as the same process and the read stands; only
    /// a tier's own reads (the executable under the roots) see it. NEGATIVE CONTROL: compare the
    /// two reads' pid versions alone, and on Linux, where both are `None`, the reused pid's read
    /// stands.
    #[test]
    fn markers_read_across_an_exec_are_refused() {
        let id = |birth, idversion| super::super::Identity {
            pid: 71,
            birth: super::super::Birth(birth),
            idversion,
        };
        let script = |reads: Vec<Option<super::super::Identity>>| {
            let mut reads = reads.into_iter();
            bracketed(71, move |_| reads.next().flatten(), || Some("env"))
        };
        let apple = |idversion| Some(id(1_000_002, Some(idversion)));
        assert_eq!(script(vec![apple(7), apple(7)]), Some("env"));
        assert_eq!(script(vec![apple(7), apple(8)]), None, "an exec");
        assert_eq!(script(vec![apple(7), None]), None, "an exit");
        let linux = |ticks| Some(id(ticks, None));
        assert_eq!(
            script(vec![linux(51_832_760), linux(51_832_901)]),
            None,
            "a pid reused between the reads"
        );
        assert_eq!(
            script(vec![linux(51_832_760), linux(51_832_760)]),
            Some("env"),
            "an exec under one pid and start is one identity"
        );
    }

    /// Set on [`a_marked_child_stand_in`] to make it the marked child.
    const MARKED_CHILD_ENV: &str = "ATERM_UDS_MARKED_CHILD";

    /// The line [`a_marked_child_stand_in`] writes once it runs.
    const MARKED_CHILD_RUNS: &str = "the marked child runs";

    /// The one-minute hang detector of the marked child's waits.
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);

    /// The child [`a_spawned_childs_markers_are_read_off_its_exec_environment`] reads: with
    /// [`MARKED_CHILD_ENV`] set it writes [`MARKED_CHILD_RUNS`] and returns once its stdin
    /// closes, which its parent's end closes too; without it, it returns.
    #[test]
    fn a_marked_child_stand_in() {
        if std::env::var_os(MARKED_CHILD_ENV).is_none() {
            return;
        }
        println!("{MARKED_CHILD_RUNS}");
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
    }

    /// A child's four markers are read off the kernel's copy of its exec environment
    /// (`KERN_PROCARGS2` on macOS, `/proc/<pid>/environ` on Linux) between two identity reads,
    /// and nothing once it has exited, unreaped or reaped, or for pid 1. Its serial is its own:
    /// read again it is the same, this process's differs, and once it is reaped none reads. The child is this test
    /// binary, run in place: macOS withholds a platform binary's environment (`/bin/sleep`'s).
    /// NEGATIVE CONTROLS: hand the macOS parser an empty buffer in place of the child's
    /// arguments, and its markers do not read; read Linux's `environ` without the identity
    /// reads around it (`bracketed`), and the exited child's empty `environ` reads as withheld
    /// markers.
    #[test]
    fn a_spawned_childs_markers_are_read_off_its_exec_environment() {
        use super::super::{ProcessPresence, process_presence};
        let me = i32::try_from(std::process::id()).expect("pid");
        let run = format!("{me}-1");
        let mut child = crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "process::kin::tests::a_marked_child_stand_in",
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
            .env(MARKED_CHILD_ENV, "1")
            .env(VERIFY_RUN_VAR, &run)
            .env(TRUST_CHECK_VAR, "5eed")
            .env("ATERM_CHILD", "1")
            .env("ATERM_PARENT_SESSION_ID", "s-markers")
            .stdin(crate::spawnfd::Stdio::piped())
            .stdout(crate::spawnfd::Stdio::piped())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("re-run this test binary as the marked child");
        crate::deadline::prints(&mut child, MARKED_CHILD_RUNS, HANG);
        let pid = i32::try_from(child.id()).expect("pid");
        let read = read_process_markers(pid);
        let (serial, again) = (process_serial(pid), process_serial(pid));
        drop(child.stdin.take());
        let deadline = std::time::Instant::now() + HANG;
        while process_presence(pid) != ProcessPresence::Exited {
            assert!(
                std::time::Instant::now() < deadline,
                "the marked child (pid {pid}) did not exit within its hang detector"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let unreaped = read_process_markers(pid);
        let _ = child.wait();
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(
                (read, serial),
                (None, None),
                "no marker or serial reader here"
            );
            return;
        }
        assert_eq!(
            read,
            Some(Markers {
                withheld: false,
                parent_session: Some("s-markers".into()),
                child: Some("1".into()),
                verify_run: Some(run),
                trust_check: Some("5eed".into()),
            }),
            "the child's markers"
        );
        assert!(serial.is_some() && serial == again, "{serial:?} {again:?}");
        assert_ne!(serial, process_serial(me));
        assert_eq!(process_serial(pid), None, "a reaped child's serial");
        assert_eq!(unreaped, None, "an exited child its parent has not reaped");
        assert_eq!(read_process_markers(pid), None, "a reaped child");
        assert_eq!(read_process_markers(1), None, "launchd's or init's");
    }

    #[test]
    fn a_marker_named_twice_reads_as_getenv_reads_it() {
        let words: [&[u8]; 4] = [
            b"/bin/aterm",
            b"ATERM_VERIFY_RUN=71-1",
            b"ATERM_VERIFY_RUN=72-2",
            b"ATERM_CHILD=1",
        ];
        let read = markers_from_procargs2(&frame(1, &words)).expect("a buffer");
        assert_eq!(read.verify_run.as_deref(), Some("71-1"), "{read:?}");
        assert_eq!(read.child.as_deref(), Some("1"));
    }

    /// A pid namespace is the inode its link names, digits only; another kind of namespace, or
    /// a link with no inode, names none. Linux reads this process's; macOS has none.
    #[test]
    fn a_pid_namespace_is_the_inode_its_link_names() {
        assert_eq!(
            namespace_inode("pid:[4026531836]").as_deref(),
            Some("4026531836")
        );
        assert_eq!(namespace_inode("pid:[]"), None);
        assert_eq!(namespace_inode("net:[4026531840]"), None, "another kind");
        assert_eq!(namespace_inode("pid:[40265x]"), None);
        assert_eq!(pid_namespace().is_some(), cfg!(target_os = "linux"));
    }

    /// launchd's children split three ways by their launcher's pid version: launchd's own is
    /// launchd-started, another is an orphan launchd adopted, and with launchd's version unread
    /// neither is claimed. A process whose parent is not launchd is neither, and one with no
    /// launcher version (Linux) is never launchd-started.
    #[test]
    fn launchd_started_and_orphaned_split_launchds_children() {
        const LAUNCHD: i32 = 7;
        let unread = || -> Option<u64> { panic!("a versioned lineage reads no parent") };
        let started = lineage(1, Some(LAUNCHD));
        assert!(started.launchd_started(Some(LAUNCHD)), "{started:?}");
        assert!(!started.orphaned(Some(LAUNCHD), 100, unread), "{started:?}");
        let adopted = lineage(1, Some(31));
        assert!(!adopted.launchd_started(Some(LAUNCHD)), "{adopted:?}");
        assert!(adopted.orphaned(Some(LAUNCHD), 100, unread), "{adopted:?}");
        let l7 = lineage(LAUNCHD.unsigned_abs(), Some(LAUNCHD));
        assert!(!l7.launchd_started(Some(LAUNCHD)), "{l7:?}");
        assert!(!l7.orphaned(Some(LAUNCHD), 100, unread), "{l7:?}");
        for launchd_unread in [started, adopted] {
            assert!(!launchd_unread.launchd_started(None), "{launchd_unread:?}");
            assert!(
                !launchd_unread.orphaned(None, 100, unread),
                "{launchd_unread:?}"
            );
        }
        assert!(
            !lineage(1, None).launchd_started(None),
            "no versions prove no launch"
        );
    }

    /// Only a row the judge calls held counts, and the oldest is the oldest held row's fork age:
    /// a lone unstarted thread that is young, a started process and an unreadable row count
    /// nothing.
    #[test]
    fn held_programs_counts_old_unstarted_rows_only() {
        use aterm_sysprobe::jobs::StartFacts;
        const NINE_H: u64 = 9 * 3_600_000;
        let held = |age_ms| StartFacts {
            lone_zero: true,
            local: Some(true),
            age_ms: Some(age_ms),
            ..StartFacts::default()
        };
        let read = |pid| match pid {
            1 => Some(held(NINE_H)),
            2 => Some(held(20_000)),
            3 => Some(held(0)),
            4 => Some(StartFacts::default()),
            _ => None,
        };
        assert_eq!(
            held_among(&[1, 2, 3, 4, 5], read),
            HeldNow {
                count: 2,
                oldest_ms: NINE_H
            }
        );
        assert_eq!(held_among(&[3, 4, 5], read), HeldNow::default());
    }

    /// A held program's age reads in seconds, then minutes, then hours with two-digit minutes.
    #[test]
    fn a_held_age_reads_in_seconds_minutes_then_hours() {
        for (ms, words) in [
            (20_999, "20 s"),
            (59_999, "59 s"),
            (60_000, "1 m"),
            (3_599_999, "59 m"),
            (3_600_000, "1 h 00 m"),
            (36_300_000, "10 h 05 m"),
            (35_940_000, "9 h 59 m"),
        ] {
            assert_eq!(held_age(ms), words, "{ms}");
        }
    }

    #[test]
    fn a_launcher_version_is_older_across_the_wrap() {
        assert!(launched_before(10, 11));
        assert!(!launched_before(11, 10));
        assert!(launched_before(i32::MAX, i32::MIN));
    }

    /// The Linux environment's words go through the same selection as macOS's: only the three
    /// markers, the first of each, a word with no `=` skipped; an empty file is withheld and a
    /// read past the bound is unknown. NEGATIVE CONTROL: start `markers_from_words` with
    /// `withheld` false, and the empty file reads as an environment that names no run.
    #[test]
    fn linux_markers_are_read_off_environ_and_an_empty_one_is_withheld() {
        let environ = b"HOME=/h\0NO_EQUALS\0ATERM_VERIFY_RUN=71-1\0ATERM_CHILD=1\0\
                        ATERM_VERIFY_RUN=72-2\0ATERM_PARENT_SESSION_ID=s7\0";
        assert_eq!(
            markers_from_environ(environ),
            Some(Markers {
                withheld: false,
                parent_session: Some("s7".into()),
                child: Some("1".into()),
                verify_run: Some("71-1".into()),
                trust_check: None,
            })
        );
        let empty = markers_from_environ(b"").expect("an empty file reads");
        assert!(
            empty.withheld,
            "an empty environment is withheld: {empty:?}"
        );
        assert_eq!(empty.verify_run, None);
        let unterminated = markers_from_environ(b"ATERM_CHILD=1").expect("a last word");
        assert_eq!(unterminated.child.as_deref(), Some("1"));
        let past = vec![b'A'; super::super::PROCESS_ARGV_MAX + 1];
        assert_eq!(markers_from_environ(&past), None, "past the bound");
    }

    fn identity(idversion: Option<i32>) -> super::super::Identity {
        super::super::Identity {
            pid: 4242,
            birth: super::super::Birth(51_832_760),
            idversion,
        }
    }

    fn lineage(parent: u32, launcher_version: Option<i32>) -> Lineage {
        Lineage {
            identity: identity(launcher_version.map(|_| 9)),
            uniqueid: 51_832_760,
            parent,
            pgid: 4242,
            launcher_version,
            tty: None,
        }
    }

    /// A launcher is proven by two pid versions that agree, and by nothing where the kernel
    /// keeps none. NEGATIVE CONTROL: compare the two `Option`s directly, and the Linux row's two
    /// missing versions read as a launch.
    #[test]
    fn a_launcher_is_proven_only_by_two_versions() {
        assert_eq!(
            lineage(1, Some(7)).launched_by(&identity(Some(7))),
            Some(true)
        );
        assert_eq!(
            lineage(1, Some(7)).launched_by(&identity(Some(8))),
            Some(false)
        );
        assert_eq!(lineage(1, None).launched_by(&identity(None)), None);
        assert_eq!(lineage(1, Some(7)).launched_by(&identity(None)), None);
    }

    /// With a launcher version, orphaned is launchd's rule and the parent is never read; without
    /// one, init adopted it or a parent older than the run did. NEGATIVE CONTROLS: `<=` for `<`
    /// makes the run's own launcher (born at the floor) an adopter; dropping the older-parent
    /// clause misses the subreaper.
    #[test]
    fn orphaned_is_launchds_rule_with_a_version_and_an_older_adopter_without_one() {
        let unread = || -> Option<u64> { panic!("a versioned lineage reads no parent") };
        assert!(lineage(1, Some(7)).orphaned(Some(1026), 100, unread));
        assert!(
            !lineage(1, Some(1026)).orphaned(Some(1026), 100, unread),
            "launchd's own"
        );
        assert!(
            !lineage(1, Some(7)).orphaned(None, 100, unread),
            "launchd unread"
        );
        assert!(
            !lineage(7, Some(7)).orphaned(Some(1026), 100, unread),
            "a live parent"
        );

        assert!(
            lineage(1, None).orphaned(None, 100, || None),
            "init adopted it"
        );
        for (parent, want, case) in [
            (Some(10), true, "a subreaper older than the run"),
            (Some(100), false, "the run's own launcher"),
            (Some(150), false, "a parent the run started"),
            (None, false, "a parent that does not read"),
        ] {
            assert_eq!(
                lineage(4243, None).orphaned(None, 100, || parent),
                want,
                "{case}"
            );
        }
    }

    #[test]
    fn this_process_reads_its_lineage_and_its_group() {
        let me = i32::try_from(std::process::id()).expect("pid");
        if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
            assert_eq!(read_lineage(me), None);
            return;
        }
        let mine = read_lineage(me).expect("this process reads");
        assert_eq!(mine.identity.pid, me);
        assert_eq!(
            Some(mine.identity.birth),
            super::super::read_birth(me),
            "the birth read_birth reads"
        );
        let members = group_members(mine.pgid).expect("the group lists");
        assert!(members.pids.contains(&me), "{members:?}");
        assert!(!mine.orphaned(launchd_idversion(), 0, || None));
        if cfg!(target_vendor = "apple") {
            assert!(launchd_idversion().is_some(), "launchd's version reads");
        } else {
            assert_eq!(mine.identity.idversion, None, "Linux keeps no pid version");
        }
        assert_eq!(at_program_check(me), Some(false), "this process runs");
        let mut child = crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn");
        let pid = i32::try_from(child.id()).expect("pid");
        let read = read_lineage(pid);
        let listed = group_members(mine.pgid).is_some_and(|m| m.pids.contains(&pid));
        let _ = child.kill();
        let _ = child.wait();
        let read = read.expect("the child reads");
        assert_eq!(read.parent, me.unsigned_abs());
        assert_eq!(read.pgid, mine.pgid, "spawned into this group");
        if cfg!(target_vendor = "apple") {
            assert!(
                read.uniqueid > mine.uniqueid,
                "a later fork reads a larger unique id"
            );
        } else {
            assert!(
                read.uniqueid >= mine.uniqueid,
                "a later fork never reads an earlier tick"
            );
        }
        assert!(listed, "a child in the group is listed");
        assert_eq!(read_lineage(pid), None, "a reaped child reads nothing");
        assert_eq!(at_program_check(pid), None, "a reaped child is not judged");
        assert_eq!(
            at_program_check(1),
            None,
            "launchd's or init's is not judged"
        );
        assert!(boot_id().is_some(), "this boot's id reads");
    }

    /// To a reader that would read or signal it, a process whose one thread never ran has not
    /// run, whatever the program check's words call it: a start-suspended child (one thread,
    /// handle `0`, stopped and young, so not held) has not, and this process has.
    #[test]
    fn a_start_suspended_child_has_not_run_though_it_is_not_held() {
        let me = i32::try_from(std::process::id()).expect("pid");
        if !cfg!(target_vendor = "apple") {
            assert_eq!(thread_ran(me), cfg!(target_os = "linux").then_some(true));
            return;
        }
        let held = crate::deadline::Held::start(crate::spawnfd::command("/bin/sleep").arg("30"))
            .expect("a start-suspended sleep");
        let pid = i32::try_from(held.id()).expect("pid");
        assert_eq!(
            at_program_check(pid),
            Some(false),
            "the words do not hold it"
        );
        assert_eq!(thread_ran(pid), Some(false), "its thread never ran");
        assert_eq!(thread_ran(me), Some(true), "this process runs");
        drop(held);
    }
}
