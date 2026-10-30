// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A FRONT'S SIGNALS (`docs/DESIGN-adaptive-build-2026-10-24.md`, BUILD-ADMIT): what reaches a
//! supervised build through the front that holds its claim. With the build run as the front's
//! child, a signal sent to the pid its caller holds reaches the front alone, where an exec'd call
//! was the build itself: the front passes it on.
//!
//! Every catchable signal whose default action ends a process with no core ([`PASSED_ON`]: the
//! ones a supervisor, `timeout` or a person's `kill` sends) is passed on whenever it reaches the
//! front. The keys' signals ([`KEYS`], SIGINT and SIGQUIT) are passed on only when the front does
//! not lead its terminal's foreground job ([`key_passes`]): a terminal's ^C or ^\ goes to that
//! whole job, the build with it, and a second copy would be its second press. A front of no
//! terminal (an agent's shell, a supervisor's pipe) passes every one on; a front leading its
//! terminal's job swallows one sent to its pid alone, as `system(3)` does.
//!
//! A handler only notes the signal on a pipe, which is async-signal-safe; a thread of the front's
//! own reads it and sends it on, once the build's pid is known (a signal that arrives while the
//! build is being spawned waits for it, and one whose build never started ends the front by
//! itself, with its default action), and never once the build is reaped: the front learns of its
//! exit before it reaps it ([`Front::reap`]), so the pid it signals is never another process's.
//! The handlers are armed before the build is spawned; an exec puts a caught signal back to its
//! default, so the build starts with its own. One front at a time arms a process's dispositions
//! ([`crate::platform::FORWARDING_TURN`]), and [`Front::disarm`] puts back what it found.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// The signals a front passes on to its build whenever they reach it: every catchable one whose
/// default action ends a process with no core dumped. (SIGPIPE is the front's own write's, and
/// SIGXCPU and SIGXFSZ its own limits'.)
pub const PASSED_ON: [libc::c_int; 7] = [
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGALRM,
    libc::SIGPROF,
    libc::SIGVTALRM,
];

/// The terminal keys' signals: passed on only where the terminal's key would not reach the build
/// itself ([`key_passes`]).
pub const KEYS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGQUIT];

/// The write end of the armed front's pipe, `-1` while none is armed.
static NOTE_TO: AtomicI32 = AtomicI32::new(-1);

/// The armed front's controlling terminal, held open, `-1` for none.
static TERMINAL: AtomicI32 = AtomicI32::new(-1);

/// Whether a key's signal reaches the build only through the front: `terminal` (the front's
/// controlling terminal, `-1` for none) is no terminal whose foreground job this process's group
/// is. Async-signal-safe: `tcgetpgrp` and `getpgrp` are.
#[must_use]
pub fn key_passes(terminal: RawFd) -> bool {
    // SAFETY: two integer calls; a descriptor that is no terminal answers -1.
    terminal < 0 || unsafe { libc::tcgetpgrp(terminal) != libc::getpgrp() }
}

/// This thread's `errno`.
fn errno() -> *mut libc::c_int {
    #[cfg(target_vendor = "apple")]
    // SAFETY: the thread's own errno slot, always valid.
    unsafe {
        libc::__error()
    }
    #[cfg(not(target_vendor = "apple"))]
    // SAFETY: the thread's own errno slot, always valid.
    unsafe {
        libc::__errno_location()
    }
}

/// Note `sig` on the armed front's pipe: one byte, written whole or not at all, never blocking
/// (the pipe is non-blocking), `errno` kept.
fn note(sig: libc::c_int) {
    let fd = NOTE_TO.load(Ordering::SeqCst);
    let Ok(byte) = u8::try_from(sig) else {
        return;
    };
    if fd < 0 {
        return;
    }
    // SAFETY: `errno` is the thread's own slot; `write` is async-signal-safe and reads one byte
    // from `byte`, live across the call.
    unsafe {
        let saved = *errno();
        libc::write(fd, (&raw const byte).cast(), 1);
        *errno() = saved;
    }
}

/// The handler of [`PASSED_ON`]'s signals.
extern "C" fn pass_on(sig: libc::c_int) {
    note(sig);
}

/// The handler of [`KEYS`]' signals.
extern "C" fn pass_on_key(sig: libc::c_int) {
    if key_passes(TERMINAL.load(Ordering::SeqCst)) {
        note(sig);
    }
}

/// Where the front sends what it passes on.
enum Aim {
    /// The build is being spawned: hold what arrives.
    Arming,
    /// The build runs as this pid.
    Build(libc::pid_t),
    /// The build has been reaped: nothing is sent.
    Reaped,
    /// The build never started: what arrives ends the front.
    Never,
}

/// What the front and its forwarding thread share.
struct Shared {
    aim: Mutex<Aim>,
    moved: Condvar,
    /// The last signal passed on to the build.
    passed: Mutex<Option<libc::c_int>>,
    /// A signal that arrived for a build that never started.
    unsent: Mutex<Option<libc::c_int>>,
    /// The build's tree, read before each signal is sent on, so what it leaves is known
    /// ([`aterm_capacity::tree_meter::TreeWatch::left`]).
    tree: Mutex<Option<aterm_capacity::tree_meter::TreeWatch>>,
}

/// A FRONT ARMED to pass signals on to the build it spawns ([`Front::arm`], [`Front::aim`]).
pub struct Front {
    shared: Arc<Shared>,
    write: Option<OwnedFd>,
    forwarder: Option<JoinHandle<()>>,
    previous: Vec<(libc::c_int, libc::sigaction)>,
    terminal: Option<std::fs::File>,
    _turn: MutexGuard<'static, ()>,
}

/// Install `handler` for `sig` with `SA_RESTART`, unless `sig` was ignored: the disposition it
/// replaced, to put back; `None` where it was ignored (the front's caller asked for that) or the
/// call failed.
fn install(sig: libc::c_int, handler: extern "C" fn(libc::c_int)) -> Option<libc::sigaction> {
    let mut old = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null new action reads the disposition into `old`, zeroed and live.
    if unsafe { libc::sigaction(sig, std::ptr::null(), old.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: written whole by the call that succeeded.
    let old = unsafe { old.assume_init() };
    if old.sa_sigaction == libc::SIG_IGN {
        return None;
    }
    let mut new = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: `new` is one zeroed `sigaction`, given a handler that makes only async-signal-safe
    // calls ([`note`]), an empty mask and `SA_RESTART`, then installed; `old` is a valid
    // disposition read above.
    unsafe {
        let action = new.as_mut_ptr();
        (*action).sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigemptyset(&raw mut (*action).sa_mask);
        (*action).sa_flags = libc::SA_RESTART;
        (libc::sigaction(sig, action, std::ptr::null_mut()) == 0).then_some(old)
    }
}

/// A non-blocking pipe, both ends close-on-exec: its read end and write end.
fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors are this process's own, each owned once here.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [read.as_raw_fd(), write.as_raw_fd()] {
        // SAFETY: `fcntl` on a descriptor this function owns, with plain integers.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((read, write))
}

/// The byte [`Front::disarm`] writes to end the forwarding thread: no signal's number.
const STOP: u8 = 0;

/// The forwarding thread: each signal noted on `read`, sent on as [`Shared::aim`] says, until
/// [`STOP`] (a child that inherited the write end in the moment before it was made close-on-exec
/// would keep it from ever reading the pipe's end) or the pipe's end.
fn forward(read: OwnedFd, shared: &Shared) {
    loop {
        let mut byte = 0u8;
        // SAFETY: reads at most one byte into `byte`, live across the call, from a descriptor
        // this thread owns.
        let n = unsafe { libc::read(read.as_raw_fd(), (&raw mut byte).cast(), 1) };
        if n == 0 || (n == 1 && byte == STOP) {
            return;
        }
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        let sig = libc::c_int::from(byte);
        let mut aim = shared.aim.lock().unwrap_or_else(PoisonError::into_inner);
        while matches!(*aim, Aim::Arming) {
            aim = shared
                .moved
                .wait(aim)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match *aim {
            Aim::Build(pid) => {
                let watch = shared
                    .tree
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Some(watch) = watch {
                    watch.look();
                }
                // SAFETY: `kill` with two integers; `pid` is the front's unreaped child.
                unsafe { libc::kill(pid, sig) };
                *shared.passed.lock().unwrap_or_else(PoisonError::into_inner) = Some(sig);
            }
            Aim::Never => {
                *shared.unsent.lock().unwrap_or_else(PoisonError::into_inner) = Some(sig);
            }
            Aim::Arming | Aim::Reaped => {}
        }
    }
}

impl Front {
    /// ARM before the build is spawned, once no other forwarding is armed in this process
    /// ([`crate::platform::FORWARDING_TURN`]): [`PASSED_ON`]'s and [`KEYS`]' handlers, the pipe
    /// they note on, the thread that reads it, and this process's controlling terminal, held open
    /// close-on-exec, that [`key_passes`] reads. `None` where the pipe or the thread could not be
    /// made: the build then runs with nothing passed on, as a plain child would.
    #[must_use]
    pub fn arm() -> Option<Front> {
        let turn = crate::platform::FORWARDING_TURN
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (read, write) = pipe().ok()?;
        let shared = Arc::new(Shared {
            aim: Mutex::new(Aim::Arming),
            moved: Condvar::new(),
            passed: Mutex::new(None),
            unsent: Mutex::new(None),
            tree: Mutex::new(None),
        });
        let theirs = Arc::clone(&shared);
        let forwarder = std::thread::Builder::new()
            .name("build-front".to_owned())
            .spawn(move || forward(read, &theirs))
            .ok()?;
        NOTE_TO.store(write.as_raw_fd(), Ordering::SeqCst);
        let terminal = std::fs::File::open("/dev/tty").ok();
        TERMINAL.store(
            terminal
                .as_ref()
                .map_or(-1, std::os::fd::AsRawFd::as_raw_fd),
            Ordering::SeqCst,
        );
        let mut front = Front {
            shared,
            write: Some(write),
            forwarder: Some(forwarder),
            previous: Vec::new(),
            terminal,
            _turn: turn,
        };
        for sig in PASSED_ON {
            if let Some(old) = install(sig, pass_on) {
                front.previous.push((sig, old));
            }
        }
        for sig in KEYS {
            if let Some(old) = install(sig, pass_on_key) {
                front.previous.push((sig, old));
            }
        }
        Some(front)
    }

    /// The build runs as `pid`: send it what was held and what arrives, its tree (where it is
    /// metered) read just before each send, so a member it started since the last read is one
    /// it leaves.
    pub fn aim(&self, pid: u32, tree: Option<aterm_capacity::tree_meter::TreeWatch>) {
        *self
            .shared
            .tree
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = tree;
        self.set(libc::pid_t::try_from(pid).map_or(Aim::Never, Aim::Build));
    }

    /// WAIT FOR the build to end and reap it: its exit is seen first with the build left
    /// unreaped (`waitid` with `WNOWAIT`), then nothing more is sent to its pid, and only then is
    /// it reaped, so the pid this front signals cannot be another process's.
    ///
    /// # Errors
    /// The reap's.
    pub fn reap(
        &self,
        child: &mut aterm_uds::spawnfd::TransparentChild,
    ) -> std::io::Result<std::process::ExitStatus> {
        let pid: libc::id_t = child.id();
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: `info` is one zeroed `siginfo_t`, live across the call; `WNOWAIT`
            // leaves the child unreaped.
            let seen = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if seen == 0
                || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
        }
        self.set(Aim::Reaped);
        child.wait()
    }

    /// The build never started: a signal held for it ends this process at [`Self::disarm`].
    pub fn never(&self) {
        self.set(Aim::Never);
    }

    fn set(&self, aim: Aim) {
        *self
            .shared
            .aim
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = aim;
        self.shared.moved.notify_all();
    }

    /// The last signal passed on to the build; `None` where none was.
    #[must_use]
    pub fn passed(&self) -> Option<libc::c_int> {
        *self
            .shared
            .passed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// DISARM: put back every disposition [`Self::arm`] and [`Self::aim`] replaced, let every
    /// noted signal be sent, and end the forwarding thread; then a signal that arrived for a build
    /// that never started ends this process, with its default action (none of [`PASSED_ON`] dumps
    /// a core).
    pub fn disarm(mut self) {
        self.end();
    }

    fn end(&mut self) {
        for (sig, old) in self.previous.drain(..) {
            // SAFETY: puts back a disposition this front read whole before it replaced it.
            unsafe { libc::sigaction(sig, &raw const old, std::ptr::null_mut()) };
        }
        NOTE_TO.store(-1, Ordering::SeqCst);
        TERMINAL.store(-1, Ordering::SeqCst);
        drop(self.terminal.take());
        if matches!(
            *self
                .shared
                .aim
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            Aim::Arming
        ) {
            self.set(Aim::Never);
        }
        if let Some(write) = &self.write {
            let stop = STOP;
            // SAFETY: writes the one byte `stop`, live across the call, to this front's pipe.
            unsafe { libc::write(write.as_raw_fd(), (&raw const stop).cast(), 1) };
        }
        if let Some(forwarder) = self.forwarder.take() {
            let _ = forwarder.join();
        }
        drop(self.write.take());
        let unsent = self
            .shared
            .unsent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(sig) = unsent {
            // SAFETY: `signal` with the default action for one of PASSED_ON's signals, none of
            // which dumps a core; `raise` delivers it to this thread.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                #[expect(
                    clippy::disallowed_methods,
                    reason = "re-raises a signal that dumps no core (PASSED_ON) held for a build that never started"
                )]
                libc::raise(sig);
            }
        }
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.end();
    }
}
