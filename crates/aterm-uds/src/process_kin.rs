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

/// Lineage of a live same-uid process (flavor 18).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lineage {
    /// Who it is: what the end protocol re-reads before each signal ([`super::end_proven`]).
    pub identity: super::Identity,
    /// Its unique id: a later fork reads a larger one.
    pub uniqueid: u64,
    /// Its parent now: `1` once launchd adopted it.
    pub parent: u32,
    /// Its process group.
    pub pgid: u32,
    /// Its launcher's pid version (`p_orig_ppidversion`).
    pub launcher_version: i32,
    /// Its session's controlling terminal (`e_tdev`).
    pub tty: Option<u32>,
}

impl Lineage {
    /// Whether launchd adopted the process without having started it ([`launchd_idversion`]).
    #[must_use]
    pub fn orphaned(&self, launchd: Option<i32>) -> bool {
        self.parent == 1 && launchd.is_some_and(|v| v != self.launcher_version)
    }

    /// Started by launchd itself: launchd is its parent, and its launcher's pid version is
    /// launchd's ([`launchd_idversion`]). Such a process is never an orphan.
    #[must_use]
    pub fn launchd_started(&self, launchd: Option<i32>) -> bool {
        self.parent == 1 && launchd == Some(self.launcher_version)
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
            birth: ProcessBirth {
                seconds: info.pbsd.pbi_start_tvsec,
                microseconds: info.pbsd.pbi_start_tvusec,
            },
            idversion: info.p_idversion,
        },
        uniqueid: info.p_uniqueid,
        parent: info.pbsd.pbi_ppid,
        pgid: info.pbsd.pbi_pgid,
        launcher_version: info.p_orig_ppidversion,
        tty: (info.pbsd.e_tdev != NODEV).then_some(info.pbsd.e_tdev),
    })
}

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
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
    /// The members, a zombie among them while it is unreaped (an exited stage leader is).
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

/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn group_members(_pgid: u32) -> Option<GroupMembers> {
    None
}

/// Environment marker variables read by [`read_process_markers`].
pub const MARKER_VARS: [&str; 3] = ["ATERM_PARENT_SESSION_ID", "ATERM_CHILD", VERIFY_RUN_VAR];

/// `ATERM_VERIFY_RUN`.
pub const VERIFY_RUN_VAR: &str = "ATERM_VERIFY_RUN";

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
}

/// `pid`'s [`Markers`], read between two identity reads that must agree ([`bracketed`]).
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
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn bracketed<T>(
    pid: i32,
    mut identity: impl FnMut(i32) -> Option<super::Identity>,
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
    let mut markers = Markers {
        withheld: true,
        ..Markers::default()
    };
    let mut seen = [false; MARKER_VARS.len()];
    for word in p.env() {
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
            _ => markers.verify_run = value,
        }
    }
    Some(markers)
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

/// Whether macOS's program check holds `pid` now: `Some(true)` is the whole held predicate (`docs/DESIGN-leftover-processes-2026-10-16.md` §4.2), one thread that never ran, 20 s or more since the fork, neither stopped nor traced, native, its executable on a local volume, as aterm-sysprobe's one reader judges it ([`aterm_sysprobe::jobs::at_start`]). `None` when unread (gone, another user's, a pid that is no process).
#[cfg(target_vendor = "apple")]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn at_program_check(pid: i32) -> Option<bool> {
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
    aterm_sysprobe::jobs::at_start(pid).map(|at| at == aterm_sysprobe::jobs::AtStart::Held)
}

/// See the Apple twin: Linux has no program check, so no process waits at one, and every positive pid answers `Some(false)` with nothing read (whether it lives is the birth readers' to say).
#[cfg(target_os = "linux")]
#[must_use]
pub fn at_program_check(pid: i32) -> Option<bool> {
    (pid > 0).then_some(false)
}

/// See the Apple twin: nothing is read here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
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
    fn only_the_three_markers_are_read_and_an_empty_environment_is_withheld() {
        let words: [&[u8]; 6] = [
            b"/bin/aterm",
            b"ATERM_VERIFY_RUN=argv-not-env",
            b"HOME=/h",
            b"ATERM_VERIFY_RUN=71-1792163800331520",
            b"ATERM_PARENT_SESSION_ID=s7",
            b"ATERM_CHILD=1",
        ];
        let read = markers_from_procargs2(&frame(2, &words)).expect("a buffer");
        assert_eq!(
            read,
            Markers {
                withheld: false,
                parent_session: Some("s7".into()),
                child: Some("1".into()),
                verify_run: Some("71-1792163800331520".into()),
            }
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

    #[test]
    fn markers_read_across_an_exec_are_refused() {
        let id = |idversion| super::super::Identity {
            pid: 71,
            birth: super::super::ProcessBirth {
                seconds: 1,
                microseconds: 2,
            },
            idversion,
        };
        let script = |reads: Vec<Option<super::super::Identity>>| {
            let mut reads = reads.into_iter();
            bracketed(71, move |_| reads.next().flatten(), || Some("env"))
        };
        assert_eq!(script(vec![Some(id(7)), Some(id(7))]), Some("env"));
        assert_eq!(script(vec![Some(id(7)), Some(id(8))]), None, "an exec");
        assert_eq!(script(vec![Some(id(7)), None]), None, "an exit");
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

    /// launchd's children split three ways by their launcher's pid version: launchd's own is
    /// launchd-started, another is an orphan launchd adopted, and with launchd's version unread
    /// neither is claimed. A process whose parent is not launchd is neither.
    #[test]
    fn launchd_started_and_orphaned_split_launchds_children() {
        const LAUNCHD: i32 = 7;
        let lineage = |parent, launcher_version| Lineage {
            identity: super::super::Identity {
                pid: 4242,
                birth: super::super::ProcessBirth {
                    seconds: 1,
                    microseconds: 2,
                },
                idversion: 90,
            },
            uniqueid: 900,
            parent,
            pgid: 4242,
            launcher_version,
            tty: None,
        };
        let started = lineage(1, LAUNCHD);
        assert!(started.launchd_started(Some(LAUNCHD)), "{started:?}");
        assert!(!started.orphaned(Some(LAUNCHD)), "{started:?}");
        let adopted = lineage(1, 31);
        assert!(!adopted.launchd_started(Some(LAUNCHD)), "{adopted:?}");
        assert!(adopted.orphaned(Some(LAUNCHD)), "{adopted:?}");
        let l7 = lineage(LAUNCHD.unsigned_abs(), LAUNCHD);
        assert!(!l7.launchd_started(Some(LAUNCHD)), "{l7:?}");
        assert!(!l7.orphaned(Some(LAUNCHD)), "{l7:?}");
        for unread in [started, adopted] {
            assert!(!unread.launchd_started(None), "{unread:?}");
            assert!(!unread.orphaned(None), "{unread:?}");
        }
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

    #[test]
    fn a_launcher_version_is_older_across_the_wrap() {
        assert!(launched_before(10, 11));
        assert!(!launched_before(11, 10));
        assert!(launched_before(i32::MAX, i32::MIN));
    }

    #[test]
    fn this_process_reads_its_lineage_and_its_group() {
        let me = i32::try_from(std::process::id()).expect("pid");
        if !cfg!(target_vendor = "apple") {
            assert_eq!(
                at_program_check(me),
                cfg!(target_os = "linux").then_some(false),
                "Linux has no program check to wait at"
            );
            assert_eq!(read_lineage(me), None);
            return;
        }
        let mine = read_lineage(me).expect("this process reads");
        assert_eq!(mine.identity.pid, me);
        assert_eq!(
            Some(mine.identity.birth),
            super::super::read_process_birth(me),
            "the birth read_process_birth reads"
        );
        let members = group_members(mine.pgid).expect("the group lists");
        assert!(members.pids.contains(&me), "{members:?}");
        assert!(!mine.orphaned(launchd_idversion()));
        assert!(launchd_idversion().is_some(), "launchd's version reads");
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
        assert!(
            read.uniqueid > mine.uniqueid,
            "a later fork reads a larger unique id"
        );
        assert!(listed, "a child in the group is listed");
        assert_eq!(read_lineage(pid), None, "a reaped child reads nothing");
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
