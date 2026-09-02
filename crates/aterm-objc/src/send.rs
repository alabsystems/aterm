// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One `unsafe fn` per C PROTOTYPE — the typed [`msg`] casts every send needs.
//!
//! # Why these are in the RUNTIME crate and not in a binding layer
//!
//! This crate's charter says a send "casts [`msg`] to the EXACT prototype of
//! the selector; a wrong prototype corrupts registers silently, so a typed cast
//! per selector is the only sound form." These functions ARE that rule, written
//! once per shape instead of once per call site — and every one of them is pure
//! runtime: **not one names a framework class, a selector, or a framework
//! symbol.** `send_v_usize(recv, sel, a)` knows nothing about AppKit. That is
//! the test for whether something belongs here, and these pass it, so the
//! module's presence does not weaken the "zero framework bindings" rule stated
//! in [the crate note](crate) — the constants, the `NSString` globals and the
//! class lookups that DO name AppKit stay with their callers.
//!
//! # Where they came from, and why they moved
//!
//! W2 wrote them in `aterm-gui`'s `appkit.rs` as `pub(crate)` helpers, where
//! they grew to 35 shapes serving that crate's 206 distinct selectors. W8 needed
//! the same shapes inside `vendor/winit`, which cannot import an `aterm-gui`
//! private module — and the vendored fork is measured by a RATCHETED
//! `third_party_loc`, so a second copy there would have charged ~500 lines of
//! first-party ABI plumbing to the third-party budget the campaign exists to
//! shrink, and left two copies of a safety-critical layer free to drift.
//!
//! Moving them here costs the fork nothing, gives `aterm-gui` the same
//! functions under the same names (`appkit` re-exports this module), and leaves
//! exactly ONE definition of each prototype in the tree. `appkit.rs`'s call
//! sites — `toolbar.rs`'s 268 among them — are unchanged.
//!
//! # Ownership
//!
//! These are prototypes, not policy: none of them retains, releases or
//! autoreleases, and none inspects the selector to guess whether the object it
//! returns is +1 or +0. The CALLER decides, exactly as it must in Objective-C —
//! an `alloc`/`new`/`copy`/`mutableCopy` result goes into [`Obj`] or
//! [`Retained`]; anything else is BORROWED and lives until the enclosing
//! [`autoreleasepool`] pops.
//!
//! # Safety
//!
//! Every function here is `unsafe` for one reason: the caller asserts that the
//! selector it passes really has the C prototype named in the function's name,
//! and that `recv` is a live receiver that responds to it. Picking the wrong
//! helper is the same defect as writing the wrong cast by hand, and has the
//! same consequence — corrupted registers on both Apple ABIs.

use crate::{Bool, CGPoint, CGRect, CGSize, ClassPtr, Id, NSRange, Sel, msg};

/// `+alloc` on `cls` — a +1, zero-filled, UNINITIALISED instance.
///
/// # Safety
/// `cls` must be a live, registered class. The result is +1 and the caller owes
/// it exactly one `-init…` and one eventual release.
#[must_use]
pub unsafe fn alloc(cls: ClassPtr) -> Id {
    // SAFETY: `+alloc` is `-(id)(Class, SEL)` on every class in the runtime.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        f(cls.as_id(), crate::sel!(alloc))
    }
}

/// `-(void)sel` — no arguments.
///
/// # Safety
/// See the module note: `sel` must be `-(void)` with no arguments on `recv`.
pub unsafe fn send_v(recv: Id, sel: Sel) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = msg();
        f(recv, sel);
    }
}

/// `-(void)sel:(id)a`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id(recv: Id, sel: Sel, a: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(id)a other:(id)b`.
///
/// # Safety
/// See the module note.
// Reached from the ported modules' RUNTIME TESTS (which is where a
// declared class is interrogated), not from their production paths.
#[allow(dead_code)]
pub unsafe fn send_v_id_id(recv: Id, sel: Sel, a: Id, b: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(BOOL)a`.
///
/// # Safety
/// See the module note. The argument is [`Bool`], never a Rust `bool`: on the
/// `x86_64-apple-darwin` compat slice `BOOL` is `signed char`.
pub unsafe fn send_v_bool(recv: Id, sel: Sel, a: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Bool) = msg();
        f(recv, sel, Bool::new(a));
    }
}

/// `-(void)sel:(NSInteger)a`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_isize(recv: Id, sel: Sel, a: isize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSUInteger)a`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_usize(recv: Id, sel: Sel, a: usize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSSize)a`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_size(recv: Id, sel: Sel, a: CGSize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGSize) = msg();
        f(recv, sel, a);
    }
}

/// `-(id)sel`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id(recv: Id, sel: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(id)a`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id(recv: Id, sel: Sel, a: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(id)a b:(SEL)b c:(id)c` — `NSMenuItem`'s designated initializer
/// shape (`initWithTitle:action:keyEquivalent:`).
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id_sel_id(recv: Id, sel: Sel, a: Id, b: Sel, c: Id) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel, Id) -> Id = msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(id)sel:(CGFloat)a`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_f64(recv: Id, sel: Sel, a: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, f64) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(NSRect)a` — `-[NSView initWithFrame:]` and friends.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_rect(recv: Id, sel: Sel, a: CGRect) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(NSInteger)a` — `-[NSMenu itemAtIndex:]` and friends.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
// Reached from the ported modules' RUNTIME TESTS (which is where a
// declared class is interrogated), not from their production paths.
#[allow(dead_code)]
pub unsafe fn send_id_isize(recv: Id, sel: Sel, a: isize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(BOOL)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
// Reached from the ported modules' RUNTIME TESTS (which is where a
// declared class is interrogated), not from their production paths.
#[allow(dead_code)]
pub unsafe fn send_bool(recv: Id, sel: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it. `Bool`
    // and not `bool`: `msg` refuses the latter, deliberately.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Bool = msg();
        f(recv, sel).as_bool()
    }
}

/// `-(BOOL)sel:(SEL)a` — `-respondsToSelector:`.
///
/// # Safety
/// See the module note.
#[must_use]
// Reached from the ported modules' RUNTIME TESTS (which is where a
// declared class is interrogated), not from their production paths.
#[allow(dead_code)]
pub unsafe fn send_bool_sel(recv: Id, sel: Sel, a: Sel) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) -> Bool = msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(BOOL)sel:(id)a` — `-isEqual:`, `-conformsToProtocol:`, …
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_id(recv: Id, sel: Sel, a: Id) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Bool = msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(NSInteger)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_isize(recv: Id, sel: Sel) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = msg();
        f(recv, sel)
    }
}

/// `-(NSUInteger)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
// Reached from the ported modules' RUNTIME TESTS (which is where a
// declared class is interrogated), not from their production paths.
#[allow(dead_code)]
pub unsafe fn send_usize(recv: Id, sel: Sel) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        f(recv, sel)
    }
}

/// `-(CGFloat)sel`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_f64(recv: Id, sel: Sel) -> f64 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 = msg();
        f(recv, sel)
    }
}

/// `-(NSRect)sel`.
///
/// This is the shape that made `objc_msgSend_stret` a blocker (D4): an
/// `NSRect` is 32 bytes, so on the `x86_64-apple-darwin` compat slice this send
/// goes to a DIFFERENT entry point. No call site has to remember that —
/// [`msg`] reads it off the return type in a `const` block.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_rect(recv: Id, sel: Sel) -> CGRect {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> CGRect = msg();
        f(recv, sel)
    }
}

/// `-(void)sel:(CGFloat)a` — `-[NSBezierPath setLineWidth:]`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_f64(recv: Id, sel: Sel, a: f64) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, f64) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSPoint)a` — `-[NSBezierPath moveToPoint:]` / `lineToPoint:`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_point(recv: Id, sel: Sel, a: CGPoint) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGPoint) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(NSRect)a` — `-[NSView setFrame:]`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect(recv: Id, sel: Sel, a: CGRect) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect) = msg();
        f(recv, sel, a);
    }
}

/// `-(void)sel:(id)a b:(id)b c:(id)c` — `+[NSMenu
/// popUpContextMenu:withEvent:forView:]`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_id_id(recv: Id, sel: Sel, a: Id, b: Id, c: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) = msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(void)sel:(id)a b:(NSInteger)b c:(id)c` — `-[NSView
/// addSubview:positioned:relativeTo:]`, whose middle argument is an
/// `NSWindowOrderingMode`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_isize_id(recv: Id, sel: Sel, a: Id, b: isize, c: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, isize, Id) = msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(NSPoint)sel` — `-[NSEvent locationInWindow]`.
///
/// An `NSPoint` is SIXTEEN bytes of two `f64`, which is the row of
/// [`crate::returns_indirectly`]'s measured table that returns DIRECTLY on
/// both arches — unlike the 32-byte [`send_rect`], which sits five functions
/// ABOVE this one and takes the `_stret` entry on the compat slice. Neither
/// call site has to know that; the choice is read off the return type.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point(recv: Id, sel: Sel) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> CGPoint = msg();
        f(recv, sel)
    }
}

/// `-(NSPoint)sel:(NSPoint)a` — `-[NSWindow convertPointToScreen:]`.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point_point(recv: Id, sel: Sel, a: CGPoint) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGPoint) -> CGPoint = msg();
        f(recv, sel, a)
    }
}

/// `-(NSPoint)sel:(NSPoint)a fromView:(id)b` — `-[NSView
/// convertPoint:fromView:]`, where a nil `b` means WINDOW coordinates.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_point_point_id(recv: Id, sel: Sel, a: CGPoint, b: Id) -> CGPoint {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGPoint, Id) -> CGPoint = msg();
        f(recv, sel, a, b)
    }
}

/// `-(NSRect)sel:(NSRect)a fromView:(id)b` — `-[NSView
/// convertRect:fromView:]`. The 32-byte return is [`send_rect`]'s `_stret`
/// case, with two more arguments.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_rect_rect_id(recv: Id, sel: Sel, a: CGRect, b: Id) -> CGRect {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, Id) -> CGRect = msg();
        f(recv, sel, a, b)
    }
}

/// `-(id)sel:(NSUInteger)a` — `-[NSWindow standardWindowButton:]`, whose
/// argument is an `NSWindowButton` (an `NSUInteger` enum, NOT the signed one
/// [`send_id_isize`] takes).
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_usize(recv: Id, sel: Sel, a: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(NSRect)a xRadius:(CGFloat)b yRadius:(CGFloat)c` —
/// `+[NSBezierPath bezierPathWithRoundedRect:xRadius:yRadius:]`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_rect_f64_f64(recv: Id, sel: Sel, a: CGRect, b: f64, c: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, f64, f64) -> Id = msg();
        f(recv, sel, a, b, c)
    }
}

/// `-(id)sel:(CGFloat)a b:(CGFloat)b c:(CGFloat)c d:(CGFloat)d` —
/// `+[NSColor colorWithSRGBRed:green:blue:alpha:]`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_f64_f64_f64_f64(recv: Id, sel: Sel, a: f64, b: f64, c: f64, d: f64) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, f64, f64, f64, f64) -> Id = msg();
        f(recv, sel, a, b, c, d)
    }
}

/// `-(id)sel:(NSRect)a options:(NSUInteger)b owner:(id)c userInfo:(id)d` —
/// `-[NSTrackingArea initWithRect:options:owner:userInfo:]`.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_rect_usize_id_id(
    recv: Id,
    sel: Sel,
    a: CGRect,
    b: usize,
    c: Id,
    d: Id,
) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, usize, Id, Id) -> Id = msg();
        f(recv, sel, a, b, c, d)
    }
}

/// `-(id)sel:(id)a target:(id)b action:(SEL)c` — `+[NSButton
/// buttonWithTitle:target:action:]`. The MIRROR of [`send_id_id_sel_id`]: same
/// three arguments, `SEL` in the LAST position rather than the middle, which is
/// exactly the distinction a shared `*const c_void` spelling would have lost.
///
/// # Safety
/// See the module note, including the ownership paragraph.
#[must_use]
pub unsafe fn send_id_id_id_sel(recv: Id, sel: Sel, a: Id, b: Id, c: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Sel) -> Id = msg();
        f(recv, sel, a, b, c)
    }
}

// ---------------------------------------------------------------------------
// W8. The shapes `vendor/winit`'s `window_delegate.rs` and `view.rs` need that
// `aterm-gui` never did. Each carries the SDK declaration it was derived from,
// because the prototype IS the safety obligation and a reader who cannot
// re-derive it cannot check it.
// ---------------------------------------------------------------------------

/// `-(void)sel:(NSRect)a flag:(BOOL)b`.
///
/// `NSWindow.h` — `-setFrame:(NSRect)frameRect display:(BOOL)displayFlag`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect_bool(recv: Id, sel: Sel, a: CGRect, b: bool) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, Bool) = msg();
        f(recv, sel, a, Bool::new(b));
    }
}

/// `-(void)sel:(NSRect)a obj:(id)b`.
///
/// `NSView.h` — `-addCursorRect:(NSRect)rect cursor:(NSCursor *)object`.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_rect_id(recv: Id, sel: Sel, a: CGRect, b: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, Id) = msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(id)a other:(NSInteger)b`.
///
/// `NSWindow.h` — `-addChildWindow:(NSWindow *)childWin
/// ordered:(NSWindowOrderingMode)place`. `NSWindowOrderingMode` is an
/// `NSInteger` enum, so the second word is signed.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_isize(recv: Id, sel: Sel, a: Id, b: isize) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, isize) = msg();
        f(recv, sel, a, b);
    }
}

/// `-(void)sel:(id)a b:(id)b c:(NSUInteger)c d:(void *)d`.
///
/// `NSKeyValueObserving.h` — `-addObserver:(NSObject *)observer
/// forKeyPath:(NSString *)keyPath options:(NSKeyValueObservingOptions)options
/// context:(void *)context`.
///
/// THE `context:` ARGUMENT IS `^v`, NOT `@`, and this crate has been wrong
/// about that before: the note on [`crate::IvarSlot`] records "a mutable raw
/// void pointer in a method position always means `id`" as a claim the KVO
/// registration refutes. It is a bare pointer AppKit stores and hands back
/// verbatim; it is never messaged and never retained.
///
/// # Safety
/// See the module note. `d` is stored by the observed object and returned to
/// the observer unchanged — it is not dereferenced here and must satisfy
/// whatever the observing method assumes of it.
pub unsafe fn send_v_id_id_usize_ptr(
    recv: Id,
    sel: Sel,
    a: Id,
    b: Id,
    c: usize,
    d: *mut core::ffi::c_void,
) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, usize, *mut core::ffi::c_void) = msg();
        f(recv, sel, a, b, c, d);
    }
}

/// `-(void)sel:(SEL)a withObject:(id)b afterDelay:(NSTimeInterval)c`.
///
/// `NSRunLoop.h` — `-performSelector:(SEL)aSelector withObject:(id)anArgument
/// afterDelay:(NSTimeInterval)delay`. `NSTimeInterval` is `double`.
///
/// # Safety
/// See the module note. `a` is scheduled, not sent, so `recv` must still be
/// live when the run loop reaches it — the send itself does not retain, but
/// `-performSelector:withObject:afterDelay:` does.
pub unsafe fn send_v_sel_id_f64(recv: Id, sel: Sel, a: Sel, b: Id, c: f64) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel, Id, f64) = msg();
        f(recv, sel, a, b, c);
    }
}

/// `-(void)sel:(id)a selector:(SEL)b name:(id)c object:(id)d`.
///
/// `NSNotification.h` — `-addObserver:(id)observer selector:(SEL)aSelector
/// name:(NSNotificationName)aName object:(id)anObject`. The observer is
/// registered UNRETAINED; removing it before it dies is the caller's business.
///
/// # Safety
/// See the module note.
pub unsafe fn send_v_id_sel_id_id(recv: Id, sel: Sel, a: Id, b: Sel, c: Id, d: Id) {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel, Id, Id) = msg();
        f(recv, sel, a, b, c, d);
    }
}

/// `+(id)sel:(const id *)a count:(NSUInteger)b`.
///
/// `NSArray.h` — `+arrayWithObjects:(const ObjectType _Nonnull [])objects
/// count:(NSUInteger)cnt`, the one array constructor that does not need a
/// nil-terminated variadic list. A VARIADIC send would be the wrong shape on
/// AAPCS64 (see the crate note), which is why this counted form is the one
/// used.
///
/// # `idptr`, not `ptr`, and the census is why
///
/// This was `send_id_ptr_usize` for one commit, sharing the `ptr` token with
/// [`send_v_id_id_usize_ptr`]'s KVO `context:`. They are NOT the same argument:
/// `context:` encodes `^v` (a bare `void *` the runtime never touches) and this
/// one encodes `r^@` — a CONST pointer to object pointers, which Foundation
/// dereferences `b` times and retains each element of. The Rust types were
/// always right (`*mut c_void` against `*const Id`); the NAMES were not, and
/// `winit_sent_prototypes.rs` said so the first time it ran.
///
/// # Safety
/// See the module note, including the ownership paragraph. `a` must point to
/// at least `b` live object pointers, none of them nil.
#[must_use]
pub unsafe fn send_id_idptr_usize(recv: Id, sel: Sel, a: *const Id, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const Id, usize) -> Id = msg();
        f(recv, sel, a, b)
    }
}

/// `-(NSRect)sel:(NSRect)a`.
///
/// `NSWindow.h` — `-contentRectForFrameRect:(NSRect)frameRect`;
/// `NSView.h` — `-convertRectToScreen:(NSRect)rect`.
///
/// Both the argument and the return are 32-byte homogeneous floating-point
/// aggregates, so on AAPCS64 the argument goes in `d0`-`d3` and the result
/// comes back in `d0`-`d3` — neither takes the indirect path. [`msg`] picks
/// `objc_msgSend_stret` from the RETURN type on `x86_64` and this shape is one
/// of the cases where the two ABIs genuinely differ; see
/// [`crate::returns_indirectly`].
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_rect_rect(recv: Id, sel: Sel, a: CGRect) -> CGRect {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect) -> CGRect = msg();
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSUInteger)a` — MIXED SIGNEDNESS, and it is measured.
///
/// `NSApplication.h` — `-requestUserAttention:(NSRequestUserAttentionType)`
/// returns the request identifier, which every caller in this tree discards.
/// It is still declared with its real return type: reading the result is
/// optional, DECLARING it is not, because a prototype that returns `()` where
/// the IMP returns a word is a different function type.
///
/// THE ARGUMENT IS UNSIGNED AND THE RETURN IS SIGNED, which is not a shape
/// anything else here has and is not what it was first written as. This helper
/// was `send_isize_isize` until `method_getTypeEncoding` was asked:
///
/// ```text
/// NSApplication  requestUserAttention:  q24@0:8Q16
/// ```
///
/// `q` out, `Q` in. `NSRequestUserAttentionType` is an `NSUInteger` enum while
/// `NSModalResponse`-style identifiers are `NSInteger`, and the selector
/// straddles the two. Nothing would have caught it at runtime — both are 64-bit
/// words on both Apple ABIs and the only two values passed are 0 and 10 — which
/// is exactly why the encodings are read rather than reasoned about.
///
/// # Safety
/// See the module note.
pub unsafe fn send_isize_usize(recv: Id, sel: Sel, a: usize) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) -> isize = msg();
        f(recv, sel, a)
    }
}

/// `-(NSInteger)sel:(NSRect)a owner:(id)b userData:(void *)c assumeInside:(BOOL)d`.
///
/// `NSView.h` — `-addTrackingRect:(NSRect)rect owner:(id)owner
/// userData:(void *)data assumeInside:(BOOL)flag`, returning an
/// `NSTrackingRectTag` (an `NSInteger`). `userData` is `^v` and is handed back
/// verbatim to the owner's mouse-entered/exited rows; see the note on
/// [`send_v_id_id_usize_ptr`].
///
/// # Safety
/// See the module note. `b` is retained by neither the view nor this send, so
/// it must outlive the tracking rect or be removed first.
pub unsafe fn send_isize_rect_id_ptr_bool(
    recv: Id,
    sel: Sel,
    a: CGRect,
    b: Id,
    c: *mut core::ffi::c_void,
    d: bool,
) -> isize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGRect, Id, *mut core::ffi::c_void, Bool) -> isize =
            msg();
        f(recv, sel, a, b, c, Bool::new(d))
    }
}

/// `-(unsigned short)sel`.
///
/// `NSEvent.h` — `-keyCode`, the ONE selector in either ported tree whose
/// return is narrower than a word. It is `unsigned short`, not `NSUInteger`,
/// and the difference is not academic: a prototype claiming a full word would
/// read the upper 48 bits of `x0`, which AAPCS64 leaves UNSPECIFIED for a
/// `short` return.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_u16(recv: Id, sel: Sel) -> u16 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u16 = msg();
        f(recv, sel)
    }
}

/// `-(BOOL)sel:(Class)a`.
///
/// `NSObject.h` — `-isKindOfClass:(Class)aClass`. A `Class` is an object
/// pointer at the ABI, but it is a DIFFERENT argument than an instance and the
/// helper says so rather than letting a call site launder one into the other
/// through [`ClassPtr::as_id`].
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_bool_cls(recv: Id, sel: Sel, a: ClassPtr) -> bool {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, ClassPtr) -> Bool = msg();
        f(recv, sel, a).as_bool()
    }
}

/// `-(NSRange)sel`.
///
/// `NSTextInputClient.h` — `-markedRange` / `-selectedRange`. 16 bytes of two
/// `NSUInteger`s, which is NOT a homogeneous floating-point aggregate: on
/// AAPCS64 it comes back in `x0`/`x1` rather than indirectly, and on `x86_64`
/// in `rax`/`rdx`. Neither ABI uses `x8`/`objc_msgSend_stret` for it.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_range(recv: Id, sel: Sel) -> NSRange {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRange = msg();
        f(recv, sel)
    }
}

/// `+(id)keyEventWithType:location:modifierFlags:timestamp:windowNumber:`
/// `context:characters:charactersIgnoringModifiers:isARepeat:keyCode:`.
///
/// TEN arguments, and the only send in either tree that needs its own shape.
/// `NSEvent.h` declares it
///
/// ```text
/// + (nullable NSEvent *)keyEventWithType:(NSEventType)type
///                              location:(NSPoint)location
///                         modifierFlags:(NSEventModifierFlags)flags
///                             timestamp:(NSTimeInterval)time
///                          windowNumber:(NSInteger)wNum
///                               context:(nullable NSGraphicsContext *)unusedPassNil
///                            characters:(NSString *)keys
///           charactersIgnoringModifiers:(NSString *)ukeys
///                             isARepeat:(BOOL)flag
///                               keyCode:(unsigned short)code;
/// ```
///
/// `NSEventType` and `NSEventModifierFlags` are both `NSUInteger`;
/// `NSTimeInterval` is `double`; `keyCode` is `unsigned short` (see
/// [`send_u16`]). The name is mechanical, like every other shape here, because
/// a shape whose name is a nickname cannot be checked against its own
/// signature by reading.
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// autoreleased, not +1.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub unsafe fn send_id_usize_point_usize_f64_isize_id_id_id_bool_u16(
    recv: Id,
    sel: Sel,
    a: usize,
    b: CGPoint,
    c: usize,
    d: f64,
    e: isize,
    f_: Id,
    g: Id,
    h: Id,
    i: bool,
    j: u16,
) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(
            Id,
            Sel,
            usize,
            CGPoint,
            usize,
            f64,
            isize,
            Id,
            Id,
            Id,
            Bool,
            u16,
        ) -> Id = msg();
        f(recv, sel, a, b, c, d, e, f_, g, h, Bool::new(i), j)
    }
}

/// `+[NSEvent otherEventWithType:location:modifierFlags:timestamp:windowNumber:
/// context:subtype:data1:data2:]` — the application-defined event constructor.
///
/// ```objc
/// + (nullable NSEvent *)otherEventWithType:(NSEventType)type
///                                 location:(NSPoint)location
///                            modifierFlags:(NSEventModifierFlags)flags
///                                timestamp:(NSTimeInterval)time
///                             windowNumber:(NSInteger)wNum
///                                  context:(nullable NSGraphicsContext *)unusedPassNil
///                                  subtype:(short)subtype
///                                    data1:(NSInteger)d1
///                                    data2:(NSInteger)d2;
/// ```
///
/// # `subtype:` is a SIGNED SHORT, and it is the only one of its kind here
///
/// Every other narrow integer in this module is `unsigned short` — `keyCode:`,
/// which [`send_u16`] exists for. `subtype:` is not: the header spells it bare
/// `short`, and the runtime agrees. The full encoding, read from
/// `method_getTypeEncoding` rather than derived from the header, is
///
/// ```text
/// @92@0:8Q16{CGPoint=dd}24Q40d48q56@64s72q76q84
/// ```
///
/// — note the `s` at offset 72, where `S` would be the unsigned twin. This is
/// the same class of distinction as the `-requestUserAttention:` defect W8
/// shipped and this crate's prototype census caught: two spellings that occupy
/// the same register and differ only in sign. `NSEventSubtypeWindowExposed` is
/// 0, so no value the fork passes could ever have exposed the difference at
/// runtime, which is exactly why it is written from the encoding.
///
/// # Safety
/// See the module note, including the ownership paragraph — the result is +0
/// autoreleased, not +1.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub unsafe fn send_id_usize_point_usize_f64_isize_id_i16_isize_isize(
    recv: Id,
    sel: Sel,
    a: usize,
    b: CGPoint,
    c: usize,
    d: f64,
    e: isize,
    f_: Id,
    g: i16,
    h: isize,
    i: isize,
) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(
            Id,
            Sel,
            usize,
            CGPoint,
            usize,
            f64,
            isize,
            Id,
            i16,
            isize,
            isize,
        ) -> Id = msg();
        f(recv, sel, a, b, c, d, e, f_, g, h, i)
    }
}

// ---------------------------------------------------------------------------
// W9 phase 2 — the shapes `cursor.rs` and `view.rs` need.
//
// Six, and TWO OF THEM ARE HERE BECAUSE THE RUNTIME DISAGREED WITH THE
// BINDING'S APPEARANCE. `-magnification` is `d16@0:8` and `-rotation` is
// `f16@0:8`: the two are adjacent properties on `NSEvent`, both feed a gesture
// event, both read as "a float" in Rust — and one of them is a `float` while
// the other is a `double`. On AAPCS64 a `float` return arrives in `s0`, the low
// half of `d0`, so reading it with the `d` prototype yields a denormal built
// out of the caller's stale high bits rather than a wrong-but-plausible number.
// Nothing but `method_getTypeEncoding` distinguishes them, which is what
// [`send_f32`] exists for.
// ---------------------------------------------------------------------------

/// `-(float)sel` — a C `float`, NOT a `CGFloat`.
///
/// ```text
/// NSEvent  rotation  f16@0:8
/// NSEvent  pressure  f16@0:8
/// ```
///
/// Both are `float` in `NSEvent.h` (`@property(readonly) float rotation;`)
/// while every geometric quantity beside them — `-magnification`,
/// `-scrollingDeltaX`, `-timestamp` — is `double`. [`send_f64`] is the wrong
/// prototype for these two and would not have been caught by any value they
/// carry: a rotation of `0.0` reads back `0.0` under either cast, because the
/// register happens to be zero.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_f32(recv: Id, sel: Sel) -> f32 {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f32 = msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(SEL)a`.
///
/// `NSObject.h` — `-performSelector:(SEL)aSelector`, which `cursor.rs` uses to
/// reach the undocumented `+[NSCursor _helpCursor]` family by name.
///
/// # Safety
/// See the module note, including the ownership paragraph. `-performSelector:`
/// returns whatever the invoked method returns under ITS OWN convention: the
/// cursor selectors here are `+…Cursor` accessors, so the result is +0
/// autoreleased and must not be released.
#[must_use]
pub unsafe fn send_id_sel(recv: Id, sel: Sel, a: Sel) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(NSUInteger)sel:(NSUInteger)a`.
///
/// `NSString.h` — `-lengthOfBytesUsingEncoding:(NSStringEncoding)enc`, read as
/// `Q24@0:8Q16`.
///
/// # Why this shape is not a nicety
///
/// `objc2-foundation` spells two different selectors as two nearly identical
/// Rust methods: `NSString::len()` is
/// `-lengthOfBytesUsingEncoding:NSUTF8StringEncoding` — a UTF-8 BYTE count —
/// and `NSString::len_utf16()` is `-length`, a UTF-16 CODE UNIT count. `view.rs`
/// calls both, three lines apart, on the IME pre-edit path: `-length` clamps
/// the index the input method proposes, and the byte count is what winit's
/// `Ime::Preedit` cursor range is measured in. Porting both to `-length` would
/// compile, would be exactly right for ASCII, and would put the pre-edit
/// caret in the wrong place for every non-ASCII composition — which is most of
/// what an IME is for.
///
/// # Safety
/// See the module note.
#[must_use]
pub unsafe fn send_usize_usize(recv: Id, sel: Sel, a: usize) -> usize {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) -> usize = msg();
        f(recv, sel, a)
    }
}

/// `-(unsigned char *)sel` — an interior pointer into the receiver's storage.
///
/// `NSBitmapImageRep.h` — `-bitmapData`, read as `*16@0:8`. The `*` is the
/// runtime's spelling of `char *`, which is what `@encode(unsigned char *)`
/// reduces to; it is NOT `^C`.
///
/// # Safety
/// See the module note. The pointer is INTERIOR to `recv` and is valid only
/// while `recv` is alive and its buffer un-reallocated; writing through it is
/// writing into the receiver's pixels.
#[must_use]
pub unsafe fn send_charptr(recv: Id, sel: Sel) -> *mut core::ffi::c_uchar {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> *mut core::ffi::c_uchar = msg();
        f(recv, sel)
    }
}

/// `-(id)sel:(const void *)a length:(NSUInteger)b`.
///
/// `NSData.h` — `-initWithBytes:(const void *)bytes length:(NSUInteger)length`,
/// read as `@32@0:8r^v16Q24`.
///
/// # `cptr`, not `ptr`, and it is the same distinction the census already made
///
/// [`send_v_id_id_usize_ptr`]'s KVO `context:` is `^v` — a bare `void *` the
/// runtime stores and hands back untouched. This one is `r^v`: a CONST pointer
/// Foundation reads `b` bytes out of and copies. They occupy the same register
/// and encode differently, which is precisely the pair
/// `winit_sent_prototypes.rs` was written to keep apart, so the name carries
/// the difference rather than the comment.
///
/// # Safety
/// See the module note, including the ownership paragraph — `-initWithBytes:`
/// is an initialiser and its result is +1. `a` must point to at least `b`
/// readable bytes for the duration of the call; they are COPIED, so the buffer
/// may be freed afterwards.
#[must_use]
pub unsafe fn send_id_cptr_usize(recv: Id, sel: Sel, a: *const core::ffi::c_void, b: usize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const core::ffi::c_void, usize) -> Id = msg();
        f(recv, sel, a, b)
    }
}

/// `-[NSBitmapImageRep initWithBitmapDataPlanes:pixelsWide:pixelsHigh:
/// bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:
/// bitsPerPixel:]` — the ten-argument raw-bitmap initialiser.
///
/// ```objc
/// - (instancetype)initWithBitmapDataPlanes:(unsigned char * _Nullable * _Nullable)planes
///                               pixelsWide:(NSInteger)width
///                               pixelsHigh:(NSInteger)height
///                            bitsPerSample:(NSInteger)bps
///                          samplesPerPixel:(NSInteger)spp
///                                 hasAlpha:(BOOL)alpha
///                                 isPlanar:(BOOL)isPlanar
///                           colorSpaceName:(NSColorSpaceName)colorSpaceName
///                              bytesPerRow:(NSInteger)rBytes
///                             bitsPerPixel:(NSInteger)pBits;
/// ```
///
/// Read from the runtime rather than the header:
///
/// ```text
/// @88@0:8^*16q24q32q40q48B56B60@64q72q80
/// ```
///
/// — ten arguments plus `self` and `_cmd`, so four of them are passed on the
/// STACK on AAPCS64 rather than in `x0`-`x7`. That is the reason a shape this
/// long is written out in full instead of approximated: a variadic declaration
/// would place them somewhere else entirely, and the two `B`s at offsets 56 and
/// 60 are single BYTES the caller must not widen.
///
/// `planeptr` is `^*` — a pointer to `char *`, i.e. an ARRAY of plane pointers
/// — and is distinct from both `ptr` (`^v`) and `cptr` (`r^v`). `cursor.rs`
/// passes null, which asks the receiver to allocate its own buffer.
///
/// # Safety
/// See the module note, including the ownership paragraph — this is an
/// initialiser and its result is +1. `a` must be null or point to `spp` plane
/// pointers; `h` must be a live `NSColorSpaceName` (an `NSString`).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub unsafe fn send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize(
    recv: Id,
    sel: Sel,
    a: *mut *mut core::ffi::c_uchar,
    b: isize,
    c: isize,
    d: isize,
    e: isize,
    f_: bool,
    g: bool,
    h: Id,
    i: isize,
    j: isize,
) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(
            Id,
            Sel,
            *mut *mut core::ffi::c_uchar,
            isize,
            isize,
            isize,
            isize,
            Bool,
            Bool,
            Id,
            isize,
            isize,
        ) -> Id = msg();
        f(
            recv,
            sel,
            a,
            b,
            c,
            d,
            e,
            Bool::new(f_),
            Bool::new(g),
            h,
            i,
            j,
        )
    }
}

/// `-(id)sel:(NSSize)a`.
///
/// `NSImage.h` — `-initWithSize:(NSSize)size`, read as `@32@0:8{CGSize=dd}16`.
/// A 16-byte homogeneous floating-point aggregate, so it goes in `d0`/`d1` on
/// AAPCS64 and in `xmm0`/`xmm1` on the compat slice; neither is an indirect
/// argument.
///
/// # Safety
/// See the module note, including the ownership paragraph — this is an
/// initialiser and its result is +1.
#[must_use]
pub unsafe fn send_id_size(recv: Id, sel: Sel, a: CGSize) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CGSize) -> Id = msg();
        f(recv, sel, a)
    }
}

/// `-(id)sel:(id)a hotSpot:(NSPoint)b`.
///
/// `NSCursor.h` — `-initWithImage:(NSImage *)newImage hotSpot:(NSPoint)point`,
/// read as `@40@0:8@16{CGPoint=dd}24`.
///
/// # Safety
/// See the module note, including the ownership paragraph — this is an
/// initialiser and its result is +1. `a` must be a live `NSImage`; the receiver
/// retains it for itself.
#[must_use]
pub unsafe fn send_id_id_point(recv: Id, sel: Sel, a: Id, b: CGPoint) -> Id {
    // SAFETY: the caller pins the prototype; this is the cast for it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, CGPoint) -> Id = msg();
        f(recv, sel, a, b)
    }
}
