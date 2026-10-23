// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/view.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#![allow(clippy::unnecessary_cast)]
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ptr;

use aterm_objc::send::{
    alloc, send_bool, send_bool_cls, send_bool_id, send_f64, send_id, send_id_id,
    send_id_idptr_usize, send_id_usize, send_id_usize_point_usize_f64_isize_id_id_id_bool_u16,
    send_isize, send_isize_rect_id_ptr_bool, send_point, send_point_point_id, send_rect,
    send_rect_rect, send_rect_rect_id, send_u16, send_usize, send_usize_usize, send_v, send_v_bool,
    send_v_id, send_v_id_sel_id_id, send_v_isize, send_v_rect_id,
};
use aterm_objc::{
    autoreleasepool, class, sel, Bool, CGPoint, CGRect, CGSize, Id, NSRange, Obj, Sel, WeakObj,
};

use super::app_state::ApplicationDelegate;
use super::aterm_objc_seam::{self as seam, consts};
use super::cursor::default_cursor;
use super::event::{
    code_to_key, code_to_location, create_key_event, event_mods, option_is_alt,
    scancode_to_physicalkey, KeyEventExtra,
};
use super::window::WinitWindow;
use super::DEVICE_ID;
use crate::dpi::{LogicalPosition, LogicalSize};
use crate::event::{
    ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use crate::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey};
use crate::platform::macos::OptionAsAlt;

#[derive(Debug)]
struct CursorState {
    cursor: Obj,
}

impl Default for CursorState {
    fn default() -> Self {
        Self { cursor: default_cursor() }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Default)]
enum ImeState {
    #[default]
    /// The IME events are disabled, so only `ReceivedCharacter` is being sent to the user.
    Disabled,

    /// The ground state of enabled IME input.
    Ground,

    /// The IME is in preedit.
    Preedit,

    /// The text was just committed, so the next input from the keyboard must be ignored.
    Committed,
}

aterm_types::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct ModLocationMask: u8 {
        const LEFT     = 0b0001;
        const RIGHT    = 0b0010;
    }
}
impl ModLocationMask {
    fn from_location(loc: KeyLocation) -> ModLocationMask {
        match loc {
            KeyLocation::Left => ModLocationMask::LEFT,
            KeyLocation::Right => ModLocationMask::RIGHT,
            _ => unreachable!(),
        }
    }
}

const fn key_to_modifier(key: &Key) -> Option<ModifiersState> {
    match key {
        Key::Named(NamedKey::Alt) => Some(ModifiersState::ALT),
        Key::Named(NamedKey::Control) => Some(ModifiersState::CONTROL),
        Key::Named(NamedKey::Super) => Some(ModifiersState::SUPER),
        Key::Named(NamedKey::Shift) => Some(ModifiersState::SHIFT),
        _ => None,
    }
}

fn get_right_modifier_code(key: &Key) -> KeyCode {
    match key {
        Key::Named(NamedKey::Alt) => KeyCode::AltRight,
        Key::Named(NamedKey::Control) => KeyCode::ControlRight,
        Key::Named(NamedKey::Shift) => KeyCode::ShiftRight,
        Key::Named(NamedKey::Super) => KeyCode::SuperRight,
        _ => unreachable!(),
    }
}

fn get_left_modifier_code(key: &Key) -> KeyCode {
    match key {
        Key::Named(NamedKey::Alt) => KeyCode::AltLeft,
        Key::Named(NamedKey::Control) => KeyCode::ControlLeft,
        Key::Named(NamedKey::Shift) => KeyCode::ShiftLeft,
        Key::Named(NamedKey::Super) => KeyCode::SuperLeft,
        _ => unreachable!(),
    }
}

#[derive(Debug)]
pub struct ViewState {
    /// Strong reference to the global application state.
    app_delegate: aterm_objc::Retained<ApplicationDelegate>,

    cursor_state: RefCell<CursorState>,
    ime_position: Cell<CGPoint>,
    ime_size: Cell<CGSize>,
    modifiers: Cell<Modifiers>,
    phys_modifiers: RefCell<HashMap<Key, ModLocationMask>>,
    tracking_rect: Cell<Option<isize>>,
    ime_state: Cell<ImeState>,
    input_source: RefCell<String>,

    /// True iff the application wants IME events.
    ime_allowed: Cell<bool>,

    /// True if the current key event should be forwarded to the application, even during IME.
    forward_key_to_app: Cell<bool>,

    /// Option-as-Alt mapping configuration.
    option_as_alt: Cell<OptionAsAlt>,

    marked_text: RefCell<Obj>,

    _ns_window: WeakObj,
}

aterm_objc::declare_class! {
    /// The `NSView` this backend installs as every window's content view.
    pub(super) struct WinitView: NSView {
        const NAME: &str = "WinitView";
        type Ivars = ViewState;
        protocols: [NSTextInputClient];

        @sel(isFlipped) @abort_on_exception
        fn is_flipped(&self) -> Bool {
            Bool::YES
        }

        @sel(viewDidMoveToWindow)
        fn view_did_move_to_window(&self) {
            trace_scope!("viewDidMoveToWindow");
            self.reset_tracking_rect();
        }

        @sel(frameDidChange:)
        fn frame_did_change(&self, _note: Id) {
            trace_scope!("frameDidChange:");
            let rect = self.reset_tracking_rect();

            let logical_size = LogicalSize::new(rect.size.width as f64, rect.size.height as f64);
            let size = logical_size.to_physical::<u32>(self.scale_factor());
            self.queue_event(WindowEvent::Resized(size));
        }

        @sel(drawRect:)
        fn draw_rect(&self, _rect: CGRect) {
            trace_scope!("drawRect:");

            if let Some(window) = self.ivars()._ns_window.load() {
                self.ivars().app_delegate.handle_redraw(WinitWindow::id_of(window.id()));
            }

        }

        @sel(acceptsFirstResponder) @abort_on_exception
        fn accepts_first_responder(&self) -> Bool {
            trace_scope!("acceptsFirstResponder");
            Bool::YES
        }

        @sel(touchBar)
        fn touch_bar(&self) -> Id {
            trace_scope!("touchBar");
            Id::NIL
        }

        @sel(resetCursorRects)
        fn reset_cursor_rects(&self) {
            trace_scope!("resetCursorRects");
            // SAFETY: `self` is a live `WinitView`, hence a live `NSView`.
            unsafe {
                let bounds = send_rect(self.as_id(), sel!(bounds));
                let cursor_state = self.ivars().cursor_state.borrow();
                send_v_rect_id(
                    self.as_id(),
                    sel!(addCursorRect:cursor:),
                    bounds,
                    cursor_state.cursor.id(),
                );
            }
        }

        @sel(hasMarkedText)
        fn has_marked_text(&self) -> Bool {
            trace_scope!("hasMarkedText");
            Bool::new(self.marked_text_length() > 0)
        }

        @sel(markedRange) @abort_on_exception
        fn marked_range(&self) -> NSRange {
            trace_scope!("markedRange");
            let length = self.marked_text_length();
            if length > 0 {
                NSRange { location: 0, length }
            } else {
                NSRange { location: consts::NS_NOT_FOUND, length: 0 }
            }
        }

        @sel(selectedRange) @abort_on_exception
        fn selected_range(&self) -> NSRange {
            trace_scope!("selectedRange");
            NSRange { location: consts::NS_NOT_FOUND, length: 0 }
        }

        @sel(setMarkedText:selectedRange:replacementRange:)
        fn set_marked_text(
            &self,
            string: Id,
            selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            trace_scope!("setMarkedText:selectedRange:replacementRange:");

            let (marked_text, string) = unsafe {
                let mutable = alloc(class(c"NSMutableAttributedString"));
                if send_bool_cls(string, sel!(isKindOfClass:), class(c"NSAttributedString")) {
                    (
                        Obj::from_owned(send_id_id(
                            mutable,
                            sel!(initWithAttributedString:),
                            string,
                        ))
                        .expect("NSMutableAttributedString to accept an attributed string"),
                        Obj::retain(send_id(string, sel!(string)))
                            .expect("-[NSAttributedString string] to answer a string"),
                    )
                } else {
                    (
                        Obj::from_owned(send_id_id(mutable, sel!(initWithString:), string))
                            .expect("NSMutableAttributedString to accept a string"),
                        Obj::from_owned(send_id(string, sel!(copy)))
                            .expect("-[NSString copy] to answer a string"),
                    )
                }
            };

            *self.ivars().marked_text.borrow_mut() = marked_text;

            if self.ivars().ime_state.get() == ImeState::Disabled {
                *self.ivars().input_source.borrow_mut() = self.current_input_source();
                self.queue_event(WindowEvent::Ime(Ime::Enabled));
            }

            if self.has_marked_text().into() {
                self.ivars().ime_state.set(ImeState::Preedit);
            } else {
                self.ivars().ime_state.set(ImeState::Ground);
            }

            // SAFETY: `string` owns a +1 reference to a live `NSString`.
            let cursor_range = if unsafe { utf8_len(string.id()) } == 0 {
                None
            } else {
                // SAFETY: As above.
                let len = unsafe { utf16_len(string.id()) };
                let location = selected_range.location.min(len);
                let end = selected_range
                    .location
                    .saturating_add(selected_range.length)
                    .min(len);
                // SAFETY: `-substringToIndex:` is `@24@0:8Q16`.
                autoreleasepool(|_| unsafe {
                    let sub_a = send_id_usize(string.id(), sel!(substringToIndex:), location);
                    let sub_b = send_id_usize(string.id(), sel!(substringToIndex:), end);
                    Some((utf8_len(sub_a), utf8_len(sub_b)))
                })
            };

            // SAFETY: `string` owns a +1 reference to a live `NSString`.
            let text = unsafe { seam::nsstring_to_rust(string.id()) };
            self.queue_event(WindowEvent::Ime(Ime::Preedit(text, cursor_range)));
        }

        @sel(unmarkText)
        fn unmark_text(&self) {
            trace_scope!("unmarkText");
            *self.ivars().marked_text.borrow_mut() = new_marked_text();

            self.queue_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
            if self.is_ime_enabled() {
                self.ivars().ime_state.set(ImeState::Ground);
            } else {
                tracing::warn!("Expected to have IME enabled when receiving unmarkText");
            }

            // SAFETY: `-inputContext` `@16@0:8` (+0 borrowed).
            unsafe {
                let input_context = send_id(self.as_id(), sel!(inputContext));
                assert!(!input_context.is_null(), "input context");
                send_v(input_context, sel!(discardMarkedText));
            }
        }

        @sel(validAttributesForMarkedText)
        fn valid_attributes_for_marked_text(&self) -> Id {
            trace_scope!("validAttributesForMarkedText");
            // SAFETY: `+new` is `@16@0:8` and in the `new` family, so the array arrives +1.
            let array = unsafe { Obj::from_owned(send_id(class(c"NSArray").as_id(), sel!(new))) }
                .expect("+[NSArray new] to answer an array");
            array.autorelease()
        }

        @sel(attributedSubstringForProposedRange:actualRange:)
        fn attributed_substring_for_proposed_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> Id {
            trace_scope!("attributedSubstringForProposedRange:actualRange:");
            Id::NIL
        }

        @sel(characterIndexForPoint:) @abort_on_exception
        fn character_index_for_point(&self, _point: CGPoint) -> usize {
            trace_scope!("characterIndexForPoint:");
            0
        }

        @sel(firstRectForCharacterRange:actualRange:)
        fn first_rect_for_character_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> CGRect {
            trace_scope!("firstRectForCharacterRange:actualRange:");
            let rect = CGRect {
                origin: self.ivars().ime_position.get(),
                size: self.ivars().ime_size.get(),
            };
            // SAFETY: `-convertRect:toView:` is `{CGRect=…}56@0:8{CGRect=…}16@48` (`toView:` nil).
            unsafe {
                let in_window =
                    send_rect_rect_id(self.as_id(), sel!(convertRect:toView:), rect, Id::NIL);
                send_rect_rect(self.window().id(), sel!(convertRectToScreen:), in_window)
            }
        }

        @sel(insertText:replacementRange:)
        fn insert_text(&self, string: Id, _replacement_range: NSRange) {
            trace_scope!("insertText:replacementRange:");

            // SAFETY: AppKit passes an `NSString` or `NSAttributedString` to this method.
            let string = autoreleasepool(|_| unsafe {
                if send_bool_cls(string, sel!(isKindOfClass:), class(c"NSAttributedString")) {
                    seam::nsstring_to_rust(send_id(string, sel!(string)))
                } else {
                    seam::nsstring_to_rust(string)
                }
            });

            let is_control = string.chars().next().is_some_and(|c| c.is_control());

            if self.has_marked_text().into() && self.is_ime_enabled() && !is_control {
                self.queue_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                self.queue_event(WindowEvent::Ime(Ime::Commit(string)));
                self.ivars().ime_state.set(ImeState::Committed);
            }
        }

        @sel(doCommandBySelector:)
        fn do_command_by_selector(&self, _command: Sel) {
            trace_scope!("doCommandBySelector:");
            if self.ivars().ime_state.get() == ImeState::Committed {
                return;
            }

            self.ivars().forward_key_to_app.set(true);

            if self.has_marked_text().into() && self.ivars().ime_state.get() == ImeState::Preedit
            {
                self.ivars().ime_state.set(ImeState::Ground);
            }
        }

        @sel(keyDown:)
        fn key_down(&self, event: Id) {
            trace_scope!("keyDown:");
            {
                let mut prev_input_source = self.ivars().input_source.borrow_mut();
                let current_input_source = self.current_input_source();
                if *prev_input_source != current_input_source && self.is_ime_enabled() {
                    *prev_input_source = current_input_source;
                    drop(prev_input_source);
                    self.ivars().ime_state.set(ImeState::Disabled);
                    self.queue_event(WindowEvent::Ime(Ime::Disabled));
                }
            }

            let old_ime_state = self.ivars().ime_state.get();
            self.ivars().forward_key_to_app.set(false);
            let event = replace_event(event, self.ivars().option_as_alt.get());

            if self.ivars().ime_allowed.get() {
                // SAFETY: `+arrayWithObjects:count:` is `@32@0:8r^@16Q24`.
                autoreleasepool(|_| unsafe {
                    let one = [event.id()];
                    let events_for_nsview = send_id_idptr_usize(
                        class(c"NSArray").as_id(),
                        sel!(arrayWithObjects:count:),
                        one.as_ptr(),
                        1,
                    );
                    send_v_id(self.as_id(), sel!(interpretKeyEvents:), events_for_nsview);
                });

                if self.ivars().ime_state.get() == ImeState::Committed {
                    *self.ivars().marked_text.borrow_mut() = new_marked_text();
                }
            }

            self.update_modifiers(event.id(), false);

            let had_ime_input = match self.ivars().ime_state.get() {
                ImeState::Committed => {
                    self.ivars().ime_state.set(ImeState::Ground);
                    true
                }
                ImeState::Preedit => true,
                _ => old_ime_state != self.ivars().ime_state.get(),
            };

            if !had_ime_input || self.ivars().forward_key_to_app.get() {
                // SAFETY: `-isARepeat` is `B16@0:8` on `NSEvent`.
                let key_event =
                    create_key_event(event.id(), true, unsafe {
                        send_bool(event.id(), sel!(isARepeat))
                    });
                self.queue_event(WindowEvent::KeyboardInput {
                    device_id: DEVICE_ID,
                    event: key_event,
                    is_synthetic: false,
                });
            }
        }

        @sel(keyUp:)
        fn key_up(&self, event: Id) {
            trace_scope!("keyUp:");
            let event = replace_event(event, self.ivars().option_as_alt.get());
            self.update_modifiers(event.id(), false);

            if matches!(
                self.ivars().ime_state.get(),
                ImeState::Ground | ImeState::Disabled
            ) {
                self.queue_event(WindowEvent::KeyboardInput {
                    device_id: DEVICE_ID,
                    event: create_key_event(event.id(), false, false),
                    is_synthetic: false,
                });
            }
        }

        @sel(flagsChanged:)
        fn flags_changed(&self, event: Id) {
            trace_scope!("flagsChanged:");

            self.update_modifiers(event, true);
        }

        @sel(insertTab:)
        fn insert_tab(&self, _sender: Id) {
            trace_scope!("insertTab:");
            self.select_key_view(sel!(selectNextKeyView:));
        }

        @sel(insertBackTab:)
        fn insert_back_tab(&self, _sender: Id) {
            trace_scope!("insertBackTab:");
            self.select_key_view(sel!(selectPreviousKeyView:));
        }

        @sel(cancelOperation:)
        fn cancel_operation(&self, _sender: Id) {
            let _mtm = self.mtm();
            trace_scope!("cancelOperation:");

            // SAFETY: `+sharedApplication` `@16@0:8` (process-lifetime singleton).
            let event = unsafe {
                let app = send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
                let event = send_id(app, sel!(currentEvent));
                if event.is_null() {
                    return;
                }
                let ty = send_usize(event, sel!(type));
                if !(10..=11).contains(&ty) {
                    return;
                }
                event
            };

            self.update_modifiers(event, false);
            // SAFETY: `event` is the live `NSEvent` just read from AppKit.
            let event =
                create_key_event(event, true, unsafe { send_bool(event, sel!(isARepeat)) });

            self.queue_event(WindowEvent::KeyboardInput {
                device_id: DEVICE_ID,
                event,
                is_synthetic: false,
            });
        }

        @sel(mouseDown:)
        fn mouse_down(&self, event: Id) {
            trace_scope!("mouseDown:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Pressed);
        }

        @sel(mouseUp:)
        fn mouse_up(&self, event: Id) {
            trace_scope!("mouseUp:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Released);
        }

        @sel(rightMouseDown:)
        fn right_mouse_down(&self, event: Id) {
            trace_scope!("rightMouseDown:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Pressed);
        }

        @sel(rightMouseUp:)
        fn right_mouse_up(&self, event: Id) {
            trace_scope!("rightMouseUp:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Released);
        }

        @sel(otherMouseDown:)
        fn other_mouse_down(&self, event: Id) {
            trace_scope!("otherMouseDown:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Pressed);
        }

        @sel(otherMouseUp:)
        fn other_mouse_up(&self, event: Id) {
            trace_scope!("otherMouseUp:");
            self.mouse_motion(event);
            self.mouse_click(event, ElementState::Released);
        }

        @sel(mouseMoved:)
        fn mouse_moved(&self, event: Id) {
            self.mouse_motion(event);
        }

        @sel(mouseDragged:)
        fn mouse_dragged(&self, event: Id) {
            self.mouse_motion(event);
        }

        @sel(rightMouseDragged:)
        fn right_mouse_dragged(&self, event: Id) {
            self.mouse_motion(event);
        }

        @sel(otherMouseDragged:)
        fn other_mouse_dragged(&self, event: Id) {
            self.mouse_motion(event);
        }

        @sel(mouseEntered:)
        fn mouse_entered(&self, _event: Id) {
            trace_scope!("mouseEntered:");
            self.queue_event(WindowEvent::CursorEntered {
                device_id: DEVICE_ID,
            });
        }

        @sel(mouseExited:)
        fn mouse_exited(&self, _event: Id) {
            trace_scope!("mouseExited:");

            self.queue_event(WindowEvent::CursorLeft {
                device_id: DEVICE_ID,
            });
        }

        @sel(scrollWheel:)
        fn scroll_wheel(&self, event: Id) {
            trace_scope!("scrollWheel:");
            self.mouse_motion(event);

            // SAFETY: `-scrollingDeltaX`/`-scrollingDeltaY` are `d16@0:8`.
            let delta = {
                let (x, y) = unsafe {
                    (
                        send_f64(event, sel!(scrollingDeltaX)),
                        send_f64(event, sel!(scrollingDeltaY)),
                    )
                };
                if unsafe { send_bool(event, sel!(hasPreciseScrollingDeltas)) } {
                    let delta = LogicalPosition::new(x, y).to_physical(self.scale_factor());
                    MouseScrollDelta::PixelDelta(delta)
                } else {
                    MouseScrollDelta::LineDelta(x as f32, y as f32)
                }
            };

            // SAFETY: `-momentumPhase` and `-phase` are both `Q16@0:8`.
            let phase = match unsafe { send_usize(event, sel!(momentumPhase)) } {
                consts::NS_EVENT_PHASE_MAY_BEGIN | consts::NS_EVENT_PHASE_BEGAN => {
                    TouchPhase::Started
                },
                consts::NS_EVENT_PHASE_ENDED | consts::NS_EVENT_PHASE_CANCELLED => {
                    TouchPhase::Ended
                },
                _ => match unsafe { send_usize(event, sel!(phase)) } {
                    consts::NS_EVENT_PHASE_MAY_BEGIN | consts::NS_EVENT_PHASE_BEGAN => {
                        TouchPhase::Started
                    },
                    consts::NS_EVENT_PHASE_ENDED | consts::NS_EVENT_PHASE_CANCELLED => {
                        TouchPhase::Ended
                    },
                    _ => TouchPhase::Moved,
                },
            };

            self.update_modifiers(event, false);

            self.queue_event(WindowEvent::MouseWheel {
                device_id: DEVICE_ID,
                delta,
                phase,
            });
        }

        @sel(_wantsKeyDownForEvent:) @abort_on_exception
        fn wants_key_down_for_event(&self, _event: Id) -> Bool {
            trace_scope!("_wantsKeyDownForEvent:");
            Bool::YES
        }

        @sel(acceptsFirstMouse:) @abort_on_exception
        fn accepts_first_mouse(&self, _event: Id) -> Bool {
            trace_scope!("acceptsFirstMouse:");
            Bool::YES
        }
    }
}

impl WinitView {
    pub(super) fn new(
        app_delegate: &ApplicationDelegate,
        window: &WinitWindow,
    ) -> aterm_objc::Retained<Self> {
        let mt = aterm_objc::MainThread::new()
            .expect("WinitView::new off the main thread; AppKit builds views on it");
        let this = WinitView::alloc_init(mt, ViewState {
            app_delegate: app_delegate.retained(),
            cursor_state: Default::default(),
            ime_position: Default::default(),
            ime_size: Default::default(),
            modifiers: Default::default(),
            phys_modifiers: Default::default(),
            tracking_rect: Default::default(),
            ime_state: Default::default(),
            input_source: Default::default(),
            ime_allowed: Default::default(),
            forward_key_to_app: Default::default(),
            option_as_alt: Default::default(),
            marked_text: RefCell::new(new_marked_text()),
            // SAFETY: `as_id()` is a live non-null object pointer.
            _ns_window: unsafe { WeakObj::new(window.as_id()) },
        })
        .expect("couldn't create `WinitView`");

        // SAFETY: `-setPostsFrameChangedNotifications:` is `v20@0:8B16`.
        unsafe {
            send_v_bool(this.as_id(), sel!(setPostsFrameChangedNotifications:), true);
            let notification_center =
                send_id(class(c"NSNotificationCenter").as_id(), sel!(defaultCenter));
            send_v_id_sel_id_id(
                notification_center,
                sel!(addObserver:selector:name:object:),
                this.as_id(),
                sel!(frameDidChange:),
                seam::NS_VIEW_FRAME_DID_CHANGE_NOTIFICATION,
                this.as_id(),
            );
        }

        *this.ivars().input_source.borrow_mut() = this.current_input_source();

        this
    }

    /// Main-thread witness for AppKit view callbacks.
    #[track_caller]
    fn mtm(&self) -> aterm_objc::MainThread {
        aterm_objc::MainThread::new()
            .expect("a WinitView method ran off the main thread; AppKit delivers on it")
    }

    /// `-firstResponder` is this view, so move the key view on.
    fn select_key_view(&self, which: Sel) {
        // SAFETY: `-firstResponder` is `@16@0:8` on `NSWindow` and +0 borrowed.
        unsafe {
            let window = self.window();
            let first_responder = send_id(window.id(), sel!(firstResponder));
            if !first_responder.is_null()
                && send_bool_id(first_responder, sel!(isEqual:), self.as_id())
            {
                send_v_id(window.id(), which, self.as_id());
            }
        }
    }

    /// Marked text length in UTF-16 code units.
    fn marked_text_length(&self) -> usize {
        // SAFETY: the ivar holds a live `NSMutableAttributedString`.
        unsafe { send_usize(self.ivars().marked_text.borrow().id(), sel!(length)) }
    }

    /// Replace the tracking rect over the current frame and return that frame.
    fn reset_tracking_rect(&self) -> CGRect {
        // SAFETY: `-removeTrackingRect:` is `v24@0:8q16`.
        unsafe {
            if let Some(tracking_rect) = self.ivars().tracking_rect.take() {
                send_v_isize(self.as_id(), sel!(removeTrackingRect:), tracking_rect);
            }

            let rect = send_rect(self.as_id(), sel!(frame));
            let tracking_rect = send_isize_rect_id_ptr_bool(
                self.as_id(),
                sel!(addTrackingRect:owner:userData:assumeInside:),
                rect,
                self.as_id(),
                ptr::null_mut(),
                false,
            );
            assert_ne!(tracking_rect, 0, "failed adding tracking rect");
            self.ivars().tracking_rect.set(Some(tracking_rect));
            rect
        }
    }

    /// Returns the owning `NSWindow` as [`Obj`].
    fn window(&self) -> Obj {
        self.ivars()._ns_window.load().expect("view to have a window")
    }

    fn queue_event(&self, event: WindowEvent) {
        let id = WinitWindow::id_of(self.window().id());
        self.ivars().app_delegate.maybe_queue_window_event(id, event);
    }

    fn scale_factor(&self) -> f64 {
        // SAFETY: `-backingScaleFactor` is `d16@0:8` on `NSWindow`.
        unsafe { send_f64(self.window().id(), sel!(backingScaleFactor)) }
    }

    fn is_ime_enabled(&self) -> bool {
        !matches!(self.ivars().ime_state.get(), ImeState::Disabled)
    }

    fn current_input_source(&self) -> String {
        // SAFETY: `-inputContext` and `-selectedKeyboardInputSource` are both `@16@0:8` and +0.
        autoreleasepool(|_| unsafe {
            let input_context = send_id(self.as_id(), sel!(inputContext));
            assert!(!input_context.is_null(), "input context");
            let source = send_id(input_context, sel!(selectedKeyboardInputSource));
            seam::nsstring_to_rust(source)
        })
    }

    /// Current cursor icon as a retained `Obj`.
    pub(super) fn cursor_icon(&self) -> Obj {
        self.ivars().cursor_state.borrow().cursor.clone_retained()
    }

    pub(super) fn set_cursor_icon(&self, icon: Obj) {
        let mut cursor_state = self.ivars().cursor_state.borrow_mut();
        cursor_state.cursor = icon;
    }

    pub(super) fn set_option_as_alt(&self, option_as_alt: OptionAsAlt) {
        self.ivars().option_as_alt.set(option_as_alt);
    }

    pub(super) fn option_as_alt(&self) -> OptionAsAlt {
        self.ivars().option_as_alt.get()
    }

    pub(super) fn set_ime_allowed(&self, ime_allowed: bool) {
        if self.ivars().ime_allowed.get() == ime_allowed {
            return;
        }
        self.ivars().ime_allowed.set(ime_allowed);
        if self.ivars().ime_allowed.get() {
            return;
        }

        *self.ivars().marked_text.borrow_mut() = new_marked_text();

        if self.ivars().ime_state.get() != ImeState::Disabled {
            self.ivars().ime_state.set(ImeState::Disabled);
            self.queue_event(WindowEvent::Ime(Ime::Disabled));
        }
    }

    pub(super) fn set_ime_cursor_area(&self, position: CGPoint, size: CGSize) {
        self.ivars().ime_position.set(position);
        self.ivars().ime_size.set(size);
        // SAFETY: `-inputContext` `@16@0:8` (+0) and `-invalidateCharacterCoordinates` `v16@0:8`.
        unsafe {
            let input_context = send_id(self.as_id(), sel!(inputContext));
            assert!(!input_context.is_null(), "input context");
            send_v(input_context, sel!(invalidateCharacterCoordinates));
        }
    }

    /// Reset modifiers and emit a synthetic ModifiersChanged event if deemed necessary.
    pub(super) fn reset_modifiers(&self) {
        if !self.ivars().modifiers.get().state().is_empty() {
            self.ivars().modifiers.set(Modifiers::default());
            self.queue_event(WindowEvent::ModifiersChanged(self.ivars().modifiers.get()));
        }
    }

    /// Last modifier state derived from an event.
    pub(super) fn cached_modifiers(&self) -> ModifiersState {
        self.ivars().modifiers.get().state()
    }

    /// Update modifiers if `ns_event` has something different.
    fn update_modifiers(&self, ns_event: Id, is_flags_changed_event: bool) {
        use ElementState::{Pressed, Released};

        let current_modifiers = event_mods(ns_event);
        let prev_modifiers = self.ivars().modifiers.get();
        self.ivars().modifiers.set(current_modifiers);

        'send_event: {
            // SAFETY: `-keyCode` is `S16@0:8` on `NSEvent` — an UNSIGNED short.
            let scancode = if is_flags_changed_event {
                unsafe { send_u16(ns_event, sel!(keyCode)) }
            } else {
                0
            };
            if is_flags_changed_event && scancode != 0 {
                let physical_key = scancode_to_physicalkey(scancode as u32);

                let logical_key = code_to_key(physical_key, scancode);
                let Some(event_modifier) = key_to_modifier(&logical_key) else {
                    break 'send_event;
                };

                let mut event = KeyEvent {
                    location: code_to_location(physical_key),
                    logical_key: logical_key.clone(),
                    physical_key,
                    repeat: false,
                    state: Pressed,
                    text: None,
                    platform_specific: KeyEventExtra {
                        text_with_all_modifiers: None,
                        key_without_modifiers: logical_key.clone(),
                    },
                };

                let location_mask = ModLocationMask::from_location(event.location);

                let mut phys_mod_state = self.ivars().phys_modifiers.borrow_mut();
                let phys_mod =
                    phys_mod_state.entry(logical_key).or_insert(ModLocationMask::empty());

                let is_active = current_modifiers.state().contains(event_modifier);
                let mut events = VecDeque::with_capacity(2);

                if !is_active {
                    event.state = Released;
                    if phys_mod.contains(ModLocationMask::LEFT) {
                        let mut event = event.clone();
                        event.location = KeyLocation::Left;
                        event.physical_key = get_left_modifier_code(&event.logical_key).into();
                        events.push_back(WindowEvent::KeyboardInput {
                            device_id: DEVICE_ID,
                            event,
                            is_synthetic: false,
                        });
                    }
                    if phys_mod.contains(ModLocationMask::RIGHT) {
                        event.location = KeyLocation::Right;
                        event.physical_key = get_right_modifier_code(&event.logical_key).into();
                        events.push_back(WindowEvent::KeyboardInput {
                            device_id: DEVICE_ID,
                            event,
                            is_synthetic: false,
                        });
                    }
                    *phys_mod = ModLocationMask::empty();
                } else {
                    if *phys_mod == location_mask {
                        event.state = Pressed;
                    } else {
                        phys_mod.toggle(location_mask);
                        let is_pressed = phys_mod.contains(location_mask);
                        event.state = if is_pressed { Pressed } else { Released };
                    }

                    events.push_back(WindowEvent::KeyboardInput {
                        device_id: DEVICE_ID,
                        event,
                        is_synthetic: false,
                    });
                }

                drop(phys_mod_state);

                for event in events {
                    self.queue_event(event);
                }
            }
        }

        if prev_modifiers == current_modifiers {
            return;
        }

        self.queue_event(WindowEvent::ModifiersChanged(self.ivars().modifiers.get()));
    }

    fn mouse_click(&self, event: Id, button_state: ElementState) {
        let button = mouse_button(event);

        self.update_modifiers(event, false);

        self.queue_event(WindowEvent::MouseInput {
            device_id: DEVICE_ID,
            state: button_state,
            button,
        });
    }

    fn mouse_motion(&self, event: Id) {
        // SAFETY: `-locationInWindow` is `{CGPoint=dd}16@0:8`.
        let (window_point, view_point, frame) = unsafe {
            let window_point = send_point(event, sel!(locationInWindow));
            (
                window_point,
                send_point_point_id(
                    self.as_id(),
                    sel!(convertPoint:fromView:),
                    window_point,
                    Id::NIL,
                ),
                send_rect(self.as_id(), sel!(frame)),
            )
        };
        let _ = window_point;

        if view_point.x.is_sign_negative()
            || view_point.y.is_sign_negative()
            || view_point.x > frame.size.width
            || view_point.y > frame.size.height
        {
            // SAFETY: `+pressedMouseButtons` is `Q16@0:8` on `NSEvent`.
            let mouse_buttons_down =
                unsafe { send_usize(class(c"NSEvent").as_id(), sel!(pressedMouseButtons)) };
            if mouse_buttons_down == 0 {
                return;
            }
        }

        let view_point = LogicalPosition::new(view_point.x, view_point.y);

        self.update_modifiers(event, false);

        self.queue_event(WindowEvent::CursorMoved {
            device_id: DEVICE_ID,
            position: view_point.to_physical(self.scale_factor()),
        });
    }
}

/// UTF-8 byte length of a live `NSString` `s`.
/// # Safety
/// `s` must be a live `NSString`.
unsafe fn utf8_len(s: Id) -> usize {
    // SAFETY: The caller pins `s` as a live `NSString`.
    unsafe {
        send_usize_usize(s, sel!(lengthOfBytesUsingEncoding:), consts::NS_UTF8_STRING_ENCODING)
    }
}

/// UTF-16 code-unit length of a live `NSString` `s`.
/// # Safety
/// `s` must be a live `NSString`.
unsafe fn utf16_len(s: Id) -> usize {
    // SAFETY: The caller pins `s` as a live `NSString`; `-length` is `Q16@0:8`.
    unsafe { send_usize(s, sel!(length)) }
}

/// Creates an empty `+1` `NSMutableAttributedString`.
fn new_marked_text() -> Obj {
    // SAFETY: `+new` is `@16@0:8` and is in the `new` family.
    unsafe { Obj::from_owned(send_id(class(c"NSMutableAttributedString").as_id(), sel!(new))) }
        .expect("+[NSMutableAttributedString new] to answer a string")
}

/// Get the mouse button from the NSEvent.
fn mouse_button(event: Id) -> MouseButton {
    // SAFETY: `-buttonNumber` is `q16@0:8` on `NSEvent`.
    match unsafe { send_isize(event, sel!(buttonNumber)) } {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Back,
        4 => MouseButton::Forward,
        n => MouseButton::Other(n as u16),
    }
}

/// Returns a `+1` key event, stripping Option characters when [`option_is_alt`] holds.
fn replace_event(event: Id, option_as_alt: OptionAsAlt) -> Obj {
    // SAFETY: `event` is the live `NSEvent` AppKit delivered.
    let (kind, flags) =
        unsafe { (send_usize(event, sel!(type)), send_usize(event, sel!(modifierFlags))) };
    if !(10..=11).contains(&kind) || !option_is_alt(option_as_alt, flags) {
        return copy_event(event);
    }
    autoreleasepool(|_| {
        // SAFETY: `event` is a key event, so `-charactersIgnoringModifiers` is `@16@0:8`.
        let chars = unsafe { send_id(event, sel!(charactersIgnoringModifiers)) };
        // SAFETY: `chars` is nil or the live `NSString` just read.
        if chars.is_null() || unsafe { seam::nsstring_to_rust(chars) }.is_empty() {
            return copy_event(event);
        }
        // SAFETY: the receiver is the `NSEvent` class.
        let rebuilt = unsafe {
            send_id_usize_point_usize_f64_isize_id_id_id_bool_u16(
                class(c"NSEvent").as_id(),
                sel!(
                    keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                ),
                kind,
                send_point(event, sel!(locationInWindow)),
                flags,
                send_f64(event, sel!(timestamp)),
                send_isize(event, sel!(windowNumber)),
                Id::NIL,
                chars,
                chars,
                send_bool(event, sel!(isARepeat)),
                send_u16(event, sel!(keyCode)),
            )
        };
        // SAFETY: `rebuilt` is nil or the live autoreleased event.
        unsafe { Obj::retain(rebuilt) }.unwrap_or_else(|| copy_event(event))
    })
}

/// A +1 copy of a key event (`replace_event` with nothing to rebuild).
fn copy_event(event: Id) -> Obj {
    // SAFETY: `-copy` is `@16@0:8` and is in the `copy` family.
    unsafe { Obj::from_owned(send_id(event, sel!(copy))) }
        .expect("-[NSEvent copy] to answer an event")
}
