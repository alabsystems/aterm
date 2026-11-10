// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Darwin watcher's end of the process group its child leads ([`end_group`]), and its watch on
//! that group once the child exits first ([`linger`]).
//!
//! A tethered child spawned at the head of its own group (`process_group(0)`, or a session) is a
//! pipeline's root: Git's `fetch` runs `remote-http`, which runs `fetch-pack`, which runs
//! `index-pack`, all in that group. Ending the root alone leaves the rest adopted by launchd and
//! running on (Git ends none of them on a `SIGTERM`), so the watcher of a group leader signals the
//! group, never a single member of it.
//!
//! The group is the child's for as long as the watcher lives, by its anchor: before it leaves the
//! child's group, the watcher forks a child of its own that exits at once and that it never reaps,
//! so the group always holds that zombie. XNU keeps a group while any member, a zombie included,
//! is in it, and allocates no pid that names a live group, so the group id names the child's group
//! and no other until the watcher exits and launchd reaps the anchor
//! (`kill(-pgid, 0)` answers `EPERM` while only the anchor is left, `ESRCH` once it is reaped).
//! Every member a group signal reaches is therefore in the child's group: a member the watcher
//! never saw, the leader's death before the watcher's first look, or a member forked unsignalled
//! as a signal landed whose parent then died changes nothing. The watcher signals only before it
//! exits, so no reuse of the id can come between a read and a signal.

use std::time::{Duration, Instant};

/// How long [`end_group`] keeps sending `SIGKILL` rounds to a group that has not emptied: a member
/// a `SIGKILL` cannot end (one blocked in the kernel) is left to the kernel after this. XNU's fork
/// lets a child born as a group signal lands join the group unsignalled, so one round is not
/// enough there (`process::kill_group_until_empty`).
pub(super) const KILL_ROUNDS: Duration = Duration::from_secs(5);

/// The longest pause between two of [`end_group`]'s reads or rounds.
const PAUSE: Duration = Duration::from_millis(20);

/// How long [`linger`] waits before it reads the group again when no exit can wake it: the
/// listing or a member did not read, or a member's exit could not be watched.
pub(super) const UNWATCHED_RECHECK: Duration = Duration::from_secs(1);

/// What woke [`GroupControl::wait`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Woke {
    /// The parent exited or execed in its own place.
    Parent,
    /// Anything else: a member's exit, a member already gone, or the patience run out.
    Other,
}

/// One listed process's state, keeping an unavailable read distinct from an ended process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Member {
    /// A live process whose current group was read.
    Group(i32),
    /// The process is absent or has exited.
    Gone,
    /// Its live group or ended state could not be established.
    Unread,
}

/// What [`end_group`] and [`linger`] read and do: the kernel, or a test's script.
pub(super) trait GroupControl {
    /// `pid`'s known live group, proven end, or unavailable reading.
    fn member(&mut self, pid: i32) -> Member;
    /// The pids of group `pgid`'s members, and whether the listing is whole; `None` when unread.
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)>;
    /// `sig` to every member of group `pgid` (`kill(-pgid, sig)`).
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()>;
    /// Waits for the parent's exit or exec or for one of `members` to exit, at most `patience`
    /// when it is given.
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke;
    /// The clock.
    fn now(&mut self) -> Instant;
    /// Sleeps `pause`.
    fn nap(&mut self, pause: Duration);
}

/// What [`end_group`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct GroupEnded {
    /// A `SIGTERM` went to the group.
    pub termed: bool,
    /// A `SIGKILL` went to the group (a member outlived the grace).
    pub killed: bool,
    /// A whole listing found no live member left.
    pub emptied: bool,
}

/// The live members of a group as one read finds them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Live {
    /// A whole listing proved no live member (the anchor is known exited).
    Empty,
    /// These read live in the group (a listing that was not whole may hold more).
    Members(Vec<i32>),
    /// No live member read, but the listing or an individual member remained unknown.
    Unknown,
}

/// Lists group `pgid` and reads each member's group: the members still live in it.
fn live(control: &mut impl GroupControl, pgid: i32) -> Live {
    let Some((pids, whole)) = control.listing(pgid) else {
        return Live::Unknown;
    };
    let mut members = Vec::new();
    let mut unread = !whole;
    for pid in pids {
        match control.member(pid) {
            Member::Group(group) if group == pgid => members.push(pid),
            Member::Group(_) | Member::Gone => {}
            Member::Unread => unread = true,
        }
    }
    if !members.is_empty() {
        Live::Members(members)
    } else if unread {
        Live::Unknown
    } else {
        Live::Empty
    }
}

/// Ends group `pgid`, the one the watched child led when its watcher armed, whose anchor the
/// caller holds: a `SIGTERM` to the group, then, for `grace`, a wait for it to empty, then
/// `SIGKILL` rounds for up to [`KILL_ROUNDS`] until it has. A group already empty gets nothing.
/// The caller has seen the child's thread run, or the child exit.
pub(super) fn end_group(pgid: i32, grace: Duration, control: &mut impl GroupControl) -> GroupEnded {
    const SIGTERM: i32 = 15;
    const SIGKILL: i32 = 9;
    let mut ended = GroupEnded::default();
    if live(control, pgid) == Live::Empty {
        ended.emptied = true;
        return ended;
    }
    // A refusal (`EPERM`: nothing the signal could reach) leaves the reads to judge the group.
    ended.termed = control.signal_group(pgid, SIGTERM).is_ok();
    let until = crate::deadline::saturating_after(control.now(), grace);
    let mut pause = Duration::from_millis(1);
    loop {
        if live(control, pgid) == Live::Empty {
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
        if live(control, pgid) == Live::Empty {
            ended.emptied = true;
            return ended;
        }
        if control.now() >= until {
            return ended;
        }
    }
}

/// The watch on group `pgid` once the child that led it exited first, with the parent still
/// there: `false` once a whole listing finds no live member (the group has nothing left to end),
/// `true` once the parent exits or execs first, when the caller ends the group ([`end_group`]).
/// Each read follows a wake: a listed member's exit, or [`UNWATCHED_RECHECK`] when the listing
/// or a member did not read. A member forked after a listing is in the next one, since its
/// parent, a listed member, has not exited while that one waits, or has and woke it.
pub(super) fn linger(pgid: i32, control: &mut impl GroupControl) -> bool {
    loop {
        let woke = match live(control, pgid) {
            Live::Empty => return false,
            Live::Members(members) => control.wait(&members, None),
            Live::Unknown => control.wait(&[], Some(UNWATCHED_RECHECK)),
        };
        if woke == Woke::Parent {
            return true;
        }
    }
}

/// The kernel's [`GroupControl`]: [`read_lineage`](crate::process::read_lineage),
/// [`group_members`](crate::process::group_members), `kill(2)` of the negated group id, the
/// watcher's kqueue (`kq`, which watches `parent`'s exit and exec) and the thread's sleep.
#[cfg(target_vendor = "apple")]
pub(super) struct GroupLive {
    /// The watcher's kqueue.
    pub kq: i32,
    /// The parent the watcher ties the child to.
    pub parent: i32,
}

#[cfg(target_vendor = "apple")]
impl GroupControl for GroupLive {
    fn member(&mut self, pid: i32) -> Member {
        use crate::process::{ProcessPresence, process_presence, read_lineage};
        if let Some(lineage) = read_lineage(pid) {
            return i32::try_from(lineage.pgid).map_or(Member::Unread, Member::Group);
        }
        match process_presence(pid) {
            ProcessPresence::Absent | ProcessPresence::Exited => Member::Gone,
            ProcessPresence::Present | ProcessPresence::Unknown => Member::Unread,
        }
    }
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)> {
        crate::process::group_members(u32::try_from(pgid).ok()?)
            .map(|listing| (listing.pids, listing.complete))
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
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

#[cfg(test)]
#[path = "tether_group_tests.rs"]
mod tests;
