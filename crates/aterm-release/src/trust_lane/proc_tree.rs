// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SIGKILL: i32 = 9;
pub const SIGTERM: i32 = 15;

unsafe extern "C" {
    fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn geteuid() -> u32;
    #[cfg(target_vendor = "apple")]
    fn getsid(pid: i32) -> i32;
}

/// Effective user ID of the current process.
#[must_use]
pub fn euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    unsafe { geteuid() }
}

/// Whether the current process has write permission on `dir` (`access(2)`).
#[must_use]
pub fn writable(dir: &std::path::Path) -> bool {
    const W_OK: i32 = 2;
    may(dir, W_OK).is_ok()
}

/// Whether the current process may execute `path` (`access(2)`): the error when not.
pub fn executable(path: &std::path::Path) -> std::io::Result<()> {
    const X_OK: i32 = 1;
    may(path, X_OK)
}

/// `access(path, mode)`.
fn may(path: &std::path::Path, mode: i32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: `c` is a live NUL-terminated string that `access` only reads.
    if unsafe { access(c.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Configures `cmd` to start in a new session (`setsid`).
pub const fn new_session(cmd: &mut aterm_uds::spawnfd::Command) {
    cmd.setsid(true);
}

/// What every process of one check carries: [`super::CHECK_VAR`]`=<nonce>` in the
/// environment it was exec'd with, and the root the lane spawned, by pid and serial
/// ([`aterm_uds::process::process_serial`]), once the record names it. The root leads a
/// session of its own, which its pid names ([`new_session`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The random nonce the lane gave the check; `None` for an earlier build's check,
    /// whose processes carry none.
    pub nonce: Option<String>,
    /// The root's pid and serial.
    pub root: Option<(i32, u64)>,
    /// The processes an earlier sweep kept as the check's ([`Procs::keep`]), by pid and
    /// serial, each the check's while its pid holds that serial.
    pub kept: BTreeSet<(i32, u64)>,
}

/// One of this user's processes as a sweep reads it ([`table`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    /// Its parent; 0 when unread.
    pub ppid: i32,
    /// Its serial, which each signal re-reads first ([`may_signal`]); `None` when unread,
    /// and then it is never signalled.
    pub serial: Option<u64>,
    /// Its session; `None` when unread.
    pub sid: Option<i32>,
    /// A `trustd`: the lane never ends one, nor what it runs.
    pub trustd: bool,
    /// Whether it carries the mark; `None` when that could not be read.
    pub marked: Option<bool>,
    /// Its executable, read only when `marked` could not be.
    pub exe: Option<PathBuf>,
    /// In the session the root's pid names, and working in, running from or holding a file
    /// in the slot, or unreadable as to that: read only while the lane does not hold the
    /// root ([`table`]).
    pub at_slot: bool,
    /// In the session the root's pid names while no other process holds that pid, and
    /// nothing shows it is a stranger's: its environment, when read, carries the mark's
    /// nonce (or the mark names none), and it never had a controlling terminal, which the
    /// root's session, made by `setsid` with stdin null, never gains. Once that session
    /// empties, the pid may come to lead a stranger's, which its leader can leave behind.
    /// Read only while the lane does not hold the root ([`table`]).
    pub in_session: bool,
    /// Caught mid-spawn (macOS): a `posix_spawn` child whose new image is not in yet reads
    /// as its spawner (name, path, session), and a signal sent to it lands on that image,
    /// maybe a `trustd`. It has neither run a thread nor exec'd, where a fork runs its
    /// parent's thread and a program held at its first exec has exec'd. It is never
    /// signalled; the next listing reads it as what it became.
    pub spawning: bool,
    /// The parents through which alone it is the check's ([`marked`]), each by pid and
    /// serial, from its own up to the first that is the check's by itself: it is signalled
    /// only while each still holds that serial, each under the next ([`may_signal`]). Empty
    /// in a listing's own rows.
    pub tie: Vec<(i32, u64)>,
}

/// One listing of this user's processes ([`table`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub procs: Vec<Proc>,
    /// Whether a list taken after its reads named no process identity it had not read,
    /// so none of the check's processes escaped it. A reused pid is a new identity.
    pub whole: bool,
}

/// What a sweep does with the check's processes it finds ([`end_marked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Sends them `SIGTERM`, then `SIGKILL`.
    End,
    /// Signals nothing: while any is listed, the check is left.
    Watch,
}

/// What a sweep found of a check ([`end_marked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Left {
    /// None of its processes runs, and none tied to it may be ([`unread`]), by a whole
    /// listing ([`Listing::whole`]).
    Nothing,
    /// Its processes were signalled, and a whole listing found none left.
    Ended,
    /// Its processes were signalled, or only watched ([`Act::Watch`]), and one is still
    /// listed, or one tied to it may be, or no whole listing followed.
    Lives,
    /// No whole listing was read, or one tied to the check may be its: nothing was
    /// signalled.
    Unread,
}

impl Left {
    /// Whether nothing of the check is left, so its record may go.
    #[must_use]
    pub const fn spent(self) -> bool {
        matches!(self, Left::Nothing | Left::Ended)
    }
}

/// The reads and signals a sweep takes ([`Live`]).
pub trait Procs {
    /// This user's processes, each judged against the mark; `None` when they could not
    /// be listed.
    fn table(&mut self) -> Option<Listing>;
    /// The check's session in `listed`, when it is proven ([`proven_session`]).
    fn session(&mut self, listed: &[Proc]) -> Option<i32>;
    /// Whether `p` still runs: its pid holds the process listed, and not as a zombie.
    fn alive(&mut self, p: &Proc) -> bool;
    /// Reads each of `procs` again and keeps in the check's record each that read shows
    /// may be signalled ([`may_signal`]), by pid and serial, so a later sweep finds it as
    /// the check's ([`Mark::kept`]) though this one dies after a signal that ended what
    /// tied it to the check. Returns whether the record keeps them.
    fn keep(&mut self, procs: &[Proc]) -> bool;
    /// Reads each of `procs` again, in order, and sends `sig` to each that read shows may be
    /// signalled ([`may_signal`]), but one caught mid-spawn ([`Proc::spawning`]). Returns
    /// each such read showed, signalled or mid-spawn, by pid and serial: the check's for as
    /// long as that serial runs.
    fn send(&mut self, procs: &[Proc], sig: i32) -> Vec<(i32, u64)>;
    /// How long a sweep waits for what it signalled to end before it lists again
    /// ([`settle`]): a second for real processes.
    fn settle_within(&self) -> Duration {
        Duration::from_secs(1)
    }
}

/// The session `root`, the root's pid, names, when every process in it is the check's: a
/// session holds only what its leader, the root, started, and no process or session is
/// given its number while the root's pid is held or a member is left. So it is proven while
/// this process holds the root as its unreaped child (`held`), or while one of the check's
/// own processes listed in it (the root, or one that carries the mark) `still` runs in it
/// after the listing, which covers every read the listing made.
pub fn proven_session(
    listed: &[Proc],
    root: Option<i32>,
    held: bool,
    still: &mut dyn FnMut(&Proc) -> bool,
) -> Option<i32> {
    let sid = root?;
    (held
        || listed
            .iter()
            .any(|p| p.marked == Some(true) && p.sid == Some(sid) && still(p)))
    .then_some(sid)
}

/// The kernel's [`Procs`] for one check's mark.
pub struct Live<'a> {
    pub mark: &'a Mark,
    /// The root's pid while this process holds it as its own unreaped child.
    pub held: Option<i32>,
    /// The check's slot, which a member of its session may be using ([`Proc::at_slot`]).
    pub slot: &'a Path,
    /// The check's record ([`super::keep_in_record`]).
    pub record: &'a Path,
}

impl Procs for Live<'_> {
    fn table(&mut self) -> Option<Listing> {
        table(self.mark, self.held, self.slot)
    }

    fn session(&mut self, listed: &[Proc]) -> Option<i32> {
        let root = self.held.or(self.mark.root.map(|(pid, _)| pid));
        proven_session(listed, root, self.held.is_some(), &mut |p| {
            kin(p.pid).is_some_and(|k| {
                !k.zombie && Some(k.serial) == p.serial && k.sid.is_some() && k.sid == p.sid
            })
        })
    }

    fn alive(&mut self, p: &Proc) -> bool {
        p.serial.is_some() && kin(p.pid).is_some_and(|k| !k.zombie && Some(k.serial) == p.serial)
    }

    fn keep(&mut self, procs: &[Proc]) -> bool {
        // A send's own reads, signalling nothing.
        super::keep_in_record(self.record, &send_with(procs, 0, &mut kin, &mut |_, _| {}))
    }

    // The pid can still pass to another process between the read and `kill`, kill(2)'s own
    // race; no sweep stops anything, so that leaves nothing stuck.
    fn send(&mut self, procs: &[Proc], sig: i32) -> Vec<(i32, u64)> {
        send_with(procs, sig, &mut kin, &mut |pid, sig| {
            // SAFETY: `kill` takes two integers and touches no memory of ours.
            unsafe {
                kill(pid, sig);
            }
        })
    }
}

/// [`Procs::send`] over the reads `kin` takes and the signals `signal` sends.
pub(super) fn send_with(
    procs: &[Proc],
    sig: i32,
    kin: &mut dyn FnMut(i32) -> Option<Kin>,
    signal: &mut dyn FnMut(i32, i32),
) -> Vec<(i32, u64)> {
    let mut reached = Vec::new();
    for p in procs {
        let Some(now) = kin(p.pid) else {
            continue;
        };
        if !may_signal(p, &now, kin) {
            continue;
        }
        reached.push((p.pid, now.serial));
        if !p.spawning {
            signal(p.pid, sig);
        }
    }
    reached
}

/// Whether `listed` may be signalled, its pid read again right before the signal as `now`,
/// and its parents through `kin`: it is still the process listed (no zombie, the listed
/// serial) under the parent listed, each parent that tied it ([`Proc::tie`]) still holds
/// its serial under the next, and it is neither a `trustd` nor run by one. So a child whose
/// parent died since (it reads its reaper as its parent), one tied through a pid that passed
/// to another process, at any depth, and one that exec'd a `trustd` since it was listed are
/// never signalled.
pub(super) fn may_signal(
    listed: &Proc,
    now: &Kin,
    kin: &mut dyn FnMut(i32) -> Option<Kin>,
) -> bool {
    !now.zombie
        && Some(now.serial) == listed.serial
        && now.ppid == listed.ppid
        && !now.trustd
        && !kin(now.ppid).is_some_and(|k| k.trustd)
        && listed.tie.iter().enumerate().all(|(i, &(pid, serial))| {
            kin(pid).is_some_and(|k| {
                k.serial == serial && listed.tie.get(i + 1).is_none_or(|&(up, _)| k.ppid == up)
            })
        })
}

/// What the kernel holds of one of this user's processes.
#[derive(Clone, Copy)]
pub(super) struct Kin {
    pub(super) ppid: i32,
    pub(super) serial: u64,
    /// Its session; `None` when unread.
    pub(super) sid: Option<i32>,
    pub(super) zombie: bool,
    /// It has, or had, a controlling terminal: macOS keeps the mark on a process once its
    /// session leader is gone; Linux clears it then.
    pub(super) ctty: bool,
    pub(super) trustd: bool,
    /// macOS: it runs the image it was forked with (no `PROC_FLAG_EXEC`), as a
    /// `posix_spawn` child does until its new image is in ([`Proc::spawning`]). Never set
    /// elsewhere.
    pub(super) forked: bool,
    /// Linux: a zombie thread-group leader whose other threads still run, which reads as
    /// a zombie (no image, no environment) while the process runs. Never set elsewhere.
    pub(super) leader_only: bool,
}

/// `pid`'s [`Kin`], or `None` when unread: gone, another user's, or no reader here.
#[cfg(target_vendor = "apple")]
fn kin(pid: i32) -> Option<Kin> {
    const SZOMB: u32 = 5;
    const PROC_FLAG_CONTROLT: u32 = 0x80;
    const PROC_FLAG_EXEC: u32 = 0x4000;
    const NODEV: u32 = u32::MAX;
    let info = aterm_uds::process::bsdinfo_uniq(pid)?;
    if info.pbsd.pbi_pid != pid.unsigned_abs() || info.pbsd.pbi_uid != euid() {
        return None;
    }
    let trustd = info
        .pbsd
        .pbi_comm
        .iter()
        .map(|c| c.to_ne_bytes()[0])
        .take_while(|&b| b != 0)
        .eq(b"trustd".iter().copied());
    // SAFETY: `getsid` takes an integer and touches no memory of ours.
    let sid = unsafe { getsid(pid) };
    // The session is the process read above only while its pid still holds it.
    let sid = (sid > 0 && aterm_uds::process::process_serial(pid) == Some(info.p_uniqueid))
        .then_some(sid);
    Some(Kin {
        ppid: i32::try_from(info.pbsd.pbi_ppid).unwrap_or(0),
        serial: info.p_uniqueid,
        sid,
        zombie: info.pbsd.pbi_status == SZOMB,
        ctty: info.pbsd.pbi_flags & PROC_FLAG_CONTROLT != 0 || info.pbsd.e_tdev != NODEV,
        trustd,
        forked: info.pbsd.pbi_flags & PROC_FLAG_EXEC == 0,
        leader_only: false,
    })
}

/// See the Apple twin: `/proc/<pid>/stat` ([`stat_kin`]).
#[cfg(target_os = "linux")]
fn kin(pid: i32) -> Option<Kin> {
    stat_kin(
        &aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/stat")))
            .ok()?,
        pid,
    )
}

/// See the Apple twin: nothing is read here.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn kin(_pid: i32) -> Option<Kin> {
    None
}

/// [`Kin`] from `pid`'s `/proc/<pid>/stat` line, `pid (comm) state ppid pgrp session
/// tty_nr …`, its serial the start in clock ticks ([`aterm_uds::process::process_serial`]).
/// A zombie counts itself among its threads until it is reaped, so one that counts more
/// is a leader whose other threads run (as `aterm_uds`' procfs reader tells them apart).
#[cfg(any(target_os = "linux", test))]
fn stat_kin(stat: &str, pid: i32) -> Option<Kin> {
    let open = stat.find(" (")?;
    if super::parse_uint::<i32>(stat[..open].trim())? != pid {
        return None;
    }
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 2..close)?;
    let mut fields = stat[close + 1..].split_ascii_whitespace();
    let state = fields.next()?;
    let ppid = super::parse_uint(fields.next()?)?;
    let sid = super::parse_uint(fields.nth(1)?).filter(|s| *s > 0);
    let tty: i64 = fields
        .next()
        .filter(|s| !s.starts_with('+'))?
        .parse()
        .ok()?;
    let threads: u64 = super::parse_uint(fields.nth(12)?)?;
    let serial = super::parse_uint(fields.nth(1)?)?;
    let exited = matches!(state, "Z" | "X" | "x");
    Some(Kin {
        ppid,
        serial,
        sid,
        zombie: exited && threads <= 1,
        ctty: tty != 0,
        trustd: comm == "trustd",
        forked: false,
        leader_only: exited && threads > 1,
    })
}

/// How long a listing lists before it is not whole ([`table`]). Each list is cheap (a
/// serial per pid, a row only for a new one), but on a busy machine every list may name a
/// process not yet read for seconds on end, where an idle machine's listing is whole within
/// milliseconds; so a listing gives up after a time, not a count of lists, and too short a
/// time holds slots whose checks left nothing.
pub(super) const LISTING: Duration = Duration::from_secs(10);

/// Rounds of `SIGKILL` a sweep sends, each settling up to a second, before it says one of
/// the check's processes lives ([`end_marked_with`]).
const KILLS: usize = 16;

/// This user's processes but this one and its zombies, each judged against `mark`: the
/// root and those it keeps by their pid and serial, any other by the environment it was
/// exec'd with ([`aterm_uds::process::read_process_markers`]), read only once its thread
/// ran (a read of a program held mid-start can hang). While this process does not hold
/// the root (`held`), a member of the session the root's pid names is read for its use of
/// `slot` ([`Proc::at_slot`]) and for whether the session is the root's
/// ([`Proc::in_session`]). A process gone mid-read is left out. `None` when they could not
/// be listed.
///
/// No list is atomic: a process that exits before its row is read may have forked one
/// after the list was taken. So the listing then reads each process a new list names that
/// it did not read yet, by pid and serial, and is whole once a list names none: whatever
/// runs then was read while it ran, or was started by what was. A serial unread or changed
/// across a row, or a process caught mid-spawn, is not cached; an exited, unreaped process
/// is read as [`EXITED`]. Once it has listed for [`LISTING`], a list after the first that
/// still names one ends it, not whole.
#[must_use]
pub fn table(mark: &Mark, held: Option<i32>, slot: &Path) -> Option<Listing> {
    #[cfg(test)]
    if let Some(pids) = LISTED.with(|l| l.borrow().clone()) {
        return table_from(mark, held, slot, LISTING, &mut || Ok(pids.clone()));
    }
    #[cfg(test)]
    let mut pids = || aterm_uds::holders::uid_pids().map(this_binarys);
    #[cfg(not(test))]
    let mut pids = aterm_uds::holders::uid_pids;
    table_from(mark, held, slot, LISTING, &mut pids)
}

#[cfg(test)]
thread_local! {
    /// The pids [`table`] lists on this thread in place of all of this user's, so a test
    /// can judge a sweep by a listing whose wholeness no other process's churn decides.
    pub(super) static LISTED: std::cell::RefCell<Option<Vec<u32>>> =
        const { std::cell::RefCell::new(None) };
}

/// Of `pids`, those this test binary started and those its tree left to an adopter: each
/// whose parents, read up from it while they are younger than this process, lead to this
/// process, or to an adopter of orphans that did not start it itself. The adopters are pid 1
/// and this process's ancestors, since a Linux subreaper (`systemd --user`) adopts only its
/// descendants' orphans. Apple keeps a process's launcher's pid version through adoption
/// ([`launched_by`]), so there what an adopter started itself, launchd's jobs and the
/// installed app's new tabs, is left out; Linux keeps none, so there every process an
/// ancestor started since this one is kept. On both, what another program daemonized since
/// this one started is kept, with all it starts. [`table`] lists only these under test, so
/// the rest of the machine's churn does not decide the wholeness a test's sweep needs: on a
/// loaded machine a list after the first nearly always names a process not read yet, and a
/// sweep then keeps the record it watched for as long as the load lasts. A listed process
/// gone by its read is left out; one whose parent went mid-read is kept, since an adopter
/// then holds it, and so is one whose parents' pids were reused into a loop.
#[cfg(test)]
fn this_binarys(pids: Vec<u32>) -> Vec<u32> {
    let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    let Some(born) = aterm_uds::process::process_serial(me) else {
        return pids;
    };
    let mut adopters = BTreeSet::from([1]);
    let mut up = me;
    while let Some(parent) = aterm_uds::process::read_process_parent(up)
        .and_then(|p| i32::try_from(p).ok())
        .filter(|&p| adopters.insert(p))
    {
        up = parent;
    }
    let mut known: BTreeMap<i32, bool> = BTreeMap::new();
    pids.into_iter()
        .filter(|&pid| {
            let Ok(pid) = i32::try_from(pid) else {
                return false;
            };
            let mut chain = Vec::new();
            let mut at = pid;
            let ours = loop {
                if at == me || chain.contains(&at) {
                    break true;
                }
                if let Some(&ours) = known.get(&at) {
                    break ours;
                }
                let Some(k) = kin(at) else {
                    break at != pid;
                };
                if k.serial < born {
                    break false;
                }
                chain.push(at);
                if adopters.contains(&k.ppid) {
                    break !launched_by(at, k.ppid);
                }
                at = k.ppid;
            };
            for p in chain {
                known.insert(p, ours);
            }
            ours
        })
        .collect()
}

/// Whether `parent`, `pid`'s parent now, started it rather than adopted it: its launcher's
/// pid version, which Apple keeps through adoption, is `parent`'s.
#[cfg(all(test, target_vendor = "apple"))]
fn launched_by(pid: i32, parent: i32) -> bool {
    let launcher = aterm_uds::process::bsdinfo_uniq(pid).map(|i| i.p_orig_ppidversion);
    launcher.is_some()
        && launcher == aterm_uds::process::uniq_identifier(parent).map(|i| i.p_idversion)
}

/// See the Apple twin: no launcher is kept here, so no process reads as its parent's own.
#[cfg(all(test, not(target_vendor = "apple")))]
fn launched_by(_pid: i32, _parent: i32) -> bool {
    false
}

/// The serial [`table_from`] reads for a process that exited and is not yet reaped, whose
/// own serial macOS no longer reads: it runs nothing again and its pid stays its own until
/// it is reaped, after which a process given that pid reads its real serial, never this.
const EXITED: u64 = u64::MAX;

/// [`table`] over the lists `pids` takes, listing for up to `within`.
pub(super) fn table_from(
    mark: &Mark,
    held: Option<i32>,
    slot: &Path,
    within: Duration,
    pids: &mut dyn FnMut() -> std::io::Result<Vec<u32>>,
) -> Option<Listing> {
    let session = mark.root.map(|(pid, _)| pid).filter(|_| held.is_none());
    // Another process at the root's pid leads any session that bears it.
    let taken = session.is_some_and(|pid| {
        aterm_uds::process::process_serial(pid)
            .is_some_and(|serial| mark.root.map(|(_, root)| root) != Some(serial))
    });
    let dirs: Vec<PathBuf> = std::iter::once(slot.to_path_buf())
        .chain(std::fs::canonicalize(slot).ok())
        .collect();
    let since = Instant::now();
    table_read(
        within,
        &mut || since.elapsed(),
        pids,
        &mut |pid| {
            let pid = i32::try_from(pid).ok()?;
            aterm_uds::process::process_serial(pid).or_else(|| {
                (aterm_uds::process::process_presence(pid)
                    == aterm_uds::process::ProcessPresence::Exited)
                    .then_some(EXITED)
            })
        },
        &mut |pid| row(mark, session, taken, &dirs, pid),
    )
}

/// [`table_from`]'s repeated lists and identity-checked row cache ([`table`]); `now` is its
/// clock.
pub(super) fn table_read(
    within: Duration,
    now: &mut dyn FnMut() -> Duration,
    pids: &mut dyn FnMut() -> std::io::Result<Vec<u32>>,
    serial: &mut dyn FnMut(u32) -> Option<u64>,
    row: &mut dyn FnMut(u32) -> Option<Proc>,
) -> Option<Listing> {
    let since = now();
    let mut read = BTreeMap::new();
    let mut procs = BTreeMap::new();
    let mut first = true;
    loop {
        let Ok(list) = pids() else {
            return (!first).then_some(Listing {
                procs: procs.into_values().collect(),
                whole: false,
            });
        };
        let mut new = false;
        for pid in list {
            let before = serial(pid);
            if before.is_some_and(|s| read.get(&pid) == Some(&s)) {
                continue;
            }
            new = true;
            let process = row(pid);
            if let Some(s) = before
                && serial(pid) == Some(s)
                && process
                    .as_ref()
                    .is_none_or(|p| p.serial == Some(s) && !p.spawning)
            {
                read.insert(pid, s);
            } else {
                read.remove(&pid);
            }
            // A pid that now holds another process must not leave its old parent,
            // mark or trustd status in the listing.
            procs.remove(&pid);
            if let Some(process) = process {
                procs.insert(pid, process);
            }
        }
        if !first && (!new || now().saturating_sub(since) >= within) {
            return Some(Listing {
                procs: procs.into_values().collect(),
                whole: !new,
            });
        }
        first = false;
    }
}

/// `pid`'s row ([`table`]), `None` for this process, a zombie, or one gone mid-read.
fn row(mark: &Mark, session: Option<i32>, taken: bool, dirs: &[PathBuf], pid: u32) -> Option<Proc> {
    use aterm_uds::holders::{Exe, exe_of};
    let p = i32::try_from(pid).ok()?;
    if pid == std::process::id() || p <= 1 {
        return None;
    }
    let kin = kin(p);
    if kin.as_ref().is_some_and(|k| k.zombie) {
        return None;
    }
    // A leader whose other threads still run has no image of its own to read: it is the
    // check's by its pid and serial, its session or its parent ([`marked`]), and a stranger's
    // tied to none of those counts for nothing ([`unread`]).
    let leader_only = kin.as_ref().is_some_and(|k| k.leader_only);
    let ran = aterm_uds::process::thread_ran(p);
    // An environment the kernel withheld (a platform binary's on macOS) reads as unread.
    let marked = match &kin {
        Some(k) if mark.root == Some((p, k.serial)) || mark.kept.contains(&(p, k.serial)) => {
            Some(true)
        }
        Some(_) if leader_only => None,
        Some(_) if ran == Some(true) => aterm_uds::process::read_process_markers(p)
            .filter(|m| !m.withheld)
            .map(|m| m.trust_check.is_some() && m.trust_check == mark.nonce),
        _ => None,
    };
    let exe = match marked {
        Some(_) => None,
        None if leader_only => None,
        None => match exe_of(pid) {
            Exe::Exited => return None,
            Exe::At(path) => Some(path),
            Exe::Unnamed => None,
        },
    };
    let sid = kin.as_ref().and_then(|k| k.sid);
    let member = session.is_some() && sid == session;
    let at_slot = member
        && !aterm_uds::holders::scan_path_users_of(&[pid], dirs, &[])
            .is_ok_and(|s| s.users.is_empty() && s.unreadable.is_empty());
    let another =
        kin.as_ref().is_some_and(|k| k.ctty) || (mark.nonce.is_some() && marked == Some(false));
    Some(Proc {
        pid: p,
        ppid: kin.as_ref().map_or(0, |k| k.ppid),
        serial: kin.as_ref().map(|k| k.serial),
        sid,
        trustd: kin.as_ref().is_some_and(|k| k.trustd),
        marked,
        exe,
        at_slot,
        in_session: member && !taken && !another,
        spawning: kin.as_ref().is_some_and(|k| k.forked) && ran != Some(true),
        tie: Vec::new(),
    })
}

/// Each `trustd` in `procs` and all it started, by parent: the lane never ends one.
fn spared(procs: &[Proc]) -> BTreeSet<i32> {
    let mut spared: BTreeSet<i32> = procs.iter().filter(|p| p.trustd).map(|p| p.pid).collect();
    loop {
        let before = spared.len();
        for p in procs {
            if spared.contains(&p.ppid) {
                spared.insert(p.pid);
            }
        }
        if spared.len() == before {
            return spared;
        }
    }
}

/// The check's processes in `procs`: each that carries its mark, each in its proven
/// `session` ([`proven_session`]), each a signal of this sweep reached (`reached`, by pid
/// and serial: [`Procs::send`]), and all they started (by parent, [`Proc::tie`]), never a
/// `trustd` or what it runs. Nothing else is the check's: not a process in its root's
/// group, nor one that works in or holds a file in its slot. macOS withholds a platform
/// binary's environment (`/bin/sh`, `/bin/sleep`, the `/usr/bin` tools), so once its
/// parent is gone such a process is found only by its session, while that is proven, or
/// as one a signal reached or a sweep kept ([`Mark::kept`]).
#[must_use]
pub fn marked(procs: &[Proc], session: Option<i32>, reached: &BTreeSet<(i32, u64)>) -> Vec<Proc> {
    let spared = spared(procs);
    marked_with_spared(procs, session, reached, &spared)
}

fn marked_with_spared(
    procs: &[Proc],
    session: Option<i32>,
    reached: &BTreeSet<(i32, u64)>,
    spared: &BTreeSet<i32>,
) -> Vec<Proc> {
    let mut out: Vec<Proc> = procs
        .iter()
        .filter(|p| {
            (p.marked == Some(true)
                || (session.is_some() && p.sid == session)
                || p.serial.is_some_and(|s| reached.contains(&(p.pid, s))))
                && !spared.contains(&p.pid)
        })
        .cloned()
        .collect();
    let mut seen: BTreeSet<i32> = out.iter().map(|p| p.pid).collect();
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].pid;
        let tie: Vec<(i32, u64)> =
            std::iter::once((parent, out[i].serial.unwrap_or(super::NO_SERIAL)))
                .chain(out[i].tie.iter().copied())
                .collect();
        for p in procs {
            if p.ppid == parent && !spared.contains(&p.pid) && seen.insert(p.pid) {
                out.push(Proc {
                    tie: tie.clone(),
                    ..p.clone()
                });
            }
        }
        i += 1;
    }
    out
}

/// Whether one of `procs` tied to the check, though not shown to be one of its processes,
/// may be: a member of its root's session ([`Proc::in_session`], [`Proc::at_slot`]); one
/// whose mark could not be read that runs from `slot`, where a program of the check's may
/// be in its first exec; or, on macOS, one whose mark and executable both could not be
/// read and that one carrying the mark started. A `trustd` and what it runs never count,
/// nor does a process tied to nothing, however unreadable: another user's is never
/// listed, and a stranger's would hold the slot for as long as it lives. Linux holds no
/// first exec, so there an executable it could not name counts for nothing.
#[must_use]
pub fn unread(procs: &[Proc], slot: &Path) -> bool {
    let canonical = std::fs::canonicalize(slot).ok();
    let spared = spared(procs);
    let started: BTreeSet<i32> = marked_with_spared(procs, None, &BTreeSet::new(), &spared)
        .iter()
        .map(|p| p.pid)
        .collect();
    procs.iter().any(|p| {
        !p.trustd
            && (p.at_slot
                || (p.in_session && !spared.contains(&p.pid))
                || (p.marked.is_none()
                    && p.exe.as_deref().map_or_else(
                        || cfg!(not(target_os = "linux")) && started.contains(&p.pid),
                        |exe| {
                            exe.starts_with(slot)
                                || canonical.as_deref().is_some_and(|c| exe.starts_with(c))
                        },
                    )))
    })
}

/// Sweeps what is left of a check ([`marked`]), sparing every `trustd`; `act` says whether
/// its processes are ended or only watched. `held` is the root's pid while this process
/// holds it unreaped, `slot` is what a process not shown to be the check's must use to
/// count ([`unread`]), and `record` keeps what the sweep signals ([`Procs::keep`]).
#[must_use]
pub fn end_marked(mark: &Mark, held: Option<i32>, slot: &Path, record: &Path, act: Act) -> Left {
    end_marked_with(
        &mut Live {
            mark,
            held,
            slot,
            record,
        },
        slot,
        act,
    )
}

/// [`end_marked`] over `procs`. No process is ever stopped, so a sweep cut short (its
/// lane killed) leaves each running or ended. The check's processes are sent `SIGTERM`;
/// after up to a second, those each fresh listing names are sent `SIGKILL`, until one
/// names none or [`KILLS`] listings have, and only a whole one that names none, with
/// nothing tied to the check left ([`unread`]), says it ended. A signal ends what tied a
/// survivor to the check (its parent, the carriers that proved its session), so each
/// listing counts what a signal already reached as the check's ([`marked`]), and the
/// record keeps each before it is signalled ([`Procs::keep`]); when it cannot, they get
/// `SIGKILL` at once, not a `SIGTERM` a survivor could outlive. Signals go to children
/// before their parents ([`targets`]) and never to a row caught mid-spawn; a watch
/// signals nothing.
pub(super) fn end_marked_with(procs: &mut dyn Procs, slot: &Path, act: Act) -> Left {
    let mut reached = BTreeSet::new();
    let Some((first, found)) = list(procs, &reached) else {
        return Left::Unread;
    };
    if found.is_empty() {
        return if first.whole && !unread(&first.procs, slot) {
            Left::Nothing
        } else {
            Left::Unread
        };
    }
    if act == Act::Watch {
        return Left::Lives;
    }
    let term = targets(&found);
    let sig = if procs.keep(&term) { SIGTERM } else { SIGKILL };
    reached.extend(procs.send(&term, sig));
    settle(procs, &term);
    for _ in 0..KILLS {
        let Some((listed, mine)) = list(procs, &reached) else {
            return Left::Lives;
        };
        if mine.is_empty() {
            return if listed.whole && !unread(&listed.procs, slot) {
                Left::Ended
            } else {
                Left::Lives
            };
        }
        let kill = targets(&mine);
        procs.keep(&kill);
        reached.extend(procs.send(&kill, SIGKILL));
        settle(procs, &kill);
    }
    Left::Lives
}

/// The check's processes `mine` in the order a signal goes to them, children before their
/// parents, so a child's listed parent still holds when it is signalled ([`may_signal`]).
fn targets(mine: &[Proc]) -> Vec<Proc> {
    let mut ppids = std::collections::BTreeMap::new();
    for q in mine {
        ppids.entry(q.pid).or_insert(q.ppid);
    }
    let depth = |p: &Proc| {
        let mut depth = 0;
        let mut at = p.ppid;
        while let Some(&parent_ppid) = ppids.get(&at) {
            depth += 1;
            if depth > mine.len() {
                break;
            }
            at = parent_ppid;
        }
        depth
    };
    let mut out = mine.to_vec();
    out.sort_by_cached_key(|p| std::cmp::Reverse(depth(p)));
    out
}

/// One listing, and the check's processes in it, with each `reached` names ([`marked`]).
fn list(procs: &mut dyn Procs, reached: &BTreeSet<(i32, u64)>) -> Option<(Listing, Vec<Proc>)> {
    let listed = procs.table()?;
    let session = procs.session(&listed.procs);
    let mine = marked(&listed.procs, session, reached);
    Some((listed, mine))
}

/// Waits until none of `set` runs but those caught mid-spawn, which no signal went to, for
/// at most [`Procs::settle_within`].
fn settle(procs: &mut dyn Procs, set: &[Proc]) {
    let deadline = crate::durable::deadline_after(procs.settle_within());
    while set.iter().any(|p| !p.spawning && procs.alive(p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
#[path = "proc_tree_tests.rs"]
mod tests;
