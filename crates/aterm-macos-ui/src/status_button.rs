// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A menu-bar status item's button that acts on a left click and opens its menu on a right
//! click or a Control-click: the AppKit half of `aterm_types::status_click`.
//!
//! Such an item has no `-menu` (AppKit would open that on every press). Its button sends one
//! action on both releases ([`wire_clicks`]); the handler reads the app's current event
//! ([`current_event`]), classifies it, and for a menu click pops the menu up below the button
//! itself ([`pop_up`]). [`add_show_menu_action`] gives VoiceOver, which cannot right-click,
//! the menu; [`template_image`] is the button's face. The mug (`aterm acaffeine`) and aterm's
//! own `❯` item both stand on this.

use aterm_objc::send::{
    alloc, send_bool, send_f64, send_id, send_id_cptr_usize, send_id_id, send_id_id_id_sel,
    send_id_size, send_isize, send_isize_usize, send_rect, send_usize, send_v_bool, send_v_id,
    send_v_sel, send_v_size,
};
use aterm_objc::{Bool, CGPoint, CGRect, CGSize, Id, MainThread, Obj, Sel, autoreleasepool, sel};
use aterm_types::status_click::EventKey;

use crate::appkit::{application, class};

/// `NSEventMaskLeftMouseUp | NSEventMaskRightMouseUp`.
const CLICK_MASK: usize = (1 << 2) | (1 << 4);
/// The gap between the button and the menu below it, in points.
const POPUP_GAP: f64 = 3.0;
/// The name of the button's custom accessibility action, which opens the menu.
pub const SHOW_MENU: &str = "Show Menu";

/// `-[NSMenu popUpMenuPositioningItem:atLocation:inView:]`, `B48@0:8@16{CGPoint=dd}24@40`.
type PopUpFn = unsafe extern "C-unwind" fn(Id, Sel, Id, CGPoint, Id) -> Bool;

/// Whether an event of type `kind` is a mouse button's press or release: the events that
/// answer `-clickCount` (any other raises).
const fn of_a_mouse_button(kind: usize) -> bool {
    // NSEventTypeLeftMouseDown, LeftMouseUp, RightMouseDown, RightMouseUp, OtherMouseDown and
    // OtherMouseUp.
    matches!(kind, 1..=4 | 25 | 26)
}

/// The app's current event as `aterm_types::status_click` reads it; `None` where there is no
/// app yet or it holds no event.
#[must_use]
pub fn current_event(main: MainThread) -> Option<EventKey> {
    let app = application(main);
    if app.is_null() {
        return None;
    }
    autoreleasepool(|_| {
        // SAFETY: on the main thread (`main`); `-currentEvent` is `@16@0:8` on the live app and
        // answers the event (+0) or nil; on it `-type` and `-modifierFlags` are `Q16@0:8`,
        // `-timestamp` `d16@0:8`, and `-clickCount` `q16@0:8`, which only a mouse button's
        // event answers and is asked of no other.
        unsafe {
            let event = send_id(app, sel!(currentEvent));
            (!event.is_null()).then(|| {
                let kind = send_usize(event, sel!(type));
                EventKey {
                    kind,
                    flags: send_usize(event, sel!(modifierFlags)),
                    at: send_f64(event, sel!(timestamp)),
                    clicks: if of_a_mouse_button(kind) {
                        send_isize(event, sel!(clickCount))
                    } else {
                        0
                    },
                }
            })
        }
    })
}

/// Make `button` send `action` to `target` when the left or the right mouse button is released
/// over it.
///
/// # Safety
/// On the main thread; `button` is a live `NSStatusBarButton`, and `target` a live object that
/// answers `action` as `v@:@` and outlives the button (AppKit holds it weakly).
pub unsafe fn wire_clicks(button: Id, target: Id, action: Sel) {
    // SAFETY: the caller's contract; `-setTarget:` is `v24@0:8@16`, `-setAction:` `v24@0:8:16`
    // and `-sendActionOn:` `q24@0:8Q16`.
    unsafe {
        send_v_id(button, sel!(setTarget:), target);
        send_v_sel(button, sel!(setAction:), action);
        let _ = send_isize_usize(button, sel!(sendActionOn:), CLICK_MASK);
    }
}

/// Give `button` the custom accessibility action [`SHOW_MENU`], which sends `show` to `target`.
///
/// # Safety
/// On the main thread; `button` is a live `NSStatusBarButton`, and `target` a live object that
/// answers `show` as `B@:@` and outlives the button (the action holds it weakly).
pub unsafe fn add_show_menu_action(button: Id, target: Id, show: Sel) {
    let Some(name) = aterm_objc::ns_string(SHOW_MENU) else {
        return;
    };
    // SAFETY: the caller's contract. `-initWithName:target:selector:` is `@40@0:8@16@24:32` and
    // consumes the +alloc; `+arrayWithObject:` is `@24@0:8@16` and answers +0;
    // `-setAccessibilityCustomActions:` is `v24@0:8@16` and copies the array.
    unsafe {
        let Some(action) = Obj::from_owned(send_id_id_id_sel(
            alloc(class(c"NSAccessibilityCustomAction")),
            sel!(initWithName:target:selector:),
            name.id(),
            target,
            show,
        )) else {
            return;
        };
        let actions = send_id_id(
            class(c"NSArray").as_id(),
            sel!(arrayWithObject:),
            action.id(),
        );
        if !actions.is_null() {
            send_v_id(button, sel!(setAccessibilityCustomActions:), actions);
        }
    }
}

/// Where the menu's top-left corner goes for a button with `bounds`: under its bottom-left
/// corner, in its own coordinates.
#[must_use]
pub fn popup_point(bounds: CGRect, flipped: bool) -> CGPoint {
    let y = if flipped {
        bounds.origin.y + bounds.size.height + POPUP_GAP
    } else {
        bounds.origin.y - POPUP_GAP
    };
    CGPoint {
        x: bounds.origin.x,
        y,
    }
}

/// Show `menu` below `button`, highlighted while it is up, and return when it closes: this
/// runs the menu's tracking loop.
///
/// # Safety
/// On the main thread; `menu` is a live `NSMenu` and `button` a live `NSStatusBarButton` in a
/// window, both retained by the caller across the call (the tracking loop runs arbitrary
/// handlers), and the caller holds no borrow a menu item's action or the menu's delegate takes.
pub unsafe fn pop_up(menu: Id, button: Id) {
    autoreleasepool(|_| {
        // SAFETY: the caller's contract. `-highlight:` is `v20@0:8B16`, `-bounds`
        // `{CGRect=…}16@0:8`, `-isFlipped` `B16@0:8`, and the pop-up `PopUpFn`'s prototype: a nil
        // positioning item puts the menu's top-left corner at the point, in the button's
        // coordinates, and it answers whether an item was chosen.
        unsafe {
            send_v_bool(button, sel!(highlight:), true);
            let at = popup_point(
                send_rect(button, sel!(bounds)),
                send_bool(button, sel!(isFlipped)),
            );
            let pop: PopUpFn = aterm_objc::checked_msg();
            let _ = pop(
                menu,
                sel!(popUpMenuPositioningItem:atLocation:inView:),
                Id::NIL,
                at,
                button,
            );
            send_v_bool(button, sel!(highlight:), false);
        }
    });
}

/// A template `NSImage` of `size` points holding one bitmap per PNG of `reps` (one per scale:
/// the same picture at 1× and 2×), described to accessibility as `description`; `None` where a
/// PNG does not decode. AppKit tints a template image for the bar it sits on: light, dark and
/// highlighted. It draws nothing and opens no WindowServer connection.
#[must_use]
pub fn template_image(reps: &[&[u8]], size: CGSize, description: &str) -> Option<Obj> {
    autoreleasepool(|_| {
        // SAFETY: `-initWithSize:` is `@32@0:8{CGSize=dd}16`, consuming the `+alloc` and
        // answering +1.
        let image = unsafe {
            Obj::from_owned(send_id_size(
                alloc(class(c"NSImage")),
                sel!(initWithSize:),
                size,
            ))
        }?;
        for png in reps {
            let rep = bitmap(png)?;
            // SAFETY: `-setSize:` (`v32@0:8{CGSize=dd}16`) makes the bitmap's pixels that many
            // points; `-addRepresentation:` (`v24@0:8@16`) retains it into the live image.
            unsafe {
                send_v_size(rep.id(), sel!(setSize:), size);
                send_v_id(image.id(), sel!(addRepresentation:), rep.id());
            }
        }
        // SAFETY: `-setTemplate:` is `v20@0:8B16` and `-setAccessibilityDescription:`
        // `v24@0:8@16`, on the live image, handed a live string it copies.
        unsafe {
            send_v_bool(image.id(), sel!(setTemplate:), true);
            if let Some(text) = aterm_objc::ns_string(description) {
                send_v_id(image.id(), sel!(setAccessibilityDescription:), text.id());
            }
        }
        Some(image)
    })
}

/// `png` decoded into an `NSBitmapImageRep`; `None` where it is empty or no image.
fn bitmap(png: &[u8]) -> Option<Obj> {
    if png.is_empty() {
        return None;
    }
    autoreleasepool(|_| {
        // SAFETY: `-initWithBytes:length:` is `@32@0:8r^v16Q24`, copying the `png.len()` bytes
        // for the call and answering +1; `+imageRepWithData:` is `@24@0:8@16`, answering +0,
        // retained before this pool pops.
        unsafe {
            let data = Obj::from_owned(send_id_cptr_usize(
                alloc(class(c"NSData")),
                sel!(initWithBytes:length:),
                png.as_ptr().cast(),
                png.len(),
            ))?;
            Obj::retain(send_id_id(
                class(c"NSBitmapImageRep").as_id(),
                sel!(imageRepWithData:),
                data.id(),
            ))
        }
    })
}

#[cfg(test)]
#[path = "status_button_tests.rs"]
mod tests;
