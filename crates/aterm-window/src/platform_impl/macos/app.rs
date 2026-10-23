// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/app.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): upstream's `#[cfg(test)] mod tests` is removed;
// `aterm-objc/tests/swizzle.rs` covers the swizzle.

#![allow(clippy::unnecessary_cast)]

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Weak;

// LOCAL PATCH (aterm): Objective-C goes through `aterm_objc` (typed `send` prototypes,
// `Obj`/`Retained` handles, `MainThread` witnesses, the seam's `consts`) where upstream used objc2
// and its framework bindings.
use aterm_objc::send::{send_id, send_usize, send_v_id};
use aterm_objc::swizzle::SwizzleSite;
use aterm_objc::{abort_on_unwind, class, class_of, sel, Id, MainThread, Sel};

use super::aterm_objc_seam::consts::{NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_TYPE_KEY_UP};
use super::event_loop::{stop_app_on_panic, PanicInfo};

/// `-[NSApplication sendEvent:]`'s prototype, as the runtime enters it.
///
/// LOCAL PATCH (aterm): `unsafe extern "C-unwind" fn(Id, Sel, Id)` where upstream had `extern "C"
/// fn(&NSApplication, Sel, &NSEvent)`: AppKit's routing behind the original can raise, and a `"C"`
/// frame has no landing pad.
type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);

/// The site the swizzle publishes the displaced implementation into.
///
/// LOCAL PATCH (aterm): a `SwizzleSite` where upstream had a `static mut` stored after the swap;
/// `SwizzleSite::install` publishes the original before it.
static SEND_EVENT: SwizzleSite<SendEvent> = SwizzleSite::new();

thread_local! {
    // LOCAL PATCH (aterm): the event loop's panic slot, installed by
    // [`set_send_event_panic_info`] from `AppKitEventLoop::new` right after the
    // override. `sendEvent:` can reach the application's handlers directly
    // (the original's routing → a `WinitView` row → `maybe_queue_event` →
    // `handle_event` when no other event is being handled), so a Rust panic in
    // there must take the same road every other handler panic takes on this
    // backend:
    // `stop_app_on_panic` stores it here, stops the app, and `run_on_demand`
    // resumes the unwind on the main thread once `-[NSApplication run]`
    // returns. Thread-local rather than a `static mut`: the slot is written
    // and read on the main thread only, and a thread-local says so without an
    // `unsafe` block or a `static_mut_refs` allow.
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
///
/// This is an IMP: it is entered by the Objective-C runtime with the receiver
/// in the first argument position and the selector in the second, which is what
/// [`SwizzleSite::install`]'s encoding check verifies for the row it replaces.
///
/// LOCAL PATCH (aterm): the body runs under [`aterm_objc::contain`], so an NSException from the
/// original `sendEvent:` and the routing behind it is reported here and the run loop goes on.
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

    // LOCAL PATCH (aterm): upstream also queued every mouse event's deltas and buttons as
    // `DeviceEvent`s here; nothing in aterm handles one, so the override only forwards the Cmd
    // key-up above.
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
/// TWICE, for two different reasons:
/// - `SEND_EVENT.install` answers an [`aterm_objc::SwizzleError`]: the swizzle is refused (no
///   class in `app`'s chain implements `-sendEvent:`; its row's encoding is missing or differs
///   from `SendEvent`'s; this site is installed on another row, or another thread claimed it
///   first; a third party swizzled the row since an earlier install; or the row holds
///   `send_event` with no original to chain to), or, on `Raced`, it landed with a third party
///   inside its window;
/// - the row's owner is not `NSApplication` (the `assert_eq!`): `app`'s class, or a class
///   between it and `NSApplication`, overrides `-sendEvent:`, so the swizzle replaced that
///   class's method and not `NSApplication`'s.
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
