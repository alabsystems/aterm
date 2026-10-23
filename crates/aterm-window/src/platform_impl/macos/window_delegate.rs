// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/window_delegate.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#![allow(clippy::unnecessary_cast)]
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::{Arc, Mutex};

use aterm_objc::send::*;
use aterm_objc::{
    class, sel, Bool, CGPoint as Point, CGRect as Rect, CGSize as Size2D, Id, MainThread,
};
use tracing::{trace, warn};

use super::app_state::ApplicationDelegate;
use super::aterm_objc_seam::{self as seam, consts};
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

#[derive(Clone, Debug, Default)]
pub struct PlatformSpecificWindowAttributes;

#[derive(Debug)]
pub(crate) struct State {
    /// Strong reference to the global application state.
    app_delegate: aterm_objc::Retained<ApplicationDelegate>,

    window: aterm_objc::Retained<WinitWindow>,

    this: Cell<Id>,

    previous_position: Cell<Point>,

    previous_scale_factor: Cell<f64>,

    /// The current resize increments for the window content.
    resize_increments: Cell<Size2D>,
    maximized: Cell<bool>,

    /// This field tracks the current fullscreen state of the window
    fullscreen: RefCell<Option<Fullscreen>>,
    target_fullscreen: RefCell<Option<Option<Fullscreen>>>,
    in_fullscreen_transition: Cell<bool>,
    standard_frame: Cell<Option<Rect>>,
    saved_style: Cell<Option<usize>>,
}

aterm_objc::declare_class! {
    /// The `NSWindowDelegate` + `NSDraggingDestination` this backend installs on every `WinitWindow`, and the owner of the per-window state.
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
            aterm_objc::autoreleasepool(|_| {
                // SAFETY: `-setDelegate:` is `v@:@` on `NSWindow`; nil clears it.
                unsafe { send_v_id(self.win(), sel!(setDelegate:), Id::NIL) };
            });
            self.queue_event(WindowEvent::Destroyed);
        }

        @sel(windowDidResize:)
        fn window_did_resize(&self, _sender: Id) {
            trace_scope!("windowDidResize:");
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
            self.queue_event(WindowEvent::Focused(true));
        }

        @sel(windowDidResignKey:)
        fn window_did_resign_key(&self, _sender: Id) {
            trace_scope!("windowDidResignKey:");
            self.view().reset_modifiers();

            self.queue_event(WindowEvent::Focused(false));
        }

        /// Invoked when before enter fullscreen
        @sel(windowWillEnterFullScreen:)
        fn window_will_enter_fullscreen(&self, _sender: Id) {
            trace_scope!("windowWillEnterFullScreen:");

            self.ivars().in_fullscreen_transition.set(true);
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

        /// Invoked when entered fullscreen
        @sel(windowDidEnterFullScreen:)
        fn window_did_enter_fullscreen(&self, _sender: Id) {
            trace_scope!("windowDidEnterFullScreen:");
            self.ivars().in_fullscreen_transition.set(false);
            if let Some(target_fullscreen) = self.ivars().target_fullscreen.take() {
                self.set_fullscreen(target_fullscreen);
            }
            self.queue_event(WindowEvent::Resized(self.inner_size()));
        }

        /// Invoked when exited fullscreen
        @sel(windowDidExitFullScreen:)
        fn window_did_exit_fullscreen(&self, _sender: Id) {
            trace_scope!("windowDidExitFullScreen:");

            self.ivars().in_fullscreen_transition.set(false);
            let target_fullscreen = self.ivars().target_fullscreen.take();
            self.restore_state_from_fullscreen();
            if let Some(target_fullscreen) = target_fullscreen {
                self.set_fullscreen(target_fullscreen);
            }
            self.queue_event(WindowEvent::Resized(self.inner_size()));
        }

        /// Invoked when fail to enter fullscreen
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
        @sel(draggingEntered:)
        fn dragging_entered(&self, sender: Id) -> usize {
            trace_scope!("draggingEntered:");

            let Some(paths) = (
                // SAFETY: AppKit passes the dragging-info object; see `dragged_paths`.
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
        @sel(observeValueForKeyPath:ofObject:change:context:)
        fn observe_value(
            &self,
            key_path: Id,
            _object: Id,
            change: Id,
            _context: *mut c_void,
        ) {
            trace_scope!("observeValueForKeyPath:ofObject:change:context:");
            // SAFETY: KVO passes the key path it was registered with (an `NSString`) and, because `addObserver:` below asks for New|Old, the change dictionary.
            let want = seam::nsstring("effectiveAppearance").expect("literal key path");
            let matches = !key_path.is_null()
                && unsafe { send_bool_id(key_path, sel!(isEqualToString:), want.id()) };
            if matches {
                assert!(!change.is_null(), "requested a change dictionary in `addObserver`, but none was provided");
                // SAFETY: `-objectForKey:` is `@@:@` on `NSDictionary`.
                let (old, new) = unsafe {
                    (
                        send_id_id(change, sel!(objectForKey:), seam::NS_KEY_VALUE_CHANGE_OLD_KEY),
                        send_id_id(change, sel!(objectForKey:), seam::NS_KEY_VALUE_CHANGE_NEW_KEY),
                    )
                };
                assert!(!old.is_null(), "requested change dictionary did not contain `NSKeyValueChangeOldKey`");
                assert!(!new.is_null(), "requested change dictionary did not contain `NSKeyValueChangeNewKey`");

                // SAFETY: `-name` is `@@:` on `NSAppearance` and answers a borrowed `NSString`.
                trace!(
                    old = %unsafe { seam::nsstring_to_rust(send_id(old, sel!(name))) },
                    new = %unsafe { seam::nsstring_to_rust(send_id(new, sel!(name))) },
                    "effectiveAppearance changed"
                );

                // SAFETY: `-appearance` is `@@:` on `NSWindow` and answers nil when the window has not been given one.
                if !unsafe { send_id(self.win(), sel!(appearance)) }.is_null() {
                    return;
                }

                // SAFETY: both values are the `NSAppearance`s KVO reported.
                let old = unsafe { appearance_to_theme(old) };
                let new = unsafe { appearance_to_theme(new) };
                if old == new {
                    return;
                }

                self.queue_event(WindowEvent::ThemeChanged(new));
            } else {
                // SAFETY: nil answers `String::new()`; anything else is the `NSString` key path KVO delivered.
                panic!("unknown observed keypath {:?}", unsafe { seam::nsstring_to_rust(key_path) });
            }
        }
    }
}

/// The file paths on a dragging-info object's pasteboard, or `None` when it
/// # Safety
///
/// `sender` must be a live object conforming to `NSDraggingInfo` — which is
/// what AppKit passes to every `NSDraggingDestination` row.
unsafe fn dragged_paths(sender: Id) -> Option<Vec<std::path::PathBuf>> {
    // SAFETY: `-draggingPasteboard` is `@@:` on `NSDraggingInfo` and answers a borrowed `NSPasteboard`.
    unsafe {
        let pb = send_id(sender, sel!(draggingPasteboard));
        let list = send_id_id(pb, sel!(propertyListForType:), seam::NS_FILENAMES_PBOARD_TYPE);
        if list.is_null() {
            return None;
        }
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

impl Drop for State {
    fn drop(&mut self) {
        let this = self.this.get();
        if this.is_null() {
            // Never registered: `WindowDelegate::new` writes `this` only after
            return;
        }
        // SAFETY: `this` is live during `-dealloc` before `[super dealloc]` and is the registered KVO observer for this window and key path.
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
            None => Point { x: 0.0, y: 0.0 },
        };
        let frame = Rect { origin: position, size };

        let masks = consts::NS_WINDOW_STYLE_MASK_CLOSABLE
            | consts::NS_WINDOW_STYLE_MASK_MINIATURIZABLE
            | consts::NS_WINDOW_STYLE_MASK_RESIZABLE
            | consts::NS_WINDOW_STYLE_MASK_TITLED;

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

        let ns = window.as_id();

        unsafe {
            send_v_bool(ns, sel!(setReleasedWhenClosed:), false);

            set_title(ns, &attrs.title);
            send_v_bool(ns, sel!(setAcceptsMouseMovedEvents:), true);

            if attrs.position.is_none() {
                send_v(ns, sel!(center));
            }
        }

        let view = WinitView::new(app_delegate, &window);

        let v = view.as_id();
        unsafe {
            send_v_bool(v, sel!(setWantsBestResolutionOpenGLSurface:), true);

            if seam::NS_APPKIT_VERSION_NUMBER.floor() > consts::NS_APPKIT_VERSION_NUMBER_10_12 {
                send_v_bool(v, sel!(setWantsLayer:), true);
            }

            send_v_id(ns, sel!(setContentView:), v);
            send_v_id(ns, sel!(setInitialFirstResponder:), v);

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
        .ok_or_else(|| {
            os_error!(OsError::CreationError("couldn't create `WinitWindowDelegate`"))
        })?;

        if scale_factor != 1.0 {
            let delegate = delegate.clone_retained();
            RunLoop::main(mtm).queue_closure(move || {
                delegate.handle_scale_factor_changed(scale_factor);
            });
        }
        // SAFETY: `-setDelegate:` is `v@:@` on `NSWindow`.
        unsafe { send_v_id(window.as_id(), sel!(setDelegate:), delegate.as_id()) };

        // SAFETY: `-addObserver:forKeyPath:options:context:` is `v@:@@Q^v` on every `NSObject`.
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
        delegate.ivars().this.set(delegate.as_id());

        if let Some(dim) = attrs.min_inner_size {
            delegate.set_min_inner_size(Some(dim));
        }

        delegate.queue_event(WindowEvent::Focused(false));

        if attrs.visible {
            // SAFETY: `-makeKeyAndOrderFront:` is `v@:@` on `NSWindow` and accepts a nil sender.
            unsafe { send_v_id(window.as_id(), sel!(makeKeyAndOrderFront:), Id::NIL) };
        }

        Ok(delegate)
    }

    #[track_caller]
    fn mtm(&self) -> MainThread {
        MainThread::new()
            .expect("a WindowDelegate method ran off the main thread; AppKit delivers on it")
    }

    /// A +1 handle to this delegate — objc2's `NSObjectProtocol::retain`, which
    fn retained(&self) -> aterm_objc::Retained<Self> {
        // SAFETY: `self` borrows a live instance of this class, so `as_id()` is a live non-null receiver for `objc_retain`.
        unsafe { aterm_objc::Retained::retain(self.as_id()) }
            .expect("retaining a live WindowDelegate")
    }

    #[track_caller]
    pub(super) fn view(&self) -> aterm_objc::Retained<WinitView> {
        // SAFETY: `-contentView` is `@@:` on `NSWindow` and answers the view installed in `new_window`, +0 autoreleased.
        let content = unsafe { send_id(self.win(), sel!(contentView)) };
        assert!(!content.is_null(), "WinitWindow to have a content view");
        // SAFETY: the content view of a `WinitWindow` is always the `WinitView` this backend installed in `new_window` above and never replaced, so the object retained here is a live instance of that class.
        unsafe { aterm_objc::Retained::retain(content) }.expect("retaining a live WinitView")
    }

    #[track_caller]
    pub(super) fn window(&self) -> &WinitWindow {
        &self.ivars().window
    }

    /// This delegate's window as the RECEIVER for an AppKit send.
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
    fn content_rect(&self) -> Rect {
        // SAFETY: `-frame` is `{CGRect}@:` and `-contentRectForFrameRect:` is `{CGRect}@:{CGRect}` on `NSWindow`.
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

        // SAFETY: `-frame` is `{CGRect}@:` and `-contentRectForFrameRect:` is `{CGRect}@:{CGRect}` on `NSWindow`.
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
        // SAFETY: `-setStyleMask:` is `v@:Q` and `-makeFirstResponder:` is `B@:@` on `NSWindow`.
        unsafe {
            send_v_usize(self.win(), sel!(setStyleMask:), mask);
            let _ = send_bool_id(self.win(), sel!(makeFirstResponder:), self.view().as_id());
        }
    }

    pub fn set_title(&self, title: &str) {
        // SAFETY: `self.win()` is a live `NSWindow`; see `set_title` below.
        unsafe { set_title(self.win(), title) }
    }

    pub fn set_visible(&self, visible: bool) {
        // SAFETY: `-makeKeyAndOrderFront:` and `-orderOut:` are both `v@:@` on `NSWindow` and both accept a nil sender.
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
        // SAFETY: `-frame` is `{CGRect}@:` and `-setFrameOrigin:` is `v@:{CGPoint}` on `NSWindow`.
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
        // SAFETY: `-setContentMinSize:` and `-setContentSize:` are both `v@:{CGSize}` on `NSWindow`.
        unsafe { send_v_size(self.win(), sel!(setContentMinSize:), min_size) };

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
        // SAFETY: `-setContentResizeIncrements:` is `v@:{CGSize}` on `NSWindow`.
        unsafe { send_v_size(self.win(), sel!(setContentResizeIncrements:), size) };
    }

    pub fn set_cursor(&self, cursor: Cursor) {
        let view = self.view();

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
        // SAFETY: `-currentEvent` is `@@:` on `NSApplication` and answers nil outside event delivery.
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
        let curr_mask = self.style_mask();

        let required = consts::NS_WINDOW_STYLE_MASK_TITLED | consts::NS_WINDOW_STYLE_MASK_RESIZABLE;
        let needs_temp_mask = curr_mask & required != required;
        if needs_temp_mask {
            self.set_style_mask(required);
        }

        // SAFETY: `-isZoomed` is `B@:` on `NSWindow`.
        let is_zoomed = unsafe { send_bool(self.win(), sel!(isZoomed)) };

        if needs_temp_mask {
            self.set_style_mask(curr_mask);
        }

        is_zoomed
    }

    fn saved_style(&self) -> usize {
        let base_mask = self.ivars().saved_style.take().unwrap_or_else(|| self.style_mask());
        base_mask | consts::NS_WINDOW_STYLE_MASK_RESIZABLE
    }

    /// This is called when the window is exiting fullscreen, whether by the user clicking on the green fullscreen button or programmatically by `toggleFullScreen:`
    pub(crate) fn restore_state_from_fullscreen(&self) {
        self.ivars().fullscreen.replace(None);

        let maximized = self.ivars().maximized.get();
        let mask = self.saved_style();

        self.set_style_mask(mask);
        self.set_maximized(maximized);
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        // SAFETY: `-isMiniaturized` is `B@:`, and `-miniaturize:` and `-deminiaturize:` are `v@:@`, on `NSWindow`.
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
        // SAFETY: `-zoom:` is `v@:@` and accepts a nil sender.
        unsafe {
            if self.style_mask() & consts::NS_WINDOW_STYLE_MASK_RESIZABLE != 0 {
                // Just use the native zoom if resizable
                send_v_id(self.win(), sel!(zoom:), Id::NIL);
            } else {
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
            self.ivars().target_fullscreen.replace(Some(fullscreen));
            return;
        }
        let old_fullscreen = self.ivars().fullscreen.borrow().clone();
        if fullscreen == old_fullscreen {
            return;
        }

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
            let Some(new_screen) = monitor.ns_screen(mtm) else {
                return;
            };

            // SAFETY: `-screen` is `@@:` on `NSWindow` and is non-nil for a window that is on screen.
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

        let toggle_fullscreen = || {
            // SAFETY: `-toggleFullScreen:` is `v@:@` on `NSWindow` and accepts a nil sender.
            unsafe { send_v_id(self.win(), sel!(toggleFullScreen:), Id::NIL) };
        };

        match (old_fullscreen, fullscreen) {
            (None, Some(_)) => {
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
        // SAFETY: `-isMiniaturized` and `-isVisible` are `B@:` on `NSWindow`.
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
            // SAFETY: `-requestUserAttention:` is `q24@0:8Q16` on `NSApplication`.
            unsafe { send_isize_usize(app(), sel!(requestUserAttention:), ty) };
        }
    }

    #[inline]
    pub(crate) fn current_monitor_inner(&self) -> Option<MonitorHandle> {
        // SAFETY: `-screen` is `@@:` on `NSWindow` and answers nil for an offscreen window.
        let screen = unsafe { send_id(self.win(), sel!(screen)) };
        if screen.is_null() {
            return None;
        }
        let display_id = get_display_id(screen);
        if let Some(monitor) = MonitorHandle::new(display_id) {
            Some(monitor)
        } else {
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

    /// LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 6).
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
        // SAFETY: `-appearance` is `@@:` on `NSWindow` and answers nil unless the window was given one.
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
        // SAFETY: `-setAppearance:` is `v@:@` on `NSWindow`.
        unsafe {
            send_v_id(
                self.win(),
                sel!(setAppearance:),
                appearance.as_ref().map_or(Id::NIL, aterm_objc::Obj::id),
            );
        };
    }

    pub fn title(&self) -> String {
        // SAFETY: `-title` is `@@:` on `NSWindow` and answers a borrowed `NSString` (never nil — an untitled window answers @"").
        unsafe { seam::nsstring_to_rust(send_id(self.win(), sel!(title))) }
    }
}

impl WindowExtMacOS for WindowDelegate {
    fn cached_modifiers(&self) -> ModifiersState {
        self.view().cached_modifiers()
    }

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
fn dark_appearance_name() -> aterm_objc::Obj {
    seam::nsstring("NSAppearanceNameDarkAqua").expect("literal appearance name")
}

/// The theme an `NSAppearance` best matches.
///
/// # Safety
/// `appearance` must be a live `NSAppearance`.
pub unsafe fn appearance_to_theme(appearance: Id) -> Theme {
    let dark = dark_appearance_name();
    // SAFETY: `+arrayWithObjects:count:` is `@@:r^@Q` on `NSArray` (the objects pointer is `const id *`, hence the `r`; forge/objc-w7 ddef27fe7).
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
    let dark;
    // SAFETY: `NS_APPEARANCE_NAME_AQUA` is an AppKit global that exists for the life of the process.
    let name = match theme? {
        Theme::Light => unsafe { seam::NS_APPEARANCE_NAME_AQUA },
        Theme::Dark => {
            dark = dark_appearance_name();
            dark.id()
        },
    };
    // SAFETY: `+appearanceNamed:` is `@@:@` on `NSAppearance` and answers nil for a name it does not know.
    let appearance =
        unsafe { send_id_id(class(c"NSAppearance").as_id(), sel!(appearanceNamed:), name) };
    // SAFETY: non-nil here is a live +0 `NSAppearance`.
    if let Some(appearance) = unsafe { aterm_objc::Obj::retain(appearance) } {
        Some(appearance)
    } else {
        warn!(?theme, "could not find appearance for theme");
        None
    }
}

/// `+[NSApplication sharedApplication]`, the shared instance.
fn app() -> Id {
    // SAFETY: `+sharedApplication` is `@@:` on `NSApplication` and creates the instance on first call.
    unsafe { send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) }
}

/// `+[NSScreen mainScreen]`, retained, or `None` when there is no screen.
fn main_screen() -> Option<aterm_objc::Obj> {
    // SAFETY: `+mainScreen` is `@@:` on `NSScreen` and answers nil in a session with no display.
    unsafe { aterm_objc::Obj::retain(send_id(class(c"NSScreen").as_id(), sel!(mainScreen))) }
}

/// `-[NSWindow setTitle:]`.
/// # Safety
/// `window` must be a live `NSWindow`.
unsafe fn set_title(window: Id, title: &str) {
    let s = seam::nsstring(title).expect("window title");
    // SAFETY: `-setTitle:` is `v@:@` on `NSWindow` and COPIES its argument, so the +1 string may be released when this returns.
    unsafe { send_v_id(window, sel!(setTitle:), s.id()) };
}
