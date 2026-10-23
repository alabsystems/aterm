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
//! Keep that delegate in place. To decide whether AppKit may terminate the application,
//! register a callback with [`set_application_should_terminate_handler`]. It runs on the main
//! thread; returning `false` cancels termination. Only the first registration takes effect.
//!
//! ```no_run
//! use aterm_window::platform::macos::set_application_should_terminate_handler;
//!
//! fn allow_termination() -> bool {
//!     true
//! }
//!
//! set_application_should_terminate_handler(allow_termination);
//! ```

use crate::event_loop::EventLoopBuilder;
use crate::keyboard::ModifiersState;
use crate::monitor::MonitorHandle;
use crate::window::Window;

/// Register a handler consulted on the main thread before AppKit terminates the application.
#[inline]
pub fn set_application_should_terminate_handler(handler: fn() -> bool) {
    crate::platform_impl::set_quit_confirm_hook(handler);
}

/// Printable ASCII character produced only via an Option chord on `key` under the active keyboard layout.
///
/// LOCAL PATCH (aterm): added; reads the layout with `UCKeyTranslate`, on the main thread.
#[inline]
pub fn option_only_character(key: crate::keyboard::PhysicalKey, shift: bool) -> Option<char> {
    crate::platform_impl::option_only_character(key, shift)
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
// presenter and aterm-gui's chrome reach it without `raw-window-handle` (`rwh_06` is opt-in and
// wraps this view).
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
