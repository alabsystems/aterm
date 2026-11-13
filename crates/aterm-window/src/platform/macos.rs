// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/macos.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! ## Application termination
//!
//! This fork installs its own `NSApplicationDelegate` to deliver application lifecycle events.
//! Keep that delegate in place. To decide what AppKit hears when it asks whether the application
//! may terminate, register a callback with [`set_application_should_terminate_handler`]. It runs
//! on the main thread, is told whether a reply could come from AppKit's wait ([`ReplyWait`]), and
//! answers a [`TerminateAnswer`]. Only the first registration takes effect.
//!
//! A quit the application takes up ([`TerminateAnswer::Later`]) is answered once: YES when the
//! event loop next exits, after its `LoopExiting` (in which the application does its shutdown,
//! since inside AppKit's `terminate:` `run_app` never returns: [`exit_ends_in_appkit`]), or NO
//! from [`refuse_deferred_termination`]. A logout, restart or shutdown goes on after the YES and
//! is abandoned after the NO. A `Later` to an ask [`ReplyWait::Unanswerable`] describes (AppKit
//! asking from inside the application's own handler) is refused instead, since no reply to it
//! could come.
//!
//! ```no_run
//! use aterm_window::platform::macos::{
//!     set_application_should_terminate_handler, ReplyWait, TerminateAnswer,
//! };
//!
//! fn take_up_the_quit(wait: ReplyWait) -> TerminateAnswer {
//!     match wait {
//!         ReplyWait::Answerable => TerminateAnswer::Later,
//!         ReplyWait::Unanswerable => TerminateAnswer::Cancel,
//!     }
//! }
//!
//! set_application_should_terminate_handler(take_up_the_quit);
//! ```

use crate::event::InputOrigin;
use crate::event_loop::EventLoopBuilder;
use crate::keyboard::ModifiersState;
use crate::monitor::MonitorHandle;
use crate::window::Window;

pub use crate::platform_impl::{ReplyWait, TerminateAnswer};

/// Complete AppKit launch before entering the application's event loop.
///
/// No application callbacks or windows are created. A subsequent run delivers
/// the normal initialization events. Headless loops do nothing.
pub trait EventLoopExtMacOS {
    /// Finish native launch while the caller can still serve its old windows.
    fn prepare_launch(&mut self);
}

impl<T: 'static> EventLoopExtMacOS for crate::event_loop::EventLoop<T> {
    fn prepare_launch(&mut self) {
        self.event_loop.prepare_launch();
    }
}

/// Register the handler that answers AppKit's `applicationShouldTerminate:` on the main thread.
#[inline]
pub fn set_application_should_terminate_handler(handler: fn(ReplyWait) -> TerminateAnswer) {
    crate::platform_impl::set_quit_confirm_hook(handler);
}

/// Refuse the quit AppKit waits on (a [`TerminateAnswer::Later`]): it hears NO, once, and a
/// logout, restart or shutdown it stood for is abandoned. False, with nothing sent, when AppKit
/// waits on no reply, as on any thread but the main one, and while the event loop's exit is
/// delivered inside AppKit ([`exit_ends_in_appkit`]): that exit ends the process, so AppKit hears
/// YES after it whatever is cancelled in it.
#[inline]
pub fn refuse_deferred_termination() -> bool {
    crate::platform_impl::refuse_deferred_termination()
}

/// Whether the event loop's exit being delivered now (`ApplicationHandler::exiting`) happens
/// inside AppKit's `terminate:`, which ends the process when it returns: after a deferred quit's
/// exit, AppKit hears YES, and after `applicationWillTerminate:` it exits by itself. `run_app`
/// never returns then, so what the application does after it must be done in `exiting`.
#[inline]
pub fn exit_ends_in_appkit() -> bool {
    crate::platform_impl::exit_ends_in_appkit()
}

/// The original native key, modifier or mouse event before Option handling or input-method
/// interpretation.
///
/// LOCAL PATCH (aterm): identity only; the window layer assigns it no input authority.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NativeInputEvent {
    /// The `NSEventType`: `NSEventTypeKeyDown` (10), `NSEventTypeKeyUp` (11) or
    /// `NSEventTypeFlagsChanged` (12) for a keyboard event, or a mouse button's down or up or
    /// `NSEventTypeScrollWheel` (22).
    pub kind: usize,
    /// The native virtual keycode of a key or changed modifier; 0 for a mouse event, which has
    /// none.
    pub keycode: u16,
    /// The native timestamp in seconds since boot.
    pub timestamp: f64,
    /// The Unix process ID of the process the event came from (`kCGEventSourceUnixProcessID`,
    /// [`aterm_cg::event_source_pid`]): the process that posted it, or 0 for the HID system.
    pub source_pid: i64,
}

/// Install the application's classifier before its first window receives a key, modifier change
/// or click.
///
/// The first registration takes effect. Each native key, modifier-change, mouse-button and scroll
/// event is classified once before it can generate window callbacks; that application's decision
/// is retained through deferred delivery. The window layer does not interpret the decision or
/// grant any authority.
#[inline]
pub fn set_window_input_classifier(classifier: fn(NativeInputEvent) -> InputOrigin) {
    crate::platform_impl::set_window_input_classifier(classifier);
}

/// The application's retained source decision for this window-event callback.
///
/// `None` means outside retained callback delivery. `Some(InputOrigin::Person)` also names
/// callbacks generated without a classified native event, so they never borrow a later AppKit
/// event's identity.
#[inline]
pub fn window_event_origin() -> Option<InputOrigin> {
    crate::platform_impl::window_event_origin()
}

/// Printable ASCII character produced only via an Option chord on `key` under the active keyboard layout.
///
/// LOCAL PATCH (aterm): added; reads the layout with `UCKeyTranslate`, on the main thread.
#[inline]
pub fn option_only_character(key: crate::keyboard::PhysicalKey, shift: bool) -> Option<char> {
    crate::platform_impl::option_only_character(key, shift)
}

/// LOCAL PATCH (aterm): a weak reference (`objc_initWeak`) to the `CADisplayLink` that `window`'s
/// view holds now for its frame clock (`Window::set_frame_clock`); `None` before the clock's first
/// run, once the window closed, and off the main thread. A seam for `aterm-gui`'s
/// `objc_frame_clock_drive`, which reads the link's own `-isPaused` through it, and takes it
/// before a close and loads it (`objc_loadWeakRetained`) after: the main run loop the link was
/// added to retains it until `-invalidate`, so it loads nil only once the link was invalidated and
/// nothing else holds it.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn frame_clock_link(window: &Window) -> Option<aterm_objc::WeakObj> {
    window.window.frame_clock_link()
}

/// LOCAL PATCH (aterm): a weak reference to every `CADisplayLink` that `window`'s view made for its
/// frame clock, in the order made, each recorded where the link's maker returned it; empty off the
/// main thread. A seam for `aterm-gui`'s `objc_frame_clock_drive`, which takes them before a close
/// and requires each to load nil after it, so a link the view replaced without `-invalidate`
/// (which the main run loop goes on retaining) is seen as well as the one it held at the close.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn frame_clock_links(window: &Window) -> Vec<aterm_objc::WeakObj> {
    window.window.frame_clock_links()
}

/// Additional methods on [`Window`] that are specific to macOS.
///
/// LOCAL PATCH (aterm): the methods aterm calls. Upstream's simple fullscreen, shadow, tabbing,
/// document-edited and borderless-game controls are gone, with `WindowAttributesExtMacOS` (its
/// settings stand at their defaults), `ActiveEventLoopExtMacOS`,
/// `EventLoopBuilderExtMacOS::with_default_menu` (the default menu is always built) and
/// `MonitorHandleExtMacOS::ns_screen`.
pub trait WindowExtMacOS {
    /// Modifier state last derived from an event for this window.
    ///
    /// LOCAL PATCH (aterm): winit's own cache, written before the matching `ModifiersChanged` is
    /// queued, for re-syncing after a contained NSException; never a WindowServer probe.
    fn cached_modifiers(&self) -> ModifiersState;

    /// Configure which Option keys are treated as Alt for this window's key events.
    fn set_option_as_alt(&self, option_as_alt: OptionAsAlt);

    /// What [`WindowExtMacOS::set_option_as_alt`] last set ([`OptionAsAlt::None`] until then).
    fn option_as_alt(&self) -> OptionAsAlt;
}

impl WindowExtMacOS for Window {
    #[inline]
    fn cached_modifiers(&self) -> ModifiersState {
        self.window.maybe_wait_on_main(|w| w.cached_modifiers())
    }

    #[inline]
    fn set_option_as_alt(&self, option_as_alt: OptionAsAlt) {
        self.window.maybe_queue_on_main(move |w| w.set_option_as_alt(option_as_alt))
    }

    #[inline]
    fn option_as_alt(&self) -> OptionAsAlt {
        self.window.maybe_wait_on_main(|w| w.option_as_alt())
    }
}

/// Which Option keys a window treats as Alt ([`WindowExtMacOS::set_option_as_alt`]).
///
/// LOCAL PATCH (aterm): upstream's type, kept with the setter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionAsAlt {
    /// The left Option key is Alt; the right one composes.
    OnlyLeft,
    /// The right Option key is Alt; the left one composes.
    OnlyRight,
    /// Both Option keys are Alt.
    Both,
    /// Neither: both compose, AppKit's default.
    #[default]
    None,
}

// LOCAL PATCH (aterm): the content view through first-party `aterm_objc::HasNsView`, so the Metal
// presenter and aterm-gui's chrome reach it without `raw-window-handle` (upstream's `rwh_06`
// handles are cut).
// SAFETY: the content view is installed at window creation and never replaced; the `NSWindow`
// retains it and this `Window` retains the `NSWindow` until it drops, so the view outlives `self`.
unsafe impl aterm_objc::HasNsView for Window {
    #[inline]
    fn ns_view(&self) -> aterm_objc::Id {
        self.window.ns_view()
    }
}

/// Corresponds to `NSApplicationActivationPolicy`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivationPolicy {
    /// Corresponds to `NSApplicationActivationPolicyRegular`.
    #[default]
    Regular,

    /// Corresponds to `NSApplicationActivationPolicyAccessory`.
    Accessory,

    /// Corresponds to `NSApplicationActivationPolicyProhibited`.
    Prohibited,
}

pub trait EventLoopBuilderExtMacOS {
    /// Set the macOS activation policy for the application.
    ///
    /// # Example
    ///
    /// Set the activation policy to "accessory".
    ///
    /// ```
    /// use aterm_window::event_loop::EventLoop;
    /// #[cfg(target_os = "macos")]
    /// use aterm_window::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
    ///
    /// let mut builder = EventLoop::builder();
    /// #[cfg(target_os = "macos")]
    /// builder.with_activation_policy(ActivationPolicy::Accessory);
    /// # if false { // We can't test this part
    /// let event_loop = builder.build();
    /// # }
    /// ```
    fn with_activation_policy(&mut self, activation_policy: ActivationPolicy) -> &mut Self;

    /// Configure whether the application activates automatically on launch when another app is active.
    fn with_activate_ignoring_other_apps(&mut self, ignore: bool) -> &mut Self;
}

impl<T> EventLoopBuilderExtMacOS for EventLoopBuilder<T> {
    #[inline]
    fn with_activation_policy(&mut self, activation_policy: ActivationPolicy) -> &mut Self {
        self.platform_specific.activation_policy = Some(activation_policy);
        self
    }

    #[inline]
    fn with_activate_ignoring_other_apps(&mut self, ignore: bool) -> &mut Self {
        self.platform_specific.activate_ignoring_other_apps = ignore;
        self
    }
}

/// Additional methods on [`MonitorHandle`] that are specific to MacOS.
pub trait MonitorHandleExtMacOS {
    /// Returns the identifier of the monitor for Cocoa.
    fn native_id(&self) -> u32;
}

impl MonitorHandleExtMacOS for MonitorHandle {
    #[inline]
    fn native_id(&self) -> u32 {
        self.inner.native_identifier()
    }
}
