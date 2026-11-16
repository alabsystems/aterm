// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hang-up bell ([`Bell`], [`Armed`], [`Ring`]): one thread per bell hears an armed stream's
//! peer close entirely and rings that arm's [`Ring`], so a parked wait hears its caller go without
//! polling.

use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

/// Hears each stream armed on it ([`Bell::arm`]) become HUNG UP, as
/// [`fd_peer_closed`](super::fd_peer_closed) judges it: every descriptor of the peer end closed,
/// where a peer's write-side shutdown is not, and rings that arm's [`Ring`] once for it. One
/// `aterm-hangup` thread serves a bell, started by [`Bell::start`] and never by an arm: a start
/// opens the kernel queue through [`crate::kept_out`], which on Apple waits for every spawn in
/// flight, and a park arms under the lock of the state it guards, so the start comes first, with
/// no such lock held. As that thread ends it rings every arm standing then once more, a
/// hung-up one included. A bell not started, whose thread could not start, or has ended, arms
/// nothing, so its callers keep asking after their callers themselves.
/// On macOS the thread waits on a kqueue (`EVFILT_WRITE` with `EV_CLEAR`, acting on `EV_EOF`
/// alone: the filter `poll(POLLOUT)` reads, which reports `POLLHUP` exactly then); on Linux on an
/// epoll instance (interest 0, one-shot: `EPOLLHUP` and `EPOLLERR` alone report, once); elsewhere
/// no bell starts.
pub struct Bell {
    /// The started bell, or `None` where it could not start: decided once, at the first
    /// [`Bell::start`]; unset until then.
    started: OnceLock<Option<Arc<Shared>>>,
}

/// One stream armed on a [`Bell`], borrowed for as long as it is armed, so its descriptor stays
/// open (and keeps its number) until the drop has taken the registration out: a ring for it
/// never reaches a later stream that reuses the number, and no ring runs after the drop returns.
#[must_use = "dropping it disarms the bell"]
pub struct Armed<'a> {
    shared: Arc<Shared>,
    fd: BorrowedFd<'a>,
    id: u64,
}

/// What a bell's thread and its arms share.
struct Shared {
    /// The kernel queue the thread waits on, held here so it stays open while an arm or the
    /// thread can still name it.
    queue: sys::Queue,
    /// Whether the thread still serves this bell: cleared under `arms`' lock as it ends, after
    /// which every arm it held has been rung and no arm succeeds.
    live: AtomicBool,
    /// The standing arms. A STRICT LEAF: nothing else is locked under it, and a ring under it
    /// ([`Ring::ring`]) never blocks.
    arms: Mutex<Arms>,
}

/// The standing arms and the next registration id. The kernel registration carries the id, never
/// the descriptor, so a thread that read an event just before its arm's drop finds no arm.
struct Arms {
    next: u64,
    rings: Vec<Arm>,
}

struct Arm {
    id: u64,
    fd: RawFd,
    ring: Ring,
}

/// What an arm's ring wakes. A ring runs on the bell's thread under its arms' lock, so each kind
/// is a call that never blocks and takes no lock of its caller's.
#[derive(Clone, Debug)]
pub enum Ring {
    /// `try_send(())` on a channel. A single-slot channel keeps a ring its waiter has not yet
    /// read, so a wait that begins after the ring still ends at once: none is lost.
    Slot(SyncSender<()>),
    /// `notify_all` on the condition variable a park waits on with the lock of its state. A
    /// notify finds only the waits already begun: one sent between the park's last look and its
    /// wait is lost, so such a park bounds each wait while armed.
    Notify(Arc<Condvar>),
}

impl Ring {
    /// Wake what this ring names, without blocking.
    fn ring(&self) {
        match self {
            Ring::Slot(slot) => {
                let _ = slot.try_send(());
            }
            Ring::Notify(signal) => signal.notify_all(),
        }
    }
}

impl From<SyncSender<()>> for Ring {
    fn from(slot: SyncSender<()>) -> Self {
        Ring::Slot(slot)
    }
}

/// What one wait of a bell's queue heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heard {
    /// The stream armed under this registration id became HUNG UP.
    HungUp(u64),
    /// The bell's owner asked its thread to end ([`sys::Queue::stop`]).
    Stop,
}

impl Bell {
    /// A bell whose thread starts at its first [`Self::start`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started: OnceLock::new(),
        }
    }

    /// This process's bell, which is never dropped.
    #[must_use]
    pub fn process() -> &'static Self {
        static PROCESS: Bell = Bell::new();
        &PROCESS
    }

    /// Open this bell's kernel queue and start its thread, at the first call; every later call
    /// reads what the first decided. Whether the bell started (its thread may have ended since).
    /// The first call opens the queue through [`crate::kept_out`], which on Apple waits for every
    /// spawn in flight: make it with none of the caller's locks held, before any arm.
    pub fn start(&self) -> bool {
        self.started.get_or_init(start).is_some()
    }

    /// Ring `ring` ([`Ring`]: a channel's `try_send`, which never blocks, a full channel already
    /// holding a wake, or a condition variable's `notify_all`) once when `stream`'s peer is HUNG
    /// UP, now or later, until the returned [`Armed`] drops. A stream already hung up rings at
    /// once. The thread's end rings it too, after which [`Armed::ringing`] reads false. An arm
    /// never starts the bell ([`Self::start`]): it takes only the bell's own leaf lock and makes
    /// one kernel registration, so it may be made under its caller's locks.
    ///
    /// # Errors
    /// The bell was never started, could not start (no kernel queue or thread here, or this
    /// platform has none), or its thread has ended; `stream`'s descriptor is already armed on
    /// this bell; or the kernel refused the registration. The caller then asks after its caller
    /// itself, as without a bell.
    pub fn arm<'a>(&self, stream: &'a impl AsFd, ring: impl Into<Ring>) -> io::Result<Armed<'a>> {
        let shared = match self.started.get() {
            Some(Some(shared)) => shared,
            Some(None) => return Err(io::Error::other("the hang-up bell did not start")),
            None => return Err(io::Error::other("the hang-up bell was not started")),
        };
        let fd = stream.as_fd();
        let raw = fd.as_raw_fd();
        let mut arms = shared.lock_arms();
        if !shared.live.load(Ordering::Acquire) {
            return Err(io::Error::other("the hang-up bell's thread has ended"));
        }
        if arms.rings.iter().any(|arm| arm.fd == raw) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "this descriptor is already armed on the hang-up bell",
            ));
        }
        let id = arms.next;
        arms.next += 1;
        shared.queue.add(raw, id)?;
        arms.rings.push(Arm {
            id,
            fd: raw,
            ring: ring.into(),
        });
        Ok(Armed {
            shared: Arc::clone(shared),
            fd,
            id,
        })
    }

    /// Ask this bell's thread to end, as a failure of its kernel queue would end it. It ends on
    /// its own thread, perhaps after this returns: as it ends, every arm standing then is rung and
    /// reads [`Armed::ringing`] false, and no arm made after succeeds. A test waits for an arm's
    /// ring to know it has ended. A bell that never started is left as it is.
    #[cfg(any(test, debug_assertions))]
    pub fn end_thread_for_test(&self) {
        if let Some(Some(shared)) = self.started.get() {
            shared.queue.stop();
        }
    }

    /// A bell whose thread has ended, so it arms nothing: a test's park on it keeps its lane's
    /// slices. Its end is seen through the ring of an arm standing then, waited for under a
    /// minute's hang detector.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn ended_for_test() -> &'static Self {
        static ENDED: Bell = Bell::new();
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let (ring, rung) = std::sync::mpsc::sync_channel(1);
            if ENDED.start()
                && let Ok((near, _far)) = crate::CtlStream::pair()
                && let Ok(_armed) = ENDED.arm(&near, ring)
            {
                ENDED.end_thread_for_test();
                let _ = rung.recv_timeout(std::time::Duration::from_secs(60));
            }
        });
        &ENDED
    }

    /// Whether descriptor `fd` stands armed on this bell: a test's sign that a park armed the
    /// connection it serves, so a close from then on reaches that park only through the ring.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn armed_for_test(&self, fd: RawFd) -> bool {
        self.started
            .get()
            .and_then(Option::as_ref)
            .is_some_and(|shared| shared.lock_arms().rings.iter().any(|arm| arm.fd == fd))
    }
}

impl Default for Bell {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Bell {
    /// Ends the thread of a bell that started; arms still standing are rung as it ends.
    fn drop(&mut self) {
        if let Some(Some(shared)) = self.started.get() {
            shared.queue.stop();
        }
    }
}

impl Armed<'_> {
    /// Whether the bell that armed this still serves it: false once its thread has ended, which
    /// rang this arm as it ended, so a wait that relied on the ring asks for itself again.
    #[must_use]
    pub fn ringing(&self) -> bool {
        self.shared.live.load(Ordering::Acquire)
    }
}

impl Drop for Armed<'_> {
    /// Takes the registration out of the kernel queue and the arms under the arms' lock, so the
    /// thread, which rings only under that lock, rings nothing for it once this returns.
    fn drop(&mut self) {
        let mut arms = self.shared.lock_arms();
        self.shared.queue.delete(self.fd.as_raw_fd());
        arms.rings.retain(|arm| arm.id != self.id);
    }
}

impl Shared {
    fn lock_arms(&self) -> MutexGuard<'_, Arms> {
        self.arms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ring the arm registered under `id`, if it still stands.
    fn ring(&self, id: u64) {
        let arms = self.lock_arms();
        if let Some(arm) = arms.rings.iter().find(|arm| arm.id == id) {
            arm.ring.ring();
        }
    }
}

/// Open the kernel queue and start the thread; `None` where either fails.
fn start() -> Option<Arc<Shared>> {
    let shared = Arc::new(Shared {
        queue: sys::Queue::open().ok()?,
        live: AtomicBool::new(true),
        arms: Mutex::new(Arms {
            next: 1,
            rings: Vec::new(),
        }),
    });
    let serving = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("aterm-hangup".into())
        .spawn(move || serve(&serving))
        .ok()?;
    Some(shared)
}

/// The bell's thread: ring each arm its queue hears hang up, until a stop or a queue failure.
fn serve(shared: &Shared) {
    let _ended = Ended(shared);
    let mut heard = Vec::new();
    loop {
        heard.clear();
        match shared.queue.wait(&mut heard) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
        for &event in &heard {
            match event {
                Heard::HungUp(id) => shared.ring(id),
                Heard::Stop => return,
            }
        }
    }
}

/// Marks a bell's thread ended however it ends (a stop, a queue failure, a panic): under the
/// arms' lock it clears `live` and rings every standing arm, so each wait relying on one wakes and
/// finds the bell gone, and no arm made after it succeeds.
struct Ended<'a>(&'a Shared);

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        let arms = self.0.lock_arms();
        self.0.live.store(false, Ordering::Release);
        for arm in &arms.rings {
            arm.ring.ring();
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use super::Heard;
    use crate::exitwatch::kq;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};

    /// The `EVFILT_USER` ident whose trigger ends the thread (its own ident space, apart from the
    /// descriptors `EVFILT_WRITE` names).
    const STOP: usize = 0;

    /// How many events one wait reads.
    const BATCH: usize = 16;

    /// A kqueue holding the stop event and one `EVFILT_WRITE` per arm.
    pub(super) struct Queue {
        kq: OwnedFd,
    }

    fn change(ident: usize, filter: i16, flags: u16, fflags: u32, udata: u64) -> kq::Kevent {
        kq::Kevent {
            ident,
            filter,
            flags,
            fflags,
            data: 0,
            udata: udata as usize as *mut core::ffi::c_void,
        }
    }

    impl Queue {
        /// A close-on-exec kqueue kept out of children, with its stop event registered.
        #[cfg_attr(trust_verify, trust::skip)]
        pub(super) fn open() -> io::Result<Self> {
            let kq = crate::kept_out(|| -> io::Result<OwnedFd> {
                // SAFETY: kqueue creates a fresh descriptor or returns -1.
                let fd = unsafe { kq::kqueue() };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the descriptor is fresh and has one owner here.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                // SAFETY: mark this live descriptor close-on-exec, inside spawn serialization.
                if unsafe { kq::fcntl(fd.as_raw_fd(), kq::F_SETFD, kq::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(fd)
            })?;
            let queue = Self { kq };
            queue.apply(&change(
                STOP,
                libc::EVFILT_USER,
                kq::EV_ADD | libc::EV_CLEAR,
                0,
                0,
            ))?;
            Ok(queue)
        }

        /// Register `fd`'s write filter under `id`. `EV_CLEAR` resets the event as it is read,
        /// so a hung-up descriptor is reported once, not at every wait.
        pub(super) fn add(&self, fd: RawFd, id: u64) -> io::Result<()> {
            let ident =
                usize::try_from(fd).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            self.apply(&change(
                ident,
                libc::EVFILT_WRITE,
                kq::EV_ADD | libc::EV_CLEAR,
                0,
                id,
            ))
        }

        /// Take `fd`'s write filter out. A descriptor never registered is no error worth a caller.
        pub(super) fn delete(&self, fd: RawFd) {
            if let Ok(ident) = usize::try_from(fd) {
                let _ = self.apply(&change(ident, libc::EVFILT_WRITE, libc::EV_DELETE, 0, 0));
            }
        }

        /// Trigger the stop event.
        pub(super) fn stop(&self) {
            let _ = self.apply(&change(STOP, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER, 0));
        }

        /// Wait for events and add what each means to `heard`: a write filter's `EV_EOF` is a
        /// hang-up; its plain writability is nothing.
        ///
        /// # Errors
        /// `kevent(2)`'s errno, `EINTR` included.
        #[cfg_attr(trust_verify, trust::skip)]
        pub(super) fn wait(&self, heard: &mut Vec<Heard>) -> io::Result<()> {
            let mut events: [kq::Kevent; BATCH] = std::array::from_fn(|_| change(0, 0, 0, 0, 0));
            // SAFETY: no changes; `events` is a live buffer of BATCH entries for the call, and a
            // null timeout waits until an event comes.
            let n = unsafe {
                kq::kevent(
                    self.kq.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    BATCH as core::ffi::c_int,
                    std::ptr::null(),
                )
            };
            let n = usize::try_from(n).map_err(|_| io::Error::last_os_error())?;
            for event in &events[..n.min(BATCH)] {
                if event.filter == libc::EVFILT_USER && event.ident == STOP {
                    heard.push(Heard::Stop);
                } else if event.filter == libc::EVFILT_WRITE && event.flags & libc::EV_EOF != 0 {
                    heard.push(Heard::HungUp(event.udata as usize as u64));
                }
            }
            Ok(())
        }

        /// Apply one change, with no event list: a refused change is `kevent`'s `-1` and errno.
        #[cfg_attr(trust_verify, trust::skip)]
        fn apply(&self, change: &kq::Kevent) -> io::Result<()> {
            // SAFETY: one live change entry for the call; no event list is read.
            let rc = unsafe {
                kq::kevent(
                    self.kq.as_raw_fd(),
                    change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::Heard;
    use core::ffi::c_int;
    use std::io::{self, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};

    /// `struct epoll_event`: packed on x86-64 alone, as the kernel's UAPI header declares it.
    #[cfg_attr(target_arch = "x86_64", repr(C, packed))]
    #[cfg_attr(not(target_arch = "x86_64"), repr(C))]
    #[derive(Clone, Copy)]
    struct EpollEvent {
        events: u32,
        data: u64,
    }

    const EPOLL_CLOEXEC: c_int = 0o2_000_000;
    const EPOLL_CTL_ADD: c_int = 1;
    const EPOLL_CTL_DEL: c_int = 2;
    const EPOLLIN: u32 = 0x1;
    const EPOLLERR: u32 = 0x8;
    const EPOLLHUP: u32 = 0x10;
    const EPOLLONESHOT: u32 = 1 << 30;

    unsafe extern "C" {
        fn epoll_create1(flags: c_int) -> c_int;
        fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut EpollEvent) -> c_int;
        fn epoll_wait(
            epfd: c_int,
            events: *mut EpollEvent,
            maxevents: c_int,
            timeout: c_int,
        ) -> c_int;
    }

    /// The registration data of the stop eventfd; arms' ids start at 1.
    const STOP: u64 = 0;

    /// How many events one wait reads.
    const BATCH: usize = 16;

    /// An epoll instance holding the stop eventfd and one registration per arm.
    pub(super) struct Queue {
        epoll: OwnedFd,
        stop: std::fs::File,
    }

    impl Queue {
        /// A close-on-exec epoll instance and stop eventfd kept out of children, the eventfd
        /// registered for reading.
        #[cfg_attr(trust_verify, trust::skip)]
        pub(super) fn open() -> io::Result<Self> {
            let epoll = crate::kept_out(|| -> io::Result<OwnedFd> {
                // SAFETY: epoll_create1 creates a fresh descriptor or returns -1.
                let fd = unsafe { epoll_create1(EPOLL_CLOEXEC) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the descriptor is fresh and has one owner here.
                Ok(unsafe { OwnedFd::from_raw_fd(fd) })
            })?;
            let stop = crate::kept_out(|| -> io::Result<std::fs::File> {
                // SAFETY: eventfd creates a fresh descriptor or returns -1.
                let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: the descriptor is fresh and has one owner here.
                Ok(std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
            })?;
            let queue = Self { epoll, stop };
            queue.ctl(EPOLL_CTL_ADD, queue.stop.as_raw_fd(), EPOLLIN, STOP)?;
            Ok(queue)
        }

        /// Register `fd` under `id` with interest 0, one-shot: the kernel adds `EPOLLHUP` and
        /// `EPOLLERR` to every registration, so they alone report, and once (a level-triggered
        /// hang-up would report at every wait until the arm drops).
        pub(super) fn add(&self, fd: RawFd, id: u64) -> io::Result<()> {
            self.ctl(EPOLL_CTL_ADD, fd, EPOLLONESHOT, id)
        }

        /// Take `fd`'s registration out. A descriptor never registered is no error worth a caller.
        pub(super) fn delete(&self, fd: RawFd) {
            let _ = self.ctl(EPOLL_CTL_DEL, fd, 0, 0);
        }

        /// Make the stop eventfd readable.
        pub(super) fn stop(&self) {
            let _ = (&self.stop).write(&1_u64.to_ne_bytes());
        }

        /// Wait for events and add what each means to `heard`.
        ///
        /// # Errors
        /// `epoll_wait(2)`'s errno, `EINTR` included.
        #[cfg_attr(trust_verify, trust::skip)]
        pub(super) fn wait(&self, heard: &mut Vec<Heard>) -> io::Result<()> {
            let mut events = [EpollEvent { events: 0, data: 0 }; BATCH];
            // SAFETY: `events` is a live buffer of BATCH entries for the call; -1 waits until an
            // event comes.
            let n = unsafe {
                epoll_wait(
                    self.epoll.as_raw_fd(),
                    events.as_mut_ptr(),
                    BATCH as c_int,
                    -1,
                )
            };
            let n = usize::try_from(n).map_err(|_| io::Error::last_os_error())?;
            for event in &events[..n.min(BATCH)] {
                let (bits, data) = (event.events, event.data);
                if data == STOP {
                    heard.push(Heard::Stop);
                } else if bits & (EPOLLHUP | EPOLLERR) != 0 {
                    heard.push(Heard::HungUp(data));
                }
            }
            Ok(())
        }

        #[cfg_attr(trust_verify, trust::skip)]
        fn ctl(&self, op: c_int, fd: RawFd, events: u32, data: u64) -> io::Result<()> {
            let mut event = EpollEvent { events, data };
            // SAFETY: `event` is live for the call (the kernel ignores it for EPOLL_CTL_DEL).
            if unsafe { epoll_ctl(self.epoll.as_raw_fd(), op, fd, &mut event) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use super::Heard;
    use std::io;
    use std::os::fd::RawFd;

    /// No kernel queue here: a bell never starts.
    pub(super) enum Queue {}

    impl Queue {
        pub(super) fn open() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no hang-up bell on this platform",
            ))
        }

        pub(super) fn add(&self, _fd: RawFd, _id: u64) -> io::Result<()> {
            match *self {}
        }

        pub(super) fn delete(&self, _fd: RawFd) {
            match *self {}
        }

        pub(super) fn stop(&self) {
            match *self {}
        }

        pub(super) fn wait(&self, _heard: &mut Vec<Heard>) -> io::Result<()> {
            match *self {}
        }
    }
}

#[cfg(test)]
#[path = "hangup_bell_tests.rs"]
mod tests;
