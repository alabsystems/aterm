// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/event_loop.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).
use std::any::Any;
use std::cell::Cell;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::panic::{catch_unwind, resume_unwind, RefUnwindSafe, UnwindSafe};
use std::ptr;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use aterm_objc::cf::{
    kCFRunLoopCommonModes, CFIndex, CFRelease, CFRetain, CFRunLoopAddSource, CFRunLoopGetMain,
    CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceRef, CFRunLoopSourceSignal,
    CFRunLoopWakeUp,
};
// LOCAL PATCH (aterm): Objective-C goes through `aterm_objc` (typed `send` prototypes,
// `Obj`/`Retained` handles, `MainThread` witnesses, the seam's `consts`) where upstream used objc2
// and its framework bindings.
use aterm_objc::send::{
    send_bool_sel, send_id, send_id_usize, send_usize, send_v, send_v_id, send_v_id_bool,
};
use aterm_objc::{autoreleasepool, class, sel, Id, MainThread, Obj};

use super::aterm_objc_seam::consts::{
    NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY, NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED,
    NS_APPLICATION_ACTIVATION_POLICY_REGULAR,
};

use super::app::{override_send_event, set_send_event_panic_info};
use super::app_state::{ApplicationDelegate, HandlePendingUserEvents};
use super::event::dummy_event;
use super::monitor::{self, MonitorHandle};
use super::observer::setup_control_flow_observers;
use super::wake_queue;
use crate::error::EventLoopError;
use crate::event::Event;
use crate::event_loop::{ActiveEventLoop as RootWindowTarget, ControlFlow, EventLoopClosed};
use crate::platform::macos::ActivationPolicy;
use crate::platform::pump_events::PumpStatus;
use crate::window::Theme;

#[derive(Default)]
pub struct PanicInfo {
    inner: Cell<Option<Box<dyn Any + Send + 'static>>>,
}

impl UnwindSafe for PanicInfo {}
impl RefUnwindSafe for PanicInfo {}
impl PanicInfo {
    pub fn is_panicking(&self) -> bool {
        let inner = self.inner.take();
        let result = inner.is_some();
        self.inner.set(inner);
        result
    }

    /// Overwrites the current state if not already panicking.
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
    backend: Backend,
    /// Main-thread witness (`aterm_objc::MainThread`).
    pub(super) mtm: MainThread,
}

/// LOCAL PATCH (aterm): the AppKit loop's delegate, or the display-free loop's state
/// (`headless.rs`), whose arms below answer without AppKit or CoreGraphics' display services
/// (a WindowServer connection).
#[derive(Debug)]
enum Backend {
    AppKit(aterm_objc::Retained<ApplicationDelegate>),
    Headless(super::headless::LoopState),
}

impl ActiveEventLoop {
    pub(super) fn new_root(
        delegate: aterm_objc::Retained<ApplicationDelegate>,
    ) -> RootWindowTarget {
        let mtm = MainThread::new().expect("an ActiveEventLoop was rooted off the main thread");
        let p = Self { backend: Backend::AppKit(delegate), mtm };
        RootWindowTarget { p, _marker: PhantomData }
    }

    pub(super) fn new_headless(mtm: MainThread, drained: wake_queue::Drained) -> Self {
        Self { backend: Backend::Headless(super::headless::LoopState::new(drained)), mtm }
    }

    /// Returns the AppKit application delegate, or `None` on a headless loop.
    pub(super) fn app_delegate(&self) -> Option<&ApplicationDelegate> {
        match &self.backend {
            Backend::AppKit(delegate) => Some(delegate),
            Backend::Headless(_) => None,
        }
    }

    pub(super) const fn headless_state(&self) -> Option<&super::headless::LoopState> {
        match &self.backend {
            Backend::AppKit(_) => None,
            Backend::Headless(state) => Some(state),
        }
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        match &self.backend {
            Backend::AppKit(_) => monitor::available_monitors(),
            Backend::Headless(_) => VecDeque::new(),
        }
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        match &self.backend {
            Backend::AppKit(_) => monitor::primary_monitor(),
            Backend::Headless(_) => None,
        }
    }

    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        if self.headless_state().is_some() {
            return None;
        }
        let _ = self.mtm;
        // SAFETY: `-respondsToSelector:` (`B24@0:8:16`) and `-effectiveAppearance` (`@16@0:8`) are
        // valid on `NSApplication`, which answers a live `NSAppearance` borrowed for the statement.
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

    pub(crate) fn set_control_flow(&self, control_flow: ControlFlow) {
        match &self.backend {
            Backend::AppKit(delegate) => delegate.set_control_flow(control_flow),
            Backend::Headless(state) => state.set_control_flow(control_flow),
        }
    }

    pub(crate) fn exit(&self) {
        match &self.backend {
            Backend::AppKit(delegate) => delegate.exit(),
            Backend::Headless(state) => state.exit(),
        }
    }

    pub(crate) fn exiting(&self) -> bool {
        match &self.backend {
            Backend::AppKit(delegate) => delegate.exiting(),
            Backend::Headless(state) => state.exiting(),
        }
    }

    pub(crate) fn user_events_drained(&self) -> bool {
        match &self.backend {
            Backend::AppKit(delegate) => delegate.user_events_drained(),
            Backend::Headless(state) => state.user_events_drained(),
        }
    }
}

fn map_user_event<T: 'static>(
    mut handler: impl FnMut(Event<T>, &RootWindowTarget),
    receiver: Rc<wake_queue::Receiver<T>>,
) -> impl FnMut(Event<HandlePendingUserEvents>, &RootWindowTarget) {
    // LOCAL PATCH (aterm): the drain never waits on a sender (`wake_queue.rs`); events the handler
    // posts during it are handed out in it, as `try_iter` did.
    move |event, window_target| match event.map_nonuser_event() {
        Ok(event) => (handler)(event, window_target),
        Err(_) => {
            while let Some(event) = receiver.try_next() {
                (handler)(Event::UserEvent(event), window_target);
            }
        },
    }
}

/// macOS event loop backed by AppKit or the headless run loop (`headless.rs`).
///
/// LOCAL PATCH (aterm): the AppKit loop or the display-free one, chosen at `new` by the builder's
/// `with_headless`.
pub enum EventLoop<T: 'static> {
    AppKit(AppKitEventLoop<T>),
    Headless(super::headless::EventLoop<T>),
}

impl<T> EventLoop<T> {
    pub(crate) fn new(
        attributes: &PlatformSpecificEventLoopAttributes,
    ) -> Result<Self, EventLoopError> {
        // Before anything touches AppKit: the AppKit arm's first act is
        // `+[NSApplication sharedApplication]`.
        if attributes.headless {
            return super::headless::EventLoop::new().map(Self::Headless);
        }
        AppKitEventLoop::new(attributes).map(Self::AppKit)
    }

    pub const fn window_target(&self) -> &RootWindowTarget {
        match self {
            Self::AppKit(el) => el.window_target(),
            Self::Headless(el) => el.window_target(),
        }
    }

    pub fn run<F>(mut self, handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        self.run_on_demand(handler)
    }

    pub fn run_on_demand<F>(&mut self, handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        match self {
            Self::AppKit(el) => el.run_on_demand(handler),
            Self::Headless(el) => el.run_on_demand(handler),
        }
    }

    pub fn pump_events<F>(&mut self, timeout: Option<Duration>, handler: F) -> PumpStatus
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        match self {
            Self::AppKit(el) => el.pump_events(timeout, handler),
            Self::Headless(el) => el.pump_events(timeout, handler),
        }
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        match self {
            Self::AppKit(el) => el.create_proxy(),
            Self::Headless(el) => el.create_proxy(),
        }
    }
}

pub struct AppKitEventLoop<T: 'static> {
    /// Store a reference to the application for convenience.
    app: Obj,
    /// The application delegate that we've registered.
    delegate: aterm_objc::Retained<ApplicationDelegate>,

    sender: wake_queue::Sender<T>,
    receiver: Rc<wake_queue::Receiver<T>>,

    window_target: RootWindowTarget,
    panic_info: Rc<PanicInfo>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {
    pub(crate) activation_policy: Option<ActivationPolicy>,
    pub(crate) activate_ignoring_other_apps: bool,
    /// Selects the display-free headless event loop (`headless.rs`).
    ///
    /// LOCAL PATCH (aterm): set only by `with_headless`; the AppKit knobs above mean nothing to it.
    pub(crate) headless: bool,
}

impl Default for PlatformSpecificEventLoopAttributes {
    fn default() -> Self {
        Self { activation_policy: None, activate_ignoring_other_apps: true, headless: false }
    }
}

impl<T> AppKitEventLoop<T> {
    fn new(attributes: &PlatformSpecificEventLoopAttributes) -> Result<Self, EventLoopError> {
        let mtm =
            MainThread::new().expect("on macOS, `EventLoop` must be created on the main thread!");

        // SAFETY: `+sharedApplication` returns the process-lifetime singleton, retained here.
        let app = unsafe { Obj::retain(app()) }.expect("+sharedApplication to answer");

        let activation_policy = match attributes.activation_policy {
            None => None,
            Some(ActivationPolicy::Regular) => Some(NS_APPLICATION_ACTIVATION_POLICY_REGULAR),
            Some(ActivationPolicy::Accessory) => Some(NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY),
            Some(ActivationPolicy::Prohibited) => Some(NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED),
        };
        let (sender, receiver) = wake_queue::channel();
        let delegate = ApplicationDelegate::new(
            mtm,
            activation_policy,
            attributes.activate_ignoring_other_apps,
            receiver.armed(),
            receiver.drained(),
        );

        autoreleasepool(|_| {
            // SAFETY: `-setDelegate:` (`v24@0:8@16`) takes a weak reference, so `EventLoop` keeps
            // its own handle to the retained `NSApplicationDelegate`.
            unsafe { send_v_id(app.id(), sel!(setDelegate:), delegate.as_delegate_id()) };
        });

        // SAFETY: `app` owns a +1 reference to the live shared `NSApplication`.
        unsafe { override_send_event(app.id(), mtm) };

        let panic_info: Rc<PanicInfo> = Default::default();
        // LOCAL PATCH (aterm): the `sendEvent:` override's panic guard is `stop_app_on_panic`, so
        // it gets the observers' slot.
        set_send_event_panic_info(mtm, Rc::downgrade(&panic_info));
        setup_control_flow_observers(mtm, Rc::downgrade(&panic_info));

        Ok(AppKitEventLoop {
            app,
            delegate: delegate.clone_retained(),
            sender,
            receiver: Rc::new(receiver),
            window_target: RootWindowTarget {
                p: ActiveEventLoop { backend: Backend::AppKit(delegate), mtm },
                _marker: PhantomData,
            },
            panic_info,
        })
    }

    const fn window_target(&self) -> &RootWindowTarget {
        &self.window_target
    }

    pub fn run_on_demand<F>(&mut self, handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &RootWindowTarget),
    {
        let handler = map_user_event(handler, self.receiver.clone());

        self.delegate.set_event_handler(handler, || {
            autoreleasepool(|_| {
                self.delegate.set_wait_timeout(None);
                self.delegate.set_stop_before_wait(false);
                self.delegate.set_stop_after_wait(false);
                self.delegate.set_stop_on_redraw(false);

                if self.delegate.is_launched() {
                    debug_assert!(!self.delegate.is_running());
                    self.delegate.set_is_running(true);
                    self.delegate.dispatch_init_events();
                }

                // SAFETY: `-run` (`v16@0:8`) is not entered re-entrantly.
                unsafe { send_v(self.app.id(), sel!(run)) };

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
                    // SAFETY: `-run` is not entered re-entrantly.
                    unsafe { send_v(self.app.id(), sel!(run)) };
                } else if !self.delegate.is_running() {
                    self.delegate.set_is_running(true);
                    self.delegate.dispatch_init_events();
                } else {
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
                    // SAFETY: `-run` is not entered re-entrantly.
                    unsafe { send_v(self.app.id(), sel!(run)) };
                }

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

/// Stops the shared `NSApplication` immediately and posts a dummy wake event.
/// # Safety
/// `app` must be the live shared `NSApplication`.
pub(super) unsafe fn stop_app_immediately(app: Id) {
    autoreleasepool(|_| {
        // SAFETY: `-stop:` (`v24@0:8@16`) and `-postEvent:atStart:` (`v32@0:8@16B24`) are valid on
        // `NSApplication`.
        unsafe {
            send_v_id(app, sel!(stop:), Id::NIL);
            let dummy = dummy_event().expect("NSEvent refused the dummy event");
            send_v_id_bool(app, sel!(postEvent:atStart:), dummy.id(), true);
        }
    });
}

/// Closes all application windows on exit.
/// # Safety
/// `app` must be the live shared `NSApplication`.
pub(super) unsafe fn notify_windows_of_exit(app: Id) {
    // LOCAL PATCH (aterm): the array is walked by index, in an explicit pool: `-windows` is +0
    // autoreleased and `-close` runs a whole teardown inside it.
    autoreleasepool(|_| {
        // SAFETY: `-windows`, `-count`, `-objectAtIndex:` and `-close` match AppKit signatures on
        // `app`; `-windows` answers a snapshot, so a `-close` cannot shift the indices.
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

/// Catches panics inside `f` and stops the shared `NSApplication`.
#[inline]
pub fn stop_app_on_panic<F: FnOnce() -> R + UnwindSafe, R>(
    mtm: MainThread,
    panic_info: Weak<PanicInfo>,
    f: F,
) -> Option<R> {
    match catch_unwind(f) {
        Ok(r) => Some(r),
        Err(e) => {
            // LOCAL PATCH (aterm): no `unwrap`. This guard is the outer half of the `"C-unwind"`
            // `sendEvent:` override, so a second panic would unwind into AppKit; a gone `PanicInfo`
            // is a named abort.
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
    sender: wake_queue::Sender<T>,
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
        // LOCAL PATCH (aterm): cloning a sender must not wake the loop. Share its
        // registered source; constructing another one wakes an otherwise idle park.
        // SAFETY: `self.source` is live and the clone's retain is balanced by `Drop`.
        unsafe { CFRetain(self.source as _) };
        EventLoopProxy { sender: self.sender.clone(), source: self.source }
    }
}

impl<T> EventLoopProxy<T> {
    /// Creates a proxy sharing the headless loop's wake `source`.
    ///
    /// LOCAL PATCH (aterm): not [`Self::new`]'s own source. The display-free loop wakes only for
    /// a source it handles, and CoreFoundation calls two version-0 sources equal when order,
    /// callbacks and `info` match, so a second `new` source is never added to the run loop.
    ///
    /// # Safety
    /// `source` must be a live version-0 source in the main run loop.
    pub(super) unsafe fn sharing(
        sender: wake_queue::Sender<T>,
        source: CFRunLoopSourceRef,
    ) -> Self {
        // SAFETY: caller guarantees `source` is live; retain is balanced by `Drop`.
        unsafe { CFRetain(source as _) };
        EventLoopProxy { sender, source }
    }

    fn new(sender: wake_queue::Sender<T>) -> Self {
        unsafe {
            const extern "C" fn event_loop_proxy_handler(_: *mut c_void) {}

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
            let source = CFRunLoopSourceCreate(ptr::null(), CFIndex::MAX - 1, &mut context);
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            wake_main();

            EventLoopProxy { sender, source }
        }
    }

    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        // LOCAL PATCH (aterm): only the send that armed the wake signals, and only after its
        // push. A drain that found the queue's lock held relies on that signal, and the sends
        // behind it until the next take are drained by the turn it brings (`wake_queue.rs`).
        if self.sender.send(event).map_err(EventLoopClosed)? {
            self.signal();
        }
        Ok(())
    }

    /// [`Self::send_event`] by a sender held part-way through its push
    /// (`wake_queue::Sender::send_held`), which signals as `send_event` does once it lets go.
    #[cfg(any(test, debug_assertions))]
    pub fn send_event_held_for_test(
        &self,
        event: T,
        held: &std::sync::mpsc::Sender<()>,
        release: &std::sync::mpsc::Receiver<()>,
    ) -> Result<(), EventLoopClosed<T>> {
        if self.sender.send_held(event, held, release).map_err(EventLoopClosed)? {
            self.signal();
        }
        Ok(())
    }

    fn signal(&self) {
        // SAFETY: `self.source` is a source this proxy retains until `Drop`, and signalling a
        // source may be done from any thread.
        unsafe { CFRunLoopSourceSignal(self.source) };
        wake_main();
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread woke the main run loop.
    static MAIN_WAKES: Cell<usize> = const { Cell::new(0) };
}

/// Wakes the main run loop.
fn wake_main() {
    #[cfg(test)]
    MAIN_WAKES.with(|wakes| wakes.set(wakes.get() + 1));
    // SAFETY: the main run loop lives for the process, and waking it may be done from any thread.
    unsafe { CFRunLoopWakeUp(CFRunLoopGetMain()) }
}

fn app() -> Id {
    // SAFETY: `+sharedApplication` (`@16#0:8`) returns the `NSApplication` singleton.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFGetRetainCount(cf: *const c_void) -> CFIndex;
    }

    fn main_wakes() -> usize {
        MAIN_WAKES.with(Cell::get)
    }

    fn retains(source: CFRunLoopSourceRef) -> CFIndex {
        // SAFETY: the caller holds a proxy that retains `source`, so it is a live CF object.
        unsafe { CFGetRetainCount(source as *const c_void) }
    }

    #[test]
    fn a_proxy_clone_wakes_nothing_and_a_send_through_it_wakes_the_loop_once() {
        let (sender, receiver) = wake_queue::channel::<u32>();
        let proxy = EventLoopProxy::new(sender);
        let source = proxy.source;
        let held = retains(source);
        let before = main_wakes();
        let clones: Vec<_> = (0..64).map(|_| proxy.clone()).collect();
        assert_eq!(main_wakes(), before, "a clone is no event, so it wakes nothing");
        assert!(clones.iter().all(|clone| clone.source == source), "a clone shares its source");
        assert_eq!(retains(source), held + 64, "each clone holds its own retain");
        let kept = clones[0].clone();
        drop(clones);
        drop(proxy);
        assert_eq!(retains(source), held, "each dropped proxy gave its retain back");
        kept.send_event(7).expect("the receiver is alive");
        assert_eq!(main_wakes(), before + 1, "a send wakes the loop once");
        assert_eq!(receiver.try_next(), Some(7));
    }
}
