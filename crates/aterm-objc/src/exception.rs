// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Containing an `NSException` raised inside Rust code the Objective-C runtime
//! called — a declared method, a block, a dispatch trampoline, an override.
//!
//! Rust cannot catch a foreign exception, so without this an AppKit raise
//! inside a callback aborts with no method name. `-[NSApplication run]` itself
//! catches, reports and continues.
//!
//! # The primitive
//!
//! [`objc_try`] has the shape of Rust's `try` intrinsic: a data pointer, a
//! body and a handler, both `extern "C-unwind"`. The wrapper is the assembly
//! `clang -S -fobjc-exceptions` emits for `@try { body(data) } @catch (id e)
//! { on_exc(data, e) }`, embedded with `global_asm!` (one `.s` per
//! architecture), so nothing is compiled from C and there is no `build.rs`.
//! It cannot be Rust source: catching needs an LSDA naming the Objective-C
//! `id` typeinfo, which rustc does not emit. Trampolines pack their typed
//! arguments into a stack closure so the wrapper carries one pointer, which
//! also handles stack-passed arguments. It adds about 1 ns per declared call
//! and nothing to sends.
//!
//! # Order
//!
//! `catch_unwind` outside, `@try` inside. `@catch (id)` matches only the `id`
//! typeinfo, so a Rust panic runs its drops, passes through the wrapper, and
//! reaches the outer guard, which aborts with
//! ``aterm-objc: panic escaped Objective-C method `…`; aborting``.
//!
//! # What is safe to continue from
//!
//! When the handler returns, the unwound Rust locals are dropped, AppKit has run
//! its `@finally`/ARC cleanups, and `objc_end_catch` has freed the exception.
//! AppKit's internal state mid-call is not restored (the same exposure every
//! Cocoa app has under `-[NSApplication run]`'s catch). So:
//!
//! * a contained method returns [`inert`], the all-zero value (`nil`, `NO`,
//!   `0`, `0.0`, the empty range, the origin rect), never a half-built one;
//! * a containment is an event: reported once with the method, the exception's
//!   class, name, reason and call stack ([`report`] through [`set_sink`]),
//!   counted ([`contained_count`]), and the app re-requests whatever the
//!   method was answering on its next turn;
//! * `-dealloc` still aborts, and a declaration whose zero is not inert opts
//!   out with `@abort_on_exception`.
//!
//! # Why not let AppKit catch
//!
//! AppKit's catch is silent, becomes `SIGTRAP` under the
//! `NSApplicationCrashOnExceptions` default, and requires that no Rust frame
//! between the throw and the run loop be `nounwind` or contain a
//! `catch_unwind`. Local containment names the method and does not depend on
//! that default. `NSSetUncaughtExceptionHandler` never fires under a Rust
//! trampoline (a Rust catch-all landing pad claims the exception in phase 1).

use std::ffi::c_void;
use std::io::Write as _;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use crate::runtime::{ClassPtr, Id, ProtocolPtr, Sel};
use crate::{Bool, CGPoint, CGRect, CGSize, NSRange};

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("objc_try_aarch64.s"));
// `att_syntax`: the text is clang's AT&T output, while `global_asm!` defaults
// to Intel on x86. Only a real x86_64 build (codegen) catches a mismatch.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("objc_try_x86_64.s"), options(att_syntax));

// The wrapper references libobjc (the personality, `objc_begin_catch`,
// `objc_end_catch`, `OBJC_EHTYPE_id`) and libunwind (`_Unwind_Resume`, in
// libSystem).
#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// `@try { body(data); return 0 } @catch (id e) { on_exc(data, e); return 1 }`,
    /// defined by the `global_asm!` above. `"C-unwind"` because a Rust panic in
    /// `body` passes through it to the caller's `catch_unwind`.
    fn aterm_objc_try(
        data: *mut u8,
        body: unsafe extern "C-unwind" fn(*mut u8),
        on_exc: unsafe extern "C-unwind" fn(*mut u8, Id),
    ) -> i32;
}

/// The evidence that an Objective-C exception was caught and released.
///
/// Carries only the report's ordinal: the exception object is valid only
/// inside the handler passed to [`objc_try`], and everything worth keeping
/// ([`report`]'s description) is copied out there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contained {
    /// [`report`]'s ordinal for this containment, or `0` when the handler was
    /// not [`report`].
    ordinal: u64,
}

impl Contained {
    /// The ordinal [`report`] assigned to this containment (1-based), or `0`
    /// if the handler did not report. Prefer this over reading
    /// [`contained_count`] afterwards, which other threads also advance.
    #[inline]
    #[must_use]
    pub(crate) fn ordinal(self) -> u64 {
        self.ordinal
    }
}

/// What [`objc_try`]'s two callbacks share: the body, the handler and the
/// body's result.
///
/// `ManuallyDrop`/`MaybeUninit` rather than `Option`s, since this is on every
/// declared-method call. Protocol: `run_body` moves `body` out exactly once;
/// `run_on_exc` moves `on_exc` out iff an exception was caught; `out` is
/// written iff `body` returned (the wrapper answers `0`).
///
/// `on_exc_live` covers the third exit: a Rust panic in `body` passes through
/// to the caller's `catch_unwind` without running `run_on_exc`, so `Drop`
/// releases the handler if it was never taken (a real leak otherwise under
/// winit's `stop_app_on_panic`, which continues).
struct Slot<R, B, H> {
    body: ManuallyDrop<B>,
    on_exc: ManuallyDrop<H>,
    on_exc_live: bool,
    out: MaybeUninit<R>,
}

impl<R, B, H> Drop for Slot<R, B, H> {
    #[inline]
    fn drop(&mut self) {
        if self.on_exc_live {
            // SAFETY: `run_on_exc` clears `on_exc_live` just before its one
            // `ManuallyDrop::take`, so a live flag means the handler is still
            // owned here; it is dropped exactly once.
            unsafe { ManuallyDrop::drop(&mut self.on_exc) };
        }
        // `body` is never live here: `run_body` always runs and takes it (an
        // unwind through `run_body` dropped it). `out` is read by `objc_try`'s
        // `Ok` arm before the slot drops.
    }
}

/// The body callback: move the closure out and run it. Its result is stored
/// only if it returns; an exception unwinds through this `"C-unwind"` frame
/// (dropping the closure) into the wrapper's catch.
unsafe extern "C-unwind" fn run_body<R, B: FnOnce() -> R, H>(data: *mut u8) {
    // SAFETY: `objc_try` passes a live `Slot<R, B, H>` of exactly these
    // parameters and holds no reference to it while the wrapper runs; the
    // wrapper calls this once, so `body` is moved out once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    let body = unsafe { ManuallyDrop::take(&mut slot.body) };
    slot.out.write(body());
}

/// The handler callback, run between `objc_begin_catch` and `objc_end_catch`
/// with the exception object live.
unsafe extern "C-unwind" fn run_on_exc<R, B, H: FnOnce(Id)>(data: *mut u8, exception: Id) {
    // SAFETY: as `run_body`; the body's frames are gone, and the wrapper runs
    // this at most once, so `on_exc` is moved out at most once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    slot.on_exc_live = false;
    let on_exc = unsafe { ManuallyDrop::take(&mut slot.on_exc) };
    on_exc(exception);
}

/// Run `body`; if an Objective-C exception escapes it, run `on_exc` with the
/// exception object and return [`Contained`] instead of a value.
///
/// The exception `Id` passed to `on_exc` is borrowed and valid only during that
/// call; read `-name`, `-reason`, `-callStackSymbols` there ([`report`] does).
///
/// A Rust panic in `body` is not caught; it unwinds to the caller. Every Rust
/// `Drop` between the raise and this call runs first on both kinds of unwind,
/// and `on_exc` is dropped unrun on the panic path.
///
/// The `Err`'s [`Contained::ordinal`] is `0`: this reports nothing ([`contain`]
/// is the reporting form).
///
/// # Unwind safety
///
/// Whatever `body` borrowed may be mid-update after an exception, as after a
/// panic. No `UnwindSafe` bound is demanded; callers assert it at their guard.
#[inline]
pub fn objc_try<R, B, H>(body: B, on_exc: H) -> Result<R, Contained>
where
    B: FnOnce() -> R,
    H: FnOnce(Id),
{
    let mut slot: Slot<R, B, H> = Slot {
        body: ManuallyDrop::new(body),
        on_exc: ManuallyDrop::new(on_exc),
        on_exc_live: true,
        out: MaybeUninit::uninit(),
    };
    // SAFETY: the wrapper calls `run_body::<R, B, H>` and, on an exception,
    // `run_on_exc::<R, B, H>`, each with the address of `slot`, a local that
    // outlives the synchronous call and is untouched meanwhile.
    let caught = unsafe {
        aterm_objc_try(
            (&raw mut slot).cast::<u8>(),
            run_body::<R, B, H>,
            run_on_exc::<R, B, H>,
        )
    };
    if caught == 0 {
        // SAFETY: `0` means `body` returned and `run_body` wrote `out`;
        // `on_exc` is still live and the slot's `Drop` releases it.
        unsafe { Ok(slot.out.assume_init_read()) }
    } else {
        // `body` was dropped by the unwind; `run_on_exc` cleared the flag and
        // ran `on_exc`; `out` was never written. `Drop` finds nothing live.
        Err(Contained { ordinal: 0 })
    }
}

// ---------------------------------------------------------------------------
// The record of a containment, the count, and the sink
// ---------------------------------------------------------------------------

/// One contained exception, as handed to the installed sink. Fields are
/// borrowed from the reporting frame; copy out what you keep.
#[derive(Debug, Clone, Copy)]
pub struct ContainedException<'a> {
    /// Where it was caught: the selector of the declared method (`poke:`),
    /// or the name of the non-method site (`block invoke`, `sendEvent:`,
    /// `CFRunLoop observer`).
    pub method: &'a str,
    /// The Objective-C class of the exception object — `NSException` for
    /// every raise AppKit performs itself.
    pub class: &'a str,
    /// `-[NSException name]`, e.g. `NSInternalInconsistencyException`.
    pub name: &'a str,
    /// `-[NSException reason]`.
    pub reason: &'a str,
    /// `-[NSException callStackSymbols]` as one line, frames joined by ` | `,
    /// innermost first, capped at [`STACK_FRAMES_MAX`].
    pub call_stack: &'a str,
    /// The value of [`contained_count`] after this containment.
    pub ordinal: u64,
}

/// The most call-stack frames [`report`] copies out.
pub(crate) const STACK_FRAMES_MAX: usize = 48;

/// How many exceptions this process has contained.
pub(crate) static CONTAINED: AtomicU64 = AtomicU64::new(0);

/// How many exceptions this process has contained so far. An application
/// polls it each event-loop turn; a change is the signal to re-request
/// whatever the contained method was answering.
#[inline]
#[must_use]
pub fn contained_count() -> u64 {
    CONTAINED.load(Ordering::Relaxed)
}

/// The installed sink, as a type-erased pointer. Null means [`default_sink`].
/// A bare `fn` so the crate stays dependency-free and the sink can be set
/// before any allocator or logger is up.
static SINK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Route every containment report to `sink` instead of stderr. A later call
/// replaces an earlier one.
///
/// The sink runs on the raising thread, inside the catch: it must not block or
/// raise, and must tolerate AppKit being mid-operation.
pub fn set_sink(sink: fn(&ContainedException<'_>)) {
    SINK.store(sink as *mut (), Ordering::Release);
}

/// The sink installed by [`set_sink`], or the default.
fn sink() -> fn(&ContainedException<'_>) {
    let raw = SINK.load(Ordering::Acquire);
    if raw.is_null() {
        default_sink
    } else {
        // SAFETY: the only writer is `set_sink`, which stores a
        // `fn(&ContainedException<'_>)` cast to `*mut ()`; function and data
        // pointers have the same width on 64-bit Apple targets.
        unsafe { std::mem::transmute::<*mut (), fn(&ContainedException<'_>)>(raw) }
    }
}

/// Where a report goes when no sink is installed: one best-effort line on
/// stderr. Nothing here may panic (it runs inside the catch), as with
/// [`crate::abort_on_unwind`].
pub fn default_sink(e: &ContainedException<'_>) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"aterm-objc: contained ");
    let _ = err.write_all(e.name.as_bytes());
    let _ = err.write_all(b" (");
    let _ = err.write_all(e.class.as_bytes());
    let _ = err.write_all(b") in Objective-C method `");
    let _ = err.write_all(e.method.as_bytes());
    let _ = err.write_all(b"`: ");
    let _ = err.write_all(e.reason.as_bytes());
    let _ = err.write_all(b" | stack: ");
    let _ = err.write_all(e.call_stack.as_bytes());
    let _ = err.write_all(b"\n");
    let _ = err.flush();
}

/// Describe a live exception object and hand the description to the sink;
/// returns the containment's ordinal (the value of [`contained_count`] as this
/// call incremented it).
///
/// The handler every trampoline passes to [`objc_try`]. It runs inside the
/// catch with `exception` borrowed. `-name`, `-reason` and `-callStackSymbols`
/// are read under a nested [`objc_try`], so a second raise while describing
/// cannot escape; the report says so and the containment still counts.
#[cold]
#[inline(never)]
pub(crate) fn report(method: &str, exception: Id) -> u64 {
    let ordinal = CONTAINED.fetch_add(1, Ordering::Relaxed) + 1;
    // Its own pool: the accessors return autoreleased objects and there may be
    // no caller pool. The exception itself is retained by the runtime from the
    // throw until `objc_end_catch`, after this returns.
    let described = crate::runtime::autoreleasepool(|_pool| {
        objc_try(
            || describe(exception),
            |_second| {
                // Nothing to read from the second one: reading is what raised.
            },
        )
    });
    let (class, name, reason, call_stack) = match described {
        Ok(d) => d,
        Err(Contained { .. }) => (
            String::from("(unknown)"),
            String::from("(unknown)"),
            String::from("(a second exception was raised while describing the first)"),
            String::new(),
        ),
    };
    let record = ContainedException {
        method,
        class: &class,
        name: &name,
        reason: &reason,
        call_stack: &call_stack,
        ordinal,
    };
    (sink())(&record);
    ordinal
}

/// `(class, name, reason, call stack)` of a live exception object, through
/// ordinary sends.
fn describe(exception: Id) -> (String, String, String, String) {
    if exception.is_null() {
        return (
            String::from("(nil)"),
            String::from("(nil)"),
            String::from("(a nil exception object was caught)"),
            String::new(),
        );
    }
    // SAFETY: `exception` is the live object the runtime handed to the catch;
    // the accessors are `@@:` on `NSException`, and a non-exception `@throw`
    // still answers `-respondsToSelector:`, which the guard below asks first.
    unsafe {
        let class = crate::runtime::class_name(crate::runtime::class_of(exception))
            .to_string_lossy()
            .into_owned();
        let responds =
            |s: Sel| crate::send::send_bool_sel(exception, crate::sel!(respondsToSelector:), s);
        let name = if responds(crate::sel!(name)) {
            crate::runtime::ns_string_to_rust(crate::send::send_id(exception, crate::sel!(name)))
        } else {
            String::from("(not an NSException)")
        };
        let reason = if responds(crate::sel!(reason)) {
            crate::runtime::ns_string_to_rust(crate::send::send_id(exception, crate::sel!(reason)))
        } else {
            String::new()
        };
        let mut stack = String::new();
        if responds(crate::sel!(callStackSymbols)) {
            let symbols = crate::send::send_id(exception, crate::sel!(callStackSymbols));
            if !symbols.is_null() {
                let n = crate::send::send_usize(symbols, crate::sel!(count));
                for i in 0..n.min(STACK_FRAMES_MAX) {
                    let frame = crate::send::send_id_usize(symbols, crate::sel!(objectAtIndex:), i);
                    let text = crate::runtime::ns_string_to_rust(frame);
                    if i > 0 {
                        stack.push_str(" | ");
                    }
                    // Collapse AppKit's runs of padding spaces within a frame.
                    let mut prev_space = false;
                    for ch in text.chars() {
                        if ch == ' ' {
                            if !prev_space {
                                stack.push(' ');
                            }
                            prev_space = true;
                        } else {
                            stack.push(ch);
                            prev_space = false;
                        }
                    }
                }
                if n > STACK_FRAMES_MAX {
                    stack.push_str(" | …");
                }
            }
        }
        (class, name, reason, stack)
    }
}

/// Run `body` under containment, reporting a caught exception against `method`:
/// [`objc_try`] with [`report`] as the handler. What every trampoline uses.
#[inline]
pub fn contain<R, B: FnOnce() -> R>(method: &str, body: B) -> Result<R, Contained> {
    // `report` assigns the ordinal and `objc_try`'s `Err` cannot know it, so it
    // travels out through a stack cell (one store, cold path only).
    let ordinal = std::cell::Cell::new(0);
    objc_try(body, |exception| ordinal.set(report(method, exception))).map_err(|_| Contained {
        ordinal: ordinal.get(),
    })
}

// ---------------------------------------------------------------------------
// The inert return value
// ---------------------------------------------------------------------------

/// A return type whose all-zero value means "nothing happened".
///
/// A contained method must return the value its caller reads as no result:
/// `nil`, `NO`, `0`, `0.0`, `{0, 0}` for an `NSRange`, the origin rect. For
/// every type this crate passes over the C ABI that is the all-zero bit
/// pattern, so the trampoline can produce it without knowing the selector.
///
/// A method whose return type is not `InertZero`, or whose zero is a real
/// answer for its selector, declares `@abort_on_exception` after `@sel(…)`.
///
/// # Safety
///
/// [`inert`] materialises the value with `MaybeUninit::zeroed().assume_init()`,
/// so all-zero bytes must be a valid, initialised value of `Self`, and the one
/// its Objective-C readers take as "no result". Plain `#[repr(C)]` /
/// `#[repr(transparent)]` data (integers, floats, raw pointers, `Option` of a
/// non-null pointer, structs of those) qualifies. A type holding a `NonNull`,
/// a reference, an enum without a zero discriminant, or a wrapper adding an
/// invariant zero violates must not implement it.
pub unsafe trait InertZero: Sized {}

/// The all-zero value of `R` — see [`InertZero`] for why it is the right one.
#[inline]
#[must_use]
pub fn inert<R: InertZero>() -> R {
    // SAFETY: `InertZero` is the promise that all-zero bytes are a valid,
    // initialised `R`.
    unsafe { MaybeUninit::<R>::zeroed().assume_init() }
}

macro_rules! inert_zero {
    ($($t:ty),* $(,)?) => {$(
        // SAFETY: each is a `#[repr(C)]`/`#[repr(transparent)]` plain-data type
        // (integers, floats, raw pointers, structs of them) whose all-zero
        // pattern is its "no result" value.
        unsafe impl InertZero for $t {}
    )*};
}
inert_zero! {
    (),
    i8, u8, i16, u16, i32, u32, i64, u64, isize, usize,
    f32, f64,
    Bool, Id, ClassPtr, ProtocolPtr, Sel,
    *mut c_void, *const c_void,
    *const std::ffi::c_char, *mut std::ffi::c_uchar, *mut Bool,
    CGPoint, CGSize, CGRect, NSRange, *mut NSRange, *const NSRange,
    crate::block::BlockPtr,
}

/// `Option<T>` of a pointer-shaped `T` is `None` at zero, which is the inert
/// answer for a method that returns one.
// SAFETY: `Option<NonNull<T>>` and `Option<&T>` are guaranteed
// null-pointer-optimised, so all-zero is `None`.
unsafe impl<T> InertZero for Option<std::ptr::NonNull<T>> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rust_panic_passes_through_the_wrapper_to_catch_unwind() {
        use std::cell::Cell;
        let dropped = Cell::new(false);
        struct Tell<'a>(&'a Cell<bool>);
        impl Drop for Tell<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let handler_ran = Cell::new(false);
        let outer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            objc_try(
                || {
                    let _t = Tell(&dropped);
                    if dropped.get() {
                        return 1;
                    }
                    panic!("a Rust panic inside the body");
                },
                |_| handler_ran.set(true),
            )
        }));
        assert!(outer.is_err(), "the panic reached the OUTER catch_unwind");
        assert!(dropped.get(), "the body's local was dropped by the unwind");
        assert!(
            !handler_ran.get(),
            "@catch(id) must not swallow a Rust panic"
        );
    }

    /// The handler is released on every way out of the wrapper, including the
    /// Rust-panic pass-through where it never ran.
    #[test]
    fn the_unrun_handler_is_dropped_on_both_the_ok_and_the_panic_path() {
        use std::rc::Rc;
        let token = Rc::new(());
        let handler_token = Rc::clone(&token);
        let r = objc_try(
            || 1,
            move |_| {
                let _ = &handler_token;
            },
        );
        assert_eq!(r, Ok(1));
        assert_eq!(
            Rc::strong_count(&token),
            1,
            "the handler was dropped on the Ok path"
        );

        let handler_token = Rc::clone(&token);
        let outer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            objc_try(
                || -> i32 { panic!("a Rust panic inside the body") },
                move |_| {
                    let _ = &handler_token;
                },
            )
        }));
        assert!(outer.is_err());
        assert_eq!(
            Rc::strong_count(&token),
            1,
            "the handler that never ran was dropped on the panic path"
        );
    }
}
