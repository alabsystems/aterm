// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! AppKit's contract for a quit the application defers.
//!
//! `applicationShouldTerminate:` is synchronous, and an application that needs time before it
//! may quit (a question, saves, its own shutdown) answers `NSTerminateLater` and later calls
//! `-[NSApplication replyToApplicationShouldTerminate:]` once: YES lets the quit go on, and NO
//! refuses it. Measured in-process on macOS 27.2 (an `NSApplication` with no window and the
//! Prohibited activation policy, its delegate answering Later): AppKit then turns the run loop in
//! `NSModalPanelRunLoopMode` inside `terminate:` until the reply, firing the common-mode
//! observers and sources the event loop runs on; after YES the reply returns to its caller, that
//! loop unwinds, AppKit posts `applicationWillTerminate:` and calls `exit(0)`, so `-[NSApp run]`
//! never returns; after NO `terminate:` returns and the main loop goes on; `-[NSApp stop:]` inside
//! that loop ends nothing; and a second `terminate:` while the reply is owed is not put to the
//! delegate: AppKit terminates at once (`applicationWillTerminate:`, then `exit(0)`).
//! Cancelling instead (`NSTerminateCancel`) is what makes macOS abandon a logout, restart or
//! shutdown, by AppKit's documented contract; that is not measured here.
//!
//! So the loop's exit, while a reply is owed, happens inside AppKit: the application gets
//! `LoopExiting` there ([`exit_ends_in_appkit`] says so), its shutdown runs in it, and only then
//! does AppKit hear YES ([`exit_then_proceed`]). That exit ends the process whatever is cancelled
//! during it, so a refusal then is no refusal ([`refuse`]); only the event loop's panic guard
//! answers NO there ([`refuse_unwinding`]), for a shutdown that did not finish.
//!
//! The application's handler can answer only from AppKit's wait, so a `Later` needs the handler
//! free to run there. AppKit asking from inside the handler (a modal alert or panel the handler
//! runs, whose loop dispatched the quit) or with no event loop running leaves nothing that could
//! reply before `terminate:` returns, which it does only after a reply: that ask is
//! [`ReplyWait::Unanswerable`], and AppKit never hears `Later` for it.

use std::cell::Cell;

/// What `applicationShouldTerminate:` answers AppKit, as the application's handler decides
/// (`platform::macos::set_application_should_terminate_handler`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateAnswer {
    /// AppKit terminates now, without the application's own shutdown first.
    Now,
    /// The quit is refused at once (`NSTerminateCancel`); for a logout, restart or shutdown,
    /// macOS abandons it.
    Cancel,
    /// The application takes the quit up (`NSTerminateLater`) and AppKit waits for its one reply:
    /// YES once the event loop has exited (its `LoopExiting` delivered, the application's
    /// shutdown done in it), or NO from
    /// [`refuse_deferred_termination`](crate::platform::macos::refuse_deferred_termination).
    Later,
}

/// Whether the application could answer a `Later` to the quit AppKit asks about now, as its
/// handler is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyWait {
    /// The event loop runs and AppKit asked from outside the application's handler, which then
    /// runs in AppKit's wait: a `Later` can be answered.
    Answerable,
    /// AppKit asked from inside the application's handler, or with no event loop running: nothing
    /// can reply before `terminate:` returns, so a `Later` is answered `NSTerminateCancel`.
    Unanswerable,
}

impl TerminateAnswer {
    /// The `NSApplicationTerminateReply` AppKit reads: `NSTerminateCancel` = 0,
    /// `NSTerminateNow` = 1, `NSTerminateLater` = 2.
    const fn ns_reply(self) -> usize {
        match self {
            Self::Cancel => 0,
            Self::Now => 1,
            Self::Later => 2,
        }
    }
}

thread_local! {
    /// Whether AppKit waits on a reply to a `Later`. AppKit asks and is answered on the main
    /// thread, so on any other thread nothing is ever owed.
    static OWED: Cell<bool> = const { Cell::new(false) };
    /// Whether the event loop's exit is being delivered from inside AppKit's `terminate:`.
    static IN_APPKIT_EXIT: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// Each reply this thread would have sent AppKit, in order.
    static SENT: std::cell::RefCell<Vec<bool>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The value `applicationShouldTerminate:` returns for `answer` to an ask `wait` describes; a
/// `Later` owes AppKit one reply, and one nothing could answer is refused instead
/// (`NSTerminateCancel`), so AppKit never waits on a reply that cannot come. AppKit puts no second
/// `terminate:` to the delegate while a reply is owed (measured, above), so a `Later` never stacks
/// on another.
pub(super) fn should_terminate(answer: TerminateAnswer, wait: ReplyWait) -> usize {
    let answer = match (answer, wait) {
        (TerminateAnswer::Later, ReplyWait::Unanswerable) => {
            tracing::warn!(
                "a quit AppKit asked about where no reply could come was answered Later; refused"
            );
            TerminateAnswer::Cancel
        },
        (answer, _) => answer,
    };
    if answer == TerminateAnswer::Later {
        OWED.set(true);
    }
    answer.ns_reply()
}

/// Whether AppKit waits on a reply to a `Later`.
pub(super) fn reply_owed() -> bool {
    OWED.get()
}

/// Refuse the quit AppKit waits on: NO, once. False, sending nothing, when no reply is owed, and
/// while the event loop's exit is being delivered inside AppKit ([`exit_ends_in_appkit`]), the
/// handler work owed before it included: that exit closes the windows and runs the application's
/// shutdown, and it ends the process whatever is cancelled in it, so AppKit hears YES after it
/// ([`exit_then_proceed`]), or nothing once AppKit terminates by itself ([`appkit_terminates`]).
/// A NO there would leave a process with its windows closed and no event loop, inside a
/// `terminate:` that `-[NSApp stop:]` cannot end.
pub(crate) fn refuse() -> bool {
    if IN_APPKIT_EXIT.get() {
        if OWED.get() {
            tracing::warn!(
                "a quit was cancelled while its exit was under way inside AppKit: the exit ends \
                 the process, so macOS hears that the quit goes on"
            );
        }
        return false;
    }
    refuse_unwinding()
}

/// The event loop's panic guard's refusal (`event_loop::stop_app_on_panic`): NO, once, whenever a
/// reply is owed, the exit's delivery included, since a process going without its shutdown does
/// not tell macOS the quit went. False, sending nothing, when no reply is owed.
pub(crate) fn refuse_unwinding() -> bool {
    if !OWED.replace(false) {
        return false;
    }
    send_reply(false);
    true
}

/// The event loop's exit while AppKit waits on a reply: `deliver` (the windows' close and the
/// loop's `LoopExiting`, in which the application does its shutdown) runs first, inside AppKit
/// ([`exit_ends_in_appkit`]), and AppKit hears YES only after it, which ends the process once
/// AppKit's `terminate:` unwinds; a cancel during `deliver` does not refuse it ([`refuse`]).
/// Answers whether that YES was sent: false when no reply was owed (nothing runs then, and the
/// caller ends the run as usual, `-[NSApp stop:]`), or when the panic guard refused it during
/// `deliver` ([`refuse_unwinding`]), which also queued the stop that ends the run once AppKit's
/// `terminate:` returns, since `-[NSApp stop:]` inside it ends nothing.
pub(super) fn exit_then_proceed(deliver: impl FnOnce()) -> bool {
    if !OWED.get() {
        return false;
    }
    exit_in_appkit(deliver);
    if !OWED.replace(false) {
        return false;
    }
    send_reply(true);
    true
}

/// AppKit ends the process itself once this returns (`applicationWillTerminate:`, then
/// `exit(0)`), so a reply it waited on is moot from the start: it terminates without one, and
/// nothing in `deliver` can refuse it. `deliver` (the loop's exit) runs inside it
/// ([`exit_ends_in_appkit`]) unless `handler_busy`: AppKit terminating at once (a second
/// `terminate:` during its wait, measured) from inside the application's handler (a modal alert
/// or panel the handler runs) finds the handler on the stack, where a `LoopExiting` could not be
/// delivered and the process would abort; then the exit is left undelivered, and the process ends
/// without the application's shutdown.
pub(super) fn appkit_terminates(handler_busy: bool, deliver: impl FnOnce()) {
    OWED.set(false);
    if handler_busy {
        tracing::warn!(
            "AppKit terminates while the event handler runs (a modal alert or panel it opened): \
             the event loop's exit is not delivered, and the application's shutdown does not run"
        );
        return;
    }
    exit_in_appkit(deliver);
}

/// `deliver`, told it runs inside AppKit ([`exit_ends_in_appkit`]) until it returns or unwinds.
fn exit_in_appkit(deliver: impl FnOnce()) {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_APPKIT_EXIT.set(self.0);
        }
    }
    let _restore = Restore(IN_APPKIT_EXIT.replace(true));
    deliver();
}

/// Whether the event loop's exit being delivered now happens inside AppKit's `terminate:`, which
/// ends the process when it returns: `run_app` never returns, so what the application does after
/// it must be done in its `exiting`.
pub(crate) fn exit_ends_in_appkit() -> bool {
    IN_APPKIT_EXIT.get()
}

/// `-[NSApplication replyToApplicationShouldTerminate:]`.
fn send_reply(proceed: bool) {
    #[cfg(test)]
    SENT.with(|sent| sent.borrow_mut().push(proceed));
    #[cfg(not(test))]
    // SAFETY: `+sharedApplication` is `@16#0:8` on `NSApplication`, and a reply is owed only on
    // the main thread (`OWED`), where AppKit asked; `-replyToApplicationShouldTerminate:` is
    // `v20@0:8B16`.
    unsafe {
        let app = aterm_objc::send::send_id(
            aterm_objc::class(c"NSApplication").as_id(),
            aterm_objc::sel!(sharedApplication),
        );
        aterm_objc::send::send_v_bool(
            app,
            aterm_objc::sel!(replyToApplicationShouldTerminate:),
            proceed,
        );
    }
}

#[cfg(test)]
#[path = "terminate_tests.rs"]
mod tests;
