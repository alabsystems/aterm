// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Catching and reporting Objective-C `NSException`s across Rust callback boundaries.

use std::ffi::c_void;
use std::io::Write as _;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use crate::runtime::{ClassPtr, Id, ProtocolPtr, Sel};
use crate::{Bool, CGPoint, CGRect, CGSize, NSRange};

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(include_str!("objc_try_aarch64.s"));
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(include_str!("objc_try_x86_64.s"), options(att_syntax));

#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// `@try { body(data); return 0 } @catch (id e) { on_exc(data, e); return 1 }`,
    /// defined by the `global_asm!` above.
    fn aterm_objc_try(
        data: *mut u8,
        body: unsafe extern "C-unwind" fn(*mut u8),
        on_exc: unsafe extern "C-unwind" fn(*mut u8, Id),
    ) -> i32;
}

/// Evidence that an Objective-C exception was caught and released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contained {
    /// [`report`]'s ordinal for this containment, or `0` when the handler was
    /// not [`report`].
    ordinal: u64,
}

impl Contained {
    /// Returns the 1-based ordinal assigned by [`report`], or `0` if unreported.
    #[inline]
    #[must_use]
    pub(crate) const fn ordinal(self) -> u64 {
        self.ordinal
    }
}

/// What [`objc_try`]'s two callbacks share: the body, the handler and the
/// body's result.
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
            // SAFETY: `on_exc_live` is cleared before `ManuallyDrop::take`, so `on_exc` is dropped once.
            unsafe { ManuallyDrop::drop(&mut self.on_exc) };
        }
    }
}

/// The body callback: move the closure out and run it. Its result is stored
/// only if it returns; an exception unwinds through this `"C-unwind"` frame
/// (dropping the closure) into the wrapper's catch.
unsafe extern "C-unwind" fn run_body<R, B: FnOnce() -> R, H>(data: *mut u8) {
    // SAFETY: `data` points to a live `Slot<R, B, H>` that `objc_try` holds no reference to while
    // this runs, and `run_body` is called once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    let body = unsafe { ManuallyDrop::take(&mut slot.body) };
    slot.out.write(body());
}

/// The handler callback, run between `objc_begin_catch` and `objc_end_catch`
/// with the exception object live.
unsafe extern "C-unwind" fn run_on_exc<R, B, H: FnOnce(Id)>(data: *mut u8, exception: Id) {
    // SAFETY: `data` points to a live `Slot<R, B, H>` and `run_on_exc` is called at most once.
    let slot = unsafe { &mut *data.cast::<Slot<R, B, H>>() };
    slot.on_exc_live = false;
    let on_exc = unsafe { ManuallyDrop::take(&mut slot.on_exc) };
    on_exc(exception);
}

/// Runs `body`, invoking `on_exc` and returning `Err(Contained)` if an Objective-C exception escapes.
///
/// The exception `Id` passed to `on_exc` is borrowed and valid only during that call. A Rust panic
/// in `body` is not caught: it unwinds to the caller, and `on_exc` is dropped unrun.
///
/// # Unwind safety
/// Whatever `body` borrowed may be mid-update after an exception, as after a panic. No
/// `UnwindSafe` bound is demanded; callers assert it at their guard.
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
    // SAFETY: `slot` outlives `aterm_objc_try` and matches the callback type parameters.
    let caught = unsafe {
        aterm_objc_try(
            (&raw mut slot).cast::<u8>(),
            run_body::<R, B, H>,
            run_on_exc::<R, B, H>,
        )
    };
    if caught == 0 {
        // SAFETY: `caught == 0` guarantees `run_body` initialized `slot.out`.
        unsafe { Ok(slot.out.assume_init_read()) }
    } else {
        // `body` was dropped by the unwind; `run_on_exc` cleared the flag and
        // ran `on_exc`; `out` was never written. `Drop` finds nothing live.
        Err(Contained { ordinal: 0 })
    }
}

/// Borrowed description of a caught Objective-C exception passed to the reporting sink.
#[derive(Debug, Clone, Copy)]
pub struct ContainedException<'a> {
    /// Selector or callback site where the exception was caught.
    pub method: &'a str,
    /// Objective-C class name of the exception object.
    pub class: &'a str,
    /// `-[NSException name]`.
    pub name: &'a str,
    /// `-[NSException reason]`.
    pub reason: &'a str,
    /// `-[NSException callStackSymbols]` joined by ` | `, capped at [`STACK_FRAMES_MAX`].
    pub call_stack: &'a str,
    /// Value of [`contained_count`] after this containment.
    pub ordinal: u64,
}

/// Maximum number of call-stack frames copied by [`report`].
pub(crate) const STACK_FRAMES_MAX: usize = 48;

/// Process-wide count of contained Objective-C exceptions.
pub(crate) static CONTAINED: AtomicU64 = AtomicU64::new(0);

/// Returns the number of Objective-C exceptions contained by this process so far.
#[inline]
#[must_use]
pub fn contained_count() -> u64 {
    CONTAINED.load(Ordering::Relaxed)
}

/// The installed sink, as a type-erased pointer. Null means [`default_sink`].
static SINK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Routes containment reports to `sink` instead of [`default_sink`].
pub fn set_sink(sink: fn(&ContainedException<'_>)) {
    SINK.store(sink as *mut (), Ordering::Release);
}

/// The sink installed by [`set_sink`], or the default.
fn sink() -> fn(&ContainedException<'_>) {
    let raw = SINK.load(Ordering::Acquire);
    if raw.is_null() {
        default_sink
    } else {
        // SAFETY: `set_sink` only stores `fn(&ContainedException<'_>)` cast to `*mut ()`; function
        // and data pointers have the same width on 64-bit Apple targets.
        unsafe { std::mem::transmute::<*mut (), fn(&ContainedException<'_>)>(raw) }
    }
}

/// Default containment sink that writes one best-effort line to stderr.
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

/// Describes `exception`, invokes the active sink, and returns the containment ordinal.
#[cold]
#[inline(never)]
pub(crate) fn report(method: &str, exception: Id) -> u64 {
    let ordinal = CONTAINED.fetch_add(1, Ordering::Relaxed) + 1;
    let described = crate::runtime::autoreleasepool(|_pool| {
        objc_try(
            || describe(exception),
            |_second| {
                // Secondary exception ignored while describing the first.
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
    // SAFETY: `exception` is the live caught object and selector support is checked before sending.
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

/// Runs `body` under [`objc_try`], reporting any caught exception against `method` via [`report`].
#[inline]
pub fn contain<R, B: FnOnce() -> R>(method: &str, body: B) -> Result<R, Contained> {
    let ordinal = std::cell::Cell::new(0);
    objc_try(body, |exception| ordinal.set(report(method, exception))).map_err(|_| Contained {
        ordinal: ordinal.get(),
    })
}

/// Return type whose all-zero bit pattern is a valid inert value when an exception is contained.
/// # Safety
/// All-zero bytes must be a valid initialized value of `Self`.
pub unsafe trait InertZero: Sized {}

/// Returns the all-zero inert value of `R`.
#[inline]
#[must_use]
pub const fn inert<R: InertZero>() -> R {
    // SAFETY: `InertZero` guarantees all-zero bytes are a valid `R`.
    unsafe { MaybeUninit::<R>::zeroed().assume_init() }
}

macro_rules! inert_zero {
    ($($t:ty),* $(,)?) => {$(
        // SAFETY: plain-data primitive or `#[repr(C)]` type valid at all-zero bytes.
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

// SAFETY: `Option<NonNull<T>>` and `Option<&T>` are null-pointer-optimized to `None`.
/// `Option<T>` of a pointer-shaped `T` is `None` at zero, which is the inert
/// answer for a method that returns one.
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
