// Modified by the aterm project in 2026; see the repository NOTICE.
// (The application delegate is declared with `aterm_objc::declare_class!`, and
//  every `objc2` binding call in this file is now a typed send through
//  `aterm_objc`. Search for the aterm local-patch marker.)
use std::any::Any;
use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::panic::{catch_unwind, resume_unwind, RefUnwindSafe, UnwindSafe};
use std::ptr;
use std::rc::{Rc, Weak};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use core_foundation::base::{CFIndex, CFRelease};
use core_foundation::runloop::{
    kCFRunLoopCommonModes, CFRunLoopAddSource, CFRunLoopGetMain, CFRunLoopSourceContext,
    CFRunLoopSourceCreate, CFRunLoopSourceRef, CFRunLoopSourceSignal, CFRunLoopWakeUp,
};
// LOCAL PATCH (aterm): objc2's `autoreleasepool`/`Retained`/`sel!`, its four
// `NSApplication`, `NSApplicationActivationPolicy`, `NSEvent` and `NSWindow`
// bindings, `MainThreadMarker` and `NSObjectProtocol` are all gone. The
// activation-policy values are the seam's, at the SDK's own (SIGNED) values.
use aterm_objc::send::{
    send_bool, send_bool_sel, send_id, send_id_usize, send_usize, send_v, send_v_bool, send_v_id,
    send_v_id_bool,
};
use aterm_objc::{Id, MainThread, Obj, autoreleasepool, class, sel};

use super::aterm_objc_seam::consts::{
    NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY, NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED,
    NS_APPLICATION_ACTIVATION_POLICY_REGULAR,
};

use super::app::{override_send_event, set_send_event_panic_info};
use super::app_state::{ApplicationDelegate, HandlePendingUserEvents};
use super::event::dummy_event;
use super::monitor::{self, MonitorHandle};
use super::observer::setup_control_flow_observers;
use crate::error::EventLoopError;
use crate::event::Event;
use crate::event_loop::{
    ActiveEventLoop as RootWindowTarget, ControlFlow, DeviceEvents, EventLoopClosed,
};
use crate::platform::macos::ActivationPolicy;
use crate::platform::pump_events::PumpStatus;
use crate::platform_impl::platform::cursor::CustomCursor;
use crate::window::{CustomCursor as RootCustomCursor, CustomCursorSource, Theme};

#[derive(Default)]
pub struct PanicInfo {
    inner: Cell<Option<Box<dyn Any + Send + 'static>>>,
}

// WARNING:
// As long as this struct is used through its `impl`, it is UnwindSafe.
// (If `get_mut` is called on `inner`, unwind safety may get broken.)
impl UnwindSafe for PanicInfo {}
impl RefUnwindSafe for PanicInfo {}
impl PanicInfo {
    pub fn is_panicking(&self) -> bool {
        let inner = self.inner.take();
        let result = inner.is_some();
        self.inner.set(inner);
        result
    }

    /// Overwrites the current state if the current state is not panicking
    pub fn set_panic(&self, p: Box<dyn Any + Send + 'static>) {
        if !self.is_panicking() {
            self.inner.set(Some(p));
        }
    }

    pub fn take(&self) -> Option<Box<dyn Any + Send + 'static>> {
        self.inner.take()
    }
}

#[derive(Debug)]
pub struct ActiveEventLoop {
    delegate: aterm_objc::Retained<ApplicationDelegate>,
    /// LOCAL PATCH (aterm), W12: `aterm_objc::MainThread`, not
    /// `MainThreadMarker`. `window.rs` reads this field, and reading it is what
    /// used to make that file need `objc2_foundation` — a CROSS-FILE
    /// CONSUMPTION through a struct field rather than through a call.
    pub(super) mtm: MainThread,
}

impl ActiveEventLoop {
    pub(super) fn new_root(
        delegate: aterm_objc::Retained<ApplicationDelegate>,
    ) -> RootWindowTarget {
        // LOCAL PATCH (aterm): `MainThreadMarker::from(&*delegate)` was a
        // COMPILE-TIME derivation off objc2's `mutability::MainThreadOnly`, and
        // `ApplicationDelegate` no longer carries it. This asks the same
        // question the marker's own constructor asks.
        let mtm =
            MainThread::new().expect("an ActiveEventLoop was rooted off the main thread");
        let p = Self { delegate, mtm };
        RootWindowTarget { p, _marker: PhantomData }
    }

    pub(super) fn app_delegate(&self) -> &ApplicationDelegate {
        &self.delegate
    }

    pub fn create_custom_cursor(&self, source: CustomCursorSource) -> RootCustomCursor {
        RootCustomCursor { inner: CustomCursor::new(source.inner) }
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        monitor::available_monitors()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        monitor::primary_monitor()
    }

    #[inline]
    pub fn listen_device_events(&self, _allowed: DeviceEvents) {}

    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::RawDisplayHandle::AppKit(rwh_05::AppKitDisplayHandle::empty())
    }

    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        let _ = self.mtm;
        // LOCAL PATCH (aterm), W12: `appearance_to_theme` takes the raw `id`
        // its own sends take, and this file now HAS one — the crossing that
        // used to be on these lines is gone.
        //
        // SAFETY: `-respondsToSelector:` is `B24@0:8:16` and
        // `-effectiveAppearance` is `@16@0:8` on `NSApplication`, answering a
        // live `NSAppearance` borrowed for the duration of this statement.
        unsafe {
            let app = app();
            if send_bool_sel(app, sel!(respondsToSelector:), sel!(effectiveAppearance)) {
                Some(super::window_delegate::appearance_to_theme(send_id(
                    app,
                    sel!(effectiveAppearance),
                )))
            } else {
                Some(Theme::Light)
            }
        }
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::AppKit(rwh_06::AppKitDisplayHandle::new()))
    }

    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        self.delegate.set_control_flow(control_flow)
    }

    pub(crate) fn control_flow(&self) -> ControlFlow {
        self.delegate.control_flow()
    }

    pub(crate) fn exit(&self) {
        self.delegate.exit()
    }

    pub(crate) fn clear_exit(&self) {
        self.delegate.clear_exit()
    }

    pub(crate) fn exiting(&self) -> bool {
        self.delegate.exiting()
    }

    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle
    }

    pub(crate) fn hide_application(&self) {
        let _ = self.mtm;
        // SAFETY: `-hide:` is `v24@0:8@16` on `NSApplication`; its argument is
        // the `nil` sender upstream passed as `None`.
        unsafe { send_v_id(app(), sel!(hide:), Id::NIL) }
    }

    pub(crate) fn hide_other_applications(&self) {
        let _ = self.mtm;
        // SAFETY: `-hideOtherApplications:` is `v24@0:8@16` on `NSApplication`.
        unsafe { send_v_id(app(), sel!(hideOtherApplications:), Id::NIL) }
    }

    pub(crate) fn set_allows_automatic_window_tabbing(&self, enabled: bool) {
        let _ = self.mtm;
        // SAFETY: `+setAllowsAutomaticWindowTabbing:` is `v20@0:8B16` — a CLASS
        // method on `NSWindow`, which is why the receiver is the class object.
        unsafe {
            send_v_bool(
                class(c"NSWindow").as_id(),
                sel!(setAllowsAutomaticWindowTabbing:),
                enabled,
            )
        }
    }

    pub(crate) fn allows_automatic_window_tabbing(&self) -> bool {
        let _ = self.mtm;
        // SAFETY: `+allowsAutomaticWindowTabbing` is `B16@0:8`, a class method.
        unsafe {
            send_bool(
                class(c"NSWindow").as_id(),
                sel!(allowsAutomaticWindowTabbing),
            )
        }
    }
}

fn map_user_event<T: 'static>(
    mut handler: impl FnMut(Event<T>, &RootWindowTarget),
    receiver: Rc<mpsc::Receiver<T>>,
) -> impl FnMut(Event<HandlePendingUserEvents>, &RootWindowTarget) {
    move |event, window_target| match event.map_nonuser_event() {
        Ok(event) => (handler)(event, window_target),
        Err(_) => {
            for event in receiver.try_iter() {
                (handler)(Event::UserEvent(event), window_target);
            }
        },
    }
}

pub struct EventLoop<T: 'static> {
    /// Store a reference to the application for convenience.
    ///
    /// We intentionally don't store `WinitApplication` since we want to have
    /// the possibility of swapping that out at some point.
    ///
    /// LOCAL PATCH (aterm): an `aterm_objc::Obj` — the same +1 handle
    /// `Retained<NSApplication>` was.
    app: Obj,
    /// The application delegate that we've registered.
    ///
    /// The delegate is only weakly referenced by NSApplication, so we must
    /// keep it around here as well.
    delegate: aterm_objc::Retained<ApplicationDelegate>,

    // Event sender and receiver, used for EventLoopProxy.
    sender: mpsc::Sender<T>,
    receiver: Rc<mpsc::Receiver<T>>,

    window_target: RootWindowTarget,
    panic_info: Rc<PanicInfo>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
    pub(crate) activation_policy: Option<ActivationPolicy>,
    pub(crate) default_menu: bool,
    pub(crate) activate_ignoring_other_apps: bool,
}

impl Default for PlatformSpecificEventLoopAttributes {
    fn default() -> Self {
        Self { activation_policy: None, default_menu: true, activate_ignoring_other_apps: true }
    }
}

impl<T> EventLoop<T> {
    pub(crate) fn new(
        attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        let mtm = MainThread::new()
            .expect("on macOS, `EventLoop` must be created on the main thread!");

        // Initialize the application (if it has not already been).
        // SAFETY: `+sharedApplication` answers the process-lifetime singleton,
        // +0 autoreleased, so it is retained into a handle this struct owns.
        let app = unsafe { Obj::retain(app()) }.expect("+sharedApplication to answer");

        let activation_policy = match attributes.activation_policy {
            None => None,
            Some(ActivationPolicy::Regular) => Some(NS_APPLICATION_ACTIVATION_POLICY_REGULAR),
            Some(ActivationPolicy::Accessory) => Some(NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY),
            Some(ActivationPolicy::Prohibited) => Some(NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED),
        };
        let delegate = ApplicationDelegate::new(
            mtm,
            activation_policy,
            attributes.default_menu,
            attributes.activate_ignoring_other_apps,
        );

        autoreleasepool(|_| {
            // SAFETY: `-setDelegate:` is `v24@0:8@16` on `NSApplication` and
            // takes a WEAK reference — which is why `EventLoop` keeps its own
            // handle to the delegate below. `as_delegate_id` asserts the class
            // really conforms to `NSApplicationDelegate`, which is the
            // compile-time fact `ProtocolObject` used to carry.
            unsafe { send_v_id(app.id(), sel!(setDelegate:), delegate.as_delegate_id()) };
        });

        // Override `sendEvent:` on the application to forward to our application state.
        // SAFETY: `app` owns a +1 to the live shared `NSApplication`.
        unsafe { override_send_event(app.id(), mtm) };

        let panic_info: Rc<PanicInfo> = Default::default();
        // LOCAL PATCH (aterm): the override's Rust-panic guard is
        // `stop_app_on_panic`, so it needs the slot the observers get below.
        set_send_event_panic_info(mtm, Rc::downgrade(&panic_info));
        // LOCAL PATCH (aterm): `observer.rs` speaks `aterm_objc::MainThread`,
        // and since W12 so does this file — the crossing that used to sit on
        // this line is gone.
        setup_control_flow_observers(mtm, Rc::downgrade(&panic_info));

        let (sender, receiver) = mpsc::channel();
        Ok(EventLoop {
            app,
            delegate: delegate.clone_retained(),
            sender,
            receiver: Rc::new(receiver),
            window_target: RootWindowTarget {
                p: ActiveEventLoop { delegate, mtm },
                _marker: PhantomData,
            },
            panic_info,
        })
    }

    pub fn window_target(&self) -> &RootWindowTarget {
        &self.window_target
    }

    pub fn run<F>(mut self, handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        self.run_on_demand(handler)
    }

    // NB: we don't base this on `pump_events` because for `MacOs` we can't support
    // `pump_events` elegantly (we just ask to run the loop for a "short" amount of
    // time and so a layered implementation would end up using a lot of CPU due to
    // redundant wake ups.
    pub fn run_on_demand<F>(&mut self, handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        let handler = map_user_event(handler, self.receiver.clone());

        self.delegate.set_event_handler(handler, || {
            autoreleasepool(|_| {
                // clear / normalize pump_events state
                self.delegate.set_wait_timeout(None);
                self.delegate.set_stop_before_wait(false);
                self.delegate.set_stop_after_wait(false);
                self.delegate.set_stop_on_redraw(false);

                if self.delegate.is_launched() {
                    debug_assert!(!self.delegate.is_running());
                    self.delegate.set_is_running(true);
                    self.delegate.dispatch_init_events();
                }

                // SAFETY: We do not run the application re-entrantly.
                // `-run` is `v16@0:8` on `NSApplication`.
                unsafe { send_v(self.app.id(), sel!(run)) };

                // While the app is running it's possible that we catch a panic
                // to avoid unwinding across an objective-c ffi boundary, which
                // will lead to us stopping the `NSApplication` and saving the
                // `PanicInfo` so that we can resume the unwind at a controlled,
                // safe point in time.
                if let Some(panic) = self.panic_info.take() {
                    resume_unwind(panic);
                }

                self.delegate.internal_exit()
            })
        });

        Ok(())
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, handler: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        let handler = map_user_event(handler, self.receiver.clone());

        self.delegate.set_event_handler(handler, || {
            autoreleasepool(|_| {
                // As a special case, if the application hasn't been launched yet then we at least
                // run the loop until it has fully launched.
                if !self.delegate.is_launched() {
                    debug_assert!(!self.delegate.is_running());

                    self.delegate.set_stop_on_launch();
                    // SAFETY: We do not run the application re-entrantly.
                    unsafe { send_v(self.app.id(), sel!(run)) };

                    // Note: we dispatch `NewEvents(Init)` + `Resumed` events after the application
                    // has launched
                } else if !self.delegate.is_running() {
                    // Even though the application may have been launched, it's possible we aren't
                    // running if the `EventLoop` was run before and has since
                    // exited. This indicates that we just starting to re-run
                    // the same `EventLoop` again.
                    self.delegate.set_is_running(true);
                    self.delegate.dispatch_init_events();
                } else {
                    // Only run for as long as the given `Duration` allows so we don't block the
                    // external loop.
                    match timeout {
                        Some(Duration::ZERO) => {
                            self.delegate.set_wait_timeout(None);
                            self.delegate.set_stop_before_wait(true);
                        },
                        Some(duration) => {
                            self.delegate.set_stop_before_wait(false);
                            let timeout = Instant::now() + duration;
                            self.delegate.set_wait_timeout(Some(timeout));
                            self.delegate.set_stop_after_wait(true);
                        },
                        None => {
                            self.delegate.set_wait_timeout(None);
                            self.delegate.set_stop_before_wait(false);
                            self.delegate.set_stop_after_wait(true);
                        },
                    }
                    self.delegate.set_stop_on_redraw(true);
                    // SAFETY: We do not run the application re-entrantly.
                    unsafe { send_v(self.app.id(), sel!(run)) };
                }

                // While the app is running it's possible that we catch a panic
                // to avoid unwinding across an objective-c ffi boundary, which
                // will lead to us stopping the application and saving the
                // `PanicInfo` so that we can resume the unwind at a controlled,
                // safe point in time.
                if let Some(panic) = self.panic_info.take() {
                    resume_unwind(panic);
                }

                if self.delegate.exiting() {
                    self.delegate.internal_exit();
                    PumpStatus::Exit(0)
                } else {
                    PumpStatus::Continue
                }
            })
        })
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy::new(self.sender.clone())
    }
}

#[derive(Clone)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_05")]
    #[inline]
    pub fn raw_display_handle_rwh_05(&self) -> rwh_05::RawDisplayHandle {
        rwh_05::AppKitDisplayHandle::empty().into()
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::AppKitDisplayHandle::new().into())
    }
}

///
/// # Safety
/// `app` must be the live shared `NSApplication`.
pub(super) unsafe fn stop_app_immediately(app: Id) {
    autoreleasepool(|_| {
        // SAFETY: `-stop:` is `v24@0:8@16` on `NSApplication` and its argument
        // is the `nil` sender upstream passed as `None`;
        // `-postEvent:atStart:` is `v32@0:8@16B24`.
        unsafe {
            send_v_id(app, sel!(stop:), Id::NIL);
            // To stop event loop immediately, we need to post some event here.
            // See: https://stackoverflow.com/questions/48041279/stopping-the-nsapplication-main-event-loop/48064752#48064752
            //
            // LOCAL PATCH (aterm), W12: `dummy_event` answers an
            // `aterm_objc::Obj` (a +1 handle) rather than `Retained<NSEvent>`,
            // and this file now sends the raw `id` — the crossing that used to
            // sit here is gone. The `Obj` is bound to a local so its +1
            // outlives the send.
            let dummy = dummy_event().expect("NSEvent refused the dummy event");
            send_v_id_bool(app, sel!(postEvent:atStart:), dummy.id(), true);
        }
    });
}

/// Tell all windows to close.
///
/// This will synchronously trigger `WindowEvent::Destroyed` within
/// `windowWillClose:`, giving the application one last chance to handle
/// those events. It doesn't matter if the user also ends up closing the
/// windows in `Window`'s `Drop` impl, once a window has been closed once, it
/// stays closed.
///
/// This ensures that no windows linger on after the event loop has exited,
/// see <https://github.com/rust-windowing/winit/issues/4135>.
///
/// # Safety
/// `app` must be the live shared `NSApplication`.
pub(super) unsafe fn notify_windows_of_exit(app: Id) {
    // LOCAL PATCH (aterm): the array is walked by index rather than by
    // `NSFastEnumeration`, for `monitor.rs`'s reason. The pool is EXPLICIT
    // because `-windows` answers +0 autoreleased and `-close` runs a whole
    // teardown — including `windowWillClose:` and the fork's own `Destroyed`
    // dispatch — inside it.
    autoreleasepool(|_| {
        // SAFETY: `-windows` is `@16@0:8` on `NSApplication`; `-count` is
        // `Q16@0:8` and `-objectAtIndex:` is `@24@0:8Q16` on `NSArray`;
        // `-close` is `v16@0:8` on `NSWindow`. THE LENGTH IS READ ONCE, which
        // is upstream's behaviour: `for window in app.windows()` walked a
        // SNAPSHOT array, so a `-close` cannot shift the indices.
        unsafe {
            let windows = send_id(app, sel!(windows));
            let n = send_usize(windows, sel!(count));
            for i in 0..n {
                let window = send_id_usize(windows, sel!(objectAtIndex:), i);
                send_v(window, sel!(close));
            }
        }
    });
}

/// Catches panics that happen inside `f` and when a panic
/// happens, stops the `sharedApplication`
#[inline]
// LOCAL PATCH (aterm): takes `aterm_objc::MainThread`, because its only caller
// (`observer.rs::control_flow_handler`) is ported and no longer mints an objc2
// marker. Since W12 nothing here consumes one, so the witness is simply the
// proof and the re-derivation is gone.
pub fn stop_app_on_panic<F: FnOnce() -> R + UnwindSafe, R>(
    mtm: MainThread,
    panic_info: Weak<PanicInfo>,
    f: F,
) -> Option<R> {
    match catch_unwind(f) {
        Ok(r) => Some(r),
        Err(e) => {
            // It's important that we set the panic before requesting a `stop`
            // because some callback are still called during the `stop` message
            // and we need to know in those callbacks if the application is currently
            // panicking
            // LOCAL PATCH (aterm): no `unwrap`. This guard is also the OUTER
            // half of the `"C-unwind"` `sendEvent:` override, so a second
            // panic here would escape straight into AppKit's caller with no
            // Rust catch above it. If the event loop's `PanicInfo` is gone
            // (teardown ordering) the honest answer is the named abort.
            match panic_info.upgrade() {
                Some(panic_info) => panic_info.set_panic(e),
                None => aterm_objc::abort_on_unwind("stop_app_on_panic (event loop gone)"),
            }
            let _ = mtm;
            // SAFETY: `app()` is the live shared `NSApplication`.
            unsafe { stop_app_immediately(app()) };
            None
        },
    }
}

pub struct EventLoopProxy<T> {
    sender: mpsc::Sender<T>,
    source: CFRunLoopSourceRef,
}

unsafe impl<T: Send> Send for EventLoopProxy<T> {}
unsafe impl<T: Send> Sync for EventLoopProxy<T> {}

impl<T> Drop for EventLoopProxy<T> {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.source as _);
        }
    }
}

impl<T> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        EventLoopProxy::new(self.sender.clone())
    }
}

impl<T> EventLoopProxy<T> {
    fn new(sender: mpsc::Sender<T>) -> Self {
        unsafe {
            // just wake up the eventloop
            extern "C" fn event_loop_proxy_handler(_: *const c_void) {}

            // adding a Source to the main CFRunLoop lets us wake it up and
            // process user events through the normal OS EventLoop mechanisms.
            let rl = CFRunLoopGetMain();
            let mut context = CFRunLoopSourceContext {
                version: 0,
                info: ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: event_loop_proxy_handler,
            };
            let source = CFRunLoopSourceCreate(ptr::null_mut(), CFIndex::MAX - 1, &mut context);
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CFRunLoopWakeUp(rl);

            EventLoopProxy { sender, source }
        }
    }

    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        self.sender.send(event).map_err(|mpsc::SendError(x)| EventLoopClosed(x))?;
        unsafe {
            // let the main thread know there's a new event
            CFRunLoopSourceSignal(self.source);
            let rl = CFRunLoopGetMain();
            CFRunLoopWakeUp(rl);
        }
        Ok(())
    }
}

/// `+[NSApplication sharedApplication]`, the process-lifetime singleton — the
/// same helper `app_state.rs` and `window_delegate.rs` carry, for the same
/// reason: the marker objc2's binding demanded is objc2's requirement, not
/// AppKit's, and every call site here still holds a witness.
fn app() -> Id {
    // SAFETY: `+sharedApplication` is `@16#0:8` on `NSApplication` and creates
    // the instance on first call.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}
