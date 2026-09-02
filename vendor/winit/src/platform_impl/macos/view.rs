// Modified by the aterm project in 2026; see the repository NOTICE.
// (The WinitView class and all 43 of its trampolines are DECLARED with
// `aterm_objc::declare_class!` instead of objc2's; its `-dealloc` and Drop moved
// to the ivars; the weak window reference is an `aterm_objc::WeakObj`; the two
// IME-geometry ivars are `aterm_objc`'s `CGPoint`/`CGSize`; and W9 phase 2
// replaced every remaining `objc2` BINDING call in every method body with a
// typed send. Search for the aterm local-patch marker.)
//
// NOTE ON THIS NOTICE: it was MISSING until W8, through three waves that
// modified this file heavily — the Apache-2.0 4(b) obligation is on the file,
// and a fork discipline that is applied to some modified files and not others
// is not a discipline. `monitor.rs` was the same and is fixed in the same
// commit.
#![allow(clippy::unnecessary_cast)]
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ptr;

// LOCAL PATCH (aterm): W8 took objc2's class-DECLARATION surface; W9 phase 2
// took the last of it, the twenty-three BINDING names the method bodies
// imported. `Obj` is the +1 handle `Retained<T>` was, `Id` the borrowed
// receiver every trampoline already held, `WeakObj` the weak window reference.
// Two of the twenty-three needed no replacement at all — `NSTrackingRectTag` is
// a bare `NSInteger` and `NSAttributedStringKey` a bare `NSString *`.
use aterm_objc::send::{
    alloc, send_bool, send_bool_cls, send_bool_id, send_f32, send_f64, send_id, send_id_id,
    send_id_idptr_usize, send_id_usize, send_id_usize_point_usize_f64_isize_id_id_id_bool_u16,
    send_isize, send_isize_rect_id_ptr_bool, send_point, send_point_point_id, send_rect,
    send_rect_rect, send_rect_rect_id, send_u16, send_usize, send_usize_usize, send_v, send_v_bool,
    send_v_id, send_v_id_sel_id_id, send_v_isize, send_v_rect_id,
};
use aterm_objc::{
    Bool, CGPoint, CGRect, CGSize, Id, NSRange, Obj, Sel, WeakObj, autoreleasepool, class, sel,
};

use super::aterm_objc_seam::{self as seam, consts};
use super::app_state::ApplicationDelegate;
use super::cursor::{default_cursor, invisible_cursor};
use super::event::{
    code_to_key, code_to_location, create_key_event, event_mods, lalt_pressed, ralt_pressed,
    scancode_to_physicalkey, KeyEventExtra,
};
use super::window::WinitWindow;
use super::DEVICE_ID;
use crate::dpi::{LogicalPosition, LogicalSize};
use crate::event::{
    DeviceEvent, ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase,
    WindowEvent,
};
use crate::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey};
use crate::platform::macos::OptionAsAlt;

#[derive(Debug)]
struct CursorState {
    visible: bool,
    // LOCAL PATCH (aterm): `Retained<NSCursor>` -> `Obj`. The reason
    // `cursor.rs` was ported before this file: an objc2 handle held as STATE in
    // a class `aterm_objc` declares, released by the generated `-dealloc`.
    cursor: Obj,
}

impl Default for CursorState {
    fn default() -> Self {
        Self { visible: true, cursor: default_cursor() }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Default)]
enum ImeState {
    #[default]
    /// The IME events are disabled, so only `ReceivedCharacter` is being sent to the user.
    Disabled,

    /// The ground state of enabled IME input. It means that both Preedit and regular keyboard
    /// input could be start from it.
    Ground,

    /// The IME is in preedit.
    Preedit,

    /// The text was just committed, so the next input from the keyboard must be ignored.
    Committed,
}

bitflags::bitflags! {
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

fn key_to_modifier(key: &Key) -> Option<ModifiersState> {
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
    // LOCAL PATCH (aterm): `NSPoint`/`NSSize` -> `aterm_objc`'s. The pairs are
    // `#[repr(C)]` over `f64` in the same order, so this is a re-typing; what
    // it buys is that `window_delegate.rs` — whose bindings W8 ported — hands
    // these in the same types the sends below take.
    ime_position: Cell<CGPoint>,
    ime_size: Cell<CGSize>,
    modifiers: Cell<Modifiers>,
    phys_modifiers: RefCell<HashMap<Key, ModLocationMask>>,
    // LOCAL PATCH (aterm): `NSTrackingRectTag` is `typedef NSInteger`;
    // `-addTrackingRect:…` answers `q` and `-removeTrackingRect:` takes `q`.
    tracking_rect: Cell<Option<isize>>,
    ime_state: Cell<ImeState>,
    input_source: RefCell<String>,

    /// True iff the application wants IME events.
    ///
    /// Can be set using `set_ime_allowed`
    ime_allowed: Cell<bool>,

    /// True if the current key event should be forwarded
    /// to the application, even during IME
    forward_key_to_app: Cell<bool>,

    // LOCAL PATCH (aterm): `Retained<NSMutableAttributedString>` -> `Obj`, and
    // no longer `Default`-constructed: an empty attributed string is a message
    // send, not a zero. `WinitView::new` builds it explicitly.
    marked_text: RefCell<Obj>,
    accepts_first_mouse: bool,

    // Weak reference because the window keeps a strong reference to the view
    //
    // LOCAL PATCH (aterm): `WeakId<WinitWindow>` -> `WeakId<NSWindow>` (W8) ->
    // `aterm_objc::WeakObj` (W9 phase 2). The field the `weak` module was
    // written for, and why it is UNTYPED: objc2's `WeakId<T>` needs
    // `T: Message + IsRetainable`, so W8 had to widen to a BINDING type just to
    // keep a weak reference. The cycle broken is the same one, the object the
    // same object, and the only consumer of the type — `WinitWindow::id_of` —
    // is an address formula.
    _ns_window: WeakObj,

    /// The state of the `Option` as `Alt`.
    option_as_alt: Cell<OptionAsAlt>,
}

// LOCAL PATCH (aterm): the class pair, the ivars, the `dealloc` and all 43
// trampolines below are declared by `aterm_objc::declare_class!` rather than
// `objc2::declare_class!` — the same move `window_delegate.rs` made, against a
// much harder surface. The METHOD BODIES still speak objc2 for their AppKit
// BINDINGS; what is first-party is the class creation, the ivar slot, the panic
// guards and every type encoding.
//
// The 44 registered rows (43 declared + the generated `-dealloc`) are READ OFF
// THE LIVE CLASS by `crates/aterm-gui/examples/objc_live_class_audit.rs`, which
// the verify ladder runs, and which now audits THIS class beside the delegate.
// Do not maintain a count here; that is what made `window_delegate.rs:156`
// stale about its own commit.
//
// THREE SHAPES THIS FILE IS THE FIRST TO REGISTER, and why each is not silent:
//
//  1. A NON-`NSObject` SUPERCLASS. `WinitView` is an `NSView`, so the ivar slot
//     sits past `NSView`'s own storage and `-dealloc` super-sends into `NSView`.
//     The chain is checked live (`WinitView -> NSView -> NSResponder ->
//     NSObject`).
//  2. STRUCT RETURNS — `markedRange` and `selectedRange` return `NSRange`,
//     `firstRectForCharacterRange:actualRange:` returns `NSRect`. NEITHER takes
//     the arm64 indirect path, and the campaign's premise that they would is
//     REFUTED: `NSRange` is 16 bytes and comes back in `x0`/`x1`, `NSRect` is a
//     homogeneous floating-point aggregate and comes back in `d0`-`d3`. The
//     indirect path is x86_64's here (`objc_msgSend_stret`), and `aterm-objc`
//     picks it from the return type in a `const` block. P1's
//     `tests/stret_declared.rs` executes the arm64 indirect path with a 24-byte
//     non-HFA so the DECLARED side of `x8` is proved by something.
//  3. `NSTextInputClient` — an IME surface Foundation reads the frame layout
//     for out of the REGISTERED STRING. A wrong encoding here is not a silent
//     lie: it becomes a candidate window at a garbage rectangle. Every row's
//     encoding is checked against `protocol_getMethodDescription` on the live
//     protocol, and the IME path is driven end to end by
//     `crates/aterm-gui/examples/objc_ime_drive.rs`.
//
// The `protocols:` list is ONE name, and that is measured, not assumed: objc2's
// `unsafe impl NSTextInputClient for WinitView` called `class_addProtocol` once,
// and the live class before this port claimed exactly `["NSTextInputClient"]`.
// There is no `NSObjectProtocol` here (unlike `WindowDelegate`, which objc2 gave
// one), so adding `NSObject` would have made the ported class claim MORE than
// the class it replaces.
aterm_objc::declare_class! {
    /// The `NSView` this backend installs as every window's content view: the
    /// key/mouse/gesture responder, the `NSTextInputClient` that drives IME, and
    /// the owner of the per-view state.
    pub(super) struct WinitView: NSView {
        const NAME: &str = "WinitView";
        type Ivars = ViewState;
        protocols: [NSTextInputClient];

        @sel(isFlipped)
        fn is_flipped(&self) -> Bool {
            // `winit` uses the upper-left corner as the origin.
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

            // Emit resize event here rather than from windowDidResize because:
            // 1. When a new window is created as a tab, the frame size may change without a window resize occurring.
            // 2. Even when a window resize does occur on a new tabbed window, it contains the wrong size (includes tab height).
            let logical_size = LogicalSize::new(rect.size.width as f64, rect.size.height as f64);
            let size = logical_size.to_physical::<u32>(self.scale_factor());
            self.queue_event(WindowEvent::Resized(size));
        }

        @sel(drawRect:)
        fn draw_rect(&self, _rect: CGRect) {
            trace_scope!("drawRect:");

            // It's a workaround for https://github.com/rust-windowing/winit/issues/2640, don't replace with `self.window_id()`.
            if let Some(window) = self.ivars()._ns_window.load() {
                self.ivars().app_delegate.handle_redraw(WinitWindow::id_of(window.id()));
            }

            // This is a direct subclass of NSView, no need to call superclass' drawRect:
        }

        @sel(acceptsFirstResponder)
        fn accepts_first_responder(&self) -> Bool {
            trace_scope!("acceptsFirstResponder");
            Bool::YES
        }

        // This is necessary to prevent a beefy terminal error on MacBook Pros:
        // IMKInputSession [0x7fc573576ff0 presentFunctionRowItemTextInputViewWithEndpoint:completionHandler:] : [self textInputContext]=0x7fc573558e10 *NO* NSRemoteViewController to client, NSError=Error Domain=NSCocoaErrorDomain Code=4099 "The connection from pid 0 was invalidated from this process." UserInfo={NSDebugDescription=The connection from pid 0 was invalidated from this process.}, com.apple.inputmethod.EmojiFunctionRowItem
        // TODO: Add an API extension for using `NSTouchBar`
        // LOCAL PATCH (aterm): objc2's `#[method_id(touchBar)]` wrapped the
        // `Option<Retained<NSObject>>` return in its +0 convention. `nil` is
        // the same pointer under either, and the ownership question a `None`
        // return raises is no question at all.
        @sel(touchBar)
        fn touch_bar(&self) -> Id {
            trace_scope!("touchBar");
            Id::NIL
        }

        @sel(resetCursorRects)
        fn reset_cursor_rects(&self) {
            trace_scope!("resetCursorRects");
            // SAFETY: `self` is a live `WinitView`, hence a live `NSView`.
            // `-bounds` is `{CGRect=…}16@0:8` and `-addCursorRect:cursor:` is
            // `v56@0:8{CGRect=…}16@48`; the cursor is borrowed for the call.
            unsafe {
                let bounds = send_rect(self.as_id(), sel!(bounds));
                let cursor_state = self.ivars().cursor_state.borrow();
                // We correctly invoke `addCursorRect` only from inside `resetCursorRects`
                if cursor_state.visible {
                    send_v_rect_id(
                        self.as_id(),
                        sel!(addCursorRect:cursor:),
                        bounds,
                        cursor_state.cursor.id(),
                    );
                } else {
                    let invisible = invisible_cursor();
                    send_v_rect_id(
                        self.as_id(),
                        sel!(addCursorRect:cursor:),
                        bounds,
                        invisible.id(),
                    );
                }
            }
        }

        // ------------------------------------------------------------------
        // NSTextInputClient. Eleven rows, and the reason this file is the risk
        // in the port: Foundation reads the frame layout for every one of them
        // out of the string `class_addMethod` was handed, so an encoding that
        // is merely PLAUSIBLE puts the candidate window at a garbage rectangle
        // instead of raising. Each row's encoding is checked against
        // `protocol_getMethodDescription(NSTextInputClient, …)` by the live
        // auditor, and the composition path is driven for real by
        // `objc_ime_drive.rs`.
        // ------------------------------------------------------------------

        @sel(hasMarkedText)
        fn has_marked_text(&self) -> Bool {
            trace_scope!("hasMarkedText");
            Bool::new(self.marked_text_length() > 0)
        }

        @sel(markedRange)
        fn marked_range(&self) -> NSRange {
            trace_scope!("markedRange");
            let length = self.marked_text_length();
            if length > 0 {
                NSRange { location: 0, length }
            } else {
                // Documented to return `{NSNotFound, 0}` if there is no marked range.
                NSRange { location: consts::NS_NOT_FOUND, length: 0 }
            }
        }

        @sel(selectedRange)
        fn selected_range(&self) -> NSRange {
            trace_scope!("selectedRange");
            // Documented to return `{NSNotFound, 0}` if there is no selection.
            NSRange { location: consts::NS_NOT_FOUND, length: 0 }
        }

        @sel(setMarkedText:selectedRange:replacementRange:)
        fn set_marked_text(
            &self,
            string: Id,
            selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            // TODO: Use _replacement_range, requires changing the event to report surrounding text.
            trace_scope!("setMarkedText:selectedRange:replacementRange:");

            // LOCAL PATCH (aterm): objc2's three narrowing casts are gone with
            // the types; `-isKindOfClass:` decides as before. OWNERSHIP is
            // where the arms differ and always did: `-string` is +0
            // autoreleased and RETAINED, `-copy` is +1 and ADOPTED. Same way
            // round would leak or double-free one composition per keystroke.
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

            // Update marked text.
            *self.ivars().marked_text.borrow_mut() = marked_text;

            // Notify IME is active if application still doesn't know it.
            if self.ivars().ime_state.get() == ImeState::Disabled {
                *self.ivars().input_source.borrow_mut() = self.current_input_source();
                self.queue_event(WindowEvent::Ime(Ime::Enabled));
            }

            // LOCAL PATCH (aterm): this was `unsafe { self.hasMarkedText() }`,
            // an objc2 protocol method that round-tripped through
            // `objc_msgSend` to reach this same class's own IMP. The declared
            // row's body is now an inherent Rust method, so it is called
            // directly. Same answer, one dispatch fewer, and `Bool` rather
            // than `bool` at the boundary.
            if self.has_marked_text().into() {
                self.ivars().ime_state.set(ImeState::Preedit);
            } else {
                // In case the preedit was cleared, set IME into the Ground state.
                self.ivars().ime_state.set(ImeState::Ground);
            }

            // SAFETY: `string` owns a +1 reference to a live `NSString`.
            let cursor_range = if unsafe { utf8_len(string.id()) } == 0 {
                // An empty string basically means that there's no preedit, so indicate that by
                // sending a `None` cursor range.
                //
                // LOCAL PATCH (aterm): objc2's `is_empty()` is `len() == 0` and
                // its `len()` is the UTF-8 BYTE count. `-length == 0` is the
                // same answer HERE and the wrong one three lines down.
                None
            } else {
                // Clamp to string length to avoid NSRangeException from out-of-bounds
                // indices sent by macOS IME (e.g. native Pinyin, see
                // https://github.com/alacritty/alacritty/issues/8791).
                //
                // LOCAL PATCH (aterm): UTF-16 — the unit the input method's
                // `selected_range` uses and `-substringToIndex:` counts.
                // SAFETY: as above.
                let len = unsafe { utf16_len(string.id()) };
                let location = selected_range.location.min(len);
                // LOCAL PATCH (aterm): objc2's `NSRange::end()`, spelled out.
                // `aterm_objc::NSRange` is a two-field POD by design — it
                // exists to carry `{_NSRange=QQ}` across a declared boundary,
                // not to reimplement Foundation's range algebra. Saturating,
                // because `location + length` is what `end()` computes and a
                // hostile IME can hand us a pair that overflows `usize`, which
                // would panic in a debug build inside an ObjC frame.
                let end = selected_range
                    .location
                    .saturating_add(selected_range.length)
                    .min(len);
                // Convert the selected range from UTF-16 indices to UTF-8 indices.
                //
                // LOCAL PATCH (aterm): THE CONVERSION, and where the two length
                // helpers earn their names — `-substringToIndex:` cuts at a
                // UTF-16 index and `utf8_len` measures the piece in BYTES.
                //
                // SAFETY: `-substringToIndex:` is `@24@0:8Q16`; both indices
                // were clamped to `-length` above, so neither can raise
                // `NSRangeException`. Results are +0 autoreleased and are
                // measured before the pool pops.
                autoreleasepool(|_| unsafe {
                    let sub_a = send_id_usize(string.id(), sel!(substringToIndex:), location);
                    let sub_b = send_id_usize(string.id(), sel!(substringToIndex:), end);
                    Some((utf8_len(sub_a), utf8_len(sub_b)))
                })
            };

            // Send WindowEvent for updating marked text
            // SAFETY: `string` owns a +1 reference to a live `NSString`.
            let text = unsafe { seam::nsstring_to_rust(string.id()) };
            self.queue_event(WindowEvent::Ime(Ime::Preedit(text, cursor_range)));
        }

        @sel(unmarkText)
        fn unmark_text(&self) {
            trace_scope!("unmarkText");
            *self.ivars().marked_text.borrow_mut() = new_marked_text();

            // SAFETY: `-inputContext` `@16@0:8` (+0 borrowed);
            // `-discardMarkedText` `v16@0:8` on `NSTextInputContext`.
            unsafe {
                let input_context = send_id(self.as_id(), sel!(inputContext));
                assert!(!input_context.is_null(), "input context");
                send_v(input_context, sel!(discardMarkedText));
            }

            self.queue_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
            if self.is_ime_enabled() {
                // Leave the Preedit self.ivars()
                self.ivars().ime_state.set(ImeState::Ground);
            } else {
                tracing::warn!("Expected to have IME enabled when receiving unmarkText");
            }
        }

        // LOCAL PATCH (aterm): objc2's `#[method_id(…)]` applied the +0
        // convention to this return; `aterm_objc` returns a raw `Id`, so the
        // convention is applied HERE and visibly. `NSArray::new()` is +1, and a
        // selector not named `new*`/`alloc`/`copy*` owes its caller +0, so the
        // +1 goes to the pool. This used to add that the +1 "leaks one empty
        // array per call": MEASURED FALSE — an empty NSArray is the immortal
        // `__NSArray0` singleton (retainCount -1, no-op retain/release). The
        // convention holds anyway; the singleton is Foundation's to withdraw.
        @sel(validAttributesForMarkedText)
        fn valid_attributes_for_marked_text(&self) -> Id {
            trace_scope!("validAttributesForMarkedText");
            // SAFETY: `+new` is `@16@0:8` and in the `new` family, so the array
            // arrives +1; `Obj::autorelease` hands it to the innermost pool and
            // returns the same pointer, borrowed until that pool pops.
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

        @sel(characterIndexForPoint:)
        // LOCAL PATCH (aterm): `NSUInteger` was an `objc2-foundation` type
        // ALIAS for `usize` and carried nothing else; the declared row's
        // encoding is `Q` either way.
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
            // LOCAL PATCH (aterm): the two IME ivars are `aterm_objc`'s
            // `CGPoint`/`CGSize` as of W8 (see `ViewState`), because
            // `window_delegate.rs` — whose bindings W8 ported — is what writes
            // them. The conversion INTO objc2's `NSRect` is here, at the two
            // binding calls that still need one, and it is a field copy rather
            // than a transmute so the compiler re-checks the layouts.
            let rect = CGRect {
                origin: self.ivars().ime_position.get(),
                size: self.ivars().ime_size.get(),
            };
            // Return value is expected to be in screen coordinates, so we need a conversion here
            //
            // LOCAL PATCH (aterm): the computation is all `aterm_objc` `CGRect`
            // now, so `seam::cg_rect` — which existed for THIS ONE CALL SITE
            // and said so — is deleted with this commit.
            //
            // SAFETY: `-convertRect:toView:` is `{CGRect=…}56@0:8{CGRect=…}16@48`
            // with a nil `toView:` meaning the window's base coordinates;
            // `-convertRectToScreen:` is `{CGRect=…}48@0:8{CGRect=…}16`. Neither
            // takes the arm64 indirect path (a 32-byte homogeneous float
            // aggregate returns in `d0`-`d3`); `msg` picks
            // `objc_msgSend_stret` for the x86_64 slice from the return type.
            unsafe {
                let in_window =
                    send_rect_rect_id(self.as_id(), sel!(convertRect:toView:), rect, Id::NIL);
                send_rect_rect(self.window().id(), sel!(convertRectToScreen:), in_window)
            }
        }

        @sel(insertText:replacementRange:)
        fn insert_text(&self, string: Id, _replacement_range: NSRange) {
            // TODO: Use _replacement_range, requires changing the event to report surrounding text.
            trace_scope!("insertText:replacementRange:");

            // SAFETY: This method is guaranteed to get either a `NSString` or a `NSAttributedString`
            // — see `setMarkedText:`. `-string` is `@16@0:8`, +0 autoreleased,
            // so the pool is explicit and the bytes are copied before it pops.
            let string = autoreleasepool(|_| unsafe {
                if send_bool_cls(string, sel!(isKindOfClass:), class(c"NSAttributedString")) {
                    seam::nsstring_to_rust(send_id(string, sel!(string)))
                } else {
                    seam::nsstring_to_rust(string)
                }
            });

            let is_control = string.chars().next().is_some_and(|c| c.is_control());

            // Commit only if we have marked text.
            if self.has_marked_text().into() && self.is_ime_enabled() && !is_control {
                self.queue_event(WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                self.queue_event(WindowEvent::Ime(Ime::Commit(string)));
                self.ivars().ime_state.set(ImeState::Committed);
            }
        }

        // Basically, we're sent this message whenever a keyboard event that doesn't generate a "human
        // readable" character happens, i.e. newlines, tabs, and Ctrl+C.
        @sel(doCommandBySelector:)
        fn do_command_by_selector(&self, _command: Sel) {
            trace_scope!("doCommandBySelector:");
            // We shouldn't forward any character from just committed text, since we'll end up sending
            // it twice with some IMEs like Korean one. We'll also always send `Enter` in that case,
            // which is not desired given it was used to confirm IME input.
            if self.ivars().ime_state.get() == ImeState::Committed {
                return;
            }

            self.ivars().forward_key_to_app.set(true);

            if self.has_marked_text().into() && self.ivars().ime_state.get() == ImeState::Preedit
            {
                // Leave preedit so that we also report the key-up for this key.
                self.ivars().ime_state.set(ImeState::Ground);
            }
        }

        // ------------------------------------------------------------------
        // NSResponder. Every row below takes the `NSEvent` AppKit is
        // delivering; the declared argument is a bare `id` (`@`, which is what
        // `NSResponder` itself registers) and each body borrows it as objc2's
        // `&NSEvent` at the top.
        // ------------------------------------------------------------------

        @sel(keyDown:)
        fn key_down(&self, event: Id) {
            trace_scope!("keyDown:");
            // LOCAL PATCH (aterm): `self.as_event(event)`, which narrowed the
            // declared `@` to objc2's `&NSEvent`, is gone — the trampoline
            // already holds what AppKit delivered.
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

            // Get the characters from the event.
            let old_ime_state = self.ivars().ime_state.get();
            self.ivars().forward_key_to_app.set(false);
            let event = replace_event(event, self.option_as_alt());

            // The `interpretKeyEvents` function might call
            // `setMarkedText`, `insertText`, and `doCommandBySelector`.
            // It's important that we call this before queuing the KeyboardInput, because
            // we must send the `KeyboardInput` event during IME if it triggered
            // `doCommandBySelector`. (doCommandBySelector means that the keyboard input
            // is not handled by IME and should be handled by the application)
            if self.ivars().ime_allowed.get() {
                // SAFETY: `+arrayWithObjects:count:` is `@32@0:8r^@16Q24` (the
                // same constructor `NSArray::from_slice` used, +0 autoreleased)
                // and dereferences exactly one live object pointer here;
                // `-interpretKeyEvents:` is `v24@0:8@16`. The pool is explicit:
                // `keyDown:` is also reached from the driver.
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

                // If the text was committed we must treat the next keyboard event as IME related.
                if self.ivars().ime_state.get() == ImeState::Committed {
                    // Remove any marked text, so normal input can continue.
                    *self.ivars().marked_text.borrow_mut() = new_marked_text();
                }
            }

            self.update_modifiers(event.id(), false);

            let had_ime_input = match self.ivars().ime_state.get() {
                ImeState::Committed => {
                    // Allow normal input after the commit.
                    self.ivars().ime_state.set(ImeState::Ground);
                    true
                }
                ImeState::Preedit => true,
                // `key_down` could result in preedit clear, so compare old and current state.
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
            let event = replace_event(event, self.option_as_alt());
            self.update_modifiers(event.id(), false);

            // We want to send keyboard input when we are currently in the ground state.
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

        // Allows us to receive Cmd-. (the shortcut for closing a dialog)
        // https://bugs.eclipse.org/bugs/show_bug.cgi?id=300620#c6
        @sel(cancelOperation:)
        fn cancel_operation(&self, _sender: Id) {
            // LOCAL PATCH (aterm): the marker was never AppKit's requirement,
            // it was objc2's (`NSApplication` is `MainThreadOnly`), so the
            // result is DISCARDED as `window_delegate.rs`'s five
            // `+sharedApplication` sites discard theirs. Still asked: it is the
            // only remaining check that a responder row is on the main thread.
            let _mtm = self.mtm();
            trace_scope!("cancelOperation:");

            // SAFETY: `+sharedApplication` `@16@0:8` (process-lifetime
            // singleton); `-currentEvent` `@16@0:8`, +0 and valid for this
            // callback; `-isARepeat` `B16@0:8`.
            let event = unsafe {
                let app = send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
                let event = send_id(app, sel!(currentEvent));
                assert!(!event.is_null(), "could not find current event");
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

        // In the past (?), `mouseMoved:` events were not generated when the
        // user hovered over a window from a separate window, and as such the
        // application might not know the location of the mouse in the event.
        //
        // To fix this, we emit `mouse_motion` inside of mouse click, mouse
        // scroll, magnify and other gesture event handlers, to ensure that
        // the application's state of where the mouse click was located is up
        // to date.
        //
        // See https://github.com/rust-windowing/winit/pull/1490 for history.

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

        // No tracing on these because that would be overly verbose

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

            // SAFETY: `-scrollingDeltaX`/`-scrollingDeltaY` are `d16@0:8` and
            // `-hasPreciseScrollingDeltas` is `B16@0:8`, all on `NSEvent`.
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

            // The "momentum phase," if any, has higher priority than touch phase (the two should
            // be mutually exclusive anyhow, which is why the API is rather incoherent). If no momentum
            // phase is recorded (or rather, the started/ended cases of the momentum phase) then we
            // report the touch phase.
            // LOCAL PATCH (aterm): `NSEventPhase` was an `objc2-app-kit`
            // bitflags type; these are the header's values, static-asserted on
            // both arches. It is a BITMASK — `NSEventPhaseEnded` is `1 << 3`,
            // not `3`.
            //
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

            self.ivars().app_delegate.maybe_queue_device_event(DeviceEvent::MouseWheel { delta });
            self.queue_event(WindowEvent::MouseWheel {
                device_id: DEVICE_ID,
                delta,
                phase,
            });
        }

        @sel(magnifyWithEvent:)
        fn magnify_with_event(&self, event: Id) {
            trace_scope!("magnifyWithEvent:");
            self.mouse_motion(event);

            // SAFETY: `event` is the `NSEvent` AppKit delivered to this row.
            let Some(phase) = (unsafe { gesture_phase(event) }) else { return };

            // SAFETY: `-magnification` is `d16@0:8` on `NSEvent` — a `double`,
            // unlike `-rotation` in the row below. See `send_f32`.
            self.queue_event(WindowEvent::PinchGesture {
                device_id: DEVICE_ID,
                delta: unsafe { send_f64(event, sel!(magnification)) },
                phase,
            });
        }

        @sel(smartMagnifyWithEvent:)
        fn smart_magnify_with_event(&self, event: Id) {
            trace_scope!("smartMagnifyWithEvent:");

            self.mouse_motion(event);

            self.queue_event(WindowEvent::DoubleTapGesture {
                device_id: DEVICE_ID,
            });
        }

        @sel(rotateWithEvent:)
        fn rotate_with_event(&self, event: Id) {
            trace_scope!("rotateWithEvent:");
            self.mouse_motion(event);

            // SAFETY: `event` is the `NSEvent` AppKit delivered to this row.
            let Some(phase) = (unsafe { gesture_phase(event) }) else { return };

            // SAFETY: `-rotation` is `f16@0:8` — a C `float`, NOT a `CGFloat`,
            // which is why `send_f32` exists. `-magnification` one row up is a
            // `double`; the `d` prototype here returns a denormal built from
            // stale high bits, not a wrong angle.
            self.queue_event(WindowEvent::RotationGesture {
                device_id: DEVICE_ID,
                delta: unsafe { send_f32(event, sel!(rotation)) },
                phase,
            });
        }

        @sel(pressureChangeWithEvent:)
        fn pressure_change_with_event(&self, event: Id) {
            trace_scope!("pressureChangeWithEvent:");
            // SAFETY: `-pressure` is `f16@0:8` — a C `float`, like `-rotation`.
            // `-stage` is `q16@0:8`.
            self.queue_event(WindowEvent::TouchpadPressure {
                device_id: DEVICE_ID,
                pressure: unsafe { send_f32(event, sel!(pressure)) },
                stage: unsafe { send_isize(event, sel!(stage)) } as i64,
            });
        }

        // Allows us to receive Ctrl-Tab and Ctrl-Esc.
        // Note that this *doesn't* help with any missing Cmd inputs.
        // https://github.com/chromium/chromium/blob/a86a8a6bcfa438fa3ac2eba6f02b3ad1f8e0756f/ui/views/cocoa/bridged_content_view.mm#L816
        @sel(_wantsKeyDownForEvent:)
        fn wants_key_down_for_event(&self, _event: Id) -> Bool {
            trace_scope!("_wantsKeyDownForEvent:");
            Bool::YES
        }

        @sel(acceptsFirstMouse:)
        fn accepts_first_mouse(&self, _event: Id) -> Bool {
            trace_scope!("acceptsFirstMouse:");
            Bool::new(self.ivars().accepts_first_mouse)
        }
    }
}

impl WinitView {
    pub(super) fn new(
        app_delegate: &ApplicationDelegate,
        window: &WinitWindow,
        accepts_first_mouse: bool,
        option_as_alt: OptionAsAlt,
    ) -> aterm_objc::Retained<Self> {
        // LOCAL PATCH (aterm): W8 re-derived this marker from
        // `window.ns_window()` and crossed back through `seam::witness`; the
        // witness is asked for directly now, which is what `seam::witness` did
        // anyway after its `expect`. That takes two of `ns_window()`'s three
        // callers (this and the `WeakId` below), leaving one — `window.rs`'s
        // `Drop`.
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
            // LOCAL PATCH (aterm): explicit — `Obj` has no `Default`.
            marked_text: RefCell::new(new_marked_text()),
            accepts_first_mouse,
            // LOCAL PATCH (aterm): `WeakId::new(window.ns_window())` needed the
            // binding only for `WeakId`'s bounds; the reference was always to
            // this instance's address.
            //
            // SAFETY: `as_id()` is a live non-null object pointer, which is what
            // `objc_initWeak` requires. The slot lives behind `WeakObj`'s `Box`
            // and so keeps its address across every move of `ViewState` into
            // the ivars — the property the `weak` module exists for.
            _ns_window: unsafe { WeakObj::new(window.as_id()) },
            option_as_alt: Cell::new(option_as_alt),
        })
        // LOCAL PATCH (aterm): `alloc_init` is `+alloc`, store the ivars,
        // `-init` — the same three steps and the same ORDER as the objc2 pair
        // it replaces (`mtm.alloc().set_ivars(..)` then
        // `msg_send_id![super(this), init]`), against a `MainThread` witness.
        //
        // `-init` IS the right initializer even though this is an `NSView`.
        // `NSView`'s designated initializer is `-initWithFrame:`, and
        // `alloc_ivars` exists for classes that must go through one — but the
        // code being replaced sent plain `init` too, `-[NSView init]` is
        // Foundation's own `-initWithFrame:NSZeroRect` funnel, and the frame is
        // set moments later by `setContentView:`. Sending `initWithFrame:` here
        // would be a BEHAVIOUR CHANGE smuggled into a mechanical port.
        //
        // The one difference from `msg_send_id![super(this), init]` is that the
        // send goes to `self`, not to `super`. It reaches the same IMP:
        // `WinitView` declares no `-init`, so `[self init]` and `[super init]`
        // both resolve into `NSView`. The audit's live method table is what
        // makes that checkable rather than argued.
        .expect("couldn't create `WinitView`");

        // LOCAL PATCH (aterm): observer and object were `&this`, which objc2's
        // inheritance made an `&AnyObject`; both are the raw `id` now.
        //
        // SAFETY: `-setPostsFrameChangedNotifications:` `v20@0:8B16`;
        // `+defaultCenter` `@16@0:8` (process-lifetime singleton);
        // `-addObserver:selector:name:object:` `v48@0:8@16:24@32@40`, which
        // registers the observer UNRETAINED — deregistration stays
        // `NSNotificationCenter`'s zeroing-weak behaviour, as through the
        // binding. `NS_VIEW_FRAME_DID_CHANGE_NOTIFICATION` is a live global.
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

    // ---------------------------------------------------------------------
    // LOCAL PATCH (aterm): THE FIVE CROSSINGS W8 WROTE HERE ARE GONE.
    // `ns_view`, `as_responder`, `as_any`, `as_event` and `retained` each
    // existed to hand an objc2 BINDING TYPE to an objc2 binding METHOD; with
    // the bindings ported there is nothing on the far side, and `self.as_id()`
    // is the receiver for all forty sends. `as_event` is the clearest: it
    // narrowed the declared `@` back to `&NSEvent`, and the trampoline had the
    // right thing in hand the whole time. `mtm` survives because it is a CHECK,
    // not a crossing.
    // ---------------------------------------------------------------------

    /// The main-thread witness objc2's `mutability::MainThreadOnly` used to
    /// derive from the receiver's type.
    ///
    /// It is a real question, not `mint`: AppKit delivers every one of this
    /// class's rows on the main thread, so the check is expected to be free of
    /// failures rather than free of cost, and a view method reached off it is a
    /// bug this names at the frame that noticed.
    #[track_caller]
    fn mtm(&self) -> aterm_objc::MainThread {
        aterm_objc::MainThread::new()
            .expect("a WinitView method ran off the main thread; AppKit delivers on it")
    }

    /// `-firstResponder` is this view, so move the key view on.
    ///
    /// LOCAL PATCH (aterm): `insertTab:`/`insertBackTab:` were nine identical
    /// lines apart from the selector. The identity test was
    /// `&*first_responder == self.as_responder()`, which objc2 forwarded to
    /// `-isEqual:`; it is the same send here, not a pointer comparison.
    fn select_key_view(&self, which: Sel) {
        // SAFETY: `-firstResponder` is `@16@0:8` on `NSWindow` and +0 borrowed;
        // `-isEqual:` is `B24@0:8@16`; `-selectNextKeyView:` and
        // `-selectPreviousKeyView:` are both `v24@0:8@16` on `NSWindow`.
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

    /// The marked text's length in UTF-16 code units — `-length` on
    /// `NSAttributedString`, which is what `markedRange` reports.
    fn marked_text_length(&self) -> usize {
        // SAFETY: the ivar holds a live `NSMutableAttributedString`; `-length`
        // is `Q16@0:8` on `NSAttributedString`.
        unsafe { send_usize(self.ivars().marked_text.borrow().id(), sel!(length)) }
    }

    /// Drop the old tracking rect and install one over the current frame,
    /// answering that frame.
    ///
    /// LOCAL PATCH (aterm): `viewDidMoveToWindow` and `frameDidChange:` carried
    /// nine byte-identical lines each. Both had to grow a `ns_view()` crossing,
    /// which would have made it eleven twice; it is one function once, and the
    /// `frameDidChange:` half now uses the frame it already computed rather
    /// than asking `-frame` a second time.
    fn reset_tracking_rect(&self) -> CGRect {
        // SAFETY: `-removeTrackingRect:` `v24@0:8q16`, `-frame`
        // `{CGRect=…}16@0:8`, `-addTrackingRect:owner:userData:assumeInside:`
        // `q68@0:8{CGRect=…}16@48^v56B64`. The owner is UNRETAINED and is this
        // view, which outlives the rect because it is what removes it;
        // `userData:` is opaque to AppKit and null, as through the binding.
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

    /// LOCAL PATCH (aterm): the return is a bare [`Obj`] rather than
    /// `Retained<NSWindow>`, for the reason on `ViewState::_ns_window`. Every
    /// caller wanted an AppKit method; the one that wanted the id gets it from
    /// [`WinitWindow::id_of`].
    fn window(&self) -> Obj {
        // TODO: Simply use `window` property on `NSView`.
        // That only returns a window _after_ the view has been attached though!
        // (which is incompatible with `frameDidChange:`)
        //
        // unsafe { msg_send_id![self, window] }
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
        // SAFETY: `-inputContext` and `-selectedKeyboardInputSource` are both
        // `@16@0:8` and +0. The pool is explicit: the string is autoreleased
        // and this is reached from `keyDown:` on every keystroke.
        autoreleasepool(|_| unsafe {
            let input_context = send_id(self.as_id(), sel!(inputContext));
            assert!(!input_context.is_null(), "input context");
            let source = send_id(input_context, sel!(selectedKeyboardInputSource));
            // `nsstring_to_rust` answers `String::new()` for nil, which is what
            // `.map(..).unwrap_or_default()` did.
            seam::nsstring_to_rust(source)
        })
    }

    /// LOCAL PATCH (aterm): `Retained<NSCursor>` -> `Obj` both ways. These are
    /// `window_delegate.rs`'s third and fourth undocumented consumptions of a
    /// ported neighbour — see `set_cursor` there.
    pub(super) fn cursor_icon(&self) -> Obj {
        self.ivars().cursor_state.borrow().cursor.clone_retained()
    }

    pub(super) fn set_cursor_icon(&self, icon: Obj) {
        let mut cursor_state = self.ivars().cursor_state.borrow_mut();
        cursor_state.cursor = icon;
    }

    /// Set whether the cursor should be visible or not.
    ///
    /// Returns whether the state changed.
    pub(super) fn set_cursor_visible(&self, visible: bool) -> bool {
        let mut cursor_state = self.ivars().cursor_state.borrow_mut();
        if visible != cursor_state.visible {
            cursor_state.visible = visible;
            true
        } else {
            false
        }
    }

    pub(super) fn set_ime_allowed(&self, ime_allowed: bool) {
        if self.ivars().ime_allowed.get() == ime_allowed {
            return;
        }
        self.ivars().ime_allowed.set(ime_allowed);
        if self.ivars().ime_allowed.get() {
            return;
        }

        // Clear markedText
        *self.ivars().marked_text.borrow_mut() = new_marked_text();

        if self.ivars().ime_state.get() != ImeState::Disabled {
            self.ivars().ime_state.set(ImeState::Disabled);
            self.queue_event(WindowEvent::Ime(Ime::Disabled));
        }
    }

    pub(super) fn set_ime_cursor_area(&self, position: CGPoint, size: CGSize) {
        self.ivars().ime_position.set(position);
        self.ivars().ime_size.set(size);
        // SAFETY: `-inputContext` `@16@0:8` (+0) and
        // `-invalidateCharacterCoordinates` `v16@0:8`. Invalidating is what
        // makes AppKit re-ask `firstRectForCharacterRange:` for the two ivars.
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

    pub(super) fn set_option_as_alt(&self, value: OptionAsAlt) {
        self.ivars().option_as_alt.set(value)
    }

    pub(super) fn option_as_alt(&self) -> OptionAsAlt {
        self.ivars().option_as_alt.get()
    }

    /// Update modifiers if `event` has something different
    fn update_modifiers(&self, ns_event: Id, is_flags_changed_event: bool) {
        use ElementState::{Pressed, Released};

        // LOCAL PATCH (aterm): `seam::id_of` re-badged objc2's `&NSEvent` as
        // the raw `id` `event.rs` already took. The parameter IS that `id` now,
        // so all SEVEN of its call sites — every caller it had — are gone, and
        // the function is deleted from the seam with this commit.
        let current_modifiers = event_mods(ns_event);
        let prev_modifiers = self.ivars().modifiers.get();
        self.ivars().modifiers.set(current_modifiers);

        // This function was called form the flagsChanged event, which is triggered
        // when the user presses/releases a modifier even if the same kind of modifier
        // has already been pressed.
        //
        // When flags changed event has key code of zero it means that event doesn't carry any key
        // event, thus we can't generate regular presses based on that. The `ModifiersChanged`
        // later will work though, since the flags are attached to the event and contain valid
        // information.
        'send_event: {
            // LOCAL PATCH (aterm): `-keyCode` IS ONLY VALID ON A KEY EVENT.
            // AppKit asserts on it for any other type (`NSEvent.m`), and this
            // function is called with a MOUSE event on every pointer move —
            // `mouse_motion` ends in `update_modifiers(event, false)`. Reading
            // it unconditionally therefore raised an NSException inside a Rust
            // method, which crossed the `declare_class!` trampoline as a
            // FOREIGN exception and aborted the process: v0.72.0 crashed with
            // `*** Assertion failure ... NSEvent.m:3220` under
            // `_routeMouseMovedEvent` after a few minutes of ordinary mouse
            // movement.
            //
            // The value is USED only inside the `is_flags_changed_event` arm
            // below, so gating the READ on the same flag is semantics-
            // preserving and removes the invalid send entirely.
            //
            // SAFETY: `-keyCode` is `S16@0:8` on `NSEvent` — an UNSIGNED
            // short; see `send_u16` — and it is now only ever sent to the
            // flags-changed event, which is a key event.
            let scancode = if is_flags_changed_event {
                unsafe { send_u16(ns_event, sel!(keyCode)) }
            } else {
                0
            };
            if is_flags_changed_event && scancode != 0 {
                let physical_key = scancode_to_physicalkey(scancode as u32);

                let logical_key = code_to_key(physical_key, scancode);
                // Ignore processing of unknown modifiers because we can't determine whether
                // it was pressed or release reliably.
                //
                // Furthermore, sometimes normal keys are reported inside flagsChanged:, such as
                // when holding Caps Lock while pressing another key, see:
                // https://github.com/alacritty/alacritty/issues/8268
                let Some(event_modifier) = key_to_modifier(&logical_key) else {
                    break 'send_event;
                };

                let mut event = KeyEvent {
                    location: code_to_location(physical_key),
                    logical_key: logical_key.clone(),
                    physical_key,
                    repeat: false,
                    // We'll correct this later.
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

                // There is no API for getting whether the button was pressed or released
                // during this event. For this reason we have to do a bit of magic below
                // to come up with a good guess whether this key was pressed or released.
                // (This is not trivial because there are multiple buttons that may affect
                // the same modifier)
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
                        // Here we hit a contradiction:
                        // The modifier state was "changed" to active,
                        // yet the only pressed modifier key was the one that we
                        // just got a change event for.
                        // This seemingly means that the only pressed modifier is now released,
                        // but at the same time the modifier became active.
                        //
                        // But this scenario is possible if we released modifiers
                        // while the application was not in focus. (Because we don't
                        // get informed of modifier key events while the application
                        // is not focused)

                        // In this case we prioritize the information
                        // about the current modifier state which means
                        // that the button was pressed.
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
        // SAFETY: `-locationInWindow` `{CGPoint=dd}16@0:8`;
        // `-convertPoint:fromView:` `{CGPoint=dd}40@0:8{CGPoint=dd}16@32`;
        // `-frame` `{CGRect=…}16@0:8`. A nil `fromView:` means the window's
        // base coordinates, which is what `None` meant through the binding.
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
                // Point is outside of the client area (view) and no buttons are pressed
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

// ---------------------------------------------------------------------------
// LOCAL PATCH (aterm): the file-level helpers the port needed, and only these.
// Each replaces an objc2 method that READ AS A PROPERTY and was a message send
// with a convention. The two length helpers are the pair that matters: objc2's
// `len()` and `len_utf16()` differ by six characters in Rust and by an entire
// encoding at the runtime, and this file calls both on one string three lines
// apart.
// ---------------------------------------------------------------------------

/// An `NSString`'s length in UTF-8 BYTES — objc2's `NSString::len()`, i.e.
/// `-lengthOfBytesUsingEncoding:NSUTF8StringEncoding`, which is the unit
/// winit's `Ime::Preedit` cursor range is in. Answers 0 for a string the
/// encoding cannot represent, as objc2's did.
///
/// # Safety
/// `s` must be a live `NSString`.
unsafe fn utf8_len(s: Id) -> usize {
    // SAFETY: the caller pins `s` as a live `NSString`;
    // `-lengthOfBytesUsingEncoding:` is `Q24@0:8Q16`.
    unsafe { send_usize_usize(s, sel!(lengthOfBytesUsingEncoding:), consts::NS_UTF8_STRING_ENCODING) }
}

/// An `NSString`'s length in UTF-16 CODE UNITS — objc2's `len_utf16()`, plain
/// `-length`: the unit an input method's `selectedRange` uses and the unit
/// `-substringToIndex:` cuts at.
///
/// # Safety
/// `s` must be a live `NSString`.
unsafe fn utf16_len(s: Id) -> usize {
    // SAFETY: the caller pins `s` as a live `NSString`; `-length` is `Q16@0:8`.
    unsafe { send_usize(s, sel!(length)) }
}

/// An empty `NSMutableAttributedString`, +1 — `objc2`'s
/// `NSMutableAttributedString::new()`.
fn new_marked_text() -> Obj {
    // SAFETY: `+new` is `@16@0:8` and is in the `new` family, so the result is
    // +1 and is adopted rather than retained.
    unsafe { Obj::from_owned(send_id(class(c"NSMutableAttributedString").as_id(), sel!(new))) }
        .expect("+[NSMutableAttributedString new] to answer a string")
}

/// The `TouchPhase` a magnify/rotate gesture row reports, or `None` for a phase
/// the fork ignores.
///
/// LOCAL PATCH (aterm): `magnifyWithEvent:` and `rotateWithEvent:` carried the
/// same five-arm match; the `_ => return` both had is the `None` the callers
/// `let else` on.
///
/// # Safety
/// `event` must be the live `NSEvent` a responder row was handed.
unsafe fn gesture_phase(event: Id) -> Option<TouchPhase> {
    // SAFETY: `-phase` is `Q16@0:8` on `NSEvent`.
    match unsafe { send_usize(event, sel!(phase)) } {
        consts::NS_EVENT_PHASE_BEGAN => Some(TouchPhase::Started),
        consts::NS_EVENT_PHASE_CHANGED => Some(TouchPhase::Moved),
        consts::NS_EVENT_PHASE_CANCELLED => Some(TouchPhase::Cancelled),
        consts::NS_EVENT_PHASE_ENDED => Some(TouchPhase::Ended),
        _ => None,
    }
}

/// Get the mouse button from the NSEvent.
fn mouse_button(event: Id) -> MouseButton {
    // The buttonNumber property only makes sense for the mouse events:
    // NSLeftMouse.../NSRightMouse.../NSOtherMouse...
    // For the other events, it's always set to 0.
    // MacOS only defines the left, right and middle buttons, 3..=31 are left as generic buttons,
    // but 3 and 4 are very commonly used as Back and Forward by hardware vendors and applications.
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

// NOTE: to get option as alt working we need to rewrite events
// we're getting from the operating system, which makes it
// impossible to provide such events as extra in `KeyEvent`.
fn replace_event(event: Id, option_as_alt: OptionAsAlt) -> Obj {
    let ev_mods = event_mods(event).state;
    let ignore_alt_characters = match option_as_alt {
        OptionAsAlt::OnlyLeft if lalt_pressed(event) => true,
        OptionAsAlt::OnlyRight if ralt_pressed(event) => true,
        OptionAsAlt::Both if ev_mods.alt_key() => true,
        _ => false,
    } && !ev_mods.control_key()
        && !ev_mods.super_key();

    if ignore_alt_characters {
        // SAFETY: read from the live runtime — `-charactersIgnoringModifiers`
        // `@16@0:8` (+0), `-type`/`-modifierFlags` `Q16@0:8`,
        // `-locationInWindow` `{CGPoint=dd}16@0:8`, `-timestamp` `d16@0:8`,
        // `-windowNumber` `q16@0:8`, `-isARepeat` `B16@0:8`, `-keyCode`
        // `S16@0:8`; the constructor is
        // `@96@0:8Q16{CGPoint=dd}24Q40d48q56@64@72@80B88S92` and is +0
        // AUTORELEASED (not `new`/`alloc`/`copy`), so its result is retained,
        // as objc2's binding did. The pool wraps both: this runs on every
        // keystroke and is driven directly by `objc_ime_drive`.
        autoreleasepool(|_| unsafe {
            let ns_chars = send_id(event, sel!(charactersIgnoringModifiers));
            assert!(!ns_chars.is_null(), "expected characters to be non-null");

            let replaced = send_id_usize_point_usize_f64_isize_id_id_id_bool_u16(
                class(c"NSEvent").as_id(),
                sel!(
                    keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                ),
                send_usize(event, sel!(type)),
                send_point(event, sel!(locationInWindow)),
                send_usize(event, sel!(modifierFlags)),
                send_f64(event, sel!(timestamp)),
                send_isize(event, sel!(windowNumber)),
                Id::NIL,
                ns_chars,
                ns_chars,
                send_bool(event, sel!(isARepeat)),
                send_u16(event, sel!(keyCode)),
            );
            Obj::retain(replaced).expect("+[NSEvent keyEventWithType:…] to answer an event")
        })
    } else {
        // SAFETY: `-copy` is `@16@0:8` and is in the `copy` family, so the
        // result is +1 and is ADOPTED rather than retained.
        unsafe { Obj::from_owned(send_id(event, sel!(copy))) }
            .expect("-[NSEvent copy] to answer an event")
    }
}
