// Modified by the aterm project in 2026; see the repository NOTICE.
// (`override_send_event` swizzles through `aterm_objc::SwizzleSite` instead of
//  objc2's `Method::set_implementation`, and takes a raw `id` plus a
//  `MainThread` witness. The `sendEvent:` override is `extern "C-unwind"`,
//  makes its sends through `aterm_objc`, and CONTAINS an NSException raised by
//  AppKit's own routing behind it — see the note on `send_event`. The
//  `#[cfg(test)] mod tests` upstream carried is DELETED — see the note on
//  `override_send_event` for what it tested, why it could never have run in
//  this repository, and where each of its two cases is covered for real.)
#![allow(clippy::unnecessary_cast)]

use std::cell::RefCell;
use std::panic::AssertUnwindSafe;
use std::rc::Weak;

// LOCAL PATCH (aterm): objc2's `Imp`/`Sel`/`sel!` and its four `NSApplication`,
// `NSEvent`, `NSEventModifierFlags` and `NSEventType` bindings are gone. The
// event-type and modifier constants are the seam's, at the SDK's own values.
use aterm_objc::send::{send_f64, send_id, send_isize, send_usize, send_v_id};
use aterm_objc::swizzle::SwizzleSite;
use aterm_objc::{abort_on_unwind, class, class_of, sel, Id, MainThread, Sel};

use super::app_state::ApplicationDelegate;
use super::aterm_objc_seam::consts::{
    NS_EVENT_MODIFIER_FLAG_COMMAND, NS_EVENT_TYPE_KEY_UP, NS_EVENT_TYPE_LEFT_MOUSE_DOWN,
    NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED, NS_EVENT_TYPE_LEFT_MOUSE_UP, NS_EVENT_TYPE_MOUSE_MOVED,
    NS_EVENT_TYPE_OTHER_MOUSE_DOWN, NS_EVENT_TYPE_OTHER_MOUSE_DRAGGED,
    NS_EVENT_TYPE_OTHER_MOUSE_UP, NS_EVENT_TYPE_RIGHT_MOUSE_DOWN,
    NS_EVENT_TYPE_RIGHT_MOUSE_DRAGGED, NS_EVENT_TYPE_RIGHT_MOUSE_UP,
};
use super::event_loop::{stop_app_on_panic, PanicInfo};
use crate::event::{DeviceEvent, ElementState};

/// `-[NSApplication sendEvent:]`'s prototype, as the runtime enters it.
///
/// LOCAL PATCH (aterm): `unsafe extern "C-unwind" fn(Id, Sel, Id)` where
/// upstream had `extern "C" fn(&NSApplication, Sel, &NSEvent)`.
///
/// `unsafe`, because an IMP entered DIRECTLY is not a message send —
/// `objc_msgSend`'s nil-receiver short-circuit does not apply. `Id` rather
/// than the binding references, because that is what lets
/// [`aterm_objc::MethodFn`] DERIVE the encoding from this type and compare it
/// against the runtime's.
///
/// `"C-unwind"`, not `"C"`, and this is the prototype that matters most in the
/// backend: it is the one the ORIGINAL `-[NSApplication sendEvent:]` is called
/// through (`SwizzleSite::original` answers this same type), and AppKit's
/// routing behind it can raise. Declared `"C"` the call is `nounwind` to rustc,
/// the frame has no landing pad, and a raise skips this frame's destructors
/// (measured; docs/measured/2026-09-03-delivery-and-objc-exception-containment.md).
type SendEvent = unsafe extern "C-unwind" fn(Id, Sel, Id);

/// The site the swizzle publishes the displaced implementation into.
///
/// LOCAL PATCH (aterm): a `static SwizzleSite`, where upstream had
/// `static mut ORIGINAL: Cell<Option<SendEvent>>` with a NOTE saying it wanted
/// a `MainThreadBound` and could not have one "with this `objc2` version". It
/// wants neither: the site holds an `AtomicPtr`, so reading it needs no
/// `unsafe` and no `static_mut_refs` allow.
///
/// THE ORDERING IS THE POINT. Upstream stored the original AFTER the swap, so a
/// `-sendEvent:` dispatched in that window hit
/// `.expect("no existing sendEvent: handler set")`; its answer is a comment
/// ("only usable from the main thread, so we're good!") that is true of
/// AppKit's dispatch and not of a second thread calling `override_send_event`.
/// `SwizzleSite::install` publishes BEFORE the swap and claims the site with a
/// compare-exchange.
static SEND_EVENT: SwizzleSite<SendEvent> = SwizzleSite::new();

thread_local! {
    // LOCAL PATCH (aterm): the event loop's panic slot, installed by
    // [`set_send_event_panic_info`] from `EventLoop::new` right after the
    // override. `sendEvent:` can reach the application's handlers directly
    // (`maybe_dispatch_device_event` → `maybe_queue_event` → `handle_event`
    // when no other event is being handled), so a Rust panic in there must
    // take the same road every other handler panic takes on this backend:
    // `stop_app_on_panic` stores it here, stops the app, and `run_on_demand`
    // resumes the unwind on the main thread once `-[NSApplication run]`
    // returns. Thread-local rather than a `static mut`: the slot is written
    // and read on the main thread only, and a thread-local says so without an
    // `unsafe` block or a `static_mut_refs` allow.
    static PANIC_INFO: RefCell<Option<Weak<PanicInfo>>> = const { RefCell::new(None) };
}

/// Give the `sendEvent:` override the event loop's panic slot.
///
/// Until this is called (a driver that overrides `sendEvent:` on a private
/// `NSApplication`, say) a Rust panic inside the override ABORTS with a name
/// through [`aterm_objc::abort_on_unwind`] — the pre-containment behaviour,
/// which was an unnamed "panic in a function that cannot unwind" abort.
pub(crate) fn set_send_event_panic_info(mtm: MainThread, panic_info: Weak<PanicInfo>) {
    let _ = mtm;
    PANIC_INFO.with(|slot| *slot.borrow_mut() = Some(panic_info));
}

/// The `sendEvent:` override AppKit calls for every event.
///
/// LOCAL PATCH (aterm): CONTAINMENT. The body runs under
/// [`aterm_objc::contain`], so an `NSException` raised by anything this frame
/// calls — the original `sendEvent:` and the window/view routing behind it —
/// is caught HERE, reported against `sendEvent:` with its name, reason and
/// call stack, its Rust frames dropped, and the event is over; the run loop
/// continues. Without this the raise would reach `-[NSApplication run]`'s own
/// catch, which is silent, or `SIGTRAP` under `NSApplicationCrashOnExceptions`.
/// Rows the ported `WinitView` declares contain at their own trampolines first;
/// this catches what is raised by AppKit's own code between here and there.
///
/// Every send this function and `maybe_dispatch_device_event` make is an
/// `aterm_objc` send (`"C-unwind"`), and the chain to the displaced original
/// goes through [`SendEvent`] (`"C-unwind"`), so no `nounwind` frame sits on
/// the path a raise takes — the measured precondition for the frames above the
/// raise running their destructors.
///
/// A RUST PANIC is the other half, and it keeps winit's own policy: the
/// guard OUTSIDE the containment is `stop_app_on_panic` (the measured order —
/// `@catch (id)` does not swallow a Rust panic, so it crosses the containment
/// and lands here), which stores the payload in the event loop's
/// [`PanicInfo`], stops the app, and lets `run_on_demand` resume the unwind
/// on the main thread — exactly what a panic in the `CFRunLoop` observers'
/// handlers does. With no event loop installed ([`set_send_event_panic_info`]
/// not called) the panic aborts, named, through
/// [`aterm_objc::abort_on_unwind`]. Nothing unwinds out of this frame into
/// AppKit either way, which is why it may be `"C-unwind"` at all.
///
/// # Safety
///
/// This is an IMP: it is entered by the Objective-C runtime with the receiver
/// in the first argument position and the selector in the second, which is what
/// [`SwizzleSite::install`]'s encoding check verifies for the row it replaces.
unsafe extern "C-unwind" fn send_event(app: Id, cmd: Sel, event: Id) {
    let panic_info = PANIC_INFO.with(|slot| slot.borrow().clone());
    let contained = AssertUnwindSafe(move || {
        // SAFETY: AppKit delivers `-sendEvent:` on the main thread, and both
        // arguments are the live objects it dispatched with.
        let _ = aterm_objc::contain("sendEvent:", || unsafe { send_event_body(app, cmd, event) });
    });
    // The main-thread question is ASKED here, where upstream DERIVED it at
    // compile time from objc2's `MainThreadOnly` receiver type (`view.rs` and
    // `window_delegate.rs` ask the same way in their trampolines, and for the
    // same reason: the check is expected to be free of failures rather than
    // free of cost). A `None` cannot panic out of this frame: it takes the
    // abort arm below, and the body's own witness check names it.
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
    // LOCAL PATCH (aterm), W12: `MainThreadMarker::from(app)` was a COMPILE-TIME
    // derivation off objc2's `MainThreadOnly`. There is no type to derive from,
    // so the question is asked. This runs INSIDE the guards above, so a `None`
    // is a named abort, not an unwind into AppKit.
    let mtm = MainThread::new()
        .expect("-[NSApplication sendEvent:] ran off the main thread; AppKit delivers on it");

    // Normally, holding Cmd + any key never sends us a `keyUp` event for that key.
    // Overriding `sendEvent:` fixes that. (https://stackoverflow.com/a/15294196)
    // Fun fact: Firefox still has this bug! (https://bugzilla.mozilla.org/show_bug.cgi?id=1299553)
    //
    // For posterity, there are some undocumented event types
    // (https://github.com/servo/cocoa-rs/issues/155)
    // but that doesn't really matter here.
    //
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

    // Events are generally scoped to the window level, so the best way
    // to get device events is to listen for them on NSApplication.
    let delegate = ApplicationDelegate::get(mtm);
    // SAFETY: `event` is the live `NSEvent` AppKit dispatched with, and
    // `event_type` was just read off it.
    unsafe { maybe_dispatch_device_event(&delegate, event, event_type) };

    // CHAIN. `original()` is SAFE and typed — the install compared this
    // prototype against the encoding the runtime holds for the row, so the
    // address has this shape — and it can only be `None` if the trampoline is
    // reached without an install, which the publish-before-swap ordering makes
    // unreachable through `override_send_event`.
    let original = SEND_EVENT
        .original()
        .expect("no existing sendEvent: handler set");
    // SAFETY: `original` is the implementation this site displaced, entered
    // exactly as the runtime enters it: receiver, selector, then the argument
    // AppKit dispatched with. The prototype is `"C-unwind"`, so a raise inside
    // it unwinds THROUGH this frame to the containment above with this
    // frame's destructors run.
    unsafe { original(app, cmd, event) }
}

/// Override the `-sendEvent:` method on the given application's class.
///
/// The previous implementation created a subclass of `NSApplication`, however we would like to
/// give the user full control over their `NSApplication`, so we override the method here using
/// method swizzling instead.
///
/// This _should_ also allow two versions of Winit to exist in the same application.
///
/// See the following links for more info on method swizzling:
/// - <https://nshipster.com/method-swizzling/>
/// - <https://spin.atomicobject.com/method-swizzling-objective-c/>
/// - <https://web.archive.org/web/20130308110627/http://cocoadev.com/wiki/MethodSwizzling>
///
/// NOTE: This function assumes that the passed in application object is the one returned from
/// `+[NSApplication sharedApplication]`, i.e. the one and only global shared application object.
/// For testing though, we allow it to be a different object.
///
/// # LOCAL PATCH (aterm), W12
///
/// The three steps upstream wrote by hand — `class.instance_method(sel)`,
/// `mem::transmute::<SendEvent, Imp>`, `method.set_implementation` — are one
/// call to [`SwizzleSite::install`], and the two `transmute`s are gone with
/// them. Beyond the ordering fix on [`SEND_EVENT`], that buys three things:
/// the ENCODING is compared against [`SendEvent`]'s own Rust type and the
/// install refuses on a mismatch (upstream's "1. The same signature as
/// `sendEvent:`" was a claim a reader had to check by eye); the idempotent
/// re-entry is the API's rather than a function-pointer comparison under an
/// `#[allow(unpredictable_function_pointer_comparisons)]`; and the BLAST RADIUS
/// is reported — `class_getInstanceMethod` on a SUBCLASS answers the ancestor's
/// own `Method`, so this patches `NSApplication` process-wide, which was true
/// of upstream too and silently.
///
/// Upstream's `#[cfg(test)] mod tests` (`test_override`, `test_custom_class`,
/// and the containment's `the_event_constants_match_the_binding`) is DELETED,
/// not ported — `winit` is not a workspace member, so no compiler here would
/// ever see it. The first two cases run for real in `aterm-objc`'s
/// `examples/objc_swizzle_drive.rs`; the constants are checked against the SDK
/// by `aterm-objc`'s `winit_seam_constants.rs`; the reasoning is in the roadmap.
///
/// # Panics
///
/// TWICE, for two different reasons. (1) The swizzle is REFUSED — every
/// [`aterm_objc::SwizzleError`] except `Raced` means this class has no
/// `-sendEvent:` the fork compiled against, and continuing would drop key-up
/// under Cmd and every device event; `Raced` means the install DID land, with a
/// third party inside its window. (2) The row's OWNER is not `NSApplication` —
/// the `assert_eq!` below, which fires for an application class that OVERRIDES
/// `-sendEvent:` rather than inheriting it. Upstream silently swizzled that
/// subclass; this backend refuses, because its device-event stream assumes the
/// patch covers every application object.
///
/// # Safety
///
/// `app` must be a live `NSApplication` (or an instance of a subclass of one).
pub(crate) unsafe fn override_send_event(app: Id, mtm: MainThread) {
    let _ = mtm;
    // SAFETY: the caller pins `app` as a live `NSApplication` instance, so
    // `object_getClass` answers its class.
    let cls = unsafe { class_of(app) };

    // SAFETY: `send_event` is entered exactly as the runtime enters the
    // implementation it replaces (receiver, selector, one object argument —
    // checked against the row's registered encoding by `install`), and nothing
    // unwinds out of it: a Rust panic meets `stop_app_on_panic`'s
    // `catch_unwind` (or, before the event loop hands over its slot, one that
    // lands on `abort_on_unwind`), and an NSException below it is contained by
    // `aterm_objc::contain` — see `send_event`.
    let done = unsafe { SEND_EVENT.install(cls, sel!(sendEvent:), send_event) }
        .expect("NSApplication must have a swizzleable sendEvent: method");

    // THE ROW LIVES ON `NSApplication`, whatever `app`'s class is. A HARD
    // assert, not a `debug_assert`: it runs once per process launch, and it is
    // the one fact about this swizzle that is surprising and that upstream
    // never wrote down — `class_getInstanceMethod` on a SUBCLASS answers the
    // ancestor's own `Method`, so patching "the application's class" patches
    // `NSApplication` for every instance in the process. A `debug_assert` would
    // also leave `done` unused in release.
    assert_eq!(
        done.owner().addr(),
        class(c"NSApplication").addr(),
        "-sendEvent: is owned by {:?}, not NSApplication; the swizzle's blast \
         radius is not what this backend assumes",
        // SAFETY: `done.owner()` is the live class `install` read the row from.
        unsafe { aterm_objc::class_name(done.owner()) }
    );
}

/// # Safety
/// `event` must be the live `NSEvent` AppKit dispatched with, and `event_type`
/// its `-type`.
unsafe fn maybe_dispatch_device_event(delegate: &ApplicationDelegate, event: Id, event_type: usize) {
    match event_type {
        NS_EVENT_TYPE_MOUSE_MOVED
        | NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED
        | NS_EVENT_TYPE_OTHER_MOUSE_DRAGGED
        | NS_EVENT_TYPE_RIGHT_MOUSE_DRAGGED => {
            // SAFETY: `-deltaX`/`-deltaY` are `d16@0:8`, valid on the mouse
            // types matched here.
            let delta_x = unsafe { send_f64(event, sel!(deltaX)) };
            // SAFETY: as above.
            let delta_y = unsafe { send_f64(event, sel!(deltaY)) };

            if delta_x != 0.0 {
                delegate.maybe_queue_device_event(DeviceEvent::Motion { axis: 0, value: delta_x });
            }

            if delta_y != 0.0 {
                delegate.maybe_queue_device_event(DeviceEvent::Motion { axis: 1, value: delta_y })
            }

            if delta_x != 0.0 || delta_y != 0.0 {
                delegate.maybe_queue_device_event(DeviceEvent::MouseMotion {
                    delta: (delta_x, delta_y),
                });
            }
        },
        NS_EVENT_TYPE_LEFT_MOUSE_DOWN
        | NS_EVENT_TYPE_RIGHT_MOUSE_DOWN
        | NS_EVENT_TYPE_OTHER_MOUSE_DOWN => {
            delegate.maybe_queue_device_event(DeviceEvent::Button {
                // SAFETY: `-buttonNumber` is `q16@0:8`, valid on the button types.
                button: unsafe { send_isize(event, sel!(buttonNumber)) } as u32,
                state: ElementState::Pressed,
            });
        },
        NS_EVENT_TYPE_LEFT_MOUSE_UP
        | NS_EVENT_TYPE_RIGHT_MOUSE_UP
        | NS_EVENT_TYPE_OTHER_MOUSE_UP => {
            delegate.maybe_queue_device_event(DeviceEvent::Button {
                // SAFETY: as above.
                button: unsafe { send_isize(event, sel!(buttonNumber)) } as u32,
                state: ElementState::Released,
            });
        },
        _ => (),
    }
}
