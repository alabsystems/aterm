// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tether watcher's end of the process group its child leads ([`end_group`]), and its watch on
//! that group once the child exits first ([`linger`]), on Darwin and Linux.
//!
//! A tethered child spawned at the head of its own group (`process_group(0)`, or a session) is a
//! pipeline's root: Git's `fetch` runs `remote-http`, which runs `fetch-pack`, which runs
//! `index-pack`, all in that group. Ending the root alone leaves the rest adopted (by launchd; on
//! Linux by init, or the nearest subreaper) and running on (Git ends none of them on a
//! `SIGTERM`), so the watcher of a group leader signals the group, never a single member of it.
//!
//! A watcher knows its group one of two ways ([`GroupControl::census`]). From Linux 6.9 it holds
//! a pidfd of the child, opened while the child led the group, and the kernel signals the group by
//! it (`pidfd_send_signal` with `PIDFD_SIGNAL_PROCESS_GROUP`, [`GroupPidfd`]): it reaches the
//! processes whose group is the pid the descriptor names, by the kernel's own record of that pid
//! (its `struct pid`, which the descriptor keeps) rather than its number, which a later group may
//! reuse, so the group is the one the child led even after the child was reaped or left it; its
//! signal `0` answers `ESRCH` once no process, zombie or live, is left in it. Measured on Linux 6.17: after the leader was reaped a member
//! still answers, `ESRCH` once it exits; a group holding only an unreaped zombie answers `0`.
//!
//! Elsewhere, on Darwin and on an older Linux, the group is the child's for as long as the
//! watcher lives by its anchor ([`Listed`]): before it leaves the child's group, the watcher forks
//! a child of its own that exits at once and that it never reaps, so the group always holds that
//! zombie. XNU keeps a group while any member, a zombie included, is in it, and allocates no pid
//! that names a live group, so the group id names the child's group and no other until the
//! watcher exits and launchd reaps the anchor (`kill(-pgid, 0)` answers `EPERM` while only the
//! anchor is left, `ESRCH` once it is reaped). Linux frees a pid only once no process holds it as
//! its pid, its group's or its session's, a zombie included until it is reaped, so the same holds
//! there (`kill(-pgid, 0)` answers `0` while only the anchor is left, as Linux signals a zombie).
//! Every member a group signal reaches is therefore in the child's group: a member the watcher
//! never saw, the leader's death before the watcher's first look, or a member forked unsignalled
//! as a signal landed whose parent then died changes nothing. The watcher signals only before it
//! exits, so no reuse of the id can come between a read and a signal. Such a watcher judges the
//! group empty by listings ([`live`]), which only a `/proc` that lists every process can make
//! whole on Linux.

use std::time::{Duration, Instant};

use crate::process::ProcessPresence;

/// How long [`end_group`] keeps sending `SIGKILL` rounds to a group that has not emptied: a member
/// a `SIGKILL` cannot end (one blocked in the kernel) is left to the kernel after this. XNU's fork
/// lets a child born as a group signal lands join the group unsignalled, so one round is not
/// enough there (`process::kill_group_until_empty`).
pub(super) const KILL_ROUNDS: Duration = Duration::from_secs(5);

/// The longest pause between two of [`end_group`]'s reads or rounds.
const PAUSE: Duration = Duration::from_millis(20);

/// How long [`linger`] waits before it reads the group again when no exit can wake it: the
/// listing did not read, a member's exit could not be watched, or the census names no member
/// ([`GroupPidfd`]).
pub(super) const UNWATCHED_RECHECK: Duration = Duration::from_secs(1);

/// How long [`linger`] waits on its listed members' exits before it lists the group again: a
/// member can leave the group without exiting (`setsid`, `setpgid`; a daemon's detach), which no
/// exit watch sees, and the group, the anchor alone, is then empty. It bounds how long a watcher
/// outlives such a group; each wake costs one listing.
pub(super) const MEMBER_RECHECK: Duration = Duration::from_secs(5);

/// What woke [`GroupControl::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Woke {
    /// The parent exited or execed in its own place.
    Parent,
    /// Anything else: a member's exit, a member already gone, or the patience run out.
    Other,
}

/// Current execution evidence for the originally watched child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// Matching identity and unique id bracket a thread that has run.
    Started,
    /// Its original armed NOTE_EXIT was positively observed.
    Exited,
    /// Matching identity/unique id, but its thread has not run.
    Held,
    /// Required execution evidence could not be established.
    Unavailable,
}

impl Readiness {
    fn allows_signal(self) -> bool {
        matches!(self, Self::Started | Self::Exited)
    }
}

/// What [`end_group`] and [`linger`] read and do: the kernel, or a test's script.
pub(super) trait GroupControl {
    /// The live members of group `pgid` as one read finds them: by listings ([`live`]) where the
    /// group is held by an anchor, by the kernel's own count where it signals the group by pidfd
    /// ([`GroupPidfd`]).
    fn census(&mut self, pgid: i32) -> Live;
    /// `sig` to every member of group `pgid`.
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()>;
    /// Waits for the parent's exit or exec or for one of `members` to exit, at most `patience`
    /// when it is given.
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke;
    /// The clock.
    fn now(&mut self) -> Instant;
    /// Sleeps `pause`.
    fn nap(&mut self, pause: Duration);
}

/// The reads a census by listings takes ([`live`]), for a group its watcher holds by an anchor.
pub(super) trait Listed {
    /// The caller's actual, exited child, held unreaped for this entire watch.
    fn anchor(&self) -> i32;
    /// `pid`'s process group as it reads now: `None` when gone, a zombie, another user's, or
    /// unread.
    fn group_of(&mut self, pid: i32) -> Option<i32>;
    /// A positive ended/absent observation is distinct from an unread identity.
    fn presence(&mut self, pid: i32) -> ProcessPresence;
    /// The pids of group `pgid`'s members, and whether the listing is whole; `None` when unread.
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)>;
}

/// Ends this owned group only with positive execution evidence, renewed
/// immediately before every TERM/KILL. Point rechecks never wait or extend the
/// existing grace/round bounds. The original child's positive exit is monotone.
pub(super) fn end_group_guarded(
    pgid: i32,
    grace: Duration,
    control: &mut impl GroupControl,
    mut readiness: impl FnMut() -> Readiness,
) -> Option<GroupEnded> {
    if !readiness().allows_signal() {
        return None;
    }
    Some(end_group(pgid, grace, &mut Guarded { control, readiness }))
}

struct Guarded<'a, C, R> {
    control: &'a mut C,
    readiness: R,
}

impl<C: GroupControl, R: FnMut() -> Readiness> GroupControl for Guarded<'_, C, R> {
    fn census(&mut self, pgid: i32) -> Live {
        self.control.census(pgid)
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
        if !(self.readiness)().allows_signal() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "owned group execution readiness is held or unavailable",
            ));
        }
        self.control.signal_group(pgid, sig)
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        self.control.wait(members, patience)
    }
    fn now(&mut self) -> Instant {
        self.control.now()
    }
    fn nap(&mut self, pause: Duration) {
        self.control.nap(pause);
    }
}

/// What [`end_group`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GroupEnded {
    /// A `SIGTERM` went to the group.
    pub termed: bool,
    /// A `SIGKILL` went to the group (a member outlived the grace).
    pub killed: bool,
    /// A census found no live member left ([`Live::Empty`]).
    pub emptied: bool,
}

/// The live members of a group as one read finds them ([`GroupControl::census`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Live {
    /// No member is left: a fresh whole listing contains only the caller's owned exited anchor
    /// ([`live`]), or the kernel finds no process in the group, or two whole listings no live one
    /// ([`GroupPidfd`]).
    Empty,
    /// These read live in the group (a listing that was not whole may hold more).
    Members(Vec<i32>),
    /// Not shown empty, and no member named: the listing did not read, or was not whole and
    /// found none, or the kernel counts a member without naming it ([`GroupPidfd`]).
    Unknown,
}

/// Lists group `pgid` and reads each member's positive presence and group.
/// Empty requires a fresh whole listing of the caller's pinned anchor alone.
/// Earlier ended/absent pids cannot prove it: another member can fork after a
/// listing, reusing an earlier absent pid, then exit before its own read.
pub(super) fn live(control: &mut impl Listed, pgid: i32) -> Live {
    let Some((pids, whole)) = control.listing(pgid) else {
        return Live::Unknown;
    };
    let mut members = Vec::new();
    let mut unknown = !whole;
    for pid in pids {
        match control.presence(pid) {
            ProcessPresence::Exited | ProcessPresence::Absent => {}
            ProcessPresence::Present if control.group_of(pid) == Some(pgid) => members.push(pid),
            ProcessPresence::Present | ProcessPresence::Unknown => unknown = true,
        }
    }
    if !members.is_empty() {
        return Live::Members(members);
    }
    if unknown {
        return Live::Unknown;
    }
    // Only the actually owned, never-reaped anchor has lifetime continuity.
    // A recycled pid from the first list, even one previously observed ended,
    // is not this anchor and must keep the watch/recheck path alive.
    match control.listing(pgid) {
        Some((again, true))
            if again.as_slice() == [control.anchor()]
                && control.presence(control.anchor()) == ProcessPresence::Exited =>
        {
            Live::Empty
        }
        _ => Live::Unknown,
    }
}

/// Ends group `pgid`, the one the watched child led when its watcher armed, which the caller
/// holds (by its anchor, or by the child's pidfd): a `SIGTERM` to the group, then, for `grace`, a
/// wait for it to empty, then `SIGKILL` rounds for up to [`KILL_ROUNDS`] until it has. A group
/// already empty gets nothing. The caller has seen the child's thread run, or the child exit.
pub(super) fn end_group(pgid: i32, grace: Duration, control: &mut impl GroupControl) -> GroupEnded {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    let mut ended = GroupEnded::default();
    if control.census(pgid) == Live::Empty {
        ended.emptied = true;
        return ended;
    }
    // A refusal (`EPERM`: nothing the signal could reach) leaves the reads to judge the group.
    ended.termed = control.signal_group(pgid, SIGTERM).is_ok();
    let until = crate::deadline::saturating_after(control.now(), grace);
    let mut pause = Duration::from_millis(1);
    loop {
        if control.census(pgid) == Live::Empty {
            ended.emptied = true;
            return ended;
        }
        if control.now() >= until {
            break;
        }
        control.nap(pause);
        pause = (pause * 2).min(PAUSE);
    }
    let until = crate::deadline::saturating_after(control.now(), KILL_ROUNDS);
    let mut pause = Duration::from_millis(1);
    loop {
        if control.signal_group(pgid, SIGKILL).is_ok() {
            ended.killed = true;
        }
        control.nap(pause);
        pause = (pause * 2).min(PAUSE);
        if control.census(pgid) == Live::Empty {
            ended.emptied = true;
            return ended;
        }
        if control.now() >= until {
            return ended;
        }
    }
}

/// The watch on group `pgid` once the child that led it exited first, with the parent still
/// there: `false` once a census finds it empty ([`Live::Empty`]), `true` once the parent exits or
/// execs first, when the caller ends the group ([`end_group`]). Each census follows a wake: a
/// listed member's exit or [`MEMBER_RECHECK`], whichever comes first, since a member can leave
/// the group without exiting; or [`UNWATCHED_RECHECK`] when the census named no member. On
/// Darwin, whose listing shows a zombie, and by the kernel's count alone ([`GroupPidfd`] where no
/// listing can be whole), other exited members conservatively keep this watch until reaped or the
/// parent ends; Linux's listing shows none, and an exited member, which can fork nothing, is
/// passed by. A listed member's exit wakes
/// another census, including when it forked after enumeration.
pub(super) fn linger(pgid: i32, control: &mut impl GroupControl) -> bool {
    loop {
        let woke = match control.census(pgid) {
            Live::Empty => return false,
            Live::Members(members) => control.wait(&members, Some(MEMBER_RECHECK)),
            Live::Unknown => control.wait(&[], Some(UNWATCHED_RECHECK)),
        };
        if woke == Woke::Parent {
            return true;
        }
    }
}

/// `sig` to every member of group `pgid` (`kill(2)` of the negated id); refused for an id that
/// names no one group.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn live_signal(pgid: i32, sig: i32) -> std::io::Result<()> {
    if pgid <= 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{pgid} names no one process group"),
        ));
    }
    // SAFETY: a plain syscall taking no pointer. `pgid` > 1, so it names one group, never
    // this process's own (0) or every process (-1).
    if unsafe { libc::kill(-pgid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The kernel's [`GroupControl`]: [`read_lineage`](crate::process::read_lineage),
/// [`group_members`](crate::process::group_members), `kill(2)` of the negated group id
/// ([`live_signal`]), the watcher's kqueue (`kq`, which watches `parent`'s exit and exec) and the
/// thread's sleep.
#[cfg(target_vendor = "apple")]
pub(super) struct GroupLive<'a> {
    /// The watcher's kqueue.
    pub kq: i32,
    /// The parent the watcher ties the child to.
    pub parent: i32,
    /// Borrowed native wait custody, never reconstructed from a listed pid.
    pub anchor: &'a super::Anchor,
}

#[cfg(target_vendor = "apple")]
impl Listed for GroupLive<'_> {
    fn anchor(&self) -> i32 {
        self.anchor.pid
    }
    fn group_of(&mut self, pid: i32) -> Option<i32> {
        crate::process::read_lineage(pid).and_then(|lineage| i32::try_from(lineage.pgid).ok())
    }
    fn presence(&mut self, pid: i32) -> ProcessPresence {
        crate::process::process_presence(pid)
    }
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)> {
        crate::process::group_members(u32::try_from(pgid).ok()?)
            .map(|listing| (listing.pids, listing.complete))
    }
}

#[cfg(target_vendor = "apple")]
impl GroupControl for GroupLive<'_> {
    fn census(&mut self, pgid: i32) -> Live {
        live(self, pgid)
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
        live_signal(pgid, sig)
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        const BLANK: libc::kevent = super::watch_of(0, 0);
        let mut patience = patience;
        if !members.is_empty() {
            let changes: Vec<libc::kevent> = members
                .iter()
                .map(|&pid| super::watch_of(pid, libc::NOTE_EXIT))
                .collect();
            let mut receipts = vec![BLANK; changes.len()];
            let n = i32::try_from(changes.len()).unwrap_or(i32::MAX);
            // SAFETY: `changes` and `receipts` are live arrays of `n` kevents; with
            // `EV_RECEIPT` the call writes one receipt per change into `receipts` and waits on
            // nothing.
            let got = unsafe {
                libc::kevent(
                    self.kq,
                    changes.as_ptr(),
                    n,
                    receipts.as_mut_ptr(),
                    n,
                    std::ptr::null(),
                )
            };
            let Ok(got) = usize::try_from(got) else {
                return Woke::Other;
            };
            for receipt in &receipts[..got.min(receipts.len())] {
                if receipt.flags & libc::EV_ERROR != 0 && receipt.data != 0 {
                    // A member already gone: read the group again now. One whose exit cannot
                    // be watched: read it again after a while.
                    if receipt.data == libc::ESRCH as isize {
                        return Woke::Other;
                    }
                    patience = Some(UNWATCHED_RECHECK);
                }
            }
        }
        let pause = patience.map(|p| libc::timespec {
            tv_sec: libc::time_t::try_from(p.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(p.subsec_nanos()),
        });
        let mut events = [BLANK; 8];
        // SAFETY: `events` is a live array of eight kevents the call fills; nothing is changed;
        // `pause` outlives the call when given.
        let got = unsafe {
            libc::kevent(
                self.kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                8,
                pause.as_ref().map_or(std::ptr::null(), std::ptr::from_ref),
            )
        };
        let Ok(got) = usize::try_from(got) else {
            return Woke::Other;
        };
        if super::noted(
            &events[..got.min(8)],
            self.parent,
            libc::NOTE_EXIT | libc::NOTE_EXEC,
        ) {
            Woke::Parent
        } else {
            Woke::Other
        }
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn nap(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// The kernel's [`GroupControl`] where a Linux watcher holds its group by an anchor: on a kernel
/// older than 6.9, which signals no group by pidfd ([`GroupPidfd`]), one whose child's pidfd
/// probe failed, and in a test that asks for it (`super::BY_LISTING_VAR`). Darwin's reads, with a pidfd of each member in place of the kqueue's
/// watch, waited on beside `parent`, the watcher's descriptor of the parent's end
/// (`super::parent_end`). Linux's listing passes every exited process by, a zombie included
/// ([`group_members`](crate::process::group_members)), so each listing of the watched group here
/// adds the anchor, which stays in it until it is reaped: it was forked there, and a zombie moves
/// to no other group. Another exited member is passed by as that listing passes it; it can fork
/// nothing more. A walk of `/proc` is no snapshot: the listing follows the ids allocated behind it
/// (`procfs::chased_group_of`), yet a fork the kernel has numbered but not yet made visible can be
/// missed by one listing whose forker exits before its own row is read. [`live`] proves a group
/// empty only by two whole listings in a row, which such a relay would have to outrun twice.
#[cfg(target_os = "linux")]
pub(super) struct GroupLive<'a> {
    /// The watcher's descriptor of the parent's end, readable once it exited, or execed (from
    /// Linux 6.9, `super::parent_end`).
    pub parent: i32,
    /// The group the watched child led, whose members [`GroupControl::wait`] watches.
    pub pgid: i32,
    /// Borrowed native wait custody, never reconstructed from a listed pid.
    pub anchor: &'a super::Anchor,
}

#[cfg(target_os = "linux")]
impl Listed for GroupLive<'_> {
    fn anchor(&self) -> i32 {
        self.anchor.pid
    }
    fn group_of(&mut self, pid: i32) -> Option<i32> {
        crate::process::read_lineage(pid).and_then(|lineage| i32::try_from(lineage.pgid).ok())
    }
    fn presence(&mut self, pid: i32) -> ProcessPresence {
        crate::process::process_presence(pid)
    }
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)> {
        let listing = crate::process::group_members(u32::try_from(pgid).ok()?)?;
        let mut pids = listing.pids;
        if pgid == self.pgid {
            pids.push(self.anchor.pid);
        }
        Some((pids, listing.complete))
    }
}

#[cfg(target_os = "linux")]
impl GroupControl for GroupLive<'_> {
    fn census(&mut self, pgid: i32) -> Live {
        live(self, pgid)
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
        live_signal(pgid, sig)
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        members_or_parent(self.parent, self.pgid, members, patience)
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn nap(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// [`GroupControl::wait`] on Linux: a pidfd of each of `members` of group `pgid`, waited on
/// beside `parent`, the watcher's descriptor of the parent's end, up to `patience`
/// ([`parent_or_exit`]); [`UNWATCHED_RECHECK`] at most where a member's exit cannot be watched.
#[cfg(target_os = "linux")]
fn members_or_parent(parent: i32, pgid: i32, members: &[i32], patience: Option<Duration>) -> Woke {
    let mut patience = patience;
    let mut exits = Vec::with_capacity(members.len());
    for &pid in members {
        match super::pidfd_open(pid, 0) {
            // The pidfd names the member listed while its pid still reads in the group with the
            // pidfd open: a pid reused since by a process outside it reads otherwise, and the
            // group is read again.
            Ok(exit) => match crate::process::read_lineage(pid)
                .and_then(|lineage| i32::try_from(lineage.pgid).ok())
            {
                Some(group) if group == pgid => exits.push(exit),
                Some(_) => return Woke::Other,
                None => match crate::process::process_presence(pid) {
                    ProcessPresence::Absent | ProcessPresence::Exited => return Woke::Other,
                    ProcessPresence::Present | ProcessPresence::Unknown => {
                        exits.push(exit);
                        patience = Some(UNWATCHED_RECHECK);
                    }
                },
            },
            // A member already gone: read the group again now.
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Woke::Other,
            // One whose exit cannot be watched: read it again after a while.
            Err(_) => patience = Some(UNWATCHED_RECHECK),
        }
    }
    parent_or_exit(parent, &exits, patience)
}

/// Waits up to `patience` (none: for as long as it takes) for `parent`, the watcher's descriptor
/// of the parent's end, or one of `exits`, members' pidfds, to read: [`Woke::Parent`] once the
/// parent's does.
#[cfg(target_os = "linux")]
fn parent_or_exit(parent: i32, exits: &[std::os::fd::OwnedFd], patience: Option<Duration>) -> Woke {
    use std::os::fd::AsRawFd;
    let mut fds: Vec<libc::pollfd> = std::iter::once(parent)
        .chain(exits.iter().map(AsRawFd::as_raw_fd))
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let timeout = patience.map_or(-1, |p| i32::try_from(p.as_millis()).unwrap_or(i32::MAX));
    let count = libc::nfds_t::try_from(fds.len()).unwrap_or(libc::nfds_t::MAX);
    // SAFETY: `fds` is a live array of `count` pollfds, which `poll` reads and writes and does not
    // retain.
    let got = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
    if got > 0 && fds[0].revents != 0 {
        Woke::Parent
    } else {
        Woke::Other
    }
}

/// `PIDFD_SIGNAL_PROCESS_GROUP` (Linux 6.9): `pidfd_send_signal`'s scope of every process whose
/// group is the pid the pidfd names (the kernel's `kill_pgrp_info` over that `struct pid`).
#[cfg(target_os = "linux")]
const PIDFD_SIGNAL_PROCESS_GROUP: u32 = 1 << 2;

/// `sig` to every process whose group is the pid `pidfd` names, the group that process leads or
/// led, `pidfd_send_signal(pidfd, sig, NULL, PIDFD_SIGNAL_PROCESS_GROUP)`; `sig` `0` sends none
/// and answers whether one is there.
///
/// # Errors
/// The kernel's: `ESRCH` once no process, zombie or live, is in that group, `EPERM` when none
/// there may be signalled by this one, `EINVAL` before Linux 6.9 (the scope unknown), `ENOSYS`
/// before Linux 5.1.
#[cfg(target_os = "linux")]
pub(super) fn signal_by_pidfd(pidfd: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: `pidfd_send_signal` takes a descriptor, a signal, a null info pointer (the kernel
    // fills its own) and flags; it touches no memory of ours.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd,
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            PIDFD_SIGNAL_PROCESS_GROUP,
        )
    };
    if sent == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The kernel's [`GroupControl`] on Linux 6.9 and later: the group whose id is the pid of the
/// watched child's pidfd, which the watcher opened while it shared the child's group and the child
/// led it and waited on the watcher's arming, so the descriptor names that child and its group. A
/// group signal goes by the kernel's record of that pid ([`signal_by_pidfd`]), never by the
/// group's number, so no later group that reuses the number can be reached, and the watcher needs
/// no anchor to hold the number: the descriptor keeps the record, a process joins the group by its
/// number only while a member holds it, and none joins once the group emptied.
///
/// The census is signal `0`'s answer first: [`Live::Empty`] at `ESRCH`, no process left in the
/// group. The kernel counts a zombie awaiting its reaping (an exited leader whose parent has not
/// reaped it yet, a member reparented to a pid 1 that never reaps), which can fork nothing, so
/// where a listing here can be whole (`whole`, `process::group_listing_can_be_whole`) the
/// group's listing by its number, which passes zombies by, decides the rest: its live members
/// ([`Live::Members`], whose exits a wait watches), or [`Live::Empty`] after two whole listings in
/// a row with none. A number whose group emptied since, its last zombie reaped, may name a later
/// group by then, whose members only keep the watch: nothing is ever signalled by the number.
/// Where no listing can be whole, [`Live::Unknown`], so an exited member keeps the watch until it
/// is reaped or the parent ends, and an end at the parent's exec, its unreaped zombie child the
/// leader, spends the grace and [`KILL_ROUNDS`] on it.
#[cfg(target_os = "linux")]
pub(super) struct GroupPidfd {
    /// The watcher's descriptor of the parent's end (`super::parent_end`).
    pub parent: i32,
    /// The watched child's pidfd.
    pub leader: i32,
    /// The group's number, the watched child's pid, which its listing and its members' reads
    /// name.
    pub pgid: i32,
    /// Whether a listing of the group here can be whole.
    pub whole: bool,
}

#[cfg(target_os = "linux")]
impl GroupControl for GroupPidfd {
    fn census(&mut self, pgid: i32) -> Live {
        if matches!(signal_by_pidfd(self.leader, 0), Err(e) if e.raw_os_error() == Some(libc::ESRCH))
        {
            return Live::Empty;
        }
        if !self.whole {
            return Live::Unknown;
        }
        let listed = || crate::process::group_members(u32::try_from(pgid).ok()?);
        match listed() {
            Some(first) if !first.pids.is_empty() => Live::Members(first.pids),
            Some(first)
                if first.complete
                    && listed().is_some_and(|again| again.complete && again.pids.is_empty()) =>
            {
                Live::Empty
            }
            _ => Live::Unknown,
        }
    }
    fn signal_group(&mut self, _pgid: i32, sig: i32) -> std::io::Result<()> {
        signal_by_pidfd(self.leader, sig)
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        members_or_parent(self.parent, self.pgid, members, patience)
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn nap(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// How a Linux watcher holds the group its child leads: by the child's pidfd where the kernel
/// signals a group by one ([`GroupPidfd`]), else by its anchor and listings ([`GroupLive`]).
#[cfg(target_os = "linux")]
pub(super) enum Holding<'a> {
    /// By the child's pidfd (Linux 6.9 and later).
    Pidfd(GroupPidfd),
    /// By the watcher's anchor, judged by listings.
    Anchor(GroupLive<'a>),
}

#[cfg(target_os = "linux")]
impl GroupControl for Holding<'_> {
    fn census(&mut self, pgid: i32) -> Live {
        match self {
            Self::Pidfd(held) => held.census(pgid),
            Self::Anchor(held) => held.census(pgid),
        }
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
        match self {
            Self::Pidfd(held) => held.signal_group(pgid, sig),
            Self::Anchor(held) => held.signal_group(pgid, sig),
        }
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        match self {
            Self::Pidfd(held) => held.wait(members, patience),
            Self::Anchor(held) => held.wait(members, patience),
        }
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn nap(&mut self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

#[cfg(test)]
#[path = "tether_group_tests.rs"]
mod tests;
