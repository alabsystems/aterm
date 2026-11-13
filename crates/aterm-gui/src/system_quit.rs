// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quit the system asked for (a logout, a restart or a shutdown), the quit System Settings
//! asked for to reopen aterm ([`crate::quit_reopen`]), and the end of a failed event loop, after
//! which the agents aterm hosted relaunch at the next start, with the status a run ends with
//! ([`run_end_status`]). The first two are the person's
//! answer already, so aterm puts no question of its own before them ([`explicit`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_window::error::EventLoopError;

/// `keyAEQuitReason`, `'why?'`.
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

/// The quit reasons that are the system's.
#[cfg(any(target_os = "macos", test))]
const SYSTEM_REASONS: [[u8; 4]; 6] = [*b"logo", *b"rlgo", *b"rrst", *b"rest", *b"rsdn", *b"shut"];

/// Who asked for a `terminate:`.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asker {
    /// A person (the menu, a key, the Dock), or a program with no claim to more.
    Person,
    /// The system: a logout, a restart or a shutdown.
    System,
    /// System Settings' Quit & Reopen, sent once a person turned an access on for aterm.
    Reopen,
}

static SYSTEM_QUIT: AtomicU64 = AtomicU64::new(0);

static REOPEN_QUIT: AtomicU64 = AtomicU64::new(0);

static EVENT_LOOP_FAILED: AtomicBool = AtomicBool::new(false);

/// The exit status of a run whose window system went away under it.
pub(crate) const EXIT_WINDOW_SYSTEM_GONE: i32 = 69;

/// The exit status of a run whose event loop failed some other way.
pub(crate) const EXIT_EVENT_LOOP_FAILED: i32 = 70;

/// The exit status of a run whose first window surface could not be created (sysexits'
/// `EX_CANTCREAT`, "cannot create output"). A handoff parent books it, as any failing exit,
/// against the bytes it launched (`PhysicalFailureShape::successor_failed_to_start`), where exit 0
/// reads as the successor's ordinary refusal and marks nothing.
pub(crate) const EXIT_FIRST_SURFACE_FAILED: i32 = 73;

/// The status a run ends with: its failed event loop's own ([`EXIT_WINDOW_SYSTEM_GONE`],
/// [`EXIT_EVENT_LOOP_FAILED`]) where `loop_failed` names one, else
/// [`EXIT_FIRST_SURFACE_FAILED`] where its first window surface could not be created, else 0.
pub(crate) const fn run_end_status(loop_failed: Option<i32>, first_surface_failed: bool) -> i32 {
    match loop_failed {
        Some(status) => status,
        None if first_surface_failed => EXIT_FIRST_SURFACE_FAILED,
        None => 0,
    }
}

/// What the event loop does once its first window's surface was attached, or could not be
/// created ([`crate::App::first_surface_attached`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstSurface {
    /// The surface is there: the run goes on.
    Attached,
    /// It could not be created, and the loop exits now.
    ExitNow,
    /// It could not be created, and the loop exits once the account drain lets it
    /// (`Continuation::StartupFailure`).
    ExitOnceDrained,
}

impl crate::App {
    /// The first window's surface was `attached`, or could not be created: then the run is marked
    /// to end with [`EXIT_FIRST_SURFACE_FAILED`] ([`crate::App::end_status`]), and the answer says
    /// whether the loop exits now or once the account drain lets it.
    pub(crate) fn first_surface_attached(&mut self, attached: bool) -> FirstSurface {
        if attached {
            return FirstSurface::Attached;
        }
        crate::logging::stderr_line!(
            "aterm-gui: could not create the initial window surface; exiting with status \
             {EXIT_FIRST_SURFACE_FAILED}"
        );
        self.first_surface_failed = true;
        if self.initial_surface_exit_ready() {
            FirstSurface::ExitNow
        } else {
            FirstSurface::ExitOnceDrained
        }
    }

    /// The status this run ends with ([`run_end_status`]), its event loop having failed with
    /// `loop_failed`'s status or not.
    pub(crate) const fn end_status(&self, loop_failed: Option<i32>) -> i32 {
        run_end_status(loop_failed, self.first_surface_failed)
    }
}

/// Whether quit reason `code` is a logout, a restart or a shutdown.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn reason_is_the_systems(code: u32) -> bool {
    SYSTEM_REASONS
        .iter()
        .any(|reason| u32::from_be_bytes(*reason) == code)
}

/// Who asked for a quit whose Apple event comes from the program at `sender` and carries the
/// quit reason `reason`. System Settings' Quit & Reopen gives a logout's reason (`rlgo`, measured
/// on macOS 27.0), so its sender decides first; a logout, a restart or a shutdown comes from
/// loginwindow.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn asker_of(reason: Option<u32>, sender: Option<&std::path::Path>) -> Asker {
    if sender.is_some_and(crate::quit_reopen::sender_reopens) {
        Asker::Reopen
    } else if reason.is_some_and(reason_is_the_systems) {
        Asker::System
    } else {
        Asker::Person
    }
}

/// `asker` asked this process to quit, through the `terminate:` of `generation`; a person's
/// quit marks nothing.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn note(generation: u64, asker: Asker) {
    let mark = match asker {
        Asker::Person => return,
        Asker::System => &SYSTEM_QUIT,
        Asker::Reopen => &REOPEN_QUIT,
    };
    if generation != 0 {
        mark.store(generation, Ordering::SeqCst);
    }
}

/// The quit of `generation` was cancelled.
pub(crate) fn forget(generation: u64) {
    for mark in [&SYSTEM_QUIT, &REOPEN_QUIT] {
        let _ = mark.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// The event loop ended with an error under this process, its window system gone or another.
pub(crate) fn note_event_loop_failed() {
    EVENT_LOOP_FAILED.store(true, Ordering::SeqCst);
}

/// Whether the system or System Settings' Quit & Reopen asked this process to quit, by a quit
/// nobody cancelled, or its event loop failed.
pub(crate) fn asked() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst) != 0
        || REOPEN_QUIT.load(Ordering::SeqCst) != 0
        || EVENT_LOOP_FAILED.load(Ordering::SeqCst)
}

/// Whether the quit of `generation` is one the system (a logout, a restart or a shutdown) or
/// System Settings' Quit & Reopen asked for, by a quit nobody cancelled: that ask is the person's
/// answer already (they chose to log out, or pressed Quit & Reopen), so aterm asks nothing more
/// of it (`App::native_termination_ready`: aterm asks no question it can answer itself). A
/// person's own quit (the menu, ⌘Q, the Dock) is not one.
#[cfg(unix)]
pub(crate) fn explicit(generation: u64) -> bool {
    generation != 0
        && (SYSTEM_QUIT.load(Ordering::SeqCst) == generation
            || REOPEN_QUIT.load(Ordering::SeqCst) == generation)
}

/// Whether the system asked this process to quit (a logout, a restart or a shutdown) by a quit
/// nobody cancelled: not System Settings' Quit & Reopen, whose quit reason is a logout's too
/// ([`asker_of`]), nor a failed event loop. What gives up this Mac's account claims at the end of
/// the run (`crate::harness_host::claims_off_as_the_system_quits`).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn by_the_system() -> bool {
    SYSTEM_QUIT.load(Ordering::SeqCst) != 0
}

/// Whether aterm opens again once this process is gone: System Settings' Quit & Reopen asked for
/// a quit nobody cancelled, and no logout, restart, shutdown or failed event loop is under it.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn reopens() -> bool {
    REOPEN_QUIT.load(Ordering::SeqCst) != 0
        && SYSTEM_QUIT.load(Ordering::SeqCst) == 0
        && !EVENT_LOOP_FAILED.load(Ordering::SeqCst)
}

/// How a run whose event loop ended with `error` ends.
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

/// No mark at all, for a test that starts from a fresh arbiter (`menu::terminate_test_serial`).
#[cfg(test)]
pub(crate) fn clear_for_test() {
    SYSTEM_QUIT.store(0, Ordering::SeqCst);
    REOPEN_QUIT.store(0, Ordering::SeqCst);
    EVENT_LOOP_FAILED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
#[path = "system_quit_tests.rs"]
mod tests;
