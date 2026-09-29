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

use std::sync::atomic::{AtomicU64, Ordering};

/// `keyAEQuitReason`, `'why?'`.
pub(crate) const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

/// The quit reasons that are the system's: `kAELogOut` (`logo`),
/// `kAEReallyLogOut` (`rlgo`), `kAEShowRestartDialog` (`rrst`), `kAERestart`
/// (`rest`), `kAEShowShutdownDialog` (`rsdn`), `kAEShutDown` (`shut`).
const SYSTEM_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];

/// The generation of the quit the system asked for, or `0` for none (the
/// arbiter mints generations from `1`).
static SYSTEM_QUIT: AtomicU64 = AtomicU64::new(0);

/// Whether quit reason `code` is a logout, a restart or a shutdown.
pub(crate) fn reason_is_the_systems(code: u32) -> bool {
    SYSTEM_REASONS
        .iter()
        .any(|reason| u32::from_be_bytes(*reason) == code)
}

/// The system asked this process to quit, through the `terminate:` of
/// `generation`. Only the macOS `terminate:` relay notes one.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
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

/// Whether the system asked this process to quit, by a quit nobody cancelled.
pub(crate) fn asked() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst) != 0
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
}
