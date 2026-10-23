// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/macos.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.

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
#[inline]
pub fn option_only_character(key: crate::keyboard::PhysicalKey, shift: bool) -> Option<char> {
    crate::platform_impl::option_only_character(key, shift)
}

/// Additional methods on [`Window`] that are specific to macOS.
pub trait WindowExtMacOS {
    /// Modifier state last derived from an event for this window.
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

// SAFETY: the content view is installed at window creation and retained by `NSWindow` for the life of `self`.
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
