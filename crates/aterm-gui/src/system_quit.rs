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

use std::sync::atomic::{AtomicBool, Ordering};

/// `keyAEQuitReason`, `'why?'`.
pub(crate) const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

/// The quit reasons that are the system's: `kAELogOut` (`logo`),
/// `kAEReallyLogOut` (`rlgo`), `kAEShowRestartDialog` (`rrst`), `kAERestart`
/// (`rest`), `kAEShowShutdownDialog` (`rsdn`), `kAEShutDown` (`shut`).
const SYSTEM_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];

static SYSTEM_QUIT: AtomicBool = AtomicBool::new(false);

/// Whether quit reason `code` is a logout, a restart or a shutdown.
pub(crate) fn reason_is_the_systems(code: u32) -> bool {
    SYSTEM_REASONS
        .iter()
        .any(|reason| u32::from_be_bytes(*reason) == code)
}

/// The system asked this process to quit.
pub(crate) fn note() {
    SYSTEM_QUIT.store(true, Ordering::SeqCst);
}

/// Whether the system asked this process to quit.
pub(crate) fn asked() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst)
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
}
