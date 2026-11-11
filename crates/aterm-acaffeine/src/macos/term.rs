// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SIGTERM handling: a SIGTERM a process sent stops the mug, which keeps the saved mode (the app's
//! `stop_from_outside`), while one the system sent (launchd's, the kernel's) has its default
//! action.

use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::sync::atomic::{AtomicI32, Ordering};

static WAKE: AtomicI32 = AtomicI32::new(-1);
static SENDER: AtomicI32 = AtomicI32::new(0);

/// Whether a SIGTERM from `sender` is the system's: launchd's (pid 1) or the kernel's (0).
pub(super) const fn sent_by_the_system(sender: libc::pid_t) -> bool {
    sender <= 1
}

/// SIGTERM is taken while this lives ([`watch`]); dropped, it has its default action again.
#[derive(Debug)]
pub(super) struct Watching {
    _private: (),
}

impl Drop for Watching {
    fn drop(&mut self) {
        set_default();
        WAKE.store(-1, Ordering::Release);
    }
}

/// Installs the SIGTERM handler until the returned [`Watching`] is dropped.
pub(super) fn watch(quit: fn(libc::pid_t)) -> Option<Watching> {
    // SAFETY: all-zero is valid storage for the action; a null new action only reads.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: as above; `current` is a valid out-parameter owned by this frame.
    let read = unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut current) };
    if read != 0 || current.sa_sigaction != libc::SIG_DFL {
        return None;
    }
    let (mut reader, writer) = aterm_uds::kept_out(io::pipe).ok()?;
    // SAFETY: `fcntl` reads and sets the status flags of the pipe's live write end.
    let nonblocking = unsafe {
        let flags = libc::fcntl(writer.as_raw_fd(), libc::F_GETFL);
        flags >= 0 && libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
    };
    if !nonblocking {
        return None;
    }
    std::thread::Builder::new()
        .name("acaffeine-sigterm".to_owned())
        .spawn(move || {
            let mut wakes = [0_u8; 64];
            loop {
                match reader.read(&mut wakes) {
                    Ok(0) => return,
                    Ok(_) => {
                        let sender = SENDER.load(Ordering::Acquire);
                        if std::panic::catch_unwind(|| quit(sender)).is_err() {
                            end_by(libc::SIGTERM);
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        })
        .ok()?;
    WAKE.store(writer.as_raw_fd(), Ordering::Release);
    if !install() {
        WAKE.store(-1, Ordering::Release);
        return None;
    }
    let _ = writer.into_raw_fd();
    Some(Watching { _private: () })
}

extern "C" fn on_term(sig: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    let sender = if info.is_null() {
        0
    } else {
        // SAFETY: the kernel hands an `SA_SIGINFO` handler a valid `siginfo_t`.
        unsafe { (*info).si_pid }
    };
    let wake = WAKE.load(Ordering::Acquire);
    if wake < 0 || sent_by_the_system(sender) {
        end_by(sig);
        return;
    }
    SENDER.store(sender, Ordering::Release);
    // SAFETY: `__error` is this thread's errno, put back after the `write`, which reads one byte of this frame into the pipe's write end, kept open for good. A failed write loses nothing: a full pipe holds a wake already.
    unsafe {
        let errno = libc::__error();
        let saved = *errno;
        let wake_byte = 1_u8;
        let _ = libc::write(wake, (&raw const wake_byte).cast(), 1);
        *errno = saved;
    }
}

fn install() -> bool {
    // SAFETY: all-zero is a valid action, filled in below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_term
        as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
        as libc::sighandler_t;
    // SAFETY: `sa_mask` is a valid out-parameter owned by this frame.
    unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
    // SAFETY: installs `on_term`, which is async-signal-safe; the old action is not asked for.
    unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) == 0 }
}

fn set_default() {
    // SAFETY: all-zero is a valid empty mask and no flags; `SIG_DFL` is the default action.
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: `default` is fully initialized; the old action is not asked for.
    unsafe { libc::sigaction(libc::SIGTERM, &raw const default, std::ptr::null_mut()) };
}

/// Terminates the process using SIGTERM's default action.
pub(super) fn end_by_sigterm() {
    end_by(libc::SIGTERM);
}

fn end_by(sig: libc::c_int) {
    // SAFETY: `signal` and `raise` are async-signal-safe; the default action of the one signal this is called with, SIGTERM, ends the process without a core.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        #[expect(
            clippy::disallowed_methods,
            reason = "re-raises SIGTERM with its default action, which dumps no core"
        )]
        libc::raise(sig);
    }
}

#[cfg(test)]
#[path = "term_tests.rs"]
mod tests;
