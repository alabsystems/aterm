// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-wide lock between spawning a child and creating a descriptor kept out of
//! children ([`crate::kept_out`]), Go's `syscall.ForkLock` held the other way round.
//!
//! Apple has no atomic `FD_CLOFORK` at `socket(2)`, `socketpair(2)`, `accept(2)` or `dup(2)`:
//! a descriptor is created first and marked after ([`crate::keep_out_of_children`]). A child
//! spawned by another thread between the two takes a copy, holds it until its `exec` closes it,
//! and a same-user descriptor scan of that child drains the socket for good. So a spawn holds
//! [`spawning`] (shared) across the call that copies the descriptor table (`posix_spawn`,
//! `fork`), and a creation holds the lock exclusively from the descriptor's creation through its
//! mark: no table is copied while a descriptor this process means to keep out of children is
//! unmarked. Fork exclusion additionally requires the kernel to retain `FD_CLOFORK`;
//! kernels that ignore it still use the normal close-on-exec spawn policy.
//!
//! Spawns share the lock, so none waits for another (a first exec's assessment holds one
//! `posix_spawn` for about 117 ms); a creation waits only for the spawns in flight, and once it
//! waits, later spawns queue behind it (std's `RwLock` on macOS admits no new reader while any
//! thread waits), so a steady stream of spawns cannot starve it. Off Apple a spawn takes
//! nothing: there is no such drain, and the mark does nothing.

#[cfg(unix)]
use std::io;

/// What [`crate::kept_out`] marks once its `create` made it: each descriptor in it.
#[cfg(unix)]
pub trait Descriptors {
    /// Mark each descriptor ([`crate::keep_out_of_children`]).
    ///
    /// # Errors
    /// The first mark's `fcntl(2)` errno.
    fn mark(&self) -> io::Result<()>;
}

#[cfg(unix)]
macro_rules! descriptor {
    ($($t:ty),* $(,)?) => {$(
        impl Descriptors for $t {
            fn mark(&self) -> io::Result<()> {
                crate::keep_out_of_children(self)
            }
        }
    )*};
}

#[cfg(unix)]
descriptor!(
    std::os::fd::OwnedFd,
    std::fs::File,
    aterm_dirfd::Dir,
    std::io::PipeReader,
    std::io::PipeWriter,
    std::net::TcpListener,
    std::net::TcpStream,
    std::net::UdpSocket,
    std::os::unix::net::UnixDatagram,
    std::os::unix::net::UnixListener,
    std::os::unix::net::UnixStream,
);

/// An address an `accept` answers beside the connection: no descriptor.
#[cfg(unix)]
impl Descriptors for std::os::unix::net::SocketAddr {
    fn mark(&self) -> io::Result<()> {
        Ok(())
    }
}

/// An address an `accept` answers beside the connection: no descriptor.
#[cfg(unix)]
impl Descriptors for std::net::SocketAddr {
    fn mark(&self) -> io::Result<()> {
        Ok(())
    }
}

/// What an open answers beside the file ([`crate::private_file::open_private`]): no descriptor.
#[cfg(unix)]
impl Descriptors for std::fs::Metadata {
    fn mark(&self) -> io::Result<()> {
        Ok(())
    }
}

/// A pair (`socketpair`, `pipe`, `accept`'s connection and address): each.
#[cfg(unix)]
impl<A: Descriptors, B: Descriptors> Descriptors for (A, B) {
    fn mark(&self) -> io::Result<()> {
        self.0.mark()?;
        self.1.mark()
    }
}

/// A spawn in flight ([`spawning`]): no descriptor is between its creation and its mark while
/// it is held.
#[must_use = "the spawn is guarded only while this is held"]
#[derive(Debug)]
pub struct Spawning {
    #[cfg(target_vendor = "apple")]
    held: Option<std::sync::RwLockReadGuard<'static, ()>>,
    /// Never sent: the read guard is released on the thread that took it.
    _here: std::marker::PhantomData<*const ()>,
}

/// The hold ends when the guard is dropped, on every platform alike (off Apple it holds
/// nothing), so a caller drops or forgets it the same way everywhere: a forked child never
/// releases its copy.
impl Drop for Spawning {
    fn drop(&mut self) {
        #[cfg(target_vendor = "apple")]
        drop(self.held.take());
    }
}

/// Hold across the one call that copies this process's descriptor table into a child
/// (`posix_spawn`, `fork`), and only that call: a descriptor created through
/// [`crate::kept_out`] waits for it, and no spawn waits for another. A `std::process::Command`
/// spawn is not that call (with a `pre_exec` hook it forks and then waits for the child's
/// `exec`): it goes through [`spawn_std`], which releases the hold at the fork.
///
/// Off Apple it holds nothing. On a thread inside [`crate::kept_out`]'s `create` (which must not
/// spawn) it holds nothing either, as that thread already excludes every spawn.
pub fn spawning() -> Spawning {
    Spawning {
        #[cfg(target_vendor = "apple")]
        held: apple::spawn_lock_shared(),
        _here: std::marker::PhantomData,
    }
}

/// [`spawning`] for a spawn that hands its own child some of `handed` (each by a `dup2` file
/// action, or a `dup2` in the forked child): neither `posix_spawn` nor `fork` copies a
/// descriptor marked [`crate::keep_out_of_children`] (`EBADF` from the action, measured), so
/// each marked one loses its mark for this spawn and gets it back when the guard drops.
/// Meanwhile the lock is held exclusively, so no other spawn copies it while it is unmarked. A
/// spawn that hands no marked descriptor takes the shared hold.
#[cfg(target_vendor = "apple")]
pub fn handing(handed: &[std::os::fd::RawFd]) -> Handing {
    let marked: Vec<_> = handed
        .iter()
        .filter_map(|&fd| {
            // SAFETY: `F_GETFD` reads one descriptor's flags and touches no memory.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            (flags >= 0 && flags & apple::FD_CLOFORK != 0).then_some((fd, flags))
        })
        .collect();
    if marked.is_empty() {
        return Handing {
            unmarked: Vec::new(),
            _shared: Some(spawning()),
            _exclusive: None,
        };
    }
    let exclusive = apple::spawn_lock_exclusive();
    let unmarked = marked
        .into_iter()
        .filter(|&(fd, flags)| {
            // SAFETY: `F_SETFD` sets the flags of a descriptor the spawn's caller holds open
            // across the spawn, and touches no memory.
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !apple::FD_CLOFORK) == 0 }
        })
        .collect();
    Handing {
        unmarked,
        _shared: None,
        _exclusive: Some(exclusive),
    }
}

/// Off Apple, [`spawning`]: no descriptor here carries a mark a spawn would have to lift.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub fn handing(_handed: &[std::os::fd::RawFd]) -> Handing {
    Handing {
        _shared: spawning(),
    }
}

/// A spawn in flight that hands its child descriptors ([`handing`]).
#[cfg(all(unix, not(target_vendor = "apple")))]
#[must_use = "the spawn is guarded only while this is held"]
pub struct Handing {
    _shared: Spawning,
}

/// A spawn in flight that hands its child descriptors ([`handing`]).
#[cfg(target_vendor = "apple")]
#[must_use = "the spawn is guarded only while this is held"]
pub struct Handing {
    /// Each descriptor unmarked for this spawn, with the flags it gets back.
    unmarked: Vec<(std::os::fd::RawFd, std::ffi::c_int)>,
    /// The shared hold of a spawn that hands nothing marked.
    _shared: Option<Spawning>,
    /// The exclusive hold of one that does.
    _exclusive: Option<apple::Exclusive>,
}

/// Each handed descriptor is marked again before the lock is released (the fields drop after).
#[cfg(target_vendor = "apple")]
impl Drop for Handing {
    fn drop(&mut self) {
        for &(fd, flags) in &self.unmarked {
            // SAFETY: `F_SETFD` on a descriptor the spawn's caller still holds open (the spawn
            // has returned); it touches no memory.
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags) };
        }
    }
}

/// Spawn std's `command`, handing its child `handed` as [`handing`] does, with the spawn lock held
/// across its fork alone.
///
/// With a `pre_exec` hook (every reviewed std spawn has one: [`crate::spawnfd::InheritNothing`])
/// std forks and then blocks its parent on the exec-status pipe until the child execs or fails,
/// which under load (the kernel's and `syspolicyd`'s checks of the new image) takes seconds. A
/// guard held around `command.spawn()` would hold the lock across all of that, and every creation
/// ([`crate::kept_out`]: each `accept`, each socket and pipe of this process) would wait for
/// another process's exec. Here the hold ends in the fork's parent half (a `pthread_atfork`
/// handler on the forking thread), once the child has its copy of the descriptor table and before
/// std waits: each handed descriptor's mark is back and the lock is released then. A spawn that
/// never forks (it failed first, or std chose `posix_spawn`, which is itself the copy) releases it
/// when `spawn` returns.
///
/// Off Apple, `command.spawn()`: the lock holds nothing there.
///
/// # Errors
/// `spawn`'s, or on Apple `pthread_atfork`'s when the release handler cannot be registered (then
/// nothing is spawned).
#[cfg(unix)]
pub fn spawn_std(
    command: &mut crate::spawnfd::StdCommand,
    handed: &[std::os::fd::RawFd],
) -> io::Result<std::process::Child> {
    #[cfg(target_vendor = "apple")]
    {
        apple::spawn_std(command, handed)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = handed;
        command.spawn()
    }
}

/// Run `create` and then `mark` with no spawn in flight: the body of [`crate::kept_out`].
#[cfg(unix)]
pub(crate) fn creating<T, E>(
    create: impl FnOnce() -> Result<T, E>,
    mark: impl FnOnce(&T) -> Result<(), E>,
) -> Result<T, E> {
    #[cfg(target_vendor = "apple")]
    let _held = apple::spawn_lock_exclusive();
    let made = create()?;
    mark(&made)?;
    Ok(made)
}

#[cfg(target_vendor = "apple")]
mod apple {
    use std::cell::Cell;
    use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};

    /// The lock. It guards no data: a holder's panic poisons nothing it relies on.
    static SPAWN_LOCK: RwLock<()> = RwLock::new(());

    /// `FD_CLOFORK`, which Apple's libc crate does not name.
    pub(super) const FD_CLOFORK: std::ffi::c_int = 2;

    thread_local! {
        /// Whether this thread holds [`SPAWN_LOCK`] exclusively: inside a `create`, where a nested
        /// creation takes nothing more and a spawn (which `create` must not make) does not wait
        /// for itself.
        static CREATING: Cell<bool> = const { Cell::new(false) };
    }

    /// Apple's `pthread_key_t`.
    type PthreadKey = std::ffi::c_ulong;

    // Apple's libc crate binds none of these.
    unsafe extern "C" {
        fn pthread_atfork(
            prepare: Option<extern "C" fn()>,
            parent: Option<extern "C" fn()>,
            child: Option<extern "C" fn()>,
        ) -> std::ffi::c_int;
        fn pthread_key_create(
            key: *mut PthreadKey,
            destructor: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
        ) -> std::ffi::c_int;
        fn pthread_getspecific(key: PthreadKey) -> *mut std::ffi::c_void;
        fn pthread_setspecific(key: PthreadKey, value: *const std::ffi::c_void) -> std::ffi::c_int;
    }

    /// A std spawn in flight on one thread ([`spawn_std`]).
    struct InFlight {
        /// The spawn's hold, which the fork's parent half releases ([`released_at_fork`]); it is
        /// still here when the spawn returns only where the spawn made no fork.
        hold: Option<super::Handing>,
        /// The process that armed it. A child forked with it armed has a copy, which a fork of
        /// its own (in a `pre_exec` hook) must not release, as no lock call is safe there.
        armed_by: libc::pid_t,
    }

    /// The pthread key under which a thread keeps its [`InFlight`] while it is inside
    /// [`spawn_std`], or the errno that refused the key or the fork handler. A key and not a
    /// `thread_local!`: every thread of the process pays for the image's whole thread-local
    /// template (`aterm-gui`'s `thread_local_template` test), and only a thread that spawns
    /// needs this slot.
    static IN_FLIGHT: std::sync::OnceLock<Result<PthreadKey, std::ffi::c_int>> =
        std::sync::OnceLock::new();

    /// The key, made with the fork handler registered, once.
    fn in_flight_key() -> std::io::Result<PthreadKey> {
        IN_FLIGHT
            .get_or_init(|| {
                let mut key: PthreadKey = 0;
                // SAFETY: `key` is a live out-parameter, and no destructor is registered: the
                // slot is emptied before `spawn_std` returns or unwinds (`Armed`).
                let made = unsafe { pthread_key_create(&raw mut key, None) };
                if made != 0 {
                    return Err(made);
                }
                // SAFETY: registers one `extern "C"` function that lives as long as the process
                // and cannot unwind.
                let registered = unsafe { pthread_atfork(None, Some(released_at_fork), None) };
                if registered != 0 {
                    return Err(registered);
                }
                Ok(key)
            })
            .map_err(std::io::Error::from_raw_os_error)
    }

    /// The `pthread_atfork` parent handler: on the forking thread, once `fork(2)` has returned
    /// in the parent. It releases the hold of a std spawn in flight on this thread (each handed
    /// descriptor marked again first, then the lock), and does nothing for any other fork. It
    /// cannot unwind: it reads one slot, and the release only sets flags and unlocks.
    extern "C" fn released_at_fork() {
        let Some(&Ok(key)) = IN_FLIGHT.get() else {
            return;
        };
        // SAFETY: `key` is the live key `in_flight_key` made; the slot holds null or the
        // `InFlight` this thread's `spawn_std` boxed, which stays allocated until its `Armed`
        // drops, after `fork` (and so this handler) has returned.
        let Some(in_flight) = (unsafe { pthread_getspecific(key).cast::<InFlight>().as_mut() })
        else {
            return;
        };
        // SAFETY: `getpid` takes nothing and cannot fail.
        if in_flight.armed_by == unsafe { libc::getpid() } {
            drop(in_flight.hold.take());
        }
    }

    /// Empties the thread's slot when [`spawn_std`]'s spawn returns (or unwinds), releasing a
    /// hold the fork did not.
    struct Armed(PthreadKey);

    impl Drop for Armed {
        fn drop(&mut self) {
            // SAFETY: the key is live; the slot holds null or the pointer `spawn_std` took from
            // `Box::into_raw` on this thread, which nothing else frees.
            let in_flight = unsafe { pthread_getspecific(self.0).cast::<InFlight>() };
            // SAFETY: a non-null slot names that live box, and no other reference to it exists:
            // the fork handler's ended when `fork` returned.
            let Some(live) = (unsafe { in_flight.as_mut() }) else {
                return;
            };
            // Released and disarmed before the slot is emptied, so that a store the system
            // refuses still leaves nothing held and nothing a later fork's handler would release.
            drop(live.hold.take());
            live.armed_by = 0;
            // SAFETY: the key is live. Once the slot no longer names the box it is owned here
            // alone. A refused store keeps the disarmed box allocated, since the slot still
            // names it and a later fork on this thread reads it.
            unsafe {
                if pthread_setspecific(self.0, std::ptr::null()) == 0 {
                    drop(Box::from_raw(in_flight));
                }
            }
        }
    }

    /// [`super::spawn_std`] on Apple.
    pub(super) fn spawn_std(
        command: &mut crate::spawnfd::StdCommand,
        handed: &[std::os::fd::RawFd],
    ) -> std::io::Result<std::process::Child> {
        let key = in_flight_key()?;
        let in_flight = Box::into_raw(Box::new(InFlight {
            hold: Some(super::handing(handed)),
            // SAFETY: `getpid` takes nothing and cannot fail.
            armed_by: unsafe { libc::getpid() },
        }));
        // SAFETY: the key is live, and the pointer is this thread's own until `Armed` frees it.
        let stored = unsafe { pthread_setspecific(key, in_flight.cast_const().cast()) };
        if stored != 0 {
            // SAFETY: the store was refused, so the box is still owned here alone; dropping it
            // releases the hold before anything is spawned.
            drop(unsafe { Box::from_raw(in_flight) });
            return Err(std::io::Error::from_raw_os_error(stored));
        }
        let _armed = Armed(key);
        command.spawn()
    }

    /// [`super::spawning`]'s guard: `None` on a thread that already holds the lock exclusively.
    pub(super) fn spawn_lock_shared() -> Option<RwLockReadGuard<'static, ()>> {
        if CREATING.with(Cell::get) {
            return None;
        }
        Some(match SPAWN_LOCK.try_read() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(held)) => held.into_inner(),
            Err(TryLockError::WouldBlock) => {
                #[cfg(any(test, debug_assertions))]
                let _queued = super::seam::Queued::here();
                SPAWN_LOCK.read().unwrap_or_else(PoisonError::into_inner)
            }
        })
    }

    /// The exclusive hold of one creation, or of none on a thread already inside one.
    pub(super) struct Exclusive(Option<RwLockWriteGuard<'static, ()>>);

    impl Drop for Exclusive {
        fn drop(&mut self) {
            if let Some(held) = self.0.take() {
                CREATING.with(|creating| creating.set(false));
                #[cfg(any(test, debug_assertions))]
                super::seam::holding(None);
                drop(held);
            }
        }
    }

    pub(super) fn spawn_lock_exclusive() -> Exclusive {
        if CREATING.with(Cell::get) {
            return Exclusive(None);
        }
        let held = match SPAWN_LOCK.try_write() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(held)) => held.into_inner(),
            Err(TryLockError::WouldBlock) => {
                #[cfg(any(test, debug_assertions))]
                let _waiting = super::seam::Waiting::exclusive();
                SPAWN_LOCK.write().unwrap_or_else(PoisonError::into_inner)
            }
        };
        CREATING.with(|creating| creating.set(true));
        #[cfg(any(test, debug_assertions))]
        super::seam::holding(Some(std::thread::current().id()));
        Exclusive(Some(held))
    }
}

/// Test support (test and debug builds): which threads wait in [`spawning`] for a creation now,
/// how many wait to hold the lock exclusively, and which thread holds it exclusively, so a test
/// can tell whose hold a spawn or a creation waits behind.
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, PoisonError};
    use std::thread::ThreadId;

    static QUEUED: Mutex<Vec<ThreadId>> = Mutex::new(Vec::new());

    /// How many threads wait to hold the lock exclusively. A count, not a list: a wait here sits
    /// in every creation's path (a records write, a socket), and a list's first push leaves its
    /// buffer and Darwin's boxed mutex allocated on the waiting thread for good, the 96 bytes a
    /// test counting one records write's retained heap caught whenever another test's creation
    /// held the lock (aterm-secrets' `server_record_persistence_memory`, 6 of 24 runs).
    static EXCLUSIVE_WAITING: AtomicUsize = AtomicUsize::new(0);

    /// The thread holding the lock exclusively (a creation or a spawn handing a kept-out
    /// descriptor), set while it holds it.
    static EXCLUSIVE: Mutex<Option<ThreadId>> = Mutex::new(None);

    /// Whether `thread` waits now in [`super::spawning`] behind a creation. Always `false` off
    /// Apple, where a spawn waits for nothing.
    #[must_use]
    pub fn spawn_waits(thread: ThreadId) -> bool {
        QUEUED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&thread)
    }

    /// How many threads of this process wait now to hold the lock exclusively (a creation in
    /// [`crate::kept_out`], or a spawn handing a kept-out descriptor). Always 0 off Apple, where
    /// nothing takes it.
    #[must_use]
    pub fn exclusive_waiters() -> usize {
        EXCLUSIVE_WAITING.load(Ordering::SeqCst)
    }

    /// The thread that holds the lock exclusively now, if any. Always `None` off Apple, where
    /// nothing takes it.
    #[must_use]
    pub fn exclusive_holder() -> Option<ThreadId> {
        *EXCLUSIVE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record the exclusive holder: set by the thread that took the lock, cleared by it before
    /// it releases the lock, so no other holder's record is cleared.
    #[cfg(target_vendor = "apple")]
    pub(super) fn holding(holder: Option<ThreadId>) {
        *EXCLUSIVE.lock().unwrap_or_else(PoisonError::into_inner) = holder;
    }

    /// This thread, listed while it waits.
    #[cfg(target_vendor = "apple")]
    pub(super) struct Queued(&'static Mutex<Vec<ThreadId>>, ThreadId);

    #[cfg(target_vendor = "apple")]
    impl Queued {
        /// A spawn waiting behind a creation ([`spawn_waits`]).
        pub(super) fn here() -> Self {
            let me = std::thread::current().id();
            QUEUED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(me);
            Self(&QUEUED, me)
        }
    }

    /// A thread counted while it waits to hold the lock exclusively ([`exclusive_waiters`]).
    #[cfg(target_vendor = "apple")]
    pub(super) struct Waiting(());

    #[cfg(target_vendor = "apple")]
    impl Waiting {
        /// Count this thread until the guard drops.
        pub(super) fn exclusive() -> Self {
            EXCLUSIVE_WAITING.fetch_add(1, Ordering::SeqCst);
            Self(())
        }
    }

    #[cfg(target_vendor = "apple")]
    impl Drop for Waiting {
        fn drop(&mut self) {
            EXCLUSIVE_WAITING.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[cfg(target_vendor = "apple")]
    impl Drop for Queued {
        fn drop(&mut self) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|t| *t != self.1);
        }
    }
}

#[cfg(test)]
#[path = "spawn_lock_tests.rs"]
pub(crate) mod tests;
