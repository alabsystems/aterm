// Modified by the aterm project in 2026; see the repository NOTICE.
// (The window delegate AND the window class are now declared with
//  `aterm_objc::declare_class!`, so the handles this file stores for them are
//  that crate's `Retained`, not objc2's — and since W12 the thread-affinity
//  container holding them is `aterm_objc::MainThreadBound` and the one AppKit
//  send left is a typed one.)
#![allow(clippy::unnecessary_cast)]

// LOCAL PATCH (aterm): objc2's `autoreleasepool`, its `NSWindow` binding,
// `MainThreadBound` and `MainThreadMarker` are all gone. `NSWindow` was only
// needed for the `ns_window()` crossing this file no longer has —
// `declare_class!` takes its superclass as a NAME, not as a Rust type.
use aterm_objc::send::send_v;
use aterm_objc::{Bool, Id, MainThread, MainThreadBound, autoreleasepool, sel};

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
    // `ClassType`.
    //
    // LOCAL PATCH (aterm), W12: and `MainThreadBound` is `aterm_objc`'s, which
    // is the LAST capability this backend was waiting on. The substitution is
    // faithful rather than approximate, and the point that matters is the
    // `Drop`: objc2's reschedules through `run_on_main` and so does this one,
    // so a `Window` moved to another thread and dropped there still releases
    // its two `Retained`s — and runs `WindowDelegate`'s Rust ivar destructor —
    // on the main thread. (Both implementations even need the same odd
    // rebinding inside the closure, `let this = self`, because edition-2021
    // captures DISJOINT FIELDS and `&mut ManuallyDrop<T>` is `Send` only if `T`
    // is. Two independent authors, one workaround.)
    delegate: MainThreadBound<aterm_objc::Retained<WindowDelegate>>,
}

impl Drop for Window {
    fn drop(&mut self) {
        // LOCAL PATCH (aterm), W12: `window.ns_window().close()` was this
        // file's only AppKit send and `ns_window()`'s only remaining caller —
        // so porting it deletes BOTH, and with them the last `seam::objc2_ref`
        // in `window.rs`.
        self.window.get_on_main(|window| {
            autoreleasepool(|_| {
                // SAFETY: `window` borrows a live `WinitWindow`, which is
                // registered with `NSWindow` as its superclass; `-close` is
                // `v16@0:8` on `NSWindow`. `get_on_main` has put this on the
                // main thread, which is where a window teardown — and the
                // `windowWillClose:` it dispatches — must run.
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
        // LOCAL PATCH (aterm), W12: `window_target.mtm` IS an
        // `aterm_objc::MainThread` now. That field was a CROSS-FILE
        // CONSUMPTION with no `objc2` token on the line that read it — this
        // file needed `objc2_foundation` because a struct field in
        // `event_loop.rs` had that type, not because of anything written here.
        let mtm = window_target.mtm;
        let delegate = autoreleasepool(|_| {
            // LOCAL PATCH (aterm), W9: `WindowDelegate::new` takes a witness now.
            WindowDelegate::new(window_target.app_delegate(), attributes, mtm)
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
        //
        // NOT CONTAINED, both rows: the real answer is YES and the contained
        // answer would be NO, which makes the window unfocusable — a real
        // answer, not an inert one. Neither body makes a send, so no raise can
        // reach them today; the marker keeps the policy visible at the
        // declaration rather than implied by the body being send-free.
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

    // ---------------------------------------------------------------------
    // `ns_window()` IS GONE, and it went the way `seam::id_of` and five of the
    // seam's six geometry conversions went: not because it was over-projected,
    // but because THE OTHER SIDE STOPPED NEEDING IT. It answered an objc2
    // `&NSWindow` and had three callers under W8 — two in `view.rs`, which W9
    // phase 2 ported, and one in this file's `Drop`, which W12 ports. A
    // crossing dies when either side stops needing it, and a count of call
    // sites says nothing about which.
    // ---------------------------------------------------------------------

    /// A +1 handle to this window — objc2's `NSObjectProtocol::retain`, which
    /// this class no longer inherits.
    pub(super) fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is
        // a live non-null receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live WinitWindow")
    }
}
