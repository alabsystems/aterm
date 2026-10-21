// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quit the system asked for: a logout, a restart or a shutdown, after which the agents
//! aterm hosted relaunch at the next start. A crash leaves the crash journal, which carries
//! the agents ([`crate::crash_journal`]); a system quit takes the ordinary path, whose
//! manifest carries no agent — right for a person's own ⌘Q, wrong here. macOS names the
//! reason in its quit Apple Event's `kAEQuitReason`, read where AppKit asks
//! `applicationShouldTerminate:` (`menu::terminate_is_the_systems`) and noted here; the
//! graceful exit then fills the agents in ([`crate::restore::RestoreManifest::fill_agents`]).
//!
//! THE MARK BELONGS TO ONE QUIT (2026-09-28). It was a bare flag that nothing
//! cleared, so a system quit that did NOT happen — the person answered Cancel,
//! a document held it, or an update successor that had not taken over yet
//! answered it cancelled — still marked the NEXT quit, a person's own ⌘Q, as
//! the system's, and the agents they had just quit came back at the next
//! start. The mark is now the generation of the `terminate:` it came with
//! (`menu`'s arbiter mints it), and every cancel of that generation forgets it
//! (`menu::cancel_native_termination`, `menu::cancel_current_native_termination`).
//!
//! THE WINDOW SYSTEM GOING AWAY IS ONE TOO (COMPOSITOR-GONE, the Linux lane,
//! 2026-10-16). When the display connection fails under a running window (a
//! Wayland compositor or an X server that crashed or restarted), the event
//! loop ends with [`EventLoopError::DisplayLost`], and the run ends the way a
//! logout ends it: the ordinary quit, its manifest keeping the agents
//! ([`note_event_loop_failed`]), then exit status
//! [`EXIT_WINDOW_SYSTEM_GONE`]. Nothing cancels it: there is no window left to
//! answer a Cancel in.
//!
//! ONLY THAT IS A LOGOUT (GONE-2, 2026-10-17). Any other error the event loop
//! ends with (a protocol error, a connection the client library closed, a
//! failed `poll`) is said as itself and exits [`EXIT_EVENT_LOOP_FAILED`]
//! ([`failed_loop_end`]); the person did not choose it either, so its quit
//! keeps the agents too. A person's own quit ends the loop without an error
//! and exits 0.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_window::error::EventLoopError;

/// `keyAEQuitReason`, `'why?'`. The quit reason is read only off macOS's quit
/// Apple Event (`menu::terminate_is_the_systems`).
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

/// The quit reasons that are the system's: `kAELogOut` (`logo`),
/// `kAEReallyLogOut` (`rlgo`), `kAEShowRestartDialog` (`rrst`), `kAERestart`
/// (`rest`), `kAEShowShutdownDialog` (`rsdn`), `kAEShutDown` (`shut`).
#[cfg(any(target_os = "macos", test))]
const SYSTEM_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];

/// The generation of the quit the system asked for, or `0` for none (the
/// arbiter mints generations from `1`).
static SYSTEM_QUIT: AtomicU64 = AtomicU64::new(0);

/// The event loop ended with an error under this process ([`note_event_loop_failed`]).
static EVENT_LOOP_FAILED: AtomicBool = AtomicBool::new(false);

/// The exit status of a run whose window system went away under it:
/// `EX_UNAVAILABLE` (sysexits), nonzero, so whatever started aterm sees an end
/// nobody chose.
pub(crate) const EXIT_WINDOW_SYSTEM_GONE: i32 = 69;

/// The exit status of a run whose event loop failed some other way:
/// `EX_SOFTWARE` (sysexits), aterm's own failure, not the display's.
pub(crate) const EXIT_EVENT_LOOP_FAILED: i32 = 70;

/// Whether quit reason `code` is a logout, a restart or a shutdown.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn reason_is_the_systems(code: u32) -> bool {
    SYSTEM_REASONS
        .iter()
        .any(|reason| u32::from_be_bytes(*reason) == code)
}

/// The system asked this process to quit, through the `terminate:` of
/// `generation`. Only the macOS `terminate:` relay notes one.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn note(generation: u64) {
    if generation != 0 {
        SYSTEM_QUIT.store(generation, Ordering::SeqCst);
    }
}

/// The quit of `generation` was cancelled: if it was the system's, it no longer
/// is, and no later quit inherits its mark. A cancel of any other generation
/// leaves the mark alone.
pub(crate) fn forget(generation: u64) {
    let _ = SYSTEM_QUIT.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// The event loop ended with an error under this process, its window system
/// gone or another: its quit is the system's, as a logout's is, for the rest
/// of the run.
pub(crate) fn note_event_loop_failed() {
    EVENT_LOOP_FAILED.store(true, Ordering::SeqCst);
}

/// Whether the system asked this process to quit, by a quit nobody cancelled,
/// or its event loop failed.
pub(crate) fn asked() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst) != 0 || EVENT_LOOP_FAILED.load(Ordering::SeqCst)
}

/// How a run whose event loop ended with `error` ends: the status it exits
/// with and the line that says why. Only the display connection's death
/// ([`EventLoopError::DisplayLost`]) ends it as a logout does, with
/// [`EXIT_WINDOW_SYSTEM_GONE`] and a line saying the display connection
/// ended; any other error is said as itself, with
/// [`EXIT_EVENT_LOOP_FAILED`].
pub(crate) fn failed_loop_end(error: &EventLoopError) -> (i32, String) {
    match error {
        EventLoopError::DisplayLost(why) => {
            let status = EXIT_WINDOW_SYSTEM_GONE;
            (
                status,
                format!(
                    "the display connection ended ({why}); quitting as a logout would, \
                     exit status {status}"
                ),
            )
        }
        other => {
            let status = EXIT_EVENT_LOOP_FAILED;
            (
                status,
                format!("the event loop failed ({other}); quitting, exit status {status}"),
            )
        }
    }
}

/// Whether the quit of `generation` is the system's.
#[cfg(test)]
pub(crate) fn asked_for(generation: u64) -> bool {
    generation != 0 && SYSTEM_QUIT.load(Ordering::SeqCst) == generation
}

/// No mark at all, for a test that starts from a fresh arbiter
/// (`menu::terminate_test_serial`).
#[cfg(test)]
pub(crate) fn clear_for_test() {
    SYSTEM_QUIT.store(0, Ordering::SeqCst);
    EVENT_LOOP_FAILED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only logout, restart and shutdown codes are the system's: an AppleScript
    /// `quit` or a Dock Quit carries no reason, which reads as `0`.
    #[test]
    fn only_a_logout_a_restart_or_a_shutdown_is_the_systems() {
        for r in [b"logo", b"rlgo", b"rrst", b"rest", b"rsdn", b"shut"] {
            assert!(reason_is_the_systems(u32::from_be_bytes(*r)), "{r:?}");
        }
        for other in [
            0,
            u32::from_be_bytes(*b"quit"),
            u32::from_be_bytes(*b"why?"),
        ] {
            assert!(!reason_is_the_systems(other), "{other:#x}");
        }
        assert_eq!(KEY_QUIT_REASON, 0x7768_793f);
    }

    /// A cancelled system quit marks no later quit, through either cancel path; a
    /// duplicate the system sends while a quit is pending marks that pending quit.
    #[test]
    fn a_cancelled_system_quit_does_not_mark_a_later_person_quit() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        let NativeTerminateDecision::Dispatch(logout) = admit_native_terminate(true) else {
            panic!("a first request dispatches");
        };
        assert!(asked() && asked_for(logout));
        assert!(crate::menu::cancel_current_native_termination());
        assert!(!asked(), "the cancelled logout left its mark");
        let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(false) else {
            panic!("the arbiter is free again");
        };
        assert_ne!(person, logout);
        assert!(!asked(), "the person's quit reads as the system's");

        // The system asks again while the person's quit is pending: the pending
        // quit is now the system's too, and its cancel forgets that.
        assert_eq!(
            admit_native_terminate(true),
            NativeTerminateDecision::DeferExisting(person)
        );
        assert!(asked_for(person));
        #[cfg(unix)]
        assert!(crate::menu::cancel_native_termination(person));
        #[cfg(not(unix))]
        assert!(crate::menu::cancel_current_native_termination());
        assert!(!asked());

        // A cancel of some other generation leaves a live mark alone.
        note(person + 100);
        forget(person);
        assert!(asked_for(person + 100));
        forget(person + 100);
        assert!(!asked());
    }

    /// A FAILED EVENT LOOP (a window system that went away, or another error)
    /// MAKES THE QUIT THE SYSTEM'S, so the manifest keeps the agents as a
    /// logout's does, and no cancel of any generation unmarks it: no window is
    /// left to answer a Cancel in.
    #[test]
    fn a_failed_event_loop_marks_the_quit_and_no_cancel_unmarks_it() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        assert!(!asked());
        note_event_loop_failed();
        assert!(asked());
        let NativeTerminateDecision::Dispatch(generation) = admit_native_terminate(false) else {
            panic!("a first request dispatches");
        };
        assert!(crate::menu::cancel_current_native_termination());
        forget(generation);
        assert!(asked(), "a cancel unmarked a gone window system's quit");
        clear_for_test();
        assert!(!asked());
    }

    /// ONLY A DEAD DISPLAY ENDS THE RUN AS A LOGOUT (GONE-2): the line that
    /// says the display connection ended, and 69. Every other error the loop
    /// can end with (the X11 error at a quit's last sync was one, until the
    /// backend kept it out of the run's end) is said as itself, with 70: the
    /// negative control, which COMPOSITOR-GONE's mapping of every error to
    /// the logout failed.
    #[test]
    fn only_a_dead_display_ends_the_run_as_a_logout() {
        let (status, line) = failed_loop_end(&EventLoopError::DisplayLost(
            std::io::ErrorKind::ConnectionReset.into(),
        ));
        assert_eq!(status, EXIT_WINDOW_SYSTEM_GONE);
        assert!(line.starts_with("the display connection ended ("), "{line}");
        assert!(
            line.ends_with("quitting as a logout would, exit status 69"),
            "{line}"
        );
        for other in [
            EventLoopError::ExitFailure(1),
            // Linux's `EPROTO`, a protocol error's errno.
            EventLoopError::ExitFailure(71),
            EventLoopError::RecreationAttempt,
        ] {
            let (status, line) = failed_loop_end(&other);
            assert_eq!(status, EXIT_EVENT_LOOP_FAILED, "{other}");
            assert!(!line.contains("display connection ended"), "{line}");
            assert!(line.starts_with("the event loop failed ("), "{line}");
        }
    }
}
