// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Raw mode on a terminal, put back however the program leaves it.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::{Mutex, Once, PoisonError};
use std::thread::ThreadId;

use crate::signals;
use crate::tty::{Mode, Modes, RESET_ORDER, disable_bytes, enable_bytes, set_order};

const TERMIOS_BYTES: usize = size_of::<libc::termios>();

/// A termios kept as bytes, aligned for the struct: saved into atomics a signal handler
/// reads, so no handler ever races a writer on a typed value.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
struct Termios([u8; TERMIOS_BYTES]);

const _: () = assert!(align_of::<libc::termios>() <= 8);

impl Termios {
    const fn as_ptr(&self) -> *const libc::termios {
        self.0.as_ptr().cast()
    }

    const fn as_mut_ptr(&mut self) -> *mut libc::termios {
        self.0.as_mut_ptr().cast()
    }

    /// The settings of the terminal on `fd`.
    fn read(fd: RawFd) -> io::Result<Self> {
        let mut termios = Self([0; TERMIOS_BYTES]);
        // SAFETY: the buffer is a writable, aligned termios; `tcgetattr` fills it.
        if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(termios)
    }

    /// These settings made raw.
    fn raw(self) -> Self {
        let mut raw = self;
        // SAFETY: the buffer is an aligned termios every bit pattern of which is valid.
        unsafe {
            libc::cfmakeraw(raw.as_mut_ptr());
            let t = &mut *raw.as_mut_ptr();
            t.c_iflag &= !(libc::ICRNL | libc::IXON);
            t.c_oflag &= !libc::OPOST;
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
        }
        raw
    }

    fn apply(&self, fd: RawFd, when: libc::c_int) -> io::Result<()> {
        // SAFETY: `self` is a termios `tcgetattr` filled (or one edited from it).
        if unsafe { libc::tcsetattr(fd, when, self.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A guard is alive.
static CLAIMED: AtomicBool = AtomicBool::new(false);
/// The terminal's descriptor; -1 when no guard is alive.
static FD: AtomicI32 = AtomicI32::new(-1);
/// The saved settings are to be put back.
static ARMED: AtomicBool = AtomicBool::new(false);
/// The modes set ([`Modes`] bits).
static MODES: AtomicU8 = AtomicU8::new(0);
/// The saved settings, a byte at a time.
static SAVED: [AtomicU8; TERMIOS_BYTES] = [const { AtomicU8::new(0) }; TERMIOS_BYTES];
/// The thread that made the live guard: a panic there puts the terminal back.
static OWNER: Mutex<Option<ThreadId>> = Mutex::new(None);

/// Serializes the tests that make this process's one guard or signal wake.
#[cfg(test)]
pub(crate) static TEST_TERMINAL: Mutex<()> = Mutex::new(());

fn save(termios: &Termios) {
    for (slot, byte) in SAVED.iter().zip(termios.0) {
        slot.store(byte, Ordering::Relaxed);
    }
}

fn saved() -> Termios {
    let mut termios = Termios([0; TERMIOS_BYTES]);
    for (byte, slot) in termios.0.iter_mut().zip(&SAVED) {
        *byte = slot.load(Ordering::Relaxed);
    }
    termios
}

/// Write all of `bytes` to `fd`, giving up on any error but EINTR. Async-signal-safe.
fn write_raw(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: a write(2) from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = bytes.get(n.unsigned_abs()..).unwrap_or_default();
        } else if n < 0 && signals::errno() == libc::EINTR {
            continue;
        } else {
            return;
        }
    }
}

/// Put the terminal back: leave every mode set, then restore the saved settings and
/// drop the input the program never read, so replies and keys meant for it are not
/// typed into the shell.
fn restore() {
    let fd = FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    for (mode, bytes) in RESET_ORDER {
        if MODES.load(Ordering::Acquire) & mode.bits() != 0 {
            write_raw(fd, bytes);
            MODES.fetch_and(!mode.bits(), Ordering::AcqRel);
        }
    }
    if ARMED.load(Ordering::Acquire) {
        let saved = saved();
        // SAFETY: `saved` holds the settings `tcgetattr` gave when the guard was made; the flush.
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, saved.as_ptr());
            libc::tcflush(fd, libc::TCIFLUSH);
        }
        ARMED.store(false, Ordering::Release);
    }
}

/// SIGINT, SIGTERM or SIGHUP while a guard is alive: the terminal put back, then the
/// process ended as the signal ends it, the default disposition set again and the
/// signal raised, which is held until this handler returns.
extern "C" fn restore_and_end(signal: libc::c_int) {
    restore();
    // SAFETY: `sigaction` is plain data for which all-zero is a valid value.
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: the default disposition, for the signal this handler took.
    unsafe { libc::sigaction(signal, &raw const default, std::ptr::null_mut()) };
    #[expect(
        clippy::disallowed_methods,
        reason = "SIGINT, SIGTERM or SIGHUP, no core signal: the process ends as that signal \
                  ends it, its terminal put back"
    )]
    // SAFETY: `raise` is async-signal-safe; the disposition is the default.
    unsafe {
        libc::raise(signal);
    }
}

/// Route `sig` to [`restore_and_end`] unless it is ignored; the disposition replaced.
fn catch_fatal(sig: libc::c_int) -> Option<(libc::c_int, libc::sigaction)> {
    // SAFETY: an all-zero `sigaction` is a valid value.
    let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action only reads the current one.
    unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut was) };
    if was.sa_sigaction == libc::SIG_IGN {
        return None;
    }
    // SAFETY: as above.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = restore_and_end as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `sa.sa_mask` is a valid out-parameter owned by this frame.
    unsafe { libc::sigemptyset(&raw mut sa.sa_mask) };
    // SAFETY: installs an async-signal-safe handler; `was` receives the one replaced.
    unsafe { libc::sigaction(sig, &raw const sa, &raw mut was) };
    Some((sig, was))
}

/// Chain [`restore`] before the panic hook already installed, once per process.
fn chain_panic_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if cfg!(panic = "abort") || on_guard_thread() {
                restore();
            }
            previous(info);
        }));
    });
}

/// Whether the calling thread made the live guard.
fn on_guard_thread() -> bool {
    let owner = *OWNER.lock().unwrap_or_else(PoisonError::into_inner);
    owner == Some(std::thread::current().id())
}

/// The terminal on a descriptor in raw mode.
#[derive(Debug)]
pub struct RawGuard {
    fd: RawFd,
    /// The flags the kitty entry carries, to push again after a suspension.
    kitty: u8,
    /// The SIGINT, SIGTERM and SIGHUP dispositions replaced.
    replaced: Vec<(libc::c_int, libc::sigaction)>,
}

impl RawGuard {
    /// Save the settings of the terminal on `fd` and set it raw.
    /// # Errors
    /// `fd` is not a terminal.
    pub fn enter(fd: RawFd) -> io::Result<Self> {
        if CLAIMED.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                "a terminal is already raw in this process",
            ));
        }
        let saved = match Termios::read(fd) {
            Ok(saved) => saved,
            Err(error) => {
                CLAIMED.store(false, Ordering::Release);
                return Err(error);
            }
        };
        save(&saved);
        MODES.store(0, Ordering::Release);
        FD.store(fd, Ordering::Release);
        ARMED.store(true, Ordering::Release);
        *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = Some(std::thread::current().id());
        chain_panic_hook();
        let replaced = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            .filter_map(catch_fatal)
            .collect();
        let guard = Self {
            fd,
            kitty: 0,
            replaced,
        };
        saved.raw().apply(fd, libc::TCSANOW)?;
        Ok(guard)
    }

    /// The modes set now.
    #[must_use]
    pub fn modes(&self) -> Modes {
        Modes::from_bits(MODES.load(Ordering::Acquire))
    }

    /// Set `mode`, to be put back however the program leaves.
    /// # Errors
    /// The write's OS error.
    pub fn enable(&mut self, mode: Mode) -> io::Result<()> {
        let bit = mode.bit().bits();
        let before = MODES.fetch_or(bit, Ordering::AcqRel);
        if before & bit != 0 {
            return Ok(());
        }
        if let Mode::KittyKeys(flags) = mode {
            self.kitty = flags;
        }
        self.write(&enable_bytes(mode, Modes::from_bits(before), self.kitty))
    }

    /// Put `mode` back now.
    /// # Errors
    /// The write's OS error.
    pub fn disable(&mut self, mode: Mode) -> io::Result<()> {
        let set = self.modes();
        if !set.contains(mode.bit()) {
            return Ok(());
        }
        let written = self.write(&disable_bytes(mode, set, self.kitty));
        MODES.fetch_and(!mode.bit().bits(), Ordering::AcqRel);
        written
    }

    /// Set the terminal raw again.
    /// # Errors
    /// The settings or the write failed.
    pub fn reassert(&mut self) -> io::Result<()> {
        if !ARMED.load(Ordering::Acquire) {
            return Ok(());
        }
        saved().raw().apply(self.fd, libc::TCSANOW)?;
        let bytes: Vec<u8> = set_order(self.modes(), self.kitty)
            .flat_map(Mode::reassert_bytes)
            .collect();
        self.write(&bytes)
    }

    /// Put the terminal back for a suspension; the modes that were set.
    pub(crate) fn leave(&mut self) -> Modes {
        let modes = self.modes();
        restore();
        modes
    }

    /// Back from a suspension.
    pub(crate) fn reenter(&mut self, modes: Modes) -> io::Result<()> {
        let saved = Termios::read(self.fd)?;
        save(&saved);
        ARMED.store(true, Ordering::Release);
        saved.raw().apply(self.fd, libc::TCSANOW)?;
        for mode in set_order(modes, self.kitty) {
            self.enable(mode)?;
        }
        Ok(())
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            // SAFETY: a write(2) from a live slice to the guard's open descriptor.
            let n = unsafe { libc::write(self.fd, rest.as_ptr().cast(), rest.len()) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            rest = rest.get(n.unsigned_abs()..).unwrap_or_default();
        }
        Ok(())
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore();
        for (sig, was) in &self.replaced {
            // SAFETY: puts back the disposition `enter` replaced.
            unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
        }
        FD.store(-1, Ordering::Release);
        *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = None;
        CLAIMED.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
