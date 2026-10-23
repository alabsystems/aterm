// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/window.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#![allow(clippy::unnecessary_cast)]

use aterm_objc::send::send_v;
use aterm_objc::{autoreleasepool, sel, Bool, Id, MainThread, MainThreadBound};

use super::event_loop::ActiveEventLoop;
use super::window_delegate::WindowDelegate;
use crate::error::OsError as RootOsError;
use crate::window::WindowAttributes;

pub(crate) struct Window {
    window: MainThreadBound<aterm_objc::Retained<WinitWindow>>,
    /// The window only keeps a weak reference to this, so we must keep it around here.
    delegate: MainThreadBound<aterm_objc::Retained<WindowDelegate>>,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.get_on_main(|window| {
            autoreleasepool(|_| {
                // SAFETY: `window` borrows a live `WinitWindow`, which is registered with `NSWindow` as its superclass; `-close` is `v16@0:8` on `NSWindow`.
                unsafe { send_v(window.as_id(), sel!(close)) };
            })
        })
    }
}

impl Window {
    pub(crate) fn new(
        window_target: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let mtm = window_target.mtm;
        let Some(app_delegate) = window_target.app_delegate() else {
            return Err(os_error!(super::OsError::CreationError(
                "cannot create a window on a headless event loop: it has no display"
            )));
        };
        let delegate = autoreleasepool(|_| {
            // LOCAL PATCH (aterm), W9: `WindowDelegate::new` takes a witness now.
            WindowDelegate::new(app_delegate, attributes, mtm)
        })?;
        Ok(Window {
            window: MainThreadBound::new(delegate.window().retained(), mtm),
            delegate: MainThreadBound::new(delegate, mtm),
        })
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&WindowDelegate) + Send + 'static) {
        // For now, don't actually do queuing, since it may be less predictable
        self.maybe_wait_on_main(f)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(
        &self,
        f: impl FnOnce(&WindowDelegate) -> R + Send,
    ) -> R {
        self.delegate.get_on_main(|delegate| f(delegate))
    }

    /// LOCAL PATCH (aterm), 2026-10-15: the content view behind `aterm_objc::HasNsView` (`platform/macos.rs`); nil off the main thread, where the `rwh_06` handle below answers `Unavailable`.
    #[inline]
    pub(crate) fn ns_view(&self) -> Id {
        MainThread::new().map_or(Id::NIL, |mtm| self.delegate.get(mtm).ns_view())
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub(crate) fn raw_window_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        if let Some(mtm) = MainThread::new() {
            Ok(self.delegate.get(mtm).raw_window_handle_rwh_06())
        } else {
            Err(rwh_06::HandleError::Unavailable)
        }
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub(crate) fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::AppKit(rwh_06::AppKitDisplayHandle::new()))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(pub usize);

impl WindowId {
    pub const fn dummy() -> Self {
        Self(0)
    }
}

impl From<WindowId> for u64 {
    fn from(window_id: WindowId) -> Self {
        window_id.0 as u64
    }
}

impl From<u64> for WindowId {
    fn from(raw_id: u64) -> Self {
        Self(raw_id as usize)
    }
}

aterm_objc::declare_class! {
    /// The `NSWindow` subclass every winit window on macOS is an instance of.
    #[derive(Debug)]
    pub struct WinitWindow: NSWindow {
        const NAME: &str = "WinitWindow";
        type Ivars = ();

        @sel(canBecomeMainWindow) @abort_on_exception
        fn can_become_main_window(&self) -> Bool {
            trace_scope!("canBecomeMainWindow");
            Bool::YES
        }

        @sel(canBecomeKeyWindow) @abort_on_exception
        fn can_become_key_window(&self) -> Bool {
            trace_scope!("canBecomeKeyWindow");
            Bool::YES
        }
    }
}

impl WinitWindow {
    pub(super) fn id(&self) -> WindowId {
        WindowId(self as *const Self as usize)
    }

    /// The `WindowId` of the `WinitWindow` at `window`.
    pub(super) fn id_of(window: Id) -> WindowId {
        WindowId(window.addr())
    }

    /// A +1 handle to this window — objc2's `NSObjectProtocol::retain`, which this class no longer inherits.
    pub(super) fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is a live non-null receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }.expect("retaining a live WinitWindow")
    }
}
