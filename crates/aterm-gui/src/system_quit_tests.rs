// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let NativeTerminateDecision::Dispatch(logout) = admit_native_terminate(Asker::System) else {
        panic!("a first request dispatches");
    };
    assert!(asked() && asked_for(logout));
    assert!(crate::menu::cancel_current_native_termination());
    assert!(!asked(), "the cancelled logout left its mark");
    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
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
    let NativeTerminateDecision::Dispatch(reopen) = admit_native_terminate(Asker::Reopen) else {
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

    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
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

/// The quit the system or System Settings' Quit & Reopen asked for is explicit, its own
/// generation only, until it is cancelled; a person's quit is not, nor is a failed event loop
/// (which marks no generation), and a Quit & Reopen sent onto the person's pending quit makes
/// that quit explicit.
///
/// NEGATIVE CONTROL: make `explicit` answer `asked()`, and another generation and the failed
/// event loop read as explicit.
#[cfg(unix)]
#[test]
#[cfg(unix)]
fn only_the_system_or_quit_and_reopen_asks_an_explicit_quit() {
    use crate::menu::{NativeTerminateDecision, admit_native_terminate};
    let _serial = crate::menu::terminate_test_serial();
    for asker in [Asker::Reopen, Asker::System] {
        let NativeTerminateDecision::Dispatch(generation) = admit_native_terminate(asker) else {
            panic!("a first request dispatches");
        };
        assert!(explicit(generation), "{asker:?}");
        assert!(!explicit(generation + 1), "{asker:?}: another generation");
        assert!(!explicit(0), "{asker:?}: no generation");
        assert!(crate::menu::cancel_current_native_termination());
        assert!(!explicit(generation), "{asker:?}: cancelled");
    }
    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
        panic!("the arbiter is free again");
    };
    assert!(!explicit(person), "a person's quit");
    assert_eq!(
        admit_native_terminate(Asker::Reopen),
        NativeTerminateDecision::DeferExisting(person)
    );
    assert!(explicit(person), "Quit & Reopen onto the person's quit");
    assert!(crate::menu::cancel_current_native_termination());
    note_event_loop_failed();
    assert!(asked() && !explicit(person), "a failed event loop");
    clear_for_test();
}

/// Only the system's quit nobody cancelled gives up this Mac's account claims at the end of the
/// run: a person's quit, System Settings' Quit & Reopen (its logout reason notwithstanding),
/// a failed event loop and a cancelled logout do not.
///
/// NEGATIVE CONTROL: make `by_the_system` answer `asked()`, and the Quit & Reopen and the
/// failed event loop read as the system's.
#[test]
fn only_the_systems_quit_nobody_cancelled_is_by_the_system() {
    use crate::menu::{NativeTerminateDecision, admit_native_terminate};
    let _serial = crate::menu::terminate_test_serial();
    let NativeTerminateDecision::Dispatch(person) = admit_native_terminate(Asker::Person) else {
        panic!("a first request dispatches");
    };
    assert!(!by_the_system(), "a person's quit");
    assert!(crate::menu::cancel_current_native_termination());

    let NativeTerminateDecision::Dispatch(reopen) = admit_native_terminate(Asker::Reopen) else {
        panic!("the arbiter is free again");
    };
    assert!(
        asked() && !by_the_system(),
        "System Settings' Quit & Reopen"
    );
    assert!(crate::menu::cancel_current_native_termination());

    let NativeTerminateDecision::Dispatch(logout) = admit_native_terminate(Asker::System) else {
        panic!("the arbiter is free again");
    };
    assert_ne!(logout, reopen);
    assert_ne!(logout, person);
    assert!(by_the_system(), "the logout");
    assert!(crate::menu::cancel_current_native_termination());
    assert!(!by_the_system(), "the cancelled logout");

    note_event_loop_failed();
    assert!(asked() && !by_the_system(), "a failed event loop");
    clear_for_test();
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

/// A RUN WHOSE FIRST SURFACE FAILED ENDS WITH ITS OWN STATUS: a successor whose first window
/// surface could not be created ended its loop and exited 0, the status of its ordinary refusal,
/// so the handoff parent that launched it marked nothing against those bytes. The event loop's
/// answer to a failed attach (`App::first_surface_attached`, all `resumed` does with it) now marks
/// the run, and the status `main_entry_with_network` exits with (`App::end_status`) is
/// `EXIT_FIRST_SURFACE_FAILED`, a failing exit the parent books; a failed loop's own status still
/// wins, and a run whose surface was attached ends 0 and goes on.
/// NEGATIVE CONTROL: an attach failure that leaves the run unmarked, or an end status that reads
/// 0 for it, fails "THE SURFACE".
#[test]
fn a_run_whose_first_surface_failed_ends_with_its_own_status() {
    let mut app = crate::App::headless_for_test();
    assert_eq!(
        app.first_surface_attached(true),
        FirstSurface::Attached,
        "CONTROL: an attached surface goes on"
    );
    assert_eq!(
        app.end_status(None),
        0,
        "CONTROL: a run that failed nothing"
    );
    assert_ne!(
        app.first_surface_attached(false),
        FirstSurface::Attached,
        "a failed attach ends the loop, now or once the accounts drain"
    );
    assert_eq!(
        app.end_status(None),
        EXIT_FIRST_SURFACE_FAILED,
        "THE SURFACE: a run whose first surface could not be created says so in its status"
    );
    for failed in [EXIT_WINDOW_SYSTEM_GONE, EXIT_EVENT_LOOP_FAILED] {
        assert_eq!(
            app.end_status(Some(failed)),
            failed,
            "CONTROL: a failed event loop's own status wins"
        );
    }
}
