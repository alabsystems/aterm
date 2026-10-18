// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/window_delegate.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
// (Marked hunks below: the WindowDelegate class is DECLARED with
// `aterm_objc::declare_class!` instead of objc2's, and its Drop moved to the
// ivars; `draggingEntered:` returns NSDragOperation, executing the DECISION
// recorded here last wave; two hunks emit a settled-size Resized after each
// fullscreen transition; and EVERY AppKit BINDING CALL in the file is now a
// typed `aterm_objc::send::*` prototype rather than an `objc2-app-kit` method.
// Search for the aterm local-patch marker.)
#![allow(clippy::unnecessary_cast)]
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::{Arc, Mutex};

// LOCAL PATCH (aterm): W8 ported this file's BINDINGS. What went is every
// `objc2-app-kit` / `objc2-foundation` method and every `NS*::CONSTANT` path;
// what arrived is `aterm_objc::send::*` (one typed prototype per C signature)
// plus this backend's own `aterm_objc_seam::consts`. `objc2` survives in
// exactly ONE type position, named on its `use` below.
use aterm_objc::send::*;
use aterm_objc::{
    class, sel, Bool, CGPoint as Point, CGRect as Rect, CGSize as Size2D, Id, MainThread,
};
// LOCAL PATCH (aterm), W9: `MainThreadMarker` WAS the last `objc2` name in this
// file, and the condition written here for its departure — that `observer.rs`
// (`RunLoop::main`), `monitor.rs` (`ns_screen`) and `app_state.rs`
// (`ApplicationDelegate::get`) stop taking one — is now met. All three take
// `aterm_objc::MainThread`, so this file's currency is the witness throughout
// and the import is GONE.
//
// The lever was not the send count. W8 ported all 177 of this file's binding
// sites and the file did not leave the list, because one zero-sized marker in a
// type position kept it there. What moved it was `aterm_objc_seam::marker` —
// the INVERSE re-badge — which let three signatures flip without their bodies
// being ported at all. See that function for why one-way conversion forced a
// root-first order and two-way allows a leaf-first one.
//
// AND THE FILE IS "OFF THE LIST" BY A METRIC THAT COUNTS NAMES, which is worth
// writing down where the claim is made. TWO LINES here still CONSUME an `objc2`
// TYPE — `monitor.ns_screen(mtm)` answers `Option<Retained<NSScreen>>`, and its
// `Retained<NSScreen>` reaches `seam::obj_of<T>` through a GENERIC parameter,
// so no `objc2` token appears in this file and no instrument scoped to names
// can see it. The lines are `:731` (`WindowDelegate::new`'s fullscreen screen)
// and `:1851` (`set_fullscreen`'s target screen); both are marked at the site
// with a `SITE n OF 2` comment, which is what a moving line number cannot be.
// They CHANGE AGAIN when `monitor.rs` ports, and a reader of this header who
// took "freed" to mean "finished with" would be surprised by that. It means
// exactly one thing: this file's own NAMES are ported.
use tracing::{trace, warn};

use super::app_state::ApplicationDelegate;
use super::aterm_objc_seam::{self as seam, consts};
// LOCAL PATCH (aterm), W9 phase 2: `same_cursor` joins `cursor_from_icon`.
// This import line is the cross-file consumption the file-count metric cannot
// see — `cursor_from_icon` answers a CONCRETE type, and when that type stopped
// being `Retained<NSCursor>` this file had to change without ever having named
// `objc2` itself. There were FOUR such edges into `cursor.rs` and `view.rs`
// from here, not the two the roadmap recorded; see `set_cursor` below.
use super::cursor::{cursor_from_icon, same_cursor};
use super::monitor::{self, flip_window_screen_coordinates, get_display_id};
use super::observer::RunLoop;
use super::view::WinitView;
use super::window::WinitWindow;
use super::{Fullscreen, MonitorHandle, OsError, WindowId};
use crate::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{ExternalError, NotSupportedError, OsError as RootOsError};
use crate::event::{InnerSizeWriter, WindowEvent};
use crate::keyboard::ModifiersState;
use crate::platform::macos::{OptionAsAlt, WindowExtMacOS};
use crate::window::{Cursor, ImePurpose, Theme, UserAttentionType, WindowAttributes};

// LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5):
// empty. Its twelve settings were reachable only through `WindowAttributesExtMacOS`, which
// nothing in aterm called, so every window was built at their defaults — and `new_window`
// below now builds exactly that: an opaque titlebar with its title and buttons shown, not
// movable by its background, a standard (not full-size) content view, a best-resolution
// surface, a shadow, first mouse accepted, no tabbing identifier, Option not rewritten as Alt,
// and not a "borderless game".
#[derive(Clone, Debug, Default)]
pub struct PlatformSpecificWindowAttributes;

#[derive(Debug)]
pub(crate) struct State {
    /// Strong reference to the global application state.
    app_delegate: aterm_objc::Retained<ApplicationDelegate>,

    window: aterm_objc::Retained<WinitWindow>,

    // LOCAL PATCH (aterm): the delegate instance these ivars belong to, as a
    // BORROWED `id` — never retained, or the delegate would keep itself alive
    // and `-dealloc` would never run.
    //
    // It exists because the KVO de-registration moved from
    // `Drop for WindowDelegate` to `Drop for State` (see the note there): the
    // observer AppKit holds is the delegate, and `&mut State` cannot name it.
    // NIL until `WindowDelegate::new` has actually registered the observer, so
    // the `Drop` below can tell "registered" from "died before registration"
    // rather than raising out of a `-dealloc`.
    this: Cell<Id>,

    // During `windowDidResize`, we use this to only send Moved if the position changed.
    //
    // This is expressed in desktop coordinates, and flipped to match Winit's coordinate system.
    // LOCAL PATCH (aterm): `NSPoint` -> `aterm_objc::CGPoint`, here and in the
    // three geometry ivars below. The pairs are `#[repr(C)]` over `f64` in the
    // same order, so this is a re-typing; what it BUYS is that the value handed
    // to a `send_*` prototype and the value stored here are one type, with no
    // conversion at the boundary to get wrong.
    previous_position: Cell<Point>,

    // Used to prevent redundant events.
    previous_scale_factor: Cell<f64>,

    /// The current resize increments for the window content.
    resize_increments: Cell<Size2D>,
    // LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): the `decorations` and `resizable`
    // ivars are gone with `set_decorations`/`set_resizable`, which no aterm crate calls; every
    // window is decorated and resizable (`is_decorated` answers `true`).
    maximized: Cell<bool>,

    // LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): the presentation
    // options upstream saved around exclusive fullscreen are gone with it.
    /// This field tracks the current fullscreen state of the window
    /// (as seen by `WindowDelegate`).
    fullscreen: RefCell<Option<Fullscreen>>,
    // If it is attempted to toggle fullscreen when in_fullscreen_transition is true,
    // Set target_fullscreen and do after fullscreen transition is end.
    target_fullscreen: RefCell<Option<Option<Fullscreen>>>,
    // This is true between windowWillEnterFullScreen and windowDidEnterFullScreen
    // or windowWillExitFullScreen and windowDidExitFullScreen.
    // We must not toggle fullscreen when this is true.
    in_fullscreen_transition: Cell<bool>,
    standard_frame: Cell<Option<Rect>>,
    // LOCAL PATCH (aterm): `NSWindowStyleMask` -> the `NSUInteger` it was
    // transparent over, so `|` and `&!` work on the word. (`is_simple_fullscreen`
    // and `is_borderless_game` stood beside it until winit-core step 5 cut the
    // simple-fullscreen and borderless-game controls; both were always false.)
    saved_style: Cell<Option<usize>>,
}

// LOCAL PATCH (aterm): the class pair, the ivars, the `dealloc` and all 21
// trampolines below are declared by `aterm_objc::declare_class!` rather than
// `objc2::declare_class!`. The METHOD BODIES still speak objc2 for their AppKit
// BINDINGS — `self.window()` is a `&WinitWindow` and everything it is sent is
// objc2's — exactly as `aterm-gui`'s four ported `toolbar.rs` classes do. What
// is first-party is the class creation, the ivar slot, the panic guards and
// every type encoding.
//
// The 21 rows are 17 `void` + 3 `BOOL` + 1 `NSUInteger`, READ OFF THE
// REGISTERED CLASS rather than counted from this source. (23 until winit-core
// step 5 cut `windowDidChangeScreen:` with simple fullscreen and
// `window:willUseFullScreenPresentationOptions:` with exclusive fullscreen.)
// This line said "4 BOOL + 1 NSUInteger" until 2026-09-01 — the pre-P3 shape, stale about the
// one thing the commit that wrote it changed. No struct return here. What is
// registered is now read back off the LIVE class by
// `crates/aterm-gui/examples/objc_live_class_audit.rs`, which the verify
// ladder runs — and which audits `view.rs`'s `WinitView` beside this class as
// of W3 phase 2.
//
// TWO THINGS CHANGED SHAPE, and neither is silent:
//
//  1. `draggingEntered:` now returns `usize`, not `bool` — the P3 decision this
//     file recorded in writing last wave, executed. See the note on the method.
//  2. `Drop for WindowDelegate` became `Drop for State`. objc2's `declare_class!`
//     ran the Rust destructor of the CLASS type from its generated `-dealloc`;
//     `aterm_objc`'s class type is a zero-sized marker with no destructor to
//     run, and the thing that owns Rust state is the ivars. Its `-dealloc`
//     drops the ivar slot (before `[super dealloc]`, so the instance is still
//     live and messageable while `State::drop` runs) and that is where the KVO
//     de-registration now lives — which is why `State` records `this`.
aterm_objc::declare_class! {
    /// The `NSWindowDelegate` + `NSDraggingDestination` this backend installs on
    /// every `WinitWindow`, and the owner of the per-window state.
    ///
    /// The protocol list is not decoration: objc2's `unsafe impl <Proto> for X {}`
    /// called `class_addProtocol` for each one, so dropping it would make
    /// `conformsToProtocol:` start answering NO to AppKit for a class whose whole
    /// job is to conform. `NSObject` is in the list for the same reason — it is
    /// what objc2's `unsafe impl NSObjectProtocol for WindowDelegate {}` added.
    pub(crate) struct WindowDelegate: NSObject {
        const NAME: &str = "WinitWindowDelegate";
        type Ivars = State;
        protocols: [NSObject, NSWindowDelegate, NSDraggingDestination];

        @sel(windowShouldClose:)
        fn window_should_close(&self, _sender: Id) -> Bool {
            trace_scope!("windowShouldClose:");
            self.queue_event(WindowEvent::CloseRequested);
            Bool::NO
        }

        @sel(windowWillClose:)
        fn window_will_close(&self, _sender: Id) {
            trace_scope!("windowWillClose:");
            // `setDelegate:` retains the previous value and then autoreleases it
            aterm_objc::autoreleasepool(|_| {
                // Since El Capitan, we need to be careful that delegate methods can't
                // be called after the window closes.
                // SAFETY: `-setDelegate:` is `v@:@` on `NSWindow`; nil clears it.
                unsafe { send_v_id(self.win(), sel!(setDelegate:), Id::NIL) };
            });
            self.queue_event(WindowEvent::Destroyed);
        }

        @sel(windowDidResize:)
        fn window_did_resize(&self, _sender: Id) {
            trace_scope!("windowDidResize:");
            // NOTE: WindowEvent::Resized is reported in frameDidChange.
            self.emit_move_event();
        }

        @sel(windowWillStartLiveResize:)
        fn window_will_start_live_resize(&self, _sender: Id) {
            trace_scope!("windowWillStartLiveResize:");

            let increments = self.ivars().resize_increments.get();
            self.set_resize_increments_inner(increments);
        }

        @sel(windowDidEndLiveResize:)
        fn window_did_end_live_resize(&self, _sender: Id) {
            trace_scope!("windowDidEndLiveResize:");
            self.set_resize_increments_inner(Size2D { width: 1., height: 1. });
        }

        /// This won't be triggered if the move was part of a resize.
        @sel(windowDidMove:)
        fn window_did_move(&self, _sender: Id) {
            trace_scope!("windowDidMove:");
            self.emit_move_event();
        }

        @sel(windowDidChangeBackingProperties:)
        fn window_did_change_backing_properties(&self, _sender: Id) {
            trace_scope!("windowDidChangeBackingProperties:");
            let scale_factor = self.scale_factor();
            if scale_factor == self.ivars().previous_scale_factor.get() {
                return;
            };
            self.ivars().previous_scale_factor.set(scale_factor);

            let mtm = self.mtm();
            let this = self.retained();
            RunLoop::main(mtm).queue_closure(move || {
                this.handle_scale_factor_changed(scale_factor);
            });
        }

        @sel(windowDidBecomeKey:)
        fn window_did_become_key(&self, _sender: Id) {
            trace_scope!("windowDidBecomeKey:");
            // TODO: center the cursor if the window had mouse grab when it
            // lost focus
            self.queue_event(WindowEvent::Focused(true));
        }

        @sel(windowDidResignKey:)
        fn window_did_resign_key(&self, _sender: Id) {
            trace_scope!("windowDidResignKey:");
            // It happens rather often, e.g. when the user is Cmd+Tabbing, that the
            // NSWindowDelegate will receive a didResignKey event despite no event
            // being received when the modifiers are released.  This is because
            // flagsChanged events are received by the NSView instead of the
            // NSWindowDelegate, and as a result a tracked modifiers state can quite
            // easily fall out of synchrony with reality.  This requires us to emit
            // a synthetic ModifiersChanged event when we lose focus.
            self.view().reset_modifiers();

            self.queue_event(WindowEvent::Focused(false));
        }

        /// Invoked when before enter fullscreen
        ///
        /// LOCAL PATCH (aterm): the state-machine flags are written BEFORE any
        /// send. A contained `NSException` in `-isZoomed` or in
        /// `current_monitor_inner`'s `-screen` used to leave
        /// `in_fullscreen_transition == false` and `fullscreen == None` while
        /// AppKit finished entering fullscreen, after which `set_fullscreen(None)`
        /// short-circuited on `None == None` and only the green button could
        /// leave. Now the transition flag is up first and the `None` arm
        /// records "borderless, current monitor" before it asks which monitor.
        @sel(windowWillEnterFullScreen:)
        fn window_will_enter_fullscreen(&self, _sender: Id) {
            trace_scope!("windowWillEnterFullScreen:");

            self.ivars().in_fullscreen_transition.set(true);
            // `None` here means the user clicked the green button (a
            // borderless request is `set_fullscreen`'s own doing and is
            // already recorded): record the fact BEFORE the monitor is
            // known, so a raise while asking leaves a state that
            // `set_fullscreen(None)` can still leave.
            let entered_by_user = {
                let mut fullscreen = self.ivars().fullscreen.borrow_mut();
                let by_user = fullscreen.is_none();
                if by_user {
                    *fullscreen = Some(Fullscreen::Borderless(None));
                }
                by_user
            };
            self.ivars().maximized.set(self.is_zoomed());
            if entered_by_user {
                let current_monitor = self.current_monitor_inner();
                *self.ivars().fullscreen.borrow_mut() =
                    Some(Fullscreen::Borderless(current_monitor));
            }
        }

        /// Invoked when before exit fullscreen
        @sel(windowWillExitFullScreen:)
        fn window_will_exit_fullscreen(&self, _sender: Id) {
            trace_scope!("windowWillExitFullScreen:");

            self.ivars().in_fullscreen_transition.set(true);
        }

        // LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): no
        // `window:willUseFullScreenPresentationOptions:` row. Upstream's
        // replaced AppKit's proposed options only in exclusive fullscreen,
        // which is gone on macOS; AppKit's default for a delegate that does not
        // implement it is the proposed options, which is what the row answered
        // for every other window.

        /// Invoked when entered fullscreen
        @sel(windowDidEnterFullScreen:)
        fn window_did_enter_fullscreen(&self, _sender: Id) {
            trace_scope!("windowDidEnterFullScreen:");
            self.ivars().in_fullscreen_transition.set(false);
            if let Some(target_fullscreen) = self.ivars().target_fullscreen.take() {
                self.set_fullscreen(target_fullscreen);
            }
            // LOCAL PATCH (aterm): AppKit finishes re-hosting the titlebar
            // chrome AFTER the last frameDidChange: of the transition, and
            // with fullSizeContentView that re-host changes no view frame —
            // so a consumer that derives chrome geometry (the titlebar band)
            // from a Resized event would keep a MID-TRANSITION sample
            // forever. Emit one Resized at the settled size so geometry
            // derived on resize is re-derived exactly once per completed
            // transition.
            self.queue_event(WindowEvent::Resized(self.inner_size()));
        }

        /// Invoked when exited fullscreen
        ///
        /// LOCAL PATCH (aterm): the transition flag is cleared and the parked
        /// request taken BEFORE `restore_state_from_fullscreen`'s sends
        /// (`-setStyleMask:`, `-isZoomed`, `-zoom:`/`-setFrame:`). A contained
        /// raise in those used to leave `in_fullscreen_transition == true` for
        /// the life of the window: every later `set_fullscreen` parked its
        /// request and nothing ever consumed it.
        @sel(windowDidExitFullScreen:)
        fn window_did_exit_fullscreen(&self, _sender: Id) {
            trace_scope!("windowDidExitFullScreen:");

            self.ivars().in_fullscreen_transition.set(false);
            let target_fullscreen = self.ivars().target_fullscreen.take();
            self.restore_state_from_fullscreen();
            if let Some(target_fullscreen) = target_fullscreen {
                self.set_fullscreen(target_fullscreen);
            }
            // LOCAL PATCH (aterm): see windowDidEnterFullScreen: — the same
            // post-transition re-derivation, for the restored chrome.
            self.queue_event(WindowEvent::Resized(self.inner_size()));
        }

        /// Invoked when fail to enter fullscreen
        ///
        /// From Apple doc:
        /// In some cases, the transition to enter full-screen mode can fail,
        /// due to being in the midst of handling some other animation or user gesture.
        /// This method indicates that there was an error, and you should clean up any
        /// work you may have done to prepare to enter full-screen mode.
        ///
        /// LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): upstream retried a failed
        /// entry with `performSelector:withObject:afterDelay:` when the window was BUILT
        /// fullscreen (`with_fullscreen`, which no aterm crate called and is gone). A failed
        /// entry is now always one a `set_fullscreen` asked for, and it is cleaned up the way
        /// upstream cleaned those up.
        @sel(windowDidFailToEnterFullScreen:)
        fn window_did_fail_to_enter_fullscreen(&self, _sender: Id) {
            trace_scope!("windowDidFailToEnterFullScreen:");
            self.ivars().in_fullscreen_transition.set(false);
            self.ivars().target_fullscreen.replace(None);
            self.restore_state_from_fullscreen();
        }

        /// Invoked when the occlusion state of the window changes
        @sel(windowDidChangeOcclusionState:)
        fn window_did_change_occlusion_state(&self, _sender: Id) {
            trace_scope!("windowDidChangeOcclusionState:");
            // SAFETY: `-occlusionState` is `Q@:` on `NSWindow`.
            let state = unsafe { send_usize(self.win(), sel!(occlusionState)) };
            let visible = state & consts::NS_WINDOW_OCCLUSION_STATE_VISIBLE != 0;
            self.queue_event(WindowEvent::Occluded(!visible));
        }

        /// Invoked when the dragged image enters destination bounds or frame
        ///
        // LOCAL PATCH (aterm) — THE P3 DECISION, EXECUTED. This row used to
        // return `bool` and register a ONE-byte `B@:@` where
        // `NSDraggingDestination` declares `-draggingEntered:` as
        // `Q24@0:8@16` — an EIGHT-byte `NSDragOperation`. It is now `usize`,
        // and the two neighbours below stay `Bool` because they really are
        // `BOOL` (`B24@0:8@16`), which is what made this a defect rather than
        // a pattern.
        //
        // NO BEHAVIOUR CHANGES ON arm64, and that is measured, not assumed:
        // `-> bool` compiles to `cset w0, ne`, so x0 was already exactly 0 or
        // 1 and AppKit read `NSDragOperationNone` / `NSDragOperationCopy`
        // (= 1) from it — the values written literally here. The x86_64
        // compat slice is where the old row lied: `setne %al` writes only AL
        // and leaves RAX's upper 56 bits holding the last `objc_msgSend`'s
        // pointer result, which AppKit would read as an enormous
        // NSDragOperation mask.
        @sel(draggingEntered:)
        fn dragging_entered(&self, sender: Id) -> usize {
            trace_scope!("draggingEntered:");

            let Some(paths) = (
                // SAFETY: AppKit passes the dragging-info object; see
                // `dragged_paths`.
                unsafe { dragged_paths(sender) }
            ) else {
                return consts::NS_DRAG_OPERATION_NONE;
            };
            for path in paths {
                self.queue_event(WindowEvent::HoveredFile(path));
            }

            consts::NS_DRAG_OPERATION_COPY
        }

        /// Invoked when the image is released
        @sel(prepareForDragOperation:)
        fn prepare_for_drag_operation(&self, _sender: Id) -> Bool {
            trace_scope!("prepareForDragOperation:");
            Bool::YES
        }

        /// Invoked after the released image has been removed from the screen
        @sel(performDragOperation:)
        fn perform_drag_operation(&self, sender: Id) -> Bool {
            trace_scope!("performDragOperation:");

            // SAFETY: as in `draggingEntered:` — AppKit's dragging-info object.
            let Some(paths) = (unsafe { dragged_paths(sender) }) else {
                return Bool::NO;
            };
            for path in paths {
                self.queue_event(WindowEvent::DroppedFile(path));
            }

            Bool::YES
        }

        /// Invoked when the dragging operation is complete
        @sel(concludeDragOperation:)
        fn conclude_drag_operation(&self, _sender: Id) {
            trace_scope!("concludeDragOperation:");
        }

        /// Invoked when the dragging operation is cancelled
        @sel(draggingExited:)
        fn dragging_exited(&self, _sender: Id) {
            trace_scope!("draggingExited:");
            self.queue_event(WindowEvent::HoveredFileCancelled);
        }

        /// Key-Value Observing. The only row here whose authority is a CLASS
        /// (`NSObject`) rather than a protocol, and the only one taking a bare
        /// pointer: `v@:@@@^v`. The `context:` argument is a `void *` and is
        /// encoded `"^v"` — NOT `"@"` — which is the one place in this backend
        /// where "a mutable raw pointer in a method position means `id`" is
        /// false.
        @sel(observeValueForKeyPath:ofObject:change:context:)
        fn observe_value(
            &self,
            key_path: Id,
            _object: Id,
            change: Id,
            _context: *mut c_void,
        ) {
            trace_scope!("observeValueForKeyPath:ofObject:change:context:");
            // NOTE: We don't _really_ need to check the key path, as there should only be one, but
            // in the future we might want to observe other key paths.
            //
            // LOCAL PATCH (aterm): objc2 compared two `&NSString`s with `==`,
            // which is `-isEqual:`. This asks `-isEqualToString:` on the same
            // two strings, which is what `NSString`'s `-isEqual:` dispatches to
            // once it has established that the argument is a string.
            //
            // SAFETY: KVO passes the key path it was registered with (an
            // `NSString`) and, because `addObserver:` below asks for New|Old,
            // the change dictionary. Both are nullable in principle, so both
            // are null-checked before any send — the null check objc2's
            // `Option<&T>` signature used to perform.
            let want = seam::nsstring("effectiveAppearance").expect("literal key path");
            let matches = !key_path.is_null()
                && unsafe { send_bool_id(key_path, sel!(isEqualToString:), want.id()) };
            if matches {
                assert!(!change.is_null(), "requested a change dictionary in `addObserver`, but none was provided");
                // SAFETY: `-objectForKey:` is `@@:@` on `NSDictionary`; both
                // keys were asked for by `addObserver:…options:` below, so both
                // are present, and each value is an `NSAppearance`.
                let (old, new) = unsafe {
                    (
                        send_id_id(change, sel!(objectForKey:), seam::NS_KEY_VALUE_CHANGE_OLD_KEY),
                        send_id_id(change, sel!(objectForKey:), seam::NS_KEY_VALUE_CHANGE_NEW_KEY),
                    )
                };
                assert!(!old.is_null(), "requested change dictionary did not contain `NSKeyValueChangeOldKey`");
                assert!(!new.is_null(), "requested change dictionary did not contain `NSKeyValueChangeNewKey`");

                // SAFETY: `-name` is `@@:` on `NSAppearance` and answers a
                // borrowed `NSString`.
                trace!(
                    old = %unsafe { seam::nsstring_to_rust(send_id(old, sel!(name))) },
                    new = %unsafe { seam::nsstring_to_rust(send_id(new, sel!(name))) },
                    "effectiveAppearance changed"
                );

                // Ignore the change if the window's theme is customized by the user (since in that
                // case the `effectiveAppearance` is only emitted upon said customization, and then
                // it's triggered directly by a user action, and we don't want to emit the event).
                //
                // SAFETY: `-appearance` is `@@:` on `NSWindow` and answers nil
                // when the window has not been given one.
                if !unsafe { send_id(self.win(), sel!(appearance)) }.is_null() {
                    return;
                }

                // SAFETY: both values are the `NSAppearance`s KVO reported.
                let old = unsafe { appearance_to_theme(old) };
                let new = unsafe { appearance_to_theme(new) };
                // Check that the theme changed in Winit's terms (the theme might have changed on
                // other parameters, such as level of contrast, but the event should not be emitted
                // in those cases).
                if old == new {
                    return;
                }

                self.queue_event(WindowEvent::ThemeChanged(new));
            } else {
                // SAFETY: nil answers `String::new()`; anything else is the
                // `NSString` key path KVO delivered.
                panic!("unknown observed keypath {:?}", unsafe { seam::nsstring_to_rust(key_path) });
            }
        }
    }
}

/// The file paths on a dragging-info object's pasteboard, or `None` when it
/// carries no filenames list.
///
/// LOCAL PATCH (aterm): `draggingEntered:` and `performDragOperation:` did this
/// same read through `objc2-app-kit`'s `NSPasteboard` binding, in two copies
/// that differed only in which event they queued. This is that read, once.
///
/// # Safety
///
/// `sender` must be a live object conforming to `NSDraggingInfo` — which is
/// what AppKit passes to every `NSDraggingDestination` row.
unsafe fn dragged_paths(sender: Id) -> Option<Vec<std::path::PathBuf>> {
    // SAFETY: `-draggingPasteboard` is `@@:` on `NSDraggingInfo` and answers a
    // borrowed `NSPasteboard`; `-propertyListForType:` is `@@:@` and answers
    // nil when the pasteboard holds nothing of that type — which is the `None`
    // arm both callers already had. The result for `NSFilenamesPboardType` is
    // documented to be an `NSArray` of `NSString`.
    unsafe {
        let pb = send_id(sender, sel!(draggingPasteboard));
        let list = send_id_id(pb, sel!(propertyListForType:), seam::NS_FILENAMES_PBOARD_TYPE);
        if list.is_null() {
            return None;
        }
        // LOCAL PATCH (aterm): the property list is DOCUMENTED to be an
        // `NSArray` of `NSString`, but the pasteboard is written by the
        // dragging source, not by AppKit — a source that puts any other
        // object under `NSFilenamesPboardType` would make `-count` /
        // `-objectAtIndex:` / `-UTF8String` an unrecognized-selector
        // NSException inside `draggingEntered:`, which aterm's trampoline
        // turns into a process abort. Check the classes first (`-isKindOfClass:`
        // is on NSObject, so it is safe on anything): a non-array root, or
        // ANY non-string entry, refuses the whole list — the `None` arm both
        // callers already have — so a malformed drop is never half-delivered
        // with the copy badge shown. MEASURED 2026-09-04 (macOS 26.6.2): every
        // public write route for such a list throws in the SOURCE process
        // inside CFPasteboardSetData, so this is defence against a private
        // pasteboard writer, not a reproduced input; an empty `@[]` still
        // reads as `Some(vec![])`, upstream winit's own behaviour.
        let is_a = |obj: Id, cls: &'static std::ffi::CStr| -> bool {
            send_bool_id(obj, sel!(isKindOfClass:), class(cls).as_id())
        };
        if !is_a(list, c"NSArray") {
            return None;
        }
        let count = send_usize(list, sel!(count));
        (0..count)
            .map(|i| {
                let file = send_id_usize(list, sel!(objectAtIndex:), i);
                (!file.is_null() && is_a(file, c"NSString"))
                    .then(|| std::path::PathBuf::from(seam::nsstring_to_rust(file)))
            })
            .collect::<Option<Vec<_>>>()
    }
}

// LOCAL PATCH (aterm): this WAS `impl Drop for WindowDelegate`. objc2's
// `declare_class!` ran the class type's Rust destructor from its generated
// `-dealloc`; `aterm_objc`'s class type is a zero-sized marker with no
// destructor, and the thing that owns Rust state — and that its `-dealloc`
// drops, while the instance is still live and messageable, before
// `[super dealloc]` — is the ivars. So the de-registration moved here, and
// `State::this` exists to carry the observer identity the old `&mut self`
// used to be.
impl Drop for State {
    fn drop(&mut self) {
        let this = self.this.get();
        if this.is_null() {
            // Never registered: `WindowDelegate::new` writes `this` only after
            // `addObserver:forKeyPath:options:context:` has returned, so a
            // delegate that dies before that point owes no removal — and
            // asking for one would RAISE ("not registered as an observer"),
            // across Rust frames, out of a `-dealloc`.
            return;
        }
        // SAFETY: `this` is the delegate instance whose ivars these are, still
        // live for the duration of `-dealloc` (the slot is disposed before
        // `[super dealloc]`), and it is the observer `new` registered under
        // this exact key path on this exact window.
        let key = seam::nsstring("effectiveAppearance").expect("literal key path");
        // SAFETY: `-removeObserver:forKeyPath:` is `v@:@@` on every `NSObject`.
        unsafe {
            send_v_id_id(self.window.as_id(), sel!(removeObserver:forKeyPath:), this, key.id());
        }
    }
}

fn new_window(
    app_delegate: &ApplicationDelegate,
    attrs: &WindowAttributes,
    mtm: MainThread,
) -> Option<aterm_objc::Retained<WinitWindow>> {
    aterm_objc::autoreleasepool(|_| {
        // LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md, winit-core step
        // 5): a window is built windowed (`with_fullscreen` is gone, so there is no screen to fill)
        // with the default style mask (`with_decorations`, `with_resizable` and
        // `with_enabled_buttons` are gone); `set_fullscreen` is how it leaves that state.
        //
        // SAFETY: `-backingScaleFactor` is `d@:` on `NSScreen`.
        let scale_factor = main_screen()
            .map(|screen| unsafe { send_f64(screen.id(), sel!(backingScaleFactor)) })
            .unwrap_or(1.0);
        let size = match attrs.inner_size {
            Some(size) => {
                let size = size.to_logical(scale_factor);
                Size2D { width: size.width, height: size.height }
            },
            None => Size2D { width: 800.0, height: 600.0 },
        };
        let position = match attrs.position {
            Some(position) => {
                let position = position.to_logical(scale_factor);
                flip_window_screen_coordinates(Rect {
                    origin: Point { x: position.x, y: position.y },
                    size,
                })
            },
            // This value is ignored by calling win.center() below
            None => Point { x: 0.0, y: 0.0 },
        };
        let frame = Rect { origin: position, size };

        // Resizable window with titlebar and titlebar buttons.
        let masks = consts::NS_WINDOW_STYLE_MASK_CLOSABLE
            | consts::NS_WINDOW_STYLE_MASK_MINIATURIZABLE
            | consts::NS_WINDOW_STYLE_MASK_RESIZABLE
            | consts::NS_WINDOW_STYLE_MASK_TITLED;

        // LOCAL PATCH (aterm): `+alloc`, store the (empty) ivars, then send the
        // DESIGNATED INITIALIZER — the same three steps and the same order as
        // the objc2 pair this replaces (`mtm.alloc().set_ivars(())` then
        // `msg_send_id![super(this), initWithContentRect: …]`).
        //
        // `alloc_ivars`, NOT `alloc_init`: `-[NSWindow init]` is not this
        // class's designated initializer and sending it would be a behaviour
        // change. See the note on the `declare_class!` site in `window.rs` for
        // what an initializer that substitutes an instance would cost here, and
        // for the 1,024-window measurement that says this one does not.
        //
        // The send goes to `self`, not to `super`, and reaches the same IMP:
        // `WinitWindow` declares no `-initWithContentRect:styleMask:backing:
        // defer:`, so the lookup walks straight into `NSWindow` — which the
        // audit's registered method table is what makes checkable.
        //
        // The argument types are the runtime's, read off the live method:
        // `@68@0:8{CGRect=…}16Q48Q56B64` — a `CGRect`, two `NSUInteger`s and a
        // `BOOL`. `NSWindowStyleMask` and `NSBackingStoreType` are both
        // `#[repr(transparent)]` over `NSUInteger` in objc2-app-kit, so `.0` is
        // the `usize` the runtime wants.
        let window: Option<aterm_objc::Retained<WinitWindow>> = unsafe {
            let raw = WinitWindow::alloc_ivars(mtm, ());
            let init: unsafe extern "C-unwind" fn(
                Id,
                aterm_objc::Sel,
                aterm_objc::CGRect,
                usize,
                usize,
                Bool,
            ) -> Id = aterm_objc::checked_msg();
            aterm_objc::Retained::from_owned(init(
                raw,
                aterm_objc::sel!(initWithContentRect:styleMask:backing:defer:),
                frame,
                masks,
                consts::NS_BACKING_STORE_BUFFERED,
                Bool::NO,
            ))
        };
        let window = window?;

        // LOCAL PATCH (aterm): ONE crossing for the whole configuration block
        // below, where objc2's `Deref` used to make every `window.setX(…)` an
        // `NSWindow` send implicitly. It is the raw receiver now, and every
        // line below is a typed prototype rather than a generated binding.
        //
        // SAFETY (for the whole block): `ns` is the `WinitWindow` just created,
        // a live `NSWindow` subclass. Every selector sent to it below is
        // declared on `NSWindow` with the C signature its helper names —
        // `-setReleasedWhenClosed:` and `-setAcceptsMouseMovedEvents:` are
        // `v@:B`, `-setTitle:` is `v@:@` and `-center` is `v@:`. The `NSString`
        // is +1 and lives to the end of its statement.
        let ns = window.as_id();

        unsafe {
            // It is very important for correct memory management that we
            // disable the extra release that would otherwise happen when
            // calling `close` on the window.
            send_v_bool(ns, sel!(setReleasedWhenClosed:), false);

            set_title(ns, &attrs.title);
            send_v_bool(ns, sel!(setAcceptsMouseMovedEvents:), true);

            if attrs.position.is_none() {
                send_v(ns, sel!(center));
            }
        }

        let view = WinitView::new(app_delegate, &window);

        // SAFETY (for the block below): `v` is the `WinitView` just created, a
        // live `NSView` subclass, and `ns` the window it is about to be
        // installed in. `-setWantsBestResolutionOpenGLSurface:` and
        // `-setWantsLayer:` are `v@:B` on `NSView`; `-setContentView:` and
        // `-setInitialFirstResponder:` are `v@:@` on `NSWindow`.
        let v = view.as_id();
        unsafe {
            // The default value of `setWantsBestResolutionOpenGLSurface:` was `false` until
            // macos 10.14 and `true` after 10.15, we should set it to `YES` or `NO` to avoid
            // always the default system value in favour of the user's code
            send_v_bool(v, sel!(setWantsBestResolutionOpenGLSurface:), true);

            // On Mojave, views automatically become layer-backed shortly after being added to
            // a window. Changing the layer-backedness of a view breaks the association between
            // the view and its associated OpenGL context. To work around this, on Mojave we
            // explicitly make the view layer-backed up front so that AppKit doesn't do it
            // itself and break the association with its context.
            if seam::NS_APPKIT_VERSION_NUMBER.floor() > consts::NS_APPKIT_VERSION_NUMBER_10_12 {
                send_v_bool(v, sel!(setWantsLayer:), true);
            }

            // Configure the new view as the "key view" for the window
            // LOCAL PATCH (aterm): `&view` was an `&NSView` by objc2 inheritance.
            send_v_id(ns, sel!(setContentView:), v);
            send_v_id(ns, sel!(setInitialFirstResponder:), v);

            // register for drag and drop operations.
            //
            // LOCAL PATCH (aterm): objc2 built the one-element array through
            // `NSArray::from_id_slice`, which COPIES the pasteboard-type string
            // first (`-copy`, +1). `+arrayWithObjects:count:` retains what it
            // is given, and the element here is an immutable framework global
            // that outlives the process, so the copy bought nothing and is
            // dropped. The array itself is +0 autoreleased, which is what
            // `-registerForDraggedTypes:` — which copies it — wants.
            let types = [seam::NS_FILENAMES_PBOARD_TYPE];
            let arr = send_id_idptr_usize(
                class(c"NSArray").as_id(),
                sel!(arrayWithObjects:count:),
                types.as_ptr(),
                types.len(),
            );
            send_v_id(ns, sel!(registerForDraggedTypes:), arr);
        }

        Some(window)
    })
}

impl WindowDelegate {
    pub(super) fn new(
        app_delegate: &ApplicationDelegate,
        attrs: WindowAttributes,
        mtm: MainThread,
    ) -> Result<aterm_objc::Retained<Self>, RootOsError> {
        let window = new_window(app_delegate, &attrs, mtm)
            .ok_or_else(|| os_error!(OsError::CreationError("couldn't create `NSWindow`")))?;

        // SAFETY: `-backingScaleFactor` is `d@:` on `NSWindow`.
        let scale_factor: f64 = unsafe { send_f64(window.as_id(), sel!(backingScaleFactor)) };

        // LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): `with_parent_window`,
        // `with_resize_increments` and `with_theme` are gone, so a window starts with no parent,
        // unit increments (`set_resize_increments` changes them) and the inherited appearance.
        let resize_increments = Size2D { width: 1., height: 1. };

        let delegate = WindowDelegate::alloc_init(mtm, State {
            app_delegate: app_delegate.retained(),
            window: window.retained(),
            this: Cell::new(Id::NIL),
            // SAFETY: `-frame` is `{CGRect}@:` on `NSWindow`.
            previous_position: Cell::new(flip_window_screen_coordinates(unsafe {
                send_rect(window.as_id(), sel!(frame))
            })),
            previous_scale_factor: Cell::new(scale_factor),
            resize_increments: Cell::new(resize_increments),
            maximized: Cell::new(false),
            fullscreen: RefCell::new(None),
            target_fullscreen: RefCell::new(None),
            in_fullscreen_transition: Cell::new(false),
            standard_frame: Cell::new(None),
            saved_style: Cell::new(None),
        })
        // LOCAL PATCH (aterm): `alloc_init` is `+alloc`, store the ivars, `-init`
        // — the same three steps and the same ORDER as the objc2 pair it
        // replaces (`mtm.alloc().set_ivars(..)` then
        // `msg_send_id![super(delegate), init]`), against a `MainThread`
        // witness. `WindowDelegate`'s superclass is `NSObject`, whose
        // designated initializer IS `-init`, so this is the right half of the
        // pair; the `alloc_ivars` half exists for the `NSView`/`NSWindow`
        // subclasses in this backend that will need `-initWithFrame:`. It
        // returns `None` rather than a nil-carrying handle, which is a failure
        // this call site had no way to observe before.
        .ok_or_else(|| {
            os_error!(OsError::CreationError("couldn't create `WinitWindowDelegate`"))
        })?;

        if scale_factor != 1.0 {
            let delegate = delegate.clone_retained();
            RunLoop::main(mtm).queue_closure(move || {
                delegate.handle_scale_factor_changed(scale_factor);
            });
        }
        // LOCAL PATCH (aterm): objc2's `-setDelegate:` took a
        // `&ProtocolObject<dyn NSWindowDelegate>`, whose constructor
        // `ProtocolObject::from_ref` is a COMPILE-TIME proof of conformance —
        // and this class is no longer an objc2 `ClassType`, so there was
        // nothing left to prove it from. The typed send takes the `id` the
        // runtime takes, and the conformance is established where it always
        // really was: at registration, by the `protocols:` list above
        // (`class_addProtocol(NSWindowDelegate)`), which the live-class audit
        // reads back off the registered class.
        //
        // SAFETY: `-setDelegate:` is `v@:@` on `NSWindow`. `delegate` is a live
        // instance of `WinitWindowDelegate` and the window does NOT retain it —
        // which is why `window.rs` keeps its own handle.
        unsafe { send_v_id(window.as_id(), sel!(setDelegate:), delegate.as_id()) };

        // Listen for theme change event.
        //
        // SAFETY: `-addObserver:forKeyPath:options:context:` is `v@:@@Q^v` on
        // every `NSObject` — the `context:` argument is `^v`, NOT `@`, which is
        // the one place in this backend where "a mutable raw pointer in a
        // method position means `id`" is false. The observer is un-registered
        // in the `Drop` of the delegate's ivars (see `impl Drop for State`),
        // and KVO does not retain it.
        unsafe {
            let key = seam::nsstring("effectiveAppearance").expect("literal key path");
            send_v_id_id_usize_ptr(
                window.as_id(),
                sel!(addObserver:forKeyPath:options:context:),
                delegate.as_id(),
                key.id(),
                consts::NS_KEY_VALUE_OBSERVING_OPTION_NEW
                    | consts::NS_KEY_VALUE_OBSERVING_OPTION_OLD,
                ptr::null_mut(),
            );
        };
        // LOCAL PATCH (aterm): ONLY NOW is the removal owed, so only now is the
        // observer identity recorded. See `State::this` and `impl Drop for State`.
        delegate.ivars().this.set(delegate.as_id());

        if let Some(dim) = attrs.min_inner_size {
            delegate.set_min_inner_size(Some(dim));
        }

        // LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): upstream applied blur, a maximum
        // size, the window level, the cursor, fullscreen and maximized state here, and focus
        // when shown. Their builders are gone, and each default was a no-op on a fresh window
        // (normal level, arrow cursor, windowed), so a window is built unblurred, unbounded,
        // windowed and active.

        // XXX Send `Focused(false)` right after creating the window delegate, so we won't
        // obscure the real focused events on the startup.
        delegate.queue_event(WindowEvent::Focused(false));

        if attrs.visible {
            // Tightly linked with `app_state::window_activation_hack`
            //
            // SAFETY: `-makeKeyAndOrderFront:` is `v@:@` on `NSWindow` and accepts a nil sender.
            unsafe { send_v_id(window.as_id(), sel!(makeKeyAndOrderFront:), Id::NIL) };
        }

        Ok(delegate)
    }

    // LOCAL PATCH (aterm): objc2's `mutability::MainThreadOnly` made
    // `MainThreadMarker::from(self)` a COMPILE-TIME derivation — the marker fell
    // out of the receiver's type. `aterm_objc` puts the witness where the
    // instance is BORN (`alloc_init` takes a `MainThread`) and not on the
    // receiver, so the eight sites that used to write
    // `MainThreadMarker::from(self)` ask here instead.
    //
    // It is a real question, not `new_unchecked`: a delegate method reached off
    // the main thread would be a bug in AppKit or in this backend, and the
    // panic names it at the frame that noticed rather than letting an AppKit
    // call go through on a thread that may not own the window server
    // connection. All eight callers are already on paths AppKit delivers to the
    // main thread (`RunLoop::main`, `maybe_wait_on_main`), so the check is
    // expected to be free of failures, not of cost — and its cost is one
    // `pthread_main_np()`-class query per SETTER call.
    #[track_caller]
    fn mtm(&self) -> MainThread {
        MainThread::new()
            .expect("a WindowDelegate method ran off the main thread; AppKit delivers on it")
    }

    /// A +1 handle to this delegate — objc2's `NSObjectProtocol::retain`, which
    /// this class no longer inherits.
    // LOCAL PATCH (aterm).
    fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is
        // a live non-null receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live WindowDelegate")
    }

    #[track_caller]
    pub(super) fn view(&self) -> aterm_objc::Retained<WinitView> {
        // LOCAL PATCH (aterm): `-contentView` is +0 autoreleased (objc2's
        // binding turned it into a +1 `Retained` by retaining it, which is what
        // `Retained::retain` does here). The `Retained::cast` this used to
        // perform was a reinterpretation between two of OBJC2's handle types
        // and `WinitView` is no longer one of them.
        // SAFETY: `-contentView` is `@@:` on `NSWindow` and answers the view
        // installed in `new_window`, +0 autoreleased.
        let content = unsafe { send_id(self.win(), sel!(contentView)) };
        assert!(!content.is_null(), "WinitWindow to have a content view");
        // SAFETY: the content view of a `WinitWindow` is always the `WinitView`
        // this backend installed in `new_window` above and never replaced, so
        // the object retained here is a live instance of that class.
        unsafe { aterm_objc::Retained::retain(content) }.expect("retaining a live WinitView")
    }

    #[track_caller]
    pub(super) fn window(&self) -> &WinitWindow {
        &self.ivars().window
    }

    /// This delegate's window as the RECEIVER for an AppKit send.
    ///
    /// LOCAL PATCH (aterm): this was `ns_window(&self) -> &NSWindow`, the one
    /// spelled crossing into objc2's binding, because objc2's
    /// `#[inherits(NSResponder, NSObject)] type Super = NSWindow` had made
    /// `&WinitWindow` an `&NSWindow` by `Deref` and every AppKit send in this
    /// file read `self.window().setTitle(…)`. W8 ported those sends, so what
    /// they need is the raw `id` the runtime takes and there is no binding type
    /// left in the middle. `window()` above still answers the fork's own type,
    /// for `id()` and for the raw-window-handle pointer.
    #[track_caller]
    pub(super) fn win(&self) -> Id {
        self.window().as_id()
    }

    #[track_caller]
    pub(crate) fn id(&self) -> WindowId {
        self.window().id()
    }

    /// `-[NSWindow styleMask]`, as the `NSUInteger` option set it is.
    fn style_mask(&self) -> usize {
        // SAFETY: `-styleMask` is `Q@:` on `NSWindow`.
        unsafe { send_usize(self.win(), sel!(styleMask)) }
    }

    /// `-[NSWindow contentRectForFrameRect:[self frame]]` — the content rect in
    /// screen coordinates, which five callers here want and none want apart.
    fn content_rect(&self) -> Rect {
        // SAFETY: `-frame` is `{CGRect}@:` and `-contentRectForFrameRect:` is
        // `{CGRect}@:{CGRect}` on `NSWindow`.
        unsafe {
            let frame = send_rect(self.win(), sel!(frame));
            send_rect_rect(self.win(), sel!(contentRectForFrameRect:), frame)
        }
    }

    pub(crate) fn queue_event(&self, event: WindowEvent) {
        self.ivars().app_delegate.maybe_queue_window_event(self.window().id(), event);
    }

    fn handle_scale_factor_changed(&self, scale_factor: f64) {
        let app_delegate = &self.ivars().app_delegate;
        let window = self.window();
        let ns = window.as_id();

        // SAFETY: `-frame` is `{CGRect}@:` and `-contentRectForFrameRect:` is
        // `{CGRect}@:{CGRect}` on `NSWindow`.
        let content_size = unsafe {
            send_rect_rect(ns, sel!(contentRectForFrameRect:), send_rect(ns, sel!(frame)))
        }
        .size;
        let content_size = LogicalSize::new(content_size.width, content_size.height);

        let suggested_size = content_size.to_physical(scale_factor);
        let new_inner_size = Arc::new(Mutex::new(suggested_size));
        app_delegate.handle_window_event(window.id(), WindowEvent::ScaleFactorChanged {
            scale_factor,
            inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&new_inner_size)),
        });
        let physical_size = *new_inner_size.lock().unwrap();
        drop(new_inner_size);

        if physical_size != suggested_size {
            let logical_size = physical_size.to_logical(scale_factor);
            let size = Size2D { width: logical_size.width, height: logical_size.height };
            // SAFETY: `-setContentSize:` is `v@:{CGSize}` on `NSWindow`.
            unsafe { send_v_size(ns, sel!(setContentSize:), size) };
        }
        app_delegate.handle_window_event(window.id(), WindowEvent::Resized(physical_size));
    }

    fn emit_move_event(&self) {
        // SAFETY: `-frame` is `{CGRect}@:` on `NSWindow`.
        let position =
            flip_window_screen_coordinates(unsafe { send_rect(self.win(), sel!(frame)) });
        if self.ivars().previous_position.get() == position {
            return;
        }
        self.ivars().previous_position.set(position);

        let position =
            LogicalPosition::new(position.x, position.y).to_physical(self.scale_factor());
        self.queue_event(WindowEvent::Moved(position));
    }

    fn set_style_mask(&self, mask: usize) {
        // SAFETY: `-setStyleMask:` is `v@:Q` and `-makeFirstResponder:` is
        // `B@:@` on `NSWindow`; the view is this window's live content view.
        unsafe {
            send_v_usize(self.win(), sel!(setStyleMask:), mask);
            // If we don't do this, key handling will break
            // (at least until the window is clicked again/etc.)
            let _ = send_bool_id(self.win(), sel!(makeFirstResponder:), self.view().as_id());
        }
    }

    pub fn set_title(&self, title: &str) {
        // SAFETY: `self.win()` is a live `NSWindow`; see `set_title` below.
        unsafe { set_title(self.win(), title) }
    }

    pub fn set_visible(&self, visible: bool) {
        // SAFETY: `-makeKeyAndOrderFront:` and `-orderOut:` are both `v@:@`
        // on `NSWindow` and both accept a nil sender.
        unsafe {
            let sel = if visible { sel!(makeKeyAndOrderFront:) } else { sel!(orderOut:) };
            send_v_id(self.win(), sel, Id::NIL);
        }
    }

    #[inline]
    pub fn is_visible(&self) -> Option<bool> {
        // SAFETY: `-isVisible` is `B@:` on `NSWindow`.
        Some(unsafe { send_bool(self.win(), sel!(isVisible)) })
    }

    pub fn request_redraw(&self) {
        self.ivars().app_delegate.queue_redraw(self.window().id());
    }

    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        // SAFETY: `-frame` is `{CGRect}@:` on `NSWindow`.
        let position =
            flip_window_screen_coordinates(unsafe { send_rect(self.win(), sel!(frame)) });
        Ok(LogicalPosition::new(position.x, position.y).to_physical(self.scale_factor()))
    }

    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
        let position = flip_window_screen_coordinates(self.content_rect());
        Ok(LogicalPosition::new(position.x, position.y).to_physical(self.scale_factor()))
    }

    pub fn set_outer_position(&self, position: Position) {
        let position = position.to_logical(self.scale_factor());
        // SAFETY: `-frame` is `{CGRect}@:` and `-setFrameOrigin:` is
        // `v@:{CGPoint}` on `NSWindow`.
        unsafe {
            let point = flip_window_screen_coordinates(Rect {
                origin: Point { x: position.x, y: position.y },
                size: send_rect(self.win(), sel!(frame)).size,
            });
            send_v_point(self.win(), sel!(setFrameOrigin:), point);
        }
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let content_rect = self.content_rect();
        let logical = LogicalSize::new(content_rect.size.width, content_rect.size.height);
        logical.to_physical(self.scale_factor())
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        // SAFETY: `-frame` is `{CGRect}@:` on `NSWindow`.
        let frame = unsafe { send_rect(self.win(), sel!(frame)) };
        let logical = LogicalSize::new(frame.size.width, frame.size.height);
        logical.to_physical(self.scale_factor())
    }

    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let scale_factor = self.scale_factor();
        let size = size.to_logical(scale_factor);
        // SAFETY: `-setContentSize:` is `v@:{CGSize}` on `NSWindow`.
        unsafe {
            send_v_size(self.win(), sel!(setContentSize:), Size2D {
                width: size.width,
                height: size.height,
            });
        }
        None
    }

    pub fn set_min_inner_size(&self, dimensions: Option<Size>) {
        let dimensions =
            dimensions.unwrap_or(Size::Logical(LogicalSize { width: 0.0, height: 0.0 }));
        let min_size = dimensions.to_logical::<f64>(self.scale_factor());

        let min_size = Size2D { width: min_size.width, height: min_size.height };
        // SAFETY: `-setContentMinSize:` and `-setContentSize:` are both
        // `v@:{CGSize}` on `NSWindow`.
        unsafe { send_v_size(self.win(), sel!(setContentMinSize:), min_size) };

        // If necessary, resize the window to match constraint
        let mut current_size = self.content_rect().size;
        if current_size.width < min_size.width {
            current_size.width = min_size.width;
        }
        if current_size.height < min_size.height {
            current_size.height = min_size.height;
        }
        unsafe { send_v_size(self.win(), sel!(setContentSize:), current_size) };
    }

    pub fn set_resize_increments(&self, increments: Option<Size>) {
        // XXX the resize increments are only used during live resizes.
        self.ivars().resize_increments.set(
            increments
                .map(|increments| {
                    let logical = increments.to_logical::<f64>(self.scale_factor());
                    Size2D { width: logical.width.max(1.0), height: logical.height.max(1.0) }
                })
                .unwrap_or(Size2D { width: 1.0, height: 1.0 }),
        );
    }

    pub(crate) fn set_resize_increments_inner(&self, size: Size2D) {
        // It was concluded (#2411) that there is never a use-case for
        // "outer" resize increments, hence we set "inner" ones here.
        // ("outer" in macOS being just resizeIncrements, and "inner" - contentResizeIncrements)
        // This is consistent with X11 size hints behavior
        // SAFETY: `-setContentResizeIncrements:` is `v@:{CGSize}` on `NSWindow`.
        unsafe { send_v_size(self.win(), sel!(setContentResizeIncrements:), size) };
    }

    pub fn set_cursor(&self, cursor: Cursor) {
        let view = self.view();

        // LOCAL PATCH (aterm), W9 phase 2: three consumptions of two ported
        // neighbours live in these lines, and NOT ONE of them mentions
        // `objc2`:
        //
        //   1. `cursor_from_icon(icon)` — `cursor.rs`, concrete return type.
        //   2. `view.cursor_icon()` — `view.rs`, concrete return type.
        //   3. `view.set_cursor_icon(..)` — `view.rs`, concrete argument type.
        //
        // All three were `Retained<NSCursor>` and are `aterm_objc::Obj` now.
        // (A fourth, `CustomCursor`'s field, left with custom cursors in
        // winit-core step 5.) The `==` is the thing that changed and the one
        // that could have gone wrong silently: it was `Retained`'s
        // `PartialEq`, which objc2 forwards to `-isEqual:`. `Obj` has no
        // `PartialEq`, so the obvious replacement is a pointer comparison —
        // same answer for `NSCursor` as it ships (measured; see `cursor.rs`),
        // different question. The send is named instead.
        let Cursor::Icon(icon) = cursor;
        let cursor = cursor_from_icon(icon);

        if same_cursor(&view.cursor_icon(), &cursor) {
            return;
        }

        view.set_cursor_icon(cursor);
        // SAFETY: `-invalidateCursorRectsForView:` is `v@:@` on `NSWindow`.
        unsafe { send_v_id(self.win(), sel!(invalidateCursorRectsForView:), view.as_id()) };
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        // SAFETY: `-backingScaleFactor` is `d@:` on `NSWindow`.
        unsafe { send_f64(self.win(), sel!(backingScaleFactor)) }
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), ExternalError> {
        let mtm = self.mtm();
        let _ = mtm;
        // SAFETY: `-currentEvent` is `@@:` on `NSApplication` and answers nil
        // outside event delivery; `-performWindowDragWithEvent:` is `v@:@` on
        // `NSWindow`.
        unsafe {
            let event = send_id(app(), sel!(currentEvent));
            if event.is_null() {
                return Err(ExternalError::Ignored);
            }
            send_v_id(self.win(), sel!(performWindowDragWithEvent:), event);
        }
        Ok(())
    }

    pub(crate) fn is_zoomed(&self) -> bool {
        // because `isZoomed` doesn't work if the window's borderless,
        // we make it resizable temporarily.
        let curr_mask = self.style_mask();

        let required = consts::NS_WINDOW_STYLE_MASK_TITLED | consts::NS_WINDOW_STYLE_MASK_RESIZABLE;
        let needs_temp_mask = curr_mask & required != required;
        if needs_temp_mask {
            self.set_style_mask(required);
        }

        // SAFETY: `-isZoomed` is `B@:` on `NSWindow`.
        let is_zoomed = unsafe { send_bool(self.win(), sel!(isZoomed)) };

        // Roll back temp styles
        if needs_temp_mask {
            self.set_style_mask(curr_mask);
        }

        is_zoomed
    }

    fn saved_style(&self) -> usize {
        let base_mask = self.ivars().saved_style.take().unwrap_or_else(|| self.style_mask());
        base_mask | consts::NS_WINDOW_STYLE_MASK_RESIZABLE
    }

    /// This is called when the window is exiting fullscreen, whether by the
    /// user clicking on the green fullscreen button or programmatically by
    /// `toggleFullScreen:`
    pub(crate) fn restore_state_from_fullscreen(&self) {
        self.ivars().fullscreen.replace(None);

        let maximized = self.ivars().maximized.get();
        let mask = self.saved_style();

        self.set_style_mask(mask);
        self.set_maximized(maximized);
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        // SAFETY: `-isMiniaturized` is `B@:`, and `-miniaturize:` and
        // `-deminiaturize:` are `v@:@`, on `NSWindow`. The sender is this
        // delegate, a live `NSObject`.
        unsafe {
            let is_minimized = send_bool(self.win(), sel!(isMiniaturized));
            if is_minimized == minimized {
                return;
            }

            let sel = if minimized { sel!(miniaturize:) } else { sel!(deminiaturize:) };
            send_v_id(self.win(), sel, self.as_id());
        }
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        // SAFETY: `-isMiniaturized` is `B@:` on `NSWindow`.
        Some(unsafe { send_bool(self.win(), sel!(isMiniaturized)) })
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        let mtm = self.mtm();
        let is_zoomed = self.is_zoomed();
        if is_zoomed == maximized {
            return;
        };

        // Save the standard frame sized if it is not zoomed
        if !is_zoomed {
            // SAFETY: `-frame` is `{CGRect}@:` on `NSWindow`.
            self.ivars().standard_frame.set(Some(unsafe { send_rect(self.win(), sel!(frame)) }));
        }

        self.ivars().maximized.set(maximized);

        if self.ivars().fullscreen.borrow().is_some() {
            // Handle it in window_did_exit_fullscreen
            return;
        }

        let _ = mtm;
        // SAFETY: `-zoom:` is `v@:@` and accepts a nil sender;
        // `-visibleFrame` is `{CGRect}@:` on `NSScreen`; `-setFrame:display:`
        // is `v@:{CGRect}B` on `NSWindow`.
        unsafe {
            if self.style_mask() & consts::NS_WINDOW_STYLE_MASK_RESIZABLE != 0 {
                // Just use the native zoom if resizable
                send_v_id(self.win(), sel!(zoom:), Id::NIL);
            } else {
                // if it's not resizable, we set the frame directly
                let new_rect = if maximized {
                    let screen = main_screen().expect("no screen found");
                    send_rect(screen.id(), sel!(visibleFrame))
                } else {
                    self.ivars().standard_frame.get().unwrap_or(DEFAULT_STANDARD_FRAME)
                };
                send_v_rect_bool(self.win(), sel!(setFrame:display:), new_rect, false);
            }
        }
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        self.ivars().fullscreen.borrow().clone()
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.is_zoomed()
    }

    #[inline]
    pub(crate) fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let mtm = self.mtm();

        if self.ivars().in_fullscreen_transition.get() {
            // We can't set fullscreen here.
            // Set fullscreen after transition.
            self.ivars().target_fullscreen.replace(Some(fullscreen));
            return;
        }
        let old_fullscreen = self.ivars().fullscreen.borrow().clone();
        if fullscreen == old_fullscreen {
            return;
        }

        // If the fullscreen is on a different monitor, we must move the window
        // to that monitor before we toggle fullscreen (as `toggleFullScreen`
        // does not take a screen parameter, but uses the current screen)
        if let Some(ref fullscreen) = fullscreen {
            let monitor = match fullscreen {
                Fullscreen::Borderless(Some(monitor)) => monitor.clone(),
                Fullscreen::Borderless(None) => {
                    if let Some(monitor) = self.current_monitor_inner() {
                        monitor
                    } else {
                        return;
                    }
                },
            };
            // LOCAL PATCH (aterm): upstream `unwrap`s the NSScreen here and
            // `assert!`s below that the window is on one. Between a display
            // leaving (unplug, sleep/wake) and AppKit's own reshuffle both are
            // false for an instant, and under aterm's trampoline policy either
            // panic is a process abort inside `windowDidEnterFullScreen:` /
            // `windowDidExitFullScreen:`. When the target screen or the
            // window's own screen is nil the fullscreen REQUEST IS DROPPED —
            // no state change, no `toggleFullScreen:` — exactly as the
            // `Borderless(None)` arm above already does when there is no
            // current monitor; the caller may ask again once a screen exists.
            let Some(new_screen) = monitor.ns_screen(mtm) else {
                return;
            };
            // `new_screen` was an objc2 `Retained<NSScreen>` here, re-badged
            // generically; since W12 `ns_screen` answers the `Obj` directly and
            // the re-badge is gone. (This was site 2 of 2; site 1, the screen a
            // window was BUILT fullscreen on, left with `with_fullscreen`.)

            // SAFETY: `-screen` is `@@:` on `NSWindow` and is non-nil for a
            // window that is on screen; `-frame` is `{CGRect}@:` on `NSScreen`;
            // `-setFrameOrigin:` is `v@:{CGPoint}` on `NSWindow`. The screens
            // are compared by IDENTITY, which is what objc2's `!=` on two
            // `Retained<NSScreen>` did — `NSScreen` does not override
            // `-isEqual:`.
            unsafe {
                let old_screen = send_id(self.win(), sel!(screen));
                if old_screen.is_null() {
                    return;
                }
                if old_screen != new_screen.id() {
                    let origin = send_rect(new_screen.id(), sel!(frame)).origin;
                    send_v_point(self.win(), sel!(setFrameOrigin:), origin);
                }
            }
        }

        self.ivars().fullscreen.replace(fullscreen.clone());

        // LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md,
        // winit-core step 5): `Fullscreen::Exclusive` is gone on macOS (no aterm
        // crate asked for a video mode), and with it the display capture, the
        // video-mode switch and its fades, the shielding-level and
        // presentation-option bookkeeping, and `restore_and_release_display`.
        // Every transition left is a borderless one, which `toggleFullScreen:`
        // makes; the window level it once had to reset first is always normal
        // now (`set_window_level` is gone too).
        let toggle_fullscreen = || {
            // SAFETY: `-toggleFullScreen:` is `v@:@` on `NSWindow` and accepts a
            // nil sender.
            unsafe { send_v_id(self.win(), sel!(toggleFullScreen:), Id::NIL) };
        };

        match (old_fullscreen, fullscreen) {
            (None, Some(_)) => {
                // `toggleFullScreen` doesn't work if the `StyleMask` is none, so we
                // set a normal style temporarily. The previous state will be
                // restored in `WindowDelegate::window_did_exit_fullscreen`.
                let curr_mask = self.style_mask();
                let required =
                    consts::NS_WINDOW_STYLE_MASK_TITLED | consts::NS_WINDOW_STYLE_MASK_RESIZABLE;
                if curr_mask & required != required {
                    self.set_style_mask(required);
                    self.ivars().saved_style.set(Some(curr_mask));
                }

                toggle_fullscreen();
            },
            (Some(_), None) => {
                // State is restored by `window_did_exit_fullscreen`
                toggle_fullscreen();
            },
            _ => {},
        };
    }

    #[inline]
    pub fn is_decorated(&self) -> bool {
        true
    }

    #[inline]
    pub fn set_ime_cursor_area(&self, spot: Position, size: Size) {
        let scale_factor = self.scale_factor();
        let logical_spot = spot.to_logical(scale_factor);
        let logical_spot = Point { x: logical_spot.x, y: logical_spot.y };

        let size = size.to_logical(scale_factor);
        let size = Size2D { width: size.width, height: size.height };

        self.view().set_ime_cursor_area(logical_spot, size);
    }

    #[inline]
    pub fn set_ime_allowed(&self, allowed: bool) {
        self.view().set_ime_allowed(allowed);
    }

    #[inline]
    pub fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    #[inline]
    pub fn focus_window(&self) {
        let _ = self.mtm();
        // SAFETY: `-isMiniaturized` and `-isVisible` are `B@:` on `NSWindow`;
        // `-activateIgnoringOtherApps:` is `v@:B` on `NSApplication`
        // (deprecated since 14.0, and the same one objc2 sent);
        // `-makeKeyAndOrderFront:` is `v@:@` and accepts a nil sender.
        unsafe {
            let is_minimized = send_bool(self.win(), sel!(isMiniaturized));
            let is_visible = send_bool(self.win(), sel!(isVisible));

            if !is_minimized && is_visible {
                send_v_bool(app(), sel!(activateIgnoringOtherApps:), true);
                send_v_id(self.win(), sel!(makeKeyAndOrderFront:), Id::NIL);
            }
        }
    }

    #[inline]
    pub fn request_user_attention(&self, request_type: Option<UserAttentionType>) {
        let _ = self.mtm();
        let ns_request_type = request_type.map(|ty| match ty {
            UserAttentionType::Critical => consts::NS_CRITICAL_REQUEST,
            UserAttentionType::Informational => consts::NS_INFORMATIONAL_REQUEST,
        });
        if let Some(ty) = ns_request_type {
            // SAFETY: `-requestUserAttention:` is `q24@0:8Q16` on
            // `NSApplication` — read off the live method, not inferred; the
            // request identifier it answers is discarded here exactly as
            // objc2's binding discarded it.
            unsafe { send_isize_usize(app(), sel!(requestUserAttention:), ty) };
        }
    }

    #[inline]
    // Allow directly accessing the current monitor internally without unwrapping.
    pub(crate) fn current_monitor_inner(&self) -> Option<MonitorHandle> {
        // SAFETY: `-screen` is `@@:` on `NSWindow` and answers nil for an
        // offscreen window. `get_display_id` took an objc2 `&NSScreen` until
        // W12 and this line crossed back for exactly that call — a PARAMETER
        // TYPE pinning a binding in a file that named none. It takes the raw
        // `id` now.
        let screen = unsafe { send_id(self.win(), sel!(screen)) };
        if screen.is_null() {
            return None;
        }
        let display_id = get_display_id(screen);
        if let Some(monitor) = MonitorHandle::new(display_id) {
            Some(monitor)
        } else {
            // NOTE: Display ID was just fetched from live NSScreen, but can still result in `None`
            // with certain Thunderbolt docked monitors.
            warn!(display_id, "got screen with invalid display ID");
            None
        }
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        self.current_monitor_inner()
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        monitor::available_monitors()
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        monitor::primary_monitor()
    }

    /// LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md,
    /// winit-core step 6): the content view as a borrowed `id`, what
    /// `aterm_objc::HasNsView` hands out (`platform/macos.rs`). The `rwh_06`
    /// handle below wraps the same pointer. Non-nil: `view()` asserts it.
    #[inline]
    pub(crate) fn ns_view(&self) -> Id {
        self.view().as_id()
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_window_handle_rwh_06(&self) -> rwh_06::RawWindowHandle {
        let window_handle = rwh_06::AppKitWindowHandle::new({
            let ptr: *mut ::core::ffi::c_void = self.ns_view().as_ptr();
            std::ptr::NonNull::new(ptr).expect("Retained<T> should never be null")
        });
        rwh_06::RawWindowHandle::AppKit(window_handle)
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        // SAFETY: `-isKeyWindow` is `B@:` on `NSWindow`.
        unsafe { send_bool(self.win(), sel!(isKeyWindow)) }
    }

    pub fn theme(&self) -> Option<Theme> {
        // SAFETY: `-appearance` is `@@:` on `NSWindow` and answers nil unless
        // the window was given one; `-respondsToSelector:` is `B@::` and
        // `-effectiveAppearance` is `@@:` on `NSApplication`.
        unsafe {
            let own = send_id(self.win(), sel!(appearance));
            if !own.is_null() {
                return Some(appearance_to_theme(own));
            }
            let app = app();
            if send_bool_sel(app, sel!(respondsToSelector:), sel!(effectiveAppearance)) {
                Some(appearance_to_theme(send_id(app, sel!(effectiveAppearance))))
            } else {
                Some(Theme::Light)
            }
        }
    }

    pub fn set_theme(&self, theme: Option<Theme>) {
        let appearance = theme_to_appearance(theme);
        // SAFETY: `-setAppearance:` is `v@:@` on `NSWindow`; nil restores the
        // inherited appearance, which is what `as_deref()` on a `None` did.
        unsafe {
            send_v_id(
                self.win(),
                sel!(setAppearance:),
                appearance.as_ref().map_or(Id::NIL, aterm_objc::Obj::id),
            );
        };
    }

    pub fn title(&self) -> String {
        // SAFETY: `-title` is `@@:` on `NSWindow` and answers a borrowed
        // `NSString` (never nil — an untitled window answers @"").
        unsafe { seam::nsstring_to_rust(send_id(self.win(), sel!(title))) }
    }
}

impl WindowExtMacOS for WindowDelegate {
    // LOCAL PATCH (aterm): see `WinitView::cached_modifiers`.
    fn cached_modifiers(&self) -> ModifiersState {
        self.view().cached_modifiers()
    }

    // LOCAL PATCH (aterm), 2026-10-16: upstream's option-as-alt, restored; the view keeps it.
    fn set_option_as_alt(&self, option_as_alt: OptionAsAlt) {
        self.view().set_option_as_alt(option_as_alt);
    }

    fn option_as_alt(&self) -> OptionAsAlt {
        self.view().option_as_alt()
    }
}

const DEFAULT_STANDARD_FRAME: Rect =
    Rect { origin: Point { x: 50.0, y: 50.0 }, size: Size2D { width: 800.0, height: 600.0 } };

/// `NSAppearanceNameDarkAqua`, built rather than linked.
///
/// Not the extern global: the symbol only exists from macOS 10.14, and binding
/// it would make this backend fail to LINK on anything older. This is the same
/// workaround the upstream file carries, spelled with a first-party string.
fn dark_appearance_name() -> aterm_objc::Obj {
    seam::nsstring("NSAppearanceNameDarkAqua").expect("literal appearance name")
}

/// The theme an `NSAppearance` best matches.
///
/// # Safety
/// `appearance` must be a live `NSAppearance`.
pub unsafe fn appearance_to_theme(appearance: Id) -> Theme {
    let dark = dark_appearance_name();
    // SAFETY: `+arrayWithObjects:count:` is `@@:r^@Q` on `NSArray` (the
    // objects pointer is `const id *`, hence the `r`; forge/objc-w7 ddef27fe7);
    // `-bestMatchFromAppearancesWithNames:` is `@@:@` on `NSAppearance` and
    // answers nil when none of the names matches; `-isEqualToString:` is `B@:@`
    // on `NSString`. Both names are live for the whole call.
    unsafe {
        let names = [seam::NS_APPEARANCE_NAME_AQUA, dark.id()];
        let arr = send_id_idptr_usize(
            class(c"NSArray").as_id(),
            sel!(arrayWithObjects:count:),
            names.as_ptr(),
            names.len(),
        );
        let best_match = send_id_id(appearance, sel!(bestMatchFromAppearancesWithNames:), arr);
        if best_match.is_null() {
            warn!("failed to determine the theme of the appearance");
            // Default to light in this case
            return Theme::Light;
        }
        if send_bool_id(best_match, sel!(isEqualToString:), dark.id()) {
            Theme::Dark
        } else {
            Theme::Light
        }
    }
}

fn theme_to_appearance(theme: Option<Theme>) -> Option<aterm_objc::Obj> {
    // THE OWNER IS BOUND, NOT BORROWED FROM A TEMPORARY, and this is a fix
    // rather than a style. The first spelling was
    //
    //     let name = match theme? {
    //         Theme::Light => seam::NS_APPEARANCE_NAME_AQUA,
    //         Theme::Dark => dark_appearance_name().id(),   // <-- temporary
    //     };
    //     send_id_id(…, sel!(appearanceNamed:), name)
    //
    // where `dark_appearance_name()` answers a +1 `Obj` whose ONLY owner was
    // that temporary. Rust drops it at the end of the `let name = …;`
    // statement — releasing the `NSString` — and the send below then handed
    // `+appearanceNamed:` a freed pointer. It compiled, it passed every test in
    // the tree, and `objc_window_drive`'s theme stage SEGFAULTED on it the
    // first time it ran.
    //
    // It is the same defect shape the tenth pass found in `app_introspect.rs`
    // and that `appkit::objc2_ref` was re-signatured to make impossible: an
    // object's lifetime carried through a raw pointer the compiler cannot see.
    // `Id` is `Copy` and knows nothing about ownership, so the only thing that
    // can keep an `Obj` alive across a send is a BINDING that outlives it.
    let dark;
    // SAFETY: `NS_APPEARANCE_NAME_AQUA` is an AppKit global that exists for the
    // life of the process; reading it is a load of an initialised `NSString *`.
    let name = match theme? {
        Theme::Light => unsafe { seam::NS_APPEARANCE_NAME_AQUA },
        Theme::Dark => {
            dark = dark_appearance_name();
            dark.id()
        },
    };
    // SAFETY: `+appearanceNamed:` is `@@:@` on `NSAppearance` and answers nil
    // for a name it does not know; the result is +0 autoreleased, so it is
    // retained into the `Obj` the caller keeps. `name` is live for this send —
    // either an AppKit global or `dark`, which outlives the statement.
    let appearance =
        unsafe { send_id_id(class(c"NSAppearance").as_id(), sel!(appearanceNamed:), name) };
    // SAFETY: non-nil here is a live +0 `NSAppearance`.
    if let Some(appearance) = unsafe { aterm_objc::Obj::retain(appearance) } {
        Some(appearance)
    } else {
        warn!(?theme, "could not find appearance for theme");
        // Assume system appearance in this case
        None
    }
}

// ---------------------------------------------------------------------------
// LOCAL PATCH (aterm): the small shared sends this file's 177 ported call sites
// factor out. Each of these had ONE spelling through objc2 — a binding method
// or a `Deref` — and would otherwise be repeated verbatim at every site.
// ---------------------------------------------------------------------------

/// `+[NSApplication sharedApplication]`, the shared instance.
///
/// The `MainThreadMarker` objc2's binding demanded for this is not AppKit's
/// requirement but objc2's: its `NSApplication` is `MainThreadOnly`, so EVERY
/// method on it asked for the marker. `mtm()` is still consulted at each of the
/// five call sites (it is what would catch a delegate method running off the
/// main thread), and its result is discarded here rather than threaded through
/// a send the runtime does not want.
fn app() -> Id {
    // SAFETY: `+sharedApplication` is `@@:` on `NSApplication` and creates the
    // instance on first call; the result is a process-lifetime singleton.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

/// `+[NSScreen mainScreen]`, retained, or `None` when there is no screen.
fn main_screen() -> Option<aterm_objc::Obj> {
    // SAFETY: `+mainScreen` is `@@:` on `NSScreen` and answers nil in a session
    // with no display; the result is +0 autoreleased, so it is retained.
    unsafe { aterm_objc::Obj::retain(send_id(class(c"NSScreen").as_id(), sel!(mainScreen))) }
}

/// `-[NSWindow setTitle:]`.
///
/// # Safety
/// `window` must be a live `NSWindow`.
unsafe fn set_title(window: Id, title: &str) {
    let s = seam::nsstring(title).expect("window title");
    // SAFETY: `-setTitle:` is `v@:@` on `NSWindow` and COPIES its argument, so
    // the +1 string may be released when this returns.
    unsafe { send_v_id(window, sel!(setTitle:), s.id()) };
}
