// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Containing an `NSException` raised inside Rust code the Objective-C runtime
//! called — a declared method, a block, a dispatch trampoline, an override.
//!
//! # What this replaces, and what was measured
//!
//! Until this module, every trampoline [`crate::declare_class!`] generated
//! aborted the process if AppKit raised inside it: the exception unwound into
//! the trampoline's `catch_unwind`, Rust cannot catch a foreign exception, and
//! `__rust_foreign_exception` aborted with no method name. That is what turned
//! two wrong-typed accessor sends into the v0.72.0 mouse-move crash, and it
//! made every future AppKit assertion inside a callback fatal. AppKit itself
//! does not work that way — `-[NSApplication run]` catches, reports and
//! continues.
//!
//! The replacement was MEASURED before it was built, mechanism by mechanism,
//! in a standalone probe that raised a real `NSInternalInconsistencyException`
//! through each candidate shape
//! (`docs/measured/2026-09-03-delivery-and-objc-exception-containment.md`,
//! §2). The row this module implements:
//!
//! ```text
//! mechanism                                  survives   Rust Drops run   cost
//! today (extern "C" sends, catch_unwind)     no         partial          1.53 ns/call
//! @try/@catch(id) wrapper as global_asm!     YES        all, before      +1.0 ns per
//!   around a "C-unwind" Rust body                       the catch        declared call
//! ```
//!
//! # The one primitive
//!
//! [`objc_try`] is the shape of Rust's own `try` intrinsic: a data pointer, a
//! body, and a handler, both `extern "C-unwind"`. The wrapper is the text
//! `clang -S -fobjc-exceptions` emits for `@try { body(data) } @catch (id e)
//! { on_exc(data, e) }`, embedded with `global_asm!` — one `.s` per
//! architecture — so that NOTHING is compiled from C or Objective-C and the
//! crate keeps its zero-dependency, no-`build.rs` construction. It cannot be
//! written in Rust source: catching needs an LSDA naming the Objective-C
//! `id` typeinfo, which rustc will not emit, and the 64-bit runtime has no
//! `setjmp`-style catch API.
//!
//! A per-method assembly stub that re-forwards registers was rejected: it
//! cannot handle the stack-passed arguments of a sixteen-parameter method. The
//! trampoline instead packs its typed arguments into a stack struct — the
//! closure it hands to [`objc_try`] — and the wrapper carries one pointer.
//!
//! # Order matters, and was measured
//!
//! `catch_unwind` OUTSIDE, `@try` INSIDE. A Rust panic is not swallowed by
//! `@catch (id)`: the personality matches only the `id` typeinfo, a foreign
//! (Rust) exception matches no typeinfo but a catch-all, so the panic runs its
//! drops, crosses this wrapper, and reaches the outer guard. Today's
//! abort-on-panic policy and its exact message
//! (``aterm-objc: panic escaped Objective-C method `…`; aborting``) survive
//! unchanged.
//!
//! # What is safe to continue from
//!
//! When the handler returns, the unwound Rust locals have been dropped, AppKit
//! has run its own `@finally`/ARC cleanups, and `objc_end_catch` has freed the
//! exception. What is NOT restored is whatever AppKit-internal state the
//! raising call was in the middle of — precisely the exposure Apple ships to
//! every Cocoa app that lets `-[NSApplication run]` catch. The rule that makes
//! continuing sound is therefore:
//!
//! * a contained method returns the value meaning "nothing happened" for its
//!   selector — [`inert`], the all-zero value: `nil`, `NO`, `0`, `0.0`, the
//!   empty range, the origin rect — never a half-built one;
//! * the application treats a containment as an EVENT: it is logged once with
//!   the method, the exception's class, name, reason and call stack
//!   ([`report`] through the installed [`set_sink`]), counted
//!   ([`contained_count`]), and the app re-requests on its next turn whatever
//!   the method was answering (a redraw, the modifier state);
//! * `-dealloc` still aborts — a half-deallocated object is not a state to
//!   continue from — and a declaration whose zero is NOT inert opts out with
//!   `@abort_on_exception` and keeps today's abort.
//!
//! # Why not let AppKit catch
//!
//! It works on a default machine — measured: a `"C-unwind"` raise from inside
//! `-[NSApplication run]` is caught by AppKit and the loop continues — but it
//! is silent (nothing on stderr, nothing in `log show`), it is turned back into
//! `SIGTRAP` by the `NSApplicationCrashOnExceptions` user default, and it
//! requires that NOTHING Rust between the throw and the run loop be
//! `nounwind` or contain a `catch_unwind`, forever. Containment here is local,
//! names the method statically, and survives regardless of that default.
//!
//! `NSSetUncaughtExceptionHandler` never fires under any Rust trampoline (a
//! Rust catch-all landing pad claims the exception in phase 1), and
//! `objc_setExceptionPreprocessor` is subsumed: the catch already holds the
//! exception and knows the method.

use std::ffi::c_void;
use std::io::Write as _;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use crate::runtime::{ClassPtr, Id, ProtocolPtr, Sel};
use crate::{Bool, CGPoint, CGRect, CGSize, NSRange};

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("objc_try_aarch64.s"));
// `att_syntax`: the text is what `clang -S` emits, and clang's default is
// AT&T, while `global_asm!`'s default on x86 is Intel. Without the option the
// compat slice fails at every `movq`/`callq` — found by building the crate for
// `x86_64-apple-darwin`, which a `cargo check` (no codegen, so no assembly)
// never would have.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("objc_try_x86_64.s"), options(att_syntax));

// The wrapper's five external references live in libobjc (the personality,
// `objc_begin_catch`, `objc_end_catch`, `OBJC_EHTYPE_id`) and libunwind
// (`_Unwind_Resume`, part of libSystem). `runtime.rs` already links libobjc;
// the attribute is repeated here so the module states its own link need.
#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// `@try { body(data); return 0 } @catch (id e) { on_exc(data, e); return 1 }`.
    ///
    /// Defined by the `global_asm!` above. `"C-unwind"` because a Rust panic
    /// raised in `body` passes THROUGH it to the caller's `catch_unwind`.
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
    /// [`report`]'s ordinal for this containment — the value of
    /// [`contained_count`] as THIS report incremented it — or `0` when the
    /// handler was not [`report`] (a bare [`objc_try`] with a caller's own
    /// handler counts nothing).
    ordinal: u64,
}

impl Contained {
    /// The ordinal [`report`] assigned to this containment (1-based), or `0`
    /// if the handler did not report.
    ///
    /// Read this rather than [`contained_count`] after the fact: the global
    /// counter is shared by every thread, so a containment elsewhere in
    /// between would make the count name a different report.
    #[inline]
    #[must_use]
    pub fn ordinal(self) -> u64 {
        self.ordinal
    }
}

/// What [`objc_try`]'s two callbacks share: the body to run, the handler to
/// run instead, and the body's result.
///
/// `ManuallyDrop` and `MaybeUninit` rather than `Option`s, deliberately: this
/// is on the path of EVERY declared-method call, and the wrapper's whole
/// budget is one nanosecond. The ownership protocol is fixed by the wrapper:
/// `body` is moved out exactly once, by `run_body`; `on_exc` is moved out by
/// `run_on_exc` if and only if an exception was caught; `out` is written if
/// and only if `body` returned, which is exactly when the wrapper answers
/// `0`.
///
/// `on_exc_live` is what makes the THIRD way out of the wrapper leak-free: a
/// Rust panic in `body` passes through `aterm_objc_try` to the caller's
/// `catch_unwind`, so neither `run_on_exc` nor [`objc_try`]'s `Ok` arm ever
/// runs — and a handler capturing an `Arc` or a `Retained` would leak on
/// that path (harmless under an abort, a real leak under winit's
/// `stop_app_on_panic`, which continues). The slot's `Drop` releases the
/// handler if it was never taken, on every path, for the price of one byte
/// store when it is.
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
            // SAFETY: `on_exc_live` is cleared by `run_on_exc` immediately
            // before the one `ManuallyDrop::take`, so a live flag means the
            // handler is still owned here and is dropped exactly once.
            unsafe { ManuallyDrop::drop(&mut self.on_exc) };
        }
        // `body` is never live here: `run_body` takes it before running it,
        // and the wrapper always runs `run_body` — a `body` that panicked or
        // raised was dropped by the unwind through `run_body`'s frame.
        // `out` is read by `objc_try`'s `Ok` arm before the slot drops.
    }
}

/// The body callback: move the closure out of the slot and run it. Its result
/// is stored only if it returns; an exception unwinds through this frame
/// (dropping the moved-out closure on the way — this is a `"C-unwind"` frame,
/// so it has its landing pad) and lands in the wrapper's catch.
unsafe extern "C-unwind" fn run_body<R, B: FnOnce() -> R, H>(data: *mut u8) {
    // SAFETY: `objc_try` passes the address of a live `Slot<R, B, H>` of
    // exactly these parameters, and holds no reference of its own to it while
    // the wrapper runs, so this is the only reference. The wrapper calls this
    // exactly once, so `body` is moved out exactly once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    let body = unsafe { ManuallyDrop::take(&mut slot.body) };
    slot.out.write(body());
}

/// The handler callback, run between `objc_begin_catch` and `objc_end_catch`
/// with the exception object live.
unsafe extern "C-unwind" fn run_on_exc<R, B, H: FnOnce(Id)>(data: *mut u8, exception: Id) {
    // SAFETY: as `run_body`; the body's frames are gone by the time the
    // handler runs, so again this is the only reference, and the wrapper runs
    // this at most once per call, so `on_exc` is moved out at most once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    slot.on_exc_live = false;
    let on_exc = unsafe { ManuallyDrop::take(&mut slot.on_exc) };
    on_exc(exception);
}

/// Run `body`; if an Objective-C exception escapes it, run `on_exc` with the
/// exception object and return [`Contained`] instead of a value.
///
/// The exception `Id` handed to `on_exc` is BORROWED and valid only for the
/// duration of that call — the wrapper releases it on return. Read `-name`,
/// `-reason` and `-callStackSymbols` there ([`report`] does), and copy out what
/// is worth keeping.
///
/// A Rust panic in `body` is NOT caught here; it unwinds through to the
/// caller, exactly as it would without the wrapper. Every Rust `Drop` on the
/// frames between the raise and this call runs before `on_exc` does, on both
/// kinds of unwind — and `on_exc` itself is dropped, unrun, on the panic path
/// (see `Slot`), so a handler capturing an owned value does not leak.
///
/// The `Err` carries a [`Contained`] whose [`Contained::ordinal`] is `0`:
/// this function reports nothing. [`contain`] is the reporting form.
///
/// # Unwind safety
///
/// `body` is observed after an exception exactly as a `catch_unwind` body is
/// observed after a panic: whatever it borrowed may be mid-update. No
/// `UnwindSafe` bound is demanded because every caller in this crate already
/// asserts it at the guard outside; a caller elsewhere owes the same
/// judgement.
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
    // `run_on_exc::<R, B, H>`, each with the address of `slot` — a local that
    // outlives the call because the wrapper is synchronous — and nothing here
    // touches `slot` while the wrapper runs. Both callbacks are instantiated
    // at exactly the `Slot` whose address is passed.
    let caught = unsafe {
        aterm_objc_try(
            (&raw mut slot).cast::<u8>(),
            run_body::<R, B, H>,
            run_on_exc::<R, B, H>,
        )
    };
    if caught == 0 {
        // SAFETY: `0` means `body` returned, so `run_body` wrote `out`;
        // `run_on_exc` never ran, so `on_exc` is still live and the slot's
        // `Drop` releases it right after this read.
        unsafe { Ok(slot.out.assume_init_read()) }
    } else {
        // `run_body` moved `body` out and the unwind dropped it; `run_on_exc`
        // cleared the flag, moved `on_exc` out and ran it; `out` was never
        // written. The slot's `Drop` finds nothing live.
        Err(Contained { ordinal: 0 })
    }
}

// ---------------------------------------------------------------------------
// The record of a containment, the count, and the sink
// ---------------------------------------------------------------------------

/// One contained exception, as handed to the installed sink.
///
/// Every field is borrowed from the reporting frame; a sink that wants to keep
/// any of it copies it out.
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
    /// `-[NSException callStackSymbols]`, joined into ONE line with ` | `
    /// between frames, innermost first, and capped at [`STACK_FRAMES_MAX`]
    /// frames.
    pub call_stack: &'a str,
    /// The value of [`contained_count`] AFTER this containment.
    pub ordinal: u64,
}

/// The most call-stack frames [`report`] copies out.
pub const STACK_FRAMES_MAX: usize = 48;

/// How many exceptions this process has contained.
pub static CONTAINED: AtomicU64 = AtomicU64::new(0);

/// How many exceptions this process has contained so far.
///
/// An application reads this on every turn of its event loop: a change since
/// the last turn is the EVENT a containment is — the signal to re-request
/// whatever the contained method was answering.
#[inline]
#[must_use]
pub fn contained_count() -> u64 {
    CONTAINED.load(Ordering::Relaxed)
}

/// The installed sink, as a type-erased pointer. Null means [`default_sink`].
///
/// A bare `fn` rather than a boxed closure so that this crate stays
/// dependency-free and the sink can be installed before any allocator or
/// logger is up; the application's sink reaches its logger through its own
/// statics.
static SINK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Route every containment report to `sink` instead of stderr.
///
/// Installed once, at application startup, by the crate that owns the log
/// file; a later call replaces the earlier one. The sink runs on the thread
/// the exception was raised on, INSIDE the catch, before the trampoline
/// returns — it must not block, must not raise, and must tolerate being
/// called while AppKit is mid-operation.
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
        // `fn(&ContainedException<'_>)` cast to `*mut ()`; a function pointer
        // and a data pointer are the same width on every Apple 64-bit target,
        // and a `fn` pointer has no provenance to lose.
        unsafe { std::mem::transmute::<*mut (), fn(&ContainedException<'_>)>(raw) }
    }
}

/// Where a report goes when no sink is installed: ONE line on stderr,
/// best-effort — a closed or full stderr is an ordinary state for a GUI app,
/// and this is the same rule [`crate::abort_on_unwind`] follows for the same
/// reason (nothing here may panic; it runs inside the catch).
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
/// returns the containment's ordinal (the value of [`contained_count`] as
/// this call incremented it).
///
/// This is the handler every trampoline in the crate passes to [`objc_try`].
/// It runs inside the catch, with `exception` borrowed and live. The
/// description is read through ordinary sends (`-name`, `-reason`,
/// `-callStackSymbols`) under a NESTED [`objc_try`], so a second raise while
/// describing the first cannot escape: the report then says so and the
/// containment still counts.
#[cold]
#[inline(never)]
pub fn report(method: &str, exception: Id) -> u64 {
    let ordinal = CONTAINED.fetch_add(1, Ordering::Relaxed) + 1;
    // Under a pool of its own: `-name`, `-reason` and `-callStackSymbols`
    // answer autoreleased objects, and a containment may happen on a thread
    // (or at a depth) with no pool of the caller's in place. The exception
    // object itself is not in this pool — the runtime retained it at the
    // throw and releases it at `objc_end_catch`, after this returns.
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
    // SAFETY: `exception` is the live, retained object the runtime handed to
    // the catch; `-name`, `-reason` and `-callStackSymbols` are `@@:` on
    // `NSException`, and an object of another class (a bare `@throw` of a
    // non-exception) still answers `-respondsToSelector:`, which is what the
    // guard below asks before sending anything class-specific.
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
                    // AppKit pads each frame with runs of spaces between the
                    // index, the image and the address; collapse them so the
                    // line stays readable.
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

/// Run `body` under containment, reporting a caught exception against
/// `method` — [`objc_try`] with [`report`] as the handler.
///
/// This is the form every trampoline uses; [`objc_try`] is exported for a
/// site that wants to see the exception object itself.
#[inline]
pub fn contain<R, B: FnOnce() -> R>(method: &str, body: B) -> Result<R, Contained> {
    // The ordinal travels out of the handler through a stack cell: `report`
    // is the only place that assigns one, and `objc_try`'s `Err` cannot know
    // it. One store on the cold path; nothing on the hot one.
    let ordinal = std::cell::Cell::new(0);
    objc_try(body, |exception| ordinal.set(report(method, exception))).map_err(|_| Contained {
        ordinal: ordinal.get(),
    })
}

// ---------------------------------------------------------------------------
// The inert return value
// ---------------------------------------------------------------------------

/// A return type whose ALL-ZERO value means "nothing happened".
///
/// A contained method must return something, and it must be the value the
/// selector's caller reads as no result: `nil` for an object, `NO` for a
/// `BOOL`, `0` for a count or an index, `0.0` for a measurement, `{0, 0}` for
/// an `NSRange`, the origin rect for an `NSRect`. For every type this crate
/// crosses the C ABI with, that value IS the all-zero bit pattern, which is
/// why the trampoline can produce it without knowing the selector.
///
/// The trait is `unsafe` to implement because [`inert`] materialises the
/// value with `MaybeUninit::zeroed().assume_init()`: an implementor promises
/// that all-zero bytes are a valid, initialised value of `Self`. Every
/// [`crate::Encode`] type in this crate is a `#[repr(C)]` plain-data type for
/// which that holds; a type holding a `NonNull`, a reference, or an enum
/// without a zero discriminant must not implement it.
///
/// A method whose return type is not `InertZero` — or whose zero is a valid
/// answer that is NOT inert for its selector — declares
/// `@abort_on_exception` after its `@sel(…)` and keeps today's abort.
///
/// # Safety
///
/// The all-zero bit pattern must be a valid, initialised value of `Self`, and
/// the one its Objective-C readers take as "no result". `#[repr(C)]` and
/// `#[repr(transparent)]` plain-data types — integers, floats, raw pointers,
/// `Option` of a non-null pointer, and structs of those — qualify. A type
/// holding a `NonNull`, a reference, a `bool` (only two of its 256 patterns are
/// valid, but zero is one of them — the hazard is a wrapper that adds an
/// invariant), or an enum without a zero discriminant must not implement it.
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
        // SAFETY: each of these is a `#[repr(C)]`/`#[repr(transparent)]`
        // plain-data type — integers, floats, raw pointers and structs of
        // them — for which the all-zero pattern is the value the paragraph on
        // `InertZero` names.
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
// null-pointer-optimised, so all-zero IS `None`.
unsafe impl<T> InertZero for Option<std::ptr::NonNull<T>> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_that_returns_is_ok_and_counts_nothing() {
        let before = contained_count();
        let r = objc_try(|| 41 + 1, |_| panic!("no exception was raised"));
        assert_eq!(r, Ok(42));
        assert_eq!(contained_count(), before);
    }

    #[test]
    fn drops_in_the_body_run_on_the_normal_path() {
        use std::cell::Cell;
        let dropped = Cell::new(false);
        struct Tell<'a>(&'a Cell<bool>);
        impl Drop for Tell<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let r = objc_try(
            || {
                let _t = Tell(&dropped);
                7
            },
            |_| {},
        );
        assert_eq!(r, Ok(7));
        assert!(dropped.get());
    }

    #[test]
    fn inert_is_the_zero_value() {
        assert_eq!(inert::<i64>(), 0);
        assert_eq!(inert::<f64>(), 0.0);
        assert!(inert::<Id>().is_null());
        assert!(!inert::<Bool>().as_bool());
        let r = inert::<NSRange>();
        assert_eq!((r.location, r.length), (0, 0));
        assert!(inert::<Option<std::ptr::NonNull<u8>>>().is_none());
    }

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

    /// The handler is an owned value; it is released on EVERY way out of the
    /// wrapper — the normal return, and the Rust-panic pass-through where it
    /// never ran.
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

    #[test]
    fn a_bare_objc_try_reports_no_ordinal() {
        // No exception here, so only the type-level claim is checked: the
        // `Err` of a bare `objc_try` says ordinal 0. The reporting form is
        // measured in `tests/exception_containment.rs`.
        let c = Contained { ordinal: 0 };
        assert_eq!(c.ordinal(), 0);
    }
}
