// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A QUIT THE SYSTEM ASKED FOR (2026-09-27, the owner: relaunch the agents
//! aterm hosted after an exit they did not choose — a crash, a kill, or a
//! restart or logout). A crash leaves the crash journal, which carries the
//! agents ([`crate::crash_journal`]). A restart, a logout or a shutdown quits
//! aterm the ordinary way instead, and the ordinary quit writes a manifest
//! that carries no agent — right for a person's own ⌘Q, wrong for these.
//! macOS says which it is: its quit Apple Event carries a `kAEQuitReason`
//! naming the logout, restart or shutdown, read where AppKit asks
//! `applicationShouldTerminate:` (`menu::terminate_is_the_systems`) and
//! noted here; the graceful exit then fills the agents in
//! ([`crate::restore::RestoreManifest::fill_agents`]).
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
//! loop ends with an error, and the run ends the way a logout ends it: the
//! ordinary quit, its manifest keeping the agents
//! ([`note_window_system_gone`]), then exit status
//! [`EXIT_WINDOW_SYSTEM_GONE`]. Nothing cancels it: there is no window left to
//! answer a Cancel in.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

/// The window system went away under this process ([`note_window_system_gone`]).
static WINDOW_SYSTEM_GONE: AtomicBool = AtomicBool::new(false);

/// The exit status of a run whose window system went away under it:
/// `EX_UNAVAILABLE` (sysexits), nonzero, so whatever started aterm sees an end
/// nobody chose.
pub(crate) const EXIT_WINDOW_SYSTEM_GONE: i32 = 69;

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

/// The window system went away under this process: its quit is the
/// system's, as a logout's is, for the rest of the run.
pub(crate) fn note_window_system_gone() {
    WINDOW_SYSTEM_GONE.store(true, Ordering::SeqCst);
}

/// Whether the system asked this process to quit, by a quit nobody cancelled,
/// or its window system went away.
pub(crate) fn asked() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst) != 0 || WINDOW_SYSTEM_GONE.load(Ordering::SeqCst)
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
    WINDOW_SYSTEM_GONE.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A logout, a restart and a shutdown, asked or confirmed, are the
    /// system's; no other code is — an AppleScript `quit` or a Dock Quit
    /// carries none, and `0` is what a missing reason reads as.
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

    /// A SYSTEM QUIT THAT DID NOT HAPPEN MARKS NO LATER QUIT (2026-09-28). A
    /// logout aterm answered Cancel to (the person's Cancel, a held document, or
    /// an update successor that had not taken over yet) left the bare flag set,
    /// so the person's own quit after it kept every agent for the next start.
    /// The mark now goes with its generation's cancel — through either cancel
    /// path — and a duplicate the system sends while a quit is pending marks
    /// that pending quit.
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

    /// A WINDOW SYSTEM THAT WENT AWAY MAKES THE QUIT THE SYSTEM'S, so the
    /// manifest keeps the agents as a logout's does, and no cancel of any
    /// generation unmarks it: no window is left to answer a Cancel in.
    #[test]
    fn a_gone_window_system_marks_the_quit_and_no_cancel_unmarks_it() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        assert!(!asked());
        note_window_system_gone();
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
}
