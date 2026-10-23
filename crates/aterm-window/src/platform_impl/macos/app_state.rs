// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/app_state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::cell::{Cell, RefCell};
use std::mem;
use std::rc::Weak;
use std::sync::OnceLock;
use std::time::Instant;

use aterm_objc::send::{
    send_bool, send_bool_isize, send_id, send_id_usize, send_usize, send_v_bool, send_v_id,
};
use aterm_objc::{autoreleasepool, class, sel, Bool, Id, MainThread};

static QUIT_CONFIRM_HOOK: OnceLock<fn() -> bool> = OnceLock::new();

/// Registers a handler consulted before the app terminates via AppKit's `terminate:`.
pub(crate) fn set_quit_confirm_hook(hook: fn() -> bool) {
    let _ = QUIT_CONFIRM_HOOK.set(hook);
}

fn quit_confirm_allows() -> bool {
    QUIT_CONFIRM_HOOK.get().is_none_or(|hook| hook())
}

use super::aterm_objc_seam::consts::NS_APPLICATION_ACTIVATION_POLICY_REGULAR;
use super::event_handler::EventHandler;
use super::event_loop::{notify_windows_of_exit, stop_app_immediately, ActiveEventLoop, PanicInfo};
use super::observer::{EventLoopWaker, RunLoop};
use super::{menu, wake_queue, WindowId};
use crate::event::{Event, StartCause, WindowEvent};
use crate::event_loop::{ActiveEventLoop as RootActiveEventLoop, ControlFlow};
use crate::window::WindowId as RootWindowId;

#[derive(Debug)]
pub(super) struct AppState {
    /// LOCAL PATCH (aterm): the raw `NSApplicationActivationPolicy`, which is
    /// an `NSInteger` — see the seam's `NS_APPLICATION_ACTIVATION_POLICY_*`.
    activation_policy: Option<isize>,
    activate_ignoring_other_apps: bool,
    run_loop: RunLoop,
    event_handler: EventHandler,
    stop_on_launch: Cell<bool>,
    stop_before_wait: Cell<bool>,
    stop_after_wait: Cell<bool>,
    stop_on_redraw: Cell<bool>,
    /// Whether `applicationDidFinishLaunching:` has been run or not.
    is_launched: Cell<bool>,
    /// Whether an `EventLoop` is currently running.
    is_running: Cell<bool>,
    /// Whether the user has requested the event loop to exit.
    exit: Cell<bool>,
    control_flow: Cell<ControlFlow>,
    waker: RefCell<EventLoopWaker>,
    start_time: Cell<Option<Instant>>,
    wait_timeout: Cell<Option<Instant>>,
    pending_redraw: RefCell<Vec<WindowId>>,
    /// LOCAL PATCH (aterm), 2026-10-17: the proxies' wake flag, read at the
    /// end of each `cleared` (`wake_queue.rs`, COALESCED WAKES).
    wake_armed: wake_queue::Armed,
}

aterm_objc::declare_class! {
    /// The `NSApplicationDelegate` installed on `NSApp` and owner of the event-loop state.
    #[derive(Debug)]
    pub(super) struct ApplicationDelegate: NSObject {
        const NAME: &str = "WinitApplicationDelegate";
        type Ivars = AppState;
        protocols: [NSObject, NSApplicationDelegate];

        @sel(applicationDidFinishLaunching:) @abort_on_exception
        fn app_did_finish_launching(&self, _notification: Id) {
            self.did_finish_launching()
        }

        @sel(applicationWillTerminate:) @abort_on_exception
        fn app_will_terminate(&self, _notification: Id) {
            self.will_terminate()
        }

        @sel(applicationShouldTerminate:)
        fn app_should_terminate(&self, _sender: Id) -> usize {
            // NSApplicationTerminateReply: NSTerminateCancel = 0, NSTerminateNow = 1.
            if quit_confirm_allows() {
                1
            } else {
                0
            }
        }
    }
}

impl ApplicationDelegate {
    pub(super) fn new(
        mtm: MainThread,
        activation_policy: Option<isize>,
        activate_ignoring_other_apps: bool,
        wake_armed: wake_queue::Armed,
    ) -> aterm_objc::Retained<Self> {
        ApplicationDelegate::alloc_init(mtm, AppState {
            activation_policy,
            activate_ignoring_other_apps,
            run_loop: RunLoop::main(mtm),
            event_handler: EventHandler::new(),
            stop_on_launch: Cell::new(false),
            stop_before_wait: Cell::new(false),
            stop_after_wait: Cell::new(false),
            stop_on_redraw: Cell::new(false),
            is_launched: Cell::new(false),
            is_running: Cell::new(false),
            exit: Cell::new(false),
            control_flow: Cell::new(ControlFlow::default()),
            waker: RefCell::new(EventLoopWaker::new(mtm)),
            start_time: Cell::new(None),
            wait_timeout: Cell::new(None),
            pending_redraw: RefCell::new(vec![]),
            wake_armed,
        })
        .expect("couldn't create `WinitApplicationDelegate`")
    }

    fn did_finish_launching(&self) {
        trace_scope!("applicationDidFinishLaunching:");
        self.ivars().is_launched.set(true);

        let mtm = self.mtm();
        let ns_app = app();
        // SAFETY (for this whole block): `ns_app` is the live shared `NSApplication`, as
        // `window_activation_hack` requires; `-setActivationPolicy:` is `B24@0:8q16` (its answer is
        // ignored, as upstream ignored it); `+currentApplication` is `@16#0:8` and
        // `-bundleIdentifier` `@16@0:8`, which may answer nil; `-activateIgnoringOtherApps:` is
        // `v20@0:8B16`.
        unsafe {
            if let Some(activation_policy) = self.ivars().activation_policy {
                send_bool_isize(ns_app, sel!(setActivationPolicy:), activation_policy);
            } else {
                let running =
                    send_id(class(c"NSRunningApplication").as_id(), sel!(currentApplication));
                let is_bundled = !send_id(running, sel!(bundleIdentifier)).is_null();
                if !is_bundled {
                    send_bool_isize(
                        ns_app,
                        sel!(setActivationPolicy:),
                        NS_APPLICATION_ACTIVATION_POLICY_REGULAR,
                    );
                }
            }

            window_activation_hack(ns_app);
            send_v_bool(
                ns_app,
                sel!(activateIgnoringOtherApps:),
                self.ivars().activate_ignoring_other_apps,
            );
        }

        // SAFETY: `ns_app` is the live shared `NSApplication`.
        unsafe { menu::initialize(ns_app, mtm) };

        self.ivars().waker.borrow_mut().start();

        self.set_is_running(true);
        self.dispatch_init_events();

        if self.ivars().stop_on_launch.get() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }
    }

    fn will_terminate(&self) {
        trace_scope!("applicationWillTerminate:");
        let _ = self.mtm();
        // SAFETY: `app()` is the live shared `NSApplication`.
        unsafe { notify_windows_of_exit(app()) };
        self.internal_exit();
    }

    /// The delegate `NSApp` is holding, as this class.
    pub fn get(w: aterm_objc::MainThread) -> aterm_objc::Retained<Self> {
        let _ = w;
        // SAFETY: `-delegate` is `@16@0:8` on `NSApplication` and answers +0.
        let delegate = unsafe { send_id(app(), sel!(delegate)) };
        assert!(!delegate.is_null(), "a delegate was not configured on the application");
        // SAFETY: `delegate` is the live object `-[NSApplication delegate]` just answered, and
        // `-isKindOfClass:` is `B@:#` on every Apple runtime.
        let is_ours = unsafe {
            let send: unsafe extern "C-unwind" fn(
                Id,
                aterm_objc::Sel,
                aterm_objc::ClassPtr,
            ) -> Bool = aterm_objc::checked_msg();
            send(
                delegate,
                aterm_objc::sel!(isKindOfClass:),
                <Self as aterm_objc::ClassType>::class(),
            )
        };
        assert!(
            is_ours.as_bool(),
            "tried to get a delegate that was not the one Winit has registered"
        );
        // SAFETY: just checked that the delegate is an instance of `ApplicationDelegate`; `retain`
        // takes the borrowed reference to +1.
        unsafe { aterm_objc::Retained::retain(delegate) }.expect("a non-null delegate retains")
    }

    /// The main-thread witness objc2's `mutability::MainThreadOnly` used to
    /// derive from the receiver's type.
    #[track_caller]
    fn mtm(&self) -> MainThread {
        MainThread::new().expect(
            "a WinitApplicationDelegate method ran off the main thread; AppKit delivers on it",
        )
    }

    /// Returns a retained `+1` handle to this delegate.
    pub(super) fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is a live non-null
        // receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live ApplicationDelegate")
    }

    /// This delegate as the `id` `-[NSApplication setDelegate:]` takes.
    pub(super) fn as_delegate_id(&self) -> Id {
        let id = self.as_id();
        debug_assert!(
            {
                // SAFETY: `id` is a live instance of this class; `-conformsToProtocol:` is
                // `B24@0:8@16` on `NSObject`.
                unsafe {
                    let f: unsafe extern "C-unwind" fn(
                        Id,
                        aterm_objc::Sel,
                        aterm_objc::ProtocolPtr,
                    ) -> Bool = aterm_objc::checked_msg();
                    f(id, sel!(conformsToProtocol:), aterm_objc::protocol(c"NSApplicationDelegate"))
                        .as_bool()
                }
            },
            "WinitApplicationDelegate does not conform to NSApplicationDelegate: \
             class_addProtocol did not run at declaration, so aterm's live-class \
             audit and typed consumers see a delegate that does not claim the \
             protocol (AppKit itself reaches the @optional rows by \
             -respondsToSelector: and is unaffected)"
        );
        id
    }

    /// Place the event handler in the application delegate for the duration of the given closure.
    pub fn set_event_handler<R>(
        &self,
        handler: impl FnMut(Event<HandlePendingUserEvents>, &RootActiveEventLoop),
        closure: impl FnOnce() -> R,
    ) -> R {
        self.ivars().event_handler.set(handler, closure)
    }

    /// Stops the initial `-[NSApplication run]` pass once launching finishes.
    pub fn set_stop_on_launch(&self) {
        self.ivars().stop_on_launch.set(true);
    }

    pub fn set_stop_before_wait(&self, value: bool) {
        self.ivars().stop_before_wait.set(value)
    }

    pub fn set_stop_after_wait(&self, value: bool) {
        self.ivars().stop_after_wait.set(value)
    }

    pub fn set_stop_on_redraw(&self, value: bool) {
        self.ivars().stop_on_redraw.set(value)
    }

    pub fn set_wait_timeout(&self, value: Option<Instant>) {
        self.ivars().wait_timeout.set(value)
    }

    /// Clears the `running` state and resets the `control_flow` state when an `EventLoop` exits.
    pub fn internal_exit(&self) {
        self.handle_event(Event::LoopExiting);

        self.set_is_running(false);
        self.set_stop_on_redraw(false);
        self.set_stop_before_wait(false);
        self.set_stop_after_wait(false);
        self.set_wait_timeout(None);
    }

    pub fn is_launched(&self) -> bool {
        self.ivars().is_launched.get()
    }

    pub fn set_is_running(&self, value: bool) {
        self.ivars().is_running.set(value)
    }

    pub fn is_running(&self) -> bool {
        self.ivars().is_running.get()
    }

    pub fn exit(&self) {
        self.ivars().exit.set(true)
    }

    pub fn exiting(&self) -> bool {
        self.ivars().exit.get()
    }

    pub fn set_control_flow(&self, value: ControlFlow) {
        self.ivars().control_flow.set(value)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.ivars().control_flow.get()
    }

    pub fn maybe_queue_window_event(&self, window_id: WindowId, event: WindowEvent) {
        self.maybe_queue_event(Event::WindowEvent { window_id: RootWindowId(window_id), event });
    }

    pub fn handle_window_event(&self, window_id: WindowId, event: WindowEvent) {
        self.handle_event(Event::WindowEvent { window_id: RootWindowId(window_id), event });
    }

    pub fn handle_redraw(&self, window_id: WindowId) {
        let _ = self.mtm();
        if !self.ivars().event_handler.in_use() {
            self.handle_event(Event::WindowEvent {
                window_id: RootWindowId(window_id),
                event: WindowEvent::RedrawRequested,
            });

            if self.ivars().stop_on_redraw.get() {
                // SAFETY: `app()` is the live shared `NSApplication`.
                unsafe { stop_app_immediately(app()) };
            }
        }
    }

    pub fn queue_redraw(&self, window_id: WindowId) {
        let mut pending_redraw = self.ivars().pending_redraw.borrow_mut();
        if !pending_redraw.contains(&window_id) {
            pending_redraw.push(window_id);
        }
        self.ivars().run_loop.wakeup();
    }

    #[track_caller]
    fn maybe_queue_event(&self, event: Event<HandlePendingUserEvents>) {
        if !self.ivars().event_handler.in_use() {
            self.handle_event(event);
        } else {
            tracing::debug!(?event, "had to queue event since another is currently being handled");
            let this = self.retained();
            self.ivars().run_loop.queue_closure(move || this.handle_event(event));
        }
    }

    #[track_caller]
    fn handle_event(&self, event: Event<HandlePendingUserEvents>) {
        self.ivars().event_handler.handle_event(event, &ActiveEventLoop::new_root(self.retained()))
    }

    /// Dispatches `NewEvents(Init)` and `Resumed`.
    pub fn dispatch_init_events(&self) {
        self.handle_event(Event::NewEvents(StartCause::Init));
        self.handle_event(Event::Resumed);
    }

    fn stop_waker_in_nested_loop(&self) {
        if self.ivars().event_handler.in_use() {
            self.ivars().waker.borrow_mut().stop();
        }
    }

    pub fn wakeup(&self, panic_info: Weak<PanicInfo>) {
        let _ = self.mtm();
        let panic_info = panic_info
            .upgrade()
            .expect("The panic info must exist here. This failure indicates a developer error.");

        if panic_info.is_panicking() || !self.ivars().event_handler.ready() || !self.is_running() {
            self.stop_waker_in_nested_loop();
            return;
        }

        if self.ivars().stop_after_wait.get() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }

        let start = self.ivars().start_time.get().unwrap();
        let cause = match self.control_flow() {
            ControlFlow::Poll => StartCause::Poll,
            ControlFlow::Wait => StartCause::WaitCancelled { start, requested_resume: None },
            ControlFlow::WaitUntil(requested_resume) => {
                if Instant::now() >= requested_resume {
                    StartCause::ResumeTimeReached { start, requested_resume }
                } else {
                    StartCause::WaitCancelled { start, requested_resume: Some(requested_resume) }
                }
            },
        };

        self.handle_event(Event::NewEvents(cause));
    }

    pub fn cleared(&self, panic_info: Weak<PanicInfo>) {
        let _ = self.mtm();
        let panic_info = panic_info
            .upgrade()
            .expect("The panic info must exist here. This failure indicates a developer error.");

        if panic_info.is_panicking() || !self.ivars().event_handler.ready() || !self.is_running() {
            self.stop_waker_in_nested_loop();
            return;
        }

        let _ = aterm_objc::contain("CFRunLoop observer: UserEvent", || {
            self.handle_event(Event::UserEvent(HandlePendingUserEvents));
        });

        let redraw = mem::take(&mut *self.ivars().pending_redraw.borrow_mut());
        let handled = Cell::new(0_usize);
        let contained = aterm_objc::contain("CFRunLoop observer: RedrawRequested", || {
            for window_id in &redraw {
                self.handle_event(Event::WindowEvent {
                    window_id: RootWindowId(*window_id),
                    event: WindowEvent::RedrawRequested,
                });
                handled.set(handled.get() + 1);
            }
        });
        if contained.is_err() {
            let mut pending = self.ivars().pending_redraw.borrow_mut();
            let undelivered = redraw.iter().skip(handled.get() + 1).copied();
            let mut restored: Vec<WindowId> =
                undelivered.filter(|w| !pending.contains(w)).collect();
            restored.append(&mut pending);
            *pending = restored;
        }

        let _ = aterm_objc::contain("CFRunLoop observer: AboutToWait", || {
            self.handle_event(Event::AboutToWait);
        });

        if self.exiting() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe {
                let app = app();
                stop_app_immediately(app);
                notify_windows_of_exit(app);
            }
        }

        if self.ivars().stop_before_wait.get() {
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
        }
        self.ivars().start_time.set(Some(Instant::now()));
        let wait_timeout = self.ivars().wait_timeout.get();
        let app_timeout = match self.control_flow() {
            ControlFlow::Wait => None,
            ControlFlow::Poll => Some(Instant::now()),
            ControlFlow::WaitUntil(instant) => Some(instant),
        };
        self.ivars().waker.borrow_mut().start_at(min_timeout(wait_timeout, app_timeout));

        if self.ivars().wake_armed.is_set() {
            self.ivars().run_loop.wakeup();
        }
    }
}

#[derive(Debug)]
pub(crate) struct HandlePendingUserEvents;

fn min_timeout(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    a.map_or(b, |a_timeout| b.map_or(Some(a_timeout), |b_timeout| Some(a_timeout.min(b_timeout))))
}

unsafe fn window_activation_hack(app: Id) {
    autoreleasepool(|_| {
        // SAFETY: `-windows` is `@16@0:8` on `NSApplication`; `-count` is `Q16@0:8` and
        // `-objectAtIndex:` is `@24@0:8Q16` on `NSArray`, and no index below the count is out of
        // range; `-isVisible` is `B16@0:8` and `-makeKeyAndOrderFront:` is `v24@0:8@16` on
        // `NSWindow`, whose argument is the `nil` sender upstream passed as `None`.
        unsafe {
            let windows = send_id(app, sel!(windows));
            let n = send_usize(windows, sel!(count));
            for i in 0..n {
                let window = send_id_usize(windows, sel!(objectAtIndex:), i);
                if send_bool(window, sel!(isVisible)) {
                    tracing::trace!("Activating visible window");
                    send_v_id(window, sel!(makeKeyAndOrderFront:), Id::NIL);
                } else {
                    tracing::trace!("Skipping activating invisible window");
                }
            }
        }
    });
}

fn app() -> Id {
    // SAFETY: `+sharedApplication` is `@16#0:8` on `NSApplication` and creates the instance on
    // first call.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}
