// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::catch_stops`]'s and [`super::System::defer_stops`]'s live arms: catches
//! SIGINT, SIGTERM, SIGHUP and SIGQUIT, and leaves one already ignored (`nohup`, `trap '' HUP`)
//! ignored. [`say_on_stop`] says one line at such a stop and lets it end the process.

use std::ffi::c_int;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

const STOPS: [c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// Only ordinary code owns this mutex. The signal handlers touch atomics, never this state.
static OWNERS: Mutex<Owners> = Mutex::new(Owners {
    catches: 0,
    sayings: 0,
    resend: false,
    was: None,
});

struct Owners {
    catches: usize,
    sayings: usize,
    /// A deferring owner may finish before the last catching owner.
    resend: bool,
    was: Option<[(c_int, Option<libc::sigaction>); 4]>,
}

impl Owners {
    /// Select the process handler from the live owners, preserving the original ignored
    /// signals. An owner changing underneath a catch never replaces the catch's handler.
    fn select(&mut self) {
        let handler = if self.catches != 0 {
            note as extern "C" fn(c_int)
        } else if self.sayings != 0 {
            say_then_stop as extern "C" fn(c_int)
        } else {
            if let Some(was) = self.was.take() {
                put_back(&was);
            }
            return;
        };
        // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty
        // mask, no flags).
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        if let Some(was) = &self.was {
            for (sig, previous) in was {
                if previous.is_some() {
                    // SAFETY: selects a valid handler for a signal this lifetime replaced;
                    // the original action remains in `was` until its last owner is gone.
                    unsafe { libc::sigaction(*sig, &action, std::ptr::null_mut()) };
                }
            }
        } else {
            self.was = Some(STOPS.map(|sig| replace_unless_ignored(sig, &action)));
        }
    }
}

/// The handler: keep the first signal, nothing else (async-signal-safe).
extern "C" fn note(sig: c_int) {
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

/// The line [`say_on_stop`]'s handler writes, one pointer the handler reads whole.
static SAID: AtomicPtr<&'static str> = AtomicPtr::new(std::ptr::null_mut());

/// Whether [`say_on_stop`]'s handler wrote its line since it was armed: a second stop that
/// meets the handler before the first ends the process says nothing more.
static SPOKEN: AtomicBool = AtomicBool::new(false);

/// [`say_on_stop`]'s handler (async-signal-safe: atomics, `write`, `sigaction`, `raise` and
/// `_exit` only): the line on standard error once, a terminal gone with its tab being no error,
/// then the stop again. The signal's disposition goes back to its default first, so the stop
/// sent again (held until the handler returns, as the kernel holds a signal while its own
/// handler runs) ends the process as it would have; a default SIGQUIT would dump core and write
/// a crash report, so it exits with the shell's status for it, as [`resend`] does.
extern "C" fn say_then_stop(sig: c_int) {
    let said = SAID.load(Ordering::SeqCst);
    if !said.is_null() && !SPOKEN.swap(true, Ordering::SeqCst) {
        // SAFETY: a non-null `SAID` is a leaked `&'static str` [`say_on_stop`] stored, never
        // freed or written through; its bytes live as long as the process and are only read.
        let line: &'static str = unsafe { *said };
        // SAFETY: `write` reads `line.len()` bytes from `line`, which holds them.
        let _ = unsafe { libc::write(libc::STDERR_FILENO, line.as_ptr().cast(), line.len()) };
    }
    if sig == libc::SIGQUIT {
        // SAFETY: `_exit` takes an integer and ends the process at once.
        unsafe { libc::_exit(128 + sig) };
    }
    // SAFETY: as in `Owners::select`.
    let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
    default.sa_sigaction = libc::SIG_DFL;
    // SAFETY: the default disposition for `sig`; nothing reads the one it replaces.
    let _ = unsafe { libc::sigaction(sig, &default, std::ptr::null_mut()) };
    #[expect(
        clippy::disallowed_methods,
        reason = "the stop sent again to its default disposition, which for SIGINT, SIGTERM and \
                  SIGHUP dumps no core; SIGQUIT exits above"
    )]
    // SAFETY: `raise` takes an integer and touches no memory of ours.
    let _ = unsafe { libc::raise(sig) };
}

/// [`say_on_stop`]'s logical owner, underneath any live catches.
struct Saying;

impl Drop for Saying {
    fn drop(&mut self) {
        let mut owners = OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.sayings -= 1;
        if owners.catches == 0 && owners.sayings == 0 {
            owners.select();
        }
    }
}

/// Until the returned handle drops, a stop ([`STOPS`]) writes `line` (its newline included) to
/// standard error and then ends the process as that stop would have. A signal this process
/// ignores already stays ignored, as [`catch`] leaves it. A catch armed inside it
/// ([`super::System::catch_stops`], [`super::System::defer_stops`]) replaces it while it lives
/// and puts it back as it drops, so the stop a deferral sends again says the line too.
pub fn say_on_stop(line: &'static str) -> Box<dyn std::any::Any> {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // One leaked reference per arm (a process arms it once), so the handler reads the line
    // through one atomic pointer, never a pointer and a length a second arm could split.
    SAID.store(Box::into_raw(Box::new(line)), Ordering::SeqCst);
    SPOKEN.store(false, Ordering::SeqCst);
    owners.sayings += 1;
    if owners.catches == 0 && owners.sayings == 1 {
        owners.select();
    }
    Box::new(Saying)
}

/// Set `sig`'s disposition to `action` unless this process ignores it: the one it replaced, or
/// `None` where it is ignored or could not be set.
fn replace_unless_ignored(
    sig: c_int,
    action: &libc::sigaction,
) -> (c_int, Option<libc::sigaction>) {
    // SAFETY: as in `Owners::select`.
    let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: reads `sig`'s action only.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &mut was) } != 0
        || was.sa_sigaction == libc::SIG_IGN
    {
        return (sig, None);
    }
    // SAFETY: a valid disposition for `sig`; the one it replaces is `was`, read above.
    let set = unsafe { libc::sigaction(sig, action, std::ptr::null_mut()) } == 0;
    (sig, set.then_some(was))
}

/// Put back each disposition [`replace_unless_ignored`] replaced.
fn put_back(was: &[(c_int, Option<libc::sigaction>)]) {
    for (sig, was) in was {
        if let Some(was) = was {
            // SAFETY: puts back the whole action `replace_unless_ignored` replaced for `sig`.
            unsafe { libc::sigaction(*sig, was, std::ptr::null_mut()) };
        }
    }
}

struct Catching;

impl Drop for Catching {
    fn drop(&mut self) {
        let mut owners = OWNERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.catches -= 1;
        if owners.catches != 0 {
            return;
        }
        owners.select();
        // Read once every disposition is back: a stop noted before its own came back is here,
        // and one after it met that disposition itself.
        let noted = CAUGHT.swap(0, Ordering::SeqCst);
        if std::mem::take(&mut owners.resend) && noted != 0 {
            // Keep lifecycle ownership through the resend: a new catch cannot clear the
            // noted stop or intercept it between restoration and delivery. Neither signal
            // handler accesses this mutex.
            resend(noted);
        }
    }
}

/// Catch [`STOPS`] until the last overlapping handle drops, then forget them unless an owner
/// deferred them. A signal this process ignores already stays ignored: whoever started it asked
/// it to outlive that signal (a hang-up under `nohup`), so it is no stop.
pub(super) fn catch() -> Box<dyn std::any::Any> {
    arm(false)
}

/// Catch [`STOPS`] as [`catch`] does, and once every overlapping catch has finished and the
/// underlying dispositions are back, send the one noted again: the run finishes its steps and
/// cleanup, then stops as it would have.
pub(super) fn defer() -> Box<dyn std::any::Any> {
    arm(true)
}

/// Send `sig` to this process again, now that its own disposition is back: a default SIGINT,
/// SIGTERM or SIGHUP ends it as the signal would have, and a handler runs. A default SIGQUIT would
/// dump core and write a crash report, so the process exits with the shell's status for it.
fn resend(sig: c_int) {
    if sig == libc::SIGQUIT && disposition(sig) == libc::SIG_DFL {
        std::process::exit(128 + sig);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a deferred stop sent to the process's own disposition, which for SIGINT, \
                  SIGTERM and SIGHUP dumps no core; a default SIGQUIT exits above"
    )]
    // SAFETY: `raise` takes an integer and touches no memory of ours.
    let _ = unsafe { libc::raise(sig) };
}

/// `sig`'s disposition now (`SIG_DFL` when it cannot be read).
fn disposition(sig: c_int) -> libc::sighandler_t {
    // SAFETY: as in `Owners::select`.
    let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: reads the action only.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &mut now) } != 0 {
        return libc::SIG_DFL;
    }
    now.sa_sigaction
}

fn arm(resend: bool) -> Box<dyn std::any::Any> {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    owners.catches += 1;
    owners.resend |= resend;
    if owners.catches == 1 {
        CAUGHT.store(0, Ordering::SeqCst);
        owners.select();
    }
    Box::new(Catching)
}

/// Set SIGHUP and SIGPIPE to `SIG_IGN` for good ([`super::System::ignore_hangups`]).
pub(super) fn ignore_hangups() {
    let mut owners = OWNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for sig in [libc::SIGHUP, libc::SIGPIPE] {
        // SAFETY: as in `Owners::select`.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = libc::SIG_IGN;
        // SAFETY: a valid disposition for `sig`; the one it replaces is not wanted back.
        let ignored = unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } == 0;
        if ignored && let Some(was) = &mut owners.was {
            // This is permanent, including after owners that predate detachment finish.
            if let Some((_, previous)) = was.iter_mut().find(|(saved, _)| *saved == sig) {
                *previous = None;
            }
        }
    }
}

/// The first signal [`catch`] or [`defer`] noted, while its handle lives.
pub(super) fn caught() -> Option<i32> {
    match CAUGHT.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}

/// `SIGINT`, `SIGTERM`, `SIGHUP` or `SIGQUIT` for one of [`STOPS`]; `signal <n>` otherwise.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        libc::SIGINT => "SIGINT".into(),
        libc::SIGTERM => "SIGTERM".into(),
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        n => format!("signal {n}"),
    }
}

#[cfg(test)]
#[path = "stops_tests.rs"]
mod tests;
