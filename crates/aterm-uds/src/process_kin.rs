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

    /// Orphaned. With a launcher version (Apple): launchd adopted it, and its launcher's version
    /// is not `launchd`, launchd's own ([`launchd_idversion`]; unread, nothing is orphaned);
    /// `parent_uniqueid` is never called. Without one (Linux, which keeps no original parent):
    /// init adopted it, or its parent is older than `floor`, the unique id of the run it is
    /// judged for. No process the run started is older than the run, so such a parent is an
    /// adopter (a subreaper, as `systemd --user` is on desktop Linux); the run's own launcher,
    /// born at `floor`, never counts. `parent_uniqueid` reads the parent's unique id; `None`
    /// (unreadable, another uid) is not orphaned.
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

/// Lists up to [`MAX_GROUP_PIDS`] members of process group `pgid`
/// (`proc_listpids(PROC_PGRP_ONLY)`).
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
    crate::holders::clear_errno();
    // SAFETY: `pids` is `bytes` writable bytes of pid slots; libproc (`holders`' declaration)
    // writes at most that many and answers how many bytes it wrote, or a number [`group_listing`]
    // reads as a failure.
    let got = unsafe {
        crate::holders::ffi::proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr(), bytes)
    };
    group_listing(got, crate::holders::errno(), &pids)
}

/// `proc_listpids`'s answer, `got` bytes and the `errno` read after it, as a listing of `pids`.
/// libproc answers 0 both for a group with no process and for a failure, a sandbox's denial
/// among them, which alone sets `errno`; read as a whole empty listing, a failure would prove the
/// group empty.
#[cfg(target_vendor = "apple")]
fn group_listing(got: i32, errno: i32, pids: &[i32; MAX_GROUP_PIDS]) -> Option<GroupMembers> {
    if got == 0 && errno != 0 {
        return None;
    }
    let got = usize::try_from(got).ok()?;
    let n = (got / std::mem::size_of::<i32>()).min(MAX_GROUP_PIDS);
    Some(GroupMembers {
        pids: pids[..n].iter().copied().filter(|p| *p > 0).collect(),
        complete: n < MAX_GROUP_PIDS,
    })
}

/// A failed read is no listing, whatever it wrote: 0 with `errno` set and a negative count are
/// `None`, while 0 with no `errno` is a whole, empty group. NEGATIVE CONTROL: drop the `errno`
/// check from [`group_listing`] and a denied read lists a group as empty.
#[cfg(all(test, target_vendor = "apple"))]
#[test]
fn a_failed_group_listing_is_no_listing() {
    let pids = [0i32; MAX_GROUP_PIDS];
    assert_eq!(group_listing(0, libc::EPERM, &pids), None);
    assert_eq!(group_listing(-1, 0, &pids), None);
    assert_eq!(
        group_listing(0, 0, &pids),
        Some(GroupMembers {
            pids: Vec::new(),
            complete: true,
        })
    );
    let mut one = pids;
    one[0] = 4242;
    assert_eq!(
        group_listing(4, libc::EPERM, &one),
        Some(GroupMembers {
            pids: vec![4242],
            complete: true,
        }),
        "errno means nothing beside a count"
    );
}

/// See the Apple twin: every `/proc/<pid>/stat` ([`super::stat_row`]), then the row of every
/// id allocated behind that walk ([`super::process_row`]), judged by
/// [`super::procfs::chased_group_of`], since a walk of `/proc` is no snapshot as Apple's
/// listing is. A process gone by its read is skipped; any other failed read leaves the listing
/// incomplete, and so do allocations it could not follow and a `/proc` that can show no whole
/// listing ([`group_listing_can_be_whole`]).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[must_use]
pub fn group_members(pgid: u32) -> Option<GroupMembers> {
    use super::procfs::StatRead;
    if pgid <= 1 {
        return None;
    }
    let from = super::last_pid();
    let rows = std::fs::read_dir("/proc").ok()?.filter_map(|entry| {
        let Ok(entry) = entry else {
            return Some((0, StatRead::Unreadable));
        };
        let pid = entry
            .file_name()
            .to_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<i32>()
            .ok()?;
        Some((pid, super::stat_row(&entry.path().join("stat"))))
    });
    Some(super::procfs::chased_group_of(
        rows,
        from,
        super::last_pid,
        super::pid_max,
        super::process_row,
        pgid,
        group_listing_can_be_whole(),
    ))
}

/// Whether a [`group_members`] listing here can be complete: on Linux, whether `/proc` lists
/// every process to every reader ([`super::procfs::shows_every_process`] over
/// `/proc/self/mountinfo`), is this process's own pid namespace's
/// ([`super::proc_is_this_namespaces`]), and reads the namespace's last allocated id
/// (`ns_last_pid`), which a listing follows past its walk; on Apple, whose kernel lists a group
/// itself, it can; elsewhere nothing lists one.
#[must_use]
pub fn group_listing_can_be_whole() -> bool {
    #[cfg(target_vendor = "apple")]
    {
        true
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        super::proc_shows_every_process()
            && super::proc_is_this_namespaces()
            && super::last_pid().is_some()
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

/// Whether a child kept from this process's `ATERM_*` settings has `name` removed: every
/// `ATERM_*` variable but [`VERIFY_RUN_VAR`]. The verify run's mark is no authority, and with it
/// the run still reads the child as its own ([`read_process_markers`]).
#[must_use]
pub fn stripped_from_child(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with("ATERM_") && name != VERIFY_RUN_VAR
}

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
/// Linux (`/proc/<pid>/stat`). `None` when unread: no such process, a zombie on macOS, or no
/// reader here.
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
        let stat =
            super::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat"))).ok()?;
        super::procfs::stat_start(&stat, pid).map(|(_, ticks)| ticks)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        None
    }
}

/// This OS machine's stable identity, independent of a boot, HOME and copied state directories.
/// Apple uses gethostuuid; Linux requires a root-owned, non-writable regular /etc/machine-id.
/// Missing, malformed or unsupported identity stays unknown. OS clones must have distinct
/// provisioned machine identities; this is no remote attestation or physical hardware proof.
#[must_use]
pub fn host_id() -> Option<String> {
    static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOST.get_or_init(read_host_id).clone()
}

#[cfg(target_vendor = "apple")]
fn read_host_id() -> Option<String> {
    unsafe extern "C" {
        fn gethostuuid(id: *mut u8, wait: *const libc::timespec) -> libc::c_int;
    }
    let mut id = [0u8; 16];
    let wait = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: id has exactly uuid_t's 16 writable bytes. wait is a valid, live timespec;
    // neither pointer is retained by gethostuuid.
    if unsafe { gethostuuid(id.as_mut_ptr(), &raw const wait) } != 0 || id == [0; 16] {
        return None;
    }
    use std::fmt::Write as _;
    let mut text = String::from("darwin:");
    for byte in id {
        let _ = write!(text, "{byte:02x}");
    }
    Some(text)
}

#[cfg(target_os = "linux")]
fn read_host_id() -> Option<String> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/etc/machine-id")
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 64
    {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(metadata.len().saturating_add(1)).unwrap_or(0));
    file.take(65).read_to_string(&mut text).ok()?;
    let text = text.trim();
    (text.len() == 32
        && text.bytes().all(|byte| byte.is_ascii_hexdigit())
        && text.bytes().any(|byte| byte != b'0'))
    .then(|| format!("linux:{}", text.to_ascii_lowercase()))
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn read_host_id() -> Option<String> {
    None
}

/// This boot's id, which a record kept for one boot names ([`astream_sys::boot_session`]).
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
/// in `/proc/self/ns/pid` (`namespace_inode`). Namespaces share a boot, so a record of pids
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

/// Whether a program-check hold is established for `pid`: `Some(true)` when
/// [`aterm_sysprobe::jobs::at_start`] judges it [`aterm_sysprobe::jobs::AtStart::Held`]
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.2). `None` when unread (gone, another
/// user's, a pid that is no process). `Some(false)` includes an unresolved start:
/// the native libproc reader does not identify a per-PID syspolicyd request.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn at_program_check(pid: i32) -> Option<bool> {
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
    aterm_sysprobe::jobs::at_start(pid).map(|at| at == aterm_sysprobe::jobs::AtStart::Held)
}

/// See the Apple twin: Linux has no program check, so a process [`read_identity`] reads (live,
/// this uid's) is never held there, and any other pid (gone, a zombie, another uid's, a thread's
/// id, pid 1) is unread, as Apple's `ESRCH` and `EPERM` are.
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

/// Whether `pid`'s one thread has run, for a reader that would read its arguments or signal it:
/// A nonzero thread handle or positive kernel thread user time establishes execution.
/// A lone zero handle with zero user time remains unresolved (`Some(false)`),
/// whatever its age, arch, volume or stop. Its arguments can hang a reader and
/// whether a signal reaches it is unmeasured. `None` when unread, including
/// unread user time for a lone zero handle. Neither unresolved result attributes a cause.
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn thread_ran(pid: i32) -> Option<bool> {
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
    aterm_sysprobe::jobs::read_start(pid, aterm_sysprobe::jobs::now_us()).and_then(|f| {
        if f.lone_zero {
            f.user_ns.map(|ns| ns > 0)
        } else {
            Some(true)
        }
    })
}

/// See the Apple twin: nothing on Linux holds a program before its first instruction, so every
/// positive pid answers `Some(true)` with nothing read.
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
    /// The oldest held program's fork age in ms (an upper bound on its wait); 0 when none is.
    pub oldest_ms: u64,
}

impl HeldNow {
    /// Whether this reading, taken after `before` (the same reader's earlier one; `None` when that
    /// one did not read), is a backed-up queue by the jobs engine's rule:
    /// [`aterm_vitals::jobs::HELD_AT_ONCE`] held at once, or a program held at both readings. A
    /// count names no row, so a different program held at each counts too: each has stood unrun
    /// past the 20 s age bound, where a normal exec reads so for under 0.3 ms. A new program
    /// started now would wait in the same line.
    #[must_use]
    pub const fn backed_up(&self, before: Option<Self>) -> bool {
        self.count >= aterm_vitals::jobs::HELD_AT_ONCE
            || (self.count > 0 && matches!(before, Some(b) if b.count > 0))
    }
}

/// How long a program has waited at macOS's program check, as every surface that counts the wait
/// words it: `20 s` under a minute, `5 m` under an hour, then `10 h 05 m`.
#[must_use]
pub fn held_age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} m", s / 60),
        _ => format!("{} h {:02} m", s / 3600, s % 3600 / 60),
    }
}

/// [`held_programs`]' count over `pids`, each read by `read`: a row `read` cannot read is
/// skipped, and only one [`aterm_sysprobe::jobs::judge_start`] calls held is counted.
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

/// This user's programs macOS's program check holds now: one two-slot thread list per process of
/// this uid, and the start records of a lone `0` only, with one mount table for them all (no
/// argument or environment is read). `None` when the process list cannot be read, and off macOS.
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

/// How many of `pids`, each read by `read`, are held at macOS's program check
/// ([`aterm_sysprobe::jobs::judge_start`]) and forked at or after `since_us` (µs since the epoch,
/// [`aterm_sysprobe::jobs::StartFacts::start_us`]): checks sent to a syspolicyd that started at
/// `since_us` that it has yet to answer. A held row whose fork is unread counts, since it may be
/// one. The count is a lower bound of that daemon's queue: a row `read` cannot read is skipped,
/// and a fork is all one read dates, so a check queued before `since_us` or sent after it by a
/// pid that forked before it is not counted.
#[cfg(any(unix, windows))]
#[must_use]
pub fn held_since_among(
    pids: &[u32],
    read: impl Fn(u32) -> Option<aterm_sysprobe::jobs::StartFacts>,
    since_us: u64,
) -> usize {
    use aterm_sysprobe::jobs::{AtStart, judge_start};
    pids.iter()
        .filter_map(|&pid| read(pid))
        .filter(|f| judge_start(f) == AtStart::Held && f.start_us.is_none_or(|s| s >= since_us))
        .count()
}

/// [`held_since_among`] over this user's processes, read as [`held_programs`] reads them: how
/// many held programs forked at or after `since_us`, the start of the syspolicyd running now.
/// `None` when the process list cannot be read, and off macOS.
#[cfg(target_os = "macos")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn held_since(since_us: u64) -> Option<usize> {
    let pids = crate::holders::uid_pids().ok()?;
    let now = aterm_sysprobe::jobs::now_us();
    let mounts = std::cell::OnceCell::new();
    Some(held_since_among(
        &pids,
        |pid| aterm_sysprobe::jobs::read_start_in(pid, now, &mounts),
        since_us,
    ))
}

/// See the macOS twin: there is no program check to count here.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn held_since(_since_us: u64) -> Option<usize> {
    None
}

#[cfg(all(test, unix))]
#[path = "process_kin_tests.rs"]
mod tests;
