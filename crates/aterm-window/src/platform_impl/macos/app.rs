// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/app.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

#![allow(clippy::unnecessary_cast)]

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Weak;

use aterm_objc::send::{send_id, send_usize, send_v_id};
use aterm_objc::swizzle::SwizzleSite;
use aterm_objc::{abort_on_unwind, class, class_of, sel, Id, MainThread, Sel};

use super::aterm_objc_seam::consts::{NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_TYPE_KEY_UP};
use super::event_loop::{stop_app_on_panic, PanicInfo};

/// `-[NSApplication sendEvent:]`'s prototype, as the runtime enters it.
type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);

/// The site the swizzle publishes the displaced implementation into.
static SEND_EVENT: SwizzleSite<SendEvent> = SwizzleSite::new();

thread_local! {
    static PANIC_INFO: RefCell<Option<Weak<PanicInfo>>> = const { RefCell::new(None) };
}

/// Give the `sendEvent:` override the event loop's panic slot.
pub(crate) fn set_send_event_panic_info(mtm: MainThread, panic_info: Weak<PanicInfo>) {
    let _ = mtm;
    PANIC_INFO.with(|slot| *slot.borrow_mut() = Some(panic_info));
}

/// The `sendEvent:` override AppKit calls for every event.
///
/// # Safety
/// `app` must be the live `NSApplication` and `event` the live `NSEvent` the
/// runtime dispatched with.
unsafe extern "C-unwind" fn send_event(app: Id, cmd: Sel, event: Id) {
    let panic_info = PANIC_INFO.with(|slot| slot.borrow().clone());
    let contained = AssertUnwindSafe(move || {
        // SAFETY: AppKit delivers `-sendEvent:` on the main thread, and both arguments are the live objects it dispatched with.
        let _ = aterm_objc::contain("sendEvent:", || unsafe { send_event_body(app, cmd, event) });
    });
    match (panic_info, MainThread::new()) {
        (Some(panic_info), Some(mtm)) => {
            let _ = stop_app_on_panic(mtm, panic_info, contained);
        },
        _ => {
            if std::panic::catch_unwind(contained).is_err() {
                abort_on_unwind("sendEvent:");
            }
        },
    }
}

/// # Safety
/// `app` must be the live `NSApplication` and `event` the live `NSEvent` the
/// runtime dispatched with.
unsafe fn send_event_body(app: Id, cmd: Sel, event: Id) {
    let mtm = MainThread::new()
        .expect("-[NSApplication sendEvent:] ran off the main thread; AppKit delivers on it");

    // SAFETY: `-type` is `Q16@0:8` and `-modifierFlags` is `Q16@0:8` on
    // `NSEvent`; `-keyWindow` is `@16@0:8` on `NSApplication` and answers nil
    // when there is none; `-sendEvent:` is `v24@0:8@16` on `NSWindow`.
    let event_type = unsafe { send_usize(event, sel!(type)) };
    let modifier_flags = unsafe { send_usize(event, sel!(modifierFlags)) };
    if event_type == NS_EVENT_TYPE_KEY_UP && modifier_flags & NS_EVENT_MODIFIER_FLAG_COMMAND != 0 {
        let key_window = unsafe { send_id(app, sel!(keyWindow)) };
        if !key_window.is_null() {
            unsafe { send_v_id(key_window, sel!(sendEvent:), event) };
        }
        return;
    }

    let _ = mtm;

    let original = SEND_EVENT.original().expect("no existing sendEvent: handler set");
    // SAFETY: `original` is the implementation this site displaced, entered
    // exactly as the runtime enters it: receiver, selector, then the argument
    // AppKit dispatched with. The prototype is `"C-unwind"`, so a raise inside
    // it unwinds THROUGH this frame to the containment above with this
    // frame's destructors run.
    unsafe { original(app, cmd, event) }
}

/// Override the `-sendEvent:` method on the given application's class.
///
/// # Panics
/// TWICE, for two different reasons.
///
/// # Safety
/// `app` must be a live `NSApplication` (or an instance of a subclass of one).
pub(crate) unsafe fn override_send_event(app: Id, mtm: MainThread) {
    let _ = mtm;
    // SAFETY: the caller pins `app` as a live `NSApplication` instance, so `object_getClass` answers its class.
    let cls = unsafe { class_of(app) };

    // SAFETY: `send_event` matches the replaced selector's registered encoding (checked by `install`) and cannot unwind.
    let done = unsafe { SEND_EVENT.install(cls, sel!(sendEvent:), send_event) }
        .expect("NSApplication must have a swizzleable sendEvent: method");

    assert_eq!(
        done.owner().addr(),
        class(c"NSApplication").addr(),
        "-sendEvent: is owned by {:?}, not NSApplication; the swizzle's blast \
         radius is not what this backend assumes",
        // SAFETY: `done.owner()` is the live class `install` read the row from.
        unsafe { aterm_objc::class_name(done.owner()) }
    );
}
