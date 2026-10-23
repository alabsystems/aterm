// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session `exec` process watcher ([`ProcWatch::new`](crate::procwatch::ProcWatch::new), [`ProcWatch::open`](crate::procwatch::ProcWatch::open),
//! [`ProcWatch::fd`](crate::procwatch::ProcWatch::fd), [`ProcWatch::drain`](crate::procwatch::ProcWatch::drain), [`ProcWatch::follow`](crate::procwatch::ProcWatch::follow),
//! [`ProcWatch::scripted`](crate::procwatch::ProcWatch::scripted), [`ProcScript`](crate::procwatch::ProcScript), [`ProcEvent::Exec`](crate::procwatch::ProcEvent::Exec), [`MAX_CHILDREN`](crate::procwatch::MAX_CHILDREN),
//! [`NoWatch`](crate::procwatch::NoWatch)).

#[cfg(target_vendor = "apple")]
use crate::exitwatch::kq;

/// How many of the shell's children one watch registers.
pub const MAX_CHILDREN: usize = 16;

/// What the kernel reported, in the order the reader should hear it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcEvent {
    /// The shell forked `child`, now watched for its `exec` and its exit.
    Fork {
        /// The new child.
        child: i32,
    },
    /// `pid` replaced its image.
    Exec {
        /// The process that `exec`ed.
        pid: i32,
        /// Its process group, read as the event was drained (`0`: already gone or unreadable).
        pgid: i32,
        /// The `exec` may already be known.
        late: bool,
    },
    /// A watched process ended (its knote went with it).
    Exit {
        /// The process that exited.
        pid: i32,
    },
}

/// Why [`ProcWatch::open`] made no watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoWatch {
    /// No exec event on this platform (everything but Darwin).
    Unsupported,
    /// The shell is no process anyone watches (`<= 1`), or it is already gone.
    NoShell,
    /// No kqueue could be made (`EMFILE`, `ENFILE`, `ENOMEM`).
    NoQueue,
}

/// One process under watch.
#[cfg(any(target_vendor = "apple", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Child {
    pid: i32,
    /// Reported `late`: its next kernel `exec` may be the same one.
    late: bool,
}

/// The kernel calls the watch's bookkeeping makes ([`Tracked`]).
#[cfg(any(target_vendor = "apple", test))]
trait Kernel {
    /// Register `pids` for `NOTE_EXEC|NOTE_EXIT`, one call.
    fn register(&mut self, pids: &[i32]) -> Vec<bool>;
    /// Drop `pid`'s knote.
    fn delete(&mut self, pid: i32);
    /// What a process is now ([`short_info`]).
    fn info(&mut self, pid: i32) -> Option<ProcShort>;
}

/// The watch's bookkeeping ([`Kernel`], [`MAX_CHILDREN`]).
#[cfg(any(target_vendor = "apple", test))]
#[derive(Debug, Default)]
struct Tracked {
    children: Vec<Child>,
    followed: Option<Child>,
    /// Children the cap left unwatched, still alive at the last fork (so each counts once).
    unwatched: Vec<i32>,
    overflow: u64,
}

#[cfg(any(target_vendor = "apple", test))]
impl Tracked {
    /// Track the existing child `pid` (a new watch's, with no event for its past).
    fn adopt(&mut self, pid: i32, kernel: &mut impl Kernel) {
        if self.children.len() >= MAX_CHILDREN {
            self.overflow = self.overflow.saturating_add(1);
            self.unwatched.push(pid);
            return;
        }
        if kernel.register(&[pid]) == [true] {
            let late = kernel.info(pid).is_some_and(|i| i.execed);
            self.children.push(Child { pid, late });
        }
    }

    /// The shell forked and has the children `now`.
    fn forked(&mut self, now: &[i32], kernel: &mut impl Kernel, out: &mut Vec<ProcEvent>) {
        self.unwatched.retain(|pid| now.contains(pid));
        let followed = self.followed.map(|f| f.pid);
        let fresh: Vec<i32> = now
            .iter()
            .copied()
            .filter(|pid| Some(*pid) != followed)
            .filter(|pid| !self.unwatched.contains(pid))
            .filter(|pid| !self.children.iter().any(|c| c.pid == *pid))
            .collect();
        let room = MAX_CHILDREN
            .saturating_sub(self.children.len())
            .min(fresh.len());
        self.overflow = self.overflow.saturating_add((fresh.len() - room) as u64);
        self.unwatched.extend_from_slice(&fresh[room..]);
        let fresh = &fresh[..room];
        if fresh.is_empty() {
            return;
        }
        let watched = kernel.register(fresh);
        for (pid, watched) in fresh.iter().zip(watched) {
            if !watched {
                continue; // already gone: nothing of it can reach the terminal any more
            }
            out.push(ProcEvent::Fork { child: *pid });
            let info = kernel.info(*pid);
            let late = info.is_some_and(|i| i.execed);
            self.children.push(Child { pid: *pid, late });
            if late {
                out.push(ProcEvent::Exec {
                    pid: *pid,
                    pgid: info.map_or(0, |i| i.pgid),
                    late: true,
                });
            }
        }
    }

    /// Watch `leader`, the terminal's holder, unless something already does.
    fn follow(&mut self, leader: i32, shell: i32, kernel: &mut impl Kernel) {
        if leader <= 1
            || leader == shell
            || self.followed.is_some_and(|f| f.pid == leader)
            || self.children.iter().any(|c| c.pid == leader)
        {
            return;
        }
        self.unfollow(kernel);
        if kernel.register(&[leader]) == [true] {
            let late = kernel.info(leader).is_some_and(|i| i.execed);
            self.followed = Some(Child { pid: leader, late });
        }
    }

    fn unfollow(&mut self, kernel: &mut impl Kernel) {
        if let Some(followed) = self.followed.take() {
            kernel.delete(followed.pid);
        }
    }

    /// Whether `pid` was reported late (clearing it): its next kernel `exec` may be that one.
    fn take_late(&mut self, pid: i32) -> bool {
        self.children
            .iter_mut()
            .chain(self.followed.as_mut())
            .find(|c| c.pid == pid)
            .is_some_and(|c| std::mem::take(&mut c.late))
    }

    /// `pid` exited: its knote went with it.
    fn exited(&mut self, pid: i32) {
        self.children.retain(|c| c.pid != pid);
        if self.followed.is_some_and(|f| f.pid == pid) {
            self.followed = None;
        }
    }
}

/// One session's process watch.
#[derive(Debug)]
pub struct ProcWatch {
    /// The kqueue, or a scripted watch's pipe read end.
    fd: std::os::fd::OwnedFd,
    shell: i32,
    #[cfg(target_vendor = "apple")]
    tracked: Tracked,
    #[cfg(any(test, debug_assertions))]
    script: Option<std::sync::Arc<Script>>,
}

/// The events a scripted watch hands out and what its reader asked of it.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
struct Script {
    queue: std::sync::Mutex<std::collections::VecDeque<ProcEvent>>,
    follows: std::sync::Mutex<Vec<Option<i32>>>,
    drains: std::sync::atomic::AtomicUsize,
}

/// The test side of a [`ProcWatch::scripted`] watch.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Clone)]
pub struct ProcScript {
    script: std::sync::Arc<Script>,
    wake: std::sync::Arc<std::io::PipeWriter>,
}

#[cfg(any(test, debug_assertions))]
impl ProcScript {
    /// Queue `events` and make the watch's descriptor readable.
    pub fn post(&self, events: &[ProcEvent]) {
        use std::io::Write as _;
        self.script
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(events.iter().copied());
        let _ = (&*self.wake).write(&[1]);
    }

    /// Every [`ProcWatch::follow`] (`Some`) and [`ProcWatch::unfollow`] (`None`) so far.
    #[must_use]
    pub fn follows(&self) -> Vec<Option<i32>> {
        self.script
            .follows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many times the reader drained the watch.
    #[must_use]
    pub fn drains(&self) -> usize {
        self.script.drains.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Events posted and not yet drained.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.script
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// The kqueue as the bookkeeping's [`Kernel`].
#[cfg(target_vendor = "apple")]
struct Kqueue<'a>(&'a std::os::fd::OwnedFd);

#[cfg(target_vendor = "apple")]
impl Kernel for Kqueue<'_> {
    fn register(&mut self, pids: &[i32]) -> Vec<bool> {
        register(self.0, pids, libc::NOTE_EXEC | kq::NOTE_EXIT)
    }

    fn delete(&mut self, pid: i32) {
        delete(self.0, pid);
    }

    fn info(&mut self, pid: i32) -> Option<ProcShort> {
        short_info(pid)
    }
}

impl ProcWatch {
    /// Watch `shell_pid` and its current children.
    #[must_use]
    pub fn new(shell_pid: i32) -> Option<Self> {
        Self::open(shell_pid).ok()
    }

    /// [`ProcWatch::new`], saying why there is no watch.
    #[cfg(target_vendor = "apple")]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn open(shell_pid: i32) -> Result<Self, NoWatch> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        if shell_pid <= 1 {
            return Err(NoWatch::NoShell);
        }
        // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
        let raw = unsafe { kq::kqueue() };
        if raw < 0 {
            return Err(NoWatch::NoQueue);
        }
        // SAFETY: `raw` is a fresh descriptor this process exclusively owns.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
        // SAFETY: Set close-on-exec on the kqueue descriptor. F_SETFD takes an int and touches
        // only this descriptor's flags.
        unsafe { kq::fcntl(fd.as_raw_fd(), kq::F_SETFD, kq::FD_CLOEXEC) };
        let shell_notes = libc::NOTE_FORK | libc::NOTE_EXEC | kq::NOTE_EXIT;
        if register(&fd, &[shell_pid], shell_notes) != [true] {
            return Err(NoWatch::NoShell);
        }
        let mut tracked = Tracked::default();
        for pid in crate::process::child_pids(shell_pid).unwrap_or_default() {
            tracked.adopt(pid, &mut Kqueue(&fd));
        }
        Ok(Self {
            fd,
            shell: shell_pid,
            tracked,
            #[cfg(any(test, debug_assertions))]
            script: None,
        })
    }

    /// See the Darwin twin: no exec event here.
    #[cfg(not(target_vendor = "apple"))]
    pub fn open(_shell_pid: i32) -> Result<Self, NoWatch> {
        Err(NoWatch::Unsupported)
    }

    /// A watch that reports only what its [`ProcScript`] posts, for a reader under test.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn scripted(shell_pid: i32) -> Option<(Self, ProcScript)> {
        use std::os::fd::AsRawFd as _;
        let (rd, wr) = std::io::pipe().ok()?;
        let fd = std::os::fd::OwnedFd::from(rd);
        // SAFETY: F_GETFL/F_SETFL on a descriptor this process owns.
        let nonblocking = unsafe {
            let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
        };
        if !nonblocking {
            return None;
        }
        let script = std::sync::Arc::new(Script::default());
        Some((
            Self {
                fd,
                shell: shell_pid,
                #[cfg(target_vendor = "apple")]
                tracked: Tracked::default(),
                script: Some(std::sync::Arc::clone(&script)),
            },
            ProcScript {
                script,
                wake: std::sync::Arc::new(wr),
            },
        ))
    }

    /// The descriptor `poll(2)` reports readable once an event is queued.
    #[must_use]
    pub fn fd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.fd.as_raw_fd()
    }

    /// Return the watched shell's PID.
    #[must_use]
    pub const fn shell(&self) -> i32 {
        self.shell
    }

    /// Children the cap left unwatched, so far (each counted once).
    #[cfg(target_vendor = "apple")]
    #[must_use]
    pub const fn overflow(&self) -> u64 {
        self.tracked.overflow
    }

    /// See the Darwin twin: nothing is watched here.
    #[cfg(not(target_vendor = "apple"))]
    #[must_use]
    pub const fn overflow(&self) -> u64 {
        0
    }

    /// Take every queued event into `out`, oldest first ([`ProcEvent::Fork`], [`ProcEvent::Exec`]).
    pub fn drain(&mut self, out: &mut Vec<ProcEvent>) {
        #[cfg(any(test, debug_assertions))]
        if let Some(script) = &self.script {
            let mut sink = [0u8; 64];
            // SAFETY: Reading into `sink`'s 64 bytes from this watch's own non-blocking pipe.
            while unsafe { libc::read(self.fd(), sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
            script
                .drains
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            out.extend(
                script
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .drain(..),
            );
            return;
        }
        #[cfg(target_vendor = "apple")]
        self.drain_kernel(out);
        #[cfg(not(target_vendor = "apple"))]
        let _ = out;
    }

    /// Watch `NOTE_EXEC|NOTE_EXIT` on the terminal's foreground `leader` if not already watched.
    pub fn follow(&mut self, leader: i32) {
        #[cfg(any(test, debug_assertions))]
        if let Some(script) = &self.script {
            script
                .follows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Some(leader));
            return;
        }
        #[cfg(target_vendor = "apple")]
        self.tracked
            .follow(leader, self.shell, &mut Kqueue(&self.fd));
        #[cfg(not(target_vendor = "apple"))]
        let _ = leader;
    }

    /// Drop the followed holder's knote, if any.
    pub fn unfollow(&mut self) {
        #[cfg(any(test, debug_assertions))]
        if let Some(script) = &self.script {
            script
                .follows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(None);
            #[cfg(target_vendor = "apple")]
            return;
        }
        #[cfg(target_vendor = "apple")]
        self.tracked.unfollow(&mut Kqueue(&self.fd));
    }

    #[cfg(target_vendor = "apple")]
    #[cfg_attr(trust_verify, trust::skip)]
    fn drain_kernel(&mut self, out: &mut Vec<ProcEvent>) {
        use std::os::fd::AsRawFd as _;
        const BATCH: usize = 16;
        let zero = kq::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let mut events: [kq::Kevent; BATCH] = std::array::from_fn(|_| empty_event());
            // SAFETY: BATCH event slots and one timespec, live for the call; no changes.
            let n = unsafe {
                kq::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    BATCH as i32,
                    &zero,
                )
            };
            let Ok(n) = usize::try_from(n) else {
                return; // EINTR or a broken queue: the next poll says whether to come back
            };
            for event in &events[..n.min(BATCH)] {
                if event.filter != kq::EVFILT_PROC || event.flags & kq::EV_ERROR != 0 {
                    continue;
                }
                let Ok(pid) = i32::try_from(event.ident) else {
                    continue;
                };
                if event.fflags & libc::NOTE_EXEC != 0 {
                    let late = self.tracked.take_late(pid);
                    out.push(ProcEvent::Exec {
                        pid,
                        pgid: short_info(pid).map_or(0, |i| i.pgid),
                        late,
                    });
                }
                if pid == self.shell
                    && event.fflags & libc::NOTE_FORK != 0
                    && let Some(now) = crate::process::child_pids(self.shell)
                {
                    self.tracked.forked(&now, &mut Kqueue(&self.fd), out);
                }
                if event.fflags & kq::NOTE_EXIT != 0 {
                    self.tracked.exited(pid);
                    out.push(ProcEvent::Exit { pid });
                }
            }
            if n < BATCH {
                return;
            }
        }
    }
}

/// Register `pids` with `fflags`, `EV_CLEAR`, one call, each answering in its own receipt.
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn register(fd: &std::os::fd::OwnedFd, pids: &[i32], fflags: u32) -> Vec<bool> {
    use std::os::fd::AsRawFd as _;
    let changes: Vec<kq::Kevent> = pids
        .iter()
        .map(|pid| kq::Kevent {
            ident: *pid as usize,
            filter: kq::EVFILT_PROC,
            flags: kq::EV_ADD | libc::EV_CLEAR | libc::EV_RECEIPT,
            fflags,
            data: 0,
            udata: std::ptr::null_mut(),
        })
        .collect();
    let mut receipts: Vec<kq::Kevent> = pids.iter().map(|_| empty_event()).collect();
    let Ok(n) = i32::try_from(changes.len()) else {
        return vec![false; pids.len()];
    };
    let zero = kq::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `n` change entries and `n` receipt slots, live for the call; EV_RECEIPT
    // answers every change in its own slot (never a pending event: the slots are full).
    let got = unsafe {
        kq::kevent(
            fd.as_raw_fd(),
            changes.as_ptr(),
            n,
            receipts.as_mut_ptr(),
            n,
            &zero,
        )
    };
    let got = usize::try_from(got).unwrap_or(0).min(receipts.len());
    pids.iter()
        .map(|pid| {
            receipts[..got]
                .iter()
                .any(|r| r.ident == *pid as usize && r.flags & kq::EV_ERROR != 0 && r.data == 0)
        })
        .collect()
}

/// Remove `pid`'s knote (a gone process's went with it; that error is the answer).
#[cfg(target_vendor = "apple")]
#[cfg_attr(trust_verify, trust::skip)]
fn delete(fd: &std::os::fd::OwnedFd, pid: i32) {
    use std::os::fd::AsRawFd as _;
    let change = kq::Kevent {
        ident: pid as usize,
        filter: kq::EVFILT_PROC,
        flags: libc::EV_DELETE,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: One change entry, live for the call; no event list requested.
    unsafe {
        kq::kevent(
            fd.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
}

/// An all-zero kevent slot.
#[cfg(target_vendor = "apple")]
const fn empty_event() -> kq::Kevent {
    kq::Kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

/// What a watch reads of a process at its fork and its `exec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcShort {
    /// Its process group.
    pub pgid: i32,
    /// It has replaced the image it was forked with (`PROC_FLAG_EXEC`).
    pub execed: bool,
}

/// Read `pid`'s process group and `exec` flag via [`crate::process::read_short_info`].
#[cfg(target_vendor = "apple")]
#[must_use]
pub fn short_info(pid: i32) -> Option<ProcShort> {
    use crate::process::BsdShortInfo;
    let info = crate::process::read_short_info(pid)?;
    if info.pbsi_status == BsdShortInfo::SZOMB {
        return None;
    }
    Some(ProcShort {
        pgid: i32::try_from(info.pbsi_pgid).ok()?,
        execed: info.pbsi_flags & BsdShortInfo::PROC_FLAG_EXEC != 0,
    })
}

/// See the Darwin twin: not read here.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn short_info(_pid: i32) -> Option<ProcShort> {
    None
}

#[cfg(test)]
#[path = "procwatch_tests.rs"]
mod tests;
