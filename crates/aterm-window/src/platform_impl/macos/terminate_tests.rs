// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

/// A fresh thread's state, and what it sent AppKit by the end of `run`.
fn on_fresh_thread(run: impl FnOnce() + Send + 'static) -> Vec<bool> {
    std::thread::spawn(move || {
        run();
        SENT.with(|sent| sent.borrow().clone())
    })
    .join()
    .expect("the test thread")
}

/// A quit taken up answers `NSTerminateLater` and owes AppKit a reply; one answered now or
/// cancelled owes none.
///
/// NEGATIVE CONTROL: the hook's old bool answer gave AppKit `NSTerminateCancel` (0) for
/// every quit aterm took up, so a logout, restart or shutdown was refused.
#[test]
fn a_quit_taken_up_waits_on_one_reply_and_others_owe_none() {
    let sent = on_fresh_thread(|| {
        assert_eq!(should_terminate(TerminateAnswer::Cancel, ReplyWait::Answerable), 0);
        assert!(!reply_owed());
        assert_eq!(should_terminate(TerminateAnswer::Now, ReplyWait::Answerable), 1);
        assert!(!reply_owed());
        assert!(!exit_then_proceed(|| panic!("no reply owed: nothing runs")));
        assert!(!refuse(), "nothing owed, nothing refused");
        assert_eq!(should_terminate(TerminateAnswer::Later, ReplyWait::Answerable), 2);
        assert!(reply_owed());
    });
    assert!(sent.is_empty(), "no reply before the quit ends: {sent:?}");
}

/// The loop's exit while AppKit waits: the application's exit runs first, told it is inside
/// AppKit, with no reply sent yet; AppKit hears YES once, after it; nothing is sent again.
#[test]
fn the_shutdown_runs_before_the_one_yes() {
    let sent = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        let ran = Cell::new(false);
        assert!(exit_then_proceed(|| {
            assert!(exit_ends_in_appkit(), "the application is told AppKit ends the process");
            assert!(SENT.with(|sent| sent.borrow().is_empty()), "no reply before the shutdown");
            ran.set(true);
        }));
        assert!(ran.get());
        assert!(!exit_ends_in_appkit(), "only while the exit is delivered");
        assert!(!reply_owed());
        assert!(!exit_then_proceed(|| panic!("answered once")));
        assert!(!refuse(), "a reply sent is not sent again");
    });
    assert_eq!(sent, [true]);
}

/// A refused quit hears NO once; the loop's exit after it owes AppKit nothing and leaves the
/// run to end as usual.
#[test]
fn a_refused_quit_hears_one_no() {
    let sent = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        assert!(refuse());
        assert!(!refuse());
        assert!(!exit_then_proceed(|| panic!("nothing owed after the NO")));
    });
    assert_eq!(sent, [false]);
}

/// A cancel from inside the application's exit (a question dropped with its window, say)
/// refuses nothing: that exit ends the process, so AppKit hears YES after it, once.
///
/// NEGATIVE CONTROL: let `refuse` answer NO during the exit (the code before this test) and
/// AppKit hears NO while the exit, its windows closed and its shutdown done, is under way;
/// the caller's `-[NSApp stop:]` then ends nothing inside AppKit's `terminate:`, and the
/// process lingers with no event loop.
#[test]
fn a_cancel_during_the_exit_does_not_refuse_it() {
    let sent = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        assert!(exit_then_proceed(|| {
            assert!(!refuse(), "the exit under way is not refused");
            assert!(reply_owed(), "AppKit still waits on the exit's YES");
        }));
        assert!(!reply_owed());
    });
    assert_eq!(sent, [true]);
}

/// AppKit's own termination (a `terminate:` it carried out without asking, or one answered
/// now) delivers the exit inside it and owes no reply from the start, so nothing in that exit
/// refuses it; with the application's handler on the stack (AppKit terminating from inside a
/// modal alert or panel the handler runs) the exit is not delivered at all.
///
/// NEGATIVE CONTROL: deliver the exit whether or not the handler is busy (the code before
/// this test) and `applicationWillTerminate:` handles `LoopExiting` on a borrowed handler,
/// which panics inside a method that aborts on one.
#[test]
fn appkits_own_termination_owes_no_reply() {
    let sent = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        let ran = Cell::new(false);
        appkit_terminates(false, || {
            assert!(exit_ends_in_appkit());
            assert!(!reply_owed(), "AppKit terminates without a reply");
            assert!(!refuse() && !refuse_unwinding(), "nothing to refuse");
            ran.set(true);
        });
        assert!(ran.get());
        assert!(!reply_owed());
        assert!(!exit_ends_in_appkit());

        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        appkit_terminates(true, || panic!("the busy handler is not handed the exit"));
        assert!(!reply_owed());
        assert!(!exit_ends_in_appkit());
    });
    assert!(sent.is_empty(), "{sent:?}");
}

/// An ask nothing could answer from AppKit's wait (from inside the application's handler, or
/// with no event loop running) is never waited on: a `Later` for it is refused, and AppKit is
/// owed nothing.
///
/// NEGATIVE CONTROL: answer it `NSTerminateLater` and AppKit waits inside the handler for a
/// reply only that handler could send, so the quit, a logout under it, and every window hang.
#[test]
fn an_ask_nothing_could_answer_is_never_waited_on() {
    let sent = on_fresh_thread(|| {
        assert_eq!(should_terminate(TerminateAnswer::Later, ReplyWait::Unanswerable), 0);
        assert!(!reply_owed());
        assert_eq!(should_terminate(TerminateAnswer::Now, ReplyWait::Unanswerable), 1);
        assert_eq!(should_terminate(TerminateAnswer::Cancel, ReplyWait::Unanswerable), 0);
        assert!(!reply_owed());
        assert!(!exit_then_proceed(|| panic!("no reply owed: nothing runs")));
    });
    assert!(sent.is_empty(), "{sent:?}");
}

/// A shutdown that panics inside AppKit gets one reply, NO, since the process goes without its
/// shutdown: the panic guard, which catches it inside the exit, refuses the quit there and no
/// YES follows; a panic that unwinds out of the exit leaves the reply owed and the exit no
/// longer marked as inside AppKit, and the guard's NO is still the one reply.
///
/// NEGATIVE CONTROL: send YES before `deliver` and a panicking shutdown has already told
/// macOS the quit went; have the guard refuse through `refuse` and its NO during the exit is
/// swallowed, so AppKit hears YES from a process whose shutdown did not finish.
#[test]
fn a_shutdown_that_panics_leaves_appkit_one_no() {
    let caught = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        assert!(!exit_then_proceed(|| assert!(refuse_unwinding(), "the guard's NO")));
        assert!(!reply_owed());
        assert!(!refuse_unwinding(), "one NO");
    });
    assert_eq!(caught, [false]);

    let unwound = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        let unwound =
            std::panic::catch_unwind(|| exit_then_proceed(|| panic!("the shutdown panicked")));
        assert!(unwound.is_err());
        assert!(!exit_ends_in_appkit(), "the unwind leaves AppKit's exit");
        assert!(reply_owed(), "no YES for a shutdown that did not finish");
        assert!(refuse_unwinding(), "the panic guard's NO");
        assert!(!refuse_unwinding());
    });
    assert_eq!(unwound, [false]);
}

/// Off the main thread AppKit waits on nothing this thread could answer.
#[test]
fn another_thread_owes_nothing() {
    let sent = on_fresh_thread(|| {
        let _ = should_terminate(TerminateAnswer::Later, ReplyWait::Answerable);
        let other = std::thread::spawn(refuse).join().expect("the other thread");
        assert!(!other, "the reply belongs to the thread AppKit asked on");
        assert!(reply_owed());
    });
    assert!(sent.is_empty(), "{sent:?}");
}
