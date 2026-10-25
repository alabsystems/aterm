// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quit the system asked for (a logout, a restart or a shutdown), the quit System Settings
//! asked for to reopen aterm ([`crate::quit_reopen`]), and the end of a failed event loop, after
//! which the agents aterm hosted relaunch at the next start.

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
mod tests {
    use super::*;

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

    #[test]
    fn a_cancelled_system_quit_does_not_mark_a_later_person_quit() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        let NativeTerminateDecision::Dispatch(logout) = admit_native_terminate(Asker::System)
        else {
            panic!("a first request dispatches");
        };
        assert!(asked() && asked_for(logout));
        assert!(crate::menu::cancel_current_native_termination());
        assert!(!asked(), "the cancelled logout left its mark");
        let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person)
        else {
            panic!("the arbiter is free again");
        };
        assert_ne!(person, logout);
        assert!(!asked(), "the person's quit reads as the system's");

        assert_eq!(
            admit_native_terminate(Asker::System),
            NativeTerminateDecision::DeferExisting(person)
        );
        assert!(asked_for(person));
        #[cfg(unix)]
        assert!(crate::menu::cancel_native_termination(person));
        #[cfg(not(unix))]
        assert!(crate::menu::cancel_current_native_termination());
        assert!(!asked());

        note(person + 100, Asker::System);
        forget(person);
        assert!(asked_for(person + 100));
        forget(person + 100);
        assert!(!asked());
    }

    #[test]
    fn a_failed_event_loop_marks_the_quit_and_no_cancel_unmarks_it() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        assert!(!asked());
        note_event_loop_failed();
        assert!(asked());
        let NativeTerminateDecision::Dispatch(generation) = admit_native_terminate(Asker::Person)
        else {
            panic!("a first request dispatches");
        };
        assert!(crate::menu::cancel_current_native_termination());
        forget(generation);
        assert!(asked(), "a cancel unmarked a gone window system's quit");
        clear_for_test();
        assert!(!asked());
    }

    #[test]
    fn a_quit_and_reopen_reopens_only_while_nothing_cancelled_or_outranks_it() {
        use crate::menu::{NativeTerminateDecision, admit_native_terminate};
        let _serial = crate::menu::terminate_test_serial();
        let NativeTerminateDecision::Dispatch(reopen) = admit_native_terminate(Asker::Reopen)
        else {
            panic!("a first request dispatches");
        };
        assert!(
            reopens() && asked(),
            "System Settings' quit neither reopens nor keeps agents"
        );
        assert!(crate::menu::cancel_current_native_termination());
        assert!(
            !reopens() && !asked(),
            "the cancelled Quit & Reopen left its mark"
        );

        let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person)
        else {
            panic!("the arbiter is free again");
        };
        assert_ne!(person, reopen);
        assert!(!reopens(), "a person's quit reopens");
        assert_eq!(
            admit_native_terminate(Asker::Reopen),
            NativeTerminateDecision::DeferExisting(person)
        );
        assert!(
            reopens(),
            "System Settings' quit onto a pending one does not reopen"
        );
        note(person, Asker::System);
        assert!(
            !reopens() && asked(),
            "a logout under a Quit & Reopen reopens"
        );
        forget(person);
        assert!(!reopens() && !asked());

        note(person + 100, Asker::Reopen);
        forget(person);
        assert!(reopens(), "a cancel of another quit unmarked this one");
        note_event_loop_failed();
        assert!(!reopens(), "a gone window system's quit reopens");
        clear_for_test();
        assert!(!reopens() && !asked());
    }

    #[test]
    fn system_settings_quit_is_a_reopen_whatever_reason_it_gives() {
        use std::path::Path;
        let settings = Path::new(
            "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex/Contents/\
             MacOS/SecurityPrivacyExtension",
        );
        let loginwindow =
            Path::new("/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow");
        let dock = Path::new("/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock");
        let reason = |code: &[u8; 4]| Some(u32::from_be_bytes(*code));
        assert_eq!(asker_of(reason(b"rlgo"), Some(settings)), Asker::Reopen);
        assert_eq!(asker_of(reason(b"quit"), Some(settings)), Asker::Reopen);
        assert_eq!(asker_of(None, Some(settings)), Asker::Reopen);
        assert_eq!(asker_of(reason(b"rlgo"), Some(loginwindow)), Asker::System);
        assert_eq!(asker_of(reason(b"shut"), None), Asker::System);
        assert_eq!(asker_of(reason(b"quit"), Some(dock)), Asker::Person);
        assert_eq!(asker_of(None, None), Asker::Person);
    }

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
