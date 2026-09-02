// Modified by the aterm project in 2026; see the repository NOTICE.
// (The window delegate AND the window class are now declared with
//  `aterm_objc::declare_class!`, so the handles this file stores for them are
//  that crate's `Retained`, not objc2's.)
#![allow(clippy::unnecessary_cast)]

use aterm_objc::{Bool, Id};
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWindow;
use objc2_foundation::{MainThreadBound, MainThreadMarker};

use super::aterm_objc_seam;

use super::event_loop::ActiveEventLoop;
use super::window_delegate::WindowDelegate;
use crate::error::OsError as RootOsError;
use crate::window::WindowAttributes;

pub(crate) struct Window {
    window: MainThreadBound<aterm_objc::Retained<WinitWindow>>,
    /// The window only keeps a weak reference to this, so we must keep it around here.
    ///
    // LOCAL PATCH (aterm): `aterm_objc::Retained`, not objc2's — `WindowDelegate`
    // is declared by `aterm_objc::declare_class!` and so is no longer an objc2
    // `ClassType`. `MainThreadBound<T>` is generic over any `T` (its `Send`/`Sync`
    // come from the marker it is constructed with, not from `T`), and
    // `aterm_objc::Retained<T>` derefs to `T` exactly as objc2's does, so
    // `get_on_main`'s callers below are unchanged.
    delegate: MainThreadBound<aterm_objc::Retained<WindowDelegate>>,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.get_on_main(|window| autoreleasepool(|_| window.ns_window().close()))
    }
}

impl Window {
    pub(crate) fn new(
        window_target: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, RootOsError> {
        let mtm = window_target.mtm;
        let delegate = autoreleasepool(|_| {
            // LOCAL PATCH (aterm), W9: `WindowDelegate::new` takes a witness now.
            // This file keeps `MainThreadMarker` as its currency because
            // `MainThreadBound` — an objc2 CONTAINER, not a marker — consumes one
            // in `new` and `get`; see the note above `Window`.
            WindowDelegate::new(
                window_target.app_delegate(),
                attributes,
                aterm_objc_seam::witness(mtm),
            )
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

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub(crate) fn raw_window_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        if let Some(mtm) = MainThreadMarker::new() {
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

// LOCAL PATCH (aterm): the class pair, the ivar slot, the `dealloc` and both
// trampolines below are declared by `aterm_objc::declare_class!` rather than
// `objc2::declare_class!` — the same move `window_delegate.rs`, `view.rs` and
// `app_state.rs` made. The registered rows are READ OFF THE LIVE CLASS by
// `crates/aterm-gui/examples/objc_live_class_audit.rs` on the window the
// audited view is attached to.
//
// THE SUPERCLASS IS THE FINDING, and it was measured rather than carried over
// from `NSView`. `WinitWindow` is the SECOND non-`NSObject` superclass this
// crate registers, and it differs from the first in the only two ways that
// matter to the ivar slot:
//
//  1. THE OFFSET IS NOT THE INSTANCE SIZE, and on `NSWindow` it is not even
//     ALIGNED. `-[NSWindow class]`'s instance size is 520 bytes, and
//     `class_addIvar` places a 1-byte, align-1 slot — which is what
//     `IvarSlot<()>` is — at offset **513**, inside the tail padding. An
//     8-byte slot lands at 520 and a 16-byte one at 528. (`NSView` answers 536
//     for the 8-byte case.) Nothing here may be derived from a size: the macro
//     reads `ivar_getOffset` after registration and that is the only correct
//     source. Measured with `class_addIvar`/`ivar_getOffset` against
//     `NSWindow` itself, three slot shapes.
//  2. THE DESIGNATED INITIALIZER IS NOT `-init`, AND IT IS ONE THAT MAY RETURN
//     A DIFFERENT INSTANCE THAN `+alloc` PRODUCED. `-[NSWindow
//     initWithContentRect:styleMask:backing:defer:]` is an `id`-returning
//     initializer like any other, and an initializer is entitled to release
//     `self` and answer another object; `alloc_init` would be wrong here for
//     the same reason it is wrong for a view. `alloc_ivars` is what this class
//     uses, and the send is written out in `window_delegate.rs::new_window`.
//
//     MEASURED, on this box, 1,024 windows: `same=1024/1024`,
//     `class-kept=1024/1024`, `ivar-survived=1024/1024`, across all four style
//     masks `new_window` can build (Titled|Closable|Miniaturizable|Resizable,
//     Borderless|Resizable|Miniaturizable, Titled|FullSizeContentView, bare
//     Borderless) x `defer:` both ways, and unchanged after
//     `-setTabbingIdentifier:`/`-setTabbingMode:`/`-close`. So the substitution
//     does not happen for the sends this fork makes.
//
//     IT IS STILL NOT PROMISED, and the sentence that used to price it is
//     wrong for three survivors in four. It said the SURVIVOR's slot reads
//     `initialized == false` because `class_createInstance` zero-fills, so
//     `IvarSlot::get` panics by name: true of a same-class survivor ONLY. A
//     plain `NSWindow` panics because 513 is its TAIL PADDING and it is still
//     zero; a SIBLING subclass's own first ivar lands at 513 and its honest
//     `true` is read as ours, so `get` hands out a `&T` over another class's
//     bytes; an `NSObject`-sized survivor reads 497 bytes PAST its allocation.
//     Measured in `winit_seam.rs::a_substituting_initializer_defeats_the_
//     initialized_flag`, mechanism on `IvarSlot::get`. Zero cost HERE (`Ivars`
//     = `()`); a class carrying STATE must prove its init does not substitute.
aterm_objc::declare_class! {
    /// The `NSWindow` subclass every winit window on macOS is an instance of.
    ///
    /// Two rows, no ivars and no protocols — objc2's `declare_class!` called
    /// `class_addProtocol` zero times here, so the `protocols:` list is absent
    /// rather than empty, and the audit's `claimed` list for this class is `[]`.
    #[derive(Debug)]
    pub struct WinitWindow: NSWindow {
        const NAME: &str = "WinitWindow";
        type Ivars = ();

        // Both rows are `Bool`, and that is the runtime's answer rather than
        // upstream's: `NSWindow` implements `-canBecomeMainWindow` and
        // `-canBecomeKeyWindow` as `B16@0:8` (measured; no protocol `NSWindow`
        // claims declares either). Upstream types them `-> bool`, which objc2
        // also encodes `B`, so the two agree — unlike `draggingEntered:`, where
        // upstream's `-> bool` registered a one-byte `B` against a protocol
        // declaring an eight-byte `Q` and the port had to CHANGE the type. The
        // rule is the same in both cases and only the outcome differs: the
        // encoding comes from the authority the runtime holds, never from the
        // Rust signature that happens to be there.
        @sel(canBecomeMainWindow)
        fn can_become_main_window(&self) -> Bool {
            trace_scope!("canBecomeMainWindow");
            Bool::YES
        }

        @sel(canBecomeKeyWindow)
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
    ///
    /// LOCAL PATCH (aterm): the same formula as [`Self::id`] — a `WindowId` IS
    /// the instance address — reached from a raw pointer, for the two callers
    /// in `view.rs` that hold the window through a weak reference rather than
    /// as a `&WinitWindow`.
    ///
    /// The parameter was `&NSWindow` under W8, because an objc2 `WeakId` could
    /// only carry a binding type. That made this function a CROSS-FILE
    /// CONSUMPTION of `objc2` by a file that never named it: `view.rs` had no
    /// `objc2` token on either of its two call lines, and porting `view.rs`
    /// nevertheless forced this signature to change. The endgame metric counts
    /// names per file and could not have seen it.
    pub(super) fn id_of(window: Id) -> WindowId {
        WindowId(window.addr())
    }

    // ---------------------------------------------------------------------
    // LOCAL PATCH (aterm): the crossings. objc2's `#[inherits(NSResponder,
    // NSObject)]` made `&WinitWindow` an `&NSWindow`, an `&NSResponder` and an
    // `&AnyObject` by `Deref`; this class's Rust type is `aterm_objc`'s
    // zero-sized marker, so each becomes a named function performing the SAME
    // reinterpretation objc2 performed silently.
    //
    // THERE ARE TWO, NOT THREE, and the count is measured by the compiler
    // rather than projected: `as_responder` and `as_any` were written for the
    // symmetry with `view.rs` — which needs both, for `-firstResponder`
    // identity and for `observer:`/`object:` — and BOTH WERE DEAD here. Not one
    // of the backend's 97 sends to a `WinitWindow` wants an `NSResponder` or a
    // bare `AnyObject`; the window is the RECEIVER everywhere and the view is
    // what gets passed as an argument. Deleted, the way five of the seam's six
    // projected geometry conversions were.
    // ---------------------------------------------------------------------

    /// This window through objc2's `NSWindow` binding — the receiver for every
    /// AppKit method the backend sends it.
    pub(super) fn ns_window(&self) -> &NSWindow {
        // SAFETY: `self` borrows a live instance of `WinitWindow`, which is
        // registered with `NSWindow` as its superclass (the live audit reads
        // the chain), so it IS an `NSWindow`; `NSWindow` is a zero-sized
        // binding marker and borrows none of the instance's bytes.
        unsafe { aterm_objc_seam::objc2_ref(self.as_id()) }
    }

    /// A +1 handle to this window — objc2's `NSObjectProtocol::retain`, which
    /// this class no longer inherits.
    pub(super) fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is
        // a live non-null receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live WinitWindow")
    }
}
