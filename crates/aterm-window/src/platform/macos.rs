// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/macos.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE.
//!
//! # macOS / AppKit
//!
//! Winit has an OS requirement of macOS 10.11 or higher (same as Rust
//! itself), and is regularly tested on macOS 10.14.
//!
//! ## Window initialization
//!
//! A lot of functionality expects the application to be ready before you
//! start doing anything; this includes creating windows, fetching monitors,
//! drawing, and so on, see issues [#2238], [#2051] and [#2087].
//!
//! If you encounter problems, you should try doing your initialization inside
//! [`ApplicationHandler::resumed`].
//!
//! [#2238]: https://github.com/rust-windowing/winit/issues/2238
//! [#2051]: https://github.com/rust-windowing/winit/issues/2051
//! [#2087]: https://github.com/rust-windowing/winit/issues/2087
//! [`ApplicationHandler::resumed`]: crate::application::ApplicationHandler::resumed
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

/// Register a handler consulted before the application terminates via AppKit's
/// `terminate:` — the Dock-icon "Quit", AppleScript `quit`, and logout/restart/shutdown,
/// none of which pass through the application menu's Quit item (so an app that handles
/// its own ⌘Q cannot otherwise confirm them). Return `true` to allow termination,
/// `false` to cancel it (e.g. after a "really quit?" dialog). The handler runs on the
/// main thread inside `applicationShouldTerminate:`, so it may show a modal. Only the
/// first registration takes effect; without one, termination always proceeds — the
/// default, so stock winit behavior is unchanged.
pub fn set_application_should_terminate_handler(handler: fn() -> bool) {
    crate::platform_impl::set_quit_confirm_hook(handler);
}

/// Additional methods on [`Window`] that are specific to MacOS.
///
/// LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5): the
/// methods aterm calls. Upstream's simple fullscreen, shadow, native tabbing, document-edited and
/// borderless-game controls are cut, with `WindowAttributesExtMacOS` (whose titlebar, shadow,
/// HiDPI, first-mouse, tabbing and option-as-alt settings now stand at the defaults aterm always
/// ran with; option-as-alt is set after creation, [`WindowExtMacOS::set_option_as_alt`]),
/// `ActiveEventLoopExtMacOS`, `EventLoopBuilderExtMacOS::with_default_menu` (winit's default
/// menu, and with it the Services menu registration, is still always built) and
/// `MonitorHandleExtMacOS::ns_screen`. Option-as-alt was cut in step 5 and restored on
/// 2026-10-16 (ATERM-KI-1015-9: aterm's Meta mode needs it).
pub trait WindowExtMacOS {
    /// LOCAL PATCH (aterm): the modifier state winit last derived from an
    /// event for this window — its own cache, written before the matching
    /// `ModifiersChanged` is queued. For re-syncing after a contained
    /// `NSException`; never a WindowServer-backed probe.
    fn cached_modifiers(&self) -> ModifiersState;

    /// Make the Option keys `option_as_alt` names an Alt for this window's key events: AppKit
    /// is handed the event with the characters the key types without Option
    /// (`-charactersIgnoringModifiers`), so Option neither composes a glyph (Option+a, "å")
    /// nor starts a dead key (Option+e), and the key event's text is the layout's character
    /// at the Shift level. The Alt modifier is still reported. Not for a chord with Control
    /// or Command, whose text AppKit already reads without Option.
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
/// LOCAL PATCH (aterm), 2026-10-16: upstream's type, restored with the setter.
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

// LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6):
// the window's content view through first-party `aterm_objc::HasNsView`, so the Metal presenter
// (aterm-gpu) and aterm-gui's chrome reach it without `raw-window-handle`, whose `rwh_06`
// feature is no longer a default. The opt-in `rwh_06` handle wraps this same view.
//
// SAFETY: the view is the `WinitView` installed as the window's content view at creation and
// never replaced. The `NSWindow` retains it, and this `Window` retains the `NSWindow` until it
// drops, so the view outlives `self`. Off the main thread the answer is nil.
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
    /// Sets the activation policy for the application. If used, this will override
    /// any relevant settings provided in the package manifest.
    /// For instance, `with_activation_policy(ActivationPolicy::Regular)` will prevent
    /// the application from running as an "agent", even if LSUIElement is set to true.
    ///
    /// If unused, the Winit will honor the package manifest.
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

    /// Used to prevent the application from automatically activating when launched if
    /// another application is already active.
    ///
    /// The default behavior is to ignore other applications and activate when launched.
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
